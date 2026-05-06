// Alpaca provider integration.
//
// Provider-specific endpoints, feed selection, and account-mode behavior live
// here so shared ML, storage, and execution code can stay broker-neutral.
//
// Function map:
// - *_url_for(): build Alpaca REST URLs for account/feed-specific calls.
// - data_feeds_for*(): select SIP/IEX fallback order from config.
// - TradingClock/Calendar structs: deserialize provider market-session data.

use crate::config;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct Position {
    pub symbol: String,
    pub qty: String,
    pub avg_entry_price: Option<String>,
    pub current_price: Option<String>,
    pub market_value: Option<String>,
    pub unrealized_pl: Option<String>,
    pub unrealized_plpc: Option<String>,
    pub asset_class: Option<String>,
    pub exchange: Option<String>,
    pub side: Option<String>,
}

pub const DATA_URL: &str = "https://data.alpaca.markets";

// Official Alpaca Trading API endpoints:
// https://docs.alpaca.markets/docs/getting-started
// https://docs.alpaca.markets/reference/api-references
const PAPER_BROKER_API_URL: &str = "https://paper-api.alpaca.markets/v2";
const PAPER_TRADING_API_BASE_URL: &str = "https://paper-api.alpaca.markets";
const INDIVIDUAL_BROKER_API_URL: &str = "https://api.alpaca.markets";
const INDIVIDUAL_TRADING_API_BASE_URL: &str = "https://api.alpaca.markets";

pub const FULL_HISTORY_PROBE_START: &str = "1900-01-01";

// Returns whether paper is true.
pub fn is_paper() -> bool {
    config::alpaca_primary_account()
        .map(|account| account.is_paper())
        .unwrap_or(true)
}

// Handles account mode for matching or metadata.
pub fn account_mode_for(account: &config::AlpacaAccount) -> &'static str {
    if account.is_paper() {
        "paper"
    } else {
        "individual"
    }
}

// Handles broker api url logic.
pub fn broker_api_url(path: &str) -> String {
    config::alpaca_primary_account()
        .map(|account| broker_api_url_for(&account, path))
        .unwrap_or_else(|_| broker_api_url_for_mode("paper", path))
}

// Handles broker api url for logic.
pub fn broker_api_url_for(account: &config::AlpacaAccount, path: &str) -> String {
    if let Some(base_url) = account.trading_base_url.as_deref() {
        return broker_api_url_for_base(base_url, path);
    }
    broker_api_url_for_mode(&account.account_mode, path)
}

// Handles broker api url for an explicit test or override base URL.
fn broker_api_url_for_base(base_url: &str, path: &str) -> String {
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    format!("{}/v2{}", base_url.trim_end_matches('/'), path)
}

// Handles broker api url for mode logic.
pub fn broker_api_url_for_mode(account_mode: &str, path: &str) -> String {
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };

    if matches!(account_mode, "individual" | "live") {
        format!("{INDIVIDUAL_BROKER_API_URL}/v2{path}")
    } else {
        format!("{PAPER_BROKER_API_URL}{path}")
    }
}

// Handles trading api base url for mode calculations.
fn trading_api_base_url_for_mode(account_mode: &str) -> &'static str {
    if matches!(account_mode, "individual" | "live") {
        INDIVIDUAL_TRADING_API_BASE_URL
    } else {
        PAPER_TRADING_API_BASE_URL
    }
}

// Handles trading api url for calculations.
pub fn trading_api_url_for(account: &config::AlpacaAccount, version: &str, path: &str) -> String {
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    if let Some(base_url) = account.trading_base_url.as_deref() {
        return format!("{}/{version}{path}", base_url.trim_end_matches('/'));
    }
    format!(
        "{}/{version}{path}",
        trading_api_base_url_for_mode(&account.account_mode)
    )
}

// Handles clock v3 url for logic.
pub fn clock_v3_url_for(account: &config::AlpacaAccount, markets: &[String]) -> String {
    let markets = if markets.is_empty() {
        "NYSE,NASDAQ".to_string()
    } else {
        markets.join(",")
    };
    let mut url = reqwest::Url::parse(&trading_api_url_for(account, "v3", "/clock"))
        .expect("valid Alpaca clock URL");
    url.query_pairs_mut().append_pair("markets", &markets);
    url.to_string()
}

// Handles calendar v3 url for logic.
pub fn calendar_v3_url_for(
    account: &config::AlpacaAccount,
    market: &str,
    start: &str,
    end: &str,
    timezone: &str,
) -> String {
    let mut url = reqwest::Url::parse(&trading_api_url_for(
        account,
        "v3",
        &format!("/calendar/{market}"),
    ))
    .expect("valid Alpaca calendar URL");
    url.query_pairs_mut()
        .append_pair("start", start)
        .append_pair("end", end)
        .append_pair("timezone", timezone);
    url.to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MarketInfo {
    pub acronym: Option<String>,
    pub mic: Option<String>,
    pub name: Option<String>,
    pub timezone: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TradingClockResponse {
    #[serde(default)]
    pub clocks: Vec<TradingClock>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TradingClock {
    pub is_market_day: Option<bool>,
    pub market: Option<MarketInfo>,
    pub next_market_close: Option<String>,
    pub next_market_open: Option<String>,
    pub phase: Option<String>,
    pub phase_until: Option<String>,
    pub timestamp: Option<String>,
}

impl TradingClock {
    // Handles market label logic.
    pub fn market_label(&self) -> String {
        self.market
            .as_ref()
            .and_then(|market| market.acronym.as_ref().or(market.mic.as_ref()))
            .cloned()
            .unwrap_or_else(|| "unknown".to_string())
    }

    // Returns whether be trading is true.
    pub fn may_be_trading(&self) -> bool {
        if !self.is_market_day.unwrap_or(false) {
            return false;
        }
        !matches!(
            self.phase
                .as_deref()
                .unwrap_or("")
                .to_ascii_lowercase()
                .as_str(),
            "" | "closed"
        )
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TradingCalendarResponse {
    #[serde(default)]
    pub calendar: Vec<TradingCalendarDay>,
    pub market: Option<MarketInfo>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TradingCalendarDay {
    pub date: String,
    pub core_start: Option<String>,
    pub core_end: Option<String>,
    pub pre_start: Option<String>,
    pub pre_end: Option<String>,
    pub post_start: Option<String>,
    pub post_end: Option<String>,
    pub settlement_date: Option<String>,
}

// Returns Alpaca data-feed feed mode information.
pub fn data_feed_mode() -> String {
    let requested = config::alpaca_data_feed();
    match requested.as_str() {
        "auto" | "sip" | "iex" => requested,
        other => {
            eprintln!(
                "warning: unsupported alpaca.accounts[].data_feed={}; using auto (SIP then IEX fallback).",
                other
            );
            "auto".to_string()
        }
    }
}

// Returns Alpaca data-feed feeds information.
pub fn data_feeds() -> Vec<String> {
    data_feeds_for_mode(&data_feed_mode())
}

// Returns Alpaca data-feed feeds for information.
pub fn data_feeds_for(account: &config::AlpacaAccount) -> Vec<String> {
    data_feeds_for_mode(&account.data_feed)
}

// Returns Alpaca data-feed feeds for mode information.
pub fn data_feeds_for_mode(data_feed: &str) -> Vec<String> {
    match data_feed {
        "sip" => vec!["sip".to_string()],
        "iex" => vec!["iex".to_string()],
        _ => vec!["sip".to_string(), "iex".to_string()],
    }
}

// Returns the Alpaca market data base URL for the primary configured account.
pub fn data_base_url() -> String {
    config::alpaca_primary_account()
        .ok()
        .and_then(|account| account.data_base_url)
        .unwrap_or_else(|| DATA_URL.to_string())
}

// Handles stock quote url logic.
pub fn stock_quote_url(symbol: &str, feed: &str) -> String {
    format!(
        "{}/v2/stocks/{}/quotes/latest?feed={}",
        data_base_url(),
        symbol,
        feed
    )
}

// Handles stock snapshot url logic.
pub fn stock_snapshot_url(symbol: &str, feed: &str) -> String {
    format!(
        "{}/v2/stocks/{}/snapshot?feed={}",
        data_base_url(),
        symbol,
        feed
    )
}
