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
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use multiaddr::Multiaddr;
use parking_lot::RwLock;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, warn};

use config::NodeConfig;

use kvnc_consensus::engine::{BlockManagerTrait, DagStoreTrait};
use kvnc_consensus::{AuthorityInfo, CommitteeInfo, ConsensusConfig, ConsensusEngine};
use kvnc_dag::{BlockManager, BlockManagerError, DagStore, DagStoreError};
use kvnc_execution::ExecutionContext;
use kvnc_mempool::{Mempool, MempoolConfig, MempoolError};
use kvnc_network::{NetworkConfig, NetworkEvent, NetworkService};
use kvnc_rpc::{RpcServer, RpcState};
use kvnc_staking::{StakingState, MIN_VALIDATOR_STAKE};
use kvnc_storage::{StateStoreError, Storage};
use kvnc_types::{
    block::{BlockReference, StatementBlock},
    Address, AuthorityIndex, Hash, PublicKey, Round, Signature, SigningKey, Transaction,
    MAX_TXS_PER_BLOCK,
};

/// Command line arguments.
#[derive(Parser, Debug)]
#[command(name = "kvnc-node")]
#[command(about = "Kovanica (KVNC) full node", long_about = None)]
struct Args {
    /// Path to config file
    #[arg(short, long, default_value = "config.toml")]
    config: String,
}

/// Outbound network commands handled by the network task.
enum NetworkCommand {
    /// Gossip a newly produced block.
    BroadcastBlock(StatementBlock),
    /// Gossip a newly accepted transaction.
    BroadcastTransaction(Transaction),
    /// Ask the network task to stop.
    Shutdown,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let args = Args::parse();
    let config = NodeConfig::load(&args.config)?;
    info!(data_dir = %config.data_dir.display(), "starting KVNC node");

    run_node(config, shutdown_signal()).await
}

/// Install the tracing subscriber, honouring `RUST_LOG` when set.
fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
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

    // Load the staking state and build the committee up front so both the RPC
    // server and the consensus engine can share them.
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
    let committee = build_committee(&public_key, &validator_address, &config.listen_addr)?;

    // ------------------------------------------------------------------
    // 4. Networking
    // ------------------------------------------------------------------
    let network_config = build_network_config(&config)?;
    let (network, network_events) =
        NetworkService::new(network_config, dag_store.clone(), mempool.clone())
            .context("creating network service")?;
    let peer_count = network.peer_count_handle();
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
    let rpc_state = RpcState {
        storage: state_storage.clone(),
        mempool: mempool.clone(),
        staking: Arc::new(tokio::sync::RwLock::new(staking_state)),
        committee: committee.clone(),
        peer_count,
    };
    let rpc_server = RpcServer::new(rpc_socket, rpc_state).await;
    let rpc_handle = rpc_server.start().await.context("starting RPC server")?;

    // ------------------------------------------------------------------
    // 6. Consensus engine
    // ------------------------------------------------------------------
    // The node-provided validator identity remains authoritative in the block
    // manager; consensus does not generate or replace a signing key.
    let block_manager = Arc::new(BlockManager::new(dag_store.clone()));
    block_manager.set_authority(0);
    block_manager.set_signing_key(signing_key.clone());
    block_manager.set_authority_keys(
        committee
            .authorities()
            .iter()
            .map(|authority| (authority.index, authority.public_key))
            .collect(),
    );
    let engine = Arc::new(ConsensusEngine::new(
        ConsensusConfig {
            round_duration_ms: config.round_duration_ms,
            use_mysticghost: config.use_mysticghost,
            ..Default::default()
        },
        committee,
        Arc::new(NodeDagStore {
            inner: dag_store.clone(),
        }),
        Arc::new(RwLock::new(NodeBlockManager {
            inner: block_manager.clone(),
        })),
        signing_key.clone(),
    ));

    // ------------------------------------------------------------------
    // 7. Shutdown plumbing + task spawning
    // ------------------------------------------------------------------
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (exec_tx, exec_rx) = mpsc::unbounded_channel::<kvnc_consensus::CommittedSubDag>();
    engine.set_commit_sender(exec_tx.clone());
    for subdag in recover_committed_subdags(&dag_store)? {
        exec_tx
            .send(subdag)
            .context("queueing a previously committed sub-DAG for replay")?;
    }

    let network_task = tokio::spawn(run_network(network, network_cmd_rx, shutdown_rx.clone()));
    let event_task = tokio::spawn(run_event_handler(
        network_events,
        mempool.clone(),
        engine.clone(),
        network_cmd_tx.clone(),
        shutdown_rx.clone(),
    ));
    let builder_task = tokio::spawn(run_block_builder(
        config.round_duration_ms,
        dag_store.clone(),
        mempool.clone(),
        network_cmd_tx.clone(),
        signing_key,
        shutdown_rx.clone(),
    ));
    let exec_task = tokio::spawn(run_execution(
        exec_rx,
        state_storage.clone(),
        shutdown_rx.clone(),
    ));

    let engine_for_task = engine.clone();
    let engine_task = tokio::spawn(async move {
        if let Err(e) = engine_for_task.start().await {
            error!(error = %e, "consensus engine stopped with an error");
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
        ("builder", builder_task),
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

    // TODO: apply the founder premine and the genesis validator set once the
    // genesis ceremony format is finalised. For now the treasury address is the
    // only allocation recorded at genesis.
    let txn = state_storage.begin_write()?;
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
    };
    dag_store.put_block(&genesis)?;
    info!(digest = %genesis.digest, "genesis block created");
    Ok(())
}

/// Run the P2P network service and process outbound broadcast commands.
///
/// The network task owns the [`NetworkService`] because
/// `NetworkService::start` takes `&mut self`. Broadcasts are serviced by
/// re-entering `start` after each command, which re-runs the (idempotent)
/// listen/dial preamble.
// TODO: replace with a shared network handle once `kvnc-network` exposes
// `start(&self)` or a dedicated broadcast handle, to avoid the re-listen churn.
async fn run_network(
    mut service: NetworkService,
    mut cmd_rx: mpsc::UnboundedReceiver<NetworkCommand>,
    mut shutdown: watch::Receiver<bool>,
) {
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
                Some(NetworkCommand::Shutdown) | None => {
                    info!("network task stopping");
                    break;
                }
            },
            res = service.start() => {
                match res {
                    Ok(()) => info!("network service stopped"),
                    Err(e) => error!(error = %e, "network service error"),
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
}

/// Ingest events emitted by the network layer into the DAG store and mempool.
async fn run_event_handler(
    mut events: mpsc::UnboundedReceiver<NetworkEvent>,
    mempool: Arc<Mempool>,
    engine: Arc<ConsensusEngine<NodeDagStore, NodeBlockManager>>,
    network_cmd_tx: mpsc::UnboundedSender<NetworkCommand>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            maybe = events.recv() => match maybe {
                Some(event) => handle_network_event(
                    event,
                    &mempool,
                    &engine,
                    &network_cmd_tx,
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
    network_cmd_tx: &mpsc::UnboundedSender<NetworkCommand>,
) {
    match event {
        NetworkEvent::BlockReceived(block) => match engine.process_block(&block) {
            Ok(()) => debug!(digest = %block.digest, "validated received block into consensus"),
            Err(e) => {
                warn!(digest = %block.digest, error = %e, "rejected received consensus block")
            }
        },
        NetworkEvent::TransactionReceived(tx) => match mempool.add_transaction(tx.clone()) {
            Ok(()) => {
                // Re-gossip locally-accepted transactions.
                let _ = network_cmd_tx.send(NetworkCommand::BroadcastTransaction(tx));
            }
            Err(MempoolError::AlreadyExists) => {
                debug!(hash = %tx.hash, "transaction already in mempool");
            }
            Err(e) => warn!(error = %e, "rejected received transaction"),
        },
        NetworkEvent::PeerConnected(peer) => info!(%peer, "peer connected"),
        NetworkEvent::PeerDisconnected(peer) => info!(%peer, "peer disconnected"),
        NetworkEvent::PeerDiscovered(peer, addr) => debug!(%peer, %addr, "peer discovered"),
        NetworkEvent::SyncRequest {
            peer,
            from_round,
            to_round,
        } => debug!(%peer, from_round, to_round, "sync request received"),
    }
}

/// Periodically drain the mempool into a signed block and broadcast it.
///
/// Blocks are only produced when the mempool has transactions: the consensus
/// engine already proposes (empty) blocks for each round it leads, so the
/// builder deliberately stays out of the way when there is nothing to include.
async fn run_block_builder(
    round_duration_ms: u64,
    dag_store: Arc<DagStore>,
    mempool: Arc<Mempool>,
    network_cmd_tx: mpsc::UnboundedSender<NetworkCommand>,
    signing_key: SigningKey,
    mut shutdown: watch::Receiver<bool>,
) {
    let author: AuthorityIndex = 0;
    let mut round: Round = 1;
    let mut interval = tokio::time::interval(Duration::from_millis(round_duration_ms.max(1)));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let transactions = mempool.get_next_transactions(MAX_TXS_PER_BLOCK);
                if transactions.is_empty() {
                    continue;
                }

                // TODO: derive the round from the consensus tip instead of a
                // local counter once the engine exposes its current round.
                let parents = dag_store.find_parents(round, 3).unwrap_or_default();
                let digest = StatementBlock::compute_digest(author, round, &parents, &transactions);
                let signature = kvnc_crypto::sign(&signing_key, digest.as_ref());
                let block = StatementBlock {
                    author,
                    round,
                    parents,
                    transactions,
                    statements: Vec::new(),
                    signature,
                    digest,
                };

                match dag_store.put_block(&block) {
                    Ok(()) => {
                        info!(round, digest = %block.digest, "produced block");
                        let _ = network_cmd_tx.send(NetworkCommand::BroadcastBlock(block));
                    }
                    Err(e) => warn!(error = %e, "failed to store produced block"),
                }
                round += 1;
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }
}

/// Execute committed sub-DAGs as they are produced by consensus.
async fn run_execution(
    mut subdags: mpsc::UnboundedReceiver<kvnc_consensus::CommittedSubDag>,
    state_storage: Arc<Storage>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut ctx = ExecutionContext::new();
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
                    Ok(result) => info!(
                        round = subdag.leader_round,
                        txs = result.txs_applied,
                        "executed committed sub-DAG"
                    ),
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
}

/// Adapter implementing [`BlockManagerTrait`] over the concrete [`BlockManager`].
struct NodeBlockManager {
    inner: Arc<BlockManager>,
}

impl BlockManagerTrait for NodeBlockManager {
    fn propose_block(&self, round: Round) -> Result<StatementBlock, BlockManagerError> {
        self.inner.propose_block(round)
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
fn recover_committed_subdags(dag_store: &DagStore) -> Result<Vec<kvnc_consensus::CommittedSubDag>> {
    let rounds = dag_store.get_decided_rounds(u64::MAX)?;
    let mut subdags = Vec::new();
    for round in rounds {
        for leader_hash in dag_store.get_decided_leaders(round)? {
            let leader = dag_store.get_block(&leader_hash)?;
            if leader.round != round || leader.digest != leader_hash {
                anyhow::bail!(
                    "persisted commit decision for round {round} references inconsistent leader block (block round {}, block digest {}, expected digest {leader_hash})",
                    leader.round,
                    leader.digest,
                );
            }
            let history = dag_store
                .get_ancestors(&leader_hash, 0)?
                .into_iter()
                .map(|hash| dag_store.get_block(&hash))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            subdags.push(kvnc_consensus::Linearizer::new().linearize(leader, history));
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

/// Build the initial single-validator committee from the local identity.
fn build_committee(
    public_key: &PublicKey,
    address: &Address,
    listen_addr: &str,
) -> Result<CommitteeInfo> {
    let authority = AuthorityInfo {
        index: 0,
        stake: MIN_VALIDATOR_STAKE,
        public_key: *public_key,
        address: *address,
        network_address: listen_addr.to_string(),
    };
    Ok(CommitteeInfo::try_new(0, vec![authority])?)
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
        };
        dag.put_block(&block).unwrap();
        dag.mark_round_decided(1, &digest).unwrap();

        let recovered = recover_committed_subdags(&dag).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].leader.digest, digest);
        assert_eq!(recovered[0].blocks.len(), 1);
    }

    #[test]
    fn committed_subdag_recovery_rejects_missing_decided_block() {
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
        };
        dag.put_block(&valid_block).unwrap();
        dag.mark_round_decided(1, &valid_digest).unwrap();

        let missing_digest = StatementBlock::compute_digest(0, 2, &[], &[]);
        dag.mark_round_decided(2, &missing_digest).unwrap();

        assert!(recover_committed_subdags(&dag).is_err());
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
        };
        dag.put_block(&block).unwrap();
        dag.mark_round_decided(1, &digest).unwrap();

        let error = recover_committed_subdags(&dag).unwrap_err();
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
        let error = recover_committed_subdags(&dag).unwrap_err().to_string();
        assert!(
            error.contains("block digest"),
            "recovery error should identify the mismatched block digest: {error}"
        );
        assert!(
            error.contains(&format!("expected digest {digest}")),
            "recovery error should identify the persisted decision digest: {error}"
        );
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
            .join_validator(validator_address, MIN_VALIDATOR_STAKE, 0, Some(payout))
            .unwrap();
        let txn = state_storage.begin_write().unwrap();
        state_storage
            .state()
            .save_staking_state(&txn, &staking)
            .unwrap();
        txn.commit().unwrap();

        let committee = build_committee(&public_key, &validator_address, "127.0.0.1:0")
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
        );
        let (exec_tx, exec_rx) = mpsc::unbounded_channel();
        engine.set_commit_sender(exec_tx);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let execution = tokio::spawn(run_execution(exec_rx, state_storage.clone(), shutdown_rx));

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
            .join_validator(validator_address, MIN_VALIDATOR_STAKE, 0, Some(payout))
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
        run_execution(exec_rx, state_storage.clone(), shutdown_rx).await;

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
}
