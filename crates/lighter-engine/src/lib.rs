//! Live trading engine for the [Lighter](https://lighter.xyz) perpetual DEX.
//!
//! Layers, bottom to top:
//!
//! * [`signer`]  – offline transaction signing (wraps the `lighters` port of `lighter-go`).
//! * [`rest`]    – async REST client (`/api/v1/*`): market metadata, candles, nonce, `sendTx`.
//! * [`ws`]      – WebSocket stream: order books, own orders, account state, auto-reconnect.
//! * [`nonce`]   – per-API-key nonce manager (optimistic, resyncs on failure).
//! * [`gateway`] – decimal-aware order gateway: rounding, min-size checks, sign + submit.
//! * [`exec`]    – execution algos built on the above: iceberg slicing and two-leg pair orders.
//!
//! Everything that touches the network is `async` (tokio). Nothing here holds the private key
//! except the signer; the key is read from `LIGHTER_API_KEY_PRIVATE` by [`config::Config`].

pub mod binance;
pub mod candles;
pub mod config;
pub mod error;
pub mod exec;
pub mod gateway;
pub mod nonce;
pub mod rest;
pub mod signer;
pub mod types;
pub mod ws;

pub use config::{Config, Network};
pub use error::{Error, Result};
pub use gateway::{MarketCatalog, NewOrder, OrderGateway, OrderKind, Submitted};
pub use rest::RestClient;
pub use signer::{LightersSigner, OrderParams, OrderType, SignedTx, Signer, TimeInForce};
pub use types::{Candle, MarketInfo, Order, OrderBook, Side};
