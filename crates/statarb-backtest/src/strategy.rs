//! Spread construction and signal parameters.

use serde::{Deserialize, Serialize};

use crate::stats;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, clap::ValueEnum)]
pub enum HedgeMode {
    /// Fit `alpha, beta` once on the first `train_bars` bars, then hold them fixed
    /// ("static arb": the hedge ratio does not drift with the window).
    Static,
    /// Re-fit `alpha, beta` on each bar over the trailing `lookback` window.
    Rolling,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Params {
    /// Window (bars) for spread mean/std, and for the rolling hedge regression.
    pub lookback: usize,
    /// Bars used to fit the static hedge ratio (ignored for `Rolling`).
    pub train_bars: usize,
    pub hedge: HedgeMode,
    /// Regress log prices (dollar-neutral hedge) instead of raw prices (unit hedge).
    pub log_prices: bool,
    pub entry_z: f64,
    pub exit_z: f64,
    /// Hard stop when |z| exceeds this. 0 disables.
    pub stop_z: f64,
    /// Force exit after this many bars in a position. 0 disables.
    pub max_hold_bars: usize,
    /// Skip entries when the fitted beta is outside `[beta_min, beta_max]`.
    pub beta_min: f64,
    pub beta_max: f64,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            lookback: 144,    // 1 day of 10-minute bars
            train_bars: 1008, // 7 days of 10-minute bars
            hedge: HedgeMode::Static,
            log_prices: true,
            entry_z: 2.0,
            exit_z: 0.5,
            stop_z: 4.0,
            max_hold_bars: 288,
            beta_min: 0.1,
            beta_max: 10.0,
        }
    }
}

/// Hedge ratio and intercept.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hedge {
    pub alpha: f64,
    pub beta: f64,
}

/// Transforms a price for regression (log or raw).
pub fn xform(price: f64, log_prices: bool) -> f64 {
    if log_prices {
        price.ln()
    } else {
        price
    }
}

/// Fits `a = alpha + beta * b` over the given (already transformed) windows.
pub fn fit_hedge(a: &[f64], b: &[f64]) -> Option<Hedge> {
    stats::ols(a, b).map(|(alpha, beta)| Hedge { alpha, beta })
}

/// `spread = a - alpha - beta * b` in transformed space.
pub fn spread(a: f64, b: f64, h: Hedge) -> f64 {
    a - h.alpha - h.beta * b
}

/// Per-bar diagnostics the engine computes before deciding.
#[derive(Debug, Clone, Copy, Default)]
pub struct Signal {
    pub spread: f64,
    pub zscore: f64,
    pub beta: f64,
    pub alpha: f64,
}

/// Summary statistics of a pair over a full sample: used by `analyze`.
#[derive(Debug, Clone, Serialize)]
pub struct PairDiagnostics {
    pub n: usize,
    pub alpha: f64,
    pub beta: f64,
    pub correlation: f64,
    pub adf_t: Option<f64>,
    pub adf_pass_5pct: bool,
    pub half_life_bars: Option<f64>,
    pub spread_std: f64,
}

pub fn diagnose(a_px: &[f64], b_px: &[f64], log_prices: bool) -> Option<PairDiagnostics> {
    let a: Vec<f64> = a_px.iter().map(|p| xform(*p, log_prices)).collect();
    let b: Vec<f64> = b_px.iter().map(|p| xform(*p, log_prices)).collect();
    let h = fit_hedge(&a, &b)?;
    let s: Vec<f64> = a.iter().zip(&b).map(|(x, y)| spread(*x, *y, h)).collect();
    let adf_t = stats::adf_tstat(&s, 1);
    Some(PairDiagnostics {
        n: s.len(),
        alpha: h.alpha,
        beta: h.beta,
        correlation: stats::pearson(&a, &b),
        adf_t,
        adf_pass_5pct: adf_t.map(|t| t < stats::ADF_CRIT_5PCT).unwrap_or(false),
        half_life_bars: stats::half_life(&s),
        spread_std: stats::std(&s),
    })
}
