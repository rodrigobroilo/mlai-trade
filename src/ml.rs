// ══════════════════════════════════════════════════════════════════
// ML MODULE — Feature engineering, label computation, export, predict
// ══════════════════════════════════════════════════════════════════
//
// Features computed purely from OHLCV bars in the Alpaca market research DB:
//   - Momentum returns (1d, 5d, 20d, 60d)
//   - Volatility (20d rolling std of daily returns)
//   - Volume ratios (5d, 20d)
//   - VWAP distance
//   - Range features (high-low, close-to-high/low 20d)
//   - Technical indicators (RSI-14, MACD signal, Bollinger position, SMA cross, ATR-14, OBV slope)
//   - Cross-sectional percentile ranks (return, volume, volatility, momentum)
//   - Market context from SPY, QQQ, VIX, and sector ETF basket proxies
//   - Feed context from dated news/filing aggregates; missing feed data is neutral zero
//
// Labels:
//   - Forward 5/10/20 day returns
//
// Storage: wide-format table ml_features (one row per symbol-date, 26 feature columns)
//
// Function map:
// - cmd_ml_features/labels/export(): build feature, label, and dataset artifacts.
// - write_lgb_training_files*(): stream SQLite rows into bounded ML datasets.
// - cmd_ml_train/baselines/walk_forward(): train and validate model families.
// - cmd_ml_predict/ensemble/explain*: refresh predictions, ensemble, and SHAP.
// ══════════════════════════════════════════════════════════════════

use crate::accelerators;
use crate::config;
use crate::paths;
use chrono::NaiveDate;
use lightgbm3::{Booster, Dataset, ImportanceType};
use rayon::prelude::*;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::BufRead;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

#[cfg(mlai_xgboost)]
use std::ffi::{CStr, CString};

#[cfg(mlai_xgboost)]
extern crate openmp_sys;

/// All feature column names in the wide table
const FEATURE_COLS: &[&str] = &[
    "return_1d",
    "return_5d",
    "return_20d",
    "return_60d",
    "volatility_20d",
    "volume_ratio_5d",
    "volume_ratio_20d",
    "vwap_distance",
    "high_low_range",
    "close_to_high_20d",
    "close_to_low_20d",
    "rsi_14",
    "macd_signal",
    "bb_position",
    "sma_cross_50_200",
    "atr_14",
    "obv_slope_20d",
    "sp500_return_1d",
    "sp500_return_5d",
    "sp500_return_20d",
    "relative_return_20d",
    "spy_return_1d",
    "spy_return_5d",
    "spy_return_20d",
    "qqq_return_1d",
    "qqq_return_5d",
    "qqq_return_20d",
    "relative_spy_20d",
    "relative_qqq_20d",
    "vix_level",
    "vix_change_1d",
    "vix_change_5d",
    "vix_change_20d",
    "sector_avg_return_20d",
    "relative_sector_avg_20d",
    "feed_sentiment_1d",
    "feed_sentiment_3d",
    "feed_sentiment_7d",
    "feed_sentiment_30d",
    "feed_article_count_1d",
    "feed_article_count_7d",
    "feed_article_count_30d",
    "feed_sec_8k_7d",
    "feed_form4_7d",
    "feed_negative_count_7d",
    "feed_universe_return_20d",
    "relative_feed_universe_20d",
    "feed_universe_corr_30d",
    "feed_universe_corr_90d",
    "rank_return_1d",
    "rank_volume_ratio",
    "rank_volatility",
    "rank_momentum",
];

const SP500_FEATURE_COLS: &[&str] = &[
    "sp500_return_1d",
    "sp500_return_5d",
    "sp500_return_20d",
    "relative_return_20d",
];

const SECTOR_ETFS: &[&str] = &[
    "XLB", "XLC", "XLE", "XLF", "XLI", "XLK", "XLP", "XLRE", "XLU", "XLV", "XLY",
];

const DEFAULT_ROUND_TRIP_SPREAD_SLIPPAGE_BPS: f64 = 50.0;
const INCREMENTAL_FEATURE_WARMUP_BARS: usize = 512;

type ReturnFeatures = HashMap<String, (Option<f64>, Option<f64>, Option<f64>)>;

// Handles ml eligible asset predicate logic.
pub fn ml_eligible_asset_predicate(symbol_expr: &str, asset_alias: &str) -> String {
    let name = format!("LOWER(COALESCE({asset_alias}.name, ''))");
    let symbol = format!("UPPER({symbol_expr})");
    let blocked = config::blocked_symbols_sql_predicate(symbol_expr);
    format!(
        "({blocked}
          AND {symbol} NOT LIKE '%.WS'
          AND (
              (SELECT COUNT(*) FROM assets) = 0
              OR (
                  {asset_alias}.symbol IS NOT NULL
                  AND LOWER(COALESCE({asset_alias}.status, 'inactive')) = 'active'
                  AND COALESCE({asset_alias}.tradable, 0) = 1
              )
          )
          AND {name} NOT LIKE '%warrant%'
          AND {name} NOT LIKE '% right%'
          AND {name} NOT LIKE '%rights%'
          AND {name} NOT LIKE '% unit%'
          AND {name} NOT LIKE '%units%'
          AND {name} NOT LIKE '%preferred%'
          AND {name} NOT LIKE '%preference%'
          AND {name} NOT LIKE '%depositary%'
          AND {name} NOT LIKE '%debenture%'
          AND {name} NOT LIKE '%note due%'
          AND {name} NOT LIKE '%senior note%'
          AND {name} NOT LIKE '%subordinated note%')"
    )
}

// Handles ml eligible asset join logic.
fn ml_eligible_asset_join(table_alias: &str, asset_alias: &str) -> String {
    format!("LEFT JOIN assets {asset_alias} ON {asset_alias}.symbol = {table_alias}.symbol")
}

#[derive(Debug, Clone, Default)]
struct MarketContext {
    sp500: ReturnFeatures,
    spy: ReturnFeatures,
    qqq: ReturnFeatures,
    vix: VixFeatures,
    sector_avg_20d: HashMap<String, Option<f64>>,
    feeds: HashMap<String, HashMap<String, FeedAgg>>,
    feed_universe: FeedUniverseContext,
}

type VixFeature = (Option<f64>, Option<f64>, Option<f64>, Option<f64>);
type VixFeatures = HashMap<String, VixFeature>;
type AssetStatusRow = (Option<String>, Option<i64>, Option<String>, Option<String>);

#[derive(Debug, Clone, Default)]
struct FeedUniverseContext {
    daily_return: HashMap<String, f64>,
    return_20d: HashMap<String, f64>,
    min_overlap_days: usize,
}

#[derive(Debug, Clone, Default)]
struct FeedAgg {
    sentiment_sum_1d: f64,
    sentiment_count_1d: f64,
    sentiment_sum_3d: f64,
    sentiment_count_3d: f64,
    sentiment_sum_7d: f64,
    sentiment_count_7d: f64,
    sentiment_sum_30d: f64,
    sentiment_count_30d: f64,
    article_count_1d: f64,
    article_count_7d: f64,
    article_count_30d: f64,
    sec_8k_7d: f64,
    form4_7d: f64,
    negative_count_7d: f64,
}

impl FeedAgg {
    // Handles add logic.
    fn add(&mut self, age_trading_days: usize, sentiment: f64, filing_type: Option<&str>) {
        if age_trading_days <= 1 {
            self.sentiment_sum_1d += sentiment;
            self.sentiment_count_1d += 1.0;
            self.article_count_1d += 1.0;
        }
        if age_trading_days <= 3 {
            self.sentiment_sum_3d += sentiment;
            self.sentiment_count_3d += 1.0;
        }
        if age_trading_days <= 7 {
            self.sentiment_sum_7d += sentiment;
            self.sentiment_count_7d += 1.0;
            self.article_count_7d += 1.0;
            if sentiment < -0.3 {
                self.negative_count_7d += 1.0;
            }
            match filing_type.unwrap_or_default() {
                "8-K" => self.sec_8k_7d += 1.0,
                "4" => self.form4_7d += 1.0,
                _ => {}
            }
        }
        if age_trading_days <= 30 {
            self.sentiment_sum_30d += sentiment;
            self.sentiment_count_30d += 1.0;
            self.article_count_30d += 1.0;
        }
    }

    // Handles avg logic.
    fn avg(sum: f64, count: f64) -> f64 {
        if count > 0.0 {
            sum / count
        } else {
            0.0
        }
    }

    // Handles sentiment 1d logic.
    fn sentiment_1d(&self) -> f64 {
        Self::avg(self.sentiment_sum_1d, self.sentiment_count_1d)
    }

    // Handles sentiment 3d logic.
    fn sentiment_3d(&self) -> f64 {
        Self::avg(self.sentiment_sum_3d, self.sentiment_count_3d)
    }

    // Handles sentiment 7d logic.
    fn sentiment_7d(&self) -> f64 {
        Self::avg(self.sentiment_sum_7d, self.sentiment_count_7d)
    }

    // Handles sentiment 30d logic.
    fn sentiment_30d(&self) -> f64 {
        Self::avg(self.sentiment_sum_30d, self.sentiment_count_30d)
    }
}

// ── Table creation ───────────────────────────────────────────────

pub fn init_ml_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ml_features (
            symbol TEXT NOT NULL,
            date TEXT NOT NULL,
            return_1d REAL, return_5d REAL, return_20d REAL, return_60d REAL,
            volatility_20d REAL, volume_ratio_5d REAL, volume_ratio_20d REAL,
            vwap_distance REAL, high_low_range REAL,
            close_to_high_20d REAL, close_to_low_20d REAL,
            rsi_14 REAL, macd_signal REAL, bb_position REAL,
            sma_cross_50_200 REAL, atr_14 REAL, obv_slope_20d REAL,
            sp500_return_1d REAL, sp500_return_5d REAL, sp500_return_20d REAL, relative_return_20d REAL,
            spy_return_1d REAL, spy_return_5d REAL, spy_return_20d REAL,
            qqq_return_1d REAL, qqq_return_5d REAL, qqq_return_20d REAL,
            relative_spy_20d REAL, relative_qqq_20d REAL,
            vix_level REAL, vix_change_1d REAL, vix_change_5d REAL, vix_change_20d REAL,
            sector_avg_return_20d REAL, relative_sector_avg_20d REAL,
            feed_sentiment_1d REAL DEFAULT 0.0, feed_sentiment_3d REAL DEFAULT 0.0,
            feed_sentiment_7d REAL DEFAULT 0.0, feed_sentiment_30d REAL DEFAULT 0.0,
            feed_article_count_1d REAL DEFAULT 0.0, feed_article_count_7d REAL DEFAULT 0.0,
            feed_article_count_30d REAL DEFAULT 0.0, feed_sec_8k_7d REAL DEFAULT 0.0,
            feed_form4_7d REAL DEFAULT 0.0, feed_negative_count_7d REAL DEFAULT 0.0,
            feed_universe_return_20d REAL DEFAULT 0.0, relative_feed_universe_20d REAL DEFAULT 0.0,
            feed_universe_corr_30d REAL DEFAULT 0.0, feed_universe_corr_90d REAL DEFAULT 0.0,
            rank_return_1d REAL, rank_volume_ratio REAL, rank_volatility REAL, rank_momentum REAL,
            PRIMARY KEY (symbol, date)
        );
        CREATE INDEX IF NOT EXISTS idx_mlf_date ON ml_features(date);

        CREATE TABLE IF NOT EXISTS ml_labels (
            symbol TEXT NOT NULL,
            date TEXT NOT NULL,
            fwd_5d REAL, fwd_10d REAL, fwd_20d REAL,
            PRIMARY KEY (symbol, date)
        );
        CREATE INDEX IF NOT EXISTS idx_mll_date ON ml_labels(date);

        CREATE TABLE IF NOT EXISTS ml_predictions (
            symbol TEXT NOT NULL,
            date TEXT NOT NULL,
            predicted_score REAL,
            predicted_quintile INTEGER,
            model_version TEXT,
            PRIMARY KEY (symbol, date)
        );
        CREATE INDEX IF NOT EXISTS idx_mlp_date ON ml_predictions(date);
        ",
    )?;
    for col in [
        "sp500_return_1d",
        "sp500_return_5d",
        "sp500_return_20d",
        "relative_return_20d",
        "spy_return_1d",
        "spy_return_5d",
        "spy_return_20d",
        "qqq_return_1d",
        "qqq_return_5d",
        "qqq_return_20d",
        "relative_spy_20d",
        "relative_qqq_20d",
        "vix_level",
        "vix_change_1d",
        "vix_change_5d",
        "vix_change_20d",
        "sector_avg_return_20d",
        "relative_sector_avg_20d",
        "feed_sentiment_1d",
        "feed_sentiment_3d",
        "feed_sentiment_7d",
        "feed_sentiment_30d",
        "feed_article_count_1d",
        "feed_article_count_7d",
        "feed_article_count_30d",
        "feed_sec_8k_7d",
        "feed_form4_7d",
        "feed_negative_count_7d",
        "feed_universe_return_20d",
        "relative_feed_universe_20d",
        "feed_universe_corr_30d",
        "feed_universe_corr_90d",
    ] {
        let ddl = if col.starts_with("feed_") {
            format!("ALTER TABLE ml_features ADD COLUMN {col} REAL DEFAULT 0.0;")
        } else {
            format!("ALTER TABLE ml_features ADD COLUMN {col} REAL;")
        };
        let _ = conn.execute_batch(&ddl);
    }
    Ok(())
}

// ── Per-symbol bar data ──────────────────────────────────────────

#[derive(Debug, Clone)]
struct Bar {
    date: String,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
    vwap: f64,
}

// ── Feature computation (single symbol) ──────────────────────────

struct FeatureRow {
    symbol: String,
    date: String,
    return_1d: Option<f64>,
    return_5d: Option<f64>,
    return_20d: Option<f64>,
    return_60d: Option<f64>,
    volatility_20d: Option<f64>,
    volume_ratio_5d: Option<f64>,
    volume_ratio_20d: Option<f64>,
    vwap_distance: Option<f64>,
    high_low_range: Option<f64>,
    close_to_high_20d: Option<f64>,
    close_to_low_20d: Option<f64>,
    rsi_14: Option<f64>,
    macd_signal: Option<f64>,
    bb_position: Option<f64>,
    sma_cross_50_200: Option<f64>,
    atr_14: Option<f64>,
    obv_slope_20d: Option<f64>,
    sp500_return_1d: Option<f64>,
    sp500_return_5d: Option<f64>,
    sp500_return_20d: Option<f64>,
    relative_return_20d: Option<f64>,
    spy_return_1d: Option<f64>,
    spy_return_5d: Option<f64>,
    spy_return_20d: Option<f64>,
    qqq_return_1d: Option<f64>,
    qqq_return_5d: Option<f64>,
    qqq_return_20d: Option<f64>,
    relative_spy_20d: Option<f64>,
    relative_qqq_20d: Option<f64>,
    vix_level: Option<f64>,
    vix_change_1d: Option<f64>,
    vix_change_5d: Option<f64>,
    vix_change_20d: Option<f64>,
    sector_avg_return_20d: Option<f64>,
    relative_sector_avg_20d: Option<f64>,
    feed_sentiment_1d: Option<f64>,
    feed_sentiment_3d: Option<f64>,
    feed_sentiment_7d: Option<f64>,
    feed_sentiment_30d: Option<f64>,
    feed_article_count_1d: Option<f64>,
    feed_article_count_7d: Option<f64>,
    feed_article_count_30d: Option<f64>,
    feed_sec_8k_7d: Option<f64>,
    feed_form4_7d: Option<f64>,
    feed_negative_count_7d: Option<f64>,
    feed_universe_return_20d: Option<f64>,
    relative_feed_universe_20d: Option<f64>,
    feed_universe_corr_30d: Option<f64>,
    feed_universe_corr_90d: Option<f64>,
}

// Computes features for symbol from prepared inputs.
fn compute_features_for_symbol(
    bars: &[Bar],
    symbol: &str,
    feed_universe: &FeedUniverseContext,
    output_dates: Option<&HashSet<String>>,
) -> Vec<FeatureRow> {
    let n = bars.len();
    if n < 2 {
        return vec![];
    }

    // Pre-compute daily returns
    let mut daily_ret = vec![0.0f64; n];
    for i in 1..n {
        if bars[i - 1].close > 0.0 {
            daily_ret[i] = bars[i].close / bars[i - 1].close - 1.0;
        }
    }
    let feed_corr_30d = rolling_feed_universe_corr(bars, &daily_ret, feed_universe, 30);
    let feed_corr_90d = rolling_feed_universe_corr(bars, &daily_ret, feed_universe, 90);

    // Pre-compute EMAs for MACD
    let ema12 = ema(&bars.iter().map(|b| b.close).collect::<Vec<_>>(), 12);
    let ema26 = ema(&bars.iter().map(|b| b.close).collect::<Vec<_>>(), 26);
    let mut macd_line = vec![0.0f64; n];
    for i in 0..n {
        macd_line[i] = ema12[i] - ema26[i];
    }
    let signal_line = ema(&macd_line, 9);

    // Pre-compute SMA 50 and 200
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let sma50 = rolling_mean(&closes, 50);
    let sma200 = rolling_mean(&closes, 200);

    // Pre-compute OBV
    let obv = compute_obv(bars);

    let mut rows = Vec::with_capacity(n);

    for i in 0..n {
        // Need at least 60 bars of history for return_60d
        // But we output rows starting from index 1 (need prev close)
        if i < 1 {
            continue;
        }
        if output_dates
            .map(|dates| !dates.contains(&bars[i].date))
            .unwrap_or(false)
        {
            continue;
        }

        let bar = &bars[i];

        // Returns
        let r1d = if bars[i - 1].close > 0.0 {
            Some(bar.close / bars[i - 1].close - 1.0)
        } else {
            None
        };
        let r5d = if i >= 5 && bars[i - 5].close > 0.0 {
            Some(bar.close / bars[i - 5].close - 1.0)
        } else {
            None
        };
        let r20d = if i >= 20 && bars[i - 20].close > 0.0 {
            Some(bar.close / bars[i - 20].close - 1.0)
        } else {
            None
        };
        let r60d = if i >= 60 && bars[i - 60].close > 0.0 {
            Some(bar.close / bars[i - 60].close - 1.0)
        } else {
            None
        };

        // Volatility 20d (std of daily returns over last 20 days)
        let vol20 = if i >= 20 {
            let slice = &daily_ret[i - 19..=i];
            Some(std_dev(slice))
        } else {
            None
        };

        // Volume ratios
        let vr5 = if i >= 5 {
            let avg: f64 = bars[i - 4..=i].iter().map(|b| b.volume).sum::<f64>() / 5.0;
            if avg > 0.0 {
                Some(bar.volume / avg)
            } else {
                None
            }
        } else {
            None
        };
        let vr20 = if i >= 20 {
            let avg: f64 = bars[i - 19..=i].iter().map(|b| b.volume).sum::<f64>() / 20.0;
            if avg > 0.0 {
                Some(bar.volume / avg)
            } else {
                None
            }
        } else {
            None
        };

        // VWAP distance
        let vwap_d = if bar.vwap > 0.0 {
            Some((bar.close - bar.vwap) / bar.vwap)
        } else {
            None
        };

        // High-low range
        let hl = if bar.close > 0.0 {
            Some((bar.high - bar.low) / bar.close)
        } else {
            None
        };

        // Close to 20d high/low
        let (c2h, c2l) = if i >= 20 {
            let h20 = bars[i - 19..=i]
                .iter()
                .map(|b| b.high)
                .fold(f64::NEG_INFINITY, f64::max);
            let l20 = bars[i - 19..=i]
                .iter()
                .map(|b| b.low)
                .fold(f64::INFINITY, f64::min);
            (
                if h20 > 0.0 {
                    Some(bar.close / h20)
                } else {
                    None
                },
                if l20 > 0.0 {
                    Some(bar.close / l20)
                } else {
                    None
                },
            )
        } else {
            (None, None)
        };

        // RSI 14
        let rsi = if i >= 14 {
            Some(compute_rsi(&daily_ret, i, 14))
        } else {
            None
        };

        // MACD signal (macd_line - signal_line)
        let macd_s = if i >= 33 {
            // 26 + 9 - 2 bars needed
            Some(macd_line[i] - signal_line[i])
        } else {
            None
        };

        // Bollinger position: (close - lower) / (upper - lower)
        let bb = if i >= 20 {
            let mean = sma(&closes[i - 19..=i]);
            let sd = std_dev(&closes[i - 19..=i]);
            let upper = mean + 2.0 * sd;
            let lower = mean - 2.0 * sd;
            let range = upper - lower;
            if range > 0.0 {
                Some((bar.close - lower) / range)
            } else {
                None
            }
        } else {
            None
        };

        // SMA cross 50/200
        let sma_x = if i >= 200 && sma200[i] > 0.0 {
            Some(sma50[i] / sma200[i])
        } else {
            None
        };

        // ATR 14
        let atr = if i >= 14 {
            Some(compute_atr(bars, i, 14))
        } else {
            None
        };

        // OBV slope 20d (linear regression slope, normalized by avg volume)
        let obv_s = if i >= 20 {
            let slice = &obv[i - 19..=i];
            let slope = linreg_slope(slice);
            let avg_vol: f64 = bars[i - 19..=i].iter().map(|b| b.volume).sum::<f64>() / 20.0;
            if avg_vol > 0.0 {
                Some(slope / avg_vol)
            } else {
                Some(0.0)
            }
        } else {
            None
        };

        rows.push(FeatureRow {
            symbol: symbol.to_string(),
            date: bar.date.clone(),
            return_1d: r1d,
            return_5d: r5d,
            return_20d: r20d,
            return_60d: r60d,
            volatility_20d: vol20,
            volume_ratio_5d: vr5,
            volume_ratio_20d: vr20,
            vwap_distance: vwap_d,
            high_low_range: hl,
            close_to_high_20d: c2h,
            close_to_low_20d: c2l,
            rsi_14: rsi,
            macd_signal: macd_s,
            bb_position: bb,
            sma_cross_50_200: sma_x,
            atr_14: atr,
            obv_slope_20d: obv_s,
            sp500_return_1d: None,
            sp500_return_5d: None,
            sp500_return_20d: None,
            relative_return_20d: None,
            spy_return_1d: None,
            spy_return_5d: None,
            spy_return_20d: None,
            qqq_return_1d: None,
            qqq_return_5d: None,
            qqq_return_20d: None,
            relative_spy_20d: None,
            relative_qqq_20d: None,
            vix_level: None,
            vix_change_1d: None,
            vix_change_5d: None,
            vix_change_20d: None,
            sector_avg_return_20d: None,
            relative_sector_avg_20d: None,
            feed_sentiment_1d: Some(0.0),
            feed_sentiment_3d: Some(0.0),
            feed_sentiment_7d: Some(0.0),
            feed_sentiment_30d: Some(0.0),
            feed_article_count_1d: Some(0.0),
            feed_article_count_7d: Some(0.0),
            feed_article_count_30d: Some(0.0),
            feed_sec_8k_7d: Some(0.0),
            feed_form4_7d: Some(0.0),
            feed_negative_count_7d: Some(0.0),
            feed_universe_return_20d: feed_universe.return_20d.get(&bar.date).copied(),
            relative_feed_universe_20d: r20d
                .zip(feed_universe.return_20d.get(&bar.date).copied())
                .map(|(stock, universe)| stock - universe),
            feed_universe_corr_30d: feed_corr_30d[i],
            feed_universe_corr_90d: feed_corr_90d[i],
        });
    }

    rows
}

// ── Helper math functions ────────────────────────────────────────

fn sma(data: &[f64]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    data.iter().sum::<f64>() / data.len() as f64
}

// Computes standard deviation for metric calculations.
fn std_dev(data: &[f64]) -> f64 {
    let n = data.len() as f64;
    if n < 2.0 {
        return 0.0;
    }
    let mean = data.iter().sum::<f64>() / n;
    let var = data.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0);
    var.sqrt()
}

// Handles rolling mean logic.
fn rolling_mean(data: &[f64], window: usize) -> Vec<f64> {
    let n = data.len();
    let mut result = vec![0.0; n];
    let mut sum = 0.0;
    for i in 0..n {
        sum += data[i];
        if i >= window {
            sum -= data[i - window];
        }
        if i >= window - 1 {
            result[i] = sum / window as f64;
        }
    }
    result
}

// Handles ema logic.
fn ema(data: &[f64], period: usize) -> Vec<f64> {
    let n = data.len();
    let mut result = vec![0.0; n];
    if n == 0 {
        return result;
    }
    let k = 2.0 / (period as f64 + 1.0);
    result[0] = data[0];
    for i in 1..n {
        result[i] = data[i] * k + result[i - 1] * (1.0 - k);
    }
    result
}

// Computes rsi from prepared inputs.
fn compute_rsi(daily_ret: &[f64], idx: usize, period: usize) -> f64 {
    if idx < period {
        return 50.0;
    }
    let mut avg_gain = 0.0;
    let mut avg_loss = 0.0;
    // First average
    for daily_return in &daily_ret[(idx - period + 1)..=idx] {
        if *daily_return > 0.0 {
            avg_gain += daily_return;
        } else {
            avg_loss += daily_return.abs();
        }
    }
    avg_gain /= period as f64;
    avg_loss /= period as f64;
    if avg_loss == 0.0 {
        return 100.0;
    }
    let rs = avg_gain / avg_loss;
    100.0 - 100.0 / (1.0 + rs)
}

// Computes atr from prepared inputs.
fn compute_atr(bars: &[Bar], idx: usize, period: usize) -> f64 {
    if idx < period {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in (idx - period + 1)..=idx {
        let tr = (bars[i].high - bars[i].low)
            .max((bars[i].high - bars[i - 1].close).abs())
            .max((bars[i].low - bars[i - 1].close).abs());
        sum += tr;
    }
    sum / period as f64
}

// Computes obv from prepared inputs.
fn compute_obv(bars: &[Bar]) -> Vec<f64> {
    let n = bars.len();
    let mut obv = vec![0.0; n];
    for i in 1..n {
        if bars[i].close > bars[i - 1].close {
            obv[i] = obv[i - 1] + bars[i].volume;
        } else if bars[i].close < bars[i - 1].close {
            obv[i] = obv[i - 1] - bars[i].volume;
        } else {
            obv[i] = obv[i - 1];
        }
    }
    obv
}

// Handles linreg slope logic.
fn linreg_slope(data: &[f64]) -> f64 {
    let n = data.len() as f64;
    if n < 2.0 {
        return 0.0;
    }
    let x_mean = (n - 1.0) / 2.0;
    let y_mean: f64 = data.iter().sum::<f64>() / n;
    let mut num = 0.0;
    let mut den = 0.0;
    for (i, &y) in data.iter().enumerate() {
        let x = i as f64;
        num += (x - x_mean) * (y - y_mean);
        den += (x - x_mean).powi(2);
    }
    if den == 0.0 {
        0.0
    } else {
        num / den
    }
}

// Computes rolling correlation between a symbol and managed feed universe.
fn rolling_feed_universe_corr(
    bars: &[Bar],
    daily_ret: &[f64],
    feed_universe: &FeedUniverseContext,
    window: usize,
) -> Vec<Option<f64>> {
    let mut out = vec![None; bars.len()];
    let min_overlap = feed_universe.min_overlap_days.min(window).max(2);
    let mut pairs: VecDeque<Option<(f64, f64)>> = VecDeque::new();
    let mut sx = 0.0;
    let mut sy = 0.0;
    let mut sxx = 0.0;
    let mut syy = 0.0;
    let mut sxy = 0.0;
    let mut count = 0usize;

    for i in 0..bars.len() {
        let pair = feed_universe
            .daily_return
            .get(&bars[i].date)
            .copied()
            .filter(|value| value.is_finite())
            .and_then(|market| {
                let stock = daily_ret[i];
                if stock.is_finite() {
                    Some((stock, market))
                } else {
                    None
                }
            });
        if let Some((x, y)) = pair {
            sx += x;
            sy += y;
            sxx += x * x;
            syy += y * y;
            sxy += x * y;
            count += 1;
        }
        pairs.push_back(pair);
        if pairs.len() > window {
            if let Some(Some((x, y))) = pairs.pop_front() {
                sx -= x;
                sy -= y;
                sxx -= x * x;
                syy -= y * y;
                sxy -= x * y;
                count = count.saturating_sub(1);
            }
        }
        if count >= min_overlap {
            let n = count as f64;
            let numerator = n * sxy - sx * sy;
            let dx = n * sxx - sx * sx;
            let dy = n * syy - sy * sy;
            let denom = (dx * dy).sqrt();
            if denom > 0.0 {
                out[i] = Some((numerator / denom).clamp(-1.0, 1.0));
            }
        }
    }

    out
}

// ── Cross-sectional rank computation ─────────────────────────────

fn compute_ranks(values: &mut [(String, f64)]) -> HashMap<String, f64> {
    values.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let n = values.len() as f64;
    let mut ranks = HashMap::new();
    for (i, (sym, _)) in values.iter().enumerate() {
        ranks.insert(sym.clone(), (i as f64 + 1.0) / n);
    }
    ranks
}

// Handles apply market context logic.
fn apply_market_context(row: &mut FeatureRow, context: &MarketContext) {
    if let Some((r1, r5, r20)) = context.sp500.get(&row.date) {
        row.sp500_return_1d = *r1;
        row.sp500_return_5d = *r5;
        row.sp500_return_20d = *r20;
        row.relative_return_20d = row.return_20d.zip(*r20).map(|(stock, bench)| stock - bench);
    }
    if let Some((r1, r5, r20)) = context.spy.get(&row.date) {
        row.spy_return_1d = *r1;
        row.spy_return_5d = *r5;
        row.spy_return_20d = *r20;
        row.relative_spy_20d = row.return_20d.zip(*r20).map(|(stock, bench)| stock - bench);
    }
    if let Some((r1, r5, r20)) = context.qqq.get(&row.date) {
        row.qqq_return_1d = *r1;
        row.qqq_return_5d = *r5;
        row.qqq_return_20d = *r20;
        row.relative_qqq_20d = row.return_20d.zip(*r20).map(|(stock, bench)| stock - bench);
    }
    if let Some((level, ch1, ch5, ch20)) = context.vix.get(&row.date) {
        row.vix_level = *level;
        row.vix_change_1d = *ch1;
        row.vix_change_5d = *ch5;
        row.vix_change_20d = *ch20;
    }
    if let Some(sector_avg) = context.sector_avg_20d.get(&row.date) {
        row.sector_avg_return_20d = *sector_avg;
        row.relative_sector_avg_20d = row
            .return_20d
            .zip(*sector_avg)
            .map(|(stock, sector)| stock - sector);
    }
    if let Some(feed) = context
        .feeds
        .get(&row.symbol)
        .and_then(|by_date| by_date.get(&row.date))
    {
        row.feed_sentiment_1d = Some(feed.sentiment_1d());
        row.feed_sentiment_3d = Some(feed.sentiment_3d());
        row.feed_sentiment_7d = Some(feed.sentiment_7d());
        row.feed_sentiment_30d = Some(feed.sentiment_30d());
        row.feed_article_count_1d = Some(feed.article_count_1d);
        row.feed_article_count_7d = Some(feed.article_count_7d);
        row.feed_article_count_30d = Some(feed.article_count_30d);
        row.feed_sec_8k_7d = Some(feed.sec_8k_7d);
        row.feed_form4_7d = Some(feed.form4_7d);
        row.feed_negative_count_7d = Some(feed.negative_count_7d);
    }
}

// ── CMD: ml features ─────────────────────────────────────────────

pub fn cmd_ml_features(
    symbol_filter: Option<String>,
    force: bool,
    json: bool,
) -> anyhow::Result<()> {
    let conn = open_ml_db()?;
    init_ml_tables(&conn)?;

    // Get unique dates to process
    let existing_dates: std::collections::HashSet<String> = if symbol_filter.is_none() && !force {
        let mut stmt = conn.prepare("SELECT DISTINCT date FROM ml_features")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.filter_map(|r| r.ok()).collect()
    } else {
        std::collections::HashSet::new()
    };

    // Get all trading dates
    let all_dates: Vec<String> = {
        let mut stmt = conn.prepare("SELECT DISTINCT date FROM bars ORDER BY date")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    let dates_to_process: Vec<String> = if symbol_filter.is_none() && !force {
        let mut dates = all_dates
            .iter()
            .skip(1)
            .filter(|d| !existing_dates.contains(*d))
            .cloned()
            .collect::<Vec<_>>();
        if let Some(latest_date) = all_dates.last() {
            if all_dates.len() > 1 && !dates.contains(latest_date) {
                dates.push(latest_date.clone());
            }
        }
        if let Some(latest_existing_feature_date) = existing_dates.iter().max() {
            if !dates.contains(latest_existing_feature_date) {
                dates.push(latest_existing_feature_date.clone());
            }
        }
        dates
    } else {
        all_dates.to_vec()
    };

    if dates_to_process.is_empty() && symbol_filter.is_none() && !force {
        if json {
            println!(
                "{{\"status\":\"up_to_date\",\"total_dates\":{}}}",
                all_dates.len()
            );
        } else {
            println!("✅ Features already up to date ({} dates)", all_dates.len());
        }
        return Ok(());
    }
    let process_dates: std::collections::HashSet<String> = dates_to_process.into_iter().collect();
    let process_start = process_dates
        .iter()
        .min()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no ML feature dates selected"))?;
    let process_end = process_dates
        .iter()
        .max()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no ML feature dates selected"))?;
    let full_history =
        force || symbol_filter.is_some() || process_dates.len() > all_dates.len().saturating_div(2);

    // Get symbols
    let symbols: Vec<String> = if let Some(ref sym) = symbol_filter {
        vec![sym.to_uppercase()]
    } else {
        let mut stmt = conn.prepare("SELECT DISTINCT symbol FROM bars ORDER BY symbol")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    let total_symbols = symbols.len();
    eprintln!(
        "Computing features for {} symbols ({} dates, {}, {} CPU workers)...",
        total_symbols,
        process_dates.len(),
        if full_history {
            "full history"
        } else {
            "bounded incremental history"
        },
        config::cpu_worker_threads()
    );
    let progress = crate::progress::bar_if(!json, total_symbols as u64, "Computing ML features");
    let market_context = load_market_context(&conn)?;

    // Process in batches to manage memory
    let batch_size = config::ml_symbol_batch_size();
    let mut total_rows = 0u64;

    for (batch_idx, sym_batch) in symbols.chunks(batch_size).enumerate() {
        let worker_count = config::cpu_worker_threads().min(sym_batch.len()).max(1);
        let worker_chunk_size = sym_batch.len().div_ceil(worker_count).max(1);
        let worker_results = sym_batch
            .par_chunks(worker_chunk_size)
            .map(|worker_symbols| -> anyhow::Result<Vec<FeatureRow>> {
                let read_conn = open_ml_read_db()?;
                let mut rows = Vec::new();
                for sym in worker_symbols {
                    let bars = if full_history {
                        load_bars(&read_conn, sym)?
                    } else {
                        load_bars_window(
                            &read_conn,
                            sym,
                            &process_start,
                            &process_end,
                            INCREMENTAL_FEATURE_WARMUP_BARS,
                        )?
                    };
                    if bars.len() < 61 {
                        continue;
                    }
                    let mut symbol_rows = compute_features_for_symbol(
                        &bars,
                        sym,
                        &market_context.feed_universe,
                        Some(&process_dates),
                    );
                    for row in &mut symbol_rows {
                        apply_market_context(row, &market_context);
                    }
                    rows.extend(symbol_rows);
                }
                Ok(rows)
            })
            .collect::<Vec<_>>();
        let mut computed_rows = Vec::new();
        for result in worker_results {
            computed_rows.extend(result?);
        }

        let tx = conn.unchecked_transaction()?;
        let mut batch_rows = 0u64;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO ml_features (
                    symbol, date, return_1d, return_5d, return_20d, return_60d,
                    volatility_20d, volume_ratio_5d, volume_ratio_20d,
                    vwap_distance, high_low_range,
                    close_to_high_20d, close_to_low_20d,
                    rsi_14, macd_signal, bb_position,
                    sma_cross_50_200, atr_14, obv_slope_20d,
                    sp500_return_1d, sp500_return_5d, sp500_return_20d, relative_return_20d,
                    spy_return_1d, spy_return_5d, spy_return_20d,
                    qqq_return_1d, qqq_return_5d, qqq_return_20d,
                    relative_spy_20d, relative_qqq_20d,
                    vix_level, vix_change_1d, vix_change_5d, vix_change_20d,
                    sector_avg_return_20d, relative_sector_avg_20d,
                    feed_sentiment_1d, feed_sentiment_3d, feed_sentiment_7d, feed_sentiment_30d,
                    feed_article_count_1d, feed_article_count_7d, feed_article_count_30d,
                    feed_sec_8k_7d, feed_form4_7d, feed_negative_count_7d,
                    feed_universe_return_20d, relative_feed_universe_20d,
                    feed_universe_corr_30d, feed_universe_corr_90d,
                    rank_return_1d, rank_volume_ratio, rank_volatility, rank_momentum
                ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31,?32,?33,?34,?35,?36,?37,?38,?39,?40,?41,?42,?43,?44,?45,?46,?47,?48,?49,?50,?51,?52,?53,?54,?55)"
            )?;

            for row in computed_rows {
                stmt.execute(params![
                    row.symbol,
                    row.date,
                    row.return_1d,
                    row.return_5d,
                    row.return_20d,
                    row.return_60d,
                    row.volatility_20d,
                    row.volume_ratio_5d,
                    row.volume_ratio_20d,
                    row.vwap_distance,
                    row.high_low_range,
                    row.close_to_high_20d,
                    row.close_to_low_20d,
                    row.rsi_14,
                    row.macd_signal,
                    row.bb_position,
                    row.sma_cross_50_200,
                    row.atr_14,
                    row.obv_slope_20d,
                    row.sp500_return_1d,
                    row.sp500_return_5d,
                    row.sp500_return_20d,
                    row.relative_return_20d,
                    row.spy_return_1d,
                    row.spy_return_5d,
                    row.spy_return_20d,
                    row.qqq_return_1d,
                    row.qqq_return_5d,
                    row.qqq_return_20d,
                    row.relative_spy_20d,
                    row.relative_qqq_20d,
                    row.vix_level,
                    row.vix_change_1d,
                    row.vix_change_5d,
                    row.vix_change_20d,
                    row.sector_avg_return_20d,
                    row.relative_sector_avg_20d,
                    row.feed_sentiment_1d,
                    row.feed_sentiment_3d,
                    row.feed_sentiment_7d,
                    row.feed_sentiment_30d,
                    row.feed_article_count_1d,
                    row.feed_article_count_7d,
                    row.feed_article_count_30d,
                    row.feed_sec_8k_7d,
                    row.feed_form4_7d,
                    row.feed_negative_count_7d,
                    row.feed_universe_return_20d,
                    row.relative_feed_universe_20d,
                    row.feed_universe_corr_30d,
                    row.feed_universe_corr_90d,
                    Option::<f64>::None,
                    Option::<f64>::None,
                    Option::<f64>::None,
                    Option::<f64>::None,
                ])?;
                batch_rows += 1;
            }
        }
        tx.commit()?;
        total_rows += batch_rows;

        let done = ((batch_idx + 1) * batch_size).min(total_symbols);
        progress.set_position(done as u64);
        progress.set_message(format!("{total_rows} feature rows"));
    }
    progress.finish_and_clear();

    // Now compute cross-sectional ranks per date
    if total_rows > 0 {
        eprintln!("Computing cross-sectional ranks...");
        compute_ranks_for_dates(&conn, Some(&process_dates), !json)?;
    }

    if json {
        println!(
            "{{\"status\":\"done\",\"symbols\":{},\"rows\":{}}}",
            total_symbols, total_rows
        );
    } else {
        println!(
            "✅ Features computed: {} symbols, {} rows",
            total_symbols, total_rows
        );
    }

    Ok(())
}

// Computes ranks for dates from prepared inputs.
fn compute_ranks_for_dates(
    conn: &Connection,
    date_filter: Option<&std::collections::HashSet<String>>,
    show_progress: bool,
) -> anyhow::Result<()> {
    let dates: Vec<String> = {
        let mut stmt = conn.prepare("SELECT DISTINCT date FROM ml_features ORDER BY date")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.filter_map(|r| r.ok())
            .filter(|date| {
                date_filter
                    .map(|filter| filter.contains(date))
                    .unwrap_or(true)
            })
            .collect()
    };

    let total_dates = dates.len();
    let progress = crate::progress::bar_if(
        show_progress,
        total_dates as u64,
        "Computing cross-sectional ranks",
    );
    let tx = conn.unchecked_transaction()?;

    for (di, date) in dates.iter().enumerate() {
        // Load feature values for ranking
        let mut stmt = tx.prepare_cached(
            "SELECT symbol, return_1d, volume_ratio_5d, volatility_20d, return_20d
             FROM ml_features WHERE date = ?1 AND return_1d IS NOT NULL",
        )?;
        let mut ret_vals: Vec<(String, f64)> = Vec::new();
        let mut vol_vals: Vec<(String, f64)> = Vec::new();
        let mut volat_vals: Vec<(String, f64)> = Vec::new();
        let mut mom_vals: Vec<(String, f64)> = Vec::new();

        let rows = stmt.query_map(params![date], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<f64>>(1)?,
                r.get::<_, Option<f64>>(2)?,
                r.get::<_, Option<f64>>(3)?,
                r.get::<_, Option<f64>>(4)?,
            ))
        })?;

        for row in rows {
            let (sym, r1d, vr, vol, mom) = row?;
            if let Some(v) = r1d {
                ret_vals.push((sym.clone(), v));
            }
            if let Some(v) = vr {
                vol_vals.push((sym.clone(), v));
            }
            if let Some(v) = vol {
                volat_vals.push((sym.clone(), v));
            }
            if let Some(v) = mom {
                mom_vals.push((sym.clone(), v));
            }
        }

        let ret_ranks = compute_ranks(&mut ret_vals);
        let vol_ranks = compute_ranks(&mut vol_vals);
        let volat_ranks = compute_ranks(&mut volat_vals);
        let mom_ranks = compute_ranks(&mut mom_vals);

        // Update ranks
        let mut upd = tx.prepare_cached(
            "UPDATE ml_features SET rank_return_1d=?1, rank_volume_ratio=?2, rank_volatility=?3, rank_momentum=?4
             WHERE symbol=?5 AND date=?6"
        )?;

        // Gather all symbols for this date
        let all_syms: std::collections::HashSet<&String> = ret_ranks
            .keys()
            .chain(vol_ranks.keys())
            .chain(volat_ranks.keys())
            .chain(mom_ranks.keys())
            .collect();

        for sym in all_syms {
            upd.execute(params![
                ret_ranks.get(sym),
                vol_ranks.get(sym),
                volat_ranks.get(sym),
                mom_ranks.get(sym),
                sym,
                date,
            ])?;
        }

        progress.set_position((di + 1) as u64);
        if (di + 1) % 25 == 0 || di + 1 == total_dates {
            progress.set_message(format!("{}/{} dates", di + 1, total_dates));
        }
    }
    tx.commit()?;
    progress.finish_and_clear();
    Ok(())
}

// ── CMD: ml labels ───────────────────────────────────────────────

// Upserts labels from each symbol's actual future observations, not global market dates.
fn upsert_labels_for_dates(conn: &Connection, dates: &[String]) -> anyhow::Result<u64> {
    let tx = conn.unchecked_transaction()?;
    let mut total_rows = 0u64;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO ml_labels(symbol, date, fwd_5d, fwd_10d, fwd_20d)
             SELECT base.symbol,
                    base.date,
                    (SELECT future.close/base.close-1.0
                     FROM bars future
                     WHERE future.symbol=base.symbol AND future.date>base.date
                     ORDER BY future.date LIMIT 1 OFFSET 4),
                    (SELECT future.close/base.close-1.0
                     FROM bars future
                     WHERE future.symbol=base.symbol AND future.date>base.date
                     ORDER BY future.date LIMIT 1 OFFSET 9),
                    (SELECT future.close/base.close-1.0
                     FROM bars future
                     WHERE future.symbol=base.symbol AND future.date>base.date
                     ORDER BY future.date LIMIT 1 OFFSET 19)
             FROM bars base
             WHERE base.date=?1 AND base.close>0
             ON CONFLICT(symbol, date) DO UPDATE SET
                 fwd_5d=excluded.fwd_5d,
                 fwd_10d=excluded.fwd_10d,
                 fwd_20d=excluded.fwd_20d",
        )?;
        for date in dates {
            total_rows += stmt.execute(params![date])? as u64;
        }
    }
    tx.commit()?;
    Ok(total_rows)
}

pub fn cmd_ml_labels(horizon: u32, json: bool) -> anyhow::Result<()> {
    let conn = open_ml_db()?;
    init_ml_tables(&conn)?;

    if !matches!(horizon, 5 | 10 | 20) {
        anyhow::bail!("Unsupported label horizon {}. Use 5, 10, or 20.", horizon);
    }

    eprintln!(
        "Computing forward return labels (5d/10d/20d columns; requested horizon {}d)...",
        horizon
    );

    let all_dates: Vec<String> = {
        let mut stmt = conn.prepare("SELECT DISTINCT date FROM bars ORDER BY date")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };
    let eligible_len = all_dates.len().saturating_sub(horizon as usize);
    let eligible_ordered: Vec<String> = all_dates.iter().take(eligible_len).cloned().collect();
    let eligible_dates: std::collections::HashSet<String> =
        eligible_ordered.iter().cloned().collect();
    let existing_dates: std::collections::HashSet<String> = {
        let mut stmt = conn.prepare("SELECT DISTINCT date FROM ml_labels")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };
    let mut dates_to_process: std::collections::HashSet<String> = eligible_dates
        .difference(&existing_dates)
        .cloned()
        .collect();
    let recent_recompute = std::cmp::max(20, horizon as usize);
    for date in eligible_ordered.iter().rev().take(recent_recompute) {
        dates_to_process.insert(date.clone());
    }

    if dates_to_process.is_empty() {
        if json {
            println!(
                "{{\"status\":\"up_to_date\",\"eligible_dates\":{}}}",
                eligible_dates.len()
            );
        } else {
            println!(
                "✅ Labels already up to date ({} eligible dates)",
                eligible_dates.len()
            );
        }
        return Ok(());
    }

    let ordered_process_dates = eligible_ordered
        .iter()
        .filter(|date| dates_to_process.contains(*date))
        .cloned()
        .collect::<Vec<_>>();
    let progress = crate::progress::bar_if(
        !json,
        ordered_process_dates.len() as u64,
        "Computing ML labels",
    );
    let total_rows = upsert_labels_for_dates(&conn, &ordered_process_dates)?;
    progress.set_position(ordered_process_dates.len() as u64);
    progress.set_message(format!("{total_rows} label rows"));
    progress.finish_and_clear();

    if json {
        println!("{{\"status\":\"done\",\"rows\":{}}}", total_rows);
    } else {
        println!(
            "✅ Labels computed: {} rows across {} dates",
            total_rows,
            ordered_process_dates.len()
        );
    }

    Ok(())
}

// ── CMD: ml export ───────────────────────────────────────────────

pub fn cmd_ml_export(format: String, json: bool) -> anyhow::Result<()> {
    let conn = open_ml_db()?;

    let out_path = if format == "csv" {
        paths::ml_dataset_csv_path().to_string_lossy().to_string()
    } else {
        anyhow::bail!("Only csv format supported currently");
    };

    eprintln!("Exporting features + labels to {}...", out_path);
    let progress = crate::progress::spinner_if(!json, "Exporting ML dataset");

    let feature_cols = FEATURE_COLS
        .iter()
        .map(|col| format!("f.{col}"))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        "SELECT f.symbol, f.date,
                {feature_cols},
                l.fwd_5d, l.fwd_10d, l.fwd_20d
         FROM ml_features f
         LEFT JOIN ml_labels l ON f.symbol = l.symbol AND f.date = l.date
         WHERE f.return_1d IS NOT NULL
         ORDER BY f.date, f.symbol"
    );
    let mut stmt = conn.prepare(&query)?;

    let mut wtr =
        std::io::BufWriter::new(paths::create_private_file(std::path::Path::new(&out_path))?);
    use std::io::Write;

    // Header
    writeln!(
        wtr,
        "symbol,date,{},fwd_5d,fwd_10d,fwd_20d",
        FEATURE_COLS.join(",")
    )?;

    let mut count = 0u64;
    let rows = stmt.query_map([], |r| {
        let mut vals = Vec::with_capacity(FEATURE_COLS.len() + 5);
        vals.push(r.get::<_, String>(0)?); // symbol
        vals.push(r.get::<_, String>(1)?); // date
        for i in 2..(FEATURE_COLS.len() + 5) {
            let v: Option<f64> = r.get(i)?;
            vals.push(v.map(|x| format!("{:.6}", x)).unwrap_or_default());
        }
        Ok(vals)
    })?;

    for row in rows {
        let vals = row?;
        writeln!(wtr, "{}", vals.join(","))?;
        count += 1;
        if count.is_multiple_of(10_000) {
            progress.set_message(format!("{count} rows written"));
        }
    }

    wtr.flush()?;
    progress.finish_and_clear();

    if json {
        println!(
            "{{\"status\":\"done\",\"path\":\"{}\",\"rows\":{}}}",
            out_path, count
        );
    } else {
        println!("✅ Exported {} rows to {}", count, out_path);
    }

    Ok(())
}

// Builds LightGBM params configuration.
fn lgb_params(
    num_iterations: i64,
    early_stopping_rounds: Option<i64>,
    backend: LgbBackend,
) -> serde_json::Value {
    let mut params = json!({
        "objective": "regression",
        "metric": "l2",
        "boosting_type": "gbdt",
        "num_iterations": num_iterations,
        "num_leaves": 63,
        "learning_rate": 0.05,
        "feature_fraction": 0.7,
        "bagging_fraction": 0.7,
        "bagging_freq": 5,
        "min_child_samples": 100,
        "lambda_l1": 0.1,
        "lambda_l2": 1.0,
        "max_depth": 6,
        "num_threads": config::cpu_worker_threads() as i64,
        "verbose": -1,
        "seed": 42,
    });
    match backend {
        LgbBackend::Cpu | LgbBackend::Auto => {
            params["device_type"] = json!("cpu");
        }
        LgbBackend::Cuda => {
            params["device_type"] = json!("cuda");
        }
    }
    if let Some(rounds) = early_stopping_rounds {
        params["early_stopping_rounds"] = json!(rounds);
    }
    params
}

// Handles unique sorted dates logic.
fn unique_sorted_dates(dates: &[String]) -> Vec<String> {
    let mut out = dates.to_vec();
    out.sort();
    out.dedup();
    out
}

#[derive(Debug, Clone)]
struct LgbFiles {
    train_path: std::path::PathBuf,
    valid_path: std::path::PathBuf,
    train_rows: usize,
    valid_rows: usize,
    train_candidate_rows: usize,
    valid_candidate_rows: usize,
    train_stride: usize,
    valid_stride: usize,
    valid_start: String,
    valid_end: Option<String>,
    date_start: String,
    date_end: String,
    unique_dates: usize,
}

static LGB_DATASET_CACHE: OnceLock<Mutex<HashMap<String, LgbFiles>>> = OnceLock::new();

fn lgb_dataset_cache() -> &'static Mutex<HashMap<String, LgbFiles>> {
    LGB_DATASET_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lgb_dataset_cache_key(
    feature_cols: &[&str],
    train_path: &Path,
    valid_path: &Path,
    valid_start: &str,
    valid_end: Option<&str>,
) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}",
        feature_cols.join(","),
        train_path.display(),
        valid_path.display(),
        valid_start,
        valid_end.unwrap_or_default(),
        config::lightgbm_max_train_rows(),
        config::lightgbm_max_valid_rows()
    )
}

// Writes lgb training files to disk or storage.
fn write_lgb_training_files(conn: &Connection, show_progress: bool) -> anyhow::Result<LgbFiles> {
    write_lgb_training_files_for_cols(
        conn,
        FEATURE_COLS,
        paths::lightgbm_training_dataset_path(),
        paths::lightgbm_validation_dataset_path(),
        show_progress,
    )
}

// Writes lgb training files for cols to disk or storage.
fn write_lgb_training_files_for_cols(
    conn: &Connection,
    feature_cols_in: &[&str],
    train_path: std::path::PathBuf,
    valid_path: std::path::PathBuf,
    show_progress: bool,
) -> anyhow::Result<LgbFiles> {
    let raw_dates = {
        let mut stmt = conn.prepare("SELECT DISTINCT date FROM ml_features ORDER BY date")?;
        let dates = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect::<Vec<_>>();
        dates
    };
    let unique_dates = unique_sorted_dates(&raw_dates);
    let valid_start_idx = ((unique_dates.len() as f64) * 0.9).floor() as usize;
    let valid_start = unique_dates[valid_start_idx.min(unique_dates.len() - 1)].clone();
    write_lgb_training_files_for_cols_and_dates(
        conn,
        feature_cols_in,
        train_path,
        valid_path,
        &valid_start,
        None,
        show_progress,
    )
}

// Handles count lgb candidate rows logic.
fn count_lgb_candidate_rows(
    conn: &Connection,
    valid_start: &str,
    valid_end: Option<&str>,
) -> anyhow::Result<(usize, usize)> {
    let eligible_q = ml_eligible_asset_predicate("b.symbol", "a");
    let valid_case = if valid_end.is_some() {
        "f.date >= ?1 AND f.date < ?2"
    } else {
        "f.date >= ?1"
    };
    let query = format!(
        "SELECT
            COALESCE(SUM(CASE WHEN f.date < ?1 THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN {valid_case} THEN 1 ELSE 0 END), 0)
         FROM ml_features f
         INNER JOIN ml_labels l ON f.symbol = l.symbol AND f.date = l.date
         INNER JOIN (
             SELECT b.symbol FROM bars b
             LEFT JOIN assets a ON a.symbol = b.symbol
             WHERE {eligible_q}
             GROUP BY b.symbol
             HAVING COUNT(*) >= 200 AND AVG(volume) > 500000
         ) q ON f.symbol = q.symbol
         WHERE f.return_1d IS NOT NULL
           AND f.volatility_20d IS NOT NULL
           AND l.fwd_5d IS NOT NULL"
    );
    let (train, valid): (i64, i64) = if let Some(valid_end) = valid_end {
        conn.query_row(&query, params![valid_start, valid_end], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
    } else {
        conn.query_row(&query, params![valid_start], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
    };
    Ok((train.max(0) as usize, valid.max(0) as usize))
}

// Handles row stride for limit logic.
fn row_stride_for_limit(total: usize, max_rows: usize) -> usize {
    if max_rows == 0 || total <= max_rows {
        1
    } else {
        total.div_ceil(max_rows).max(1)
    }
}

// Writes lgb training files for cols and dates to disk or storage.
fn write_lgb_training_files_for_cols_and_dates(
    conn: &Connection,
    feature_cols_in: &[&str],
    train_path: std::path::PathBuf,
    valid_path: std::path::PathBuf,
    valid_start: &str,
    valid_end: Option<&str>,
    show_progress: bool,
) -> anyhow::Result<LgbFiles> {
    let _ = paths::ensure_state_dir()?;
    let cache_key = lgb_dataset_cache_key(
        feature_cols_in,
        &train_path,
        &valid_path,
        valid_start,
        valid_end,
    );
    if let Some(files) = lgb_dataset_cache()
        .lock()
        .ok()
        .and_then(|cache| cache.get(&cache_key).cloned())
        .filter(|files| files.train_path.is_file() && files.valid_path.is_file())
    {
        if show_progress {
            eprintln!(
                "  Reusing streamed dataset: {} train rows, {} validation rows",
                files.train_rows, files.valid_rows
            );
        }
        return Ok(files);
    }

    let raw_dates = {
        let mut stmt = conn.prepare("SELECT DISTINCT date FROM ml_features ORDER BY date")?;
        let dates = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect::<Vec<_>>();
        dates
    };
    let unique_dates = unique_sorted_dates(&raw_dates);
    if unique_dates.len() < 30 {
        anyhow::bail!(
            "Not enough feature dates for training: {}",
            unique_dates.len()
        );
    }

    let feature_cols = feature_cols_in
        .iter()
        .map(|col| format!("f.{col}"))
        .collect::<Vec<_>>()
        .join(", ");
    let eligible_q = ml_eligible_asset_predicate("b.symbol", "a");
    let query = format!(
        "SELECT f.symbol, f.date, {feature_cols}, l.fwd_5d
         FROM ml_features f
         INNER JOIN ml_labels l ON f.symbol = l.symbol AND f.date = l.date
         INNER JOIN (
             SELECT b.symbol FROM bars b
             LEFT JOIN assets a ON a.symbol = b.symbol
             WHERE {eligible_q}
             GROUP BY b.symbol
             HAVING COUNT(*) >= 200 AND AVG(volume) > 500000
         ) q ON f.symbol = q.symbol
         WHERE f.return_1d IS NOT NULL
           AND f.volatility_20d IS NOT NULL
           AND l.fwd_5d IS NOT NULL
         ORDER BY f.date, f.symbol"
    );

    let (train_candidate_rows, valid_candidate_rows) =
        count_lgb_candidate_rows(conn, valid_start, valid_end)?;
    let train_stride =
        row_stride_for_limit(train_candidate_rows, config::lightgbm_max_train_rows());
    let valid_stride =
        row_stride_for_limit(valid_candidate_rows, config::lightgbm_max_valid_rows());

    let mut train = std::io::BufWriter::new(paths::create_private_file(&train_path)?);
    let mut valid = std::io::BufWriter::new(paths::create_private_file(&valid_path)?);
    let mut stmt = conn.prepare(&query)?;
    let mut rows = stmt.query([])?;
    let mut train_rows = 0usize;
    let mut valid_rows = 0usize;
    let mut seen_rows = 0usize;
    let mut train_seen = 0usize;
    let mut valid_seen = 0usize;
    let mut date_start = String::new();
    let mut date_end = String::new();
    let progress = crate::progress::spinner_if(
        show_progress,
        "Streaming LightGBM training rows from SQLite",
    );

    while let Some(row) = rows.next()? {
        seen_rows += 1;
        let date: String = row.get(1)?;
        let Some(target) = row
            .get::<_, Option<f64>>(feature_cols_in.len() + 2)?
            .filter(|v| v.is_finite())
        else {
            continue;
        };

        let mut line = String::new();
        line.push_str(&format!("{target:.10}"));
        for i in 0..feature_cols_in.len() {
            if let Some(value) = row.get::<_, Option<f64>>(i + 2)?.filter(|v| v.is_finite()) {
                if value != 0.0 {
                    line.push_str(&format!(" {i}:{value:.10}"));
                }
            }
        }
        line.push('\n');

        if date_start.is_empty() {
            date_start = date.clone();
        }
        date_end = date.clone();

        let is_valid = date.as_str() >= valid_start
            && valid_end.map(|end| date.as_str() < end).unwrap_or(true);
        let is_train = date.as_str() < valid_start;

        if is_valid {
            valid_seen += 1;
            if (valid_seen - 1).is_multiple_of(valid_stride) {
                valid.write_all(line.as_bytes())?;
                valid_rows += 1;
            }
        } else if is_train {
            train_seen += 1;
            if (train_seen - 1).is_multiple_of(train_stride) {
                train.write_all(line.as_bytes())?;
                train_rows += 1;
            }
        }
        if seen_rows.is_multiple_of(25_000) {
            progress.set_message(format!(
                "{seen_rows} rows scanned, {train_rows} train, {valid_rows} valid"
            ));
        }
    }
    train.flush()?;
    valid.flush()?;
    progress.finish_and_clear();

    if train_rows < 10_000 {
        anyhow::bail!("Not enough LightGBM training rows: {}", train_rows);
    }
    if valid_rows < 100 {
        anyhow::bail!("Not enough LightGBM validation rows: {}", valid_rows);
    }

    let files = LgbFiles {
        train_path,
        valid_path,
        train_rows,
        valid_rows,
        train_candidate_rows,
        valid_candidate_rows,
        train_stride,
        valid_stride,
        valid_start: valid_start.to_string(),
        valid_end: valid_end.map(|value| value.to_string()),
        date_start,
        date_end,
        unique_dates: unique_dates.len(),
    };
    if let Ok(mut cache) = lgb_dataset_cache().lock() {
        cache.insert(cache_key, files.clone());
    }
    Ok(files)
}

#[derive(Debug, Clone)]
pub struct ScoredReturn {
    pub symbol: String,
    pub date: String,
    pub score: f64,
    pub fwd_return: f64,
}

// Computes the arithmetic mean for metric calculations.
fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

// Computes standard deviation for metric calculations.
fn stddev(values: &[f64], avg: f64) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let var = values
        .iter()
        .map(|value| {
            let d = value - avg;
            d * d
        })
        .sum::<f64>()
        / (values.len() - 1) as f64;
    var.sqrt()
}

// Handles trading metrics json calculations.
pub fn trading_metrics_json(
    model: &str,
    rows: &[ScoredReturn],
    top_n: usize,
    round_trip_slippage_bps: f64,
) -> serde_json::Value {
    let mut by_date: std::collections::BTreeMap<&str, Vec<&ScoredReturn>> =
        std::collections::BTreeMap::new();
    for row in rows {
        if row.score.is_finite() && row.fwd_return.is_finite() {
            by_date.entry(row.date.as_str()).or_default().push(row);
        }
    }

    let slippage = round_trip_slippage_bps / 10_000.0;
    let mut gross_daily = Vec::new();
    let mut net_daily = Vec::new();
    let mut selected_trade_returns = Vec::new();
    let mut selected_symbols = std::collections::BTreeSet::new();
    let mut selected = 0usize;

    for (_date, mut day_rows) in by_date {
        day_rows.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let day_top = day_rows.into_iter().take(top_n).collect::<Vec<_>>();
        if day_top.is_empty() {
            continue;
        }
        let gross = day_top.iter().map(|row| row.fwd_return).sum::<f64>() / day_top.len() as f64;
        let net = gross - slippage;
        gross_daily.push(gross);
        net_daily.push(net);
        selected += day_top.len();
        selected_symbols.extend(day_top.iter().map(|row| row.symbol.as_str()));
        selected_trade_returns.extend(day_top.iter().map(|row| row.fwd_return - slippage));
    }

    let avg_gross = mean(&gross_daily);
    let avg_net = mean(&net_daily);
    let net_std = stddev(&net_daily, avg_net);
    let periods_per_year: f64 = 252.0 / 5.0;
    let sharpe = if net_std > 0.0 {
        avg_net / net_std * periods_per_year.sqrt()
    } else {
        0.0
    };
    let mut equity = 1.0;
    let mut peak = 1.0;
    let mut max_drawdown = 0.0;
    for ret in &net_daily {
        equity *= 1.0 + ret;
        if equity > peak {
            peak = equity;
        }
        if peak > 0.0 {
            let drawdown = equity / peak - 1.0;
            if drawdown < max_drawdown {
                max_drawdown = drawdown;
            }
        }
    }
    let years = if net_daily.is_empty() {
        0.0
    } else {
        net_daily.len() as f64 / periods_per_year
    };
    let annualized_return = if years > 0.0 && equity > 0.0 {
        equity.powf(1.0 / years) - 1.0
    } else {
        0.0
    };
    let win_rate = if selected_trade_returns.is_empty() {
        0.0
    } else {
        selected_trade_returns
            .iter()
            .filter(|value| **value > 0.0)
            .count() as f64
            / selected_trade_returns.len() as f64
    };

    serde_json::json!({
        "model": model,
        "target": "fwd_5d_return",
        "selection": "long_top_n_by_score_each_validation_date",
        "top_n_per_date": top_n,
        "round_trip_slippage_bps": round_trip_slippage_bps,
        "execution_assumption": "long entries pay offer-or-worse and exits receive bid-or-worse; fixed round-trip bps approximates spread plus slippage for historical validation where historical NBBO is not stored",
        "validation_dates": net_daily.len(),
        "selected_trades": selected,
        "selected_unique_symbols": selected_symbols.len(),
        "avg_gross_5d_return": avg_gross,
        "avg_net_5d_return": avg_net,
        "trade_win_rate_after_slippage": win_rate,
        "cumulative_net_return": equity - 1.0,
        "annualized_net_return": annualized_return,
        "sharpe_5d_bucket": sharpe,
        "max_drawdown": max_drawdown,
        "annualization_periods_per_year": periods_per_year,
        "note": "Returns use realized fwd_5d labels. Daily validation buckets overlap, and historical NBBO is not stored for these rows, so annualized metrics are useful for ordering models but are not a standalone tradable portfolio backtest."
    })
}

// Loads validation meta rows from storage or configuration.
fn load_validation_meta_rows(
    conn: &Connection,
    valid_start: &str,
    valid_end: Option<&str>,
) -> anyhow::Result<Vec<(String, String, f64)>> {
    let eligible_q = ml_eligible_asset_predicate("b.symbol", "a");
    let query = format!(
        "
         SELECT f.symbol, f.date, l.fwd_5d
         FROM ml_features f
         INNER JOIN ml_labels l ON f.symbol = l.symbol AND f.date = l.date
         INNER JOIN (
             SELECT b.symbol FROM bars b
             LEFT JOIN assets a ON a.symbol = b.symbol
             WHERE {eligible_q}
             GROUP BY b.symbol
             HAVING COUNT(*) >= 200 AND AVG(volume) > 500000
         ) q ON f.symbol = q.symbol
         WHERE f.return_1d IS NOT NULL
           AND f.volatility_20d IS NOT NULL
           AND l.fwd_5d IS NOT NULL
           AND f.date >= ?1
           AND (?2 IS NULL OR f.date < ?2)
         ORDER BY f.date, f.symbol"
    );
    let mut stmt = conn.prepare(&query)?;
    let rows = stmt
        .query_map(params![valid_start, valid_end], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// Handles scored rows from predictions logic.
fn scored_rows_from_predictions(
    meta: &[(String, String, f64)],
    preds: &[f64],
) -> Vec<ScoredReturn> {
    meta.iter()
        .zip(preds.iter())
        .map(|((symbol, date, fwd_return), score)| ScoredReturn {
            symbol: symbol.clone(),
            date: date.clone(),
            score: *score,
            fwd_return: *fwd_return,
        })
        .collect()
}

// Handles trading metrics for predictions calculations.
fn trading_metrics_for_predictions(
    model: &str,
    files: &LgbFiles,
    preds: &[f64],
    top_n: usize,
    slippage_bps: f64,
) -> anyhow::Result<serde_json::Value> {
    let conn = open_ml_db()?;
    let meta = load_validation_meta_rows(&conn, &files.valid_start, files.valid_end.as_deref())?;
    let scored = scored_rows_from_predictions(&meta, preds);
    Ok(trading_metrics_json(model, &scored, top_n, slippage_bps))
}

// Handles cleanup transient training datasets logic.
pub fn cleanup_transient_training_datasets(json_out: bool) -> anyhow::Result<usize> {
    let state_dir = paths::state_dir();
    let mut removed = 0usize;
    if !state_dir.exists() {
        return Ok(0);
    }

    for entry in std::fs::read_dir(&state_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let is_transient = name.ends_with("_training_dataset.txt")
            || name.ends_with("_validation_dataset.txt")
            || matches!(name, "lightgbm_train.txt" | "lightgbm_valid.txt");
        if is_transient && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }

    if json_out {
        println!(
            "{}",
            serde_json::json!({
                "status": "done",
                "removed_transient_training_datasets": removed,
            })
        );
    } else if removed > 0 {
        println!("🧹 Removed {removed} transient LightGBM dataset files");
    }
    Ok(removed)
}

#[derive(Debug, Clone)]
struct ValidationFeatureRow {
    symbol: String,
    date: String,
    target: f64,
    features: Vec<f64>,
    lstm_score: f64,
}

// Loads validation feature rows from lstm from storage or configuration.
fn load_validation_feature_rows_from_lstm(
    lstm_scores: &[ScoredReturn],
) -> anyhow::Result<Vec<ValidationFeatureRow>> {
    let conn = open_ml_db()?;
    let feature_cols = FEATURE_COLS.join(", ");
    let query = format!(
        "SELECT {feature_cols}
         FROM ml_features
         WHERE symbol = ?1
           AND date = ?2
           AND return_1d IS NOT NULL"
    );
    let mut stmt = conn.prepare_cached(&query)?;
    let mut rows = Vec::with_capacity(lstm_scores.len());
    for scored in lstm_scores {
        let features = stmt
            .query_row(params![scored.symbol, scored.date], |r| {
                let mut values = Vec::with_capacity(FEATURE_COLS.len());
                for i in 0..FEATURE_COLS.len() {
                    values.push(r.get::<_, Option<f64>>(i)?.unwrap_or(0.0));
                }
                Ok(values)
            })
            .ok();
        if let Some(features) = features {
            rows.push(ValidationFeatureRow {
                symbol: scored.symbol.clone(),
                date: scored.date.clone(),
                target: scored.fwd_return,
                features,
                lstm_score: scored.score,
            });
        }
    }
    Ok(rows)
}

// Handles zscores logic.
fn zscores(values: &[f64]) -> Vec<f64> {
    let finite = values
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    let avg = mean(&finite);
    let sd = stddev(&finite, avg).max(1e-9);
    values.iter().map(|value| (value - avg) / sd).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LgbBackend {
    Auto,
    Cpu,
    Cuda,
}

impl LgbBackend {
    // Handles config backend logic.
    fn from_config() -> Self {
        match config::lightgbm_backend().as_str() {
            "cpu" => Self::Cpu,
            "cuda" | "gpu" => Self::Cuda,
            "auto" | "" => Self::Auto,
            other => {
                eprintln!(
                    "warning: unsupported backend.lightgbm={}; using auto.",
                    other
                );
                Self::Auto
            }
        }
    }

    // Handles label logic.
    fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cpu => "cpu",
            Self::Cuda => "cuda",
        }
    }
}

#[cfg(mlai_xgboost)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum XgbBackend {
    Auto,
    Cpu,
    Cuda,
}

#[cfg(mlai_xgboost)]
impl XgbBackend {
    // Handles from env logic.
    fn from_env() -> Self {
        match config::xgboost_backend().as_str() {
            "cpu" | "hist" => Self::Cpu,
            "cuda" | "gpu" => Self::Cuda,
            "auto" | "" => Self::Auto,
            other => {
                eprintln!(
                    "warning: unsupported backend.xgboost={}; using auto.",
                    other
                );
                Self::Auto
            }
        }
    }

    // Handles label logic.
    fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cpu => "cpu",
            Self::Cuda => "cuda",
        }
    }
}

// Handles feature indices for cols logic.
fn feature_indices_for_cols(feature_cols: &[&str]) -> Vec<usize> {
    feature_cols
        .iter()
        .filter_map(|feature| {
            FEATURE_COLS
                .iter()
                .position(|candidate| candidate == feature)
        })
        .collect()
}

// Handles without sp500 feature cols logic.
fn without_sp500_feature_cols() -> Vec<&'static str> {
    FEATURE_COLS
        .iter()
        .copied()
        .filter(|feature| !SP500_FEATURE_COLS.contains(feature))
        .collect()
}

// Selects features from available candidates.
fn select_features(row: &ValidationFeatureRow, indices: &[usize]) -> Vec<f64> {
    indices
        .iter()
        .map(|idx| row.features.get(*idx).copied().unwrap_or(0.0))
        .collect()
}

// Returns latest eligible feature rows from local storage.
fn latest_eligible_feature_rows(
    conn: &Connection,
) -> anyhow::Result<(String, Vec<ValidationFeatureRow>)> {
    let latest_date: String = conn.query_row(
        "SELECT COALESCE(MAX(date),'none') FROM ml_features WHERE return_1d IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    if latest_date == "none" {
        anyhow::bail!("No features found. Run 'mlai-trade ml features' first.");
    }

    let feature_cols = FEATURE_COLS.join(", ");
    let asset_join = ml_eligible_asset_join("f", "a");
    let eligible = ml_eligible_asset_predicate("f.symbol", "a");
    let query = format!(
        "SELECT f.symbol, {feature_cols}
         FROM ml_features f
         {asset_join}
         WHERE f.date = ?1
           AND f.return_1d IS NOT NULL
           AND {eligible}
         ORDER BY f.symbol"
    );
    let mut stmt = conn.prepare(&query)?;
    let rows = stmt
        .query_map(params![&latest_date], |r| {
            let symbol: String = r.get(0)?;
            let mut features = Vec::with_capacity(FEATURE_COLS.len());
            for i in 0..FEATURE_COLS.len() {
                features.push(r.get::<_, Option<f64>>(i + 1)?.unwrap_or(0.0));
            }
            Ok(ValidationFeatureRow {
                symbol,
                date: latest_date.clone(),
                target: 0.0,
                features,
                lstm_score: 0.0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if rows.is_empty() {
        anyhow::bail!("No eligible features for date {}", latest_date);
    }

    Ok((latest_date, rows))
}

// Handles mse for scores logic.
fn mse_for_scores(scores: &[f64], targets: &[f64]) -> f64 {
    scores
        .iter()
        .zip(targets)
        .map(|(p, y)| {
            let err = p - y;
            err * err
        })
        .sum::<f64>()
        / targets.len().max(1) as f64
}

// Handles generate weight grid logic.
fn generate_weight_grid(n: usize, steps: usize) -> Vec<Vec<f64>> {
    // Handles rec logic.
    fn rec(
        idx: usize,
        n: usize,
        remaining: usize,
        steps: usize,
        current: &mut Vec<usize>,
        out: &mut Vec<Vec<f64>>,
    ) {
        if idx + 1 == n {
            current.push(remaining);
            out.push(
                current
                    .iter()
                    .map(|value| *value as f64 / steps as f64)
                    .collect(),
            );
            current.pop();
            return;
        }
        for value in 0..=remaining {
            current.push(value);
            rec(idx + 1, n, remaining - value, steps, current, out);
            current.pop();
        }
    }

    let mut out = Vec::new();
    rec(0, n, steps, steps, &mut Vec::new(), &mut out);
    out
}

#[cfg(mlai_xgboost)]
// Handles XGBoost predict feature rows FFI operations.
fn xgb_predict_feature_rows(
    model_path: &std::path::Path,
    rows: &[ValidationFeatureRow],
    feature_indices: &[usize],
) -> anyhow::Result<Vec<f64>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let dmatrix = xgb_dmatrix_from_feature_rows(rows, feature_indices)?;
    let booster = xgb_load_model(model_path)?;
    xgb_predict_dmatrix(&booster, &dmatrix)
}

#[cfg(not(mlai_xgboost))]
// Handles XGBoost predict feature rows FFI operations.
fn xgb_predict_feature_rows(
    _model_path: &std::path::Path,
    _rows: &[ValidationFeatureRow],
    _feature_indices: &[usize],
) -> anyhow::Result<Vec<f64>> {
    anyhow::bail!("XGBoost support is not available on this operating system.")
}

// Handles the ml select default ensemble CLI action.
pub fn cmd_ml_select_default_ensemble(
    json_out: bool,
    top_n: usize,
    slippage_bps: f64,
) -> anyhow::Result<serde_json::Value> {
    let progress = crate::progress::spinner_if(!json_out, "Loading ensemble validation rows");
    let lstm_scored = crate::lstm::validation_scores(false)?;
    let rows = load_validation_feature_rows_from_lstm(&lstm_scored)?;
    progress.finish_and_clear();
    if rows.len() < 1000 {
        anyhow::bail!(
            "Not enough overlapping validation rows for ensemble search: {}",
            rows.len()
        );
    }

    let targets = rows.iter().map(|row| row.target).collect::<Vec<_>>();
    let mut models: Vec<(String, Vec<f64>, Vec<f64>)> = Vec::new();
    let mut component_reports = Vec::new();

    let lgb_path = paths::ml_model_path();
    if lgb_path.exists() {
        let model = LgbModel::load(&lgb_path.to_string_lossy())?;
        let scores = rows
            .iter()
            .map(|row| model.predict_one(&row.features))
            .collect::<Vec<_>>();
        let scored = rows
            .iter()
            .zip(scores.iter())
            .map(|(row, score)| ScoredReturn {
                symbol: row.symbol.clone(),
                date: row.date.clone(),
                score: *score,
                fwd_return: row.target,
            })
            .collect::<Vec<_>>();
        component_reports.push(serde_json::json!({
            "model": "lightgbm",
            "valid_mse": mse_for_scores(&scores, &targets),
            "valid_ic_spearman": spearman_corr(&scores, &targets),
            "trading_metrics_after_slippage": trading_metrics_json("lightgbm", &scored, top_n, slippage_bps),
        }));
        models.push(("lightgbm".to_string(), scores.clone(), zscores(&scores)));
    }

    let xgb_path = paths::state_dir().join("xgboost_baseline_model.json");
    if xgb_path.exists() {
        match xgb_predict_feature_rows(&xgb_path, &rows, &feature_indices_for_cols(FEATURE_COLS)) {
            Ok(scores) => {
                let scored = rows
                    .iter()
                    .zip(scores.iter())
                    .map(|(row, score)| ScoredReturn {
                        symbol: row.symbol.clone(),
                        date: row.date.clone(),
                        score: *score,
                        fwd_return: row.target,
                    })
                    .collect::<Vec<_>>();
                component_reports.push(serde_json::json!({
                    "model": "xgboost",
                    "valid_mse": mse_for_scores(&scores, &targets),
                    "valid_ic_spearman": spearman_corr(&scores, &targets),
                    "trading_metrics_after_slippage": trading_metrics_json("xgboost", &scored, top_n, slippage_bps),
                }));
                models.push(("xgboost".to_string(), scores.clone(), zscores(&scores)));
            }
            Err(err) => component_reports.push(serde_json::json!({
                "model": "xgboost",
                "available": false,
                "error": err.to_string(),
            })),
        }
    }

    let lstm_scores = rows.iter().map(|row| row.lstm_score).collect::<Vec<_>>();
    let lstm_component_scored = rows
        .iter()
        .zip(lstm_scores.iter())
        .map(|(row, score)| ScoredReturn {
            symbol: row.symbol.clone(),
            date: row.date.clone(),
            score: *score,
            fwd_return: row.target,
        })
        .collect::<Vec<_>>();
    component_reports.push(serde_json::json!({
        "model": "lstm",
        "valid_mse": mse_for_scores(&lstm_scores, &targets),
        "valid_ic_spearman": spearman_corr(&lstm_scores, &targets),
        "trading_metrics_after_slippage": trading_metrics_json("lstm", &lstm_component_scored, top_n, slippage_bps),
    }));
    models.push((
        "lstm".to_string(),
        lstm_scores.clone(),
        zscores(&lstm_scores),
    ));

    let mut candidates = Vec::new();
    let weight_grid = generate_weight_grid(models.len(), 10);
    let progress = crate::progress::bar_if(
        !json_out,
        weight_grid.len() as u64,
        "Searching ensemble weights",
    );
    for weights in weight_grid {
        if weights.iter().all(|weight| *weight == 0.0) {
            progress.inc(1);
            continue;
        }

        let mut scores = vec![0.0; rows.len()];
        let mut weight_sum = 0.0;
        let mut weight_json = serde_json::Map::new();
        let mut name_parts = Vec::new();
        for (idx, weight) in weights.iter().enumerate() {
            if *weight <= 0.0 {
                continue;
            }
            weight_sum += weight;
            let name = &models[idx].0;
            name_parts.push(format!("{name}:{weight:.1}"));
            weight_json.insert(name.clone(), serde_json::json!(weight));
            for (row_idx, score) in models[idx].2.iter().enumerate() {
                scores[row_idx] += weight * score;
            }
        }
        if weight_sum <= 0.0 {
            progress.inc(1);
            continue;
        }
        for score in &mut scores {
            *score /= weight_sum;
        }

        let scored = rows
            .iter()
            .zip(scores.iter())
            .map(|(row, score)| ScoredReturn {
                symbol: row.symbol.clone(),
                date: row.date.clone(),
                score: *score,
                fwd_return: row.target,
            })
            .collect::<Vec<_>>();
        let trading = trading_metrics_json(
            &format!("ensemble_{}", name_parts.join("_")),
            &scored,
            top_n,
            slippage_bps,
        );
        candidates.push(serde_json::json!({
            "weights": serde_json::Value::Object(weight_json),
            "score_scale": "zscore_per_model_then_weighted_sum",
            "valid_ic_spearman": spearman_corr(&scores, &targets),
            "trading_metrics_after_slippage": trading,
        }));
        progress.set_message(name_parts.join(","));
        progress.inc(1);
    }
    progress.finish_and_clear();

    candidates.sort_by(|a, b| {
        let a_net = a["trading_metrics_after_slippage"]["avg_net_5d_return"]
            .as_f64()
            .unwrap_or(f64::NEG_INFINITY);
        let b_net = b["trading_metrics_after_slippage"]["avg_net_5d_return"]
            .as_f64()
            .unwrap_or(f64::NEG_INFINITY);
        b_net
            .partial_cmp(&a_net)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b["valid_ic_spearman"]
                    .as_f64()
                    .unwrap_or(f64::NEG_INFINITY)
                    .partial_cmp(&a["valid_ic_spearman"].as_f64().unwrap_or(f64::NEG_INFINITY))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    let best = candidates
        .first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("No ensemble candidates generated"))?;
    let config = serde_json::json!({
        "status": "done",
        "selected_by": "highest_avg_net_5d_return_after_slippage_on_filtered_validation_rows",
        "top_n_per_date": top_n,
        "round_trip_slippage_bps": slippage_bps,
        "weights": best["weights"].clone(),
        "score_scale": best["score_scale"].clone(),
    });
    let config_path = paths::state_dir().join("ml_default_ensemble_config.json");
    paths::write_private_file(&config_path, serde_json::to_string_pretty(&config)?)?;

    let top_candidates = candidates.iter().take(20).cloned().collect::<Vec<_>>();
    let candidate_count = candidates.len();
    let report = serde_json::json!({
        "status": "done",
        "validation_rows": rows.len(),
        "validation_dates": rows.iter().map(|row| row.date.as_str()).collect::<std::collections::BTreeSet<_>>().len(),
        "universe": "filtered_common_stock_etf_like_assets",
        "selection_metric": "avg_net_5d_return_after_slippage",
        "top_n_per_date": top_n,
        "round_trip_slippage_bps": slippage_bps,
        "component_models": component_reports,
        "best": best,
        "candidate_count": candidate_count,
        "all_candidates": candidates,
        "top_candidates": top_candidates,
        "config_path": config_path.display().to_string(),
        "note": "Validation uses the LSTM validation rows so model combinations are compared on the same symbols/dates. Scores are z-scored per model before weighting because model output scales differ.",
    });
    let report_path = paths::state_dir().join("ml_ensemble_search_report.json");
    paths::write_private_file(&report_path, serde_json::to_string_pretty(&report)?)?;

    if json_out {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("🧪 Ensemble Search");
        println!("  Rows:   {}", rows.len());
        println!(
            "  Best:   {}",
            serde_json::to_string(&report["best"]["weights"])?
        );
        println!("  Report: {}", report_path.display());
    }
    Ok(report)
}

#[derive(Clone)]
struct SweepComponent {
    name: String,
    scores_z: Vec<f64>,
    scores_raw: Vec<f64>,
}

// Handles component report logic.
fn component_report(
    component: &SweepComponent,
    rows: &[ValidationFeatureRow],
    targets: &[f64],
    top_n: usize,
    slippage_bps: f64,
) -> serde_json::Value {
    let scored = rows
        .iter()
        .zip(component.scores_raw.iter())
        .map(|(row, score)| ScoredReturn {
            symbol: row.symbol.clone(),
            date: row.date.clone(),
            score: *score,
            fwd_return: row.target,
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "model": component.name,
        "valid_mse": mse_for_scores(&component.scores_raw, targets),
        "valid_ic_spearman": spearman_corr(&component.scores_raw, targets),
        "trading_metrics_after_slippage": trading_metrics_json(&component.name, &scored, top_n, slippage_bps),
    })
}

// Builds LightGBM component configuration.
fn lgb_component(
    name: &str,
    model_path: &std::path::Path,
    rows: &[ValidationFeatureRow],
    feature_indices: &[usize],
) -> anyhow::Result<SweepComponent> {
    let model = LgbModel::load(&model_path.to_string_lossy())?;
    let scores_raw = rows
        .iter()
        .map(|row| model.predict_one(&select_features(row, feature_indices)))
        .collect::<Vec<_>>();
    let scores_z = zscores(&scores_raw);
    Ok(SweepComponent {
        name: name.to_string(),
        scores_z,
        scores_raw,
    })
}

// Returns LSTM component runtime settings.
fn lstm_component(
    name: &str,
    rows: &[ValidationFeatureRow],
    scored_rows: &[ScoredReturn],
) -> anyhow::Result<SweepComponent> {
    let by_key = scored_rows
        .iter()
        .map(|row| ((row.symbol.as_str(), row.date.as_str()), row.score))
        .collect::<std::collections::HashMap<_, _>>();
    let mut scores_raw = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(score) = by_key
            .get(&(row.symbol.as_str(), row.date.as_str()))
            .copied()
        else {
            anyhow::bail!(
                "LSTM component {name} is missing score for {} {}",
                row.symbol,
                row.date
            );
        };
        scores_raw.push(score);
    }
    let scores_z = zscores(&scores_raw);
    Ok(SweepComponent {
        name: name.to_string(),
        scores_z,
        scores_raw,
    })
}

// Handles XGBoost component FFI operations.
fn xgb_component(
    name: &str,
    model_path: &std::path::Path,
    rows: &[ValidationFeatureRow],
    feature_indices: &[usize],
) -> anyhow::Result<SweepComponent> {
    let scores_raw = xgb_predict_feature_rows(model_path, rows, feature_indices)?;
    let scores_z = zscores(&scores_raw);
    Ok(SweepComponent {
        name: name.to_string(),
        scores_z,
        scores_raw,
    })
}

// Returns true when XGBoost is optional and should not break full ML refresh.
fn xgboost_is_auto_optional() -> bool {
    config::xgboost_backend()
        .trim()
        .eq_ignore_ascii_case("auto")
}

#[derive(Clone)]
struct SweepAgg {
    key: String,
    feature_set: String,
    grid_step_pct: f64,
    weights: serde_json::Value,
    count: usize,
    avg_net_sum: f64,
    sharpe_sum: f64,
    win_sum: f64,
    worst_drawdown: f64,
    ic: f64,
}

impl SweepAgg {
    // Handles update logic.
    fn update(&mut self, trading: &serde_json::Value) {
        self.count += 1;
        self.avg_net_sum += trading["avg_net_5d_return"].as_f64().unwrap_or(0.0);
        self.sharpe_sum += trading["sharpe_5d_bucket"].as_f64().unwrap_or(0.0);
        self.win_sum += trading["trade_win_rate_after_slippage"]
            .as_f64()
            .unwrap_or(0.0);
        let drawdown = trading["max_drawdown"].as_f64().unwrap_or(0.0);
        if self.count == 1 || drawdown < self.worst_drawdown {
            self.worst_drawdown = drawdown;
        }
    }

    // Handles json logic.
    fn json(&self) -> serde_json::Value {
        let count = self.count.max(1) as f64;
        serde_json::json!({
            "key": self.key,
            "feature_set": self.feature_set,
            "grid_step_pct": self.grid_step_pct,
            "weights": self.weights,
            "valid_ic_spearman": self.ic,
            "mean_avg_net_5d_return": self.avg_net_sum / count,
            "mean_sharpe_5d_bucket": self.sharpe_sum / count,
            "mean_win_rate": self.win_sum / count,
            "worst_max_drawdown": self.worst_drawdown,
            "evaluations": self.count,
        })
    }
}

// Sorts by metric desc by the requested metric.
fn sort_by_metric_desc(values: &mut [serde_json::Value], path: &[&str]) {
    values.sort_by(|a, b| {
        let mut av = a;
        let mut bv = b;
        for key in path {
            av = &av[*key];
            bv = &bv[*key];
        }
        bv.as_f64()
            .unwrap_or(f64::NEG_INFINITY)
            .partial_cmp(&av.as_f64().unwrap_or(f64::NEG_INFINITY))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

// Handles the ml ensemble robust sweep CLI action.
pub fn cmd_ml_ensemble_robust_sweep(json_out: bool) -> anyhow::Result<serde_json::Value> {
    let top_n_values = [5usize, 10, 20, 50];
    let slippage_values = [10.0f64, 25.0, 50.0, 100.0, 200.0];
    let grid_steps = [20usize, 40usize];
    let default_top_n = 20usize;
    let default_slippage = 50.0f64;

    let progress = crate::progress::spinner_if(!json_out, "Loading ensemble validation rows");
    let lstm_full = crate::lstm::validation_scores(false)?;
    let rows = load_validation_feature_rows_from_lstm(&lstm_full)?;
    progress.finish_and_clear();
    if rows.len() < 1000 {
        anyhow::bail!(
            "Not enough overlapping validation rows for robust sweep: {}",
            rows.len()
        );
    }
    let targets = rows.iter().map(|row| row.target).collect::<Vec<_>>();
    let full_indices = feature_indices_for_cols(FEATURE_COLS);
    let without_cols = without_sp500_feature_cols();
    let without_indices = feature_indices_for_cols(&without_cols);

    let mut model_sets: Vec<(String, Vec<SweepComponent>)> = Vec::new();
    let mut component_reports = serde_json::Map::new();
    let mut skipped_components = Vec::new();

    let mut full_components = Vec::new();
    full_components.push(lgb_component(
        "lightgbm",
        &paths::ml_model_path(),
        &rows,
        &full_indices,
    )?);
    let xgb_path = paths::state_dir().join("xgboost_baseline_model.json");
    if xgb_path.exists() {
        match xgb_component("xgboost", &xgb_path, &rows, &full_indices) {
            Ok(component) => full_components.push(component),
            Err(err) if xgboost_is_auto_optional() => {
                let message = err.to_string();
                eprintln!("  warning: XGBoost full-feature component skipped: {message}");
                skipped_components.push(serde_json::json!({
                    "model": "xgboost",
                    "feature_set": "full_features",
                    "available": false,
                    "status": "skipped_auto",
                    "error": message,
                }));
            }
            Err(err) => return Err(err),
        }
    }
    full_components.push(lstm_component("lstm", &rows, &lstm_full)?);
    component_reports.insert(
        "full_features".to_string(),
        serde_json::json!(full_components
            .iter()
            .map(|component| component_report(
                component,
                &rows,
                &targets,
                default_top_n,
                default_slippage
            ))
            .collect::<Vec<_>>()),
    );
    model_sets.push(("full_features".to_string(), full_components));

    let mut without_components = Vec::new();
    let lgb_without_path = paths::state_dir().join("lightgbm_without_sp500_model.txt");
    if lgb_without_path.exists() {
        without_components.push(lgb_component(
            "lightgbm_without_sp500",
            &lgb_without_path,
            &rows,
            &without_indices,
        )?);
    }
    let xgb_without_path = paths::state_dir().join("xgboost_without_sp500_model.json");
    if xgb_without_path.exists() {
        match xgb_component(
            "xgboost_without_sp500",
            &xgb_without_path,
            &rows,
            &without_indices,
        ) {
            Ok(component) => without_components.push(component),
            Err(err) if xgboost_is_auto_optional() => {
                let message = err.to_string();
                eprintln!("  warning: XGBoost no-S&P component skipped: {message}");
                skipped_components.push(serde_json::json!({
                    "model": "xgboost_without_sp500",
                    "feature_set": "without_sp500",
                    "available": false,
                    "status": "skipped_auto",
                    "error": message,
                }));
            }
            Err(err) => return Err(err),
        }
    }
    let lstm_without_path = paths::state_dir().join("lstm_sequence_model_without_sp500.bin");
    if lstm_without_path.exists() {
        let lstm_without = crate::lstm::validation_scores(true)?;
        without_components.push(lstm_component("lstm_without_sp500", &rows, &lstm_without)?);
    }
    if without_components.len() >= 2 {
        component_reports.insert(
            "without_sp500".to_string(),
            serde_json::json!(without_components
                .iter()
                .map(|component| component_report(
                    component,
                    &rows,
                    &targets,
                    default_top_n,
                    default_slippage
                ))
                .collect::<Vec<_>>()),
        );
        model_sets.push(("without_sp500".to_string(), without_components));
    }
    if !skipped_components.is_empty() {
        component_reports.insert(
            "skipped_components".to_string(),
            serde_json::Value::Array(skipped_components),
        );
    }

    let mut all_candidates = Vec::new();
    let mut aggregate: std::collections::HashMap<String, SweepAgg> =
        std::collections::HashMap::new();
    let sweep_units = model_sets
        .iter()
        .flat_map(|(_, components)| {
            grid_steps.iter().map(move |grid_step| {
                generate_weight_grid(components.len(), *grid_step)
                    .into_iter()
                    .filter(|weights| weights.iter().any(|weight| *weight > 0.0))
                    .count()
            })
        })
        .sum::<usize>()
        .saturating_mul(top_n_values.len())
        .saturating_mul(slippage_values.len());
    let progress = crate::progress::bar_if(
        !json_out,
        sweep_units as u64,
        "Evaluating ensemble combinations",
    );

    for (feature_set, components) in &model_sets {
        for &grid_step in &grid_steps {
            for weights in generate_weight_grid(components.len(), grid_step) {
                let mut weight_json = serde_json::Map::new();
                let mut weights_key_parts = Vec::new();
                for (idx, weight) in weights.iter().enumerate() {
                    if *weight > 0.0 {
                        weight_json.insert(components[idx].name.clone(), serde_json::json!(weight));
                        weights_key_parts.push(format!("{}:{weight:.4}", components[idx].name));
                    }
                }
                if weight_json.is_empty() {
                    continue;
                }

                let mut scores = vec![0.0; rows.len()];
                for (component_idx, weight) in weights.iter().enumerate() {
                    if *weight <= 0.0 {
                        continue;
                    }
                    for (row_idx, score) in components[component_idx].scores_z.iter().enumerate() {
                        scores[row_idx] += weight * score;
                    }
                }
                let ic = spearman_corr(&scores, &targets);
                let scored = rows
                    .iter()
                    .zip(scores.iter())
                    .map(|(row, score)| ScoredReturn {
                        symbol: row.symbol.clone(),
                        date: row.date.clone(),
                        score: *score,
                        fwd_return: row.target,
                    })
                    .collect::<Vec<_>>();

                let weights_value = serde_json::Value::Object(weight_json);
                let key = format!(
                    "{feature_set}|{}|{}",
                    100.0 / grid_step as f64,
                    weights_key_parts.join(",")
                );
                for &top_n in &top_n_values {
                    for &slippage in &slippage_values {
                        progress.set_message(format!(
                            "{feature_set} {} top={top_n} slip={slippage:.0}bps",
                            weights_key_parts.join(",")
                        ));
                        let trading = trading_metrics_json(
                            &format!("robust_{}_{}", feature_set, weights_key_parts.join("_")),
                            &scored,
                            top_n,
                            slippage,
                        );
                        all_candidates.push(serde_json::json!({
                            "feature_set": feature_set,
                            "grid_step_pct": 100.0 / grid_step as f64,
                            "weights": weights_value.clone(),
                            "top_n_per_date": top_n,
                            "round_trip_slippage_bps": slippage,
                            "valid_ic_spearman": ic,
                            "trading_metrics_after_slippage": trading.clone(),
                        }));

                        let entry = aggregate.entry(key.clone()).or_insert_with(|| SweepAgg {
                            key: key.clone(),
                            feature_set: feature_set.clone(),
                            grid_step_pct: 100.0 / grid_step as f64,
                            weights: weights_value.clone(),
                            count: 0,
                            avg_net_sum: 0.0,
                            sharpe_sum: 0.0,
                            win_sum: 0.0,
                            worst_drawdown: 0.0,
                            ic,
                        });
                        entry.update(&trading);
                        progress.inc(1);
                    }
                }
            }
        }
    }
    progress.finish_and_clear();

    sort_by_metric_desc(
        &mut all_candidates,
        &["trading_metrics_after_slippage", "avg_net_5d_return"],
    );
    let mut aggregate_values = aggregate
        .values()
        .map(SweepAgg::json)
        .collect::<Vec<serde_json::Value>>();
    sort_by_metric_desc(&mut aggregate_values, &["mean_avg_net_5d_return"]);

    let best_overall = all_candidates
        .first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("No robust sweep candidates generated"))?;
    let best_by_ic = all_candidates
        .iter()
        .max_by(|a, b| {
            a["valid_ic_spearman"]
                .as_f64()
                .unwrap_or(f64::NEG_INFINITY)
                .partial_cmp(&b["valid_ic_spearman"].as_f64().unwrap_or(f64::NEG_INFINITY))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .cloned()
        .unwrap_or_else(|| best_overall.clone());
    let best_by_sharpe = all_candidates
        .iter()
        .max_by(|a, b| {
            a["trading_metrics_after_slippage"]["sharpe_5d_bucket"]
                .as_f64()
                .unwrap_or(f64::NEG_INFINITY)
                .partial_cmp(
                    &b["trading_metrics_after_slippage"]["sharpe_5d_bucket"]
                        .as_f64()
                        .unwrap_or(f64::NEG_INFINITY),
                )
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .cloned()
        .unwrap_or_else(|| best_overall.clone());
    let best_by_drawdown = all_candidates
        .iter()
        .max_by(|a, b| {
            a["trading_metrics_after_slippage"]["max_drawdown"]
                .as_f64()
                .unwrap_or(f64::NEG_INFINITY)
                .partial_cmp(
                    &b["trading_metrics_after_slippage"]["max_drawdown"]
                        .as_f64()
                        .unwrap_or(f64::NEG_INFINITY),
                )
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .cloned()
        .unwrap_or_else(|| best_overall.clone());
    let best_ic_floor = all_candidates
        .iter()
        .filter(|candidate| candidate["valid_ic_spearman"].as_f64().unwrap_or(0.0) >= 0.10)
        .max_by(|a, b| {
            a["trading_metrics_after_slippage"]["avg_net_5d_return"]
                .as_f64()
                .unwrap_or(f64::NEG_INFINITY)
                .partial_cmp(
                    &b["trading_metrics_after_slippage"]["avg_net_5d_return"]
                        .as_f64()
                        .unwrap_or(f64::NEG_INFINITY),
                )
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .cloned()
        .unwrap_or_else(|| best_overall.clone());

    let robust_default = aggregate_values
        .iter()
        .filter(|candidate| {
            candidate["grid_step_pct"].as_f64().unwrap_or(0.0) <= 2.5
                && candidate["valid_ic_spearman"].as_f64().unwrap_or(0.0) >= 0.10
        })
        .max_by(|a, b| {
            a["mean_avg_net_5d_return"]
                .as_f64()
                .unwrap_or(f64::NEG_INFINITY)
                .partial_cmp(
                    &b["mean_avg_net_5d_return"]
                        .as_f64()
                        .unwrap_or(f64::NEG_INFINITY),
                )
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .cloned()
        .or_else(|| aggregate_values.first().cloned())
        .ok_or_else(|| anyhow::anyhow!("No aggregate robust candidates generated"))?;

    let config = serde_json::json!({
        "status": "done",
        "selected_by": "robust_mean_avg_net_5d_after_slippage_across_topn_and_slippage_with_ic_floor",
        "feature_set": robust_default["feature_set"].clone(),
        "weights": robust_default["weights"].clone(),
        "score_scale": "zscore_per_model_then_weighted_sum",
        "robust_metrics": robust_default,
    });
    let config_path = paths::state_dir().join("ml_default_ensemble_config.json");
    paths::write_private_file(&config_path, serde_json::to_string_pretty(&config)?)?;

    let report = serde_json::json!({
        "status": "done",
        "validation_rows": rows.len(),
        "validation_dates": rows.iter().map(|row| row.date.as_str()).collect::<std::collections::BTreeSet<_>>().len(),
        "grid_steps_pct": grid_steps.iter().map(|step| 100.0 / *step as f64).collect::<Vec<_>>(),
        "top_n_values": top_n_values,
        "slippage_bps_values": slippage_values,
        "candidate_count": all_candidates.len(),
        "aggregate_candidate_count": aggregate_values.len(),
        "component_models": component_reports,
        "objective_winners": {
            "highest_avg_net_5d": best_overall,
            "highest_ic": best_by_ic,
            "highest_sharpe": best_by_sharpe,
            "lowest_drawdown": best_by_drawdown,
            "ic_at_least_0_10_then_highest_avg_net_5d": best_ic_floor,
        },
        "robust_default": config["robust_metrics"].clone(),
        "top_aggregate_candidates": aggregate_values.iter().take(50).cloned().collect::<Vec<_>>(),
        "all_candidates": all_candidates,
        "config_path": config_path.display().to_string(),
        "note": "Robust sweep compares full-feature and no-SP500 model sets where trained artifacts exist. The saved deployable default is the best aggregate candidate with IC >= 0.10 on the finest grid; ensemble-default can materialize either feature set into ml_predictions.",
    });
    let report_path = paths::state_dir().join("ml_ensemble_robust_sweep_report.json");
    paths::write_private_file(&report_path, serde_json::to_string_pretty(&report)?)?;

    if json_out {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("🧪 Robust Ensemble Sweep");
        println!("  Validation rows: {}", rows.len());
        println!("  Candidates:      {}", report["candidate_count"]);
        println!(
            "  Default:         {}",
            serde_json::to_string(&config["weights"])?
        );
        println!("  Report:          {}", report_path.display());
    }
    Ok(report)
}

// Loads lgb text dataset from storage or configuration.
fn load_lgb_text_dataset(
    path: &std::path::Path,
    n_features: usize,
) -> anyhow::Result<(Vec<f64>, Vec<f64>)> {
    let content = std::fs::read_to_string(path)?;
    let mut labels = Vec::new();
    let mut features = Vec::with_capacity(n_features * 1024);

    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let mut parts = line.split_whitespace();
        let Some(label) = parts.next() else {
            continue;
        };
        labels.push(label.parse::<f64>()?);
        let row_start = features.len();
        features.resize(row_start + n_features, 0.0);
        for part in parts {
            if let Some((idx, value)) = part.split_once(':') {
                let idx = idx.parse::<usize>()?;
                if idx >= n_features {
                    anyhow::bail!(
                        "Invalid LightGBM feature index {} in {}",
                        idx,
                        path.display()
                    );
                }
                features[row_start + idx] = value.parse::<f64>()?;
            }
        }
    }

    Ok((labels, features))
}

// Ranks values for model evaluation.
fn rank_values(values: &[f64]) -> Vec<f64> {
    let mut indexed = values
        .iter()
        .copied()
        .enumerate()
        .collect::<Vec<(usize, f64)>>();
    indexed.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut ranks = vec![0.0; values.len()];
    let mut i = 0usize;
    while i < indexed.len() {
        let mut j = i + 1;
        while j < indexed.len() && (indexed[j].1 - indexed[i].1).abs() < 1e-12 {
            j += 1;
        }
        let avg_rank = (i + j - 1) as f64 / 2.0;
        for k in i..j {
            ranks[indexed[k].0] = avg_rank;
        }
        i = j;
    }
    ranks
}

// Computes pearson corr correlation for model evaluation.
fn pearson_corr(xs: &[f64], ys: &[f64]) -> f64 {
    if xs.len() != ys.len() || xs.len() < 2 {
        return 0.0;
    }
    let n = xs.len() as f64;
    let mean_x = xs.iter().sum::<f64>() / n;
    let mean_y = ys.iter().sum::<f64>() / n;
    let mut cov = 0.0;
    let mut var_x = 0.0;
    let mut var_y = 0.0;
    for (&x, &y) in xs.iter().zip(ys) {
        let dx = x - mean_x;
        let dy = y - mean_y;
        cov += dx * dy;
        var_x += dx * dx;
        var_y += dy * dy;
    }
    if var_x <= 0.0 || var_y <= 0.0 {
        0.0
    } else {
        cov / (var_x.sqrt() * var_y.sqrt())
    }
}

// Computes spearman corr correlation for model evaluation.
fn spearman_corr(xs: &[f64], ys: &[f64]) -> f64 {
    pearson_corr(&rank_values(xs), &rank_values(ys))
}

// Handles train lgb variant logic.
fn train_lgb_variant(
    conn: &Connection,
    name: &str,
    feature_cols: &[&str],
    quick: bool,
    show_progress: bool,
) -> anyhow::Result<serde_json::Value> {
    let state_dir = paths::ensure_state_dir()?;
    let files = if feature_cols == FEATURE_COLS {
        write_lgb_training_files(conn, show_progress)?
    } else {
        write_lgb_training_files_for_cols(
            conn,
            feature_cols,
            state_dir.join(format!("lightgbm_{name}_training_dataset.txt")),
            state_dir.join(format!("lightgbm_{name}_validation_dataset.txt")),
            show_progress,
        )?
    };
    eprintln!(
        "  [{}] train rows: {}, valid rows: {}, features: {}",
        name,
        files.train_rows,
        files.valid_rows,
        feature_cols.len()
    );
    train_lgb_from_files(name, feature_cols, &files, quick, true, show_progress)
}

struct LgbTrainOutcome {
    booster: Booster,
    backend: LgbBackend,
    requested_backend: LgbBackend,
    fallback_failures: Vec<Value>,
}

// Handles LightGBM backend attempts with CPU fallback for auto mode.
fn train_lgb_booster_from_files(
    name: &str,
    feature_cols: &[&str],
    files: &LgbFiles,
    quick: bool,
    show_progress: bool,
) -> anyhow::Result<LgbTrainOutcome> {
    let requested_backend = LgbBackend::from_config();
    let specific_backend_requested = requested_backend != LgbBackend::Auto;
    let mut attempts = match requested_backend {
        LgbBackend::Auto => {
            if cfg!(mlai_lightgbm_cuda) {
                vec![LgbBackend::Cuda, LgbBackend::Cpu]
            } else {
                vec![LgbBackend::Cpu]
            }
        }
        backend => vec![backend],
    };
    attempts.dedup();

    let mut fallback_failures = Vec::new();
    for backend in attempts {
        match train_lgb_booster_from_files_once(
            name,
            feature_cols,
            files,
            quick,
            backend,
            show_progress,
        ) {
            Ok(booster) => {
                return Ok(LgbTrainOutcome {
                    booster,
                    backend,
                    requested_backend,
                    fallback_failures,
                });
            }
            Err(err) if !specific_backend_requested => {
                eprintln!(
                    "  warning: LightGBM backend '{}' failed cleanly: {}; falling back to CPU.",
                    backend.label(),
                    err
                );
                fallback_failures.push(serde_json::json!({
                    "backend": backend.label(),
                    "error": err.to_string(),
                }));
            }
            Err(err) => return Err(err),
        }
    }

    anyhow::bail!(
        "All LightGBM backend attempts failed: {:?}",
        fallback_failures
    )
}

// Trains a LightGBM booster for one concrete backend.
fn train_lgb_booster_from_files_once(
    name: &str,
    feature_cols: &[&str],
    files: &LgbFiles,
    quick: bool,
    backend: LgbBackend,
    show_progress: bool,
) -> anyhow::Result<Booster> {
    if backend == LgbBackend::Cuda {
        return train_lgb_booster_with_cli(name, files, quick, backend, show_progress);
    }

    let progress = crate::progress::spinner_if(
        show_progress,
        format!("Loading LightGBM datasets: {name} ({})", backend.label()),
    );
    let mut train = Dataset::from_file(&files.train_path.to_string_lossy())
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let names = feature_cols
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let _ = train.set_feature_names(&names);
    let valid =
        Dataset::from_file_with_reference(&files.valid_path.to_string_lossy(), Some(&train))
            .map_err(|e| anyhow::anyhow!("{}", e))?;
    progress.finish_and_clear();
    let params = lgb_params(
        if quick { 100 } else { 500 },
        Some(if quick { 20 } else { 50 }),
        backend,
    );
    let progress = crate::progress::spinner_if(
        show_progress,
        format!("Training LightGBM booster: {name} ({})", backend.label()),
    );
    let booster = Booster::train_with_valid(train, Some(valid), &params)
        .map_err(|e| anyhow::anyhow!("{}", e));
    progress.finish_and_clear();
    booster
}

// Trains LightGBM through the packaged CLI so native CUDA crashes stay isolated.
fn train_lgb_booster_with_cli(
    name: &str,
    files: &LgbFiles,
    quick: bool,
    backend: LgbBackend,
    show_progress: bool,
) -> anyhow::Result<Booster> {
    let lightgbm = packaged_lightgbm_binary()?;
    let state_dir = paths::ensure_state_dir()?;
    let unique = format!(
        "{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let safe_name = safe_lightgbm_artifact_name(name);
    let config_path = state_dir.join(format!("lightgbm_{safe_name}_{unique}.conf"));
    let model_path = state_dir.join(format!("lightgbm_{safe_name}_{unique}.model"));
    let params = lgb_params(
        if quick { 100 } else { 500 },
        Some(if quick { 20 } else { 50 }),
        backend,
    );
    fs::write(
        &config_path,
        lightgbm_cli_config(&params, files, &model_path)?,
    )?;

    let progress = crate::progress::spinner_if(
        show_progress,
        format!("Training LightGBM booster: {name} ({})", backend.label()),
    );
    let output = Command::new(&lightgbm)
        .arg(format!("config={}", config_path.display()))
        .env("LD_LIBRARY_PATH", packaged_library_path()?)
        .output()
        .map_err(|err| anyhow::anyhow!("failed to run {}: {err}", lightgbm.display()));
    progress.finish_and_clear();

    let output = match output {
        Ok(output) => output,
        Err(err) => {
            let _ = fs::remove_file(&config_path);
            let _ = fs::remove_file(&model_path);
            return Err(err);
        }
    };

    if !output.status.success() {
        let _ = fs::remove_file(&config_path);
        let _ = fs::remove_file(&model_path);
        anyhow::bail!(
            "LightGBM CLI backend '{}' failed with status {}: stdout={} stderr={}",
            backend.label(),
            process_status_label(output.status),
            compact_process_output(&output.stdout),
            compact_process_output(&output.stderr)
        );
    }

    let booster = Booster::from_file(&model_path.to_string_lossy())
        .map_err(|err| anyhow::anyhow!("failed to load LightGBM CLI model: {err}"));
    let _ = fs::remove_file(&config_path);
    let _ = fs::remove_file(&model_path);
    booster
}

// Converts JSON params into LightGBM CLI config text.
fn lightgbm_cli_config(
    params: &serde_json::Value,
    files: &LgbFiles,
    model_path: &Path,
) -> anyhow::Result<String> {
    let mut lines = vec![
        "task=train".to_string(),
        format!("data={}", files.train_path.display()),
        format!("valid_data={}", files.valid_path.display()),
        format!("output_model={}", model_path.display()),
    ];
    let obj = params
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("LightGBM params must be a JSON object"))?;
    let mut keys = obj.keys().collect::<Vec<_>>();
    keys.sort();
    for key in keys {
        let value = &obj[key];
        let value = match value {
            serde_json::Value::Bool(value) => value.to_string(),
            serde_json::Value::Number(value) => value.to_string(),
            serde_json::Value::String(value) => value.clone(),
            serde_json::Value::Null => continue,
            other => anyhow::bail!("unsupported LightGBM CLI param {key}={other}"),
        };
        lines.push(format!("{key}={value}"));
    }
    lines.push(String::new());
    Ok(lines.join("\n"))
}

// Returns a filesystem-safe artifact name for LightGBM child-process files.
fn safe_lightgbm_artifact_name(name: &str) -> String {
    let safe = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if safe.is_empty() {
        "model".to_string()
    } else {
        safe
    }
}

// Formats process status without relying on platform-specific display output.
fn process_status_label(status: std::process::ExitStatus) -> String {
    status
        .code()
        .map(|code| format!("exit code {code}"))
        .unwrap_or_else(|| "terminated by signal".to_string())
}

// Keeps subprocess output useful without dumping large native logs.
fn compact_process_output(output: &[u8]) -> String {
    const MAX_CHARS: usize = 2000;
    let text = String::from_utf8_lossy(output).trim().to_string();
    if text.chars().count() <= MAX_CHARS {
        return text;
    }
    let mut compact = text.chars().take(MAX_CHARS).collect::<String>();
    compact.push_str("...");
    compact
}

// Handles train lgb from files logic.
fn train_lgb_from_files(
    name: &str,
    feature_cols: &[&str],
    files: &LgbFiles,
    quick: bool,
    keep_model: bool,
    show_progress: bool,
) -> anyhow::Result<serde_json::Value> {
    let state_dir = paths::ensure_state_dir()?;
    let trained = train_lgb_booster_from_files(name, feature_cols, files, quick, show_progress)?;
    let booster = trained.booster;

    let model_path = state_dir.join(format!("lightgbm_{name}_model.txt"));
    if keep_model {
        booster
            .save_file(&model_path.to_string_lossy())
            .map_err(|e| anyhow::anyhow!("{}", e))?;
    }

    let progress = crate::progress::spinner_if(
        show_progress,
        format!("Evaluating LightGBM validation: {name}"),
    );
    let (labels, features) = load_lgb_text_dataset(&files.valid_path, feature_cols.len())?;
    let preds = booster
        .predict(&features, feature_cols.len() as i32, true)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let mse = preds
        .iter()
        .zip(labels.iter())
        .map(|(p, y)| {
            let err = p - y;
            err * err
        })
        .sum::<f64>()
        / labels.len().max(1) as f64;
    let ic = spearman_corr(&preds, &labels);
    let trading_metrics = trading_metrics_for_predictions(
        name,
        files,
        &preds,
        20,
        DEFAULT_ROUND_TRIP_SPREAD_SLIPPAGE_BPS,
    )?;
    let importances = booster
        .feature_importance(ImportanceType::Gain)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let feature_importance = feature_cols
        .iter()
        .zip(importances.iter())
        .map(|(feature, value)| serde_json::json!({"feature": feature, "gain": value}))
        .collect::<Vec<_>>();
    progress.finish_and_clear();

    Ok(serde_json::json!({
        "name": name,
        "features": feature_cols,
        "feature_count": feature_cols.len(),
        "model_path": if keep_model { serde_json::json!(model_path) } else { serde_json::Value::Null },
        "train_rows": files.train_rows,
        "valid_rows": files.valid_rows,
        "train_candidate_rows": files.train_candidate_rows,
        "valid_candidate_rows": files.valid_candidate_rows,
        "train_stride": files.train_stride,
        "valid_stride": files.valid_stride,
        "date_start": files.date_start,
        "date_end": files.date_end,
        "unique_dates": files.unique_dates,
        "lightgbm_backend": trained.backend.label(),
        "requested_backend": trained.requested_backend.label(),
        "fallback_failures": trained.fallback_failures,
        "valid_mse": mse,
        "valid_ic_spearman": ic,
        "trading_metrics_after_slippage": trading_metrics,
        "feature_importance": feature_importance,
    }))
}

// Handles the ml ablate sp500 CLI action.
pub fn cmd_ml_ablate_sp500(quick: bool, json_out: bool) -> anyhow::Result<()> {
    let conn = open_ml_db()?;
    let with_cols = FEATURE_COLS.to_vec();
    let without_cols = FEATURE_COLS
        .iter()
        .copied()
        .filter(|feature| !SP500_FEATURE_COLS.contains(feature))
        .collect::<Vec<_>>();

    eprintln!("LightGBM S&P 500 feature ablation");
    eprintln!("  Training with S&P 500 features...");
    let with_sp500 = train_lgb_variant(&conn, "with_sp500", &with_cols, quick, !json_out)?;
    eprintln!("  Training without S&P 500 features...");
    let without_sp500 = train_lgb_variant(&conn, "without_sp500", &without_cols, quick, !json_out)?;

    let with_mse = with_sp500["valid_mse"].as_f64().unwrap_or(0.0);
    let without_mse = without_sp500["valid_mse"].as_f64().unwrap_or(0.0);
    let with_ic = with_sp500["valid_ic_spearman"].as_f64().unwrap_or(0.0);
    let without_ic = without_sp500["valid_ic_spearman"].as_f64().unwrap_or(0.0);
    let recommendation = if with_ic > without_ic && with_mse <= without_mse * 1.01 {
        "keep_sp500_features"
    } else if without_ic > with_ic && without_mse <= with_mse * 1.01 {
        "remove_sp500_features"
    } else {
        "mixed_result_review_before_changing"
    };

    let report = serde_json::json!({
        "status": "done",
        "quick": quick,
        "with_sp500": with_sp500,
        "without_sp500": without_sp500,
        "delta": {
            "valid_mse_with_minus_without": with_mse - without_mse,
            "valid_ic_with_minus_without": with_ic - without_ic,
        },
        "recommendation": recommendation,
        "note": "Ablation models are separate artifacts and do not replace the production LightGBM model.",
    });
    let report_path = paths::state_dir().join("sp500_feature_ablation_report.json");
    paths::write_private_file(&report_path, serde_json::to_string_pretty(&report)?)?;

    if json_out {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("S&P 500 Feature Ablation Complete");
        println!("  With S&P 500:    MSE {:.6}, IC {:.4}", with_mse, with_ic);
        println!(
            "  Without S&P 500: MSE {:.6}, IC {:.4}",
            without_mse, without_ic
        );
        println!("  Δ MSE:           {:+.6}", with_mse - without_mse);
        println!("  Δ IC:            {:+.4}", with_ic - without_ic);
        println!("  Recommendation:  {}", recommendation);
        println!("  Report:          {}", report_path.display());
    }

    Ok(())
}

// Parses lgb line from user or provider input.
fn parse_lgb_line(line: &str, feature_count: usize) -> anyhow::Result<(f64, Vec<f64>)> {
    let mut parts = line.split_whitespace();
    let target = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing target"))?
        .parse::<f64>()?;
    let mut features = vec![0.0; feature_count];
    for part in parts {
        let Some((idx, value)) = part.split_once(':') else {
            continue;
        };
        let idx = idx.parse::<usize>()?;
        if idx < feature_count {
            features[idx] = value.parse::<f64>()?;
        }
    }
    Ok((target, features))
}

// Handles solve linear system logic.
fn solve_linear_system(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> anyhow::Result<Vec<f64>> {
    let n = b.len();
    for i in 0..n {
        let mut pivot = i;
        for r in (i + 1)..n {
            if a[r][i].abs() > a[pivot][i].abs() {
                pivot = r;
            }
        }
        if a[pivot][i].abs() < 1e-12 {
            anyhow::bail!("ridge normal equation is singular at column {}", i);
        }
        if pivot != i {
            a.swap(i, pivot);
            b.swap(i, pivot);
        }

        let div = a[i][i];
        for value in &mut a[i][i..] {
            *value /= div;
        }
        b[i] /= div;

        let pivot_row = a[i].clone();
        for (r, row) in a.iter_mut().enumerate() {
            if r == i {
                continue;
            }
            let factor = row[i];
            if factor == 0.0 {
                continue;
            }
            for (value, pivot_value) in row[i..].iter_mut().zip(&pivot_row[i..]) {
                *value -= factor * pivot_value;
            }
            b[r] -= factor * b[i];
        }
    }
    Ok(b)
}

// Handles train ridge from lgb logic.
fn train_ridge_from_lgb(
    path: &std::path::Path,
    feature_count: usize,
    lambda: f64,
) -> anyhow::Result<Vec<f64>> {
    let dim = feature_count + 1;
    let mut xtx = vec![vec![0.0; dim]; dim];
    let mut xty = vec![0.0; dim];
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);

    for line in reader.lines() {
        let (target, mut features) = parse_lgb_line(&line?, feature_count)?;
        features.push(1.0);
        for (i, feature_i) in features.iter().copied().enumerate() {
            xty[i] += feature_i * target;
            for (j, feature_j) in features.iter().copied().enumerate().skip(i) {
                let value = feature_i * feature_j;
                xtx[i][j] += value;
                if i != j {
                    xtx[j][i] += value;
                }
            }
        }
    }

    for (i, row) in xtx.iter_mut().enumerate().take(feature_count) {
        row[i] += lambda;
    }
    solve_linear_system(xtx, xty)
}

// Handles eval linear model logic.
fn eval_linear_model(
    path: &std::path::Path,
    feature_count: usize,
    weights: &[f64],
) -> anyhow::Result<(usize, f64, f64)> {
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    let mut labels = Vec::new();
    let mut preds = Vec::new();

    for line in reader.lines() {
        let (target, features) = parse_lgb_line(&line?, feature_count)?;
        let pred = features
            .iter()
            .zip(weights.iter())
            .map(|(x, w)| x * w)
            .sum::<f64>()
            + weights[feature_count];
        labels.push(target);
        preds.push(pred);
    }

    let mse = preds
        .iter()
        .zip(labels.iter())
        .map(|(p, y)| {
            let err = p - y;
            err * err
        })
        .sum::<f64>()
        / labels.len().max(1) as f64;
    let ic = spearman_corr(&preds, &labels);
    Ok((labels.len(), mse, ic))
}

// Handles predict linear model logic.
fn predict_linear_model(
    path: &std::path::Path,
    feature_count: usize,
    weights: &[f64],
) -> anyhow::Result<Vec<f64>> {
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    let mut preds = Vec::new();

    for line in reader.lines() {
        let (_target, features) = parse_lgb_line(&line?, feature_count)?;
        let pred = features
            .iter()
            .zip(weights.iter())
            .map(|(x, w)| x * w)
            .sum::<f64>()
            + weights[feature_count];
        preds.push(pred);
    }

    Ok(preds)
}

#[cfg(mlai_xgboost)]
// Handles XGBoost check FFI operations.
fn xgb_check(ret: i32) -> anyhow::Result<()> {
    if ret == 0 {
        return Ok(());
    }
    let msg = unsafe {
        let ptr = xgboost_lib_sys::XGBGetLastError();
        if ptr.is_null() {
            "unknown XGBoost error".to_string()
        } else {
            CStr::from_ptr(ptr).to_string_lossy().to_string()
        }
    };
    anyhow::bail!("XGBoost C API error: {}", msg)
}

#[cfg(mlai_xgboost)]
struct XgbDMatrix {
    handle: xgboost_lib_sys::DMatrixHandle,
}

#[cfg(mlai_xgboost)]
impl Drop for XgbDMatrix {
    // Releases owned runtime resources when the wrapper is dropped.
    fn drop(&mut self) {
        unsafe {
            let _ = xgboost_lib_sys::XGDMatrixFree(self.handle);
        }
    }
}

#[cfg(mlai_xgboost)]
struct XgbBooster {
    handle: xgboost_lib_sys::BoosterHandle,
}

#[cfg(mlai_xgboost)]
impl Drop for XgbBooster {
    // Releases owned runtime resources when the wrapper is dropped.
    fn drop(&mut self) {
        unsafe {
            let _ = xgboost_lib_sys::XGBoosterFree(self.handle);
        }
    }
}

#[cfg(mlai_xgboost)]
// Builds an XGBoost DMatrix from dense row-major f32 data.
fn xgb_dmatrix_from_dense(
    features: &[f32],
    rows: usize,
    cols: usize,
    labels: Option<&[f32]>,
) -> anyhow::Result<XgbDMatrix> {
    if rows == 0 {
        anyhow::bail!("XGBoost DMatrix requires at least one row");
    }
    if cols == 0 {
        anyhow::bail!("XGBoost DMatrix requires at least one feature column");
    }
    if features.len() != rows.saturating_mul(cols) {
        anyhow::bail!(
            "XGBoost dense matrix shape mismatch: {} values for {}x{}",
            features.len(),
            rows,
            cols
        );
    }
    if let Some(labels) = labels {
        if labels.len() != rows {
            anyhow::bail!(
                "XGBoost label shape mismatch: {} labels for {} rows",
                labels.len(),
                rows
            );
        }
    }

    let mut handle = std::ptr::null_mut();
    xgb_check(unsafe {
        xgboost_lib_sys::XGDMatrixCreateFromMat(
            features.as_ptr(),
            rows as xgboost_lib_sys::bst_ulong,
            cols as xgboost_lib_sys::bst_ulong,
            f32::NAN,
            &mut handle,
        )
    })?;
    let dmatrix = XgbDMatrix { handle };
    if let Some(labels) = labels {
        let field = CString::new("label")?;
        xgb_check(unsafe {
            xgboost_lib_sys::XGDMatrixSetFloatInfo(
                dmatrix.handle,
                field.as_ptr(),
                labels.as_ptr(),
                labels.len() as xgboost_lib_sys::bst_ulong,
            )
        })?;
    }
    Ok(dmatrix)
}

#[cfg(mlai_xgboost)]
// Loads our LightGBM-format text dataset into an in-memory XGBoost DMatrix.
fn xgb_dmatrix_from_lgb_file(
    path: &std::path::Path,
    feature_count: usize,
) -> anyhow::Result<XgbDMatrix> {
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    let mut labels = Vec::<f32>::new();
    let mut features = Vec::<f32>::with_capacity(feature_count.saturating_mul(1024));

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(label) = parts.next() else {
            continue;
        };
        labels.push(label.parse::<f32>()?);
        let row_start = features.len();
        features.resize(row_start + feature_count, 0.0);
        for part in parts {
            if let Some((idx, value)) = part.split_once(':') {
                let idx = idx.parse::<usize>()?;
                if idx >= feature_count {
                    anyhow::bail!(
                        "Invalid LightGBM feature index {} in {}",
                        idx,
                        path.display()
                    );
                }
                let value = value.parse::<f32>()?;
                if value.is_finite() {
                    features[row_start + idx] = value;
                }
            }
        }
    }

    xgb_dmatrix_from_dense(&features, labels.len(), feature_count, Some(&labels))
}

#[cfg(mlai_xgboost)]
// Builds an XGBoost DMatrix for prediction rows without touching temp files.
fn xgb_dmatrix_from_feature_rows(
    rows: &[ValidationFeatureRow],
    feature_indices: &[usize],
) -> anyhow::Result<XgbDMatrix> {
    let mut features = Vec::with_capacity(rows.len().saturating_mul(feature_indices.len()));
    for row in rows {
        for idx in feature_indices {
            let value = row.features.get(*idx).copied().unwrap_or(0.0);
            features.push(if value.is_finite() { value as f32 } else { 0.0 });
        }
    }
    xgb_dmatrix_from_dense(&features, rows.len(), feature_indices.len(), None)
}

#[cfg(mlai_xgboost)]
// Handles XGBoost set param FFI operations.
fn xgb_set_param(booster: &XgbBooster, name: &str, value: &str) -> anyhow::Result<()> {
    let name = CString::new(name)?;
    let value = CString::new(value)?;
    xgb_check(unsafe {
        xgboost_lib_sys::XGBoosterSetParam(booster.handle, name.as_ptr(), value.as_ptr())
    })
}

#[cfg(mlai_xgboost)]
// Handles XGBoost load model FFI operations.
fn xgb_load_model(path: &std::path::Path) -> anyhow::Result<XgbBooster> {
    let mut handle = std::ptr::null_mut();
    xgb_check(unsafe { xgboost_lib_sys::XGBoosterCreate(std::ptr::null(), 0, &mut handle) })?;
    let booster = XgbBooster { handle };
    let path_c = CString::new(path.to_string_lossy().as_bytes())?;
    xgb_check(unsafe { xgboost_lib_sys::XGBoosterLoadModel(booster.handle, path_c.as_ptr()) })?;
    Ok(booster)
}

#[cfg(mlai_xgboost)]
// Handles XGBoost predict dmatrix FFI operations.
fn xgb_predict_dmatrix(booster: &XgbBooster, dmatrix: &XgbDMatrix) -> anyhow::Result<Vec<f64>> {
    let config = CString::new(
        serde_json::json!({
            "type": 0,
            "training": false,
            "iteration_begin": 0,
            "iteration_end": 0,
            "strict_shape": false,
        })
        .to_string(),
    )?;
    let mut out_shape: *const xgboost_lib_sys::bst_ulong = std::ptr::null();
    let mut out_dim: xgboost_lib_sys::bst_ulong = 0;
    let mut out_result: *const f32 = std::ptr::null();
    xgb_check(unsafe {
        xgboost_lib_sys::XGBoosterPredictFromDMatrix(
            booster.handle,
            dmatrix.handle,
            config.as_ptr(),
            &mut out_shape,
            &mut out_dim,
            &mut out_result,
        )
    })?;
    if out_result.is_null() {
        anyhow::bail!("XGBoost returned null predictions");
    }
    if out_shape.is_null() || out_dim == 0 {
        anyhow::bail!("XGBoost returned invalid prediction shape");
    }
    let shape = unsafe { std::slice::from_raw_parts(out_shape, out_dim as usize) };
    let out_len = shape.iter().try_fold(1usize, |acc, dim| {
        acc.checked_mul(*dim as usize)
            .ok_or_else(|| anyhow::anyhow!("XGBoost prediction shape overflow"))
    })?;
    Ok(unsafe { std::slice::from_raw_parts(out_result, out_len) }
        .iter()
        .map(|value| *value as f64)
        .collect())
}

#[cfg(mlai_xgboost)]
// Handles train xgboost from files logic.
fn train_xgboost_from_files(
    name: &str,
    files: &LgbFiles,
    feature_count: usize,
    quick: bool,
    show_progress: bool,
) -> anyhow::Result<serde_json::Value> {
    train_xgboost_from_files_with_backend(
        name,
        files,
        feature_count,
        quick,
        XgbBackend::from_env(),
        show_progress,
    )
}

#[cfg(mlai_xgboost)]
// Handles train xgboost from files with backend logic.
fn train_xgboost_from_files_with_backend(
    name: &str,
    files: &LgbFiles,
    feature_count: usize,
    quick: bool,
    requested_backend: XgbBackend,
    show_progress: bool,
) -> anyhow::Result<serde_json::Value> {
    let specific_backend_requested = requested_backend != XgbBackend::Auto;
    let mut attempts = match requested_backend {
        XgbBackend::Auto => {
            if cfg!(mlai_nvidia_cuda) {
                vec![XgbBackend::Cuda, XgbBackend::Cpu]
            } else {
                vec![XgbBackend::Cpu]
            }
        }
        backend => vec![backend],
    };
    attempts.dedup();

    let mut failures = Vec::new();
    for backend in attempts {
        match train_xgboost_from_files_once(
            name,
            files,
            feature_count,
            quick,
            backend,
            show_progress,
        ) {
            Ok(mut report) => {
                report["requested_backend"] = serde_json::json!(requested_backend.label());
                if !failures.is_empty() {
                    report["fallback_failures"] = serde_json::json!(failures);
                }
                return Ok(report);
            }
            Err(err) if !specific_backend_requested => {
                eprintln!(
                    "  warning: XGBoost backend '{}' failed cleanly: {}; falling back to CPU.",
                    backend.label(),
                    err
                );
                failures.push(serde_json::json!({
                    "backend": backend.label(),
                    "error": err.to_string(),
                }));
            }
            Err(err) => return Err(err),
        }
    }

    anyhow::bail!("All XGBoost backend attempts failed: {:?}", failures)
}

#[cfg(mlai_xgboost)]
// Handles train xgboost from files once logic.
fn train_xgboost_from_files_once(
    name: &str,
    files: &LgbFiles,
    feature_count: usize,
    quick: bool,
    backend: XgbBackend,
    show_progress: bool,
) -> anyhow::Result<serde_json::Value> {
    let model_path = xgboost_model_path(name);
    let booster = if backend == XgbBackend::Cuda
        && std::env::var("MLAI_TRADE_XGBOOST_CHILD").ok().as_deref() != Some("1")
    {
        train_xgboost_model_with_child(
            name,
            files,
            feature_count,
            quick,
            backend,
            &model_path,
            show_progress,
        )?;
        xgb_load_model(&model_path)?
    } else {
        train_xgboost_model_in_process(
            name,
            files,
            feature_count,
            quick,
            backend,
            &model_path,
            show_progress,
        )?
    };

    let progress =
        crate::progress::spinner_if(show_progress, format!("Evaluating XGBoost: {name}"));
    let valid = xgb_dmatrix_from_lgb_file(&files.valid_path, feature_count)?;
    let preds = xgb_predict_dmatrix(&booster, &valid)?;
    let (labels, _) = load_lgb_text_dataset(&files.valid_path, feature_count)?;
    let mse = preds
        .iter()
        .zip(labels.iter())
        .map(|(p, y)| {
            let err = p - y;
            err * err
        })
        .sum::<f64>()
        / labels.len().max(1) as f64;
    let ic = spearman_corr(&preds, &labels);
    let trading_metrics = trading_metrics_for_predictions(
        name,
        files,
        &preds,
        20,
        DEFAULT_ROUND_TRIP_SPREAD_SLIPPAGE_BPS,
    )?;
    progress.finish_and_clear();

    Ok(serde_json::json!({
        "available": true,
        "backend": "xgboost_lib_sys",
        "xgboost_backend": backend.label(),
        "cpu_threads": if backend == XgbBackend::Cuda {
            serde_json::json!("uncapped_gpu_backend")
        } else {
            serde_json::json!(config::cpu_worker_threads())
        },
        "name": name,
        "target": "fwd_5d_return",
        "rounds": if quick { 100 } else { 500 },
        "model_path": model_path,
        "feature_count": feature_count,
        "valid_mse": mse,
        "valid_ic_spearman": ic,
        "trading_metrics_after_slippage": trading_metrics,
    }))
}

#[cfg(mlai_xgboost)]
// Trains one XGBoost model in-process and saves it to the requested path.
fn train_xgboost_model_in_process(
    name: &str,
    files: &LgbFiles,
    feature_count: usize,
    quick: bool,
    backend: XgbBackend,
    model_path: &Path,
    show_progress: bool,
) -> anyhow::Result<XgbBooster> {
    let progress = crate::progress::spinner_if(
        show_progress,
        format!("Loading XGBoost datasets: {name} ({})", backend.label()),
    );
    let train = xgb_dmatrix_from_lgb_file(&files.train_path, feature_count)?;
    progress.finish_and_clear();
    let dmats = [train.handle];
    let mut handle = std::ptr::null_mut();
    xgb_check(unsafe {
        xgboost_lib_sys::XGBoosterCreate(dmats.as_ptr(), dmats.len() as _, &mut handle)
    })?;
    let booster = XgbBooster { handle };

    let cpu_threads = config::cpu_worker_threads().to_string();
    let nthread = if backend == XgbBackend::Cuda {
        "0"
    } else {
        cpu_threads.as_str()
    };
    for (name, value) in [
        ("objective", "reg:squarederror"),
        ("eval_metric", "rmse"),
        ("booster", "gbtree"),
        ("eta", "0.05"),
        ("max_depth", "6"),
        ("subsample", "0.7"),
        ("colsample_bytree", "0.7"),
        ("lambda", "1.0"),
        ("alpha", "0.1"),
        ("nthread", nthread),
        ("seed", "42"),
    ] {
        xgb_set_param(&booster, name, value)?;
    }
    match backend {
        XgbBackend::Cpu | XgbBackend::Auto => {
            xgb_set_param(&booster, "tree_method", "hist")?;
            xgb_set_param(&booster, "device", "cpu")?;
        }
        XgbBackend::Cuda => {
            xgb_set_param(&booster, "tree_method", "hist")?;
            xgb_set_param(&booster, "device", "cuda")?;
        }
    }

    let rounds = if quick { 100 } else { 500 };
    let progress = crate::progress::bar_if(
        show_progress,
        rounds as u64,
        format!("Training XGBoost: {name} ({})", backend.label()),
    );
    for iter in 0..rounds {
        xgb_check(unsafe {
            xgboost_lib_sys::XGBoosterUpdateOneIter(booster.handle, iter, train.handle)
        })?;
        progress.set_position((iter + 1) as u64);
    }
    progress.finish_and_clear();

    let model_path_c = CString::new(model_path.to_string_lossy().as_bytes())?;
    xgb_check(unsafe {
        xgboost_lib_sys::XGBoosterSaveModel(booster.handle, model_path_c.as_ptr())
    })?;

    Ok(booster)
}

#[cfg(mlai_xgboost)]
// Trains XGBoost through a child process so native CUDA crashes can fall back.
fn train_xgboost_model_with_child(
    name: &str,
    files: &LgbFiles,
    feature_count: usize,
    quick: bool,
    backend: XgbBackend,
    model_path: &Path,
    show_progress: bool,
) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let progress = crate::progress::spinner_if(
        show_progress,
        format!("Training XGBoost: {name} ({})", backend.label()),
    );
    let mut command = Command::new(exe);
    command
        .arg("ml")
        .arg("__xgboost-train-child")
        .arg("--name")
        .arg(name)
        .arg("--train-path")
        .arg(&files.train_path)
        .arg("--valid-path")
        .arg(&files.valid_path)
        .arg("--feature-count")
        .arg(feature_count.to_string())
        .arg("--backend")
        .arg(backend.label())
        .arg("--model-path")
        .arg(model_path)
        .env("MLAI_TRADE_XGBOOST_CHILD", "1")
        .env("LD_LIBRARY_PATH", packaged_library_path()?);
    if quick {
        command.arg("--quick");
    }
    let output = command
        .output()
        .map_err(|err| anyhow::anyhow!("failed to run XGBoost child process: {err}"));
    progress.finish_and_clear();
    let output = output?;
    if !output.status.success() {
        anyhow::bail!(
            "XGBoost child backend '{}' failed with status {}: stdout={} stderr={}",
            backend.label(),
            process_status_label(output.status),
            compact_process_output(&output.stdout),
            compact_process_output(&output.stderr)
        );
    }
    if !model_path.is_file() {
        anyhow::bail!(
            "XGBoost child backend '{}' did not produce model file {}",
            backend.label(),
            model_path.display()
        );
    }
    Ok(())
}

#[cfg(mlai_xgboost)]
// Returns the model path used by XGBoost training variants.
fn xgboost_model_path(name: &str) -> PathBuf {
    if name == "xgboost" {
        paths::state_dir().join("xgboost_baseline_model.json")
    } else {
        paths::state_dir().join(format!("{name}_model.json"))
    }
}

pub struct XgboostTrainChildRequest {
    pub name: String,
    pub train_path: PathBuf,
    pub valid_path: PathBuf,
    pub feature_count: usize,
    pub quick: bool,
    pub backend: String,
    pub model_path: PathBuf,
    pub json_out: bool,
}

#[cfg(mlai_xgboost)]
// Hidden CLI entrypoint used only for process-isolated XGBoost CUDA training.
pub fn cmd_ml_xgboost_train_child(request: XgboostTrainChildRequest) -> anyhow::Result<()> {
    let XgboostTrainChildRequest {
        name,
        train_path,
        valid_path,
        feature_count,
        quick,
        backend,
        model_path,
        json_out,
    } = request;
    let backend = match backend.as_str() {
        "cpu" => XgbBackend::Cpu,
        "cuda" | "gpu" => XgbBackend::Cuda,
        other => anyhow::bail!("unsupported XGBoost child backend '{}'", other),
    };
    let files = LgbFiles {
        train_path,
        valid_path,
        train_rows: 0,
        valid_rows: 0,
        train_candidate_rows: 0,
        valid_candidate_rows: 0,
        train_stride: 1,
        valid_stride: 1,
        valid_start: "1970-01-01".to_string(),
        valid_end: None,
        date_start: "1970-01-01".to_string(),
        date_end: "1970-01-01".to_string(),
        unique_dates: 0,
    };
    let _booster = train_xgboost_model_in_process(
        &name,
        &files,
        feature_count,
        quick,
        backend,
        &model_path,
        !json_out,
    )?;
    if json_out {
        println!(
            "{}",
            serde_json::json!({
                "status": "ok",
                "backend": backend.label(),
                "model_path": model_path,
            })
        );
    } else {
        println!(
            "XGBoost child training complete: backend={} model={}",
            backend.label(),
            model_path.display()
        );
    }
    Ok(())
}

#[cfg(not(mlai_xgboost))]
// Hidden CLI entrypoint used only for process-isolated XGBoost CUDA training.
pub fn cmd_ml_xgboost_train_child(_request: XgboostTrainChildRequest) -> anyhow::Result<()> {
    anyhow::bail!("XGBoost is not available in this build.")
}

#[cfg(mlai_xgboost)]
// Handles train xgboost baseline logic.
fn train_xgboost_baseline(
    files: &LgbFiles,
    quick: bool,
    show_progress: bool,
) -> anyhow::Result<serde_json::Value> {
    train_xgboost_from_files("xgboost", files, FEATURE_COLS.len(), quick, show_progress)
}

#[cfg(not(mlai_xgboost))]
// Handles train xgboost baseline logic.
fn train_xgboost_baseline(
    _files: &LgbFiles,
    _quick: bool,
    _show_progress: bool,
) -> anyhow::Result<serde_json::Value> {
    Ok(serde_json::json!({
        "available": false,
        "status": "not_available_on_this_os",
        "note": "XGBoost is mandatory on macOS and Linux builds. This target uses the portable CPU baseline without XGBoost."
    }))
}

// Handles the ml baselines CLI action.
pub fn cmd_ml_baselines(_quick: bool, json_out: bool) -> anyhow::Result<()> {
    let conn = open_ml_db()?;
    init_ml_tables(&conn)?;
    let files = write_lgb_training_files(&conn, !json_out)?;
    let lambda = 10.0;
    eprintln!(
        "Training Ridge baseline from {} rows and {} features...",
        files.train_rows,
        FEATURE_COLS.len()
    );
    let progress = crate::progress::spinner_if(!json_out, "Training Ridge baseline");
    let weights = train_ridge_from_lgb(&files.train_path, FEATURE_COLS.len(), lambda)?;
    let (valid_rows, valid_mse, valid_ic) =
        eval_linear_model(&files.valid_path, FEATURE_COLS.len(), &weights)?;
    let ridge_preds = predict_linear_model(&files.valid_path, FEATURE_COLS.len(), &weights)?;
    let ridge_trading_metrics = trading_metrics_for_predictions(
        "ridge",
        &files,
        &ridge_preds,
        20,
        DEFAULT_ROUND_TRIP_SPREAD_SLIPPAGE_BPS,
    )?;
    progress.finish_and_clear();
    let xgboost = match train_xgboost_baseline(&files, _quick, !json_out) {
        Ok(report) => report,
        Err(err) => serde_json::json!({
            "available": false,
            "status": "xgboost_c_api_failed",
            "error": err.to_string(),
        }),
    };
    let report = serde_json::json!({
        "feature_count": FEATURE_COLS.len(),
        "target": "fwd_5d_return",
        "train_rows": files.train_rows,
        "train_candidate_rows": files.train_candidate_rows,
        "valid_rows": valid_rows,
        "valid_candidate_rows": files.valid_candidate_rows,
        "train_stride": files.train_stride,
        "valid_stride": files.valid_stride,
        "ridge": {
            "lambda_l2": lambda,
            "valid_mse": valid_mse,
            "valid_ic_spearman": valid_ic,
            "trading_metrics_after_slippage": ridge_trading_metrics
        },
        "xgboost": xgboost
    });
    let report_path = paths::state_dir().join("ml_baseline_report.json");
    paths::write_private_file(&report_path, serde_json::to_string_pretty(&report)?)?;

    if json_out {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("📏 ML Baselines");
        println!("  Features:     {}", FEATURE_COLS.len());
        println!("  Train rows:   {}", files.train_rows);
        println!("  Valid rows:   {}", valid_rows);
        println!("  Ridge MSE:    {:.6}", valid_mse);
        println!("  Ridge IC:     {:.4}", valid_ic);
        println!("  XGBoost:      see report");
        println!("  Report:       {}", report_path.display());
    }

    Ok(())
}

// Handles the ml xgboost ablate sp500 CLI action.
pub fn cmd_ml_xgboost_ablate_sp500(quick: bool, json_out: bool) -> anyhow::Result<()> {
    #[cfg(mlai_xgboost)]
    {
        let conn = open_ml_db()?;
        init_ml_tables(&conn)?;
        let without_cols = without_sp500_feature_cols();
        let state_dir = paths::ensure_state_dir()?;
        let files = write_lgb_training_files_for_cols(
            &conn,
            &without_cols,
            state_dir.join("xgboost_without_sp500_training_dataset.txt"),
            state_dir.join("xgboost_without_sp500_validation_dataset.txt"),
            !json_out,
        )?;
        eprintln!(
            "Training XGBoost without S&P 500 features from {} rows and {} features...",
            files.train_rows,
            without_cols.len()
        );
        let without_sp500 = train_xgboost_from_files(
            "xgboost_without_sp500",
            &files,
            without_cols.len(),
            quick,
            !json_out,
        )?;
        let _ = std::fs::remove_file(&files.train_path);
        let _ = std::fs::remove_file(&files.valid_path);

        let report = serde_json::json!({
            "status": "done",
            "quick": quick,
            "without_sp500": without_sp500,
        });
        let report_path = paths::state_dir().join("xgboost_sp500_ablation_report.json");
        paths::write_private_file(&report_path, serde_json::to_string_pretty(&report)?)?;

        if json_out {
            println!("{}", serde_json::to_string(&report)?);
        } else {
            println!("XGBoost S&P 500 Ablation Complete");
            println!(
                "  Without S&P 500: MSE {:.6}, IC {:.4}",
                report["without_sp500"]["valid_mse"].as_f64().unwrap_or(0.0),
                report["without_sp500"]["valid_ic_spearman"]
                    .as_f64()
                    .unwrap_or(0.0)
            );
            println!("  Report:          {}", report_path.display());
        }
        Ok(())
    }
    #[cfg(not(mlai_xgboost))]
    {
        let _ = (quick, json_out);
        anyhow::bail!("XGBoost support is not available on this operating system.")
    }
}

// Handles walk forward years logic.
fn walk_forward_years(conn: &Connection, max_folds: usize) -> anyhow::Result<Vec<i32>> {
    let mut stmt = conn.prepare(
        "SELECT CAST(substr(f.date, 1, 4) AS INTEGER) AS yr, COUNT(DISTINCT f.date) AS n_dates
         FROM ml_features f
         INNER JOIN ml_labels l ON f.symbol = l.symbol AND f.date = l.date
         WHERE f.return_1d IS NOT NULL
           AND f.volatility_20d IS NOT NULL
           AND l.fwd_5d IS NOT NULL
         GROUP BY yr
         HAVING n_dates >= 100
         ORDER BY yr",
    )?;
    let years = stmt
        .query_map([], |r| Ok((r.get::<_, i32>(0)?, r.get::<_, i64>(1)?)))?
        .filter_map(|r| r.ok())
        .map(|(year, _)| year)
        .collect::<Vec<_>>();

    let mut eligible = Vec::new();
    for year in years {
        let train_dates: i64 = conn.query_row(
            "SELECT COUNT(DISTINCT f.date)
             FROM ml_features f
             INNER JOIN ml_labels l ON f.symbol = l.symbol AND f.date = l.date
             WHERE f.date < ?1
               AND f.return_1d IS NOT NULL
               AND f.volatility_20d IS NOT NULL
               AND l.fwd_5d IS NOT NULL",
            params![format!("{year}-01-01")],
            |r| r.get(0),
        )?;
        if train_dates >= 252 {
            eligible.push(year);
        }
    }

    let keep = max_folds.max(1);
    if eligible.len() > keep {
        Ok(eligible[eligible.len() - keep..].to_vec())
    } else {
        Ok(eligible)
    }
}

// Handles the ml walk forward CLI action.
pub fn cmd_ml_walk_forward(quick: bool, max_folds: usize, json_out: bool) -> anyhow::Result<()> {
    let conn = open_ml_db()?;
    init_ml_tables(&conn)?;
    let state_dir = paths::ensure_state_dir()?;
    let years = walk_forward_years(&conn, max_folds)?;
    if years.is_empty() {
        anyhow::bail!("No eligible walk-forward validation years found.");
    }

    eprintln!(
        "Walk-forward validation over {} yearly folds, target=fwd_5d_return",
        years.len()
    );

    let mut fold_reports = Vec::new();
    let mut lgb_mse_sum = 0.0;
    let mut lgb_ic_sum = 0.0;
    let mut ridge_mse_sum = 0.0;
    let mut ridge_ic_sum = 0.0;
    let fold_progress =
        crate::progress::bar_if(!json_out, years.len() as u64, "Walk-forward folds");

    for year in years {
        let valid_start = format!("{year}-01-01");
        let valid_end = format!("{}-01-01", year + 1);
        let name = format!("walk_forward_{year}");
        fold_progress.set_message(format!("validating {year}"));
        eprintln!(
            "  Fold {}: train < {}, validate {}",
            year, valid_start, year
        );
        let files = write_lgb_training_files_for_cols_and_dates(
            &conn,
            FEATURE_COLS,
            state_dir.join(format!("{name}_training_dataset.txt")),
            state_dir.join(format!("{name}_validation_dataset.txt")),
            &valid_start,
            Some(&valid_end),
            !json_out,
        )?;

        let lgb = train_lgb_from_files(&name, FEATURE_COLS, &files, quick, false, !json_out)?;
        let lgb_mse = lgb["valid_mse"].as_f64().unwrap_or(0.0);
        let lgb_ic = lgb["valid_ic_spearman"].as_f64().unwrap_or(0.0);

        let ridge_weights = train_ridge_from_lgb(&files.train_path, FEATURE_COLS.len(), 10.0)?;
        let (_, ridge_mse, ridge_ic) =
            eval_linear_model(&files.valid_path, FEATURE_COLS.len(), &ridge_weights)?;
        let ridge_preds =
            predict_linear_model(&files.valid_path, FEATURE_COLS.len(), &ridge_weights)?;
        let ridge_trading_metrics = trading_metrics_for_predictions(
            "ridge",
            &files,
            &ridge_preds,
            20,
            DEFAULT_ROUND_TRIP_SPREAD_SLIPPAGE_BPS,
        )?;

        let xgboost = match train_xgboost_baseline(&files, quick, !json_out) {
            Ok(report) => report,
            Err(err) => serde_json::json!({
                "available": false,
                "status": "xgboost_c_api_failed",
                "error": err.to_string(),
            }),
        };

        lgb_mse_sum += lgb_mse;
        lgb_ic_sum += lgb_ic;
        ridge_mse_sum += ridge_mse;
        ridge_ic_sum += ridge_ic;

        fold_reports.push(serde_json::json!({
            "year": year,
            "target": "fwd_5d_return",
            "train_rows": files.train_rows,
            "valid_rows": files.valid_rows,
            "lightgbm": {
                "valid_mse": lgb_mse,
                "valid_ic_spearman": lgb_ic
            },
            "ridge": {
                "lambda_l2": 10.0,
                "valid_mse": ridge_mse,
                "valid_ic_spearman": ridge_ic,
                "trading_metrics_after_slippage": ridge_trading_metrics
            },
            "xgboost": xgboost,
        }));

        let _ = std::fs::remove_file(files.train_path);
        let _ = std::fs::remove_file(files.valid_path);
        fold_progress.inc(1);
    }
    fold_progress.finish_and_clear();

    let n = fold_reports.len().max(1) as f64;
    let report = serde_json::json!({
        "target": "fwd_5d_return",
        "feature_count": FEATURE_COLS.len(),
        "folds": fold_reports,
        "summary": {
            "fold_count": n as usize,
            "lightgbm": {
                "mean_valid_mse": lgb_mse_sum / n,
                "mean_valid_ic_spearman": lgb_ic_sum / n
            },
            "ridge": {
                "mean_valid_mse": ridge_mse_sum / n,
                "mean_valid_ic_spearman": ridge_ic_sum / n
            }
        }
    });
    let report_path = state_dir.join("ml_walk_forward_report.json");
    paths::write_private_file(&report_path, serde_json::to_string_pretty(&report)?)?;

    if json_out {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("📈 Walk-Forward Validation");
        println!("  Target:       fwd_5d_return");
        println!("  Features:     {}", FEATURE_COLS.len());
        println!("  Folds:        {}", n as usize);
        println!("  LGB MSE:      {:.6}", lgb_mse_sum / n);
        println!("  LGB IC:       {:.4}", lgb_ic_sum / n);
        println!("  Ridge MSE:    {:.6}", ridge_mse_sum / n);
        println!("  Ridge IC:     {:.4}", ridge_ic_sum / n);
        println!("  Report:       {}", report_path.display());
    }

    Ok(())
}

// Handles the ml train CLI action.
pub fn cmd_ml_train(quick: bool, backtest_only: bool, json_out: bool) -> anyhow::Result<()> {
    let _ = paths::ensure_state_dir()?;
    let conn = open_ml_db()?;
    eprintln!("LightGBM native Rust training");
    eprintln!("  Streaming SQLite rows into LightGBM text datasets...");
    let files = write_lgb_training_files(&conn, !json_out)?;
    eprintln!("  Train rows: {}", files.train_rows);
    eprintln!("  Valid rows: {}", files.valid_rows);
    eprintln!("  Train file: {}", files.train_path.display());
    eprintln!("  Valid file: {}", files.valid_path.display());

    if backtest_only {
        anyhow::bail!(
            "Native Rust backtest-only mode is not implemented yet. Use full `mlai-trade ml train` to train LightGBM and write results."
        );
    }

    let trained =
        train_lgb_booster_from_files("production", FEATURE_COLS, &files, quick, !json_out)?;
    let booster = trained.booster;

    let model_path = paths::ml_model_path();
    booster
        .save_file(&model_path.to_string_lossy())
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let importances = booster
        .feature_importance(ImportanceType::Gain)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let feature_importance = FEATURE_COLS
        .iter()
        .zip(importances.iter())
        .map(|(name, value)| ((*name).to_string(), *value))
        .collect::<HashMap<_, _>>();
    let progress = crate::progress::spinner_if(!json_out, "Evaluating LightGBM production model");
    let (labels, features) = load_lgb_text_dataset(&files.valid_path, FEATURE_COLS.len())?;
    let preds = booster
        .predict(&features, FEATURE_COLS.len() as i32, true)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let valid_mse = preds
        .iter()
        .zip(labels.iter())
        .map(|(p, y)| {
            let err = p - y;
            err * err
        })
        .sum::<f64>()
        / labels.len().max(1) as f64;
    let valid_ic = spearman_corr(&preds, &labels);
    let trading_metrics = trading_metrics_for_predictions(
        "lightgbm",
        &files,
        &preds,
        20,
        DEFAULT_ROUND_TRIP_SPREAD_SLIPPAGE_BPS,
    )?;
    progress.finish_and_clear();
    let results = serde_json::json!({
        "status": "done",
        "engine": "rust-lightgbm3",
        "model_path": model_path.display().to_string(),
        "train_rows": files.train_rows,
        "valid_rows": files.valid_rows,
        "train_candidate_rows": files.train_candidate_rows,
        "valid_candidate_rows": files.valid_candidate_rows,
        "train_stride": files.train_stride,
        "valid_stride": files.valid_stride,
        "cpu_threads": config::cpu_worker_threads(),
        "lightgbm_backend": trained.backend.label(),
        "requested_backend": trained.requested_backend.label(),
        "fallback_failures": trained.fallback_failures,
        "features": FEATURE_COLS.len(),
        "date_start": files.date_start,
        "date_end": files.date_end,
        "unique_dates": files.unique_dates,
        "valid_mse": valid_mse,
        "valid_ic_spearman": valid_ic,
        "trading_metrics_after_slippage": trading_metrics,
        "feature_importance": feature_importance,
        "note": "Native Rust LightGBM training uses streamed .svm files from SQLite to avoid holding the full matrix in memory.",
    });
    let results_path = paths::lightgbm_training_report_path();
    paths::write_private_file(&results_path, serde_json::to_string_pretty(&results)?)?;

    if json_out {
        println!("{}", serde_json::to_string(&results)?);
    } else {
        println!("LightGBM training complete");
        println!("  Engine:  rust-lightgbm3");
        println!("  Backend: {}", trained.backend.label());
        println!("  Train:   {} rows", files.train_rows);
        println!("  Valid:   {} rows", files.valid_rows);
        println!("  Threads: {}", config::cpu_worker_threads());
        println!("  Model:   {}", model_path.display());
        println!("  Results: {}", results_path.display());
    }
    Ok(())
}

// ══════════════════════════════════════════════════════════════════
// Pure Rust LightGBM model loader + inference engine
// Parses the text format directly — no Python, no C bindings
// ══════════════════════════════════════════════════════════════════

/// A single decision tree node
#[derive(Debug, Clone)]
struct TreeNode {
    split_feature: i32, // -1 for leaf
    threshold: f64,
    left_child: i32, // negative = leaf index (-(idx+1))
    right_child: i32,
}

/// A parsed decision tree
#[derive(Debug, Clone)]
struct Tree {
    nodes: Vec<TreeNode>,
    leaf_values: Vec<f64>,
    shrinkage: f64,
}

/// A loaded LightGBM model
struct LgbModel {
    trees: Vec<Tree>,
}

impl LgbModel {
    // Handles load logic.
    fn load(path: &str) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let mut trees = Vec::new();
        let mut num_features = 0usize;

        // Parse header
        for line in content.lines() {
            if line.starts_with("max_feature_idx=") {
                num_features = line
                    .trim_start_matches("max_feature_idx=")
                    .parse::<usize>()?
                    + 1;
                break;
            }
        }

        // Parse trees
        let tree_blocks: Vec<&str> = content.split("\nTree=").collect();
        for (i, block) in tree_blocks.iter().enumerate() {
            if i == 0 {
                continue;
            } // skip header

            let mut num_leaves = 0usize;
            let mut split_features: Vec<i32> = Vec::new();
            let mut thresholds: Vec<f64> = Vec::new();
            let mut left_children: Vec<i32> = Vec::new();
            let mut right_children: Vec<i32> = Vec::new();
            let mut leaf_values: Vec<f64> = Vec::new();
            let mut shrinkage = 0.05f64;

            for line in block.lines() {
                if line.starts_with("num_leaves=") {
                    num_leaves = line.trim_start_matches("num_leaves=").parse()?;
                } else if line.starts_with("split_feature=") {
                    split_features = line
                        .trim_start_matches("split_feature=")
                        .split_whitespace()
                        .map(|s| s.parse().unwrap_or(0))
                        .collect();
                } else if line.starts_with("threshold=") {
                    thresholds = line
                        .trim_start_matches("threshold=")
                        .split_whitespace()
                        .map(|s| s.parse().unwrap_or(0.0))
                        .collect();
                } else if line.starts_with("left_child=") {
                    left_children = line
                        .trim_start_matches("left_child=")
                        .split_whitespace()
                        .map(|s| s.parse().unwrap_or(0))
                        .collect();
                } else if line.starts_with("right_child=") {
                    right_children = line
                        .trim_start_matches("right_child=")
                        .split_whitespace()
                        .map(|s| s.parse().unwrap_or(0))
                        .collect();
                } else if line.starts_with("leaf_value=") {
                    leaf_values = line
                        .trim_start_matches("leaf_value=")
                        .split_whitespace()
                        .map(|s| s.parse().unwrap_or(0.0))
                        .collect();
                } else if line.starts_with("shrinkage=") {
                    shrinkage = line
                        .trim_start_matches("shrinkage=")
                        .parse()
                        .unwrap_or(0.05);
                }
            }

            if split_features.is_empty() || leaf_values.is_empty() {
                continue;
            }

            let num_internal = num_leaves - 1;
            let mut nodes = Vec::with_capacity(num_internal);
            for j in 0..num_internal {
                nodes.push(TreeNode {
                    split_feature: *split_features.get(j).unwrap_or(&0),
                    threshold: *thresholds.get(j).unwrap_or(&0.0),
                    left_child: *left_children.get(j).unwrap_or(&0),
                    right_child: *right_children.get(j).unwrap_or(&0),
                });
            }

            trees.push(Tree {
                nodes,
                leaf_values,
                shrinkage,
            });
        }

        eprintln!(
            "  Loaded model: {} trees, {} features",
            trees.len(),
            num_features
        );
        Ok(LgbModel { trees })
    }

    // Handles predict one logic.
    fn predict_one(&self, features: &[f64]) -> f64 {
        let mut score = 0.0;
        for tree in &self.trees {
            let leaf_val = self.traverse_tree(tree, features);
            score += tree.shrinkage * leaf_val;
        }
        score
    }

    // Handles traverse tree logic.
    fn traverse_tree(&self, tree: &Tree, features: &[f64]) -> f64 {
        let mut node_idx: i32 = 0;
        loop {
            if node_idx < 0 {
                // Leaf: index is -(node_idx + 1)
                let leaf_idx = (-node_idx - 1) as usize;
                return *tree.leaf_values.get(leaf_idx).unwrap_or(&0.0);
            }
            let idx = node_idx as usize;
            if idx >= tree.nodes.len() {
                return 0.0; // safety
            }
            let node = &tree.nodes[idx];
            let feat_val = if (node.split_feature as usize) < features.len() {
                features[node.split_feature as usize]
            } else {
                0.0
            };

            // LightGBM: decision_type=2 means numerical, go left if <= threshold
            if feat_val <= node.threshold {
                node_idx = node.left_child;
            } else {
                node_idx = node.right_child;
            }
        }
    }
}

// ── CMD: ml predict (pure Rust — no Python dependency) ───────────

pub fn cmd_ml_predict(json: bool) -> anyhow::Result<()> {
    let model_path = paths::ml_model_path();

    if !model_path.exists() {
        anyhow::bail!(
            "Model not found: {} — run 'mlai-trade ml refresh' first",
            model_path.display()
        );
    }

    eprintln!("Loading model...");
    let model_path_str = model_path.to_string_lossy().to_string();
    let model = LgbModel::load(&model_path_str)?;

    let conn = open_ml_db()?;
    init_ml_tables(&conn)?;

    // Get latest date with features
    let latest_date: String = conn.query_row(
        "SELECT COALESCE(MAX(date),'none') FROM ml_features WHERE return_1d IS NOT NULL",
        [],
        |r| r.get(0),
    )?;

    if latest_date == "none" {
        anyhow::bail!("No features found. Run 'mlai-trade ml features' first.");
    }

    eprintln!("Predicting for date {}...", latest_date);

    // Load features for latest date
    let feature_cols = FEATURE_COLS.join(", ");
    let asset_join = ml_eligible_asset_join("f", "a");
    let eligible = ml_eligible_asset_predicate("f.symbol", "a");
    let count_query = format!(
        "SELECT COUNT(*)
         FROM ml_features f
         {asset_join}
         WHERE f.date = ?1
           AND f.return_1d IS NOT NULL
           AND {eligible}"
    );
    let total_rows: i64 = conn.query_row(&count_query, params![&latest_date], |r| r.get(0))?;
    let progress = crate::progress::bar_if(!json, total_rows.max(0) as u64, "LightGBM predictions");
    let query = format!(
        "SELECT f.symbol, {feature_cols}
         FROM ml_features f
         {asset_join}
         WHERE f.date = ?1
           AND f.return_1d IS NOT NULL
           AND {eligible}
         ORDER BY f.symbol"
    );
    let mut stmt = conn.prepare(&query)?;
    let mut scored: Vec<(String, f64)> = Vec::new();

    let rows = stmt.query_map(params![&latest_date], |r| {
        let sym: String = r.get(0)?;
        let mut feats = Vec::with_capacity(FEATURE_COLS.len());
        for i in 0..FEATURE_COLS.len() {
            let v: Option<f64> = r.get(i + 1)?;
            feats.push(v.unwrap_or(0.0)); // Replace NULL with 0
        }
        Ok((sym, feats))
    })?;

    for row in rows {
        let (sym, feats) = row?;
        if config::is_blocked_symbol(&sym) {
            progress.inc(1);
            continue;
        }
        scored.push((sym, model.predict_one(&feats)));
        progress.inc(1);
    }
    progress.finish_and_clear();

    if scored.is_empty() {
        anyhow::bail!("No features for date {}", latest_date);
    }

    // Assign quintiles
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let n = scored.len();
    let q_size = (n / 5).max(1);

    // Store in DB
    let model_version = chrono::Utc::now().format("%Y%m%d").to_string();
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM ml_predictions WHERE date = ?1",
        params![latest_date],
    )?;
    {
        let mut ins = tx.prepare_cached(
            "INSERT INTO ml_predictions (symbol, date, predicted_score, predicted_quintile, model_version)
             VALUES (?1, ?2, ?3, ?4, ?5)"
        )?;
        for (rank, (symbol, score)) in scored.iter().enumerate() {
            let quintile = std::cmp::min((rank / q_size) as i32 + 1, 5);
            ins.execute(params![symbol, latest_date, score, quintile, model_version])?;
        }
    }
    tx.commit()?;

    // Build output
    let mut predictions: Vec<serde_json::Value> = Vec::new();
    for (rank, (symbol, score)) in scored.iter().take(30).enumerate() {
        let quintile = std::cmp::min((rank / q_size) as i32 + 1, 5);
        predictions.push(serde_json::json!({
            "symbol": symbol,
            "score": (score * 10000.0).round() / 10000.0,
            "quintile": quintile,
        }));
    }

    if json {
        println!(
            "{}",
            serde_json::json!({
                "status": "done",
                "date": latest_date,
                "total": n,
                "model_version": model_version,
                "engine": "rust",
                "trees": model.trees.len(),
                "predictions": predictions,
            })
        );
    } else {
        println!("🤖 ML Stock Rankings (Top 20) — Pure Rust Engine");
        println!("{:<8} {:>10} {:>8}", "Symbol", "Score", "Quintile");
        println!("{}", "─".repeat(30));
        for (rank, (symbol, score)) in scored.iter().take(20).enumerate() {
            let quintile = std::cmp::min((rank / q_size) as i32 + 1, 5);
            println!("{:<8} {:>10.4} {:>8}", symbol, score, quintile);
        }
        println!(
            "\nTotal predictions: {} | Trees: {} | Engine: Rust",
            n,
            model.trees.len()
        );
    }

    Ok(())
}

// Handles the ml xgboost predict CLI action.
pub fn cmd_ml_xgboost_predict(json: bool) -> anyhow::Result<()> {
    let model_path = paths::state_dir().join("xgboost_baseline_model.json");
    if !model_path.exists() {
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "status": "skipped",
                    "reason": "xgboost_model_not_found",
                    "model_path": model_path.display().to_string(),
                })
            );
        } else {
            eprintln!(
                "  warning: XGBoost prediction skipped; model not found: {}",
                model_path.display()
            );
        }
        return Ok(());
    }

    let conn = open_ml_db()?;
    init_ml_tables(&conn)?;
    init_ensemble_columns(&conn)?;

    let (latest_date, rows) = latest_eligible_feature_rows(&conn)?;

    let progress = crate::progress::spinner_if(!json, "Running XGBoost predictions");
    let preds =
        xgb_predict_feature_rows(&model_path, &rows, &feature_indices_for_cols(FEATURE_COLS))?;
    progress.finish_and_clear();
    let tx = conn.unchecked_transaction()?;
    {
        let mut upd = tx.prepare_cached(
            "UPDATE ml_predictions SET xgb_score = ?1 WHERE symbol = ?2 AND date = ?3",
        )?;
        let progress =
            crate::progress::bar_if(!json, preds.len() as u64, "Writing XGBoost predictions");
        for (row, score) in rows.iter().zip(preds.iter()) {
            upd.execute(params![score, row.symbol, latest_date])?;
            progress.inc(1);
        }
        progress.finish_and_clear();
    }
    tx.commit()?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "status": "done",
                "date": latest_date,
                "rows": preds.len(),
                "model_path": model_path.display().to_string(),
            })
        );
    } else {
        println!(
            "✅ XGBoost predictions refreshed: {} rows for {}",
            preds.len(),
            latest_date
        );
    }
    Ok(())
}

// Handles the ml evaluate latest CLI action.
pub fn cmd_ml_evaluate_latest(
    json_out: bool,
    top_n: usize,
    slippage_bps: f64,
) -> anyhow::Result<serde_json::Value> {
    let conn = open_ml_db()?;
    init_ml_tables(&conn)?;

    let latest_date: String = conn.query_row(
        "SELECT COALESCE(MAX(date),'none') FROM ml_predictions WHERE ensemble_score IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    if latest_date == "none" {
        anyhow::bail!("No ensemble predictions found. Run `mlai-trade ml ensemble` first.");
    }

    let mut stmt = conn.prepare(
        "SELECT p.symbol, p.lgb_score, p.lstm_score, p.ensemble_score, l.fwd_5d
         FROM ml_predictions p
         INNER JOIN ml_labels l ON p.symbol = l.symbol AND p.date = l.date
         WHERE p.date = ?1
           AND p.lgb_score IS NOT NULL
           AND p.lstm_score IS NOT NULL
           AND p.ensemble_score IS NOT NULL
           AND l.fwd_5d IS NOT NULL",
    )?;
    let rows = stmt
        .query_map(params![latest_date], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, f64>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if rows.is_empty() {
        anyhow::bail!(
            "No forward-return labels available yet for latest prediction date {}. Re-run after at least five trading days.",
            latest_date
        );
    }

    let to_scored = |score_idx: usize| -> Vec<ScoredReturn> {
        rows.iter()
            .map(|(symbol, lgb, lstm, ensemble, fwd_return)| {
                let score = match score_idx {
                    0 => *lgb,
                    1 => *lstm,
                    _ => *ensemble,
                };
                ScoredReturn {
                    symbol: symbol.clone(),
                    date: latest_date.clone(),
                    score,
                    fwd_return: *fwd_return,
                }
            })
            .collect()
    };

    let models = vec![
        trading_metrics_json("lightgbm_latest", &to_scored(0), top_n, slippage_bps),
        trading_metrics_json("lstm_latest", &to_scored(1), top_n, slippage_bps),
        trading_metrics_json("ensemble_latest", &to_scored(2), top_n, slippage_bps),
    ];
    let mut ordered = models.clone();
    ordered.sort_by(|a, b| {
        b["avg_net_5d_return"]
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&a["avg_net_5d_return"].as_f64().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let report = serde_json::json!({
        "status": "done",
        "date": latest_date,
        "top_n_per_date": top_n,
        "round_trip_slippage_bps": slippage_bps,
        "models": models,
        "ordered_by_avg_net_5d_return": ordered,
        "note": "Only available for prediction dates that already have fwd_5d labels. Latest live dates usually cannot be evaluated until five trading days later."
    });
    let report_path = paths::state_dir().join("ml_latest_trading_evaluation_report.json");
    paths::write_private_file(&report_path, serde_json::to_string_pretty(&report)?)?;

    if json_out {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("📈 Latest Prediction Trading Evaluation");
        println!("  Date:   {}", report["date"].as_str().unwrap_or("unknown"));
        println!("  Report: {}", report_path.display());
    }

    Ok(report)
}

// ── CMD: ml status ───────────────────────────────────────────────

pub fn cmd_ml_status(json: bool) -> anyhow::Result<()> {
    let conn = open_ml_db()?;
    init_ml_tables(&conn)?;
    init_ensemble_columns(&conn)?;

    let feat_count: i64 = conn.query_row("SELECT COUNT(*) FROM ml_features", [], |r| r.get(0))?;
    let feat_dates: i64 =
        conn.query_row("SELECT COUNT(DISTINCT date) FROM ml_features", [], |r| {
            r.get(0)
        })?;
    let feat_syms: i64 =
        conn.query_row("SELECT COUNT(DISTINCT symbol) FROM ml_features", [], |r| {
            r.get(0)
        })?;
    let label_count: i64 = conn.query_row("SELECT COUNT(*) FROM ml_labels", [], |r| r.get(0))?;
    let label_dates: i64 =
        conn.query_row("SELECT COUNT(DISTINCT date) FROM ml_labels", [], |r| {
            r.get(0)
        })?;
    let pred_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM ml_predictions", [], |r| r.get(0))?;
    let pred_latest: String = conn.query_row(
        "SELECT COALESCE(MAX(date), 'none') FROM ml_predictions",
        [],
        |r| r.get(0),
    )?;
    let bars_count: i64 = conn.query_row("SELECT COUNT(*) FROM bars", [], |r| r.get(0))?;
    let bars_range: (String, String) = conn.query_row(
        "SELECT COALESCE(MIN(date),'none'), COALESCE(MAX(date),'none') FROM bars",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let feature_range: (String, String) = conn.query_row(
        "SELECT COALESCE(MIN(date),'none'), COALESCE(MAX(date),'none') FROM ml_features",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let label_range: (String, String) = conn.query_row(
        "SELECT COALESCE(MIN(date),'none'), COALESCE(MAX(date),'none') FROM ml_labels",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let sp500_latest: String = conn
        .query_row(
            "SELECT COALESCE(MAX(date),'none') FROM macro_series WHERE series_id='SP500'",
            [],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "none".into());
    let vix_latest: String = conn
        .query_row(
            "SELECT COALESCE(MAX(date),'none') FROM macro_series WHERE series_id='VIXCLS'",
            [],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "none".into());
    let asset_total: i64 = conn
        .query_row("SELECT COUNT(*) FROM assets", [], |r| r.get(0))
        .unwrap_or(0);
    let active_tradable_assets: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM assets
             WHERE LOWER(COALESCE(status, 'inactive'))='active'
               AND COALESCE(tradable, 0)=1",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let inactive_assets: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM assets
             WHERE LOWER(COALESCE(status, 'inactive'))!='active'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let non_tradable_assets: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM assets WHERE COALESCE(tradable, 0)=0",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let latest_feature_symbols: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT symbol) FROM ml_features
             WHERE date=(SELECT MAX(date) FROM ml_features)",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let latest_prediction_symbols: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT symbol) FROM ml_predictions
             WHERE date=(SELECT MAX(date) FROM ml_predictions)",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // LSTM model info
    let lstm_path = paths::lstm_model_path();
    let lstm_exists = lstm_path.exists();

    let lstm_pred_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='ml_lstm_predictions'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let lstm_preds = if lstm_pred_count > 0 {
        conn.query_row("SELECT COUNT(*) FROM ml_lstm_predictions", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap_or(0)
    } else {
        0
    };
    let lstm_latest: String = if lstm_pred_count > 0 {
        conn.query_row(
            "SELECT COALESCE(MAX(date),'none') FROM ml_lstm_predictions",
            [],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "none".into())
    } else {
        "none".into()
    };

    // Ensemble info
    let ensemble_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ml_predictions WHERE ensemble_score IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let ensemble_latest: String = conn
        .query_row(
            "SELECT COALESCE(MAX(date),'none') FROM ml_predictions WHERE ensemble_score IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "none".into());

    // SHAP info
    let shap_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='ml_shap_values'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let shap_rows = if shap_count > 0 {
        conn.query_row(
            "SELECT COUNT(DISTINCT symbol) FROM ml_shap_values",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    } else {
        0
    };

    let lightgbm_report = fs::read_to_string(paths::lightgbm_training_report_path())
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let lstm_report = fs::read_to_string(paths::state_dir().join("lstm_training_report.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());

    let mut freshness_notes = Vec::new();
    if bars_range.1 != "none" && sp500_latest != "none" && sp500_latest < bars_range.1 {
        freshness_notes.push(format!(
            "FRED SP500 is older than bars ({} < {}). Macro features use the latest local value until FRED publishes.",
            sp500_latest, bars_range.1
        ));
    }
    if bars_range.1 != "none" && vix_latest != "none" && vix_latest < bars_range.1 {
        freshness_notes.push(format!(
            "FRED VIXCLS is older than bars ({} < {}). Volatility context uses the latest local value until FRED publishes.",
            vix_latest, bars_range.1
        ));
    }
    if bars_range.1 != "none" && feature_range.1 != "none" && feature_range.1 < bars_range.1 {
        freshness_notes.push(format!(
            "ML features are behind bars ({} < {}). Run `mlai-trade ml refresh`.",
            feature_range.1, bars_range.1
        ));
    }
    if feature_range.1 != "none" && pred_latest != "none" && pred_latest < feature_range.1 {
        freshness_notes.push(format!(
            "LightGBM predictions are behind features ({} < {}).",
            pred_latest, feature_range.1
        ));
    }
    if feature_range.1 != "none" && lstm_latest != "none" && lstm_latest < feature_range.1 {
        freshness_notes.push(format!(
            "LSTM predictions are behind features ({} < {}).",
            lstm_latest, feature_range.1
        ));
    }
    if feature_range.1 != "none" && ensemble_latest != "none" && ensemble_latest < feature_range.1 {
        freshness_notes.push(format!(
            "Ensemble predictions are behind features ({} < {}).",
            ensemble_latest, feature_range.1
        ));
    }
    if freshness_notes.is_empty() {
        freshness_notes.push("Bars, features, predictions, and ensemble are aligned for the latest available feature date.".to_string());
    }
    freshness_notes.push(
        "Labels intentionally lag the latest bars because forward-return labels require future trading sessions.".to_string(),
    );
    let accelerator_status = accelerators::accelerator_status_json();

    if json {
        println!(
            "{}",
            serde_json::json!({
                "features": {"rows": feat_count, "dates": feat_dates, "symbols": feat_syms, "from": feature_range.0, "to": feature_range.1},
                "labels": {"rows": label_count, "from": label_range.0, "to": label_range.1, "note": "forward-return labels lag live bars by design"},
                "predictions": {"rows": pred_count, "latest": pred_latest},
                "bars": {"rows": bars_count, "from": bars_range.0, "to": bars_range.1},
                "macro": {"sp500_latest": sp500_latest, "vix_latest": vix_latest},
                "universe": {
                    "provider_assets": asset_total,
                    "active_tradable_assets": active_tradable_assets,
                    "inactive_assets": inactive_assets,
                    "non_tradable_assets": non_tradable_assets,
                    "latest_feature_symbols": latest_feature_symbols,
                    "latest_prediction_symbols": latest_prediction_symbols,
                },
                "lstm": {"model_exists": lstm_exists, "predictions": lstm_preds, "latest": lstm_latest},
                "ensemble": {"predictions_with_ensemble": ensemble_count, "latest": ensemble_latest},
                "shap": {"symbols_explained": shap_rows},
                "training_data": {
                    "labeled_rows": label_count,
                    "dates": label_dates,
                    "from": label_range.0,
                    "to": label_range.1,
                    "lightgbm_report": lightgbm_report,
                    "lstm_report": lstm_report,
                },
                "accelerators": accelerator_status,
                "freshness": {
                    "bars_latest": bars_range.1,
                    "sp500_latest": sp500_latest,
                    "vix_latest": vix_latest,
                    "features_latest": feature_range.1,
                    "labels_latest": label_range.1,
                    "lgb_predictions_latest": pred_latest,
                    "lstm_predictions_latest": lstm_latest,
                    "ensemble_latest": ensemble_latest,
                    "info": freshness_notes,
                },
                "next_step": if bars_count == 0 || feat_count == 0 || label_count == 0 || pred_count == 0 || !lstm_exists || ensemble_count == 0 {
                    "Run `mlai-trade ml refresh` to sync missing data, compute features/labels, train models, and refresh predictions."
                } else {
                    "ML artifacts are ready. Use `mlai-trade ml explain SYMBOL`, `mlai-trade data suggest`, or `mlai-trade auto run`."
                },
            })
        );
    } else {
        println!("📊 ML Pipeline Status");
        println!("{}", "─".repeat(50));
        println!(
            "Bars:         {} rows ({} → {})",
            bars_count, bars_range.0, bars_range.1
        );
        println!(
            "Universe:     {} active tradable assets | {} provider symbols | {} latest feature symbols | {} latest ensemble symbols",
            active_tradable_assets, asset_total, latest_feature_symbols, latest_prediction_symbols
        );
        if inactive_assets > 0 || non_tradable_assets > 0 {
            println!(
                "  Asset flags:   {} inactive | {} non-tradable",
                inactive_assets, non_tradable_assets
            );
        }
        if latest_prediction_symbols < latest_feature_symbols {
            println!(
                "  Prediction note: ensemble output is limited to symbols with all required model predictions."
            );
        }
        println!(
            "Features:     {} rows ({} dates, {} symbols)",
            feat_count, feat_dates, feat_syms
        );
        println!("  Feature range: {} → {}", feature_range.0, feature_range.1);
        println!(
            "Labels:       {} rows ({} → {})",
            label_count, label_range.0, label_range.1
        );
        println!(
            "LGB Preds:    {} rows (latest: {})",
            pred_count, pred_latest
        );
        println!(
            "LSTM Model:   {}",
            if lstm_exists {
                "✅ loaded"
            } else {
                "❌ not trained"
            }
        );
        println!(
            "LSTM Preds:   {} rows (latest: {})",
            lstm_preds, lstm_latest
        );
        println!(
            "Ensemble:     {} predictions with ensemble scores",
            ensemble_count
        );
        println!("SHAP:         {} cached symbol explanations", shap_rows);
        println!();
        println!("Accelerators");
        for line in accelerators::accelerator_status_lines() {
            println!("  {}", line);
        }
        println!();
        println!("Training Data");
        println!(
            "  Labeled rows: {} rows ({} dates; {} → {})",
            label_count, label_dates, label_range.0, label_range.1
        );
        if let Some(report) = &lightgbm_report {
            println!(
                "  LightGBM report: train={} valid={} data={} → {}",
                report
                    .get("train_rows")
                    .and_then(Value::as_i64)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "not available".into()),
                report
                    .get("valid_rows")
                    .and_then(Value::as_i64)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "not available".into()),
                report
                    .get("date_start")
                    .and_then(Value::as_str)
                    .unwrap_or("not available"),
                report
                    .get("date_end")
                    .and_then(Value::as_str)
                    .unwrap_or("not available"),
            );
        } else {
            println!("  LightGBM report: not available");
        }
        if let Some(report) = &lstm_report {
            let split = report.get("split").unwrap_or(&Value::Null);
            println!(
                "  LSTM report: train={} validation={} data={} → {}",
                report
                    .get("train_samples")
                    .and_then(Value::as_i64)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "not available".into()),
                report
                    .get("val_samples")
                    .and_then(Value::as_i64)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "not available".into()),
                split
                    .get("train_start_date")
                    .and_then(Value::as_str)
                    .unwrap_or("not available"),
                split
                    .get("validation_end_date")
                    .and_then(Value::as_str)
                    .unwrap_or("not available"),
            );
        } else {
            println!("  LSTM report: not available");
        }
        println!();
        println!("Freshness");
        println!("  Bars latest:      {}", bars_range.1);
        println!(
            "  FRED latest:      SP500={} VIXCLS={}",
            sp500_latest, vix_latest
        );
        println!("  Features latest:  {}", feature_range.1);
        println!("  Labels latest:    {} (expected lag)", label_range.1);
        println!(
            "  Predictions:      LGB={} LSTM={} Ensemble={}",
            pred_latest, lstm_latest, ensemble_latest
        );
        for note in &freshness_notes {
            println!("  Info: {}", note);
        }
        println!();
        if bars_count == 0 {
            println!("Next step: run `mlai-trade ml refresh`.");
            println!("  This will sync the universe, FRED benchmarks, Alpaca bars, features, labels, LightGBM, baselines, LSTM, predictions, and ensemble.");
        } else if feat_count == 0 || label_count == 0 {
            println!("Next step: run `mlai-trade ml refresh` to compute missing features/labels and train models.");
        } else if pred_count == 0 || !lstm_exists || ensemble_count == 0 {
            println!("Next step: run `mlai-trade ml refresh` to train missing models and refresh predictions.");
        } else if shap_rows == 0 {
            println!("Optional: run `mlai-trade ml explain SYMBOL` when you want SHAP details for a specific symbol.");
            println!("  Example: `mlai-trade ml explain AAPL`");
        } else {
            println!(
                "Ready: ML artifacts are available for explain, data suggest, and auto-trade decisions."
            );
            println!(
                "Explain a symbol on demand with `mlai-trade ml explain SYMBOL`, for example `mlai-trade ml explain AAPL`."
            );
        }
    }

    Ok(())
}

fn smoke_pass(name: &str, details: Value) -> Value {
    serde_json::json!({
        "name": name,
        "status": "passed",
        "details": details,
    })
}

fn smoke_skip(name: &str, reason: impl Into<String>) -> Value {
    serde_json::json!({
        "name": name,
        "status": "skipped",
        "reason": reason.into(),
    })
}

fn smoke_fail(name: &str, err: anyhow::Error) -> Value {
    serde_json::json!({
        "name": name,
        "status": "failed",
        "error": err.to_string(),
    })
}

fn status_available(status: &Value, key: &str) -> bool {
    status
        .get(key)
        .and_then(|value| value.get("available"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

#[cfg(mlai_xgboost)]
fn xgboost_cuda_smoke_test() -> anyhow::Result<Value> {
    let features = [
        0.0f32, 1.0, 0.2, 0.1, 0.9, 0.3, 0.2, 0.8, 0.4, 0.3, 0.7, 0.5, 0.4, 0.6, 0.6, 0.5, 0.5,
        0.7, 0.6, 0.4, 0.8, 0.7, 0.3, 0.9, 0.8, 0.2, 1.0, 0.9, 0.1, 0.9,
    ];
    let labels = [0.0f32, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9];
    let train = xgb_dmatrix_from_dense(&features, labels.len(), 3, Some(&labels))?;
    let dmats = [train.handle];
    let mut handle = std::ptr::null_mut();
    xgb_check(unsafe {
        xgboost_lib_sys::XGBoosterCreate(dmats.as_ptr(), dmats.len() as _, &mut handle)
    })?;
    let booster = XgbBooster { handle };
    for (name, value) in [
        ("objective", "reg:squarederror"),
        ("eval_metric", "rmse"),
        ("tree_method", "hist"),
        ("device", "cuda"),
        ("max_depth", "2"),
        ("eta", "0.1"),
        ("nthread", "0"),
        ("seed", "42"),
    ] {
        xgb_set_param(&booster, name, value)?;
    }
    for iter in 0..2 {
        xgb_check(unsafe {
            xgboost_lib_sys::XGBoosterUpdateOneIter(booster.handle, iter, train.handle)
        })?;
    }
    let preds = xgb_predict_dmatrix(&booster, &train)?;
    if preds.len() != labels.len() {
        anyhow::bail!(
            "XGBoost CUDA smoke prediction length mismatch: {} != {}",
            preds.len(),
            labels.len()
        );
    }
    Ok(serde_json::json!({
        "rows": labels.len(),
        "features": 3,
        "iterations": 2,
        "predictions": preds.len(),
    }))
}

#[cfg(not(mlai_xgboost))]
fn xgboost_cuda_smoke_test() -> anyhow::Result<Value> {
    anyhow::bail!("XGBoost is not available in this build.")
}

fn packaged_lightgbm_binary() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let exe_dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("unable to determine mlai-trade executable directory"))?;
    let candidate = exe_dir.join("tools/lightgbm");
    if candidate.is_file() {
        return Ok(candidate);
    }
    anyhow::bail!(
        "packaged LightGBM smoke binary not found at {}; run scripts/package-local-linux.sh",
        candidate.display()
    )
}

fn packaged_library_path() -> anyhow::Result<String> {
    let exe = std::env::current_exe()?;
    let exe_dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("unable to determine mlai-trade executable directory"))?;
    let mut paths = vec![exe_dir.join("lib")];
    if let Some(existing) = std::env::var_os("LD_LIBRARY_PATH") {
        paths.extend(std::env::split_paths(&existing));
    }
    Ok(std::env::join_paths(paths)?.to_string_lossy().to_string())
}

fn lightgbm_cuda_smoke_test() -> anyhow::Result<Value> {
    let unique = format!(
        "{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let train_path = std::env::temp_dir().join(format!("mlai-trade-lgb-cuda-smoke-{unique}.train"));
    let valid_path = std::env::temp_dir().join(format!("mlai-trade-lgb-cuda-smoke-{unique}.valid"));
    let config_path = std::env::temp_dir().join(format!("mlai-trade-lgb-cuda-smoke-{unique}.conf"));
    let model_path = std::env::temp_dir().join(format!("mlai-trade-lgb-cuda-smoke-{unique}.model"));
    let mut train_text = String::new();
    for i in 0..240 {
        let x0 = (i as f64) / 239.0;
        let x1 = 1.0 - x0;
        let x2 = ((i * 17) % 101) as f64 / 100.0;
        let y = 0.6 * x0 + 0.2 * x2;
        train_text.push_str(&format!("{y:.6} 0:{x0:.6} 1:{x1:.6} 2:{x2:.6}\n"));
    }
    let mut valid_text = String::new();
    for i in 0..24 {
        let x0 = (i as f64) / 23.0;
        let x1 = 1.0 - x0;
        let x2 = ((i * 19) % 101) as f64 / 100.0;
        let y = 0.6 * x0 + 0.2 * x2;
        valid_text.push_str(&format!("{y:.6} 0:{x0:.6} 1:{x1:.6} 2:{x2:.6}\n"));
    }
    fs::write(&train_path, train_text)?;
    fs::write(&valid_path, valid_text)?;

    fs::write(
        &config_path,
        format!(
            "\
task=train
boosting=gbdt
objective=regression
metric=l2
device_type=cuda
data={}
valid_data={}
output_model={}
num_iterations=2
num_leaves=3
min_data_in_leaf=1
min_data_in_bin=1
max_bin=15
learning_rate=0.1
num_threads=1
verbose=-1
seed=42
",
            train_path.display(),
            valid_path.display(),
            model_path.display()
        ),
    )?;

    let lightgbm = packaged_lightgbm_binary()?;
    let output = Command::new(&lightgbm)
        .arg(format!("config={}", config_path.display()))
        .env("LD_LIBRARY_PATH", packaged_library_path()?)
        .output()?;

    for path in [&train_path, &valid_path, &config_path, &model_path] {
        let _ = fs::remove_file(path);
    }

    if !output.status.success() {
        anyhow::bail!(
            "LightGBM CUDA smoke process failed with status {:?}: stdout={} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(serde_json::json!({
        "rows": 240,
        "features": 3,
        "iterations": 2,
        "process": lightgbm.display().to_string(),
    }))
}

fn lstm_accelerator_smoke(backend: crate::lstm::LstmBackend) -> anyhow::Result<Value> {
    let device = crate::lstm::lstm_accelerator_smoke_test(backend)?;
    Ok(serde_json::json!({ "device": device }))
}

// Shows accelerator status plus tiny runtime smoke tests for available backends.
pub fn cmd_ml_accelerators(json: bool, strict: bool) -> anyhow::Result<()> {
    let status = accelerators::accelerator_status_json();
    let mut tests = Vec::new();

    if status_available(&status, "nvidia") {
        tests.push(smoke_pass(
            "nvidia",
            serde_json::json!({ "source": "nvidia-smi" }),
        ));
    } else {
        tests.push(smoke_skip(
            "nvidia",
            status["nvidia"]
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("NVIDIA GPU is not visible"),
        ));
    }

    if status_available(&status, "xgboost_cuda") {
        match xgboost_cuda_smoke_test() {
            Ok(details) => tests.push(smoke_pass("xgboost_cuda", details)),
            Err(err) => tests.push(smoke_fail("xgboost_cuda", err)),
        }
    } else {
        tests.push(smoke_skip(
            "xgboost_cuda",
            status["xgboost_cuda"]
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("XGBoost CUDA is not available"),
        ));
    }

    if status_available(&status, "lightgbm_cuda") {
        match lightgbm_cuda_smoke_test() {
            Ok(details) => tests.push(smoke_pass("lightgbm_cuda", details)),
            Err(err) => tests.push(smoke_fail("lightgbm_cuda", err)),
        }
    } else {
        tests.push(smoke_skip(
            "lightgbm_cuda",
            status["lightgbm_cuda"]
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("LightGBM CUDA is not available"),
        ));
    }

    if status_available(&status, "mlx") {
        match lstm_accelerator_smoke(crate::lstm::LstmBackend::Mlx) {
            Ok(details) => tests.push(smoke_pass("mlx", details)),
            Err(err) => tests.push(smoke_fail("mlx", err)),
        }
    } else {
        tests.push(smoke_skip(
            "mlx",
            status["mlx"]
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("MLX is not available"),
        ));
    }

    if status_available(&status, "tch") {
        match lstm_accelerator_smoke(crate::lstm::LstmBackend::Tch) {
            Ok(details) => tests.push(smoke_pass("tch", details)),
            Err(err) => tests.push(smoke_fail("tch", err)),
        }
    } else {
        tests.push(smoke_skip(
            "tch",
            status["tch"]
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("tch CUDA is not available"),
        ));
    }

    let failed = tests
        .iter()
        .any(|test| test.get("status").and_then(Value::as_str) == Some("failed"));
    let ready = !failed;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "ready": ready,
                "accelerators": status,
                "smoke_tests": tests,
            })
        );
    } else {
        println!("ML Accelerator Readiness");
        println!("{}", "─".repeat(50));
        for line in accelerators::accelerator_status_lines() {
            println!("  {}", line);
        }
        println!();
        println!("Smoke Tests");
        for test in &tests {
            let name = test
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            match test
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
            {
                "passed" => println!("  {name}: passed"),
                "failed" => println!(
                    "  {name}: failed ({})",
                    test.get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error")
                ),
                "skipped" => println!(
                    "  {name}: skipped ({})",
                    test.get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("not available")
                ),
                other => println!("  {name}: {other}"),
            }
        }
        println!();
        println!("Ready: {}", if ready { "yes" } else { "no" });
    }

    if strict && failed {
        anyhow::bail!("one or more accelerator smoke tests failed");
    }
    Ok(())
}

// ── DB helper ────────────────────────────────────────────────────

fn open_ml_db() -> anyhow::Result<Connection> {
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

// Opens an immutable-per-query reader for parallel feature workers.
fn open_ml_read_db() -> anyhow::Result<Connection> {
    let db_path = paths::scanner_db_path();
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.execute_batch(&format!(
        "PRAGMA query_only=ON; {}",
        config::sqlite_runtime_pragma_sql()
    ))?;
    Ok(conn)
}

// ══════════════════════════════════════════════════════════════════
// SHAP — TreeSHAP for LightGBM (interventional approach)
// ══════════════════════════════════════════════════════════════════
//
// For each prediction, compute the marginal contribution of each
// feature by comparing predictions with/without that feature.
// Uses a background set of random samples to marginalize over.
// ══════════════════════════════════════════════════════════════════

pub fn init_shap_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ml_shap_values (
            symbol TEXT NOT NULL,
            date TEXT NOT NULL,
            feature_name TEXT NOT NULL,
            shap_value REAL NOT NULL,
            feature_value REAL,
            base_value REAL,
            PRIMARY KEY (symbol, date, feature_name)
        );
        CREATE INDEX IF NOT EXISTS idx_shap_date ON ml_shap_values(date);
        CREATE INDEX IF NOT EXISTS idx_shap_sym ON ml_shap_values(symbol);",
    )?;
    Ok(())
}

impl LgbModel {
    /// Computes signed permutation SHAP-style contributions for one sample.
    ///
    /// Each path starts from a background row and turns target features on one
    /// by one. Averaging deterministic feature orders keeps the sum close to
    /// `predict(target) - E[predict(background)]`, so negative anchors are
    /// visible instead of being hidden by non-additive marginal comparisons.
    pub fn shap_values(&self, features: &[f64], background: &[Vec<f64>]) -> Vec<f64> {
        let n_features = features.len();
        let valid_background = background
            .iter()
            .filter(|row| row.len() >= n_features)
            .collect::<Vec<_>>();
        if valid_background.is_empty() {
            return vec![0.0; n_features];
        }

        let mut shap = vec![0.0; n_features];
        let permutation_count = 8usize.min(n_features.max(1));
        let orders = (0..permutation_count)
            .map(|seed| shap_permutation_order(n_features, seed))
            .collect::<Vec<_>>();

        for bg in &valid_background {
            for order in &orders {
                let mut current = (*bg).clone();
                let mut previous_prediction = self.predict_one(&current);
                for &feature_idx in order {
                    current[feature_idx] = features[feature_idx];
                    let next_prediction = self.predict_one(&current);
                    shap[feature_idx] += next_prediction - previous_prediction;
                    previous_prediction = next_prediction;
                }
            }
        }

        let denom = (valid_background.len() * orders.len()) as f64;
        for value in &mut shap {
            *value /= denom;
        }

        shap
    }
}

// Builds a deterministic pseudo-random feature order for permutation SHAP.
fn shap_permutation_order(n_features: usize, seed: usize) -> Vec<usize> {
    let mut order = (0..n_features).collect::<Vec<_>>();
    let seed_key = (seed as u64 + 1).wrapping_mul(0x9e3779b97f4a7c15);
    order.sort_by_key(|idx| shap_mix64((*idx as u64) ^ seed_key));
    order
}

// Mixes integer keys so SHAP permutations are stable without runtime RNG.
fn shap_mix64(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58476d1ce4e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}

// ── CMD: ml explain <SYMBOL> ─────────────────────────────────────

#[derive(Debug)]
struct ShapExplanation {
    symbol: String,
    date: String,
    feature_values: Vec<f64>,
    shap_values: Vec<f64>,
    base_value: f64,
    predicted: f64,
}

// Handles table exists database metadata.
fn table_exists(conn: &Connection, table: &str) -> anyhow::Result<bool> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        params![table],
        |row| row.get(0),
    )?;
    Ok(exists > 0)
}

fn asset_status_is_active(status: Option<&str>) -> bool {
    status.unwrap_or("inactive").eq_ignore_ascii_case("active")
}

fn ml_asset_status(conn: &Connection, symbol: &str) -> anyhow::Result<Option<Value>> {
    if !table_exists(conn, "assets")? {
        return Ok(None);
    }
    let asset_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM assets", [], |row| row.get(0))
        .unwrap_or(0);
    if asset_count == 0 {
        return Ok(None);
    }

    let row: Option<AssetStatusRow> = conn
        .query_row(
            "SELECT status, tradable, name, exchange
             FROM assets WHERE UPPER(symbol)=UPPER(?1)",
            params![symbol],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;

    let Some((status, tradable, name, exchange)) = row else {
        return Ok(Some(json!({
            "symbol": symbol,
            "classification": "missing_from_provider_assets",
            "status": "missing",
            "active": false,
            "tradable": false,
            "ml_eligible": false,
            "explainable": false,
            "action": "manual provider review; normal ML/trading is skipped",
        })));
    };

    let active = asset_status_is_active(status.as_deref());
    let tradable = tradable.unwrap_or(0) != 0;
    let classification = if active && tradable {
        "active_tradable"
    } else if active {
        "active_not_tradable"
    } else {
        "inactive"
    };
    Ok(Some(json!({
        "symbol": symbol,
        "name": name,
        "exchange": exchange,
        "classification": classification,
        "status": status.unwrap_or_else(|| "unknown".to_string()),
        "active": active,
        "tradable": tradable,
        "ml_eligible": active && tradable,
        "explainable": active && tradable,
        "action": if active && tradable {
            "normal"
        } else {
            "manual provider review; normal ML/trading is skipped"
        },
    })))
}

fn is_non_explainable_asset(asset_status: Option<&Value>) -> bool {
    asset_status
        .and_then(|value| value.get("ml_eligible"))
        .and_then(Value::as_bool)
        == Some(false)
}

// Returns latest feature date from local storage.
fn latest_feature_date(conn: &Connection) -> anyhow::Result<String> {
    let latest_date: String = conn.query_row(
        "SELECT COALESCE(MAX(date),'none') FROM ml_features WHERE return_1d IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    if latest_date == "none" {
        anyhow::bail!("No ML features found. Run `mlai-trade ml refresh` first.");
    }
    Ok(latest_date)
}

// Loads feature vector from storage or configuration.
fn load_feature_vector(
    conn: &Connection,
    symbol: &str,
    date: &str,
) -> anyhow::Result<Option<Vec<f64>>> {
    let query = format!(
        "SELECT {} FROM ml_features WHERE symbol = ?1 AND date = ?2",
        FEATURE_COLS.join(", ")
    );
    conn.query_row(&query, params![symbol, date], |r| {
        let mut values = Vec::with_capacity(FEATURE_COLS.len());
        for i in 0..FEATURE_COLS.len() {
            let val: Option<f64> = r.get(i)?;
            values.push(val.unwrap_or(0.0));
        }
        Ok(values)
    })
    .optional()
    .map_err(Into::into)
}

// Loads shap background from storage or configuration.
fn load_shap_background(
    conn: &Connection,
    date: &str,
    limit: usize,
) -> anyhow::Result<Vec<Vec<f64>>> {
    let query = format!(
        "WITH ranked AS (
            SELECT {}, ROW_NUMBER() OVER (ORDER BY symbol) AS rn, COUNT(*) OVER () AS total
            FROM ml_features
            WHERE date = ?1 AND return_1d IS NOT NULL
         )
         SELECT {}
         FROM ranked
         WHERE ((rn - 1) % CAST(MAX(total / ?2, 1) AS INTEGER)) = 0
         ORDER BY rn
         LIMIT ?2",
        FEATURE_COLS.join(", "),
        FEATURE_COLS.join(", ")
    );
    let mut stmt = conn.prepare(&query)?;
    let background = stmt
        .query_map(params![date, limit as i64], |r| {
            let mut values = Vec::with_capacity(FEATURE_COLS.len());
            for i in 0..FEATURE_COLS.len() {
                let val: Option<f64> = r.get(i)?;
                values.push(val.unwrap_or(0.0));
            }
            Ok(values)
        })?
        .filter_map(|row| row.ok())
        .collect::<Vec<_>>();
    if background.is_empty() {
        anyhow::bail!("No background samples for SHAP.");
    }
    Ok(background)
}

// Computes shap explanation from prepared inputs.
fn compute_shap_explanation(
    model: &LgbModel,
    symbol: &str,
    date: &str,
    feature_values: Vec<f64>,
    background: &[Vec<f64>],
    base_value: f64,
) -> ShapExplanation {
    let shap_values = model.shap_values(&feature_values, background);
    let predicted = model.predict_one(&feature_values);
    ShapExplanation {
        symbol: symbol.to_string(),
        date: date.to_string(),
        feature_values,
        shap_values,
        base_value,
        predicted,
    }
}

// Stores shap explanation in local storage.
fn store_shap_explanation(conn: &Connection, explanation: &ShapExplanation) -> anyhow::Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM ml_shap_values WHERE symbol=?1 AND date=?2",
        params![explanation.symbol, explanation.date],
    )?;
    {
        let mut ins = tx.prepare_cached(
            "INSERT INTO ml_shap_values (symbol, date, feature_name, shap_value, feature_value, base_value)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
        )?;
        for (idx, shap_value) in explanation.shap_values.iter().enumerate() {
            ins.execute(params![
                explanation.symbol,
                explanation.date,
                FEATURE_COLS[idx],
                shap_value,
                explanation.feature_values[idx],
                explanation.base_value,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

// Opens position symbols with the configured runtime settings.
fn open_position_symbols(conn: &Connection) -> anyhow::Result<Vec<String>> {
    if !table_exists(conn, "auto_positions")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT DISTINCT UPPER(symbol)
         FROM auto_positions
         WHERE status='open'
         ORDER BY UPPER(symbol)",
    )?;
    let symbols = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .filter_map(|row| row.ok())
        .collect();
    Ok(symbols)
}

// Handles default shap candidates logic.
fn default_shap_candidates(
    conn: &Connection,
    latest_date: &str,
    top_limit: usize,
) -> anyhow::Result<(Vec<String>, usize, usize)> {
    let mut symbols = Vec::new();
    let mut seen = HashSet::new();
    let mut open_count = 0usize;
    for symbol in open_position_symbols(conn)? {
        if config::is_blocked_symbol(&symbol) {
            continue;
        }
        if seen.insert(symbol.clone()) {
            symbols.push(symbol);
            open_count += 1;
        }
    }

    let mut top_count = 0usize;
    let mut stmt = conn.prepare(
        "SELECT p.symbol
         FROM ml_predictions p
         INNER JOIN ml_features f ON p.symbol=f.symbol AND p.date=f.date
         WHERE p.date=?1
           AND p.ensemble_score IS NOT NULL
           AND f.return_1d IS NOT NULL
         ORDER BY p.ensemble_score DESC
         LIMIT ?2",
    )?;
    let top_symbols = stmt
        .query_map(
            params![latest_date, (top_limit * 3).max(top_limit) as i64],
            |r| r.get::<_, String>(0),
        )?
        .filter_map(|row| row.ok())
        .collect::<Vec<_>>();
    for symbol in top_symbols {
        let symbol = symbol.to_ascii_uppercase();
        if config::is_blocked_symbol(&symbol) {
            continue;
        }
        if seen.insert(symbol.clone()) {
            symbols.push(symbol);
            top_count += 1;
            if top_count >= top_limit {
                break;
            }
        }
    }

    Ok((symbols, open_count, top_count))
}

// Handles the ml cache default shap CLI action.
pub fn cmd_ml_cache_default_shap(
    top_limit: usize,
    json: bool,
) -> anyhow::Result<serde_json::Value> {
    let model_path = paths::ml_model_path();
    if !model_path.exists() {
        anyhow::bail!(
            "Model not found: {} — run `mlai-trade ml refresh` first",
            model_path.display()
        );
    }
    let model = LgbModel::load(&model_path.to_string_lossy())?;
    let conn = open_ml_db()?;
    init_ml_tables(&conn)?;
    init_ensemble_columns(&conn)?;
    init_shap_tables(&conn)?;

    let latest_date = latest_feature_date(&conn)?;
    let (symbols, open_count, top_count) = default_shap_candidates(&conn, &latest_date, top_limit)?;
    let background = load_shap_background(&conn, &latest_date, 100)?;
    let base_value = background
        .iter()
        .map(|row| model.predict_one(row))
        .sum::<f64>()
        / background.len() as f64;

    let mut cached = Vec::new();
    let mut skipped = Vec::new();
    let progress = crate::progress::bar_if(
        !json,
        symbols.len() as u64,
        "Caching default SHAP explanations",
    );
    for symbol in &symbols {
        progress.set_message(symbol);
        let Some(features) = load_feature_vector(&conn, symbol, &latest_date)? else {
            skipped.push(serde_json::json!({
                "symbol": symbol,
                "reason": "no_latest_features",
            }));
            progress.inc(1);
            continue;
        };
        let explanation = compute_shap_explanation(
            &model,
            symbol,
            &latest_date,
            features,
            &background,
            base_value,
        );
        store_shap_explanation(&conn, &explanation)?;
        cached.push(serde_json::json!({
            "symbol": symbol,
            "predicted": (explanation.predicted * 10000.0).round() / 10000.0,
        }));
        progress.inc(1);
    }
    progress.finish_and_clear();

    let report = serde_json::json!({
        "status": "ok",
        "date": latest_date,
        "background_samples": background.len(),
        "open_position_symbols": open_count,
        "top_ensemble_symbols": top_count,
        "requested_symbols": symbols.len(),
        "cached_symbols": cached.len(),
        "skipped": skipped,
        "symbols": cached,
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "Default SHAP Cache - {}",
            report["date"].as_str().unwrap_or("?")
        );
        println!("{}", "─".repeat(50));
        println!(
            "Cached {} symbols ({} open positions + {} top ensemble candidates; {} background samples)",
            report["cached_symbols"].as_u64().unwrap_or(0),
            open_count,
            top_count,
            background.len()
        );
        if report["skipped"]
            .as_array()
            .map(|items| !items.is_empty())
            .unwrap_or(false)
        {
            println!("Skipped: {}", report["skipped"]);
        }
        println!("List cache with: `mlai-trade ml explained`");
    }
    Ok(report)
}

// Handles the ml explainable CLI action.
pub fn cmd_ml_explainable(limit: usize, json: bool) -> anyhow::Result<()> {
    let conn = open_ml_db()?;
    init_ml_tables(&conn)?;
    init_ensemble_columns(&conn)?;

    let latest_prediction_date: String = conn
        .query_row(
            "SELECT COALESCE(MAX(date),'none') FROM ml_predictions WHERE ensemble_score IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "none".to_string());
    let latest_feature_date: String = conn.query_row(
        "SELECT COALESCE(MAX(date),'none') FROM ml_features WHERE return_1d IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    if latest_feature_date == "none" {
        anyhow::bail!("No ML features found. Run `mlai-trade ml refresh` first.");
    }

    let mut rows = Vec::new();
    if latest_prediction_date != "none" {
        let mut stmt = conn.prepare(
            "SELECT p.symbol, p.ensemble_score, p.predicted_quintile
             FROM ml_predictions p
             INNER JOIN ml_features f ON p.symbol=f.symbol AND p.date=f.date
             WHERE p.date=?1
               AND p.ensemble_score IS NOT NULL
               AND f.return_1d IS NOT NULL
             ORDER BY p.ensemble_score DESC
             LIMIT ?2",
        )?;
        rows = stmt
            .query_map(
                params![latest_prediction_date, (limit * 2).max(limit) as i64],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<f64>>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                    ))
                },
            )?
            .filter_map(|row| row.ok())
            .filter(|(symbol, _, _)| !config::is_blocked_symbol(symbol))
            .take(limit)
            .collect();
    }

    if rows.is_empty() {
        let mut stmt = conn.prepare(
            "SELECT symbol, NULL, NULL
             FROM ml_features
             WHERE date=?1 AND return_1d IS NOT NULL
             ORDER BY symbol
             LIMIT ?2",
        )?;
        rows = stmt
            .query_map(
                params![latest_feature_date, (limit * 2).max(limit) as i64],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<f64>>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                    ))
                },
            )?
            .filter_map(|row| row.ok())
            .filter(|(symbol, _, _)| !config::is_blocked_symbol(symbol))
            .take(limit)
            .collect();
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "prediction_date": latest_prediction_date,
                "feature_date": latest_feature_date,
                "symbols": rows.iter().map(|(symbol, score, quintile)| {
                    serde_json::json!({
                        "symbol": symbol,
                        "ensemble_score": score,
                        "predicted_quintile": quintile,
                        "command": format!("mlai-trade ml explain {}", symbol),
                    })
                }).collect::<Vec<_>>(),
            }))?
        );
        return Ok(());
    }

    println!("Explainable Symbols");
    println!("{}", "─".repeat(50));
    if latest_prediction_date != "none" {
        println!(
            "Ranked by latest ensemble score: {}",
            latest_prediction_date
        );
    } else {
        println!(
            "No ensemble predictions found; listing latest feature symbols: {latest_feature_date}"
        );
    }
    println!(
        "{:<6} {:<8} {:>12} {:>8}",
        "Rank", "Symbol", "Ensemble", "Q"
    );
    for (idx, (symbol, score, quintile)) in rows.iter().enumerate() {
        println!(
            "{:<6} {:<8} {:>12} {:>8}",
            idx + 1,
            symbol,
            score
                .map(|value| format!("{value:.6}"))
                .unwrap_or_else(|| "-".to_string()),
            quintile
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string())
        );
    }
    println!();
    println!("Explain one with: `mlai-trade ml explain SYMBOL`");
    Ok(())
}

// Handles the ml explained CLI action.
pub fn cmd_ml_explained(limit: usize, json: bool) -> anyhow::Result<()> {
    let conn = open_ml_db()?;
    init_shap_tables(&conn)?;

    let mut stmt = conn.prepare(
        "SELECT symbol, date, COUNT(*) AS features, MAX(ABS(shap_value)) AS max_abs_shap
         FROM ml_shap_values
         GROUP BY symbol, date
         ORDER BY date DESC, symbol
         LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "cached_explanations": rows.iter().map(|(symbol, date, features, max_abs_shap)| {
                    serde_json::json!({
                        "symbol": symbol,
                        "date": date,
                        "feature_rows": features,
                        "max_abs_shap": max_abs_shap,
                        "command": format!("mlai-trade ml explain {}", symbol),
                    })
                }).collect::<Vec<_>>(),
            }))?
        );
        return Ok(());
    }

    if rows.is_empty() {
        println!("No cached SHAP explanations yet.");
        println!("Create one with: `mlai-trade ml explain SYMBOL`");
        return Ok(());
    }

    println!("Cached SHAP Explanations");
    println!("{}", "─".repeat(50));
    println!(
        "{:<8} {:<12} {:>8} {:>14}",
        "Symbol", "Date", "Features", "Max |SHAP|"
    );
    for (symbol, date, features, max_abs_shap) in rows {
        println!(
            "{:<8} {:<12} {:>8} {:>14.6}",
            symbol, date, features, max_abs_shap
        );
    }
    println!();
    println!("Re-open one with: `mlai-trade ml explain SYMBOL`");
    Ok(())
}

// Handles the ml explain CLI action.
pub fn cmd_ml_explain(symbol: String, json: bool) -> anyhow::Result<()> {
    let symbol = symbol.trim().to_ascii_uppercase();
    if symbol.is_empty() {
        anyhow::bail!("Symbol is required.");
    }
    if config::is_blocked_symbol(&symbol) {
        anyhow::bail!(
            "{} is blocked by auto.compliance.blocked_symbols in {}. Blocked symbols are excluded from ML data and cannot be explained.",
            symbol,
            config::config_path().display()
        );
    }

    let conn = open_ml_db()?;
    init_ml_tables(&conn)?;
    init_shap_tables(&conn)?;

    let asset_status = ml_asset_status(&conn, &symbol)?;
    if is_non_explainable_asset(asset_status.as_ref()) {
        let latest_date =
            latest_feature_date(&conn).unwrap_or_else(|_| "not available".to_string());
        let classification = asset_status
            .as_ref()
            .and_then(|value| value.get("classification"))
            .and_then(Value::as_str)
            .unwrap_or("not_tradable");
        let status = asset_status
            .as_ref()
            .and_then(|value| value.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let report = json!({
            "status": "not_explainable",
            "symbol": symbol,
            "date": latest_date,
            "explainable": false,
            "reason": "asset_not_active_tradable",
            "message": format!("{symbol} is {classification} in the provider asset universe (status={status}); ML explanations are skipped for symbols that are not active/tradable."),
            "asset_status": asset_status,
        });
        if json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            println!("ML explanation unavailable");
            println!("{}", "─".repeat(50));
            println!("  Symbol: {}", report["symbol"].as_str().unwrap_or("?"));
            println!("  Status: {}", classification);
            println!("  Tradable: false");
            println!("  Reason: {}", report["message"].as_str().unwrap_or("?"));
            println!("  Action: resolve the provider/corporate-action position manually; ML/trading skips this symbol.");
        }
        return Ok(());
    }

    let model_path = paths::ml_model_path();

    if !model_path.exists() {
        anyhow::bail!(
            "Model not found: {} — run 'mlai-trade ml refresh' first",
            model_path.display()
        );
    }

    let model_path_str = model_path.to_string_lossy().to_string();
    let model = LgbModel::load(&model_path_str)?;
    let latest_date = latest_feature_date(&conn)?;
    let target_feats = load_feature_vector(&conn, &symbol, &latest_date)?.ok_or_else(|| {
        anyhow::anyhow!(
            "No features for {} on {}. Run `mlai-trade ml explainable` to list symbols that can be explained.",
            symbol,
            latest_date
        )
    })?;
    let background = load_shap_background(&conn, &latest_date, 100)?;
    let base_value: f64 =
        background.iter().map(|b| model.predict_one(b)).sum::<f64>() / background.len() as f64;

    eprintln!(
        "Computing SHAP values ({} background samples)...",
        background.len()
    );
    let progress = crate::progress::spinner_if(!json, format!("Computing SHAP for {symbol}"));
    let explanation = compute_shap_explanation(
        &model,
        &symbol,
        &latest_date,
        target_feats,
        &background,
        base_value,
    );
    store_shap_explanation(&conn, &explanation)?;
    progress.finish_and_clear();

    // Sort by absolute SHAP value
    let mut indexed: Vec<(usize, f64)> = explanation
        .shap_values
        .iter()
        .enumerate()
        .map(|(i, &v)| (i, v))
        .collect();
    indexed.sort_by(|a, b| {
        b.1.abs()
            .partial_cmp(&a.1.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if json {
        let shap_sum = explanation.shap_values.iter().sum::<f64>();
        let prediction_minus_base = explanation.predicted - explanation.base_value;
        let features: Vec<serde_json::Value> = indexed
            .iter()
            .map(|&(i, sv)| {
                serde_json::json!({
                    "feature": FEATURE_COLS[i],
                    "shap_value": (sv * 100000.0).round() / 100000.0,
                    "feature_value": (explanation.feature_values[i] * 10000.0).round() / 10000.0,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "symbol": explanation.symbol,
                "date": explanation.date,
                "predicted": (explanation.predicted * 10000.0).round() / 10000.0,
                "base_value": (explanation.base_value * 10000.0).round() / 10000.0,
                "shap_sum": (shap_sum * 100000.0).round() / 100000.0,
                "prediction_minus_base": (prediction_minus_base * 100000.0).round() / 100000.0,
                "additivity_error": ((shap_sum - prediction_minus_base) * 100000.0).round() / 100000.0,
                "asset_status": asset_status,
                "features": features,
            })
        );
    } else {
        println!(
            "🔍 SHAP Explanation for {} ({})",
            explanation.symbol, explanation.date
        );
        println!(
            "Base value: {:.4} │ Predicted: {:.4}",
            explanation.base_value, explanation.predicted
        );
        println!("{}", "─".repeat(50));
        println!("Top positive contributors");
        for &(i, sv) in indexed.iter().filter(|(_, sv)| *sv > 0.0).take(10) {
            println!(
                "▲ {:>+8.4}  {:<22} (val: {:.4})",
                sv, FEATURE_COLS[i], explanation.feature_values[i]
            );
        }
        println!();
        println!("Top negative anchors");
        let mut printed_negative = false;
        for &(i, sv) in indexed.iter().filter(|(_, sv)| *sv < 0.0).take(10) {
            printed_negative = true;
            println!(
                "▼ {:>+8.4}  {:<22} (val: {:.4})",
                sv, FEATURE_COLS[i], explanation.feature_values[i]
            );
        }
        if !printed_negative {
            println!("  none");
        }
    }

    Ok(())
}

// ══════════════════════════════════════════════════════════════════
// ENSEMBLE — Combine LightGBM + LSTM predictions
// ══════════════════════════════════════════════════════════════════

pub fn init_ensemble_columns(conn: &Connection) -> rusqlite::Result<()> {
    // Add columns if they don't exist (SQLite ADD COLUMN is idempotent-safe with IF NOT EXISTS... but not supported)
    // We'll just try and ignore errors
    let _ = conn.execute("ALTER TABLE ml_predictions ADD COLUMN lgb_score REAL", []);
    let _ = conn.execute("ALTER TABLE ml_predictions ADD COLUMN lstm_score REAL", []);
    let _ = conn.execute("ALTER TABLE ml_predictions ADD COLUMN xgb_score REAL", []);
    let _ = conn.execute(
        "ALTER TABLE ml_predictions ADD COLUMN ensemble_score REAL",
        [],
    );
    Ok(())
}

// Handles the ml ensemble default CLI action.
pub fn cmd_ml_ensemble_default(json: bool) -> anyhow::Result<()> {
    let config_path = paths::state_dir().join("ml_default_ensemble_config.json");
    let mut lgb_weight = 0.4;
    let mut lstm_weight = 0.6;
    let mut xgb_weight = 0.0;
    let mut feature_set = "full_features".to_string();
    if config_path.exists() {
        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config_path)?)?;
        feature_set = config["feature_set"]
            .as_str()
            .unwrap_or("full_features")
            .to_string();
        if let Some(weights) = config["weights"].as_object() {
            if feature_set == "without_sp500" {
                lgb_weight = weights
                    .get("lightgbm_without_sp500")
                    .and_then(|value| value.as_f64())
                    .unwrap_or(0.0);
                lstm_weight = weights
                    .get("lstm_without_sp500")
                    .and_then(|value| value.as_f64())
                    .unwrap_or(0.0);
                xgb_weight = weights
                    .get("xgboost_without_sp500")
                    .and_then(|value| value.as_f64())
                    .unwrap_or(0.0);
            } else {
                lgb_weight = weights
                    .get("lightgbm")
                    .and_then(|value| value.as_f64())
                    .unwrap_or(0.0);
                lstm_weight = weights
                    .get("lstm")
                    .and_then(|value| value.as_f64())
                    .unwrap_or(0.0);
                xgb_weight = weights
                    .get("xgboost")
                    .and_then(|value| value.as_f64())
                    .unwrap_or(0.0);
            }
        }
    }

    // Zero weights for models whose training steps were skipped.
    // A skipped training step means the model file on disk is stale (from a
    // previous run or missing entirely) — using it would silently degrade
    // prediction quality.  After zeroing, renormalize the remaining weights
    // so they sum to 1.0.
    let skip_steps = crate::config::ml_pipeline_skip_steps();
    let zeroed_models = crate::config::skipped_ensemble_models(&skip_steps);
    if !zeroed_models.is_empty() {
        for model in &zeroed_models {
            match *model {
                "lstm" if lstm_weight > 0.0 => {
                    eprintln!(
                        "  ensemble: zeroing lstm weight ({:.1}% → 0%) — \
                         lstm-variants skipped in ml.skip_steps",
                        lstm_weight * 100.0
                    );
                    lstm_weight = 0.0;
                }
                "xgboost" if xgb_weight > 0.0 => {
                    eprintln!(
                        "  ensemble: zeroing xgboost weight ({:.1}% → 0%) — \
                         baselines skipped in ml.skip_steps",
                        xgb_weight * 100.0
                    );
                    xgb_weight = 0.0;
                }
                "lightgbm" if lgb_weight > 0.0 => {
                    eprintln!(
                        "  ensemble: zeroing lightgbm weight ({:.1}% → 0%) — \
                         lightgbm-train skipped in ml.skip_steps",
                        lgb_weight * 100.0
                    );
                    lgb_weight = 0.0;
                }
                _ => {}
            }
        }
        // Renormalize remaining weights so they sum to 1.0.
        let total = lgb_weight + lstm_weight + xgb_weight;
        if total > f64::EPSILON {
            lgb_weight /= total;
            lstm_weight /= total;
            xgb_weight /= total;
            eprintln!(
                "  ensemble: renormalized weights → lightgbm={:.1}%, lstm={:.1}%, xgboost={:.1}%",
                lgb_weight * 100.0,
                lstm_weight * 100.0,
                xgb_weight * 100.0
            );
        } else {
            anyhow::bail!(
                "All ensemble model weights are zero after skip_steps adjustment. \
                 At least one model training step must remain enabled."
            );
        }
    }

    if feature_set == "without_sp500" {
        cmd_ml_ensemble_without_sp500_weighted(lgb_weight, lstm_weight, xgb_weight, json)
    } else {
        cmd_ml_ensemble_weighted(lgb_weight, lstm_weight, xgb_weight, json)
    }
}

// Handles the ml ensemble without sp500 weighted CLI action.
fn cmd_ml_ensemble_without_sp500_weighted(
    lgb_weight: f64,
    lstm_weight: f64,
    xgb_weight: f64,
    json: bool,
) -> anyhow::Result<()> {
    let conn = open_ml_db()?;
    init_ml_tables(&conn)?;
    init_ensemble_columns(&conn)?;

    let wt = lgb_weight + lstm_weight + xgb_weight;
    if wt <= 0.0 {
        anyhow::bail!("At least one ensemble weight must be greater than zero.");
    }

    let lgb_path = paths::state_dir().join("lightgbm_without_sp500_model.txt");
    if !lgb_path.exists() {
        anyhow::bail!(
            "Missing no-S&P LightGBM model: {}. Run `mlai-trade ml ablate-sp500` first.",
            lgb_path.display()
        );
    }

    let xgb_path = paths::state_dir().join("xgboost_without_sp500_model.json");
    if xgb_weight > 0.0 && !xgb_path.exists() {
        anyhow::bail!(
            "Missing no-S&P XGBoost model: {}. Run `mlai-trade ml xgboost-ablate-sp500` first.",
            xgb_path.display()
        );
    }

    let (latest_date, rows) = latest_eligible_feature_rows(&conn)?;
    let lstm_date: Option<String> = conn
        .query_row(
            "SELECT MAX(date) FROM ml_lstm_predictions_without_sp500",
            [],
            |r| r.get(0),
        )
        .unwrap_or(None);
    if lstm_date.as_deref() != Some(latest_date.as_str()) {
        if json {
            anyhow::bail!(
                "No current no-S&P LSTM predictions for {}. Run `mlai-trade ml lstm-predict --without-sp500` first.",
                latest_date
            );
        }
        eprintln!(
            "Refreshing no-S&P LSTM predictions for latest feature date {}...",
            latest_date
        );
        crate::lstm::cmd_ml_lstm_predict(false, true, crate::lstm::configured_inference_backend())?;
    }

    let without_cols = without_sp500_feature_cols();
    let without_indices = feature_indices_for_cols(&without_cols);
    let lgb_model = LgbModel::load(&lgb_path.to_string_lossy())?;
    let lgb_scores = rows
        .iter()
        .map(|row| lgb_model.predict_one(&select_features(row, &without_indices)))
        .collect::<Vec<_>>();
    let xgb_scores = if xgb_weight > 0.0 {
        Some(xgb_predict_feature_rows(
            &xgb_path,
            &rows,
            &without_indices,
        )?)
    } else {
        None
    };

    let mut lstm_stmt = conn.prepare(
        "SELECT symbol, lstm_score FROM ml_lstm_predictions_without_sp500 WHERE date = ?1",
    )?;
    let lstm_preds: HashMap<String, f64> = lstm_stmt
        .query_map(params![latest_date], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    if lstm_preds.is_empty() {
        anyhow::bail!("No no-S&P LSTM predictions found for date {}.", latest_date);
    }

    let mut raw = Vec::new();
    for (idx, row) in rows.iter().enumerate() {
        let Some(lstm_score) = lstm_preds.get(&row.symbol).copied() else {
            continue;
        };
        let xgb_score = xgb_scores
            .as_ref()
            .and_then(|scores| scores.get(idx).copied());
        raw.push((row.symbol.clone(), lgb_scores[idx], lstm_score, xgb_score));
    }
    if raw.is_empty() {
        anyhow::bail!("No overlapping no-S&P LightGBM/LSTM/XGBoost predictions.");
    }

    let score_stats = |values: Vec<f64>| -> (f64, f64) {
        let avg = mean(&values);
        (avg, stddev(&values, avg).max(1e-9))
    };
    let (lgb_mean, lgb_std) = score_stats(raw.iter().map(|(_, lgb, _, _)| *lgb).collect());
    let (lstm_mean, lstm_std) = score_stats(raw.iter().map(|(_, _, lstm, _)| *lstm).collect());
    let (xgb_mean, xgb_std) = if xgb_weight > 0.0 {
        score_stats(
            raw.iter()
                .filter_map(|(_, _, _, xgb)| *xgb)
                .collect::<Vec<_>>(),
        )
    } else {
        (0.0, 1.0)
    };

    let mut ensemble = raw
        .into_iter()
        .filter_map(|(symbol, lgb_s, lstm_s, xgb_s)| {
            if xgb_weight > 0.0 && xgb_s.is_none() {
                return None;
            }
            let lgb_z = (lgb_s - lgb_mean) / lgb_std;
            let lstm_z = (lstm_s - lstm_mean) / lstm_std;
            let xgb_z = xgb_s
                .map(|score| (score - xgb_mean) / xgb_std)
                .unwrap_or(0.0);
            let ens = (lgb_weight * lgb_z + lstm_weight * lstm_z + xgb_weight * xgb_z) / wt;
            Some((symbol, lgb_s, lstm_s, xgb_s, ens))
        })
        .collect::<Vec<_>>();
    if ensemble.is_empty() {
        anyhow::bail!("No overlapping no-S&P model predictions after XGBoost filtering.");
    }

    ensemble.sort_by(|a, b| b.4.partial_cmp(&a.4).unwrap_or(std::cmp::Ordering::Equal));
    let n = ensemble.len();
    let q_size = (n / 5).max(1);
    let model_version = format!(
        "ensemble_without_sp500_{}",
        chrono::Utc::now().format("%Y%m%d")
    );

    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM ml_predictions WHERE date = ?1",
        params![latest_date],
    )?;
    {
        let mut ins = tx.prepare_cached(
            "INSERT INTO ml_predictions
             (symbol, date, predicted_score, predicted_quintile, model_version,
              lgb_score, lstm_score, xgb_score, ensemble_score)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        for (rank, (sym, lgb_s, lstm_s, xgb_s, ens)) in ensemble.iter().enumerate() {
            let quintile = std::cmp::min((rank / q_size) as i64 + 1, 5);
            ins.execute(params![
                sym,
                latest_date,
                ens,
                quintile,
                model_version,
                lgb_s,
                lstm_s,
                xgb_s,
                ens
            ])?;
        }
    }
    tx.commit()?;

    if json {
        let top: Vec<serde_json::Value> = ensemble
            .iter()
            .take(30)
            .enumerate()
            .map(|(rank, (s, lg, ls, xg, en))| {
                serde_json::json!({
                    "symbol": s,
                    "lgb_score": (lg * 10000.0).round() / 10000.0,
                    "lstm_score": (ls * 10000.0).round() / 10000.0,
                    "xgb_score": xg.map(|value| (value * 10000.0).round() / 10000.0),
                    "ensemble_score": (en * 10000.0).round() / 10000.0,
                    "quintile": std::cmp::min((rank / q_size) as i64 + 1, 5),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "status": "done",
                "date": latest_date,
                "feature_set": "without_sp500",
                "total": n,
                "lgb_weight": lgb_weight,
                "lstm_weight": lstm_weight,
                "xgb_weight": xgb_weight,
                "model_version": model_version,
                "predictions": top,
            })
        );
    } else {
        println!("🎯 Ensemble Rankings (Top 20) — {}", latest_date);
        println!("  Feature set: without_sp500");
        println!(
            "  Weights: no-S&P LGB {:.1}% + no-S&P LSTM {:.1}% + no-S&P XGB {:.1}%",
            lgb_weight * 100.0,
            lstm_weight * 100.0,
            xgb_weight * 100.0
        );
        println!(
            "{:<8} {:>10} {:>10} {:>10} {:>10} {:>4}",
            "Symbol", "LGB", "LSTM", "XGB", "Ensemble", "Q"
        );
        println!("{}", "─".repeat(60));
        for (rank, (sym, lgb_s, lstm_s, xgb_s, ens)) in ensemble.iter().take(20).enumerate() {
            let q = std::cmp::min((rank / q_size) as i64 + 1, 5);
            println!(
                "{:<8} {:>10.4} {:>10.4} {:>10.4} {:>10.4} {:>4}",
                sym,
                lgb_s,
                lstm_s,
                xgb_s.unwrap_or(0.0),
                ens,
                q
            );
        }
        println!("\nTotal: {} symbols with required model predictions", n);
    }

    Ok(())
}

// Handles the ml ensemble weighted CLI action.
pub fn cmd_ml_ensemble_weighted(
    lgb_weight: f64,
    lstm_weight: f64,
    xgb_weight: f64,
    json: bool,
) -> anyhow::Result<()> {
    let conn = open_ml_db()?;
    init_ml_tables(&conn)?;
    init_ensemble_columns(&conn)?;

    // Check LSTM predictions table exists
    let has_lstm = conn
        .prepare("SELECT 1 FROM ml_lstm_predictions LIMIT 1")
        .is_ok();
    if !has_lstm {
        anyhow::bail!("No LSTM predictions found. Run 'mlai-trade ml lstm-predict' first.");
    }

    // Get latest prediction dates
    let lgb_date: String = conn.query_row(
        "SELECT COALESCE(MAX(date),'none') FROM ml_predictions",
        [],
        |r| r.get(0),
    )?;
    let lstm_date: String = conn.query_row(
        "SELECT COALESCE(MAX(date),'none') FROM ml_lstm_predictions",
        [],
        |r| r.get(0),
    )?;

    if lgb_date == "none" {
        anyhow::bail!("No LightGBM predictions found.");
    }
    if lstm_date == "none" {
        anyhow::bail!("No LSTM predictions found.");
    }

    // Use the most recent common date
    let target_date = if lgb_date <= lstm_date {
        &lgb_date
    } else {
        &lstm_date
    };
    eprintln!(
        "Computing ensemble for date {} (weights: LGB={:.0}%, LSTM={:.0}%, XGB={:.0}%)",
        target_date,
        lgb_weight * 100.0,
        lstm_weight * 100.0,
        xgb_weight * 100.0
    );

    // Load LGB predictions for target date
    let mut lgb_stmt =
        conn.prepare("SELECT symbol, predicted_score FROM ml_predictions WHERE date = ?1")?;
    let lgb_preds: std::collections::HashMap<String, f64> = lgb_stmt
        .query_map(params![target_date], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    // Load LSTM predictions for target date
    let mut lstm_stmt =
        conn.prepare("SELECT symbol, lstm_score FROM ml_lstm_predictions WHERE date = ?1")?;
    let lstm_preds: std::collections::HashMap<String, f64> = lstm_stmt
        .query_map(params![target_date], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    let xgb_preds: std::collections::HashMap<String, f64> = if xgb_weight > 0.0 {
        let mut xgb_stmt = conn.prepare(
            "SELECT symbol, xgb_score FROM ml_predictions WHERE date = ?1 AND xgb_score IS NOT NULL",
        )?;
        let preds = xgb_stmt
            .query_map(params![target_date], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        preds
    } else {
        std::collections::HashMap::new()
    };
    let score_stats = |values: &std::collections::HashMap<String, f64>| -> (f64, f64) {
        let vals = values.values().copied().collect::<Vec<_>>();
        let avg = mean(&vals);
        (avg, stddev(&vals, avg).max(1e-9))
    };
    let (lgb_mean, lgb_std) = score_stats(&lgb_preds);
    let (lstm_mean, lstm_std) = score_stats(&lstm_preds);
    let (xgb_mean, xgb_std) = if xgb_weight > 0.0 {
        score_stats(&xgb_preds)
    } else {
        (0.0, 1.0)
    };

    // Compute ensemble for symbols with both predictions
    let mut ensemble: Vec<(String, f64, f64, Option<f64>, f64)> = Vec::new(); // (symbol, lgb, lstm, xgb, ensemble)
    let wt = lgb_weight + lstm_weight + xgb_weight;
    if wt <= 0.0 {
        anyhow::bail!("At least one ensemble weight must be greater than zero.");
    }

    let progress =
        crate::progress::bar_if(!json, lgb_preds.len() as u64, "Computing ensemble scores");
    for (sym, &lgb_s) in &lgb_preds {
        progress.set_message(sym);
        if let Some(&lstm_s) = lstm_preds.get(sym) {
            let xgb_s = if xgb_weight > 0.0 {
                let Some(score) = xgb_preds.get(sym).copied() else {
                    progress.inc(1);
                    continue;
                };
                Some(score)
            } else {
                None
            };
            let lgb_z = (lgb_s - lgb_mean) / lgb_std;
            let lstm_z = (lstm_s - lstm_mean) / lstm_std;
            let xgb_z = xgb_s
                .map(|score| (score - xgb_mean) / xgb_std)
                .unwrap_or(0.0);
            let ens = (lgb_weight * lgb_z + lstm_weight * lstm_z + xgb_weight * xgb_z) / wt;
            ensemble.push((sym.clone(), lgb_s, lstm_s, xgb_s, ens));
        }
        progress.inc(1);
    }
    progress.finish_and_clear();

    if ensemble.is_empty() {
        anyhow::bail!("No overlapping symbols between LGB and LSTM predictions.");
    }

    // Sort by ensemble score desc for quintile assignment
    ensemble.sort_by(|a, b| b.4.partial_cmp(&a.4).unwrap_or(std::cmp::Ordering::Equal));
    let n = ensemble.len();
    let q_size = (n / 5).max(1);

    // Update ml_predictions with ensemble scores + re-ranked quintiles
    let tx = conn.unchecked_transaction()?;
    {
        let mut upd = tx.prepare_cached(
            "UPDATE ml_predictions
             SET lgb_score = ?1, lstm_score = ?2, xgb_score = COALESCE(?3, xgb_score), ensemble_score = ?4,
                 predicted_score = ?4, predicted_quintile = ?5
             WHERE symbol = ?6 AND date = ?7",
        )?;
        let progress =
            crate::progress::bar_if(!json, ensemble.len() as u64, "Writing ensemble predictions");
        for (rank, (sym, lgb_s, lstm_s, xgb_s, ens)) in ensemble.iter().enumerate() {
            let quintile = std::cmp::min((rank / q_size) as i64 + 1, 5);
            upd.execute(params![
                lgb_s,
                lstm_s,
                xgb_s,
                ens,
                quintile,
                sym,
                target_date
            ])?;
            progress.inc(1);
        }
        progress.finish_and_clear();
    }
    tx.commit()?;

    // Display
    if json {
        let top: Vec<serde_json::Value> = ensemble
            .iter()
            .take(30)
            .enumerate()
            .map(|(rank, (s, lg, ls, xg, en))| {
                serde_json::json!({
                    "symbol": s,
                    "lgb_score": (lg * 10000.0).round() / 10000.0,
                    "lstm_score": (ls * 10000.0).round() / 10000.0,
                    "xgb_score": xg.map(|value| (value * 10000.0).round() / 10000.0),
                    "ensemble_score": (en * 10000.0).round() / 10000.0,
                    "quintile": std::cmp::min((rank / q_size) as i64 + 1, 5),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "status": "done",
                "date": target_date,
                "total": n,
                "lgb_weight": lgb_weight,
                "lstm_weight": lstm_weight,
                "xgb_weight": xgb_weight,
                "predictions": top,
            })
        );
    } else {
        println!("🎯 Ensemble Rankings (Top 20) — {}", target_date);
        println!(
            "  Weights: LGB {:.0}% + LSTM {:.0}% + XGB {:.0}%",
            lgb_weight * 100.0,
            lstm_weight * 100.0,
            xgb_weight * 100.0
        );
        println!(
            "{:<8} {:>10} {:>10} {:>10} {:>10} {:>4}",
            "Symbol", "LGB", "LSTM", "XGB", "Ensemble", "Q"
        );
        println!("{}", "─".repeat(60));
        for (rank, (sym, lgb_s, lstm_s, xgb_s, ens)) in ensemble.iter().take(20).enumerate() {
            let q = std::cmp::min((rank / q_size) as i64 + 1, 5);
            println!(
                "{:<8} {:>10.4} {:>10.4} {:>10.4} {:>10.4} {:>4}",
                sym,
                lgb_s,
                lstm_s,
                xgb_s.unwrap_or(0.0),
                ens,
                q
            );
        }
        println!("\nTotal: {} symbols with required model predictions", n);
    }

    Ok(())
}

// Handles the ml compare sp500 final CLI action.
pub fn cmd_ml_compare_sp500_final(
    lgb_weight: f64,
    lstm_weight: f64,
    json_out: bool,
) -> anyhow::Result<()> {
    let conn = open_ml_db()?;
    let latest_date: String = conn.query_row(
        "SELECT COALESCE(MAX(date),'none') FROM ml_features WHERE return_1d IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    if latest_date == "none" {
        anyhow::bail!("No features found.");
    }

    let without_model_path = paths::state_dir().join("lightgbm_without_sp500_model.txt");
    if !without_model_path.exists() {
        anyhow::bail!(
            "Missing no-S&P LightGBM model: {}. Run `mlai-trade ml ablate-sp500` first.",
            without_model_path.display()
        );
    }
    let without_lstm_table_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='ml_lstm_predictions_without_sp500'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if without_lstm_table_exists == 0 {
        anyhow::bail!(
            "Missing no-S&P LSTM predictions. Run `mlai-trade ml lstm-train --without-sp500` and `mlai-trade ml lstm-predict --without-sp500` first."
        );
    }

    let without_cols = FEATURE_COLS
        .iter()
        .copied()
        .filter(|feature| !SP500_FEATURE_COLS.contains(feature))
        .collect::<Vec<_>>();
    let feature_cols = without_cols.join(", ");
    let model = LgbModel::load(&without_model_path.to_string_lossy())?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ml_predictions_without_sp500 (
            symbol TEXT NOT NULL,
            date TEXT NOT NULL,
            lgb_score REAL NOT NULL,
            lstm_score REAL,
            ensemble_score REAL,
            ensemble_quintile INTEGER,
            PRIMARY KEY (symbol, date)
        );
        CREATE INDEX IF NOT EXISTS idx_mlp_no_sp500_date ON ml_predictions_without_sp500(date);",
    )?;

    let query = format!(
        "SELECT symbol, {feature_cols}
         FROM ml_features
         WHERE date = ?1 AND return_1d IS NOT NULL"
    );
    let mut stmt = conn.prepare(&query)?;
    let rows = stmt.query_map(params![latest_date], |r| {
        let symbol: String = r.get(0)?;
        let mut feats = Vec::with_capacity(without_cols.len());
        for i in 0..without_cols.len() {
            let val: Option<f64> = r.get(i + 1)?;
            feats.push(val.unwrap_or(0.0));
        }
        Ok((symbol, feats))
    })?;

    let mut lgb_scores = Vec::new();
    for row in rows {
        let (symbol, feats) = row?;
        if config::is_blocked_symbol(&symbol) {
            continue;
        }
        lgb_scores.push((symbol, model.predict_one(&feats)));
    }

    let mut lstm_stmt = conn.prepare(
        "SELECT symbol, lstm_score FROM ml_lstm_predictions_without_sp500 WHERE date = ?1",
    )?;
    let lstm_scores = lstm_stmt
        .query_map(params![latest_date], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect::<HashMap<_, _>>();

    let weight_sum = lgb_weight + lstm_weight;
    let mut final_scores = lgb_scores
        .into_iter()
        .filter_map(|(symbol, lgb)| {
            lstm_scores.get(&symbol).map(|lstm| {
                let ensemble = (lgb_weight * lgb + lstm_weight * *lstm) / weight_sum;
                (symbol, lgb, *lstm, ensemble)
            })
        })
        .collect::<Vec<_>>();
    final_scores.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));

    let n = final_scores.len();
    if n == 0 {
        anyhow::bail!("No overlapping symbols for no-S&P LightGBM/LSTM final comparison.");
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM ml_predictions_without_sp500 WHERE date = ?1",
        params![latest_date],
    )?;
    {
        let mut ins = tx.prepare_cached(
            "INSERT INTO ml_predictions_without_sp500
             (symbol, date, lgb_score, lstm_score, ensemble_score, ensemble_quintile)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for (rank, (symbol, lgb, lstm, ensemble)) in final_scores.iter().enumerate() {
            let quintile = ((rank * 5) / n.max(1) + 1).min(5) as i64;
            ins.execute(params![symbol, latest_date, lgb, lstm, ensemble, quintile])?;
        }
    }
    tx.commit()?;

    let production_top = {
        let mut stmt = conn.prepare(
            "SELECT symbol FROM ml_predictions
             WHERE date = ?1 AND ensemble_score IS NOT NULL
             ORDER BY ensemble_score DESC LIMIT 20",
        )?;
        let rows = stmt.query_map(params![latest_date], |r| r.get::<_, String>(0))?;
        rows.filter_map(|r| r.ok()).collect::<Vec<_>>()
    };
    let without_top = final_scores
        .iter()
        .take(20)
        .map(|row| row.0.clone())
        .collect::<Vec<_>>();
    let production_set = production_top
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    let overlap = without_top
        .iter()
        .filter(|symbol| production_set.contains(*symbol))
        .count();

    let report_path = paths::state_dir().join("sp500_final_ml_comparison_report.json");
    let report = serde_json::json!({
        "status": "done",
        "date": latest_date,
        "weights": {"lightgbm": lgb_weight, "lstm": lstm_weight},
        "with_sp500": {
            "source": "ml_predictions",
            "top20": production_top,
        },
        "without_sp500": {
            "source": "ml_predictions_without_sp500",
            "rows": n,
            "top20": without_top,
            "top20_scores": final_scores.iter().take(20).map(|(symbol, lgb, lstm, ensemble)| {
                serde_json::json!({"symbol": symbol, "lgb": lgb, "lstm": lstm, "ensemble": ensemble})
            }).collect::<Vec<_>>(),
        },
        "top20_overlap": overlap,
        "note": "Without-S&P LSTM keeps the same 25-input architecture with S&P feature inputs zeroed so architecture stays comparable.",
    });
    paths::write_private_file(&report_path, serde_json::to_string_pretty(&report)?)?;

    if json_out {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("Final S&P 500 ML Comparison — {}", latest_date);
        println!("  Rows compared: {}", n);
        println!("  Top-20 overlap: {}/20", overlap);
        println!("  With S&P 500 top 10:");
        for (idx, symbol) in production_top.iter().take(10).enumerate() {
            println!("    {:>2}. {}", idx + 1, symbol);
        }
        println!("  Without S&P 500 top 10:");
        for (idx, (symbol, lgb, lstm, ensemble)) in final_scores.iter().take(10).enumerate() {
            println!(
                "    {:>2}. {:<8} ensemble={:.4} lgb={:.4} lstm={:.4}",
                idx + 1,
                symbol,
                ensemble,
                lgb,
                lstm
            );
        }
        println!("  Report: {}", report_path.display());
    }

    Ok(())
}

// ── CMD: ml status (upgraded with LSTM/SHAP info) ────────────────

fn load_bars(conn: &Connection, symbol: &str) -> anyhow::Result<Vec<Bar>> {
    let mut stmt = conn.prepare_cached(
        "SELECT date, high, low, close, volume, COALESCE(vwap, 0.0)
         FROM bars WHERE symbol = ?1 ORDER BY date",
    )?;
    let rows = stmt.query_map(params![symbol], |r| {
        Ok(Bar {
            date: r.get(0)?,
            high: r.get(1)?,
            low: r.get(2)?,
            close: r.get(3)?,
            volume: r.get::<_, i64>(4)? as f64,
            vwap: r.get(5)?,
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

// Loads only the target interval plus enough preceding rows for stable indicators.
fn load_bars_window(
    conn: &Connection,
    symbol: &str,
    start: &str,
    end: &str,
    warmup_bars: usize,
) -> anyhow::Result<Vec<Bar>> {
    let mut stmt = conn.prepare_cached(
        "WITH history_start AS (
             SELECT MIN(date) AS date
             FROM (
                 SELECT date
                 FROM bars
                 WHERE symbol=?1 AND date<?2
                 ORDER BY date DESC
                 LIMIT ?4
             )
         )
         SELECT date, high, low, close, volume, COALESCE(vwap, 0.0)
         FROM bars
         WHERE symbol=?1
           AND date>=COALESCE((SELECT date FROM history_start), ?2)
           AND date<=?3
         ORDER BY date",
    )?;
    let rows = stmt.query_map(params![symbol, start, end, warmup_bars as i64], |r| {
        Ok(Bar {
            date: r.get(0)?,
            high: r.get(1)?,
            low: r.get(2)?,
            close: r.get(3)?,
            volume: r.get::<_, i64>(4)? as f64,
            vwap: r.get(5)?,
        })
    })?;
    Ok(rows.filter_map(|row| row.ok()).collect())
}

// Loads sp500 features from storage or configuration.
fn load_sp500_features(conn: &Connection) -> anyhow::Result<ReturnFeatures> {
    load_macro_return_features(conn, "SP500")
}

// Loads macro return features from storage or configuration.
fn load_macro_return_features(
    conn: &Connection,
    series_id: &str,
) -> anyhow::Result<ReturnFeatures> {
    let mut stmt =
        conn.prepare("SELECT date, value FROM macro_series WHERE series_id = ?1 ORDER BY date")?;
    let rows = stmt.query_map(params![series_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
    })?;
    let values: Vec<(String, f64)> = rows.filter_map(|r| r.ok()).collect();
    Ok(compute_return_features(&values))
}

// Loads bar return features from storage or configuration.
fn load_bar_return_features(conn: &Connection, symbol: &str) -> anyhow::Result<ReturnFeatures> {
    let mut stmt = conn.prepare("SELECT date, close FROM bars WHERE symbol = ?1 ORDER BY date")?;
    let rows = stmt.query_map(params![symbol], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
    })?;
    let values: Vec<(String, f64)> = rows.filter_map(|r| r.ok()).collect();
    Ok(compute_return_features(&values))
}

// Computes return features from prepared inputs.
fn compute_return_features(values: &[(String, f64)]) -> ReturnFeatures {
    let mut out = HashMap::new();
    for i in 0..values.len() {
        let current = values[i].1;
        let r1 = if i >= 1 && values[i - 1].1 > 0.0 {
            Some(current / values[i - 1].1 - 1.0)
        } else {
            None
        };
        let r5 = if i >= 5 && values[i - 5].1 > 0.0 {
            Some(current / values[i - 5].1 - 1.0)
        } else {
            None
        };
        let r20 = if i >= 20 && values[i - 20].1 > 0.0 {
            Some(current / values[i - 20].1 - 1.0)
        } else {
            None
        };
        out.insert(values[i].0.clone(), (r1, r5, r20));
    }
    out
}

// Loads vix features from storage or configuration.
fn load_vix_features(conn: &Connection) -> anyhow::Result<VixFeatures> {
    let mut stmt = conn
        .prepare("SELECT date, value FROM macro_series WHERE series_id='VIXCLS' ORDER BY date")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))?;
    let values: Vec<(String, f64)> = rows.filter_map(|r| r.ok()).collect();
    let mut out = HashMap::new();
    for i in 0..values.len() {
        let current = values[i].1;
        let r1 = if i >= 1 && values[i - 1].1 > 0.0 {
            Some(current / values[i - 1].1 - 1.0)
        } else {
            None
        };
        let r5 = if i >= 5 && values[i - 5].1 > 0.0 {
            Some(current / values[i - 5].1 - 1.0)
        } else {
            None
        };
        let r20 = if i >= 20 && values[i - 20].1 > 0.0 {
            Some(current / values[i - 20].1 - 1.0)
        } else {
            None
        };
        out.insert(values[i].0.clone(), (Some(current), r1, r5, r20));
    }
    Ok(out)
}

// Loads sector avg 20d from storage or configuration.
fn load_sector_avg_20d(conn: &Connection) -> anyhow::Result<HashMap<String, Option<f64>>> {
    let mut by_date: HashMap<String, Vec<f64>> = HashMap::new();
    for symbol in SECTOR_ETFS {
        for (date, (_, _, r20)) in load_bar_return_features(conn, symbol)? {
            if let Some(value) = r20 {
                by_date.entry(date).or_default().push(value);
            }
        }
    }

    let mut out = HashMap::new();
    for (date, values) in by_date {
        if values.is_empty() {
            out.insert(date, None);
        } else {
            out.insert(date, Some(values.iter().sum::<f64>() / values.len() as f64));
        }
    }
    Ok(out)
}

// Parses feed date from user or provider input.
fn parse_feed_date(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value.get(..10)?, "%Y-%m-%d").ok()
}

// Loads feed context from storage or configuration.
fn load_feed_context(
    conn: &Connection,
) -> anyhow::Result<HashMap<String, HashMap<String, FeedAgg>>> {
    let trading_dates: Vec<String> = {
        let mut stmt = conn.prepare("SELECT DISTINCT date FROM bars ORDER BY date")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.filter_map(|row| row.ok()).collect()
    };
    if trading_dates.is_empty() {
        return Ok(HashMap::new());
    }

    let trading_date_values = trading_dates
        .iter()
        .filter_map(|date| parse_feed_date(date).map(|parsed| (date.clone(), parsed)))
        .collect::<Vec<_>>();
    let trading_day_by_date = trading_date_values
        .iter()
        .enumerate()
        .map(|(idx, (_, date))| (*date, idx))
        .collect::<HashMap<_, _>>();

    let mut context: HashMap<String, HashMap<String, FeedAgg>> = HashMap::new();
    let mut stmt = match conn.prepare(
        "SELECT article_symbol.symbol, article.published_date,
                article.sentiment_score, article.filing_type
         FROM news_article_symbols article_symbol
         JOIN news_articles article ON article.id=article_symbol.article_id
         WHERE article.published_date IS NOT NULL",
    ) {
        Ok(stmt) => stmt,
        Err(_) => return Ok(context),
    };
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<f64>>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;

    for row in rows {
        let (symbol, published_date, sentiment, filing_type) = row?;
        let Some(published_date) = published_date.and_then(|date| parse_feed_date(&date)) else {
            continue;
        };
        let start_idx = trading_day_by_date
            .get(&published_date)
            .copied()
            .or_else(|| {
                trading_date_values
                    .iter()
                    .position(|(_, date)| *date > published_date)
            });
        let Some(start_idx) = start_idx else {
            continue;
        };
        let sentiment = sentiment.unwrap_or(0.0);
        let by_date = context.entry(symbol).or_default();
        for offset in 1..=30 {
            let idx = start_idx + offset;
            let Some((feature_date, _)) = trading_date_values.get(idx) else {
                break;
            };
            by_date.entry(feature_date.clone()).or_default().add(
                offset,
                sentiment,
                filing_type.as_deref(),
            );
        }
    }

    Ok(context)
}

// Builds point-in-time equal-weight returns for the managed feed universe.
fn load_feed_universe_context(conn: &Connection) -> anyhow::Result<FeedUniverseContext> {
    let cfg = config::feeds_correlation_config();
    let mut stmt = match conn.prepare(
        "WITH feed_symbols AS (
            SELECT symbol
            FROM feed_subscriptions
            ORDER BY managed DESC, symbol ASC
            LIMIT ?1
         )
         SELECT b.symbol, b.date, b.close
         FROM bars b
         JOIN feed_symbols fs ON fs.symbol = b.symbol
         WHERE b.close IS NOT NULL AND b.close > 0
         ORDER BY b.symbol, b.date",
    ) {
        Ok(stmt) => stmt,
        Err(_) => {
            return Ok(FeedUniverseContext {
                min_overlap_days: cfg.min_overlap_days,
                ..FeedUniverseContext::default()
            })
        }
    };
    let rows = stmt.query_map(params![cfg.max_symbols as i64], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, f64>(2)?,
        ))
    })?;

    let mut by_date: HashMap<String, (f64, usize)> = HashMap::new();
    let mut current_symbol = String::new();
    let mut previous_close = 0.0;
    for row in rows {
        let (symbol, date, close) = row?;
        if symbol != current_symbol {
            current_symbol = symbol;
            previous_close = close;
            continue;
        }
        if previous_close > 0.0 {
            let daily_return = close / previous_close - 1.0;
            if daily_return.is_finite() {
                let entry = by_date.entry(date).or_insert((0.0, 0));
                entry.0 += daily_return;
                entry.1 += 1;
            }
        }
        previous_close = close;
    }

    let mut daily_return = HashMap::new();
    let mut dated_returns = by_date
        .into_iter()
        .filter_map(|(date, (sum, count))| {
            if count > 0 {
                let value = sum / count as f64;
                daily_return.insert(date.clone(), value);
                Some((date, value))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    dated_returns.sort_by(|a, b| a.0.cmp(&b.0));

    let mut synthetic_index = Vec::with_capacity(dated_returns.len());
    let mut level = 100.0;
    for (_, daily) in &dated_returns {
        level *= 1.0 + daily;
        synthetic_index.push(level);
    }
    let mut return_20d = HashMap::new();
    for i in 20..dated_returns.len() {
        let base = synthetic_index[i - 20];
        if base > 0.0 {
            return_20d.insert(dated_returns[i].0.clone(), synthetic_index[i] / base - 1.0);
        }
    }

    Ok(FeedUniverseContext {
        daily_return,
        return_20d,
        min_overlap_days: cfg.min_overlap_days,
    })
}

// Loads market context from storage or configuration.
fn load_market_context(conn: &Connection) -> anyhow::Result<MarketContext> {
    Ok(MarketContext {
        sp500: load_sp500_features(conn)?,
        spy: load_bar_return_features(conn, "SPY")?,
        qqq: load_bar_return_features(conn, "QQQ")?,
        vix: load_vix_features(conn)?,
        sector_avg_20d: load_sector_avg_20d(conn)?,
        feeds: load_feed_context(conn)?,
        feed_universe: load_feed_universe_context(conn)?,
    })
}

#[cfg(test)]
mod optimization_tests {
    use super::*;

    fn assert_optional_close(left: Option<f64>, right: Option<f64>) {
        match (left, right) {
            (Some(left), Some(right)) => {
                assert!((left - right).abs() < 1e-10, "{left} != {right}")
            }
            (None, None) => {}
            values => panic!("optional feature mismatch: {values:?}"),
        }
    }

    #[test]
    fn bounded_feature_history_matches_full_history_for_target_dates() -> anyhow::Result<()> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE bars (
                 symbol TEXT NOT NULL, date TEXT NOT NULL,
                 high REAL, low REAL, close REAL, volume INTEGER, vwap REAL,
                 PRIMARY KEY(symbol, date)
             );",
        )?;
        let first = NaiveDate::from_ymd_opt(2023, 1, 1).unwrap();
        for idx in 0..800 {
            let date = (first + chrono::Duration::days(idx))
                .format("%Y-%m-%d")
                .to_string();
            let close = 100.0 + idx as f64 * 0.05 + (idx as f64 / 11.0).sin();
            conn.execute(
                "INSERT INTO bars(symbol, date, high, low, close, volume, vwap)
                 VALUES ('TEST', ?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    date,
                    close * 1.01,
                    close * 0.99,
                    close,
                    1_000_000 + idx * 100,
                    close * 0.999
                ],
            )?;
        }
        let all_bars = load_bars(&conn, "TEST")?;
        let output_dates = all_bars[760..780]
            .iter()
            .map(|bar| bar.date.clone())
            .collect::<HashSet<_>>();
        let full = compute_features_for_symbol(
            &all_bars,
            "TEST",
            &FeedUniverseContext::default(),
            Some(&output_dates),
        );
        let bounded_bars = load_bars_window(
            &conn,
            "TEST",
            &all_bars[760].date,
            &all_bars[779].date,
            INCREMENTAL_FEATURE_WARMUP_BARS,
        )?;
        let bounded = compute_features_for_symbol(
            &bounded_bars,
            "TEST",
            &FeedUniverseContext::default(),
            Some(&output_dates),
        );
        assert_eq!(full.len(), bounded.len());
        for (full, bounded) in full.iter().zip(&bounded) {
            assert_eq!(full.symbol, bounded.symbol);
            assert_eq!(full.date, bounded.date);
            assert_optional_close(full.return_60d, bounded.return_60d);
            assert_optional_close(full.volatility_20d, bounded.volatility_20d);
            assert_optional_close(full.rsi_14, bounded.rsi_14);
            assert_optional_close(full.macd_signal, bounded.macd_signal);
            assert_optional_close(full.sma_cross_50_200, bounded.sma_cross_50_200);
            assert_optional_close(full.obv_slope_20d, bounded.obv_slope_20d);
        }
        Ok(())
    }

    #[test]
    fn labels_use_each_symbols_actual_future_bars() -> anyhow::Result<()> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE bars (
                 symbol TEXT NOT NULL, date TEXT NOT NULL, close REAL,
                 PRIMARY KEY(symbol, date)
             );
             CREATE TABLE ml_labels (
                 symbol TEXT NOT NULL, date TEXT NOT NULL,
                 fwd_5d REAL, fwd_10d REAL, fwd_20d REAL,
                 PRIMARY KEY(symbol, date)
             );",
        )?;
        let first = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        for idx in 0..30 {
            let date = (first + chrono::Duration::days(idx))
                .format("%Y-%m-%d")
                .to_string();
            conn.execute(
                "INSERT INTO bars(symbol, date, close) VALUES ('FULL', ?1, ?2)",
                params![date, 100.0 + idx as f64],
            )?;
            if idx % 2 == 0 {
                conn.execute(
                    "INSERT INTO bars(symbol, date, close) VALUES ('GAP', ?1, ?2)",
                    params![date, 100.0 + (idx / 2) as f64],
                )?;
            }
        }
        let base_date = first.format("%Y-%m-%d").to_string();
        assert_eq!(
            upsert_labels_for_dates(&conn, std::slice::from_ref(&base_date))?,
            2
        );
        let gap_labels: (f64, f64, Option<f64>) = conn.query_row(
            "SELECT fwd_5d, fwd_10d, fwd_20d FROM ml_labels
             WHERE symbol='GAP' AND date=?1",
            params![base_date],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert!((gap_labels.0 - 0.05).abs() < 1e-12);
        assert!((gap_labels.1 - 0.10).abs() < 1e-12);
        assert_eq!(gap_labels.2, None);
        Ok(())
    }
}
