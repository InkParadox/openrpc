//! Axum HTTP server — EIP-1193 JSON-RPC proxy.
//!
//! Routes:
//!   POST /v1/{chain}        — proxy a JSON-RPC request for the named chain
//!   GET  /v1/chains         — list supported chains
//!   GET  /v1/health         — pool health stats per chain
//!   GET  /v1/cache/stats    — cache hit/miss stats

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json as AxumJson};
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use openrpc_core::chain::{chain_by_id, CHAINS};
use openrpc_core::rpc::{JsonRpcRequest, JsonRpcResponse};

use crate::cache::ResponseCache;
use crate::pool::ProviderPool;

/// Configuration for the proxy server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Address to bind to (default: 0.0.0.0:3000)
    pub bind: SocketAddr,
    /// Path for the SQLite cache file. None = in-memory.
    pub cache_path: Option<String>,
    /// Which chains to enable. Empty = all built-in chains.
    pub chains: Vec<String>,
    /// Extra providers to prepend per chain: chain_id → [url, ...]
    /// These are tried first (e.g. Helius for eth/sol with API key).
    pub extra_providers: HashMap<String, Vec<String>>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:3000".parse().unwrap(),
            cache_path: None,
            chains: vec![],
            extra_providers: HashMap::new(),
        }
    }
}

impl ServerConfig {
    /// Convenience: load extra providers from env vars.
    ///
    /// Reads `OPENRPC_EXTRA_{CHAIN}` (e.g. `OPENRPC_EXTRA_ETH`, `OPENRPC_EXTRA_SOL`)
    /// as comma-separated URLs, prepending them to the built-in provider list.
    ///
    /// Example (shell):
    /// ```text
    /// OPENRPC_EXTRA_ETH=https://eth-mainnet.g.alchemy.com/v2/YOUR_KEY
    /// OPENRPC_EXTRA_SOL=https://mainnet.helius-rpc.com/?api-key=YOUR_KEY
    /// ```
    pub fn with_env_providers(mut self) -> Self {
        use std::env;
        for chain in openrpc_core::chain::CHAINS {
            let var = format!("OPENRPC_EXTRA_{}", chain.id.to_uppercase());
            if let Ok(val) = env::var(&var) {
                let urls: Vec<String> = val.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                if !urls.is_empty() {
                    info!("extra providers for '{}' from env: {} url(s)", chain.id, urls.len());
                    self.extra_providers.insert(chain.id.to_string(), urls);
                }
            }
        }
        self
    }
}

/// Shared state across all handlers.
struct AppState {
    pools: HashMap<String, Mutex<ProviderPool>>,
    cache: Mutex<ResponseCache>,
    // Simple hit/miss counters per chain
    cache_hits: Mutex<HashMap<String, u64>>,
    cache_misses: Mutex<HashMap<String, u64>>,
}

/// Start the proxy server. Blocks until the server exits.
pub async fn serve(config: ServerConfig) -> anyhow::Result<()> {
    // Build provider pools
    let chains_to_enable: Vec<&str> = if config.chains.is_empty() {
        CHAINS.iter().map(|c| c.id).collect()
    } else {
        config.chains.iter().map(|s| s.as_str()).collect()
    };

    let mut pools = HashMap::new();
    for chain_id in chains_to_enable {
        let Some(chain) = chain_by_id(chain_id) else {
            warn!("unknown chain '{}' — skipping", chain_id);
            continue;
        };

        // Prepend any extra (keyed) providers before the free built-ins
        let extra = config.extra_providers.get(chain_id).map(|v| v.as_slice()).unwrap_or(&[]);
        let mut all_providers: Vec<&str> = extra.iter().map(|s| s.as_str()).collect();
        all_providers.extend_from_slice(chain.providers);

        info!("chain '{}' enabled with {} providers ({} keyed)", chain.id, all_providers.len(), extra.len());
        pools.insert(
            chain.id.to_string(),
            Mutex::new(ProviderPool::new(chain.id, &all_providers)),
        );
    }

    // Build cache
    let cache = match &config.cache_path {
        Some(path) => {
            info!("cache → {}", path);
            ResponseCache::open(path)?
        }
        None => {
            info!("cache → in-memory");
            ResponseCache::in_memory()?
        }
    };

    let state = Arc::new(AppState {
        pools,
        cache: Mutex::new(cache),
        cache_hits: Mutex::new(HashMap::new()),
        cache_misses: Mutex::new(HashMap::new()),
    });

    let app = Router::new()
        .route("/v1/{chain}", post(proxy_handler))
        .route("/v1/chains", get(chains_handler))
        .route("/v1/health", get(health_handler))
        .route("/v1/cache/stats", get(cache_stats_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    info!("OpenRPC proxy listening on http://{}", config.bind);
    axum::serve(listener, app).await?;
    Ok(())
}

/// POST /v1/{chain} — proxy a single JSON-RPC request.
async fn proxy_handler(
    Path(chain_id): Path<String>,
    State(state): State<Arc<AppState>>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    // Parse request body
    let req: JsonRpcRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                AxumJson(JsonRpcResponse::err(Value::Null, -32700, format!("Parse error: {e}"))),
            );
        }
    };

    let id = req.id.clone();

    // Validate chain
    if !state.pools.contains_key(&chain_id) {
        return (
            StatusCode::NOT_FOUND,
            AxumJson(JsonRpcResponse::err(id, -32000, format!("Unknown chain: {chain_id}"))),
        );
    }

    // Check cache first
    {
        let cache = state.cache.lock().await;
        match cache.get(&chain_id, &req) {
            Ok(Some(cached)) => {
                debug!(chain = %chain_id, method = %req.method, "cache hit");
                let mut hits = state.cache_hits.lock().await;
                *hits.entry(chain_id.clone()).or_insert(0) += 1;
                return (StatusCode::OK, AxumJson(JsonRpcResponse::ok(id, cached)));
            }
            Ok(None) => {
                let mut misses = state.cache_misses.lock().await;
                *misses.entry(chain_id.clone()).or_insert(0) += 1;
            }
            Err(e) => {
                warn!("cache read error: {e}");
            }
        }
    }

    // Forward to provider pool
    let result = {
        let pool = state.pools.get(&chain_id).unwrap();
        let mut pool = pool.lock().await;
        pool.send(&req).await
    };

    match result {
        Ok(value) => {
            // Store in cache
            {
                let cache = state.cache.lock().await;
                if let Err(e) = cache.set(&chain_id, &req, &value) {
                    warn!("cache write error: {e}");
                }
            }
            (StatusCode::OK, AxumJson(JsonRpcResponse::ok(id, value)))
        }
        Err(e) => {
            error!(chain = %chain_id, method = %req.method, error = %e, "all providers failed");
            (
                StatusCode::BAD_GATEWAY,
                AxumJson(JsonRpcResponse::err(id, -32603, e.to_string())),
            )
        }
    }
}

/// GET /v1/chains — list all enabled chains.
async fn chains_handler(State(state): State<Arc<AppState>>) -> AxumJson<Value> {
    let chains: Vec<Value> = CHAINS
        .iter()
        .filter(|c| state.pools.contains_key(c.id))
        .map(|c| {
            json!({
                "id": c.id,
                "name": c.name,
                "chain_id": c.chain_id,
                "native_symbol": c.native_symbol,
                "provider_count": c.providers.len(),
                "rpc_url": format!("/v1/{}", c.id),
            })
        })
        .collect();
    AxumJson(json!({ "chains": chains, "count": chains.len() }))
}

/// GET /v1/health — per-chain, per-provider health stats.
async fn health_handler(State(state): State<Arc<AppState>>) -> AxumJson<Value> {
    let mut result = serde_json::Map::new();

    for (chain_id, pool_mutex) in &state.pools {
        let pool = pool_mutex.lock().await;
        let stats: Vec<Value> = pool
            .stats()
            .into_iter()
            .map(|s| {
                json!({
                    "url": s.url,
                    "avg_ms": s.avg_ms,
                    "consecutive_failures": s.consecutive_failures,
                    "total_ok": s.total_ok,
                    "total_err": s.total_err,
                    "status": format!("{:?}", s.status),
                })
            })
            .collect();
        result.insert(chain_id.clone(), json!(stats));
    }

    AxumJson(Value::Object(result))
}

/// GET /v1/cache/stats — cache hit/miss counters.
async fn cache_stats_handler(State(state): State<Arc<AppState>>) -> AxumJson<Value> {
    let hits = state.cache_hits.lock().await.clone();
    let misses = state.cache_misses.lock().await.clone();
    let total_entries = {
        let cache = state.cache.lock().await;
        cache.count().unwrap_or(0)
    };

    let mut by_chain = serde_json::Map::new();
    let all_chains: std::collections::HashSet<_> = hits
        .keys()
        .chain(misses.keys())
        .cloned()
        .collect();

    for chain in all_chains {
        let h = hits.get(&chain).copied().unwrap_or(0);
        let m = misses.get(&chain).copied().unwrap_or(0);
        let total = h + m;
        let hit_rate = if total > 0 { h as f64 / total as f64 } else { 0.0 };
        by_chain.insert(
            chain,
            json!({ "hits": h, "misses": m, "hit_rate": hit_rate }),
        );
    }

    AxumJson(json!({
        "total_cached_entries": total_entries,
        "by_chain": by_chain,
    }))
}
