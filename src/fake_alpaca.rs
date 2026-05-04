// Local fake Alpaca server for end-to-end tests.
//
// Function map:
// - cmd_run(): starts the HTTP fixture and prints a ready JSON line.
// - handle_request(): routes the subset of Alpaca Trading/Data endpoints used by mlai-trade.
// - fill_order(): mutates fake paper account positions/orders/fills like a filled market order.
// - fixture_*(): builds deterministic one-month stock/ETF market data.

use anyhow::Context;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use chrono::{Datelike, Duration, NaiveDate};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
struct FakeAsset {
    symbol: &'static str,
    name: &'static str,
    exchange: &'static str,
    base: f64,
    drift: f64,
    wiggle: f64,
    volume: u64,
}

#[derive(Debug, Clone)]
struct FakePosition {
    symbol: String,
    qty: f64,
    avg_entry_price: f64,
}

#[derive(Debug)]
struct FakeAlpacaState {
    assets: Vec<FakeAsset>,
    bars: HashMap<String, Vec<Value>>,
    positions: BTreeMap<String, FakePosition>,
    orders: Vec<Value>,
    fills: Vec<Value>,
    order_seq: u64,
    cash: f64,
    account_number: String,
}

impl FakeAlpacaState {
    // Builds deterministic account, market, order, and position state.
    fn new() -> Self {
        let assets = fixture_assets();
        let bars = fixture_bars(&assets);
        Self {
            assets,
            bars,
            positions: BTreeMap::new(),
            orders: Vec::new(),
            fills: Vec::new(),
            order_seq: 1,
            cash: 100_000.0,
            account_number: "PA33QLBK985G".to_string(),
        }
    }

    // Returns the latest close price for a symbol.
    fn latest_price(&self, symbol: &str) -> f64 {
        self.bars
            .get(symbol)
            .and_then(|bars| bars.last())
            .and_then(|bar| bar.get("c"))
            .and_then(Value::as_f64)
            .unwrap_or(100.0)
    }

    // Calculates account equity from cash plus marked positions.
    fn equity(&self) -> f64 {
        self.cash
            + self
                .positions
                .values()
                .map(|position| position.qty * self.latest_price(&position.symbol))
                .sum::<f64>()
    }

    // Builds an Alpaca-like account response.
    fn account_json(&self) -> Value {
        let equity = self.equity();
        json!({
            "id": "fake-account-id",
            "account_number": self.account_number,
            "status": "ACTIVE",
            "portfolio_value": money(equity),
            "equity": money(equity),
            "last_equity": money(equity),
            "cash": money(self.cash),
            "buying_power": money(self.cash * 2.0),
            "pattern_day_trader": false,
            "trading_blocked": false,
        })
    }

    // Builds an Alpaca-like position row.
    fn position_json(&self, position: &FakePosition) -> Value {
        let current_price = self.latest_price(&position.symbol);
        let market_value = position.qty * current_price;
        let cost_basis = position.qty * position.avg_entry_price;
        let unrealized_pl = market_value - cost_basis;
        let unrealized_plpc = if cost_basis.abs() > f64::EPSILON {
            unrealized_pl / cost_basis
        } else {
            0.0
        };
        json!({
            "symbol": position.symbol,
            "qty": qty(position.qty),
            "avg_entry_price": money(position.avg_entry_price),
            "current_price": money(current_price),
            "market_value": money(market_value),
            "unrealized_pl": money(unrealized_pl),
            "unrealized_plpc": format!("{unrealized_plpc:.8}"),
        })
    }

    // Fills a buy/sell order immediately and updates account state.
    fn fill_order(&mut self, body: Value) -> Result<Value, (StatusCode, Value)> {
        let symbol = body
            .get("symbol")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_uppercase();
        if symbol.is_empty() || !self.bars.contains_key(&symbol) {
            return Err((
                StatusCode::BAD_REQUEST,
                json!({"message": "unknown or missing symbol"}),
            ));
        }
        let order_qty = string_number(&body, "qty").unwrap_or(0.0);
        if order_qty <= 0.0 {
            return Err((
                StatusCode::BAD_REQUEST,
                json!({"message": "qty must be positive"}),
            ));
        }
        let side = body
            .get("side")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(side.as_str(), "buy" | "sell") {
            return Err((StatusCode::BAD_REQUEST, json!({"message": "invalid side"})));
        }

        let price = if side == "buy" {
            self.latest_price(&symbol) + 0.02
        } else {
            self.latest_price(&symbol) - 0.02
        };
        if side == "buy" {
            let notional = order_qty * price;
            if notional > self.cash {
                return Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    json!({"message": "insufficient buying power"}),
                ));
            }
            self.cash -= notional;
            self.add_position(&symbol, order_qty, price);
        } else {
            self.reduce_position(&symbol, order_qty)?;
            self.cash += order_qty * price;
        }

        let now = "2026-05-01T15:30:00Z";
        let id = format!("fake-order-{:06}", self.order_seq);
        self.order_seq += 1;
        let order = json!({
            "id": id,
            "client_order_id": body.get("client_order_id").and_then(Value::as_str).unwrap_or(""),
            "symbol": symbol,
            "side": side,
            "qty": qty(order_qty),
            "filled_qty": qty(order_qty),
            "type": body.get("type").and_then(Value::as_str).unwrap_or("market"),
            "time_in_force": body.get("time_in_force").and_then(Value::as_str).unwrap_or("day"),
            "status": "filled",
            "filled_avg_price": money(price),
            "submitted_at": now,
            "created_at": now,
            "updated_at": now,
            "filled_at": now,
        });
        let fill = json!({
            "id": format!("fake-fill-{:06}", self.order_seq),
            "order_id": order["id"],
            "symbol": order["symbol"],
            "side": order["side"],
            "qty": order["qty"],
            "price": order["filled_avg_price"],
            "cum_qty": order["qty"],
            "leaves_qty": "0",
            "activity_type": "FILL",
            "transaction_time": now,
        });
        self.orders.push(order.clone());
        self.fills.push(fill);
        Ok(order)
    }

    // Adds to an existing position using weighted average cost.
    fn add_position(&mut self, symbol: &str, add_qty: f64, price: f64) {
        self.positions
            .entry(symbol.to_string())
            .and_modify(|position| {
                let current_cost = position.qty * position.avg_entry_price;
                let added_cost = add_qty * price;
                position.qty += add_qty;
                position.avg_entry_price = (current_cost + added_cost) / position.qty;
            })
            .or_insert_with(|| FakePosition {
                symbol: symbol.to_string(),
                qty: add_qty,
                avg_entry_price: price,
            });
    }

    // Removes shares from an existing position.
    fn reduce_position(
        &mut self,
        symbol: &str,
        remove_qty: f64,
    ) -> Result<(), (StatusCode, Value)> {
        let Some(position) = self.positions.get_mut(symbol) else {
            return Err((
                StatusCode::NOT_FOUND,
                json!({"message": "position not found"}),
            ));
        };
        if remove_qty > position.qty + f64::EPSILON {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({"message": "insufficient position quantity"}),
            ));
        }
        position.qty -= remove_qty;
        if position.qty <= f64::EPSILON {
            self.positions.remove(symbol);
        }
        Ok(())
    }
}

// Starts the fake server and blocks until terminated.
pub async fn cmd_run(addr: String) -> anyhow::Result<()> {
    let requested: SocketAddr = addr.parse().context("invalid --addr value")?;
    let listener = tokio::net::TcpListener::bind(requested).await?;
    let actual = listener.local_addr()?;
    let state = Arc::new(Mutex::new(FakeAlpacaState::new()));
    let app = Router::new().fallback(handle_request).with_state(state);
    println!(
        "{}",
        json!({
            "status": "ready",
            "base_url": format!("http://{actual}"),
            "addr": actual.to_string(),
        })
    );
    let _ = std::io::stdout().flush();
    axum::serve(listener, app).await?;
    Ok(())
}

// Handles all fake Alpaca HTTP requests.
async fn handle_request(
    State(state): State<Arc<Mutex<FakeAlpacaState>>>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> Response {
    let body_json = if body.is_empty() {
        Value::Null
    } else {
        match serde_json::from_slice::<Value>(&body) {
            Ok(value) => value,
            Err(err) => {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    json!({"message": format!("invalid JSON body: {err}")}),
                )
            }
        }
    };
    let path = uri.path().trim_end_matches('/').to_string();
    let query = query_map(uri.query().unwrap_or(""));
    let mut state = match state.lock() {
        Ok(state) => state,
        Err(_) => {
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"message": "fake server state lock poisoned"}),
            )
        }
    };
    route_request(&mut state, &method, &path, &query, body_json)
}

// Routes one fake Alpaca request to the relevant fixture response.
fn route_request(
    state: &mut FakeAlpacaState,
    method: &Method,
    path: &str,
    query: &HashMap<String, String>,
    body: Value,
) -> Response {
    match (method.clone(), path) {
        (Method::GET, "/v2/account") => json_response(StatusCode::OK, state.account_json()),
        (Method::GET, "/v2/assets") => json_response(StatusCode::OK, assets_json(state)),
        (Method::GET, "/v2/positions") => json_response(StatusCode::OK, positions_json(state)),
        (Method::GET, "/v2/orders") => json_response(StatusCode::OK, orders_json(state, query)),
        (Method::POST, "/v2/orders") => match state.fill_order(body) {
            Ok(order) => json_response(StatusCode::OK, order),
            Err((status, value)) => json_response(status, value),
        },
        (Method::DELETE, "/v2/orders") => {
            for order in &mut state.orders {
                if order.get("status").and_then(Value::as_str) == Some("new") {
                    order["status"] = json!("canceled");
                }
            }
            StatusCode::NO_CONTENT.into_response()
        }
        (Method::DELETE, "/v2/positions") => {
            let symbols = state.positions.keys().cloned().collect::<Vec<_>>();
            for symbol in symbols {
                close_symbol(state, &symbol);
            }
            StatusCode::NO_CONTENT.into_response()
        }
        (Method::GET, "/v2/account/activities/FILL") => {
            json_response(StatusCode::OK, Value::Array(state.fills.clone()))
        }
        (Method::GET, "/v3/clock") => json_response(StatusCode::OK, clock_json()),
        _ => dynamic_route(state, method, path, query),
    }
}

// Handles routes with symbol/market/order path parameters.
fn dynamic_route(
    state: &mut FakeAlpacaState,
    method: &Method,
    path: &str,
    query: &HashMap<String, String>,
) -> Response {
    if method == Method::GET && path.starts_with("/v2/positions/") {
        let symbol = path
            .trim_start_matches("/v2/positions/")
            .to_ascii_uppercase();
        return match state.positions.get(&symbol) {
            Some(position) => json_response(StatusCode::OK, state.position_json(position)),
            None => json_response(
                StatusCode::NOT_FOUND,
                json!({"message": "position not found"}),
            ),
        };
    }
    if method == Method::DELETE && path.starts_with("/v2/positions/") {
        let symbol = path
            .trim_start_matches("/v2/positions/")
            .to_ascii_uppercase();
        if state.positions.contains_key(&symbol) {
            close_symbol(state, &symbol);
            return StatusCode::NO_CONTENT.into_response();
        }
        return json_response(
            StatusCode::NOT_FOUND,
            json!({"message": "position not found"}),
        );
    }
    if method == Method::DELETE && path.starts_with("/v2/orders/") {
        let order_id = path.trim_start_matches("/v2/orders/");
        for order in &mut state.orders {
            if order.get("id").and_then(Value::as_str) == Some(order_id) {
                order["status"] = json!("canceled");
                return StatusCode::NO_CONTENT.into_response();
            }
        }
        return json_response(StatusCode::NOT_FOUND, json!({"message": "order not found"}));
    }
    if method == Method::GET && path.starts_with("/v3/calendar/") {
        let market = path.trim_start_matches("/v3/calendar/");
        return json_response(StatusCode::OK, calendar_json(market, query));
    }
    if method == Method::GET && path.starts_with("/v2/stocks/") {
        return stock_data_route(state, path, query);
    }
    if method == Method::GET && path == "/v1beta1/news" {
        return json_response(StatusCode::OK, news_json(query));
    }
    if method == Method::GET && path == "/v1beta1/screener/stocks/movers" {
        return json_response(StatusCode::OK, movers_json());
    }
    json_response(
        StatusCode::NOT_FOUND,
        json!({"message": format!("fake Alpaca route not implemented: {method} {path}")}),
    )
}

// Handles market-data stock routes.
fn stock_data_route(
    state: &FakeAlpacaState,
    path: &str,
    query: &HashMap<String, String>,
) -> Response {
    if path == "/v2/stocks/bars" {
        let symbols = query
            .get("symbols")
            .map(|value| split_symbols(value))
            .unwrap_or_default();
        let mut bars = serde_json::Map::new();
        for symbol in symbols {
            bars.insert(
                symbol.clone(),
                Value::Array(filtered_bars(state, &symbol, query)),
            );
        }
        return json_response(
            StatusCode::OK,
            json!({"bars": bars, "next_page_token": null}),
        );
    }

    let rest = path.trim_start_matches("/v2/stocks/");
    let Some((symbol, action)) = rest.split_once('/') else {
        return json_response(
            StatusCode::NOT_FOUND,
            json!({"message": "missing stock action"}),
        );
    };
    let symbol = symbol.to_ascii_uppercase();
    match action {
        "quotes/latest" => json_response(StatusCode::OK, quote_json(state, &symbol)),
        "snapshot" => json_response(StatusCode::OK, snapshot_json(state, &symbol)),
        "bars" => json_response(
            StatusCode::OK,
            json!({"bars": filtered_bars(state, &symbol, query)}),
        ),
        _ => json_response(
            StatusCode::NOT_FOUND,
            json!({"message": "unknown stock route"}),
        ),
    }
}

// Closes a fake position through a filled sell order.
fn close_symbol(state: &mut FakeAlpacaState, symbol: &str) {
    let Some(position) = state.positions.get(symbol).cloned() else {
        return;
    };
    let body = json!({
        "symbol": symbol,
        "qty": qty(position.qty),
        "side": "sell",
        "type": "market",
        "time_in_force": "day",
        "client_order_id": format!("fake-close-{symbol}"),
    });
    let _ = state.fill_order(body);
}

// Returns assets in Alpaca's asset-list shape.
fn assets_json(state: &FakeAlpacaState) -> Value {
    Value::Array(
        state
            .assets
            .iter()
            .map(|asset| {
                json!({
                    "symbol": asset.symbol,
                    "name": asset.name,
                    "exchange": asset.exchange,
                    "status": "active",
                    "tradable": true,
                    "fractionable": true,
                    "shortable": true,
                    "asset_class": "us_equity",
                })
            })
            .collect(),
    )
}

// Returns fake positions.
fn positions_json(state: &FakeAlpacaState) -> Value {
    Value::Array(
        state
            .positions
            .values()
            .map(|position| state.position_json(position))
            .collect(),
    )
}

// Returns fake orders with basic status/limit filtering.
fn orders_json(state: &FakeAlpacaState, query: &HashMap<String, String>) -> Value {
    let status = query.get("status").map(String::as_str).unwrap_or("all");
    let limit = query
        .get("limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(500);
    let mut orders = state
        .orders
        .iter()
        .filter(|order| {
            status == "all"
                || order.get("status").and_then(Value::as_str) == Some(status)
                || (status == "closed"
                    && order.get("status").and_then(Value::as_str) == Some("filled"))
        })
        .cloned()
        .collect::<Vec<_>>();
    orders.reverse();
    orders.truncate(limit);
    Value::Array(orders)
}

// Returns the latest NBBO-like quote.
fn quote_json(state: &FakeAlpacaState, symbol: &str) -> Value {
    let price = state.latest_price(symbol);
    json!({
        "quote": {
            "bp": round2(price - 0.02),
            "ap": round2(price + 0.02),
            "bs": 100.0,
            "as": 100.0,
            "t": "2026-05-01T15:30:00Z"
        }
    })
}

// Returns the latest stock snapshot.
fn snapshot_json(state: &FakeAlpacaState, symbol: &str) -> Value {
    let bars = state.bars.get(symbol).cloned().unwrap_or_default();
    let latest = bars.last().cloned().unwrap_or_else(|| json!({}));
    let previous = bars
        .iter()
        .rev()
        .nth(1)
        .cloned()
        .unwrap_or_else(|| latest.clone());
    json!({
        "dailyBar": latest,
        "prevDailyBar": previous,
        "latestTrade": {
            "p": latest.get("c").and_then(Value::as_f64).unwrap_or(0.0),
            "t": "2026-05-01T15:30:00Z"
        }
    })
}

// Filters fixture bars by simple start/end/limit query parameters.
fn filtered_bars(
    state: &FakeAlpacaState,
    symbol: &str,
    query: &HashMap<String, String>,
) -> Vec<Value> {
    let start = query
        .get("start")
        .map(|value| value.chars().take(10).collect::<String>());
    let end = query
        .get("end")
        .map(|value| value.chars().take(10).collect::<String>());
    let limit = query
        .get("limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(10_000);
    let sort_desc = query.get("sort").map(String::as_str) == Some("desc");
    let mut bars = state.bars.get(symbol).cloned().unwrap_or_default();
    bars.retain(|bar| {
        let date = bar
            .get("t")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(10)
            .collect::<String>();
        start.as_ref().map(|start| date >= *start).unwrap_or(true)
            && end.as_ref().map(|end| date < *end).unwrap_or(true)
    });
    if sort_desc {
        bars.reverse();
    }
    bars.truncate(limit);
    bars
}

// Returns fake Alpaca news.
fn news_json(query: &HashMap<String, String>) -> Value {
    let symbols = query
        .get("symbols")
        .map(|value| split_symbols(value))
        .filter(|symbols| !symbols.is_empty())
        .unwrap_or_else(|| vec!["AAPL".to_string(), "NVDA".to_string()]);
    let mut news = Vec::new();
    for symbol in symbols {
        news.push(json!({
            "headline": format!("Fake Alpaca news for {symbol}"),
            "source": "fake_alpaca",
            "created_at": "2026-05-01T13:00:00Z",
            "summary": format!("{symbol} fixture news used by mlai-trade provider tests."),
            "symbols": [symbol],
            "url": format!("https://fake.alpaca.local/news/{symbol}"),
        }));
    }
    json!({"news": news, "next_page_token": null})
}

// Returns fake movers.
fn movers_json() -> Value {
    json!({
        "gainers": [
            {"symbol": "NVDA", "price": 135.25, "change": 4.10, "percent_change": 3.12},
            {"symbol": "AAPL", "price": 192.42, "change": 2.15, "percent_change": 1.13}
        ],
        "losers": [
            {"symbol": "XLF", "price": 42.10, "change": -0.42, "percent_change": -0.99}
        ]
    })
}

// Returns fake v3 clock response.
fn clock_json() -> Value {
    json!({
        "clocks": [
            {
                "is_market_day": true,
                "market": {"acronym": "NYSE", "mic": "XNYS", "name": "New York Stock Exchange", "timezone": "America/New_York"},
                "next_market_close": "2026-05-01T20:00:00Z",
                "next_market_open": "2026-05-04T13:30:00Z",
                "phase": "open",
                "phase_until": "2026-05-01T20:00:00Z",
                "timestamp": "2026-05-01T15:30:00Z"
            }
        ]
    })
}

// Returns fake v3 calendar response.
fn calendar_json(market: &str, query: &HashMap<String, String>) -> Value {
    let start = query
        .get("start")
        .map(String::as_str)
        .unwrap_or("2026-05-01");
    json!({
        "market": {"acronym": market, "mic": market, "name": format!("{market} fixture market"), "timezone": "America/New_York"},
        "calendar": [
            {
                "date": start,
                "core_start": format!("{start}T13:30:00Z"),
                "core_end": format!("{start}T20:00:00Z"),
                "pre_start": format!("{start}T08:00:00Z"),
                "pre_end": format!("{start}T13:30:00Z"),
                "post_start": format!("{start}T20:00:00Z"),
                "post_end": format!("{start}T23:59:00Z"),
                "settlement_date": start
            }
        ]
    })
}

// Builds the one-month fake tradable universe.
fn fixture_assets() -> Vec<FakeAsset> {
    vec![
        FakeAsset {
            symbol: "AAPL",
            name: "Fake Apple Inc",
            exchange: "NASDAQ",
            base: 188.0,
            drift: 0.18,
            wiggle: 0.65,
            volume: 62_000_000,
        },
        FakeAsset {
            symbol: "MSFT",
            name: "Fake Microsoft Corp",
            exchange: "NASDAQ",
            base: 420.0,
            drift: 0.22,
            wiggle: 0.72,
            volume: 31_000_000,
        },
        FakeAsset {
            symbol: "NVDA",
            name: "Fake NVIDIA Corp",
            exchange: "NASDAQ",
            base: 128.0,
            drift: 0.35,
            wiggle: 1.05,
            volume: 58_000_000,
        },
        FakeAsset {
            symbol: "GOOG",
            name: "Fake Alphabet Inc",
            exchange: "NASDAQ",
            base: 160.0,
            drift: 0.12,
            wiggle: 0.51,
            volume: 22_000_000,
        },
        FakeAsset {
            symbol: "IBM",
            name: "Fake IBM Corp",
            exchange: "NYSE",
            base: 225.0,
            drift: 0.08,
            wiggle: 0.38,
            volume: 4_000_000,
        },
        FakeAsset {
            symbol: "SPY",
            name: "Fake SPDR S&P 500 ETF",
            exchange: "ARCA",
            base: 520.0,
            drift: 0.11,
            wiggle: 0.42,
            volume: 72_000_000,
        },
        FakeAsset {
            symbol: "QQQ",
            name: "Fake Invesco QQQ ETF",
            exchange: "NASDAQ",
            base: 450.0,
            drift: 0.15,
            wiggle: 0.55,
            volume: 56_000_000,
        },
        FakeAsset {
            symbol: "XLK",
            name: "Fake Technology Select Sector ETF",
            exchange: "ARCA",
            base: 212.0,
            drift: 0.09,
            wiggle: 0.30,
            volume: 12_000_000,
        },
        FakeAsset {
            symbol: "XLF",
            name: "Fake Financial Select Sector ETF",
            exchange: "ARCA",
            base: 42.0,
            drift: 0.02,
            wiggle: 0.08,
            volume: 36_000_000,
        },
        FakeAsset {
            symbol: "IWM",
            name: "Fake Russell 2000 ETF",
            exchange: "ARCA",
            base: 206.0,
            drift: 0.06,
            wiggle: 0.28,
            volume: 29_000_000,
        },
    ]
}

// Builds one month of deterministic business-day bars.
fn fixture_bars(assets: &[FakeAsset]) -> HashMap<String, Vec<Value>> {
    let mut out = HashMap::new();
    let start = NaiveDate::from_ymd_opt(2026, 4, 1).expect("valid fixture date");
    for asset in assets {
        let mut bars = Vec::new();
        for day in 0..35 {
            let date = start + Duration::days(day);
            if date.weekday().number_from_monday() > 5 {
                continue;
            }
            let idx = bars.len() as f64;
            let close =
                asset.base + idx * asset.drift + ((idx as i64 % 7) - 3) as f64 * asset.wiggle;
            let open = close * 0.997;
            let high = close * 1.012;
            let low = close * 0.988;
            let volume = asset.volume + (idx as u64 % 9) * 10_000;
            bars.push(json!({
                "t": format!("{}T00:00:00Z", date.format("%Y-%m-%d")),
                "o": round4(open),
                "h": round4(high),
                "l": round4(low),
                "c": round4(close),
                "v": volume,
                "vw": round4((open + high + low + close) / 4.0)
            }));
        }
        out.insert(asset.symbol.to_string(), bars);
    }
    out
}

// Parses a simple query string.
fn query_map(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter_map(|part| part.split_once('='))
        .map(|(key, value)| (key.to_string(), value.replace("%2F", "/")))
        .collect()
}

// Splits comma-delimited symbols and normalizes case.
fn split_symbols(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|symbol| symbol.trim().to_ascii_uppercase())
        .filter(|symbol| !symbol.is_empty())
        .collect()
}

// Reads a numeric string field from a JSON body.
fn string_number(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(|value| match value {
        Value::String(value) => value.parse::<f64>().ok(),
        Value::Number(value) => value.as_f64(),
        _ => None,
    })
}

// Formats f64 as Alpaca-style string money.
fn money(value: f64) -> String {
    format!("{value:.4}")
}

// Formats f64 as Alpaca-style string quantity.
fn qty(value: f64) -> String {
    format!("{value:.6}")
}

// Rounds to two decimals.
fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

// Rounds to four decimals.
fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

// Builds a JSON HTTP response.
fn json_response(status: StatusCode, value: Value) -> Response {
    (status, axum::Json(value)).into_response()
}
