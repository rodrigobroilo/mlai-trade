// ══════════════════════════════════════════════════════════════════
// LSTM MODULE — Long Short-Term Memory for stock return prediction
// ══════════════════════════════════════════════════════════════════
//
// Pure Rust implementation. No Python, no external ML framework.
//
// Architecture:
//   Input:  20-day lookback × feature columns (same FEATURE_COLS as LightGBM)
//   LSTM:   64 hidden units, 1 layer
//   Output: Linear(64 → 1) → predicted 5-day forward return
//
// Training: Mini-batch BPTT with Adam optimizer
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
use std::io::{Read, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LstmBackend {
    Auto,
    Cpu,
    Mlx,
    Tch,
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

#[cfg(all(feature = "mlx-lstm", target_os = "macos", target_arch = "aarch64"))]
// Handles MLX auto backend acceleration support.
fn mlx_auto_backend() -> Option<LstmBackend> {
    Some(LstmBackend::Mlx)
}

#[cfg(not(all(feature = "mlx-lstm", target_os = "macos", target_arch = "aarch64")))]
// Handles MLX auto backend acceleration support.
fn mlx_auto_backend() -> Option<LstmBackend> {
    None
}

#[cfg(all(feature = "tch-lstm", target_os = "linux"))]
// Handles tch/CUDA auto backend acceleration support.
fn tch_auto_backend() -> Option<LstmBackend> {
    if tch::Cuda::is_available() {
        eprintln!(
            "⚠️  LSTM auto backend: CUDA is available, but tch/CUDA LSTM training is not implemented yet; falling back to CPU/Rayon."
        );
    } else {
        eprintln!(
            "⚠️  LSTM auto backend: tch-lstm is compiled, but CUDA is not available at runtime; falling back to CPU/Rayon."
        );
    }
    None
}

#[cfg(not(all(feature = "tch-lstm", target_os = "linux")))]
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
            #[cfg(all(
                not(feature = "mlx-lstm"),
                target_os = "macos",
                target_arch = "aarch64"
            ))]
            {
                eprintln!(
                    "⚠️  LSTM auto backend: Apple Silicon detected, but mlx-lstm was not enabled; falling back to CPU/Rayon. Build with `--features mlx-lstm` after installing the Apple Metal Toolchain."
                );
            }
            #[cfg(all(not(feature = "tch-lstm"), target_os = "linux"))]
            {
                eprintln!(
                    "⚠️  LSTM auto backend: tch-lstm was not enabled; falling back to CPU/Rayon. Build with `--features tch-lstm` on Linux CUDA hosts."
                );
            }
            Ok(LstmBackend::Cpu)
        }
        LstmBackend::Cpu => Ok(LstmBackend::Cpu),
        LstmBackend::Mlx => {
            #[cfg(all(feature = "mlx-lstm", target_os = "macos", target_arch = "aarch64"))]
            {
                Ok(LstmBackend::Mlx)
            }
            #[cfg(not(all(feature = "mlx-lstm", target_os = "macos", target_arch = "aarch64")))]
            {
                anyhow::bail!(
                    "MLX LSTM backend was requested, but it is not available. Requirements: Apple Silicon macOS, `--features mlx-lstm`, Xcode or Xcode Command Line Tools, and Apple Metal Toolchain (`xcodebuild -downloadComponent MetalToolchain`)."
                )
            }
        }
        LstmBackend::Tch => {
            #[cfg(all(feature = "tch-lstm", target_os = "linux"))]
            {
                if tch::Cuda::is_available() {
                    Ok(LstmBackend::Tch)
                } else {
                    anyhow::bail!(
                        "tch/CUDA LSTM backend was requested, but CUDA is not available at runtime."
                    )
                }
            }
            #[cfg(not(all(feature = "tch-lstm", target_os = "linux")))]
            {
                anyhow::bail!(
                    "tch/CUDA LSTM backend was requested, but it is not available. Requirements: Linux, NVIDIA driver/CUDA visible through `nvidia-smi`, libtorch available to torch-sys, native build tools, and `--features tch-lstm`."
                )
            }
        }
    }
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
const HIDDEN_DIM: usize = 64;
const SEQ_LEN: usize = 20; // 20-day lookback
const GATE_DIM: usize = INPUT_DIM + HIDDEN_DIM; // concatenated [h, x]

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
    // Gate weights: each is HIDDEN_DIM × GATE_DIM (concatenated [h_{t-1}, x_t])
    w_i: Vec<f64>,
    b_i: Vec<f64>, // input gate
    w_f: Vec<f64>,
    b_f: Vec<f64>, // forget gate
    w_o: Vec<f64>,
    b_o: Vec<f64>, // output gate
    w_c: Vec<f64>,
    b_c: Vec<f64>, // cell candidate

    // Output projection: HIDDEN_DIM → 1
    w_out: Vec<f64>, // 1 × HIDDEN_DIM
    b_out: f64,
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

impl LstmModel {
    // Handles new random logic.
    pub fn new_random(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let gd = GATE_DIM;
        let hd = HIDDEN_DIM;

        let mut m = LstmModel {
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
        let hd = HIDDEN_DIM;
        let gd = GATE_DIM;

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

    /// Full forward pass: sequence → scalar prediction
    pub fn forward(&self, sequence: &[Vec<f64>]) -> f64 {
        let hd = HIDDEN_DIM;
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
        out
    }

    /// Forward pass returning caches for backprop
    fn forward_with_cache(&self, sequence: &[Vec<f64>]) -> (f64, Vec<StepCache>) {
        let hd = HIDDEN_DIM;
        let mut h = vec![0.0; hd];
        let mut c = vec![0.0; hd];
        let mut caches = Vec::with_capacity(sequence.len());

        for step in sequence {
            let (h_new, c_new, cache) = self.step_forward(step, &h, &c);
            h = h_new;
            c = c_new;
            caches.push(cache);
        }

        let mut out = self.b_out;
        for i in 0..hd {
            out += self.w_out[i] * h[i];
        }
        (out, caches)
    }

    /// Backward pass through entire sequence (BPTT).
    /// Returns gradients for all parameters.
    fn backward(&self, caches: &[StepCache], d_output: f64) -> LstmGrads {
        let hd = HIDDEN_DIM;
        let gd = GATE_DIM;
        let n_steps = caches.len();

        let mut grads = LstmGrads::zeros();

        // Gradient of output layer
        let last_h = &caches[n_steps - 1].h;
        for i in 0..hd {
            grads.dw_out[i] += d_output * last_h[i];
        }
        grads.db_out += d_output;

        // dh from output layer
        let mut dh_next = vec![0.0; hd];
        for i in 0..hd {
            dh_next[i] = d_output * self.w_out[i];
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
    pub fn train_on_data(
        &mut self,
        sequences: &[Vec<Vec<f64>>],
        targets: &[f64],
        epochs: usize,
        lr: f64,
        batch_size: usize,
        show_progress: bool,
    ) -> Vec<f64> {
        let n = sequences.len();
        eprintln!("  LSTM trainer threads: {}", rayon::current_num_threads());
        let gd = GATE_DIM;
        let hd = HIDDEN_DIM;
        let wsize = hd * gd;

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
                        || (LstmGrads::zeros(), 0.0f64),
                        |mut acc, &idx| {
                            let (pred, caches) = model_snapshot.forward_with_cache(&sequences[idx]);
                            let err = pred - targets[idx];

                            // d_loss/d_pred = 2 * err / bs  (MSE gradient)
                            let d_out = 2.0 * err / bs as f64;
                            let sample_grads = model_snapshot.backward(&caches, d_out);
                            acc.0.accumulate(&sample_grads);
                            acc.1 += err * err;
                            acc
                        },
                    )
                    .reduce(
                        || (LstmGrads::zeros(), 0.0f64),
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

                _n_batches += 1;
                progress.inc(1);
            }

            let avg_loss = total_loss / n as f64;
            epoch_losses.push(avg_loss);
            progress.set_message(format!("epoch {}/{} loss={avg_loss:.6}", epoch + 1, epochs));

            if epoch % 2 == 0 || epoch == epochs - 1 {
                eprintln!("  Epoch {}/{}: loss={:.6}", epoch + 1, epochs, avg_loss);
            }
        }

        progress.finish_and_clear();
        epoch_losses
    }

    /// Save model to binary file
    pub fn save(&self, path: &str) -> anyhow::Result<()> {
        let mut f = crate::paths::create_private_file(std::path::Path::new(path))?;
        // Magic + version
        f.write_all(b"LSTM0001")?;
        // Dimensions
        f.write_all(&(INPUT_DIM as u32).to_le_bytes())?;
        f.write_all(&(HIDDEN_DIM as u32).to_le_bytes())?;
        f.write_all(&(SEQ_LEN as u32).to_le_bytes())?;

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
        if &magic != b"LSTM0001" {
            anyhow::bail!("Invalid LSTM model file (bad magic)");
        }
        let mut buf4 = [0u8; 4];
        f.read_exact(&mut buf4)?;
        let _input_dim = u32::from_le_bytes(buf4);
        f.read_exact(&mut buf4)?;
        let _hidden_dim = u32::from_le_bytes(buf4);
        f.read_exact(&mut buf4)?;
        let _seq_len = u32::from_le_bytes(buf4);

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
    fn zeros() -> Self {
        let wsize = HIDDEN_DIM * GATE_DIM;
        LstmGrads {
            dw_i: vec![0.0; wsize],
            db_i: vec![0.0; HIDDEN_DIM],
            dw_f: vec![0.0; wsize],
            db_f: vec![0.0; HIDDEN_DIM],
            dw_o: vec![0.0; wsize],
            db_o: vec![0.0; HIDDEN_DIM],
            dw_c: vec![0.0; wsize],
            db_c: vec![0.0; HIDDEN_DIM],
            dw_out: vec![0.0; HIDDEN_DIM],
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
        for i in 0..HIDDEN_DIM {
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

/// Load sequences grouped by symbol from DB
fn load_sequences(
    conn: &Connection,
    max_symbols: usize,
    without_sp500: bool,
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
    })
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
) -> anyhow::Result<()> {
    let backend = resolve_lstm_backend(requested_backend)?;
    if backend != LstmBackend::Cpu {
        match cmd_ml_lstm_train_accelerated(json, without_sp500, backend) {
            Ok(()) => return Ok(()),
            Err(err) if requested_backend == LstmBackend::Auto => {
                eprintln!(
                    "⚠️  LSTM auto backend '{}' failed: {}; falling back to CPU/Rayon.",
                    backend, err
                );
            }
            Err(err) => return Err(err),
        }
    }

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
    let dataset = load_sequences(&conn, usize::MAX, without_sp500, !json)?;

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
    let train_targets = &dataset.targets[..split];
    let val_seqs = &dataset.sequences[split..];
    let val_targets = &dataset.targets[split..];

    eprintln!(
        "  Train: {} sequences, Val: {} sequences",
        train_seqs.len(),
        val_seqs.len()
    );

    // Initialize and train
    eprintln!(
        "\n🏗️  Training LSTM (hidden={}, seq_len={})...",
        HIDDEN_DIM, SEQ_LEN
    );
    let mut model = LstmModel::new_random(42);
    let cpu_cap = config::cpu_worker_threads();
    let requested_threads = if single_thread { Some(1) } else { threads };
    let worker_threads = requested_threads
        .map(|value| if value == 0 { 0 } else { value.min(cpu_cap) })
        .unwrap_or(cpu_cap);
    if worker_threads == 0 {
        anyhow::bail!("LSTM thread count must be greater than zero");
    }
    eprintln!(
        "  CPU cap: {} worker threads ({}% of {} logical CPUs)",
        worker_threads,
        config::runtime_resources().cpu_budget_percent,
        config::runtime_resources().cpu_total_threads
    );
    let batch_size = config::lstm_batch_size();
    let losses = rayon::ThreadPoolBuilder::new()
        .num_threads(worker_threads)
        .build()?
        .install(|| model.train_on_data(train_seqs, train_targets, 10, 0.001, batch_size, !json));

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
    let val_ic = spearman_corr(&val_preds, val_targets);
    let val_mse: f64 = val_preds
        .iter()
        .zip(val_targets)
        .map(|(p, t)| (p - t) * (p - t))
        .sum::<f64>()
        / val_targets.len() as f64;

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
            "model_path": model_path.display().to_string(),
            "train_samples": train_seqs.len(),
            "val_samples": val_seqs.len(),
            "final_loss": losses.last().unwrap_or(&0.0),
            "val_mse": val_mse,
            "val_ic": val_ic,
            "hidden_dim": HIDDEN_DIM,
            "seq_len": SEQ_LEN,
            "epochs": losses.len(),
            "cpu_threads": worker_threads,
        }))?,
    )?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "status": "done",
                "variant": if without_sp500 { "without_sp500" } else { "with_sp500" },
                "model_path": model_path.display().to_string(),
                "train_samples": train_seqs.len(),
                "val_samples": val_seqs.len(),
                "final_loss": losses.last().unwrap_or(&0.0),
                "val_mse": val_mse,
                "val_ic": val_ic,
                "hidden_dim": HIDDEN_DIM,
                "seq_len": SEQ_LEN,
                "epochs": losses.len(),
                "cpu_threads": worker_threads,
            })
        );
    } else {
        println!("🧠 LSTM Training Complete");
        println!("{}", "─".repeat(40));
        println!("  Architecture:  LSTM({} → {} → 1)", INPUT_DIM, HIDDEN_DIM);
        println!("  Seq length:    {}", SEQ_LEN);
        println!("  Train samples: {}", train_seqs.len());
        println!("  Val samples:   {}", val_seqs.len());
        println!("  Final loss:    {:.6}", losses.last().unwrap_or(&0.0));
        println!("  Val MSE:       {:.6}", val_mse);
        println!("  Val IC:        {:.4}", val_ic);
        println!("  CPU threads:   {}", worker_threads);
        println!("  Model:         {}", model_path.display());
    }

    Ok(())
}

// Handles the ml lstm train accelerated CLI action.
fn cmd_ml_lstm_train_accelerated(
    json: bool,
    without_sp500: bool,
    backend: LstmBackend,
) -> anyhow::Result<()> {
    let _ = (json, without_sp500);
    match backend {
        LstmBackend::Mlx => {
            #[cfg(all(feature = "mlx-lstm", target_os = "macos", target_arch = "aarch64"))]
            {
                cmd_ml_lstm_train_mlx(json, without_sp500)
            }
            #[cfg(not(all(feature = "mlx-lstm", target_os = "macos", target_arch = "aarch64")))]
            {
                anyhow::bail!("MLX backend is not available in this build.")
            }
        }
        LstmBackend::Tch => {
            #[cfg(all(feature = "tch-lstm", target_os = "linux"))]
            {
                let cuda_available = tch::Cuda::is_available();
                anyhow::bail!(
                    "tch backend is linked (CUDA available: {}), but LSTM training is not implemented yet. CPU/Rayon remains the working backend.",
                    cuda_available
                )
            }
            #[cfg(not(all(feature = "tch-lstm", target_os = "linux")))]
            {
                anyhow::bail!("tch backend is not available in this build.")
            }
        }
        LstmBackend::Auto | LstmBackend::Cpu => unreachable!("accelerated backend expected"),
    }
}

#[cfg(all(feature = "mlx-lstm", target_os = "macos", target_arch = "aarch64"))]
#[derive(Debug, mlx_macros::ModuleParameters)]
#[module(root = mlx_rs)]
struct MlxReturnLstm {
    #[param]
    lstm: mlx_rs::nn::Lstm,
    #[param]
    output: mlx_rs::nn::Linear,
}

#[cfg(all(feature = "mlx-lstm", target_os = "macos", target_arch = "aarch64"))]
impl MlxReturnLstm {
    // Constructs a new instance with the provided inputs.
    fn new() -> anyhow::Result<Self> {
        Ok(Self {
            lstm: mlx_rs::nn::Lstm::new(INPUT_DIM as i32, HIDDEN_DIM as i32)?,
            output: mlx_rs::nn::Linear::new(HIDDEN_DIM as i32, 1)?,
        })
    }
}

#[cfg(all(feature = "mlx-lstm", target_os = "macos", target_arch = "aarch64"))]
impl mlx_rs::module::Module<&mlx_rs::Array> for MlxReturnLstm {
    type Error = mlx_rs::error::Exception;
    type Output = mlx_rs::Array;

    // Handles forward logic.
    fn forward(&mut self, x: &mlx_rs::Array) -> Result<Self::Output, Self::Error> {
        use mlx_rs::ops::indexing::{Ellipsis, IndexOp};

        let (hidden, _cell) = self.lstm.forward(x)?;
        let last_hidden = hidden.index((Ellipsis, (SEQ_LEN as i32) - 1, 0..));
        self.output.forward(&last_hidden)?.squeeze_axes(&[-1])
    }

    // Handles training mode logic.
    fn training_mode(&mut self, mode: bool) {
        <mlx_rs::nn::Lstm as mlx_rs::module::Module<&mlx_rs::Array>>::training_mode(
            &mut self.lstm,
            mode,
        );
        <mlx_rs::nn::Linear as mlx_rs::module::Module<&mlx_rs::Array>>::training_mode(
            &mut self.output,
            mode,
        );
    }
}

#[cfg(all(feature = "mlx-lstm", target_os = "macos", target_arch = "aarch64"))]
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

#[cfg(all(feature = "mlx-lstm", target_os = "macos", target_arch = "aarch64"))]
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

#[cfg(all(feature = "mlx-lstm", target_os = "macos", target_arch = "aarch64"))]
// Handles MLX model to cpu model acceleration support.
fn mlx_model_to_cpu_model(model: &MlxReturnLstm) -> anyhow::Result<LstmModel> {
    let wx = model.lstm.wx.value.as_slice::<f32>();
    let wh = model.lstm.wh.value.as_slice::<f32>();
    let bias_array = model
        .lstm
        .bias
        .value
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("MLX LSTM model missing bias"))?;
    let bias = bias_array.as_slice::<f32>();
    let output_weight = model.output.weight.value.as_slice::<f32>();
    let output_bias_array = model
        .output
        .bias
        .value
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("MLX output layer missing bias"))?;
    let output_bias = output_bias_array.as_slice::<f32>();

    // Handles gate weight logic.
    fn gate_weight(wx: &[f32], wh: &[f32], gate: usize) -> Vec<f64> {
        let mut out = Vec::with_capacity(HIDDEN_DIM * GATE_DIM);
        for h in 0..HIDDEN_DIM {
            let row = gate * HIDDEN_DIM + h;
            out.extend((0..HIDDEN_DIM).map(|col| wh[row * HIDDEN_DIM + col] as f64));
            out.extend((0..INPUT_DIM).map(|col| wx[row * INPUT_DIM + col] as f64));
        }
        out
    }

    // Handles gate bias logic.
    fn gate_bias(bias: &[f32], gate: usize) -> Vec<f64> {
        let start = gate * HIDDEN_DIM;
        bias[start..start + HIDDEN_DIM]
            .iter()
            .map(|value| *value as f64)
            .collect()
    }

    Ok(LstmModel {
        w_i: gate_weight(wx, wh, 0),
        b_i: gate_bias(bias, 0),
        w_f: gate_weight(wx, wh, 1),
        b_f: gate_bias(bias, 1),
        w_c: gate_weight(wx, wh, 2),
        b_c: gate_bias(bias, 2),
        w_o: gate_weight(wx, wh, 3),
        b_o: gate_bias(bias, 3),
        w_out: output_weight.iter().map(|value| *value as f64).collect(),
        b_out: output_bias.first().copied().unwrap_or(0.0) as f64,
    })
}

#[cfg(all(feature = "mlx-lstm", target_os = "macos", target_arch = "aarch64"))]
// Handles the ml lstm train mlx CLI action.
fn cmd_ml_lstm_train_mlx(json: bool, without_sp500: bool) -> anyhow::Result<()> {
    use mlx_rs::module::{Module, ModuleParameters, ModuleParametersExt};
    use mlx_rs::nn;
    use mlx_rs::optimizers::{Adam, Optimizer};
    use mlx_rs::transforms::eval_params;
    use mlx_rs::{ops, Device};

    Device::set_default(&Device::gpu());
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
    let dataset = load_sequences(&conn, usize::MAX, without_sp500, !json)?;

    if dataset.sequences.len() < 1000 {
        anyhow::bail!(
            "Not enough sequences ({}) for LSTM training. Need >= 1000.",
            dataset.sequences.len()
        );
    }

    let n = dataset.sequences.len();
    let split = (n as f64 * 0.8) as usize;
    let train_seqs = &dataset.sequences[..split];
    let train_targets = &dataset.targets[..split];
    let val_seqs = &dataset.sequences[split..];
    let val_targets = &dataset.targets[split..];

    eprintln!(
        "  Train: {} sequences, Val: {} sequences",
        train_seqs.len(),
        val_seqs.len()
    );

    let mut model = MlxReturnLstm::new()?;
    let mut optimizer = Adam::new(0.001);
    let loss_fn = |model: &mut MlxReturnLstm,
                   (x, y): (&mlx_rs::Array, &mlx_rs::Array)|
     -> Result<mlx_rs::Array, mlx_rs::error::Exception> {
        let pred = model.forward(x)?;
        ops::mean(&ops::square(pred.subtract(y)?)?, None)
    };
    let mut value_and_grad = nn::value_and_grad(loss_fn);
    let batch_size = config::lstm_batch_size();
    let epochs = 10usize;
    let mut rng = Rng::new(42);
    let mut indices: Vec<usize> = (0..train_seqs.len()).collect();
    let mut losses = Vec::new();

    eprintln!(
        "\n🏗️  Training MLX LSTM (hidden={}, seq_len={}, batch={})...",
        HIDDEN_DIM, SEQ_LEN, batch_size
    );
    let batches_per_epoch = indices.len().div_ceil(batch_size);
    let progress = crate::progress::bar_if(
        !json,
        (epochs * batches_per_epoch) as u64,
        "Training MLX LSTM",
    );
    for epoch in 0..epochs {
        for i in (1..indices.len()).rev() {
            let j = (rng.next_u64() as usize) % (i + 1);
            indices.swap(i, j);
        }

        let mut total_loss = 0.0f64;
        let mut total_rows = 0usize;
        for batch in indices.chunks(batch_size) {
            let (x, y) = batch_to_mlx_arrays(train_seqs, train_targets, batch);
            let (loss, gradients) = value_and_grad(&mut model, (&x, &y))?;
            optimizer.update(&mut model, gradients)?;
            eval_params(model.parameters())?;
            total_loss += loss.item::<f32>() as f64 * batch.len() as f64;
            total_rows += batch.len();
            progress.inc(1);
        }

        let avg_loss = total_loss / total_rows as f64;
        losses.push(avg_loss);
        progress.set_message(format!("epoch {}/{} loss={avg_loss:.6}", epoch + 1, epochs));
        if epoch % 2 == 0 || epoch == epochs - 1 {
            eprintln!("  Epoch {}/{}: loss={:.6}", epoch + 1, epochs, avg_loss);
        }
    }
    progress.finish_and_clear();

    eprintln!("\n📈 Validation...");
    model.training_mode(false);
    model.eval()?;
    let progress = crate::progress::spinner_if(!json, "Validating MLX LSTM");
    let val_preds = mlx_predict_batches(&mut model, val_seqs, batch_size)?;
    progress.finish_and_clear();
    let val_ic = spearman_corr(&val_preds, val_targets);
    let val_mse: f64 = val_preds
        .iter()
        .zip(val_targets)
        .map(|(p, t)| (p - t) * (p - t))
        .sum::<f64>()
        / val_targets.len() as f64;
    eprintln!("  Val MSE: {:.6}, Val IC: {:.4}", val_mse, val_ic);

    let cpu_model = mlx_model_to_cpu_model(&model)?;
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
            "train_samples": train_seqs.len(),
            "val_samples": val_seqs.len(),
            "final_loss": losses.last().unwrap_or(&0.0),
            "val_mse": val_mse,
            "val_ic": val_ic,
            "hidden_dim": HIDDEN_DIM,
            "seq_len": SEQ_LEN,
            "epochs": losses.len(),
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
                "train_samples": train_seqs.len(),
                "val_samples": val_seqs.len(),
                "final_loss": losses.last().unwrap_or(&0.0),
                "val_mse": val_mse,
                "val_ic": val_ic,
                "hidden_dim": HIDDEN_DIM,
                "seq_len": SEQ_LEN,
                "epochs": losses.len(),
            })
        );
    } else {
        println!("🧠 LSTM Training Complete");
        println!("{}", "─".repeat(40));
        println!("  Backend:       MLX");
        println!("  Device:        {}", Device::try_default()?);
        println!("  Architecture:  LSTM({} → {} → 1)", INPUT_DIM, HIDDEN_DIM);
        println!("  Seq length:    {}", SEQ_LEN);
        println!("  Train samples: {}", train_seqs.len());
        println!("  Val samples:   {}", val_seqs.len());
        println!("  Final loss:    {:.6}", losses.last().unwrap_or(&0.0));
        println!("  Val MSE:       {:.6}", val_mse);
        println!("  Val IC:        {:.4}", val_ic);
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
    let dataset = load_sequences(&conn, usize::MAX, without_sp500, !json)?;
    if dataset.sequences.len() < 1000 {
        anyhow::bail!(
            "Not enough sequences ({}) for LSTM evaluation. Need >= 1000.",
            dataset.sequences.len()
        );
    }

    let split = (dataset.sequences.len() as f64 * 0.8) as usize;
    let val_seqs = &dataset.sequences[split..];
    let val_targets = &dataset.targets[split..];
    let val_symbols = &dataset.symbols[split..];
    let val_dates = &dataset.dates[split..];

    let mut preds = Vec::with_capacity(val_seqs.len());
    let progress = crate::progress::bar_if(
        !json,
        val_seqs.len() as u64,
        "Evaluating LSTM validation set",
    );
    for seq in val_seqs {
        preds.push(model.forward(seq));
        progress.inc(1);
    }
    progress.finish_and_clear();
    let val_mse = preds
        .iter()
        .zip(val_targets)
        .map(|(p, t)| {
            let err = p - t;
            err * err
        })
        .sum::<f64>()
        / val_targets.len() as f64;
    let val_ic = spearman_corr(&preds, val_targets);
    let scored = preds
        .iter()
        .zip(val_targets)
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

    let report = serde_json::json!({
        "status": "done",
        "variant": if without_sp500 { "without_sp500" } else { "with_sp500" },
        "model_path": model_path.display().to_string(),
        "val_samples": val_targets.len(),
        "valid_mse": val_mse,
        "valid_ic_spearman": val_ic,
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
        println!("  Val MSE:     {:.6}", val_mse);
        println!("  Val IC:      {:.4}", val_ic);
        println!("  Report:      {}", report_path.display());
    }

    Ok(report)
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
    let dataset = load_sequences(&conn, usize::MAX, without_sp500, false)?;
    if dataset.sequences.len() < 1000 {
        anyhow::bail!(
            "Not enough sequences ({}) for LSTM validation scoring. Need >= 1000.",
            dataset.sequences.len()
        );
    }

    let split = (dataset.sequences.len() as f64 * 0.8) as usize;
    let mut scored = Vec::with_capacity(dataset.sequences.len().saturating_sub(split));
    for idx in split..dataset.sequences.len() {
        scored.push(crate::ml::ScoredReturn {
            symbol: dataset.symbols[idx].clone(),
            date: dataset.dates[idx].clone(),
            score: model.forward(&dataset.sequences[idx]),
            fwd_return: dataset.targets[idx],
        });
    }
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
