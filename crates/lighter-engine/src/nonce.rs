//! Nonce management for one `(account, api_key)` pair.
//!
//! Lighter requires `nonce = previous + 1` per API key. The manager primes itself from
//! `GET /nextNonce`, hands out consecutive values locally (no round-trip per order), and is
//! reset with [`NonceManager::resync`] after any submission failure, since a rejection by the
//! API server does *not* consume the nonce while a sequencer-side rejection does.

use tokio::sync::Mutex;

use crate::error::Result;
use crate::rest::RestClient;

pub struct NonceManager {
    rest: RestClient,
    account_index: i64,
    api_key_index: u8,
    next: Mutex<Option<i64>>,
}

impl NonceManager {
    pub fn new(rest: RestClient, account_index: i64, api_key_index: u8) -> Self {
        NonceManager {
            rest,
            account_index,
            api_key_index,
            next: Mutex::new(None),
        }
    }

    /// Reserves `n` consecutive nonces (n >= 1). Batches need consecutive values.
    pub async fn reserve(&self, n: usize) -> Result<Vec<i64>> {
        let mut guard = self.next.lock().await;
        let start = match *guard {
            Some(v) => v,
            None => {
                self.rest
                    .next_nonce(self.account_index, self.api_key_index)
                    .await?
            }
        };
        *guard = Some(start + n as i64);
        Ok((0..n as i64).map(|i| start + i).collect())
    }

    pub async fn next(&self) -> Result<i64> {
        Ok(self.reserve(1).await?[0])
    }

    /// Forget the local counter; the next call re-fetches from the API.
    pub async fn resync(&self) {
        *self.next.lock().await = None;
    }

    /// Current locally cached next nonce, if primed.
    pub async fn peek(&self) -> Option<i64> {
        *self.next.lock().await
    }
}
