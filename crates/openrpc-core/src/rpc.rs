//! EIP-1193 / JSON-RPC 2.0 request and response types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A JSON-RPC 2.0 request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// A successful JSON-RPC 2.0 response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// A JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcRequest {
    /// Whether this method returns immutable data (safe to cache forever).
    ///
    /// Block data by hash, transactions by hash, receipts — immutable once confirmed.
    pub fn is_immutable(&self) -> bool {
        matches!(
            self.method.as_str(),
            "eth_getTransactionByHash"
                | "eth_getTransactionReceipt"
                | "eth_getBlockByHash"
                | "eth_getBlockTransactionCountByHash"
                | "eth_getUncleByBlockHashAndIndex"
                | "eth_getUncleCountByBlockHash"
        )
    }

    /// Whether this method returns data that changes with every block (don't cache or very short TTL).
    pub fn is_realtime(&self) -> bool {
        matches!(
            self.method.as_str(),
            "eth_blockNumber"
                | "eth_gasPrice"
                | "eth_maxPriorityFeePerGas"
                | "eth_feeHistory"
                | "eth_syncing"
        )
    }

    /// Cache TTL for this method in seconds. None = do not cache.
    pub fn cache_ttl_secs(&self) -> Option<u64> {
        if self.is_immutable() {
            return Some(u64::MAX); // forever
        }
        match self.method.as_str() {
            // Very short-lived
            "eth_blockNumber" | "eth_gasPrice" | "eth_maxPriorityFeePerGas" => Some(4),
            // Balance / state — refresh after ~15s (1 block)
            "eth_getBalance"
            | "eth_getTransactionCount"
            | "eth_getCode"
            | "eth_getStorageAt" => Some(15),
            // Block by number — stable after enough confirmations; short TTL for "latest"
            "eth_getBlockByNumber" | "eth_getBlockTransactionCountByNumber" => {
                // If the param references "latest"/"pending", short cache
                if self.params_contain_latest() {
                    Some(4)
                } else {
                    Some(300) // 5 min for historical blocks
                }
            }
            // Logs / filters
            "eth_getLogs" => Some(30),
            // Calls — cache briefly
            "eth_call" | "eth_estimateGas" => Some(10),
            // Don't cache write methods or unknowns
            _ => None,
        }
    }

    fn params_contain_latest(&self) -> bool {
        let s = self.params.to_string();
        s.contains("latest") || s.contains("pending")
    }
}

impl JsonRpcResponse {
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn req(method: &str) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: method.into(),
            params: json!([]),
        }
    }

    #[test]
    fn immutable_methods_cache_forever() {
        assert_eq!(req("eth_getTransactionByHash").cache_ttl_secs(), Some(u64::MAX));
        assert_eq!(req("eth_getTransactionReceipt").cache_ttl_secs(), Some(u64::MAX));
    }

    #[test]
    fn realtime_methods_cache_briefly() {
        let ttl = req("eth_blockNumber").cache_ttl_secs();
        assert!(ttl.is_some());
        assert!(ttl.unwrap() <= 10);
    }

    #[test]
    fn write_methods_not_cached() {
        assert_eq!(req("eth_sendRawTransaction").cache_ttl_secs(), None);
        assert_eq!(req("eth_sendTransaction").cache_ttl_secs(), None);
    }

    #[test]
    fn response_ok_round_trips() {
        let r = JsonRpcResponse::ok(json!(42), json!("0x1234"));
        let s = serde_json::to_string(&r).unwrap();
        let d: JsonRpcResponse = serde_json::from_str(&s).unwrap();
        assert!(d.result.is_some());
        assert!(d.error.is_none());
    }

    #[test]
    fn response_err_round_trips() {
        let r = JsonRpcResponse::err(json!(1), -32601, "Method not found");
        assert!(r.error.is_some());
        assert_eq!(r.error.unwrap().code, -32601);
    }
}
