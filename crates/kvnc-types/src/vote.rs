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
    /// Canonical bytes signed for a vote.
    ///
    /// This is the single canonical vote encoding used both for signing
    /// (`kvnc-consensus`) and for verification
    /// (`kvnc_crypto::verify_vote_signature`). Layout (40 bytes):
    ///
    /// | offset | len | field                               |
    /// |--------|-----|-------------------------------------|
    /// | 0      | 8   | `leader_round`, `u64` little-endian |
    /// | 8      | 32  | `leader_hash` raw bytes             |
    ///
    /// `voter` and `signature` are **not** covered.
    ///
    /// **PROVISIONAL:** the vote wire/signing format is still being agreed
    /// (the discussion is led by Main). Candidate additions are a domain
    /// separation tag, the `voter` index and a chain id / epoch. Any change
    /// here is consensus-breaking and must update signer and verifier
    /// together; the golden-vector test below pins the current layout.
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
    fn vote_signature_data_golden_layout() {
        let mut hash = [0u8; 32];
        for (i, b) in hash.iter_mut().enumerate() {
            *b = i as u8;
        }
        let vote = Vote {
            leader_round: 0x0102_0304_0506_0708,
            leader_hash: Hash(hash),
            voter: 7,
            signature: Signature([0xAAu8; 64]),
        };
        let data = vote.signature_data();
        assert_eq!(data.len(), 40);
        assert_eq!(
            &data[..8],
            &[0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]
        );
        assert_eq!(&data[8..], &hash);

        // voter and signature are not part of the signed bytes.
        let mut other = vote.clone();
        other.voter = 9;
        other.signature = Signature([0u8; 64]);
        assert_eq!(other.signature_data(), data);
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
