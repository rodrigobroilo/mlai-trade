# Configuration

`mlai-trade` reads local runtime configuration from:

```text
~/mlai-trade/config/mlai-trade.json
```

The runtime home defaults to `~/mlai-trade`. You can override it for a run:

```sh
mlai-trade --home /path/to/mlai-trade runtime version
```

or by setting `MLAI_TRADE_HOME` in the process environment before launch.

The CLI creates these folders automatically:

- `bin/`
- `config/`
- `data/`
- `db/`
- `docs/`
- `logs/`
- `api/`
- `tmp/`

PID files are runtime control files and default to `tmp/` inside the runtime home.

Real credentials must stay in the local runtime config file. Do not commit them. The repository tracks only `config/mlai-trade.example.json` with placeholder values.

Provider enablement is explicit. At least one provider must be enabled or the CLI exits:

```json
{
  "providers": {
    "alpaca": { "enabled": true },
    "other": {}
  }
}
```

Future providers can be added under `providers.other` without changing the config shape.

## Required Example

The authoritative template is:

```text
config/mlai-trade.example.json
```

It intentionally lists every supported configuration key. The runtime file should keep the same shape and replace only local values such as API keys, account names, and enabled flags.

## Providers And Accounts

`providers.alpaca.enabled` enables the Alpaca provider module. `providers.other` is a generic enablement map reserved for future provider modules.

`alpaca.accounts[]` can contain multiple accounts. Each account has:

- `name`: stable account reference stored in DB rows.
- `enabled`: include or skip the account.
- `account_mode`: `paper` or `individual`.
- `data_feed`: `auto`, `sip`, or `iex`.
- `api_key_id` and `secret_key`: local credentials, never committed.

Paper and real accounts are separate execution universes. Real accounts share real-money compliance blockers across accounts. Paper accounts obey the same rules in a separate paper compliance universe.

## Daemon

`daemon.enabled` controls whether `mlai-trade daemon start` is allowed. The daemon loop runs `auto run`, tax-estimate refresh, log rotation, and an optional once-per-day maintenance refresh.

- `enabled`: `true` or `false`.
- `auto_trade_interval_seconds`: default `60`, clamped to `10`-`300`.
- `daily_refresh_enabled`: default `true`; runs the non-trading daily maintenance path once per exchange date after the configured time.
- `daily_refresh_time`: default `18:30:00`.
- `daily_refresh_timezone`: default `America/New_York`.
- `daily_refresh_days`: default `0`, meaning gap-aware full available history discovery on first run and incremental missing/latest-day refresh after that.
- `daily_refresh_quick`: default `false`; set `true` only for faster validation runs.
- `daily_refresh_walk_forward_folds`: default `5`.
- `daily_refresh_top_n`: default `20`.
- `daily_refresh_slippage_bps`: default `50`.
- `daily_refresh_sync_orders`: default `true`; syncs provider orders/fills before daily ML maintenance.
- `daily_refresh_feeds_sync`: default `true`; performs an extra subscribed feed sync after ML maintenance. The ML refresh itself also syncs the managed feed universe before training when `feeds.sync_before_training=true`.
- `daily_refresh_feeds_days`: default `7`.
- `pid_file`: optional override; blank means `tmp/mlai-trade.pid`.
- `log_file`: optional override; blank means `logs/mlai-trade-daemon.log`.

The daily maintenance order is: rotate logs, sync provider orders, run `ml refresh` (which reconciles/syncs feeds before training), optionally sync feeds again, refresh tax estimates, then write `tmp/mlai-trade-daily-refresh.stamp`. If a step fails, the daemon logs the failure and retries after roughly one hour.

Lifecycle commands:

```sh
mlai-trade daemon start
mlai-trade daemon status
mlai-trade daemon reload
mlai-trade daemon restart
mlai-trade daemon stop
```

## API

`api.enabled` controls whether `mlai-trade api start` is allowed. The API listens on a local Unix socket and exposes only an explicit allowlist of CLI actions as JSON responses.

- `enabled`: `true` or `false`.
- `socket_file`: optional override; blank means `api/mlai-trade-api.sock`. The socket file is created with `0600` permissions.
- `pid_file`: optional override; blank means `tmp/mlai-trade-api.pid`.
- `log_file`: optional override; blank means `logs/mlai-trade-api.log`.
- `request_timeout_seconds`: default `60`, clamped to `5`-`300`, used for normal API calls.
- `long_request_timeout_seconds`: default `3600`, clamped to `60`-`86400`, used for `ml refresh` and `feeds sync`.

Lifecycle and health commands:

```sh
mlai-trade api start
mlai-trade api status
mlai-trade api test
mlai-trade api reload
mlai-trade api restart
mlai-trade api stop
```

`api test` sends `GET /health` through the configured Unix socket. `api status --json` lists the allowlisted sections and actions. Runtime commands are not exposed through the API. Trade mutation endpoints (`buy`, `sell`, `cancel`, `close`) are rejected while auto-trading is enabled.

The full API route list, request parameters, response wrapper, and curl examples are documented in `docs/API.md`.

## Logs

Active logs are written under `logs/` by default:

- `mlai-trade-daemon.log`
- `mlai-trade-api.log`
- `mlai-trade-auto.log`

Logs rotate daily. The active file keeps the stable name, and the previous day's content is gzip-compressed as `YYYYMMDD-<log-file>.gz`, for example `20260502-mlai-trade-auto.log.gz`.

## Feeds

`feeds` controls news/filing feed collection and feed-derived ML features:

- `sync_before_training`: default `true`; `ml refresh` and `data daily` reconcile/sync feeds before feature computation and training.
- `sync_orders_before_training`: default `true`; syncs provider orders/fills first so bought symbols are current.
- `include_current_sp500`: default `true`; current S&P 500 symbols seed the feed universe only.
- `include_open_positions`: default `true`; provider and auto-trade open positions are included.
- `include_bought_symbols`: default `true`; recent provider buys are included.
- `bought_symbol_lookback_days`: default `365`.
- `include_q1_candidates`: default `true`; latest Q1 ML candidates are included.
- `q1_top_n`: default `500`.
- `sync_days`: default `30`; feed sources are queried for this recent window.
- `extra_symbols`: config-managed extra symbols that should always be included.

Managed feed subscriptions are reconciled every run. Symbols no longer needed by S&P 500/current positions/recent buys/Q1/config are removed from the managed subscription list. Manual subscriptions added with `mlai-trade feeds add` are not removed by reconciliation.

Current S&P 500 membership is intentionally not a model feature because that would introduce survivorship bias without point-in-time membership data. The model receives only symbol/date feed aggregates such as sentiment windows, article counts, 8-K counts, Form 4 counts, and negative-news counts.

## Tax

`tax` contains the inputs for the federal estimate:

- `filing_status`: `single`, `married_filing_jointly`, `married_filing_separately`, or `head_of_household`.
- `estimated_annual_income`: estimated annual taxable ordinary income before trading gains. The example default is `1000000.0`.
- `include_paper_accounts_for_estimate`: defaults to `false`.
- `brackets_file`: JSON file under `config/` containing ordinary income, regular long-term capital gains, and Net Investment Income Tax rates and thresholds. The default is `tax-brackets.json`.

The example default filing status is `married_filing_jointly`.

Tax brackets and percentages are data, not code. Copy `config/tax-brackets.example.json` to `~/mlai-trade/config/tax-brackets.json`. When IRS publishes a new year, add that year to the JSON file and review the diff.

`mlai-trade compliance tax --accounts` lists tax-visible account selectors. `mlai-trade compliance tax --show-brackets --year YYYY` lists the configured filing-status brackets for ordinary/short-term gains, long-term capital gains, and Net Investment Income Tax. `mlai-trade compliance tax --year YYYY` calculates the year-to-date/current-year estimate for all real accounts by default. Add `--account SELECTOR` to select one or more accounts, including paper accounts for simulation, and add `--details` to list estimated tax impact per matched operation. Add `--quarter 1`, `--quarter 1,2`, or `--quarter 1-4` to select one contiguous quarter period. Estimates are persisted in `db/tax.db`. `--export csv` writes `data/tax_YYYY_<period>.csv`.

## Market Calendar And Clock

`auto.market` controls when auto-trade may run:

- `mode=auto`: Alpaca v3 calendar + Alpaca v3 clock + local schedule.
- `mode=provider`: provider calendar/clock only unless local checks are explicitly enabled.
- `mode=local`: local configured schedule only.
- `timezone`: defaults to `America/New_York`; this controls local exchange-hour guardrails and is stored with trade records.
- `provider_markets`: defaults to `["NYSE", "NASDAQ"]`.
- `regular_open` / `regular_close`: local regular-session guardrail.
- `buy_start` / `buy_end`: local buy window.
- `sell_start` / `sell_end`: local sell window.
- `closed_dates`: local override dates, `YYYY-MM-DD`.

Alpaca v3 calendar is queried in UTC. The code stores UTC timestamps for events and stores the configured/provider market timezone/session context alongside trade records.

Manual verification:

```sh
mlai-trade market clock
mlai-trade market calendar
mlai-trade market calendar --market NYSE --market NASDAQ
```

## Compliance

Legal and regulatory floors are compiled into code. Config can make behavior stricter, not weaker.

`auto.compliance.wash_sale_safety_buffer_days` defaults to `1`. The IRS wash-sale replacement window is hardcoded at 30 days, so the default forward block is 31 days after a loss sale. Setting the buffer below 1 is rejected or clamped by the code path.

`auto.compliance.blocked_symbols` is a user/company policy list. It supports multiple symbols and normalizes input to uppercase before comparison, so `meta`, `Meta`, and `META` all block market symbol `META`.

Dollar thresholds from tax/regulatory sources are not user-tunable downward. If a future config exposes a dollar safety buffer, the effective threshold must be the hardcoded floor plus the user buffer.

`auto.log_file` optionally overrides the auto-trade audit log path. Blank means `logs/mlai-trade-auto.log`. Entries are JSON lines and include `source` (`daemon`, `cli`, or `api`), cycle status, per-account results, buys, sells, skipped reasons, market-closed decisions, provider sync summaries, and errors.

## Provider Order Sync

`mlai-trade auto sync-orders` is a read-only provider sync. For Alpaca accounts, it stores provider order snapshots in `provider_order_snapshots` and fill activities in `provider_fill_activities` inside `db/mlai_trade.db`.

The first sync starts at the oldest provider history available. Later syncs rewind the latest local provider timestamp by one day, refresh that day, and fill forward. Auto-trade runs sync before account decisions and sync again after confirmed provider orders.

## ML Backends

`backend.lstm` supports `auto`, `cpu`, `mlx`, or `tch`.

- `auto`: choose the best available backend for the platform.
- `mlx`: Apple Silicon MLX path when compiled and available.
- `tch`: Linux/NVIDIA CUDA path when compiled and available.
- `cpu`: portable fallback.

`backend.xgboost` supports `auto`, `cpu`, or `cuda` when XGBoost support is compiled in. `backend.lightgbm` and `backend.ridge` are CPU-only in the current Rust implementation and should remain `cpu`.

## Autocomplete

Autocomplete is optional. The CLI works without it. Install or remove completion scripts with:

```sh
mlai-trade runtime completions install zsh
mlai-trade runtime completions uninstall zsh
mlai-trade runtime completions install bash
mlai-trade runtime completions install fish
```

Use `mlai-trade runtime completions generate zsh` when you only want to print the script to stdout.
