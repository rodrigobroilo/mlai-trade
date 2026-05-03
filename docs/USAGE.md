# mlai-trade Usage

Last updated: 2026-05-02

`mlai-trade` is a Rust CLI for provider-backed market data, shared ML training, compliance guardrails, and optional auto-trade execution. It is not financial, legal, tax, or trading advice. Use at your own risk.

## Runtime Home

Default runtime home:

```sh
~/mlai-trade
```

The CLI creates:

- `api/`: Unix-socket API runtime files, including `mlai-trade-api.sock`.
- `bin/`: installed local binaries and helper executables.
- `config/`: local configuration and secrets, never committed.
- `data/`: generated ML datasets, models, reports, and market research artifacts.
- `db/`: SQLite databases for trades, market data, compliance state, predictions, and scanner state.
- `docs/`: local documentation copies.
- `logs/`: JSON-line application logs and compressed rotated logs.
- `tmp/`: PID files, daily refresh stamps, and other transient runtime state.

Override the runtime home with:

```sh
mlai-trade --home /path/to/mlai-trade runtime version
```

or:

```sh
MLAI_TRADE_HOME=/path/to/mlai-trade mlai-trade runtime version
```

## Configuration

Runtime config is:

```text
~/mlai-trade/config/mlai-trade.json
```

The repository tracks only:

```text
config/mlai-trade.example.json
```

The example JSON is intentionally explicit. Every supported config key should appear there, with `_comment` fields explaining valid values and defaults. The real `mlai-trade.json` contains credentials and is ignored by Git.

## Command Topics

The top-level CLI is intentionally grouped by topic. Hidden legacy aliases still exist for compatibility, but interactive help and autocomplete should lead users to these sections:

| Topic | Commands |
| --- | --- |
| `runtime` | `version`, `completions generate`, `completions install`, `completions uninstall` |
| `daemon` | `start`, `stop`, `restart`, `reload`, `status` |
| `api` | `start`, `stop`, `restart`, `reload`, `status`, `test` |
| `trade` | `account`, `orders`, `positions`, `buy`, `sell`, `cancel`, `close` |
| `market` | `data-feed`, `quote`, `watch`, `bars`, `news`, `sp500`, `history-start`, `clock`, `calendar` |
| `data` | `universe`, `scan`, `daily`, `screen`, `movers`, `watchlist`, `suggest`, `status` |
| `compliance` | `wash`, `pdt`, `tax` |
| `feeds` | `add`, `remove`, `sync`, `list`, `search`, `graph`, `sentiment`, `correlate`, `status` |
| `ml` | `refresh`, `full-refresh`, `features`, `labels`, `export`, `train`, `baselines`, `walk-forward`, `ablate-sp500`, `xgboost-ablate-sp500`, `lstm-train`, `lstm-predict`, `lstm-evaluate`, `predict`, `xgboost-predict`, `ensemble`, `ensemble-search`, `ensemble-default`, `ensemble-robust-sweep`, `compare-sp500-final`, `cache-shap`, `explain`, `explainable`, `explained`, `status` |
| `auto` | `run`, `sync-orders`, `status`, `history`, `config`, `enable`, `disable` |

Every wrong or incomplete command should print a useful error plus the relevant `--help` command. API routes follow the same topic names except `runtime`, which is intentionally not exposed.

## Providers And Accounts

At least one provider must be enabled. Alpaca is implemented today:

```json
{
  "providers": {
    "alpaca": { "enabled": true },
    "other": {}
  }
}
```

`alpaca.accounts[]` supports multiple paper and/or individual brokerage accounts. Positions, trades, cash/equity reads, realized P&L, unrealized P&L, order counts, and history are scoped by `provider`, `account_ref`, and paper-vs-real account mode.

The ML model, feature set, labels, predictions, and ensemble rankings are shared. Auto-trade accounts consume the shared predictions but write account-specific execution records.

List configured provider accounts before selecting one for trading or tax views:

```sh
mlai-trade trade account
mlai-trade trade orders --account paper-main --sync
```

`account` prints a stable selector such as `alpaca:paper-main`. `buy`, `sell`, `cancel`, and `close` require `--account` because the same symbol can exist in more than one account. `orders` and `positions` default to all enabled accounts, and also accept `--account` one or more times or as a comma-separated list.

## Compliance State

Real-money IRS/tax compliance ledgers are taxpayer-wide across all real provider accounts. Paper accounts obey the same rule logic in a separate paper compliance universe so simulation never contaminates real brokerage compliance.

The wash-sale statutory rule is not configurable: the code uses the IRS 30-day replacement window and adds `auto.compliance.wash_sale_safety_buffer_days`, defaulting to `1`, so replacement buys are blocked for 31 days by default. Config can increase the buffer but cannot reduce it below 1.

Execution records remain per account:

- `auto_positions`
- `auto_trades`
- account cash/equity/status reads
- realized and unrealized P&L summaries

Tax/compliance blockers are scoped by universe:

- `real`: shared across all real provider/accounts
- `paper`: shared across paper simulations only

Options trading remains disabled by hard rule.

## Market Clock

Auto-trade checks local clock guardrails, Alpaca provider calendar sessions, and Alpaca provider market clock behavior.

`auto.market.mode` values:

- `auto`: use Alpaca v3 calendar + v3 clock plus local exchange schedule.
- `provider`: use provider calendar/clock only unless explicit local checks are enabled.
- `local`: use configured local exchange schedule only.

Defaults model regular U.S. stock exchange hours:

```json
{
  "timezone": "America/New_York",
  "provider_markets": ["NYSE", "NASDAQ"],
  "use_provider_calendar": true,
  "use_provider_clock": true,
  "regular_open": "09:30:00",
  "regular_close": "16:00:00",
  "buy_start": "09:35:00",
  "buy_end": "15:45:00",
  "sell_start": "09:30:00",
  "sell_end": "15:55:00"
}
```

The provider v3 calendar endpoint returns the session bounds used for the trading decision and is queried in UTC. The provider v3 clock endpoint confirms the current market phase. `auto.market.timezone` remains the local exchange-hour guardrail timezone and defaults to `America/New_York`. If provider checks fail and `allow_local_clock_fallback=true`, the local configured schedule still prevents trading outside known hours. Add special closed days to `closed_dates` only when you need a local override.

Official Alpaca references: https://docs.alpaca.markets/reference/calendar-2 and https://docs.alpaca.markets/reference/clock-1.

Useful checks:

```sh
mlai-trade market clock
mlai-trade market calendar --market NYSE --market NASDAQ
```

DB timestamps are stored in UTC. Trade and compliance rows also store `market_timezone`, `market_session_source`, and provider session fields when available. Provider calendar core session fields are UTC.

## Data Feeds

Alpaca stock/ETF feed selection is configured per account in `alpaca.accounts[].data_feed`.

Values:

- `auto`: try SIP first, then IEX fallback.
- `sip`: force SIP only, no fallback.
- `iex`: force IEX only, no fallback.

SIP is the consolidated U.S. equities feed across exchanges and venues. IEX is one exchange. Paid/live strategies that care about NBBO and slippage should use SIP.

Check the active feed mode:

```sh
mlai-trade market data-feed
```

Probe earliest available Alpaca daily bar history for the configured feed:

```sh
mlai-trade market history-start
mlai-trade market history-start AAPL IBM SPY
```

Sync FRED S&P 500/VIX benchmark data:

```sh
mlai-trade market sp500 --days 0
```

## Backend Selection

Backends are configured under `backend`:

```json
{
  "backend": {
    "lstm": "auto",
    "xgboost": "auto",
    "lightgbm": "cpu",
    "ridge": "cpu"
  }
}
```

Backend support:

| Engine | Valid Values | Notes |
| --- | --- | --- |
| LSTM | `auto`, `cpu`, `mlx`, `tch` | `auto` chooses MLX on Apple Silicon when built with `mlx-lstm`, tch/CUDA on Linux NVIDIA when built with `tch-lstm` and available, otherwise CPU/Rayon. |
| XGBoost | `auto`, `cpu`, `cuda` | Requires optional `xgboost-baseline` build feature. CUDA is XGBoost-native CUDA on Linux/NVIDIA, not MLX/tch. |
| LightGBM | `cpu` | CPU-only in this Rust code today. Keep explicit for visibility. |
| Ridge | `cpu` | CPU-only in this Rust code today. Keep explicit for visibility. |

Forcing an unavailable accelerated backend should fail clearly. Auto mode may fall back to CPU when the accelerated runtime is missing or fails.

## Daily Pipeline

`data daily` is the operator-facing non-trading command that refreshes data and prepares ML outputs for auto-trade:

```sh
mlai-trade data daily --days 0 --walk-forward-folds 5 --top-n 20 --slippage-bps 50
```

By default, `data daily` runs the same shared full incremental pipeline as `ml refresh`. That means it includes the feed universe reconciliation, feed sync, feature generation, labels, LightGBM, Ridge/XGBoost, LSTM, walk-forward validation, post-slippage trading metrics, predictions, ensemble, SHAP cache, and cleanup. The only exception is `mlai-trade data daily --skip-train`, which refreshes data/features/labels but intentionally skips model training/evaluation/prediction refresh.

`--days 0` means discover/use full available Alpaca daily stock-bar history. Future runs are gap-aware: if data already exists, the scanner overwrites the latest stored market date and fills only missing dates.

The DB can be large because it stores full-history bars plus wide ML feature rows. Runtime memory and CPU worker caps are automatic by default: mlai-trade detects usable RAM on macOS, Linux, FreeBSD, or generic Unix, budgets 80%, derives SQLite cache/mmap, ML batch, LSTM, and LightGBM caps from that budget, and caps CPU-bound ML workers to 80% of logical CPUs. GPU/NPU backends are not CPU-capped. Inspect and maintain the DB with:

```sh
mlai-trade data db-stats
mlai-trade data db-optimize
```

`data db-stats` shows the detected memory source and final resource caps. Config mistakes fail before a command runs with a precise JSON path and expected value range, and daemon/API config errors are also written as JSON log events.

Pipeline order:

1. Refresh tradable universe.
2. Sync FRED market observations.
3. Sync Alpaca bars.
4. Reconcile the ML feed universe and sync feeds.
5. Compute ML features, including dated feed aggregates.
6. Compute forward-return labels.
7. Train LightGBM.
8. Run walk-forward validation.
9. Train Ridge/XGBoost baselines.
10. Run S&P 500 feature comparisons.
11. Train/evaluate LSTM variants.
12. Run ensemble robustness sweep.
13. Refresh predictions and default ensemble.
14. Cache default SHAP explanations for open positions and the top 100 ensemble candidates.
15. Evaluate latest predictions when labels are available.
16. Clean transient training matrices.

`data daily` never places trades. It is safe to run for preparation while auto-trade is disabled or outside market hours.

### Automatic Daemon Prep

The daemon can run the same preparation automatically when:

- `daemon.enabled=true`
- `daemon.daily_refresh_enabled=true`
- `daemon.daily_refresh_trigger=market_close` and the current open market date is at least `daemon.daily_refresh_after_close_minutes` past configured regular close
- the daily success stamp has not already been written for that market date

With the default config, this means once per open New York market date, one hour after the regular close. The daemon checks this on every loop; it does not need cron. After success, it writes `tmp/mlai-trade-daily-refresh.stamp` with that market date and will not run daily prep again for the same date.

The closed-market auto-trade backoff does not stop daily prep. Weekend and configured closed dates are skipped by the market-close trigger. If the daemon misses a day, the next successful incremental refresh still fills missing Alpaca/FRED/feed/model data because `ml refresh` is gap-aware.

`daily_refresh_time` is available only when `daemon.daily_refresh_trigger=time`. That mode is a raw fixed-clock fallback. The recommended default is `market_close`, because it uses the configured exchange close and avoids another magic schedule value.

Daemon daily prep triggers this sequence:

1. Rotate/sanitize JSON logs.
2. Sync provider orders/fills when `daemon.daily_refresh_sync_orders=true`.
3. Run `ml refresh` with configured `days`, `backend=auto`, walk-forward folds, top-N, and slippage settings.
4. During `ml refresh`, refresh the tradable universe.
5. Sync FRED S&P 500/VIX/macro observations and fill missing observations.
6. Sync Alpaca SIP/IEX bars according to config. With `data_feed=sip`, SIP is used; `auto` tries SIP and falls back to IEX; forced `sip` has no fallback.
7. Overwrite the latest local bar date and fill missing bar ranges only.
8. Build the managed feed universe from current S&P 500 symbols, open positions, recent provider buys, latest Q1 candidates, and `feeds.extra_symbols`.
9. Add needed managed feed symbols and remove stale managed symbols. Manual `feeds add` subscriptions are kept.
10. Sync feed articles/filings before training when `feeds.sync_before_training=true`.
11. Compute dated feed aggregates and include those feed-derived values in the ML feature rows.
12. Compute forward-return labels.
13. Train/evaluate LightGBM, Ridge/XGBoost, and LSTM according to configured backends.
14. Run walk-forward validation and post-slippage trading metrics.
15. Refresh predictions, ensemble output, and default SHAP cache.
16. Optionally run an extra `feeds sync` when `daemon.daily_refresh_feeds_sync=true`.
17. Refresh federal tax estimates.
18. Write `tmp/mlai-trade-daily-refresh.stamp`.

Daily refresh config keys:

| Key | Function |
| --- | --- |
| `daemon.daily_refresh_enabled` | Turns this daemon-owned non-trading prep job on or off. |
| `daemon.daily_refresh_trigger` | `market_close` runs after market close; `time` runs after `daily_refresh_time`. |
| `daemon.daily_refresh_after_close_minutes` | Safety delay after `auto.market.regular_close`; default `60`. |
| `daemon.daily_refresh_time` | Fixed time used only by `daily_refresh_trigger=time`. |
| `daemon.daily_refresh_timezone` | Market-local timezone for date and time calculations. |
| `daemon.daily_refresh_days` | Passed to `ml refresh --days`; `0` means full discovery first, incremental later. |
| `daemon.daily_refresh_quick` | Adds `--quick` to the refresh command for faster validation runs. |
| `daemon.daily_refresh_walk_forward_folds` | Passed to walk-forward validation. |
| `daemon.daily_refresh_top_n` | Number of ranked candidates used for trading-metric evaluation. |
| `daemon.daily_refresh_slippage_bps` | Slippage assumption used in evaluation. |
| `daemon.daily_refresh_sync_orders` | Syncs provider orders/fills before training; default `true`. |
| `daemon.daily_refresh_feeds_sync` | Performs an extra feed sync after training; default `true`. |
| `daemon.daily_refresh_feeds_days` | Lookback window for the extra post-training feed sync. |

Manual single-command equivalents:

```sh
mlai-trade data daily       # full incremental prep; same shared pipeline as ml refresh
mlai-trade ml refresh       # same full incremental prep under the ML topic; used by daemon daily maintenance
mlai-trade ml full-refresh  # force rebuild of selected data/features/models/artifacts
```

Data utilities:

```sh
mlai-trade data universe
mlai-trade data scan --days 0
mlai-trade data screen --min-volume 500000
mlai-trade data movers
mlai-trade data watchlist
mlai-trade data suggest
mlai-trade data status
```

`data scan` is gap-aware. With `--days 0`, the first run discovers Alpaca's earliest available daily stock-bar date for the feed. Later runs overwrite the latest locally stored market date and fill missing dates only. `--force` intentionally re-requests the full selected window.

## ML Refresh

Use `ml refresh` for first setup, normal repairs, and daily manual ML preparation:

```sh
mlai-trade ml refresh
mlai-trade ml refresh --days 0 --walk-forward-folds 5 --top-n 20 --slippage-bps 50
```

It is gap-aware and incremental: it refreshes the universe, fills missing FRED observations, fills missing Alpaca bars, reconciles/syncs the feed universe, overwrites/recomputes the latest local day where needed, recomputes missing/latest feature and label rows, trains/evaluates LightGBM, Ridge/XGBoost, LSTM, writes predictions plus the default ensemble, and caches default SHAP explanations.

Feed symbols are reconciled before training. Managed feed subscriptions come from current S&P 500 symbols, provider/open positions, recent provider buys, latest Q1 candidates, and `feeds.extra_symbols`. Managed stale symbols are removed automatically. Manual subscriptions from `mlai-trade feeds add` are kept.

Current S&P 500 membership is used only as a feed collection universe. It is not stored as a historical training membership feature; feed-derived model inputs are dated article/filing aggregates only.

Use `ml full-refresh` when you intentionally want to force a rebuild:

```sh
mlai-trade ml full-refresh --days 0
```

`full-refresh` follows the same order but forces the Alpaca bar scan and ML feature recomputation instead of only filling gaps.

Detailed ML commands:

```sh
mlai-trade ml features --force
mlai-trade ml labels --horizon 5
mlai-trade ml export --format csv
mlai-trade ml train
mlai-trade ml baselines
mlai-trade ml walk-forward --folds 5
mlai-trade ml ablate-sp500
mlai-trade ml xgboost-ablate-sp500
mlai-trade ml lstm-train --backend auto
mlai-trade ml lstm-predict
mlai-trade ml lstm-evaluate --top-n 20 --slippage-bps 50
mlai-trade ml predict
mlai-trade ml xgboost-predict
mlai-trade ml ensemble-search
mlai-trade ml ensemble-default
mlai-trade ml ensemble-robust-sweep
mlai-trade ml compare-sp500-final
mlai-trade ml status
```

Models predict forward returns, not prices. Trading metrics are evaluated after configured round-trip slippage/spread assumptions.

## Feeds

Feeds collect news, RSS, SEC filing, sentiment, relationship, and correlation data used by dashboards and feed-derived ML features.

```sh
mlai-trade feeds add AAPL NVDA
mlai-trade feeds remove AAPL
mlai-trade feeds sync --days 7
mlai-trade feeds list
mlai-trade feeds search "AI" --limit 20
mlai-trade feeds graph NVDA
mlai-trade feeds sentiment NVDA
mlai-trade feeds correlate --days 30
mlai-trade feeds status
```

`feeds remove SYMBOL` fails when the symbol is not subscribed. This is deliberate so CLI/API callers can tell the difference between a successful removal and a typo.

## ML Explanations

SHAP explanations are cached for the latest feature date. The default cache includes:

- open auto-trade positions
- top 100 latest ensemble candidates
- any symbol explicitly requested by the user

Commands:

```sh
mlai-trade ml cache-shap
mlai-trade ml explained
mlai-trade ml explainable --limit 100
mlai-trade ml explain AAPL
```

The full market universe is not cached by default because SHAP is compute-heavy and most symbols are never inspected. Use `cache-shap --top N` to broaden the cache without generating all historical symbol/date explanations.

## Auto-Trade

Auto-trade does not share execution state across accounts.

```sh
mlai-trade auto status
mlai-trade auto sync-orders
mlai-trade auto run
mlai-trade auto history --limit 50
```

`auto sync-orders` is read-only. It syncs Alpaca orders and fill activities into `db/mlai_trade.db` so the provider remains the source of truth. The first run starts from the oldest provider history available; future runs rewind the latest local provider timestamp by one day, refresh that day, and then fill forward.

Before buy/sell orders, the engine checks:

- provider/account enabled state
- local exchange schedule
- provider calendar when enabled
- provider clock when enabled
- options hard ban
- blocked symbols
- cash-only buying rule
- spread and quote-size guardrails
- wash-sale compliance universe
- PDT/day-trade tracker
- configured buy/sell windows

Live execution prices use quotes first:

- buys use ask/offer or worse
- sells use bid or worse
- bar close fallback is allowed only when configured and is adjusted by `bar_fallback_bps`

The daemon does not call auto-trade again and again on a closed market date. Once all enabled accounts report `market_closed`, it logs `auto_market_closed_backoff_started` and backs off auto-trade cycles until the next market date. Manual `mlai-trade auto run` remains available for explicit checks.

Auto control:

```sh
mlai-trade auto enable
mlai-trade auto disable
mlai-trade auto config
mlai-trade auto config max_positions 10
```

## Daemon

Daemon mode runs the automatic auto-trade cycle, refreshes tax estimates, rotates logs, and can run the daily non-trading maintenance path without cron. It refuses to start unless `daemon.enabled=true` in `mlai-trade.json`.

```sh
mlai-trade daemon start
mlai-trade daemon status
mlai-trade daemon status --details
mlai-trade daemon reload
mlai-trade daemon restart
mlai-trade daemon stop
```

`reload` sends the daemon a config reload signal. The loop also rereads config between cycles. The auto-trade provider check interval is configured by `daemon.auto_trade_interval_seconds`, default `60`, clamped to `10`-`300` seconds.

Daily maintenance is controlled by `daemon.daily_refresh_*` config. By default, once per open New York market date one hour after the configured regular close, the daemon syncs provider orders, runs `ml refresh` (which reconciles/syncs feeds before training), optionally syncs subscribed feeds again, refreshes tax estimates, and records success in `tmp/mlai-trade-daily-refresh.stamp`. Set `daemon.daily_refresh_trigger=time` only if you want to use the fixed `daemon.daily_refresh_time` fallback instead.

`daemon status --details` reads the daemon heartbeat file and shows loop count, last auto-trade summary, last daily-refresh summary, CPU time, RSS memory, file descriptor count, and thread count when available. Missing platform metrics are shown as `not available`.

Default daemon files:

- `tmp/mlai-trade-daemon.pid`
- `tmp/mlai-trade-daily-refresh.stamp`
- `logs/mlai-trade-daemon.log`

## API

The API is a separate Unix-socket service. It refuses to start unless `api.enabled=true` in `mlai-trade.json`.

```sh
mlai-trade api start
mlai-trade api status
mlai-trade api status --details
mlai-trade api test
mlai-trade api reload
mlai-trade api restart
mlai-trade api stop
```

Default API files:

- `api/mlai-trade-api.sock`
- `tmp/mlai-trade-api.pid`
- `logs/mlai-trade-api.log`

All API responses are JSON. `mlai-trade api test` sends a local health request through the Unix socket. `mlai-trade api status --details` asks the API process for live counters and resource usage over the Unix socket. The allowlist is visible with:

```sh
mlai-trade api status --json
```

The exposed sections are: `daemon` reload/status; `ml` refresh/explain/explainable/explained/status; `market` quote/bars/news/clock/calendar; `trade` account/orders/positions plus buy/sell/cancel/close only when auto-trading is disabled; `data` movers/screen/watchlist/suggest/status; `compliance` wash/pdt/tax; `auto` sync-orders/status/history/config; and `feeds` add/remove/sync/list/search/graph/sentiment/correlate/status. `runtime` is intentionally not exposed.

See `docs/API.md` for the full route table, request parameters, response wrapper, and curl examples.

The API treats resource misses as errors. If a command returns JSON with `ok:false`, the API wrapper also returns `ok:false` and a non-2xx HTTP status. This applies to the whole API surface, not just feeds.

The API has local overload protection. `api.rate_limit_per_minute`, `api.max_concurrent_requests`, `api.max_concurrent_long_requests`, `api.max_body_bytes`, and `api.overload_retry_after_seconds` are explicit config keys. When rate or concurrency limits are exceeded, the API returns HTTP `429` with `ok:false`, `reason`, `retry_after_seconds`, and a `Retry-After` header. Oversized request bodies return HTTP `413`.

## Tax

Configure federal tax estimate inputs in `tax`:

```json
{
  "filing_status": "married_filing_jointly",
  "estimated_annual_income": 1000000.0,
  "include_paper_accounts_for_estimate": false
}
```

Supported filing statuses:

- `single`
- `married_filing_jointly`
- `married_filing_separately`
- `head_of_household`

Run:

```sh
mlai-trade compliance tax --accounts
mlai-trade compliance tax --show-brackets --year 2026
mlai-trade compliance tax --year 2026
mlai-trade compliance tax --year 2026 --account paper-main --details
mlai-trade compliance tax --year 2026 --quarter 1
mlai-trade compliance tax --year 2026 --quarter 1,2 --export csv
mlai-trade compliance tax --year 2026 --quarter 1-4
```

`--year` is mandatory for estimates and bracket display. `--quarter` is optional; omit it for the year-to-date/current-year view, or pass one contiguous quarter list/range such as `1`, `1,2`, or `1-4`. Tax estimates read closed `auto_positions` plus provider fill activities matched FIFO, exclude paper accounts by default, classify short-term vs long-term by holding period, apply short-term gains as incremental ordinary income, apply long-term gains through IRS 0%/15%/20% capital-gain brackets, and call out estimated 3.8% Net Investment Income Tax when the configured income crosses the filing-status threshold. Results include quarter breakdowns and are saved to `db/tax.db` with consolidated, provider, and account scopes. CSV exports are written to `data/tax_<year>_<period>.csv`.

Use `--account paper-main` to include a paper account for simulation. Without an explicit paper account selector, paper positions remain excluded.

## Files

Current runtime names:

| Path | Purpose |
| --- | --- |
| `db/mlai_trade.db` | SQLite database for market data, ML rows, predictions, account execution records, and compliance state. |
| `db/tax.db` | SQLite database for saved federal tax estimates by consolidated/provider/account scope. |
| `tmp/mlai-trade-daemon.pid` | Daemon PID file when daemon mode is running. |
| `api/mlai-trade-api.sock` | Unix socket used by the local API service; created with `0600` permissions. |
| `tmp/mlai-trade-api.pid` | API service PID file when API mode is running. |
| `logs/mlai-trade-daemon.log` | Daemon output log. |
| `logs/mlai-trade-api.log` | API service output log and API request JSON lines. |
| `logs/mlai-trade-auto.log` | Auto-trade JSONL audit log for cycles, provider syncs, decisions, buys, sells, skips, source, and errors. |
| `logs/mlai-trade-data.log` | Data command JSONL lifecycle log. |
| `logs/mlai-trade-ml.log` | ML command JSONL lifecycle log. |
| `logs/mlai-trade-training.log` | Training and validation command JSONL lifecycle log. |
| `logs/mlai-trade-feeds.log` | Feed command JSONL lifecycle log. |
| `logs/YYYYMMDD-*.log.gz` | Daily compressed log archives, for example `20260502-mlai-trade-auto.log.gz`. |
| `config/mlai-trade.json` | Local runtime config with secrets. Ignored by Git. |
| `config/tax-brackets.json` | Local IRS bracket/rate data used by `compliance tax`. |
| `data/lightgbm_model.txt` | LightGBM model. |
| `data/lstm_sequence_model.bin` | LSTM model. |
| `data/ml_default_ensemble_config.json` | Saved ensemble weights/config. |
| `data/ml_ensemble_robust_sweep_report.json` | Ensemble sweep report. |
| `data/lightgbm_training_report.json` | Latest LightGBM report. |

Sensitive runtime directories are private (`0700`) and sensitive runtime files are private (`0600`). That includes `config/`, `data/`, `db/`, `logs/`, `api/`, generated ML artifacts, DBs, logs, and sockets. PID files are runtime metadata and use `0644`. Relative log, PID, socket, and tax-bracket path overrides are resolved inside their expected runtime folders so files do not land in the caller's current directory.

Provider sync tables inside `db/mlai_trade.db`:

| Table | Purpose |
| --- | --- |
| `provider_order_snapshots` | Raw and parsed provider order rows by provider/account/order id. |
| `provider_fill_activities` | Raw and parsed provider fill activity rows by provider/account/activity id. |

Legacy names are migrated when encountered in the runtime home.
