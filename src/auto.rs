// ══════════════════════════════════════════════════════════════════
// AUTO — Autonomous Trading Engine
// ══════════════════════════════════════════════════════════════════
//
// Strategy: ML Q1 + Scanner signals → buy; stop-loss/take-profit/
// time-stop/signal-degradation → sell.
//
// Compliance (hardcoded, non-negotiable):
//   ⛔ Configured blocked symbols — user/company restricted list
//   ⛔ NO OPTIONS — stocks only
//   ⛔ Wash sale avoidance — skip symbols in 30-day statutory window + safety buffer
//   ⛔ PDT monitoring — max 3 day trades / 5 rolling days
//   ⛔ Position sizing — max 8% per position, max 10 positions
//
// Function map:
// - init_auto_tables(): creates/migrates auto-trade tracking tables.
// - sync_*(): imports provider orders/fills as source-of-truth history.
// - get_execution_price(): NBBO quote path with configured bar fallback.
// - run_auto_cycle(): runs one provider/account-safe trading decision cycle.
// - cmd_auto_*(): CLI/status/config/history entrypoints.
// ══════════════════════════════════════════════════════════════════

use crate::{alpaca, compliance, config, logging, origin, paths};
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, Utc};
use chrono_tz::Tz;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;

// ── Default strategy parameters ──────────────────────────────────

const DEF_MAX_POSITIONS: i64 = 10;
const DEF_POSITION_SIZE_PCT: f64 = 8.0; // % of equity per position
const DEF_STOP_LOSS_PCT: f64 = 7.0; // hard stop
const DEF_TAKE_PROFIT_PCT: f64 = 15.0;
const DEF_MAX_HOLD_DAYS: i64 = 5; // matches ML 5d horizon
const DEF_MIN_PRICE: f64 = 5.0;
const DEF_MIN_AVG_VOLUME: i64 = 500_000;
const DEF_MAX_SPREAD_BPS: f64 = 25.0;
const DEF_MIN_QUOTE_SIZE: f64 = 1.0;
const DEF_ALLOW_BAR_PRICE_FALLBACK: bool = true;
const DEF_BAR_FALLBACK_BPS: f64 = 50.0;
const DEF_ML_QUINTILE_BUY: i64 = 1; // only buy Q1
const DEF_ML_QUINTILE_EXIT: i64 = 4; // exit if drops to Q4+
const DEF_STOP_CONFIRMATION_ENABLED: bool = true;
const DEF_STOP_CONFIRMATION_CYCLES: i64 = 3;
const DEF_STOP_CONFIRMATION_MAX_MINUTES: i64 = 5;
const DEF_EMERGENCY_STOP_LOSS_PCT: f64 = 10.0;
const DEF_TAKE_PROFIT_CONFIRMATION_ENABLED: bool = true;
const DEF_TAKE_PROFIT_CONFIRMATION_CYCLES: i64 = 3;
const DEF_TAKE_PROFIT_MIN_HOLD_MINUTES: i64 = 5;
const DEF_TAKE_PROFIT_TRAILING_ENABLED: bool = true;
const DEF_TAKE_PROFIT_TRAILING_GIVEBACK_PCT: f64 = 3.0;
const DEF_MARKET_TIMEZONE: &str = "America/New_York";
const DEF_MARKET_OPEN: &str = "09:30:00";
const DEF_MARKET_CLOSE: &str = "16:00:00";
const ALPACA_CALENDAR_TIMEZONE: &str = "UTC";

// ── DB helpers ───────────────────────────────────────────────────

fn open_db() -> anyhow::Result<Connection> {
    let _ = paths::ensure_state_dir()?;
    let db_path = paths::scanner_db_path();
    let conn = Connection::open(&db_path)?;
    conn.execute_batch(&format!(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; {}",
        config::sqlite_runtime_pragma_sql()
    ))?;
    let _ = paths::harden_sqlite_files(&db_path);
    Ok(conn)
}

// Handles append auto log logic.
fn append_auto_log(mut event: serde_json::Value) {
    if let Some(object) = event.as_object_mut() {
        object.entry("ts".to_string()).or_insert_with(|| {
            serde_json::json!(Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string())
        });
        object
            .entry("component".to_string())
            .or_insert_with(|| serde_json::json!("auto_trade"));
    }
    let path = config::auto_log_file();
    if let Some(parent) = path.parent() {
        if let Err(err) = paths::ensure_private_dir(parent) {
            auto_stderr_log(serde_json::json!({
                "event": "auto_log_dir_create_failed",
                "level": "error",
                "dir": parent.display().to_string(),
                "error": err.to_string(),
            }));
            return;
        }
    }
    if let Err(err) = logging::rotate_if_needed(&path) {
        auto_stderr_log(serde_json::json!({
            "event": "auto_log_rotation_failed",
            "level": "error",
            "log_file": path.display().to_string(),
            "error": err.to_string(),
        }));
    }
    let line = match serde_json::to_string(&event) {
        Ok(line) => line,
        Err(err) => {
            auto_stderr_log(serde_json::json!({
                "event": "auto_log_serialization_failed",
                "level": "error",
                "error": err.to_string(),
            }));
            return;
        }
    };
    match paths::open_private_append(&path) {
        Ok(mut file) => {
            if let Err(err) = writeln!(file, "{line}") {
                auto_stderr_log(serde_json::json!({
                    "event": "auto_log_write_failed",
                    "level": "error",
                    "log_file": path.display().to_string(),
                    "error": err.to_string(),
                }));
            }
        }
        Err(err) => auto_stderr_log(serde_json::json!({
            "event": "auto_log_open_failed",
            "level": "error",
            "log_file": path.display().to_string(),
            "error": err.to_string(),
        })),
    }
}

// Handles auto-trading stderr log state.
fn auto_stderr_log(mut event: serde_json::Value) {
    if let Some(object) = event.as_object_mut() {
        object.entry("ts".to_string()).or_insert_with(|| {
            serde_json::json!(Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string())
        });
        object
            .entry("component".to_string())
            .or_insert_with(|| serde_json::json!("auto_trade"));
    }
    let line = serde_json::to_string(&event).unwrap_or_else(|err| {
        serde_json::json!({
            "ts": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "component": "auto_trade",
            "event": "log_serialization_failed",
            "level": "error",
            "error": err.to_string(),
        })
        .to_string()
    });
    eprintln!("{line}");
}

// Handles invocation source logic.
fn invocation_source(default_source: &str) -> String {
    if std::env::var("MLAI_TRADE_API_REQUEST")
        .map(|value| value == "1")
        .unwrap_or(false)
    {
        "api".to_string()
    } else {
        default_source.to_string()
    }
}

// Handles table columns database metadata.
fn table_columns(conn: &Connection, table: &str) -> anyhow::Result<HashSet<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|row| row.ok())
        .collect();
    Ok(columns)
}

// Ensures column exists or meets required invariants.
fn ensure_column(conn: &Connection, table: &str, column: &str, ddl: &str) -> anyhow::Result<()> {
    if !table_columns(conn, table)?.contains(column) {
        conn.execute_batch(&format!("ALTER TABLE {} ADD COLUMN {}", table, ddl))?;
    }
    Ok(())
}

// Ensures account columns exists or meets required invariants.
fn ensure_account_columns(conn: &Connection, table: &str) -> anyhow::Result<()> {
    ensure_column(
        conn,
        table,
        "provider",
        "provider TEXT NOT NULL DEFAULT 'alpaca'",
    )?;
    ensure_column(
        conn,
        table,
        "account_ref",
        "account_ref TEXT NOT NULL DEFAULT 'default'",
    )?;
    ensure_column(conn, table, "broker_account_id", "broker_account_id TEXT")?;
    ensure_column(
        conn,
        table,
        "account_mode",
        "account_mode TEXT NOT NULL DEFAULT 'paper'",
    )?;
    ensure_column(
        conn,
        table,
        "paper_account",
        "paper_account INTEGER NOT NULL DEFAULT 1",
    )?;
    ensure_column(
        conn,
        table,
        "market_timezone",
        "market_timezone TEXT NOT NULL DEFAULT 'UTC'",
    )?;
    ensure_column(
        conn,
        table,
        "market_session_source",
        "market_session_source TEXT",
    )?;
    ensure_column(conn, table, "provider_market", "provider_market TEXT")?;
    ensure_column(
        conn,
        table,
        "provider_core_start",
        "provider_core_start TEXT",
    )?;
    ensure_column(conn, table, "provider_core_end", "provider_core_end TEXT")?;
    Ok(())
}

// Ensures exit-confirmation columns exist for old auto-position databases.
fn ensure_auto_position_exit_columns(conn: &Connection) -> anyhow::Result<()> {
    ensure_column(
        conn,
        "auto_positions",
        "exit_order_id",
        "exit_order_id TEXT",
    )?;
    ensure_column(
        conn,
        "auto_positions",
        "entry_timestamp",
        "entry_timestamp TEXT",
    )?;
    ensure_column(
        conn,
        "auto_positions",
        "stop_loss_breach_count",
        "stop_loss_breach_count INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "auto_positions",
        "stop_loss_first_breach_at",
        "stop_loss_first_breach_at TEXT",
    )?;
    ensure_column(
        conn,
        "auto_positions",
        "take_profit_breach_count",
        "take_profit_breach_count INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "auto_positions",
        "take_profit_first_breach_at",
        "take_profit_first_breach_at TEXT",
    )?;
    ensure_column(
        conn,
        "auto_positions",
        "take_profit_peak_pct",
        "take_profit_peak_pct REAL",
    )?;
    ensure_column(
        conn,
        "auto_positions",
        "take_profit_peak_price",
        "take_profit_peak_price REAL",
    )?;
    Ok(())
}

// Handles table has id column database metadata.
fn table_has_id_column(conn: &Connection, table: &str) -> anyhow::Result<bool> {
    Ok(table_columns(conn, table)?.contains("id"))
}

// Migrates wash sale tracker to the current schema.
fn migrate_wash_sale_tracker(conn: &Connection) -> anyhow::Result<()> {
    ensure_column(
        conn,
        "wash_sale_tracker",
        "provider",
        "provider TEXT NOT NULL DEFAULT 'legacy'",
    )?;
    ensure_column(
        conn,
        "wash_sale_tracker",
        "account_ref",
        "account_ref TEXT NOT NULL DEFAULT 'default'",
    )?;
    ensure_column(
        conn,
        "wash_sale_tracker",
        "broker_account_id",
        "broker_account_id TEXT",
    )?;
    ensure_column(
        conn,
        "wash_sale_tracker",
        "paper_account",
        "paper_account INTEGER NOT NULL DEFAULT 1",
    )?;
    ensure_column(conn, "wash_sale_tracker", "sell_time", "sell_time TEXT")?;
    ensure_column(
        conn,
        "wash_sale_tracker",
        "sell_timestamp_utc",
        "sell_timestamp_utc TEXT",
    )?;
    ensure_column(
        conn,
        "wash_sale_tracker",
        "event_timezone",
        "event_timezone TEXT NOT NULL DEFAULT 'UTC'",
    )?;

    if table_has_id_column(conn, "wash_sale_tracker")? {
        return Ok(());
    }

    let backup = format!(
        "wash_sale_tracker_legacy_{}",
        Utc::now().format("%Y%m%d%H%M%S")
    );
    conn.execute_batch(&format!(
        "
        ALTER TABLE wash_sale_tracker RENAME TO {backup};
        CREATE TABLE wash_sale_tracker (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            symbol TEXT NOT NULL,
            sell_date TEXT NOT NULL,
            sell_time TEXT,
            sell_timestamp_utc TEXT,
            event_timezone TEXT NOT NULL DEFAULT 'UTC',
            sell_price REAL,
            loss_amount REAL,
            wash_window_end TEXT NOT NULL,
            status TEXT DEFAULT 'active',
            provider TEXT NOT NULL DEFAULT 'legacy',
            account_ref TEXT NOT NULL DEFAULT 'default',
            broker_account_id TEXT,
            paper_account INTEGER NOT NULL DEFAULT 1
        );
        INSERT INTO wash_sale_tracker (
            symbol, sell_date, sell_time, sell_timestamp_utc, event_timezone, sell_price,
            loss_amount, wash_window_end, status, provider, account_ref, broker_account_id,
            paper_account
        )
        SELECT symbol, sell_date, sell_time, sell_timestamp_utc, event_timezone, sell_price,
               loss_amount, wash_window_end, status, provider, account_ref, broker_account_id,
               paper_account
        FROM {backup};
        "
    ))?;
    Ok(())
}

// Migrates day trades to the current schema.
fn migrate_day_trades(conn: &Connection) -> anyhow::Result<()> {
    ensure_column(
        conn,
        "day_trades",
        "provider",
        "provider TEXT NOT NULL DEFAULT 'legacy'",
    )?;
    ensure_column(
        conn,
        "day_trades",
        "account_ref",
        "account_ref TEXT NOT NULL DEFAULT 'default'",
    )?;
    ensure_column(
        conn,
        "day_trades",
        "broker_account_id",
        "broker_account_id TEXT",
    )?;
    ensure_column(
        conn,
        "day_trades",
        "paper_account",
        "paper_account INTEGER NOT NULL DEFAULT 1",
    )?;
    ensure_column(
        conn,
        "day_trades",
        "sell_timestamp_utc",
        "sell_timestamp_utc TEXT",
    )?;
    ensure_column(
        conn,
        "day_trades",
        "event_timezone",
        "event_timezone TEXT NOT NULL DEFAULT 'UTC'",
    )?;

    if table_has_id_column(conn, "day_trades")? {
        return Ok(());
    }

    let backup = format!("day_trades_legacy_{}", Utc::now().format("%Y%m%d%H%M%S"));
    conn.execute_batch(&format!(
        "
        ALTER TABLE day_trades RENAME TO {backup};
        CREATE TABLE day_trades (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            trade_date TEXT NOT NULL,
            symbol TEXT NOT NULL,
            buy_time TEXT,
            sell_time TEXT,
            sell_timestamp_utc TEXT,
            event_timezone TEXT NOT NULL DEFAULT 'UTC',
            provider TEXT NOT NULL DEFAULT 'legacy',
            account_ref TEXT NOT NULL DEFAULT 'default',
            broker_account_id TEXT,
            paper_account INTEGER NOT NULL DEFAULT 1
        );
        INSERT INTO day_trades (
            trade_date, symbol, buy_time, sell_time, sell_timestamp_utc, event_timezone,
            provider, account_ref, broker_account_id, paper_account
        )
        SELECT trade_date, symbol, buy_time, sell_time, sell_timestamp_utc, event_timezone,
               provider, account_ref, broker_account_id, paper_account
        FROM {backup};
        "
    ))?;
    Ok(())
}

// Backfills origin labels for pre-existing provider and auto rows.
fn backfill_execution_origins(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "
        UPDATE auto_positions
        SET entry_execution_origin='mlai_auto'
        WHERE entry_execution_origin IS NULL OR entry_execution_origin='' OR entry_execution_origin='unknown';

        UPDATE auto_positions
        SET exit_order_id=(
            SELECT t.order_id
            FROM auto_trades t
            WHERE t.provider=auto_positions.provider
              AND t.account_ref=auto_positions.account_ref
              AND t.paper_account=auto_positions.paper_account
              AND t.auto_position_id=auto_positions.id
              AND t.side='sell'
              AND t.order_id IS NOT NULL
              AND t.order_id<>''
            ORDER BY t.timestamp DESC
            LIMIT 1
        )
        WHERE status='closed'
          AND (exit_order_id IS NULL OR exit_order_id='')
          AND EXISTS (
            SELECT 1 FROM auto_trades t
            WHERE t.provider=auto_positions.provider
              AND t.account_ref=auto_positions.account_ref
              AND t.paper_account=auto_positions.paper_account
              AND t.auto_position_id=auto_positions.id
              AND t.side='sell'
              AND t.order_id IS NOT NULL
              AND t.order_id<>''
          );

        UPDATE auto_positions
        SET exit_execution_origin=COALESCE((
            SELECT o.execution_origin
            FROM provider_order_snapshots o
            WHERE o.provider=auto_positions.provider
              AND o.account_ref=auto_positions.account_ref
              AND o.paper_account=auto_positions.paper_account
              AND o.order_id=auto_positions.exit_order_id
            LIMIT 1
        ), exit_execution_origin)
        WHERE status='closed'
          AND exit_order_id IS NOT NULL
          AND exit_order_id<>'';

        UPDATE auto_positions
        SET exit_execution_origin='provider_external'
        WHERE status='closed'
          AND (exit_execution_origin IS NULL OR exit_execution_origin='' OR exit_execution_origin='unknown')
          AND COALESCE(exit_reason, '') LIKE 'PROVIDER_SYNC_%';

        UPDATE auto_positions
        SET exit_execution_origin='mlai_auto'
        WHERE status='closed'
          AND (exit_execution_origin IS NULL OR exit_execution_origin='' OR exit_execution_origin='unknown');

        UPDATE auto_positions
        SET execution_origin=CASE
            WHEN status='closed'
             AND COALESCE(entry_execution_origin, 'mlai_auto') <> COALESCE(exit_execution_origin, entry_execution_origin, 'mlai_auto')
            THEN 'mixed'
            ELSE COALESCE(entry_execution_origin, 'mlai_auto')
        END
        WHERE execution_origin IS NULL
           OR execution_origin=''
           OR execution_origin='unknown'
           OR status='closed';

        UPDATE auto_trades
        SET execution_origin='mlai_auto'
        WHERE execution_origin IS NULL OR execution_origin='' OR execution_origin='unknown';

        UPDATE provider_order_snapshots
        SET execution_origin='mlai_auto'
        WHERE order_id IN (
            SELECT order_id FROM auto_trades WHERE order_id IS NOT NULL AND order_id<>''
        );

        UPDATE provider_order_snapshots
        SET execution_origin='mlai_auto'
        WHERE COALESCE(client_order_id, '') LIKE 'mlai-auto-%';

        UPDATE provider_order_snapshots
        SET execution_origin='mlai_cli'
        WHERE COALESCE(client_order_id, '') LIKE 'mlai-cli-%'
           OR COALESCE(client_order_id, '') LIKE 'plm-%'
           OR order_id IN (
                SELECT order_id FROM order_execution_origins
                WHERE execution_origin='mlai_cli'
           );

        UPDATE provider_order_snapshots
        SET execution_origin='provider_external'
        WHERE execution_origin IS NULL OR execution_origin='' OR execution_origin='unknown';

        UPDATE provider_fill_activities
        SET execution_origin=COALESCE((
            SELECT o.execution_origin
            FROM provider_order_snapshots o
            WHERE o.provider=provider_fill_activities.provider
              AND o.account_ref=provider_fill_activities.account_ref
              AND o.paper_account=provider_fill_activities.paper_account
              AND o.order_id=provider_fill_activities.order_id
            LIMIT 1
        ), execution_origin);

        UPDATE provider_fill_activities
        SET execution_origin='mlai_auto'
        WHERE order_id IN (
            SELECT order_id FROM auto_trades WHERE order_id IS NOT NULL AND order_id<>''
        );

        UPDATE provider_fill_activities
        SET execution_origin='provider_external'
        WHERE execution_origin IS NULL OR execution_origin='' OR execution_origin='unknown';

        UPDATE auto_positions
        SET exit_execution_origin=COALESCE((
            SELECT o.execution_origin
            FROM provider_order_snapshots o
            WHERE o.provider=auto_positions.provider
              AND o.account_ref=auto_positions.account_ref
              AND o.paper_account=auto_positions.paper_account
              AND o.order_id=auto_positions.exit_order_id
            LIMIT 1
        ), exit_execution_origin)
        WHERE status='closed'
          AND exit_order_id IS NOT NULL
          AND exit_order_id<>'';

        UPDATE auto_positions
        SET execution_origin=CASE
            WHEN status='closed'
             AND COALESCE(entry_execution_origin, 'mlai_auto') <> COALESCE(exit_execution_origin, entry_execution_origin, 'mlai_auto')
            THEN 'mixed'
            ELSE COALESCE(entry_execution_origin, 'mlai_auto')
        END
        WHERE status='closed';

        INSERT INTO auto_positions (
            provider, account_ref, broker_account_id, account_mode, paper_account,
            market_timezone, market_session_source, provider_market, provider_core_start,
            provider_core_end, symbol, entry_date, entry_timestamp, entry_price, shares,
            cost_basis, stop_loss_price, take_profit_price, exit_by_date, ml_quintile,
            ml_score, suggest_score, entry_signals, status, exit_date, exit_price,
            exit_reason, pnl, pnl_pct, order_id, exit_order_id,
            entry_execution_origin, exit_execution_origin, execution_origin
        )
        SELECT p.provider, p.account_ref, p.broker_account_id, p.account_mode, p.paper_account,
               p.market_timezone, p.market_session_source, p.provider_market, p.provider_core_start,
               p.provider_core_end, p.symbol, p.entry_date, p.entry_timestamp, p.entry_price,
               CAST(ROUND(s.qty) AS INTEGER), p.entry_price * s.qty, p.stop_loss_price,
               p.take_profit_price, p.exit_by_date, p.ml_quintile, p.ml_score, p.suggest_score,
               p.entry_signals, 'closed', substr(s.exit_ts, 1, 10), s.exit_price,
               'PROVIDER_SYNC_PARTIAL (provider reports fewer shares)',
               (s.exit_price - p.entry_price) * s.qty,
               ((s.exit_price / p.entry_price) - 1.0) * 100.0,
               p.order_id, s.exit_key, COALESCE(p.entry_execution_origin, 'mlai_auto'),
               s.exit_execution_origin,
               CASE
                 WHEN COALESCE(p.entry_execution_origin, 'mlai_auto') <> s.exit_execution_origin
                 THEN 'mixed'
                 ELSE COALESCE(p.entry_execution_origin, 'mlai_auto')
               END
        FROM auto_positions p
        JOIN (
            SELECT provider, account_ref, paper_account, UPPER(symbol) AS symbol,
                   COALESCE(NULLIF(order_id, ''), activity_id) AS exit_key,
                   MAX(COALESCE(transaction_time, synced_at_utc)) AS exit_ts,
                   CASE
                     WHEN SUM(COALESCE(qty, 0.0)) > 0.0
                     THEN SUM(COALESCE(price, 0.0) * COALESCE(qty, 0.0)) / SUM(COALESCE(qty, 0.0))
                     ELSE MAX(COALESCE(price, 0.0))
                   END AS exit_price,
                   SUM(COALESCE(qty, 0.0)) AS qty,
                   COALESCE(MAX(execution_origin), 'provider_external') AS exit_execution_origin
            FROM provider_fill_activities
            WHERE UPPER(COALESCE(side, ''))='SELL'
              AND COALESCE(execution_origin, 'provider_external') <> 'mlai_auto'
            GROUP BY provider, account_ref, paper_account, UPPER(symbol),
                     COALESCE(NULLIF(order_id, ''), activity_id)
        ) s
          ON s.provider=p.provider
         AND s.account_ref=p.account_ref
         AND s.paper_account=p.paper_account
         AND s.symbol=UPPER(p.symbol)
        WHERE p.status='open'
          AND s.qty > 0.0
          AND s.exit_price > 0.0
          AND COALESCE(p.entry_timestamp, p.entry_date || 'T00:00:00Z') <= s.exit_ts
          AND NOT EXISTS (
              SELECT 1 FROM auto_positions existing
              WHERE existing.provider=p.provider
                AND existing.account_ref=p.account_ref
                AND existing.paper_account=p.paper_account
                AND existing.exit_order_id=s.exit_key
          );
        ",
    )?;
    Ok(())
}

// Initializes auto tables tables or runtime state.
pub fn init_auto_tables(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS auto_positions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            provider TEXT NOT NULL DEFAULT 'alpaca',
            account_ref TEXT NOT NULL DEFAULT 'default',
            broker_account_id TEXT,
            account_mode TEXT NOT NULL DEFAULT 'paper',
            paper_account INTEGER NOT NULL DEFAULT 1,
            market_timezone TEXT NOT NULL DEFAULT 'UTC',
            market_session_source TEXT,
            provider_market TEXT,
            provider_core_start TEXT,
            provider_core_end TEXT,
            symbol TEXT NOT NULL,
            entry_date TEXT NOT NULL,
            entry_price REAL NOT NULL,
            shares INTEGER NOT NULL,
            cost_basis REAL NOT NULL,
            stop_loss_price REAL,
            take_profit_price REAL,
            exit_by_date TEXT,
            ml_quintile INTEGER,
            ml_score REAL,
            suggest_score INTEGER,
            entry_signals TEXT,
            status TEXT DEFAULT 'open',
            exit_date TEXT,
            exit_price REAL,
            exit_reason TEXT,
            pnl REAL,
            pnl_pct REAL,
            order_id TEXT,
            exit_order_id TEXT,
            entry_timestamp TEXT,
            stop_loss_breach_count INTEGER NOT NULL DEFAULT 0,
            stop_loss_first_breach_at TEXT,
            take_profit_breach_count INTEGER NOT NULL DEFAULT 0,
            take_profit_first_breach_at TEXT,
            take_profit_peak_pct REAL,
            take_profit_peak_price REAL,
            entry_execution_origin TEXT NOT NULL DEFAULT 'mlai_auto',
            exit_execution_origin TEXT,
            execution_origin TEXT NOT NULL DEFAULT 'mlai_auto'
        );
        CREATE TABLE IF NOT EXISTS auto_trades (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            provider TEXT NOT NULL DEFAULT 'alpaca',
            account_ref TEXT NOT NULL DEFAULT 'default',
            broker_account_id TEXT,
            account_mode TEXT NOT NULL DEFAULT 'paper',
            paper_account INTEGER NOT NULL DEFAULT 1,
            market_timezone TEXT NOT NULL DEFAULT 'UTC',
            market_session_source TEXT,
            provider_market TEXT,
            provider_core_start TEXT,
            provider_core_end TEXT,
            timestamp TEXT NOT NULL,
            symbol TEXT NOT NULL,
            side TEXT NOT NULL,
            shares INTEGER NOT NULL,
            price REAL,
            order_id TEXT,
            reason TEXT,
            auto_position_id INTEGER,
            execution_origin TEXT NOT NULL DEFAULT 'mlai_auto'
        );
        CREATE TABLE IF NOT EXISTS auto_config (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS wash_sale_tracker (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            symbol TEXT NOT NULL,
            sell_date TEXT NOT NULL,
            sell_time TEXT,
            sell_timestamp_utc TEXT,
            event_timezone TEXT NOT NULL DEFAULT 'UTC',
            sell_price REAL,
            loss_amount REAL,
            wash_window_end TEXT NOT NULL,
            status TEXT DEFAULT 'active',
            provider TEXT NOT NULL DEFAULT 'legacy',
            account_ref TEXT NOT NULL DEFAULT 'default',
            broker_account_id TEXT,
            paper_account INTEGER NOT NULL DEFAULT 1
        );
        CREATE TABLE IF NOT EXISTS day_trades (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            trade_date TEXT NOT NULL,
            symbol TEXT NOT NULL,
            buy_time TEXT,
            sell_time TEXT,
            sell_timestamp_utc TEXT,
            event_timezone TEXT NOT NULL DEFAULT 'UTC',
            provider TEXT NOT NULL DEFAULT 'legacy',
            account_ref TEXT NOT NULL DEFAULT 'default',
            broker_account_id TEXT,
            paper_account INTEGER NOT NULL DEFAULT 1
        );
        CREATE TABLE IF NOT EXISTS provider_order_snapshots (
            provider TEXT NOT NULL,
            account_ref TEXT NOT NULL,
            broker_account_id TEXT,
            account_mode TEXT NOT NULL,
            paper_account INTEGER NOT NULL,
            order_id TEXT NOT NULL,
            client_order_id TEXT,
            symbol TEXT,
            side TEXT,
            order_type TEXT,
            time_in_force TEXT,
            status TEXT,
            qty REAL,
            filled_qty REAL,
            limit_price REAL,
            stop_price REAL,
            filled_avg_price REAL,
            submitted_at TEXT,
            filled_at TEXT,
            canceled_at TEXT,
            expired_at TEXT,
            replaced_at TEXT,
            updated_at TEXT,
            execution_origin TEXT NOT NULL DEFAULT 'unknown',
            synced_at_utc TEXT NOT NULL,
            raw_json TEXT NOT NULL,
            PRIMARY KEY (provider, account_ref, paper_account, order_id)
        );
        CREATE TABLE IF NOT EXISTS provider_fill_activities (
            provider TEXT NOT NULL,
            account_ref TEXT NOT NULL,
            broker_account_id TEXT,
            account_mode TEXT NOT NULL,
            paper_account INTEGER NOT NULL,
            activity_id TEXT NOT NULL,
            order_id TEXT,
            symbol TEXT,
            side TEXT,
            qty REAL,
            price REAL,
            cum_qty REAL,
            leaves_qty REAL,
            activity_type TEXT,
            transaction_time TEXT,
            execution_origin TEXT NOT NULL DEFAULT 'unknown',
            synced_at_utc TEXT NOT NULL,
            raw_json TEXT NOT NULL,
            PRIMARY KEY (provider, account_ref, paper_account, activity_id)
        );
        CREATE TABLE IF NOT EXISTS provider_account_snapshots (
            provider TEXT NOT NULL,
            account_ref TEXT NOT NULL,
            broker_account_id TEXT,
            account_mode TEXT NOT NULL,
            paper_account INTEGER NOT NULL,
            equity REAL,
            cash REAL,
            buying_power REAL,
            portfolio_value REAL,
            synced_at_utc TEXT NOT NULL,
            raw_json TEXT NOT NULL,
            PRIMARY KEY (provider, account_ref, paper_account)
        );
        CREATE TABLE IF NOT EXISTS provider_position_snapshots (
            provider TEXT NOT NULL,
            account_ref TEXT NOT NULL,
            broker_account_id TEXT,
            account_mode TEXT NOT NULL,
            paper_account INTEGER NOT NULL,
            symbol TEXT NOT NULL,
            qty REAL,
            avg_entry_price REAL,
            current_price REAL,
            market_value REAL,
            unrealized_pl REAL,
            unrealized_plpc REAL,
            asset_class TEXT,
            exchange TEXT,
            side TEXT,
            synced_at_utc TEXT NOT NULL,
            raw_json TEXT NOT NULL,
            PRIMARY KEY (provider, account_ref, paper_account, symbol)
        );
        CREATE TABLE IF NOT EXISTS position_management_overrides (
            provider TEXT NOT NULL,
            account_ref TEXT NOT NULL,
            broker_account_id TEXT,
            account_mode TEXT NOT NULL,
            paper_account INTEGER NOT NULL,
            symbol TEXT NOT NULL,
            management_origin TEXT NOT NULL,
            auto_managed INTEGER NOT NULL DEFAULT 0,
            reason TEXT,
            updated_at_utc TEXT NOT NULL,
            PRIMARY KEY (provider, account_ref, paper_account, symbol)
        );
        CREATE INDEX IF NOT EXISTS idx_auto_pos_status ON auto_positions(status);
        CREATE INDEX IF NOT EXISTS idx_auto_pos_symbol ON auto_positions(symbol);
    ",
    )?;
    origin::init_tables(conn)?;
    ensure_account_columns(conn, "auto_positions")?;
    ensure_account_columns(conn, "auto_trades")?;
    ensure_auto_position_exit_columns(conn)?;
    ensure_column(
        conn,
        "auto_positions",
        "entry_execution_origin",
        "entry_execution_origin TEXT NOT NULL DEFAULT 'mlai_auto'",
    )?;
    ensure_column(
        conn,
        "auto_positions",
        "exit_execution_origin",
        "exit_execution_origin TEXT",
    )?;
    ensure_column(
        conn,
        "auto_positions",
        "execution_origin",
        "execution_origin TEXT NOT NULL DEFAULT 'mlai_auto'",
    )?;
    ensure_column(
        conn,
        "auto_trades",
        "execution_origin",
        "execution_origin TEXT NOT NULL DEFAULT 'mlai_auto'",
    )?;
    ensure_column(
        conn,
        "provider_order_snapshots",
        "execution_origin",
        "execution_origin TEXT NOT NULL DEFAULT 'unknown'",
    )?;
    ensure_column(
        conn,
        "provider_fill_activities",
        "execution_origin",
        "execution_origin TEXT NOT NULL DEFAULT 'unknown'",
    )?;
    migrate_wash_sale_tracker(conn)?;
    migrate_day_trades(conn)?;
    backfill_execution_origins(conn)?;
    conn.execute_batch(
        "
        CREATE INDEX IF NOT EXISTS idx_auto_pos_account_status ON auto_positions(provider, account_ref, paper_account, status);
        CREATE INDEX IF NOT EXISTS idx_auto_trades_account_date ON auto_trades(provider, account_ref, paper_account, timestamp);
        CREATE INDEX IF NOT EXISTS idx_wash_sale_active_real ON wash_sale_tracker(symbol, status, wash_window_end, paper_account);
        CREATE INDEX IF NOT EXISTS idx_day_trades_account_date ON day_trades(provider, account_ref, paper_account, trade_date);
        CREATE INDEX IF NOT EXISTS idx_provider_orders_account_time ON provider_order_snapshots(provider, account_ref, paper_account, submitted_at);
        CREATE INDEX IF NOT EXISTS idx_provider_fills_account_time ON provider_fill_activities(provider, account_ref, paper_account, transaction_time);
        CREATE INDEX IF NOT EXISTS idx_provider_fills_tax_time ON provider_fill_activities(paper_account, transaction_time, activity_id);
        CREATE INDEX IF NOT EXISTS idx_provider_account_snapshots_account ON provider_account_snapshots(provider, account_ref, paper_account, synced_at_utc);
        CREATE INDEX IF NOT EXISTS idx_provider_position_snapshots_account ON provider_position_snapshots(provider, account_ref, paper_account, symbol);
        CREATE INDEX IF NOT EXISTS idx_position_management_overrides_account ON position_management_overrides(provider, account_ref, paper_account, symbol, auto_managed);
        CREATE INDEX IF NOT EXISTS idx_wash_sale_universe_event ON wash_sale_tracker(paper_account, symbol, sell_timestamp_utc, sell_price);
        ",
    )?;
    Ok(())
}

// ── Config read/write ────────────────────────────────────────────

fn get_config(conn: &Connection, key: &str, default: &str) -> String {
    conn.query_row(
        "SELECT value FROM auto_config WHERE key=?1",
        params![key],
        |r| r.get::<_, String>(0),
    )
    .unwrap_or_else(|_| default.into())
}

// Returns configured auto with defaults applied.
fn configured_auto() -> config::AutoConfig {
    config::load().map(|config| config.auto).unwrap_or_default()
}

// Returns config f64 from config, storage, or provider data.
fn get_config_f64(conn: &Connection, key: &str, default: f64) -> f64 {
    get_config(conn, key, &default.to_string())
        .parse()
        .unwrap_or(default)
}

// Returns config i64 from config, storage, or provider data.
fn get_config_i64(conn: &Connection, key: &str, default: i64) -> i64 {
    get_config(conn, key, &default.to_string())
        .parse()
        .unwrap_or(default)
}

// Returns config bool from config, storage, or provider data.
fn get_config_bool(conn: &Connection, key: &str, default: bool) -> bool {
    match get_config(conn, key, if default { "true" } else { "false" })
        .to_ascii_lowercase()
        .as_str()
    {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default,
    }
}

// Handles auto-trading f64 state.
fn auto_f64(conn: &Connection, key: &str, file_value: Option<f64>, default: f64) -> f64 {
    get_config_f64(conn, key, file_value.unwrap_or(default))
}

// Handles auto-trading i64 state.
fn auto_i64(conn: &Connection, key: &str, file_value: Option<i64>, default: i64) -> i64 {
    get_config_i64(conn, key, file_value.unwrap_or(default))
}

// Handles auto-trading bool state.
fn auto_bool(conn: &Connection, key: &str, file_value: Option<bool>, default: bool) -> bool {
    get_config_bool(conn, key, file_value.unwrap_or(default))
}

// Sets config in local state.
fn set_config(conn: &Connection, key: &str, value: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO auto_config (key, value) VALUES (?1, ?2)",
        params![key, value],
    )?;
    Ok(())
}

// Returns whether enabled is true.
fn is_enabled(conn: &Connection) -> bool {
    get_config_bool(conn, "enabled", configured_auto().enabled.unwrap_or(true))
}

// Handles auto-trading trading enabled state.
pub fn auto_trading_enabled() -> anyhow::Result<bool> {
    let conn = open_db()?;
    init_auto_tables(&conn)?;
    Ok(is_enabled(&conn))
}

struct StrategyConfig {
    max_positions: i64,
    position_size_pct: f64,
    stop_loss_pct: f64,
    take_profit_pct: f64,
    stop_loss_confirmation: StopLossConfirmation,
    take_profit_confirmation: TakeProfitConfirmation,
    max_hold_days: i64,
    min_price: f64,
    min_avg_volume: i64,
    max_spread_bps: f64,
    min_quote_size: f64,
    allow_bar_price_fallback: bool,
    bar_fallback_bps: f64,
    ml_quintile_buy: i64,
    ml_quintile_exit: i64,
    wash_sale_safety_buffer_days: i64,
}

#[derive(Debug, Clone)]
struct StopLossConfirmation {
    enabled: bool,
    cycles: i64,
    max_confirmation_minutes: i64,
    emergency_stop_loss_pct: f64,
}

#[derive(Debug, Clone)]
struct TakeProfitConfirmation {
    enabled: bool,
    cycles: i64,
    min_hold_minutes: i64,
    trailing_enabled: bool,
    trailing_giveback_pct: f64,
}

#[derive(Debug, Clone, Default)]
struct ExitConfirmationState {
    stop_loss_breach_count: i64,
    stop_loss_first_breach_at: Option<String>,
    take_profit_breach_count: i64,
    take_profit_first_breach_at: Option<String>,
    take_profit_peak_pct: Option<f64>,
    take_profit_peak_price: Option<f64>,
}

#[derive(Debug, Clone)]
struct OpenAutoPosition {
    id: i64,
    symbol: String,
    entry_date: String,
    entry_timestamp: Option<String>,
    entry_price: f64,
    shares: i64,
    cost_basis: f64,
    stop_loss: Option<f64>,
    take_profit: Option<f64>,
    exit_by: Option<String>,
    entry_execution_origin: origin::ExecutionOrigin,
    confirmation: ExitConfirmationState,
}

#[derive(Debug, Clone)]
struct ProviderExitFill {
    timestamp: String,
    price: f64,
    qty: f64,
    order_id: Option<String>,
    execution_origin: origin::ExecutionOrigin,
}

#[derive(Debug, Clone)]
struct ExitConfirmationDecision {
    reason: Option<String>,
    state: ExitConfirmationState,
    note: Option<String>,
    rule: Option<String>,
    cycles_remaining: Option<i64>,
    minutes_remaining: Option<i64>,
}

#[derive(Debug, Clone)]
struct MarketSchedule {
    require_local_clock: bool,
    use_provider_clock: bool,
    use_provider_calendar: bool,
    allow_local_clock_fallback: bool,
    timezone_name: String,
    timezone: Tz,
    provider_markets: Vec<String>,
    regular_open: NaiveTime,
    regular_close: NaiveTime,
    buy_start: NaiveTime,
    buy_end: NaiveTime,
    sell_start: NaiveTime,
    sell_end: NaiveTime,
    closed_dates: HashSet<String>,
}

#[derive(Debug, Clone, Copy)]
enum TradePhase {
    Buy,
    Sell,
}

// Loads config from storage or configuration.
fn load_config(conn: &Connection) -> StrategyConfig {
    let file_cfg = configured_auto();
    let file_wash_sale_safety_buffer_days =
        compliance::wash_sale_safety_buffer_days(file_cfg.compliance.wash_sale_safety_buffer_days);
    StrategyConfig {
        max_positions: auto_i64(
            conn,
            "max_positions",
            file_cfg.max_positions,
            DEF_MAX_POSITIONS,
        ),
        position_size_pct: auto_f64(
            conn,
            "position_size_pct",
            file_cfg.position_size_pct,
            DEF_POSITION_SIZE_PCT,
        ),
        stop_loss_pct: auto_f64(
            conn,
            "stop_loss_pct",
            file_cfg.stop_loss_pct,
            DEF_STOP_LOSS_PCT,
        ),
        take_profit_pct: auto_f64(
            conn,
            "take_profit_pct",
            file_cfg.take_profit_pct,
            DEF_TAKE_PROFIT_PCT,
        ),
        stop_loss_confirmation: StopLossConfirmation {
            enabled: auto_bool(
                conn,
                "stop_loss_confirmation_enabled",
                file_cfg.stop_loss_confirmation.enabled,
                DEF_STOP_CONFIRMATION_ENABLED,
            ),
            cycles: auto_i64(
                conn,
                "stop_loss_confirmation_cycles",
                file_cfg.stop_loss_confirmation.cycles,
                DEF_STOP_CONFIRMATION_CYCLES,
            )
            .clamp(1, 60),
            max_confirmation_minutes: auto_i64(
                conn,
                "stop_loss_confirmation_max_confirmation_minutes",
                file_cfg.stop_loss_confirmation.max_confirmation_minutes,
                DEF_STOP_CONFIRMATION_MAX_MINUTES,
            )
            .clamp(0, 390),
            emergency_stop_loss_pct: auto_f64(
                conn,
                "stop_loss_confirmation_emergency_stop_loss_pct",
                file_cfg.stop_loss_confirmation.emergency_stop_loss_pct,
                DEF_EMERGENCY_STOP_LOSS_PCT,
            )
            .clamp(0.0, 100.0),
        },
        take_profit_confirmation: TakeProfitConfirmation {
            enabled: auto_bool(
                conn,
                "take_profit_confirmation_enabled",
                file_cfg.take_profit_confirmation.enabled,
                DEF_TAKE_PROFIT_CONFIRMATION_ENABLED,
            ),
            cycles: auto_i64(
                conn,
                "take_profit_confirmation_cycles",
                file_cfg.take_profit_confirmation.cycles,
                DEF_TAKE_PROFIT_CONFIRMATION_CYCLES,
            )
            .clamp(1, 60),
            min_hold_minutes: auto_i64(
                conn,
                "take_profit_confirmation_min_hold_minutes",
                file_cfg.take_profit_confirmation.min_hold_minutes,
                DEF_TAKE_PROFIT_MIN_HOLD_MINUTES,
            )
            .clamp(0, 390),
            trailing_enabled: auto_bool(
                conn,
                "take_profit_confirmation_trailing_enabled",
                file_cfg.take_profit_confirmation.trailing_enabled,
                DEF_TAKE_PROFIT_TRAILING_ENABLED,
            ),
            trailing_giveback_pct: auto_f64(
                conn,
                "take_profit_confirmation_trailing_giveback_pct",
                file_cfg.take_profit_confirmation.trailing_giveback_pct,
                DEF_TAKE_PROFIT_TRAILING_GIVEBACK_PCT,
            )
            .clamp(0.0, 100.0),
        },
        max_hold_days: auto_i64(
            conn,
            "max_hold_days",
            file_cfg.max_hold_days,
            DEF_MAX_HOLD_DAYS,
        ),
        min_price: auto_f64(conn, "min_price", file_cfg.min_price, DEF_MIN_PRICE),
        min_avg_volume: auto_i64(
            conn,
            "min_avg_volume",
            file_cfg.min_avg_volume,
            DEF_MIN_AVG_VOLUME,
        ),
        max_spread_bps: auto_f64(
            conn,
            "max_spread_bps",
            file_cfg.max_spread_bps,
            DEF_MAX_SPREAD_BPS,
        ),
        min_quote_size: auto_f64(
            conn,
            "min_quote_size",
            file_cfg.min_quote_size,
            DEF_MIN_QUOTE_SIZE,
        ),
        bar_fallback_bps: auto_f64(
            conn,
            "bar_fallback_bps",
            file_cfg.bar_fallback_bps,
            DEF_BAR_FALLBACK_BPS,
        ),
        ml_quintile_buy: auto_i64(
            conn,
            "ml_quintile_buy",
            file_cfg.ml_quintile_buy,
            DEF_ML_QUINTILE_BUY,
        ),
        ml_quintile_exit: auto_i64(
            conn,
            "ml_quintile_exit",
            file_cfg.ml_quintile_exit,
            DEF_ML_QUINTILE_EXIT,
        ),
        allow_bar_price_fallback: auto_bool(
            conn,
            "allow_bar_price_fallback",
            file_cfg.allow_bar_price_fallback,
            DEF_ALLOW_BAR_PRICE_FALLBACK,
        ),
        wash_sale_safety_buffer_days: compliance::wash_sale_safety_buffer_days(Some(auto_i64(
            conn,
            "wash_sale_safety_buffer_days",
            Some(file_wash_sale_safety_buffer_days),
            file_wash_sale_safety_buffer_days,
        ))),
    }
}

// Parses market time from user or provider input.
fn parse_market_time(
    value: Option<String>,
    default: &str,
    field: &str,
) -> anyhow::Result<NaiveTime> {
    let value = value.unwrap_or_else(|| default.to_string());
    NaiveTime::parse_from_str(&value, "%H:%M:%S")
        .or_else(|_| NaiveTime::parse_from_str(&value, "%H:%M"))
        .map_err(|err| anyhow::anyhow!("invalid auto.market.{}='{}': {}", field, value, err))
}

// Loads market schedule from storage or configuration.
fn load_market_schedule() -> anyhow::Result<MarketSchedule> {
    let market = config::load()
        .map(|config| config.auto.market)
        .unwrap_or_default();
    let mode = market
        .mode
        .as_deref()
        .unwrap_or("auto")
        .trim()
        .to_ascii_lowercase();
    let timezone_name = market
        .timezone
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEF_MARKET_TIMEZONE.to_string());
    let timezone = timezone_name.parse::<Tz>().map_err(|err| {
        anyhow::anyhow!("invalid auto.market.timezone='{}': {}", timezone_name, err)
    })?;
    let regular_open = parse_market_time(market.regular_open, DEF_MARKET_OPEN, "regular_open")?;
    let regular_close = parse_market_time(market.regular_close, DEF_MARKET_CLOSE, "regular_close")?;
    let buy_start = parse_market_time(market.buy_start, DEF_MARKET_OPEN, "buy_start")?;
    let buy_end = parse_market_time(market.buy_end, DEF_MARKET_CLOSE, "buy_end")?;
    let sell_start = parse_market_time(market.sell_start, DEF_MARKET_OPEN, "sell_start")?;
    let sell_end = parse_market_time(market.sell_end, DEF_MARKET_CLOSE, "sell_end")?;
    Ok(MarketSchedule {
        require_local_clock: market
            .require_local_clock
            .unwrap_or_else(|| !matches!(mode.as_str(), "provider")),
        use_provider_clock: market
            .use_provider_clock
            .unwrap_or_else(|| !matches!(mode.as_str(), "local")),
        use_provider_calendar: market
            .use_provider_calendar
            .unwrap_or_else(|| matches!(mode.as_str(), "auto" | "provider")),
        allow_local_clock_fallback: market
            .allow_local_clock_fallback
            .unwrap_or_else(|| matches!(mode.as_str(), "auto" | "local")),
        timezone_name,
        timezone,
        provider_markets: if market.provider_markets.is_empty() {
            vec!["NYSE".to_string(), "NASDAQ".to_string()]
        } else {
            market.provider_markets
        },
        regular_open,
        regular_close,
        buy_start,
        buy_end,
        sell_start,
        sell_end,
        closed_dates: market.closed_dates.into_iter().collect(),
    })
}

// Handles time in window logic.
fn time_in_window(now: NaiveTime, start: NaiveTime, end: NaiveTime) -> bool {
    if start <= end {
        now >= start && now <= end
    } else {
        now >= start || now <= end
    }
}

// Handles local market session block logic.
fn local_market_session_block(schedule: &MarketSchedule) -> Option<String> {
    let now = Utc::now().with_timezone(&schedule.timezone);
    let date = now.date_naive();
    let date_str = date.format("%Y-%m-%d").to_string();
    if schedule.closed_dates.contains(&date_str) {
        return Some(format!(
            "local market calendar marks {} closed in {}",
            date_str, schedule.timezone_name
        ));
    }
    let weekday = date.weekday();
    if weekday == chrono::Weekday::Sat || weekday == chrono::Weekday::Sun {
        return Some(format!(
            "local market calendar is closed on weekend {} in {}",
            date_str, schedule.timezone_name
        ));
    }
    let time = now.time();
    if !time_in_window(time, schedule.regular_open, schedule.regular_close) {
        return Some(format!(
            "outside configured regular market hours {}-{} {}",
            schedule.regular_open.format("%H:%M:%S"),
            schedule.regular_close.format("%H:%M:%S"),
            schedule.timezone_name
        ));
    }
    None
}

// Handles local market block logic.
fn local_market_block(schedule: &MarketSchedule, phase: TradePhase) -> Option<String> {
    if let Some(reason) = local_market_session_block(schedule) {
        return Some(reason);
    }
    let now = Utc::now().with_timezone(&schedule.timezone);
    let time = now.time();
    let (start, end, label) = match phase {
        TradePhase::Buy => (schedule.buy_start, schedule.buy_end, "buy"),
        TradePhase::Sell => (schedule.sell_start, schedule.sell_end, "sell"),
    };
    if !time_in_window(time, start, end) {
        return Some(format!(
            "outside configured {} window {}-{} {}",
            label,
            start.format("%H:%M:%S"),
            end.format("%H:%M:%S"),
            schedule.timezone_name
        ));
    }
    None
}

#[derive(Debug, Clone, Default)]
struct ProviderSession {
    market: String,
    core_start: String,
    core_end: String,
}

impl ProviderSession {
    // Handles market logic.
    fn market(&self) -> Option<&str> {
        if self.market.is_empty() {
            None
        } else {
            Some(self.market.as_str())
        }
    }

    // Handles core start logic.
    fn core_start(&self) -> Option<&str> {
        if self.core_start.is_empty() {
            None
        } else {
            Some(self.core_start.as_str())
        }
    }

    // Handles core end logic.
    fn core_end(&self) -> Option<&str> {
        if self.core_end.is_empty() {
            None
        } else {
            Some(self.core_end.as_str())
        }
    }
}

// Returns provider clock block state.
async fn provider_clock_block(
    client: &reqwest::Client,
    account: &config::AlpacaAccount,
    schedule: &MarketSchedule,
) -> anyhow::Result<Option<String>> {
    let url = alpaca::clock_v3_url_for(account, &schedule.provider_markets);
    let clock: alpaca::TradingClockResponse = api_get(client, &url).await?;
    if clock.clocks.is_empty() {
        anyhow::bail!("provider clock returned no market clocks");
    }
    let open_markets = clock
        .clocks
        .iter()
        .filter(|clock| clock.may_be_trading())
        .map(|clock| clock.market_label())
        .collect::<Vec<_>>();
    if open_markets.is_empty() {
        let details = clock
            .clocks
            .iter()
            .map(|clock| {
                format!(
                    "{} phase={} next_open={}",
                    clock.market_label(),
                    clock.phase.as_deref().unwrap_or("?"),
                    clock.next_market_open.as_deref().unwrap_or("?")
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Ok(Some(format!("provider reports market closed: {}", details)));
    }
    Ok(None)
}

// Returns provider calendar session state.
async fn provider_calendar_session(
    client: &reqwest::Client,
    account: &config::AlpacaAccount,
    schedule: &MarketSchedule,
) -> anyhow::Result<Option<ProviderSession>> {
    let today = Utc::now().format("%Y-%m-%d").to_string();
    for market in &schedule.provider_markets {
        let url =
            alpaca::calendar_v3_url_for(account, market, &today, &today, ALPACA_CALENDAR_TIMEZONE);
        let response: alpaca::TradingCalendarResponse = api_get(client, &url).await?;
        if let Some(day) = response.calendar.into_iter().find(|day| day.date == today) {
            if let (Some(core_start), Some(core_end)) = (day.core_start, day.core_end) {
                return Ok(Some(ProviderSession {
                    market: market.clone(),
                    core_start,
                    core_end,
                }));
            }
        }
    }
    Ok(None)
}

// Returns provider calendar gate state.
async fn provider_calendar_gate(
    client: &reqwest::Client,
    account: &config::AlpacaAccount,
    schedule: &MarketSchedule,
) -> anyhow::Result<Result<ProviderSession, String>> {
    let Some(session) = provider_calendar_session(client, account, schedule).await? else {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        return Ok(Err(format!(
            "provider calendar has no core session for {} in UTC",
            today
        )));
    };
    let now = Utc::now();
    let core_start = chrono::DateTime::parse_from_rfc3339(&session.core_start)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|err| {
            anyhow::anyhow!(
                "provider calendar returned invalid core_start '{}': {}",
                session.core_start,
                err
            )
        })?;
    let core_end = chrono::DateTime::parse_from_rfc3339(&session.core_end)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|err| {
            anyhow::anyhow!(
                "provider calendar returned invalid core_end '{}': {}",
                session.core_end,
                err
            )
        })?;
    if now < core_start || now > core_end {
        return Ok(Err(format!(
            "outside provider {} core session {} - {} UTC",
            session.market, session.core_start, session.core_end
        )));
    }
    Ok(Ok(session))
}

// ── Alpaca API helpers ───────────────────────────────────────────

fn safe_account_ref(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
        .take(18)
        .collect();
    if cleaned.is_empty() {
        "default".to_string()
    } else {
        cleaned
    }
}

// Handles paper flag logic.
fn paper_flag(account: &config::AlpacaAccount) -> i64 {
    if account.is_paper() {
        1
    } else {
        0
    }
}

// Builds client order id values.
fn client_order_id(account: &config::AlpacaAccount, side: &str, symbol: &str) -> String {
    format!(
        "mlai-auto-{}-{}-{}-{}",
        safe_account_ref(account.account_ref()),
        side,
        symbol,
        Utc::now().timestamp_millis()
    )
}

// Builds client from configured inputs.
fn build_client(account: &config::AlpacaAccount) -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "APCA-API-KEY-ID",
        reqwest::header::HeaderValue::from_str(&account.api_key_id).unwrap(),
    );
    headers.insert(
        "APCA-API-SECRET-KEY",
        reqwest::header::HeaderValue::from_str(&account.secret_key).unwrap(),
    );
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap()
}

// Runs the api get API helper.
async fn api_get<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
) -> anyhow::Result<T> {
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("API error {}: {}", status, body);
    }
    Ok(resp.json().await?)
}

// Runs the api post API helper.
async fn api_post<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    body: &impl Serialize,
) -> anyhow::Result<T> {
    let resp = client.post(url).json(body).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        anyhow::bail!("API error {}: {}", status, body_text);
    }
    Ok(resp.json().await?)
}

// Handles json str logic.
fn json_str(value: &serde_json::Value, key: &str) -> Option<String> {
    value.get(key).and_then(|value| {
        let parsed = match value {
            serde_json::Value::String(value) => value.trim().to_string(),
            serde_json::Value::Number(value) => value.to_string(),
            _ => return None,
        };
        if parsed.is_empty() || parsed.eq_ignore_ascii_case("null") {
            None
        } else {
            Some(parsed)
        }
    })
}

// Handles json symbol logic.
fn json_symbol(value: &serde_json::Value, key: &str) -> Option<String> {
    json_str(value, key).map(|symbol| symbol.to_ascii_uppercase())
}

// Handles json f64 logic.
fn json_f64(value: &serde_json::Value, key: &str) -> Option<f64> {
    value.get(key).and_then(|value| match value {
        serde_json::Value::Number(value) => value.as_f64(),
        serde_json::Value::String(value) => value.trim().parse::<f64>().ok(),
        _ => None,
    })
}

// Handles order cursor logic.
fn order_cursor(value: &serde_json::Value) -> Option<String> {
    json_str(value, "submitted_at")
        .or_else(|| json_str(value, "created_at"))
        .or_else(|| json_str(value, "updated_at"))
        .or_else(|| json_str(value, "filled_at"))
}

// Handles fill cursor logic.
fn fill_cursor(value: &serde_json::Value) -> Option<String> {
    json_str(value, "transaction_time")
}

// Synchronizes cursor from latest with external or local state.
fn sync_cursor_from_latest(latest: Option<String>) -> String {
    latest
        .as_deref()
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|timestamp| {
            (timestamp.with_timezone(&Utc) - Duration::days(1))
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        })
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

// Handles last order cursor logic.
fn last_order_cursor(conn: &Connection, account: &config::AlpacaAccount) -> Option<String> {
    conn.query_row(
        "SELECT MAX(COALESCE(submitted_at, updated_at, filled_at, synced_at_utc))
         FROM provider_order_snapshots
         WHERE provider=?1 AND account_ref=?2 AND paper_account=?3",
        params![
            account.provider(),
            account.account_ref(),
            paper_flag(account)
        ],
        |row| row.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

// Handles last fill cursor logic.
fn last_fill_cursor(conn: &Connection, account: &config::AlpacaAccount) -> Option<String> {
    conn.query_row(
        "SELECT MAX(COALESCE(transaction_time, synced_at_utc))
         FROM provider_fill_activities
         WHERE provider=?1 AND account_ref=?2 AND paper_account=?3",
        params![
            account.provider(),
            account.account_ref(),
            paper_flag(account)
        ],
        |row| row.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

// Handles account table stats matching or metadata.
fn account_table_stats(
    conn: &Connection,
    account: &config::AlpacaAccount,
    table: &str,
    time_column: &str,
) -> anyhow::Result<serde_json::Value> {
    let sql = format!(
        "SELECT COUNT(*), MIN({time_column}), MAX({time_column})
         FROM {table}
         WHERE provider=?1 AND account_ref=?2 AND paper_account=?3"
    );
    let (count, oldest, newest): (i64, Option<String>, Option<String>) = conn.query_row(
        &sql,
        params![
            account.provider(),
            account.account_ref(),
            paper_flag(account)
        ],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    Ok(serde_json::json!({
        "local_count": count,
        "oldest": oldest,
        "newest": newest,
    }))
}

// Counts provider rows by execution origin for one account.
fn account_execution_origin_counts(
    conn: &Connection,
    account: &config::AlpacaAccount,
    table: &str,
) -> anyhow::Result<serde_json::Value> {
    let sql = match table {
        "provider_order_snapshots" => {
            "SELECT COALESCE(execution_origin, 'unknown'), COUNT(*)
             FROM provider_order_snapshots
             WHERE provider=?1 AND account_ref=?2 AND paper_account=?3
             GROUP BY COALESCE(execution_origin, 'unknown')
             ORDER BY 1"
        }
        "provider_fill_activities" => {
            "SELECT COALESCE(execution_origin, 'unknown'), COUNT(*)
             FROM provider_fill_activities
             WHERE provider=?1 AND account_ref=?2 AND paper_account=?3
             GROUP BY COALESCE(execution_origin, 'unknown')
             ORDER BY 1"
        }
        _ => return Ok(serde_json::json!({})),
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(
        params![
            account.provider(),
            account.account_ref(),
            paper_flag(account)
        ],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
    )?;
    let mut map = serde_json::Map::new();
    for row in rows {
        let (execution_origin, count) = row?;
        map.insert(execution_origin, serde_json::Value::from(count));
    }
    Ok(serde_json::Value::Object(map))
}

// Formats execution-origin count maps for CLI output.
fn compact_origin_counts(value: &serde_json::Value) -> String {
    let Some(map) = value.as_object() else {
        return "none".to_string();
    };
    if map.is_empty() {
        return "none".to_string();
    }
    map.iter()
        .map(|(origin_value, count)| {
            format!(
                "{}={}",
                origin::ExecutionOrigin::parse(origin_value).short_label(),
                count.as_i64().unwrap_or(0)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// Normalizes one ticker symbol for account-scoped auto tracking commands.
fn normalize_symbol(value: &str) -> anyhow::Result<String> {
    let symbol = value.trim().to_ascii_uppercase();
    if symbol.is_empty() {
        anyhow::bail!("symbol is required");
    }
    if symbol == "ALL" {
        anyhow::bail!("track/untrack requires one explicit symbol; ALL is not allowed.");
    }
    Ok(symbol)
}

// Splits account selectors supplied through repeated or comma-separated --account.
fn account_selector_tokens(selectors: &[String]) -> Vec<String> {
    selectors
        .iter()
        .flat_map(|value| value.split(','))
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

// Checks whether a selector refers to the configured account.
fn account_selector_matches(selector: &str, account: &config::AlpacaAccount) -> bool {
    let account_ref = account.account_ref().to_ascii_lowercase();
    let provider_account = format!("{}:{account_ref}", account.provider().to_ascii_lowercase());
    selector == provider_account
}

// Resolves required account selectors for auto ownership commands.
fn selected_auto_accounts(selectors: &[String]) -> anyhow::Result<Vec<config::AlpacaAccount>> {
    let tokens = account_selector_tokens(selectors);
    if tokens.is_empty() {
        anyhow::bail!(
            "--account is required. Run `mlai-trade trade account` to list account selector IDs."
        );
    }
    if tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "all" | "provider" | "providers" | "alpaca" | "paper" | "real" | "live" | "individual"
        ) || !token.contains(':')
    }) {
        anyhow::bail!(
            "track/untrack requires full provider:account-ref selectors like alpaca:paper-main; bare refs and broad selectors such as all, paper, real, or provider names are not allowed."
        );
    }
    let accounts = config::alpaca_accounts()?;
    let mut selected = Vec::<config::AlpacaAccount>::new();
    let mut missing = Vec::new();
    for token in &tokens {
        let mut matched = false;
        for account in &accounts {
            if account_selector_matches(token, account) {
                matched = true;
                if !selected.iter().any(|seen| {
                    seen.provider() == account.provider()
                        && seen.account_ref() == account.account_ref()
                }) {
                    selected.push(account.clone());
                }
            }
        }
        if !matched {
            missing.push(token.clone());
        }
    }
    if !missing.is_empty() {
        let available = accounts
            .iter()
            .map(|account| format!("{}:{}", account.provider(), account.account_ref()))
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "Unknown account selector(s): {}. Available accounts: {}.",
            missing.join(", "),
            available
        );
    }
    if selected.is_empty() {
        anyhow::bail!("No accounts matched the requested selector.");
    }
    Ok(selected)
}

// Persists the current management owner for a provider-held position.
fn set_position_management_override(
    conn: &Connection,
    account: &config::AlpacaAccount,
    broker_id: Option<&str>,
    symbol: &str,
    management_origin: origin::ExecutionOrigin,
    auto_managed: bool,
    reason: &str,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO position_management_overrides (
            provider, account_ref, broker_account_id, account_mode, paper_account,
            symbol, management_origin, auto_managed, reason, updated_at_utc
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(provider, account_ref, paper_account, symbol) DO UPDATE SET
            broker_account_id=excluded.broker_account_id,
            account_mode=excluded.account_mode,
            management_origin=excluded.management_origin,
            auto_managed=excluded.auto_managed,
            reason=excluded.reason,
            updated_at_utc=excluded.updated_at_utc",
        params![
            account.provider(),
            account.account_ref(),
            broker_id,
            alpaca::account_mode_for(account),
            paper_flag(account),
            symbol,
            management_origin.as_str(),
            if auto_managed { 1 } else { 0 },
            reason,
            Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        ],
    )?;
    Ok(())
}

// Reads the latest ML prediction for one symbol, if available.
fn latest_ml_prediction_for_symbol(
    conn: &Connection,
    symbol: &str,
) -> anyhow::Result<(Option<i64>, Option<f64>, Option<String>)> {
    let row = conn
        .query_row(
            "SELECT predicted_quintile, COALESCE(ensemble_score, predicted_score), date
             FROM ml_predictions
             WHERE UPPER(symbol)=?1
             ORDER BY date DESC
             LIMIT 1",
            params![symbol],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<f64>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()
        .unwrap_or(None);
    Ok(row.unwrap_or((None, None, None)))
}

// Infers the original entry execution origin for a provider-held position.
fn provider_position_entry_origin(
    conn: &Connection,
    account: &config::AlpacaAccount,
    symbol: &str,
) -> origin::ExecutionOrigin {
    let raw: Option<String> = conn
        .query_row(
            "SELECT CASE
                WHEN COUNT(*) = 0 THEN NULL
                WHEN COUNT(DISTINCT COALESCE(NULLIF(execution_origin, ''), 'unknown')) > 1 THEN 'mixed'
                ELSE MIN(COALESCE(NULLIF(execution_origin, ''), 'unknown'))
              END
             FROM provider_fill_activities
             WHERE provider=?1 AND account_ref=?2 AND paper_account=?3
               AND UPPER(symbol)=?4
               AND UPPER(COALESCE(side, ''))='BUY'",
            params![
                account.provider(),
                account.account_ref(),
                paper_flag(account),
                symbol
            ],
            |row| row.get(0),
        )
        .ok()
        .flatten();
    origin::ExecutionOrigin::parse(raw.as_deref().unwrap_or("provider_external"))
}

// Returns whether a provider order snapshot was already stored.
fn provider_order_seen(
    conn: &Connection,
    account: &config::AlpacaAccount,
    order_id: &str,
) -> anyhow::Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM provider_order_snapshots
         WHERE provider=?1 AND account_ref=?2 AND paper_account=?3 AND order_id=?4",
        params![
            account.provider(),
            account.account_ref(),
            paper_flag(account),
            order_id
        ],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

// Returns whether an order came from mlai-trade auto execution.
fn auto_trade_order_seen(
    conn: &Connection,
    account: &config::AlpacaAccount,
    order_id: &str,
) -> anyhow::Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM auto_trades
         WHERE provider=?1 AND account_ref=?2 AND paper_account=?3 AND order_id=?4",
        params![
            account.provider(),
            account.account_ref(),
            paper_flag(account),
            order_id
        ],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

// Stores provider order in local storage.
fn upsert_provider_order(
    conn: &Connection,
    account: &config::AlpacaAccount,
    broker_id: Option<&str>,
    order: &serde_json::Value,
) -> anyhow::Result<()> {
    let order_id = json_str(order, "id")
        .or_else(|| json_str(order, "order_id"))
        .ok_or_else(|| anyhow::anyhow!("Alpaca order row missing id/order_id"))?;
    let submitted_at = json_str(order, "submitted_at").or_else(|| json_str(order, "created_at"));
    let synced_at = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let raw_json = serde_json::to_string(order)?;
    let already_seen = provider_order_seen(conn, account, &order_id)?;
    let known_auto_order = auto_trade_order_seen(conn, account, &order_id)?;
    let client_order_id = json_str(order, "client_order_id");
    let execution_origin = origin::classify_order(
        conn,
        account.provider(),
        account.account_ref(),
        paper_flag(account),
        Some(&order_id),
        client_order_id.as_deref(),
        known_auto_order,
    )?;
    conn.execute(
        "INSERT INTO provider_order_snapshots (
            provider, account_ref, broker_account_id, account_mode, paper_account, order_id,
            client_order_id, symbol, side, order_type, time_in_force, status, qty, filled_qty,
            limit_price, stop_price, filled_avg_price, submitted_at, filled_at, canceled_at,
            expired_at, replaced_at, updated_at, execution_origin, synced_at_utc, raw_json
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)
         ON CONFLICT(provider, account_ref, paper_account, order_id) DO UPDATE SET
            broker_account_id=excluded.broker_account_id,
            account_mode=excluded.account_mode,
            client_order_id=excluded.client_order_id,
            symbol=excluded.symbol,
            side=excluded.side,
            order_type=excluded.order_type,
            time_in_force=excluded.time_in_force,
            status=excluded.status,
            qty=excluded.qty,
            filled_qty=excluded.filled_qty,
            limit_price=excluded.limit_price,
            stop_price=excluded.stop_price,
            filled_avg_price=excluded.filled_avg_price,
            submitted_at=excluded.submitted_at,
            filled_at=excluded.filled_at,
            canceled_at=excluded.canceled_at,
            expired_at=excluded.expired_at,
            replaced_at=excluded.replaced_at,
            updated_at=excluded.updated_at,
            execution_origin=excluded.execution_origin,
            synced_at_utc=excluded.synced_at_utc,
            raw_json=excluded.raw_json",
        params![
            account.provider(),
            account.account_ref(),
            broker_id,
            alpaca::account_mode_for(account),
            paper_flag(account),
            order_id,
            client_order_id,
            json_symbol(order, "symbol"),
            json_str(order, "side"),
            json_str(order, "type"),
            json_str(order, "time_in_force"),
            json_str(order, "status"),
            json_f64(order, "qty"),
            json_f64(order, "filled_qty"),
            json_f64(order, "limit_price"),
            json_f64(order, "stop_price"),
            json_f64(order, "filled_avg_price"),
            submitted_at,
            json_str(order, "filled_at"),
            json_str(order, "canceled_at"),
            json_str(order, "expired_at"),
            json_str(order, "replaced_at"),
            json_str(order, "updated_at"),
            execution_origin.as_str(),
            synced_at,
            raw_json,
        ],
    )?;
    if !already_seen && execution_origin == origin::ExecutionOrigin::ProviderExternal {
        append_auto_log(serde_json::json!({
            "event": "provider_external_order_observed",
            "level": "info",
            "execution_origin": execution_origin.as_str(),
            "source": "provider_sync",
            "provider": account.provider(),
            "account_ref": account.account_ref(),
            "broker_account_id": broker_id.unwrap_or("not available"),
            "account_mode": alpaca::account_mode_for(account),
            "tax_universe": if account.is_paper() { "paper" } else { "real" },
            "order_id": order_id,
            "symbol": json_symbol(order, "symbol").unwrap_or_else(|| "not available".to_string()),
            "side": json_str(order, "side").unwrap_or_else(|| "not available".to_string()),
            "status": json_str(order, "status").unwrap_or_else(|| "not available".to_string()),
            "qty": json_f64(order, "qty")
                .map(serde_json::Value::from)
                .unwrap_or_else(|| serde_json::json!("not available")),
            "filled_qty": json_f64(order, "filled_qty")
                .map(serde_json::Value::from)
                .unwrap_or_else(|| serde_json::json!("not available")),
            "filled_avg_price": json_f64(order, "filled_avg_price")
                .map(serde_json::Value::from)
                .unwrap_or_else(|| serde_json::json!("not available")),
            "submitted_at": submitted_at.unwrap_or_else(|| "not available".to_string()),
            "message": "Provider order was not created by mlai-trade; stored as source-of-truth external activity.",
        }));
    }
    Ok(())
}

// Returns whether a provider fill activity was already stored.
fn provider_fill_seen(
    conn: &Connection,
    account: &config::AlpacaAccount,
    activity_id: &str,
) -> anyhow::Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM provider_fill_activities
         WHERE provider=?1 AND account_ref=?2 AND paper_account=?3 AND activity_id=?4",
        params![
            account.provider(),
            account.account_ref(),
            paper_flag(account),
            activity_id
        ],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

// Stores provider fill in local storage.
fn upsert_provider_fill(
    conn: &Connection,
    account: &config::AlpacaAccount,
    broker_id: Option<&str>,
    fill: &serde_json::Value,
) -> anyhow::Result<()> {
    let activity_id = json_str(fill, "id").unwrap_or_else(|| {
        format!(
            "{}:{}:{}",
            json_str(fill, "order_id").unwrap_or_else(|| "unknown_order".to_string()),
            json_symbol(fill, "symbol").unwrap_or_else(|| "UNKNOWN".to_string()),
            json_str(fill, "transaction_time").unwrap_or_else(|| "unknown_time".to_string())
        )
    });
    let synced_at = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let raw_json = serde_json::to_string(fill)?;
    let order_id = json_str(fill, "order_id");
    let already_seen = provider_fill_seen(conn, account, &activity_id)?;
    let known_auto_order = order_id
        .as_deref()
        .map(|id| auto_trade_order_seen(conn, account, id))
        .transpose()?
        .unwrap_or(false);
    let snapshot_client_order_id = if let Some(order_id) = order_id.as_deref() {
        conn.query_row(
            "SELECT client_order_id
             FROM provider_order_snapshots
             WHERE provider=?1 AND account_ref=?2 AND paper_account=?3 AND order_id=?4",
            params![
                account.provider(),
                account.account_ref(),
                paper_flag(account),
                order_id
            ],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
    } else {
        None
    };
    let execution_origin = origin::classify_order(
        conn,
        account.provider(),
        account.account_ref(),
        paper_flag(account),
        order_id.as_deref(),
        snapshot_client_order_id.as_deref(),
        known_auto_order,
    )?;
    conn.execute(
        "INSERT INTO provider_fill_activities (
            provider, account_ref, broker_account_id, account_mode, paper_account, activity_id,
            order_id, symbol, side, qty, price, cum_qty, leaves_qty, activity_type,
            transaction_time, execution_origin, synced_at_utc, raw_json
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
         ON CONFLICT(provider, account_ref, paper_account, activity_id) DO UPDATE SET
            broker_account_id=excluded.broker_account_id,
            account_mode=excluded.account_mode,
            order_id=excluded.order_id,
            symbol=excluded.symbol,
            side=excluded.side,
            qty=excluded.qty,
            price=excluded.price,
            cum_qty=excluded.cum_qty,
            leaves_qty=excluded.leaves_qty,
            activity_type=excluded.activity_type,
            transaction_time=excluded.transaction_time,
            execution_origin=excluded.execution_origin,
            synced_at_utc=excluded.synced_at_utc,
            raw_json=excluded.raw_json",
        params![
            account.provider(),
            account.account_ref(),
            broker_id,
            alpaca::account_mode_for(account),
            paper_flag(account),
            activity_id,
            order_id,
            json_symbol(fill, "symbol"),
            json_str(fill, "side"),
            json_f64(fill, "qty"),
            json_f64(fill, "price"),
            json_f64(fill, "cum_qty"),
            json_f64(fill, "leaves_qty"),
            json_str(fill, "activity_type").or_else(|| json_str(fill, "type")),
            json_str(fill, "transaction_time"),
            execution_origin.as_str(),
            synced_at,
            raw_json,
        ],
    )?;
    if !already_seen && execution_origin == origin::ExecutionOrigin::ProviderExternal {
        append_auto_log(serde_json::json!({
            "event": "provider_external_fill_observed",
            "level": "info",
            "execution_origin": execution_origin.as_str(),
            "source": "provider_sync",
            "provider": account.provider(),
            "account_ref": account.account_ref(),
            "broker_account_id": broker_id.unwrap_or("not available"),
            "account_mode": alpaca::account_mode_for(account),
            "tax_universe": if account.is_paper() { "paper" } else { "real" },
            "activity_id": activity_id,
            "order_id": json_str(fill, "order_id").unwrap_or_else(|| "not available".to_string()),
            "symbol": json_symbol(fill, "symbol").unwrap_or_else(|| "not available".to_string()),
            "side": json_str(fill, "side").unwrap_or_else(|| "not available".to_string()),
            "qty": json_f64(fill, "qty")
                .map(serde_json::Value::from)
                .unwrap_or_else(|| serde_json::json!("not available")),
            "price": json_f64(fill, "price")
                .map(serde_json::Value::from)
                .unwrap_or_else(|| serde_json::json!("not available")),
            "transaction_time": json_str(fill, "transaction_time")
                .unwrap_or_else(|| "not available".to_string()),
            "message": "Provider fill was not created by mlai-trade; stored as source-of-truth external activity.",
        }));
    }
    Ok(())
}

// Stores and logs provider account cash/equity changes.
fn sync_provider_account_snapshot(
    conn: &Connection,
    account: &config::AlpacaAccount,
    broker_id: Option<&str>,
    info: &AccountInfo,
    source: &str,
) -> anyhow::Result<serde_json::Value> {
    let previous: Option<(Option<f64>, Option<f64>, Option<f64>)> = conn
        .query_row(
            "SELECT cash, equity, portfolio_value
             FROM provider_account_snapshots
             WHERE provider=?1 AND account_ref=?2 AND paper_account=?3",
            params![
                account.provider(),
                account.account_ref(),
                paper_flag(account)
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .ok();
    let cash = info
        .cash
        .as_deref()
        .and_then(|value| value.parse::<f64>().ok());
    let equity = info
        .equity
        .as_deref()
        .and_then(|value| value.parse::<f64>().ok());
    let buying_power = info
        .buying_power
        .as_deref()
        .and_then(|value| value.parse::<f64>().ok());
    let portfolio_value = info
        .portfolio_value
        .as_deref()
        .and_then(|value| value.parse::<f64>().ok());
    let synced_at = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let raw_json = serde_json::to_string(info)?;
    conn.execute(
        "INSERT INTO provider_account_snapshots (
            provider, account_ref, broker_account_id, account_mode, paper_account,
            equity, cash, buying_power, portfolio_value, synced_at_utc, raw_json
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(provider, account_ref, paper_account) DO UPDATE SET
            broker_account_id=excluded.broker_account_id,
            account_mode=excluded.account_mode,
            equity=excluded.equity,
            cash=excluded.cash,
            buying_power=excluded.buying_power,
            portfolio_value=excluded.portfolio_value,
            synced_at_utc=excluded.synced_at_utc,
            raw_json=excluded.raw_json",
        params![
            account.provider(),
            account.account_ref(),
            broker_id,
            alpaca::account_mode_for(account),
            paper_flag(account),
            equity,
            cash,
            buying_power,
            portfolio_value,
            synced_at,
            raw_json
        ],
    )?;

    let mut changed = false;
    let mut cash_delta = None;
    let mut equity_delta = None;
    let mut portfolio_delta = None;
    if let Some((prev_cash, prev_equity, prev_portfolio)) = previous {
        cash_delta = cash
            .zip(prev_cash)
            .map(|(current, previous)| current - previous);
        equity_delta = equity
            .zip(prev_equity)
            .map(|(current, previous)| current - previous);
        portfolio_delta = portfolio_value
            .zip(prev_portfolio)
            .map(|(current, previous)| current - previous);
        changed = cash_delta.map(|delta| delta.abs() >= 0.01).unwrap_or(false);
    }
    if changed {
        append_auto_log(serde_json::json!({
            "event": "provider_account_snapshot_changed",
            "level": "info",
            "source": source,
            "provider": account.provider(),
            "account_ref": account.account_ref(),
            "broker_account_id": broker_id.unwrap_or("not available"),
            "account_mode": alpaca::account_mode_for(account),
            "tax_universe": if account.is_paper() { "paper" } else { "real" },
            "cash": cash.map(serde_json::Value::from).unwrap_or_else(|| serde_json::json!("not available")),
            "cash_delta": cash_delta.map(serde_json::Value::from).unwrap_or_else(|| serde_json::json!("not available")),
            "equity": equity.map(serde_json::Value::from).unwrap_or_else(|| serde_json::json!("not available")),
            "equity_delta": equity_delta.map(serde_json::Value::from).unwrap_or_else(|| serde_json::json!("not available")),
            "portfolio_value": portfolio_value.map(serde_json::Value::from).unwrap_or_else(|| serde_json::json!("not available")),
            "portfolio_value_delta": portfolio_delta.map(serde_json::Value::from).unwrap_or_else(|| serde_json::json!("not available")),
            "message": "Provider cash changed outside local auto-position state; treating provider as source of truth.",
        }));
    }

    Ok(serde_json::json!({
        "cash": cash.map(serde_json::Value::from).unwrap_or_else(|| serde_json::json!("not available")),
        "equity": equity.map(serde_json::Value::from).unwrap_or_else(|| serde_json::json!("not available")),
        "buying_power": buying_power.map(serde_json::Value::from).unwrap_or_else(|| serde_json::json!("not available")),
        "portfolio_value": portfolio_value.map(serde_json::Value::from).unwrap_or_else(|| serde_json::json!("not available")),
        "changed": changed,
        "synced_at_utc": synced_at,
    }))
}

// Stores the provider's current live positions as the local holdings snapshot.
pub fn sync_provider_position_snapshots_from_json(
    conn: &Connection,
    provider: &str,
    account_ref: &str,
    broker_account_id: Option<&str>,
    account_mode: &str,
    paper_account: bool,
    positions: &[serde_json::Value],
) -> anyhow::Result<serde_json::Value> {
    let synced_at = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let paper_flag = if paper_account { 1 } else { 0 };
    let mut seen_symbols = Vec::new();
    let tx = conn.unchecked_transaction()?;
    for position in positions {
        let Some(symbol) = json_symbol(position, "symbol") else {
            continue;
        };
        seen_symbols.push(symbol.clone());
        tx.execute(
            "INSERT INTO provider_position_snapshots (
                provider, account_ref, broker_account_id, account_mode, paper_account,
                symbol, qty, avg_entry_price, current_price, market_value,
                unrealized_pl, unrealized_plpc, asset_class, exchange, side,
                synced_at_utc, raw_json
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
             ON CONFLICT(provider, account_ref, paper_account, symbol) DO UPDATE SET
                broker_account_id=excluded.broker_account_id,
                account_mode=excluded.account_mode,
                qty=excluded.qty,
                avg_entry_price=excluded.avg_entry_price,
                current_price=excluded.current_price,
                market_value=excluded.market_value,
                unrealized_pl=excluded.unrealized_pl,
                unrealized_plpc=excluded.unrealized_plpc,
                asset_class=excluded.asset_class,
                exchange=excluded.exchange,
                side=excluded.side,
                synced_at_utc=excluded.synced_at_utc,
                raw_json=excluded.raw_json",
            params![
                provider,
                account_ref,
                broker_account_id,
                account_mode,
                paper_flag,
                symbol,
                json_f64(position, "qty"),
                json_f64(position, "avg_entry_price"),
                json_f64(position, "current_price"),
                json_f64(position, "market_value"),
                json_f64(position, "unrealized_pl"),
                json_f64(position, "unrealized_plpc"),
                json_str(position, "asset_class"),
                json_str(position, "exchange"),
                json_str(position, "side"),
                synced_at,
                serde_json::to_string(position)?,
            ],
        )?;
    }
    tx.execute(
        "DELETE FROM provider_position_snapshots
         WHERE provider=?1 AND account_ref=?2 AND paper_account=?3 AND synced_at_utc<>?4",
        params![provider, account_ref, paper_flag, synced_at],
    )?;
    tx.commit()?;
    Ok(serde_json::json!({
        "local_count": seen_symbols.len(),
        "symbols_seen": seen_symbols.len(),
        "synced_at_utc": synced_at,
    }))
}

// Stores typed provider positions by converting them to the raw Alpaca shape.
fn sync_provider_position_snapshots(
    conn: &Connection,
    account: &config::AlpacaAccount,
    broker_id: Option<&str>,
    positions: &[alpaca::Position],
) -> anyhow::Result<serde_json::Value> {
    let rows = positions
        .iter()
        .map(|position| {
            serde_json::json!({
                "symbol": position.symbol,
                "qty": position.qty,
                "avg_entry_price": position.avg_entry_price,
                "current_price": position.current_price,
                "market_value": position.market_value,
                "unrealized_pl": position.unrealized_pl,
                "unrealized_plpc": position.unrealized_plpc,
                "asset_class": position.asset_class,
                "exchange": position.exchange,
                "side": position.side,
            })
        })
        .collect::<Vec<_>>();
    sync_provider_position_snapshots_from_json(
        conn,
        account.provider(),
        account.account_ref(),
        broker_id,
        alpaca::account_mode_for(account),
        account.is_paper(),
        &rows,
    )
}

// Moves prior local rows for a renamed account onto the current account ref.
fn canonicalize_account_ref_for_broker(
    conn: &Connection,
    account: &config::AlpacaAccount,
    broker_id: Option<&str>,
) -> anyhow::Result<usize> {
    let Some(broker_id) = broker_id.filter(|value| !value.trim().is_empty()) else {
        return Ok(0);
    };
    let provider = account.provider();
    let account_ref = account.account_ref();
    let paper = paper_flag(account);
    let mut changed = 0usize;
    let mut old_refs = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT account_ref
             FROM (
                 SELECT account_ref
                 FROM provider_order_snapshots
                 WHERE provider=?1 AND paper_account=?2 AND broker_account_id=?3 AND account_ref<>?4
                 UNION
                 SELECT account_ref
                 FROM provider_fill_activities
                 WHERE provider=?1 AND paper_account=?2 AND broker_account_id=?3 AND account_ref<>?4
                 UNION
                 SELECT account_ref
                 FROM provider_account_snapshots
                 WHERE provider=?1 AND paper_account=?2 AND broker_account_id=?3 AND account_ref<>?4
                 UNION
                 SELECT account_ref
                 FROM auto_positions
                 WHERE provider=?1 AND paper_account=?2 AND broker_account_id=?3 AND account_ref<>?4
                 UNION
                 SELECT account_ref
                 FROM auto_trades
                 WHERE provider=?1 AND paper_account=?2 AND broker_account_id=?3 AND account_ref<>?4
                 UNION
                 SELECT account_ref
                 FROM position_management_overrides
                 WHERE provider=?1 AND paper_account=?2 AND broker_account_id=?3 AND account_ref<>?4
             )
             ORDER BY account_ref",
        )?;
        let rows = stmt.query_map(params![provider, paper, broker_id, account_ref], |row| {
            row.get::<_, String>(0)
        })?;
        for row in rows {
            old_refs.push(row?);
        }
    }

    changed += conn.execute(
        "DELETE FROM provider_order_snapshots
         WHERE provider=?1 AND paper_account=?2 AND broker_account_id=?3 AND account_ref<>?4
           AND EXISTS (
               SELECT 1 FROM provider_order_snapshots target
               WHERE target.provider=provider_order_snapshots.provider
                 AND target.paper_account=provider_order_snapshots.paper_account
                 AND target.account_ref=?4
                 AND target.order_id=provider_order_snapshots.order_id
           )",
        params![provider, paper, broker_id, account_ref],
    )?;
    changed += conn.execute(
        "UPDATE provider_order_snapshots
         SET account_ref=?4
         WHERE provider=?1 AND paper_account=?2 AND broker_account_id=?3 AND account_ref<>?4",
        params![provider, paper, broker_id, account_ref],
    )?;

    changed += conn.execute(
        "DELETE FROM provider_fill_activities
         WHERE provider=?1 AND paper_account=?2 AND broker_account_id=?3 AND account_ref<>?4
           AND EXISTS (
               SELECT 1 FROM provider_fill_activities target
               WHERE target.provider=provider_fill_activities.provider
                 AND target.paper_account=provider_fill_activities.paper_account
                 AND target.account_ref=?4
                 AND target.activity_id=provider_fill_activities.activity_id
           )",
        params![provider, paper, broker_id, account_ref],
    )?;
    changed += conn.execute(
        "UPDATE provider_fill_activities
         SET account_ref=?4
         WHERE provider=?1 AND paper_account=?2 AND broker_account_id=?3 AND account_ref<>?4",
        params![provider, paper, broker_id, account_ref],
    )?;

    changed += conn.execute(
        "DELETE FROM provider_account_snapshots
         WHERE provider=?1 AND paper_account=?2 AND broker_account_id=?3 AND account_ref<>?4
           AND EXISTS (
               SELECT 1 FROM provider_account_snapshots target
               WHERE target.provider=provider_account_snapshots.provider
                 AND target.paper_account=provider_account_snapshots.paper_account
                 AND target.account_ref=?4
           )",
        params![provider, paper, broker_id, account_ref],
    )?;
    changed += conn.execute(
        "UPDATE provider_account_snapshots
         SET account_ref=?4
         WHERE provider=?1 AND paper_account=?2 AND broker_account_id=?3 AND account_ref<>?4",
        params![provider, paper, broker_id, account_ref],
    )?;

    changed += conn.execute(
        "DELETE FROM provider_position_snapshots
         WHERE provider=?1 AND paper_account=?2 AND broker_account_id=?3 AND account_ref<>?4
           AND EXISTS (
               SELECT 1 FROM provider_position_snapshots target
               WHERE target.provider=provider_position_snapshots.provider
                 AND target.paper_account=provider_position_snapshots.paper_account
                 AND target.account_ref=?4
                 AND target.symbol=provider_position_snapshots.symbol
           )",
        params![provider, paper, broker_id, account_ref],
    )?;
    changed += conn.execute(
        "UPDATE provider_position_snapshots
         SET account_ref=?4
         WHERE provider=?1 AND paper_account=?2 AND broker_account_id=?3 AND account_ref<>?4",
        params![provider, paper, broker_id, account_ref],
    )?;

    changed += conn.execute(
        "DELETE FROM position_management_overrides
         WHERE provider=?1 AND paper_account=?2 AND broker_account_id=?3 AND account_ref<>?4
           AND EXISTS (
               SELECT 1 FROM position_management_overrides target
               WHERE target.provider=position_management_overrides.provider
                 AND target.paper_account=position_management_overrides.paper_account
                 AND target.account_ref=?4
                 AND target.symbol=position_management_overrides.symbol
           )",
        params![provider, paper, broker_id, account_ref],
    )?;

    for table in [
        "auto_positions",
        "auto_trades",
        "position_management_overrides",
    ] {
        changed += conn.execute(
            &format!(
                "UPDATE {table}
                 SET account_ref=?4
                 WHERE provider=?1 AND paper_account=?2 AND broker_account_id=?3
                   AND account_ref<>?4"
            ),
            params![provider, paper, broker_id, account_ref],
        )?;
    }

    changed += conn.execute(
        "DELETE FROM wash_sale_tracker
         WHERE provider=?1 AND paper_account=?2 AND broker_account_id=?3 AND account_ref<>?4
           AND EXISTS (
               SELECT 1 FROM wash_sale_tracker target
               WHERE target.paper_account=wash_sale_tracker.paper_account
                 AND target.account_ref=?4
                 AND target.symbol=wash_sale_tracker.symbol
                 AND COALESCE(target.sell_timestamp_utc, '') = COALESCE(wash_sale_tracker.sell_timestamp_utc, '')
                 AND ABS(COALESCE(target.sell_price, 0.0) - COALESCE(wash_sale_tracker.sell_price, 0.0)) < 0.000001
           )",
        params![provider, paper, broker_id, account_ref],
    )?;
    changed += conn.execute(
        "UPDATE wash_sale_tracker
         SET account_ref=?4
         WHERE provider=?1 AND paper_account=?2 AND broker_account_id=?3 AND account_ref<>?4",
        params![provider, paper, broker_id, account_ref],
    )?;
    for old_ref in &old_refs {
        changed += conn.execute(
            "DELETE FROM wash_sale_tracker
             WHERE provider=?1 AND paper_account=?2 AND broker_account_id IS NULL AND account_ref=?3
               AND EXISTS (
                   SELECT 1 FROM wash_sale_tracker target
                   WHERE target.paper_account=wash_sale_tracker.paper_account
                     AND target.account_ref=?4
                     AND target.symbol=wash_sale_tracker.symbol
                     AND COALESCE(target.sell_timestamp_utc, '') = COALESCE(wash_sale_tracker.sell_timestamp_utc, '')
                     AND ABS(COALESCE(target.sell_price, 0.0) - COALESCE(wash_sale_tracker.sell_price, 0.0)) < 0.000001
               )",
            params![provider, paper, old_ref, account_ref],
        )?;
        changed += conn.execute(
            "UPDATE wash_sale_tracker
             SET account_ref=?4, broker_account_id=?5
             WHERE provider=?1 AND paper_account=?2 AND broker_account_id IS NULL AND account_ref=?3",
            params![provider, paper, old_ref, account_ref, broker_id],
        )?;
    }

    changed += conn.execute(
        "DELETE FROM day_trades
         WHERE provider=?1 AND paper_account=?2 AND broker_account_id=?3 AND account_ref<>?4
           AND EXISTS (
               SELECT 1 FROM day_trades target
               WHERE target.provider=day_trades.provider
                 AND target.paper_account=day_trades.paper_account
                 AND target.account_ref=?4
                 AND target.trade_date=day_trades.trade_date
                 AND target.symbol=day_trades.symbol
                 AND COALESCE(target.buy_time, '') = COALESCE(day_trades.buy_time, '')
                 AND COALESCE(target.sell_time, '') = COALESCE(day_trades.sell_time, '')
           )",
        params![provider, paper, broker_id, account_ref],
    )?;
    changed += conn.execute(
        "UPDATE day_trades
         SET account_ref=?4
         WHERE provider=?1 AND paper_account=?2 AND broker_account_id=?3 AND account_ref<>?4",
        params![provider, paper, broker_id, account_ref],
    )?;

    Ok(changed)
}

#[derive(Debug, Clone)]
struct SyncedFill {
    provider: String,
    account_ref: String,
    broker_account_id: Option<String>,
    paper_account: i64,
    symbol: String,
    side: String,
    qty: f64,
    price: f64,
    transaction_time: String,
}

#[derive(Debug, Clone, Copy)]
struct FillLot {
    qty_remaining: f64,
    price: f64,
}

#[derive(Debug, Clone, Default)]
struct WashSaleReconcileSummary {
    fills_scanned: usize,
    sell_fills: usize,
    loss_sells: usize,
    windows_inserted: usize,
    windows_existing: usize,
    unmatched_sell_qty: f64,
}

impl WashSaleReconcileSummary {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "fills_scanned": self.fills_scanned,
            "sell_fills": self.sell_fills,
            "loss_sells": self.loss_sells,
            "windows_inserted": self.windows_inserted,
            "windows_existing": self.windows_existing,
            "unmatched_sell_qty": self.unmatched_sell_qty,
        })
    }
}

// Normalizes provider fill timestamps to UTC seconds.
fn normalize_fill_timestamp_utc(value: &str) -> chrono::DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

// Loads provider-confirmed fills across one paper/real tax universe.
fn load_synced_fills_for_tax_universe(
    conn: &Connection,
    paper_account: i64,
) -> anyhow::Result<Vec<SyncedFill>> {
    let mut stmt = conn.prepare(
        "SELECT provider, account_ref, broker_account_id, paper_account,
                symbol, side, qty, price, transaction_time
         FROM provider_fill_activities
         WHERE paper_account=?1
           AND symbol IS NOT NULL AND side IS NOT NULL
           AND qty IS NOT NULL AND qty > 0
           AND price IS NOT NULL AND price > 0
           AND transaction_time IS NOT NULL
         ORDER BY transaction_time ASC, activity_id ASC",
    )?;
    let rows = stmt.query_map(params![paper_account], |row| {
        Ok(SyncedFill {
            provider: row.get(0)?,
            account_ref: row.get(1)?,
            broker_account_id: row.get(2)?,
            paper_account: row.get(3)?,
            symbol: row.get::<_, String>(4)?.to_ascii_uppercase(),
            side: row.get::<_, String>(5)?.to_ascii_lowercase(),
            qty: row.get(6)?,
            price: row.get(7)?,
            transaction_time: row.get(8)?,
        })
    })?;
    let mut fills = Vec::new();
    for row in rows {
        fills.push(row?);
    }
    Ok(fills)
}

// Returns true when a provider-confirmed loss sale already has a wash window.
fn wash_sale_window_exists(
    conn: &Connection,
    paper_account: i64,
    symbol: &str,
    sell_timestamp_utc: &str,
    sell_price: f64,
) -> anyhow::Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM wash_sale_tracker
         WHERE paper_account=?1 AND symbol=?2 AND sell_timestamp_utc=?3
           AND ABS(COALESCE(sell_price, 0.0) - ?4) < 0.000001",
        params![paper_account, symbol, sell_timestamp_utc, sell_price],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

// Inserts a provider-reconciled wash-sale monitor row when missing.
fn insert_reconciled_wash_sale(
    conn: &Connection,
    sell_fill: &SyncedFill,
    symbol: &str,
    sell_price: f64,
    loss_amount: f64,
    transaction_time: &str,
    safety_buffer_days: i64,
) -> anyhow::Result<bool> {
    let timestamp = normalize_fill_timestamp_utc(transaction_time);
    let sell_date = timestamp.format("%Y-%m-%d").to_string();
    let sell_time = timestamp.format("%H:%M:%S").to_string();
    let sell_timestamp_utc = timestamp.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    if wash_sale_window_exists(
        conn,
        sell_fill.paper_account,
        symbol,
        &sell_timestamp_utc,
        sell_price,
    )? {
        return Ok(false);
    }
    let window_end = {
        let date = NaiveDate::parse_from_str(&sell_date, "%Y-%m-%d")?;
        (date
            + chrono::Duration::days(compliance::wash_sale_forward_block_days(Some(
                safety_buffer_days,
            ))))
        .format("%Y-%m-%d")
        .to_string()
    };
    conn.execute(
        "INSERT INTO wash_sale_tracker (
            symbol, sell_date, sell_time, sell_timestamp_utc, event_timezone,
            sell_price, loss_amount, wash_window_end, status, provider, account_ref,
            broker_account_id, paper_account
         )
         VALUES (?1, ?2, ?3, ?4, 'UTC', ?5, ?6, ?7, 'active', ?8, ?9, ?10, ?11)",
        params![
            symbol,
            sell_date,
            sell_time,
            sell_timestamp_utc,
            sell_price,
            loss_amount.abs(),
            window_end,
            sell_fill.provider.as_str(),
            sell_fill.account_ref.as_str(),
            sell_fill.broker_account_id.as_deref(),
            sell_fill.paper_account,
        ],
    )?;
    Ok(true)
}

// Rebuilds missed wash-sale monitor rows from all fills in one tax universe.
fn reconcile_wash_sales_for_tax_universe(
    conn: &Connection,
    paper_account: i64,
) -> anyhow::Result<WashSaleReconcileSummary> {
    let cfg = load_config(conn);
    let fills = load_synced_fills_for_tax_universe(conn, paper_account)?;
    let mut summary = WashSaleReconcileSummary {
        fills_scanned: fills.len(),
        ..WashSaleReconcileSummary::default()
    };
    let mut lots: HashMap<String, VecDeque<FillLot>> = HashMap::new();

    for fill in fills {
        match fill.side.as_str() {
            "buy" => {
                lots.entry(fill.symbol).or_default().push_back(FillLot {
                    qty_remaining: fill.qty,
                    price: fill.price,
                });
            }
            "sell" => {
                summary.sell_fills += 1;
                let mut remaining = fill.qty;
                let mut loss_amount = 0.0;
                let symbol_lots = lots.entry(fill.symbol.clone()).or_default();
                while remaining > 0.000001 {
                    let Some(front) = symbol_lots.front_mut() else {
                        summary.unmatched_sell_qty += remaining;
                        break;
                    };
                    let matched_qty = remaining.min(front.qty_remaining);
                    if fill.price < front.price {
                        loss_amount += (front.price - fill.price) * matched_qty;
                    }
                    front.qty_remaining -= matched_qty;
                    remaining -= matched_qty;
                    if front.qty_remaining <= 0.000001 {
                        symbol_lots.pop_front();
                    }
                }
                if loss_amount > 0.000001 {
                    summary.loss_sells += 1;
                    let inserted = insert_reconciled_wash_sale(
                        conn,
                        &fill,
                        &fill.symbol,
                        fill.price,
                        loss_amount,
                        &fill.transaction_time,
                        cfg.wash_sale_safety_buffer_days,
                    )?;
                    if inserted {
                        summary.windows_inserted += 1;
                    } else {
                        summary.windows_existing += 1;
                    }
                }
            }
            _ => {}
        }
    }

    Ok(summary)
}

// Handles orders url logic.
fn orders_url(account: &config::AlpacaAccount, after: &str) -> anyhow::Result<String> {
    let mut url = reqwest::Url::parse(&alpaca::broker_api_url_for(account, "/orders"))?;
    url.query_pairs_mut()
        .append_pair("status", "all")
        .append_pair("limit", "500")
        .append_pair("direction", "asc")
        .append_pair("after", after);
    Ok(url.to_string())
}

// Handles fills url logic.
fn fills_url(account: &config::AlpacaAccount, after: &str) -> anyhow::Result<String> {
    let mut url = reqwest::Url::parse(&alpaca::broker_api_url_for(
        account,
        "/account/activities/FILL",
    ))?;
    url.query_pairs_mut()
        .append_pair("direction", "asc")
        .append_pair("page_size", "100")
        .append_pair("after", after);
    Ok(url.to_string())
}

// Synchronizes provider orders with external or local state.
async fn sync_provider_orders(
    conn: &Connection,
    account: &config::AlpacaAccount,
    client: &reqwest::Client,
    broker_id: Option<&str>,
) -> anyhow::Result<usize> {
    let mut after = sync_cursor_from_latest(last_order_cursor(conn, account));
    let mut seen = 0usize;
    loop {
        let page: Vec<serde_json::Value> = api_get(client, &orders_url(account, &after)?).await?;
        if page.is_empty() {
            break;
        }
        let next_after = page.iter().filter_map(order_cursor).last();
        for order in &page {
            upsert_provider_order(conn, account, broker_id, order)?;
            seen += 1;
        }
        if page.len() < 500 {
            break;
        }
        let Some(next_after) = next_after else {
            break;
        };
        if next_after <= after {
            break;
        }
        after = next_after;
    }
    Ok(seen)
}

// Synchronizes provider fills with external or local state.
async fn sync_provider_fills(
    conn: &Connection,
    account: &config::AlpacaAccount,
    client: &reqwest::Client,
    broker_id: Option<&str>,
) -> anyhow::Result<usize> {
    let mut after = sync_cursor_from_latest(last_fill_cursor(conn, account));
    let mut seen = 0usize;
    loop {
        let page: Vec<serde_json::Value> = api_get(client, &fills_url(account, &after)?).await?;
        if page.is_empty() {
            break;
        }
        let next_after = page.iter().filter_map(fill_cursor).last();
        for fill in &page {
            upsert_provider_fill(conn, account, broker_id, fill)?;
            seen += 1;
        }
        if page.len() < 100 {
            break;
        }
        let Some(next_after) = next_after else {
            break;
        };
        if next_after <= after {
            break;
        }
        after = next_after;
    }
    Ok(seen)
}

// Synchronizes provider history with context with external or local state.
async fn sync_provider_history_with_context(
    conn: &Connection,
    account: &config::AlpacaAccount,
    client: &reqwest::Client,
    broker_id: Option<&str>,
) -> anyhow::Result<serde_json::Value> {
    let account_info: Option<AccountInfo> =
        api_get(&client, &alpaca::broker_api_url_for(account, "/account"))
            .await
            .ok();
    let account_broker_id = account_info.as_ref().and_then(broker_account_id);
    let effective_broker_id = broker_id
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .or(account_broker_id);
    let canonicalized_rows =
        canonicalize_account_ref_for_broker(conn, account, effective_broker_id.as_deref())?;
    let provider_account_snapshot = match account_info.as_ref() {
        Some(info) => sync_provider_account_snapshot(
            conn,
            account,
            effective_broker_id.as_deref(),
            info,
            "provider_sync",
        )?,
        None => serde_json::json!({
            "status": "warning",
            "message": "Provider account snapshot was not refreshed during history sync.",
        }),
    };
    let provider_positions: Vec<alpaca::Position> =
        api_get(client, &alpaca::broker_api_url_for(account, "/positions")).await?;
    let provider_position_snapshot = sync_provider_position_snapshots(
        conn,
        account,
        effective_broker_id.as_deref(),
        &provider_positions,
    )?;
    let orders_seen =
        sync_provider_orders(conn, account, client, effective_broker_id.as_deref()).await?;
    let fills_seen =
        sync_provider_fills(conn, account, client, effective_broker_id.as_deref()).await?;
    let wash_sale_reconciliation =
        reconcile_wash_sales_for_tax_universe(conn, paper_flag(account))?;
    Ok(serde_json::json!({
        "status": "ok",
        "provider": account.provider(),
        "account_ref": account.account_ref(),
        "broker_account_id": effective_broker_id.as_deref().unwrap_or("not available"),
        "account_mode": alpaca::account_mode_for(account),
        "tax_universe": if account.is_paper() { "paper" } else { "real" },
        "canonicalized_account_ref_rows": canonicalized_rows,
        "provider_account_snapshot": provider_account_snapshot,
        "provider_position_snapshot": provider_position_snapshot,
        "provider_position_count": provider_positions.len(),
        "orders_seen": orders_seen,
        "fill_activities_seen": fills_seen,
        "wash_sale_reconciliation": wash_sale_reconciliation.to_json(),
        "orders": account_table_stats(conn, account, "provider_order_snapshots", "submitted_at")?,
        "order_origins": account_execution_origin_counts(conn, account, "provider_order_snapshots")?,
        "fill_activities": account_table_stats(conn, account, "provider_fill_activities", "transaction_time")?,
        "fill_origins": account_execution_origin_counts(conn, account, "provider_fill_activities")?,
        "synced_at_utc": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
    }))
}

// Synchronizes provider history for account with external or local state.
async fn sync_provider_history_for_account(
    conn: &Connection,
    account: &config::AlpacaAccount,
) -> anyhow::Result<serde_json::Value> {
    let client = build_client(account);
    sync_provider_history_with_context(conn, account, &client, None).await
}

// Synchronizes orders all accounts with external or local state.
pub async fn sync_orders_all_accounts(
    show_progress: bool,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let conn = open_db()?;
    init_auto_tables(&conn)?;
    let accounts = if config::provider_enabled("alpaca") {
        config::alpaca_accounts()?
    } else {
        Vec::new()
    };
    if accounts.is_empty() {
        anyhow::bail!(
            "No enabled Alpaca accounts found. Enable providers.alpaca and alpaca.accounts[] in {}.",
            config::config_path().display()
        );
    }
    let mut results = Vec::new();
    let progress = crate::progress::bar_if(
        show_progress,
        accounts.len() as u64,
        "Syncing provider order/fill history",
    );
    for account in &accounts {
        progress.set_message(format!("{}:{}", account.provider(), account.account_ref()));
        match sync_provider_history_for_account(&conn, account).await {
            Ok(result) => results.push(result),
            Err(err) => results.push(serde_json::json!({
                "status": "error",
                "provider": account.provider(),
                "account_ref": account.account_ref(),
                "account_mode": alpaca::account_mode_for(account),
                "tax_universe": if account.is_paper() { "paper" } else { "real" },
                "error": err.to_string(),
            })),
        }
        progress.inc(1);
    }
    progress.finish_and_clear();
    Ok(results)
}

// Handles the sync orders CLI action.
pub async fn cmd_sync_orders(json: bool) -> anyhow::Result<()> {
    let results = sync_orders_all_accounts(!json).await?;
    let status = if results
        .iter()
        .any(|result| result["status"].as_str() == Some("error"))
    {
        "partial_error"
    } else {
        "ok"
    };
    append_auto_log(serde_json::json!({
        "event": "auto_provider_order_sync",
        "source": invocation_source("cli"),
        "status": status,
        "account_count": results.len(),
        "accounts": results.clone(),
    }));
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "status": status,
                "accounts": results,
            }))?
        );
        return Ok(());
    }
    println!("Provider Order/Fill Sync - {}", status);
    println!("{}", "=".repeat(50));
    for result in &results {
        println!(
            "{}:{} [{} / {}]",
            result["provider"].as_str().unwrap_or("?"),
            result["account_ref"].as_str().unwrap_or("?"),
            result["account_mode"].as_str().unwrap_or("?"),
            result["tax_universe"].as_str().unwrap_or("?")
        );
        println!(
            "  Broker account ID:    {}",
            result["broker_account_id"]
                .as_str()
                .unwrap_or("not available")
        );
        if result["status"].as_str() == Some("error") {
            println!("  Error: {}", result["error"].as_str().unwrap_or("?"));
            continue;
        }
        let canonicalized = result["canonicalized_account_ref_rows"]
            .as_u64()
            .unwrap_or(0);
        if canonicalized > 0 {
            println!(
                "  Account rename cleanup: {} local rows moved to this account ref",
                canonicalized
            );
        }
        println!(
            "  Orders seen this sync: {} | local rows: {} | oldest: {} | newest: {}",
            result["orders_seen"].as_u64().unwrap_or(0),
            result["orders"]["local_count"].as_i64().unwrap_or(0),
            result["orders"]["oldest"].as_str().unwrap_or("none"),
            result["orders"]["newest"].as_str().unwrap_or("none")
        );
        println!(
            "    origins: {}",
            compact_origin_counts(&result["order_origins"])
        );
        println!(
            "  Fills seen this sync:  {} | local rows: {} | oldest: {} | newest: {}",
            result["fill_activities_seen"].as_u64().unwrap_or(0),
            result["fill_activities"]["local_count"]
                .as_i64()
                .unwrap_or(0),
            result["fill_activities"]["oldest"]
                .as_str()
                .unwrap_or("none"),
            result["fill_activities"]["newest"]
                .as_str()
                .unwrap_or("none")
        );
        println!(
            "    origins: {}",
            compact_origin_counts(&result["fill_origins"])
        );
        println!(
            "  Positions synced:     {} live provider positions",
            result["provider_position_count"].as_u64().unwrap_or(0)
        );
    }
    println!();
    println!("Compliance universe checks:");
    for universe in ["paper", "real"] {
        println!("  {} universe:", universe);
        let universe_accounts = results
            .iter()
            .filter(|result| result["tax_universe"].as_str() == Some(universe))
            .count();
        let wash_result = results.iter().find(|result| {
            result["tax_universe"].as_str() == Some(universe)
                && result["wash_sale_reconciliation"].is_object()
        });
        if let Some(result) = wash_result {
            let wash = &result["wash_sale_reconciliation"];
            println!(
                "    Wash-sale check: fills={} loss_sells={} inserted={} existing={}",
                wash["fills_scanned"].as_u64().unwrap_or(0),
                wash["loss_sells"].as_u64().unwrap_or(0),
                wash["windows_inserted"].as_u64().unwrap_or(0),
                wash["windows_existing"].as_u64().unwrap_or(0)
            );
        } else if universe_accounts > 0 {
            println!("    Wash-sale check: not run; provider sync failed");
        } else {
            println!(
                "    Wash-sale check: not run; no enabled {} accounts",
                universe
            );
        }
    }
    Ok(())
}

#[derive(Debug)]
struct ProviderSnapshotPosition {
    broker_account_id: Option<String>,
    qty: f64,
    avg_entry_price: f64,
    current_price: f64,
    market_value: f64,
}

// Reads a provider live-position snapshot after an account sync.
fn provider_position_snapshot(
    conn: &Connection,
    account: &config::AlpacaAccount,
    symbol: &str,
) -> anyhow::Result<Option<ProviderSnapshotPosition>> {
    conn.query_row(
        "SELECT broker_account_id, COALESCE(qty, 0.0),
                COALESCE(avg_entry_price, 0.0), COALESCE(current_price, 0.0),
                COALESCE(market_value, 0.0)
         FROM provider_position_snapshots
         WHERE provider=?1 AND account_ref=?2 AND paper_account=?3 AND UPPER(symbol)=?4",
        params![
            account.provider(),
            account.account_ref(),
            paper_flag(account),
            symbol
        ],
        |row| {
            Ok(ProviderSnapshotPosition {
                broker_account_id: row.get(0)?,
                qty: row.get(1)?,
                avg_entry_price: row.get(2)?,
                current_price: row.get(3)?,
                market_value: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

// Returns whether auto is already managing the account/symbol position.
fn open_auto_position_exists(
    conn: &Connection,
    account: &config::AlpacaAccount,
    symbol: &str,
) -> anyhow::Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM auto_positions
         WHERE provider=?1 AND account_ref=?2 AND paper_account=?3
           AND UPPER(symbol)=?4 AND status='open'",
        params![
            account.provider(),
            account.account_ref(),
            paper_flag(account),
            symbol
        ],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

// Adopts a provider-held position into auto management without rewriting buy history.
async fn track_account_position(
    conn: &Connection,
    account: &config::AlpacaAccount,
    symbol: &str,
    cfg: &StrategyConfig,
    schedule: &MarketSchedule,
) -> anyhow::Result<serde_json::Value> {
    let sync = sync_provider_history_for_account(conn, account).await?;
    let broker_id = sync["broker_account_id"]
        .as_str()
        .filter(|value| *value != "not available")
        .map(ToOwned::to_owned);
    let Some(snapshot) = provider_position_snapshot(conn, account, symbol)? else {
        anyhow::bail!(
            "{}:{} has no provider-held {} position to adopt.",
            account.provider(),
            account.account_ref(),
            symbol
        );
    };
    if snapshot.qty <= 0.0 {
        anyhow::bail!(
            "{}:{} provider-held {} quantity is not positive.",
            account.provider(),
            account.account_ref(),
            symbol
        );
    }
    let shares = snapshot.qty.floor() as i64;
    if shares <= 0 {
        anyhow::bail!(
            "{}:{} provider-held {} quantity {:.4} is below one whole share; auto only tracks whole-share positions.",
            account.provider(),
            account.account_ref(),
            symbol,
            snapshot.qty
        );
    }
    if snapshot.avg_entry_price <= 0.0 {
        anyhow::bail!(
            "{}:{} provider-held {} average entry price is not available.",
            account.provider(),
            account.account_ref(),
            symbol
        );
    }
    let effective_broker_id = snapshot
        .broker_account_id
        .as_deref()
        .or(broker_id.as_deref());
    if open_auto_position_exists(conn, account, symbol)? {
        set_position_management_override(
            conn,
            account,
            effective_broker_id,
            symbol,
            origin::ExecutionOrigin::MlaiAuto,
            true,
            "already_auto_tracked",
        )?;
        return Ok(serde_json::json!({
            "status": "already_tracked",
            "provider": account.provider(),
            "account_ref": account.account_ref(),
            "broker_account_id": effective_broker_id.unwrap_or("not available"),
            "symbol": symbol,
            "qty": snapshot.qty,
            "shares_tracked": shares,
            "message": "Position is already tracked by auto rules.",
        }));
    }

    let now_ts = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let stop_loss_price = snapshot.avg_entry_price * (1.0 - cfg.stop_loss_pct / 100.0);
    let take_profit_price = snapshot.avg_entry_price * (1.0 + cfg.take_profit_pct / 100.0);
    let exit_by = add_business_days(&today, cfg.max_hold_days);
    let (ml_quintile, ml_score, ml_date) = latest_ml_prediction_for_symbol(conn, symbol)?;
    let entry_execution_origin = provider_position_entry_origin(conn, account, symbol);
    let cost_basis = snapshot.avg_entry_price * shares as f64;
    conn.execute(
        "INSERT INTO auto_positions (
            provider, account_ref, broker_account_id, account_mode, paper_account,
            market_timezone, market_session_source, provider_market, provider_core_start,
            provider_core_end, symbol, entry_date, entry_timestamp, entry_price, shares,
            cost_basis, stop_loss_price, take_profit_price, exit_by_date, ml_quintile,
            ml_score, suggest_score, entry_signals, status, order_id,
            entry_execution_origin, execution_origin
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'manual_adopt', NULL, NULL, NULL,
                 ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, NULL,
                 ?18, 'open', NULL, ?19, ?20)",
        params![
            account.provider(),
            account.account_ref(),
            effective_broker_id,
            alpaca::account_mode_for(account),
            paper_flag(account),
            schedule.timezone_name,
            symbol,
            today,
            now_ts,
            snapshot.avg_entry_price,
            shares,
            cost_basis,
            stop_loss_price,
            take_profit_price,
            exit_by,
            ml_quintile,
            ml_score,
            serde_json::json!({
                "source": "manual_adopt",
                "provider_qty": snapshot.qty,
                "provider_market_value": snapshot.market_value,
                "provider_current_price": snapshot.current_price,
                "ml_prediction_date": ml_date,
            })
            .to_string(),
            entry_execution_origin.as_str(),
            entry_execution_origin.as_str(),
        ],
    )?;
    let auto_position_id = conn.last_insert_rowid();
    set_position_management_override(
        conn,
        account,
        effective_broker_id,
        symbol,
        origin::ExecutionOrigin::MlaiAuto,
        true,
        "manual_adopt",
    )?;
    let event = serde_json::json!({
        "event": "auto_position_tracking_changed",
        "level": "info",
        "action": "track",
        "source": invocation_source("cli"),
        "provider": account.provider(),
        "account_ref": account.account_ref(),
        "broker_account_id": effective_broker_id.unwrap_or("not available"),
        "account_mode": alpaca::account_mode_for(account),
        "tax_universe": if account.is_paper() { "paper" } else { "real" },
        "symbol": symbol,
        "auto_position_id": auto_position_id,
        "management_origin": origin::ExecutionOrigin::MlaiAuto.as_str(),
        "entry_execution_origin": entry_execution_origin.as_str(),
        "shares": shares,
        "avg_entry_price": snapshot.avg_entry_price,
        "stop_loss_price": stop_loss_price,
        "take_profit_price": take_profit_price,
        "exit_by_date": exit_by,
        "message": "Provider-held position adopted into auto tracking; provider buy history was not rewritten.",
    });
    append_auto_log(event.clone());
    Ok(event)
}

// Releases an auto-managed position to manual CLI ownership without selling it.
async fn untrack_account_position(
    conn: &Connection,
    account: &config::AlpacaAccount,
    symbol: &str,
) -> anyhow::Result<serde_json::Value> {
    let sync = sync_provider_history_for_account(conn, account).await?;
    let broker_id = sync["broker_account_id"]
        .as_str()
        .filter(|value| *value != "not available")
        .map(ToOwned::to_owned);
    let provider_snapshot = provider_position_snapshot(conn, account, symbol)?;
    if provider_snapshot.is_none() {
        anyhow::bail!(
            "{}:{} has no provider-held {} position to release.",
            account.provider(),
            account.account_ref(),
            symbol
        );
    }
    let updated = conn.execute(
        "UPDATE auto_positions
         SET status='manual', exit_reason='AUTO_TRACKING_RELEASED_TO_MANUAL',
             exit_execution_origin=NULL, execution_origin=entry_execution_origin
         WHERE provider=?1 AND account_ref=?2 AND paper_account=?3
           AND UPPER(symbol)=?4 AND status='open'",
        params![
            account.provider(),
            account.account_ref(),
            paper_flag(account),
            symbol
        ],
    )?;
    set_position_management_override(
        conn,
        account,
        broker_id.as_deref(),
        symbol,
        origin::ExecutionOrigin::MlaiCli,
        false,
        "manual_release",
    )?;
    let event = serde_json::json!({
        "event": "auto_position_tracking_changed",
        "level": "info",
        "action": "untrack",
        "source": invocation_source("cli"),
        "provider": account.provider(),
        "account_ref": account.account_ref(),
        "broker_account_id": broker_id.as_deref().unwrap_or("not available"),
        "account_mode": alpaca::account_mode_for(account),
        "tax_universe": if account.is_paper() { "paper" } else { "real" },
        "symbol": symbol,
        "auto_positions_released": updated,
        "management_origin": origin::ExecutionOrigin::MlaiCli.as_str(),
        "message": "Position released from auto tracking; provider position remains open and no order was submitted.",
    });
    append_auto_log(event.clone());
    Ok(event)
}

// CMD: auto track — adopt a provider-held position into auto management.
pub async fn cmd_auto_track(
    symbol: String,
    accounts: Vec<String>,
    json: bool,
) -> anyhow::Result<()> {
    let symbol = normalize_symbol(&symbol)?;
    let conn = open_db()?;
    init_auto_tables(&conn)?;
    let cfg = load_config(&conn);
    let schedule = load_market_schedule()?;
    let accounts = selected_auto_accounts(&accounts)?;
    let mut results = Vec::new();
    for account in &accounts {
        match track_account_position(&conn, account, &symbol, &cfg, &schedule).await {
            Ok(result) => results.push(result),
            Err(err) => results.push(serde_json::json!({
                "status": "error",
                "provider": account.provider(),
                "account_ref": account.account_ref(),
                "symbol": symbol,
                "error": err.to_string(),
            })),
        }
    }
    let status = if results
        .iter()
        .any(|result| result["status"].as_str() == Some("error"))
    {
        "partial_error"
    } else {
        "ok"
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "status": status,
                "action": "track",
                "symbol": symbol,
                "accounts": results,
            }))?
        );
    } else {
        println!("Auto position tracking - {}", status);
        println!("{}", "=".repeat(50));
        for result in &results {
            println!(
                "{}:{} {} -> {}",
                result["provider"].as_str().unwrap_or("?"),
                result["account_ref"].as_str().unwrap_or("?"),
                result["symbol"].as_str().unwrap_or(&symbol),
                result["status"].as_str().unwrap_or("ok")
            );
            if let Some(error) = result["error"].as_str() {
                println!("  Error: {}", error);
            } else {
                println!(
                    "  Management: mlai-auto | shares: {} | avg entry: ${:.2}",
                    result["shares"].as_i64().unwrap_or(0),
                    result["avg_entry_price"].as_f64().unwrap_or(0.0)
                );
                println!("  No provider order was submitted.");
            }
        }
    }
    Ok(())
}

// CMD: auto untrack — release an auto-managed position to manual CLI ownership.
pub async fn cmd_auto_untrack(
    symbol: String,
    accounts: Vec<String>,
    json: bool,
) -> anyhow::Result<()> {
    let symbol = normalize_symbol(&symbol)?;
    let conn = open_db()?;
    init_auto_tables(&conn)?;
    let accounts = selected_auto_accounts(&accounts)?;
    let mut results = Vec::new();
    for account in &accounts {
        match untrack_account_position(&conn, account, &symbol).await {
            Ok(result) => results.push(result),
            Err(err) => results.push(serde_json::json!({
                "status": "error",
                "provider": account.provider(),
                "account_ref": account.account_ref(),
                "symbol": symbol,
                "error": err.to_string(),
            })),
        }
    }
    let status = if results
        .iter()
        .any(|result| result["status"].as_str() == Some("error"))
    {
        "partial_error"
    } else {
        "ok"
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "status": status,
                "action": "untrack",
                "symbol": symbol,
                "accounts": results,
            }))?
        );
    } else {
        println!("Auto position release - {}", status);
        println!("{}", "=".repeat(50));
        for result in &results {
            println!(
                "{}:{} {} -> {}",
                result["provider"].as_str().unwrap_or("?"),
                result["account_ref"].as_str().unwrap_or("?"),
                result["symbol"].as_str().unwrap_or(&symbol),
                result["status"].as_str().unwrap_or("ok")
            );
            if let Some(error) = result["error"].as_str() {
                println!("  Error: {}", error);
            } else {
                println!(
                    "  Management: mlai-cli | auto positions released: {}",
                    result["auto_positions_released"].as_u64().unwrap_or(0)
                );
                println!("  No provider order was submitted.");
            }
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize, Serialize)]
struct AccountInfo {
    id: Option<String>,
    account_number: Option<String>,
    equity: Option<String>,
    cash: Option<String>,
    #[allow(dead_code)]
    portfolio_value: Option<String>,
    buying_power: Option<String>,
    status: Option<String>,
}

// Handles broker account id logic.
fn broker_account_id(info: &AccountInfo) -> Option<String> {
    info.id
        .as_ref()
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .or_else(|| {
            info.account_number
                .as_ref()
                .filter(|value| !value.trim().is_empty())
                .cloned()
        })
}

// Returns provider-held long symbols from the current broker position snapshot.
fn provider_position_symbols(positions: &[alpaca::Position]) -> HashSet<String> {
    positions
        .iter()
        .filter(|position| position.qty.parse::<f64>().unwrap_or(0.0) > 0.0)
        .map(|position| position.symbol.trim().to_ascii_uppercase())
        .filter(|symbol| !symbol.is_empty())
        .collect()
}

// Returns provider-held long share quantities by normalized symbol.
fn provider_position_qty_map(positions: &[alpaca::Position]) -> HashMap<String, f64> {
    positions
        .iter()
        .filter_map(|position| {
            let symbol = position.symbol.trim().to_ascii_uppercase();
            let qty = parse_provider_f64(Some(position.qty.as_str()));
            (!symbol.is_empty() && qty > 0.0).then_some((symbol, qty))
        })
        .collect()
}

// Parses provider number strings without trusting missing values.
fn parse_provider_f64(value: Option<&str>) -> f64 {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.0)
}

// Estimates long exposure from the provider's current positions snapshot.
fn provider_long_market_value(positions: &[alpaca::Position]) -> f64 {
    positions
        .iter()
        .filter_map(|position| {
            let qty = parse_provider_f64(Some(position.qty.as_str()));
            if qty <= 0.0 {
                return None;
            }
            let market_value = parse_provider_f64(position.market_value.as_deref()).abs();
            if market_value > 0.0 {
                return Some(market_value);
            }
            let current_price = parse_provider_f64(position.current_price.as_deref());
            if current_price > 0.0 {
                return Some(qty * current_price);
            }
            let entry_price = parse_provider_f64(position.avg_entry_price.as_deref());
            (entry_price > 0.0).then_some(qty * entry_price)
        })
        .sum()
}

#[derive(Debug, Clone, Copy)]
struct CashOnlyGuard {
    provider_cash: f64,
    equity: f64,
    provider_long_exposure: f64,
    local_auto_exposure: f64,
    pending_buy_reserved: f64,
    deployable_cash: f64,
}

impl CashOnlyGuard {
    // Emits audit fields for cash-only decisions.
    fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "provider_cash": round_money(self.provider_cash),
            "equity": round_money(self.equity),
            "provider_long_exposure": round_money(self.provider_long_exposure),
            "local_auto_exposure": round_money(self.local_auto_exposure),
            "pending_buy_reserved": round_money(self.pending_buy_reserved),
            "deployable_cash": round_money(self.deployable_cash),
            "rule": "cash-only deployable cash = min(provider cash, equity - max(provider long exposure, local auto exposure) - pending buy reservations)",
        })
    }
}

// Rounds money-like values for logs and JSON output.
fn round_money(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

// Returns the reserve value for an unfilled buy order.
fn pending_buy_order_value(
    row_qty: f64,
    row_filled_qty: f64,
    row_limit: f64,
    row_fill: f64,
) -> f64 {
    let remaining_qty = (row_qty - row_filled_qty.max(0.0)).max(0.0);
    if remaining_qty <= 0.0 {
        return 0.0;
    }
    let price = if row_limit > 0.0 { row_limit } else { row_fill };
    if price > 0.0 {
        remaining_qty * price
    } else {
        0.0
    }
}

// Reserves cash for provider buy orders that are not terminal yet.
fn pending_buy_reservations(
    conn: &Connection,
    account: &config::AlpacaAccount,
) -> anyhow::Result<f64> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(status,''), COALESCE(qty,0.0), COALESCE(filled_qty,0.0),
                COALESCE(limit_price,0.0), COALESCE(filled_avg_price,0.0)
         FROM provider_order_snapshots
         WHERE provider=?1 AND account_ref=?2 AND paper_account=?3
           AND LOWER(COALESCE(side,''))='buy'",
    )?;
    let rows = stmt.query_map(
        params![
            account.provider(),
            account.account_ref(),
            paper_flag(account)
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, f64>(2)?,
                row.get::<_, f64>(3)?,
                row.get::<_, f64>(4)?,
            ))
        },
    )?;
    let mut reserved = 0.0;
    for row in rows {
        let (status, qty, filled_qty, limit_price, filled_avg_price) = row?;
        let status = status.trim().to_ascii_lowercase();
        let terminal = matches!(
            status.as_str(),
            "filled" | "canceled" | "cancelled" | "expired" | "rejected" | "stopped"
        );
        if !terminal {
            reserved += pending_buy_order_value(qty, filled_qty, limit_price, filled_avg_price);
        }
    }
    Ok(reserved)
}

// Computes cash-only buying capacity from synchronized provider/local state.
fn cash_only_guard(
    conn: &Connection,
    account: &config::AlpacaAccount,
    provider_cash: f64,
    equity: f64,
    provider_positions: &[alpaca::Position],
    local_auto_exposure: f64,
) -> anyhow::Result<CashOnlyGuard> {
    let provider_long_exposure = provider_long_market_value(provider_positions);
    let pending_buy_reserved = pending_buy_reservations(conn, account)?;
    let exposure = provider_long_exposure.max(local_auto_exposure);
    let equity_remaining = equity - exposure - pending_buy_reserved;
    let deployable_cash = provider_cash.min(equity_remaining);
    Ok(CashOnlyGuard {
        provider_cash,
        equity,
        provider_long_exposure,
        local_auto_exposure,
        pending_buy_reserved,
        deployable_cash,
    })
}

// Formats UTC timestamps for DB/log fields with second precision.
fn utc_ts(now: DateTime<Utc>) -> String {
    now.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

// Parses DB/log UTC timestamps from current and legacy formats.
fn parse_utc_ts(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

// Converts position entry fields into an approximate UTC timestamp.
fn position_entry_time(entry_timestamp: Option<&str>, entry_date: &str) -> Option<DateTime<Utc>> {
    entry_timestamp.and_then(parse_utc_ts).or_else(|| {
        NaiveDate::parse_from_str(entry_date, "%Y-%m-%d")
            .ok()
            .and_then(|date| date.and_hms_opt(0, 0, 0))
            .map(|naive| DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
    })
}

// Returns elapsed whole minutes for confirmation windows.
fn elapsed_minutes(now: DateTime<Utc>, since: Option<&str>) -> Option<i64> {
    since
        .and_then(parse_utc_ts)
        .map(|start| (now - start).num_minutes().max(0))
}

// Evaluates stop-loss and take-profit confirmation state without placing orders.
fn evaluate_confirmed_exit(
    cfg: &StrategyConfig,
    mut state: ExitConfirmationState,
    now: DateTime<Utc>,
    entry_time: Option<DateTime<Utc>>,
    current_price: f64,
    entry_price: f64,
    stop_loss: Option<f64>,
    take_profit: Option<f64>,
) -> ExitConfirmationDecision {
    let now_text = utc_ts(now);
    let pnl_pct = (current_price / entry_price - 1.0) * 100.0;

    if let Some(sl) = stop_loss {
        if current_price <= sl {
            if cfg.stop_loss_confirmation.emergency_stop_loss_pct > 0.0
                && pnl_pct <= -cfg.stop_loss_confirmation.emergency_stop_loss_pct
            {
                state.stop_loss_breach_count += 1;
                if state.stop_loss_first_breach_at.is_none() {
                    state.stop_loss_first_breach_at = Some(now_text);
                }
                return ExitConfirmationDecision {
                    reason: Some(format!(
                        "STOP_LOSS_EMERGENCY ({:.2}%, threshold -{:.1}%)",
                        pnl_pct, cfg.stop_loss_confirmation.emergency_stop_loss_pct
                    )),
                    state,
                    note: None,
                    rule: Some("emergency_stop_loss".to_string()),
                    cycles_remaining: Some(0),
                    minutes_remaining: Some(0),
                };
            }
            if !cfg.stop_loss_confirmation.enabled || cfg.stop_loss_confirmation.cycles <= 1 {
                state.stop_loss_breach_count += 1;
                if state.stop_loss_first_breach_at.is_none() {
                    state.stop_loss_first_breach_at = Some(now_text);
                }
                return ExitConfirmationDecision {
                    reason: Some(format!("STOP_LOSS ({:.2}%)", pnl_pct)),
                    state,
                    note: None,
                    rule: Some("stop_loss".to_string()),
                    cycles_remaining: Some(0),
                    minutes_remaining: Some(0),
                };
            }

            state.stop_loss_breach_count += 1;
            if state.stop_loss_first_breach_at.is_none() {
                state.stop_loss_first_breach_at = Some(now_text);
            }
            let waited_minutes =
                elapsed_minutes(now, state.stop_loss_first_breach_at.as_deref()).unwrap_or(0);
            if state.stop_loss_breach_count >= cfg.stop_loss_confirmation.cycles
                || (cfg.stop_loss_confirmation.max_confirmation_minutes > 0
                    && waited_minutes >= cfg.stop_loss_confirmation.max_confirmation_minutes)
            {
                return ExitConfirmationDecision {
                    reason: Some(format!(
                        "STOP_LOSS_CONFIRMED ({:.2}%, {}/{} cycles)",
                        pnl_pct, state.stop_loss_breach_count, cfg.stop_loss_confirmation.cycles
                    )),
                    state,
                    note: None,
                    rule: Some("stop_loss_confirmation".to_string()),
                    cycles_remaining: Some(0),
                    minutes_remaining: Some(0),
                };
            }
            let cycles_remaining =
                (cfg.stop_loss_confirmation.cycles - state.stop_loss_breach_count).max(0);
            let minutes_remaining = if cfg.stop_loss_confirmation.max_confirmation_minutes > 0 {
                Some((cfg.stop_loss_confirmation.max_confirmation_minutes - waited_minutes).max(0))
            } else {
                None
            };
            return ExitConfirmationDecision {
                reason: None,
                note: Some(format!(
                    "stop-loss confirmation {}/{} at {:.2}% loss",
                    state.stop_loss_breach_count, cfg.stop_loss_confirmation.cycles, -pnl_pct
                )),
                state,
                rule: Some("stop_loss_confirmation".to_string()),
                cycles_remaining: Some(cycles_remaining),
                minutes_remaining,
            };
        }
    }
    state.stop_loss_breach_count = 0;
    state.stop_loss_first_breach_at = None;

    let held_minutes = entry_time.map(|entry| (now - entry).num_minutes().max(0));
    let min_hold_ok = held_minutes
        .map(|minutes| minutes >= cfg.take_profit_confirmation.min_hold_minutes)
        .unwrap_or(cfg.take_profit_confirmation.min_hold_minutes == 0);
    let take_profit_seen = take_profit.map(|tp| current_price >= tp).unwrap_or(false);
    if take_profit_seen {
        state.take_profit_breach_count += 1;
        if state.take_profit_first_breach_at.is_none() {
            state.take_profit_first_breach_at = Some(now_text);
        }
        if state
            .take_profit_peak_pct
            .map(|peak| pnl_pct > peak)
            .unwrap_or(true)
        {
            state.take_profit_peak_pct = Some(pnl_pct);
            state.take_profit_peak_price = Some(current_price);
        }
        if !cfg.take_profit_confirmation.enabled {
            return ExitConfirmationDecision {
                reason: Some(format!("TAKE_PROFIT ({:+.2}%)", pnl_pct)),
                state,
                note: None,
                rule: Some("take_profit".to_string()),
                cycles_remaining: Some(0),
                minutes_remaining: Some(0),
            };
        }
    } else {
        state.take_profit_breach_count = 0;
        state.take_profit_first_breach_at = None;
        if state.take_profit_peak_pct.is_none() {
            state.take_profit_peak_price = None;
        }
    }

    if cfg.take_profit_confirmation.enabled
        && cfg.take_profit_confirmation.trailing_enabled
        && min_hold_ok
    {
        if let Some(peak_pct) = state.take_profit_peak_pct {
            let giveback = peak_pct - pnl_pct;
            if giveback >= cfg.take_profit_confirmation.trailing_giveback_pct {
                return ExitConfirmationDecision {
                    reason: Some(format!(
                        "TAKE_PROFIT_TRAIL ({:+.2}%, peak {:+.2}%, giveback {:.1}%)",
                        pnl_pct, peak_pct, cfg.take_profit_confirmation.trailing_giveback_pct
                    )),
                    state,
                    note: None,
                    rule: Some("take_profit_trailing".to_string()),
                    cycles_remaining: Some(0),
                    minutes_remaining: Some(0),
                };
            }
        }
    }

    if take_profit_seen && min_hold_ok {
        if state.take_profit_breach_count >= cfg.take_profit_confirmation.cycles {
            return ExitConfirmationDecision {
                reason: Some(format!(
                    "TAKE_PROFIT_CONFIRMED ({:+.2}%, {}/{} cycles)",
                    pnl_pct, state.take_profit_breach_count, cfg.take_profit_confirmation.cycles
                )),
                state,
                note: None,
                rule: Some("take_profit_confirmation".to_string()),
                cycles_remaining: Some(0),
                minutes_remaining: Some(0),
            };
        }
    }

    let mut rule = None;
    let mut cycles_remaining = None;
    let mut minutes_remaining = None;
    let note = if take_profit_seen {
        if min_hold_ok {
            rule = Some("take_profit_confirmation".to_string());
            cycles_remaining =
                Some((cfg.take_profit_confirmation.cycles - state.take_profit_breach_count).max(0));
            Some(format!(
                "take-profit confirmation {}/{} at {:+.2}%",
                state.take_profit_breach_count, cfg.take_profit_confirmation.cycles, pnl_pct
            ))
        } else {
            rule = Some("take_profit_min_hold".to_string());
            minutes_remaining = Some(
                (cfg.take_profit_confirmation.min_hold_minutes - held_minutes.unwrap_or(0)).max(0),
            );
            Some(format!(
                "take-profit min-hold waiting {}m/{}m at {:+.2}%",
                held_minutes.unwrap_or(0),
                cfg.take_profit_confirmation.min_hold_minutes,
                pnl_pct
            ))
        }
    } else {
        None
    };

    ExitConfirmationDecision {
        reason: None,
        state,
        note,
        rule,
        cycles_remaining,
        minutes_remaining,
    }
}

// Persists exit-confirmation state after a non-trading decision.
fn update_exit_confirmation_state(
    conn: &Connection,
    account: &config::AlpacaAccount,
    pos_id: i64,
    state: &ExitConfirmationState,
) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE auto_positions
         SET stop_loss_breach_count=?1, stop_loss_first_breach_at=?2,
             take_profit_breach_count=?3, take_profit_first_breach_at=?4,
             take_profit_peak_pct=?5, take_profit_peak_price=?6
         WHERE id=?7 AND provider=?8 AND account_ref=?9 AND paper_account=?10",
        params![
            state.stop_loss_breach_count,
            state.stop_loss_first_breach_at.as_deref(),
            state.take_profit_breach_count,
            state.take_profit_first_breach_at.as_deref(),
            state.take_profit_peak_pct,
            state.take_profit_peak_price,
            pos_id,
            account.provider(),
            account.account_ref(),
            paper_flag(account)
        ],
    )?;
    Ok(())
}

// Finds the latest provider-confirmed sell fill for a local auto position.
fn latest_provider_sell_exit(
    conn: &Connection,
    account: &config::AlpacaAccount,
    position: &OpenAutoPosition,
) -> Option<ProviderExitFill> {
    let entry_after = position
        .entry_timestamp
        .as_deref()
        .and_then(parse_utc_ts)
        .or_else(|| position_entry_time(None, &position.entry_date))
        .map(utc_ts)
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());
    conn.query_row(
        "SELECT MAX(COALESCE(transaction_time, synced_at_utc)),
                CASE
                    WHEN SUM(COALESCE(qty, 0.0)) > 0.0
                    THEN SUM(COALESCE(price, 0.0) * COALESCE(qty, 0.0)) / SUM(COALESCE(qty, 0.0))
                    ELSE MAX(COALESCE(price, 0.0))
                END,
                SUM(COALESCE(qty, 0.0)),
                NULLIF(order_id, ''),
                COALESCE(execution_origin, 'provider_external')
         FROM provider_fill_activities
         WHERE provider=?1 AND account_ref=?2 AND paper_account=?3
           AND UPPER(symbol)=UPPER(?4) AND UPPER(COALESCE(side,''))='SELL'
           AND COALESCE(transaction_time, synced_at_utc) >= ?5
         GROUP BY COALESCE(NULLIF(order_id, ''), activity_id)
         ORDER BY MAX(COALESCE(transaction_time, synced_at_utc)) DESC
         LIMIT 1",
        params![
            account.provider(),
            account.account_ref(),
            paper_flag(account),
            position.symbol,
            entry_after
        ],
        |row| {
            Ok(ProviderExitFill {
                timestamp: row
                    .get::<_, Option<String>>(0)?
                    .unwrap_or_else(|| entry_after.clone()),
                price: row.get(1)?,
                qty: row.get(2)?,
                order_id: row.get(3)?,
                execution_origin: origin::ExecutionOrigin::parse(
                    &row.get::<_, String>(4)
                        .unwrap_or_else(|_| "provider_external".to_string()),
                ),
            })
        },
    )
    .ok()
    .or_else(|| {
        conn.query_row(
            "SELECT COALESCE(filled_at, updated_at, submitted_at, synced_at_utc),
                    COALESCE(filled_avg_price, limit_price, 0.0),
                    COALESCE(filled_qty, qty, 0.0),
                    order_id,
                    COALESCE(execution_origin, 'provider_external')
             FROM provider_order_snapshots
             WHERE provider=?1 AND account_ref=?2 AND paper_account=?3
               AND UPPER(symbol)=UPPER(?4) AND UPPER(COALESCE(side,''))='SELL'
               AND UPPER(COALESCE(status,''))='FILLED'
               AND COALESCE(filled_at, updated_at, submitted_at, synced_at_utc) >= ?5
             ORDER BY COALESCE(filled_at, updated_at, submitted_at, synced_at_utc) DESC
             LIMIT 1",
            params![
                account.provider(),
                account.account_ref(),
                paper_flag(account),
                position.symbol,
                entry_after
            ],
            |row| {
                Ok(ProviderExitFill {
                    timestamp: row
                        .get::<_, Option<String>>(0)?
                        .unwrap_or_else(|| entry_after.clone()),
                    price: row.get(1)?,
                    qty: row.get(2)?,
                    order_id: row.get(3)?,
                    execution_origin: origin::ExecutionOrigin::parse(
                        &row.get::<_, String>(4)
                            .unwrap_or_else(|_| "provider_external".to_string()),
                    ),
                })
            },
        )
        .ok()
    })
}

// Reconciles local open auto positions against provider source-of-truth shares.
fn reconcile_open_positions_with_provider(
    conn: &Connection,
    account: &config::AlpacaAccount,
    broker_account_id: Option<&str>,
    positions: &mut Vec<OpenAutoPosition>,
    provider_positions: &[alpaca::Position],
    now_ts: &str,
    source: &str,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let provider_qty = provider_position_qty_map(provider_positions);
    let mut kept = Vec::with_capacity(positions.len());
    let mut reconciled = Vec::new();

    for mut position in positions.drain(..) {
        let symbol = position.symbol.trim().to_ascii_uppercase();
        let provider_shares = provider_qty.get(&symbol).copied().unwrap_or(0.0);
        if provider_shares < 1.0 {
            let provider_exit = latest_provider_sell_exit(conn, account, &position);
            let exit_timestamp = provider_exit
                .as_ref()
                .map(|fill| fill.timestamp.clone())
                .unwrap_or_else(|| now_ts.to_string());
            let exit_date = parse_utc_ts(&exit_timestamp)
                .map(|ts| ts.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string());
            let exit_price = provider_exit.as_ref().and_then(|fill| {
                if fill.price > 0.0 {
                    Some(fill.price)
                } else {
                    None
                }
            });
            let pnl =
                exit_price.map(|price| (price - position.entry_price) * position.shares as f64);
            let pnl_pct = exit_price.map(|price| (price / position.entry_price - 1.0) * 100.0);
            let exit_order_id = provider_exit
                .as_ref()
                .and_then(|fill| fill.order_id.as_deref())
                .filter(|order_id| !order_id.trim().is_empty());
            let exit_execution_origin = provider_exit
                .as_ref()
                .map(|fill| fill.execution_origin)
                .unwrap_or(origin::ExecutionOrigin::ProviderExternal);
            let combined_origin =
                origin::combine(position.entry_execution_origin, exit_execution_origin);
            let reason = "PROVIDER_SYNC_CLOSED (provider reports no long position)";
            conn.execute(
                "UPDATE auto_positions
                 SET status='closed', exit_date=?1, exit_price=?2, exit_reason=?3,
                     pnl=?4, pnl_pct=?5, exit_order_id=COALESCE(?6, exit_order_id),
                     exit_execution_origin=?7, execution_origin=?8
                 WHERE id=?9 AND provider=?10 AND account_ref=?11 AND paper_account=?12",
                params![
                    exit_date,
                    exit_price,
                    reason,
                    pnl,
                    pnl_pct,
                    exit_order_id,
                    exit_execution_origin.as_str(),
                    combined_origin.as_str(),
                    position.id,
                    account.provider(),
                    account.account_ref(),
                    paper_flag(account)
                ],
            )?;
            let event = serde_json::json!({
                "event": "auto_position_reconciled_from_provider",
                "level": "warn",
                "source": source,
                "provider": account.provider(),
                "account_ref": account.account_ref(),
                "broker_account_id": broker_account_id.unwrap_or("not available"),
                "account_mode": alpaca::account_mode_for(account),
                "tax_universe": if account.is_paper() { "paper" } else { "real" },
                "symbol": position.symbol.as_str(),
                "auto_position_id": position.id,
                "local_shares": position.shares,
                "provider_shares": provider_shares,
                "status": "closed",
                "reason": reason,
                "provider_exit_timestamp": exit_timestamp,
                "provider_exit_price": exit_price
                    .map(serde_json::Value::from)
                    .unwrap_or_else(|| serde_json::json!("not available")),
                "provider_exit_qty": provider_exit
                    .as_ref()
                    .map(|fill| serde_json::json!(fill.qty))
                    .unwrap_or_else(|| serde_json::json!("not available")),
                "provider_exit_order_id": exit_order_id.unwrap_or("not available"),
                "entry_execution_origin": position.entry_execution_origin.as_str(),
                "exit_execution_origin": exit_execution_origin.as_str(),
                "execution_origin": combined_origin.as_str(),
            });
            append_auto_log(event.clone());
            reconciled.push(event);
            continue;
        }

        if provider_shares + f64::EPSILON < position.shares as f64 {
            let local_shares_before = position.shares;
            let adjusted_shares = provider_shares.floor() as i64;
            let sold_shares = (local_shares_before - adjusted_shares).max(0);
            let partial_provider_exit = if sold_shares > 0 {
                latest_provider_sell_exit(conn, account, &position)
            } else {
                None
            };
            let exit_execution_origin = partial_provider_exit
                .as_ref()
                .map(|fill| fill.execution_origin)
                .unwrap_or(origin::ExecutionOrigin::ProviderExternal);
            let combined_origin =
                origin::combine(position.entry_execution_origin, exit_execution_origin);
            let reason = "PROVIDER_SYNC_PARTIAL (provider reports fewer shares)";
            if sold_shares > 0 {
                if let Some(provider_exit) = partial_provider_exit.as_ref() {
                    let exit_date = parse_utc_ts(&provider_exit.timestamp)
                        .map(|ts| ts.format("%Y-%m-%d").to_string())
                        .unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string());
                    conn.execute(
                        "INSERT INTO auto_positions (
                            provider, account_ref, broker_account_id, account_mode, paper_account,
                            market_timezone, market_session_source, provider_market, provider_core_start,
                            provider_core_end, symbol, entry_date, entry_timestamp, entry_price, shares,
                            cost_basis, stop_loss_price, take_profit_price, exit_by_date, ml_quintile,
                            ml_score, suggest_score, entry_signals, status, exit_date, exit_price,
                            exit_reason, pnl, pnl_pct, order_id, exit_order_id,
                            entry_execution_origin, exit_execution_origin, execution_origin
                         )
                         SELECT provider, account_ref, broker_account_id, account_mode, paper_account,
                            market_timezone, market_session_source, provider_market, provider_core_start,
                            provider_core_end, symbol, entry_date, entry_timestamp, entry_price, ?1,
                            entry_price * ?1, stop_loss_price, take_profit_price, exit_by_date, ml_quintile,
                            ml_score, suggest_score, entry_signals, 'closed', ?2, ?3,
                            ?4, (?3 - entry_price) * ?1, ((?3 / entry_price) - 1.0) * 100.0,
                            order_id, ?5, entry_execution_origin, ?6, ?7
                         FROM auto_positions
                         WHERE id=?8 AND provider=?9 AND account_ref=?10 AND paper_account=?11",
                        params![
                            sold_shares,
                            exit_date,
                            provider_exit.price,
                            reason,
                            provider_exit.order_id.as_deref(),
                            exit_execution_origin.as_str(),
                            combined_origin.as_str(),
                            position.id,
                            account.provider(),
                            account.account_ref(),
                            paper_flag(account)
                        ],
                    )?;
                }
            }
            position.shares = adjusted_shares;
            position.cost_basis = position.entry_price * adjusted_shares as f64;
            conn.execute(
                "UPDATE auto_positions
                 SET shares=?1, cost_basis=?2
                 WHERE id=?3 AND provider=?4 AND account_ref=?5 AND paper_account=?6",
                params![
                    position.shares,
                    position.cost_basis,
                    position.id,
                    account.provider(),
                    account.account_ref(),
                    paper_flag(account)
                ],
            )?;
            let event = serde_json::json!({
                "event": "auto_position_reconciled_from_provider",
                "level": "warn",
                "source": source,
                "provider": account.provider(),
                "account_ref": account.account_ref(),
                "broker_account_id": broker_account_id.unwrap_or("not available"),
                "account_mode": alpaca::account_mode_for(account),
                "tax_universe": if account.is_paper() { "paper" } else { "real" },
                "symbol": position.symbol.as_str(),
                "auto_position_id": position.id,
                "local_shares_before": local_shares_before,
                "local_shares_after": position.shares,
                "provider_shares": provider_shares,
                "status": "shares_adjusted",
                "reason": "PROVIDER_SYNC_ADJUSTED (provider reports fewer shares)",
                "partial_closed_qty": sold_shares,
                "provider_exit_timestamp": partial_provider_exit
                    .as_ref()
                    .map(|fill| serde_json::json!(fill.timestamp.as_str()))
                    .unwrap_or_else(|| serde_json::json!("not available")),
                "provider_exit_price": partial_provider_exit
                    .as_ref()
                    .map(|fill| serde_json::json!(fill.price))
                    .unwrap_or_else(|| serde_json::json!("not available")),
                "provider_exit_qty": partial_provider_exit
                    .as_ref()
                    .map(|fill| serde_json::json!(fill.qty))
                    .unwrap_or_else(|| serde_json::json!("not available")),
                "provider_exit_order_id": partial_provider_exit
                    .as_ref()
                    .and_then(|fill| fill.order_id.as_deref())
                    .unwrap_or("not available"),
                "entry_execution_origin": position.entry_execution_origin.as_str(),
                "exit_execution_origin": exit_execution_origin.as_str(),
                "execution_origin": combined_origin.as_str(),
            });
            append_auto_log(event.clone());
            reconciled.push(event);
            kept.push(position);
            continue;
        }

        kept.push(position);
    }

    *positions = kept;
    Ok(reconciled)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Builds a complete strategy config for pure exit-rule simulations.
    fn test_strategy_config() -> StrategyConfig {
        StrategyConfig {
            max_positions: DEF_MAX_POSITIONS,
            position_size_pct: DEF_POSITION_SIZE_PCT,
            stop_loss_pct: DEF_STOP_LOSS_PCT,
            take_profit_pct: DEF_TAKE_PROFIT_PCT,
            stop_loss_confirmation: StopLossConfirmation {
                enabled: true,
                cycles: 3,
                max_confirmation_minutes: 5,
                emergency_stop_loss_pct: 10.0,
            },
            take_profit_confirmation: TakeProfitConfirmation {
                enabled: true,
                cycles: 3,
                min_hold_minutes: 5,
                trailing_enabled: true,
                trailing_giveback_pct: 3.0,
            },
            max_hold_days: DEF_MAX_HOLD_DAYS,
            min_price: DEF_MIN_PRICE,
            min_avg_volume: DEF_MIN_AVG_VOLUME,
            max_spread_bps: DEF_MAX_SPREAD_BPS,
            min_quote_size: DEF_MIN_QUOTE_SIZE,
            allow_bar_price_fallback: DEF_ALLOW_BAR_PRICE_FALLBACK,
            bar_fallback_bps: DEF_BAR_FALLBACK_BPS,
            ml_quintile_buy: DEF_ML_QUINTILE_BUY,
            ml_quintile_exit: DEF_ML_QUINTILE_EXIT,
            wash_sale_safety_buffer_days: 1,
        }
    }

    // Parses test UTC timestamps.
    fn test_ts(value: &str) -> DateTime<Utc> {
        parse_utc_ts(value).expect("valid UTC timestamp")
    }

    #[test]
    fn provider_long_market_value_prefers_provider_market_value() {
        let positions = vec![
            alpaca::Position {
                symbol: "AAPL".to_string(),
                qty: "10".to_string(),
                avg_entry_price: Some("90".to_string()),
                current_price: Some("101".to_string()),
                market_value: Some("1000".to_string()),
                unrealized_pl: None,
                unrealized_plpc: None,
                asset_class: None,
                exchange: None,
                side: None,
            },
            alpaca::Position {
                symbol: "MSFT".to_string(),
                qty: "5".to_string(),
                avg_entry_price: Some("20".to_string()),
                current_price: Some("30".to_string()),
                market_value: None,
                unrealized_pl: None,
                unrealized_plpc: None,
                asset_class: None,
                exchange: None,
                side: None,
            },
        ];
        assert_eq!(provider_long_market_value(&positions), 1150.0);
    }

    #[test]
    fn provider_position_qty_map_uses_only_positive_long_shares() {
        let positions = vec![
            alpaca::Position {
                symbol: "damd".to_string(),
                qty: "1441".to_string(),
                avg_entry_price: None,
                current_price: None,
                market_value: None,
                unrealized_pl: None,
                unrealized_plpc: None,
                asset_class: None,
                exchange: None,
                side: None,
            },
            alpaca::Position {
                symbol: "ZERO".to_string(),
                qty: "0".to_string(),
                avg_entry_price: None,
                current_price: None,
                market_value: None,
                unrealized_pl: None,
                unrealized_plpc: None,
                asset_class: None,
                exchange: None,
                side: None,
            },
            alpaca::Position {
                symbol: "SHORT".to_string(),
                qty: "-5".to_string(),
                avg_entry_price: None,
                current_price: None,
                market_value: None,
                unrealized_pl: None,
                unrealized_plpc: None,
                asset_class: None,
                exchange: None,
                side: None,
            },
        ];
        let map = provider_position_qty_map(&positions);
        assert_eq!(map.get("DAMD").copied(), Some(1441.0));
        assert!(!map.contains_key("ZERO"));
        assert!(!map.contains_key("SHORT"));
    }

    #[test]
    fn pending_buy_order_value_reserves_unfilled_limit_qty() {
        assert_eq!(pending_buy_order_value(100.0, 25.0, 10.0, 0.0), 750.0);
        assert_eq!(pending_buy_order_value(100.0, 100.0, 10.0, 0.0), 0.0);
        assert_eq!(pending_buy_order_value(10.0, 0.0, 0.0, 12.0), 120.0);
    }

    #[test]
    fn stop_loss_waits_for_configured_confirmation_cycles() {
        let cfg = test_strategy_config();
        let entry_time = Some(test_ts("2026-05-06T14:00:00Z"));
        let first = evaluate_confirmed_exit(
            &cfg,
            ExitConfirmationState::default(),
            test_ts("2026-05-06T14:01:00Z"),
            entry_time,
            92.0,
            100.0,
            Some(93.0),
            Some(115.0),
        );
        assert!(first.reason.is_none());
        assert_eq!(first.cycles_remaining, Some(2));

        let second = evaluate_confirmed_exit(
            &cfg,
            first.state,
            test_ts("2026-05-06T14:02:00Z"),
            entry_time,
            92.0,
            100.0,
            Some(93.0),
            Some(115.0),
        );
        assert!(second.reason.is_none());
        assert_eq!(second.cycles_remaining, Some(1));

        let third = evaluate_confirmed_exit(
            &cfg,
            second.state,
            test_ts("2026-05-06T14:03:00Z"),
            entry_time,
            92.0,
            100.0,
            Some(93.0),
            Some(115.0),
        );
        assert!(third
            .reason
            .as_deref()
            .unwrap_or_default()
            .starts_with("STOP_LOSS_CONFIRMED"));
    }

    #[test]
    fn emergency_stop_loss_sells_immediately() {
        let cfg = test_strategy_config();
        let decision = evaluate_confirmed_exit(
            &cfg,
            ExitConfirmationState::default(),
            test_ts("2026-05-06T14:01:00Z"),
            Some(test_ts("2026-05-06T14:00:00Z")),
            89.0,
            100.0,
            Some(93.0),
            Some(115.0),
        );
        assert!(decision
            .reason
            .as_deref()
            .unwrap_or_default()
            .starts_with("STOP_LOSS_EMERGENCY"));
    }

    #[test]
    fn take_profit_waits_for_hold_and_confirmation_cycles() {
        let cfg = test_strategy_config();
        let entry_time = Some(test_ts("2026-05-06T14:00:00Z"));
        let first = evaluate_confirmed_exit(
            &cfg,
            ExitConfirmationState::default(),
            test_ts("2026-05-06T14:10:00Z"),
            entry_time,
            116.0,
            100.0,
            Some(93.0),
            Some(115.0),
        );
        assert!(first.reason.is_none());
        assert_eq!(first.cycles_remaining, Some(2));

        let second = evaluate_confirmed_exit(
            &cfg,
            first.state,
            test_ts("2026-05-06T14:11:00Z"),
            entry_time,
            116.0,
            100.0,
            Some(93.0),
            Some(115.0),
        );
        assert!(second.reason.is_none());

        let third = evaluate_confirmed_exit(
            &cfg,
            second.state,
            test_ts("2026-05-06T14:12:00Z"),
            entry_time,
            116.0,
            100.0,
            Some(93.0),
            Some(115.0),
        );
        assert!(third
            .reason
            .as_deref()
            .unwrap_or_default()
            .starts_with("TAKE_PROFIT_CONFIRMED"));
    }

    #[test]
    fn trailing_take_profit_sells_after_configured_giveback() {
        let cfg = test_strategy_config();
        let entry_time = Some(test_ts("2026-05-06T14:00:00Z"));
        let peak = evaluate_confirmed_exit(
            &cfg,
            ExitConfirmationState::default(),
            test_ts("2026-05-06T14:10:00Z"),
            entry_time,
            118.0,
            100.0,
            Some(93.0),
            Some(115.0),
        );
        assert!(peak.reason.is_none());
        assert!((peak.state.take_profit_peak_pct.unwrap_or_default() - 18.0).abs() < 0.0001);

        let pullback = evaluate_confirmed_exit(
            &cfg,
            peak.state,
            test_ts("2026-05-06T14:11:00Z"),
            entry_time,
            114.0,
            100.0,
            Some(93.0),
            Some(115.0),
        );
        assert!(pullback
            .reason
            .as_deref()
            .unwrap_or_default()
            .starts_with("TAKE_PROFIT_TRAIL"));
    }
}

#[derive(Debug, Serialize)]
struct OrderRequest {
    symbol: String,
    qty: String,
    side: String,
    r#type: String,
    time_in_force: String,
    client_order_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit_price: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OrderResponse {
    id: Option<String>,
    #[allow(dead_code)]
    filled_avg_price: Option<String>,
}

#[derive(Debug, Deserialize)]
struct QuoteResponse {
    quote: Option<QuoteData>,
}

#[derive(Debug, Deserialize)]
struct QuoteData {
    #[serde(rename = "bp")]
    bid_price: Option<f64>,
    #[serde(rename = "ap")]
    ask_price: Option<f64>,
    #[serde(rename = "bs")]
    bid_size: Option<f64>,
    #[serde(rename = "as")]
    ask_size: Option<f64>,
}

#[derive(Debug, Clone)]
struct LiveQuote {
    feed: String,
    bid_price: f64,
    ask_price: f64,
    bid_size: Option<f64>,
    ask_size: Option<f64>,
}

impl LiveQuote {
    // Handles from quote logic.
    fn from_quote(symbol: &str, feed: &str, quote: QuoteData) -> anyhow::Result<Self> {
        let bid_price = quote.bid_price.unwrap_or(0.0);
        let ask_price = quote.ask_price.unwrap_or(0.0);
        if bid_price <= 0.0 || ask_price <= 0.0 {
            anyhow::bail!("No executable NBBO quote for {}", symbol);
        }
        if ask_price < bid_price {
            anyhow::bail!(
                "Invalid NBBO quote for {}: ask {:.4} below bid {:.4}",
                symbol,
                ask_price,
                bid_price
            );
        }
        Ok(Self {
            feed: feed.to_string(),
            bid_price,
            ask_price,
            bid_size: quote.bid_size,
            ask_size: quote.ask_size,
        })
    }

    // Handles mid price logic.
    fn mid_price(&self) -> f64 {
        (self.bid_price + self.ask_price) / 2.0
    }

    // Handles spread bps logic.
    fn spread_bps(&self) -> f64 {
        let mid = self.mid_price();
        if mid > 0.0 {
            (self.ask_price - self.bid_price) / mid * 10_000.0
        } else {
            f64::INFINITY
        }
    }

    // Handles buy price logic.
    fn buy_price(&self) -> f64 {
        self.ask_price
    }

    // Handles sell price logic.
    fn sell_price(&self) -> f64 {
        self.bid_price
    }

    // Handles entry reject reason logic.
    fn entry_reject_reason(&self, cfg: &StrategyConfig) -> Option<String> {
        let spread_bps = self.spread_bps();
        if spread_bps > cfg.max_spread_bps {
            return Some(format!(
                "spread {:.1} bps exceeds max {:.1} bps",
                spread_bps, cfg.max_spread_bps
            ));
        }
        if let Some(ask_size) = self.ask_size {
            if ask_size < cfg.min_quote_size {
                return Some(format!(
                    "ask size {:.0} below min {:.0}",
                    ask_size, cfg.min_quote_size
                ));
            }
        }
        None
    }
}

#[derive(Debug, Clone, Copy)]
enum ExecutionSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone)]
struct ExecutionPrice {
    price: f64,
    source: &'static str,
    bid_price: Option<f64>,
    ask_price: Option<f64>,
    bid_size: Option<f64>,
    ask_size: Option<f64>,
    spread_bps: Option<f64>,
    quote_feed: Option<String>,
    bar_close: Option<f64>,
    fallback_bps: Option<f64>,
}

impl ExecutionPrice {
    // Handles from quote logic.
    fn from_quote(side: ExecutionSide, quote: LiveQuote) -> Self {
        Self {
            price: match side {
                ExecutionSide::Buy => quote.buy_price(),
                ExecutionSide::Sell => quote.sell_price(),
            },
            source: "sip_or_configured_quote",
            bid_price: Some(quote.bid_price),
            ask_price: Some(quote.ask_price),
            bid_size: quote.bid_size,
            ask_size: quote.ask_size,
            spread_bps: Some(quote.spread_bps()),
            quote_feed: Some(quote.feed),
            bar_close: None,
            fallback_bps: None,
        }
    }

    // Handles from bar logic.
    fn from_bar(side: ExecutionSide, close: f64, fallback_bps: f64) -> Self {
        let adjustment = fallback_bps / 10_000.0;
        let price = match side {
            ExecutionSide::Buy => close * (1.0 + adjustment),
            ExecutionSide::Sell => (close * (1.0 - adjustment)).max(0.0001),
        };
        Self {
            price,
            source: "bar_fallback",
            bid_price: None,
            ask_price: None,
            bid_size: None,
            ask_size: None,
            spread_bps: None,
            quote_feed: None,
            bar_close: Some(close),
            fallback_bps: Some(fallback_bps),
        }
    }

    // Handles json fields logic.
    fn json_fields(&self) -> serde_json::Value {
        serde_json::json!({
            "price_source": self.source,
            "bid": self.bid_price,
            "ask": self.ask_price,
            "bid_size": self.bid_size,
            "ask_size": self.ask_size,
            "spread_bps": self.spread_bps,
            "quote_feed": self.quote_feed,
            "bar_close": self.bar_close,
            "bar_fallback_bps": self.fallback_bps,
        })
    }
}

// Formats order price for output.
fn format_order_price(price: f64) -> String {
    if price >= 1.0 {
        format!("{price:.2}")
    } else {
        format!("{price:.4}")
    }
}

// ── Compliance checks ────────────────────────────────────────────

fn is_blocked(sym: &str) -> bool {
    config::is_blocked_symbol(sym) || looks_like_option_symbol(sym)
}

// Returns whether like option symbol is true.
fn looks_like_option_symbol(symbol: &str) -> bool {
    let compact = symbol.replace([' ', '-'], "");
    let bytes = compact.as_bytes();
    if bytes.len() < 15 {
        return false;
    }

    for i in 1..bytes.len().saturating_sub(14) {
        let date = &bytes[i..i + 6];
        let cp = bytes[i + 6];
        let strike = &bytes[i + 7..];
        if date.iter().all(u8::is_ascii_digit)
            && matches!(cp, b'C' | b'P')
            && strike.len() >= 8
            && strike.iter().all(u8::is_ascii_digit)
        {
            return true;
        }
    }
    false
}

// Returns whether wash sale window is true.
fn has_wash_sale_window(conn: &Connection, account: &config::AlpacaAccount, symbol: &str) -> bool {
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let count: i64 = if account.is_paper() {
        conn.query_row(
            "SELECT COUNT(*) FROM wash_sale_tracker
             WHERE symbol=?1 AND status='active' AND wash_window_end >= ?2 AND paper_account=1",
            params![symbol, today],
            |r| r.get(0),
        )
        .unwrap_or(0)
    } else {
        conn.query_row(
            "SELECT COUNT(*) FROM wash_sale_tracker
             WHERE symbol=?1 AND status='active' AND wash_window_end >= ?2 AND paper_account=0",
            params![symbol, today],
            |r| r.get(0),
        )
        .unwrap_or(0)
    };
    count > 0
}

// Handles pdt day trades count logic.
fn pdt_day_trades_count(conn: &Connection, account: &config::AlpacaAccount) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM day_trades
         WHERE provider=?1 AND account_ref=?2 AND paper_account=?3
           AND trade_date >= date('now', '-5 days')",
        params![
            account.provider(),
            account.account_ref(),
            paper_flag(account)
        ],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

// Returns live quote from config, storage, or provider data.
async fn get_live_quote(
    client: &reqwest::Client,
    account: &config::AlpacaAccount,
    symbol: &str,
) -> anyhow::Result<LiveQuote> {
    let mut last_error = None;
    let mut qr = None;
    let mut used_feed = None;
    let feeds = alpaca::data_feeds_for(account);
    for (idx, feed) in feeds.iter().enumerate() {
        let url = alpaca::stock_quote_url(symbol, feed);
        match api_get::<QuoteResponse>(client, &url).await {
            Ok(response) => {
                qr = Some(response);
                used_feed = Some(feed.clone());
                break;
            }
            Err(err) => {
                if idx + 1 < feeds.len() {
                    auto_stderr_log(serde_json::json!({
                        "event": "stock_quote_feed_fallback",
                        "level": "warn",
                        "feed": feed,
                        "provider": account.provider(),
                        "account_ref": account.account_ref(),
                        "broker_account_id": "not available",
                        "symbol": symbol,
                        "error": err.to_string(),
                        "message": "stock quote feed failed; trying fallback feed",
                    }));
                }
                last_error = Some(err);
            }
        }
    }
    let qr = qr.ok_or_else(|| {
        last_error.unwrap_or_else(|| anyhow::anyhow!("No stock quote feed configured"))
    })?;
    let q = qr.quote.unwrap_or(QuoteData {
        bid_price: None,
        ask_price: None,
        bid_size: None,
        ask_size: None,
    });
    let feed = used_feed.unwrap_or_else(|| "unknown".to_string());
    LiveQuote::from_quote(symbol, &feed, q)
}

// Returns latest bar close from local storage.
fn latest_bar_close(conn: &Connection, symbol: &str) -> anyhow::Result<f64> {
    conn.query_row(
        "SELECT close FROM bars WHERE symbol=?1 ORDER BY date DESC LIMIT 1",
        params![symbol],
        |r| r.get::<_, f64>(0),
    )
    .map_err(|err| anyhow::anyhow!("No bar fallback close for {}: {}", symbol, err))
}

// Returns execution price from config, storage, or provider data.
async fn get_execution_price(
    conn: &Connection,
    client: &reqwest::Client,
    account: &config::AlpacaAccount,
    symbol: &str,
    side: ExecutionSide,
    cfg: &StrategyConfig,
) -> anyhow::Result<ExecutionPrice> {
    match get_live_quote(client, account, symbol).await {
        Ok(quote) => {
            if matches!(side, ExecutionSide::Buy) {
                if let Some(reason) = quote.entry_reject_reason(cfg) {
                    anyhow::bail!("quote rejected: {}", reason);
                }
            }
            Ok(ExecutionPrice::from_quote(side, quote))
        }
        Err(quote_err) => {
            if !cfg.allow_bar_price_fallback {
                return Err(quote_err);
            }
            let close = latest_bar_close(conn, symbol)?;
            auto_stderr_log(serde_json::json!({
                "event": "quote_bar_price_fallback",
                "level": "warn",
                "provider": account.provider(),
                "account_ref": account.account_ref(),
                "broker_account_id": "not available",
                "side": match side {
                    ExecutionSide::Buy => "buy",
                    ExecutionSide::Sell => "sell",
                },
                "symbol": symbol,
                "bar_fallback_bps": cfg.bar_fallback_bps,
                "adjustment": match side {
                    ExecutionSide::Buy => "upward",
                    ExecutionSide::Sell => "downward",
                },
                "error": quote_err.to_string(),
            }));
            Ok(ExecutionPrice::from_bar(side, close, cfg.bar_fallback_bps))
        }
    }
}

// ── Trading helpers ──────────────────────────────────────────────

fn add_business_days(from: &str, days: i64) -> String {
    let mut date =
        NaiveDate::parse_from_str(from, "%Y-%m-%d").unwrap_or_else(|_| Utc::now().date_naive());
    let mut added = 0;
    while added < days {
        date = date.succ_opt().unwrap_or(date);
        let wd = date.weekday();
        if wd != chrono::Weekday::Sat && wd != chrono::Weekday::Sun {
            added += 1;
        }
    }
    date.format("%Y-%m-%d").to_string()
}

// Records wash sale for audit and compliance.
fn record_wash_sale(
    conn: &Connection,
    account: &config::AlpacaAccount,
    broker_account_id: Option<&str>,
    symbol: &str,
    sell_price: f64,
    entry_price: f64,
    timestamp_utc: &str,
    safety_buffer_days: i64,
) -> anyhow::Result<()> {
    let loss = sell_price - entry_price;
    if loss < 0.0 {
        let timestamp = chrono::DateTime::parse_from_rfc3339(timestamp_utc)
            .map(|timestamp| timestamp.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        let today = timestamp.format("%Y-%m-%d").to_string();
        let sell_time = timestamp.format("%H:%M:%S").to_string();
        let timestamp_utc = timestamp.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let window_end = {
            let d = NaiveDate::parse_from_str(&today, "%Y-%m-%d")?;
            let end = d + chrono::Duration::days(compliance::wash_sale_forward_block_days(Some(
                safety_buffer_days,
            )));
            end.format("%Y-%m-%d").to_string()
        };
        conn.execute(
            "INSERT INTO wash_sale_tracker (
                symbol, sell_date, sell_time, sell_timestamp_utc, event_timezone,
                sell_price, loss_amount, wash_window_end, status, provider, account_ref,
                broker_account_id, paper_account
             )
             VALUES (?1, ?2, ?3, ?4, 'UTC', ?5, ?6, ?7, 'active', ?8, ?9, ?10, ?11)",
            params![
                symbol,
                today,
                sell_time,
                timestamp_utc,
                sell_price,
                loss.abs(),
                window_end,
                account.provider(),
                account.account_ref(),
                broker_account_id,
                paper_flag(account)
            ],
        )?;
    }
    Ok(())
}

// Records day trade for audit and compliance.
fn record_day_trade(
    conn: &Connection,
    account: &config::AlpacaAccount,
    broker_account_id: Option<&str>,
    symbol: &str,
    timestamp_utc: &str,
) -> anyhow::Result<()> {
    let timestamp = chrono::DateTime::parse_from_rfc3339(timestamp_utc)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    let today = timestamp.format("%Y-%m-%d").to_string();
    // Check if we bought this symbol today (would make it a day trade)
    let bought_today: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM auto_trades
             WHERE provider=?1 AND account_ref=?2 AND paper_account=?3
               AND symbol=?4 AND side='buy' AND timestamp LIKE ?5",
            params![
                account.provider(),
                account.account_ref(),
                paper_flag(account),
                symbol,
                format!("{}%", today)
            ],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if bought_today > 0 {
        let sell_time = timestamp.format("%H:%M:%S").to_string();
        let timestamp_utc = timestamp.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        conn.execute(
            "INSERT INTO day_trades (
                symbol, trade_date, buy_time, sell_time, sell_timestamp_utc, event_timezone,
                provider, account_ref, broker_account_id, paper_account
             )
             VALUES (?1, ?2, 'same_day', ?3, ?4, 'UTC', ?5, ?6, ?7, ?8)",
            params![
                symbol,
                today,
                sell_time,
                timestamp_utc,
                account.provider(),
                account.account_ref(),
                broker_account_id,
                paper_flag(account)
            ],
        )?;
    }
    Ok(())
}

// ── Candidate selection ──────────────────────────────────────────

struct BuyCandidate {
    symbol: String,
    ml_score: f64,
    ml_quintile: i64,
}

// Handles find buy candidates logic.
fn find_buy_candidates(
    conn: &Connection,
    account: &config::AlpacaAccount,
    cfg: &StrategyConfig,
    provider_open_symbols: &HashSet<String>,
) -> anyhow::Result<Vec<BuyCandidate>> {
    let pred_date: String = conn.query_row(
        "SELECT COALESCE(MAX(date),'none') FROM ml_predictions",
        [],
        |r| r.get(0),
    )?;
    if pred_date == "none" {
        return Ok(vec![]);
    }

    // Get all ML Q1 predictions — prefer ensemble score if available
    let mut stmt = conn.prepare(
        "SELECT symbol, COALESCE(ensemble_score, predicted_score) as score, predicted_quintile FROM ml_predictions
         WHERE date=?1 AND predicted_quintile <= ?2
         ORDER BY COALESCE(ensemble_score, predicted_score) DESC"
    )?;

    let ml_rows: Vec<(String, f64, i64)> = stmt
        .query_map(params![pred_date, cfg.ml_quintile_buy], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    // Get open auto positions to skip
    let open_syms: HashSet<String> = {
        let mut s = conn.prepare(
            "SELECT symbol FROM auto_positions
             WHERE provider=?1 AND account_ref=?2 AND paper_account=?3 AND status='open'",
        )?;
        let results: Vec<String> = s
            .query_map(
                params![
                    account.provider(),
                    account.account_ref(),
                    paper_flag(account)
                ],
                |r| r.get::<_, String>(0),
            )?
            .filter_map(|r| r.ok())
            .collect();
        results.into_iter().collect()
    };

    let mut candidates = Vec::new();

    for (symbol, ml_score, ml_quintile) in &ml_rows {
        // Skip blocked
        if is_blocked(symbol) {
            continue;
        }
        // Skip already in position
        let normalized_symbol = symbol.trim().to_ascii_uppercase();
        if open_syms.contains(symbol) || provider_open_symbols.contains(&normalized_symbol) {
            continue;
        }
        // Skip wash sale window
        if has_wash_sale_window(conn, account, symbol) {
            continue;
        }

        // Get latest close from bars
        let latest_close: f64 = match conn.query_row(
            "SELECT close FROM bars WHERE symbol=?1 ORDER BY date DESC LIMIT 1",
            params![symbol],
            |r| r.get(0),
        ) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Price filter
        if latest_close < cfg.min_price {
            continue;
        }

        // Volume filter — 20d average
        let avg_vol_f: f64 = conn.query_row(
            "SELECT COALESCE(AVG(volume), 0) FROM (SELECT volume FROM bars WHERE symbol=?1 ORDER BY date DESC LIMIT 20)",
            params![symbol], |r| r.get(0)
        ).unwrap_or(0.0);
        if (avg_vol_f as i64) < cfg.min_avg_volume {
            continue;
        }

        candidates.push(BuyCandidate {
            symbol: symbol.clone(),
            ml_score: *ml_score,
            ml_quintile: *ml_quintile,
        });

        // Cap candidates to evaluate
        if candidates.len() >= 50 {
            break;
        }
    }

    Ok(candidates)
}

// ══════════════════════════════════════════════════════════════════
// CMD: auto run — execute one trading cycle
// ══════════════════════════════════════════════════════════════════

async fn run_auto_account(
    conn: &Connection,
    account: &config::AlpacaAccount,
    cfg: &StrategyConfig,
    schedule: &MarketSchedule,
    today: &str,
    now_ts: &str,
    source: &str,
) -> anyhow::Result<serde_json::Value> {
    let client = build_client(account);
    let mut provider_session = ProviderSession::default();

    if schedule.require_local_clock {
        if let Some(reason) = local_market_session_block(schedule) {
            return Ok(serde_json::json!({
                "provider": account.provider(),
                "account_ref": account.account_ref(),
                "account_mode": alpaca::account_mode_for(account),
                "tax_universe": if account.is_paper() { "paper" } else { "real" },
                "status": "market_closed",
                "clock_source": "local",
                "message": reason,
            }));
        }
    }

    let mut clock_source = "local";
    if schedule.use_provider_calendar {
        match provider_calendar_gate(&client, account, schedule).await {
            Ok(Err(reason)) => {
                return Ok(serde_json::json!({
                    "provider": account.provider(),
                    "account_ref": account.account_ref(),
                    "account_mode": alpaca::account_mode_for(account),
                    "tax_universe": if account.is_paper() { "paper" } else { "real" },
                    "status": "market_closed",
                    "clock_source": "provider_calendar",
                    "message": reason,
                }));
            }
            Ok(Ok(session)) => {
                provider_session = session;
                clock_source = "provider_calendar+local";
            }
            Err(err) => {
                if !schedule.allow_local_clock_fallback {
                    anyhow::bail!(
                        "provider calendar failed and auto.market.allow_local_clock_fallback=false: {}",
                        err
                    );
                }
                auto_stderr_log(serde_json::json!({
                    "event": "provider_calendar_fallback",
                    "level": "warn",
                    "provider": account.provider(),
                    "account_ref": account.account_ref(),
                    "broker_account_id": "not available",
                    "error": err.to_string(),
                    "message": "provider calendar failed; using configured local exchange schedule",
                }));
            }
        }
    }
    if schedule.use_provider_clock {
        match provider_clock_block(&client, account, schedule).await {
            Ok(Some(reason)) => {
                return Ok(serde_json::json!({
                    "provider": account.provider(),
                    "account_ref": account.account_ref(),
                    "account_mode": alpaca::account_mode_for(account),
                    "tax_universe": if account.is_paper() { "paper" } else { "real" },
                    "status": "market_closed",
                    "clock_source": "provider_clock",
                    "message": reason,
                }));
            }
            Ok(None) => {
                clock_source = if schedule.use_provider_calendar {
                    "provider_calendar+clock+local"
                } else {
                    "provider_clock+local"
                };
            }
            Err(err) => {
                if !schedule.allow_local_clock_fallback {
                    anyhow::bail!(
                        "provider market clock failed and auto.market.allow_local_clock_fallback=false: {}",
                        err
                    );
                }
                auto_stderr_log(serde_json::json!({
                    "event": "provider_market_clock_fallback",
                    "level": "warn",
                    "provider": account.provider(),
                    "account_ref": account.account_ref(),
                    "broker_account_id": "not available",
                    "error": err.to_string(),
                    "message": "provider market clock failed; using configured local exchange schedule",
                }));
            }
        }
    }

    let acct: AccountInfo =
        api_get(&client, &alpaca::broker_api_url_for(account, "/account")).await?;
    let broker_id = broker_account_id(&acct);
    let provider_account_snapshot =
        sync_provider_account_snapshot(conn, account, broker_id.as_deref(), &acct, source)?;
    let provider_positions: Vec<alpaca::Position> =
        api_get(&client, &alpaca::broker_api_url_for(account, "/positions")).await?;
    let provider_open_symbols = provider_position_symbols(&provider_positions);
    let equity = acct
        .equity
        .as_deref()
        .unwrap_or("0")
        .parse::<f64>()
        .unwrap_or(0.0);
    let cash = acct
        .cash
        .as_deref()
        .unwrap_or("0")
        .parse::<f64>()
        .unwrap_or(0.0);

    let mut buys = Vec::new();
    let mut sells = Vec::new();
    let mut skipped_reasons: Vec<String> = Vec::new();

    let mut open_positions: Vec<OpenAutoPosition> = {
        let mut stmt = conn.prepare(
            "SELECT id, symbol, entry_date, entry_timestamp, entry_price, shares, cost_basis,
                    stop_loss_price, take_profit_price, exit_by_date, ml_quintile,
                    COALESCE(stop_loss_breach_count, 0), stop_loss_first_breach_at,
                    COALESCE(take_profit_breach_count, 0), take_profit_first_breach_at,
                    take_profit_peak_pct, take_profit_peak_price,
                    COALESCE(entry_execution_origin, 'mlai_auto')
             FROM auto_positions
             WHERE provider=?1 AND account_ref=?2 AND paper_account=?3 AND status='open'",
        )?;
        let rows: Vec<_> = stmt
            .query_map(
                params![
                    account.provider(),
                    account.account_ref(),
                    paper_flag(account)
                ],
                |r| {
                    Ok(OpenAutoPosition {
                        id: r.get(0)?,
                        symbol: r.get(1)?,
                        entry_date: r.get(2)?,
                        entry_timestamp: r.get(3)?,
                        entry_price: r.get(4)?,
                        shares: r.get(5)?,
                        cost_basis: r.get(6)?,
                        stop_loss: r.get(7)?,
                        take_profit: r.get(8)?,
                        exit_by: r.get(9)?,
                        entry_execution_origin: origin::ExecutionOrigin::parse(
                            &r.get::<_, String>(17)
                                .unwrap_or_else(|_| "mlai_auto".to_string()),
                        ),
                        confirmation: ExitConfirmationState {
                            stop_loss_breach_count: r.get(11)?,
                            stop_loss_first_breach_at: r.get(12)?,
                            take_profit_breach_count: r.get(13)?,
                            take_profit_first_breach_at: r.get(14)?,
                            take_profit_peak_pct: r.get(15)?,
                            take_profit_peak_price: r.get(16)?,
                        },
                    })
                },
            )?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };
    let provider_position_reconciliation = reconcile_open_positions_with_provider(
        conn,
        account,
        broker_id.as_deref(),
        &mut open_positions,
        &provider_positions,
        now_ts,
        source,
    )?;
    let local_auto_exposure: f64 = open_positions
        .iter()
        .map(|position| position.cost_basis)
        .sum();
    let cash_guard = cash_only_guard(
        conn,
        account,
        cash,
        equity,
        &provider_positions,
        local_auto_exposure,
    )?;
    let mut remaining_cash = cash_guard.deployable_cash;

    let now_dt = parse_utc_ts(now_ts).unwrap_or_else(Utc::now);

    for position in &open_positions {
        let symbol = &position.symbol;
        let exec_price =
            match get_execution_price(conn, &client, account, symbol, ExecutionSide::Sell, cfg)
                .await
            {
                Ok(price) => price,
                Err(_) => {
                    skipped_reasons.push(format!("{}: no quote or bar fallback available", symbol));
                    continue;
                }
            };
        let current_price = exec_price.price;
        let pnl = (current_price - position.entry_price) * position.shares as f64;
        let pnl_pct = (current_price / position.entry_price - 1.0) * 100.0;
        let entry_time =
            position_entry_time(position.entry_timestamp.as_deref(), &position.entry_date);
        let exit_decision = evaluate_confirmed_exit(
            cfg,
            position.confirmation.clone(),
            now_dt,
            entry_time,
            current_price,
            position.entry_price,
            position.stop_loss,
            position.take_profit,
        );
        let confirmation_rule = exit_decision.rule.clone();
        let confirmation_cycles_remaining = exit_decision.cycles_remaining;
        let confirmation_minutes_remaining = exit_decision.minutes_remaining;
        let mut exit_reason = exit_decision.reason.clone();
        update_exit_confirmation_state(conn, account, position.id, &exit_decision.state)?;
        if let Some(note) = exit_decision.note.clone() {
            append_auto_log(serde_json::json!({
                "event": "auto_exit_confirmation_wait",
                "level": "info",
                "source": source,
                "provider": account.provider(),
                "account_ref": account.account_ref(),
                "broker_account_id": broker_id.as_deref().unwrap_or("not available"),
                "account_mode": alpaca::account_mode_for(account),
                "tax_universe": if account.is_paper() { "paper" } else { "real" },
                "symbol": symbol,
                "rule": confirmation_rule.as_deref().unwrap_or("exit_confirmation"),
                "current_price": current_price,
                "entry_price": position.entry_price,
                "pnl_pct": pnl_pct,
                "stop_loss_price": position.stop_loss,
                "take_profit_price": position.take_profit,
                "cycles_seen": if confirmation_rule.as_deref().unwrap_or("").starts_with("stop_loss") {
                    exit_decision.state.stop_loss_breach_count
                } else {
                    exit_decision.state.take_profit_breach_count
                },
                "cycles_remaining": confirmation_cycles_remaining
                    .map(serde_json::Value::from)
                    .unwrap_or_else(|| serde_json::json!("not available")),
                "minutes_remaining": confirmation_minutes_remaining
                    .map(serde_json::Value::from)
                    .unwrap_or_else(|| serde_json::json!("not available")),
                "message": note.clone(),
            }));
            skipped_reasons.push(format!("{}: {}", symbol, note));
        }

        if exit_reason.is_none() {
            if let Some(exit_date) = &position.exit_by {
                if today >= exit_date.as_str() {
                    exit_reason = Some(format!(
                        "TIME_STOP ({}d, {:+.2}%)",
                        cfg.max_hold_days, pnl_pct
                    ));
                }
            }
        }
        if exit_reason.is_none() {
            let current_q: Option<i64> = conn
                .query_row(
                    "SELECT predicted_quintile FROM ml_predictions
                     WHERE symbol=?1 ORDER BY date DESC LIMIT 1",
                    params![symbol],
                    |r| r.get(0),
                )
                .ok();
            if let Some(q) = current_q {
                if q >= cfg.ml_quintile_exit {
                    exit_reason = Some(format!("ML_DEGRADED (Q{}, {:+.2}%)", q, pnl_pct));
                }
            }
        }

        if let Some(reason) = exit_reason {
            if let Some(block_reason) = local_market_block(schedule, TradePhase::Sell) {
                skipped_reasons.push(format!("{}: sell window closed: {}", symbol, block_reason));
                continue;
            }
            let pdt_count = pdt_day_trades_count(conn, account);
            if pdt_count >= 3 {
                let bought_today: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM auto_trades
                         WHERE provider=?1 AND account_ref=?2 AND paper_account=?3
                           AND symbol=?4 AND side='buy' AND timestamp LIKE ?5",
                        params![
                            account.provider(),
                            account.account_ref(),
                            paper_flag(account),
                            symbol,
                            format!("{}%", today)
                        ],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                if bought_today > 0 {
                    skipped_reasons.push(format!(
                        "{}: PDT limit reached (3/3), can't day-trade",
                        symbol
                    ));
                    continue;
                }
            }

            append_auto_log(serde_json::json!({
                "event": "auto_exit_rule_triggered",
                "level": "info",
                "execution_origin": origin::ExecutionOrigin::MlaiAuto.as_str(),
                "source": source,
                "provider": account.provider(),
                "account_ref": account.account_ref(),
                "broker_account_id": broker_id.as_deref().unwrap_or("not available"),
                "account_mode": alpaca::account_mode_for(account),
                "tax_universe": if account.is_paper() { "paper" } else { "real" },
                "symbol": symbol,
                "rule": confirmation_rule.as_deref().unwrap_or_else(|| {
                    if reason.starts_with("TIME_STOP") {
                        "time_stop"
                    } else if reason.starts_with("ML_DEGRADED") {
                        "ml_degraded"
                    } else {
                        "exit_rule"
                    }
                }),
                "reason": reason.as_str(),
                "current_price": current_price,
                "entry_price": position.entry_price,
                "pnl_pct": pnl_pct,
                "action": "submit_sell_order",
            }));

            let order = OrderRequest {
                symbol: symbol.clone(),
                qty: format!("{}", position.shares),
                side: "sell".into(),
                r#type: "limit".into(),
                time_in_force: "day".into(),
                client_order_id: client_order_id(account, "sell", symbol),
                limit_price: Some(format_order_price(current_price)),
            };

            match api_post::<OrderResponse>(
                &client,
                &alpaca::broker_api_url_for(account, "/orders"),
                &order,
            )
            .await
            {
                Ok(resp) => {
                    let order_id = resp.id.unwrap_or_default();
                    conn.execute(
                        "UPDATE auto_positions
                         SET status='closed', exit_date=?1, exit_price=?2, exit_reason=?3,
                             pnl=?4, pnl_pct=?5, exit_order_id=?6,
                             exit_execution_origin='mlai_auto', execution_origin='mlai_auto'
                         WHERE id=?7 AND provider=?8 AND account_ref=?9 AND paper_account=?10",
                        params![
                            today,
                            current_price,
                            reason.as_str(),
                            pnl,
                            pnl_pct,
                            order_id,
                            position.id,
                            account.provider(),
                            account.account_ref(),
                            paper_flag(account)
                        ],
                    )?;
                    conn.execute(
                        "INSERT INTO auto_trades (
                            provider, account_ref, broker_account_id, account_mode, paper_account,
                            market_timezone, market_session_source, provider_market, provider_core_start,
                            provider_core_end, timestamp, symbol, side, shares, price, order_id, reason,
                            auto_position_id
                         )
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'sell', ?13, ?14, ?15, ?16, ?17)",
                        params![
                            account.provider(),
                            account.account_ref(),
                            broker_id.as_deref(),
                            alpaca::account_mode_for(account),
                            paper_flag(account),
                            schedule.timezone_name,
                            clock_source,
                            provider_session.market(),
                            provider_session.core_start(),
                            provider_session.core_end(),
                            now_ts,
                            symbol,
                            position.shares,
                            current_price,
                            order_id,
                            reason.as_str(),
                            position.id
                        ],
                    )?;
                    record_wash_sale(
                        conn,
                        account,
                        broker_id.as_deref(),
                        symbol,
                        current_price,
                        position.entry_price,
                        now_ts,
                        cfg.wash_sale_safety_buffer_days,
                    )?;
                    record_day_trade(conn, account, broker_id.as_deref(), symbol, now_ts)?;
                    append_auto_log(serde_json::json!({
                        "event": "auto_exit_order_submitted",
                        "level": "info",
                        "execution_origin": origin::ExecutionOrigin::MlaiAuto.as_str(),
                        "source": source,
                        "provider": account.provider(),
                        "account_ref": account.account_ref(),
                        "broker_account_id": broker_id.as_deref().unwrap_or("not available"),
                        "account_mode": alpaca::account_mode_for(account),
                        "tax_universe": if account.is_paper() { "paper" } else { "real" },
                        "symbol": symbol,
                        "rule": confirmation_rule.as_deref().unwrap_or_else(|| {
                            if reason.starts_with("TIME_STOP") {
                                "time_stop"
                            } else if reason.starts_with("ML_DEGRADED") {
                                "ml_degraded"
                            } else {
                                "exit_rule"
                            }
                        }),
                        "reason": reason.as_str(),
                        "order_id": order_id.as_str(),
                        "shares": position.shares,
                        "limit_price": current_price,
                        "pnl": pnl,
                        "pnl_pct": pnl_pct,
                    }));
                    let provider_sync = match sync_provider_history_with_context(
                        conn,
                        account,
                        &client,
                        broker_id.as_deref(),
                    )
                    .await
                    {
                        Ok(sync) => sync,
                        Err(err) => serde_json::json!({
                            "status": "warning",
                            "error": err.to_string(),
                        }),
                    };
                    sells.push(serde_json::json!({
                        "symbol": symbol,
                        "execution_origin": origin::ExecutionOrigin::MlaiAuto.as_str(),
                        "shares": position.shares,
                        "price": current_price,
                        "execution": exec_price.json_fields(),
                        "pnl": (pnl * 100.0).round() / 100.0,
                        "pnl_pct": (pnl_pct * 100.0).round() / 100.0,
                        "reason": reason,
                        "order_id": order_id,
                        "provider_sync": provider_sync,
                    }));
                }
                Err(e) => {
                    append_auto_log(serde_json::json!({
                        "event": "auto_exit_order_failed",
                        "level": "error",
                        "execution_origin": origin::ExecutionOrigin::MlaiAuto.as_str(),
                        "source": source,
                        "provider": account.provider(),
                        "account_ref": account.account_ref(),
                        "broker_account_id": broker_id.as_deref().unwrap_or("not available"),
                        "account_mode": alpaca::account_mode_for(account),
                        "tax_universe": if account.is_paper() { "paper" } else { "real" },
                        "symbol": symbol,
                        "rule": confirmation_rule.as_deref().unwrap_or("exit_rule"),
                        "reason": reason.as_str(),
                        "error": e.to_string(),
                    }));
                    skipped_reasons.push(format!("{}: sell failed - {}", symbol, e));
                }
            }
        }
    }

    let local_open_symbols: HashSet<String> = open_positions
        .iter()
        .map(|position| position.symbol.trim().to_ascii_uppercase())
        .filter(|symbol| !symbol.is_empty())
        .collect();
    let total_open_symbols = local_open_symbols
        .union(&provider_open_symbols)
        .count()
        .try_into()
        .unwrap_or(i64::MAX);
    let slots = cfg.max_positions.saturating_sub(total_open_symbols);
    if slots > 0 {
        if let Some(reason) = local_market_block(schedule, TradePhase::Buy) {
            skipped_reasons.push(format!("buy window closed: {}", reason));
        } else if remaining_cash <= 0.0 {
            skipped_reasons.push(format!(
                "buying skipped: account cash ${:.2}; cash-only trading enforced",
                remaining_cash
            ));
        } else {
            let position_budget = equity * cfg.position_size_pct / 100.0;
            let candidates = find_buy_candidates(conn, account, cfg, &provider_open_symbols)?;
            let mut filled = 0i64;
            for cand in &candidates {
                if filled >= slots {
                    break;
                }
                let exec_price = match get_execution_price(
                    conn,
                    &client,
                    account,
                    &cand.symbol,
                    ExecutionSide::Buy,
                    cfg,
                )
                .await
                {
                    Ok(price) => price,
                    Err(_) => {
                        skipped_reasons
                            .push(format!("{}: no live quote or bar fallback", cand.symbol));
                        continue;
                    }
                };
                let price = exec_price.price;
                if price < cfg.min_price {
                    continue;
                }
                let shares = (position_budget / price).floor() as i64;
                if shares <= 0 {
                    continue;
                }
                let order_value = shares as f64 * price;
                if remaining_cash <= 0.0 {
                    skipped_reasons.push(format!(
                        "{}: REJECTED - account in margin (cash ${:.2}). Cash-only trading enforced.",
                        cand.symbol, remaining_cash
                    ));
                    continue;
                }
                if remaining_cash - order_value < 0.0 {
                    skipped_reasons.push(format!(
                        "{}: REJECTED - would require margin (${:.0} order, ${:.0} cash available). Cash-only trading enforced.",
                        cand.symbol, order_value, remaining_cash
                    ));
                    continue;
                }
                if order_value > remaining_cash * 0.95 {
                    skipped_reasons.push(format!(
                        "{}: insufficient cash ({:.0} needed, {:.0} available)",
                        cand.symbol, order_value, remaining_cash
                    ));
                    break;
                }

                let order = OrderRequest {
                    symbol: cand.symbol.clone(),
                    qty: format!("{}", shares),
                    side: "buy".into(),
                    r#type: "limit".into(),
                    time_in_force: "day".into(),
                    client_order_id: client_order_id(account, "buy", &cand.symbol),
                    limit_price: Some(format_order_price(price)),
                };

                match api_post::<OrderResponse>(
                    &client,
                    &alpaca::broker_api_url_for(account, "/orders"),
                    &order,
                )
                .await
                {
                    Ok(resp) => {
                        let order_id = resp.id.unwrap_or_default();
                        let stop_loss_price = price * (1.0 - cfg.stop_loss_pct / 100.0);
                        let take_profit_price = price * (1.0 + cfg.take_profit_pct / 100.0);
                        let exit_by = add_business_days(today, cfg.max_hold_days);
                        conn.execute(
                        "INSERT INTO auto_positions (
                            provider, account_ref, broker_account_id, account_mode, paper_account,
                            market_timezone, market_session_source, provider_market, provider_core_start,
                            provider_core_end, symbol, entry_date, entry_timestamp, entry_price, shares, cost_basis,
                            stop_loss_price, take_profit_price, exit_by_date, ml_quintile, ml_score,
                            status, order_id
                         )
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, 'open', ?22)",
                        params![
                            account.provider(),
                            account.account_ref(),
                            broker_id.as_deref(),
                            alpaca::account_mode_for(account),
                            paper_flag(account),
                            schedule.timezone_name,
                            clock_source,
                            provider_session.market(),
                            provider_session.core_start(),
                            provider_session.core_end(),
                            cand.symbol,
                            today,
                            now_ts,
                                price,
                                shares,
                                order_value,
                                stop_loss_price,
                                take_profit_price,
                                exit_by,
                                cand.ml_quintile,
                                cand.ml_score,
                                order_id
                            ],
                        )?;
                        conn.execute(
                        "INSERT INTO auto_trades (
                            provider, account_ref, broker_account_id, account_mode, paper_account,
                            market_timezone, market_session_source, provider_market, provider_core_start,
                            provider_core_end, timestamp, symbol, side, shares, price, order_id, reason
                         )
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'buy', ?13, ?14, ?15, ?16)",
                        params![
                            account.provider(),
                            account.account_ref(),
                            broker_id.as_deref(),
                            alpaca::account_mode_for(account),
                            paper_flag(account),
                            schedule.timezone_name,
                            clock_source,
                            provider_session.market(),
                            provider_session.core_start(),
                            provider_session.core_end(),
                            now_ts,
                            cand.symbol,
                                shares,
                                price,
                                order_id,
                                format!("ML_Q{} score={:.4}", cand.ml_quintile, cand.ml_score)
                            ],
                        )?;
                        let provider_sync = match sync_provider_history_with_context(
                            conn,
                            account,
                            &client,
                            broker_id.as_deref(),
                        )
                        .await
                        {
                            Ok(sync) => sync,
                            Err(err) => serde_json::json!({
                                "status": "warning",
                                "error": err.to_string(),
                            }),
                        };
                        remaining_cash -= order_value;
                        buys.push(serde_json::json!({
                            "symbol": cand.symbol,
                            "execution_origin": origin::ExecutionOrigin::MlaiAuto.as_str(),
                            "shares": shares,
                            "price": price,
                            "execution": exec_price.json_fields(),
                            "cost": order_value,
                            "cash_remaining_after": (remaining_cash * 100.0).round() / 100.0,
                            "stop_loss": (stop_loss_price * 100.0).round() / 100.0,
                            "take_profit": (take_profit_price * 100.0).round() / 100.0,
                            "exit_by": exit_by,
                            "ml_quintile": cand.ml_quintile,
                            "ml_score": (cand.ml_score * 10000.0).round() / 10000.0,
                            "order_id": order_id,
                            "provider_sync": provider_sync,
                        }));
                        filled += 1;
                    }
                    Err(e) => skipped_reasons.push(format!("{}: buy failed - {}", cand.symbol, e)),
                }
            }
        }
    }

    let final_open: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM auto_positions
             WHERE provider=?1 AND account_ref=?2 AND paper_account=?3 AND status='open'",
            params![
                account.provider(),
                account.account_ref(),
                paper_flag(account)
            ],
            |r| r.get(0),
        )
        .unwrap_or(0);

    Ok(serde_json::json!({
        "provider": account.provider(),
        "account_ref": account.account_ref(),
        "broker_account_id": broker_id.as_deref().unwrap_or("not available"),
        "account_mode": alpaca::account_mode_for(account),
        "tax_universe": if account.is_paper() { "paper" } else { "real" },
        "clock_source": clock_source,
        "market_timezone": schedule.timezone_name,
        "provider_market": provider_session.market(),
        "provider_core_start": provider_session.core_start(),
        "provider_core_end": provider_session.core_end(),
        "provider_account_snapshot": provider_account_snapshot,
        "provider_position_count": provider_open_symbols.len(),
        "provider_position_reconciliation": provider_position_reconciliation,
        "cash_only_guard": cash_guard.to_json(),
        "status": "ok",
        "timestamp": now_ts,
        "buys": buys,
        "sells": sells,
        "open_positions": final_open,
        "max_positions": cfg.max_positions,
        "skipped": skipped_reasons,
    }))
}

// Handles the auto run CLI action.
pub async fn cmd_auto_run(json: bool) -> anyhow::Result<()> {
    let source = invocation_source("cli");
    cmd_auto_run_with_source(json, &source).await
}

// Handles the auto run with source CLI action.
pub async fn cmd_auto_run_with_source(json: bool, source: &str) -> anyhow::Result<()> {
    let payload = run_auto_cycle(source, !json).await?;
    if json {
        println!("{}", serde_json::to_string(&payload)?);
    } else {
        print_auto_cycle_human(&payload);
    }
    Ok(())
}

// Handles run auto cycle logic.
pub async fn run_auto_cycle(
    source: &str,
    show_progress: bool,
) -> anyhow::Result<serde_json::Value> {
    let conn = open_db()?;
    init_auto_tables(&conn)?;
    let now_ts = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    if !is_enabled(&conn) {
        append_auto_log(serde_json::json!({
            "event": "auto_trade_cycle",
            "execution_origin": origin::ExecutionOrigin::MlaiAuto.as_str(),
            "source": source,
            "status": "disabled",
            "message": "Auto-trading is disabled.",
        }));
        return Ok(serde_json::json!({
            "status": "disabled",
            "timestamp": now_ts,
            "source": source,
            "execution_origin": origin::ExecutionOrigin::MlaiAuto.as_str(),
            "message": "Auto-trading is disabled. Run 'mlai-trade auto enable' to start.",
            "accounts": [],
        }));
    }

    let accounts = if config::provider_enabled("alpaca") {
        config::alpaca_accounts()?
    } else {
        Vec::new()
    };
    if accounts.is_empty() {
        append_auto_log(serde_json::json!({
            "event": "auto_trade_cycle",
            "execution_origin": origin::ExecutionOrigin::MlaiAuto.as_str(),
            "source": source,
            "status": "error",
            "stage": "account_discovery",
            "error": "No enabled auto-trade accounts found.",
        }));
        anyhow::bail!(
            "No enabled auto-trade accounts found. Alpaca is the implemented provider module today; enable providers.alpaca and alpaca.accounts[] in {}.",
            config::config_path().display()
        );
    }

    let cfg = load_config(&conn);
    let schedule = load_market_schedule()?;
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let mut results = Vec::new();
    let progress = crate::progress::bar_if(
        show_progress,
        accounts.len() as u64,
        "Auto-trade account cycle",
    );

    for account in &accounts {
        progress.set_message(format!("{}:{}", account.provider(), account.account_ref()));
        let provider_sync = match sync_provider_history_for_account(&conn, account).await {
            Ok(sync) => sync,
            Err(err) => {
                results.push(serde_json::json!({
                    "provider": account.provider(),
                    "account_ref": account.account_ref(),
                    "account_mode": alpaca::account_mode_for(account),
                    "tax_universe": if account.is_paper() { "paper" } else { "real" },
                    "auto_trade_enabled": account.auto_trade_enabled,
                    "status": "error",
                    "stage": "provider_sync",
                    "error": err.to_string(),
                }));
                progress.inc(1);
                continue;
            }
        };
        if !account.auto_trade_enabled {
            results.push(serde_json::json!({
                    "provider": account.provider(),
                    "account_ref": account.account_ref(),
                    "broker_account_id": provider_sync["broker_account_id"].clone(),
                    "account_mode": alpaca::account_mode_for(account),
                    "tax_universe": if account.is_paper() { "paper" } else { "real" },
                    "auto_trade_enabled": false,
                "status": "auto_trade_disabled",
                "message": "Auto trading is disabled for this account; provider order/fill sync still ran.",
                "provider_sync": provider_sync,
            }));
            progress.inc(1);
            continue;
        }
        match run_auto_account(&conn, account, &cfg, &schedule, &today, &now_ts, source).await {
            Ok(mut result) => {
                if let Some(object) = result.as_object_mut() {
                    object.insert("auto_trade_enabled".to_string(), serde_json::json!(true));
                    object.insert(
                        "broker_account_id".to_string(),
                        provider_sync["broker_account_id"].clone(),
                    );
                    object.insert("provider_sync".to_string(), provider_sync);
                }
                results.push(result);
            }
            Err(err) => results.push(serde_json::json!({
                "provider": account.provider(),
                "account_ref": account.account_ref(),
                "broker_account_id": provider_sync["broker_account_id"].clone(),
                "account_mode": alpaca::account_mode_for(account),
                "tax_universe": if account.is_paper() { "paper" } else { "real" },
                "auto_trade_enabled": true,
                "status": "error",
                "error": err.to_string(),
            })),
        }
        progress.inc(1);
    }
    progress.finish_and_clear();

    let cycle_status = if results
        .iter()
        .any(|result| result["status"].as_str() == Some("error"))
    {
        "partial_error"
    } else if results
        .iter()
        .all(|result| result["status"].as_str() == Some("market_closed"))
    {
        "market_closed"
    } else if results
        .iter()
        .all(|result| result["status"].as_str() == Some("auto_trade_disabled"))
    {
        "auto_trade_disabled"
    } else {
        "ok"
    };
    append_auto_log(serde_json::json!({
        "event": "auto_trade_cycle",
        "execution_origin": origin::ExecutionOrigin::MlaiAuto.as_str(),
        "source": source,
        "status": cycle_status,
        "timestamp": now_ts,
        "account_count": accounts.len(),
        "accounts": results.clone(),
    }));

    Ok(serde_json::json!({
        "status": cycle_status,
        "timestamp": now_ts,
        "source": source,
        "execution_origin": origin::ExecutionOrigin::MlaiAuto.as_str(),
        "accounts": results,
    }))
}

// Prints auto cycle human in human-readable form.
fn print_auto_cycle_human(payload: &serde_json::Value) {
    if payload["status"].as_str() == Some("disabled") {
        println!(
            "{}",
            payload["message"]
                .as_str()
                .unwrap_or("Auto-trading is disabled.")
        );
        return;
    }
    println!(
        "Auto-Trade Cycle - {}",
        payload["timestamp"].as_str().unwrap_or("?")
    );
    println!("{}", "=".repeat(50));
    if let Some(results) = payload["accounts"].as_array() {
        for result in results {
            let label = format!(
                "{}:{}",
                result["provider"].as_str().unwrap_or("?"),
                result["account_ref"].as_str().unwrap_or("?")
            );
            println!(
                "\n{} [{} / {}]",
                label,
                result["account_mode"].as_str().unwrap_or("?"),
                result["tax_universe"].as_str().unwrap_or("?")
            );
            println!(
                "  Broker account ID: {}",
                result["broker_account_id"]
                    .as_str()
                    .unwrap_or("not available")
            );
            match result["status"].as_str().unwrap_or("?") {
                "ok" => {
                    let buys = result["buys"].as_array().cloned().unwrap_or_default();
                    let sells = result["sells"].as_array().cloned().unwrap_or_default();
                    if buys.is_empty() && sells.is_empty() {
                        println!("  No trades this cycle.");
                    }
                    for s in &sells {
                        println!(
                            "  SELL {} {} @ ${:.2} [{}] P&L {:+.2} ({:+.2}%) - {}",
                            s["symbol"].as_str().unwrap_or("?"),
                            s["shares"],
                            s["price"].as_f64().unwrap_or(0.0),
                            s["execution"]["price_source"].as_str().unwrap_or("?"),
                            s["pnl"].as_f64().unwrap_or(0.0),
                            s["pnl_pct"].as_f64().unwrap_or(0.0),
                            s["reason"].as_str().unwrap_or("")
                        );
                    }
                    for b in &buys {
                        println!(
                            "  BUY  {} {} @ ${:.2} [{}] cost ${:.0} | SL ${:.2} | TP ${:.2} | ML Q{} {:.4}",
                            b["symbol"].as_str().unwrap_or("?"),
                            b["shares"],
                            b["price"].as_f64().unwrap_or(0.0),
                            b["execution"]["price_source"].as_str().unwrap_or("?"),
                            b["cost"].as_f64().unwrap_or(0.0),
                            b["stop_loss"].as_f64().unwrap_or(0.0),
                            b["take_profit"].as_f64().unwrap_or(0.0),
                            b["ml_quintile"].as_i64().unwrap_or(0),
                            b["ml_score"].as_f64().unwrap_or(0.0)
                        );
                    }
                    if let Some(skipped) = result["skipped"].as_array() {
                        if !skipped.is_empty() {
                            println!("  Skipped:");
                            for reason in skipped {
                                println!("    - {}", reason.as_str().unwrap_or("?"));
                            }
                        }
                    }
                    println!(
                        "  Positions: {}/{}",
                        result["open_positions"].as_i64().unwrap_or(0),
                        result["max_positions"].as_i64().unwrap_or(0)
                    );
                }
                "market_closed" => println!(
                    "  {}",
                    result["message"].as_str().unwrap_or("Market is closed.")
                ),
                "auto_trade_disabled" => println!(
                    "  {}",
                    result["message"]
                        .as_str()
                        .unwrap_or("Auto trading is disabled for this account.")
                ),
                "error" => println!("  Error: {}", result["error"].as_str().unwrap_or("?")),
                status => println!("  Status: {}", status),
            }
        }
    }
}

// ══════════════════════════════════════════════════════════════════
// CMD: auto status
// ══════════════════════════════════════════════════════════════════

pub async fn cmd_auto_status(json: bool) -> anyhow::Result<()> {
    let conn = open_db()?;
    init_auto_tables(&conn)?;
    let cfg = load_config(&conn);
    let enabled = is_enabled(&conn);
    let accounts = if config::provider_enabled("alpaca") {
        config::alpaca_accounts()?
    } else {
        Vec::new()
    };

    // Renders provider-external origins with the concrete provider name.
    fn status_origin_label(provider: &str, value: &str) -> String {
        let parsed = origin::ExecutionOrigin::parse(value);
        if parsed == origin::ExecutionOrigin::ProviderExternal {
            provider.to_string()
        } else {
            parsed.short_label().to_string()
        }
    }

    #[derive(Debug)]
    struct OpenPos {
        symbol: String,
        entry_date: String,
        entry_timestamp: Option<String>,
        entry_price: f64,
        shares: i64,
        cost_basis: f64,
        stop_loss: f64,
        take_profit: f64,
        exit_by: String,
        ml_q: i64,
        ml_score: f64,
        execution_origin: origin::ExecutionOrigin,
    }

    #[derive(Debug)]
    struct ProviderOpenPos {
        symbol: String,
        entry_timestamp: Option<String>,
        qty: f64,
        avg_entry_price: f64,
        current_price: f64,
        market_value: f64,
        unrealized_pl: f64,
        unrealized_plpc_pct: f64,
        asset_class: Option<String>,
        exchange: Option<String>,
        side: Option<String>,
        synced_at_utc: String,
        execution_origin: origin::ExecutionOrigin,
        management_origin: origin::ExecutionOrigin,
        management_reason: Option<String>,
        management_updated_at: Option<String>,
    }

    let mut account_json = Vec::new();
    for account in &accounts {
        let client = build_client(account);
        let acct: Option<AccountInfo> =
            api_get(&client, &alpaca::broker_api_url_for(account, "/account"))
                .await
                .ok();
        let equity = acct
            .as_ref()
            .and_then(|a| a.equity.as_deref())
            .and_then(|value| value.parse::<f64>().ok());
        let cash = acct
            .as_ref()
            .and_then(|a| a.cash.as_deref())
            .and_then(|value| value.parse::<f64>().ok());
        let broker_id = acct.as_ref().and_then(broker_account_id);
        let mut provider_position_source = "db_snapshot".to_string();
        let mut provider_position_sync_error = None::<String>;
        match api_get::<Vec<alpaca::Position>>(
            &client,
            &alpaca::broker_api_url_for(account, "/positions"),
        )
        .await
        {
            Ok(live_positions) => {
                if let Err(err) = sync_provider_position_snapshots(
                    &conn,
                    account,
                    broker_id.as_deref(),
                    &live_positions,
                ) {
                    provider_position_sync_error = Some(err.to_string());
                } else {
                    provider_position_source = "live_synced".to_string();
                }
            }
            Err(err) => {
                provider_position_sync_error = Some(err.to_string());
            }
        }

        let mut stmt = conn.prepare(
            "SELECT symbol, entry_date, entry_timestamp, entry_price, shares, cost_basis, stop_loss_price,
                    take_profit_price, exit_by_date, ml_quintile, ml_score,
                    COALESCE(execution_origin, 'mlai_auto')
             FROM auto_positions
             WHERE provider=?1 AND account_ref=?2 AND paper_account=?3 AND status='open'
             ORDER BY entry_date",
        )?;
        let positions: Vec<OpenPos> = stmt
            .query_map(
                params![
                    account.provider(),
                    account.account_ref(),
                    paper_flag(account)
                ],
                |r| {
                    Ok(OpenPos {
                        symbol: r.get(0)?,
                        entry_date: r.get(1)?,
                        entry_timestamp: r.get(2)?,
                        entry_price: r.get(3)?,
                        shares: r.get(4)?,
                        cost_basis: r.get(5)?,
                        stop_loss: r.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
                        take_profit: r.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
                        exit_by: r.get::<_, Option<String>>(8)?.unwrap_or_default(),
                        ml_q: r.get::<_, Option<i64>>(9)?.unwrap_or(0),
                        ml_score: r.get::<_, Option<f64>>(10)?.unwrap_or(0.0),
                        execution_origin: origin::ExecutionOrigin::parse(
                            &r.get::<_, String>(11)
                                .unwrap_or_else(|_| "mlai_auto".to_string()),
                        ),
                    })
                },
            )?
            .filter_map(|r| r.ok())
            .collect();
        let auto_symbols: HashSet<String> = positions
            .iter()
            .map(|position| position.symbol.to_ascii_uppercase())
            .collect();

        let mut provider_stmt = conn.prepare(
            "SELECT p.symbol,
                    COALESCE((
                        SELECT MIN(f.transaction_time)
                        FROM provider_fill_activities f
                        WHERE f.provider=p.provider
                          AND f.account_ref=p.account_ref
                          AND f.paper_account=p.paper_account
                          AND UPPER(f.symbol)=UPPER(p.symbol)
                          AND UPPER(COALESCE(f.side, ''))='BUY'
                          AND COALESCE(f.transaction_time, '') <> ''
                    ), NULL) AS entry_timestamp,
                    COALESCE(p.qty, 0.0), COALESCE(p.avg_entry_price, 0.0),
                    COALESCE(p.current_price, 0.0), COALESCE(p.market_value, 0.0),
                    COALESCE(p.unrealized_pl, 0.0), COALESCE(p.unrealized_plpc, 0.0),
                    p.asset_class, p.exchange, p.side, p.synced_at_utc,
                    COALESCE((
                        SELECT CASE
                            WHEN COUNT(*) = 0 THEN NULL
                            WHEN COUNT(DISTINCT COALESCE(NULLIF(f.execution_origin, ''), 'unknown')) > 1
                                THEN 'mixed'
                            ELSE MIN(COALESCE(NULLIF(f.execution_origin, ''), 'unknown'))
                        END
                        FROM provider_fill_activities f
                        WHERE f.provider=p.provider
                          AND f.account_ref=p.account_ref
                          AND f.paper_account=p.paper_account
                          AND UPPER(f.symbol)=UPPER(p.symbol)
                          AND UPPER(COALESCE(f.side, ''))='BUY'
                    ), 'unknown') AS execution_origin,
                    COALESCE(o.management_origin, (
                        SELECT CASE
                            WHEN COUNT(*) = 0 THEN NULL
                            WHEN COUNT(DISTINCT COALESCE(NULLIF(f.execution_origin, ''), 'unknown')) > 1
                                THEN 'mixed'
                            ELSE MIN(COALESCE(NULLIF(f.execution_origin, ''), 'unknown'))
                        END
                        FROM provider_fill_activities f
                        WHERE f.provider=p.provider
                          AND f.account_ref=p.account_ref
                          AND f.paper_account=p.paper_account
                          AND UPPER(f.symbol)=UPPER(p.symbol)
                          AND UPPER(COALESCE(f.side, ''))='BUY'
                    ), 'unknown') AS management_origin,
                    o.reason,
                    o.updated_at_utc
             FROM provider_position_snapshots p
             LEFT JOIN position_management_overrides o
               ON o.provider=p.provider
              AND o.account_ref=p.account_ref
              AND o.paper_account=p.paper_account
              AND UPPER(o.symbol)=UPPER(p.symbol)
             WHERE p.provider=?1 AND p.account_ref=?2 AND p.paper_account=?3
             ORDER BY UPPER(p.symbol)",
        )?;
        let provider_positions: Vec<ProviderOpenPos> = provider_stmt
            .query_map(
                params![
                    account.provider(),
                    account.account_ref(),
                    paper_flag(account)
                ],
                |r| {
                    let raw_origin = r
                        .get::<_, String>(12)
                        .unwrap_or_else(|_| "unknown".to_string());
                    Ok(ProviderOpenPos {
                        symbol: r.get(0)?,
                        entry_timestamp: r.get(1)?,
                        qty: r.get(2)?,
                        avg_entry_price: r.get(3)?,
                        current_price: r.get(4)?,
                        market_value: r.get(5)?,
                        unrealized_pl: r.get(6)?,
                        unrealized_plpc_pct: r.get::<_, f64>(7)? * 100.0,
                        asset_class: r.get(8)?,
                        exchange: r.get(9)?,
                        side: r.get(10)?,
                        synced_at_utc: r.get(11)?,
                        execution_origin: origin::ExecutionOrigin::parse(&raw_origin),
                        management_origin: origin::ExecutionOrigin::parse(
                            &r.get::<_, String>(13)
                                .unwrap_or_else(|_| raw_origin.clone()),
                        ),
                        management_reason: r.get(14)?,
                        management_updated_at: r.get(15)?,
                    })
                },
            )?
            .filter_map(|r| r.ok())
            .collect();

        let total_closed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM auto_positions
                 WHERE provider=?1 AND account_ref=?2 AND paper_account=?3 AND status='closed'",
                params![
                    account.provider(),
                    account.account_ref(),
                    paper_flag(account)
                ],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let total_pnl: f64 = conn
            .query_row(
                "SELECT COALESCE(SUM(pnl), 0.0) FROM auto_positions
                 WHERE provider=?1 AND account_ref=?2 AND paper_account=?3 AND status='closed'",
                params![
                    account.provider(),
                    account.account_ref(),
                    paper_flag(account)
                ],
                |r| r.get(0),
            )
            .unwrap_or(0.0);
        let win_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM auto_positions
                 WHERE provider=?1 AND account_ref=?2 AND paper_account=?3 AND status='closed' AND pnl > 0",
                params![account.provider(), account.account_ref(), paper_flag(account)],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let total_trades: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM auto_trades WHERE provider=?1 AND account_ref=?2 AND paper_account=?3",
                params![account.provider(), account.account_ref(), paper_flag(account)],
                |r| r.get(0),
            )
            .unwrap_or(0);

        let mut pos_json = Vec::new();
        let mut total_cost = 0.0;
        let mut total_unrealized = 0.0;
        for p in &positions {
            let current_price = get_execution_price(
                &conn,
                &client,
                account,
                &p.symbol,
                ExecutionSide::Sell,
                &cfg,
            )
            .await
            .map(|price| price.price)
            .unwrap_or(p.entry_price);
            let unrealized = (current_price - p.entry_price) * p.shares as f64;
            let pnl_pct = (current_price / p.entry_price - 1.0) * 100.0;
            total_cost += p.cost_basis;
            total_unrealized += unrealized;
            pos_json.push(serde_json::json!({
                "symbol": p.symbol,
                "entry_date": p.entry_date,
                "entry_timestamp": p.entry_timestamp,
                "entry_price": p.entry_price,
                "current_price": current_price,
                "shares": p.shares,
                "cost_basis": p.cost_basis,
                "unrealized_pnl": unrealized,
                "unrealized_pnl_pct": pnl_pct,
                "stop_loss": p.stop_loss,
                "take_profit": p.take_profit,
                "exit_by": p.exit_by,
                "ml_quintile": p.ml_q,
                "ml_score": p.ml_score,
                "execution_origin": p.execution_origin.as_str(),
                "execution_origin_label": status_origin_label(account.provider(), p.execution_origin.as_str()),
                "management_origin": origin::ExecutionOrigin::MlaiAuto.as_str(),
                "management_origin_label": origin::ExecutionOrigin::MlaiAuto.short_label(),
                "tracking_state": "auto_managed",
            }));
        }
        let provider_pos_json = provider_positions
            .iter()
            .map(|p| {
                serde_json::json!({
                    "symbol": p.symbol,
                    "entry_timestamp": p.entry_timestamp,
                    "qty": p.qty,
                    "avg_entry_price": p.avg_entry_price,
                    "current_price": p.current_price,
                    "market_value": p.market_value,
                    "unrealized_pnl": p.unrealized_pl,
                    "unrealized_pnl_pct": p.unrealized_plpc_pct,
                    "asset_class": p.asset_class,
                    "exchange": p.exchange,
                    "side": p.side,
                    "synced_at_utc": p.synced_at_utc,
                    "execution_origin": p.execution_origin.as_str(),
                    "execution_origin_label": status_origin_label(account.provider(), p.execution_origin.as_str()),
                    "management_origin": if auto_symbols.contains(&p.symbol.to_ascii_uppercase()) {
                        origin::ExecutionOrigin::MlaiAuto.as_str()
                    } else {
                        p.management_origin.as_str()
                    },
                    "management_origin_label": if auto_symbols.contains(&p.symbol.to_ascii_uppercase()) {
                        origin::ExecutionOrigin::MlaiAuto.short_label().to_string()
                    } else {
                        status_origin_label(account.provider(), p.management_origin.as_str())
                    },
                    "management_reason": p.management_reason,
                    "management_updated_at": p.management_updated_at,
                    "tracking_state": if auto_symbols.contains(&p.symbol.to_ascii_uppercase()) {
                        "auto_managed"
                    } else {
                        "not_tracked"
                    },
                    "auto_managed": auto_symbols.contains(&p.symbol.to_ascii_uppercase()),
                })
            })
            .collect::<Vec<_>>();
        let unmanaged_pos_json = provider_positions
            .iter()
            .filter(|p| !auto_symbols.contains(&p.symbol.to_ascii_uppercase()))
            .map(|p| {
                serde_json::json!({
                    "symbol": p.symbol,
                    "entry_timestamp": p.entry_timestamp,
                    "qty": p.qty,
                    "avg_entry_price": p.avg_entry_price,
                    "current_price": p.current_price,
                    "market_value": p.market_value,
                    "unrealized_pnl": p.unrealized_pl,
                    "unrealized_pnl_pct": p.unrealized_plpc_pct,
                    "asset_class": p.asset_class,
                    "exchange": p.exchange,
                    "side": p.side,
                    "synced_at_utc": p.synced_at_utc,
                    "execution_origin": p.execution_origin.as_str(),
                    "execution_origin_label": status_origin_label(account.provider(), p.execution_origin.as_str()),
                    "management_origin": p.management_origin.as_str(),
                    "management_origin_label": status_origin_label(account.provider(), p.management_origin.as_str()),
                    "management_reason": p.management_reason,
                    "management_updated_at": p.management_updated_at,
                    "tracking_state": "not_tracked",
                    "auto_managed": false,
                })
            })
            .collect::<Vec<_>>();

        let auto_managed_open_count = positions.len();
        let provider_open_count = provider_positions.len();
        let unmanaged_open_count = unmanaged_pos_json.len();
        account_json.push(serde_json::json!({
            "provider": account.provider(),
            "account_ref": account.account_ref(),
            "account_mode": alpaca::account_mode_for(account),
            "tax_universe": if account.is_paper() { "paper" } else { "real" },
            "auto_trade_enabled": account.auto_trade_enabled,
            "broker_account_id": broker_id,
            "equity": equity,
            "cash": cash,
            "open_positions": pos_json.clone(),
            "open_count": auto_managed_open_count,
            "auto_managed_positions": pos_json,
            "auto_managed_open_count": auto_managed_open_count,
            "provider_positions": provider_pos_json,
            "provider_open_count": provider_open_count,
            "provider_position_source": provider_position_source,
            "provider_position_sync_error": provider_position_sync_error,
            "unmanaged_positions": unmanaged_pos_json,
            "unmanaged_open_count": unmanaged_open_count,
            "max_positions": cfg.max_positions,
            "invested_cost": total_cost,
            "unrealized_pnl": total_unrealized,
            "closed_total": total_closed,
            "closed_pnl": total_pnl,
            "win_rate": if total_closed > 0 { win_count as f64 / total_closed as f64 } else { 0.0 },
            "total_trades": total_trades,
        }));
    }

    if json {
        println!(
            "{}",
            serde_json::json!({
                "enabled": enabled,
                "accounts": account_json,
                "config": {
                    "position_size_pct": cfg.position_size_pct,
                    "stop_loss_pct": cfg.stop_loss_pct,
                    "take_profit_pct": cfg.take_profit_pct,
                    "stop_loss_confirmation": {
                        "enabled": cfg.stop_loss_confirmation.enabled,
                        "cycles": cfg.stop_loss_confirmation.cycles,
                        "max_confirmation_minutes": cfg.stop_loss_confirmation.max_confirmation_minutes,
                        "emergency_stop_loss_pct": cfg.stop_loss_confirmation.emergency_stop_loss_pct,
                    },
                    "take_profit_confirmation": {
                        "enabled": cfg.take_profit_confirmation.enabled,
                        "cycles": cfg.take_profit_confirmation.cycles,
                        "min_hold_minutes": cfg.take_profit_confirmation.min_hold_minutes,
                        "trailing_enabled": cfg.take_profit_confirmation.trailing_enabled,
                        "trailing_giveback_pct": cfg.take_profit_confirmation.trailing_giveback_pct,
                    },
                    "max_hold_days": cfg.max_hold_days,
                    "min_price": cfg.min_price,
                    "min_avg_volume": cfg.min_avg_volume,
                    "max_spread_bps": cfg.max_spread_bps,
                    "min_quote_size": cfg.min_quote_size,
                    "allow_bar_price_fallback": cfg.allow_bar_price_fallback,
                    "bar_fallback_bps": cfg.bar_fallback_bps,
                    "irs_wash_sale_window_days": compliance::IRS_WASH_SALE_WINDOW_DAYS,
                    "wash_sale_safety_buffer_days": cfg.wash_sale_safety_buffer_days,
                    "wash_sale_effective_forward_block_days": compliance::wash_sale_forward_block_days(Some(cfg.wash_sale_safety_buffer_days)),
                }
            })
        );
        return Ok(());
    }

    println!(
        "Auto-Trading Status - {}",
        if enabled { "ENABLED" } else { "DISABLED" }
    );
    println!("{}", "=".repeat(50));
    println!("Strategy:");
    println!("  Position size: {:.1}% of equity", cfg.position_size_pct);
    println!("  Stop loss:     -{:.1}%", cfg.stop_loss_pct);
    println!(
        "    confirm:     {} for {} cycles, emergency -{:.1}%, max wait {}m",
        if cfg.stop_loss_confirmation.enabled {
            "enabled"
        } else {
            "disabled"
        },
        cfg.stop_loss_confirmation.cycles,
        cfg.stop_loss_confirmation.emergency_stop_loss_pct,
        cfg.stop_loss_confirmation.max_confirmation_minutes
    );
    println!("  Take profit:   +{:.1}%", cfg.take_profit_pct);
    println!(
        "    confirm:     {} for {} cycles, min hold {}m, trailing {} giveback {:.1}%",
        if cfg.take_profit_confirmation.enabled {
            "enabled"
        } else {
            "disabled"
        },
        cfg.take_profit_confirmation.cycles,
        cfg.take_profit_confirmation.min_hold_minutes,
        cfg.take_profit_confirmation.trailing_enabled,
        cfg.take_profit_confirmation.trailing_giveback_pct
    );
    println!("  Max hold:      {} business days", cfg.max_hold_days);
    println!("  Max spread:    {:.1} bps", cfg.max_spread_bps);
    println!(
        "  Wash sale:     {} IRS days + {} buffer",
        compliance::IRS_WASH_SALE_WINDOW_DAYS,
        cfg.wash_sale_safety_buffer_days
    );
    println!(
        "  ML buy/exit:   Q{} / Q{}+",
        cfg.ml_quintile_buy, cfg.ml_quintile_exit
    );

    if account_json.is_empty() {
        println!("\nNo enabled auto-trade accounts.");
        return Ok(());
    }

    for account in &account_json {
        let provider = account["provider"].as_str().unwrap_or("provider");
        println!(
            "\n{}:{} [{} / {}]",
            provider,
            account["account_ref"].as_str().unwrap_or("?"),
            account["account_mode"].as_str().unwrap_or("?"),
            account["tax_universe"].as_str().unwrap_or("?")
        );
        println!(
            "  Auto trading: {}",
            if account["auto_trade_enabled"].as_bool().unwrap_or(true) {
                "enabled for this account"
            } else {
                "disabled for this account"
            }
        );
        println!(
            "  Equity: {} | Cash: {} | Closed P&L: {:+.2}",
            account["equity"]
                .as_f64()
                .map(|v| format!("${:.2}", v))
                .unwrap_or_else(|| "?".to_string()),
            account["cash"]
                .as_f64()
                .map(|v| format!("${:.2}", v))
                .unwrap_or_else(|| "?".to_string()),
            account["closed_pnl"].as_f64().unwrap_or(0.0)
        );
        println!(
            "  Auto-managed: {}/{} | {} open: {} | Not tracked: {}",
            account["auto_managed_open_count"].as_u64().unwrap_or(0),
            account["max_positions"].as_i64().unwrap_or(0),
            provider,
            account["provider_open_count"].as_u64().unwrap_or(0),
            account["unmanaged_open_count"].as_u64().unwrap_or(0)
        );
        println!(
            "  Auto invested: ${:.2} | Auto unrealized P&L: {:+.2} | Auto orders: {}",
            account["invested_cost"].as_f64().unwrap_or(0.0),
            account["unrealized_pnl"].as_f64().unwrap_or(0.0),
            account["total_trades"].as_i64().unwrap_or(0)
        );
        if let Some(err) = account["provider_position_sync_error"].as_str() {
            println!(
                "  {} position source: {} (live sync failed: {})",
                provider,
                account["provider_position_source"]
                    .as_str()
                    .unwrap_or("db_snapshot"),
                err
            );
        } else {
            println!(
                "  {} position source: {}",
                provider,
                account["provider_position_source"]
                    .as_str()
                    .unwrap_or("db_snapshot")
            );
        }
        let positions = account["auto_managed_positions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if positions.is_empty() {
            println!("  No auto-managed open positions.");
        } else {
            println!("  Auto-managed positions (tracked by auto rules):");
            println!(
                "  {:<8} {:<10} {:>10} {:>10} {:>10} {:>12} {:>12} {:>9} {:>4}",
                "Symbol", "Origin", "Qty", "Avg Cost", "Current", "Mkt Value", "P&L", "P&L%", "MLQ"
            );
            for p in &positions {
                let qty = p["shares"].as_i64().unwrap_or(0) as f64;
                let current_price = p["current_price"].as_f64().unwrap_or(0.0);
                let market_value = current_price * qty;
                println!(
                    "  {:<8} {:<10} {:>10.2} {:>10.2} {:>10.2} {:>12.2} {:>+12.2} {:>+8.1}% {:>4}",
                    p["symbol"].as_str().unwrap_or("?"),
                    p["management_origin_label"].as_str().unwrap_or("unknown"),
                    qty,
                    p["entry_price"].as_f64().unwrap_or(0.0),
                    current_price,
                    market_value,
                    p["unrealized_pnl"].as_f64().unwrap_or(0.0),
                    p["unrealized_pnl_pct"].as_f64().unwrap_or(0.0),
                    format!("Q{}", p["ml_quintile"].as_i64().unwrap_or(0))
                );
            }
        }
        let unmanaged_positions = account["unmanaged_positions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if unmanaged_positions.is_empty() {
            println!("  No {} positions outside auto tracking.", provider);
        } else {
            println!("  {} positions not tracked by auto:", provider);
            println!(
                "  {:<8} {:<10} {:>10} {:>10} {:>10} {:>12} {:>12} {:>9} {:>4}",
                "Symbol", "Origin", "Qty", "Avg Cost", "Current", "Mkt Value", "P&L", "P&L%", "MLQ"
            );
            for p in &unmanaged_positions {
                println!(
                    "  {:<8} {:<10} {:>10.2} {:>10.2} {:>10.2} {:>12.2} {:>+12.2} {:>+8.1}% {:>4}",
                    p["symbol"].as_str().unwrap_or("?"),
                    p["management_origin_label"].as_str().unwrap_or("unknown"),
                    p["qty"].as_f64().unwrap_or(0.0),
                    p["avg_entry_price"].as_f64().unwrap_or(0.0),
                    p["current_price"].as_f64().unwrap_or(0.0),
                    p["market_value"].as_f64().unwrap_or(0.0),
                    p["unrealized_pnl"].as_f64().unwrap_or(0.0),
                    p["unrealized_pnl_pct"].as_f64().unwrap_or(0.0),
                    "-"
                );
            }
        }
    }
    Ok(())
}

// ══════════════════════════════════════════════════════════════════
// CMD: auto history
// ══════════════════════════════════════════════════════════════════

pub async fn cmd_auto_history(limit: u32, json: bool) -> anyhow::Result<()> {
    let conn = open_db()?;
    init_auto_tables(&conn)?;

    let mut stmt = conn.prepare(
        "SELECT provider, account_ref, account_mode, paper_account, symbol, entry_date, exit_date,
                entry_price, exit_price, shares, pnl, pnl_pct, exit_reason, ml_quintile,
                COALESCE(execution_origin, 'mlai_auto')
         FROM auto_positions WHERE status='closed' ORDER BY exit_date DESC LIMIT ?1",
    )?;
    let rows: Vec<(
        String,
        String,
        String,
        i64,
        String,
        String,
        Option<String>,
        f64,
        Option<f64>,
        i64,
        Option<f64>,
        Option<f64>,
        Option<String>,
        Option<i64>,
        String,
    )> = stmt
        .query_map(params![limit], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
                r.get(11)?,
                r.get(12)?,
                r.get(13)?,
                r.get(14)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();

    if json {
        let items: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "provider": r.0,
                    "account_ref": r.1,
                    "account_mode": r.2,
                    "tax_universe": if r.3 == 1 { "paper" } else { "real" },
                    "symbol": r.4,
                    "entry_date": r.5,
                    "exit_date": r.6,
                    "entry_price": r.7,
                    "exit_price": r.8,
                    "shares": r.9,
                    "pnl": r.10,
                    "pnl_pct": r.11,
                    "exit_reason": r.12,
                    "ml_quintile": r.13,
                    "execution_origin": r.14,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({"trades": items, "count": items.len()})
        );
    } else {
        println!("📜 Auto-Trade History (last {})", limit);
        println!("{}", "═".repeat(90));
        if rows.is_empty() {
            println!("  No closed trades.");
            return Ok(());
        }
        println!(
            "{:<18} {:<8} {:<10} {:>10} {:>10} {:>8} {:>8} {:>6} {:>10} {:>7} {}",
            "Account",
            "Symbol",
            "Origin",
            "Entry",
            "Exit",
            "Buy$",
            "Sell$",
            "Shares",
            "P&L",
            "P&L%",
            "Reason"
        );
        println!("{}", "─".repeat(122));
        for r in &rows {
            let emoji = if r.10.unwrap_or(0.0) >= 0.0 {
                "🟢"
            } else {
                "🔴"
            };
            let account = format!("{}:{}", r.0, r.1);
            println!(
                "{}{:<17} {:<8} {:<10} {:>10} {:>10} {:>8.2} {:>8.2} {:>6} {:>+10.2} {:>+6.1}% {}",
                emoji,
                account,
                r.4,
                origin::ExecutionOrigin::parse(&r.14).short_label(),
                r.5,
                r.6.as_deref().unwrap_or("-"),
                r.7,
                r.8.unwrap_or(0.0),
                r.9,
                r.10.unwrap_or(0.0),
                r.11.unwrap_or(0.0),
                r.12.as_deref().unwrap_or("?")
            );
        }
    }
    Ok(())
}

// ══════════════════════════════════════════════════════════════════
// CMD: auto enable / disable
// ══════════════════════════════════════════════════════════════════

pub fn cmd_auto_enable(json: bool) -> anyhow::Result<()> {
    let conn = open_db()?;
    init_auto_tables(&conn)?;
    set_config(&conn, "enabled", "true")?;
    if json {
        println!("{{\"enabled\":true}}");
    } else {
        println!("🟢 Auto-trading ENABLED. Will execute on next `mlai-trade auto run`.");
    }
    Ok(())
}

// Handles the auto disable CLI action.
pub fn cmd_auto_disable(json: bool) -> anyhow::Result<()> {
    let conn = open_db()?;
    init_auto_tables(&conn)?;
    set_config(&conn, "enabled", "false")?;
    if json {
        println!("{{\"enabled\":false}}");
    } else {
        println!("🔴 Auto-trading DISABLED. Open positions will remain until manually closed or re-enabled.");
    }
    Ok(())
}

// ══════════════════════════════════════════════════════════════════
// CMD: auto config — show/set strategy parameters
// ══════════════════════════════════════════════════════════════════

pub fn cmd_auto_config(
    key: Option<String>,
    value: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let conn = open_db()?;
    init_auto_tables(&conn)?;

    match (key, value) {
        (Some(k), Some(v)) => {
            let valid_keys = [
                "max_positions",
                "position_size_pct",
                "stop_loss_pct",
                "take_profit_pct",
                "stop_loss_confirmation_enabled",
                "stop_loss_confirmation_cycles",
                "stop_loss_confirmation_max_confirmation_minutes",
                "stop_loss_confirmation_emergency_stop_loss_pct",
                "take_profit_confirmation_enabled",
                "take_profit_confirmation_cycles",
                "take_profit_confirmation_min_hold_minutes",
                "take_profit_confirmation_trailing_enabled",
                "take_profit_confirmation_trailing_giveback_pct",
                "max_hold_days",
                "min_price",
                "min_avg_volume",
                "max_spread_bps",
                "min_quote_size",
                "allow_bar_price_fallback",
                "bar_fallback_bps",
                "ml_quintile_buy",
                "ml_quintile_exit",
                "wash_sale_safety_buffer_days",
            ];
            if !valid_keys.contains(&k.as_str()) {
                anyhow::bail!("Unknown config key: {}. Valid: {:?}", k, valid_keys);
            }
            if k == "wash_sale_safety_buffer_days" {
                let parsed = v.parse::<i64>().map_err(|err| {
                    anyhow::anyhow!("wash_sale_safety_buffer_days must be an integer: {}", err)
                })?;
                if parsed < compliance::MIN_WASH_SALE_SAFETY_BUFFER_DAYS {
                    anyhow::bail!(
                        "wash_sale_safety_buffer_days cannot be below {}. IRS wash-sale days are hardcoded at {}; config can only keep or increase the safety buffer.",
                        compliance::MIN_WASH_SALE_SAFETY_BUFFER_DAYS,
                        compliance::IRS_WASH_SALE_WINDOW_DAYS
                    );
                }
            }
            set_config(&conn, &k, &v)?;
            if json {
                println!("{{\"key\":\"{}\",\"value\":\"{}\"}}", k, v);
            } else {
                println!("✅ Set {} = {}", k, v);
            }
        }
        _ => {
            let cfg = load_config(&conn);
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "enabled": is_enabled(&conn),
                        "max_positions": cfg.max_positions,
                        "position_size_pct": cfg.position_size_pct,
                        "stop_loss_pct": cfg.stop_loss_pct,
                        "take_profit_pct": cfg.take_profit_pct,
                        "stop_loss_confirmation": {
                            "enabled": cfg.stop_loss_confirmation.enabled,
                            "cycles": cfg.stop_loss_confirmation.cycles,
                            "max_confirmation_minutes": cfg.stop_loss_confirmation.max_confirmation_minutes,
                            "emergency_stop_loss_pct": cfg.stop_loss_confirmation.emergency_stop_loss_pct,
                        },
                        "take_profit_confirmation": {
                            "enabled": cfg.take_profit_confirmation.enabled,
                            "cycles": cfg.take_profit_confirmation.cycles,
                            "min_hold_minutes": cfg.take_profit_confirmation.min_hold_minutes,
                            "trailing_enabled": cfg.take_profit_confirmation.trailing_enabled,
                            "trailing_giveback_pct": cfg.take_profit_confirmation.trailing_giveback_pct,
                        },
                        "max_hold_days": cfg.max_hold_days,
                        "min_price": cfg.min_price,
                        "min_avg_volume": cfg.min_avg_volume,
                        "max_spread_bps": cfg.max_spread_bps,
                        "min_quote_size": cfg.min_quote_size,
                        "allow_bar_price_fallback": cfg.allow_bar_price_fallback,
                        "bar_fallback_bps": cfg.bar_fallback_bps,
                        "ml_quintile_buy": cfg.ml_quintile_buy,
                        "ml_quintile_exit": cfg.ml_quintile_exit,
                        "irs_wash_sale_window_days": compliance::IRS_WASH_SALE_WINDOW_DAYS,
                        "wash_sale_safety_buffer_days": cfg.wash_sale_safety_buffer_days,
                        "wash_sale_effective_forward_block_days": compliance::wash_sale_forward_block_days(Some(cfg.wash_sale_safety_buffer_days)),
                    })
                );
            } else {
                println!("⚙️  Auto-Trading Config");
                println!("{}", "─".repeat(40));
                println!("  enabled:           {}", is_enabled(&conn));
                println!("  max_positions:     {}", cfg.max_positions);
                println!("  position_size_pct: {:.1}%", cfg.position_size_pct);
                println!("  stop_loss_pct:     {:.1}%", cfg.stop_loss_pct);
                println!("  take_profit_pct:   {:.1}%", cfg.take_profit_pct);
                println!(
                    "  stop_confirm:      {} cycles={} max_wait={}m emergency={:.1}%",
                    cfg.stop_loss_confirmation.enabled,
                    cfg.stop_loss_confirmation.cycles,
                    cfg.stop_loss_confirmation.max_confirmation_minutes,
                    cfg.stop_loss_confirmation.emergency_stop_loss_pct
                );
                println!(
                    "  profit_confirm:    {} cycles={} min_hold={}m trailing={} giveback={:.1}%",
                    cfg.take_profit_confirmation.enabled,
                    cfg.take_profit_confirmation.cycles,
                    cfg.take_profit_confirmation.min_hold_minutes,
                    cfg.take_profit_confirmation.trailing_enabled,
                    cfg.take_profit_confirmation.trailing_giveback_pct
                );
                println!("  max_hold_days:     {}", cfg.max_hold_days);
                println!("  min_price:         ${:.0}", cfg.min_price);
                println!("  min_avg_volume:    {}", cfg.min_avg_volume);
                println!("  max_spread_bps:    {:.1}", cfg.max_spread_bps);
                println!("  min_quote_size:    {:.0}", cfg.min_quote_size);
                println!(
                    "  bar_fallback:      {} ({:.1} bps per side)",
                    cfg.allow_bar_price_fallback, cfg.bar_fallback_bps
                );
                println!("  ml_quintile_buy:   Q{}", cfg.ml_quintile_buy);
                println!("  ml_quintile_exit:  Q{}+", cfg.ml_quintile_exit);
                println!(
                    "  wash_sale_days:    {} IRS + {} buffer = {} days",
                    compliance::IRS_WASH_SALE_WINDOW_DAYS,
                    cfg.wash_sale_safety_buffer_days,
                    compliance::wash_sale_forward_block_days(Some(
                        cfg.wash_sale_safety_buffer_days
                    ))
                );
            }
        }
    }
    Ok(())
}
