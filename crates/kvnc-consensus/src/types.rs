//! Consensus-specific types.

#![allow(missing_docs)]
pub use kvnc_types::CommittedSubDag;
use kvnc_types::{block::StatementBlock, AuthorityIndex, Round, Stake};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

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
    epoch: u64,
    /// Authorities in the committee.
    authorities: Vec<AuthorityInfo>,
    /// Total stake.
    total_stake: Stake,
    /// Quorum threshold (2f+1).
    quorum_threshold: Stake,
    /// Validity threshold (f+1).
    validity_threshold: Stake,
}

/// Errors returned when constructing an invalid consensus committee.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CommitteeInfoError {
    /// A consensus committee must contain at least one authority.
    #[error("committee must not be empty")]
    EmptyCommittee,
    /// An authority must have non-zero voting stake.
    #[error("authority {index} has zero stake")]
    ZeroStake { index: AuthorityIndex },
    /// Authority indices must uniquely identify committee members.
    #[error("duplicate authority index {index}")]
    DuplicateAuthorityIndex { index: AuthorityIndex },
    /// Each committee member must use a distinct public key.
    #[error("duplicate authority public key")]
    DuplicatePublicKey,
    /// Total committee stake must fit in `Stake`.
    #[error("total committee stake overflows u64")]
    StakeOverflow,
}

impl CommitteeInfo {
    /// Construct a validated committee and derive its voting thresholds.
    pub fn try_new(
        epoch: u64,
        authorities: Vec<AuthorityInfo>,
    ) -> Result<Self, CommitteeInfoError> {
        if authorities.is_empty() {
            return Err(CommitteeInfoError::EmptyCommittee);
        }

        let mut indices = HashSet::with_capacity(authorities.len());
        let mut public_keys = HashSet::with_capacity(authorities.len());
        let mut total_stake: Stake = 0;
        for authority in &authorities {
            if authority.stake == 0 {
                return Err(CommitteeInfoError::ZeroStake {
                    index: authority.index,
                });
            }
            if !indices.insert(authority.index) {
                return Err(CommitteeInfoError::DuplicateAuthorityIndex {
                    index: authority.index,
                });
            }
            if !public_keys.insert(authority.public_key) {
                return Err(CommitteeInfoError::DuplicatePublicKey);
            }
            total_stake = total_stake
                .checked_add(authority.stake)
                .ok_or(CommitteeInfoError::StakeOverflow)?;
        }

        // Compute thresholds in u128 so doubling the u64 total cannot
        // overflow. For positive T, both results are <= T and therefore fit
        // in Stake.
        let quorum_threshold = (u128::from(total_stake) * 2 / 3 + 1) as Stake;
        let validity_threshold = (u128::from(total_stake) / 3 + 1) as Stake;

        Ok(Self {
            epoch,
            authorities,
            total_stake,
            quorum_threshold,
            validity_threshold,
        })
    }

    /// Current committee epoch.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Authorities in this committee.
    pub fn authorities(&self) -> &[AuthorityInfo] {
        &self.authorities
    }

    /// Total voting stake in this committee.
    pub fn total_stake(&self) -> Stake {
        self.total_stake
    }

    /// Quorum voting threshold, `floor(2T/3) + 1`.
    pub fn quorum_threshold(&self) -> Stake {
        self.quorum_threshold
    }

    /// Validity voting threshold, `floor(T/3) + 1`.
    pub fn validity_threshold(&self) -> Stake {
        self.validity_threshold
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

    /// Get the leader for a given round using deterministic round-robin selection.
    pub fn leader(&self, round: Round) -> AuthorityIndex {
        if self.authorities.is_empty() {
            return 0;
        }
        // Simple deterministic selection based on round
        let idx = (round as usize) % self.authorities.len();
        self.authorities[idx].index
    }

    /// Deterministic stake-weighted leader selection.
    ///
    /// Uses `round` as a seed against total stake so the same
    /// (committee, round) pair always selects the same leader.
    pub fn stake_weighted_leader(&self, round: Round) -> AuthorityIndex {
        if self.authorities.is_empty() {
            return 0;
        }
        let seed = round as u128;
        let total = self.total_stake.max(1) as u128;
        let target = (seed % total) as Stake;
        let mut sorted: Vec<&AuthorityInfo> = self.authorities.iter().collect();
        sorted.sort_by_key(|a| a.index);
        let mut cumulative = 0u128;
        for auth in sorted {
            cumulative += auth.stake as u128;
            if cumulative > target as u128 {
                return auth.index;
            }
        }
        // Fallback (should not reach here for non-empty, positive-stake committee)
        self.authorities.last().unwrap().index
    }
}

#[cfg(test)]
mod stake_weighted_tests {
    use super::*;
    use kvnc_types::crypto::PublicKey;

    fn info(index: AuthorityIndex, stake: Stake) -> AuthorityInfo {
        AuthorityInfo {
            index,
            stake,
            public_key: PublicKey([index as u8; 32]),
            address: kvnc_types::Address([index as u8; 32]),
            network_address: format!("/ip4/127.0.0.1/tcp/900{}", index),
        }
    }

    #[test]
    fn stake_weighted_deterministic() {
        let authorities = vec![info(0, 100), info(1, 200), info(2, 300)];
        let committee = CommitteeInfo::try_new(0, authorities).unwrap();
        assert_eq!(
            committee.stake_weighted_leader(42),
            committee.stake_weighted_leader(42)
        );
    }

    #[test]
    fn stake_weighted_splits_by_stake() {
        let authorities = vec![info(0, 100), info(1, 100)];
        let committee = CommitteeInfo::try_new(0, authorities).unwrap();
        // With equal stake and seed 0, target = 0 % 200 = 0; first auth (cum 100 > 0) wins
        assert_eq!(committee.stake_weighted_leader(0), 0);
        // Seed 99 → target = 99; first auth (cum 100 > 99) still wins
        assert_eq!(committee.stake_weighted_leader(99), 0);
        // Seed 100 → target = 100; first auth cum 100 > 100? No (100 > 100 false). Second wins.
        assert_eq!(committee.stake_weighted_leader(100), 1);
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
