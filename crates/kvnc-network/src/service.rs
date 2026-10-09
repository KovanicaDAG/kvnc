//! [`NetworkService`]: swarm lifecycle, gossip handling and peer bookkeeping.

use crate::{
    behaviour::{self, Behaviour},
    block_sync::{BlockSyncRequest, BlockSyncResponse},
    error::NetworkError,
    sync::SyncRequest,
    topics, NetworkConfig, NetworkEvent,
};
use futures::StreamExt;
use kvnc_dag::DagStore;
use kvnc_mempool::{Mempool, MempoolError};
use kvnc_types::{StatementBlock, Transaction, Vote};
use libp2p::{
    gossipsub, identify, kad, request_response,
    swarm::{dial_opts::DialOpts, ConnectionId, NetworkBehaviour, SwarmEvent},
    Multiaddr, PeerId, Swarm,
};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, MutexGuard, PoisonError,
    },
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Event type produced by the composite [`Behaviour`].
type BehaviourEvent = <Behaviour as NetworkBehaviour>::ToSwarm;

/// Consecutive ping failures tolerated before a peer is disconnected, banned
/// and evicted from the Kademlia routing table.
const PING_FAILURE_LIMIT: u32 = 3;

/// Maximum consecutive invalid block failures before a peer is banned.
const INVALID_BLOCK_FAILURE_LIMIT: u32 = 5;

/// Keep bootstrap retries bounded and separated so stale seeds do not trigger
/// a burst of simultaneous dials.
const BOOTSTRAP_RETRY_BASE: Duration = Duration::from_secs(1);
const BOOTSTRAP_RETRY_MAX: Duration = Duration::from_secs(60);
const BOOTSTRAP_DIAL_STAGGER: Duration = Duration::from_millis(250);

/// Main network service.
pub struct NetworkService {
    /// Effective configuration.
    config: NetworkConfig,
    /// DAG store for serving block sync requests.
    dag_store: Arc<DagStore>,
    /// Mempool that ingests transactions received over gossip.
    mempool: Arc<Mempool>,
    /// Sender half of the event stream handed to the caller.
    event_tx: mpsc::UnboundedSender<NetworkEvent>,
    /// The swarm: polled by [`NetworkService::start`], published to by the
    /// broadcast methods.
    swarm: Mutex<Swarm<Behaviour>>,
    /// Peers we reported as connected; source of [`NetworkService::connected_peers`].
    connected: Mutex<HashSet<PeerId>>,
    /// Shared snapshot of the distinct connected-peer count for status surfaces.
    peer_count: Arc<AtomicUsize>,
    /// Consecutive ping failures per peer.
    ping_failures: Mutex<HashMap<PeerId, u32>>,
    /// Invalid block failures per peer (invalid sig, unknown author, bad parents, etc.)
    invalid_block_failures: Mutex<HashMap<PeerId, u32>>,
    /// Addresses learned for a peer (from Kademlia), used to evict banned peers
    /// from the routing table.
    peer_addresses: Mutex<HashMap<PeerId, Vec<Multiaddr>>>,
    /// Rate-limited block sync request timestamps per peer (audit 6.2).
    sync_requests: Mutex<HashMap<PeerId, Instant>>,
    /// Bootstrap transport attempts survive cancellation/re-entry of `start`.
    bootstrap: Mutex<BootstrapMaintenance>,
}

/// State for one distinct, configured bootstrap address.
struct BootstrapAddress {
    address: Multiaddr,
    retry_at: Instant,
    failures: u32,
    pending: Option<ConnectionId>,
    peer: Option<PeerId>,
    endpoint: Option<Multiaddr>,
}

/// Small deterministic state machine for bootstrap-only transport maintenance.
/// It deliberately knows nothing about discovered or otherwise connected peers.
struct BootstrapMaintenance {
    addresses: Vec<BootstrapAddress>,
    next_dial_at: Option<Instant>,
}

impl BootstrapMaintenance {
    fn new(addresses: &[Multiaddr], max_peers: usize, now: Instant) -> Self {
        let mut seen = HashSet::new();
        let addresses = addresses
            .iter()
            .filter(|address| seen.insert((*address).clone()))
            .take(max_peers)
            .map(|address| BootstrapAddress {
                address: address.clone(),
                retry_at: now,
                failures: 0,
                pending: None,
                peer: None,
                endpoint: None,
            })
            .collect();
        Self {
            addresses,
            next_dial_at: None,
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        let retry = self
            .addresses
            .iter()
            .filter(|state| state.pending.is_none() && state.peer.is_none())
            .map(|state| state.retry_at)
            .min()?;
        Some(
            self.next_dial_at
                .map_or(retry, |stagger| retry.max(stagger)),
        )
    }

    fn due_address(&mut self, now: Instant) -> Option<(usize, Multiaddr)> {
        if self.next_dial_at.is_some_and(|deadline| now < deadline) {
            return None;
        }
        let (index, state) = self.addresses.iter_mut().enumerate().find(|(_, state)| {
            state.pending.is_none() && state.peer.is_none() && state.retry_at <= now
        })?;
        self.next_dial_at = Some(now + BOOTSTRAP_DIAL_STAGGER);
        Some((index, state.address.clone()))
    }

    fn set_attempt(&mut self, index: usize, connection_id: ConnectionId) {
        if let Some(state) = self.addresses.get_mut(index) {
            state.pending = Some(connection_id);
        }
    }

    fn fail_attempt(&mut self, connection_id: ConnectionId, now: Instant) -> bool {
        let Some((index, state)) = self
            .addresses
            .iter_mut()
            .enumerate()
            .find(|(_, state)| state.pending == Some(connection_id))
        else {
            return false;
        };
        state.pending = None;
        Self::schedule_retry(index, state, now);
        true
    }

    fn establish_attempt(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        endpoint: Multiaddr,
    ) -> bool {
        let Some(state) = self
            .addresses
            .iter_mut()
            .find(|state| state.pending == Some(connection_id))
        else {
            return false;
        };
        state.pending = None;
        state.failures = 0;
        state.peer = Some(peer);
        state.endpoint = Some(endpoint);
        true
    }

    fn peer_disconnected(&mut self, peer: PeerId, num_established: u32, now: Instant) -> bool {
        if num_established != 0 {
            return false;
        }
        let mut matched = false;
        for (index, state) in self.addresses.iter_mut().enumerate() {
            if state.peer == Some(peer) {
                state.peer = None;
                state.endpoint = None;
                Self::schedule_retry(index, state, now);
                matched = true;
            }
        }
        matched
    }

    fn schedule_retry(index: usize, state: &mut BootstrapAddress, now: Instant) {
        state.failures = state.failures.saturating_add(1);
        let shift = state.failures.saturating_sub(1).min(31);
        let backoff = BOOTSTRAP_RETRY_BASE
            .checked_mul(1u32 << shift)
            .unwrap_or(BOOTSTRAP_RETRY_MAX)
            .min(BOOTSTRAP_RETRY_MAX);
        // Apply a stable per-address offset even when several failures happen
        // together, while keeping the total delay within the retry cap.
        let stagger = BOOTSTRAP_DIAL_STAGGER.saturating_mul((index as u32).min(240));
        state.retry_at = now + backoff.saturating_add(stagger).min(BOOTSTRAP_RETRY_MAX);
    }
}

/// Lock a mutex, recovering from poisoning: a panic while a lock is held must
/// not take the whole network layer down.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl NetworkService {
    /// Create a new network service and its event stream.
    ///
    /// The swarm is built immediately (no ports are bound until
    /// [`NetworkService::start`]), so broadcasts work as soon as the service
    /// exists.
    pub fn new(
        config: NetworkConfig,
        dag_store: Arc<DagStore>,
        mempool: Arc<Mempool>,
    ) -> Result<(Self, mpsc::UnboundedReceiver<NetworkEvent>), NetworkError> {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let swarm = behaviour::build_swarm(&config)?;
        let bootstrap =
            BootstrapMaintenance::new(&config.bootstrap_nodes, config.max_peers, Instant::now());
        info!(peer_id = %swarm.local_peer_id(), "network swarm created");

        Ok((
            Self {
                config,
                dag_store,
                mempool,
                event_tx,
                swarm: Mutex::new(swarm),
                connected: Mutex::new(HashSet::new()),
                peer_count: Arc::new(AtomicUsize::new(0)),
                ping_failures: Mutex::new(HashMap::new()),
                invalid_block_failures: Mutex::new(HashMap::new()),
                peer_addresses: Mutex::new(HashMap::new()),
                sync_requests: Mutex::new(HashMap::new()),
                bootstrap: Mutex::new(bootstrap),
            },
            event_rx,
        ))
    }

    /// Start the network service: listen on every configured address, dial the
    /// bootstrap nodes and then run the swarm event loop.
    ///
    /// The loop only returns on an unrecoverable (storage) error; transport and
    /// dial failures are logged and tolerated.
    ///
    /// Takes `&self`: the swarm and all bookkeeping use interior mutability, so
    /// the listen/dial preamble is executed exactly once per call. Callers must
    /// run this loop in its own task, not inside a `select!` that can cancel it
    /// and re-enter the preamble (which leaks listeners/file descriptors).
    pub async fn start(&self) -> Result<(), NetworkError> {
        for addr in &self.config.listen_addrs {
            match self.swarm().listen_on(addr.clone()) {
                Ok(_) => info!(%addr, "listening"),
                Err(err) => warn!(%addr, %err, "failed to listen on address"),
            }
        }

        self.dial_due_bootstrap();

        if let Err(err) = self.swarm().behaviour_mut().kad.bootstrap() {
            debug!(%err, "kad bootstrap deferred: no known peers yet");
        }

        info!(
            listen = self.config.listen_addrs.len(),
            bootstraps = self.config.bootstrap_nodes.len(),
            "network service started"
        );

        loop {
            let event = match self.next_bootstrap_deadline() {
                Some(deadline) => {
                    tokio::select! {
                        event = self.next_swarm_event() => event,
                        _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                            self.dial_due_bootstrap();
                            continue;
                        }
                    }
                }
                None => self.next_swarm_event().await,
            };
            let Some(event) = event else {
                return Err(NetworkError::Swarm(
                    "swarm event stream ended unexpectedly".to_string(),
                ));
            };
            self.handle_swarm_event(event)?;
        }
    }

    /// Broadcast a block to all peers.
    pub fn broadcast_block(&self, block: &StatementBlock) -> Result<(), NetworkError> {
        let payload = bincode::serialize(block)?;
        self.publish(topics::BLOCKS, payload)
    }

    /// Broadcast a transaction to all peers.
    pub fn broadcast_transaction(&self, tx: &Transaction) -> Result<(), NetworkError> {
        let payload = bincode::serialize(tx)?;
        self.publish(topics::TRANSACTIONS, payload)
    }

    /// Broadcast a vote to all peers.
    pub fn broadcast_vote(&self, vote: &Vote) -> Result<(), NetworkError> {
        let payload = bincode::serialize(vote)?;
        self.publish(topics::VOTES, payload)
    }

    /// Get connected peers.
    pub fn connected_peers(&self) -> HashSet<PeerId> {
        lock(&self.connected).clone()
    }

    /// Get peer count.
    pub fn peer_count(&self) -> usize {
        lock(&self.connected).len()
    }

    /// Get a cloneable snapshot handle for the distinct connected-peer count.
    ///
    /// The value is updated by the network event loop whenever its existing
    /// distinct-peer bookkeeping changes.
    pub fn peer_count_handle(&self) -> Arc<AtomicUsize> {
        self.peer_count.clone()
    }

    /// Lock the swarm.
    fn swarm(&self) -> MutexGuard<'_, Swarm<Behaviour>> {
        lock(&self.swarm)
    }

    /// Poll the swarm for its next event.
    ///
    /// The lock is taken only for the duration of a single `poll`: the swarm
    /// stores the task waker internally when it returns `Pending`, so the guard
    /// can be dropped right away and re-acquired on the next wake-up. Holding it
    /// across an await point would block [`NetworkService::publish`].
    async fn next_swarm_event(&self) -> Option<SwarmEvent<BehaviourEvent>> {
        std::future::poll_fn(|ctx| lock(&self.swarm).poll_next_unpin(ctx)).await
    }

    fn next_bootstrap_deadline(&self) -> Option<Instant> {
        lock(&self.bootstrap).next_deadline()
    }

    /// Start at most one due configured-bootstrap dial. The next attempt is
    /// staggered, and pending connection IDs survive cancellation of `start`.
    fn dial_due_bootstrap(&self) {
        let now = Instant::now();
        let Some((index, address)) = lock(&self.bootstrap).due_address(now) else {
            return;
        };
        let opts = DialOpts::unknown_peer_id().address(address.clone()).build();
        let connection_id = opts.connection_id();
        lock(&self.bootstrap).set_attempt(index, connection_id);
        match self.swarm().dial(opts) {
            Ok(()) => debug!(%address, ?connection_id, "dialing configured bootstrap"),
            Err(error) => {
                lock(&self.bootstrap).fail_attempt(connection_id, Instant::now());
                warn!(%address, %error, "failed to dial bootstrap node");
            }
        }
    }

    /// Publish a serialized payload on a gossipsub topic.
    fn publish(&self, topic: &str, payload: Vec<u8>) -> Result<(), NetworkError> {
        let mut swarm = self.swarm();
        match swarm
            .behaviour_mut()
            .gossipsub
            .publish(topics::ident(topic), payload)
        {
            Ok(_) => Ok(()),
            Err(gossipsub::PublishError::NoPeersSubscribedToTopic) => {
                // Nobody is subscribed yet (fresh node, no peers): the message
                // has nothing to propagate to, which is not an error for us.
                debug!(topic, "no peers to gossip to; broadcast skipped");
                Ok(())
            }
            Err(err) => Err(NetworkError::Gossipsub(err.to_string())),
        }
    }

    /// Respond to a block sync request.
    pub fn respond_block_sync(
        &self,
        channel: request_response::ResponseChannel<BlockSyncResponse>,
        response: BlockSyncResponse,
    ) -> Result<(), NetworkError> {
        let mut swarm = self.swarm();
        swarm
            .behaviour_mut()
            .block_sync
            .send_response(channel, response)
            .map_err(|e| NetworkError::Swarm(format!("failed to send block sync response: {e}")))
    }

    /// Send a block sync request to a peer.
    pub fn request_block_sync(
        &self,
        peer: PeerId,
        request: BlockSyncRequest,
    ) -> Result<request_response::OutboundRequestId, NetworkError> {
        let mut swarm = self.swarm();
        let request_id = swarm
            .behaviour_mut()
            .block_sync
            .send_request(&peer, request);
        Ok(request_id)
    }

    /// Process a block; when a parent is missing, enqueue a sync request.
    /// Requests are rate-limited to 1 per peer per 500 ms (audit 6.2).
    pub fn process_block(&self, block: &StatementBlock, peer: PeerId) -> Result<(), NetworkError> {
        // Check for missing parents via DAG store
        for parent in &block.parents {
            if !self.dag_store.has_block(&parent.digest).unwrap_or(false) {
                // Rate limit: allow at most 1 request per 500 ms per peer
                let now = Instant::now();
                let allowed = {
                    let mut reqs = lock(&self.sync_requests);
                    let last = reqs.get(&peer);
                    let ok =
                        last.is_none_or(|t| now.duration_since(*t) >= Duration::from_millis(500));
                    if ok {
                        reqs.insert(peer, now);
                    }
                    ok
                };
                if allowed {
                    let request = BlockSyncRequest::ByHash(parent.digest);
                    debug!(%peer, ?request, "missing parent -> enqueue sync request");
                    self.request_block_sync(peer, request)?;
                }
            }
        }
        Ok(())
    }

    /// Forward an event to the caller. A dropped receiver only means nobody is
    /// consuming events anymore, so this never fails the event loop.
    fn emit(&self, event: NetworkEvent) {
        if self.event_tx.send(event).is_err() {
            debug!("network event receiver dropped; event discarded");
        }
    }

    /// Turn a swarm event into [`NetworkEvent`]s.
    fn handle_swarm_event(&self, event: SwarmEvent<BehaviourEvent>) -> Result<(), NetworkError> {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                info!(%address, "listening on new address");
                Ok(())
            }
            SwarmEvent::ConnectionEstablished {
                peer_id,
                connection_id,
                endpoint,
                ..
            } => {
                lock(&self.bootstrap).establish_attempt(
                    connection_id,
                    peer_id,
                    endpoint.get_remote_address().clone(),
                );
                self.on_peer_connected(peer_id)
            }
            SwarmEvent::ConnectionClosed {
                peer_id,
                num_established,
                ..
            } => {
                if num_established == 0 {
                    if lock(&self.bootstrap).peer_disconnected(
                        peer_id,
                        num_established,
                        Instant::now(),
                    ) {
                        debug!(%peer_id, "configured bootstrap disconnected; retry scheduled");
                    }
                    self.on_peer_disconnected(peer_id);
                }
                Ok(())
            }
            SwarmEvent::OutgoingConnectionError {
                peer_id,
                connection_id,
                error,
            } => {
                if lock(&self.bootstrap).fail_attempt(connection_id, Instant::now()) {
                    debug!(?peer_id, ?connection_id, %error, "bootstrap dial failed; retry scheduled");
                } else {
                    debug!(?peer_id, ?connection_id, %error, "outgoing connection failed");
                }
                Ok(())
            }
            SwarmEvent::IncomingConnectionError { error, .. } => {
                debug!(%error, "incoming connection failed");
                Ok(())
            }
            SwarmEvent::ListenerError { error, .. } => {
                warn!(%error, "listener error");
                Ok(())
            }
            SwarmEvent::Behaviour(event) => self.handle_behaviour_event(event),
            _ => Ok(()),
        }
    }

    /// Dispatch a behaviour-level event to the matching protocol handler.
    fn handle_behaviour_event(&self, event: BehaviourEvent) -> Result<(), NetworkError> {
        match event {
            BehaviourEvent::Gossipsub(event) => self.handle_gossipsub_event(event),
            BehaviourEvent::Kad(event) => self.handle_kad_event(event),
            BehaviourEvent::Ping(event) => self.handle_ping_event(event),
            BehaviourEvent::Identify(event) => self.handle_identify_event(event),
            BehaviourEvent::BlockSync(event) => self.handle_request_response_event(event),
        }
    }

    /// Handle gossipsub traffic: route payloads by topic.
    fn handle_gossipsub_event(&self, event: gossipsub::Event) -> Result<(), NetworkError> {
        match event {
            gossipsub::Event::Message { message, .. } => match message.topic.as_str() {
                topics::BLOCKS => self.on_block_message(&message.data),
                topics::TRANSACTIONS => self.on_transaction_message(&message.data),
                topics::SYNC => self.on_sync_message(message.source, &message.data),
                topics::VOTES => self.on_vote_message(message.source, &message.data),
                other => {
                    debug!(topic = other, "message on unexpected topic");
                    Ok(())
                }
            },
            gossipsub::Event::Subscribed { peer_id, topic } => {
                debug!(%peer_id, topic = topic.as_str(), "peer subscribed");
                Ok(())
            }
            gossipsub::Event::Unsubscribed { peer_id, topic } => {
                debug!(%peer_id, topic = topic.as_str(), "peer unsubscribed");
                Ok(())
            }
            gossipsub::Event::GossipsubNotSupported { peer_id } => {
                warn!(%peer_id, "peer does not support gossipsub");
                Ok(())
            }
            // libp2p-gossipsub >= 0.48 reports peers that lag behind on message
            // delivery. We have no backpressure hook for this yet, so just log.
            gossipsub::Event::SlowPeer { peer_id, .. } => {
                debug!(%peer_id, "gossipsub slow peer");
                Ok(())
            }
        }
    }

    /// Handle Kademlia events: bootstrap progress and peer discovery.
    fn handle_kad_event(&self, event: kad::Event) -> Result<(), NetworkError> {
        match event {
            kad::Event::OutboundQueryProgressed {
                result: kad::QueryResult::Bootstrap(result),
                ..
            } => {
                match result {
                    Ok(ok) => debug!(?ok, "kad bootstrap finished"),
                    Err(err) => warn!(%err, "kad bootstrap failed"),
                }
                Ok(())
            }
            kad::Event::RoutingUpdated {
                peer,
                is_new_peer: true,
                addresses,
                ..
            } => {
                let learned = addresses.into_vec();
                let Some(address) = learned.first().cloned() else {
                    return Ok(());
                };
                lock(&self.peer_addresses).insert(peer, learned);
                debug!(%peer, %address, "peer discovered via kad");
                self.emit(NetworkEvent::PeerDiscovered(peer, address));
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Handle ping results: reset the failure counter on success, ban the peer
    /// once the failure limit is reached.
    fn handle_ping_event(&self, event: libp2p::ping::Event) -> Result<(), NetworkError> {
        match event.result {
            Ok(_) => {
                lock(&self.ping_failures).remove(&event.peer);
                Ok(())
            }
            Err(failure) => {
                let failures = {
                    let mut failures = lock(&self.ping_failures);
                    let counter = failures.entry(event.peer).or_insert(0);
                    *counter += 1;
                    *counter
                };
                if failures >= PING_FAILURE_LIMIT {
                    warn!(peer = %event.peer, failures, ?failure, "unreachable peer is being banned");
                    self.ban_peer(event.peer);
                } else {
                    debug!(peer = %event.peer, failures, ?failure, "ping failed");
                }
                Ok(())
            }
        }
    }

    /// Handle identify: learn the peer's listen addresses for Kademlia.
    fn handle_identify_event(&self, event: identify::Event) -> Result<(), NetworkError> {
        if let identify::Event::Received { peer_id, info, .. } = event {
            if !info.listen_addrs.is_empty() {
                let mut swarm = self.swarm();
                for address in info.listen_addrs {
                    swarm.behaviour_mut().kad.add_address(&peer_id, address);
                }
            }
        }
        Ok(())
    }

    /// Handle request-response events (block sync).
    fn handle_request_response_event(
        &self,
        event: request_response::Event<BlockSyncRequest, BlockSyncResponse>,
    ) -> Result<(), NetworkError> {
        match event {
            request_response::Event::Message {
                peer,
                message,
                connection_id: _,
            } => match message {
                request_response::Message::Request {
                    request_id: _,
                    request,
                    channel,
                } => {
                    debug!(%peer, ?request, "block sync request received");
                    // Automatically respond using the DAG store
                    let response = match request {
                        BlockSyncRequest::ByHash(hash) => match self.dag_store.get_block(&hash) {
                            Ok(block) => BlockSyncResponse::Block(block),
                            Err(_) => BlockSyncResponse::NotFound,
                        },
                        BlockSyncRequest::ByAuthorRound { author, round } => {
                            match self.dag_store.get_block_by_author_round(author, round) {
                                Ok(Some(block)) => BlockSyncResponse::Block(block),
                                Ok(None) => BlockSyncResponse::NotFound,
                                Err(_) => BlockSyncResponse::InvalidRequest,
                            }
                        }
                    };
                    let mut swarm = self.swarm();
                    if let Err(e) = swarm
                        .behaviour_mut()
                        .block_sync
                        .send_response(channel, response)
                    {
                        warn!(%peer, %e, "failed to send block sync response");
                    }
                }
                request_response::Message::Response {
                    request_id,
                    response,
                } => {
                    debug!(%peer, ?request_id, ?response, "block sync response received");
                    self.emit(NetworkEvent::BlockSyncResponse {
                        peer,
                        request_id,
                        response,
                    });
                }
            },
            request_response::Event::OutboundFailure {
                peer,
                request_id,
                error,
                connection_id: _,
            } => {
                warn!(%peer, ?request_id, %error, "block sync request failed");
                self.emit(NetworkEvent::BlockSyncResponse {
                    peer,
                    request_id,
                    response: BlockSyncResponse::InvalidRequest,
                });
            }
            request_response::Event::InboundFailure {
                peer,
                request_id,
                error,
                connection_id: _,
            } => {
                warn!(%peer, ?request_id, %error, "incoming block sync request failed");
            }
            request_response::Event::ResponseSent { .. } => {
                debug!("block sync response sent");
            }
        }
        Ok(())
    }

    /// Decode a block received over gossip and announce it to the caller.
    /// Validation and persistence belong to the BlockManager, not the network
    /// ingress path.
    fn on_block_message(&self, payload: &[u8]) -> Result<(), NetworkError> {
        let block: StatementBlock = match bincode::deserialize(payload) {
            Ok(block) => block,
            Err(err) => {
                warn!(%err, "dropping malformed block gossip");
                return Ok(());
            }
        };
        // Signature validation and persistence belong to the BlockManager / node
        // hot path, not the network ingress. Announcing the decoded block keeps a
        // single validation authority (see `on_block_message` doc comment).
        let (round, digest) = (block.round, block.digest);
        info!(round, %digest, "block received over gossip");
        self.emit(NetworkEvent::BlockReceived(block));
        Ok(())
    }

    /// Ingest a transaction received over gossip and announce it to the caller.
    fn on_transaction_message(&self, payload: &[u8]) -> Result<(), NetworkError> {
        let tx: Transaction = match bincode::deserialize(payload) {
            Ok(tx) => tx,
            Err(err) => {
                warn!(%err, "dropping malformed transaction gossip");
                return Ok(());
            }
        };
        let hash = tx.hash;
        match self.mempool.add_transaction(tx.clone()) {
            Ok(()) => {
                info!(%hash, "accepted transaction received over gossip");
                self.emit(NetworkEvent::TransactionReceived(tx));
                Ok(())
            }
            Err(MempoolError::AlreadyExists) => {
                debug!(%hash, "transaction already in the mempool");
                Ok(())
            }
            Err(err) => {
                // Peer-supplied data we reject is not our problem.
                warn!(%hash, %err, "rejected transaction received over gossip");
                Ok(())
            }
        }
    }

    /// Decode a vote received over gossip and announce it to the caller.
    fn on_vote_message(&self, source: Option<PeerId>, payload: &[u8]) -> Result<(), NetworkError> {
        let vote: Vote = match bincode::deserialize(payload) {
            Ok(vote) => vote,
            Err(err) => {
                warn!(%err, "dropping malformed vote gossip");
                return Ok(());
            }
        };
        let Some(peer) = source else {
            warn!("dropping vote message without an author");
            return Ok(());
        };
        debug!(%peer, leader_round = vote.leader_round, %vote.leader_hash, "vote received over gossip");
        self.emit(NetworkEvent::VoteReceived { peer, vote });
        Ok(())
    }

    /// Decode a sync-range request and announce it to the caller.
    fn on_sync_message(&self, source: Option<PeerId>, payload: &[u8]) -> Result<(), NetworkError> {
        let request = match SyncRequest::decode(payload) {
            Ok(request) => request,
            Err(err) => {
                warn!(%err, "dropping malformed sync request");
                return Ok(());
            }
        };
        let Some(peer) = source else {
            warn!("dropping sync request without an author");
            return Ok(());
        };
        if !request.is_valid() {
            warn!(
                %peer,
                from = request.from_round,
                to = request.to_round,
                "dropping inverted sync request"
            );
            return Ok(());
        }
        debug!(
            %peer,
            from = request.from_round,
            to = request.to_round,
            "sync request received"
        );
        self.emit(NetworkEvent::SyncRequest {
            peer,
            from_round: request.from_round,
            to_round: request.to_round,
        });
        Ok(())
    }

    /// Record a new peer, enforcing [`NetworkConfig::max_peers`].
    fn on_peer_connected(&self, peer: PeerId) -> Result<(), NetworkError> {
        let mut connected = lock(&self.connected);
        if connected.contains(&peer) {
            return Ok(());
        }
        if connected.len() >= self.config.max_peers {
            drop(connected);
            warn!(
                %peer,
                max_peers = self.config.max_peers,
                "rejecting peer: peer limit reached"
            );
            let _ = self.swarm().disconnect_peer_id(peer);
            return Ok(());
        }
        connected.insert(peer);
        self.peer_count.store(connected.len(), Ordering::Relaxed);
        drop(connected);
        info!(%peer, "peer connected");
        self.emit(NetworkEvent::PeerConnected(peer));
        Ok(())
    }

    /// Record a disconnected peer (and only report it once).
    fn on_peer_disconnected(&self, peer: PeerId) {
        let removed = {
            let mut connected = lock(&self.connected);
            let removed = connected.remove(&peer);
            if removed {
                self.peer_count.store(connected.len(), Ordering::Relaxed);
            }
            removed
        };
        if removed {
            lock(&self.ping_failures).remove(&peer);
            lock(&self.peer_addresses).remove(&peer);
            info!(%peer, "peer disconnected");
            self.emit(NetworkEvent::PeerDisconnected(peer));
        }
    }

    /// Disconnect a peer and evict it from the Kademlia routing table so the
    /// swarm stops redialing it.
    fn ban_peer(&self, peer: PeerId) {
        lock(&self.ping_failures).remove(&peer);
        lock(&self.invalid_block_failures).remove(&peer);
        let known: Vec<Multiaddr> = lock(&self.peer_addresses).remove(&peer).unwrap_or_default();

        let mut swarm = self.swarm();
        if swarm.disconnect_peer_id(peer).is_err() {
            debug!(%peer, "peer was already disconnected");
        }
        let kad = &mut swarm.behaviour_mut().kad;
        if known.is_empty() {
            // Nothing learned about the peer: evict it outright.
            kad.remove_peer(&peer);
        } else {
            // Removing the last known address evicts the peer from the table.
            for address in &known {
                kad.remove_address(&peer, address);
            }
        }
        info!(%peer, "banned peer");
    }

    /// Record an invalid block failure from a peer. If the failure limit is
    /// reached, ban the peer.
    pub fn record_invalid_block_failure(&self, peer: PeerId, reason: &str) {
        let failures = {
            let mut failures = lock(&self.invalid_block_failures);
            let counter = failures.entry(peer).or_insert(0);
            *counter += 1;
            *counter
        };
        if failures >= INVALID_BLOCK_FAILURE_LIMIT {
            warn!(peer = %peer, failures, reason, "peer exceeded invalid block limit; banning");
            self.ban_peer(peer);
        } else {
            debug!(peer = %peer, failures, reason, "invalid block failure recorded");
        }
    }

    /// Reset invalid block failure counter for a peer (e.g., on successful validation).
    pub fn reset_invalid_block_failures(&self, peer: PeerId) {
        lock(&self.invalid_block_failures).remove(&peer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvnc_mempool::MempoolConfig;
    use kvnc_storage::Storage;
    use kvnc_types::{Hash, Signature};

    /// Build a service over throwaway databases. No ports are bound.
    fn test_service() -> (tempfile::TempDir, NetworkService) {
        let dir = tempfile::tempdir().expect("temp dir");
        let dag_storage = Storage::new(dir.path().join("dag.db")).expect("dag storage");
        let mempool_storage =
            Arc::new(Storage::new(dir.path().join("mempool.db")).expect("mempool storage"));
        let dag_store = Arc::new(DagStore::new(dag_storage).expect("dag store"));
        let mempool = Arc::new(Mempool::new(MempoolConfig::default(), mempool_storage));
        let (service, _events) =
            NetworkService::new(NetworkConfig::default(), dag_store, mempool).expect("service");
        (dir, service)
    }

    fn sample_block() -> StatementBlock {
        StatementBlock {
            author: 0,
            round: 1,
            parents: Vec::new(),
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: Signature([0u8; 64]),
            digest: Hash::zero(),
            merkle_root: Default::default(),
        }
    }

    fn address(text: &str) -> Multiaddr {
        text.parse().expect("valid test multiaddr")
    }

    fn test_id(id: usize) -> ConnectionId {
        ConnectionId::new_unchecked(id)
    }

    #[test]
    fn bootstrap_addresses_are_deduplicated_and_capped() {
        let now = Instant::now();
        let first = address("/ip4/127.0.0.1/tcp/19001");
        let second = address("/ip4/127.0.0.1/tcp/19002");
        let third = address("/ip4/127.0.0.1/tcp/19003");
        let maintenance = BootstrapMaintenance::new(
            &[first.clone(), first.clone(), second.clone(), third],
            2,
            now,
        );

        assert_eq!(maintenance.addresses.len(), 2);
        assert_eq!(maintenance.addresses[0].address, first);
        assert_eq!(maintenance.addresses[1].address, second);
    }

    #[test]
    fn bootstrap_retry_backoff_is_exponential_and_capped() {
        let now = Instant::now();
        let mut maintenance =
            BootstrapMaintenance::new(&[address("/ip4/127.0.0.1/tcp/19001")], 1, now);

        let (index, _) = maintenance.due_address(now).expect("initial dial due");
        let first_id = test_id(1);
        maintenance.set_attempt(index, first_id);
        assert!(maintenance.fail_attempt(first_id, now));
        assert_eq!(
            maintenance.addresses[0].retry_at,
            now + Duration::from_secs(1)
        );

        let second_due = now + Duration::from_secs(1);
        let (index, _) = maintenance
            .due_address(second_due)
            .expect("second dial due");
        let second_id = test_id(2);
        maintenance.set_attempt(index, second_id);
        assert!(maintenance.fail_attempt(second_id, second_due));
        assert_eq!(
            maintenance.addresses[0].retry_at,
            second_due + Duration::from_secs(2)
        );

        let mut retry_at = maintenance.addresses[0].retry_at;
        let mut latest_attempt_at = None;
        for id in 3..16 {
            latest_attempt_at = Some(retry_at);
            let (index, _) = maintenance.due_address(retry_at).expect("retry due");
            let connection_id = test_id(id);
            maintenance.set_attempt(index, connection_id);
            assert!(maintenance.fail_attempt(connection_id, retry_at));
            retry_at = maintenance.addresses[0].retry_at;
        }
        assert_eq!(
            retry_at - latest_attempt_at.expect("at least one capped retry"),
            BOOTSTRAP_RETRY_MAX
        );
    }

    #[test]
    fn pending_bootstrap_attempt_survives_start_reentry_and_old_events_are_exact() {
        let now = Instant::now();
        let mut maintenance =
            BootstrapMaintenance::new(&[address("/ip4/127.0.0.1/tcp/19001")], 1, now);
        let (index, _) = maintenance.due_address(now).expect("initial dial due");
        let older_id = test_id(41);
        maintenance.set_attempt(index, older_id);

        // Re-entering `start` sees the same pending state and does not create a
        // replacement dial just because the start future was canceled.
        assert!(maintenance
            .due_address(now + Duration::from_secs(10))
            .is_none());
        assert_eq!(maintenance.addresses[0].pending, Some(older_id));

        assert!(maintenance.fail_attempt(older_id, now));
        let retry_at = maintenance.addresses[0].retry_at;
        let (index, _) = maintenance.due_address(retry_at).expect("retry due");
        let newer_id = test_id(42);
        maintenance.set_attempt(index, newer_id);
        assert!(!maintenance.fail_attempt(older_id, retry_at + Duration::from_secs(1)));
        assert_eq!(maintenance.addresses[0].pending, Some(newer_id));
    }

    #[test]
    fn bootstrap_disconnect_retries_only_associated_peer_after_last_connection() {
        let now = Instant::now();
        let mut maintenance =
            BootstrapMaintenance::new(&[address("/ip4/127.0.0.1/tcp/19001")], 1, now);
        let (index, _) = maintenance.due_address(now).expect("initial dial due");
        let connection_id = test_id(51);
        let peer = PeerId::random();
        maintenance.set_attempt(index, connection_id);
        assert!(maintenance.establish_attempt(
            connection_id,
            peer,
            address("/ip4/127.0.0.1/tcp/19001")
        ));

        assert!(!maintenance.peer_disconnected(peer, 1, now));
        assert_eq!(maintenance.addresses[0].peer, Some(peer));
        assert!(!maintenance.peer_disconnected(PeerId::random(), 0, now));
        assert!(maintenance.peer_disconnected(peer, 0, now));
        assert_eq!(maintenance.addresses[0].peer, None);
        assert_eq!(
            maintenance.addresses[0].retry_at,
            now + BOOTSTRAP_RETRY_BASE
        );
    }

    #[tokio::test]
    async fn new_service_starts_with_no_peers() {
        let (_dir, service) = test_service();
        assert_eq!(service.peer_count(), 0);
        assert!(service.connected_peers().is_empty());
    }

    #[tokio::test]
    async fn broadcast_without_peers_is_not_an_error() {
        let (_dir, service) = test_service();
        service
            .broadcast_block(&sample_block())
            .expect("broadcast with no peers");
        assert_eq!(service.peer_count(), 0);
    }

    #[tokio::test]
    async fn gossip_message_ingestion_is_reversible_on_bad_data() {
        let (_dir, service) = test_service();
        // Malformed payloads are dropped, not propagated.
        service
            .on_block_message(&[0xff, 0xff])
            .expect("malformed block is tolerated");
        service
            .on_transaction_message(&[0xff, 0xff])
            .expect("malformed transaction is tolerated");
        service
            .on_sync_message(None, &[0xff, 0xff])
            .expect("anonymous malformed sync request is tolerated");
        service
            .on_sync_message(Some(PeerId::random()), &[0xff, 0xff])
            .expect("malformed sync request is tolerated");
    }

    #[tokio::test]
    async fn block_gossip_emits_for_validation_without_pre_storing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let dag_storage = Storage::new(dir.path().join("dag.db")).expect("dag storage");
        let dag_store = Arc::new(DagStore::new(dag_storage).expect("dag store"));
        let mempool_storage =
            Arc::new(Storage::new(dir.path().join("mempool.db")).expect("mempool storage"));
        let mempool = Arc::new(Mempool::new(MempoolConfig::default(), mempool_storage));
        let (service, mut events) =
            NetworkService::new(NetworkConfig::default(), dag_store.clone(), mempool)
                .expect("service");

        // This payload is structurally decodable but has an invalid digest and
        // signature. The event remains available for the caller to reject.
        let block = sample_block();
        let payload = bincode::serialize(&block).expect("serialize block");
        service
            .on_block_message(&payload)
            .expect("decoded gossip is announced");

        assert!(!dag_store.has_block(&block.digest).expect("query store"));
        match events.try_recv().expect("block event emitted") {
            NetworkEvent::BlockReceived(received) => {
                assert_eq!(received.digest, block.digest);
                assert_eq!(received.signature, block.signature);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn valid_sync_request_reaches_the_caller() {
        let dir = tempfile::tempdir().expect("temp dir");
        let dag_storage = Storage::new(dir.path().join("dag.db")).expect("dag storage");
        let mempool_storage =
            Arc::new(Storage::new(dir.path().join("mempool.db")).expect("mempool storage"));
        let dag_store = Arc::new(DagStore::new(dag_storage).expect("dag store"));
        let mempool = Arc::new(Mempool::new(MempoolConfig::default(), mempool_storage));
        let (service, mut events) =
            NetworkService::new(NetworkConfig::default(), dag_store, mempool).expect("service");

        let peer = PeerId::random();
        let payload = SyncRequest::new(4, 8).encode().expect("encodes");
        service
            .on_sync_message(Some(peer), &payload)
            .expect("valid request is accepted");

        match events.try_recv().expect("event emitted") {
            NetworkEvent::SyncRequest {
                peer: got,
                from_round,
                to_round,
            } => {
                assert_eq!(got, peer);
                assert_eq!(from_round, 4);
                assert_eq!(to_round, 8);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn peer_bookkeeping_enforces_max_peers() {
        let (_dir, service) = test_service();
        let shared_peer_count = service.peer_count_handle();
        let first = PeerId::random();

        service.on_peer_connected(first).expect("connect");
        assert_eq!(service.peer_count(), 1);
        assert_eq!(shared_peer_count.load(Ordering::Relaxed), 1);
        // Duplicate connection events do not double count.
        service.on_peer_connected(first).expect("reconnect");
        assert_eq!(service.peer_count(), 1);
        assert_eq!(shared_peer_count.load(Ordering::Relaxed), 1);

        service.on_peer_disconnected(first);
        assert_eq!(service.peer_count(), 0);
        assert_eq!(shared_peer_count.load(Ordering::Relaxed), 0);

        // Fill up to the configured limit, then the next peer must be rejected.
        let mut peers = Vec::new();
        for _ in 0..service.config.max_peers {
            let peer = PeerId::random();
            peers.push(peer);
            service.on_peer_connected(peer).expect("connect");
        }
        assert_eq!(service.peer_count(), service.config.max_peers);

        let overflow = PeerId::random();
        service.on_peer_connected(overflow).expect("reject");
        assert!(!service.connected_peers().contains(&overflow));
        assert_eq!(service.peer_count(), service.config.max_peers);

        for peer in peers {
            service.on_peer_disconnected(peer);
        }
        assert_eq!(service.peer_count(), 0);
    }

    #[tokio::test]
    async fn repeated_ping_failures_ban_a_peer() {
        let (_dir, service) = test_service();
        let peer = PeerId::random();

        for attempt in 0..PING_FAILURE_LIMIT {
            service
                .handle_ping_event(libp2p::ping::Event {
                    peer,
                    connection: libp2p::swarm::ConnectionId::new_unchecked(attempt as usize),
                    result: Err(libp2p::ping::Failure::Timeout),
                })
                .expect("ping failure handled");
        }

        // Once banned, both the failure counter and the address snapshot are gone.
        assert!(lock(&service.ping_failures).get(&peer).is_none());
        assert!(lock(&service.peer_addresses).get(&peer).is_none());

        // A later successful ping starts counting from scratch.
        service
            .handle_ping_event(libp2p::ping::Event {
                peer,
                connection: libp2p::swarm::ConnectionId::new_unchecked(0),
                result: Ok(std::time::Duration::from_millis(5)),
            })
            .expect("ping success handled");
        assert!(lock(&service.ping_failures).get(&peer).is_none());
    }
}
