//! KVNC full node binary.
//!
//! Wires the protocol crates together into a runnable node:
//!
//! 1. load configuration (TOML + environment overrides),
//! 2. open the consensus and state databases, initialise genesis if fresh,
//! 3. start the P2P network, the JSON-RPC server and the consensus engine,
//! 4. run the event-ingest, block-builder and execution loops,
//! 5. shut down cleanly on SIGINT / SIGTERM.
//!
//! Several integration points are deliberately marked `TODO` where the
//! underlying crate does not yet expose a hook (see the comments in
//! [`run_node`]).

// `DagStoreTrait`/`BlockManagerTrait` return the storage-adjacent error enums
// from `kvnc-dag`, whose redb-backed variants exceed clippy's error size limit.
#![allow(clippy::result_large_err)]
// `NetworkCommand` carries block/transaction payloads next to a zero-sized
// `Shutdown` variant.
#![allow(clippy::large_enum_variant)]

mod config;

use std::future::Future;
use std::sync::{atomic::Ordering, Arc};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use kvnc_network::{Multiaddr, PeerId};
use parking_lot::RwLock;
use serde::Deserialize;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, warn};

use config::NodeConfig;

use kvnc_consensus::engine::{BlockManagerTrait, DagStoreTrait};
use kvnc_consensus::metrics::{record_mempool_size, record_peer_count};
use kvnc_consensus::{AuthorityInfo, CommitteeInfo, ConsensusConfig, ConsensusEngine, Vote};
use kvnc_dag::{BlockManager, BlockManagerError, DagStore, DagStoreError};
use kvnc_execution::ExecutionContext;
use kvnc_mempool::{Mempool, MempoolConfig, MempoolError};
use kvnc_network::{NetworkConfig, NetworkEvent, NetworkService, BlockSyncRequest, BlockSyncResponse, StateSyncResponse};
use kvnc_rpc::{EventBus, RpcServer, RpcState, AuthConfig, RateLimitConfig, RateLimiterState};
use kvnc_execution::{LogPublisher, TransactionReceipt};

/// Wrapper around EventBus to implement LogPublisher.
struct EventLogPublisher(EventBus);

impl LogPublisher for EventLogPublisher {
    fn publish_logs(&self, receipts: &[TransactionReceipt]) {
        self.0.publish_logs(receipts);
    }
}
use kvnc_staking::{
    StakingState, FOUNDER_PREMINE, MAX_ACTIVE_VALIDATORS, MIN_VALIDATOR_STAKE, ONE_KVNC,
};
use kvnc_storage::{StateStoreError, Storage};
use kvnc_types::{
    block::{BlockReference, StatementBlock},
    Address, AuthorityIndex, Hash, PublicKey, Round, Signature, SigningKey, Transaction,
};

/// Command line arguments.
#[derive(Parser, Debug)]
#[command(name = "kvnc-node")]
#[command(about = "Kovanica (KVNC) full node", long_about = None)]
#[command(subcommand_required = false, arg_required_else_help = false)]
struct Args {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to config file (used when running node)
    #[arg(short, long, default_value = "config.toml")]
    config: String,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Genesis ceremony: build genesis block + staking state from validator keys
    Genesis(GenesisArgs),
}

#[derive(clap::Args, Debug)]
struct GenesisArgs {
    /// Path to validators JSON file (array: {address, public_key?, stake?})
    #[arg(long, value_name = "FILE", default_value = "validators.json")]
    validators: String,

    /// Treasury address (32-byte hex)
    #[arg(long, value_name = "HEX")]
    treasury_address: String,

    /// Founder premine address (32-byte hex, receives 200,000 KVNC)
    #[arg(long, value_name = "HEX")]
    founder_address: String,

    /// Output genesis JSON file
    #[arg(long, value_name = "FILE", default_value = "genesis.json")]
    output: String,

    /// Output validator key files directory (optional)
    #[arg(long, value_name = "DIR")]
    validator_keys_out: Option<String>,

    /// Overwrite existing output files
    #[arg(long)]
    force: bool,
}

/// Outbound network commands handled by the network task.
enum NetworkCommand {
    /// Gossip a newly produced block.
    BroadcastBlock(StatementBlock),
    /// Gossip a newly accepted transaction.
    BroadcastTransaction(Transaction),
    /// Gossip a consensus vote.
    BroadcastVote(Vote),
    /// Request a block sync from a specific peer.
    RequestSync(PeerId, BlockSyncRequest),
    /// Ask the network task to stop.
    Shutdown,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let args = Args::parse();
    match args.command {
        Some(Commands::Genesis(g)) => {
            return run_genesis(g);
        }
        None => {
            let config = NodeConfig::load(&args.config)?;
            info!(data_dir = %config.data_dir.display(), "starting KVNC node");
            run_node(config, shutdown_signal()).await
        }
    }
}

/// Run the genesis ceremony: read validators.json, build staking state
/// and write genesis.json (genesis block + staking state).
fn run_genesis(args: GenesisArgs) -> Result<()> {
    use std::fs;

    // Read validators JSON
    let text = fs::read_to_string(&args.validators)
        .with_context(|| format!("reading validators {}", args.validators))?;
    let validators_input: Vec<GenesisValidatorInput> =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", args.validators))?;

    // Build treasury
    let treasury = parse_address_hex(Some(&args.treasury_address))?;
    let founder = parse_address_hex(Some(&args.founder_address))?;
    let mut staking = StakingState::new();
    staking.init_treasury(treasury);

    // Founder premine (200,000 KVNC) - included in genesis allocations output
    // Actual balance set when genesis block is executed on first run

    // Join each validator
    for v in validators_input {
        let address = parse_address_hex(Some(&v.address))?;
        let pk = v
            .public_key
            .as_ref()
            .map(|s| parse_public_key_hex(s))
            .transpose()?;
        let stake = v.stake.unwrap_or(MIN_VALIDATOR_STAKE);
        staking
            .join_validator(address, stake, 0, Some(address), pk)
            .with_context(|| format!("joining validator {}", v.address))?;
    }

    // Generate validator keys if output directory specified
    let mut validator_keys = Vec::new();
    if let Some(keys_dir) = &args.validator_keys_out {
        fs::create_dir_all(keys_dir)?;
        for (i, v) in staking.validators.iter().enumerate() {
            let (sk, pk) = kvnc_crypto::generate_keypair();
            let seed = sk.to_bytes();
            let key_file = format!("{}/validator{}.key", keys_dir, i + 1);
            fs::write(&key_file, hex::encode(seed))?;
            validator_keys.push(serde_json::json!({
                "index": i,
                "address_hex": hex::encode(v.address.0),
                "public_key_hex": hex::encode(pk.0),
                "key_file": key_file,
            }));
            info!("Generated validator {} key: {}", i + 1, key_file);
        }
    }

    info!(validators = staking.validators.len(), treasury = %treasury, founder = %founder, "genesis allocations computed");

    // Build genesis StatementBlock
    let digest = StatementBlock::compute_digest(0, 0, &[], &[]);
    let genesis_block = StatementBlock {
        author: 0,
        round: 0,
        parents: Vec::new(),
        transactions: Vec::new(),
        statements: Vec::new(),
        signature: Signature([0u8; 64]),
        digest,
        merkle_root: Hash::zero(),
    };

    // Serialize output JSON
    let output = serde_json::json!({
        "genesis_block": {
            "author": genesis_block.author,
            "round": genesis_block.round,
            "digest_hex": hex::encode(genesis_block.digest.0),
            "signature_hex": hex::encode(genesis_block.signature.0),
            "transactions": genesis_block.transactions,
            "statements": genesis_block.statements,
            "parents": genesis_block.parents,
        },
        "staking_state": staking,
        "allocations": {
            "treasury_address_hex": args.treasury_address,
            "treasury_address_bytes": hex::encode(treasury.0),
            "founder_address_hex": args.founder_address,
            "founder_address_bytes": hex::encode(founder.0),
            "founder_premine_atoms": FOUNDER_PREMINE,
            "founder_premine_kvnc": FOUNDER_PREMINE / ONE_KVNC,
            "validators": staking.validators.iter().map(|v| serde_json::json!({
                "address_hex": hex::encode(v.address.0),
                "stake": v.stake,
                "public_key_hex": v.public_key.as_ref().map(|pk| hex::encode(pk.0)),
            })).collect::<Vec<_>>(),
        },
        "validator_keys": validator_keys,
    });

    // Write output with optional force
    if args.force {
        fs::write(&args.output, serde_json::to_string_pretty(&output)?)
            .with_context(|| format!("writing genesis {}", args.output))?;
    } else if fs::metadata(&args.output).is_ok() {
        anyhow::bail!(
            "output file {} exists; use --force to overwrite",
            args.output
        );
    } else {
        fs::write(&args.output, serde_json::to_string_pretty(&output)?)
            .with_context(|| format!("writing genesis {}", args.output))?;
    }
    info!(file = %args.output, validators = staking.validators.len(), "genesis file written");
    Ok(())
}

/// Validator entry read from validators.json
#[derive(Debug, Deserialize)]
struct GenesisValidatorInput {
    address: String,
    public_key: Option<String>,
    stake: Option<u64>,
}

/// Install the tracing subscriber, honouring `RUST_LOG` when set.
fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let json_layer = fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(true)
        .with_target(true)
        .with_thread_ids(true)
        .with_thread_names(true);

    tracing_subscriber::registry()
        .with(filter)
        .with(json_layer)
        .init();
}

/// Round tracing utilities for structured logging with trace_id per round.
pub mod round_trace {
    use std::sync::atomic::{AtomicU64, Ordering};
    use tracing::{span, Level};

    /// Global counter for generating unique trace IDs.
    static TRACE_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Generate a new trace ID for a round.
    pub fn new_trace_id(round: u64) -> String {
        let counter = TRACE_COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("round-{:010}-{:06}", round, counter)
    }

    /// Create a tracing span for a round with trace_id.
    pub fn round_span(round: u64) -> tracing::Span {
        let trace_id = new_trace_id(round);
        span!(Level::INFO, "round", round = round, trace_id = %trace_id)
    }

    /// Create a tracing span for block processing with trace_id.
    pub fn block_span(round: u64, block_hash: &str, author: &str) -> tracing::Span {
        let trace_id = new_trace_id(round);
        span!(
            Level::INFO,
            "block",
            round = round,
            trace_id = %trace_id,
            block_hash = %block_hash,
            author = %author
        )
    }

    /// Create a tracing span for vote processing with trace_id.
    pub fn vote_span(round: u64, leader_round: u64, voter: &str) -> tracing::Span {
        let trace_id = new_trace_id(round);
        span!(
            Level::INFO,
            "vote",
            round = round,
            trace_id = %trace_id,
            leader_round = leader_round,
            voter = %voter
        )
    }

    /// Create a tracing span for transaction processing with trace_id.
    pub fn tx_span(round: u64, tx_hash: &str) -> tracing::Span {
        let trace_id = new_trace_id(round);
        span!(
            Level::INFO,
            "transaction",
            round = round,
            trace_id = %trace_id,
            tx_hash = %tx_hash
        )
    }

    /// Create a tracing span for execution with trace_id.
    pub fn execution_span(round: u64, tx_count: usize) -> tracing::Span {
        let trace_id = new_trace_id(round);
        span!(
            Level::INFO,
            "execution",
            round = round,
            trace_id = %trace_id,
            tx_count = tx_count
        )
    }
}

/// A future that resolves when the process receives SIGINT or SIGTERM.
///
/// Signal handlers are installed eagerly when this function is called (not when
/// the returned future is first polled), so it is safe to register them before
/// any other startup work.
fn shutdown_signal() -> impl Future<Output = ()> + Send + 'static {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");
        let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");

        async move {
            tokio::select! {
                _ = sigint.recv() => info!("received SIGINT"),
                _ = sigterm.recv() => info!("received SIGTERM"),
            }
        }
    }

    #[cfg(not(unix))]
    {
        async move {
            let _ = tokio::signal::ctrl_c().await;
            info!("received Ctrl-C");
        }
    }
}

/// Boot the node and run until `shutdown` resolves, then stop it cleanly.
async fn run_node<F>(config: NodeConfig, shutdown: F) -> Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    // ------------------------------------------------------------------
    // 1. Data directory + storage
    // ------------------------------------------------------------------
    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("creating data directory {}", config.data_dir.display()))?;

    // The DAG/consensus store and the account/staking store live in separate
    // redb files: `kvnc-dag::DagStore` owns its `Storage`, while the mempool and
    // execution layer need a shareable `Arc<Storage>`. redb locks a file to a
    // single `Database`, so they cannot share one file.
    let dag_store = Arc::new(DagStore::new(Storage::new(config.consensus_db_path())?)?);
    let state_storage = Arc::new(Storage::new(config.state_db_path())?);
    info!(dir = %config.data_dir.display(), "opened storage");

    // ------------------------------------------------------------------
    // 2. Genesis / staking state
    // ------------------------------------------------------------------
    init_genesis(&config, &dag_store, &state_storage)?;

    // ------------------------------------------------------------------
    // 3. Mempool + validator identity
    // ------------------------------------------------------------------
    let mempool = Arc::new(Mempool::new(
        MempoolConfig::default(),
        state_storage.clone(),
    ));
    let (signing_key, public_key) = load_validator_key(config.validator_key.as_deref());
    let validator_address = Address::from_public_key(&public_key);
    info!(validator = %validator_address, "validator identity ready");

    // Build the committee from active validators in StakingState.
    let committee = build_committee(
        &state_storage,
        &public_key,
        &validator_address,
        &config.listen_addr,
    )?;

    // Register committee public keys with the crypto batch verifier (used by the
    // hot path on network ingest). Without this every remote block is rejected
    // with `InvalidPublicKey` (the static key list stays empty).
    let committee_keys: Vec<PublicKey> = committee
        .authorities()
        .iter()
        .map(|a| a.public_key)
        .collect();
    kvnc_crypto::set_validator_keys_from_public(committee_keys);
    info!(
        committee = committee.authorities().len(),
        "committee public keys registered with batch verifier"
    );

    // Determine our authority index from the committee (match local public key)
    let our_authority = if config.run_validator {
        committee
            .authorities()
            .iter()
            .find(|a| a.public_key == public_key)
            .map(|a| a.index)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "local validator public key not found in committee; cannot run as validator"
                )
            })?
    } else {
        info!("run_validator=false; not participating in consensus");
        u16::MAX // Invalid authority index
    };

    // Load the staking state for the RPC server (shared with consensus via Arc<RwLock>).
    let staking_state = {
        let read = state_storage.begin_read()?;
        match state_storage.state().load_staking_state(&read) {
            Ok(state) => state,
            Err(e) => {
                warn!(error = %e, "could not load staking state for RPC; using empty state");
                StakingState::new()
            }
        }
    };

    // ------------------------------------------------------------------
    // 4. Networking
    // ------------------------------------------------------------------
    let network_config = build_network_config(&config)?;
    let (network, network_events) =
        NetworkService::new(network_config, dag_store.clone(), mempool.clone())
            .context("creating network service")?;
    // Share one service between the swarm event loop and the broadcast command
    // loop so broadcast commands never re-enter `start` or rebuild listeners.
    let network = Arc::new(network);
    let peer_count = network.peer_count_handle();
    let peer_count_for_rpc = peer_count.clone();
    let (network_cmd_tx, network_cmd_rx) = mpsc::unbounded_channel::<NetworkCommand>();

    // ------------------------------------------------------------------
    // 5. JSON-RPC server
    // ------------------------------------------------------------------
    // NOTE: the node keeps the DAG/consensus store (`dag_store`) and the
    // account/staking store (`state_storage`) in separate redb files, so the
    // read-only storage handed to the RPC server is `state_storage`. Block and
    // consensus queries therefore observe the state database; wiring them to the
    // DAG database requires exposing `DagStore`'s inner `Arc<Storage>`.
    let rpc_socket = config.rpc_socket_addr()?;
    // Fan-out bus for WebSocket subscription events (newHeads,
    // newCommittedLeader, pendingTransactions).
    let events = EventBus::new();
    let rate_limit_config = RateLimitConfig::from_requests_per_minute(config.rpc_rate_limit_per_min);
    let rpc_state = RpcState {
        storage: state_storage.clone(),
        consensus_store: dag_store.clone(),
        mempool: mempool.clone(),
        staking: Arc::new(tokio::sync::RwLock::new(staking_state)),
        committee: committee.clone(),
        peer_count: peer_count_for_rpc,
        events: events.clone(),
        rate_limiter: Arc::new(RateLimiterState::new(rate_limit_config.clone())),
        auth_config: Arc::new(if std::env::var("KVNC_RPC_AUTH").as_deref() == Ok("disable") {
            AuthConfig {
                write_tokens: vec!["test".to_string()],
                require_auth_for_writes: false,
            }
        } else {
            AuthConfig::default()
        }),
    };
    let rpc_server = RpcServer::new(
        rpc_socket,
        rpc_state,
        Some(rate_limit_config),
        Some(if std::env::var("KVNC_RPC_AUTH").as_deref() == Ok("disable") {
            AuthConfig {
                write_tokens: vec!["test".to_string()],
                require_auth_for_writes: false,
            }
        } else {
            AuthConfig::default()
        }),
    ).await;
    let rpc_handle = rpc_server.start().await.context("starting RPC server")?;

    // ------------------------------------------------------------------
    // 6. Consensus engine
    // ------------------------------------------------------------------
    // The node-provided validator identity remains authoritative in the block
    // manager; consensus does not generate or replace a signing key.
    let block_manager = Arc::new(BlockManager::new(dag_store.clone()));
    if config.run_validator {
        block_manager.set_authority(our_authority);
        block_manager.set_signing_key(signing_key.clone());
    } else {
        // Set an invalid authority index so we never think we're the leader
        block_manager.set_authority(u16::MAX);
    }
    block_manager.set_authority_keys(
        committee
            .authorities()
            .iter()
            .map(|authority| (authority.index, authority.public_key))
            .collect(),
    );
    block_manager.set_authority_stakes(
        committee
            .authorities()
            .iter()
            .map(|authority| (authority.index, authority.stake))
            .collect(),
    );

    // Populate the global validator key registry for batch signature verification
    // The registry is indexed by authority index, so we need to ensure keys are in index order
    let mut validator_keys = vec![None; committee.size() as usize];
    for authority in committee.authorities() {
        validator_keys[authority.index as usize] = Some(authority.public_key);
    }
    let public_keys: Vec<PublicKey> = validator_keys
        .into_iter()
        .map(|opt| opt.expect("validator key missing for authority index"))
        .collect();
    kvnc_crypto::set_validator_keys_from_public(public_keys);
    let engine = Arc::new(ConsensusEngine::new(
        ConsensusConfig {
            round_duration_ms: config.round_duration_ms,
            use_mysticghost: config.use_mysticghost,
            ..Default::default()
        },
        committee.clone(),
        Arc::new(NodeDagStore {
            inner: dag_store.clone(),
        }),
        Arc::new(RwLock::new(NodeBlockManager {
            inner: block_manager.clone(),
        })),
        signing_key.clone(),
        if config.run_validator {
            Some(mempool.clone())
        } else {
            None
        },
    ));

    // Set the round watch receiver on the block manager so it can track the current round
    // from the consensus engine's watch channel (Phase 16.2).
    let round_rx = engine.subscribe_round();
    block_manager.set_round_receiver(round_rx);

    // Set up block broadcaster to gossip proposed blocks
    let broadcast_tx = network_cmd_tx.clone();
    engine.set_block_broadcaster(Arc::new(move |block: &StatementBlock| {
        let _ = broadcast_tx.send(NetworkCommand::BroadcastBlock(block.clone()));
    }));

    // Set up vote broadcaster to gossip votes
    let vote_broadcast_tx = network_cmd_tx.clone();
    engine.set_vote_broadcaster(Arc::new(move |vote: &Vote| {
        let _ = vote_broadcast_tx.send(NetworkCommand::BroadcastVote(vote.clone()));
    }));

    // ------------------------------------------------------------------
    // 7. Shutdown plumbing + task spawning
    // ------------------------------------------------------------------
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (exec_tx, exec_rx) = mpsc::unbounded_channel::<kvnc_consensus::CommittedSubDag>();
    engine.set_commit_sender(exec_tx.clone());
    for subdag in recover_committed_subdags(&dag_store, config.use_mysticghost)? {
        exec_tx
            .send(subdag)
            .context("queueing a previously committed sub-DAG for replay")?;
    }

    let network_task = tokio::spawn(run_network(network, network_cmd_rx, shutdown_rx.clone()));
    let event_task = tokio::spawn(run_event_handler(
        network_events,
        mempool.clone(),
        engine.clone(),
        dag_store.clone(),
        network_cmd_tx.clone(),
        events.clone(),
        shutdown_rx.clone(),
    ));

    // The consensus engine now handles block production internally with mempool transactions
    // No separate builder task needed
    let _builder_task = tokio::spawn(async {});
    let exec_task = tokio::spawn(run_execution(
        exec_rx,
        state_storage.clone(),
        events.clone(),
        shutdown_rx.clone(),
    ));

    let engine_for_task = engine.clone();
    let engine_task = tokio::spawn(async move {
        if let Err(e) = engine_for_task.start().await {
            error!(error = %e, "consensus engine stopped with an error");
        }
    });

    // Background task: periodically update Prometheus metrics from node state
    let mempool_for_metrics = mempool.clone();
    let peer_count_for_metrics = peer_count.clone();
    let mut shutdown_for_metrics = shutdown_rx.clone();
    let _metrics_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    // Update mempool size
                    let mempool_size = mempool_for_metrics.len() as i64;
                    record_mempool_size(mempool_size);

                    // Update peer count
                    let peer_count_val = peer_count_for_metrics.load(Ordering::Relaxed) as i64;
                    record_peer_count(peer_count_val);
                }
                _ = shutdown_for_metrics.changed() => {
                    if *shutdown_for_metrics.borrow() {
                        break;
                    }
                }
            }
        }
    });

    info!("KVNC node started");

    // ------------------------------------------------------------------
    // 8. Run until shutdown, then stop everything
    // ------------------------------------------------------------------
    shutdown.await;
    info!("shutdown signal received; stopping node");

    let _ = shutdown_tx.send(true);
    engine.stop();
    let _ = network_cmd_tx.send(NetworkCommand::Shutdown);
    drop(network_cmd_tx);
    drop(exec_tx);
    rpc_handle.abort();

    for (name, handle) in [
        ("network", network_task),
        ("events", event_task),
        ("execution", exec_task),
        ("consensus", engine_task),
    ] {
        match tokio::time::timeout(Duration::from_secs(3), handle).await {
            Ok(Ok(())) => debug!(task = name, "task stopped"),
            Ok(Err(e)) => warn!(task = name, error = %e, "task panicked"),
            Err(_) => warn!(task = name, "task did not stop before timeout"),
        }
    }

    // redb commits each write transaction synchronously, so there is nothing
    // left to flush: dropping the handles closes the databases.
    info!("KVNC node stopped cleanly");
    Ok(())
}

/// Initialise genesis state if the database is fresh.
fn init_genesis(config: &NodeConfig, dag_store: &DagStore, state_storage: &Storage) -> Result<()> {
    let existing = {
        let read = state_storage.begin_read()?;
        state_storage.state().load_staking_state(&read)
    };

    match existing {
        Ok(_) => {
            info!("existing chain state detected; skipping genesis");
            return Ok(());
        }
        Err(StateStoreError::NotFound(_)) => {}
        Err(e) => return Err(e.into()),
    }

    info!("initialising genesis state");
    let treasury = parse_address_hex(config.treasury_address.as_deref())?;
    let mut staking = StakingState::new();
    staking.init_treasury(treasury);

    // Load full validator set from validators.json (Phase 14 ceremony) if present.
    let validators_json_path = config.data_dir.join("validators.json");
    if validators_json_path.exists() {
        let text = std::fs::read_to_string(&validators_json_path)
            .with_context(|| format!("reading {}", validators_json_path.display()))?;
        let validators_input: Vec<GenesisValidatorInput> = serde_json::from_str(&text)
            .with_context(|| format!("parsing {}", validators_json_path.display()))?;
        for v in &validators_input {
            let address = parse_address_hex(Some(&v.address))?;
            let pk = v
                .public_key
                .as_ref()
                .map(|s| parse_public_key_hex(s))
                .transpose()?;
            let stake = v.stake.unwrap_or(kvnc_staking::MIN_VALIDATOR_STAKE);
            staking.join_validator(address, stake, 0, Some(address), pk)?;
        }
        info!(count = validators_input.len(), "loaded validators.json");
    }

    // Begin write transaction early (needed for premine + staking save)
    let txn = state_storage.begin_write()?;

    // Founder premine (200_000 KVNC) — write to state store if founder file present.
    let premine_path = config.data_dir.join("founder_premine.hex");
    if premine_path.exists() {
        let hex = std::fs::read_to_string(&premine_path)?.trim().to_string();
        if !hex.is_empty() {
            let founder = parse_address_hex(Some(&hex))?;
            use kvnc_storage::state_store::Account;
            let state = state_storage.state();
            let acct = Account {
                balance: kvnc_staking::FOUNDER_PREMINE,
                nonce: 0,
                code_hash: [0; 32],
                code: vec![],
            };
            state.set_account(&txn, &founder, &acct)?;
            info!(address = %founder, "founder premine applied");
        }
    }

    // Load 4-node validator set from genesis file if present (Phase 16.6).
    let genesis_validators_path = config.data_dir.join("genesis_validators.toml");
    if genesis_validators_path.exists() {
        let text = std::fs::read_to_string(&genesis_validators_path).with_context(|| {
            format!(
                "reading genesis validators {}",
                genesis_validators_path.display()
            )
        })?;
        let validators: GenesisValidatorsFile = toml::from_str(&text).with_context(|| {
            format!(
                "parsing genesis validators {}",
                genesis_validators_path.display()
            )
        })?;
        let validators = validators.validator;
        let count = validators.len();
        for v in validators {
            let address = parse_address_hex(Some(&v.address))?;
            let pk = if let Some(p) = v.public_key.as_ref() {
                Some(parse_public_key_hex(p)?)
            } else {
                None
            };
            staking.join_validator(address, v.stake, 0, Some(address), pk)?;
        }
        info!(count = count, "loaded genesis validator set");
    }

    state_storage.state().save_staking_state(&txn, &staking)?;
    txn.commit()?;

    let digest = StatementBlock::compute_digest(0, 0, &[], &[]);
    let genesis = StatementBlock {
        author: 0,
        round: 0,
        parents: Vec::new(),
        transactions: Vec::new(),
        statements: Vec::new(),
        signature: Signature([0u8; 64]),
        digest,
        merkle_root: Hash::zero(),
    };
    dag_store.put_block(&genesis)?;
    info!(digest = %genesis.digest, "genesis block created");
    Ok(())
}

/// Run the P2P network service and process outbound broadcast commands.
///
/// `NetworkService::start` takes `&self` and runs the swarm event loop forever,
/// so it is spawned once in its own task and owns the listen/dial preamble. The
/// command loop only calls the `&self` broadcast methods (interior mutability),
/// so a broadcast never cancels or re-enters `start` and never re-runs the
/// preamble.
async fn run_network(
    service: Arc<NetworkService>,
    mut cmd_rx: mpsc::UnboundedReceiver<NetworkCommand>,
    mut shutdown: watch::Receiver<bool>,
) {
    // Run the swarm event loop exactly once, independent of command handling.
    let swarm_service = service.clone();
    let mut swarm_task = tokio::spawn(async move {
        match swarm_service.start().await {
            Ok(()) => info!("network service stopped"),
            Err(e) => error!(error = %e, "network service error"),
        }
    });

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => match cmd {
                Some(NetworkCommand::BroadcastBlock(block)) => {
                    if let Err(e) = service.broadcast_block(&block) {
                        warn!(error = %e, "failed to broadcast block");
                    }
                }
                Some(NetworkCommand::BroadcastTransaction(tx)) => {
                    if let Err(e) = service.broadcast_transaction(&tx) {
                        warn!(error = %e, "failed to broadcast transaction");
                    }
                }
                Some(NetworkCommand::BroadcastVote(vote)) => {
                    if let Err(e) = service.broadcast_vote(&vote) {
                        warn!(error = %e, "failed to broadcast vote");
                    }
                }
                Some(NetworkCommand::RequestSync(peer, request)) => {
                    if let Err(e) = service.request_block_sync(peer, request) {
                        warn!(%peer, error = %e, "failed to request block sync");
                    }
                }
                Some(NetworkCommand::Shutdown) | None => {
                    info!("network task stopping");
                    break;
                }
            },
            res = &mut swarm_task => {
                match res {
                    Ok(()) => info!("network swarm loop exited"),
                    Err(e) => error!(error = %e, "network swarm task failed"),
                }
                break;
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    info!("network task received shutdown");
                    break;
                }
            }
        }
    }

    // Cancelling the swarm loop drops the swarm (and closes its listeners),
    // mirroring the previous shutdown behaviour; await it so the sockets are
    // released before this task returns.
    swarm_task.abort();
    let _ = swarm_task.await;
}

/// Ingest events emitted by the network layer into the DAG store and mempool.
async fn run_event_handler(
    mut network_events: mpsc::UnboundedReceiver<NetworkEvent>,
    mempool: Arc<Mempool>,
    engine: Arc<ConsensusEngine<NodeDagStore, NodeBlockManager>>,
    dag_store: Arc<DagStore>,
    network_cmd_tx: mpsc::UnboundedSender<NetworkCommand>,
    events: EventBus,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            maybe = network_events.recv() => match maybe {
                Some(event) => handle_network_event(
                    event,
                    &mempool,
                    &engine,
                    &dag_store,
                    &network_cmd_tx,
                    &events,
                ),
                None => break,
            },
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }
}

/// Handle a single [`NetworkEvent`].
fn handle_network_event(
    event: NetworkEvent,
    mempool: &Mempool,
    engine: &ConsensusEngine<NodeDagStore, NodeBlockManager>,
    dag_store: &DagStore,
    network_cmd_tx: &mpsc::UnboundedSender<NetworkCommand>,
    events: &EventBus,
) {
    match event {
        NetworkEvent::BlockReceived(block) => {
            // Hot-path validation (audit 3.1): verify signature before engine.process_block
            if let Err(e) = kvnc_crypto::verify_batch(std::slice::from_ref(&block)) {
                warn!(digest = %block.digest, error = %e, "rejected invalid block signature in main hot path");
                return;
            }
            match engine.process_block(&block) {
                Ok(()) => {
                    debug!(digest = %block.digest, "validated received block into consensus");
                    events.publish_new_head(&block);
                }
                Err(e) => {
                    warn!(digest = %block.digest, error = %e, "rejected received consensus block")
                }
            }
        }
        NetworkEvent::TransactionReceived(tx) => match mempool.add_transaction(tx.clone()) {
            Ok(()) => {
                events.publish_pending_transaction(&tx);
                // Re-gossip locally-accepted transactions.
                let _ = network_cmd_tx.send(NetworkCommand::BroadcastTransaction(tx));
            }
            Err(MempoolError::AlreadyExists) => {
                debug!(hash = %tx.hash, "transaction already in mempool");
            }
            Err(e) => warn!(error = %e, "rejected received transaction"),
        },
        NetworkEvent::VoteReceived { peer, vote } => {
            debug!(%peer, leader_round = vote.leader_round, %vote.leader_hash, "processing received vote");
            if let Err(e) = engine.process_vote(vote.leader_round, vote.voter, vote.leader_hash) {
                warn!(%peer, %e, "failed to process received vote");
            }
        }
        NetworkEvent::PeerConnected(peer) => info!(%peer, "peer connected"),
        NetworkEvent::PeerDisconnected(peer) => info!(%peer, "peer disconnected"),
        NetworkEvent::PeerDiscovered(peer, addr) => debug!(%peer, %addr, "peer discovered"),
        NetworkEvent::SyncRequest {
            peer,
            from_round,
            to_round,
        } => debug!(%peer, from_round, to_round, "sync request received"),
        NetworkEvent::BlockSyncResponse {
            peer,
            request_id: _,
            response,
        } => {
            debug!(%peer, ?response, "block sync response received");
            match response {
                BlockSyncResponse::Block(block) => {
                    // Hot-path validation before inserting into DAG
                    if let Err(e) = kvnc_crypto::verify_batch(std::slice::from_ref(&block)) {
                        warn!(digest = %block.digest, error = %e, "rejected invalid block signature from sync response");
                    } else {
                        match dag_store.put_block(&block) {
                            Ok(()) => debug!(digest = %block.digest, "synced block inserted into DAG"),
                            Err(e) => warn!(digest = %block.digest, error = %e, "failed to insert synced block"),
                        }
                    }
                }
                BlockSyncResponse::NotFound => {
                    debug!(%peer, "requested block not found on peer");
                }
                BlockSyncResponse::InvalidRequest => {
                    warn!(%peer, "block sync request was invalid");
                }
                BlockSyncResponse::MissingBlocks(hashes) => {
                    debug!(%peer, count = hashes.len(), "peer returned missing parent hashes");
                    // Re-request each missing block
                    for hash in hashes {
                        let request = BlockSyncRequest::ByHash(hash);
                        if let Err(e) = network_cmd_tx.send(NetworkCommand::RequestSync(peer, request)) {
                            debug!(error = %e, "failed to queue re-request for missing block");
                        }
                    }
                }
            }
        }
        NetworkEvent::StateSyncResponse {
            peer,
            request_id: _,
            response,
        } => {
            debug!(%peer, ?response, "state sync response received");
            match response {
                StateSyncResponse::Snapshot(data) => {
                    debug!(%peer, size = data.len(), "received state snapshot");
                    // TODO: Import snapshot into state storage
                }
                StateSyncResponse::NotFound => {
                    debug!(%peer, "requested state snapshot not found on peer");
                }
                StateSyncResponse::InvalidRequest => {
                    warn!(%peer, "state sync request was invalid");
                }
            }
        }
    }
}

/// Execute committed sub-DAGs as they are produced by consensus.
async fn run_execution(
    mut subdags: mpsc::UnboundedReceiver<kvnc_consensus::CommittedSubDag>,
    state_storage: Arc<Storage>,
    events: EventBus,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut ctx = ExecutionContext::new();
    ctx.log_publisher = Some(Box::new(EventLogPublisher(events.clone())));
    match state_storage.begin_read() {
        Ok(txn) => match state_storage.state().load_staking_state(&txn) {
            Ok(state) => ctx.staking = state,
            Err(e) => warn!(error = %e, "could not load staking state for execution"),
        },
        Err(e) => warn!(error = %e, "could not open read transaction for execution"),
    }

    loop {
        tokio::select! {
            maybe = subdags.recv() => match maybe {
                Some(subdag) => match ctx.execute_committed_subdag(&subdag, &state_storage) {
                    Ok(result) => {
                        info!(
                            round = subdag.leader_round,
                            txs = result.txs_applied,
                            "executed committed sub-DAG"
                        );
                        events.publish_committed_leader(&subdag);
                    }
                    Err(e) => {
                        error!(
                            error = %e,
                            round = subdag.leader_round,
                            "execution failed; stopping worker so committed decisions replay after restart"
                        );
                        break;
                    }
                },
                None => break,
            },
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }
}

/// Adapter implementing [`DagStoreTrait`] over the concrete [`DagStore`].
struct NodeDagStore {
    inner: Arc<DagStore>,
}

impl DagStoreTrait for NodeDagStore {
    fn get_block(&self, hash: &Hash) -> Result<StatementBlock, DagStoreError> {
        self.inner.get_block(hash)
    }

    fn get_ancestors(&self, hash: &Hash, min_round: Round) -> Result<Vec<Hash>, DagStoreError> {
        self.inner.get_ancestors(hash, min_round)
    }

    fn get_parents(&self, hash: &Hash) -> Result<Vec<Hash>, DagStoreError> {
        self.inner.get_parents(hash)
    }

    fn get_block_by_author_round(
        &self,
        author: AuthorityIndex,
        round: Round,
    ) -> Result<Option<StatementBlock>, DagStoreError> {
        self.inner.get_block_by_author_round(author, round)
    }

    fn get_blocks_by_round(&self, round: Round) -> Result<Vec<StatementBlock>, DagStoreError> {
        self.inner.get_blocks_by_round(round)
    }

    fn has_block(&self, hash: &Hash) -> Result<bool, DagStoreError> {
        self.inner.has_block(hash)
    }

    fn put_block(&self, block: &StatementBlock) -> Result<(), DagStoreError> {
        self.inner.put_block(block)
    }

    fn find_parents(
        &self,
        round: Round,
        max_parents: usize,
    ) -> Result<Vec<BlockReference>, DagStoreError> {
        self.inner.find_parents(round, max_parents)
    }

    fn commit_leader(&self, leader_hash: &Hash) -> Result<u64, DagStoreError> {
        self.inner.commit_leader(leader_hash)
    }

    fn mark_round_decided(&self, round: Round, leader_hash: &Hash) -> Result<(), DagStoreError> {
        self.inner.mark_round_decided(round, leader_hash)
    }

    fn mergeset(&self, leader: &Hash) -> Result<Vec<Hash>, DagStoreError> {
        self.inner.mergeset(leader)
    }

    fn get_blocks(&self, hashes: &[Hash]) -> Result<Vec<StatementBlock>, DagStoreError> {
        let mut blocks = Vec::new();
        for hash in hashes {
            if let Ok(block) = self.inner.get_block(hash) {
                blocks.push(block);
            }
        }
        Ok(blocks)
    }

    fn get_decided_leaders(&self, round: Round) -> Result<Vec<Hash>, DagStoreError> {
        self.inner.get_decided_leaders(round)
    }

    fn get_decided_rounds(&self, max_round: Round) -> Result<Vec<Round>, DagStoreError> {
        self.inner.get_decided_rounds(max_round)
    }

    fn prune_non_blue(
        &self,
        blue_hashes: &[Hash],
        committed_wave: u64,
    ) -> Result<u64, DagStoreError> {
        self.inner.prune_non_blue(blue_hashes, committed_wave)
    }

    fn prune_waves_before(&self, wave: u64, prune_window_waves: u64) -> Result<u64, DagStoreError> {
        self.inner.prune_waves_before(wave, prune_window_waves)
    }
}

/// Adapter implementing [`BlockManagerTrait`] over the concrete [`BlockManager`].
struct NodeBlockManager {
    inner: Arc<BlockManager>,
}

impl BlockManagerTrait for NodeBlockManager {
    fn propose_block(&self, round: Round) -> Result<StatementBlock, BlockManagerError> {
        self.inner.propose_block(round)
    }

    fn propose_block_with_txs(
        &self,
        round: Round,
        transactions: Vec<Transaction>,
    ) -> Result<StatementBlock, BlockManagerError> {
        self.inner.propose_block_with_txs(round, transactions)
    }

    fn process_block(&self, block: &StatementBlock) -> Result<(), BlockManagerError> {
        self.inner.process_block(block)
    }

    fn our_authority(&self) -> AuthorityIndex {
        self.inner.our_authority()
    }

    fn set_signing_key(&self, key: SigningKey) {
        self.inner.set_signing_key(key);
    }
}

/// Reconstruct durable commit decisions for restart-safe execution replay.
///
/// M3: the ancestry walks below pass `min_round = 0` (unbounded, back to
/// genesis) and rely on `DagStore::get_ancestors` being NotFound tolerant of
/// dangling edges left by `prune_waves_before`, rather than bounding the walk.
/// This mirrors the live committer's `get_previous_tips`; both places must
/// stay consistent.
///
// TODO(M4): replay currently stops using the *global* `last_committed` in the
// mergeset walk, so recovery of an older sub-DAG can be truncated by a newer
// commit. Review whether replay should pass an explicit stop-round (the
// sub-DAG's own leader) instead; leave behaviour unchanged until that is
// clear to avoid destabilising recovery.
fn recover_committed_subdags(
    dag_store: &DagStore,
    use_mysticghost: bool,
) -> Result<Vec<kvnc_consensus::CommittedSubDag>> {
    let rounds = dag_store.get_decided_rounds(u64::MAX)?;
    let mut subdags = Vec::new();

    // Pre-compute all decided leaders for previous tips
    let mut all_previous_tips = Vec::new();
    for round in &rounds {
        let leaders = dag_store.get_decided_leaders(*round)?;
        for leader_hash in leaders {
            if let Ok(block) = dag_store.get_block(&leader_hash) {
                if block.round == *round && block.digest == leader_hash {
                    all_previous_tips.push(leader_hash);
                }
            }
        }
    }

    for round in rounds {
        for leader_hash in dag_store.get_decided_leaders(round)? {
            // Skip decided-round entries whose leader block has been pruned:
            // recovery must remain possible after pruning rather than fail.
            let leader = match dag_store.get_block(&leader_hash) {
                Ok(block) => block,
                Err(kvnc_dag::DagStoreError::NotFound(_)) => continue,
                Err(e) => return Err(e.into()),
            };
            if leader.round != round || leader.digest != leader_hash {
                anyhow::bail!(
                    "persisted commit decision for round {round} references inconsistent leader block (block round {}, block digest {}, expected digest {leader_hash})",
                    leader.round,
                    leader.digest,
                );
            }

            if use_mysticghost {
                // Previous tips must be the decided leaders that are neither
                // this leader nor ancestors of it. This mirrors the live
                // committer's `get_previous_tips` filter so restart recovery
                // produces the same committed sub-DAG as first-time commit.
                let ancestors: std::collections::HashSet<Hash> = dag_store
                    .get_ancestors(&leader_hash, 0)?
                    .into_iter()
                    .collect();
                let previous_tips: Vec<Hash> = all_previous_tips
                    .iter()
                    .copied()
                    .filter(|h| *h != leader_hash && !ancestors.contains(h))
                    .collect();
                let subdag = recover_committed_subdag_mysticghost(
                    dag_store,
                    &leader,
                    round,
                    &previous_tips,
                )?;
                subdags.push(subdag);
            } else {
                // Original linearizer path (bit-identical to current behaviour)
                let history = dag_store
                    .get_ancestors(&leader_hash, 0)?
                    .into_iter()
                    .map(|hash| dag_store.get_block(&hash))
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                subdags.push(kvnc_consensus::Linearizer::new().linearize(leader, history));
            }
        }
    }
    subdags.sort_by_key(|subdag| {
        (
            subdag.leader_round,
            subdag.leader_author,
            subdag.leader.digest.0,
        )
    });
    Ok(subdags)
}

/// MysticGhost recovery path for a single committed sub-DAG.
fn recover_committed_subdag_mysticghost(
    dag_store: &DagStore,
    leader_block: &StatementBlock,
    leader_round: Round,
    previous_tips: &[Hash],
) -> Result<kvnc_consensus::CommittedSubDag> {
    use kvnc_consensus::mysticghost::{order_committed_wave, MysticGhostConfig, MysticGhostOrder};

    // 1. Get mergeset hashes
    let mergeset_hashes = dag_store.mergeset(&leader_block.digest)?;

    // 2. Get mergeset blocks
    let mut mergeset_blocks = Vec::new();
    for hash in mergeset_hashes {
        if let Ok(block) = dag_store.get_block(&hash) {
            mergeset_blocks.push(block);
        }
    }

    // 3. Configure MysticGhost
    let mg_config = MysticGhostConfig {
        enabled: true,
        k: 3,
        max_mergeset_blocks: 2_000,
    };

    // 4. Run MysticGhost ordering
    match order_committed_wave(&mg_config, &mergeset_blocks, previous_tips) {
        MysticGhostOrder::Ghost { colouring } => {
            // Use the blue-set order from GHOSTDAG
            let blue_ordered = colouring.blue_ordered();

            // Build blocks in blue order, filtering to only those in mergeset
            let block_map: std::collections::HashMap<Hash, StatementBlock> =
                mergeset_blocks.into_iter().map(|b| (b.digest, b)).collect();

            let mut ordered_blocks = Vec::new();
            for hash in blue_ordered {
                if let Some(block) = block_map.get(&hash) {
                    ordered_blocks.push(block.clone());
                }
            }

            // Ensure leader is included (should be blue)
            if !ordered_blocks
                .iter()
                .any(|b| b.digest == leader_block.digest)
            {
                ordered_blocks.push(leader_block.clone());
            }

            Ok(kvnc_consensus::CommittedSubDag {
                blocks: ordered_blocks,
                leader: leader_block.clone(),
                leader_round,
                leader_author: leader_block.author,
            })
        }
        MysticGhostOrder::Fallback => {
            // Fall back to original linearizer
            let history = dag_store
                .get_ancestors(&leader_block.digest, 0)?
                .into_iter()
                .map(|hash| dag_store.get_block(&hash))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(kvnc_consensus::Linearizer::new().linearize(leader_block.clone(), history))
        }
    }
}

/// Build the committee from active validators in StakingState.
fn build_committee(
    storage: &Storage,
    local_public_key: &PublicKey,
    local_address: &Address,
    listen_addr: &str,
) -> Result<CommitteeInfo> {
    let read = storage.begin_read()?;
    let staking_state = storage.state().load_staking_state(&read)?;

    // Filter active validators with sufficient stake
    let mut active_validators: Vec<_> = staking_state
        .validators
        .iter()
        .filter(|v| v.active && v.stake >= MIN_VALIDATOR_STAKE)
        .collect();

    // If no validators in staking state (fresh genesis), fall back to local validator identity
    // This allows a fresh node to start and produce blocks until validators join via staking txs.
    if active_validators.is_empty() {
        info!("no active validators in staking state; falling back to local validator identity");
        let authority = AuthorityInfo {
            index: 0,
            stake: MIN_VALIDATOR_STAKE,
            public_key: *local_public_key,
            address: *local_address,
            network_address: listen_addr.to_string(),
        };
        return Ok(CommitteeInfo::try_new(0, vec![authority])?);
    }

    // Sort by address for deterministic ordering across all nodes
    active_validators.sort_by_key(|a| a.address.0);

    // Take up to MAX_ACTIVE_VALIDATORS top validators by stake (then by address for tie-breaking)
    // Sort by stake desc, take top 21, then sort by address for index assignment.
    active_validators.sort_by(|a, b| {
        b.stake
            .cmp(&a.stake)
            .then_with(|| a.address.0.cmp(&b.address.0))
    });
    let top_validators = active_validators
        .into_iter()
        .take(MAX_ACTIVE_VALIDATORS)
        .collect::<Vec<_>>();

    // Re-sort by address for stable index assignment
    let mut top_validators = top_validators;
    top_validators.sort_by_key(|a| a.address.0);

    if top_validators.is_empty() {
        anyhow::bail!("no active validators found in staking state; committee cannot be empty");
    }

    let mut authorities = Vec::with_capacity(top_validators.len());
    for (index, validator) in top_validators.iter().enumerate() {
        // Determine public key: use local node's key if this is our validator, otherwise use stored key
        let public_key = if validator.address == *local_address {
            *local_public_key
        } else if let Some(pk) = validator.public_key {
            pk
        } else {
            anyhow::bail!(
                "validator {} missing public key; run registration or add to genesis",
                validator.address
            );
        };

        let authority = AuthorityInfo {
            index: index as u16,
            stake: validator.stake,
            public_key,
            address: validator.address,
            network_address: listen_addr.to_string(),
        };
        authorities.push(authority);
    }

    if authorities.is_empty() {
        anyhow::bail!("no validators with registered public keys; committee cannot be empty");
    }

    Ok(CommitteeInfo::try_new(0, authorities)?)
}

/// Build the [`NetworkConfig`] from the node configuration.
fn build_network_config(config: &NodeConfig) -> Result<NetworkConfig> {
    let listen = to_multiaddr(&config.listen_addr)?;

    let mut bootstrap_nodes = Vec::with_capacity(config.bootnodes.len());
    for node in &config.bootnodes {
        match to_multiaddr(node) {
            Ok(addr) => bootstrap_nodes.push(addr),
            Err(e) => warn!(node = %node, error = %e, "skipping invalid bootnode"),
        }
    }

    Ok(NetworkConfig {
        listen_addrs: vec![listen],
        bootstrap_nodes,
        max_peers: config.max_peers,
        ping_interval: Duration::from_secs(10),
    })
}

/// Genesis validator entry for Phase 16.6 4-node quorum.
#[derive(Debug, Deserialize)]
struct GenesisValidatorEntry {
    address: String,
    stake: u64,
    public_key: Option<String>,
}

/// Top-level shape of `genesis_validators.toml` (`[[validator]]` array of tables).
#[derive(Debug, Deserialize)]
struct GenesisValidatorsFile {
    validator: Vec<GenesisValidatorEntry>,
}

/// Parse a 32-byte hex public key (PublicKey is [u8; 32]).
fn parse_public_key_hex(value: &str) -> Result<PublicKey> {
    let bytes =
        hex::decode(value.trim()).with_context(|| format!("invalid public key hex `{}`", value))?;
    if bytes.len() != 32 {
        anyhow::bail!("public key must be 32 bytes, got {}", bytes.len());
    }
    let mut pk = [0u8; 32];
    pk.copy_from_slice(&bytes);
    Ok(PublicKey(pk))
}

/// Convert a `host:port` string or a full multiaddr into a [`Multiaddr`].
fn to_multiaddr(value: &str) -> Result<Multiaddr> {
    let value = value.trim();
    if value.starts_with('/') {
        return value
            .parse::<Multiaddr>()
            .with_context(|| format!("invalid multiaddr `{value}`"));
    }

    let (host, port) = value
        .rsplit_once(':')
        .with_context(|| format!("expected `host:port`, got `{value}`"))?;
    let port: u16 = port
        .parse()
        .with_context(|| format!("invalid port in `{value}`"))?;

    let multiaddr = if host.parse::<std::net::Ipv4Addr>().is_ok() {
        format!("/ip4/{host}/tcp/{port}")
    } else if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("/ip6/{host}/tcp/{port}")
    } else {
        format!("/dns4/{host}/tcp/{port}")
    };

    multiaddr
        .parse::<Multiaddr>()
        .with_context(|| format!("invalid address `{value}`"))
}

/// Load a validator signing key from a hex-seed file, or generate an ephemeral
/// key when none is configured.
fn load_validator_key(path: Option<&str>) -> (SigningKey, PublicKey) {
    if let Some(path) = path {
        match read_signing_key(path) {
            Ok(signing_key) => {
                let public_key = PublicKey::from(signing_key.verifying_key());
                info!(path, "loaded validator key");
                return (signing_key, public_key);
            }
            Err(e) => warn!(
                path,
                error = %e,
                "failed to load validator key; generating an ephemeral key"
            ),
        }
    } else {
        info!("no validator key configured; generating an ephemeral key");
    }

    // TODO: support encrypted keystores / OS keychains instead of a raw hex
    // seed file. Keys must never be logged or transmitted to peers.
    kvnc_crypto::generate_keypair()
}

/// Read a 32-byte hex seed from `path` and build a signing key.
fn read_signing_key(path: &str) -> Result<SigningKey> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("reading key file {path}"))?;
    let bytes = hex::decode(raw.trim()).with_context(|| "key file must contain hex")?;
    if bytes.len() != 32 {
        anyhow::bail!(
            "validator key must be a 32-byte hex seed, got {} bytes",
            bytes.len()
        );
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&bytes);
    Ok(SigningKey::from_bytes(&seed))
}

/// Parse an optional 32-byte hex address, defaulting to the zero address.
fn parse_address_hex(value: Option<&str>) -> Result<Address> {
    let Some(value) = value else {
        return Ok(Address::default());
    };

    let bytes =
        hex::decode(value.trim()).with_context(|| format!("invalid address hex `{value}`"))?;
    if bytes.len() != 32 {
        anyhow::bail!("address must be 32 bytes, got {}", bytes.len());
    }
    let mut address = [0u8; 32];
    address.copy_from_slice(&bytes);
    Ok(Address(address))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvnc_staking::{StakingState, MAX_ACTIVE_VALIDATORS, MIN_VALIDATOR_STAKE, ONE_KVNC};
    use kvnc_storage::BincodeSerialize;
    use redb::{Database, TableDefinition};
    use std::collections::HashMap;

    const DAG_BLOCKS: TableDefinition<[u8; 32], Vec<u8>> = TableDefinition::new("dag_blocks");

    #[test]
    fn host_port_converts_to_multiaddr() {
        assert_eq!(
            to_multiaddr("127.0.0.1:9000").unwrap().to_string(),
            "/ip4/127.0.0.1/tcp/9000"
        );
        assert_eq!(
            to_multiaddr("seed.kovanica.online:9000")
                .unwrap()
                .to_string(),
            "/dns4/seed.kovanica.online/tcp/9000"
        );
        assert_eq!(
            to_multiaddr("/ip4/0.0.0.0/tcp/9000").unwrap().to_string(),
            "/ip4/0.0.0.0/tcp/9000"
        );
        assert!(to_multiaddr("not-an-address").is_err());
    }

    #[test]
    fn zero_address_is_default_treasury() {
        assert_eq!(parse_address_hex(None).unwrap(), Address::default());
        assert!(parse_address_hex(Some("zz")).is_err());
    }

    #[test]
    fn committed_subdag_decisions_are_recovered_from_dag_storage() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::new(dir.path().join("dag.redb")).unwrap();
        let dag = DagStore::new(storage).unwrap();
        let digest = StatementBlock::compute_digest(0, 1, &[], &[]);
        let block = StatementBlock {
            author: 0,
            round: 1,
            parents: Vec::new(),
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: Signature([0; 64]),
            digest,
            merkle_root: Default::default(),
        };
        dag.put_block(&block).unwrap();
        dag.mark_round_decided(1, &digest).unwrap();

        let recovered = recover_committed_subdags(&dag, false).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].leader.digest, digest);
        assert_eq!(recovered[0].blocks.len(), 1);
    }

    #[test]
    fn committed_subdag_recovery_skips_missing_decided_block() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::new(dir.path().join("dag.redb")).unwrap();
        let dag = DagStore::new(storage).unwrap();
        let valid_digest = StatementBlock::compute_digest(0, 1, &[], &[]);
        let valid_block = StatementBlock {
            author: 0,
            round: 1,
            parents: Vec::new(),
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: Signature([0; 64]),
            digest: valid_digest,
            merkle_root: Default::default(),
        };
        dag.put_block(&valid_block).unwrap();
        dag.mark_round_decided(1, &valid_digest).unwrap();

        let missing_digest = StatementBlock::compute_digest(0, 2, &[], &[]);
        dag.mark_round_decided(2, &missing_digest).unwrap();

        // A pruned/missing decided leader must be skipped, not fatal: recovery
        // still reconstructs the decisions whose blocks remain available.
        let recovered = recover_committed_subdags(&dag, false).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].leader.digest, valid_digest);
    }

    #[test]
    fn committed_subdag_recovery_rejects_decision_round_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::new(dir.path().join("dag.redb")).unwrap();
        let dag = DagStore::new(storage).unwrap();
        let digest = StatementBlock::compute_digest(0, 2, &[], &[]);
        let block = StatementBlock {
            author: 0,
            round: 2,
            parents: Vec::new(),
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: Signature([0; 64]),
            digest,
            merkle_root: Default::default(),
        };
        dag.put_block(&block).unwrap();
        dag.mark_round_decided(1, &digest).unwrap();

        let error = recover_committed_subdags(&dag, false).unwrap_err();
        assert!(
            error.to_string().contains("round 1"),
            "unexpected recovery error: {error}"
        );
        assert!(
            error.to_string().contains("block round 2"),
            "unexpected recovery error: {error}"
        );
    }

    #[test]
    fn committed_subdag_recovery_rejects_embedded_digest_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("dag.redb");
        let storage = Storage::new(&db_path).unwrap();
        let dag = DagStore::new(storage).unwrap();
        let digest = StatementBlock::compute_digest(0, 1, &[], &[]);
        let block = StatementBlock {
            author: 0,
            round: 1,
            parents: Vec::new(),
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: Signature([0; 64]),
            digest,
            merkle_root: Default::default(),
        };
        dag.put_block(&block).unwrap();
        dag.mark_round_decided(1, &digest).unwrap();
        drop(dag);

        let raw_db = Database::open(&db_path).unwrap();
        let mut corrupted_block = block;
        corrupted_block.digest = Hash::new("kvnc-test/embedded-digest-mismatch");
        let write_txn = raw_db.begin_write().unwrap();
        {
            let mut table = write_txn.open_table(DAG_BLOCKS).unwrap();
            table
                .insert(digest.0, corrupted_block.to_bytes().unwrap())
                .unwrap();
        }
        write_txn.commit().unwrap();
        drop(raw_db);

        let storage = Storage::new(&db_path).unwrap();
        let dag = DagStore::new(storage).unwrap();
        let error = recover_committed_subdags(&dag, false)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("block digest"),
            "recovery error should identify the mismatched block digest: {error}"
        );
        assert!(
            error.contains(&format!("expected digest {digest}")),
            "recovery error should identify the persisted decision digest: {error}"
        );
    }

    #[test]
    fn committed_subdag_recovery_with_mysticghost_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::new(dir.path().join("dag.redb")).unwrap();
        let dag = DagStore::new(storage).unwrap();
        let digest = StatementBlock::compute_digest(0, 1, &[], &[]);
        let block = StatementBlock {
            author: 0,
            round: 1,
            parents: Vec::new(),
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: Signature([0; 64]),
            digest,
            merkle_root: Default::default(),
        };
        dag.put_block(&block).unwrap();
        dag.mark_round_decided(1, &digest).unwrap();

        // Test recovery with MysticGhost enabled (should fall back to linearizer for simple case)
        let recovered = recover_committed_subdags(&dag, true).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].leader.digest, digest);
        assert_eq!(recovered[0].blocks.len(), 1);
    }

    #[tokio::test]
    async fn quorum_commit_reaches_execution_and_persists_state() {
        let dir = tempfile::tempdir().unwrap();
        let dag =
            Arc::new(DagStore::new(Storage::new(dir.path().join("dag.redb")).unwrap()).unwrap());
        let state_storage = Arc::new(Storage::new(dir.path().join("state.redb")).unwrap());
        let (signing_key, public_key) = kvnc_crypto::generate_keypair();
        let validator_address = Address::from_public_key(&public_key);
        let payout = Address([0x91; 32]);
        let mut staking = StakingState::new();
        staking
            .join_validator(
                validator_address,
                MIN_VALIDATOR_STAKE,
                0,
                Some(payout),
                Some(public_key),
            )
            .unwrap();
        let txn = state_storage.begin_write().unwrap();
        state_storage
            .state()
            .save_staking_state(&txn, &staking)
            .unwrap();
        txn.commit().unwrap();

        let committee = build_committee(
            &state_storage,
            &public_key,
            &validator_address,
            "127.0.0.1:0",
        )
        .expect("single-validator committee is valid");
        let block_manager = Arc::new(BlockManager::new(dag.clone()));
        block_manager.set_authority(0);
        block_manager.set_signing_key(signing_key.clone());
        block_manager.set_authority_keys(HashMap::from([(0, public_key)]));
        let engine = ConsensusEngine::new(
            ConsensusConfig::default(),
            committee,
            Arc::new(NodeDagStore { inner: dag.clone() }),
            Arc::new(RwLock::new(NodeBlockManager {
                inner: block_manager,
            })),
            signing_key.clone(),
            None, // No mempool for this test
        );
        let (exec_tx, exec_rx) = mpsc::unbounded_channel();
        engine.set_commit_sender(exec_tx);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let execution = tokio::spawn(run_execution(
            exec_rx,
            state_storage.clone(),
            EventBus::new(),
            shutdown_rx,
        ));

        let leader_round = 3;
        let digest = StatementBlock::compute_digest(0, leader_round, &[], &[]);
        let block = StatementBlock {
            author: 0,
            round: leader_round,
            parents: Vec::new(),
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: kvnc_crypto::sign(&signing_key, digest.as_ref()),
            digest,
            merkle_root: Default::default(),
        };
        engine.process_block(&block).unwrap();
        engine.process_vote(leader_round, 0, digest).unwrap();

        let mut committed_height = 0;
        for _ in 0..100 {
            let read = state_storage.begin_read().unwrap();
            committed_height = state_storage
                .state()
                .load_staking_state(&read)
                .unwrap()
                .committed_leader_height;
            if committed_height == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(committed_height, 1, "execution consumed committed sub-DAG");
        let read = state_storage.begin_read().unwrap();
        let account = state_storage
            .state()
            .get_account_or_default(&read, &payout)
            .unwrap();
        assert!(
            account.balance > 0,
            "leader reward persisted to payout account"
        );

        shutdown_tx.send(true).unwrap();
        execution.await.unwrap();
    }

    #[tokio::test]
    async fn execution_worker_stops_after_failure_before_later_commits() {
        let dir = tempfile::tempdir().unwrap();
        let state_storage = Arc::new(Storage::new(dir.path().join("state.redb")).unwrap());
        let validator_address = Address([0x22; 32]);
        let payout = Address([0xa1; 32]);
        let mut staking = StakingState::new();
        staking
            .join_validator(
                validator_address,
                MIN_VALIDATOR_STAKE,
                0,
                Some(payout),
                Some(PublicKey([0x22; 32])),
            )
            .unwrap();
        let txn = state_storage.begin_write().unwrap();
        state_storage
            .state()
            .save_staking_state(&txn, &staking)
            .unwrap();
        txn.commit().unwrap();

        let committed_subdag = |round, author| {
            let digest = StatementBlock::compute_digest(author, round, &[], &[]);
            let leader = StatementBlock {
                author,
                round,
                parents: Vec::new(),
                transactions: Vec::new(),
                statements: Vec::new(),
                signature: Signature([0; 64]),
                digest,
                merkle_root: Default::default(),
            };
            kvnc_consensus::CommittedSubDag {
                blocks: vec![leader.clone()],
                leader,
                leader_round: round,
                leader_author: author,
            }
        };
        let (exec_tx, exec_rx) = mpsc::unbounded_channel();
        exec_tx.send(committed_subdag(1, 9)).unwrap(); // Unregistered authority fails.
        exec_tx.send(committed_subdag(2, 0)).unwrap(); // Must not be executed next.
        drop(exec_tx);

        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        run_execution(exec_rx, state_storage.clone(), EventBus::new(), shutdown_rx).await;

        let read = state_storage.begin_read().unwrap();
        let state = state_storage.state().load_staking_state(&read).unwrap();
        assert_eq!(
            state.committed_leader_height, 0,
            "worker must not advance to a later queued commit after failure"
        );
        assert_eq!(
            state_storage
                .state()
                .get_account_or_default(&read, &payout)
                .unwrap()
                .balance,
            0,
            "later committed leader must not receive a reward"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn node_starts_and_shuts_down() {
        let rpc_port = free_port();
        let dir = tempfile::tempdir().expect("tempdir");

        let config = NodeConfig {
            data_dir: dir.path().to_path_buf(),
            listen_addr: "127.0.0.1:0".to_string(),
            rpc_addr: "127.0.0.1".to_string(),
            rpc_port,
            bootnodes: Vec::new(),
            round_duration_ms: 100,
            ..Default::default()
        };

        // `shutdown_signal()` installs the SIGTERM handler synchronously before
        // the node task is spawned, so the process is safe to signal below.
        let node = tokio::spawn(run_node(config, shutdown_signal()));

        wait_for_port(rpc_port, Duration::from_secs(10)).await;

        let status = std::process::Command::new("kill")
            .arg("-TERM")
            .arg(std::process::id().to_string())
            .status()
            .expect("run kill");
        assert!(status.success(), "kill -TERM failed");

        let joined = tokio::time::timeout(Duration::from_secs(10), node)
            .await
            .expect("node did not shut down in time")
            .expect("node task panicked");
        assert!(joined.is_ok(), "node returned error: {joined:?}");
    }

    #[cfg(unix)]
    fn free_port() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        listener.local_addr().expect("local addr").port()
    }

    #[cfg(unix)]
    async fn wait_for_port(port: u16, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "RPC port {port} did not open in time"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[test]
    fn build_committee_from_staking_state() {
        // Create a temp directory for the state database
        let dir = tempfile::tempdir().unwrap();
        let state_storage = Storage::new(dir.path().join("state.redb")).unwrap();

        // Generate 4 validators with different stakes and addresses
        let (_sk1, pk1) = kvnc_crypto::generate_keypair();
        let addr1 = Address::from_public_key(&pk1);
        let (_sk2, pk2) = kvnc_crypto::generate_keypair();
        let addr2 = Address::from_public_key(&pk2);
        let (_sk3, pk3) = kvnc_crypto::generate_keypair();
        let addr3 = Address::from_public_key(&pk3);
        let (_sk4, pk4) = kvnc_crypto::generate_keypair();
        let addr4 = Address::from_public_key(&pk4);

        // Create staking state with 4 validators with different stakes
        let mut staking = StakingState::new();
        staking.init_treasury(Address([0xaa; 32]));

        // Validator stakes: 100K, 80K, 120K, 60K KVNC (all above MIN_VALIDATOR_STAKE = 50K)
        staking
            .join_validator(addr1, 100_000 * ONE_KVNC, 0, Some(addr1), Some(pk1))
            .unwrap();
        staking
            .join_validator(addr2, 80_000 * ONE_KVNC, 0, Some(addr2), Some(pk2))
            .unwrap();
        staking
            .join_validator(addr3, 120_000 * ONE_KVNC, 0, Some(addr3), Some(pk3))
            .unwrap();
        staking
            .join_validator(addr4, 60_000 * ONE_KVNC, 0, Some(addr4), Some(pk4))
            .unwrap();

        // Save to storage
        let txn = state_storage.begin_write().unwrap();
        state_storage
            .state()
            .save_staking_state(&txn, &staking)
            .unwrap();
        txn.commit().unwrap();

        // Use addr1 as the "local" validator (first one by address sort)
        let local_pk = pk1;
        let local_addr = addr1;

        // Build committee
        let committee = build_committee(&state_storage, &local_pk, &local_addr, "127.0.0.1:0")
            .expect("committee should be built successfully");

        // Verify committee has all 4 validators
        assert_eq!(committee.size(), 4, "committee should have 4 validators");

        // Verify validators are sorted by address (deterministic ordering)
        // Since we sort by address for index assignment, the order should be by address
        let authorities = committee.authorities();

        // Collect addresses in index order
        let committee_addresses: Vec<Address> = authorities.iter().map(|a| a.address).collect();

        // Sort the expected addresses to match committee ordering
        let mut expected_addresses = vec![addr1, addr2, addr3, addr4];
        expected_addresses.sort_by_key(|a| a.0);

        assert_eq!(
            committee_addresses, expected_addresses,
            "committee validators should be sorted by address for deterministic index assignment"
        );

        // Verify indices are 0..3
        for (idx, authority) in authorities.iter().enumerate() {
            assert_eq!(
                authority.index, idx as u16,
                "authority index should match sorted position"
            );
        }

        // Verify stakes match
        let addr_to_stake = vec![
            (addr1, 100_000 * ONE_KVNC),
            (addr2, 80_000 * ONE_KVNC),
            (addr3, 120_000 * ONE_KVNC),
            (addr4, 60_000 * ONE_KVNC),
        ];
        for (addr, expected_stake) in addr_to_stake {
            let auth = committee
                .get_by_index(expected_addresses.iter().position(|a| *a == addr).unwrap() as u16)
                .expect("authority should exist");
            assert_eq!(
                auth.stake, expected_stake,
                "stake should match for address {}",
                addr
            );
        }

        // Verify leader round-robin selection
        // Round 0 -> index 0, Round 1 -> index 1, etc.
        for round in 0..8 {
            let leader_idx = committee.leader(round);
            let expected_idx = (round as usize) % 4;
            assert_eq!(
                leader_idx, expected_idx as u16,
                "leader for round {} should be index {} (round-robin)",
                round, expected_idx
            );
        }
    }

    #[test]
    fn build_committee_skips_inactive_and_low_stake() {
        let dir = tempfile::tempdir().unwrap();
        let state_storage = Storage::new(dir.path().join("state.redb")).unwrap();

        let (_sk1, pk1) = kvnc_crypto::generate_keypair();
        let addr1 = Address::from_public_key(&pk1);
        let (_sk2, pk2) = kvnc_crypto::generate_keypair();
        let addr2 = Address::from_public_key(&pk2);
        let (_sk3, pk3) = kvnc_crypto::generate_keypair();
        let addr3 = Address::from_public_key(&pk3);

        let mut staking = StakingState::new();
        staking.init_treasury(Address([0xbb; 32]));

        // Active, sufficient stake
        staking
            .join_validator(addr1, 100_000 * ONE_KVNC, 0, Some(addr1), Some(pk1))
            .unwrap();
        // Inactive - should be skipped
        staking
            .join_validator(addr2, 100_000 * ONE_KVNC, 0, Some(addr2), Some(pk2))
            .unwrap();
        staking
            .validators
            .iter_mut()
            .find(|v| v.address == addr2)
            .unwrap()
            .active = false;
        // Below MIN_VALIDATOR_STAKE - manually add (bypassing join_validator check)
        staking.validators.push(kvnc_staking::ValidatorInfo {
            address: addr3,
            stake: 10_000 * ONE_KVNC,
            commission_bps: 0,
            active: true,
            payout_address: addr3,
            public_key: Some(pk3),
        });
        staking.total_staked += 10_000 * ONE_KVNC;

        let txn = state_storage.begin_write().unwrap();
        state_storage
            .state()
            .save_staking_state(&txn, &staking)
            .unwrap();
        txn.commit().unwrap();

        let committee = build_committee(&state_storage, &pk1, &addr1, "127.0.0.1:0")
            .expect("committee should be built with only active, sufficient-stake validators");

        assert_eq!(
            committee.size(),
            1,
            "only 1 validator should be active and have sufficient stake"
        );
        assert_eq!(committee.authorities()[0].address, addr1);
    }

    #[test]
    fn build_committee_limits_to_max_active_validators() {
        let dir = tempfile::tempdir().unwrap();
        let state_storage = Storage::new(dir.path().join("state.redb")).unwrap();

        let mut staking = StakingState::new();
        staking.init_treasury(Address([0xcc; 32]));

        // Create 25 validators (MAX_ACTIVE_VALIDATORS = 21) - manually bypass join_validator limit
        let mut validators = Vec::new();
        for _ in 0..25 {
            let (_sk, pk) = kvnc_crypto::generate_keypair();
            let addr = Address::from_public_key(&pk);
            validators.push((addr, pk));
            staking.validators.push(kvnc_staking::ValidatorInfo {
                address: addr,
                stake: 100_000 * ONE_KVNC,
                commission_bps: 0,
                active: true,
                payout_address: addr,
                public_key: Some(pk),
            });
            staking.total_staked += 100_000 * ONE_KVNC;
        }

        let txn = state_storage.begin_write().unwrap();
        state_storage
            .state()
            .save_staking_state(&txn, &staking)
            .unwrap();
        txn.commit().unwrap();

        // Use first validator as local
        let (local_addr, local_pk) = validators[0];

        let committee = build_committee(&state_storage, &local_pk, &local_addr, "127.0.0.1:0")
            .expect("committee should be built");

        // Should only have MAX_ACTIVE_VALIDATORS (21)
        assert_eq!(
            committee.size(),
            MAX_ACTIVE_VALIDATORS,
            "committee should be limited to MAX_ACTIVE_VALIDATORS"
        );
    }
}
