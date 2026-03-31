# openrpc

> Drop-in multi-chain JSON-RPC load balancer. No API key required.

A zero-config replacement for Alchemy / Infura that routes requests across multiple free public RPC providers with health tracking, circuit breaking, and a smart SQLite response cache.

## Quick start

```bash
cargo install --path crates/openrpc-cli
openrpc serve
# → http://0.0.0.0:3000
```

## Usage

```
openrpc serve [OPTIONS]
openrpc chains

OPTIONS:
  -p, --port <PORT>       Port to listen on [default: 3000]
      --host <HOST>       Host to bind to [default: 0.0.0.0]
  -c, --chain <CHAIN>     Chains to enable (repeat for multiple) [default: all]
      --cache <PATH>      Path for persistent SQLite cache [default: in-memory]
      --log <LEVEL>       Log level: trace | debug | info | warn | error [default: info]
```

**Examples:**

```bash
# All 8 chains, in-memory cache, port 3000
openrpc serve

# Ethereum + Base only, persistent cache
openrpc serve --chain eth --chain base --cache ./rpc.db

# Custom port
openrpc serve --port 8545

# List built-in chains
openrpc chains
```

## API

### `POST /v1/{chain}` — proxy a JSON-RPC call

```bash
curl http://localhost:3000/v1/eth \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}'
```

### `GET /v1/chains` — list enabled chains

```json
{
  "chains": [
    { "id": "eth", "name": "Ethereum", "chain_id": 1, "provider_count": 6, "rpc_url": "/v1/eth" }
  ]
}
```

### `GET /v1/health` — per-provider health stats

```json
{
  "eth": [
    { "url": "https://eth.llamarpc.com", "avg_ms": 124, "consecutive_failures": 0, "status": "Healthy" }
  ]
}
```

### `GET /v1/cache/stats` — cache hit/miss counters

```json
{
  "total_cached_entries": 412,
  "by_chain": {
    "eth": { "hits": 1200, "misses": 412, "hit_rate": 0.744 }
  }
}
```

## Built-in chains

| ID      | Network             | Chain ID | Providers |
|---------|---------------------|----------|-----------|
| eth     | Ethereum            | 1        | 6         |
| base    | Base                | 8453     | 5         |
| arb     | Arbitrum One        | 42161    | 5         |
| op      | Optimism            | 10       | 5         |
| polygon | Polygon             | 137      | 5         |
| bsc     | BNB Smart Chain     | 56       | 5         |
| avax    | Avalanche C-Chain   | 43114    | 4         |
| gnosis  | Gnosis Chain        | 100      | 4         |

## How it works

**Routing:** requests fan out to the top 3 healthiest providers concurrently; first valid response wins.

**Health tracking:** exponential moving average for latency; circuit breaker trips after 3 consecutive failures, cools down for 60 s.

**Cache:** SQLite with method-specific TTLs:
- Immutable data (tx by hash, receipts, blocks by hash) → cached forever
- `eth_blockNumber`, `eth_gasPrice` → 4 s
- `eth_getBalance`, `eth_getCode` → 15 s
- Mutable calls (`eth_sendRawTransaction`, `eth_call`, etc.) → not cached

Cache key is `SHA-256(chain | method | canonical_params)` — object keys sorted for stability.

## Library usage

```rust
use openrpc_proxy::{ServerConfig, serve};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = ServerConfig {
        bind: "127.0.0.1:8545".parse()?,
        cache_path: Some("./cache.db".into()),
        chains: vec!["eth".into(), "base".into()],
    };
    serve(config).await
}
```

## License

MIT
