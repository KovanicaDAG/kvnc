//! Kovanica (KUNA) Faucet Service
//!
//! Rate-limited faucet that dispenses test KUNA to addresses.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
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
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use tracing::{info, warn};
use zeroize::Zeroizing;

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
    /// Network signing context (signature format v1): every dispensed
    /// transaction commits to this `chain_id`.
    signing_ctx: kvnc_types::SigningContext,
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
#[command(about = "Kovanica (KUNA) Faucet Service")]
struct Cli {
    /// Faucet keystore path
    #[arg(long, default_value = "faucet.keystore")]
    keystore: String,
    /// File containing the keystore passphrase (unix: must be 0600 or 0400).
    /// Alternatives: env KVNC_FAUCET_PASSPHRASE, or an interactive prompt on a TTY.
    #[arg(long)]
    passphrase_file: Option<PathBuf>,
    /// Amount to dispense per request (in KUNA)
    #[arg(long, default_value = "10")]
    dispense_kvnc: u64,
    /// Rate limit window (seconds)
    #[arg(long, default_value = "3600")]
    rate_limit_window: u64,
    /// Max requests per window per IP
    #[arg(long, default_value = "3")]
    max_requests: usize,
    /// Network chain_id committed to by every transaction signature
    /// (1 mainnet, 2 testnet, 3 devnet, 1337 local). No default on purpose.
    #[arg(long)]
    chain_id: u64,
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
    if !kvnc_types::signing::chain_id::is_registered(cli.chain_id) {
        anyhow::bail!(
            "unknown chain id {}: expected 1 (mainnet), 2 (testnet), 3 (devnet) or 1337 (local)",
            cli.chain_id
        );
    }

    // Load faucet keystore
    let keystore = wallet::load(std::path::Path::new(&cli.keystore))
        .with_context(|| format!("loading faucet keystore {}", cli.keystore))?;
    // The passphrase is only needed to decrypt the seed; it is wiped right
    // after and never stored in config/state or logged.
    let cli_passphrase = if keystore.encrypted {
        Some(resolve_passphrase(cli.passphrase_file.as_deref())?)
    } else {
        None
    };
    let passphrase = cli_passphrase.as_ref().map(|p| p.as_str());
    let seed =
        wallet::secret_seed(&keystore, passphrase).with_context(|| "decrypting faucet keystore")?;
    let signer = SigningKey::from_bytes(&seed);
    drop(cli_passphrase);
    let faucet_address = Address::from_public_key(&PublicKey::from(signer.verifying_key()));

    info!(faucet_address = %faucet_address, "faucet identity loaded");

    let config = FaucetConfig {
        dispense_amount: cli.dispense_kvnc * 1_000_000_000, // KUNA to atoms
        rate_limit_window: cli.rate_limit_window,
        max_requests_per_window: cli.max_requests,
        rpc_url: cli.rpc_url.clone(),
        signing_ctx: kvnc_types::SigningContext::new(cli.chain_id),
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
    let nonce = match get_nonce(&state.client, &state.config.rpc_url, &state.faucet_address).await {
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
        fee: 1_000_000, // 0.001 KUNA fee
        kind: TransactionKind::Transfer {
            to: to_address,
            amount: state.config.dispense_amount,
        },
        signature: Signature([0; 64]),
        hash: Hash::zero(),
    };

    // Sign transaction
    let signed_tx = match sign_transaction(tx, &state.signer, &state.config.signing_ctx) {
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
    match submit_transaction(&state.client, &state.config.rpc_url, &signed_tx).await {
        Ok(tx_hash) => {
            info!(to = %to_address, amount = state.config.dispense_amount, tx_hash = %tx_hash, "faucet dispensed");
            (
                StatusCode::OK,
                Json(FaucetResponse {
                    success: true,
                    message: format!(
                        "Dispensed {} KUNA",
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

async fn get_nonce(client: &reqwest::Client, rpc_url: &str, address: &Address) -> Result<u64> {
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

fn sign_transaction(
    mut tx: Transaction,
    signer: &SigningKey,
    ctx: &kvnc_types::SigningContext,
) -> Result<Transaction> {
    let msg = tx.signing_hash(ctx);
    let sig = signer.sign(&msg.0);
    tx.signature = Signature(sig.to_bytes());
    tx.hash = msg; // signing_hash(ctx) is the v1 domain- and chain-separated hash
    Ok(tx)
}

async fn submit_transaction(
    client: &reqwest::Client,
    rpc_url: &str,
    tx: &Transaction,
) -> Result<String> {
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

/// Environment variable holding the faucet keystore passphrase.
const PASSPHRASE_ENV: &str = "KVNC_FAUCET_PASSPHRASE";

/// Resolve the keystore passphrase without ever taking it from argv.
///
/// Order: `--passphrase-file`, then `KVNC_FAUCET_PASSPHRASE`, then an
/// interactive no-echo prompt if stdin is a TTY. Errors if none is available.
fn resolve_passphrase(file: Option<&Path>) -> Result<Zeroizing<String>> {
    let env = std::env::var(PASSPHRASE_ENV).ok().map(Zeroizing::new);
    resolve_passphrase_from(file, env, || {
        if std::io::stdin().is_terminal() {
            let p = rpassword::prompt_password("Faucet keystore passphrase: ")
                .context("reading passphrase from TTY")?;
            Ok(Some(Zeroizing::new(p)))
        } else {
            Ok(None)
        }
    })
}

fn resolve_passphrase_from<F>(
    file: Option<&Path>,
    env: Option<Zeroizing<String>>,
    prompt: F,
) -> Result<Zeroizing<String>>
where
    F: FnOnce() -> Result<Option<Zeroizing<String>>>,
{
    if let Some(path) = file {
        return read_passphrase_file(path);
    }
    if let Some(p) = env {
        if !p.is_empty() {
            return Ok(p);
        }
    }
    if let Some(p) = prompt()? {
        return Ok(p);
    }
    bail!(
        "keystore is encrypted but no passphrase source is available: \
         use --passphrase-file <path>, set {PASSPHRASE_ENV}, or run on a TTY"
    )
}

fn read_passphrase_file(path: &Path) -> Result<Zeroizing<String>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .with_context(|| format!("reading metadata of passphrase file {}", path.display()))?
            .permissions()
            .mode()
            & 0o777;
        if mode & 0o077 != 0 {
            bail!(
                "refusing passphrase file {}: permissions {:o} are too broad (use 0600 or 0400)",
                path.display(),
                mode
            );
        }
    }
    let mut raw = Zeroizing::new(
        std::fs::read_to_string(path)
            .with_context(|| format!("reading passphrase file {}", path.display()))?,
    );
    while raw.ends_with('\n') || raw.ends_with('\r') {
        raw.pop();
    }
    if raw.is_empty() {
        bail!("passphrase file {} is empty", path.display());
    }
    Ok(raw)
}

#[cfg(test)]
mod passphrase_tests {
    use super::*;
    use std::io::Write;

    fn write_file(contents: &str, mode: u32) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(mode)).unwrap();
        }
        #[cfg(not(unix))]
        let _ = mode;
        f
    }

    fn no_prompt() -> Result<Option<Zeroizing<String>>> {
        Ok(None)
    }

    #[test]
    fn file_trims_trailing_newline_and_wins_over_env() {
        let f = write_file("s3cret\r\n", 0o600);
        let env = Some(Zeroizing::new("from-env".to_string()));
        let p = resolve_passphrase_from(Some(f.path()), env, no_prompt).unwrap();
        assert_eq!(p.as_str(), "s3cret");
    }

    #[test]
    fn file_0400_is_accepted() {
        let f = write_file("pw\n", 0o400);
        assert_eq!(read_passphrase_file(f.path()).unwrap().as_str(), "pw");
    }

    #[cfg(unix)]
    #[test]
    fn file_with_broad_permissions_is_refused() {
        let f = write_file("pw\n", 0o644);
        let err = read_passphrase_file(f.path()).unwrap_err().to_string();
        assert!(err.contains("too broad"), "{err}");
        assert!(!err.contains("pw\n"));
    }

    #[test]
    fn env_used_when_no_file() {
        let env = Some(Zeroizing::new("from-env".to_string()));
        let p = resolve_passphrase_from(None, env, no_prompt).unwrap();
        assert_eq!(p.as_str(), "from-env");
    }

    #[test]
    fn prompt_used_as_last_resort() {
        let p =
            resolve_passphrase_from(None, None, || Ok(Some(Zeroizing::new("typed".to_string()))))
                .unwrap();
        assert_eq!(p.as_str(), "typed");
    }

    #[test]
    fn errors_clearly_when_no_source() {
        let err = resolve_passphrase_from(None, Some(Zeroizing::new(String::new())), no_prompt)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("--passphrase-file") && err.contains(PASSPHRASE_ENV),
            "{err}"
        );
    }
}
