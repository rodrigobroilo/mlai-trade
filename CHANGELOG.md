# Changelog

## 1.1.37 - 2026-05-07

### Changed

- Dashboard overview now uses green/red real P&L charts in the style of trade
  performance/drawdown views instead of a simple market line.
- Overview allocation moved under the performance graph as a two-column
  scrollable list so it fits more positions without hiding small holdings.
- Accounts now show per-account P&L charts plus allocation bars.
- Positions now include compact per-position P&L charts built from live market
  bars, and allocation uses a scrollable bar list so all open positions can be
  represented instead of only the largest donut slices.
- Orders, positions, tax details, and wash-sale tables page by 50 rows with a
  `Show more +50` action.
- Dashboard tax refresh preserves the selected year/account, including explicit
  paper-account selectors.
- Dashboard charts include date labels and share a range control with Today,
  3-day, 7-day, and custom start/end selections. Current-day charts are the
  default.
- Market-bars API responses now return structured JSON under `data.bars`, which
  the dashboard uses for dated position/account charts.

### Fixed

- Dashboard wash-sale rows are separated into paper and real universes.
- Wash-sale rows with the same universe, symbol, sold date, and window end are
  grouped into one row with summed loss and event count.

## 1.1.36 - 2026-05-07

### Changed

- Simplified the React dashboard navigation to Overview, Accounts, Positions,
  Orders, Auto, Data, and Compliance.
- Replaced the overview market-bars chart with a real trading performance chart
  based on auto history closed P&L plus current auto-managed open P&L.
- Allocation now shows a symbol legend with each position's portfolio
  percentage.
- Dashboard page load now populates live read-only data and then refreshes it
  automatically; provider order sync remains an explicit action.

### Fixed

- Added a React error boundary and defensive table rendering so a bad refresh
  payload cannot blank the whole dashboard.
- Open-position MLQ values now fall back to auto-status model metadata when the
  provider positions route does not include MLQ directly.
- Compliance wash-sale rows now read `sell_timestamp_utc` and
  `wash_window_end`, and federal tax can be loaded for a selected
  provider/account selector, including paper accounts.

## 1.1.35 - 2026-05-07

### Changed

- Rebuilt the React dashboard as a live route-backed webapp covering accounts,
  positions, orders, auto trading, market data, ML, data pipeline, compliance,
  feeds, and system/API routes. The dashboard no longer generates synthetic
  portfolio chart data.
- Dashboard refreshes no longer force provider order/position sync. A
  `Sync before read` toggle and explicit `Sync orders` action are available
  when a manual sync is wanted.

### Fixed

- API SSL TCP/HTTPS request logs now use the accepted socket's local address for
  `dest_ip`, so localhost requests log `127.0.0.1` or `::1` instead of the bind
  wildcard address.
- API SSL H3/QUIC request logs now use Quinn's per-connection local IP for
  `dest_ip` and remain IPv4/IPv6 compatible. IPv6 bind hosts such as `::` are
  parsed correctly.
- API SSL now has explicit `api.ssl.ipv4_enabled` and
  `api.ssl.ipv6_enabled` controls. Both stacks are enabled by default and the
  service binds each available stack separately, continuing with one stack if
  the other is unavailable.

## 1.1.34 - 2026-05-06

### Changed

- Remote API SSL now exposes a normal TLS 1.3 TCP HTTPS listener and the H3/QUIC
  listener on the same configured port, defaulting to `443`. TCP HTTPS serves
  the React dashboard and JSON API directly, while still advertising `Alt-Svc`
  for browsers that can upgrade to HTTP/3.
- `api ssl cert info` now defaults to `--target h3`; ACME challenge
  certificates remain available only when explicitly requested with
  `--target acme`.
- ACME TLS-ALPN-01 remains disabled by default. When enabled in the future, the
  TCP ACME responder is still challenge-only and exposes no API routes.

### Security

- TCP and UDP remote API listeners now have process-local active-connection
  caps. Rejection logs include source/destination IP and port, but are rate
  limited to one JSON event per protocol per 60 seconds with a suppressed-count
  field to prevent log flooding during overload.
- TCP HTTPS logs now include the RFC 9110 `User-Agent` field when an HTTP
  request is available. Values are sanitized, capped at 4 KiB, and reported as
  `not available` for TLS failures or missing headers.
- TCP HTTPS responses now include the same no-index and browser security
  headers used by H3 responses.

## 1.1.33 - 2026-05-06

### Changed

- Changed the default remote API SSL key exchange policy to
  `mlkem_secure_fallback`: H3/QUIC and the TCP bootstrap prefer hybrid ML-KEM
  groups, then allow only strong TLS 1.3 classical groups (`X25519`, `P-256`,
  `P-384`) for current browser compatibility.
- `mlkem_required` remains available for controlled clients that support the
  required ML-KEM/hybrid groups.
- API SSL status and JSON logs now report `network_protocol` (`tcp` or `udp`),
  key exchange policy, offered groups, and whether classical fallback is
  enabled.
- Added `api.ssl.ech` planning/status fields for Encrypted ClientHello. If ECH
  is enabled before server-side RFC 9849 support exists in the TLS/QUIC stack,
  `api ssl start` fails closed and logs `api_ssl_ech_unsupported`.
- Documented OpenSSL as the preferred ECH implementation path, with BoringSSL
  treated as browser-proven but not ideal for a stable cross-platform CLI
  dependency.

### Fixed

- Browser TCP bootstrap handshakes no longer fail solely because Chrome/Safari
  do not offer an ML-KEM-compatible group yet.
- TLS handshake errors are logged from the client perspective, for example
  `client is incompatible: NoKxGroupsInCommon`.
- `api ssl cert info` now prints a renew note explaining why ACME
  TLS-ALPN-01 challenge certificates are not auto-renewed.

## 1.1.32 - 2026-05-06

### Changed

- Added an optional TCP/5443 browser bootstrap listener for `api ssl`. It serves
  no API routes and only returns a small TLS 1.3 response with `Alt-Svc:
  h3=":5443"` so Chrome/Safari can discover and retry the dashboard over
  HTTP/3/QUIC when DNS HTTPS/SVCB records are not available yet.
- `mlai-trade status` and `mlai-trade api ssl status` now show the TCP
  bootstrap bind/port separately from the UDP/H3 data plane and ACME challenge
  listener.
- Split SSL certificate writes into explicit `api ssl cert generate|renew
  --target h3|acme`, added read-only `api ssl cert info --target all|h3|acme`,
  and made `--acme-key-authorization` valid only for `--target acme`.
- API SSL startup now checks certificate expiry and auto-renews only
  mlai-trade-generated self-signed certificates; provided/public CA
  certificates are reported but never overwritten.

## 1.1.31 - 2026-05-06

### Added

- Implemented the remote `api ssl` HTTP/3 data plane as a QUIC/UDP listener
  with TLS 1.3, ALPN `h3`, and an ML-KEM-only Rustls provider.
- Added `mlai-trade api ssl start|stop|restart|reload|status|enable|disable`
  lifecycle commands.
- Added `mlai-trade api ssl cert generate|renew` to create local H3
  certificates and RFC 8737-shaped TLS-ALPN-01 challenge certificates under
  `config/cert/` with private file permissions.
- Added `api.ssl.auth` config. Non-localhost remote clients must use HTTP Basic
  authentication; localhost source traffic is allowed without auth for the
  built-in local dashboard.
- Added a responsive React dashboard under `api/html/`, served by the remote
  H3 listener. It shows portfolio, positions, recent orders, auto-trade status,
  ML status, market quote/bars, and system health.
- Added `robots.txt` that disallows all crawlers, including common AI crawler
  user agents. This is advisory only; remote authentication is still the actual
  protection.

### Security

- Remote SSL/H3 startup refuses non-loopback binds when auth is disabled,
  missing, or still set to the example password.
- Public-domain startup can enforce HTTPS/SVCB `alpn=h3` DNS checks before
  opening the remote listener.
- Static webapp serving is limited to built assets and `robots.txt`; path
  traversal attempts are rejected.
- Remote logs include transport, method, path, status, duration, source
  IP/port, and destination IP/port.

## 1.1.30 - 2026-05-06

### Fixed

- Successful manual full daily prep now satisfies the daemon's daily refresh
  stamp for the current market date after market close. This prevents the daemon
  from rerunning the same `ml refresh` later after a user already completed
  `mlai-trade data daily`, `mlai-trade ml refresh`, or
  `mlai-trade ml full-refresh`.
- Daemon-owned child commands are tagged internally so the daemon itself remains
  responsible for writing the stamp only after all daemon maintenance steps
  succeed, including post-training feed sync and tax refresh.

## 1.1.29 - 2026-05-06

### Fixed

- XGBoost no longer feeds LightGBM/libsvm text files into XGBoost's deprecated
  text-file parser. The Rust pipeline now parses the existing training dataset
  and creates in-memory XGBoost DMatrix objects for training and prediction.
- XGBoost prediction now uses `XGBoosterPredictFromDMatrix` instead of the
  deprecated C API prediction entry point.

### Changed

- Daemon daily maintenance now defaults to 6 hours after regular market close
  (`daily_refresh_after_close_minutes=360`) instead of 1 hour, reducing
  post-close provider churn and avoiding repeated immediate refresh attempts
  after manual daemon restarts.
- The example config clarifies that `daily_refresh_time` is ignored unless
  `daily_refresh_trigger=time`.
- Rotated gzip logs now live under `logs/archived/` while current JSONL logs
  stay directly under `logs/`.

## 1.1.28 - 2026-05-06

### Changed

- Changed the planned remote API defaults to port `5443` for both QUIC/UDP and
  the optional TCP ACME challenge listener.
- DNS HTTPS/SVCB validation now checks the configured QUIC port instead of
  assuming `443`.

## 1.1.27 - 2026-05-06

### Added

- Added explicit API transport command groups:
  `mlai-trade api unix ...` for the local Unix-socket API and
  `mlai-trade api ssl ...` for remote HTTP/3 planning/status checks.
- Added `api.unix` and `api.ssl` config sections while preserving the legacy
  top-level Unix API keys for backward compatibility.
- Added `mlai-trade api ssl status` and `mlai-trade api ssl dns-check DOMAIN`
  so remote deployments can validate the H3-only DNS HTTPS/SVCB discovery
  record before the future remote server is started.

### Security

- Documented the remote API policy as HTTP/3 over QUIC on UDP/5443 only, TLS
  1.3 only, ALPN `h3` only, and `mlkem_required` key exchange with no
  classical fallback.
- Documented TCP/5443 as Let's Encrypt TLS-ALPN-01 challenge-only when ACME is
  enabled; it exposes no normal HTTPS API routes.

## 1.1.26 - 2026-05-06

### Changed

- Added backend-aware LSTM stabilization knobs to `mlai-trade-ml-tuning.json`:
  `loss_function`, `huber_delta`, `dropout_rate`, and `weight_decay`.
- LSTM regression training now z-scales forward-return targets during training
  and decodes predictions back into return space for validation, prediction,
  and ensemble use.
- The built-in accelerator default now uses the best balanced paused 365-day
  real-data sweep result from 442 completed variants:
  `hidden_dim=128`, `learning_rate=0.0001`, `loss_function=mse`,
  `dropout_rate=0.1`, and `weight_decay=0.01`.
- The default ensemble fallback is now `LightGBM=40%` and `LSTM=60%`, matching
  the same balanced sweep result. A saved
  `data/ml_default_ensemble_config.json` still takes precedence.

### Notes

- The sweep was paused at 442/649 variants and saved outside the repository at
  `/tmp/mlai-full-ml-real-365-20260504T170654Z/sweep/RESUME.md`; the helper
  script was intentionally not committed.

## 1.1.25 - 2026-05-06

### Fixed

- `trade positions` now reports sync behavior accurately. The command already
  refreshes `provider_position_snapshots` from the live provider response every
  time it lists positions; the output now says `Position snapshot sync:
  completed` and reserves `Order/fill sync` for the optional `--sync` history
  refresh.
- JSON output for `trade positions` now exposes
  `position_snapshot_synced_before_listing=true` and
  `order_fill_sync_before_listing` separately, while keeping the older
  `local_db_sync_before_listing=true` compatibility field.
- `ml explain` now uses signed permutation SHAP-style contributions whose sum
  reconciles with `predicted - base_value`, so negative anchors are visible.
  Human output shows top positive contributors and top negative anchors
  separately; JSON includes `shap_sum`, `prediction_minus_base`, and
  `additivity_error`.
- SHAP background sampling is now deterministic instead of `ORDER BY RANDOM()`,
  so repeated explanations for the same symbol/date are stable.

## 1.1.24 - 2026-05-06

### Added

- Added `mlai-trade auto track SYMBOL --account ACCOUNT` to adopt one
  provider/CLI-held position into auto-trade management without submitting a
  buy order or rewriting the original provider fill history.
- Added `mlai-trade auto untrack SYMBOL --account ACCOUNT` to release one
  auto-managed position back to manual `mlai-cli` management without selling.
- The Unix-socket API exposes the same guarded actions through `/auto/track`
  and `/auto/untrack`. Both CLI and API require one explicit symbol and one or
  more full `provider:account-ref` selectors; `ALL`, bare refs, and broad
  selectors are rejected for these ownership changes.

### Changed

- `auto status` JSON now separates historical `execution_origin` from current
  `management_origin`, so a provider-origin position can be auto-managed from
  now on without losing the audit trail of how it was originally opened.
- Startup config validation now rejects duplicate account names within the same
  provider namespace. The same account name remains valid across different
  future providers because the selector includes the provider prefix.

## 1.1.23 - 2026-05-06

### Changed

- The Unix-socket API now accepts `sync=true` on `/trade/positions`, matching
  `mlai-trade trade positions --sync`, so provider live-position snapshots can
  be refreshed and queried through the API.
- API docs now call out the `auto status` `execution_origin_label` field, which
  renders direct provider-origin holdings with the concrete provider name such
  as `alpaca`.

## 1.1.22 - 2026-05-06

### Changed

- `auto status` now uses the same position columns for auto-managed and
  not-tracked provider positions: symbol, origin, quantity, average cost,
  current price, market value, unrealized P&L, unrealized P&L percent, and ML
  quintile.
- External provider-origin positions now render the concrete provider name such
  as `alpaca` instead of the generic `provider` label.

## 1.1.21 - 2026-05-06

### Changed

- `auto status` now refreshes the provider live-position snapshot and separates
  positions that auto-trade is actively managing from provider-held positions
  that are not tracked by auto rules. JSON output includes
  `auto_managed_positions`, `provider_positions`, `unmanaged_positions`, and
  matching counts so CLI/API consumers can distinguish auto, CLI, and direct
  provider activity.

## 1.1.20 - 2026-05-06

### Added

- Provider live positions are now stored in `provider_position_snapshots` for
  each provider/account. This table tracks the current holdings that Alpaca
  reports, including non-auto positions and positions changed outside
  mlai-trade.

### Changed

- `trade positions` stores the live provider position snapshot every time it
  lists positions, and `trade positions --sync`, `auto sync-orders`, and daemon
  provider sync refresh the same table.
- `status` now reports current provider-position rows per account so the local
  DB can be checked against Alpaca's live 14/13 position counts.

## 1.1.19 - 2026-05-06

### Fixed

- Provider-position reconciliation logs now include entry, exit, and combined
  execution origin for partial provider-side sells, matching the closed partial
  lots stored in `auto_positions` and shown by tax reports.
- Runtime auto logs were backfilled so historical buy/sell cycle entries and
  provider reconciliation events expose the same origin fields as the DB.

## 1.1.18 - 2026-05-06

### Fixed

- Closed auto positions now store separate entry and exit execution origins.
  If a position was opened by auto-trade but sold directly at the provider or
  by CLI, the realized lot is reported as `mixed` instead of incorrectly
  showing the whole P&L as `mlai_auto`.
- Provider reconciliation now records partial provider-side sells as closed
  partial lots before reducing the remaining open auto position. This preserves
  realized P&L for partial external exits such as manual provider sells.
- Existing closed auto-position rows are backfilled from provider order
  snapshots and auto-trade rows, including missing `exit_order_id` values for
  prior auto exits.

## 1.1.17 - 2026-05-06

### Fixed

- New `auto_trade_cycle` logs and JSON payloads now include
  `execution_origin=mlai_auto`, matching the historical log backfill and the
  order/fill origin-reporting model added in 1.1.16.

## 1.1.16 - 2026-05-06

### Added

- Provider orders and fills now carry `execution_origin`: `mlai_auto`,
  `mlai_cli`, `provider_external`, `mixed`, or `unknown`.
- `trade orders`, `auto status`, `auto history`, `auto sync-orders`, `status`,
  and tax detail output now expose origin so provider-web activity, CLI
  activity, and daemon auto-trading can be separated.
- Tax estimates now include realized P&L grouped by execution origin, and
  detailed tax rows include entry, exit, and overall origin.

### Changed

- New CLI orders use the `mlai-cli-*` client order id prefix. Older `plm-*`
  client order ids are still classified as CLI-originated.
- Provider sync backfills existing DB rows from client order ids and
  `auto_trades`, and logs only truly external provider activity as external.

## 1.1.15 - 2026-05-06

### Fixed

- Auto-trade now reconciles local open `auto_positions` against the provider's
  live position snapshot before evaluating stop-loss, take-profit, time-stop,
  or ML-degraded exits. If Alpaca no longer reports a long position, the local
  auto position is closed from provider-confirmed sell history instead of
  submitting a sell order that would become an invalid short sale.
- If the provider reports fewer shares than the local auto position, the local
  share count and cost basis are adjusted down before exit sizing.
- Reconciliation emits `auto_position_reconciled_from_provider` JSON log events
  and appears in the account cycle payload under
  `provider_position_reconciliation`.
- Provider history sync now audits provider-side activity that did not originate
  from mlai-trade. New external orders and fills log
  `provider_external_order_observed` and `provider_external_fill_observed`, and
  provider cash changes log `provider_account_snapshot_changed`.

## 1.1.14 - 2026-05-06

### Added

- Auto-trade exits now support configurable confirmation windows for normal
  stop-loss and take-profit triggers. Defaults wait for three consecutive
  daemon cycles before selling, while an emergency stop-loss at a deeper loss
  sells immediately.
- Take-profit exits now support a minimum hold window plus trailing giveback
  logic after the profit threshold is first crossed.
- `auto_positions` now stores entry timestamps, exit order IDs, confirmation
  counters, first-breach timestamps, and take-profit peak state for audit and
  restart-safe exit confirmation.
- Auto-trade JSON logs now emit `auto_exit_confirmation_wait`,
  `auto_exit_rule_triggered`, `auto_exit_order_submitted`, and
  `auto_exit_order_failed` events so waiting cycles, rule triggers, and sell
  submissions can be audited by account/symbol/rule.

## 1.1.13 - 2026-05-06

### Fixed

- Strengthened auto-trade cash-only enforcement across daemon cycles. Buy
  capacity now uses the lower of provider-reported cash and a conservative
  exposure calculation: account equity minus the greater of provider long
  exposure or local auto-position exposure, minus pending buy reservations.
  This prevents stale Alpaca paper `/account.cash` values from allowing a later
  daemon cycle to use margin buying power after recent fills.
- Auto-trade JSON now includes `cash_only_guard` audit fields for each account
  cycle so cash decisions can be inspected from logs and API output.

## 1.1.12 - 2026-05-05

### Changed

- Cargo now compiles the mandatory backend set from the target platform instead
  of requiring manual feature flags: macOS/Linux get XGBoost, Apple Silicon gets
  MLX, Linux gets `tch`/libtorch, and FreeBSD keeps the portable CPU baseline.
- Linux and FreeBSD validation scripts now use the normal Cargo commands so the
  platform policy is what gets tested.
- Legacy feature names remain as no-op compatibility aliases for older command
  lines.

## 1.1.11 - 2026-05-05

### Fixed

- Fixed the custom lowercase version shortcut so normal commands such as
  `mlai-trade auto status` no longer fail with a missing `version` argument.
- Full macOS feature builds now include XGBoost plus MLX while keeping the
  Linux `tch`/libtorch dependency target-gated, so Apple Silicon builds are not
  blocked by an unimplemented libtorch/MPS path.
- Daemon/API captured command output is sanitized before JSON logging, removing
  terminal control sequences from `stdout_tail`, `stderr_tail`, and API text
  fields while still redacting configured secrets.

## 1.1.10 - 2026-05-05

### Changed

- The global version shortcut is now `-v` instead of clap's default `-V`.
  `--version` remains supported.
- Documentation examples now consistently use provider account selectors such
  as `alpaca:paper-main` when a paper account is selected explicitly.
- Configuration and usage docs clarify that the config account `name` is the
  mutable local selector while Alpaca's broker account ID is the stable account
  identity used to reconcile local rows after renames.

## 1.1.9 - 2026-05-05

### Changed

- `auto sync-orders` human output now prints account order/fill sync details
  separately from shared compliance-universe wash-sale checks.
- Sync, account, order, position, auto status, auto cycle, and daemon summary
  JSON now include the stable broker account ID alongside the mutable local
  account ref when the provider exposes it.

## 1.1.8 - 2026-05-05

### Added

- Alpaca accounts now support `auto_trade_enabled`. This disables autonomous
  buy/sell decisions per account while leaving provider sync, account status,
  orders, positions, tax simulation, and compliance reconciliation enabled.
- `trade positions --sync` can refresh provider orders/fills before showing
  live provider positions, matching `trade orders --sync`.

### Changed

- Provider order/fill sync now canonicalizes renamed local account refs using
  Alpaca's broker account ID. Renaming a config account such as `paper-main` to
  `paper-original` moves old local rows to the current name instead of leaving
  duplicated order/fill snapshots, wash-sale rows, day trades, or auto-trade
  rows.
- `trade orders` and `trade positions` now describe their sync behavior
  explicitly as a live provider query plus an optional local DB sync before
  listing, instead of showing the confusing `Synced: false` label.
- Account and auto status output now shows whether autonomous trading is
  enabled for each account.

## 1.1.7 - 2026-05-05

### Added

- Feed refresh now computes bounded price correlations for managed feed
  subscriptions before ML training when
  `feeds.compute_correlations_before_training=true`.
- ML feature rows now include point-in-time managed-feed-universe return and
  rolling 30/90 day correlation features, derived only from bars available on
  the feature date.
- Provider order/fill sync now reconciles missed wash-sale monitor rows from
  provider-confirmed fills. Paper accounts are reconciled in a separate paper
  universe, while all real-money accounts share one IRS-relevant universe by
  symbol.
- `compliance wash` now shows the UTC sell time in human output and includes
  `sell_time_utc`/`sell_timestamp_utc` in JSON.

### Changed

- `mlai-trade.example.json` documents all feed correlation controls:
  `correlation_days`, `correlation_min_overlap_days`,
  `correlation_strong_threshold`, and `correlation_max_symbols`.
- `auto sync-orders` output includes the wash-sale reconciliation summary.

## 1.1.6 - 2026-05-05

### Fixed

- `backend.xgboost=auto` now degrades cleanly when the binary is not compiled
  with `xgboost-baseline`, even if old XGBoost model artifacts exist in
  `data/`. Forced XGBoost modes still fail clearly.
- Daemon pre-open handling now pauses provider auto-trade checks until near the
  next regular market open instead of syncing provider orders/fills every
  interval for hours while the configured local market is closed.
- Daemon auto-trade summaries no longer fill portfolio fields with
  `"not available"` when the market gate closes before portfolio evaluation.
  Closed-market summaries now set `portfolio_evaluated=false` and omit
  unavailable position counters.

### Validation

- `RUSTFLAGS=-Dwarnings cargo check --locked --features mlx-lstm`
- `RUSTFLAGS=-Dwarnings cargo build --release --locked --features mlx-lstm`
- Installed locally and restarted API/daemon; verified pre-open status now logs
  `auto_market_preopen_pause_started` and holds `waiting_for_market_open`
  without repeating provider auto-trade cycles every daemon interval.

## 1.1.5 - 2026-05-04

### Fixed

- Auto-trade now fetches current provider positions before every decision cycle
  and counts them together with local `auto_positions` for max-position slots.
  Symbols already held at the provider are skipped as buy candidates, preventing
  duplicate buys when positions were opened outside `mlai-trade`.
- Fixed daemon auto-trade pre-market handling. A `market_closed` cycle before
  regular market open now stays on the normal daemon retry interval instead of
  backing off for the full market date.
- Fixed cash-only enforcement inside one auto-trade cycle. Accepted buy orders
  now decrement the cycle's remaining cash budget, preventing multiple buys
  from each independently using the same starting cash balance.
- Improved auto-trade cash logging. Accounts with zero or negative cash now
  emit one account-level skip message with the provider-reported cash balance
  instead of repeated per-symbol margin rejection messages.
- Daemon logs now record a compact `auto_trade_cycle_summary` and point to
  `mlai-trade-auto.log` for full auto-trade details, avoiding duplicated full
  auto-cycle payloads in both logs.
- CLI lifecycle logs now include `status`, `pid`, and `finished_at_utc` fields,
  and panics are recorded as JSON `command_panicked` events before the process
  exits with the normal error/help message.

### Validation

- `RUSTFLAGS=-Dwarnings cargo check --locked --features mlx-lstm`
- `RUSTFLAGS=-Dwarnings cargo build --release --locked --features mlx-lstm`
- Installed and restarted the local API/daemon; verified daemon auto cycle ran
  during the open provider session and skipped buys with a single cash-only log.
- Installed and restarted the local API/daemon again; verified new daemon cycles
  log `auto_trade_cycle_summary` while full account details remain in
  `mlai-trade-auto.log`.

## 1.1.4 - 2026-05-04

### Added

- Added `config/mlai-trade-ml-tuning.example.json` and private runtime
  `mlai-trade-ml-tuning.json` support for ML hyperparameter tuning outside the
  provider/runtime config.
- Added backend-aware LSTM tuning profiles for CPU, MLX, and tch/CUDA targets.
  `backend.lstm=auto` now resolves the runtime backend first, then applies the
  matching tuning profile; accelerator fallback uses the CPU profile.
- Added LSTM training knobs for target mode, forward-return direction
  threshold, hidden width, epochs, learning rate, early stopping, and early
  stopping validation sample size.
- Added one-off CLI overrides for `ml lstm-train`: `--target-mode`,
  `--hidden-dim`, `--epochs`, and `--learning-rate`.
- Added LSTM training/evaluation report fields for target mode, direction
  metrics, best epoch, validation losses, early stopping, selected profile,
  and backend.

### Changed

- Built-in LSTM defaults are now backend-specific: CPU uses a conservative
  64-hidden-unit/10-epoch profile, while MLX and tch target profiles use a
  wider 128-hidden-unit/20-epoch profile.
- LSTM defaults continue to train on forward returns rather than prices.
  Direction/classification mode is available for experiments, but return
  regression remains the default because ensemble ranking needs comparable
  return scores.
- Documentation now covers the separate ML tuning file, backend-specific LSTM
  profiles, built-in defaults, sequence scaling, technical indicators, and
  command-line LSTM tuning overrides.

### Fixed

- Replaced the MLX LSTM trainer's built-in MLX LSTM conversion path with a
  custom MLX cell that matches the portable Rust inference layout. Saved MLX
  models now validate consistently through the same CPU-portable model used by
  `ml lstm-predict` and `ml lstm-evaluate`.

### Validation

- `cargo check --locked`
- `cargo check --locked --features mlx-lstm`
- `cargo test --locked`
- `cargo build --release --locked --features mlx-lstm`
- `scripts/e2e-synthetic-test.sh run target/release/mlai-trade`
- `scripts/provider-fake-alpaca-test.sh run target/release/mlai-trade`
- `arc lint --never-apply-patches`
- `git diff --check`
- Synthetic LSTM comparison on 60 symbols / 38,580 bars:
  CPU 64 regression, CPU 128 regression, MLX 64 regression, MLX 128
  regression, MLX 256 regression, and MLX 128 direction.

## 1.1.3 - 2026-05-04

### Added

- `feeds sync` now uses per-source parallelism: SEC EDGAR defaults to one
  symbol query at a time, while Alpaca, Yahoo RSS, and Google RSS default to
  two concurrent symbol queries each.
- Added feed source timeout, retry, and auto-tune settings in config:
  `source_timeout_seconds`, `source_retry_count`, `auto_tune_sources`,
  `alpaca_concurrency`, `sec_edgar_concurrency`, `yahoo_rss_concurrency`, and
  `google_rss_concurrency`.
- Feed sync source summaries are logged as JSON with article counts, errors,
  timeouts, attempts, configured concurrency, and final auto-tuned concurrency.

## 1.1.2 - 2026-05-04

### Fixed

- LSTM backend `auto` now treats MLX/tch accelerator panics as recoverable
  backend failures and falls back to the CPU/Rayon trainer instead of crashing
  the full `data daily` or `ml refresh` pipeline.
- MLX training now runs a small Metal-kernel smoke check before loading the
  large LSTM sequence dataset, so missing MLX Metal libraries are detected
  quickly with a clear fallback path.

## 1.1.1 - 2026-05-04

Initial public release.

### Added

- Rust CLI organized by topic: `runtime`, `daemon`, `api`, `trade`, `market`,
  `data`, `compliance`, `feeds`, `ml`, and `auto`.
- Multi-account Alpaca provider support with separate paper and real-money
  compliance universes, per-account selectors, order/fill sync, positions,
  account status, quote/bars/news/calendar/clock commands, and configurable
  SIP/IEX data feed behavior.
- Local JSON configuration under `~/mlai-trade/config/` with explicit examples,
  provider/account configuration, daemon/API settings, tax profile, ML backend
  selection, resource budgets, and runtime paths.
- Private runtime layout under `~/mlai-trade/`: `api/`, `bin/`, `config/`,
  `data/`, `db/`, `docs/`, `logs/`, and `tmp/`.
- Security hardening for local runtime files: private directories, private
  config/data/db/log/API files, secret redaction in command/API logs, and real
  config exclusion from Git.
- Unix-socket JSON API with allowlisted routes, lifecycle commands, API health
  test, request body limit, rate limiting, concurrency caps, long-operation
  caps, timeout handling, and JSON request logging.
- Daemon mode with JSON logs, PID files, status/details output, auto-trade
  cycles, provider order/fill sync, tax refresh, and daily non-trading ML prep.
- Runtime update lock for long `data daily`, `ml refresh`, and
  `ml full-refresh` jobs so manual and daemon-triggered preparation cannot
  overlap. Start, finish, failure, cancellation, duration, and stale-lock
  cleanup events are written as JSON.
- Daily log rotation with compressed historical logs and JSONL log sanitization
  for daemon, API, auto-trade, data, feeds, ML, and training components.
- Full non-trading ML preparation pipeline: universe refresh, FRED benchmark
  sync, Alpaca historical bar sync, missing/latest bar repair, managed feed
  reconciliation, feed sync, feature generation, labels, model training,
  validation, predictions, ensemble output, and SHAP cache.
- ML models and baselines: native Rust LightGBM, Ridge baseline, optional
  XGBoost support, pure Rust CPU LSTM, Apple Silicon MLX LSTM path, Linux
  NVIDIA/tch backend readiness, walk-forward validation, S&P 500 ablation, and
  ensemble search.
- Feed pipeline for subscribed symbols, S&P 500/watch/position/candidate
  symbols, RSS/Alpaca/SEC ingestion, sentiment, relationships, correlations,
  and feed-derived ML features.
- Compliance and tax tools for IRS-oriented wash-sale/PDT tracking, blocked
  symbols from config, tax account selection, quarterly/year-to-date tax
  estimates, tax detail rows, CSV export, and bracket display from JSON tax
  bracket files.
- Resource controls that auto-detect RAM and logical CPU capacity on macOS,
  Linux, FreeBSD, and generic Unix, then size SQLite/ML limits and CPU worker
  caps automatically.
- Cross-platform validation harnesses: macOS host checks, Ubuntu Docker
  validation from non-Linux hosts, FreeBSD Lima validation from non-FreeBSD
  hosts, synthetic market data, and a fake Alpaca provider/API fixture.
- Documentation covering setup, runtime layout, configuration, API, usage,
  debugging, testing, validation results, IRS/trading references, third-party
  licenses, and project license/disclaimer.

### Fixed

- Daemon lifecycle commands remain usable when `mlai-trade.json` is invalid,
  so `daemon stop/status/reload` and API lifecycle inspection/control can still
  recover a bad config state.
- Daemon invalid-config sleep is interruptible by stop/reload signals.
- `daemon restart` now surfaces stop failures instead of ignoring them.
- FreeBSD builds link the C++ runtime required by the native LightGBM
  dependency.

### Validation

- `cargo fmt --check`
- `RUSTFLAGS=-D warnings cargo check`
- `cargo test`
- `cargo build --release --features mlx-lstm`
- Focused invalid-config daemon stop regression
- `scripts/linux-ubuntu-test.sh run`
- `scripts/freebsd-lima-test.sh run`
- `arc lint --rev origin/main --never-apply-patches`
- `git diff --check`
