//! WebSocket market-data and account stream (`wss://<host>/stream`).
//!
//! A background task owns the socket: it subscribes after the server's `connected` frame,
//! answers pings, sends its own keepalive, maintains L2 books in a shared [`BookStore`] and
//! forwards typed [`WsEvent`]s over a channel. On any failure it reconnects with backoff,
//! clears local state and emits [`WsEvent::Reconnected`] so consumers treat the next
//! snapshots as authoritative.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::{mpsc, watch, RwLock};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use crate::signer::now_ms;
use crate::types::{Level, Order, OrderBook};

pub type BookStore = Arc<RwLock<HashMap<i64, OrderBook>>>;

#[derive(Debug, Clone)]
pub struct WsConfig {
    pub url: String,
    pub order_books: Vec<i64>,
    pub trades: Vec<i64>,
    pub account_index: Option<i64>,
    /// Needed for `account_all_orders`; produced by `Signer::auth_token`.
    pub auth_token: Option<String>,
    pub account_all: bool,
    pub account_orders: bool,
    pub ping_interval: Duration,
    pub idle_timeout: Duration,
    /// Forward every frame as `WsEvent::Raw` (debugging).
    pub emit_raw: bool,
}

impl WsConfig {
    pub fn new(url: impl Into<String>) -> Self {
        WsConfig {
            url: url.into(),
            order_books: Vec::new(),
            trades: Vec::new(),
            account_index: None,
            auth_token: None,
            account_all: false,
            account_orders: false,
            ping_interval: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(90),
            emit_raw: false,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Position {
    #[serde(default, deserialize_with = "crate::types::de_i64")]
    pub market_id: i64,
    #[serde(default)]
    pub symbol: String,
    #[serde(default, deserialize_with = "crate::types::de_i64")]
    pub sign: i64,
    #[serde(default, deserialize_with = "crate::types::de_decimal")]
    pub position: Decimal,
    #[serde(default, deserialize_with = "crate::types::de_decimal")]
    pub avg_entry_price: Decimal,
    #[serde(default, deserialize_with = "crate::types::de_decimal")]
    pub unrealized_pnl: Decimal,
}

impl Position {
    pub fn signed_size(&self) -> Decimal {
        if self.sign < 0 {
            -self.position.abs()
        } else {
            self.position.abs()
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PublicTrade {
    #[serde(default, deserialize_with = "crate::types::de_i64")]
    pub trade_id: i64,
    #[serde(default, deserialize_with = "crate::types::de_i64")]
    pub market_id: i64,
    #[serde(default, deserialize_with = "crate::types::de_decimal")]
    pub size: Decimal,
    #[serde(default, deserialize_with = "crate::types::de_decimal")]
    pub price: Decimal,
    #[serde(default)]
    pub is_maker_ask: bool,
    #[serde(default, deserialize_with = "crate::types::de_i64")]
    pub timestamp: i64,
}

#[derive(Debug, Clone)]
pub enum WsEvent {
    Connected,
    Reconnected,
    /// The book for `market_id` in the shared [`BookStore`] changed.
    Book {
        market_id: i64,
        snapshot: bool,
    },
    /// Own orders (snapshot on subscribe, then deltas).
    Orders {
        orders: Vec<Order>,
        snapshot: bool,
    },
    /// Account snapshot/delta: positions present in this message.
    Account {
        positions: Vec<Position>,
        snapshot: bool,
    },
    Trades {
        market_id: i64,
        trades: Vec<PublicTrade>,
    },
    Error {
        code: i64,
        message: String,
    },
    Raw(Value),
}

pub struct WsHandle {
    pub events: mpsc::Receiver<WsEvent>,
    pub books: BookStore,
    shutdown: watch::Sender<bool>,
}

impl WsHandle {
    pub fn shutdown(&self) {
        let _ = self.shutdown.send(true);
    }

    pub async fn book(&self, market_id: i64) -> Option<OrderBook> {
        self.books.read().await.get(&market_id).cloned()
    }
}

pub fn spawn(cfg: WsConfig) -> WsHandle {
    let (tx, rx) = mpsc::channel(1024);
    let (stop_tx, stop_rx) = watch::channel(false);
    let books: BookStore = Arc::new(RwLock::new(HashMap::new()));
    tokio::spawn(run(cfg, tx, stop_rx, books.clone()));
    WsHandle {
        events: rx,
        books,
        shutdown: stop_tx,
    }
}

fn subscriptions(cfg: &WsConfig) -> Vec<String> {
    let mut subs = Vec::new();
    for m in &cfg.order_books {
        subs.push(
            serde_json::json!({"type":"subscribe","channel":format!("order_book/{m}")}).to_string(),
        );
    }
    for m in &cfg.trades {
        subs.push(
            serde_json::json!({"type":"subscribe","channel":format!("trade/{m}")}).to_string(),
        );
    }
    if let Some(acct) = cfg.account_index {
        if cfg.account_all {
            let mut v =
                serde_json::json!({"type":"subscribe","channel":format!("account_all/{acct}")});
            if let Some(t) = &cfg.auth_token {
                v["auth"] = Value::String(t.clone());
            }
            subs.push(v.to_string());
        }
        if cfg.account_orders {
            let mut v = serde_json::json!({"type":"subscribe","channel":format!("account_all_orders/{acct}")});
            if let Some(t) = &cfg.auth_token {
                v["auth"] = Value::String(t.clone());
            }
            subs.push(v.to_string());
        }
    }
    subs
}

async fn run(
    cfg: WsConfig,
    tx: mpsc::Sender<WsEvent>,
    mut stop: watch::Receiver<bool>,
    books: BookStore,
) {
    let mut backoff = Duration::from_millis(500);
    let mut first = true;
    loop {
        if *stop.borrow() {
            return;
        }
        match tokio_tungstenite::connect_async(&cfg.url).await {
            Ok((ws, _)) => {
                backoff = Duration::from_millis(500);
                books.write().await.clear();
                let ev = if first {
                    WsEvent::Connected
                } else {
                    WsEvent::Reconnected
                };
                first = false;
                if tx.send(ev).await.is_err() {
                    return;
                }
                let outcome = session(&cfg, ws, &tx, &mut stop, &books).await;
                if *stop.borrow() {
                    return;
                }
                match outcome {
                    Ok(()) => info!("websocket closed; reconnecting"),
                    Err(e) => warn!(error = %e, "websocket session error; reconnecting"),
                }
            }
            Err(e) => warn!(error = %e, "websocket connect failed"),
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {},
            _ = stop.changed() => return,
        }
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn session(
    cfg: &WsConfig,
    mut ws: Socket,
    tx: &mpsc::Sender<WsEvent>,
    stop: &mut watch::Receiver<bool>,
    books: &BookStore,
) -> Result<(), String> {
    let mut ping = tokio::time::interval(cfg.ping_interval);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping.tick().await; // first tick fires immediately; skip it
    let mut subscribed = false;
    loop {
        let frame = tokio::select! {
            f = ws.next() => f,
            _ = ping.tick() => {
                ws.send(Message::text(r#"{"type":"ping"}"#)).await.map_err(|e| e.to_string())?;
                continue;
            }
            _ = stop.changed() => {
                let _ = ws.close(None).await;
                return Ok(());
            }
            _ = tokio::time::sleep(cfg.idle_timeout) => return Err("idle timeout".into()),
        };
        let msg = match frame {
            None => return Ok(()),
            Some(Err(e)) => return Err(e.to_string()),
            Some(Ok(m)) => m,
        };
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
            Message::Ping(p) => {
                ws.send(Message::Pong(p)).await.map_err(|e| e.to_string())?;
                continue;
            }
            Message::Close(_) => return Ok(()),
            _ => continue,
        };
        let value: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if cfg.emit_raw && tx.send(WsEvent::Raw(value.clone())).await.is_err() {
            return Ok(());
        }
        let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "connected" => {
                if !subscribed {
                    for s in subscriptions(cfg) {
                        ws.send(Message::text(s)).await.map_err(|e| e.to_string())?;
                    }
                    subscribed = true;
                }
            }
            "ping" => ws
                .send(Message::text(r#"{"type":"pong"}"#))
                .await
                .map_err(|e| e.to_string())?,
            "pong" => {}
            "error" => {
                let code = value.get("code").and_then(Value::as_i64).unwrap_or(0);
                let message = value
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                warn!(code, %message, "websocket error frame");
                let _ = tx.send(WsEvent::Error { code, message }).await;
            }
            k if k.ends_with("/order_book") => {
                let snapshot = k.starts_with("subscribed");
                if let Some((market_id, resubscribe)) = apply_book(&value, snapshot, books).await {
                    if resubscribe {
                        warn!(market_id, "order book sequence gap; resubscribing");
                        let ch = format!("order_book/{market_id}");
                        ws.send(Message::text(
                            serde_json::json!({"type":"unsubscribe","channel":ch}).to_string(),
                        ))
                        .await
                        .map_err(|e| e.to_string())?;
                        ws.send(Message::text(
                            serde_json::json!({"type":"subscribe","channel":ch}).to_string(),
                        ))
                        .await
                        .map_err(|e| e.to_string())?;
                    } else if tx
                        .send(WsEvent::Book {
                            market_id,
                            snapshot,
                        })
                        .await
                        .is_err()
                    {
                        return Ok(());
                    }
                }
            }
            k if k.ends_with("/account_all_orders") || k.ends_with("/account_orders") => {
                let snapshot = k.starts_with("subscribed");
                let orders = collect_orders(&value);
                if tx.send(WsEvent::Orders { orders, snapshot }).await.is_err() {
                    return Ok(());
                }
            }
            k if k.ends_with("/account_all") => {
                let snapshot = k.starts_with("subscribed");
                let positions = collect_map_or_vec::<Position>(value.get("positions"));
                if tx
                    .send(WsEvent::Account {
                        positions,
                        snapshot,
                    })
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
            k if k.ends_with("/trade") => {
                let market_id = channel_id(&value);
                let trades = collect_map_or_vec::<PublicTrade>(value.get("trades"));
                if !trades.is_empty()
                    && tx
                        .send(WsEvent::Trades { market_id, trades })
                        .await
                        .is_err()
                {
                    return Ok(());
                }
            }
            other => debug!(kind = other, "unhandled websocket frame"),
        }
    }
}

/// Channel suffix after `:` (responses use `order_book:0`, requests `order_book/0`).
fn channel_id(v: &Value) -> i64 {
    v.get("channel")
        .and_then(Value::as_str)
        .and_then(|c| c.rsplit([':', '/']).next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(-1)
}

/// Applies a snapshot or delta to the shared book. Returns `(market_id, needs_resubscribe)`.
async fn apply_book(v: &Value, snapshot: bool, books: &BookStore) -> Option<(i64, bool)> {
    let market_id = channel_id(v);
    if market_id < 0 {
        return None;
    }
    let ob = v.get("order_book").filter(|o| !o.is_null())?;
    let bids: Vec<Level> = ob
        .get("bids")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .ok()
        .flatten()
        .unwrap_or_default();
    let asks: Vec<Level> = ob
        .get("asks")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .ok()
        .flatten()
        .unwrap_or_default();
    let seq = |k: &str| {
        ob.get(k).or_else(|| v.get(k)).and_then(|x| {
            x.as_i64()
                .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
        })
    };
    let begin = seq("begin_nonce");
    let end = seq("nonce").or_else(|| seq("offset"));

    let mut guard = books.write().await;
    let book = guard
        .entry(market_id)
        .or_insert_with(|| OrderBook::new(market_id));
    if snapshot {
        book.replace(&bids, &asks);
    } else {
        if let (Some(prev), Some(b)) = (book.nonce, begin) {
            if b != prev {
                book.nonce = None;
                return Some((market_id, true));
            }
        }
        book.apply(&bids, &asks);
    }
    if end.is_some() {
        book.nonce = end;
    }
    book.updated_at_ms = now_ms();
    Some((market_id, false))
}

/// `orders` arrives either as `{market_id: [Order,...]}` or `[Order,...]`.
fn collect_orders(v: &Value) -> Vec<Order> {
    collect_map_or_vec::<Order>(v.get("orders"))
}

fn collect_map_or_vec<T: serde::de::DeserializeOwned>(v: Option<&Value>) -> Vec<T> {
    let mut out = Vec::new();
    match v {
        Some(Value::Array(items)) => {
            for it in items {
                if let Ok(o) = serde_json::from_value(it.clone()) {
                    out.push(o);
                }
            }
        }
        Some(Value::Object(map)) => {
            for (_, val) in map {
                match val {
                    Value::Array(items) => {
                        for it in items {
                            if let Ok(o) = serde_json::from_value(it.clone()) {
                                out.push(o);
                            }
                        }
                    }
                    Value::Object(_) => {
                        if let Ok(o) = serde_json::from_value(val.clone()) {
                            out.push(o);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[tokio::test]
    async fn snapshot_then_delta_then_gap() {
        let books: BookStore = Arc::new(RwLock::new(HashMap::new()));
        let snap: Value = serde_json::from_str(
            r#"{"type":"subscribed/order_book","channel":"order_book:0",
                "order_book":{"bids":[{"price":"100","size":"1"}],"asks":[{"price":"101","size":"2"}],"nonce":10}}"#,
        )
        .unwrap();
        assert_eq!(apply_book(&snap, true, &books).await, Some((0, false)));
        let delta: Value = serde_json::from_str(
            r#"{"type":"update/order_book","channel":"order_book:0",
                "order_book":{"bids":[{"price":"100","size":"0"},{"price":"99","size":"3"}],"asks":[],"begin_nonce":10,"nonce":11}}"#,
        )
        .unwrap();
        assert_eq!(apply_book(&delta, false, &books).await, Some((0, false)));
        {
            let b = books.read().await;
            let book = b.get(&0).unwrap();
            assert_eq!(book.best_bid().unwrap(), (dec!(99), dec!(3)));
            assert_eq!(book.best_ask().unwrap(), (dec!(101), dec!(2)));
            assert_eq!(book.nonce, Some(11));
        }
        let gap: Value = serde_json::from_str(
            r#"{"type":"update/order_book","channel":"order_book:0",
                "order_book":{"bids":[],"asks":[],"begin_nonce":13,"nonce":14}}"#,
        )
        .unwrap();
        assert_eq!(apply_book(&gap, false, &books).await, Some((0, true)));
    }

    #[test]
    fn orders_from_map_or_array() {
        let m: Value = serde_json::from_str(
            r#"{"orders":{"0":[{"order_index":1,"client_order_index":5,"status":"open"}],"1":[{"order_index":2}]}}"#,
        )
        .unwrap();
        assert_eq!(collect_orders(&m).len(), 2);
        let a: Value = serde_json::from_str(r#"{"orders":[{"order_index":3}]}"#).unwrap();
        assert_eq!(collect_orders(&a)[0].order_index, 3);
    }

    #[test]
    fn subscriptions_include_auth() {
        let mut cfg = WsConfig::new("wss://x/stream");
        cfg.order_books = vec![0, 1];
        cfg.account_index = Some(7);
        cfg.account_orders = true;
        cfg.auth_token = Some("tok".into());
        let subs = subscriptions(&cfg);
        assert_eq!(subs.len(), 3);
        assert!(subs[2].contains("account_all_orders/7"));
        assert!(subs[2].contains("\"auth\":\"tok\""));
    }
}
