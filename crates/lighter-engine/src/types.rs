//! Core data types: markets, order books, orders, candles, and the decimal <-> integer
//! scaling Lighter uses on the wire.

use std::collections::BTreeMap;

use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn is_ask(self) -> bool {
        matches!(self, Side::Sell)
    }
    pub fn opposite(self) -> Side {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }
    pub fn from_is_ask(is_ask: bool) -> Side {
        if is_ask {
            Side::Sell
        } else {
            Side::Buy
        }
    }
}

impl std::fmt::Display for Side {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Side::Buy => "buy",
            Side::Sell => "sell",
        })
    }
}

/// Accepts a JSON number or a numeric string and parses it into a `Decimal`.
pub fn de_decimal<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Decimal, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    decimal_from_value(&v).ok_or_else(|| serde::de::Error::custom(format!("bad decimal {v}")))
}

pub fn de_decimal_opt<'de, D: Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<Decimal>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    if v.is_null() {
        return Ok(None);
    }
    Ok(decimal_from_value(&v))
}

/// Accepts a JSON integer or an integer string.
pub fn de_i64<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<i64, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    match &v {
        serde_json::Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .ok_or_else(|| serde::de::Error::custom("bad int")),
        serde_json::Value::String(s) => s.parse().map_err(serde::de::Error::custom),
        _ => Err(serde::de::Error::custom(format!("bad int {v}"))),
    }
}

pub fn decimal_from_value(v: &serde_json::Value) -> Option<Decimal> {
    match v {
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(Decimal::from(i))
            } else {
                n.as_f64().and_then(Decimal::from_f64)
            }
        }
        serde_json::Value::String(s) => Decimal::from_str_exact(s.trim())
            .ok()
            .or_else(|| s.trim().parse::<f64>().ok().and_then(Decimal::from_f64)),
        _ => None,
    }
}

/// Market metadata from `GET /api/v1/orderBookDetails`. Only the fields the engine needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketInfo {
    pub symbol: String,
    pub market_id: i64,
    #[serde(default)]
    pub market_type: String,
    #[serde(default)]
    pub status: String,
    pub size_decimals: u32,
    pub price_decimals: u32,
    #[serde(deserialize_with = "de_decimal")]
    pub min_base_amount: Decimal,
    #[serde(deserialize_with = "de_decimal")]
    pub min_quote_amount: Decimal,
    #[serde(default, deserialize_with = "de_decimal_opt")]
    pub taker_fee: Option<Decimal>,
    #[serde(default, deserialize_with = "de_decimal_opt")]
    pub maker_fee: Option<Decimal>,
    #[serde(default, deserialize_with = "de_decimal_opt")]
    pub last_trade_price: Option<Decimal>,
    #[serde(default, deserialize_with = "de_decimal_opt")]
    pub mark_price: Option<Decimal>,
    #[serde(default, deserialize_with = "de_decimal_opt")]
    pub index_price: Option<Decimal>,
    #[serde(default)]
    pub is_frozen: bool,
    #[serde(default, rename = "daily_quote_token_volume")]
    pub daily_quote_volume: f64,
}

impl MarketInfo {
    pub fn market_index(&self) -> Result<i16> {
        i16::try_from(self.market_id)
            .map_err(|_| Error::UnknownMarket(format!("market id {} out of range", self.market_id)))
    }

    pub fn size_tick(&self) -> Decimal {
        Decimal::new(1, self.size_decimals)
    }

    pub fn price_tick(&self) -> Decimal {
        Decimal::new(1, self.price_decimals)
    }

    /// Rounds a size down to the market's size tick.
    pub fn round_size(&self, size: Decimal) -> Decimal {
        size.round_dp_with_strategy(self.size_decimals, RoundingStrategy::ToZero)
    }

    /// Rounds a limit price toward the passive side: buys round down, sells round up.
    pub fn round_price(&self, price: Decimal, side: Side) -> Decimal {
        let strat = match side {
            Side::Buy => RoundingStrategy::ToNegativeInfinity,
            Side::Sell => RoundingStrategy::ToPositiveInfinity,
        };
        price.round_dp_with_strategy(self.price_decimals, strat)
    }

    /// `size * 10^size_decimals` as the integer `base_amount` the protocol signs.
    pub fn size_to_units(&self, size: Decimal) -> Result<i64> {
        let scaled = size * Decimal::from(10i64.pow(self.size_decimals));
        if scaled.fract() != Decimal::ZERO {
            return Err(Error::InvalidOrder(format!(
                "size {size} is not a multiple of the size tick {}",
                self.size_tick()
            )));
        }
        scaled
            .to_i64()
            .ok_or_else(|| Error::InvalidOrder(format!("size {size} out of range")))
    }

    /// `price * 10^price_decimals` as the integer `price` the protocol signs (u32 on the wire).
    pub fn price_to_units(&self, price: Decimal) -> Result<u32> {
        let scaled = price * Decimal::from(10i64.pow(self.price_decimals));
        if scaled.fract() != Decimal::ZERO {
            return Err(Error::InvalidOrder(format!(
                "price {price} is not a multiple of the price tick {}",
                self.price_tick()
            )));
        }
        scaled
            .to_u32()
            .ok_or_else(|| Error::InvalidOrder(format!("price {price} out of range")))
    }

    pub fn units_to_size(&self, units: i64) -> Decimal {
        Decimal::new(units, self.size_decimals)
    }

    pub fn units_to_price(&self, units: u32) -> Decimal {
        Decimal::new(units as i64, self.price_decimals)
    }

    /// Lighter's minimum order: `size >= min_base_amount` and `size*price >= min_quote_amount`.
    pub fn validate_min(&self, size: Decimal, price: Decimal) -> Result<()> {
        if size <= Decimal::ZERO {
            return Err(Error::InvalidOrder("size must be positive".into()));
        }
        if size < self.min_base_amount {
            return Err(Error::InvalidOrder(format!(
                "{}: size {size} < min_base_amount {}",
                self.symbol, self.min_base_amount
            )));
        }
        if price > Decimal::ZERO && size * price < self.min_quote_amount {
            return Err(Error::InvalidOrder(format!(
                "{}: notional {} < min_quote_amount {}",
                self.symbol,
                size * price,
                self.min_quote_amount
            )));
        }
        Ok(())
    }
}

/// One price level as streamed by Lighter: decimal strings.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Level {
    #[serde(deserialize_with = "de_decimal")]
    pub price: Decimal,
    #[serde(default, deserialize_with = "de_decimal")]
    pub size: Decimal,
}

/// `(price, size)` pair.
pub type Level2 = (Decimal, Decimal);

/// Locally maintained L2 order book. Bids and asks are keyed by price; a level with size 0
/// in an update removes the key.
#[derive(Debug, Clone, Default)]
pub struct OrderBook {
    pub market_id: i64,
    pub bids: BTreeMap<Decimal, Decimal>,
    pub asks: BTreeMap<Decimal, Decimal>,
    /// Sequence number of the last applied update (for continuity checks).
    pub nonce: Option<i64>,
    pub updated_at_ms: i64,
}

impl OrderBook {
    pub fn new(market_id: i64) -> Self {
        OrderBook {
            market_id,
            ..Default::default()
        }
    }

    pub fn replace(&mut self, bids: &[Level], asks: &[Level]) {
        self.bids.clear();
        self.asks.clear();
        self.apply(bids, asks);
    }

    pub fn apply(&mut self, bids: &[Level], asks: &[Level]) {
        for l in bids {
            if l.size.is_zero() {
                self.bids.remove(&l.price);
            } else {
                self.bids.insert(l.price, l.size);
            }
        }
        for l in asks {
            if l.size.is_zero() {
                self.asks.remove(&l.price);
            } else {
                self.asks.insert(l.price, l.size);
            }
        }
    }

    pub fn best_bid(&self) -> Option<(Decimal, Decimal)> {
        self.bids.iter().next_back().map(|(p, s)| (*p, *s))
    }

    pub fn best_ask(&self) -> Option<(Decimal, Decimal)> {
        self.asks.iter().next().map(|(p, s)| (*p, *s))
    }

    pub fn mid(&self) -> Option<Decimal> {
        match (self.best_bid(), self.best_ask()) {
            (Some((b, _)), Some((a, _))) => Some((a + b) / Decimal::TWO),
            _ => None,
        }
    }

    pub fn spread(&self) -> Option<Decimal> {
        match (self.best_bid(), self.best_ask()) {
            (Some((b, _)), Some((a, _))) => Some(a - b),
            _ => None,
        }
    }

    /// Walks the opposite side of the book and returns the worst price needed to fill `size`
    /// right now, or `None` if the visible book is too thin.
    pub fn worst_price_for(&self, side: Side, size: Decimal) -> Option<Decimal> {
        let mut remaining = size;
        let levels: Box<dyn Iterator<Item = (&Decimal, &Decimal)>> = match side {
            Side::Buy => Box::new(self.asks.iter()),
            Side::Sell => Box::new(self.bids.iter().rev()),
        };
        for (p, s) in levels {
            if *s >= remaining {
                return Some(*p);
            }
            remaining -= *s;
        }
        None
    }

    /// Size-weighted average fill price for `size` against the visible book.
    pub fn vwap_for(&self, side: Side, size: Decimal) -> Option<Decimal> {
        let mut remaining = size;
        let mut notional = Decimal::ZERO;
        let levels: Box<dyn Iterator<Item = (&Decimal, &Decimal)>> = match side {
            Side::Buy => Box::new(self.asks.iter()),
            Side::Sell => Box::new(self.bids.iter().rev()),
        };
        for (p, s) in levels {
            let take = remaining.min(*s);
            notional += take * *p;
            remaining -= take;
            if remaining.is_zero() {
                return Some(notional / size);
            }
        }
        None
    }

    pub fn depth(&self, n: usize) -> (Vec<Level2>, Vec<Level2>) {
        let bids = self
            .bids
            .iter()
            .rev()
            .take(n)
            .map(|(p, s)| (*p, *s))
            .collect();
        let asks = self.asks.iter().take(n).map(|(p, s)| (*p, *s)).collect();
        (bids, asks)
    }
}

/// An order as returned by REST (`accountActiveOrders`) and the `account_all_orders` stream.
/// Lighter sends amounts as decimal strings and indexes as integers; both are accepted.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Order {
    #[serde(default, deserialize_with = "de_i64")]
    pub order_index: i64,
    #[serde(default, deserialize_with = "de_i64")]
    pub client_order_index: i64,
    #[serde(default, deserialize_with = "de_i64")]
    pub market_index: i64,
    #[serde(default, deserialize_with = "de_i64")]
    pub owner_account_index: i64,
    #[serde(default, deserialize_with = "de_decimal")]
    pub initial_base_amount: Decimal,
    #[serde(default, deserialize_with = "de_decimal")]
    pub remaining_base_amount: Decimal,
    #[serde(default, deserialize_with = "de_decimal")]
    pub filled_base_amount: Decimal,
    #[serde(default, deserialize_with = "de_decimal")]
    pub filled_quote_amount: Decimal,
    #[serde(default, deserialize_with = "de_decimal")]
    pub price: Decimal,
    #[serde(default)]
    pub is_ask: bool,
    #[serde(default)]
    pub status: String,
    #[serde(default, rename = "type")]
    pub order_type: String,
    #[serde(default)]
    pub time_in_force: String,
    #[serde(default)]
    pub reduce_only: bool,
    #[serde(default, deserialize_with = "de_i64")]
    pub order_expiry: i64,
    #[serde(default, deserialize_with = "de_i64")]
    pub timestamp: i64,
}

impl Order {
    pub fn side(&self) -> Side {
        Side::from_is_ask(self.is_ask)
    }

    /// Filled amount, derived from `initial - remaining` when `filled_base_amount` is absent.
    pub fn filled(&self) -> Decimal {
        if self.filled_base_amount > Decimal::ZERO {
            self.filled_base_amount
        } else {
            (self.initial_base_amount - self.remaining_base_amount).max(Decimal::ZERO)
        }
    }

    /// True when the order can no longer fill (filled, canceled, expired, rejected...).
    pub fn is_terminal(&self) -> bool {
        let s = self.status.to_ascii_lowercase();
        if s.is_empty() {
            return self.remaining_base_amount.is_zero()
                && self.initial_base_amount > Decimal::ZERO;
        }
        !(s.contains("open")
            || s.contains("pending")
            || s.contains("partial")
            || s == "in-progress"
            || s == "new")
    }

    pub fn is_filled(&self) -> bool {
        self.status.eq_ignore_ascii_case("filled")
            || (self.remaining_base_amount.is_zero() && self.initial_base_amount > Decimal::ZERO)
    }
}

/// OHLCV candle from `GET /api/v1/candles`. Times are Unix milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Candle {
    #[serde(rename = "t")]
    pub open_time_ms: i64,
    #[serde(rename = "o")]
    pub open: f64,
    #[serde(rename = "h")]
    pub high: f64,
    #[serde(rename = "l")]
    pub low: f64,
    #[serde(rename = "c")]
    pub close: f64,
    #[serde(rename = "v", default)]
    pub volume: f64,
    #[serde(rename = "V", default)]
    pub quote_volume: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn eth() -> MarketInfo {
        serde_json::from_str(
            r#"{"symbol":"ETH","market_id":0,"size_decimals":4,"price_decimals":2,
                "min_base_amount":"0.0020","min_quote_amount":"10.000000","taker_fee":"0.0000"}"#,
        )
        .unwrap()
    }

    #[test]
    fn scaling_round_trips() {
        let m = eth();
        assert_eq!(m.size_to_units(dec!(0.01)).unwrap(), 100);
        assert_eq!(m.price_to_units(dec!(3100.00)).unwrap(), 310_000);
        assert_eq!(m.units_to_size(100), dec!(0.0100));
        assert_eq!(m.units_to_price(310_000), dec!(3100.00));
        assert!(m.size_to_units(dec!(0.00005)).is_err());
        assert!(m.price_to_units(dec!(0.001)).is_err());
    }

    #[test]
    fn rounding_is_passive() {
        let m = eth();
        assert_eq!(m.round_price(dec!(1000.126), Side::Buy), dec!(1000.12));
        assert_eq!(m.round_price(dec!(1000.121), Side::Sell), dec!(1000.13));
        assert_eq!(m.round_size(dec!(0.12349),), dec!(0.1234));
    }

    #[test]
    fn min_checks() {
        let m = eth();
        assert!(m.validate_min(dec!(0.001), dec!(2000)).is_err());
        assert!(m.validate_min(dec!(0.002), dec!(2000)).is_err()); // $4 notional
        assert!(m.validate_min(dec!(0.01), dec!(2000)).is_ok());
    }

    #[test]
    fn book_walks_levels() {
        let mut b = OrderBook::new(0);
        b.replace(
            &[
                Level {
                    price: dec!(99),
                    size: dec!(1),
                },
                Level {
                    price: dec!(98),
                    size: dec!(2),
                },
            ],
            &[
                Level {
                    price: dec!(101),
                    size: dec!(1),
                },
                Level {
                    price: dec!(102),
                    size: dec!(5),
                },
            ],
        );
        assert_eq!(b.best_bid().unwrap().0, dec!(99));
        assert_eq!(b.best_ask().unwrap().0, dec!(101));
        assert_eq!(b.mid().unwrap(), dec!(100));
        assert_eq!(b.worst_price_for(Side::Buy, dec!(3)).unwrap(), dec!(102));
        assert_eq!(b.worst_price_for(Side::Sell, dec!(2)).unwrap(), dec!(98));
        assert!(b.worst_price_for(Side::Buy, dec!(100)).is_none());
        assert_eq!(b.vwap_for(Side::Buy, dec!(2)).unwrap(), dec!(101.5));
        b.apply(
            &[Level {
                price: dec!(99),
                size: dec!(0),
            }],
            &[],
        );
        assert_eq!(b.best_bid().unwrap().0, dec!(98));
    }

    #[test]
    fn order_parses_mixed_types() {
        let o: Order = serde_json::from_str(
            r#"{"order_index":"281478346434840","client_order_index":7,"market_index":0,
                "initial_base_amount":"1.5","remaining_base_amount":"0.5","price":"1700.1",
                "is_ask":true,"status":"open","type":"limit"}"#,
        )
        .unwrap();
        assert_eq!(o.order_index, 281478346434840);
        assert_eq!(o.filled(), dec!(1.0));
        assert!(!o.is_terminal());
        assert_eq!(o.side(), Side::Sell);
    }
}
