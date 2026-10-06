//! KVNC command-line interface.
//!
//! Wallet operations (keygen/import/export/sign), node operations
//! (status/balance/staking/governance) and contract subcommands
//! (htlc/vault/multisig/token). All commands support `--json` for
//! machine-readable output and `--rpc-url` to target a specific node.

mod contracts;
mod node;
mod output;
mod rpc;
mod tx;
mod wallet;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use serde_json::json;

use contracts::{ContractCommand, HtlcCommand, MultisigCommand, TokenCommand, VaultCommand};
use kvnc_types::transaction::TransactionKind;
use rpc::RpcClient;

#[derive(Parser)]
#[command(name = "kvnc", version, about = "Kovanica (KVNC) CLI", long_about = None)]
struct Cli {
    /// JSON-RPC endpoint of the node
    #[arg(long, global = true, default_value = "http://127.0.0.1:8545")]
    rpc_url: String,

    /// Emit machine-readable JSON instead of human-readable tables
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate a new Ed25519 keypair and save a keystore
    Keygen(KeygenArgs),
    /// Load a keystore and print its address
    Import(KeystoreArgs),
    /// Export the private key from a keystore as hex
    Export(KeystoreArgs),
    /// Sign a message offline and print the signature as hex
    Sign(SignArgs),
    /// Submit a native KVNC transfer
    Transfer(TransferArgs),
    /// Show chain/node status
    Status,
    /// Show chain info (alias of `status`)
    Info,
    /// Query an account balance
    Balance { address: String },
    /// Stake KVNC (not yet supported by the node)
    Stake(AmountArgs),
    /// Unstake KVNC (not yet supported by the node)
    Unstake(AmountArgs),
    /// Delegate stake to a validator (not yet supported by the node)
    Delegate(DelegateArgs),
    /// Claim staking rewards (not yet supported by the node)
    ClaimRewards,
    /// Submit a governance proposal (not yet supported by the node)
    Propose(ProposeArgs),
    /// Vote on a governance proposal (not yet supported by the node)
    Vote(VoteArgs),
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

/// Arguments for `kvnc keygen`.
#[derive(Args)]
struct KeygenArgs {
    /// Output keystore path
    #[arg(long, short, default_value = "keystore.json")]
    out: PathBuf,
    /// Encrypt the private key with this passphrase
    #[arg(long)]
    passphrase: Option<String>,
    /// Overwrite an existing keystore file
    #[arg(long)]
    force: bool,
}

/// Arguments selecting an existing keystore.
#[derive(Args)]
struct KeystoreArgs {
    /// Keystore file path
    #[arg(long, short, default_value = "keystore.json")]
    keystore: PathBuf,
    /// Passphrase to decrypt the keystore
    #[arg(long)]
    passphrase: Option<String>,
}

/// Arguments for `kvnc sign`.
#[derive(Args)]
struct SignArgs {
    /// Keystore file path
    #[arg(long, short, default_value = "keystore.json")]
    keystore: PathBuf,
    /// Passphrase to decrypt the keystore
    #[arg(long)]
    passphrase: Option<String>,
    /// Message to sign
    #[arg(long)]
    message: String,
    /// Interpret `--message` as hex bytes instead of UTF-8 text
    #[arg(long)]
    hex: bool,
}

/// Arguments for `kvnc transfer`.
#[derive(Args)]
struct TransferArgs {
    /// Recipient address (32-byte hex)
    #[arg(long)]
    to: String,
    /// Amount in atoms
    #[arg(long)]
    amount: u64,
    /// Keystore file path
    #[arg(long, short, default_value = "keystore.json")]
    keystore: PathBuf,
    /// Passphrase to decrypt the keystore
    #[arg(long)]
    passphrase: Option<String>,
    /// Fee in atoms (the mempool rejects zero-fee transactions)
    #[arg(long, default_value_t = 1)]
    fee: u64,
}

/// Arguments carrying a single amount.
#[derive(Args)]
struct AmountArgs {
    /// Amount in atoms
    #[arg(long)]
    amount: u64,
}

/// Arguments for `kvnc delegate`.
#[derive(Args)]
struct DelegateArgs {
    /// Validator address (32-byte hex)
    #[arg(long)]
    validator: String,
    /// Amount in atoms
    #[arg(long)]
    amount: u64,
}

/// Arguments for `kvnc propose`.
#[derive(Args)]
struct ProposeArgs {
    /// Proposal title
    #[arg(long)]
    title: String,
}

/// Arguments for `kvnc vote`.
#[derive(Args)]
struct VoteArgs {
    /// Proposal id
    #[arg(long = "proposal-id")]
    proposal_id: u64,
    /// Vote in favour (omit for against)
    #[arg(long)]
    approve: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let json = cli.json;
    let client = RpcClient::new(cli.rpc_url);

    match cli.command {
        Commands::Keygen(args) => cmd_keygen(args, json),
        Commands::Import(args) => cmd_import(args, json),
        Commands::Export(args) => cmd_export(args, json),
        Commands::Sign(args) => cmd_sign(args, json),
        Commands::Transfer(args) => cmd_transfer(&client, args, json).await,
        Commands::Status | Commands::Info => node::status(&client, json).await,
        Commands::Balance { address } => node::balance(&client, &address, json).await,
        Commands::Stake(args) => node::not_implemented(
            "stake",
            "kvnc_stake",
            json!({ "amount": args.amount }),
            json,
        ),
        Commands::Unstake(args) => node::not_implemented(
            "unstake",
            "kvnc_unstake",
            json!({ "amount": args.amount }),
            json,
        ),
        Commands::Delegate(args) => node::not_implemented(
            "delegate",
            "kvnc_delegate",
            json!({ "validator": args.validator, "amount": args.amount }),
            json,
        ),
        Commands::ClaimRewards => {
            node::not_implemented("claim-rewards", "kvnc_claimRewards", json!({}), json)
        }
        Commands::Propose(args) => node::not_implemented(
            "propose",
            "kvnc_propose",
            json!({ "title": args.title }),
            json,
        ),
        Commands::Vote(args) => node::not_implemented(
            "vote",
            "kvnc_vote",
            json!({ "proposal_id": args.proposal_id, "approve": args.approve }),
            json,
        ),
        Commands::Htlc { command } => {
            contracts::run(&client, ContractCommand::Htlc { command }, json).await
        }
        Commands::Vault { command } => {
            contracts::run(&client, ContractCommand::Vault { command }, json).await
        }
        Commands::Multisig { command } => {
            contracts::run(&client, ContractCommand::Multisig { command }, json).await
        }
        Commands::Token { command } => {
            contracts::run(&client, ContractCommand::Token { command }, json).await
        }
    }
}

fn cmd_keygen(args: KeygenArgs, json_output: bool) -> Result<()> {
    if args.out.exists() && !args.force {
        anyhow::bail!(
            "{} already exists (use --force to overwrite)",
            args.out.display()
        );
    }

    let keystore = wallet::keygen(args.passphrase.as_deref())?;
    wallet::save(&args.out, &keystore)?;

    if json_output {
        return output::print_json(&json!({
            "keystore": args.out.display().to_string(),
            "address": keystore.address,
            "publicKey": keystore.public_key,
            "encrypted": keystore.encrypted,
        }));
    }

    println!("Keystore written to {}", args.out.display());
    println!("Address:    {}", keystore.address);
    println!("Public key: {}", keystore.public_key);
    if !keystore.encrypted {
        println!("WARNING: keystore is not encrypted (no --passphrase given).");
    }
    Ok(())
}

fn cmd_import(args: KeystoreArgs, json_output: bool) -> Result<()> {
    let keystore = wallet::load(&args.keystore)?;
    let public_key = wallet::public_key(&keystore)?;

    if json_output {
        return output::print_json(&json!({
            "keystore": args.keystore.display().to_string(),
            "address": keystore.address,
            "publicKey": hex::encode(public_key.as_bytes()),
            "encrypted": keystore.encrypted,
        }));
    }

    println!("Keystore:   {}", args.keystore.display());
    println!("Address:    {}", keystore.address);
    println!("Public key: {}", hex::encode(public_key.as_bytes()));
    println!("Encrypted:  {}", keystore.encrypted);
    Ok(())
}

fn cmd_export(args: KeystoreArgs, json_output: bool) -> Result<()> {
    let keystore = wallet::load(&args.keystore)?;
    let seed = wallet::secret_seed(&keystore, args.passphrase.as_deref())?;
    let private_key = hex::encode(seed);

    if json_output {
        return output::print_json(&json!({
            "address": keystore.address,
            "privateKey": private_key,
        }));
    }

    println!("{private_key}");
    Ok(())
}

fn cmd_sign(args: SignArgs, json_output: bool) -> Result<()> {
    let keystore = wallet::load(&args.keystore)?;
    let signing_key = wallet::signing_key(&keystore, args.passphrase.as_deref())?;

    let message = if args.hex {
        let raw = args.message.strip_prefix("0x").unwrap_or(&args.message);
        hex::decode(raw).context("--message is not valid hex")?
    } else {
        args.message.clone().into_bytes()
    };

    let signature = kvnc_crypto::sign(&signing_key, &message);
    let signature_hex = hex::encode(signature.as_bytes());

    if json_output {
        return output::print_json(&json!({
            "address": keystore.address,
            "message": args.message,
            "messageHex": hex::encode(&message),
            "signature": signature_hex,
        }));
    }

    println!("{signature_hex}");
    Ok(())
}

async fn cmd_transfer(client: &RpcClient, args: TransferArgs, json_output: bool) -> Result<()> {
    let keystore = wallet::load(&args.keystore)?;
    let signing_key = wallet::signing_key(&keystore, args.passphrase.as_deref())?;
    let sender = wallet::address(&keystore)?;
    let recipient = wallet::parse_address(&args.to, "to")?;

    // The account nonce is the next unused nonce, so it is used as-is.
    let nonce_value = client
        .call("kvnc_getNonce", json!([sender.to_hex()]))
        .await?;
    let nonce = rpc::parse_quantity(&nonce_value)?;

    let kind = TransactionKind::Transfer {
        to: recipient,
        amount: args.amount,
    };
    let transaction = tx::build_signed(sender, nonce, kind, args.fee, &signing_key)?;
    let raw = tx::encode_raw(&transaction)?;

    let result = client.call("kvnc_sendRawTransaction", json!([raw])).await?;
    let tx_hash = result.as_str().unwrap_or_default().to_string();

    if json_output {
        return output::print_json(&json!({
            "transactionHash": tx_hash,
            "sender": sender.to_hex(),
            "to": recipient.to_hex(),
            "amount": args.amount,
            "fee": args.fee,
            "nonce": nonce,
        }));
    }

    println!("Submitted transaction {tx_hash}");
    println!("  from:   {}", sender.to_hex());
    println!("  to:     {}", recipient.to_hex());
    println!("  amount: {} atoms", args.amount);
    println!("  fee:    {} atoms", args.fee);
    println!("  nonce:  {nonce}");
    Ok(())
}
