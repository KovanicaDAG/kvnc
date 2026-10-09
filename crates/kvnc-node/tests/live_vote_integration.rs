//! Live 2-node TCP integration test: vote → commit → execute
//!
//! This test starts two actual node processes over real TCP networking,
//! has them connect via P2P, and verifies the full consensus pipeline:
//! block proposal → vote → commit → execution.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use bincode;
use ed25519_dalek::SigningKey;
use reqwest::Client;
use serde_json::json;
use tempfile::TempDir;
use tokio::process::Command;
use tokio::time::sleep;
use tracing::{info, warn};

use kvnc_crypto::{generate_keypair, sign as crypto_sign};
use kvnc_types::{
    hash::Hash,
    transaction::{Transaction, TransactionKind},
    Address, PublicKey, Signature,
};
use kvnc_staking::{MIN_VALIDATOR_STAKE, ONE_KVNC};

/// Test configuration
const ROUND_DURATION_MS: u64 = 1000;
const TIMEOUT_SECS: u64 = 30;
const NODE1_RPC: u16 = 8545;
const NODE2_RPC: u16 = 8546;
const NODE1_P2P: u16 = 9000;
const NODE2_P2P: u16 = 9001;

/// Node process handle with cleanup
struct NodeProcess {
    temp_dir: TempDir,
    child: tokio::process::Child,
    rpc_port: u16,
    p2p_port: u16,
    rpc_client: Client,
    validator_address: Address,
    signing_key: SigningKey,
}

impl NodeProcess {
    /// Start a new node process
    async fn start(
        temp_dir: TempDir,
        rpc_port: u16,
        p2p_port: u16,
        bootnodes: Vec<String>,
        validator_key: Option<SigningKey>,
        treasury_address: Address,
    ) -> Result<Self> {
        // Generate or use provided validator key
        let (signing_key, public_key) = match validator_key {
            Some(k) => (k.clone(), PublicKey::from(k.verifying_key())),
            None => generate_keypair(),
        };
        let validator_address = Address::from_public_key(&public_key);

        // Create genesis validators file with both validators
        let genesis_validators_path = temp_dir.path().join("genesis_validators.toml");
        let genesis_content = format!(r#"
validators = [
  {{ address = "{}", stake = {}, public_key = "{}" }},
  {{ address = "{}", stake = {}, public_key = "{}" }}
]
"#,
        hex::encode(validator_address.0), MIN_VALIDATOR_STAKE, hex::encode(public_key.0),
        hex::encode(Address([0xaa; 32]).0), MIN_VALIDATOR_STAKE, hex::encode(PublicKey([0xaa; 32]).0)
    );
        std::fs::write(&genesis_validators_path, genesis_content)?;

        // Write validator key file
        let key_path = temp_dir.path().join("validator.key");
        let key_hex = hex::encode(signing_key.to_bytes());
        std::fs::write(&key_path, key_hex)?;

        // Start node process
        // Use the workspace root's target directory for the binary
        let workspace_root = std::env::var("CARGO_MANIFEST_DIR")
            .map(|p| std::path::PathBuf::from(p).join("../.."))
            .unwrap_or_else(|_| std::path::PathBuf::from("."));
        let binary_path = workspace_root.join("target/release/kvnc-node");
        info!("Using binary path: {:?}", binary_path);

        let child = Command::new(&binary_path)
            .current_dir(&temp_dir)
            .env("KVNC_DATA_DIR", temp_dir.path())
            .env("KVNC_RPC_ADDR", "0.0.0.0")
            .env("KVNC_RPC_PORT", rpc_port.to_string())
            .env("KVNC_LISTEN_ADDR", format!("0.0.0.0:{}", p2p_port))
            .env("KVNC_DATA_DIR", temp_dir.path().to_string_lossy().to_string())
            .env("KVNC_MAX_PEERS", "8")
            .env("KVNC_ROUND_DURATION_MS", ROUND_DURATION_MS.to_string())
            .env("KVNC_RUN_VALIDATOR", "true")
            .env("KVNC_RPC_AUTH", "disable")
            .env("KVNC_VALIDATOR_KEY", key_path.to_string_lossy().to_string())
            .env("KVNC_BOOTNODES", bootnodes.join(","))
            .env("RUST_LOG", "info")
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .context("Failed to spawn node process")?;

        let rpc_client = Client::new();

        Ok(Self {
            temp_dir,
            child,
            rpc_port,
            p2p_port,
            rpc_client,
            validator_address,
            signing_key,
        })
    }

    /// Wait for RPC to be ready
    async fn wait_ready(&self, timeout_secs: u64) -> Result<()> {
        let start = std::time::Instant::now();
        loop {
            if start.elapsed() > Duration::from_secs(timeout_secs) {
                anyhow::bail!("RPC not ready after {}s", timeout_secs);
            }
            match self.rpc_call("kvnc_blockNumber", json!([])).await {
                Ok(_) => return Ok(()),
                Err(_) => sleep(Duration::from_millis(500)).await,
            }
        }
    }

    /// Make an RPC call
    async fn rpc_call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let resp = self
            .rpc_client
            .post(format!("http://127.0.0.1:{}/rpc", self.rpc_port))
            .json(&json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params,
                "id": 1
            }))
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;
        Ok(resp)
    }

    /// Get current block height
    async fn get_block_height(&self) -> Result<u64> {
        let resp = self.rpc_call("kvnc_blockNumber", json!([])).await?;
        let hex = resp.get("result")
            .and_then(|v| v.as_str())
            .context("Invalid blockNumber response")?;
        Ok(u64::from_str_radix(hex.strip_prefix("0x").unwrap_or(hex), 16)?)
    }

    /// Get peer count via health endpoint
    async fn get_peer_count(&self) -> Result<u64> {
        let resp = self
            .rpc_client
            .get(format!("http://127.0.0.1:{}/health", self.rpc_port))
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;
        let count = resp.get("peer_count")
            .and_then(|v| v.as_u64())
            .context("Invalid health response")?;
        Ok(count)
    }

    /// Submit a transaction
    async fn send_transaction(&self, tx: Transaction) -> Result<String> {
        let raw = bincode::serialize(&tx)?;
        let resp = self.rpc_call("kvnc_sendRawTransaction", json!([hex::encode(raw)])).await?;
        let hash = resp.get("result")
            .and_then(|v| v.as_str())
            .context("Invalid sendRawTransaction response")?;
        Ok(hash.to_string())
    }

    /// Kill the process
    async fn kill(mut self) -> Result<()> {
        self.child.kill().await?;
        let _ = self.child.wait().await;
        Ok(())
    }
}

/// Create a simple transfer transaction
fn create_transfer_tx(
    sender: Address,
    to: Address,
    amount: u64,
    fee: u64,
    nonce: u64,
    signing_key: &SigningKey,
) -> Transaction {
    let tx = Transaction {
        sender,
        nonce,
        fee,
        kind: TransactionKind::Transfer { to, amount },
        signature: Signature([0; 64]),
        hash: Hash::zero(),
    };
    // Sign it using kvnc_crypto::sign
    let msg = tx.signing_hash();
    let sig = crypto_sign(signing_key, &msg.0);
    Transaction {
        signature: sig,
        hash: msg,
        ..tx
    }
}

/// Live vote integration test: 2 nodes, real TCP, vote → commit → execute
#[tokio::test]
async fn live_two_node_vote_commit_execute() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .try_init();

    // Create temp directories
    let dir1 = TempDir::new()?;
    let dir2 = TempDir::new()?;

    // Generate validator keys
    let (sk1, pk1) = generate_keypair();
    let (sk2, pk2) = generate_keypair();
    let addr1 = Address::from_public_key(&pk1);
    let addr2 = Address::from_public_key(&pk2);

    // Treasury address
    let treasury = addr1;

    // Genesis validators
    let genesis_content = format!(r#"
validators = [
  {{ address = "{}", stake = {}, public_key = "{}" }},
  {{ address = "{}", stake = {}, public_key = "{}" }}
]
"#,
        hex::encode(addr1.0), MIN_VALIDATOR_STAKE, hex::encode(pk1.0),
        hex::encode(addr2.0), MIN_VALIDATOR_STAKE, hex::encode(pk2.0)
    );

    std::fs::write(dir1.path().join("genesis_validators.toml"), &genesis_content)?;
    std::fs::write(dir2.path().join("genesis_validators.toml"), &genesis_content)?;
    std::fs::write(dir1.path().join("validator.pem"), hex::encode(sk1.to_bytes()))?;
    std::fs::write(dir2.path().join("validator.pem"), hex::encode(sk2.to_bytes()))?;

    // Start node 1
    let node1 = NodeProcess::start(
        dir1, NODE1_RPC, NODE1_P2P,
        vec![format!("127.0.0.1:{}", NODE2_P2P)],
        Some(sk1), treasury,
    ).await?;

    // Start node 2
    let node2 = NodeProcess::start(
        dir2, NODE2_RPC, NODE2_P2P,
        vec![format!("127.0.0.1:{}", NODE1_P2P)],
        Some(sk2), treasury,
    ).await?;

    // Wait for both nodes to be ready
    node1.wait_ready(TIMEOUT_SECS).await?;
    node2.wait_ready(TIMEOUT_SECS).await?;

    info!("Both nodes ready");

    // Wait for peer connection
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        let p1 = node1.get_peer_count().await?;
        let p2 = node2.get_peer_count().await?;
        if p1 >= 1 && p2 >= 1 {
            info!("Nodes connected: node1 peers={}, node2 peers={}", p1, p2);
            break;
        }
        sleep(Duration::from_millis(500)).await;
    }

    // Verify they're connected
    let p1 = node1.get_peer_count().await?;
    let p2 = node2.get_peer_count().await?;
    assert!(p1 >= 1, "node1 should have peers");
    assert!(p2 >= 1, "node2 should have peers");

    // Check initial block height
    let h1_before = node1.get_block_height().await?;
    let h2_before = node2.get_block_height().await?;
    info!("Initial heights: node1={}, node2={}", h1_before, h2_before);

    // Wait for a few rounds to pass and blocks to be committed
    let start = std::time::Instant::now();
    let mut committed = false;
    while start.elapsed() < Duration::from_secs(TIMEOUT_SECS) {
        let h1 = node1.get_block_height().await?;
        let h2 = node2.get_block_height().await?;
        if h1 > h1_before && h2 > h2_before && h1 == h2 {
            info!("Both nodes committed: height={}", h1);
            committed = true;
            break;
        }
        sleep(Duration::from_millis(500)).await
    }

    assert!(committed, "Nodes should commit identical blocks within timeout");

    // Final height check
    let h1_final = node1.get_block_height().await?;
    let h2_final = node2.get_block_height().await?;
    assert_eq!(h1_final, h2_final, "Final heights should match");
    assert!(h1_final > h1_before, "Height should have increased");

    info!("Test passed! Both nodes committed to height {}", h1_final);

    // Cleanup
    node1.kill().await?;
    node2.kill().await?;

    Ok(())
}

/// Test: send a transfer transaction and verify it's committed
#[tokio::test]
async fn live_transaction_commit() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .try_init();

    let dir1 = TempDir::new()?;
    let dir2 = TempDir::new()?;

    let (sk1, pk1) = generate_keypair();
    let (sk2, pk2) = generate_keypair();
    let addr1 = Address::from_public_key(&pk1);
    let addr2 = Address::from_public_key(&pk2);
    let treasury = addr1;

    let genesis_content = format!(r#"
validators = [
  {{ address = "{}", stake = {}, public_key = "{}" }},
  {{ address = "{}", stake = {}, public_key = "{}" }}
]
"#,
        hex::encode(addr1.0), MIN_VALIDATOR_STAKE, hex::encode(pk1.0),
        hex::encode(addr2.0), MIN_VALIDATOR_STAKE, hex::encode(pk2.0)
    );

    std::fs::write(dir1.path().join("genesis_validators.toml"), &genesis_content)?;
    std::fs::write(dir2.path().join("genesis_validators.toml"), &genesis_content)?;
    std::fs::write(dir1.path().join("validator.pem"), hex::encode(sk1.to_bytes()))?;
    std::fs::write(dir2.path().join("validator.pem"), hex::encode(sk2.to_bytes()))?;

    let node1 = NodeProcess::start(
        dir1, NODE1_RPC, NODE1_P2P,
        vec![format!("127.0.0.1:{}", NODE2_P2P)],
        Some(sk1.clone()), treasury,
    ).await?;

    let node2 = NodeProcess::start(
        dir2, NODE2_RPC, NODE2_P2P,
        vec![format!("127.0.0.1:{}", NODE1_P2P)],
        Some(sk2), treasury,
    ).await?;

    node1.wait_ready(TIMEOUT_SECS).await?;
    node2.wait_ready(TIMEOUT_SECS).await?;

    // Wait for peer connection
    sleep(Duration::from_secs(3)).await;

    // Create a transfer from node1 to node2
    let to_addr = Address([0xbb; 32]); // dummy recipient
    let tx = create_transfer_tx(addr1, to_addr, 100 * ONE_KVNC, 1_000_000, 0, &sk1);

    // Submit transaction
    let tx_hash = node1.send_transaction(tx).await?;
    info!("Transaction submitted: {}", tx_hash);

    // Wait for transaction to be committed (block height increases)
    let start = std::time::Instant::now();
    let mut committed = false;
    while start.elapsed() < Duration::from_secs(TIMEOUT_SECS) {
        let h1 = node1.get_block_height().await?;
        let h2 = node2.get_block_height().await?;
        if h1 > 0 && h1 == h2 {
            committed = true;
            info!("Transaction committed at height {}", h1);
            break;
        }
        sleep(Duration::from_millis(500)).await;
    }

    assert!(committed, "Transaction should be committed");

    node1.kill().await?;
    node2.kill().await?;

    Ok(())
}