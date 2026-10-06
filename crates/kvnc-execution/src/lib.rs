//! State transition function – executes committed sub-DAGs and distributes rewards.
//!
//! Every time a `CommittedSubDag` is finalized by consensus:
//! 1. Native transactions and WASM calls inside the blocks are applied
//!    (placeholder — see `execute_committed_subdag`).
//! 2. The mining reward for the leader block is paid to the leader’s payout
//!    address — the balance is credited in the provided [`Storage`].
//! 3. Treasury vesting is advanced (inside `StakingState`).
//!
//! Contract entry points are dispatched through [`contracts`]: the native
//! typed-API path ([`contracts::execute_contract_call`]) and the wasmi path
//! ([`contracts::ContractRunner::execute_wasm_call`]) share one
//! [`contracts::ContractHost`] state model with commit-only-on-success
//! semantics.

#![deny(unsafe_code)]
// `ExecutionError` wraps `kvnc_storage`'s error enums, whose largest variants
// are redb errors (>128 bytes) we cannot shrink from this crate. Same
// allowance as the other storage-adjacent crates (kvnc-storage, kvnc-dag,
// kvnc-consensus, kvnc-mempool, kvnc-network).
#![allow(clippy::result_large_err)]

use kvnc_consensus::CommittedSubDag;
use kvnc_staking::{RewardOutcome, StakingError, StakingState};
use kvnc_storage::{Storage, StorageError};
use kvnc_types::Address;
use thiserror::Error;
use tracing::{debug, info};

pub mod contracts;
pub use contracts::{
    execute_contract_call, is_entry_point, ContractEvent, ContractHost, ContractRunner, WasmCall,
    ENTRY_POINTS,
};

#[derive(Error, Debug)]
pub enum ExecutionError {
    #[error("Staking error: {0}")]
    Staking(#[from] StakingError),
    /// The contract rejected the call (`ContractError::code()` codes).
    /// Nothing of the failed call was committed.
    #[error("Contract rejected: {0}")]
    Contract(kvnc_common::ContractError),
    /// Wasmi runtime failure (trap, out of gas, missing export, …) — also
    /// means nothing of the failed call was committed.
    #[error("Runtime error: {0}")]
    Runtime(#[from] kvnc_runtime::RuntimeError),
    #[error("Storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("State store error: {0}")]
    StateStore(#[from] kvnc_storage::StateStoreError),
    #[error("Execution failed: {0}")]
    Other(String),
}

// `ContractError` is `no_std`-friendly and does not implement
// `std::error::Error`, so thiserror cannot generate the `From` impl.
impl From<kvnc_common::ContractError> for ExecutionError {
    fn from(err: kvnc_common::ContractError) -> Self {
        Self::Contract(err)
    }
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
    /// the author of the leader block receives the block reward, **credited
    /// into `storage`** (its payout address account balance) in the same call.
    ///
    /// Ordering / atomicity notes:
    ///
    /// - The storage write transaction is opened *before* the staking
    ///   accounting: if `on_leader_committed` fails (unknown authority) the
    ///   transaction is dropped and storage is untouched.
    /// - The staking state (committed_leader_height, total_mining_issued,
    ///   treasury vesting) is persisted in the same transaction.
    pub fn execute_committed_subdag(
        &mut self,
        subdag: &CommittedSubDag,
        storage: &Storage,
    ) -> Result<ExecutionResult, ExecutionError> {
        // 1. Apply transactions contained in the blocks (placeholder)
        let mut txs_applied = 0;
        for block in &subdag.blocks {
            txs_applied += block.transactions.len();
            // TODO: real native + WASM execution
        }

        // 2. Pay the mining reward to the leader of this sub-DAG.
        let txn = storage.begin_write()?;
        let reward = self.staking.on_leader_committed(subdag.leader_author)?;
        storage
            .state()
            .add_balance(&txn, &reward.recipient, reward.amount)?;
        // Persist staking state (height, total_mining_issued, treasury vesting)
        storage.state().save_staking_state(&txn, &self.staking)?;
        txn.commit().map_err(StorageError::Commit)?;

        info!(
            target: "kvnc-execution",
            "Committed leader round={} author={} reward={} KVNC (height={}) paid to {:?}",
            subdag.leader_round,
            subdag.leader_author,
            reward.amount / kvnc_staking::ONE_KVNC,
            reward.height,
            reward.recipient,
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

    /// Claim available treasury funds and credit them to the treasury address.
    ///
    /// This can be called by an operator / governance process to move vested
    /// treasury funds into circulation. The treasury address must have been
    /// configured at genesis via `init_treasury`.
    ///
    /// Returns the amount actually claimed (may be less than requested if
    /// not enough has vested).
    pub fn claim_treasury(
        &mut self,
        amount: u64,
        storage: &Storage,
    ) -> Result<u64, ExecutionError> {
        let treasury_address = self
            .staking
            .treasury_address()
            .ok_or_else(|| ExecutionError::Other("treasury not configured".into()))?;

        let claimable = self.staking.treasury_claimable();
        if claimable == 0 {
            return Ok(0);
        }

        let to_claim = amount.min(claimable);
        let claimed = self.staking.claim_treasury(to_claim)?;

        if claimed == 0 {
            return Ok(0);
        }

        // Credit the treasury address balance
        let txn = storage.begin_write()?;
        storage
            .state()
            .add_balance(&txn, &treasury_address, claimed)?;
        // Persist updated staking state (claimed counter)
        storage.state().save_staking_state(&txn, &self.staking)?;
        txn.commit().map_err(StorageError::Commit)?;

        info!(
            target: "kvnc-execution",
            "Treasury claim: {} KVNC credited to {:?}",
            claimed / kvnc_staking::ONE_KVNC,
            treasury_address
        );

        Ok(claimed)
    }
}

impl Default for ExecutionContext {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvnc_types::{Signature, StatementBlock};

    fn sample_block(author: u16) -> StatementBlock {
        StatementBlock {
            author,
            round: 1,
            parents: Vec::new(),
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: Signature([0u8; 64]),
            digest: kvnc_types::Hash::zero(),
        }
    }

    fn read_balance(storage: &Storage, address: &Address) -> u64 {
        let txn = storage.begin_read().expect("read txn");
        storage
            .state()
            .get_account_or_default(&txn, address)
            .expect("account")
            .balance
    }

    // (c) execute_committed_subdag must credit the leader's payout address
    // with the block reward in persistent storage.
    #[test]
    fn committed_subdag_credits_leader_payout_address() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().join("test.redb")).expect("storage");

        let mut ctx = ExecutionContext::new();
        let validator = Address([1u8; 32]);
        let payout = Address([9u8; 32]);
        ctx.staking
            .join_validator(
                validator,
                kvnc_staking::MIN_VALIDATOR_STAKE,
                0,
                Some(payout),
            )
            .expect("join validator");

        let subdag = CommittedSubDag {
            leader: sample_block(0),
            blocks: vec![sample_block(0)],
            leader_round: 1,
            leader_author: 0,
        };

        let first = ctx
            .execute_committed_subdag(&subdag, &storage)
            .expect("first commit");
        let reward = first.reward.expect("reward paid");
        assert_eq!(
            reward.recipient, payout,
            "reward must go to the payout address"
        );
        assert_eq!(reward.height, 0);
        assert!(reward.amount > 0);
        assert_eq!(first.txs_applied, 0, "no transactions in the sample blocks");

        // The payout address actually HOLDS the reward in storage …
        assert_eq!(read_balance(&storage, &payout), reward.amount);
        // … and nothing was credited anywhere else.
        assert_eq!(read_balance(&storage, &validator), 0);
        assert_eq!(read_balance(&storage, &Address([7u8; 32])), 0);

        // A second commit pays the next height's reward on top.
        let second = ctx
            .execute_committed_subdag(&subdag, &storage)
            .expect("second commit");
        let reward2 = second.reward.expect("second reward");
        assert_eq!(reward2.height, reward.height + 1, "height counter advances");
        assert_eq!(
            read_balance(&storage, &payout),
            reward.amount + reward2.amount,
            "rewards accumulate on the payout address"
        );
        assert_eq!(
            second.circulating_supply,
            first.circulating_supply + reward2.amount
        );

        // An unknown leader authority fails BEFORE any balance is credited.
        let bad = CommittedSubDag {
            leader: sample_block(7),
            blocks: vec![sample_block(7)],
            leader_round: 2,
            leader_author: 7,
        };
        let err = ctx
            .execute_committed_subdag(&bad, &storage)
            .expect_err("unregistered authority must fail");
        assert!(matches!(err, ExecutionError::Staking(_)), "{err:?}");
        assert_eq!(
            read_balance(&storage, &payout),
            reward.amount + reward2.amount,
            "failed commit credits nothing"
        );
    }

    #[test]
    fn treasury_claim_works() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().join("test.redb")).expect("storage");

        let mut ctx = ExecutionContext::new();
        let treasury_addr = Address([8u8; 32]);
        ctx.init_treasury(treasury_addr);

        // Register a validator to be the leader
        let validator = Address([1u8; 32]);
        let payout = Address([9u8; 32]);
        ctx.staking
            .join_validator(
                validator,
                kvnc_staking::MIN_VALIDATOR_STAKE,
                0,
                Some(payout),
            )
            .expect("join validator");

        // Manually advance treasury vesting for testing (simulate 2 years elapsed)
        // by directly setting vested amount on the treasury state.
        if let Some(ref mut treasury) = ctx.staking.treasury {
            treasury.vested = 2 * kvnc_staking::TREASURY_ANNUAL;
        }

        // Treasury should have 2M KVNC vested and claimable
        let claimable = ctx.staking.treasury_claimable();
        assert_eq!(claimable, 2 * kvnc_staking::TREASURY_ANNUAL);

        // Claim 1M KVNC
        let claimed = ctx
            .claim_treasury(kvnc_staking::TREASURY_ANNUAL, &storage)
            .expect("claim treasury");
        assert_eq!(claimed, kvnc_staking::TREASURY_ANNUAL);

        // Verify treasury address was credited
        assert_eq!(read_balance(&storage, &treasury_addr), claimed);

        // Verify staking state updated
        assert_eq!(ctx.staking.treasury.as_ref().unwrap().claimed, claimed);
        assert_eq!(
            ctx.staking.treasury_claimable(),
            kvnc_staking::TREASURY_ANNUAL
        );

        // Claim the rest
        let claimed2 = ctx
            .claim_treasury(kvnc_staking::TREASURY_ANNUAL, &storage)
            .expect("claim treasury again");
        assert_eq!(claimed2, kvnc_staking::TREASURY_ANNUAL);
        assert_eq!(
            read_balance(&storage, &treasury_addr),
            2 * kvnc_staking::TREASURY_ANNUAL
        );

        // Double claim beyond vested amount returns 0
        let claimed3 = ctx
            .claim_treasury(kvnc_staking::TREASURY_ANNUAL, &storage)
            .expect("claim treasury third time");
        assert_eq!(claimed3, 0);

        // Claim without treasury configured fails
        let mut ctx2 = ExecutionContext::new();
        let err = ctx2
            .claim_treasury(kvnc_staking::TREASURY_ANNUAL, &storage)
            .expect_err("claim without treasury must fail");
        assert!(matches!(err, ExecutionError::Other(_)));
    }
}
