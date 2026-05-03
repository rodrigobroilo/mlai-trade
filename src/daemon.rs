use crate::{auto, config, logging, paths, tax};
use chrono::{NaiveDate, NaiveTime, Utc};
use chrono_tz::Tz;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static TERMINATE: AtomicBool = AtomicBool::new(false);
static RELOAD: AtomicBool = AtomicBool::new(false);

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

fn configured_path(value: Option<String>, default: PathBuf) -> PathBuf {
    value
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or(default)
}

fn daemon_config_paths() -> (PathBuf, PathBuf) {
    let config = config::load().unwrap_or_default();
    (
        configured_path(
            config.daemon.pid_file,
            paths::tmp_dir().join("mlai-trade.pid"),
        ),
        configured_path(
            config.daemon.log_file,
            paths::logs_dir().join("mlai-trade-daemon.log"),
        ),
    )
}

fn pid_file() -> PathBuf {
    daemon_config_paths().0
}

fn log_file() -> PathBuf {
    daemon_config_paths().1
}

fn daily_refresh_stamp_file() -> PathBuf {
    paths::tmp_dir().join("mlai-trade-daily-refresh.stamp")
}

fn read_daily_refresh_stamp() -> Option<String> {
    fs::read_to_string(daily_refresh_stamp_file())
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn write_daily_refresh_stamp(date: NaiveDate) -> anyhow::Result<()> {
    let path = daily_refresh_stamp_file();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, date.to_string())?;
    Ok(())
}

fn read_pid(path: &PathBuf) -> Option<u32> {
    fs::read_to_string(path)
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
}

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

fn remove_stale_pid(path: &PathBuf) {
    if let Some(pid) = read_pid(path) {
        if !process_alive(pid) {
            let _ = fs::remove_file(path);
        }
    }
}

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
        fs::create_dir_all(parent)?;
    }
    if let Err(err) = logging::rotate_if_needed(&status.log_file) {
        eprintln!(
            "warning: unable to rotate daemon log {}: {}",
            status.log_file.display(),
            err
        );
    }
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&status.log_file)?;
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

pub fn cmd_restart(json: bool) -> anyhow::Result<()> {
    let _ = cmd_stop(false);
    cmd_start(json)
}

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
        "  Daily:       enabled={} time={} {} last={}",
        status.daily_refresh.enabled,
        status.daily_refresh.time,
        status.daily_refresh.timezone,
        status.daily_refresh_last_date.as_deref().unwrap_or("never")
    );
    Ok(())
}

fn parse_daily_time(value: &str) -> NaiveTime {
    NaiveTime::parse_from_str(value.trim(), "%H:%M:%S")
        .or_else(|_| NaiveTime::parse_from_str(value.trim(), "%H:%M"))
        .unwrap_or_else(|_| NaiveTime::from_hms_opt(18, 30, 0).unwrap())
}

fn daily_refresh_due(config: &config::DaemonDailyRefreshConfig) -> Option<NaiveDate> {
    if !config.enabled {
        return None;
    }
    let timezone = config
        .timezone
        .parse::<Tz>()
        .unwrap_or(chrono_tz::America::New_York);
    let now = Utc::now().with_timezone(&timezone);
    if now.time() < parse_daily_time(&config.time) {
        return None;
    }
    let today = now.date_naive();
    if read_daily_refresh_stamp().as_deref() == Some(&today.to_string()) {
        return None;
    }
    Some(today)
}

fn run_daemon_command(label: &str, args: &[String]) -> anyhow::Result<()> {
    eprintln!(
        "{} daemon daily maintenance step started: {} ({})",
        Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
        label,
        args.join(" ")
    );
    let status = Command::new(std::env::current_exe()?)
        .arg("--home")
        .arg(paths::root_dir())
        .args(args)
        .env("MLAI_TRADE_PROGRESS", "0")
        .status()?;
    if !status.success() {
        anyhow::bail!("daemon daily maintenance step failed: {label} ({status})");
    }
    eprintln!(
        "{} daemon daily maintenance step completed: {}",
        Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
        label
    );
    Ok(())
}

fn run_daily_maintenance(
    config: &config::DaemonDailyRefreshConfig,
    date: NaiveDate,
) -> anyhow::Result<()> {
    eprintln!(
        "{} daemon daily maintenance started schedule_date={} scheduled_time={} {}",
        Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
        date,
        config.time,
        config.timezone
    );
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
    eprintln!(
        "{} daemon daily maintenance completed schedule_date={}",
        Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
        date
    );
    Ok(())
}

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
        fs::create_dir_all(parent)?;
    }
    fs::write(&pid_path, std::process::id().to_string())?;
    if let Err(err) = logging::rotate_if_needed(&log_file()) {
        eprintln!(
            "{} daemon log rotation failed: {}",
            Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
            err
        );
    }
    writeln!(
        std::io::stdout(),
        "{} daemon started pid={} interval={}s",
        Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
        std::process::id(),
        config::daemon_auto_trade_interval_seconds()
    )?;

    let mut last_daily_attempt: Option<Instant> = None;
    while !TERMINATE.load(Ordering::SeqCst) {
        if let Err(err) = logging::rotate_if_needed(&log_file()) {
            eprintln!(
                "{} daemon log rotation failed: {}",
                Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
                err
            );
        }
        if !config::daemon_enabled() {
            eprintln!(
                "{} daemon.enabled=false; stopping daemon",
                Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
            );
            break;
        }
        if RELOAD.swap(false, Ordering::SeqCst) {
            eprintln!(
                "{} daemon config reloaded; interval={}s",
                Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
                config::daemon_auto_trade_interval_seconds()
            );
        }
        if let Err(err) = auto::cmd_auto_run_with_source(true, "daemon").await {
            eprintln!(
                "{} auto run failed: {}",
                Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
                err
            );
        }
        if let Err(err) = tax::refresh_current_year_estimates() {
            eprintln!(
                "{} tax refresh failed: {}",
                Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
                err
            );
        }
        let daily_config = config::daemon_daily_refresh_config();
        if let Some(date) = daily_refresh_due(&daily_config) {
            let retry_ok = last_daily_attempt
                .map(|last| last.elapsed() >= Duration::from_secs(3600))
                .unwrap_or(true);
            if retry_ok {
                last_daily_attempt = Some(Instant::now());
                if let Err(err) = run_daily_maintenance(&daily_config, date) {
                    eprintln!(
                        "{} daemon daily maintenance failed: {}",
                        Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
                        err
                    );
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
    eprintln!(
        "{} daemon stopped pid={}",
        Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
        current_pid
    );
    Ok(())
}
