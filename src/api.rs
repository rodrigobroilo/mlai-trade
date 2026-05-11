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
use axum::http::{header, HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine;
use chrono::Utc;
use flate2::{
    write::{GzEncoder, ZlibEncoder},
    Compression,
};
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
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};
use tokio::process::Command as TokioCommand;
use tokio::sync::Mutex;
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;
use tower_http::compression::CompressionLayer;
use x509_parser::parse_x509_certificate;
use x509_parser::pem::parse_x509_pem;

static TERMINATE: AtomicBool = AtomicBool::new(false);
static RELOAD: AtomicBool = AtomicBool::new(false);
static API_SSL_TCP_ACTIVE: AtomicUsize = AtomicUsize::new(0);
static API_SSL_UDP_ACTIVE: AtomicUsize = AtomicUsize::new(0);
static API_SSL_TCP_REJECT_LOG_UNTIL_EPOCH: AtomicUsize = AtomicUsize::new(0);
static API_SSL_UDP_REJECT_LOG_UNTIL_EPOCH: AtomicUsize = AtomicUsize::new(0);
static API_SSL_TCP_TLS_CLIENT_REJECT_LOG_UNTIL_EPOCH: AtomicUsize = AtomicUsize::new(0);
static API_SSL_TCP_REJECT_SUPPRESSED: AtomicUsize = AtomicUsize::new(0);
static API_SSL_UDP_REJECT_SUPPRESSED: AtomicUsize = AtomicUsize::new(0);
static API_SSL_TCP_TLS_CLIENT_REJECT_SUPPRESSED: AtomicUsize = AtomicUsize::new(0);
const DEF_SSL_CERT_DAYS: u32 = 397;
const DEF_SSL_RENEW_BEFORE_DAYS: i64 = 30;
const DEF_SSL_TCP_MAX_HEADER_BYTES: usize = 64 * 1024;
const DEF_SSL_TCP_MAX_ACTIVE_CONNECTIONS: usize = 128;
const DEF_SSL_UDP_MAX_ACTIVE_CONNECTIONS: usize = 128;
const DEF_SSL_REJECT_LOG_COOLDOWN_SECONDS: u64 = 60;
const DEF_RESPONSE_COMPRESSION_MIN_BYTES: usize = 1024;
const DEF_API_RESPONSE_MAX_BYTES: usize = 64 * 1024 * 1024;
const DEF_DASHBOARD_ORDERS_LIMIT: usize = 100;
const DEF_DASHBOARD_TABLE_INITIAL_ROWS: usize = 50;
const DEF_DASHBOARD_TABLE_PAGE_ROWS: usize = 50;
const DEF_REALTIME_STREAM_INTERVAL_SECONDS: u64 = 60;
const DEF_REALTIME_STREAM_HEARTBEAT_SECONDS: u64 = 15;
const DEF_REALTIME_STREAM_MAX_SECONDS: u64 = 6 * 60 * 60;
const DEF_REALTIME_MAX_ACTIVE_STREAMS: usize = 16;
const DEF_SSL_CERT_ORGANIZATION: &str = "MLAI-TRADE";
const DEF_SSL_CERT_ORGANIZATIONAL_UNIT: &str = "MLAI-TRADE";

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
        "max_response_bytes": DEF_API_RESPONSE_MAX_BYTES,
        "overload_retry_after_seconds": limits.overload_retry_after_seconds,
        "market_bars_max_symbols": crate::MARKET_BARS_MAX_SYMBOLS,
        "market_bars_max_total_bars": crate::MARKET_BARS_MAX_TOTAL_BARS,
        "recommended_market_bars_batch_symbols": 25,
        "dashboard_orders_limit": DEF_DASHBOARD_ORDERS_LIMIT,
        "dashboard_table_initial_rows": DEF_DASHBOARD_TABLE_INITIAL_ROWS,
        "dashboard_table_page_rows": DEF_DASHBOARD_TABLE_PAGE_ROWS,
        "realtime": {
            "snapshot_path": "/events/snapshot",
            "stream_path": "/events/stream",
            "stream_content_type": "text/event-stream",
            "interval_seconds": DEF_REALTIME_STREAM_INTERVAL_SECONDS,
            "heartbeat_seconds": DEF_REALTIME_STREAM_HEARTBEAT_SECONDS,
            "max_stream_seconds": DEF_REALTIME_STREAM_MAX_SECONDS,
            "max_active_streams": DEF_REALTIME_MAX_ACTIVE_STREAMS,
            "transport_preference": ["http3_quic", "tcp_https"],
            "fallback": "snapshot_polling",
        },
        "response_compression": {
            "accepted": ["zstd", "br", "gzip", "deflate"],
            "preferred_order": ["zstd", "br", "gzip", "deflate"],
            "enabled_when_client_sends_accept_encoding": true,
            "required": false
        },
    })
}

// Returns the public API limits response for adaptive clients.
fn api_limits_response() -> Value {
    json!({
        "ok": true,
        "limits": api_limits_json(&config::api_limit_config()),
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
    pub tcp_enabled: bool,
    pub tcp_bind_host: String,
    pub tcp_port: u16,
    pub auth_enabled: bool,
}

#[derive(Debug, Clone)]
struct HttpsDnsAnswer {
    priority: u16,
    target: String,
    alpn: Vec<String>,
    port: Option<u16>,
    ech: Option<Vec<u8>>,
    ttl: u32,
}

#[derive(Debug, Clone)]
struct HttpsDnsCheck {
    ok: bool,
    domain: String,
    resolver: String,
    required_alpn: String,
    required_port: u16,
    required_ech: bool,
    answers: Vec<HttpsDnsAnswer>,
    errors: Vec<String>,
}

#[derive(Debug)]
struct ApiRuntimeState {
    service: &'static str,
    started_at: Instant,
    started_at_utc: String,
    active_requests: AtomicUsize,
    active_long_requests: AtomicUsize,
    total_requests: AtomicUsize,
    rejected_requests: AtomicUsize,
    market_bar_api_requests: AtomicUsize,
    market_bar_results: AtomicUsize,
    market_bar_cache_hits: AtomicUsize,
    market_bar_provider_fetches: AtomicUsize,
    market_bar_empty_results: AtomicUsize,
    market_bar_cache_rows_stored: AtomicUsize,
    realtime_active_streams: AtomicUsize,
    realtime_total_streams: AtomicUsize,
    realtime_events_sent: AtomicUsize,
    rate: Mutex<ApiRateState>,
}

impl ApiRuntimeState {
    // Constructs a new instance with the provided inputs.
    fn new(service: &'static str) -> Self {
        Self {
            service,
            started_at: Instant::now(),
            started_at_utc: Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            active_requests: AtomicUsize::new(0),
            active_long_requests: AtomicUsize::new(0),
            total_requests: AtomicUsize::new(0),
            rejected_requests: AtomicUsize::new(0),
            market_bar_api_requests: AtomicUsize::new(0),
            market_bar_results: AtomicUsize::new(0),
            market_bar_cache_hits: AtomicUsize::new(0),
            market_bar_provider_fetches: AtomicUsize::new(0),
            market_bar_empty_results: AtomicUsize::new(0),
            market_bar_cache_rows_stored: AtomicUsize::new(0),
            realtime_active_streams: AtomicUsize::new(0),
            realtime_total_streams: AtomicUsize::new(0),
            realtime_events_sent: AtomicUsize::new(0),
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
        let market_bar_results = self.market_bar_results.load(Ordering::SeqCst);
        let market_bar_cache_hits = self.market_bar_cache_hits.load(Ordering::SeqCst);
        let market_bar_provider_fetches = self.market_bar_provider_fetches.load(Ordering::SeqCst);
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
            "cache": {
                "market_bars": {
                    "api_requests": self.market_bar_api_requests.load(Ordering::SeqCst),
                    "results": market_bar_results,
                    "cache_hits": market_bar_cache_hits,
                    "provider_fetches": market_bar_provider_fetches,
                    "empty_results": self.market_bar_empty_results.load(Ordering::SeqCst),
                    "cache_rows_stored": self.market_bar_cache_rows_stored.load(Ordering::SeqCst),
                    "cache_hit_rate": if market_bar_results > 0 {
                        market_bar_cache_hits as f64 / market_bar_results as f64
                    } else {
                        0.0
                    },
                    "provider_fetch_rate": if market_bar_results > 0 {
                        market_bar_provider_fetches as f64 / market_bar_results as f64
                    } else {
                        0.0
                    },
                }
            },
            "realtime": {
                "active_streams": self.realtime_active_streams.load(Ordering::SeqCst),
                "total_streams": self.realtime_total_streams.load(Ordering::SeqCst),
                "events_sent": self.realtime_events_sent.load(Ordering::SeqCst),
                "interval_seconds": DEF_REALTIME_STREAM_INTERVAL_SECONDS,
                "heartbeat_seconds": DEF_REALTIME_STREAM_HEARTBEAT_SECONDS,
                "max_stream_seconds": DEF_REALTIME_STREAM_MAX_SECONDS,
                "max_active_streams": DEF_REALTIME_MAX_ACTIVE_STREAMS,
            },
            "resources": process::current_process_usage_json(Some(self.started_at)),
        })
    }

    // Builds a status-file health payload for status commands outside this process.
    fn health_json(&self) -> Value {
        json!({
            "ok": true,
            "service": self.service,
            "updated_at_utc": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "limits": api_limits_json(&config::api_limit_config()),
            "runtime": self.runtime_json(),
        })
    }

    // Writes SSL/H3 runtime counters so CLI status can report browser traffic.
    fn write_ssl_status_file(&self) {
        if self.service != "mlai-trade-api-ssl" {
            return;
        }
        let path = api_ssl_runtime_status_file();
        if let Ok(payload) = serde_json::to_string_pretty(&self.health_json()) {
            let _ = paths::write_runtime_metadata_file(&path, payload);
        }
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

struct ApiRealtimeStreamGuard {
    state: Arc<ApiRuntimeState>,
}

impl ApiRealtimeStreamGuard {
    // Marks a realtime stream as active for runtime status.
    fn try_new(state: Arc<ApiRuntimeState>) -> Option<Self> {
        if !try_increment_counter(
            &state.realtime_active_streams,
            DEF_REALTIME_MAX_ACTIVE_STREAMS,
        ) {
            state.rejected_requests.fetch_add(1, Ordering::SeqCst);
            return None;
        }
        state.realtime_total_streams.fetch_add(1, Ordering::SeqCst);
        Some(Self { state })
    }
}

// Builds the payload sent by the realtime snapshot and stream routes.
fn realtime_event_payload(
    state: &ApiRuntimeState,
    event: &str,
    transport: &str,
    sequence: usize,
) -> Value {
    json!({
        "ok": true,
        "event": event,
        "sequence": sequence,
        "transport": transport,
        "updated_at_utc": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "refresh": {
            "recommended": event == "dashboard.refresh",
            "interval_seconds": DEF_REALTIME_STREAM_INTERVAL_SECONDS,
            "heartbeat_seconds": DEF_REALTIME_STREAM_HEARTBEAT_SECONDS,
            "mode": "dashboard_snapshot",
        },
        "runtime": state.runtime_json(),
    })
}

// Serializes one server-sent event frame.
fn sse_event_bytes(event: &str, sequence: usize, payload: Value) -> anyhow::Result<Bytes> {
    let data = serde_json::to_string(&payload)?;
    Ok(Bytes::from(format!(
        "id: {sequence}\nevent: {event}\ndata: {data}\n\n"
    )))
}

// Records that a realtime event was delivered.
fn count_realtime_event(state: &ApiRuntimeState) {
    state.realtime_events_sent.fetch_add(1, Ordering::SeqCst);
    state.write_ssl_status_file();
}

impl Drop for ApiRealtimeStreamGuard {
    // Releases the active stream counter when the client disconnects.
    fn drop(&mut self) {
        self.state
            .realtime_active_streams
            .fetch_sub(1, Ordering::SeqCst);
        self.state.write_ssl_status_file();
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

// Returns the SSL/H3 runtime status file used by CLI status commands.
fn api_ssl_runtime_status_file() -> PathBuf {
    paths::tmp_dir().join("mlai-trade-api-ssl-status.json")
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
    let mut ech = None;
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
            5 => {
                ech = Some(value.to_vec());
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
        ech,
        ttl,
    })
}

// Queries DNS HTTPS records and verifies the public H3 discovery policy.
fn check_https_dns_record(domain: &str, required_port: u16, required_ech: bool) -> HttpsDnsCheck {
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
        let ech_ok = !required_ech || answer.ech.is_some();
        port_ok && h3 && !tcp_fallback && ech_ok
    });
    HttpsDnsCheck {
        ok,
        domain: domain.to_string(),
        resolver,
        required_alpn: "h3".to_string(),
        required_port,
        required_ech,
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
    let certificates = ssl_cert_info_values(status, "all").unwrap_or_else(|err| {
        vec![json!({
            "status": "parse_error",
            "error": err.to_string(),
        })]
    });
    json!({
        "api_enabled": status.api_enabled,
        "enabled": status.enabled,
        "running": read_pid(&status.pid_file).map(process_alive).unwrap_or(false),
        "pid": read_pid(&status.pid_file),
        "transport": "http3_quic_and_tcp_https",
        "listen": {
            "host": status.bind_host.clone(),
            "ipv4_enabled": status.ipv4_enabled,
            "ipv6_enabled": status.ipv6_enabled,
            "udp_port": status.udp_port,
            "normal_tcp_https": status.tcp_enabled,
            "tcp_https": {
                "enabled": status.tcp_enabled,
                "host": status.tcp_bind_host.clone(),
                "tcp_port": status.tcp_port,
                "purpose": "browser_https_dashboard_and_json_api",
                "api_routes_allowed": true,
            },
            "tcp_bootstrap": {
                "enabled": status.tcp_bootstrap_enabled,
                "host": status.tcp_bootstrap_bind_host.clone(),
                "tcp_port": status.tcp_bootstrap_port,
                "purpose": "legacy_alias_for_tcp_https",
                "api_routes_allowed": true,
            },
        },
        "connection_limits": {
            "tcp_active": API_SSL_TCP_ACTIVE.load(Ordering::Relaxed),
            "tcp_max_active": DEF_SSL_TCP_MAX_ACTIVE_CONNECTIONS,
            "udp_active": API_SSL_UDP_ACTIVE.load(Ordering::Relaxed),
            "udp_max_active": DEF_SSL_UDP_MAX_ACTIVE_CONNECTIONS,
        },
        "auth": {
            "enabled_for_non_localhost": status.auth_enabled,
            "username_configured": !status.auth_username.is_empty(),
            "password_configured": !status.auth_password.is_empty() && status.auth_password != "replace_me",
            "localhost_bypass": true,
        },
        "trusted_proxy": {
            "enabled": status.trusted_proxy_enabled,
            "cidrs": status.trusted_proxy_cidrs.clone(),
            "raw_forwarding_headers_logged": false,
        },
        "ech": {
            "enabled": status.ech_enabled,
            "supported_by_tls_stack": false,
            "supported_by_current_listener": false,
            "listener_stack": "rustls_quinn",
            "status": if status.ech_enabled { "unsupported_fail_closed" } else { "disabled" },
            "public_name": status.ech_public_name.clone(),
            "config_file": status.ech_config_file.display().to_string(),
            "key_file": status.ech_key_file.display().to_string(),
            "config_file_exists": status.ech_config_file.exists(),
            "key_file_exists": status.ech_key_file.exists(),
            "require_dns_https_record": status.ech_require_dns_https_record,
            "dns_svcb_parameter": "ech",
            "dns_svcb_key": 5,
            "activation": "blocked_until_server_side_ech_tls_stack_is_available",
            "rfc": ["RFC9849", "RFC9848"],
        },
        "tls": {
            "version": "TLS1.3",
            "alpn": ["h3", "http/1.1"],
            "udp_alpn": ["h3"],
            "tcp_alpn": ["http/1.1"],
            "key_exchange_policy": status.key_exchange_policy.clone(),
            "mlkem_required": status.key_exchange_policy == "mlkem_required",
            "fallback_to_classical": ssl_kx_policy_allows_classical_fallback(&status.key_exchange_policy),
            "key_exchange_groups": ssl_kx_group_labels(&status.key_exchange_policy),
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
        "certificates": certificates,
        "pid_file": status.pid_file.display().to_string(),
        "runtime_status_file": api_ssl_runtime_status_file().display().to_string(),
        "log_file": status.log_file.display().to_string(),
        "implementation_status": "implemented_http3_quic_and_tcp_https_listeners",
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

// Returns whether the cert target selector includes the named target.
fn cert_target_includes(target: &str, name: &str) -> anyhow::Result<bool> {
    match target {
        "all" => Ok(true),
        "h3" | "acme" => Ok(target == name),
        _ => anyhow::bail!("invalid certificate target `{target}`; use h3 or acme"),
    }
}

// Returns a certificate extension OID as dotted decimal.
fn cert_oid_to_string(oid: &x509_parser::der_parser::oid::Oid) -> String {
    oid.to_id_string()
}

// Reads useful X.509 metadata for status and renewal decisions.
fn certificate_info_value(
    name: &str,
    cert_file: &PathBuf,
    key_file: &PathBuf,
    generated_mode: &str,
    auto_renew_allowed_by_policy: bool,
) -> Value {
    let cert_exists = cert_file.exists();
    let key_exists = key_file.exists();
    let auto_renew_reason = if name == "acme" {
        "TLS-ALPN-01 challenge certificates require the current ACME key authorization; they are generated per challenge and are not auto-renewed"
    } else if generated_mode == "mlai-trade" && auto_renew_allowed_by_policy {
        "mlai-trade-generated certificate"
    } else if generated_mode != "mlai-trade" {
        "provided/public CA certificates are not overwritten"
    } else {
        "auto-renew disabled by policy"
    };
    let mut payload = json!({
        "target": name,
        "cert_file": cert_file.display().to_string(),
        "key_file": key_file.display().to_string(),
        "cert_exists": cert_exists,
        "key_exists": key_exists,
        "generated_mode": generated_mode,
        "auto_renew_allowed": generated_mode == "mlai-trade" && auto_renew_allowed_by_policy,
        "auto_renew_reason": auto_renew_reason,
    });
    if !cert_exists {
        if let Some(object) = payload.as_object_mut() {
            object.insert("status".to_string(), json!("missing"));
        }
        return payload;
    }
    let parsed = (|| -> anyhow::Result<Value> {
        let bytes = fs::read(cert_file)?;
        let (_, pem) = parse_x509_pem(&bytes)?;
        let (_, cert) = parse_x509_certificate(&pem.contents)?;
        let now = time::OffsetDateTime::now_utc();
        let not_before = cert.validity().not_before.to_datetime();
        let not_after = cert.validity().not_after.to_datetime();
        let days_remaining = (not_after - now).whole_days();
        let sans = cert
            .subject_alternative_name()?
            .map(|extension| {
                extension
                    .value
                    .general_names
                    .iter()
                    .map(|name| name.to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let custom_extension_oids = cert
            .extensions()
            .iter()
            .map(|extension| cert_oid_to_string(&extension.oid))
            .collect::<Vec<_>>();
        let has_acme_identifier = custom_extension_oids
            .iter()
            .any(|oid| oid == "1.3.6.1.5.5.7.1.31");
        let generated_by_mlai = cert.subject().to_string().contains("mlai-trade")
            || cert.issuer().to_string().contains("mlai-trade")
            || generated_mode == "mlai-trade";
        Ok(json!({
            "status": "ok",
            "pem_label": pem.label,
            "subject": cert.subject().to_string(),
            "issuer": cert.issuer().to_string(),
            "serial": cert.raw_serial_as_string(),
            "version": format!("{:?}", cert.version()),
            "signature_algorithm": cert.signature_algorithm.algorithm.to_string(),
            "not_before": not_before.format(&time::format_description::well_known::Rfc3339).unwrap_or_else(|_| not_before.to_string()),
            "not_after": not_after.format(&time::format_description::well_known::Rfc3339).unwrap_or_else(|_| not_after.to_string()),
            "days_remaining": days_remaining,
            "expires_soon": days_remaining <= DEF_SSL_RENEW_BEFORE_DAYS,
            "expired": days_remaining < 0,
            "subject_alt_names": sans,
            "custom_extension_oids": custom_extension_oids,
            "has_acme_identifier": has_acme_identifier,
            "generated_by_mlai_trade": generated_by_mlai,
            "auto_renew_allowed": generated_mode == "mlai-trade" && generated_by_mlai && auto_renew_allowed_by_policy,
            "auto_renew_before_days": DEF_SSL_RENEW_BEFORE_DAYS,
        }))
    })();
    match parsed {
        Ok(parsed) => {
            if let (Some(object), Some(parsed_object)) =
                (payload.as_object_mut(), parsed.as_object())
            {
                for (key, value) in parsed_object {
                    object.insert(key.clone(), value.clone());
                }
            }
            payload
        }
        Err(err) => {
            if let Some(object) = payload.as_object_mut() {
                object.insert("status".to_string(), json!("parse_error"));
                object.insert("error".to_string(), json!(err.to_string()));
            }
            payload
        }
    }
}

// Returns both configured cert metadata objects.
fn ssl_cert_info_values(
    status: &config::ApiSslRuntimeConfig,
    target: &str,
) -> anyhow::Result<Vec<Value>> {
    let mut certs = Vec::new();
    let h3_generated_mode = if status.cert_mode == "self_signed" {
        "mlai-trade"
    } else {
        "provided"
    };
    if cert_target_includes(target, "h3")? {
        certs.push(certificate_info_value(
            "h3",
            &status.cert_file,
            &status.key_file,
            h3_generated_mode,
            status.cert_mode == "self_signed",
        ));
    }
    if cert_target_includes(target, "acme")? {
        certs.push(certificate_info_value(
            "acme",
            &status.acme_challenge_cert_file,
            &status.acme_challenge_key_file,
            "mlai-trade",
            false,
        ));
    }
    Ok(certs)
}

// Prints configured SSL certificate metadata.
pub fn cmd_ssl_cert_info(target: String, json_out: bool) -> anyhow::Result<()> {
    let status = config::api_ssl_runtime_config();
    let certs = ssl_cert_info_values(&status, target.as_str())?;
    let payload = json!({
        "ok": true,
        "target": target,
        "cert_mode": status.cert_mode,
        "auto_renew_before_days": DEF_SSL_RENEW_BEFORE_DAYS,
        "certificates": certs,
    });
    if json_out {
        print_json(payload)?;
        return Ok(());
    }
    println!("API SSL Certificate Info");
    println!("  Cert mode:   {}", status.cert_mode);
    println!(
        "  Auto renew:  only mlai-trade-generated certificates, {} days before expiry",
        DEF_SSL_RENEW_BEFORE_DAYS
    );
    if let Some(items) = payload["certificates"].as_array() {
        for item in items {
            println!();
            println!(
                "{} certificate:",
                item["target"].as_str().unwrap_or("unknown")
            );
            println!(
                "  Status:      {}",
                item["status"].as_str().unwrap_or("unknown")
            );
            println!(
                "  Cert file:   {}",
                item["cert_file"].as_str().unwrap_or("not available")
            );
            println!(
                "  Key file:    {}",
                item["key_file"].as_str().unwrap_or("not available")
            );
            println!(
                "  Subject:     {}",
                item["subject"].as_str().unwrap_or("not available")
            );
            println!(
                "  Issuer:      {}",
                item["issuer"].as_str().unwrap_or("not available")
            );
            println!(
                "  Serial:      {}",
                item["serial"].as_str().unwrap_or("not available")
            );
            println!(
                "  Not before:  {}",
                item["not_before"].as_str().unwrap_or("not available")
            );
            println!(
                "  Not after:   {}",
                item["not_after"].as_str().unwrap_or("not available")
            );
            println!(
                "  Days left:   {}",
                item["days_remaining"]
                    .as_i64()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "not available".to_string())
            );
            println!(
                "  SANs:        {}",
                item["subject_alt_names"]
                    .as_array()
                    .map(|values| values
                        .iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", "))
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "not available".to_string())
            );
            println!(
                "  ACME ext:    {}",
                item["has_acme_identifier"]
                    .as_bool()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "not available".to_string())
            );
            println!(
                "  Generated:   {}",
                item["generated_by_mlai_trade"]
                    .as_bool()
                    .map(|v| if v { "mlai-trade" } else { "external/provided" })
                    .unwrap_or("not available")
            );
            println!(
                "  Auto renew:  {}",
                item["auto_renew_allowed"]
                    .as_bool()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "false".to_string())
            );
            println!(
                "  Renew note:  {}",
                item["auto_renew_reason"]
                    .as_str()
                    .unwrap_or("not available")
            );
        }
    }
    Ok(())
}

// Returns whether a certificate info object should be renewed now.
fn cert_info_needs_renewal(info: &Value) -> bool {
    if !info["auto_renew_allowed"].as_bool().unwrap_or(false) {
        return false;
    }
    if info["status"].as_str() == Some("missing") {
        return true;
    }
    if info["status"].as_str() != Some("ok") {
        return false;
    }
    info["days_remaining"].as_i64().unwrap_or(i64::MAX) <= DEF_SSL_RENEW_BEFORE_DAYS
}

// Auto-renews mlai-trade-managed certificates when startup detects expiry risk.
fn maybe_auto_renew_ssl_certs(status: &config::ApiSslRuntimeConfig) -> anyhow::Result<Vec<Value>> {
    let mut actions = Vec::new();
    if status.cert_mode != "self_signed" {
        api_ssl_log(json!({
            "event": "api_ssl_cert_auto_renew_skipped",
            "level": "info",
            "reason": "cert_mode_not_self_signed",
            "cert_mode": status.cert_mode,
        }));
        return Ok(actions);
    }
    for cert in ssl_cert_info_values(status, "all")? {
        let target = cert["target"].as_str().unwrap_or("unknown");
        if cert_info_needs_renewal(&cert) {
            let acme_key_authorization = if target == "acme" { None } else { None };
            generate_ssl_certs(
                status.clone(),
                target.to_string(),
                None,
                Vec::new(),
                DEF_SSL_CERT_DAYS,
                acme_key_authorization,
                None,
                None,
                true,
            )?;
            let action = json!({
                "target": target,
                "previous_status": cert["status"].clone(),
                "previous_days_remaining": cert["days_remaining"].clone(),
                "renewed": true,
            });
            api_ssl_log(json!({
                "event": "api_ssl_cert_auto_renewed",
                "level": "info",
                "target": target,
                "previous_status": cert["status"].clone(),
                "previous_days_remaining": cert["days_remaining"].clone(),
            }));
            actions.push(action);
        } else {
            api_ssl_log(json!({
                "event": "api_ssl_cert_auto_renew_not_needed",
                "level": "info",
                "target": target,
                "status": cert["status"].clone(),
                "days_remaining": cert["days_remaining"].clone(),
                "auto_renew_allowed": cert["auto_renew_allowed"].clone(),
            }));
        }
    }
    Ok(actions)
}

// Builds the RFC 8737 ACME TLS-ALPN-01 challenge certificate.
fn acme_challenge_cert(
    domain: &str,
    acme_key_authorization: Option<&str>,
    organization: &str,
    organizational_unit: &str,
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
    push_cert_subject_org(
        &mut params.distinguished_name,
        organization,
        organizational_unit,
    );
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
    target: String,
    domain: Option<String>,
    sans: Vec<String>,
    days: u32,
    acme_key_authorization: Option<String>,
    organization: Option<String>,
    organizational_unit: Option<String>,
    force: bool,
    json_out: bool,
) -> anyhow::Result<()> {
    let status = config::api_ssl_runtime_config();
    validate_ssl_cert_target_args(target.as_str(), acme_key_authorization.as_ref())?;
    let generate_h3 = cert_target_includes(target.as_str(), "h3")?;
    let generate_acme = cert_target_includes(target.as_str(), "acme")?;
    if !force && generate_h3 && (status.cert_file.exists() || status.key_file.exists()) {
        anyhow::bail!(
            "H3 certificate files already exist; use `mlai-trade api ssl cert renew --target h3` or `--force` to overwrite"
        );
    }
    if !force
        && generate_acme
        && (status.acme_challenge_cert_file.exists() || status.acme_challenge_key_file.exists())
    {
        anyhow::bail!(
            "ACME challenge certificate files already exist; use `mlai-trade api ssl cert renew --target acme` or `--force` to overwrite"
        );
    }
    generate_ssl_certs(
        status,
        target,
        domain,
        sans,
        days,
        acme_key_authorization,
        organization,
        organizational_unit,
        json_out,
    )
}

// Renews the remote API identity cert plus the RFC 8737 challenge cert.
pub fn cmd_ssl_cert_renew(
    target: String,
    domain: Option<String>,
    sans: Vec<String>,
    days: u32,
    acme_key_authorization: Option<String>,
    organization: Option<String>,
    organizational_unit: Option<String>,
    json_out: bool,
) -> anyhow::Result<()> {
    validate_ssl_cert_target_args(target.as_str(), acme_key_authorization.as_ref())?;
    generate_ssl_certs(
        config::api_ssl_runtime_config(),
        target,
        domain,
        sans,
        days,
        acme_key_authorization,
        organization,
        organizational_unit,
        json_out,
    )
}

// Validates certificate target-specific arguments.
fn validate_ssl_cert_target_args(
    target: &str,
    acme_key_authorization: Option<&String>,
) -> anyhow::Result<()> {
    if acme_key_authorization
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
        && target != "acme"
    {
        anyhow::bail!("--acme-key-authorization is only valid with --target acme");
    }
    Ok(())
}

// Normalizes generated certificate subject organization fields.
fn ssl_cert_subject_value(value: Option<String>, default_value: &str) -> String {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default_value.to_string())
}

// Adds organization fields to generated certificate subjects.
fn push_cert_subject_org(
    distinguished_name: &mut rcgen::DistinguishedName,
    organization: &str,
    organizational_unit: &str,
) {
    distinguished_name.push(rcgen::DnType::OrganizationName, organization.to_string());
    distinguished_name.push(
        rcgen::DnType::OrganizationalUnitName,
        organizational_unit.to_string(),
    );
}

// Handles shared certificate generation logic.
fn generate_ssl_certs(
    status: config::ApiSslRuntimeConfig,
    target: String,
    domain: Option<String>,
    sans: Vec<String>,
    days: u32,
    acme_key_authorization: Option<String>,
    organization: Option<String>,
    organizational_unit: Option<String>,
    json_out: bool,
) -> anyhow::Result<()> {
    let generate_h3 = cert_target_includes(target.as_str(), "h3")?;
    let generate_acme = cert_target_includes(target.as_str(), "acme")?;
    let (primary, names) = ssl_cert_names(&status, domain, sans);
    let organization = ssl_cert_subject_value(organization, DEF_SSL_CERT_ORGANIZATION);
    let organizational_unit =
        ssl_cert_subject_value(organizational_unit, DEF_SSL_CERT_ORGANIZATIONAL_UNIT);
    let mut h3_payload = json!({"target": "h3", "skipped": true});
    if generate_h3 {
        let mut params = rcgen::CertificateParams::new(names.clone())?;
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, primary.clone());
        push_cert_subject_org(
            &mut params.distinguished_name,
            &organization,
            &organizational_unit,
        );
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
        h3_payload = json!({
            "target": "h3",
            "cert_file": status.cert_file.display().to_string(),
            "key_file": status.key_file.display().to_string(),
            "subject_alt_names": names,
            "valid_days": days.max(1),
            "purpose": "http3_h3_identity",
            "generated_by": "mlai-trade",
            "organization": organization.clone(),
            "organizational_unit": organizational_unit.clone(),
        });
    }

    let mut acme_payload = json!({"target": "acme", "skipped": true});
    if generate_acme {
        let (acme_cert, acme_key, acme_digest, acme_ready) = acme_challenge_cert(
            &primary,
            acme_key_authorization.as_deref(),
            &organization,
            &organizational_unit,
        )?;
        write_cert_pair(
            &status.acme_challenge_cert_file,
            &status.acme_challenge_key_file,
            &acme_cert,
            &acme_key,
        )?;
        acme_payload = json!({
            "target": "acme",
            "cert_file": status.acme_challenge_cert_file.display().to_string(),
            "key_file": status.acme_challenge_key_file.display().to_string(),
            "domain": primary,
            "rfc": "RFC8737",
            "alpn": "acme-tls/1",
            "acme_identifier_sha256": acme_digest,
            "ready_for_real_acme_validation": acme_ready,
            "organization": organization.clone(),
            "organizational_unit": organizational_unit.clone(),
            "note": if acme_ready {
                "challenge cert contains the supplied key authorization digest"
            } else {
                "placeholder challenge cert generated; pass --target acme --acme-key-authorization to generate a certificate for a live RFC 8737 authorization"
            },
        });
    }
    let payload = json!({
        "ok": true,
        "target": target,
        "certificate": h3_payload,
        "acme_tls_alpn_01_challenge_certificate": acme_payload,
    });
    api_ssl_log(json!({
        "event": "api_ssl_cert_generated",
        "level": "info",
        "target": target,
        "cert_file": status.cert_file.display().to_string(),
        "key_file": status.key_file.display().to_string(),
        "acme_challenge_cert_file": status.acme_challenge_cert_file.display().to_string(),
        "acme_ready": acme_payload["ready_for_real_acme_validation"].clone(),
    }));
    if json_out {
        print_json(payload)?;
    } else {
        println!("API SSL certificate generated");
        if generate_h3 {
            println!("  Target:       h3");
            println!("  H3 cert:      {}", status.cert_file.display());
            println!("  H3 key:       {}", status.key_file.display());
            println!("  Domain:       {}", primary);
            println!("  H3 SANs:      {:?}", names);
            println!("  Subject O:    {}", organization);
            println!("  Subject OU:   {}", organizational_unit);
        }
        if generate_acme {
            println!("  Target:       acme");
            println!(
                "  ACME cert:    {}",
                status.acme_challenge_cert_file.display()
            );
            println!(
                "  ACME key:     {}",
                status.acme_challenge_key_file.display()
            );
            println!("  Domain:       {}", primary);
            println!("  Subject O:    {}", organization);
            println!("  Subject OU:   {}", organizational_unit);
            println!(
                "  ACME status:  {}",
                if acme_payload["ready_for_real_acme_validation"]
                    .as_bool()
                    .unwrap_or(false)
                {
                    "ready for current RFC 8737 key authorization"
                } else {
                    "placeholder; pass --acme-key-authorization with --target acme for a live challenge"
                }
            );
        }
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

// Returns configured TLS key exchange groups for remote SSL listeners.
fn ssl_kx_groups(
    policy: &str,
) -> (
    Vec<&'static dyn rustls::crypto::SupportedKxGroup>,
    Vec<&'static str>,
    bool,
) {
    match policy {
        "mlkem_required" => (
            vec![
                rustls::crypto::aws_lc_rs::kx_group::X25519MLKEM768,
                rustls::crypto::aws_lc_rs::kx_group::SECP256R1MLKEM768,
                rustls::crypto::aws_lc_rs::kx_group::MLKEM768,
            ],
            vec!["X25519MLKEM768", "SECP256R1MLKEM768", "MLKEM768"],
            false,
        ),
        _ => (
            vec![
                rustls::crypto::aws_lc_rs::kx_group::X25519MLKEM768,
                rustls::crypto::aws_lc_rs::kx_group::SECP256R1MLKEM768,
                rustls::crypto::aws_lc_rs::kx_group::X25519,
                rustls::crypto::aws_lc_rs::kx_group::SECP256R1,
                rustls::crypto::aws_lc_rs::kx_group::SECP384R1,
            ],
            vec![
                "X25519MLKEM768",
                "SECP256R1MLKEM768",
                "X25519",
                "SECP256R1",
                "SECP384R1",
            ],
            true,
        ),
    }
}

// Returns the configured TLS key exchange group labels.
fn ssl_kx_group_labels(policy: &str) -> Vec<&'static str> {
    let (_, labels, _) = ssl_kx_groups(policy);
    labels
}

// Returns whether the policy permits strong classical fallback groups.
fn ssl_kx_policy_allows_classical_fallback(policy: &str) -> bool {
    let (_, _, fallback) = ssl_kx_groups(policy);
    fallback
}

// Presents library handshake errors from the API client's point of view.
fn ssl_client_error_message(err: &anyhow::Error) -> String {
    err.to_string().replace("peer is ", "client is ")
}

// Returns a bounded header value for JSON logs.
fn log_header_value(value: Option<&str>) -> String {
    const MAX_LOG_HEADER_CHARS: usize = 4096;
    let Some(value) = value else {
        return "not available".to_string();
    };
    let value = value.trim().replace(['\r', '\n'], " ");
    if value.is_empty() {
        return "not available".to_string();
    }
    value.chars().take(MAX_LOG_HEADER_CHARS).collect()
}

#[derive(Clone, Debug)]
struct ForwardedClientLogFields {
    client_ip: String,
    client_ip_source: &'static str,
    forwarded_headers_trusted: bool,
    cf_ray: String,
}

// Returns the prefix length for an IP version.
fn ip_prefix_bits(ip: IpAddr) -> u8 {
    if ip.is_ipv4() {
        32
    } else {
        128
    }
}

// Parses one trusted proxy entry as CIDR, treating bare IPs as host routes.
fn parse_trusted_proxy_cidr(value: &str) -> Option<(IpAddr, u8)> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let (addr, prefix) = value
        .split_once('/')
        .map(|(addr, prefix)| (addr, Some(prefix)))
        .unwrap_or((value, None));
    let ip = addr.parse::<IpAddr>().ok()?;
    let prefix = prefix
        .map(str::parse::<u8>)
        .transpose()
        .ok()?
        .unwrap_or_else(|| ip_prefix_bits(ip));
    (prefix <= ip_prefix_bits(ip)).then_some((ip, prefix))
}

// Normalizes IPv4-mapped IPv6 peers before trusted-proxy CIDR matching.
fn normalize_proxy_peer_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(ip)),
        IpAddr::V4(_) => ip,
    }
}

// Returns whether an IP belongs to a CIDR.
fn ip_in_cidr(ip: IpAddr, network: IpAddr, prefix: u8) -> bool {
    match (
        normalize_proxy_peer_ip(ip),
        normalize_proxy_peer_ip(network),
    ) {
        (IpAddr::V4(ip), IpAddr::V4(network)) => {
            let ip = u32::from(ip);
            let network = u32::from(network);
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - u32::from(prefix))
            };
            (ip & mask) == (network & mask)
        }
        (IpAddr::V6(ip), IpAddr::V6(network)) => {
            let ip = u128::from(ip);
            let network = u128::from(network);
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - u32::from(prefix))
            };
            (ip & mask) == (network & mask)
        }
        _ => false,
    }
}

// Returns whether forwarding headers are trusted for client IP attribution.
fn trusted_forwarding_peer(status: &config::ApiSslRuntimeConfig, ip: IpAddr) -> bool {
    status.trusted_proxy_enabled
        && status
            .trusted_proxy_cidrs
            .iter()
            .filter_map(|cidr| parse_trusted_proxy_cidr(cidr))
            .any(|(network, prefix)| ip_in_cidr(ip, network, prefix))
}

// Parses the first IP from proxy-style comma-separated forwarding headers.
fn first_forwarded_ip(value: &str) -> Option<IpAddr> {
    value
        .split(',')
        .map(str::trim)
        .map(|part| part.trim_matches('"').trim_matches('[').trim_matches(']'))
        .find_map(|part| part.parse::<IpAddr>().ok())
}

// Finds a case-insensitive header from the parsed TCP HTTPS header list.
fn parsed_header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

// Builds client attribution fields from TCP HTTPS request headers.
fn forwarded_client_fields_from_pairs(
    status: &config::ApiSslRuntimeConfig,
    headers: &[(String, String)],
    source_ip: IpAddr,
) -> ForwardedClientLogFields {
    let cf_ray = log_header_value(parsed_header_value(headers, "cf-ray"));
    let trusted = trusted_forwarding_peer(status, source_ip);
    let forwarded = if trusted {
        parsed_header_value(headers, "cf-connecting-ip")
            .and_then(first_forwarded_ip)
            .map(|ip| (ip, "cloudflare"))
            .or_else(|| {
                parsed_header_value(headers, "true-client-ip")
                    .and_then(first_forwarded_ip)
                    .map(|ip| (ip, "trusted_proxy"))
            })
            .or_else(|| {
                parsed_header_value(headers, "x-forwarded-for")
                    .and_then(first_forwarded_ip)
                    .map(|ip| (ip, "trusted_proxy"))
            })
            .or_else(|| {
                parsed_header_value(headers, "x-real-ip")
                    .and_then(first_forwarded_ip)
                    .map(|ip| (ip, "trusted_proxy"))
            })
    } else {
        None
    };
    let (client_ip, client_ip_source) = forwarded.unwrap_or((source_ip, "socket_source_ip"));
    ForwardedClientLogFields {
        client_ip: socket_ip_for_log(client_ip),
        client_ip_source,
        forwarded_headers_trusted: trusted,
        cf_ray,
    }
}

// Builds client attribution fields from HTTP/3 request headers.
fn forwarded_client_fields_from_header_map(
    status: &config::ApiSslRuntimeConfig,
    headers: &HeaderMap,
    source_ip: IpAddr,
) -> ForwardedClientLogFields {
    let lookup = |name: &'static str| headers.get(name).and_then(|value| value.to_str().ok());
    let cf_ray = log_header_value(lookup("cf-ray"));
    let trusted = trusted_forwarding_peer(status, source_ip);
    let forwarded = if trusted {
        lookup("cf-connecting-ip")
            .and_then(first_forwarded_ip)
            .map(|ip| (ip, "cloudflare"))
            .or_else(|| {
                lookup("true-client-ip")
                    .and_then(first_forwarded_ip)
                    .map(|ip| (ip, "trusted_proxy"))
            })
            .or_else(|| {
                lookup("x-forwarded-for")
                    .and_then(first_forwarded_ip)
                    .map(|ip| (ip, "trusted_proxy"))
            })
            .or_else(|| {
                lookup("x-real-ip")
                    .and_then(first_forwarded_ip)
                    .map(|ip| (ip, "trusted_proxy"))
            })
    } else {
        None
    };
    let (client_ip, client_ip_source) = forwarded.unwrap_or((source_ip, "socket_source_ip"));
    ForwardedClientLogFields {
        client_ip: socket_ip_for_log(client_ip),
        client_ip_source,
        forwarded_headers_trusted: trusted,
        cf_ray,
    }
}

// Adds client forwarding attribution fields to an API SSL/H3 log event.
fn add_forwarded_client_fields(event: &mut Value, fields: &ForwardedClientLogFields) {
    if let Some(object) = event.as_object_mut() {
        object.insert("client_ip".to_string(), json!(fields.client_ip));
        object.insert(
            "client_ip_source".to_string(),
            json!(fields.client_ip_source),
        );
        object.insert(
            "forwarded_headers_trusted".to_string(),
            json!(fields.forwarded_headers_trusted),
        );
        object.insert("cf_ray".to_string(), json!(fields.cf_ray));
    }
}

// Writes an API SSL/H3 log event with HTTP forwarding attribution.
fn api_ssl_log_with_client(mut event: Value, fields: &ForwardedClientLogFields) {
    add_forwarded_client_fields(&mut event, fields);
    api_ssl_log(event);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResponseCompression {
    Zstd,
    Br,
    Gzip,
    Deflate,
}

impl ResponseCompression {
    fn content_encoding(self) -> &'static str {
        match self {
            Self::Zstd => "zstd",
            Self::Br => "br",
            Self::Gzip => "gzip",
            Self::Deflate => "deflate",
        }
    }

    fn priority(self) -> usize {
        match self {
            Self::Zstd => 0,
            Self::Br => 1,
            Self::Gzip => 2,
            Self::Deflate => 3,
        }
    }
}

// Parses q-values from Accept-Encoding tokens.
fn accept_encoding_q(token_parts: &[&str]) -> f64 {
    token_parts
        .iter()
        .skip(1)
        .find_map(|part| {
            let (name, value) = part.trim().split_once('=')?;
            name.trim().eq_ignore_ascii_case("q").then(|| {
                value
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|q| q.is_finite())
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0)
            })
        })
        .unwrap_or(1.0)
}

// Chooses the strongest supported compression advertised by the client.
fn accepted_response_compression(value: Option<&str>) -> Option<ResponseCompression> {
    let value = value?;
    let mut explicit = HashMap::<String, f64>::new();
    for part in value.split(',') {
        let token_parts = part.split(';').collect::<Vec<_>>();
        let Some(name) = token_parts
            .first()
            .map(|name| name.trim().to_ascii_lowercase())
        else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let q = accept_encoding_q(&token_parts);
        explicit
            .entry(name)
            .and_modify(|current| *current = current.max(q))
            .or_insert(q);
    }
    let wildcard_q = explicit.get("*").copied();
    [
        ResponseCompression::Zstd,
        ResponseCompression::Br,
        ResponseCompression::Gzip,
        ResponseCompression::Deflate,
    ]
    .into_iter()
    .filter_map(|encoding| {
        let q = explicit
            .get(encoding.content_encoding())
            .copied()
            .or(wildcard_q)
            .unwrap_or(0.0);
        (q > 0.0).then_some((encoding, q))
    })
    .max_by(|(left_encoding, left_q), (right_encoding, right_q)| {
        left_q
            .partial_cmp(right_q)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right_encoding.priority().cmp(&left_encoding.priority()))
    })
    .map(|(encoding, _)| encoding)
}

// Compresses response bytes with the selected content coding when worthwhile.
fn maybe_compress_body(
    body: Bytes,
    encoding: Option<ResponseCompression>,
    already_encoded: bool,
) -> anyhow::Result<(Bytes, Option<ResponseCompression>)> {
    let Some(encoding) = encoding else {
        return Ok((body, None));
    };
    if already_encoded || body.len() < DEF_RESPONSE_COMPRESSION_MIN_BYTES {
        return Ok((body, None));
    }
    let compressed = match encoding {
        ResponseCompression::Zstd => Bytes::from(zstd::encode_all(&body[..], 3)?),
        ResponseCompression::Br => {
            let mut output = Vec::new();
            {
                let mut encoder = brotli::CompressorWriter::new(&mut output, 4096, 5, 22);
                encoder.write_all(&body)?;
            }
            Bytes::from(output)
        }
        ResponseCompression::Gzip => {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(&body)?;
            Bytes::from(encoder.finish()?)
        }
        ResponseCompression::Deflate => {
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(&body)?;
            Bytes::from(encoder.finish()?)
        }
    };
    Ok((compressed, Some(encoding)))
}

// Adds compression response headers when a body was encoded.
fn add_compression_headers(
    mut builder: http::response::Builder,
    encoding: Option<ResponseCompression>,
) -> http::response::Builder {
    if let Some(encoding) = encoding {
        builder = builder.header(header::CONTENT_ENCODING, encoding.content_encoding());
        builder = builder.header(header::VARY, "accept-encoding");
    }
    builder
}

// Returns whether a TLS failure means the client rejected the presented cert.
fn is_client_certificate_unknown(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    error.contains("CertificateUnknown")
        || error.contains("UnknownCA")
        || lower.contains("certificate unknown")
        || lower.contains("unknown ca")
        || lower.contains("unknownca")
}

// Logs client-side certificate trust failures once per cooldown window.
fn api_ssl_log_client_tls_reject_with_cooldown(
    source_addr: SocketAddr,
    dest_addr: SocketAddr,
    key_exchange_policy: &str,
    error: &str,
) {
    let now = Utc::now().timestamp().max(0) as usize;
    let until_value = API_SSL_TCP_TLS_CLIENT_REJECT_LOG_UNTIL_EPOCH.load(Ordering::Relaxed);
    if now < until_value {
        API_SSL_TCP_TLS_CLIENT_REJECT_SUPPRESSED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    API_SSL_TCP_TLS_CLIENT_REJECT_LOG_UNTIL_EPOCH.store(
        now.saturating_add(DEF_SSL_REJECT_LOG_COOLDOWN_SECONDS as usize),
        Ordering::Relaxed,
    );
    let suppressed_count = API_SSL_TCP_TLS_CLIENT_REJECT_SUPPRESSED.swap(0, Ordering::Relaxed);
    api_ssl_log(json!({
        "event": "api_ssl_tcp_client_tls_rejected",
        "level": "warn",
        "reason": "client_rejected_certificate",
        "message": "client rejected the API SSL certificate during TLS handshake",
        "suppressed_since_last_log": suppressed_count,
        "log_cooldown_seconds": DEF_SSL_REJECT_LOG_COOLDOWN_SECONDS,
        "source_ip": socket_ip_for_log(source_addr.ip()),
        "source_port": source_addr.port(),
        "dest_ip": socket_ip_for_log(dest_addr.ip()),
        "dest_port": dest_addr.port(),
        "network_protocol": "tcp",
        "transport": "tcp_https",
        "key_exchange_policy": key_exchange_policy,
        "error": error,
    }));
}

// Logs overload rejections once per cooldown window with suppressed counts.
fn api_ssl_log_rejection_with_cooldown(
    protocol: &str,
    event: &str,
    active: usize,
    max_active: usize,
    source_addr: SocketAddr,
    dest_addr: SocketAddr,
) {
    let (until, suppressed, active_key, max_key) = if protocol == "tcp" {
        (
            &API_SSL_TCP_REJECT_LOG_UNTIL_EPOCH,
            &API_SSL_TCP_REJECT_SUPPRESSED,
            "active_tcp_connections",
            "max_active_tcp_connections",
        )
    } else {
        (
            &API_SSL_UDP_REJECT_LOG_UNTIL_EPOCH,
            &API_SSL_UDP_REJECT_SUPPRESSED,
            "active_udp_connections",
            "max_active_udp_connections",
        )
    };
    let now = Utc::now().timestamp().max(0) as usize;
    let until_value = until.load(Ordering::Relaxed);
    if now < until_value {
        suppressed.fetch_add(1, Ordering::Relaxed);
        return;
    }
    until.store(
        now.saturating_add(DEF_SSL_REJECT_LOG_COOLDOWN_SECONDS as usize),
        Ordering::Relaxed,
    );
    let suppressed_count = suppressed.swap(0, Ordering::Relaxed);
    let mut event = json!({
        "event": event,
        "level": "warn",
        "reason": "too_many_active_connections",
        "suppressed_since_last_log": suppressed_count,
        "log_cooldown_seconds": DEF_SSL_REJECT_LOG_COOLDOWN_SECONDS,
        "source_ip": socket_ip_for_log(source_addr.ip()),
        "source_port": source_addr.port(),
        "dest_ip": socket_ip_for_log(dest_addr.ip()),
        "dest_port": dest_addr.port(),
        "network_protocol": protocol,
        "transport": if protocol == "tcp" { "tcp_https" } else { "http3_quic" },
    });
    if let Some(object) = event.as_object_mut() {
        object.insert(active_key.to_string(), json!(active));
        object.insert(max_key.to_string(), json!(max_active));
    }
    api_ssl_log(event);
}

// Builds a Rustls server config with H3 ALPN and configured secure KX groups.
fn build_mlkem_h3_server_config(
    certs: Vec<rustls::pki_types::CertificateDer<'static>>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
    policy: &str,
) -> anyhow::Result<rustls::ServerConfig> {
    let mut provider = rustls::crypto::aws_lc_rs::default_provider();
    let (groups, _, _) = ssl_kx_groups(policy);
    provider.kx_groups = groups;
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(provider))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    config.alpn_protocols = vec![b"h3".to_vec()];
    Ok(config)
}

// Builds the TCP HTTPS TLS config for browsers and clients without H3.
fn build_mlkem_tcp_https_server_config(
    certs: Vec<rustls::pki_types::CertificateDer<'static>>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
    policy: &str,
) -> anyhow::Result<rustls::ServerConfig> {
    let mut provider = rustls::crypto::aws_lc_rs::default_provider();
    let (groups, _, _) = ssl_kx_groups(policy);
    provider.kx_groups = groups;
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(provider))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
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

// Formats host:port strings correctly for IPv4, IPv6, and DNS names.
fn host_port_for_socket_addrs(host: &str, port: u16) -> String {
    let host = host.trim();
    if host.starts_with('[') || !host.contains(':') {
        format!("{host}:{port}")
    } else {
        format!("[{host}]:{port}")
    }
}

// Converts IPv4-mapped IPv6 addresses to IPv4 for clearer logs.
fn socket_ip_for_log(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(|mapped| mapped.to_string())
            .unwrap_or_else(|| ip.to_string()),
    }
}

// Treats IPv4-mapped loopback as loopback for dual-stack sockets.
fn ip_is_loopback_or_mapped_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback(),
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(|mapped| mapped.is_loopback())
            .unwrap_or_else(|| ip.is_loopback()),
    }
}

// Resolves configured bind hosts while honoring enabled IP stacks.
fn resolve_bind_addrs(
    host: &str,
    port: u16,
    ipv4_enabled: bool,
    ipv6_enabled: bool,
    context: &str,
) -> anyhow::Result<Vec<SocketAddr>> {
    if !ipv4_enabled && !ipv6_enabled {
        anyhow::bail!("{context}: both IPv4 and IPv6 are disabled");
    }
    let host = host.trim();
    let mut addrs = if host.is_empty() || host == "0.0.0.0" || host == "::" || host == "[::]" {
        let mut wildcard = Vec::new();
        if ipv4_enabled {
            wildcard.push(SocketAddr::from(([0, 0, 0, 0], port)));
        }
        if ipv6_enabled {
            wildcard.push(SocketAddr::from(([0_u16; 8], port)));
        }
        wildcard
    } else {
        host_port_for_socket_addrs(host, port)
            .to_socket_addrs()
            .map_err(|err| anyhow::anyhow!("{context}: {err}"))?
            .collect::<Vec<_>>()
    };
    addrs.retain(|addr| match addr {
        SocketAddr::V4(_) => ipv4_enabled,
        SocketAddr::V6(_) => ipv6_enabled,
    });
    addrs.sort();
    addrs.dedup();
    if addrs.is_empty() {
        anyhow::bail!("{context}: no enabled socket addresses resolved");
    }
    Ok(addrs)
}

// Returns true when the remote SSL listener can accept non-loopback clients.
fn ssl_bind_allows_non_loopback(
    host: &str,
    port: u16,
    ipv4_enabled: bool,
    ipv6_enabled: bool,
) -> bool {
    resolve_bind_addrs(
        host,
        port,
        ipv4_enabled,
        ipv6_enabled,
        "remote SSL auth bind check",
    )
    .map(|addrs| {
        addrs
            .into_iter()
            .any(|addr| !ip_is_loopback_or_mapped_loopback(addr.ip()))
    })
    .unwrap_or(true)
}

// Rejects unsafe remote auth combinations before opening UDP to the network.
fn validate_ssl_remote_auth(status: &config::ApiSslRuntimeConfig) -> anyhow::Result<()> {
    if !ssl_bind_allows_non_loopback(
        &status.bind_host,
        status.udp_port,
        status.ipv4_enabled,
        status.ipv6_enabled,
    ) {
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
    let require_ech = status.ech_enabled && status.ech_require_dns_https_record;
    let check = check_https_dns_record(domain, status.udp_port, require_ech);
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

// Runs the TCP HTTPS listener for browsers and clients without H3.
async fn run_tcp_https_listener(
    status: config::ApiSslRuntimeConfig,
    state: Arc<ApiRuntimeState>,
) -> anyhow::Result<()> {
    let bind_addrs = resolve_bind_addrs(
        &status.tcp_bind_host,
        status.tcp_port,
        status.ipv4_enabled,
        status.ipv6_enabled,
        "unable to resolve API SSL/H3 TCP bind address",
    )?;
    let mut tasks = Vec::new();
    for bind_addr in bind_addrs {
        match start_tcp_https_listener(status.clone(), state.clone(), bind_addr).await {
            Ok(task) => tasks.push(task),
            Err(err) => api_ssl_log(json!({
                "event": "api_ssl_tcp_https_bind_failed",
                "level": "warn",
                "bind": bind_addr.to_string(),
                "network_protocol": "tcp",
                "transport": "tcp_https",
                "error": ssl_client_error_message(&err),
            })),
        }
    }
    if tasks.is_empty() {
        anyhow::bail!("API SSL/H3 TCP HTTPS listener could not bind any enabled IP stack");
    }
    for task in tasks {
        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => api_ssl_log(json!({
                "event": "api_ssl_tcp_https_failed",
                "level": "error",
                "network_protocol": "tcp",
                "transport": "tcp_https",
                "error": ssl_client_error_message(&err),
            })),
            Err(err) => api_ssl_log(json!({
                "event": "api_ssl_tcp_https_join_failed",
                "level": "error",
                "network_protocol": "tcp",
                "transport": "tcp_https",
                "error": err.to_string(),
            })),
        }
    }
    Ok(())
}

// Starts one TCP HTTPS accept loop on a concrete bind address.
async fn start_tcp_https_listener(
    status: config::ApiSslRuntimeConfig,
    state: Arc<ApiRuntimeState>,
    bind_addr: SocketAddr,
) -> anyhow::Result<tokio::task::JoinHandle<anyhow::Result<()>>> {
    let (certs, key) = load_rustls_cert_key(&status.cert_file, &status.key_file)?;
    let tls_config = build_mlkem_tcp_https_server_config(certs, key, &status.key_exchange_policy)?;
    let acceptor = TlsAcceptor::from(Arc::new(tls_config));
    let listener = TcpListener::bind(bind_addr).await?;
    let local_addr = listener.local_addr()?;
    api_ssl_log(json!({
        "event": "api_ssl_tcp_https_started",
        "level": "info",
        "pid": std::process::id(),
        "bind": local_addr.to_string(),
        "ip_stack": if local_addr.is_ipv4() { "ipv4" } else { "ipv6" },
        "network_protocol": "tcp",
        "transport": "tcp_https",
        "tls": {
            "version": "TLS1.3",
            "alpn": ["http/1.1"],
            "key_exchange_policy": status.key_exchange_policy.clone(),
            "key_exchange_groups": ssl_kx_group_labels(&status.key_exchange_policy),
            "fallback_to_classical": ssl_kx_policy_allows_classical_fallback(&status.key_exchange_policy),
        },
        "api_routes_allowed": true,
        "redirects_to": format!("h3=\":{}\"", status.udp_port),
    }));

    Ok(tokio::spawn(run_tcp_https_accept_loop(
        status, state, acceptor, listener, local_addr,
    )))
}

// Accepts TCP HTTPS connections for one bound socket.
async fn run_tcp_https_accept_loop(
    status: config::ApiSslRuntimeConfig,
    state: Arc<ApiRuntimeState>,
    acceptor: TlsAcceptor,
    listener: TcpListener,
    local_addr: SocketAddr,
) -> anyhow::Result<()> {
    loop {
        if TERMINATE.load(Ordering::SeqCst) || !config::api_ssl_runtime_config().enabled {
            break;
        }
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, source_addr) = accepted?;
                let dest_addr = stream.local_addr().unwrap_or(local_addr);
                let active = API_SSL_TCP_ACTIVE.fetch_add(1, Ordering::SeqCst) + 1;
                if active > DEF_SSL_TCP_MAX_ACTIVE_CONNECTIONS {
                    API_SSL_TCP_ACTIVE.fetch_sub(1, Ordering::SeqCst);
                    api_ssl_log_rejection_with_cooldown(
                        "tcp",
                        "api_ssl_tcp_connection_rejected",
                        active,
                        DEF_SSL_TCP_MAX_ACTIVE_CONNECTIONS,
                        source_addr,
                        dest_addr,
                    );
                    drop(stream);
                    continue;
                }
                let acceptor = acceptor.clone();
                let status = status.clone();
                let state = state.clone();
                tokio::spawn(async move {
                    let key_exchange_policy = status.key_exchange_policy.clone();
                    let result = handle_tcp_https_connection(
                        status,
                        state,
                        acceptor,
                        stream,
                        source_addr,
                        dest_addr,
                    )
                    .await;
                    API_SSL_TCP_ACTIVE.fetch_sub(1, Ordering::SeqCst);
                    if let Err(err) = result {
                        let error = ssl_client_error_message(&err);
                        if is_client_certificate_unknown(&error) {
                            api_ssl_log_client_tls_reject_with_cooldown(
                                source_addr,
                                dest_addr,
                                &key_exchange_policy,
                                &error,
                            );
                        } else {
                            api_ssl_log(json!({
                                "event": "api_ssl_tcp_https_failed",
                                "level": "error",
                                "source_ip": socket_ip_for_log(source_addr.ip()),
                                "source_port": source_addr.port(),
                                "dest_ip": socket_ip_for_log(dest_addr.ip()),
                                "dest_port": dest_addr.port(),
                                "network_protocol": "tcp",
                                "transport": "tcp_https",
                                "key_exchange_policy": key_exchange_policy,
                                "error": error,
                            }));
                        }
                    }
                });
            }
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
    }

    api_ssl_log(json!({
        "event": "api_ssl_tcp_https_stopped",
        "level": "info",
        "pid": std::process::id(),
        "bind": local_addr.to_string(),
    }));
    Ok(())
}

// Handles one TCP HTTPS request and dispatches through the remote API surface.
async fn handle_tcp_https_connection(
    status: config::ApiSslRuntimeConfig,
    state: Arc<ApiRuntimeState>,
    acceptor: TlsAcceptor,
    stream: TcpStream,
    source_addr: SocketAddr,
    dest_addr: SocketAddr,
) -> anyhow::Result<()> {
    let started = Instant::now();
    let limits = config::api_limit_config();
    let mut tls_stream = timeout(Duration::from_secs(5), acceptor.accept(stream)).await??;
    let mut request = Vec::with_capacity(4096);
    let mut buf = [0_u8; 2048];
    let mut header_end = None;
    while request.len() < DEF_SSL_TCP_MAX_HEADER_BYTES {
        let read = timeout(Duration::from_secs(2), tls_stream.read(&mut buf)).await??;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buf[..read]);
        if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            header_end = Some((index, 4));
            break;
        }
        if let Some(index) = request.windows(2).position(|window| window == b"\n\n") {
            header_end = Some((index, 2));
            break;
        }
    }
    let Some((header_index, header_terminator_len)) = header_end else {
        anyhow::bail!(
            "HTTP request headers exceeded {DEF_SSL_TCP_MAX_HEADER_BYTES} bytes or were incomplete"
        );
    };
    let request_text = String::from_utf8_lossy(&request[..header_index]);
    let mut lines = request_text.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut headers = Vec::<(String, String)>::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    let user_agent = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
        .map(|(_, value)| log_header_value(Some(value)));
    let forwarded_client = forwarded_client_fields_from_pairs(&status, &headers, source_addr.ip());
    let accepted_compression = accepted_response_compression(
        headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("accept-encoding"))
            .map(|(_, value)| value.as_str()),
    );
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let path = parts.next().unwrap_or("/");
    let mut body = Vec::new();
    let content_length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    if content_length > limits.max_body_bytes {
        let response = api_error(StatusCode::PAYLOAD_TOO_LARGE, "request body too large");
        let status_code = response.status().as_u16();
        let response = tcp_https_response(&status, response, method, accepted_compression).await?;
        tls_stream.write_all(&response).await?;
        let _ = tls_stream.shutdown().await;
        api_ssl_log_with_client(
            json!({
                "event": "api_ssl_tcp_https_request",
                "level": "warn",
                "method": method,
                "path": path,
                "status": status_code,
                "duration_ms": started.elapsed().as_millis(),
                "source_ip": socket_ip_for_log(source_addr.ip()),
                "source_port": source_addr.port(),
                "dest_ip": socket_ip_for_log(dest_addr.ip()),
                "dest_port": dest_addr.port(),
                "network_protocol": "tcp",
                "transport": "tcp_https",
                "key_exchange_policy": status.key_exchange_policy.clone(),
                "user_agent": user_agent.unwrap_or_else(|| "not available".to_string()),
                "error": "request body too large",
                "api_routes_allowed": true,
            }),
            &forwarded_client,
        );
        return Ok(());
    }
    let body_start = header_index + header_terminator_len;
    body.extend_from_slice(&request[body_start..]);
    while body.len() < content_length {
        let read = timeout(Duration::from_secs(2), tls_stream.read(&mut buf)).await??;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&buf[..read]);
        if body.len() > limits.max_body_bytes {
            break;
        }
    }
    body.truncate(content_length);
    let method = method.parse::<Method>().unwrap_or(Method::GET);
    let path_and_query = if path.starts_with('/') { path } else { "/" };
    let uri = path_and_query
        .parse::<Uri>()
        .unwrap_or_else(|_| Uri::from_static("/"));
    let mut request_builder = http::Request::builder()
        .method(method.clone())
        .uri(uri.clone());
    for (name, value) in &headers {
        let Ok(header_name) = header::HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
        let Ok(header_value) = header::HeaderValue::from_str(value) else {
            continue;
        };
        request_builder = request_builder.header(header_name, header_value);
    }
    let request_for_auth = request_builder.body(())?;
    let reads_asset = matches!(method, Method::GET | Method::HEAD);
    if method == Method::GET && uri.path() == "/events/stream" {
        if let Err(response) = authorize_remote_request(&request_for_auth, source_addr) {
            let status_code = response.status().as_u16();
            let response =
                tcp_https_response(&status, response, "GET", accepted_compression).await?;
            tls_stream.write_all(&response).await?;
            let _ = tls_stream.shutdown().await;
            api_ssl_log_with_client(
                json!({
                    "event": "api_ssl_realtime_stream_rejected",
                    "level": "warn",
                    "method": "GET",
                    "path": path_and_query,
                    "status": status_code,
                    "duration_ms": started.elapsed().as_millis(),
                    "source_ip": socket_ip_for_log(source_addr.ip()),
                    "source_port": source_addr.port(),
                    "dest_ip": socket_ip_for_log(dest_addr.ip()),
                    "dest_port": dest_addr.port(),
                    "network_protocol": "tcp",
                    "transport": "tcp_https_sse",
                    "key_exchange_policy": status.key_exchange_policy.clone(),
                    "user_agent": user_agent.unwrap_or_else(|| "not available".to_string()),
                    "api_routes_allowed": true,
                    "error": "authentication required",
                }),
                &forwarded_client,
            );
            return Ok(());
        }
        handle_tcp_https_realtime_stream(
            &status,
            state,
            &mut tls_stream,
            started,
            path_and_query,
            source_addr,
            dest_addr,
            user_agent.unwrap_or_else(|| "not available".to_string()),
            forwarded_client,
        )
        .await?;
        return Ok(());
    }
    let response = if let Err(response) = authorize_remote_request(&request_for_auth, source_addr) {
        response
    } else if reads_asset && path_and_query == "/robots.txt" {
        serve_webapp_asset(path_and_query)
            .unwrap_or_else(|| api_error(StatusCode::NOT_FOUND, "webapp asset not found"))
    } else if reads_asset {
        if let Some(response) = serve_webapp_asset(path_and_query) {
            response
        } else {
            handle_remote_api_request(state, method.clone(), uri, Bytes::from(body)).await
        }
    } else {
        handle_remote_api_request(state, method.clone(), uri, Bytes::from(body)).await
    };
    let status_code = response.status().as_u16();
    let response =
        tcp_https_response(&status, response, method.as_str(), accepted_compression).await?;
    tls_stream.write_all(&response).await?;
    let _ = tls_stream.shutdown().await;
    api_ssl_log_with_client(
        json!({
            "event": "api_ssl_tcp_https_request",
            "level": "info",
            "method": method.as_str(),
            "path": path_and_query,
            "status": status_code,
            "duration_ms": started.elapsed().as_millis(),
            "source_ip": socket_ip_for_log(source_addr.ip()),
            "source_port": source_addr.port(),
            "dest_ip": socket_ip_for_log(dest_addr.ip()),
            "dest_port": dest_addr.port(),
            "network_protocol": "tcp",
            "transport": "tcp_https",
            "key_exchange_policy": status.key_exchange_policy.clone(),
            "user_agent": user_agent.unwrap_or_else(|| "not available".to_string()),
            "api_routes_allowed": true,
        }),
        &forwarded_client,
    );
    Ok(())
}

// Streams dashboard refresh hints over browser-compatible HTTPS SSE.
async fn handle_tcp_https_realtime_stream(
    status: &config::ApiSslRuntimeConfig,
    state: Arc<ApiRuntimeState>,
    tls_stream: &mut tokio_rustls::server::TlsStream<TcpStream>,
    started: Instant,
    path: &str,
    source_addr: SocketAddr,
    dest_addr: SocketAddr,
    user_agent: String,
    forwarded_client: ForwardedClientLogFields,
) -> anyhow::Result<()> {
    let alt_svc = format!("h3=\":{}\"; ma=86400", status.udp_port);
    let limits = config::api_limit_config();
    if let Err(response) = check_api_rate_limit(&state, &limits, "GET", path, started, None).await {
        let response = tcp_https_response(status, response, "GET", None).await?;
        tls_stream.write_all(&response).await?;
        let _ = tls_stream.shutdown().await;
        return Ok(());
    }
    let Some(_guard) = ApiRealtimeStreamGuard::try_new(state.clone()) else {
        let response = api_backoff_logged(
            "max_realtime_streams_exceeded",
            config::api_limit_config().overload_retry_after_seconds,
            "GET",
            path,
            started,
            None,
        );
        let response = tcp_https_response(status, response, "GET", None).await?;
        tls_stream.write_all(&response).await?;
        let _ = tls_stream.shutdown().await;
        return Ok(());
    };
    let headers = format!(
        "HTTP/1.1 200 OK\r\n\
         content-type: text/event-stream; charset=utf-8\r\n\
         cache-control: no-store\r\n\
         connection: close\r\n\
         alt-svc: {alt_svc}\r\n\
         strict-transport-security: max-age=31536000\r\n\
         x-content-type-options: nosniff\r\n\
         x-frame-options: DENY\r\n\
         referrer-policy: no-referrer\r\n\
         x-robots-tag: noindex, nofollow, noai, noimageai\r\n\
         permissions-policy: geolocation=(), microphone=(), camera=(), payment=()\r\n\
         content-security-policy: default-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'\r\n\
         \r\n"
    );
    tls_stream.write_all(headers.as_bytes()).await?;
    let connected = sse_event_bytes(
        "connected",
        0,
        realtime_event_payload(&state, "connected", "tcp_https_sse", 0),
    )?;
    tls_stream.write_all(&connected).await?;
    tls_stream.flush().await?;
    count_realtime_event(&state);
    api_ssl_log_with_client(
        json!({
            "event": "api_ssl_realtime_stream_started",
            "level": "info",
            "method": "GET",
            "path": path,
            "status": 200,
            "duration_ms": started.elapsed().as_millis(),
            "source_ip": socket_ip_for_log(source_addr.ip()),
            "source_port": source_addr.port(),
            "dest_ip": socket_ip_for_log(dest_addr.ip()),
            "dest_port": dest_addr.port(),
            "network_protocol": "tcp",
            "transport": "tcp_https_sse",
            "key_exchange_policy": status.key_exchange_policy.clone(),
            "user_agent": user_agent.clone(),
            "api_routes_allowed": true,
            "interval_seconds": DEF_REALTIME_STREAM_INTERVAL_SECONDS,
            "heartbeat_seconds": DEF_REALTIME_STREAM_HEARTBEAT_SECONDS,
            "max_stream_seconds": DEF_REALTIME_STREAM_MAX_SECONDS,
        }),
        &forwarded_client,
    );

    let max_events =
        (DEF_REALTIME_STREAM_MAX_SECONDS / DEF_REALTIME_STREAM_HEARTBEAT_SECONDS).max(1);
    let refresh_every =
        (DEF_REALTIME_STREAM_INTERVAL_SECONDS / DEF_REALTIME_STREAM_HEARTBEAT_SECONDS).max(1);
    let mut sent_refresh_events = 0_u64;
    let mut sent_heartbeat_events = 0_u64;
    let mut disconnect_error = None::<String>;
    for sequence in 1..=max_events {
        if TERMINATE.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_secs(DEF_REALTIME_STREAM_HEARTBEAT_SECONDS)).await;
        let event_name = if sequence % refresh_every == 0 {
            "dashboard.refresh"
        } else {
            "heartbeat"
        };
        let frame = sse_event_bytes(
            event_name,
            usize::try_from(sequence).unwrap_or(usize::MAX),
            realtime_event_payload(
                &state,
                event_name,
                "tcp_https_sse",
                usize::try_from(sequence).unwrap_or(usize::MAX),
            ),
        )?;
        if let Err(err) = tls_stream.write_all(&frame).await {
            disconnect_error = Some(err.to_string());
            break;
        }
        if let Err(err) = tls_stream.flush().await {
            disconnect_error = Some(err.to_string());
            break;
        }
        if event_name == "dashboard.refresh" {
            sent_refresh_events += 1;
        } else {
            sent_heartbeat_events += 1;
        }
        count_realtime_event(&state);
    }
    let _ = tls_stream.shutdown().await;
    api_ssl_log_with_client(
        json!({
            "event": "api_ssl_realtime_stream_closed",
            "level": if disconnect_error.is_some() { "warn" } else { "info" },
            "method": "GET",
            "path": path,
            "status": 200,
            "duration_ms": started.elapsed().as_millis(),
            "source_ip": socket_ip_for_log(source_addr.ip()),
            "source_port": source_addr.port(),
            "dest_ip": socket_ip_for_log(dest_addr.ip()),
            "dest_port": dest_addr.port(),
            "network_protocol": "tcp",
            "transport": "tcp_https_sse",
            "key_exchange_policy": status.key_exchange_policy.clone(),
            "user_agent": user_agent,
            "api_routes_allowed": true,
            "refresh_events": sent_refresh_events,
            "heartbeat_events": sent_heartbeat_events,
            "error": disconnect_error.unwrap_or_else(|| "not available".to_string()),
        }),
        &forwarded_client,
    );
    Ok(())
}

// Converts an Axum response into a minimal HTTP/1.1 response.
async fn tcp_https_response(
    status: &config::ApiSslRuntimeConfig,
    response: Response,
    method: &str,
    accepted_compression: Option<ResponseCompression>,
) -> anyhow::Result<Vec<u8>> {
    let alt_svc = format!("h3=\":{}\"; ma=86400", status.udp_port);
    let status_code = response.status().as_u16();
    let reason = response.status().canonical_reason().unwrap_or("OK");
    let (parts, body) = response.into_parts();
    let body_bytes = to_bytes(body, DEF_API_RESPONSE_MAX_BYTES).await?;
    let already_encoded = parts.headers.contains_key(header::CONTENT_ENCODING);
    let (body_bytes, applied_compression) =
        maybe_compress_body(body_bytes, accepted_compression, already_encoded)?;
    let body_len = if method == "HEAD" {
        0
    } else {
        body_bytes.len()
    };
    let mut headers = String::new();
    headers.push_str(&format!(
        "HTTP/1.1 {} {}\r\nalt-svc: {}\r\ncontent-length: {}\r\nconnection: close\r\n",
        status_code, reason, alt_svc, body_len
    ));
    headers.push_str("strict-transport-security: max-age=31536000\r\n");
    headers.push_str("x-content-type-options: nosniff\r\n");
    headers.push_str("x-frame-options: DENY\r\n");
    headers.push_str("referrer-policy: no-referrer\r\n");
    headers.push_str("x-robots-tag: noindex, nofollow, noai, noimageai\r\n");
    headers
        .push_str("permissions-policy: geolocation=(), microphone=(), camera=(), payment=()\r\n");
    headers.push_str("content-security-policy: default-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'\r\n");
    for (name, value) in parts.headers {
        if let Some(name) = name {
            let header_name = name.as_str();
            if name == header::CONTENT_LENGTH
                || name == header::CONNECTION
                || header_name.eq_ignore_ascii_case("alt-svc")
                || header_name.eq_ignore_ascii_case("strict-transport-security")
                || header_name.eq_ignore_ascii_case("x-content-type-options")
                || header_name.eq_ignore_ascii_case("x-frame-options")
                || header_name.eq_ignore_ascii_case("referrer-policy")
                || header_name.eq_ignore_ascii_case("x-robots-tag")
                || header_name.eq_ignore_ascii_case("permissions-policy")
                || header_name.eq_ignore_ascii_case("content-security-policy")
                || (applied_compression.is_some() && name == header::CONTENT_ENCODING)
                || (applied_compression.is_some() && name == header::VARY)
            {
                continue;
            }
            if let Ok(value) = value.to_str() {
                headers.push_str(header_name);
                headers.push_str(": ");
                headers.push_str(&value.replace(['\r', '\n'], " "));
                headers.push_str("\r\n");
            }
        }
    }
    if let Some(encoding) = applied_compression {
        headers.push_str("content-encoding: ");
        headers.push_str(encoding.content_encoding());
        headers.push_str("\r\n");
        headers.push_str("vary: accept-encoding\r\n");
    }
    headers.push_str("\r\n");
    let mut response = headers.into_bytes();
    if method != "HEAD" {
        response.extend_from_slice(&body_bytes);
    }
    Ok(response)
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
            "API SSL/H3 certificate files are missing. Run `mlai-trade api ssl cert generate --target h3` first."
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
        .arg("--json")
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
    let mut child = command.spawn()?;
    std::thread::sleep(Duration::from_millis(300));
    if let Some(exit_status) = child.try_wait()? {
        anyhow::bail!(
            "API SSL/H3 child exited immediately with status {exit_status}; check {}",
            status.log_file.display()
        );
    }
    if json_out {
        print_json(json!({
            "status": "started",
            "pid": child.id(),
            "udp_port": status.udp_port,
            "tcp_enabled": status.tcp_enabled,
            "tcp_port": status.tcp_port,
            "log_file": status.log_file.display().to_string(),
        }))?;
    } else {
        println!("API SSL/H3 started with pid {}.", child.id());
        println!(
            "UDP: {} (IPv4={} IPv6={})",
            host_port_for_socket_addrs(&status.bind_host, status.udp_port),
            status.ipv4_enabled,
            status.ipv6_enabled
        );
        if status.tcp_enabled {
            println!(
                "TCP HTTPS: {}",
                host_port_for_socket_addrs(&status.tcp_bind_host, status.tcp_port)
            );
        }
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
            "ech": check.required_ech,
        },
        "answers": check.answers.iter().map(|answer| json!({
            "priority": answer.priority,
            "target": answer.target.clone(),
            "alpn": answer.alpn.clone(),
            "port": answer.port.unwrap_or(443),
            "ech_present": answer.ech.is_some(),
            "ech_length": answer.ech.as_ref().map(|value| value.len()).unwrap_or(0),
            "ech_base64": answer.ech.as_ref().map(|value| {
                base64::engine::general_purpose::STANDARD.encode(value)
            }),
            "ttl": answer.ttl,
        })).collect::<Vec<_>>(),
        "errors": check.errors.clone(),
    })
}

// Shows configured remote HTTP/3 API status.
pub fn cmd_ssl_status(json_out: bool, details: bool) -> anyhow::Result<()> {
    let status = config::api_ssl_runtime_config();
    let mut payload = ssl_status_json(&status);
    let health = if details
        && read_pid(&status.pid_file)
            .map(process_alive)
            .unwrap_or(false)
    {
        fetch_api_ssl_health_snapshot()
    } else {
        None
    };
    if details {
        payload["details"] = health.clone().unwrap_or_else(|| json!("not available"));
        payload["configured_resources"] = config::runtime_resources_json();
        payload["accelerators"] = accelerators::accelerator_status_json();
    }
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
        "  IP stacks:    IPv4={} IPv6={}",
        status.ipv4_enabled, status.ipv6_enabled
    );
    if status.tcp_enabled {
        println!(
            "  TCP HTTPS:    enabled on {} (dashboard/API + Alt-Svc h3)",
            host_port_for_socket_addrs(&status.tcp_bind_host, status.tcp_port)
        );
    } else {
        println!("  TCP HTTPS:    disabled");
    }
    println!(
        "  Domain:       {}",
        if status.domain.is_empty() {
            "not configured"
        } else {
            &status.domain
        }
    );
    println!(
        "  ECH:          {}",
        if status.ech_enabled {
            "requested but unsupported by current server TLS stack (fails closed)"
        } else {
            "disabled"
        }
    );
    if status.ech_enabled {
        println!(
            "    Public name: {}",
            if status.ech_public_name.is_empty() {
                "not configured"
            } else {
                &status.ech_public_name
            }
        );
        println!(
            "    Config file: {} ({})",
            status.ech_config_file.display(),
            if status.ech_config_file.exists() {
                "exists"
            } else {
                "missing"
            }
        );
        println!(
            "    Key file:    {} ({})",
            status.ech_key_file.display(),
            if status.ech_key_file.exists() {
                "exists"
            } else {
                "missing"
            }
        );
        println!(
            "    DNS check:   {}",
            if status.ech_require_dns_https_record {
                "requires HTTPS/SVCB ech parameter"
            } else {
                "ECH DNS requirement disabled"
            }
        );
    }
    println!("  TLS:          TLS 1.3 only, ALPN h3 and http/1.1");
    println!("  Key exchange: {}", status.key_exchange_policy);
    println!(
        "    Groups:     {}",
        ssl_kx_group_labels(&status.key_exchange_policy).join(", ")
    );
    println!(
        "    Fallback:   {}",
        if ssl_kx_policy_allows_classical_fallback(&status.key_exchange_policy) {
            "enabled for strong TLS 1.3 groups only"
        } else {
            "disabled"
        }
    );
    println!("  Certificate:  {}", status.cert_mode);
    if let Ok(certs) = ssl_cert_info_values(&status, "all") {
        for cert in certs {
            println!(
                "  Cert {}:      status={} days_left={} auto_renew={}",
                cert["target"].as_str().unwrap_or("unknown"),
                cert["status"].as_str().unwrap_or("unknown"),
                cert["days_remaining"]
                    .as_i64()
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "not available".to_string()),
                cert["auto_renew_allowed"]
                    .as_bool()
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "false".to_string())
            );
        }
    }
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
    println!(
        "  Trusted proxy: {} ({})",
        if status.trusted_proxy_enabled {
            "enabled"
        } else {
            "disabled"
        },
        if status.trusted_proxy_cidrs.is_empty() {
            "no CIDRs configured".to_string()
        } else {
            status.trusted_proxy_cidrs.join(", ")
        }
    );
    println!("  PID file:     {}", status.pid_file.display());
    println!(
        "  Status file:  {}",
        api_ssl_runtime_status_file().display()
    );
    println!("  Log file:     {}", status.log_file.display());
    if status.cert_mode == "letsencrypt" && status.tcp_acme_tls_alpn_enabled {
        println!(
            "  TCP challenge: ACME TLS-ALPN-01 challenge only on {}:{}",
            status.tcp_acme_bind_host, status.tcp_acme_port
        );
    } else {
        println!(
            "  TCP API:      {}",
            if status.tcp_enabled {
                "enabled for HTTPS dashboard/API traffic"
            } else {
                "disabled"
            }
        );
    }
    println!("  Data plane:   HTTP/3 over QUIC and TCP HTTPS listeners implemented");
    if details {
        print_api_details("SSL/H3 Runtime", health.as_ref());
    }
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
    let require_ech = configured.ech_enabled && configured.ech_require_dns_https_record;
    let check = check_https_dns_record(&domain, configured.udp_port, require_ech);
    let payload = https_dns_check_json(&check);
    if json_out {
        print_json(payload)?;
        return Ok(());
    }
    println!("API SSL/H3 DNS HTTPS Check");
    println!("  Domain:   {}", check.domain);
    println!("  Resolver: {}", check.resolver);
    println!(
        "  Required: HTTPS/SVCB alpn=h3, port={}, no h2/http/1.1 fallback{}",
        check.required_port,
        if check.required_ech {
            ", ech present"
        } else {
            ""
        }
    );
    if check.answers.is_empty() {
        println!("  Answers:  none");
    } else {
        for answer in &check.answers {
            println!(
                "  Answer:   priority={} target={} alpn={:?} port={} ech={} ttl={}",
                answer.priority,
                answer.target,
                answer.alpn,
                answer.port.unwrap_or(443),
                answer
                    .ech
                    .as_ref()
                    .map(|value| format!("yes ({} bytes)", value.len()))
                    .unwrap_or_else(|| "no".to_string()),
                answer.ttl
            );
        }
    }
    if !check.errors.is_empty() {
        println!("  Errors:   {}", check.errors.join("; "));
    }
    println!("  Result:   {}", if check.ok { "ok" } else { "not ready" });
    if !check.ok {
        if check.required_ech {
            println!(
                "  Fix:      publish an HTTPS/SVCB record advertising only ALPN h3 on port {} and ech=<ECHConfigList base64>",
                check.required_port
            );
        } else {
            println!(
                "  Fix:      publish an HTTPS/SVCB record advertising only ALPN h3 on port {}",
                check.required_port
            );
        }
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
        tcp_enabled: status.tcp_enabled,
        tcp_bind_host: status.tcp_bind_host,
        tcp_port: status.tcp_port,
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

// Reads the remote SSL/H3 runtime status written by the SSL API process.
fn fetch_api_ssl_health_snapshot() -> Option<Value> {
    let raw = fs::read_to_string(api_ssl_runtime_status_file()).ok()?;
    serde_json::from_str(raw.trim()).ok()
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

// Formats fraction metrics as percentages for status output.
fn percent_text(value: Option<&Value>, decimals: usize) -> String {
    value
        .and_then(Value::as_f64)
        .map(|number| format!("{:.decimals$}", number * 100.0))
        .unwrap_or_else(|| metric_text(value))
}

// Prints live API runtime counters and resource usage.
fn print_api_details(label: &str, health: Option<&Value>) {
    let Some(runtime) = health.and_then(|value| value.get("runtime")) else {
        println!("  {label}: not available");
        return;
    };
    println!("  {label}:");
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
    if let Some(cache) = runtime
        .get("cache")
        .and_then(|value| value.get("market_bars"))
    {
        println!(
            "    Market bars cache: requests={}, results={}, cache_hits={}, provider_fetches={}, empty={}, rows_stored={}",
            metric_text(cache.get("api_requests")),
            metric_text(cache.get("results")),
            metric_text(cache.get("cache_hits")),
            metric_text(cache.get("provider_fetches")),
            metric_text(cache.get("empty_results")),
            metric_text(cache.get("cache_rows_stored")),
        );
        println!(
            "      hit_rate={}%, provider_rate={}%",
            percent_text(cache.get("cache_hit_rate"), 1),
            percent_text(cache.get("provider_fetch_rate"), 1),
        );
    }
    if let Some(realtime) = runtime.get("realtime") {
        println!(
            "    Realtime: active_streams={}, total_streams={}, events_sent={}, interval={}s",
            metric_text(realtime.get("active_streams")),
            metric_text(realtime.get("total_streams")),
            metric_text(realtime.get("events_sent")),
            metric_text(realtime.get("interval_seconds")),
        );
    }
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
    let ssl_status = config::api_ssl_runtime_config();
    let ssl_running = read_pid(&ssl_status.pid_file)
        .map(process_alive)
        .unwrap_or(false);
    let health = if details && status.running {
        fetch_health_snapshot(&status.socket_file)
    } else {
        None
    };
    let ssl_health = if details && ssl_running {
        fetch_api_ssl_health_snapshot()
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
            payload["ssl_details"] = ssl_health.unwrap_or_else(|| json!("not available"));
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
        print_api_details("Unix Runtime", health.as_ref());
        print_api_details("SSL/H3 Runtime", ssl_health.as_ref());
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

    let state = Arc::new(ApiRuntimeState::new("mlai-trade-api-unix"));
    let app = Router::new()
        .route("/health", get(handle_health))
        .route("/limits", get(handle_limits))
        .route("/routes", get(handle_routes))
        .route("/events/snapshot", get(handle_events_snapshot))
        .route("/{section}/{action}", get(handle_two).post(handle_two))
        .route(
            "/{section}/{action}/{target}",
            get(handle_three).post(handle_three),
        )
        .with_state(state)
        .layer(
            CompressionLayer::new()
                .zstd(true)
                .br(true)
                .gzip(true)
                .deflate(true),
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

// Handles the remote SSL/H3 API run loop.
async fn start_h3_endpoint(
    status: config::ApiSslRuntimeConfig,
    state: Arc<ApiRuntimeState>,
    bind_addr: SocketAddr,
    auto_renewed_certs: Vec<Value>,
) -> anyhow::Result<tokio::task::JoinHandle<anyhow::Result<()>>> {
    let (certs, key) = load_rustls_cert_key(&status.cert_file, &status.key_file)?;
    let rustls_config = build_mlkem_h3_server_config(certs, key, &status.key_exchange_policy)?;
    let quic_config = quinn::crypto::rustls::QuicServerConfig::try_from(rustls_config)?;
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_config));
    let endpoint = quinn::Endpoint::server(server_config, bind_addr)?;
    let local_addr = endpoint.local_addr()?;
    api_ssl_log(json!({
        "event": "api_ssl_server_started",
        "level": "info",
        "pid": std::process::id(),
        "bind": local_addr.to_string(),
        "ip_stack": if local_addr.is_ipv4() { "ipv4" } else { "ipv6" },
        "network_protocol": "udp",
        "transport": "http3_quic",
        "tls": {
            "version": "TLS1.3",
            "alpn": ["h3"],
            "key_exchange_policy": status.key_exchange_policy.clone(),
            "key_exchange_groups": ssl_kx_group_labels(&status.key_exchange_policy),
            "fallback_to_classical": ssl_kx_policy_allows_classical_fallback(&status.key_exchange_policy),
        },
        "tcp_https": {
            "enabled": status.tcp_enabled,
            "bind": host_port_for_socket_addrs(&status.tcp_bind_host, status.tcp_port),
            "api_routes_allowed": true,
        },
        "auto_renewed_certs": auto_renewed_certs,
        "cert_file": status.cert_file.display().to_string(),
        "key_file": status.key_file.display().to_string(),
    }));
    Ok(tokio::spawn(run_h3_endpoint_loop(
        status, state, endpoint, local_addr,
    )))
}

// Accepts H3/QUIC connections for one bound UDP socket.
async fn run_h3_endpoint_loop(
    status: config::ApiSslRuntimeConfig,
    state: Arc<ApiRuntimeState>,
    endpoint: quinn::Endpoint,
    local_addr: SocketAddr,
) -> anyhow::Result<()> {
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
                let remote_addr = incoming.remote_address();
                let dest_addr = SocketAddr::new(incoming.local_ip().unwrap_or(local_addr.ip()), local_addr.port());
                let active = API_SSL_UDP_ACTIVE.fetch_add(1, Ordering::SeqCst) + 1;
                if active > DEF_SSL_UDP_MAX_ACTIVE_CONNECTIONS {
                    API_SSL_UDP_ACTIVE.fetch_sub(1, Ordering::SeqCst);
                    api_ssl_log_rejection_with_cooldown(
                        "udp",
                        "api_ssl_udp_connection_rejected",
                        active,
                        DEF_SSL_UDP_MAX_ACTIVE_CONNECTIONS,
                        remote_addr,
                        dest_addr,
                    );
                    continue;
                }
                let state = state.clone();
                let status = status.clone();
                tokio::spawn(async move {
                    let result = handle_h3_connection(state, incoming, dest_addr, status).await;
                    API_SSL_UDP_ACTIVE.fetch_sub(1, Ordering::SeqCst);
                    if let Err(err) = result {
                        api_ssl_log(json!({
                            "event": "api_ssl_connection_failed",
                            "level": "error",
                            "network_protocol": "udp",
                            "transport": "http3_quic",
                            "error": ssl_client_error_message(&err),
                        }));
                    }
                });
            }
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
    }
    endpoint.close(0u32.into(), b"shutdown");
    api_ssl_log(json!({
        "event": "api_ssl_server_stopped",
        "level": "info",
        "pid": std::process::id(),
        "bind": local_addr.to_string(),
        "network_protocol": "udp",
        "transport": "http3_quic",
    }));
    let _ = status;
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
    if status.ech_enabled {
        api_ssl_log(json!({
            "event": "api_ssl_ech_unsupported",
            "level": "error",
            "status": "fail_closed",
            "rfc": ["RFC9849", "RFC9848"],
            "listener_stack": "rustls_quinn",
            "supported_by_current_listener": false,
            "ech_config_file": status.ech_config_file.display().to_string(),
            "ech_key_file": status.ech_key_file.display().to_string(),
            "ech_dns_required": status.ech_require_dns_https_record,
            "message": "api.ssl.ech.enabled=true but the current Rustls/Quinn server path does not expose server-side ECH support",
        }));
        anyhow::bail!(
            "api.ssl.ech.enabled=true but server-side ECH is not supported by the current Rustls/Quinn server path; disable api.ssl.ech.enabled until ECH server support is available"
        );
    }
    let auto_renewed_certs = maybe_auto_renew_ssl_certs(&status)?;
    let state = Arc::new(ApiRuntimeState::new("mlai-trade-api-ssl"));
    state.write_ssl_status_file();
    let h3_bind_addrs = resolve_bind_addrs(
        &status.bind_host,
        status.udp_port,
        status.ipv4_enabled,
        status.ipv6_enabled,
        "unable to resolve API SSL/H3 bind address",
    )?;
    let mut h3_tasks = Vec::new();
    for bind_addr in h3_bind_addrs {
        match start_h3_endpoint(
            status.clone(),
            state.clone(),
            bind_addr,
            auto_renewed_certs.clone(),
        )
        .await
        {
            Ok(task) => h3_tasks.push(task),
            Err(err) => api_ssl_log(json!({
                "event": "api_ssl_udp_bind_failed",
                "level": "warn",
                "bind": bind_addr.to_string(),
                "network_protocol": "udp",
                "transport": "http3_quic",
                "error": ssl_client_error_message(&err),
            })),
        }
    }
    if h3_tasks.is_empty() {
        anyhow::bail!("API SSL/H3 UDP listener could not bind any enabled IP stack");
    }
    let tcp_task = if status.tcp_enabled {
        Some(tokio::spawn(run_tcp_https_listener(
            status.clone(),
            state.clone(),
        )))
    } else {
        None
    };
    paths::write_runtime_metadata_file(&status.pid_file, std::process::id().to_string())?;
    state.write_ssl_status_file();
    for task in h3_tasks {
        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => api_ssl_log(json!({
                "event": "api_ssl_udp_listener_failed",
                "level": "error",
                "network_protocol": "udp",
                "transport": "http3_quic",
                "error": ssl_client_error_message(&err),
            })),
            Err(err) => api_ssl_log(json!({
                "event": "api_ssl_udp_join_failed",
                "level": "error",
                "network_protocol": "udp",
                "transport": "http3_quic",
                "error": err.to_string(),
            })),
        }
    }
    if let Some(task) = tcp_task {
        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => api_ssl_log(json!({
                "event": "api_ssl_tcp_https_failed",
                "level": "error",
                "network_protocol": "tcp",
                "transport": "tcp_https",
                "error": ssl_client_error_message(&err),
            })),
            Err(err) => api_ssl_log(json!({
                "event": "api_ssl_tcp_https_join_failed",
                "level": "error",
                "error": err.to_string(),
            })),
        }
    }
    let current_pid = std::process::id();
    if read_pid(&status.pid_file) == Some(current_pid) {
        let _ = fs::remove_file(&status.pid_file);
    }
    let _ = fs::remove_file(api_ssl_runtime_status_file());
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
    status: config::ApiSslRuntimeConfig,
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
        "source_ip": socket_ip_for_log(source_addr.ip()),
        "source_port": source_addr.port(),
        "dest_ip": socket_ip_for_log(dest_addr.ip()),
        "dest_port": dest_addr.port(),
        "network_protocol": "udp",
        "transport": "http3_quic",
        "alpn": protocol,
    }));
    let mut h3_conn = h3::server::builder()
        .build(h3_quinn::Connection::new(connection))
        .await?;
    while let Some(resolver) = h3_conn.accept().await? {
        let state = state.clone();
        let status = status.clone();
        tokio::spawn(async move {
            if let Err(err) =
                handle_h3_request(state, resolver, source_addr, dest_addr, status).await
            {
                api_ssl_log(json!({
                    "event": "api_ssl_request_failed",
                    "level": "error",
                    "source_ip": socket_ip_for_log(source_addr.ip()),
                    "source_port": source_addr.port(),
                    "dest_ip": socket_ip_for_log(dest_addr.ip()),
                    "dest_port": dest_addr.port(),
                    "network_protocol": "udp",
                    "transport": "http3_quic",
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
    status: config::ApiSslRuntimeConfig,
) -> anyhow::Result<()> {
    use bytes::Buf;
    let started = Instant::now();
    let (request, mut stream) = resolver.resolve_request().await?;
    let method = request.method().clone();
    let uri = request.uri().clone();
    let path = uri.path().to_string();
    let user_agent = log_header_value(
        request
            .headers()
            .get(header::USER_AGENT)
            .and_then(|value| value.to_str().ok()),
    );
    let forwarded_client =
        forwarded_client_fields_from_header_map(&status, request.headers(), source_addr.ip());
    let accepted_compression = accepted_response_compression(
        request
            .headers()
            .get(header::ACCEPT_ENCODING)
            .and_then(|value| value.to_str().ok()),
    );
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
            builder = add_compression_headers(builder, None);
            stream.send_response(builder.body(())?).await?;
            if !response_body.is_empty() {
                stream.send_data(response_body).await?;
            }
            stream.finish().await?;
            api_ssl_log_with_client(
                json!({
                    "event": "api_ssl_request",
                    "level": "info",
                    "method": method.as_str(),
                    "path": path,
                    "status": status.as_u16(),
                    "duration_ms": started.elapsed().as_millis(),
                    "source_ip": socket_ip_for_log(source_addr.ip()),
                    "source_port": source_addr.port(),
                    "dest_ip": socket_ip_for_log(dest_addr.ip()),
                    "dest_port": dest_addr.port(),
                    "network_protocol": "udp",
                    "transport": "http3_quic",
                    "user_agent": user_agent.clone(),
                    "auth": "not_required_for_robots",
                }),
                &forwarded_client,
            );
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
        builder = add_compression_headers(builder, None);
        stream.send_response(builder.body(())?).await?;
        if !response_body.is_empty() {
            stream.send_data(response_body).await?;
        }
        stream.finish().await?;
        api_ssl_log_with_client(
            json!({
                "event": "api_ssl_request",
                "level": "warn",
                "method": method.as_str(),
                "path": path,
                "status": status.as_u16(),
                "duration_ms": started.elapsed().as_millis(),
                "source_ip": socket_ip_for_log(source_addr.ip()),
                "source_port": source_addr.port(),
                "dest_ip": socket_ip_for_log(dest_addr.ip()),
                "dest_port": dest_addr.port(),
                "network_protocol": "udp",
                "transport": "http3_quic",
                "user_agent": user_agent.clone(),
                "error": "authentication required",
            }),
            &forwarded_client,
        );
        return Ok(());
    }
    if method == Method::GET && path == "/events/stream" {
        let limits = config::api_limit_config();
        if let Err(response) =
            check_api_rate_limit(&state, &limits, method.as_str(), &path, started, None).await
        {
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
            builder = add_compression_headers(builder, None);
            stream.send_response(builder.body(())?).await?;
            if !response_body.is_empty() {
                stream.send_data(response_body).await?;
            }
            stream.finish().await?;
            api_ssl_log_with_client(
                json!({
                    "event": "api_ssl_realtime_stream_rejected",
                    "level": "warn",
                    "method": method.as_str(),
                    "path": path,
                    "status": status.as_u16(),
                    "duration_ms": started.elapsed().as_millis(),
                    "source_ip": socket_ip_for_log(source_addr.ip()),
                    "source_port": source_addr.port(),
                    "dest_ip": socket_ip_for_log(dest_addr.ip()),
                    "dest_port": dest_addr.port(),
                    "network_protocol": "udp",
                    "transport": "http3_quic_sse",
                    "user_agent": user_agent.clone(),
                    "error": "rate_or_concurrency_limit",
                }),
                &forwarded_client,
            );
            return Ok(());
        }
        let Some(_guard) = ApiRealtimeStreamGuard::try_new(state.clone()) else {
            let response = api_backoff_logged(
                "max_realtime_streams_exceeded",
                config::api_limit_config().overload_retry_after_seconds,
                method.as_str(),
                &path,
                started,
                None,
            );
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
            builder = add_compression_headers(builder, None);
            stream.send_response(builder.body(())?).await?;
            if !response_body.is_empty() {
                stream.send_data(response_body).await?;
            }
            stream.finish().await?;
            api_ssl_log_with_client(
                json!({
                    "event": "api_ssl_realtime_stream_rejected",
                    "level": "warn",
                    "method": method.as_str(),
                    "path": path,
                    "status": status.as_u16(),
                    "duration_ms": started.elapsed().as_millis(),
                    "source_ip": socket_ip_for_log(source_addr.ip()),
                    "source_port": source_addr.port(),
                    "dest_ip": socket_ip_for_log(dest_addr.ip()),
                    "dest_port": dest_addr.port(),
                    "network_protocol": "udp",
                    "transport": "http3_quic_sse",
                    "user_agent": user_agent.clone(),
                    "error": "max_realtime_streams_exceeded",
                }),
                &forwarded_client,
            );
            return Ok(());
        };
        let mut builder = http::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-store");
        builder = add_h3_security_headers(builder);
        builder = add_compression_headers(builder, None);
        stream.send_response(builder.body(())?).await?;
        stream
            .send_data(sse_event_bytes(
                "connected",
                0,
                realtime_event_payload(&state, "connected", "http3_quic_sse", 0),
            )?)
            .await?;
        count_realtime_event(&state);
        api_ssl_log_with_client(
            json!({
                "event": "api_ssl_realtime_stream_started",
                "level": "info",
                "method": method.as_str(),
                "path": path,
                "status": 200,
                "duration_ms": started.elapsed().as_millis(),
                "source_ip": socket_ip_for_log(source_addr.ip()),
                "source_port": source_addr.port(),
                "dest_ip": socket_ip_for_log(dest_addr.ip()),
                "dest_port": dest_addr.port(),
                "network_protocol": "udp",
                "transport": "http3_quic_sse",
                "user_agent": user_agent.clone(),
                "interval_seconds": DEF_REALTIME_STREAM_INTERVAL_SECONDS,
                "heartbeat_seconds": DEF_REALTIME_STREAM_HEARTBEAT_SECONDS,
                "max_stream_seconds": DEF_REALTIME_STREAM_MAX_SECONDS,
            }),
            &forwarded_client,
        );

        let max_events =
            (DEF_REALTIME_STREAM_MAX_SECONDS / DEF_REALTIME_STREAM_HEARTBEAT_SECONDS).max(1);
        let refresh_every =
            (DEF_REALTIME_STREAM_INTERVAL_SECONDS / DEF_REALTIME_STREAM_HEARTBEAT_SECONDS).max(1);
        let mut sent_refresh_events = 0_u64;
        let mut sent_heartbeat_events = 0_u64;
        let mut disconnect_error = None::<String>;
        for sequence in 1..=max_events {
            if TERMINATE.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_secs(DEF_REALTIME_STREAM_HEARTBEAT_SECONDS)).await;
            let event_name = if sequence % refresh_every == 0 {
                "dashboard.refresh"
            } else {
                "heartbeat"
            };
            let sequence_usize = usize::try_from(sequence).unwrap_or(usize::MAX);
            let frame = sse_event_bytes(
                event_name,
                sequence_usize,
                realtime_event_payload(&state, event_name, "http3_quic_sse", sequence_usize),
            )?;
            if let Err(err) = stream.send_data(frame).await {
                disconnect_error = Some(err.to_string());
                break;
            }
            if event_name == "dashboard.refresh" {
                sent_refresh_events += 1;
            } else {
                sent_heartbeat_events += 1;
            }
            count_realtime_event(&state);
        }
        let _ = stream.finish().await;
        api_ssl_log_with_client(
            json!({
                "event": "api_ssl_realtime_stream_closed",
                "level": if disconnect_error.is_some() { "warn" } else { "info" },
                "method": method.as_str(),
                "path": path,
                "status": 200,
                "duration_ms": started.elapsed().as_millis(),
                "source_ip": socket_ip_for_log(source_addr.ip()),
                "source_port": source_addr.port(),
                "dest_ip": socket_ip_for_log(dest_addr.ip()),
                "dest_port": dest_addr.port(),
                "network_protocol": "udp",
                "transport": "http3_quic_sse",
                "user_agent": user_agent.clone(),
                "refresh_events": sent_refresh_events,
                "heartbeat_events": sent_heartbeat_events,
                "error": disconnect_error.unwrap_or_else(|| "not available".to_string()),
            }),
            &forwarded_client,
        );
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
    let response_body = to_bytes(body, DEF_API_RESPONSE_MAX_BYTES).await?;
    let already_encoded = parts.headers.contains_key(header::CONTENT_ENCODING);
    let (response_body, applied_compression) =
        maybe_compress_body(response_body, accepted_compression, already_encoded)?;
    let mut builder = http::Response::builder().status(parts.status);
    for (name, value) in parts.headers {
        if let Some(name) = name {
            if applied_compression.is_some()
                && (name == header::CONTENT_ENCODING || name == header::VARY)
            {
                continue;
            }
            builder = builder.header(name, value);
        }
    }
    builder = add_h3_security_headers(builder);
    builder = add_compression_headers(builder, applied_compression);
    stream.send_response(builder.body(())?).await?;
    if !response_body.is_empty() {
        stream.send_data(response_body).await?;
    }
    stream.finish().await?;
    api_ssl_log_with_client(
        json!({
            "event": "api_ssl_request",
            "level": "info",
            "method": method.as_str(),
            "path": path,
            "status": status.as_u16(),
            "duration_ms": started.elapsed().as_millis(),
            "source_ip": socket_ip_for_log(source_addr.ip()),
            "source_port": source_addr.port(),
            "dest_ip": socket_ip_for_log(dest_addr.ip()),
            "dest_port": dest_addr.port(),
            "network_protocol": "udp",
            "transport": "http3_quic",
            "user_agent": user_agent.clone(),
        }),
        &forwarded_client,
    );
    Ok(())
}

// Enforces username/password for non-loopback remote clients.
fn authorize_remote_request<B>(
    request: &http::Request<B>,
    source_addr: SocketAddr,
) -> Result<(), Response> {
    if ip_is_loopback_or_mapped_loopback(source_addr.ip()) {
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

// Enforces username/password for non-loopback H3 clients.
fn authorize_h3_request<B>(
    request: &http::Request<B>,
    source_addr: SocketAddr,
) -> Result<(), Response> {
    authorize_remote_request(request, source_addr)
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
    if path == "/events/snapshot" {
        if let Err(response) =
            check_api_rate_limit(&state, &limits, method.as_str(), &path, started, None).await
        {
            return response;
        }
        state.write_ssl_status_file();
        return json_response(
            StatusCode::OK,
            realtime_event_payload(&state, "dashboard.snapshot", "snapshot_polling", 0),
        );
    }
    if path == "/health" {
        if let Err(response) =
            check_api_rate_limit(&state, &limits, method.as_str(), &path, started, None).await
        {
            return response;
        }
        state.write_ssl_status_file();
        return json_response(StatusCode::OK, state.health_json());
    }
    if path == "/limits" {
        if let Err(response) =
            check_api_rate_limit(&state, &limits, method.as_str(), &path, started, None).await
        {
            return response;
        }
        return json_response(StatusCode::OK, api_limits_response());
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

// Parses and URL-decodes a query string for the H3/TCP remote API bridge.
fn parse_query_map(query: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (key, value) in form_urlencoded::parse(query.as_bytes()) {
        out.insert(key.into_owned(), value.into_owned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_query_parser_decodes_symbols_and_rfc3339_values() {
        let query = "symbols=ATEC%2CAUGO%2CBKNG&start=2026-05-07T07%3A00%3A00.000Z&end=2026-05-08T06%3A59%3A59.999Z";
        let parsed = parse_query_map(query);
        assert_eq!(parsed.get("symbols"), Some(&"ATEC,AUGO,BKNG".to_string()));
        assert_eq!(
            parsed.get("start"),
            Some(&"2026-05-07T07:00:00.000Z".to_string())
        );
        assert_eq!(
            parsed.get("end"),
            Some(&"2026-05-08T06:59:59.999Z".to_string())
        );
    }
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
            "limits": api_limits_json(&limits),
            "runtime": state.runtime_json(),
        }),
    )
}

// Handles the limits request for adaptive API clients.
async fn handle_limits(
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
    json_response(status_code, api_limits_response())
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

// Handles the lightweight realtime snapshot route.
async fn handle_events_snapshot(
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
    json_response(
        status_code,
        realtime_event_payload(&state, "dashboard.snapshot", "snapshot_polling", 0),
    )
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
    run_cli(args, method, path, started, state).await
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
            let mut args = vec!["market".into(), "bars".into()];
            let mut has_symbol = false;
            if let Some(symbol) = target
                .map(str::to_string)
                .or_else(|| input.value(&["symbol"]))
            {
                args.push(symbol);
                has_symbol = true;
            }
            let symbols = input.list(&["symbols"]);
            if !symbols.is_empty() {
                has_symbol = true;
            }
            if !has_symbol {
                return Err(ApiBuildError::new(
                    StatusCode::BAD_REQUEST,
                    "market bars requires symbol or symbols",
                ));
            }
            for symbol in symbols {
                args.push("--symbols".into());
                args.push(symbol);
            }
            push_option(&mut args, "--timeframe", input, &["timeframe"]);
            push_option(&mut args, "--limit", input, &["limit"]);
            push_option(&mut args, "--start", input, &["start"]);
            push_option(&mut args, "--end", input, &["end"]);
            Ok(args)
        }
        ("market", "warm-bars") => {
            let mut args = vec!["market".into(), "warm-bars".into()];
            for symbol in input.list(&["symbol", "symbols"]) {
                args.push("--symbols".into());
                args.push(symbol);
            }
            push_option(
                &mut args,
                "--limit-symbols",
                input,
                &["limit_symbols", "limit-symbols"],
            );
            push_option(
                &mut args,
                "--fresh-seconds",
                input,
                &["fresh_seconds", "fresh-seconds"],
            );
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

// Records cache/provider hit counters from market-bar command JSON output.
fn record_market_bar_metrics(state: &ApiRuntimeState, args: &[String], parsed: Option<&Value>) {
    let is_market_bars = matches!(
        (
            args.first().map(String::as_str),
            args.get(1).map(String::as_str)
        ),
        (Some("market"), Some("bars")) | (Some("market"), Some("warm-bars"))
    );
    if !is_market_bars {
        return;
    }
    state.market_bar_api_requests.fetch_add(1, Ordering::SeqCst);
    let Some(parsed) = parsed else {
        return;
    };
    if parsed.get("ok").and_then(Value::as_bool) == Some(false) {
        return;
    }

    if args.get(1).map(String::as_str) == Some("warm-bars") {
        let refreshed = parsed.get("refreshed").and_then(Value::as_u64).unwrap_or(0) as usize;
        let cache_hits = parsed
            .get("cache_hits")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let empty = parsed.get("empty").and_then(Value::as_u64).unwrap_or(0) as usize;
        state
            .market_bar_results
            .fetch_add(refreshed + cache_hits + empty, Ordering::SeqCst);
        state
            .market_bar_cache_hits
            .fetch_add(cache_hits, Ordering::SeqCst);
        state
            .market_bar_provider_fetches
            .fetch_add(refreshed + empty, Ordering::SeqCst);
        state
            .market_bar_empty_results
            .fetch_add(empty, Ordering::SeqCst);
        return;
    }

    let mut values = Vec::new();
    if let Some(results) = parsed.get("results").and_then(Value::as_object) {
        values.extend(results.values());
    } else if parsed.get("source").is_some() {
        values.push(parsed);
    }
    for value in values {
        state.market_bar_results.fetch_add(1, Ordering::SeqCst);
        let source = value
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("not available");
        let bars = value
            .get("bars")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        if source.starts_with("cache") {
            state.market_bar_cache_hits.fetch_add(1, Ordering::SeqCst);
        } else {
            state
                .market_bar_provider_fetches
                .fetch_add(1, Ordering::SeqCst);
        }
        if bars == 0 {
            state
                .market_bar_empty_results
                .fetch_add(1, Ordering::SeqCst);
        }
        let stored = value
            .get("cache_rows_stored")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        state
            .market_bar_cache_rows_stored
            .fetch_add(stored, Ordering::SeqCst);
    }
}

// Handles run cli logic.
async fn run_cli(
    args: Vec<String>,
    method: String,
    path: String,
    started: Instant,
    state: Arc<ApiRuntimeState>,
) -> Response {
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

    let stdout_raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr_raw = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let parsed = serde_json::from_str::<Value>(&stdout_raw).ok();
    let parsed_stderr = serde_json::from_str::<Value>(&stderr_raw).ok();
    record_market_bar_metrics(&state, &args, parsed.as_ref());
    state.write_ssl_status_file();
    let stdout = config::sanitize_logged_command_output(&stdout_raw);
    let stderr = config::sanitize_logged_command_output(&stderr_raw);
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
        json!({
            "section": "market",
            "actions": ["quote", "bars", "warm-bars", "news", "clock", "calendar"],
            "limits": {
                "market_bars_max_symbols": crate::MARKET_BARS_MAX_SYMBOLS,
                "market_bars_max_total_bars": crate::MARKET_BARS_MAX_TOTAL_BARS,
                "recommended_market_bars_batch_symbols": 25
            }
        }),
        json!({"section": "trade", "actions": ["account", "orders", "positions", "buy", "sell", "cancel", "close"], "mutation_guard": "buy/sell/cancel/close require auto-trading disabled"}),
        json!({"section": "data", "actions": ["movers", "screen", "watchlist", "suggest", "status"]}),
        json!({"section": "compliance", "actions": ["wash", "pdt", "tax"]}),
        json!({"section": "auto", "actions": ["sync-orders", "status", "history", "config", "track", "untrack"]}),
        json!({"section": "feeds", "actions": ["add", "remove", "sync", "list", "search", "graph", "sentiment", "correlate", "status"]}),
        json!({"section": "events", "actions": ["snapshot", "stream"], "transports": ["http3_quic", "tcp_https"], "stream_content_type": "text/event-stream", "fallback": "snapshot_polling"}),
    ]
}
