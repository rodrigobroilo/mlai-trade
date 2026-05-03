use crate::paths;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub providers: ProvidersConfig,
    #[serde(default)]
    pub alpaca: AlpacaConfig,
    #[serde(default)]
    pub fred: FredConfig,
    #[serde(default)]
    pub scan: ScanConfig,
    #[serde(default)]
    pub auto: AutoConfig,
    #[serde(default)]
    pub tax: TaxConfig,
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub api: ApiConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub feeds: FeedsConfig,
    #[serde(default)]
    pub backend: BackendConfig,
    #[cfg(feature = "xgboost-baseline")]
    #[serde(default)]
    pub xgboost: XgboostConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProvidersConfig {
    #[serde(default = "default_alpaca_switch")]
    pub alpaca: ProviderSwitch,
    #[serde(default)]
    pub other: BTreeMap<String, ProviderSwitch>,
}

fn default_alpaca_switch() -> ProviderSwitch {
    ProviderSwitch { enabled: true }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderSwitch {
    pub enabled: bool,
}

impl Default for ProviderSwitch {
    fn default() -> Self {
        Self { enabled: false }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AlpacaConfig {
    pub enabled: Option<bool>,
    #[serde(default)]
    pub accounts: Vec<AlpacaAccountConfig>,
    pub api_key_id: Option<String>,
    pub secret_key: Option<String>,
    pub account_mode: Option<String>,
    pub data_feed: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AlpacaAccountConfig {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub api_key_id: Option<String>,
    pub secret_key: Option<String>,
    pub account_mode: Option<String>,
    pub data_feed: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AlpacaAccount {
    pub name: String,
    pub api_key_id: String,
    pub secret_key: String,
    pub account_mode: String,
    pub data_feed: String,
}

impl AlpacaAccount {
    pub fn provider(&self) -> &'static str {
        "alpaca"
    }

    pub fn account_ref(&self) -> &str {
        &self.name
    }

    pub fn is_paper(&self) -> bool {
        !matches!(self.account_mode.as_str(), "individual" | "live")
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FredConfig {
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ScanConfig {
    pub max_concurrent: Option<usize>,
    pub max_retries: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaxConfig {
    pub filing_status: Option<String>,
    pub estimated_annual_income: Option<f64>,
    pub include_paper_accounts_for_estimate: Option<bool>,
    pub brackets_file: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DaemonConfig {
    pub enabled: Option<bool>,
    pub auto_trade_interval_seconds: Option<u64>,
    pub daily_refresh_enabled: Option<bool>,
    pub daily_refresh_time: Option<String>,
    pub daily_refresh_timezone: Option<String>,
    pub daily_refresh_days: Option<u32>,
    pub daily_refresh_quick: Option<bool>,
    pub daily_refresh_walk_forward_folds: Option<usize>,
    pub daily_refresh_top_n: Option<usize>,
    pub daily_refresh_slippage_bps: Option<f64>,
    pub daily_refresh_sync_orders: Option<bool>,
    pub daily_refresh_feeds_sync: Option<bool>,
    pub daily_refresh_feeds_days: Option<u32>,
    pub pid_file: Option<String>,
    pub log_file: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DaemonDailyRefreshConfig {
    pub enabled: bool,
    pub time: String,
    pub timezone: String,
    pub days: u32,
    pub quick: bool,
    pub walk_forward_folds: usize,
    pub top_n: usize,
    pub slippage_bps: f64,
    pub sync_orders: bool,
    pub feeds_sync: bool,
    pub feeds_days: u32,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApiConfig {
    pub enabled: Option<bool>,
    pub socket_file: Option<String>,
    pub pid_file: Option<String>,
    pub log_file: Option<String>,
    pub request_timeout_seconds: Option<u64>,
    pub long_request_timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LoggingConfig {
    pub data_log_file: Option<String>,
    pub ml_log_file: Option<String>,
    pub training_log_file: Option<String>,
    pub feeds_log_file: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FeedsConfig {
    pub sync_before_training: Option<bool>,
    pub sync_orders_before_training: Option<bool>,
    pub include_current_sp500: Option<bool>,
    pub include_open_positions: Option<bool>,
    pub include_bought_symbols: Option<bool>,
    pub bought_symbol_lookback_days: Option<u32>,
    pub include_q1_candidates: Option<bool>,
    pub q1_top_n: Option<usize>,
    pub sync_days: Option<u32>,
    pub extra_symbols: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct FeedsMlSyncConfig {
    pub sync_before_training: bool,
    pub sync_orders_before_training: bool,
    pub include_current_sp500: bool,
    pub include_open_positions: bool,
    pub include_bought_symbols: bool,
    pub bought_symbol_lookback_days: u32,
    pub include_q1_candidates: bool,
    pub q1_top_n: usize,
    pub sync_days: u32,
    pub extra_symbols: Vec<String>,
}

pub fn daemon_enabled() -> bool {
    load()
        .ok()
        .and_then(|config| config.daemon.enabled)
        .unwrap_or(false)
}

pub fn daemon_auto_trade_interval_seconds() -> u64 {
    load()
        .ok()
        .and_then(|config| config.daemon.auto_trade_interval_seconds)
        .unwrap_or(60)
        .clamp(10, 300)
}

pub fn daemon_daily_refresh_config() -> DaemonDailyRefreshConfig {
    let daemon = load().ok().map(|config| config.daemon).unwrap_or_default();
    DaemonDailyRefreshConfig {
        enabled: daemon.daily_refresh_enabled.unwrap_or(true),
        time: daemon
            .daily_refresh_time
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "18:30:00".to_string()),
        timezone: daemon
            .daily_refresh_timezone
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "America/New_York".to_string()),
        days: daemon.daily_refresh_days.unwrap_or(0),
        quick: daemon.daily_refresh_quick.unwrap_or(false),
        walk_forward_folds: daemon.daily_refresh_walk_forward_folds.unwrap_or(5).max(1),
        top_n: daemon.daily_refresh_top_n.unwrap_or(20).max(1),
        slippage_bps: daemon.daily_refresh_slippage_bps.unwrap_or(50.0).max(0.0),
        sync_orders: daemon.daily_refresh_sync_orders.unwrap_or(true),
        feeds_sync: daemon.daily_refresh_feeds_sync.unwrap_or(true),
        feeds_days: daemon.daily_refresh_feeds_days.unwrap_or(7).max(1),
    }
}

pub fn api_enabled() -> bool {
    load()
        .ok()
        .and_then(|config| config.api.enabled)
        .unwrap_or(false)
}

pub fn api_request_timeout_seconds() -> u64 {
    load()
        .ok()
        .and_then(|config| config.api.request_timeout_seconds)
        .unwrap_or(60)
        .clamp(5, 300)
}

pub fn api_long_request_timeout_seconds() -> u64 {
    load()
        .ok()
        .and_then(|config| config.api.long_request_timeout_seconds)
        .unwrap_or(3600)
        .clamp(60, 86_400)
}

pub fn feeds_ml_sync_config() -> FeedsMlSyncConfig {
    let feeds = load().ok().map(|config| config.feeds).unwrap_or_default();
    FeedsMlSyncConfig {
        sync_before_training: feeds.sync_before_training.unwrap_or(true),
        sync_orders_before_training: feeds.sync_orders_before_training.unwrap_or(true),
        include_current_sp500: feeds.include_current_sp500.unwrap_or(true),
        include_open_positions: feeds.include_open_positions.unwrap_or(true),
        include_bought_symbols: feeds.include_bought_symbols.unwrap_or(true),
        bought_symbol_lookback_days: feeds.bought_symbol_lookback_days.unwrap_or(365).max(1),
        include_q1_candidates: feeds.include_q1_candidates.unwrap_or(true),
        q1_top_n: feeds.q1_top_n.unwrap_or(500).max(1),
        sync_days: feeds.sync_days.unwrap_or(30).max(1),
        extra_symbols: feeds.extra_symbols.unwrap_or_default(),
    }
}

fn normalize_symbol(value: &str) -> Option<String> {
    let symbol = value.trim().to_ascii_uppercase();
    if symbol.is_empty() {
        None
    } else {
        Some(symbol)
    }
}

pub fn blocked_symbols() -> Vec<String> {
    load()
        .ok()
        .map(|config| config.auto.compliance.blocked_symbols)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|symbol| normalize_symbol(&symbol))
        .collect()
}

pub fn is_blocked_symbol(symbol: &str) -> bool {
    let Some(symbol) = normalize_symbol(symbol) else {
        return false;
    };
    blocked_symbols().iter().any(|blocked| blocked == &symbol)
}

pub fn blocked_symbols_sql_predicate(symbol_expr: &str) -> String {
    let blocked = blocked_symbols();
    if blocked.is_empty() {
        return "1=1".to_string();
    }
    let values = blocked
        .iter()
        .map(|symbol| format!("'{}'", symbol.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");
    format!("UPPER({symbol_expr}) NOT IN ({values})")
}

#[cfg(test)]
mod tests {
    use super::normalize_symbol;

    #[test]
    fn normalize_symbol_uppercases_and_trims_market_symbols() {
        assert_eq!(normalize_symbol("meta").as_deref(), Some("META"));
        assert_eq!(normalize_symbol(" Meta ").as_deref(), Some("META"));
        assert_eq!(normalize_symbol("brk.b").as_deref(), Some("BRK.B"));
        assert_eq!(normalize_symbol("").as_deref(), None);
        assert_eq!(normalize_symbol("   ").as_deref(), None);
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AutoConfig {
    pub enabled: Option<bool>,
    pub log_file: Option<String>,
    #[serde(default)]
    pub market: AutoMarketConfig,
    #[serde(default)]
    pub compliance: AutoComplianceConfig,
    pub max_positions: Option<i64>,
    pub position_size_pct: Option<f64>,
    pub stop_loss_pct: Option<f64>,
    pub take_profit_pct: Option<f64>,
    pub max_hold_days: Option<i64>,
    pub min_price: Option<f64>,
    pub min_avg_volume: Option<i64>,
    pub max_spread_bps: Option<f64>,
    pub min_quote_size: Option<f64>,
    pub allow_bar_price_fallback: Option<bool>,
    pub bar_fallback_bps: Option<f64>,
    pub ml_quintile_buy: Option<i64>,
    pub ml_quintile_exit: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AutoComplianceConfig {
    pub wash_sale_safety_buffer_days: Option<i64>,
    #[serde(default)]
    pub blocked_symbols: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AutoMarketConfig {
    pub mode: Option<String>,
    pub require_local_clock: Option<bool>,
    pub use_provider_clock: Option<bool>,
    pub use_provider_calendar: Option<bool>,
    pub allow_local_clock_fallback: Option<bool>,
    pub timezone: Option<String>,
    #[serde(default)]
    pub provider_markets: Vec<String>,
    pub regular_open: Option<String>,
    pub regular_close: Option<String>,
    pub buy_start: Option<String>,
    pub buy_end: Option<String>,
    pub sell_start: Option<String>,
    pub sell_end: Option<String>,
    #[serde(default)]
    pub closed_dates: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct BackendConfig {
    pub lstm: Option<String>,
    pub xgboost: Option<String>,
    pub lightgbm: Option<String>,
    pub ridge: Option<String>,
}

#[cfg(feature = "xgboost-baseline")]
#[derive(Debug, Clone, Default, Deserialize)]
pub struct XgboostConfig {
    pub backend: Option<String>,
}

pub fn config_path() -> PathBuf {
    paths::config_dir().join("mlai-trade.json")
}

pub fn load() -> anyhow::Result<AppConfig> {
    let path = config_path();
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let content = std::fs::read_to_string(&path)?;
    let config = serde_json::from_str::<AppConfig>(&content)
        .map_err(|err| anyhow::anyhow!("invalid config file {}: {}", path.display(), err))?;
    Ok(config)
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_string();
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    })
}

pub fn provider_enabled(provider: &str) -> bool {
    let Ok(config) = load() else {
        return provider == "alpaca";
    };
    match provider {
        "alpaca" => alpaca_provider_enabled(&config),
        other => config
            .providers
            .other
            .get(other)
            .map(|provider| provider.enabled)
            .unwrap_or(false),
    }
}

pub fn enabled_providers() -> Vec<String> {
    let Ok(config) = load() else {
        return vec!["alpaca".to_string()];
    };
    let mut providers = Vec::new();
    if alpaca_provider_enabled(&config) {
        providers.push("alpaca".to_string());
    }
    providers.extend(
        config
            .providers
            .other
            .iter()
            .filter(|(_, provider)| provider.enabled)
            .map(|(name, _)| name.clone()),
    );
    providers
}

pub fn require_enabled_provider() -> anyhow::Result<Vec<String>> {
    let providers = enabled_providers();
    if providers.is_empty() {
        anyhow::bail!(
            "No trading provider is enabled. Enable at least one provider in {}, for example providers.alpaca.enabled=true.",
            config_path().display()
        );
    }
    Ok(providers)
}

pub fn alpaca_account_mode() -> String {
    if let Ok(account) = alpaca_primary_account() {
        return account.account_mode;
    }
    load()
        .ok()
        .and_then(|config| normalize_account_mode(non_empty(config.alpaca.account_mode)))
        .unwrap_or_else(|| "paper".to_string())
}

pub fn alpaca_data_feed() -> String {
    if let Ok(account) = alpaca_primary_account() {
        return account.data_feed;
    }
    load()
        .ok()
        .and_then(|config| normalize_data_feed(non_empty(config.alpaca.data_feed)))
        .unwrap_or_else(|| "auto".to_string())
}

fn alpaca_provider_enabled(config: &AppConfig) -> bool {
    config
        .alpaca
        .enabled
        .unwrap_or(config.providers.alpaca.enabled)
}

fn normalize_account_mode(value: Option<String>) -> Option<String> {
    value.map(|value| match value.to_ascii_lowercase().as_str() {
        "individual" | "live" => "individual".to_string(),
        "paper" => "paper".to_string(),
        other => other.to_string(),
    })
}

fn normalize_data_feed(value: Option<String>) -> Option<String> {
    value.map(|value| match value.to_ascii_lowercase().as_str() {
        "sip" => "sip".to_string(),
        "iex" => "iex".to_string(),
        "auto" => "auto".to_string(),
        other => other.to_string(),
    })
}

fn resolve_alpaca_account(
    config: &AlpacaConfig,
    account: Option<&AlpacaAccountConfig>,
    default_name: String,
) -> anyhow::Result<AlpacaAccount> {
    let name = account
        .and_then(|account| non_empty(account.name.clone()))
        .unwrap_or(default_name);
    let api_key_id = account
        .and_then(|account| non_empty(account.api_key_id.clone()))
        .or_else(|| non_empty(config.api_key_id.clone()));
    let secret_key = account
        .and_then(|account| non_empty(account.secret_key.clone()))
        .or_else(|| non_empty(config.secret_key.clone()));
    let account_mode = account
        .and_then(|account| normalize_account_mode(non_empty(account.account_mode.clone())))
        .or_else(|| normalize_account_mode(non_empty(config.account_mode.clone())))
        .unwrap_or_else(|| "paper".to_string());
    let data_feed = account
        .and_then(|account| normalize_data_feed(non_empty(account.data_feed.clone())))
        .or_else(|| normalize_data_feed(non_empty(config.data_feed.clone())))
        .unwrap_or_else(|| "auto".to_string());

    match (api_key_id, secret_key) {
        (Some(api_key_id), Some(secret_key)) => Ok(AlpacaAccount {
            name,
            api_key_id,
            secret_key,
            account_mode,
            data_feed,
        }),
        (None, Some(_)) => anyhow::bail!(
            "Alpaca key ID not set for account '{}'. Add alpaca.accounts[].api_key_id to {}.",
            name,
            config_path().display()
        ),
        (Some(_), None) => anyhow::bail!(
            "Alpaca secret key not set for account '{}'. Add alpaca.accounts[].secret_key to {}.",
            name,
            config_path().display()
        ),
        (None, None) => anyhow::bail!(
            "Alpaca credentials not set for account '{}'. Add alpaca.accounts[].api_key_id and alpaca.accounts[].secret_key to {}.",
            name,
            config_path().display()
        ),
    }
}

pub fn alpaca_accounts() -> anyhow::Result<Vec<AlpacaAccount>> {
    let config = load()?;
    if !alpaca_provider_enabled(&config) {
        anyhow::bail!(
            "Alpaca provider is disabled in {}. Set providers.alpaca.enabled=true to use Alpaca.",
            config_path().display()
        );
    }

    if config.alpaca.accounts.is_empty() {
        return Ok(vec![resolve_alpaca_account(
            &config.alpaca,
            None,
            "default".to_string(),
        )?]);
    }

    let mut accounts = Vec::new();
    for (idx, account) in config.alpaca.accounts.iter().enumerate() {
        if !account.enabled.unwrap_or(true) {
            continue;
        }
        accounts.push(resolve_alpaca_account(
            &config.alpaca,
            Some(account),
            format!("account-{}", idx + 1),
        )?);
    }
    if accounts.is_empty() {
        anyhow::bail!(
            "Alpaca provider is enabled, but no Alpaca accounts are enabled in {}.",
            config_path().display()
        );
    }
    Ok(accounts)
}

pub fn alpaca_primary_account() -> anyhow::Result<AlpacaAccount> {
    alpaca_accounts()?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("No enabled Alpaca accounts configured."))
}

pub fn fred_api_key() -> anyhow::Result<String> {
    load()?
        .fred
        .api_key
        .and_then(|value| non_empty(Some(value)))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "FRED API key not set. Add fred.api_key to {}.",
                config_path().display()
            )
        })
}

pub fn tax_brackets_path() -> PathBuf {
    let value = load()
        .ok()
        .and_then(|config| non_empty(config.tax.brackets_file))
        .unwrap_or_else(|| "tax-brackets.json".to_string());
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        paths::config_dir().join(path)
    }
}

pub fn scan_max_concurrent(default: usize) -> usize {
    load()
        .ok()
        .and_then(|config| config.scan.max_concurrent)
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

pub fn scan_max_retries(default: usize) -> usize {
    load()
        .ok()
        .and_then(|config| config.scan.max_retries)
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[cfg(feature = "xgboost-baseline")]
pub fn xgboost_backend() -> String {
    load()
        .ok()
        .and_then(|config| {
            non_empty(config.backend.xgboost).or_else(|| non_empty(config.xgboost.backend))
        })
        .unwrap_or_else(|| "auto".to_string())
        .to_ascii_lowercase()
}

#[cfg(not(feature = "xgboost-baseline"))]
pub fn xgboost_backend() -> String {
    load()
        .ok()
        .and_then(|config| non_empty(config.backend.xgboost))
        .unwrap_or_else(|| "auto".to_string())
        .to_ascii_lowercase()
}

pub fn lstm_backend() -> String {
    load()
        .ok()
        .and_then(|config| non_empty(config.backend.lstm))
        .unwrap_or_else(|| "auto".to_string())
        .to_ascii_lowercase()
}

pub fn lightgbm_backend() -> String {
    load()
        .ok()
        .and_then(|config| non_empty(config.backend.lightgbm))
        .unwrap_or_else(|| "cpu".to_string())
        .to_ascii_lowercase()
}

pub fn ridge_backend() -> String {
    load()
        .ok()
        .and_then(|config| non_empty(config.backend.ridge))
        .unwrap_or_else(|| "cpu".to_string())
        .to_ascii_lowercase()
}

pub fn auto_log_file() -> PathBuf {
    load()
        .ok()
        .and_then(|config| non_empty(config.auto.log_file))
        .map(PathBuf::from)
        .unwrap_or_else(|| paths::logs_dir().join("mlai-trade-auto.log"))
}

pub fn auto_market_provider_markets() -> Vec<String> {
    let mut markets = load()
        .ok()
        .map(|config| config.auto.market.provider_markets)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|market| {
            let market = market.trim().to_string();
            if market.is_empty() {
                None
            } else {
                Some(market)
            }
        })
        .collect::<Vec<_>>();
    if markets.is_empty() {
        markets = vec!["NYSE".to_string(), "NASDAQ".to_string()];
    }
    markets
}
