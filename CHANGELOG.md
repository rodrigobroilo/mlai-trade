# Changelog

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
