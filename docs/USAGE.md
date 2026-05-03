# mlai-trade Usage

Last updated: 2026-05-02

`mlai-trade` is a Rust CLI for provider-backed market data, shared ML training, compliance guardrails, and optional auto-trade execution. It is not financial, legal, tax, or trading advice. Use at your own risk.

## Runtime Home

Default runtime home:

```sh
~/mlai-trade
```

The CLI creates:

- `bin/`
- `config/`
- `data/`
- `db/`
- `docs/`
- `logs/`
- `api/`
- `tmp/`

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

Alpaca stock/ETF feed selection is configured in `alpaca.data_feed` or per account in `alpaca.accounts[].data_feed`.

Values:

- `auto`: try SIP first, then IEX fallback.
- `sip`: force SIP only, no fallback.
- `iex`: force IEX only, no fallback.

SIP is the consolidated U.S. equities feed across exchanges and venues. IEX is one exchange. Paid/live strategies that care about NBBO and slippage should use SIP.

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

`data daily` is the non-trading command that refreshes data and prepares ML outputs for auto-trade:

```sh
mlai-trade data daily --days 0 --walk-forward-folds 5 --top-n 20 --slippage-bps 50
```

`--days 0` means discover/use full available Alpaca daily stock-bar history. Future runs are gap-aware: if data already exists, the scanner overwrites the latest stored market date and fills only missing dates.

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

## Daemon

Daemon mode runs the automatic auto-trade cycle, refreshes tax estimates, rotates logs, and can run the daily non-trading maintenance path without cron. It refuses to start unless `daemon.enabled=true` in `mlai-trade.json`.

```sh
mlai-trade daemon start
mlai-trade daemon status
mlai-trade daemon reload
mlai-trade daemon restart
mlai-trade daemon stop
```

`reload` sends the daemon a config reload signal. The loop also rereads config between cycles. The auto-trade provider check interval is configured by `daemon.auto_trade_interval_seconds`, default `60`, clamped to `10`-`300` seconds.

Daily maintenance is controlled by `daemon.daily_refresh_*` config. By default, once per New York market date after `18:30:00`, the daemon syncs provider orders, runs `ml refresh` (which reconciles/syncs feeds before training), optionally syncs subscribed feeds again, refreshes tax estimates, and records success in `tmp/mlai-trade-daily-refresh.stamp`.

Default daemon files:

- `tmp/mlai-trade.pid`
- `tmp/mlai-trade-daily-refresh.stamp`
- `logs/mlai-trade-daemon.log`

## API

The API is a separate Unix-socket service. It refuses to start unless `api.enabled=true` in `mlai-trade.json`.

```sh
mlai-trade api start
mlai-trade api status
mlai-trade api test
mlai-trade api reload
mlai-trade api restart
mlai-trade api stop
```

Default API files:

- `api/mlai-trade-api.sock`
- `tmp/mlai-trade-api.pid`
- `logs/mlai-trade-api.log`

All API responses are JSON. `mlai-trade api test` sends a local health request through the Unix socket. The allowlist is visible with:

```sh
mlai-trade api status --json
```

The exposed sections are: `daemon` reload/status; `ml` refresh/explain/explainable/explained/status; `market` quote/bars/news/clock/calendar; `trade` account/orders/positions plus buy/sell/cancel/close only when auto-trading is disabled; `data` movers/screen/watchlist/suggest/status; `compliance` wash/pdt/tax; `auto` sync-orders/status/history/config; and `feeds` add/remove/sync/list/search/graph/sentiment/correlate/status. `runtime` is intentionally not exposed.

See `docs/API.md` for the full route table, request parameters, response wrapper, and curl examples.

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
| `tmp/mlai-trade.pid` | Daemon PID file when daemon mode is running. |
| `api/mlai-trade-api.sock` | Unix socket used by the local API service; created with `0600` permissions. |
| `tmp/mlai-trade-api.pid` | API service PID file when API mode is running. |
| `logs/mlai-trade-daemon.log` | Daemon output log. |
| `logs/mlai-trade-api.log` | API service output log and API request JSON lines. |
| `logs/mlai-trade-auto.log` | Auto-trade JSONL audit log for cycles, provider syncs, decisions, buys, sells, skips, source, and errors. |
| `logs/YYYYMMDD-*.log.gz` | Daily compressed log archives, for example `20260502-mlai-trade-auto.log.gz`. |
| `data/lightgbm_model.txt` | LightGBM model. |
| `data/lstm_sequence_model.bin` | LSTM model. |
| `data/ml_default_ensemble_config.json` | Saved ensemble weights/config. |
| `data/ml_ensemble_robust_sweep_report.json` | Ensemble sweep report. |
| `data/lightgbm_training_report.json` | Latest LightGBM report. |

Provider sync tables inside `db/mlai_trade.db`:

| Table | Purpose |
| --- | --- |
| `provider_order_snapshots` | Raw and parsed provider order rows by provider/account/order id. |
| `provider_fill_activities` | Raw and parsed provider fill activity rows by provider/account/activity id. |

Legacy names are migrated when encountered in the runtime home.
