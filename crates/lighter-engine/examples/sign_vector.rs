//! Prints a deterministic signed CreateOrder so it can be cross-checked against the official
//! Go signer (`tools/go-signer-check`). Usage:
//!
//! `cargo run -p lighter-engine --example sign_vector -- <priv_key_hex> <nonce> <expired_at_ms>`

use lighter_engine::{LightersSigner, OrderParams, OrderType, Signer, TimeInForce};
use lighters::transactions::TxInfo;
use lighters::{OrderReq, TransactOpts, TxClient};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let key = args.get(1).map(String::as_str).unwrap_or(
        "0x07000000000000000300000000000000000000000000000000000000000000000000000000000000",
    );
    let nonce: i64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(42);
    let expired_at: i64 = args
        .get(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_700_000_000_000);

    // Same inputs the Go checker uses.
    let params = OrderParams {
        market_index: 0,
        client_order_index: 1,
        base_amount: 1000,
        price: 50_000,
        is_ask: false,
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::Gtt,
        reduce_only: false,
        trigger_price: 0,
        order_expiry_ms: 99_999_999,
    };
    let signer = LightersSigner::new(key, 1, 0, 304).unwrap();
    // Use the lower-level client so `expired_at` is pinned (the engine normally picks now+10m).
    let client = TxClient::new(key, 1, 0, 304).unwrap();
    let tx = client
        .create_order(
            OrderReq {
                market_index: params.market_index,
                client_order_index: params.client_order_index,
                base_amount: params.base_amount,
                price: params.price,
                is_ask: params.is_ask as u8,
                order_type: params.order_type as u8,
                time_in_force: params.time_in_force as u8,
                reduce_only: params.reduce_only as u8,
                trigger_price: params.trigger_price,
                order_expiry: params.order_expiry_ms,
            },
            &TransactOpts {
                nonce: Some(nonce),
                expired_at: Some(expired_at),
                ..Default::default()
            },
        )
        .unwrap();
    let out = serde_json::json!({
        "public_key": signer.public_key_hex(),
        "tx_type": tx.tx_type(),
        "tx_hash": tx.tx_hash(),
        "tx_info": tx.to_json().unwrap(),
        "auth_token": signer.auth_token(1_800_000_000).unwrap(),
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
