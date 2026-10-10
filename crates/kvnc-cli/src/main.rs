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
mod stake;
mod tx;
pub mod wallet;

use std::{
    io::{self, Write},
    path::PathBuf,
};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde_json::json;

use contracts::{ContractCommand, HtlcCommand, MultisigCommand, TokenCommand, VaultCommand};
use kvnc_types::transaction::TransactionKind;
use kvnc_types::{crypto::PublicKey, crypto::SigningKey, Address};
use rpc::RpcClient;
use zeroize::Zeroizing;

const UNSAFE_SIGN_WARNING: &str = "WARNING: arbitrary message signing is unsafe and cross-context; signatures may be replayed or misinterpreted by other protocols.";

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
    /// Explicitly export private seed material from a keystore
    Export(ExportArgs),
    /// Import a raw seed or 24-word English BIP-39 entropy phrase (hidden; no BIP-39 passphrase/HD derivation)
    ImportKey(ImportKeyArgs),
    /// Explicitly migrate a legacy v1 keystore to authenticated v2 encryption
    Migrate(MigrateArgs),
    /// Derive the canonical address + public key from a raw 32-byte hex seed file (offline; never prints the seed)
    Address(AddressArgs),
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
    /// Stake KVNC (builds and submits a signed Stake transaction)
    Stake(AmountArgs),
    /// Unstake KVNC (builds and submits a signed Unstake transaction)
    Unstake(AmountArgs),
    /// Delegate stake to a validator
    Delegate(DelegateArgs),
    /// Claim staking rewards
    ClaimRewards(ClaimRewardsArgs),
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
}

#[derive(Clone, Copy, ValueEnum)]
enum SecretFormat {
    Raw,
    Mnemonic,
}

#[derive(Args)]
struct ExportArgs {
    #[command(flatten)]
    keystore: KeystoreArgs,
    /// Explicit secret export format; mnemonic is seed entropy, not BIP-39 passphrase/HD-derived
    #[arg(long, value_enum)]
    format: SecretFormat,
}

#[derive(Args)]
struct ImportKeyArgs {
    /// Input encoding for the hidden prompt
    #[arg(long, value_enum)]
    format: SecretFormat,
    /// Output keystore path
    #[arg(long, short, default_value = "keystore.json")]
    out: PathBuf,
    /// Overwrite only after explicit interactive confirmation
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
struct MigrateArgs {
    /// Legacy v1 source path
    #[arg(long, short, default_value = "keystore.json")]
    keystore: PathBuf,
    /// Destination path (defaults to <source>.v2.json)
    #[arg(long, short)]
    out: Option<PathBuf>,
    /// Replace the v1 source only after destination verification and typed confirmation
    #[arg(long)]
    replace_source: bool,
}

/// Arguments for `kvnc address`.
#[derive(Args)]
struct AddressArgs {
    /// File containing a 32-byte hex seed (the seed itself is never printed)
    #[arg(long, value_name = "FILE")]
    key_file: PathBuf,
    /// Allow raw hex seed import (disabled by default for security; enable with caution)
    #[arg(long, default_value_t = false)]
    allow_raw_hex: bool,
}

/// Arguments for `kvnc sign`.
#[derive(Args)]
struct SignArgs {
    /// Keystore file path
    #[arg(long, short, default_value = "keystore.json")]
    keystore: PathBuf,
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
    /// Fee in atoms (the mempool rejects zero-fee transactions)
    #[arg(long, default_value_t = 1)]
    fee: u64,
}

/// Arguments carrying a single amount plus the signing keystore.
#[derive(Args)]
struct AmountArgs {
    /// Amount in atoms
    #[arg(long)]
    amount: u64,
    /// Keystore file path
    #[arg(long, short, default_value = "keystore.json")]
    keystore: PathBuf,
    /// Fee in atoms (the mempool rejects zero-fee transactions)
    #[arg(long, default_value_t = 1)]
    fee: u64,
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
    /// Keystore file path
    #[arg(long, short, default_value = "keystore.json")]
    keystore: PathBuf,
    /// Fee in atoms (the mempool rejects zero-fee transactions)
    #[arg(long, default_value_t = 1)]
    fee: u64,
}

/// Arguments for `kvnc claim-rewards`.
#[derive(Args)]
struct ClaimRewardsArgs {
    /// Validator address (32-byte hex, optional - claims all if omitted)
    #[arg(long)]
    validator: Option<String>,
    /// Keystore file path
    #[arg(long, short, default_value = "keystore.json")]
    keystore: PathBuf,
    /// Fee in atoms (the mempool rejects zero-fee transactions)
    #[arg(long, default_value_t = 1)]
    fee: u64,
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
        Commands::ImportKey(args) => cmd_import_key(args, json),
        Commands::Migrate(args) => cmd_migrate(args, json),
        Commands::Address(args) => cmd_address(args, json),
        Commands::Sign(args) => cmd_sign(args, json),
        Commands::Transfer(args) => cmd_transfer(&client, args, json).await,
        Commands::Status | Commands::Info => node::status(&client, json).await,
        Commands::Balance { address } => node::balance(&client, &address, json).await,
        Commands::Stake(args) => cmd_stake(&client, args, json).await,
        Commands::Unstake(args) => cmd_unstake(&client, args, json).await,
        Commands::Delegate(args) => cmd_delegate(&client, args, json).await,
        Commands::ClaimRewards(args) => cmd_claim_rewards(&client, args, json).await,
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
    if args.force {
        confirm_typed(&format!("OVERWRITE {}", args.out.display()))?;
    }
    let passphrase = prompt_confirmed_passphrase("New keystore passphrase: ")?;
    let keystore = wallet::keygen(&passphrase)?;
    if args.force {
        wallet::save_replace(&args.out, &keystore)?;
    } else {
        wallet::save(&args.out, &keystore)?;
    }

    if json_output {
        return output::print_json(&keystore_summary_json(&args.out, &keystore)?);
    }

    println!("Keystore written to {}", args.out.display());
    println!("Address:    {}", wallet::address(&keystore)?);
    println!("Public key: {}", keystore.public_key);
    Ok(())
}

fn cmd_import(args: KeystoreArgs, json_output: bool) -> Result<()> {
    let keystore = wallet::load(&args.keystore)?;
    let password = prompt_keystore_password(&keystore)?;
    // Inspection is not accepted as import until its seed-derived identity is verified.
    let _ = wallet::secret_seed(&keystore, password.as_ref().map(|p| p.as_str()))?;

    if json_output {
        return output::print_json(&keystore_summary_json(&args.keystore, &keystore)?);
    }

    let public_key = wallet::public_key(&keystore)?;
    println!("Keystore:   {}", args.keystore.display());
    println!("Address:    {}", wallet::address(&keystore)?);
    println!("Public key: {}", hex::encode(public_key.as_bytes()));
    println!("Encrypted:  {}", keystore.encrypted);
    Ok(())
}

fn cmd_export(args: ExportArgs, json_output: bool) -> Result<()> {
    if json_output {
        anyhow::bail!("secret export is disabled in JSON mode");
    }
    let keystore = wallet::load(&args.keystore.keystore)?;
    let password = prompt_keystore_password(&keystore)?;
    let seed = wallet::secret_seed(&keystore, password.as_ref().map(|p| p.as_str()))?;
    let label = match args.format {
        SecretFormat::Raw => "raw seed",
        SecretFormat::Mnemonic => "24-word English BIP-39 mnemonic",
    };
    if matches!(args.format, SecretFormat::Mnemonic) {
        eprintln!("This English BIP-39 phrase represents the exact seed entropy; no BIP-39 passphrase or HD derivation is used.");
    }
    let secret = export_after_confirmation(
        || confirm_typed(&format!("EXPORT {}", label.to_ascii_uppercase())),
        || printable_export(&seed, args.format),
    )?;
    writeln!(io::stdout().lock(), "{}", secret.as_str())?;
    Ok(())
}

fn printable_export(seed: &[u8; 32], format: SecretFormat) -> Result<Zeroizing<String>> {
    match format {
        SecretFormat::Raw => Ok(Zeroizing::new(hex::encode(seed))),
        SecretFormat::Mnemonic => Ok(Zeroizing::new(wallet::seed_to_mnemonic(seed)?)),
    }
}

fn keystore_summary_json(
    path: &std::path::Path,
    keystore: &wallet::Keystore,
) -> Result<serde_json::Value> {
    let public_key = wallet::public_key(keystore)?;
    Ok(json!({
        "keystore": path.display().to_string(),
        "address": wallet::address(keystore)?.to_string(),
        "publicKey": hex::encode(public_key.as_bytes()),
        "encrypted": keystore.encrypted,
    }))
}

fn cmd_sign(args: SignArgs, json_output: bool) -> Result<()> {
    let keystore = wallet::load(&args.keystore)?;
    let password = prompt_keystore_password(&keystore)?;

    let message = if args.hex {
        let raw = args.message.strip_prefix("0x").unwrap_or(&args.message);
        hex::decode(raw).context("--message is not valid hex")?
    } else {
        args.message.clone().into_bytes()
    };

    let signer = wallet::address(&keystore)?.to_string();
    let message_digest = hex::encode(kvnc_types::Hash::new(&message).0);
    eprintln!("{UNSAFE_SIGN_WARNING}");
    eprintln!("Signer: {signer}");
    eprintln!("Message digest: {message_digest}");
    confirm_typed(&format!("SIGN {signer} {message_digest}"))?;
    let signing_key = wallet::signing_key(&keystore, password.as_ref().map(|p| p.as_str()))?;

    let signature = kvnc_crypto::sign(&signing_key, &message);
    let signature_hex = hex::encode(signature.as_bytes());

    if json_output {
        return output::print_json(&json!({
            "address": wallet::address(&keystore)?.to_string(),
            "message": args.message,
            "messageHex": hex::encode(&message),
            "messageDigest": message_digest,
            "signature": signature_hex,
        }));
    }

    println!("{signature_hex}");
    Ok(())
}

async fn cmd_transfer(client: &RpcClient, args: TransferArgs, json_output: bool) -> Result<()> {
    let keystore = wallet::load(&args.keystore)?;
    let password = prompt_keystore_password(&keystore)?;
    let signing_key = wallet::signing_key(&keystore, password.as_ref().map(|p| p.as_str()))?;
    let sender = wallet::address(&keystore)?;
    let recipient = wallet::parse_address(&args.to, "to")?;

    // The account nonce is the next unused nonce, so it is used as-is.
    let nonce_value = client
        .call("kvnc_getNonce", json!([sender.to_string()]))
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
            "sender": sender.to_string(),
            "to": recipient.to_string(),
            "amount": args.amount,
            "fee": args.fee,
            "nonce": nonce,
        }));
    }

    println!("Submitted transaction {tx_hash}");
    println!("  from:   {}", sender);
    println!("  to:     {}", recipient);
    println!("  amount: {} atoms", args.amount);
    println!("  fee:    {} atoms", args.fee);
    println!("  nonce:  {nonce}");
    Ok(())
}

/// Sign a self-addressed transaction kind with the local keystore and submit it.
///
/// Mirrors [`cmd_transfer`]'s flow: nonce is read from `kvnc_getNonce`, the
/// signable hash is built offline, and only the bincode-encoded signed
/// transaction is sent over `kvnc_sendRawTransaction`. Private key material
/// never leaves the machine and no staking-specific node RPC is introduced.
async fn sign_and_submit_self(
    client: &RpcClient,
    keystore_path: &std::path::Path,
    kind: TransactionKind,
    fee: u64,
) -> Result<(String, kvnc_types::Address, u64)> {
    let keystore = wallet::load(keystore_path)?;
    let password = prompt_keystore_password(&keystore)?;
    let signing_key = wallet::signing_key(&keystore, password.as_ref().map(|p| p.as_str()))?;
    let sender = wallet::address(&keystore)?;

    // The account nonce is the next unused nonce, so it is used as-is.
    let nonce_value = client
        .call("kvnc_getNonce", json!([sender.to_string()]))
        .await?;
    let nonce = rpc::parse_quantity(&nonce_value)?;

    let transaction = tx::build_signed(sender, nonce, kind, fee, &signing_key)?;
    let raw = tx::encode_raw(&transaction)?;

    let result = client.call("kvnc_sendRawTransaction", json!([raw])).await?;
    let tx_hash = result.as_str().unwrap_or_default().to_string();
    Ok((tx_hash, sender, nonce))
}

async fn cmd_stake(client: &RpcClient, args: AmountArgs, json_output: bool) -> Result<()> {
    let (tx_hash, sender, nonce) = sign_and_submit_self(
        client,
        &args.keystore,
        TransactionKind::Stake {
            amount: args.amount,
        },
        args.fee,
    )
    .await?;

    if json_output {
        return output::print_json(&json!({
            "transactionHash": tx_hash,
            "sender": sender.to_string(),
            "amount": args.amount,
            "fee": args.fee,
            "nonce": nonce,
        }));
    }

    println!("Submitted transaction {tx_hash}");
    println!("  from:   {}", sender);
    println!("  amount: {} atoms", args.amount);
    println!("  fee:    {} atoms", args.fee);
    println!("  nonce:  {nonce}");
    Ok(())
}

async fn cmd_unstake(client: &RpcClient, args: AmountArgs, json_output: bool) -> Result<()> {
    let (tx_hash, sender, nonce) = sign_and_submit_self(
        client,
        &args.keystore,
        TransactionKind::Unstake {
            amount: args.amount,
        },
        args.fee,
    )
    .await?;

    if json_output {
        return output::print_json(&json!({
            "transactionHash": tx_hash,
            "sender": sender.to_string(),
            "amount": args.amount,
            "fee": args.fee,
            "nonce": nonce,
        }));
    }

    println!("Submitted transaction {tx_hash}");
    println!("  from:   {}", sender);
    println!("  amount: {} atoms", args.amount);
    println!("  fee:    {} atoms", args.fee);
    println!("  nonce:  {nonce}");
    Ok(())
}

async fn cmd_delegate(client: &RpcClient, args: DelegateArgs, json_output: bool) -> Result<()> {
    let validator = args.validator.parse::<kvnc_types::Address>()?;
    let (tx_hash, sender, nonce) = sign_and_submit_self(
        client,
        &args.keystore,
        TransactionKind::Delegate {
            validator,
            amount: args.amount,
        },
        args.fee,
    )
    .await?;

    if json_output {
        return output::print_json(&json!({
            "transactionHash": tx_hash,
            "sender": sender.to_string(),
            "validator": validator.to_string(),
            "amount": args.amount,
            "fee": args.fee,
            "nonce": nonce,
        }));
    }

    println!("Submitted transaction {tx_hash}");
    println!("  from:      {}", sender);
    println!("  validator: {}", validator);
    println!("  amount:    {} atoms", args.amount);
    println!("  fee:       {} atoms", args.fee);
    println!("  nonce:     {nonce}");
    Ok(())
}

async fn cmd_claim_rewards(
    client: &RpcClient,
    args: ClaimRewardsArgs,
    json_output: bool,
) -> Result<()> {
    let validator = args
        .validator
        .map(|v| v.parse::<kvnc_types::Address>())
        .transpose()?;
    let (tx_hash, sender, nonce) = sign_and_submit_self(
        client,
        &args.keystore,
        TransactionKind::ClaimRewards { validator },
        args.fee,
    )
    .await?;

    if json_output {
        return output::print_json(&json!({
            "transactionHash": tx_hash,
            "sender": sender.to_string(),
            "validator": validator.map(|v| v.to_string()),
            "fee": args.fee,
            "nonce": nonce,
        }));
    }

    println!("Submitted transaction {tx_hash}");
    println!("  from:      {}", sender);
    if let Some(v) = validator {
        println!("  validator: {}", v);
    } else {
        println!("  validator: all delegated validators");
    }
    println!("  fee:       {} atoms", args.fee);
    println!("  nonce:     {nonce}");
    Ok(())
}

fn cmd_import_key(args: ImportKeyArgs, json_output: bool) -> Result<()> {
    if json_output {
        anyhow::bail!("secret import is disabled in JSON mode");
    }
    let input = Zeroizing::new(rpassword::prompt_password(match args.format {
        SecretFormat::Raw => "Raw 32-byte seed hex (hidden): ",
        SecretFormat::Mnemonic => "24 English BIP-39 words (hidden): ",
    })?);
    let seed = match args.format {
        SecretFormat::Raw => wallet::seed_from_raw_hex(&input)?,
        SecretFormat::Mnemonic => wallet::seed_from_mnemonic(&input)?,
    };
    if matches!(args.format, SecretFormat::Mnemonic) {
        eprintln!("Mnemonic import uses its exact entropy as the Ed25519 seed; no BIP-39 passphrase or HD derivation is used.");
    }
    if args.force {
        confirm_typed(&format!("OVERWRITE {}", args.out.display()))?;
    }
    let passphrase = prompt_confirmed_passphrase("New keystore passphrase: ")?;
    let keystore = wallet::from_seed(&seed, &passphrase)?;
    if args.force {
        wallet::save_replace(&args.out, &keystore)?;
    } else {
        wallet::save(&args.out, &keystore)?;
    }
    println!("Encrypted keystore written to {}", args.out.display());
    println!("Address: {}", wallet::address(&keystore)?);
    Ok(())
}

fn cmd_migrate(args: MigrateArgs, json_output: bool) -> Result<()> {
    if json_output {
        anyhow::bail!("migration is interactive and disabled in JSON mode");
    }
    let old = wallet::load_v1_for_migration(&args.keystore)?;
    let old_password = prompt_password_if_encrypted(old.is_encrypted())?;
    let seed = wallet::migration_seed(&old, old_password.as_ref().map(|p| p.as_str()))?;
    let destination = args.out.unwrap_or_else(|| {
        let mut name = args.keystore.as_os_str().to_os_string();
        name.push(".v2.json");
        PathBuf::from(name)
    });
    if destination == args.keystore {
        anyhow::bail!("migration destination must differ from source");
    }
    let new_password = prompt_confirmed_passphrase("New v2 keystore passphrase: ")?;
    let migrated = wallet::from_seed(&seed, &new_password)?;
    wallet::save(&destination, &migrated)?;
    let reloaded = wallet::load(&destination)?;
    let verified = wallet::secret_seed(&reloaded, Some(&new_password))?;
    if *verified != *seed {
        anyhow::bail!("migrated keystore verification failed");
    }
    if args.replace_source {
        confirm_typed(&format!("REPLACE {}", args.keystore.display()))?;
        wallet::replace_file(&args.keystore, &destination)?;
        let final_store = wallet::load(&args.keystore)?;
        let _ = wallet::secret_seed(&final_store, Some(&new_password))?;
    }
    println!(
        "Verified v2 keystore written to {}",
        if args.replace_source {
            args.keystore.display()
        } else {
            destination.display()
        }
    );
    Ok(())
}

/// `kvnc address --key-file <FILE>` — offline derivation of the canonical
/// address and public key from a raw 32-byte hex seed. Prints only public
/// identity material; the seed is never echoed.
///
/// Security: requires `--allow-raw-hex` flag and key file must have 0600 permissions.
fn cmd_address(args: AddressArgs, json_output: bool) -> Result<()> {
    if !args.allow_raw_hex {
        anyhow::bail!(
            "raw hex seed import is disabled by default for security. \
            Use --allow-raw-hex to enable (ensure key file has 0600 permissions and is not shared)."
        );
    }

    // Enforce key file permissions (0600) to prevent accidental exposure
    let metadata = std::fs::metadata(&args.key_file)
        .with_context(|| format!("reading key file metadata {}", args.key_file.display()))?;
    let permissions = metadata.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if permissions.mode() & 0o077 != 0 {
            anyhow::bail!(
                "key file {} has insecure permissions (mode: {:o}). \
                Must be 0600 (readable/writable only by owner). \
                Run: chmod 600 {}",
                args.key_file.display(),
                permissions.mode() & 0o777,
                args.key_file.display()
            );
        }
    }

    let raw = std::fs::read_to_string(&args.key_file)
        .with_context(|| format!("reading key file {}", args.key_file.display()))?;
    let seed = wallet::seed_from_raw_hex(&raw)?;
    let signing_key = SigningKey::from_bytes(&seed);
    let public_key = PublicKey::from(signing_key.verifying_key());
    let address = Address::from_public_key(&public_key);
    let public_key_hex = hex::encode(public_key.as_bytes());

    if json_output {
        return output::print_json(&json!({
            "address": address.to_string(),
            "publicKey": public_key_hex,
        }));
    }

    println!("Address:    {address}");
    println!("Public key: {public_key_hex}");
    Ok(())
}

fn prompt_confirmed_passphrase(prompt: &str) -> Result<Zeroizing<String>> {
    let first = Zeroizing::new(rpassword::prompt_password(prompt)?);
    let second = Zeroizing::new(rpassword::prompt_password("Confirm passphrase: ")?);
    if first.is_empty() {
        anyhow::bail!("passphrase must not be empty");
    }
    if *first != *second {
        anyhow::bail!("passphrases do not match");
    }
    Ok(first)
}

fn prompt_keystore_password(keystore: &wallet::Keystore) -> Result<Option<Zeroizing<String>>> {
    prompt_password_if_encrypted(keystore.encrypted)
}

fn prompt_password_if_encrypted(encrypted: bool) -> Result<Option<Zeroizing<String>>> {
    if !encrypted {
        return Ok(None);
    }
    Ok(Some(Zeroizing::new(rpassword::prompt_password(
        "Keystore passphrase: ",
    )?)))
}

fn confirm_typed(expected: &str) -> Result<()> {
    eprintln!("DANGER: this action exposes or replaces private wallet material.");
    eprintln!("Type exactly: {expected}");
    let mut response = String::new();
    io::stdin().read_line(&mut response)?;
    if !typed_confirmation_matches(expected, &response) {
        anyhow::bail!("confirmation did not match");
    }
    Ok(())
}

fn typed_confirmation_matches(expected: &str, response: &str) -> bool {
    response.trim_end() == expected
}

fn export_after_confirmation<C, F>(confirm: C, construct: F) -> Result<Zeroizing<String>>
where
    C: FnOnce() -> Result<()>,
    F: FnOnce() -> Result<Zeroizing<String>>,
{
    confirm()?;
    construct()
}

#[cfg(test)]
mod wallet_cli_security_tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn secret_passphrases_are_not_cli_arguments() {
        let recipient = "00".repeat(32);
        for args in [
            vec!["kvnc", "keygen", "--passphrase", "secret"],
            vec!["kvnc", "sign", "--message", "hi", "--passphrase", "secret"],
            vec![
                "kvnc",
                "transfer",
                "--to",
                &recipient,
                "--amount",
                "1",
                "--passphrase",
                "secret",
            ],
            vec![
                "kvnc",
                "export",
                "--format",
                "raw",
                "--passphrase",
                "secret",
            ],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }

    #[test]
    fn secret_export_is_rejected_in_json_mode() {
        let args = ExportArgs {
            keystore: KeystoreArgs {
                keystore: PathBuf::from("does-not-need-to-exist"),
            },
            format: SecretFormat::Raw,
        };
        assert!(cmd_export(args, true).is_err());
    }

    #[test]
    fn address_command_derives_and_rejects_bad_seed() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("validator.pem");
        std::fs::write(&key_path, hex::encode([0x2a; 32])).unwrap();
        // Set secure permissions (0600)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(cmd_address(
            AddressArgs {
                key_file: key_path.clone(),
                allow_raw_hex: true,
            },
            false
        )
        .is_ok());
        std::fs::write(&key_path, "00").unwrap();
        assert!(cmd_address(
            AddressArgs {
                key_file: key_path,
                allow_raw_hex: true
            },
            false
        )
        .is_err());
    }

    #[test]
    fn routine_keystore_json_contains_metadata_only() {
        let keystore = wallet::from_seed(&[8; 32], "test passphrase").unwrap();
        let value = keystore_summary_json(std::path::Path::new("wallet.json"), &keystore).unwrap();
        assert!(value.get("address").is_some());
        assert!(value.get("publicKey").is_some());
        let address = value["address"].as_str().unwrap();
        assert!(address.starts_with("kvnc") && address.ends_with("dag"));
        for secret_field in ["secret_key", "secretKey", "privateKey", "seed", "mnemonic"] {
            assert!(value.get(secret_field).is_none());
        }
    }

    #[test]
    fn typed_sign_confirmation_binds_signer_and_message_digest() {
        assert!(UNSAFE_SIGN_WARNING.contains("unsafe and cross-context"));
        assert!(UNSAFE_SIGN_WARNING.contains("replayed or misinterpreted"));
        let address = "11".repeat(32);
        let digest = "ab".repeat(32);
        let required = format!("SIGN {address} {digest}");
        assert!(typed_confirmation_matches(&required, &required));
        assert!(!typed_confirmation_matches(
            &required,
            &format!("SIGN {} {digest}", "22".repeat(32))
        ));
        assert!(!typed_confirmation_matches(
            &required,
            &format!("SIGN {address} {}", "cd".repeat(32))
        ));
        assert!(!typed_confirmation_matches(
            &required,
            &format!("SIGN {address} {digest} extra")
        ));
    }

    #[test]
    fn cancelled_export_never_constructs_printable_secret() {
        let seed = [0x5a; 32];
        let mut constructed = false;
        let result = export_after_confirmation(
            || anyhow::bail!("confirmation did not match"),
            || {
                constructed = true;
                printable_export(&seed, SecretFormat::Raw)
            },
        );
        assert!(result.is_err());
        assert!(!constructed);
    }

    #[test]
    fn stake_and_unstake_accept_keystore_and_fee() {
        let cli = Cli::try_parse_from([
            "kvnc",
            "stake",
            "--amount",
            "42",
            "--fee",
            "3",
            "--keystore",
            "wallet.json",
        ])
        .unwrap();
        match cli.command {
            Commands::Stake(args) => {
                assert_eq!(args.amount, 42);
                assert_eq!(args.fee, 3);
                assert_eq!(args.keystore, PathBuf::from("wallet.json"));
            }
            _ => panic!("expected stake command"),
        }

        let cli = Cli::try_parse_from(["kvnc", "unstake", "--amount", "7"]).unwrap();
        match cli.command {
            Commands::Unstake(args) => {
                assert_eq!(args.amount, 7);
                assert_eq!(args.fee, 1);
                assert_eq!(args.keystore, PathBuf::from("keystore.json"));
            }
            _ => panic!("expected unstake command"),
        }
    }
}
