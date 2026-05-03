// Runtime process metrics.
//
// Function map:
// - current_process_usage_json(): returns resource usage for the running process.
// - current_rss_bytes(): reads current resident memory without external commands.
// - open_fd_count(): counts open descriptors from the platform fd directory.

use chrono::Utc;
use serde_json::{json, Value};
use std::fs;
use std::time::Instant;

// Converts an optional metric to a user-facing JSON value.
fn metric_value<T: serde::Serialize>(value: Option<T>) -> Value {
    value
        .map(|value| serde_json::to_value(value).unwrap_or_else(|_| json!("not available")))
        .unwrap_or_else(|| json!("not available"))
}

// Converts a timeval to fractional seconds.
fn timeval_seconds(value: libc::timeval) -> f64 {
    value.tv_sec as f64 + (value.tv_usec as f64 / 1_000_000.0)
}

// Returns the number of logical CPUs visible to the process.
fn logical_cpu_count() -> u64 {
    unsafe {
        let count = libc::sysconf(libc::_SC_NPROCESSORS_ONLN);
        if count > 0 {
            count as u64
        } else {
            1
        }
    }
}

#[cfg(target_os = "linux")]
// Reads current RSS from /proc/self/statm on Linux.
fn current_rss_bytes() -> Option<u64> {
    let statm = fs::read_to_string("/proc/self/statm").ok()?;
    let rss_pages = statm.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page_size <= 0 {
        return None;
    }
    rss_pages.checked_mul(page_size as u64)
}

#[cfg(target_os = "macos")]
// Reads current RSS from Mach task_info on macOS.
fn current_rss_bytes() -> Option<u64> {
    unsafe {
        let mut info: libc::mach_task_basic_info = std::mem::zeroed();
        let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
        #[allow(deprecated)]
        let task = libc::mach_task_self();
        let status = libc::task_info(
            task,
            libc::MACH_TASK_BASIC_INFO,
            &mut info as *mut _ as libc::task_info_t,
            &mut count,
        );
        if status == libc::KERN_SUCCESS {
            Some(info.resident_size as u64)
        } else {
            None
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
// Returns no current RSS when the platform does not expose a cheap local path.
fn current_rss_bytes() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
// Reads the Linux thread count from /proc/self/status.
fn thread_count() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|line| {
        line.strip_prefix("Threads:")
            .and_then(|value| value.trim().parse::<u64>().ok())
    })
}

#[cfg(not(target_os = "linux"))]
// Returns no thread count when the platform path is not available.
fn thread_count() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
// Returns the platform fd directory for the running process.
fn fd_dir() -> &'static str {
    "/proc/self/fd"
}

#[cfg(target_os = "macos")]
// Returns the platform fd directory for the running process.
fn fd_dir() -> &'static str {
    "/dev/fd"
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
// Returns an empty fd directory when the platform is unsupported.
fn fd_dir() -> &'static str {
    ""
}

// Counts open file descriptors for the current process.
fn open_fd_count() -> Option<u64> {
    let dir = fd_dir();
    if dir.is_empty() {
        return None;
    }
    Some(fs::read_dir(dir).ok()?.filter_map(Result::ok).count() as u64)
}

#[cfg(target_os = "linux")]
// Converts ru_maxrss from Linux KiB to bytes.
fn max_rss_bytes(raw: libc::c_long) -> Option<u64> {
    (raw as u64).checked_mul(1024)
}

#[cfg(target_os = "macos")]
// Converts ru_maxrss from macOS bytes to bytes.
fn max_rss_bytes(raw: libc::c_long) -> Option<u64> {
    Some(raw as u64)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
// Returns no peak RSS when rusage units are unknown.
fn max_rss_bytes(_raw: libc::c_long) -> Option<u64> {
    None
}

// Returns OS resource usage for the current mlai-trade process.
pub fn current_process_usage_json(started_at: Option<Instant>) -> Value {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    let rusage_ok = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) == 0 };
    let usage = if rusage_ok {
        Some(unsafe { usage.assume_init() })
    } else {
        None
    };
    let user_cpu_seconds = usage.map(|value| timeval_seconds(value.ru_utime));
    let system_cpu_seconds = usage.map(|value| timeval_seconds(value.ru_stime));
    let total_cpu_seconds = user_cpu_seconds.unwrap_or(0.0) + system_cpu_seconds.unwrap_or(0.0);
    let uptime_seconds = started_at.map(|started| started.elapsed().as_secs_f64());
    let avg_cpu_percent_since_start = uptime_seconds
        .filter(|value| *value > 0.0)
        .map(|uptime| (total_cpu_seconds / uptime) * 100.0 / logical_cpu_count() as f64);
    json!({
        "pid": std::process::id(),
        "sampled_at_utc": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "uptime_seconds": metric_value(uptime_seconds),
        "logical_cpus": logical_cpu_count(),
        "user_cpu_seconds": metric_value(user_cpu_seconds),
        "system_cpu_seconds": metric_value(system_cpu_seconds),
        "total_cpu_seconds": metric_value(if rusage_ok { Some(total_cpu_seconds) } else { None }),
        "avg_cpu_percent_since_start": metric_value(avg_cpu_percent_since_start),
        "current_rss_bytes": metric_value(current_rss_bytes()),
        "peak_rss_bytes": metric_value(usage.and_then(|value| max_rss_bytes(value.ru_maxrss))),
        "open_fd_count": metric_value(open_fd_count()),
        "thread_count": metric_value(thread_count()),
    })
}
