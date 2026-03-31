//! OpenRPC CLI — drop-in multi-chain JSON-RPC load balancer.
//!
//! Usage:
//!   openrpc serve                          # all chains, port 3000
//!   openrpc serve --port 8545              # custom port
//!   openrpc serve --chain eth --chain base # specific chains only
//!   openrpc serve --cache ./rpc-cache.db   # persistent SQLite cache
//!   openrpc chains                         # list built-in chains

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

        /// Log level (trace, debug, info, warn, error).
        #[arg(long, default_value = "info")]
        log: String,
    },

    /// List all built-in chains and their providers.
    Chains,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Serve { port, host, chains, cache, log } => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| EnvFilter::new(&log)),
                )
                .init();

            let bind: std::net::SocketAddr = format!("{host}:{port}").parse()?;

            let config = ServerConfig {
                bind,
                cache_path: cache,
                chains,
            };

            serve(config).await?;
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
