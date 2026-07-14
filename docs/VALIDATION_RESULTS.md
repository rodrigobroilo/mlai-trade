# Validation Results

This file records release-level validation runs that were completed before
publishing. It is intentionally concise; detailed command coverage lives in
`docs/TESTING.md`.

## 3.0.0 - 2026-07-14

Host: Apple M4 Max, 16 CPU cores, 40 GPU cores, 64 GB RAM, macOS arm64.

Validation passed:

```sh
cargo fmt --check
cargo check
cargo test
cargo test --release
cargo clippy --all-targets
cargo build --release
cd api/html && npm run build
scripts/cli-smoke-test.sh run target/release/mlai-trade
scripts/e2e-synthetic-test.sh run target/release/mlai-trade
scripts/provider-fake-alpaca-test.sh run target/release/mlai-trade
target/release/mlai-trade --json ml accelerators --strict
git diff --check
```

Results:

- 27 Rust tests passed, including news migration, missing-session labels, and
  bounded/full-history feature equivalence.
- Strict MLX smoke passed on the Metal GPU after installing Xcode's official
  Metal Toolchain component. NPU/Neural Engine remains unavailable because the
  application has no supported Core ML model path.
- A full-size online SQLite backup completed in about 22 seconds while services
  stayed up. Clone migrations completed in 18-35 seconds with about 3.9 GB peak
  resident memory.
- Production migration completed in 10.60 seconds; total stop-to-start time was
  60 seconds. `bars` (19,500,787), `news_articles` (465,007), and
  `price_correlations` (535,152) counts were preserved. Relationship integrity
  and Unix/SSL API plus daemon health checks passed after restart.

## 1.1.26 - 2026-05-06

Release scope:

- Added LSTM loss, dropout, weight-decay, and target-scaling controls.
- Selected the accelerator default from the paused 365-day real-data sweep.
- Left the sweep helper script and partial sweep artifacts outside Git; resume
  metadata is stored with the local sweep result folder.

Paused real-data sweep:

| Scope | Value |
| --- | --- |
| Dataset | latest 365 labeled days from local real market/features/feed data |
| Completed variants | 442 / 649 |
| Resume file | `/tmp/mlai-full-ml-real-365-20260504T170654Z/sweep/RESUME.md` |
| Selected default | `h128_lr0p0001_mse0_do0p1_wd0p01` |

Selected default metrics:

| Metric | Value |
| --- | ---: |
| Direction accuracy | 55.40% |
| Eval IC | 0.2122 |
| Standalone mean return | 3.5560 |
| Standalone win rate | 52.37% |
| Ensemble weights | LightGBM 40% / LSTM 60% |
| Ensemble IC | 0.1971 |
| Ensemble mean return | 5.8765 |
| Ensemble win rate | 60.79% |

Notes:

- The highest standalone IC variant had better IC but much lower standalone
  mean return. The highest ensemble-return variant had weaker IC. The selected
  default is the best balanced result observed before pausing.
- Final completion of all 649 variants can supersede this default.

## 1.1.4 - 2026-05-04

Release scope:

- Added separate ML tuning config:
  `~/mlai-trade/config/mlai-trade-ml-tuning.json`.
- Added backend-aware LSTM profiles for CPU, MLX, and tch/CUDA targets.
- Added LSTM target mode, hidden width, epoch, learning-rate, and early
  stopping controls.
- Replaced the MLX built-in LSTM conversion path with a custom MLX cell that
  round-trips into the same portable Rust inference model used by prediction
  and evaluation commands.
- Documented ML tuning, LSTM defaults, sequence scaling, technical indicators,
  and focused LSTM comparison commands.

macOS host commands that passed:

```sh
cargo fmt --check
cargo check --locked
RUSTFLAGS="-D warnings" cargo check --locked
cargo test --locked
cargo build --release --locked
scripts/e2e-synthetic-test.sh run target/release/mlai-trade
scripts/provider-fake-alpaca-test.sh run target/release/mlai-trade
arc lint --never-apply-patches
git diff --check
```

Focused synthetic LSTM comparison:

| Variant | Target | Hidden | LR | Epochs | Val MSE | Val IC | Direction |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| CPU | return | 64 | 0.001 | 10 | 0.000043 | 0.8886 | 92.01% |
| CPU | return | 128 | 0.001 | 20 | 0.000022 | 0.9477 | 96.67% |
| MLX | return | 64 | 0.001 | 10 | 0.000024 | 0.9429 | 96.57% |
| MLX | return | 128 | 0.001 | 20 | 0.000014 | 0.9698 | 98.48% |
| MLX | return | 256 | 0.001 | 19 | 0.000014 | 0.9625 | 99.14% |
| MLX | direction | 128 | 0.001 | 11 | 0.003620 | 0.6993 | 99.52% |

Result: return regression remains the default because it produced much better
ranking IC for ensemble/auto-trade scoring. MLX 128/20 is the best default on
this fixture: it materially improved over MLX 64 and CPU 64 while avoiding the
extra width of MLX 256, which did not improve IC.

## 1.1.0 - 2026-05-03

Release scope:

- Added cross-platform validation scripts for macOS host, Linux/Ubuntu, and
  FreeBSD.
- Added the local fake Alpaca provider fixture with one month of deterministic
  stock/ETF data.
- Validated provider account, market-data, order, fill, position, tax, and
  Unix-socket API paths without live Alpaca credentials.
- Added per-account Alpaca endpoint overrides for test fixtures:
  `trading_base_url` and `data_base_url`.
- Added FreeBSD build/link support for the LightGBM dependency.
- Added documentation for test commands, storage locations, and inspection
  workflows.

Validated platforms:

| Platform | Command | Result |
| --- | --- | --- |
| macOS host | Host Rust checks plus smoke/e2e scripts | Passed |
| Linux Lima VM | `scripts/linux-lima-test.sh run` | Pending rerun |
| FreeBSD 16 Lima VM | `scripts/freebsd-lima-test.sh run` | Passed |

macOS host commands that passed:

```sh
cargo fmt --check
RUSTFLAGS=-D warnings cargo check
cargo test
cargo build --release
scripts/cli-smoke-test.sh run target/release/mlai-trade
scripts/e2e-synthetic-test.sh run target/release/mlai-trade
scripts/provider-fake-alpaca-test.sh run target/release/mlai-trade
arc lint --never-apply-patches
git diff --check
```

Linux validation covered:

- `cargo fmt --check`
- `cargo check`
- `cargo test`
- `cargo build --release`
- CLI smoke test
- synthetic data/ML/API/daemon end-to-end test
- fake Alpaca provider end-to-end test

FreeBSD validation covered the same sequence as Linux inside the cached
FreeBSD 16 Lima VM.

Post-run cleanup:

- The Linux Lima VM is reusable for future validation.
- The FreeBSD Lima VM `mlai-trade-freebsd16-test` was stopped.
