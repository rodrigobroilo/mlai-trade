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
mlai-trade daemon status --json
```

`data daily` and `ml refresh` are non-trading preparation commands. They do not buy or sell. By default they use the same full incremental pipeline: refresh data, reconcile/sync feeds, compute features/labels, train/evaluate models, refresh predictions/ensemble output, cache default SHAP explanations, and make ML artifacts ready for auto-trade decisions. `data daily --skip-train` is the data-only exception.

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
