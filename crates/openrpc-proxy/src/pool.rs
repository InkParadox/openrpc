//! Health-aware provider pool.
//!
//! Maintains health state for each provider and routes requests to the
//! healthiest available providers. Uses a circuit breaker: after 3
//! consecutive failures, a provider is cooled down for 60 seconds.
//!
//! Routing strategy:
//! 1. Filter out cooled-down providers
//! 2. Sort remaining by (consecutive_failures ASC, avg_ms ASC)
//! 3. Fire up to 3 concurrently (race — first wins)
//! 4. Update health stats from the winning response

use std::time::{Duration, Instant};

use reqwest::Client;
use serde_json::Value;
use tracing::{debug, warn};

use crate::error::ProxyError;
use openrpc_core::rpc::JsonRpcRequest;

const CIRCUIT_BREAK_FAILURES: u32 = 3;
const CIRCUIT_BREAK_COOLDOWN: Duration = Duration::from_secs(60);
const MAX_CONCURRENT: usize = 3;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(8);

/// Per-provider health tracking
#[derive(Debug, Clone)]
struct ProviderHealth {
    url: String,
    avg_ms: f64,
    consecutive_failures: u32,
    cooldown_until: Option<Instant>,
    total_ok: u64,
    total_err: u64,
}

impl ProviderHealth {
    fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            avg_ms: 400.0, // conservative initial estimate
            consecutive_failures: 0,
            cooldown_until: None,
            total_ok: 0,
            total_err: 0,
        }
    }

    fn is_available(&self) -> bool {
        match self.cooldown_until {
            Some(until) => Instant::now() >= until,
            None => true,
        }
    }

    fn record_success(&mut self, elapsed_ms: f64) {
        // Exponential moving average — weight recent measurements more
        self.avg_ms = self.avg_ms * 0.7 + elapsed_ms * 0.3;
        self.consecutive_failures = 0;
        self.cooldown_until = None;
        self.total_ok += 1;
    }

    fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        self.total_err += 1;
        if self.consecutive_failures >= CIRCUIT_BREAK_FAILURES {
            let until = Instant::now() + CIRCUIT_BREAK_COOLDOWN;
            self.cooldown_until = Some(until);
            warn!(
                url = %self.url,
                failures = self.consecutive_failures,
                "circuit breaker tripped — cooling down for 60s"
            );
        }
    }

    /// Adaptive timeout: 3× avg, bounded 1.5s–8s
    fn timeout(&self) -> Duration {
        let ms = (self.avg_ms * 3.0).round() as u64;
        let ms = ms.clamp(1_500, 8_000);
        Duration::from_millis(ms)
    }
}

/// Public stats snapshot for a provider
#[derive(Debug, Clone)]
pub struct ProviderStats {
    pub url: String,
    pub avg_ms: u64,
    pub consecutive_failures: u32,
    pub total_ok: u64,
    pub total_err: u64,
    pub status: ProviderStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProviderStatus {
    Healthy,
    Degraded,
    Cooldown,
}

/// Multi-provider pool with health tracking and circuit breaker.
pub struct ProviderPool {
    chain: String,
    providers: Vec<ProviderHealth>,
    client: Client,
}

impl ProviderPool {
    /// Create a pool for the given chain using the provided RPC URLs.
    pub fn new(chain: impl Into<String>, urls: &[&str]) -> Self {
        let client = Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .build()
            .expect("failed to build HTTP client");

        Self {
            chain: chain.into(),
            providers: urls.iter().map(|url| ProviderHealth::new(*url)).collect(),
            client,
        }
    }

    /// Send a JSON-RPC request through the pool.
    ///
    /// Fires up to 3 healthy providers concurrently — first valid response wins.
    /// Updates health stats for all participating providers.
    pub async fn send(&mut self, req: &JsonRpcRequest) -> Result<Value, ProxyError> {
        // Reset all cooled-down providers if ALL are in cooldown
        if self.providers.iter().all(|p| !p.is_available()) {
            warn!(chain = %self.chain, "all providers on cooldown — resetting");
            for p in &mut self.providers {
                p.cooldown_until = None;
                p.consecutive_failures = 0;
            }
        }

        // Get healthy provider indices sorted by quality
        let mut available: Vec<usize> = self
            .providers
            .iter()
            .enumerate()
            .filter(|(_, p)| p.is_available())
            .map(|(i, _)| i)
            .collect();

        available.sort_by(|&a, &b| {
            let pa = &self.providers[a];
            let pb = &self.providers[b];
            pa.consecutive_failures
                .cmp(&pb.consecutive_failures)
                .then_with(|| pa.avg_ms.partial_cmp(&pb.avg_ms).unwrap_or(std::cmp::Ordering::Equal))
        });

        // Take up to MAX_CONCURRENT
        let candidates: Vec<usize> = available.into_iter().take(MAX_CONCURRENT).collect();
        if candidates.is_empty() {
            return Err(ProxyError::AllProvidersFailed {
                chain: self.chain.clone(),
                details: "no providers available".into(),
            });
        }

        let body = serde_json::to_string(req)?;
        let client = self.client.clone();

        // Fire all candidates concurrently, collect (index, result, elapsed_ms)
        let mut handles = Vec::new();
        for &idx in &candidates {
            let url = self.providers[idx].url.clone();
            let timeout = self.providers[idx].timeout();
            let body = body.clone();
            let client = client.clone();

            handles.push(tokio::spawn(async move {
                let start = Instant::now();
                let result = client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .body(body)
                    .timeout(timeout)
                    .send()
                    .await;

                let elapsed_ms = start.elapsed().as_millis() as f64;

                match result {
                    Ok(resp) if resp.status().is_success() => {
                        match resp.json::<Value>().await {
                            Ok(v) if v.get("error").is_none() => {
                                if let Some(result) = v.get("result").cloned() {
                                    debug!(url = %url, elapsed_ms = elapsed_ms, "provider ok");
                                    Ok((idx, result, elapsed_ms))
                                } else {
                                    Err((idx, format!("missing 'result' field"), elapsed_ms))
                                }
                            }
                            Ok(v) => {
                                let msg = v["error"]["message"]
                                    .as_str()
                                    .unwrap_or("rpc error")
                                    .to_string();
                                Err((idx, msg, elapsed_ms))
                            }
                            Err(e) => Err((idx, e.to_string(), elapsed_ms)),
                        }
                    }
                    Ok(resp) => Err((idx, format!("HTTP {}", resp.status()), elapsed_ms)),
                    Err(e) => Err((idx, e.to_string(), elapsed_ms)),
                }
            }));
        }

        // Collect results as they complete; take first success
        let mut first_success: Option<(usize, Value, f64)> = None;
        let mut failures: Vec<(usize, String, f64)> = Vec::new();

        for handle in handles {
            match handle.await {
                Ok(Ok(success)) => {
                    if first_success.is_none() {
                        first_success = Some(success);
                    }
                }
                Ok(Err(failure)) => {
                    failures.push(failure);
                }
                Err(join_err) => {
                    warn!("task join error: {join_err}");
                }
            }
        }

        // Update health stats
        if let Some((winner_idx, _, elapsed)) = &first_success {
            self.providers[*winner_idx].record_success(*elapsed);
        }
        for (fail_idx, _, _) in &failures {
            self.providers[*fail_idx].record_failure();
        }

        match first_success {
            Some((_, result, _)) => Ok(result),
            None => {
                let msgs: Vec<String> = failures.into_iter().map(|(_, msg, _)| msg).collect();
                Err(ProxyError::AllProvidersFailed {
                    chain: self.chain.clone(),
                    details: msgs.join("; "),
                })
            }
        }
    }

    /// Snapshot of current health for all providers.
    pub fn stats(&self) -> Vec<ProviderStats> {
        self.providers
            .iter()
            .map(|p| ProviderStats {
                url: p.url.clone(),
                avg_ms: p.avg_ms.round() as u64,
                consecutive_failures: p.consecutive_failures,
                total_ok: p.total_ok,
                total_err: p.total_err,
                status: if p.cooldown_until.map(|u| Instant::now() < u).unwrap_or(false) {
                    ProviderStatus::Cooldown
                } else if p.consecutive_failures > 0 {
                    ProviderStatus::Degraded
                } else {
                    ProviderStatus::Healthy
                },
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request(method: &str) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: serde_json::json!(1),
            method: method.into(),
            params: serde_json::json!([]),
        }
    }

    #[test]
    fn provider_health_circuit_breaker() {
        let mut ph = ProviderHealth::new("https://example.com");
        assert!(ph.is_available());

        // 2 failures — still available (degraded)
        ph.record_failure();
        ph.record_failure();
        assert!(ph.is_available());

        // 3rd failure — circuit breaks
        ph.record_failure();
        assert!(!ph.is_available());
    }

    #[test]
    fn provider_health_recovers_on_success() {
        let mut ph = ProviderHealth::new("https://example.com");
        ph.record_failure();
        ph.record_failure();
        ph.record_success(150.0);
        assert_eq!(ph.consecutive_failures, 0);
        assert!(ph.is_available());
    }

    #[test]
    fn provider_health_ema() {
        let mut ph = ProviderHealth::new("https://example.com");
        // Initial avg_ms = 400
        ph.record_success(100.0);
        // 400 * 0.7 + 100 * 0.3 = 310
        assert!((ph.avg_ms - 310.0).abs() < 1.0);
    }

    #[test]
    fn provider_health_adaptive_timeout() {
        let ph = ProviderHealth::new("https://example.com");
        // avg_ms = 400, timeout = 400 * 3 = 1200 → clamped to 1500
        let t = ph.timeout();
        assert_eq!(t, Duration::from_millis(1500));
    }

    #[test]
    fn pool_stats_includes_all_providers() {
        let pool = ProviderPool::new("eth", &["https://a.com", "https://b.com"]);
        let stats = pool.stats();
        assert_eq!(stats.len(), 2);
        assert!(stats.iter().all(|s| s.status == ProviderStatus::Healthy));
    }

    #[test]
    fn pool_all_cooldown_resets() {
        let mut pool = ProviderPool::new("eth", &["https://a.com"]);
        // Force all into cooldown
        for _ in 0..5 {
            pool.providers[0].record_failure();
        }
        assert!(!pool.providers[0].is_available());
        // stats still shows cooldown
        assert_eq!(pool.stats()[0].status, ProviderStatus::Cooldown);
    }
}
