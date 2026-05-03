// Alpaca provider integration.
//
// Provider-specific endpoints, feed selection, and account-mode behavior live
// here so shared ML, storage, and execution code can stay broker-neutral.

use crate::config;
use serde::{Deserialize, Serialize};

pub const DATA_URL: &str = "https://data.alpaca.markets";

// Official Alpaca Trading API endpoints:
// https://docs.alpaca.markets/docs/getting-started
// https://docs.alpaca.markets/reference/api-references
const PAPER_BROKER_API_URL: &str = "https://paper-api.alpaca.markets/v2";
const PAPER_TRADING_API_BASE_URL: &str = "https://paper-api.alpaca.markets";
const INDIVIDUAL_BROKER_API_URL: &str = "https://api.alpaca.markets";
const INDIVIDUAL_TRADING_API_BASE_URL: &str = "https://api.alpaca.markets";

pub const FULL_HISTORY_PROBE_START: &str = "1900-01-01";

pub fn is_paper() -> bool {
    config::alpaca_primary_account()
        .map(|account| account.is_paper())
        .unwrap_or(true)
}

pub fn account_mode_for(account: &config::AlpacaAccount) -> &'static str {
    if account.is_paper() {
        "paper"
    } else {
        "individual"
    }
}

pub fn broker_api_url(path: &str) -> String {
    let account_mode = config::alpaca_primary_account()
        .map(|account| account.account_mode)
        .unwrap_or_else(|_| "paper".to_string());
    broker_api_url_for_mode(&account_mode, path)
}

pub fn broker_api_url_for(account: &config::AlpacaAccount, path: &str) -> String {
    broker_api_url_for_mode(&account.account_mode, path)
}

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

fn trading_api_base_url_for_mode(account_mode: &str) -> &'static str {
    if matches!(account_mode, "individual" | "live") {
        INDIVIDUAL_TRADING_API_BASE_URL
    } else {
        PAPER_TRADING_API_BASE_URL
    }
}

pub fn trading_api_url_for(account: &config::AlpacaAccount, version: &str, path: &str) -> String {
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    format!(
        "{}/{version}{path}",
        trading_api_base_url_for_mode(&account.account_mode)
    )
}

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
    pub fn market_label(&self) -> String {
        self.market
            .as_ref()
            .and_then(|market| market.acronym.as_ref().or(market.mic.as_ref()))
            .cloned()
            .unwrap_or_else(|| "unknown".to_string())
    }

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

pub fn data_feeds() -> Vec<String> {
    data_feeds_for_mode(&data_feed_mode())
}

pub fn data_feeds_for(account: &config::AlpacaAccount) -> Vec<String> {
    data_feeds_for_mode(&account.data_feed)
}

pub fn data_feeds_for_mode(data_feed: &str) -> Vec<String> {
    match data_feed {
        "sip" => vec!["sip".to_string()],
        "iex" => vec!["iex".to_string()],
        _ => vec!["sip".to_string(), "iex".to_string()],
    }
}

pub fn stock_quote_url(symbol: &str, feed: &str) -> String {
    format!(
        "{}/v2/stocks/{}/quotes/latest?feed={}",
        DATA_URL, symbol, feed
    )
}

pub fn stock_snapshot_url(symbol: &str, feed: &str) -> String {
    format!("{}/v2/stocks/{}/snapshot?feed={}", DATA_URL, symbol, feed)
}
