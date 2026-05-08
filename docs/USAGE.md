# mlai-trade Usage

Last updated: 2026-05-07

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
- `logs/`: current JSON-line application logs.
- `logs/archived/`: compressed rotated log archives.
- `tmp/`: PID files, daily refresh stamps, the update lock, and other
  transient runtime state.

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

## Global Options

Use `mlai-trade -v` or `mlai-trade --version` to print the binary version.
Use `mlai-trade runtime version` when you also want runtime paths, configured
mode, database path, and disclaimer text.

## Command Topics

The top-level CLI is intentionally grouped by topic. Hidden legacy aliases still exist for compatibility, but interactive help and autocomplete should lead users to these sections:

| Topic | Commands |
| --- | --- |
| `runtime` | `version`, `completions generate`, `completions install`, `completions uninstall` |
| `daemon` | `start`, `stop`, `restart`, `reload`, `status` |
| `api` | `unix ...`, `ssl status`, `ssl dns-check` |
| `trade` | `account`, `orders`, `positions`, `buy`, `sell`, `cancel`, `close` |
| `market` | `quote`, `watch`, `bars`, `warm-bars`, `news`, ... |
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

`alpaca.accounts[]` supports multiple paper and/or individual brokerage
accounts. Positions, trades, cash/equity reads, realized P&L, unrealized P&L,
order counts, and history are scoped by `provider`, `account_ref`, and
paper-vs-real account mode. The local `account_ref` comes from the config
account `name`; for Alpaca, provider sync also stores the stable broker account
ID and uses it to reconcile local rows if the config name is changed later.

The ML model, feature set, labels, predictions, and ensemble rankings are shared. Auto-trade accounts consume the shared predictions but write account-specific execution records.

List configured provider accounts before selecting one for trading or tax views:

```sh
mlai-trade trade account
mlai-trade trade orders --account alpaca:paper-main --sync
```

`account` prints a selector such as `alpaca:paper-main` plus the provider
broker account ID when available. Use the selector for CLI/API account
selection. `buy`, `sell`, `cancel`, and `close` require `--account` because the
same symbol can exist in more than one account. `orders` and `positions`
default to all enabled accounts, and also accept `--account` one or more times
or as a comma-separated list.

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
| LSTM | `auto`, `cpu`, `mlx`, `tch` | Auto accelerator or CPU/Rayon. |
| XGBoost | `auto`, `cpu`, `cuda` | Default on macOS/Linux; not on FreeBSD. |
| LightGBM | `cpu` | CPU-only in this Rust code today. Keep explicit for visibility. |
| Ridge | `cpu` | CPU-only in this Rust code today. Keep explicit for visibility. |

Forcing an unavailable accelerated backend should fail clearly. Auto mode falls
back to CPU when the accelerated runtime is missing or fails, including MLX
Metal library load failures.

XGBoost CUDA is XGBoost-native on Linux/NVIDIA. It is separate from MLX and
`tch`.

LSTM `auto` chooses MLX on Apple Silicon by default. The tch/CUDA profile is
present for Linux/NVIDIA builds. PyTorch MPS exists on Apple Silicon, but
mlai-trade uses MLX there rather than `tch`/MPS until a separate MPS trainer is
implemented and validated. If the
accelerated runtime fails, `auto` falls back to CPU/Rayon; forced `mlx` or
`tch` fails clearly.

LSTM hyperparameters are separated from provider/runtime configuration:

```text
~/mlai-trade/config/mlai-trade-ml-tuning.json
```

Copy from `config/mlai-trade-ml-tuning.example.json` when you want local tuning.
`backend.lstm=auto` resolves the backend first, then applies the matching
profile. Built-in defaults are CPU `64` hidden units for `10` epochs, and
accelerator profiles use `128` hidden units for up to `50` epochs with
`lr=0.0001`, MSE, dropout `0.1`, and weight decay `0.01`. All profiles default
to return regression because auto-trade ranking and ensemble selection need
comparable forward-return scores. Regression targets are z-scaled during
training and decoded back into return space for validation, prediction, and
ensemble output. Direction mode is available for experiments and reports
directional accuracy, precision, and recall.

The accelerator default was selected from the paused 365-day real-data sweep at
442/649 variants. The current balanced winner is
`h128_lr0p0001_mse0_do0p1_wd0p01`, and the default ensemble fallback is
`LightGBM=40%` plus `LSTM=60%` unless
`data/ml_default_ensemble_config.json` exists.

## Daily Pipeline

`data daily` is the operator-facing non-trading command that refreshes data and prepares ML outputs for auto-trade:

```sh
mlai-trade data daily --days 0 --walk-forward-folds 5 --top-n 20 --slippage-bps 50
```

By default, `data daily` runs the same shared full incremental pipeline as `ml refresh`. That means it includes the feed universe reconciliation, feed sync, feature generation, labels, LightGBM, Ridge/XGBoost, LSTM, walk-forward validation, post-slippage trading metrics, predictions, ensemble, SHAP cache, and cleanup. The only exception is `mlai-trade data daily --skip-train`, which refreshes data/features/labels but intentionally skips model training/evaluation/prediction refresh.

`--days 0` means discover/use full available Alpaca daily stock-bar history. Future runs are gap-aware: if data already exists, the scanner overwrites the latest stored market date and fills only missing dates.

The DB can be large because it stores full-history bars plus wide ML feature rows. Runtime memory and CPU worker caps are automatic by default: mlai-trade detects usable RAM on macOS, Linux, FreeBSD, or generic Unix, budgets 80%, derives SQLite cache/mmap, ML batch, LSTM, and LightGBM caps from that budget, and caps Tokio async workers plus CPU-bound workers to 80% of total logical CPU capacity. On 16 logical CPUs, that target is `1280%` in top-style CPU terms. GPU/NPU backends are not CPU-capped. Inspect and maintain the DB with:

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
5. Compute bounded feed-subscription price correlations.
6. Compute ML features, including dated feed aggregates and feed-universe
   return/correlation features.
7. Compute forward-return labels.
8. Train LightGBM.
9. Run walk-forward validation.
10. Train Ridge/XGBoost baselines.
11. Run S&P 500 feature comparisons.
12. Train/evaluate LSTM variants.
13. Run ensemble robustness sweep.
14. Refresh predictions and default ensemble.
15. Cache default SHAP explanations for open positions and the top 100 ensemble
    candidates.
16. Evaluate latest predictions when labels are available.
17. Clean transient training matrices.

`data daily` never places trades. It is safe to run for preparation while auto-trade is disabled or outside market hours.

Long preparation commands are mutually exclusive. `data daily`, `ml refresh`,
`ml full-refresh`, and daemon daily maintenance all use
`tmp/mlai-trade-update.lock`. If another update is active, the command refuses
to start and reports the owning PID, source, operation, start time, and lock
path. The lock writes JSON start/finish events to the daemon/data/ml/training
and feeds logs, including duration and status. Ctrl-C/SIGTERM attempts are
logged as `cancelled_by_signal`; hard exits are detected as stale locks on the
next run and cleaned up before a new update starts.

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
11. Compute bounded feed-subscription price correlations when
    `feeds.compute_correlations_before_training=true`.
12. Compute dated feed aggregates and point-in-time feed-universe
    return/correlation features in the ML feature rows.
13. Compute forward-return labels.
14. Train/evaluate LightGBM, Ridge/XGBoost, and LSTM according to configured
    backends.
15. Run walk-forward validation and post-slippage trading metrics.
16. Refresh predictions, ensemble output, and default SHAP cache.
17. Optionally run an extra `feeds sync` when
    `daemon.daily_refresh_feeds_sync=true`.
18. Refresh federal tax estimates.
19. Write `tmp/mlai-trade-daily-refresh.stamp`.

Daily refresh config keys:

| Key | Function |
| --- | --- |
| `daemon.daily_refresh_enabled` | Turns this daemon-owned non-trading prep job on or off. |
| `daemon.daily_refresh_trigger` | `market_close` runs after market close; `time` runs after `daily_refresh_time`. |
| `daemon.daily_refresh_after_close_minutes` | Delay after close. |
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
mlai-trade ml lstm-train --backend mlx --hidden-dim 128 \
  --epochs 20 --learning-rate 0.001
mlai-trade ml lstm-train --backend cpu --target-mode direction
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

`feeds sync` uses per-source parallelism. By default SEC EDGAR runs one symbol
query at a time, while Alpaca, Yahoo RSS, and Google RSS run two symbol queries
at a time. Each source request attempt times out after `10s` and retries twice
by default. With `feeds.auto_tune_sources=true`, a source backs down after
timeout/error waves and cautiously returns to its configured concurrency after
clean waves.

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
mlai-trade auto track WSHP --account alpaca:paper-original
mlai-trade auto untrack WSHP --account alpaca:paper-original
mlai-trade auto run
mlai-trade auto history --limit 50
mlai-trade trade orders --sync
mlai-trade trade positions --sync
```

`trade positions` always queries the provider live and refreshes the local
`provider_position_snapshots` table from that response before printing. The
`--sync` flag adds the heavier provider order/fill history refresh first, which
is useful when you want positions and execution/compliance history reconciled in
one command.

`ml explain SYMBOL` reports signed model contributors. The human view separates
positive contributors from negative anchors, and JSON includes
`shap_sum`, `prediction_minus_base`, and `additivity_error` so the explanation
can be checked against the LightGBM prediction.

`auto track SYMBOL --account ACCOUNT` adopts exactly one existing
provider-held position into auto management. It does not buy more shares and it
does not rewrite the original buy/fill audit trail. Auto rules start managing
that position from the adoption point forward. `SYMBOL` and `--account` are
mandatory. `--account` must be the full `provider:account-ref` selector, such
as `alpaca:paper-original`. Bare refs and broad selectors such as `all`,
`paper`, `real`, or `alpaca` are rejected.

`auto untrack SYMBOL --account ACCOUNT` releases exactly one auto-managed
position back to manual `mlai-cli` management. It does not sell the holding.
The provider position remains visible under the not-tracked section of
`auto status`, and auto-trade will no longer apply exit rules to it unless it
is tracked again. It has the same explicit-symbol and explicit-account safety
rules as `auto track`.

`auto sync-orders` is read-only. It syncs Alpaca orders and fill activities into
`db/mlai_trade.db` so the provider remains the source of truth. The first run
starts from the oldest provider history available; future runs rewind the latest
local provider timestamp by one day, refresh that day, and then fill forward.
Human output prints order/fill sync under each account, then prints shared
wash-sale reconciliation under `Compliance universe checks` for the paper and
real tax universes.
Provider order/fill rows also store `execution_origin`:

- `mlai_auto`: submitted by the daemon auto-trade engine.
- `mlai_cli`: submitted by `mlai-trade trade buy`, `sell`, or `close`.
- `provider_external`: observed at the provider but not created by mlai-trade.
- `mixed`: realized lot where entry and exit came from different origins.
- `unknown`: older or incomplete provider rows that could not be classified.

`trade orders`, `auto sync-orders`, `auto status`, `auto history`, `status`, and
`compliance tax --details` expose the origin. Older `plm-*` client order IDs are
classified as CLI activity; new CLI orders use the `mlai-cli-*` prefix.

`auto status` refreshes the provider's current live-position snapshot before it
prints account status. It shows auto-managed positions separately from provider
positions that are not tracked by auto rules, so holdings opened by CLI commands
or directly at Alpaca remain visible without being confused with positions auto
is allowed to exit. Both sections use the same position columns. Direct
provider-origin rows display the provider name, such as `alpaca`, instead of a
generic provider label.

Origin and management are intentionally separate. `execution_origin` answers
how the position was opened historically, for audit/tax/P&L review.
`management_origin` answers who is allowed to manage it now. For example, an
`alpaca` position can be adopted into `mlai-auto` management without changing
the fact that the provider was the original source of the buy.

After every provider fill sync, `mlai-trade` reconciles wash-sale monitor rows
from provider-confirmed fills. Paper fills are reconciled as one paper
simulation universe; real-money fills are reconciled as one IRS-relevant real
universe across all real provider accounts. The stored wash-sale row keeps the
provider/account that produced the loss sale for audit, but the active blocker
is by tax universe and symbol, not by account.

If an account is renamed in config, the next provider sync uses Alpaca's broker
account ID to move local history from the old account ref to the current one.
This prevents a local rename such as `paper-main` to `paper-original` from
doubling order/fill snapshots or wash-sale rows.
New JSON logs and account/order/position/status JSON include both the mutable
local `account_ref` and the stable provider `broker_account_id` whenever Alpaca
exposes it, so account history remains traceable across future renames.

`providers.alpaca.enabled` and `alpaca.accounts[].enabled` decide whether an
account participates in provider operations.
`alpaca.accounts[].auto_trade_enabled` is narrower: when it is `false`,
provider sync and status still run, but autonomous buy/sell decisions are
skipped for that account.

Before exit rules are evaluated, auto-trade compares local open
`auto_positions` with the provider's live position snapshot. If the provider no
longer reports a long position, the local row is closed from provider-confirmed
sell history and no sell order is sent. If the provider reports fewer shares
than the local row, the local share count is adjusted down before sizing an
exit. These repairs log `auto_position_reconciled_from_provider` and are
included in the cycle payload as `provider_position_reconciliation`.

Provider sync is also the audit path for activity outside mlai-trade. If a user
buys, sells, cancels, or moves cash directly at Alpaca, the next provider sync
stores the provider order/fill/account snapshot and logs
`provider_external_order_observed`, `provider_external_fill_observed`, or
`provider_account_snapshot_changed`. The provider remains the source of truth;
mlai-trade does not invent auto-position rows for manual positions, but it
counts provider-held symbols and cash before making new auto-trade decisions.
Equity and portfolio value snapshots are updated every sync, but only cash
changes produce `provider_account_snapshot_changed` log entries to avoid
mark-to-market log noise.

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

Stop-loss and take-profit exits are no longer single-tick decisions by default.
A normal stop-loss breach waits for the configured confirmation cycles before
selling, while a deeper emergency stop-loss sells immediately. A take-profit
breach waits for the configured confirmation cycles and minimum hold time; once
the take-profit threshold has been crossed, optional trailing giveback can sell
if profit pulls back from the best observed level. Defaults are:

- stop-loss confirmation: enabled, 3 cycles, max wait 5 minutes, emergency
  stop at -10%.
- take-profit confirmation: enabled, 3 cycles, minimum hold 5 minutes,
  trailing enabled with 3 percentage-point giveback.

Each waiting cycle logs `auto_exit_confirmation_wait` with symbol, account,
rule, cycles remaining, minutes remaining when applicable, current price,
entry price, and P&L. When the rule reaches confirmation or emergency breach,
the log records `auto_exit_rule_triggered` and then
`auto_exit_order_submitted` or `auto_exit_order_failed`.

The cash-only rule does not trust broker buying power and does not rely only on
the latest provider `cash` value. Before each buy decision, `mlai-trade`
syncs provider orders/fills, fetches live account and position snapshots, and
computes deployable cash as the lower of provider cash and account equity after
current long exposure plus pending buy reservations. This keeps paper and real
accounts from intentionally using margin even if the broker exposes margin
buying power.

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
mlai-trade auto config stop_loss_confirmation_cycles 3
mlai-trade auto config take_profit_confirmation_trailing_giveback_pct 3
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

Daily maintenance is controlled by `daemon.daily_refresh_*` config. By
default, once per open New York market date 6 hours after the configured
regular close, the daemon syncs provider orders, runs `ml refresh`, optionally
syncs subscribed feeds again, refreshes tax estimates, and records success in
`tmp/mlai-trade-daily-refresh.stamp`. Set `daemon.daily_refresh_trigger=time`
only if you want to use the fixed `daemon.daily_refresh_time` fallback instead.
Successful manual full prep after market close also updates this stamp, so a
completed `data daily`, `ml refresh`, or `ml full-refresh` prevents the daemon
from rerunning the same market date later.

`daemon status --details` reads the daemon heartbeat file and shows loop count,
last auto-trade summary, last daily-refresh summary, process CPU,
machine-normalized CPU, CPU capacity, CPU worker budget, accelerator
availability, CPU time, RSS memory, memory budget, open files/sockets, and OS
thread count. Runtime metrics use native Linux `/proc`, macOS Mach APIs, and
FreeBSD `sysctl`/`kinfo_proc` paths where available. Missing platform metrics
are shown as `not available`.

Fake Alpaca provider validation is available through:

```sh
scripts/provider-fake-alpaca-test.sh run target/release/mlai-trade
```

This starts the hidden local fixture server (`runtime fake-alpaca-server`) on an
ephemeral loopback port, writes a disposable config with
`alpaca.accounts[].trading_base_url` and `alpaca.accounts[].data_base_url`
pointing at that fixture, and validates one month of fake stock/ETF data,
provider account/order/position sync, paper buy/sell, tax selection, and
Unix-socket API requests. It never uses live Alpaca credentials.

Linux-path validation is available through:

```sh
scripts/linux-ubuntu-test.sh run
```

On Linux this runs natively. On macOS, FreeBSD, or another non-Linux host, it
runs inside an Ubuntu 24.04 Docker container. On macOS the script automatically
provisions Docker CLI + Colima with Homebrew if needed. The Ubuntu image is
cached locally as `mlai-trade:ubuntu-test`; normal runs reuse it offline when
the Dockerfile fingerprint matches. Use `scripts/linux-ubuntu-test.sh update`
only when you want to pull/rebuild the image.

To inspect the container:

```sh
docker images mlai-trade
docker ps
docker ps -a
scripts/linux-ubuntu-test.sh container
docker exec -it mlai-trade-ubuntu-test bash
docker rm -f mlai-trade-ubuntu-test
```

Linux validation runs `cargo fmt`, `cargo check`, `cargo test`, release build,
the CLI smoke test, the synthetic ML e2e test, and the fake Alpaca provider
test.

The repo-owned Linux test image definition is `tests/linux-ubuntu/Dockerfile`.
The FreeBSD/Lima harness notes live in `tests/freebsd-lima/`. Executable
entrypoints remain in `scripts/`.

Validation modes:

- `scripts/linux-ubuntu-test.sh run`: run validation. On Linux this runs
  natively; on non-Linux hosts it uses the cached Ubuntu image, removes stale
  kept inspection containers first, and removes the validation container after
  the run.
- `scripts/linux-ubuntu-test.sh clean`: remove the named kept inspection
  container while preserving the cached image and build volumes.
- `scripts/linux-ubuntu-test.sh container`: keep a named Ubuntu container
  running for inspection.
- `scripts/linux-ubuntu-test.sh shell`: open a temporary interactive Ubuntu
  shell and remove it on exit.
- `scripts/linux-ubuntu-test.sh update`: pull/rebuild the cached Ubuntu image.
- `scripts/linux-ubuntu-test.sh delete`: remove the named container, cached
  image, and build-cache volumes.
- `scripts/linux-ubuntu-test.sh --help`: show script commands and environment
  overrides.

Inside the inspection container, the filtered repo copy is available at
`/tmp/mlai-trade-src`.

Linux validation storage:

- Docker image: `mlai-trade:ubuntu-test`.
- Debug container, when explicitly kept: `mlai-trade-ubuntu-test`.
- Docker volumes: `mlai-trade-cargo-registry`, `mlai-trade-cargo-git`,
  `mlai-trade-target-linux-ubuntu`.
- On macOS with Colima, the Docker engine/profile lives under
  `~/.colima/default`; inside the engine, Docker reports its data root with
  `docker info --format '{{.DockerRootDir}}'`.

FreeBSD-path validation is available through:

```sh
scripts/freebsd-lima-test.sh run
```

On FreeBSD this runs natively. On macOS, Linux, or another non-FreeBSD host, it
uses a cached Lima FreeBSD 16 VM named `mlai-trade-freebsd16-test`. On macOS
the script automatically provisions Lima + QEMU with Homebrew if needed.

FreeBSD validation modes:

FreeBSD validation runs the same check/test/build, CLI smoke, synthetic ML e2e,
and fake Alpaca provider test sequence inside the guest.

- `scripts/freebsd-lima-test.sh run`: run validation. On FreeBSD this is native;
  on non-FreeBSD hosts it uses the cached FreeBSD VM and removes stale guest
  test work directories first.
- `scripts/freebsd-lima-test.sh clean`: remove stale guest repo/test runtime
  directories while preserving the cached VM.
- `scripts/freebsd-lima-test.sh shell`: copy the filtered repo and open a
  shell inside `/tmp/mlai-trade-src`.
- `scripts/freebsd-lima-test.sh update`: delete/recreate the cached VM.
- `scripts/freebsd-lima-test.sh stop`: stop the cached VM.
- `scripts/freebsd-lima-test.sh delete`: delete the cached VM.
- `scripts/freebsd-lima-test.sh --help`: show script commands and environment
  overrides.

FreeBSD validation storage:

- Lima instance: `mlai-trade-freebsd16-test`.
- Host directory: `~/.lima/mlai-trade-freebsd16-test`.
- Guest repo copy: `/tmp/mlai-trade-src`, recreated each run.

Default daemon files:

- `tmp/mlai-trade-daemon.pid`
- `tmp/mlai-trade-daily-refresh.stamp`
- `logs/mlai-trade-daemon.log`

## API

The API has explicit transports:

- `api unix`: local Unix-socket JSON API, active today.
- `api ssl`: optional remote HTTPS transport, TCP/443 and UDP/443 by default,
  TLS 1.3 only. It prefers hybrid ML-KEM key exchange and falls back only to
  strong TLS 1.3 classical groups for browser compatibility. TCP HTTPS serves
  the dashboard and JSON API directly and advertises `Alt-Svc` for browsers
  that can upgrade to H3/QUIC.

The Unix transport refuses to start unless `api.enabled=true` and
`api.unix.enabled=true` in `mlai-trade.json`. Legacy `mlai-trade api start`
commands target the Unix transport.

```sh
mlai-trade api unix start
mlai-trade api unix status
mlai-trade api unix status --details
mlai-trade api unix test
mlai-trade api unix reload
mlai-trade api unix restart
mlai-trade api unix stop

mlai-trade api start
mlai-trade api status
mlai-trade api status --details
mlai-trade api test
mlai-trade api reload
mlai-trade api restart
mlai-trade api stop
```

Remote H3 planning/status:

```sh
mlai-trade api ssl enable
mlai-trade api ssl cert generate --target h3
mlai-trade api ssl cert renew --target h3 --domain localhost \
  --organization MLAI-TRADE --organizational-unit MLAI-TRADE
mlai-trade api ssl cert info
mlai-trade api ssl start
mlai-trade api ssl status
mlai-trade api ssl status --json
mlai-trade api ssl dns-check example.com
mlai-trade api ssl stop
```

Remote public discovery should use DNS HTTPS/SVCB records with `alpn=h3` and
port `443` when possible. Browsers can connect over TCP HTTPS first and then
upgrade to H3/QUIC when they honor the `Alt-Svc` response. The Let's Encrypt
TLS-ALPN-01 challenge responder remains disabled by default; when enabled, it
is separate and exposes no API routes.

`api ssl dns-check` also reports DNS HTTPS/SVCB `ech` parameters when present.
If `api.ssl.ech.enabled=true`, startup still fails closed because the current
rustls/quinn listener cannot terminate server-side ECH yet.

Generated H3 and ACME challenge certificates default to
`O=MLAI-TRADE` and `OU=MLAI-TRADE`. Override those subject fields with
`--organization`/`--o` and `--organizational-unit`/`--ou` on
`api ssl cert generate` or `api ssl cert renew`.

The default TLS key exchange policy is `mlkem_secure_fallback`: H3/QUIC and TCP
HTTPS both offer hybrid ML-KEM groups first, then allow only strong TLS 1.3
classical fallback groups (`X25519`, `P-256`, `P-384`). Set
`api.ssl.key_exchange_policy=mlkem_required` only for controlled clients that
are known to support the required ML-KEM/hybrid groups.

The remote H3 listener also serves the built React dashboard:

```text
https://localhost/
https://127.0.0.1/
https://[::1]/
```

Localhost browser access bypasses auth. Non-localhost clients must authenticate
with `api.ssl.auth.username` and `api.ssl.auth.password`. Startup refuses
non-loopback binds when auth is disabled or the password is still the example
`replace_me` value. The dashboard is responsive for mobile and notebook/desktop
screens and uses real API routes for accounts, positions, orders, data,
compliance, feed sentiment, and ML explain output. It polls read-only account,
position, order, and compliance snapshots after page load, with slower
data-pipeline refreshes in the background. Normal dashboard refreshes do not
force provider sync; use `Sync orders` when a manual provider reconciliation is
wanted.
The active dashboard tab is stored in the URL hash and local browser storage,
so refreshing `#positions` stays on Positions. The top-bar account selector
defaults to all accounts; selecting one account filters account, position,
order, and auto-trade views locally without changing the underlying API
snapshot.
Position symbols are clickable. They open a symbol insight overlay containing
feed sentiment, recent headlines, ML explain values, and plain-English SHAP
feature descriptions.
The dashboard opens `/events/stream` for lightweight realtime refresh hints.
When the browser is connected over H3, those events ride the HTTP/3/QUIC stream;
otherwise they use TCP HTTPS. If the stream is not available, the dashboard
keeps normal snapshot polling. The stream sends a 15-second heartbeat and a
60-second refresh hint; it only coordinates refresh timing and does not force
provider order/position sync.
The overview and account pages show green/red P&L charts and allocation bars.
Charts include date labels and share a range selector for Today, 3 days, 7
days, or a custom start/end range. Overview allocation sits under the
performance chart as a two-column scrollable list. The positions page adds a
compact P&L chart per open position from market-bar snapshots. Chart bars use
range-aware defaults: Today uses 1-minute bars, 3 days uses 5-minute bars, 7
range-aware defaults: Today uses 5-minute bars, 3 days uses 15-minute bars, 7
days uses 30-minute bars, 8-30 days uses hourly bars, and longer ranges use
daily bars. Provider bars are backfilled into `market_bar_cache`, which is
separate from the daily ML `bars` table. Fresh cache rows are served before a
provider request, and the daemon proactively warms the cache for current
provider positions when `daemon.dashboard_bar_cache_enabled=true`. The toolbar
shows the active bar interval, and chart hover tooltips show the nearest
timestamp and P&L value.
The top-bar account selector also scopes the Tax selector. Leaving it on
all accounts keeps the default real-account tax estimate; selecting a provider
account loads that account's tax view, including paper accounts for simulation.
Changing the Tax year or account selector reloads the estimate automatically.
The dashboard batches position chart bars with `/market/bars?symbols=...`.
The API accepts up to 50 symbols and 25,000 requested bars per market-bars
batch. Requested bars are `symbols * limit`. Clients can query `/limits` to
discover the current caps, dashboard table sizes, and supported response
compression encodings, then split symbol lists or date ranges when needed. The
dashboard reads those limits and normally uses 25-symbol batches.
API responses can use `zstd`, `br`, `gzip`, or `deflate` when the client sends
the matching `Accept-Encoding`; browsers decompress automatically, and scripts
can use `curl --compressed`.
The Overview performance chart aggregates provider open-position P&L from those
intraday bar series instead of using a two-point current-value fallback. P&L
charts label the entry break-even line and draw a vertical buy marker when the
entry timestamp is inside the selected range. Per-position charts show an
explicit no-bars message when the provider has no data for the selected range.
The dashboard does not expose raw API response panels, a separate Auto tab, or
the auto configuration payload.
Orders,
positions, tax details, and wash-sale tables start at 50 rows and expand with
`Show more +50`. Tax can be loaded for explicit paper account selectors for
simulation, while default tax still excludes paper.
Watchlist and Movers start at 20 rows and expand with `Show more +20`.

IPv4 and IPv6 listeners are both enabled by default with
`api.ssl.ipv4_enabled=true` and `api.ssl.ipv6_enabled=true`; disable either
stack in config if needed. IPv6 bind hosts such as `::` and IPv6 localhost
`::1` are supported. Request logs report the concrete destination address that
accepted the request instead of the wildcard bind address.

The default TCP challenge port is `443`; ACME is off unless
`api.ssl.cert_mode=letsencrypt` and
`api.ssl.tcp_acme_tls_alpn_enabled=true`.

Default API files:

- `api/mlai-trade-api.sock`
- `tmp/mlai-trade-api.pid`
- `logs/mlai-trade-api.log`
- `tmp/mlai-trade-api-ssl.pid` for the remote H3 service.
- `logs/mlai-trade-api-ssl.log` for remote H3 JSONL logs.

All API responses are JSON. `mlai-trade api test` sends a local health request
through the Unix socket. `mlai-trade api status --details` prints both Unix
runtime counters and SSL/H3 runtime counters when the remote listener is
running. Use the `SSL/H3 Runtime` block for browser dashboard market-bar
cache hit/provider-fetch counters and realtime stream counters.
`mlai-trade api ssl status --details` shows only the remote listener counters.
The allowlist is visible with:

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
mlai-trade compliance tax --year 2026 --account alpaca:paper-main --details
mlai-trade compliance tax --year 2026 --quarter 1
mlai-trade compliance tax --year 2026 --quarter 1,2 --export csv
mlai-trade compliance tax --year 2026 --quarter 1-4
```

`--year` is mandatory for estimates and bracket display. `--quarter` is
optional; omit it for the year-to-date/current-year view, or pass one
contiguous quarter list/range such as `1`, `1,2`, or `1-4`. Tax estimates read
closed `auto_positions` plus provider fill activities matched FIFO, exclude
paper accounts by default, classify short-term vs long-term by holding period,
apply short-term gains as incremental ordinary income, apply long-term gains
through IRS 0%/15%/20% capital-gain brackets, and call out estimated 3.8% Net
Investment Income Tax when configured income crosses the filing-status
threshold. Results include quarter breakdowns, realized P&L by execution
origin, and detail rows with entry/exit/overall origin when `--details` is
used. Estimates are saved to `db/tax.db` with consolidated, provider, and
account scopes. CSV exports are written to `data/tax_<year>_<period>.csv`.

Use `--account alpaca:paper-main` to include a paper account for simulation.
Without an explicit paper account selector, paper positions remain excluded.

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
| `logs/archived/YYYYMMDD-*.log.gz` | Daily compressed log archives. |
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
