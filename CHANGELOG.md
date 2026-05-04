# Changelog

All notable user-facing changes are tracked here, starting from `0.1.0`.

## 1.1.0 - 2026-05-03

### Added

- Added `scripts/cli-smoke-test.sh` to validate cross-platform status, JSON,
  API lifecycle, and daemon lifecycle behavior in a disposable runtime home.
- Added `scripts/freebsd-lima-test.sh` for automated FreeBSD validation. It
  runs natively on FreeBSD and uses a cached Lima FreeBSD 16 VM on non-FreeBSD
  hosts.
- Added `scripts/seed-synthetic-market.sh` and `scripts/e2e-synthetic-test.sh`
  to validate data, feeds, ML features/labels/training/prediction/ensemble,
  API, and daemon behavior with fake stock/ETF data and no live trades.
- Added a hidden local fake Alpaca fixture server and
  `scripts/provider-fake-alpaca-test.sh`. The test uses one month of
  deterministic stock/ETF bars and fake paper account/order/position endpoints
  to validate provider market data, order sync, paper buy/sell, tax account
  selection, and Unix-socket API routes without live credentials.
- Added per-account Alpaca endpoint overrides
  (`alpaca.accounts[].trading_base_url` and
  `alpaca.accounts[].data_base_url`) for provider integration tests.
- Added `docs/TESTING.md` with the host/Linux/FreeBSD matrix, smoke-test
  coverage, live-provider boundaries, and full pre-push test order.
- Added `--help` and explicit `run` commands to all local validation scripts.
  Scripts now print usage instead of running when no command is passed.
- Added validation cleanup commands: Linux `run` removes stale kept containers,
  Linux `delete` removes cached image/volumes, and FreeBSD `run`/`clean`
  remove stale guest test work directories while preserving the cached VM.

### Fixed

- Linux daemon/API PID detection now treats zombie processes as stopped, which
  prevents stale PID files from making `status` or `stop` report a terminated
  process as still running in containerized test environments.
- FreeBSD builds now link the C++ runtime needed by the bundled LightGBM native
  dependency.
- Linux validation now runs the shared CLI smoke test after the release build.
- Linux and FreeBSD validation scripts now run the synthetic non-trading e2e
  test after the shared smoke test.
- Linux and FreeBSD validation scripts now run the fake Alpaca provider e2e
  test after the synthetic ML e2e test.
- Fresh DB initialization now adds account-scoping columns to the local
  wash-sale and PDT tables before manual buy/sell commands use them.

### Documentation

- Expanded Linux validation docs with cached-image behavior, validation modes,
  container inspection commands, forced image refresh, and cleanup commands.
- Documented FreeBSD validation through the cached Lima FreeBSD 16 VM and the
  synthetic stock/ETF end-to-end test path.
- Documented where Linux Docker images/volumes and the FreeBSD Lima VM cache
  are stored.

## 1.0.10 - 2026-05-03

### Changed

- Switched the Ubuntu Linux validation harness to Docker CLI only.
- On macOS, `scripts/linux-ubuntu-test.sh` now automatically installs Docker
  CLI + Colima with Homebrew when Docker is missing, then starts Colima in the
  background. Linux hosts are expected to use their native Docker engine and
  are not auto-provisioned.
- `scripts/linux-ubuntu-test.sh` now runs validation natively on Linux hosts
  and uses the Ubuntu Docker container only from non-Linux hosts.
- The Ubuntu test image is now cached locally as `mlai-trade:ubuntu-test` and
  reused offline when the Dockerfile fingerprint matches. Use
  `scripts/linux-ubuntu-test.sh update` to pull/rebuild intentionally.
- Added `container`/`shell` modes for opening the cached Ubuntu test image for
  manual inspection with `docker ps` and `docker exec`.
- Linux/container validation now runs with `RUSTFLAGS=-D warnings`, so warnings
  in mlai-trade fail the run.

## 1.0.9 - 2026-05-03

### Added

- Added an Ubuntu 24.04 container test harness at
  `scripts/linux-ubuntu-test.sh`.
- Added `.dockerignore` protections so local runtime data, databases, logs,
  sockets, and real config files are not sent to container build contexts.
- Added accelerator capability reporting to `api status --details` and
  `daemon status --details` for MLX and tch/CUDA.

### Changed

- Detailed API/daemon status now shows memory budget alongside live RSS usage.
- CPU status now explicitly states when GPU/NPU accelerator paths are
  unavailable or, when available, uncapped.

## 1.0.8 - 2026-05-03

### Fixed

- Corrected CPU reporting to use top-style process CPU percentages across all logical CPUs. For example, 16 logical CPUs now reports total capacity as `1600%`; an 80% budget is shown as `1280%`.
- Made the CPU worker cap display the total-process budget and the integer worker-thread capacity, so users can see both the configured target and the practical thread cap.

## 1.0.7 - 2026-05-03

### Added

- Added native macOS OS-thread metrics through Mach `task_threads`.
- Added native FreeBSD process metrics through `sysctl`/`kinfo_proc`, including current RSS, OS thread count, and open file descriptor count when available.
- Initialized Tokio's multi-thread runtime and the global Rayon worker pool from `resources.cpu_budget_percent` so async/network work and CPU-bound parallel work default to the automatic CPU budget across the process.

### Changed

- Made `api status --details` and `daemon status --details` more human-readable: uptime and CPU time are formatted as durations, memory is shown as MiB, and `fd_count` is displayed as `open files/sockets`.
- Kept JSON metrics machine-readable while adding clearer aliases such as `open_file_descriptor_count` and `os_thread_count`.

## 1.0.6 - 2026-05-03

### Added

- Added `api status --details` with live API request counters, active request counts, average requests/second, and process resource metrics from the API process itself.
- Added `daemon status --details` with daemon heartbeat, loop count, last auto-trade/daily-refresh summary, and process resource metrics from the daemon process itself.
- Added automatic CPU worker-thread budgeting through `resources.cpu_budget_percent`, defaulting to 80% of logical CPUs for CPU-bound ML work.

### Changed

- CPU-bound LightGBM, CPU XGBoost, and CPU/Rayon LSTM now use the automatic CPU cap by default.
- GPU/NPU paths remain uncapped: MLX, tch/CUDA, and XGBoost CUDA are allowed to use their accelerator path without the CPU worker-thread cap.
- Missing status metrics are reported as `"not available"` instead of `null`.

## 1.0.5 - 2026-05-03

### Added

- Added automatic memory sizing for SQLite cache/mmap, ML symbol batches, LSTM sequences/batches, and LightGBM train/validation row caps.
- Added macOS, Linux cgroup/host, FreeBSD, and generic Unix memory detection so auto resource limits work across supported platforms.
- Added strict runtime config validation with precise JSON paths, expected values, and invalid-value errors before commands run.
- Added concise function maps to Rust modules to make future debugging easier.

### Changed

- `config/mlai-trade.example.json` now uses `resources.* = "auto"` by default and `resources.memory_budget_percent=80`.
- `mlai-trade data db-stats` now reports detected memory source, total memory, memory budget, and derived resource limits.

### Fixed

- Fixed Linux cgroup memory detection to check both cgroup v2 and v1 files instead of stopping after the first missing path.
- Invalid daemon/API config is now logged as JSON `config_invalid` and long-running services pause/fail safely until the config is fixed.

## 1.0.4 - 2026-05-03

### Added

- Added runtime permission hardening: sensitive runtime directories are enforced as `0700`, sensitive config/data/db/log/socket files are enforced as `0600`, and PID metadata files are enforced as `0644`.
- Added API/daemon output redaction for configured Alpaca and FRED secrets before captured command output is logged or returned.
- Added `resources` config for SQLite cache/temp/mmap limits, feature/label batch size, LSTM sequence/batch caps, and LightGBM train/validation row caps.
- Added `mlai-trade data db-stats` and `mlai-trade data db-optimize` for SQLite size inspection and safe maintenance.
- Added `.arclint` so non-Rust text lint can run through `arc lint` in this repository.

### Changed

- Renamed the daemon default PID file from `tmp/mlai-trade.pid` to `tmp/mlai-trade-daemon.pid` so process files are explicit beside `tmp/mlai-trade-api.pid`.
- Resolved blank or relative log, socket, PID, and tax-bracket paths inside their expected runtime folders to prevent accidental writes into the caller's current working directory.
- Generated ML reports, datasets, models, CSV exports, DB files, logs, and runtime control files are now written with private permissions by default.
- Reduced default memory pressure for large historical runs: SQLite temp storage defaults to disk, LSTM materializes at most 50k sampled windows, and LightGBM native datasets are capped by deterministic stride unless configured otherwise.

### Fixed

- Prevented relative log config from creating application logs under `data/` or any other current working directory.

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
