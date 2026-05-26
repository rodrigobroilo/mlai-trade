# Alpaca Trading API Knowledge Base
> Compiled from 9 Alpaca blog posts on trading strategies, API patterns, best practices, and source-checked tax/compliance rules.
> Last updated: May 2, 2026
> Source verification: Alpaca articles, FINRA, SEC, IRS, FinCEN, and California FTB pages checked May 2, 2026

---

## 1. Executive Summary

After reading all 9 Alpaca trading blog posts, here are the key takeaways:

**Strategies Covered:**
- 0DTE Bull Put Spread (options, delta-based selection)
- Multi-Strategy Backtesting Dashboard (6 strategies across equities + crypto)
- Bollinger Bands Mean Reversion
- MACD Crossover (signal line, centerline, histogram)
- Covered Call (via TradingView)
- GTC Options Orders
- VWAP/TWAP Algorithmic Execution

**Key API Features Learned:**
- Advanced order types: VWAP, TWAP (Elite Smart Router only)
- GTC orders now supported for options (limit orders only)
- Notional orders for dollar-based position sizing
- Trailing stop orders with `trail_price` or `trail_percent`
- Bracket orders (OTO, OCO) for automated take-profit/stop-loss
- Extended hours trading (limit orders only, DAY or GTC)
- 24/5 overnight trading with BOATS/Overnight feeds
- MCP Server for AI-assisted trading workflows
- `client_order_id` for idempotent order tracking
- `replace_order_by_id` for modifying orders in-place

**Critical Gotchas:**
- Free tier: SIP feed NOT available for real-time snapshots (use IEX or default)
- Fractional orders: MUST be DAY market orders, minimum $1.00 notional
- GTC options: limit orders only, auto-cancelled after 90 days
- Extended hours: only DAY or GTC limit orders allowed
- Race conditions: wait for cancel confirmation before closing positions
- PDT/day-trading margin rules change on June 4, 2026; query broker account state instead of hard-coding the old $25K/4-day-trade rule
- `notional` and `qty` are mutually exclusive parameters

### Source-Verified Operations Updates (May 2, 2026)

- Rust binary operations, artifact names, versioning, daily gap-aware refresh, and the non-trading ML workflow are documented in `ml/alpaca-rust-operations.md`.

These are not Alpaca blog conclusions; they are implementation constraints learned from primary tax, securities, and broker-rule sources.

**Day-Trading Rules**
- Through June 3, 2026, the classic PDT framework still matters for margin accounts: 4+ day trades in 5 business days can trigger the $25,000 minimum-equity regime.
- FINRA Notice 26-10 and SEC Release 34-105226 approve a new day-trading framework effective June 4, 2026. The old PDT minimum-equity designation is replaced by intraday margin standards.
- FINRA permits broker-dealer implementation phase-in through October 20, 2027, so production code must read broker/account restrictions and not assume every account changes behavior on the same day.
- Cash-only operation remains the simplest default for this project. If margin is ever enabled, track intraday exposure, calls, restrictions, and forced-liquidation risk.

**Tax-Aware Trading Records**
- Store every order, replacement, cancel request, cancel confirmation, rejection, fill, broker error payload, and `client_order_id`.
- `client_order_id` should be deterministic/idempotent and persisted with strategy name, code version, model version, parameters, signal timestamp, and risk decision.
- Export realized-lot records compatible with Form 8949/Schedule D workflows: symbol, CUSIP if available, acquisition date, disposition date, proceeds, cost basis, adjustment code, adjustment amount, and gain/loss.
- Reconcile internal lots against broker 1099-B data. Broker wash-sale reporting can miss cross-account, spouse, IRA, and non-identical-but-substantially-identical replacement positions.

**Wash Sale Controls**
- A loss sale requires checking the full 61-day statutory window: 30 days before the sale, the sale date, and 30 days after.
- The engine should flag prior replacement buys, block or explicitly approve replacement buys for 30 days after a loss, adjust replacement-share basis, carry holding period where required, and mark Form 8949 adjustment code `W`.
- `mlai-trade` treats the IRS 30-day forward window as a hardcoded floor and adds a configurable safety buffer that defaults to 1 day. Config cannot reduce the effective block below 31 calendar days.
- Tax lots must survive partial fills, partial exits, corporate actions, and symbol-level replacement logic. A symbol-only check is useful but not sufficient for reporting.

**Cash, Margin, And Settlement**
- For cash accounts, track settled cash separately from buying power to avoid good-faith and free-riding violations.
- For margin accounts, track broker margin restrictions, day-trading status through June 3, 2026, and intraday margin behavior from June 4, 2026 onward.
- California taxes capital gains as ordinary income and does not provide the federal long-term capital-gains rate preference, so reporting snapshots should include federal and California views.
- Estimated-tax snapshots are useful when expected tax owed may exceed IRS safe-harbor thresholds.

**Options Scope**
- This Rust Alpaca tool must not trade options. Options are disabled by hard rule in source code, even though Alpaca supports options endpoints.
- If a future project ever adds options, it must be a separate reviewed implementation that handles equity options, index options, Section 1256 contracts, assignment/exercise, expiration, wash-sale interactions, and options-specific broker permissions.

**ML Data And Memory**
- The scanner should default to approximately 9 years of daily Alpaca stock bars for ML training history.
- Bulk bar downloads must write bounded fetch waves to SQLite instead of retaining the full universe in memory.
- Feature generation should insert per symbol/per batch instead of accumulating all feature rows in RAM.
- Prediction should score rows as they stream out of SQLite and retain only the ranking data needed for quintiles/output.
- LSTM training may use all qualifying symbols from local Alpaca history, but sequence sampling remains necessary to keep memory bounded.

**Daemon Daily Prep**
- The daemon should treat post-market daily prep as a market-session event, not a magic wall-clock cron time.
- Default behavior is `daily_refresh_trigger=market_close`: run once per open market date after configured regular close plus a safety offset, currently 60 minutes.
- Fixed `daily_refresh_time` remains useful only as an explicit fallback/override mode.
- The prep path is non-trading: sync provider history, reconcile/sync feed symbols, fill missing/latest SIP/IEX bars, compute features/labels, train/evaluate models, refresh predictions/ensemble/SHAP, then refresh tax estimates.
- Closed-market auto-trade backoff must not suppress this prep path; the system still needs data and models ready for the next business day.

**FRED S&P 500 Benchmark Data**
- The FRED API is appropriate for macro and benchmark series that should be stored separately from Alpaca tradable assets.
- The S&P 500 index series is `SP500`; use `GET https://api.stlouisfed.org/fred/series/observations`.
- Required request fields for this CLI: `series_id=SP500`, `api_key=<fred.api_key from mlai-trade.json>`, `file_type=json`, `observation_start=YYYY-MM-DD`, `sort_order=asc`, and `limit=100000`.
- FRED observations may contain missing values such as `.`; skip non-numeric values before writing to SQLite.
- Store FRED data in a macro/benchmark table such as `macro_series`, not in Alpaca OHLCV `bars`, because S&P 500 index observations are not an orderable Alpaca equity.

**Primary Sources**
- FINRA Notice 26-10: https://www.finra.org/rules-guidance/notices/26-10
- SEC Release 34-105226: https://www.sec.gov/files/rules/sro/finra/2026/34-105226.pdf
- IRS Publication 550: https://www.irs.gov/publications/p550
- IRS Form 8949 instructions: https://www.irs.gov/instructions/i8949
- IRS Publication 505: https://www.irs.gov/publications/p505
- California FTB capital gains: https://www.ftb.ca.gov/file/personal/income-types/capital-gains-and-losses.html
- FRED API overview: https://fred.stlouisfed.org/docs/api/fred/
- FRED series observations: https://fred.stlouisfed.org/docs/api/fred/series_observations.html

### Official Alpaca API Reference Notes (May 2, 2026)

These notes are from the official Alpaca API reference, not blog examples.

**Trading API vs Broker API**
- This Rust CLI is an individual Trading API client, not a Broker API integration.
- Trading API authentication uses the `APCA-API-KEY-ID` and `APCA-API-SECRET-KEY` headers.
- Broker API reference pages use sandbox broker endpoints such as `broker-api.sandbox.alpaca.markets/v1/...` and Basic auth. Do not mix those with this individual Trading API client.

**Broker/Trading Endpoints Used By This CLI**
- Paper trading base: `https://paper-api.alpaca.markets/v2`
- Live individual trading base: `https://api.alpaca.markets` plus `/v2/...` paths.
- Account: `GET /account`
- Assets: `GET /assets`
- Orders: `GET /orders`, `POST /orders`, `DELETE /orders`, `DELETE /orders/{order_id}`
- Positions: `GET /positions`, `GET /positions/{symbol_or_asset_id}`, `DELETE /positions`, `DELETE /positions/{symbol_or_asset_id}`
- Clock: `GET /clock`

**Market Data Endpoints Used By This CLI**
- Market data is separate from broker/trading endpoints.
- Stock latest quote: `GET https://data.alpaca.markets/v2/stocks/{symbol}/quotes/latest`
- The latest quote endpoint supports feed selection such as `sip`, `iex`, `delayed_sip`, `boats`, `overnight`, and `otc`; free/default behavior may resolve to IEX when SIP is unavailable.

**Order Reference Details To Preserve In Code**
- `GET /orders` defaults to open orders, supports `status`, `limit`, `after`, `until`, `direction`, `nested`, `symbols`, `side`, `asset_class`, `before_order_id`, and `after_order_id`.
- Alpaca can generate a `client_order_id`, but this project should supply and persist deterministic IDs for idempotency and audit trails.
- Buying power is reduced by open long-buy and short-sell opening orders until those orders execute or cancel.
- Extended-hours orders have stricter eligibility and liquidity risks; use explicit limits and keep regular-hours-only behavior unless intentionally enabled.
- GTC orders are subject to Alpaca's aged-order policy and can be canceled after 90 days.

---

## 2. Per-Article Detailed Notes

### Article 1: Backtesting 0DTE Bull Put Spread Options Strategy
**Date:** Aug 26, 2025
**Source:** https://alpaca.markets/learn/backtesting-zero-dte-bull-put-spread-options-strategy-with-python

#### Key Concepts
- **0DTE (Zero Days to Expiration):** Options that expire on the same day they're traded
- **Bull Put Spread:** Sell higher-strike put (collect premium) + buy lower-strike put (limit risk) = net credit
- Uses dual API integration: Alpaca for stock data + Databento for OPRA options tick data
- Backtests at 1-minute intervals to find valid option pairs

#### Strategy Parameters
- **Underlying:** SPY
- **Short Put Delta Range:** -0.60 to -0.20
- **Long Put Delta Range:** -0.40 to -0.20
- **Strike Spread:** $2-$4 configurable
- **Buffer %:** ±5% around daily high/low for strike range
- **Stop Loss:** 2x initial delta OR 50% of initial credit
- **Risk Free Rate:** 0.01

#### Selection Process
1. Scan historical data chronologically at 1-minute intervals
2. Find first valid option pair meeting delta criteria
3. Hold until exit condition (stop loss, profit target, or expiration)
4. Repeat

#### Code Patterns
- `StockBarsRequest` for daily OHLCV data
- Strike price formatting: `SPY{YYMMDD}P{strike*1000:08d}` (e.g., `SPY250616P00571000`)
- Options late close: SPY gets +15 minutes added to market close time
- Uses `TradingClient`, `StockHistoricalDataClient`, and Databento's historical client

#### Practical Takeaway
- Need Databento subscription for OPRA options data (usage-based pricing)
- Alpaca alone provides stock bars but not historical options tick data
- The backtest generates cumulative P&L charts for strategy evaluation

---

### Article 2: From Value Investing to Systematic Trading — Multi-Strategy Dashboard
**Date:** Apr 9, 2026
**Source:** https://alpaca.markets/learn/from-value-investing-to-systematic-trading-building-a-multi-strategy-backtesting-dashboard-with-ai-and-alpaca

#### Key Concepts
- Non-programmer built 6 trading strategies using AI (Claude Code) as co-developer
- Interactive backtesting dashboard with parameter sliders for real-time re-simulation
- Architecture: Data ingestion → Signal computation → Simulation & rendering

#### Why Alpaca (from the author's perspective)
- **Free data:** 2+ years daily OHLCV at no cost via Market Data API
- **Paper = Live:** Same API endpoints, same order types, one-line config change to go live
- **Simple REST API:** Standard HTTP + JSON, no proprietary SDK needed
- **Commission-free:** Critical for small position sizes ($500 seed, 3-5% positions)
- **Rate limits:** "Generous enough that you will not hit them during normal backtesting"
- **IEX feed (free tier):** Sufficient for daily strategies
- **WebSocket streaming:** "Remarkably stable" for live monitoring

#### Dashboard Architecture
1. **Data Ingestion:** REST calls to Market Data API → daily OHLCV bars → clean JSON
2. **Signal Computation:** Composite scoring (multiple indicators weighted), regime detection, dynamic volatility-adjusted thresholds
3. **Simulation:** Position sizing, configurable slippage, trailing stops, capital allocation limits

#### 6 Strategies Mentioned
1. **Buffett Value Strategy** (equities) — RSI buy threshold, dip below SMA, min entry score, trailing stop
2. **Mean Reversion** (equities)
3. **Momentum** (equities)
4. Three crypto strategies (via Jupiter DEX and Kraken)

#### Key Design Principles
- Keep Alpaca data layer separate from strategy logic (swap data sources without touching strategy code)
- Parameter sliders for instant re-simulation (understand what each parameter does)
- **Overfitting warning:** Best backtest parameters ≠ best live parameters
- Discord webhooks for trade notifications

#### Tech Stack
- TypeScript + Node.js (chosen over Python for type safety)
- Claude Code as AI co-developer
- Discord for notifications
- MCP Server for AI-assisted portfolio queries

---

### Article 3: Bollinger Bands Strategy with Alpaca MCP Server
**Date:** Apr 6, 2026
**Source:** https://alpaca.markets/learn/how-to-build-backtest-bollinger-bands-strategy-with-alpaca-mcp-server

#### Bollinger Bands Formula
- **Middle Band:** 20-period SMA of closing prices
- **Upper Band:** Middle + (2 × 20-period standard deviation)
- **Lower Band:** Middle - (2 × 20-period standard deviation)
- Bands expand during high volatility, contract during low volatility

#### Mean Reversion Strategy Rules
- **Buy Signal:** Close crosses BELOW lower Bollinger Band (oversold)
- **Exit Signal:** Close crosses ABOVE upper Bollinger Band (overbought)
- **Hold:** Between bands, maintain current position

#### Code Implementation
```python
def calculate_bollinger_bands(df, window=20, num_std=2):
    df["bb_middle"] = df["close"].rolling(window=window).mean()
    df["bb_std"] = df["close"].rolling(window=window).std()
    df["bb_upper"] = df["bb_middle"] + (num_std * df["bb_std"])
    df["bb_lower"] = df["bb_middle"] - (num_std * df["bb_std"])
    return df
```

#### Vectorized Backtest Pattern
```python
df["position"] = df["signal"].replace(0, np.nan).ffill().fillna(0)
df["daily_return"] = df["close"].pct_change()
df["strategy_return"] = df["position"].shift(1) * df["daily_return"]
df["cumulative_strategy"] = (1 + df["strategy_return"]).cumprod()
```

#### Performance Metrics Calculated
- Total Return, Annualized Return, Annualized Volatility
- Sharpe Ratio (assuming 0% risk-free rate)
- Maximum Drawdown
- Number of Signals

#### Enhancement: Adding RSI Filter
- Combine Bollinger Bands with RSI to reduce false signals
- Modified buy: close < lower band AND RSI < 30
- Modified sell: close > upper band AND RSI > 70
```python
df["rsi"] = compute_rsi(df["close"], period=14)
buy_signal = (df["close"] < df["bb_lower"]) & (df["rsi"] < 30)
sell_signal = (df["close"] > df["bb_upper"]) & (df["rsi"] > 70)
```

#### MCP Server Usage
- Natural language prompts to fetch data, calculate indicators, evaluate signals
- Example: "Extract historical market data for AAPL over the past 90 days"
- Translates prompts into structured API requests

---

### Article 4: Deploy Alpaca MCP Server Remotely on Claude Mobile
**Date:** Nov 20, 2025
**Source:** https://alpaca.markets/learn/how-to-deploy-alpaca-mcp-server-remotely-on-claude-mobile-app

#### MCP Server Capabilities
- Pull account details, positions, unrealized P&L, market data, news
- Analyze market trends and earnings with AI reasoning
- Build/refine trading algorithms in connected workflow
- Place trades through natural language

#### Remote Deployment Steps
These steps describe the external Alpaca MCP article, not `mlai-trade` runtime configuration. `mlai-trade` uses `~/mlai-trade/config/mlai-trade.json`.

1. Clone the external `mlai-trade-mcp-server` repo
2. Deploy on a cloud service (Render example)
3. Set env vars: `ALPACA_API_KEY`, `ALPACA_SECRET_KEY`, `HOST=0.0.0.0`
4. Start the server with the article's recommended runtime command
5. Connect to Claude via custom MCP connector URL: `https://your-app.onrender.com/mcp`

#### Security Considerations
- Remote MCP servers must use HTTPS/TLS
- Token-based authentication recommended
- Protect API keys from unauthorized access

#### Practical Note
- MCP Server GitHub: https://github.com/alpacahq/alpaca-mcp-server
- Works with Claude Desktop, Claude Mobile, Cursor, VS Code, ChatGPT
- We already have the MCP server installed via `uvx alpaca-mcp-server`

---

### Article 5: 30 Common Trading API Errors — Error Cheat Sheet
**Date:** Mar 4, 2026
**Source:** https://alpaca.markets/learn/how-to-fix-common-trading-api-errors-at-alpaca

This is the most practically useful article. Complete error reference below:

#### Authentication & Authorization Errors (1-3)
| # | Error | HTTP | Fix |
|---|-------|------|-----|
| 1 | `"unauthorized"` | 401 | Wrong API key/secret or wrong environment (paper vs live). Headers: `APCA-API-KEY-ID` + `APCA-API-SECRET-KEY` |
| 2 | `"insufficient permission"` | 403 | Account-level restriction: ACH return, crypto/options/shorting not enabled, PDT protection |
| 3 | `"subscription does not permit querying recent SIP data"` | 403 | Need Algo Trader Plus plan for SIP data. Free plan: use `feed=DataFeed.OVERNIGHT` or `DataFeed.IEX` |

#### Funding Errors (4-7)
| # | Error | HTTP | Fix |
|---|-------|------|-----|
| 4 | Service Unavailable | 503 | Scheduled maintenance. Check status.alpaca.markets |
| 5 | Gateway Timeout | 504 | Backend slow. Narrow time windows, paginate queries. Don't blindly resend orders |
| 6 | Insufficient funds for withdrawal | 400 | Fees exceed balance. Verify available funds first |
| 7 | Server error in funding | 500 | Transient. Retry later or contact support |

#### Order Parameter Errors (8-24)
| # | Error | HTTP | Fix |
|---|-------|------|-----|
| 8 | `"invalid order type"` | 422 | Valid types: market, limit, stop, stop_limit, trailing_stop |
| 9 | `"invalid time_in_force"` | 422 | **Equities:** day, gtc, opg, cls, ioc, fok. **Options:** day, gtc. **Crypto:** gtc, ioc |
| 10 | `"market orders require no stop or limit price"` | 422 | Don't set limit_price/stop_price on market orders |
| 11 | `"client_order_id must be unique"` | 422 | Use unique ID per active order |
| 12 | `"qty or notional is required"` | 422 | Provide one (mutually exclusive). Notional requires DAY TIF |
| 13 | `"stop orders require a stop price"` | 422 | Set stop_price parameter |
| 14 | `"limit orders require a limit price"` | 422 | Set limit_price parameter |
| 15 | `"stop limit orders require both stop and limit price"` | 422 | Set both stop_price and limit_price |
| 16 | `"trailing stop orders must specify one of trail_price or trail_percent"` | 422 | Provide exactly one |
| 17 | `"extended hours order must be DAY or GTC limit orders"` | 422 | Extended hours = limit orders only, DAY or GTC |
| 18 | `"market orders must not have trail_price"` | 422 | Remove trail fields from market orders |
| 19 | `"market orders require no stop or limit price"` | 422 | Same as #10 |
| 20 | `"fractional orders must be DAY orders"` | 422 | Fractional = DAY TIF only |
| 21 | `"fractional orders cannot be sold short"` | 422 | Can only sell fractional from existing long position |
| 22 | `"notional must be >= 1.00"` | 422 | Minimum $1.00 for notional orders |
| 23 | `"fractional orders must be DAY orders"` | 422 | Same as #20, GTC not allowed for fractional |
| 24 | `"asset is not fractionable"` | 403 | Check `fractionable: true` via `get_asset()` |

#### Account/Buying Power/Risk Errors (25-30)
| # | Error | HTTP | Fix |
|---|-------|------|-----|
| 25 | `"insufficient buying power"` | 403 | Reduce qty/notional or add funds. Check `account.buying_power` |
| 26 | `"insufficient qty available for order"` | 403 | **5 scenarios:** (a) shares held by open orders, (b) bracket order child orders holding shares, (c) race condition between cancel and close, (d) boxed long+short positions, (e) fat finger selling more than owned |
| 27 | `"account is not allowed to short"` | 403 | Enable shorting + set margin multiplier > 1 |
| 28 | `"account is not authorized to trade"` | 403 | Account not in active trading state |
| 29 | `"symbol not found or not tradable"` | 422 | Symbol delisted, invalid, or not supported |
| 30 | Day-trading account restriction | 403 | Before June 4, 2026: classic PDT restrictions may block margin accounts below $25K after 4+ day trades in 5 business days. From June 4, 2026: FINRA intraday margin standards apply, with broker phase-in possible through October 20, 2027. Query account restrictions and broker messages. |

#### Critical Race Condition Pattern (Error #26)
```
# WRONG: Race condition
DELETE /v2/orders          # cancel all
DELETE /v2/positions/SPY   # close immediately → FAILS (cancels not processed)

# RIGHT: Wait for confirmation
cancel_orders()
wait_for_cancellation()  # verify orders are actually cancelled
close_position()
```

#### Market Data Feed Reference
| Plan | Feed | Data Type | Delay |
|------|------|-----------|-------|
| Algo Trader Plus | `boats` | Latest Quotes/Trades, Snapshots, Historical | Real-time |
| Free | `overnight` | Latest Bars, Quotes (indicative), Trades | 15-min delayed |
| Free | `boats` | Historical only | 15-min delayed |
| Free | `iex` | Quotes/Trades | Real-time (IEX only) |
| Free | `sip` | Historical bars | Works for historical |

---

### Article 6: MACD Crossovers with Alpaca
**Date:** Apr 8, 2026
**Source:** https://alpaca.markets/learn/how-to-implement-macd-crossovers-with-alpaca

#### MACD Components
- **MACD Line:** EMA(close, 12) - EMA(close, 26)
- **Signal Line:** EMA(MACD Line, 9)
- **Histogram:** MACD Line - Signal Line
- Standard parameters: (12, 26, 9)

#### Three Crossover Types
1. **Signal Line Crossover** (most common)
   - Bullish: MACD crosses above signal line
   - Bearish: MACD crosses below signal line
2. **Centerline (Zero Line) Crossover**
   - Bullish: MACD goes from negative to positive (12 EMA > 26 EMA)
   - Bearish: MACD goes from positive to negative
3. **Histogram Reversal**
   - Bars shrinking toward zero = crossover approaching

#### Implementation
```python
def compute_macd(df, fast=12, slow=26, signal=9):
    ema_fast = df["close"].ewm(span=fast, adjust=False).mean()
    ema_slow = df["close"].ewm(span=slow, adjust=False).mean()
    df["macd_line"] = ema_fast - ema_slow
    df["signal_line"] = df["macd_line"].ewm(span=signal, adjust=False).mean()
    df["histogram"] = df["macd_line"] - df["signal_line"]
    return df
```

#### Crossover Detection
```python
def detect_signal_crossover(df):
    prev = df.iloc[-2]
    curr = df.iloc[-1]
    prev_diff = prev["macd_line"] - prev["signal_line"]
    curr_diff = curr["macd_line"] - curr["signal_line"]
    if prev_diff <= 0 and curr_diff > 0:
        return "bullish"
    if prev_diff >= 0 and curr_diff < 0:
        return "bearish"
    return "none"
```

#### Order Submission Pattern
```python
# Market order on crossover
req = MarketOrderRequest(
    symbol=symbol,
    qty=qty,
    side=OrderSide.BUY,  # or SELL
    type=OrderType.MARKET,
    time_in_force=TimeInForce.DAY
)
trading_client.submit_order(req)

# Limit order for price control
req = LimitOrderRequest(
    symbol=symbol,
    qty=qty,
    side=OrderSide.BUY,
    type=OrderType.LIMIT,
    time_in_force=TimeInForce.GTC,
    limit_price=target_price
)
```

#### Position Management
```python
# Check existing positions before trading
try:
    position = trading_client.get_open_position(symbol)
    current_qty = float(position.qty)
except:
    current_qty = 0
```

#### MACD Limitations
- Lagging indicator (signals arrive after trend started)
- False signals in sideways/choppy markets (whipsaws)
- No upper/lower boundaries (unlike RSI)
- Should combine with other indicators (volume, support/resistance, RSI)

#### Data Requirement
- 120 days of daily bars for stable 26-period EMA computation

---

### Article 7: GTC Orders for Options Trading
**Date:** Mar 5, 2026
**Source:** https://alpaca.markets/learn/how-to-trade-good-til-canceled-options-trading-with-python-and-alpaca

#### GTC for Options — Key Facts
- **Only limit orders** supported for GTC options (not market orders — price volatility risk)
- **Auto-expires after 90 days** (modifying resets the clock)
- **Ties up buying power** until filled or cancelled
- **No extended hours fills** — options trade 9:30 AM - 4:00 PM ET only
- **Stock splits:** Forward splits adjust price/qty; reverse splits CANCEL all GTC orders
- **Dividends:** Buy Limit/Sell Stop prices auto-reduced by dividend amount (unless marked DNR)
- **Modify in-place:** Use `replace_order_by_id` to update limit price without cancelling

#### Options Contract Discovery
```python
# Find available contracts
req = GetOptionContractsRequest(
    underlying_symbols=["SPY"],
    status=AssetStatus.ACTIVE,
    type=ContractType.CALL,
    # Strike range: 98-102% of underlying price
    # Expiration: 12-22 days out
)
contracts = trade_client.get_option_contracts(req).option_contracts

# Get latest quote for an option
req = OptionLatestQuoteRequest(
    symbol_or_symbols="SPY26MMDDC00687000",
    feed=OptionsFeed.INDICATIVE
)
quotes = option_data_client.get_option_latest_quote(req)
```

#### GTC Limit Order Placement
```python
order_request = LimitOrderRequest(
    symbol="SPY26MMDDC00687000",
    qty=1,
    side=OrderSide.BUY,
    type=OrderType.LIMIT,
    time_in_force=TimeInForce.GTC,
    limit_price=quotes[option_symbol].ask_price
)
trade_client.submit_order(order_request)
```

#### Options Symbol Format
- `SPY{YYMMDD}{C|P}{strike*1000:08d}`
- Example: `SPY260315C00687000` = SPY Mar 15 2026 $687 Call

---

### Article 8: Options Trading on TradingView with Alpaca
**Date:** Oct 28, 2025
**Source:** https://alpaca.markets/learn/how-to-trade-options-on-tradingview-with-alpaca-trading-api-account

#### TradingView Integration Facts
- Supports stocks, ETFs, crypto, AND options
- **No multi-leg options** on TradingView (must use API or dashboard)
- **TIF for options on TradingView:** Day only (not GTC at time of writing)
- Order types available: Market and Limit
- Strategy Builder for payoff diagram visualization
- Depth of Market (DOM) for limit order placement via price ladder

#### Covered Call Example
- Own underlying stock + sell OTM call
- Parameters: underlying price, strike price, expiration date
- Strategy Builder shows: win rate, breakeven, delta, max profit/loss

#### Connection Flow
1. TradingView Trading Panel → Select Alpaca as broker
2. Authorize connection (supports both live and paper)
3. Apply "Options" filter when searching symbols
4. Select specific contract from options chain
5. Place order via Trade button or DOM

---

### Article 9: VWAP and TWAP Orders with Alpaca Elite
**Date:** Aug 21, 2025
**Source:** https://alpaca.markets/learn/optimize-your-orders-with-vwap-and-twap-on-alpaca

#### VWAP (Volume-Weighted Average Price)
- **Purpose:** Execute proportional to market volume to minimize impact
- **Formula:** Σ(Volume × Price) / Σ(Volume)
- **Use case:** Large orders where you want to match market activity
- **74% of hedge funds** use VWAP (2025 survey)
- Paces trades to never exceed a configurable % of total volume

#### TWAP (Time-Weighted Average Price)
- **Purpose:** Execute evenly over time regardless of volume
- **Formula:** Σ(Prices at equal intervals) / Number of intervals
- **Use case:** Predictable pacing, low-liquidity environments
- **42% of hedge funds** use TWAP

#### API Implementation
```python
# VWAP Market Order
req = MarketOrderRequest(
    symbol="SPY",
    qty=5000,
    side=OrderSide.BUY,
    type=OrderType.MARKET,
    order_class=OrderClass.SIMPLE,
    advanced_instructions={
        "algorithm": "VWAP",
        "start_time": "2025-07-21T09:30:00-04:00",
        "end_time": "2025-07-21T14:30:00-04:00",
        "max_percentage": "0.123"  # Max 12.3% of ticker volume
    }
)

# TWAP with Limit
req = LimitOrderRequest(
    symbol="SPY",
    qty=5000,
    limit_price=623.80,
    side=OrderSide.BUY,
    type=OrderType.LIMIT,
    advanced_instructions={
        "algorithm": "TWAP",
        "start_time": "2025-07-21T09:30:00-04:00",
        "end_time": "2025-07-21T14:30:00-04:00",
        "max_percentage": "0.123"
    }
)
```

#### Important Constraints
- **Elite Smart Router only** — error `40310000` "account not allowed to use advanced_instructions" if not subscribed
- Times must be ISO 8601 format
- Upcoming: PVOL and MPEG algorithm types
- Works with both market and limit orders

---

## 3. Actionable Ideas for Our Scanner/Trading System

### Strategies We Could Implement in Our Rust `mlai-trade` Binary

#### A. Bollinger Bands Mean Reversion Scanner Signal
- Add `BB_SQUEEZE` signal: detect when bands narrow (low volatility → breakout coming)
- Add `BB_OVERSOLD` signal: close < lower band (+ optional RSI < 30 filter)
- Add `BB_OVERBOUGHT` signal: close > upper band (+ optional RSI > 70 filter)
- **Implementation:** 20-period rolling mean/std on close prices

#### B. MACD Crossover Signal
- Add `MACD_BULL_CROSS`: MACD line crosses above signal line
- Add `MACD_BEAR_CROSS`: MACD line crosses below signal line
- Add `MACD_ZERO_CROSS`: MACD line crosses zero line
- **Implementation:** EMA(12) - EMA(26) vs EMA(9) of MACD
- **Data requirement:** 120 days of bars minimum for stable computation

#### C. Composite Scoring (from Multi-Strategy Dashboard article)
- Instead of single-signal triggers, weight multiple indicators:
  - RSI score + MACD score + Bollinger score + Volume score = composite entry score
  - Regime detection: is market trending or mean-reverting?
  - Dynamic thresholds adjusted by recent volatility

#### D. Enhanced Order Types for the Trading Module
When we merge into the Rust `mlai-trade` binary, add support for:
1. **Limit orders** (`mlai-trade buy AAPL --limit 150.00`)
2. **Stop orders** (`mlai-trade sell AAPL --stop 145.00`)
3. **Stop-limit orders** (`mlai-trade sell AAPL --stop 145 --limit 144.50`)
4. **Trailing stop** (`mlai-trade sell AAPL --trail-percent 5` or `--trail-price 3.00`)
5. **Bracket orders** (`mlai-trade buy AAPL --qty 10 --take-profit 160 --stop-loss 140`)
6. **Notional orders** (`mlai-trade buy AAPL --notional 500` for $500 worth)
7. **GTC time-in-force** (`mlai-trade buy AAPL --limit 150 --tif gtc`)
8. **Extended hours** (`mlai-trade buy AAPL --limit 150 --extended-hours`)

#### E. Backtesting Framework
- Build a simple backtesting command: `mlai-trade backtest --strategy bollinger --symbol AAPL --days 365`
- Use the vectorized pattern: signal → forward-fill position → shift(1) → strategy_return
- Calculate Sharpe, max drawdown, total return
- Compare against buy-and-hold benchmark

#### F. Position Safety Checks
Before any sell order:
```
1. Get current position qty
2. Get open sell orders for same symbol
3. available_qty = position_qty - sum(open_sell_order_qtys)
4. Only allow sell if requested_qty <= available_qty
```

### Dashboard Enhancements

#### Add to Trading Dashboard Space:
1. **Bollinger Bands visualization** on symbol detail chart
2. **MACD histogram** below price chart
3. **Composite signal score** column in watchlist
4. **Backtesting panel** — run strategy simulations from the UI
5. **Order type selector** — limit, stop, stop-limit, trailing stop

---

## 4. API Tips and Gotchas

### Authentication
- **Headers:** `APCA-API-KEY-ID` and `APCA-API-SECRET-KEY`
- Paper vs Live use DIFFERENT API key pairs
- Paper endpoint: `https://paper-api.alpaca.markets`
- Live endpoint: `https://api.alpaca.markets`
- Data endpoint: `https://data.alpaca.markets`

### Rate Limits
- "Generous enough" for backtesting (per multi-strategy article)
- For heavy data queries, narrow time windows and paginate
- 504 timeouts possible on wide-range activity queries

### Market Data Feeds (Our Setup)
- **Historical bars:** SIP feed works on free tier ✓
- **Real-time snapshots:** Use default/no feed param (NOT SIP) on free tier
- **IEX feed:** Real-time but only IEX exchange data
- **Overnight data (8PM-4AM ET):** Use `feed=overnight` on free tier

### Order Types Reference
| Type | Required Params | TIF Support |
|------|----------------|-------------|
| market | symbol, qty/notional, side | day, gtc, opg, cls, ioc, fok |
| limit | + limit_price | day, gtc, opg, cls, ioc, fok |
| stop | + stop_price | day, gtc |
| stop_limit | + stop_price, limit_price | day, gtc |
| trailing_stop | + trail_price OR trail_percent | day, gtc |

### Time-in-Force by Asset Class
| TIF | Equities | Options | Crypto |
|-----|----------|---------|--------|
| day | ✓ | ✓ | ✗ |
| gtc | ✓ | ✓ (limit only) | ✓ |
| opg | ✓ | ✗ | ✗ |
| cls | ✓ | ✗ | ✗ |
| ioc | ✓ | ✗ | ✓ |
| fok | ✓ | ✗ | ✗ |

### Fractional Trading Rules
- Only DAY market orders
- Minimum $1.00 notional
- Cannot short fractional shares
- Not all assets support it (check `fractionable` attribute)
- `notional` and `qty` are mutually exclusive

### Extended Hours / 24/5 Trading
- Only limit orders (DAY or GTC)
- Set `extended_hours=True` on the order
- Overnight data available 8PM-4AM ET via BOATS/Overnight feeds

### GTC Options Specifics
- Limit orders only (no market)
- Auto-cancel after 90 days (modify resets timer)
- No fills during extended hours (options: 9:30 AM - 4:00 PM ET only)
- Reverse splits cancel all GTC orders
- Use `replace_order_by_id()` to modify without cancelling

### Error Handling Best Practices
1. Always check order status before resending after timeout
2. Use `client_order_id` for idempotency
3. Wait for cancel confirmation before closing positions
4. Check `buying_power` before placing orders
5. Check `fractionable` before fractional/notional orders
6. Validate position qty before sell orders (account for held shares)

---

## 5. Trading Strategies Catalog

### Strategy 1: Bollinger Bands Mean Reversion
**Type:** Mean reversion | **Timeframe:** Daily | **Indicators:** BB(20,2), optionally RSI(14)

**Entry (Long):**
- Close price crosses below lower Bollinger Band
- (Optional) RSI < 30

**Exit:**
- Close price crosses above upper Bollinger Band
- (Optional) RSI > 70

**Pseudocode:**
```
for each bar:
    bb_upper = SMA(close, 20) + 2 * STDDEV(close, 20)
    bb_lower = SMA(close, 20) - 2 * STDDEV(close, 20)
    rsi = RSI(close, 14)

    if close < bb_lower AND rsi < 30 AND not in_position:
        BUY (market, DAY)
        in_position = true

    if close > bb_upper AND rsi > 70 AND in_position:
        SELL (market, DAY)
        in_position = false
```

---

### Strategy 2: MACD Signal Line Crossover
**Type:** Trend following | **Timeframe:** Daily | **Indicators:** MACD(12,26,9)

**Entry (Long):**
- MACD line crosses above signal line (bullish crossover)
- (Optional confirmation) MACD line > 0 (above centerline)

**Exit:**
- MACD line crosses below signal line (bearish crossover)

**Pseudocode:**
```
for each bar:
    ema12 = EMA(close, 12)
    ema26 = EMA(close, 26)
    macd = ema12 - ema26
    signal = EMA(macd, 9)

    prev_diff = prev_macd - prev_signal
    curr_diff = macd - signal

    if prev_diff <= 0 AND curr_diff > 0:  # bullish crossover
        BUY
    if prev_diff >= 0 AND curr_diff < 0:  # bearish crossover
        SELL
```

---

### Strategy 3: 0DTE Bull Put Spread (Options)
**Type:** Options premium collection | **Timeframe:** Intraday (1-min) | **Indicators:** Delta

**Entry:**
- Find put option pair on SPY expiring today
- Short put: delta between -0.60 and -0.20
- Long put: delta between -0.40 and -0.20
- Strike spread: $2-$4
- Net credit received

**Exit:**
- Delta stop loss: short put delta > 2× initial delta
- Profit target: 50% of initial credit
- Expiration (all remaining value is profit if OTM)

**Requirements:**
- Options approval
- Intraday options data (Databento OPRA)
- Margin account

---

### Strategy 4: Composite Multi-Signal Scoring
**Type:** Hybrid | **Timeframe:** Daily | **Indicators:** RSI, MACD, BB, Volume, SMA

**Entry:**
- Calculate composite score from weighted signals:
  - RSI oversold (< 30): +2 points
  - Price below SMA(50): +1 point
  - MACD bullish crossover: +2 points
  - Price below lower BB: +1 point
  - Volume spike (> 2× average): +1 point
- Enter when score ≥ threshold (e.g., 4)

**Exit:**
- Trailing stop (configurable %, e.g., 5%)
- OR composite score drops below exit threshold
- OR take profit at target %

**Pseudocode:**
```
for each bar:
    score = 0
    if RSI(14) < 30: score += 2
    if close < SMA(50): score += 1
    if MACD_bullish_crossover: score += 2
    if close < BB_lower(20,2): score += 1
    if volume > 2 * AVG_VOL(20): score += 1

    if score >= 4 AND not in_position:
        BUY with trailing_stop at 5%

    if in_position AND (score <= 1 OR trailing_stop_hit):
        SELL
```

---

### Strategy 5: Covered Call (via TradingView or API)
**Type:** Income/hedging | **Timeframe:** Weekly/Monthly | **Indicators:** None specific

**Setup:**
- Own 100 shares of underlying
- Sell 1 OTM call (strike above current price)
- Expiration: 2-4 weeks out

**Profit:**
- Premium collected + any stock appreciation up to strike
- Max profit: premium + (strike - entry price) × 100

**Risk:**
- Stock drops significantly (put protection needed = collar)
- Stock rises above strike (capped upside)

---

## 6. API Endpoint Quick Reference

### REST Endpoints Used
| Action | Method | Endpoint |
|--------|--------|----------|
| Get Account | GET | `/v2/account` |
| Submit Order | POST | `/v2/orders` |
| Get Orders | GET | `/v2/orders` |
| Cancel Order | DELETE | `/v2/orders/{id}` |
| Cancel All | DELETE | `/v2/orders` |
| Get Positions | GET | `/v2/positions` |
| Close Position | DELETE | `/v2/positions/{symbol}` |
| Get Asset | GET | `/v2/assets/{symbol}` |
| Replace Order | PATCH | `/v2/orders/{id}` |
| Stock Bars | GET | `/v2/stocks/{symbol}/bars` |
| Stock Latest Quote | GET | `/v2/stocks/{symbol}/quotes/latest` |
| Option Contracts | GET | `/v2/options/contracts` |
| Option Latest Quote | GET | `/v2/options/quotes/latest` |
| Market Clock | GET | `/v2/clock` in older SDK examples; `mlai-trade` uses Alpaca v3 `/clock` for market-aware provider clocks |
| Market Calendar | GET | `/v2/calendar` in older SDK examples; `mlai-trade` uses Alpaca v3 `/calendar/{market}` with configured timezone |

### Python SDK Classes
```python
from alpaca.trading.client import TradingClient
from alpaca.trading.requests import (
    MarketOrderRequest, LimitOrderRequest, StopOrderRequest,
    StopLimitOrderRequest, TrailingStopOrderRequest,
    GetOrdersRequest, GetOptionContractsRequest
)
from alpaca.trading.enums import (
    OrderSide, OrderType, TimeInForce, OrderClass,
    QueryOrderStatus, AssetStatus, ContractType
)
from alpaca.data.historical.stock import StockHistoricalDataClient
from alpaca.data.requests import (
    StockBarsRequest, StockLatestBarRequest,
    StockLatestQuoteRequest, OptionLatestQuoteRequest
)
from alpaca.data.timeframe import TimeFrame
from alpaca.data.enums import DataFeed, OptionsFeed
```

---

---

## 7. INDEPENDENT VERIFICATION & ACADEMIC EVIDENCE

> ⚠️ **This section provides independent academic verification of every strategy discussed in the Alpaca blog posts.**
> The blog posts are educational marketing content. This section is the reality check.

---

### 7.1 Strategy Verdicts Summary

| Strategy | Verdict | Academic Consensus |
|----------|---------|-------------------|
| Bollinger Bands Mean Reversion | ❌ DEBUNKED | Underperforms buy-and-hold across all settings after costs |
| MACD Crossover | ❌ DEBUNKED | 3% success rate on daily charts; 26% on 5-min (606K trades tested) |
| RSI Overbought/Oversold | ⚠️ MIXED | Works better as filter combined with other signals; poor standalone |
| 0DTE Bull Put Spread | ⚠️ MIXED | Harvests variance risk premium but extreme gamma risk; tail losses devastating |
| Covered Call | ❌ DEBUNKED | "Devil's Bargain" — generates yield but reduces total returns vs buy-and-hold |
| VWAP Execution | ✅ VERIFIED | Legitimate execution algorithm for minimizing market impact (not alpha generation) |
| TWAP Execution | ✅ VERIFIED | Legitimate execution algorithm for predictable pacing (not alpha generation) |
| Momentum (Jegadeesh-Titman) | ✅ VERIFIED | One of the most robust anomalies in finance; 30+ years of evidence |
| Moving Average Crossover | ⚠️ MIXED | Some evidence in emerging/commodity markets; profits erode after transaction costs in developed markets |
| Composite Multi-Signal | ⚠️ MIXED | Theoretically sound but extreme overfitting risk; no academic consensus |

---

### 7.2 Detailed Academic Evidence by Strategy

#### Bollinger Bands Mean Reversion — ❌ DEBUNKED

**Key Study: CXO Advisory (Apr 1993–Nov 2019, SPY)**
- Tested BB settings from 0.5 to 2.5 standard deviations around 21-day SMA
- **Best BB strategy CAGR: 8.6% vs Buy-and-Hold CAGR: 10.3%**
- BB strategies NEVER beat buy-and-hold across ANY setting
- Max drawdowns were similar to buy-and-hold (no meaningful risk reduction)
- Sharpe ratios consistently below buy-and-hold

**Key Study: SSRN #2484322 "Popularity versus Profitability: Evidence from Bollinger Bands"**
- Finds that Bollinger Bands' popularity does not translate to profitability
- After transaction costs, returns diminish further

**Key Study: Sainbuyan (2024, College of Wooster Thesis)**
- Tested Moving Average Crossover, Bollinger Bands, and BB+RSI on AAPL, MSFT, META
- In-sample: possible to find parameter sets that outperform SPY benchmark
- **Out-of-sample: BB+RSI strategy failed to execute any trades at all**
- MA and BB maintained some performance on MSFT/META but **failed to beat benchmark on AAPL**
- Conclusion: "emphasizes the trade-offs between optimizing historical performance and ensuring future robustness"

**Key Study: Leeds (2012, arXiv:1212.4890) "Bollinger Bands Thirty Years Later"**
- Provides rigorous statistical foundations for Bollinger Bands
- Shows BB is essentially a rolling regression model
- Useful for pairs trading (mean reversion of spread), NOT for directional trading on single assets
- Developed "Fixed Forecast Maximum Duration Bands" variant that statistically outperforms standard BB in pairs trading

**Verdict:** Bollinger Bands have solid statistical foundations but the simple mean-reversion strategy (buy below lower band, sell above upper) **does not beat buy-and-hold** after any reasonable transaction costs. The indicator is more useful as a volatility measure or in pairs trading contexts.

---

#### MACD Crossover — ❌ DEBUNKED

**Key Study: Liberated Stock Trader (606,422 tested trades)**
- **Daily chart success rate: 3%**
- **5-minute chart success rate: 26%**
- MACD produces many small losses during consolidation periods
- "MACD is a poor-performing chart indicator"
- Visually appealing but not profitable
- Slightly better performance on Heikin Ashi charts

**Key Study: MDPI Journal of Risk & Financial Management (2026, Gold Market)**
- "Optimal and Non-Optimal MACD Parameter Ranges with Stop-Loss and Take-Profit Rules"
- Optimized MACD parameters CAN generate profits in gold futures
- BUT: profits are highly parameter-dependent (overfitting risk)
- Standard (12,26,9) parameters are often non-optimal

**Key Study: Nikkei 225 Futures (ResearchGate, Chien et al.)**
- "Improving MACD Technical Analysis by Optimizing Parameters and Modifying Trading Rules"
- Modified MACD rules can improve performance
- Standard crossover rules are unprofitable after transaction costs
- Optimized rules show improvement but sample is limited

**Key Study: SSRN #5186655 "Moving Average Crossover vs. Buy-and-Hold: Evidence from Major Tech Stocks"**
- MA crossover strategies generally underperform buy-and-hold for tech stocks
- Some evidence of downside protection during crashes, but net returns lower

**Verdict:** Standard MACD (12,26,9) crossover is essentially useless as a standalone strategy. The 3% daily success rate is damning. It's a **lagging indicator that confirms what already happened.** Optimized parameters can improve results, but that's classic overfitting. MACD's best use is as a **confirmation filter** combined with other signals, never as primary entry/exit.

---

#### RSI Overbought/Oversold — ⚠️ MIXED

**Key Study: ResearchGate (Indian Market)**
- RSI generates accurate buy/sell signals in the Indian market
- Best results when combined with moving averages
- Standalone RSI less reliable than RSI + MA combination

**Key Study: Various backtests**
- RSI < 30 (oversold) signals tend to identify genuine mean-reversion opportunities
- RSI > 70 (overbought) signals are less reliable — strong trends can stay overbought for extended periods
- Works better in range-bound markets, fails in strong trends

**Verdict:** RSI has some value as a **filter** (especially the oversold signal) but is unreliable as a standalone strategy. Best used in combination with other indicators. The overbought signal is particularly unreliable because strong uptrends routinely maintain RSI > 70.

---

#### 0DTE Bull Put Spread — ⚠️ MIXED (HIGH RISK)

**Key Study: CBOE "Much Ado About 0DTEs" (Mandy Xu)**
- 0DTE SPX options went from 5% of volume (2016) to 50%+ (2023)
- Market maker flow is "remarkably balanced" (buy vs sell), limiting systemic risk
- Net gamma exposure from 0DTEs is small relative to total market gamma
- **0DTE gamma is NOT causing market instability** (contrary to media fears)

**Key Study: SSRN #4692190 "0DTEs: Trading, Gamma Risk and Volatility Propagation"**
- 60-page academic study on gamma risk from 0DTE options
- Extreme gamma sensitivity means small price moves create large delta adjustments
- Tail risk events can cause catastrophic losses on short premium positions
- The variance risk premium (VRP) exists and can be harvested, BUT drawdowns are severe

**Key Study: SSRN #5641974 "Do S&P500 Options Increase Market Volatility? Evidence from 0DTEs"**
- 74-page study (Oct 2025) examining whether 0DTEs increase market volatility
- Finds limited evidence of systematic volatility amplification

**Key Study: Northern Trust "Navigating 0DTE Options"**
- Theta decay is extremely rapid (100% of time value lost in one day)
- Gamma exposure is enormous — small moves in underlying = massive option price changes
- Suitable only for experienced traders with strict risk management

**Verdict:** 0DTE strategies harvest the variance risk premium (options tend to be overpriced relative to realized volatility). This is a **real economic effect**. However, the strategy has severe tail risk — a single bad day can wipe out months of premium collection. The blog post's backtesting is limited (10 days) and doesn't capture tail events. Academic evidence says VRP harvesting works over long periods but requires **robust risk management** and **large sample sizes** to be statistically meaningful.

---

#### Covered Call — ❌ DEBUNKED (for return enhancement)

**Key Study: Israelov & Ndong (2023, SSRN) "A Devil's Bargain"**
- Analyzed covered call performance on S&P 500 (Jan 1999–Jun 2023)
- **Selling call options on SPX, on average, LOST money**
- Higher yield = greater losses: "high-yield covered call underperformed low-yield, and materially so"
- 6% yield target → -0.60% annual P&L from option selling
- 12% yield target → -1.08% annual P&L from option selling
- Recent period (2011-2023) even worse: 6% target lost -3.1%/yr, 12% target lost -4.7%/yr

**Key Findings:**
- Premium is NOT income — it's compensation for shorting volatility
- Higher yield = lower market beta (less equity risk premium capture)
- Creates **negative skewness** (eliminates upside potential, keeps downside)
- "Popularity of yield-enhancing strategies led to overcrowding, shrinking premiums"
- Tax-inefficient: premium income taxed at higher rates

**Key Study: Purdue PhD Thesis "Covered Call: Is it a Conservative Winning Strategy?"**
- Title is rhetorical — findings suggest covered calls don't reliably beat buy-and-hold

**Verdict:** Covered calls generate **yield, not alpha**. They sacrifice upside for premium income and systematically underperform in bull markets. The academic evidence is clear: covered call strategies **reduce total returns** compared to simply holding the underlying. They may have a role in portfolio management (income generation, volatility reduction) but should **never** be marketed as return enhancers.

---

#### VWAP Execution — ✅ VERIFIED (as execution tool)

**Key Research: Extensive academic literature**
- VWAP is a legitimate execution benchmark, NOT a trading strategy
- arXiv:2503.02680 — "VWAP Execution with Signature-Enhanced Transformers" shows ML approaches achieving 35.96% improvement in tracking
- arXiv:2212.14670 — "Hierarchical Deep Reinforcement Learning for VWAP Strategy Optimization"
- SSRN #3380177 — "Optimal VWAP Execution Under Transient Price Impact"
- 74% of hedge funds use VWAP (TRADE 2025 Algorithmic Trading Survey)

**Important Distinction:** VWAP/TWAP are **execution algorithms**, not **alpha-generating strategies**. They help you execute large orders at fair prices with minimal market impact. They don't tell you WHAT to buy — they help you buy/sell it efficiently.

**Verdict:** VWAP is well-validated for its intended purpose: benchmark-tracking execution of large orders. It's a tool, not a strategy. The Alpaca blog post correctly presents it this way. Note: requires Elite Smart Router subscription ($$$).

---

#### TWAP Execution — ✅ VERIFIED (as execution tool)

Same category as VWAP. TWAP is simpler (equal time slicing vs. volume-weighted). More useful when volume patterns are unpredictable or in low-liquidity environments. 42% of hedge funds use it.

**Verdict:** Legitimate execution tool. Not alpha-generating.

---

#### Momentum Trading (Jegadeesh-Titman) — ✅ VERIFIED

**Key Study: Jegadeesh & Titman (1993) — Seminal paper**
- Stocks that performed well (poorly) over 3-12 months continue to perform well (poorly) over the next 3-12 months
- One of the most robust anomalies in financial markets
- Replicated across dozens of markets, time periods, and asset classes

**Key Study: "Momentum: what do we know 30 years after Jegadeesh and Titman's seminal paper?" (2022, Financial Markets and Portfolio Management)**
- Comprehensive 30-year review of momentum research
- Momentum remains profitable but has experienced crashes (2009, etc.)
- Risk factors partially explain returns but significant alpha remains
- Works across equities, currencies, commodities, bonds

**Key Study: Jegadeesh & Titman (2001, SSRN #166840)**
- "Profitability of Momentum Strategies: An Evaluation of Alternative Explanations"
- Momentum profits are NOT explained by risk factors
- Behavioral biases (under-reaction to news) are a more likely explanation

**Verdict:** Momentum is one of the few **academically verified** trading anomalies. It has strong evidence spanning 30+ years across multiple asset classes. However, it experiences **momentum crashes** (sudden reversals) that can be devastating. The blog posts don't discuss momentum directly, but our scanner's BIG_MOVE and NEW_HIGH signals capture some of this effect.

---

#### Composite/Multi-Signal Strategies — ⚠️ MIXED

**No single academic study validates "composite scoring" in general.** The concept is sound (multiple weak signals combined = stronger signal), but:
- Extreme overfitting risk when choosing weights and thresholds
- In-sample optimization almost always overstates out-of-sample performance
- SSRN #2308659 "Pseudo-Mathematics and Financial Charlatanism: The Effects of Backtest Overfitting on Out-of-Sample Performance" — shows that with enough parameters, ANY random strategy can appear profitable in backtests

**Verdict:** Theoretically sound but practically dangerous. Requires rigorous out-of-sample testing, walk-forward validation, and minimal parameter tuning. Most retail implementations will overfit.

---

### 7.3 The Meta-Study: What Does Academia Say About Technical Analysis Overall?

#### Park & Irwin (2007) "What Do We Know About the Profitability of Technical Analysis?"
*Journal of Economic Surveys, Vol. 21, No. 4, pp. 786-826*

This is the **definitive meta-study** on technical analysis profitability. Key findings:

1. **Survey evidence:** 30-40% of practitioners believe TA is important for price movements at horizons up to 6 months
2. **Early studies (pre-1990s):** TA was profitable in forex and futures, but NOT in stock markets
3. **Modern studies (1990s):** TA consistently generated profits across speculative markets until the early 1990s
4. **Critical caveat:** Most studies suffer from:
   - Data snooping bias (testing many rules, reporting winners)
   - Survivorship bias (studying strategies that "worked" historically)
   - Transaction cost underestimation
   - Look-ahead bias in parameter selection
5. **Key conclusion:** "Whether or not technical trading profits have existed in the past, the empirical evidence clearly suggests that technical trading profits have been declining over time and have disappeared in more recent periods."

**Translation:** Technical analysis may have worked in less efficient markets and earlier time periods. As markets became more efficient and transaction costs dropped (enabling more participants), the edge eroded. **The alpha has been arbitraged away.**

#### Fama (1970, 1991) — Efficient Market Hypothesis
- Weak-form efficiency: past prices contain no predictive information
- If true, ALL technical analysis is useless by definition
- Modern view: markets are "mostly efficient" with occasional small, transient inefficiencies

#### Arxiv:1302.1228 "Efficient Markets, Behavioral Finance and Statistical Evidence of Technical Analysis Validity"
- Attempts to bridge EMH and behavioral finance
- Finds some statistical evidence for TA validity in specific conditions
- Behavioral biases (herding, overreaction, anchoring) create temporary mispricings
- These mispricings may be exploitable but are **small, transient, and competed away quickly**

---

### 7.4 Critical Analysis: What the Blog Posts Don't Tell You

#### A. Survivorship Bias
The blog posts only show strategies that "work" in their specific backtests. They don't show:
- The hundreds of parameter combinations that failed
- The time periods where the same strategy lost money
- The out-of-sample degradation that almost always occurs

#### B. Transaction Costs
Most blog post backtests assume zero or minimal transaction costs. In reality:
- Bid-ask spread on options is typically 5-15% of premium (0DTE can be worse)
- Even Alpaca's "commission-free" trades have hidden costs (payment for order flow, spread)
- Slippage on larger orders can be significant (especially for the strategies targeting volume spikes)
- **After realistic costs, most of these strategies become unprofitable**

#### C. Slippage and Market Impact
- Blog posts assume you can execute at the exact price shown in historical data
- In reality, your order MOVES the price (especially on less liquid stocks)
- Our scanner targets volume spikes and movers — exactly the situations where slippage is highest
- VWAP/TWAP exist specifically to address this problem

#### D. Overfitting in Backtesting
- The multi-strategy dashboard article brags about "parameter sliders that instantly re-simulate"
- This is literally a tool for overfitting
- The article does mention overfitting briefly, but then provides sliders to do exactly that
- Academic evidence (SSRN #2308659): with enough parameters, ANY random strategy looks profitable in-sample
- **Rule of thumb:** the more parameters your strategy has, the more likely it's overfit

#### E. Data Period Cherry-Picking
- The 0DTE backtest uses only 10 trading days — statistically meaningless
- Bollinger Bands and MACD tests often use specific time periods where they happened to work
- A proper backtest needs: multiple market regimes (bull, bear, sideways), at least 5-10 years of data, out-of-sample validation

#### F. The Variance Risk Premium (VRP) — The One Real Edge
The only strategy category with genuine academic support for generating alpha is **selling options premium** (which includes the bull put spread, covered calls, etc.). The VRP exists because:
- Options buyers are willing to pay a premium for insurance (portfolio protection)
- This creates a systematic overpricing of options
- Premium sellers can harvest this over time

**BUT:** The VRP comes with massive tail risk. Selling premium is like "picking up nickels in front of a steamroller." Expected value is positive, but the distribution is severely negatively skewed. Occasional massive losses can wipe out years of premium collection. This is exactly what academic papers warn about.

#### G. What Actually Works (Academic Consensus)
1. **Momentum** — Buy recent winners, sell recent losers (3-12 month horizon). Well-documented anomaly.
2. **Value** — Buy cheap assets (low P/E, P/B). Long-term evidence, but has underperformed since 2010.
3. **Size** — Small caps tend to outperform (but with higher risk). Evidence weakening.
4. **Low volatility** — Lower-risk stocks tend to deliver higher risk-adjusted returns. Robust evidence.
5. **Quality** — Companies with strong balance sheets, high ROE. Growing evidence.
6. **Diversification** — The only "free lunch" in finance. Reduces risk without reducing expected return.

These are **factor-based** strategies with decades of academic support. They are fundamentally different from the technical indicator strategies in the blog posts.

---

### 7.5 Recommendations for Our System Based on Evidence

#### What to BUILD (evidence-backed):
1. **Momentum scanner signals** — Our existing BIG_MOVE, NEW_HIGH, NEW_LOW signals partially capture this. Add 3-month and 6-month momentum rankings.
2. **Factor screening** — Add value (P/E, P/B), quality (ROE, debt/equity), and momentum factors to the scanner. These have academic support.
3. **VWAP/TWAP execution** — If we ever trade large sizes, these are the right tools. But requires Elite subscription.
4. **Volatility-based position sizing** — Use Bollinger Bands width as a volatility measure (not as entry/exit signals). Size positions inversely to volatility.
5. **Risk management framework** — Maximum position size, portfolio-level stop loss, sector diversification.

#### What to use with CAUTION:
1. **RSI as a filter** — Only for confirming other signals, not standalone. RSI < 30 + another signal = potentially useful.
2. **MACD as confirmation** — Use histogram to confirm momentum direction, never as primary signal.
3. **Options premium selling** — VRP exists, but needs strict risk management. Never risk more than 2% of portfolio on any single spread.

#### What to AVOID:
1. **Bollinger Bands mean reversion as entry/exit** — Data clearly shows it underperforms buy-and-hold.
2. **MACD crossover as primary signal** — 3% success rate speaks for itself.
3. **Covered calls for "income"** — Sacrifices returns for yield. Just hold the stock.
4. **Optimizing strategy parameters to historical data** — Overfitting is the #1 killer of trading strategies.
5. **Short backtesting periods** — 10 days (like the 0DTE article) is statistically meaningless.

---

### 7.6 Academic Papers Reference List

| Paper | Authors | Year | Key Finding |
|-------|---------|------|-------------|
| "What Do We Know About the Profitability of Technical Analysis?" | Park & Irwin | 2007 | TA profits have declined over time and may have disappeared |
| "Popularity versus Profitability: Evidence from Bollinger Bands" | SSRN #2484322 | 2014 | BB popularity ≠ profitability |
| "Bollinger Bands Thirty Years Later" | Leeds | 2012 | BB has statistical foundations but best for pairs trading |
| "A Devil's Bargain: When Generating Income Undermines Investment Returns" | Israelov & Ndong | 2023 | Covered calls systematically reduce returns |
| "0DTEs: Trading, Gamma Risk and Volatility Propagation" | SSRN #4692190 | 2024 | Extreme gamma risk in 0DTE, needs strict risk management |
| "Pseudo-Mathematics and Financial Charlatanism" | Bailey, Borwein et al. | 2014 | Backtest overfitting makes any random strategy look profitable |
| "Returns to Buying Winners and Selling Losers" | Jegadeesh & Titman | 1993 | Momentum is a real, robust anomaly |
| "Momentum: 30 years after Jegadeesh and Titman" | Various | 2022 | Momentum remains valid but experiences crashes |
| "Much Ado About 0DTEs" | Xu (CBOE) | 2023 | 0DTE gamma risk is balanced; not systemic |
| "MACD Indicator: 606,422 Tested Trades" | Liberated Stock Trader | 2025 | 3% daily success rate, 26% on 5-min charts |
| "Optimal VWAP Execution Under Transient Price Impact" | SSRN #3380177 | 2019 | VWAP execution algorithms work for intended purpose |
| CXO Advisory Bollinger Bands Backtest | LeCompte | 2019 | BB best CAGR 8.6% vs B&H 10.3% — never beats market |

---

*End of knowledge base. Update as new learnings are acquired.*
