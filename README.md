# mlai-trade

ML/AI trading CLI with broker modules, shared ML pipelines, compliance guardrails, and configurable local runtime storage.

Default runtime home:

```sh
~/mlai-trade
```

Runtime layout:

- `api/`: Unix-socket API runtime files and built React webapp assets for the
  optional remote H3 dashboard
- `bin/`: installed local binaries and helper executables
- `config/`: local configuration and secrets, never committed
- `data/`: generated ML datasets, models, reports, and market research artifacts
- `db/`: SQLite databases for trades, market data, compliance state, predictions, and scanner state
- `docs/`: local documentation copies
- `logs/`: current JSON-line application logs
- `logs/archived/`: compressed rotated logs
- `tmp/`: PID files, daily refresh stamps, the update lock, and other
  transient runtime state

The CLI creates these folders automatically on startup. Runtime privacy is enforced on startup and when files are written: sensitive directories (`config/`, `data/`, `db/`, `logs/`, `api/`, and `tmp/`) are `0700`; sensitive files inside them, including `mlai-trade.example.json`, `mlai-trade.json`, DBs, generated ML artifacts, logs, and sockets, are `0600`. PID files are runtime metadata and use `0644`.

Override the runtime home with:

```sh
mlai-trade --home /path/to/runtime runtime version
```

or in local process configuration:

```sh
MLAI_TRADE_HOME=/path/to/runtime mlai-trade runtime version
```

Configuration file:

```text
~/mlai-trade/config/mlai-trade.json
```

ML/training tuning file:

```text
~/mlai-trade/config/mlai-trade-ml-tuning.json
```

The repository tracks `config/mlai-trade.example.json` and
`config/mlai-trade-ml-tuning.example.json`; real keys and local tuning stay in
the local runtime config files.

Alpaca is the implemented provider today. The config supports multiple Alpaca accounts, with paper and real-money compliance state separated. Real accounts share taxpayer-wide compliance blockers across accounts; paper accounts obey the same rules in a separate simulation universe.

Auto-trade uses provider calendar/clock checks plus local exchange-time guardrails. Event timestamps are stored in UTC, with the provider/exchange timezone and session source stored alongside trade records.
Normal stop-loss and take-profit exits use restart-safe confirmation counters
by default. The auto log records why an exit is waiting, how many cycles remain,
and when the rule finally submits a sell order.
Before exit orders, local auto positions are reconciled with the provider's
live position snapshot so stale local rows do not submit invalid short sells.
Submitted exit orders do not close local tracking until provider fills or the
provider position snapshot confirm that the shares are gone. If a limit exit
expires unfilled, the position remains auto-managed and can be retried by the
next cycle. If provider reconciliation later finds shares still held for a
previously `mlai-auto` position and the user did not explicitly untrack it,
tracking is recovered from the provider source of truth.
Manual provider-side orders, fills, and cash changes are stored and logged as
external activity during provider sync; the broker remains the source of truth.
Provider orders/fills are also classified by execution origin:
`mlai_auto`, `mlai_cli`, or `provider_external`, with `mixed` used for realized
lots entered and exited through different channels. Orders, auto history, tax
details, and status reports expose these labels so P&L can be reviewed by
provider-web activity, CLI trades, auto-trading, and overall totals.
Position management can be changed without placing orders:
`mlai-trade auto track SYMBOL --account alpaca:paper-main` adopts an existing
provider/CLI-held position into auto management, while `mlai-trade auto untrack
SYMBOL --account alpaca:paper-main` releases it back to manual management.
Both commands require one explicit symbol and the full `provider:account-ref`
selector; `ALL` is intentionally rejected, and bare or broad account selectors
such as `paper` or `alpaca` are not accepted for ownership changes.

Daemon mode can run the automatic auto-trade loop and tax estimate refresh without cron:

```sh
mlai-trade daemon start
mlai-trade daemon status
mlai-trade daemon status --details
mlai-trade daemon reload
mlai-trade daemon restart
mlai-trade daemon stop
```

The API has explicit transports. `api unix` is the local Unix-socket JSON API.
`api ssl` is the optional remote HTTPS transport: TCP/443 and UDP/443 by
default, TLS 1.3 only, hybrid ML-KEM preferred, and only strong TLS 1.3
classical fallback groups for browser compatibility. TCP HTTPS serves the
dashboard and JSON API directly and advertises `Alt-Svc: h3=":443"` so browsers
can upgrade to HTTP/3/QUIC. The Let's Encrypt TLS-ALPN-01 responder is disabled
by default and remains challenge-only when enabled.

The Unix-socket API has its own lifecycle and refuses to start unless
`api.enabled=true` and `api.unix.enabled=true`:

```sh
mlai-trade api unix start
mlai-trade api unix status
mlai-trade api unix test
mlai-trade api start
mlai-trade api status
mlai-trade api status --details
mlai-trade api test
mlai-trade api reload
mlai-trade api restart
mlai-trade api stop
```

Remote planning/status commands:

```sh
mlai-trade api ssl cert generate --target h3
mlai-trade api ssl cert renew --target h3 --domain localhost \
  --organization MLAI-TRADE --organizational-unit MLAI-TRADE
mlai-trade api ssl cert info
mlai-trade api ssl start
mlai-trade api ssl status
mlai-trade api ssl dns-check example.com
```

`api ssl dns-check` validates DNS HTTPS/SVCB H3 discovery and reports the `ech`
parameter when present. Enabling `api.ssl.ech.enabled=true` still fails closed
until the server listener stack can terminate RFC 9849 ECH.

Generated remote API certificates default to `O=MLAI-TRADE` and
`OU=MLAI-TRADE`. Override with `--organization`/`--o` and
`--organizational-unit`/`--ou` when generating or renewing a certificate.

The remote listener also serves the built React dashboard from `api/html/dist`.
The dashboard uses live API routes for accounts, positions, orders, auto
trading, data, and compliance. It opens directly into the portfolio overview,
polls read-only account/position/order/auto/compliance snapshots, and keeps
provider order sync as an explicit action. Portfolio views include green/red
P&L charts with date labels and a shared Today/3-day/7-day/custom range
selector, two-column overview allocation bars, per-account allocation bars,
per-position mini charts, and paged tables for larger order/position/tax
datasets. The dashboard keeps the active tab in the URL hash, so browser
refreshes and copied links reopen the same section. The top-bar account
selector defaults to all accounts, can filter the view to one account, and also
scopes the Tax selector when an account is selected. Browser/dashboard dates
and times render in the browser timezone; API requests include that timezone in
`x-mlai-client-timezone` for app clients that want to log or adapt display
context. The Orders tab shows FIFO realized P&L for filled sell orders once
provider fills have been synced.
The dashboard opens `/events/stream` for lightweight realtime refresh hints.
When the browser is using H3, those events ride the HTTP/3/QUIC stream; if the
stream is unavailable, the dashboard keeps its normal snapshot polling fallback.
`/events/snapshot` exposes the same runtime heartbeat as JSON for adaptive
clients.
Charts request market bars at range-appropriate granularity: Today uses
5-minute bars, 3 days uses 15-minute bars, 7 days uses 30-minute bars, 8-30
days uses hourly bars, and longer ranges use daily bars. These bars are cached
in `market_bar_cache`, not in the daily ML `bars` table. Fresh cache rows are
used before a provider request, and the daemon can proactively warm current
provider-position bars so the dashboard usually reads locally. The active chart
interval is shown in the dashboard toolbar, and chart hover tooltips show the
nearest timestamp and P&L value. The Overview performance chart aggregates
provider open-position P&L from those intraday bar series. P&L charts label the
entry break-even line; per-position charts also draw a vertical buy marker when
the entry timestamp is inside the selected range. Per-position charts show an
explicit no-bars message when the provider has no data for the selected range.
The dashboard does not render raw API response panels.
Position chart bars are requested in `/market/bars?symbols=...` batches. The
API limit is 50 symbols and 25,000 requested bars per batch. Clients can query
`/limits` to discover caps, dashboard table sizes, and supported compression
encodings. API responses can use `zstd`, `br`, `gzip`, or `deflate` when the
client sends the matching `Accept-Encoding`; use `curl --compressed` in scripts.
Localhost browser access over `localhost`, `127.0.0.1`, or `[::1]` does not
require authentication. Non-localhost remote clients must authenticate with the
configured `api.ssl.auth` username/password. `robots.txt` blocks crawler and
AI-agent indexing, but authentication is the real access control. IPv4 and IPv6
remote SSL listeners are both enabled by default and can be disabled separately
with `api.ssl.ipv4_enabled` or `api.ssl.ipv6_enabled`.

Full API routes, request shapes, and curl examples are documented in
`docs/API.md`. Both API transports include explicit overload protection with
rate, concurrency, long-operation, and body-size limits configured under `api`.
API command output is redacted for configured Alpaca and FRED secrets before it
is returned or logged.

Documentation map:

- `docs/README.md`: documentation index and first commands.
- `docs/USAGE.md`: operator guide and command reference.
- `docs/CONFIGURATION.md`: config file reference.
- `docs/LINUX.md`: native Linux build, CUDA packaging, and operations guide.
- `docs/API.md`: Unix-socket API reference.
- `docs/DEBUGGING.md`: troubleshooting and JSONL log inspection.
- `docs/IRS_TAX_RULES.md`: tax/compliance reference.
- `docs/TRADING_KNOWLEDGE.md`: Alpaca/trading API and strategy notes.
- `docs/VALIDATION_RESULTS.md`: release validation commands and results.
- `CHANGELOG.md`: user-facing changes by release.

Federal tax estimates are available through:

```sh
mlai-trade compliance tax --accounts
mlai-trade compliance tax --show-brackets --year 2026
mlai-trade compliance tax --year 2026
mlai-trade compliance tax --year 2026 --account alpaca:paper-main --details
mlai-trade compliance tax --year 2026 --quarter 1,2 --export csv
```

Use `mlai-trade -v` or `mlai-trade --version` for the binary version. The
runtime-path view remains `mlai-trade runtime version`.

Tax bracket/rate data is read from `~/mlai-trade/config/tax-brackets.json`.
Start from `config/tax-brackets.example.json` and add future IRS years as JSON diffs.

First ML setup or repair:

```sh
mlai-trade ml refresh
mlai-trade data daily
```

`ml refresh` and `data daily` share the same full incremental non-trading prep
path by default. They fill missing data/artifacts, reconcile/sync the managed
feed universe before training, use dated feed aggregates as ML features,
train/evaluate all configured models, refresh predictions/ensemble output, and
cache default SHAP explanations. Alpaca daily bars catch up through the latest
completed configured market date even if FRED/SP500 observations are still
lagging. Bar catch-up is symbol-aware: incomplete symbols, including current
provider-held positions, are backfilled without re-requesting symbols that
already have complete local coverage. `data daily --skip-train` is the
data-only exception. `ml full-refresh` forces a rebuild of market data,
features, labels, models, predictions, and ensemble output.

Only one long update can run at a time. Manual `data daily`, `ml refresh`,
`ml full-refresh`, and daemon daily maintenance share
`tmp/mlai-trade-update.lock`; a second command reports the current owner with
PID, operation, and start time instead of overlapping work. Start, finish,
failure, cancellation, duration, and stale-lock cleanup events are JSON logged.

Large DBs are expected when full-history bars and wide ML features are enabled. Runtime resources are controlled automatically by the `resources` config section. By default, mlai-trade detects usable RAM on macOS, Linux, FreeBSD, or generic Unix, sizes SQLite/ML limits from an 80% memory budget, and caps Tokio async workers plus CPU-bound worker threads to 80% of total logical CPU capacity. On 16 logical CPUs, that target is `1280%` in top-style CPU terms. GPU/NPU paths are not CPU-capped. Use `mlai-trade data db-stats` to inspect table sizes, detected memory source, and active resource caps.

LSTM training uses backend-aware profiles. CPU defaults stay conservative for
small machines; MLX/TCH accelerator profiles can use wider models and longer
training. `backend.lstm=auto` selects the backend first, then applies the
matching profile from `mlai-trade-ml-tuning.json`. If an accelerator fails and
auto falls back to CPU, the CPU tuning profile is used. The current accelerator
default comes from the paused 365-day real-data sweep: hidden `128`,
`lr=0.0001`, MSE, dropout `0.1`, weight decay `0.01`, and default ensemble
fallback `LightGBM=40%` plus `LSTM=60%`.

Linux NVIDIA builds are packaged with CUDA automatically when `nvidia-smi`,
`nvcc`, `cmake`, `ninja`, `git`, and a compatible CUDA toolkit are available:

```sh
scripts/package-local-linux.sh
```

CUDA packaging builds upstream XGBoost `v3.2.0` with CUDA and links the Rust
FFI crate to that library; `MLAI_TRADE_XGBOOST_VERSION` can select another
upstream tag. It also enables LightGBM CUDA and the Linux `tch`/libtorch CUDA
LSTM path when the toolkit is available. Use
`MLAI_TRADE_CUDA=1 scripts/package-local-linux.sh` to require CUDA or fail, and
`MLAI_TRADE_CUDA=0 scripts/package-local-linux.sh` to require the CPU package.
XGBoost, LightGBM, and LSTM `auto` try CUDA in CUDA-packaged binaries and fall
back to CPU if the accelerated child process fails. Ridge is CPU-only today.
Run `bin/mlai-trade ml status` or `bin/mlai-trade --json ml status` to see
CUDA, MLX, and tch support for the current binary and host. See
`docs/LINUX.md` for the full Linux build and operations guide.

Config is validated before commands run. Invalid keys or values fail with a precise JSON path and expected values, for example `$.resources.memory_budget_percent` must be `auto` or an integer from `10` to `95`, and `$.resources.cpu_budget_percent` must be `auto` or an integer from `10` to `100`.

Full validation matrix:

```sh
docs/TESTING.md
```

Synthetic no-credential end-to-end validation:

```sh
scripts/e2e-synthetic-test.sh run target/release/mlai-trade
```

Fake Alpaca provider validation with one month of local stock/ETF data and no
live credentials:

```sh
scripts/provider-fake-alpaca-test.sh run target/release/mlai-trade
```

Linux-path validation:

```sh
scripts/linux-lima-test.sh run
```

On Linux, the script runs validation natively and does not install or use a
VM. On macOS, FreeBSD, or another non-Linux host, it runs the same checks
inside a cached Lima Ubuntu 24.04 x86_64 VM named
`mlai-trade-linux-amd64-test`. The x86_64 guest keeps the Linux `tch`/libtorch,
AWS-LC, XGBoost, and LightGBM paths close to production Linux and avoids the
binfmt user-mode QEMU compiler crashes seen with `aws-lc-sys`. On macOS,
if Lima or QEMU is missing, the script can install them with Homebrew. Normal
runs reuse the cached VM; use `scripts/linux-lima-test.sh update` only when you
intentionally want to recreate it.

The validation commands are:

```sh
cargo fmt --check
cargo check
cargo test
cargo build --release
scripts/cli-smoke-test.sh run target/release/mlai-trade
scripts/e2e-synthetic-test.sh run target/release/mlai-trade
scripts/provider-fake-alpaca-test.sh run target/release/mlai-trade
```

Production binaries now compile the mandatory platform feature set by default:
Apple Silicon gets MLX LSTM plus XGBoost, Linux gets XGBoost plus the
`tch`/libtorch dependency, and FreeBSD keeps the portable CPU baseline.
The default macOS Lima Linux validation does not provide NVIDIA GPU passthrough,
so CUDA should report unavailable there. Native Linux builds remain the
authoritative Linux validation path.

Linux Lima validation modes:

- `scripts/linux-lima-test.sh run`: run validation. On Linux this is native; on
  non-Linux hosts it uses the cached Ubuntu VM and removes stale guest test work
  directories first.
- `scripts/linux-lima-test.sh clean`: remove stale guest repo/test runtime
  directories while preserving the cached VM.
- `scripts/linux-lima-test.sh shell`: copy the filtered repo and open a shell
  inside `/tmp/mlai-trade-src`.
- `scripts/linux-lima-test.sh update`: delete/recreate the cached Linux VM.
- `scripts/linux-lima-test.sh stop`: stop the cached Linux VM.
- `scripts/linux-lima-test.sh delete`: delete the cached Linux VM.
- `scripts/linux-lima-test.sh --help`: show script commands and environment
  overrides.

Useful Linux VM inspection commands:

```sh
limactl list
limactl shell mlai-trade-linux-amd64-test uname -mrs
limactl shell mlai-trade-linux-amd64-test
scripts/linux-lima-test.sh stop
scripts/linux-lima-test.sh clean
scripts/linux-lima-test.sh --help
```

The cached Linux VM directory is `~/.lima/mlai-trade-linux-amd64-test`. The
repo copy inside the guest is `/tmp/mlai-trade-src` and is recreated each run.
The guest Cargo target cache is `/tmp/mlai-trade-target`.

Repo-owned test fixtures live under `tests/`: Linux/Lima notes are in
`tests/linux-lima/`, and FreeBSD/Lima notes are in `tests/freebsd-lima/`.
Executable entrypoints remain in `scripts/`.

FreeBSD-path validation:

```sh
scripts/freebsd-lima-test.sh run
```

On FreeBSD, the script runs validation natively. On macOS, Linux, or another
non-FreeBSD host, it runs the same checks inside a cached Lima FreeBSD 16 VM
named `mlai-trade-freebsd16-test`. On macOS, if Lima or QEMU is missing, it
installs them with Homebrew. Normal runs reuse the cached VM; use
`scripts/freebsd-lima-test.sh update` only when you intentionally want to
delete and recreate it.

Useful FreeBSD VM inspection commands:

```sh
limactl list
limactl shell mlai-trade-freebsd16-test uname -mrs
limactl shell mlai-trade-freebsd16-test freebsd-version
limactl shell mlai-trade-freebsd16-test
scripts/freebsd-lima-test.sh stop
scripts/freebsd-lima-test.sh clean
scripts/freebsd-lima-test.sh --help
```

The cached FreeBSD VM directory is `~/.lima/mlai-trade-freebsd16-test`. The
repo copy inside the guest is `/tmp/mlai-trade-src` and is recreated each run.
`run` removes stale guest test work directories first; `clean` does that
without running validation, and `delete` removes the cached VM.

Optional shell autocomplete scripts:

```sh
mlai-trade runtime completions install zsh
mlai-trade runtime completions uninstall zsh
mlai-trade runtime completions generate zsh
```
