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

For market-hours problems, verify the configured provider timezone and Alpaca v3 sessions:

```sh
mlai-trade market clock
mlai-trade market calendar
```

For daemon problems:

```sh
mlai-trade data status
tail -f ~/mlai-trade/logs/mlai-trade-daemon.log
tail -f ~/mlai-trade/logs/mlai-trade-auto.log
```

Logs rotate daily. Current files keep the stable names above; old logs are compressed as `YYYYMMDD-<log-file>.gz`.

If `mlai-trade daemon start` refuses to run, set `daemon.enabled=true` in the local config. The interval is clamped to 10-300 seconds.

If daily daemon maintenance did not run, check `daemon.daily_refresh_enabled`, `daemon.daily_refresh_time`, `daemon.daily_refresh_timezone`, and the last success stamp:

```sh
cat ~/mlai-trade/tmp/mlai-trade-daily-refresh.stamp
tail -f ~/mlai-trade/logs/mlai-trade-daemon.log
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

If provider order/fill tables are empty, run the read-only provider sync:

```sh
mlai-trade auto sync-orders
```

The Git repo ignores generated DBs, datasets, models, reports, local config, and secrets. Use `git status --ignored` when in doubt.
