//! Consensus vote types.

use crate::crypto::Signature;
use crate::hash::Hash;
use crate::{AuthorityIndex, Round};
use serde::{Deserialize, Serialize};

/// A consensus vote for a leader block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vote {
    /// The round of the leader being voted for.
    pub leader_round: Round,
    /// The hash of the leader block being voted for.
    pub leader_hash: Hash,
    /// The authority index of the voter.
    pub voter: AuthorityIndex,
    /// Signature of the voter over (leader_round, leader_hash).
    pub signature: Signature,
}

impl Vote {
    /// Create the data that is signed for a vote.
    pub fn signature_data(&self) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&self.leader_round.to_le_bytes());
        data.extend_from_slice(&self.leader_hash.0);
        data
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::Hash;

    #[test]
    fn vote_signature_data_is_deterministic() {
        let vote = Vote {
            leader_round: 3,
            leader_hash: Hash::new("test-leader-hash"),
            voter: 0,
            signature: Signature([0u8; 64]),
        };
        let data1 = vote.signature_data();
        let data2 = vote.signature_data();
        assert_eq!(data1, data2);
    }

    #[test]
    fn vote_round_trip() {
        let vote = Vote {
            leader_round: 42,
            leader_hash: Hash::new("test-hash"),
            voter: 5,
            signature: Signature([1u8; 64]),
        };
        let encoded = bincode::serialize(&vote).expect("serializes");
        let decoded: Vote = bincode::deserialize(&encoded).expect("deserializes");
        assert_eq!(vote, decoded);
    }
}