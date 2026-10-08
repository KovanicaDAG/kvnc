//! P2P networking layer (libp2p).
//!
//! The crate runs a [`libp2p::Swarm`] composed of gossipsub (topics `blocks`,
//! `transactions`, `votes` and `sync`), Kademlia peer discovery, ping-based
//! liveness and identify, wrapped in [`NetworkService`]. Swarm activity is
//! translated into a stream of [`NetworkEvent`]s for the rest of the node.

#![deny(unsafe_code)]
#![allow(missing_docs)]
#![allow(clippy::result_large_err)]
#![allow(clippy::large_enum_variant)]

mod behaviour;
mod block_sync;
mod error;
mod service;
mod sync;
pub mod topics;

pub use block_sync::{BlockRequest, BlockResponse, BlockSyncRequestEvent, BlockSyncResponseEvent, BLOCK_SYNC_PROTOCOL};
pub use error::NetworkError;
pub use service::NetworkService;
pub use sync::SyncRequest;

use kvnc_types::{block::StatementBlock, transaction::Transaction, Vote, Round};
use libp2p::{Multiaddr, PeerId};
use std::time::Duration;

/// Network event emitted by the networking layer.
#[derive(Debug)]
pub enum NetworkEvent {
    /// A new block was received.
    BlockReceived(StatementBlock),
    /// A new transaction was received.
    TransactionReceived(Transaction),
    /// A new peer was connected.
    PeerConnected(PeerId),
    /// A peer was disconnected.
    PeerDisconnected(PeerId),
    /// A new peer was discovered.
    PeerDiscovered(PeerId, Multiaddr),
    /// Sync request received (gossipsub-based).
    SyncRequest {
        /// Peer that issued the request.
        peer: PeerId,
        /// First round of the requested range.
        from_round: Round,
        /// Last round of the requested range.
        to_round: Round,
    },
    /// Block sync response received (request-response).
    BlockSyncResponse {
        /// Peer that sent the response.
        peer: PeerId,
        /// The request ID this response corresponds to.
        request_id: libp2p::request_response::OutboundRequestId,
        /// The response.
        response: BlockResponse,
    },
    /// A consensus vote was received.
    VoteReceived {
        /// Peer that sent the vote.
        peer: PeerId,
        /// The vote.
        vote: Vote,
    },
}

/// Network configuration.
#[derive(Clone, Debug)]
pub struct NetworkConfig {
    /// Listening addresses.
    pub listen_addrs: Vec<Multiaddr>,
    /// Bootstrap nodes for initial peer discovery.
    pub bootstrap_nodes: Vec<Multiaddr>,
    /// Maximum number of peers.
    pub max_peers: usize,
    /// Ping interval.
    pub ping_interval: Duration,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        // NOTE: the `/p2p/` component must carry the seed's real libp2p peer id.
        // The value below is a syntactically valid placeholder until the seed
        // publishes one; a mismatching peer id only fails the dial, which the
        // event loop tolerates.
        Self {
            listen_addrs: vec!["/ip4/0.0.0.0/tcp/9000"
                .parse()
                .expect("valid listen multiaddr")],
            bootstrap_nodes: vec!["/dns4/seed.kovanica.online/tcp/9000/p2p/12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN"
                .parse()
                .expect("valid bootstrap multiaddr")],
            max_peers: 50,
            ping_interval: Duration::from_secs(10),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_parses_without_panicking() {
        let config = NetworkConfig::default();
        assert_eq!(config.listen_addrs.len(), 1);
        assert_eq!(config.listen_addrs[0].to_string(), "/ip4/0.0.0.0/tcp/9000");
        assert_eq!(config.bootstrap_nodes.len(), 1);
        assert!(
            config.bootstrap_nodes[0]
                .to_string()
                .contains("/dns4/seed.kovanica.online/tcp/9000/p2p/"),
            "unexpected default bootstrap: {}",
            config.bootstrap_nodes[0]
        );
        assert_eq!(config.max_peers, 50);
        assert_eq!(config.ping_interval, Duration::from_secs(10));
    }

    #[test]
    fn network_events_carry_their_payloads() {
        let peer = PeerId::random();
        let event = NetworkEvent::PeerDiscovered(
            peer,
            "/ip4/127.0.0.1/tcp/9000".parse().expect("valid multiaddr"),
        );
        match event {
            NetworkEvent::PeerDiscovered(seen, addr) => {
                assert_eq!(seen, peer);
                assert_eq!(addr.to_string(), "/ip4/127.0.0.1/tcp/9000");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
