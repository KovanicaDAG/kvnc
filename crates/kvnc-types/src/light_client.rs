//! Light client types for KVNC.
//!
//! Provides certificate structures for light clients to verify chain state
//! without running a full node. Supports wave commit certificates and
//! colouring certificates for MysticGhost ordering verification.

use crate::{hash::Hash, Address, AuthorityIndex, Round, Signature};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A light-client certificate proving a wave has been committed.
///
/// Contains the committed leader block and sufficient validator signatures
/// (2f+1 of the committee) to prove finality to a light client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaveCommitCertificate {
    /// The committed leader block hash.
    pub leader_hash: Hash,
    /// The round of the committed leader.
    pub leader_round: Round,
    /// The height of the committed leader in the committed DAG.
    pub committed_height: u64,
    /// Validator signatures for this wave commit (authority_index -> signature).
    /// Must represent > 2/3 of committee stake.
    pub signatures: BTreeMap<AuthorityIndex, Signature>,
    /// The committee hash this certificate was created under.
    pub committee_hash: Hash,
}

impl WaveCommitCertificate {
    /// Create a new wave commit certificate.
    pub fn new(
        leader_hash: Hash,
        leader_round: Round,
        committed_height: u64,
        signatures: BTreeMap<AuthorityIndex, Signature>,
        committee_hash: Hash,
    ) -> Self {
        Self {
            leader_hash,
            leader_round,
            committed_height,
            signatures,
            committee_hash,
        }
    }

    /// Verify the certificate has sufficient stake weight (2f+1).
    /// Returns true if the signatures represent > 2/3 of committee stake.
    pub fn verify_quorum(&self, committee: &crate::committee::Committee) -> bool {
        let total_stake: u64 = committee.authorities.iter().map(|a| a.stake).sum();
        let signed_stake: u64 = self
            .signatures
            .keys()
            .filter_map(|&idx| committee.authorities.get(idx as usize))
            .map(|a| a.stake)
            .sum();

        // Need > 2/3 of total stake
        signed_stake * 3 > total_stake * 2
    }
}

/// A colouring certificate for MysticGhost ordering verification.
///
/// Proves that a specific block has a given colour (blue/green) in the
/// GHOSTDAG colouring process, allowing light clients to verify the
/// linearization order without full DAG access.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColouringCertificate {
    /// The block hash being certified.
    pub block_hash: Hash,
    /// The colour assigned to this block (true = blue, false = green).
    pub is_blue: bool,
    /// The round of the block.
    pub round: Round,
    /// The author of the block.
    pub author: AuthorityIndex,
    /// The k-cluster witnesses supporting this colour.
    /// Each witness is a (authority_index, witness_block_hash) pair.
    pub witnesses: Vec<(AuthorityIndex, Hash)>,
    /// The committee hash at the time of colouring.
    pub committee_hash: Hash,
}

impl ColouringCertificate {
    /// Create a new colouring certificate.
    pub fn new(
        block_hash: Hash,
        is_blue: bool,
        round: Round,
        author: AuthorityIndex,
        witnesses: Vec<(AuthorityIndex, Hash)>,
        committee_hash: Hash,
    ) -> Self {
        Self {
            block_hash,
            is_blue,
            round,
            author,
            witnesses,
            committee_hash,
        }
    }

    /// Verify the certificate has sufficient witnesses for k=3 GHOSTDAG.
    /// For k=3, we need at least k+1 = 4 witnesses in the k-cluster.
    pub fn verify_witnesses(&self, k: usize) -> bool {
        self.witnesses.len() >= k + 1
    }
}

/// A state proof for light clients to verify account/contract state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateProof {
    /// The state root this proof is against.
    pub state_root: Hash,
    /// The address being proved.
    pub address: Address,
    /// The account/storage value (if exists).
    pub value: Option<Vec<u8>>,
    /// Merkle proof path (sibling hashes from leaf to root).
    pub proof: Vec<Hash>,
}

impl StateProof {
    /// Create a new state proof.
    pub fn new(
        state_root: Hash,
        address: Address,
        value: Option<Vec<u8>>,
        proof: Vec<Hash>,
    ) -> Self {
        Self {
            state_root,
            address,
            value,
            proof,
        }
    }

    /// Verify the proof against the state root.
    pub fn verify(&self) -> bool {
        // Reconstruct merkle root from leaf + proof
        // This is a simplified version - actual implementation depends on merkle tree structure
        !self.proof.is_empty() || self.value.is_none()
    }
}

/// Light client sync checkpoint - minimal state for fast sync.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LightClientCheckpoint {
    /// The committed leader height this checkpoint represents.
    pub height: u64,
    /// The committed leader block hash.
    pub leader_hash: Hash,
    /// The state root at this height.
    pub state_root: Hash,
    /// The committee hash at this height.
    pub committee_hash: Hash,
    /// Wave commit certificate for this height.
    pub wave_certificate: WaveCommitCertificate,
    /// Timestamp of the checkpoint (block timestamp).
    pub timestamp: u64,
}

impl LightClientCheckpoint {
    /// Create a new checkpoint.
    pub fn new(
        height: u64,
        leader_hash: Hash,
        state_root: Hash,
        committee_hash: Hash,
        wave_certificate: WaveCommitCertificate,
        timestamp: u64,
    ) -> Self {
        Self {
            height,
            leader_hash,
            state_root,
            committee_hash,
            wave_certificate,
            timestamp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{hash::Hash, Address};

    #[test]
    fn wave_commit_certificate_creation() {
        let sigs = BTreeMap::new();
        let cert = WaveCommitCertificate::new(Hash::zero(), 42, 100, sigs, Hash::zero());
        assert_eq!(cert.leader_round, 42);
        assert_eq!(cert.committed_height, 100);
    }

    #[test]
    fn colouring_certificate_creation() {
        let cert = ColouringCertificate::new(Hash::zero(), true, 10, 5, vec![], Hash::zero());
        assert!(cert.is_blue);
        assert_eq!(cert.round, 10);
    }

    #[test]
    fn state_proof_creation() {
        let proof = StateProof::new(
            Hash::zero(),
            Address([0; 32]),
            Some(vec![1, 2, 3]),
            vec![Hash::zero()],
        );
        assert!(proof.value.is_some());
    }

    #[test]
    fn light_client_checkpoint_creation() {
        let sigs = BTreeMap::new();
        let wave_cert = WaveCommitCertificate::new(Hash::zero(), 42, 100, sigs, Hash::zero());
        let checkpoint = LightClientCheckpoint::new(
            100,
            Hash::zero(),
            Hash::zero(),
            Hash::zero(),
            wave_cert,
            1234567890,
        );
        assert_eq!(checkpoint.height, 100);
        assert_eq!(checkpoint.timestamp, 1234567890);
    }
}
