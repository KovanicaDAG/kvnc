//! Gossipsub topic names shared by the whole crate.

use libp2p::gossipsub::IdentTopic;

/// Topic carrying full [`kvnc_types::block::StatementBlock`]s.
pub const BLOCKS: &str = "blocks";

/// Topic carrying broadcast [`kvnc_types::transaction::Transaction`]s.
pub const TRANSACTIONS: &str = "transactions";

/// Topic carrying votes for the consensus layer.
pub const VOTES: &str = "votes";

/// Topic carrying sync-range requests.
pub const SYNC: &str = "sync";

/// Topic carrying state sync requests (fast sync).
pub const STATE_SYNC: &str = "state_sync";

/// Every topic subscribed to at startup, in subscription order.
pub const ALL: [&str; 5] = [BLOCKS, TRANSACTIONS, VOTES, SYNC, STATE_SYNC];

/// Topic handle for a topic name (round-trips through [`gossipsub::TopicHash`]).
///
/// [`gossipsub::TopicHash`]: libp2p::gossipsub::TopicHash
pub fn ident(name: &str) -> IdentTopic {
    IdentTopic::new(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn all_topics_are_unique() {
        let mut seen = HashSet::new();
        for name in ALL {
            assert!(seen.insert(name), "duplicate topic name: {name}");
        }
        assert_eq!(ALL.len(), 5);
    }

    #[test]
    fn topic_handles_round_trip_to_their_names() {
        for name in ALL {
            assert_eq!(ident(name).hash().as_str(), name);
        }
    }
}
