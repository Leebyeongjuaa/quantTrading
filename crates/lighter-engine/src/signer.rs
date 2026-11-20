//! Offline transaction signing.
//!
//! The [`Signer`] trait is the only thing the rest of the engine depends on, so the backend
//! can be swapped (e.g. for an FFI binding to Lighter's official Go shared library). The
//! default [`LightersSigner`] wraps the `lighters` crate, a pure-Rust port of `lighter-go`
//! whose transaction hashes and Schnorr signatures match the Go reference vectors.

use lighters::transactions::TxInfo;
use lighters::{OrderReq, TransactOpts, TxClient};

use crate::error::{Error, Result};

/// Protocol order types (`Type` field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OrderType {
    Limit = 0,
    Market = 1,
    StopLoss = 2,
    StopLossLimit = 3,
    TakeProfit = 4,
    TakeProfitLimit = 5,
    Twap = 6,
}

/// Protocol time-in-force (`TimeInForce` field).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[repr(u8)]
pub enum TimeInForce {
    /// Fill what crosses now, cancel the rest. Used for market orders.
    Ioc = 0,
    /// Rests until `order_expiry` (5 minutes .. 30 days ahead).
    Gtt = 1,
    /// Rests; rejected instead of taking liquidity.
    PostOnly = 2,
}

/// Fully specified order in protocol integer units. Build it from decimals with
/// [`crate::gateway::OrderGateway::build_order`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderParams {
    pub market_index: i16,
    pub client_order_index: i64,
    /// `size * 10^size_decimals`
    pub base_amount: i64,
    /// `price * 10^price_decimals`; for IOC/market orders this is the worst acceptable price.
    pub price: u32,
    pub is_ask: bool,
    pub order_type: OrderType,
    pub time_in_force: TimeInForce,
    pub reduce_only: bool,
    pub trigger_price: u32,
    /// Unix ms. Must be 0 for IOC.
    pub order_expiry_ms: i64,
}

#[derive(Debug, Clone)]
pub struct SignedTx {
    pub tx_type: u8,
    /// JSON body accepted verbatim by `sendTx`'s `tx_info` field.
    pub tx_info: String,
    pub tx_hash: String,
    pub nonce: i64,
}

pub trait Signer: Send + Sync {
    fn account_index(&self) -> i64;
    fn api_key_index(&self) -> u8;
    fn public_key_hex(&self) -> String;
    fn sign_create_order(&self, p: &OrderParams, nonce: i64) -> Result<SignedTx>;
    fn sign_cancel_order(
        &self,
        market_index: i16,
        order_index: i64,
        nonce: i64,
    ) -> Result<SignedTx>;
    /// `time_in_force`: 0 immediate, 1 scheduled (at `time_ms`), 2 abort scheduled.
    fn sign_cancel_all(&self, time_in_force: u8, time_ms: i64, nonce: i64) -> Result<SignedTx>;
    fn sign_modify_order(
        &self,
        market_index: i16,
        order_index: i64,
        base_amount: i64,
        price: u32,
        trigger_price: u32,
        nonce: i64,
    ) -> Result<SignedTx>;
    /// `deadline:account:apiKey:signature` token for private REST/WS endpoints.
    fn auth_token(&self, deadline_unix_secs: i64) -> Result<String>;
}

pub struct LightersSigner {
    inner: TxClient,
}

impl LightersSigner {
    pub fn new(
        private_key_hex: &str,
        account_index: i64,
        api_key_index: u8,
        chain_id: u32,
    ) -> Result<Self> {
        if api_key_index == 255 {
            return Err(Error::Config("api key index 255 is reserved".into()));
        }
        let inner = TxClient::new(private_key_hex, account_index, api_key_index, chain_id)?;
        Ok(LightersSigner { inner })
    }

    /// Generates a fresh API key pair: `(private_key_hex, public_key_hex)`. The public key
    /// must be registered on-chain with a `ChangePubKey` tx (signed by the L1 wallet) before use.
    pub fn generate_key() -> (String, String) {
        lighters::generate_api_key()
    }

    fn opts(nonce: i64) -> TransactOpts {
        TransactOpts {
            nonce: Some(nonce),
            ..Default::default()
        }
    }

    fn pack<T: TxInfo>(tx: &T, nonce: i64) -> Result<SignedTx> {
        Ok(SignedTx {
            tx_type: tx.tx_type(),
            tx_info: tx.to_json()?,
            tx_hash: tx.tx_hash().to_string(),
            nonce,
        })
    }
}

impl Signer for LightersSigner {
    fn account_index(&self) -> i64 {
        self.inner.account_index()
    }

    fn api_key_index(&self) -> u8 {
        self.inner.api_key_index()
    }

    fn public_key_hex(&self) -> String {
        self.inner.key_manager().public_key_hex()
    }

    fn sign_create_order(&self, p: &OrderParams, nonce: i64) -> Result<SignedTx> {
        if p.time_in_force == TimeInForce::Ioc && p.order_expiry_ms != 0 {
            return Err(Error::InvalidOrder(
                "IOC orders must have order_expiry 0".into(),
            ));
        }
        let tx = self.inner.create_order(
            OrderReq {
                market_index: p.market_index,
                client_order_index: p.client_order_index,
                base_amount: p.base_amount,
                price: p.price,
                is_ask: p.is_ask as u8,
                order_type: p.order_type as u8,
                time_in_force: p.time_in_force as u8,
                reduce_only: p.reduce_only as u8,
                trigger_price: p.trigger_price,
                order_expiry: p.order_expiry_ms,
            },
            &Self::opts(nonce),
        )?;
        Self::pack(&tx, nonce)
    }

    fn sign_cancel_order(
        &self,
        market_index: i16,
        order_index: i64,
        nonce: i64,
    ) -> Result<SignedTx> {
        let tx = self
            .inner
            .cancel_order(market_index, order_index, &Self::opts(nonce))?;
        Self::pack(&tx, nonce)
    }

    fn sign_cancel_all(&self, time_in_force: u8, time_ms: i64, nonce: i64) -> Result<SignedTx> {
        let tx = self
            .inner
            .cancel_all_orders(time_in_force, time_ms, &Self::opts(nonce))?;
        Self::pack(&tx, nonce)
    }

    fn sign_modify_order(
        &self,
        market_index: i16,
        order_index: i64,
        base_amount: i64,
        price: u32,
        trigger_price: u32,
        nonce: i64,
    ) -> Result<SignedTx> {
        let tx = self.inner.modify_order(
            market_index,
            order_index,
            base_amount,
            price,
            trigger_price,
            &Self::opts(nonce),
        )?;
        Self::pack(&tx, nonce)
    }

    fn auth_token(&self, deadline_unix_secs: i64) -> Result<String> {
        Ok(self
            .inner
            .auth_token(deadline_unix_secs, &TransactOpts::default())?)
    }
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str =
        "0x07000000000000000300000000000000000000000000000000000000000000000000000000000000";

    fn signer() -> LightersSigner {
        LightersSigner::new(KEY, 1, 0, 304).unwrap()
    }

    /// Hash and JSON layout must match the Go reference vector shipped with `lighters`
    /// (`tests/vectors.json`, name `create_order`, generated by `lighter-go`).
    #[test]
    fn create_order_matches_go_vector() {
        let tx = signer()
            .inner
            .create_order(
                OrderReq {
                    market_index: 0,
                    client_order_index: 1,
                    base_amount: 1000,
                    price: 50_000,
                    is_ask: 0,
                    order_type: 0,
                    time_in_force: 1,
                    reduce_only: 0,
                    trigger_price: 0,
                    order_expiry: 99_999_999,
                },
                &TransactOpts {
                    nonce: Some(42),
                    expired_at: Some(1_700_000_000_000),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            tx.tx_hash(),
            "df8102da5ffa45e0b14a4fffdcab19d21d52039ba9036396f3a3c11c21145baaebece4f51e983629"
        );
        let json: serde_json::Value = serde_json::from_str(&tx.to_json().unwrap()).unwrap();
        assert_eq!(json["AccountIndex"], 1);
        assert_eq!(json["MarketIndex"], 0);
        assert_eq!(json["BaseAmount"], 1000);
        assert_eq!(json["Price"], 50000);
        assert_eq!(json["TimeInForce"], 1);
        assert_eq!(json["Nonce"], 42);
        assert_eq!(json["ExpiredAt"], 1_700_000_000_000i64);
        assert!(json["Sig"].as_str().map(|s| !s.is_empty()).unwrap_or(false));
    }

    #[test]
    fn signature_verifies_with_own_public_key() {
        let s = signer();
        let p = OrderParams {
            market_index: 0,
            client_order_index: 9,
            base_amount: 100,
            price: 310_000,
            is_ask: true,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::PostOnly,
            reduce_only: false,
            trigger_price: 0,
            order_expiry_ms: now_ms() + 3_600_000,
        };
        let signed = s.sign_create_order(&p, 7).unwrap();
        assert_eq!(signed.tx_type, 14);
        let v: serde_json::Value = serde_json::from_str(&signed.tx_info).unwrap();
        let sig = lighters_base64_decode(v["Sig"].as_str().unwrap());
        let hash = hex_decode(&signed.tx_hash);
        assert!(s.inner.key_manager().verify(&hash, &sig));
        assert!(lighters::signer::verify(
            &hash,
            &sig,
            &s.inner.key_manager().public_key_bytes()
        ));
    }

    #[test]
    fn ioc_requires_zero_expiry() {
        let s = signer();
        let p = OrderParams {
            market_index: 0,
            client_order_index: 1,
            base_amount: 100,
            price: 1,
            is_ask: false,
            order_type: OrderType::Market,
            time_in_force: TimeInForce::Ioc,
            reduce_only: false,
            trigger_price: 0,
            order_expiry_ms: 5,
        };
        assert!(s.sign_create_order(&p, 1).is_err());
    }

    #[test]
    fn auth_token_format() {
        let t = signer().auth_token(1_800_000_000).unwrap();
        let parts: Vec<&str> = t.split(':').collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], "1800000000");
        assert_eq!(parts[1], "1");
        assert_eq!(parts[2], "0");
        assert_eq!(parts[3].len(), 160); // 80-byte Schnorr signature, hex
    }

    fn hex_decode(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn lighters_base64_decode(s: &str) -> Vec<u8> {
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = Vec::new();
        let mut buf = 0u32;
        let mut bits = 0;
        for &c in s.as_bytes() {
            if c == b'=' {
                break;
            }
            let v = T.iter().position(|&t| t == c).unwrap() as u32;
            buf = (buf << 6) | v;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buf >> bits) as u8);
                buf &= (1 << bits) - 1;
            }
        }
        out
    }
}
