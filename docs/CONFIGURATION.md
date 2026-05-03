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

## Runtime Security

The CLI enforces private runtime permissions on startup and when sensitive files are created:

| Runtime Path | Mode | Notes |
| --- | --- | --- |
| `~/mlai-trade` | `0700` | Runtime home is private to the OS user. |
| `config/`, `data/`, `db/`, `logs/`, `api/`, `tmp/` | `0700` | Sensitive runtime directories. |
| `config/mlai-trade.example.json`, `config/mlai-trade.json`, `config/tax-brackets*.json` | `0600` | Config files and examples are private in the runtime copy. |
| `db/*` | `0600` | SQLite DBs and related files are private. |
| `data/*` | `0600` | Generated datasets, models, reports, and CSV exports are private. |
| `logs/*`, `api/mlai-trade-api.sock` | `0600` | Runtime audit files and the local API socket are private. |
| `tmp/*.pid` | `0644` | PID files are runtime metadata. |

Blank or relative path overrides for logs, sockets, PID files, and tax brackets resolve inside the expected runtime folder (`logs/`, `api/`, `tmp/`, or `config/`). This prevents accidental writes into the caller's current directory. API and daemon-captured command output is redacted for configured Alpaca and FRED secrets before it is logged or returned.

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

`daemon.enabled` controls whether `mlai-trade daemon start` is allowed. The daemon loop runs auto-trade cycles, tax-estimate refresh, log rotation, and an optional once-per-day maintenance refresh.

| Key | Default | What It Does |
| --- | --- | --- |
| `enabled` | `false` | Allows or refuses daemon lifecycle commands. `mlai-trade daemon start` exits with an error when this is `false`. |
| `auto_trade_interval_seconds` | `60` | How often the daemon checks provider accounts for auto-trade decisions. The value is clamped to `10`-`300`. |
| `daily_refresh_enabled` | `true` | Enables the daemon's once-per-market-date non-trading prep job. This job never buys or sells. |
| `daily_refresh_trigger` | `market_close` | Chooses when the daily prep job becomes eligible. `market_close` is market-aware. `time` uses the fixed `daily_refresh_time` clock. |
| `daily_refresh_after_close_minutes` | `60` | With `market_close`, waits this many minutes after `auto.market.regular_close` before running daily prep. The value is clamped to `0`-`360`. |
| `daily_refresh_time` | `18:30:00` | With `daily_refresh_trigger=time`, runs after this local time. This mode is a raw clock fallback and does not use the market-close trigger. |
| `daily_refresh_timezone` | `America/New_York` | Timezone used to calculate the market-local date and trigger time. |
| `daily_refresh_days` | `0` | Passed to `ml refresh --days`. `0` means first-run full available history discovery and later incremental missing/latest-day refresh. |
| `daily_refresh_quick` | `false` | Adds `--quick` to the daemon-run `ml refresh`; intended for validation runs, not production prep. |
| `daily_refresh_walk_forward_folds` | `5` | Passed to `ml refresh --walk-forward-folds`. |
| `daily_refresh_top_n` | `20` | Passed to `ml refresh --top-n` for trading-metric evaluation. |
| `daily_refresh_slippage_bps` | `50` | Passed to `ml refresh --slippage-bps` so validation uses post-slippage metrics. |
| `daily_refresh_sync_orders` | `true` | Runs `mlai-trade auto sync-orders` before ML prep so provider orders/fills and bought-symbol feeds are current. |
| `daily_refresh_feeds_sync` | `true` | Runs an extra `mlai-trade feeds sync` after ML prep. The ML refresh itself still syncs feeds before training when `feeds.sync_before_training=true`. |
| `daily_refresh_feeds_days` | `7` | Number of recent days requested by the extra post-refresh `feeds sync`. |
| `pid_file` | blank | Optional override. Blank means `tmp/mlai-trade-daemon.pid`. |
| `log_file` | blank | Optional override. Blank means `logs/mlai-trade-daemon.log`. |

When `daemon.daily_refresh_enabled=true` and `daemon.daily_refresh_trigger=market_close`, the daemon checks the configured market-local clock on every daemon loop. It runs daily prep only when all of these are true:

- The daemon is running and daemon mode is enabled.
- The current market-local date is not Saturday or Sunday.
- The current market-local date is not listed in `auto.market.closed_dates`.
- The current time is at least `auto.market.regular_close + daemon.daily_refresh_after_close_minutes`.
- `tmp/mlai-trade-daily-refresh.stamp` does not already contain the current market-local date.

That means the default behavior is: run once per open New York market date, about one hour after the regular close (`16:00:00 + 60 minutes = 17:00:00 America/New_York`). After a successful run, the stamp file prevents another daily prep run for the same market-local date.

The daily maintenance order is:

1. Rotate/sanitize JSON logs.
2. Sync provider orders/fills when `daemon.daily_refresh_sync_orders=true`.
3. Run `ml refresh` with `--days`, `--backend auto`, walk-forward folds, top-N, slippage, and optional `--quick`. This is the same shared full incremental pipeline used by `data daily` when `--skip-train` is not set.
4. Inside `ml refresh`, refresh the universe, FRED data, Alpaca bars, managed feed universe, feed sync, features, labels, model training, validation, predictions, ensemble, and SHAP cache.
5. Run an extra subscribed feed sync when `daemon.daily_refresh_feeds_sync=true`.
6. Refresh current-year tax estimates.
7. Write `tmp/mlai-trade-daily-refresh.stamp`.

If any step fails, the daemon logs a JSON `daily_maintenance_failed` event and retries later instead of writing the success stamp.

When every enabled account reports `market_closed`, the daemon logs `auto_market_closed_backoff_started` and suppresses further daemon-driven auto-trade cycles until the next configured market date. Tax refresh and daily maintenance are separate jobs and are not disabled by this trading backoff.

`daemon status --details` reads the daemon heartbeat from `tmp/mlai-trade-daemon-status.json` and reports loop count, last auto-trade summary, last daily-refresh summary, CPU time, RSS memory, open file descriptor count, and thread count when available. Metrics that are not available on a platform are reported as `not available`.

Lifecycle commands:

```sh
mlai-trade daemon start
mlai-trade daemon status
mlai-trade daemon status --details
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
- `max_concurrent_requests`: default `8`, clamped to `1`-`128`; maximum command requests running at the same time.
- `max_concurrent_long_requests`: default `1`, clamped to `1`-`16`; maximum long operations such as `ml refresh` or `feeds sync` running at the same time.
- `rate_limit_per_minute`: default `120`, clamped to `1`-`10000`; process-local API request budget per rolling minute.
- `max_body_bytes`: default `65536`, clamped to `1024`-`1048576`; oversized bodies are rejected with HTTP `413`.
- `overload_retry_after_seconds`: default `5`, clamped to `1`-`300`; retry hint returned when concurrency is exhausted.

Lifecycle and health commands:

```sh
mlai-trade api start
mlai-trade api status
mlai-trade api status --details
mlai-trade api test
mlai-trade api reload
mlai-trade api restart
mlai-trade api stop
```

`api test` sends `GET /health` through the configured Unix socket. `api status --json` lists the allowlisted sections and actions. `api status --details` asks the API process for live counters and resource usage over the Unix socket. Runtime commands are not exposed through the API. Trade mutation endpoints (`buy`, `sell`, `cancel`, `close`) are rejected while auto-trading is enabled.

The full API route list, request parameters, response wrapper, and curl examples are documented in `docs/API.md`.

API errors are explicit. If an underlying CLI command returns JSON with `ok:false`, the API wrapper returns `ok:false` with a non-2xx status. Command JSON can include `status_code`/`http_status` to request a specific error status such as `404`.

API overload protection is explicit. If request rate or concurrency is exhausted, the API returns HTTP `429` with `ok:false`, `reason`, and `retry_after_seconds`; clients should back off instead of immediately retrying. This is backpressure only; the API does not cache responses. CLI stdout/stderr captured by the API is redacted for configured Alpaca and FRED secrets before it is returned or logged.

## Logs

Active logs are written under `logs/` by default:

- `mlai-trade-daemon.log`
- `mlai-trade-api.log`
- `mlai-trade-auto.log`
- `mlai-trade-data.log`
- `mlai-trade-ml.log`
- `mlai-trade-training.log`
- `mlai-trade-feeds.log`

All application logs are JSON lines. Logs rotate daily. The active file keeps the stable name, and the previous day's content is gzip-compressed as `YYYYMMDD-<log-file>.gz`, for example `20260502-mlai-trade-auto.log.gz`.

The optional `logging` config section can override component log paths. Blank or relative values resolve under `logs/`; absolute paths outside the runtime logs directory are reduced to their filename under `logs/` so application logs stay in one private folder:

- `data_log_file`
- `ml_log_file`
- `training_log_file`
- `feeds_log_file`

Default component logs:

| Config Key | Default |
| --- | --- |
| `logging.data_log_file` | `logs/mlai-trade-data.log` |
| `logging.ml_log_file` | `logs/mlai-trade-ml.log` |
| `logging.training_log_file` | `logs/mlai-trade-training.log` |
| `logging.feeds_log_file` | `logs/mlai-trade-feeds.log` |

Command lifecycle records are written to these component logs for data, feeds, ML, and training commands. Each record is one JSON object per line with `event`, `component`, `command`, `source`, `duration_ms`, and error fields when applicable.

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

### Feed Reconciliation

`mlai-trade data daily` and `mlai-trade ml refresh` both run feed reconciliation when `feeds.sync_before_training=true`. The daemon daily job runs `ml refresh`, so daemon daily prep also uses the same feed reconciliation path.

Feed reconciliation rebuilds the desired managed feed universe each run:

| Source | Config Key | Effect |
| --- | --- | --- |
| Current S&P 500 list | `feeds.include_current_sp500` | Adds the current S&P 500 symbols to the managed feed universe for data collection only. |
| Open auto positions and provider positions | `feeds.include_open_positions` | Keeps symbols currently held by any enabled account in the feed universe. |
| Recent provider buy fills | `feeds.include_bought_symbols` and `feeds.bought_symbol_lookback_days` | Keeps recently bought symbols in the feed universe. |
| Latest Q1 ML candidates | `feeds.include_q1_candidates` and `feeds.q1_top_n` | Adds the top latest predicted-quintile-1 candidates to the feed universe. |
| Config extras | `feeds.extra_symbols` | Always keeps these symbols in the managed feed universe. |

For each desired symbol, the reconciler upserts `feed_subscriptions` with `managed=1`, updates `subscription_source`, preserves any existing CIK, and fills missing CIK values when SEC lookup has one. Existing managed symbols that are no longer desired are removed. Existing manual subscriptions created with `mlai-trade feeds add` have `managed=0`; they are kept unless the user removes them with `mlai-trade feeds remove SYMBOL`.

After reconciliation, feed sync pulls recent Alpaca news, SEC EDGAR filings, Yahoo RSS, and Google RSS for every subscribed symbol. Those articles/filings are converted into dated feed aggregates and included in ML feature computation before training.

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

## Resources

`resources` controls memory, CPU worker threads, and SQLite behavior so the application can run on small machines even when `db/mlai_trade.db` is many GB. Defaults are automatic and should not require user tuning:

- `memory_budget_percent`: percent of detected usable RAM used to derive auto caps. Default `80`, valid range `10`-`95`.
- `cpu_budget_percent`: percent of logical CPUs used for CPU-bound ML work. Default `80`, valid range `10`-`100`. CPU-bound LightGBM, CPU XGBoost, and CPU/Rayon LSTM use this cap. GPU/NPU backends (`mlx`, `tch`, XGBoost CUDA) are intentionally uncapped.
- `sqlite_cache_mb`: `auto` or a per-connection SQLite page cache in MB. Auto derives a bounded value from the memory budget.
- `sqlite_temp_store`: `auto`, `file`, or `memory`. Auto uses `file` so large sorts/temp tables do not consume RAM.
- `sqlite_mmap_mb`: `auto` or a SQLite mmap limit in MB. Auto enables mmap only when enough RAM is detected.
- `ml_symbol_batch_size`: `auto` or feature/label symbol batch size.
- `lstm_max_sequences`: `auto` or maximum materialized LSTM training windows sampled across all eligible symbols/dates.
- `lstm_batch_size`: `auto` or LSTM training mini-batch size.
- `lightgbm_max_train_rows`: `auto`, `0`/`unlimited`, or maximum native LightGBM train rows.
- `lightgbm_max_valid_rows`: `auto`, `0`/`unlimited`, or maximum native LightGBM validation rows.

Memory detection uses macOS `sysctl hw.memsize`, Linux cgroup limits when smaller than host RAM, Linux `/proc/meminfo`, FreeBSD `sysctl`, and then generic Unix `sysconf` as a fallback. CPU detection uses Rust's platform `available_parallelism`. `data db-stats` prints the detected source and final derived caps.

The full market database is not loaded into RAM. SQLite rows are streamed for features, labels, exports, and LightGBM text generation. The caps above bound the places that must materialize ML training data in process memory or native ML libraries.

Config validation runs before commands execute. Unknown keys, wrong types, out-of-range numbers, and unsupported enum values fail with a precise JSON path and expected values. Example: `$.resources.memory_budget_percent` must be `auto` or an integer from `10` to `95`.

Inspect DB size and largest SQLite objects:

```sh
mlai-trade data db-stats
```

Run safe SQLite maintenance:

```sh
mlai-trade data db-optimize
```

`mlai-trade data db-optimize --vacuum` rewrites the SQLite file to reclaim free pages. Use it only when you intentionally want a long-running DB rewrite and have enough free disk space.

## Autocomplete

Autocomplete is optional. The CLI works without it. Install or remove completion scripts with:

```sh
mlai-trade runtime completions install zsh
mlai-trade runtime completions uninstall zsh
mlai-trade runtime completions install bash
mlai-trade runtime completions install fish
```

Use `mlai-trade runtime completions generate zsh` when you only want to print the script to stdout.
