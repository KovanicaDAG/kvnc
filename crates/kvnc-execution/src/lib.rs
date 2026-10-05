//! State transition function – executes committed sub-DAGs and distributes rewards.
//!
//! Every time a `CommittedSubDag` is finalized by consensus:
//! 1. Native transactions and WASM calls inside the blocks are applied.
//! 2. The mining reward for the leader block is paid to the leader’s payout address.
//! 3. Treasury vesting is advanced.

#![deny(unsafe_code)]

use kvnc_consensus::CommittedSubDag;
use kvnc_staking::{RewardOutcome, StakingError, StakingState};
use kvnc_types::Address;
use thiserror::Error;
use tracing::{debug, info};

#[derive(Error, Debug)]
pub enum ExecutionError {
    #[error("Staking error: {0}")]
    Staking(#[from] StakingError),
    #[error("Execution failed: {0}")]
    Other(String),
}

/// Result of executing one committed sub-DAG.
#[derive(Debug)]
pub struct ExecutionResult {
    /// Reward that was paid for the leader block (if any).
    pub reward: Option<RewardOutcome>,
    /// Number of transactions that were applied.
    pub txs_applied: usize,
    /// New circulating supply after this execution.
    pub circulating_supply: u64,
}

/// Minimal execution context (will later hold the full account state / Merkle tree).
pub struct ExecutionContext {
    pub staking: StakingState,
    // TODO: account balances, contract storage, etc.
}

impl ExecutionContext {
    pub fn new() -> Self {
        Self {
            staking: StakingState::new(),
        }
    }

    /// Configure treasury address at genesis.
    pub fn init_treasury(&mut self, treasury_address: Address) {
        self.staking.init_treasury(treasury_address);
    }

    /// Execute a committed sub-DAG produced by consensus.
    ///
    /// This is the main entry point that ties the DAG commit path to tokenomics:
    /// the author of the leader block receives the block reward.
    pub fn execute_committed_subdag(
        &mut self,
        subdag: &CommittedSubDag,
    ) -> Result<ExecutionResult, ExecutionError> {
        // 1. Apply transactions contained in the blocks (placeholder)
        let mut txs_applied = 0;
        for block in &subdag.blocks {
            txs_applied += block.transactions.len();
            // TODO: real native + WASM execution
        }

        // 2. Pay the mining reward to the leader of this sub-DAG
        let reward = self.staking.on_leader_committed(subdag.leader_author)?;

        info!(
            target: "kvnc-execution",
            "Committed leader round={} author={} reward={} KVNC (height={})",
            subdag.leader_round,
            subdag.leader_author,
            reward.amount / kvnc_staking::ONE_KVNC,
            reward.height
        );

        debug!(
            target: "kvnc-execution",
            "Applied {} txs, circulating supply now {}",
            txs_applied,
            self.staking.circulating_supply()
        );

        Ok(ExecutionResult {
            reward: Some(reward),
            txs_applied,
            circulating_supply: self.staking.circulating_supply(),
        })
    }
}

impl Default for ExecutionContext {
    fn default() -> Self {
        Self::new()
    }
}
