//! Bar-by-bar pairs backtest with next-open fills, fees and slippage.
//!
//! Timeline for bar `t`: the signal is computed from closes up to and including `t`; any
//! resulting order fills at bar `t+1`'s open (adjusted by slippage). Equity is marked at each
//! close. This keeps the simulation free of look-ahead.

use serde::Serialize;

use crate::data::PairSeries;
use crate::stats;
use crate::strategy::{fit_hedge, spread, xform, Hedge, HedgeMode, Params, Signal};

#[derive(Debug, Clone, Serialize)]
pub struct Costs {
    /// Taker fee per side, basis points of traded notional.
    pub fee_bps: f64,
    /// Adverse slippage per fill, basis points of price.
    pub slippage_bps: f64,
    /// Notional (USD) of leg A per entry; leg B is hedged against it.
    pub notional: f64,
}

impl Default for Costs {
    fn default() -> Self {
        Costs {
            fee_bps: 2.0,
            slippage_bps: 1.0,
            notional: 10_000.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Dir {
    /// Long A / short B (spread expected to rise).
    LongSpread,
    /// Short A / long B.
    ShortSpread,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ExitReason {
    MeanReversion,
    Stop,
    MaxHold,
    EndOfData,
}

#[derive(Debug, Clone, Serialize)]
pub struct Trade {
    pub dir: Dir,
    pub entry_ts: i64,
    pub exit_ts: i64,
    pub bars_held: usize,
    pub entry_z: f64,
    pub exit_z: f64,
    pub beta: f64,
    pub qty_a: f64,
    pub qty_b: f64,
    pub entry_px_a: f64,
    pub entry_px_b: f64,
    pub exit_px_a: f64,
    pub exit_px_b: f64,
    pub gross_pnl: f64,
    pub fees: f64,
    pub net_pnl: f64,
    pub reason: ExitReason,
}

#[derive(Debug, Clone, Serialize)]
pub struct EquityPoint {
    pub ts: i64,
    pub equity: f64,
    pub zscore: f64,
    pub spread: f64,
    pub beta: f64,
    /// +1 long spread, -1 short spread, 0 flat.
    pub position: i8,
}

#[derive(Debug, Clone, Serialize)]
pub struct BacktestResult {
    pub trades: Vec<Trade>,
    pub equity: Vec<EquityPoint>,
    pub interval_ms: i64,
    pub initial_equity: f64,
}

#[derive(Debug, Clone)]
struct Open {
    dir: Dir,
    entry_ts: i64,
    entry_bar: usize,
    entry_z: f64,
    beta: f64,
    qty_a: f64,
    qty_b: f64,
    px_a: f64,
    px_b: f64,
    fees: f64,
}

/// Pending order decided at bar t, to be filled at bar t+1 open.
#[derive(Debug, Clone, Copy)]
enum Pending {
    Enter(Dir, f64 /*z*/, Hedge),
    Exit(ExitReason, f64 /*z*/),
}

pub struct Backtest {
    pub params: Params,
    pub costs: Costs,
}

impl Backtest {
    pub fn new(params: Params, costs: Costs) -> Self {
        Backtest { params, costs }
    }

    fn slip(&self, px: f64, buying: bool) -> f64 {
        let s = px * self.costs.slippage_bps / 10_000.0;
        if buying {
            px + s
        } else {
            px - s
        }
    }

    fn fee(&self, notional: f64) -> f64 {
        notional.abs() * self.costs.fee_bps / 10_000.0
    }

    /// Leg sizes for `dir` given prices and hedge. Leg A notional is fixed; leg B is sized so
    /// the pair is hedged per the regression (dollar-neutral × beta for log prices, unit × beta
    /// for raw prices).
    fn sizes(&self, dir: Dir, px_a: f64, px_b: f64, beta: f64) -> (f64, f64) {
        let qty_a = self.costs.notional / px_a;
        let qty_b = if self.params.log_prices {
            self.costs.notional * beta / px_b
        } else {
            qty_a * beta
        };
        match dir {
            Dir::LongSpread => (qty_a, -qty_b),
            Dir::ShortSpread => (-qty_a, qty_b),
        }
    }

    pub fn run(&self, data: &PairSeries) -> BacktestResult {
        let p = &self.params;
        let n = data.len();
        let interval_ms = data.interval_ms().unwrap_or(600_000);
        let initial_equity = self.costs.notional * 2.0;
        let mut result = BacktestResult {
            trades: Vec::new(),
            equity: Vec::with_capacity(n),
            interval_ms,
            initial_equity,
        };
        if n < p.lookback + 2 {
            return result;
        }

        let a: Vec<f64> = data
            .a
            .iter()
            .map(|b| xform(b.close, p.log_prices))
            .collect();
        let b: Vec<f64> = data
            .b
            .iter()
            .map(|b| xform(b.close, p.log_prices))
            .collect();

        // Static hedge: fit on the training window once; trading starts after it.
        let static_hedge = match p.hedge {
            HedgeMode::Static => {
                let t = p.train_bars.max(p.lookback).min(n - 1);
                fit_hedge(&a[..t], &b[..t])
            }
            HedgeMode::Rolling => None,
        };
        let first_tradable = match p.hedge {
            HedgeMode::Static => p.train_bars.max(p.lookback),
            HedgeMode::Rolling => p.lookback,
        };

        let mut realized = 0.0;
        let mut open: Option<Open> = None;
        let mut pending: Option<Pending> = None;
        let mut spreads: Vec<f64> = vec![f64::NAN; n];

        for t in 0..n {
            let bar_a = data.a[t];
            let bar_b = data.b[t];

            // 1. Fill whatever was decided on the previous bar at this bar's open.
            if let Some(pd) = pending.take() {
                match pd {
                    Pending::Enter(dir, z, h) if open.is_none() => {
                        let (qa, qb) = self.sizes(dir, bar_a.open, bar_b.open, h.beta);
                        let px_a = self.slip(bar_a.open, qa > 0.0);
                        let px_b = self.slip(bar_b.open, qb > 0.0);
                        let fees = self.fee(qa * px_a) + self.fee(qb * px_b);
                        open = Some(Open {
                            dir,
                            entry_ts: bar_a.ts,
                            entry_bar: t,
                            entry_z: z,
                            beta: h.beta,
                            qty_a: qa,
                            qty_b: qb,
                            px_a,
                            px_b,
                            fees,
                        });
                    }
                    Pending::Exit(reason, z) => {
                        if let Some(o) = open.take() {
                            let px_a = self.slip(bar_a.open, o.qty_a < 0.0);
                            let px_b = self.slip(bar_b.open, o.qty_b < 0.0);
                            let gross = o.qty_a * (px_a - o.px_a) + o.qty_b * (px_b - o.px_b);
                            let fees = o.fees + self.fee(o.qty_a * px_a) + self.fee(o.qty_b * px_b);
                            realized += gross - fees;
                            result.trades.push(Trade {
                                dir: o.dir,
                                entry_ts: o.entry_ts,
                                exit_ts: bar_a.ts,
                                bars_held: t - o.entry_bar,
                                entry_z: o.entry_z,
                                exit_z: z,
                                beta: o.beta,
                                qty_a: o.qty_a,
                                qty_b: o.qty_b,
                                entry_px_a: o.px_a,
                                entry_px_b: o.px_b,
                                exit_px_a: px_a,
                                exit_px_b: px_b,
                                gross_pnl: gross,
                                fees,
                                net_pnl: gross - fees,
                                reason,
                            });
                        }
                    }
                    _ => {}
                }
            }

            // 2. Signal from closes up to t.
            let hedge = match p.hedge {
                HedgeMode::Static => static_hedge,
                HedgeMode::Rolling if t + 1 >= p.lookback => {
                    fit_hedge(&a[t + 1 - p.lookback..=t], &b[t + 1 - p.lookback..=t])
                }
                HedgeMode::Rolling => None,
            };
            // With a rolling hedge the historical spread must be recomputed under the current
            // hedge so the z-score is internally consistent.
            let sig = hedge.and_then(|h| {
                if t + 1 < p.lookback {
                    return None;
                }
                let window: Vec<f64> = match p.hedge {
                    HedgeMode::Static => {
                        spreads[t] = spread(a[t], b[t], h);
                        spreads[t + 1 - p.lookback..=t].to_vec()
                    }
                    HedgeMode::Rolling => (t + 1 - p.lookback..=t)
                        .map(|i| spread(a[i], b[i], h))
                        .collect(),
                };
                let s = *window.last().unwrap();
                let (m, sd) = (stats::mean(&window), stats::std(&window));
                if sd <= 0.0 || !sd.is_finite() {
                    return None;
                }
                Some(Signal {
                    spread: s,
                    zscore: (s - m) / sd,
                    beta: h.beta,
                    alpha: h.alpha,
                })
            });

            // 3. Decide for next bar.
            if t >= first_tradable {
                if let Some(s) = sig {
                    match &open {
                        None => {
                            let beta_ok = s.beta >= p.beta_min && s.beta <= p.beta_max;
                            if beta_ok && t + 1 < n {
                                let h = Hedge {
                                    alpha: s.alpha,
                                    beta: s.beta,
                                };
                                if s.zscore <= -p.entry_z {
                                    pending = Some(Pending::Enter(Dir::LongSpread, s.zscore, h));
                                } else if s.zscore >= p.entry_z {
                                    pending = Some(Pending::Enter(Dir::ShortSpread, s.zscore, h));
                                }
                            }
                        }
                        Some(o) => {
                            let held = t - o.entry_bar;
                            let reverted = match o.dir {
                                Dir::LongSpread => s.zscore >= -p.exit_z,
                                Dir::ShortSpread => s.zscore <= p.exit_z,
                            };
                            let stopped = p.stop_z > 0.0
                                && s.zscore.abs() >= p.stop_z
                                && match o.dir {
                                    Dir::LongSpread => s.zscore < 0.0,
                                    Dir::ShortSpread => s.zscore > 0.0,
                                };
                            let reason = if stopped {
                                Some(ExitReason::Stop)
                            } else if reverted {
                                Some(ExitReason::MeanReversion)
                            } else if p.max_hold_bars > 0 && held >= p.max_hold_bars {
                                Some(ExitReason::MaxHold)
                            } else {
                                None
                            };
                            if let Some(r) = reason {
                                pending = Some(Pending::Exit(r, s.zscore));
                            }
                        }
                    }
                }
            }

            // 4. Mark to market at close.
            let unrealized = open
                .as_ref()
                .map(|o| {
                    o.qty_a * (bar_a.close - o.px_a) + o.qty_b * (bar_b.close - o.px_b) - o.fees
                })
                .unwrap_or(0.0);
            result.equity.push(EquityPoint {
                ts: bar_a.ts,
                equity: initial_equity + realized + unrealized,
                zscore: sig.map(|s| s.zscore).unwrap_or(f64::NAN),
                spread: sig.map(|s| s.spread).unwrap_or(f64::NAN),
                beta: sig.map(|s| s.beta).unwrap_or(f64::NAN),
                position: match &open {
                    Some(o) if o.dir == Dir::LongSpread => 1,
                    Some(_) => -1,
                    None => 0,
                },
            });
        }

        // Close anything still open at the last close.
        if let Some(o) = open.take() {
            let last_a = data.a[n - 1];
            let last_b = data.b[n - 1];
            let px_a = self.slip(last_a.close, o.qty_a < 0.0);
            let px_b = self.slip(last_b.close, o.qty_b < 0.0);
            let gross = o.qty_a * (px_a - o.px_a) + o.qty_b * (px_b - o.px_b);
            let fees = o.fees + self.fee(o.qty_a * px_a) + self.fee(o.qty_b * px_b);
            realized += gross - fees;
            if let Some(last) = result.equity.last_mut() {
                last.equity = initial_equity + realized;
                last.position = 0;
            }
            result.trades.push(Trade {
                dir: o.dir,
                entry_ts: o.entry_ts,
                exit_ts: last_a.ts,
                bars_held: n - 1 - o.entry_bar,
                entry_z: o.entry_z,
                exit_z: result.equity.last().map(|e| e.zscore).unwrap_or(f64::NAN),
                beta: o.beta,
                qty_a: o.qty_a,
                qty_b: o.qty_b,
                entry_px_a: o.px_a,
                entry_px_b: o.px_b,
                exit_px_a: px_a,
                exit_px_b: px_b,
                gross_pnl: gross,
                fees,
                net_pnl: gross - fees,
                reason: ExitReason::EndOfData,
            });
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic;

    fn params() -> Params {
        Params {
            lookback: 60,
            train_bars: 300,
            hedge: HedgeMode::Static,
            log_prices: true,
            entry_z: 2.0,
            exit_z: 0.3,
            stop_z: 5.0,
            max_hold_bars: 200,
            beta_min: 0.1,
            beta_max: 10.0,
        }
    }

    #[test]
    fn profitable_on_cointegrated_pair_without_costs() {
        let data = synthetic::cointegrated_pair(4000, 42, 600_000, 1.2, 0.05, 0.01);
        let bt = Backtest::new(
            params(),
            Costs {
                fee_bps: 0.0,
                slippage_bps: 0.0,
                notional: 10_000.0,
            },
        );
        let r = bt.run(&data);
        assert!(r.trades.len() > 20, "trades={}", r.trades.len());
        let final_eq = r.equity.last().unwrap().equity;
        assert!(
            final_eq > r.initial_equity,
            "final {final_eq} <= initial {}",
            r.initial_equity
        );
        let wins = r.trades.iter().filter(|t| t.net_pnl > 0.0).count();
        assert!(wins as f64 / r.trades.len() as f64 > 0.6);
    }

    #[test]
    fn costs_reduce_pnl_and_stops_fire() {
        let data = synthetic::cointegrated_pair(4000, 42, 600_000, 1.2, 0.05, 0.01);
        let free = Backtest::new(
            params(),
            Costs {
                fee_bps: 0.0,
                slippage_bps: 0.0,
                notional: 10_000.0,
            },
        )
        .run(&data);
        let paid = Backtest::new(
            params(),
            Costs {
                fee_bps: 5.0,
                slippage_bps: 2.0,
                notional: 10_000.0,
            },
        )
        .run(&data);
        assert!(paid.equity.last().unwrap().equity < free.equity.last().unwrap().equity);
        assert!(paid.trades.iter().all(|t| t.fees > 0.0));
    }

    #[test]
    fn no_trades_before_training_window_and_fills_next_open() {
        let data = synthetic::cointegrated_pair(1000, 7, 600_000, 1.0, 0.1, 0.02);
        let r = Backtest::new(params(), Costs::default()).run(&data);
        for t in &r.trades {
            let idx = data.ts.iter().position(|ts| *ts == t.entry_ts).unwrap();
            assert!(idx > 300, "entered during training at bar {idx}");
            // Entry fill price is the bar open (± slippage), never the signal bar's close.
            let bar = data.a[idx];
            assert!((t.entry_px_a - bar.open).abs() / bar.open < 0.001);
            assert!(t.exit_ts > t.entry_ts);
        }
        assert_eq!(r.equity.len(), data.len());
    }

    #[test]
    fn rolling_hedge_runs_and_closes_at_end() {
        let data = synthetic::cointegrated_pair(1500, 3, 600_000, 0.8, 0.05, 0.01);
        let mut p = params();
        p.hedge = HedgeMode::Rolling;
        p.lookback = 120;
        p.max_hold_bars = 0;
        p.stop_z = 0.0;
        let r = Backtest::new(p, Costs::default()).run(&data);
        assert!(!r.trades.is_empty());
        assert_eq!(r.equity.last().unwrap().position, 0);
        assert!(r.trades.iter().all(|t| t.beta > 0.1));
    }

    #[test]
    fn too_short_series_is_empty() {
        let data = synthetic::cointegrated_pair(50, 1, 600_000, 1.0, 0.1, 0.01);
        let r = Backtest::new(params(), Costs::default()).run(&data);
        assert!(r.trades.is_empty() && r.equity.is_empty());
    }
}
