# Debugging

Start with path and config visibility:

```sh
mlai-trade runtime version
```

This prints the active runtime home, data directory, database directory, config directory, docs directory, database path, and model path.

If the CLI exits with "No trading provider is enabled", edit:

```text
~/mlai-trade/config/mlai-trade.json
```

and enable at least one provider, for example:

```json
{
  "providers": {
    "alpaca": { "enabled": true },
    "other": {}
  }
}
```

If Alpaca or FRED calls fail, verify the local runtime config file has the
relevant keys. The repository examples intentionally use placeholders. FRED
benchmark sync retries transient upstream failures 10 times. Retry failures and
stale local macro-data fallbacks are written to `logs/mlai-trade-data.log` as
JSON events such as `fred_fetch_retry_failed` and
`fred_stale_local_fallback_used`.

Config is validated before every command. If a key is misspelled or a value is invalid, the error includes the exact JSON path and expected value. Example:

```text
config error at $.resources.memory_budget_percent: value -1 is out of range; expected auto or integer 10-95
```

The same failure is written as a JSON `config_invalid` event in the command log. For daemon/API processes, invalid config pauses or fails request handling safely until the file is fixed.

To debug provider code without live credentials, use the fake Alpaca provider
test:

```sh
scripts/provider-fake-alpaca-test.sh run target/release/mlai-trade
```

To keep its temporary runtime home for inspection:

```sh
MLAI_TRADE_FAKE_ALPACA_KEEP_HOME=1 \
  scripts/provider-fake-alpaca-test.sh run target/release/mlai-trade
```

The test starts a local HTTP fixture with one month of fake stock/ETF bars and
paper trading endpoints, points `alpaca.accounts[].trading_base_url` and
`data_base_url` at that fixture, then tests CLI and Unix-socket API paths. The
fixture log is `logs/fake-alpaca.log` inside the temporary runtime home.

For memory/resource issues, inspect the automatic caps:

```sh
mlai-trade data db-stats
```

This prints the detected memory source. macOS uses `sysctl`, Linux uses cgroup limits or `/proc/meminfo`, FreeBSD uses `sysctl`, and other Unix targets use `sysconf`.

Platform support target is macOS, Linux, and FreeBSD. Native builds on each OS are expected to work with the normal Rust/C toolchain. Cross-checking Linux or FreeBSD from macOS also requires the target C compiler/sysroot because dependencies such as `ring` compile C code.

Linux-path validation can be run with:

```sh
scripts/linux-ubuntu-test.sh run
```

On Linux, the script runs validation natively and does not install or use a
container. On macOS, FreeBSD, or another non-Linux host, it runs the same checks
inside an Ubuntu 24.04 Docker container. Non-Linux hosts default that container
to `linux/amd64` because the mandatory Linux `tch`/libtorch path depends on
upstream amd64 libtorch binaries. Override with `MLAI_TRADE_LINUX_PLATFORM=...`
only when validating a different Linux architecture intentionally. On macOS, if
Docker is missing or stopped, it installs Docker CLI + Colima + buildx with
Homebrew and starts Colima in the background. The image is cached locally as
`mlai-trade:ubuntu-test`; normal runs reuse it offline when the Dockerfile
fingerprint and platform match. Run `scripts/linux-ubuntu-test.sh update` only
when you want to pull/rebuild the image.

Validation runs with `RUSTFLAGS=-D warnings` and executes:

```sh
cargo fmt --check
cargo check
cargo test
cargo build --release
```

Production builds compile the mandatory feature set for the current OS by
default. Apple Silicon links MLX and XGBoost; Linux links XGBoost and
`tch`/libtorch; FreeBSD keeps the portable CPU baseline.
The default macOS Docker Linux validation does not provide NVIDIA GPU
passthrough, so CUDA checks should report unavailable there. For macOS
`linux/amd64` emulation, the script defaults to one Cargo job, clang/clang++,
and CMake parallel level 1 inside the validation container to reduce QEMU
compiler instability. Native Linux validation remains the authoritative Linux
validation path.

Container mode builds `tests/linux-ubuntu/Dockerfile`, mounts the repo
read-only, and copies a `.dockerignore`-filtered tree inside the container.

Docker validation modes:

- `scripts/linux-ubuntu-test.sh run`: run validation. On Linux this runs
  natively; on non-Linux hosts it uses the cached Ubuntu image, removes stale
  kept inspection containers first, and removes the validation container after
  the run.
- `scripts/linux-ubuntu-test.sh clean`: remove the named kept inspection
  container while preserving the cached image and volumes.
- `scripts/linux-ubuntu-test.sh container`: keep a named Ubuntu container
  running for inspection.
- `scripts/linux-ubuntu-test.sh shell`: open a temporary interactive Ubuntu
  shell and remove it on exit.
- `scripts/linux-ubuntu-test.sh update`: pull/rebuild the cached Ubuntu image.
- `scripts/linux-ubuntu-test.sh delete`: remove the named container, cached
  image, and build-cache volumes.
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
```

The copied repo is at `/tmp/mlai-trade-src` inside the container.

Linux validation storage:

- Image: `mlai-trade:ubuntu-test`
- Optional kept container: `mlai-trade-ubuntu-test`
- Docker volumes: `mlai-trade-cargo-registry`, `mlai-trade-cargo-git`,
  `mlai-trade-target-linux-ubuntu`
- Linux Docker data root: `docker info --format '{{.DockerRootDir}}'`
- macOS Colima profile backing Docker: `~/.colima/default`
- Cleanup: `scripts/linux-ubuntu-test.sh clean` removes the kept inspection
  container, while `scripts/linux-ubuntu-test.sh delete` removes cached Docker
  resources.

FreeBSD-path validation can be run with:

```sh
scripts/freebsd-lima-test.sh run
```

On FreeBSD, the script runs validation natively. On macOS, Linux, or another
non-FreeBSD host, it uses a cached Lima FreeBSD 16 VM named
`mlai-trade-freebsd16-test`. On macOS it can install Lima + QEMU with Homebrew
when missing. Normal runs reuse the cached VM; run
`scripts/freebsd-lima-test.sh update` only when you want to recreate it.

Useful FreeBSD inspection commands:

```sh
limactl list
limactl shell mlai-trade-freebsd16-test uname -mrs
limactl shell mlai-trade-freebsd16-test freebsd-version
limactl shell mlai-trade-freebsd16-test
scripts/freebsd-lima-test.sh stop
scripts/freebsd-lima-test.sh --help
```

FreeBSD validation storage:

- Lima instance: `mlai-trade-freebsd16-test`
- Host directory: `~/.lima/mlai-trade-freebsd16-test`
- Guest repo copy: `/tmp/mlai-trade-src`
- Cleanup: `scripts/freebsd-lima-test.sh clean` removes stale guest repo/test
  runtime directories while preserving the VM.

For market-hours problems, verify the configured provider timezone and Alpaca v3 sessions:

```sh
mlai-trade market clock
mlai-trade market calendar
```

For daemon problems:

```sh
mlai-trade data status
tail -f ~/mlai-trade/logs/mlai-trade-daemon.log | jq -c .
tail -f ~/mlai-trade/logs/mlai-trade-auto.log | jq -c .
tail -f ~/mlai-trade/logs/mlai-trade-data.log | jq -c .
tail -f ~/mlai-trade/logs/mlai-trade-ml.log | jq -c .
tail -f ~/mlai-trade/logs/mlai-trade-training.log | jq -c .
tail -f ~/mlai-trade/logs/mlai-trade-feeds.log | jq -c .
```

Logs are JSON lines and rotate daily. Current files keep the stable names above.
Old logs are compressed under `logs/archived/` as
`YYYYMMDD-<log-file>.gz`. Empty current log files mean that component has not
written anything since the last rotation.

Validate active logs with `jq`:

```sh
find ~/mlai-trade/logs -maxdepth 1 -name '*.log' -print \
  -exec sh -c 'jq -c . "$1" >/dev/null' sh {} \;
```

Validate runtime permissions:

```sh
find ~/mlai-trade/config ~/mlai-trade/data ~/mlai-trade/db ~/mlai-trade/logs ~/mlai-trade/api ~/mlai-trade/tmp -maxdepth 1 -print -exec ls -ld {} \;
```

Sensitive directories should be `drwx------`; sensitive files should be `-rw-------`. PID files under `tmp/` are runtime metadata and should be `-rw-r--r--`. The API socket should be owner-only. If a stale log appears outside `logs/`, move it back into `~/mlai-trade/logs/` and restart with the current binary so relative log paths are normalized.

Useful event filters:

```sh
jq 'select(.event == "auto_market_closed_backoff_started")' ~/mlai-trade/logs/mlai-trade-daemon.log
jq 'select(.event == "auto_position_reconciled_from_provider")' \
  ~/mlai-trade/logs/mlai-trade-auto.log
jq 'select(
  .event == "provider_external_order_observed"
  or .event == "provider_external_fill_observed"
)' \
  ~/mlai-trade/logs/mlai-trade-auto.log
jq 'select(.event == "provider_account_snapshot_changed")' \
  ~/mlai-trade/logs/mlai-trade-auto.log
jq 'select(.event | startswith("auto_exit_"))' \
  ~/mlai-trade/logs/mlai-trade-auto.log
jq 'select(.event == "command_failed")' ~/mlai-trade/logs/mlai-trade-ml.log
jq 'select(.event == "api_request" and .status >= 400)' ~/mlai-trade/logs/mlai-trade-api.log
```

For stop-loss/take-profit questions, `auto_exit_confirmation_wait` means the
rule saw a breach but is waiting for configured confirmation cycles or minimum
hold time. `auto_exit_rule_triggered` means the rule reached confirmation or an
emergency threshold and submitted a sell attempt. `auto_exit_order_submitted`
or `auto_exit_order_failed` shows the provider order result.

`auto_position_reconciled_from_provider` means the provider source-of-truth
position snapshot disagreed with a local open `auto_positions` row. The daemon
closed or adjusted the local row before evaluating exit rules so it would not
try to sell shares the broker no longer reports.

`provider_external_order_observed` and `provider_external_fill_observed` mean
the broker reported an order/fill that was not created by mlai-trade.
`provider_account_snapshot_changed` means provider cash changed since the
previous snapshot. These are expected if the user trades, withdraws, or funds
the account directly at the provider. Equity-only mark-to-market changes are
stored in the latest snapshot but are not logged every cycle.

To audit execution origin, use:

```sh
mlai-trade trade orders --sync
mlai-trade auto sync-orders
mlai-trade compliance tax --year 2026 --details --json \
  | jq '.by_execution_origin'
```

`mlai_auto` means daemon auto-trade, `mlai_cli` means an mlai-trade CLI order,
`provider_external` means provider-web/API activity outside mlai-trade, and
`mixed` means the entry and exit came from different origins.

If `mlai-trade daemon start` refuses to run, set `daemon.enabled=true` in the local config. The interval is clamped to 10-300 seconds.

If auto-trade does not repeat during a weekend or holiday, check for `auto_market_closed_backoff_started`. That means the daemon already observed closed market state for the current market date and will try again on the next market date. Manual `mlai-trade auto run` is still available for explicit checks.

If daily daemon maintenance did not run, check `daemon.daily_refresh_enabled`, `daemon.daily_refresh_trigger`, `daemon.daily_refresh_after_close_minutes`, `daemon.daily_refresh_timezone`, and the last success stamp:

```sh
cat ~/mlai-trade/tmp/mlai-trade-daily-refresh.stamp
tail -f ~/mlai-trade/logs/mlai-trade-daemon.log | jq -c .
```

For tax estimates, verify `tax.filing_status` and `tax.estimated_annual_income` are set, then run:

```sh
mlai-trade compliance tax --accounts
mlai-trade compliance tax --year 2026
mlai-trade compliance tax --year 2026 --account alpaca:paper-main --details
mlai-trade compliance tax --year 2026 --quarter 1,2 --export csv
```

If `ml status` shows empty bars/features/models, run:

```sh
mlai-trade ml refresh
```

For data/training visibility during or after a run:

```sh
tail -f ~/mlai-trade/logs/mlai-trade-data.log | jq -c .
tail -f ~/mlai-trade/logs/mlai-trade-training.log | jq -c .
tail -f ~/mlai-trade/logs/mlai-trade-ml.log | jq -c .
```

If provider order/fill tables are empty, run the read-only provider sync:

```sh
mlai-trade auto sync-orders
```

The Git repo ignores generated DBs, datasets, models, reports, local config, and secrets. Use `git status --ignored` when in doubt.

For API resource-miss debugging, call with `curl -s` and inspect `ok`, `status_code`, `error`, and `data`:

```sh
curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock \
  http://localhost/feeds/remove/MTA | jq
```

A wrong symbol, missing resource, or disallowed action should return `ok:false`, not a silent success.

For API overload debugging, inspect HTTP `429` responses and API logs:

```sh
curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock \
  http://localhost/ml/refresh | jq
jq 'select(.event == "api_request" and .status == 429)' ~/mlai-trade/logs/mlai-trade-api.log
```

The JSON response includes `reason` and `retry_after_seconds`. Increase only the relevant config limit: `api.rate_limit_per_minute`, `api.max_concurrent_requests`, or `api.max_concurrent_long_requests`.
