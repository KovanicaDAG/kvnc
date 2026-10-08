//! Swarm construction: transport, security and the composite [`Behaviour`].

use crate::{block_sync, error::NetworkError, topics, NetworkConfig};
use libp2p::{
    gossipsub, identify, kad, kad::store::MemoryStore, noise, ping, request_response,
    request_response::ProtocolSupport, swarm::NetworkBehaviour, tcp, yamux, Multiaddr, PeerId,
    StreamProtocol, Swarm, SwarmBuilder,
};
use std::{error::Error, time::Duration};

/// Maximum gossip frame size: large enough for a full block, small enough to
/// keep a hostile peer from forcing us to buffer an unbounded message.
const MAX_TRANSMIT_SIZE: usize = 1024 * 1024;

/// Identify protocol version advertised to peers.
const IDENTIFY_PROTOCOL: &str = "/kovanica/1.0.0";

/// Kademlia protocol name; peers speaking a different name are ignored by the
/// DHT even if they share the transport.
const KAD_PROTOCOL: &str = "/kovanica/kad/1.0.0";

/// Composite libp2p behaviour: gossip, peer routing, liveness, identification and request-response.
#[derive(NetworkBehaviour)]
pub(crate) struct Behaviour {
    /// Gossipsub carrying blocks, transactions, votes and sync requests.
    pub(crate) gossipsub: gossipsub::Behaviour,
    /// Kademlia routing table for peer discovery and bootstrapping.
    pub(crate) kad: kad::Behaviour<MemoryStore>,
    /// Ping used for liveness checks and peer banning.
    pub(crate) ping: ping::Behaviour,
    /// Identify used for protocol exchange and address discovery.
    pub(crate) identify: identify::Behaviour,
    /// Request-response for block sync.
    pub(crate) block_sync: request_response::Behaviour<block_sync::BlockSyncCodec>,
}

/// Deterministic message id: BLAKE3 of the payload.
///
/// Content addressing means the same block or transaction relayed by several
/// peers is only delivered to the node once, no matter who forwarded it.
pub(crate) fn message_id(data: &[u8]) -> gossipsub::MessageId {
    gossipsub::MessageId::from(kvnc_types::Hash::new(data).0.to_vec())
}

/// Gossipsub configuration: signed messages, strict validation and
/// deterministic message ids.
///
/// `validate_messages` is deliberately left off - received payloads are
/// handled directly from [`gossipsub::Event::Message`] instead.
fn gossipsub_config() -> Result<gossipsub::Config, NetworkError> {
    gossipsub::ConfigBuilder::default()
        .validation_mode(gossipsub::ValidationMode::Strict)
        .message_id_fn(|message| message_id(&message.data))
        .max_transmit_size(MAX_TRANSMIT_SIZE)
        .build()
        .map_err(|err| NetworkError::Gossipsub(err.to_string()))
}

/// Split a bootstrap address at its trailing `/p2p/<peer-id>` component.
///
/// Returns `Ok(None)` for addresses without a peer id (they can still be
/// dialed, we just cannot preload them into Kademlia) and an error when the
/// peer id component is present but malformed.
pub(crate) fn split_peer_suffix(
    addr: &Multiaddr,
) -> Result<Option<(PeerId, Multiaddr)>, NetworkError> {
    let text = addr.to_string();
    let Some(index) = text.rfind("/p2p/") else {
        return Ok(None);
    };
    let peer: PeerId = text[index + "/p2p/".len()..].parse().map_err(|err| {
        NetworkError::Config(format!(
            "invalid peer id in bootstrap address {text}: {err}"
        ))
    })?;
    let base: Multiaddr = text[..index]
        .parse()
        .map_err(|err| NetworkError::Config(format!("invalid bootstrap address {text}: {err}")))?;
    Ok(Some((peer, base)))
}

/// Build the swarm: TCP + Noise + Yamux over a DNS-enabled transport, running
/// [`Behaviour`].
///
/// Bootstrap addresses that carry `/p2p/<id>` are pre-loaded into the Kademlia
/// routing table so the first `bootstrap()` query has somewhere to go.
pub(crate) fn build_swarm(config: &NetworkConfig) -> Result<Swarm<Behaviour>, NetworkError> {
    let mut bootstrap_peers = Vec::new();
    for addr in &config.bootstrap_nodes {
        if let Some(entry) = split_peer_suffix(addr)? {
            bootstrap_peers.push(entry);
        }
    }

    let ping_interval = config.ping_interval;
    // Keep connections alive well past the ping interval; the default idle
    // timeout (5s) would otherwise drop peers between pings.
    let idle_timeout = std::cmp::max(ping_interval * 4, Duration::from_secs(60));

    let swarm = SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .map_err(|err| NetworkError::Transport(err.to_string()))?
        .with_dns()
        .map_err(|err| NetworkError::Transport(err.to_string()))?
        .with_behaviour(|key| -> Result<Behaviour, Box<dyn Error + Send + Sync>> {
            let local_peer_id = key.public().to_peer_id();

            let mut gossipsub = gossipsub::Behaviour::new(
                gossipsub::MessageAuthenticity::Signed(key.clone()),
                gossipsub_config()?,
            )
            .map_err(|err| NetworkError::Gossipsub(err.to_string()))?;
            for name in topics::ALL {
                gossipsub
                    .subscribe(&gossipsub::IdentTopic::new(name))
                    .map_err(|err| NetworkError::Gossipsub(err.to_string()))?;
            }

            let mut kad = kad::Behaviour::with_config(
                local_peer_id,
                MemoryStore::new(local_peer_id),
                kad::Config::new(StreamProtocol::new(KAD_PROTOCOL)),
            );
            for (peer, addr) in &bootstrap_peers {
                kad.add_address(peer, addr.clone());
            }

            let ping = ping::Behaviour::new(ping::Config::new().with_interval(ping_interval));
            let identify = identify::Behaviour::new(identify::Config::new(
                IDENTIFY_PROTOCOL.to_string(),
                key.public(),
            ));

            // Block sync request-response protocol
            let block_sync = request_response::Behaviour::new(
                [(
                    libp2p::StreamProtocol::new(block_sync::BLOCK_SYNC_PROTOCOL),
                    ProtocolSupport::Full,
                )],
                block_sync::block_sync_protocol(),
            );

            Ok(Behaviour {
                gossipsub,
                kad,
                ping,
                identify,
                block_sync,
            })
        })
        .map_err(|err| NetworkError::Swarm(err.to_string()))?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(idle_timeout))
        .build();

    Ok(swarm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_id_is_deterministic() {
        let first = message_id(b"kovanica-block");
        let second = message_id(b"kovanica-block");
        let other = message_id(b"kovanica-transaction");
        assert_eq!(first, second);
        assert_ne!(first, other);
    }

    #[test]
    fn split_peer_suffix_handles_bootstrap_addresses() {
        let addr: Multiaddr = "/dns4/seed.kovanica.online/tcp/9000/p2p/12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN"
            .parse()
            .expect("valid multiaddr");
        let (peer, base) = split_peer_suffix(&addr)
            .expect("parses")
            .expect("has a peer id suffix");
        assert_eq!(base.to_string(), "/dns4/seed.kovanica.online/tcp/9000");
        assert_eq!(addr.to_string(), format!("{base}/p2p/{peer}"));
    }

    #[test]
    fn split_peer_suffix_accepts_dial_only_addresses() {
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/9000".parse().expect("valid multiaddr");
        assert!(split_peer_suffix(&addr).expect("parses").is_none());
    }

    #[test]
    fn build_swarm_subscribes_to_every_topic() {
        let swarm = build_swarm(&NetworkConfig::default()).expect("swarm builds");
        let subscribed: Vec<String> = swarm
            .behaviour()
            .gossipsub
            .topics()
            .map(ToString::to_string)
            .collect();
        for name in topics::ALL {
            assert!(
                subscribed.iter().any(|topic| topic == name),
                "missing subscription for topic {name}"
            );
        }
        assert!(!swarm.local_peer_id().to_string().is_empty());
    }
}
