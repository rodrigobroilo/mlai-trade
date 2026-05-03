use crate::{auto, config, daemon, logging, paths};
use axum::body::Bytes;
use axum::extract::{Path, Query};
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::net::UnixStream;
use tokio::process::Command as TokioCommand;
use tokio::time::timeout;

static TERMINATE: AtomicBool = AtomicBool::new(false);
static RELOAD: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_signal(signal: libc::c_int) {
    match signal {
        libc::SIGTERM | libc::SIGINT => TERMINATE.store(true, Ordering::SeqCst),
        libc::SIGHUP => RELOAD.store(true, Ordering::SeqCst),
        _ => {}
    }
}

fn print_json(value: Value) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn api_log(mut event: Value) {
    if let Some(object) = event.as_object_mut() {
        object
            .entry("ts".to_string())
            .or_insert_with(|| json!(Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()));
        object
            .entry("component".to_string())
            .or_insert_with(|| json!("api"));
    }
    let line = serde_json::to_string(&event).unwrap_or_else(|err| {
        json!({
            "ts": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "component": "api",
            "event": "log_serialization_failed",
            "level": "error",
            "error": err.to_string(),
        })
        .to_string()
    });
    eprintln!("{line}");
}

fn log_api_request(
    method: &str,
    path: &str,
    status: StatusCode,
    started: Instant,
    command: Option<&[String]>,
    error: Option<&str>,
) {
    let (_, _, log_file) = api_config_paths();
    if let Err(err) = logging::rotate_if_needed(&log_file) {
        api_log(json!({
            "event": "log_rotation_failed",
            "level": "error",
            "log_file": log_file.display().to_string(),
            "error": err.to_string(),
        }));
    }
    let mut event = json!({
        "ts": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "event": "api_request",
        "method": method,
        "path": path,
        "status": status.as_u16(),
        "duration_ms": started.elapsed().as_millis(),
    });
    if let Some(command) = command {
        event["command"] = json!(command);
    }
    if let Some(error) = error {
        event["error"] = json!(error);
    }
    api_log(event);
}

#[derive(Debug, Clone)]
pub struct ApiStatus {
    pub enabled: bool,
    pub running: bool,
    pub pid: Option<u32>,
    pub socket_file: PathBuf,
    pub pid_file: PathBuf,
    pub log_file: PathBuf,
    pub request_timeout_seconds: u64,
    pub long_request_timeout_seconds: u64,
}

fn configured_path(value: Option<String>, default: PathBuf) -> PathBuf {
    value
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or(default)
}

fn api_config_paths() -> (PathBuf, PathBuf, PathBuf) {
    let config = config::load().unwrap_or_default();
    (
        configured_path(
            config.api.socket_file,
            paths::api_dir().join("mlai-trade-api.sock"),
        ),
        configured_path(
            config.api.pid_file,
            paths::tmp_dir().join("mlai-trade-api.pid"),
        ),
        configured_path(
            config.api.log_file,
            paths::logs_dir().join("mlai-trade-api.log"),
        ),
    )
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

pub fn status() -> ApiStatus {
    let (socket_file, pid_file, log_file) = api_config_paths();
    let pid = read_pid(&pid_file);
    let running = pid.map(process_alive).unwrap_or(false);
    ApiStatus {
        enabled: config::api_enabled(),
        running,
        pid: if running { pid } else { None },
        socket_file,
        pid_file,
        log_file,
        request_timeout_seconds: config::api_request_timeout_seconds(),
        long_request_timeout_seconds: config::api_long_request_timeout_seconds(),
    }
}

fn remove_stale_runtime_files(status: &ApiStatus) {
    if let Some(pid) = read_pid(&status.pid_file) {
        if !process_alive(pid) {
            let _ = fs::remove_file(&status.pid_file);
        }
    }
    if !status.running && status.socket_file.exists() {
        let _ = fs::remove_file(&status.socket_file);
    }
}

pub fn cmd_start(json_out: bool) -> anyhow::Result<()> {
    paths::ensure_runtime_dirs()?;
    if !config::api_enabled() {
        anyhow::bail!(
            "cannot run API server: api.enabled=false in {}. Set api.enabled=true before starting.",
            config::config_path().display()
        );
    }
    let status = status();
    if status.running {
        if json_out {
            print_json(json!({
                "status": "already_running",
                "pid": status.pid,
                "socket_file": status.socket_file.display().to_string(),
            }))?;
        } else {
            println!(
                "API server already running with pid {}.",
                status.pid.unwrap_or(0)
            );
            println!("Socket: {}", status.socket_file.display());
        }
        return Ok(());
    }
    remove_stale_runtime_files(&status);

    if let Some(parent) = status.log_file.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Err(err) = logging::ensure_json_lines(&status.log_file, "api") {
        eprintln!(
            "{}",
            json!({
                "ts": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                "component": "api",
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
            json!({
                "ts": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                "component": "api",
                "event": "log_rotation_failed",
                "level": "error",
                "log_file": status.log_file.display().to_string(),
                "error": err.to_string(),
            })
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
        .arg("api-run")
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
    if json_out {
        print_json(json!({
            "status": "started",
            "pid": child.id(),
            "socket_file": status.socket_file.display().to_string(),
            "log_file": status.log_file.display().to_string(),
        }))?;
    } else {
        println!("API server started with pid {}.", child.id());
        println!("Socket: {}", status.socket_file.display());
        println!("Log file: {}", status.log_file.display());
    }
    Ok(())
}

pub fn cmd_stop(json_out: bool) -> anyhow::Result<()> {
    let status = status();
    let Some(pid) = read_pid(&status.pid_file) else {
        let _ = fs::remove_file(&status.socket_file);
        if json_out {
            print_json(json!({"status": "not_running"}))?;
        } else {
            println!("API server is not running.");
        }
        return Ok(());
    };
    if !process_alive(pid) {
        let _ = fs::remove_file(&status.pid_file);
        let _ = fs::remove_file(&status.socket_file);
        if json_out {
            print_json(json!({"status": "stale_pid_removed", "pid": pid}))?;
        } else {
            println!("Removed stale API server pid file for pid {}.", pid);
        }
        return Ok(());
    }
    unsafe {
        if libc::kill(pid as libc::pid_t, libc::SIGTERM) != 0 {
            anyhow::bail!(
                "unable to stop API server pid {}: {}",
                pid,
                std::io::Error::last_os_error()
            );
        }
    }
    for _ in 0..50 {
        if !process_alive(pid) {
            let _ = fs::remove_file(&status.pid_file);
            let _ = fs::remove_file(&status.socket_file);
            if json_out {
                print_json(json!({"status": "stopped", "pid": pid}))?;
            } else {
                println!("API server stopped.");
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!("API server pid {} did not stop within timeout", pid)
}

pub fn cmd_reload(json_out: bool) -> anyhow::Result<()> {
    let status = status();
    let Some(pid) = status.pid else {
        anyhow::bail!("API server is not running");
    };
    unsafe {
        if libc::kill(pid as libc::pid_t, libc::SIGHUP) != 0 {
            anyhow::bail!(
                "unable to reload API server pid {}: {}",
                pid,
                std::io::Error::last_os_error()
            );
        }
    }
    if json_out {
        print_json(json!({"status": "reloaded", "pid": pid}))?;
    } else {
        println!("API server reload signal sent to pid {}.", pid);
    }
    Ok(())
}

pub fn cmd_restart(json_out: bool) -> anyhow::Result<()> {
    let _ = cmd_stop(false);
    cmd_start(json_out)
}

pub fn cmd_status(json_out: bool) -> anyhow::Result<()> {
    let status = status();
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "enabled": status.enabled,
                "running": status.running,
                "pid": status.pid,
                "socket_file": status.socket_file.display().to_string(),
                "pid_file": status.pid_file.display().to_string(),
                "log_file": status.log_file.display().to_string(),
                "request_timeout_seconds": status.request_timeout_seconds,
                "long_request_timeout_seconds": status.long_request_timeout_seconds,
                "routes": route_specs(),
            }))?
        );
        return Ok(());
    }

    println!("API Server Status");
    println!("  Enabled:     {}", status.enabled);
    println!("  Running:     {}", status.running);
    if let Some(pid) = status.pid {
        println!("  PID:         {}", pid);
    }
    println!("  Socket:      {}", status.socket_file.display());
    println!("  PID file:    {}", status.pid_file.display());
    println!("  Log file:    {}", status.log_file.display());
    println!("  Timeout:     {}s", status.request_timeout_seconds);
    println!(
        "  Long timeout: {}s (ML refresh / feed sync)",
        status.long_request_timeout_seconds
    );
    println!("  Routes:      GET/POST over Unix socket; run with --json to list route specs");
    Ok(())
}

pub async fn cmd_test(json_out: bool) -> anyhow::Result<()> {
    let status = status();
    if !status.running {
        anyhow::bail!(
            "API server is not running. Start it with `mlai-trade api start`, then retry `mlai-trade api test`."
        );
    }
    let mut stream = UnixStream::connect(&status.socket_file).await?;
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: mlai-trade\r\nConnection: close\r\n\r\n")
        .await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    let response = String::from_utf8_lossy(&response);
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| anyhow::anyhow!("API server returned an invalid HTTP response"))?;
    let status_line = headers.lines().next().unwrap_or_default().to_string();
    let body_json = serde_json::from_str::<Value>(body.trim()).unwrap_or_else(|_| {
        json!({
            "raw": body.trim(),
        })
    });

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ok": status_line.contains(" 200 "),
                "status_line": status_line,
                "socket_file": status.socket_file.display().to_string(),
                "response": body_json,
            }))?
        );
    } else {
        println!("API test");
        println!("  Socket:      {}", status.socket_file.display());
        println!("  HTTP:        {}", status_line);
        println!(
            "  Result:      {}",
            if status_line.contains(" 200 ") {
                "ok"
            } else {
                "failed"
            }
        );
    }
    Ok(())
}

pub async fn cmd_run() -> anyhow::Result<()> {
    paths::ensure_runtime_dirs()?;
    if !config::api_enabled() {
        anyhow::bail!(
            "cannot run API server: api.enabled=false in {}. Set api.enabled=true before starting.",
            config::config_path().display()
        );
    }
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

    let status = status();
    if let Some(parent) = status.pid_file.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(parent) = status.socket_file.parent() {
        fs::create_dir_all(parent)?;
    }
    if status.socket_file.exists() {
        fs::remove_file(&status.socket_file)?;
    }
    let listener = UnixListener::bind(&status.socket_file)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&status.socket_file, fs::Permissions::from_mode(0o600))?;
    }
    fs::write(&status.pid_file, std::process::id().to_string())?;
    if let Err(err) = logging::ensure_json_lines(&status.log_file, "api") {
        api_log(json!({
            "event": "log_json_sanitize_failed",
            "level": "error",
            "log_file": status.log_file.display().to_string(),
            "error": err.to_string(),
        }));
    }
    if let Err(err) = logging::rotate_if_needed(&status.log_file) {
        api_log(json!({
            "event": "log_rotation_failed",
            "level": "error",
            "log_file": status.log_file.display().to_string(),
            "error": err.to_string(),
        }));
    }

    api_log(json!({
        "event": "api_server_started",
        "level": "info",
        "pid": std::process::id(),
        "socket": status.socket_file.display().to_string(),
        "timeout_seconds": config::api_request_timeout_seconds(),
        "long_timeout_seconds": config::api_long_request_timeout_seconds(),
    }));

    let app = Router::new()
        .route("/health", get(handle_health))
        .route("/routes", get(handle_routes))
        .route("/{section}/{action}", get(handle_two).post(handle_two))
        .route(
            "/{section}/{action}/{target}",
            get(handle_three).post(handle_three),
        );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    let current_pid = std::process::id();
    if read_pid(&status.pid_file) == Some(current_pid) {
        let _ = fs::remove_file(&status.pid_file);
    }
    let _ = fs::remove_file(&status.socket_file);
    api_log(json!({
        "event": "api_server_stopped",
        "level": "info",
        "pid": current_pid,
    }));
    Ok(())
}

async fn shutdown_signal() {
    while !TERMINATE.load(Ordering::SeqCst) {
        if RELOAD.swap(false, Ordering::SeqCst) {
            api_log(json!({
                "event": "api_server_config_reloaded",
                "level": "info",
                "timeout_seconds": config::api_request_timeout_seconds(),
                "long_timeout_seconds": config::api_long_request_timeout_seconds(),
            }));
        }
        if !config::api_enabled() {
            api_log(json!({
                "event": "api_server_stopping",
                "level": "warn",
                "reason": "api.enabled=false",
            }));
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn handle_health(method: Method, uri: Uri) -> Response {
    let started = Instant::now();
    let status = status();
    let status_code = StatusCode::OK;
    log_api_request(
        method.as_str(),
        uri.path(),
        status_code,
        started,
        None,
        None,
    );
    json_response(
        status_code,
        json!({
            "ok": true,
            "service": "mlai-trade-api",
            "enabled": status.enabled,
            "running": status.running,
            "pid": status.pid,
            "socket_file": status.socket_file.display().to_string(),
        }),
    )
}

async fn handle_routes(method: Method, uri: Uri) -> Response {
    let started = Instant::now();
    let status_code = StatusCode::OK;
    log_api_request(
        method.as_str(),
        uri.path(),
        status_code,
        started,
        None,
        None,
    );
    json_response(status_code, json!({"ok": true, "routes": route_specs()}))
}

async fn handle_two(
    method: Method,
    uri: Uri,
    Path((section, action)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    handle_allowed_command(method, uri, section, action, None, query, body).await
}

async fn handle_three(
    method: Method,
    uri: Uri,
    Path((section, action, target)): Path<(String, String, String)>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    handle_allowed_command(method, uri, section, action, Some(target), query, body).await
}

async fn handle_allowed_command(
    method: Method,
    uri: Uri,
    section: String,
    action: String,
    target: Option<String>,
    query: HashMap<String, String>,
    body: Bytes,
) -> Response {
    let started = Instant::now();
    let method = method.as_str().to_string();
    let path = uri.path().to_string();
    if let Some((status, payload)) = handle_direct_command(&section, &action) {
        let error = payload
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_string);
        log_api_request(&method, &path, status, started, None, error.as_deref());
        return json_response(status, payload);
    }
    let input = match RequestInput::new(query, body) {
        Ok(input) => input,
        Err(err) => {
            return api_error_logged(StatusCode::BAD_REQUEST, err, &method, &path, started, None)
        }
    };
    let args = match build_cli_args(&section, &action, target.as_deref(), &input) {
        Ok(args) => args,
        Err(err) => {
            return api_error_logged(err.status, err.message, &method, &path, started, None)
        }
    };
    run_cli(args, method, path, started).await
}

fn handle_direct_command(section: &str, action: &str) -> Option<(StatusCode, Value)> {
    match (
        normalize_token(section).as_str(),
        normalize_token(action).as_str(),
    ) {
        ("daemon", "status") => {
            let status = daemon::status();
            Some((
                StatusCode::OK,
                json!({
                    "ok": true,
                    "enabled": status.enabled,
                    "running": status.running,
                    "pid": status.pid,
                    "pid_file": status.pid_file.display().to_string(),
                    "log_file": status.log_file.display().to_string(),
                    "interval_seconds": status.interval_seconds,
                }),
            ))
        }
        ("daemon", "reload") => {
            let status = daemon::status();
            let Some(pid) = status.pid else {
                return Some((
                    StatusCode::CONFLICT,
                    json!({"ok": false, "error": "daemon is not running"}),
                ));
            };
            unsafe {
                if libc::kill(pid as libc::pid_t, libc::SIGHUP) != 0 {
                    return Some((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        json!({"ok": false, "error": format!(
                            "unable to reload daemon pid {}: {}",
                            pid,
                            std::io::Error::last_os_error()
                        )}),
                    ));
                }
            }
            Some((
                StatusCode::OK,
                json!({"ok": true, "status": "reloaded", "pid": pid}),
            ))
        }
        _ => None,
    }
}

#[derive(Debug)]
struct ApiBuildError {
    status: StatusCode,
    message: String,
}

impl ApiBuildError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

struct RequestInput {
    query: HashMap<String, String>,
    body: Value,
}

impl RequestInput {
    fn new(query: HashMap<String, String>, body: Bytes) -> Result<Self, String> {
        let body = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body)
                .map_err(|err| format!("request body must be valid JSON: {err}"))?
        };
        Ok(Self { query, body })
    }

    fn value(&self, keys: &[&str]) -> Option<String> {
        for key in keys {
            if let Some(value) = self
                .query
                .get(*key)
                .filter(|value| !value.trim().is_empty())
            {
                return Some(value.trim().to_string());
            }
            if let Some(value) = self.body_value(key) {
                return Some(value);
            }
        }
        None
    }

    fn bool_value(&self, keys: &[&str]) -> bool {
        self.value(keys)
            .map(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false)
    }

    fn list(&self, keys: &[&str]) -> Vec<String> {
        for key in keys {
            if let Some(value) = self
                .query
                .get(*key)
                .filter(|value| !value.trim().is_empty())
            {
                return split_list(value);
            }
            if let Some(values) = self.body_list(key) {
                return values;
            }
        }
        Vec::new()
    }

    fn body_value(&self, key: &str) -> Option<String> {
        let obj = self.body.as_object()?;
        value_to_string(obj.get(key)?)
    }

    fn body_list(&self, key: &str) -> Option<Vec<String>> {
        let obj = self.body.as_object()?;
        let value = obj.get(key)?;
        match value {
            Value::Array(values) => Some(
                values
                    .iter()
                    .filter_map(value_to_string)
                    .flat_map(|value| split_list(&value))
                    .collect(),
            ),
            _ => value_to_string(value).map(|value| split_list(&value)),
        }
    }
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => {
            let value = value.trim();
            if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            }
        }
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn normalize_token(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('_', "-")
}

fn required_value(
    input: &RequestInput,
    target: Option<&str>,
    keys: &[&str],
    label: &str,
) -> Result<String, ApiBuildError> {
    target
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string())
        .or_else(|| input.value(keys))
        .ok_or_else(|| {
            ApiBuildError::new(
                StatusCode::BAD_REQUEST,
                format!("missing required {label}; pass it in the path or JSON/query"),
            )
        })
}

fn push_option(args: &mut Vec<String>, flag: &str, input: &RequestInput, keys: &[&str]) {
    if let Some(value) = input.value(keys) {
        args.push(flag.to_string());
        args.push(value);
    }
}

fn push_bool(args: &mut Vec<String>, flag: &str, input: &RequestInput, keys: &[&str]) {
    if input.bool_value(keys) {
        args.push(flag.to_string());
    }
}

fn push_accounts(args: &mut Vec<String>, input: &RequestInput) {
    let accounts = input.list(&["account", "accounts"]);
    if !accounts.is_empty() {
        args.push("--account".to_string());
        args.push(accounts.join(","));
    }
}

fn ensure_auto_trade_off() -> Result<(), ApiBuildError> {
    match auto::auto_trading_enabled() {
        Ok(false) => Ok(()),
        Ok(true) => Err(ApiBuildError::new(
            StatusCode::FORBIDDEN,
            "trade mutation API is disabled while auto-trading is enabled; disable auto-trading first",
        )),
        Err(err) => Err(ApiBuildError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("unable to read auto-trading state: {err}"),
        )),
    }
}

fn build_cli_args(
    section: &str,
    action: &str,
    target: Option<&str>,
    input: &RequestInput,
) -> Result<Vec<String>, ApiBuildError> {
    let section = normalize_token(section);
    let action = normalize_token(action);
    match (section.as_str(), action.as_str()) {
        ("daemon", "reload") => Ok(vec!["daemon".into(), "reload".into()]),
        ("daemon", "status") => Ok(vec!["daemon".into(), "status".into()]),

        ("ml", "refresh") => {
            let mut args = vec!["ml".into(), "refresh".into()];
            push_option(&mut args, "--days", input, &["days"]);
            push_bool(&mut args, "--quick", input, &["quick"]);
            push_option(&mut args, "--backend", input, &["backend", "lstm_backend"]);
            push_option(
                &mut args,
                "--walk-forward-folds",
                input,
                &["walk_forward_folds", "walk-forward-folds"],
            );
            push_option(&mut args, "--top-n", input, &["top_n", "top-n"]);
            push_option(
                &mut args,
                "--slippage-bps",
                input,
                &["slippage_bps", "slippage-bps"],
            );
            Ok(args)
        }
        ("ml", "explain") => {
            let symbol = required_value(input, target, &["symbol"], "symbol")?;
            Ok(vec!["ml".into(), "explain".into(), symbol])
        }
        ("ml", "explainable") => {
            let mut args = vec!["ml".into(), "explainable".into()];
            push_option(&mut args, "--limit", input, &["limit"]);
            Ok(args)
        }
        ("ml", "explained") => {
            let mut args = vec!["ml".into(), "explained".into()];
            push_option(&mut args, "--limit", input, &["limit"]);
            Ok(args)
        }
        ("ml", "status") => Ok(vec!["ml".into(), "status".into()]),

        ("market", "quote") => {
            let symbol = required_value(input, target, &["symbol"], "symbol")?;
            Ok(vec!["market".into(), "quote".into(), symbol])
        }
        ("market", "bars") => {
            let symbol = required_value(input, target, &["symbol"], "symbol")?;
            let mut args = vec!["market".into(), "bars".into(), symbol];
            push_option(&mut args, "--timeframe", input, &["timeframe"]);
            push_option(&mut args, "--limit", input, &["limit"]);
            Ok(args)
        }
        ("market", "news") => {
            let mut args = vec!["market".into(), "news".into()];
            if let Some(symbol) = target
                .map(str::to_string)
                .or_else(|| input.value(&["symbol"]))
            {
                args.push(symbol);
            }
            push_option(&mut args, "--limit", input, &["limit"]);
            Ok(args)
        }
        ("market", "clock") => Ok(vec!["market".into(), "clock".into()]),
        ("market", "calendar") => {
            let mut args = vec!["market".into(), "calendar".into()];
            push_option(&mut args, "--start", input, &["start"]);
            push_option(&mut args, "--end", input, &["end"]);
            for market in input.list(&["market", "markets"]) {
                args.push("--market".into());
                args.push(market);
            }
            Ok(args)
        }

        ("trade", "account") => {
            let mut args = vec!["trade".into(), "account".into()];
            push_accounts(&mut args, input);
            Ok(args)
        }
        ("trade", "orders") => {
            let mut args = vec!["trade".into(), "orders".into()];
            push_accounts(&mut args, input);
            push_option(&mut args, "--status", input, &["status"]);
            push_option(&mut args, "--limit", input, &["limit"]);
            push_bool(&mut args, "--sync", input, &["sync"]);
            Ok(args)
        }
        ("trade", "positions") => {
            let mut args = vec!["trade".into(), "positions".into()];
            push_accounts(&mut args, input);
            Ok(args)
        }
        ("trade", "buy") | ("trade", "sell") => {
            ensure_auto_trade_off()?;
            let symbol = required_value(input, target, &["symbol"], "symbol")?;
            let qty = required_value(input, None, &["qty", "quantity"], "qty")?;
            let mut args = vec!["trade".into(), action.clone(), symbol, qty];
            push_accounts(&mut args, input);
            push_option(&mut args, "--type", input, &["type", "order_type"]);
            push_option(
                &mut args,
                "--limit-price",
                input,
                &["limit_price", "limit-price"],
            );
            push_option(
                &mut args,
                "--stop-price",
                input,
                &["stop_price", "stop-price"],
            );
            push_option(&mut args, "--tif", input, &["tif", "time_in_force"]);
            Ok(args)
        }
        ("trade", "cancel") => {
            ensure_auto_trade_off()?;
            let order_id = required_value(input, target, &["order_id", "id"], "order_id")?;
            let mut args = vec!["trade".into(), "cancel".into(), order_id];
            push_accounts(&mut args, input);
            Ok(args)
        }
        ("trade", "close") => {
            ensure_auto_trade_off()?;
            let symbol = required_value(input, target, &["symbol"], "symbol")?;
            let mut args = vec!["trade".into(), "close".into(), symbol];
            push_accounts(&mut args, input);
            Ok(args)
        }

        ("data", "movers") => Ok(vec!["data".into(), "movers".into()]),
        ("data", "screen") => {
            let mut args = vec!["data".into(), "screen".into()];
            push_option(
                &mut args,
                "--min-volume",
                input,
                &["min_volume", "min-volume"],
            );
            Ok(args)
        }
        ("data", "watchlist") => Ok(vec!["data".into(), "watchlist".into()]),
        ("data", "suggest") => Ok(vec!["data".into(), "suggest".into()]),
        ("data", "status") => Ok(vec!["data".into(), "status".into()]),

        ("compliance", "wash") => Ok(vec!["compliance".into(), "wash".into()]),
        ("compliance", "pdt") => Ok(vec!["compliance".into(), "pdt".into()]),
        ("compliance", "tax") => {
            let mut args = vec!["compliance".into(), "tax".into()];
            push_bool(
                &mut args,
                "--accounts",
                input,
                &["accounts_list", "accounts-list"],
            );
            push_bool(&mut args, "--details", input, &["details"]);
            push_bool(&mut args, "--show", input, &["show"]);
            push_bool(
                &mut args,
                "--show-brackets",
                input,
                &["show_brackets", "show-brackets"],
            );
            push_option(&mut args, "--year", input, &["year"]);
            push_option(&mut args, "--quarter", input, &["quarter"]);
            push_option(&mut args, "--export", input, &["export"]);
            push_accounts(&mut args, input);
            Ok(args)
        }

        ("auto", "sync-orders") => Ok(vec!["auto".into(), "sync-orders".into()]),
        ("auto", "status") => Ok(vec!["auto".into(), "status".into()]),
        ("auto", "history") => {
            let mut args = vec!["auto".into(), "history".into()];
            push_option(&mut args, "--limit", input, &["limit"]);
            Ok(args)
        }
        ("auto", "config") => {
            let mut args = vec!["auto".into(), "config".into()];
            if let Some(key) = target.map(str::to_string).or_else(|| input.value(&["key"])) {
                args.push(key);
            }
            if let Some(value) = input.value(&["value"]) {
                args.push(value);
            }
            Ok(args)
        }

        ("feeds", "add") => {
            let symbols = if let Some(target) = target {
                split_list(target)
            } else {
                input.list(&["symbol", "symbols"])
            };
            if symbols.is_empty() {
                return Err(ApiBuildError::new(
                    StatusCode::BAD_REQUEST,
                    "missing required symbols for feeds add",
                ));
            }
            let mut args = vec!["feeds".into(), "add".into()];
            args.extend(symbols);
            Ok(args)
        }
        ("feeds", "remove") => {
            let symbol = required_value(input, target, &["symbol"], "symbol")?;
            Ok(vec!["feeds".into(), "remove".into(), symbol])
        }
        ("feeds", "sync") => {
            let mut args = vec!["feeds".into(), "sync".into()];
            push_option(&mut args, "--days", input, &["days"]);
            Ok(args)
        }
        ("feeds", "list") => Ok(vec!["feeds".into(), "list".into()]),
        ("feeds", "search") => {
            let query = required_value(input, target, &["query", "q"], "query")?;
            let mut args = vec!["feeds".into(), "search".into(), query];
            push_option(&mut args, "--limit", input, &["limit"]);
            Ok(args)
        }
        ("feeds", "graph") => {
            let symbol = required_value(input, target, &["symbol"], "symbol")?;
            Ok(vec!["feeds".into(), "graph".into(), symbol])
        }
        ("feeds", "sentiment") => {
            let symbol = required_value(input, target, &["symbol"], "symbol")?;
            Ok(vec!["feeds".into(), "sentiment".into(), symbol])
        }
        ("feeds", "correlate") => {
            let mut args = vec!["feeds".into(), "correlate".into()];
            push_option(&mut args, "--days", input, &["days"]);
            Ok(args)
        }
        ("feeds", "status") => Ok(vec!["feeds".into(), "status".into()]),

        ("runtime", _) => Err(ApiBuildError::new(
            StatusCode::FORBIDDEN,
            "runtime commands are not exposed through the API",
        )),
        _ => Err(ApiBuildError::new(
            StatusCode::NOT_FOUND,
            format!("API route is not allowlisted: {section}/{action}"),
        )),
    }
}

async fn run_cli(args: Vec<String>, method: String, path: String, started: Instant) -> Response {
    let timeout_seconds = api_timeout_for_command(&args);
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            return api_error_logged(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("unable to resolve current executable: {err}"),
                &method,
                &path,
                started,
                Some(&args),
            )
        }
    };

    let mut command = TokioCommand::new(exe);
    command
        .arg("--home")
        .arg(paths::root_dir())
        .arg("--json")
        .args(&args)
        .env("MLAI_TRADE_PROGRESS", "0")
        .env("MLAI_TRADE_API_REQUEST", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = match timeout(Duration::from_secs(timeout_seconds), command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            return api_error_logged(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("unable to execute command: {err}"),
                &method,
                &path,
                started,
                Some(&args),
            )
        }
        Err(_) => {
            return api_error_logged(
                StatusCode::REQUEST_TIMEOUT,
                format!("command exceeded API timeout of {timeout_seconds}s"),
                &method,
                &path,
                started,
                Some(&args),
            )
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let parsed = serde_json::from_str::<Value>(&stdout).ok();
    let parsed_stderr = serde_json::from_str::<Value>(&stderr).ok();
    let parsed_ok_false = parsed
        .as_ref()
        .and_then(|value| value.get("ok"))
        .and_then(Value::as_bool)
        == Some(false);
    let ok = output.status.success() && !parsed_ok_false;
    let parsed_status = parsed
        .as_ref()
        .or(parsed_stderr.as_ref())
        .and_then(|value| {
            value
                .get("status_code")
                .or_else(|| value.get("http_status"))
                .and_then(Value::as_u64)
        })
        .and_then(|code| u16::try_from(code).ok())
        .and_then(|code| StatusCode::from_u16(code).ok())
        .filter(|status| status.is_client_error() || status.is_server_error());
    let status = if ok {
        StatusCode::OK
    } else if parsed_ok_false || parsed_stderr.is_some() {
        parsed_status.unwrap_or(StatusCode::BAD_REQUEST)
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    let mut payload = json!({
        "ok": ok,
        "command": args,
        "exit_code": output.status.code(),
        "duration_ms": started.elapsed().as_millis(),
        "data": parsed,
    });
    if parsed.is_none() && !stdout.is_empty() {
        payload["text"] = Value::String(stdout);
    }
    if !stderr.is_empty() {
        payload["stderr"] = Value::String(stderr);
    }
    if let Some(stderr_json) = &parsed_stderr {
        payload["stderr_json"] = stderr_json.clone();
    }
    if parsed_ok_false {
        let parsed_error = parsed
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(Value::as_str)
            .or_else(|| {
                parsed
                    .as_ref()
                    .and_then(|value| value.get("message"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("command returned ok=false");
        payload["error"] = Value::String(parsed_error.to_string());
    } else if let Some(stderr_error) = parsed_stderr
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(Value::as_str)
    {
        payload["error"] = Value::String(stderr_error.to_string());
    }
    let error = if ok {
        None
    } else {
        payload
            .get("error")
            .and_then(Value::as_str)
            .or_else(|| payload.get("stderr").and_then(Value::as_str))
            .or_else(|| payload.get("text").and_then(Value::as_str))
    };
    log_api_request(&method, &path, status, started, Some(&args), error);
    json_response(status, payload)
}

fn api_timeout_for_command(args: &[String]) -> u64 {
    if matches!(
        (
            args.first().map(String::as_str),
            args.get(1).map(String::as_str)
        ),
        (Some("ml"), Some("refresh")) | (Some("feeds"), Some("sync"))
    ) {
        config::api_long_request_timeout_seconds()
    } else {
        config::api_request_timeout_seconds()
    }
}

fn api_error(status: StatusCode, message: impl Into<String>) -> Response {
    json_response(status, json!({"ok": false, "error": message.into()}))
}

fn api_error_logged(
    status: StatusCode,
    message: impl Into<String>,
    method: &str,
    path: &str,
    started: Instant,
    command: Option<&[String]>,
) -> Response {
    let message = message.into();
    log_api_request(method, path, status, started, command, Some(&message));
    api_error(status, message)
}

fn json_response(status: StatusCode, value: Value) -> Response {
    (status, Json(value)).into_response()
}

fn route_specs() -> Vec<Value> {
    vec![
        json!({"section": "daemon", "actions": ["reload", "status"]}),
        json!({"section": "ml", "actions": ["refresh", "explain", "explainable", "explained", "status"]}),
        json!({"section": "market", "actions": ["quote", "bars", "news", "clock", "calendar"]}),
        json!({"section": "trade", "actions": ["account", "orders", "positions", "buy", "sell", "cancel", "close"], "mutation_guard": "buy/sell/cancel/close require auto-trading disabled"}),
        json!({"section": "data", "actions": ["movers", "screen", "watchlist", "suggest", "status"]}),
        json!({"section": "compliance", "actions": ["wash", "pdt", "tax"]}),
        json!({"section": "auto", "actions": ["sync-orders", "status", "history", "config"]}),
        json!({"section": "feeds", "actions": ["add", "remove", "sync", "list", "search", "graph", "sentiment", "correlate", "status"]}),
    ]
}
