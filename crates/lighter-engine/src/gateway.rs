//! Decimal-aware order gateway: turns human orders (symbol, side, decimal price/size) into
//! validated protocol orders, signs them, and submits them (single or batched).

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rust_decimal::Decimal;
use tracing::{info, warn};

use crate::error::{Error, Result};
use crate::nonce::NonceManager;
use crate::rest::RestClient;
use crate::signer::{now_ms, OrderParams, OrderType, SignedTx, Signer, TimeInForce};
use crate::types::{MarketInfo, Side};

/// Symbol <-> market lookup loaded from `orderBookDetails`.
#[derive(Debug, Clone, Default)]
pub struct MarketCatalog {
    by_symbol: HashMap<String, MarketInfo>,
    by_id: HashMap<i64, MarketInfo>,
}

impl MarketCatalog {
    pub fn from_markets(markets: Vec<MarketInfo>) -> Self {
        let mut c = MarketCatalog::default();
        for m in markets {
            c.by_symbol.insert(m.symbol.to_ascii_uppercase(), m.clone());
            c.by_id.insert(m.market_id, m);
        }
        c
    }

    pub async fn load(rest: &RestClient) -> Result<Self> {
        Ok(Self::from_markets(rest.order_book_details(None).await?))
    }

    /// Looks a market up by symbol (case-insensitive) or by numeric id.
    pub fn get(&self, key: &str) -> Result<&MarketInfo> {
        if let Some(m) = self.by_symbol.get(&key.to_ascii_uppercase()) {
            return Ok(m);
        }
        if let Ok(id) = key.parse::<i64>() {
            if let Some(m) = self.by_id.get(&id) {
                return Ok(m);
            }
        }
        Err(Error::UnknownMarket(key.to_string()))
    }

    pub fn by_id(&self, id: i64) -> Option<&MarketInfo> {
        self.by_id.get(&id)
    }

    pub fn all(&self) -> impl Iterator<Item = &MarketInfo> {
        self.by_id.values()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderKind {
    /// Resting limit order. `expiry` is how long it rests (clamped to Lighter's 5m..30d).
    Limit { tif: TimeInForce, expiry: Duration },
    /// Immediate-or-cancel at `price` as the worst acceptable fill (Lighter market order).
    Market,
}

impl OrderKind {
    pub fn limit_gtt(expiry: Duration) -> Self {
        OrderKind::Limit {
            tif: TimeInForce::Gtt,
            expiry,
        }
    }
    pub fn post_only(expiry: Duration) -> Self {
        OrderKind::Limit {
            tif: TimeInForce::PostOnly,
            expiry,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewOrder {
    pub symbol: String,
    pub side: Side,
    pub size: Decimal,
    /// Limit price, or worst acceptable price for `OrderKind::Market`.
    pub price: Decimal,
    pub kind: OrderKind,
    pub reduce_only: bool,
    /// Caller-chosen id echoed back on the `account_all_orders` stream. Auto-assigned if `None`.
    pub client_order_index: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct Submitted {
    pub symbol: String,
    pub client_order_index: i64,
    pub params: OrderParams,
    pub tx_hash: String,
    pub nonce: i64,
    /// Rounded decimal values actually sent.
    pub size: Decimal,
    pub price: Decimal,
}

pub struct OrderGateway {
    rest: RestClient,
    signer: Arc<dyn Signer>,
    nonce: NonceManager,
    markets: MarketCatalog,
    next_coi: AtomicI64,
    /// Sign but do not submit; `tx_hash` is the local hash.
    pub dry_run: bool,
}

const MIN_EXPIRY: Duration = Duration::from_secs(5 * 60 + 30);
const MAX_EXPIRY: Duration = Duration::from_secs(30 * 24 * 3600 - 60);

impl OrderGateway {
    pub fn new(
        rest: RestClient,
        signer: Arc<dyn Signer>,
        markets: MarketCatalog,
        dry_run: bool,
    ) -> Self {
        let nonce = NonceManager::new(rest.clone(), signer.account_index(), signer.api_key_index());
        OrderGateway {
            rest,
            signer,
            nonce,
            markets,
            // Millisecond timestamp seed keeps ids unique across restarts.
            next_coi: AtomicI64::new(now_ms()),
            dry_run,
        }
    }

    pub fn markets(&self) -> &MarketCatalog {
        &self.markets
    }

    pub fn rest(&self) -> &RestClient {
        &self.rest
    }

    pub fn signer(&self) -> &Arc<dyn Signer> {
        &self.signer
    }

    pub fn account_index(&self) -> i64 {
        self.signer.account_index()
    }

    pub fn next_client_order_index(&self) -> i64 {
        self.next_coi.fetch_add(1, Ordering::Relaxed)
    }

    /// Auth token valid for `ttl` (max 8h) for private REST / WS channels.
    pub fn auth_token(&self, ttl: Duration) -> Result<String> {
        let ttl = ttl.min(Duration::from_secs(8 * 3600 - 60));
        self.signer
            .auth_token(chrono::Utc::now().timestamp() + ttl.as_secs() as i64)
    }

    /// Rounds, validates against market minimums and converts to protocol units.
    pub fn build_order(&self, o: &NewOrder) -> Result<(OrderParams, Decimal, Decimal)> {
        let m = self.markets.get(&o.symbol)?;
        if m.is_frozen {
            return Err(Error::InvalidOrder(format!("{} is frozen", m.symbol)));
        }
        let size = m.round_size(o.size);
        let price = m.round_price(o.price, o.side);
        m.validate_min(size, price)?;
        let (order_type, tif, expiry_ms) = match &o.kind {
            OrderKind::Market => (OrderType::Market, TimeInForce::Ioc, 0),
            OrderKind::Limit { tif, expiry } => {
                if *tif == TimeInForce::Ioc {
                    (OrderType::Limit, TimeInForce::Ioc, 0)
                } else {
                    let e = (*expiry).clamp(MIN_EXPIRY, MAX_EXPIRY);
                    (OrderType::Limit, *tif, now_ms() + e.as_millis() as i64)
                }
            }
        };
        let params = OrderParams {
            market_index: m.market_index()?,
            client_order_index: o
                .client_order_index
                .unwrap_or_else(|| self.next_client_order_index()),
            base_amount: m.size_to_units(size)?,
            price: m.price_to_units(price)?,
            is_ask: o.side.is_ask(),
            order_type,
            time_in_force: tif,
            reduce_only: o.reduce_only,
            trigger_price: 0,
            order_expiry_ms: expiry_ms,
        };
        Ok((params, size, price))
    }

    async fn send(&self, tx: &SignedTx) -> Result<String> {
        if self.dry_run {
            info!(tx_type = tx.tx_type, hash = %tx.tx_hash, "dry-run: not sending");
            return Ok(tx.tx_hash.clone());
        }
        match self.rest.send_tx(tx.tx_type, &tx.tx_info, true).await {
            Ok(r) => Ok(r.tx_hash),
            Err(e) => {
                warn!(error = %e, "sendTx failed; resyncing nonce");
                self.nonce.resync().await;
                Err(e)
            }
        }
    }

    /// Signs and submits one order.
    pub async fn submit(&self, o: &NewOrder) -> Result<Submitted> {
        let (params, size, price) = self.build_order(o)?;
        let nonce = self.nonce.next().await?;
        let tx = self.signer.sign_create_order(&params, nonce)?;
        let tx_hash = self.send(&tx).await?;
        info!(symbol = %o.symbol, side = %o.side, %size, %price, coi = params.client_order_index, %tx_hash, "order submitted");
        Ok(Submitted {
            symbol: o.symbol.clone(),
            client_order_index: params.client_order_index,
            params,
            tx_hash,
            nonce,
            size,
            price,
        })
    }

    /// Signs all orders with consecutive nonces and submits them in one `sendTxBatch`, so the
    /// legs reach the sequencer together. Any validation failure aborts before anything is sent.
    pub async fn submit_batch(&self, orders: &[NewOrder]) -> Result<Vec<Submitted>> {
        let built: Vec<_> = orders
            .iter()
            .map(|o| self.build_order(o))
            .collect::<Result<_>>()?;
        let nonces = self.nonce.reserve(built.len()).await?;
        let mut signed = Vec::with_capacity(built.len());
        for ((params, _, _), nonce) in built.iter().zip(&nonces) {
            signed.push(self.signer.sign_create_order(params, *nonce)?);
        }
        let hashes: Vec<String> = if self.dry_run {
            info!(n = signed.len(), "dry-run: not sending batch");
            signed.iter().map(|t| t.tx_hash.clone()).collect()
        } else {
            let txs: Vec<(u8, String)> = signed
                .iter()
                .map(|t| (t.tx_type, t.tx_info.clone()))
                .collect();
            match self.rest.send_tx_batch(&txs).await {
                Ok(r) => {
                    if r.tx_hash.len() == signed.len() {
                        r.tx_hash
                    } else {
                        signed.iter().map(|t| t.tx_hash.clone()).collect()
                    }
                }
                Err(e) => {
                    warn!(error = %e, "sendTxBatch failed; resyncing nonce");
                    self.nonce.resync().await;
                    return Err(e);
                }
            }
        };
        Ok(orders
            .iter()
            .zip(built)
            .zip(nonces)
            .zip(hashes)
            .map(|(((o, (params, size, price)), nonce), tx_hash)| Submitted {
                symbol: o.symbol.clone(),
                client_order_index: params.client_order_index,
                params,
                tx_hash,
                nonce,
                size,
                price,
            })
            .collect())
    }

    pub async fn cancel(&self, symbol: &str, order_index: i64) -> Result<String> {
        let m = self.markets.get(symbol)?;
        let nonce = self.nonce.next().await?;
        let tx = self
            .signer
            .sign_cancel_order(m.market_index()?, order_index, nonce)?;
        let h = self.send(&tx).await?;
        info!(%symbol, order_index, tx_hash = %h, "cancel submitted");
        Ok(h)
    }

    /// Cancels every open order of the account across all markets.
    pub async fn cancel_all(&self) -> Result<String> {
        let nonce = self.nonce.next().await?;
        let tx = self.signer.sign_cancel_all(0, 0, nonce)?;
        let h = self.send(&tx).await?;
        info!(tx_hash = %h, "cancel-all submitted");
        Ok(h)
    }

    pub async fn modify(
        &self,
        symbol: &str,
        order_index: i64,
        size: Decimal,
        price: Decimal,
        side: Side,
    ) -> Result<String> {
        let m = self.markets.get(symbol)?;
        let size = m.round_size(size);
        let price = m.round_price(price, side);
        m.validate_min(size, price)?;
        let nonce = self.nonce.next().await?;
        let tx = self.signer.sign_modify_order(
            m.market_index()?,
            order_index,
            m.size_to_units(size)?,
            m.price_to_units(price)?,
            0,
            nonce,
        )?;
        self.send(&tx).await
    }

    /// Open orders for one market via REST (used to reconcile with the WS stream).
    pub async fn active_orders(&self, symbol: &str) -> Result<Vec<crate::types::Order>> {
        let m = self.markets.get(symbol)?;
        let token = self.auth_token(Duration::from_secs(600))?;
        self.rest
            .active_orders(self.account_index(), m.market_id, &token)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signer::LightersSigner;
    use rust_decimal_macros::dec;

    fn gateway() -> OrderGateway {
        let m: MarketInfo = serde_json::from_str(
            r#"{"symbol":"ETH","market_id":0,"size_decimals":4,"price_decimals":2,
                "min_base_amount":"0.0020","min_quote_amount":"10.000000"}"#,
        )
        .unwrap();
        let signer = LightersSigner::new(
            "0x07000000000000000300000000000000000000000000000000000000000000000000000000000000",
            1,
            4,
            304,
        )
        .unwrap();
        OrderGateway::new(
            RestClient::new("https://testnet.zklighter.elliot.ai").unwrap(),
            Arc::new(signer),
            MarketCatalog::from_markets(vec![m]),
            true,
        )
    }

    #[test]
    fn builds_limit_order_in_units() {
        let g = gateway();
        let (p, size, price) = g
            .build_order(&NewOrder {
                symbol: "eth".into(),
                side: Side::Buy,
                size: dec!(0.01234),
                price: dec!(1999.999),
                kind: OrderKind::post_only(Duration::from_secs(3600)),
                reduce_only: false,
                client_order_index: Some(5),
            })
            .unwrap();
        assert_eq!(size, dec!(0.0123));
        assert_eq!(price, dec!(1999.99));
        assert_eq!(p.base_amount, 123);
        assert_eq!(p.price, 199_999);
        assert_eq!(p.time_in_force, TimeInForce::PostOnly);
        assert!(p.order_expiry_ms > now_ms());
        assert_eq!(p.client_order_index, 5);
    }

    #[test]
    fn market_order_is_ioc_with_zero_expiry() {
        let g = gateway();
        let (p, _, _) = g
            .build_order(&NewOrder {
                symbol: "ETH".into(),
                side: Side::Sell,
                size: dec!(0.01),
                price: dec!(1500),
                kind: OrderKind::Market,
                reduce_only: true,
                client_order_index: None,
            })
            .unwrap();
        assert_eq!(p.order_type, OrderType::Market);
        assert_eq!(p.time_in_force, TimeInForce::Ioc);
        assert_eq!(p.order_expiry_ms, 0);
        assert!(p.is_ask);
        assert!(p.reduce_only);
    }

    #[test]
    fn rejects_below_minimum() {
        let g = gateway();
        assert!(g
            .build_order(&NewOrder {
                symbol: "ETH".into(),
                side: Side::Buy,
                size: dec!(0.001),
                price: dec!(2000),
                kind: OrderKind::Market,
                reduce_only: false,
                client_order_index: None,
            })
            .is_err());
        assert!(matches!(
            g.build_order(&NewOrder {
                symbol: "DOGE".into(),
                side: Side::Buy,
                size: dec!(1),
                price: dec!(1),
                kind: OrderKind::Market,
                reduce_only: false,
                client_order_index: None,
            }),
            Err(Error::UnknownMarket(_))
        ));
    }

    #[test]
    fn client_order_indexes_are_unique() {
        let g = gateway();
        let a = g.next_client_order_index();
        let b = g.next_client_order_index();
        assert!(b > a);
    }
}
