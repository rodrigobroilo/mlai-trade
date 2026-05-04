# Changelog

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
