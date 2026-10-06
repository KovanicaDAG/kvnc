//! Contract subcommands (HTLC / vault / multisig / token).
//!
//! These map onto the typed contract RPC handlers in `kvnc-rpc`. Contract
//! parameters use serde's `[u8; 32]` representation, so addresses, hashes and
//! byte blobs are sent as JSON arrays of numbers.

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Value};

use crate::output::print_json;
use crate::rpc::{bytes32_json, bytes_json, RpcClient};

/// Top-level contract command group.
#[derive(Subcommand)]
pub enum ContractCommand {
    /// HTLC atomic swaps (KVP-104)
    Htlc {
        #[command(subcommand)]
        command: HtlcCommand,
    },
    /// Time-lock vaults (KVP-105)
    Vault {
        #[command(subcommand)]
        command: VaultCommand,
    },
    /// Multisig wallets (KVP-101)
    Multisig {
        #[command(subcommand)]
        command: MultisigCommand,
    },
    /// Native multi-asset tokens (KVP-102)
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },
}

/// Dispatch a contract subcommand.
pub async fn run(client: &RpcClient, command: ContractCommand, json_output: bool) -> Result<()> {
    match command {
        ContractCommand::Htlc { command } => run_htlc(client, command, json_output).await,
        ContractCommand::Vault { command } => run_vault(client, command, json_output).await,
        ContractCommand::Multisig { command } => run_multisig(client, command, json_output).await,
        ContractCommand::Token { command } => run_token(client, command, json_output).await,
    }
}

/// Extract a field from a JSON object for human-readable output.
///
/// 32-byte blobs (serde `[u8; 32]`, e.g. `contract_address`) are rendered as
/// `0x`-prefixed hex rather than a raw JSON array.
fn field(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => {
            let is_byte_blob = items.len() == 32
                && items
                    .iter()
                    .all(|v| v.as_u64().map(|n| n <= 0xff).unwrap_or(false));
            if is_byte_blob {
                let bytes: Vec<u8> = items
                    .iter()
                    .map(|v| v.as_u64().unwrap_or_default() as u8)
                    .collect();
                format!("0x{}", hex::encode(bytes))
            } else {
                Value::Array(items.clone()).to_string()
            }
        }
        Some(other) => other.to_string(),
        None => "-".to_string(),
    }
}

/// Print a contract result as JSON or as key/value rows.
fn emit(json_output: bool, result: &Value, rows: &[(&str, String)]) -> Result<()> {
    if json_output {
        return print_json(result);
    }
    let width = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    for (key, value) in rows {
        println!("{key:<width$}  {value}");
    }
    Ok(())
}

// ============================================================================
// HTLC
// ============================================================================

/// HTLC subcommands.
#[derive(Subcommand)]
pub enum HtlcCommand {
    /// Lock funds against a hash lock
    Create(HtlcCreateArgs),
    /// Claim a lock using the preimage
    Claim(HtlcClaimArgs),
    /// Refund a lock after expiry
    Refund(HtlcRefundArgs),
}

/// Arguments for `kvnc htlc create`.
#[derive(Args)]
pub struct HtlcCreateArgs {
    /// Recipient (claimer) address, 32-byte hex
    #[arg(long)]
    pub claimer: String,
    /// Amount in atoms
    #[arg(long)]
    pub amount: u64,
    /// BLAKE3-256 hash lock (64 hex chars)
    #[arg(long = "hash-lock")]
    pub hash_lock: String,
    /// Unix expiry timestamp (seconds)
    #[arg(long)]
    pub expiry: u64,
}

/// Arguments for `kvnc htlc claim`.
#[derive(Args)]
pub struct HtlcClaimArgs {
    /// Swap id (32-byte hex)
    #[arg(long)]
    pub id: String,
    /// Preimage (hex)
    #[arg(long)]
    pub preimage: String,
}

/// Arguments for `kvnc htlc refund`.
#[derive(Args)]
pub struct HtlcRefundArgs {
    /// Swap id (32-byte hex)
    #[arg(long)]
    pub id: String,
}

async fn run_htlc(client: &RpcClient, command: HtlcCommand, json_output: bool) -> Result<()> {
    match command {
        HtlcCommand::Create(args) => {
            let params = json!({
                "claimer": bytes32_json(&args.claimer, "claimer")?,
                "amount": args.amount,
                "hash_lock": bytes32_json(&args.hash_lock, "hash-lock")?,
                "expiry": args.expiry,
            });
            let result = client.call("htlc_create", params).await?;
            emit(
                json_output,
                &result,
                &[("swap_id", field(&result, "swap_id"))],
            )
        }
        HtlcCommand::Claim(args) => {
            let preimage = hex::decode(args.preimage.strip_prefix("0x").unwrap_or(&args.preimage))
                .map_err(|e| anyhow!("preimage: invalid hex: {e}"))?;
            let params = json!({
                "id": bytes32_json(&args.id, "id")?,
                "preimage": bytes_json(&preimage),
            });
            let result = client.call("htlc_claim", params).await?;
            emit(json_output, &result, &[("ok", field(&result, "ok"))])
        }
        HtlcCommand::Refund(args) => {
            let params = json!({ "id": bytes32_json(&args.id, "id")? });
            let result = client.call("htlc_refund", params).await?;
            emit(json_output, &result, &[("ok", field(&result, "ok"))])
        }
    }
}

// ============================================================================
// Vault
// ============================================================================

/// Vault subcommands.
#[derive(Subcommand)]
pub enum VaultCommand {
    /// Create a time-locked vault
    Create(VaultCreateArgs),
    /// Claim the vested amount
    Claim(VaultIdArgs),
    /// Cancel and reclaim the unclaimed balance
    Cancel(VaultIdArgs),
}

/// Arguments for `kvnc vault create`.
#[derive(Args)]
pub struct VaultCreateArgs {
    /// Beneficiary address, 32-byte hex
    #[arg(long)]
    pub beneficiary: String,
    /// Amount in atoms
    #[arg(long)]
    pub amount: u64,
    /// Absolute unlock timestamp (seconds) — mutually exclusive with --start/--end
    #[arg(long = "unlock-at")]
    pub unlock_at: Option<u64>,
    /// Linear vesting start (seconds)
    #[arg(long, requires = "end")]
    pub start: Option<u64>,
    /// Linear vesting end (seconds)
    #[arg(long, requires = "start")]
    pub end: Option<u64>,
    /// Optional cliff for linear vesting (seconds)
    #[arg(long)]
    pub cliff: Option<u64>,
}

/// Arguments carrying only a vault id.
#[derive(Args)]
pub struct VaultIdArgs {
    /// Vault id (32-byte hex)
    #[arg(long)]
    pub id: String,
}

async fn run_vault(client: &RpcClient, command: VaultCommand, json_output: bool) -> Result<()> {
    match command {
        VaultCommand::Create(args) => {
            let schedule = match (args.unlock_at, args.start, args.end) {
                (Some(unlock_at), None, None) => json!({ "Absolute": { "unlock_at": unlock_at } }),
                (None, Some(start), Some(end)) => json!({
                    "Linear": { "start": start, "end": end, "cliff": args.cliff }
                }),
                (Some(_), _, _) => {
                    return Err(anyhow!(
                        "--unlock-at cannot be combined with --start/--end"
                    ))
                }
                (None, _, _) => {
                    return Err(anyhow!(
                        "provide --unlock-at for an absolute vault, or --start and --end for linear vesting"
                    ))
                }
            };

            let params = json!({
                "beneficiary": bytes32_json(&args.beneficiary, "beneficiary")?,
                "amount": args.amount,
                "schedule": schedule,
            });
            let result = client.call("vault_create", params).await?;
            emit(
                json_output,
                &result,
                &[("vault_id", field(&result, "vault_id"))],
            )
        }
        VaultCommand::Claim(args) => {
            let params = json!({ "id": bytes32_json(&args.id, "id")? });
            let result = client.call("vault_claim", params).await?;
            emit(
                json_output,
                &result,
                &[("amount_claimed", field(&result, "amount_claimed"))],
            )
        }
        VaultCommand::Cancel(args) => {
            let params = json!({ "id": bytes32_json(&args.id, "id")? });
            let result = client.call("vault_cancel", params).await?;
            emit(json_output, &result, &[("ok", field(&result, "ok"))])
        }
    }
}

// ============================================================================
// Multisig
// ============================================================================

/// Multisig subcommands.
#[derive(Subcommand)]
pub enum MultisigCommand {
    /// Create an M-of-N multisig
    Create(MultisigCreateArgs),
    /// Propose a transfer (proposer auto-confirms)
    Propose(MultisigProposeArgs),
    /// Confirm a proposed transfer
    Confirm(MultisigTxArgs),
    /// Execute a fully-confirmed transfer
    Execute(MultisigTxArgs),
}

/// Arguments for `kvnc multisig create`.
#[derive(Args)]
pub struct MultisigCreateArgs {
    /// Comma-separated owner addresses (32-byte hex each)
    #[arg(long, value_delimiter = ',')]
    pub owners: Vec<String>,
    /// Required confirmation threshold
    #[arg(long)]
    pub threshold: u32,
}

/// Arguments for `kvnc multisig propose`.
#[derive(Args)]
pub struct MultisigProposeArgs {
    /// Multisig id (32-byte hex)
    #[arg(long)]
    pub id: String,
    /// Recipient address (32-byte hex)
    #[arg(long)]
    pub to: String,
    /// Amount in atoms
    #[arg(long)]
    pub amount: u64,
    /// Optional calldata (hex)
    #[arg(long)]
    pub data: Option<String>,
}

/// Arguments for `kvnc multisig confirm` / `execute`.
#[derive(Args)]
pub struct MultisigTxArgs {
    /// Multisig id (32-byte hex)
    #[arg(long)]
    pub id: String,
    /// Proposal transaction id
    #[arg(long = "tx-id")]
    pub tx_id: u64,
}

async fn run_multisig(
    client: &RpcClient,
    command: MultisigCommand,
    json_output: bool,
) -> Result<()> {
    match command {
        MultisigCommand::Create(args) => {
            let owners: Vec<Value> = args
                .owners
                .iter()
                .map(|owner| bytes32_json(owner, "owner"))
                .collect::<Result<_>>()?;
            let params = json!({ "owners": owners, "threshold": args.threshold });
            let result = client.call("multisig_create", params).await?;
            emit(
                json_output,
                &result,
                &[("multisig_id", field(&result, "multisig_id"))],
            )
        }
        MultisigCommand::Propose(args) => {
            let data = match &args.data {
                Some(hex_str) => {
                    let raw = hex_str.strip_prefix("0x").unwrap_or(hex_str);
                    bytes_json(&hex::decode(raw).map_err(|e| anyhow!("data: invalid hex: {e}"))?)
                }
                None => bytes_json(&[]),
            };
            let params = json!({
                "multisig_id": bytes32_json(&args.id, "id")?,
                "to": bytes32_json(&args.to, "to")?,
                "amount": args.amount,
                "data": data,
            });
            let result = client.call("multisig_propose", params).await?;
            emit(json_output, &result, &[("tx_id", field(&result, "tx_id"))])
        }
        MultisigCommand::Confirm(args) => {
            let params = json!({
                "id": bytes32_json(&args.id, "id")?,
                "tx_id": args.tx_id,
            });
            let result = client.call("multisig_confirm", params).await?;
            emit(json_output, &result, &[("ok", field(&result, "ok"))])
        }
        MultisigCommand::Execute(args) => {
            let params = json!({
                "id": bytes32_json(&args.id, "id")?,
                "tx_id": args.tx_id,
            });
            let result = client.call("multisig_execute", params).await?;
            emit(json_output, &result, &[("ok", field(&result, "ok"))])
        }
    }
}

// ============================================================================
// Token
// ============================================================================

/// Token subcommands.
#[derive(Subcommand)]
pub enum TokenCommand {
    /// Create (deploy) a token
    Create(TokenCreateArgs),
    /// Transfer tokens
    Transfer(TokenAmountToArgs),
    /// Mint tokens (minter only)
    Mint(TokenAmountToArgs),
    /// Burn tokens
    Burn(TokenBurnArgs),
    /// Query a token balance
    Balance(TokenBalanceArgs),
    /// Approve an allowance (not yet supported by the node)
    Approve(TokenApproveArgs),
}

/// Arguments for `kvnc token create`.
#[derive(Args)]
pub struct TokenCreateArgs {
    /// Token name
    #[arg(long)]
    pub name: String,
    /// Token symbol
    #[arg(long)]
    pub symbol: String,
    /// Decimals
    #[arg(long, default_value_t = 9)]
    pub decimals: u8,
    /// Initial supply in base units
    #[arg(long = "initial-supply")]
    pub initial_supply: u64,
}

/// Arguments for `kvnc token transfer` / `mint`.
#[derive(Args)]
pub struct TokenAmountToArgs {
    /// Recipient address (32-byte hex)
    #[arg(long)]
    pub to: String,
    /// Amount in base units
    #[arg(long)]
    pub amount: u64,
}

/// Arguments for `kvnc token burn`.
#[derive(Args)]
pub struct TokenBurnArgs {
    /// Amount in base units
    #[arg(long)]
    pub amount: u64,
}

/// Arguments for `kvnc token balance`.
#[derive(Args)]
pub struct TokenBalanceArgs {
    /// Address to query (32-byte hex)
    #[arg(long)]
    pub address: String,
}

/// Arguments for `kvnc token approve`.
#[derive(Args)]
pub struct TokenApproveArgs {
    /// Spender address (32-byte hex)
    #[arg(long)]
    pub spender: String,
    /// Amount in base units
    #[arg(long)]
    pub amount: u64,
}

async fn run_token(client: &RpcClient, command: TokenCommand, json_output: bool) -> Result<()> {
    match command {
        TokenCommand::Create(args) => {
            let params = json!({
                "name": args.name,
                "symbol": args.symbol,
                "decimals": args.decimals,
                "initial_supply": args.initial_supply,
            });
            let result = client.call("token_create", params).await?;
            emit(
                json_output,
                &result,
                &[("contract_address", field(&result, "contract_address"))],
            )
        }
        TokenCommand::Transfer(args) => {
            let params = json!({
                "to": bytes32_json(&args.to, "to")?,
                "amount": args.amount,
            });
            let result = client.call("token_transfer", params).await?;
            emit(json_output, &result, &[("ok", field(&result, "ok"))])
        }
        TokenCommand::Mint(args) => {
            let params = json!({
                "to": bytes32_json(&args.to, "to")?,
                "amount": args.amount,
            });
            let result = client.call("token_mint", params).await?;
            emit(json_output, &result, &[("ok", field(&result, "ok"))])
        }
        TokenCommand::Burn(args) => {
            let params = json!({ "amount": args.amount });
            let result = client.call("token_burn", params).await?;
            emit(json_output, &result, &[("ok", field(&result, "ok"))])
        }
        TokenCommand::Balance(args) => {
            let params = json!({ "address": bytes32_json(&args.address, "address")? });
            let result = client.call("token_balance", params).await?;
            emit(
                json_output,
                &result,
                &[("amount", field(&result, "amount"))],
            )
        }
        TokenCommand::Approve(_args) => {
            // TODO: `token_approve` is not registered by the node's RPC server
            // yet (only create/transfer/mint/burn/balance exist).
            crate::node::not_implemented(
                "token approve",
                "token_approve",
                json!({ "spender": "<address>", "amount": "<atoms>" }),
                json_output,
            )
        }
    }
}
