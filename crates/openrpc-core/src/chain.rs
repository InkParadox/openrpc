//! Built-in chain configurations with free public RPC providers.
//!
//! All providers listed here are free, require no API key, and have
//! demonstrated production reliability as of March 2026.

use serde::Serialize;

/// Configuration for a supported blockchain.
#[derive(Debug, Clone, Serialize)]
pub struct ChainConfig {
    /// Short identifier used in URLs (e.g. "eth", "base")
    pub id: &'static str,
    /// Human-readable name
    pub name: &'static str,
    /// EVM chain ID
    pub chain_id: u64,
    /// Native currency symbol
    pub native_symbol: &'static str,
    /// Ordered list of free public RPC endpoints.
    /// Earlier = preferred (lowest latency from our measurements).
    pub providers: &'static [&'static str],
    /// Block time in milliseconds (approximate)
    pub block_time_ms: u64,
}

impl ChainConfig {
    /// Whether this chain is EVM-compatible.
    pub fn is_evm(&self) -> bool {
        self.chain_id > 0
    }
}

/// All built-in chains. Use `chain_by_id()` to look up by slug.
pub const CHAINS: &[ChainConfig] = &[
    ChainConfig {
        id: "eth",
        name: "Ethereum",
        chain_id: 1,
        native_symbol: "ETH",
        providers: &[
            "https://eth.llamarpc.com",
            "https://cloudflare-eth.com",
            "https://rpc.ankr.com/eth",
            "https://ethereum.publicnode.com",
            "https://1rpc.io/eth",
            "https://rpc.flashbots.net",
        ],
        block_time_ms: 12_000,
    },
    ChainConfig {
        id: "base",
        name: "Base",
        chain_id: 8453,
        native_symbol: "ETH",
        providers: &[
            "https://mainnet.base.org",
            "https://base.llamarpc.com",
            "https://rpc.ankr.com/base",
            "https://base.publicnode.com",
            "https://1rpc.io/base",
        ],
        block_time_ms: 2_000,
    },
    ChainConfig {
        id: "arb",
        name: "Arbitrum One",
        chain_id: 42161,
        native_symbol: "ETH",
        providers: &[
            "https://arb1.arbitrum.io/rpc",
            "https://arbitrum.llamarpc.com",
            "https://rpc.ankr.com/arbitrum",
            "https://arbitrum.publicnode.com",
            "https://1rpc.io/arb",
        ],
        block_time_ms: 250,
    },
    ChainConfig {
        id: "op",
        name: "Optimism",
        chain_id: 10,
        native_symbol: "ETH",
        providers: &[
            "https://mainnet.optimism.io",
            "https://optimism.llamarpc.com",
            "https://rpc.ankr.com/optimism",
            "https://optimism.publicnode.com",
            "https://1rpc.io/op",
        ],
        block_time_ms: 2_000,
    },
    ChainConfig {
        id: "polygon",
        name: "Polygon",
        chain_id: 137,
        native_symbol: "MATIC",
        providers: &[
            "https://polygon-rpc.com",
            "https://polygon.llamarpc.com",
            "https://rpc.ankr.com/polygon",
            "https://polygon.publicnode.com",
            "https://1rpc.io/matic",
        ],
        block_time_ms: 2_200,
    },
    ChainConfig {
        id: "bsc",
        name: "BNB Smart Chain",
        chain_id: 56,
        native_symbol: "BNB",
        providers: &[
            "https://bsc-dataseed1.binance.org",
            "https://bsc-dataseed2.binance.org",
            "https://rpc.ankr.com/bsc",
            "https://bsc.publicnode.com",
            "https://1rpc.io/bnb",
        ],
        block_time_ms: 3_000,
    },
    ChainConfig {
        id: "avax",
        name: "Avalanche C-Chain",
        chain_id: 43114,
        native_symbol: "AVAX",
        providers: &[
            "https://api.avax.network/ext/bc/C/rpc",
            "https://rpc.ankr.com/avalanche",
            "https://avalanche.publicnode.com",
            "https://1rpc.io/avax/c",
        ],
        block_time_ms: 2_000,
    },
    ChainConfig {
        id: "gnosis",
        name: "Gnosis Chain",
        chain_id: 100,
        native_symbol: "xDAI",
        providers: &[
            "https://rpc.gnosischain.com",
            "https://rpc.ankr.com/gnosis",
            "https://gnosis.publicnode.com",
            "https://1rpc.io/gnosis",
        ],
        block_time_ms: 5_000,
    },
];

/// Look up a chain config by its short ID (e.g. "eth", "base").
pub fn chain_by_id(id: &str) -> Option<&'static ChainConfig> {
    CHAINS.iter().find(|c| c.id == id)
}

/// Look up a chain config by EVM chain ID.
pub fn chain_by_chain_id(chain_id: u64) -> Option<&'static ChainConfig> {
    CHAINS.iter().find(|c| c.chain_id == chain_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_chains_have_providers() {
        for chain in CHAINS {
            assert!(!chain.providers.is_empty(), "chain {} has no providers", chain.id);
        }
    }

    #[test]
    fn chain_ids_unique() {
        let mut seen = std::collections::HashSet::new();
        for chain in CHAINS {
            assert!(seen.insert(chain.chain_id), "duplicate chain_id {}", chain.chain_id);
        }
    }

    #[test]
    fn lookup_by_id() {
        assert!(chain_by_id("eth").is_some());
        assert!(chain_by_id("base").is_some());
        assert!(chain_by_id("notachain").is_none());
    }

    #[test]
    fn lookup_by_chain_id() {
        assert!(chain_by_chain_id(1).is_some());
        assert_eq!(chain_by_chain_id(1).unwrap().id, "eth");
        assert_eq!(chain_by_chain_id(8453).unwrap().id, "base");
    }

    #[test]
    fn eth_has_minimum_providers() {
        let eth = chain_by_id("eth").unwrap();
        assert!(eth.providers.len() >= 4, "ETH should have at least 4 free providers");
    }
}
