//! Candle utilities: resampling Lighter's native resolutions into arbitrary bar sizes
//! (e.g. 5m -> 10m, which Lighter does not serve) and CSV export.

use std::io::Write;

use crate::error::{Error, Result};
use crate::types::Candle;

/// Aggregates `candles` (sorted or not) into `target_ms` buckets aligned to the Unix epoch.
/// Buckets with no source candle are omitted, matching Lighter's own behaviour.
pub fn resample(candles: &[Candle], target_ms: i64) -> Vec<Candle> {
    if target_ms <= 0 || candles.is_empty() {
        return Vec::new();
    }
    let mut sorted: Vec<Candle> = candles.to_vec();
    sorted.sort_by_key(|c| c.open_time_ms);
    let mut out: Vec<Candle> = Vec::new();
    for c in sorted {
        let bucket = c.open_time_ms.div_euclid(target_ms) * target_ms;
        match out.last_mut() {
            Some(last) if last.open_time_ms == bucket => {
                last.high = last.high.max(c.high);
                last.low = last.low.min(c.low);
                last.close = c.close;
                last.volume += c.volume;
                last.quote_volume += c.quote_volume;
            }
            _ => out.push(Candle {
                open_time_ms: bucket,
                ..c
            }),
        }
    }
    out
}

pub const CSV_HEADER: &str = "ts,open,high,low,close,volume,quote_volume";

/// Writes `ts,open,high,low,close,volume,quote_volume` with `ts` in Unix milliseconds. This is
/// the input format of the `statarb-backtest` crate.
pub fn write_csv<W: Write>(mut w: W, candles: &[Candle]) -> Result<()> {
    writeln!(w, "{CSV_HEADER}").map_err(io_err)?;
    for c in candles {
        writeln!(
            w,
            "{},{},{},{},{},{},{}",
            c.open_time_ms, c.open, c.high, c.low, c.close, c.volume, c.quote_volume
        )
        .map_err(io_err)?;
    }
    Ok(())
}

fn io_err(e: std::io::Error) -> Error {
    Error::Config(format!("io error: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(t_min: i64, o: f64, h: f64, l: f64, cl: f64, v: f64) -> Candle {
        Candle {
            open_time_ms: t_min * 60_000,
            open: o,
            high: h,
            low: l,
            close: cl,
            volume: v,
            quote_volume: v * cl,
        }
    }

    #[test]
    fn five_minute_to_ten_minute() {
        let src = vec![
            c(0, 1.0, 2.0, 0.5, 1.5, 1.0),
            c(5, 1.5, 3.0, 1.0, 2.5, 2.0),
            c(10, 2.5, 2.6, 2.4, 2.5, 1.0),
        ];
        let out = resample(&src, 600_000);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].open_time_ms, 0);
        assert_eq!(out[0].open, 1.0);
        assert_eq!(out[0].high, 3.0);
        assert_eq!(out[0].low, 0.5);
        assert_eq!(out[0].close, 2.5);
        assert_eq!(out[0].volume, 3.0);
        assert_eq!(out[1].open_time_ms, 600_000);
    }

    #[test]
    fn unsorted_input_and_gaps() {
        let src = vec![
            c(25, 1.0, 1.0, 1.0, 1.0, 1.0),
            c(5, 2.0, 2.0, 2.0, 2.0, 1.0),
            c(0, 3.0, 3.0, 3.0, 3.0, 1.0),
        ];
        let out = resample(&src, 600_000);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].open, 3.0);
        assert_eq!(out[0].close, 2.0);
        assert_eq!(out[1].open_time_ms, 20 * 60_000);
    }
}
