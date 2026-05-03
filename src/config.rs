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
    #[serde(default)]
    pub resources: ResourcesConfig,
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
    #[serde(default)]
    pub accounts: Vec<AlpacaAccountConfig>,
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
    pub daily_refresh_trigger: Option<String>,
    pub daily_refresh_after_close_minutes: Option<i64>,
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
    pub trigger: String,
    pub after_close_minutes: i64,
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
    pub max_concurrent_requests: Option<usize>,
    pub max_concurrent_long_requests: Option<usize>,
    pub rate_limit_per_minute: Option<usize>,
    pub max_body_bytes: Option<usize>,
    pub overload_retry_after_seconds: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ApiLimitConfig {
    pub max_concurrent_requests: usize,
    pub max_concurrent_long_requests: usize,
    pub rate_limit_per_minute: usize,
    pub max_body_bytes: usize,
    pub overload_retry_after_seconds: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ResourcesConfig {
    pub sqlite_cache_mb: Option<i64>,
    pub sqlite_temp_store: Option<String>,
    pub sqlite_mmap_mb: Option<i64>,
    pub ml_symbol_batch_size: Option<usize>,
    pub lstm_max_sequences: Option<usize>,
    pub lstm_batch_size: Option<usize>,
    pub lightgbm_max_train_rows: Option<usize>,
    pub lightgbm_max_valid_rows: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct RuntimeResources {
    pub sqlite_cache_mb: i64,
    pub sqlite_temp_store: String,
    pub sqlite_mmap_mb: i64,
    pub ml_symbol_batch_size: usize,
    pub lstm_max_sequences: usize,
    pub lstm_batch_size: usize,
    pub lightgbm_max_train_rows: usize,
    pub lightgbm_max_valid_rows: usize,
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
        trigger: daemon
            .daily_refresh_trigger
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "market_close".to_string())
            .to_ascii_lowercase(),
        after_close_minutes: daemon
            .daily_refresh_after_close_minutes
            .unwrap_or(60)
            .clamp(0, 360),
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

pub fn api_limit_config() -> ApiLimitConfig {
    let api = load().ok().map(|config| config.api).unwrap_or_default();
    ApiLimitConfig {
        max_concurrent_requests: api.max_concurrent_requests.unwrap_or(8).clamp(1, 128),
        max_concurrent_long_requests: api.max_concurrent_long_requests.unwrap_or(1).clamp(1, 16),
        rate_limit_per_minute: api.rate_limit_per_minute.unwrap_or(120).clamp(1, 10_000),
        max_body_bytes: api.max_body_bytes.unwrap_or(65_536).clamp(1024, 1_048_576),
        overload_retry_after_seconds: api.overload_retry_after_seconds.unwrap_or(5).clamp(1, 300),
    }
}

pub fn runtime_resources() -> RuntimeResources {
    let resources = load()
        .ok()
        .map(|config| config.resources)
        .unwrap_or_default();
    let sqlite_temp_store = match resources
        .sqlite_temp_store
        .unwrap_or_else(|| "file".to_string())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "memory" => "MEMORY".to_string(),
        _ => "FILE".to_string(),
    };
    RuntimeResources {
        sqlite_cache_mb: resources.sqlite_cache_mb.unwrap_or(32).clamp(4, 512),
        sqlite_temp_store,
        sqlite_mmap_mb: resources.sqlite_mmap_mb.unwrap_or(0).clamp(0, 4096),
        ml_symbol_batch_size: resources
            .ml_symbol_batch_size
            .unwrap_or(250)
            .clamp(25, 2000),
        lstm_max_sequences: resources
            .lstm_max_sequences
            .unwrap_or(50_000)
            .clamp(1_000, 500_000),
        lstm_batch_size: resources.lstm_batch_size.unwrap_or(64).clamp(8, 1024),
        lightgbm_max_train_rows: resources
            .lightgbm_max_train_rows
            .unwrap_or(2_000_000)
            .min(20_000_000),
        lightgbm_max_valid_rows: resources
            .lightgbm_max_valid_rows
            .unwrap_or(250_000)
            .min(5_000_000),
    }
}

pub fn sqlite_runtime_pragma_sql() -> String {
    let resources = runtime_resources();
    let cache_kib = resources.sqlite_cache_mb * 1024;
    let mmap_bytes = resources.sqlite_mmap_mb * 1_048_576;
    format!(
        "PRAGMA cache_size=-{cache_kib}; PRAGMA temp_store={}; PRAGMA mmap_size={mmap_bytes};",
        resources.sqlite_temp_store
    )
}

pub fn ml_symbol_batch_size() -> usize {
    runtime_resources().ml_symbol_batch_size
}

pub fn lstm_max_sequences() -> usize {
    runtime_resources().lstm_max_sequences
}

pub fn lstm_batch_size() -> usize {
    runtime_resources().lstm_batch_size
}

pub fn lightgbm_max_train_rows() -> usize {
    runtime_resources().lightgbm_max_train_rows
}

pub fn lightgbm_max_valid_rows() -> usize {
    runtime_resources().lightgbm_max_valid_rows
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
    use super::{normalize_symbol, AppConfig};

    fn has_path(value: &serde_json::Value, path: &[&str]) -> bool {
        path.iter()
            .try_fold(value, |current, key| {
                current.get(*key).or_else(|| {
                    key.parse::<usize>()
                        .ok()
                        .and_then(|index| current.get(index))
                })
            })
            .is_some()
    }

    #[test]
    fn example_config_parses_and_documents_every_supported_key() {
        let raw = include_str!("../config/mlai-trade.example.json");
        let value: serde_json::Value = serde_json::from_str(raw).expect("valid example JSON");
        let _: AppConfig = serde_json::from_value(value.clone()).expect("example config parses");
        for path in [
            &["providers", "alpaca", "enabled"][..],
            &["providers", "other"],
            &["alpaca", "accounts", "0", "name"],
            &["alpaca", "accounts", "0", "enabled"],
            &["alpaca", "accounts", "0", "account_mode"],
            &["alpaca", "accounts", "0", "data_feed"],
            &["alpaca", "accounts", "0", "api_key_id"],
            &["alpaca", "accounts", "0", "secret_key"],
            &["fred", "api_key"],
            &["tax", "filing_status"],
            &["tax", "estimated_annual_income"],
            &["tax", "include_paper_accounts_for_estimate"],
            &["tax", "brackets_file"],
            &["daemon", "enabled"],
            &["daemon", "auto_trade_interval_seconds"],
            &["daemon", "daily_refresh_enabled"],
            &["daemon", "daily_refresh_trigger"],
            &["daemon", "daily_refresh_after_close_minutes"],
            &["daemon", "daily_refresh_time"],
            &["daemon", "daily_refresh_timezone"],
            &["daemon", "daily_refresh_days"],
            &["daemon", "daily_refresh_quick"],
            &["daemon", "daily_refresh_walk_forward_folds"],
            &["daemon", "daily_refresh_top_n"],
            &["daemon", "daily_refresh_slippage_bps"],
            &["daemon", "daily_refresh_sync_orders"],
            &["daemon", "daily_refresh_feeds_sync"],
            &["daemon", "daily_refresh_feeds_days"],
            &["daemon", "pid_file"],
            &["daemon", "log_file"],
            &["api", "enabled"],
            &["api", "socket_file"],
            &["api", "pid_file"],
            &["api", "log_file"],
            &["api", "request_timeout_seconds"],
            &["api", "long_request_timeout_seconds"],
            &["api", "max_concurrent_requests"],
            &["api", "max_concurrent_long_requests"],
            &["api", "rate_limit_per_minute"],
            &["api", "max_body_bytes"],
            &["api", "overload_retry_after_seconds"],
            &["logging", "data_log_file"],
            &["logging", "ml_log_file"],
            &["logging", "training_log_file"],
            &["logging", "feeds_log_file"],
            &["feeds", "sync_before_training"],
            &["feeds", "sync_orders_before_training"],
            &["feeds", "include_current_sp500"],
            &["feeds", "include_open_positions"],
            &["feeds", "include_bought_symbols"],
            &["feeds", "bought_symbol_lookback_days"],
            &["feeds", "include_q1_candidates"],
            &["feeds", "q1_top_n"],
            &["feeds", "sync_days"],
            &["feeds", "extra_symbols"],
            &["scan", "max_concurrent"],
            &["scan", "max_retries"],
            &["backend", "lstm"],
            &["backend", "xgboost"],
            &["backend", "lightgbm"],
            &["backend", "ridge"],
            &["resources", "sqlite_cache_mb"],
            &["resources", "sqlite_temp_store"],
            &["resources", "sqlite_mmap_mb"],
            &["resources", "ml_symbol_batch_size"],
            &["resources", "lstm_max_sequences"],
            &["resources", "lstm_batch_size"],
            &["resources", "lightgbm_max_train_rows"],
            &["resources", "lightgbm_max_valid_rows"],
            &["auto", "enabled"],
            &["auto", "log_file"],
            &["auto", "market", "mode"],
            &["auto", "market", "require_local_clock"],
            &["auto", "market", "use_provider_clock"],
            &["auto", "market", "use_provider_calendar"],
            &["auto", "market", "allow_local_clock_fallback"],
            &["auto", "market", "timezone"],
            &["auto", "market", "provider_markets"],
            &["auto", "market", "regular_open"],
            &["auto", "market", "regular_close"],
            &["auto", "market", "buy_start"],
            &["auto", "market", "buy_end"],
            &["auto", "market", "sell_start"],
            &["auto", "market", "sell_end"],
            &["auto", "market", "closed_dates"],
            &["auto", "compliance", "blocked_symbols"],
            &["auto", "compliance", "wash_sale_safety_buffer_days"],
            &["auto", "max_positions"],
            &["auto", "position_size_pct"],
            &["auto", "stop_loss_pct"],
            &["auto", "take_profit_pct"],
            &["auto", "max_hold_days"],
            &["auto", "min_price"],
            &["auto", "min_avg_volume"],
            &["auto", "max_spread_bps"],
            &["auto", "min_quote_size"],
            &["auto", "allow_bar_price_fallback"],
            &["auto", "bar_fallback_bps"],
            &["auto", "ml_quintile_buy"],
            &["auto", "ml_quintile_exit"],
        ] {
            assert!(
                has_path(&value, path),
                "config/mlai-trade.example.json missing {}",
                path.join(".")
            );
        }
    }

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

pub fn config_path() -> PathBuf {
    paths::config_dir().join("mlai-trade.json")
}

pub fn load() -> anyhow::Result<AppConfig> {
    let path = config_path();
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let _ = paths::harden_file_if_exists(&path);
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

pub fn alpaca_data_feed() -> String {
    if let Ok(account) = alpaca_primary_account() {
        return account.data_feed;
    }
    "auto".to_string()
}

fn alpaca_provider_enabled(config: &AppConfig) -> bool {
    config.providers.alpaca.enabled
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
    account: &AlpacaAccountConfig,
    default_name: String,
) -> anyhow::Result<AlpacaAccount> {
    let name = account
        .name
        .clone()
        .and_then(|name| non_empty(Some(name)))
        .unwrap_or(default_name);
    let api_key_id = non_empty(account.api_key_id.clone());
    let secret_key = non_empty(account.secret_key.clone());
    let account_mode = normalize_account_mode(non_empty(account.account_mode.clone()))
        .unwrap_or_else(|| "paper".to_string());
    let data_feed = normalize_data_feed(non_empty(account.data_feed.clone()))
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
        anyhow::bail!(
            "Alpaca provider is enabled, but no Alpaca accounts are configured in {}. Add at least one alpaca.accounts[] entry with api_key_id and secret_key.",
            config_path().display()
        );
    }

    let mut accounts = Vec::new();
    for (idx, account) in config.alpaca.accounts.iter().enumerate() {
        if !account.enabled.unwrap_or(true) {
            continue;
        }
        accounts.push(resolve_alpaca_account(
            account,
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
        .and_then(|config| non_empty(config.tax.brackets_file));
    paths::path_in_runtime_dir(paths::config_dir(), value, "tax-brackets.json")
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
        .and_then(|config| non_empty(config.backend.xgboost))
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
    let value = load()
        .ok()
        .and_then(|config| non_empty(config.auto.log_file));
    paths::path_in_runtime_dir(paths::logs_dir(), value, "mlai-trade-auto.log")
}

fn secret_candidate(value: Option<String>) -> Option<String> {
    non_empty(value).filter(|value| value.len() >= 8 && value != "replace_me")
}

pub fn configured_secret_values() -> Vec<String> {
    let Ok(config) = load() else {
        return Vec::new();
    };
    let mut secrets = Vec::new();
    if let Some(value) = secret_candidate(config.fred.api_key) {
        secrets.push(value);
    }
    for account in config.alpaca.accounts {
        if let Some(value) = secret_candidate(account.api_key_id) {
            secrets.push(value);
        }
        if let Some(value) = secret_candidate(account.secret_key) {
            secrets.push(value);
        }
    }
    secrets.sort_by_key(|value| std::cmp::Reverse(value.len()));
    secrets.dedup();
    secrets
}

pub fn redact_configured_secrets(text: &str) -> String {
    let mut redacted = text.to_string();
    for secret in configured_secret_values() {
        redacted = redacted.replace(&secret, "[REDACTED]");
    }
    redacted
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
