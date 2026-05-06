# Validation Results

This file records release-level validation runs that were completed before
publishing. It is intentionally concise; detailed command coverage lives in
`docs/TESTING.md`.

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
cargo check --locked --all-features
RUSTFLAGS="-D warnings" cargo check --locked
RUSTFLAGS="-D warnings" cargo check --locked --all-features
cargo test --locked
cargo build --release --locked --all-features
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
| Linux Ubuntu 24.04 container | `scripts/linux-ubuntu-test.sh run` | Passed |
| FreeBSD 16 Lima VM | `scripts/freebsd-lima-test.sh run` | Passed |

macOS host commands that passed:

```sh
cargo fmt --check
RUSTFLAGS=-D warnings cargo check --all-features
cargo test --all-features
cargo build --release --all-features
scripts/cli-smoke-test.sh run target/release/mlai-trade
scripts/e2e-synthetic-test.sh run target/release/mlai-trade
scripts/provider-fake-alpaca-test.sh run target/release/mlai-trade
arc lint --never-apply-patches
git diff --check
```

Linux validation covered:

- `cargo fmt --check`
- `cargo check --all-features`
- `cargo test --all-features`
- `cargo build --release --all-features`
- CLI smoke test
- synthetic data/ML/API/daemon end-to-end test
- fake Alpaca provider end-to-end test

FreeBSD validation covered the same sequence as Linux inside the cached
FreeBSD 16 Lima VM.

Post-run cleanup:

- No Docker test container was left running.
- The FreeBSD Lima VM `mlai-trade-freebsd16-test` was stopped.
