//! OpenRPC CLI — drop-in multi-chain JSON-RPC load balancer.
//!
//! Usage:
//!   openrpc serve                          # all chains, port 3000
//!   openrpc serve --port 8545              # custom port
//!   openrpc serve --chain eth --chain base # specific chains only
//!   openrpc serve --cache ./rpc-cache.db   # persistent SQLite cache
//!   openrpc chains                         # list built-in chains
//!   openrpc bench                          # benchmark local proxy
//!   openrpc bench --proxy https://api.mevici.com/rpc --compare  # vs production

mod bench;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use openrpc_core::chain::CHAINS;
use openrpc_proxy::{ServerConfig, serve};

#[derive(Parser)]
#[command(
    name = "openrpc",
    about = "Multi-chain JSON-RPC proxy with health-aware load balancing and smart caching",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the JSON-RPC proxy server.
    Serve {
        /// Port to listen on.
        #[arg(short, long, default_value = "3000")]
        port: u16,

        /// Host to bind to.
        #[arg(long, default_value = "0.0.0.0")]
        host: String,

        /// Chains to enable (repeat for multiple). Default: all built-in chains.
        #[arg(short, long = "chain", value_name = "CHAIN")]
        chains: Vec<String>,

        /// Path for persistent SQLite cache. Default: in-memory.
        #[arg(long, value_name = "PATH")]
        cache: Option<String>,

        /// Extra provider URL for a chain: chain=url (repeat for multiple).
        /// Extra providers are tried first (e.g. your Helius or Alchemy key).
        /// Also reads OPENRPC_EXTRA_{CHAIN} env vars automatically.
        ///
        /// Example: --extra sol=https://mainnet.helius-rpc.com/?api-key=KEY
        #[arg(long = "extra", value_name = "CHAIN=URL")]
        extras: Vec<String>,

        /// Log level (trace, debug, info, warn, error).
        #[arg(long, default_value = "info")]
        log: String,
    },

    /// List all built-in chains and their providers.
    Chains,

    /// Benchmark a running proxy: latency, cache effect, provider health.
    Bench {
        /// Proxy base URL to benchmark.
        #[arg(long, default_value = "http://localhost:3000")]
        proxy: String,

        /// Chain to test.
        #[arg(short, long, default_value = "eth")]
        chain: String,

        /// Number of requests per method.
        #[arg(short, long, default_value = "10")]
        n: usize,

        /// Also compare proxy vs a direct provider URL.
        #[arg(long)]
        compare: bool,

        /// Direct provider URL to compare against (requires --compare).
        /// Defaults to Cloudflare ETH if not set.
        #[arg(long)]
        direct: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Serve { port, host, chains, cache, extras, log } => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| EnvFilter::new(&log)),
                )
                .init();

            let bind: std::net::SocketAddr = format!("{host}:{port}").parse()?;

            // Parse --extra chain=url flags
            let mut extra_providers: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
            for entry in &extras {
                if let Some((chain, url)) = entry.split_once('=') {
                    extra_providers.entry(chain.to_string()).or_default().push(url.to_string());
                } else {
                    eprintln!("warning: --extra '{}' ignored (expected chain=url format)", entry);
                }
            }

            let config = ServerConfig {
                bind,
                cache_path: cache,
                chains,
                extra_providers,
            }.with_env_providers(); // also load OPENRPC_EXTRA_* from env

            serve(config).await?;
        }

        Commands::Bench { proxy, chain, n, compare, direct } => {
            let direct_url = direct.as_deref().or(if chain == "eth" {
                Some("https://cloudflare-eth.com")
            } else {
                None
            });
            bench::run(&proxy, &chain, n, compare, direct_url).await?;
        }

        Commands::Chains => {
            println!("{:<10} {:<30} {:<12} {:<8}", "ID", "NAME", "CHAIN ID", "PROVIDERS");
            println!("{}", "-".repeat(65));
            for chain in CHAINS {
                println!(
                    "{:<10} {:<30} {:<12} {:<8}",
                    chain.id,
                    chain.name,
                    chain.chain_id,
                    chain.providers.len(),
                );
            }
        }
    }

    Ok(())
}
