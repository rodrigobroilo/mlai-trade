# mlai-trade

ML/AI trading CLI with broker modules, shared ML pipelines, compliance guardrails, and configurable local runtime storage.

Default runtime home:

```sh
~/mlai-trade
```

Runtime layout:

- `bin/`: installed local binaries and helper executables
- `config/`: local configuration and secrets, never committed
- `data/`: generated ML datasets, models, reports, and market research artifacts
- `db/`: SQLite databases for trades, market data, compliance state, predictions, and scanner state
- `docs/`: local documentation copies

The CLI creates these folders automatically on startup. Override the runtime home with:

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
mlai-trade daemon reload
mlai-trade daemon restart
mlai-trade daemon stop
```

The Unix-socket API has its own lifecycle and refuses to start unless `api.enabled=true`:

```sh
mlai-trade api start
mlai-trade api status
mlai-trade api test
mlai-trade api reload
mlai-trade api restart
mlai-trade api stop
```

Full API routes, request shapes, and curl examples are documented in `docs/API.md`.

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
```

`ml refresh` is incremental and fills missing data/artifacts. It reconciles/syncs the managed feed universe before training, then uses dated feed aggregates as ML features. `ml full-refresh` forces a rebuild of market data, features, labels, models, predictions, and ensemble output.

Optional shell autocomplete scripts:

```sh
mlai-trade runtime completions install zsh
mlai-trade runtime completions uninstall zsh
mlai-trade runtime completions generate zsh
```
