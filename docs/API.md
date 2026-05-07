# mlai-trade API

Last updated: 2026-05-06

The API has two transports. The local Unix-socket transport is intended for
local automation. The optional remote transport is an HTTP/3-over-QUIC service
that also serves the built React dashboard. API command responses are JSON. The
API does not expose `runtime` commands.

Remote transport policy:

- Remote API data plane: UDP/5443 by default, HTTP/3, TLS 1.3, ALPN `h3` only.
- Key exchange policy: `mlkem_required`; startup must fail if the compiled TLS
  provider cannot enforce ML-KEM-capable groups without classical fallback.
- TCP/5443: not a normal API listener. When
  `api.ssl.tcp_bootstrap_enabled=true`, it serves only a tiny TLS 1.3
  bootstrap response with `Alt-Svc: h3=":5443"` so browsers can learn and retry
  the dashboard over QUIC. It exposes no API routes.
- The Let's Encrypt TLS-ALPN-01 challenge responder is separate and is allowed
  only when `api.ssl.cert_mode=letsencrypt` and
  `api.ssl.tcp_acme_tls_alpn_enabled=true`.
- Public Let's Encrypt TLS-ALPN-01 validation requires TCP `443`; set
  `api.ssl.tcp_acme_port=443` for real ACME issuance.
- Clients without HTTP/3 support fail closed. Public discovery should use DNS
  HTTPS/SVCB records with `alpn=h3`.
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
      "domain": "",
      "bind_host": "0.0.0.0",
      "udp_port": 5443,
      "pid_file": "",
      "log_file": "",
      "cert_mode": "provided",
      "cert_file": "",
      "key_file": "",
      "acme_challenge_cert_file": "",
      "acme_challenge_key_file": "",
      "key_exchange_policy": "mlkem_required",
      "dns_https_check_required": true,
      "tcp_acme_tls_alpn_enabled": false,
      "tcp_acme_bind_host": "0.0.0.0",
      "tcp_acme_port": 5443
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

Remote H3/QUIC fields:

- `api.ssl.enabled`: enables the remote HTTP/3 transport only when
  `api.enabled=true`.
- `api.ssl.auth.enabled`: remote non-localhost clients must authenticate.
  Startup refuses non-loopback binds when auth is disabled or still uses the
  example password.
- `api.ssl.auth.username` / `password`: HTTP Basic credentials for remote H3
  clients. Localhost source traffic bypasses auth so the local webapp works
  without a login prompt.
- `api.ssl.domain`: public DNS name expected in the TLS certificate and
  HTTPS/SVCB record.
- `api.ssl.bind_host` / `udp_port`: QUIC listener address. Production should
  use UDP `5443`.
- `api.ssl.tcp_bootstrap_enabled`: opens a TCP TLS listener on
  `api.ssl.tcp_bootstrap_port`, defaulting to `5443`, only to advertise
  `Alt-Svc: h3=":5443"` to browsers. It does not expose API routes or the
  dashboard payload over TCP.
- `api.ssl.tcp_bootstrap_bind_host` / `tcp_bootstrap_port`: browser discovery
  listener address. Blank bind host inherits `api.ssl.bind_host`; blank port
  inherits `api.ssl.udp_port`.
- `api.ssl.cert_mode`: `provided`, `self_signed`, or `letsencrypt`.
- `api.ssl.cert_file` / `key_file`: certificate paths; blank resolves under
  `~/mlai-trade/config/cert/`.
- `api.ssl.acme_challenge_cert_file` / `acme_challenge_key_file`: RFC
  8737-style TLS-ALPN-01 challenge certificate/key paths.
- `api.ssl.key_exchange_policy`: must be `mlkem_required`; no classical
  fallback.
- `api.ssl.dns_https_check_required`: require DNS HTTPS/SVCB validation before
  remote startup.
- `api.ssl.tcp_acme_tls_alpn_enabled`: Let's Encrypt TLS-ALPN-01 TCP/5443
  responder only; no API routes.

The default challenge port is `5443` for local/private testing. Public
Let's Encrypt TLS-ALPN-01 validation requires TCP `443`, so public ACME
deployments must set `api.ssl.tcp_acme_port=443`.

## Overload Protection

Both API transports protect the host from accidental overload:

- Every request counts toward `api.rate_limit_per_minute`.
- Command routes must acquire a global concurrency slot from `api.max_concurrent_requests`.
- Long commands such as `ml refresh` and `feeds sync` must also acquire a long-operation slot from `api.max_concurrent_long_requests`.
- Request bodies larger than `api.max_body_bytes` are rejected before command execution.

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

Clients should wait at least `retry_after_seconds` before retrying. There is no response cache yet; protection is backpressure, not caching. Trading mutation routes are never cached.

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
mlai-trade api ssl cert info --target all
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
mlai-trade api ssl cert info --target all
mlai-trade api ssl cert generate --target h3 --domain localhost
mlai-trade api ssl cert renew --target h3 --domain localhost
mlai-trade api ssl cert generate --target acme --domain example.com \
  --acme-key-authorization TOKEN.THUMBPRINT
mlai-trade api ssl cert renew --target acme --domain example.com \
  --acme-key-authorization TOKEN.THUMBPRINT
```

Certificate writes are intentionally one target at a time:

- `--target h3`: the H3 identity certificate used by QUIC clients.
- `--target acme`: the TLS-ALPN-01 challenge certificate with ALPN
  `acme-tls/1` metadata shape described by RFC 8737.
- `cert info --target all|h3|acme`: read-only metadata, expiry, SANs, issuer,
  serial, ACME extension state, and auto-renew eligibility.

When `--acme-key-authorization` is omitted, the challenge certificate contains
only a placeholder digest. `--acme-key-authorization` is valid only with
`--target acme`. When an ACME order flow supplies a real key authorization,
`cert renew --target acme --acme-key-authorization ...` regenerates the
challenge certificate for that authorization.

On `api ssl start`, mlai-trade inspects certificate expiry. It auto-renews only
mlai-trade-generated certificates when `api.ssl.cert_mode=self_signed` and a
certificate is missing or within 30 days of expiry. Provided/public CA
certificates are reported by `cert info` but are never overwritten.

Remote DNS validation:

```sh
mlai-trade api ssl dns-check example.com
mlai-trade api ssl dns-check --json
```

When `api.ssl.dns_https_check_required=true`, startup enforces HTTPS/SVCB
`alpn=h3` discovery for configured public domains. `localhost`, IP literals,
and blank domains are treated as local/private testing and skip the DNS check.

## React Dashboard

The remote H3 listener serves the built React app from:

```text
~/mlai-trade/api/html/dist
```

Source lives in the repository under `api/html/src`. Build it with:

```sh
cd api/html
npm install
npm run build
```

The dashboard is responsive for mobile and notebook/desktop screens. It uses
the same API allowlist as command clients, including account, positions, orders,
auto status, ML status, market quote/bars, data status, and health.

Localhost browser access is unauthenticated:

```text
https://localhost:5443/
```

Non-localhost access requires HTTP Basic auth using `api.ssl.auth`. Browsers can
use the TCP bootstrap listener to learn `Alt-Svc`, then load the app over H3.
Apps that cannot use HTTP/3 still fail closed because the TCP listener exposes
no API data plane.

## Security Review Checklist

Before exposing the remote listener beyond localhost:

- Change `api.ssl.auth.password` from `replace_me`; startup refuses public binds
  with the example password.
- Keep `api.ssl.key_exchange_policy=mlkem_required`; startup uses TLS 1.3,
  ALPN `h3`, and an ML-KEM-only Rustls provider.
- Keep cert/key files under `config/cert/`; generated private keys are written
  with `0600` permissions and the directory is `0700`.
- Use `api ssl dns-check DOMAIN` and an HTTPS/SVCB record advertising `alpn=h3`
  for public clients when possible. Keep the TCP bootstrap enabled for browsers
  that need an initial `Alt-Svc` response.
- H3 responses include `Alt-Svc`, HSTS, `X-Content-Type-Options`, frame denial,
  no-referrer, a restrictive CSP, `Permissions-Policy`, and `X-Robots-Tag`.
- TCP bootstrap responses include `Alt-Svc` and the same no-index/security
  headers, but they do not dispatch API commands.
- Treat `robots.txt` as advisory only. It blocks cooperative crawlers and common
  AI-agent user agents, but authentication is the actual protection.
- Review `logs/mlai-trade-api-ssl.log`; every remote request logs method, path,
  status, duration, source IP/port, and destination IP/port as JSON.

`api test` and `api unix test` send `GET /health` through the configured socket.
`api status --details` asks the API process for its own live counters over the
Unix socket. It reports uptime, active requests, active long requests, total
requests, rejected requests, average requests per second, process CPU,
machine-normalized CPU, CPU capacity, CPU worker budget, accelerator
availability, RSS memory, memory budget, open files/sockets, and OS thread
count. Runtime metrics use native Linux `/proc`, macOS Mach APIs, and FreeBSD
`sysctl`/`kinfo_proc` paths where available. Metrics that cannot be read are
reported as `not available`.

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
  -d '{"symbol":"AAPL","timeframe":"1Day","limit":5}' \
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
| `GET` | `/routes` | Full allowlist as JSON. |

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
| `GET/POST` | `/market/bars/{symbol}` | `timeframe`, `limit` |
| `GET/POST` | `/market/news/{symbol}` | optional symbol, `limit` |
| `GET/POST` | `/market/clock` | none |
| `GET/POST` | `/market/calendar` | `start`, `end`, `market` or `markets` |

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
