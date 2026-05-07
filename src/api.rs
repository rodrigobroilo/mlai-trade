// Unix-socket JSON API.
//
// Function map:
// - cmd_start/stop/reload/restart/status/test/run(): API lifecycle commands.
// - handle_*(): route incoming Unix-socket HTTP requests to allowed CLI actions.
// - build_cli_args(): converts JSON/body/query input into safe CLI arguments.
// - run_cli(): executes allowed commands with timeout, redaction, and JSON output.

use crate::{accelerators, auto, config, daemon, logging, paths, process};
use axum::body::{to_bytes, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{header, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine;
use chrono::Utc;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::convert::TryFrom;
use std::fs;
use std::io::{BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::net::UnixStream;
use tokio::process::Command as TokioCommand;
use tokio::sync::Mutex;
use tokio::time::timeout;

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

// Prints json in human-readable form.
fn print_json(value: Value) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

// Runs the api log API helper.
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

// Handles log api request logic.
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

// Runs the api limits json API helper.
fn api_limits_json(limits: &config::ApiLimitConfig) -> Value {
    json!({
        "max_concurrent_requests": limits.max_concurrent_requests,
        "max_concurrent_long_requests": limits.max_concurrent_long_requests,
        "rate_limit_per_minute": limits.rate_limit_per_minute,
        "max_body_bytes": limits.max_body_bytes,
        "overload_retry_after_seconds": limits.overload_retry_after_seconds,
    })
}

// Returns whether long api command is true.
fn is_long_api_command(args: &[String]) -> bool {
    matches!(
        (
            args.first().map(String::as_str),
            args.get(1).map(String::as_str)
        ),
        (Some("ml"), Some("refresh")) | (Some("feeds"), Some("sync"))
    )
}

// Handles try increment counter logic.
fn try_increment_counter(counter: &AtomicUsize, limit: usize) -> bool {
    loop {
        let current = counter.load(Ordering::SeqCst);
        if current >= limit {
            return false;
        }
        if counter
            .compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return true;
        }
    }
}

// Handles check api rate limit logic.
async fn check_api_rate_limit(
    state: &Arc<ApiRuntimeState>,
    limits: &config::ApiLimitConfig,
    method: &str,
    path: &str,
    started: Instant,
    command: Option<&[String]>,
) -> Result<(), Response> {
    state.total_requests.fetch_add(1, Ordering::SeqCst);
    let mut rate = state.rate.lock().await;
    let elapsed = rate.window_start.elapsed();
    if elapsed >= Duration::from_secs(60) {
        rate.window_start = Instant::now();
        rate.count = 0;
    }
    if rate.count >= limits.rate_limit_per_minute {
        state.rejected_requests.fetch_add(1, Ordering::SeqCst);
        let retry_after = 60u64.saturating_sub(elapsed.as_secs()).max(1);
        return Err(api_backoff_logged(
            "rate_limit_exceeded",
            retry_after,
            method,
            path,
            started,
            command,
        ));
    }
    rate.count += 1;
    Ok(())
}

// Handles acquire api request guard logic.
fn acquire_api_request_guard(
    state: &Arc<ApiRuntimeState>,
    limits: &config::ApiLimitConfig,
    long_request: bool,
    method: &str,
    path: &str,
    started: Instant,
    command: Option<&[String]>,
) -> Result<ApiRequestGuard, Response> {
    if !try_increment_counter(&state.active_requests, limits.max_concurrent_requests) {
        state.rejected_requests.fetch_add(1, Ordering::SeqCst);
        return Err(api_backoff_logged(
            "max_concurrent_requests_exceeded",
            limits.overload_retry_after_seconds,
            method,
            path,
            started,
            command,
        ));
    }

    if long_request
        && !try_increment_counter(
            &state.active_long_requests,
            limits.max_concurrent_long_requests,
        )
    {
        state.rejected_requests.fetch_add(1, Ordering::SeqCst);
        state.active_requests.fetch_sub(1, Ordering::SeqCst);
        return Err(api_backoff_logged(
            "max_concurrent_long_requests_exceeded",
            limits.overload_retry_after_seconds,
            method,
            path,
            started,
            command,
        ));
    }

    Ok(ApiRequestGuard {
        state: Arc::clone(state),
        long_request,
    })
}

// Runs the api backoff logged API helper.
fn api_backoff_logged(
    reason: &str,
    retry_after_seconds: u64,
    method: &str,
    path: &str,
    started: Instant,
    command: Option<&[String]>,
) -> Response {
    let message = format!("API overloaded: {reason}; retry after {retry_after_seconds}s");
    log_api_request(
        method,
        path,
        StatusCode::TOO_MANY_REQUESTS,
        started,
        command,
        Some(&message),
    );
    let value = json!({
        "ok": false,
        "error": message,
        "reason": reason,
        "retry_after_seconds": retry_after_seconds,
        "status_code": StatusCode::TOO_MANY_REQUESTS.as_u16(),
    });
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, retry_after_seconds.to_string())],
        Json(value),
    )
        .into_response()
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
    pub limits: config::ApiLimitConfig,
}

#[derive(Debug, Clone)]
pub struct ApiSslStatus {
    pub api_enabled: bool,
    pub enabled: bool,
    pub running: bool,
    pub pid: Option<u32>,
    pub bind_host: String,
    pub udp_port: u16,
    pub auth_enabled: bool,
}

#[derive(Debug, Clone)]
struct HttpsDnsAnswer {
    priority: u16,
    target: String,
    alpn: Vec<String>,
    port: Option<u16>,
    ttl: u32,
}

#[derive(Debug, Clone)]
struct HttpsDnsCheck {
    ok: bool,
    domain: String,
    resolver: String,
    required_alpn: String,
    required_port: u16,
    answers: Vec<HttpsDnsAnswer>,
    errors: Vec<String>,
}

#[derive(Debug)]
struct ApiRuntimeState {
    started_at: Instant,
    started_at_utc: String,
    active_requests: AtomicUsize,
    active_long_requests: AtomicUsize,
    total_requests: AtomicUsize,
    rejected_requests: AtomicUsize,
    rate: Mutex<ApiRateState>,
}

impl ApiRuntimeState {
    // Constructs a new instance with the provided inputs.
    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            started_at_utc: Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            active_requests: AtomicUsize::new(0),
            active_long_requests: AtomicUsize::new(0),
            total_requests: AtomicUsize::new(0),
            rejected_requests: AtomicUsize::new(0),
            rate: Mutex::new(ApiRateState {
                window_start: Instant::now(),
                count: 0,
            }),
        }
    }

    // Builds a runtime metrics snapshot for status and health responses.
    fn runtime_json(&self) -> Value {
        let uptime_seconds = self.started_at.elapsed().as_secs_f64();
        let total_requests = self.total_requests.load(Ordering::SeqCst);
        json!({
            "started_at_utc": self.started_at_utc,
            "uptime_seconds": uptime_seconds,
            "active_requests": self.active_requests.load(Ordering::SeqCst),
            "active_long_requests": self.active_long_requests.load(Ordering::SeqCst),
            "total_requests": total_requests,
            "rejected_requests": self.rejected_requests.load(Ordering::SeqCst),
            "average_requests_per_second": if uptime_seconds > 0.0 {
                total_requests as f64 / uptime_seconds
            } else {
                0.0
            },
            "resources": process::current_process_usage_json(Some(self.started_at)),
        })
    }
}

#[derive(Debug)]
struct ApiRateState {
    window_start: Instant,
    count: usize,
}

struct ApiRequestGuard {
    state: Arc<ApiRuntimeState>,
    long_request: bool,
}

impl Drop for ApiRequestGuard {
    // Releases owned runtime resources when the wrapper is dropped.
    fn drop(&mut self) {
        self.state.active_requests.fetch_sub(1, Ordering::SeqCst);
        if self.long_request {
            self.state
                .active_long_requests
                .fetch_sub(1, Ordering::SeqCst);
        }
    }
}

// Returns configured path with defaults applied.
fn configured_path(value: Option<String>, base: PathBuf, default_name: &str) -> PathBuf {
    paths::path_in_runtime_dir(base, value, default_name)
}

// Runs the api config paths API helper.
fn api_config_paths() -> (PathBuf, PathBuf, PathBuf) {
    (
        configured_path(
            config::api_unix_socket_file(),
            paths::api_dir(),
            "mlai-trade-api.sock",
        ),
        configured_path(
            config::api_unix_pid_file(),
            paths::tmp_dir(),
            "mlai-trade-api.pid",
        ),
        configured_path(
            config::api_unix_log_file(),
            paths::logs_dir(),
            "mlai-trade-api.log",
        ),
    )
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

// Returns a socket address for a DNS resolver.
fn resolver_socket_addr(resolver: &str) -> anyhow::Result<SocketAddr> {
    if let Ok(ip) = resolver.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, 53));
    }
    let mut addrs = format!("{resolver}:53").to_socket_addrs()?;
    addrs
        .next()
        .ok_or_else(|| anyhow::anyhow!("resolver {resolver} did not resolve"))
}

// Returns system DNS resolvers from resolv.conf, with a public fallback.
fn dns_resolvers() -> Vec<String> {
    let mut resolvers = fs::read_to_string("/etc/resolv.conf")
        .ok()
        .map(|data| {
            data.lines()
                .filter_map(|line| {
                    let line = line.trim();
                    line.strip_prefix("nameserver")
                        .and_then(|rest| rest.split_whitespace().next())
                        .map(str::to_string)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if resolvers.is_empty() {
        resolvers.push("1.1.1.1".to_string());
    }
    resolvers
}

// Encodes a DNS name into wire format.
fn encode_dns_name(domain: &str) -> anyhow::Result<Vec<u8>> {
    let domain = domain.trim().trim_end_matches('.');
    if domain.is_empty() {
        anyhow::bail!("domain is empty");
    }
    let mut out = Vec::new();
    for label in domain.split('.') {
        if label.is_empty() || label.len() > 63 {
            anyhow::bail!("invalid DNS label in {domain}");
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    Ok(out)
}

// Parses a DNS name, including compression pointers.
fn parse_dns_name(buf: &[u8], offset: usize) -> anyhow::Result<(String, usize)> {
    let mut labels = Vec::new();
    let mut pos = offset;
    let mut next = offset;
    let mut jumped = false;
    for _ in 0..32 {
        let Some(&len) = buf.get(pos) else {
            anyhow::bail!("DNS name truncated");
        };
        if len & 0xc0 == 0xc0 {
            let Some(&low) = buf.get(pos + 1) else {
                anyhow::bail!("DNS compression pointer truncated");
            };
            let pointer = (((len as usize) & 0x3f) << 8) | low as usize;
            if !jumped {
                next = pos + 2;
            }
            pos = pointer;
            jumped = true;
            continue;
        }
        if len == 0 {
            if !jumped {
                next = pos + 1;
            }
            return Ok((
                if labels.is_empty() {
                    ".".to_string()
                } else {
                    labels.join(".")
                },
                next,
            ));
        }
        if len & 0xc0 != 0 {
            anyhow::bail!("unsupported DNS label encoding");
        }
        let start = pos + 1;
        let end = start + len as usize;
        if end > buf.len() {
            anyhow::bail!("DNS label truncated");
        }
        labels.push(String::from_utf8_lossy(&buf[start..end]).to_string());
        pos = end;
    }
    anyhow::bail!("DNS name compression loop detected")
}

// Parses HTTPS/SVCB RDATA into the fields mlai-trade validates.
fn parse_https_rdata(
    buf: &[u8],
    start: usize,
    end: usize,
    ttl: u32,
) -> anyhow::Result<HttpsDnsAnswer> {
    if start + 3 > end {
        anyhow::bail!("HTTPS RDATA truncated");
    }
    let priority = u16::from_be_bytes([buf[start], buf[start + 1]]);
    let (target, mut pos) = parse_dns_name(buf, start + 2)?;
    if pos > end {
        anyhow::bail!("HTTPS target name extends past RDATA");
    }
    let mut alpn = Vec::new();
    let mut port = None;
    while pos + 4 <= end {
        let key = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
        let len = u16::from_be_bytes([buf[pos + 2], buf[pos + 3]]) as usize;
        pos += 4;
        let value_end = pos + len;
        if value_end > end {
            anyhow::bail!("HTTPS SvcParam value truncated");
        }
        let value = &buf[pos..value_end];
        match key {
            1 => {
                let mut offset = 0;
                while offset < value.len() {
                    let size = value[offset] as usize;
                    offset += 1;
                    if offset + size > value.len() {
                        anyhow::bail!("HTTPS alpn SvcParam truncated");
                    }
                    alpn.push(String::from_utf8_lossy(&value[offset..offset + size]).to_string());
                    offset += size;
                }
            }
            3 if value.len() == 2 => {
                port = Some(u16::from_be_bytes([value[0], value[1]]));
            }
            _ => {}
        }
        pos = value_end;
    }
    Ok(HttpsDnsAnswer {
        priority,
        target,
        alpn,
        port,
        ttl,
    })
}

// Queries DNS HTTPS records and verifies the public H3 discovery policy.
fn check_https_dns_record(domain: &str, required_port: u16) -> HttpsDnsCheck {
    let mut errors = Vec::new();
    let mut answers = Vec::new();
    let resolver = dns_resolvers()
        .into_iter()
        .next()
        .unwrap_or_else(|| "1.1.1.1".to_string());
    let result = (|| -> anyhow::Result<()> {
        let resolver_addr = resolver_socket_addr(&resolver)?;
        let mut query = Vec::new();
        let id = (Utc::now().timestamp_micros() as u16).to_be_bytes();
        query.extend_from_slice(&id);
        query.extend_from_slice(&0x0100_u16.to_be_bytes());
        query.extend_from_slice(&1_u16.to_be_bytes());
        query.extend_from_slice(&0_u16.to_be_bytes());
        query.extend_from_slice(&0_u16.to_be_bytes());
        query.extend_from_slice(&0_u16.to_be_bytes());
        query.extend_from_slice(&encode_dns_name(domain)?);
        query.extend_from_slice(&65_u16.to_be_bytes());
        query.extend_from_slice(&1_u16.to_be_bytes());

        let bind_addr = if resolver_addr.is_ipv6() {
            "[::]:0"
        } else {
            "0.0.0.0:0"
        };
        let socket = UdpSocket::bind(bind_addr)?;
        socket.set_read_timeout(Some(Duration::from_secs(5)))?;
        socket.set_write_timeout(Some(Duration::from_secs(5)))?;
        socket.send_to(&query, resolver_addr)?;
        let mut buf = [0_u8; 4096];
        let (len, _) = socket.recv_from(&mut buf)?;
        let buf = &buf[..len];
        if buf.len() < 12 {
            anyhow::bail!("DNS response header truncated");
        }
        let qdcount = u16::from_be_bytes([buf[4], buf[5]]) as usize;
        let ancount = u16::from_be_bytes([buf[6], buf[7]]) as usize;
        let mut pos = 12;
        for _ in 0..qdcount {
            let (_, next) = parse_dns_name(buf, pos)?;
            pos = next + 4;
            if pos > buf.len() {
                anyhow::bail!("DNS question truncated");
            }
        }
        for _ in 0..ancount {
            let (_, next) = parse_dns_name(buf, pos)?;
            pos = next;
            if pos + 10 > buf.len() {
                anyhow::bail!("DNS answer truncated");
            }
            let rr_type = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
            let rr_class = u16::from_be_bytes([buf[pos + 2], buf[pos + 3]]);
            let ttl = u32::from_be_bytes([buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]]);
            let rdlen = u16::from_be_bytes([buf[pos + 8], buf[pos + 9]]) as usize;
            pos += 10;
            let rdata_end = pos + rdlen;
            if rdata_end > buf.len() {
                anyhow::bail!("DNS RDATA truncated");
            }
            if rr_type == 65 && rr_class == 1 {
                answers.push(parse_https_rdata(buf, pos, rdata_end, ttl)?);
            }
            pos = rdata_end;
        }
        Ok(())
    })();
    if let Err(err) = result {
        errors.push(err.to_string());
    }
    let ok = answers.iter().any(|answer| {
        let port_ok = answer.port.unwrap_or(443) == required_port;
        let h3 = answer.alpn.iter().any(|value| value == "h3");
        let tcp_fallback = answer
            .alpn
            .iter()
            .any(|value| matches!(value.as_str(), "h2" | "http/1.1"));
        port_ok && h3 && !tcp_fallback
    });
    HttpsDnsCheck {
        ok,
        domain: domain.to_string(),
        resolver,
        required_alpn: "h3".to_string(),
        required_port,
        answers,
        errors,
    }
}

// Serializes remote API status as JSON.
fn ssl_status_json(status: &config::ApiSslRuntimeConfig) -> Value {
    let cert_exists = status.cert_file.exists();
    let key_exists = status.key_file.exists();
    let acme_cert_exists = status.acme_challenge_cert_file.exists();
    let acme_key_exists = status.acme_challenge_key_file.exists();
    json!({
        "api_enabled": status.api_enabled,
        "enabled": status.enabled,
        "running": read_pid(&status.pid_file).map(process_alive).unwrap_or(false),
        "pid": read_pid(&status.pid_file),
        "transport": "http3_quic",
        "listen": {
            "host": status.bind_host.clone(),
            "udp_port": status.udp_port,
            "normal_tcp_https": false,
        },
        "auth": {
            "enabled_for_non_localhost": status.auth_enabled,
            "username_configured": !status.auth_username.is_empty(),
            "password_configured": !status.auth_password.is_empty() && status.auth_password != "replace_me",
            "localhost_bypass": true,
        },
        "tls": {
            "version": "TLS1.3",
            "alpn": ["h3"],
            "key_exchange_policy": status.key_exchange_policy.clone(),
            "mlkem_required": status.key_exchange_policy == "mlkem_required",
            "fallback_to_classical": false,
        },
        "certificate": {
            "mode": status.cert_mode.clone(),
            "domain": status.domain.clone(),
            "cert_file": status.cert_file.display().to_string(),
            "key_file": status.key_file.display().to_string(),
            "cert_exists": cert_exists,
            "key_exists": key_exists,
        },
        "acme_tls_alpn_01_challenge_certificate": {
            "cert_file": status.acme_challenge_cert_file.display().to_string(),
            "key_file": status.acme_challenge_key_file.display().to_string(),
            "cert_exists": acme_cert_exists,
            "key_exists": acme_key_exists,
            "rfc": "RFC8737",
            "alpn": "acme-tls/1",
            "requires_runtime_key_authorization": true,
        },
        "letsencrypt_tls_alpn_01": {
            "enabled": status.cert_mode == "letsencrypt" && status.tcp_acme_tls_alpn_enabled,
            "host": status.tcp_acme_bind_host.clone(),
            "tcp_port": status.tcp_acme_port,
            "api_routes_allowed": false,
        },
        "dns_https_check_required": status.dns_https_check_required,
        "pid_file": status.pid_file.display().to_string(),
        "log_file": status.log_file.display().to_string(),
        "implementation_status": "implemented_http3_quic_listener",
    })
}

// Writes a remote API JSON log event to the SSL/H3 log file.
fn api_ssl_log(mut event: Value) {
    if let Some(object) = event.as_object_mut() {
        object
            .entry("ts".to_string())
            .or_insert_with(|| json!(Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()));
        object
            .entry("component".to_string())
            .or_insert_with(|| json!("api_ssl"));
    }
    let line = serde_json::to_string(&event).unwrap_or_else(|err| {
        json!({
            "ts": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "component": "api_ssl",
            "event": "log_serialization_failed",
            "level": "error",
            "error": err.to_string(),
        })
        .to_string()
    });
    let status = config::api_ssl_runtime_config();
    let write_result = paths::open_private_append(&status.log_file).and_then(|mut file| {
        writeln!(file, "{line}")?;
        file.flush()
    });
    if let Err(err) = write_result {
        eprintln!(
            "{}",
            json!({
                "ts": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                "component": "api_ssl",
                "event": "log_write_failed",
                "level": "error",
                "error": err.to_string(),
            })
        );
    }
}

// Returns the configured hostnames used for local self-signed cert generation.
fn ssl_cert_names(
    status: &config::ApiSslRuntimeConfig,
    domain: Option<String>,
    sans: Vec<String>,
) -> (String, Vec<String>) {
    let primary = domain
        .filter(|value| !value.trim().is_empty())
        .or_else(|| (!status.domain.trim().is_empty()).then_some(status.domain.clone()))
        .unwrap_or_else(|| "localhost".to_string());
    let mut names = vec![
        primary.clone(),
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];
    for san in sans {
        let san = san.trim();
        if !san.is_empty() {
            names.push(san.to_string());
        }
    }
    names.sort();
    names.dedup();
    (primary, names)
}

// Returns a hex string for display and logging.
fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

// Writes a PEM certificate/key pair using the runtime private permissions.
fn write_cert_pair(
    cert_path: &PathBuf,
    key_path: &PathBuf,
    cert_pem: &str,
    key_pem: &str,
) -> anyhow::Result<()> {
    paths::write_private_file(cert_path, cert_pem)?;
    paths::write_private_file(key_path, key_pem)?;
    Ok(())
}

// Builds the RFC 8737 ACME TLS-ALPN-01 challenge certificate.
fn acme_challenge_cert(
    domain: &str,
    acme_key_authorization: Option<&str>,
) -> anyhow::Result<(String, String, String, bool)> {
    if domain.parse::<IpAddr>().is_ok() {
        anyhow::bail!("TLS-ALPN-01 requires a DNS hostname, not an IP address: {domain}");
    }
    let digest = match acme_key_authorization {
        Some(value) if !value.trim().is_empty() => Sha256::digest(value.trim().as_bytes()).to_vec(),
        _ => Sha256::digest(format!("mlai-trade-placeholder:{domain}").as_bytes()).to_vec(),
    };
    let real_challenge = acme_key_authorization
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let signing_key = rcgen::KeyPair::generate()?;
    let mut params = rcgen::CertificateParams::new(vec![domain.to_string()])?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, domain.to_string());
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::minutes(5);
    params.not_after = now + time::Duration::days(7);
    params
        .custom_extensions
        .push(rcgen::CustomExtension::new_acme_identifier(&digest));
    let cert = params.self_signed(&signing_key)?;
    Ok((
        cert.pem(),
        signing_key.serialize_pem(),
        hex_bytes(&digest),
        real_challenge,
    ))
}

// Generates the remote API identity cert plus the RFC 8737 challenge cert.
pub fn cmd_ssl_cert_generate(
    domain: Option<String>,
    sans: Vec<String>,
    days: u32,
    acme_key_authorization: Option<String>,
    force: bool,
    json_out: bool,
) -> anyhow::Result<()> {
    let status = config::api_ssl_runtime_config();
    if !force && (status.cert_file.exists() || status.key_file.exists()) {
        anyhow::bail!(
            "certificate files already exist; use `mlai-trade api ssl cert renew` or `--force` to overwrite"
        );
    }
    generate_ssl_certs(status, domain, sans, days, acme_key_authorization, json_out)
}

// Renews the remote API identity cert plus the RFC 8737 challenge cert.
pub fn cmd_ssl_cert_renew(
    domain: Option<String>,
    sans: Vec<String>,
    days: u32,
    acme_key_authorization: Option<String>,
    json_out: bool,
) -> anyhow::Result<()> {
    generate_ssl_certs(
        config::api_ssl_runtime_config(),
        domain,
        sans,
        days,
        acme_key_authorization,
        json_out,
    )
}

// Handles shared certificate generation logic.
fn generate_ssl_certs(
    status: config::ApiSslRuntimeConfig,
    domain: Option<String>,
    sans: Vec<String>,
    days: u32,
    acme_key_authorization: Option<String>,
    json_out: bool,
) -> anyhow::Result<()> {
    let (primary, names) = ssl_cert_names(&status, domain, sans);
    let mut params = rcgen::CertificateParams::new(names.clone())?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, primary.clone());
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::minutes(5);
    params.not_after = now + time::Duration::days(days.max(1).into());
    let signing_key = rcgen::KeyPair::generate()?;
    let cert = params.self_signed(&signing_key)?;
    write_cert_pair(
        &status.cert_file,
        &status.key_file,
        &cert.pem(),
        &signing_key.serialize_pem(),
    )?;

    let (acme_cert, acme_key, acme_digest, acme_ready) =
        acme_challenge_cert(&primary, acme_key_authorization.as_deref())?;
    write_cert_pair(
        &status.acme_challenge_cert_file,
        &status.acme_challenge_key_file,
        &acme_cert,
        &acme_key,
    )?;
    let payload = json!({
        "ok": true,
        "certificate": {
            "cert_file": status.cert_file.display().to_string(),
            "key_file": status.key_file.display().to_string(),
            "subject_alt_names": names,
            "valid_days": days.max(1),
            "purpose": "http3_h3_identity",
        },
        "acme_tls_alpn_01_challenge_certificate": {
            "cert_file": status.acme_challenge_cert_file.display().to_string(),
            "key_file": status.acme_challenge_key_file.display().to_string(),
            "domain": primary,
            "rfc": "RFC8737",
            "alpn": "acme-tls/1",
            "acme_identifier_sha256": acme_digest,
            "ready_for_real_acme_validation": acme_ready,
            "note": if acme_ready {
                "challenge cert contains the supplied key authorization digest"
            } else {
                "placeholder challenge cert generated; pass --acme-key-authorization to generate a certificate for a live RFC 8737 authorization"
            },
        },
    });
    api_ssl_log(json!({
        "event": "api_ssl_cert_generated",
        "level": "info",
        "cert_file": status.cert_file.display().to_string(),
        "key_file": status.key_file.display().to_string(),
        "acme_challenge_cert_file": status.acme_challenge_cert_file.display().to_string(),
        "acme_ready": acme_ready,
    }));
    if json_out {
        print_json(payload)?;
    } else {
        println!("API SSL certificates generated");
        println!("  H3 cert:      {}", status.cert_file.display());
        println!("  H3 key:       {}", status.key_file.display());
        println!(
            "  ACME cert:    {}",
            status.acme_challenge_cert_file.display()
        );
        println!(
            "  ACME key:     {}",
            status.acme_challenge_key_file.display()
        );
        println!("  Domain:       {}", primary);
        println!("  H3 SANs:      {:?}", names);
        println!(
            "  ACME status:  {}",
            if acme_ready {
                "ready for current RFC 8737 key authorization"
            } else {
                "placeholder; real ACME renewal regenerates this automatically"
            }
        );
    }
    Ok(())
}

// Enables or disables the remote SSL/H3 API in the JSON config.
pub fn cmd_ssl_set_enabled(enabled: bool, json_out: bool) -> anyhow::Result<()> {
    let path = config::config_path();
    let raw = fs::read_to_string(&path)?;
    let mut value: Value = serde_json::from_str(&raw)?;
    value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("config root must be a JSON object"))?
        .entry("api".to_string())
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("$.api must be a JSON object"))?
        .entry("ssl".to_string())
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("$.api.ssl must be a JSON object"))?
        .insert("enabled".to_string(), json!(enabled));
    paths::write_private_file(&path, serde_json::to_string_pretty(&value)?)?;
    let payload =
        json!({"ok": true, "api_ssl_enabled": enabled, "config_file": path.display().to_string()});
    if json_out {
        print_json(payload)?;
    } else {
        println!(
            "API SSL/H3 {} in {}",
            if enabled { "enabled" } else { "disabled" },
            path.display()
        );
    }
    Ok(())
}

// Returns the remote SSL/H3 runtime status tuple.
fn ssl_running(status: &config::ApiSslRuntimeConfig) -> (bool, Option<u32>) {
    let pid = read_pid(&status.pid_file);
    let running = pid.map(process_alive).unwrap_or(false);
    (running, if running { pid } else { None })
}

// Loads a PEM certificate chain and private key for Rustls.
fn load_rustls_cert_key(
    cert_file: &PathBuf,
    key_file: &PathBuf,
) -> anyhow::Result<(
    Vec<rustls::pki_types::CertificateDer<'static>>,
    rustls::pki_types::PrivateKeyDer<'static>,
)> {
    let cert_reader = fs::File::open(cert_file)?;
    let mut cert_reader = BufReader::new(cert_reader);
    let certs = rustls_pemfile::certs(&mut cert_reader).collect::<Result<Vec<_>, _>>()?;
    if certs.is_empty() {
        anyhow::bail!(
            "certificate file has no certificates: {}",
            cert_file.display()
        );
    }
    let key_reader = fs::File::open(key_file)?;
    let mut key_reader = BufReader::new(key_reader);
    let key = rustls_pemfile::private_key(&mut key_reader)?.ok_or_else(|| {
        anyhow::anyhow!(
            "private key file has no supported key: {}",
            key_file.display()
        )
    })?;
    Ok((certs, key))
}

// Builds a Rustls server config with only ML-KEM key exchange and H3 ALPN.
fn build_mlkem_h3_server_config(
    certs: Vec<rustls::pki_types::CertificateDer<'static>>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
) -> anyhow::Result<rustls::ServerConfig> {
    let mut provider = rustls::crypto::aws_lc_rs::default_provider();
    provider.kx_groups = vec![rustls::crypto::aws_lc_rs::kx_group::MLKEM768];
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(provider))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    config.alpn_protocols = vec![b"h3".to_vec()];
    Ok(config)
}

// Adds remote API/webapp security headers to H3 responses.
fn add_h3_security_headers(mut builder: http::response::Builder) -> http::response::Builder {
    let status = config::api_ssl_runtime_config();
    let alt_svc = format!("h3=\":{}\"; ma=86400", status.udp_port);
    builder = builder.header("alt-svc", alt_svc);
    builder = builder.header("strict-transport-security", "max-age=31536000");
    builder = builder.header("x-content-type-options", "nosniff");
    builder = builder.header("x-frame-options", "DENY");
    builder = builder.header("referrer-policy", "no-referrer");
    builder = builder.header("x-robots-tag", "noindex, nofollow, noai, noimageai");
    builder = builder.header(
        "permissions-policy",
        "geolocation=(), microphone=(), camera=(), payment=()",
    );
    builder.header(
        "content-security-policy",
        "default-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
    )
}

// Returns true when the remote SSL listener can accept non-loopback clients.
fn ssl_bind_allows_non_loopback(bind_host: &str) -> bool {
    bind_host
        .to_socket_addrs()
        .map(|mut addrs| addrs.any(|addr| !addr.ip().is_loopback()))
        .unwrap_or(true)
}

// Rejects unsafe remote auth combinations before opening UDP to the network.
fn validate_ssl_remote_auth(status: &config::ApiSslRuntimeConfig) -> anyhow::Result<()> {
    if !ssl_bind_allows_non_loopback(&format!("{}:{}", status.bind_host, status.udp_port)) {
        return Ok(());
    }
    if !status.auth_enabled {
        anyhow::bail!(
            "refusing to start remote API on non-loopback bind without api.ssl.auth.enabled=true"
        );
    }
    if status.auth_username.trim().is_empty()
        || status.auth_password.trim().is_empty()
        || status.auth_password == "replace_me"
    {
        anyhow::bail!(
            "refusing to start remote API on non-loopback bind with missing/default api.ssl.auth credentials"
        );
    }
    Ok(())
}

// Enforces HTTPS/SVCB discovery for public H3 domains when requested.
fn validate_ssl_dns_for_start(status: &config::ApiSslRuntimeConfig) -> anyhow::Result<()> {
    if !status.dns_https_check_required {
        return Ok(());
    }
    let domain = status.domain.trim();
    if domain.is_empty()
        || domain.eq_ignore_ascii_case("localhost")
        || domain.parse::<std::net::IpAddr>().is_ok()
    {
        return Ok(());
    }
    let check = check_https_dns_record(domain, status.udp_port);
    if check.ok {
        return Ok(());
    }
    anyhow::bail!(
        "remote API DNS HTTPS/SVCB check failed for {}:{}; run `mlai-trade api ssl dns-check {}` or set api.ssl.dns_https_check_required=false for private testing",
        domain,
        status.udp_port,
        domain
    )
}

// Starts the remote SSL/H3 API listener.
pub fn cmd_ssl_start(json_out: bool) -> anyhow::Result<()> {
    paths::ensure_runtime_dirs()?;
    let status = config::api_ssl_runtime_config();
    if !status.api_enabled {
        anyhow::bail!(
            "cannot run API SSL/H3: api.enabled=false in {}",
            config::config_path().display()
        );
    }
    if !status.enabled {
        anyhow::bail!(
            "cannot run API SSL/H3: api.ssl.enabled=false in {}",
            config::config_path().display()
        );
    }
    let (running, pid) = ssl_running(&status);
    if running {
        if json_out {
            print_json(
                json!({"status": "already_running", "pid": pid, "udp_port": status.udp_port}),
            )?;
        } else {
            println!("API SSL/H3 already running with pid {}.", pid.unwrap_or(0));
        }
        return Ok(());
    }
    if !status.cert_file.exists() || !status.key_file.exists() {
        anyhow::bail!(
            "API SSL/H3 certificate files are missing. Run `mlai-trade api ssl cert generate` first."
        );
    }
    validate_ssl_remote_auth(&status)?;
    validate_ssl_dns_for_start(&status)?;
    if let Some(parent) = status.log_file.parent() {
        paths::ensure_private_dir(parent)?;
    }
    let stdout = paths::open_private_append(&status.log_file)?;
    let stderr = stdout.try_clone()?;
    let exe = std::env::current_exe()?;
    let mut command = Command::new(exe);
    command
        .arg("--home")
        .arg(paths::root_dir())
        .arg("api-ssl-run")
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
            "udp_port": status.udp_port,
            "log_file": status.log_file.display().to_string(),
        }))?;
    } else {
        println!("API SSL/H3 started with pid {}.", child.id());
        println!("UDP: {}:{}", status.bind_host, status.udp_port);
        println!("Log file: {}", status.log_file.display());
    }
    Ok(())
}

// Stops the remote SSL/H3 API listener.
pub fn cmd_ssl_stop(json_out: bool) -> anyhow::Result<()> {
    let status = config::api_ssl_runtime_config();
    let Some(pid) = read_pid(&status.pid_file) else {
        if json_out {
            print_json(json!({"status": "not_running"}))?;
        } else {
            println!("API SSL/H3 is not running.");
        }
        return Ok(());
    };
    if !process_alive(pid) {
        let _ = fs::remove_file(&status.pid_file);
        if json_out {
            print_json(json!({"status": "stale_pid_removed", "pid": pid}))?;
        } else {
            println!("Removed stale API SSL/H3 pid file for pid {}.", pid);
        }
        return Ok(());
    }
    unsafe {
        if libc::kill(pid as libc::pid_t, libc::SIGTERM) != 0 {
            anyhow::bail!(
                "unable to stop API SSL/H3 pid {}: {}",
                pid,
                std::io::Error::last_os_error()
            );
        }
    }
    for _ in 0..50 {
        if !process_alive(pid) {
            let _ = fs::remove_file(&status.pid_file);
            if json_out {
                print_json(json!({"status": "stopped", "pid": pid}))?;
            } else {
                println!("API SSL/H3 stopped.");
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!("API SSL/H3 pid {} did not stop within timeout", pid)
}

// Restarts the remote SSL/H3 API listener.
pub fn cmd_ssl_restart(json_out: bool) -> anyhow::Result<()> {
    let _ = cmd_ssl_stop(json_out);
    cmd_ssl_start(json_out)
}

// Reloads the remote SSL/H3 API listener.
pub fn cmd_ssl_reload(json_out: bool) -> anyhow::Result<()> {
    let status = config::api_ssl_runtime_config();
    let Some(pid) = read_pid(&status.pid_file) else {
        anyhow::bail!("API SSL/H3 is not running. Start it with `mlai-trade api ssl start`.");
    };
    unsafe {
        if libc::kill(pid as libc::pid_t, libc::SIGHUP) != 0 {
            anyhow::bail!(
                "unable to reload API SSL/H3 pid {}: {}",
                pid,
                std::io::Error::last_os_error()
            );
        }
    }
    if json_out {
        print_json(json!({"status": "reload_sent", "pid": pid}))?;
    } else {
        println!("API SSL/H3 reload signal sent to pid {}.", pid);
    }
    Ok(())
}

// Serializes HTTPS DNS check output as JSON.
fn https_dns_check_json(check: &HttpsDnsCheck) -> Value {
    json!({
        "ok": check.ok,
        "domain": check.domain.clone(),
        "resolver": check.resolver.clone(),
        "required": {
            "alpn": check.required_alpn.clone(),
            "port": check.required_port,
            "disallow_tcp_http_alpn": ["h2", "http/1.1"],
        },
        "answers": check.answers.iter().map(|answer| json!({
            "priority": answer.priority,
            "target": answer.target.clone(),
            "alpn": answer.alpn.clone(),
            "port": answer.port.unwrap_or(443),
            "ttl": answer.ttl,
        })).collect::<Vec<_>>(),
        "errors": check.errors.clone(),
    })
}

// Shows configured remote HTTP/3 API status.
pub fn cmd_ssl_status(json_out: bool) -> anyhow::Result<()> {
    let status = config::api_ssl_runtime_config();
    let payload = ssl_status_json(&status);
    if json_out {
        print_json(payload)?;
        return Ok(());
    }
    println!("API SSL/H3 Remote Status");
    println!("  API enabled:  {}", status.api_enabled);
    println!("  Enabled:      {}", status.enabled);
    println!(
        "  Running:      {}",
        read_pid(&status.pid_file)
            .map(process_alive)
            .unwrap_or(false)
    );
    println!("  Transport:    HTTP/3 over QUIC, UDP {}", status.udp_port);
    println!("  Bind host:    {}", status.bind_host);
    println!(
        "  Domain:       {}",
        if status.domain.is_empty() {
            "not configured"
        } else {
            &status.domain
        }
    );
    println!("  TLS:          TLS 1.3 only, ALPN h3 only");
    println!(
        "  Key exchange: {} (classical fallback disabled)",
        status.key_exchange_policy
    );
    println!("  Certificate:  {}", status.cert_mode);
    println!(
        "  Cert file:    {} ({})",
        status.cert_file.display(),
        if status.cert_file.exists() {
            "exists"
        } else {
            "missing"
        }
    );
    println!(
        "  Key file:     {} ({})",
        status.key_file.display(),
        if status.key_file.exists() {
            "exists"
        } else {
            "missing"
        }
    );
    println!(
        "  ACME cert:    {} ({})",
        status.acme_challenge_cert_file.display(),
        if status.acme_challenge_cert_file.exists() {
            "exists"
        } else {
            "missing"
        }
    );
    println!(
        "  ACME key:     {} ({})",
        status.acme_challenge_key_file.display(),
        if status.acme_challenge_key_file.exists() {
            "exists"
        } else {
            "missing"
        }
    );
    println!(
        "  Auth:         {} for non-localhost clients; localhost bypass enabled",
        if status.auth_enabled {
            "required"
        } else {
            "disabled"
        }
    );
    println!("  PID file:     {}", status.pid_file.display());
    println!("  Log file:     {}", status.log_file.display());
    if status.cert_mode == "letsencrypt" && status.tcp_acme_tls_alpn_enabled {
        println!(
            "  TCP challenge: ACME TLS-ALPN-01 challenge only on {}:{}",
            status.tcp_acme_bind_host, status.tcp_acme_port
        );
    } else {
        println!("  TCP listener: disabled for normal HTTPS/API traffic");
    }
    println!("  Data plane:   HTTP/3 over QUIC listener implemented");
    Ok(())
}

// Checks DNS HTTPS/SVCB discovery for the remote H3 API.
pub fn cmd_ssl_dns_check(domain: Option<String>, json_out: bool) -> anyhow::Result<()> {
    let configured = config::api_ssl_runtime_config();
    let domain = domain
        .or_else(|| (!configured.domain.trim().is_empty()).then_some(configured.domain))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "remote API domain is not configured. Set api.ssl.domain or pass `mlai-trade api ssl dns-check DOMAIN`."
            )
        })?;
    let check = check_https_dns_record(&domain, configured.udp_port);
    let payload = https_dns_check_json(&check);
    if json_out {
        print_json(payload)?;
        return Ok(());
    }
    println!("API SSL/H3 DNS HTTPS Check");
    println!("  Domain:   {}", check.domain);
    println!("  Resolver: {}", check.resolver);
    println!(
        "  Required: HTTPS/SVCB alpn=h3, port={}, no h2/http/1.1 fallback",
        check.required_port
    );
    if check.answers.is_empty() {
        println!("  Answers:  none");
    } else {
        for answer in &check.answers {
            println!(
                "  Answer:   priority={} target={} alpn={:?} port={} ttl={}",
                answer.priority,
                answer.target,
                answer.alpn,
                answer.port.unwrap_or(443),
                answer.ttl
            );
        }
    }
    if !check.errors.is_empty() {
        println!("  Errors:   {}", check.errors.join("; "));
    }
    println!("  Result:   {}", if check.ok { "ok" } else { "not ready" });
    if !check.ok {
        println!(
            "  Fix:      publish an HTTPS/SVCB record advertising only ALPN h3 on port {}",
            check.required_port
        );
    }
    Ok(())
}

// Handles status logic.
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
        limits: config::api_limit_config(),
    }
}

// Returns a compact remote SSL/H3 status for top-level status output.
pub fn ssl_status() -> ApiSslStatus {
    let status = config::api_ssl_runtime_config();
    let pid = read_pid(&status.pid_file);
    let running = pid.map(process_alive).unwrap_or(false);
    ApiSslStatus {
        api_enabled: status.api_enabled,
        enabled: status.enabled,
        running,
        pid: if running { pid } else { None },
        bind_host: status.bind_host,
        udp_port: status.udp_port,
        auth_enabled: status.auth_enabled,
    }
}

// Removes stale runtime files from local state.
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

// Handles the start CLI action.
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
        paths::ensure_private_dir(parent)?;
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
    let stdout = paths::open_private_append(&status.log_file)?;
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

// Handles the stop CLI action.
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

// Handles the reload CLI action.
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

// Handles the restart CLI action.
pub fn cmd_restart(json_out: bool) -> anyhow::Result<()> {
    let _ = cmd_stop(false);
    cmd_start(json_out)
}

// Fetches the API process health snapshot over its Unix socket.
fn fetch_health_snapshot(socket_file: &PathBuf) -> Option<Value> {
    let mut stream = StdUnixStream::connect(socket_file).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: mlai-trade\r\nConnection: close\r\n\r\n")
        .ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    let (_, body) = response.split_once("\r\n\r\n")?;
    serde_json::from_str(body.trim()).ok()
}

// Formats optional JSON metrics for human-readable status output.
fn metric_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(value)) => value.to_string(),
        Some(other) => other.to_string(),
        None => "not available".to_string(),
    }
}

// Formats byte metrics as MiB when available.
fn bytes_mib_text(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_u64)
        .map(|bytes| format!("{:.2} MiB", bytes as f64 / 1_048_576.0))
        .unwrap_or_else(|| "not available".to_string())
}

// Formats raw bytes as GiB for resource budgets.
fn bytes_gib_text(bytes: u64) -> String {
    format!("{:.2} GiB", bytes as f64 / 1_073_741_824.0)
}

// Formats seconds in a compact human-readable form.
fn seconds_text(value: Option<&Value>) -> String {
    let Some(seconds) = value.and_then(Value::as_f64) else {
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

// Formats floating-point metrics with a stable number of decimals.
fn number_text(value: Option<&Value>, decimals: usize) -> String {
    value
        .and_then(Value::as_f64)
        .map(|number| format!("{number:.decimals$}"))
        .unwrap_or_else(|| metric_text(value))
}

// Prints live API runtime counters and resource usage.
fn print_api_details(health: Option<&Value>) {
    let Some(runtime) = health.and_then(|value| value.get("runtime")) else {
        println!("  Runtime:     not available");
        return;
    };
    println!("  Runtime:");
    println!(
        "    Uptime:    {}",
        seconds_text(runtime.get("uptime_seconds"))
    );
    println!(
        "    Requests:  total={}, active={}, long-running={}, rejected={}, average={}/s",
        metric_text(runtime.get("total_requests")),
        metric_text(runtime.get("active_requests")),
        metric_text(runtime.get("active_long_requests")),
        metric_text(runtime.get("rejected_requests")),
        number_text(runtime.get("average_requests_per_second"), 3),
    );
    if let Some(resources) = runtime.get("resources") {
        println!(
            "    CPU:       avg process={}%, avg machine={}%, CPU time={}, capacity={}%",
            number_text(resources.get("avg_cpu_percent_since_start"), 2),
            number_text(resources.get("avg_machine_cpu_percent_since_start"), 2),
            seconds_text(resources.get("total_cpu_seconds")),
            metric_text(resources.get("total_cpu_capacity_percent")),
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
            bytes_mib_text(resources.get("current_rss_bytes")),
            bytes_mib_text(resources.get("peak_rss_bytes")),
        );
        println!(
            "    Memory cap: budget={} ({}% of {}, source={})",
            bytes_gib_text(configured.memory_budget_bytes),
            configured.memory_budget_percent,
            bytes_gib_text(configured.memory_total_bytes),
            configured.memory_source,
        );
        println!(
            "    Process:   open files/sockets={}, OS threads={}",
            metric_text(resources.get("open_file_descriptor_count")),
            metric_text(resources.get("os_thread_count")),
        );
    }
}

// Handles the status CLI action.
pub fn cmd_status(json_out: bool, details: bool) -> anyhow::Result<()> {
    let status = status();
    let health = if details && status.running {
        fetch_health_snapshot(&status.socket_file)
    } else {
        None
    };
    if json_out {
        let mut payload = json!({
            "enabled": status.enabled,
            "running": status.running,
            "pid": status.pid,
            "socket_file": status.socket_file.display().to_string(),
            "pid_file": status.pid_file.display().to_string(),
            "log_file": status.log_file.display().to_string(),
            "request_timeout_seconds": status.request_timeout_seconds,
            "long_request_timeout_seconds": status.long_request_timeout_seconds,
            "limits": api_limits_json(&status.limits),
            "routes": route_specs(),
        });
        if details {
            payload["details"] = health.unwrap_or_else(|| json!("not available"));
            payload["configured_resources"] = config::runtime_resources_json();
            payload["accelerators"] = accelerators::accelerator_status_json();
        }
        println!("{}", serde_json::to_string_pretty(&payload)?);
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
    println!(
        "  Concurrency: {} total / {} long",
        status.limits.max_concurrent_requests, status.limits.max_concurrent_long_requests
    );
    println!(
        "  Rate limit:  {}/minute (backoff {}s)",
        status.limits.rate_limit_per_minute, status.limits.overload_retry_after_seconds
    );
    println!("  Max body:    {} bytes", status.limits.max_body_bytes);
    println!("  Routes:      GET/POST over Unix socket; run with --json to list route specs");
    if details {
        print_api_details(health.as_ref());
    }
    Ok(())
}

// Handles the test CLI action.
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

// Handles the run CLI action.
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
        paths::ensure_private_dir(parent)?;
    }
    if let Some(parent) = status.socket_file.parent() {
        paths::ensure_private_dir(parent)?;
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
    paths::write_runtime_metadata_file(&status.pid_file, std::process::id().to_string())?;
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
        "limits": api_limits_json(&config::api_limit_config()),
    }));

    let state = Arc::new(ApiRuntimeState::new());
    let app = Router::new()
        .route("/health", get(handle_health))
        .route("/routes", get(handle_routes))
        .route("/{section}/{action}", get(handle_two).post(handle_two))
        .route(
            "/{section}/{action}/{target}",
            get(handle_three).post(handle_three),
        )
        .with_state(state);

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

// Handles the remote SSL/H3 API run loop.
pub async fn cmd_ssl_run() -> anyhow::Result<()> {
    paths::ensure_runtime_dirs()?;
    let status = config::api_ssl_runtime_config();
    if !status.api_enabled {
        anyhow::bail!(
            "cannot run API SSL/H3: api.enabled=false in {}",
            config::config_path().display()
        );
    }
    if !status.enabled {
        anyhow::bail!(
            "cannot run API SSL/H3: api.ssl.enabled=false in {}",
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
    if let Some(parent) = status.pid_file.parent() {
        paths::ensure_private_dir(parent)?;
    }
    if let Some(parent) = status.log_file.parent() {
        paths::ensure_private_dir(parent)?;
    }
    logging::ensure_json_lines(&status.log_file, "api_ssl")?;
    logging::rotate_if_needed(&status.log_file)?;
    let (certs, key) = load_rustls_cert_key(&status.cert_file, &status.key_file)?;
    let rustls_config = build_mlkem_h3_server_config(certs, key)?;
    let quic_config = quinn::crypto::rustls::QuicServerConfig::try_from(rustls_config)?;
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_config));
    let bind_addr = format!("{}:{}", status.bind_host, status.udp_port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow::anyhow!("unable to resolve API SSL/H3 bind address"))?;
    let endpoint = quinn::Endpoint::server(server_config, bind_addr)?;
    let local_addr = endpoint.local_addr()?;
    paths::write_runtime_metadata_file(&status.pid_file, std::process::id().to_string())?;
    api_ssl_log(json!({
        "event": "api_ssl_server_started",
        "level": "info",
        "pid": std::process::id(),
        "bind": local_addr.to_string(),
        "transport": "http3_quic",
        "tls": {"version": "TLS1.3", "alpn": ["h3"], "key_exchange": "MLKEM768"},
        "cert_file": status.cert_file.display().to_string(),
        "key_file": status.key_file.display().to_string(),
    }));

    let state = Arc::new(ApiRuntimeState::new());
    loop {
        if TERMINATE.load(Ordering::SeqCst) || !config::api_ssl_runtime_config().enabled {
            break;
        }
        if RELOAD.swap(false, Ordering::SeqCst) {
            api_ssl_log(json!({
                "event": "api_ssl_reload_requested",
                "level": "info",
                "message": "restart required for certificate, bind, and TLS provider changes",
            }));
        }
        tokio::select! {
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break; };
                let state = state.clone();
                let dest_addr = local_addr;
                tokio::spawn(async move {
                    if let Err(err) = handle_h3_connection(state, incoming, dest_addr).await {
                        api_ssl_log(json!({
                            "event": "api_ssl_connection_failed",
                            "level": "error",
                            "error": err.to_string(),
                        }));
                    }
                });
            }
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
    }
    endpoint.close(0u32.into(), b"shutdown");
    let current_pid = std::process::id();
    if read_pid(&status.pid_file) == Some(current_pid) {
        let _ = fs::remove_file(&status.pid_file);
    }
    api_ssl_log(json!({
        "event": "api_ssl_server_stopped",
        "level": "info",
        "pid": current_pid,
    }));
    Ok(())
}

// Handles one H3 connection.
async fn handle_h3_connection(
    state: Arc<ApiRuntimeState>,
    incoming: quinn::Incoming,
    dest_addr: SocketAddr,
) -> anyhow::Result<()> {
    let connection = incoming.await?;
    let source_addr = connection.remote_address();
    let protocol = connection
        .handshake_data()
        .and_then(|data| data.downcast::<quinn::crypto::rustls::HandshakeData>().ok())
        .and_then(|data| data.protocol.clone())
        .map(|protocol| String::from_utf8_lossy(&protocol).to_string())
        .unwrap_or_else(|| "not available".to_string());
    api_ssl_log(json!({
        "event": "api_ssl_connection_started",
        "level": "info",
        "source_ip": source_addr.ip().to_string(),
        "source_port": source_addr.port(),
        "dest_ip": dest_addr.ip().to_string(),
        "dest_port": dest_addr.port(),
        "alpn": protocol,
    }));
    let mut h3_conn = h3::server::builder()
        .build(h3_quinn::Connection::new(connection))
        .await?;
    while let Some(resolver) = h3_conn.accept().await? {
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_h3_request(state, resolver, source_addr, dest_addr).await {
                api_ssl_log(json!({
                    "event": "api_ssl_request_failed",
                    "level": "error",
                    "source_ip": source_addr.ip().to_string(),
                    "source_port": source_addr.port(),
                    "dest_ip": dest_addr.ip().to_string(),
                    "dest_port": dest_addr.port(),
                    "error": err.to_string(),
                }));
            }
        });
    }
    Ok(())
}

// Handles one H3 request and bridges it into the same API command surface.
async fn handle_h3_request(
    state: Arc<ApiRuntimeState>,
    resolver: h3::server::RequestResolver<h3_quinn::Connection, bytes::Bytes>,
    source_addr: SocketAddr,
    dest_addr: SocketAddr,
) -> anyhow::Result<()> {
    use bytes::Buf;
    let started = Instant::now();
    let (request, mut stream) = resolver.resolve_request().await?;
    let method = request.method().clone();
    let uri = request.uri().clone();
    let path = uri.path().to_string();
    if method == Method::GET && path == "/robots.txt" {
        if let Some(response) = serve_webapp_asset(&path) {
            let status = response.status();
            let (parts, body) = response.into_parts();
            let response_body = to_bytes(body, 64 * 1024).await?;
            let mut builder = http::Response::builder().status(parts.status);
            for (name, value) in parts.headers {
                if let Some(name) = name {
                    builder = builder.header(name, value);
                }
            }
            builder = add_h3_security_headers(builder);
            stream.send_response(builder.body(())?).await?;
            if !response_body.is_empty() {
                stream.send_data(response_body).await?;
            }
            stream.finish().await?;
            api_ssl_log(json!({
                "event": "api_ssl_request",
                "level": "info",
                "method": method.as_str(),
                "path": path,
                "status": status.as_u16(),
                "duration_ms": started.elapsed().as_millis(),
                "source_ip": source_addr.ip().to_string(),
                "source_port": source_addr.port(),
                "dest_ip": dest_addr.ip().to_string(),
                "dest_port": dest_addr.port(),
                "auth": "not_required_for_robots",
            }));
            return Ok(());
        }
    }
    if let Err(response) = authorize_h3_request(&request, source_addr) {
        let status = response.status();
        let (parts, body) = response.into_parts();
        let response_body = to_bytes(body, 16 * 1024).await?;
        let mut builder = http::Response::builder().status(parts.status);
        for (name, value) in parts.headers {
            if let Some(name) = name {
                builder = builder.header(name, value);
            }
        }
        builder = add_h3_security_headers(builder);
        stream.send_response(builder.body(())?).await?;
        if !response_body.is_empty() {
            stream.send_data(response_body).await?;
        }
        stream.finish().await?;
        api_ssl_log(json!({
            "event": "api_ssl_request",
            "level": "warn",
            "method": method.as_str(),
            "path": path,
            "status": status.as_u16(),
            "duration_ms": started.elapsed().as_millis(),
            "source_ip": source_addr.ip().to_string(),
            "source_port": source_addr.port(),
            "dest_ip": dest_addr.ip().to_string(),
            "dest_port": dest_addr.port(),
            "error": "authentication required",
        }));
        return Ok(());
    }
    let limits = config::api_limit_config();
    let mut body = bytes::BytesMut::new();
    let mut body_too_large = false;
    while let Some(mut chunk) = stream.recv_data().await? {
        while chunk.has_remaining() {
            let bytes = chunk.copy_to_bytes(chunk.remaining());
            if body.len() + bytes.len() > limits.max_body_bytes {
                body_too_large = true;
                break;
            }
            body.extend_from_slice(&bytes);
        }
        if body_too_large || body.len() >= limits.max_body_bytes {
            break;
        }
    }
    let response = if body_too_large {
        api_error_logged(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request body too large",
            method.as_str(),
            &path,
            started,
            None,
        )
    } else {
        handle_remote_api_request(state, method.clone(), uri, body.freeze()).await
    };
    let status = response.status();
    let (parts, body) = response.into_parts();
    let response_body = to_bytes(body, limits.max_body_bytes.max(1024 * 1024)).await?;
    let mut builder = http::Response::builder().status(parts.status);
    for (name, value) in parts.headers {
        if let Some(name) = name {
            builder = builder.header(name, value);
        }
    }
    builder = add_h3_security_headers(builder);
    stream.send_response(builder.body(())?).await?;
    if !response_body.is_empty() {
        stream.send_data(response_body).await?;
    }
    stream.finish().await?;
    api_ssl_log(json!({
        "event": "api_ssl_request",
        "level": "info",
        "method": method.as_str(),
        "path": path,
        "status": status.as_u16(),
        "duration_ms": started.elapsed().as_millis(),
        "source_ip": source_addr.ip().to_string(),
        "source_port": source_addr.port(),
        "dest_ip": dest_addr.ip().to_string(),
        "dest_port": dest_addr.port(),
    }));
    Ok(())
}

// Enforces username/password for non-loopback H3 clients.
fn authorize_h3_request<B>(
    request: &http::Request<B>,
    source_addr: SocketAddr,
) -> Result<(), Response> {
    if source_addr.ip().is_loopback() {
        return Ok(());
    }
    let status = config::api_ssl_runtime_config();
    if !status.auth_enabled {
        return Ok(());
    }
    let Some(value) = request.headers().get(header::AUTHORIZATION) else {
        return Err((
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Basic realm=\"mlai-trade\"")],
            Json(json!({"ok": false, "error": "authentication required"})),
        )
            .into_response());
    };
    let Ok(value) = value.to_str() else {
        return Err(api_error(
            StatusCode::UNAUTHORIZED,
            "invalid authorization header",
        ));
    };
    let Some(encoded) = value.strip_prefix("Basic ") else {
        return Err(api_error(
            StatusCode::UNAUTHORIZED,
            "basic authentication required",
        ));
    };
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded.trim()) else {
        return Err(api_error(
            StatusCode::UNAUTHORIZED,
            "invalid basic authentication payload",
        ));
    };
    let Ok(decoded) = String::from_utf8(decoded) else {
        return Err(api_error(
            StatusCode::UNAUTHORIZED,
            "invalid basic authentication encoding",
        ));
    };
    let Some((username, password)) = decoded.split_once(':') else {
        return Err(api_error(
            StatusCode::UNAUTHORIZED,
            "invalid basic authentication format",
        ));
    };
    if constant_time_eq(username.as_bytes(), status.auth_username.as_bytes())
        & constant_time_eq(password.as_bytes(), status.auth_password.as_bytes())
    {
        return Ok(());
    }
    Err(api_error(
        StatusCode::UNAUTHORIZED,
        "invalid username or password",
    ))
}

// Compares secret values without short-circuiting on the first different byte.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut diff = left.len() ^ right.len();
    let max_len = left.len().max(right.len());
    for i in 0..max_len {
        let a = left.get(i).copied().unwrap_or(0);
        let b = right.get(i).copied().unwrap_or(0);
        diff |= (a ^ b) as usize;
    }
    diff == 0
}

// Handles a remote API request without relying on Axum extractors.
async fn handle_remote_api_request(
    state: Arc<ApiRuntimeState>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> Response {
    let started = Instant::now();
    let path = uri.path().to_string();
    let limits = config::api_limit_config();
    if path == "/health" {
        if let Err(response) =
            check_api_rate_limit(&state, &limits, method.as_str(), &path, started, None).await
        {
            return response;
        }
        return json_response(
            StatusCode::OK,
            json!({
                "ok": true,
                "service": "mlai-trade-api-ssl",
                "transport": "http3_quic",
                "runtime": state.runtime_json(),
            }),
        );
    }
    if path == "/routes" {
        if let Err(response) =
            check_api_rate_limit(&state, &limits, method.as_str(), &path, started, None).await
        {
            return response;
        }
        return json_response(StatusCode::OK, json!({"ok": true, "routes": route_specs()}));
    }
    if method == Method::GET {
        if let Some(response) = serve_webapp_asset(&path) {
            return response;
        }
    }
    let segments = path
        .trim_start_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let query = parse_query_map(uri.query().unwrap_or_default());
    match segments.as_slice() {
        [section, action] => {
            handle_allowed_command(
                state,
                method,
                uri,
                section.clone(),
                action.clone(),
                None,
                query,
                body,
            )
            .await
        }
        [section, action, target] => {
            handle_allowed_command(
                state,
                method,
                uri,
                section.clone(),
                action.clone(),
                Some(target.clone()),
                query,
                body,
            )
            .await
        }
        _ => api_error_logged(
            StatusCode::NOT_FOUND,
            "unknown API route",
            method.as_str(),
            &path,
            started,
            None,
        ),
    }
}

// Serves the built React webapp from runtime api/html/dist.
fn serve_webapp_asset(path: &str) -> Option<Response> {
    let (base, relative) = match path {
        "/robots.txt" => (paths::api_dir().join("html"), "robots.txt".to_string()),
        "/" | "/app" | "/app/" | "/index.html" => (
            paths::api_dir().join("html").join("dist"),
            "index.html".to_string(),
        ),
        _ => {
            let relative = path.strip_prefix("/assets/")?;
            (
                paths::api_dir().join("html").join("dist").join("assets"),
                relative.to_string(),
            )
        }
    };
    if relative.contains("..") || relative.starts_with('/') {
        return Some(api_error(StatusCode::BAD_REQUEST, "invalid webapp path"));
    }
    let file = base.join(relative);
    let bytes = match fs::read(&file) {
        Ok(bytes) => bytes,
        Err(_) => return None,
    };
    let content_type = match file.extension().and_then(|value| value.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("txt") => "text/plain; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    };
    Some(
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, content_type)],
            bytes,
        )
            .into_response(),
    )
}

// Parses a simple query string for the H3 API bridge.
fn parse_query_map(query: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let mut parts = pair.splitn(2, '=');
        let key = parts.next().unwrap_or_default();
        let value = parts.next().unwrap_or_default();
        out.insert(key.to_string(), value.to_string());
    }
    out
}

// Handles shutdown signal logic.
async fn shutdown_signal() {
    while !TERMINATE.load(Ordering::SeqCst) {
        if RELOAD.swap(false, Ordering::SeqCst) {
            api_log(json!({
                "event": "api_server_config_reloaded",
                "level": "info",
                "timeout_seconds": config::api_request_timeout_seconds(),
                "long_timeout_seconds": config::api_long_request_timeout_seconds(),
                "limits": api_limits_json(&config::api_limit_config()),
            }));
        }
        if let Err(err) = config::load() {
            api_log(json!({
                "event": "config_invalid",
                "level": "error",
                "config_file": config::config_path().display().to_string(),
                "error": err.to_string(),
                "message": "API keeps running, but CLI-backed requests will fail until configuration is fixed",
            }));
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
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

// Handles the health request or signal.
async fn handle_health(
    State(state): State<Arc<ApiRuntimeState>>,
    method: Method,
    uri: Uri,
) -> Response {
    let started = Instant::now();
    let limits = config::api_limit_config();
    if let Err(response) =
        check_api_rate_limit(&state, &limits, method.as_str(), uri.path(), started, None).await
    {
        return response;
    }
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
            "runtime": state.runtime_json(),
        }),
    )
}

// Handles the routes request or signal.
async fn handle_routes(
    State(state): State<Arc<ApiRuntimeState>>,
    method: Method,
    uri: Uri,
) -> Response {
    let started = Instant::now();
    let limits = config::api_limit_config();
    if let Err(response) =
        check_api_rate_limit(&state, &limits, method.as_str(), uri.path(), started, None).await
    {
        return response;
    }
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

// Handles the two request or signal.
async fn handle_two(
    State(state): State<Arc<ApiRuntimeState>>,
    method: Method,
    uri: Uri,
    Path((section, action)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    handle_allowed_command(state, method, uri, section, action, None, query, body).await
}

// Handles the three request or signal.
async fn handle_three(
    State(state): State<Arc<ApiRuntimeState>>,
    method: Method,
    uri: Uri,
    Path((section, action, target)): Path<(String, String, String)>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    handle_allowed_command(
        state,
        method,
        uri,
        section,
        action,
        Some(target),
        query,
        body,
    )
    .await
}

// Handles the allowed command request or signal.
async fn handle_allowed_command(
    state: Arc<ApiRuntimeState>,
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
    let limits = config::api_limit_config();
    if body.len() > limits.max_body_bytes {
        state.total_requests.fetch_add(1, Ordering::SeqCst);
        state.rejected_requests.fetch_add(1, Ordering::SeqCst);
        return api_error_logged(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "request body is {} bytes; api.max_body_bytes is {}",
                body.len(),
                limits.max_body_bytes
            ),
            &method,
            &path,
            started,
            None,
        );
    }
    if let Err(response) =
        check_api_rate_limit(&state, &limits, &method, &path, started, None).await
    {
        return response;
    }
    if let Some((status, payload)) = handle_direct_command(&section, &action) {
        let _guard = match acquire_api_request_guard(
            &state, &limits, false, &method, &path, started, None,
        ) {
            Ok(guard) => guard,
            Err(response) => return response,
        };
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
    let long_request = is_long_api_command(&args);
    let _guard = match acquire_api_request_guard(
        &state,
        &limits,
        long_request,
        &method,
        &path,
        started,
        Some(&args),
    ) {
        Ok(guard) => guard,
        Err(response) => return response,
    };
    run_cli(args, method, path, started).await
}

// Handles the direct command request or signal.
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
    // Constructs a new instance with the provided inputs.
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
    // Constructs a new instance with the provided inputs.
    fn new(query: HashMap<String, String>, body: Bytes) -> Result<Self, String> {
        let body = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body)
                .map_err(|err| format!("request body must be valid JSON: {err}"))?
        };
        Ok(Self { query, body })
    }

    // Handles value logic.
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

    // Handles bool value logic.
    fn bool_value(&self, keys: &[&str]) -> bool {
        self.value(keys)
            .map(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false)
    }

    // Handles list logic.
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

    // Handles body value logic.
    fn body_value(&self, key: &str) -> Option<String> {
        let obj = self.body.as_object()?;
        value_to_string(obj.get(key)?)
    }

    // Handles body list logic.
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

// Handles value to string logic.
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

// Handles split list logic.
fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

// Normalizes token into canonical form.
fn normalize_token(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('_', "-")
}

// Handles required value logic.
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

// Handles push option logic.
fn push_option(args: &mut Vec<String>, flag: &str, input: &RequestInput, keys: &[&str]) {
    if let Some(value) = input.value(keys) {
        args.push(flag.to_string());
        args.push(value);
    }
}

// Handles push bool logic.
fn push_bool(args: &mut Vec<String>, flag: &str, input: &RequestInput, keys: &[&str]) {
    if input.bool_value(keys) {
        args.push(flag.to_string());
    }
}

// Handles push accounts logic.
fn push_accounts(args: &mut Vec<String>, input: &RequestInput) {
    let accounts = input.list(&["account", "accounts"]);
    if !accounts.is_empty() {
        args.push("--account".to_string());
        args.push(accounts.join(","));
    }
}

// Pushes a required account selector for safety-sensitive API actions.
fn push_required_accounts(
    args: &mut Vec<String>,
    input: &RequestInput,
    action: &str,
) -> Result<(), ApiBuildError> {
    let accounts = input.list(&["account", "accounts"]);
    if accounts.is_empty() {
        return Err(ApiBuildError::new(
            StatusCode::BAD_REQUEST,
            format!("{action} requires account; pass account or accounts in JSON/query"),
        ));
    }
    args.push("--account".to_string());
    args.push(accounts.join(","));
    Ok(())
}

// Ensures auto trade off exists or meets required invariants.
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

// Builds cli args from configured inputs.
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
            push_bool(&mut args, "--sync", input, &["sync"]);
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
        ("auto", "track") => {
            let symbol = required_value(input, target, &["symbol"], "symbol")?;
            let mut args = vec!["auto".into(), "track".into(), symbol];
            push_required_accounts(&mut args, input, "auto track")?;
            Ok(args)
        }
        ("auto", "untrack") => {
            let symbol = required_value(input, target, &["symbol"], "symbol")?;
            let mut args = vec!["auto".into(), "untrack".into(), symbol];
            push_required_accounts(&mut args, input, "auto untrack")?;
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

// Handles run cli logic.
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

    let stdout =
        config::sanitize_logged_command_output(String::from_utf8_lossy(&output.stdout).trim());
    let stderr =
        config::sanitize_logged_command_output(String::from_utf8_lossy(&output.stderr).trim());
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

// Runs the api timeout for command API helper.
fn api_timeout_for_command(args: &[String]) -> u64 {
    if is_long_api_command(args) {
        config::api_long_request_timeout_seconds()
    } else {
        config::api_request_timeout_seconds()
    }
}

// Runs the api error API helper.
fn api_error(status: StatusCode, message: impl Into<String>) -> Response {
    json_response(status, json!({"ok": false, "error": message.into()}))
}

// Runs the api error logged API helper.
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

// Handles json response logic.
fn json_response(status: StatusCode, value: Value) -> Response {
    (status, Json(value)).into_response()
}

// Handles route specs logic.
fn route_specs() -> Vec<Value> {
    vec![
        json!({"section": "daemon", "actions": ["reload", "status"]}),
        json!({"section": "ml", "actions": ["refresh", "explain", "explainable", "explained", "status"]}),
        json!({"section": "market", "actions": ["quote", "bars", "news", "clock", "calendar"]}),
        json!({"section": "trade", "actions": ["account", "orders", "positions", "buy", "sell", "cancel", "close"], "mutation_guard": "buy/sell/cancel/close require auto-trading disabled"}),
        json!({"section": "data", "actions": ["movers", "screen", "watchlist", "suggest", "status"]}),
        json!({"section": "compliance", "actions": ["wash", "pdt", "tax"]}),
        json!({"section": "auto", "actions": ["sync-orders", "status", "history", "config", "track", "untrack"]}),
        json!({"section": "feeds", "actions": ["add", "remove", "sync", "list", "search", "graph", "sentiment", "correlate", "status"]}),
    ]
}
