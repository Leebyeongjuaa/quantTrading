//! Async REST client for `https://<host>/api/v1/*`.
//!
//! Every Lighter response carries `{"code": 200, ...}` on success; anything else is surfaced
//! as [`Error::Api`]. Signed transactions are submitted as `application/x-www-form-urlencoded`
//! with the signer's `tx_info` JSON passed through unchanged.

use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::types::{Candle, MarketInfo, Order};

#[derive(Clone)]
pub struct RestClient {
    base: String,
    http: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SendTxResponse {
    pub code: i64,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub tx_hash: String,
    #[serde(default)]
    pub predicted_execution_time_ms: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SendTxBatchResponse {
    pub code: i64,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub tx_hash: Vec<String>,
    #[serde(default)]
    pub predicted_execution_time_ms: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BookOrder {
    #[serde(deserialize_with = "crate::types::de_i64")]
    pub order_index: i64,
    #[serde(deserialize_with = "crate::types::de_i64")]
    pub owner_account_index: i64,
    #[serde(deserialize_with = "crate::types::de_decimal")]
    pub remaining_base_amount: rust_decimal::Decimal,
    #[serde(deserialize_with = "crate::types::de_decimal")]
    pub price: rust_decimal::Decimal,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderBookOrders {
    #[serde(default)]
    pub asks: Vec<BookOrder>,
    #[serde(default)]
    pub bids: Vec<BookOrder>,
}

/// Lighter only serves these resolutions; `10m` must be built client-side (see [`crate::candles`]).
pub const CANDLE_RESOLUTIONS: &[&str] = &["1m", "5m", "15m", "30m", "1h", "4h", "12h", "1d"];
const CANDLE_PAGE: i64 = 500;

pub fn resolution_ms(res: &str) -> Option<i64> {
    let (num, unit) = res.split_at(res.len().checked_sub(1)?);
    let n: i64 = num.parse().ok()?;
    let unit_ms = match unit {
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return None,
    };
    Some(n * unit_ms)
}

impl RestClient {
    pub fn new(base: impl Into<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent("lighter-engine/0.1")
            .build()?;
        Ok(RestClient {
            base: base.into().trim_end_matches('/').to_string(),
            http,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        auth: Option<&str>,
    ) -> Result<T> {
        let mut req = self
            .http
            .get(format!("{}/api/v1/{path}", self.base))
            .query(query);
        if let Some(token) = auth {
            req = req.header("Authorization", token);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let body = resp.text().await?;
        let env: Envelope = serde_json::from_str(&body).unwrap_or(Envelope {
            code: status.as_u16() as i64,
            message: Some(body.chars().take(300).collect()),
        });
        if env.code != 200 {
            return Err(Error::Api {
                code: env.code,
                message: env.message.unwrap_or_else(|| format!("HTTP {status}")),
            });
        }
        Ok(serde_json::from_str(&body)?)
    }

    /// All perp and spot markets with their decimals, minimums and live prices.
    pub async fn order_book_details(&self, market_id: Option<i64>) -> Result<Vec<MarketInfo>> {
        #[derive(Deserialize)]
        struct Resp {
            #[serde(default)]
            order_book_details: Vec<MarketInfo>,
            #[serde(default)]
            spot_order_book_details: Vec<MarketInfo>,
        }
        let mut q = vec![("filter", "all".to_string())];
        if let Some(id) = market_id {
            q.push(("market_id", id.to_string()));
        }
        let r: Resp = self.get_json("orderBookDetails", &q, None).await?;
        let mut all = r.order_book_details;
        all.extend(r.spot_order_book_details);
        Ok(all)
    }

    /// Resting orders near the top of book (REST snapshot; the WS feed is the live source).
    pub async fn order_book_orders(&self, market_id: i64, limit: u32) -> Result<OrderBookOrders> {
        self.get_json(
            "orderBookOrders",
            &[
                ("market_id", market_id.to_string()),
                ("limit", limit.to_string()),
            ],
            None,
        )
        .await
    }

    /// One page (max 500) of candles. `count_back` only matters when `start_ms == 0`.
    pub async fn candles(
        &self,
        market_id: i64,
        resolution: &str,
        start_ms: i64,
        end_ms: i64,
        count_back: i64,
    ) -> Result<Vec<Candle>> {
        #[derive(Deserialize)]
        struct Resp {
            #[serde(default, rename = "c")]
            candles: Vec<Candle>,
        }
        let r: Resp = self
            .get_json(
                "candles",
                &[
                    ("market_id", market_id.to_string()),
                    ("resolution", resolution.to_string()),
                    ("start_timestamp", start_ms.to_string()),
                    ("end_timestamp", end_ms.to_string()),
                    ("count_back", count_back.to_string()),
                ],
                None,
            )
            .await?;
        Ok(r.candles)
    }

    /// Fetches `[start_ms, end_ms)` in 500-candle pages, de-duplicated and sorted by time.
    pub async fn candles_range(
        &self,
        market_id: i64,
        resolution: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Candle>> {
        let step = resolution_ms(resolution)
            .ok_or_else(|| Error::Config(format!("unsupported resolution {resolution}")))?;
        let mut out: Vec<Candle> = Vec::new();
        let mut cursor = start_ms;
        while cursor < end_ms {
            let page_end = (cursor + step * CANDLE_PAGE).min(end_ms);
            // Bulk downloads trip Lighter's rate limiter now and then; retry with backoff.
            let mut attempt = 0;
            let page = loop {
                match self
                    .candles(market_id, resolution, cursor, page_end, CANDLE_PAGE)
                    .await
                {
                    Ok(p) => break p,
                    Err(e) if attempt < 3 => {
                        attempt += 1;
                        tokio::time::sleep(Duration::from_millis(500 * attempt)).await;
                        tracing::warn!(error = %e, attempt, "candles page failed; retrying");
                    }
                    Err(e) => return Err(e),
                }
            };
            if page.is_empty() {
                cursor = page_end;
                continue;
            }
            let last = page
                .iter()
                .map(|c| c.open_time_ms)
                .max()
                .unwrap_or(page_end);
            out.extend(page);
            // Be polite on long histories (dozens of pages per market).
            tokio::time::sleep(Duration::from_millis(120)).await;
            cursor = (last + step).max(page_end.min(last + step));
            if last + step <= cursor && cursor <= last {
                break;
            }
        }
        out.sort_by_key(|c| c.open_time_ms);
        out.dedup_by_key(|c| c.open_time_ms);
        Ok(out)
    }

    pub async fn next_nonce(&self, account_index: i64, api_key_index: u8) -> Result<i64> {
        #[derive(Deserialize)]
        struct Resp {
            nonce: i64,
        }
        let r: Resp = self
            .get_json(
                "nextNonce",
                &[
                    ("account_index", account_index.to_string()),
                    ("api_key_index", api_key_index.to_string()),
                ],
                None,
            )
            .await?;
        Ok(r.nonce)
    }

    /// Registered public key for `(account, api_key_index)`; used to verify the local key.
    pub async fn api_key_public(
        &self,
        account_index: i64,
        api_key_index: u8,
    ) -> Result<Option<String>> {
        let v: Value = self
            .get_json(
                "apikeys",
                &[
                    ("account_index", account_index.to_string()),
                    ("api_key_index", api_key_index.to_string()),
                ],
                None,
            )
            .await?;
        Ok(v["api_keys"]
            .as_array()
            .and_then(|a| {
                a.iter()
                    .find(|k| k["api_key_index"].as_i64() == Some(api_key_index as i64))
            })
            .and_then(|k| k["public_key"].as_str().map(str::to_string)))
    }

    /// Raw account document (`by` is `index` or `l1_address`).
    pub async fn account(&self, by: &str, value: &str) -> Result<Value> {
        self.get_json(
            "account",
            &[("by", by.to_string()), ("value", value.to_string())],
            None,
        )
        .await
    }

    /// Open orders for an account. Requires an auth token (see [`crate::signer::Signer::auth_token`]).
    pub async fn active_orders(
        &self,
        account_index: i64,
        market_id: i64,
        auth: &str,
    ) -> Result<Vec<Order>> {
        #[derive(Deserialize)]
        struct Resp {
            #[serde(default)]
            orders: Vec<Order>,
        }
        let r: Resp = self
            .get_json(
                "accountActiveOrders",
                &[
                    ("account_index", account_index.to_string()),
                    ("market_id", market_id.to_string()),
                ],
                Some(auth),
            )
            .await?;
        Ok(r.orders)
    }

    /// `POST /api/v1/sendTx`. A `200` means *accepted by the API*, not executed: watch the
    /// `account_all_orders` stream (or `tx`) for the outcome.
    pub async fn send_tx(
        &self,
        tx_type: u8,
        tx_info: &str,
        price_protection: bool,
    ) -> Result<SendTxResponse> {
        let resp = self
            .http
            .post(format!("{}/api/v1/sendTx", self.base))
            .form(&[
                ("tx_type", tx_type.to_string()),
                ("tx_info", tx_info.to_string()),
                ("price_protection", price_protection.to_string()),
            ])
            .send()
            .await?;
        let body = resp.text().await?;
        let r: SendTxResponse = serde_json::from_str(&body).map_err(|_| Error::Api {
            code: 0,
            message: body.chars().take(300).collect(),
        })?;
        if r.code != 200 {
            return Err(Error::Api {
                code: r.code,
                message: r.message.unwrap_or_default(),
            });
        }
        Ok(r)
    }

    /// `POST /api/v1/sendTxBatch`: up to 50 transactions from one API key with consecutive
    /// nonces, applied together. Used for two-leg pair orders.
    pub async fn send_tx_batch(&self, txs: &[(u8, String)]) -> Result<SendTxBatchResponse> {
        if txs.is_empty() || txs.len() > 50 {
            return Err(Error::InvalidOrder(
                "batch must contain 1..=50 transactions".into(),
            ));
        }
        let types: Vec<u8> = txs.iter().map(|t| t.0).collect();
        let infos: Vec<&str> = txs.iter().map(|t| t.1.as_str()).collect();
        let resp = self
            .http
            .post(format!("{}/api/v1/sendTxBatch", self.base))
            .form(&[
                ("tx_types", serde_json::to_string(&types)?),
                ("tx_infos", serde_json::to_string(&infos)?),
            ])
            .send()
            .await?;
        let body = resp.text().await?;
        let r: SendTxBatchResponse = serde_json::from_str(&body).map_err(|_| Error::Api {
            code: 0,
            message: body.chars().take(300).collect(),
        })?;
        if r.code != 200 {
            return Err(Error::Api {
                code: r.code,
                message: r.message.unwrap_or_default(),
            });
        }
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_parsing() {
        assert_eq!(resolution_ms("1m"), Some(60_000));
        assert_eq!(resolution_ms("5m"), Some(300_000));
        assert_eq!(resolution_ms("10m"), Some(600_000));
        assert_eq!(resolution_ms("4h"), Some(14_400_000));
        assert_eq!(resolution_ms("1d"), Some(86_400_000));
        assert_eq!(resolution_ms("x"), None);
    }
}
