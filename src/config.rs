// Runtime configuration loader and normalizer.
//
// Function map:
// - load(): reads ~/mlai-trade/config/mlai-trade.json or built-in defaults.
// - runtime_resources(): auto-sizes memory/ML caps from detected system RAM.
// - alpaca_accounts(): resolves provider/account credentials and modes.
// - *_backend(), *_enabled(), *_path(): expose normalized config to modules.
// - redact_configured_secrets(): prevents configured keys leaking into output.
// - sanitize_logged_command_output(): strips terminal control codes from logs.

use crate::paths;
use chrono::NaiveTime;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
#[cfg(target_os = "linux")]
use std::fs;
use std::net::IpAddr;
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
pub struct MlTuningConfig {
    #[serde(default)]
    pub lstm: LstmConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProvidersConfig {
    #[serde(default = "default_alpaca_switch")]
    pub alpaca: ProviderSwitch,
    #[serde(default)]
    pub other: BTreeMap<String, ProviderSwitch>,
}

// Handles default alpaca switch logic.
fn default_alpaca_switch() -> ProviderSwitch {
    ProviderSwitch { enabled: true }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderSwitch {
    pub enabled: bool,
}

impl Default for ProviderSwitch {
    // Provides the default value for this configuration type.
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
    pub auto_trade_enabled: Option<bool>,
    pub api_key_id: Option<String>,
    pub secret_key: Option<String>,
    pub account_mode: Option<String>,
    pub data_feed: Option<String>,
    pub trading_base_url: Option<String>,
    pub data_base_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AlpacaAccount {
    pub name: String,
    pub auto_trade_enabled: bool,
    pub api_key_id: String,
    pub secret_key: String,
    pub account_mode: String,
    pub data_feed: String,
    pub trading_base_url: Option<String>,
    pub data_base_url: Option<String>,
}

impl AlpacaAccount {
    // Handles provider logic.
    pub fn provider(&self) -> &'static str {
        "alpaca"
    }

    // Handles account ref matching or metadata.
    pub fn account_ref(&self) -> &str {
        &self.name
    }

    // Returns whether paper is true.
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
    pub dashboard_bar_cache_enabled: Option<bool>,
    pub dashboard_bar_cache_interval_seconds: Option<u64>,
    pub dashboard_bar_cache_symbols_limit: Option<usize>,
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

#[derive(Debug, Clone)]
pub struct DaemonDashboardBarCacheConfig {
    pub enabled: bool,
    pub interval_seconds: u64,
    pub symbols_limit: usize,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApiConfig {
    pub enabled: Option<bool>,
    #[serde(default)]
    pub unix: ApiUnixConfig,
    #[serde(default)]
    pub ssl: ApiSslConfig,
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

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApiUnixConfig {
    pub enabled: Option<bool>,
    pub socket_file: Option<String>,
    pub pid_file: Option<String>,
    pub log_file: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApiSslConfig {
    pub enabled: Option<bool>,
    #[serde(default)]
    pub auth: ApiSslAuthConfig,
    #[serde(default)]
    pub ech: ApiSslEchConfig,
    pub domain: Option<String>,
    pub bind_host: Option<String>,
    pub ipv4_enabled: Option<bool>,
    pub ipv6_enabled: Option<bool>,
    pub udp_port: Option<u16>,
    pub tcp_enabled: Option<bool>,
    pub tcp_bind_host: Option<String>,
    pub tcp_port: Option<u16>,
    pub tcp_bootstrap_enabled: Option<bool>,
    pub tcp_bootstrap_bind_host: Option<String>,
    pub tcp_bootstrap_port: Option<u16>,
    pub trusted_proxy_enabled: Option<bool>,
    pub trusted_proxy_cidrs: Option<Vec<String>>,
    pub pid_file: Option<String>,
    pub log_file: Option<String>,
    pub cert_mode: Option<String>,
    pub cert_file: Option<String>,
    pub key_file: Option<String>,
    pub acme_challenge_cert_file: Option<String>,
    pub acme_challenge_key_file: Option<String>,
    pub key_exchange_policy: Option<String>,
    pub dns_https_check_required: Option<bool>,
    pub tcp_acme_tls_alpn_enabled: Option<bool>,
    pub tcp_acme_bind_host: Option<String>,
    pub tcp_acme_port: Option<u16>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApiSslEchConfig {
    pub enabled: Option<bool>,
    pub public_name: Option<String>,
    pub config_file: Option<String>,
    pub key_file: Option<String>,
    pub require_dns_https_record: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApiSslAuthConfig {
    pub enabled: Option<bool>,
    pub username: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ApiSslRuntimeConfig {
    pub api_enabled: bool,
    pub enabled: bool,
    pub auth_enabled: bool,
    pub auth_username: String,
    pub auth_password: String,
    pub ech_enabled: bool,
    pub ech_public_name: String,
    pub ech_config_file: PathBuf,
    pub ech_key_file: PathBuf,
    pub ech_require_dns_https_record: bool,
    pub domain: String,
    pub bind_host: String,
    pub ipv4_enabled: bool,
    pub ipv6_enabled: bool,
    pub udp_port: u16,
    pub tcp_enabled: bool,
    pub tcp_bind_host: String,
    pub tcp_port: u16,
    pub tcp_bootstrap_enabled: bool,
    pub tcp_bootstrap_bind_host: String,
    pub tcp_bootstrap_port: u16,
    pub trusted_proxy_enabled: bool,
    pub trusted_proxy_cidrs: Vec<String>,
    pub pid_file: PathBuf,
    pub log_file: PathBuf,
    pub cert_mode: String,
    pub cert_file: PathBuf,
    pub key_file: PathBuf,
    pub acme_challenge_cert_file: PathBuf,
    pub acme_challenge_key_file: PathBuf,
    pub key_exchange_policy: String,
    pub dns_https_check_required: bool,
    pub tcp_acme_tls_alpn_enabled: bool,
    pub tcp_acme_bind_host: String,
    pub tcp_acme_port: u16,
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
    pub memory_budget_percent: Option<ResourceSetting>,
    pub cpu_budget_percent: Option<ResourceSetting>,
    pub sqlite_cache_mb: Option<ResourceSetting>,
    pub sqlite_temp_store: Option<String>,
    pub sqlite_mmap_mb: Option<ResourceSetting>,
    pub ml_symbol_batch_size: Option<ResourceSetting>,
    pub lstm_max_sequences: Option<ResourceSetting>,
    pub lstm_batch_size: Option<ResourceSetting>,
    pub lightgbm_max_train_rows: Option<ResourceSetting>,
    pub lightgbm_max_valid_rows: Option<ResourceSetting>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ResourceSetting {
    Number(i64),
    Text(String),
}

#[derive(Debug, Clone)]
pub struct RuntimeResources {
    pub memory_total_bytes: u64,
    pub memory_source: String,
    pub memory_budget_percent: u64,
    pub memory_budget_bytes: u64,
    pub cpu_total_threads: usize,
    pub cpu_budget_percent: u64,
    pub cpu_total_capacity_percent: u64,
    pub cpu_budget_process_percent: u64,
    pub cpu_worker_capacity_percent: u64,
    pub cpu_worker_threads: usize,
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
    pub compute_correlations_before_training: Option<bool>,
    pub include_current_sp500: Option<bool>,
    pub include_open_positions: Option<bool>,
    pub include_bought_symbols: Option<bool>,
    pub bought_symbol_lookback_days: Option<u32>,
    pub include_q1_candidates: Option<bool>,
    pub q1_top_n: Option<usize>,
    pub sync_days: Option<u32>,
    pub source_timeout_seconds: Option<u64>,
    pub source_retry_count: Option<usize>,
    pub auto_tune_sources: Option<bool>,
    pub alpaca_concurrency: Option<usize>,
    pub sec_edgar_concurrency: Option<usize>,
    pub yahoo_rss_concurrency: Option<usize>,
    pub google_rss_concurrency: Option<usize>,
    pub correlation_days: Option<u32>,
    pub correlation_min_overlap_days: Option<usize>,
    pub correlation_strong_threshold: Option<f64>,
    pub correlation_max_symbols: Option<usize>,
    pub extra_symbols: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct FeedsMlSyncConfig {
    pub sync_before_training: bool,
    pub sync_orders_before_training: bool,
    pub compute_correlations_before_training: bool,
    pub include_current_sp500: bool,
    pub include_open_positions: bool,
    pub include_bought_symbols: bool,
    pub bought_symbol_lookback_days: u32,
    pub include_q1_candidates: bool,
    pub q1_top_n: usize,
    pub sync_days: u32,
    pub extra_symbols: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct FeedsCorrelationConfig {
    pub days: u32,
    pub min_overlap_days: usize,
    pub strong_threshold: f64,
    pub max_symbols: usize,
}

#[derive(Debug, Clone)]
pub struct FeedsSourceSyncConfig {
    pub source_timeout_seconds: u64,
    pub source_retry_count: usize,
    pub auto_tune_sources: bool,
    pub alpaca_concurrency: usize,
    pub sec_edgar_concurrency: usize,
    pub yahoo_rss_concurrency: usize,
    pub google_rss_concurrency: usize,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LstmConfig {
    pub profile: Option<String>,
    #[serde(default)]
    pub profiles: LstmProfilesConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LstmProfilesConfig {
    pub cpu: Option<LstmProfileConfig>,
    pub mlx: Option<LstmProfileConfig>,
    pub tch: Option<LstmProfileConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LstmProfileConfig {
    pub target_mode: Option<String>,
    pub direction_threshold: Option<f64>,
    pub hidden_dim: Option<usize>,
    pub epochs: Option<usize>,
    pub learning_rate: Option<f64>,
    pub loss_function: Option<String>,
    pub huber_delta: Option<f64>,
    pub dropout_rate: Option<f64>,
    pub weight_decay: Option<f64>,
    pub early_stopping_enabled: Option<bool>,
    pub early_stopping_patience: Option<usize>,
    pub early_stopping_min_delta: Option<f64>,
    pub early_stopping_sample_size: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct LstmTrainingConfig {
    pub profile: String,
    pub target_mode: String,
    pub direction_threshold: f64,
    pub hidden_dim: usize,
    pub epochs: usize,
    pub learning_rate: f64,
    pub loss_function: String,
    pub huber_delta: f64,
    pub dropout_rate: f64,
    pub weight_decay: f64,
    pub early_stopping_enabled: bool,
    pub early_stopping_patience: usize,
    pub early_stopping_min_delta: f64,
    pub early_stopping_sample_size: usize,
}

// Returns built-in backend defaults when no local ML tuning file is present.
fn default_lstm_profile(profile_name: &str) -> LstmProfileConfig {
    let accelerator = matches!(profile_name, "mlx" | "tch");
    LstmProfileConfig {
        target_mode: Some("regression".to_string()),
        direction_threshold: Some(0.0),
        hidden_dim: Some(if accelerator { 128 } else { 64 }),
        epochs: Some(if accelerator { 50 } else { 10 }),
        learning_rate: Some(if accelerator { 0.000_1 } else { 0.001 }),
        loss_function: Some("mse".to_string()),
        huber_delta: Some(0.01),
        dropout_rate: Some(if accelerator { 0.1 } else { 0.0 }),
        weight_decay: Some(if accelerator { 0.01 } else { 0.0 }),
        early_stopping_enabled: Some(true),
        early_stopping_patience: Some(if accelerator { 10 } else { 5 }),
        early_stopping_min_delta: Some(0.000_001),
        early_stopping_sample_size: Some(if accelerator { 100_000 } else { 50_000 }),
    }
}

// Handles daemon enabled state.
pub fn daemon_enabled() -> bool {
    load()
        .ok()
        .and_then(|config| config.daemon.enabled)
        .unwrap_or(false)
}

// Handles daemon auto trade interval seconds state.
pub fn daemon_auto_trade_interval_seconds() -> u64 {
    load()
        .ok()
        .and_then(|config| config.daemon.auto_trade_interval_seconds)
        .unwrap_or(60)
        .clamp(10, 300)
}

// Returns daemon dashboard market-bar cache warmup settings.
pub fn daemon_dashboard_bar_cache_config() -> DaemonDashboardBarCacheConfig {
    let daemon = load().ok().map(|config| config.daemon).unwrap_or_default();
    DaemonDashboardBarCacheConfig {
        enabled: daemon.dashboard_bar_cache_enabled.unwrap_or(true),
        interval_seconds: daemon
            .dashboard_bar_cache_interval_seconds
            .unwrap_or_else(daemon_auto_trade_interval_seconds)
            .clamp(30, 300),
        symbols_limit: daemon
            .dashboard_bar_cache_symbols_limit
            .unwrap_or(100)
            .clamp(1, 500),
    }
}

// Handles daemon daily refresh config state.
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
            .unwrap_or(360)
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

// Runs the api enabled API helper.
pub fn api_enabled() -> bool {
    api_unix_enabled()
}

// Returns whether the local Unix-socket API transport is enabled.
pub fn api_unix_enabled() -> bool {
    load()
        .ok()
        .map(|config| {
            config.api.enabled.unwrap_or(false) && config.api.unix.enabled.unwrap_or(true)
        })
        .unwrap_or(false)
}

// Runs the api request timeout seconds API helper.
pub fn api_request_timeout_seconds() -> u64 {
    load()
        .ok()
        .and_then(|config| config.api.request_timeout_seconds)
        .unwrap_or(60)
        .clamp(5, 300)
}

// Runs the api long request timeout seconds API helper.
pub fn api_long_request_timeout_seconds() -> u64 {
    load()
        .ok()
        .and_then(|config| config.api.long_request_timeout_seconds)
        .unwrap_or(3600)
        .clamp(60, 86_400)
}

// Runs the api limit config API helper.
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

// Returns the configured Unix API socket path override, preserving legacy keys.
pub fn api_unix_socket_file() -> Option<String> {
    load()
        .ok()
        .and_then(|config| config.api.unix.socket_file.or(config.api.socket_file))
}

// Returns the configured Unix API PID path override, preserving legacy keys.
pub fn api_unix_pid_file() -> Option<String> {
    load()
        .ok()
        .and_then(|config| config.api.unix.pid_file.or(config.api.pid_file))
}

// Returns the configured Unix API log path override, preserving legacy keys.
pub fn api_unix_log_file() -> Option<String> {
    load()
        .ok()
        .and_then(|config| config.api.unix.log_file.or(config.api.log_file))
}

// Returns the default trusted local/private proxy CIDRs for SSL/H3 forwarding headers.
fn default_ssl_trusted_proxy_cidrs() -> Vec<String> {
    [
        "127.0.0.1/32",
        "::1/128",
        "10.0.0.0/8",
        "172.16.0.0/12",
        "192.168.0.0/16",
        "169.254.0.0/16",
        "fc00::/7",
        "fe80::/10",
    ]
    .into_iter()
    .map(ToString::to_string)
    .collect()
}

// Returns the normalized remote HTTP/3 API settings.
pub fn api_ssl_runtime_config() -> ApiSslRuntimeConfig {
    let api = load().ok().map(|config| config.api).unwrap_or_default();
    let ssl = api.ssl;
    let api_enabled = api.enabled.unwrap_or(false);
    let cert_mode = ssl
        .cert_mode
        .unwrap_or_else(|| "provided".to_string())
        .trim()
        .to_ascii_lowercase();
    let cert_file = paths::path_in_runtime_dir(
        paths::config_dir().join("cert"),
        ssl.cert_file,
        "mlai-trade-api.crt",
    );
    let key_file = paths::path_in_runtime_dir(
        paths::config_dir().join("cert"),
        ssl.key_file,
        "mlai-trade-api.key",
    );
    let acme_challenge_cert_file = paths::path_in_runtime_dir(
        paths::config_dir().join("cert"),
        ssl.acme_challenge_cert_file,
        "mlai-trade-api-acme-tls-alpn-01.crt",
    );
    let acme_challenge_key_file = paths::path_in_runtime_dir(
        paths::config_dir().join("cert"),
        ssl.acme_challenge_key_file,
        "mlai-trade-api-acme-tls-alpn-01.key",
    );
    let ech_config_file = paths::path_in_runtime_dir(
        paths::config_dir().join("cert"),
        ssl.ech.config_file,
        "mlai-trade-api-ech-config.bin",
    );
    let ech_key_file = paths::path_in_runtime_dir(
        paths::config_dir().join("cert"),
        ssl.ech.key_file,
        "mlai-trade-api-ech.key",
    );
    ApiSslRuntimeConfig {
        api_enabled,
        enabled: api_enabled && ssl.enabled.unwrap_or(false),
        auth_enabled: ssl.auth.enabled.unwrap_or(true),
        auth_username: ssl.auth.username.unwrap_or_else(|| "admin".to_string()),
        auth_password: ssl
            .auth
            .password
            .unwrap_or_else(|| "replace_me".to_string()),
        ech_enabled: ssl.ech.enabled.unwrap_or(false),
        ech_public_name: ssl.ech.public_name.unwrap_or_default(),
        ech_config_file,
        ech_key_file,
        ech_require_dns_https_record: ssl.ech.require_dns_https_record.unwrap_or(true),
        domain: ssl.domain.unwrap_or_default(),
        bind_host: ssl
            .bind_host
            .clone()
            .unwrap_or_else(|| "0.0.0.0".to_string()),
        ipv4_enabled: ssl.ipv4_enabled.unwrap_or(true),
        ipv6_enabled: ssl.ipv6_enabled.unwrap_or(true),
        udp_port: ssl.udp_port.unwrap_or(443).clamp(1, u16::MAX),
        tcp_enabled: ssl
            .tcp_enabled
            .or(ssl.tcp_bootstrap_enabled)
            .unwrap_or(true),
        tcp_bind_host: ssl
            .tcp_bind_host
            .clone()
            .or_else(|| ssl.tcp_bootstrap_bind_host.clone())
            .or_else(|| ssl.bind_host.clone())
            .unwrap_or_else(|| "0.0.0.0".to_string()),
        tcp_port: ssl
            .tcp_port
            .or(ssl.tcp_bootstrap_port)
            .or(ssl.udp_port)
            .unwrap_or(443)
            .clamp(1, u16::MAX),
        tcp_bootstrap_enabled: ssl
            .tcp_bootstrap_enabled
            .or(ssl.tcp_enabled)
            .unwrap_or(true),
        tcp_bootstrap_bind_host: ssl
            .tcp_bootstrap_bind_host
            .clone()
            .or_else(|| ssl.tcp_bind_host.clone())
            .or_else(|| ssl.bind_host.clone())
            .unwrap_or_else(|| "0.0.0.0".to_string()),
        tcp_bootstrap_port: ssl
            .tcp_bootstrap_port
            .or(ssl.tcp_port)
            .or(ssl.udp_port)
            .unwrap_or(443)
            .clamp(1, u16::MAX),
        trusted_proxy_enabled: ssl.trusted_proxy_enabled.unwrap_or(true),
        trusted_proxy_cidrs: ssl
            .trusted_proxy_cidrs
            .filter(|values| !values.is_empty())
            .unwrap_or_else(default_ssl_trusted_proxy_cidrs),
        pid_file: paths::path_in_runtime_dir(
            paths::tmp_dir(),
            ssl.pid_file,
            "mlai-trade-api-ssl.pid",
        ),
        log_file: paths::path_in_runtime_dir(
            paths::logs_dir(),
            ssl.log_file,
            "mlai-trade-api-ssl.log",
        ),
        cert_mode,
        cert_file,
        key_file,
        acme_challenge_cert_file,
        acme_challenge_key_file,
        key_exchange_policy: ssl
            .key_exchange_policy
            .unwrap_or_else(|| "mlkem_secure_fallback".to_string())
            .trim()
            .to_ascii_lowercase(),
        dns_https_check_required: ssl.dns_https_check_required.unwrap_or(true),
        tcp_acme_tls_alpn_enabled: ssl.tcp_acme_tls_alpn_enabled.unwrap_or(false),
        tcp_acme_bind_host: ssl
            .tcp_acme_bind_host
            .unwrap_or_else(|| "0.0.0.0".to_string()),
        tcp_acme_port: ssl.tcp_acme_port.unwrap_or(443).clamp(1, u16::MAX),
    }
}

// Handles runtime resources logic.
pub fn runtime_resources() -> RuntimeResources {
    let resources = load()
        .ok()
        .map(|config| config.resources)
        .unwrap_or_default();
    let memory = detect_memory_limit();
    let memory_budget_percent =
        resource_setting_u64(resources.memory_budget_percent.as_ref()).unwrap_or(80);
    let memory_budget_percent = memory_budget_percent.clamp(10, 95);
    let memory_budget_bytes = memory
        .bytes
        .saturating_mul(memory_budget_percent)
        .saturating_div(100);
    let cpu_total_threads = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1)
        .max(1);
    let cpu_budget_percent =
        resource_setting_u64(resources.cpu_budget_percent.as_ref()).unwrap_or(80);
    let cpu_budget_percent = cpu_budget_percent.clamp(10, 100);
    let cpu_worker_threads = ((cpu_total_threads as u64)
        .saturating_mul(cpu_budget_percent)
        .saturating_div(100))
    .max(1)
    .min(cpu_total_threads as u64) as usize;
    let sqlite_temp_store = match resources
        .sqlite_temp_store
        .unwrap_or_else(|| "file".to_string())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "auto" => "FILE".to_string(),
        "memory" => "MEMORY".to_string(),
        _ => "FILE".to_string(),
    };
    let auto = auto_resource_defaults(memory_budget_bytes);
    RuntimeResources {
        memory_total_bytes: memory.bytes,
        memory_source: memory.source,
        memory_budget_percent,
        memory_budget_bytes,
        cpu_total_threads,
        cpu_budget_percent,
        cpu_total_capacity_percent: (cpu_total_threads as u64).saturating_mul(100),
        cpu_budget_process_percent: (cpu_total_threads as u64).saturating_mul(cpu_budget_percent),
        cpu_worker_capacity_percent: (cpu_worker_threads as u64).saturating_mul(100),
        cpu_worker_threads,
        sqlite_cache_mb: resource_setting_u64(resources.sqlite_cache_mb.as_ref())
            .map(|value| value as i64)
            .unwrap_or(auto.sqlite_cache_mb)
            .clamp(4, 4096),
        sqlite_temp_store,
        sqlite_mmap_mb: resource_setting_u64(resources.sqlite_mmap_mb.as_ref())
            .map(|value| value as i64)
            .unwrap_or(auto.sqlite_mmap_mb)
            .clamp(0, 16_384),
        ml_symbol_batch_size: resource_setting_u64(resources.ml_symbol_batch_size.as_ref())
            .map(|value| value as usize)
            .unwrap_or(auto.ml_symbol_batch_size)
            .clamp(25, 5000),
        lstm_max_sequences: resource_setting_u64(resources.lstm_max_sequences.as_ref())
            .map(|value| value as usize)
            .unwrap_or(auto.lstm_max_sequences)
            .clamp(1_000, 2_000_000),
        lstm_batch_size: resource_setting_u64(resources.lstm_batch_size.as_ref())
            .map(|value| value as usize)
            .unwrap_or(auto.lstm_batch_size)
            .clamp(8, 2048),
        lightgbm_max_train_rows: resource_setting_u64(resources.lightgbm_max_train_rows.as_ref())
            .map(|value| value.min(100_000_000) as usize)
            .unwrap_or(auto.lightgbm_max_train_rows),
        lightgbm_max_valid_rows: resource_setting_u64(resources.lightgbm_max_valid_rows.as_ref())
            .map(|value| value.min(25_000_000) as usize)
            .unwrap_or_else(|| {
                auto.lightgbm_max_valid_rows
                    .min(auto.lightgbm_max_train_rows.saturating_div(4).max(1))
            }),
    }
}

// Returns runtime resource limits as JSON for status output.
pub fn runtime_resources_json() -> Value {
    let resources = runtime_resources();
    serde_json::json!({
        "memory_total_bytes": resources.memory_total_bytes,
        "memory_source": resources.memory_source,
        "memory_budget_percent": resources.memory_budget_percent,
        "memory_budget_bytes": resources.memory_budget_bytes,
        "cpu_total_threads": resources.cpu_total_threads,
        "cpu_budget_percent": resources.cpu_budget_percent,
        "cpu_total_capacity_percent": resources.cpu_total_capacity_percent,
        "cpu_budget_process_percent": resources.cpu_budget_process_percent,
        "cpu_worker_capacity_percent": resources.cpu_worker_capacity_percent,
        "cpu_worker_threads": resources.cpu_worker_threads,
        "sqlite_cache_mb": resources.sqlite_cache_mb,
        "sqlite_temp_store": resources.sqlite_temp_store,
        "sqlite_mmap_mb": resources.sqlite_mmap_mb,
        "ml_symbol_batch_size": resources.ml_symbol_batch_size,
        "lstm_max_sequences": resources.lstm_max_sequences,
        "lstm_batch_size": resources.lstm_batch_size,
        "lightgbm_max_train_rows": resources.lightgbm_max_train_rows,
        "lightgbm_max_valid_rows": resources.lightgbm_max_valid_rows,
    })
}

// Returns the automatic CPU worker-thread cap for CPU-bound training.
pub fn cpu_worker_threads() -> usize {
    runtime_resources().cpu_worker_threads
}

#[derive(Debug, Clone)]
struct MemoryLimit {
    bytes: u64,
    source: String,
}

#[derive(Debug, Clone)]
struct AutoResourceDefaults {
    sqlite_cache_mb: i64,
    sqlite_mmap_mb: i64,
    ml_symbol_batch_size: usize,
    lstm_max_sequences: usize,
    lstm_batch_size: usize,
    lightgbm_max_train_rows: usize,
    lightgbm_max_valid_rows: usize,
}

// Handles resource setting u64 logic.
fn resource_setting_u64(setting: Option<&ResourceSetting>) -> Option<u64> {
    match setting {
        Some(ResourceSetting::Number(value)) => (*value >= 0).then_some(*value as u64),
        Some(ResourceSetting::Text(value)) => {
            let value = value.trim();
            if value.is_empty() || value.eq_ignore_ascii_case("auto") {
                None
            } else if value.eq_ignore_ascii_case("unlimited") || value.eq_ignore_ascii_case("none")
            {
                Some(0)
            } else {
                value.parse::<u64>().ok()
            }
        }
        None => None,
    }
}

// Handles auto-trading resource defaults state.
fn auto_resource_defaults(memory_budget_bytes: u64) -> AutoResourceDefaults {
    let budget_mib = bytes_to_mib(memory_budget_bytes).max(512);
    let sqlite_cache_mb = (budget_mib / 50).clamp(32, 4096) as i64;
    let sqlite_mmap_mb = if budget_mib < 8_192 {
        0
    } else {
        (budget_mib / 8).clamp(512, 16_384) as i64
    };
    let ml_symbol_batch_size = match budget_mib {
        0..=3_999 => 250,
        4_000..=7_999 => 500,
        8_000..=15_999 => 1000,
        16_000..=31_999 => 2000,
        _ => 5000,
    };
    let lstm_max_sequences = ((memory_budget_bytes / 4) / 12_000).clamp(10_000, 2_000_000) as usize;
    let lstm_batch_size = match budget_mib {
        0..=3_999 => 32,
        4_000..=7_999 => 64,
        8_000..=15_999 => 128,
        16_000..=31_999 => 256,
        32_000..=63_999 => 512,
        _ => 1024,
    };
    let lightgbm_max_train_rows =
        ((memory_budget_bytes / 3) / 1024).clamp(250_000, 100_000_000) as usize;
    let lightgbm_max_valid_rows = (lightgbm_max_train_rows / 8).clamp(50_000, 25_000_000);
    AutoResourceDefaults {
        sqlite_cache_mb,
        sqlite_mmap_mb,
        ml_symbol_batch_size,
        lstm_max_sequences,
        lstm_batch_size,
        lightgbm_max_train_rows,
        lightgbm_max_valid_rows,
    }
}

// Handles bytes to mib logic.
fn bytes_to_mib(bytes: u64) -> u64 {
    bytes / 1_048_576
}

// Handles detect memory limit logic.
fn detect_memory_limit() -> MemoryLimit {
    const FALLBACK_BYTES: u64 = 4 * 1024 * 1024 * 1024;
    platform_memory_limit()
        .or_else(unix_sysconf_memory_limit)
        .unwrap_or_else(|| MemoryLimit {
            bytes: FALLBACK_BYTES,
            source: "fallback_4gib".to_string(),
        })
}

#[cfg(target_os = "linux")]
// Handles platform memory limit logic.
fn platform_memory_limit() -> Option<MemoryLimit> {
    let host = linux_proc_mem_total();
    let cgroup = linux_cgroup_memory_limit();
    match (host, cgroup) {
        (Some((host_bytes, _)), Some((cgroup_bytes, cgroup_source)))
            if cgroup_bytes < host_bytes =>
        {
            Some(MemoryLimit {
                bytes: cgroup_bytes,
                source: cgroup_source,
            })
        }
        (Some((host_bytes, host_source)), _) => Some(MemoryLimit {
            bytes: host_bytes,
            source: host_source,
        }),
        (_, Some((cgroup_bytes, cgroup_source))) => Some(MemoryLimit {
            bytes: cgroup_bytes,
            source: cgroup_source,
        }),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
// Handles linux proc mem total logic.
fn linux_proc_mem_total() -> Option<(u64, String)> {
    let data = fs::read_to_string("/proc/meminfo").ok()?;
    let line = data.lines().find(|line| line.starts_with("MemTotal:"))?;
    let kib = line.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    Some((kib.saturating_mul(1024), "proc_meminfo".to_string()))
}

#[cfg(target_os = "linux")]
// Handles linux cgroup memory limit logic.
fn linux_cgroup_memory_limit() -> Option<(u64, String)> {
    const MIN_REASONABLE_BYTES: u64 = 128 * 1024 * 1024;
    const MAX_REASONABLE_BYTES: u64 = 1_u64 << 60;
    for path in [
        "/sys/fs/cgroup/memory.max",
        "/sys/fs/cgroup/memory/memory.limit_in_bytes",
    ] {
        let Ok(raw) = fs::read_to_string(path) else {
            continue;
        };
        let raw = raw.trim();
        if raw.eq_ignore_ascii_case("max") {
            continue;
        }
        let bytes = raw.parse::<u64>().ok()?;
        if (MIN_REASONABLE_BYTES..MAX_REASONABLE_BYTES).contains(&bytes) {
            return Some((bytes, path.to_string()));
        }
    }
    None
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
// Handles sysctl memory limit logic.
fn sysctl_memory_limit(name: &str, source: &str) -> Option<MemoryLimit> {
    use std::ffi::CString;
    let name = CString::new(name).ok()?;
    let mut bytes = 0_u64;
    let mut len = std::mem::size_of::<u64>();
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            &mut bytes as *mut u64 as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc == 0 && bytes > 0 {
        return Some(MemoryLimit {
            bytes,
            source: source.to_string(),
        });
    }

    let mut bytes = 0 as libc::c_ulong;
    let mut len = std::mem::size_of::<libc::c_ulong>();
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            &mut bytes as *mut libc::c_ulong as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && bytes > 0).then(|| MemoryLimit {
        bytes: bytes as u64,
        source: source.to_string(),
    })
}

#[cfg(target_os = "macos")]
// Handles platform memory limit logic.
fn platform_memory_limit() -> Option<MemoryLimit> {
    sysctl_memory_limit("hw.memsize", "sysctl_hw_memsize")
}

#[cfg(target_os = "freebsd")]
// Handles platform memory limit logic.
fn platform_memory_limit() -> Option<MemoryLimit> {
    sysctl_memory_limit("hw.physmem", "sysctl_hw_physmem")
        .or_else(|| sysctl_memory_limit("hw.realmem", "sysctl_hw_realmem"))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
// Handles platform memory limit logic.
fn platform_memory_limit() -> Option<MemoryLimit> {
    None
}

#[cfg(unix)]
// Handles unix sysconf memory limit logic.
fn unix_sysconf_memory_limit() -> Option<MemoryLimit> {
    let pages = unsafe { libc::sysconf(libc::_SC_PHYS_PAGES) };
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if pages <= 0 || page_size <= 0 {
        return None;
    }
    Some(MemoryLimit {
        bytes: (pages as u64).saturating_mul(page_size as u64),
        source: "sysconf_phys_pages".to_string(),
    })
}

#[cfg(not(unix))]
// Handles unix sysconf memory limit logic.
fn unix_sysconf_memory_limit() -> Option<MemoryLimit> {
    None
}

// Handles SQLite runtime pragma sql safely.
pub fn sqlite_runtime_pragma_sql() -> String {
    let resources = runtime_resources();
    let cache_kib = resources.sqlite_cache_mb * 1024;
    let mmap_bytes = resources.sqlite_mmap_mb * 1_048_576;
    format!(
        "PRAGMA cache_size=-{cache_kib}; PRAGMA temp_store={}; PRAGMA mmap_size={mmap_bytes};",
        resources.sqlite_temp_store
    )
}

// Handles ml symbol batch size logic.
pub fn ml_symbol_batch_size() -> usize {
    runtime_resources().ml_symbol_batch_size
}

// Returns LSTM max sequences runtime settings.
pub fn lstm_max_sequences() -> usize {
    runtime_resources().lstm_max_sequences
}

// Returns LSTM batch size runtime settings.
pub fn lstm_batch_size() -> usize {
    runtime_resources().lstm_batch_size
}

// Handles lightgbm max train rows logic.
pub fn lightgbm_max_train_rows() -> usize {
    runtime_resources().lightgbm_max_train_rows
}

// Handles lightgbm max valid rows logic.
pub fn lightgbm_max_valid_rows() -> usize {
    runtime_resources().lightgbm_max_valid_rows
}

// Handles feeds ml sync config logic.
pub fn feeds_ml_sync_config() -> FeedsMlSyncConfig {
    let feeds = load().ok().map(|config| config.feeds).unwrap_or_default();
    FeedsMlSyncConfig {
        sync_before_training: feeds.sync_before_training.unwrap_or(true),
        sync_orders_before_training: feeds.sync_orders_before_training.unwrap_or(true),
        compute_correlations_before_training: feeds
            .compute_correlations_before_training
            .unwrap_or(true),
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

// Returns feed price-correlation policy settings.
pub fn feeds_correlation_config() -> FeedsCorrelationConfig {
    let feeds = load().ok().map(|config| config.feeds).unwrap_or_default();
    FeedsCorrelationConfig {
        days: feeds.correlation_days.unwrap_or(90).clamp(10, 252),
        min_overlap_days: feeds
            .correlation_min_overlap_days
            .unwrap_or(30)
            .clamp(10, 252),
        strong_threshold: feeds
            .correlation_strong_threshold
            .unwrap_or(0.7)
            .clamp(0.1, 0.99),
        max_symbols: feeds.correlation_max_symbols.unwrap_or(1500).clamp(2, 5000),
    }
}

// Returns feed source concurrency, timeout, and retry settings.
pub fn feeds_source_sync_config() -> FeedsSourceSyncConfig {
    let feeds = load().ok().map(|config| config.feeds).unwrap_or_default();
    FeedsSourceSyncConfig {
        source_timeout_seconds: feeds.source_timeout_seconds.unwrap_or(10).clamp(5, 120),
        source_retry_count: feeds.source_retry_count.unwrap_or(2).clamp(0, 10),
        auto_tune_sources: feeds.auto_tune_sources.unwrap_or(true),
        alpaca_concurrency: feeds.alpaca_concurrency.unwrap_or(2).clamp(1, 16),
        sec_edgar_concurrency: feeds.sec_edgar_concurrency.unwrap_or(1).clamp(1, 4),
        yahoo_rss_concurrency: feeds.yahoo_rss_concurrency.unwrap_or(2).clamp(1, 16),
        google_rss_concurrency: feeds.google_rss_concurrency.unwrap_or(2).clamp(1, 16),
    }
}

// Returns LSTM architecture and training policy for the resolved backend.
pub fn lstm_training_config_for_backend(backend: &str) -> LstmTrainingConfig {
    let lstm = load_ml_tuning()
        .ok()
        .map(|config| config.lstm)
        .unwrap_or_default();
    let requested_profile = lstm
        .profile
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| matches!(value.as_str(), "auto" | "cpu" | "mlx" | "tch"))
        .unwrap_or_else(|| "auto".to_string());
    let profile_name = if requested_profile == "auto" {
        match backend.trim().to_ascii_lowercase().as_str() {
            "mlx" => "mlx",
            "tch" => "tch",
            _ => "cpu",
        }
    } else {
        requested_profile.as_str()
    };
    let profile = match profile_name {
        "mlx" => lstm.profiles.mlx,
        "tch" => lstm.profiles.tch,
        _ => lstm.profiles.cpu,
    }
    .unwrap_or_else(|| default_lstm_profile(profile_name));
    let target_mode = profile
        .target_mode
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| matches!(value.as_str(), "regression" | "direction"))
        .unwrap_or_else(|| "regression".to_string());
    LstmTrainingConfig {
        profile: profile_name.to_string(),
        target_mode,
        direction_threshold: profile.direction_threshold.unwrap_or(0.0).clamp(0.0, 1.0),
        hidden_dim: profile.hidden_dim.unwrap_or(64).clamp(16, 512),
        epochs: profile.epochs.unwrap_or(10).clamp(1, 200),
        learning_rate: profile.learning_rate.unwrap_or(0.001).clamp(0.000_001, 0.1),
        loss_function: profile
            .loss_function
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| matches!(value.as_str(), "mse" | "huber" | "l1" | "bce"))
            .unwrap_or_else(|| "mse".to_string()),
        huber_delta: profile.huber_delta.unwrap_or(0.01).clamp(0.000_001, 1.0),
        dropout_rate: profile.dropout_rate.unwrap_or(0.0).clamp(0.0, 0.9),
        weight_decay: profile.weight_decay.unwrap_or(0.0).clamp(0.0, 1.0),
        early_stopping_enabled: profile.early_stopping_enabled.unwrap_or(true),
        early_stopping_patience: profile.early_stopping_patience.unwrap_or(5).clamp(1, 50),
        early_stopping_min_delta: profile
            .early_stopping_min_delta
            .unwrap_or(0.000_001)
            .clamp(0.0, 1.0),
        early_stopping_sample_size: profile
            .early_stopping_sample_size
            .unwrap_or(50_000)
            .clamp(1_000, 1_000_000),
    }
}

// Normalizes symbol into canonical form.
fn normalize_symbol(value: &str) -> Option<String> {
    let symbol = value.trim().to_ascii_uppercase();
    if symbol.is_empty() {
        None
    } else {
        Some(symbol)
    }
}

// Handles blocked symbols logic.
pub fn blocked_symbols() -> Vec<String> {
    load()
        .ok()
        .map(|config| config.auto.compliance.blocked_symbols)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|symbol| normalize_symbol(&symbol))
        .collect()
}

// Returns whether blocked symbol is true.
pub fn is_blocked_symbol(symbol: &str) -> bool {
    let Some(symbol) = normalize_symbol(symbol) else {
        return false;
    };
    blocked_symbols().iter().any(|blocked| blocked == &symbol)
}

// Handles blocked symbols sql predicate logic.
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
    use super::{
        normalize_symbol, validate_config_value, validate_ml_tuning_config_value, AppConfig,
        MlTuningConfig,
    };

    // Returns whether path is true.
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
    // Handles example config parses and documents every supported key logic.
    fn example_config_parses_and_documents_every_supported_key() {
        let raw = include_str!("../config/mlai-trade.example.json");
        let value: serde_json::Value = serde_json::from_str(raw).expect("valid example JSON");
        let _: AppConfig = serde_json::from_value(value.clone()).expect("example config parses");
        for path in [
            &["providers", "alpaca", "enabled"][..],
            &["providers", "other"],
            &["alpaca", "accounts", "0", "name"],
            &["alpaca", "accounts", "0", "enabled"],
            &["alpaca", "accounts", "0", "auto_trade_enabled"],
            &["alpaca", "accounts", "0", "account_mode"],
            &["alpaca", "accounts", "0", "data_feed"],
            &["alpaca", "accounts", "0", "trading_base_url"],
            &["alpaca", "accounts", "0", "data_base_url"],
            &["alpaca", "accounts", "0", "api_key_id"],
            &["alpaca", "accounts", "0", "secret_key"],
            &["fred", "api_key"],
            &["tax", "filing_status"],
            &["tax", "estimated_annual_income"],
            &["tax", "include_paper_accounts_for_estimate"],
            &["tax", "brackets_file"],
            &["daemon", "enabled"],
            &["daemon", "auto_trade_interval_seconds"],
            &["daemon", "dashboard_bar_cache_enabled"],
            &["daemon", "dashboard_bar_cache_interval_seconds"],
            &["daemon", "dashboard_bar_cache_symbols_limit"],
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
            &["feeds", "source_timeout_seconds"],
            &["feeds", "source_retry_count"],
            &["feeds", "auto_tune_sources"],
            &["feeds", "alpaca_concurrency"],
            &["feeds", "sec_edgar_concurrency"],
            &["feeds", "yahoo_rss_concurrency"],
            &["feeds", "google_rss_concurrency"],
            &["feeds", "extra_symbols"],
            &["scan", "max_concurrent"],
            &["scan", "max_retries"],
            &["backend", "lstm"],
            &["backend", "xgboost"],
            &["backend", "lightgbm"],
            &["backend", "ridge"],
            &["resources", "memory_budget_percent"],
            &["resources", "cpu_budget_percent"],
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
            &["auto", "stop_loss_confirmation", "enabled"],
            &["auto", "stop_loss_confirmation", "cycles"],
            &["auto", "stop_loss_confirmation", "max_confirmation_minutes"],
            &["auto", "stop_loss_confirmation", "emergency_stop_loss_pct"],
            &["auto", "take_profit_confirmation", "enabled"],
            &["auto", "take_profit_confirmation", "cycles"],
            &["auto", "take_profit_confirmation", "min_hold_minutes"],
            &["auto", "take_profit_confirmation", "trailing_enabled"],
            &["auto", "take_profit_confirmation", "trailing_giveback_pct"],
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
    // Normalizes symbol uppercases and trims market symbols into canonical form.
    fn normalize_symbol_uppercases_and_trims_market_symbols() {
        assert_eq!(normalize_symbol("meta").as_deref(), Some("META"));
        assert_eq!(normalize_symbol(" Meta ").as_deref(), Some("META"));
        assert_eq!(normalize_symbol("brk.b").as_deref(), Some("BRK.B"));
        assert_eq!(normalize_symbol("").as_deref(), None);
        assert_eq!(normalize_symbol("   ").as_deref(), None);
    }

    #[test]
    // Builds or returns validation reports invalid resource path configuration state.
    fn config_validation_reports_invalid_resource_path() {
        let mut value: serde_json::Value =
            serde_json::from_str(include_str!("../config/mlai-trade.example.json"))
                .expect("valid example JSON");
        value["resources"]["memory_budget_percent"] = serde_json::json!(-1);
        let err = validate_config_value(&value).expect_err("negative budget must fail");
        let text = err.to_string();
        assert!(text.contains("$.resources.memory_budget_percent"));
        assert!(text.contains("10-95"));
    }

    #[test]
    // Builds or returns validation reports unknown key path configuration state.
    fn config_validation_reports_unknown_key_path() {
        let mut value: serde_json::Value =
            serde_json::from_str(include_str!("../config/mlai-trade.example.json"))
                .expect("valid example JSON");
        value["resources"]["memory_budget_percnt"] = serde_json::json!(80);
        let err = validate_config_value(&value).expect_err("unknown key must fail");
        assert!(err.to_string().contains("$.resources.memory_budget_percnt"));
    }

    #[test]
    // Rejects duplicate account names within the same provider namespace.
    fn config_validation_rejects_duplicate_alpaca_account_names() {
        let mut value: serde_json::Value =
            serde_json::from_str(include_str!("../config/mlai-trade.example.json"))
                .expect("valid example JSON");
        value["alpaca"]["accounts"][1]["name"] = serde_json::json!("PAPER-MAIN");
        let err = validate_config_value(&value).expect_err("duplicate account name must fail");
        let text = err.to_string();
        assert!(text.contains("$.alpaca.accounts[1].name"));
        assert!(text.contains("duplicate account name"));
    }

    #[test]
    // Handles ML tuning example config parsing and validation.
    fn ml_tuning_example_config_parses() {
        let raw = include_str!("../config/mlai-trade-ml-tuning.example.json");
        let value: serde_json::Value = serde_json::from_str(raw).expect("valid tuning JSON");
        validate_ml_tuning_config_value(&value).expect("tuning config validates");
        let _: MlTuningConfig = serde_json::from_value(value).expect("tuning config parses");
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
    #[serde(default)]
    pub stop_loss_confirmation: StopLossConfirmationConfig,
    #[serde(default)]
    pub take_profit_confirmation: TakeProfitConfirmationConfig,
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
pub struct StopLossConfirmationConfig {
    pub enabled: Option<bool>,
    pub cycles: Option<i64>,
    pub max_confirmation_minutes: Option<i64>,
    pub emergency_stop_loss_pct: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TakeProfitConfirmationConfig {
    pub enabled: Option<bool>,
    pub cycles: Option<i64>,
    pub min_hold_minutes: Option<i64>,
    pub trailing_enabled: Option<bool>,
    pub trailing_giveback_pct: Option<f64>,
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

// Returns the runtime path for join.
fn path_join(path: &str, key: &str) -> String {
    if path == "$" {
        format!("$.{key}")
    } else {
        format!("{path}.{key}")
    }
}

// Builds or returns error configuration state.
fn config_error(path: &str, message: impl AsRef<str>, expected: impl AsRef<str>) -> anyhow::Error {
    anyhow::anyhow!(
        "config error at {path}: {}; expected {}",
        message.as_ref(),
        expected.as_ref()
    )
}

// Handles object at logic.
fn object_at<'a>(
    value: &'a Value,
    path: &str,
) -> anyhow::Result<&'a serde_json::Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| config_error(path, "value has the wrong type", "an object"))
}

// Handles allow object keys logic.
fn allow_object_keys(
    value: &Value,
    path: &str,
    allowed: &[&str],
) -> anyhow::Result<BTreeSet<String>> {
    let object = object_at(value, path)?;
    let allowed_set = allowed.iter().copied().collect::<BTreeSet<_>>();
    for key in object.keys() {
        if !allowed_set.contains(key.as_str()) {
            return Err(config_error(
                &path_join(path, key),
                "unknown configuration key",
                format!("one of: {}", allowed.join(", ")),
            ));
        }
    }
    Ok(object.keys().cloned().collect())
}

// Handles optional child logic.
fn optional_child<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.as_object().and_then(|object| object.get(key))
}

// Validates bool against supported rules.
fn validate_bool(value: &Value, path: &str) -> anyhow::Result<()> {
    value
        .as_bool()
        .map(|_| ())
        .ok_or_else(|| config_error(path, "value has the wrong type", "true or false"))
}

// Validates string against supported rules.
fn validate_string(value: &Value, path: &str) -> anyhow::Result<()> {
    value
        .as_str()
        .map(|_| ())
        .ok_or_else(|| config_error(path, "value has the wrong type", "a string"))
}

// Validates string array against supported rules.
fn validate_string_array(value: &Value, path: &str) -> anyhow::Result<()> {
    let array = value
        .as_array()
        .ok_or_else(|| config_error(path, "value has the wrong type", "an array of strings"))?;
    for (idx, item) in array.iter().enumerate() {
        validate_string(item, &format!("{path}[{idx}]"))?;
    }
    Ok(())
}

// Validates one IP or CIDR value.
fn validate_ip_cidr(value: &Value, path: &str) -> anyhow::Result<()> {
    let text = value
        .as_str()
        .ok_or_else(|| config_error(path, "value has the wrong type", "an IP or CIDR string"))?
        .trim();
    if text.is_empty() {
        return Err(config_error(
            path,
            "empty CIDR is not allowed",
            "an IP or CIDR string",
        ));
    }
    let (addr, prefix) = text
        .split_once('/')
        .map(|(addr, prefix)| (addr, Some(prefix)))
        .unwrap_or((text, None));
    let ip = addr
        .parse::<IpAddr>()
        .map_err(|_| config_error(path, "invalid IP address", "an IP or CIDR string"))?;
    if let Some(prefix) = prefix {
        let prefix = prefix
            .parse::<u8>()
            .map_err(|_| config_error(path, "invalid CIDR prefix", "a numeric CIDR prefix"))?;
        let max_prefix = if ip.is_ipv4() { 32 } else { 128 };
        if prefix > max_prefix {
            return Err(config_error(
                path,
                "CIDR prefix is out of range",
                format!("0-{max_prefix}"),
            ));
        }
    }
    Ok(())
}

// Validates an array of IP/CIDR values.
fn validate_ip_cidr_array(value: &Value, path: &str) -> anyhow::Result<()> {
    let array = value.as_array().ok_or_else(|| {
        config_error(
            path,
            "value has the wrong type",
            "an array of IP/CIDR strings",
        )
    })?;
    for (idx, item) in array.iter().enumerate() {
        validate_ip_cidr(item, &format!("{path}[{idx}]"))?;
    }
    Ok(())
}

// Validates number range against supported rules.
fn validate_number_range(
    value: &Value,
    path: &str,
    min: f64,
    max: f64,
    expected: &str,
) -> anyhow::Result<()> {
    let Some(number) = value.as_f64() else {
        return Err(config_error(path, "value has the wrong type", expected));
    };
    if !number.is_finite() || number < min || number > max {
        return Err(config_error(
            path,
            format!("value {number} is out of range"),
            expected,
        ));
    }
    Ok(())
}

// Validates int range against supported rules.
fn validate_int_range(
    value: &Value,
    path: &str,
    min: i64,
    max: i64,
    expected: &str,
) -> anyhow::Result<()> {
    let Some(number) = value.as_i64() else {
        return Err(config_error(path, "value has the wrong type", expected));
    };
    if number < min || number > max {
        return Err(config_error(
            path,
            format!("value {number} is out of range"),
            expected,
        ));
    }
    Ok(())
}

// Validates enum against supported rules.
fn validate_enum(value: &Value, path: &str, allowed: &[&str]) -> anyhow::Result<()> {
    let Some(text) = value.as_str() else {
        return Err(config_error(
            path,
            "value has the wrong type",
            format!("one of: {}", allowed.join(", ")),
        ));
    };
    let normalized = text.trim().to_ascii_lowercase().replace([' ', '-'], "_");
    if !allowed.iter().any(|allowed| *allowed == normalized) {
        return Err(config_error(
            path,
            format!("unsupported value '{text}'"),
            format!("one of: {}", allowed.join(", ")),
        ));
    }
    Ok(())
}

// Validates time against supported rules.
fn validate_time(value: &Value, path: &str) -> anyhow::Result<()> {
    let Some(text) = value.as_str() else {
        return Err(config_error(path, "value has the wrong type", "HH:MM:SS"));
    };
    NaiveTime::parse_from_str(text, "%H:%M:%S")
        .map(|_| ())
        .map_err(|_| config_error(path, format!("invalid time '{text}'"), "HH:MM:SS"))
}

// Validates resource setting against supported rules.
fn validate_resource_setting(
    value: &Value,
    path: &str,
    min: i64,
    max: i64,
    allow_zero: bool,
    allow_unlimited: bool,
) -> anyhow::Result<()> {
    if let Some(text) = value.as_str() {
        let normalized = text.trim().to_ascii_lowercase();
        if normalized == "auto" {
            return Ok(());
        }
        if allow_unlimited && matches!(normalized.as_str(), "unlimited" | "none") {
            return Ok(());
        }
        if let Ok(number) = normalized.parse::<i64>() {
            return validate_resource_number(number, path, min, max, allow_zero);
        }
        let expected = if allow_unlimited {
            format!("auto, unlimited, none, or integer {min}-{max}")
        } else {
            format!("auto or integer {min}-{max}")
        };
        return Err(config_error(
            path,
            format!("unsupported value '{text}'"),
            expected,
        ));
    }
    let Some(number) = value.as_i64() else {
        return Err(config_error(
            path,
            "value has the wrong type",
            "auto or a non-negative integer",
        ));
    };
    validate_resource_number(number, path, min, max, allow_zero)
}

// Validates resource number against supported rules.
fn validate_resource_number(
    number: i64,
    path: &str,
    min: i64,
    max: i64,
    allow_zero: bool,
) -> anyhow::Result<()> {
    if allow_zero && number == 0 {
        return Ok(());
    }
    if number < min || number > max {
        return Err(config_error(
            path,
            format!("value {number} is out of range"),
            format!("auto or integer {min}-{max}"),
        ));
    }
    Ok(())
}

// Validates provider switch against supported rules.
fn validate_provider_switch(value: &Value, path: &str) -> anyhow::Result<()> {
    allow_object_keys(value, path, &["_comment", "enabled"])?;
    let enabled = optional_child(value, "enabled").ok_or_else(|| {
        config_error(
            &path_join(path, "enabled"),
            "missing required provider switch",
            "true or false",
        )
    })?;
    validate_bool(enabled, &path_join(path, "enabled"))?;
    Ok(())
}

// Validates config value against supported rules.
fn validate_config_value(value: &Value) -> anyhow::Result<()> {
    allow_object_keys(
        value,
        "$",
        &[
            "_comment",
            "providers",
            "alpaca",
            "fred",
            "tax",
            "daemon",
            "api",
            "logging",
            "feeds",
            "scan",
            "backend",
            "resources",
            "auto",
        ],
    )?;
    if let Some(section) = optional_child(value, "providers") {
        allow_object_keys(section, "$.providers", &["_comment", "alpaca", "other"])?;
        if let Some(alpaca) = optional_child(section, "alpaca") {
            validate_provider_switch(alpaca, "$.providers.alpaca")?;
        }
        if let Some(other) = optional_child(section, "other") {
            let object = object_at(other, "$.providers.other")?;
            for (name, provider) in object {
                validate_provider_switch(provider, &format!("$.providers.other.{name}"))?;
            }
        }
    }
    if let Some(section) = optional_child(value, "alpaca") {
        allow_object_keys(section, "$.alpaca", &["_comment", "accounts"])?;
        if let Some(accounts) = optional_child(section, "accounts") {
            let array = accounts.as_array().ok_or_else(|| {
                config_error("$.alpaca.accounts", "value has the wrong type", "an array")
            })?;
            let mut seen_names = BTreeMap::<String, usize>::new();
            for (idx, account) in array.iter().enumerate() {
                let path = format!("$.alpaca.accounts[{idx}]");
                allow_object_keys(
                    account,
                    &path,
                    &[
                        "_comment",
                        "name",
                        "enabled",
                        "auto_trade_enabled",
                        "api_key_id",
                        "secret_key",
                        "account_mode",
                        "data_feed",
                        "trading_base_url",
                        "data_base_url",
                    ],
                )?;
                for key in [
                    "name",
                    "api_key_id",
                    "secret_key",
                    "trading_base_url",
                    "data_base_url",
                ] {
                    if let Some(child) = optional_child(account, key) {
                        validate_string(child, &path_join(&path, key))?;
                    }
                }
                let resolved_name = optional_child(account, "name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("account-{}", idx + 1));
                let normalized_name = resolved_name.to_ascii_lowercase();
                if let Some(first_idx) = seen_names.insert(normalized_name, idx) {
                    return Err(config_error(
                        &path_join(&path, "name"),
                        format!(
                            "duplicate account name '{resolved_name}' within provider 'alpaca'"
                        ),
                        format!("a unique name; first used at $.alpaca.accounts[{first_idx}].name"),
                    ));
                }
                if let Some(child) = optional_child(account, "enabled") {
                    validate_bool(child, &path_join(&path, "enabled"))?;
                }
                if let Some(child) = optional_child(account, "auto_trade_enabled") {
                    validate_bool(child, &path_join(&path, "auto_trade_enabled"))?;
                }
                if let Some(child) = optional_child(account, "account_mode") {
                    validate_enum(
                        child,
                        &path_join(&path, "account_mode"),
                        &["paper", "individual", "live"],
                    )?;
                }
                if let Some(child) = optional_child(account, "data_feed") {
                    validate_enum(
                        child,
                        &path_join(&path, "data_feed"),
                        &["auto", "sip", "iex"],
                    )?;
                }
            }
        }
    }
    if let Some(section) = optional_child(value, "fred") {
        allow_object_keys(section, "$.fred", &["_comment", "api_key"])?;
        if let Some(api_key) = optional_child(section, "api_key") {
            validate_string(api_key, "$.fred.api_key")?;
        }
    }
    if let Some(section) = optional_child(value, "tax") {
        allow_object_keys(
            section,
            "$.tax",
            &[
                "_comment",
                "filing_status",
                "estimated_annual_income",
                "include_paper_accounts_for_estimate",
                "brackets_file",
            ],
        )?;
        if let Some(child) = optional_child(section, "filing_status") {
            validate_enum(
                child,
                "$.tax.filing_status",
                &[
                    "single",
                    "married_filing_jointly",
                    "married_jointly",
                    "mfj",
                    "qualifying_surviving_spouse",
                    "qualifying_survive_spouse",
                    "qss",
                    "married_filing_separately",
                    "married_separately",
                    "mfs",
                    "head_of_household",
                    "hoh",
                ],
            )?;
        }
        if let Some(child) = optional_child(section, "estimated_annual_income") {
            validate_number_range(
                child,
                "$.tax.estimated_annual_income",
                0.0,
                1_000_000_000.0,
                "a number from 0 to 1000000000",
            )?;
        }
        if let Some(child) = optional_child(section, "include_paper_accounts_for_estimate") {
            validate_bool(child, "$.tax.include_paper_accounts_for_estimate")?;
        }
        if let Some(child) = optional_child(section, "brackets_file") {
            validate_string(child, "$.tax.brackets_file")?;
        }
    }
    if let Some(section) = optional_child(value, "daemon") {
        allow_object_keys(
            section,
            "$.daemon",
            &[
                "_comment",
                "enabled",
                "auto_trade_interval_seconds",
                "dashboard_bar_cache_enabled",
                "dashboard_bar_cache_interval_seconds",
                "dashboard_bar_cache_symbols_limit",
                "daily_refresh_enabled",
                "daily_refresh_trigger",
                "daily_refresh_after_close_minutes",
                "daily_refresh_time",
                "daily_refresh_timezone",
                "daily_refresh_days",
                "daily_refresh_quick",
                "daily_refresh_walk_forward_folds",
                "daily_refresh_top_n",
                "daily_refresh_slippage_bps",
                "daily_refresh_sync_orders",
                "daily_refresh_feeds_sync",
                "daily_refresh_feeds_days",
                "pid_file",
                "log_file",
            ],
        )?;
        for key in [
            "enabled",
            "dashboard_bar_cache_enabled",
            "daily_refresh_enabled",
            "daily_refresh_quick",
            "daily_refresh_sync_orders",
            "daily_refresh_feeds_sync",
        ] {
            if let Some(child) = optional_child(section, key) {
                validate_bool(child, &path_join("$.daemon", key))?;
            }
        }
        for (key, min, max) in [
            ("auto_trade_interval_seconds", 10, 300),
            ("dashboard_bar_cache_interval_seconds", 30, 300),
            ("dashboard_bar_cache_symbols_limit", 1, 500),
            ("daily_refresh_after_close_minutes", 0, 360),
            ("daily_refresh_days", 0, 3650),
            ("daily_refresh_walk_forward_folds", 1, 100),
            ("daily_refresh_top_n", 1, 10_000),
            ("daily_refresh_feeds_days", 1, 3650),
        ] {
            if let Some(child) = optional_child(section, key) {
                validate_int_range(
                    child,
                    &path_join("$.daemon", key),
                    min,
                    max,
                    &format!("integer {min}-{max}"),
                )?;
            }
        }
        if let Some(child) = optional_child(section, "daily_refresh_slippage_bps") {
            validate_number_range(
                child,
                "$.daemon.daily_refresh_slippage_bps",
                0.0,
                10_000.0,
                "number 0-10000",
            )?;
        }
        if let Some(child) = optional_child(section, "daily_refresh_trigger") {
            validate_enum(
                child,
                "$.daemon.daily_refresh_trigger",
                &["market_close", "time"],
            )?;
        }
        if let Some(child) = optional_child(section, "daily_refresh_time") {
            validate_time(child, "$.daemon.daily_refresh_time")?;
        }
        for key in ["daily_refresh_timezone", "pid_file", "log_file"] {
            if let Some(child) = optional_child(section, key) {
                validate_string(child, &path_join("$.daemon", key))?;
            }
        }
    }
    if let Some(section) = optional_child(value, "api") {
        allow_object_keys(
            section,
            "$.api",
            &[
                "_comment",
                "enabled",
                "unix",
                "ssl",
                "socket_file",
                "pid_file",
                "log_file",
                "request_timeout_seconds",
                "long_request_timeout_seconds",
                "max_concurrent_requests",
                "max_concurrent_long_requests",
                "rate_limit_per_minute",
                "max_body_bytes",
                "overload_retry_after_seconds",
            ],
        )?;
        if let Some(child) = optional_child(section, "enabled") {
            validate_bool(child, "$.api.enabled")?;
        }
        if let Some(child) = optional_child(section, "unix") {
            allow_object_keys(
                child,
                "$.api.unix",
                &["_comment", "enabled", "socket_file", "pid_file", "log_file"],
            )?;
            if let Some(value) = optional_child(child, "enabled") {
                validate_bool(value, "$.api.unix.enabled")?;
            }
            for key in ["socket_file", "pid_file", "log_file"] {
                if let Some(value) = optional_child(child, key) {
                    validate_string(value, &path_join("$.api.unix", key))?;
                }
            }
        }
        if let Some(child) = optional_child(section, "ssl") {
            allow_object_keys(
                child,
                "$.api.ssl",
                &[
                    "_comment",
                    "enabled",
                    "auth",
                    "ech",
                    "domain",
                    "bind_host",
                    "ipv4_enabled",
                    "ipv6_enabled",
                    "udp_port",
                    "tcp_enabled",
                    "tcp_bind_host",
                    "tcp_port",
                    "tcp_bootstrap_enabled",
                    "tcp_bootstrap_bind_host",
                    "tcp_bootstrap_port",
                    "trusted_proxy_enabled",
                    "trusted_proxy_cidrs",
                    "pid_file",
                    "log_file",
                    "cert_mode",
                    "cert_file",
                    "key_file",
                    "acme_challenge_cert_file",
                    "acme_challenge_key_file",
                    "key_exchange_policy",
                    "_key_exchange_policy_comment",
                    "dns_https_check_required",
                    "tcp_acme_tls_alpn_enabled",
                    "tcp_acme_bind_host",
                    "tcp_acme_port",
                ],
            )?;
            if let Some(ech) = optional_child(child, "ech") {
                allow_object_keys(
                    ech,
                    "$.api.ssl.ech",
                    &[
                        "_comment",
                        "enabled",
                        "public_name",
                        "config_file",
                        "key_file",
                        "require_dns_https_record",
                    ],
                )?;
                for key in ["enabled", "require_dns_https_record"] {
                    if let Some(value) = optional_child(ech, key) {
                        validate_bool(value, &path_join("$.api.ssl.ech", key))?;
                    }
                }
                for key in ["public_name", "config_file", "key_file"] {
                    if let Some(value) = optional_child(ech, key) {
                        validate_string(value, &path_join("$.api.ssl.ech", key))?;
                    }
                }
            }
            if let Some(auth) = optional_child(child, "auth") {
                allow_object_keys(
                    auth,
                    "$.api.ssl.auth",
                    &["_comment", "enabled", "username", "password"],
                )?;
                if let Some(value) = optional_child(auth, "enabled") {
                    validate_bool(value, "$.api.ssl.auth.enabled")?;
                }
                for key in ["username", "password"] {
                    if let Some(value) = optional_child(auth, key) {
                        validate_string(value, &path_join("$.api.ssl.auth", key))?;
                    }
                }
            }
            for key in [
                "enabled",
                "ipv4_enabled",
                "ipv6_enabled",
                "tcp_enabled",
                "tcp_bootstrap_enabled",
                "trusted_proxy_enabled",
                "dns_https_check_required",
                "tcp_acme_tls_alpn_enabled",
            ] {
                if let Some(value) = optional_child(child, key) {
                    validate_bool(value, &path_join("$.api.ssl", key))?;
                }
            }
            for key in [
                "domain",
                "bind_host",
                "tcp_bind_host",
                "tcp_bootstrap_bind_host",
                "pid_file",
                "log_file",
                "cert_file",
                "key_file",
                "acme_challenge_cert_file",
                "acme_challenge_key_file",
                "tcp_acme_bind_host",
            ] {
                if let Some(value) = optional_child(child, key) {
                    validate_string(value, &path_join("$.api.ssl", key))?;
                }
            }
            if let Some(value) = optional_child(child, "cert_mode") {
                validate_enum(
                    value,
                    "$.api.ssl.cert_mode",
                    &["provided", "self_signed", "letsencrypt"],
                )?;
            }
            if let Some(value) = optional_child(child, "key_exchange_policy") {
                validate_enum(
                    value,
                    "$.api.ssl.key_exchange_policy",
                    &["mlkem_secure_fallback", "mlkem_required"],
                )?;
            }
            for key in [
                "udp_port",
                "tcp_port",
                "tcp_bootstrap_port",
                "tcp_acme_port",
            ] {
                if let Some(value) = optional_child(child, key) {
                    validate_int_range(
                        value,
                        &path_join("$.api.ssl", key),
                        1,
                        65_535,
                        "integer 1-65535",
                    )?;
                }
            }
            if let Some(value) = optional_child(child, "trusted_proxy_cidrs") {
                validate_ip_cidr_array(value, "$.api.ssl.trusted_proxy_cidrs")?;
            }
        }
        for key in ["socket_file", "pid_file", "log_file"] {
            if let Some(child) = optional_child(section, key) {
                validate_string(child, &path_join("$.api", key))?;
            }
        }
        for (key, min, max) in [
            ("request_timeout_seconds", 5, 300),
            ("long_request_timeout_seconds", 60, 86_400),
            ("max_concurrent_requests", 1, 128),
            ("max_concurrent_long_requests", 1, 16),
            ("rate_limit_per_minute", 1, 10_000),
            ("max_body_bytes", 1024, 1_048_576),
            ("overload_retry_after_seconds", 1, 300),
        ] {
            if let Some(child) = optional_child(section, key) {
                validate_int_range(
                    child,
                    &path_join("$.api", key),
                    min,
                    max,
                    &format!("integer {min}-{max}"),
                )?;
            }
        }
    }
    if let Some(section) = optional_child(value, "logging") {
        allow_object_keys(
            section,
            "$.logging",
            &[
                "_comment",
                "data_log_file",
                "ml_log_file",
                "training_log_file",
                "feeds_log_file",
            ],
        )?;
        for key in [
            "data_log_file",
            "ml_log_file",
            "training_log_file",
            "feeds_log_file",
        ] {
            if let Some(child) = optional_child(section, key) {
                validate_string(child, &path_join("$.logging", key))?;
            }
        }
    }
    if let Some(section) = optional_child(value, "feeds") {
        allow_object_keys(
            section,
            "$.feeds",
            &[
                "_comment",
                "sync_before_training",
                "sync_orders_before_training",
                "compute_correlations_before_training",
                "include_current_sp500",
                "include_open_positions",
                "include_bought_symbols",
                "bought_symbol_lookback_days",
                "include_q1_candidates",
                "q1_top_n",
                "sync_days",
                "source_timeout_seconds",
                "source_retry_count",
                "auto_tune_sources",
                "alpaca_concurrency",
                "sec_edgar_concurrency",
                "yahoo_rss_concurrency",
                "google_rss_concurrency",
                "correlation_days",
                "correlation_min_overlap_days",
                "correlation_strong_threshold",
                "correlation_max_symbols",
                "extra_symbols",
            ],
        )?;
        for key in [
            "sync_before_training",
            "sync_orders_before_training",
            "compute_correlations_before_training",
            "include_current_sp500",
            "include_open_positions",
            "include_bought_symbols",
            "include_q1_candidates",
            "auto_tune_sources",
        ] {
            if let Some(child) = optional_child(section, key) {
                validate_bool(child, &path_join("$.feeds", key))?;
            }
        }
        for (key, min, max) in [
            ("bought_symbol_lookback_days", 1, 3650),
            ("q1_top_n", 1, 50_000),
            ("sync_days", 1, 3650),
            ("source_timeout_seconds", 5, 120),
            ("source_retry_count", 0, 10),
            ("alpaca_concurrency", 1, 16),
            ("sec_edgar_concurrency", 1, 4),
            ("yahoo_rss_concurrency", 1, 16),
            ("google_rss_concurrency", 1, 16),
            ("correlation_days", 10, 252),
            ("correlation_min_overlap_days", 10, 252),
            ("correlation_max_symbols", 2, 5000),
        ] {
            if let Some(child) = optional_child(section, key) {
                validate_int_range(
                    child,
                    &path_join("$.feeds", key),
                    min,
                    max,
                    &format!("integer {min}-{max}"),
                )?;
            }
        }
        if let Some(child) = optional_child(section, "correlation_strong_threshold") {
            validate_number_range(
                child,
                "$.feeds.correlation_strong_threshold",
                0.1,
                0.99,
                "number 0.1-0.99",
            )?;
        }
        if let Some(child) = optional_child(section, "extra_symbols") {
            validate_string_array(child, "$.feeds.extra_symbols")?;
        }
    }
    if let Some(section) = optional_child(value, "scan") {
        allow_object_keys(
            section,
            "$.scan",
            &["_comment", "max_concurrent", "max_retries"],
        )?;
        for (key, min, max) in [("max_concurrent", 1, 128), ("max_retries", 0, 100)] {
            if let Some(child) = optional_child(section, key) {
                validate_int_range(
                    child,
                    &path_join("$.scan", key),
                    min,
                    max,
                    &format!("integer {min}-{max}"),
                )?;
            }
        }
    }
    if let Some(section) = optional_child(value, "backend") {
        allow_object_keys(
            section,
            "$.backend",
            &["_comment", "lstm", "xgboost", "lightgbm", "ridge"],
        )?;
        if let Some(child) = optional_child(section, "lstm") {
            validate_enum(child, "$.backend.lstm", &["auto", "cpu", "mlx", "tch"])?;
        }
        if let Some(child) = optional_child(section, "xgboost") {
            validate_enum(child, "$.backend.xgboost", &["auto", "cpu", "cuda"])?;
        }
        if let Some(child) = optional_child(section, "lightgbm") {
            validate_enum(child, "$.backend.lightgbm", &["auto", "cpu", "cuda"])?;
        }
        if let Some(child) = optional_child(section, "ridge") {
            validate_enum(child, "$.backend.ridge", &["cpu"])?;
        }
    }
    if let Some(section) = optional_child(value, "resources") {
        allow_object_keys(
            section,
            "$.resources",
            &[
                "_comment",
                "memory_budget_percent",
                "cpu_budget_percent",
                "sqlite_cache_mb",
                "sqlite_temp_store",
                "sqlite_mmap_mb",
                "ml_symbol_batch_size",
                "lstm_max_sequences",
                "lstm_batch_size",
                "lightgbm_max_train_rows",
                "lightgbm_max_valid_rows",
            ],
        )?;
        if let Some(child) = optional_child(section, "memory_budget_percent") {
            validate_resource_setting(
                child,
                "$.resources.memory_budget_percent",
                10,
                95,
                false,
                false,
            )?;
        }
        if let Some(child) = optional_child(section, "cpu_budget_percent") {
            validate_resource_setting(
                child,
                "$.resources.cpu_budget_percent",
                10,
                100,
                false,
                false,
            )?;
        }
        if let Some(child) = optional_child(section, "sqlite_cache_mb") {
            validate_resource_setting(child, "$.resources.sqlite_cache_mb", 4, 4096, false, false)?;
        }
        if let Some(child) = optional_child(section, "sqlite_temp_store") {
            validate_enum(
                child,
                "$.resources.sqlite_temp_store",
                &["auto", "file", "memory"],
            )?;
        }
        if let Some(child) = optional_child(section, "sqlite_mmap_mb") {
            validate_resource_setting(child, "$.resources.sqlite_mmap_mb", 0, 16_384, true, false)?;
        }
        if let Some(child) = optional_child(section, "ml_symbol_batch_size") {
            validate_resource_setting(
                child,
                "$.resources.ml_symbol_batch_size",
                25,
                5000,
                false,
                false,
            )?;
        }
        if let Some(child) = optional_child(section, "lstm_max_sequences") {
            validate_resource_setting(
                child,
                "$.resources.lstm_max_sequences",
                1_000,
                2_000_000,
                false,
                false,
            )?;
        }
        if let Some(child) = optional_child(section, "lstm_batch_size") {
            validate_resource_setting(child, "$.resources.lstm_batch_size", 8, 2048, false, false)?;
        }
        if let Some(child) = optional_child(section, "lightgbm_max_train_rows") {
            validate_resource_setting(
                child,
                "$.resources.lightgbm_max_train_rows",
                250_000,
                100_000_000,
                true,
                true,
            )?;
        }
        if let Some(child) = optional_child(section, "lightgbm_max_valid_rows") {
            validate_resource_setting(
                child,
                "$.resources.lightgbm_max_valid_rows",
                50_000,
                25_000_000,
                true,
                true,
            )?;
        }
    }
    if let Some(section) = optional_child(value, "auto") {
        allow_object_keys(
            section,
            "$.auto",
            &[
                "_comment",
                "enabled",
                "log_file",
                "market",
                "compliance",
                "stop_loss_confirmation",
                "take_profit_confirmation",
                "max_positions",
                "position_size_pct",
                "stop_loss_pct",
                "take_profit_pct",
                "max_hold_days",
                "min_price",
                "min_avg_volume",
                "max_spread_bps",
                "min_quote_size",
                "allow_bar_price_fallback",
                "bar_fallback_bps",
                "ml_quintile_buy",
                "ml_quintile_exit",
            ],
        )?;
        if let Some(child) = optional_child(section, "enabled") {
            validate_bool(child, "$.auto.enabled")?;
        }
        if let Some(child) = optional_child(section, "log_file") {
            validate_string(child, "$.auto.log_file")?;
        }
        if let Some(market) = optional_child(section, "market") {
            validate_auto_market(market)?;
        }
        if let Some(compliance) = optional_child(section, "compliance") {
            validate_auto_compliance(compliance)?;
        }
        if let Some(child) = optional_child(section, "stop_loss_confirmation") {
            validate_stop_loss_confirmation(child)?;
        }
        if let Some(child) = optional_child(section, "take_profit_confirmation") {
            validate_take_profit_confirmation(child)?;
        }
        for (key, min, max) in [
            ("max_positions", 1, 1000),
            ("max_hold_days", 1, 3650),
            ("min_avg_volume", 0, 10_000_000_000),
            ("ml_quintile_buy", 1, 5),
            ("ml_quintile_exit", 1, 5),
        ] {
            if let Some(child) = optional_child(section, key) {
                validate_int_range(
                    child,
                    &path_join("$.auto", key),
                    min,
                    max,
                    &format!("integer {min}-{max}"),
                )?;
            }
        }
        for (key, min, max) in [
            ("position_size_pct", 0.0, 100.0),
            ("stop_loss_pct", 0.0, 100.0),
            ("take_profit_pct", 0.0, 10_000.0),
            ("min_price", 0.0, 1_000_000.0),
            ("max_spread_bps", 0.0, 10_000.0),
            ("min_quote_size", 0.0, 1_000_000_000.0),
            ("bar_fallback_bps", 0.0, 10_000.0),
        ] {
            if let Some(child) = optional_child(section, key) {
                validate_number_range(
                    child,
                    &path_join("$.auto", key),
                    min,
                    max,
                    &format!("number {min}-{max}"),
                )?;
            }
        }
        if let Some(child) = optional_child(section, "allow_bar_price_fallback") {
            validate_bool(child, "$.auto.allow_bar_price_fallback")?;
        }
    }
    Ok(())
}

// Validates stop-loss confirmation against supported rules.
fn validate_stop_loss_confirmation(value: &Value) -> anyhow::Result<()> {
    allow_object_keys(
        value,
        "$.auto.stop_loss_confirmation",
        &[
            "_comment",
            "enabled",
            "cycles",
            "max_confirmation_minutes",
            "emergency_stop_loss_pct",
        ],
    )?;
    if let Some(child) = optional_child(value, "enabled") {
        validate_bool(child, "$.auto.stop_loss_confirmation.enabled")?;
    }
    if let Some(child) = optional_child(value, "cycles") {
        validate_int_range(
            child,
            "$.auto.stop_loss_confirmation.cycles",
            1,
            60,
            "integer 1-60",
        )?;
    }
    if let Some(child) = optional_child(value, "max_confirmation_minutes") {
        validate_int_range(
            child,
            "$.auto.stop_loss_confirmation.max_confirmation_minutes",
            0,
            390,
            "integer 0-390",
        )?;
    }
    if let Some(child) = optional_child(value, "emergency_stop_loss_pct") {
        validate_number_range(
            child,
            "$.auto.stop_loss_confirmation.emergency_stop_loss_pct",
            0.0,
            100.0,
            "number 0-100",
        )?;
    }
    Ok(())
}

// Validates take-profit confirmation against supported rules.
fn validate_take_profit_confirmation(value: &Value) -> anyhow::Result<()> {
    allow_object_keys(
        value,
        "$.auto.take_profit_confirmation",
        &[
            "_comment",
            "enabled",
            "cycles",
            "min_hold_minutes",
            "trailing_enabled",
            "trailing_giveback_pct",
        ],
    )?;
    if let Some(child) = optional_child(value, "enabled") {
        validate_bool(child, "$.auto.take_profit_confirmation.enabled")?;
    }
    if let Some(child) = optional_child(value, "cycles") {
        validate_int_range(
            child,
            "$.auto.take_profit_confirmation.cycles",
            1,
            60,
            "integer 1-60",
        )?;
    }
    if let Some(child) = optional_child(value, "min_hold_minutes") {
        validate_int_range(
            child,
            "$.auto.take_profit_confirmation.min_hold_minutes",
            0,
            390,
            "integer 0-390",
        )?;
    }
    if let Some(child) = optional_child(value, "trailing_enabled") {
        validate_bool(child, "$.auto.take_profit_confirmation.trailing_enabled")?;
    }
    if let Some(child) = optional_child(value, "trailing_giveback_pct") {
        validate_number_range(
            child,
            "$.auto.take_profit_confirmation.trailing_giveback_pct",
            0.0,
            100.0,
            "number 0-100",
        )?;
    }
    Ok(())
}

// Validates auto market against supported rules.
fn validate_auto_market(value: &Value) -> anyhow::Result<()> {
    allow_object_keys(
        value,
        "$.auto.market",
        &[
            "_comment",
            "mode",
            "require_local_clock",
            "use_provider_clock",
            "use_provider_calendar",
            "allow_local_clock_fallback",
            "timezone",
            "provider_markets",
            "regular_open",
            "regular_close",
            "buy_start",
            "buy_end",
            "sell_start",
            "sell_end",
            "closed_dates",
        ],
    )?;
    if let Some(child) = optional_child(value, "mode") {
        validate_enum(child, "$.auto.market.mode", &["auto", "provider", "local"])?;
    }
    for key in [
        "require_local_clock",
        "use_provider_clock",
        "use_provider_calendar",
        "allow_local_clock_fallback",
    ] {
        if let Some(child) = optional_child(value, key) {
            validate_bool(child, &path_join("$.auto.market", key))?;
        }
    }
    for key in ["timezone"] {
        if let Some(child) = optional_child(value, key) {
            validate_string(child, &path_join("$.auto.market", key))?;
        }
    }
    if let Some(child) = optional_child(value, "provider_markets") {
        validate_string_array(child, "$.auto.market.provider_markets")?;
    }
    if let Some(child) = optional_child(value, "closed_dates") {
        validate_string_array(child, "$.auto.market.closed_dates")?;
    }
    for key in [
        "regular_open",
        "regular_close",
        "buy_start",
        "buy_end",
        "sell_start",
        "sell_end",
    ] {
        if let Some(child) = optional_child(value, key) {
            validate_time(child, &path_join("$.auto.market", key))?;
        }
    }
    Ok(())
}

// Validates auto compliance against supported rules.
fn validate_auto_compliance(value: &Value) -> anyhow::Result<()> {
    allow_object_keys(
        value,
        "$.auto.compliance",
        &[
            "_comment",
            "blocked_symbols",
            "wash_sale_safety_buffer_days",
        ],
    )?;
    if let Some(child) = optional_child(value, "blocked_symbols") {
        validate_string_array(child, "$.auto.compliance.blocked_symbols")?;
    }
    if let Some(child) = optional_child(value, "wash_sale_safety_buffer_days") {
        validate_int_range(
            child,
            "$.auto.compliance.wash_sale_safety_buffer_days",
            1,
            365,
            "integer 1-365",
        )?;
    }
    Ok(())
}

// Builds or returns path configuration state.
pub fn config_path() -> PathBuf {
    paths::config_dir().join("mlai-trade.json")
}

// Builds or returns ML tuning configuration path state.
pub fn ml_tuning_config_path() -> PathBuf {
    paths::config_dir().join("mlai-trade-ml-tuning.json")
}

// Validates one LSTM profile in the ML tuning config file.
fn validate_lstm_profile_config(value: &Value, path: &str) -> anyhow::Result<()> {
    allow_object_keys(
        value,
        path,
        &[
            "_comment",
            "target_mode",
            "direction_threshold",
            "hidden_dim",
            "epochs",
            "learning_rate",
            "loss_function",
            "huber_delta",
            "dropout_rate",
            "weight_decay",
            "early_stopping_enabled",
            "early_stopping_patience",
            "early_stopping_min_delta",
            "early_stopping_sample_size",
        ],
    )?;
    if let Some(child) = optional_child(value, "target_mode") {
        validate_enum(
            child,
            &path_join(path, "target_mode"),
            &["regression", "direction"],
        )?;
    }
    if let Some(child) = optional_child(value, "direction_threshold") {
        validate_number_range(
            child,
            &path_join(path, "direction_threshold"),
            0.0,
            1.0,
            "number 0-1",
        )?;
    }
    for (key, min, max) in [
        ("hidden_dim", 16, 512),
        ("epochs", 1, 200),
        ("early_stopping_patience", 1, 50),
        ("early_stopping_sample_size", 1_000, 1_000_000),
    ] {
        if let Some(child) = optional_child(value, key) {
            validate_int_range(
                child,
                &path_join(path, key),
                min,
                max,
                &format!("integer {min}-{max}"),
            )?;
        }
    }
    if let Some(child) = optional_child(value, "learning_rate") {
        validate_number_range(
            child,
            &path_join(path, "learning_rate"),
            0.000_001,
            0.1,
            "number 0.000001-0.1",
        )?;
    }
    if let Some(child) = optional_child(value, "loss_function") {
        validate_enum(
            child,
            &path_join(path, "loss_function"),
            &["mse", "huber", "l1", "bce"],
        )?;
    }
    if let Some(child) = optional_child(value, "huber_delta") {
        validate_number_range(
            child,
            &path_join(path, "huber_delta"),
            0.000_001,
            1.0,
            "number 0.000001-1",
        )?;
    }
    for key in ["dropout_rate", "weight_decay"] {
        if let Some(child) = optional_child(value, key) {
            validate_number_range(child, &path_join(path, key), 0.0, 1.0, "number 0-1")?;
        }
    }
    if let Some(child) = optional_child(value, "early_stopping_enabled") {
        validate_bool(child, &path_join(path, "early_stopping_enabled"))?;
    }
    if let Some(child) = optional_child(value, "early_stopping_min_delta") {
        validate_number_range(
            child,
            &path_join(path, "early_stopping_min_delta"),
            0.0,
            1.0,
            "number 0-1",
        )?;
    }
    Ok(())
}

// Validates the standalone ML tuning config file.
fn validate_ml_tuning_config_value(value: &Value) -> anyhow::Result<()> {
    allow_object_keys(value, "$", &["_comment", "lstm"])?;
    let Some(lstm) = optional_child(value, "lstm") else {
        return Ok(());
    };
    allow_object_keys(lstm, "$.lstm", &["_comment", "profile", "profiles"])?;
    if let Some(profile) = optional_child(lstm, "profile") {
        validate_enum(profile, "$.lstm.profile", &["auto", "cpu", "mlx", "tch"])?;
    }
    if let Some(profiles) = optional_child(lstm, "profiles") {
        allow_object_keys(
            profiles,
            "$.lstm.profiles",
            &["_comment", "cpu", "mlx", "tch"],
        )?;
        for key in ["cpu", "mlx", "tch"] {
            if let Some(profile) = optional_child(profiles, key) {
                validate_lstm_profile_config(profile, &path_join("$.lstm.profiles", key))?;
            }
        }
    }
    Ok(())
}

// Handles ML tuning config load logic.
pub fn load_ml_tuning() -> anyhow::Result<MlTuningConfig> {
    let path = ml_tuning_config_path();
    if !path.exists() {
        return Ok(MlTuningConfig::default());
    }
    let _ = paths::harden_file_if_exists(&path);
    let content = std::fs::read_to_string(&path)?;
    let value = serde_json::from_str::<Value>(&content).map_err(|err| {
        anyhow::anyhow!(
            "invalid ML tuning config file {}: JSON syntax error at line {}, column {}: {}",
            path.display(),
            err.line(),
            err.column(),
            err
        )
    })?;
    validate_ml_tuning_config_value(&value).map_err(|err| {
        anyhow::anyhow!("invalid ML tuning config file {}: {}", path.display(), err)
    })?;
    serde_json::from_value::<MlTuningConfig>(value).map_err(|err| {
        anyhow::anyhow!(
            "invalid ML tuning config file {}: unable to parse validated config: {}",
            path.display(),
            err
        )
    })
}

// Handles load logic.
pub fn load() -> anyhow::Result<AppConfig> {
    let path = config_path();
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let _ = paths::harden_file_if_exists(&path);
    let content = std::fs::read_to_string(&path)?;
    let value = serde_json::from_str::<Value>(&content).map_err(|err| {
        anyhow::anyhow!(
            "invalid config file {}: JSON syntax error at line {}, column {}: {}",
            path.display(),
            err.line(),
            err.column(),
            err
        )
    })?;
    validate_config_value(&value)
        .map_err(|err| anyhow::anyhow!("invalid config file {}: {}", path.display(), err))?;
    let config = serde_json::from_value::<AppConfig>(value).map_err(|err| {
        anyhow::anyhow!(
            "invalid config file {}: unable to parse validated config: {}",
            path.display(),
            err
        )
    })?;
    Ok(config)
}

// Handles non empty logic.
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

// Returns provider enabled state.
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

// Handles enabled providers logic.
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

// Handles require enabled provider logic.
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

// Handles alpaca data feed logic.
pub fn alpaca_data_feed() -> String {
    if let Ok(account) = alpaca_primary_account() {
        return account.data_feed;
    }
    "auto".to_string()
}

// Handles alpaca provider enabled logic.
fn alpaca_provider_enabled(config: &AppConfig) -> bool {
    config.providers.alpaca.enabled
}

// Normalizes account mode into canonical form.
fn normalize_account_mode(value: Option<String>) -> Option<String> {
    value.map(|value| match value.to_ascii_lowercase().as_str() {
        "individual" | "live" => "individual".to_string(),
        "paper" => "paper".to_string(),
        other => other.to_string(),
    })
}

// Normalizes data feed into canonical form.
fn normalize_data_feed(value: Option<String>) -> Option<String> {
    value.map(|value| match value.to_ascii_lowercase().as_str() {
        "sip" => "sip".to_string(),
        "iex" => "iex".to_string(),
        "auto" => "auto".to_string(),
        other => other.to_string(),
    })
}

// Resolves alpaca account using config and defaults.
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
    let trading_base_url = non_empty(account.trading_base_url.clone());
    let data_base_url = non_empty(account.data_base_url.clone());
    let auto_trade_enabled = account.auto_trade_enabled.unwrap_or(true);

    match (api_key_id, secret_key) {
        (Some(api_key_id), Some(secret_key)) => Ok(AlpacaAccount {
            name,
            auto_trade_enabled,
            api_key_id,
            secret_key,
            account_mode,
            data_feed,
            trading_base_url,
            data_base_url,
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

// Handles alpaca accounts logic.
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

// Handles alpaca primary account logic.
pub fn alpaca_primary_account() -> anyhow::Result<AlpacaAccount> {
    alpaca_accounts()?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("No enabled Alpaca accounts configured."))
}

// Handles fred api key logic.
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

// Handles tax brackets path data or configuration.
pub fn tax_brackets_path() -> PathBuf {
    let value = load()
        .ok()
        .and_then(|config| non_empty(config.tax.brackets_file));
    paths::path_in_runtime_dir(paths::config_dir(), value, "tax-brackets.json")
}

// Handles scan max concurrent logic.
pub fn scan_max_concurrent(default: usize) -> usize {
    load()
        .ok()
        .and_then(|config| config.scan.max_concurrent)
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

// Handles scan max retries logic.
pub fn scan_max_retries(default: usize) -> usize {
    load()
        .ok()
        .and_then(|config| config.scan.max_retries)
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

// Handles xgboost backend logic.
pub fn xgboost_backend() -> String {
    load()
        .ok()
        .and_then(|config| non_empty(config.backend.xgboost))
        .unwrap_or_else(|| "auto".to_string())
        .to_ascii_lowercase()
}

// Returns LSTM backend runtime settings.
pub fn lstm_backend() -> String {
    load()
        .ok()
        .and_then(|config| non_empty(config.backend.lstm))
        .unwrap_or_else(|| "auto".to_string())
        .to_ascii_lowercase()
}

// Handles lightgbm backend logic.
pub fn lightgbm_backend() -> String {
    load()
        .ok()
        .and_then(|config| non_empty(config.backend.lightgbm))
        .unwrap_or_else(|| "auto".to_string())
        .to_ascii_lowercase()
}

// Handles ridge backend logic.
pub fn ridge_backend() -> String {
    load()
        .ok()
        .and_then(|config| non_empty(config.backend.ridge))
        .unwrap_or_else(|| "cpu".to_string())
        .to_ascii_lowercase()
}

// Handles auto-trading log file state.
pub fn auto_log_file() -> PathBuf {
    let value = load()
        .ok()
        .and_then(|config| non_empty(config.auto.log_file));
    paths::path_in_runtime_dir(paths::logs_dir(), value, "mlai-trade-auto.log")
}

// Handles secret candidate logic.
fn secret_candidate(value: Option<String>) -> Option<String> {
    non_empty(value).filter(|value| value.len() >= 8 && value != "replace_me")
}

// Returns configured secret values with defaults applied.
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

// Redacts configured secrets before logging or output.
pub fn redact_configured_secrets(text: &str) -> String {
    let mut redacted = text.to_string();
    for secret in configured_secret_values() {
        redacted = redacted.replace(&secret, "[REDACTED]");
    }
    redacted
}

// Strips terminal control codes and secrets from captured command output.
pub fn sanitize_logged_command_output(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if matches!(chars.peek(), Some('[')) {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            continue;
        }
        if ch.is_control() && !matches!(ch, '\n' | '\r' | '\t') {
            continue;
        }
        clean.push(ch);
    }
    redact_configured_secrets(&clean)
}

// Handles auto-trading market provider markets state.
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
