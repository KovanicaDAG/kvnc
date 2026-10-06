//! [`NetworkService`]: swarm lifecycle, gossip handling and peer bookkeeping.

use crate::{
    behaviour::{self, Behaviour},
    error::NetworkError,
    sync::SyncRequest,
    topics, NetworkConfig, NetworkEvent,
};
use futures::StreamExt;
use kvnc_dag::DagStore;
use kvnc_mempool::{Mempool, MempoolError};
use kvnc_types::{StatementBlock, Transaction};
use libp2p::{
    gossipsub, identify, kad,
    swarm::{NetworkBehaviour, SwarmEvent},
    Multiaddr, PeerId, Swarm,
};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Event type produced by the composite [`Behaviour`].
type BehaviourEvent = <Behaviour as NetworkBehaviour>::ToSwarm;

/// Consecutive ping failures tolerated before a peer is disconnected, banned
/// and evicted from the Kademlia routing table.
const PING_FAILURE_LIMIT: u32 = 3;

/// Main network service.
pub struct NetworkService {
    /// Effective configuration.
    config: NetworkConfig,
    /// Store that ingests blocks received over gossip.
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
    /// Consecutive ping failures per peer.
    ping_failures: Mutex<HashMap<PeerId, u32>>,
    /// Addresses learned for a peer (from Kademlia), used to evict banned peers
    /// from the routing table.
    peer_addresses: Mutex<HashMap<PeerId, Vec<Multiaddr>>>,
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
        info!(peer_id = %swarm.local_peer_id(), "network swarm created");

        Ok((
            Self {
                config,
                dag_store,
                mempool,
                event_tx,
                swarm: Mutex::new(swarm),
                connected: Mutex::new(HashSet::new()),
                ping_failures: Mutex::new(HashMap::new()),
                peer_addresses: Mutex::new(HashMap::new()),
            },
            event_rx,
        ))
    }

    /// Start the network service: listen on every configured address, dial the
    /// bootstrap nodes and then run the swarm event loop.
    ///
    /// The loop only returns on an unrecoverable (storage) error; transport and
    /// dial failures are logged and tolerated.
    pub async fn start(&mut self) -> Result<(), NetworkError> {
        for addr in &self.config.listen_addrs {
            match self.swarm().listen_on(addr.clone()) {
                Ok(_) => info!(%addr, "listening"),
                Err(err) => warn!(%addr, %err, "failed to listen on address"),
            }
        }

        for addr in &self.config.bootstrap_nodes {
            if let Err(err) = self.swarm().dial(addr.clone()) {
                warn!(%addr, %err, "failed to dial bootstrap node");
            }
        }

        if let Err(err) = self.swarm().behaviour_mut().kad.bootstrap() {
            debug!(%err, "kad bootstrap deferred: no known peers yet");
        }

        info!(
            listen = self.config.listen_addrs.len(),
            bootstraps = self.config.bootstrap_nodes.len(),
            "network service started"
        );

        loop {
            let Some(event) = self.next_swarm_event().await else {
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

    /// Get connected peers.
    pub fn connected_peers(&self) -> HashSet<PeerId> {
        lock(&self.connected).clone()
    }

    /// Get peer count.
    pub fn peer_count(&self) -> usize {
        lock(&self.connected).len()
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

    /// Forward an event to the caller. A dropped receiver only means nobody is
    /// consuming events anymore, so this never fails the event loop.
    fn emit(&self, event: NetworkEvent) {
        if self.event_tx.send(event).is_err() {
            debug!("network event receiver dropped; event discarded");
        }
    }

    /// Turn a swarm event into [`NetworkEvent`]s and store writes.
    fn handle_swarm_event(&self, event: SwarmEvent<BehaviourEvent>) -> Result<(), NetworkError> {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                info!(%address, "listening on new address");
                Ok(())
            }
            SwarmEvent::ConnectionEstablished { peer_id, .. } => self.on_peer_connected(peer_id),
            SwarmEvent::ConnectionClosed {
                peer_id,
                num_established,
                ..
            } => {
                if num_established == 0 {
                    self.on_peer_disconnected(peer_id);
                }
                Ok(())
            }
            SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                // Dial failures are tolerated: bootstrap addresses go stale and
                // Kademlia retries through other routing table entries.
                debug!(?peer_id, %error, "outgoing connection failed");
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
        }
    }

    /// Handle gossipsub traffic: route payloads by topic.
    fn handle_gossipsub_event(&self, event: gossipsub::Event) -> Result<(), NetworkError> {
        match event {
            gossipsub::Event::Message { message, .. } => match message.topic.as_str() {
                topics::BLOCKS => self.on_block_message(&message.data),
                topics::TRANSACTIONS => self.on_transaction_message(&message.data),
                topics::SYNC => self.on_sync_message(message.source, &message.data),
                topics::VOTES => {
                    debug!("ignoring vote message: tallying is not wired up yet");
                    Ok(())
                }
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

    /// Ingest a block received over gossip and announce it to the caller.
    fn on_block_message(&self, payload: &[u8]) -> Result<(), NetworkError> {
        let block: StatementBlock = match bincode::deserialize(payload) {
            Ok(block) => block,
            Err(err) => {
                warn!(%err, "dropping malformed block gossip");
                return Ok(());
            }
        };
        let (round, digest) = (block.round, block.digest);
        // A local storage failure is fatal for the event loop: better to surface
        // it than to keep accepting blocks we cannot persist.
        self.dag_store.put_block(&block)?;
        info!(round, %digest, "stored block received over gossip");
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
        drop(connected);
        info!(%peer, "peer connected");
        self.emit(NetworkEvent::PeerConnected(peer));
        Ok(())
    }

    /// Record a disconnected peer (and only report it once).
    fn on_peer_disconnected(&self, peer: PeerId) {
        if lock(&self.connected).remove(&peer) {
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
        }
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
        let first = PeerId::random();

        service.on_peer_connected(first).expect("connect");
        assert_eq!(service.peer_count(), 1);
        // Duplicate connection events do not double count.
        service.on_peer_connected(first).expect("reconnect");
        assert_eq!(service.peer_count(), 1);

        service.on_peer_disconnected(first);
        assert_eq!(service.peer_count(), 0);

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
