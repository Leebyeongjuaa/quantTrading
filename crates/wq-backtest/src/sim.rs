//! Position construction and PnL simulation, following WorldQuant BRAIN semantics.
//!
//! Per bar `t`: alpha vector `a_t` → optional linear decay over `decay` bars → universe mask
//! (instruments with a valid alpha, a valid next return and, if set, top-`universe` by trailing
//! dollar volume) → neutralization (market: demean) → scale to `booksize` (sum |w| = booksize)
//! → truncation (cap |w_i| ≤ truncation · booksize, re-scaled) → weights `w_t`.
//!
//! Two turnover controls sit between the target weights and the held weights (see
//! [`gate_weights`]): a *distance gate* that lets an instrument re-trade only after its price
//! has travelled `trade_distance` (sum of |returns| since its last trade), and an *entry speed*
//! that moves the held weight only a fraction of the way toward the target per trade.
//!
//! With `delay = 1` the weights at `t` come from the alpha computed at `t-1` (data through the
//! previous close); `delay = 0` uses the alpha at `t`. Weights `w_t` are held over `(t, t+1]`
//! and earn `w_t · r_{t+1}`. Turnover at `t` is `sum |w_t - w_{t-1}| / booksize`.

use serde::{Deserialize, Serialize};

use crate::mat::Mat;
use crate::ops::{scale_row, truncate_row, ts_decay_linear};
use crate::panel::Panel;

/// What the distance gate measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
pub enum DistanceScope {
    /// One shared gate for the whole book. Independent of the number of instruments.
    Portfolio,
    /// A gate per instrument.
    Instrument,
}

/// Which distance the gate accumulates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
pub enum GateMode {
    /// Price travel: cumulative |return| since the last trade (portfolio scope uses the
    /// cross-sectional mean |return| per bar). Threshold is a return fraction (0.02 = 2%).
    Price,
    /// Signal distance: how far the target weights have drifted from the held weights.
    /// Portfolio scope: `sum|target - held| / booksize` (the turnover a rebalance would cost);
    /// instrument scope: `|target_i - held_i|` relative to the average absolute weight
    /// `booksize / n`. Threshold is that fraction (0.5 = rebalance once half the book is stale).
    Signal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
pub enum Neutralization {
    None,
    /// Subtract the cross-sectional mean so longs equal shorts in dollars.
    Market,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// 0 or 1 bars between the alpha's last data point and the position.
    pub delay: usize,
    /// Linear decay window in bars (0/1 = none).
    pub decay: usize,
    pub neutralization: Neutralization,
    /// Max |weight| as a fraction of booksize (0 or >=1 disables). BRAIN default 0.1.
    pub truncation: f64,
    /// Gross dollar book: sum |w| each bar.
    pub booksize: f64,
    /// Instruments per bar, ranked by trailing dollar volume (0 = all).
    pub universe: usize,
    /// Trailing window (bars) for the liquidity ranking.
    pub universe_window: usize,
    /// Transaction cost per dollar traded, basis points (0 = BRAIN's cost-free PnL).
    pub cost_bps: f64,
    /// Distance gate: an instrument's weight is only updated once the sum of |returns| since
    /// its last trade reaches this fraction (0.02 = 2%). 0 disables (trade every bar).
    pub trade_distance: f64,
    /// Entry speed: when a trade is allowed, move this fraction of the way from the current
    /// weight to the target (1 = jump to target, 0.25 = ease in over ~4 trades).
    pub entry_speed: f64,
    /// While the distance gate stays closed, shrink the held weight toward zero by this
    /// fraction per bar (0 = hold the position unchanged, 0.1 = fade out over ~10 bars).
    pub exit_speed: f64,
    pub distance_scope: DistanceScope,
    pub gate_mode: GateMode,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            delay: 1,
            decay: 0,
            neutralization: Neutralization::Market,
            truncation: 0.1,
            booksize: 1_000_000.0,
            universe: 0,
            universe_window: 20,
            cost_bps: 0.0,
            trade_distance: 0.0,
            entry_speed: 1.0,
            exit_speed: 0.0,
            distance_scope: DistanceScope::Portfolio,
            gate_mode: GateMode::Price,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SimResult {
    pub ts: Vec<i64>,
    /// PnL earned over `(t-1, t]` from weights set at `t-1`, after costs.
    pub pnl: Vec<f64>,
    pub gross_pnl: Vec<f64>,
    pub costs: Vec<f64>,
    /// `sum |w_t - w_{t-1}| / booksize`.
    pub turnover: Vec<f64>,
    pub long_count: Vec<usize>,
    pub short_count: Vec<usize>,
    pub long_notional: Vec<f64>,
    pub short_notional: Vec<f64>,
    /// Dollars traded at `t`.
    pub traded: Vec<f64>,
    /// Spearman IC between the raw alpha used at `t` and `r_{t+1}`.
    pub ic: Vec<f64>,
    pub weights: Mat,
    pub settings: Settings,
    pub booksize: f64,
}

/// Builds the weight row from an alpha row (in place): mask → neutralize → scale → truncate.
pub fn weights_from_alpha(alpha: &mut [f64], mask: &[bool], s: &Settings) {
    for (a, m) in alpha.iter_mut().zip(mask) {
        if !*m {
            *a = f64::NAN;
        }
    }
    if s.neutralization == Neutralization::Market {
        let v: Vec<f64> = alpha.iter().copied().filter(|x| x.is_finite()).collect();
        if !v.is_empty() {
            let mu = v.iter().sum::<f64>() / v.len() as f64;
            for a in alpha.iter_mut() {
                if a.is_finite() {
                    *a -= mu;
                }
            }
        }
    }
    scale_row(alpha, s.booksize);
    if s.truncation > 0.0 && s.truncation < 1.0 {
        truncate_row(alpha, s.truncation);
    }
    for a in alpha.iter_mut() {
        if !a.is_finite() {
            *a = 0.0;
        }
    }
}

/// Accumulated price distance since the last trade: per instrument plus one shared value.
#[derive(Debug, Clone)]
pub struct GateState {
    pub per: Vec<f64>,
    pub all: f64,
}

impl GateState {
    pub fn new(n: usize) -> Self {
        GateState {
            per: vec![0.0; n],
            all: 0.0,
        }
    }
}

/// Turns target weights into held weights for one bar.
///
/// The gate decides whether a trade is allowed (see [`GateMode`] and [`DistanceScope`]).
/// When it is open the weight moves `entry_speed` of the way from `prev` to `target` (and a
/// price-distance gate resets); when it is closed the weight holds, or fades toward zero by
/// `exit_speed` per bar. With `trade_distance == 0` the gate is always open. Instruments
/// outside the universe (`mask[i] == false`) are always flattened.
pub fn gate_weights(
    target: &[f64],
    prev: &[f64],
    mask: &[bool],
    ret_now: &[f64],
    state: &mut GateState,
    s: &Settings,
) -> Vec<f64> {
    let speed = if s.entry_speed <= 0.0 || s.entry_speed > 1.0 {
        1.0
    } else {
        s.entry_speed
    };
    let fade = s.exit_speed.clamp(0.0, 1.0);
    let n = target.len();
    let portfolio = s.distance_scope == DistanceScope::Portfolio;
    let always = s.trade_distance <= 0.0;

    // Price distances: per-instrument |ret| and the universe mean |ret|.
    let (mut sum, mut cnt) = (0.0, 0usize);
    for i in 0..n {
        if ret_now[i].is_finite() {
            state.per[i] += ret_now[i].abs();
            if mask[i] {
                sum += ret_now[i].abs();
                cnt += 1;
            }
        }
    }
    if cnt > 0 {
        state.all += sum / cnt as f64;
    }
    // Signal distance: pending turnover of the whole book, and the per-name scale.
    let n_mask = mask.iter().filter(|m| **m).count().max(1);
    let pending: f64 = (0..n)
        .filter(|&i| mask[i])
        .map(|i| (target[i] - prev[i]).abs())
        .sum::<f64>()
        / s.booksize;
    let per_scale = s.booksize / n_mask as f64;

    let book_open = always
        || match s.gate_mode {
            GateMode::Price => state.all >= s.trade_distance,
            GateMode::Signal => pending >= s.trade_distance,
        };

    let mut out = Vec::with_capacity(n);
    let mut traded_any = false;
    for i in 0..n {
        if !mask[i] {
            out.push(0.0);
            state.per[i] = 0.0;
            continue;
        }
        let open = if portfolio {
            book_open
        } else {
            always
                || match s.gate_mode {
                    GateMode::Price => state.per[i] >= s.trade_distance || prev[i] == 0.0,
                    GateMode::Signal => (target[i] - prev[i]).abs() >= s.trade_distance * per_scale,
                }
        };
        if open {
            let w = prev[i] + speed * (target[i] - prev[i]);
            if w != prev[i] {
                state.per[i] = 0.0;
                traded_any = true;
            }
            out.push(w);
        } else {
            out.push(prev[i] * (1.0 - fade));
        }
    }
    if portfolio && book_open && traded_any {
        state.all = 0.0;
    }
    out
}

fn spearman(a: &[f64], b: &[f64]) -> f64 {
    let mut ra: Vec<f64> = Vec::new();
    let mut rb: Vec<f64> = Vec::new();
    for (x, y) in a.iter().zip(b) {
        if x.is_finite() && y.is_finite() {
            ra.push(*x);
            rb.push(*y);
        }
    }
    if ra.len() < 3 {
        return f64::NAN;
    }
    crate::ops::rank_row(&mut ra);
    crate::ops::rank_row(&mut rb);
    let n = ra.len() as f64;
    let (ma, mb) = (ra.iter().sum::<f64>() / n, rb.iter().sum::<f64>() / n);
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for (x, y) in ra.iter().zip(&rb) {
        sxy += (x - ma) * (y - mb);
        sxx += (x - ma).powi(2);
        syy += (y - mb).powi(2);
    }
    if sxx > 0.0 && syy > 0.0 {
        sxy / (sxx * syy).sqrt()
    } else {
        f64::NAN
    }
}

pub fn simulate(panel: &Panel, alpha: &Mat, s: &Settings) -> SimResult {
    let (t, n) = (panel.t(), panel.n());
    assert_eq!((alpha.t, alpha.n), (t, n), "alpha shape must match panel");
    let decayed = if s.decay > 1 {
        ts_decay_linear(alpha, s.decay)
    } else {
        alpha.clone()
    };
    let liquidity = if s.universe > 0 {
        Some(
            crate::ops::evaluate(
                panel,
                &crate::expr::Expr::Call {
                    name: "ts_mean".into(),
                    args: vec![
                        crate::expr::Expr::Ident("dollar_volume".into()),
                        crate::expr::Expr::Num(s.universe_window as f64),
                    ],
                    kwargs: vec![],
                },
            )
            .expect("ts_mean"),
        )
    } else {
        None
    };

    let mut weights = Mat::filled(t, n, 0.0);
    let mut res = SimResult {
        ts: panel.ts.clone(),
        pnl: vec![0.0; t],
        gross_pnl: vec![0.0; t],
        costs: vec![0.0; t],
        turnover: vec![0.0; t],
        long_count: vec![0; t],
        short_count: vec![0; t],
        long_notional: vec![0.0; t],
        short_notional: vec![0.0; t],
        traded: vec![0.0; t],
        ic: vec![f64::NAN; t],
        weights: Mat::filled(0, 0, 0.0),
        settings: s.clone(),
        booksize: s.booksize,
    };
    let mut prev: Vec<f64> = vec![0.0; n];
    let mut gate = GateState::new(n);

    for ti in 0..t {
        // Alpha row that drives the position held over (ti, ti+1].
        let src = match ti.checked_sub(s.delay) {
            Some(src) => src,
            None => continue,
        };
        let mut row: Vec<f64> = decayed.row(src).to_vec();
        // Universe: needs a tradeable next return (we must be able to hold the instrument).
        let next_ret: Vec<f64> = if ti + 1 < t {
            panel.returns.row(ti + 1).to_vec()
        } else {
            vec![f64::NAN; n]
        };
        let mut mask: Vec<bool> = (0..n)
            .map(|i| row[i].is_finite() && panel.close.get(ti, i).is_finite())
            .collect();
        if let Some(liq) = &liquidity {
            let mut ranked: Vec<(usize, f64)> = (0..n)
                .filter(|&i| mask[i] && liq.get(src, i).is_finite())
                .map(|i| (i, liq.get(src, i)))
                .collect();
            ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            let keep: std::collections::HashSet<usize> =
                ranked.iter().take(s.universe).map(|x| x.0).collect();
            for (i, m) in mask.iter_mut().enumerate() {
                *m = keep.contains(&i);
            }
        }
        res.ic[ti] = spearman(
            &row.iter()
                .zip(&mask)
                .map(|(v, m)| if *m { *v } else { f64::NAN })
                .collect::<Vec<_>>(),
            &next_ret,
        );
        weights_from_alpha(&mut row, &mask, s);
        let row = if s.trade_distance > 0.0 || s.entry_speed < 1.0 {
            gate_weights(&row, &prev, &mask, panel.returns.row(ti), &mut gate, s)
        } else {
            row
        };

        let traded: f64 = row.iter().zip(&prev).map(|(w, p)| (w - p).abs()).sum();
        res.traded[ti] = traded;
        res.turnover[ti] = traded / s.booksize;
        res.costs[ti] = traded * s.cost_bps / 10_000.0;
        res.long_count[ti] = row.iter().filter(|w| **w > 0.0).count();
        res.short_count[ti] = row.iter().filter(|w| **w < 0.0).count();
        res.long_notional[ti] = row.iter().filter(|w| **w > 0.0).sum();
        res.short_notional[ti] = row.iter().filter(|w| **w < 0.0).map(|w| -w).sum();
        // Costs are paid when trading at ti; PnL from these weights lands at ti+1.
        res.pnl[ti] -= res.costs[ti];
        if ti + 1 < t {
            let gross: f64 = row
                .iter()
                .zip(&next_ret)
                .map(|(w, r)| if r.is_finite() { w * r } else { 0.0 })
                .sum();
            res.gross_pnl[ti + 1] += gross;
            res.pnl[ti + 1] += gross;
        }
        weights.row_mut(ti).copy_from_slice(&row);
        prev = row;
    }
    res.weights = weights;
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::parse;
    use crate::ops::evaluate;
    use crate::panel::tests::bars;

    fn panel() -> Panel {
        Panel::from_series(vec![
            (
                "A".into(),
                bars(0, &[10.0, 11.0, 12.0, 11.0, 13.0, 14.0, 13.0, 15.0]),
            ),
            (
                "B".into(),
                bars(0, &[50.0, 49.0, 48.0, 50.0, 47.0, 46.0, 48.0, 45.0]),
            ),
            (
                "C".into(),
                bars(0, &[5.0, 5.0, 5.5, 6.0, 6.0, 7.0, 6.5, 7.0]),
            ),
            (
                "D".into(),
                bars(0, &[1.0, 1.1, 1.0, 1.2, 1.1, 1.3, 1.2, 1.4]),
            ),
        ])
        .unwrap()
    }

    fn settings() -> Settings {
        Settings {
            delay: 0,
            decay: 0,
            neutralization: Neutralization::Market,
            truncation: 0.0,
            booksize: 100.0,
            universe: 0,
            universe_window: 2,
            cost_bps: 0.0,
            trade_distance: 0.0,
            entry_speed: 1.0,
            exit_speed: 0.0,
            distance_scope: DistanceScope::Instrument,
            gate_mode: GateMode::Price,
        }
    }

    #[test]
    fn portfolio_gate_uses_mean_abs_return() {
        let mut s = settings();
        s.distance_scope = DistanceScope::Portfolio;
        s.trade_distance = 0.02;
        let mask = [true, true, true, true];
        let mut g = GateState::new(4);
        let tgt = [25.0, -25.0, 25.0, -25.0];
        // Mean |ret| = 1% < 2%: nothing trades yet, even from flat.
        let w1 = gate_weights(
            &tgt,
            &[0.0; 4],
            &mask,
            &[0.01, 0.01, 0.01, 0.01],
            &mut g,
            &s,
        );
        assert_eq!(w1, vec![0.0; 4]);
        assert!((g.all - 0.01).abs() < 1e-12);
        // One name moves 4%, others 0: mean 1% -> cumulative 2%: whole book trades, reset.
        let w2 = gate_weights(&tgt, &w1, &mask, &[0.04, 0.0, 0.0, 0.0], &mut g, &s);
        assert_eq!(w2, tgt.to_vec());
        assert_eq!(g.all, 0.0);
        // Doubling the universe with identical returns leaves the mean unchanged.
        let mut g8 = GateState::new(8);
        let w = gate_weights(&[10.0; 8], &[0.0; 8], &[true; 8], &[0.02; 8], &mut g8, &s);
        assert_eq!(w, vec![10.0; 8]);
    }

    #[test]
    fn signal_gate_trades_on_pending_turnover() {
        let mut s = settings();
        s.gate_mode = GateMode::Signal;
        s.distance_scope = DistanceScope::Portfolio;
        s.trade_distance = 0.5; // half the book must be stale
        let mask = [true, true];
        let mut g = GateState::new(2);
        // From flat the pending turnover is 1.0 >= 0.5: trade.
        let w1 = gate_weights(&[50.0, -50.0], &[0.0, 0.0], &mask, &[0.0, 0.0], &mut g, &s);
        assert_eq!(w1, vec![50.0, -50.0]);
        // Target drifts by 20 per name = 40% of the book: hold.
        let w2 = gate_weights(&[30.0, -70.0], &w1, &mask, &[0.0, 0.0], &mut g, &s);
        assert_eq!(w2, w1);
        // Target flips = 200% pending: trade.
        let w3 = gate_weights(&[-50.0, 50.0], &w2, &mask, &[0.0, 0.0], &mut g, &s);
        assert_eq!(w3, vec![-50.0, 50.0]);
        // Instrument scope: threshold relative to booksize / n = 50 per name.
        s.distance_scope = DistanceScope::Instrument;
        s.trade_distance = 0.5; // 25 per name
        let w4 = gate_weights(&[-30.0, 20.0], &w3, &mask, &[0.0, 0.0], &mut g, &s);
        assert_eq!(w4, vec![-50.0, 20.0]); // first moved 20 (<25): hold; second moved 30: trade
    }

    #[test]
    fn closed_gate_fades_toward_flat() {
        let mut s = settings();
        s.trade_distance = 0.05;
        s.exit_speed = 0.5;
        let mut dist = GateState::new(1);
        let w1 = gate_weights(&[80.0], &[0.0], &[true], &[0.0], &mut dist, &s);
        assert_eq!(w1, vec![80.0]);
        // Gate closed: halve toward zero each bar, distance keeps accumulating.
        let w2 = gate_weights(&[-80.0], &w1, &[true], &[0.01], &mut dist, &s);
        assert_eq!(w2, vec![40.0]);
        let w3 = gate_weights(&[-80.0], &w2, &[true], &[0.01], &mut dist, &s);
        assert_eq!(w3, vec![20.0]);
        assert!((dist.per[0] - 0.02).abs() < 1e-12);
        // Gate opens: jump to the new target.
        let w4 = gate_weights(&[-80.0], &w3, &[true], &[0.03], &mut dist, &s);
        assert_eq!(w4, vec![-80.0]);
    }

    #[test]
    fn distance_gate_holds_until_price_travels() {
        let mut s = settings();
        s.trade_distance = 0.05;
        let mask = [true, true];
        let mut dist = GateState::new(2);
        // First trade from flat always happens.
        let w1 = gate_weights(
            &[50.0, -50.0],
            &[0.0, 0.0],
            &mask,
            &[0.0, 0.0],
            &mut dist,
            &s,
        );
        assert_eq!(w1, vec![50.0, -50.0]);
        // Target flips but the price moved only 1%: hold.
        let w2 = gate_weights(&[-50.0, 50.0], &w1, &mask, &[0.01, -0.01], &mut dist, &s);
        assert_eq!(w2, w1);
        // Cumulative 5%: trade, and the distance resets.
        let w3 = gate_weights(&[-50.0, 50.0], &w2, &mask, &[0.04, 0.04], &mut dist, &s);
        assert_eq!(w3, vec![-50.0, 50.0]);
        assert_eq!(dist.per, vec![0.0, 0.0]);
        // Leaving the universe flattens regardless of the gate.
        let w4 = gate_weights(
            &[0.0, 50.0],
            &w3,
            &[false, true],
            &[0.0, 0.0],
            &mut dist,
            &s,
        );
        assert_eq!(w4[0], 0.0);
    }

    #[test]
    fn entry_speed_eases_in() {
        let mut s = settings();
        s.entry_speed = 0.5;
        let mut dist = GateState::new(1);
        let w1 = gate_weights(&[100.0], &[0.0], &[true], &[0.0], &mut dist, &s);
        assert_eq!(w1, vec![50.0]);
        let w2 = gate_weights(&[100.0], &w1, &[true], &[0.0], &mut dist, &s);
        assert_eq!(w2, vec![75.0]);
    }

    #[test]
    fn gate_reduces_turnover_and_defaults_match_baseline() {
        let p = panel();
        let a = evaluate(&p, &parse("-ts_delta(close, 1)").unwrap()).unwrap();
        let base = simulate(&p, &a, &settings());
        let mut s = settings();
        let same = simulate(&p, &a, &s);
        assert_eq!(base.pnl, same.pnl);
        s.trade_distance = 0.08;
        let gated = simulate(&p, &a, &s);
        let t0: f64 = base.turnover.iter().sum();
        let t1: f64 = gated.turnover.iter().sum();
        assert!(t1 < t0, "gate should cut turnover: {t1} vs {t0}");
        s.trade_distance = 0.0;
        s.entry_speed = 0.25;
        let slow = simulate(&p, &a, &s);
        let t2: f64 = slow.turnover.iter().sum();
        assert!(t2 < t0, "slow entry should cut turnover: {t2} vs {t0}");
    }

    #[test]
    fn weights_are_neutral_and_scaled() {
        let mut row = vec![3.0, 1.0, f64::NAN, 2.0];
        weights_from_alpha(&mut row, &[true, true, true, true], &settings());
        assert!((row.iter().sum::<f64>()).abs() < 1e-9);
        assert!((row.iter().map(|v| v.abs()).sum::<f64>() - 100.0).abs() < 1e-9);
        assert_eq!(row[2], 0.0);
        let mut s = settings();
        s.truncation = 0.3;
        let mut row = vec![10.0, 0.0, 0.0, -1.0];
        weights_from_alpha(&mut row, &[true; 4], &s);
        assert!(row.iter().all(|w| w.abs() <= 30.0 + 1e-6));
        assert!((row.iter().map(|v| v.abs()).sum::<f64>() - 100.0).abs() < 1e-6);
    }

    #[test]
    fn perfect_foresight_alpha_is_always_profitable() {
        let p = panel();
        // Alpha = next bar's return (look-ahead on purpose) with delay 0 must win every bar.
        let mut a = Mat::nan(p.t(), p.n());
        for t in 0..p.t() - 1 {
            for i in 0..p.n() {
                a.set(t, i, p.returns.get(t + 1, i));
            }
        }
        let r = simulate(&p, &a, &settings());
        for t in 1..p.t() {
            assert!(r.pnl[t] > 0.0, "bar {t} pnl {}", r.pnl[t]);
            assert!((r.ic[t - 1] - 1.0).abs() < 1e-9);
        }
        // Same alpha with delay 1 is stale by one bar and no longer perfect.
        let mut s = settings();
        s.delay = 1;
        let r1 = simulate(&p, &a, &s);
        assert!(r1.pnl.iter().sum::<f64>() < r.pnl.iter().sum::<f64>());
        assert_eq!(r1.long_count[0], 0); // nothing to trade before the first alpha
    }

    #[test]
    fn turnover_costs_and_universe() {
        let p = panel();
        let a = evaluate(&p, &parse("-ts_delta(close, 1)").unwrap()).unwrap();
        let mut s = settings();
        s.cost_bps = 10.0;
        let r = simulate(&p, &a, &s);
        // First trade from flat: turnover = 1 (full book), costs = 100 * 10bps = 0.1.
        let first = (0..p.t()).find(|&t| r.turnover[t] > 0.0).unwrap();
        assert!((r.turnover[first] - 1.0).abs() < 1e-9);
        assert!((r.costs[first] - 0.1).abs() < 1e-9);
        assert!(r.turnover.iter().all(|x| *x <= 2.0 + 1e-9));
        s.universe = 2;
        let r2 = simulate(&p, &a, &s);
        assert!(r2
            .long_count
            .iter()
            .zip(&r2.short_count)
            .all(|(l, sh)| l + sh <= 2));
        assert_eq!(r.weights.t, p.t());
    }

    #[test]
    fn decay_smooths_turnover() {
        let p = panel();
        let a = evaluate(&p, &parse("-ts_delta(close, 1)").unwrap()).unwrap();
        let s0 = settings();
        let mut s4 = settings();
        s4.decay = 3;
        let t0: f64 = simulate(&p, &a, &s0).turnover[3..].iter().sum();
        let t4: f64 = simulate(&p, &a, &s4).turnover[3..].iter().sum();
        assert!(t4 < t0, "decay should lower turnover: {t4} vs {t0}");
    }
}
