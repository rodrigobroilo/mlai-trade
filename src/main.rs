// ══════════════════════════════════════════════════════════════════
// MLAI-TRADE — ML/AI trading CLI (broker modules + shared ML/compliance)
// ══════════════════════════════════════════════════════════════════
//
// ⛔ HARD NOs (non-negotiable):
//   1. NO OPTIONS TRADING — stocks/ETFs only. Do NOT add options.
//   2. CONFIGURED BLOCKED SYMBOLS — user/company restricted list
//   3. NO MARKET MANIPULATION — spoofing, layering, wash trading
//   4. NO INSIDER TRADING — no trades on material non-public info
//   5. NO MARGIN TRADING — cash only, never borrow. Reject buys that would result in negative cash.
//
// ⚠️ TAX RULES (tracked/enforced):
//   1. Wash Sale Rule (§1091) — 30-day statutory window + safety buffer tracked in wash_sale_tracker
//   2. Pattern Day Trader — rolling 5-day window in day_trades table
//   3. Position sizing — max 5% per position, max 20 positions
//
// 📊 STRATEGY CONFIDENCE (academic evidence):
//   🟢 HIGH: Momentum (Jegadeesh-Titman), Volume anomalies, New highs
//   🟡 MEDIUM: Gaps (informational), Low volatility
//   🔴 LOW: MACD crossover (3% success), RSI standalone
//
// References:
//   - docs/IRS_TAX_RULES.md
//   - docs/TRADING_KNOWLEDGE.md
//   - Alpaca official docs: https://docs.alpaca.markets/docs/getting-started
//   - Alpaca API reference: https://docs.alpaca.markets/reference/api-references
//   - Alpaca Market Data API: https://docs.alpaca.markets/docs/about-market-data-api
//
// Function map:
// - parse_cli_or_exit()/command_help_path*(): structured CLI parsing/help.
// - cmd_*(): topic command handlers for trade, market, data, feeds, and status.
// - cmd_daily()/cmd_ml_refresh(): all-in-one non-trading prep pipelines.
// - main(): validates runtime/config, logs command lifecycle, dispatches actions.
// ══════════════════════════════════════════════════════════════════

mod accelerators;
mod alpaca;
mod api;
mod auto;
mod compliance;
mod config;
mod daemon;
mod logging;
mod lstm;
mod ml;
mod paths;
mod process;
mod progress;
mod tax;

use chrono::{Duration, NaiveDate, Utc};
use clap::{error::ErrorKind, CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
use compliance::{
    IRS_WASH_SALE_WINDOW_DAYS, PDT_MIN_EQUITY_DOLLARS_PRE_2026_06_04, PDT_TRADE_LIMIT,
};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE, USER_AGENT};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

const FRED_SERIES_OBSERVATIONS_URL: &str = "https://api.stlouisfed.org/fred/series/observations";
const FRED_SP500_SERIES_ID: &str = "SP500";
const FRED_VIX_SERIES_ID: &str = "VIXCLS";
const DEFAULT_HISTORY_DAYS: u32 = 0;
const BATCH_SIZE: usize = 80;
const MAX_CONCURRENT: usize = 5;
const MARKET_BENCHMARK_SYMBOLS: &[&str] = &[
    "SPY", "QQQ", "XLB", "XLC", "XLE", "XLF", "XLI", "XLK", "XLP", "XLRE", "XLU", "XLV", "XLY",
];
const HISTORY_PROBE_SYMBOLS: &[&str] = &[
    "IBM", "XOM", "GE", "AAPL", "MSFT", "SPY", "DIA", "QQQ", "IWM",
];

// Position sizing limits
const MAX_POSITION_PCT: f64 = 0.05; // 5% of portfolio
const MAX_TOTAL_POSITIONS: usize = 20;
const DEFAULT_MARKET_TIMEZONE: &str = "America/New_York";

// Masks account number for safe display.
fn mask_account_number(value: Option<&str>) -> String {
    let Some(value) = value else {
        return "?".into();
    };
    let len = value.chars().count();
    if len <= 4 {
        return "****".into();
    }
    let suffix: String = value.chars().skip(len - 4).collect();
    format!("****{}", suffix)
}

// Builds client order id values.
fn client_order_id(prefix: &str, side: &str, symbol: &str) -> String {
    format!(
        "plm-{}-{}-{}-{}",
        prefix,
        side,
        symbol,
        Utc::now().timestamp_millis()
    )
}

// Handles account selector tokens matching or metadata.
fn account_selector_tokens(selectors: &[String]) -> Vec<String> {
    selectors
        .iter()
        .flat_map(|value| value.split(','))
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

// Handles account selector matches matching or metadata.
fn account_selector_matches(selector: &str, account: &config::AlpacaAccount) -> bool {
    let account_ref = account.account_ref().to_ascii_lowercase();
    let provider_account = format!("{}:{}", account.provider(), account_ref);
    selector == "all"
        || selector == account_ref
        || selector == provider_account
        || selector
            == format!(
                "{}:{}",
                account.provider(),
                alpaca::account_mode_for(account)
            )
        || selector == alpaca::account_mode_for(account)
        || (selector == "paper" && account.is_paper())
        || (matches!(selector, "real" | "live" | "individual") && !account.is_paper())
}

// Handles selected alpaca accounts logic.
fn selected_alpaca_accounts(
    selectors: &[String],
    default_all: bool,
) -> anyhow::Result<Vec<config::AlpacaAccount>> {
    let accounts = config::alpaca_accounts()?;
    let tokens = account_selector_tokens(selectors);
    if tokens.is_empty() {
        if default_all {
            return Ok(accounts);
        }
        anyhow::bail!(
            "--account is required. Run `mlai-trade trade account` to list account selector IDs."
        );
    }

    let mut selected = Vec::new();
    let mut unmatched = Vec::new();
    for token in &tokens {
        let mut matched = false;
        for account in &accounts {
            if account_selector_matches(token, account)
                && !selected
                    .iter()
                    .any(|seen: &config::AlpacaAccount| seen.account_ref() == account.account_ref())
            {
                selected.push(account.clone());
                matched = true;
            }
        }
        if !matched {
            unmatched.push(token.clone());
        }
    }
    if !unmatched.is_empty() {
        let available = accounts
            .iter()
            .map(|account| format!("{}:{}", account.provider(), account.account_ref()))
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "Unknown account selector(s): {}. Available accounts: {}.",
            unmatched.join(", "),
            available
        );
    }
    if selected.is_empty() {
        anyhow::bail!("No accounts matched the requested selector.");
    }
    Ok(selected)
}

// Handles account label matching or metadata.
fn account_label(account: &config::AlpacaAccount, account_number: Option<&str>) -> String {
    format!(
        "{}:{} [{} / {}] broker {}",
        account.provider(),
        account.account_ref(),
        alpaca::account_mode_for(account),
        if account.is_paper() { "paper" } else { "real" },
        mask_account_number(account_number)
    )
}

// Handles account json metadata matching or metadata.
fn account_json_metadata(
    account: &config::AlpacaAccount,
    account_number: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "provider": account.provider(),
        "account_ref": account.account_ref(),
        "account_mode": alpaca::account_mode_for(account),
        "tax_universe": if account.is_paper() { "paper" } else { "real" },
        "paper_account": account.is_paper(),
        "account_number": mask_account_number(account_number),
        "data_feed": account.data_feed,
    })
}

// Validates equity order against supported rules.
fn validate_equity_order(
    symbol: &str,
    qty: f64,
    order_type: &str,
    tif: &str,
) -> anyhow::Result<(String, String)> {
    if qty <= 0.0 {
        anyhow::bail!("Quantity must be greater than zero.");
    }
    check_blocked(symbol)?;

    let order_type = order_type.to_ascii_lowercase();
    if !matches!(
        order_type.as_str(),
        "market" | "limit" | "stop" | "stop_limit" | "trailing_stop"
    ) {
        anyhow::bail!(
            "Unsupported order type '{}'. Use market, limit, stop, stop_limit, or trailing_stop.",
            order_type
        );
    }

    let tif = tif.to_ascii_lowercase();
    if !matches!(tif.as_str(), "day" | "gtc" | "opg" | "cls" | "ioc" | "fok") {
        anyhow::bail!(
            "Unsupported equities time_in_force '{}'. Use day, gtc, opg, cls, ioc, or fok.",
            tif
        );
    }

    Ok((order_type, tif))
}

// ── Confidence system ────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
enum Confidence {
    High,
    Medium,
    Low,
}

impl Confidence {
    // Handles emoji logic.
    fn emoji(&self) -> &'static str {
        match self {
            Confidence::High => "🟢",
            Confidence::Medium => "🟡",
            Confidence::Low => "🔴",
        }
    }
    // Handles label logic.
    fn label(&self) -> &'static str {
        match self {
            Confidence::High => "HIGH",
            Confidence::Medium => "MED",
            Confidence::Low => "LOW",
        }
    }
}

// Handles signal confidence logic.
fn signal_confidence(signal: &str) -> Confidence {
    let base = signal.split('(').next().unwrap_or(signal);
    match base {
        "NEW_HIGH" | "NEW_LOW" | "BIG_MOVE" | "MOMENTUM_3M" | "MOMENTUM_6M" => Confidence::High,
        "VOL_SPIKE" | "LOW_VOL" | "GAP_UP" | "GAP_DOWN" => Confidence::Medium,
        "RSI_HIGH" | "RSI_LOW" | "MA_CROSS_UP" | "MA_CROSS_DOWN" => Confidence::Low,
        _ => Confidence::Medium,
    }
}

// Handles signal base logic.
fn signal_base(signal: &str) -> &str {
    signal.split('(').next().unwrap_or(signal)
}

// Returns whether blocked is true.
fn is_blocked(symbol: &str) -> bool {
    config::is_blocked_symbol(symbol)
}

// Returns whether like option symbol is true.
fn looks_like_option_symbol(symbol: &str) -> bool {
    let compact = symbol.replace([' ', '-'], "");
    let bytes = compact.as_bytes();
    if bytes.len() < 15 {
        return false;
    }

    for i in 1..bytes.len().saturating_sub(14) {
        let date = &bytes[i..i + 6];
        let cp = bytes[i + 6];
        let strike = &bytes[i + 7..];
        if date.iter().all(u8::is_ascii_digit)
            && matches!(cp, b'C' | b'P')
            && strike.len() >= 8
            && strike.iter().all(u8::is_ascii_digit)
        {
            return true;
        }
    }
    false
}

// Handles check no options logic.
fn check_no_options(symbol: &str) -> anyhow::Result<()> {
    if looks_like_option_symbol(symbol) {
        anyhow::bail!(
            "⛔ Options trading is disabled by hard rule. This client only permits stocks/ETFs."
        );
    }
    Ok(())
}

// Handles check blocked logic.
fn check_blocked(symbol: &str) -> anyhow::Result<()> {
    check_no_options(symbol)?;
    if is_blocked(symbol) {
        anyhow::bail!(
            "⛔ {} is BLOCKED by auto.compliance.blocked_symbols in {}\n   This is a configured hard block. Cannot buy, sell, or trade {}.",
            symbol,
            config::config_path().display(),
            symbol
        );
    }
    Ok(())
}

// Formats money for output.
fn fmt_money(val: f64) -> String {
    if val < 0.0 {
        format!("-${:.2}", val.abs())
    } else {
        format!("${:.2}", val)
    }
}

// Formats money comma for output.
fn fmt_money_comma(val: f64) -> String {
    let s = format!("{:.2}", val.abs());
    let parts: Vec<&str> = s.split('.').collect();
    let int_part = parts[0];
    let dec_part = parts.get(1).unwrap_or(&"00");
    let mut result = String::new();
    for (i, c) in int_part.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    let formatted: String = result.chars().rev().collect();
    if val < 0.0 {
        format!("-${}.{}", formatted, dec_part)
    } else {
        format!("${}.{}", formatted, dec_part)
    }
}

// Returns configured wash sale safety buffer days with defaults applied.
fn configured_wash_sale_safety_buffer_days() -> i64 {
    let configured = config::load()
        .ok()
        .and_then(|config| config.auto.compliance.wash_sale_safety_buffer_days);
    compliance::wash_sale_safety_buffer_days(configured)
}

// Returns configured wash sale forward block days with defaults applied.
fn configured_wash_sale_forward_block_days() -> i64 {
    compliance::wash_sale_forward_block_days(Some(configured_wash_sale_safety_buffer_days()))
}

// Returns configured market timezone name with defaults applied.
fn configured_market_timezone_name() -> String {
    config::load()
        .ok()
        .and_then(|config| config.auto.market.timezone)
        .filter(|timezone| !timezone.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_MARKET_TIMEZONE.to_string())
}

// Handles utc today logic.
fn utc_today() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

// Initializes Rayon global CPU workers from the automatic CPU budget.
fn init_global_cpu_worker_pool() {
    let resources = config::runtime_resources();
    let _ = rayon::ThreadPoolBuilder::new()
        .num_threads(resources.cpu_worker_threads)
        .thread_name(|idx| format!("mlai-trade-cpu-{idx}"))
        .build_global();
}

// ── CLI ──────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "mlai-trade",
    version,
    about = "ML/AI trading CLI with broker modules — shared ML + compliance core",
    after_help = "Command topics:\n  runtime: version, completions\n  daemon: start, stop, restart, reload, status\n  api: Unix-socket API lifecycle and status\n  trade: account, buy, sell, cancel, close, orders, positions\n  market: quote, watch, bars, news, sp500, data-feed, history-start, clock, calendar\n  data: universe, scan, daily, screen, movers, watchlist, suggest, status\n  compliance: wash, pdt, tax\n  feeds: news feed monitoring and sentiment\n  ml: training, validation, prediction, explanation\n  auto: autonomous trading engine\n\nFirst run: mlai-trade ml refresh\nNormal daily prep: mlai-trade data daily\nOptional autocomplete: mlai-trade runtime completions install zsh"
)]
struct Cli {
    /// Output JSON instead of human-readable text
    #[arg(long, global = true, help_heading = "Global Options")]
    json: bool,
    /// Runtime home directory; defaults to ~/mlai-trade
    #[arg(
        long,
        global = true,
        value_name = "DIR",
        help_heading = "Global Options"
    )]
    home: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

// Handles the version CLI action.
fn cmd_version(json: bool) -> anyhow::Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    let db_path = paths::scanner_db_path();
    let model_path = paths::ml_model_path();
    let root_dir = paths::root_dir();
    let data_dir = paths::state_dir();
    let disclaimer = "Educational/research software only. The author is not responsible for trading, tax, financial, legal, or operational outcomes. Use at your own risk.";

    if json {
        print_json_pretty(serde_json::json!({
            "name": "mlai-trade",
            "version": version,
            "account_mode": if alpaca::is_paper() { "paper" } else { "individual" },
            "home": root_dir,
                "data_dir": data_dir,
                "db_dir": paths::db_dir(),
                "config_dir": paths::config_dir(),
                "docs_dir": paths::docs_dir(),
                "logs_dir": paths::logs_dir(),
                "api_dir": paths::api_dir(),
                "tmp_dir": paths::tmp_dir(),
                "database": db_path,
                "lightgbm_model": model_path,
                "disclaimer": disclaimer,
        }))?;
    } else {
        println!("mlai-trade {}", version);
        println!(
            "  Mode:          {}",
            if alpaca::is_paper() {
                "paper"
            } else {
                "individual"
            }
        );
        println!("  Home:          {}", root_dir.display());
        println!("  Data dir:      {}", data_dir.display());
        println!("  DB dir:        {}", paths::db_dir().display());
        println!("  Config dir:    {}", paths::config_dir().display());
        println!("  Docs dir:      {}", paths::docs_dir().display());
        println!("  Logs dir:      {}", paths::logs_dir().display());
        println!("  API dir:       {}", paths::api_dir().display());
        println!("  Tmp dir:       {}", paths::tmp_dir().display());
        println!("  Database:      {}", db_path.display());
        println!("  LightGBM:      {}", model_path.display());
        println!("  Disclaimer:    {}", disclaimer);
    }

    Ok(())
}

// Prints json pretty in human-readable form.
fn print_json_pretty(value: serde_json::Value) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

// Handles shell completion script logic.
fn completion_script(shell: Shell) -> anyhow::Result<String> {
    let mut cmd = Cli::command();
    let mut bytes = Vec::new();
    generate(shell, &mut cmd, "mlai-trade", &mut bytes);
    let script = String::from_utf8(bytes)?;
    if shell == Shell::Zsh {
        Ok(filter_zsh_public_root_completions(script))
    } else {
        Ok(script)
    }
}

// Handles shell completion filter zsh public root completions logic.
fn filter_zsh_public_root_completions(script: String) -> String {
    const TARGETS: &[&str] = &[
        "_mlai-trade_commands() {",
        "_mlai-trade__subcmd__help_commands() {",
    ];
    const PUBLIC_COMMANDS: &[&str] = &[
        "'runtime:Runtime utilities\\: version and shell completions' \\",
        "'daemon:Daemon lifecycle and status' \\",
        "'api:Unix-socket API lifecycle and status' \\",
        "'trade:Trading\\: accounts, orders, positions, buy/sell/cancel/close' \\",
        "'market:Market data\\: quotes, bars, news, clocks, calendars, feeds' \\",
        "'data:Data pipeline\\: universe, scanner, daily prep, watchlists' \\",
        "'compliance:Compliance and taxes\\: wash-sale, PDT, federal tax estimates' \\",
        "'feeds:News feed monitoring, sentiment & company relationships' \\",
        "'ml:Machine Learning stock ranker pipeline' \\",
        "'auto:Autonomous trading engine (ML + scanner signals)' \\",
        "'help:Print this message or the help of the given subcommand(s)' \\",
    ];

    let lines = script.lines().collect::<Vec<_>>();
    let mut filtered = Vec::with_capacity(lines.len());
    let mut idx = 0;
    while idx < lines.len() {
        let line = lines[idx];
        filtered.push(line.to_string());
        idx += 1;

        if !TARGETS.iter().any(|target| line.trim() == *target) {
            continue;
        }
        if idx >= lines.len() || !lines[idx].contains("local commands; commands=(") {
            continue;
        }

        filtered.push(lines[idx].to_string());
        idx += 1;
        while idx < lines.len() && lines[idx].trim() != ")" {
            idx += 1;
        }
        for command in PUBLIC_COMMANDS {
            filtered.push(format!("{command}"));
        }
        if idx < lines.len() {
            filtered.push(lines[idx].to_string());
            idx += 1;
        }
    }

    let mut output = filtered.join("\n");
    output.push('\n');
    output
}

// Returns the runtime path for completion path.
fn completion_path(shell: Shell) -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| {
        anyhow::anyhow!("unable to determine home directory for completion install")
    })?;
    let path = match shell {
        Shell::Zsh => home.join(".zsh").join("completions").join("_mlai-trade"),
        Shell::Bash => home
            .join(".local")
            .join("share")
            .join("bash-completion")
            .join("completions")
            .join("mlai-trade"),
        Shell::Fish => home
            .join(".config")
            .join("fish")
            .join("completions")
            .join("mlai-trade.fish"),
        Shell::PowerShell => home
            .join(".config")
            .join("powershell")
            .join("Completions")
            .join("_mlai-trade.ps1"),
        Shell::Elvish => home
            .join(".config")
            .join("elvish")
            .join("completions")
            .join("mlai-trade.elv"),
        _ => anyhow::bail!("unsupported completion shell: {}", shell),
    };
    Ok(path)
}

// Returns the runtime path for zshrc path.
fn zshrc_path() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| {
        anyhow::anyhow!("unable to determine home directory for zsh completion setup")
    })?;
    Ok(home.join(".zshrc"))
}

// Handles shell completion zshrc has completion setup logic.
fn zshrc_has_completion_setup(content: &str) -> bool {
    let mut has_fpath = false;
    let mut has_autoload = false;
    let mut has_compinit = false;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.contains("fpath")
            && (line.contains(".zsh/completions") || line.contains("$HOME/.zsh/completions"))
        {
            has_fpath = true;
        }
        if line.contains("autoload") && line.contains("compinit") {
            has_autoload = true;
        }
        if line == "compinit" || line.starts_with("compinit ") {
            has_compinit = true;
        }
    }

    has_fpath && has_autoload && has_compinit
}

// Ensures zsh completion setup exists or meets required invariants.
fn ensure_zsh_completion_setup() -> anyhow::Result<(PathBuf, bool)> {
    let path = zshrc_path()?;
    let mut content = fs::read_to_string(&path).unwrap_or_default();
    if zshrc_has_completion_setup(&content) {
        return Ok((path, false));
    }

    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    if !content.is_empty() {
        content.push('\n');
    }
    content.push_str("# mlai-trade shell completions\n");
    content.push_str("fpath=(~/.zsh/completions $fpath)\n");
    content.push_str("autoload -Uz compinit\n");
    content.push_str("compinit\n");
    fs::write(&path, content)?;
    Ok((path, true))
}

// Handles the completions CLI action.
fn cmd_completions(action: CompletionAction, json: bool) -> anyhow::Result<()> {
    match action {
        CompletionAction::Generate { shell } => {
            print!("{}", completion_script(shell)?);
            Ok(())
        }
        CompletionAction::Install { shell } => {
            let path = completion_path(shell)?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, completion_script(shell)?)?;
            let zshrc = if shell == Shell::Zsh {
                Some(ensure_zsh_completion_setup()?)
            } else {
                None
            };
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "installed",
                        "shell": shell.to_string(),
                        "path": path,
                        "zshrc_path": zshrc.as_ref().map(|(path, _)| path),
                        "zshrc_updated": zshrc.as_ref().map(|(_, updated)| *updated),
                    })
                );
            } else {
                println!("Installed {} completions: {}", shell, path.display());
                if let Some((path, updated)) = zshrc {
                    if updated {
                        println!("Updated zsh startup file: {}", path.display());
                    } else {
                        println!("zsh startup file already configured: {}", path.display());
                    }
                    println!("Open a new shell or run: source ~/.zshrc");
                }
            }
            Ok(())
        }
        CompletionAction::Uninstall { shell } => {
            let path = completion_path(shell)?;
            let existed = path.exists();
            if existed {
                fs::remove_file(&path)?;
            }
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": if existed { "uninstalled" } else { "not_found" },
                        "shell": shell.to_string(),
                        "path": path,
                    })
                );
            } else if existed {
                println!("Removed {} completions: {}", shell, path.display());
            } else {
                println!(
                    "No installed {} completions found at {}",
                    shell,
                    path.display()
                );
            }
            Ok(())
        }
    }
}

#[derive(Subcommand)]
enum Commands {
    /// Runtime utilities: version and shell completions
    #[command(arg_required_else_help = true)]
    Runtime {
        #[command(subcommand)]
        action: RuntimeAction,
    },
    /// Daemon lifecycle and status
    #[command(arg_required_else_help = true)]
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Unix-socket API lifecycle and status
    #[command(arg_required_else_help = true)]
    Api {
        #[command(subcommand)]
        action: ApiAction,
    },
    /// Trading: accounts, orders, positions, buy/sell/cancel/close
    #[command(arg_required_else_help = true)]
    Trade {
        #[command(subcommand)]
        action: TradeAction,
    },
    /// Market data: quotes, bars, news, clocks, calendars, feeds
    #[command(arg_required_else_help = true)]
    Market {
        #[command(subcommand)]
        action: MarketAction,
    },
    /// Data pipeline: universe, scanner, daily prep, watchlists
    #[command(arg_required_else_help = true)]
    Data {
        #[command(subcommand)]
        action: DataAction,
    },
    /// Compliance and taxes: wash-sale, PDT, federal tax estimates
    #[command(arg_required_else_help = true)]
    Compliance {
        #[command(subcommand)]
        action: ComplianceAction,
    },

    /// Generate, install, or uninstall shell autocomplete
    #[command(hide = true, arg_required_else_help = true)]
    Completions {
        #[command(subcommand)]
        action: CompletionAction,
    },
    /// Show binary version and runtime paths
    #[command(hide = true)]
    Version,
    /// Internal daemon loop entrypoint
    #[command(hide = true)]
    DaemonRun,
    /// Internal API server entrypoint
    #[command(hide = true)]
    ApiRun,
    /// Start the mlai-trade daemon
    #[command(hide = true)]
    Start,
    /// Stop the mlai-trade daemon
    #[command(hide = true)]
    Stop,
    /// Restart the mlai-trade daemon
    #[command(hide = true)]
    Restart,
    /// Reload daemon configuration
    #[command(hide = true)]
    Reload,

    /// Show account status and balances
    #[command(hide = true)]
    Account {
        /// Account selector ID from `mlai-trade trade account`; repeat or comma-separate. Defaults to all accounts.
        #[arg(long = "account", value_delimiter = ',')]
        accounts: Vec<String>,
    },
    /// Place a buy order (compliance checks enforced)
    #[command(hide = true)]
    Buy {
        symbol: String,
        qty: f64,
        /// Required account selector ID from `mlai-trade trade account`; repeat or comma-separate.
        #[arg(long = "account", value_delimiter = ',', required = true)]
        accounts: Vec<String>,
        #[arg(long, default_value = "market")]
        r#type: String,
        #[arg(long)]
        limit_price: Option<f64>,
        #[arg(long)]
        stop_price: Option<f64>,
        #[arg(long, default_value = "day")]
        tif: String,
    },
    /// Cancel an order (or 'all')
    #[command(hide = true)]
    Cancel {
        order_id: String,
        /// Required account selector ID from `mlai-trade trade account`; repeat or comma-separate.
        #[arg(long = "account", value_delimiter = ',', required = true)]
        accounts: Vec<String>,
    },
    /// Close a position (or 'all')
    #[command(hide = true)]
    Close {
        symbol: String,
        /// Required account selector ID from `mlai-trade trade account`; repeat or comma-separate.
        #[arg(long = "account", value_delimiter = ',', required = true)]
        accounts: Vec<String>,
    },
    /// List recent orders
    #[command(hide = true)]
    Orders {
        /// Account selector ID from `mlai-trade trade account`; repeat or comma-separate. Defaults to all accounts.
        #[arg(long = "account", value_delimiter = ',')]
        accounts: Vec<String>,
        #[arg(long, default_value = "all")]
        status: String,
        #[arg(long, default_value = "10")]
        limit: u32,
        /// Sync provider orders/fills before listing
        #[arg(long)]
        sync: bool,
    },
    /// List open positions
    #[command(hide = true)]
    Positions {
        /// Account selector ID from `mlai-trade trade account`; repeat or comma-separate. Defaults to all accounts.
        #[arg(long = "account", value_delimiter = ',')]
        accounts: Vec<String>,
    },
    /// Place a sell order (PDT + wash sale tracking)
    #[command(hide = true)]
    Sell {
        symbol: String,
        qty: f64,
        /// Required account selector ID from `mlai-trade trade account`; repeat or comma-separate.
        #[arg(long = "account", value_delimiter = ',', required = true)]
        accounts: Vec<String>,
        #[arg(long, default_value = "market")]
        r#type: String,
        #[arg(long)]
        limit_price: Option<f64>,
        #[arg(long)]
        stop_price: Option<f64>,
        #[arg(long, default_value = "day")]
        tif: String,
    },

    /// Explain Alpaca SIP/IEX data feeds and the active feed mode
    #[command(hide = true)]
    DataFeed,
    /// Get latest quote for a symbol
    #[command(hide = true)]
    Quote { symbol: String },
    /// Snapshot of multiple symbols
    #[command(hide = true)]
    Watch { symbols: Vec<String> },
    /// Get price bars (OHLCV) for a single symbol
    #[command(hide = true)]
    Bars {
        symbol: String,
        #[arg(long, short, default_value = "1Day")]
        timeframe: String,
        #[arg(long, short, default_value = "10")]
        limit: u32,
    },
    /// Get recent news
    #[command(hide = true)]
    News {
        symbol: Option<String>,
        #[arg(long, default_value = "5")]
        limit: u32,
    },
    /// Sync market benchmark observations from FRED
    #[command(hide = true)]
    Sp500 {
        /// Days to sync; 0 means full available history
        #[arg(long, default_value_t = DEFAULT_HISTORY_DAYS)]
        days: u32,
    },
    /// Probe Alpaca stock data feeds for the earliest available daily bar
    #[command(hide = true)]
    HistoryStart {
        /// Optional symbols to probe. Defaults to long-lived liquid symbols.
        symbols: Vec<String>,
    },
    /// Show market clock status
    #[command(hide = true)]
    Clock,
    /// Show provider market calendar sessions
    #[command(hide = true)]
    Calendar {
        /// Start date, YYYY-MM-DD. Defaults to today in auto.market.timezone.
        #[arg(long)]
        start: Option<String>,
        /// End date, YYYY-MM-DD. Defaults to start date.
        #[arg(long)]
        end: Option<String>,
        /// Provider market acronym, repeatable. Defaults to auto.market.provider_markets.
        #[arg(long = "market")]
        markets: Vec<String>,
    },

    /// Pull all tradable US equities into the database
    #[command(hide = true)]
    Universe,
    /// Bulk download daily bars for the entire universe
    #[command(hide = true)]
    Scan {
        /// Days to sync; 0 means discover and use Alpaca's first available stock bar date
        #[arg(long, default_value_t = DEFAULT_HISTORY_DAYS)]
        days: u32,
        /// Ignore local coverage and re-request the full window
        #[arg(long)]
        force: bool,
    },
    /// Non-trading daily refresh: sync missing data, train/evaluate all ML models, refresh predictions/ensemble
    #[command(hide = true)]
    Daily {
        /// Days to sync; 0 means discover and use Alpaca's first available stock bar date
        #[arg(long, default_value_t = DEFAULT_HISTORY_DAYS)]
        days: u32,
        /// Skip all model training/evaluation/prediction refresh after refreshing data
        #[arg(long)]
        skip_train: bool,
        /// Use fewer model rounds/epochs for faster daily validation
        #[arg(long)]
        quick: bool,
        /// LSTM backend to use when training: auto, cpu, mlx, or tch
        #[arg(long, default_value = "auto")]
        backend: lstm::LstmBackend,
        /// Number of walk-forward validation years
        #[arg(long, default_value = "5")]
        walk_forward_folds: usize,
        /// Number of top-ranked symbols per validation date for trading metrics
        #[arg(long, default_value = "20")]
        top_n: usize,
        /// Round-trip spread/slippage cost in basis points for trading metrics
        #[arg(long, default_value = "50")]
        slippage_bps: f64,
    },
    /// Run screening filters with confidence ratings
    #[command(hide = true)]
    Screen {
        #[arg(long, default_value = "500000")]
        min_volume: u64,
    },
    /// Top movers from Alpaca screener
    #[command(hide = true)]
    Movers,
    /// Show latest screen results with confidence tags
    #[command(hide = true)]
    Watchlist,
    /// Show top buy suggestions (evidence-based scoring)
    #[command(hide = true)]
    Suggest,
    /// DB stats and system status
    #[command(hide = true)]
    Status,

    /// Show active wash sale windows (IRS §1091)
    #[command(hide = true)]
    Wash,
    /// Show Pattern Day Trader status
    #[command(hide = true)]
    Pdt,
    /// Show federal tax estimate for realized trading gains/losses
    #[command(
        hide = true,
        after_help = "Examples:\n  mlai-trade compliance tax --accounts\n  mlai-trade compliance tax --year 2026\n  mlai-trade compliance tax --year 2026 --account alpaca:paper-main\n  mlai-trade compliance tax --year 2026 --account alpaca:paper-main --details\n  mlai-trade compliance tax --show-brackets --year 2026"
    )]
    Tax {
        /// List tax-visible accounts and selectors
        #[arg(long = "accounts")]
        accounts_list: bool,
        /// Account selector ID from `mlai-trade trade account`; repeat or comma-separate. Defaults to all real accounts.
        #[arg(long = "account", value_delimiter = ',')]
        accounts: Vec<String>,
        /// Show per-operation tax details
        #[arg(long)]
        details: bool,
        /// Show the tax estimate
        #[arg(long)]
        show: bool,
        /// Show built-in IRS brackets and configured income rates
        #[arg(long = "show-brackets")]
        show_brackets: bool,
        /// Tax year to estimate
        #[arg(long)]
        year: Option<i32>,
        /// Calendar quarter(s) to estimate: 1, 1,2, 1-4, or omitted for year-to-date
        #[arg(long)]
        quarter: Option<String>,
        /// Export format. Supported: csv
        #[arg(long)]
        export: Option<String>,
    },

    #[command(next_help_heading = "News Feeds")]
    /// News feed monitoring, sentiment & company relationships
    #[command(arg_required_else_help = true)]
    Feeds {
        #[command(subcommand)]
        action: FeedsAction,
    },

    #[command(next_help_heading = "Machine Learning")]
    /// Machine Learning stock ranker pipeline
    #[command(arg_required_else_help = true)]
    Ml {
        #[command(subcommand)]
        action: MlAction,
    },

    #[command(next_help_heading = "Automation")]
    /// Autonomous trading engine (ML + scanner signals)
    #[command(arg_required_else_help = true)]
    Auto {
        #[command(subcommand)]
        action: AutoAction,
    },
}

#[derive(Subcommand)]
enum RuntimeAction {
    /// Show binary version and runtime paths
    Version,
    /// Generate, install, or uninstall shell autocomplete
    #[command(arg_required_else_help = true)]
    Completions {
        #[command(subcommand)]
        action: CompletionAction,
    },
}

#[derive(Subcommand)]
enum TradeAction {
    /// Show account status and balances
    Account {
        /// Account selector ID from `mlai-trade trade account`; repeat or comma-separate. Defaults to all accounts.
        #[arg(long = "account", value_delimiter = ',')]
        accounts: Vec<String>,
    },
    /// Place a buy order (compliance checks enforced)
    Buy {
        symbol: String,
        qty: f64,
        /// Required account selector ID from `mlai-trade trade account`; repeat or comma-separate.
        #[arg(long = "account", value_delimiter = ',', required = true)]
        accounts: Vec<String>,
        #[arg(long, default_value = "market")]
        r#type: String,
        #[arg(long)]
        limit_price: Option<f64>,
        #[arg(long)]
        stop_price: Option<f64>,
        #[arg(long, default_value = "day")]
        tif: String,
    },
    /// Cancel an order (or 'all')
    Cancel {
        order_id: String,
        /// Required account selector ID from `mlai-trade trade account`; repeat or comma-separate.
        #[arg(long = "account", value_delimiter = ',', required = true)]
        accounts: Vec<String>,
    },
    /// Close a position (or 'all')
    Close {
        symbol: String,
        /// Required account selector ID from `mlai-trade trade account`; repeat or comma-separate.
        #[arg(long = "account", value_delimiter = ',', required = true)]
        accounts: Vec<String>,
    },
    /// List recent orders
    Orders {
        /// Account selector ID from `mlai-trade trade account`; repeat or comma-separate. Defaults to all accounts.
        #[arg(long = "account", value_delimiter = ',')]
        accounts: Vec<String>,
        #[arg(long, default_value = "all")]
        status: String,
        #[arg(long, default_value = "10")]
        limit: u32,
        /// Sync provider orders/fills before listing
        #[arg(long)]
        sync: bool,
    },
    /// List open positions
    Positions {
        /// Account selector ID from `mlai-trade trade account`; repeat or comma-separate. Defaults to all accounts.
        #[arg(long = "account", value_delimiter = ',')]
        accounts: Vec<String>,
    },
    /// Place a sell order (PDT + wash sale tracking)
    Sell {
        symbol: String,
        qty: f64,
        /// Required account selector ID from `mlai-trade trade account`; repeat or comma-separate.
        #[arg(long = "account", value_delimiter = ',', required = true)]
        accounts: Vec<String>,
        #[arg(long, default_value = "market")]
        r#type: String,
        #[arg(long)]
        limit_price: Option<f64>,
        #[arg(long)]
        stop_price: Option<f64>,
        #[arg(long, default_value = "day")]
        tif: String,
    },
}

#[derive(Subcommand)]
enum MarketAction {
    /// Explain Alpaca SIP/IEX data feeds and the active feed mode
    DataFeed,
    /// Get latest quote for a symbol
    Quote { symbol: String },
    /// Snapshot of multiple symbols
    Watch { symbols: Vec<String> },
    /// Get price bars (OHLCV) for a single symbol
    Bars {
        symbol: String,
        #[arg(long, short, default_value = "1Day")]
        timeframe: String,
        #[arg(long, short, default_value = "10")]
        limit: u32,
    },
    /// Get recent news
    News {
        symbol: Option<String>,
        #[arg(long, default_value = "5")]
        limit: u32,
    },
    /// Sync market benchmark observations from FRED
    Sp500 {
        /// Days to sync; 0 means full available history
        #[arg(long, default_value_t = DEFAULT_HISTORY_DAYS)]
        days: u32,
    },
    /// Probe Alpaca stock data feeds for the earliest available daily bar
    HistoryStart {
        /// Optional symbols to probe. Defaults to long-lived liquid symbols.
        symbols: Vec<String>,
    },
    /// Show market clock status
    Clock,
    /// Show provider market calendar sessions
    Calendar {
        /// Start date, YYYY-MM-DD. Defaults to today in auto.market.timezone.
        #[arg(long)]
        start: Option<String>,
        /// End date, YYYY-MM-DD. Defaults to start date.
        #[arg(long)]
        end: Option<String>,
        /// Provider market acronym, repeatable. Defaults to auto.market.provider_markets.
        #[arg(long = "market")]
        markets: Vec<String>,
    },
}

#[derive(Subcommand)]
enum DataAction {
    /// Pull all tradable US equities into the database
    Universe,
    /// Bulk download daily bars for the entire universe
    Scan {
        /// Days to sync; 0 means discover and use Alpaca's first available stock bar date
        #[arg(long, default_value_t = DEFAULT_HISTORY_DAYS)]
        days: u32,
        /// Ignore local coverage and re-request the full window
        #[arg(long)]
        force: bool,
    },
    /// Full incremental non-trading prep; same ML path as `ml refresh` unless --skip-train is used
    Daily {
        /// Days to sync; 0 means discover and use Alpaca's first available stock bar date
        #[arg(long, default_value_t = DEFAULT_HISTORY_DAYS)]
        days: u32,
        /// Skip all model training/evaluation/prediction refresh after refreshing data
        #[arg(long)]
        skip_train: bool,
        /// Use fewer model rounds/epochs for faster daily validation
        #[arg(long)]
        quick: bool,
        /// LSTM backend to use when training: auto, cpu, mlx, or tch
        #[arg(long, default_value = "auto")]
        backend: lstm::LstmBackend,
        /// Number of walk-forward validation years
        #[arg(long, default_value = "5")]
        walk_forward_folds: usize,
        /// Number of top-ranked symbols per validation date for trading metrics
        #[arg(long, default_value = "20")]
        top_n: usize,
        /// Round-trip spread/slippage cost in basis points for trading metrics
        #[arg(long, default_value = "50")]
        slippage_bps: f64,
    },
    /// Run screening filters with confidence ratings
    Screen {
        #[arg(long, default_value = "500000")]
        min_volume: u64,
    },
    /// Top movers from Alpaca screener
    Movers,
    /// Show latest screen results with confidence tags
    Watchlist,
    /// Show top buy suggestions (evidence-based scoring)
    Suggest,
    /// DB stats and system status
    Status,
    /// SQLite size, cache, and largest-table breakdown
    DbStats,
    /// Run SQLite maintenance; VACUUM only when explicitly requested
    DbOptimize {
        /// Rebuild the SQLite file to reclaim free pages. Requires extra disk space and can take a long time.
        #[arg(long)]
        vacuum: bool,
    },
}

#[derive(Subcommand)]
enum ComplianceAction {
    /// Show active wash sale windows (IRS §1091)
    Wash,
    /// Show Pattern Day Trader status
    Pdt,
    /// Show federal tax estimate for realized trading gains/losses
    #[command(
        after_help = "Examples:\n  mlai-trade compliance tax --accounts\n  mlai-trade compliance tax --year 2026\n  mlai-trade compliance tax --year 2026 --account alpaca:paper-main\n  mlai-trade compliance tax --year 2026 --account alpaca:paper-main --details\n  mlai-trade compliance tax --show-brackets --year 2026"
    )]
    Tax {
        /// List tax-visible accounts and selectors
        #[arg(long = "accounts")]
        accounts_list: bool,
        /// Account selector ID from `mlai-trade trade account`; repeat or comma-separate. Defaults to all real accounts.
        #[arg(long = "account", value_delimiter = ',')]
        accounts: Vec<String>,
        /// Show per-operation tax details
        #[arg(long)]
        details: bool,
        /// Show the tax estimate
        #[arg(long)]
        show: bool,
        /// Show built-in IRS brackets and configured income rates
        #[arg(long = "show-brackets")]
        show_brackets: bool,
        /// Tax year to estimate
        #[arg(long)]
        year: Option<i32>,
        /// Calendar quarter(s) to estimate: 1, 1,2, 1-4, or omitted for year-to-date
        #[arg(long)]
        quarter: Option<String>,
        /// Export format. Supported: csv
        #[arg(long)]
        export: Option<String>,
    },
}

#[derive(Subcommand)]
enum CompletionAction {
    /// Print completion script to stdout
    Generate {
        /// Shell to generate completions for
        shell: Shell,
    },
    /// Install completion script for the selected shell
    Install {
        /// Shell to install completions for
        shell: Shell,
    },
    /// Remove installed completion script for the selected shell
    Uninstall {
        /// Shell to uninstall completions for
        shell: Shell,
    },
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Reload daemon configuration
    Reload,
    /// Restart the mlai-trade daemon
    Restart,
    /// Start the mlai-trade daemon
    Start,
    /// Show daemon runtime status
    Status {
        /// Include daemon heartbeat and resource usage details
        #[arg(long)]
        details: bool,
    },
    /// Stop the mlai-trade daemon
    Stop,
}

#[derive(Subcommand)]
enum ApiAction {
    /// Reload API server configuration
    Reload,
    /// Restart the API server
    Restart,
    /// Start the API server
    Start,
    /// Show API server runtime status
    Status {
        /// Include API counters and resource usage details
        #[arg(long)]
        details: bool,
    },
    /// Send a local health request through the Unix socket
    Test,
    /// Stop the API server
    Stop,
}

#[derive(Subcommand)]
#[command(
    after_help = "Auto-trade topics:\n  Execution: run, sync-orders\n  Inspection: status, history, config\n  Control: enable, disable"
)]
enum AutoAction {
    #[command(next_help_heading = "Execution")]
    /// Execute one trading cycle (check exits, then entries)
    Run,
    /// Sync provider order/fill history into local DB
    SyncOrders,
    #[command(next_help_heading = "Inspection")]
    /// Show auto-trading status and open positions
    Status,
    /// Show closed trade history
    History {
        #[arg(long, default_value = "20")]
        limit: u32,
    },
    /// Show or set strategy config parameters
    Config {
        key: Option<String>,
        value: Option<String>,
    },
    #[command(next_help_heading = "Control")]
    /// Enable auto-trading
    Enable,
    /// Disable auto-trading (positions remain until manually closed)
    Disable,
}

#[derive(Subcommand)]
#[command(
    after_help = "ML topics:\n  Pipeline: refresh, full-refresh\n  Data preparation: features, labels, export\n  Training & validation: train, baselines, walk-forward, ablate-sp500, xgboost-ablate-sp500\n  LSTM: lstm-train, lstm-predict, lstm-evaluate\n  Prediction & explanation: predict, xgboost-predict, ensemble, ensemble-search, ensemble-default, ensemble-robust-sweep, compare-sp500-final, cache-shap, explain, explained, explainable, status\n\nFirst run or repair: mlai-trade ml refresh\nForce rebuild: mlai-trade ml full-refresh"
)]
enum MlAction {
    #[command(next_help_heading = "Pipeline")]
    /// Incremental non-trading ML refresh: fill missing data, train/evaluate models, refresh predictions
    Refresh {
        /// Days to sync; 0 means discover and use Alpaca's first available stock bar date
        #[arg(long, default_value_t = DEFAULT_HISTORY_DAYS)]
        days: u32,
        /// Use fewer boosting rounds for faster validation
        #[arg(long)]
        quick: bool,
        /// LSTM backend to use: auto, cpu, mlx, or tch
        #[arg(long, default_value = "auto")]
        backend: lstm::LstmBackend,
        /// Number of walk-forward validation years
        #[arg(long, default_value = "5")]
        walk_forward_folds: usize,
        /// Number of top-ranked symbols per validation date for trading metrics
        #[arg(long, default_value = "20")]
        top_n: usize,
        /// Round-trip spread/slippage cost in basis points for trading metrics
        #[arg(long, default_value = "50")]
        slippage_bps: f64,
    },
    /// Full non-trading ML refresh: force-rebuild bars/features/labels/models/predictions
    FullRefresh {
        /// Days to sync; 0 means discover and use Alpaca's first available stock bar date
        #[arg(long, default_value_t = DEFAULT_HISTORY_DAYS)]
        days: u32,
        /// Use fewer boosting rounds for faster validation
        #[arg(long)]
        quick: bool,
        /// LSTM backend to use: auto, cpu, mlx, or tch
        #[arg(long, default_value = "auto")]
        backend: lstm::LstmBackend,
        /// Number of walk-forward validation years
        #[arg(long, default_value = "5")]
        walk_forward_folds: usize,
        /// Number of top-ranked symbols per validation date for trading metrics
        #[arg(long, default_value = "20")]
        top_n: usize,
        /// Round-trip spread/slippage cost in basis points for trading metrics
        #[arg(long, default_value = "50")]
        slippage_bps: f64,
    },
    #[command(next_help_heading = "Data Preparation")]
    /// Compute features from OHLCV bars for all symbols
    Features {
        #[arg(long)]
        symbol: Option<String>,
        /// Recompute existing feature rows, needed after adding new feature columns
        #[arg(long)]
        force: bool,
    },
    /// Compute forward return labels
    Labels {
        #[arg(long, default_value = "5")]
        horizon: u32,
    },
    /// Export features + labels as CSV
    Export {
        #[arg(long, default_value = "csv")]
        format: String,
    },
    #[command(next_help_heading = "Training & Validation")]
    /// Train LightGBM model using the integrated trainer
    Train {
        #[arg(long)]
        quick: bool,
        #[arg(long)]
        backtest_only: bool,
    },
    /// Compare LightGBM validation quality with and without S&P 500 features
    AblateSp500 {
        /// Use fewer boosting rounds for a faster comparison
        #[arg(long)]
        quick: bool,
    },
    /// Train/evaluate XGBoost without S&P 500 features
    XgboostAblateSp500 {
        /// Use fewer boosting rounds for a faster comparison
        #[arg(long)]
        quick: bool,
    },
    /// Train/evaluate non-production baselines for model comparison
    Baselines {
        /// Use fewer rows for faster baseline validation where supported
        #[arg(long)]
        quick: bool,
    },
    /// Run yearly walk-forward validation for return-prediction models
    WalkForward {
        /// Use fewer boosting rounds for faster fold validation
        #[arg(long)]
        quick: bool,
        /// Maximum number of validation years to evaluate
        #[arg(long, default_value = "5")]
        folds: usize,
    },
    /// Run predictions using trained model
    Predict,
    /// Run XGBoost predictions for the latest feature date
    XgboostPredict,
    #[command(next_help_heading = "LSTM")]
    /// Train LSTM model (walk-forward, pure Rust)
    LstmTrain {
        /// Backend to use: auto, cpu, mlx, or tch
        #[arg(long, default_value = "auto")]
        backend: lstm::LstmBackend,
        /// Force single-threaded LSTM training
        #[arg(long)]
        single_thread: bool,
        /// Number of Rayon worker threads for LSTM training
        #[arg(long)]
        threads: Option<usize>,
        /// Train a comparison model with S&P 500 features zeroed out
        #[arg(long)]
        without_sp500: bool,
    },
    /// Run LSTM predictions for latest date
    LstmPredict {
        /// Predict with the comparison model that excludes S&P 500 signal
        #[arg(long)]
        without_sp500: bool,
    },
    /// Evaluate LSTM validation rows and write post-slippage metrics
    LstmEvaluate {
        /// Evaluate the comparison model that excludes S&P 500 signal
        #[arg(long)]
        without_sp500: bool,
        /// Number of top-ranked symbols per validation date for trading metrics
        #[arg(long, default_value = "20")]
        top_n: usize,
        /// Round-trip spread/slippage cost in basis points for trading metrics
        #[arg(long, default_value = "50")]
        slippage_bps: f64,
    },
    #[command(next_help_heading = "Prediction & Explanation")]
    /// Explain a prediction with SHAP values
    Explain {
        /// Stock symbol to explain
        symbol: String,
    },
    /// List latest symbols that have features/predictions and can be explained
    Explainable {
        /// Maximum number of symbols to show
        #[arg(long, default_value = "25")]
        limit: usize,
    },
    /// List symbols/date pairs that already have cached SHAP explanations
    Explained {
        /// Maximum number of cached explanations to show
        #[arg(long, default_value = "50")]
        limit: usize,
    },
    /// Cache SHAP for open positions plus top ensemble candidates
    CacheShap {
        /// Number of top ensemble candidates to cache after open positions
        #[arg(long, default_value = "100")]
        top: usize,
    },
    /// Compute ensemble (LGB + LSTM) predictions
    Ensemble {
        /// LightGBM weight (0.0-1.0)
        #[arg(long, default_value = "0.6")]
        lgb_weight: f64,
        /// LSTM weight (0.0-1.0)
        #[arg(long, default_value = "0.4")]
        lstm_weight: f64,
        /// XGBoost weight (0.0-1.0)
        #[arg(long, default_value = "0.0")]
        xgb_weight: f64,
    },
    /// Search model/ensemble combinations on filtered validation rows and save default weights
    EnsembleSearch {
        /// Number of top-ranked symbols per validation date for trading metrics
        #[arg(long, default_value = "20")]
        top_n: usize,
        /// Round-trip spread/slippage cost in basis points for trading metrics
        #[arg(long, default_value = "50")]
        slippage_bps: f64,
    },
    /// Compute ensemble using saved default weights from ensemble-search
    EnsembleDefault,
    /// Run the full ensemble robustness sweep: grids, objectives, top-N, slippage, feature variants
    EnsembleRobustSweep,
    /// Compare final LightGBM+LSTM ensemble rankings with and without S&P 500 features
    CompareSp500Final {
        #[arg(long, default_value = "0.6")]
        lgb_weight: f64,
        #[arg(long, default_value = "0.4")]
        lstm_weight: f64,
    },
    /// Show ML pipeline status
    Status,
}

#[derive(Subcommand)]
enum FeedsAction {
    /// Subscribe to a stock's news feeds (SEC + RSS + Alpaca)
    Add { symbols: Vec<String> },
    /// Remove a subscription
    Remove { symbol: String },
    /// Poll all subscribed feeds and store articles
    Sync {
        #[arg(long, default_value = "7")]
        days: u32,
    },
    /// List all subscriptions
    List,
    /// Search stored articles
    Search {
        query: String,
        #[arg(long, default_value = "20")]
        limit: u32,
    },
    /// Show company relationship graph for a symbol
    Graph { symbol: String },
    /// Show news sentiment summary for a symbol
    Sentiment { symbol: String },
    /// Compute price correlations for subscribed symbols
    Correlate {
        #[arg(long, default_value = "30")]
        days: u32,
    },
    /// Show feed stats
    Status,
}

// Handles shell completion action name logic.
fn completion_action_name(action: &CompletionAction) -> &'static str {
    match action {
        CompletionAction::Generate { .. } => "generate",
        CompletionAction::Install { .. } => "install",
        CompletionAction::Uninstall { .. } => "uninstall",
    }
}

// Returns the runtime path for runtime action path.
fn runtime_action_path(action: &RuntimeAction) -> Vec<&'static str> {
    match action {
        RuntimeAction::Version => vec!["runtime", "version"],
        RuntimeAction::Completions { action } => {
            vec!["runtime", "completions", completion_action_name(action)]
        }
    }
}

// Handles daemon action name state.
fn daemon_action_name(action: &DaemonAction) -> &'static str {
    match action {
        DaemonAction::Reload => "reload",
        DaemonAction::Restart => "restart",
        DaemonAction::Start => "start",
        DaemonAction::Status { .. } => "status",
        DaemonAction::Stop => "stop",
    }
}

// Runs the api action name API helper.
fn api_action_name(action: &ApiAction) -> &'static str {
    match action {
        ApiAction::Reload => "reload",
        ApiAction::Restart => "restart",
        ApiAction::Start => "start",
        ApiAction::Status { .. } => "status",
        ApiAction::Test => "test",
        ApiAction::Stop => "stop",
    }
}

// Handles trade action name logic.
fn trade_action_name(action: &TradeAction) -> &'static str {
    match action {
        TradeAction::Account { .. } => "account",
        TradeAction::Buy { .. } => "buy",
        TradeAction::Cancel { .. } => "cancel",
        TradeAction::Close { .. } => "close",
        TradeAction::Orders { .. } => "orders",
        TradeAction::Positions { .. } => "positions",
        TradeAction::Sell { .. } => "sell",
    }
}

// Handles market action name logic.
fn market_action_name(action: &MarketAction) -> &'static str {
    match action {
        MarketAction::DataFeed => "data-feed",
        MarketAction::Quote { .. } => "quote",
        MarketAction::Watch { .. } => "watch",
        MarketAction::Bars { .. } => "bars",
        MarketAction::News { .. } => "news",
        MarketAction::Sp500 { .. } => "sp500",
        MarketAction::HistoryStart { .. } => "history-start",
        MarketAction::Clock => "clock",
        MarketAction::Calendar { .. } => "calendar",
    }
}

// Returns Alpaca data-feed action name information.
fn data_action_name(action: &DataAction) -> &'static str {
    match action {
        DataAction::Universe => "universe",
        DataAction::Scan { .. } => "scan",
        DataAction::Daily { .. } => "daily",
        DataAction::Screen { .. } => "screen",
        DataAction::Movers => "movers",
        DataAction::Watchlist => "watchlist",
        DataAction::Suggest => "suggest",
        DataAction::Status => "status",
        DataAction::DbStats => "db-stats",
        DataAction::DbOptimize { .. } => "db-optimize",
    }
}

// Handles compliance action name logic.
fn compliance_action_name(action: &ComplianceAction) -> &'static str {
    match action {
        ComplianceAction::Wash => "wash",
        ComplianceAction::Pdt => "pdt",
        ComplianceAction::Tax { .. } => "tax",
    }
}

// Handles auto-trading action name state.
fn auto_action_name(action: &AutoAction) -> &'static str {
    match action {
        AutoAction::Run => "run",
        AutoAction::SyncOrders => "sync-orders",
        AutoAction::Status => "status",
        AutoAction::History { .. } => "history",
        AutoAction::Config { .. } => "config",
        AutoAction::Enable => "enable",
        AutoAction::Disable => "disable",
    }
}

// Handles ml action name logic.
fn ml_action_name(action: &MlAction) -> &'static str {
    match action {
        MlAction::Refresh { .. } => "refresh",
        MlAction::FullRefresh { .. } => "full-refresh",
        MlAction::Features { .. } => "features",
        MlAction::Labels { .. } => "labels",
        MlAction::Export { .. } => "export",
        MlAction::Train { .. } => "train",
        MlAction::AblateSp500 { .. } => "ablate-sp500",
        MlAction::XgboostAblateSp500 { .. } => "xgboost-ablate-sp500",
        MlAction::Baselines { .. } => "baselines",
        MlAction::WalkForward { .. } => "walk-forward",
        MlAction::Predict => "predict",
        MlAction::XgboostPredict => "xgboost-predict",
        MlAction::LstmTrain { .. } => "lstm-train",
        MlAction::LstmPredict { .. } => "lstm-predict",
        MlAction::LstmEvaluate { .. } => "lstm-evaluate",
        MlAction::Explain { .. } => "explain",
        MlAction::Explainable { .. } => "explainable",
        MlAction::Explained { .. } => "explained",
        MlAction::CacheShap { .. } => "cache-shap",
        MlAction::Ensemble { .. } => "ensemble",
        MlAction::EnsembleSearch { .. } => "ensemble-search",
        MlAction::EnsembleDefault => "ensemble-default",
        MlAction::EnsembleRobustSweep => "ensemble-robust-sweep",
        MlAction::CompareSp500Final { .. } => "compare-sp500-final",
        MlAction::Status => "status",
    }
}

// Handles feeds action name logic.
fn feeds_action_name(action: &FeedsAction) -> &'static str {
    match action {
        FeedsAction::Add { .. } => "add",
        FeedsAction::Remove { .. } => "remove",
        FeedsAction::Sync { .. } => "sync",
        FeedsAction::List => "list",
        FeedsAction::Search { .. } => "search",
        FeedsAction::Graph { .. } => "graph",
        FeedsAction::Sentiment { .. } => "sentiment",
        FeedsAction::Correlate { .. } => "correlate",
        FeedsAction::Status => "status",
    }
}

// Returns the runtime path for command help path.
fn command_help_path(command: &Commands) -> Vec<&'static str> {
    match command {
        Commands::Runtime { action } => runtime_action_path(action),
        Commands::Completions { action } => {
            vec!["runtime", "completions", completion_action_name(action)]
        }
        Commands::Daemon { action } => vec!["daemon", daemon_action_name(action)],
        Commands::Api { action } => vec!["api", api_action_name(action)],
        Commands::Version => vec!["runtime", "version"],
        Commands::DaemonRun => vec!["daemon"],
        Commands::ApiRun => vec!["api"],
        Commands::Start => vec!["daemon", "start"],
        Commands::Stop => vec!["daemon", "stop"],
        Commands::Restart => vec!["daemon", "restart"],
        Commands::Reload => vec!["daemon", "reload"],
        Commands::Trade { action } => vec!["trade", trade_action_name(action)],
        Commands::Market { action } => vec!["market", market_action_name(action)],
        Commands::Data { action } => vec!["data", data_action_name(action)],
        Commands::Compliance { action } => vec!["compliance", compliance_action_name(action)],
        Commands::Account { .. } => vec!["trade", "account"],
        Commands::Buy { .. } => vec!["trade", "buy"],
        Commands::Cancel { .. } => vec!["trade", "cancel"],
        Commands::Close { .. } => vec!["trade", "close"],
        Commands::Orders { .. } => vec!["trade", "orders"],
        Commands::Positions { .. } => vec!["trade", "positions"],
        Commands::Sell { .. } => vec!["trade", "sell"],
        Commands::DataFeed => vec!["market", "data-feed"],
        Commands::Quote { .. } => vec!["market", "quote"],
        Commands::Watch { .. } => vec!["market", "watch"],
        Commands::Bars { .. } => vec!["market", "bars"],
        Commands::News { .. } => vec!["market", "news"],
        Commands::Sp500 { .. } => vec!["market", "sp500"],
        Commands::HistoryStart { .. } => vec!["market", "history-start"],
        Commands::Clock => vec!["market", "clock"],
        Commands::Calendar { .. } => vec!["market", "calendar"],
        Commands::Universe => vec!["data", "universe"],
        Commands::Scan { .. } => vec!["data", "scan"],
        Commands::Daily { .. } => vec!["data", "daily"],
        Commands::Screen { .. } => vec!["data", "screen"],
        Commands::Movers => vec!["data", "movers"],
        Commands::Watchlist => vec!["data", "watchlist"],
        Commands::Suggest => vec!["data", "suggest"],
        Commands::Status => vec!["data", "status"],
        Commands::Wash => vec!["compliance", "wash"],
        Commands::Pdt => vec!["compliance", "pdt"],
        Commands::Tax { .. } => vec!["compliance", "tax"],
        Commands::Feeds { action } => vec!["feeds", feeds_action_name(action)],
        Commands::Ml { action } => vec!["ml", ml_action_name(action)],
        Commands::Auto { action } => vec!["auto", auto_action_name(action)],
    }
}

// Handles push unique component logic.
fn push_unique_component(components: &mut Vec<&'static str>, component: &'static str) {
    if !components.contains(&component) {
        components.push(component);
    }
}

// Handles ml action log components logic.
fn ml_action_log_components(action: &MlAction) -> Vec<&'static str> {
    let mut components = vec!["ml"];
    match action {
        MlAction::Refresh { .. } | MlAction::FullRefresh { .. } => {
            push_unique_component(&mut components, "data");
            push_unique_component(&mut components, "feeds");
            push_unique_component(&mut components, "training");
        }
        MlAction::Features { .. } | MlAction::Labels { .. } | MlAction::Export { .. } => {
            push_unique_component(&mut components, "data");
        }
        MlAction::Train { .. }
        | MlAction::AblateSp500 { .. }
        | MlAction::XgboostAblateSp500 { .. }
        | MlAction::Baselines { .. }
        | MlAction::WalkForward { .. }
        | MlAction::LstmTrain { .. }
        | MlAction::LstmEvaluate { .. }
        | MlAction::EnsembleSearch { .. }
        | MlAction::EnsembleRobustSweep
        | MlAction::CompareSp500Final { .. } => {
            push_unique_component(&mut components, "training");
        }
        _ => {}
    }
    components
}

// Handles CLI command log components routing.
fn command_log_components(command: &Commands) -> Vec<&'static str> {
    let mut components = Vec::new();
    match command {
        Commands::Data {
            action: DataAction::Daily { .. },
        }
        | Commands::Daily { .. } => {
            push_unique_component(&mut components, "data");
            push_unique_component(&mut components, "feeds");
            push_unique_component(&mut components, "ml");
            push_unique_component(&mut components, "training");
        }
        Commands::Data { .. }
        | Commands::Universe
        | Commands::Scan { .. }
        | Commands::Screen { .. }
        | Commands::Movers
        | Commands::Watchlist
        | Commands::Suggest
        | Commands::Status => {
            push_unique_component(&mut components, "data");
        }
        Commands::Market {
            action: MarketAction::Sp500 { .. } | MarketAction::HistoryStart { .. },
        }
        | Commands::Sp500 { .. }
        | Commands::HistoryStart { .. } => {
            push_unique_component(&mut components, "data");
        }
        Commands::Feeds { .. } => {
            push_unique_component(&mut components, "feeds");
        }
        Commands::Ml { action } => {
            for component in ml_action_log_components(action) {
                push_unique_component(&mut components, component);
            }
        }
        _ => {}
    }
    components
}

// Handles CLI command log command event routing.
fn log_command_event(
    components: &[&'static str],
    event: &str,
    command_path: &[&'static str],
    started: Instant,
    error: Option<&str>,
) {
    if components.is_empty() {
        return;
    }
    for component in components {
        let mut payload = serde_json::json!({
            "event": event,
            "level": if error.is_some() { "error" } else { "info" },
            "command": command_path,
            "duration_ms": started.elapsed().as_millis(),
            "source": std::env::var("MLAI_TRADE_API_REQUEST")
                .map(|value| if value == "1" { "api" } else { "cli" })
                .unwrap_or("cli"),
        });
        if let Some(error) = error {
            payload["error"] = serde_json::json!(error);
        }
        logging::append_component_event_lossy(component, payload);
    }
}

// Handles CLI command help command routing.
fn help_command(path: &[&str]) -> String {
    let mut parts = vec!["mlai-trade".to_string()];
    parts.extend(path.iter().map(|part| part.to_string()));
    parts.push("--help".to_string());
    parts.join(" ")
}

// Handles CLI command tokens from args routing.
fn command_tokens_from_args(args: &[OsString]) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut idx = 1;
    while idx < args.len() {
        let arg = args[idx].to_string_lossy();
        if arg == "--" {
            tokens.extend(
                args[idx + 1..]
                    .iter()
                    .map(|arg| arg.to_string_lossy().into_owned()),
            );
            break;
        }
        if arg == "--json" {
            idx += 1;
            continue;
        }
        if arg == "--home" {
            idx += 2;
            continue;
        }
        if arg.starts_with("--home=") {
            idx += 1;
            continue;
        }
        if arg.starts_with('-') {
            idx += 1;
            continue;
        }
        tokens.push(arg.into_owned());
        idx += 1;
    }
    tokens
}

// Handles CLI command canonical top command routing.
fn canonical_top_command(token: &str) -> Option<&'static str> {
    match token {
        "runtime" | "daemon" | "api" | "trade" | "market" | "data" | "compliance" | "feeds"
        | "ml" | "auto" => Some(match token {
            "runtime" => "runtime",
            "daemon" => "daemon",
            "api" => "api",
            "trade" => "trade",
            "market" => "market",
            "data" => "data",
            "compliance" => "compliance",
            "feeds" => "feeds",
            "ml" => "ml",
            "auto" => "auto",
            _ => unreachable!(),
        }),
        _ => None,
    }
}

// Handles CLI command canonical nested command routing.
fn canonical_nested_command(parent: &str, token: &str) -> Option<&'static str> {
    match parent {
        "runtime" => match token {
            "version" => Some("version"),
            "completions" => Some("completions"),
            _ => None,
        },
        "completions" => match token {
            "generate" => Some("generate"),
            "install" => Some("install"),
            "uninstall" => Some("uninstall"),
            _ => None,
        },
        "daemon" => match token {
            "reload" => Some("reload"),
            "restart" => Some("restart"),
            "start" => Some("start"),
            "status" => Some("status"),
            "stop" => Some("stop"),
            _ => None,
        },
        "api" => match token {
            "reload" => Some("reload"),
            "restart" => Some("restart"),
            "start" => Some("start"),
            "status" => Some("status"),
            "test" => Some("test"),
            "stop" => Some("stop"),
            _ => None,
        },
        "trade" => match token {
            "account" => Some("account"),
            "buy" => Some("buy"),
            "cancel" => Some("cancel"),
            "close" => Some("close"),
            "orders" => Some("orders"),
            "positions" => Some("positions"),
            "sell" => Some("sell"),
            _ => None,
        },
        "market" => match token {
            "data-feed" => Some("data-feed"),
            "quote" => Some("quote"),
            "watch" => Some("watch"),
            "bars" => Some("bars"),
            "news" => Some("news"),
            "sp500" => Some("sp500"),
            "history-start" => Some("history-start"),
            "clock" => Some("clock"),
            "calendar" => Some("calendar"),
            _ => None,
        },
        "data" => match token {
            "universe" => Some("universe"),
            "scan" => Some("scan"),
            "daily" => Some("daily"),
            "screen" => Some("screen"),
            "movers" => Some("movers"),
            "watchlist" => Some("watchlist"),
            "suggest" => Some("suggest"),
            "status" => Some("status"),
            _ => None,
        },
        "compliance" => match token {
            "wash" => Some("wash"),
            "pdt" => Some("pdt"),
            "tax" => Some("tax"),
            _ => None,
        },
        "auto" => match token {
            "run" => Some("run"),
            "sync-orders" => Some("sync-orders"),
            "status" => Some("status"),
            "history" => Some("history"),
            "config" => Some("config"),
            "enable" => Some("enable"),
            "disable" => Some("disable"),
            _ => None,
        },
        "ml" => match token {
            "refresh" => Some("refresh"),
            "full-refresh" => Some("full-refresh"),
            "features" => Some("features"),
            "labels" => Some("labels"),
            "export" => Some("export"),
            "train" => Some("train"),
            "ablate-sp500" => Some("ablate-sp500"),
            "xgboost-ablate-sp500" => Some("xgboost-ablate-sp500"),
            "baselines" => Some("baselines"),
            "walk-forward" => Some("walk-forward"),
            "predict" => Some("predict"),
            "xgboost-predict" => Some("xgboost-predict"),
            "lstm-train" => Some("lstm-train"),
            "lstm-predict" => Some("lstm-predict"),
            "lstm-evaluate" => Some("lstm-evaluate"),
            "explain" => Some("explain"),
            "explainable" => Some("explainable"),
            "explained" => Some("explained"),
            "cache-shap" => Some("cache-shap"),
            "ensemble" => Some("ensemble"),
            "ensemble-search" => Some("ensemble-search"),
            "ensemble-default" => Some("ensemble-default"),
            "ensemble-robust-sweep" => Some("ensemble-robust-sweep"),
            "compare-sp500-final" => Some("compare-sp500-final"),
            "status" => Some("status"),
            _ => None,
        },
        "feeds" => match token {
            "add" => Some("add"),
            "remove" => Some("remove"),
            "sync" => Some("sync"),
            "list" => Some("list"),
            "search" => Some("search"),
            "graph" => Some("graph"),
            "sentiment" => Some("sentiment"),
            "correlate" => Some("correlate"),
            "status" => Some("status"),
            _ => None,
        },
        _ => None,
    }
}

// Returns the runtime path for direct alias help path.
fn direct_alias_help_path(tokens: &[String]) -> Option<Vec<&'static str>> {
    let first = tokens.first()?.as_str();
    let path = match first {
        "completions" => {
            let mut path = vec!["runtime", "completions"];
            if let Some(action) = tokens
                .get(1)
                .and_then(|token| canonical_nested_command("completions", token))
            {
                path.push(action);
            }
            path
        }
        "version" => vec!["runtime", "version"],
        "account" => vec!["trade", "account"],
        "buy" => vec!["trade", "buy"],
        "cancel" => vec!["trade", "cancel"],
        "close" => vec!["trade", "close"],
        "orders" => vec!["trade", "orders"],
        "positions" => vec!["trade", "positions"],
        "sell" => vec!["trade", "sell"],
        "data-feed" => vec!["market", "data-feed"],
        "quote" => vec!["market", "quote"],
        "watch" => vec!["market", "watch"],
        "bars" => vec!["market", "bars"],
        "news" => vec!["market", "news"],
        "sp500" => vec!["market", "sp500"],
        "history-start" => vec!["market", "history-start"],
        "clock" => vec!["market", "clock"],
        "calendar" => vec!["market", "calendar"],
        "universe" => vec!["data", "universe"],
        "scan" => vec!["data", "scan"],
        "daily" => vec!["data", "daily"],
        "screen" => vec!["data", "screen"],
        "movers" => vec!["data", "movers"],
        "watchlist" => vec!["data", "watchlist"],
        "suggest" => vec!["data", "suggest"],
        "status" => vec!["data", "status"],
        "wash" => vec!["compliance", "wash"],
        "pdt" => vec!["compliance", "pdt"],
        "tax" => vec!["compliance", "tax"],
        _ => return None,
    };
    Some(path)
}

// Handles CLI command help path from args routing.
fn command_help_path_from_args(args: &[OsString]) -> Vec<&'static str> {
    let tokens = command_tokens_from_args(args);
    let Some(first) = tokens.first().map(String::as_str) else {
        return Vec::new();
    };
    match first {
        "start" => return vec!["daemon", "start"],
        "stop" => return vec!["daemon", "stop"],
        "restart" => return vec!["daemon", "restart"],
        "reload" => return vec!["daemon", "reload"],
        _ => {}
    }
    if let Some(path) = direct_alias_help_path(&tokens) {
        return path;
    }
    let Some(top) = canonical_top_command(first) else {
        return Vec::new();
    };
    let mut path = vec![top];
    if let Some(second) = tokens
        .get(1)
        .and_then(|token| canonical_nested_command(top, token))
    {
        path.push(second);
        if top == "runtime" && second == "completions" {
            if let Some(third) = tokens
                .get(2)
                .and_then(|token| canonical_nested_command("completions", token))
            {
                path.push(third);
            }
        }
    }
    path
}

// Parses cli or exit from user or provider input.
fn parse_cli_or_exit() -> Cli {
    let args = std::env::args_os().collect::<Vec<_>>();
    match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(err) => {
            let kind = err.kind();
            let code = err.exit_code();
            let _ = err.print();
            if !matches!(kind, ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                eprintln!("Try: {}", help_command(&command_help_path_from_args(&args)));
            }
            std::process::exit(code);
        }
    }
}

// Handles exit with error and help logic.
fn exit_with_error_and_help(message: &str, help_path: &[&str], json: bool) -> ! {
    let help = help_command(help_path);
    if json {
        eprintln!(
            "{}",
            serde_json::json!({
                "status": "error",
                "error": message,
                "help": help,
            })
        );
    } else {
        eprintln!("❌ Error: {}", message);
        eprintln!("Try: {}", help);
    }
    std::process::exit(1);
}

// ── API types ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Asset {
    symbol: String,
    name: Option<String>,
    exchange: Option<String>,
    status: Option<String>,
    tradable: Option<bool>,
    fractionable: Option<bool>,
    shortable: Option<bool>,
    #[serde(default)]
    #[allow(dead_code)]
    asset_class: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Bar {
    t: String,
    o: f64,
    h: f64,
    l: f64,
    c: f64,
    v: u64,
    vw: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct BarsResponse {
    bars: Option<HashMap<String, Vec<Bar>>>,
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MoverEntry {
    symbol: Option<String>,
    #[serde(default)]
    price: Option<f64>,
    #[serde(default)]
    change: Option<f64>,
    percent_change: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct MoversResponse {
    #[serde(default)]
    gainers: Option<Vec<MoverEntry>>,
    #[serde(default)]
    losers: Option<Vec<MoverEntry>>,
}

// Trading API types
#[derive(Debug, Deserialize)]
struct AccountInfo {
    account_number: Option<String>,
    status: Option<String>,
    portfolio_value: Option<String>,
    equity: Option<String>,
    last_equity: Option<String>,
    cash: Option<String>,
    buying_power: Option<String>,
    pattern_day_trader: Option<bool>,
    trading_blocked: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct Position {
    symbol: String,
    qty: String,
    avg_entry_price: String,
    current_price: String,
    market_value: String,
    unrealized_pl: String,
    unrealized_plpc: String,
}

#[derive(Debug, Serialize)]
struct OrderRequest {
    symbol: String,
    qty: String,
    side: String,
    r#type: String,
    time_in_force: String,
    client_order_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit_price: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_price: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OrderResponse {
    id: Option<String>,
    symbol: Option<String>,
    qty: Option<String>,
    r#type: Option<String>,
    time_in_force: Option<String>,
    status: Option<String>,
    limit_price: Option<String>,
    filled_avg_price: Option<String>,
    side: Option<String>,
    created_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct QuoteData {
    #[serde(alias = "bp")]
    bid_price: Option<f64>,
    #[serde(alias = "ap")]
    ask_price: Option<f64>,
    #[serde(alias = "bs")]
    bid_size: Option<f64>,
    #[serde(alias = "as")]
    ask_size: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct QuoteResponse {
    quote: Option<QuoteData>,
}

#[derive(Debug, Deserialize)]
struct SingleBarsResponse {
    bars: Option<Vec<Bar>>,
}

#[derive(Debug, Deserialize)]
struct SnapshotBar {
    c: Option<f64>,
    h: Option<f64>,
    l: Option<f64>,
    v: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct SnapshotTrade {
    p: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct Snapshot {
    #[serde(alias = "dailyBar")]
    daily_bar: Option<SnapshotBar>,
    #[serde(alias = "prevDailyBar")]
    prev_daily_bar: Option<SnapshotBar>,
    #[serde(alias = "latestTrade")]
    latest_trade: Option<SnapshotTrade>,
}

#[derive(Debug, Deserialize)]
struct NewsItem {
    headline: Option<String>,
    source: Option<String>,
    created_at: Option<String>,
    summary: Option<String>,
    symbols: Option<Vec<String>>,
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NewsResponse {
    news: Option<Vec<NewsItem>>,
}

#[derive(Debug, Deserialize)]
struct FredObservationsResponse {
    observations: Vec<FredObservation>,
}

#[derive(Debug, Deserialize)]
struct FredObservation {
    date: String,
    value: String,
}

// ── DB Helpers ───────────────────────────────────────────────────

fn db_path() -> PathBuf {
    paths::scanner_db_path()
}

// Opens db with the configured runtime settings.
fn open_db() -> rusqlite::Result<Connection> {
    let _ = paths::ensure_state_dir();
    let path = db_path();
    let conn = Connection::open(&path)?;
    conn.execute_batch(&format!(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; {}",
        config::sqlite_runtime_pragma_sql()
    ))?;
    let _ = paths::harden_sqlite_files(&path);
    init_tables(&conn)?;
    Ok(conn)
}

// Handles main table has column database metadata.
fn main_table_has_column(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

// Ensures main column exists or meets required invariants.
fn ensure_main_column(
    conn: &Connection,
    table: &str,
    column: &str,
    ddl: &str,
) -> rusqlite::Result<()> {
    if !main_table_has_column(conn, table, column)? {
        conn.execute_batch(&format!("ALTER TABLE {} ADD COLUMN {}", table, ddl))?;
    }
    Ok(())
}

// Handles SQLite quote ident safely.
fn sqlite_quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

// Handles SQLite i64 safely.
fn sqlite_i64(conn: &Connection, sql: &str) -> anyhow::Result<i64> {
    Ok(conn.query_row(sql, [], |row| row.get::<_, i64>(0))?)
}

// Handles the db stats CLI action.
fn cmd_db_stats(json_out: bool) -> anyhow::Result<()> {
    let conn = open_db()?;
    let database = db_path();
    let file_size = fs::metadata(&database).map(|meta| meta.len()).unwrap_or(0);
    let resources = config::runtime_resources();
    let page_size = sqlite_i64(&conn, "PRAGMA page_size")?;
    let page_count = sqlite_i64(&conn, "PRAGMA page_count")?;
    let freelist_count = sqlite_i64(&conn, "PRAGMA freelist_count")?;
    let cache_size = sqlite_i64(&conn, "PRAGMA cache_size")?;
    let mmap_size = sqlite_i64(&conn, "PRAGMA mmap_size")?;

    let mut largest_objects = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT name, SUM(pgsize) AS bytes, COUNT(*) AS pages
         FROM dbstat GROUP BY name ORDER BY bytes DESC LIMIT 30",
    ) {
        let rows = stmt.query_map([], |row| {
            Ok(serde_json::json!({
                "name": row.get::<_, String>(0)?,
                "bytes": row.get::<_, i64>(1)?,
                "pages": row.get::<_, i64>(2)?,
            }))
        })?;
        largest_objects = rows.filter_map(|row| row.ok()).collect();
    }

    let mut row_counts = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master
         WHERE type='table' AND name NOT LIKE 'sqlite_%'
         ORDER BY name",
    )?;
    let table_names = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .filter_map(|row| row.ok())
        .collect::<Vec<_>>();
    for table in table_names {
        let sql = format!("SELECT COUNT(*) FROM {}", sqlite_quote_ident(&table));
        if let Ok(count) = sqlite_i64(&conn, &sql) {
            row_counts.push(serde_json::json!({
                "table": table,
                "rows": count,
            }));
        }
    }

    let output = serde_json::json!({
        "database": database.display().to_string(),
        "file_size_bytes": file_size,
        "sqlite": {
            "page_size": page_size,
            "page_count": page_count,
            "freelist_count": freelist_count,
            "cache_size_pages_or_negative_kib": cache_size,
            "mmap_size_bytes": mmap_size,
        },
        "configured_resources": {
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
        },
        "largest_objects": largest_objects,
        "row_counts": row_counts,
        "note": "Large DB size is expected when storing full-history bars plus wide ML feature rows. Commands stream SQLite rows and use bounded cache settings; LSTM and LightGBM training are capped by resources.* config to avoid OOM on small hosts.",
    });
    if json_out {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("SQLite DB Stats");
        println!("  File:      {}", database.display());
        println!("  Size:      {:.2} GB", file_size as f64 / 1_073_741_824.0);
        println!("  Pages:     {} x {}", page_count, page_size);
        println!("  Freelist:  {} pages", freelist_count);
        println!(
            "  Memory:    {:.2} GB detected via {}, budget {}% ({:.2} GB)",
            resources.memory_total_bytes as f64 / 1_073_741_824.0,
            resources.memory_source,
            resources.memory_budget_percent,
            resources.memory_budget_bytes as f64 / 1_073_741_824.0
        );
        println!(
            "  Cache:     {} MB, temp_store={}, mmap={} MB",
            resources.sqlite_cache_mb, resources.sqlite_temp_store, resources.sqlite_mmap_mb
        );
        println!(
            "  CPU cap:   budget {}% total process CPU ({}% of {} logical CPUs), workers {} threads (~{}% CPU); GPU/NPU backends are uncapped",
            resources.cpu_budget_process_percent,
            resources.cpu_budget_percent,
            resources.cpu_total_threads,
            resources.cpu_worker_threads,
            resources.cpu_worker_capacity_percent
        );
        println!("  Largest objects:");
        let empty = Vec::new();
        for object in output["largest_objects"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .take(10)
        {
            println!(
                "    {:<36} {:>8.2} GB",
                object["name"].as_str().unwrap_or(""),
                object["bytes"].as_i64().unwrap_or(0) as f64 / 1_073_741_824.0
            );
        }
        println!("  Use `mlai-trade data db-optimize` for PRAGMA optimize/checkpoint.");
    }
    Ok(())
}

// Handles the db optimize CLI action.
fn cmd_db_optimize(vacuum: bool, json_out: bool) -> anyhow::Result<()> {
    let conn = open_db()?;
    conn.execute_batch("PRAGMA optimize; PRAGMA wal_checkpoint(TRUNCATE);")?;
    if vacuum {
        conn.execute_batch("VACUUM;")?;
    }
    let path = db_path();
    let _ = paths::harden_sqlite_files(&path);
    let output = serde_json::json!({
        "status": "done",
        "database": path.display().to_string(),
        "ran": {
            "pragma_optimize": true,
            "wal_checkpoint_truncate": true,
            "vacuum": vacuum,
        },
        "file_size_bytes": fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0),
        "note": if vacuum {
            "VACUUM rewrote the SQLite file; it can take a long time on large DBs."
        } else {
            "Default maintenance does not rewrite the main DB file. Add --vacuum only when you intentionally want to reclaim free pages."
        },
    });
    if json_out {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("SQLite maintenance complete");
        println!("  DB:      {}", path.display());
        println!("  VACUUM:  {}", vacuum);
    }
    Ok(())
}

// Initializes tables tables or runtime state.
fn init_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS assets (
            symbol TEXT PRIMARY KEY, name TEXT, exchange TEXT, status TEXT,
            tradable INTEGER, fractionable INTEGER, shortable INTEGER, updated_at TEXT
        );
        CREATE TABLE IF NOT EXISTS bars (
            symbol TEXT NOT NULL, date TEXT NOT NULL,
            open REAL, high REAL, low REAL, close REAL, volume INTEGER, vwap REAL,
            PRIMARY KEY (symbol, date)
        );
        CREATE INDEX IF NOT EXISTS idx_bars_date ON bars(date);
        CREATE TABLE IF NOT EXISTS screen_results (
            date TEXT NOT NULL, symbol TEXT NOT NULL,
            close REAL, change_pct REAL, volume_ratio REAL, signals TEXT,
            confidence TEXT DEFAULT NULL,
            PRIMARY KEY (date, symbol)
        );
        CREATE TABLE IF NOT EXISTS wash_sale_tracker (
            symbol TEXT NOT NULL, sell_date TEXT NOT NULL,
            sell_price REAL, loss_amount REAL,
            wash_window_end TEXT NOT NULL, status TEXT DEFAULT 'active',
            PRIMARY KEY (symbol, sell_date)
        );
        CREATE TABLE IF NOT EXISTS day_trades (
            trade_date TEXT NOT NULL, symbol TEXT NOT NULL,
            buy_time TEXT, sell_time TEXT,
            PRIMARY KEY (trade_date, symbol, buy_time)
        );

        -- News feed tables
        CREATE TABLE IF NOT EXISTS news_articles (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source TEXT NOT NULL,
            title TEXT NOT NULL,
            url TEXT UNIQUE,
            summary TEXT,
            symbols TEXT,
            published_at TEXT,
            published_date TEXT,
            fetched_at TEXT NOT NULL,
            sentiment_score REAL,
            filing_type TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_news_symbols ON news_articles(symbols);
        CREATE INDEX IF NOT EXISTS idx_news_published ON news_articles(published_at);

        CREATE TABLE IF NOT EXISTS feed_subscriptions (
            symbol TEXT PRIMARY KEY,
            cik TEXT,
            added_at TEXT NOT NULL,
            last_sync TEXT,
            subscription_source TEXT NOT NULL DEFAULT 'manual',
            managed INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS company_relationships (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            symbol_a TEXT NOT NULL,
            symbol_b TEXT NOT NULL,
            relationship TEXT NOT NULL,
            strength REAL DEFAULT 1.0,
            source TEXT,
            discovered_at TEXT NOT NULL,
            UNIQUE(symbol_a, symbol_b, relationship)
        );

        CREATE TABLE IF NOT EXISTS price_correlations (
            symbol_a TEXT NOT NULL,
            symbol_b TEXT NOT NULL,
            correlation_30d REAL,
            correlation_90d REAL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (symbol_a, symbol_b)
        );

        CREATE TABLE IF NOT EXISTS macro_series (
            series_id TEXT NOT NULL,
            date TEXT NOT NULL,
            value REAL NOT NULL,
            source TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (series_id, date)
        );",
    )?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_macro_series_date ON macro_series(series_id, date);",
    )?;
    let _ =
        conn.execute_batch("ALTER TABLE screen_results ADD COLUMN confidence TEXT DEFAULT NULL;");
    ensure_main_column(conn, "wash_sale_tracker", "sell_time", "sell_time TEXT")?;
    ensure_main_column(
        conn,
        "wash_sale_tracker",
        "sell_timestamp_utc",
        "sell_timestamp_utc TEXT",
    )?;
    ensure_main_column(
        conn,
        "wash_sale_tracker",
        "event_timezone",
        "event_timezone TEXT NOT NULL DEFAULT 'UTC'",
    )?;
    ensure_main_column(
        conn,
        "wash_sale_tracker",
        "paper_account",
        "paper_account INTEGER NOT NULL DEFAULT 1",
    )?;
    ensure_main_column(
        conn,
        "day_trades",
        "sell_timestamp_utc",
        "sell_timestamp_utc TEXT",
    )?;
    ensure_main_column(
        conn,
        "day_trades",
        "event_timezone",
        "event_timezone TEXT NOT NULL DEFAULT 'UTC'",
    )?;
    ensure_main_column(
        conn,
        "day_trades",
        "paper_account",
        "paper_account INTEGER NOT NULL DEFAULT 1",
    )?;
    ensure_main_column(
        conn,
        "news_articles",
        "published_date",
        "published_date TEXT",
    )?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_news_published_date ON news_articles(published_date);",
    )?;
    ensure_main_column(
        conn,
        "feed_subscriptions",
        "subscription_source",
        "subscription_source TEXT NOT NULL DEFAULT 'manual'",
    )?;
    ensure_main_column(
        conn,
        "feed_subscriptions",
        "managed",
        "managed INTEGER NOT NULL DEFAULT 0",
    )?;
    let _ = conn.execute(
        "UPDATE news_articles
         SET published_date = substr(published_at, 1, 10)
         WHERE published_date IS NULL
           AND published_at IS NOT NULL
           AND length(published_at) >= 10",
        [],
    );
    Ok(())
}

// ── HTTP Helpers ─────────────────────────────────────────────────

fn build_headers() -> HeaderMap {
    let account = config::alpaca_primary_account().unwrap_or_else(|e| {
        eprintln!("❌ Error: {}", e);
        eprintln!("Try: mlai-trade --help");
        std::process::exit(1);
    });
    build_headers_for(&account)
}

// Builds headers for from configured inputs.
fn build_headers_for(account: &config::AlpacaAccount) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(
        "APCA-API-KEY-ID",
        HeaderValue::from_str(&account.api_key_id).unwrap(),
    );
    h.insert(
        "APCA-API-SECRET-KEY",
        HeaderValue::from_str(&account.secret_key).unwrap(),
    );
    h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    h.insert(ACCEPT, HeaderValue::from_static("application/json"));
    h
}

// Builds client from configured inputs.
fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .default_headers(build_headers())
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("Failed to build HTTP client")
}

// Builds client for from configured inputs.
fn build_client_for(account: &config::AlpacaAccount) -> reqwest::Client {
    reqwest::Client::builder()
        .default_headers(build_headers_for(account))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("Failed to build HTTP client")
}

// Runs the api get API helper.
async fn api_get<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
) -> anyhow::Result<T> {
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("API error {}: {}", status, body);
    }
    Ok(resp.json().await?)
}

// Runs the api get stock quote API helper.
async fn api_get_stock_quote(
    client: &reqwest::Client,
    symbol: &str,
) -> anyhow::Result<QuoteResponse> {
    let feeds = alpaca::data_feeds();
    let mut last_error = None;
    for (idx, feed) in feeds.iter().enumerate() {
        match api_get::<QuoteResponse>(client, &alpaca::stock_quote_url(symbol, feed)).await {
            Ok(value) => return Ok(value),
            Err(err) => {
                if idx + 1 < feeds.len() {
                    eprintln!(
                        "  warning: stock quote feed '{}' failed for {}; trying fallback feed",
                        feed, symbol
                    );
                }
                last_error = Some(err);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("No stock quote feed configured")))
}

// Runs the api get stock snapshot API helper.
async fn api_get_stock_snapshot(
    client: &reqwest::Client,
    symbol: &str,
) -> anyhow::Result<Snapshot> {
    let feeds = alpaca::data_feeds();
    let mut last_error = None;
    for (idx, feed) in feeds.iter().enumerate() {
        match api_get::<Snapshot>(client, &alpaca::stock_snapshot_url(symbol, feed)).await {
            Ok(value) => return Ok(value),
            Err(err) => {
                if idx + 1 < feeds.len() {
                    eprintln!(
                        "  warning: stock snapshot feed '{}' failed for {}; trying fallback feed",
                        feed, symbol
                    );
                }
                last_error = Some(err);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("No stock snapshot feed configured")))
}

// Runs the api post API helper.
async fn api_post<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    body: &impl Serialize,
) -> anyhow::Result<T> {
    let resp = client.post(url).json(body).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        anyhow::bail!("API error {}: {}", status, body_text);
    }
    Ok(resp.json().await?)
}

// Runs the api delete text API helper.
async fn api_delete_text(client: &reqwest::Client, url: &str) -> anyhow::Result<()> {
    let resp = client.delete(url).send().await?;
    if !resp.status().is_success() && resp.status().as_u16() != 204 {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("API error {}: {}", status, body);
    }
    Ok(())
}

// ══════════════════════════════════════════════════════════════════
//  TRADING COMMANDS
// ══════════════════════════════════════════════════════════════════

async fn cmd_account(accounts: Vec<String>, json_out: bool) -> anyhow::Result<()> {
    let accounts = selected_alpaca_accounts(&accounts, true)?;
    let mut rows = Vec::new();

    for account in &accounts {
        let client = build_client_for(account);
        match api_get::<AccountInfo>(&client, &alpaca::broker_api_url_for(account, "/account"))
            .await
        {
            Ok(acct) => {
                let equity = acct
                    .equity
                    .as_deref()
                    .unwrap_or("0")
                    .parse::<f64>()
                    .unwrap_or(0.0);
                let last_eq = acct
                    .last_equity
                    .as_deref()
                    .unwrap_or("0")
                    .parse::<f64>()
                    .unwrap_or(0.0);
                let cash = acct
                    .cash
                    .as_deref()
                    .unwrap_or("0")
                    .parse::<f64>()
                    .unwrap_or(0.0);
                let bp = acct
                    .buying_power
                    .as_deref()
                    .unwrap_or("0")
                    .parse::<f64>()
                    .unwrap_or(0.0);
                let pv = acct
                    .portfolio_value
                    .as_deref()
                    .unwrap_or("0")
                    .parse::<f64>()
                    .unwrap_or(0.0);
                let day_change = equity - last_eq;
                let day_pct = if last_eq > 0.0 {
                    day_change / last_eq * 100.0
                } else {
                    0.0
                };

                if json_out {
                    rows.push(serde_json::json!({
                        "status": "ok",
                        "account": account_json_metadata(account, acct.account_number.as_deref()),
                        "broker_status": acct.status.clone().unwrap_or_default(),
                        "portfolio_value": pv,
                        "equity": equity,
                        "cash": cash,
                        "buying_power": bp,
                        "last_equity": last_eq,
                        "day_pnl": day_change,
                        "day_pnl_pct": day_pct,
                        "pattern_day_trader": acct.pattern_day_trader.unwrap_or(false),
                        "trading_blocked": acct.trading_blocked.unwrap_or(false),
                    }));
                    continue;
                }

                let mode = if account.is_paper() {
                    "🟡 PAPER"
                } else {
                    "🔴 LIVE"
                };
                println!(
                    "{} {} Trading Account: {}",
                    mode,
                    format!("{}:{}", account.provider(), account.account_ref()),
                    mask_account_number(acct.account_number.as_deref())
                );
                println!("  Account ID:      {}", account.account_ref());
                println!(
                    "  Selector:        {}:{}",
                    account.provider(),
                    account.account_ref()
                );
                println!(
                    "  Tax universe:    {}",
                    if account.is_paper() { "paper" } else { "real" }
                );
                println!("  Data feed:       {}", account.data_feed);
                println!(
                    "  Status:          {}",
                    acct.status.as_deref().unwrap_or("?")
                );
                println!("  Portfolio Value:  {}", fmt_money_comma(pv));
                println!("  Equity:          {}", fmt_money_comma(equity));
                println!("  Cash:            {}", fmt_money_comma(cash));
                println!("  Buying Power:    {}", fmt_money_comma(bp));
                println!(
                    "  Day P&L:         {} ({:+.2}%)",
                    fmt_money_comma(day_change),
                    day_pct
                );
                println!(
                    "  Pattern Day Trader: {}",
                    acct.pattern_day_trader.unwrap_or(false)
                );
                println!(
                    "  Trading Blocked:    {}",
                    acct.trading_blocked.unwrap_or(false)
                );
                println!();
            }
            Err(err) => {
                if json_out {
                    rows.push(serde_json::json!({
                        "status": "error",
                        "account": account_json_metadata(account, None),
                        "error": err.to_string(),
                    }));
                } else {
                    println!("{}: error: {}", account_label(account, None), err);
                }
            }
        }
    }

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "accounts": rows }))?
        );
    }
    Ok(())
}

// Handles the buy CLI action.
async fn cmd_buy(
    symbol: String,
    qty: f64,
    accounts: Vec<String>,
    order_type: String,
    limit_price: Option<f64>,
    stop_price: Option<f64>,
    tif: String,
) -> anyhow::Result<()> {
    let sym = symbol.to_uppercase();
    let (order_type, tif) = validate_equity_order(&sym, qty, &order_type, &tif)?;
    let accounts = selected_alpaca_accounts(&accounts, false)?;

    for account in &accounts {
        println!("Account: {}", account_label(account, None));

        // Wash sale replacement buys are compliance blockers. Real accounts share
        // one taxpayer-wide universe; paper accounts are scoped to the paper account.
        {
            let conn = open_db()?;
            let today = Utc::now().format("%Y-%m-%d").to_string();
            let rows: Vec<(String, f64, f64, String)> = if account.is_paper() {
                let mut stmt = conn.prepare(
                    "SELECT sell_date, sell_price, loss_amount, wash_window_end
                     FROM wash_sale_tracker
                     WHERE symbol=?1 AND status='active' AND wash_window_end >= ?2
                       AND paper_account=1 AND provider=?3 AND account_ref=?4",
                )?;
                let rows = stmt
                    .query_map(
                        params![sym, today, account.provider(), account.account_ref()],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )?
                    .filter_map(|r| r.ok())
                    .collect();
                rows
            } else {
                let mut stmt = conn.prepare(
                    "SELECT sell_date, sell_price, loss_amount, wash_window_end
                     FROM wash_sale_tracker
                     WHERE symbol=?1 AND status='active' AND wash_window_end >= ?2
                       AND paper_account=0",
                )?;
                let rows = stmt
                    .query_map(params![sym, today], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                    })?
                    .filter_map(|r| r.ok())
                    .collect();
                rows
            };
            for (sell_date, sell_price, loss_amt, window_end) in &rows {
                let days_left = chrono::NaiveDate::parse_from_str(window_end, "%Y-%m-%d")
                    .ok()
                    .and_then(|e| {
                        chrono::NaiveDate::parse_from_str(&today, "%Y-%m-%d")
                            .ok()
                            .map(|t| (e - t).num_days())
                    })
                    .unwrap_or(0);
                println!(
                    "⚠️  WASH SALE WARNING: {} was sold at a loss on {}",
                    sym, sell_date
                );
                println!(
                    "   Loss: {} | Sold at: {}",
                    fmt_money(*loss_amt),
                    fmt_money(*sell_price)
                );
                println!(
                    "   Wash window ends: {} ({} days left; {} IRS days + {} buffer)",
                    window_end,
                    days_left,
                    IRS_WASH_SALE_WINDOW_DAYS,
                    configured_wash_sale_safety_buffer_days()
                );
                println!(
                    "   Buying now will DISALLOW the {} loss deduction (IRS §1091)\n",
                    fmt_money(*loss_amt)
                );
            }
            if !rows.is_empty() {
                anyhow::bail!("Blocked {} buy for {}:{}: active wash-sale replacement window. Wait until the window expires or review `mlai-trade compliance wash`.", sym, account.provider(), account.account_ref());
            }
        }

        let client = build_client_for(account);
        if let Ok(acct) =
            api_get::<AccountInfo>(&client, &alpaca::broker_api_url_for(account, "/account")).await
        {
            println!(
                "  Broker account: {}",
                mask_account_number(acct.account_number.as_deref())
            );
            let equity = acct
                .equity
                .as_deref()
                .unwrap_or("0")
                .parse::<f64>()
                .unwrap_or(0.0);
            let cash = acct
                .cash
                .as_deref()
                .unwrap_or("0")
                .parse::<f64>()
                .unwrap_or(0.0);
            if equity > 0.0 {
                // Estimate order value
                let est_price = if let Some(lp) = limit_price {
                    lp
                } else {
                    match api_get_stock_quote(&client, &sym).await {
                        Ok(qr) => {
                            let q = qr.quote.unwrap_or(QuoteData {
                                bid_price: None,
                                ask_price: None,
                                bid_size: None,
                                ask_size: None,
                            });
                            let bp = q.bid_price.unwrap_or(0.0);
                            let ap = q.ask_price.unwrap_or(0.0);
                            if bp > 0.0 && ap > 0.0 {
                                (bp + ap) / 2.0
                            } else {
                                bp.max(ap)
                            }
                        }
                        Err(_) => 0.0,
                    }
                };
                if est_price <= 0.0 {
                    anyhow::bail!("Blocked {} buy: could not estimate order value for cash-only compliance check.", sym);
                }
                if est_price > 0.0 {
                    let order_value = est_price * qty;
                    if cash <= 0.0 || order_value > cash * 0.95 {
                        anyhow::bail!(
                            "Blocked {} buy: cash-only trading enforced (order {}, available cash {}).",
                            sym,
                            fmt_money_comma(order_value),
                            fmt_money_comma(cash)
                        );
                    }
                    let pct = order_value / equity;
                    if pct > MAX_POSITION_PCT {
                        println!(
                            "⚠️  POSITION SIZE WARNING: {} would be {:.1}% of portfolio",
                            sym,
                            pct * 100.0
                        );
                        println!(
                            "   Recommended max: {:.0}% ({})",
                            MAX_POSITION_PCT * 100.0,
                            fmt_money_comma(equity * MAX_POSITION_PCT)
                        );
                        println!("   This order: {}\n", fmt_money_comma(order_value));
                    }
                }
                // Check total positions
                if let Ok(positions) = api_get::<Vec<Position>>(
                    &client,
                    &alpaca::broker_api_url_for(account, "/positions"),
                )
                .await
                {
                    let is_new = !positions.iter().any(|p| p.symbol == sym);
                    if is_new && positions.len() >= MAX_TOTAL_POSITIONS {
                        println!(
                            "⚠️  DIVERSIFICATION WARNING: You already have {} positions (max {})\n",
                            positions.len(),
                            MAX_TOTAL_POSITIONS
                        );
                    }
                }
            }
        }

        let mut otype = order_type.clone();
        let lp = limit_price.map(|p| format!("{}", p));
        let sp = stop_price.map(|p| format!("{}", p));
        if limit_price.is_some() {
            otype = "limit".into();
        }
        if stop_price.is_some() {
            if limit_price.is_some() {
                otype = "stop_limit".into();
            } else {
                otype = "stop".into();
            }
        }

        let order = OrderRequest {
            symbol: sym.clone(),
            qty: format!("{}", qty),
            side: "buy".into(),
            r#type: otype,
            time_in_force: tif.clone(),
            client_order_id: client_order_id(account.account_ref(), "buy", &sym),
            limit_price: lp,
            stop_price: sp,
        };

        let result: OrderResponse = api_post(
            &client,
            &alpaca::broker_api_url_for(account, "/orders"),
            &order,
        )
        .await?;
        println!("✅ Buy order placed!");
        println!(
            "  Account:  {}:{}",
            account.provider(),
            account.account_ref()
        );
        println!("  Symbol:   {}", result.symbol.as_deref().unwrap_or("?"));
        println!("  Qty:      {}", result.qty.as_deref().unwrap_or("?"));
        println!("  Type:     {}", result.r#type.as_deref().unwrap_or("?"));
        println!(
            "  TIF:      {}",
            result.time_in_force.as_deref().unwrap_or("?")
        );
        println!("  Status:   {}", result.status.as_deref().unwrap_or("?"));
        println!("  Order ID: {}", result.id.as_deref().unwrap_or("?"));
        if let Some(lp) = &result.limit_price {
            println!("  Limit:    {}", lp);
        }
    }
    if let Err(err) = auto::sync_orders_all_accounts(true).await {
        eprintln!(
            "warning: provider order/fill sync after buy failed: {}",
            err
        );
    }
    Ok(())
}

// Handles the sell CLI action.
async fn cmd_sell(
    symbol: String,
    qty: f64,
    accounts: Vec<String>,
    order_type: String,
    limit_price: Option<f64>,
    stop_price: Option<f64>,
    tif: String,
) -> anyhow::Result<()> {
    let sym = symbol.to_uppercase();
    let (order_type, tif) = validate_equity_order(&sym, qty, &order_type, &tif)?;
    let accounts = selected_alpaca_accounts(&accounts, false)?;

    for account in &accounts {
        println!("Account: {}", account_label(account, None));
        let client = build_client_for(account);
        let acct =
            api_get::<AccountInfo>(&client, &alpaca::broker_api_url_for(account, "/account"))
                .await
                .ok();
        if let Some(acct) = &acct {
            println!(
                "  Broker account: {}",
                mask_account_number(acct.account_number.as_deref())
            );
        }

        // PDT check is scoped per account.
        let conn = open_db()?;
        let window_start = (Utc::now() - chrono::Duration::days(7))
            .format("%Y-%m-%d")
            .to_string();
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM day_trades
                 WHERE provider=?1 AND account_ref=?2 AND paper_account=?3
                   AND trade_date >= ?4 AND trade_date <= ?5",
                params![
                    account.provider(),
                    account.account_ref(),
                    if account.is_paper() { 1 } else { 0 },
                    window_start,
                    today
                ],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if count >= PDT_TRADE_LIMIT {
            println!(
                "🚨 PDT WARNING: You have {} day trades in the last 5 business days!",
                count
            );
            println!(
                "   Current PDT rules may restrict margin accounts below {}.",
                fmt_money_comma(PDT_MIN_EQUITY_DOLLARS_PRE_2026_06_04)
            );
            println!("   FINRA intraday margin rules replace PDT on June 4, 2026; broker implementation may vary.\n");
        } else if count == PDT_TRADE_LIMIT - 1 {
            println!(
                "⚠️  PDT CAUTION: {} day trades in 5 days. ONE MORE triggers PDT!\n",
                count
            );
        }

        // Get avg entry for wash sale tracking.
        let avg_entry: Option<f64> = api_get::<Position>(
            &client,
            &alpaca::broker_api_url_for(account, &format!("/positions/{}", sym)),
        )
        .await
        .ok()
        .and_then(|p| p.avg_entry_price.parse::<f64>().ok());

        let mut otype = order_type.clone();
        let lp = limit_price.map(|p| format!("{}", p));
        let sp = stop_price.map(|p| format!("{}", p));
        if limit_price.is_some() {
            otype = "limit".into();
        }
        if stop_price.is_some() {
            if limit_price.is_some() {
                otype = "stop_limit".into();
            } else {
                otype = "stop".into();
            }
        }

        let order = OrderRequest {
            symbol: sym.clone(),
            qty: format!("{}", qty),
            side: "sell".into(),
            r#type: otype,
            time_in_force: tif.clone(),
            client_order_id: client_order_id(account.account_ref(), "sell", &sym),
            limit_price: lp,
            stop_price: sp,
        };
        let result: OrderResponse = api_post(
            &client,
            &alpaca::broker_api_url_for(account, "/orders"),
            &order,
        )
        .await?;
        println!("✅ Sell order placed!");
        println!(
            "  Account:  {}:{}",
            account.provider(),
            account.account_ref()
        );
        println!("  Symbol:   {}", result.symbol.as_deref().unwrap_or("?"));
        println!("  Qty:      {}", result.qty.as_deref().unwrap_or("?"));
        println!("  Type:     {}", result.r#type.as_deref().unwrap_or("?"));
        println!(
            "  TIF:      {}",
            result.time_in_force.as_deref().unwrap_or("?")
        );
        println!("  Status:   {}", result.status.as_deref().unwrap_or("?"));
        println!("  Order ID: {}", result.id.as_deref().unwrap_or("?"));

        // Record day trade.
        let now = Utc::now();
        let sell_time = now.format("%H:%M:%S").to_string();
        let sell_timestamp_utc = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let _ = conn.execute(
            "INSERT OR IGNORE INTO day_trades (
                trade_date, symbol, buy_time, sell_time, sell_timestamp_utc, event_timezone,
                provider, account_ref, paper_account
             )
             VALUES (?1, ?2, 'same_day', ?3, ?4, 'UTC', ?5, ?6, ?7)",
            params![
                today,
                sym,
                sell_time,
                sell_timestamp_utc,
                account.provider(),
                account.account_ref(),
                if account.is_paper() { 1 } else { 0 }
            ],
        );

        // Wash sale tracking: record current estimated losses by account/universe.
        if let Some(avg) = avg_entry {
            let sell_price = limit_price.unwrap_or(0.0);
            let est_sell = if sell_price > 0.0 {
                sell_price
            } else {
                match api_get_stock_quote(&client, &sym).await {
                    Ok(qr) => qr.quote.and_then(|q| q.bid_price).unwrap_or(0.0),
                    Err(_) => 0.0,
                }
            };
            if est_sell > 0.0 && est_sell < avg {
                let loss_per_share = avg - est_sell;
                let total_loss = loss_per_share * qty;
                println!(
                    "\n📝 Loss detected: {} ({:.2}/share)",
                    fmt_money(total_loss),
                    loss_per_share
                );
                let sell_date = now.format("%Y-%m-%d").to_string();
                let forward_days = configured_wash_sale_forward_block_days();
                let window_end = (now + chrono::Duration::days(forward_days))
                    .format("%Y-%m-%d")
                    .to_string();
                conn.execute(
                    "INSERT OR REPLACE INTO wash_sale_tracker (
                        symbol, sell_date, sell_time, sell_timestamp_utc, event_timezone,
                        sell_price, loss_amount, wash_window_end, status, provider, account_ref,
                        paper_account
                     )
                     VALUES (?1, ?2, ?3, ?4, 'UTC', ?5, ?6, ?7, 'active', ?8, ?9, ?10)",
                    params![
                        sym,
                        sell_date,
                        sell_time,
                        sell_timestamp_utc,
                        est_sell,
                        total_loss,
                        window_end,
                        account.provider(),
                        account.account_ref(),
                        if account.is_paper() { 1 } else { 0 }
                    ],
                )?;
                println!(
                    "📝 Wash sale window recorded: {} — {} IRS days + {} buffer, until {}",
                    sym,
                    IRS_WASH_SALE_WINDOW_DAYS,
                    configured_wash_sale_safety_buffer_days(),
                    window_end
                );
            }
        }
    }
    if let Err(err) = auto::sync_orders_all_accounts(true).await {
        eprintln!(
            "warning: provider order/fill sync after sell failed: {}",
            err
        );
    }
    Ok(())
}

// Handles the positions CLI action.
async fn cmd_positions(accounts: Vec<String>, json_out: bool) -> anyhow::Result<()> {
    let accounts = selected_alpaca_accounts(&accounts, true)?;
    let mut account_rows = Vec::new();

    for account in &accounts {
        let client = build_client_for(account);
        let acct =
            api_get::<AccountInfo>(&client, &alpaca::broker_api_url_for(account, "/account"))
                .await
                .ok();
        let positions: Vec<Position> =
            api_get(&client, &alpaca::broker_api_url_for(account, "/positions")).await?;

        if json_out {
            let items: Vec<serde_json::Value> = positions
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "account": account_json_metadata(account, acct.as_ref().and_then(|acct| acct.account_number.as_deref())),
                        "symbol": p.symbol,
                        "qty": p.qty.parse::<f64>().unwrap_or(0.0),
                        "avg_entry_price": p.avg_entry_price.parse::<f64>().unwrap_or(0.0),
                        "current_price": p.current_price.parse::<f64>().unwrap_or(0.0),
                        "market_value": p.market_value.parse::<f64>().unwrap_or(0.0),
                        "unrealized_pl": p.unrealized_pl.parse::<f64>().unwrap_or(0.0),
                        "unrealized_plpc": p.unrealized_plpc.parse::<f64>().unwrap_or(0.0) * 100.0,
                    })
                })
                .collect();
            account_rows.push(serde_json::json!({
                "account": account_json_metadata(account, acct.as_ref().and_then(|acct| acct.account_number.as_deref())),
                "positions": items,
            }));
            continue;
        }

        println!(
            "Account: {}",
            account_label(
                account,
                acct.as_ref()
                    .and_then(|acct| acct.account_number.as_deref())
            )
        );
        if positions.is_empty() {
            println!("No open positions.\n");
            continue;
        }
        println!(
            "{:<8} {:>8} {:>10} {:>10} {:>12} {:>12} {:>8}",
            "Symbol", "Qty", "Avg Cost", "Current", "Mkt Value", "P&L", "P&L%"
        );
        println!("{}", "-".repeat(72));
        for p in &positions {
            let qty: f64 = p.qty.parse().unwrap_or(0.0);
            let avg: f64 = p.avg_entry_price.parse().unwrap_or(0.0);
            let cur: f64 = p.current_price.parse().unwrap_or(0.0);
            let mv: f64 = p.market_value.parse().unwrap_or(0.0);
            let pnl: f64 = p.unrealized_pl.parse().unwrap_or(0.0);
            let pnl_pct: f64 = p.unrealized_plpc.parse().unwrap_or(0.0) * 100.0;
            println!(
                "{:<8} {:>8.2} {:>10} {:>10} {:>12} {:>12} {:>+7.2}%",
                p.symbol,
                qty,
                fmt_money(avg),
                fmt_money(cur),
                fmt_money_comma(mv),
                fmt_money_comma(pnl),
                pnl_pct
            );
        }
        println!();
    }

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "accounts": account_rows }))?
        );
    }
    Ok(())
}

// Handles the orders CLI action.
async fn cmd_orders(
    accounts: Vec<String>,
    status: String,
    limit: u32,
    sync: bool,
    json_out: bool,
) -> anyhow::Result<()> {
    let accounts = selected_alpaca_accounts(&accounts, true)?;
    let status = status.to_ascii_lowercase();
    if !matches!(status.as_str(), "open" | "closed" | "all") {
        anyhow::bail!(
            "Invalid order status '{}'. Use open, closed, or all.",
            status
        );
    }
    let limit = limit.clamp(1, 500);
    if sync {
        let _ = auto::sync_orders_all_accounts(!json_out).await?;
    }

    let mut account_rows = Vec::new();
    let mut printed_any = false;
    for account in &accounts {
        let client = build_client_for(account);
        let acct =
            api_get::<AccountInfo>(&client, &alpaca::broker_api_url_for(account, "/account"))
                .await
                .ok();
        let orders: Vec<OrderResponse> = api_get(
            &client,
            &alpaca::broker_api_url_for(
                account,
                &format!("/orders?status={}&limit={}&direction=desc", status, limit),
            ),
        )
        .await?;

        if json_out {
            let items: Vec<serde_json::Value> = orders.iter().map(|o| {
            serde_json::json!({
                "account": account_json_metadata(account, acct.as_ref().and_then(|acct| acct.account_number.as_deref())),
                "id": o.id.clone().unwrap_or_default(),
                "symbol": o.symbol.clone().unwrap_or_default(),
                "side": o.side.clone().unwrap_or_default(),
                "qty": o.qty.clone().unwrap_or_default(),
                "type": o.r#type.clone().unwrap_or_default(),
                "time_in_force": o.time_in_force.clone().unwrap_or_default(),
                "status": o.status.clone().unwrap_or_default(),
                "filled_avg_price": o.filled_avg_price.clone().unwrap_or_default(),
                "created_at": o.created_at.as_deref().unwrap_or("").chars().take(19).collect::<String>(),
            })
        }).collect();
            account_rows.push(serde_json::json!({
            "account": account_json_metadata(account, acct.as_ref().and_then(|acct| acct.account_number.as_deref())),
            "status_filter": status,
            "limit": limit,
            "synced_before_listing": sync,
            "orders": items,
        }));
            continue;
        }

        println!(
            "Account: {}",
            account_label(
                account,
                acct.as_ref()
                    .and_then(|acct| acct.account_number.as_deref())
            )
        );
        println!(
            "Status filter: {} | Limit: {} | Synced: {}",
            status, limit, sync
        );
        if orders.is_empty() {
            println!("No orders found.");
            println!();
            continue;
        }
        println!(
            "{:<20} {:<8} {:<5} {:>6} {:<12} {:<12} {:>10}",
            "Time", "Symbol", "Side", "Qty", "Type", "Status", "Fill Price"
        );
        println!("{}", "-".repeat(80));
        for o in &orders {
            let ts = o.created_at.as_deref().unwrap_or("?");
            let ts_short = if ts.len() >= 19 { &ts[..19] } else { ts };
            let fill = o.filled_avg_price.as_deref().unwrap_or("—");
            println!(
                "{:<20} {:<8} {:<5} {:>6} {:<12} {:<12} {:>10}",
                ts_short.replace("T", " "),
                o.symbol.as_deref().unwrap_or("?"),
                o.side.as_deref().unwrap_or("?"),
                o.qty.as_deref().unwrap_or("?"),
                o.r#type.as_deref().unwrap_or("?"),
                o.status.as_deref().unwrap_or("?"),
                fill
            );
        }
        println!();
        printed_any = true;
    }

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "synced_before_listing": sync,
                "accounts": account_rows,
            }))?
        );
    } else if !printed_any {
        println!("No listed orders across selected accounts.");
    }
    Ok(())
}

// Handles the cancel CLI action.
async fn cmd_cancel(order_id: String, accounts: Vec<String>) -> anyhow::Result<()> {
    let accounts = selected_alpaca_accounts(&accounts, false)?;
    for account in &accounts {
        let client = build_client_for(account);
        println!("Account: {}", account_label(account, None));
        if order_id == "all" {
            api_delete_text(&client, &alpaca::broker_api_url_for(account, "/orders")).await?;
            println!(
                "✅ All open orders cancelled for {}:{}.",
                account.provider(),
                account.account_ref()
            );
        } else {
            api_delete_text(
                &client,
                &alpaca::broker_api_url_for(account, &format!("/orders/{}", order_id)),
            )
            .await?;
            println!(
                "✅ Order {} cancelled for {}:{}.",
                order_id,
                account.provider(),
                account.account_ref()
            );
        }
    }
    if let Err(err) = auto::sync_orders_all_accounts(true).await {
        eprintln!(
            "warning: provider order/fill sync after cancel failed: {}",
            err
        );
    }
    Ok(())
}

// Handles the close CLI action.
async fn cmd_close(symbol: String, accounts: Vec<String>) -> anyhow::Result<()> {
    let accounts = selected_alpaca_accounts(&accounts, false)?;
    let sym = if symbol == "all" {
        "all".to_string()
    } else {
        symbol.to_uppercase()
    };
    if sym != "all" {
        check_blocked(&sym)?;
    }

    for account in &accounts {
        let client = build_client_for(account);
        println!("Account: {}", account_label(account, None));
        if sym == "all" {
            api_delete_text(&client, &alpaca::broker_api_url_for(account, "/positions")).await?;
            println!(
                "✅ All positions closed for {}:{}.",
                account.provider(),
                account.account_ref()
            );
        } else {
            // Check for loss before closing (wash sale tracking).
            if let Ok(pos) = api_get::<Position>(
                &client,
                &alpaca::broker_api_url_for(account, &format!("/positions/{}", sym)),
            )
            .await
            {
                let pnl: f64 = pos.unrealized_pl.parse().unwrap_or(0.0);
                if pnl < 0.0 {
                    let cur: f64 = pos.current_price.parse().unwrap_or(0.0);
                    let conn = open_db()?;
                    let now = Utc::now();
                    let sell_date = now.format("%Y-%m-%d").to_string();
                    let sell_time = now.format("%H:%M:%S").to_string();
                    let sell_timestamp_utc = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
                    let forward_days = configured_wash_sale_forward_block_days();
                    let window_end = (now + chrono::Duration::days(forward_days))
                        .format("%Y-%m-%d")
                        .to_string();
                    conn.execute(
                        "INSERT OR REPLACE INTO wash_sale_tracker (
                            symbol, sell_date, sell_time, sell_timestamp_utc, event_timezone,
                            sell_price, loss_amount, wash_window_end, status, provider, account_ref,
                            paper_account
                         )
                         VALUES (?1, ?2, ?3, ?4, 'UTC', ?5, ?6, ?7, 'active', ?8, ?9, ?10)",
                        params![
                            sym,
                            sell_date,
                            sell_time,
                            sell_timestamp_utc,
                            cur,
                            pnl.abs(),
                            window_end,
                            account.provider(),
                            account.account_ref(),
                            if account.is_paper() { 1 } else { 0 }
                        ],
                    )?;
                    println!(
                        "📝 Loss of {} — wash sale window until {} ({} IRS days + {} buffer)",
                        fmt_money(pnl.abs()),
                        window_end,
                        IRS_WASH_SALE_WINDOW_DAYS,
                        configured_wash_sale_safety_buffer_days()
                    );
                }
            }
            api_delete_text(
                &client,
                &alpaca::broker_api_url_for(account, &format!("/positions/{}", sym)),
            )
            .await?;
            println!(
                "✅ Position in {} closed for {}:{}.",
                sym,
                account.provider(),
                account.account_ref()
            );
        }
    }
    if let Err(err) = auto::sync_orders_all_accounts(true).await {
        eprintln!(
            "warning: provider order/fill sync after close failed: {}",
            err
        );
    }
    Ok(())
}

// ══════════════════════════════════════════════════════════════════
//  DATA COMMANDS
// ══════════════════════════════════════════════════════════════════

async fn cmd_quote(symbol: String, json_out: bool) -> anyhow::Result<()> {
    let sym = symbol.to_uppercase();
    check_blocked(&sym)?;

    let client = build_client();

    if json_out {
        // For JSON mode, fetch both quote and snapshot for rich data
        let mut obj = serde_json::json!({"symbol": sym});
        // Quote
        if let Ok(qr) = api_get_stock_quote(&client, &sym).await {
            if let Some(q) = qr.quote {
                obj["bid_price"] = serde_json::json!(q.bid_price.unwrap_or(0.0));
                obj["bid_size"] = serde_json::json!(q.bid_size.unwrap_or(0.0) as u64);
                obj["ask_price"] = serde_json::json!(q.ask_price.unwrap_or(0.0));
                obj["ask_size"] = serde_json::json!(q.ask_size.unwrap_or(0.0) as u64);
            }
        }
        // Snapshot for daily bar data
        if let Ok(snap) = api_get_stock_snapshot(&client, &sym).await {
            let price = snap.latest_trade.as_ref().and_then(|t| t.p).unwrap_or(0.0);
            obj["current_price"] = serde_json::json!(price);
            if let Some(db) = &snap.daily_bar {
                obj["day_high"] = serde_json::json!(db.h.unwrap_or(0.0));
                obj["day_low"] = serde_json::json!(db.l.unwrap_or(0.0));
                obj["day_volume"] = serde_json::json!(db.v.unwrap_or(0));
            }
            if let Some(pb) = &snap.prev_daily_bar {
                let prev_close = pb.c.unwrap_or(0.0);
                obj["prev_close"] = serde_json::json!(prev_close);
                if prev_close > 0.0 {
                    obj["change_pct"] =
                        serde_json::json!((price - prev_close) / prev_close * 100.0);
                }
            }
        }
        print_json_pretty(obj)?;
        return Ok(());
    }

    // Try stock first
    match api_get_stock_quote(&client, &sym).await {
        Ok(qr) => {
            let q = qr.quote.unwrap_or(QuoteData {
                bid_price: None,
                ask_price: None,
                bid_size: None,
                ask_size: None,
            });
            println!("📊 {} (Stock)", sym);
            println!(
                "  Bid:  {} x {}",
                fmt_money(q.bid_price.unwrap_or(0.0)),
                q.bid_size.unwrap_or(0.0) as u64
            );
            println!(
                "  Ask:  {} x {}",
                fmt_money(q.ask_price.unwrap_or(0.0)),
                q.ask_size.unwrap_or(0.0) as u64
            );
            if let (Some(bp), Some(ap)) = (q.bid_price, q.ask_price) {
                if bp > 0.0 && ap > 0.0 {
                    println!("  Mid:  {}", fmt_money((bp + ap) / 2.0));
                }
            }
        }
        Err(_) => {
            // Try crypto
            let pair = if sym.contains('/') {
                sym.clone()
            } else {
                format!("{}/USD", sym)
            };
            let encoded = pair.replace("/", "%2F");
            match api_get::<serde_json::Value>(
                &client,
                &format!(
                    "{}/v1beta3/crypto/us/latest/quotes?symbols={}",
                    alpaca::DATA_URL,
                    encoded
                ),
            )
            .await
            {
                Ok(data) => {
                    if let Some(quotes) = data.get("quotes").and_then(|q| q.as_object()) {
                        if let Some((_k, v)) = quotes.iter().next() {
                            println!("₿ {} (Crypto)", sym);
                            println!("  Bid:  {}", fmt_money(v["bp"].as_f64().unwrap_or(0.0)));
                            println!("  Ask:  {}", fmt_money(v["ap"].as_f64().unwrap_or(0.0)));
                        } else {
                            println!("No quote found for {}", sym);
                        }
                    } else {
                        println!("No quote found for {}", sym);
                    }
                }
                Err(e) => println!("No quote found for {}: {}", sym, e),
            }
        }
    }
    Ok(())
}

// Handles the data feed CLI action.
fn cmd_data_feed(json_out: bool) -> anyhow::Result<()> {
    let mode = alpaca::data_feed_mode();
    let feeds = alpaca::data_feeds();
    if json_out {
        print_json_pretty(serde_json::json!({
            "active_mode": mode,
            "effective_order": feeds,
            "modes": {
                "auto": "Default. Try SIP first, then IEX fallback if SIP fails.",
                "sip": "Force SIP only. Paid consolidated U.S. stock/ETF market data; no fallback.",
                "iex": "Force IEX only. Free IEX-exchange stock/ETF data; no fallback."
            },
            "sip": {
                "coverage": "Consolidated U.S. exchanges and alternative trading venues.",
                "use_case": "Paid/live/high-volume or latency-sensitive trading where NBBO and full market visibility matter."
            },
            "iex": {
                "coverage": "IEX exchange only.",
                "use_case": "Free paper testing, slower strategies, or cost-sensitive development."
            },
            "historical_note": "For stock bars, probe with `mlai-trade market history-start`; forced SIP on this account discovered daily bars back to 2016-01-04.",
            "configuration": "Set alpaca.accounts[].data_feed in ~/mlai-trade/config/mlai-trade.json. Values: auto, sip, iex."
        }))?;
        return Ok(());
    }

    println!("Alpaca Stock/ETF Data Feed");
    println!("  Active mode:     {}", mode);
    println!("  Effective order: {}", feeds.join(" -> "));
    println!();
    println!("Modes:");
    println!("  auto  Default. Try SIP first, then IEX fallback if SIP fails.");
    println!("  sip   Force SIP only; no fallback. Use this for paid consolidated data rebuilds.");
    println!("  iex   Force IEX only; no fallback. Use this for free-tier/testing scenarios.");
    println!();
    println!("SIP:");
    println!("  Paid consolidated U.S. stock/ETF feed across exchanges and alternative venues.");
    println!("  Use when full market visibility, NBBO context, and slippage control matter.");
    println!();
    println!("IEX:");
    println!("  Free real-time stock/ETF feed limited to IEX exchange prints/quotes.");
    println!("  Useful for paper tests, low-frequency strategies, or cost-sensitive development.");
    println!();
    println!(
        "Historical: run `mlai-trade market history-start` to query Alpaca for the first available daily bar."
    );
    println!("Configuration: set alpaca.accounts[].data_feed in mlai-trade.json to auto|sip|iex");
    Ok(())
}

// Handles the watch CLI action.
async fn cmd_watch(symbols: Vec<String>) -> anyhow::Result<()> {
    let client = build_client();
    println!(
        "{:<8} {:>10} {:>10} {:>8} {:>12} {:>10} {:>10}",
        "Symbol", "Price", "Change", "Change%", "Volume", "High", "Low"
    );
    println!("{}", "-".repeat(72));
    for s in &symbols {
        let sym = s.to_uppercase();
        if is_blocked(&sym) {
            println!("{:<8} ⛔ BLOCKED (configured)", sym);
            continue;
        }
        match api_get_stock_snapshot(&client, &sym).await {
            Ok(snap) => {
                let price = snap
                    .latest_trade
                    .as_ref()
                    .and_then(|t| t.p)
                    .or(snap.daily_bar.as_ref().and_then(|b| b.c))
                    .unwrap_or(0.0);
                let prev_close = snap
                    .prev_daily_bar
                    .as_ref()
                    .and_then(|b| b.c)
                    .unwrap_or(0.0);
                let change = if prev_close > 0.0 {
                    price - prev_close
                } else {
                    0.0
                };
                let change_pct = if prev_close > 0.0 {
                    change / prev_close * 100.0
                } else {
                    0.0
                };
                let vol = snap.daily_bar.as_ref().and_then(|b| b.v).unwrap_or(0);
                let high = snap.daily_bar.as_ref().and_then(|b| b.h).unwrap_or(0.0);
                let low = snap.daily_bar.as_ref().and_then(|b| b.l).unwrap_or(0.0);
                println!(
                    "{:<8} {:>10} {:>10} {:>+7.2}% {:>12} {:>10} {:>10}",
                    sym,
                    fmt_money(price),
                    fmt_money(change),
                    change_pct,
                    format!("{}", vol),
                    fmt_money(high),
                    fmt_money(low)
                );
            }
            Err(e) => println!("{:<8} — {}", sym, e),
        }
    }
    Ok(())
}

// Handles the bars single CLI action.
async fn cmd_bars_single(symbol: String, timeframe: String, limit: u32) -> anyhow::Result<()> {
    let sym = symbol.to_uppercase();
    let now = Utc::now();
    let start =
        if timeframe.contains("Day") || timeframe.contains("Week") || timeframe.contains("Month") {
            (now - chrono::Duration::days((limit as i64 * 7).max(90)))
                .format("%Y-%m-%d")
                .to_string()
        } else {
            (now - chrono::Duration::days(7))
                .format("%Y-%m-%d")
                .to_string()
        };

    let client = build_client();
    let mut stock_error: Option<anyhow::Error> = None;
    let mut bars: Vec<Bar> = Vec::new();
    for feed in alpaca::data_feeds() {
        let url = format!(
            "{}/v2/stocks/{}/bars?timeframe={}&limit={}&sort=desc&start={}&feed={}",
            alpaca::DATA_URL,
            sym,
            timeframe,
            limit,
            start,
            feed
        );
        match api_get::<SingleBarsResponse>(&client, &url).await {
            Ok(r) => {
                bars = r.bars.unwrap_or_default();
                if !bars.is_empty() {
                    break;
                }
            }
            Err(err) => {
                eprintln!("  warning: stock bars feed '{}' failed: {}", feed, err);
                stock_error = Some(err);
            }
        }
    }

    if bars.is_empty() {
        bars = match stock_error {
            Some(_) => {
                // Try crypto
                let pair = if sym.contains('/') {
                    sym.clone()
                } else {
                    format!("{}/USD", sym)
                };
                let encoded = pair.replace("/", "%2F");
                let crypto_url =
                    format!(
                    "{}/v1beta3/crypto/us/bars?symbols={}&timeframe={}&limit={}&sort=desc&start={}",
                    alpaca::DATA_URL, encoded, timeframe, limit, start
                );
                match api_get::<serde_json::Value>(&client, &crypto_url).await {
                    Ok(data) => {
                        if let Some(bars_obj) = data.get("bars").and_then(|b| b.as_object()) {
                            if let Some((_k, v)) = bars_obj.iter().next() {
                                serde_json::from_value(v.clone()).unwrap_or_default()
                            } else {
                                vec![]
                            }
                        } else {
                            vec![]
                        }
                    }
                    Err(_) => vec![],
                }
            }
            None => vec![],
        };
    }

    if bars.is_empty() {
        println!("No bars for {}", sym);
        return Ok(());
    }
    println!("📈 {} — {} bars (latest {})", sym, timeframe, limit);
    println!(
        "{:<12} {:>10} {:>10} {:>10} {:>10} {:>12}",
        "Date", "Open", "High", "Low", "Close", "Volume"
    );
    println!("{}", "-".repeat(68));
    for b in &bars {
        let ts = &b.t[..b.t.len().min(10)];
        println!(
            "{:<12} {:>10} {:>10} {:>10} {:>10} {:>12}",
            ts,
            fmt_money(b.o),
            fmt_money(b.h),
            fmt_money(b.l),
            fmt_money(b.c),
            format!("{}", b.v)
        );
    }
    Ok(())
}

// Handles the news CLI action.
async fn cmd_news(symbol: Option<String>, limit: u32, json_out: bool) -> anyhow::Result<()> {
    let client = build_client();
    let mut url = format!("{}/v1beta1/news?limit={}", alpaca::DATA_URL, limit);
    if let Some(ref s) = symbol {
        url.push_str(&format!("&symbols={}", s.to_uppercase()));
    }
    let data: NewsResponse = api_get(&client, &url).await?;
    let news = data.news.unwrap_or_default();

    if json_out {
        let items: Vec<serde_json::Value> = news.iter().map(|n| {
            serde_json::json!({
                "headline": n.headline.clone().unwrap_or_default(),
                "source": n.source.clone().unwrap_or_default(),
                "summary": n.summary.clone().unwrap_or_default(),
                "created_at": n.created_at.as_deref().unwrap_or("").chars().take(10).collect::<String>(),
                "symbols": n.symbols.clone().unwrap_or_default(),
                "url": n.url.clone().unwrap_or_default(),
            })
        }).collect();
        print_json_pretty(serde_json::json!({"news": items}))?;
        return Ok(());
    }

    if news.is_empty() {
        println!("No news found.");
        return Ok(());
    }
    for n in &news {
        let ts = n.created_at.as_deref().unwrap_or("?");
        let ts_short = if ts.len() >= 10 { &ts[..10] } else { ts };
        let syms = n.symbols.as_ref().map(|s| s.join(", ")).unwrap_or_default();
        println!("📰 [{}] {}", ts_short, n.headline.as_deref().unwrap_or("?"));
        if !syms.is_empty() {
            println!("   Symbols: {}", syms);
        }
        println!("   Source: {}", n.source.as_deref().unwrap_or("N/A"));
        if let Some(summary) = &n.summary {
            let trunc = if summary.len() > 200 {
                &summary[..200]
            } else {
                summary
            };
            println!("   {}", trunc);
        }
        println!();
    }
    Ok(())
}

// Fetches fred series from the remote source.
async fn fetch_fred_series(
    client: &reqwest::Client,
    api_key: &str,
    series_id: &str,
    start: &str,
) -> anyhow::Result<Vec<(String, f64)>> {
    let url = reqwest::Url::parse_with_params(
        FRED_SERIES_OBSERVATIONS_URL,
        &[
            ("series_id", series_id),
            ("api_key", api_key),
            ("file_type", "json"),
            ("observation_start", start),
            ("sort_order", "asc"),
            ("limit", "100000"),
        ],
    )?;

    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("FRED API error {} for {}: {}", status, series_id, body);
    }

    let data: FredObservationsResponse = resp.json().await?;
    let rows = data
        .observations
        .into_iter()
        .filter_map(|obs| obs.value.parse::<f64>().ok().map(|value| (obs.date, value)))
        .collect();
    Ok(rows)
}

// Handles the sp500 CLI action.
async fn cmd_sp500(days: u32, json_out: bool) -> anyhow::Result<()> {
    let api_key = config::fred_api_key()?;
    let start = if days == 0 {
        alpaca::FULL_HISTORY_PROBE_START.to_string()
    } else {
        (Utc::now() - chrono::Duration::days(days as i64))
            .format("%Y-%m-%d")
            .to_string()
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let series = [
        (FRED_SP500_SERIES_ID, "S&P 500"),
        (FRED_VIX_SERIES_ID, "CBOE Volatility Index"),
    ];

    let mut conn = open_db()?;
    let tx = conn.transaction()?;
    let updated_at = Utc::now().to_rfc3339();
    let mut summaries = Vec::new();
    let progress = progress::bar_if(!json_out, series.len() as u64, "FRED market benchmarks");

    for (series_id, label) in series {
        progress.set_message(format!("fetching {series_id}"));
        let rows = fetch_fred_series(&client, &api_key, series_id, &start).await?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO macro_series (series_id, date, value, source, updated_at)
                 VALUES (?1, ?2, ?3, 'FRED', ?4)",
            )?;
            for (date, value) in &rows {
                stmt.execute(params![series_id, date, value, updated_at])?;
            }
        }
        let first_date = rows
            .first()
            .map(|(date, _)| date.clone())
            .unwrap_or_else(|| "none".to_string());
        let latest = rows
            .last()
            .map(|(date, value)| (date.clone(), *value))
            .unwrap_or_else(|| ("none".to_string(), 0.0));
        summaries.push((series_id, label, rows.len(), first_date, latest.0, latest.1));
        progress.inc(1);
        progress.set_message(format!("{series_id}: {} rows", rows.len()));
    }
    tx.commit()?;
    progress.finish_and_clear();

    if json_out {
        let series_json: Vec<_> = summaries
            .iter()
            .map(
                |(series_id, label, rows, first_date, latest_date, latest_value)| {
                    serde_json::json!({
                        "series_id": series_id,
                        "label": label,
                        "source": "FRED",
                        "rows_stored": rows,
                        "first_date": first_date,
                        "latest_date": latest_date,
                        "latest_value": latest_value,
                    })
                },
            )
            .collect();
        print_json_pretty(serde_json::json!({ "series": series_json }))?;
        return Ok(());
    }

    println!("Market benchmarks synced from FRED");
    for (series_id, label, rows, first_date, latest_date, latest_value) in summaries {
        println!("  {} ({})", label, series_id);
        println!("    Rows stored: {}", rows);
        println!("    First date:  {}", first_date);
        println!("    Latest:      {} = {:.2}", latest_date, latest_value);
    }
    println!("  Table:        macro_series");
    Ok(())
}

#[derive(Clone, Debug)]
struct HistoryProbeResult {
    feed: String,
    symbol: String,
    first_date: Option<String>,
    error: Option<String>,
}

#[derive(Clone, Debug)]
struct HistoryDiscovery {
    start_date: String,
    feed_mode: String,
    probes: Vec<HistoryProbeResult>,
}

// Normalizes bar date into canonical form.
fn normalize_bar_date(raw: &str) -> String {
    raw.get(0..10).unwrap_or(raw).to_string()
}

// Handles history probe symbols logic.
fn history_probe_symbols(symbols: Vec<String>) -> Vec<String> {
    let mut out = if symbols.is_empty() {
        HISTORY_PROBE_SYMBOLS
            .iter()
            .map(|symbol| (*symbol).to_string())
            .collect::<Vec<_>>()
    } else {
        symbols
    };
    out.extend(
        MARKET_BENCHMARK_SYMBOLS
            .iter()
            .map(|symbol| (*symbol).to_string()),
    );
    out.sort_by_key(|symbol| symbol.to_ascii_uppercase());
    out.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    out
}

// Returns oldest stock bar for feed from provider data.
async fn oldest_stock_bar_for_feed(
    client: &reqwest::Client,
    symbol: &str,
    feed: &str,
) -> anyhow::Result<Option<String>> {
    let end = (Utc::now() + Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let url = format!(
        "{}/v2/stocks/{}/bars?timeframe=1Day&start={}&end={}&limit=1&sort=asc&feed={}",
        alpaca::DATA_URL,
        symbol,
        alpaca::FULL_HISTORY_PROBE_START,
        end,
        feed
    );
    let response: SingleBarsResponse = api_get(client, &url).await?;
    Ok(response
        .bars
        .unwrap_or_default()
        .first()
        .map(|bar| normalize_bar_date(&bar.t)))
}

// Discovers alpaca stock history start from provider data.
async fn discover_alpaca_stock_history_start(
    client: &reqwest::Client,
    symbols: Vec<String>,
    show_progress: bool,
) -> anyhow::Result<HistoryDiscovery> {
    let feed_mode = alpaca::data_feed_mode();
    let feeds = alpaca::data_feeds();
    let symbols = history_probe_symbols(symbols);
    let mut probes = Vec::new();
    let mut earliest: Option<String> = None;
    let progress = progress::bar_if(
        show_progress,
        (feeds.len() * symbols.len()) as u64,
        "Probing Alpaca history start",
    );

    for feed in &feeds {
        for symbol in &symbols {
            progress.set_message(format!("{feed}:{symbol}"));
            match oldest_stock_bar_for_feed(client, symbol, feed).await {
                Ok(first_date) => {
                    if let Some(date) = &first_date {
                        if earliest.as_ref().map_or(true, |current| date < current) {
                            earliest = Some(date.clone());
                        }
                    }
                    probes.push(HistoryProbeResult {
                        feed: feed.clone(),
                        symbol: symbol.clone(),
                        first_date,
                        error: None,
                    });
                }
                Err(err) => probes.push(HistoryProbeResult {
                    feed: feed.clone(),
                    symbol: symbol.clone(),
                    first_date: None,
                    error: Some(err.to_string()),
                }),
            }
            progress.inc(1);
        }
    }
    progress.finish_and_clear();

    let Some(start_date) = earliest else {
        let errors = probes
            .iter()
            .filter_map(|probe| {
                probe
                    .error
                    .as_ref()
                    .map(|err| format!("{}:{} {}", probe.feed, probe.symbol, err))
            })
            .take(5)
            .collect::<Vec<_>>()
            .join("; ");
        anyhow::bail!(
            "Could not discover earliest Alpaca stock bar date for feed mode '{}'. {}",
            feed_mode,
            errors
        );
    };

    Ok(HistoryDiscovery {
        start_date,
        feed_mode,
        probes,
    })
}

// Handles the history start CLI action.
async fn cmd_history_start(symbols: Vec<String>, json_out: bool) -> anyhow::Result<()> {
    let client = build_client();
    let discovery = discover_alpaca_stock_history_start(&client, symbols, !json_out).await?;
    if json_out {
        let probes = discovery
            .probes
            .iter()
            .map(|probe| {
                serde_json::json!({
                    "feed": probe.feed,
                    "symbol": probe.symbol,
                    "first_date": probe.first_date,
                    "error": probe.error,
                })
            })
            .collect::<Vec<_>>();
        print_json_pretty(serde_json::json!({
            "feed_mode": discovery.feed_mode,
            "feeds": alpaca::data_feeds(),
            "start_date": discovery.start_date,
            "probe_start": alpaca::FULL_HISTORY_PROBE_START,
            "probes": probes,
        }))?;
        return Ok(());
    }

    println!("Alpaca stock history probe");
    println!("  Feed mode:    {}", discovery.feed_mode);
    println!("  Feeds tried:   {}", alpaca::data_feeds().join(", "));
    println!("  Probe start:   {}", alpaca::FULL_HISTORY_PROBE_START);
    println!("  Earliest date: {}", discovery.start_date);
    for probe in &discovery.probes {
        match (&probe.first_date, &probe.error) {
            (Some(date), _) => println!("    {:<4} {:<6} {}", probe.feed, probe.symbol, date),
            (_, Some(err)) => println!("    {:<4} {:<6} error: {}", probe.feed, probe.symbol, err),
            _ => println!("    {:<4} {:<6} no bars", probe.feed, probe.symbol),
        }
    }
    Ok(())
}

// Handles the clock CLI action.
async fn cmd_clock(json_out: bool) -> anyhow::Result<()> {
    let account = config::alpaca_primary_account()?;
    let markets = config::auto_market_provider_markets();
    let client = build_client();
    let clock: alpaca::TradingClockResponse =
        api_get(&client, &alpaca::clock_v3_url_for(&account, &markets)).await?;

    if json_out {
        print_json_pretty(serde_json::json!({
            "provider": account.provider(),
            "account_ref": account.account_ref(),
            "account_mode": alpaca::account_mode_for(&account),
            "endpoint": "v3/clock",
            "markets": markets,
            "clocks": clock.clocks,
        }))?;
        return Ok(());
    }

    println!("Alpaca market clock (v3)");
    println!(
        "  Account: {}:{}",
        account.provider(),
        account.account_ref()
    );
    println!("  Markets: {}", markets.join(", "));
    if clock.clocks.is_empty() {
        println!("  No provider clocks returned.");
    }
    for market_clock in &clock.clocks {
        let label = market_clock.market_label();
        let status = if market_clock.may_be_trading() {
            "OPEN"
        } else {
            "CLOSED"
        };
        let timezone = market_clock
            .market
            .as_ref()
            .and_then(|market| market.timezone.as_deref())
            .unwrap_or("?");
        println!(
            "  {:<8} {:<6} phase={} market_day={} timezone={}",
            label,
            status,
            market_clock.phase.as_deref().unwrap_or("?"),
            market_clock.is_market_day.unwrap_or(false),
            timezone
        );
        println!(
            "           timestamp={} next_open={} next_close={}",
            market_clock.timestamp.as_deref().unwrap_or("?"),
            market_clock.next_market_open.as_deref().unwrap_or("?"),
            market_clock.next_market_close.as_deref().unwrap_or("?")
        );
    }
    Ok(())
}

// Handles the calendar CLI action.
async fn cmd_calendar(
    start: Option<String>,
    end: Option<String>,
    markets: Vec<String>,
    json_out: bool,
) -> anyhow::Result<()> {
    let account = config::alpaca_primary_account()?;
    let market_timezone = configured_market_timezone_name();
    let calendar_timezone = "UTC";
    let start = start.unwrap_or_else(utc_today);
    let end = end.unwrap_or_else(|| start.clone());
    let markets = if markets.is_empty() {
        config::auto_market_provider_markets()
    } else {
        markets
    };
    let client = build_client();
    let mut responses = Vec::new();
    for market in &markets {
        let response: alpaca::TradingCalendarResponse = api_get(
            &client,
            &alpaca::calendar_v3_url_for(&account, market, &start, &end, calendar_timezone),
        )
        .await?;
        responses.push(serde_json::json!({
            "market": market,
            "provider_market": response.market,
            "calendar": response.calendar,
        }));
    }

    if json_out {
        print_json_pretty(serde_json::json!({
            "provider": account.provider(),
            "account_ref": account.account_ref(),
            "account_mode": alpaca::account_mode_for(&account),
            "endpoint": "v3/calendar/{market}",
            "calendar_timezone": calendar_timezone,
            "local_market_timezone": market_timezone,
            "start": start,
            "end": end,
            "markets": responses,
        }))?;
        return Ok(());
    }

    println!("Alpaca market calendar (v3)");
    println!(
        "  Account:  {}:{}",
        account.provider(),
        account.account_ref()
    );
    println!("  Calendar timezone: {}", calendar_timezone);
    println!("  Local guardrail timezone: {}", market_timezone);
    println!("  Window:   {} to {}", start, end);
    for response in &responses {
        println!("\n  Market: {}", response["market"].as_str().unwrap_or("?"));
        let days = response["calendar"].as_array().cloned().unwrap_or_default();
        if days.is_empty() {
            println!("    No sessions returned.");
            continue;
        }
        for day in &days {
            println!(
                "    {} core {} - {} pre {} - {} post {} - {} settlement {}",
                day["date"].as_str().unwrap_or("?"),
                day["core_start"].as_str().unwrap_or("-"),
                day["core_end"].as_str().unwrap_or("-"),
                day["pre_start"].as_str().unwrap_or("-"),
                day["pre_end"].as_str().unwrap_or("-"),
                day["post_start"].as_str().unwrap_or("-"),
                day["post_end"].as_str().unwrap_or("-"),
                day["settlement_date"].as_str().unwrap_or("-")
            );
        }
    }
    Ok(())
}

// ══════════════════════════════════════════════════════════════════
//  SCANNER COMMANDS
// ══════════════════════════════════════════════════════════════════

async fn cmd_universe() -> anyhow::Result<()> {
    let client = build_client();
    eprintln!("Fetching all tradable US equities...");
    let progress = progress::spinner("Alpaca asset universe");
    let resp = client
        .get(alpaca::broker_api_url(
            "/assets?status=active&asset_class=us_equity",
        ))
        .send()
        .await?;
    progress.set_message("parsing response");
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("API error {}: {}", status, body);
    }
    let assets: Vec<Asset> = resp.json().await?;
    let tradable: Vec<&Asset> = assets
        .iter()
        .filter(|a| a.tradable == Some(true))
        .filter(|a| !is_blocked(&a.symbol))
        .collect();
    let blocked_count = assets.iter().filter(|a| is_blocked(&a.symbol)).count();

    let conn = open_db()?;
    let now = Utc::now().to_rfc3339();
    let tx = conn.unchecked_transaction()?;
    progress.set_message(format!("storing {} tradable assets", tradable.len()));
    for a in &tradable {
        tx.execute(
            "INSERT OR REPLACE INTO assets (symbol,name,exchange,status,tradable,fractionable,shortable,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![a.symbol, a.name, a.exchange, a.status, a.tradable.unwrap_or(false) as i32, a.fractionable.unwrap_or(false) as i32, a.shortable.unwrap_or(false) as i32, now],
        )?;
    }
    tx.commit()?;
    progress.finish_and_clear();

    println!(
        "✅ {} tradable US equities stored (of {} total, {} blocked)",
        tradable.len(),
        assets.len(),
        blocked_count
    );
    let mut exchanges: HashMap<String, usize> = HashMap::new();
    for a in &tradable {
        *exchanges
            .entry(a.exchange.clone().unwrap_or_else(|| "UNKNOWN".into()))
            .or_insert(0) += 1;
    }
    let mut ex_list: Vec<_> = exchanges.into_iter().collect();
    ex_list.sort_by(|a, b| b.1.cmp(&a.1));
    for (ex, count) in &ex_list {
        println!("  {}: {}", ex, count);
    }
    if blocked_count > 0 {
        println!("  ⛔ Blocked: {:?}", config::blocked_symbols());
    }
    Ok(())
}

// Returns the next date value.
fn next_date(date: &str) -> anyhow::Result<String> {
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d")?;
    Ok((date + Duration::days(1)).format("%Y-%m-%d").to_string())
}

// Handles collapse missing dates logic.
fn collapse_missing_dates(dates: &[String]) -> anyhow::Result<Vec<(String, String)>> {
    if dates.is_empty() {
        return Ok(Vec::new());
    }

    let mut ranges = Vec::new();
    let mut start = NaiveDate::parse_from_str(&dates[0], "%Y-%m-%d")?;
    let mut previous = start;

    for raw in dates.iter().skip(1) {
        let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")?;
        if date.signed_duration_since(previous).num_days() > 4 {
            ranges.push((
                start.format("%Y-%m-%d").to_string(),
                (previous + Duration::days(1))
                    .format("%Y-%m-%d")
                    .to_string(),
            ));
            start = date;
        }
        previous = date;
    }

    ranges.push((
        start.format("%Y-%m-%d").to_string(),
        (previous + Duration::days(1))
            .format("%Y-%m-%d")
            .to_string(),
    ));
    Ok(ranges)
}

// Handles scan ranges logic.
fn scan_ranges(
    conn: &Connection,
    start_date: &str,
    today: &str,
    force: bool,
) -> anyhow::Result<Vec<(String, String)>> {
    let tomorrow = next_date(today)?;
    if force {
        return Ok(vec![(start_date.to_string(), tomorrow)]);
    }

    let range: (Option<String>, Option<String>) =
        conn.query_row("SELECT MIN(date), MAX(date) FROM bars", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;

    let Some(min_bar_date) = range.0 else {
        return Ok(vec![(start_date.to_string(), tomorrow)]);
    };
    let max_bar_date = range.1.unwrap_or_else(|| min_bar_date.clone());

    let mut ranges = Vec::new();
    if min_bar_date.as_str() > start_date {
        ranges.push((start_date.to_string(), min_bar_date.clone()));
    }

    let expected_dates: Vec<String> = conn
        .prepare(
            "SELECT date FROM macro_series
             WHERE series_id = ?1 AND date >= ?2 AND date <= ?3
             ORDER BY date",
        )
        .and_then(|mut stmt| {
            let rows = stmt.query_map(params![FRED_SP500_SERIES_ID, start_date, today], |r| {
                r.get::<_, String>(0)
            })?;
            Ok(rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
        })
        .unwrap_or_default();

    if expected_dates.is_empty() {
        ranges.push((max_bar_date.clone(), tomorrow));
        return Ok(ranges);
    }

    let latest_expected = expected_dates
        .last()
        .cloned()
        .unwrap_or_else(|| today.to_string());

    let existing_dates: std::collections::HashSet<String> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT date FROM bars WHERE date >= ?1 AND date <= ?2 ORDER BY date",
        )?;
        let rows = stmt.query_map(params![start_date, today], |r| r.get::<_, String>(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    let missing_dates = expected_dates
        .iter()
        .filter(|date| {
            date.as_str() >= min_bar_date.as_str()
                && date.as_str() <= max_bar_date.as_str()
                && !existing_dates.contains(*date)
        })
        .cloned()
        .collect::<Vec<_>>();
    ranges.extend(collapse_missing_dates(&missing_dates)?);

    if max_bar_date <= latest_expected {
        ranges.push((max_bar_date.clone(), next_date(&latest_expected)?));
    }

    ranges.sort();
    ranges.dedup();
    Ok(ranges)
}

// Prints bar coverage in human-readable form.
fn print_bar_coverage(conn: &Connection, label: &str) -> anyhow::Result<()> {
    let (oldest, newest, rows): (Option<String>, Option<String>, i64) =
        conn.query_row("SELECT MIN(date), MAX(date), COUNT(*) FROM bars", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;

    match (oldest, newest) {
        (Some(oldest), Some(newest)) => {
            println!("{label}: {oldest} -> {newest} ({rows} rows)");
        }
        _ => {
            println!("{label}: empty");
        }
    }
    Ok(())
}

// Handles the scan CLI action.
async fn cmd_scan(days: u32, force: bool) -> anyhow::Result<()> {
    let conn = open_db()?;
    let client = Arc::new(build_client());
    let mut symbols: Vec<String> = {
        let mut stmt =
            conn.prepare("SELECT symbol FROM assets WHERE tradable=1 ORDER BY symbol")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.filter_map(|r| r.ok())
            .filter(|s| !is_blocked(s))
            .collect()
    };
    for symbol in MARKET_BENCHMARK_SYMBOLS {
        if !symbols.iter().any(|existing| existing == symbol) {
            symbols.push((*symbol).to_string());
        }
    }
    symbols.sort();
    symbols.dedup();
    if symbols.is_empty() {
        println!("❌ No assets in DB. Run `mlai-trade data universe` first.");
        return Ok(());
    }

    let start_date = if days == 0 {
        let discovery =
            discover_alpaca_stock_history_start(client.as_ref(), Vec::new(), true).await?;
        println!(
            "Discovered Alpaca stock history start for feed mode '{}': {}",
            discovery.feed_mode, discovery.start_date
        );
        discovery.start_date
    } else {
        (Utc::now() - Duration::days(days as i64))
            .format("%Y-%m-%d")
            .to_string()
    };
    let today = Utc::now().format("%Y-%m-%d").to_string();
    print_bar_coverage(&conn, "Local bar coverage before scan")?;
    let ranges = scan_ranges(&conn, &start_date, &today, force)?;
    if ranges.is_empty() {
        if days == 0 {
            println!("✅ Bars already up to date for full available Alpaca history");
        } else {
            println!(
                "✅ Bars already up to date for the requested {} day window",
                days
            );
        }
        print_bar_coverage(&conn, "Local bar coverage after scan")?;
        return Ok(());
    }

    let total = symbols.len();
    let default_concurrent = if days == 0 || days > 365 {
        1
    } else {
        MAX_CONCURRENT
    };
    let max_concurrent = config::scan_max_concurrent(default_concurrent);
    let max_retries = config::scan_max_retries(8);
    println!(
        "Fetching missing bar ranges for {} symbols ({} ranges, batches of {}, concurrency {})...",
        total,
        ranges.len(),
        BATCH_SIZE,
        max_concurrent
    );
    for (start, end) in &ranges {
        println!("  Range: {} -> {} (end exclusive)", start, end);
    }

    let feeds = alpaca::data_feeds();
    let mut done_symbols = 0usize;
    let mut bar_count = 0usize;
    let mut symbols_with_bars = 0usize;
    let progress = progress::bar(
        (total as u64).saturating_mul(ranges.len() as u64),
        "Alpaca bar sync",
    );

    for (range_start, range_end) in ranges {
        progress.set_message(format!("{range_start} -> {range_end}"));
        for wave in symbols.chunks(BATCH_SIZE * max_concurrent) {
            let mut handles = Vec::new();

            for batch in wave.chunks(BATCH_SIZE) {
                let client = Arc::clone(&client);
                let start = range_start.clone();
                let end = range_end.clone();
                let feeds = feeds.clone();
                let batch: Vec<String> = batch.to_vec();

                let handle = tokio::spawn(async move {
                    let syms_str = batch.join(",");
                    let mut page_token: Option<String> = None;
                    let mut batch_bars: Vec<(String, Vec<Bar>)> = Vec::new();

                    loop {
                        let mut data: Option<BarsResponse> = None;
                        let mut last_error: Option<String> = None;
                        for feed in &feeds {
                            let mut url = format!(
                                "{}/v2/stocks/bars?symbols={}&timeframe=1Day&start={}&end={}&limit=10000&feed={}",
                                alpaca::DATA_URL, syms_str, start, end, feed
                            );
                            if let Some(ref token) = page_token {
                                url.push_str(&format!("&page_token={}", token));
                            }

                            for attempt in 0..=max_retries {
                                let resp = match client.get(&url).send().await {
                                    Ok(r) => r,
                                    Err(e) => {
                                        last_error = Some(e.to_string());
                                        if attempt < max_retries {
                                            let delay = 2u64.pow(attempt.min(5) as u32);
                                            eprintln!(
                                                "  ⚠️  Request error on feed '{}': {}; retrying in {}s",
                                                feed, e, delay
                                            );
                                            tokio::time::sleep(std::time::Duration::from_secs(
                                                delay,
                                            ))
                                            .await;
                                            continue;
                                        }
                                        break;
                                    }
                                };

                                if !resp.status().is_success() {
                                    let status = resp.status();
                                    let body = resp.text().await.unwrap_or_default();
                                    last_error = Some(format!(
                                        "API {} after {} retries: {}",
                                        status,
                                        attempt,
                                        &body[..body.len().min(200)]
                                    ));
                                    if status.as_u16() == 429 && attempt < max_retries {
                                        let delay = 2u64.pow(attempt.min(5) as u32);
                                        eprintln!(
                                            "  ⚠️  API 429 rate limit on feed '{}'; retrying in {}s",
                                            feed, delay
                                        );
                                        tokio::time::sleep(std::time::Duration::from_secs(delay))
                                            .await;
                                        continue;
                                    }
                                    break;
                                }

                                match resp.json::<BarsResponse>().await {
                                    Ok(parsed) => {
                                        data = Some(parsed);
                                        break;
                                    }
                                    Err(e) if attempt < max_retries => {
                                        last_error = Some(e.to_string());
                                        let delay = 2u64.pow(attempt.min(5) as u32);
                                        eprintln!(
                                            "  ⚠️  Parse error on feed '{}': {}; retrying in {}s",
                                            feed, e, delay
                                        );
                                        tokio::time::sleep(std::time::Duration::from_secs(delay))
                                            .await;
                                    }
                                    Err(e) => {
                                        last_error = Some(e.to_string());
                                        break;
                                    }
                                }
                            }

                            if data.is_some() {
                                break;
                            }

                            if feeds.len() > 1 {
                                eprintln!(
                                    "  ⚠️  Feed '{}' failed for batch; trying fallback feed",
                                    feed
                                );
                            }
                        }

                        let data = data.ok_or_else(|| {
                            anyhow::anyhow!(
                                "no response data after feed fallback/retries: {}",
                                last_error.unwrap_or_else(|| "unknown error".to_string())
                            )
                        })?;
                        if let Some(bars_map) = data.bars {
                            for (sym, bars) in bars_map {
                                if !is_blocked(&sym) {
                                    batch_bars.push((sym, bars));
                                }
                            }
                        }
                        match data.next_page_token {
                            Some(token) if !token.is_empty() => {
                                page_token = Some(token);
                                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            }
                            _ => break,
                        }
                    }

                    Ok::<_, anyhow::Error>((batch.len(), batch_bars))
                });
                handles.push(handle);
            }

            let tx = conn.unchecked_transaction()?;
            {
                let mut insert = tx.prepare("INSERT OR REPLACE INTO bars (symbol,date,open,high,low,close,volume,vwap) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)")?;
                for handle in handles {
                    let (batch_len, bars_data) = handle.await??;
                    done_symbols += batch_len;
                    symbols_with_bars += bars_data.len();

                    for (sym, bars) in bars_data {
                        for b in bars {
                            let date = &b.t[..10];
                            insert.execute(params![
                                sym, date, b.o, b.h, b.l, b.c, b.v as i64, b.vw
                            ])?;
                            bar_count += 1;
                        }
                    }
                }
            }
            tx.commit()?;
            progress.set_position(done_symbols as u64);
            progress.set_message(format!("{bar_count} bars stored"));
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
    }
    progress.finish_and_clear();

    println!(
        "✅ Stored {} bars for {} symbols",
        bar_count, symbols_with_bars
    );
    print_bar_coverage(&conn, "Local bar coverage after scan")?;
    Ok(())
}

// Handles the screen CLI action.
async fn cmd_screen(min_volume: u64, json_out: bool) -> anyhow::Result<()> {
    let conn = open_db()?;
    let latest_date: String = conn
        .query_row("SELECT MAX(date) FROM bars", [], |r| r.get(0))
        .unwrap_or_else(|_| "".into());
    if latest_date.is_empty() {
        if json_out {
            return print_json_pretty(serde_json::json!({
                "ok": false,
                "error": "No bar data",
                "status_code": 404,
                "next": "mlai-trade data scan"
            }));
        }
        println!("❌ No bar data. Run `mlai-trade data scan` first.");
        return Ok(());
    }
    if !json_out {
        println!(
            "Screening on data through {} (min avg volume: {})...",
            latest_date, min_volume
        );
    }

    let mut stmt = conn.prepare("SELECT DISTINCT symbol FROM bars WHERE date = ?1")?;
    let symbols: Vec<String> = stmt
        .query_map(params![latest_date], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .filter(|s| !is_blocked(s))
        .collect();
    if !json_out {
        println!("  Analyzing {} symbols...", symbols.len());
    }
    let progress = progress::bar_if(!json_out, symbols.len() as u64, "Screening symbols");

    struct ScreenResult {
        symbol: String,
        close: f64,
        change_pct: f64,
        volume_ratio: f64,
        signals: Vec<String>,
        best_confidence: Confidence,
    }
    let mut results: Vec<ScreenResult> = Vec::new();

    for (idx, sym) in symbols.iter().enumerate() {
        progress.set_position(idx as u64);
        progress.set_message(format!("{} signals", results.len()));
        let mut bstmt = conn.prepare("SELECT date,open,high,low,close,volume FROM bars WHERE symbol=?1 ORDER BY date DESC LIMIT 260")?;
        let bars: Vec<(String, f64, f64, f64, f64, i64)> = bstmt
            .query_map(params![sym], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .collect();

        if bars.len() < 2 {
            continue;
        }
        let today = &bars[0];
        let yesterday = &bars[1];
        let close = today.4;
        let prev_close = yesterday.4;
        let volume = today.5 as f64;
        if close <= 0.0 || prev_close <= 0.0 {
            continue;
        }
        let change_pct = ((close - prev_close) / prev_close) * 100.0;

        // Min volume filter
        if bars.len() >= 21 {
            let avg_vol_20d: f64 = bars[1..21].iter().map(|b| b.5 as f64).sum::<f64>() / 20.0;
            if (avg_vol_20d as u64) < min_volume {
                continue;
            }
        } else if min_volume > 0 {
            continue;
        }

        let mut signals: Vec<String> = Vec::new();

        // Volume spike
        let vol_ratio = if bars.len() >= 21 {
            let avg_vol: f64 = bars[1..21].iter().map(|b| b.5 as f64).sum::<f64>() / 20.0;
            let vr = if avg_vol > 0.0 { volume / avg_vol } else { 0.0 };
            if vr > 2.0 {
                signals.push(format!("VOL_SPIKE({:.1}x)", vr));
            }
            vr
        } else {
            0.0
        };

        // Gap up/down
        let gap_pct = ((today.1 - prev_close) / prev_close) * 100.0;
        if gap_pct > 2.0 {
            signals.push(format!("GAP_UP({:+.1}%)", gap_pct));
        } else if gap_pct < -2.0 {
            signals.push(format!("GAP_DOWN({:+.1}%)", gap_pct));
        }

        // New high/low
        let lookback = bars.len().min(260);
        if lookback >= 20 {
            let max_c = bars[1..lookback]
                .iter()
                .map(|b| b.4)
                .fold(f64::NEG_INFINITY, f64::max);
            let min_c = bars[1..lookback]
                .iter()
                .map(|b| b.4)
                .fold(f64::INFINITY, f64::min);
            if close > max_c {
                signals.push("NEW_HIGH".into());
            }
            if close < min_c {
                signals.push("NEW_LOW".into());
            }
        }

        // RSI(14)
        if bars.len() >= 15 {
            let (mut gains, mut losses) = (0.0f64, 0.0f64);
            for i in 0..14 {
                let diff = bars[i].4 - bars[i + 1].4;
                if diff > 0.0 {
                    gains += diff;
                } else {
                    losses += diff.abs();
                }
            }
            let rsi = if losses == 0.0 {
                100.0
            } else {
                100.0 - (100.0 / (1.0 + (gains / 14.0) / (losses / 14.0)))
            };
            if rsi > 70.0 {
                signals.push(format!("RSI_HIGH({:.0})", rsi));
            } else if rsi < 30.0 {
                signals.push(format!("RSI_LOW({:.0})", rsi));
            }
        }

        // MA crossover
        if bars.len() >= 51 {
            let sma10_t: f64 = bars[0..10].iter().map(|b| b.4).sum::<f64>() / 10.0;
            let sma50_t: f64 = bars[0..50].iter().map(|b| b.4).sum::<f64>() / 50.0;
            let sma10_y: f64 = bars[1..11].iter().map(|b| b.4).sum::<f64>() / 10.0;
            let sma50_y: f64 = bars[1..51].iter().map(|b| b.4).sum::<f64>() / 50.0;
            if sma10_t > sma50_t && sma10_y <= sma50_y {
                signals.push("MA_CROSS_UP".into());
            }
            if sma10_t < sma50_t && sma10_y >= sma50_y {
                signals.push("MA_CROSS_DOWN".into());
            }
        }

        // Big move
        if change_pct.abs() > 5.0 {
            signals.push(format!("BIG_MOVE({:+.1}%)", change_pct));
        }

        // Momentum 3M
        if bars.len() >= 63 {
            let p3m = bars[62].4;
            if p3m > 0.0 {
                let ret = ((close - p3m) / p3m) * 100.0;
                if ret > 20.0 {
                    signals.push(format!("MOMENTUM_3M({:+.1}%)", ret));
                }
            }
        }

        // Momentum 6M
        if bars.len() >= 126 {
            let p6m = bars[125].4;
            if p6m > 0.0 {
                let ret = ((close - p6m) / p6m) * 100.0;
                if ret > 30.0 {
                    signals.push(format!("MOMENTUM_6M({:+.1}%)", ret));
                }
            }
        }

        // Low volatility
        if bars.len() >= 21 {
            let returns: Vec<f64> = (0..20)
                .filter_map(|i| {
                    let (c1, c2) = (bars[i].4, bars[i + 1].4);
                    if c2 > 0.0 {
                        Some((c1 / c2).ln())
                    } else {
                        None
                    }
                })
                .collect();
            if returns.len() >= 15 {
                let mean = returns.iter().sum::<f64>() / returns.len() as f64;
                let var =
                    returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64;
                let annual_vol = var.sqrt() * (252.0_f64).sqrt() * 100.0;
                if annual_vol > 0.0 && annual_vol < 15.0 {
                    signals.push(format!("LOW_VOL({:.0}%)", annual_vol));
                }
            }
        }

        if !signals.is_empty() {
            let best = signals
                .iter()
                .map(|s| signal_confidence(s))
                .min_by_key(|c| match c {
                    Confidence::High => 0,
                    Confidence::Medium => 1,
                    Confidence::Low => 2,
                })
                .unwrap_or(Confidence::Medium);
            results.push(ScreenResult {
                symbol: sym.clone(),
                close,
                change_pct,
                volume_ratio: vol_ratio,
                signals,
                best_confidence: best,
            });
        }
    }
    progress.set_position(symbols.len() as u64);
    progress.finish_and_clear();

    results.sort_by(|a, b| {
        let ca = match a.best_confidence {
            Confidence::High => 0,
            Confidence::Medium => 1,
            Confidence::Low => 2,
        };
        let cb = match b.best_confidence {
            Confidence::High => 0,
            Confidence::Medium => 1,
            Confidence::Low => 2,
        };
        ca.cmp(&cb)
            .then(b.signals.len().cmp(&a.signals.len()))
            .then(
                b.change_pct
                    .abs()
                    .partial_cmp(&a.change_pct.abs())
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });

    let today_str = &latest_date;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM screen_results WHERE date=?1",
        params![today_str],
    )?;
    for r in &results {
        let sigs_json = serde_json::to_string(&r.signals).unwrap_or_default();
        tx.execute(
            "INSERT INTO screen_results (date,symbol,close,change_pct,volume_ratio,signals,confidence) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![today_str, r.symbol, r.close, r.change_pct, r.volume_ratio, sigs_json, r.best_confidence.label()],
        )?;
    }
    tx.commit()?;

    if json_out {
        let shown = results.len().min(50);
        let rows = results
            .iter()
            .take(shown)
            .map(|r| {
                serde_json::json!({
                    "symbol": r.symbol,
                    "close": r.close,
                    "change_pct": r.change_pct,
                    "volume_ratio": r.volume_ratio,
                    "signals": r.signals,
                    "confidence": r.best_confidence.label(),
                })
            })
            .collect::<Vec<_>>();
        return print_json_pretty(serde_json::json!({
            "ok": true,
            "date": latest_date,
            "min_volume": min_volume,
            "symbols_analyzed": symbols.len(),
            "stored": results.len(),
            "shown": shown,
            "results": rows,
        }));
    }

    if results.is_empty() {
        println!("No signals triggered.");
        return Ok(());
    }
    let show = results.len().min(50);
    println!(
        "\n{:<3} {:<8} {:>10} {:>9} {:>7} {:<4} {}",
        "Conf", "Symbol", "Close", "Change%", "VolR", "#", "Signals"
    );
    println!("{}", "-".repeat(90));
    for r in results.iter().take(show) {
        let sig_display: Vec<String> = r
            .signals
            .iter()
            .map(|s| format!("{}{}", signal_confidence(s).emoji(), s))
            .collect();
        println!(
            " {}  {:<8} {:>10.2} {:>+8.2}% {:>6.1}x {:<4} {}",
            r.best_confidence.emoji(),
            r.symbol,
            r.close,
            r.change_pct,
            r.volume_ratio,
            r.signals.len(),
            sig_display.join(", ")
        );
    }
    println!(
        "\n✅ {} symbols triggered signals (showing top {}). Saved to DB.",
        results.len(),
        show
    );
    println!(
        "   Legend: 🟢 HIGH (verified) | 🟡 MED (informational) | 🔴 LOW (debunked standalone)"
    );
    Ok(())
}

// Returns configured lstm backend with defaults applied.
fn configured_lstm_backend(cli_backend: lstm::LstmBackend) -> lstm::LstmBackend {
    if cli_backend != lstm::LstmBackend::Auto {
        return cli_backend;
    }
    config::lstm_backend().parse().unwrap_or_else(|err| {
        eprintln!(
            "warning: unsupported backend.lstm in config: {}; using auto.",
            err
        );
        lstm::LstmBackend::Auto
    })
}

// Returns configured xgboost backend label with defaults applied.
fn configured_xgboost_backend_label() -> String {
    let configured = config::xgboost_backend();
    if cfg!(feature = "xgboost-baseline") {
        configured
    } else {
        format!("{} (disabled at compile time)", configured)
    }
}

// Handles the daily CLI action.
async fn cmd_daily(
    days: u32,
    skip_train: bool,
    quick: bool,
    backend: lstm::LstmBackend,
    walk_forward_folds: usize,
    top_n: usize,
    slippage_bps: f64,
    json_flag: bool,
) -> anyhow::Result<()> {
    if !skip_train {
        return cmd_ml_pipeline_refresh(
            days,
            quick,
            backend,
            walk_forward_folds,
            top_n,
            slippage_bps,
            json_flag,
            false,
        )
        .await;
    }

    let backend = configured_lstm_backend(backend);
    println!("Daily non-trading data refresh");
    println!("  Window: {} days", days);
    println!("  Trading: disabled by command design");
    println!("  Training: skipped by --skip-train");
    println!("  LSTM backend: {}", backend);
    println!("  XGBoost backend: {}", configured_xgboost_backend_label());
    println!("  LightGBM backend: {}", config::lightgbm_backend());
    println!("  Ridge backend: {}", config::ridge_backend());

    cmd_universe().await?;
    cmd_sp500(days, json_flag).await?;
    cmd_scan(days, false).await?;
    sync_ml_feed_universe(json_flag).await?;
    ml::cmd_ml_features(None, false, json_flag)?;
    ml::cmd_ml_labels(5, json_flag)?;

    if !skip_train {
        ml::cmd_ml_train(quick, false, json_flag)?;
        ml::cmd_ml_walk_forward(quick, walk_forward_folds, json_flag)?;
        ml::cmd_ml_baselines(quick, json_flag)?;
        ml::cmd_ml_ablate_sp500(quick, json_flag)?;
        if let Err(err) = ml::cmd_ml_xgboost_ablate_sp500(quick, json_flag) {
            eprintln!("  warning: no-S&P XGBoost comparison skipped: {}", err);
        }
        lstm::cmd_ml_lstm_train(json_flag, false, None, false, backend)?;
        let _ = lstm::cmd_ml_lstm_evaluate(json_flag, false, top_n, slippage_bps)?;
        lstm::cmd_ml_lstm_train(json_flag, false, None, true, backend)?;
        let _ = lstm::cmd_ml_lstm_evaluate(json_flag, true, top_n, slippage_bps)?;
        let _ = ml::cmd_ml_ensemble_robust_sweep(json_flag)?;
        ml::cmd_ml_predict(json_flag)?;
        if let Err(err) = ml::cmd_ml_xgboost_predict(json_flag) {
            eprintln!("  warning: XGBoost prediction refresh skipped: {}", err);
        }
        lstm::cmd_ml_lstm_predict(json_flag, false)?;
        ml::cmd_ml_ensemble_default(json_flag)?;
        if let Err(err) = ml::cmd_ml_cache_default_shap(100, json_flag) {
            eprintln!("  warning: default SHAP cache skipped: {}", err);
        }
        if let Err(err) = ml::cmd_ml_evaluate_latest(json_flag, top_n, slippage_bps) {
            eprintln!(
                "  warning: latest prediction trading evaluation skipped: {}",
                err
            );
        }
        ml::cleanup_transient_training_datasets(json_flag)?;
    }

    cmd_status(json_flag).await?;
    Ok(())
}

// Handles the ml pipeline refresh CLI action.
async fn cmd_ml_pipeline_refresh(
    days: u32,
    quick: bool,
    backend: lstm::LstmBackend,
    walk_forward_folds: usize,
    top_n: usize,
    slippage_bps: f64,
    json_flag: bool,
    force_rebuild: bool,
) -> anyhow::Result<()> {
    let backend = configured_lstm_backend(backend);
    println!(
        "{} non-trading ML refresh",
        if force_rebuild { "Full" } else { "Incremental" }
    );
    println!("  Window: {} days", days);
    println!("  Trading: disabled by command design");
    println!(
        "  Data mode: {}",
        if force_rebuild {
            "force re-request/recompute"
        } else {
            "gap-aware incremental"
        }
    );
    println!("  LSTM backend: {}", backend);
    println!("  XGBoost backend: {}", configured_xgboost_backend_label());
    println!("  LightGBM backend: {}", config::lightgbm_backend());
    println!("  Ridge backend: {}", config::ridge_backend());
    println!("  Slippage: {:.2} bps round trip", slippage_bps);

    println!("\n1/12 Sync tradable universe");
    cmd_universe().await?;

    println!("\n2/12 Sync FRED market benchmarks gap-aware");
    cmd_sp500(days, json_flag).await?;

    println!("\n3/12 Sync Alpaca bars gap-aware");
    cmd_scan(days, force_rebuild).await?;

    println!("\n4/13 Build and sync ML feed universe");
    sync_ml_feed_universe(json_flag).await?;

    println!("\n5/13 Compute all ML features");
    ml::cmd_ml_features(None, force_rebuild, json_flag)?;

    println!("\n6/13 Compute forward-return labels");
    ml::cmd_ml_labels(5, json_flag)?;

    println!("\n7/13 Train LightGBM production model");
    ml::cmd_ml_train(quick, false, json_flag)?;

    println!("\n8/13 Run walk-forward validation");
    ml::cmd_ml_walk_forward(quick, walk_forward_folds, json_flag)?;

    println!("\n9/13 Train Ridge/XGBoost baselines");
    ml::cmd_ml_baselines(quick, json_flag)?;

    println!("\n10/13 Train/evaluate S&P 500 feature variants");
    ml::cmd_ml_ablate_sp500(quick, json_flag)?;
    if let Err(err) = ml::cmd_ml_xgboost_ablate_sp500(quick, json_flag) {
        eprintln!("  warning: no-S&P XGBoost comparison skipped: {}", err);
    }

    println!("\n11/13 Train/evaluate LSTM variants");
    lstm::cmd_ml_lstm_train(json_flag, false, None, false, backend)?;
    let _ = lstm::cmd_ml_lstm_evaluate(json_flag, false, top_n, slippage_bps)?;
    lstm::cmd_ml_lstm_train(json_flag, false, None, true, backend)?;
    let _ = lstm::cmd_ml_lstm_evaluate(json_flag, true, top_n, slippage_bps)?;

    println!("\n12/13 Run robust ensemble sweep");
    let _ = ml::cmd_ml_ensemble_robust_sweep(json_flag)?;

    println!("\n13/13 Refresh predictions, ensemble, and default SHAP cache");
    ml::cmd_ml_predict(json_flag)?;
    if let Err(err) = ml::cmd_ml_xgboost_predict(json_flag) {
        eprintln!("  warning: XGBoost prediction refresh skipped: {}", err);
    }
    lstm::cmd_ml_lstm_predict(json_flag, false)?;
    ml::cmd_ml_ensemble_default(json_flag)?;
    if let Err(err) = ml::cmd_ml_cache_default_shap(100, json_flag) {
        eprintln!("  warning: default SHAP cache skipped: {}", err);
    }
    let _ = ml::cmd_ml_evaluate_latest(json_flag, top_n, slippage_bps).map_err(|err| {
        eprintln!(
            "  warning: latest prediction trading evaluation skipped: {}",
            err
        );
        err
    });
    ml::cleanup_transient_training_datasets(json_flag)?;

    ml::cmd_ml_status(json_flag)?;
    Ok(())
}

// Handles the ml refresh CLI action.
async fn cmd_ml_refresh(
    days: u32,
    quick: bool,
    backend: lstm::LstmBackend,
    walk_forward_folds: usize,
    top_n: usize,
    slippage_bps: f64,
    json_flag: bool,
) -> anyhow::Result<()> {
    cmd_ml_pipeline_refresh(
        days,
        quick,
        backend,
        walk_forward_folds,
        top_n,
        slippage_bps,
        json_flag,
        false,
    )
    .await
}

// Handles the ml full refresh CLI action.
async fn cmd_ml_full_refresh(
    days: u32,
    quick: bool,
    backend: lstm::LstmBackend,
    walk_forward_folds: usize,
    top_n: usize,
    slippage_bps: f64,
    json_flag: bool,
) -> anyhow::Result<()> {
    cmd_ml_pipeline_refresh(
        days,
        quick,
        backend,
        walk_forward_folds,
        top_n,
        slippage_bps,
        json_flag,
        true,
    )
    .await
}

// Handles the movers CLI action.
async fn cmd_movers(json_out: bool) -> anyhow::Result<()> {
    let client = build_client();
    let data: MoversResponse = api_get(
        &client,
        &format!("{}/v1beta1/screener/stocks/movers?top=20", alpaca::DATA_URL),
    )
    .await?;

    if json_out {
        let entries = |rows: &Option<Vec<MoverEntry>>| {
            rows.as_ref()
                .map(|rows| {
                    rows.iter()
                        .filter(|m| {
                            m.symbol
                                .as_deref()
                                .map(|symbol| !is_blocked(symbol))
                                .unwrap_or(true)
                        })
                        .map(|m| {
                            serde_json::json!({
                                "symbol": m.symbol,
                                "price": m.price,
                                "change": m.change,
                                "percent_change": m.percent_change,
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        return print_json_pretty(serde_json::json!({
            "gainers": entries(&data.gainers),
            "losers": entries(&data.losers),
        }));
    }

    if let Some(gainers) = &data.gainers {
        println!("🟢 TOP GAINERS");
        println!(
            "{:<8} {:>10} {:>10} {:>9}",
            "Symbol", "Price", "Change", "Change%"
        );
        println!("{}", "-".repeat(40));
        for m in gainers {
            let sym = m.symbol.as_deref().unwrap_or("?");
            if is_blocked(sym) {
                continue;
            }
            println!(
                "{:<8} {:>10.2} {:>+10.2} {:>+8.2}%",
                sym,
                m.price.unwrap_or(0.0),
                m.change.unwrap_or(0.0),
                m.percent_change.unwrap_or(0.0)
            );
        }
    }
    if let Some(losers) = &data.losers {
        println!("\n🔴 TOP LOSERS");
        println!(
            "{:<8} {:>10} {:>10} {:>9}",
            "Symbol", "Price", "Change", "Change%"
        );
        println!("{}", "-".repeat(40));
        for m in losers {
            let sym = m.symbol.as_deref().unwrap_or("?");
            if is_blocked(sym) {
                continue;
            }
            println!(
                "{:<8} {:>10.2} {:>+10.2} {:>+8.2}%",
                sym,
                m.price.unwrap_or(0.0),
                m.change.unwrap_or(0.0),
                m.percent_change.unwrap_or(0.0)
            );
        }
    }
    Ok(())
}

// Handles the watchlist CLI action.
async fn cmd_watchlist(json_out: bool) -> anyhow::Result<()> {
    let conn = open_db()?;
    let latest_date: String = conn
        .query_row("SELECT MAX(date) FROM screen_results", [], |r| r.get(0))
        .unwrap_or_else(|_| "".into());
    if latest_date.is_empty() {
        if json_out {
            return print_json_pretty(serde_json::json!({
                "ok": false,
                "error": "No screen results",
                "status_code": 404,
                "next": "mlai-trade data screen"
            }));
        }
        println!("❌ No screen results. Run `mlai-trade data screen` first.");
        return Ok(());
    }

    let mut stmt = conn.prepare(
        "SELECT symbol,close,change_pct,volume_ratio,signals,confidence FROM screen_results WHERE date=?1
         ORDER BY CASE confidence WHEN 'HIGH' THEN 0 WHEN 'MED' THEN 1 WHEN 'LOW' THEN 2 ELSE 3 END,
         json_array_length(signals) DESC, ABS(change_pct) DESC LIMIT 30"
    )?;

    struct Row {
        symbol: String,
        close: f64,
        change_pct: f64,
        volume_ratio: f64,
        signals: String,
        confidence: String,
    }
    let rows: Vec<Row> = stmt
        .query_map(params![latest_date], |r| {
            Ok(Row {
                symbol: r.get(0)?,
                close: r.get(1)?,
                change_pct: r.get(2)?,
                volume_ratio: r.get(3)?,
                signals: r.get(4)?,
                confidence: r
                    .get::<_, Option<String>>(5)?
                    .unwrap_or_else(|| "MED".into()),
            })
        })?
        .filter_map(|r| r.ok())
        .filter(|r| !is_blocked(&r.symbol))
        .collect();

    if json_out {
        let items = rows
            .iter()
            .map(|r| {
                let signals: Vec<String> = serde_json::from_str(&r.signals).unwrap_or_default();
                serde_json::json!({
                    "symbol": r.symbol,
                    "close": r.close,
                    "change_pct": r.change_pct,
                    "volume_ratio": r.volume_ratio,
                    "signals": signals,
                    "confidence": r.confidence,
                })
            })
            .collect::<Vec<_>>();
        return print_json_pretty(serde_json::json!({
            "ok": true,
            "date": latest_date,
            "count": items.len(),
            "results": items,
        }));
    }

    if rows.is_empty() {
        println!("No watchlist entries.");
        return Ok(());
    }
    println!("📋 Watchlist — Screen results from {}", latest_date);
    println!(
        "{:<3} {:<8} {:>10} {:>9} {:>7} {}",
        "Conf", "Symbol", "Close", "Change%", "VolR", "Signals"
    );
    println!("{}", "-".repeat(85));
    for r in &rows {
        let sigs: Vec<String> = serde_json::from_str(&r.signals).unwrap_or_default();
        let sig_display: Vec<String> = sigs
            .iter()
            .map(|s| format!("{}{}", signal_confidence(s).emoji(), s))
            .collect();
        let conf_emoji = match r.confidence.as_str() {
            "HIGH" => "🟢",
            "MED" => "🟡",
            "LOW" => "🔴",
            _ => "⚪",
        };
        println!(
            " {}  {:<8} {:>10.2} {:>+8.2}% {:>6.1}x {}",
            conf_emoji,
            r.symbol,
            r.close,
            r.change_pct,
            r.volume_ratio,
            sig_display.join(", ")
        );
    }
    println!("\nShowing top {} from {}.", rows.len(), latest_date);
    println!("Legend: 🟢 HIGH (verified) | 🟡 MED (informational) | 🔴 LOW (debunked standalone)");
    Ok(())
}

// ══════════════════════════════════════════════════════════════════
//  SUGGEST COMMAND — Evidence-based scoring algorithm
// ══════════════════════════════════════════════════════════════════

const HIGH_SIGNALS: &[&str] = &[
    "NEW_HIGH",
    "NEW_LOW",
    "BIG_MOVE",
    "MOMENTUM_3M",
    "MOMENTUM_6M",
];
const MED_SIGNALS: &[&str] = &["VOL_SPIKE", "GAP_UP", "GAP_DOWN", "LOW_VOL"];

// Handles extract momentum logic.
fn extract_momentum(signals: &[String], prefix: &str) -> Option<f64> {
    for s in signals {
        if s.starts_with(&format!("{}(", prefix)) {
            let inner = &s[prefix.len() + 1..s.len() - 1];
            let cleaned = inner.replace('%', "").replace('+', "");
            if let Ok(v) = cleaned.parse::<f64>() {
                return Some(v);
            }
        }
    }
    None
}

// Handles the suggest CLI action.
async fn cmd_suggest(json_out: bool) -> anyhow::Result<()> {
    let conn = open_db()?;

    let screen_date: String = conn
        .query_row("SELECT MAX(date) FROM screen_results", [], |r| r.get(0))
        .unwrap_or_else(|_| "".into());
    if screen_date.is_empty() {
        if json_out {
            return print_json_pretty(serde_json::json!({
                "ok": false,
                "error": "No screen results",
                "status_code": 404,
                "next": "mlai-trade data screen"
            }));
        }
        println!("❌ No screen results. Run `mlai-trade data screen` first.");
        return Ok(());
    }

    let total_screened: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT symbol) FROM bars WHERE date=?1",
            params![screen_date],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let total_with_signals: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM screen_results WHERE date=?1",
            params![screen_date],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let mut stmt = conn.prepare(
        "SELECT symbol,close,change_pct,volume_ratio,signals,confidence FROM screen_results WHERE date=?1 AND change_pct > 0 AND close > 5"
    )?;

    struct Suggestion {
        symbol: String,
        close: f64,
        change_pct: f64,
        score: i32,
        confidence: String,
        signals: Vec<String>,
        momentum_3m: Option<f64>,
        momentum_6m: Option<f64>,
        avg_volume_20d: i64,
        volatility_20d: f64,
    }

    let mut suggestions: Vec<Suggestion> = Vec::new();

    let rows: Vec<(String, f64, f64, f64, String, Option<String>)> = stmt
        .query_map(params![screen_date], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();

    for (sym, close, change_pct, _volume_ratio, signals_json, confidence) in &rows {
        if is_blocked(sym) {
            continue;
        }

        let mut sigs: Vec<String> = serde_json::from_str(signals_json).unwrap_or_default();
        let bases: Vec<&str> = sigs.iter().map(|s| signal_base(s)).collect();

        // Score calculation
        let mut score: i32 = 0;
        for b in &bases {
            if HIGH_SIGNALS.contains(b) {
                score += 3;
            } else if MED_SIGNALS.contains(b) {
                score += 1;
            }
        }

        // Bonuses
        let has_mom_3m = bases.contains(&"MOMENTUM_3M");
        let has_mom_6m = bases.contains(&"MOMENTUM_6M");
        let has_vol_spike = bases.contains(&"VOL_SPIKE");
        let has_new_high = bases.contains(&"NEW_HIGH");
        let has_any_momentum = has_mom_3m || has_mom_6m;

        if has_mom_3m && has_mom_6m {
            score += 3;
        } // Dual timeframe
        if has_vol_spike && has_any_momentum {
            score += 2;
        } // Volume confirmation
        if has_new_high && has_any_momentum {
            score += 2;
        } // New high + momentum

        // ── Feeds-based score boosts ──
        let mut feed_signals: Vec<String> = Vec::new();

        // News sentiment boost (last 7 days)
        let sentiment_avg: f64 = conn
            .query_row(
                "SELECT COALESCE(AVG(sentiment_score), 0.0) FROM news_articles
             WHERE symbols LIKE ?1 AND published_at > datetime('now', '-7 days')",
                params![format!("%{}%", sym)],
                |r| r.get(0),
            )
            .unwrap_or(0.0);
        if sentiment_avg > 0.3 {
            score += 2;
            feed_signals.push(format!("📰SENTIMENT({:+.2})", sentiment_avg));
        } else if sentiment_avg < -0.3 {
            score -= 2;
            feed_signals.push(format!("📰SENTIMENT({:+.2})", sentiment_avg));
        }

        // SEC 8-K filing boost (recent material event)
        let recent_8k: i64 = conn.query_row(
            "SELECT COUNT(*) FROM news_articles
             WHERE symbols LIKE ?1 AND filing_type = '8-K' AND published_at > datetime('now', '-3 days')",
            params![format!("%{}%", sym)], |r| r.get(0)
        ).unwrap_or(0);
        if recent_8k > 0 {
            score += 3;
            feed_signals.push("📋SEC_8K".into());
        }

        // Insider buying (Form 4 with positive sentiment)
        let insider_buy: i64 = conn.query_row(
            "SELECT COUNT(*) FROM news_articles
             WHERE symbols LIKE ?1 AND filing_type = '4' AND sentiment_score > 0 AND published_at > datetime('now', '-7 days')",
            params![format!("%{}%", sym)], |r| r.get(0)
        ).unwrap_or(0);
        if insider_buy > 0 {
            score += 2;
            feed_signals.push("👤INSIDER_BUY".into());
        }

        // Insider selling (Form 4 with negative sentiment)
        let insider_sell: i64 = conn.query_row(
            "SELECT COUNT(*) FROM news_articles
             WHERE symbols LIKE ?1 AND filing_type = '4' AND sentiment_score < 0 AND published_at > datetime('now', '-7 days')",
            params![format!("%{}%", sym)], |r| r.get(0)
        ).unwrap_or(0);
        if insider_sell > 0 {
            score -= 2;
            feed_signals.push("👤INSIDER_SELL".into());
        }

        // Correlated stock rising
        let corr_boost: i64 = conn.query_row(
            "SELECT COUNT(*) FROM price_correlations pc
             JOIN screen_results sr ON (sr.symbol = CASE WHEN pc.symbol_a = ?1 THEN pc.symbol_b ELSE pc.symbol_a END)
             WHERE (pc.symbol_a = ?1 OR pc.symbol_b = ?1) AND pc.correlation_30d > 0.7
             AND sr.date = ?2 AND sr.change_pct > 0",
            params![sym, screen_date], |r| r.get(0)
        ).unwrap_or(0);
        if corr_boost > 0 {
            score += 2;
            feed_signals.push("🔗CORR_BOOST".into());
        }

        // Append feed signals to the main signals list
        sigs.extend(feed_signals);

        if score < 6 {
            continue;
        }

        let mom_3m = extract_momentum(&sigs, "MOMENTUM_3M");
        let mom_6m = extract_momentum(&sigs, "MOMENTUM_6M");

        // Get 20-day stats from bars
        let mut bstmt = conn.prepare("SELECT close,volume FROM bars WHERE symbol=?1 AND date<=?2 ORDER BY date DESC LIMIT 21")?;
        let bar_rows: Vec<(f64, i64)> = bstmt
            .query_map(params![sym, screen_date], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();

        let avg_vol = if bar_rows.len() >= 2 {
            let vols: Vec<f64> = bar_rows.iter().take(20).map(|b| b.1 as f64).collect();
            if !vols.is_empty() {
                (vols.iter().sum::<f64>() / vols.len() as f64) as i64
            } else {
                0
            }
        } else {
            0
        };

        let vol_20d = if bar_rows.len() >= 2 {
            let closes: Vec<f64> = bar_rows
                .iter()
                .take(21)
                .map(|b| b.0)
                .filter(|c| *c > 0.0)
                .collect();
            if closes.len() >= 2 {
                let returns: Vec<f64> = (0..closes.len() - 1)
                    .map(|i| (closes[i] - closes[i + 1]) / closes[i + 1])
                    .collect();
                let mean = returns.iter().sum::<f64>() / returns.len() as f64;
                let var =
                    returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64;
                var.sqrt() * 100.0
            } else {
                0.0
            }
        } else {
            0.0
        };

        let conf = confidence.clone().unwrap_or_else(|| {
            if score >= 12 {
                "HIGH".into()
            } else {
                "MED".into()
            }
        });

        suggestions.push(Suggestion {
            symbol: sym.clone(),
            close: *close,
            change_pct: *change_pct,
            score,
            confidence: conf,
            signals: sigs,
            momentum_3m: mom_3m,
            momentum_6m: mom_6m,
            avg_volume_20d: avg_vol,
            volatility_20d: vol_20d,
        });
    }

    suggestions.sort_by(|a, b| b.score.cmp(&a.score));
    let qualified_count = suggestions.len();
    let high_count = suggestions.iter().filter(|s| s.score >= 12).count();
    suggestions.truncate(20);

    if suggestions.is_empty() {
        if json_out {
            return print_json_pretty(serde_json::json!({
                "ok": true,
                "date": screen_date,
                "score_threshold": 6,
                "summary": {
                    "screened": total_screened,
                    "with_signals": total_with_signals,
                    "qualified": 0,
                    "shown": 0,
                    "high_conviction": 0,
                },
                "suggestions": [],
            }));
        }
        println!(
            "📊 No suggestions qualify (score >= 6) from {}",
            screen_date
        );
        println!(
            "   Screened: {} | With signals: {} | Qualified: 0",
            total_screened, total_with_signals
        );
        return Ok(());
    }

    if json_out {
        let items = suggestions
            .iter()
            .enumerate()
            .map(|(idx, s)| {
                serde_json::json!({
                    "rank": idx + 1,
                    "symbol": s.symbol,
                    "close": s.close,
                    "change_pct": s.change_pct,
                    "score": s.score,
                    "confidence": s.confidence,
                    "momentum_3m": s.momentum_3m,
                    "momentum_6m": s.momentum_6m,
                    "avg_volume_20d": s.avg_volume_20d,
                    "volatility_20d": s.volatility_20d,
                    "signals": s.signals,
                })
            })
            .collect::<Vec<_>>();
        return print_json_pretty(serde_json::json!({
            "ok": true,
            "date": screen_date,
            "score_threshold": 6,
            "summary": {
                "screened": total_screened,
                "with_signals": total_with_signals,
                "qualified": qualified_count,
                "shown": items.len(),
                "high_conviction": high_count,
            },
            "scoring": {
                "high_signal": 3,
                "medium_signal": 1,
                "low_signal": 0,
                "dual_momentum_bonus": 3,
                "volume_confirmation_bonus": 2,
                "new_high_momentum_bonus": 2,
                "sentiment_adjustment": 2,
                "sec_8k_bonus": 3,
                "insider_adjustment": 2,
                "correlation_bonus": 2
            },
            "suggestions": items,
        }));
    }

    println!(
        "🎯 Top Buy Suggestions — {} (evidence-based scoring)\n",
        screen_date
    );
    println!(
        "{:<2} {:<8} {:>8} {:>8} {:>5} {:>8} {:>8} {:>10} {:>6} {}",
        "#", "Symbol", "Close", "Chg%", "Score", "Mom3M", "Mom6M", "AvgVol20d", "Vol", "Signals"
    );
    println!("{}", "-".repeat(105));

    for (i, s) in suggestions.iter().enumerate() {
        let mom3 = s
            .momentum_3m
            .map(|v| format!("{:+.1}%", v))
            .unwrap_or_else(|| "—".into());
        let mom6 = s
            .momentum_6m
            .map(|v| format!("{:+.1}%", v))
            .unwrap_or_else(|| "—".into());
        let avg_vol_str = if s.avg_volume_20d > 1_000_000 {
            format!("{:.1}M", s.avg_volume_20d as f64 / 1_000_000.0)
        } else if s.avg_volume_20d > 1_000 {
            format!("{:.0}K", s.avg_volume_20d as f64 / 1_000.0)
        } else {
            format!("{}", s.avg_volume_20d)
        };
        let conf_emoji = match s.confidence.as_str() {
            "HIGH" => "🟢",
            "MED" => "🟡",
            _ => "🔴",
        };
        let sig_short: Vec<String> = s
            .signals
            .iter()
            .map(|sig| {
                let base = signal_base(sig);
                format!("{}{}", signal_confidence(sig).emoji(), base)
            })
            .collect();
        println!(
            "{}{:<2} {:<8} {:>8.2} {:>+7.2}% {:>5} {:>8} {:>8} {:>10} {:>6.1}% {}",
            conf_emoji,
            i + 1,
            s.symbol,
            s.close,
            s.change_pct,
            s.score,
            mom3,
            mom6,
            avg_vol_str,
            s.volatility_20d,
            sig_short.join(" ")
        );
    }

    println!(
        "\n📊 Summary: {} screened → {} with signals → {} qualified (score ≥ 6) → {} shown",
        total_screened,
        total_with_signals,
        suggestions.len(),
        suggestions.len().min(20)
    );
    if high_count > 0 {
        println!("   🟢 {} high-conviction picks (score ≥ 12)", high_count);
    }
    println!("   Scoring: HIGH +3 | MED +1 | LOW +0 | Dual-momentum +3 | Vol-confirm +2 | NewHigh+Mom +2");
    println!("   Feeds:   📰Sentiment ±2 | 📋SEC_8K +3 | 👤Insider ±2 | 🔗Corr +2");
    Ok(())
}

// ══════════════════════════════════════════════════════════════════
//  COMPLIANCE COMMANDS
// ══════════════════════════════════════════════════════════════════

async fn cmd_wash(json_out: bool) -> anyhow::Result<()> {
    let conn = open_db()?;
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let mut stmt = conn.prepare("SELECT symbol,sell_date,sell_price,loss_amount,wash_window_end,status FROM wash_sale_tracker ORDER BY wash_window_end DESC")?;

    struct WashRow {
        symbol: String,
        sell_date: String,
        sell_price: f64,
        loss_amount: f64,
        wash_window_end: String,
        status: String,
    }
    let rows: Vec<WashRow> = stmt
        .query_map([], |r| {
            Ok(WashRow {
                symbol: r.get(0)?,
                sell_date: r.get(1)?,
                sell_price: r.get(2)?,
                loss_amount: r.get(3)?,
                wash_window_end: r.get(4)?,
                status: r.get(5)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    if json_out {
        let active_rows = rows
            .iter()
            .filter(|r| r.status == "active" && r.wash_window_end >= today)
            .map(|r| {
                let days_left = chrono::NaiveDate::parse_from_str(&r.wash_window_end, "%Y-%m-%d")
                    .ok()
                    .and_then(|end| {
                        chrono::NaiveDate::parse_from_str(&today, "%Y-%m-%d")
                            .ok()
                            .map(|t| (end - t).num_days())
                    })
                    .unwrap_or(0);
                serde_json::json!({
                    "symbol": r.symbol,
                    "sell_date": r.sell_date,
                    "sell_price": r.sell_price,
                    "loss_amount": r.loss_amount,
                    "wash_window_end": r.wash_window_end,
                    "status": r.status,
                    "days_left": days_left,
                })
            })
            .collect::<Vec<_>>();
        return print_json_pretty(serde_json::json!({
            "today": today,
            "active_count": active_rows.len(),
            "total_records": rows.len(),
            "active": active_rows,
        }));
    }

    if rows.is_empty() {
        println!("No wash sale records. ✅");
        return Ok(());
    }
    let active: Vec<&WashRow> = rows
        .iter()
        .filter(|r| r.status == "active" && r.wash_window_end >= today)
        .collect();
    let expired_count = rows.len() - active.len();

    if !active.is_empty() {
        println!("🚨 ACTIVE Wash Sale Windows — IRS §1091");
        println!("   Buying these symbols will DISALLOW the loss deduction!\n");
        println!(
            "{:<8} {:<12} {:>10} {:>10} {:<12} {:>9}",
            "Symbol", "Sold", "Price", "Loss", "Window End", "Days Left"
        );
        println!("{}", "-".repeat(65));
        for r in &active {
            let days_left = chrono::NaiveDate::parse_from_str(&r.wash_window_end, "%Y-%m-%d")
                .ok()
                .and_then(|end| {
                    chrono::NaiveDate::parse_from_str(&today, "%Y-%m-%d")
                        .ok()
                        .map(|t| (end - t).num_days())
                })
                .unwrap_or(0);
            println!(
                "{:<8} {:<12} {:>10.2} {:>10.2} {:<12} {:>7}d",
                r.symbol, r.sell_date, r.sell_price, r.loss_amount, r.wash_window_end, days_left
            );
        }
        println!();
    } else {
        println!("✅ No active wash sale windows.");
    }
    if expired_count > 0 {
        println!(
            "📋 {} past wash sale records (windows expired/cleared).",
            expired_count
        );
    }
    Ok(())
}

// Handles the pdt CLI action.
async fn cmd_pdt(json_out: bool) -> anyhow::Result<()> {
    let conn = open_db()?;
    let window_start = (Utc::now() - chrono::Duration::days(7))
        .format("%Y-%m-%d")
        .to_string();
    let mut stmt = conn.prepare("SELECT trade_date,symbol,sell_time FROM day_trades WHERE trade_date>=?1 ORDER BY trade_date DESC,sell_time DESC")?;

    struct PdtRow {
        trade_date: String,
        symbol: String,
        sell_time: String,
    }
    let rows: Vec<PdtRow> = stmt
        .query_map(params![window_start], |r| {
            Ok(PdtRow {
                trade_date: r.get(0)?,
                symbol: r.get(1)?,
                sell_time: r.get(2)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    let count = rows.len() as i64;
    let remaining = if count >= PDT_TRADE_LIMIT {
        0
    } else {
        PDT_TRADE_LIMIT - count
    };

    // Also get account PDT status
    let client = build_client();
    let (pdt_flag, equity) =
        match api_get::<AccountInfo>(&client, &alpaca::broker_api_url("/account")).await {
            Ok(acct) => (
                acct.pattern_day_trader.unwrap_or(false),
                acct.equity
                    .as_deref()
                    .unwrap_or("0")
                    .parse::<f64>()
                    .unwrap_or(0.0),
            ),
            Err(_) => (false, 0.0),
        };

    if json_out {
        let day_trades = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "trade_date": r.trade_date,
                    "symbol": r.symbol,
                    "sell_time": r.sell_time,
                })
            })
            .collect::<Vec<_>>();
        return print_json_pretty(serde_json::json!({
            "pattern_day_trader": pdt_flag,
            "account_equity": equity,
            "current_pdt_threshold": PDT_MIN_EQUITY_DOLLARS_PRE_2026_06_04,
            "rule_transition": "FINRA intraday margin replaces PDT on June 4, 2026; broker phase-in may vary",
            "day_trades_5d": count,
            "day_trade_limit": PDT_TRADE_LIMIT,
            "remaining_before_trigger": remaining,
            "recent_day_trades": day_trades,
        }));
    }

    println!("📊 Pattern Day Trader Status");
    println!(
        "  Alpaca PDT flag:  {}",
        if pdt_flag { "🔴 YES" } else { "🟢 NO" }
    );
    println!("  Account equity:   {}", fmt_money_comma(equity));
    println!(
        "  Current PDT threshold: {} before June 4, 2026",
        fmt_money_comma(PDT_MIN_EQUITY_DOLLARS_PRE_2026_06_04)
    );
    println!("  Rule transition:  FINRA intraday margin replaces PDT on June 4, 2026; broker phase-in may vary");
    println!("  Day trades (5d):  {} / {}", count, PDT_TRADE_LIMIT);
    println!(
        "  Remaining:        {} day trades before PDT trigger",
        remaining
    );
    println!();

    if !rows.is_empty() {
        println!("  Recent day trades:");
        for r in &rows {
            println!("    {} — {} (sold {})", r.trade_date, r.symbol, r.sell_time);
        }
    } else {
        println!("  No day trades in the last 5 business days. ✅");
    }

    if equity < PDT_MIN_EQUITY_DOLLARS_PRE_2026_06_04 && count >= PDT_TRADE_LIMIT - 1 {
        println!(
            "\n  🚨 WARNING: Equity ({}) is below {}.",
            fmt_money_comma(equity),
            fmt_money_comma(PDT_MIN_EQUITY_DOLLARS_PRE_2026_06_04)
        );
        println!("     PDT status would restrict your account!");
    }
    Ok(())
}

// Handles the status CLI action.
async fn cmd_status(json_out: bool) -> anyhow::Result<()> {
    let conn = open_db()?;
    let asset_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM assets", [], |r| r.get(0))
        .unwrap_or(0);
    let bar_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM bars", [], |r| r.get(0))
        .unwrap_or(0);
    let bar_symbols: i64 = conn
        .query_row("SELECT COUNT(DISTINCT symbol) FROM bars", [], |r| r.get(0))
        .unwrap_or(0);
    let latest_bar: String = conn
        .query_row("SELECT MAX(date) FROM bars", [], |r| r.get(0))
        .unwrap_or_else(|_| "none".into());
    let earliest_bar: String = conn
        .query_row("SELECT MIN(date) FROM bars", [], |r| r.get(0))
        .unwrap_or_else(|_| "none".into());
    let screen_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM screen_results", [], |r| r.get(0))
        .unwrap_or(0);
    let latest_screen: String = conn
        .query_row("SELECT MAX(date) FROM screen_results", [], |r| r.get(0))
        .unwrap_or_else(|_| "none".into());
    let sp500_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM macro_series WHERE series_id=?1",
            params![FRED_SP500_SERIES_ID],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let sp500_latest: String = conn
        .query_row(
            "SELECT COALESCE(MAX(date), 'none') FROM macro_series WHERE series_id=?1",
            params![FRED_SP500_SERIES_ID],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "none".into());
    let wash_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wash_sale_tracker WHERE status='active'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let pdt_count: i64 = {
        let ws = (Utc::now() - chrono::Duration::days(7))
            .format("%Y-%m-%d")
            .to_string();
        conn.query_row(
            "SELECT COUNT(*) FROM day_trades WHERE trade_date>=?1",
            params![ws],
            |r| r.get(0),
        )
        .unwrap_or(0)
    };
    let db_size = std::fs::metadata(db_path()).map(|m| m.len()).unwrap_or(0);
    let daemon_status = daemon::status();
    let api_status = api::status();
    let feed_subs: i64 = conn
        .query_row("SELECT COUNT(*) FROM feed_subscriptions", [], |r| r.get(0))
        .unwrap_or(0);
    let feed_articles: i64 = conn
        .query_row("SELECT COUNT(*) FROM news_articles", [], |r| r.get(0))
        .unwrap_or(0);
    let feed_rels: i64 = conn
        .query_row("SELECT COUNT(*) FROM company_relationships", [], |r| {
            r.get(0)
        })
        .unwrap_or(0);
    let feed_corrs: i64 = conn
        .query_row("SELECT COUNT(*) FROM price_correlations", [], |r| r.get(0))
        .unwrap_or(0);

    if json_out {
        return print_json_pretty(serde_json::json!({
            "mode": if alpaca::is_paper() { "paper" } else { "individual" },
            "assets": asset_count,
            "bars": {
                "rows": bar_count,
                "symbols": bar_symbols,
                "earliest": earliest_bar,
                "latest": latest_bar,
            },
            "screen_results": {
                "rows": screen_count,
                "latest": latest_screen,
            },
            "fred": {
                "sp500_rows": sp500_count,
                "sp500_latest": sp500_latest,
            },
            "blocked_symbols": config::blocked_symbols(),
            "wash_sale_active": wash_count,
            "day_trades_5d": pdt_count,
            "pdt_trade_limit": PDT_TRADE_LIMIT,
            "db_size_bytes": db_size,
            "daemon": {
                "enabled": daemon_status.enabled,
                "running": daemon_status.running,
                "pid": daemon_status.pid,
                "interval_seconds": daemon_status.interval_seconds,
            },
            "api": {
                "enabled": api_status.enabled,
                "running": api_status.running,
                "pid": api_status.pid,
                "socket_file": api_status.socket_file.display().to_string(),
                "request_timeout_seconds": api_status.request_timeout_seconds,
            },
            "feeds": {
                "subscriptions": feed_subs,
                "articles": feed_articles,
                "relationships": feed_rels,
                "correlations": feed_corrs,
            }
        }));
    }

    println!("📊 Alpaca CLI Status");
    println!(
        "  Mode:             {}",
        if alpaca::is_paper() {
            "🟡 PAPER"
        } else {
            "🔴 LIVE"
        }
    );
    println!("  Assets:           {} symbols", asset_count);
    println!(
        "  Bars:             {} rows ({} symbols)",
        bar_count, bar_symbols
    );
    println!("  Bar range:        {} → {}", earliest_bar, latest_bar);
    println!("  Screen results:   {} entries", screen_count);
    println!("  Last screen:      {}", latest_screen);
    println!(
        "  S&P 500/FRED:     {} rows (latest {})",
        sp500_count, sp500_latest
    );
    println!("  Blocked symbols:  {:?}", config::blocked_symbols());
    println!("  Wash sale active: {}", wash_count);
    println!("  Day trades (5d):  {} / {}", pdt_count, PDT_TRADE_LIMIT);
    println!("  DB size:          {:.1} MB", db_size as f64 / 1_048_576.0);
    println!(
        "  Daemon:           {}{} (enabled={}, interval={}s)",
        if daemon_status.running {
            "running"
        } else {
            "stopped"
        },
        daemon_status
            .pid
            .map(|pid| format!(" pid={}", pid))
            .unwrap_or_default(),
        daemon_status.enabled,
        daemon_status.interval_seconds
    );
    println!(
        "  API:              {}{} (enabled={}, socket={})",
        if api_status.running {
            "running"
        } else {
            "stopped"
        },
        api_status
            .pid
            .map(|pid| format!(" pid={}", pid))
            .unwrap_or_default(),
        api_status.enabled,
        api_status.socket_file.display()
    );
    if feed_subs > 0 || feed_articles > 0 {
        println!("  Feed subs:        {} symbols", feed_subs);
        println!("  Articles:         {}", feed_articles);
        println!("  Relationships:    {} edges", feed_rels);
        println!("  Correlations:     {} pairs", feed_corrs);
    }
    Ok(())
}

// ══════════════════════════════════════════════════════════════════
//  FEEDS — News monitoring, sentiment, company relationships
// ══════════════════════════════════════════════════════════════════

const SEC_USER_AGENT: &str = "Plumber/1.0 (rodrigo@broilo.eu)";

// Sentiment word lists
const POSITIVE_WORDS: &[&str] = &[
    "beat",
    "beats",
    "surge",
    "surges",
    "surged",
    "profit",
    "profits",
    "growth",
    "upgrade",
    "upgraded",
    "buy",
    "record",
    "strong",
    "bullish",
    "outperform",
    "revenue growth",
    "earnings beat",
    "raises guidance",
    "raised guidance",
    "new high",
    "acquisition",
    "acquired",
    "rally",
    "rallied",
    "soar",
    "soared",
    "boost",
    "boosted",
    "positive",
    "exceeded",
    "exceeds",
    "upbeat",
    "optimistic",
    "breakout",
    "momentum",
    "expansion",
    "innovative",
];

const NEGATIVE_WORDS: &[&str] = &[
    "miss",
    "misses",
    "missed",
    "drop",
    "drops",
    "dropped",
    "loss",
    "losses",
    "cut",
    "cuts",
    "downgrade",
    "downgraded",
    "sell",
    "weak",
    "bearish",
    "underperform",
    "revenue decline",
    "earnings miss",
    "lowers guidance",
    "lowered guidance",
    "investigation",
    "lawsuit",
    "bankruptcy",
    "layoff",
    "layoffs",
    "recall",
    "recalled",
    "crash",
    "crashed",
    "plunge",
    "plunged",
    "slump",
    "slumped",
    "warning",
    "warns",
    "deficit",
    "decline",
    "declined",
    "fraud",
    "scandal",
    "probe",
    "fine",
    "fined",
    "penalty",
    "default",
];

// Computes sentiment from prepared inputs.
fn compute_sentiment(text: &str) -> f64 {
    let lower = text.to_lowercase();
    let mut score = 0.0f64;
    for w in POSITIVE_WORDS {
        if lower.contains(w) {
            score += 0.1;
        }
    }
    for w in NEGATIVE_WORDS {
        if lower.contains(w) {
            score -= 0.1;
        }
    }
    score.clamp(-1.0, 1.0)
}

// Builds sec client from configured inputs.
fn build_sec_client() -> reqwest::Client {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(SEC_USER_AGENT));
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("Failed to build SEC client")
}

// Builds rss client from configured inputs.
fn build_rss_client() -> reqwest::Client {
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static("Mozilla/5.0 (compatible; Plumber/1.0)"),
    );
    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .expect("Failed to build RSS client")
}

// Stores article in local storage.
fn upsert_article(
    conn: &Connection,
    source: &str,
    title: &str,
    url: &str,
    summary: Option<&str>,
    symbols: &str,
    published_at: Option<&str>,
    filing_type: Option<&str>,
) -> bool {
    let now = Utc::now().to_rfc3339();
    let published_date = published_at.and_then(|value| value.get(..10));
    let text_for_sentiment = format!("{} {}", title, summary.unwrap_or(""));
    let sentiment = compute_sentiment(&text_for_sentiment);

    let result = conn.execute(
        "INSERT OR IGNORE INTO news_articles
            (source, title, url, summary, symbols, published_at, published_date, fetched_at, sentiment_score, filing_type)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            source,
            title,
            url,
            summary,
            symbols,
            published_at,
            published_date,
            now,
            sentiment,
            filing_type
        ],
    );
    matches!(result, Ok(1))
}

// Handles detect co mentions logic.
fn detect_co_mentions(conn: &Connection, article_symbols: &str) {
    let syms: Vec<&str> = article_symbols
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if syms.len() < 2 {
        return;
    }
    let now = Utc::now().to_rfc3339();
    for i in 0..syms.len() {
        for j in (i + 1)..syms.len() {
            let (a, b) = if syms[i] < syms[j] {
                (syms[i], syms[j])
            } else {
                (syms[j], syms[i])
            };
            // Try to increment strength if already exists
            let updated = conn
                .execute(
                    "UPDATE company_relationships SET strength = strength + 1.0
                 WHERE symbol_a = ?1 AND symbol_b = ?2 AND relationship = 'co_mention'",
                    params![a, b],
                )
                .unwrap_or(0);
            if updated == 0 {
                let _ = conn.execute(
                    "INSERT OR IGNORE INTO company_relationships (symbol_a, symbol_b, relationship, strength, source, discovered_at)
                     VALUES (?1, ?2, 'co_mention', 1.0, 'news_co_mention', ?3)",
                    params![a, b, now],
                );
            }
        }
    }
}

// Extract ticker symbols from text by matching against the assets table
fn extract_symbols_from_text(conn: &Connection, text: &str) -> Vec<String> {
    let upper = text.to_uppercase();
    // Quick regex for potential tickers: 1-5 uppercase letters surrounded by word boundaries
    let re = regex::Regex::new(r"\b([A-Z]{1,5})\b").unwrap();
    let mut found = Vec::new();
    for cap in re.captures_iter(&upper) {
        let candidate = &cap[1];
        // Skip common English words that look like tickers
        if matches!(
            candidate,
            "A" | "I"
                | "THE"
                | "AND"
                | "FOR"
                | "TO"
                | "IN"
                | "OF"
                | "IS"
                | "IT"
                | "AT"
                | "ON"
                | "OR"
                | "AN"
                | "BY"
                | "AS"
                | "BE"
                | "SO"
                | "UP"
                | "IF"
                | "AM"
                | "PM"
                | "US"
                | "UK"
                | "CEO"
                | "CFO"
                | "IPO"
                | "SEC"
                | "IRS"
                | "FDA"
                | "NYSE"
                | "API"
                | "ETF"
                | "AI"
                | "ARE"
                | "HAS"
                | "HAD"
                | "NOT"
                | "BUT"
                | "ALL"
                | "NEW"
                | "INC"
                | "LTD"
                | "CO"
                | "VS"
                | "EST"
                | "PT"
                | "ET"
                | "EPS"
                | "PE"
                | "YOY"
                | "QOQ"
                | "GDP"
                | "CPI"
                | "FED"
                | "HE"
                | "SHE"
                | "WE"
                | "MY"
        ) {
            continue;
        }
        if candidate.len() < 2 {
            continue;
        }
        // Check if this symbol exists in our assets table
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM assets WHERE symbol = ?1",
                params![candidate],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0)
            > 0;
        if exists && !found.contains(&candidate.to_string()) {
            found.push(candidate.to_string());
        }
    }
    found
}

/// Sync Alpaca news for a symbol
async fn sync_alpaca_news(
    conn: &Connection,
    client: &reqwest::Client,
    symbol: &str,
    _days: u32,
) -> anyhow::Result<usize> {
    let url = format!(
        "{}/v1beta1/news?symbols={}&limit=20",
        alpaca::DATA_URL,
        symbol
    );
    let data: NewsResponse = match api_get(client, &url).await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("  ⚠️  Alpaca news for {}: {}", symbol, e);
            return Ok(0);
        }
    };
    let news = data.news.unwrap_or_default();
    let mut new_count = 0;
    for item in &news {
        let title = item.headline.as_deref().unwrap_or("");
        let url = item.url.as_deref().unwrap_or("");
        if url.is_empty() || title.is_empty() {
            continue;
        }
        let syms_str = item
            .symbols
            .as_ref()
            .map(|s| s.join(","))
            .unwrap_or_else(|| symbol.to_string());
        let published = item.created_at.as_deref();
        let summary = item.summary.as_deref();
        if upsert_article(
            conn, "alpaca", title, url, summary, &syms_str, published, None,
        ) {
            new_count += 1;
            detect_co_mentions(conn, &syms_str);
        }
    }
    Ok(new_count)
}

/// Sync SEC EDGAR filings for a symbol
async fn sync_sec_edgar(
    conn: &Connection,
    sec_client: &reqwest::Client,
    symbol: &str,
    cik: Option<&str>,
    days: u32,
) -> anyhow::Result<usize> {
    let start = (Utc::now() - chrono::Duration::days(days as i64))
        .format("%Y-%m-%d")
        .to_string();
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let url = format!(
        "https://efts.sec.gov/LATEST/search-index?q=%22{}%22&forms=8-K,10-K,10-Q,4&dateRange=custom&startdt={}&enddt={}&from=0&size=20",
        symbol, start, today
    );
    let resp = match sec_client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  ⚠️  SEC EDGAR for {}: {}", symbol, e);
            return Ok(0);
        }
    };
    if !resp.status().is_success() {
        return Ok(0);
    }
    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("  ⚠️  SEC parse for {}: {}", symbol, e);
            return Ok(0);
        }
    };

    let mut new_count = 0;
    if let Some(hits) = body
        .get("hits")
        .and_then(|h| h.get("hits"))
        .and_then(|h| h.as_array())
    {
        for hit in hits {
            let src = hit.get("_source").unwrap_or(hit);
            let form = src.get("form").and_then(|f| f.as_str()).unwrap_or("");
            let file_date = src.get("file_date").and_then(|d| d.as_str()).unwrap_or("");
            let adsh = src.get("adsh").and_then(|a| a.as_str()).unwrap_or("");
            let display_names = src
                .get("display_names")
                .and_then(|d| d.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let file_desc = src
                .get("file_description")
                .and_then(|d| d.as_str())
                .unwrap_or("");

            let title = format!("[{}] {} — {}", form, display_names, file_desc);
            let url = format!(
                "https://www.sec.gov/Archives/edgar/data/{}",
                adsh.replace('-', "")
            );

            // Extract symbols from display names
            let mut found_syms = vec![symbol.to_string()];
            // Display names format: "Company Inc.  (TICK)  (CIK ...)"
            let re = regex::Regex::new(r"\(([A-Z]{1,5})\)").unwrap();
            for cap in re.captures_iter(&display_names) {
                let ticker = &cap[1];
                if ticker != "CIK" && !found_syms.contains(&ticker.to_string()) {
                    found_syms.push(ticker.to_string());
                }
            }
            let syms_str = found_syms.join(",");

            if upsert_article(
                conn,
                "sec_edgar",
                &title,
                &url,
                Some(file_desc),
                &syms_str,
                Some(file_date),
                Some(form),
            ) {
                new_count += 1;
                detect_co_mentions(conn, &syms_str);
            }
        }
    }

    // Also try the structured submissions API if we have a CIK
    if let Some(cik_val) = cik {
        if !cik_val.is_empty() {
            let padded = format!("{:0>10}", cik_val);
            let sub_url = format!("https://data.sec.gov/submissions/CIK{}.json", padded);
            if let Ok(resp) = sec_client.get(&sub_url).send().await {
                if resp.status().is_success() {
                    if let Ok(sub_data) = resp.json::<serde_json::Value>().await {
                        if let Some(recent) = sub_data.get("filings").and_then(|f| f.get("recent"))
                        {
                            let forms: Vec<&str> = recent
                                .get("form")
                                .and_then(|f| f.as_array())
                                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                                .unwrap_or_default();
                            let dates: Vec<&str> = recent
                                .get("filingDate")
                                .and_then(|f| f.as_array())
                                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                                .unwrap_or_default();
                            let accessions: Vec<&str> = recent
                                .get("accessionNumber")
                                .and_then(|f| f.as_array())
                                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                                .unwrap_or_default();
                            let descs: Vec<&str> = recent
                                .get("primaryDocDescription")
                                .and_then(|f| f.as_array())
                                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                                .unwrap_or_default();

                            let limit = forms.len().min(dates.len()).min(accessions.len()).min(10);
                            for i in 0..limit {
                                if !matches!(
                                    forms[i],
                                    "8-K" | "10-K" | "10-Q" | "4" | "SC 13D" | "SC 13G"
                                ) {
                                    continue;
                                }
                                if dates[i] < start.as_str() {
                                    continue;
                                }
                                let desc = descs.get(i).unwrap_or(&"");
                                let title = format!("[{}] {} — {}", forms[i], symbol, desc);
                                let url = format!(
                                    "https://www.sec.gov/Archives/edgar/data/{}/{}",
                                    padded,
                                    accessions[i].replace('-', "")
                                );
                                if upsert_article(
                                    conn,
                                    "sec_edgar",
                                    &title,
                                    &url,
                                    Some(desc),
                                    symbol,
                                    Some(dates[i]),
                                    Some(forms[i]),
                                ) {
                                    new_count += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(new_count)
}

/// Parse RSS/Atom XML and extract items
fn parse_rss_items(xml: &str) -> Vec<(String, String, String, String)> {
    // Returns Vec<(title, link, pubDate, description)>
    let mut items = Vec::new();
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut in_item = false;
    let mut in_entry = false; // Atom format
    let mut current_tag = String::new();
    let mut title = String::new();
    let mut link = String::new();
    let mut pub_date = String::new();
    let mut description = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(ref e)) => {
                let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                match tag_name.as_str() {
                    "item" => {
                        in_item = true;
                        title.clear();
                        link.clear();
                        pub_date.clear();
                        description.clear();
                    }
                    "entry" => {
                        in_entry = true;
                        title.clear();
                        link.clear();
                        pub_date.clear();
                        description.clear();
                    }
                    "link" if in_entry => {
                        // Atom: <link href="..." />
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"href" {
                                link = String::from_utf8_lossy(&attr.value).to_string();
                            }
                        }
                    }
                    _ => {}
                }
                if in_item || in_entry {
                    current_tag = tag_name;
                }
            }
            Ok(quick_xml::events::Event::Empty(ref e)) => {
                let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag_name == "link" && (in_item || in_entry) {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"href" {
                            link = String::from_utf8_lossy(&attr.value).to_string();
                        }
                    }
                }
            }
            Ok(quick_xml::events::Event::Text(ref e)) => {
                if in_item || in_entry {
                    let text = e.unescape().unwrap_or_default().to_string();
                    match current_tag.as_str() {
                        "title" => title.push_str(&text),
                        "link" => {
                            if link.is_empty() {
                                link.push_str(&text);
                            }
                        }
                        "pubDate" | "published" | "updated" => pub_date.push_str(&text),
                        "description" | "summary" | "content" => description.push_str(&text),
                        _ => {}
                    }
                }
            }
            Ok(quick_xml::events::Event::CData(ref e)) => {
                if in_item || in_entry {
                    let text = String::from_utf8_lossy(e.as_ref()).to_string();
                    match current_tag.as_str() {
                        "title" => title.push_str(&text),
                        "description" | "summary" | "content" => description.push_str(&text),
                        _ => {}
                    }
                }
            }
            Ok(quick_xml::events::Event::End(ref e)) => {
                let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if (tag_name == "item" && in_item) || (tag_name == "entry" && in_entry) {
                    if !title.is_empty() && !link.is_empty() {
                        // Strip HTML from description
                        let clean_desc = regex::Regex::new(r"<[^>]+>")
                            .unwrap()
                            .replace_all(&description, "")
                            .trim()
                            .to_string();
                        items.push((title.clone(), link.clone(), pub_date.clone(), clean_desc));
                    }
                    in_item = false;
                    in_entry = false;
                }
                current_tag.clear();
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    items
}

/// Sync Yahoo Finance RSS for a symbol
async fn sync_yahoo_rss(
    conn: &Connection,
    rss_client: &reqwest::Client,
    symbol: &str,
) -> anyhow::Result<usize> {
    let url = format!("https://finance.yahoo.com/rss/headline?s={}", symbol);
    let resp = match rss_client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  ⚠️  Yahoo RSS for {}: {}", symbol, e);
            return Ok(0);
        }
    };
    if !resp.status().is_success() {
        return Ok(0);
    }
    let xml = resp.text().await.unwrap_or_default();
    let items = parse_rss_items(&xml);

    let mut new_count = 0;
    for (title, link, pub_date, desc) in &items {
        // Find other tickers mentioned
        let mut syms = vec![symbol.to_string()];
        let extra = extract_symbols_from_text(conn, &format!("{} {}", title, desc));
        for s in extra {
            if !syms.contains(&s) {
                syms.push(s);
            }
        }
        let syms_str = syms.join(",");
        let pub_dt = if pub_date.is_empty() {
            None
        } else {
            Some(pub_date.as_str())
        };
        let summary = if desc.is_empty() {
            None
        } else {
            Some(desc.as_str())
        };
        if upsert_article(
            conn,
            "yahoo_rss",
            title,
            link,
            summary,
            &syms_str,
            pub_dt,
            None,
        ) {
            new_count += 1;
            detect_co_mentions(conn, &syms_str);
        }
    }
    Ok(new_count)
}

/// Sync Google News RSS for a symbol
async fn sync_google_rss(
    conn: &Connection,
    rss_client: &reqwest::Client,
    symbol: &str,
) -> anyhow::Result<usize> {
    let url = format!(
        "https://news.google.com/rss/search?q={}+stock&hl=en-US&gl=US&ceid=US:en",
        symbol
    );
    let resp = match rss_client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  ⚠️  Google News RSS for {}: {}", symbol, e);
            return Ok(0);
        }
    };
    if !resp.status().is_success() {
        return Ok(0);
    }
    let xml = resp.text().await.unwrap_or_default();
    let items = parse_rss_items(&xml);

    let mut new_count = 0;
    for (title, link, pub_date, desc) in items.iter().take(15) {
        let mut syms = vec![symbol.to_string()];
        let extra = extract_symbols_from_text(conn, &format!("{} {}", title, desc));
        for s in extra {
            if !syms.contains(&s) {
                syms.push(s);
            }
        }
        let syms_str = syms.join(",");
        let pub_dt = if pub_date.is_empty() {
            None
        } else {
            Some(pub_date.as_str())
        };
        let summary = if desc.is_empty() {
            None
        } else {
            Some(desc.as_str())
        };
        if upsert_article(
            conn,
            "google_rss",
            title,
            link,
            summary,
            &syms_str,
            pub_dt,
            None,
        ) {
            new_count += 1;
            detect_co_mentions(conn, &syms_str);
        }
    }
    Ok(new_count)
}

/// Look up CIK for a symbol from SEC EDGAR
async fn lookup_cik(sec_client: &reqwest::Client, symbol: &str) -> Option<String> {
    let url = format!(
        "https://efts.sec.gov/LATEST/search-index?q=%22{}%22&forms=10-K&from=0&size=1",
        symbol
    );
    let resp = sec_client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    let hits = body.get("hits")?.get("hits")?.as_array()?;
    if let Some(hit) = hits.first() {
        let src = hit.get("_source")?;
        if let Some(ciks) = src.get("ciks").and_then(|c| c.as_array()) {
            if let Some(cik) = ciks.first().and_then(|c| c.as_str()) {
                // Verify the display name contains our symbol
                let names = src
                    .get("display_names")
                    .and_then(|d| d.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                if names
                    .to_uppercase()
                    .contains(&format!("({})", symbol.to_uppercase()))
                {
                    return Some(cik.to_string());
                }
            }
        }
    }
    // Fallback: try the company tickers JSON
    let tickers_url = "https://www.sec.gov/files/company_tickers.json";
    if let Ok(resp) = sec_client.get(tickers_url).send().await {
        if resp.status().is_success() {
            if let Ok(data) = resp.json::<serde_json::Value>().await {
                if let Some(obj) = data.as_object() {
                    for (_key, val) in obj {
                        if let Some(ticker) = val.get("ticker").and_then(|t| t.as_str()) {
                            if ticker.eq_ignore_ascii_case(symbol) {
                                if let Some(cik) = val.get("cik_str").and_then(|c| c.as_u64()) {
                                    return Some(cik.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

// Fetches sec ticker ciks from the remote source.
async fn fetch_sec_ticker_ciks(sec_client: &reqwest::Client) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(resp) = sec_client
        .get("https://www.sec.gov/files/company_tickers.json")
        .send()
        .await
    else {
        return out;
    };
    if !resp.status().is_success() {
        return out;
    }
    let Ok(data) = resp.json::<serde_json::Value>().await else {
        return out;
    };
    let Some(obj) = data.as_object() else {
        return out;
    };
    for (_key, val) in obj {
        let Some(ticker) = val.get("ticker").and_then(|value| value.as_str()) else {
            continue;
        };
        let Some(cik) = val.get("cik_str").and_then(|value| value.as_u64()) else {
            continue;
        };
        out.insert(ticker.to_ascii_uppercase(), cik.to_string());
    }
    out
}

// Fetches current sp500 symbols from the remote source.
async fn fetch_current_sp500_symbols(client: &reqwest::Client) -> anyhow::Result<Vec<String>> {
    let html = client
        .get("https://en.wikipedia.org/wiki/List_of_S%26P_500_companies")
        .send()
        .await?
        .text()
        .await?;
    let start = html.find("Ticker_symbol").unwrap_or(0);
    let table_tail = &html[start..];
    let end = table_tail.find("</tbody>").unwrap_or(table_tail.len());
    let table = &table_tail[..end];
    let re = regex::Regex::new(r#"class="external text"[^>]*>([A-Za-z.]+)</a>"#)?;
    let mut symbols = BTreeSet::new();
    for cap in re.captures_iter(table) {
        let normalized = cap[1].trim().to_ascii_uppercase();
        if normalized.is_empty() || looks_like_option_symbol(&normalized) || is_blocked(&normalized)
        {
            continue;
        }
        symbols.insert(normalized);
    }
    let symbols = symbols.into_iter().collect::<Vec<_>>();
    if symbols.len() < 400 {
        anyhow::bail!(
            "current S&P 500 scrape returned only {} symbols; refusing to use partial universe",
            symbols.len()
        );
    }
    Ok(symbols)
}

// Adds feed symbol to local state.
fn add_feed_symbol(
    symbol: &str,
    source: &str,
    sources_by_symbol: &mut HashMap<String, BTreeSet<String>>,
) {
    let normalized = symbol.trim().to_ascii_uppercase();
    if normalized.is_empty() || looks_like_option_symbol(&normalized) || is_blocked(&normalized) {
        return;
    }
    sources_by_symbol
        .entry(normalized)
        .or_default()
        .insert(source.to_string());
}

// Adds db feed symbols to local state.
fn add_db_feed_symbols(
    conn: &Connection,
    query: &str,
    source: &str,
    sources_by_symbol: &mut HashMap<String, BTreeSet<String>>,
) -> anyhow::Result<usize> {
    let mut stmt = conn.prepare(query)?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut count = 0usize;
    for row in rows {
        add_feed_symbol(&row?, source, sources_by_symbol);
        count += 1;
    }
    Ok(count)
}

// Synchronizes ml feed universe with external or local state.
async fn sync_ml_feed_universe(json_out: bool) -> anyhow::Result<()> {
    let cfg = config::feeds_ml_sync_config();
    if !cfg.sync_before_training {
        if json_out {
            print_json_pretty(serde_json::json!({"feed_sync_before_training": "disabled"}))?;
        }
        return Ok(());
    }

    let conn = open_db()?;
    let mut sources_by_symbol: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut source_counts = HashMap::new();

    if cfg.sync_orders_before_training && config::provider_enabled("alpaca") {
        if let Err(err) = auto::sync_orders_all_accounts(false).await {
            eprintln!("  warning: provider order/fill sync before feed universe failed: {err}");
        }
    }

    if cfg.include_current_sp500 {
        let client = build_rss_client();
        match fetch_current_sp500_symbols(&client).await {
            Ok(symbols) => {
                source_counts.insert("current_sp500", symbols.len());
                for symbol in symbols {
                    add_feed_symbol(&symbol, "current_sp500", &mut sources_by_symbol);
                }
            }
            Err(err) => eprintln!("  warning: current S&P 500 feed universe skipped: {err}"),
        }
    }

    if cfg.include_open_positions {
        let mut added = add_db_feed_symbols(
            &conn,
            "SELECT DISTINCT symbol FROM auto_positions WHERE status='open'",
            "open_auto_position",
            &mut sources_by_symbol,
        )?;
        if config::provider_enabled("alpaca") {
            for account in config::alpaca_accounts()? {
                let client = build_client_for(&account);
                match api_get::<Vec<Position>>(
                    &client,
                    &alpaca::broker_api_url_for(&account, "/positions"),
                )
                .await
                {
                    Ok(positions) => {
                        added += positions.len();
                        for position in positions {
                            add_feed_symbol(
                                &position.symbol,
                                "provider_open_position",
                                &mut sources_by_symbol,
                            );
                        }
                    }
                    Err(err) => eprintln!(
                        "  warning: provider positions skipped for {}:{}: {}",
                        account.provider(),
                        account.account_ref(),
                        err
                    ),
                }
            }
        }
        source_counts.insert("open_positions", added);
    }

    if cfg.include_bought_symbols {
        let lookback = format!("-{} days", cfg.bought_symbol_lookback_days);
        let mut stmt = conn.prepare(
            "SELECT DISTINCT symbol FROM provider_fill_activities
             WHERE lower(side)='buy'
               AND (transaction_time IS NULL OR date(transaction_time) >= date('now', ?1))",
        )?;
        let rows = stmt.query_map(params![lookback], |row| row.get::<_, String>(0))?;
        let mut count = 0usize;
        for row in rows {
            add_feed_symbol(&row?, "recent_provider_buy", &mut sources_by_symbol);
            count += 1;
        }
        source_counts.insert("recent_buys", count);
    }

    if cfg.include_q1_candidates {
        let mut stmt = conn.prepare(
            "SELECT symbol FROM ml_predictions
             WHERE date=(SELECT MAX(date) FROM ml_predictions)
               AND predicted_quintile=1
             ORDER BY COALESCE(ensemble_score, predicted_score) DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![cfg.q1_top_n as i64], |row| row.get::<_, String>(0))?;
        let mut count = 0usize;
        for row in rows {
            add_feed_symbol(&row?, "latest_q1_candidate", &mut sources_by_symbol);
            count += 1;
        }
        source_counts.insert("q1_candidates", count);
    }

    for symbol in &cfg.extra_symbols {
        add_feed_symbol(symbol, "config_extra", &mut sources_by_symbol);
    }
    source_counts.insert("config_extra", cfg.extra_symbols.len());

    if sources_by_symbol.is_empty() {
        eprintln!(
            "  warning: feed universe is empty; keeping existing feed subscriptions unchanged"
        );
        return Ok(());
    }

    reconcile_managed_feed_subscriptions(&conn, &sources_by_symbol, !json_out).await?;
    let sync_summary = sync_all_feed_subscriptions(cfg.sync_days, json_out).await?;
    if json_out {
        print_json_pretty(serde_json::json!({
            "feed_sync_before_training": "ok",
            "desired_symbols": sources_by_symbol.len(),
            "source_counts": source_counts,
            "sync": sync_summary,
            "note": "Current S&P 500 membership is used only as a feed collection universe, not as a historical training membership feature."
        }))?;
    } else {
        println!(
            "Feed universe ready: {} managed/explicit symbols; synced {} new articles",
            sources_by_symbol.len(),
            sync_summary["new_articles"].as_u64().unwrap_or(0)
        );
    }
    Ok(())
}

// Handles reconcile managed feed subscriptions logic.
async fn reconcile_managed_feed_subscriptions(
    conn: &Connection,
    sources_by_symbol: &HashMap<String, BTreeSet<String>>,
    show_progress: bool,
) -> anyhow::Result<()> {
    let sec_client = build_sec_client();
    let cik_map = fetch_sec_ticker_ciks(&sec_client).await;
    let now = Utc::now().to_rfc3339();
    let desired = sources_by_symbol.keys().cloned().collect::<BTreeSet<_>>();

    let mut removed = 0usize;
    let existing_managed: Vec<String> = {
        let mut stmt = conn.prepare("SELECT symbol FROM feed_subscriptions WHERE managed=1")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.filter_map(|row| row.ok()).collect()
    };
    for symbol in existing_managed {
        if !desired.contains(&symbol) {
            removed += conn.execute(
                "DELETE FROM feed_subscriptions WHERE symbol=?1 AND managed=1",
                params![symbol],
            )?;
        }
    }

    let progress = progress::bar_if(
        show_progress,
        sources_by_symbol.len() as u64,
        "Reconciling feed universe",
    );
    let mut inserted_or_updated = 0usize;
    for (symbol, sources) in sources_by_symbol {
        let source = sources.iter().cloned().collect::<Vec<_>>().join(",");
        let cik = existing_feed_cik(conn, symbol)?.or_else(|| cik_map.get(symbol).cloned());
        inserted_or_updated += conn.execute(
            "INSERT INTO feed_subscriptions (symbol, cik, added_at, subscription_source, managed)
             VALUES (?1, ?2, ?3, ?4, 1)
             ON CONFLICT(symbol) DO UPDATE SET
                 cik = COALESCE(feed_subscriptions.cik, excluded.cik),
                 subscription_source = CASE
                     WHEN feed_subscriptions.managed=1 THEN excluded.subscription_source
                     ELSE feed_subscriptions.subscription_source
                 END,
                 managed = CASE
                     WHEN feed_subscriptions.managed=1 THEN 1
                     ELSE feed_subscriptions.managed
                 END",
            params![symbol, cik, now, source],
        )?;
        progress.inc(1);
    }
    progress.finish_and_clear();
    eprintln!(
        "Feed subscriptions reconciled: {} desired, {} upserted/manual-kept, {} stale managed removed",
        sources_by_symbol.len(),
        inserted_or_updated,
        removed
    );
    Ok(())
}

// Handles existing feed cik logic.
fn existing_feed_cik(conn: &Connection, symbol: &str) -> anyhow::Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT cik FROM feed_subscriptions WHERE symbol=?1",
            params![symbol],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
        .filter(|value| !value.trim().is_empty()))
}

// Synchronizes all feed subscriptions with external or local state.
async fn sync_all_feed_subscriptions(
    days: u32,
    json_out: bool,
) -> anyhow::Result<serde_json::Value> {
    let conn = open_db()?;
    let mut stmt = conn.prepare("SELECT symbol, cik FROM feed_subscriptions ORDER BY symbol")?;
    let subs: Vec<(String, Option<String>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    if subs.is_empty() {
        let value = serde_json::json!({
            "symbols_synced": 0,
            "new_articles": 0,
            "total_articles": 0,
            "by_source": {},
            "sentiment": {"positive": 0, "negative": 0}
        });
        if json_out {
            print_json_pretty(value.clone())?;
        } else {
            println!("No feed subscriptions. Run `mlai-trade feeds add <symbols>` first.");
        }
        return Ok(value);
    }

    let alpaca_client = build_client();
    let sec_client = build_sec_client();
    let rss_client = build_rss_client();
    let mut total_new = 0usize;
    let mut source_counts: HashMap<String, usize> = HashMap::new();

    if !json_out {
        println!(
            "🔄 Syncing feeds for {} symbols (last {} days)...\n",
            subs.len(),
            days
        );
    }
    let progress = progress::bar_if(!json_out, subs.len() as u64, "Feed article sync");

    for (sym, cik) in &subs {
        progress.set_message(format!("{sym}: Alpaca news"));
        let alpaca_new = sync_alpaca_news(&conn, &alpaca_client, sym, days)
            .await
            .unwrap_or(0);
        *source_counts.entry("alpaca".into()).or_insert(0) += alpaca_new;

        progress.set_message(format!("{sym}: SEC EDGAR"));
        let sec_new = sync_sec_edgar(&conn, &sec_client, sym, cik.as_deref(), days)
            .await
            .unwrap_or(0);
        *source_counts.entry("sec_edgar".into()).or_insert(0) += sec_new;
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        progress.set_message(format!("{sym}: Yahoo RSS"));
        let yahoo_new = sync_yahoo_rss(&conn, &rss_client, sym).await.unwrap_or(0);
        *source_counts.entry("yahoo_rss".into()).or_insert(0) += yahoo_new;

        progress.set_message(format!("{sym}: Google RSS"));
        let google_new = sync_google_rss(&conn, &rss_client, sym).await.unwrap_or(0);
        *source_counts.entry("google_rss".into()).or_insert(0) += google_new;

        let sym_total = alpaca_new + sec_new + yahoo_new + google_new;
        total_new += sym_total;
        progress.set_message(format!("{total_new} new articles"));
        progress.inc(1);

        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE feed_subscriptions SET last_sync = ?1 WHERE symbol = ?2",
            params![now, sym],
        )?;
    }
    progress.finish_and_clear();

    let pos_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM news_articles WHERE sentiment_score > 0.15",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let neg_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM news_articles WHERE sentiment_score < -0.15",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let total_articles: i64 = conn
        .query_row("SELECT COUNT(*) FROM news_articles", [], |r| r.get(0))
        .unwrap_or(0);

    let value = serde_json::json!({
        "symbols_synced": subs.len(),
        "new_articles": total_new,
        "total_articles": total_articles,
        "by_source": source_counts,
        "sentiment": {"positive": pos_count, "negative": neg_count}
    });

    if json_out {
        print_json_pretty(value.clone())?;
    } else {
        println!(
            "\n✅ Synced {} new articles for {} symbols",
            total_new,
            subs.len()
        );
        println!("   Total in DB: {}", total_articles);
        if let Some(source_counts) = value["by_source"].as_object() {
            for (src, cnt) in source_counts {
                if cnt.as_u64().unwrap_or(0) > 0 {
                    println!("   {}: {} new", src, cnt);
                }
            }
        }
        println!(
            "   Sentiment: {} positive, {} negative",
            pos_count, neg_count
        );
    }
    Ok(value)
}

// Handles the feeds CLI action.
async fn cmd_feeds(action: FeedsAction, json_out: bool) -> anyhow::Result<()> {
    match action {
        FeedsAction::Add { symbols } => {
            let conn = open_db()?;
            let sec_client = build_sec_client();
            let now = Utc::now().to_rfc3339();
            let mut added = Vec::new();
            let progress =
                progress::bar_if(!json_out, symbols.len() as u64, "Feed subscription setup");
            for sym in &symbols {
                let s = sym.to_uppercase();
                progress.set_message(format!("checking {s}"));
                check_blocked(&s)?;
                // Check if already subscribed
                let exists: bool = conn
                    .query_row(
                        "SELECT COUNT(*) FROM feed_subscriptions WHERE symbol = ?1",
                        params![s],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap_or(0)
                    > 0;
                if exists {
                    println!("  {} already subscribed", s);
                    progress.inc(1);
                    continue;
                }
                // Look up CIK
                progress.set_message(format!("SEC EDGAR lookup {s}"));
                let cik = lookup_cik(&sec_client, &s).await;
                conn.execute(
                    "INSERT INTO feed_subscriptions (symbol, cik, added_at, subscription_source, managed)
                     VALUES (?1, ?2, ?3, 'manual', 0)
                     ON CONFLICT(symbol) DO UPDATE SET
                         cik=COALESCE(feed_subscriptions.cik, excluded.cik),
                         subscription_source='manual',
                         managed=0",
                    params![s, cik, now],
                )?;
                added.push(s);
                progress.inc(1);
                // Rate limit for SEC
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            progress.finish_and_clear();
            if json_out {
                let obj = serde_json::json!({"added": added, "count": added.len()});
                print_json_pretty(obj)?;
            } else {
                println!(
                    "✅ Subscribed to {} symbol(s): {}",
                    added.len(),
                    added.join(", ")
                );
                println!("   Run `mlai-trade feeds sync` to pull articles.");
            }
        }

        FeedsAction::Remove { symbol } => {
            let conn = open_db()?;
            let s = symbol.to_uppercase();
            let removed = conn.execute(
                "DELETE FROM feed_subscriptions WHERE symbol = ?1",
                params![s],
            )?;
            if removed == 0 {
                if json_out {
                    print_json_pretty(serde_json::json!({
                        "ok": false,
                        "error": format!("{} was not subscribed", s),
                        "status_code": 404,
                        "symbol": s,
                    }))?;
                }
                anyhow::bail!("{} was not subscribed", s);
            }
            if json_out {
                print_json_pretty(serde_json::json!({"removed": s, "ok": true}))?;
            } else {
                println!("✅ Unsubscribed from {}", s);
            }
        }

        FeedsAction::Sync { days } => {
            let _ = sync_all_feed_subscriptions(days, json_out).await?;
        }

        FeedsAction::List => {
            let conn = open_db()?;
            let mut stmt = conn.prepare(
                "SELECT fs.symbol, fs.cik, fs.added_at, fs.last_sync,
                        (SELECT COUNT(*) FROM news_articles WHERE symbols LIKE '%' || fs.symbol || '%')
                 FROM feed_subscriptions fs ORDER BY fs.symbol"
            )?;

            struct SubRow {
                symbol: String,
                cik: Option<String>,
                added_at: String,
                last_sync: Option<String>,
                article_count: i64,
            }
            let rows: Vec<SubRow> = stmt
                .query_map([], |r| {
                    Ok(SubRow {
                        symbol: r.get(0)?,
                        cik: r.get(1)?,
                        added_at: r.get(2)?,
                        last_sync: r.get(3)?,
                        article_count: r.get(4)?,
                    })
                })?
                .filter_map(|r| r.ok())
                .collect();

            if json_out {
                let items: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "symbol": r.symbol, "cik": r.cik, "added_at": r.added_at,
                            "last_sync": r.last_sync, "article_count": r.article_count,
                        })
                    })
                    .collect();
                print_json_pretty(serde_json::json!(items))?;
                return Ok(());
            }

            if rows.is_empty() {
                println!("No subscriptions. Run `mlai-trade feeds add <symbols>` to start.");
                return Ok(());
            }
            println!("📡 Feed Subscriptions ({} symbols)\n", rows.len());
            println!(
                "{:<8} {:<12} {:>10} {:<20} {:>8}",
                "Symbol", "CIK", "Articles", "Last Sync", "Since"
            );
            println!("{}", "-".repeat(65));
            for r in &rows {
                let cik = r.cik.as_deref().unwrap_or("—");
                let sync = r
                    .last_sync
                    .as_deref()
                    .map(|s| if s.len() >= 19 { &s[..19] } else { s })
                    .unwrap_or("never");
                let since = if r.added_at.len() >= 10 {
                    &r.added_at[..10]
                } else {
                    &r.added_at
                };
                println!(
                    "{:<8} {:<12} {:>10} {:<20} {:>8}",
                    r.symbol, cik, r.article_count, sync, since
                );
            }
        }

        FeedsAction::Search { query, limit } => {
            let conn = open_db()?;
            let pattern = format!("%{}%", query);
            let mut stmt = conn.prepare(
                "SELECT source, title, url, symbols, published_at, sentiment_score, filing_type
                 FROM news_articles WHERE title LIKE ?1 OR summary LIKE ?1
                 ORDER BY published_at DESC LIMIT ?2",
            )?;

            struct ArticleRow {
                source: String,
                title: String,
                url: String,
                symbols: String,
                published_at: Option<String>,
                sentiment: Option<f64>,
                filing_type: Option<String>,
            }
            let rows: Vec<ArticleRow> = stmt
                .query_map(params![pattern, limit], |r| {
                    Ok(ArticleRow {
                        source: r.get(0)?,
                        title: r.get(1)?,
                        url: r.get(2)?,
                        symbols: r.get(3)?,
                        published_at: r.get(4)?,
                        sentiment: r.get(5)?,
                        filing_type: r.get(6)?,
                    })
                })?
                .filter_map(|r| r.ok())
                .collect();

            if json_out {
                let items: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "source": r.source, "title": r.title, "url": r.url,
                            "symbols": r.symbols, "published_at": r.published_at,
                            "sentiment": r.sentiment, "filing_type": r.filing_type,
                        })
                    })
                    .collect();
                print_json_pretty(serde_json::json!({"results": items, "count": items.len()}))?;
                return Ok(());
            }

            if rows.is_empty() {
                println!("No articles matching \"{}\"", query);
                return Ok(());
            }
            println!("🔍 Search: \"{}\" — {} results\n", query, rows.len());
            for r in &rows {
                let sent_icon = match r.sentiment {
                    Some(s) if s > 0.15 => "📈",
                    Some(s) if s < -0.15 => "📉",
                    _ => "➖",
                };
                let sent_str = r
                    .sentiment
                    .map(|s| format!("{:+.2}", s))
                    .unwrap_or_else(|| "—".into());
                let date = r
                    .published_at
                    .as_deref()
                    .map(|d| if d.len() >= 10 { &d[..10] } else { d })
                    .unwrap_or("?");
                let ft = r.filing_type.as_deref().unwrap_or("");
                let tag = if ft.is_empty() {
                    r.source.clone()
                } else {
                    format!("{}/{}", r.source, ft)
                };
                println!("{} [{}] [{}] {}", sent_icon, date, tag, r.title);
                println!("   Symbols: {} | Sentiment: {}", r.symbols, sent_str);
                println!("   {}\n", r.url);
            }
        }

        FeedsAction::Graph { symbol } => {
            let sym = symbol.to_uppercase();
            let conn = open_db()?;
            let mut stmt = conn.prepare(
                "SELECT symbol_a, symbol_b, relationship, strength, source
                 FROM company_relationships
                 WHERE symbol_a = ?1 OR symbol_b = ?1
                 ORDER BY strength DESC LIMIT 30",
            )?;

            struct RelRow {
                symbol_a: String,
                symbol_b: String,
                relationship: String,
                strength: f64,
                source: Option<String>,
            }
            let rows: Vec<RelRow> = stmt
                .query_map(params![sym], |r| {
                    Ok(RelRow {
                        symbol_a: r.get(0)?,
                        symbol_b: r.get(1)?,
                        relationship: r.get(2)?,
                        strength: r.get(3)?,
                        source: r.get(4)?,
                    })
                })?
                .filter_map(|r| r.ok())
                .collect();

            // Also get price correlations
            let mut corr_stmt = conn.prepare(
                "SELECT symbol_a, symbol_b, correlation_30d, correlation_90d
                 FROM price_correlations
                 WHERE (symbol_a = ?1 OR symbol_b = ?1) AND ABS(correlation_30d) > 0.5
                 ORDER BY ABS(correlation_30d) DESC LIMIT 15",
            )?;
            struct CorrRow {
                symbol_a: String,
                symbol_b: String,
                corr_30d: Option<f64>,
                corr_90d: Option<f64>,
            }
            let corrs: Vec<CorrRow> = corr_stmt
                .query_map(params![sym], |r| {
                    Ok(CorrRow {
                        symbol_a: r.get(0)?,
                        symbol_b: r.get(1)?,
                        corr_30d: r.get(2)?,
                        corr_90d: r.get(3)?,
                    })
                })?
                .filter_map(|r| r.ok())
                .collect();

            if json_out {
                let rels: Vec<serde_json::Value> = rows.iter().map(|r| serde_json::json!({
                    "other": if r.symbol_a == sym { &r.symbol_b } else { &r.symbol_a },
                    "relationship": r.relationship, "strength": r.strength, "source": r.source,
                })).collect();
                let corr_items: Vec<serde_json::Value> = corrs
                    .iter()
                    .map(|c| {
                        serde_json::json!({
                            "other": if c.symbol_a == sym { &c.symbol_b } else { &c.symbol_a },
                            "correlation_30d": c.corr_30d, "correlation_90d": c.corr_90d,
                        })
                    })
                    .collect();
                print_json_pretty(serde_json::json!({
                    "symbol": sym, "relationships": rels, "correlations": corr_items
                }))?;
                return Ok(());
            }

            println!("🔗 Company Relationship Graph — {}\n", sym);
            if rows.is_empty() && corrs.is_empty() {
                println!("No relationships found. Run `mlai-trade feeds sync` and `mlai-trade feeds correlate` first.");
                return Ok(());
            }

            if !rows.is_empty() {
                println!(
                    "{:<8} {:<16} {:>10} {:<20}",
                    "Related", "Type", "Strength", "Source"
                );
                println!("{}", "-".repeat(58));
                for r in &rows {
                    let other = if r.symbol_a == sym {
                        &r.symbol_b
                    } else {
                        &r.symbol_a
                    };
                    let icon = match r.relationship.as_str() {
                        "co_mention" => "📰",
                        "supply_chain" => "🏭",
                        "same_industry" => "🏢",
                        "insider_link" => "👤",
                        "price_correlated" => "📊",
                        _ => "🔗",
                    };
                    println!(
                        "{} {:<6} {:<16} {:>10.1} {:<20}",
                        icon,
                        other,
                        r.relationship,
                        r.strength,
                        r.source.as_deref().unwrap_or("—")
                    );
                }
            }

            if !corrs.is_empty() {
                println!("\n📊 Price Correlations");
                println!("{:<8} {:>12} {:>12}", "Symbol", "30d Corr", "90d Corr");
                println!("{}", "-".repeat(35));
                for c in &corrs {
                    let other = if c.symbol_a == sym {
                        &c.symbol_b
                    } else {
                        &c.symbol_a
                    };
                    let c30 = c
                        .corr_30d
                        .map(|v| format!("{:+.3}", v))
                        .unwrap_or_else(|| "—".into());
                    let c90 = c
                        .corr_90d
                        .map(|v| format!("{:+.3}", v))
                        .unwrap_or_else(|| "—".into());
                    let icon = match c.corr_30d {
                        Some(v) if v > 0.7 => "🟢",
                        Some(v) if v > 0.3 => "🟡",
                        Some(v) if v < -0.3 => "🔴",
                        _ => "⚪",
                    };
                    println!("{} {:<6} {:>12} {:>12}", icon, other, c30, c90);
                }
            }
        }

        FeedsAction::Sentiment { symbol } => {
            let sym = symbol.to_uppercase();
            let conn = open_db()?;

            // Average sentiment over different windows
            let avg_7d: f64 = conn
                .query_row(
                    "SELECT COALESCE(AVG(sentiment_score), 0.0) FROM news_articles
                 WHERE symbols LIKE ?1 AND published_at > datetime('now', '-7 days')",
                    params![format!("%{}%", sym)],
                    |r| r.get(0),
                )
                .unwrap_or(0.0);
            let avg_30d: f64 = conn
                .query_row(
                    "SELECT COALESCE(AVG(sentiment_score), 0.0) FROM news_articles
                 WHERE symbols LIKE ?1 AND published_at > datetime('now', '-30 days')",
                    params![format!("%{}%", sym)],
                    |r| r.get(0),
                )
                .unwrap_or(0.0);
            let count_7d: i64 = conn.query_row(
                "SELECT COUNT(*) FROM news_articles WHERE symbols LIKE ?1 AND published_at > datetime('now', '-7 days')",
                params![format!("%{}%", sym)], |r| r.get(0)
            ).unwrap_or(0);
            let count_30d: i64 = conn.query_row(
                "SELECT COUNT(*) FROM news_articles WHERE symbols LIKE ?1 AND published_at > datetime('now', '-30 days')",
                params![format!("%{}%", sym)], |r| r.get(0)
            ).unwrap_or(0);

            // Count by source
            let mut src_stmt = conn.prepare(
                "SELECT source, COUNT(*) FROM news_articles WHERE symbols LIKE ?1 GROUP BY source ORDER BY COUNT(*) DESC"
            )?;
            let src_counts: Vec<(String, i64)> = src_stmt
                .query_map(params![format!("%{}%", sym)], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?
                .filter_map(|r| r.ok())
                .collect();

            // Recent articles
            let mut recent_stmt = conn.prepare(
                "SELECT title, published_at, sentiment_score, source FROM news_articles
                 WHERE symbols LIKE ?1 ORDER BY published_at DESC LIMIT 10",
            )?;
            struct RecentRow {
                title: String,
                published_at: Option<String>,
                sentiment: Option<f64>,
                source: String,
            }
            let recents: Vec<RecentRow> = recent_stmt
                .query_map(params![format!("%{}%", sym)], |r| {
                    Ok(RecentRow {
                        title: r.get(0)?,
                        published_at: r.get(1)?,
                        sentiment: r.get(2)?,
                        source: r.get(3)?,
                    })
                })?
                .filter_map(|r| r.ok())
                .collect();

            // SEC filings count
            let sec_8k: i64 = conn.query_row(
                "SELECT COUNT(*) FROM news_articles WHERE symbols LIKE ?1 AND filing_type = '8-K'",
                params![format!("%{}%", sym)], |r| r.get(0)
            ).unwrap_or(0);
            let sec_form4: i64 = conn.query_row(
                "SELECT COUNT(*) FROM news_articles WHERE symbols LIKE ?1 AND filing_type = '4'",
                params![format!("%{}%", sym)], |r| r.get(0)
            ).unwrap_or(0);

            if json_out {
                let obj = serde_json::json!({
                    "symbol": sym,
                    "sentiment_7d": avg_7d, "sentiment_30d": avg_30d,
                    "articles_7d": count_7d, "articles_30d": count_30d,
                    "sec_8k_count": sec_8k, "sec_form4_count": sec_form4,
                    "by_source": src_counts.iter().map(|(s,c)| serde_json::json!({"source": s, "count": c})).collect::<Vec<_>>(),
                    "recent": recents.iter().map(|r| serde_json::json!({
                        "title": r.title, "published_at": r.published_at, "sentiment": r.sentiment, "source": r.source,
                    })).collect::<Vec<serde_json::Value>>(),
                });
                print_json_pretty(obj)?;
                return Ok(());
            }

            let trend = if avg_7d > 0.15 {
                "📈 Bullish"
            } else if avg_7d < -0.15 {
                "📉 Bearish"
            } else {
                "➖ Neutral"
            };
            println!("📰 Sentiment Report — {}\n", sym);
            println!(
                "  7-day sentiment:   {:+.3} ({} articles) — {}",
                avg_7d, count_7d, trend
            );
            println!(
                "  30-day sentiment:  {:+.3} ({} articles)",
                avg_30d, count_30d
            );
            if sec_8k > 0 {
                println!("  📋 SEC 8-K filings: {}", sec_8k);
            }
            if sec_form4 > 0 {
                println!("  👤 Insider trades (Form 4): {}", sec_form4);
            }

            if !src_counts.is_empty() {
                println!("\n  Sources:");
                for (src, cnt) in &src_counts {
                    println!("    {}: {} articles", src, cnt);
                }
            }

            if !recents.is_empty() {
                println!("\n  Recent Headlines:");
                for r in &recents {
                    let sent_icon = match r.sentiment {
                        Some(s) if s > 0.15 => "📈",
                        Some(s) if s < -0.15 => "📉",
                        _ => "➖",
                    };
                    let sent_str = r
                        .sentiment
                        .map(|s| format!("{:+.2}", s))
                        .unwrap_or_else(|| "—".into());
                    let date = r
                        .published_at
                        .as_deref()
                        .map(|d| if d.len() >= 10 { &d[..10] } else { d })
                        .unwrap_or("?");
                    println!("    {} [{}] {} ({})", sent_icon, date, r.title, sent_str);
                }
            }
        }

        FeedsAction::Correlate { days } => {
            let conn = open_db()?;
            let mut stmt = conn.prepare("SELECT symbol FROM feed_subscriptions ORDER BY symbol")?;
            let symbols: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect();

            if symbols.len() < 2 {
                println!("❌ Need at least 2 subscribed symbols. Run `mlai-trade feeds add <symbols>` first.");
                return Ok(());
            }

            println!(
                "📊 Computing price correlations for {} symbols over {} days...\n",
                symbols.len(),
                days
            );

            // Get daily returns for each symbol
            let mut returns_map: HashMap<String, Vec<(String, f64)>> = HashMap::new();
            let progress =
                progress::bar_if(!json_out, symbols.len() as u64, "Loading return windows");
            for sym in &symbols {
                progress.set_message(sym);
                let mut bstmt = conn.prepare(
                    "SELECT date, close FROM bars WHERE symbol = ?1 ORDER BY date DESC LIMIT ?2",
                )?;
                let bars: Vec<(String, f64)> = bstmt
                    .query_map(params![sym, days + 1], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .filter_map(|r| r.ok())
                    .collect();

                if bars.len() < 10 {
                    progress.inc(1);
                    continue;
                }
                let mut rets = Vec::new();
                for i in 0..bars.len() - 1 {
                    if bars[i + 1].1 > 0.0 {
                        rets.push((
                            bars[i].0.clone(),
                            (bars[i].1 - bars[i + 1].1) / bars[i + 1].1,
                        ));
                    }
                }
                returns_map.insert(sym.clone(), rets);
                progress.inc(1);
            }
            progress.finish_and_clear();

            let now = Utc::now().to_rfc3339();
            let mut pairs: Vec<(String, String, f64, Option<f64>)> = Vec::new();

            let syms_with_data: Vec<String> = returns_map.keys().cloned().collect();
            let pair_total = syms_with_data
                .len()
                .saturating_mul(syms_with_data.len().saturating_sub(1))
                / 2;
            let progress = progress::bar_if(!json_out, pair_total as u64, "Computing correlations");
            for i in 0..syms_with_data.len() {
                for j in (i + 1)..syms_with_data.len() {
                    let (sym_a, sym_b) = (&syms_with_data[i], &syms_with_data[j]);
                    progress.set_message(format!("{sym_a}/{sym_b}"));
                    let rets_a = &returns_map[sym_a];
                    let rets_b = &returns_map[sym_b];

                    // Align by date
                    let dates_a: HashMap<&str, f64> =
                        rets_a.iter().map(|(d, r)| (d.as_str(), *r)).collect();
                    let common: Vec<(f64, f64)> = rets_b
                        .iter()
                        .filter_map(|(d, rb)| dates_a.get(d.as_str()).map(|ra| (*ra, *rb)))
                        .collect();

                    if common.len() < 10 {
                        progress.inc(1);
                        continue;
                    }

                    // Compute Pearson correlation for 30d window
                    let window_30 = common.len().min(days as usize);
                    let corr_30 = pearson_correlation(&common[..window_30]);

                    // Also 90d if we have enough data
                    let corr_90 = if common.len() >= 60 {
                        let w90 = common.len().min(90);
                        Some(pearson_correlation(&common[..w90]))
                    } else {
                        None
                    };

                    // Store
                    let (a, b) = if sym_a < sym_b {
                        (sym_a.clone(), sym_b.clone())
                    } else {
                        (sym_b.clone(), sym_a.clone())
                    };
                    conn.execute(
                        "INSERT OR REPLACE INTO price_correlations (symbol_a, symbol_b, correlation_30d, correlation_90d, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![a, b, corr_30, corr_90, now],
                    )?;

                    // If high correlation, record relationship
                    if corr_30.abs() > 0.7 {
                        let _ = conn.execute(
                            "INSERT OR REPLACE INTO company_relationships (symbol_a, symbol_b, relationship, strength, source, discovered_at)
                             VALUES (?1, ?2, 'price_correlated', ?3, 'bar_correlation', ?4)",
                            params![a, b, corr_30.abs(), now],
                        );
                    }

                    pairs.push((a, b, corr_30, corr_90));
                    progress.inc(1);
                }
            }
            progress.finish_and_clear();

            pairs.sort_by(|a, b| {
                b.2.abs()
                    .partial_cmp(&a.2.abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            if json_out {
                let items: Vec<serde_json::Value> = pairs.iter().map(|(a, b, c30, c90)| serde_json::json!({
                    "symbol_a": a, "symbol_b": b, "correlation_30d": c30, "correlation_90d": c90,
                })).collect();
                print_json_pretty(serde_json::json!({"pairs": items, "count": items.len()}))?;
                return Ok(());
            }

            if pairs.is_empty() {
                println!("No correlations computed (insufficient bar data overlap).");
                return Ok(());
            }

            let show = pairs.len().min(20);
            println!(
                "{:<8} {:<8} {:>12} {:>12} {}",
                "Sym A", "Sym B", "30d Corr", "90d Corr", "Interpretation"
            );
            println!("{}", "-".repeat(65));
            for (a, b, c30, c90) in pairs.iter().take(show) {
                let c90_str = c90
                    .map(|v| format!("{:+.3}", v))
                    .unwrap_or_else(|| "—".into());
                let icon = if *c30 > 0.7 {
                    "🟢 Strong +"
                } else if *c30 > 0.3 {
                    "🟡 Moderate +"
                } else if *c30 < -0.7 {
                    "🔴 Strong -"
                } else if *c30 < -0.3 {
                    "🟡 Moderate -"
                } else {
                    "⚪ Weak"
                };
                println!("{:<8} {:<8} {:>+12.3} {:>12} {}", a, b, c30, c90_str, icon);
            }
            let strong = pairs
                .iter()
                .filter(|(_, _, c30, _)| c30.abs() > 0.7)
                .count();
            println!(
                "\n✅ {} pairs computed, {} strong correlations (|r| > 0.7)",
                pairs.len(),
                strong
            );
        }

        FeedsAction::Status => {
            let conn = open_db()?;
            let sub_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM feed_subscriptions", [], |r| r.get(0))
                .unwrap_or(0);
            let article_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM news_articles", [], |r| r.get(0))
                .unwrap_or(0);
            let rel_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM company_relationships", [], |r| {
                    r.get(0)
                })
                .unwrap_or(0);
            let corr_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM price_correlations", [], |r| r.get(0))
                .unwrap_or(0);
            let last_sync: String = conn
                .query_row("SELECT MAX(last_sync) FROM feed_subscriptions", [], |r| {
                    r.get(0)
                })
                .unwrap_or_else(|_| "never".into());
            let oldest_article: String = conn
                .query_row("SELECT MIN(published_at) FROM news_articles", [], |r| {
                    r.get(0)
                })
                .unwrap_or_else(|_| "—".into());
            let newest_article: String = conn
                .query_row("SELECT MAX(published_at) FROM news_articles", [], |r| {
                    r.get(0)
                })
                .unwrap_or_else(|_| "—".into());

            // By source
            let mut src_stmt = conn.prepare(
                "SELECT source, COUNT(*) FROM news_articles GROUP BY source ORDER BY COUNT(*) DESC",
            )?;
            let src_counts: Vec<(String, i64)> = src_stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .filter_map(|r| r.ok())
                .collect();

            // Sentiment distribution
            let pos: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM news_articles WHERE sentiment_score > 0.15",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            let neg: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM news_articles WHERE sentiment_score < -0.15",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            let neutral = article_count - pos - neg;

            if json_out {
                let obj = serde_json::json!({
                    "subscriptions": sub_count, "articles": article_count,
                    "relationships": rel_count, "correlations": corr_count,
                    "last_sync": last_sync, "article_range": [oldest_article, newest_article],
                    "by_source": src_counts.iter().map(|(s,c)| serde_json::json!({s: c})).collect::<Vec<_>>(),
                    "sentiment": {"positive": pos, "negative": neg, "neutral": neutral},
                });
                print_json_pretty(obj)?;
                return Ok(());
            }

            println!("📡 Feeds Status\n");
            println!("  Subscriptions:    {} symbols", sub_count);
            println!("  Articles:         {} total", article_count);
            println!(
                "  Article range:    {} → {}",
                oldest_article, newest_article
            );
            println!("  Relationships:    {} edges", rel_count);
            println!("  Correlations:     {} pairs", corr_count);
            let sync_short = if last_sync.len() >= 19 {
                &last_sync[..19]
            } else {
                &last_sync
            };
            println!("  Last sync:        {}", sync_short);
            if !src_counts.is_empty() {
                println!("\n  By source:");
                for (src, cnt) in &src_counts {
                    println!("    {}: {}", src, cnt);
                }
            }
            println!(
                "\n  Sentiment: 📈 {} positive | ➖ {} neutral | 📉 {} negative",
                pos, neutral, neg
            );
        }
    }
    Ok(())
}

// Computes pearson correlation correlation for model evaluation.
fn pearson_correlation(pairs: &[(f64, f64)]) -> f64 {
    let n = pairs.len() as f64;
    if n < 2.0 {
        return 0.0;
    }
    let (sum_x, sum_y, sum_xy, sum_x2, sum_y2) = pairs.iter().fold(
        (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64),
        |(sx, sy, sxy, sx2, sy2), (x, y)| (sx + x, sy + y, sxy + x * y, sx2 + x * x, sy2 + y * y),
    );
    let numerator = n * sum_xy - sum_x * sum_y;
    let denom = ((n * sum_x2 - sum_x * sum_x) * (n * sum_y2 - sum_y * sum_y)).sqrt();
    if denom == 0.0 {
        0.0
    } else {
        (numerator / denom).clamp(-1.0, 1.0)
    }
}

// ── Main ─────────────────────────────────────────────────────────

// Entrypoint that initializes runtime paths, dispatches CLI commands, and logs outcomes.
fn main() {
    let cli = parse_cli_or_exit();
    let help_path = command_help_path(&cli.command);
    if let Some(home) = &cli.home {
        std::env::set_var("MLAI_TRADE_HOME", home);
    }
    if let Err(err) = paths::ensure_runtime_dirs() {
        exit_with_error_and_help(
            &format!("unable to initialize runtime directories: {}", err),
            &help_path,
            cli.json,
        );
    }
    let json_flag = cli.json;
    let command_path = help_path.clone();
    let log_components = command_log_components(&cli.command);
    let command_started = Instant::now();
    if let Err(err) = config::load() {
        let message = err.to_string();
        let mut components = log_components.clone();
        push_unique_component(&mut components, "runtime");
        log_command_event(
            &components,
            "config_invalid",
            &command_path,
            command_started,
            Some(&message),
        );
        exit_with_error_and_help(&message, &help_path, json_flag);
    }
    init_global_cpu_worker_pool();
    let worker_threads = config::cpu_worker_threads();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .thread_name("mlai-trade-io")
        .enable_all()
        .build()
        .unwrap_or_else(|err| {
            exit_with_error_and_help(
                &format!("unable to initialize async runtime: {err}"),
                &help_path,
                json_flag,
            )
        });
    runtime.block_on(async_main(
        cli,
        help_path,
        json_flag,
        command_path,
        log_components,
        command_started,
    ));
}

// Dispatches CLI commands inside the configured multi-thread async runtime.
async fn async_main(
    cli: Cli,
    help_path: Vec<&'static str>,
    json_flag: bool,
    command_path: Vec<&'static str>,
    log_components: Vec<&'static str>,
    command_started: Instant,
) {
    let provider_required = !matches!(
        &cli.command,
        Commands::Version
            | Commands::Completions { .. }
            | Commands::Api { .. }
            | Commands::ApiRun
            | Commands::Runtime {
                action: RuntimeAction::Version | RuntimeAction::Completions { .. }
            }
    );
    if provider_required {
        if let Err(err) = config::require_enabled_provider() {
            exit_with_error_and_help(&err.to_string(), &help_path, json_flag);
        }
    }
    log_command_event(
        &log_components,
        "command_started",
        &command_path,
        command_started,
        None,
    );
    let result = match cli.command {
        Commands::Runtime { action } => match action {
            RuntimeAction::Version => cmd_version(json_flag),
            RuntimeAction::Completions { action } => cmd_completions(action, json_flag),
        },
        Commands::Trade { action } => match action {
            TradeAction::Account { accounts } => cmd_account(accounts, json_flag).await,
            TradeAction::Buy {
                symbol,
                qty,
                accounts,
                r#type,
                limit_price,
                stop_price,
                tif,
            } => cmd_buy(symbol, qty, accounts, r#type, limit_price, stop_price, tif).await,
            TradeAction::Sell {
                symbol,
                qty,
                accounts,
                r#type,
                limit_price,
                stop_price,
                tif,
            } => cmd_sell(symbol, qty, accounts, r#type, limit_price, stop_price, tif).await,
            TradeAction::Positions { accounts } => cmd_positions(accounts, json_flag).await,
            TradeAction::Orders {
                accounts,
                status,
                limit,
                sync,
            } => cmd_orders(accounts, status, limit, sync, json_flag).await,
            TradeAction::Cancel { order_id, accounts } => cmd_cancel(order_id, accounts).await,
            TradeAction::Close { symbol, accounts } => cmd_close(symbol, accounts).await,
        },
        Commands::Market { action } => match action {
            MarketAction::DataFeed => cmd_data_feed(json_flag),
            MarketAction::Quote { symbol } => cmd_quote(symbol, json_flag).await,
            MarketAction::Watch { symbols } => cmd_watch(symbols).await,
            MarketAction::Bars {
                symbol,
                timeframe,
                limit,
            } => cmd_bars_single(symbol, timeframe, limit).await,
            MarketAction::News { symbol, limit } => cmd_news(symbol, limit, json_flag).await,
            MarketAction::Sp500 { days } => cmd_sp500(days, json_flag).await,
            MarketAction::HistoryStart { symbols } => cmd_history_start(symbols, json_flag).await,
            MarketAction::Clock => cmd_clock(json_flag).await,
            MarketAction::Calendar {
                start,
                end,
                markets,
            } => cmd_calendar(start, end, markets, json_flag).await,
        },
        Commands::Data { action } => match action {
            DataAction::Universe => cmd_universe().await,
            DataAction::Scan { days, force } => cmd_scan(days, force).await,
            DataAction::Daily {
                days,
                skip_train,
                quick,
                backend,
                walk_forward_folds,
                top_n,
                slippage_bps,
            } => {
                cmd_daily(
                    days,
                    skip_train,
                    quick,
                    backend,
                    walk_forward_folds,
                    top_n,
                    slippage_bps,
                    json_flag,
                )
                .await
            }
            DataAction::Screen { min_volume } => cmd_screen(min_volume, json_flag).await,
            DataAction::Movers => cmd_movers(json_flag).await,
            DataAction::Watchlist => cmd_watchlist(json_flag).await,
            DataAction::Suggest => cmd_suggest(json_flag).await,
            DataAction::Status => cmd_status(json_flag).await,
            DataAction::DbStats => cmd_db_stats(json_flag),
            DataAction::DbOptimize { vacuum } => cmd_db_optimize(vacuum, json_flag),
        },
        Commands::Compliance { action } => match action {
            ComplianceAction::Wash => cmd_wash(json_flag).await,
            ComplianceAction::Pdt => cmd_pdt(json_flag).await,
            ComplianceAction::Tax {
                accounts_list,
                accounts,
                details,
                show,
                show_brackets,
                year,
                quarter,
                export,
            } => {
                if accounts_list {
                    tax::cmd_tax_accounts(json_flag)
                } else if show_brackets {
                    match year {
                        Some(year) => tax::cmd_tax_show_brackets(year, json_flag),
                        None => Err(anyhow::anyhow!("--year is required with --show-brackets.")),
                    }
                } else {
                    match year {
                        Some(year) => tax::cmd_tax_show(
                            show, year, quarter, export, accounts, details, json_flag,
                        ),
                        None => Err(anyhow::anyhow!("--year is required for tax estimates.")),
                    }
                }
            }
        },
        Commands::Version => cmd_version(json_flag),
        Commands::Completions { action } => cmd_completions(action, json_flag),
        Commands::Daemon { action } => match action {
            DaemonAction::Reload => daemon::cmd_reload(json_flag),
            DaemonAction::Restart => daemon::cmd_restart(json_flag),
            DaemonAction::Start => daemon::cmd_start(json_flag),
            DaemonAction::Status { details } => daemon::cmd_status(json_flag, details),
            DaemonAction::Stop => daemon::cmd_stop(json_flag),
        },
        Commands::Api { action } => match action {
            ApiAction::Reload => api::cmd_reload(json_flag),
            ApiAction::Restart => api::cmd_restart(json_flag),
            ApiAction::Start => api::cmd_start(json_flag),
            ApiAction::Status { details } => api::cmd_status(json_flag, details),
            ApiAction::Test => api::cmd_test(json_flag).await,
            ApiAction::Stop => api::cmd_stop(json_flag),
        },
        Commands::Start => daemon::cmd_start(json_flag),
        Commands::Stop => daemon::cmd_stop(json_flag),
        Commands::Restart => daemon::cmd_restart(json_flag),
        Commands::Reload => daemon::cmd_reload(json_flag),
        Commands::DaemonRun => daemon::cmd_run().await,
        Commands::ApiRun => api::cmd_run().await,
        // Trading
        Commands::Account { accounts } => cmd_account(accounts, json_flag).await,
        Commands::Buy {
            symbol,
            qty,
            accounts,
            r#type,
            limit_price,
            stop_price,
            tif,
        } => cmd_buy(symbol, qty, accounts, r#type, limit_price, stop_price, tif).await,
        Commands::Sell {
            symbol,
            qty,
            accounts,
            r#type,
            limit_price,
            stop_price,
            tif,
        } => cmd_sell(symbol, qty, accounts, r#type, limit_price, stop_price, tif).await,
        Commands::Positions { accounts } => cmd_positions(accounts, json_flag).await,
        Commands::Orders {
            accounts,
            status,
            limit,
            sync,
        } => cmd_orders(accounts, status, limit, sync, json_flag).await,
        Commands::Cancel { order_id, accounts } => cmd_cancel(order_id, accounts).await,
        Commands::Close { symbol, accounts } => cmd_close(symbol, accounts).await,
        // Data
        Commands::DataFeed => cmd_data_feed(json_flag),
        Commands::Quote { symbol } => cmd_quote(symbol, json_flag).await,
        Commands::Watch { symbols } => cmd_watch(symbols).await,
        Commands::Bars {
            symbol,
            timeframe,
            limit,
        } => cmd_bars_single(symbol, timeframe, limit).await,
        Commands::News { symbol, limit } => cmd_news(symbol, limit, json_flag).await,
        Commands::Sp500 { days } => cmd_sp500(days, json_flag).await,
        Commands::HistoryStart { symbols } => cmd_history_start(symbols, json_flag).await,
        Commands::Clock => cmd_clock(json_flag).await,
        Commands::Calendar {
            start,
            end,
            markets,
        } => cmd_calendar(start, end, markets, json_flag).await,
        // Scanner
        Commands::Universe => cmd_universe().await,
        Commands::Scan { days, force } => cmd_scan(days, force).await,
        Commands::Daily {
            days,
            skip_train,
            quick,
            backend,
            walk_forward_folds,
            top_n,
            slippage_bps,
        } => {
            cmd_daily(
                days,
                skip_train,
                quick,
                backend,
                walk_forward_folds,
                top_n,
                slippage_bps,
                json_flag,
            )
            .await
        }
        Commands::Screen { min_volume } => cmd_screen(min_volume, json_flag).await,
        Commands::Movers => cmd_movers(json_flag).await,
        Commands::Watchlist => cmd_watchlist(json_flag).await,
        Commands::Suggest => cmd_suggest(json_flag).await,
        Commands::Status => cmd_status(json_flag).await,
        // Compliance
        Commands::Wash => cmd_wash(json_flag).await,
        Commands::Pdt => cmd_pdt(json_flag).await,
        Commands::Tax {
            accounts_list,
            accounts,
            details,
            show,
            show_brackets,
            year,
            quarter,
            export,
        } => {
            if accounts_list {
                tax::cmd_tax_accounts(json_flag)
            } else if show_brackets {
                match year {
                    Some(year) => tax::cmd_tax_show_brackets(year, json_flag),
                    None => Err(anyhow::anyhow!("--year is required with --show-brackets.")),
                }
            } else {
                match year {
                    Some(year) => {
                        tax::cmd_tax_show(show, year, quarter, export, accounts, details, json_flag)
                    }
                    None => Err(anyhow::anyhow!("--year is required for tax estimates.")),
                }
            }
        }
        // Feeds
        Commands::Feeds { action } => cmd_feeds(action, json_flag).await,
        // ML
        Commands::Ml { action } => {
            let r = match action {
                MlAction::Refresh {
                    days,
                    quick,
                    backend,
                    walk_forward_folds,
                    top_n,
                    slippage_bps,
                } => cmd_ml_refresh(
                    days,
                    quick,
                    backend,
                    walk_forward_folds,
                    top_n,
                    slippage_bps,
                    json_flag,
                )
                .await
                .map_err(|e| anyhow::anyhow!("{}", e)),
                MlAction::Features { symbol, force } => {
                    ml::cmd_ml_features(symbol, force, json_flag)
                }
                MlAction::Labels { horizon } => ml::cmd_ml_labels(horizon, json_flag),
                MlAction::Export { format } => ml::cmd_ml_export(format, json_flag),
                MlAction::Train {
                    quick,
                    backtest_only,
                } => ml::cmd_ml_train(quick, backtest_only, json_flag),
                MlAction::AblateSp500 { quick } => ml::cmd_ml_ablate_sp500(quick, json_flag),
                MlAction::XgboostAblateSp500 { quick } => {
                    ml::cmd_ml_xgboost_ablate_sp500(quick, json_flag)
                }
                MlAction::Baselines { quick } => ml::cmd_ml_baselines(quick, json_flag),
                MlAction::WalkForward { quick, folds } => {
                    ml::cmd_ml_walk_forward(quick, folds, json_flag)
                }
                MlAction::Predict => ml::cmd_ml_predict(json_flag),
                MlAction::XgboostPredict => ml::cmd_ml_xgboost_predict(json_flag),
                MlAction::LstmTrain {
                    backend,
                    single_thread,
                    threads,
                    without_sp500,
                } => {
                    let backend = configured_lstm_backend(backend);
                    lstm::cmd_ml_lstm_train(
                        json_flag,
                        single_thread,
                        threads,
                        without_sp500,
                        backend,
                    )
                }
                MlAction::LstmPredict { without_sp500 } => {
                    lstm::cmd_ml_lstm_predict(json_flag, without_sp500)
                }
                MlAction::LstmEvaluate {
                    without_sp500,
                    top_n,
                    slippage_bps,
                } => lstm::cmd_ml_lstm_evaluate(json_flag, without_sp500, top_n, slippage_bps)
                    .map(|_| ()),
                MlAction::Explain { symbol } => ml::cmd_ml_explain(symbol, json_flag),
                MlAction::Explainable { limit } => ml::cmd_ml_explainable(limit, json_flag),
                MlAction::Explained { limit } => ml::cmd_ml_explained(limit, json_flag),
                MlAction::CacheShap { top } => {
                    ml::cmd_ml_cache_default_shap(top, json_flag).map(|_| ())
                }
                MlAction::Ensemble {
                    lgb_weight,
                    lstm_weight,
                    xgb_weight,
                } => ml::cmd_ml_ensemble_weighted(lgb_weight, lstm_weight, xgb_weight, json_flag),
                MlAction::EnsembleSearch {
                    top_n,
                    slippage_bps,
                } => ml::cmd_ml_select_default_ensemble(json_flag, top_n, slippage_bps).map(|_| ()),
                MlAction::EnsembleDefault => ml::cmd_ml_ensemble_default(json_flag),
                MlAction::EnsembleRobustSweep => {
                    ml::cmd_ml_ensemble_robust_sweep(json_flag).map(|_| ())
                }
                MlAction::CompareSp500Final {
                    lgb_weight,
                    lstm_weight,
                } => ml::cmd_ml_compare_sp500_final(lgb_weight, lstm_weight, json_flag),
                MlAction::Status => ml::cmd_ml_status(json_flag),
                MlAction::FullRefresh {
                    days,
                    quick,
                    backend,
                    walk_forward_folds,
                    top_n,
                    slippage_bps,
                } => cmd_ml_full_refresh(
                    days,
                    quick,
                    backend,
                    walk_forward_folds,
                    top_n,
                    slippage_bps,
                    json_flag,
                )
                .await
                .map_err(|e| anyhow::anyhow!("{}", e)),
            };
            r.map_err(|e| anyhow::anyhow!("{}", e))
        }
        // Auto-Trading
        Commands::Auto { action } => match action {
            AutoAction::Run => auto::cmd_auto_run(json_flag).await,
            AutoAction::SyncOrders => auto::cmd_sync_orders(json_flag).await,
            AutoAction::Status => auto::cmd_auto_status(json_flag).await,
            AutoAction::History { limit } => auto::cmd_auto_history(limit, json_flag).await,
            AutoAction::Config { key, value } => {
                auto::cmd_auto_config(key, value, json_flag).map_err(|e| anyhow::anyhow!("{}", e))
            }
            AutoAction::Enable => {
                auto::cmd_auto_enable(json_flag).map_err(|e| anyhow::anyhow!("{}", e))
            }
            AutoAction::Disable => {
                auto::cmd_auto_disable(json_flag).map_err(|e| anyhow::anyhow!("{}", e))
            }
        },
    };
    if let Err(e) = result {
        log_command_event(
            &log_components,
            "command_failed",
            &command_path,
            command_started,
            Some(&e.to_string()),
        );
        exit_with_error_and_help(&e.to_string(), &help_path, json_flag);
    };
    log_command_event(
        &log_components,
        "command_completed",
        &command_path,
        command_started,
        None,
    );
}
