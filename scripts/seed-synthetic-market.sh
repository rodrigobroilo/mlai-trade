#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="${MLAI_TRADE_BIN:-${repo_root}/target/release/mlai-trade}"
start_date="${MLAI_TRADE_SYNTHETIC_START:-2020-01-02}"
days="${MLAI_TRADE_SYNTHETIC_DAYS:-1800}"

usage() {
  cat <<'USAGE'
Usage: scripts/seed-synthetic-market.sh run RUNTIME_HOME

Seed fake deterministic stock/ETF bars, macro rows, feed subscriptions,
articles, and relationships into a runtime home for local ML/API/daemon tests.
This command never places trades and does not call live providers.

Commands:
  run   Seed the synthetic fixture.
  help  Show this help.

Arguments:
  RUNTIME_HOME  Disposable or test runtime home to seed.

Environment:
  MLAI_TRADE_BIN              Binary path override
  MLAI_TRADE_SYNTHETIC_HOME   Runtime home when not passed as an argument
  MLAI_TRADE_SYNTHETIC_START  First fixture date, default: 2020-01-02
  MLAI_TRADE_SYNTHETIC_DAYS   Number of calendar days, default: 1800
USAGE
}

case "${1:-}" in
  -h | --help | help)
    usage
    exit 0
    ;;
  run | seed)
    shift || true
    home_dir="${1:-${MLAI_TRADE_SYNTHETIC_HOME:-}}"
    ;;
  "")
    usage >&2
    exit 2
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac

if [[ -z "${home_dir}" ]]; then
  usage >&2
  exit 2
fi
if [[ ! -x "${bin}" ]]; then
  echo "error: mlai-trade binary is not executable: ${bin}" >&2
  exit 127
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq is required to prepare the synthetic config" >&2
  exit 127
fi
if ! command -v sqlite3 >/dev/null 2>&1; then
  echo "error: sqlite3 is required to seed synthetic market data" >&2
  exit 127
fi

mkdir -p "${home_dir}/config"
jq '
  .daemon.enabled = true
  | .daemon.daily_refresh_enabled = false
  | .api.enabled = true
  | .feeds.sync_before_training = false
  | .feeds.sync_orders_before_training = false
  | .resources.lstm_max_sequences = 5000
  | .resources.lstm_batch_size = 64
' "${repo_root}/config/mlai-trade.example.json" >"${home_dir}/config/mlai-trade.json"
cp "${repo_root}/config/tax-brackets.example.json" "${home_dir}/config/tax-brackets.json"
chmod 600 "${home_dir}/config/mlai-trade.json" "${home_dir}/config/tax-brackets.json"

"${bin}" --home "${home_dir}" data status --json >/dev/null
db="${home_dir}/db/mlai_trade.db"

sqlite3 "${db}" <<SQL
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;

DELETE FROM bars;
DELETE FROM assets;
DELETE FROM macro_series;
DELETE FROM feed_subscriptions;
DELETE FROM news_articles;
DELETE FROM company_relationships;
DELETE FROM price_correlations;
DELETE FROM screen_results;

WITH RECURSIVE gen(i) AS (
  VALUES(1)
  UNION ALL SELECT i + 1 FROM gen WHERE i < 48
),
fixed(symbol, name, exchange, base, drift, wiggle, volume_base) AS (
  VALUES
    ('AAPL','Synthetic Apple Inc','NASDAQ',180.0,0.090,0.75,62000000),
    ('MSFT','Synthetic Microsoft Corp','NASDAQ',410.0,0.110,0.95,31000000),
    ('NVDA','Synthetic NVIDIA Corp','NASDAQ',850.0,0.260,3.20,48000000),
    ('GOOG','Synthetic Alphabet Inc','NASDAQ',150.0,0.070,0.65,24000000),
    ('META','Synthetic Meta Platforms','NASDAQ',480.0,0.060,0.80,18000000),
    ('SPY','Synthetic SPDR S&P 500 ETF','ARCA',510.0,0.055,0.42,72000000),
    ('QQQ','Synthetic Invesco QQQ ETF','NASDAQ',440.0,0.075,0.55,56000000),
    ('XLK','Synthetic Technology Select Sector ETF','ARCA',210.0,0.065,0.35,12000000),
    ('XLF','Synthetic Financial Select Sector ETF','ARCA',42.0,0.012,0.08,36000000),
    ('IWM','Synthetic Russell 2000 ETF','ARCA',205.0,0.025,0.30,29000000),
    ('SYNB','Synthetic Balanced ETF','ARCA',80.0,0.018,0.12,2100000),
    ('SYNV','Synthetic Value ETF','ARCA',55.0,0.015,0.10,1800000)
),
symbols(symbol, name, exchange, base, drift, wiggle, volume_base) AS (
  SELECT symbol, name, exchange, base, drift, wiggle, volume_base FROM fixed
  UNION ALL
  SELECT
    printf('SYM%03d', i),
    printf('Synthetic Equity %03d', i),
    CASE WHEN i % 3 = 0 THEN 'NYSE' WHEN i % 3 = 1 THEN 'NASDAQ' ELSE 'ARCA' END,
    25.0 + i * 2.5,
    0.010 + (i % 7) * 0.006,
    0.050 + (i % 11) * 0.025,
    750000 + i * 25000
  FROM gen
)
INSERT INTO assets(symbol, name, exchange, status, tradable, fractionable, shortable, updated_at)
SELECT symbol, name, exchange, 'active', 1, 1, 1, datetime('now') FROM symbols;

WITH RECURSIVE dates(n, d) AS (
  VALUES(0, date('${start_date}'))
  UNION ALL
  SELECT n + 1, date(d, '+1 day') FROM dates WHERE n < ${days}
),
gen(i) AS (
  VALUES(1)
  UNION ALL SELECT i + 1 FROM gen WHERE i < 48
),
fixed(symbol, base, drift, wiggle, volume_base, phase) AS (
  VALUES
    ('AAPL',180.0,0.090,0.75,62000000,1),
    ('MSFT',410.0,0.110,0.95,31000000,2),
    ('NVDA',850.0,0.260,3.20,48000000,3),
    ('GOOG',150.0,0.070,0.65,24000000,4),
    ('META',480.0,0.060,0.80,18000000,5),
    ('SPY',510.0,0.055,0.42,72000000,6),
    ('QQQ',440.0,0.075,0.55,56000000,7),
    ('XLK',210.0,0.065,0.35,12000000,8),
    ('XLF',42.0,0.012,0.08,36000000,9),
    ('IWM',205.0,0.025,0.30,29000000,10),
    ('SYNB',80.0,0.018,0.12,2100000,11),
    ('SYNV',55.0,0.015,0.10,1800000,12)
),
symbols(symbol, base, drift, wiggle, volume_base, phase) AS (
  SELECT symbol, base, drift, wiggle, volume_base, phase FROM fixed
  UNION ALL
  SELECT
    printf('SYM%03d', i),
    25.0 + i * 2.5,
    0.010 + (i % 7) * 0.006,
    0.050 + (i % 11) * 0.025,
    750000 + i * 25000,
    i + 12
  FROM gen
),
series AS (
  SELECT
    s.symbol,
    d.d AS date,
    ROUND(s.base + d.n * s.drift + ((d.n + s.phase) % 19 - 9) * s.wiggle + ((d.n / 23) % 5) * s.wiggle, 4) AS close,
    s.volume_base + ((d.n + s.phase) % 31) * 10000 AS volume
  FROM dates d CROSS JOIN symbols s
  WHERE strftime('%w', d.d) NOT IN ('0','6')
)
INSERT OR REPLACE INTO bars(symbol, date, open, high, low, close, volume, vwap)
SELECT
  symbol,
  date,
  ROUND(close * 0.997, 4),
  ROUND(close * 1.018, 4),
  ROUND(close * 0.982, 4),
  close,
  volume,
  ROUND(close * 1.001, 4)
FROM series;

WITH RECURSIVE dates(n, d) AS (
  VALUES(0, date('${start_date}'))
  UNION ALL
  SELECT n + 1, date(d, '+1 day') FROM dates WHERE n < ${days}
),
trading_dates AS (
  SELECT n, d FROM dates WHERE strftime('%w', d) NOT IN ('0','6')
)
INSERT OR REPLACE INTO macro_series(series_id, date, value, source, updated_at)
SELECT 'SP500', d, ROUND(5100.0 + n * 1.85 + ((n % 17) - 8) * 6.0, 4), 'synthetic', datetime('now')
FROM trading_dates
UNION ALL
SELECT 'VIXCLS', d, ROUND(18.0 + ((n % 23) - 11) * 0.18, 4), 'synthetic', datetime('now')
FROM trading_dates;

INSERT OR REPLACE INTO feed_subscriptions(symbol, cik, added_at, last_sync, subscription_source, managed)
VALUES
  ('AAPL', NULL, datetime('now'), datetime('now'), 'synthetic', 1),
  ('NVDA', NULL, datetime('now'), datetime('now'), 'synthetic', 1),
  ('SPY', NULL, datetime('now'), datetime('now'), 'synthetic', 1);

INSERT OR IGNORE INTO news_articles(source, title, url, summary, symbols, published_at, published_date, fetched_at, sentiment_score, filing_type)
VALUES
  ('synthetic', 'Synthetic positive AAPL product cycle', 'synthetic://aapl-positive', 'Fixture article', 'AAPL', datetime('now', '-2 days'), date('now', '-2 days'), datetime('now'), 0.35, NULL),
  ('synthetic', 'Synthetic NVDA infrastructure demand', 'synthetic://nvda-positive', 'Fixture article', 'NVDA', datetime('now', '-1 day'), date('now', '-1 day'), datetime('now'), 0.45, NULL),
  ('synthetic', 'Synthetic SPY macro caution', 'synthetic://spy-neutral', 'Fixture article', 'SPY', datetime('now', '-3 days'), date('now', '-3 days'), datetime('now'), -0.05, '8-K');

INSERT OR REPLACE INTO company_relationships(symbol_a, symbol_b, relationship, strength, source, discovered_at)
VALUES
  ('AAPL','XLK','sector',0.8,'synthetic',datetime('now')),
  ('NVDA','QQQ','index_weight',0.7,'synthetic',datetime('now'));
SQL

"${bin}" --home "${home_dir}" data status --json >/dev/null
echo "Synthetic market fixture seeded in ${home_dir}"
sqlite3 "${db}" "SELECT 'bars=' || COUNT(*) FROM bars; SELECT 'assets=' || COUNT(*) FROM assets; SELECT 'macro=' || COUNT(*) FROM macro_series;"
