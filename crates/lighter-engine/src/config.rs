//! Runtime configuration. Secrets come from the environment (or a `.env` file), never from
//! command-line flags, so they do not end up in shell history or process listings.

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Network {
    Mainnet,
    Testnet,
}

impl Network {
    pub fn rest_url(self) -> &'static str {
        match self {
            Network::Mainnet => "https://mainnet.zklighter.elliot.ai",
            Network::Testnet => "https://testnet.zklighter.elliot.ai",
        }
    }
    pub fn ws_url(self) -> &'static str {
        match self {
            Network::Mainnet => "wss://mainnet.zklighter.elliot.ai/stream",
            Network::Testnet => "wss://testnet.zklighter.elliot.ai/stream",
        }
    }
    /// Lighter L2 chain id used in the signed transaction hash.
    pub fn chain_id(self) -> u32 {
        match self {
            Network::Mainnet => 304,
            Network::Testnet => 300,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub rest_url: String,
    pub ws_url: String,
    pub chain_id: u32,
    pub account_index: Option<i64>,
    pub api_key_index: u8,
    /// Hex-encoded 40-byte API-key private key (`0x` prefix optional). `None` = read-only.
    pub api_key_private: Option<String>,
}

impl Config {
    /// Builds a config from `network` plus environment overrides:
    ///
    /// * `LIGHTER_REST_URL`, `LIGHTER_WS_URL`, `LIGHTER_CHAIN_ID`
    /// * `LIGHTER_ACCOUNT_INDEX`, `LIGHTER_API_KEY_INDEX` (default 4; 0-3 are reserved by Lighter)
    /// * `LIGHTER_API_KEY_PRIVATE`
    pub fn from_env(network: Network) -> Result<Self> {
        let _ = dotenvy::dotenv();
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let chain_id = match get("LIGHTER_CHAIN_ID") {
            Some(v) => v
                .parse()
                .map_err(|_| Error::Config("LIGHTER_CHAIN_ID must be an integer".into()))?,
            None => network.chain_id(),
        };
        let account_index =
            match get("LIGHTER_ACCOUNT_INDEX") {
                Some(v) => Some(v.parse().map_err(|_| {
                    Error::Config("LIGHTER_ACCOUNT_INDEX must be an integer".into())
                })?),
                None => None,
            };
        let api_key_index = match get("LIGHTER_API_KEY_INDEX") {
            Some(v) => v
                .parse()
                .map_err(|_| Error::Config("LIGHTER_API_KEY_INDEX must be 0..=254".into()))?,
            None => 4,
        };
        Ok(Config {
            rest_url: get("LIGHTER_REST_URL").unwrap_or_else(|| network.rest_url().to_string()),
            ws_url: get("LIGHTER_WS_URL").unwrap_or_else(|| network.ws_url().to_string()),
            chain_id,
            account_index,
            api_key_index,
            api_key_private: get("LIGHTER_API_KEY_PRIVATE"),
        })
    }

    pub fn require_account(&self) -> Result<i64> {
        self.account_index
            .ok_or_else(|| Error::Config("LIGHTER_ACCOUNT_INDEX is not set".into()))
    }

    pub fn require_key(&self) -> Result<&str> {
        self.api_key_private
            .as_deref()
            .ok_or_else(|| Error::Config("LIGHTER_API_KEY_PRIVATE is not set".into()))
    }
}
