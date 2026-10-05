//! Commit logic (Base + Universal Committer).
//!
#![allow(missing_docs)]
//! Implements Mysticeti-style committer with direct/indirect commit rules
//! for wave-based uncertified DAG consensus.

use crate::engine::DagStoreTrait;
use crate::types::{CommitResult, CommitteeInfo, LeaderInfo, LeaderStatus};
use kvnc_types::{hash::Hash, AuthorityIndex, CommittedSubDag, Round};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Basic committer that tracks leader decisions.
pub struct BaseCommitter {
    committee: CommitteeInfo,
}

impl BaseCommitter {
    /// Create a new base committer.
    pub fn new(committee: CommitteeInfo) -> Self {
        Self { committee }
    }

    /// Try to make a direct decision on a leader round.
    ///
    /// In Mysticeti, a leader is directly decided if it receives votes from
    /// a quorum (2f+1 stake) of validators in the next round.
    pub fn try_direct_decide(
        &self,
        dag_store: &dyn DagStoreTrait,
        leader_info: &LeaderInfo,
    ) -> LeaderStatus {
        if leader_info.round == 0 {
            return LeaderStatus::Undecided;
        }

        let votes = &leader_info.votes;

        // Check if we have quorum (2f+1 stake)
        if self.committee.has_quorum(votes) {
            debug!(
                "Direct commit: leader round {} author {} has quorum ({} votes)",
                leader_info.round,
                leader_info.author,
                votes.len()
            );
            return LeaderStatus::Commit;
        }

        // Check if we have validity threshold but not quorum - could be skip
        if self.committee.has_validity(votes) {
            debug!(
                "Leader round {} author {} has validity but not quorum",
                leader_info.round, leader_info.author
            );
        }

        LeaderStatus::Undecided
    }

    /// Try to make an indirect decision on a leader round.
    ///
    /// In Mysticeti, a leader can be indirectly decided if a later leader
    /// in the same wave is directly committed, or if a leader is skipped
    /// by a later committed leader.
    pub fn try_indirect_decide(
        &self,
        dag_store: &dyn DagStoreTrait,
        leader_info: &LeaderInfo,
        decided_leaders: &HashMap<Round, LeaderInfo>,
    ) -> LeaderStatus {
        let wave = self.wave_of(leader_info.round);
        let wave_start = wave * kvnc_types::WAVE_LENGTH;
        let wave_end = wave_start + kvnc_types::WAVE_LENGTH - 1;

        // Check leaders in the same wave after this one
        for round in (leader_info.round + 1)..=wave_end {
            if let Some(later_leader) = decided_leaders.get(&round) {
                if later_leader.status == LeaderStatus::Commit {
                    // If a later leader in the same wave is committed,
                    // this leader is indirectly decided (skip or commit based on connectivity)
                    if self.has_path(dag_store, leader_info, later_leader) {
                        debug!(
                            "Indirect commit: leader round {} connected to committed leader round {}",
                            leader_info.round, later_leader.round
                        );
                        return LeaderStatus::Commit;
                    } else {
                        debug!(
                            "Indirect skip: leader round {} not connected to committed leader round {}",
                            leader_info.round, later_leader.round
                        );
                        return LeaderStatus::Skip;
                    }
                }
            }
        }

        LeaderStatus::Undecided
    }

    /// Check if there's a causal path from one leader to another.
    fn has_path(&self, dag_store: &dyn DagStoreTrait, from: &LeaderInfo, to: &LeaderInfo) -> bool {
        if let Some(from_hash) = from.block_hash {
            if let Some(to_hash) = to.block_hash {
                match dag_store.get_ancestors(&to_hash, from.round) {
                    Ok(ancestors) => ancestors.contains(&from_hash),
                    Err(_) => false,
                }
            } else {
                false
            }
        } else {
            false
        }
    }

    fn wave_of(&self, round: Round) -> u64 {
        round / kvnc_types::WAVE_LENGTH
    }
}

/// Universal committer that also handles indirect decisions.
pub struct UniversalCommitter {
    base: BaseCommitter,
    /// Last decided round.
    last_decided_round: RwLock<Round>,
    /// Map of leader info by round.
    leaders: RwLock<HashMap<Round, LeaderInfo>>,
    /// Map of decided leaders.
    decided_leaders: RwLock<HashMap<Round, LeaderInfo>>,
}

impl UniversalCommitter {
    /// Create a new universal committer.
    pub fn new(committee: CommitteeInfo) -> Self {
        Self {
            base: BaseCommitter::new(committee),
            last_decided_round: RwLock::new(0),
            leaders: RwLock::new(HashMap::new()),
            decided_leaders: RwLock::new(HashMap::new()),
        }
    }

    /// Update leader information for a round.
    pub fn update_leader(&self, leader_info: LeaderInfo) {
        let mut leaders = self.leaders.write();
        leaders.insert(leader_info.round, leader_info);
    }

    /// Mark a leader as decided.
    pub fn mark_decided(&self, round: Round, leader_info: LeaderInfo) {
        let mut decided = self.decided_leaders.write();
        decided.insert(round, leader_info);
        let mut last = self.last_decided_round.write();
        if round > *last {
            *last = round;
        }
    }

    /// Get the last decided round.
    pub fn last_decided_round(&self) -> Round {
        *self.last_decided_round.read()
    }

    /// Get leader info for a round.
    pub fn get_leader(&self, round: Round) -> Option<LeaderInfo> {
        self.leaders.read().get(&round).cloned()
    }

    /// Get all decided leaders.
    pub fn get_decided_leaders(&self) -> HashMap<Round, LeaderInfo> {
        self.decided_leaders.read().clone()
    }

    /// Get all leader info.
    pub fn get_all_leaders(&self) -> HashMap<Round, LeaderInfo> {
        self.leaders.read().clone()
    }

    /// Get all decided leaders.
    pub fn get_all_decided_leaders(&self) -> HashMap<Round, LeaderInfo> {
        self.decided_leaders.read().clone()
    }

    /// Add a vote to a leader.
    pub fn add_vote(&self, leader_round: Round, voter: AuthorityIndex, vote_hash: Hash) -> bool {
        let mut leaders = self.leaders.write();
        if let Some(leader_info) = leaders.get_mut(&leader_round) {
            leader_info.votes.insert(voter, vote_hash);

            // Check if we now have quorum
            if self.base.committee.has_quorum(&leader_info.votes) {
                leader_info.status = LeaderStatus::Commit;
                return true;
            }
        }
        false
    }

    /// Clean up old leader info to limit memory usage.
    pub fn cleanup_old_leaders(&self, cutoff: Round) {
        let mut leaders = self.leaders.write();
        leaders.retain(|&round, _| round >= cutoff);
    }

    /// Attempt to produce a new committed sub-DAG.
    /// This is the main entry point called by the core loop.
    pub fn try_commit<D: DagStoreTrait>(&self, dag_store: &D) -> Option<CommittedSubDag> {
        let last_decided = self.last_decided_round();
        let leaders = self.leaders.read().clone();
        let decided = self.decided_leaders.read().clone();

        // Walk from last decided round + 1 forward
        let mut round = last_decided + 1;
        let mut max_checked = last_decided + 100; // Limit how far we look ahead

        while round <= max_checked {
            if let Some(leader_info) = leaders.get(&round) {
                // Try direct decision
                let direct_status = self.base.try_direct_decide(dag_store, leader_info);

                if direct_status == LeaderStatus::Commit {
                    // Directly commit this leader
                    return self.build_committed_subdag(dag_store, leader_info, round);
                } else if direct_status == LeaderStatus::Skip {
                    // Mark as skipped and continue
                    self.mark_decided(round, leader_info.clone());
                    round += 1;
                    continue;
                }

                // Try indirect decision
                let indirect_status =
                    self.base
                        .try_indirect_decide(dag_store, leader_info, &decided);
                if indirect_status == LeaderStatus::Commit {
                    return self.build_committed_subdag(dag_store, leader_info, round);
                } else if indirect_status == LeaderStatus::Skip {
                    self.mark_decided(round, leader_info.clone());
                    round += 1;
                    continue;
                }

                // Still undecided
                round += 1;
            } else {
                // No leader info for this round yet
                round += 1;
            }
        }

        None
    }

    /// Build a committed sub-DAG from a leader and its causal history.
    fn build_committed_subdag<D: DagStoreTrait>(
        &self,
        dag_store: &D,
        leader_info: &LeaderInfo,
        round: Round,
    ) -> Option<CommittedSubDag> {
        let leader_hash = leader_info.block_hash?;
        let leader_block = dag_store.get_block(&leader_hash).ok()?;

        // Get all ancestors of this leader (causal history)
        let ancestors = dag_store.get_ancestors(&leader_hash, 0).ok()?;

        // Get the actual blocks for ancestors
        let mut history = Vec::new();
        for hash in ancestors {
            if let Ok(block) = dag_store.get_block(&hash) {
                history.push(block);
            }
        }

        // Mark this leader as decided
        self.mark_decided(round, leader_info.clone());

        // Linearize the sub-DAG
        let linearizer = crate::linearizer::Linearizer::new();
        let subdag = linearizer.linearize(leader_block, history);

        Some(subdag)
    }
}

impl Default for UniversalCommitter {
    fn default() -> Self {
        // Default with empty committee - will be set later
        Self::new(CommitteeInfo::new(0, Vec::new()))
    }
}
