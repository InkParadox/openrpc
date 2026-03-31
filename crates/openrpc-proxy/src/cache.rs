//! SQLite-backed response cache for JSON-RPC calls.
//!
//! Cache key = SHA-256 of (chain + method + canonical params JSON).
//! TTL is method-specific — immutable data (tx by hash, blocks by hash)
//! is cached forever; mutable data has short TTLs.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use sha2::{Digest, Sha256};

use openrpc_core::rpc::JsonRpcRequest;

/// SQLite-backed response cache.
pub struct ResponseCache {
    conn: Connection,
}

impl ResponseCache {
    /// Open or create a persistent cache at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        Self::init(&conn)?;
        Ok(Self { conn })
    }

    /// Create an in-memory cache (for testing).
    pub fn in_memory() -> Result<Self, rusqlite::Error> {
        let conn = Connection::open_in_memory()?;
        Self::init(&conn)?;
        Ok(Self { conn })
    }

    fn init(conn: &Connection) -> Result<(), rusqlite::Error> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS responses (
                 cache_key  TEXT PRIMARY KEY,
                 result     TEXT NOT NULL,
                 cached_at  INTEGER NOT NULL,
                 expires_at INTEGER           -- NULL means never expires
             );
             CREATE INDEX IF NOT EXISTS idx_expires ON responses(expires_at);",
        )
    }

    /// Get a cached result. Returns `None` if not found or expired.
    pub fn get(&self, chain: &str, req: &JsonRpcRequest) -> Result<Option<Value>, rusqlite::Error> {
        let key = cache_key(chain, req);
        let now = now_secs();

        let result: Option<String> = self
            .conn
            .query_row(
                "SELECT result FROM responses
                 WHERE cache_key = ?1
                   AND (expires_at IS NULL OR expires_at > ?2)",
                params![key, now],
                |row| row.get(0),
            )
            .optional()?;

        match result {
            Some(json) => Ok(serde_json::from_str(&json).ok()),
            None => Ok(None),
        }
    }

    /// Store a result. No-op if the request has no cache TTL.
    pub fn set(&self, chain: &str, req: &JsonRpcRequest, result: &Value) -> Result<(), rusqlite::Error> {
        let Some(ttl) = req.cache_ttl_secs() else {
            return Ok(());
        };

        let key = cache_key(chain, req);
        let now = now_secs();
        let expires_at: Option<i64> = if ttl == u64::MAX {
            None // never expires
        } else {
            Some((now + ttl as i64).min(i64::MAX))
        };

        let result_json = serde_json::to_string(result).unwrap_or_default();

        self.conn.execute(
            "INSERT OR REPLACE INTO responses (cache_key, result, cached_at, expires_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![key, result_json, now, expires_at],
        )?;

        Ok(())
    }

    /// Remove all expired entries.
    pub fn evict_expired(&self) -> Result<u64, rusqlite::Error> {
        let now = now_secs();
        let count = self.conn.execute(
            "DELETE FROM responses WHERE expires_at IS NOT NULL AND expires_at <= ?1",
            params![now],
        )?;
        Ok(count as u64)
    }

    /// Total number of cached entries.
    pub fn count(&self) -> Result<u64, rusqlite::Error> {
        self.conn
            .query_row("SELECT COUNT(*) FROM responses", [], |row| {
                row.get::<_, i64>(0).map(|n| n as u64)
            })
    }
}

/// Compute the cache key for a request: SHA-256(chain|method|canonical_params).
fn cache_key(chain: &str, req: &JsonRpcRequest) -> String {
    let canonical = format!(
        "{}|{}|{}",
        chain,
        req.method,
        canonical_params(&req.params)
    );
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    hex::encode(hasher.finalize())
}

/// Serialize params to a canonical string (sorted keys for objects).
fn canonical_params(params: &Value) -> String {
    match params {
        Value::Array(arr) => {
            let parts: Vec<String> = arr.iter().map(canonical_params).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by_key(|(k, _)| k.as_str());
            let parts: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("{}:{}", k, canonical_params(v)))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        other => other.to_string(),
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn req(method: &str, params: Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: method.into(),
            params,
        }
    }

    #[test]
    fn cache_miss_returns_none() {
        let cache = ResponseCache::in_memory().unwrap();
        let r = req("eth_blockNumber", json!([]));
        assert!(cache.get("eth", &r).unwrap().is_none());
    }

    #[test]
    fn cache_set_and_get() {
        let cache = ResponseCache::in_memory().unwrap();
        let r = req("eth_getTransactionByHash", json!(["0xabc"]));
        let result = json!({ "hash": "0xabc", "blockNumber": "0x1" });
        cache.set("eth", &r, &result).unwrap();
        let cached = cache.get("eth", &r).unwrap();
        assert_eq!(cached, Some(result));
    }

    #[test]
    fn non_cacheable_method_not_stored() {
        let cache = ResponseCache::in_memory().unwrap();
        let r = req("eth_sendRawTransaction", json!(["0xdeadbeef"]));
        cache.set("eth", &r, &json!("0xtxhash")).unwrap();
        // Should not have been stored
        assert_eq!(cache.count().unwrap(), 0);
    }

    #[test]
    fn immutable_result_has_no_expiry() {
        let cache = ResponseCache::in_memory().unwrap();
        let r = req("eth_getTransactionReceipt", json!(["0xabc"]));
        cache.set("eth", &r, &json!({"status": "0x1"})).unwrap();
        // Should be stored
        assert_eq!(cache.count().unwrap(), 1);
        // Should survive eviction
        cache.evict_expired().unwrap();
        assert_eq!(cache.count().unwrap(), 1);
    }

    #[test]
    fn different_chains_different_keys() {
        let cache = ResponseCache::in_memory().unwrap();
        let r = req("eth_getTransactionByHash", json!(["0xabc"]));
        cache.set("eth", &r, &json!("eth-result")).unwrap();
        cache.set("base", &r, &json!("base-result")).unwrap();
        // Both stored separately
        assert_eq!(cache.count().unwrap(), 2);
        assert_eq!(cache.get("eth", &r).unwrap(), Some(json!("eth-result")));
        assert_eq!(cache.get("base", &r).unwrap(), Some(json!("base-result")));
    }

    #[test]
    fn canonical_params_stable() {
        // Same params in different order should produce same key
        let r1 = req("eth_call", json!([{"to": "0xabc", "from": "0xdef"}, "latest"]));
        let r2 = req("eth_call", json!([{"from": "0xdef", "to": "0xabc"}, "latest"]));
        assert_eq!(cache_key("eth", &r1), cache_key("eth", &r2));
    }

    #[test]
    fn evict_expired_removes_nothing_for_immortal() {
        let cache = ResponseCache::in_memory().unwrap();
        let r = req("eth_getTransactionByHash", json!(["0x1"]));
        cache.set("eth", &r, &json!("ok")).unwrap();
        let removed = cache.evict_expired().unwrap();
        assert_eq!(removed, 0);
        assert_eq!(cache.count().unwrap(), 1);
    }
}
