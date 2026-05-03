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

If Alpaca or FRED calls fail, verify the local runtime config file has the relevant keys. The repository examples intentionally use placeholders.

Config is validated before every command. If a key is misspelled or a value is invalid, the error includes the exact JSON path and expected value. Example:

```text
config error at $.resources.memory_budget_percent: value -1 is out of range; expected auto or integer 10-95
```

The same failure is written as a JSON `config_invalid` event in the command log. For daemon/API processes, invalid config pauses or fails request handling safely until the file is fixed.

For memory/resource issues, inspect the automatic caps:

```sh
mlai-trade data db-stats
```

This prints the detected memory source. macOS uses `sysctl`, Linux uses cgroup limits or `/proc/meminfo`, FreeBSD uses `sysctl`, and other Unix targets use `sysconf`.

Platform support target is macOS, Linux, and FreeBSD. Native builds on each OS are expected to work with the normal Rust/C toolchain. Cross-checking Linux or FreeBSD from macOS also requires the target C compiler/sysroot because dependencies such as `ring` compile C code.

Ubuntu Linux validation can be run in a container:

```sh
scripts/linux-ubuntu-test.sh
```

The script uses Docker or Podman, builds `docker/ubuntu-test/Dockerfile`,
mounts the repo read-only, copies a `.dockerignore`-filtered tree inside the
container, and runs:

```sh
cargo fmt --check
cargo check --no-default-features
cargo test --no-default-features
cargo build --release --no-default-features
```

FreeBSD is intentionally not container-tested because normal containers do not
provide a FreeBSD kernel.

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

Logs are JSON lines and rotate daily. Current files keep the stable names above; old logs are compressed as `YYYYMMDD-<log-file>.gz`.

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
jq 'select(.event == "command_failed")' ~/mlai-trade/logs/mlai-trade-ml.log
jq 'select(.event == "api_request" and .status >= 400)' ~/mlai-trade/logs/mlai-trade-api.log
```

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
mlai-trade compliance tax --year 2026 --account paper-main --details
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
