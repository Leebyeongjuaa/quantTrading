//! Performance metrics from an equity curve and trade list.

use serde::Serialize;

use crate::engine::{BacktestResult, ExitReason};

#[derive(Debug, Clone, Serialize)]
pub struct Metrics {
    pub bars: usize,
    pub bars_per_year: f64,
    pub start_ts: i64,
    pub end_ts: i64,
    pub initial_equity: f64,
    pub final_equity: f64,
    pub total_return_pct: f64,
    pub annualized_return_pct: f64,
    pub annualized_vol_pct: f64,
    pub sharpe: f64,
    pub sortino: f64,
    pub max_drawdown_pct: f64,
    pub calmar: f64,
    pub trades: usize,
    pub win_rate_pct: f64,
    pub profit_factor: f64,
    pub avg_trade_pnl: f64,
    pub avg_bars_held: f64,
    pub exposure_pct: f64,
    pub total_fees: f64,
    pub exits_mean_reversion: usize,
    pub exits_stop: usize,
    pub exits_max_hold: usize,
}

impl Metrics {
    pub fn from_result(r: &BacktestResult) -> Option<Metrics> {
        if r.equity.len() < 2 {
            return None;
        }
        let bars = r.equity.len();
        let bars_per_year = (365.25 * 86_400_000.0) / r.interval_ms as f64;
        let eq: Vec<f64> = r.equity.iter().map(|e| e.equity).collect();
        let rets: Vec<f64> = eq
            .windows(2)
            .map(|w| if w[0] != 0.0 { w[1] / w[0] - 1.0 } else { 0.0 })
            .collect();
        let n = rets.len() as f64;
        let mean = rets.iter().sum::<f64>() / n;
        let var = rets.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0).max(1.0);
        let sd = var.sqrt();
        let downside: Vec<f64> = rets.iter().filter(|x| **x < 0.0).map(|x| x * x).collect();
        let dsd = if downside.is_empty() {
            0.0
        } else {
            (downside.iter().sum::<f64>() / n).sqrt()
        };
        let sharpe = if sd > 0.0 {
            mean / sd * bars_per_year.sqrt()
        } else {
            0.0
        };
        let sortino = if dsd > 0.0 {
            mean / dsd * bars_per_year.sqrt()
        } else {
            0.0
        };

        let mut peak = eq[0];
        let mut mdd = 0.0f64;
        for &e in &eq {
            peak = peak.max(e);
            if peak > 0.0 {
                mdd = mdd.max((peak - e) / peak);
            }
        }
        let total_return = eq[bars - 1] / eq[0] - 1.0;
        let years = (bars as f64) / bars_per_year;
        let ann_ret = if years > 0.0 && eq[bars - 1] > 0.0 {
            (eq[bars - 1] / eq[0]).powf(1.0 / years) - 1.0
        } else {
            0.0
        };

        let trades = &r.trades;
        let wins: Vec<&_> = trades.iter().filter(|t| t.net_pnl > 0.0).collect();
        let gross_win: f64 = wins.iter().map(|t| t.net_pnl).sum();
        let gross_loss: f64 = trades
            .iter()
            .filter(|t| t.net_pnl <= 0.0)
            .map(|t| -t.net_pnl)
            .sum();
        let in_market = r.equity.iter().filter(|e| e.position != 0).count();

        Some(Metrics {
            bars,
            bars_per_year,
            start_ts: r.equity[0].ts,
            end_ts: r.equity[bars - 1].ts,
            initial_equity: eq[0],
            final_equity: eq[bars - 1],
            total_return_pct: total_return * 100.0,
            annualized_return_pct: ann_ret * 100.0,
            annualized_vol_pct: sd * bars_per_year.sqrt() * 100.0,
            sharpe,
            sortino,
            max_drawdown_pct: mdd * 100.0,
            calmar: if mdd > 0.0 { ann_ret / mdd } else { 0.0 },
            trades: trades.len(),
            win_rate_pct: if trades.is_empty() {
                0.0
            } else {
                wins.len() as f64 / trades.len() as f64 * 100.0
            },
            profit_factor: if gross_loss > 0.0 {
                gross_win / gross_loss
            } else if gross_win > 0.0 {
                f64::INFINITY
            } else {
                0.0
            },
            avg_trade_pnl: if trades.is_empty() {
                0.0
            } else {
                trades.iter().map(|t| t.net_pnl).sum::<f64>() / trades.len() as f64
            },
            avg_bars_held: if trades.is_empty() {
                0.0
            } else {
                trades.iter().map(|t| t.bars_held as f64).sum::<f64>() / trades.len() as f64
            },
            exposure_pct: in_market as f64 / bars as f64 * 100.0,
            total_fees: trades.iter().map(|t| t.fees).sum(),
            exits_mean_reversion: trades
                .iter()
                .filter(|t| t.reason == ExitReason::MeanReversion)
                .count(),
            exits_stop: trades
                .iter()
                .filter(|t| t.reason == ExitReason::Stop)
                .count(),
            exits_max_hold: trades
                .iter()
                .filter(|t| t.reason == ExitReason::MaxHold)
                .count(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::EquityPoint;

    #[test]
    fn drawdown_and_return() {
        let eq = [100.0, 110.0, 99.0, 120.0];
        let r = BacktestResult {
            trades: vec![],
            equity: eq
                .iter()
                .enumerate()
                .map(|(i, e)| EquityPoint {
                    ts: i as i64 * 600_000,
                    equity: *e,
                    zscore: 0.0,
                    spread: 0.0,
                    beta: 1.0,
                    position: 0,
                })
                .collect(),
            interval_ms: 600_000,
            initial_equity: 100.0,
        };
        let m = Metrics::from_result(&r).unwrap();
        assert!((m.total_return_pct - 20.0).abs() < 1e-9);
        assert!((m.max_drawdown_pct - 10.0).abs() < 1e-9);
        assert_eq!(m.bars_per_year as i64, 52_596);
    }
}
