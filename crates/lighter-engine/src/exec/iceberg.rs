//! Iceberg execution: rest a visible `display_size` slice at a time until `total_size` is done.
//!
//! Lighter has no native iceberg order type, so the engine emulates one: it places a limit
//! slice, watches its own-orders stream for fills, and once the slice is (sufficiently) filled
//! or gone, places the next one. With [`Pricing::Peg`] the slice follows the best quote and is
//! cancelled/replaced when the market moves away by more than the configured tolerance.

use std::collections::HashMap;
use std::time::Duration;

use rust_decimal::Decimal;
use tracing::{info, warn};

use crate::error::{Error, Result};
use crate::gateway::{NewOrder, OrderGateway, OrderKind};
use crate::signer::{now_ms, TimeInForce};
use crate::types::{MarketInfo, Order, OrderBook, Side};
use crate::ws::{self, WsConfig, WsEvent};

#[derive(Debug, Clone, PartialEq)]
pub enum Pricing {
    /// Rest every slice at this limit price.
    Fixed(Decimal),
    /// Join the best quote on our side, improved by `offset_ticks` (0 = join, 1 = one tick
    /// inside the spread). Never crosses the opposite side.
    Peg { offset_ticks: i64 },
}

#[derive(Debug, Clone)]
pub struct IcebergConfig {
    pub symbol: String,
    pub side: Side,
    pub total_size: Decimal,
    pub display_size: Decimal,
    pub pricing: Pricing,
    /// `PostOnly` (default, never pays taker) or `Gtt`.
    pub tif: TimeInForce,
    /// Fraction of the visible slice that must fill before the next slice is placed
    /// (1.0 = wait for a complete fill).
    pub refill_at: Decimal,
    /// Minimum pause between consecutive slice submissions.
    pub min_interval: Duration,
    /// For `Peg`: reprice when the best quote moves more than this many ticks away.
    pub reprice_ticks: i64,
    /// Randomise each slice size within ±pct (0 disables); hides the pattern.
    pub jitter_pct: u32,
    pub reduce_only: bool,
}

/// What the planner wants the runner to do next.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    None,
    Place { size: Decimal, price: Decimal },
    Cancel { order_index: i64 },
    Done,
}

#[derive(Debug, Clone)]
struct Slice {
    client_order_index: i64,
    order_index: Option<i64>,
    size: Decimal,
    price: Decimal,
    filled: Decimal,
    sent_at_ms: i64,
    cancel_requested: bool,
}

/// Pure iceberg state machine. Feed it order updates and book updates; it returns actions.
#[derive(Debug, Clone)]
pub struct IcebergPlanner {
    pub cfg: IcebergConfig,
    pub market: MarketInfo,
    pub filled: Decimal,
    pub slices_sent: u32,
    pub notional: Decimal,
    active: Option<Slice>,
    last_sent_ms: i64,
    rng: u64,
}

impl IcebergPlanner {
    pub fn new(cfg: IcebergConfig, market: MarketInfo) -> Result<Self> {
        if cfg.display_size <= Decimal::ZERO || cfg.total_size <= Decimal::ZERO {
            return Err(Error::InvalidOrder("sizes must be positive".into()));
        }
        if cfg.display_size > cfg.total_size {
            return Err(Error::InvalidOrder(
                "display_size must not exceed total_size".into(),
            ));
        }
        if cfg.refill_at <= Decimal::ZERO || cfg.refill_at > Decimal::ONE {
            return Err(Error::InvalidOrder("refill_at must be in (0, 1]".into()));
        }
        if cfg.tif == TimeInForce::Ioc {
            return Err(Error::InvalidOrder(
                "iceberg slices must rest (PostOnly or Gtt)".into(),
            ));
        }
        market.validate_min(market.round_size(cfg.display_size), Decimal::ZERO)?;
        Ok(IcebergPlanner {
            cfg,
            market,
            filled: Decimal::ZERO,
            slices_sent: 0,
            notional: Decimal::ZERO,
            active: None,
            last_sent_ms: 0,
            rng: now_ms() as u64 | 1,
        })
    }

    pub fn remaining(&self) -> Decimal {
        (self.cfg.total_size - self.filled).max(Decimal::ZERO)
    }

    pub fn is_done(&self) -> bool {
        self.remaining() < self.market.min_base_amount || self.remaining().is_zero()
    }

    pub fn has_active(&self) -> bool {
        self.active.is_some()
    }

    pub fn active_order_index(&self) -> Option<i64> {
        self.active.as_ref().and_then(|s| s.order_index)
    }

    pub fn active_client_order_index(&self) -> Option<i64> {
        self.active.as_ref().map(|s| s.client_order_index)
    }

    fn next_u64(&mut self) -> u64 {
        // xorshift64*: deterministic, dependency-free jitter.
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Size of the next visible slice (jittered, rounded, clamped to remaining).
    pub fn next_slice_size(&mut self) -> Decimal {
        let mut size = self.cfg.display_size;
        if self.cfg.jitter_pct > 0 {
            let r = (self.next_u64() % (2 * self.cfg.jitter_pct as u64 + 1)) as i64
                - self.cfg.jitter_pct as i64;
            size += size * Decimal::from(r) / Decimal::from(100);
        }
        let size = self.market.round_size(size.min(self.remaining()));
        let remaining_after = self.remaining() - size;
        // Avoid leaving an unfillable crumb below the exchange minimum.
        if remaining_after > Decimal::ZERO && remaining_after < self.market.min_base_amount {
            self.remaining()
        } else {
            size.max(self.market.min_base_amount)
        }
    }

    /// Desired limit price for a new slice given the current book.
    pub fn target_price(&self, book: &OrderBook) -> Option<Decimal> {
        match &self.cfg.pricing {
            Pricing::Fixed(p) => Some(self.market.round_price(*p, self.cfg.side)),
            Pricing::Peg { offset_ticks } => {
                let tick = self.market.price_tick();
                let (bid, _) = book.best_bid()?;
                let (ask, _) = book.best_ask()?;
                let off = tick * Decimal::from(*offset_ticks);
                let p = match self.cfg.side {
                    Side::Buy => (bid + off).min(ask - tick),
                    Side::Sell => (ask - off).max(bid + tick),
                };
                Some(self.market.round_price(p, self.cfg.side))
            }
        }
    }

    /// Called on a timer/book tick. Decides whether to place, reprice, or finish.
    pub fn on_tick(&mut self, book: &OrderBook, now: i64) -> Action {
        if self.is_done() {
            return match &self.active {
                Some(s) if s.order_index.is_some() && !s.cancel_requested => Action::Cancel {
                    order_index: s.order_index.unwrap(),
                },
                Some(_) => Action::None,
                None => Action::Done,
            };
        }
        match &self.active {
            None => {
                if now - self.last_sent_ms < self.cfg.min_interval.as_millis() as i64 {
                    return Action::None;
                }
                let Some(price) = self.target_price(book) else {
                    return Action::None;
                };
                let size = self.next_slice_size();
                Action::Place { size, price }
            }
            Some(s) => {
                if s.cancel_requested {
                    return Action::None;
                }
                if let Pricing::Peg { .. } = self.cfg.pricing {
                    if let (Some(target), Some(idx)) = (self.target_price(book), s.order_index) {
                        let drift = ((target - s.price) / self.market.price_tick()).abs();
                        if drift > Decimal::from(self.cfg.reprice_ticks) {
                            return Action::Cancel { order_index: idx };
                        }
                    }
                }
                Action::None
            }
        }
    }

    /// Record that the runner submitted a slice.
    pub fn on_placed(&mut self, client_order_index: i64, size: Decimal, price: Decimal, now: i64) {
        self.active = Some(Slice {
            client_order_index,
            order_index: None,
            size,
            price,
            filled: Decimal::ZERO,
            sent_at_ms: now,
            cancel_requested: false,
        });
        self.slices_sent += 1;
        self.last_sent_ms = now;
    }

    pub fn on_cancel_sent(&mut self) {
        if let Some(s) = &mut self.active {
            s.cancel_requested = true;
        }
    }

    /// Apply an own-order update. Returns `true` when the active slice was closed.
    pub fn on_order(&mut self, o: &Order) -> bool {
        let Some(s) = &mut self.active else {
            return false;
        };
        let ours = o.client_order_index == s.client_order_index
            || (s.order_index.is_some() && Some(o.order_index) == s.order_index);
        if !ours || o.market_index != self.market.market_id {
            return false;
        }
        if o.order_index != 0 {
            s.order_index = Some(o.order_index);
        }
        let filled_now = o.filled().min(s.size);
        if filled_now > s.filled {
            let delta = filled_now - s.filled;
            self.filled += delta;
            self.notional += delta * if o.price.is_zero() { s.price } else { o.price };
            s.filled = filled_now;
        }
        let slice_full = s.filled >= s.size * self.cfg.refill_at;
        if o.is_terminal() || slice_full {
            info!(
                coi = s.client_order_index,
                filled = %s.filled,
                size = %s.size,
                status = %o.status,
                total_filled = %self.filled,
                "iceberg slice closed"
            );
            self.active = None;
            return true;
        }
        false
    }

    /// If a slice has had no acknowledgement for `timeout`, treat it as lost so the runner can
    /// reconcile via REST instead of waiting forever.
    pub fn stale_slice(&self, now: i64, timeout: Duration) -> Option<i64> {
        self.active
            .as_ref()
            .filter(|s| s.order_index.is_none() && now - s.sent_at_ms > timeout.as_millis() as i64)
            .map(|s| s.client_order_index)
    }

    pub fn avg_price(&self) -> Option<Decimal> {
        if self.filled.is_zero() {
            None
        } else {
            Some(self.notional / self.filled)
        }
    }
}

#[derive(Debug, Clone)]
pub struct IcebergReport {
    pub filled: Decimal,
    pub remaining: Decimal,
    pub avg_price: Option<Decimal>,
    pub slices_sent: u32,
}

pub struct IcebergExecutor<'a> {
    gateway: &'a OrderGateway,
    ws_url: String,
}

impl<'a> IcebergExecutor<'a> {
    pub fn new(gateway: &'a OrderGateway, ws_url: impl Into<String>) -> Self {
        IcebergExecutor {
            gateway,
            ws_url: ws_url.into(),
        }
    }

    /// Runs until the total is filled, `deadline` passes, or Ctrl-C. Always tries to cancel the
    /// resting slice on exit.
    pub async fn run(
        &self,
        cfg: IcebergConfig,
        deadline: Option<Duration>,
    ) -> Result<IcebergReport> {
        let market = self.gateway.markets().get(&cfg.symbol)?.clone();
        let mut planner = IcebergPlanner::new(cfg.clone(), market.clone())?;

        let mut ws_cfg = WsConfig::new(self.ws_url.clone());
        ws_cfg.order_books = vec![market.market_id];
        ws_cfg.account_index = Some(self.gateway.account_index());
        ws_cfg.account_orders = true;
        ws_cfg.auth_token = Some(self.gateway.auth_token(Duration::from_secs(8 * 3600))?);
        let mut ws = ws::spawn(ws_cfg);

        let started = now_ms();
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        let mut reconcile = tokio::time::interval(Duration::from_secs(15));
        let mut seen: HashMap<i64, Order> = HashMap::new();
        let result = loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    warn!("ctrl-c: stopping iceberg");
                    break Ok(());
                }
                ev = ws.events.recv() => {
                    let Some(ev) = ev else { break Err(Error::Ws("stream closed".into())); };
                    match ev {
                        WsEvent::Orders { orders, .. } => {
                            for o in orders {
                                seen.insert(o.client_order_index, o.clone());
                                planner.on_order(&o);
                            }
                        }
                        WsEvent::Reconnected => {
                            // Snapshot will follow; also reconcile explicitly.
                            if let Err(e) = self.reconcile(&mut planner).await { warn!(error = %e, "reconcile failed"); }
                        }
                        WsEvent::Error { code, message }
                            if code == 401 || message.to_ascii_lowercase().contains("auth") =>
                        {
                            break Err(Error::Ws(format!("auth rejected: {message}")));
                        }
                        _ => {}
                    }
                }
                _ = reconcile.tick() => {
                    if let Err(e) = self.reconcile(&mut planner).await { warn!(error = %e, "reconcile failed"); }
                }
                _ = tick.tick() => {
                    let now = now_ms();
                    if let Some(d) = deadline {
                        if now - started > d.as_millis() as i64 {
                            warn!("deadline reached");
                            break Ok(());
                        }
                    }
                    let Some(book) = ws.book(market.market_id).await else { continue };
                    match planner.on_tick(&book, now) {
                        Action::Done => break Ok(()),
                        Action::None => {}
                        Action::Cancel { order_index } => {
                            match self.gateway.cancel(&cfg.symbol, order_index).await {
                                Ok(_) => planner.on_cancel_sent(),
                                Err(e) => warn!(error = %e, "cancel failed"),
                            }
                        }
                        Action::Place { size, price } => {
                            let coi = self.gateway.next_client_order_index();
                            let order = NewOrder {
                                symbol: cfg.symbol.clone(),
                                side: cfg.side,
                                size,
                                price,
                                kind: OrderKind::Limit { tif: cfg.tif, expiry: Duration::from_secs(3600) },
                                reduce_only: cfg.reduce_only,
                                client_order_index: Some(coi),
                            };
                            match self.gateway.submit(&order).await {
                                Ok(s) => planner.on_placed(coi, s.size, s.price, now),
                                Err(e) => {
                                    warn!(error = %e, "slice submit failed; backing off");
                                    planner.last_sent_ms = now + 2_000;
                                }
                            }
                        }
                    }
                }
            }
        };
        ws.shutdown();
        if let Some(idx) = planner.active_order_index() {
            if let Err(e) = self.gateway.cancel(&cfg.symbol, idx).await {
                warn!(error = %e, order_index = idx, "failed to cancel resting slice on exit");
            }
        }
        result?;
        Ok(IcebergReport {
            filled: planner.filled,
            remaining: planner.remaining(),
            avg_price: planner.avg_price(),
            slices_sent: planner.slices_sent,
        })
    }

    /// Pull open orders over REST and feed them to the planner; marks a slice closed if it
    /// is no longer open and the stream missed the terminal update.
    async fn reconcile(&self, planner: &mut IcebergPlanner) -> Result<()> {
        if self.gateway.dry_run {
            return Ok(());
        }
        let open = self.gateway.active_orders(&planner.cfg.symbol).await?;
        let Some(coi) = planner.active_client_order_index() else {
            return Ok(());
        };
        match open.iter().find(|o| o.client_order_index == coi) {
            Some(o) => {
                planner.on_order(o);
            }
            None => {
                if planner
                    .stale_slice(now_ms(), Duration::from_secs(10))
                    .is_some()
                {
                    warn!(
                        coi,
                        "slice not found among open orders after 10s; treating as closed"
                    );
                    let mut o = Order {
                        client_order_index: coi,
                        market_index: planner.market.market_id,
                        status: "canceled".into(),
                        ..Default::default()
                    };
                    o.order_index = planner.active_order_index().unwrap_or(0);
                    planner.on_order(&o);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Level;
    use rust_decimal_macros::dec;

    fn eth() -> MarketInfo {
        serde_json::from_str(
            r#"{"symbol":"ETH","market_id":0,"size_decimals":4,"price_decimals":2,
                "min_base_amount":"0.0020","min_quote_amount":"10.000000"}"#,
        )
        .unwrap()
    }

    fn book(bid: &str, ask: &str) -> OrderBook {
        let mut b = OrderBook::new(0);
        b.replace(
            &[Level {
                price: bid.parse().unwrap(),
                size: dec!(10),
            }],
            &[Level {
                price: ask.parse().unwrap(),
                size: dec!(10),
            }],
        );
        b
    }

    fn cfg(side: Side, pricing: Pricing) -> IcebergConfig {
        IcebergConfig {
            symbol: "ETH".into(),
            side,
            total_size: dec!(1.0),
            display_size: dec!(0.25),
            pricing,
            tif: TimeInForce::PostOnly,
            refill_at: Decimal::ONE,
            min_interval: Duration::from_millis(0),
            reprice_ticks: 2,
            jitter_pct: 0,
            reduce_only: false,
        }
    }

    fn fill(coi: i64, idx: i64, initial: &str, remaining: &str, status: &str) -> Order {
        Order {
            order_index: idx,
            client_order_index: coi,
            market_index: 0,
            initial_base_amount: initial.parse().unwrap(),
            remaining_base_amount: remaining.parse().unwrap(),
            price: dec!(2000),
            status: status.into(),
            ..Default::default()
        }
    }

    #[test]
    fn slices_until_total_filled() {
        let mut p = IcebergPlanner::new(cfg(Side::Buy, Pricing::Fixed(dec!(2000))), eth()).unwrap();
        let b = book("1999", "2001");
        let mut coi = 100;
        let mut placed = 0;
        for _ in 0..10 {
            match p.on_tick(&b, 1_000) {
                Action::Place { size, price } => {
                    assert_eq!(price, dec!(2000));
                    assert_eq!(size, dec!(0.25));
                    p.on_placed(coi, size, price, 1_000);
                    placed += 1;
                    assert_eq!(p.on_tick(&b, 1_001), Action::None); // waits while slice rests
                    assert!(!p.on_order(&fill(coi, coi * 10, "0.25", "0.10", "open")));
                    assert!(p.on_order(&fill(coi, coi * 10, "0.25", "0", "filled")));
                    coi += 1;
                }
                Action::Done => break,
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(placed, 4);
        assert!(p.is_done());
        assert_eq!(p.filled, dec!(1.0));
        assert_eq!(p.avg_price(), Some(dec!(2000)));
        assert_eq!(p.on_tick(&b, 2_000), Action::Done);
    }

    #[test]
    fn partial_fill_then_cancel_refills_only_remaining() {
        let mut p =
            IcebergPlanner::new(cfg(Side::Sell, Pricing::Fixed(dec!(2000))), eth()).unwrap();
        let b = book("1999", "2001");
        let Action::Place { size, price } = p.on_tick(&b, 0) else {
            panic!()
        };
        p.on_placed(1, size, price, 0);
        assert!(p.on_order(&fill(1, 11, "0.25", "0.15", "canceled")));
        assert_eq!(p.filled, dec!(0.10));
        assert_eq!(p.remaining(), dec!(0.90));
        let Action::Place { size, .. } = p.on_tick(&b, 1) else {
            panic!()
        };
        assert_eq!(size, dec!(0.25));
    }

    #[test]
    fn peg_joins_best_and_reprices_on_drift() {
        let mut p =
            IcebergPlanner::new(cfg(Side::Buy, Pricing::Peg { offset_ticks: 1 }), eth()).unwrap();
        let b = book("1999.00", "2001.00");
        let Action::Place { price, size } = p.on_tick(&b, 0) else {
            panic!()
        };
        assert_eq!(price, dec!(1999.01));
        p.on_placed(1, size, price, 0);
        p.on_order(&fill(1, 11, "0.25", "0.25", "open"));
        // Market moves 1 tick: within tolerance, keep resting.
        assert_eq!(p.on_tick(&book("1999.01", "2001.00"), 1), Action::None);
        // Market moves 5 ticks: cancel to reprice.
        assert_eq!(
            p.on_tick(&book("1999.05", "2001.05"), 2),
            Action::Cancel { order_index: 11 }
        );
        p.on_cancel_sent();
        assert_eq!(p.on_tick(&book("1999.05", "2001.05"), 3), Action::None);
        assert!(p.on_order(&fill(1, 11, "0.25", "0.25", "canceled")));
        let Action::Place { price, .. } = p.on_tick(&book("1999.05", "2001.05"), 4) else {
            panic!()
        };
        assert_eq!(price, dec!(1999.06));
    }

    #[test]
    fn peg_never_crosses() {
        let p =
            IcebergPlanner::new(cfg(Side::Buy, Pricing::Peg { offset_ticks: 50 }), eth()).unwrap();
        assert_eq!(
            p.target_price(&book("1999.00", "1999.10")).unwrap(),
            dec!(1999.09)
        );
        let p =
            IcebergPlanner::new(cfg(Side::Sell, Pricing::Peg { offset_ticks: 50 }), eth()).unwrap();
        assert_eq!(
            p.target_price(&book("1999.00", "1999.10")).unwrap(),
            dec!(1999.01)
        );
    }

    #[test]
    fn no_crumb_below_minimum() {
        let mut c = cfg(Side::Buy, Pricing::Fixed(dec!(2000)));
        c.total_size = dec!(0.2510);
        c.display_size = dec!(0.25);
        let mut p = IcebergPlanner::new(c, eth()).unwrap();
        assert_eq!(p.next_slice_size(), dec!(0.2510));
    }

    #[test]
    fn ignores_other_orders() {
        let mut p = IcebergPlanner::new(cfg(Side::Buy, Pricing::Fixed(dec!(2000))), eth()).unwrap();
        p.on_placed(1, dec!(0.25), dec!(2000), 0);
        assert!(!p.on_order(&fill(99, 990, "1", "0", "filled")));
        assert_eq!(p.filled, Decimal::ZERO);
    }

    #[test]
    fn validation() {
        let mut c = cfg(Side::Buy, Pricing::Fixed(dec!(1)));
        c.display_size = dec!(2);
        assert!(IcebergPlanner::new(c.clone(), eth()).is_err());
        c.display_size = dec!(0.001);
        assert!(IcebergPlanner::new(c.clone(), eth()).is_err());
        c.display_size = dec!(0.25);
        c.tif = TimeInForce::Ioc;
        assert!(IcebergPlanner::new(c, eth()).is_err());
    }
}
