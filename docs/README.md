# mlai-trade Documentation

`mlai-trade` is a Rust CLI for local, provider-backed market data, ML model preparation, compliance tracking, optional auto-trade execution, and a local Unix-socket JSON API.

This documentation set is copied into the runtime docs folder at:

```text
~/mlai-trade/docs/
```

## Documents

| File | Purpose |
| --- | --- |
| `USAGE.md` | Main operator guide: command topics, daily prep, ML, feeds, daemon, API, tax, runtime files. |
| `CONFIGURATION.md` | Full config explanation for providers, daemon, API, logs, feeds, tax, market clock, compliance, and ML backends. |
| `API.md` | Unix-socket API lifecycle, allowlist, routes, request parameters, response wrapper, and curl examples. |
| `DEBUGGING.md` | Troubleshooting commands and JSONL log inspection recipes. |
| `IRS_TAX_RULES.md` | Tax/compliance reference used to shape the guardrails and tax estimator. |
| `TRADING_KNOWLEDGE.md` | Alpaca/trading API notes, data feed behavior, strategy evidence, and implementation decisions. |
| `THIRD_PARTY_LICENSES.md` | Third-party licensing notes. |
| `THIRD_PARTY_CRATES.tsv` | Crate-by-crate license inventory. |
| `LICENSE.md` | Project license and disclaimer. |
| `CHANGELOG.md` | User-facing changes by release. |

## First Commands

```sh
mlai-trade runtime version
mlai-trade data daily
mlai-trade ml refresh
mlai-trade api status --json
mlai-trade api status --details
mlai-trade daemon status --json
mlai-trade daemon status --details
```

`data daily` and `ml refresh` are non-trading preparation commands. They do not buy or sell. By default they use the same full incremental pipeline: refresh data, reconcile/sync feeds, compute features/labels, train/evaluate models, refresh predictions/ensemble output, cache default SHAP explanations, and make ML artifacts ready for auto-trade decisions. `data daily --skip-train` is the data-only exception.

For large SQLite databases, memory and CPU caps are automatic by default. The CLI detects usable RAM on macOS, Linux, FreeBSD, or generic Unix, derives SQLite and ML limits from `resources.memory_budget_percent`, and caps Tokio async workers plus CPU-bound worker threads from `resources.cpu_budget_percent` across all logical CPU capacity. GPU/NPU backends are not CPU-capped. Inspect size, detected memory source, and active caps with:

```sh
mlai-trade data db-stats
```

For Linux-path validation, run:

```sh
scripts/linux-ubuntu-test.sh
```

On Linux this runs natively. On macOS, FreeBSD, or another non-Linux host, it
runs inside an Ubuntu 24.04 Docker container. On macOS it can install Docker
CLI + Colima with Homebrew and start Colima in the background. The Ubuntu image
is cached locally as `mlai-trade:ubuntu-test`; normal runs reuse it offline when
the Dockerfile fingerprint matches. Use `scripts/linux-ubuntu-test.sh update`
only when you want to pull/rebuild the image. Use
`scripts/linux-ubuntu-test.sh container` to keep a container open, then inspect
it with `docker ps` and `docker exec -it mlai-trade-ubuntu-test bash`.

`api status --details` and `daemon status --details` show live RSS, configured
memory budget, process CPU capacity, worker caps, and MLX/tch accelerator
availability. Accelerator paths are marked uncapped only when available to the
running binary and platform.

Config is validated before commands run. Unknown keys, wrong types, out-of-range numbers, and unsupported enum values report the exact JSON path and expected values.

## Automatic Daily Prep

When enabled, the daemon performs daily non-trading prep automatically. The default trigger is `market_close`: once per open market-local date, one hour after `auto.market.regular_close`. The daemon uses the incremental `ml refresh` path, including provider order sync, feed reconciliation, feed sync, feed-derived ML features, model training/evaluation, predictions, ensemble refresh, SHAP cache, and tax refresh.

Relevant config keys:

- `daemon.enabled`
- `daemon.daily_refresh_enabled`
- `daemon.daily_refresh_trigger`
- `daemon.daily_refresh_after_close_minutes`
- `daemon.daily_refresh_timezone`
- `daemon.daily_refresh_sync_orders`
- `daemon.daily_refresh_feeds_sync`
- `feeds.sync_before_training`

The daemon auto-trade loop and the daily prep job are independent. If the market is closed, the daemon can back off trading cycles for that market date while still running daily data/ML/tax maintenance.

The success stamp is `~/mlai-trade/tmp/mlai-trade-daily-refresh.stamp`. If the daemon misses a day, the next incremental refresh fills missing data ranges rather than requiring manual intervention.

## Logs

All application logs are JSON lines under `~/mlai-trade/logs/` by default:

- `mlai-trade-daemon.log`
- `mlai-trade-api.log`
- `mlai-trade-auto.log`
- `mlai-trade-data.log`
- `mlai-trade-ml.log`
- `mlai-trade-training.log`
- `mlai-trade-feeds.log`

Use `jq` for live inspection:

```sh
tail -f ~/mlai-trade/logs/mlai-trade-daemon.log | jq -c .
tail -f ~/mlai-trade/logs/mlai-trade-training.log | jq -c .
```

## Runtime Security

The CLI hardens runtime file permissions each time it initializes the runtime tree. Sensitive directories under `~/mlai-trade` are `0700`: `config/`, `data/`, `db/`, `logs/`, `api/`, and `tmp/`. Sensitive files inside them are `0600`, including local configs, generated ML datasets/models/reports, SQLite DBs, logs, and the API socket. PID files are runtime metadata and use `0644`.

Blank or relative runtime path overrides are resolved inside their expected runtime folder, so a relative log setting cannot write into the caller's current directory. API and daemon-captured command output is redacted for configured Alpaca and FRED secrets before it is logged or returned.

## API Backpressure

The local Unix-socket API has explicit overload protection. Configure it with `api.max_concurrent_requests`, `api.max_concurrent_long_requests`, `api.rate_limit_per_minute`, `api.max_body_bytes`, and `api.overload_retry_after_seconds`. When limits are exceeded, API responses use HTTP `429` with `ok:false`, `reason`, `retry_after_seconds`, and a `Retry-After` header.
