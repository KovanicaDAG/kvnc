//! KVNC Faucet Service
//!
//! Rate-limited faucet that dispenses test KVNC to addresses.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use clap::Parser;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};

use ed25519_dalek::{Signer, SigningKey};
use kvnc_cli::wallet;
use kvnc_types::{hash::Hash, Address, PublicKey, Signature, Transaction, TransactionKind};

/// Faucet configuration
#[derive(Clone)]
struct FaucetConfig {
    /// Amount to dispense per request (in atoms)
    dispense_amount: u64,
    /// Rate limit window (seconds)
    rate_limit_window: u64,
    /// Max requests per window per IP
    max_requests_per_window: usize,
    /// RPC endpoint of the node
    rpc_url: String,
    /// Faucet keystore path
    keystore_path: String,
    /// Faucet keystore passphrase
    passphrase: String,
}

/// Rate limiter state
#[derive(Default)]
struct RateLimiter {
    requests: Mutex<std::collections::HashMap<String, Vec<std::time::Instant>>>,
}

impl RateLimiter {
    fn check_and_record(&self, ip: &str, window_secs: u64, max_requests: usize) -> bool {
        let mut requests = self.requests.lock();
        let now = std::time::Instant::now();
        let window_start = now - Duration::from_secs(window_secs);

        let entry = requests.entry(ip.to_string()).or_default();
        entry.retain(|t| *t > window_start);

        if entry.len() >= max_requests {
            return false;
        }

        entry.push(now);
        true
    }
}

/// Faucet state
struct FaucetState {
    config: FaucetConfig,
    signer: SigningKey,
    faucet_address: Address,
    rate_limiter: RateLimiter,
    client: reqwest::Client,
}

/// Request payload
#[derive(Deserialize)]
struct FaucetRequest {
    address: String,
}

/// Response payload
#[derive(Serialize)]
struct FaucetResponse {
    success: bool,
    message: String,
    tx_hash: Option<String>,
}

#[derive(Parser)]
#[command(name = "kvnc-faucet")]
#[command(about = "KVNC Faucet Service")]
struct Cli {
    /// Faucet keystore path
    #[arg(long, default_value = "faucet.keystore")]
    keystore: String,
    /// Faucet keystore passphrase
    #[arg(long, default_value = "faucet-passphrase")]
    passphrase: String,
    /// Amount to dispense per request (in KVNC)
    #[arg(long, default_value = "10")]
    dispense_kvnc: u64,
    /// Rate limit window (seconds)
    #[arg(long, default_value = "3600")]
    rate_limit_window: u64,
    /// Max requests per window per IP
    #[arg(long, default_value = "3")]
    max_requests: usize,
    /// Node RPC URL
    #[arg(long, default_value = "http://127.0.0.1:8545")]
    rpc_url: String,
    /// Listen address
    #[arg(long, default_value = "0.0.0.0:8080")]
    listen: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    // Load faucet keystore
    let keystore = wallet::load(std::path::Path::new(&cli.keystore))
        .with_context(|| format!("loading faucet keystore {}", cli.keystore))?;
    let passphrase = if keystore.encrypted {
        Some(cli.passphrase.as_str())
    } else {
        None
    };
    let seed =
        wallet::secret_seed(&keystore, passphrase).with_context(|| "decrypting faucet keystore")?;
    let signer = SigningKey::from_bytes(&seed);
    let faucet_address = Address::from_public_key(&PublicKey::from(signer.verifying_key()));

    info!(faucet_address = %faucet_address, "faucet identity loaded");

    let config = FaucetConfig {
        dispense_amount: cli.dispense_kvnc * 1_000_000_000, // KVNC to atoms
        rate_limit_window: cli.rate_limit_window,
        max_requests_per_window: cli.max_requests,
        rpc_url: cli.rpc_url.clone(),
        keystore_path: cli.keystore,
        passphrase: cli.passphrase,
    };

    let state = Arc::new(FaucetState {
        config,
        signer,
        faucet_address,
        rate_limiter: RateLimiter::default(),
        client: reqwest::Client::new(),
    });

    let app = Router::new()
        .route("/health", get(health_check))
        .route("/faucet", post(faucet_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&cli.listen).await?;
    info!(addr = %cli.listen, "faucet server listening");
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health_check() -> impl IntoResponse {
    (StatusCode::OK, "OK")
}

async fn faucet_handler(
    State(state): State<Arc<FaucetState>>,
    axum::extract::ConnectInfo(ip): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<FaucetRequest>,
) -> impl IntoResponse {
    let ip_str = ip.ip().to_string();

    // Check rate limit
    if !state.rate_limiter.check_and_record(
        &ip_str,
        state.config.rate_limit_window,
        state.config.max_requests_per_window,
    ) {
        warn!(ip = %ip_str, "rate limit exceeded");
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(FaucetResponse {
                success: false,
                message: format!(
                    "Rate limit exceeded. Max {} requests per {} seconds.",
                    state.config.max_requests_per_window, state.config.rate_limit_window
                ),
                tx_hash: None,
            }),
        )
            .into_response();
    }

    // Parse destination address
    let to_address = match parse_address(&req.address) {
        Ok(addr) => addr,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(FaucetResponse {
                    success: false,
                    message: format!("Invalid address: {}", e),
                    tx_hash: None,
                }),
            )
                .into_response();
        }
    };

    // Prevent faucet from sending to itself
    if to_address == state.faucet_address {
        return (
            StatusCode::BAD_REQUEST,
            Json(FaucetResponse {
                success: false,
                message: "Faucet cannot send to itself".to_string(),
                tx_hash: None,
            }),
        )
            .into_response();
    }

    // Get faucet nonce from RPC
    let nonce = match get_nonce(&state.config.rpc_url, &state.faucet_address).await {
        Ok(n) => n,
        Err(e) => {
            warn!("Failed to get faucet nonce: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(FaucetResponse {
                    success: false,
                    message: "Failed to get faucet nonce".to_string(),
                    tx_hash: None,
                }),
            )
                .into_response();
        }
    };

    // Build transaction
    let tx = Transaction {
        sender: state.faucet_address,
        nonce,
        fee: 1_000_000, // 0.001 KVNC fee
        kind: TransactionKind::Transfer {
            to: to_address,
            amount: state.config.dispense_amount,
        },
        signature: Signature([0; 64]),
        hash: Hash::zero(),
    };

    // Sign transaction
    let signed_tx = match sign_transaction(tx, &state.signer) {
        Ok(tx) => tx,
        Err(e) => {
            warn!("Failed to sign transaction: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(FaucetResponse {
                    success: false,
                    message: "Failed to sign transaction".to_string(),
                    tx_hash: None,
                }),
            )
                .into_response();
        }
    };

    // Submit transaction
    match submit_transaction(&state.config.rpc_url, &signed_tx).await {
        Ok(tx_hash) => {
            info!(to = %to_address, amount = state.config.dispense_amount, tx_hash = %tx_hash, "faucet dispensed");
            (
                StatusCode::OK,
                Json(FaucetResponse {
                    success: true,
                    message: format!(
                        "Dispensed {} KVNC",
                        state.config.dispense_amount / 1_000_000_000
                    ),
                    tx_hash: Some(tx_hash),
                }),
            )
                .into_response()
        }
        Err(e) => {
            warn!("Failed to submit transaction: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(FaucetResponse {
                    success: false,
                    message: "Failed to submit transaction".to_string(),
                    tx_hash: None,
                }),
            )
                .into_response()
        }
    }
}

fn parse_address(s: &str) -> Result<Address> {
    let s = s.trim();
    let s = s.strip_prefix("0x").unwrap_or(s);
    let s = s.strip_prefix("kvnc").unwrap_or(s);
    let s = s.strip_suffix("dag").unwrap_or(s);
    let bytes = hex::decode(s)?;
    if bytes.len() != 32 {
        anyhow::bail!("address must be 32 bytes");
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(Address(arr))
}

async fn get_nonce(rpc_url: &str, address: &Address) -> Result<u64> {
    let client = reqwest::Client::new();
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "kvnc_getNonce",
        "params": [hex::encode(address.0)],
        "id": 1
    });
    let resp: serde_json::Value = client.post(rpc_url).json(&req).send().await?.json().await?;
    let result = resp
        .get("result")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("invalid nonce response"))?;
    let nonce = u64::from_str_radix(result.strip_prefix("0x").unwrap_or(result), 16)?;
    Ok(nonce)
}

fn sign_transaction(mut tx: Transaction, signer: &SigningKey) -> Result<Transaction> {
    let msg = tx.signing_hash();
    let sig = signer.sign(&msg.0);
    tx.signature = Signature(sig.to_bytes());
    tx.hash = msg; // signing_hash already computes the domain-separated hash
    Ok(tx)
}

async fn submit_transaction(rpc_url: &str, tx: &Transaction) -> Result<String> {
    let client = reqwest::Client::new();
    let raw = bincode::serialize(tx)?;
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "kvnc_sendRawTransaction",
        "params": [hex::encode(raw)],
        "id": 1
    });
    let resp: serde_json::Value = client.post(rpc_url).json(&req).send().await?.json().await?;
    let result = resp
        .get("result")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("invalid tx submit response"))?;
    Ok(result.to_string())
}
