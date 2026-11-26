//! BRAIN-style summary statistics.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::sim::SimResult;

#[derive(Debug, Clone, Serialize)]
pub struct PeriodStats {
    pub period: String,
    pub pnl: f64,
    pub return_pct: f64,
    pub sharpe: f64,
    pub turnover: f64,
    pub bars: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Metrics {
    pub bars: usize,
    pub bars_per_year: f64,
    pub start_ts: i64,
    pub end_ts: i64,
    pub booksize: f64,
    pub total_pnl: f64,
    /// Annualized Sharpe of per-bar PnL / booksize.
    pub sharpe: f64,
    /// Annualized return as a fraction of booksize (BRAIN "returns").
    pub annual_return: f64,
    /// Mean per-bar turnover (fraction of booksize traded per bar).
    pub turnover_per_bar: f64,
    /// Turnover per day (per-bar × bars/day); the number BRAIN reports.
    pub turnover_daily: f64,
    /// `sharpe · sqrt(|annual_return| / max(turnover_daily, 0.125))`.
    pub fitness: f64,
    /// PnL per dollar traded, in basis points.
    pub margin_bps: f64,
    pub max_drawdown: f64,
    pub max_drawdown_pct: f64,
    pub mean_ic: f64,
    /// IC mean / IC std · sqrt(bars per year): information ratio of the IC.
    pub icir: f64,
    pub avg_long_count: f64,
    pub avg_short_count: f64,
    pub avg_gross: f64,
    pub avg_net: f64,
    pub total_costs: f64,
    pub by_period: Vec<PeriodStats>,
}

fn sharpe(pnl: &[f64], booksize: f64, bars_per_year: f64) -> f64 {
    if pnl.len() < 2 {
        return 0.0;
    }
    let r: Vec<f64> = pnl.iter().map(|p| p / booksize).collect();
    let m = r.iter().sum::<f64>() / r.len() as f64;
    let var = r.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (r.len() - 1) as f64;
    if var > 0.0 {
        m / var.sqrt() * bars_per_year.sqrt()
    } else {
        0.0
    }
}

impl Metrics {
    pub fn from_sim(r: &SimResult, bars_per_year: f64, bars_per_day: f64) -> Option<Metrics> {
        // Skip the warm-up bars before the first position.
        let first = r
            .long_count
            .iter()
            .zip(&r.short_count)
            .position(|(l, s)| l + s > 0)?;
        let pnl = &r.pnl[first..];
        let turnover = &r.turnover[first..];
        let bars = pnl.len();
        if bars < 2 {
            return None;
        }
        let total_pnl: f64 = pnl.iter().sum();
        let annual_return = total_pnl / r.booksize / bars as f64 * bars_per_year;
        let turnover_per_bar = turnover.iter().sum::<f64>() / bars as f64;
        let turnover_daily = turnover_per_bar * bars_per_day;
        let sh = sharpe(pnl, r.booksize, bars_per_year);
        let fitness = sh * (annual_return.abs() / turnover_daily.max(0.125)).sqrt();
        let traded: f64 = r.traded[first..].iter().sum();
        let margin_bps = if traded > 0.0 {
            total_pnl / traded * 10_000.0
        } else {
            0.0
        };

        let mut equity = 0.0;
        let mut peak = 0.0;
        let mut mdd = 0.0f64;
        for p in pnl {
            equity += p;
            peak = f64::max(peak, equity);
            mdd = mdd.max(peak - equity);
        }

        let ics: Vec<f64> = r.ic[first..]
            .iter()
            .copied()
            .filter(|v| v.is_finite())
            .collect();
        let (mean_ic, icir) = if ics.len() >= 2 {
            let m = ics.iter().sum::<f64>() / ics.len() as f64;
            let sd =
                (ics.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (ics.len() - 1) as f64).sqrt();
            (
                m,
                if sd > 0.0 {
                    m / sd * bars_per_year.sqrt()
                } else {
                    0.0
                },
            )
        } else {
            (f64::NAN, f64::NAN)
        };

        // Yearly breakdown (monthly when the sample is under two years).
        let span_ms = r.ts[r.ts.len() - 1] - r.ts[first];
        let monthly = span_ms < 2 * 365 * 86_400_000;
        let mut buckets: BTreeMap<String, (f64, Vec<f64>, f64)> = BTreeMap::new();
        for (k, t) in (first..r.ts.len()).enumerate() {
            let dt = chrono::DateTime::from_timestamp_millis(r.ts[t]).unwrap_or_default();
            let key = if monthly {
                dt.format("%Y-%m").to_string()
            } else {
                dt.format("%Y").to_string()
            };
            let e = buckets.entry(key).or_insert((0.0, Vec::new(), 0.0));
            e.0 += pnl[k];
            e.1.push(pnl[k]);
            e.2 += turnover[k];
        }
        let by_period = buckets
            .into_iter()
            .map(|(period, (p, series, to))| PeriodStats {
                period,
                pnl: p,
                return_pct: p / r.booksize * 100.0,
                sharpe: sharpe(&series, r.booksize, bars_per_year),
                turnover: to / series.len() as f64 * bars_per_day,
                bars: series.len(),
            })
            .collect();

        let avg = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
        Some(Metrics {
            bars,
            bars_per_year,
            start_ts: r.ts[first],
            end_ts: r.ts[r.ts.len() - 1],
            booksize: r.booksize,
            total_pnl,
            sharpe: sh,
            annual_return,
            turnover_per_bar,
            turnover_daily,
            fitness,
            margin_bps,
            max_drawdown: mdd,
            max_drawdown_pct: mdd / r.booksize * 100.0,
            mean_ic,
            icir,
            avg_long_count: r.long_count[first..].iter().map(|c| *c as f64).sum::<f64>()
                / bars as f64,
            avg_short_count: r.short_count[first..]
                .iter()
                .map(|c| *c as f64)
                .sum::<f64>()
                / bars as f64,
            avg_gross: avg(&r.long_notional[first..]) + avg(&r.short_notional[first..]),
            avg_net: avg(&r.long_notional[first..]) - avg(&r.short_notional[first..]),
            total_costs: r.costs[first..].iter().sum(),
            by_period,
        })
    }

    pub fn summary(&self) -> String {
        let fmt_ts = |ms: i64| {
            chrono::DateTime::from_timestamp_millis(ms)
                .map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_default()
        };
        let mut s = String::new();
        let row = |s: &mut String, k: &str, v: String| s.push_str(&format!("{k:<22}{v:>20}\n"));
        row(
            &mut s,
            "period",
            format!("{} .. {}", fmt_ts(self.start_ts), fmt_ts(self.end_ts)),
        );
        row(&mut s, "bars", self.bars.to_string());
        row(&mut s, "booksize", format!("{:.0}", self.booksize));
        row(&mut s, "pnl", format!("{:.2}", self.total_pnl));
        row(&mut s, "sharpe", format!("{:.3}", self.sharpe));
        row(
            &mut s,
            "returns (annual)",
            format!("{:.2}%", self.annual_return * 100.0),
        );
        row(
            &mut s,
            "turnover (daily)",
            format!("{:.2}%", self.turnover_daily * 100.0),
        );
        row(
            &mut s,
            "turnover (per bar)",
            format!("{:.2}%", self.turnover_per_bar * 100.0),
        );
        row(&mut s, "fitness", format!("{:.3}", self.fitness));
        row(&mut s, "margin", format!("{:.2} bps", self.margin_bps));
        row(
            &mut s,
            "max drawdown",
            format!("{:.2}%", self.max_drawdown_pct),
        );
        row(
            &mut s,
            "mean IC / ICIR",
            format!("{:.4} / {:.2}", self.mean_ic, self.icir),
        );
        row(
            &mut s,
            "avg long/short count",
            format!("{:.1} / {:.1}", self.avg_long_count, self.avg_short_count),
        );
        row(
            &mut s,
            "avg gross / net",
            format!("{:.0} / {:.0}", self.avg_gross, self.avg_net),
        );
        row(&mut s, "total costs", format!("{:.2}", self.total_costs));
        s.push_str(&format!(
            "\n{:<9}{:>12}{:>10}{:>9}{:>12}{:>7}\n",
            "period", "pnl", "return", "sharpe", "turnover/d", "bars"
        ));
        for p in &self.by_period {
            s.push_str(&format!(
                "{:<9}{:>12.2}{:>9.2}%{:>9.2}{:>11.1}%{:>7}\n",
                p.period,
                p.pnl,
                p.return_pct,
                p.sharpe,
                p.turnover * 100.0,
                p.bars
            ));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mat::Mat;
    use crate::sim::Settings;

    #[test]
    fn fitness_and_margin() {
        let n = 400;
        let pnl: Vec<f64> = (0..n)
            .map(|i| if i % 2 == 0 { 120.0 } else { -80.0 })
            .collect();
        let r = SimResult {
            ts: (0..n)
                .map(|i| 1_700_000_000_000 + i as i64 * 86_400_000)
                .collect(),
            pnl: pnl.clone(),
            gross_pnl: pnl,
            costs: vec![0.0; n],
            turnover: vec![0.2; n],
            long_count: vec![5; n],
            short_count: vec![5; n],
            long_notional: vec![5000.0; n],
            short_notional: vec![5000.0; n],
            traded: vec![2000.0; n],
            ic: vec![0.05; n],
            weights: Mat::filled(0, 0, 0.0),
            symbol_pnl: Mat::filled(0, 0, 0.0),
            settings: Settings::default(),
            booksize: 10_000.0,
        };
        let m = Metrics::from_sim(&r, 365.25, 1.0).unwrap();
        assert!((m.turnover_daily - 0.2).abs() < 1e-9);
        assert!((m.margin_bps - (20.0 * 400.0) / (2000.0 * 400.0) * 10_000.0).abs() < 1e-6);
        let expected_fitness = m.sharpe * (m.annual_return.abs() / 0.2).sqrt();
        assert!((m.fitness - expected_fitness).abs() < 1e-9);
        assert_eq!(m.by_period.len(), 14); // ~13 months of daily bars
        assert!(m.max_drawdown >= 80.0);
    }
}
