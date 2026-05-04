// Daemon lifecycle and scheduler.
//
// Function map:
// - cmd_start/stop/reload/restart/status/run(): daemon control entrypoints.
// - daily_refresh_due(): decides when the non-trading ML prep should run.
// - run_daily_maintenance(): syncs providers, feeds, ML artifacts, and tax.
// - rotate_runtime_logs(): keeps all component logs JSONL and daily-compressed.

use crate::{accelerators, auto, config, logging, paths, process, tax, update_lock};
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
    pub runtime_status_file: PathBuf,
    pub runtime_status: Option<serde_json::Value>,
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

// Returns the daemon heartbeat status file path.
fn runtime_status_file() -> PathBuf {
    paths::tmp_dir().join("mlai-trade-daemon-status.json")
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

// Reads daemon heartbeat status from disk.
fn read_runtime_status() -> Option<serde_json::Value> {
    fs::read_to_string(runtime_status_file())
        .ok()
        .and_then(|value| serde_json::from_str(&value).ok())
}

// Writes daemon heartbeat status for external status inspection.
fn write_runtime_status(
    started_at: Instant,
    started_at_utc: &str,
    loop_count: u64,
    market_timezone: &str,
    market_date: NaiveDate,
    last_auto_status: &serde_json::Value,
    last_daily_status: &serde_json::Value,
) {
    let payload = serde_json::json!({
        "pid": std::process::id(),
        "started_at_utc": started_at_utc,
        "heartbeat_at_utc": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "uptime_seconds": started_at.elapsed().as_secs_f64(),
        "loop_count": loop_count,
        "market_timezone": market_timezone,
        "market_date": market_date.to_string(),
        "last_auto": last_auto_status,
        "last_daily": last_daily_status,
        "resources": process::current_process_usage_json(Some(started_at)),
    });
    let path = runtime_status_file();
    if let Err(err) = paths::write_runtime_metadata_file(
        &path,
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string()),
    ) {
        daemon_log(serde_json::json!({
            "event": "daemon_runtime_status_write_failed",
            "level": "error",
            "error": err.to_string(),
            "status_file": path.display().to_string(),
        }));
    }
}

// Reads pid from disk or local state.
fn read_pid(path: &PathBuf) -> Option<u32> {
    fs::read_to_string(path)
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
}

// Handles process alive logic.
fn process_alive(pid: u32) -> bool {
    process::pid_alive(pid)
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
        runtime_status_file: runtime_status_file(),
        runtime_status: if running { read_runtime_status() } else { None },
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
    cmd_stop(false)?;
    cmd_start(json)
}

// Handles the status CLI action.
pub fn cmd_status(json: bool, details: bool) -> anyhow::Result<()> {
    let status = status();
    if json {
        let mut payload = serde_json::json!({
            "enabled": status.enabled,
            "running": status.running,
            "pid": status.pid,
            "pid_file": status.pid_file,
            "log_file": status.log_file,
            "runtime_status_file": status.runtime_status_file,
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
                "last_success_date": status.daily_refresh_last_date.clone().unwrap_or_else(|| "not available".to_string()),
            },
        });
        if details {
            payload["details"] = status
                .runtime_status
                .clone()
                .unwrap_or_else(|| serde_json::json!("not available"));
            payload["configured_resources"] = config::runtime_resources_json();
            payload["accelerators"] = accelerators::accelerator_status_json();
        }
        println!("{}", serde_json::to_string_pretty(&payload)?);
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
    if details {
        println!("  Status file: {}", status.runtime_status_file.display());
    }
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
    if details {
        print_daemon_details(status.runtime_status.as_ref());
    }
    Ok(())
}

// Formats optional JSON metrics for daemon status output.
fn daemon_metric_text(value: Option<&serde_json::Value>) -> String {
    match value {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Number(number)) => number.to_string(),
        Some(serde_json::Value::Bool(value)) => value.to_string(),
        Some(other) => other.to_string(),
        None => "not available".to_string(),
    }
}

// Formats daemon byte metrics as MiB when available.
fn daemon_bytes_mib_text(value: Option<&serde_json::Value>) -> String {
    value
        .and_then(serde_json::Value::as_u64)
        .map(|bytes| format!("{:.2} MiB", bytes as f64 / 1_048_576.0))
        .unwrap_or_else(|| "not available".to_string())
}

// Formats raw bytes as GiB for daemon resource budgets.
fn daemon_bytes_gib_text(bytes: u64) -> String {
    format!("{:.2} GiB", bytes as f64 / 1_073_741_824.0)
}

// Formats daemon seconds in a compact human-readable form.
fn daemon_seconds_text(value: Option<&serde_json::Value>) -> String {
    let Some(seconds) = value.and_then(serde_json::Value::as_f64) else {
        return "not available".to_string();
    };
    if seconds < 60.0 {
        return format!("{seconds:.1}s");
    }
    let total = seconds.round() as u64;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let secs = total % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m {secs}s")
    } else {
        format!("{minutes}m {secs}s")
    }
}

// Formats daemon floating-point metrics with stable decimals.
fn daemon_number_text(value: Option<&serde_json::Value>, decimals: usize) -> String {
    value
        .and_then(serde_json::Value::as_f64)
        .map(|number| format!("{number:.decimals$}"))
        .unwrap_or_else(|| daemon_metric_text(value))
}

// Prints daemon heartbeat and resource details.
fn print_daemon_details(runtime: Option<&serde_json::Value>) {
    let Some(runtime) = runtime else {
        println!("  Runtime:     not available");
        return;
    };
    println!("  Runtime:");
    println!(
        "    Heartbeat: {}",
        daemon_metric_text(runtime.get("heartbeat_at_utc"))
    );
    println!(
        "    Uptime:    {}",
        daemon_seconds_text(runtime.get("uptime_seconds"))
    );
    println!(
        "    Loops:     {}",
        daemon_metric_text(runtime.get("loop_count"))
    );
    if let Some(last_auto) = runtime.get("last_auto") {
        println!(
            "    Auto:      status={} message={}",
            daemon_metric_text(last_auto.get("status")),
            daemon_metric_text(last_auto.get("message")),
        );
    }
    if let Some(last_daily) = runtime.get("last_daily") {
        println!(
            "    Daily:     status={} schedule_date={}",
            daemon_metric_text(last_daily.get("status")),
            daemon_metric_text(last_daily.get("schedule_date")),
        );
    }
    if let Some(resources) = runtime.get("resources") {
        println!(
            "    CPU:       avg process={}%, avg machine={}%, CPU time={}, capacity={}%",
            daemon_number_text(resources.get("avg_cpu_percent_since_start"), 2),
            daemon_number_text(resources.get("avg_machine_cpu_percent_since_start"), 2),
            daemon_seconds_text(resources.get("total_cpu_seconds")),
            daemon_metric_text(resources.get("total_cpu_capacity_percent")),
        );
        let configured = config::runtime_resources();
        println!(
            "    CPU cap:   budget={}%, workers={} threads (~{}% CPU); logical CPUs={}",
            configured.cpu_budget_process_percent,
            configured.cpu_worker_threads,
            configured.cpu_worker_capacity_percent,
            configured.cpu_total_threads,
        );
        println!(
            "    Accelerators: {}",
            accelerators::accelerator_status_lines().join(" | ")
        );
        println!(
            "    Memory:    current RSS={}, peak RSS={}",
            daemon_bytes_mib_text(resources.get("current_rss_bytes")),
            daemon_bytes_mib_text(resources.get("peak_rss_bytes")),
        );
        println!(
            "    Memory cap: budget={} ({}% of {}, source={})",
            daemon_bytes_gib_text(configured.memory_budget_bytes),
            configured.memory_budget_percent,
            daemon_bytes_gib_text(configured.memory_total_bytes),
            configured.memory_source,
        );
        println!(
            "    Process:   open files/sockets={}, OS threads={}",
            daemon_metric_text(resources.get("open_file_descriptor_count")),
            daemon_metric_text(resources.get("os_thread_count")),
        );
    }
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
) -> anyhow::Result<bool> {
    let update_guard = match update_lock::acquire(
        "daemon",
        "daemon daily refresh",
        vec![
            "daemon".to_string(),
            "daily-refresh".to_string(),
            date.to_string(),
        ],
    ) {
        Ok(guard) => guard,
        Err(busy) => {
            daemon_log(serde_json::json!({
                "event": "daily_maintenance_skipped_update_lock_busy",
                "level": "warn",
                "schedule_date": date.to_string(),
                "message": update_lock::busy_message(&busy),
                "lock": {
                    "pid": busy.info.pid,
                    "source": busy.info.source,
                    "operation": busy.info.operation,
                    "command": busy.info.command,
                    "started_at_utc": busy.info.started_at_utc,
                    "runtime_home": busy.info.runtime_home,
                    "path": busy.path.display().to_string(),
                }
            }));
            return Ok(false);
        }
    };

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
    update_guard.finish("ok");
    daemon_log(serde_json::json!({
        "event": "daily_maintenance_completed",
        "level": "info",
        "schedule_date": date.to_string(),
    }));
    Ok(true)
}

// Sleeps in short ticks so daemon signals are handled promptly.
async fn sleep_until_signal_or_timeout(seconds: u64) {
    for _ in 0..seconds {
        if TERMINATE.load(Ordering::SeqCst) || RELOAD.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
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

    let started_at = Instant::now();
    let started_at_utc = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut loop_count: u64 = 0;
    let mut last_daily_attempt: Option<Instant> = None;
    let mut last_daily_status = serde_json::json!({"status": "not yet run"});
    let mut last_auto_status = serde_json::json!({"status": "not yet run"});
    let mut auto_market_closed_backoff_date: Option<NaiveDate> = None;
    while !TERMINATE.load(Ordering::SeqCst) {
        loop_count = loop_count.saturating_add(1);
        rotate_runtime_logs();
        if let Err(err) = config::load() {
            daemon_log(serde_json::json!({
                "event": "config_invalid",
                "level": "error",
                "config_file": config::config_path().display().to_string(),
                "error": err.to_string(),
                "message": "daemon paused until configuration is fixed",
            }));
            sleep_until_signal_or_timeout(config::daemon_auto_trade_interval_seconds()).await;
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
            last_auto_status = serde_json::json!({
                "status": "backoff_until_next_market_date",
                "market_date": market_date.to_string(),
                "message": "market was already observed closed today",
            });
        } else {
            match auto::run_auto_cycle("daemon", false).await {
                Ok(result) => {
                    last_auto_status = serde_json::json!({
                        "status": result.get("status").and_then(serde_json::Value::as_str).unwrap_or("ok"),
                        "account_count": result.get("account_count").cloned().unwrap_or_else(|| serde_json::json!("not available")),
                        "message": result.get("message").and_then(serde_json::Value::as_str).unwrap_or("not available"),
                    });
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
                    last_auto_status = serde_json::json!({
                        "status": "error",
                        "error": err.to_string(),
                    });
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
                last_daily_status = serde_json::json!({
                    "status": "running",
                    "schedule_date": date.to_string(),
                    "started_at_utc": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                });
                write_runtime_status(
                    started_at,
                    &started_at_utc,
                    loop_count,
                    &market_timezone,
                    market_date,
                    &last_auto_status,
                    &last_daily_status,
                );
                match run_daily_maintenance(&daily_config, date) {
                    Err(err) => {
                        last_daily_status = serde_json::json!({
                            "status": "error",
                            "schedule_date": date.to_string(),
                            "error": err.to_string(),
                        });
                        daemon_log(serde_json::json!({
                            "event": "daily_maintenance_failed",
                            "level": "error",
                            "schedule_date": date.to_string(),
                            "error": err.to_string(),
                        }));
                    }
                    Ok(false) => {
                        last_daily_status = serde_json::json!({
                            "status": "blocked_by_running_update",
                            "schedule_date": date.to_string(),
                            "checked_at_utc": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                        });
                    }
                    Ok(true) => {
                        last_daily_status = serde_json::json!({
                            "status": "ok",
                            "schedule_date": date.to_string(),
                            "completed_at_utc": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                        });
                    }
                }
            }
        }
        write_runtime_status(
            started_at,
            &started_at_utc,
            loop_count,
            &market_timezone,
            market_date,
            &last_auto_status,
            &last_daily_status,
        );
        let interval = config::daemon_auto_trade_interval_seconds();
        sleep_until_signal_or_timeout(interval).await;
    }

    let current_pid = std::process::id();
    if read_pid(&pid_path) == Some(current_pid) {
        let _ = fs::remove_file(&pid_path);
    }
    let _ = fs::remove_file(runtime_status_file());
    daemon_log(serde_json::json!({
        "event": "daemon_stopped",
        "level": "info",
        "pid": current_pid,
    }));
    Ok(())
}
