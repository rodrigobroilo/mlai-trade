# mlai-trade

ML/AI trading CLI with broker modules, shared ML pipelines, compliance guardrails, and configurable local runtime storage.

Default runtime home:

```sh
~/mlai-trade
```

Runtime layout:

- `api/`: Unix-socket API runtime files, including `mlai-trade-api.sock`
- `bin/`: installed local binaries and helper executables
- `config/`: local configuration and secrets, never committed
- `data/`: generated ML datasets, models, reports, and market research artifacts
- `db/`: SQLite databases for trades, market data, compliance state, predictions, and scanner state
- `docs/`: local documentation copies
- `logs/`: JSON-line application logs and compressed rotated logs
- `tmp/`: PID files, daily refresh stamps, and other transient runtime state

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

The repository only tracks `config/mlai-trade.example.json`; real keys stay in the local runtime config file.

Alpaca is the implemented provider today. The config supports multiple Alpaca accounts, with paper and real-money compliance state separated. Real accounts share taxpayer-wide compliance blockers across accounts; paper accounts obey the same rules in a separate simulation universe.

Auto-trade uses provider calendar/clock checks plus local exchange-time guardrails. Event timestamps are stored in UTC, with the provider/exchange timezone and session source stored alongside trade records.

Daemon mode can run the automatic auto-trade loop and tax estimate refresh without cron:

```sh
mlai-trade daemon start
mlai-trade daemon status
mlai-trade daemon status --details
mlai-trade daemon reload
mlai-trade daemon restart
mlai-trade daemon stop
```

The Unix-socket API has its own lifecycle and refuses to start unless `api.enabled=true`:

```sh
mlai-trade api start
mlai-trade api status
mlai-trade api status --details
mlai-trade api test
mlai-trade api reload
mlai-trade api restart
mlai-trade api stop
```

Full API routes, request shapes, and curl examples are documented in `docs/API.md`.
The API is local Unix-socket only and includes explicit overload protection with rate, concurrency, long-operation, and body-size limits configured under `api`.
API command output is redacted for configured Alpaca and FRED secrets before it is returned or logged.

Documentation map:

- `docs/README.md`: documentation index and first commands.
- `docs/USAGE.md`: operator guide and command reference.
- `docs/CONFIGURATION.md`: config file reference.
- `docs/API.md`: Unix-socket API reference.
- `docs/DEBUGGING.md`: troubleshooting and JSONL log inspection.
- `docs/IRS_TAX_RULES.md`: tax/compliance reference.
- `docs/TRADING_KNOWLEDGE.md`: Alpaca/trading API and strategy notes.
- `CHANGELOG.md`: user-facing changes by release.

Federal tax estimates are available through:

```sh
mlai-trade compliance tax --accounts
mlai-trade compliance tax --show-brackets --year 2026
mlai-trade compliance tax --year 2026
mlai-trade compliance tax --year 2026 --account paper-main --details
mlai-trade compliance tax --year 2026 --quarter 1,2 --export csv
```

Tax bracket/rate data is read from `~/mlai-trade/config/tax-brackets.json`.
Start from `config/tax-brackets.example.json` and add future IRS years as JSON diffs.

First ML setup or repair:

```sh
mlai-trade ml refresh
mlai-trade data daily
```

`ml refresh` and `data daily` share the same full incremental non-trading prep path by default. They fill missing data/artifacts, reconcile/sync the managed feed universe before training, use dated feed aggregates as ML features, train/evaluate all configured models, refresh predictions/ensemble output, and cache default SHAP explanations. `data daily --skip-train` is the data-only exception. `ml full-refresh` forces a rebuild of market data, features, labels, models, predictions, and ensemble output.

Large DBs are expected when full-history bars and wide ML features are enabled. Runtime resources are controlled automatically by the `resources` config section. By default, mlai-trade detects usable RAM on macOS, Linux, FreeBSD, or generic Unix, sizes SQLite/ML limits from an 80% memory budget, and caps CPU-bound ML worker threads to 80% of logical CPUs. GPU/NPU paths are not CPU-capped. Use `mlai-trade data db-stats` to inspect table sizes, detected memory source, and active resource caps.

Config is validated before commands run. Invalid keys or values fail with a precise JSON path and expected values, for example `$.resources.memory_budget_percent` must be `auto` or an integer from `10` to `95`, and `$.resources.cpu_budget_percent` must be `auto` or an integer from `10` to `100`.

Optional shell autocomplete scripts:

```sh
mlai-trade runtime completions install zsh
mlai-trade runtime completions uninstall zsh
mlai-trade runtime completions generate zsh
```
