//! Data panel: aligned `T × N` matrices of the standard fields for a universe of instruments.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{bail, Context, Result};
use statarb_backtest::data::{load_csv, resample, Bar};

use crate::mat::Mat;

#[derive(Debug, Clone)]
pub struct Panel {
    pub symbols: Vec<String>,
    /// Bar open times, Unix ms, ascending.
    pub ts: Vec<i64>,
    pub open: Mat,
    pub high: Mat,
    pub low: Mat,
    pub close: Mat,
    pub volume: Mat,
    /// Dollar volume (`volume * close`, or quote volume when present in the CSV).
    pub dollar_volume: Mat,
    /// `close_t / close_{t-1} - 1`; NaN on the first bar of each instrument.
    pub returns: Mat,
    /// `(high + low + close) / 3` proxy for VWAP.
    pub vwap: Mat,
    pub interval_ms: i64,
}

impl Panel {
    pub fn t(&self) -> usize {
        self.ts.len()
    }
    pub fn n(&self) -> usize {
        self.symbols.len()
    }

    pub fn bars_per_year(&self) -> f64 {
        365.25 * 86_400_000.0 / self.interval_ms as f64
    }

    pub fn bars_per_day(&self) -> f64 {
        86_400_000.0 / self.interval_ms as f64
    }

    /// Builds a panel from per-instrument bar series. Timestamps are the union of all series;
    /// instruments missing a bar get NaN there.
    pub fn from_series(series: Vec<(String, Vec<Bar>)>) -> Result<Panel> {
        if series.is_empty() {
            bail!("no instruments");
        }
        let mut all_ts: BTreeSet<i64> = BTreeSet::new();
        for (_, bars) in &series {
            all_ts.extend(bars.iter().map(|b| b.ts));
        }
        let ts: Vec<i64> = all_ts.into_iter().collect();
        if ts.len() < 2 {
            bail!("need at least two bars");
        }
        let n = series.len();
        let t = ts.len();
        let mut p = Panel {
            symbols: series.iter().map(|(s, _)| s.clone()).collect(),
            ts: ts.clone(),
            open: Mat::nan(t, n),
            high: Mat::nan(t, n),
            low: Mat::nan(t, n),
            close: Mat::nan(t, n),
            volume: Mat::nan(t, n),
            dollar_volume: Mat::nan(t, n),
            returns: Mat::nan(t, n),
            vwap: Mat::nan(t, n),
            interval_ms: 0,
        };
        let mut gaps: Vec<i64> = ts.windows(2).map(|w| w[1] - w[0]).collect();
        gaps.sort_unstable();
        p.interval_ms = gaps[gaps.len() / 2].max(1);

        for (i, (_, bars)) in series.iter().enumerate() {
            let mut idx = 0usize;
            let mut prev_close = f64::NAN;
            for b in bars {
                while idx < t && ts[idx] < b.ts {
                    idx += 1;
                }
                if idx >= t || ts[idx] != b.ts {
                    continue;
                }
                p.open.set(idx, i, b.open);
                p.high.set(idx, i, b.high);
                p.low.set(idx, i, b.low);
                p.close.set(idx, i, b.close);
                p.volume.set(idx, i, b.volume);
                p.dollar_volume.set(idx, i, b.volume * b.close);
                p.vwap.set(idx, i, (b.high + b.low + b.close) / 3.0);
                if prev_close.is_finite() && prev_close > 0.0 {
                    p.returns.set(idx, i, b.close / prev_close - 1.0);
                }
                prev_close = b.close;
            }
        }
        Ok(p)
    }

    /// Loads every `*.csv` in `dir` as one instrument named after the file stem. With `bar`,
    /// each series is resampled to that interval first.
    pub fn load_dir(dir: impl AsRef<Path>, bar_ms: Option<i64>) -> Result<Panel> {
        let dir = dir.as_ref();
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .with_context(|| format!("read {}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension()
                    .map(|e| e.eq_ignore_ascii_case("csv"))
                    .unwrap_or(false)
            })
            .collect();
        files.sort();
        if files.is_empty() {
            bail!("no CSV files in {}", dir.display());
        }
        let mut series = Vec::new();
        for f in files {
            let name = f
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("?")
                .to_string();
            let mut bars = load_csv(&f)?;
            if let Some(ms) = bar_ms {
                bars = resample(&bars, ms);
            }
            if bars.len() >= 2 {
                series.push((name, bars));
            }
        }
        Self::from_series(series)
    }

    /// Standard field by name, as used in expressions.
    pub fn field(&self, name: &str) -> Option<&Mat> {
        Some(match name {
            "open" => &self.open,
            "high" => &self.high,
            "low" => &self.low,
            "close" => &self.close,
            "volume" => &self.volume,
            "dollar_volume" => &self.dollar_volume,
            "returns" => &self.returns,
            "vwap" => &self.vwap,
            _ => return None,
        })
    }

    pub const FIELDS: &'static [&'static str] = &[
        "open",
        "high",
        "low",
        "close",
        "volume",
        "dollar_volume",
        "returns",
        "vwap",
    ];
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn bars(ts0: i64, closes: &[f64]) -> Vec<Bar> {
        closes
            .iter()
            .enumerate()
            .map(|(i, c)| Bar {
                ts: ts0 + i as i64 * 3_600_000,
                open: *c,
                high: *c + 1.0,
                low: *c - 1.0,
                close: *c,
                volume: 10.0,
            })
            .collect()
    }

    #[test]
    fn union_timestamps_and_returns() {
        let p = Panel::from_series(vec![
            ("A".into(), bars(0, &[10.0, 11.0, 12.1])),
            ("B".into(), bars(3_600_000, &[100.0, 90.0])),
        ])
        .unwrap();
        assert_eq!(p.t(), 3);
        assert_eq!(p.n(), 2);
        assert_eq!(p.interval_ms, 3_600_000);
        assert!(p.close.get(0, 1).is_nan());
        assert!((p.returns.get(1, 0) - 0.1).abs() < 1e-12);
        assert!((p.returns.get(2, 1) + 0.1).abs() < 1e-12);
        assert!(p.returns.get(1, 1).is_nan());
        assert_eq!(p.dollar_volume.get(2, 0), 121.0);
    }
}
