//! Consensus-specific types.

#![allow(missing_docs)]
pub use kvnc_types::CommittedSubDag;
use kvnc_types::{block::StatementBlock, AuthorityIndex, Round, Stake};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Status of a leader slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeaderStatus {
    /// Decision not yet possible.
    Undecided,
    /// Should be committed.
    Commit,
    /// Should be skipped.
    Skip,
}

/// Information about a leader in a round.
#[derive(Clone, Debug)]
pub struct LeaderInfo {
    /// Round number.
    pub round: Round,
    /// Authority index of the leader.
    pub author: AuthorityIndex,
    /// Block hash if the leader block exists.
    pub block_hash: Option<kvnc_types::hash::Hash>,
    /// Current status.
    pub status: LeaderStatus,
    /// Votes received (author -> vote block hash).
    pub votes: HashMap<AuthorityIndex, kvnc_types::hash::Hash>,
}

/// Committee information for consensus.
#[derive(Clone, Debug)]
pub struct CommitteeInfo {
    /// Current epoch.
    pub epoch: u64,
    /// Authorities in the committee.
    pub authorities: Vec<AuthorityInfo>,
    /// Total stake.
    pub total_stake: Stake,
    /// Quorum threshold (2f+1).
    pub quorum_threshold: Stake,
    /// Validity threshold (f+1).
    pub validity_threshold: Stake,
}

impl CommitteeInfo {
    pub fn new(epoch: u64, authorities: Vec<AuthorityInfo>) -> Self {
        let total_stake: Stake = authorities.iter().map(|a| a.stake).sum();
        let quorum_threshold = (total_stake * 2) / 3 + 1;
        let validity_threshold = total_stake / 3 + 1;

        Self {
            epoch,
            authorities,
            total_stake,
            quorum_threshold,
            validity_threshold,
        }
    }

    pub fn size(&self) -> usize {
        self.authorities.len()
    }

    pub fn get_by_index(&self, index: AuthorityIndex) -> Option<&AuthorityInfo> {
        self.authorities.iter().find(|a| a.index == index)
    }

    pub fn stake_of(&self, index: AuthorityIndex) -> Option<Stake> {
        self.get_by_index(index).map(|a| a.stake)
    }

    /// Check if we have quorum (2f+1) of votes **for the leader block**.
    ///
    /// Only votes whose hash matches `leader_hash` are counted. A vote for a
    /// different block (equivocation, or a stale/foreign vote) must never
    /// contribute to committing this leader. When the leader block is unknown
    /// (`None`), no quorum can exist.
    pub fn has_quorum(
        &self,
        votes: &HashMap<AuthorityIndex, kvnc_types::hash::Hash>,
        leader_hash: Option<&kvnc_types::hash::Hash>,
    ) -> bool {
        self.stake_for(votes, leader_hash) >= self.quorum_threshold
    }

    /// Check if we have the validity threshold (f+1) of votes **for the leader block**.
    pub fn has_validity(
        &self,
        votes: &HashMap<AuthorityIndex, kvnc_types::hash::Hash>,
        leader_hash: Option<&kvnc_types::hash::Hash>,
    ) -> bool {
        self.stake_for(votes, leader_hash) >= self.validity_threshold
    }

    /// Sum the stake of voters whose vote hash matches `leader_hash`.
    /// Returns 0 when the leader block is unknown.
    fn stake_for(
        &self,
        votes: &HashMap<AuthorityIndex, kvnc_types::hash::Hash>,
        leader_hash: Option<&kvnc_types::hash::Hash>,
    ) -> Stake {
        match leader_hash {
            Some(target) => votes
                .iter()
                .filter(|(_, h)| *h == target)
                .filter_map(|(idx, _)| self.stake_of(*idx))
                .sum(),
            None => 0,
        }
    }

    /// Get the leader for a given round (deterministic stake-weighted selection).
    pub fn leader(&self, round: Round) -> AuthorityIndex {
        if self.authorities.is_empty() {
            return 0;
        }
        // Simple deterministic selection based on round
        let idx = (round as usize) % self.authorities.len();
        self.authorities[idx].index
    }
}

/// Individual authority information.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthorityInfo {
    pub index: AuthorityIndex,
    pub stake: Stake,
    pub public_key: kvnc_types::crypto::PublicKey,
    pub address: kvnc_types::Address,
    pub network_address: String,
}

/// Result of a commit attempt.
#[derive(Debug)]
pub struct CommitResult {
    /// Committed sub-DAG if successful.
    pub subdag: Option<CommittedSubDag>,
    /// The round that was committed.
    pub round: Round,
    /// Whether a new leader was committed.
    pub committed_new: bool,
}
