//! Synthetic cointegrated pair generator for self-tests and dry runs.

use crate::data::{Bar, PairSeries};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
    }
    fn gauss(&mut self) -> f64 {
        let u1 = self.next().max(1e-12);
        let u2 = self.next();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

/// `log B` is a random walk; `log A = alpha + beta * log B + s`, where `s` is an
/// Ornstein-Uhlenbeck process with speed `theta` and noise `sigma`. Bars have small
/// intra-bar ranges around the close path.
pub fn cointegrated_pair(
    n: usize,
    seed: u64,
    interval_ms: i64,
    beta: f64,
    theta: f64,
    sigma: f64,
) -> PairSeries {
    let mut rng = Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1);
    let mut lb = (50_000f64).ln();
    let mut s = 0.0;
    let alpha = (2_000f64).ln() - beta * lb;
    let mut out = PairSeries::default();
    let mut prev_a = (alpha + beta * lb).exp();
    let mut prev_b = lb.exp();
    for i in 0..n {
        lb += 0.002 * rng.gauss();
        s += -theta * s + sigma * rng.gauss();
        let b = lb.exp();
        let a = (alpha + beta * lb + s).exp();
        let ts = 1_700_000_000_000 + i as i64 * interval_ms;
        out.ts.push(ts);
        out.a.push(Bar {
            ts,
            open: prev_a,
            high: a.max(prev_a) * 1.0005,
            low: a.min(prev_a) * 0.9995,
            close: a,
            volume: 100.0,
        });
        out.b.push(Bar {
            ts,
            open: prev_b,
            high: b.max(prev_b) * 1.0005,
            low: b.min(prev_b) * 0.9995,
            close: b,
            volume: 10.0,
        });
        prev_a = a;
        prev_b = b;
    }
    out
}
