// ══════════════════════════════════════════════════════════════════
// LSTM MODULE — Long Short-Term Memory for stock return prediction
// ══════════════════════════════════════════════════════════════════
//
// Pure Rust implementation. No Python, no external ML framework.
//
// Default CPU architecture:
//   Input:  20-day lookback × feature columns (same FEATURE_COLS as LightGBM)
//   LSTM:   64 hidden units, 1 layer
//   Output: Linear(hidden → 1) → predicted 5-day forward return
//
// MLX/TCH tuning profiles can use wider hidden layers when accelerators exist.
// Training: Mini-batch BPTT with Adam optimizer.
//
// Function map:
// - resolve_lstm_backend(): auto-selects MLX/TCH/CPU where available.
// - load_sequences(): streams/samples feature windows for memory-bounded train.
// - cmd_ml_lstm_train*(): trains CPU or accelerated LSTM variants.
// - cmd_ml_lstm_predict/evaluate(): writes predictions and trading metrics.
// ══════════════════════════════════════════════════════════════════

use crate::{config, paths};
use rayon::prelude::*;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::io::{Read, Write};
use std::panic::AssertUnwindSafe;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LstmBackend {
    Auto,
    Cpu,
    Mlx,
    Tch,
}

#[derive(Debug, Clone, Default)]
pub struct LstmTrainOverrides {
    pub target_mode: Option<String>,
    pub hidden_dim: Option<usize>,
    pub epochs: Option<usize>,
    pub learning_rate: Option<f64>,
    pub loss_function: Option<String>,
    pub huber_delta: Option<f64>,
    pub dropout_rate: Option<f64>,
    pub weight_decay: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct LstmDataWindow {
    pub start_date: String,
    pub end_date: String,
}

impl LstmDataWindow {
    // Builds an inclusive date filter for the latest N fully completed calendar months.
    pub fn last_full_months(conn: &Connection, months: u32) -> anyhow::Result<Self> {
        if months == 0 {
            anyhow::bail!("LSTM full-month window must be at least 1 month");
        }
        let max_date: String = conn.query_row(
            "SELECT COALESCE(MAX(date), '') FROM ml_features WHERE date IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        if max_date.is_empty() {
            anyhow::bail!("No ML feature dates available for LSTM full-month window");
        }

        let end_date: String = conn.query_row(
            "SELECT date(date(?1, 'start of month'), '-1 day')",
            [&max_date],
            |row| row.get(0),
        )?;
        let start_modifier = format!("-{} months", months.saturating_sub(1));
        let start_date: String = conn.query_row(
            "SELECT date(date(?1, 'start of month'), ?2)",
            params![end_date, start_modifier],
            |row| row.get(0),
        )?;

        Ok(Self {
            start_date,
            end_date,
        })
    }

    // Builds an inclusive target-date filter for the latest N labeled full days.
    pub fn last_full_days(conn: &Connection, days: u32) -> anyhow::Result<Self> {
        if days == 0 {
            anyhow::bail!("LSTM full-day window must be at least 1 day");
        }
        let end_date: String = conn.query_row(
            "SELECT COALESCE(MAX(date), '') FROM ml_labels WHERE date IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        if end_date.is_empty() {
            anyhow::bail!("No ML label dates available for LSTM full-day window");
        }
        let start_modifier = format!("-{} days", days.saturating_sub(1));
        let start_date: String = conn.query_row(
            "SELECT date(?1, ?2)",
            params![end_date, start_modifier],
            |row| row.get(0),
        )?;

        Ok(Self {
            start_date,
            end_date,
        })
    }

    // Returns true when this target date is inside the inclusive window.
    fn contains(&self, date: &str) -> bool {
        date >= self.start_date.as_str() && date <= self.end_date.as_str()
    }
}

impl Serialize for LstmDataWindow {
    // Serializes the selected target-date window into reports.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("LstmDataWindow", 2)?;
        state.serialize_field("start_date", &self.start_date)?;
        state.serialize_field("end_date", &self.end_date)?;
        state.end()
    }
}

// Resolves the configured LSTM data window against the active runtime DB.
pub fn last_full_months_window(months: u32) -> anyhow::Result<LstmDataWindow> {
    let conn = open_lstm_db()?;
    LstmDataWindow::last_full_months(&conn, months)
}

// Resolves an LSTM data window for the latest N labeled full days.
pub fn last_full_days_window(days: u32) -> anyhow::Result<LstmDataWindow> {
    let conn = open_lstm_db()?;
    LstmDataWindow::last_full_days(&conn, days)
}

impl std::str::FromStr for LstmBackend {
    type Err = anyhow::Error;

    // Parses this value from a CLI or config string.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "cpu" | "rust" | "rayon" => Ok(Self::Cpu),
            "mlx" | "metal" | "apple" => Ok(Self::Mlx),
            "tch" | "cuda" | "nvidia" | "torch" => Ok(Self::Tch),
            _ => anyhow::bail!(
                "Unsupported LSTM backend '{}'. Use auto, cpu, mlx, or tch.",
                value
            ),
        }
    }
}

impl std::fmt::Display for LstmBackend {
    // Formats this value for display and config output.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => write!(f, "auto"),
            Self::Cpu => write!(f, "cpu"),
            Self::Mlx => write!(f, "mlx"),
            Self::Tch => write!(f, "tch"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetMode {
    Regression,
    Direction,
}

impl TargetMode {
    // Parses a training target mode from config or CLI text.
    fn parse(value: &str) -> anyhow::Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "regression" | "return" | "returns" => Ok(Self::Regression),
            "direction" | "classification" | "binary" => Ok(Self::Direction),
            _ => anyhow::bail!(
                "Unsupported LSTM target mode '{}'. Use regression or direction.",
                value
            ),
        }
    }

    // Restores target mode metadata from the binary model header.
    fn from_u32(value: u32) -> anyhow::Result<Self> {
        match value {
            0 => Ok(Self::Regression),
            1 => Ok(Self::Direction),
            _ => anyhow::bail!("Invalid LSTM model target mode: {value}"),
        }
    }

    // Serializes target mode metadata into the binary model header.
    fn as_u32(self) -> u32 {
        match self {
            Self::Regression => 0,
            Self::Direction => 1,
        }
    }

    // Converts a forward return into the trainable target for this mode.
    fn target_value(self, fwd_return: f64, threshold: f64) -> f64 {
        match self {
            Self::Regression => fwd_return,
            Self::Direction => {
                if fwd_return > threshold {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }

    // Converts the raw output logit into the model score.
    fn activate(self, raw: f64) -> f64 {
        match self {
            Self::Regression => raw,
            Self::Direction => sigmoid(raw),
        }
    }

    // Maps dLoss/dScore to dLoss/dRawOutput.
    fn output_grad(self, score: f64, grad_score: f64) -> f64 {
        match self {
            Self::Regression => grad_score,
            Self::Direction => grad_score * score * (1.0 - score),
        }
    }

    // Formats this target mode for reports and logs.
    fn as_str(self) -> &'static str {
        match self {
            Self::Regression => "regression",
            Self::Direction => "direction",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LstmLossKind {
    Mse,
    Huber,
    L1,
    Bce,
}

impl LstmLossKind {
    // Parses the robust loss function configured for LSTM training.
    fn parse(value: &str) -> anyhow::Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mse" | "mse_loss" => Ok(Self::Mse),
            "huber" | "huber_loss" => Ok(Self::Huber),
            "l1" | "mae" | "l1_loss" => Ok(Self::L1),
            "bce" | "binary_cross_entropy" | "binary_crossentropy" => Ok(Self::Bce),
            _ => anyhow::bail!(
                "Unsupported LSTM loss '{}'. Use mse, huber, l1, or bce.",
                value
            ),
        }
    }

    // Returns the sample loss and dLoss/dScore before batch averaging.
    fn sample_loss_and_grad(self, pred: f64, target: f64, huber_delta: f64) -> (f64, f64) {
        let err = pred - target;
        match self {
            Self::Mse => (err * err, 2.0 * err),
            Self::L1 => (err.abs(), err.signum()),
            Self::Huber => {
                let delta = huber_delta.max(1e-12);
                let abs = err.abs();
                if abs <= delta {
                    (0.5 * err * err, err)
                } else {
                    (delta * (abs - 0.5 * delta), delta * err.signum())
                }
            }
            Self::Bce => {
                let prob = pred.clamp(1e-6, 1.0 - 1e-6);
                let target = target.clamp(0.0, 1.0);
                let loss = -(target * prob.ln() + (1.0 - target) * (1.0 - prob).ln());
                let grad = (prob - target) / (prob * (1.0 - prob)).max(1e-12);
                (loss, grad)
            }
        }
    }
}

// Rejects target/loss combinations that do not have valid gradient semantics.
fn validate_loss_for_target(
    loss_kind: LstmLossKind,
    target_mode: TargetMode,
) -> anyhow::Result<()> {
    if loss_kind == LstmLossKind::Bce && target_mode != TargetMode::Direction {
        anyhow::bail!("LSTM loss 'bce' requires target_mode=direction.");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Serialize)]
struct TargetScaler {
    mean: f64,
    std: f64,
    enabled: bool,
}

impl TargetScaler {
    // Keeps classification probabilities unscaled and scales only regression returns.
    fn fit(target_mode: TargetMode, returns: &[f64]) -> Self {
        if target_mode != TargetMode::Regression || returns.is_empty() {
            return Self::identity();
        }
        let mean = returns.iter().sum::<f64>() / returns.len() as f64;
        let variance = returns
            .iter()
            .map(|value| {
                let diff = value - mean;
                diff * diff
            })
            .sum::<f64>()
            / returns.len() as f64;
        let std = variance.sqrt().max(1e-8);
        Self {
            mean,
            std,
            enabled: true,
        }
    }

    // Returns an identity transform for old models and classification targets.
    fn identity() -> Self {
        Self {
            mean: 0.0,
            std: 1.0,
            enabled: false,
        }
    }

    // Converts a raw forward return into training space.
    fn encode(self, value: f64) -> f64 {
        if self.enabled {
            (value - self.mean) / self.std
        } else {
            value
        }
    }

    // Converts a regression training-space prediction back to raw return space.
    fn decode(self, value: f64) -> f64 {
        if self.enabled {
            value * self.std + self.mean
        } else {
            value
        }
    }
}

#[cfg(mlai_mlx)]
// Handles MLX auto backend acceleration support.
fn mlx_auto_backend() -> Option<LstmBackend> {
    Some(LstmBackend::Mlx)
}

#[cfg(not(mlai_mlx))]
// Handles MLX auto backend acceleration support.
fn mlx_auto_backend() -> Option<LstmBackend> {
    None
}

#[cfg(mlai_tch)]
// Handles tch/CUDA auto backend acceleration support.
fn tch_auto_backend() -> Option<LstmBackend> {
    if tch::Cuda::is_available() {
        eprintln!(
            "⚠️  LSTM auto backend: CUDA is available, but tch/CUDA LSTM training is not implemented yet; falling back to CPU/Rayon."
        );
    } else {
        eprintln!(
            "⚠️  LSTM auto backend: Linux tch/libtorch is linked, but CUDA is not available at runtime; falling back to CPU/Rayon."
        );
    }
    None
}

#[cfg(not(mlai_tch))]
// Handles tch/CUDA auto backend acceleration support.
fn tch_auto_backend() -> Option<LstmBackend> {
    None
}

// Resolves lstm backend using config and defaults.
fn resolve_lstm_backend(requested: LstmBackend) -> anyhow::Result<LstmBackend> {
    match requested {
        LstmBackend::Auto => {
            if let Some(backend) = mlx_auto_backend().or_else(tch_auto_backend) {
                return Ok(backend);
            }
            #[cfg(all(not(mlai_mlx), target_os = "macos", target_arch = "aarch64"))]
            {
                eprintln!(
                    "⚠️  LSTM auto backend: Apple Silicon detected, but MLX platform cfg was not enabled; falling back to CPU/Rayon."
                );
            }
            #[cfg(all(not(mlai_tch), target_os = "linux"))]
            {
                eprintln!(
                    "⚠️  LSTM auto backend: Linux detected, but tch platform cfg was not enabled; falling back to CPU/Rayon."
                );
            }
            Ok(LstmBackend::Cpu)
        }
        LstmBackend::Cpu => Ok(LstmBackend::Cpu),
        LstmBackend::Mlx => {
            #[cfg(mlai_mlx)]
            {
                Ok(LstmBackend::Mlx)
            }
            #[cfg(not(mlai_mlx))]
            {
                anyhow::bail!(
                    "MLX LSTM backend was requested, but it is not available. Requirements: Apple Silicon macOS, Xcode or Xcode Command Line Tools, and Apple Metal Toolchain (`xcodebuild -downloadComponent MetalToolchain`)."
                )
            }
        }
        LstmBackend::Tch => {
            #[cfg(mlai_tch)]
            {
                if tch::Cuda::is_available() {
                    Ok(LstmBackend::Tch)
                } else {
                    anyhow::bail!(
                        "tch/CUDA LSTM backend was requested, but CUDA is not available at runtime."
                    )
                }
            }
            #[cfg(not(mlai_tch))]
            {
                anyhow::bail!(
                    "tch/CUDA LSTM backend was requested, but it is not available. Requirements: Linux, NVIDIA driver/CUDA visible through `nvidia-smi`, libtorch available to torch-sys, and native build tools."
                )
            }
        }
    }
}

// Combines config-file LSTM policy with one-off CLI overrides.
fn resolve_training_config(
    backend: LstmBackend,
    overrides: LstmTrainOverrides,
) -> anyhow::Result<(config::LstmTrainingConfig, TargetMode)> {
    let mut train_cfg = config::lstm_training_config_for_backend(&backend.to_string());
    if let Some(target_mode) = overrides.target_mode {
        train_cfg.target_mode = target_mode;
    }
    if let Some(hidden_dim) = overrides.hidden_dim {
        train_cfg.hidden_dim = hidden_dim.clamp(16, 512);
    }
    if let Some(epochs) = overrides.epochs {
        train_cfg.epochs = epochs.clamp(1, 200);
    }
    if let Some(learning_rate) = overrides.learning_rate {
        train_cfg.learning_rate = learning_rate.clamp(0.000_001, 0.1);
    }
    if let Some(loss_function) = overrides.loss_function {
        train_cfg.loss_function = loss_function;
    }
    if let Some(huber_delta) = overrides.huber_delta {
        train_cfg.huber_delta = huber_delta.clamp(0.000_001, 1.0);
    }
    if let Some(dropout_rate) = overrides.dropout_rate {
        train_cfg.dropout_rate = dropout_rate.clamp(0.0, 0.9);
    }
    if let Some(weight_decay) = overrides.weight_decay {
        train_cfg.weight_decay = weight_decay.clamp(0.0, 1.0);
    }
    let target_mode = TargetMode::parse(&train_cfg.target_mode)?;
    let loss_kind = LstmLossKind::parse(&train_cfg.loss_function)?;
    validate_loss_for_target(loss_kind, target_mode)?;
    Ok((train_cfg, target_mode))
}

// ── Feature columns (must match ml.rs) ───────────────────────────

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

const INPUT_DIM: usize = FEATURE_COLS.len();
const SEQ_LEN: usize = 20; // 20-day lookback

// Handles sp500 feature indices logic.
fn sp500_feature_indices() -> Vec<usize> {
    FEATURE_COLS
        .iter()
        .enumerate()
        .filter_map(|(idx, feature)| SP500_FEATURE_COLS.contains(feature).then_some(idx))
        .collect()
}

// Handles zero sp500 features logic.
fn zero_sp500_features(sequence: &mut [Vec<f64>]) {
    let indices = sp500_feature_indices();
    for step in sequence {
        for &idx in &indices {
            if let Some(value) = step.get_mut(idx) {
                *value = 0.0;
            }
        }
    }
}

// Returns LSTM model path runtime settings.
fn lstm_model_path(without_sp500: bool) -> std::path::PathBuf {
    if without_sp500 {
        paths::state_dir().join("lstm_sequence_model_without_sp500.bin")
    } else {
        paths::lstm_model_path()
    }
}

// Returns LSTM predictions table runtime settings.
fn lstm_predictions_table(without_sp500: bool) -> &'static str {
    if without_sp500 {
        "ml_lstm_predictions_without_sp500"
    } else {
        "ml_lstm_predictions"
    }
}

// ── Simple RNG (xoshiro256** seeded) ─────────────────────────────

struct Rng {
    s: [u64; 4],
}

impl Rng {
    // Constructs a new instance with the provided inputs.
    fn new(seed: u64) -> Self {
        let s = [
            seed,
            seed.wrapping_mul(6364136223846793005).wrapping_add(1),
            seed.wrapping_mul(1442695040888963407).wrapping_add(3),
            seed.wrapping_mul(3935559000370003845).wrapping_add(7),
        ];
        // warm up
        let mut r = Rng { s };
        for _ in 0..20 {
            r.next_u64();
        }
        r
    }
    // Returns the next u64 value.
    fn next_u64(&mut self) -> u64 {
        let result = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }
    // Returns the next f64 value.
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Standard normal via Box-Muller
    fn randn(&mut self) -> f64 {
        let u1 = self.next_f64().max(1e-15);
        let u2 = self.next_f64();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

// ── Matrix helpers (row-major flat vectors) ──────────────────────

fn _mat_zeros(rows: usize, cols: usize) -> Vec<f64> {
    vec![0.0; rows * cols]
}

// Handles mat xavier logic.
fn mat_xavier(rows: usize, cols: usize, rng: &mut Rng) -> Vec<f64> {
    let scale = (2.0 / (rows + cols) as f64).sqrt();
    (0..rows * cols).map(|_| rng.randn() * scale).collect()
}

// Handles vec zeros logic.
fn vec_zeros(n: usize) -> Vec<f64> {
    vec![0.0; n]
}

/// y = W * x  where W is (rows × cols), x is (cols,), y is (rows,)
#[inline]
fn mat_vec_mul(w: &[f64], x: &[f64], rows: usize, cols: usize) -> Vec<f64> {
    let mut y = vec![0.0; rows];
    for r in 0..rows {
        let base = r * cols;
        let mut sum = 0.0;
        for c in 0..cols {
            sum += w[base + c] * x[c];
        }
        y[r] = sum;
    }
    y
}

#[inline]
// Applies the sigmoid activation function.
fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

// ── LSTM Model ───────────────────────────────────────────────────

#[derive(Clone)]
pub struct LstmModel {
    hidden_dim: usize,
    target_mode: TargetMode,
    direction_threshold: f64,
    target_scaler: TargetScaler,

    // Gate weights: each is hidden_dim × gate_dim (concatenated [h_{t-1}, x_t])
    w_i: Vec<f64>,
    b_i: Vec<f64>, // input gate
    w_f: Vec<f64>,
    b_f: Vec<f64>, // forget gate
    w_o: Vec<f64>,
    b_o: Vec<f64>, // output gate
    w_c: Vec<f64>,
    b_c: Vec<f64>, // cell candidate

    // Output projection: hidden_dim → 1
    w_out: Vec<f64>,
    b_out: f64,
}

struct LstmTrainingOutcome {
    losses: Vec<f64>,
    validation_losses: Vec<f64>,
    best_epoch: usize,
    best_validation_loss: f64,
    stopped_early: bool,
}

// Adam state for one parameter vector
struct AdamState {
    m: Vec<f64>,
    v: Vec<f64>,
    t: f64,
}

impl AdamState {
    // Constructs a new instance with the provided inputs.
    fn new(size: usize) -> Self {
        AdamState {
            m: vec![0.0; size],
            v: vec![0.0; size],
            t: 0.0,
        }
    }
    // Handles step logic.
    fn step(&mut self, params: &mut [f64], grads: &[f64], lr: f64) {
        let beta1: f64 = 0.9;
        let beta2: f64 = 0.999;
        let eps = 1e-8;
        self.t += 1.0;
        let bc1 = 1.0_f64 - beta1.powf(self.t);
        let bc2 = 1.0_f64 - beta2.powf(self.t);
        for i in 0..params.len() {
            self.m[i] = beta1 * self.m[i] + (1.0 - beta1) * grads[i];
            self.v[i] = beta2 * self.v[i] + (1.0 - beta2) * grads[i] * grads[i];
            let m_hat = self.m[i] / bc1;
            let v_hat = self.v[i] / bc2;
            params[i] -= lr * m_hat / (v_hat.sqrt() + eps);
        }
    }

    // Handles step scalar logic.
    fn step_scalar(&mut self, param: &mut f64, grad: f64, lr: f64) {
        let beta1: f64 = 0.9;
        let beta2: f64 = 0.999;
        let eps = 1e-8;
        self.t += 1.0;
        let bc1 = 1.0_f64 - beta1.powf(self.t);
        let bc2 = 1.0_f64 - beta2.powf(self.t);
        self.m[0] = beta1 * self.m[0] + (1.0 - beta1) * grad;
        self.v[0] = beta2 * self.v[0] + (1.0 - beta2) * grad * grad;
        let m_hat = self.m[0] / bc1;
        let v_hat = self.v[0] / bc2;
        *param -= lr * m_hat / (v_hat.sqrt() + eps);
    }
}

// Cache for BPTT
struct StepCache {
    hx: Vec<f64>,     // concatenated [h_{t-1}, x_t]
    ig: Vec<f64>,     // input gate (after sigmoid)
    fg: Vec<f64>,     // forget gate
    og: Vec<f64>,     // output gate
    cc: Vec<f64>,     // cell candidate (after tanh)
    c_prev: Vec<f64>, // cell state from previous step
    _c: Vec<f64>,     // cell state after this step (kept for debug)
    h: Vec<f64>,      // hidden state after this step
    tanh_c: Vec<f64>, // tanh(c)
}

struct OutputDropoutCache {
    dropped_h: Vec<f64>,
    mask_scale: Vec<f64>,
}

impl LstmModel {
    // Returns the concatenated [hidden, input] gate width.
    fn gate_dim(&self) -> usize {
        INPUT_DIM + self.hidden_dim
    }

    // Handles new random logic.
    pub fn new_random(
        seed: u64,
        hidden_dim: usize,
        target_mode: TargetMode,
        direction_threshold: f64,
    ) -> Self {
        let mut rng = Rng::new(seed);
        let hd = hidden_dim;
        let gd = INPUT_DIM + hd;

        let mut m = LstmModel {
            hidden_dim: hd,
            target_mode,
            direction_threshold,
            target_scaler: TargetScaler::identity(),
            w_i: mat_xavier(hd, gd, &mut rng),
            b_i: vec_zeros(hd),
            w_f: mat_xavier(hd, gd, &mut rng),
            b_f: vec![1.0; hd], // bias init=1 for forget gate
            w_o: mat_xavier(hd, gd, &mut rng),
            b_o: vec_zeros(hd),
            w_c: mat_xavier(hd, gd, &mut rng),
            b_c: vec_zeros(hd),
            w_out: mat_xavier(1, hd, &mut rng),
            b_out: 0.0,
        };
        // Scale output weights smaller for regression
        for v in m.w_out.iter_mut() {
            *v *= 0.1;
        }
        m
    }

    /// Forward pass through one time step. Returns (h_t, c_t, cache).
    fn step_forward(
        &self,
        x: &[f64],
        h_prev: &[f64],
        c_prev: &[f64],
    ) -> (Vec<f64>, Vec<f64>, StepCache) {
        let hd = self.hidden_dim;
        let gd = self.gate_dim();

        // Concatenate [h_prev, x]
        let mut hx = Vec::with_capacity(gd);
        hx.extend_from_slice(h_prev);
        hx.extend_from_slice(x);

        // Gates
        let ig_raw = mat_vec_mul(&self.w_i, &hx, hd, gd);
        let fg_raw = mat_vec_mul(&self.w_f, &hx, hd, gd);
        let og_raw = mat_vec_mul(&self.w_o, &hx, hd, gd);
        let cc_raw = mat_vec_mul(&self.w_c, &hx, hd, gd);

        let mut ig = vec![0.0; hd];
        let mut fg = vec![0.0; hd];
        let mut og = vec![0.0; hd];
        let mut cc = vec![0.0; hd];
        let mut c = vec![0.0; hd];
        let mut h = vec![0.0; hd];
        let mut tanh_c = vec![0.0; hd];

        for i in 0..hd {
            ig[i] = sigmoid(ig_raw[i] + self.b_i[i]);
            fg[i] = sigmoid(fg_raw[i] + self.b_f[i]);
            og[i] = sigmoid(og_raw[i] + self.b_o[i]);
            cc[i] = (cc_raw[i] + self.b_c[i]).tanh();

            c[i] = fg[i] * c_prev[i] + ig[i] * cc[i];
            tanh_c[i] = c[i].tanh();
            h[i] = og[i] * tanh_c[i];
        }

        let cache = StepCache {
            hx,
            ig,
            fg,
            og,
            cc,
            c_prev: c_prev.to_vec(),
            _c: c.clone(),
            h: h.clone(),
            tanh_c,
        };
        (h, c, cache)
    }

    // Applies the stored target transform to a training-space output score.
    fn decode_prediction(&self, score: f64) -> f64 {
        match self.target_mode {
            TargetMode::Regression => self.target_scaler.decode(score),
            TargetMode::Direction => score,
        }
    }

    /// Full forward pass in training target space.
    fn forward_training_score(&self, sequence: &[Vec<f64>]) -> f64 {
        let hd = self.hidden_dim;
        let mut h = vec![0.0; hd];
        let mut c = vec![0.0; hd];

        for step in sequence {
            let (h_new, c_new, _) = self.step_forward(step, &h, &c);
            h = h_new;
            c = c_new;
        }

        // Linear output
        let mut out = self.b_out;
        for i in 0..hd {
            out += self.w_out[i] * h[i];
        }
        self.target_mode.activate(out)
    }

    /// Full forward pass: sequence → raw return or direction probability.
    pub fn forward(&self, sequence: &[Vec<f64>]) -> f64 {
        self.decode_prediction(self.forward_training_score(sequence))
    }

    /// Forward pass returning caches plus optional output-dropout state.
    fn forward_with_cache_training(
        &self,
        sequence: &[Vec<f64>],
        dropout_rate: f64,
        dropout_seed: u64,
    ) -> (f64, Vec<StepCache>, Option<OutputDropoutCache>) {
        let hd = self.hidden_dim;
        let mut h = vec![0.0; hd];
        let mut c = vec![0.0; hd];
        let mut caches = Vec::with_capacity(sequence.len());

        for step in sequence {
            let (h_new, c_new, cache) = self.step_forward(step, &h, &c);
            h = h_new;
            c = c_new;
            caches.push(cache);
        }

        let dropout = if dropout_rate > 0.0 {
            let keep = (1.0 - dropout_rate).clamp(0.001, 1.0);
            let mut rng = Rng::new(dropout_seed);
            let mut dropped_h = vec![0.0; hd];
            let mut mask_scale = vec![0.0; hd];
            for i in 0..hd {
                let scale = if rng.next_f64() < keep {
                    1.0 / keep
                } else {
                    0.0
                };
                dropped_h[i] = h[i] * scale;
                mask_scale[i] = scale;
            }
            Some(OutputDropoutCache {
                dropped_h,
                mask_scale,
            })
        } else {
            None
        };
        let output_h = dropout
            .as_ref()
            .map(|cache| cache.dropped_h.as_slice())
            .unwrap_or(h.as_slice());
        let mut out = self.b_out;
        for i in 0..hd {
            out += self.w_out[i] * output_h[i];
        }
        (self.target_mode.activate(out), caches, dropout)
    }

    /// Backward pass through entire sequence (BPTT).
    /// Returns gradients for all parameters.
    fn backward(
        &self,
        caches: &[StepCache],
        d_output: f64,
        dropout: Option<&OutputDropoutCache>,
    ) -> LstmGrads {
        let hd = self.hidden_dim;
        let gd = self.gate_dim();
        let n_steps = caches.len();

        let mut grads = LstmGrads::zeros(hd, gd);

        // Gradient of output layer
        let last_h = dropout
            .map(|cache| cache.dropped_h.as_slice())
            .unwrap_or(caches[n_steps - 1].h.as_slice());
        for i in 0..hd {
            grads.dw_out[i] += d_output * last_h[i];
        }
        grads.db_out += d_output;

        // dh from output layer
        let mut dh_next = vec![0.0; hd];
        for i in 0..hd {
            let dropout_scale = dropout.map(|cache| cache.mask_scale[i]).unwrap_or(1.0);
            dh_next[i] = d_output * self.w_out[i] * dropout_scale;
        }
        let mut dc_next = vec![0.0; hd];

        // BPTT through time steps (reverse)
        for t in (0..n_steps).rev() {
            let cache = &caches[t];

            // dh = dh_next (from future step or output layer)
            let dh = &dh_next;

            // d_tanh_c = dh * og
            // d_og = dh * tanh(c)
            // d_c from this step = d_tanh_c * (1 - tanh(c)^2) + dc_next
            let mut d_og_pre = vec![0.0; hd];
            let mut d_c = vec![0.0; hd];

            for i in 0..hd {
                d_og_pre[i] = dh[i] * cache.tanh_c[i];
                let dtc = dh[i] * cache.og[i];
                d_c[i] = dtc * (1.0 - cache.tanh_c[i] * cache.tanh_c[i]) + dc_next[i];
            }

            // d_ig = d_c * cc
            // d_cc = d_c * ig
            // d_fg = d_c * c_prev
            // dc_next (for t-1) = d_c * fg
            let mut d_ig_pre = vec![0.0; hd];
            let mut d_fg_pre = vec![0.0; hd];
            let mut d_cc_pre = vec![0.0; hd];

            for i in 0..hd {
                d_ig_pre[i] = d_c[i] * cache.cc[i];
                d_cc_pre[i] = d_c[i] * cache.ig[i];
                d_fg_pre[i] = d_c[i] * cache.c_prev[i];
                dc_next[i] = d_c[i] * cache.fg[i];
            }

            // Through activations
            // sigmoid: d_pre = d_post * sig * (1 - sig)
            // tanh:    d_pre = d_post * (1 - tanh^2)
            let mut d_ig = vec![0.0; hd];
            let mut d_fg = vec![0.0; hd];
            let mut d_og = vec![0.0; hd];
            let mut d_cc = vec![0.0; hd];

            for i in 0..hd {
                d_ig[i] = d_ig_pre[i] * cache.ig[i] * (1.0 - cache.ig[i]);
                d_fg[i] = d_fg_pre[i] * cache.fg[i] * (1.0 - cache.fg[i]);
                d_og[i] = d_og_pre[i] * cache.og[i] * (1.0 - cache.og[i]);
                d_cc[i] = d_cc_pre[i] * (1.0 - cache.cc[i] * cache.cc[i]);
            }

            // Weight gradients: dW += d_gate * hx^T  (outer product)
            // Bias gradients:   db += d_gate
            for i in 0..hd {
                grads.db_i[i] += d_ig[i];
                grads.db_f[i] += d_fg[i];
                grads.db_o[i] += d_og[i];
                grads.db_c[i] += d_cc[i];
                for j in 0..gd {
                    grads.dw_i[i * gd + j] += d_ig[i] * cache.hx[j];
                    grads.dw_f[i * gd + j] += d_fg[i] * cache.hx[j];
                    grads.dw_o[i * gd + j] += d_og[i] * cache.hx[j];
                    grads.dw_c[i * gd + j] += d_cc[i] * cache.hx[j];
                }
            }

            // dh_next for t-1: sum contributions from all gates through W[:, :hd]
            dh_next = vec![0.0; hd];
            for i in 0..hd {
                for j in 0..hd {
                    dh_next[j] += d_ig[i] * self.w_i[i * gd + j];
                    dh_next[j] += d_fg[i] * self.w_f[i * gd + j];
                    dh_next[j] += d_og[i] * self.w_o[i * gd + j];
                    dh_next[j] += d_cc[i] * self.w_c[i * gd + j];
                }
            }
        }

        grads
    }

    /// Train on prepared sequences
    fn train_on_data(
        &mut self,
        sequences: &[Vec<Vec<f64>>],
        targets: &[f64],
        val_sequences: &[Vec<Vec<f64>>],
        val_targets: &[f64],
        epochs: usize,
        lr: f64,
        batch_size: usize,
        early: &config::LstmTrainingConfig,
        show_progress: bool,
    ) -> LstmTrainingOutcome {
        let n = sequences.len();
        eprintln!("  LSTM trainer threads: {}", rayon::current_num_threads());
        let gd = self.gate_dim();
        let hd = self.hidden_dim;
        let wsize = hd * gd;
        let loss_kind = LstmLossKind::parse(&early.loss_function).unwrap_or(LstmLossKind::Mse);
        let dropout_rate = early.dropout_rate.clamp(0.0, 0.9);
        let weight_decay = early.weight_decay.clamp(0.0, 1.0);

        // Adam states
        let mut adam_wi = AdamState::new(wsize);
        let mut adam_wf = AdamState::new(wsize);
        let mut adam_wo = AdamState::new(wsize);
        let mut adam_wc = AdamState::new(wsize);
        let mut adam_bi = AdamState::new(hd);
        let mut adam_bf = AdamState::new(hd);
        let mut adam_bo = AdamState::new(hd);
        let mut adam_bc = AdamState::new(hd);
        let mut adam_wout = AdamState::new(hd);
        let mut adam_bout = AdamState::new(1);

        // Shuffle indices
        let mut rng = Rng::new(42);
        let mut indices: Vec<usize> = (0..n).collect();

        let mut epoch_losses = Vec::new();
        let mut validation_losses = Vec::new();
        let mut best_model = self.clone();
        let mut best_epoch = 0usize;
        let mut best_validation_loss = f64::INFINITY;
        let mut no_improve_epochs = 0usize;
        let batches_per_epoch = n.div_ceil(batch_size);
        let progress = crate::progress::bar_if(
            show_progress,
            (epochs * batches_per_epoch) as u64,
            "Training LSTM",
        );

        for epoch in 0..epochs {
            // Fisher-Yates shuffle
            for i in (1..n).rev() {
                let j = (rng.next_u64() as usize) % (i + 1);
                indices.swap(i, j);
            }

            let mut total_loss = 0.0;
            let mut _n_batches = 0;

            for batch_start in (0..n).step_by(batch_size) {
                let batch_end = (batch_start + batch_size).min(n);
                let bs = batch_end - batch_start;

                let model_snapshot = self.clone();
                let (mut grads, batch_loss) = indices[batch_start..batch_end]
                    .par_iter()
                    .fold(
                        || (LstmGrads::zeros(hd, gd), 0.0f64),
                        |mut acc, &idx| {
                            let dropout_seed =
                                ((epoch as u64 + 1) << 48) ^ (idx as u64).wrapping_mul(0x9E37);
                            let (pred, caches, dropout) = model_snapshot
                                .forward_with_cache_training(
                                    &sequences[idx],
                                    dropout_rate,
                                    dropout_seed,
                                );
                            let (sample_loss, sample_grad) = loss_kind.sample_loss_and_grad(
                                pred,
                                targets[idx],
                                early.huber_delta,
                            );
                            let d_score = sample_grad / bs as f64;
                            let d_out = model_snapshot.target_mode.output_grad(pred, d_score);
                            let sample_grads =
                                model_snapshot.backward(&caches, d_out, dropout.as_ref());
                            acc.0.accumulate(&sample_grads);
                            acc.1 += sample_loss;
                            acc
                        },
                    )
                    .reduce(
                        || (LstmGrads::zeros(hd, gd), 0.0f64),
                        |mut left, right| {
                            left.0.accumulate(&right.0);
                            left.1 += right.1;
                            left
                        },
                    );
                total_loss += batch_loss;

                // Gradient clipping (max norm = 5.0)
                let gnorm = grads.norm();
                if gnorm > 5.0 {
                    grads.scale(5.0 / gnorm);
                }

                // Adam updates
                adam_wi.step(&mut self.w_i, &grads.dw_i, lr);
                adam_wf.step(&mut self.w_f, &grads.dw_f, lr);
                adam_wo.step(&mut self.w_o, &grads.dw_o, lr);
                adam_wc.step(&mut self.w_c, &grads.dw_c, lr);
                adam_bi.step(&mut self.b_i, &grads.db_i, lr);
                adam_bf.step(&mut self.b_f, &grads.db_f, lr);
                adam_bo.step(&mut self.b_o, &grads.db_o, lr);
                adam_bc.step(&mut self.b_c, &grads.db_c, lr);
                adam_wout.step(&mut self.w_out, &grads.dw_out, lr);
                adam_bout.step_scalar(&mut self.b_out, grads.db_out, lr);
                if weight_decay > 0.0 {
                    let decay = (1.0 - lr * weight_decay).max(0.0);
                    for weights in [
                        &mut self.w_i,
                        &mut self.w_f,
                        &mut self.w_o,
                        &mut self.w_c,
                        &mut self.w_out,
                    ] {
                        for value in weights.iter_mut() {
                            *value *= decay;
                        }
                    }
                }

                _n_batches += 1;
                progress.inc(1);
            }

            let avg_loss = total_loss / n as f64;
            epoch_losses.push(avg_loss);
            let validation_loss = if early.early_stopping_enabled && !val_sequences.is_empty() {
                validation_loss_sample(
                    self,
                    val_sequences,
                    val_targets,
                    early.early_stopping_sample_size,
                    loss_kind,
                    early.huber_delta,
                )
            } else {
                avg_loss
            };
            validation_losses.push(validation_loss);
            progress.set_message(format!(
                "epoch {}/{} loss={avg_loss:.6} val={validation_loss:.6}",
                epoch + 1,
                epochs
            ));

            if epoch % 2 == 0 || epoch == epochs - 1 {
                eprintln!(
                    "  Epoch {}/{}: loss={:.6}, val={:.6}",
                    epoch + 1,
                    epochs,
                    avg_loss,
                    validation_loss
                );
            }

            if validation_loss + early.early_stopping_min_delta < best_validation_loss {
                best_validation_loss = validation_loss;
                best_epoch = epoch + 1;
                best_model = self.clone();
                no_improve_epochs = 0;
            } else {
                no_improve_epochs += 1;
                if early.early_stopping_enabled
                    && no_improve_epochs >= early.early_stopping_patience
                {
                    eprintln!(
                        "  Early stopping: best epoch {} val={:.6}",
                        best_epoch, best_validation_loss
                    );
                    break;
                }
            }
        }

        progress.finish_and_clear();
        let stopped_early = early.early_stopping_enabled && epoch_losses.len() < epochs;
        if early.early_stopping_enabled && best_epoch > 0 {
            *self = best_model;
        }
        LstmTrainingOutcome {
            losses: epoch_losses,
            validation_losses,
            best_epoch,
            best_validation_loss,
            stopped_early,
        }
    }

    /// Save model to binary file
    pub fn save(&self, path: &str) -> anyhow::Result<()> {
        let mut f = crate::paths::create_private_file(std::path::Path::new(path))?;
        // Magic + version
        f.write_all(b"LSTM0003")?;
        // Dimensions
        f.write_all(&(INPUT_DIM as u32).to_le_bytes())?;
        f.write_all(&(self.hidden_dim as u32).to_le_bytes())?;
        f.write_all(&(SEQ_LEN as u32).to_le_bytes())?;
        f.write_all(&self.target_mode.as_u32().to_le_bytes())?;
        f.write_all(&self.direction_threshold.to_le_bytes())?;
        f.write_all(&self.target_scaler.mean.to_le_bytes())?;
        f.write_all(&self.target_scaler.std.to_le_bytes())?;

        // Writes vec to disk or storage.
        fn write_vec(f: &mut std::fs::File, v: &[f64]) -> std::io::Result<()> {
            f.write_all(&(v.len() as u32).to_le_bytes())?;
            for &val in v {
                f.write_all(&val.to_le_bytes())?;
            }
            Ok(())
        }

        write_vec(&mut f, &self.w_i)?;
        write_vec(&mut f, &self.b_i)?;
        write_vec(&mut f, &self.w_f)?;
        write_vec(&mut f, &self.b_f)?;
        write_vec(&mut f, &self.w_o)?;
        write_vec(&mut f, &self.b_o)?;
        write_vec(&mut f, &self.w_c)?;
        write_vec(&mut f, &self.b_c)?;
        write_vec(&mut f, &self.w_out)?;
        f.write_all(&self.b_out.to_le_bytes())?;

        Ok(())
    }

    /// Load model from binary file
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let mut f = std::fs::File::open(path)?;
        let mut magic = [0u8; 8];
        f.read_exact(&mut magic)?;
        if &magic != b"LSTM0001" && &magic != b"LSTM0002" && &magic != b"LSTM0003" {
            anyhow::bail!("Invalid LSTM model file (bad magic)");
        }
        let mut buf4 = [0u8; 4];
        f.read_exact(&mut buf4)?;
        let input_dim = u32::from_le_bytes(buf4) as usize;
        f.read_exact(&mut buf4)?;
        let hidden_dim = u32::from_le_bytes(buf4) as usize;
        f.read_exact(&mut buf4)?;
        let seq_len = u32::from_le_bytes(buf4) as usize;
        if input_dim != INPUT_DIM {
            anyhow::bail!("Invalid LSTM model file (input dim {input_dim}, expected {INPUT_DIM})");
        }
        if seq_len != SEQ_LEN {
            anyhow::bail!("Invalid LSTM model file (seq len {seq_len}, expected {SEQ_LEN})");
        }
        let target_mode = if &magic == b"LSTM0002" || &magic == b"LSTM0003" {
            f.read_exact(&mut buf4)?;
            TargetMode::from_u32(u32::from_le_bytes(buf4))?
        } else {
            TargetMode::Regression
        };
        let direction_threshold = if &magic == b"LSTM0002" || &magic == b"LSTM0003" {
            let mut buf8 = [0u8; 8];
            f.read_exact(&mut buf8)?;
            f64::from_le_bytes(buf8)
        } else {
            0.0
        };
        let target_scaler = if &magic == b"LSTM0003" {
            let mut buf8 = [0u8; 8];
            f.read_exact(&mut buf8)?;
            let mean = f64::from_le_bytes(buf8);
            f.read_exact(&mut buf8)?;
            let std = f64::from_le_bytes(buf8).max(1e-8);
            TargetScaler {
                mean,
                std,
                enabled: target_mode == TargetMode::Regression,
            }
        } else {
            TargetScaler::identity()
        };

        // Reads vec from disk or local state.
        fn read_vec(f: &mut std::fs::File) -> std::io::Result<Vec<f64>> {
            let mut buf4 = [0u8; 4];
            f.read_exact(&mut buf4)?;
            let len = u32::from_le_bytes(buf4) as usize;
            let mut v = Vec::with_capacity(len);
            let mut buf8 = [0u8; 8];
            for _ in 0..len {
                f.read_exact(&mut buf8)?;
                v.push(f64::from_le_bytes(buf8));
            }
            Ok(v)
        }

        let w_i = read_vec(&mut f)?;
        let b_i = read_vec(&mut f)?;
        let w_f = read_vec(&mut f)?;
        let b_f = read_vec(&mut f)?;
        let w_o = read_vec(&mut f)?;
        let b_o = read_vec(&mut f)?;
        let w_c = read_vec(&mut f)?;
        let b_c = read_vec(&mut f)?;
        let w_out = read_vec(&mut f)?;
        let mut buf8 = [0u8; 8];
        f.read_exact(&mut buf8)?;
        let b_out = f64::from_le_bytes(buf8);

        Ok(LstmModel {
            hidden_dim,
            target_mode,
            direction_threshold,
            target_scaler,
            w_i,
            b_i,
            w_f,
            b_f,
            w_o,
            b_o,
            w_c,
            b_c,
            w_out,
            b_out,
        })
    }
}

// Gradient accumulator
struct LstmGrads {
    dw_i: Vec<f64>,
    db_i: Vec<f64>,
    dw_f: Vec<f64>,
    db_f: Vec<f64>,
    dw_o: Vec<f64>,
    db_o: Vec<f64>,
    dw_c: Vec<f64>,
    db_c: Vec<f64>,
    dw_out: Vec<f64>,
    db_out: f64,
}

impl LstmGrads {
    // Handles zeros logic.
    fn zeros(hidden_dim: usize, gate_dim: usize) -> Self {
        let wsize = hidden_dim * gate_dim;
        LstmGrads {
            dw_i: vec![0.0; wsize],
            db_i: vec![0.0; hidden_dim],
            dw_f: vec![0.0; wsize],
            db_f: vec![0.0; hidden_dim],
            dw_o: vec![0.0; wsize],
            db_o: vec![0.0; hidden_dim],
            dw_c: vec![0.0; wsize],
            db_c: vec![0.0; hidden_dim],
            dw_out: vec![0.0; hidden_dim],
            db_out: 0.0,
        }
    }

    // Handles accumulate logic.
    fn accumulate(&mut self, other: &LstmGrads) {
        for i in 0..self.dw_i.len() {
            self.dw_i[i] += other.dw_i[i];
            self.dw_f[i] += other.dw_f[i];
            self.dw_o[i] += other.dw_o[i];
            self.dw_c[i] += other.dw_c[i];
        }
        for i in 0..self.dw_out.len() {
            self.db_i[i] += other.db_i[i];
            self.db_f[i] += other.db_f[i];
            self.db_o[i] += other.db_o[i];
            self.db_c[i] += other.db_c[i];
            self.dw_out[i] += other.dw_out[i];
        }
        self.db_out += other.db_out;
    }

    // Handles norm logic.
    fn norm(&self) -> f64 {
        let mut s = 0.0;
        for v in [
            &self.dw_i,
            &self.dw_f,
            &self.dw_o,
            &self.dw_c,
            &self.db_i,
            &self.db_f,
            &self.db_o,
            &self.db_c,
            &self.dw_out,
        ] {
            for &x in v.iter() {
                s += x * x;
            }
        }
        s += self.db_out * self.db_out;
        s.sqrt()
    }

    // Handles scale logic.
    fn scale(&mut self, factor: f64) {
        for v in [
            &mut self.dw_i,
            &mut self.dw_f,
            &mut self.dw_o,
            &mut self.dw_c,
            &mut self.db_i,
            &mut self.db_f,
            &mut self.db_o,
            &mut self.db_c,
            &mut self.dw_out,
        ] {
            for x in v.iter_mut() {
                *x *= factor;
            }
        }
        self.db_out *= factor;
    }
}

// ══════════════════════════════════════════════════════════════════
// DATA PREPARATION — Build sequences from the Alpaca market research DB
// ══════════════════════════════════════════════════════════════════

struct SequenceDataset {
    sequences: Vec<Vec<Vec<f64>>>, // [n_samples][SEQ_LEN][INPUT_DIM]
    targets: Vec<f64>,
    symbols: Vec<String>,
    dates: Vec<String>, // date of the last step in each sequence
}

impl SequenceDataset {
    // Sorts samples into chronological validation order before any split.
    fn sort_by_date_symbol(mut self) -> Self {
        let mut samples = Vec::with_capacity(self.sequences.len());
        for (((sequence, target), symbol), date) in self
            .sequences
            .drain(..)
            .zip(self.targets.drain(..))
            .zip(self.symbols.drain(..))
            .zip(self.dates.drain(..))
        {
            samples.push((date, symbol, target, sequence));
        }
        samples.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

        let mut sequences = Vec::with_capacity(samples.len());
        let mut targets = Vec::with_capacity(samples.len());
        let mut symbols = Vec::with_capacity(samples.len());
        let mut dates = Vec::with_capacity(samples.len());
        for (date, symbol, target, sequence) in samples {
            dates.push(date);
            symbols.push(symbol);
            targets.push(target);
            sequences.push(sequence);
        }

        Self {
            sequences,
            targets,
            symbols,
            dates,
        }
    }
}

#[derive(Clone, Serialize)]
struct DirectionMetrics {
    accuracy: f64,
    precision: f64,
    recall: f64,
    positives: usize,
    predicted_positives: usize,
}

// Converts forward returns into the configured LSTM training target values.
fn training_targets(
    returns: &[f64],
    target_mode: TargetMode,
    threshold: f64,
    scaler: TargetScaler,
) -> Vec<f64> {
    returns
        .iter()
        .map(|value| match target_mode {
            TargetMode::Regression => scaler.encode(*value),
            TargetMode::Direction => target_mode.target_value(*value, threshold),
        })
        .collect()
}

// Computes direction-quality metrics regardless of the train target mode.
fn direction_metrics(
    preds: &[f64],
    returns: &[f64],
    target_mode: TargetMode,
    threshold: f64,
) -> DirectionMetrics {
    let mut correct = 0usize;
    let mut positives = 0usize;
    let mut predicted_positives = 0usize;
    let mut true_positives = 0usize;
    for (pred, actual) in preds.iter().zip(returns) {
        let expected_up = *actual > threshold;
        let predicted_up = match target_mode {
            TargetMode::Regression => *pred > threshold,
            TargetMode::Direction => *pred >= 0.5,
        };
        positives += usize::from(expected_up);
        predicted_positives += usize::from(predicted_up);
        true_positives += usize::from(expected_up && predicted_up);
        correct += usize::from(expected_up == predicted_up);
    }
    DirectionMetrics {
        accuracy: if preds.is_empty() {
            0.0
        } else {
            correct as f64 / preds.len() as f64
        },
        precision: if predicted_positives == 0 {
            0.0
        } else {
            true_positives as f64 / predicted_positives as f64
        },
        recall: if positives == 0 {
            0.0
        } else {
            true_positives as f64 / positives as f64
        },
        positives,
        predicted_positives,
    }
}

// Computes evaluation MSE in the same space users care about.
fn validation_mse(
    preds: &[f64],
    returns: &[f64],
    class_targets: &[f64],
    target_mode: TargetMode,
) -> f64 {
    let n = preds.len().min(returns.len()).min(class_targets.len());
    if n == 0 {
        return 0.0;
    }
    match target_mode {
        TargetMode::Regression => {
            preds
                .iter()
                .zip(returns)
                .take(n)
                .map(|(pred, actual)| {
                    let err = pred - actual;
                    err * err
                })
                .sum::<f64>()
                / n as f64
        }
        TargetMode::Direction => {
            preds
                .iter()
                .zip(class_targets)
                .take(n)
                .map(|(pred, target)| {
                    let err = pred - target;
                    err * err
                })
                .sum::<f64>()
                / n as f64
        }
    }
}

// Estimates validation loss over a bounded sample for early stopping.
fn validation_loss_sample(
    model: &LstmModel,
    sequences: &[Vec<Vec<f64>>],
    targets: &[f64],
    max_samples: usize,
    loss_kind: LstmLossKind,
    huber_delta: f64,
) -> f64 {
    let n = sequences.len().min(targets.len()).min(max_samples);
    if n == 0 {
        return f64::INFINITY;
    }
    sequences
        .iter()
        .zip(targets)
        .take(n)
        .map(|(seq, target)| {
            loss_kind
                .sample_loss_and_grad(model.forward_training_score(seq), *target, huber_delta)
                .0
        })
        .sum::<f64>()
        / n as f64
}

/// Load sequences grouped by symbol from DB
fn load_sequences(
    conn: &Connection,
    max_symbols: usize,
    without_sp500: bool,
    data_window: Option<LstmDataWindow>,
    show_progress: bool,
) -> anyhow::Result<SequenceDataset> {
    let feature_cols = FEATURE_COLS.join(", ");
    let eligible = crate::ml::ml_eligible_asset_predicate("f.symbol", "a");

    // Qualify from ml_features so short test datasets can still exercise LSTM.
    // Full production scans still naturally provide many more rows per symbol.
    let mut sym_stmt = conn.prepare(&format!(
        "SELECT f.symbol
         FROM ml_features f
         JOIN ml_labels l ON f.symbol = l.symbol AND f.date = l.date
         LEFT JOIN assets a ON a.symbol = f.symbol
         WHERE f.return_1d IS NOT NULL
           AND l.fwd_5d IS NOT NULL
           AND {eligible}
         GROUP BY f.symbol
         HAVING COUNT(*) >= ?1
         ORDER BY f.symbol
         LIMIT ?2"
    ))?;
    let min_rows = (SEQ_LEN + 5) as i64;
    let symbols: Vec<String> = sym_stmt
        .query_map(params![min_rows, max_symbols as i64], |r| r.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    eprintln!("  Loading sequences for {} symbols...", symbols.len());
    let progress = crate::progress::bar_if(
        show_progress,
        symbols.len() as u64,
        "Loading LSTM sequences",
    );

    let mut all_seqs = Vec::new();
    let mut all_targets = Vec::new();
    let mut all_symbols = Vec::new();
    let mut all_dates = Vec::new();

    let query = format!(
        "SELECT f.date, {fcols}, l.fwd_5d as target
         FROM ml_features f
         JOIN ml_labels l ON f.symbol = l.symbol AND f.date = l.date
         WHERE f.symbol = ?1 AND f.return_1d IS NOT NULL AND l.fwd_5d IS NOT NULL
         ORDER BY f.date",
        fcols = feature_cols
    );

    // Subsample: only take every Nth sequence per symbol to control memory
    // With 3000 symbols × ~1000 dates each, we'd get ~3M sequences.
    // Target: ~200K total sequences for training
    let target_total = config::lstm_max_sequences();
    let approx_per_symbol = (target_total / symbols.len().max(1)).max(20);
    let window = data_window.as_ref();

    for (si, symbol) in symbols.iter().enumerate() {
        if config::is_blocked_symbol(symbol) {
            progress.set_position((si + 1) as u64);
            continue;
        }

        let mut stmt = conn.prepare_cached(&query)?;
        let rows: Vec<(String, Vec<f64>, f64)> = stmt
            .query_map(params![symbol], |r| {
                let date: String = r.get(0)?;
                let mut feats = Vec::with_capacity(INPUT_DIM);
                for i in 0..INPUT_DIM {
                    let v: Option<f64> = r.get(i + 1)?;
                    feats.push(v.unwrap_or(0.0));
                }
                let target: f64 = r.get(INPUT_DIM + 1)?;
                Ok((date, feats, target))
            })?
            .filter_map(|r| r.ok())
            .collect();

        if rows.len() < SEQ_LEN + 5 {
            progress.set_position((si + 1) as u64);
            continue;
        }

        // Determine step size for subsampling
        let possible_seqs = rows.len() - SEQ_LEN;
        let step = if possible_seqs > approx_per_symbol {
            possible_seqs / approx_per_symbol
        } else {
            1
        };

        // Build sliding windows with subsampling
        let mut _count = 0;
        for end in (SEQ_LEN..rows.len()).step_by(step) {
            let start = end - SEQ_LEN;
            let target_date = rows[end - 1].0.as_str();
            if let Some(window) = window {
                if !window.contains(target_date) {
                    continue;
                }
            }
            let target = rows[end - 1].2;
            if target == 0.0 {
                continue;
            }

            // Normalize features within window (z-score)
            let raw: Vec<&Vec<f64>> = (start..end).map(|i| &rows[i].1).collect();
            let mut means = vec![0.0; INPUT_DIM];
            let mut stds = vec![0.0; INPUT_DIM];

            for feat in &raw {
                for j in 0..INPUT_DIM {
                    means[j] += feat[j];
                }
            }
            for j in 0..INPUT_DIM {
                means[j] /= SEQ_LEN as f64;
            }

            for feat in &raw {
                for j in 0..INPUT_DIM {
                    let d = feat[j] - means[j];
                    stds[j] += d * d;
                }
            }
            for j in 0..INPUT_DIM {
                stds[j] = (stds[j] / SEQ_LEN as f64).sqrt().max(1e-8);
            }

            let mut seq: Vec<Vec<f64>> = raw
                .iter()
                .map(|feat| {
                    (0..INPUT_DIM)
                        .map(|j| (feat[j] - means[j]) / stds[j])
                        .collect()
                })
                .collect();
            if without_sp500 {
                zero_sp500_features(&mut seq);
            }

            all_seqs.push(seq);
            all_targets.push(target);
            all_symbols.push(symbol.clone());
            all_dates.push(rows[end - 1].0.clone());
            _count += 1;
        }

        if (si + 1) % 500 == 0 {
            progress.set_message(format!("{} sequences", all_seqs.len()));
        }
        progress.set_position((si + 1) as u64);
    }
    progress.finish_and_clear();

    eprintln!(
        "  Total: {} sequences from {} symbols",
        all_seqs.len(),
        symbols.len()
    );

    Ok(SequenceDataset {
        sequences: all_seqs,
        targets: all_targets,
        symbols: all_symbols,
        dates: all_dates,
    }
    .sort_by_date_symbol())
}

// ══════════════════════════════════════════════════════════════════
// CMD: ml lstm-train
// ══════════════════════════════════════════════════════════════════

pub fn cmd_ml_lstm_train(
    json: bool,
    single_thread: bool,
    threads: Option<usize>,
    without_sp500: bool,
    requested_backend: LstmBackend,
    data_window: Option<LstmDataWindow>,
    overrides: LstmTrainOverrides,
) -> anyhow::Result<()> {
    let backend = resolve_lstm_backend(requested_backend)?;
    let (train_cfg, target_mode) = resolve_training_config(backend, overrides.clone())?;
    if backend != LstmBackend::Cpu {
        let accelerated_cfg = train_cfg.clone();
        match catch_accelerated_lstm_panic(|| {
            cmd_ml_lstm_train_accelerated(
                json,
                without_sp500,
                data_window.clone(),
                backend,
                accelerated_cfg,
                target_mode,
            )
        }) {
            Ok(()) => return Ok(()),
            Err(err) if requested_backend == LstmBackend::Auto => {
                eprintln!(
                    "⚠️  LSTM auto backend '{}' failed: {}; falling back to CPU/Rayon.",
                    backend, err
                );
                let (cpu_cfg, cpu_target_mode) =
                    resolve_training_config(LstmBackend::Cpu, overrides.clone())?;
                return cmd_ml_lstm_train_cpu(
                    json,
                    single_thread,
                    threads,
                    without_sp500,
                    LstmBackend::Cpu,
                    data_window,
                    cpu_cfg,
                    cpu_target_mode,
                );
            }
            Err(err) => return Err(err),
        }
    }

    cmd_ml_lstm_train_cpu(
        json,
        single_thread,
        threads,
        without_sp500,
        backend,
        data_window,
        train_cfg,
        target_mode,
    )
}

// Trains the portable Rust/Rayon LSTM implementation.
fn cmd_ml_lstm_train_cpu(
    json: bool,
    single_thread: bool,
    threads: Option<usize>,
    without_sp500: bool,
    backend: LstmBackend,
    data_window: Option<LstmDataWindow>,
    train_cfg: config::LstmTrainingConfig,
    target_mode: TargetMode,
) -> anyhow::Result<()> {
    let model_path = lstm_model_path(without_sp500);

    eprintln!("🧠 LSTM Training — Pure Rust CPU/Rayon Engine");
    eprintln!("  Backend: {}", backend);
    if without_sp500 {
        eprintln!("  Variant: without S&P 500 signal (SP500 features zeroed)");
    }
    eprintln!("{}", "═".repeat(50));

    let conn = open_lstm_db()?;

    // Load from every qualifying symbol in the local Alpaca history. The loader
    // subsamples sequences after symbol selection to keep training memory bounded.
    eprintln!("\n📊 Loading training data...");
    let dataset = load_sequences(&conn, usize::MAX, without_sp500, data_window.clone(), !json)?;

    if dataset.sequences.len() < 1000 {
        anyhow::bail!(
            "Not enough sequences ({}) for LSTM training. Need >= 1000.",
            dataset.sequences.len()
        );
    }

    // Split: 80% train, 20% validation (chronological)
    let n = dataset.sequences.len();
    let split = (n as f64 * 0.8) as usize;
    let train_seqs = &dataset.sequences[..split];
    let train_returns = &dataset.targets[..split];
    let train_dates = &dataset.dates[..split];
    let val_seqs = &dataset.sequences[split..];
    let val_returns = &dataset.targets[split..];
    let val_dates = &dataset.dates[split..];
    let target_scaler = TargetScaler::fit(target_mode, train_returns);
    let train_targets = training_targets(
        train_returns,
        target_mode,
        train_cfg.direction_threshold,
        target_scaler,
    );
    let val_targets = training_targets(
        val_returns,
        target_mode,
        train_cfg.direction_threshold,
        target_scaler,
    );

    eprintln!(
        "  Train: {} sequences ({} → {}), Val: {} sequences ({} → {})",
        train_seqs.len(),
        train_dates
            .first()
            .map(String::as_str)
            .unwrap_or("not available"),
        train_dates
            .last()
            .map(String::as_str)
            .unwrap_or("not available"),
        val_seqs.len(),
        val_dates
            .first()
            .map(String::as_str)
            .unwrap_or("not available"),
        val_dates
            .last()
            .map(String::as_str)
            .unwrap_or("not available")
    );

    // Initialize and train
    eprintln!(
        "\n🏗️  Training LSTM (hidden={}, seq_len={}, target={})...",
        train_cfg.hidden_dim,
        SEQ_LEN,
        target_mode.as_str()
    );
    let mut model = LstmModel::new_random(
        42,
        train_cfg.hidden_dim,
        target_mode,
        train_cfg.direction_threshold,
    );
    model.target_scaler = target_scaler;
    let cpu_cap = config::cpu_worker_threads();
    let requested_threads = if single_thread { Some(1) } else { threads };
    let worker_threads = requested_threads
        .map(|value| if value == 0 { 0 } else { value.min(cpu_cap) })
        .unwrap_or(cpu_cap);
    if worker_threads == 0 {
        anyhow::bail!("LSTM thread count must be greater than zero");
    }
    eprintln!(
        "  CPU cap: {} worker threads (~{}% process CPU; budget {}% = {}% of {} logical CPUs)",
        worker_threads,
        (worker_threads as u64).saturating_mul(100),
        config::runtime_resources().cpu_budget_process_percent,
        config::runtime_resources().cpu_budget_percent,
        config::runtime_resources().cpu_total_threads
    );
    let batch_size = config::lstm_batch_size();
    let outcome = rayon::ThreadPoolBuilder::new()
        .num_threads(worker_threads)
        .build()?
        .install(|| {
            model.train_on_data(
                train_seqs,
                &train_targets,
                val_seqs,
                &val_targets,
                train_cfg.epochs,
                train_cfg.learning_rate,
                batch_size,
                &train_cfg,
                !json,
            )
        });

    // Validation
    eprintln!("\n📈 Validation...");
    let mut val_preds = Vec::with_capacity(val_seqs.len());
    let progress = crate::progress::bar_if(!json, val_seqs.len() as u64, "Validating LSTM");
    for seq in val_seqs {
        val_preds.push(model.forward(seq));
        progress.inc(1);
    }
    progress.finish_and_clear();

    // Compute validation IC (Spearman rank correlation)
    let val_ic = spearman_corr(&val_preds, val_returns);
    let val_mse = validation_mse(&val_preds, val_returns, &val_targets, target_mode);
    let direction = direction_metrics(
        &val_preds,
        val_returns,
        target_mode,
        train_cfg.direction_threshold,
    );

    eprintln!("  Val MSE: {:.6}, Val IC: {:.4}", val_mse, val_ic);

    // Save model
    let model_path_str = model_path.to_string_lossy().to_string();
    model.save(&model_path_str)?;
    eprintln!("\n  Model saved: {}", model_path.display());
    let report_path = if without_sp500 {
        paths::state_dir().join("lstm_without_sp500_training_report.json")
    } else {
        paths::state_dir().join("lstm_training_report.json")
    };
    paths::write_private_file(
        &report_path,
        serde_json::to_string_pretty(&serde_json::json!({
            "status": "done",
            "variant": if without_sp500 { "without_sp500" } else { "with_sp500" },
            "backend": "cpu",
            "model_path": model_path.display().to_string(),
            "data_window": &data_window,
            "profile": train_cfg.profile,
            "train_samples": train_seqs.len(),
            "val_samples": val_seqs.len(),
            "split": {
                "method": "chronological_sorted_by_date_symbol",
                "train_start_date": train_dates.first().map(String::as_str).unwrap_or("not available"),
                "train_end_date": train_dates.last().map(String::as_str).unwrap_or("not available"),
                "validation_start_date": val_dates.first().map(String::as_str).unwrap_or("not available"),
                "validation_end_date": val_dates.last().map(String::as_str).unwrap_or("not available")
            },
            "final_loss": outcome.losses.last().unwrap_or(&0.0),
            "validation_losses": &outcome.validation_losses,
            "best_epoch": outcome.best_epoch,
            "best_validation_loss": outcome.best_validation_loss,
            "stopped_early": outcome.stopped_early,
            "target_mode": target_mode.as_str(),
            "direction_threshold": train_cfg.direction_threshold,
            "target_scaler": target_scaler,
            "loss_function": train_cfg.loss_function,
            "huber_delta": train_cfg.huber_delta,
            "dropout_rate": train_cfg.dropout_rate,
            "weight_decay": train_cfg.weight_decay,
            "direction_metrics": &direction,
            "val_mse": val_mse,
            "val_ic": val_ic,
            "hidden_dim": train_cfg.hidden_dim,
            "seq_len": SEQ_LEN,
            "epochs": outcome.losses.len(),
            "configured_epochs": train_cfg.epochs,
            "learning_rate": train_cfg.learning_rate,
            "cpu_threads": worker_threads,
        }))?,
    )?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "status": "done",
                "variant": if without_sp500 { "without_sp500" } else { "with_sp500" },
                "backend": "cpu",
                "model_path": model_path.display().to_string(),
                "data_window": &data_window,
                "profile": train_cfg.profile,
                "train_samples": train_seqs.len(),
                "val_samples": val_seqs.len(),
                "split": {
                    "method": "chronological_sorted_by_date_symbol",
                    "train_start_date": train_dates.first().map(String::as_str).unwrap_or("not available"),
                    "train_end_date": train_dates.last().map(String::as_str).unwrap_or("not available"),
                    "validation_start_date": val_dates.first().map(String::as_str).unwrap_or("not available"),
                    "validation_end_date": val_dates.last().map(String::as_str).unwrap_or("not available")
                },
                "final_loss": outcome.losses.last().unwrap_or(&0.0),
                "validation_losses": &outcome.validation_losses,
                "best_epoch": outcome.best_epoch,
                "best_validation_loss": outcome.best_validation_loss,
                "stopped_early": outcome.stopped_early,
                "target_mode": target_mode.as_str(),
                "direction_threshold": train_cfg.direction_threshold,
                "target_scaler": target_scaler,
                "loss_function": train_cfg.loss_function,
                "huber_delta": train_cfg.huber_delta,
                "dropout_rate": train_cfg.dropout_rate,
                "weight_decay": train_cfg.weight_decay,
                "direction_metrics": &direction,
                "val_mse": val_mse,
                "val_ic": val_ic,
                "hidden_dim": train_cfg.hidden_dim,
                "seq_len": SEQ_LEN,
                "epochs": outcome.losses.len(),
                "configured_epochs": train_cfg.epochs,
                "learning_rate": train_cfg.learning_rate,
                "cpu_threads": worker_threads,
            })
        );
    } else {
        println!("🧠 LSTM Training Complete");
        println!("{}", "─".repeat(40));
        println!(
            "  Architecture:  LSTM({} → {} → 1)",
            INPUT_DIM, train_cfg.hidden_dim
        );
        println!("  Profile:       {}", train_cfg.profile);
        println!("  Target mode:   {}", target_mode.as_str());
        if target_scaler.enabled {
            println!(
                "  Target scale:  z-score mean={:.6}, std={:.6}",
                target_scaler.mean, target_scaler.std
            );
        }
        println!("  Loss:          {}", train_cfg.loss_function);
        println!("  Dropout:       {:.2}", train_cfg.dropout_rate);
        println!("  Weight decay:  {:.4}", train_cfg.weight_decay);
        if let Some(window) = &data_window {
            println!(
                "  Data window:   {} → {}",
                window.start_date, window.end_date
            );
        }
        println!("  Seq length:    {}", SEQ_LEN);
        println!("  Train samples: {}", train_seqs.len());
        println!("  Val samples:   {}", val_seqs.len());
        println!(
            "  Val window:    {} → {}",
            val_dates
                .first()
                .map(String::as_str)
                .unwrap_or("not available"),
            val_dates
                .last()
                .map(String::as_str)
                .unwrap_or("not available")
        );
        println!(
            "  Final loss:    {:.6}",
            outcome.losses.last().unwrap_or(&0.0)
        );
        println!("  Best epoch:    {}", outcome.best_epoch);
        println!("  Val MSE:       {:.6}", val_mse);
        println!("  Val IC:        {:.4}", val_ic);
        println!("  Direction Acc: {:.2}%", direction.accuracy * 100.0);
        println!("  CPU threads:   {}", worker_threads);
        println!("  Model:         {}", model_path.display());
    }

    Ok(())
}

// Converts panic payloads from optional native accelerators into errors.
fn panic_payload_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "unknown panic payload".to_string()
}

// Runs accelerated LSTM code behind a panic boundary so auto can fall back.
fn catch_accelerated_lstm_panic<F>(f: F) -> anyhow::Result<()>
where
    F: FnOnce() -> anyhow::Result<()>,
{
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(AssertUnwindSafe(f));
    std::panic::set_hook(previous_hook);
    match result {
        Ok(result) => result,
        Err(payload) => anyhow::bail!(
            "accelerated LSTM backend panicked: {}",
            panic_payload_message(payload)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Ensures accelerator panics become recoverable errors for auto fallback.
    fn accelerated_lstm_panic_is_converted_to_error() {
        let result = catch_accelerated_lstm_panic(|| panic!("accelerator unavailable"));

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("accelerator unavailable"));
    }
}

// Handles the ml lstm train accelerated CLI action.
fn cmd_ml_lstm_train_accelerated(
    json: bool,
    without_sp500: bool,
    data_window: Option<LstmDataWindow>,
    backend: LstmBackend,
    train_cfg: config::LstmTrainingConfig,
    target_mode: TargetMode,
) -> anyhow::Result<()> {
    let _ = (json, without_sp500, &train_cfg, target_mode, &data_window);
    match backend {
        LstmBackend::Mlx => {
            #[cfg(mlai_mlx)]
            {
                cmd_ml_lstm_train_mlx(json, without_sp500, data_window, train_cfg, target_mode)
            }
            #[cfg(not(mlai_mlx))]
            {
                anyhow::bail!("MLX backend is not available in this build.")
            }
        }
        LstmBackend::Tch => {
            #[cfg(mlai_tch)]
            {
                let cuda_available = tch::Cuda::is_available();
                anyhow::bail!(
                    "tch backend is linked (CUDA available: {}), but LSTM training is not implemented yet. CPU/Rayon remains the working backend.",
                    cuda_available
                )
            }
            #[cfg(not(mlai_tch))]
            {
                anyhow::bail!("tch backend is not available in this build.")
            }
        }
        LstmBackend::Auto | LstmBackend::Cpu => unreachable!("accelerated backend expected"),
    }
}

#[cfg(mlai_mlx)]
#[derive(Debug, mlx_macros::ModuleParameters)]
#[module(root = mlx_rs)]
struct MlxReturnLstm {
    hidden_dim: usize,
    target_mode: TargetMode,
    direction_threshold: f64,
    dropout_rate: f64,
    training: bool,
    #[param]
    w_i: mlx_rs::module::Param<mlx_rs::Array>,
    #[param]
    b_i: mlx_rs::module::Param<mlx_rs::Array>,
    #[param]
    w_f: mlx_rs::module::Param<mlx_rs::Array>,
    #[param]
    b_f: mlx_rs::module::Param<mlx_rs::Array>,
    #[param]
    w_o: mlx_rs::module::Param<mlx_rs::Array>,
    #[param]
    b_o: mlx_rs::module::Param<mlx_rs::Array>,
    #[param]
    w_c: mlx_rs::module::Param<mlx_rs::Array>,
    #[param]
    b_c: mlx_rs::module::Param<mlx_rs::Array>,
    #[param]
    w_out: mlx_rs::module::Param<mlx_rs::Array>,
    #[param]
    b_out: mlx_rs::module::Param<mlx_rs::Array>,
}

#[cfg(mlai_mlx)]
impl MlxReturnLstm {
    // Constructs a new instance with the provided inputs.
    fn new(
        hidden_dim: usize,
        target_mode: TargetMode,
        direction_threshold: f64,
        dropout_rate: f64,
    ) -> anyhow::Result<Self> {
        use mlx_rs::{random, Array};
        let gate_dim = hidden_dim + INPUT_DIM;
        let scale = (2.0f32 / (hidden_dim + gate_dim) as f32).sqrt();
        let out_scale = (2.0f32 / hidden_dim.max(1) as f32).sqrt() * 0.1;
        Ok(Self {
            hidden_dim,
            target_mode,
            direction_threshold,
            dropout_rate,
            training: true,
            w_i: mlx_rs::module::Param::new(random::uniform::<_, f32>(
                -scale,
                scale,
                &[hidden_dim as i32, gate_dim as i32],
                None,
            )?),
            b_i: mlx_rs::module::Param::new(Array::zeros::<f32>(&[hidden_dim as i32])?),
            w_f: mlx_rs::module::Param::new(random::uniform::<_, f32>(
                -scale,
                scale,
                &[hidden_dim as i32, gate_dim as i32],
                None,
            )?),
            b_f: mlx_rs::module::Param::new(mlx_rs::Array::full::<f32>(
                &[hidden_dim as i32],
                mlx_rs::array!(1.0f32),
            )?),
            w_o: mlx_rs::module::Param::new(random::uniform::<_, f32>(
                -scale,
                scale,
                &[hidden_dim as i32, gate_dim as i32],
                None,
            )?),
            b_o: mlx_rs::module::Param::new(Array::zeros::<f32>(&[hidden_dim as i32])?),
            w_c: mlx_rs::module::Param::new(random::uniform::<_, f32>(
                -scale,
                scale,
                &[hidden_dim as i32, gate_dim as i32],
                None,
            )?),
            b_c: mlx_rs::module::Param::new(Array::zeros::<f32>(&[hidden_dim as i32])?),
            w_out: mlx_rs::module::Param::new(random::uniform::<_, f32>(
                -out_scale,
                out_scale,
                &[1, hidden_dim as i32],
                None,
            )?),
            b_out: mlx_rs::module::Param::new(Array::zeros::<f32>(&[1])?),
        })
    }
}

#[cfg(mlai_mlx)]
impl mlx_rs::module::Module<&mlx_rs::Array> for MlxReturnLstm {
    type Error = mlx_rs::error::Exception;
    type Output = mlx_rs::Array;

    // Handles forward logic.
    fn forward(&mut self, x: &mlx_rs::Array) -> Result<Self::Output, Self::Error> {
        use mlx_rs::ops::indexing::{Ellipsis, IndexOp};
        use mlx_rs::ops::{concatenate_axis, matmul, multiply, sigmoid, tanh};

        let batch = x.dim(0);
        let mut h = mlx_rs::Array::zeros::<f32>(&[batch, self.hidden_dim as i32])?;
        let mut c = mlx_rs::Array::zeros::<f32>(&[batch, self.hidden_dim as i32])?;
        for t in 0..SEQ_LEN {
            let xt = x.index((Ellipsis, t as i32, 0..));
            let hx = concatenate_axis(&[h.clone(), xt], -1)?;
            let ig = sigmoid(matmul(&hx, self.w_i.value.t())?.add(&self.b_i.value)?)?;
            let fg = sigmoid(matmul(&hx, self.w_f.value.t())?.add(&self.b_f.value)?)?;
            let og = sigmoid(matmul(&hx, self.w_o.value.t())?.add(&self.b_o.value)?)?;
            let cc = tanh(matmul(&hx, self.w_c.value.t())?.add(&self.b_c.value)?)?;
            c = fg.multiply(&c)?.add(ig.multiply(&cc)?)?;
            h = og.multiply(tanh(&c)?)?;
        }
        if self.training && self.dropout_rate > 0.0 {
            let keep = (1.0 - self.dropout_rate).clamp(0.001, 1.0) as f32;
            let keep_array = mlx_rs::array!(keep);
            let mask = mlx_rs::random::bernoulli(&keep_array, h.shape(), None)?.as_type::<f32>()?;
            h = multiply(multiply(mlx_rs::array!(1.0f32 / keep), mask)?, h)?;
        }
        let out = matmul(&h, self.w_out.value.t())?
            .add(&self.b_out.value)?
            .squeeze_axes(&[-1])?;
        match self.target_mode {
            TargetMode::Regression => Ok(out),
            TargetMode::Direction => mlx_rs::ops::sigmoid(&out),
        }
    }

    // Handles training mode logic.
    fn training_mode(&mut self, mode: bool) {
        self.training = mode;
    }
}

#[cfg(mlai_mlx)]
// Handles batch to mlx arrays logic.
fn batch_to_mlx_arrays(
    sequences: &[Vec<Vec<f64>>],
    targets: &[f64],
    indices: &[usize],
) -> (mlx_rs::Array, mlx_rs::Array) {
    let mut x = Vec::with_capacity(indices.len() * SEQ_LEN * INPUT_DIM);
    let mut y = Vec::with_capacity(indices.len());
    for &idx in indices {
        for step in &sequences[idx] {
            x.extend(step.iter().map(|value| *value as f32));
        }
        y.push(targets[idx] as f32);
    }
    (
        mlx_rs::Array::from_slice(
            &x,
            &[indices.len() as i32, SEQ_LEN as i32, INPUT_DIM as i32],
        ),
        mlx_rs::Array::from_slice(&y, &[indices.len() as i32]),
    )
}

#[cfg(mlai_mlx)]
// Handles MLX predict batches acceleration support.
fn mlx_predict_batches(
    model: &mut MlxReturnLstm,
    sequences: &[Vec<Vec<f64>>],
    batch_size: usize,
) -> anyhow::Result<Vec<f64>> {
    use mlx_rs::module::Module;

    let dummy_targets = vec![0.0; sequences.len()];
    let mut preds = Vec::with_capacity(sequences.len());
    for start in (0..sequences.len()).step_by(batch_size) {
        let end = (start + batch_size).min(sequences.len());
        let indices: Vec<usize> = (start..end).collect();
        let (x, _) = batch_to_mlx_arrays(sequences, &dummy_targets, &indices);
        let batch_preds = model.forward(&x)?;
        preds.extend(
            batch_preds
                .as_slice::<f32>()
                .iter()
                .map(|value| *value as f64),
        );
    }
    Ok(preds)
}

#[cfg(mlai_mlx)]
// Estimates MLX validation loss on a bounded sample for early stopping.
fn mlx_validation_loss_sample(
    model: &mut MlxReturnLstm,
    sequences: &[Vec<Vec<f64>>],
    targets: &[f64],
    batch_size: usize,
    max_samples: usize,
    loss_kind: LstmLossKind,
    huber_delta: f64,
) -> anyhow::Result<f64> {
    let n = sequences.len().min(targets.len()).min(max_samples);
    if n == 0 {
        return Ok(f64::INFINITY);
    }
    let preds = mlx_predict_batches(model, &sequences[..n], batch_size)?;
    Ok(preds
        .iter()
        .zip(&targets[..n])
        .map(|(pred, target)| {
            loss_kind
                .sample_loss_and_grad(*pred, *target, huber_delta)
                .0
        })
        .sum::<f64>()
        / n as f64)
}

#[cfg(mlai_mlx)]
// Handles MLX model to cpu model acceleration support.
fn mlx_model_to_cpu_model(
    model: &MlxReturnLstm,
    target_scaler: TargetScaler,
) -> anyhow::Result<LstmModel> {
    // Copies a flat MLX parameter into the portable CPU inference format.
    fn param_vec(param: &mlx_rs::module::Param<mlx_rs::Array>) -> Vec<f64> {
        param
            .value
            .as_slice::<f32>()
            .iter()
            .map(|value| *value as f64)
            .collect()
    }

    let output_bias = model
        .b_out
        .value
        .as_slice::<f32>()
        .first()
        .copied()
        .unwrap_or(0.0) as f64;

    Ok(LstmModel {
        hidden_dim: model.hidden_dim,
        target_mode: model.target_mode,
        direction_threshold: model.direction_threshold,
        target_scaler,
        w_i: param_vec(&model.w_i),
        b_i: param_vec(&model.b_i),
        w_f: param_vec(&model.w_f),
        b_f: param_vec(&model.b_f),
        w_o: param_vec(&model.w_o),
        b_o: param_vec(&model.b_o),
        w_c: param_vec(&model.w_c),
        b_c: param_vec(&model.b_c),
        w_out: param_vec(&model.w_out),
        b_out: output_bias,
    })
}

#[cfg(mlai_mlx)]
// Verifies MLX can execute Metal kernels before loading the large dataset.
fn mlx_runtime_smoke_test() -> anyhow::Result<()> {
    use mlx_rs::{random, Device};

    Device::set_default(&Device::gpu());
    let probe = random::normal::<f32>(&[1], None, None, None)?;
    probe.eval()?;
    Ok(())
}

#[cfg(mlai_mlx)]
// Handles the ml lstm train mlx CLI action.
fn cmd_ml_lstm_train_mlx(
    json: bool,
    without_sp500: bool,
    data_window: Option<LstmDataWindow>,
    train_cfg: config::LstmTrainingConfig,
    target_mode: TargetMode,
) -> anyhow::Result<()> {
    use mlx_rs::builder::Builder;
    use mlx_rs::module::{Module, ModuleParameters, ModuleParametersExt};
    use mlx_rs::nn;
    use mlx_rs::optimizers::{Adam, Optimizer};
    use mlx_rs::transforms::eval_params;
    use mlx_rs::{ops, random, Device};

    Device::set_default(&Device::gpu());
    mlx_runtime_smoke_test()?;
    random::seed(42)?;
    let model_path = lstm_model_path(without_sp500);

    eprintln!("🧠 LSTM Training — MLX Apple Silicon Engine");
    eprintln!("  Backend: mlx");
    eprintln!("  Device: {}", Device::try_default()?);
    if without_sp500 {
        eprintln!("  Variant: without S&P 500 signal (SP500 features zeroed)");
    }
    eprintln!("{}", "═".repeat(50));

    let conn = open_lstm_db()?;
    eprintln!("\n📊 Loading training data...");
    let dataset = load_sequences(&conn, usize::MAX, without_sp500, data_window.clone(), !json)?;

    if dataset.sequences.len() < 1000 {
        anyhow::bail!(
            "Not enough sequences ({}) for LSTM training. Need >= 1000.",
            dataset.sequences.len()
        );
    }

    let n = dataset.sequences.len();
    let split = (n as f64 * 0.8) as usize;
    let train_seqs = &dataset.sequences[..split];
    let train_returns = &dataset.targets[..split];
    let train_dates = &dataset.dates[..split];
    let val_seqs = &dataset.sequences[split..];
    let val_returns = &dataset.targets[split..];
    let val_dates = &dataset.dates[split..];
    let target_scaler = TargetScaler::fit(target_mode, train_returns);
    let train_targets = training_targets(
        train_returns,
        target_mode,
        train_cfg.direction_threshold,
        target_scaler,
    );
    let val_targets = training_targets(
        val_returns,
        target_mode,
        train_cfg.direction_threshold,
        target_scaler,
    );

    eprintln!(
        "  Train: {} sequences ({} → {}), Val: {} sequences ({} → {})",
        train_seqs.len(),
        train_dates
            .first()
            .map(String::as_str)
            .unwrap_or("not available"),
        train_dates
            .last()
            .map(String::as_str)
            .unwrap_or("not available"),
        val_seqs.len(),
        val_dates
            .first()
            .map(String::as_str)
            .unwrap_or("not available"),
        val_dates
            .last()
            .map(String::as_str)
            .unwrap_or("not available")
    );

    let mut model = MlxReturnLstm::new(
        train_cfg.hidden_dim,
        target_mode,
        train_cfg.direction_threshold,
        train_cfg.dropout_rate,
    )?;
    let mut optimizer = Adam::new(train_cfg.learning_rate as f32);
    let loss_kind = LstmLossKind::parse(&train_cfg.loss_function)?;
    let huber_delta = train_cfg.huber_delta as f32;
    let bce_loss = mlx_rs::losses::BinaryCrossEntropyBuilder::new()
        .inputs_are_logits(false)
        .reduction(mlx_rs::losses::LossReduction::Mean)
        .build()
        .map_err(|err| anyhow::anyhow!("unable to initialize MLX BCE loss: {err}"))?;
    let loss_fn = |model: &mut MlxReturnLstm,
                   (x, y): (&mlx_rs::Array, &mlx_rs::Array)|
     -> Result<mlx_rs::Array, mlx_rs::error::Exception> {
        let pred = model.forward(x)?;
        let err = pred.subtract(y)?;
        match loss_kind {
            LstmLossKind::Mse => ops::mean(&ops::square(err)?, None),
            LstmLossKind::L1 => ops::mean(&ops::abs(err)?, None),
            LstmLossKind::Huber => {
                let abs_err = ops::abs(&err)?;
                let delta = mlx_rs::array!(huber_delta.max(1e-6));
                let quadratic = ops::minimum(&abs_err, &delta)?;
                let linear = abs_err.subtract(&quadratic)?;
                let quad_loss = ops::square(&quadratic)?.multiply(mlx_rs::array!(0.5f32))?;
                let lin_loss = linear.multiply(&delta)?;
                ops::mean(&quad_loss.add(&lin_loss)?, None)
            }
            LstmLossKind::Bce => bce_loss.apply(&pred, y),
        }
    };
    let mut value_and_grad = nn::value_and_grad(loss_fn);
    let batch_size = config::lstm_batch_size();
    let mut rng = Rng::new(42);
    let mut indices: Vec<usize> = (0..train_seqs.len()).collect();
    let mut losses = Vec::new();
    let mut validation_losses = Vec::new();
    let mut best_epoch = 0usize;
    let mut best_validation_loss = f64::INFINITY;
    let mut best_cpu_model: Option<LstmModel> = None;
    let mut no_improve_epochs = 0usize;

    eprintln!(
        "\n🏗️  Training MLX LSTM (hidden={}, seq_len={}, batch={}, target={})...",
        train_cfg.hidden_dim,
        SEQ_LEN,
        batch_size,
        target_mode.as_str()
    );
    let batches_per_epoch = indices.len().div_ceil(batch_size);
    let progress = crate::progress::bar_if(
        !json,
        (train_cfg.epochs * batches_per_epoch) as u64,
        "Training MLX LSTM",
    );
    for epoch in 0..train_cfg.epochs {
        for i in (1..indices.len()).rev() {
            let j = (rng.next_u64() as usize) % (i + 1);
            indices.swap(i, j);
        }

        let mut total_loss = 0.0f64;
        let mut total_rows = 0usize;
        for batch in indices.chunks(batch_size) {
            let (x, y) = batch_to_mlx_arrays(train_seqs, &train_targets, batch);
            let (loss, gradients) = value_and_grad(&mut model, (&x, &y))?;
            optimizer.update(&mut model, gradients)?;
            if train_cfg.weight_decay > 0.0 {
                let decay =
                    (1.0 - train_cfg.learning_rate * train_cfg.weight_decay).max(0.0) as f32;
                let decay_array = mlx_rs::array!(decay);
                model.w_i.value = model.w_i.value.multiply(&decay_array)?;
                model.w_f.value = model.w_f.value.multiply(&decay_array)?;
                model.w_o.value = model.w_o.value.multiply(&decay_array)?;
                model.w_c.value = model.w_c.value.multiply(&decay_array)?;
                model.w_out.value = model.w_out.value.multiply(&decay_array)?;
            }
            eval_params(model.parameters())?;
            total_loss += loss.item::<f32>() as f64 * batch.len() as f64;
            total_rows += batch.len();
            progress.inc(1);
        }

        let avg_loss = total_loss / total_rows as f64;
        losses.push(avg_loss);
        model.training_mode(false);
        model.eval()?;
        let validation_loss = if train_cfg.early_stopping_enabled {
            mlx_validation_loss_sample(
                &mut model,
                val_seqs,
                &val_targets,
                batch_size,
                train_cfg.early_stopping_sample_size,
                loss_kind,
                train_cfg.huber_delta,
            )?
        } else {
            avg_loss
        };
        model.training_mode(true);
        validation_losses.push(validation_loss);
        progress.set_message(format!(
            "epoch {}/{} loss={avg_loss:.6} val={validation_loss:.6}",
            epoch + 1,
            train_cfg.epochs
        ));
        if epoch % 2 == 0 || epoch == train_cfg.epochs - 1 {
            eprintln!(
                "  Epoch {}/{}: loss={:.6}, val={:.6}",
                epoch + 1,
                train_cfg.epochs,
                avg_loss,
                validation_loss
            );
        }

        if validation_loss + train_cfg.early_stopping_min_delta < best_validation_loss {
            best_validation_loss = validation_loss;
            best_epoch = epoch + 1;
            best_cpu_model = Some(mlx_model_to_cpu_model(&model, target_scaler)?);
            no_improve_epochs = 0;
        } else {
            no_improve_epochs += 1;
            if train_cfg.early_stopping_enabled
                && no_improve_epochs >= train_cfg.early_stopping_patience
            {
                eprintln!(
                    "  Early stopping: best epoch {} val={:.6}",
                    best_epoch, best_validation_loss
                );
                break;
            }
        }
    }
    progress.finish_and_clear();
    let stopped_early = train_cfg.early_stopping_enabled && losses.len() < train_cfg.epochs;

    let cpu_model = if let Some(best_cpu_model) = best_cpu_model {
        best_cpu_model
    } else {
        mlx_model_to_cpu_model(&model, target_scaler)?
    };

    eprintln!("\n📈 Validation...");
    let progress = crate::progress::bar_if(!json, val_seqs.len() as u64, "Validating saved LSTM");
    let mut val_preds = Vec::with_capacity(val_seqs.len());
    for seq in val_seqs {
        val_preds.push(cpu_model.forward(seq));
        progress.inc(1);
    }
    progress.finish_and_clear();
    let val_ic = spearman_corr(&val_preds, val_returns);
    let val_mse = validation_mse(&val_preds, val_returns, &val_targets, target_mode);
    let direction = direction_metrics(
        &val_preds,
        val_returns,
        target_mode,
        train_cfg.direction_threshold,
    );
    eprintln!("  Val MSE: {:.6}, Val IC: {:.4}", val_mse, val_ic);

    let model_path_str = model_path.to_string_lossy().to_string();
    cpu_model.save(&model_path_str)?;
    eprintln!("\n  Model saved: {}", model_path.display());

    let report_path = if without_sp500 {
        paths::state_dir().join("lstm_without_sp500_training_report.json")
    } else {
        paths::state_dir().join("lstm_training_report.json")
    };
    paths::write_private_file(
        &report_path,
        serde_json::to_string_pretty(&serde_json::json!({
            "status": "done",
            "backend": "mlx",
            "device": Device::try_default()?.to_string(),
            "variant": if without_sp500 { "without_sp500" } else { "with_sp500" },
            "model_path": model_path.display().to_string(),
            "data_window": &data_window,
            "profile": train_cfg.profile,
            "train_samples": train_seqs.len(),
            "val_samples": val_seqs.len(),
            "split": {
                "method": "chronological_sorted_by_date_symbol",
                "train_start_date": train_dates.first().map(String::as_str).unwrap_or("not available"),
                "train_end_date": train_dates.last().map(String::as_str).unwrap_or("not available"),
                "validation_start_date": val_dates.first().map(String::as_str).unwrap_or("not available"),
                "validation_end_date": val_dates.last().map(String::as_str).unwrap_or("not available")
            },
            "final_loss": losses.last().unwrap_or(&0.0),
            "validation_losses": &validation_losses,
            "best_epoch": best_epoch,
            "best_validation_loss": best_validation_loss,
            "stopped_early": stopped_early,
            "target_mode": target_mode.as_str(),
            "direction_threshold": train_cfg.direction_threshold,
            "target_scaler": target_scaler,
            "loss_function": train_cfg.loss_function,
            "huber_delta": train_cfg.huber_delta,
            "dropout_rate": train_cfg.dropout_rate,
            "weight_decay": train_cfg.weight_decay,
            "direction_metrics": &direction,
            "val_mse": val_mse,
            "val_ic": val_ic,
            "hidden_dim": train_cfg.hidden_dim,
            "seq_len": SEQ_LEN,
            "epochs": losses.len(),
            "configured_epochs": train_cfg.epochs,
            "learning_rate": train_cfg.learning_rate,
        }))?,
    )?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "status": "done",
                "backend": "mlx",
                "device": Device::try_default()?.to_string(),
                "variant": if without_sp500 { "without_sp500" } else { "with_sp500" },
                "model_path": model_path.display().to_string(),
                "data_window": &data_window,
                "profile": train_cfg.profile,
                "train_samples": train_seqs.len(),
                "val_samples": val_seqs.len(),
                "split": {
                    "method": "chronological_sorted_by_date_symbol",
                    "train_start_date": train_dates.first().map(String::as_str).unwrap_or("not available"),
                    "train_end_date": train_dates.last().map(String::as_str).unwrap_or("not available"),
                    "validation_start_date": val_dates.first().map(String::as_str).unwrap_or("not available"),
                    "validation_end_date": val_dates.last().map(String::as_str).unwrap_or("not available")
                },
                "final_loss": losses.last().unwrap_or(&0.0),
                "validation_losses": &validation_losses,
                "best_epoch": best_epoch,
                "best_validation_loss": best_validation_loss,
                "stopped_early": stopped_early,
                "target_mode": target_mode.as_str(),
                "direction_threshold": train_cfg.direction_threshold,
                "target_scaler": target_scaler,
                "loss_function": train_cfg.loss_function,
                "huber_delta": train_cfg.huber_delta,
                "dropout_rate": train_cfg.dropout_rate,
                "weight_decay": train_cfg.weight_decay,
                "direction_metrics": &direction,
                "val_mse": val_mse,
                "val_ic": val_ic,
                "hidden_dim": train_cfg.hidden_dim,
                "seq_len": SEQ_LEN,
                "epochs": losses.len(),
                "configured_epochs": train_cfg.epochs,
                "learning_rate": train_cfg.learning_rate,
            })
        );
    } else {
        println!("🧠 LSTM Training Complete");
        println!("{}", "─".repeat(40));
        println!("  Backend:       MLX");
        println!("  Device:        {}", Device::try_default()?);
        println!(
            "  Architecture:  LSTM({} → {} → 1)",
            INPUT_DIM, train_cfg.hidden_dim
        );
        println!("  Profile:       {}", train_cfg.profile);
        println!("  Target mode:   {}", target_mode.as_str());
        if target_scaler.enabled {
            println!(
                "  Target scale:  z-score mean={:.6}, std={:.6}",
                target_scaler.mean, target_scaler.std
            );
        }
        println!("  Loss:          {}", train_cfg.loss_function);
        println!("  Dropout:       {:.2}", train_cfg.dropout_rate);
        println!("  Weight decay:  {:.4}", train_cfg.weight_decay);
        if let Some(window) = &data_window {
            println!(
                "  Data window:   {} → {}",
                window.start_date, window.end_date
            );
        }
        println!("  Seq length:    {}", SEQ_LEN);
        println!("  Train samples: {}", train_seqs.len());
        println!("  Val samples:   {}", val_seqs.len());
        println!(
            "  Val window:    {} → {}",
            val_dates
                .first()
                .map(String::as_str)
                .unwrap_or("not available"),
            val_dates
                .last()
                .map(String::as_str)
                .unwrap_or("not available")
        );
        println!("  Final loss:    {:.6}", losses.last().unwrap_or(&0.0));
        println!("  Best epoch:    {}", best_epoch);
        println!("  Val MSE:       {:.6}", val_mse);
        println!("  Val IC:        {:.4}", val_ic);
        println!("  Direction Acc: {:.2}%", direction.accuracy * 100.0);
        println!("  Model:         {}", model_path.display());
    }

    Ok(())
}

// ══════════════════════════════════════════════════════════════════
// CMD: ml lstm-predict — inference for latest date
// ══════════════════════════════════════════════════════════════════

pub fn cmd_ml_lstm_predict(json: bool, without_sp500: bool) -> anyhow::Result<()> {
    let model_path = lstm_model_path(without_sp500);

    if !model_path.exists() {
        anyhow::bail!(
            "LSTM model not found: {} — run 'mlai-trade ml lstm-train' first",
            model_path.display()
        );
    }

    eprintln!("Loading LSTM model...");
    if without_sp500 {
        eprintln!("  Variant: without S&P 500 signal (SP500 features zeroed)");
    }
    let model_path_str = model_path.to_string_lossy().to_string();
    let model = LstmModel::load(&model_path_str)?;
    let conn = open_lstm_db()?;

    let pred_table = lstm_predictions_table(without_sp500);
    // Init tables for storing LSTM predictions
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {pred_table} (
            symbol TEXT NOT NULL,
            date TEXT NOT NULL,
            lstm_score REAL NOT NULL,
            PRIMARY KEY (symbol, date)
        );
        CREATE INDEX IF NOT EXISTS idx_lstm_pred_date_{pred_table} ON {pred_table}(date);"
    ))?;

    let feature_cols = FEATURE_COLS.join(", ");

    // Get latest date with features
    let latest_date: String = conn.query_row(
        "SELECT COALESCE(MAX(date),'none') FROM ml_features WHERE return_1d IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    if latest_date == "none" {
        anyhow::bail!("No features found.");
    }

    eprintln!("Predicting for date {}...", latest_date);

    // We need SEQ_LEN days of features for each symbol
    // Get symbols that have features on the latest date
    let eligible = crate::ml::ml_eligible_asset_predicate("f.symbol", "a");
    let mut sym_stmt = conn.prepare(&format!(
        "SELECT DISTINCT f.symbol
         FROM ml_features f
         LEFT JOIN assets a ON a.symbol = f.symbol
         WHERE f.date = ?1
           AND f.return_1d IS NOT NULL
           AND {eligible}"
    ))?;
    let symbols: Vec<String> = sym_stmt
        .query_map(params![latest_date], |r| r.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    let query = format!(
        "SELECT {fcols} FROM ml_features
         WHERE symbol = ?1 AND return_1d IS NOT NULL
         ORDER BY date DESC LIMIT {seq}",
        fcols = feature_cols,
        seq = SEQ_LEN
    );

    let mut predictions: Vec<(String, f64)> = Vec::new();
    let progress = crate::progress::bar_if(
        !json,
        symbols.len() as u64,
        if without_sp500 {
            "LSTM predictions without S&P 500"
        } else {
            "LSTM predictions"
        },
    );

    for symbol in &symbols {
        if config::is_blocked_symbol(symbol) {
            progress.inc(1);
            continue;
        }

        let mut stmt = conn.prepare_cached(&query)?;
        let rows: Vec<Vec<f64>> = stmt
            .query_map(params![symbol], |r| {
                let mut feats = Vec::with_capacity(INPUT_DIM);
                for i in 0..INPUT_DIM {
                    let v: Option<f64> = r.get(i)?;
                    feats.push(v.unwrap_or(0.0));
                }
                Ok(feats)
            })?
            .filter_map(|r| r.ok())
            .collect();

        if rows.len() < SEQ_LEN {
            progress.inc(1);
            continue;
        }

        // Rows come DESC, reverse to chronological
        let mut seq: Vec<Vec<f64>> = rows.into_iter().rev().collect();

        // Z-score normalize within window
        let mut means = vec![0.0; INPUT_DIM];
        let mut stds = vec![0.0; INPUT_DIM];
        for feat in &seq {
            for j in 0..INPUT_DIM {
                means[j] += feat[j];
            }
        }
        for j in 0..INPUT_DIM {
            means[j] /= SEQ_LEN as f64;
        }
        for feat in &seq {
            for j in 0..INPUT_DIM {
                let d = feat[j] - means[j];
                stds[j] += d * d;
            }
        }
        for j in 0..INPUT_DIM {
            stds[j] = (stds[j] / SEQ_LEN as f64).sqrt().max(1e-8);
        }
        for step in &mut seq {
            for j in 0..INPUT_DIM {
                step[j] = (step[j] - means[j]) / stds[j];
            }
        }
        if without_sp500 {
            zero_sp500_features(&mut seq);
        }

        let score = model.forward(&seq);
        predictions.push((symbol.clone(), score));
        progress.inc(1);
    }
    progress.finish_and_clear();

    // Store in DB
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        &format!("DELETE FROM {pred_table} WHERE date = ?1"),
        params![latest_date],
    )?;
    {
        let mut ins = tx.prepare_cached(&format!(
            "INSERT INTO {pred_table} (symbol, date, lstm_score) VALUES (?1, ?2, ?3)"
        ))?;
        for (sym, score) in &predictions {
            ins.execute(params![sym, latest_date, score])?;
        }
    }
    tx.commit()?;

    // Sort by score desc for display
    predictions.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    if json {
        let top: Vec<serde_json::Value> = predictions.iter().take(30)
            .map(|(s, sc)| serde_json::json!({"symbol": s, "lstm_score": (sc * 10000.0).round() / 10000.0}))
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "status": "done",
                "variant": if without_sp500 { "without_sp500" } else { "with_sp500" },
                "date": latest_date,
                "total": predictions.len(),
                "predictions": top,
            })
        );
    } else {
        println!("🧠 LSTM Predictions (Top 20) — {}", latest_date);
        println!("{:<8} {:>12}", "Symbol", "LSTM Score");
        println!("{}", "─".repeat(24));
        for (sym, score) in predictions.iter().take(20) {
            println!("{:<8} {:>12.4}", sym, score);
        }
        println!("\nTotal: {} predictions", predictions.len());
    }

    Ok(())
}

// Handles the ml lstm evaluate CLI action.
pub fn cmd_ml_lstm_evaluate(
    json: bool,
    without_sp500: bool,
    top_n: usize,
    slippage_bps: f64,
    data_window: Option<LstmDataWindow>,
    export_predictions: Option<&std::path::Path>,
) -> anyhow::Result<serde_json::Value> {
    let model_path = lstm_model_path(without_sp500);
    if !model_path.exists() {
        anyhow::bail!(
            "LSTM model not found: {} — run 'mlai-trade ml lstm-train' first",
            model_path.display()
        );
    }

    let model_path_str = model_path.to_string_lossy().to_string();
    let model = LstmModel::load(&model_path_str)?;
    let conn = open_lstm_db()?;
    let dataset = load_sequences(&conn, usize::MAX, without_sp500, data_window.clone(), !json)?;
    if dataset.sequences.len() < 1000 {
        anyhow::bail!(
            "Not enough sequences ({}) for LSTM evaluation. Need >= 1000.",
            dataset.sequences.len()
        );
    }

    let split = (dataset.sequences.len() as f64 * 0.8) as usize;
    let val_seqs = &dataset.sequences[split..];
    let val_returns = &dataset.targets[split..];
    let val_targets = training_targets(
        val_returns,
        model.target_mode,
        model.direction_threshold,
        model.target_scaler,
    );
    let val_symbols = &dataset.symbols[split..];
    let val_dates = &dataset.dates[split..];

    let progress = crate::progress::bar_if(
        !json,
        val_seqs.len() as u64,
        "Evaluating LSTM validation set",
    );
    let worker_threads = config::cpu_worker_threads().max(1);
    let preds: Vec<f64> = rayon::ThreadPoolBuilder::new()
        .num_threads(worker_threads)
        .build()?
        .install(|| val_seqs.par_iter().map(|seq| model.forward(seq)).collect());
    progress.set_position(val_seqs.len() as u64);
    progress.finish_and_clear();
    let val_mse = validation_mse(&preds, val_returns, &val_targets, model.target_mode);
    let val_ic = spearman_corr(&preds, val_returns);
    let direction = direction_metrics(
        &preds,
        val_returns,
        model.target_mode,
        model.direction_threshold,
    );
    let scored = preds
        .iter()
        .zip(val_returns)
        .zip(val_symbols.iter().zip(val_dates))
        .map(
            |((score, fwd_return), (symbol, date))| crate::ml::ScoredReturn {
                symbol: symbol.clone(),
                date: date.clone(),
                score: *score,
                fwd_return: *fwd_return,
            },
        )
        .collect::<Vec<_>>();
    let trading_metrics = crate::ml::trading_metrics_json("lstm", &scored, top_n, slippage_bps);
    if let Some(path) = export_predictions {
        write_lstm_prediction_export(
            path,
            &preds,
            val_targets.as_slice(),
            val_returns,
            val_symbols,
            val_dates,
            model.target_mode,
            model.direction_threshold,
        )?;
    }

    let report = serde_json::json!({
        "status": "done",
        "variant": if without_sp500 { "without_sp500" } else { "with_sp500" },
        "model_path": model_path.display().to_string(),
        "data_window": data_window,
        "split": {
            "method": "chronological_sorted_by_date_symbol",
            "train_samples": split,
            "validation_samples": val_targets.len(),
            "validation_start_date": val_dates.first().map(String::as_str).unwrap_or("not available"),
            "validation_end_date": val_dates.last().map(String::as_str).unwrap_or("not available")
        },
        "prediction_export": export_predictions.map(|path| path.display().to_string()),
        "val_samples": val_targets.len(),
        "valid_mse": val_mse,
        "valid_ic_spearman": val_ic,
        "target_mode": model.target_mode.as_str(),
        "direction_threshold": model.direction_threshold,
        "target_scaler": model.target_scaler,
        "direction_metrics": direction,
        "trading_metrics_after_slippage": trading_metrics,
    });
    let report_path = if without_sp500 {
        paths::state_dir().join("lstm_without_sp500_evaluation_report.json")
    } else {
        paths::state_dir().join("lstm_evaluation_report.json")
    };
    paths::write_private_file(&report_path, serde_json::to_string_pretty(&report)?)?;

    if json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("🧠 LSTM Evaluation");
        println!("  Val samples: {}", val_targets.len());
        println!(
            "  Val window:  {} → {}",
            val_dates
                .first()
                .map(String::as_str)
                .unwrap_or("not available"),
            val_dates
                .last()
                .map(String::as_str)
                .unwrap_or("not available")
        );
        println!("  Val MSE:     {:.6}", val_mse);
        println!("  Val IC:      {:.4}", val_ic);
        println!("  Report:      {}", report_path.display());
        if let Some(path) = export_predictions {
            println!("  Pred export: {}", path.display());
        }
    }

    Ok(report)
}

// Escapes one field for CSV export.
fn csv_escape(value: &str) -> String {
    if value
        .chars()
        .any(|ch| matches!(ch, ',' | '"' | '\n' | '\r'))
    {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

// Writes exact validation rows used by LSTM evaluation for diagnostics.
fn write_lstm_prediction_export(
    path: &std::path::Path,
    preds: &[f64],
    targets: &[f64],
    returns: &[f64],
    symbols: &[String],
    dates: &[String],
    target_mode: TargetMode,
    threshold: f64,
) -> anyhow::Result<()> {
    let mut out = std::io::BufWriter::new(paths::create_private_file(path)?);
    writeln!(
        out,
        "date,symbol,prediction,target,actual_fwd_5d,predicted_up,actual_up"
    )?;
    for (((pred, target), actual), (symbol, date)) in preds
        .iter()
        .zip(targets)
        .zip(returns)
        .zip(symbols.iter().zip(dates))
    {
        let predicted_up = match target_mode {
            TargetMode::Regression => *pred > threshold,
            TargetMode::Direction => *pred >= 0.5,
        };
        let actual_up = *actual > threshold;
        writeln!(
            out,
            "{},{},{:.12},{:.12},{:.12},{},{}",
            csv_escape(date),
            csv_escape(symbol),
            pred,
            target,
            actual,
            predicted_up,
            actual_up
        )?;
    }
    Ok(())
}

// Handles validation scores logic.
pub fn validation_scores(without_sp500: bool) -> anyhow::Result<Vec<crate::ml::ScoredReturn>> {
    let model_path = lstm_model_path(without_sp500);
    if !model_path.exists() {
        anyhow::bail!(
            "LSTM model not found: {} — run 'mlai-trade ml lstm-train' first",
            model_path.display()
        );
    }

    let model_path_str = model_path.to_string_lossy().to_string();
    let model = LstmModel::load(&model_path_str)?;
    let conn = open_lstm_db()?;
    let dataset = load_sequences(&conn, usize::MAX, without_sp500, None, false)?;
    if dataset.sequences.len() < 1000 {
        anyhow::bail!(
            "Not enough sequences ({}) for LSTM validation scoring. Need >= 1000.",
            dataset.sequences.len()
        );
    }

    let split = (dataset.sequences.len() as f64 * 0.8) as usize;
    let worker_threads = config::cpu_worker_threads().max(1);
    let scored = rayon::ThreadPoolBuilder::new()
        .num_threads(worker_threads)
        .build()?
        .install(|| {
            (split..dataset.sequences.len())
                .into_par_iter()
                .map(|idx| crate::ml::ScoredReturn {
                    symbol: dataset.symbols[idx].clone(),
                    date: dataset.dates[idx].clone(),
                    score: model.forward(&dataset.sequences[idx]),
                    fwd_return: dataset.targets[idx],
                })
                .collect()
        });
    Ok(scored)
}

// ══════════════════════════════════════════════════════════════════
// HELPERS
// ══════════════════════════════════════════════════════════════════

fn open_lstm_db() -> anyhow::Result<Connection> {
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

/// Spearman rank correlation
fn spearman_corr(a: &[f64], b: &[f64]) -> f64 {
    if a.len() != b.len() || a.len() < 3 {
        return 0.0;
    }
    let n = a.len();

    // Handles ranks logic.
    fn ranks(vals: &[f64]) -> Vec<f64> {
        let mut indexed: Vec<(usize, f64)> =
            vals.iter().enumerate().map(|(i, &v)| (i, v)).collect();
        indexed.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let mut r = vec![0.0; vals.len()];
        for (rank, &(idx, _)) in indexed.iter().enumerate() {
            r[idx] = rank as f64;
        }
        r
    }

    let ra = ranks(a);
    let rb = ranks(b);
    let mean_a: f64 = ra.iter().sum::<f64>() / n as f64;
    let mean_b: f64 = rb.iter().sum::<f64>() / n as f64;

    let mut cov = 0.0;
    let mut var_a = 0.0;
    let mut var_b = 0.0;
    for i in 0..n {
        let da = ra[i] - mean_a;
        let db = rb[i] - mean_b;
        cov += da * db;
        var_a += da * da;
        var_b += db * db;
    }

    if var_a < 1e-15 || var_b < 1e-15 {
        return 0.0;
    }
    cov / (var_a.sqrt() * var_b.sqrt())
}
