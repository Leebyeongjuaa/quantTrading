//! Two-leg pair execution (e.g. long ETH / short BTC for a stat-arb spread).
//!
//! Both legs are signed with consecutive nonces and submitted in a single `sendTxBatch` so
//! they hit the sequencer together. Each leg is an IOC limit order priced off the live book
//! with a slippage cap; unfilled remainders are retried a bounded number of times, and the
//! final imbalance is reported so the caller can decide whether to hedge or unwind.

use std::collections::HashMap;
use std::time::Duration;

use rust_decimal::Decimal;
use tracing::{info, warn};

use crate::error::{Error, Result};
use crate::gateway::{NewOrder, OrderGateway, OrderKind};
use crate::signer::TimeInForce;
use crate::types::{Order, OrderBook, Side};
use crate::ws::{self, WsConfig, WsEvent};

#[derive(Debug, Clone)]
pub struct Leg {
    pub symbol: String,
    pub side: Side,
    pub size: Decimal,
}

#[derive(Debug, Clone)]
pub struct PairConfig {
    pub legs: Vec<Leg>,
    /// Max adverse distance from the current marketable price, in basis points.
    pub slippage_bps: u32,
    /// How many times to re-send IOC orders for an unfilled remainder.
    pub max_retries: u32,
    /// How long to wait for fills after each submission round.
    pub fill_timeout: Duration,
    pub reduce_only: bool,
}

#[derive(Debug, Clone, Default)]
pub struct LegReport {
    pub symbol: String,
    pub side: Option<Side>,
    pub requested: Decimal,
    pub filled: Decimal,
    pub notional: Decimal,
}

impl LegReport {
    pub fn avg_price(&self) -> Option<Decimal> {
        if self.filled.is_zero() {
            None
        } else {
            Some(self.notional / self.filled)
        }
    }
    pub fn remaining(&self) -> Decimal {
        (self.requested - self.filled).max(Decimal::ZERO)
    }
}

#[derive(Debug, Clone, Default)]
pub struct PairReport {
    pub legs: Vec<LegReport>,
    pub rounds: u32,
}

impl PairReport {
    /// Signed USD exposure left over: +long notional - short notional across filled legs.
    pub fn net_notional(&self) -> Decimal {
        self.legs
            .iter()
            .map(|l| match l.side {
                Some(Side::Buy) => l.notional,
                Some(Side::Sell) => -l.notional,
                None => Decimal::ZERO,
            })
            .sum()
    }

    pub fn fully_filled(&self, min_sizes: &HashMap<String, Decimal>) -> bool {
        self.legs.iter().all(|l| {
            l.remaining() < *min_sizes.get(&l.symbol).unwrap_or(&Decimal::ZERO)
                || l.remaining().is_zero()
        })
    }
}

/// Position sizing helpers for a two-leg spread.
pub struct PairSizer;

impl PairSizer {
    /// Dollar-neutral-ish sizing: leg A gets `notional`, leg B gets `notional * beta`
    /// (`beta` = hedge ratio from the backtest's log-price regression; 1.0 = pure dollar neutral).
    pub fn from_notional(
        notional: Decimal,
        price_a: Decimal,
        price_b: Decimal,
        beta: Decimal,
    ) -> Result<(Decimal, Decimal)> {
        if notional <= Decimal::ZERO
            || price_a <= Decimal::ZERO
            || price_b <= Decimal::ZERO
            || beta <= Decimal::ZERO
        {
            return Err(Error::InvalidOrder(
                "notional, prices and beta must be positive".into(),
            ));
        }
        Ok((notional / price_a, notional * beta / price_b))
    }
}

/// Marketable IOC limit price: worst visible price for `size`, then widened by `slippage_bps`.
pub fn ioc_price(
    book: &OrderBook,
    side: Side,
    size: Decimal,
    slippage_bps: u32,
) -> Option<Decimal> {
    let reference = book.worst_price_for(side, size).or_else(|| match side {
        Side::Buy => book.best_ask().map(|(p, _)| p),
        Side::Sell => book.best_bid().map(|(p, _)| p),
    })?;
    let slip = reference * Decimal::from(slippage_bps) / Decimal::from(10_000);
    Some(match side {
        Side::Buy => reference + slip,
        Side::Sell => reference - slip,
    })
}

pub struct PairExecutor<'a> {
    gateway: &'a OrderGateway,
    ws_url: String,
}

impl<'a> PairExecutor<'a> {
    pub fn new(gateway: &'a OrderGateway, ws_url: impl Into<String>) -> Self {
        PairExecutor {
            gateway,
            ws_url: ws_url.into(),
        }
    }

    pub async fn run(&self, cfg: PairConfig) -> Result<PairReport> {
        if cfg.legs.len() < 2 {
            return Err(Error::InvalidOrder("a pair needs at least two legs".into()));
        }
        let markets: Vec<_> = cfg
            .legs
            .iter()
            .map(|l| self.gateway.markets().get(&l.symbol).cloned())
            .collect::<Result<_>>()?;

        let mut ws_cfg = WsConfig::new(self.ws_url.clone());
        ws_cfg.order_books = markets.iter().map(|m| m.market_id).collect();
        ws_cfg.account_index = Some(self.gateway.account_index());
        ws_cfg.account_orders = true;
        ws_cfg.auth_token = Some(self.gateway.auth_token(Duration::from_secs(3600))?);
        let mut ws = ws::spawn(ws_cfg);

        let mut report = PairReport {
            legs: cfg
                .legs
                .iter()
                .map(|l| LegReport {
                    symbol: l.symbol.clone(),
                    side: Some(l.side),
                    requested: l.size,
                    ..Default::default()
                })
                .collect(),
            rounds: 0,
        };
        let min_sizes: HashMap<String, Decimal> = markets
            .iter()
            .map(|m| (m.symbol.to_ascii_uppercase(), m.min_base_amount))
            .collect();
        let mut coi_to_leg: HashMap<i64, usize> = HashMap::new();
        let mut seen_fill: HashMap<i64, Decimal> = HashMap::new();

        // Wait for every book before pricing.
        let wait_books = async {
            loop {
                let ready = {
                    let b = ws.books.read().await;
                    markets.iter().all(|m| {
                        b.get(&m.market_id)
                            .map(|ob| ob.mid().is_some())
                            .unwrap_or(false)
                    })
                };
                if ready {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(15), wait_books)
            .await
            .map_err(|_| Error::Ws("order books did not arrive in time".into()))?;

        let result: Result<()> = async {
            for round in 0..=cfg.max_retries {
                let mut orders = Vec::new();
                let mut legs_in_round = Vec::new();
                for (i, (leg, m)) in cfg.legs.iter().zip(&markets).enumerate() {
                    let remaining = report.legs[i].remaining();
                    if remaining < m.min_base_amount || remaining.is_zero() {
                        continue;
                    }
                    let book = ws.book(m.market_id).await.ok_or_else(|| Error::Ws("book missing".into()))?;
                    let price = ioc_price(&book, leg.side, remaining, cfg.slippage_bps)
                        .ok_or_else(|| Error::Execution(format!("{}: empty book", leg.symbol)))?;
                    let coi = self.gateway.next_client_order_index();
                    coi_to_leg.insert(coi, i);
                    legs_in_round.push(i);
                    orders.push(NewOrder {
                        symbol: leg.symbol.clone(),
                        side: leg.side,
                        size: remaining,
                        price,
                        kind: OrderKind::Limit { tif: TimeInForce::Ioc, expiry: Duration::ZERO },
                        reduce_only: cfg.reduce_only,
                        client_order_index: Some(coi),
                    });
                }
                if orders.is_empty() {
                    break;
                }
                report.rounds = round + 1;
                info!(round, n = orders.len(), "submitting pair legs");
                let submitted = self.gateway.submit_batch(&orders).await?;
                for s in &submitted {
                    info!(symbol = %s.symbol, size = %s.size, price = %s.price, tx = %s.tx_hash, "leg sent");
                }
                if self.gateway.dry_run {
                    break;
                }
                // Collect fills until timeout or every leg of this round is terminal.
                let deadline = tokio::time::Instant::now() + cfg.fill_timeout;
                let mut terminal: HashMap<i64, bool> = submitted.iter().map(|s| (s.client_order_index, false)).collect();
                loop {
                    if terminal.values().all(|t| *t) {
                        break;
                    }
                    let ev = tokio::select! {
                        ev = ws.events.recv() => ev,
                        _ = tokio::time::sleep_until(deadline) => { warn!("fill timeout"); break; }
                    };
                    let Some(ev) = ev else { return Err(Error::Ws("stream closed".into())) };
                    if let WsEvent::Orders { orders, .. } = ev {
                        for o in orders {
                            apply_fill(&o, &coi_to_leg, &mut seen_fill, &mut report);
                            if let Some(t) = terminal.get_mut(&o.client_order_index) {
                                if o.is_terminal() {
                                    *t = true;
                                }
                            }
                        }
                    }
                }
                if report.fully_filled(&min_sizes) {
                    break;
                }
            }
            Ok(())
        }
        .await;
        ws.shutdown();
        result?;
        for l in &report.legs {
            info!(symbol = %l.symbol, filled = %l.filled, remaining = %l.remaining(), avg = ?l.avg_price(), "leg result");
        }
        if !report.fully_filled(&min_sizes) && !self.gateway.dry_run {
            warn!(net_notional = %report.net_notional(), "pair left an imbalance");
        }
        Ok(report)
    }
}

fn apply_fill(
    o: &Order,
    coi_to_leg: &HashMap<i64, usize>,
    seen: &mut HashMap<i64, Decimal>,
    report: &mut PairReport,
) {
    let Some(&leg) = coi_to_leg.get(&o.client_order_index) else {
        return;
    };
    let prev = seen
        .get(&o.client_order_index)
        .copied()
        .unwrap_or(Decimal::ZERO);
    let now = o.filled();
    if now > prev {
        let delta = now - prev;
        let price = if o.filled_quote_amount > Decimal::ZERO && now > Decimal::ZERO {
            o.filled_quote_amount / now
        } else {
            o.price
        };
        report.legs[leg].filled += delta;
        report.legs[leg].notional += delta * price;
        seen.insert(o.client_order_index, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Level;
    use rust_decimal_macros::dec;

    #[test]
    fn sizer_applies_beta() {
        let (a, b) =
            PairSizer::from_notional(dec!(1000), dec!(2000), dec!(50000), dec!(1.5)).unwrap();
        assert_eq!(a, dec!(0.5));
        assert_eq!(b, dec!(0.03));
        assert!(PairSizer::from_notional(dec!(0), dec!(1), dec!(1), dec!(1)).is_err());
    }

    #[test]
    fn ioc_price_walks_and_widens() {
        let mut b = OrderBook::new(0);
        b.replace(
            &[Level {
                price: dec!(99),
                size: dec!(1),
            }],
            &[
                Level {
                    price: dec!(101),
                    size: dec!(1),
                },
                Level {
                    price: dec!(102),
                    size: dec!(1),
                },
            ],
        );
        assert_eq!(
            ioc_price(&b, Side::Buy, dec!(2), 100).unwrap(),
            dec!(103.02)
        );
        assert_eq!(ioc_price(&b, Side::Sell, dec!(0.5), 0).unwrap(), dec!(99));
        // Thin book: falls back to best quote.
        assert_eq!(ioc_price(&b, Side::Buy, dec!(50), 0).unwrap(), dec!(101));
    }

    #[test]
    fn fills_accumulate_per_leg() {
        let mut report = PairReport {
            legs: vec![
                LegReport {
                    symbol: "ETH".into(),
                    side: Some(Side::Buy),
                    requested: dec!(1),
                    ..Default::default()
                },
                LegReport {
                    symbol: "BTC".into(),
                    side: Some(Side::Sell),
                    requested: dec!(0.05),
                    ..Default::default()
                },
            ],
            rounds: 0,
        };
        let map: HashMap<i64, usize> = [(1, 0), (2, 1)].into_iter().collect();
        let mut seen = HashMap::new();
        let o = Order {
            client_order_index: 1,
            initial_base_amount: dec!(1),
            remaining_base_amount: dec!(0.6),
            filled_base_amount: dec!(0.4),
            filled_quote_amount: dec!(800),
            ..Default::default()
        };
        apply_fill(&o, &map, &mut seen, &mut report);
        apply_fill(&o, &map, &mut seen, &mut report); // duplicate update is idempotent
        assert_eq!(report.legs[0].filled, dec!(0.4));
        assert_eq!(report.legs[0].avg_price(), Some(dec!(2000)));
        let o2 = Order {
            client_order_index: 2,
            initial_base_amount: dec!(0.05),
            remaining_base_amount: dec!(0),
            price: dec!(50000),
            status: "filled".into(),
            ..Default::default()
        };
        apply_fill(&o2, &map, &mut seen, &mut report);
        assert_eq!(report.legs[1].filled, dec!(0.05));
        assert_eq!(report.net_notional(), dec!(800) - dec!(2500));
        let mins: HashMap<String, Decimal> = [
            ("ETH".to_string(), dec!(0.002)),
            ("BTC".to_string(), dec!(0.0001)),
        ]
        .into_iter()
        .collect();
        assert!(!report.fully_filled(&mins));
    }
}
