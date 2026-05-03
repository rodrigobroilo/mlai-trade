# Changelog

All notable user-facing changes are tracked here, starting from `0.1.0`.

## 1.0.3 - 2026-05-03

### Changed

- Simplified Alpaca config shape: `providers.alpaca.enabled` is the provider switch, and each `alpaca.accounts[]` entry owns its own `account_mode`, `data_feed`, `api_key_id`, and `secret_key`.
- Removed legacy top-level Alpaca credential/feed/account defaults from the example config and runtime config.

### Fixed

- Removed dead fallback code and stale help text that referenced top-level `alpaca.data_feed` or top-level Alpaca credentials.

## 1.0.2 - 2026-05-03

### Added

- Added API overload protection for local Unix-socket requests:
  - `api.max_concurrent_requests`
  - `api.max_concurrent_long_requests`
  - `api.rate_limit_per_minute`
  - `api.max_body_bytes`
  - `api.overload_retry_after_seconds`
- Added HTTP `429` JSON backoff responses with `reason` and `retry_after_seconds` when rate or concurrency limits are exceeded.
- Added HTTP `413` rejection for oversized request bodies.

### Changed

- Updated API status output to show active overload-protection limits.
- Updated README runtime layout to include `api/`, `logs/`, and `tmp/`.
- Updated API/config/debugging docs with overload behavior and backoff guidance.

## 1.0.1 - 2026-05-03

### Added

- Added `daemon.daily_refresh_trigger=market_close` and `daemon.daily_refresh_after_close_minutes=60` so the daemon can run the daily non-trading prep job once per open market date after the configured market close.
- Added exhaustive config-template coverage in tests: every supported config key must exist in `config/mlai-trade.example.json`.
- Added clearer documentation for daemon daily prep timing, feed reconciliation, full incremental ML refresh, API behavior, JSON logs, and runtime files.
- Added `docs/README.md` as the documentation index.

### Changed

- `mlai-trade data daily` now uses the same shared full incremental non-trading pipeline as `mlai-trade ml refresh` by default.
- `mlai-trade data daily --skip-train` remains the explicit data-only variant.
- Daemon daily prep now documents and uses the full `ml refresh` path: provider order sync, FRED/Alpaca gap fill, feed reconciliation/sync, features, labels, model training/evaluation, predictions, ensemble, SHAP cache, and tax refresh.
- Feed reconciliation explicitly adds/updates managed symbols from current S&P 500, open positions, recent buys, latest Q1 candidates, and config extras, while removing stale managed symbols and preserving manual subscriptions.
- API wrapper now treats command JSON with `ok:false` as an API failure and returns a non-2xx status instead of reporting silent success.
- Removed the hidden legacy top-level `xgboost.backend` config path; use `backend.xgboost`.

### Fixed

- Ensured `daemon.daily_refresh_sync_orders=true`, `daemon.daily_refresh_feeds_sync=true`, and `feeds.sync_before_training=true` are explicit defaults in both example and runtime config.
- Clarified that `daemon.daily_refresh_enabled=true` never places trades; it only schedules non-trading prep.
- Made daily-maintenance JSON log entries include trigger and after-close timing.

## 1.0.0 - 2026-05-02

### Added

- Initial `mlai-trade` Rust CLI release with topic-oriented command groups: `runtime`, `daemon`, `api`, `trade`, `market`, `data`, `compliance`, `feeds`, `ml`, and `auto`.
- Added configurable runtime home layout under `~/mlai-trade` with `bin/`, `config/`, `data/`, `db/`, `docs/`, `logs/`, `api/`, and `tmp/`.
- Added private runtime config support through `config/mlai-trade.example.json` and ignored local `mlai-trade.json` files for credentials.
- Added Alpaca provider support with multiple account configs, stable account selectors, paper/individual account modes, and per-account SIP/IEX/auto data-feed selection.
- Added Alpaca market calendar and clock integration, local exchange-time guardrails, UTC event timestamps, and stored market timezone/session context for trade records.
- Added read-only provider order/fill sync so local account, order, fill, tax, and auto-trade state can be reconciled from broker source-of-truth data.
- Added SIP/IEX feed explanation and history-start probing for Alpaca daily stock bars.
- Added FRED-backed S&P 500, VIX, and macro series ingestion for ML features.
- Added gap-aware Alpaca daily-bar scanner that discovers first available history, overwrites the latest local day, and fills missing ranges incrementally.
- Added ML pipeline commands for features, labels, export, LightGBM training, Ridge/XGBoost baselines, LSTM training/prediction/evaluation, walk-forward validation, S&P 500 ablation, prediction refresh, ensemble search/defaults, SHAP explanation, and ML status.
- Added optional ML acceleration config for LSTM backends (`auto`, `cpu`, `mlx`, `tch`) and XGBoost backends (`auto`, `cpu`, `cuda` when compiled).
- Added feed ingestion for Alpaca news, SEC EDGAR filings, Yahoo RSS, Google RSS, sentiment, graph/correlation helpers, and feed-derived ML features.
- Added federal tax estimator with configurable IRS bracket JSON, filing status, estimated annual income, quarter selection, CSV export, account selection, and per-operation details.
- Added compliance guardrails for IRS wash-sale simulation, PDT tracking, account-specific gains/losses, paper-vs-real compliance universes, blocked-symbol config, and hard-disabled options trading.
- Added auto-trade engine with per-account execution state, provider sync before decisions, account-scoped orders/positions, NBBO/spread guardrails, bar fallback controls, stop-loss/take-profit/max-hold controls, and ML quintile thresholds.
- Added Unix-socket JSON API with lifecycle commands, route allowlist by section, health test, JSON response wrapper, and separate API PID/socket/log files.
- Added daemon lifecycle commands, PID files, auto-trade loop scheduling, reload/restart/stop/status, daemon-driven tax refresh, and daemon/API runtime status views.
- Added JSON-line component logs for daemon, API, auto-trade, data, ML, training, and feeds.
- Added daily log rotation with gzip archives named `YYYYMMDD-<log-file>.gz`.
- Added shell completion generation/install/uninstall commands.
- Added project documentation for usage, configuration, API, debugging, IRS tax rules, trading knowledge, licenses, and third-party crate inventory.
- Added MIT project license and disclaimer that the project is not affiliated with Alpaca Markets and is used at the user's own risk.

### Changed

- Renamed the standalone Alpaca tool into the broader `mlai-trade` CLI and kept Alpaca as a provider module so future providers can share ML/compliance/runtime infrastructure.
- Moved generated data, databases, logs, configs, API sockets, and PID files into the configurable runtime home instead of hardcoded user/container paths.
- Reorganized top-level commands into categories and kept compatibility aliases hidden from primary help/autocomplete.
- Reworked JSON output support across many commands so CLI output can be consumed by the Unix-socket API and external tools.
- Converted Python-side ML training responsibilities into Rust-managed commands and artifacts.
- Moved IRS tax brackets and rates into data files so future years can be updated by JSON diff rather than code edits.

### Fixed

- Ensured daemon/API/auto logs are JSON lines rather than mixed text and JSON.
- Added closed-market daemon backoff so the auto-trade loop does not repeatedly retry after the market is known closed for the current market date.
- Ensured API and daemon runtime files use dedicated runtime folders instead of global temporary paths.
- Ensured private config, DBs, datasets, logs, generated models, and credentials are ignored by Git.

## 0.1.0 - 2026-05-02

### Added

- Created the private `mlai-trade` repository.
- Added the initial README and project naming baseline.
- Established the repository history point that later releases build from.
