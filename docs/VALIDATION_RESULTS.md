# Validation Results

This file records release-level validation runs that were completed before
publishing. It is intentionally concise; detailed command coverage lives in
`docs/TESTING.md`.

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
RUSTFLAGS=-D warnings cargo check
cargo test
cargo build --release --features mlx-lstm
scripts/cli-smoke-test.sh run target/release/mlai-trade
scripts/e2e-synthetic-test.sh run target/release/mlai-trade
scripts/provider-fake-alpaca-test.sh run target/release/mlai-trade
arc lint --never-apply-patches
git diff --check
```

Linux validation covered:

- `cargo fmt --check`
- `cargo check --no-default-features`
- `cargo test --no-default-features`
- `cargo build --release --no-default-features`
- CLI smoke test
- synthetic data/ML/API/daemon end-to-end test
- fake Alpaca provider end-to-end test

FreeBSD validation covered the same sequence as Linux inside the cached
FreeBSD 16 Lima VM.

Post-run cleanup:

- No Docker test container was left running.
- The FreeBSD Lima VM `mlai-trade-freebsd16-test` was stopped.
