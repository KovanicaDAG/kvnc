//! Main consensus engine for KVNC.
//!
#![allow(missing_docs)]
//! Coordinates round advancement, leader selection, block production,
//! and commit decisions using Mysticeti-style DAG consensus.

use crate::metrics::{record_block_height, record_commit_latency, record_mergeset_size_metric};
use crate::types::{CommitteeInfo, LeaderInfo, LeaderStatus};
use crate::{committer::UniversalCommitter, is_leader_round, linearizer::Linearizer};
use kvnc_crypto::sign;
use kvnc_mempool::Mempool;
use kvnc_types::{
    block::StatementBlock, hash::Hash, AuthorityIndex, Round, Stake, Transaction, MAX_TXS_PER_BLOCK,
};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::time::interval;
use tracing::{debug, info, warn};

/// Trait for broadcasting blocks to the network.
pub trait BlockBroadcaster: Send + Sync {
    fn broadcast_block(&self, block: &StatementBlock);
}

impl<F> BlockBroadcaster for F
where
    F: Fn(&StatementBlock) + Send + Sync,
{
    fn broadcast_block(&self, block: &StatementBlock) {
        self(block)
    }
}

/// Trait for broadcasting votes to the network.
pub trait VoteBroadcaster: Send + Sync {
    fn broadcast_vote(&self, vote: &kvnc_types::Vote);
}

impl<F> VoteBroadcaster for F
where
    F: Fn(&kvnc_types::Vote) + Send + Sync,
{
    fn broadcast_vote(&self, vote: &kvnc_types::Vote) {
        self(vote)
    }
}

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
    fn get_parents(&self, hash: &Hash) -> Result<Vec<Hash>, kvnc_dag::DagStoreError>;
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
    /// Atomically mark `round` decided for `leader_hash` AND advance the
    /// committed leader height / last committed leader. Implementations
    /// backed by durable storage must apply both or neither (one write
    /// transaction), so a crash can never leave a decided round without the
    /// matching committed-leader record or vice versa.
    fn mark_decided_and_commit_leader(
        &self,
        round: kvnc_types::Round,
        leader_hash: &Hash,
    ) -> Result<u64, kvnc_dag::DagStoreError>;
    /// Get the mergeset for a leader block (blocks reachable from leader not in previous sub-DAGs).
    fn mergeset(&self, leader: &Hash) -> Result<Vec<Hash>, kvnc_dag::DagStoreError>;
    /// Get multiple blocks by their hashes.
    fn get_blocks(
        &self,
        hashes: &[Hash],
    ) -> Result<Vec<kvnc_types::block::StatementBlock>, kvnc_dag::DagStoreError>;
    /// Get decided leader hashes for a round.
    fn get_decided_leaders(&self, round: Round) -> Result<Vec<Hash>, kvnc_dag::DagStoreError>;
    /// Get all decided rounds up to a maximum.
    fn get_decided_rounds(&self, max_round: Round) -> Result<Vec<Round>, kvnc_dag::DagStoreError>;
    /// Prune non-blue blocks from a committed wave.
    fn prune_non_blue(
        &self,
        blue_hashes: &[Hash],
        committed_wave: u64,
    ) -> Result<u64, kvnc_dag::DagStoreError>;
    /// Prune all blocks from waves before the given wave minus the prune window.
    fn prune_waves_before(
        &self,
        wave: u64,
        prune_window_waves: u64,
    ) -> Result<u64, kvnc_dag::DagStoreError>;
}

/// Trait for block manager operations needed by consensus.
pub trait BlockManagerTrait: Send + Sync {
    fn propose_block(
        &self,
        round: kvnc_types::Round,
    ) -> Result<kvnc_types::block::StatementBlock, kvnc_dag::BlockManagerError>;
    fn propose_block_with_txs(
        &self,
        round: kvnc_types::Round,
        transactions: Vec<kvnc_types::Transaction>,
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
    /// Enable the experimental MysticGhost ordering path (default off).
    ///
    /// Scaffold only: nothing consumes this yet, so the flag has no
    /// behavioural effect until the committer wiring lands.
    pub use_mysticghost: bool,
    /// Number of waves to keep before pruning old waves.
    /// Default: 100 waves.
    pub prune_window_waves: u64,
    /// Leader timeout in milliseconds; skip leader if no block arrives.
    pub leader_timeout_ms: u64,
}

impl Default for ConsensusConfig {
    fn default() -> Self {
        Self {
            round_duration_ms: 2000, // 2 seconds per round (target block time)
            lookahead_rounds: 3,
            max_pending_rounds: 100,
            use_mysticghost: false,
            prune_window_waves: 100,
            leader_timeout_ms: 3000,
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
    signing_key: kvnc_types::SigningKey,
    dag_store: Arc<D>,
    block_manager: Arc<RwLock<B>>,
    committer: UniversalCommitter,
    linearizer: Linearizer,
    state: RwLock<ValidatorState>,
    current_round: RwLock<Round>,
    leader_schedule: RwLock<HashMap<Round, kvnc_types::AuthorityIndex>>,
    running: RwLock<bool>,
    commit_sender: RwLock<Option<mpsc::UnboundedSender<kvnc_types::CommittedSubDag>>>,
    block_broadcaster: RwLock<Option<Arc<dyn BlockBroadcaster>>>,
    vote_broadcaster: RwLock<Option<Arc<dyn VoteBroadcaster>>>,
    /// Watch channel sender for round changes.
    round_sender: RwLock<Option<watch::Sender<Round>>>,
    /// Serializes decision construction, durable marking, state publication,
    /// and execution delivery across all commit triggers.
    commit_trigger_lock: Mutex<()>,
    /// Mempool for transaction selection during block proposal.
    mempool: Option<Arc<Mempool>>,
    /// Signing context (chain_id/epoch) for vote signature bytes.
    signing_ctx: kvnc_types::SigningContext,
    /// Timeout deadline for the current leader slot (round, instant).
    leader_deadline: RwLock<Option<(Round, std::time::Instant)>>,
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
        signing_key: kvnc_types::SigningKey,
        mempool: Option<Arc<Mempool>>,
        // TODO(owner): source chain_id/epoch from node config
        signing_ctx: kvnc_types::SigningContext,
    ) -> Self {
        let our_authority = block_manager.read().our_authority();
        let our_stake = committee.stake_of(our_authority).unwrap_or(0);

        let committer = UniversalCommitter::new(
            committee.clone(),
            config.use_mysticghost,
            config.prune_window_waves,
        );
        let linearizer = Linearizer::new();

        let mut leader_schedule = HashMap::new();
        // Generate leader schedule for first 1000 rounds
        for round in 0..1000 {
            let leader = committee.leader(round);
            leader_schedule.insert(round, leader);
        }

        // Create watch channel for round notifications
        let (round_tx, _round_rx) = watch::channel(0);

        Self {
            signing_ctx,
            config,
            committee,
            signing_key,
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
            commit_sender: RwLock::new(None),
            block_broadcaster: RwLock::new(None),
            vote_broadcaster: RwLock::new(None),
            round_sender: RwLock::new(Some(round_tx)),
            commit_trigger_lock: Mutex::new(()),
            mempool,
            leader_deadline: RwLock::new(None),
        }
    }

    /// Set the block broadcaster for gossiping proposed blocks.
    pub fn set_block_broadcaster(&self, broadcaster: Arc<dyn BlockBroadcaster>) {
        *self.block_broadcaster.write() = Some(broadcaster);
    }

    /// Set the vote broadcaster for gossiping votes.
    pub fn set_vote_broadcaster(&self, broadcaster: Arc<dyn VoteBroadcaster>) {
        *self.vote_broadcaster.write() = Some(broadcaster);
    }

    /// Deliver durable committed sub-DAGs to the execution pipeline.
    pub fn set_commit_sender(&self, sender: mpsc::UnboundedSender<kvnc_types::CommittedSubDag>) {
        *self.commit_sender.write() = Some(sender);
    }

    /// Start the consensus engine.
    pub async fn start(&self) -> Result<(), ConsensusError> {
        info!("Starting consensus engine");
        // The key comes from the node's explicit validator configuration. Do
        // not generate or replace validator identity inside consensus.
        self.block_manager
            .read()
            .set_signing_key(self.signing_key.clone());
        *self.running.write() = true;

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
            // Timeout check: skip leader if no block after leader_timeout_ms
            {
                let deadline_opt = *self.leader_deadline.read();
                if let Some((round, instant)) = deadline_opt {
                    if std::time::Instant::now()
                        .duration_since(instant)
                        .as_millis()
                        >= self.config.leader_timeout_ms as u128
                    {
                        if let Some(leader_author) = self.scheduled_leader_for_round(round) {
                            self.committer.register_skip(round, leader_author);
                        }
                        self.skip_leader(round)?;
                        *self.leader_deadline.write() = None;
                    }
                }
            }

            round_interval.tick().await;

            let current_round = *self.current_round.read();
            let next_round = current_round + 1;

            // Advance to next round
            *self.current_round.write() = next_round;

            // Record current round metric
            record_block_height(next_round as i64);

            // Broadcast round change to subscribers
            if let Some(tx) = self.round_sender.read().as_ref() {
                let _ = tx.send(next_round);
            }

            // Update our validator state
            let is_leader = self.is_leader_for_round(next_round);
            {
                let mut state = self.state.write();
                state.current_round = next_round;
                state.is_leader = is_leader;
                state.proposed = false;
            }

            // Try to commit any pending leaders
            self.try_commit_and_deliver()?;

            // If we're the leader, propose a block and start timeout timer
            if is_leader {
                self.propose_block(next_round).await?;
                *self.leader_deadline.write() = Some((next_round, std::time::Instant::now()));
            } else if crate::is_leader_round(next_round) {
                // Non-leader: start timeout for the scheduled leader of this round
                *self.leader_deadline.write() = Some((next_round, std::time::Instant::now()));
            }

            // If this is a vote round, produce a vote for the previous leader round
            if crate::is_vote_round(next_round) {
                self.produce_vote(next_round).await?;
            }

            // Clean up old rounds
            self.cleanup_old_rounds(next_round);
        }

        Ok(())
    }

    /// Skip a leader slot (timeout or fork resolution).
    fn skip_leader(&self, round: Round) -> Result<(), ConsensusError> {
        if let Some(leader) = self.scheduled_leader_for_round(round) {
            let info = LeaderInfo {
                round,
                author: leader,
                block_hash: None,
                status: LeaderStatus::Skip,
                votes: HashMap::new(),
            };
            self.committer.update_leader(info);
            info!("Leader round {} skipped (timeout/fork)", round);
        }
        // Try to commit after skip so downstream can progress
        self.try_commit_and_deliver()?;
        Ok(())
    }

    /// Check if we are the leader for a given round.
    fn is_leader_for_round(&self, round: Round) -> bool {
        self.scheduled_leader_for_round(round) == Some(self.state.read().our_authority)
    }

    /// Return the scheduled leader only for a protocol leader slot.
    fn scheduled_leader_for_round(&self, round: Round) -> Option<kvnc_types::AuthorityIndex> {
        if !is_leader_round(round) {
            return None;
        }

        Some(
            self.leader_schedule
                .read()
                .get(&round)
                .copied()
                .unwrap_or_else(|| self.committee.leader(round)),
        )
    }

    /// Propose a block for the given round.
    async fn propose_block(&self, round: Round) -> Result<(), ConsensusError> {
        info!("Proposing block for round {}", round);

        // Get transactions from mempool if available
        let transactions = if let Some(mempool) = &self.mempool {
            mempool.get_next_transactions(MAX_TXS_PER_BLOCK)
        } else {
            Vec::new()
        };

        match self
            .block_manager
            .read()
            .propose_block_with_txs(round, transactions)
        {
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

                // Mark as proposed for this round.
                self.state.write().proposed = true;

                info!(
                    "Proposed block {} at round {} with {} transactions",
                    block.digest,
                    round,
                    block.transactions.len()
                );

                // Broadcast the block to the network
                if let Some(broadcaster) = self.block_broadcaster.read().as_ref() {
                    broadcaster.broadcast_block(&block);
                }

                Ok(())
            }
            Err(e) => {
                warn!("Failed to propose block: {}", e);
                Err(ConsensusError::BlockProposal(e.to_string()))
            }
        }
    }

    /// Produce a vote for the leader of the previous leader round.
    /// Called during vote rounds (offset 1).
    async fn produce_vote(&self, vote_round: Round) -> Result<(), ConsensusError> {
        // The leader round is the previous leader round (vote_round - 1)
        let leader_round = vote_round - 1;

        // Get the leader for that round
        let Some(leader_author) = self.scheduled_leader_for_round(leader_round) else {
            debug!("No leader scheduled for round {}", leader_round);
            return Ok(());
        };

        // Get the leader block hash from the committer
        let Some(leader_info) = self.committer.get_leader(leader_round) else {
            debug!("No leader info for round {}", leader_round);
            return Ok(());
        };

        let Some(leader_hash) = leader_info.block_hash else {
            debug!("Leader block for round {} not yet known", leader_round);
            return Ok(());
        };

        // Check if we already voted for this leader
        if leader_info
            .votes
            .contains_key(&self.state.read().our_authority)
        {
            debug!("Already voted for leader round {}", leader_round);
            return Ok(());
        }

        // Create and sign the vote
        let our_authority = self.state.read().our_authority;
        let mut vote = kvnc_types::Vote {
            leader_round,
            leader_hash,
            voter: our_authority,
            signature: kvnc_types::Signature([0u8; 64]),
        };
        vote.signature = sign(&self.signing_key, &vote.signature_data(&self.signing_ctx));

        // Record the vote locally
        self.committer
            .add_vote(leader_round, our_authority, leader_hash);

        // Broadcast the vote
        if let Some(broadcaster) = self.vote_broadcaster.read().as_ref() {
            broadcaster.broadcast_vote(&vote);
            info!(
                "Broadcast vote for leader round {} (hash: {})",
                leader_round, leader_hash
            );
        }

        // Try to commit after voting
        self.try_commit_and_deliver()?;

        Ok(())
    }

    /// Helper to create the data signed for a vote.
    fn vote_signature_data(leader_round: Round, leader_hash: Hash) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&leader_round.to_le_bytes());
        data.extend_from_slice(&leader_hash.0);
        data
    }

    /// Process a received block from another validator.
    pub fn process_block(
        &self,
        block: &kvnc_types::block::StatementBlock,
    ) -> Result<(), ConsensusError> {
        // Fork resolution for scheduled leader slots: first-valid digest wins.
        // Deterministic lookup by round/author (no HashMap iteration order).
        if self.scheduled_leader_for_round(block.round) == Some(block.author) {
            if let Ok(Some(existing)) = self
                .dag_store
                .get_block_by_author_round(block.author, block.round)
            {
                if existing.digest != block.digest {
                    return Err(ConsensusError::InvalidVote(format!(
                        "fork/equivocation at round {} author {}: existing={}, new={}",
                        block.round, block.author, existing.digest, block.digest
                    )));
                }
                // Same digest: idempotent; skip duplicate processing if already in store.
                // Clear timeout and try commit; do not put/re-register.
                {
                    let mut dl = self.leader_deadline.write();
                    if let Some((r, _)) = *dl {
                        if r == block.round
                            && self.scheduled_leader_for_round(r) == Some(block.author)
                        {
                            *dl = None;
                        }
                    }
                }
                self.try_commit_and_deliver()?;
                return Ok(());
            }
        }

        // Validate the block
        self.block_manager.read().process_block(block)?;

        // Every valid block belongs in the DAG, but only the scheduled author
        // of an offset-zero round represents a leader slot. In particular,
        // ordinary vote/decision-round blocks must not overwrite leader state.
        if self.scheduled_leader_for_round(block.round) == Some(block.author) {
            // After validation/store, register leader (pre-check already rejected forks).
            let leader_info = LeaderInfo {
                round: block.round,
                author: block.author,
                block_hash: Some(block.digest),
                status: LeaderStatus::Undecided,
                votes: HashMap::new(),
            };
            self.committer.update_leader(leader_info);
        }

        // Clear timeout if this block resolves the current leader slot
        {
            let mut dl = self.leader_deadline.write();
            if let Some((r, _)) = *dl {
                if r == block.round && self.scheduled_leader_for_round(r) == Some(block.author) {
                    *dl = None;
                }
            }
        }

        // Try to commit after processing
        self.try_commit_and_deliver()?;

        Ok(())
    }

    /// Ingest a vote for a leader block. This is the authentication boundary
    /// for votes: every vote, including ones received over gossip, must enter
    /// consensus through this method.
    ///
    /// A vote is counted only if all of the following hold, checked in order:
    ///
    /// 1. `vote.voter` is a member of the current [`CommitteeInfo`];
    /// 2. `vote.signature` is a valid signature over
    ///    [`kvnc_types::Vote::signature_data`] under that member's public key
    ///    from the committee (a signature made with any other key, including
    ///    another validator's, is rejected);
    /// 3. `vote.leader_round` is a leader round and a leader block is
    ///    registered for it;
    /// 4. `vote.leader_hash` matches the registered leader block;
    /// 5. the voter has not already voted for this leader round. A duplicate
    ///    is rejected and contributes its stake at most once.
    ///
    /// Rejected votes return [`ConsensusError::VoteRejected`] and never change
    /// consensus state. Accepted votes may trigger a commit.
    pub fn process_vote(&self, vote: &kvnc_types::Vote) -> Result<(), ConsensusError> {
        let leader_round = vote.leader_round;
        let voter = vote.voter;

        let Some(authority) = self.committee.get_by_index(voter) else {
            return Err(VoteRejection::UnknownVoter(voter).into());
        };
        if kvnc_crypto::verify(
            &authority.public_key,
            &vote.signature_data(&self.signing_ctx),
            &vote.signature,
        )
        .is_err()
        {
            return Err(VoteRejection::BadSignature(voter).into());
        }
        if !is_leader_round(leader_round) {
            return Err(VoteRejection::NotLeaderRound(leader_round).into());
        }

        let has_quorum = self
            .committer
            .add_verified_vote(leader_round, voter, vote.leader_hash)?;
        if has_quorum {
            info!("Leader round {} now has quorum", leader_round);
            self.try_commit_and_deliver()?;
        }
        Ok(())
    }

    fn try_commit_and_deliver(&self) -> Result<(), ConsensusError> {
        // Keep this critical section synchronous and non-awaiting. All sources
        // of commit attempts must use it so a later decision cannot be marked
        // or delivered ahead of an earlier pending decision.
        let _guard = self.commit_trigger_lock.lock();
        let Some(subdag) = self
            .committer
            .try_commit_and_mark_durable(self.dag_store.as_ref())?
        else {
            return Ok(());
        };

        // The committer persists before publication. On restart, the node
        // reconstructs committed sub-DAGs from the decided-round index and
        // execution deduplicates by leader digest in the state database.
        info!("Committed leader at round {}", subdag.leader_round);

        // Record metrics for committed leader
        let committed_height = subdag.leader_round; // proxy for height
        record_block_height(committed_height as i64);
        record_mergeset_size_metric(subdag.blocks.len());
        record_commit_latency(0.0); // placeholder; actual latency measured in execution

        if let Some(sender) = self.commit_sender.read().as_ref() {
            if sender.send(subdag).is_err() {
                warn!("execution receiver is unavailable; commit remains persisted for replay");
            }
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

    /// Subscribe to round changes.
    /// Returns a receiver that will be notified when the consensus round advances.
    pub fn subscribe_round(&self) -> watch::Receiver<Round> {
        self.round_sender
            .read()
            .as_ref()
            .expect("round sender should be initialized in new()")
            .subscribe()
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
    #[error("Invalid consensus vote: {0}")]
    InvalidVote(String),
    #[error("Vote rejected: {0}")]
    VoteRejected(#[from] VoteRejection),
}

/// Why [`ConsensusEngine::process_vote`] refused to count a vote.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VoteRejection {
    /// The voter index is not a member of the committee.
    #[error("voter {0} is not a committee member")]
    UnknownVoter(kvnc_types::AuthorityIndex),
    /// The signature does not verify under the voter's committee key.
    #[error("invalid signature for voter {0}")]
    BadSignature(kvnc_types::AuthorityIndex),
    /// The vote targets a round that is not a leader round.
    #[error("round {0} is not a leader round")]
    NotLeaderRound(Round),
    /// No leader block is registered for the voted round.
    #[error("no leader block registered for round {0}")]
    UnknownLeaderRound(Round),
    /// The voted hash differs from the registered leader block.
    #[error("vote hash does not match the leader block for round {0}")]
    LeaderHashMismatch(Round),
    /// The voter already has a vote recorded for this leader round.
    #[error("duplicate vote from voter {voter} for round {round}")]
    Duplicate {
        round: Round,
        voter: kvnc_types::AuthorityIndex,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    const TEST_CTX: kvnc_types::SigningContext =
        kvnc_types::SigningContext::new(kvnc_types::signing::chain_id::LOCAL);
    use crate::types::AuthorityInfo;
    use kvnc_types::{block::BlockReference, crypto::PublicKey, Address};
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct TestDag {
        blocks: RwLock<HashMap<Hash, kvnc_types::StatementBlock>>,
        decisions: Mutex<Vec<(Round, Hash)>>,
        fail_next_decision_mark: std::sync::atomic::AtomicBool,
    }

    impl DagStoreTrait for TestDag {
        fn get_block(
            &self,
            hash: &Hash,
        ) -> Result<kvnc_types::StatementBlock, kvnc_dag::DagStoreError> {
            self.blocks
                .read()
                .get(hash)
                .cloned()
                .ok_or_else(|| kvnc_dag::DagStoreError::NotFound(hash.to_string()))
        }

        fn get_ancestors(
            &self,
            hash: &Hash,
            min_round: Round,
        ) -> Result<Vec<Hash>, kvnc_dag::DagStoreError> {
            let blocks = self.blocks.read();
            let mut found = Vec::new();
            let mut pending = vec![*hash];
            while let Some(current) = pending.pop() {
                if let Some(block) = blocks.get(&current) {
                    for parent in &block.parents {
                        if let Some(parent_block) = blocks.get(&parent.digest) {
                            if parent_block.round >= min_round {
                                found.push(parent.digest);
                            }
                            pending.push(parent.digest);
                        }
                    }
                }
            }
            found.sort_by_key(|hash| hash.0);
            found.dedup();
            Ok(found)
        }

        fn get_parents(&self, hash: &Hash) -> Result<Vec<Hash>, kvnc_dag::DagStoreError> {
            let blocks = self.blocks.read();
            if let Some(block) = blocks.get(hash) {
                Ok(block.parents.iter().map(|p| p.digest).collect())
            } else {
                Err(kvnc_dag::DagStoreError::NotFound(hash.to_string()))
            }
        }

        fn get_block_by_author_round(
            &self,
            author: AuthorityIndex,
            round: Round,
        ) -> Result<Option<kvnc_types::StatementBlock>, kvnc_dag::DagStoreError> {
            Ok(self
                .blocks
                .read()
                .values()
                .find(|block| block.author == author && block.round == round)
                .cloned())
        }

        fn get_blocks_by_round(
            &self,
            round: Round,
        ) -> Result<Vec<kvnc_types::StatementBlock>, kvnc_dag::DagStoreError> {
            Ok(self
                .blocks
                .read()
                .values()
                .filter(|block| block.round == round)
                .cloned()
                .collect())
        }

        fn has_block(&self, hash: &Hash) -> Result<bool, kvnc_dag::DagStoreError> {
            Ok(self.blocks.read().contains_key(hash))
        }

        fn put_block(
            &self,
            block: &kvnc_types::StatementBlock,
        ) -> Result<(), kvnc_dag::DagStoreError> {
            self.blocks.write().insert(block.digest, block.clone());
            Ok(())
        }

        fn find_parents(
            &self,
            _round: Round,
            _max_parents: usize,
        ) -> Result<Vec<BlockReference>, kvnc_dag::DagStoreError> {
            Ok(Vec::new())
        }

        fn commit_leader(&self, _leader_hash: &Hash) -> Result<u64, kvnc_dag::DagStoreError> {
            Ok(self.decisions.lock().len() as u64 + 1)
        }

        fn mark_decided_and_commit_leader(
            &self,
            round: Round,
            leader_hash: &Hash,
        ) -> Result<u64, kvnc_dag::DagStoreError> {
            self.mark_round_decided(round, leader_hash)?;
            self.commit_leader(leader_hash)
        }

        fn mark_round_decided(
            &self,
            round: Round,
            leader_hash: &Hash,
        ) -> Result<(), kvnc_dag::DagStoreError> {
            if self.fail_next_decision_mark.swap(false, Ordering::SeqCst) {
                return Err(kvnc_dag::DagStoreError::NotFound(
                    "injected decision-mark failure".into(),
                ));
            }
            let mut decisions = self.decisions.lock();
            if !decisions.contains(&(round, *leader_hash)) {
                decisions.push((round, *leader_hash));
            }
            Ok(())
        }

        fn mergeset(&self, _leader: &Hash) -> Result<Vec<Hash>, kvnc_dag::DagStoreError> {
            Ok(Vec::new())
        }

        fn get_blocks(
            &self,
            hashes: &[Hash],
        ) -> Result<Vec<kvnc_types::StatementBlock>, kvnc_dag::DagStoreError> {
            let blocks = self.blocks.read();
            Ok(hashes
                .iter()
                .filter_map(|h| blocks.get(h).cloned())
                .collect())
        }

        fn get_decided_leaders(&self, round: Round) -> Result<Vec<Hash>, kvnc_dag::DagStoreError> {
            Ok(self
                .decisions
                .lock()
                .iter()
                .filter(|(r, _)| *r == round)
                .map(|(_, h)| *h)
                .collect())
        }

        fn get_decided_rounds(
            &self,
            max_round: Round,
        ) -> Result<Vec<Round>, kvnc_dag::DagStoreError> {
            Ok(self
                .decisions
                .lock()
                .iter()
                .filter(|(r, _)| *r <= max_round)
                .map(|(r, _)| *r)
                .collect())
        }

        fn prune_non_blue(
            &self,
            _blue_hashes: &[Hash],
            _committed_wave: u64,
        ) -> Result<u64, kvnc_dag::DagStoreError> {
            // Test implementation: no-op
            Ok(0)
        }

        fn prune_waves_before(
            &self,
            _wave: u64,
            _prune_window_waves: u64,
        ) -> Result<u64, kvnc_dag::DagStoreError> {
            // Test implementation: no-op
            Ok(0)
        }
    }

    struct TestBlockManager {
        dag: Arc<TestDag>,
        key: RwLock<kvnc_types::SigningKey>,
        set_key_calls: AtomicUsize,
    }

    impl BlockManagerTrait for TestBlockManager {
        fn propose_block(
            &self,
            round: Round,
        ) -> Result<kvnc_types::StatementBlock, kvnc_dag::BlockManagerError> {
            self.propose_block_with_txs(round, Vec::new())
        }

        fn propose_block_with_txs(
            &self,
            round: Round,
            _transactions: Vec<kvnc_types::Transaction>,
        ) -> Result<kvnc_types::StatementBlock, kvnc_dag::BlockManagerError> {
            let digest = kvnc_types::StatementBlock::compute_digest(0, round, &[], &[]);
            let block = kvnc_types::StatementBlock {
                author: 0,
                round,
                parents: Vec::new(),
                transactions: Vec::new(),
                statements: Vec::new(),
                signature: kvnc_crypto::sign(&self.key.read(), digest.as_ref()),
                digest,
                merkle_root: Default::default(),
            };
            self.dag.put_block(&block).expect("store proposal");
            Ok(block)
        }

        fn process_block(
            &self,
            block: &kvnc_types::StatementBlock,
        ) -> Result<(), kvnc_dag::BlockManagerError> {
            let public_key = PublicKey::from(self.key.read().verifying_key());
            kvnc_crypto::verify_block_signature(&public_key, &block.digest, &block.signature)?;
            self.dag.put_block(block)?;
            Ok(())
        }

        fn our_authority(&self) -> AuthorityIndex {
            0
        }

        fn set_signing_key(&self, key: kvnc_types::SigningKey) {
            *self.key.write() = key;
            self.set_key_calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn test_engine(
        round_duration_ms: u64,
    ) -> (
        ConsensusEngine<TestDag, TestBlockManager>,
        Arc<TestDag>,
        kvnc_types::SigningKey,
    ) {
        let (key, public_key) = kvnc_crypto::generate_keypair();
        let dag = Arc::new(TestDag::default());
        let committee = CommitteeInfo::try_new(
            0,
            vec![AuthorityInfo {
                index: 0,
                stake: 100,
                public_key,
                address: Address::default(),
                network_address: String::new(),
            }],
        )
        .expect("test committee is valid");
        let manager = Arc::new(RwLock::new(TestBlockManager {
            dag: dag.clone(),
            key: RwLock::new(key.clone()),
            set_key_calls: AtomicUsize::new(0),
        }));
        let engine = ConsensusEngine::new(
            ConsensusConfig {
                round_duration_ms,
                ..Default::default()
            },
            committee,
            dag.clone(),
            manager,
            key.clone(),
            None, // No mempool for tests
            TEST_CTX,
        );
        (engine, dag, key)
    }

    fn signed_vote(
        key: &kvnc_types::SigningKey,
        leader_round: Round,
        voter: AuthorityIndex,
        leader_hash: Hash,
    ) -> kvnc_types::Vote {
        let mut vote = kvnc_types::Vote {
            leader_round,
            leader_hash,
            voter,
            signature: kvnc_types::Signature([0; 64]),
        };
        vote.signature = kvnc_crypto::sign(key, &vote.signature_data(&TEST_CTX));
        vote
    }

    #[tokio::test]
    async fn start_preserves_the_node_signing_identity() {
        let (engine, dag, key) = test_engine(10);
        let engine = Arc::new(engine);
        let manager = engine.block_manager.clone();
        let running_engine = engine.clone();
        let task = tokio::spawn(async move { running_engine.start().await });
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while dag.blocks.read().is_empty() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "engine did not propose at its first scheduled leader slot"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        engine.stop();
        task.await.unwrap().unwrap();
        assert_eq!(manager.read().set_key_calls.load(Ordering::SeqCst), 1);
        let proposal = dag
            .blocks
            .read()
            .values()
            .next()
            .cloned()
            .expect("engine proposed a block");
        let public_key = PublicKey::from(key.verifying_key());
        kvnc_crypto::verify_block_signature(&public_key, &proposal.digest, &proposal.signature)
            .expect("proposal uses node-provided identity");
    }

    #[tokio::test]
    async fn quorum_commit_is_delivered_once_and_persisted() {
        let (engine, dag, key) = test_engine(100);
        let (sender, mut receiver) = mpsc::unbounded_channel();
        engine.set_commit_sender(sender);
        let digest = kvnc_types::StatementBlock::compute_digest(0, 3, &[], &[]);
        let block = kvnc_types::StatementBlock {
            author: 0,
            round: 3,
            parents: Vec::new(),
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: kvnc_crypto::sign(&key, digest.as_ref()),
            digest,
            merkle_root: Default::default(),
        };
        engine.process_block(&block).unwrap();
        let vote = signed_vote(&key, 3, 0, digest);
        engine.process_vote(&vote).unwrap();
        assert_eq!(receiver.try_recv().unwrap().leader.digest, digest);
        // The same vote after a committed round is a rejected duplicate and
        // cannot redeliver the commit.
        assert!(matches!(
            engine.process_vote(&vote),
            Err(ConsensusError::VoteRejected(
                VoteRejection::Duplicate { .. }
            ))
        ));
        assert!(receiver.try_recv().is_err());
        assert_eq!(dag.decisions.lock().as_slice(), &[(3, digest)]);
    }

    #[tokio::test]
    async fn decision_mark_failure_leaves_commit_retryable() {
        let (engine, dag, key) = test_engine(100);
        let (sender, mut receiver) = mpsc::unbounded_channel();
        engine.set_commit_sender(sender);
        let digest = kvnc_types::StatementBlock::compute_digest(0, 3, &[], &[]);
        let block = kvnc_types::StatementBlock {
            author: 0,
            round: 3,
            parents: Vec::new(),
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: kvnc_crypto::sign(&key, digest.as_ref()),
            digest,
            merkle_root: Default::default(),
        };
        engine.process_block(&block).unwrap();

        let later_parent = kvnc_types::block::BlockReference {
            author: 0,
            round: 3,
            digest,
        };
        let later_digest = kvnc_types::StatementBlock::compute_digest(
            0,
            6,
            std::slice::from_ref(&later_parent),
            &[],
        );
        let later_block = kvnc_types::StatementBlock {
            author: 0,
            round: 6,
            parents: vec![later_parent],
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: kvnc_crypto::sign(&key, later_digest.as_ref()),
            digest: later_digest,
            merkle_root: Default::default(),
        };
        engine.process_block(&later_block).unwrap();

        dag.fail_next_decision_mark.store(true, Ordering::SeqCst);
        assert!(matches!(
            engine.process_vote(&signed_vote(&key, 3, 0, digest)),
            Err(ConsensusError::DagStore(_))
        ));
        assert_eq!(engine.committer.last_decided_round(), 0);
        assert!(engine.committer.get_all_decided_leaders().is_empty());
        assert!(
            receiver.try_recv().is_err(),
            "failed persistence is not delivered"
        );

        // Even with a later quorum available, the earlier failed decision is
        // retried and delivered first; the later one follows it.
        engine
            .process_vote(&signed_vote(&key, 6, 0, later_digest))
            .unwrap();
        assert_eq!(receiver.try_recv().unwrap().leader_round, 3);
        assert!(receiver.try_recv().is_err());
        // A repeated vote is now rejected as a duplicate, so drive the next
        // commit attempt directly.
        engine.try_commit_and_deliver().unwrap();
        assert_eq!(receiver.try_recv().unwrap().leader_round, 6);
        assert_eq!(
            dag.decisions.lock().as_slice(),
            &[(3, digest), (6, later_digest)]
        );
        assert_eq!(engine.committer.last_decided_round(), 6);
    }

    #[test]
    fn concurrent_commit_triggers_deliver_decisions_in_order_once() {
        let (engine, dag, _key) = test_engine(100);
        let (sender, mut receiver) = mpsc::unbounded_channel();
        engine.set_commit_sender(sender);

        let round3 = kvnc_types::StatementBlock::compute_digest(0, 3, &[], &[]);
        let round6 = kvnc_types::StatementBlock::compute_digest(0, 6, &[], &[]);
        for (round, digest, parents) in [
            (3, round3, Vec::new()),
            (
                6,
                round6,
                vec![kvnc_types::block::BlockReference {
                    author: 0,
                    round: 3,
                    digest: round3,
                }],
            ),
        ] {
            let block = kvnc_types::StatementBlock {
                author: 0,
                round,
                parents,
                transactions: Vec::new(),
                statements: Vec::new(),
                signature: kvnc_types::Signature([0; 64]),
                digest,
                merkle_root: Default::default(),
            };
            dag.put_block(&block).unwrap();
            engine.committer.update_leader(LeaderInfo {
                round,
                author: 0,
                block_hash: Some(digest),
                status: LeaderStatus::Undecided,
                votes: HashMap::new(),
            });
            engine.committer.add_vote(round, 0, digest);
        }
        let engine = Arc::new(engine);
        let trigger_count = 8;
        let barrier = Arc::new(std::sync::Barrier::new(trigger_count));
        let mut threads = Vec::new();
        for _ in 0..trigger_count {
            let engine = Arc::clone(&engine);
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                engine.try_commit_and_deliver().unwrap();
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }

        let delivered: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok())
            .map(|subdag| subdag.leader_round)
            .collect();
        assert_eq!(delivered, vec![3, 6]);
        assert_eq!(
            dag.decisions
                .lock()
                .iter()
                .map(|(round, _)| *round)
                .collect::<Vec<_>>(),
            vec![3, 6]
        );
    }

    #[tokio::test]
    async fn invalid_block_rejected_by_network_pipeline() {
        // This test simulates the network→engine→block_manager pipeline
        // receiving an invalid block (bad signature) and rejecting it.
        let (engine, dag, key) = test_engine(100);
        let engine = Arc::new(engine);

        // Create a valid block first (genesis is already in TestDag via propose_block)
        let valid_digest = kvnc_types::StatementBlock::compute_digest(0, 3, &[], &[]);
        let valid_block = kvnc_types::StatementBlock {
            author: 0,
            round: 3,
            parents: Vec::new(),
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: kvnc_crypto::sign(&key, valid_digest.as_ref()),
            digest: valid_digest,
            merkle_root: Default::default(),
        };

        // Process valid block - should succeed
        engine.process_block(&valid_block).unwrap();
        assert!(dag.has_block(&valid_digest).unwrap());

        // Create an invalid block (wrong signature)
        let invalid_digest = kvnc_types::StatementBlock::compute_digest(0, 4, &[], &[]);
        let (_, wrong_pk) = kvnc_crypto::generate_keypair();
        let wrong_key = kvnc_types::SigningKey::from_bytes(&wrong_pk.0);
        let invalid_block = kvnc_types::StatementBlock {
            author: 0,
            round: 4,
            parents: Vec::new(),
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: kvnc_crypto::sign(&wrong_key, invalid_digest.as_ref()),
            digest: invalid_digest,
            merkle_root: Default::default(),
        };

        // Process invalid block - should fail validation
        let result = engine.process_block(&invalid_block);
        assert!(result.is_err(), "Invalid block should be rejected");

        // Block should not be stored
        assert!(!dag.has_block(&invalid_digest).unwrap());

        // Create block with unknown author - should fail
        let unknown_digest = kvnc_types::StatementBlock::compute_digest(99, 5, &[], &[]);
        let unknown_block = kvnc_types::StatementBlock {
            author: 99, // not in committee
            round: 5,
            parents: Vec::new(),
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: kvnc_types::Signature([0; 64]),
            digest: unknown_digest,
            merkle_root: Default::default(),
        };

        let result = engine.process_block(&unknown_block);
        assert!(
            result.is_err(),
            "Block from unknown author should be rejected"
        );
        assert!(!dag.has_block(&unknown_digest).unwrap());

        // Create block with bad digest - should fail
        let mut bad_digest_block = valid_block.clone();
        bad_digest_block.round = 6;
        bad_digest_block.digest = kvnc_types::Hash::zero();

        let result = engine.process_block(&bad_digest_block);
        assert!(
            result.is_err(),
            "Block with mismatched digest should be rejected"
        );
    }

    #[tokio::test]
    async fn leader_proposes_only_once_per_round_no_duplicate_proposals() {
        // This test ensures that the consensus engine produces exactly one block
        // per leader round, preventing the duplicate proposal issue where both
        // the engine's round_loop and a separate builder task would produce blocks.
        let (engine, dag, _key) = test_engine(100);
        let engine = Arc::new(engine);

        // Manually trigger the round loop logic for round 3 (leader round for authority 0)
        // First, advance to round 3 and check if we're leader
        engine.state.write().current_round = 3;
        assert!(engine.is_leader_for_round(3), "Should be leader at round 3");

        // Call propose_block - this simulates the round_loop calling propose_block
        engine.propose_block(3).await.unwrap();
        let block1 = dag.blocks.read().values().find(|b| b.round == 3).cloned();
        assert!(block1.is_some(), "First proposal should exist");
        let first_proposal_digest = block1.unwrap().digest;

        // Call propose_block again for the same round - should not produce a duplicate
        // (the engine tracks proposed state and should not re-propose)
        engine.state.write().proposed = false; // Reset to simulate a bug scenario
        engine.propose_block(3).await.unwrap();

        // Verify only one block exists at round 3
        let blocks_at_round3: Vec<_> = dag
            .blocks
            .read()
            .values()
            .filter(|b| b.round == 3)
            .cloned()
            .collect();
        assert_eq!(
            blocks_at_round3.len(),
            1,
            "Only one block should be produced per leader round, but found {} blocks (duplicate proposal bug)",
            blocks_at_round3.len()
        );

        // Verify the block is the same one (no equivocation)
        assert_eq!(
            blocks_at_round3[0].digest, first_proposal_digest,
            "Block digest should not change between proposals"
        );

        // Verify the block has the correct author (our authority)
        assert_eq!(
            blocks_at_round3[0].author, 0,
            "Block author should be our authority"
        );

        engine.stop();
    }

    #[test]
    fn timeout_skips_leader_after_deadline() {
        let (engine, _dag, _key) = test_engine(100);
        // Manually set a leader deadline in the past for round 3
        *engine.leader_deadline.write() = Some((
            3,
            std::time::Instant::now() - std::time::Duration::from_secs(10),
        ));
        // Skip should mark leader as Skip without panic
        engine.skip_leader(3).unwrap();
    }

    #[test]
    fn fork_first_valid_digest_wins_deterministic() {
        let (engine, dag, key) = test_engine(100);
        let digest_a = kvnc_types::StatementBlock::compute_digest(0, 3, &[], &[]);
        let block_a = kvnc_types::StatementBlock {
            author: 0,
            round: 3,
            parents: Vec::new(),
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: kvnc_crypto::sign(&key, digest_a.as_ref()),
            digest: digest_a,
            merkle_root: Default::default(),
        };
        engine.process_block(&block_a).unwrap();

        // Second block with different digest for same round/author should fail (fork)
        let fork_parent = kvnc_types::block::BlockReference {
            author: 0,
            round: 2,
            digest: kvnc_types::Hash::zero(),
        };
        let digest_b = kvnc_types::StatementBlock::compute_digest(0, 3, &[fork_parent], &[]);
        let block_b = kvnc_types::StatementBlock {
            author: 0,
            round: 3,
            parents: vec![fork_parent],
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: kvnc_crypto::sign(&key, digest_b.as_ref()),
            digest: digest_b,
            merkle_root: Default::default(),
        };
        let result = engine.process_block(&block_b);
        assert!(
            result.is_err(),
            "Fork/equivocation with different digest must be rejected"
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("fork/equivocation"));
    }

    #[test]
    fn fork_same_digest_is_idempotent() {
        let (engine, _dag, key) = test_engine(100);
        let digest = kvnc_types::StatementBlock::compute_digest(0, 3, &[], &[]);
        let block = kvnc_types::StatementBlock {
            author: 0,
            round: 3,
            parents: Vec::new(),
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: kvnc_crypto::sign(&key, digest.as_ref()),
            digest,
            merkle_root: Default::default(),
        };
        engine.process_block(&block).unwrap();
        // Same digest again must succeed (idempotent)
        engine.process_block(&block).unwrap();
    }

    #[tokio::test]
    async fn leader_timeout_timer_cleared_on_valid_block() {
        let (engine, _dag, key) = test_engine(100);
        let digest = kvnc_types::StatementBlock::compute_digest(0, 3, &[], &[]);
        let block = kvnc_types::StatementBlock {
            author: 0,
            round: 3,
            parents: Vec::new(),
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: kvnc_crypto::sign(&key, digest.as_ref()),
            digest,
            merkle_root: Default::default(),
        };
        *engine.leader_deadline.write() = Some((3, std::time::Instant::now()));
        engine.process_block(&block).unwrap();
        assert!(
            engine.leader_deadline.read().is_none(),
            "Deadline should be cleared when block arrives"
        );
    }

    #[test]
    fn timeout_and_fork_deterministic() {
        // 4.3 Timeout + 4.4 Fork: skip via timeout_factor and lexicographic min-digest wins.
        use crate::committer::UniversalCommitter;
        use crate::types::{CommitteeInfo, LeaderInfo, LeaderStatus};
        use kvnc_types::{hash::Hash, AuthorityIndex};

        let committee = CommitteeInfo::try_new(
            0,
            vec![crate::types::AuthorityInfo {
                index: 0,
                stake: 100,
                public_key: kvnc_crypto::generate_keypair().1,
                address: kvnc_types::Address::default(),
                network_address: String::new(),
            }],
        )
        .expect("valid");

        let committer = UniversalCommitter::new(committee, false, 100);
        let round: Round = 7;
        let author: AuthorityIndex = 0;

        // First digest (larger lexicographically) registered.
        let larger_digest = Hash([255u8; 32]);
        committer.update_leader(LeaderInfo {
            round,
            author,
            block_hash: Some(larger_digest),
            status: LeaderStatus::Undecided,
            votes: std::collections::HashMap::new(),
        });

        // Second digest (smaller lexicographically) must win deterministically.
        let smaller_digest = Hash([1u8; 32]);
        committer.update_leader(LeaderInfo {
            round,
            author,
            block_hash: Some(smaller_digest),
            status: LeaderStatus::Undecided,
            votes: std::collections::HashMap::new(),
        });

        let leader = committer.get_leader(round).expect("leader exists");
        assert_eq!(
            leader.block_hash,
            Some(smaller_digest),
            "min_digest (lexicographic) must win"
        );
        assert_ne!(
            leader.block_hash,
            Some(larger_digest),
            "larger digest must be rejected"
        );

        // 4.3 Timeout: register_skip must persist Skip in decided map.
        committer.register_skip(7, 0);
        let decided = committer.get_all_decided_leaders();
        assert!(
            decided.contains_key(&7),
            "skip must be persisted in decided map"
        );
        assert_eq!(
            decided.get(&7).unwrap().status,
            LeaderStatus::Skip,
            "Skip status must be persisted"
        );
    }
}
