use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("lighter api error (code {code}): {message}")]
    Api { code: i64, message: String },
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("signing error: {0}")]
    Signer(String),
    #[error("websocket error: {0}")]
    Ws(String),
    #[error("unknown market: {0}")]
    UnknownMarket(String),
    #[error("invalid order: {0}")]
    InvalidOrder(String),
    #[error("execution error: {0}")]
    Execution(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<lighters::Error> for Error {
    fn from(e: lighters::Error) -> Self {
        Error::Signer(e.to_string())
    }
}
