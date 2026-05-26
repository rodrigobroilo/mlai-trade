# Linux Build And Operations

This guide covers native Linux builds and normal local operation. The supported
production path is the repo packaging script, which builds the Rust release
binary and installs it as `bin/mlai-trade` with the runtime shared libraries it
needs under `bin/lib/`.

## Prerequisites

Install the normal Rust and native build toolchain first:

```sh
rustup default stable
sudo apt-get install build-essential pkg-config libssl-dev cmake ninja-build git
```

For NVIDIA CUDA acceleration, the host also needs:

```sh
nvidia-smi
nvcc --version
```

`nvidia-smi` must see the GPU, and `nvcc` must come from a CUDA toolkit with
headers and libraries, usually `/usr/local/cuda` or `/usr/local/cuda-*`.

## Build And Package

Auto-detect the best package for the host:

```sh
scripts/package-local-linux.sh
```

Require CUDA or fail:

```sh
MLAI_TRADE_CUDA=1 scripts/package-local-linux.sh
```

Require a CPU-only package:

```sh
MLAI_TRADE_CUDA=0 scripts/package-local-linux.sh
```

CUDA packaging enables every supported NVIDIA path in one binary:

- XGBoost native CUDA, built from upstream XGBoost. Default tag: `v3.2.0`.
- LightGBM CUDA through the bundled LightGBM build.
- LSTM through `tch`/libtorch CUDA.

Useful build overrides:

```sh
MLAI_TRADE_XGBOOST_VERSION=v3.2.0 MLAI_TRADE_CUDA=1 scripts/package-local-linux.sh
MLAI_TRADE_TORCH_CUDA_VERSION=cu128 MLAI_TRADE_CUDA=1 scripts/package-local-linux.sh
MLAI_TRADE_CUDA_ARCHES=89 MLAI_TRADE_CUDA=1 scripts/package-local-linux.sh
```

The package script copies the dependency closure it can resolve into `bin/lib/`.
Do not hand-maintain the CUDA/CPU shared-library list. Re-run the package script
after changing CUDA mode, CUDA toolkit, XGBoost version, Rust dependencies, or
the binary.

## Verify The Binary

```sh
bin/mlai-trade runtime version
bin/mlai-trade data status
bin/mlai-trade ml status
bin/mlai-trade --json ml status
```

On a CUDA-capable Linux package, status should report NVIDIA as available and
XGBoost, LightGBM, and TCH as compiled/available. MLX is expected to report not
compatible on Linux because MLX is the Apple Silicon macOS accelerator.

The daily startup banner should show accelerator build support, for example:

```text
LSTM backend: auto (CUDA build)
XGBoost backend: auto (CUDA build)
LightGBM backend: auto (CUDA build)
Ridge backend: cpu
```

At runtime, `auto` tries the accelerator first and falls back to CPU if the
accelerated process fails. Explicit `cuda`, `tch`, or `mlx` settings fail when
that forced backend is unavailable.

## First Data Run

`data daily` is non-trading. It refreshes data and ML artifacts but does not buy
or sell:

```sh
bin/mlai-trade data daily
```

For bar-only repair or backfill:

```sh
bin/mlai-trade data scan --days 0
```

Preview the backfill plan without touching provider data or local bars:

```sh
bin/mlai-trade data scan --days 0 --dry-run
```

`--days 0` means full available Alpaca daily stock-bar history. The scanner is
symbol-aware: it checks every scan symbol against the expected market dates,
local bars, and completed provider fetch ranges. It backfills incomplete
symbols such as existing positions, without re-downloading already complete
symbols. Provider-held symbols are included even when they are not currently in
the tradable asset table.

The table `bar_sync_coverage` records completed fetch ranges. This prevents
newer IPOs or symbols with no pre-listing data from being re-requested forever
just because they do not have bars back to the oldest provider date.
The table is created automatically when a newer binary opens an older runtime
database; no manual migration is required.

No daily bar is expected for weekends, market holidays, symbol-specific closed
sessions, pre-listing dates, halts/suspensions, or provider/feed gaps. A
successful provider request for an empty range is still recorded as coverage so
the scanner does not retry that same empty history forever.

Use `--force` only when you intentionally want to re-request the selected
window:

```sh
bin/mlai-trade data scan --days 0 --force
```

After backfilling, run the full preparation pipeline so features, labels,
predictions, ensemble output, and SHAP explanations are rebuilt:

```sh
bin/mlai-trade data daily
```

## Explain Positions

List symbols that currently have explainable rows:

```sh
bin/mlai-trade ml explainable
```

Explain a symbol:

```sh
bin/mlai-trade ml explain BSCS
```

If `ml explain SYMBOL` reports no features for the latest date, the symbol does
not yet have enough local daily bars/features for that date. Run:

```sh
bin/mlai-trade data scan --days 0
bin/mlai-trade data daily
```

Then check `ml explainable` again.

## Confirm GPU Use

During CUDA training, these commands should show a compute process and some GPU
activity:

```sh
nvidia-smi
nvidia-smi dmon
```

Small models can use little memory, especially LSTM batches and short LightGBM
phases. Low memory use does not by itself mean CUDA is disabled; check the
mlai-trade banner/logs for `cuda`, `tch CUDA`, or the CUDA child-process
messages.

## Runtime Services

Daemon and API lifecycle:

```sh
bin/mlai-trade daemon start
bin/mlai-trade daemon status --details
bin/mlai-trade api start
bin/mlai-trade api status --details
bin/mlai-trade api ssl start
bin/mlai-trade api ssl status
```

Only enable daemon auto-trading when the account-level and global auto-trading
settings match the intended machine role. `data daily` remains non-trading
regardless of daemon or account settings.

## Troubleshooting

Use the release package script for local Linux binaries. A plain debug
`cargo test` or `cargo check` can try to provision a separate debug libtorch and
fail on offline hosts. The verified production path is:

```sh
MLAI_TRADE_CUDA=1 scripts/package-local-linux.sh
```

Common checks:

```sh
bin/mlai-trade data status
bin/mlai-trade ml status
bin/mlai-trade data db-stats
tail -f logs/mlai-trade-data.log
tail -f logs/mlai-trade-ml.log
```

If CUDA is expected but not shown, verify `nvidia-smi`, `nvcc`, `cmake`,
`ninja`, and `git`, then re-run the package script with `MLAI_TRADE_CUDA=1`.
