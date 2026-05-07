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
mlai-trade api ssl cert info
mlai-trade api ssl start
mlai-trade api ssl status
mlai-trade api ssl dns-check example.com
```

The remote listener also serves the built React dashboard from `api/html/dist`.
The dashboard uses live API routes for accounts, positions, orders, auto
trading, data, and compliance. It opens directly into the portfolio overview,
polls read-only account/position/order/auto/compliance snapshots, and keeps
provider order sync as an explicit action. Portfolio views include green/red
P&L charts with date labels and a shared Today/3-day/7-day/custom range
selector, two-column overview allocation bars, per-account allocation bars,
per-position mini charts, and paged tables for larger order/position/tax
datasets.
Charts request market bars at range-appropriate granularity: Today uses
1-minute bars, 3 days uses 5-minute bars, 7 days uses 15-minute bars, 8-30 days
uses hourly bars, and longer ranges use daily bars. These on-demand bars are
cached in `market_bar_cache`, not in the daily ML `bars` table. The active
chart interval is shown in the dashboard toolbar, and chart hover tooltips show
the nearest timestamp and P&L value. The Overview performance chart aggregates
provider open-position P&L from those intraday bar series. P&L charts label the
entry break-even line, and per-position charts show an explicit no-bars message
when the provider has no data for the selected range.
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

`ml refresh` and `data daily` share the same full incremental non-trading prep path by default. They fill missing data/artifacts, reconcile/sync the managed feed universe before training, use dated feed aggregates as ML features, train/evaluate all configured models, refresh predictions/ensemble output, and cache default SHAP explanations. `data daily --skip-train` is the data-only exception. `ml full-refresh` forces a rebuild of market data, features, labels, models, predictions, and ensemble output.

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
scripts/linux-ubuntu-test.sh run
```

On Linux, the script runs validation natively and does not install or use a
container. On macOS, FreeBSD, or another non-Linux host, it runs the same checks
inside an Ubuntu 24.04 Docker container. On macOS, if Docker is missing or
stopped, it installs Docker CLI + Colima with Homebrew and starts Colima in the
background. The Ubuntu image is cached locally as `mlai-trade:ubuntu-test`; a
normal run reuses that offline image when the Dockerfile fingerprint matches.
Run `scripts/linux-ubuntu-test.sh update` only when you want to pull/rebuild the
image.

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

Container runs use a `.dockerignore`-filtered copy of the repo. Local runtime
data, DBs, logs, sockets, and real config files are excluded from the build
context and from the test copy inside the container. Warnings in mlai-trade are
treated as errors.

Docker validation modes:

- `scripts/linux-ubuntu-test.sh run`: run validation. On non-Linux hosts this
  uses the cached Ubuntu image, removes any stale kept inspection container
  first, and removes the validation container after the run.
- `scripts/linux-ubuntu-test.sh clean`: remove stale kept containers while
  preserving the cached image and build volumes.
- `scripts/linux-ubuntu-test.sh container`: keep a named Ubuntu container
  running for manual inspection.
- `scripts/linux-ubuntu-test.sh shell`: open a temporary interactive Ubuntu
  shell and remove it on exit.
- `scripts/linux-ubuntu-test.sh update`: pull/rebuild the Ubuntu image
  intentionally.
- `scripts/linux-ubuntu-test.sh delete`: remove the kept container, cached
  image, and Docker build-cache volumes.
- `scripts/linux-ubuntu-test.sh --help`: show script commands and environment
  overrides.

Useful Docker inspection commands:

```sh
docker images mlai-trade
docker ps
docker ps -a
scripts/linux-ubuntu-test.sh container
docker exec -it mlai-trade-ubuntu-test bash
docker rm -f mlai-trade-ubuntu-test
scripts/linux-ubuntu-test.sh clean
scripts/linux-ubuntu-test.sh delete
```

Inside the inspection container, the filtered repo copy is available at
`/tmp/mlai-trade-src`.

The cached Ubuntu image is `mlai-trade:ubuntu-test`. Docker volumes
`mlai-trade-cargo-registry`, `mlai-trade-cargo-git`, and
`mlai-trade-target-linux-ubuntu` keep Rust build caches. On macOS with Colima,
the Docker engine/profile lives under `~/.colima/default`; inside that engine
Docker stores images/volumes under its reported data root, usually
`/var/lib/docker`.

Repo-owned test fixtures live under `tests/`: the Ubuntu Dockerfile is
`tests/linux-ubuntu/Dockerfile`, and FreeBSD/Lima harness notes are in
`tests/freebsd-lima/`. Executable entrypoints remain in `scripts/`.

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
