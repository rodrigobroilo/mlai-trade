// Runtime update lock for long data/ML refresh jobs.
//
// Function map:
// - acquire(): atomically reserves the update slot or reports who owns it.
// - UpdateLockGuard::finish(): logs release status and removes the lock file.
// - busy_message(): formats user-facing lock holder details.

use crate::{logging, paths, process};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, AtomicI32, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

const LOCK_HELD_ENV: &str = "MLAI_TRADE_UPDATE_LOCK_HELD";
static SIGNAL_CAUGHT: AtomicI32 = AtomicI32::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateLockInfo {
    pub pid: u32,
    pub source: String,
    pub operation: String,
    pub command: Vec<String>,
    pub started_at_utc: String,
    pub runtime_home: String,
}

#[derive(Debug)]
pub struct UpdateLockBusy {
    pub info: UpdateLockInfo,
    pub path: PathBuf,
}

pub struct UpdateLockGuard {
    info: UpdateLockInfo,
    path: PathBuf,
    started: Instant,
    finished: bool,
    signal_watcher_done: Option<Arc<AtomicBool>>,
}

// Returns the runtime update lock path.
pub fn lock_path() -> PathBuf {
    paths::tmp_dir().join("mlai-trade-update.lock")
}

// Returns true when the current process already owns the update slot.
pub fn lock_held_by_current_process() -> bool {
    std::env::var(LOCK_HELD_ENV).ok().as_deref() == Some("1")
}

// Returns the command line used for lock metadata.
pub fn current_command() -> Vec<String> {
    std::env::args().collect()
}

// Returns the public source label for the current command process.
pub fn current_source() -> &'static str {
    match std::env::var("MLAI_TRADE_API_REQUEST").ok().as_deref() {
        Some("1") => "api",
        _ => "cli",
    }
}

// Formats a busy lock for users.
pub fn busy_message(busy: &UpdateLockBusy) -> String {
    format!(
        "system update already running: source={} operation={} pid={} started_at_utc={} lock_file={}",
        busy.info.source,
        busy.info.operation,
        busy.info.pid,
        busy.info.started_at_utc,
        busy.path.display()
    )
}

// Captures update cancellation signals for the watcher thread.
extern "C" fn handle_update_signal(signal: libc::c_int) {
    SIGNAL_CAUGHT.store(signal, Ordering::SeqCst);
}

// Returns a stable label for a Unix signal.
fn signal_name(signal: i32) -> &'static str {
    match signal {
        libc::SIGINT => "SIGINT",
        libc::SIGTERM => "SIGTERM",
        _ => "unknown",
    }
}

// Reads the current lock file if it contains valid JSON metadata.
fn read_lock(path: &PathBuf) -> Option<UpdateLockInfo> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str::<UpdateLockInfo>(&text).ok()
}

// Logs lock lifecycle events to all update-related component logs.
fn log_lock_event(event: serde_json::Value) {
    for component in ["daemon", "data", "ml", "training", "feeds"] {
        logging::append_component_event_lossy(component, event.clone());
    }
}

// Logs a signal-triggered cancellation and exits before another update can start.
fn install_signal_watcher(
    info: UpdateLockInfo,
    path: PathBuf,
    started: Instant,
) -> Arc<AtomicBool> {
    SIGNAL_CAUGHT.store(0, Ordering::SeqCst);
    unsafe {
        libc::signal(
            libc::SIGINT,
            handle_update_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            handle_update_signal as *const () as libc::sighandler_t,
        );
    }

    let done = Arc::new(AtomicBool::new(false));
    let thread_done = Arc::clone(&done);
    let _ = thread::spawn(move || {
        while !thread_done.load(Ordering::SeqCst) {
            let signal = SIGNAL_CAUGHT.load(Ordering::SeqCst);
            if signal != 0 {
                let finished_at_utc = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
                log_lock_event(serde_json::json!({
                    "event": "update_lock_released",
                    "level": "warn",
                    "pid": info.pid,
                    "source": info.source,
                    "operation": info.operation,
                    "started_at_utc": info.started_at_utc,
                    "finished_at_utc": finished_at_utc,
                    "duration_ms": started.elapsed().as_millis(),
                    "status": "cancelled_by_signal",
                    "signal": signal,
                    "signal_name": signal_name(signal),
                    "lock_file": path.display().to_string(),
                }));
                if read_lock(&path)
                    .map(|current| current.pid == info.pid)
                    .unwrap_or(false)
                {
                    let _ = fs::remove_file(&path);
                }
                std::process::exit(128 + signal);
            }
            thread::sleep(Duration::from_millis(200));
        }
    });
    done
}

// Removes an existing stale lock when the owner process is gone.
fn remove_stale_lock(path: &PathBuf) -> io::Result<bool> {
    let Some(info) = read_lock(path) else {
        fs::remove_file(path)?;
        return Ok(true);
    };
    if process::pid_alive(info.pid) {
        return Ok(false);
    }
    fs::remove_file(path)?;
    log_lock_event(serde_json::json!({
        "event": "update_lock_stale_removed",
        "level": "warn",
        "pid": info.pid,
        "source": info.source,
        "operation": info.operation,
        "started_at_utc": info.started_at_utc,
        "lock_file": path.display().to_string(),
    }));
    Ok(true)
}

// Atomically acquires the runtime update lock.
pub fn acquire(
    source: &str,
    operation: &str,
    command: Vec<String>,
) -> Result<UpdateLockGuard, Box<UpdateLockBusy>> {
    if lock_held_by_current_process() {
        let info = UpdateLockInfo {
            pid: std::process::id(),
            source: source.to_string(),
            operation: operation.to_string(),
            command,
            started_at_utc: Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            runtime_home: paths::root_dir().display().to_string(),
        };
        return Ok(UpdateLockGuard {
            info,
            path: lock_path(),
            started: Instant::now(),
            finished: true,
            signal_watcher_done: None,
        });
    }

    let path = lock_path();
    if let Some(parent) = path.parent() {
        let _ = paths::ensure_private_dir(parent);
    }

    if path.exists() {
        match remove_stale_lock(&path) {
            Ok(true) => {}
            Ok(false) | Err(_) => {
                let info = read_lock(&path).unwrap_or_else(|| UpdateLockInfo {
                    pid: 0,
                    source: "unknown".into(),
                    operation: "unknown".into(),
                    command: Vec::new(),
                    started_at_utc: "unknown".into(),
                    runtime_home: paths::root_dir().display().to_string(),
                });
                return Err(Box::new(UpdateLockBusy { info, path }));
            }
        }
    }

    let info = UpdateLockInfo {
        pid: std::process::id(),
        source: source.to_string(),
        operation: operation.to_string(),
        command,
        started_at_utc: Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        runtime_home: paths::root_dir().display().to_string(),
    };
    let payload = serde_json::to_vec_pretty(&info).unwrap_or_default();
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            if let Err(err) = file.write_all(&payload).and_then(|_| file.flush()) {
                let _ = fs::remove_file(&path);
                return Err(Box::new(UpdateLockBusy {
                    info: UpdateLockInfo {
                        pid: 0,
                        source: "unknown".into(),
                        operation: format!("lock write failed: {err}"),
                        command: Vec::new(),
                        started_at_utc: "unknown".into(),
                        runtime_home: paths::root_dir().display().to_string(),
                    },
                    path,
                }));
            }
            let _ = paths::harden_file_if_exists(&path);
        }
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            let info = read_lock(&path).unwrap_or_else(|| UpdateLockInfo {
                pid: 0,
                source: "unknown".into(),
                operation: "unknown".into(),
                command: Vec::new(),
                started_at_utc: "unknown".into(),
                runtime_home: paths::root_dir().display().to_string(),
            });
            return Err(Box::new(UpdateLockBusy { info, path }));
        }
        Err(_) => {
            let info = read_lock(&path).unwrap_or_else(|| UpdateLockInfo {
                pid: 0,
                source: "unknown".into(),
                operation: "unknown".into(),
                command: Vec::new(),
                started_at_utc: "unknown".into(),
                runtime_home: paths::root_dir().display().to_string(),
            });
            return Err(Box::new(UpdateLockBusy { info, path }));
        }
    }

    std::env::set_var(LOCK_HELD_ENV, "1");
    log_lock_event(serde_json::json!({
        "event": "update_lock_acquired",
        "level": "info",
        "pid": info.pid,
        "source": info.source,
        "operation": info.operation,
        "command": info.command,
        "started_at_utc": info.started_at_utc,
        "lock_file": path.display().to_string(),
    }));

    let started = Instant::now();
    let signal_watcher_done =
        (source == "cli").then(|| install_signal_watcher(info.clone(), path.clone(), started));

    Ok(UpdateLockGuard {
        info,
        path,
        started,
        finished: false,
        signal_watcher_done,
    })
}

impl UpdateLockGuard {
    // Logs completion status and releases the update lock.
    pub fn finish(mut self, status: &str) {
        self.release(status);
    }

    // Releases the lock file if the current process owns it.
    fn release(&mut self, status: &str) {
        if self.finished {
            return;
        }
        self.finished = true;
        if let Some(done) = &self.signal_watcher_done {
            done.store(true, Ordering::SeqCst);
        }
        let finished_at_utc = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        log_lock_event(serde_json::json!({
            "event": "update_lock_released",
            "level": if status == "ok" { "info" } else { "error" },
            "pid": self.info.pid,
            "source": self.info.source,
            "operation": self.info.operation,
            "started_at_utc": self.info.started_at_utc,
            "finished_at_utc": finished_at_utc,
            "duration_ms": self.started.elapsed().as_millis(),
            "status": status,
            "lock_file": self.path.display().to_string(),
        }));
        if read_lock(&self.path)
            .map(|info| info.pid == self.info.pid)
            .unwrap_or(false)
        {
            let _ = fs::remove_file(&self.path);
        }
        std::env::remove_var(LOCK_HELD_ENV);
    }
}

impl Drop for UpdateLockGuard {
    fn drop(&mut self) {
        self.release("error_or_interrupted");
    }
}
