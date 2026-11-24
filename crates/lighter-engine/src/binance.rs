//! Binance USDT-margined futures klines from the public bulk archive (`data.binance.vision`),
//! used to extend history before a Lighter market existed. Monthly zips cover closed months;
//! the current month comes from daily zips. A missing file (symbol not yet listed) is skipped.

use std::io::Read;
use std::time::Duration;

use chrono::{Datelike, NaiveDate, Utc};
use tracing::{debug, warn};

use crate::error::{Error, Result};
use crate::rest::resolution_ms;
use crate::types::Candle;

const BASE: &str = "https://data.binance.vision/data/futures/um";

pub struct BinanceSource {
    http: reqwest::Client,
}

/// Lighter symbol -> Binance USDT-M futures symbol (`ETH` -> `ETHUSDT`).
pub fn futures_symbol(lighter_symbol: &str) -> String {
    format!("{}USDT", lighter_symbol.trim().to_ascii_uppercase())
}

fn parse_csv(text: &str) -> Vec<Candle> {
    let mut out = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 6 {
            continue;
        }
        let Ok(mut t) = f[0].parse::<i64>() else {
            continue;
        }; // header row fails here
        if t > 100_000_000_000_000 {
            t /= 1000; // microseconds (newer archives)
        }
        let num = |i: usize| f.get(i).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
        out.push(Candle {
            open_time_ms: t,
            open: num(1),
            high: num(2),
            low: num(3),
            close: num(4),
            volume: num(5),
            quote_volume: num(7),
        });
    }
    out
}

impl BinanceSource {
    pub fn new() -> Result<Self> {
        Ok(BinanceSource {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .user_agent("lighter-engine/0.1")
                .build()?,
        })
    }

    /// Downloads one zip and returns the candles inside, or `None` when the archive does not
    /// exist (404: symbol not listed for that period).
    async fn zip_candles(&self, url: &str) -> Result<Option<Vec<Candle>>> {
        let mut attempt = 0;
        let bytes = loop {
            let resp = self.http.get(url).send().await;
            match resp {
                Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => return Ok(None),
                Ok(r) if r.status().is_success() => break r.bytes().await?,
                Ok(r) => {
                    if attempt >= 3 {
                        return Err(Error::Api {
                            code: r.status().as_u16() as i64,
                            message: format!("binance archive {url}"),
                        });
                    }
                }
                Err(e) => {
                    if attempt >= 3 {
                        return Err(e.into());
                    }
                }
            }
            attempt += 1;
            tokio::time::sleep(Duration::from_millis(500 * attempt)).await;
        };
        let cursor = std::io::Cursor::new(bytes.to_vec());
        let mut archive = zip::ZipArchive::new(cursor)
            .map_err(|e| Error::Config(format!("bad zip {url}: {e}")))?;
        let mut text = String::new();
        for i in 0..archive.len() {
            let mut f = archive
                .by_index(i)
                .map_err(|e| Error::Config(format!("bad zip entry {url}: {e}")))?;
            if f.name().ends_with(".csv") {
                f.read_to_string(&mut text)
                    .map_err(|e| Error::Config(format!("read zip {url}: {e}")))?;
                break;
            }
        }
        Ok(Some(parse_csv(&text)))
    }

    /// Klines in `[start_ms, end_ms)` for a futures symbol at a Binance interval
    /// (`1m 5m 15m 30m 1h 4h 12h 1d` all exist on both venues).
    pub async fn klines(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Candle>> {
        resolution_ms(interval).ok_or_else(|| Error::Config(format!("bad interval {interval}")))?;
        let start = chrono::DateTime::from_timestamp_millis(start_ms)
            .ok_or_else(|| Error::Config("bad start".into()))?
            .date_naive();
        let end = chrono::DateTime::from_timestamp_millis(end_ms)
            .ok_or_else(|| Error::Config("bad end".into()))?
            .date_naive();
        let today = Utc::now().date_naive();
        let mut out: Vec<Candle> = Vec::new();
        let mut month = NaiveDate::from_ymd_opt(start.year(), start.month(), 1).unwrap();
        while month <= end {
            let ym = format!("{:04}-{:02}", month.year(), month.month());
            let is_current = month.year() == today.year() && month.month() == today.month();
            let monthly = if is_current {
                None
            } else {
                let url = format!(
                    "{BASE}/monthly/klines/{symbol}/{interval}/{symbol}-{interval}-{ym}.zip"
                );
                self.zip_candles(&url).await?
            };
            match monthly {
                Some(c) => out.extend(c),
                None => {
                    // Current month (or a month the archive lacks): day files.
                    let mut day = month.max(start);
                    let last = next_month(month).pred_opt().unwrap().min(end).min(today);
                    while day <= last {
                        let url = format!(
                            "{BASE}/daily/klines/{symbol}/{interval}/{symbol}-{interval}-{}.zip",
                            day.format("%Y-%m-%d")
                        );
                        match self.zip_candles(&url).await? {
                            Some(c) => out.extend(c),
                            None => debug!(%symbol, %day, "no daily archive"),
                        }
                        day = day.succ_opt().unwrap();
                    }
                }
            }
            month = next_month(month);
        }
        if out.is_empty() {
            warn!(%symbol, "no Binance futures archive found for the requested range");
        }
        out.retain(|c| c.open_time_ms >= start_ms && c.open_time_ms < end_ms);
        out.sort_by_key(|c| c.open_time_ms);
        out.dedup_by_key(|c| c.open_time_ms);
        Ok(out)
    }
}

fn next_month(d: NaiveDate) -> NaiveDate {
    if d.month() == 12 {
        NaiveDate::from_ymd_opt(d.year() + 1, 1, 1).unwrap()
    } else {
        NaiveDate::from_ymd_opt(d.year(), d.month() + 1, 1).unwrap()
    }
}

/// Splices an older Binance series in front of a Lighter series. Binance prices are scaled by
/// the median Lighter/Binance close ratio over the overlap (if at least three bars overlap), so
/// the seam does not print a fake return. Returns `(bars, ratio, binance_bars_used)`.
pub fn splice(binance: &[Candle], lighter: &[Candle]) -> (Vec<Candle>, f64, usize) {
    let Some(first) = lighter.first().map(|c| c.open_time_ms) else {
        return (binance.to_vec(), 1.0, binance.len());
    };
    let mut ratios: Vec<f64> = Vec::new();
    for l in lighter.iter().take(48) {
        if let Ok(i) = binance.binary_search_by_key(&l.open_time_ms, |c| c.open_time_ms) {
            if binance[i].close > 0.0 && l.close > 0.0 {
                ratios.push(l.close / binance[i].close);
            }
        }
    }
    let ratio = if ratios.len() >= 3 {
        ratios.sort_by(|a, b| a.partial_cmp(b).unwrap());
        ratios[ratios.len() / 2]
    } else {
        1.0
    };
    let mut out: Vec<Candle> = binance
        .iter()
        .filter(|c| c.open_time_ms < first)
        .map(|c| Candle {
            open_time_ms: c.open_time_ms,
            open: c.open * ratio,
            high: c.high * ratio,
            low: c.low * ratio,
            close: c.close * ratio,
            volume: c.volume,
            quote_volume: c.quote_volume * ratio,
        })
        .collect();
    let used = out.len();
    out.extend_from_slice(lighter);
    (out, ratio, used)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_header_ms_and_us() {
        let text = "open_time,open,high,low,close,volume,close_time,quote_volume\n\
                    1735689600000,1,2,0.5,1.5,10,1735693199999,15\n\
                    1735693200000000,1.5,2,1,1.8,20,1735696799999999,30\n";
        let c = parse_csv(text);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].open_time_ms, 1_735_689_600_000);
        assert_eq!(c[1].open_time_ms, 1_735_693_200_000);
        assert_eq!(c[1].close, 1.8);
        assert_eq!(c[1].quote_volume, 30.0);
    }

    #[test]
    fn splice_scales_and_prefixes() {
        let mk = |t: i64, px: f64| Candle {
            open_time_ms: t,
            open: px,
            high: px,
            low: px,
            close: px,
            volume: 1.0,
            quote_volume: px,
        };
        let bn: Vec<Candle> = (0..10).map(|i| mk(i * 3_600_000, 100.0)).collect();
        let lt: Vec<Candle> = (5..12).map(|i| mk(i * 3_600_000, 101.0)).collect();
        let (out, ratio, used) = splice(&bn, &lt);
        assert!((ratio - 1.01).abs() < 1e-12);
        assert_eq!(used, 5);
        assert_eq!(out.len(), 12);
        assert!((out[0].close - 101.0).abs() < 1e-9);
        assert_eq!(out[5].close, 101.0);
        assert!(out
            .windows(2)
            .all(|w| w[1].open_time_ms > w[0].open_time_ms));
        let (only, r, _) = splice(&bn, &[]);
        assert_eq!(only.len(), 10);
        assert_eq!(r, 1.0);
    }

    #[test]
    fn symbol_mapping() {
        assert_eq!(futures_symbol("eth"), "ETHUSDT");
        assert_eq!(futures_symbol("1000PEPE"), "1000PEPEUSDT");
        assert_eq!(
            next_month(NaiveDate::from_ymd_opt(2025, 12, 5).unwrap()),
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()
        );
    }
}
