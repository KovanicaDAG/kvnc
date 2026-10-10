//! State transition function – executes committed sub-DAGs and distributes rewards.
//!
//! Every time a `CommittedSubDag` is finalized by consensus:
//! 1. Native transactions and WASM calls inside the blocks are applied
//!    in topological order (GHOSTDAG linearization).
//! 2. The mining reward for the leader block is paid to the leader's payout
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
use kvnc_runtime::ExecutionConfig;
use kvnc_staking::{RewardOutcome, StakingError, StakingState};
use kvnc_storage::{
    address_to_bytes, state_store::Account, tables, BincodeSerialize, Storage, StorageError,
};
use kvnc_types::signing::{chain_id, SigningContext};
use kvnc_types::{Address, Transaction, TransactionKind};
use redb::{ReadableTable, TableDefinition, WriteTransaction};
use thiserror::Error;
use tracing::{debug, info};

pub mod contracts;
pub use contracts::{
    execute_contract_call, execute_contract_call_in_txn, is_entry_point, ContractEvent,
    ContractHost, ContractRunner, WasmCall, ENTRY_POINTS,
};

/// Table tracking executed sub-DAG leaders (idempotency).
const EXECUTED_SUBDAGS: TableDefinition<[u8; 32], u8> =
    TableDefinition::new("execution_committed_subdags");

/// Table for transaction receipts (committed_leader_height -> receipts).
const TX_RECEIPTS: TableDefinition<u64, Vec<u8>> = TableDefinition::new("tx_receipts");

/// Accrued, unclaimed delegator rewards: `delegator || validator` -> amount.
/// Kept outside `StakingState` so its persisted (bincode) format is unchanged.
const PENDING_REWARDS: TableDefinition<[u8; 64], u64> =
    TableDefinition::new("staking_pending_rewards");

/// Processed double-sign evidence ids (`StakingState::evidence_id`).
const PROCESSED_EVIDENCE: TableDefinition<[u8; 40], u8> =
    TableDefinition::new("staking_processed_evidence");

/// Supply accounting counters kept by execution (e.g. `"slashed_to_treasury"`).
const SUPPLY_COUNTERS: TableDefinition<&str, u64> = TableDefinition::new("supply_counters");

fn reward_key(delegator: &Address, validator: &Address) -> [u8; 64] {
    let mut k = [0u8; 64];
    k[..32].copy_from_slice(&delegator.0);
    k[32..].copy_from_slice(&validator.0);
    k
}

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
    #[error("Transaction validation failed: {0}")]
    Validation(String),
    #[error("Insufficient balance for transaction fee")]
    InsufficientFee,
    #[error("Nonce mismatch: expected {expected}, got {actual}")]
    NonceMismatch { expected: u64, actual: u64 },
    #[error("Execution failed: {0}")]
    Other(String),
    /// The state sync gate rejected the sub-DAG: this node must state-sync
    /// first. Nothing was written to the store.
    #[error("degraded: needs state sync")]
    NeedsStateSync,
}

/// Startup sync signals, filled in by kvnc-node (contract agreed with
/// Network). Consensus's `is_genesis_subdag` is passed separately per call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StartupSyncState {
    /// The node's store was empty when it started.
    pub store_was_empty: bool,
    /// The network is ahead of this node.
    pub network_ahead: bool,
}

impl StartupSyncState {
    /// Execute only if `!network_ahead && (!store_was_empty || is_genesis_subdag)`.
    pub fn allows(&self, is_genesis_subdag: bool) -> bool {
        !self.network_ahead && (!self.store_was_empty || is_genesis_subdag)
    }
}

// `ContractError` is `no_std`-friendly and does not implement
// `std::error::Error`, so thiserror cannot generate the `From` impl.
impl From<kvnc_common::ContractError> for ExecutionError {
    fn from(err: kvnc_common::ContractError) -> Self {
        Self::Contract(err)
    }
}

impl From<redb::TableError> for ExecutionError {
    fn from(err: redb::TableError) -> Self {
        Self::Other(format!("Table error: {err}"))
    }
}

impl From<redb::StorageError> for ExecutionError {
    fn from(err: redb::StorageError) -> Self {
        Self::Other(format!("Storage error: {err}"))
    }
}

impl From<bincode::Error> for ExecutionError {
    fn from(err: bincode::Error) -> Self {
        Self::Other(format!("Bincode error: {err}"))
    }
}

/// Transaction execution outcome.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TransactionReceipt {
    /// Transaction hash.
    pub tx_hash: kvnc_types::hash::Hash,
    /// Whether the transaction succeeded.
    pub success: bool,
    /// Gas used (for WASM calls; 0 for native transfers).
    pub gas_used: u64,
    /// Error message if failed.
    pub error: Option<String>,
    /// Events emitted during execution.
    pub events: Vec<ContractEvent>,
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
    /// Receipts for all executed transactions in this sub-DAG.
    pub receipts: Vec<TransactionReceipt>,
}

/// Minimal execution context (will later hold the full account state / Merkle tree).
pub struct ExecutionContext {
    pub staking: StakingState,
    /// Contract runner for WASM execution (shared module cache).
    pub contract_runner: ContractRunner,
    /// Event publisher for transaction logs.
    pub log_publisher: Option<Box<dyn LogPublisher>>,
    /// Signature format v1 context (`chain_id`) every transaction signature
    /// is verified against. Built from local configuration only.
    signing_ctx: SigningContext,
}

/// Trait for publishing transaction execution logs.
/// Implemented by the node to forward logs to WebSocket subscribers.
pub trait LogPublisher: Send + Sync {
    fn publish_logs(&self, receipts: &[TransactionReceipt]);
}

impl ExecutionContext {
    /// **Dev/test only.** Context for `chain_id::LOCAL` (1337). On a real
    /// network every signature (signed for mainnet/testnet/devnet) is
    /// rejected — it fails closed. Nodes must use
    /// [`ExecutionContext::with_signing_context`].
    pub fn new() -> Self {
        Self::with_signing_context(SigningContext::new(chain_id::LOCAL))
    }

    /// Execution context verifying transaction signatures (format v1,
    /// `KUNA/tx/v1` domain tag) against `ctx.chain_id`. `ctx` must come from
    /// the node's own configuration, never from a network message.
    pub fn with_signing_context(ctx: SigningContext) -> Self {
        Self {
            staking: StakingState::new(),
            contract_runner: ContractRunner::new(),
            log_publisher: None,
            signing_ctx: ctx,
        }
    }

    /// The signing context transactions are verified against.
    pub fn signing_context(&self) -> SigningContext {
        self.signing_ctx
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
    /// - The storage write transaction is opened before candidate staking
    ///   accounting: if the transition fails (unknown authority), the
    ///   transaction is dropped and storage is untouched.
    /// - The candidate staking state, payout, and executed-leader marker are
    ///   written in the same transaction. The candidate is published in memory
    ///   only after that transaction commits.
    pub fn execute_committed_subdag(
        &mut self,
        subdag: &CommittedSubDag,
        storage: &Storage,
    ) -> Result<ExecutionResult, ExecutionError> {
        self.execute_committed_subdag_with_commit(subdag, storage, |txn| {
            txn.commit()
                .map_err(StorageError::Commit)
                .map_err(ExecutionError::from)
        })
    }

    /// [`Self::execute_committed_subdag`] behind the state sync gate.
    ///
    /// If `startup.allows(is_genesis_subdag)` is false this returns
    /// [`ExecutionError::NeedsStateSync`] before opening a write transaction:
    /// no staking save, no executed-leader mark, no receipts, and the
    /// in-memory staking state is untouched. Otherwise it is exactly
    /// `execute_committed_subdag` (idempotent restart replay included).
    pub fn execute_committed_subdag_gated(
        &mut self,
        subdag: &CommittedSubDag,
        storage: &Storage,
        startup: &StartupSyncState,
        is_genesis_subdag: bool,
    ) -> Result<ExecutionResult, ExecutionError> {
        if !startup.allows(is_genesis_subdag) {
            return Err(ExecutionError::NeedsStateSync);
        }
        self.execute_committed_subdag(subdag, storage)
    }

    fn execute_committed_subdag_with_commit(
        &mut self,
        subdag: &CommittedSubDag,
        storage: &Storage,
        commit: impl FnOnce(WriteTransaction) -> Result<(), ExecutionError>,
    ) -> Result<ExecutionResult, ExecutionError> {
        // Transactions mutate `self.staking` in place. If anything fails
        // before the storage transaction commits, the in-memory staking state
        // must return to exactly what is persisted, so snapshot it here.
        let pre_subdag_staking = snapshot_staking(&self.staking);
        let result = self.execute_committed_subdag_inner(subdag, storage, commit);
        if result.is_err() {
            self.staking = pre_subdag_staking;
        }
        result
    }

    fn execute_committed_subdag_inner(
        &mut self,
        subdag: &CommittedSubDag,
        storage: &Storage,
        commit: impl FnOnce(WriteTransaction) -> Result<(), ExecutionError>,
    ) -> Result<ExecutionResult, ExecutionError> {
        // 1. Check idempotency first (before any execution)
        let mut txn = storage.begin_write()?;
        {
            let table = txn
                .open_table(EXECUTED_SUBDAGS)
                .map_err(StorageError::Table)?;
            if table
                .get(subdag.leader.digest.0)
                .map_err(StorageError::Storage)?
                .is_some()
            {
                return Ok(ExecutionResult {
                    reward: None,
                    txs_applied: 0,
                    circulating_supply: self.staking.circulating_supply(),
                    receipts: Vec::new(),
                });
            }
        }

        // 2. Execute all transactions in the committed sub-DAG
        let mut receipts = Vec::new();
        let mut txs_applied = 0;
        let committed_height = self.staking.committed_leader_height;

        for block in &subdag.blocks {
            for tx in &block.transactions {
                let receipt = self.execute_transaction(tx, storage, &mut txn, committed_height)?;
                receipts.push(receipt);
                txs_applied += 1;
            }
        }

        // 3. Pay the mining reward to the leader of this sub-DAG.
        // Perform the transition on a candidate state. The live in-memory state
        // is published only after all related storage writes commit successfully.
        let mut candidate_staking = snapshot_staking(&self.staking);
        let leader = candidate_staking
            .authority_validator(subdag.leader_author)
            .cloned()
            .ok_or(StakingError::NotValidator)?;
        let reward = candidate_staking.on_leader_committed(subdag.leader_author)?;
        // Split the reward: commission + the validator's own pro-rata share
        // go straight to its payout address; delegator shares accrue per
        // (delegator, validator) and are paid by ClaimRewards.
        let shares =
            candidate_staking.reward_share(leader.address, reward.amount, leader.commission_bps);
        {
            let mut pending = txn.open_table(PENDING_REWARDS)?;
            for (addr, amount) in &shares {
                if *addr != leader.address && *amount > 0 {
                    let key = reward_key(addr, &leader.address);
                    let cur = pending.get(key)?.map(|v| v.value()).unwrap_or(0);
                    pending.insert(key, cur.saturating_add(*amount))?;
                }
            }
        }
        let validator_part: u64 = shares
            .iter()
            .filter(|(a, _)| *a == leader.address)
            .map(|(_, x)| *x)
            .sum();
        storage
            .state()
            .add_balance(&txn, &reward.recipient, validator_part)?;
        // Pay out matured unbonding entries (UNBONDING_ROUNDS elapsed) via
        // withdraw_unbonded, in deterministic (delegator, validator) order.
        let mut ready: Vec<(Address, Address)> = candidate_staking
            .unbonding_ready()
            .iter()
            .map(|e| (e.delegator, e.validator))
            .collect();
        ready.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
        for (delegator, validator) in ready {
            let amount = candidate_staking.withdraw_unbonded(delegator, validator)?;
            storage.state().add_balance(&txn, &delegator, amount)?;
        }
        // Persist staking state (height, total_mining_issued, treasury vesting)
        storage
            .state()
            .save_staking_state(&txn, &candidate_staking)?;
        // Also increment the consensus store's committed leader height for RPC visibility
        storage
            .consensus()
            .increment_committed_leader_height(&txn)
            .map_err(StorageError::ConsensusStore)?;

        // 4. Persist transaction receipts
        let receipts_bytes = bincode::serialize(&receipts)
            .map_err(|e| ExecutionError::Other(format!("receipt serialization: {e}")))?;
        txn.open_table(TX_RECEIPTS)
            .map_err(StorageError::Table)?
            .insert(committed_height, receipts_bytes)
            .map_err(StorageError::Storage)?;

        // 5. Mark sub-DAG as executed
        txn.open_table(EXECUTED_SUBDAGS)
            .map_err(StorageError::Table)?
            .insert(subdag.leader.digest.0, 1)
            .map_err(StorageError::Storage)?;

        commit(txn)?;
        self.staking = candidate_staking;

        // Publish transaction logs to WebSocket subscribers
        if let Some(publisher) = &self.log_publisher {
            publisher.publish_logs(&receipts);
        }

        info!(
            target: "kvnc-execution",
            "Committed leader round={} author={} reward={} {} (height={}) paid to {:?}",
            subdag.leader_round,
            subdag.leader_author,
            reward.amount / kvnc_staking::ONE_KVNC,
            kvnc_staking::TICKER,
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
            receipts,
        })
    }

    /// Execute a single transaction within a committed sub-DAG.
    /// Returns a TransactionReceipt with the execution outcome.
    fn execute_transaction(
        &mut self,
        tx: &Transaction,
        storage: &Storage,
        txn: &mut WriteTransaction,
        committed_height: u64,
    ) -> Result<TransactionReceipt, ExecutionError> {
        // Every transaction must carry a valid Ed25519 signature by its
        // sender over the signing hash. There is deliberately no bypass:
        // tests sign with real keypairs (see `tests::signed_tx`).
        if !tx.verify_signature(&self.signing_ctx) {
            return Ok(TransactionReceipt {
                tx_hash: tx.hash,
                success: false,
                gas_used: 0,
                error: Some("Invalid signature".to_string()),
                events: Vec::new(),
            });
        }

        // Load sender account and validate nonce, deduct fee, increment nonce in one table access
        let sender_key = address_to_bytes(&tx.sender);
        let mut sender_account = {
            let table = txn.open_table(tables::ACCOUNTS)?;
            let value = table.get(sender_key)?;
            value
                .map(|v| Account::from_bytes(&v.value()).unwrap_or_default())
                .unwrap_or_default()
        };

        if sender_account.nonce != tx.nonce {
            return Ok(TransactionReceipt {
                tx_hash: tx.hash,
                success: false,
                gas_used: 0,
                error: Some(format!(
                    "Nonce mismatch: expected {}, got {}",
                    sender_account.nonce, tx.nonce
                )),
                events: Vec::new(),
            });
        }

        // Check fee balance
        if sender_account.balance < tx.fee {
            return Ok(TransactionReceipt {
                tx_hash: tx.hash,
                success: false,
                gas_used: 0,
                error: Some("Insufficient balance for fee".to_string()),
                events: Vec::new(),
            });
        }

        // Deduct fee upfront and increment nonce
        sender_account.balance = sender_account.balance.saturating_sub(tx.fee);
        sender_account.nonce = sender_account.nonce.saturating_add(1);

        // Save updated sender account
        {
            let mut table = txn.open_table(tables::ACCOUNTS)?;
            table.insert(sender_key, sender_account.to_bytes()?)?;
        }

        // ---- Per-transaction savepoint -------------------------------------
        //
        // Fee and nonce are CHARGED even if the body fails (they were written
        // above): this pays for inclusion and prevents replaying the same
        // signed transaction. Everything the body does is rolled back on
        // failure: the staking state is restored from a snapshot and every
        // storage row the body may write is restored from its pre-image.
        // (redb cannot take a savepoint inside a dirty write transaction, so
        // the savepoint is an explicit undo journal.)
        let staking_savepoint = snapshot_staking(&self.staking);
        let undo = AccountUndo::capture(txn, &touched_accounts(tx))?;

        // Execute based on transaction kind
        let result = match &tx.kind {
            TransactionKind::Transfer { to, amount } => {
                self.execute_transfer(txn, storage, &tx.sender, *to, *amount)
            }
            TransactionKind::Stake { amount } => {
                self.execute_stake(txn, storage, &tx.sender, *amount)
            }
            TransactionKind::Unstake { amount } => {
                self.execute_unstake(txn, storage, &tx.sender, *amount)
            }
            TransactionKind::Delegate { validator, amount } => {
                self.execute_delegate(txn, storage, &tx.sender, *validator, *amount)
            }
            TransactionKind::ClaimRewards { validator } => {
                self.execute_claim_rewards(txn, storage, &tx.sender, *validator)
            }
            TransactionKind::Deploy { code } => {
                self.execute_deploy(txn, storage, &tx.sender, code.clone())
            }
            TransactionKind::Call {
                contract,
                value,
                method,
                args,
                gas_limit,
            } => self.execute_call(
                txn,
                storage,
                &tx.sender,
                *contract,
                *value,
                method,
                args,
                *gas_limit,
                committed_height,
            ),
        };

        let (success, gas_used, error, events) = match result {
            Ok(events) => (true, 0, None, events),
            Err(e) => {
                self.staking = staking_savepoint;
                undo.restore(txn)?;
                (false, 0, Some(e.to_string()), Vec::new())
            }
        };

        // For successful calls that used gas (WASM), we'd track gas_used here
        // For now, native transactions use 0 gas

        Ok(TransactionReceipt {
            tx_hash: tx.hash,
            success,
            gas_used,
            error,
            events,
        })
    }

    /// Execute a native KUNA transfer.
    fn execute_transfer(
        &self,
        txn: &mut WriteTransaction,
        _storage: &Storage,
        from: &Address,
        to: Address,
        amount: u64,
    ) -> Result<Vec<ContractEvent>, ExecutionError> {
        if from == &to {
            return Ok(Vec::new()); // Self-transfer is a no-op
        }

        // Check sender has sufficient balance (after fee deduction) and debit sender
        let from_key = address_to_bytes(from);
        let mut sender_account = {
            let table = txn.open_table(tables::ACCOUNTS)?;
            let value = table.get(from_key)?;
            value
                .map(|v| Account::from_bytes(&v.value()).unwrap_or_default())
                .unwrap_or_default()
        };
        if sender_account.balance < amount {
            return Err(ExecutionError::Validation(
                "Insufficient balance for transfer".to_string(),
            ));
        }
        sender_account.balance = sender_account.balance.saturating_sub(amount);
        {
            let mut table = txn.open_table(tables::ACCOUNTS)?;
            table.insert(from_key, sender_account.to_bytes()?)?;
        }

        // Credit recipient
        let to_key = address_to_bytes(&to);
        let mut recipient_account = {
            let table = txn.open_table(tables::ACCOUNTS)?;
            let value = table.get(to_key)?;
            value
                .map(|v| Account::from_bytes(&v.value()).unwrap_or_default())
                .unwrap_or_default()
        };
        recipient_account.balance = recipient_account.balance.saturating_add(amount);
        {
            let mut table = txn.open_table(tables::ACCOUNTS)?;
            table.insert(to_key, recipient_account.to_bytes()?)?;
        }

        // Emit transfer event
        let events = vec![ContractEvent {
            topic: kvnc_common::events::TOKEN_TRANSFER.to_vec(),
            data: bincode::serialize(&(from, &to, amount))
                .map_err(|e| ExecutionError::Other(e.to_string()))?,
        }];

        Ok(events)
    }

    /// Execute a stake transaction: register the sender as a validator
    /// (`join_validator`, rejects duplicates) or, if it already is an active
    /// validator, bond additional self-stake (`bond_validator`).
    fn execute_stake(
        &mut self,
        txn: &mut WriteTransaction,
        storage: &Storage,
        from: &Address,
        amount: u64,
    ) -> Result<Vec<ContractEvent>, ExecutionError> {
        let account = storage.state().get_account_or_default_write(txn, from)?;
        if account.balance < amount {
            return Err(ExecutionError::Validation(
                "Insufficient balance for stake".to_string(),
            ));
        }
        let is_validator = self
            .staking
            .validators
            .iter()
            .any(|v| v.address == *from && v.active);
        if is_validator {
            self.staking.bond_validator(*from, amount)?;
        } else {
            // Address == Ed25519 public key (unchanged address format).
            let pk = kvnc_types::PublicKey(from.0);
            self.staking
                .join_validator(*from, amount, 0, None, Some(pk))?;
        }
        debit(storage, txn, from, amount)?;
        Ok(vec![ContractEvent {
            topic: b"stake".to_vec(),
            data: bincode::serialize(&(from, amount))
                .map_err(|e| ExecutionError::Other(e.to_string()))?,
        }])
    }

    /// Execute an unstake transaction. Validators unbond self-stake
    /// (`unbond_validator`); delegators unbond via `unbond()` across their
    /// delegations in validator-address order. Funds enter the unbonding
    /// queue and are paid out automatically after `UNBONDING_ROUNDS`.
    fn execute_unstake(
        &mut self,
        _txn: &mut WriteTransaction,
        _storage: &Storage,
        from: &Address,
        amount: u64,
    ) -> Result<Vec<ContractEvent>, ExecutionError> {
        if amount == 0 {
            return Err(ExecutionError::Validation(
                "Unstake amount must be positive".into(),
            ));
        }
        if self.staking.validators.iter().any(|v| v.address == *from) {
            self.staking.unbond_validator(*from, amount)?;
        } else {
            let mut per_validator: std::collections::BTreeMap<[u8; 32], u64> = Default::default();
            for d in self
                .staking
                .delegations
                .iter()
                .filter(|d| d.delegator == *from)
            {
                *per_validator.entry(d.validator.0).or_default() += d.amount;
            }
            let total: u64 = per_validator.values().fold(0, |a, b| a.saturating_add(*b));
            if total < amount {
                return Err(ExecutionError::Validation(
                    "Insufficient staked amount to unstake".to_string(),
                ));
            }
            let mut remaining = amount;
            for (validator, delegated) in per_validator {
                if remaining == 0 {
                    break;
                }
                let take = remaining.min(delegated);
                self.staking.unbond(*from, Address(validator), take)?;
                remaining -= take;
            }
        }
        Ok(vec![ContractEvent {
            topic: b"unstake".to_vec(),
            data: bincode::serialize(&(from, amount))
                .map_err(|e| ExecutionError::Other(e.to_string()))?,
        }])
    }

    /// Execute a delegate transaction via `StakingState::delegate`.
    fn execute_delegate(
        &mut self,
        txn: &mut WriteTransaction,
        storage: &Storage,
        from: &Address,
        validator: Address,
        amount: u64,
    ) -> Result<Vec<ContractEvent>, ExecutionError> {
        if amount == 0 {
            return Err(ExecutionError::Validation(
                "Delegate amount must be positive".to_string(),
            ));
        }
        let account = storage.state().get_account_or_default_write(txn, from)?;
        if account.balance < amount {
            return Err(ExecutionError::Validation(
                "Insufficient balance for delegate".to_string(),
            ));
        }
        self.staking
            .delegate(*from, validator, amount)
            .map_err(|_| {
                ExecutionError::Validation("Validator not found or inactive".to_string())
            })?;
        debit(storage, txn, from, amount)?;
        Ok(vec![ContractEvent {
            topic: b"delegate".to_vec(),
            data: bincode::serialize(&(from, &validator, amount))
                .map_err(|e| ExecutionError::Other(e.to_string()))?,
        }])
    }

    /// Execute a claim-rewards transaction: pay the sender's accrued
    /// delegator rewards (for one validator, or all) to its balance.
    fn execute_claim_rewards(
        &mut self,
        txn: &mut WriteTransaction,
        storage: &Storage,
        from: &Address,
        validator: Option<Address>,
    ) -> Result<Vec<ContractEvent>, ExecutionError> {
        // Collect first (no writes), then validate, then write.
        let claims: Vec<([u8; 64], Address, u64)> = {
            let table = txn.open_table(PENDING_REWARDS)?;
            match validator {
                Some(v) => {
                    let key = reward_key(from, &v);
                    let amt = table.get(key)?.map(|x| x.value()).unwrap_or(0);
                    vec![(key, v, amt)]
                }
                None => {
                    let lo = reward_key(from, &Address([0u8; 32]));
                    let hi = reward_key(from, &Address([0xFF; 32]));
                    let mut out = Vec::new();
                    for row in table.range(lo..=hi)? {
                        let (k, v) = row?;
                        let k = k.value();
                        let mut va = [0u8; 32];
                        va.copy_from_slice(&k[32..]);
                        out.push((k, Address(va), v.value()));
                    }
                    out
                }
            }
        };
        let total: u64 = claims.iter().map(|c| c.2).fold(0, u64::saturating_add);
        if total == 0 {
            return Err(ExecutionError::Validation(
                "No rewards to claim".to_string(),
            ));
        }
        let mut events = Vec::new();
        {
            let mut table = txn.open_table(PENDING_REWARDS)?;
            for (key, v, amt) in &claims {
                table.remove(*key)?;
                events.push(ContractEvent {
                    topic: b"claim_rewards".to_vec(),
                    data: bincode::serialize(&(v, *amt))
                        .map_err(|e| ExecutionError::Other(e.to_string()))?,
                });
            }
        }
        storage.state().add_balance(txn, from, total)?;
        Ok(events)
    }

    /// Execute a contract deployment transaction.
    fn execute_deploy(
        &mut self,
        txn: &mut WriteTransaction,
        storage: &Storage,
        from: &Address,
        code: Vec<u8>,
    ) -> Result<Vec<ContractEvent>, ExecutionError> {
        if code.is_empty() {
            return Err(ExecutionError::Validation(
                "Empty contract code".to_string(),
            ));
        }

        // Compute contract address: hash(sender || nonce)
        // For now, use a simple derivation. In production, this should be deterministic
        // and collision-resistant.
        let from_key = address_to_bytes(from);
        let sender_account = {
            let table = txn.open_table(tables::ACCOUNTS)?;
            let value = table.get(from_key)?;
            value
                .map(|v| Account::from_bytes(&v.value()).unwrap_or_default())
                .unwrap_or_default()
        };
        let mut addr_bytes = Vec::new();
        addr_bytes.extend_from_slice(&from.0);
        addr_bytes.extend_from_slice(&sender_account.nonce.to_le_bytes());
        let contract_addr = Address(kvnc_common::hash(&addr_bytes));

        // Deploy contract code
        let code_hash = storage.state().deploy_contract(txn, &contract_addr, code)?;

        // Emit deploy event
        let events = vec![ContractEvent {
            topic: b"deploy".to_vec(),
            data: bincode::serialize(&(&contract_addr, &code_hash))
                .map_err(|e| ExecutionError::Other(e.to_string()))?,
        }];

        Ok(events)
    }

    /// Execute a contract call transaction.
    #[allow(clippy::too_many_arguments)]
    fn execute_call(
        &mut self,
        txn: &mut WriteTransaction,
        storage: &Storage,
        from: &Address,
        contract: Address,
        value: u64,
        method: &str,
        args: &[u8],
        gas_limit: u64,
        block_height: u64,
    ) -> Result<Vec<ContractEvent>, ExecutionError> {
        // Check contract exists
        let contract_account = storage
            .state()
            .get_account_or_default_write(txn, &contract)?;
        if contract_account.code.is_empty() {
            return Err(ExecutionError::Validation("Contract not found".to_string()));
        }

        // Get contract code
        let code = contract_account.code.clone();

        // Execute via ContractRunner (WASM)
        let config = ExecutionConfig {
            gas_limit,
            memory_limit_pages: 256,
        };

        let wasm_call = WasmCall {
            wasm: &code,
            entry: method,
            contract,
            caller: *from,
            height: block_height,
            timestamp: block_height, // Simplified: use height as timestamp
            args,
            storage,
            // Single writer: the host reads and writes through the sub-DAG's
            // open write transaction instead of opening a second one.
            txn: Some(&*txn),
            // Signed Call.value: moved signer -> contract inside the host
            // overlay, so a failed call moves nothing (fee/nonce still
            // charged by the per-tx savepoint).
            value,
            config: &config,
        };

        let (_, events) = self.contract_runner.execute_wasm_call(wasm_call)?;

        Ok(events)
    }

    /// Apply double-sign evidence: slash 5% (SLASH_PCT_BPS) of the
    /// validator's self-stake and of each delegation, once per evidence.
    ///
    /// Destination: **treasury**. The slashed amount moves from staked to the
    /// real account balance of the configured treasury address
    /// (`TreasuryState::treasury_address`, the same account `claim_treasury`
    /// credits). Nothing is burned, total supply is unchanged, and the
    /// treasury vesting schedule (`vested`/`claimed`) is NOT touched.
    /// Cumulative total in `supply_counters["slashed_to_treasury"]`.
    /// Without a configured treasury the evidence is rejected and nothing
    /// changes.
    /// Opens its own write transaction: call only between sub-DAGs.
    pub fn apply_double_sign_evidence(
        &mut self,
        evidence: kvnc_staking::DoubleSignEvidence,
        storage: &Storage,
    ) -> Result<u64, ExecutionError> {
        let id = StakingState::evidence_id(&evidence);
        let txn = storage.begin_write()?;
        if txn.open_table(PROCESSED_EVIDENCE)?.get(id)?.is_some() {
            return Err(StakingError::EvidenceProcessed.into());
        }
        let treasury = self
            .staking
            .treasury_address()
            .ok_or_else(|| ExecutionError::Other("treasury not configured".into()))?;
        let mut candidate = snapshot_staking(&self.staking);
        let slashed = candidate.slash(evidence)?;
        storage.state().add_balance(&txn, &treasury, slashed)?;
        {
            let mut counters = txn.open_table(SUPPLY_COUNTERS)?;
            let cur = counters
                .get("slashed_to_treasury")?
                .map(|v| v.value())
                .unwrap_or(0);
            counters.insert("slashed_to_treasury", cur.saturating_add(slashed))?;
        }
        txn.open_table(PROCESSED_EVIDENCE)?.insert(id, 1)?;
        storage.state().save_staking_state(&txn, &candidate)?;
        txn.commit().map_err(StorageError::Commit)?;
        self.staking = candidate;
        Ok(slashed)
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
            "Treasury claim: {} {} credited to {:?}",
            claimed / kvnc_staking::ONE_KVNC,
            kvnc_staking::TICKER,
            treasury_address
        );

        Ok(claimed)
    }
}

/// Debit `amount` from `address` (balance checked by the caller).
/// (`StateStore::sub_balance` keeps the ACCOUNTS table open while writing,
/// which redb rejects inside one write transaction.)
fn debit(
    storage: &Storage,
    txn: &WriteTransaction,
    address: &Address,
    amount: u64,
) -> Result<(), ExecutionError> {
    let mut account = storage.state().get_account_or_default_write(txn, address)?;
    account.balance = account
        .balance
        .checked_sub(amount)
        .ok_or(ExecutionError::Validation(
            "Insufficient balance".to_string(),
        ))?;
    storage.state().set_account(txn, address, &account)?;
    Ok(())
}

/// Field-by-field copy of the staking state (it does not derive `Clone`).
fn snapshot_staking(s: &StakingState) -> StakingState {
    StakingState {
        validators: s.validators.clone(),
        delegations: s.delegations.clone(),
        total_staked: s.total_staked,
        committed_leader_height: s.committed_leader_height,
        total_mining_issued: s.total_mining_issued,
        treasury: s.treasury.clone(),
        unbonding_queue: s.unbonding_queue.clone(),
    }
}

/// Every `ACCOUNTS` row a transaction body can write (sender, plus the
/// transfer recipient / deployed contract address).
fn touched_accounts(tx: &Transaction) -> Vec<[u8; 32]> {
    let mut keys = vec![address_to_bytes(&tx.sender)];
    match &tx.kind {
        TransactionKind::Transfer { to, .. } => keys.push(address_to_bytes(to)),
        TransactionKind::Deploy { .. } => {
            // Contract address derivation mirrors `execute_deploy`; the nonce
            // has already been incremented when the body runs.
            let mut b = Vec::with_capacity(40);
            b.extend_from_slice(&tx.sender.0);
            b.extend_from_slice(&tx.nonce.saturating_add(1).to_le_bytes());
            keys.push(address_to_bytes(&Address(kvnc_common::hash(&b))));
        }
        _ => {}
    }
    keys.sort_unstable();
    keys.dedup();
    keys
}

/// Pre-images of account rows, restored when a transaction body fails.
struct AccountUndo(Vec<([u8; 32], Option<Vec<u8>>)>);

impl AccountUndo {
    fn capture(txn: &WriteTransaction, keys: &[[u8; 32]]) -> Result<Self, ExecutionError> {
        let table = txn.open_table(tables::ACCOUNTS)?;
        let mut rows = Vec::with_capacity(keys.len());
        for key in keys {
            rows.push((*key, table.get(*key)?.map(|v| v.value())));
        }
        Ok(Self(rows))
    }

    fn restore(self, txn: &WriteTransaction) -> Result<(), ExecutionError> {
        let mut table = txn.open_table(tables::ACCOUNTS)?;
        for (key, pre) in self.0 {
            match pre {
                Some(bytes) => {
                    table.insert(key, bytes)?;
                }
                None => {
                    table.remove(key)?;
                }
            }
        }
        Ok(())
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

    use kvnc_types::crypto::SigningKey;
    use kvnc_types::{Signature, StatementBlock};

    /// Tests sign for the dev/test network `ExecutionContext::new` uses.
    const TEST_CTX: SigningContext = SigningContext::new(chain_id::LOCAL);

    fn sample_block(author: u16) -> StatementBlock {
        sample_block_at(author, 1)
    }

    fn sample_block_at(author: u16, round: u64) -> StatementBlock {
        let digest = StatementBlock::compute_digest(author, round, &[], &[]);
        StatementBlock {
            author,
            round,
            parents: Vec::new(),
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: Signature([0u8; 64]),
            digest,
            merkle_root: Default::default(),
        }
    }

    fn sample_committed_subdag(round: u64, author: u16) -> CommittedSubDag {
        let leader = sample_block_at(author, round);
        CommittedSubDag {
            blocks: vec![leader.clone()],
            leader,
            leader_round: round,
            leader_author: author,
        }
    }

    fn sample_block_with_txs(author: u16, txs: Vec<Transaction>) -> StatementBlock {
        let digest = StatementBlock::compute_digest(author, 1, &[], &txs);
        StatementBlock {
            author,
            round: 1,
            parents: Vec::new(),
            transactions: txs,
            statements: Vec::new(),
            signature: Signature([0u8; 64]),
            digest,
            merkle_root: Default::default(),
        }
    }

    /// Deterministic test keypair. The secret bytes are a fixed test-only
    /// constant and are never logged or printed.
    fn test_keypair(tag: u8) -> SigningKey {
        SigningKey::from_bytes(&[tag; 32])
    }

    /// The account address controlled by a test keypair (address = Ed25519
    /// public key, unchanged address format).
    fn address_of(key: &SigningKey) -> Address {
        Address(key.verifying_key().to_bytes())
    }

    /// Build a transaction for `kind` and sign it properly with `key`.
    fn signed_tx(key: &SigningKey, kind: TransactionKind, nonce: u64, fee: u64) -> Transaction {
        let mut tx = Transaction {
            sender: address_of(key),
            nonce,
            kind,
            fee,
            signature: Signature([0u8; 64]),
            hash: kvnc_types::hash::Hash([0u8; 32]),
        };
        tx.hash = tx.signing_hash(&TEST_CTX);
        tx.signature = kvnc_crypto::sign(key, &tx.signing_hash(&TEST_CTX).0);
        assert!(
            tx.verify_signature(&TEST_CTX),
            "test helper must produce valid signatures"
        );
        tx
    }

    fn create_transfer_tx(
        key: &SigningKey,
        recipient: &Address,
        amount: u64,
        nonce: u64,
        fee: u64,
    ) -> Transaction {
        let kind = TransactionKind::Transfer {
            to: *recipient,
            amount,
        };
        signed_tx(key, kind, nonce, fee)
    }

    fn genesis_staking_state() -> StakingState {
        let mut state = StakingState::new();
        state.init_treasury(Address([42u8; 32]));
        for i in 0..3u8 {
            state
                .join_validator(
                    Address([i + 1; 32]),
                    kvnc_staking::MIN_VALIDATOR_STAKE,
                    0,
                    Some(Address([i + 11; 32])),
                    None,
                )
                .expect("genesis validator");
        }
        state
    }

    fn staking_state_bytes(state: &StakingState) -> Vec<u8> {
        bincode::serialize(state).expect("serialize staking state")
    }

    fn load_staking_state(storage: &Storage) -> StakingState {
        let txn = storage.begin_read().expect("read txn");
        storage
            .state()
            .load_staking_state(&txn)
            .expect("persisted staking state")
    }

    fn persist_initial_staking_state(storage: &Storage, state: &StakingState) {
        let txn = storage.begin_write().expect("write txn");
        storage
            .state()
            .save_staking_state(&txn, state)
            .expect("save initial staking state");
        txn.commit().expect("commit initial staking state");
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
                None,
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

        // Duplicate delivery of the same committed leader is idempotent.
        let second = ctx
            .execute_committed_subdag(&subdag, &storage)
            .expect("duplicate delivery");
        assert!(second.reward.is_none());
        assert_eq!(read_balance(&storage, &payout), reward.amount);

        // A different committed leader pays the next height's reward.
        let next_subdag = sample_committed_subdag(2, 0);
        let second = ctx
            .execute_committed_subdag(&next_subdag, &storage)
            .expect("next commit");
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
    fn failed_storage_commit_does_not_publish_candidate_staking_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().join("failed-commit.redb")).expect("storage");
        let initial_staking = genesis_staking_state();
        persist_initial_staking_state(&storage, &initial_staking);
        let mut ctx = ExecutionContext {
            staking: initial_staking,
            contract_runner: ContractRunner::new(),
            log_publisher: None,
            signing_ctx: TEST_CTX,
        };
        let initial_bytes = staking_state_bytes(&ctx.staking);
        let subdag = sample_committed_subdag(1, 0);

        let err = ctx
            .execute_committed_subdag_with_commit(&subdag, &storage, |_txn| {
                Err(ExecutionError::Other("injected commit failure".into()))
            })
            .expect_err("injected storage commit failure");
        assert!(matches!(err, ExecutionError::Other(_)));
        assert_eq!(staking_state_bytes(&ctx.staking), initial_bytes);
        assert_eq!(
            staking_state_bytes(&load_staking_state(&storage)),
            initial_bytes,
            "the aborted write transaction must not persist state"
        );
        assert_eq!(read_balance(&storage, &Address([11; 32])), 0);

        // Aborting left the leader unmarked, so replay can apply it exactly once.
        let replay = ctx
            .execute_committed_subdag(&subdag, &storage)
            .expect("replay after failed persistence");
        assert!(replay.reward.is_some());
        assert_eq!(ctx.staking.committed_leader_height, 1);
    }

    #[test]
    fn committed_leader_replay_is_deterministic_from_genesis() {
        // This exercises the implemented staking/reward transition only. Native
        // transactions and WASM calls remain placeholders in execution, so this
        // is not a claim of full account or contract state replay.
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let storage_a = Storage::new(dir_a.path().join("replay.redb")).expect("storage a");
        let storage_b = Storage::new(dir_b.path().join("replay.redb")).expect("storage b");

        let genesis = genesis_staking_state();
        let genesis_bytes = staking_state_bytes(&genesis);
        let mut context_a = ExecutionContext {
            staking: bincode::deserialize(&genesis_bytes).expect("genesis state a"),
            contract_runner: ContractRunner::new(),
            log_publisher: None,
            signing_ctx: TEST_CTX,
        };
        let mut context_b = ExecutionContext {
            staking: bincode::deserialize(&genesis_bytes).expect("genesis state b"),
            contract_runner: ContractRunner::new(),
            log_publisher: None,
            signing_ctx: TEST_CTX,
        };
        persist_initial_staking_state(&storage_a, &context_a.staking);
        persist_initial_staking_state(&storage_b, &context_b.staking);

        // These minimal committed-subdag fixtures provide execution with the
        // committed leader authors/rounds; they do not run or model consensus.
        let replay = [
            sample_committed_subdag(1, 0),
            sample_committed_subdag(2, 1),
            sample_committed_subdag(3, 2),
            sample_committed_subdag(4, 1),
            sample_committed_subdag(5, 0),
        ];
        let payout_addresses = [
            Address([11u8; 32]),
            Address([12u8; 32]),
            Address([13u8; 32]),
        ];
        let mut rewards_a = Vec::new();
        let mut rewards_b = Vec::new();

        for subdag in &replay {
            let result_a = context_a
                .execute_committed_subdag(subdag, &storage_a)
                .expect("replay on context a");
            let result_b = context_b
                .execute_committed_subdag(subdag, &storage_b)
                .expect("replay on context b");

            let reward_a = result_a.reward.expect("reward a");
            let reward_b = result_b.reward.expect("reward b");
            assert_eq!(
                (
                    reward_a.leader_author,
                    reward_a.recipient,
                    reward_a.amount,
                    reward_a.height
                ),
                (
                    reward_b.leader_author,
                    reward_b.recipient,
                    reward_b.amount,
                    reward_b.height
                ),
                "reward outcome must match after every committed leader"
            );
            assert_eq!(result_a.circulating_supply, result_b.circulating_supply);
            assert_eq!(
                context_a.staking.committed_leader_height,
                context_b.staking.committed_leader_height
            );
            assert_eq!(
                context_a.staking.total_mining_issued,
                context_b.staking.total_mining_issued
            );
            assert_eq!(
                context_a.staking.circulating_supply(),
                context_b.staking.circulating_supply()
            );
            assert_eq!(
                staking_state_bytes(&context_a.staking),
                staking_state_bytes(&context_b.staking),
                "in-memory staking states must match after every committed leader"
            );

            // Reload the committed states on both sides to include the available
            // storage persistence path in the deterministic replay check.
            context_a.staking = load_staking_state(&storage_a);
            context_b.staking = load_staking_state(&storage_b);
            assert_eq!(
                staking_state_bytes(&context_a.staking),
                staking_state_bytes(&context_b.staking),
                "reloaded staking states must match after every committed leader"
            );
            assert_eq!(
                (
                    context_a.staking.committed_leader_height,
                    context_a.staking.total_mining_issued
                ),
                (
                    context_b.staking.committed_leader_height,
                    context_b.staking.total_mining_issued
                )
            );
            assert_eq!(
                context_a.staking.circulating_supply(),
                context_b.staking.circulating_supply()
            );
            assert_eq!(
                read_balance(&storage_a, &reward_a.recipient),
                read_balance(&storage_b, &reward_b.recipient),
                "matching reward recipients must have matching persisted payouts"
            );
            for address in payout_addresses {
                assert_eq!(
                    read_balance(&storage_a, &address),
                    read_balance(&storage_b, &address),
                    "all validator payout balances must match"
                );
            }

            rewards_a.push((
                reward_a.leader_author,
                reward_a.recipient,
                reward_a.amount,
                reward_a.height,
            ));
            rewards_b.push((
                reward_b.leader_author,
                reward_b.recipient,
                reward_b.amount,
                reward_b.height,
            ));
        }

        assert_eq!(rewards_a, rewards_b, "final reward sequence must match");
        assert_eq!(
            staking_state_bytes(&context_a.staking),
            staking_state_bytes(&context_b.staking),
            "final persisted staking states must be byte-identical"
        );
        assert_eq!(
            context_a.staking.committed_leader_height,
            replay.len() as u64
        );
        assert_eq!(
            context_a.staking.total_mining_issued,
            rewards_a.iter().map(|(_, _, amount, _)| amount).sum()
        );
        assert_eq!(
            context_a.staking.circulating_supply(),
            context_b.staking.circulating_supply()
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
                None,
            )
            .expect("join validator");

        // Manually advance treasury vesting for testing (simulate 2 years elapsed)
        // by directly setting vested amount on the treasury state.
        if let Some(ref mut treasury) = ctx.staking.treasury {
            treasury.vested = 2 * kvnc_staking::TREASURY_ANNUAL;
        }

        // Treasury should have 2M KUNA vested and claimable
        let claimable = ctx.staking.treasury_claimable();
        assert_eq!(claimable, 2 * kvnc_staking::TREASURY_ANNUAL);

        // Claim 1M KUNA
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

    #[test]
    fn duplicate_delivery_after_restart_does_not_pay_twice() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("restart.redb");
        let subdag = sample_committed_subdag(1, 0);
        let payout = Address([11u8; 32]);
        let first_reward;

        {
            let storage = Storage::new(&path).expect("storage");
            let mut context = ExecutionContext {
                staking: genesis_staking_state(),
                contract_runner: ContractRunner::new(),
                log_publisher: None,
                signing_ctx: TEST_CTX,
            };
            persist_initial_staking_state(&storage, &context.staking);
            first_reward = context
                .execute_committed_subdag(&subdag, &storage)
                .expect("first delivery")
                .reward
                .expect("reward paid")
                .amount;
            assert_eq!(read_balance(&storage, &payout), first_reward);
        }

        let storage = Storage::new(&path).expect("reopened storage");
        let mut context = ExecutionContext {
            staking: load_staking_state(&storage),
            contract_runner: ContractRunner::new(),
            log_publisher: None,
            signing_ctx: TEST_CTX,
        };
        let replay = context
            .execute_committed_subdag(&subdag, &storage)
            .expect("replayed delivery");
        assert!(replay.reward.is_none());
        assert_eq!(context.staking.committed_leader_height, 1);
        assert_eq!(read_balance(&storage, &payout), first_reward);
    }

    #[test]
    fn transfer_transaction_execution() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().join("test.redb")).expect("storage");

        let mut ctx = ExecutionContext::new();
        let sender_key = test_keypair(1);
        let sender = address_of(&sender_key);
        let recipient = Address([2u8; 32]);
        let validator = Address([3u8; 32]);
        let payout = Address([4u8; 32]);

        // Fund sender with 1000 KUNA
        let txn = storage.begin_write().expect("write txn");
        storage
            .state()
            .set_account(
                &txn,
                &sender,
                &Account {
                    balance: 1000 * kvnc_staking::ONE_KVNC,
                    nonce: 0,
                    code_hash: [0u8; 32],
                    code: Vec::new(),
                },
            )
            .expect("set_account");
        txn.commit().expect("commit");

        // Setup validator for leader
        ctx.staking
            .join_validator(
                validator,
                kvnc_staking::MIN_VALIDATOR_STAKE,
                0,
                Some(payout),
                None,
            )
            .expect("join validator");

        // Create a signed transfer transaction
        let tx = create_transfer_tx(
            &sender_key,
            &recipient,
            100 * kvnc_staking::ONE_KVNC,
            0,
            1000,
        );
        let subdag = CommittedSubDag {
            leader: sample_block(0),
            blocks: vec![sample_block_with_txs(0, vec![tx.clone()])],
            leader_round: 1,
            leader_author: 0,
        };

        let result = ctx
            .execute_committed_subdag(&subdag, &storage)
            .expect("execute");
        assert_eq!(result.txs_applied, 1);
        assert_eq!(result.receipts.len(), 1);
        assert!(result.receipts[0].success);

        // Check balances
        let _sender_bal = read_balance(&storage, &sender);
        let _recipient_bal = read_balance(&storage, &recipient);
        // 1000 - 100 (transfer) - 1000 (fee) = -100, but fee is deducted first
        // Actually: 1000 - 1000 (fee) = 0, then transfer 100 fails due to insufficient balance
        // Let me fix the fee to be smaller
    }

    #[test]
    fn transfer_transaction_with_sufficient_balance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().join("test.redb")).expect("storage");

        let mut ctx = ExecutionContext::new();
        let sender_key = test_keypair(1);
        let sender = address_of(&sender_key);
        let recipient = Address([2u8; 32]);
        let validator = Address([3u8; 32]);
        let payout = Address([4u8; 32]);

        // Fund sender with 1000 KUNA
        let txn = storage.begin_write().expect("write txn");
        storage
            .state()
            .set_account(
                &txn,
                &sender,
                &Account {
                    balance: 1000 * kvnc_staking::ONE_KVNC,
                    nonce: 0,
                    code_hash: [0u8; 32],
                    code: Vec::new(),
                },
            )
            .expect("set_account");
        txn.commit().expect("commit");

        // Setup validator for leader
        ctx.staking
            .join_validator(
                validator,
                kvnc_staking::MIN_VALIDATOR_STAKE,
                0,
                Some(payout),
                None,
            )
            .expect("join validator");

        // Create a signed transfer transaction with small fee
        let tx = create_transfer_tx(
            &sender_key,
            &recipient,
            100 * kvnc_staking::ONE_KVNC,
            0,
            1000,
        );
        let subdag = CommittedSubDag {
            leader: sample_block(0),
            blocks: vec![sample_block_with_txs(0, vec![tx.clone()])],
            leader_round: 1,
            leader_author: 0,
        };

        let result = ctx
            .execute_committed_subdag(&subdag, &storage)
            .expect("execute");
        assert_eq!(result.txs_applied, 1);
        assert_eq!(result.receipts.len(), 1);
        assert!(
            result.receipts[0].success,
            "Transfer failed: {:?}",
            result.receipts[0].error
        );

        // Check balances: 1000 - 100 (transfer) - 1000 (fee) -> need more balance
        // Let's use a smaller fee
    }

    /// Fund `address`, register a leader validator and return the context.
    fn setup_funded_sender(storage: &Storage, address: &Address, balance: u64) -> ExecutionContext {
        let txn = storage.begin_write().expect("write txn");
        storage
            .state()
            .set_account(
                &txn,
                address,
                &Account {
                    balance,
                    nonce: 0,
                    code_hash: [0u8; 32],
                    code: Vec::new(),
                },
            )
            .expect("set_account");
        txn.commit().expect("commit");

        let mut ctx = ExecutionContext::new();
        ctx.staking
            .join_validator(
                Address([3u8; 32]),
                kvnc_staking::MIN_VALIDATOR_STAKE,
                0,
                Some(Address([4u8; 32])),
                None,
            )
            .expect("join validator");
        ctx
    }

    fn read_nonce(storage: &Storage, address: &Address) -> u64 {
        let txn = storage.begin_read().expect("read txn");
        storage
            .state()
            .get_account_or_default(&txn, address)
            .expect("account")
            .nonce
    }

    fn execute_single_tx(
        ctx: &mut ExecutionContext,
        storage: &Storage,
        tx: Transaction,
    ) -> ExecutionResult {
        let subdag = CommittedSubDag {
            leader: sample_block(0),
            blocks: vec![sample_block_with_txs(0, vec![tx])],
            leader_round: 1,
            leader_author: 0,
        };
        ctx.execute_committed_subdag(&subdag, storage)
            .expect("execute")
    }

    // Regression: the executor used to skip verification for any signature
    // made of 64 bytes of 0x01, so anyone could forge a transfer from any
    // account. It must now be rejected and leave all state untouched.
    #[test]
    fn all_0x01_signature_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().join("test.redb")).expect("storage");

        // Victim account the attacker does NOT hold the key for.
        let victim = address_of(&test_keypair(1));
        let attacker = Address([0xAA; 32]);
        let initial = 1000 * kvnc_staking::ONE_KVNC;
        let mut ctx = setup_funded_sender(&storage, &victim, initial);

        let mut forged = Transaction {
            sender: victim,
            nonce: 0,
            kind: TransactionKind::Transfer {
                to: attacker,
                amount: 500 * kvnc_staking::ONE_KVNC,
            },
            fee: 1000,
            signature: Signature([1u8; 64]),
            hash: kvnc_types::hash::Hash([0u8; 32]),
        };
        forged.hash = forged.signing_hash(&TEST_CTX);

        let result = execute_single_tx(&mut ctx, &storage, forged);
        assert_eq!(result.receipts.len(), 1);
        let receipt = &result.receipts[0];
        assert!(!receipt.success, "forged 0x01 signature must be rejected");
        assert_eq!(receipt.error.as_deref(), Some("Invalid signature"));
        assert_eq!(read_balance(&storage, &victim), initial, "no fee, no debit");
        assert_eq!(read_nonce(&storage, &victim), 0, "nonce not consumed");
        assert_eq!(read_balance(&storage, &attacker), 0);
    }

    // A real signature by a different key over the same payload is rejected.
    #[test]
    fn signature_by_wrong_key_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().join("test.redb")).expect("storage");

        let victim = address_of(&test_keypair(1));
        let attacker_key = test_keypair(2);
        let initial = 1000 * kvnc_staking::ONE_KVNC;
        let mut ctx = setup_funded_sender(&storage, &victim, initial);

        // Attacker signs a valid tx for its own address, then swaps in the
        // victim as sender.
        let mut tx = create_transfer_tx(
            &attacker_key,
            &address_of(&attacker_key),
            500 * kvnc_staking::ONE_KVNC,
            0,
            1000,
        );
        tx.sender = victim;
        tx.hash = tx.signing_hash(&TEST_CTX);

        let result = execute_single_tx(&mut ctx, &storage, tx);
        assert!(!result.receipts[0].success);
        assert_eq!(
            result.receipts[0].error.as_deref(),
            Some("Invalid signature")
        );
        assert_eq!(read_balance(&storage, &victim), initial);
        assert_eq!(read_balance(&storage, &address_of(&attacker_key)), 0);
    }

    // A properly signed transfer is applied with the exact balance deltas.
    #[test]
    fn properly_signed_transfer_moves_exact_amounts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().join("test.redb")).expect("storage");

        let key = test_keypair(1);
        let sender = address_of(&key);
        let recipient = Address([2u8; 32]);
        let initial = 1000 * kvnc_staking::ONE_KVNC;
        let amount = 100 * kvnc_staking::ONE_KVNC;
        let fee = 1000;
        let mut ctx = setup_funded_sender(&storage, &sender, initial);

        let tx = create_transfer_tx(&key, &recipient, amount, 0, fee);
        let result = execute_single_tx(&mut ctx, &storage, tx);
        assert!(result.receipts[0].success, "{:?}", result.receipts[0].error);
        assert_eq!(read_balance(&storage, &sender), initial - amount - fee);
        assert_eq!(read_balance(&storage, &recipient), amount);
        assert_eq!(read_nonce(&storage, &sender), 1);
    }

    fn delegate_tx(key: &SigningKey, validator: Address, amount: u64, nonce: u64) -> Transaction {
        signed_tx(
            key,
            TransactionKind::Delegate { validator, amount },
            nonce,
            1000,
        )
    }

    // Rollback: delegating to a nonexistent validator charges only fee+nonce,
    // leaves staking untouched and yields an honest failed receipt.
    #[test]
    fn failed_delegate_rolls_back_except_fee_and_nonce() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().join("test.redb")).expect("storage");
        let key = test_keypair(1);
        let sender = address_of(&key);
        let initial = 1000 * kvnc_staking::ONE_KVNC;
        let fee = 1000;
        let mut ctx = setup_funded_sender(&storage, &sender, initial);
        let staked_before = ctx.staking.total_staked;
        let delegations_before = ctx.staking.delegations.len();

        let tx = delegate_tx(&key, Address([0xEE; 32]), 100 * kvnc_staking::ONE_KVNC, 0);
        let result = execute_single_tx(&mut ctx, &storage, tx);
        let receipt = &result.receipts[0];
        assert!(!receipt.success, "receipt must report failure");
        assert!(receipt
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Validator not found"));
        assert!(receipt.events.is_empty());
        assert_eq!(
            read_balance(&storage, &sender),
            initial - fee,
            "only the fee"
        );
        assert_eq!(read_nonce(&storage, &sender), 1, "nonce consumed");
        assert_eq!(ctx.staking.total_staked, staked_before);
        assert_eq!(ctx.staking.delegations.len(), delegations_before);
    }

    // A failed tx in the middle of a sub-DAG does not affect its neighbours.
    #[test]
    fn failed_tx_mid_subdag_does_not_affect_neighbours() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().join("test.redb")).expect("storage");
        let key = test_keypair(1);
        let sender = address_of(&key);
        let recipient = Address([2u8; 32]);
        let initial = 1000 * kvnc_staking::ONE_KVNC;
        let amount = 10 * kvnc_staking::ONE_KVNC;
        let fee = 1000;
        let mut ctx = setup_funded_sender(&storage, &sender, initial);

        let txs = vec![
            create_transfer_tx(&key, &recipient, amount, 0, fee),
            delegate_tx(&key, Address([0xEE; 32]), 100 * kvnc_staking::ONE_KVNC, 1),
            create_transfer_tx(&key, &recipient, amount, 2, fee),
        ];
        let subdag = CommittedSubDag {
            leader: sample_block(0),
            blocks: vec![sample_block_with_txs(0, txs)],
            leader_round: 1,
            leader_author: 0,
        };
        let result = ctx
            .execute_committed_subdag(&subdag, &storage)
            .expect("execute");
        let ok: Vec<bool> = result.receipts.iter().map(|r| r.success).collect();
        assert_eq!(ok, vec![true, false, true]);
        assert_eq!(read_balance(&storage, &recipient), 2 * amount);
        assert_eq!(
            read_balance(&storage, &sender),
            initial - 2 * amount - 3 * fee
        );
        assert_eq!(read_nonce(&storage, &sender), 3);
    }

    // Tx-level staking writes must not leak into memory when the sub-DAG's
    // storage commit fails.
    #[test]
    fn failed_subdag_commit_rolls_back_tx_staking_changes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().join("test.redb")).expect("storage");
        let key = test_keypair(1);
        let sender = address_of(&key);
        let mut ctx = setup_funded_sender(&storage, &sender, 100_000 * kvnc_staking::ONE_KVNC);
        let before = staking_state_bytes(&ctx.staking);
        let tx = signed_tx(
            &key,
            TransactionKind::Stake {
                amount: kvnc_staking::MIN_VALIDATOR_STAKE,
            },
            0,
            1000,
        );
        let subdag = CommittedSubDag {
            leader: sample_block(0),
            blocks: vec![sample_block_with_txs(0, vec![tx])],
            leader_round: 1,
            leader_author: 0,
        };
        ctx.execute_committed_subdag_with_commit(&subdag, &storage, |_| {
            Err(ExecutionError::Other("injected commit failure".into()))
        })
        .expect_err("injected failure");
        assert_eq!(staking_state_bytes(&ctx.staking), before);
    }

    // Deterministic replay with successful and failed txs interleaved.
    #[test]
    fn replay_with_failed_txs_is_deterministic() {
        let key = test_keypair(1);
        let sender = address_of(&key);
        let recipient = Address([2u8; 32]);
        let run = || {
            let dir = tempfile::tempdir().expect("tempdir");
            let storage = Storage::new(dir.path().join("r.redb")).expect("storage");
            let mut ctx = setup_funded_sender(&storage, &sender, 1000 * kvnc_staking::ONE_KVNC);
            let txs = vec![
                create_transfer_tx(&key, &recipient, kvnc_staking::ONE_KVNC, 0, 1000),
                delegate_tx(&key, Address([0xEE; 32]), kvnc_staking::ONE_KVNC, 1),
                delegate_tx(&key, Address([3u8; 32]), kvnc_staking::ONE_KVNC, 2),
                create_transfer_tx(&key, &recipient, u64::MAX / 2, 3, 1000),
            ];
            let subdag = CommittedSubDag {
                leader: sample_block(0),
                blocks: vec![sample_block_with_txs(0, txs)],
                leader_round: 1,
                leader_author: 0,
            };
            let r = ctx
                .execute_committed_subdag(&subdag, &storage)
                .expect("execute");
            (
                bincode::serialize(&r.receipts).expect("receipts"),
                staking_state_bytes(&ctx.staking),
                read_balance(&storage, &sender),
                read_balance(&storage, &recipient),
                read_nonce(&storage, &sender),
                r.receipts.iter().map(|r| r.success).collect::<Vec<_>>(),
            )
        };
        let a = run();
        let b = run();
        assert_eq!(a.5, vec![true, false, true, false]);
        assert_eq!(a, b);
    }
    // ---- single redb writer for contract calls --------------------------

    fn leb_u32(out: &mut Vec<u8>, mut v: u32) {
        loop {
            let mut b = (v & 0x7f) as u8;
            v >>= 7;
            if v != 0 {
                b |= 0x80;
            }
            out.push(b);
            if v == 0 {
                break;
            }
        }
    }

    fn leb_i32(out: &mut Vec<u8>, mut v: i32) {
        loop {
            let b = (v as u8) & 0x7f;
            v >>= 7;
            let done = (v == 0 && b & 0x40 == 0) || (v == -1 && b & 0x40 != 0);
            out.push(if done { b } else { b | 0x80 });
            if done {
                break;
            }
        }
    }

    fn section(wasm: &mut Vec<u8>, id: u8, payload: &[u8]) {
        wasm.push(id);
        leb_u32(wasm, payload.len() as u32);
        wasm.extend(payload);
    }

    fn name(out: &mut Vec<u8>, n: &str) {
        leb_u32(out, n.len() as u32);
        out.extend(n.as_bytes());
    }

    /// Hand-assembled probe contract (entry `probe`): reads the caller's
    /// native balance via `kvnc_balance_of`, stores it (i64 LE) under raw
    /// key `[0]`, then succeeds if `args` is empty and fails (contract
    /// error) otherwise — i.e. a failing call that already wrote its overlay.
    fn probe_wasm() -> Vec<u8> {
        let mut w = b"\0asm\x01\0\0\0".to_vec();
        let mut types = vec![5];
        types.extend([0x60, 2, 0x7f, 0x7f, 1, 0x7e]); // 0 (i32,i32)->i64
        types.extend([0x60, 1, 0x7f, 1, 0x7f]); // 1 i32->i32
        types.extend([0x60, 2, 0x7f, 0x7f, 0]); // 2 (i32,i32)->()
        types.extend([0x60, 1, 0x7f, 0]); // 3 i32->()
        types.extend([0x60, 4, 0x7f, 0x7f, 0x7f, 0x7f, 1, 0x7f]); // 4
        section(&mut w, 1, &types);
        let mut imports = vec![3];
        for (n, t) in [
            ("kvnc_caller", 3u8),
            ("kvnc_balance_of", 0),
            ("kvnc_storage_set", 4),
        ] {
            name(&mut imports, "env");
            name(&mut imports, n);
            imports.extend([0, t]);
        }
        section(&mut w, 2, &imports);
        section(&mut w, 3, &[3, 1, 2, 0]);
        section(&mut w, 5, &[1, 0, 1]);
        let mut exports = vec![4];
        for (n, kind, idx) in [
            ("memory", 2u8, 0u8),
            ("kvnc_alloc", 0, 3),
            ("kvnc_dealloc", 0, 4),
            ("probe", 0, 5),
        ] {
            name(&mut exports, n);
            exports.extend([kind, idx]);
        }
        section(&mut w, 7, &exports);

        let mut alloc = vec![0, 0x41];
        leb_i32(&mut alloc, 64);
        alloc.push(0x0b);
        let dealloc = vec![0, 0x0b];
        let c = |v: i32| {
            let mut o = vec![0x41];
            leb_i32(&mut o, v);
            o
        };
        let mut entry = vec![0];
        entry.extend(c(256));
        entry.extend([0x10, 0]); // kvnc_caller(256)
        entry.extend(c(512));
        entry.extend(c(256));
        entry.extend(c(32));
        entry.extend([0x10, 1]); // kvnc_balance_of(256, 32)
        entry.extend([0x37, 0x03, 0x00]); // i64.store [512]
        entry.extend(c(600));
        entry.extend(c(1));
        entry.extend(c(512));
        entry.extend(c(8));
        entry.extend([0x10, 2, 0x1a]); // kvnc_storage_set(...); drop
        entry.extend([0x20, 1, 0x04, 0x40, 0x42, 0x7f, 0x0f, 0x0b]); // if len { return -1 }
        entry.extend([0x42, 0x00, 0x0b]); // i64.const 0; end
        let mut code = vec![3];
        for body in [&alloc, &dealloc, &entry] {
            leb_u32(&mut code, body.len() as u32);
            code.extend(body.iter());
        }
        section(&mut w, 10, &code);
        w
    }

    const PROBE_CONTRACT: Address = Address([0xC0; 32]);

    fn deploy_probe(storage: &Storage) {
        let txn = storage.begin_write().expect("write txn");
        let code = probe_wasm();
        storage
            .state()
            .set_account(
                &txn,
                &PROBE_CONTRACT,
                &Account {
                    balance: 0,
                    nonce: 0,
                    code_hash: kvnc_common::hash(&code),
                    code,
                },
            )
            .expect("deploy probe");
        txn.commit().expect("commit");
    }

    fn probe_call(key: &SigningKey, fail: bool, nonce: u64, fee: u64) -> Transaction {
        let kind = TransactionKind::Call {
            contract: PROBE_CONTRACT,
            value: 0,
            method: "probe".to_string(),
            args: if fail { vec![1] } else { Vec::new() },
            gas_limit: 1_000_000,
        };
        signed_tx(key, kind, nonce, fee)
    }

    fn probe_slot(storage: &Storage) -> Option<u64> {
        let txn = storage.begin_read().expect("read");
        storage
            .state()
            .get_storage(&txn, &PROBE_CONTRACT, &kvnc_common::hash(&[0u8]))
            .expect("storage read")
            .map(|v| u64::from_le_bytes(v.try_into().expect("8 bytes")))
    }

    /// Run `f` on a worker thread; fail (instead of hanging the suite) if it
    /// does not finish within 30 s. The old code deadlocked here.
    fn with_timeout<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(std::time::Duration::from_secs(30))
            .expect("contract call hung: a second redb writer was opened (deadlock)")
    }

    // Call via execute_committed_subdag must not hang, and the contract must
    // see the fee already deducted in the same open write transaction.
    #[test]
    fn wasm_call_in_subdag_uses_single_writer_and_sees_fee() {
        let (receipt, slot, balance) = with_timeout(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let storage = Storage::new(dir.path().join("t.redb")).expect("storage");
            let key = test_keypair(1);
            let sender = address_of(&key);
            let mut ctx = setup_funded_sender(&storage, &sender, 1_000_000);
            deploy_probe(&storage);
            let r = execute_single_tx(&mut ctx, &storage, probe_call(&key, false, 0, 1000));
            (
                r.receipts[0].clone(),
                probe_slot(&storage),
                read_balance(&storage, &sender),
            )
        });
        assert!(receipt.success, "{:?}", receipt.error);
        assert_eq!(
            slot,
            Some(1_000_000 - 1000),
            "contract saw the fee deducted"
        );
        assert_eq!(balance, 1_000_000 - 1000);
    }

    // A transfer earlier in the same block is visible to the contract.
    #[test]
    fn wasm_call_sees_earlier_writes_in_same_block() {
        let (ok, slot) = with_timeout(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let storage = Storage::new(dir.path().join("t.redb")).expect("storage");
            let key = test_keypair(1);
            let sender = address_of(&key);
            let mut ctx = setup_funded_sender(&storage, &sender, 1_000_000);
            deploy_probe(&storage);
            let txs = vec![
                create_transfer_tx(&key, &Address([2u8; 32]), 100_000, 0, 1000),
                probe_call(&key, false, 1, 1000),
            ];
            let subdag = CommittedSubDag {
                leader: sample_block(0),
                blocks: vec![sample_block_with_txs(0, txs)],
                leader_round: 1,
                leader_author: 0,
            };
            let r = ctx
                .execute_committed_subdag(&subdag, &storage)
                .expect("execute");
            (
                r.receipts.iter().map(|r| r.success).collect::<Vec<_>>(),
                probe_slot(&storage),
            )
        });
        assert_eq!(ok, vec![true, true]);
        assert_eq!(slot, Some(1_000_000 - 100_000 - 2000));
    }

    // A failing call (after it already wrote its overlay) leaves no storage
    // write; only fee and nonce are charged.
    #[test]
    fn failed_wasm_call_leaves_no_storage_writes() {
        let (receipt, slot, balance, nonce) = with_timeout(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let storage = Storage::new(dir.path().join("t.redb")).expect("storage");
            let key = test_keypair(1);
            let sender = address_of(&key);
            let mut ctx = setup_funded_sender(&storage, &sender, 1_000_000);
            deploy_probe(&storage);
            let r = execute_single_tx(&mut ctx, &storage, probe_call(&key, true, 0, 1000));
            (
                r.receipts[0].clone(),
                probe_slot(&storage),
                read_balance(&storage, &sender),
                read_nonce(&storage, &sender),
            )
        });
        assert!(!receipt.success);
        assert_eq!(slot, None, "failed call must not write contract storage");
        assert_eq!(balance, 1_000_000 - 1000);
        assert_eq!(nonce, 1);
    }

    // Deterministic replay with successful and failing contract calls.
    #[test]
    fn replay_with_contract_calls_is_deterministic() {
        let run = || {
            with_timeout(|| {
                let dir = tempfile::tempdir().expect("tempdir");
                let storage = Storage::new(dir.path().join("t.redb")).expect("storage");
                let key = test_keypair(1);
                let sender = address_of(&key);
                let mut ctx = setup_funded_sender(&storage, &sender, 1_000_000);
                deploy_probe(&storage);
                let txs = vec![
                    probe_call(&key, true, 0, 1000),
                    create_transfer_tx(&key, &Address([2u8; 32]), 5_000, 1, 1000),
                    probe_call(&key, false, 2, 1000),
                ];
                let subdag = CommittedSubDag {
                    leader: sample_block(0),
                    blocks: vec![sample_block_with_txs(0, txs)],
                    leader_round: 1,
                    leader_author: 0,
                };
                let r = ctx
                    .execute_committed_subdag(&subdag, &storage)
                    .expect("execute");
                (
                    bincode::serialize(&r.receipts).expect("receipts"),
                    staking_state_bytes(&ctx.staking),
                    probe_slot(&storage),
                    read_balance(&storage, &sender),
                    read_nonce(&storage, &sender),
                )
            })
        };
        let a = run();
        assert_eq!(a.2, Some(1_000_000 - 5_000 - 3000));
        assert_eq!(a, run());
    }
    // ---- staking wiring -------------------------------------------------

    const K: u64 = kvnc_staking::ONE_KVNC;

    fn subdag_at(round: u64, txs: Vec<Transaction>) -> CommittedSubDag {
        let leader = sample_block_at(0, round);
        let mut block = sample_block_with_txs(0, txs);
        block.round = round;
        CommittedSubDag {
            blocks: vec![block],
            leader,
            leader_round: round,
            leader_author: 0,
        }
    }

    fn run(
        ctx: &mut ExecutionContext,
        storage: &Storage,
        round: u64,
        txs: Vec<Transaction>,
    ) -> Vec<TransactionReceipt> {
        ctx.execute_committed_subdag(&subdag_at(round, txs), storage)
            .expect("execute")
            .receipts
    }

    fn pending_reward(storage: &Storage, d: &Address, v: &Address) -> u64 {
        let txn = storage.begin_read().expect("read");
        match txn.open_table(PENDING_REWARDS) {
            Ok(t) => t
                .get(reward_key(d, v))
                .expect("get")
                .map(|x| x.value())
                .unwrap_or(0),
            Err(_) => 0,
        }
    }

    #[test]
    fn stake_tx_joins_then_bonds_validator() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().join("t.redb")).expect("storage");
        let key = test_keypair(1);
        let me = address_of(&key);
        let mut ctx = setup_funded_sender(&storage, &me, 100_000 * K);
        let min = kvnc_staking::MIN_VALIDATOR_STAKE;
        let r = run(
            &mut ctx,
            &storage,
            1,
            vec![
                signed_tx(&key, TransactionKind::Stake { amount: min }, 0, 1000),
                signed_tx(&key, TransactionKind::Stake { amount: 5 * K }, 1, 1000),
            ],
        );
        assert!(r.iter().all(|r| r.success), "{r:?}");
        let v = ctx
            .staking
            .validators
            .iter()
            .find(|v| v.address == me)
            .expect("validator");
        assert!(v.active);
        assert_eq!(v.stake, min + 5 * K);
        assert_eq!(v.public_key, Some(kvnc_types::PublicKey(me.0)));
        assert_eq!(
            ctx.staking
                .validators
                .iter()
                .filter(|v| v.address == me)
                .count(),
            1
        );
        assert_eq!(
            read_balance(&storage, &me),
            100_000 * K - min - 5 * K - 2000
        );
        // Below-minimum fresh stake is rejected and rolled back.
        let k2 = test_keypair(2);
        let other = address_of(&k2);
        let txn = storage.begin_write().expect("w");
        storage
            .state()
            .add_balance(&txn, &other, 10 * K)
            .expect("fund");
        txn.commit().expect("c");
        let r = run(
            &mut ctx,
            &storage,
            2,
            vec![signed_tx(
                &k2,
                TransactionKind::Stake { amount: 5 * K },
                0,
                1000,
            )],
        );
        assert!(!r[0].success);
        assert_eq!(read_balance(&storage, &other), 10 * K - 1000);
    }

    #[test]
    fn unstake_goes_through_queue_and_pays_after_unbonding_rounds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().join("t.redb")).expect("storage");
        let key = test_keypair(1);
        let me = address_of(&key);
        let validator = Address([3u8; 32]);
        let mut ctx = setup_funded_sender(&storage, &me, 1_000 * K);
        let r = run(
            &mut ctx,
            &storage,
            1,
            vec![
                signed_tx(
                    &key,
                    TransactionKind::Delegate {
                        validator,
                        amount: 400 * K,
                    },
                    0,
                    1000,
                ),
                signed_tx(&key, TransactionKind::Unstake { amount: 150 * K }, 1, 1000),
            ],
        );
        assert!(r.iter().all(|r| r.success), "{r:?}");
        assert_eq!(ctx.staking.delegated_to(validator), 250 * K);
        assert_eq!(ctx.staking.unbonding_queue.len(), 1);
        let release = ctx.staking.unbonding_queue[0].release_height;
        assert_eq!(
            release,
            kvnc_staking::UNBONDING_ROUNDS,
            "unbonded at height 0"
        );
        let liquid = read_balance(&storage, &me);
        assert_eq!(liquid, 600 * K - 2000, "unstaked funds are not liquid yet");
        // Not yet matured.
        run(&mut ctx, &storage, 2, vec![]);
        assert_eq!(read_balance(&storage, &me), liquid);
        // Jump to just before maturity; the next commit pays out.
        ctx.staking.committed_leader_height = release - 1;
        run(&mut ctx, &storage, 3, vec![]);
        assert_eq!(read_balance(&storage, &me), liquid + 150 * K);
        assert!(ctx.staking.unbonding_queue.is_empty());
    }

    // Old behaviour: ClaimRewards only emitted events, delegators were never paid.
    #[test]
    fn delegator_rewards_accrue_and_claim_pays_them() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().join("t.redb")).expect("storage");
        let key = test_keypair(1);
        let me = address_of(&key);
        let validator = Address([3u8; 32]);
        let payout = Address([4u8; 32]);
        let mut ctx = setup_funded_sender(&storage, &me, 100_000 * K);
        let min = kvnc_staking::MIN_VALIDATOR_STAKE;
        let r = run(
            &mut ctx,
            &storage,
            1,
            vec![signed_tx(
                &key,
                TransactionKind::Delegate {
                    validator,
                    amount: min,
                },
                0,
                1000,
            )],
        );
        assert!(r[0].success);
        // Reward of round 1 (10 KUNA) split 50/50 (commission 0).
        assert_eq!(pending_reward(&storage, &me, &validator), 5 * K);
        assert_eq!(read_balance(&storage, &payout), 5 * K);
        let before = read_balance(&storage, &me);
        let r = run(
            &mut ctx,
            &storage,
            2,
            vec![signed_tx(
                &key,
                TransactionKind::ClaimRewards { validator: None },
                1,
                1000,
            )],
        );
        assert!(r[0].success, "{r:?}");
        assert_eq!(read_balance(&storage, &me), before + 5 * K - 1000);
        // Round-2 reward accrued again after the claim tx.
        assert_eq!(pending_reward(&storage, &me, &validator), 5 * K);
        // Commission is paid to the validator payout.
        ctx.staking
            .set_commission(validator, 2_000)
            .expect("commission");
        run(&mut ctx, &storage, 3, vec![]);
        // 20% of 10 = 2, rest 8 split 4/4 → validator 6, delegator +4.
        assert_eq!(read_balance(&storage, &payout), 5 * K + 5 * K + 6 * K);
        assert_eq!(pending_reward(&storage, &me, &validator), 9 * K);
        // Claiming with nothing pending fails honestly.
        let k2 = test_keypair(2);
        let other = address_of(&k2);
        let txn = storage.begin_write().expect("w");
        storage.state().add_balance(&txn, &other, K).expect("fund");
        txn.commit().expect("c");
        let r = run(
            &mut ctx,
            &storage,
            4,
            vec![signed_tx(
                &k2,
                TransactionKind::ClaimRewards {
                    validator: Some(validator),
                },
                0,
                1000,
            )],
        );
        assert!(!r[0].success);
    }

    const TREASURY: Address = Address([0x7E; 32]);

    /// Validator [3;32] (50k self) + one 10k delegation, treasury configured.
    fn slash_fixture(storage: &Storage) -> ExecutionContext {
        let mut ctx = setup_funded_sender(storage, &Address([9u8; 32]), 0);
        ctx.init_treasury(TREASURY);
        ctx.staking
            .delegate(Address([9u8; 32]), Address([3u8; 32]), 10_000 * K)
            .expect("delegate");
        ctx
    }

    fn slashed_counter(storage: &Storage) -> Option<u64> {
        let txn = storage.begin_read().expect("r");
        let t = txn.open_table(SUPPLY_COUNTERS).ok()?;
        let v = t.get("slashed_to_treasury").expect("g").map(|x| x.value());
        v
    }

    /// liquid (treasury + delegator) + staked + unbonding.
    fn total_supply(ctx: &ExecutionContext, storage: &Storage) -> u64 {
        read_balance(storage, &TREASURY)
            + read_balance(storage, &Address([9u8; 32]))
            + ctx.staking.total_staked
            + ctx
                .staking
                .unbonding_queue
                .iter()
                .map(|e| e.amount)
                .sum::<u64>()
    }

    // Old behaviour: slashed stake was burned, treasury got nothing.
    #[test]
    fn double_sign_slash_moves_stake_to_treasury_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().join("t.redb")).expect("storage");
        let mut ctx = slash_fixture(&storage);
        let v = Address([3u8; 32]);
        let supply_before = total_supply(&ctx, &storage);
        let treasury_before = read_balance(&storage, &TREASURY);
        let vesting_before = bincode::serialize(&ctx.staking.treasury).expect("t");
        let ev = kvnc_staking::DoubleSignEvidence {
            validator: v,
            height: 1,
        };
        let slashed = ctx
            .apply_double_sign_evidence(ev.clone(), &storage)
            .expect("slash");
        assert_eq!(slashed, 3_000 * K);
        assert_eq!(read_balance(&storage, &TREASURY), treasury_before + slashed);
        assert_eq!(
            total_supply(&ctx, &storage),
            supply_before,
            "nothing burned"
        );
        assert_eq!(ctx.staking.total_staked, 57_000 * K);
        assert_eq!(slashed_counter(&storage), Some(slashed));
        assert_eq!(
            bincode::serialize(&ctx.staking.treasury).expect("t"),
            vesting_before,
            "vesting schedule untouched"
        );
        // Same evidence twice: rejected, nothing moves.
        let err = ctx
            .apply_double_sign_evidence(ev, &storage)
            .expect_err("dup");
        assert!(matches!(
            err,
            ExecutionError::Staking(StakingError::EvidenceProcessed)
        ));
        assert_eq!(read_balance(&storage, &TREASURY), treasury_before + slashed);
        assert_eq!(total_supply(&ctx, &storage), supply_before);
        assert_eq!(
            staking_state_bytes(&load_staking_state(&storage)),
            staking_state_bytes(&ctx.staking)
        );
    }

    #[test]
    fn slash_without_treasury_is_rejected_and_changes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().join("t.redb")).expect("storage");
        let mut ctx = setup_funded_sender(&storage, &Address([9u8; 32]), 0);
        let before = staking_state_bytes(&ctx.staking);
        let ev = kvnc_staking::DoubleSignEvidence {
            validator: Address([3u8; 32]),
            height: 1,
        };
        assert!(ctx.apply_double_sign_evidence(ev, &storage).is_err());
        assert_eq!(staking_state_bytes(&ctx.staking), before);
        assert_eq!(slashed_counter(&storage), None);
    }

    #[test]
    fn slash_to_treasury_replay_is_deterministic() {
        let go = || {
            let dir = tempfile::tempdir().expect("tempdir");
            let storage = Storage::new(dir.path().join("t.redb")).expect("storage");
            let mut ctx = slash_fixture(&storage);
            let mut out = Vec::new();
            for h in [1u64, 2, 2, 5] {
                let ev = kvnc_staking::DoubleSignEvidence {
                    validator: Address([3u8; 32]),
                    height: h,
                };
                out.push(ctx.apply_double_sign_evidence(ev, &storage).ok());
            }
            (
                out,
                staking_state_bytes(&ctx.staking),
                read_balance(&storage, &TREASURY),
                slashed_counter(&storage),
            )
        };
        let a = go();
        assert_eq!(a.0[2], None, "duplicate evidence rejected");
        assert_eq!(a, go());
    }

    #[test]
    fn staking_replay_is_deterministic() {
        let go = || {
            let dir = tempfile::tempdir().expect("tempdir");
            let storage = Storage::new(dir.path().join("t.redb")).expect("storage");
            let key = test_keypair(1);
            let me = address_of(&key);
            let validator = Address([3u8; 32]);
            let mut ctx = setup_funded_sender(&storage, &me, 200_000 * K);
            let mut receipts = Vec::new();
            receipts.extend(run(
                &mut ctx,
                &storage,
                1,
                vec![
                    signed_tx(
                        &key,
                        TransactionKind::Delegate {
                            validator,
                            amount: 1_000 * K,
                        },
                        0,
                        1000,
                    ),
                    signed_tx(
                        &key,
                        TransactionKind::Stake {
                            amount: kvnc_staking::MIN_VALIDATOR_STAKE,
                        },
                        1,
                        1000,
                    ),
                ],
            ));
            receipts.extend(run(
                &mut ctx,
                &storage,
                2,
                vec![
                    signed_tx(
                        &key,
                        TransactionKind::ClaimRewards { validator: None },
                        2,
                        1000,
                    ),
                    signed_tx(&key, TransactionKind::Unstake { amount: 10 * K }, 3, 1000),
                ],
            ));
            (
                bincode::serialize(&receipts).expect("r"),
                staking_state_bytes(&ctx.staking),
                read_balance(&storage, &me),
                pending_reward(&storage, &me, &validator),
            )
        };
        let a = go();
        assert_eq!(a, go());
    }
    // ---- signature format v1 + Call.value -------------------------------

    fn signed_tx_for(
        key: &SigningKey,
        kind: TransactionKind,
        nonce: u64,
        fee: u64,
        ctx: &SigningContext,
    ) -> Transaction {
        let mut tx = Transaction {
            sender: address_of(key),
            nonce,
            kind,
            fee,
            signature: Signature([0u8; 64]),
            hash: kvnc_types::hash::Hash([0u8; 32]),
        };
        tx.hash = tx.signing_hash(ctx);
        tx.signature = kvnc_crypto::sign(key, &tx.signing_hash(ctx).0);
        tx
    }

    // A valid v1 signature for another chain_id is rejected; nothing charged.
    #[test]
    fn v1_signature_for_other_chain_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().join("t.redb")).expect("storage");
        let key = test_keypair(1);
        let sender = address_of(&key);
        let mut ctx = setup_funded_sender(&storage, &sender, 1_000 * K);
        let testnet = SigningContext::new(chain_id::TESTNET);
        let kind = TransactionKind::Transfer {
            to: Address([2u8; 32]),
            amount: K,
        };
        let tx = signed_tx_for(&key, kind, 0, 1000, &testnet);
        assert!(tx.verify_signature(&testnet), "valid on its own chain");
        let r = execute_single_tx(&mut ctx, &storage, tx);
        assert!(!r.receipts[0].success);
        assert_eq!(r.receipts[0].error.as_deref(), Some("Invalid signature"));
        assert_eq!(read_balance(&storage, &sender), 1_000 * K);
        assert_eq!(read_nonce(&storage, &sender), 0);
        // The same chain configured via with_signing_context accepts it.
        let dir2 = tempfile::tempdir().expect("tempdir");
        let storage2 = Storage::new(dir2.path().join("t.redb")).expect("storage");
        let mut ctx2 = setup_funded_sender(&storage2, &sender, 1_000 * K);
        ctx2.signing_ctx = testnet;
        let tx = signed_tx_for(
            &key,
            TransactionKind::Transfer {
                to: Address([2u8; 32]),
                amount: K,
            },
            0,
            1000,
            &testnet,
        );
        assert!(execute_single_tx(&mut ctx2, &storage2, tx).receipts[0].success);
        assert_eq!(
            ExecutionContext::with_signing_context(testnet).signing_context(),
            testnet
        );
        assert_eq!(
            ExecutionContext::new().signing_context().chain_id,
            chain_id::LOCAL
        );
    }

    /// Pre-v1 (v0) signing preimage: no domain tag, no chain_id, keyed with
    /// the old `KVNC-TX-v1` BLAKE3 key.
    fn v0_signing_hash(tx: &Transaction) -> kvnc_types::hash::Hash {
        let TransactionKind::Transfer { to, amount } = &tx.kind else {
            unreachable!("transfer only")
        };
        let mut data = Vec::new();
        data.extend_from_slice(&tx.sender.0);
        data.extend_from_slice(&tx.nonce.to_le_bytes());
        data.push(0);
        data.extend_from_slice(&to.0);
        data.extend_from_slice(&amount.to_le_bytes());
        data.extend_from_slice(&tx.fee.to_le_bytes());
        kvnc_types::hash::Hash::new_keyed(b"KVNC-TX-v1", &data)
    }

    #[test]
    fn v0_signature_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().join("t.redb")).expect("storage");
        let key = test_keypair(1);
        let sender = address_of(&key);
        let mut ctx = setup_funded_sender(&storage, &sender, 1_000 * K);
        let mut tx = Transaction {
            sender,
            nonce: 0,
            kind: TransactionKind::Transfer {
                to: Address([2u8; 32]),
                amount: K,
            },
            fee: 1000,
            signature: Signature([0u8; 64]),
            hash: kvnc_types::hash::Hash([0u8; 32]),
        };
        let v0 = v0_signing_hash(&tx);
        tx.hash = v0;
        tx.signature = kvnc_crypto::sign(&key, &v0.0);
        let r = execute_single_tx(&mut ctx, &storage, tx);
        assert!(!r.receipts[0].success);
        assert_eq!(r.receipts[0].error.as_deref(), Some("Invalid signature"));
        assert_eq!(read_balance(&storage, &sender), 1_000 * K);
    }

    fn probe_call_value(key: &SigningKey, fail: bool, value: u64, nonce: u64) -> Transaction {
        let kind = TransactionKind::Call {
            contract: PROBE_CONTRACT,
            value,
            method: "probe".to_string(),
            args: if fail { vec![1] } else { Vec::new() },
            gas_limit: 1_000_000,
        };
        signed_tx(key, kind, nonce, 1000)
    }

    // Call.value moves exactly that amount signer -> contract, before the
    // call runs (the contract sees the reduced caller balance).
    #[test]
    fn call_value_moves_exact_amount_to_contract() {
        let (slot, sender_bal, contract_bal, ok) = with_timeout(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let storage = Storage::new(dir.path().join("t.redb")).expect("storage");
            let key = test_keypair(1);
            let sender = address_of(&key);
            let mut ctx = setup_funded_sender(&storage, &sender, 1_000_000);
            deploy_probe(&storage);
            let r = execute_single_tx(&mut ctx, &storage, probe_call_value(&key, false, 1234, 0));
            (
                probe_slot(&storage),
                read_balance(&storage, &sender),
                read_balance(&storage, &PROBE_CONTRACT),
                r.receipts[0].success,
            )
        });
        assert!(ok);
        assert_eq!(sender_bal, 1_000_000 - 1000 - 1234);
        assert_eq!(contract_bal, 1234);
        assert_eq!(slot, Some(1_000_000 - 1000 - 1234), "moved before the call");
    }

    // value > balance: rejected, only fee/nonce charged.
    #[test]
    fn call_value_above_balance_is_rejected() {
        let (ok, sender_bal, contract_bal, nonce) = with_timeout(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let storage = Storage::new(dir.path().join("t.redb")).expect("storage");
            let key = test_keypair(1);
            let sender = address_of(&key);
            let mut ctx = setup_funded_sender(&storage, &sender, 10_000);
            deploy_probe(&storage);
            let r = execute_single_tx(&mut ctx, &storage, probe_call_value(&key, false, 9_500, 0));
            (
                r.receipts[0].success,
                read_balance(&storage, &sender),
                read_balance(&storage, &PROBE_CONTRACT),
                read_nonce(&storage, &sender),
            )
        });
        assert!(!ok);
        assert_eq!(sender_bal, 10_000 - 1000);
        assert_eq!(contract_bal, 0);
        assert_eq!(nonce, 1);
    }

    // A failing call does not move its value.
    #[test]
    fn failed_call_does_not_move_value() {
        let (ok, sender_bal, contract_bal, slot) = with_timeout(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let storage = Storage::new(dir.path().join("t.redb")).expect("storage");
            let key = test_keypair(1);
            let sender = address_of(&key);
            let mut ctx = setup_funded_sender(&storage, &sender, 1_000_000);
            deploy_probe(&storage);
            let r = execute_single_tx(&mut ctx, &storage, probe_call_value(&key, true, 5_000, 0));
            (
                r.receipts[0].success,
                read_balance(&storage, &sender),
                read_balance(&storage, &PROBE_CONTRACT),
                probe_slot(&storage),
            )
        });
        assert!(!ok);
        assert_eq!(sender_bal, 1_000_000 - 1000);
        assert_eq!(contract_bal, 0);
        assert_eq!(slot, None);
    }

    #[test]
    fn replay_with_call_values_is_deterministic() {
        let go = || {
            with_timeout(|| {
                let dir = tempfile::tempdir().expect("tempdir");
                let storage = Storage::new(dir.path().join("t.redb")).expect("storage");
                let key = test_keypair(1);
                let sender = address_of(&key);
                let mut ctx = setup_funded_sender(&storage, &sender, 1_000_000);
                deploy_probe(&storage);
                let txs = vec![
                    probe_call_value(&key, false, 100, 0),
                    probe_call_value(&key, true, 200, 1),
                    probe_call_value(&key, false, 10_000_000, 2),
                    probe_call_value(&key, false, 300, 3),
                ];
                let r = ctx
                    .execute_committed_subdag(&subdag_at(1, txs), &storage)
                    .expect("execute");
                (
                    bincode::serialize(&r.receipts).expect("r"),
                    read_balance(&storage, &sender),
                    read_balance(&storage, &PROBE_CONTRACT),
                    probe_slot(&storage),
                )
            })
        };
        let a = go();
        assert_eq!(a.2, 400);
        assert_eq!(a, go());
    }

    // --- state sync gate (StartupSyncState, contract agreed with Network) ---
    mod sync_gate {
        use super::*;

        const PAYOUT: Address = Address([9u8; 32]);

        fn genesis_storage() -> (tempfile::TempDir, Storage, ExecutionContext) {
            let dir = tempfile::tempdir().expect("tempdir");
            let storage = Storage::new(dir.path().join("g.redb")).expect("storage");
            let mut ctx = ExecutionContext::new();
            ctx.staking
                .join_validator(
                    Address([1u8; 32]),
                    kvnc_staking::MIN_VALIDATOR_STAKE,
                    0,
                    Some(PAYOUT),
                    None,
                )
                .expect("join");
            let txn = storage.begin_write().expect("w");
            storage
                .state()
                .save_staking_state(&txn, &ctx.staking)
                .expect("save");
            txn.commit().expect("commit");
            (dir, storage, ctx)
        }

        fn subdag(round: u64) -> CommittedSubDag {
            let leader = sample_block_at(0, round);
            CommittedSubDag {
                leader: leader.clone(),
                blocks: vec![leader],
                leader_round: round,
                leader_author: 0,
            }
        }

        fn count<K: redb::Key + 'static, V: redb::Value + 'static>(
            storage: &Storage,
            def: TableDefinition<K, V>,
        ) -> usize {
            let r = storage.begin_read().expect("r");
            match r.open_table(def) {
                Ok(t) => t.iter().expect("iter").count(),
                Err(_) => 0,
            }
        }

        fn persisted_staking(storage: &Storage) -> Vec<u8> {
            let r = storage.begin_read().expect("r");
            let t = r
                .open_table(kvnc_storage::tables::STAKING_STATE)
                .expect("t");
            t.get("staking").expect("get").expect("some").value()
        }

        #[test]
        fn all_eight_combinations() {
            for store_was_empty in [false, true] {
                for network_ahead in [false, true] {
                    for is_genesis in [false, true] {
                        let expected = !network_ahead && (!store_was_empty || is_genesis);
                        let startup = StartupSyncState {
                            store_was_empty,
                            network_ahead,
                        };
                        assert_eq!(startup.allows(is_genesis), expected);
                        let (_d, storage, mut ctx) = genesis_storage();
                        let staking_before = persisted_staking(&storage);
                        let mem_before = staking_state_bytes(&ctx.staking);
                        let case = format!("{startup:?} genesis={is_genesis}");
                        let r = ctx.execute_committed_subdag_gated(
                            &subdag(1),
                            &storage,
                            &startup,
                            is_genesis,
                        );
                        if expected {
                            assert!(r.expect(&case).reward.is_some(), "{case}");
                            assert_eq!(count(&storage, EXECUTED_SUBDAGS), 1, "{case}");
                        } else {
                            let err = r.expect_err(&case);
                            assert!(matches!(err, ExecutionError::NeedsStateSync), "{case}");
                            assert_eq!(err.to_string(), "degraded: needs state sync");
                            // Zero writes: no mark, no receipts, no staking save, no reward.
                            assert_eq!(count(&storage, EXECUTED_SUBDAGS), 0, "{case}");
                            assert_eq!(count(&storage, TX_RECEIPTS), 0, "{case}");
                            assert_eq!(persisted_staking(&storage), staking_before, "{case}");
                            assert_eq!(read_balance(&storage, &PAYOUT), 0, "{case}");
                            assert_eq!(staking_state_bytes(&ctx.staking), mem_before, "{case}");
                        }
                    }
                }
            }
        }

        #[test]
        fn restart_replay_stays_idempotent_through_gate() {
            let (_d, storage, mut ctx) = genesis_storage();
            let fresh = StartupSyncState {
                store_was_empty: true,
                network_ahead: false,
            };
            ctx.execute_committed_subdag_gated(&subdag(1), &storage, &fresh, true)
                .expect("genesis");
            for round in 2..=3 {
                ctx.execute_committed_subdag_gated(&subdag(round), &storage, &fresh, false)
                    .expect_err("non-genesis on empty store is gated");
                ctx.execute_committed_subdag(&subdag(round), &storage)
                    .expect("ungated for setup");
            }
            let balance = read_balance(&storage, &PAYOUT);

            // Restart: non-empty store, network not ahead, same staking as run_execution loads.
            let restart = StartupSyncState::default();
            let mut ctx2 = ExecutionContext::new();
            ctx2.staking = {
                let r = storage.begin_read().expect("r");
                storage.state().load_staking_state(&r).expect("load")
            };
            for round in 1..=3 {
                let r = ctx2
                    .execute_committed_subdag_gated(&subdag(round), &storage, &restart, false)
                    .expect("replay");
                assert!(r.reward.is_none() && r.receipts.is_empty(), "no-op replay");
            }
            assert_eq!(read_balance(&storage, &PAYOUT), balance);
            assert_eq!(count(&storage, EXECUTED_SUBDAGS), 3);
            assert_eq!(count(&storage, TX_RECEIPTS), 3);
            let r = ctx2
                .execute_committed_subdag_gated(&subdag(4), &storage, &restart, false)
                .expect("next");
            assert!(r.reward.is_some());
            assert_eq!(ctx2.staking.committed_leader_height, 4);
        }
    }
}
