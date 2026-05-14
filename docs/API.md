# mlai-trade API

Last updated: 2026-05-07

The API has two transports. The local Unix-socket transport is intended for
local automation. The optional remote transport is an HTTP/3-over-QUIC service
that also serves the built React dashboard. API command responses are JSON. The
API does not expose `runtime` commands.

Remote transport policy:

- Remote API data plane: TCP/443 HTTPS and UDP/443 HTTP/3 by default. Both use
  TLS 1.3 only. The UDP listener uses ALPN `h3`; the TCP listener uses
  `http/1.1` and advertises `Alt-Svc` for H3 upgrades.
- Key exchange policy: `mlkem_secure_fallback` by default. The server offers
  hybrid ML-KEM groups first, then only strong TLS 1.3 classical groups
  (`X25519`, `P-256`, `P-384`) for browser compatibility. Set
  `mlkem_required` only when all clients support ML-KEM/hybrid groups.
- TCP HTTPS serves the same dashboard and allowed JSON API routes as H3, then
  advertises `Alt-Svc: h3=":443"` so browsers can upgrade when supported.
- The Let's Encrypt TLS-ALPN-01 challenge responder is separate and is allowed
  only when `api.ssl.cert_mode=letsencrypt` and
  `api.ssl.tcp_acme_tls_alpn_enabled=true`.
- Public Let's Encrypt TLS-ALPN-01 validation requires TCP `443`; set
  `api.ssl.tcp_acme_port=443` for real ACME issuance.
- Browser clients without HTTP/3 support can use TCP HTTPS. App clients can
  still be configured to require H3 only.
- SNI encryption requires TLS Encrypted ClientHello (ECH). That is separate
  from certificate generation and key exchange policy: ECH needs server support
  plus DNS HTTPS/SVCB records containing an `ech` parameter. mlai-trade parses
  and reports the DNS `ech` parameter today, but the current rustls/quinn
  listener cannot terminate server-side ECH, so enabling ECH fails closed until
  an ECH-capable listener is added.
- Localhost source traffic can open the React dashboard without authentication.
  Non-localhost remote clients must authenticate with `api.ssl.auth`.
- `robots.txt` disallows all crawlers and common AI-agent user agents. This is
  advisory only; authentication remains the access control.

The Unix transport is intentionally local-only:

- It binds a Unix socket, not TCP.
- The socket file is created with `0600` permissions.
- Normal commands are synchronous and controlled by `api.request_timeout_seconds`.
- Long operations use `api.long_request_timeout_seconds`; today that applies to `ml refresh` and `feeds sync`.
- Overload protection rejects excess requests with HTTP `429` and a JSON `retry_after_seconds` value.
- Oversized request bodies are rejected with HTTP `413`.
- Underlying CLI actions run with `--json` and progress output disabled.
- CLI stdout/stderr wrapped by the API is redacted for configured Alpaca and FRED secrets before it is returned or logged.

## Configuration

Runtime config:

```text
~/mlai-trade/config/mlai-trade.json
```

Example config:

```text
config/mlai-trade.example.json
```

API config block:

```json
{
  "api": {
    "enabled": false,
    "unix": {
      "enabled": true,
      "socket_file": "",
      "pid_file": "",
      "log_file": ""
    },
    "ssl": {
      "enabled": false,
      "auth": {
        "enabled": true,
        "username": "admin",
        "password": "replace_me"
      },
      "ech": {
        "enabled": false,
        "public_name": "",
        "config_file": "",
        "key_file": "",
        "require_dns_https_record": true
      },
      "domain": "",
      "bind_host": "0.0.0.0",
      "ipv4_enabled": true,
      "ipv6_enabled": true,
      "udp_port": 443,
      "tcp_enabled": true,
      "tcp_bind_host": "0.0.0.0",
      "tcp_port": 443,
      "tcp_bootstrap_enabled": true,
      "tcp_bootstrap_bind_host": "0.0.0.0",
      "tcp_bootstrap_port": 443,
      "pid_file": "",
      "log_file": "",
      "cert_mode": "provided",
      "cert_file": "",
      "key_file": "",
      "acme_challenge_cert_file": "",
      "acme_challenge_key_file": "",
      "key_exchange_policy": "mlkem_secure_fallback",
      "dns_https_check_required": true,
      "tcp_acme_tls_alpn_enabled": false,
      "tcp_acme_bind_host": "0.0.0.0",
      "tcp_acme_port": 443
    },
    "socket_file": "",
    "pid_file": "",
    "log_file": "",
    "request_timeout_seconds": 60,
    "long_request_timeout_seconds": 3600,
    "max_concurrent_requests": 8,
    "max_concurrent_long_requests": 1,
    "rate_limit_per_minute": 120,
    "max_body_bytes": 65536,
    "overload_retry_after_seconds": 5
  }
}
```

Defaults when fields are blank:

| Field | Default |
| --- | --- |
| `socket_file` | `~/mlai-trade/api/mlai-trade-api.sock` |
| `pid_file` | `~/mlai-trade/tmp/mlai-trade-api.pid` |
| `log_file` | `~/mlai-trade/logs/mlai-trade-api.log` |
| `request_timeout_seconds` | `60`, clamped to `5`-`300` |
| `long_request_timeout_seconds` | `3600`, clamped to `60`-`86400` |
| `max_concurrent_requests` | `8`, clamped to `1`-`128` |
| `max_concurrent_long_requests` | `1`, clamped to `1`-`16` |
| `rate_limit_per_minute` | `120`, clamped to `1`-`10000` |
| `max_body_bytes` | `65536`, clamped to `1024`-`1048576` |
| `overload_retry_after_seconds` | `5`, clamped to `1`-`300` |

`socket_file`, `pid_file`, and `log_file` at the top of `api` remain accepted
for backward compatibility. New configs should use `api.unix.*`.

API logs rotate daily. The active log remains `logs/mlai-trade-api.log`.
Archived logs are compressed as
`logs/archived/YYYYMMDD-mlai-trade-api.log.gz`.
Blank or relative `socket_file`, `pid_file`, and `log_file` values resolve inside `api/`, `tmp/`, and `logs/` respectively. Runtime API files are private: the socket and log file are `0600`, the PID file is runtime metadata at `0644`, and their parent folders are `0700`.

Remote HTTPS/H3 fields:

- `api.ssl.enabled`: enables the remote HTTP/3 transport only when
  `api.enabled=true`.
- `api.ssl.auth.enabled`: remote non-localhost clients must authenticate.
  Startup refuses non-loopback binds when auth is disabled or still uses the
  example password.
- `api.ssl.auth.username` / `password`: HTTP Basic credentials for remote
  non-localhost clients. Localhost source traffic bypasses auth so the local
  webapp works without a login prompt.
- `api.ssl.ech.enabled`: planned Encrypted ClientHello switch. It defaults to
  `false`; if set to `true` before the server TLS stack supports RFC 9849,
  startup fails closed and logs `api_ssl_ech_unsupported`.
- `api.ssl.ech.public_name`: public ECH name for future DNS HTTPS/SVCB
  publication.
- `api.ssl.ech.config_file` / `key_file`: future ECH config/key paths under
  `config/cert/`.
- `api.ssl.ech.require_dns_https_record`: future startup guard requiring DNS
  HTTPS/SVCB `ech` records when ECH is supported.
- `api.ssl.domain`: public DNS name expected in the TLS certificate and
  HTTPS/SVCB record.
- `api.ssl.bind_host` / `udp_port`: QUIC listener address. The default port is
  UDP `443`.
- `api.ssl.tcp_enabled`: opens the TCP HTTPS listener for browsers and clients
  that do not use H3 yet. It serves the dashboard and allowed API routes, and
  advertises `Alt-Svc` pointing at the configured UDP/H3 port.
- `api.ssl.tcp_bind_host` / `tcp_port`: TCP HTTPS listener address. Blank bind
  host inherits `api.ssl.bind_host`; blank port inherits `api.ssl.udp_port`.
- `api.ssl.tcp_bootstrap_*`: legacy aliases accepted for backward
  compatibility. New configs should use `api.ssl.tcp_*`.
- `api.ssl.cert_mode`: `provided`, `self_signed`, or `letsencrypt`.
- `api.ssl.cert_file` / `key_file`: certificate paths; blank resolves under
  `~/mlai-trade/config/cert/`.
- `api.ssl.acme_challenge_cert_file` / `acme_challenge_key_file`: RFC
  8737-style TLS-ALPN-01 challenge certificate/key paths.
- `api.ssl.key_exchange_policy`: `mlkem_secure_fallback` or `mlkem_required`.
  The default `mlkem_secure_fallback` prefers hybrid ML-KEM groups and then
  permits only strong TLS 1.3 classical groups for browsers that do not support
  ML-KEM yet. `mlkem_required` disables all classical fallback and may break
  current browsers with `client is incompatible: NoKxGroupsInCommon`.
- `api.ssl.dns_https_check_required`: require DNS HTTPS/SVCB validation before
  remote startup.
- `api.ssl.tcp_acme_tls_alpn_enabled`: Let's Encrypt TLS-ALPN-01 TCP responder
  only; disabled by default and no API routes.

The default challenge port is `443`. ACME remains off unless
`api.ssl.cert_mode=letsencrypt` and
`api.ssl.tcp_acme_tls_alpn_enabled=true`.

## Overload Protection

Both API transports protect the host from accidental overload:

- Every request counts toward `api.rate_limit_per_minute`.
- Command routes must acquire a global concurrency slot from `api.max_concurrent_requests`.
- Long commands such as `ml refresh` and `feeds sync` must also acquire a long-operation slot from `api.max_concurrent_long_requests`.
- Request bodies larger than `api.max_body_bytes` are rejected before command execution.
- The SSL remote listener also caps active TCP HTTPS connections and active
  UDP/QUIC connections at 128 each. Excess connection attempts are dropped
  before command execution.

When the rate or concurrency guard rejects a request, the response is HTTP `429` and includes a standard `Retry-After` header:

```json
{
  "ok": false,
  "error": "API overloaded: max_concurrent_requests_exceeded; retry after 5s",
  "reason": "max_concurrent_requests_exceeded",
  "retry_after_seconds": 5,
  "status_code": 429
}
```

Clients should wait at least `retry_after_seconds` before retrying. Connection
cap rejections are logged once per protocol per 60 seconds with
`suppressed_since_last_log` so an attack cannot flood logs. There is no response
cache yet; protection is backpressure, not caching. Trading mutation routes are
never cached.

## Lifecycle

The API refuses to start unless `api.enabled=true`.

```sh
mlai-trade api start
mlai-trade api status
mlai-trade api status --details
mlai-trade api test
mlai-trade api reload
mlai-trade api restart
mlai-trade api stop
```

These legacy commands target the Unix-socket transport. The explicit form is:

```sh
mlai-trade api unix start
mlai-trade api unix status --details
mlai-trade api unix test
mlai-trade api unix stop
```

Remote SSL/H3 lifecycle:

```sh
mlai-trade api ssl enable
mlai-trade api ssl cert generate --target h3
mlai-trade api ssl cert info
mlai-trade api ssl start
mlai-trade api ssl status
mlai-trade api ssl status --json
mlai-trade api ssl reload
mlai-trade api ssl restart
mlai-trade api ssl stop
mlai-trade api ssl disable
```

Certificate commands:

```sh
mlai-trade api ssl cert info
mlai-trade api ssl cert generate --target h3 --domain localhost
mlai-trade api ssl cert renew --target h3 --domain localhost
mlai-trade api ssl cert generate --target h3 --domain api.example.com \
  --organization "Example Inc" --organizational-unit "Trading"
mlai-trade api ssl cert generate --target acme --domain example.com \
  --acme-key-authorization TOKEN.THUMBPRINT
mlai-trade api ssl cert renew --target acme --domain example.com \
  --acme-key-authorization TOKEN.THUMBPRINT
```

Certificate writes are intentionally one target at a time:

- `--target h3`: the H3 identity certificate used by QUIC clients.
- `--target acme`: the TLS-ALPN-01 challenge certificate with ALPN
  `acme-tls/1` metadata shape described by RFC 8737.
- `cert info --target h3|acme`: read-only metadata, expiry, SANs, issuer,
  serial, ACME extension state, and auto-renew eligibility. The default target
  is `h3`.
- Generated certificates default to `O=MLAI-TRADE` and `OU=MLAI-TRADE`.
  Override those subject fields with `--organization`/`--o` and
  `--organizational-unit`/`--ou` on `generate` or `renew`.

When `--acme-key-authorization` is omitted, the challenge certificate contains
only a placeholder digest. `--acme-key-authorization` is valid only with
`--target acme`. When an ACME order flow supplies a real key authorization,
`cert renew --target acme --acme-key-authorization ...` regenerates the
challenge certificate for that authorization.

Certificate key type is independent from the TLS key exchange policy. The
mlai-trade self-signed H3 and ACME certificates use ECDSA P-256 keys, while the
TLS 1.3 handshake negotiates ML-KEM/hybrid or strong classical key exchange
according to `api.ssl.key_exchange_policy`.

SNI encryption is also independent from certificate generation. The standards
path is TLS Encrypted ClientHello (ECH), published in RFC 9849 and bootstrapped
through DNS HTTPS/SVCB `ech` parameters in RFC 9848. Current status:

1. Add `api.ssl.ech.enabled`, `api.ssl.ech.public_name`, and generated ECH
   config/key files under `config/cert/`. This config surface exists.
2. Teach `api ssl dns-check` and `api ssl status` to validate and display DNS
   HTTPS/SVCB `ech` parameters, not only `alpn=h3` and port. This is
   implemented.
3. Enable ECH only when the Rustls/QUIC server path supports the RFC 9849 server
   API on the target OS. If the TLS stack cannot enforce ECH correctly, startup
   fails rather than silently exposing plaintext SNI. This is the remaining
   blocker.
4. Document public DNS records and browser/client expectations. ECH still
   depends on client support and encrypted DNS behavior.

Preferred implementation stack: OpenSSL ECH once it is available as a stable
server-side dependency on the supported OSes. OpenSSL exposes documented ECH
commands/APIs such as `openssl ech` and `SSL_CTX_set1_echstore`. BoringSSL is
browser-proven, but its API/ABI stability policy is not a good fit for
mlai-trade's cross-platform CLI distribution. If OpenSSL ECH is not viable for
the in-process H3 server, the fallback architecture is an optional local
OpenSSL-backed H3/ECH frontend that bridges to the existing Unix-socket API.

Until that lands, public-domain SNI can still be visible in the TLS ClientHello.

On `api ssl start`, mlai-trade inspects certificate expiry. It auto-renews only
mlai-trade-generated certificates when `api.ssl.cert_mode=self_signed` and a
certificate is missing or within 30 days of expiry. Provided/public CA
certificates are reported by `cert info` but are never overwritten. The ACME
TLS-ALPN-01 challenge certificate is generated per challenge and is
intentionally not auto-renewed because a real renewal requires the current ACME
key authorization from the active CA order. `cert info` prints this as
`Renew note`.

Remote DNS validation:

```sh
mlai-trade api ssl dns-check example.com
mlai-trade api ssl dns-check --json
```

When `api.ssl.dns_https_check_required=true`, startup enforces HTTPS/SVCB
`alpn=h3` discovery for configured public domains. If
`api.ssl.ech.enabled=true` and `api.ssl.ech.require_dns_https_record=true`, the
DNS check also requires the HTTPS/SVCB `ech` parameter. `localhost`, IP
literals, and blank domains are treated as local/private testing and skip the
DNS check.

## React Dashboard

The remote H3 listener serves the built React app from:

```text
~/mlai-trade/api/html/dist
```

Source lives in the repository under `api/html/src`; static public files such
as `robots.txt` live under `api/html/public`. Build it with:

```sh
cd api/html
npm install
npm run build
```

The dashboard is responsive for mobile and notebook/desktop screens. It uses
real API routes for accounts, positions, orders, data suggestions,
wash-sale/PDT status, federal tax estimates, feed sentiment, and ML explain
output. It polls read-only account, position, order, and compliance snapshots
every 60 seconds by default.
Slower data-pipeline refreshes run in the background. Normal dashboard
refreshes do not force provider order/position sync; use the dashboard
`Sync orders` action when an explicit provider reconciliation is wanted.
The dashboard stores the active section in the URL hash and local browser
storage, so refreshing `#positions` stays on Positions. The top-bar account
selector defaults to all accounts and filters account, position, order, and
auto-trade views locally when a single account is selected. It also selects the
same account for Tax; when the top-bar selector is "All accounts", Tax uses the
default real-account estimate unless another account is chosen explicitly.
Changing the Tax year or account selector reloads the estimate automatically.
Position symbols open an insight overlay with feed sentiment, recent headlines,
ML explain values, and plain-English descriptions for SHAP features.
The overview, account, and position views use live provider account/position
values for current totals and market bars for chart series. Charts include date
labels and share a range selector for Today, 3 days, 7 days, or a custom
start/end range. Overview allocation is a
two-column scrollable list under the performance chart. Large order, position,
tax, and wash-sale tables show 50 rows first and then expand in 50-row
increments. The toolbar shows the active market-bar interval, and P&L charts
expose hover tooltips with the nearest timestamp and value. The Overview
performance chart aggregates provider open-position P&L from intraday market-bar
series. The Overview metric tiles use provider account equity, provider open
market value, provider unrealized P&L, and live provider open P&L for
auto-managed positions plus auto closed P&L. P&L charts label the entry
break-even line, and per-position charts show
an entry-time buy marker when the entry timestamp is inside the selected range.
Per-position charts show an explicit no-bars message when the provider has no
data for the selected range. The dashboard renders curated views only; raw API
response panels are intentionally not exposed. Federal tax can be loaded for
any explicit provider account selector, including paper accounts for simulation.
Wash-sale windows are
separated by paper-vs-real compliance universe and grouped by universe, symbol,
sold date, and window end.

Localhost browser access over IPv4 or IPv6 loopback is unauthenticated:

```text
https://localhost/
https://127.0.0.1/
https://[::1]/
```

Request logs report both source and destination socket addresses. Remote SSL
binds are dual-stack by default through `api.ssl.ipv4_enabled=true` and
`api.ssl.ipv6_enabled=true`; either stack can be disabled in config. When the
listener is bound to a wildcard address such as `0.0.0.0` or `::`, `dest_ip`
is still the concrete local address accepted for that connection, for example
`127.0.0.1` or `::1` for localhost traffic.

Non-localhost access requires `api.ssl.auth`. Browsers receive a native login
page at `/login`; successful login sets a secure HttpOnly session cookie so the
dashboard can use the API without repeatedly prompting. The browser session is
valid for 30 days and can be cleared with the dashboard Logout button or
`POST /logout`. API clients can still use HTTP Basic auth with the same
username/password, for example `curl -u USER:PASSWORD`. Browsers can load the
app over TCP HTTPS and upgrade to H3 when they honor `Alt-Svc`.

Localhost and loopback browser sessions bypass authentication and do not show
the dashboard Logout action. Compliance tax detail responses are ordered
newest-to-oldest by exit date.

## Security Review Checklist

Before exposing the remote listener beyond localhost:

- Change `api.ssl.auth.password` from `replace_me`; startup refuses public binds
  with the example password.
- Keep `api.ssl.key_exchange_policy=mlkem_secure_fallback` for browser access.
  It still uses TLS 1.3 only, prefers hybrid ML-KEM, and allows only
  X25519/P-256/P-384 fallback. Use `mlkem_required` only for controlled clients
  known to support the required groups.
- Keep cert/key files under `config/cert/`; generated private keys are written
  with `0600` permissions and the directory is `0700`.
- Use `api ssl dns-check DOMAIN` and an HTTPS/SVCB record advertising `alpn=h3`
  for public H3 discovery when possible. TCP HTTPS remains available for
  browser compatibility unless `api.ssl.tcp_enabled=false`.
- H3 responses include `Alt-Svc`, HSTS, `X-Content-Type-Options`, frame denial,
  no-referrer, a restrictive CSP, `Permissions-Policy`, and `X-Robots-Tag`.
- TCP HTTPS responses include `Alt-Svc` and the same no-index/security headers
  as H3 responses.
- Treat `robots.txt` as advisory only. It blocks cooperative crawlers and common
  AI-agent user agents, but authentication is the actual protection.
- Review `logs/mlai-trade-api-ssl.log`; every remote HTTP request logs method,
  path, status, duration, network protocol, source IP/port, destination IP/port,
  sanitized RFC 9110 `User-Agent`, `CF-Ray` when present, and computed client
  attribution fields. `source_ip` is always the direct socket peer. `client_ip`
  can be derived from forwarding headers such as `CF-Connecting-IP`,
  `True-Client-IP`, `X-Forwarded-For`, or `X-Real-IP`, but only when the socket
  peer matches `api.ssl.trusted_proxy_cidrs`, such as a local Cloudflare
  tunnel.
  `client_ip_source` is a generic value such as `cloudflare`, `trusted_proxy`,
  or `socket_source_ip`; raw forwarding header values are not emitted in logs.
  TLS handshake failures have no HTTP headers, so header-derived fields are
  reported as `not available` or omitted depending on where the failure
  occurred.

`api test` and `api unix test` send `GET /health` through the configured socket.
`api status --details` asks the Unix API process for live counters and also
reads the SSL/H3 runtime status file when the remote listener is running.
`api ssl status --details` prints only the SSL/H3 runtime view. These detail
views report uptime, active requests, active long requests, total requests,
rejected requests, average requests per second, process CPU,
machine-normalized CPU, CPU capacity, CPU worker budget, accelerator
availability, realtime stream counters, market-bar cache hit/provider-fetch
counters, RSS memory, memory budget, open files/sockets, and OS thread count.
This distinction matters
because browser dashboard traffic normally reaches the SSL/H3 process, not the
Unix-socket process. Runtime metrics use native Linux `/proc`, macOS Mach APIs,
and FreeBSD `sysctl`/`kinfo_proc` paths where available. Metrics that cannot be
read are reported as `not available`.

## Calling The API

Use `curl -s --unix-socket`:

```sh
curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock http://localhost/health
curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock http://localhost/routes
```

Command routes use:

```text
GET  /{section}/{action}
POST /{section}/{action}
GET  /{section}/{action}/{target}
POST /{section}/{action}/{target}
```

Query parameters and JSON bodies are both accepted. For example:

```sh
curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock \
  http://localhost/market/quote/AAPL

curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock \
  -H 'content-type: application/json' \
  -d '{
    "symbol":"AAPL",
    "timeframe":"1Min",
    "limit":1000,
    "start":"2026-05-07T00:00:00Z",
    "end":"2026-05-07T23:59:59Z"
  }' \
  http://localhost/market/bars
```

Wrapped command responses look like:

```json
{
  "ok": true,
  "command": ["market", "quote", "AAPL"],
  "exit_code": 0,
  "duration_ms": 123,
  "data": {}
}
```

If a command prints non-JSON text, the API wraps it in `text`. Errors return JSON with `ok=false` and `error` or `stderr`.

Resource misses are errors. If the underlying command returns JSON with `ok:false`, the API response also uses `ok:false` and a non-2xx HTTP status even if the process exited cleanly. Commands can include `status_code` or `http_status` in their JSON to request a specific error status such as `404`.

Example miss:

```sh
curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock \
  http://localhost/feeds/remove/MTA | jq
```

Expected shape:

```json
{
  "ok": false,
  "command": ["feeds", "remove", "MTA"],
  "exit_code": 1,
  "duration_ms": 24,
  "data": {
    "ok": false,
    "error": "MTA was not subscribed",
    "status_code": 404,
    "symbol": "MTA"
  },
  "error": "MTA was not subscribed"
}
```

## Allowlist

### Health

| Method | Path | Description |
| --- | --- | --- |
| `GET` | `/health` | API process health and socket status. |
| `GET` | `/limits` | Machine-readable limits for adaptive clients. |
| `GET` | `/routes` | Full allowlist as JSON. |
| `GET` | `/events/snapshot` | Lightweight runtime heartbeat for clients. |
| `GET` | `/events/stream` | Server-sent realtime refresh hints over HTTPS/H3. |

### Realtime Events

`/events/stream` is a browser-compatible `text/event-stream` endpoint. It sends
a `connected` event immediately, a lightweight `heartbeat` every 15 seconds,
and a `dashboard.refresh` event every 60 seconds. When the browser has upgraded
to HTTP/3, this stream is carried by the H3/QUIC connection; otherwise it runs
over TCP HTTPS. The React dashboard uses the stream to trigger normal read-only
snapshot refreshes. If the stream is not available, the dashboard keeps the
existing 60-second polling fallback.

The stream does not execute provider sync by itself. It is intentionally a
lightweight coordination channel so it cannot multiply provider calls in the
background. `/events/snapshot` returns the current runtime heartbeat as JSON for
clients that prefer polling. `/limits` advertises the stream path, snapshot
path, refresh interval, heartbeat interval, maximum stream lifetime, and
maximum active streams.

### Daemon

Only status and reload are exposed.

| Method | Path | Description |
| --- | --- | --- |
| `GET/POST` | `/daemon/status` | Daemon enabled/running/pid/log/interval status. |
| `GET/POST` | `/daemon/reload` | Send daemon reload signal. Fails if daemon is not running. |

### ML

| Method | Path | Parameters |
| --- | --- | --- |
| `GET/POST` | `/ml/status` | none |
| `GET/POST` | `/ml/refresh` | `days`, `quick`, `backend`, `walk_forward_folds`, `top_n`, `slippage_bps` |
| `GET/POST` | `/ml/explain/{symbol}` | symbol in path or body/query |
| `GET/POST` | `/ml/explainable` | `limit` |
| `GET/POST` | `/ml/explained` | `limit` |

### Market

| Method | Path | Parameters |
| --- | --- | --- |
| `GET/POST` | `/market/quote/{symbol}` | symbol in path or body/query |
| `GET/POST` | `/market/bars/{symbol}` | `timeframe`, `limit`, `start`, `end` |
| `GET/POST` | `/market/bars` | batched `symbols`, `timeframe`, `limit`, dates |
| `GET/POST` | `/market/warm-bars` | optional body/query args |
| `GET/POST` | `/market/news/{symbol}` | optional symbol, `limit` |
| `GET/POST` | `/market/clock` | none |
| `GET/POST` | `/market/calendar` | `start`, `end`, `market` or `markets` |

`/market/bars` returns structured JSON under `data.bars`. Provider responses
are stored in `market_bar_cache` for dashboard backfill and fallback, and fresh
cache rows are served before a provider request. `/market/warm-bars` preloads
the dashboard's default intervals for current provider positions. The daemon
runs the same warmup when `daemon.dashboard_bar_cache_enabled=true`. This cache
is intentionally separate from the daily ML `bars` table, which continues to
hold daily history used for feature generation and training.
The React dashboard also queues browser requests and retries HTTP `429`
responses using the API `Retry-After` value, so opening a tab with many
position charts should not overwhelm the local API.
API responses can be compressed when the client sends `Accept-Encoding`. The
remote HTTPS/H3 listener and the Unix-socket API support `zstd`, `br`, `gzip`,
and `deflate`, preferring them in that order when the client advertises more
than one. Browsers, Android, iOS, and most HTTP clients decompress
automatically. Scripts can use `curl --compressed` to request and decode
compressed responses.
Use `api status --details` to monitor whether market-bar API requests are
served from `market_bar_cache` or require provider fetches. The status output
includes result counts, cache hits, provider fetches, empty results, rows
stored, and cache/provider rates. For browser dashboard traffic, read the
`SSL/H3 Runtime` block.

`/market/warm-bars` accepts optional `symbol` or `symbols`, plus
`limit_symbols` and `fresh_seconds`.

Batch market bars:

```sh
curl -s --compressed --unix-socket ~/mlai-trade/api/mlai-trade-api.sock \
  'http://localhost/market/bars?symbols=AAPL,NVDA&timeframe=1Min&limit=1000'
```

`/market/bars` accepts at most 50 symbols and 25,000 requested bars per
request. Requested bars are `symbols * limit`, so `50` symbols with
`limit=1000` is rejected even if the provider would return fewer rows. If a
client exceeds a limit, the API returns `ok:false` with `max_symbols`,
`max_total_bars`, `requested_symbols`, `requested_total_bars`, and
`suggested_symbol_batch` when available. Clients should query `/limits`, split
the symbol list and/or date range, and retry smaller batches. `/limits` also
advertises dashboard order/table page sizes and supported response-compression
encodings, so browser and mobile clients do not need hardcoded request sizes.
The React dashboard reads `/limits` and normally uses 25-symbol batches for
position charts.

### Trade

Read routes are always allowed. Mutation routes are allowed only when auto-trading is disabled.
Trade/account/order/position JSON includes `broker_account_id` when Alpaca
exposes it, alongside the mutable local `account_ref`.

| Method | Path | Parameters |
| --- | --- | --- |
| `GET/POST` | `/trade/account` | optional `account`/`accounts` |
| `GET/POST` | `/trade/orders` | optional `account`/`accounts`, `status`, `limit`, `sync` |
| `GET/POST` | `/trade/positions` | optional `account`/`accounts`, `sync` |
| `POST` | `/trade/buy/{symbol}` | required `qty`, required `account`/`accounts`, optional `type`, `limit_price`, `stop_price`, `tif` |
| `POST` | `/trade/sell/{symbol}` | required `qty`, required `account`/`accounts`, optional `type`, `limit_price`, `stop_price`, `tif` |
| `POST` | `/trade/cancel/{order_id}` | required `account`/`accounts` |
| `POST` | `/trade/close/{symbol}` | required `account`/`accounts` |

Examples:

```sh
curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock \
  'http://localhost/trade/orders?account=alpaca:paper-main&sync=true&limit=20'

curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock \
  'http://localhost/trade/positions?account=alpaca:paper-main&sync=true'

curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock \
  -H 'content-type: application/json' \
  -d '{"qty":1,"account":"alpaca:paper-main","type":"market","tif":"day"}' \
  http://localhost/trade/buy/AAPL
```

### Data

| Method | Path | Parameters |
| --- | --- | --- |
| `GET/POST` | `/data/status` | none |
| `GET/POST` | `/data/movers` | none |
| `GET/POST` | `/data/screen` | `min_volume` |
| `GET/POST` | `/data/watchlist` | none |
| `GET/POST` | `/data/suggest` | none |

### Compliance

| Method | Path | Parameters |
| --- | --- | --- |
| `GET/POST` | `/compliance/wash` | none |
| `GET/POST` | `/compliance/pdt` | none |
| `GET/POST` | `/compliance/tax` | `accounts_list`, `details`, `show`, `show_brackets`, `year`, `quarter`, `export`, `account`/`accounts` |

Examples:

```sh
curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock \
  'http://localhost/compliance/tax?year=2026'

curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock \
  'http://localhost/compliance/tax?year=2026&quarter=1,2&details=true&account=alpaca:paper-main'
```

### Auto

| Method | Path | Parameters |
| --- | --- | --- |
| `GET/POST` | `/auto/sync-orders` | none |
| `GET/POST` | `/auto/status` | none |
| `GET/POST` | `/auto/history` | `limit` |
| `GET/POST` | `/auto/config` | optional `key`, `value` |
| `GET/POST` | `/auto/config/{key}` | optional `value` |
| `GET/POST` | `/auto/track/{symbol}` | required `account` or `accounts` |
| `GET/POST` | `/auto/untrack/{symbol}` | required `account` or `accounts` |

The API does not expose `auto run`, `auto enable`, or `auto disable`.
`/auto/sync-orders` also refreshes provider account snapshots. Manual provider
orders/fills/cash changes are stored as source-of-truth provider rows and show
up in `logs/mlai-trade-auto.log` as external-provider activity events.
`/auto/track` and `/auto/untrack` are ownership changes only: they do not place
orders. Both require exactly one symbol and an explicit account selector, and
the CLI rejects `ALL` as a symbol. Broad account selectors such as `all`,
`paper`, `real`, or `alpaca` are rejected for these actions. The account value
must be the full `provider:account-ref` selector, such as
`alpaca:paper-original`.
Trade and auto responses include `execution_origin` where applicable:
`mlai_auto`, `mlai_cli`, `provider_external`, `mixed`, or `unknown`. `auto
status` position rows also include `execution_origin_label`, using the concrete
provider name such as `alpaca` for direct provider-origin holdings. Auto status
rows also include `management_origin`/`management_origin_label`; this is the
current manager (`mlai-auto`, `mlai-cli`, or provider), while
`execution_origin` remains the historical audit source. Tax JSON also includes
`by_execution_origin` plus per-operation entry/exit origin when `details=true`.

### Feeds

Feed sync can be run directly, but `ml refresh` also reconciles the managed feed
universe and syncs feeds before training when `feeds.sync_before_training=true`.
When `feeds.compute_correlations_before_training=true`, the same refresh also
computes bounded feed-subscription price correlations. These are exposed by
`feeds correlate`/`/feeds/correlate` and used as point-in-time ML
feed-universe correlation features.

| Method | Path | Parameters |
| --- | --- | --- |
| `GET/POST` | `/feeds/add` | required `symbol` or `symbols` |
| `GET/POST` | `/feeds/remove/{symbol}` | symbol in path or body/query |
| `GET/POST` | `/feeds/sync` | `days` |
| `GET/POST` | `/feeds/list` | none |
| `GET/POST` | `/feeds/search/{query}` | query in path/body/query, optional `limit` |
| `GET/POST` | `/feeds/graph/{symbol}` | symbol in path or body/query |
| `GET/POST` | `/feeds/sentiment/{symbol}` | symbol in path or body/query |
| `GET/POST` | `/feeds/correlate` | `days` |
| `GET/POST` | `/feeds/status` | none |

Examples:

```sh
curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock \
  -H 'content-type: application/json' \
  -d '{"symbols":["AAPL","NVDA"]}' \
  http://localhost/feeds/add

curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock \
  http://localhost/feeds/sentiment/NVDA
```

## Blocked Sections

`runtime` is not exposed. Requests such as `/runtime/version` return JSON with `ok=false`.

Unknown sections or non-allowlisted actions return `404` JSON errors.

All API logs are JSON lines in `logs/mlai-trade-api.log`. Request records include `method`, `path`, `command`, `status`, and `duration_ms`.
