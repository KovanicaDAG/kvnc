//! Node configuration.
//!
//! Configuration is loaded from a TOML file (path supplied on the command line)
//! and can be overridden per-field by `KVNC_*` environment variables, which is
//! the recommended way to configure containerised deployments.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Runtime configuration for a KVNC node.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct NodeConfig {
    /// Directory where chain data (consensus + state databases) is stored.
    pub data_dir: PathBuf,
    /// P2P listen address, either `host:port` or a full libp2p multiaddr.
    pub listen_addr: String,
    /// JSON-RPC bind address (IP or hostname).
    pub rpc_addr: String,
    /// JSON-RPC port.
    pub rpc_port: u16,
    /// Bootstrap peers, each `host:port` or a libp2p multiaddr.
    pub bootnodes: Vec<String>,
    /// Path to a validator signing key file (32-byte hex seed).
    pub validator_key: Option<String>,
    /// Target consensus round duration in milliseconds.
    pub round_duration_ms: u64,
    /// Maximum number of connected peers.
    pub max_peers: usize,
    /// Genesis treasury address (32-byte hex). Defaults to the zero address.
    pub treasury_address: Option<String>,
    /// Enable the experimental MysticGhost ordering path (default off).
    pub use_mysticghost: bool,
    /// Run as a validator (produce blocks, participate in consensus).
    /// When false, the node runs as an RPC/read-only node without consensus participation.
    pub run_validator: bool,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("./data"),
            listen_addr: "0.0.0.0:9000".to_string(),
            rpc_addr: "127.0.0.1".to_string(),
            rpc_port: 8545,
            bootnodes: vec!["seed.kovanica.online:9000".to_string()],
            validator_key: None,
            round_duration_ms: 2000,
            max_peers: 50,
            treasury_address: None,
            use_mysticghost: true,
            run_validator: true,
        }
    }
}

impl NodeConfig {
    /// Load configuration from a TOML file (if present) and apply environment
    /// overrides.
    ///
    /// A missing file is not an error: the built-in defaults are used and a
    /// warning is emitted. This lets a node boot from environment variables
    /// alone.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let mut config = if path.exists() {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read config file {}", path.display()))?;
            Self::from_toml_str(&text)
                .with_context(|| format!("failed to parse config file {}", path.display()))?
        } else {
            tracing::warn!(path = %path.display(), "config file not found; using defaults");
            Self::default()
        };
        config.apply_env_overrides();
        Ok(config)
    }

    /// Parse configuration from a TOML string (no environment overrides).
    pub fn from_toml_str(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }

    /// Apply `KVNC_*` environment variable overrides on top of the file values.
    pub fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("KVNC_DATA_DIR") {
            self.data_dir = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("KVNC_LISTEN_ADDR") {
            self.listen_addr = v;
        }
        if let Ok(v) = std::env::var("KVNC_RPC_ADDR") {
            self.rpc_addr = v;
        }
        if let Ok(v) = std::env::var("KVNC_RPC_PORT") {
            match v.parse() {
                Ok(port) => self.rpc_port = port,
                Err(_) => tracing::warn!(value = %v, "ignoring invalid KVNC_RPC_PORT"),
            }
        }
        if let Ok(v) = std::env::var("KVNC_BOOTNODES") {
            self.bootnodes = v
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if let Ok(v) = std::env::var("KVNC_VALIDATOR_KEY") {
            self.validator_key = Some(v);
        }
        if let Ok(v) = std::env::var("KVNC_ROUND_DURATION_MS") {
            match v.parse() {
                Ok(ms) => self.round_duration_ms = ms,
                Err(_) => tracing::warn!(value = %v, "ignoring invalid KVNC_ROUND_DURATION_MS"),
            }
        }
        if let Ok(v) = std::env::var("KVNC_MAX_PEERS") {
            match v.parse() {
                Ok(n) => self.max_peers = n,
                Err(_) => tracing::warn!(value = %v, "ignoring invalid KVNC_MAX_PEERS"),
            }
        }
        if let Ok(v) = std::env::var("KVNC_TREASURY_ADDRESS") {
            self.treasury_address = Some(v);
        }
        if let Ok(v) = std::env::var("KVNC_MYSTICGHOST") {
            match v.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => self.use_mysticghost = true,
                "0" | "false" | "no" | "off" => self.use_mysticghost = false,
                _ => tracing::warn!(value = %v, "ignoring invalid KVNC_MYSTICGHOST"),
            }
        }
        if let Ok(v) = std::env::var("KVNC_RUN_VALIDATOR") {
            match v.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => self.run_validator = true,
                "0" | "false" | "no" | "off" => self.run_validator = false,
                _ => tracing::warn!(value = %v, "ignoring invalid KVNC_RUN_VALIDATOR"),
            }
        }
    }

    /// Resolve the JSON-RPC socket address.
    pub fn rpc_socket_addr(&self) -> Result<SocketAddr> {
        let raw = format!("{}:{}", self.rpc_addr, self.rpc_port);
        SocketAddr::from_str(&raw).with_context(|| format!("invalid rpc address `{raw}`"))
    }

    /// Path to the consensus (DAG) database file.
    pub fn consensus_db_path(&self) -> PathBuf {
        self.data_dir.join("consensus.redb")
    }

    /// Path to the state (accounts + staking) database file.
    pub fn state_db_path(&self) -> PathBuf {
        self.data_dir.join("state.redb")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Tests that touch `KVNC_*` environment variables must be serialised: the
    /// environment is process-global.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn config_loads_from_toml() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("node.toml");
        let mut file = std::fs::File::create(&path).expect("create config");
        write!(
            file,
            r#"
data_dir = "/tmp/kvnc-test-data"
listen_addr = "127.0.0.1:7000"
rpc_addr = "0.0.0.0"
rpc_port = 9999
bootnodes = ["seed.example.org:9000", "127.0.0.1:9001"]
round_duration_ms = 500
max_peers = 7
treasury_address = "treasury-address-placeholder"
"#
        )
        .expect("write config");

        let config = NodeConfig::load(&path).expect("load config");
        assert_eq!(config.data_dir, PathBuf::from("/tmp/kvnc-test-data"));
        assert_eq!(config.listen_addr, "127.0.0.1:7000");
        assert_eq!(config.rpc_addr, "0.0.0.0");
        assert_eq!(config.rpc_port, 9999);
        assert_eq!(config.bootnodes.len(), 2);
        assert_eq!(config.round_duration_ms, 500);
        assert_eq!(config.max_peers, 7);
        assert!(config.validator_key.is_none());
        assert!(config.treasury_address.is_some());
        assert_eq!(config.rpc_socket_addr().expect("socket addr").port(), 9999);
        assert_eq!(
            config.consensus_db_path(),
            PathBuf::from("/tmp/kvnc-test-data/consensus.redb")
        );
    }

    #[test]
    fn missing_file_uses_defaults() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().expect("tempdir");
        let config = NodeConfig::load(dir.path().join("does-not-exist.toml")).expect("defaults");
        assert_eq!(config.rpc_port, 8545);
        assert_eq!(config.max_peers, 50);
        assert_eq!(config.round_duration_ms, 2000);
    }

    #[test]
    fn env_overrides_config() {
        let _guard = env_guard();
        let mut config = NodeConfig::default();
        std::env::set_var("KVNC_RPC_PORT", "1234");
        std::env::set_var("KVNC_MAX_PEERS", "3");
        config.apply_env_overrides();
        std::env::remove_var("KVNC_RPC_PORT");
        std::env::remove_var("KVNC_MAX_PEERS");
        assert_eq!(config.rpc_port, 1234);
        assert_eq!(config.max_peers, 3);
    }
}
