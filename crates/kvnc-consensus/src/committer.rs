//! Commit logic (Base + Universal Committer).
//!
#![allow(missing_docs)]
//! Implements Mysticeti-style committer with direct/indirect commit rules
//! for wave-based uncertified DAG consensus.

use crate::engine::DagStoreTrait;
use crate::mysticghost::{order_committed_wave, MysticGhostConfig, MysticGhostOrder};
use crate::types::{CommitResult, CommitteeInfo, LeaderInfo, LeaderStatus};
use kvnc_types::{block::StatementBlock, hash::Hash, AuthorityIndex, CommittedSubDag, Round};
use parking_lot::{Mutex, RwLock};
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
        let leader_hash = leader_info.block_hash.as_ref();

        // Check if we have quorum (2f+1 stake) of votes *for the leader block*
        if self.committee.has_quorum(votes, leader_hash) {
            debug!(
                "Direct commit: leader round {} author {} has quorum ({} votes)",
                leader_info.round,
                leader_info.author,
                votes.len()
            );
            return LeaderStatus::Commit;
        }

        // Check if we have validity threshold but not quorum - could be skip
        if self.committee.has_validity(votes, leader_hash) {
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
    /// Whether to use MysticGhost (scoped GHOSTDAG) ordering for committed waves.
    use_mysticghost: bool,
    /// Last decided round.
    last_decided_round: RwLock<Round>,
    /// Map of leader info by round.
    leaders: RwLock<HashMap<Round, LeaderInfo>>,
    /// Map of decided leaders.
    decided_leaders: RwLock<HashMap<Round, LeaderInfo>>,
    /// Serializes durable commit attempts and their in-memory publication.
    durable_commit_lock: Mutex<()>,
}

impl UniversalCommitter {
    /// Create a new universal committer.
    pub fn new(committee: CommitteeInfo, use_mysticghost: bool) -> Self {
        Self {
            base: BaseCommitter::new(committee),
            use_mysticghost,
            last_decided_round: RwLock::new(0),
            leaders: RwLock::new(HashMap::new()),
            decided_leaders: RwLock::new(HashMap::new()),
            durable_commit_lock: Mutex::new(()),
        }
    }

    /// Update leader information for a round.
    pub fn update_leader(&self, leader_info: LeaderInfo) {
        let mut leaders = self.leaders.write();
        leaders.insert(leader_info.round, leader_info);
    }

    /// Mark a leader as decided.
    fn mark_decided(&self, round: Round, leader_info: LeaderInfo) {
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

            // Check if we now have quorum of votes *for the leader block*
            if self
                .base
                .committee
                .has_quorum(&leader_info.votes, leader_info.block_hash.as_ref())
            {
                leader_info.status = LeaderStatus::Commit;
                return true;
            }
        }
        false
    }

    /// Decide (commit or skip) any still-undecided leaders *earlier in the
    /// same wave* as `commit_round`, using the indirect rule.
    ///
    /// In Mysticeti a leader that is committed (directly or indirectly) makes
    /// the earlier leaders of its wave decidable: each is committed when it is
    /// causally connected to the committed leader, skipped otherwise. Without
    /// this sweep the indirect rule is unreachable, because the main
    /// `try_commit` walk only ever looks at rounds *after* the last decided
    /// round and `try_indirect_decide` only finds *later* decided leaders in
    /// the wave.
    fn decide_earlier_in_wave<D: DagStoreTrait>(&self, dag_store: &D, commit_round: Round) {
        let wave_start = (commit_round / kvnc_types::WAVE_LENGTH) * kvnc_types::WAVE_LENGTH;
        if commit_round <= wave_start {
            return;
        }

        let leaders = self.leaders.read().clone();
        let mut decided = self.decided_leaders.read().clone();

        // Treat the round being committed as already committed for the rule.
        if let Some(committing) = leaders.get(&commit_round) {
            let mut committing_with_status = committing.clone();
            committing_with_status.status = LeaderStatus::Commit;
            decided.insert(commit_round, committing_with_status);
        }

        for round in wave_start..commit_round {
            if decided.contains_key(&round) {
                continue;
            }
            let Some(leader_info) = leaders.get(&round) else {
                continue;
            };
            let status = self
                .base
                .try_indirect_decide(dag_store, leader_info, &decided);
            if status != LeaderStatus::Undecided {
                let mut decided_leader = leader_info.clone();
                decided_leader.status = status;
                self.mark_decided(round, decided_leader);
            }
        }
    }

    /// Clean up old leader info to limit memory usage.
    pub fn cleanup_old_leaders(&self, cutoff: Round) {
        let mut leaders = self.leaders.write();
        leaders.retain(|&round, _| round >= cutoff);
    }

    /// Construct the next committed sub-DAG candidate without publishing it.
    /// Use [`Self::try_commit_and_mark_durable`] to persist and publish it.
    pub fn try_commit<D: DagStoreTrait>(&self, dag_store: &D) -> Option<CommittedSubDag> {
        self.try_commit_with_leader(dag_store)
            .map(|(subdag, _)| subdag)
    }

    /// Construct the next committed sub-DAG together with the registered
    /// leader record that established its eligibility.
    fn try_commit_with_leader<D: DagStoreTrait>(
        &self,
        dag_store: &D,
    ) -> Option<(CommittedSubDag, LeaderInfo)> {
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
                    // Construct only; candidate-only callers do not publish it.
                    return self
                        .build_committed_subdag(dag_store, leader_info)
                        .map(|subdag| (subdag, leader_info.clone()));
                } else if direct_status == LeaderStatus::Skip {
                    // Mark as skipped and continue
                    let mut skipped = leader_info.clone();
                    skipped.status = LeaderStatus::Skip;
                    self.mark_decided(round, skipped);
                    round += 1;
                    continue;
                }

                // Try indirect decision
                let indirect_status =
                    self.base
                        .try_indirect_decide(dag_store, leader_info, &decided);
                if indirect_status == LeaderStatus::Commit {
                    // Construct only; candidate-only callers do not publish it.
                    return self
                        .build_committed_subdag(dag_store, leader_info)
                        .map(|subdag| (subdag, leader_info.clone()));
                } else if indirect_status == LeaderStatus::Skip {
                    let mut skipped = leader_info.clone();
                    skipped.status = LeaderStatus::Skip;
                    self.mark_decided(round, skipped);
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

    /// Durably commit and publish the next eligible leader, if one is ready.
    ///
    /// The decision index is persisted before in-memory state is advanced. If
    /// persistence fails, the eligible leader remains available for retry.
    pub fn try_commit_and_mark_durable<D: DagStoreTrait>(
        &self,
        dag_store: &D,
    ) -> Result<Option<CommittedSubDag>, kvnc_dag::DagStoreError> {
        let _guard = self.durable_commit_lock.lock();
        let Some((subdag, mut decided_leader)) = self.try_commit_with_leader(dag_store) else {
            return Ok(None);
        };

        let leader_block = &subdag.leader;
        let metadata_matches = decided_leader.round == subdag.leader_round
            && decided_leader.author == subdag.leader_author
            && decided_leader.block_hash == Some(leader_block.digest)
            && leader_block.round == subdag.leader_round
            && leader_block.author == subdag.leader_author;
        if !metadata_matches {
            warn!(
                registered_round = decided_leader.round,
                registered_author = decided_leader.author,
                registered_digest = ?decided_leader.block_hash,
                subdag_round = subdag.leader_round,
                subdag_author = subdag.leader_author,
                subdag_digest = %leader_block.digest,
                block_round = leader_block.round,
                block_author = leader_block.author,
                "refusing to persist mismatched committed leader metadata"
            );
            return Ok(None);
        }

        // Persist the exact eligible leader selected above; never synthesize a
        // leader record from a caller-provided sub-DAG.
        dag_store.mark_round_decided(subdag.leader_round, &subdag.leader.digest)?;
        decided_leader.status = LeaderStatus::Commit;
        self.mark_decided(subdag.leader_round, decided_leader);

        // This existing same-wave bookkeeping is committed only after the
        // later leader's durable mark succeeded.
        self.decide_earlier_in_wave(dag_store, subdag.leader_round);
        Ok(Some(subdag))
    }

    /// Build a committed sub-DAG from a leader and its causal history.
    fn build_committed_subdag<D: DagStoreTrait>(
        &self,
        dag_store: &D,
        leader_info: &LeaderInfo,
    ) -> Option<CommittedSubDag> {
        let leader_hash = leader_info.block_hash?;
        let leader_block = dag_store.get_block(&leader_hash).ok()?;

        if self.use_mysticghost {
            // MysticGhost path: use scoped GHOSTDAG colouring
            self.build_committed_subdag_mysticghost(dag_store, &leader_block, leader_info.round)
        } else {
            // Original linearizer path (bit-identical to current behaviour)
            self.build_committed_subdag_linearizer(dag_store, &leader_block)
        }
    }

    /// Original linearizer path (Mysticeti).
    fn build_committed_subdag_linearizer<D: DagStoreTrait>(
        &self,
        dag_store: &D,
        leader_block: &StatementBlock,
    ) -> Option<CommittedSubDag> {
        // Get all ancestors of this leader (causal history)
        let ancestors = dag_store.get_ancestors(&leader_block.digest, 0).ok()?;

        // Get the actual blocks for ancestors
        let mut history = Vec::new();
        for hash in ancestors {
            history.push(dag_store.get_block(&hash).ok()?);
        }

        // Linearize the sub-DAG
        let linearizer = crate::linearizer::Linearizer::new();
        let subdag = linearizer.linearize(leader_block.clone(), history);

        Some(subdag)
    }

    /// MysticGhost path: scoped GHOSTDAG colouring of the mergeset.
    fn build_committed_subdag_mysticghost<D: DagStoreTrait>(
        &self,
        dag_store: &D,
        leader_block: &StatementBlock,
        leader_round: Round,
    ) -> Option<CommittedSubDag> {
        // 1. Get mergeset hashes
        let mergeset_hashes = dag_store.mergeset(&leader_block.digest).ok()?;

        // 2. Get mergeset blocks
        let mergeset_blocks = dag_store.get_blocks(&mergeset_hashes).ok()?;

        // 3. Get previous tips (decided leaders' blocks)
        let previous_tips = self.get_previous_tips(dag_store).ok()?;

        // 4. Configure MysticGhost
        let mg_config = MysticGhostConfig {
            enabled: true,
            k: 3,
            max_mergeset_blocks: 2_000,
        };

        // 5. Run MysticGhost ordering
        match order_committed_wave(&mg_config, &mergeset_blocks, &previous_tips) {
            MysticGhostOrder::Ghost { colouring } => {
                // Use the blue-set order from GHOSTDAG
                let blue_ordered = colouring.blue_ordered();
                
                // Build blocks in blue order, filtering to only those in mergeset
                let block_map: std::collections::HashMap<Hash, StatementBlock> = mergeset_blocks
                    .into_iter()
                    .map(|b| (b.digest, b))
                    .collect();
                
                let mut ordered_blocks = Vec::new();
                for hash in blue_ordered {
                    if let Some(block) = block_map.get(&hash) {
                        ordered_blocks.push(block.clone());
                    }
                }
                
                // Ensure leader is included (should be blue)
                if !ordered_blocks.iter().any(|b| b.digest == leader_block.digest) {
                    ordered_blocks.push(leader_block.clone());
                }

                Some(CommittedSubDag {
                    blocks: ordered_blocks,
                    leader: leader_block.clone(),
                    leader_round,
                    leader_author: leader_block.author,
                })
            }
            MysticGhostOrder::Fallback => {
                // Fall back to original linearizer
                self.build_committed_subdag_linearizer(dag_store, leader_block)
            }
        }
    }

    /// Get previous committed tips for MysticGhost colouring.
    fn get_previous_tips<D: DagStoreTrait>(
        &self,
        dag_store: &D,
    ) -> Result<Vec<Hash>, kvnc_dag::DagStoreError> {
        let decided_rounds = dag_store.get_decided_rounds(u64::MAX)?;
        let mut tips = Vec::new();
        
        for round in decided_rounds {
            let leaders = dag_store.get_decided_leaders(round)?;
            for leader_hash in leaders {
                // Verify the leader block exists and matches
                if let Ok(block) = dag_store.get_block(&leader_hash) {
                    if block.round == round && block.digest == leader_hash {
                        tips.push(leader_hash);
                    }
                }
            }
        }
        
        Ok(tips)
    }
}
