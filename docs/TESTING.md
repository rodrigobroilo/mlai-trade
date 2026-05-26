# Testing

mlai-trade supports three validation targets:

- macOS host validation, including Apple Silicon MLX builds when available.
- Linux validation, native on Linux and through a cached Lima Ubuntu VM on
  non-Linux hosts.
- FreeBSD validation, native on FreeBSD and through a cached Lima FreeBSD 16 VM
  on non-FreeBSD hosts.

All OS validation must be non-interactive. Scripts either reuse cached
images/VMs or update them only when explicitly requested.

## Quick Commands

Run the normal host checks:

```sh
cargo fmt --check
cargo check
cargo test
cargo build --release
scripts/cli-smoke-test.sh run target/release/mlai-trade
scripts/e2e-synthetic-test.sh run target/release/mlai-trade
scripts/provider-fake-alpaca-test.sh run target/release/mlai-trade
arc lint --never-apply-patches
git diff --check
```

Run Linux compatibility checks:

```sh
scripts/linux-lima-test.sh run
```

Run FreeBSD compatibility checks:

```sh
scripts/freebsd-lima-test.sh run
```

All test scripts support `--help`. Scripts do not run with no arguments; pass
the command you want, for example `scripts/linux-lima-test.sh run`.

## Repo Test Asset Layout

Repo-owned test infrastructure lives under `tests/`; executable entrypoints
stay under `scripts/`.

- `tests/linux-lima/`: notes for the Lima-backed Ubuntu validation path used by
  `scripts/linux-lima-test.sh` on non-Linux hosts.
- `tests/repo-sync.exclude`: file-copy exclusions shared by Lima test guests.
- `tests/freebsd-lima/`: notes for the Lima-backed FreeBSD validation path used
  by `scripts/freebsd-lima-test.sh`. The actual VM cache is outside the repo.

## OS Matrix

Targets:

- macOS: direct host validation.
- Linux host: `scripts/linux-lima-test.sh run` validates natively without a VM.
- Non-Linux host for Linux path: `scripts/linux-lima-test.sh run` uses the
  cached Lima Ubuntu 24.04 x86_64 VM; macOS can install Lima + QEMU.
- FreeBSD host: `scripts/freebsd-lima-test.sh run` validates natively without
  Lima.

macOS host validation compiles Apple Silicon MLX and XGBoost by default. On a
macOS/Linux/non-FreeBSD host, `scripts/freebsd-lima-test.sh run` validates the
FreeBSD path through the cached Lima FreeBSD 16 VM named
`mlai-trade-freebsd16-test`; on macOS, Lima + QEMU are installed automatically
when missing.

## Linux Validation

Default run:

```sh
scripts/linux-lima-test.sh run
```

Modes:

- `scripts/linux-lima-test.sh run`: run validation. On Linux this is native; on
  non-Linux hosts it uses the cached Ubuntu VM and removes stale guest work
  directories first.
- `scripts/linux-lima-test.sh clean`: remove stale guest repo/test runtime
  directories while preserving the cached VM.
- `scripts/linux-lima-test.sh shell`: copy the filtered repo and open a shell
  inside `/tmp/mlai-trade-src`.
- `scripts/linux-lima-test.sh update`: delete/recreate the cached Ubuntu VM.
- `scripts/linux-lima-test.sh stop`: stop the cached Ubuntu VM.
- `scripts/linux-lima-test.sh delete`: delete the cached Ubuntu VM.
- `scripts/linux-lima-test.sh --help`: show script commands and environment
  overrides.

On non-Linux hosts, the Lima Linux validation defaults to an x86_64 Ubuntu VM.
The default macOS Lima path does not expose an NVIDIA GPU, so CUDA availability
checks should report unavailable there. Native Linux validation remains the
authoritative Linux validation path.

Inspection:

```sh
limactl list
scripts/linux-lima-test.sh shell
limactl shell mlai-trade-linux-amd64-test
scripts/linux-lima-test.sh stop
```

Inside the guest, the filtered repo copy is available at `/tmp/mlai-trade-src`.

### Linux Cache Location

The Linux path uses these stable Lima locations:

| Item | Name | Where It Is Stored |
| --- | --- | --- |
| Linux VM | `mlai-trade-linux-amd64-test` | Host Lima store under `~/.lima/`. |
| Guest repo copy | `/tmp/mlai-trade-src` | Inside the Ubuntu guest. |
| Guest Cargo target | `/tmp/mlai-trade-target` | Inside the Ubuntu guest. |

Useful storage commands:

```sh
limactl list
du -sh ~/.lima/mlai-trade-linux-amd64-test 2>/dev/null || true
scripts/linux-lima-test.sh clean
scripts/linux-lima-test.sh delete
```

## FreeBSD Validation

Default run:

```sh
scripts/freebsd-lima-test.sh run
```

The default non-FreeBSD guest is FreeBSD 16 through Lima template
`template:experimental/freebsd-16`. The Lima instance is cached as
`mlai-trade-freebsd16-test`; normal runs reuse it. The script installs the
FreeBSD guest dependencies with `pkg` and then copies a
`tests/repo-sync.exclude`-filtered repo tree into `/tmp/mlai-trade-src`.

Modes:

- `scripts/freebsd-lima-test.sh run`: run validation. On FreeBSD this is native; on
  non-FreeBSD hosts it uses the cached Lima FreeBSD 16 VM and removes stale
  guest test work directories first.
- `scripts/freebsd-lima-test.sh clean`: remove stale guest repo/test runtime
  directories while preserving the cached VM.
- `scripts/freebsd-lima-test.sh shell`: copy the filtered repo and open a shell
  inside `/tmp/mlai-trade-src`.
- `scripts/freebsd-lima-test.sh update`: delete/recreate the cached FreeBSD VM.
- `scripts/freebsd-lima-test.sh stop`: stop the cached FreeBSD VM.
- `scripts/freebsd-lima-test.sh delete`: delete the cached FreeBSD VM.
- `scripts/freebsd-lima-test.sh --help`: show script commands and environment
  overrides.

Inspection:

```sh
limactl list
limactl shell mlai-trade-freebsd16-test uname -mrs
limactl shell mlai-trade-freebsd16-test freebsd-version
limactl shell mlai-trade-freebsd16-test
limactl stop -y mlai-trade-freebsd16-test
```

### FreeBSD VM Location

The FreeBSD path uses one cached Lima VM:

| Item | Name | Where It Is Stored |
| --- | --- | --- |
| Lima instance | `mlai-trade-freebsd16-test` | `~/.lima/mlai-trade-freebsd16-test` on the host. |
| Guest repo copy | `/tmp/mlai-trade-src` | Inside the FreeBSD guest; recreated on each run. |

The default VM sizing is 4 CPUs, 4 GiB RAM, and a 30 GiB disk. `run` reuses the
cached VM and leaves it available for future validation. Use `stop` to stop it
without deleting the cache, `clean` to remove stale guest work directories
without deleting the VM, or `delete` to remove the VM cache.

Useful storage commands:

```sh
limactl list
du -sh ~/.lima/mlai-trade-freebsd16-test
scripts/freebsd-lima-test.sh clean
scripts/freebsd-lima-test.sh stop
scripts/freebsd-lima-test.sh delete
```

## Shared CLI Smoke Test

`scripts/cli-smoke-test.sh` validates behavior that should work on every OS
without live market credentials and without placing trades. It creates a
temporary runtime home, installs the example config, enables API/daemon only for
the test, disables daemon daily refresh, and validates:

- `runtime version --json`
- root and topic help output
- `api status`
- `api status --details`
- `daemon status`
- `daemon status --details`
- `data status --json`
- `ml status --json`
- `feeds status --json`
- `auto status --json`
- API start, live `api status --details`, `api test --json`, API stop
- daemon start, live `daemon status --details`, daemon stop

The smoke test verifies JSON output with `jq`. It intentionally does not call
buy, sell, cancel, close, provider quote/news/bar endpoints, or ML training
commands that require external API credentials or long-running datasets.

## Synthetic End-to-End Test

`scripts/e2e-synthetic-test.sh` validates the non-trading data and ML path with
fake deterministic stock/ETF data. It creates a disposable runtime home and
uses `scripts/seed-synthetic-market.sh` to seed:

- tradable assets for synthetic stocks and ETFs
- OHLCV bars for stock/ETF symbols, including SPY, QQQ, and sector ETFs
- macro benchmark rows for S&P 500 and VIX
- feed subscriptions, articles, relationships, and sentiment data

The e2e test then runs:

- `data status --json`
- `feeds status --json`
- `feeds sentiment AAPL --json`
- `data screen`
- `data suggest --json`
- `ml features --force`
- `ml labels --horizon 5`
- `ml train --quick`
- `ml predict`
- `ml baselines --quick`
- `ml walk-forward --quick --folds 2`
- `ml lstm-train --backend cpu --single-thread`
- `ml lstm-predict`
- `ml ensemble`
- `ml status --json`
- `ml explainable`
- API start/status/test/stop
- daemon start/status/stop

It still does not place buy/sell/cancel/close orders and does not call live
provider market endpoints. It is designed to catch DB schema, feature, label,
model, status, API, daemon, and JSON regressions on every supported OS.

For focused LSTM tuning comparisons, seed the same fixture once and run
backend/profile variants against that disposable home:

```sh
home="$(mktemp -d /tmp/mlai-lstm-tuning.XXXXXX)"
MLAI_TRADE_SYNTHETIC_DAYS=900 scripts/seed-synthetic-market.sh run "$home"
target/release/mlai-trade --home "$home" ml features --force
target/release/mlai-trade --home "$home" ml labels --horizon 5
target/release/mlai-trade --home "$home" ml lstm-train \
  --backend cpu --single-thread
target/release/mlai-trade --home "$home" ml lstm-train --backend mlx
target/release/mlai-trade --home "$home" ml lstm-predict
target/release/mlai-trade --home "$home" ml lstm-evaluate \
  --top-n 10 --slippage-bps 10
```

Use CLI overrides such as `--hidden-dim`, `--epochs`, `--learning-rate`, and
`--target-mode direction` to compare profiles. Training reports are written to
`$home/data/lstm_training_report.json`.

## Fake Alpaca Provider Test

`scripts/provider-fake-alpaca-test.sh` validates the implemented Alpaca provider
paths without using live Alpaca credentials. It starts the hidden local fixture:

```sh
mlai-trade runtime fake-alpaca-server --addr 127.0.0.1:0
```

The fixture exposes the Alpaca endpoint shapes used by mlai-trade:

- `/v2/account`, `/v2/assets`, `/v2/orders`, `/v2/positions`, and
  `/v2/account/activities/FILL`
- `/v3/clock` and `/v3/calendar/{market}`
- `/v2/stocks/bars`, `/v2/stocks/{symbol}/bars`,
  `/v2/stocks/{symbol}/quotes/latest`, and `/v2/stocks/{symbol}/snapshot`
- `/v1beta1/news` and `/v1beta1/screener/stocks/movers`

It serves one month of deterministic stock/ETF bars for AAPL, MSFT, NVDA, GOOG,
IBM, SPY, QQQ, XLK, XLF, and IWM. Paper orders fill immediately and mutate fake
cash, positions, orders, and fills in memory.

The test writes a disposable config that sets
`alpaca.accounts[].trading_base_url` and `alpaca.accounts[].data_base_url` to
the fixture URL, then runs:

- account, clock, calendar, quote, bars, news, data-feed, movers, and
  history-start commands
- `data universe`, `data scan --days 30 --force`, `data status`, `data screen`,
  and `data suggest`
- `ml status --json`
- paper `trade buy`, `trade sell`, `trade orders --sync`, `trade positions`,
  and `auto sync-orders`
- tax account listing and paper tax estimate
- Unix-socket API start/status/health plus API trade/account, quote, bars,
  buy, sell, orders, and positions routes

Run it directly:

```sh
scripts/provider-fake-alpaca-test.sh run target/release/mlai-trade
```

Keep the disposable runtime home for inspection:

```sh
MLAI_TRADE_FAKE_ALPACA_KEEP_HOME=1 scripts/provider-fake-alpaca-test.sh run target/release/mlai-trade
```

The Linux and FreeBSD validation scripts run this fake-provider test after the
shared smoke test and synthetic ML e2e test.

## Functional Test Coverage

| Area | Safe automated test | Live/provider test |
| --- | --- | --- |
| Runtime/config | `runtime version --json`, config parse tests, unknown-key tests | Real runtime home validation before daemon/API restart |
| Shell completions | `runtime completions generate <shell>` | Optional install/uninstall in a disposable shell profile |
| Daemon | start/status/details/stop in smoke test | Full daemon cycle with paper account and real provider keys |
| API | start/status/details/test/stop in smoke test | Route allowlist and overload tests against the Unix socket |
| Trade/account | Smoke status plus fake Alpaca account/orders/positions/buy/sell/sync in `provider-fake-alpaca-test.sh` | Real paper-account account/orders/positions/sync; buy/sell/cancel/close only in explicit paper trading tests |
| Market data | Fake Alpaca quote/bars/news/clock/calendar/history-start in `provider-fake-alpaca-test.sh` | SIP quote/bars/news/clock/calendar/history-start with paper or real data keys |
| Data pipeline | Synthetic e2e plus fake Alpaca universe/scan/screen/movers/suggest | `daily`/full-history refresh with configured provider/FRED keys |
| Compliance/tax | Unit tests, empty DB status paths, and fake paper-account tax estimate | Tax estimates against synced real/paper account history |
| Feeds | `feeds status --json` without network | add/remove/list/sync/search/graph/sentiment/correlate with network enabled |
| ML | `ml status --json` without datasets | refresh/full-refresh/features/labels/train/predict/walk-forward/LSTM/XGBoost/ensemble/SHAP with full data |
| Auto trade | `auto status --json` and fake Alpaca `auto sync-orders` | run/history/config with paper account; no real-money mutations in validation |

## Full Test Order

Use this order before pushing a release:

1. Host quick commands, including `scripts/e2e-synthetic-test.sh run` and
   `scripts/provider-fake-alpaca-test.sh run`.
2. Linux validation with `scripts/linux-lima-test.sh run`, which also runs the
   CLI smoke, synthetic e2e, and fake Alpaca provider tests.
3. FreeBSD validation with `scripts/freebsd-lima-test.sh run`, which also runs the
   CLI smoke, synthetic e2e, and fake Alpaca provider tests.
4. Live paper-provider integration tests, if credentials are configured.
5. ML long-run validation only after data refresh is intentionally requested.
6. `arc lint --never-apply-patches`.
7. `git diff --check`.

Do not push until all intended OS and integration checks for the change are
complete.
