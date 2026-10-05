//! Main consensus engine for KVNC.
//!
#![allow(missing_docs)]
//! Coordinates round advancement, leader selection, block production,
//! and commit decisions using Mysticeti-style DAG consensus.

use crate::types::{CommitteeInfo, LeaderInfo, LeaderStatus};
use crate::{committer::UniversalCommitter, linearizer::Linearizer};
use kvnc_types::{hash::Hash, AuthorityIndex, Round, Stake};
use parking_lot::RwLock;
use rand::seq::SliceRandom;
use rand::thread_rng;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{interval, Instant};
use tracing::{debug, info, warn};

/// Trait for DAG store operations needed by consensus.
pub trait DagStoreTrait: Send + Sync {
    fn get_block(
        &self,
        hash: &Hash,
    ) -> Result<kvnc_types::block::StatementBlock, kvnc_dag::DagStoreError>;
    fn get_ancestors(
        &self,
        hash: &Hash,
        min_round: kvnc_types::Round,
    ) -> Result<Vec<Hash>, kvnc_dag::DagStoreError>;
    fn get_block_by_author_round(
        &self,
        author: kvnc_types::AuthorityIndex,
        round: kvnc_types::Round,
    ) -> Result<Option<kvnc_types::block::StatementBlock>, kvnc_dag::DagStoreError>;
    fn get_blocks_by_round(
        &self,
        round: kvnc_types::Round,
    ) -> Result<Vec<kvnc_types::block::StatementBlock>, kvnc_dag::DagStoreError>;
    fn has_block(&self, hash: &Hash) -> Result<bool, kvnc_dag::DagStoreError>;
    fn put_block(
        &self,
        block: &kvnc_types::block::StatementBlock,
    ) -> Result<(), kvnc_dag::DagStoreError>;
    fn find_parents(
        &self,
        round: kvnc_types::Round,
        max_parents: usize,
    ) -> Result<Vec<kvnc_types::block::BlockReference>, kvnc_dag::DagStoreError>;
    fn commit_leader(&self, leader_hash: &Hash) -> Result<u64, kvnc_dag::DagStoreError>;
    fn mark_round_decided(
        &self,
        round: kvnc_types::Round,
        leader_hash: &Hash,
    ) -> Result<(), kvnc_dag::DagStoreError>;
}

/// Trait for block manager operations needed by consensus.
pub trait BlockManagerTrait: Send + Sync {
    fn propose_block(
        &self,
        round: kvnc_types::Round,
    ) -> Result<kvnc_types::block::StatementBlock, kvnc_dag::BlockManagerError>;
    fn process_block(
        &self,
        block: &kvnc_types::block::StatementBlock,
    ) -> Result<(), kvnc_dag::BlockManagerError>;
    fn our_authority(&self) -> kvnc_types::AuthorityIndex;
    fn set_signing_key(&self, key: kvnc_types::SigningKey);
}

/// Configuration for the consensus engine.
#[derive(Clone, Debug)]
pub struct ConsensusConfig {
    /// Duration of each round in milliseconds.
    pub round_duration_ms: u64,
    /// Number of rounds to look ahead for speculative execution.
    pub lookahead_rounds: u64,
    /// Maximum number of pending rounds to keep in memory.
    pub max_pending_rounds: u64,
}

impl Default for ConsensusConfig {
    fn default() -> Self {
        Self {
            round_duration_ms: 2000, // 2 seconds per round (target block time)
            lookahead_rounds: 3,
            max_pending_rounds: 100,
        }
    }
}

/// State of a validator in the consensus protocol.
#[derive(Clone, Debug)]
pub struct ValidatorState {
    /// Our authority index.
    pub our_authority: kvnc_types::AuthorityIndex,
    /// Our stake.
    pub our_stake: Stake,
    /// Current round.
    pub current_round: Round,
    /// Whether we are the leader for the current round.
    pub is_leader: bool,
    /// Whether we have proposed a block for the current round.
    pub proposed: bool,
}

/// Main consensus engine.
pub struct ConsensusEngine<D, B>
where
    D: DagStoreTrait + 'static,
    B: BlockManagerTrait + 'static,
{
    config: ConsensusConfig,
    committee: CommitteeInfo,
    dag_store: Arc<D>,
    block_manager: Arc<RwLock<B>>,
    committer: UniversalCommitter,
    linearizer: Linearizer,
    state: RwLock<ValidatorState>,
    current_round: RwLock<Round>,
    leader_schedule: RwLock<HashMap<Round, kvnc_types::AuthorityIndex>>,
    running: RwLock<bool>,
}

impl<D, B> ConsensusEngine<D, B>
where
    D: DagStoreTrait + 'static,
    B: BlockManagerTrait + 'static,
{
    /// Create a new consensus engine.
    pub fn new(
        config: ConsensusConfig,
        committee: CommitteeInfo,
        dag_store: Arc<D>,
        block_manager: Arc<RwLock<B>>,
    ) -> Self {
        let our_authority = block_manager.read().our_authority();
        let our_stake = committee.stake_of(our_authority).unwrap_or(0);

        let committer = UniversalCommitter::new(committee.clone());
        let linearizer = Linearizer::new();

        let mut leader_schedule = HashMap::new();
        // Generate leader schedule for first 1000 rounds
        for round in 0..1000 {
            let leader = committee.leader(round);
            leader_schedule.insert(round, leader);
        }

        Self {
            config,
            committee,
            dag_store,
            block_manager,
            committer,
            linearizer,
            state: RwLock::new(ValidatorState {
                our_authority,
                our_stake,
                current_round: 0,
                is_leader: false,
                proposed: false,
            }),
            current_round: RwLock::new(0),
            leader_schedule: RwLock::new(leader_schedule),
            running: RwLock::new(false),
        }
    }

    /// Start the consensus engine.
    pub async fn start(&self) -> Result<(), ConsensusError> {
        info!("Starting consensus engine");
        *self.running.write() = true;

        // Initialize block manager with our signing key
        // In production, this would come from secure key storage
        let (signing_key, _) = kvnc_crypto::generate_keypair();
        self.block_manager.write().set_signing_key(signing_key);

        // Start the round loop
        self.round_loop().await
    }

    /// Stop the consensus engine.
    pub fn stop(&self) {
        *self.running.write() = false;
    }

    /// Main round loop - advances rounds and produces blocks.
    async fn round_loop(&self) -> Result<(), ConsensusError> {
        let mut round_interval = interval(Duration::from_millis(self.config.round_duration_ms));

        while *self.running.read() {
            round_interval.tick().await;

            let current_round = *self.current_round.read();
            let next_round = current_round + 1;

            // Advance to next round
            *self.current_round.write() = next_round;

            // Update our validator state
            let is_leader = self.is_leader_for_round(next_round);
            {
                let mut state = self.state.write();
                state.current_round = next_round;
                state.is_leader = is_leader;
                state.proposed = false;
            }

            // Try to commit any pending leaders
            if let Some(subdag) = self.committer.try_commit(self.dag_store.as_ref()) {
                info!("Committed leader at round {}", subdag.leader_round);
                // In a real implementation, we'd send this to execution layer
            }

            // If we're the leader, propose a block
            if is_leader {
                self.propose_block(next_round).await?;
            }

            // Clean up old rounds
            self.cleanup_old_rounds(next_round);
        }

        Ok(())
    }

    /// Check if we are the leader for a given round.
    fn is_leader_for_round(&self, round: Round) -> bool {
        let schedule = self.leader_schedule.read();
        schedule.get(&round).copied() == Some(self.state.read().our_authority)
    }

    /// Propose a block for the given round.
    async fn propose_block(&self, round: Round) -> Result<(), ConsensusError> {
        info!("Proposing block for round {}", round);

        match self.block_manager.read().propose_block(round) {
            Ok(block) => {
                // Update committer with leader info
                let leader_info = LeaderInfo {
                    round,
                    author: self.state.read().our_authority,
                    block_hash: Some(block.digest),
                    status: LeaderStatus::Undecided,
                    votes: HashMap::new(),
                };
                self.committer.update_leader(leader_info);

                // Mark as proposed
                self.state.write().proposed = true;

                info!("Proposed block {} at round {}", block.digest, round);
                Ok(())
            }
            Err(e) => {
                warn!("Failed to propose block: {}", e);
                Err(ConsensusError::BlockProposal(e.to_string()))
            }
        }
    }

    /// Process a received block from another validator.
    pub fn process_block(
        &self,
        block: &kvnc_types::block::StatementBlock,
    ) -> Result<(), ConsensusError> {
        // Validate the block
        self.block_manager.read().process_block(block)?;

        // Update leader info
        let leader_info = LeaderInfo {
            round: block.round,
            author: block.author,
            block_hash: Some(block.digest),
            status: LeaderStatus::Undecided,
            votes: HashMap::new(),
        };
        self.committer.update_leader(leader_info);

        // Try to commit after processing
        if let Some(subdag) = self.committer.try_commit(self.dag_store.as_ref()) {
            info!(
                "Committed leader at round {} after receiving block",
                subdag.leader_round
            );
        }

        Ok(())
    }

    /// Process a vote for a leader block.
    pub fn process_vote(
        &self,
        leader_round: Round,
        voter: kvnc_types::AuthorityIndex,
        vote_hash: Hash,
    ) -> Result<(), ConsensusError> {
        let has_quorum = self.committer.add_vote(leader_round, voter, vote_hash);
        if has_quorum {
            info!("Leader round {} now has quorum", leader_round);
        }
        Ok(())
    }

    /// Clean up old rounds to limit memory usage.
    fn cleanup_old_rounds(&self, current_round: Round) {
        let max_pending = self.config.max_pending_rounds;
        if current_round > max_pending {
            let cutoff = current_round - max_pending;
            self.committer.cleanup_old_leaders(cutoff);
        }
    }

    /// Get the current round.
    pub fn current_round(&self) -> Round {
        *self.current_round.read()
    }

    /// Get our validator state.
    pub fn state(&self) -> ValidatorState {
        self.state.read().clone()
    }

    /// Get the committee info.
    pub fn committee(&self) -> &CommitteeInfo {
        &self.committee
    }
}

/// Errors that can occur in consensus operations.
#[derive(Debug, thiserror::Error)]
pub enum ConsensusError {
    #[error("Block proposal failed: {0}")]
    BlockProposal(String),
    #[error("DAG store error: {0}")]
    DagStore(#[from] kvnc_dag::DagStoreError),
    #[error("Block manager error: {0}")]
    BlockManager(#[from] kvnc_dag::BlockManagerError),
    #[error("Crypto error: {0}")]
    Crypto(#[from] kvnc_crypto::CryptoError),
    #[error("Storage error: {0}")]
    Storage(#[from] kvnc_storage::StorageError),
}
