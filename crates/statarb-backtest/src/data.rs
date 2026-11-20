//! Bar loading, resampling and alignment.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bar {
    /// Bar open time, Unix milliseconds.
    pub ts: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

/// Two aligned series (same timestamps, same length).
#[derive(Debug, Clone, Default)]
pub struct PairSeries {
    pub ts: Vec<i64>,
    pub a: Vec<Bar>,
    pub b: Vec<Bar>,
}

impl PairSeries {
    pub fn len(&self) -> usize {
        self.ts.len()
    }
    pub fn is_empty(&self) -> bool {
        self.ts.is_empty()
    }
    /// Median bar interval in ms (robust to gaps).
    pub fn interval_ms(&self) -> Option<i64> {
        if self.ts.len() < 2 {
            return None;
        }
        let mut d: Vec<i64> = self
            .ts
            .windows(2)
            .map(|w| w[1] - w[0])
            .filter(|d| *d > 0)
            .collect();
        if d.is_empty() {
            return None;
        }
        d.sort_unstable();
        Some(d[d.len() / 2])
    }
}

/// Parses a timestamp cell: Unix ms, Unix seconds, or RFC 3339 / `YYYY-MM-DD HH:MM:SS`.
pub fn parse_ts(s: &str) -> Result<i64> {
    let s = s.trim();
    if let Ok(n) = s.parse::<i64>() {
        return Ok(if n > 100_000_000_000 { n } else { n * 1000 });
    }
    if let Ok(f) = s.parse::<f64>() {
        let n = f as i64;
        return Ok(if n > 100_000_000_000 { n } else { n * 1000 });
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(dt.timestamp_millis());
    }
    for fmt in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%d",
    ] {
        if let Ok(ndt) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            return Ok(ndt.and_utc().timestamp_millis());
        }
        if fmt == "%Y-%m-%d" {
            if let Ok(d) = chrono::NaiveDate::parse_from_str(s, fmt) {
                return Ok(d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis());
            }
        }
    }
    bail!("unparseable timestamp {s:?}")
}

/// Loads OHLCV bars from a CSV with a header. Column names are matched case-insensitively:
/// time = `ts|timestamp|time|date|open_time|t`, then `open|o`, `high|h`, `low|l`, `close|c`,
/// `volume|vol|v` (volume optional). This is the format `lighter-cli candles` writes.
pub fn load_csv(path: impl AsRef<Path>) -> Result<Vec<Bar>> {
    let path = path.as_ref();
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .trim(csv::Trim::All)
        .flexible(true)
        .from_path(path)
        .with_context(|| format!("open {}", path.display()))?;
    let headers: Vec<String> = rdr
        .headers()?
        .iter()
        .map(|h| h.to_ascii_lowercase())
        .collect();
    let find = |names: &[&str]| headers.iter().position(|h| names.contains(&h.as_str()));
    let ts_i = find(&[
        "ts",
        "timestamp",
        "time",
        "date",
        "datetime",
        "open_time",
        "t",
    ])
    .context("no timestamp column")?;
    let o_i = find(&["open", "o"]).context("no open column")?;
    let h_i = find(&["high", "h"]).context("no high column")?;
    let l_i = find(&["low", "l"]).context("no low column")?;
    let c_i = find(&["close", "c"]).context("no close column")?;
    let v_i = find(&["volume", "vol", "v", "base_volume"]);
    let mut bars = Vec::new();
    for (line, rec) in rdr.records().enumerate() {
        let rec = rec?;
        let cell = |i: usize| rec.get(i).unwrap_or("");
        let num = |i: usize| -> Result<f64> {
            cell(i).parse::<f64>().with_context(|| {
                format!("{}:{}: bad number {:?}", path.display(), line + 2, cell(i))
            })
        };
        bars.push(Bar {
            ts: parse_ts(cell(ts_i))?,
            open: num(o_i)?,
            high: num(h_i)?,
            low: num(l_i)?,
            close: num(c_i)?,
            volume: v_i.map(num).transpose()?.unwrap_or(0.0),
        });
    }
    bars.sort_by_key(|b| b.ts);
    bars.dedup_by_key(|b| b.ts);
    Ok(bars)
}

pub fn write_csv(path: impl AsRef<Path>, bars: &[Bar]) -> Result<()> {
    let mut w = csv::Writer::from_path(path.as_ref())?;
    w.write_record(["ts", "open", "high", "low", "close", "volume"])?;
    for b in bars {
        w.write_record([
            b.ts.to_string(),
            b.open.to_string(),
            b.high.to_string(),
            b.low.to_string(),
            b.close.to_string(),
            b.volume.to_string(),
        ])?;
    }
    w.flush()?;
    Ok(())
}

/// Aggregates bars into `target_ms` buckets aligned to the epoch (e.g. 1m/5m → 10m).
pub fn resample(bars: &[Bar], target_ms: i64) -> Vec<Bar> {
    let mut sorted = bars.to_vec();
    sorted.sort_by_key(|b| b.ts);
    let mut out: Vec<Bar> = Vec::new();
    for b in sorted {
        let bucket = b.ts.div_euclid(target_ms) * target_ms;
        match out.last_mut() {
            Some(last) if last.ts == bucket => {
                last.high = last.high.max(b.high);
                last.low = last.low.min(b.low);
                last.close = b.close;
                last.volume += b.volume;
            }
            _ => out.push(Bar { ts: bucket, ..b }),
        }
    }
    out
}

/// Inner-joins two series on timestamp.
pub fn align(a: &[Bar], b: &[Bar]) -> PairSeries {
    let mut out = PairSeries::default();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].ts.cmp(&b[j].ts) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.ts.push(a[i].ts);
                out.a.push(a[i]);
                out.b.push(b[j]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_700_000_400_000; // aligned to a 10-minute boundary

    fn bar(ts_min: i64, px: f64) -> Bar {
        Bar {
            ts: T0 + ts_min * 60_000,
            open: px,
            high: px + 1.0,
            low: px - 1.0,
            close: px + 0.5,
            volume: 1.0,
        }
    }

    #[test]
    fn timestamps() {
        assert_eq!(parse_ts("1700000000").unwrap(), 1_700_000_000_000);
        assert_eq!(parse_ts("1700000000000").unwrap(), 1_700_000_000_000);
        assert_eq!(parse_ts("2023-11-14T22:13:20Z").unwrap(), 1_700_000_000_000);
        assert_eq!(parse_ts("2023-11-14 22:13:20").unwrap(), 1_700_000_000_000);
        assert!(parse_ts("nope").is_err());
    }

    #[test]
    fn resample_to_ten_minutes() {
        let bars: Vec<Bar> = (0..25).map(|i| bar(i, 100.0 + i as f64)).collect();
        let out = resample(&bars, 600_000);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].open, 100.0);
        assert_eq!(out[0].close, 109.5);
        assert_eq!(out[0].high, 110.0);
        assert_eq!(out[0].low, 99.0);
        assert_eq!(out[0].volume, 10.0);
        assert_eq!(out[2].ts, T0 + 20 * 60_000);
        assert_eq!(out[2].volume, 5.0);
    }

    #[test]
    fn align_inner_join() {
        let a: Vec<Bar> = [0, 10, 20, 30].iter().map(|&t| bar(t, 1.0)).collect();
        let b: Vec<Bar> = [10, 20, 40].iter().map(|&t| bar(t, 2.0)).collect();
        let p = align(&a, &b);
        assert_eq!(p.ts, vec![T0 + 600_000, T0 + 1_200_000]);
        assert_eq!(p.interval_ms(), Some(600_000));
    }

    #[test]
    fn csv_round_trip() {
        let dir = std::env::temp_dir().join(format!("statarb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.csv");
        let bars = vec![bar(0, 1.0), bar(10, 2.0)];
        write_csv(&p, &bars).unwrap();
        let back = load_csv(&p).unwrap();
        assert_eq!(back, bars);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
