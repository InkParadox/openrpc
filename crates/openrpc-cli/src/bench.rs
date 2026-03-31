//! Latency benchmark for OpenRPC proxy and direct providers.
//!
//! Measures:
//!   - p50 / p95 / p99 per target
//!   - Cache cold vs warm effect (first N vs second N requests)
//!   - Proxy vs direct provider comparison
//!   - Provider-level breakdown from /v1/health

use std::time::{Duration, Instant};

use anyhow::Result;
use reqwest::Client;
use serde_json::Value;

// ── Test cases ────────────────────────────────────────────────────────────────

struct TestCase {
    name: &'static str,
    method: &'static str,
    /// Raw JSON string for the params array
    params_json: &'static str,
    cache_class: &'static str,
}

static TEST_CASES: &[TestCase] = &[
    TestCase {
        name: "eth_blockNumber",
        method: "eth_blockNumber",
        params_json: "[]",
        cache_class: "4s TTL",
    },
    TestCase {
        name: "eth_gasPrice",
        method: "eth_gasPrice",
        params_json: "[]",
        cache_class: "4s TTL",
    },
    TestCase {
        name: "eth_getBalance (vitalik.eth)",
        method: "eth_getBalance",
        params_json: r#"["0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045","latest"]"#,
        cache_class: "15s TTL",
    },
    TestCase {
        name: "eth_getTransactionByHash",
        method: "eth_getTransactionByHash",
        params_json: r#"["0x88df016429689c079f3b2f6ad39fa052532c56795b733da78a91ebe6a713944b"]"#,
        cache_class: "forever",
    },
    TestCase {
        name: "eth_getBlockByNumber (latest)",
        method: "eth_getBlockByNumber",
        params_json: r#"["latest",false]"#,
        cache_class: "4s TTL",
    },
];

fn build_body(method: &str, params_json: &str, id: u64) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params_json}}}"#
    )
}

// ── Timing helpers ────────────────────────────────────────────────────────────

struct Sample {
    ms: f64,
    ok: bool,
}

async fn time_request(client: &Client, url: &str, body: &str) -> Sample {
    let start = Instant::now();
    let result = client
        .post(url)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .timeout(Duration::from_secs(10))
        .send()
        .await;

    let elapsed = start.elapsed().as_secs_f64() * 1000.0;

    let ok = match result {
        Ok(r) if r.status().is_success() => {
            match r.json::<Value>().await {
                Ok(v) => v.get("error").is_none() && v.get("result").is_some(),
                Err(_) => false,
            }
        }
        _ => false,
    };

    Sample { ms: elapsed, ok }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn stats(samples: &[Sample]) -> (f64, f64, f64, f64, usize) {
    let ok: Vec<f64> = samples.iter().filter(|s| s.ok).map(|s| s.ms).collect();
    let errors = samples.len() - ok.len();

    if ok.is_empty() {
        return (0.0, 0.0, 0.0, 0.0, errors);
    }

    let mut sorted = ok.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let avg = sorted.iter().sum::<f64>() / sorted.len() as f64;
    let p50 = percentile(&sorted, 50.0);
    let p95 = percentile(&sorted, 95.0);
    let p99 = percentile(&sorted, 99.0);

    (avg, p50, p95, p99, errors)
}

fn print_row(label: &str, samples: &[Sample], note: &str) {
    let (avg, p50, p95, p99, errors) = stats(samples);
    let err_str = if errors > 0 { format!(" ({} err)", errors) } else { String::new() };
    println!(
        "  {:<35} avg={:>6.1}ms  p50={:>6.1}ms  p95={:>6.1}ms  p99={:>6.1}ms{}  {}",
        label, avg, p50, p95, p99, err_str, note
    );
}

// ── Main benchmark ─────────────────────────────────────────────────────────────

pub async fn run(
    proxy_url: &str,
    chain: &str,
    n: usize,
    compare: bool,
    direct_url: Option<&str>,
) -> Result<()> {
    let client = Client::builder()
        .timeout(Duration::from_secs(12))
        .build()?;

    let proxy_chain_url = format!("{}/v1/{}", proxy_url.trim_end_matches('/'), chain);

    println!();
    println!("OpenRPC Benchmark");
    println!("  proxy:  {}", proxy_chain_url);
    if let Some(d) = direct_url {
        println!("  direct: {}", d);
    }
    println!("  n={} requests per method per target", n);
    println!();

    // ── Per-method breakdown ──────────────────────────────────────────────────
    println!("── Per-method latency (p50 / p95 / p99) ──────────────────────────────────────");
    println!("  {:<35} {:<14} {:<14} {:<14}  {}", "Method", "avg", "p50", "p95", "cache");

    for tc in TEST_CASES {
        let body = build_body(tc.method, tc.params_json, 1);

        // Warm up (primes DNS + TCP connection pool)
        let _ = time_request(&client, &proxy_chain_url, &body).await;

        let mut samples = Vec::with_capacity(n);
        for _ in 0..n {
            samples.push(time_request(&client, &proxy_chain_url, &body).await);
        }

        print_row(tc.name, &samples, tc.cache_class);
    }

    // ── Cache cold vs warm ────────────────────────────────────────────────────
    println!();
    println!("── Cache effect: cold (first {n}) vs warm (second {n}) ───────────────────────");

    // Use a known immutable tx
    const IMMUTABLE_TX: &str = r#"["0x5c504ed432cb51138bcf09aa5e8a410dd4a1e204ef84bfed1be16dfba1b22060"]"#;
    let immutable_body = build_body("eth_getTransactionByHash", IMMUTABLE_TX, 1);

    let mut cold = Vec::with_capacity(n);
    for i in 0..n {
        // Different IDs so the proxy doesn't coalesce requests
        let b = build_body("eth_getTransactionByHash", IMMUTABLE_TX, 100 + i as u64);
        cold.push(time_request(&client, &proxy_chain_url, &b).await);
    }
    print_row("eth_getTransactionByHash cold", &cold, "cache miss → provider");

    let mut warm = Vec::with_capacity(n);
    for _ in 0..n {
        warm.push(time_request(&client, &proxy_chain_url, &immutable_body).await);
    }
    print_row("eth_getTransactionByHash warm", &warm, "cache hit → SQLite");

    // ── Direct comparison ─────────────────────────────────────────────────────
    if compare {
        if let Some(direct) = direct_url {
            println!();
            println!("── Proxy vs direct: eth_getBalance ({n} req each) ─────────────────────────────");

            const BAL_PARAMS: &str = r#"["0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045","latest"]"#;
            let bal_body = build_body("eth_getBalance", BAL_PARAMS, 1);

            let _ = time_request(&client, direct, &bal_body).await; // warmup

            let mut proxy_s = Vec::with_capacity(n);
            let mut direct_s = Vec::with_capacity(n);

            // Interleave to make it fair
            for _ in 0..n {
                proxy_s.push(time_request(&client, &proxy_chain_url, &bal_body).await);
                direct_s.push(time_request(&client, direct, &bal_body).await);
            }

            print_row("proxy (OpenRPC)", &proxy_s, "races providers + cache");
            print_row("direct", &direct_s, "single provider");
        }
    }

    // ── Health snapshot ───────────────────────────────────────────────────────
    println!();
    println!("── Provider health ────────────────────────────────────────────────────────────");
    let health_url = format!("{}/v1/health", proxy_url.trim_end_matches('/'));
    match client.get(&health_url).send().await {
        Ok(r) if r.status().is_success() => {
            if let Ok(data) = r.json::<Value>().await {
                if let Some(providers) = data.get(chain).and_then(|v| v.as_array()) {
                    for p in providers {
                        let url = p["url"].as_str().unwrap_or("?");
                        // Truncate URL for display
                        let display = if url.len() > 45 { format!("{}…", &url[..44]) } else { url.to_string() };
                        println!(
                            "  {:<47} avg={:>5}ms  failures={:>2}  {}",
                            display,
                            p["avg_ms"].as_u64().unwrap_or(0),
                            p["consecutive_failures"].as_u64().unwrap_or(0),
                            p["status"].as_str().unwrap_or("?"),
                        );
                    }
                }
            }
        }
        _ => println!("  (health endpoint unreachable)"),
    }

    // ── Cache stats ───────────────────────────────────────────────────────────
    println!();
    println!("── Cache stats ────────────────────────────────────────────────────────────────");
    let cache_url = format!("{}/v1/cache/stats", proxy_url.trim_end_matches('/'));
    match client.get(&cache_url).send().await {
        Ok(r) if r.status().is_success() => {
            if let Ok(data) = r.json::<Value>().await {
                let total = data["total_cached_entries"].as_u64().unwrap_or(0);
                println!("  total cached entries: {}", total);
                if let Some(by_chain) = data["by_chain"].as_object() {
                    for (c, stats) in by_chain {
                        let hits = stats["hits"].as_u64().unwrap_or(0);
                        let misses = stats["misses"].as_u64().unwrap_or(0);
                        let rate = stats["hit_rate"].as_f64().unwrap_or(0.0) * 100.0;
                        println!("  {}: {} hits / {} misses = {:.0}% hit rate", c, hits, misses, rate);
                    }
                }
            }
        }
        _ => println!("  (cache stats endpoint unreachable)"),
    }

    println!();
    Ok(())
}
