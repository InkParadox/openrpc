use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("all providers failed for chain {chain}: {details}")]
    AllProvidersFailed { chain: String, details: String },

    #[error("unknown chain: {0}")]
    UnknownChain(String),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("cache error: {0}")]
    Cache(#[from] rusqlite::Error),

    #[error("timeout after {ms}ms on chain {chain}")]
    Timeout { chain: String, ms: u64 },
}
