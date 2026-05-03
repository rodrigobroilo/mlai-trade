// Daemon lifecycle and scheduler.
//
// Function map:
// - cmd_start/stop/reload/restart/status/run(): daemon control entrypoints.
// - daily_refresh_due(): decides when the non-trading ML prep should run.
// - run_daily_maintenance(): syncs providers, feeds, ML artifacts, and tax.
// - rotate_runtime_logs(): keeps all component logs JSONL and daily-compressed.

use crate::{auto, config, logging, paths, tax};
use chrono::{Datelike, Duration as ChronoDuration, NaiveDate, NaiveTime, Utc};
use chrono_tz::Tz;
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static TERMINATE: AtomicBool = AtomicBool::new(false);
static RELOAD: AtomicBool = AtomicBool::new(false);

// Handles the signal request or signal.
extern "C" fn handle_signal(signal: libc::c_int) {
    match signal {
        libc::SIGTERM | libc::SIGINT => TERMINATE.store(true, Ordering::SeqCst),
        libc::SIGHUP => RELOAD.store(true, Ordering::SeqCst),
        _ => {}
    }
}

#[derive(Debug, Clone)]
pub struct DaemonStatus {
    pub enabled: bool,
    pub running: bool,
    pub pid: Option<u32>,
    pub pid_file: PathBuf,
    pub log_file: PathBuf,
    pub interval_seconds: u64,
    pub daily_refresh: config::DaemonDailyRefreshConfig,
    pub daily_refresh_stamp_file: PathBuf,
    pub daily_refresh_last_date: Option<String>,
}

// Returns configured path with defaults applied.
fn configured_path(value: Option<String>, base: PathBuf, default_name: &str) -> PathBuf {
    paths::path_in_runtime_dir(base, value, default_name)
}

// Handles daemon config paths state.
fn daemon_config_paths() -> (PathBuf, PathBuf) {
    let config = config::load().unwrap_or_default();
    (
        configured_path(
            config.daemon.pid_file,
            paths::tmp_dir(),
            "mlai-trade-daemon.pid",
        ),
        configured_path(
            config.daemon.log_file,
            paths::logs_dir(),
            "mlai-trade-daemon.log",
        ),
    )
}

// Handles pid file logic.
fn pid_file() -> PathBuf {
    daemon_config_paths().0
}

// Handles log file logic.
fn log_file() -> PathBuf {
    daemon_config_paths().1
}

// Handles daily refresh stamp file logic.
fn daily_refresh_stamp_file() -> PathBuf {
    paths::tmp_dir().join("mlai-trade-daily-refresh.stamp")
}

// Handles daemon log state.
fn daemon_log(mut event: serde_json::Value) {
    if let Some(object) = event.as_object_mut() {
        object.entry("ts".to_string()).or_insert_with(|| {
            serde_json::json!(Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string())
        });
        object
            .entry("component".to_string())
            .or_insert_with(|| serde_json::json!("daemon"));
    }
    let line = serde_json::to_string(&event).unwrap_or_else(|err| {
        serde_json::json!({
            "ts": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "component": "daemon",
            "event": "log_serialization_failed",
            "level": "error",
            "error": err.to_string(),
        })
        .to_string()
    });
    let _ = writeln!(std::io::stdout(), "{line}");
}

// Returns configured api log file with defaults applied.
fn configured_api_log_file() -> PathBuf {
    config::load()
        .ok()
        .map(|config| configured_path(config.api.log_file, paths::logs_dir(), "mlai-trade-api.log"))
        .unwrap_or_else(|| paths::logs_dir().join("mlai-trade-api.log"))
}

// Handles rotate runtime logs logic.
fn rotate_runtime_logs() {
    let mut paths = BTreeSet::new();
    paths.insert((log_file(), "daemon"));
    paths.insert((config::auto_log_file(), "auto_trade"));
    paths.insert((configured_api_log_file(), "api"));
    paths.insert((logging::component_log_path("data"), "data"));
    paths.insert((logging::component_log_path("feeds"), "feeds"));
    paths.insert((logging::component_log_path("ml"), "ml"));
    paths.insert((logging::component_log_path("training"), "training"));
    for (path, component) in paths {
        if let Err(err) = logging::ensure_json_lines(&path, component) {
            daemon_log(serde_json::json!({
                "event": "log_json_sanitize_failed",
                "level": "error",
                "log_file": path.display().to_string(),
                "error": err.to_string(),
            }));
        }
        match logging::rotate_if_needed(&path) {
            Ok(Some(archive)) => daemon_log(serde_json::json!({
                "event": "log_rotated",
                "level": "info",
                "log_file": path.display().to_string(),
                "archive_file": archive.display().to_string(),
            })),
            Ok(None) => {}
            Err(err) => daemon_log(serde_json::json!({
                "event": "log_rotation_failed",
                "level": "error",
                "log_file": path.display().to_string(),
                "error": err.to_string(),
            })),
        }
    }
}

// Handles output tail logic.
fn output_tail(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut lines = trimmed.lines().rev().take(80).collect::<Vec<_>>();
    lines.reverse();
    let joined = lines.join("\n");
    let char_count = joined.chars().count();
    if char_count <= 16_000 {
        Some(config::redact_configured_secrets(&joined))
    } else {
        let tail = joined
            .chars()
            .rev()
            .take(16_000)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<String>();
        Some(config::redact_configured_secrets(&format!("...{tail}")))
    }
}

// Handles daemon market today state.
fn daemon_market_today() -> (String, NaiveDate) {
    let timezone_name = config::load()
        .ok()
        .and_then(|config| config.auto.market.timezone)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "America/New_York".to_string());
    let timezone = timezone_name
        .parse::<Tz>()
        .unwrap_or(chrono_tz::America::New_York);
    (
        timezone_name,
        Utc::now().with_timezone(&timezone).date_naive(),
    )
}

// Reads daily refresh stamp from disk or local state.
fn read_daily_refresh_stamp() -> Option<String> {
    fs::read_to_string(daily_refresh_stamp_file())
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

// Writes daily refresh stamp to disk or storage.
fn write_daily_refresh_stamp(date: NaiveDate) -> anyhow::Result<()> {
    let path = daily_refresh_stamp_file();
    if let Some(parent) = path.parent() {
        paths::ensure_private_dir(parent)?;
    }
    paths::write_private_file(&path, date.to_string())?;
    Ok(())
}

// Reads pid from disk or local state.
fn read_pid(path: &PathBuf) -> Option<u32> {
    fs::read_to_string(path)
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
}

// Handles process alive logic.
fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    unsafe {
        if libc::kill(pid as libc::pid_t, 0) == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

// Handles status logic.
pub fn status() -> DaemonStatus {
    let pid_file = pid_file();
    let log_file = log_file();
    let pid = read_pid(&pid_file);
    let running = pid.map(process_alive).unwrap_or(false);
    DaemonStatus {
        enabled: config::daemon_enabled(),
        running,
        pid: if running { pid } else { None },
        pid_file,
        log_file,
        interval_seconds: config::daemon_auto_trade_interval_seconds(),
        daily_refresh: config::daemon_daily_refresh_config(),
        daily_refresh_stamp_file: daily_refresh_stamp_file(),
        daily_refresh_last_date: read_daily_refresh_stamp(),
    }
}

// Removes stale pid from local state.
fn remove_stale_pid(path: &PathBuf) {
    if let Some(pid) = read_pid(path) {
        if !process_alive(pid) {
            let _ = fs::remove_file(path);
        }
    }
}

// Handles the start CLI action.
pub fn cmd_start(json: bool) -> anyhow::Result<()> {
    paths::ensure_runtime_dirs()?;
    if !config::daemon_enabled() {
        anyhow::bail!(
            "cannot run as daemon: daemon.enabled=false in {}. Set daemon.enabled=true before starting.",
            config::config_path().display()
        );
    }
    let status = status();
    if status.running {
        if json {
            println!(
                "{}",
                serde_json::json!({"status": "already_running", "pid": status.pid})
            );
        } else {
            println!(
                "Daemon already running with pid {}.",
                status.pid.unwrap_or(0)
            );
        }
        return Ok(());
    }
    remove_stale_pid(&status.pid_file);

    if let Some(parent) = status.log_file.parent() {
        paths::ensure_private_dir(parent)?;
    }
    if let Err(err) = logging::ensure_json_lines(&status.log_file, "daemon") {
        eprintln!(
            "{}",
            serde_json::json!({
                "ts": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                "component": "daemon",
                "event": "log_json_sanitize_failed",
                "level": "error",
                "log_file": status.log_file.display().to_string(),
                "error": err.to_string(),
            })
        );
    }
    if let Err(err) = logging::rotate_if_needed(&status.log_file) {
        eprintln!(
            "{}",
            serde_json::json!({
                "ts": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                "component": "daemon",
                "event": "log_rotation_failed",
                "level": "error",
                "log_file": status.log_file.display().to_string(),
                "error": err.to_string(),
            })
        );
    }
    let stdout = paths::open_private_append(&status.log_file)?;
    let stderr = stdout.try_clone()?;

    let exe = std::env::current_exe()?;
    let mut command = Command::new(exe);
    command
        .arg("--home")
        .arg(paths::root_dir())
        .arg("daemon-run")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    if json {
        println!(
            "{}",
            serde_json::json!({"status": "started", "pid": child.id(), "log_file": status.log_file})
        );
    } else {
        println!("Daemon started with pid {}.", child.id());
        println!("Log file: {}", status.log_file.display());
    }
    Ok(())
}

// Handles the stop CLI action.
pub fn cmd_stop(json: bool) -> anyhow::Result<()> {
    let path = pid_file();
    let Some(pid) = read_pid(&path) else {
        if json {
            println!("{}", serde_json::json!({"status": "not_running"}));
        } else {
            println!("Daemon is not running.");
        }
        return Ok(());
    };
    if !process_alive(pid) {
        let _ = fs::remove_file(&path);
        if json {
            println!(
                "{}",
                serde_json::json!({"status": "stale_pid_removed", "pid": pid})
            );
        } else {
            println!("Removed stale daemon pid file for pid {}.", pid);
        }
        return Ok(());
    }
    unsafe {
        if libc::kill(pid as libc::pid_t, libc::SIGTERM) != 0 {
            anyhow::bail!(
                "unable to stop daemon pid {}: {}",
                pid,
                std::io::Error::last_os_error()
            );
        }
    }
    for _ in 0..50 {
        if !process_alive(pid) {
            let _ = fs::remove_file(&path);
            if json {
                println!("{}", serde_json::json!({"status": "stopped", "pid": pid}));
            } else {
                println!("Daemon stopped.");
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!("daemon pid {} did not stop within timeout", pid)
}

// Handles the reload CLI action.
pub fn cmd_reload(json: bool) -> anyhow::Result<()> {
    let status = status();
    let Some(pid) = status.pid else {
        anyhow::bail!("daemon is not running");
    };
    unsafe {
        if libc::kill(pid as libc::pid_t, libc::SIGHUP) != 0 {
            anyhow::bail!(
                "unable to reload daemon pid {}: {}",
                pid,
                std::io::Error::last_os_error()
            );
        }
    }
    if json {
        println!("{}", serde_json::json!({"status": "reloaded", "pid": pid}));
    } else {
        println!("Daemon reload signal sent to pid {}.", pid);
    }
    Ok(())
}

// Handles the restart CLI action.
pub fn cmd_restart(json: bool) -> anyhow::Result<()> {
    let _ = cmd_stop(false);
    cmd_start(json)
}

// Handles the status CLI action.
pub fn cmd_status(json: bool) -> anyhow::Result<()> {
    let status = status();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "enabled": status.enabled,
                "running": status.running,
                "pid": status.pid,
                "pid_file": status.pid_file,
                "log_file": status.log_file,
                "interval_seconds": status.interval_seconds,
                "daily_refresh": {
                    "enabled": status.daily_refresh.enabled,
                    "trigger": status.daily_refresh.trigger,
                    "after_close_minutes": status.daily_refresh.after_close_minutes,
                    "time": status.daily_refresh.time,
                    "timezone": status.daily_refresh.timezone,
                    "days": status.daily_refresh.days,
                    "quick": status.daily_refresh.quick,
                    "walk_forward_folds": status.daily_refresh.walk_forward_folds,
                    "top_n": status.daily_refresh.top_n,
                    "slippage_bps": status.daily_refresh.slippage_bps,
                    "sync_orders": status.daily_refresh.sync_orders,
                    "feeds_sync": status.daily_refresh.feeds_sync,
                    "feeds_days": status.daily_refresh.feeds_days,
                    "stamp_file": status.daily_refresh_stamp_file,
                    "last_success_date": status.daily_refresh_last_date,
                },
            }))?
        );
        return Ok(());
    }

    println!("Daemon Status");
    println!("  Enabled:     {}", status.enabled);
    println!("  Running:     {}", status.running);
    if let Some(pid) = status.pid {
        println!("  PID:         {}", pid);
    }
    println!("  PID file:    {}", status.pid_file.display());
    println!("  Log file:    {}", status.log_file.display());
    println!("  Interval:    {}s", status.interval_seconds);
    println!(
        "  Daily:       enabled={} trigger={} after_close={}m fallback_time={} {} last={}",
        status.daily_refresh.enabled,
        status.daily_refresh.trigger,
        status.daily_refresh.after_close_minutes,
        status.daily_refresh.time,
        status.daily_refresh.timezone,
        status.daily_refresh_last_date.as_deref().unwrap_or("never")
    );
    Ok(())
}

// Parses daily time from user or provider input.
fn parse_daily_time(value: &str) -> NaiveTime {
    NaiveTime::parse_from_str(value.trim(), "%H:%M:%S")
        .or_else(|_| NaiveTime::parse_from_str(value.trim(), "%H:%M"))
        .unwrap_or_else(|_| NaiveTime::from_hms_opt(18, 30, 0).unwrap())
}

// Returns configured market close with defaults applied.
fn configured_market_close() -> (NaiveTime, BTreeSet<String>) {
    let market = config::load()
        .ok()
        .map(|config| config.auto.market)
        .unwrap_or_default();
    let close = market
        .regular_close
        .as_deref()
        .map(parse_daily_time)
        .unwrap_or_else(|| NaiveTime::from_hms_opt(16, 0, 0).unwrap());
    (close, market.closed_dates.into_iter().collect())
}

// Handles daily refresh due logic.
fn daily_refresh_due(config: &config::DaemonDailyRefreshConfig) -> Option<NaiveDate> {
    if !config.enabled {
        return None;
    }
    let timezone = config
        .timezone
        .parse::<Tz>()
        .unwrap_or(chrono_tz::America::New_York);
    let now = Utc::now().with_timezone(&timezone);
    let today = now.date_naive();
    if read_daily_refresh_stamp().as_deref() == Some(&today.to_string()) {
        return None;
    }
    if config.trigger == "time" {
        if now.time() < parse_daily_time(&config.time) {
            return None;
        }
        return Some(today);
    }

    let (market_close, closed_dates) = configured_market_close();
    let today_string = today.to_string();
    if closed_dates.contains(&today_string) {
        return None;
    }
    let weekday = today.weekday();
    if weekday == chrono::Weekday::Sat || weekday == chrono::Weekday::Sun {
        return None;
    }
    let due_at = today.and_time(market_close) + ChronoDuration::minutes(config.after_close_minutes);
    if now.naive_local() < due_at {
        return None;
    }
    Some(today)
}

// Handles CLI command run daemon command routing.
fn run_daemon_command(label: &str, args: &[String]) -> anyhow::Result<()> {
    daemon_log(serde_json::json!({
        "event": "daily_maintenance_step_started",
        "level": "info",
        "label": label,
        "command": args,
    }));
    let started = Instant::now();
    let output = Command::new(std::env::current_exe()?)
        .arg("--home")
        .arg(paths::root_dir())
        .args(args)
        .env("MLAI_TRADE_PROGRESS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    let mut event = serde_json::json!({
        "event": if output.status.success() {
            "daily_maintenance_step_completed"
        } else {
            "daily_maintenance_step_failed"
        },
        "level": if output.status.success() { "info" } else { "error" },
        "label": label,
        "command": args,
        "exit_code": output.status.code(),
        "duration_ms": started.elapsed().as_millis(),
    });
    if let Some(stdout) = output_tail(&output.stdout) {
        event["stdout_tail"] = serde_json::json!(stdout);
    }
    if let Some(stderr) = output_tail(&output.stderr) {
        event["stderr_tail"] = serde_json::json!(stderr);
    }
    daemon_log(event);
    if !output.status.success() {
        anyhow::bail!(
            "daemon daily maintenance step failed: {label} ({})",
            output.status
        );
    }
    Ok(())
}

// Handles run daily maintenance logic.
fn run_daily_maintenance(
    config: &config::DaemonDailyRefreshConfig,
    date: NaiveDate,
) -> anyhow::Result<()> {
    daemon_log(serde_json::json!({
        "event": "daily_maintenance_started",
        "level": "info",
        "schedule_date": date.to_string(),
        "trigger": config.trigger.as_str(),
        "after_close_minutes": config.after_close_minutes,
        "scheduled_time": config.time.as_str(),
        "timezone": config.timezone.as_str(),
    }));
    if config.sync_orders {
        run_daemon_command(
            "sync provider orders",
            &["auto".into(), "sync-orders".into()],
        )?;
    }

    let mut refresh_args = vec![
        "ml".to_string(),
        "refresh".to_string(),
        "--days".to_string(),
        config.days.to_string(),
        "--backend".to_string(),
        "auto".to_string(),
        "--walk-forward-folds".to_string(),
        config.walk_forward_folds.to_string(),
        "--top-n".to_string(),
        config.top_n.to_string(),
        "--slippage-bps".to_string(),
        config.slippage_bps.to_string(),
    ];
    if config.quick {
        refresh_args.push("--quick".to_string());
    }
    run_daemon_command("ML/data refresh", &refresh_args)?;

    if config.feeds_sync {
        run_daemon_command(
            "feeds sync",
            &[
                "feeds".into(),
                "sync".into(),
                "--days".into(),
                config.feeds_days.to_string(),
            ],
        )?;
    }

    tax::refresh_current_year_estimates()?;
    write_daily_refresh_stamp(date)?;
    daemon_log(serde_json::json!({
        "event": "daily_maintenance_completed",
        "level": "info",
        "schedule_date": date.to_string(),
    }));
    Ok(())
}

// Handles the run CLI action.
pub async fn cmd_run() -> anyhow::Result<()> {
    paths::ensure_runtime_dirs()?;
    unsafe {
        libc::signal(
            libc::SIGTERM,
            handle_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            handle_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGHUP,
            handle_signal as *const () as libc::sighandler_t,
        );
    }
    let pid_path = pid_file();
    if let Some(parent) = pid_path.parent() {
        paths::ensure_private_dir(parent)?;
    }
    paths::write_runtime_metadata_file(&pid_path, std::process::id().to_string())?;
    rotate_runtime_logs();
    daemon_log(serde_json::json!({
        "event": "daemon_started",
        "level": "info",
        "pid": std::process::id(),
        "interval_seconds": config::daemon_auto_trade_interval_seconds(),
    }));

    let mut last_daily_attempt: Option<Instant> = None;
    let mut auto_market_closed_backoff_date: Option<NaiveDate> = None;
    while !TERMINATE.load(Ordering::SeqCst) {
        rotate_runtime_logs();
        if let Err(err) = config::load() {
            daemon_log(serde_json::json!({
                "event": "config_invalid",
                "level": "error",
                "config_file": config::config_path().display().to_string(),
                "error": err.to_string(),
                "message": "daemon paused until configuration is fixed",
            }));
            tokio::time::sleep(Duration::from_secs(
                config::daemon_auto_trade_interval_seconds(),
            ))
            .await;
            continue;
        }
        if !config::daemon_enabled() {
            daemon_log(serde_json::json!({
                "event": "daemon_stopping",
                "level": "warn",
                "reason": "daemon.enabled=false",
            }));
            break;
        }
        if RELOAD.swap(false, Ordering::SeqCst) {
            daemon_log(serde_json::json!({
                "event": "daemon_config_reloaded",
                "level": "info",
                "interval_seconds": config::daemon_auto_trade_interval_seconds(),
            }));
        }
        let (market_timezone, market_date) = daemon_market_today();
        if auto_market_closed_backoff_date == Some(market_date) {
            // The backoff-start event is emitted when the closed market is first observed.
        } else {
            match auto::run_auto_cycle("daemon", false).await {
                Ok(result) => {
                    let mut event = result.clone();
                    if let Some(object) = event.as_object_mut() {
                        object
                            .entry("event".to_string())
                            .or_insert_with(|| serde_json::json!("auto_trade_cycle"));
                    }
                    daemon_log(event);
                    if result["status"].as_str() == Some("market_closed") {
                        auto_market_closed_backoff_date = Some(market_date);
                        daemon_log(serde_json::json!({
                            "event": "auto_market_closed_backoff_started",
                            "level": "info",
                            "status": "market_closed",
                            "market_date": market_date.to_string(),
                            "market_timezone": market_timezone,
                            "next_check_date": (market_date + ChronoDuration::days(1)).to_string(),
                            "message": "market closed for all enabled accounts; backing off daemon auto-trade cycles until tomorrow",
                        }));
                    } else {
                        auto_market_closed_backoff_date = None;
                    }
                }
                Err(err) => {
                    auto_market_closed_backoff_date = None;
                    daemon_log(serde_json::json!({
                        "event": "auto_run_failed",
                        "level": "error",
                        "error": err.to_string(),
                    }));
                }
            }
        }
        if let Err(err) = tax::refresh_current_year_estimates() {
            daemon_log(serde_json::json!({
                "event": "tax_refresh_failed",
                "level": "error",
                "error": err.to_string(),
            }));
        }
        let daily_config = config::daemon_daily_refresh_config();
        if let Some(date) = daily_refresh_due(&daily_config) {
            let retry_ok = last_daily_attempt
                .map(|last| last.elapsed() >= Duration::from_secs(3600))
                .unwrap_or(true);
            if retry_ok {
                last_daily_attempt = Some(Instant::now());
                if let Err(err) = run_daily_maintenance(&daily_config, date) {
                    daemon_log(serde_json::json!({
                        "event": "daily_maintenance_failed",
                        "level": "error",
                        "schedule_date": date.to_string(),
                        "error": err.to_string(),
                    }));
                }
            }
        }
        let interval = config::daemon_auto_trade_interval_seconds();
        for _ in 0..interval {
            if TERMINATE.load(Ordering::SeqCst) || RELOAD.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    let current_pid = std::process::id();
    if read_pid(&pid_path) == Some(current_pid) {
        let _ = fs::remove_file(&pid_path);
    }
    daemon_log(serde_json::json!({
        "event": "daemon_stopped",
        "level": "info",
        "pid": current_pid,
    }));
    Ok(())
}
