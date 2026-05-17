# Configuration

`mlai-trade` reads local runtime configuration from:

```text
~/mlai-trade/config/mlai-trade.json
```

The runtime home defaults to `~/mlai-trade`. You can override it for a run:

```sh
mlai-trade --home /path/to/mlai-trade runtime version
```

or by setting `MLAI_TRADE_HOME` in the process environment before launch.

The CLI creates these folders automatically:

- `bin/`
- `config/`
- `data/`
- `db/`
- `docs/`
- `logs/`
- `logs/archived/`
- `api/`
- `tmp/`

PID files are runtime control files and default to `tmp/` inside the runtime home.

Real credentials must stay in the local runtime config file. Do not commit them. The repository tracks only `config/mlai-trade.example.json` with placeholder values.

## Runtime Security

The CLI enforces private runtime permissions on startup and when sensitive files are created:

| Runtime Path | Mode | Notes |
| --- | --- | --- |
| `~/mlai-trade` | `0700` | Runtime home is private to the OS user. |
| `config/`, `data/`, `db/`, `logs/`, `api/`, `tmp/` | `0700` | Sensitive runtime directories. |
| `config/mlai-trade.example.json`, `config/mlai-trade.json`, `config/tax-brackets*.json` | `0600` | Config files and examples are private in the runtime copy. |
| `db/*` | `0600` | SQLite DBs and related files are private. |
| `data/*` | `0600` | Generated datasets, models, reports, and CSV exports are private. |
| `logs/*`, `api/mlai-trade-api.sock` | `0600` | Runtime audit files and the local API socket are private. |
| `tmp/*.pid` | `0644` | PID files are runtime metadata. |

Blank or relative path overrides for logs, sockets, PID files, and tax brackets resolve inside the expected runtime folder (`logs/`, `api/`, `tmp/`, or `config/`). This prevents accidental writes into the caller's current directory. API and daemon-captured command output is redacted for configured Alpaca and FRED secrets before it is logged or returned.

Provider enablement is explicit. At least one provider must be enabled or the CLI exits:

```json
{
  "providers": {
    "alpaca": { "enabled": true },
    "other": {}
  }
}
```

Future providers can be added under `providers.other` without changing the config shape.

## Required Example

The authoritative template is:

```text
config/mlai-trade.example.json
```

It intentionally lists every supported configuration key. The runtime file should keep the same shape and replace only local values such as API keys, account names, and enabled flags.

## Providers And Accounts

`providers.alpaca.enabled` enables the Alpaca provider module. `providers.other` is a generic enablement map reserved for future provider modules.

`alpaca.accounts[]` can contain multiple accounts. Each account has:

- `name`: local account reference used in selectors such as
  `alpaca:paper-main`. For Alpaca, provider sync stores the stable broker
  account ID separately and uses that ID to reconcile old local rows if this
  local name is changed later. Account names must be unique within the same
  provider because `provider:account-ref` selectors need one unambiguous
  account. A future second provider may reuse the same local name because its
  selector would have a different provider prefix.
- `enabled`: include or skip the account.
- `auto_trade_enabled`: allow or block autonomous buy/sell decisions for this
  account. Provider sync, account status, orders, positions, tax simulation,
  and compliance reconciliation still run when the account is enabled.
- `account_mode`: `paper` or `individual`.
- `data_feed`: `auto`, `sip`, or `iex`.
- `trading_base_url`: optional endpoint override for trading, account, order,
  clock, and calendar calls. Leave blank for official Alpaca endpoints. Tests
  point this at the local fake Alpaca fixture.
- `data_base_url`: optional endpoint override for market-data, news, and
  screener calls. Leave blank for official Alpaca market-data endpoints. Tests
  point this at the local fake Alpaca fixture.
- `api_key_id` and `secret_key`: local credentials, never committed.

Paper and real accounts are separate execution universes. Real accounts share
real-money compliance blockers across accounts. Paper accounts obey the same
rules in a separate paper compliance universe.

Endpoint overrides are per account. Do not set a provider-level API key or
provider-level data feed; each account owns its credentials, feed mode, and
optional test endpoint overrides.

Changing an account `name` does not create a new broker account. On the next
provider order/fill sync, `mlai-trade` uses Alpaca's broker account ID to move
old local rows from the previous account reference onto the current configured
name. This keeps order/fill snapshots, auto positions/trades, day-trade rows,
and wash-sale monitor rows from being duplicated after a local rename.

Auto-trade remains cash-only even when Alpaca paper or brokerage accounts
report margin buying power. Before each buy decision, the cycle syncs provider
orders/fills, fetches live account and position snapshots, and emits
`cash_only_guard` fields. Deployable cash is the lower of provider cash and
equity after current long exposure plus pending buy reservations, which prevents
stale provider cash from being reused by a later daemon cycle.

The provider's live position snapshot is also checked before any exit order.
Local open `auto_positions` are repaired if the provider no longer reports the
shares. This prevents stale local rows from submitting a sell that Alpaca would
reject as a short sale. Repairs are logged as
`auto_position_reconciled_from_provider`.

Provider sync also stores account cash/equity snapshots and provider orders and
fills. If activity happened directly at Alpaca instead of through mlai-trade,
the sync keeps those provider rows as source-of-truth records and emits
`provider_external_order_observed`, `provider_external_fill_observed`, or
`provider_account_snapshot_changed`. Equity and portfolio values are refreshed
in the snapshot every sync, but only provider cash changes emit
`provider_account_snapshot_changed` to avoid mark-to-market log noise.
Provider orders and fills are classified as `mlai_auto`, `mlai_cli`,
`provider_external`, `mixed`, or `unknown`. New CLI orders use the
`mlai-cli-*` client order id prefix; older `plm-*` rows remain CLI-originated.
The classification is stored in `provider_order_snapshots`,
`provider_fill_activities`, `auto_positions`, and `auto_trades` so reports can
separate provider-web/manual activity, CLI trades, auto trades, and overall
P&L.

Exit rules are confirmed before normal stop-loss or take-profit sells:

- `auto.stop_loss_pct`: normal loss threshold. With the default
  `auto.stop_loss_confirmation.enabled=true`, this must breach for
  `cycles=3` consecutive auto-trade cycles before selling.
- `auto.stop_loss_confirmation.emergency_stop_loss_pct`: deeper emergency loss
  threshold that sells immediately. Default is `10.0`.
- `auto.stop_loss_confirmation.max_confirmation_minutes`: maximum wait for a
  normal stop-loss confirmation. Default is `5`.
- `auto.take_profit_pct`: normal profit threshold. With the default
  `auto.take_profit_confirmation.enabled=true`, this must breach for
  `cycles=3` consecutive auto-trade cycles before selling.
- `auto.take_profit_confirmation.min_hold_minutes`: minimum hold before a
  take-profit sell. Default is `5`.
- `auto.take_profit_confirmation.trailing_enabled`: after a position has
  crossed the take-profit threshold, sell if it gives back
  `trailing_giveback_pct` percentage points from the best observed profit.

Confirmation state is stored in `auto_positions`, so daemon restarts do not
forget active breach counters or take-profit peaks. The auto log records
`auto_exit_confirmation_wait` while waiting, then
`auto_exit_rule_triggered`/`auto_exit_order_submitted` when a rule actually
submits a sell order.

## Daemon

`daemon.enabled` controls whether `mlai-trade daemon start` is allowed. The daemon loop runs auto-trade cycles, tax-estimate refresh, log rotation, and an optional once-per-day maintenance refresh.

| Key | Default | What It Does |
| --- | --- | --- |
| `enabled` | `false` | Allows or refuses daemon lifecycle commands. `mlai-trade daemon start` exits with an error when this is `false`. |
| `auto_trade_interval_seconds` | `60` | How often the daemon checks provider accounts for auto-trade decisions. The value is clamped to `10`-`300`. |
| `dashboard_bar_cache_enabled` | `true` | See below. |
| `dashboard_bar_cache_interval_seconds` | `60` | See below. |
| `dashboard_bar_cache_symbols_limit` | `100` | See below. |
| `daily_refresh_enabled` | `true` | Enables the daemon's once-per-market-date non-trading prep job. This job never buys or sells. |
| `daily_refresh_trigger` | `market_close` | Chooses when the daily prep job becomes eligible. `market_close` is market-aware. `time` uses the fixed `daily_refresh_time` clock. |
| `daily_refresh_after_close_minutes` | `360` | Market close delay. |
| `daily_refresh_time` | `18:30:00` | With `daily_refresh_trigger=time`, runs after this local time. This mode is a raw clock fallback and does not use the market-close trigger. |
| `daily_refresh_timezone` | `America/New_York` | Timezone used to calculate the market-local date and trigger time. |
| `daily_refresh_days` | `0` | Passed to `ml refresh --days`. `0` means first-run full available history discovery and later incremental missing/latest-day refresh. |
| `daily_refresh_quick` | `false` | Adds `--quick` to the daemon-run `ml refresh`; intended for validation runs, not production prep. |
| `daily_refresh_walk_forward_folds` | `5` | Passed to `ml refresh --walk-forward-folds`. |
| `daily_refresh_top_n` | `20` | Passed to `ml refresh --top-n` for trading-metric evaluation. |
| `daily_refresh_slippage_bps` | `50` | Passed to `ml refresh --slippage-bps` so validation uses post-slippage metrics. |
| `daily_refresh_sync_orders` | `true` | Runs `mlai-trade auto sync-orders` before ML prep so provider orders/fills and bought-symbol feeds are current. |
| `daily_refresh_feeds_sync` | `true` | Runs an extra `mlai-trade feeds sync` after ML prep. The ML refresh itself still syncs feeds before training when `feeds.sync_before_training=true`. |
| `daily_refresh_feeds_days` | `7` | Number of recent days requested by the extra post-refresh `feeds sync`. |
| `pid_file` | blank | Optional override. Blank means `tmp/mlai-trade-daemon.pid`. |
| `log_file` | blank | Optional override. Blank means `logs/mlai-trade-daemon.log`. |

Dashboard bar cache warmup is a daemon-side helper for the React dashboard. It
preloads `market_bar_cache` for current provider positions, using the same
chart intervals the dashboard asks for. The interval defaults to 60 seconds and
is clamped to 30-300. One-minute bars use that cadence. Wider intervals have
longer freshness windows, so 5-minute, 15-minute, hourly, and daily bars are not
refetched every daemon tick. The symbol cap defaults to 100 and is clamped to
1-500.

When `daemon.daily_refresh_enabled=true` and `daemon.daily_refresh_trigger=market_close`, the daemon checks the configured market-local clock on every daemon loop. It runs daily prep only when all of these are true:

- The daemon is running and daemon mode is enabled.
- The current market-local date is not Saturday or Sunday.
- The current market-local date is not listed in `auto.market.closed_dates`.
- The current time is at least `auto.market.regular_close + daemon.daily_refresh_after_close_minutes`.
- `tmp/mlai-trade-daily-refresh.stamp` does not already contain the current market-local date.

That means the default behavior is: run once per open New York market date,
about 6 hours after the regular close. After a successful run, the stamp file
prevents another daily prep run for the same market-local date.

Successful manual full prep after market close also writes the same stamp.
That means `mlai-trade data daily`, `mlai-trade ml refresh`, and
`mlai-trade ml full-refresh` satisfy the daemon's once-per-market-date job
when they complete successfully outside the daemon.

The daily maintenance order is:

1. Rotate/sanitize JSON logs.
2. Sync provider orders/fills when `daemon.daily_refresh_sync_orders=true`.
3. Run `ml refresh` with `--days`, `--backend auto`, walk-forward folds, top-N, slippage, and optional `--quick`. This is the same shared full incremental pipeline used by `data daily` when `--skip-train` is not set.
4. Inside `ml refresh`, refresh the universe, FRED data, Alpaca bars, managed feed universe, feed sync, features, labels, model training, validation, predictions, ensemble, and SHAP cache.
5. Run an extra subscribed feed sync when `daemon.daily_refresh_feeds_sync=true`.
6. Refresh current-year tax estimates.
7. Write `tmp/mlai-trade-daily-refresh.stamp`.

If any step fails, the daemon logs a JSON `daily_maintenance_failed` event and retries later instead of writing the success stamp.

When every enabled account reports `market_closed`, the daemon logs `auto_market_closed_backoff_started` and suppresses further daemon-driven auto-trade cycles until the next configured market date. Tax refresh and daily maintenance are separate jobs and are not disabled by this trading backoff.

`daemon status --details` reads the daemon heartbeat from `tmp/mlai-trade-daemon-status.json` and reports loop count, last auto-trade summary, last daily-refresh summary, CPU time, RSS memory, open file descriptor count, and thread count when available. Metrics that are not available on a platform are reported as `not available`.

Lifecycle commands:

```sh
mlai-trade daemon start
mlai-trade daemon status
mlai-trade daemon status --details
mlai-trade daemon reload
mlai-trade daemon restart
mlai-trade daemon stop
```

## API

`api.enabled` is the master API switch. `api.unix` controls the local
Unix-socket transport. `api.ssl` controls the optional remote HTTP/3-over-QUIC
transport and the built React dashboard.

The Unix transport listens on a local Unix socket and exposes only an explicit
allowlist of CLI actions as JSON responses.

- `enabled`: `true` or `false`.
- `unix.enabled`: `true` or `false`. Defaults to `true` when `api.enabled=true`.
- `unix.socket_file`: optional override; blank means
  `api/mlai-trade-api.sock`. The socket file is created with `0600`
  permissions.
- `unix.pid_file`: optional override; blank means `tmp/mlai-trade-api.pid`.
- `unix.log_file`: optional override; blank means `logs/mlai-trade-api.log`.
- `socket_file`, `pid_file`, `log_file`: legacy aliases for `unix.*`;
  accepted for backward compatibility.
- `request_timeout_seconds`: default `60`, clamped to `5`-`300`, used for normal API calls.
- `long_request_timeout_seconds`: default `3600`, clamped to `60`-`86400`, used for `ml refresh` and `feeds sync`.
- `max_concurrent_requests`: default `8`, clamped to `1`-`128`; maximum command requests running at the same time.
- `max_concurrent_long_requests`: default `1`, clamped to `1`-`16`; maximum long operations such as `ml refresh` or `feeds sync` running at the same time.
- `rate_limit_per_minute`: default `120`, clamped to `1`-`10000`; process-local API request budget per rolling minute.
- `max_body_bytes`: default `65536`, clamped to `1024`-`1048576`; oversized bodies are rejected with HTTP `413`.
- `overload_retry_after_seconds`: default `5`, clamped to `1`-`300`; retry hint returned when concurrency is exhausted.

Remote HTTPS/H3 policy:

- `ssl.enabled`: `true` or `false`; also requires `api.enabled=true`.
- `ssl.auth.enabled`: require authentication for non-localhost remote clients.
  Startup refuses non-loopback binds when auth is disabled or still uses the
  example password. Browsers use the `/login` form and receive a secure
  HttpOnly 30-day session cookie; API clients can still send HTTP Basic auth
  proactively, but the server does not emit a browser-native Basic challenge.
- `ssl.auth.username` / `ssl.auth.password`: remote dashboard/API credentials.
  Localhost source traffic bypasses auth so `https://localhost/` works as a
  local dashboard without a login prompt.
- `ssl.ech.enabled`: planned Encrypted ClientHello switch. It defaults to
  `false`; when set to `true` before the server TLS stack supports RFC 9849,
  startup fails closed and logs `api_ssl_ech_unsupported`.
- `ssl.ech.public_name`: ECH public name intended for DNS HTTPS/SVCB
  publication when an ECH-capable listener is available.
- `ssl.ech.config_file` / `ssl.ech.key_file`: ECH config/key paths under
  `config/cert/`. `api ssl status --json` reports whether these files exist.
- `ssl.ech.require_dns_https_record`: require DNS HTTPS/SVCB `ech` records
  during public-domain DNS validation when ECH is requested.
- `ssl.domain`: public DNS name for the certificate and HTTPS/SVCB record.
- `ssl.bind_host`: QUIC bind host, default `0.0.0.0`.
- `ssl.ipv4_enabled` / `ssl.ipv6_enabled`: enable the IPv4 and IPv6 listener
  stacks. Both default to `true`. At least one stack must remain enabled.
- `ssl.udp_port`: QUIC UDP port, default `443`.
- `ssl.tcp_enabled`: enables the TCP HTTPS listener, default `true`. It serves
  the React dashboard and allowed JSON API routes, and advertises `Alt-Svc` for
  H3/QUIC upgrades.
- `ssl.tcp_bind_host` and `ssl.tcp_port`: TCP HTTPS listener address,
  defaulting to the QUIC bind host and UDP port.
- `ssl.tcp_bootstrap_*`: legacy aliases accepted for backward compatibility.
  New configs should use `ssl.tcp_*`.
- `ssl.trusted_proxy_enabled`: allow trusted reverse-proxy/tunnel forwarding
  headers to set the computed `client_ip`; default `true`.
- `ssl.trusted_proxy_cidrs`: direct socket peer CIDRs that may supply trusted
  forwarding headers. Defaults to loopback, RFC1918/private IPv4,
  IPv4 link-local, IPv6 unique-local, and IPv6 link-local ranges. For a
  Cloudflare Tunnel, narrow this to the local tunnel peer IP when possible,
  for example `10.0.1.254/32`.
- `ssl.pid_file`: optional override; blank means `tmp/mlai-trade-api-ssl.pid`.
- `ssl.log_file`: optional override; blank means `logs/mlai-trade-api-ssl.log`.
- SSL/H3 request logs always preserve the direct socket `source_ip/source_port`.
  When traffic arrives through a local/private tunnel or reverse proxy, the log
  also records `CF-Ray` when present and a computed `client_ip`. Forwarding
  headers such as `CF-Connecting-IP`, `True-Client-IP`, `X-Forwarded-For`, and
  `X-Real-IP` are trusted for `client_ip` only when the direct socket peer is
  loopback, private, or link-local. `client_ip_source` is generic, for example
  `cloudflare`, `trusted_proxy`, or `socket_source_ip`; raw forwarding header
  values are not emitted in logs.
- `ssl.cert_mode`: `provided`, `self_signed`, or `letsencrypt`.
- `ssl.cert_file` and `ssl.key_file`: blank resolves under `config/cert/`.
- `ssl.acme_challenge_cert_file` and `ssl.acme_challenge_key_file`: blank
  resolves under `config/cert/` for the RFC 8737 TLS-ALPN-01 challenge
  certificate generated by `api ssl cert generate --target acme` or
  `api ssl cert renew --target acme`.
- Certificate lifecycle: `api ssl cert info --target h3|acme` reports
  properties and expiry. `generate|renew` require `--target h3` or
  `--target acme`; `--acme-key-authorization` is accepted only for
  `--target acme`. On startup, mlai-trade auto-renews only mlai-trade-generated
  certificates when `ssl.cert_mode=self_signed`; provided/public CA
  certificates are never overwritten. The ACME TLS-ALPN-01 challenge
  certificate is generated per challenge and is not auto-renewed because real
  renewal requires the current ACME key authorization from the active CA order.
- Generated H3 and ACME certificate subjects default to `O=MLAI-TRADE` and
  `OU=MLAI-TRADE`. Override with `api ssl cert generate|renew
  --organization VALUE --organizational-unit VALUE` when a different subject
  organization is needed.
- `ssl.key_exchange_policy`: `mlkem_secure_fallback` or `mlkem_required`.
  `mlkem_secure_fallback` is the default for browser compatibility: it offers
  hybrid ML-KEM first and then allows only strong TLS 1.3 classical groups
  (`X25519`, `P-256`, `P-384`). `mlkem_required` disables classical fallback
  and is appropriate only for clients known to support the required groups.
- `ssl.dns_https_check_required`: require DNS HTTPS/SVCB discovery validation
  before startup.
- `ssl.tcp_acme_tls_alpn_enabled`: enables a TCP TLS-ALPN-01 challenge responder
  only when `ssl.cert_mode=letsencrypt`. It is disabled by default and exposes
  no API routes.
- `ssl.tcp_acme_bind_host` and `ssl.tcp_acme_port`: ACME challenge listener
  address, default `0.0.0.0:443`.

IPv6 bind hosts are supported. Use `::` for an IPv6 wildcard listener or `::1`
for IPv6 localhost-only testing. Logs use the concrete accepted destination IP,
so wildcard binds still log `127.0.0.1`, `::1`, or the interface address that
actually received the request.

Public remote discovery should use DNS HTTPS/SVCB with `alpn=h3` and port `443`
when possible. Browsers can connect over TCP HTTPS and upgrade to H3 when they
honor `Alt-Svc`. Apps can still choose to require H3 only.

SNI encryption requires TLS Encrypted ClientHello (ECH), which is not a
certificate option. The config shape is `api.ssl.ech.*`, and `api ssl
dns-check` validates DNS HTTPS/SVCB `ech` parameters when ECH is requested.
Actual server-side ECH termination is still blocked by the current rustls/quinn
listener stack; if `ssl.ech.enabled=true`, startup fails closed instead of
serving public traffic with plaintext SNI. Until an ECH-capable listener lands,
public domain names can still appear in the TLS ClientHello SNI.
OpenSSL is the preferred ECH implementation path because it exposes documented
server-side ECH APIs. BoringSSL is browser-proven but not a stable CLI/library
distribution target.

The default ACME challenge TCP port is `443`, but ACME is off unless
`ssl.cert_mode=letsencrypt` and `ssl.tcp_acme_tls_alpn_enabled=true`.

Remote webapp files live under `api/html/` in the runtime home. The repository
contains the React source under `api/html/src` and public static files under
`api/html/public`; `npm run build` creates `api/html/dist`, which is the only
directory the remote HTTPS/H3 listener serves. The dashboard reads real API
routes for providers/accounts/stocks, auto trading, data, and compliance. It
auto-refreshes read-only live data after page load and does not force provider
sync during normal refresh; use its explicit sync action when manual
reconciliation is wanted.
Position symbols open a dashboard insight overlay that combines feed sentiment
and ML explain output from the API, including plain-English feature
descriptions for SHAP rows.
The dashboard also opens `/events/stream` for realtime refresh hints. The
stream is `text/event-stream` over the active browser transport: HTTP/3/QUIC
when H3 is active, otherwise TCP HTTPS. If the stream is unavailable, the
dashboard falls back to snapshot polling. `/limits` advertises the stream path,
snapshot path, refresh interval, heartbeat interval, stream lifetime, and
maximum active stream count so browser and mobile clients do not hardcode these
values.
`api/html/public/robots.txt` is copied into `api/html/dist/robots.txt` and
disallows all crawlers and common AI-agent user agents, but that is advisory
only; auth is the real protection for non-localhost clients.

Lifecycle and health commands:

```sh
mlai-trade api start
mlai-trade api status
mlai-trade api status --details
mlai-trade api test
mlai-trade api reload
mlai-trade api restart
mlai-trade api stop
```

These legacy commands target the Unix transport. The explicit form is
`mlai-trade api unix start|status|test|stop`. Remote planning/status commands
are `mlai-trade api ssl status` and `mlai-trade api ssl dns-check DOMAIN`.

`api test` sends `GET /health` through the configured Unix socket.
`api status --json` lists the allowlisted sections and actions.
`api status --details` prints both Unix-socket runtime counters and SSL/H3
runtime counters when the remote listener is running, so browser dashboard
market-bar cache hit/provider-fetch rates are visible in the `SSL/H3 Runtime`
block. The same detail view also includes realtime stream counters. `api ssl
status --details` prints only the remote listener counters.
Runtime commands are not exposed through the API. Trade mutation endpoints
(`buy`, `sell`, `cancel`, `close`) are rejected while auto-trading is enabled.

The full API route list, request parameters, response wrapper, and curl examples are documented in `docs/API.md`.

API errors are explicit. If an underlying CLI command returns JSON with `ok:false`, the API wrapper returns `ok:false` with a non-2xx status. Command JSON can include `status_code`/`http_status` to request a specific error status such as `404`.

API overload protection is explicit. If request rate or concurrency is exhausted, the API returns HTTP `429` with `ok:false`, `reason`, and `retry_after_seconds`; clients should back off instead of immediately retrying. This is backpressure only; the API does not cache responses. CLI stdout/stderr captured by the API is redacted for configured Alpaca and FRED secrets before it is returned or logged.

## Logs

Active logs are written under `logs/` by default:

- `mlai-trade-daemon.log`
- `mlai-trade-api.log`
- `mlai-trade-auto.log`
- `mlai-trade-data.log`
- `mlai-trade-ml.log`
- `mlai-trade-training.log`
- `mlai-trade-feeds.log`

All application logs are JSON lines. Logs rotate daily. The active file keeps
the stable name in `logs/`, and older content is gzip-compressed under
`logs/archived/` as `YYYYMMDD-<log-file>.gz`, for example
`logs/archived/20260502-mlai-trade-auto.log.gz`. Rotation uses the first JSON
event timestamp in each active file, so long-running daemon/API processes still
archive stale logs even when they keep writing after midnight.

The optional `logging` config section can override component log paths. Blank or relative values resolve under `logs/`; absolute paths outside the runtime logs directory are reduced to their filename under `logs/` so application logs stay in one private folder:

- `data_log_file`
- `ml_log_file`
- `training_log_file`
- `feeds_log_file`

Default component logs:

| Config Key | Default |
| --- | --- |
| `logging.data_log_file` | `logs/mlai-trade-data.log` |
| `logging.ml_log_file` | `logs/mlai-trade-ml.log` |
| `logging.training_log_file` | `logs/mlai-trade-training.log` |
| `logging.feeds_log_file` | `logs/mlai-trade-feeds.log` |

Command lifecycle records are written to these component logs for data, feeds, ML, and training commands. Each record is one JSON object per line with `event`, `component`, `command`, `source`, `duration_ms`, and error fields when applicable.
Zero-byte active log files are normal after rotation when that component has
not emitted a new event yet.

## Feeds

`feeds` controls news/filing feed collection and feed-derived ML features:

- `sync_before_training`: default `true`; `ml refresh` and `data daily`
  reconcile/sync feeds before feature computation and training.
- `sync_orders_before_training`: default `true`; syncs provider orders/fills
  first so bought symbols are current.
- `compute_correlations_before_training`: default `true`; computes bounded
  feed-universe price correlations before ML features/training.
- `include_current_sp500`: default `true`; current S&P 500 symbols seed the
  feed universe only.
- `include_open_positions`: default `true`; provider and auto-trade open
  positions are included.
- `include_bought_symbols`: default `true`; recent provider buys are included.
- `bought_symbol_lookback_days`: default `365`.
- `include_q1_candidates`: default `true`; latest Q1 ML candidates are included.
- `q1_top_n`: default `500`.
- `sync_days`: default `30`; feed sources are queried for this recent window.
- `source_timeout_seconds`: default `10`; timeout for each source/symbol
  request attempt.
- `source_retry_count`: default `2`; retries after the first failed or
  timed-out attempt.
- `auto_tune_sources`: default `true`; lowers a source's concurrency after
  timeout/error waves and raises it again after clean waves.
- `alpaca_concurrency`: default `2`; concurrent Alpaca news symbol queries.
- `sec_edgar_concurrency`: default `1`; SEC is deliberately conservative.
- `yahoo_rss_concurrency`: default `2`; concurrent Yahoo RSS symbol queries.
- `google_rss_concurrency`: default `2`; concurrent Google RSS symbol queries.
- `correlation_days`: default `90`; lookback window for feed-subscription price
  correlations.
- `correlation_min_overlap_days`: default `30`; minimum overlapping trading
  days required for a pair.
- `correlation_strong_threshold`: default `0.7`; absolute threshold that
  creates a `price_correlated` relationship edge.
- `correlation_max_symbols`: default `1500`; maximum feed symbols used for
  pairwise correlations so the pair set stays bounded.
- `extra_symbols`: config-managed extra symbols that should always be included.

Managed feed subscriptions are reconciled every run. Symbols no longer needed by S&P 500/current positions/recent buys/Q1/config are removed from the managed subscription list. Manual subscriptions added with `mlai-trade feeds add` are not removed by reconciliation.

`feeds sync` runs source passes with per-source concurrency rather than one
fully serialized symbol/source loop. The default behavior is still conservative:
SEC remains single-query, while Alpaca/Yahoo/Google run two symbol queries at a
time. Each source writes JSON summary events to `logs/mlai-trade-feeds.log`
with articles, errors, timeouts, attempts, configured concurrency, and final
auto-tuned concurrency.

Current S&P 500 membership is intentionally not a model feature because that
would introduce survivorship bias without point-in-time membership data. It is a
data-collection universe only. The model receives symbol/date feed aggregates
such as sentiment windows, article counts, 8-K counts, Form 4 counts,
negative-news counts, and point-in-time managed-feed-universe return/correlation
features derived only from bars available on that feature date.

### Feed Reconciliation

`mlai-trade data daily` and `mlai-trade ml refresh` both run feed reconciliation when `feeds.sync_before_training=true`. The daemon daily job runs `ml refresh`, so daemon daily prep also uses the same feed reconciliation path.

Feed reconciliation rebuilds the desired managed feed universe each run:

| Source | Config Key | Effect |
| --- | --- | --- |
| Current S&P 500 list | `feeds.include_current_sp500` | Adds the current S&P 500 symbols to the managed feed universe for data collection only. |
| Open auto positions and provider positions | `feeds.include_open_positions` | Keeps symbols currently held by any enabled account in the feed universe. |
| Recent provider buy fills | `feeds.include_bought_symbols` and `feeds.bought_symbol_lookback_days` | Keeps recently bought symbols in the feed universe. |
| Latest Q1 ML candidates | `feeds.include_q1_candidates` and `feeds.q1_top_n` | Adds the top latest predicted-quintile-1 candidates to the feed universe. |
| Config extras | `feeds.extra_symbols` | Always keeps these symbols in the managed feed universe. |

For each desired symbol, the reconciler upserts `feed_subscriptions` with `managed=1`, updates `subscription_source`, preserves any existing CIK, and fills missing CIK values when SEC lookup has one. Existing managed symbols that are no longer desired are removed. Existing manual subscriptions created with `mlai-trade feeds add` have `managed=0`; they are kept unless the user removes them with `mlai-trade feeds remove SYMBOL`.

After reconciliation, feed sync pulls recent Alpaca news, SEC EDGAR filings, Yahoo RSS, and Google RSS for every subscribed symbol. Those articles/filings are converted into dated feed aggregates and included in ML feature computation before training.

## Tax

`tax` contains the inputs for the federal estimate:

- `filing_status`: `single`, `married_filing_jointly`, `married_filing_separately`, or `head_of_household`.
- `estimated_annual_income`: estimated annual taxable ordinary income before trading gains. The example default is `1000000.0`.
- `include_paper_accounts_for_estimate`: defaults to `false`.
- `brackets_file`: JSON file under `config/` containing ordinary income, regular long-term capital gains, and Net Investment Income Tax rates and thresholds. The default is `tax-brackets.json`.

The example default filing status is `married_filing_jointly`.

Tax brackets and percentages are data, not code. Copy `config/tax-brackets.example.json` to `~/mlai-trade/config/tax-brackets.json`. When IRS publishes a new year, add that year to the JSON file and review the diff.

`mlai-trade compliance tax --accounts` lists tax-visible account selectors. `mlai-trade compliance tax --show-brackets --year YYYY` lists the configured filing-status brackets for ordinary/short-term gains, long-term capital gains, and Net Investment Income Tax. `mlai-trade compliance tax --year YYYY` calculates the year-to-date/current-year estimate for all real accounts by default. Add `--account SELECTOR` to select one or more accounts, including paper accounts for simulation, and add `--details` to list estimated tax impact per matched operation. Add `--quarter 1`, `--quarter 1,2`, or `--quarter 1-4` to select one contiguous quarter period. Estimates are persisted in `db/tax.db`. `--export csv` writes `data/tax_YYYY_<period>.csv`.

## Market Calendar And Clock

`auto.market` controls when auto-trade may run:

- `mode=auto`: Alpaca v3 calendar + Alpaca v3 clock + local schedule.
- `mode=provider`: provider calendar/clock only unless local checks are explicitly enabled.
- `mode=local`: local configured schedule only.
- `timezone`: defaults to `America/New_York`; this controls local exchange-hour guardrails and is stored with trade records.
- `provider_markets`: defaults to `["NYSE", "NASDAQ"]`.
- `regular_open` / `regular_close`: local regular-session guardrail.
- `buy_start` / `buy_end`: local buy window.
- `sell_start` / `sell_end`: local sell window.
- `closed_dates`: local override dates, `YYYY-MM-DD`.

Alpaca v3 calendar is queried in UTC. The code stores UTC timestamps for events and stores the configured/provider market timezone/session context alongside trade records.

Manual verification:

```sh
mlai-trade market clock
mlai-trade market calendar
mlai-trade market calendar --market NYSE --market NASDAQ
```

## Compliance

Legal and regulatory floors are compiled into code. Config can make behavior stricter, not weaker.

`auto.compliance.wash_sale_safety_buffer_days` defaults to `1`. The IRS wash-sale replacement window is hardcoded at 30 days, so the default forward block is 31 days after a loss sale. Setting the buffer below 1 is rejected or clamped by the code path.

`auto.compliance.blocked_symbols` is a user/company policy list. It supports multiple symbols and normalizes input to uppercase before comparison, so `meta`, `Meta`, and `META` all block market symbol `META`.

Dollar thresholds from tax/regulatory sources are not user-tunable downward. If a future config exposes a dollar safety buffer, the effective threshold must be the hardcoded floor plus the user buffer.

`auto.log_file` optionally overrides the auto-trade audit log path. Blank means
`logs/mlai-trade-auto.log`. Entries are JSON lines and include `source`
(`daemon`, `cli`, or `api`), cycle status, per-account results, buys, sells,
skipped reasons, market-closed decisions, provider sync summaries, errors, and
exit-confirmation events. Use `jq` to inspect why a stop-loss or take-profit
waited or sold:

```sh
tail -f ~/mlai-trade/logs/mlai-trade-auto.log \
  | jq 'select(.event | startswith("auto_exit_"))'
```

## Provider Order Sync

`mlai-trade auto sync-orders` is a read-only provider sync. For Alpaca accounts, it stores provider order snapshots in `provider_order_snapshots` and fill activities in `provider_fill_activities` inside `db/mlai_trade.db`.

The first sync starts at the oldest provider history available. Later syncs
rewind the latest local provider timestamp by one day, refresh that day, and
fill forward. Auto-trade runs sync before account decisions and sync again after
confirmed provider orders.

Every provider fill sync also reconciles missed wash-sale monitor rows from
provider-confirmed fills. Paper accounts are one isolated paper tax universe.
Real-money accounts are one shared IRS-relevant universe across all real
provider accounts. The blocker is by symbol and tax universe; provider/account
fields are retained on the row for audit and source-of-truth traceability.

## ML Backends

`backend.lstm` supports `auto`, `cpu`, `mlx`, or `tch`.

- `auto`: choose the best available backend for the platform.
- `mlx`: Apple Silicon MLX path on macOS/aarch64 builds.
- `tch`: Linux/NVIDIA CUDA target path. The Linux dependency self-provisions
  libtorch. Apple Silicon PyTorch/MPS exists, but mlai-trade does not use a
  `tch`/MPS trainer today; Apple Silicon acceleration uses MLX.
- `cpu`: portable fallback.

In `auto`, accelerator runtime failures fall back to CPU/Rayon. This includes
MLX Metal library load failures and tch/CUDA unavailability. If the user forces
`mlx` or `tch`, runtime failures are returned as command errors because the
selected backend was explicit.

`backend.xgboost` supports `auto`, `cpu`, or `cuda` on macOS and Linux builds.
FreeBSD uses the portable CPU baseline without XGBoost until native FreeBSD
linking is implemented and validated. `backend.lightgbm` and `backend.ridge`
are CPU-only in the current Rust implementation and should remain `cpu`.

## ML Tuning

Model hyperparameters that are likely to change during research live outside
the provider/runtime config:

```text
~/mlai-trade/config/mlai-trade-ml-tuning.json
```

Start from:

```text
config/mlai-trade-ml-tuning.example.json
```

This file is private runtime configuration and is ignored by Git. It contains
no provider credentials by default, but it should still be `0600` because local
tuning can reveal strategy choices. The public example is tracked and documents
all supported keys.

Current tuning sections:

- `lstm.profile`: `auto`, `cpu`, `mlx`, or `tch`. `auto` waits for
  `backend.lstm` resolution first, then uses the matching profile. If MLX or
  tch fails in backend auto mode and the trainer falls back to CPU, the CPU
  profile is used.
- `lstm.profiles.cpu`: portable Rust/Rayon profile. Defaults are intentionally
  conservative for CPU-only and low-memory hosts.
- `lstm.profiles.mlx`: Apple Silicon MLX profile. Defaults use a wider model
  and longer training because MLX uses Apple Silicon GPU/unified memory.
- `lstm.profiles.tch`: Linux/NVIDIA target profile. It mirrors the accelerator
  policy so CUDA hosts can use a wider profile when tch/CUDA training is
  enabled and validated; auto falls back to CPU if tch is unavailable.

Each LSTM profile supports:

- `target_mode`: `regression` predicts forward returns. `direction` predicts
  probability that the forward return is above `direction_threshold`.
- `direction_threshold`: threshold used by direction mode. Default `0.0`.
- `hidden_dim`: LSTM hidden width, valid `16`-`512`.
- `epochs`: max epochs, valid `1`-`200`.
- `learning_rate`: Adam learning rate, valid `0.000001`-`0.1`.
- `loss_function`: `mse`, `huber`, `l1`, or `bce`. `bce` requires
  `target_mode=direction`.
- `huber_delta`: Huber transition size when `loss_function=huber`.
- `dropout_rate`: output-head dropout during training, valid `0.0`-`0.9`.
- `weight_decay`: AdamW-style weight decay, valid `0.0`-`1.0`.
- `early_stopping_enabled`: stop when validation loss no longer improves.
- `early_stopping_patience`: epochs without improvement before stopping.
- `early_stopping_min_delta`: minimum validation-loss improvement.
- `early_stopping_sample_size`: validation sample cap used for early stopping.

Built-in defaults when the tuning file is absent:

| Profile | Target | Hidden | Epochs | LR | Loss |
| --- | --- | ---: | ---: | ---: | --- |
| `cpu` | `regression` | 64 | 10 | 0.001 | `mse` |
| `mlx` | `regression` | 128 | 50 | 0.0001 | `mse` |
| `tch` | `regression` | 128 | 50 | 0.0001 | `mse` |

Regularization defaults:

| Profile | Dropout | Weight Decay | Early Stop |
| --- | ---: | ---: | --- |
| `cpu` | 0.0 | 0.0 | patience 5 |
| `mlx` | 0.1 | 0.01 | patience 10 |
| `tch` | 0.1 | 0.01 | patience 10 |

The accelerator defaults above come from the paused 365-day real-data sweep at
442 completed variants. The selected balanced result was
`h128_lr0p0001_mse0_do0p1_wd0p01`, with direction accuracy `55.4%`, eval IC
`0.2122`, standalone mean return `3.5560`, ensemble IC `0.1971`, ensemble mean
return `5.8765`, and ensemble win rate `60.8%` using `LightGBM=40%` and
`LSTM=60%`. The sweep can be resumed later to finish all 649 variants; if that
changes the winner, update this section and the tuning example together.

Sequence scaling and technical indicators are already part of the LSTM
pipeline: windows are z-scored before training, regression targets are
z-scaled and decoded back into return space, and the feature vector includes
returns, volatility, volume ratios, RSI, MACD, Bollinger position, moving
average cross, ATR, OBV slope, S&P 500/SPY/QQQ/VIX/sector-relative features,
feed sentiment/counts, SEC/Form 4 flags, and cross-sectional ranks.

## Resources

`resources` controls memory, CPU worker threads, and SQLite behavior so the application can run on small machines even when `db/mlai_trade.db` is many GB. Defaults are automatic and should not require user tuning:

- `memory_budget_percent`: percent of detected usable RAM used to derive auto caps. Default `80`, valid range `10`-`95`.
- `cpu_budget_percent`: percent of all logical CPU capacity used for mlai-trade worker pools. Default `80`, valid range `10`-`100`. On a 16 logical CPU host, total top-style CPU capacity is `1600%`, so the default target budget is `1280%`. Tokio async workers, the global Rayon worker pool, and CPU-bound ML engines use this cap as an integer worker-thread limit; GPU/NPU backends (`mlx`, `tch`, XGBoost CUDA) are intentionally uncapped.
- `sqlite_cache_mb`: `auto` or a per-connection SQLite page cache in MB. Auto derives a bounded value from the memory budget.
- `sqlite_temp_store`: `auto`, `file`, or `memory`. Auto uses `file` so large sorts/temp tables do not consume RAM.
- `sqlite_mmap_mb`: `auto` or a SQLite mmap limit in MB. Auto enables mmap only when enough RAM is detected.
- `ml_symbol_batch_size`: `auto` or feature/label symbol batch size.
- `lstm_max_sequences`: `auto` or maximum materialized LSTM training windows sampled across all eligible symbols/dates.
- `lstm_batch_size`: `auto` or LSTM training mini-batch size.
- `lightgbm_max_train_rows`: `auto`, `0`/`unlimited`, or maximum native LightGBM train rows.
- `lightgbm_max_valid_rows`: `auto`, `0`/`unlimited`, or maximum native LightGBM validation rows.

Memory detection uses macOS `sysctl hw.memsize`, Linux cgroup limits when smaller than host RAM, Linux `/proc/meminfo`, FreeBSD `sysctl`, and then generic Unix `sysconf` as a fallback. CPU detection uses Rust's platform `available_parallelism`. Runtime process metrics use Linux `/proc`, macOS Mach APIs, and FreeBSD `sysctl`/`kinfo_proc` where available. `data db-stats` prints the detected source and final derived caps.

The full market database is not loaded into RAM. SQLite rows are streamed for features, labels, exports, and LightGBM text generation. The caps above bound the places that must materialize ML training data in process memory or native ML libraries.

`api status --details` and `daemon status --details` print both live RSS and
configured memory budget. They also print MLX/tch accelerator status. If MLX
or tch/CUDA is available for the running binary and platform, that accelerator
path is explicitly marked uncapped; otherwise status explains whether the
backend is incompatible with the OS/hardware.

Config validation runs before commands execute. Unknown keys, wrong types, out-of-range numbers, and unsupported enum values fail with a precise JSON path and expected values. Example: `$.resources.memory_budget_percent` must be `auto` or an integer from `10` to `95`.

Inspect DB size and largest SQLite objects:

```sh
mlai-trade data db-stats
```

Run safe SQLite maintenance:

```sh
mlai-trade data db-optimize
```

`mlai-trade data db-optimize --vacuum` rewrites the SQLite file to reclaim free pages. Use it only when you intentionally want a long-running DB rewrite and have enough free disk space.

## Autocomplete

Autocomplete is optional. The CLI works without it. Install or remove completion scripts with:

```sh
mlai-trade runtime completions install zsh
mlai-trade runtime completions uninstall zsh
mlai-trade runtime completions install bash
mlai-trade runtime completions install fish
```

Use `mlai-trade runtime completions generate zsh` when you only want to print the script to stdout.
