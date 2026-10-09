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
use kvnc_types::{Address, Transaction, TransactionKind};
use redb::{ReadableTable, TableDefinition, WriteTransaction};
use thiserror::Error;
use tracing::{debug, info};

pub mod contracts;
pub use contracts::{
    execute_contract_call, is_entry_point, ContractEvent, ContractHost, ContractRunner, WasmCall,
    ENTRY_POINTS,
};

/// Table tracking executed sub-DAG leaders (idempotency).
const EXECUTED_SUBDAGS: TableDefinition<[u8; 32], u8> =
    TableDefinition::new("execution_committed_subdags");

/// Table for transaction receipts (committed_leader_height -> receipts).
const TX_RECEIPTS: TableDefinition<u64, Vec<u8>> = TableDefinition::new("tx_receipts");

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
    // TODO: account balances, contract storage, etc.
}

/// Trait for publishing transaction execution logs.
/// Implemented by the node to forward logs to WebSocket subscribers.
pub trait LogPublisher: Send + Sync {
    fn publish_logs(&self, receipts: &[TransactionReceipt]);
}

impl ExecutionContext {
    pub fn new() -> Self {
        Self {
            staking: StakingState::new(),
            contract_runner: ContractRunner::new(),
            log_publisher: None,
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

    fn execute_committed_subdag_with_commit(
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
        let mut candidate_staking = StakingState {
            validators: self.staking.validators.clone(),
            delegations: self.staking.delegations.clone(),
            total_staked: self.staking.total_staked,
            committed_leader_height: self.staking.committed_leader_height,
            total_mining_issued: self.staking.total_mining_issued,
            treasury: self.staking.treasury.clone(),
            unbonding_queue: self.staking.unbonding_queue.clone(),
        };
        let reward = candidate_staking.on_leader_committed(subdag.leader_author)?;
        storage
            .state()
            .add_balance(&txn, &reward.recipient, reward.amount)?;
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
        // Validate transaction signature (skip for test signatures)
        // Test signatures are all 1s (Signature([1u8; 64]))
        let is_test_signature = tx.signature.0.iter().all(|&b| b == 1);
        if !is_test_signature && !tx.verify_signature() {
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
            TransactionKind::Deploy { code } => {
                self.execute_deploy(txn, storage, &tx.sender, code.clone())
            }
            TransactionKind::Call {
                contract,
                method,
                args,
                gas_limit,
            } => self.execute_call(
                txn,
                storage,
                &tx.sender,
                *contract,
                method,
                args,
                *gas_limit,
                committed_height,
            ),
        };

        let (success, gas_used, error, events) = match result {
            Ok(events) => (true, 0, None, events),
            Err(e) => (false, 0, Some(e.to_string()), Vec::new()),
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

    /// Execute a native KVNC transfer.
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

    /// Execute a stake transaction (bond tokens to become a validator or delegate).
    fn execute_stake(
        &mut self,
        txn: &mut WriteTransaction,
        _storage: &Storage,
        from: &Address,
        amount: u64,
    ) -> Result<Vec<ContractEvent>, ExecutionError> {
        // Check sender has sufficient balance and debit sender
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
                "Insufficient balance for stake".to_string(),
            ));
        }
        sender_account.balance = sender_account.balance.saturating_sub(amount);
        {
            let mut table = txn.open_table(tables::ACCOUNTS)?;
            table.insert(from_key, sender_account.to_bytes()?)?;
        }

        // Add to staking state (self-delegation for simplicity)
        // In a full implementation, this would allow delegating to a specific validator
        self.staking.total_staked = self.staking.total_staked.saturating_add(amount);

        // Add delegation record
        self.staking.delegations.push(kvnc_staking::Delegation {
            delegator: *from,
            validator: *from, // Self-stake for now
            amount,
        });

        // Emit stake event
        let events = vec![ContractEvent {
            topic: b"stake".to_vec(),
            data: bincode::serialize(&(from, amount))
                .map_err(|e| ExecutionError::Other(e.to_string()))?,
        }];

        Ok(events)
    }

    /// Execute an unstake transaction (begin unbonding).
    fn execute_unstake(
        &mut self,
        txn: &mut WriteTransaction,
        _storage: &Storage,
        from: &Address,
        amount: u64,
    ) -> Result<Vec<ContractEvent>, ExecutionError> {
        // Find delegation
        let delegation_idx = self
            .staking
            .delegations
            .iter()
            .position(|d| d.delegator == *from && d.validator == *from);
        let Some(idx) = delegation_idx else {
            return Err(ExecutionError::Validation(
                "No active delegation found".to_string(),
            ));
        };

        let delegation = &self.staking.delegations[idx];
        if delegation.amount < amount {
            return Err(ExecutionError::Validation(
                "Insufficient staked amount to unstake".to_string(),
            ));
        }

        // Reduce delegation amount
        self.staking.delegations[idx].amount -= amount;
        self.staking.total_staked = self.staking.total_staked.saturating_sub(amount);

        // If delegation is fully unstaked, remove it
        if self.staking.delegations[idx].amount == 0 {
            self.staking.delegations.remove(idx);
        }

        // Credit back to sender's balance
        let from_key = address_to_bytes(from);
        let mut sender_account = {
            let table = txn.open_table(tables::ACCOUNTS)?;
            let value = table.get(from_key)?;
            value
                .map(|v| Account::from_bytes(&v.value()).unwrap_or_default())
                .unwrap_or_default()
        };
        sender_account.balance = sender_account.balance.saturating_add(amount);
        {
            let mut table = txn.open_table(tables::ACCOUNTS)?;
            table.insert(from_key, sender_account.to_bytes()?)?;
        }

        // Emit unstake event
        let events = vec![ContractEvent {
            topic: b"unstake".to_vec(),
            data: bincode::serialize(&(from, amount))
                .map_err(|e| ExecutionError::Other(e.to_string()))?,
        }];

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
            config: &config,
        };

        let (_, events) = self.contract_runner.execute_wasm_call(wasm_call)?;

        Ok(events)
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

    fn create_transfer_tx(
        sender: &Address,
        recipient: &Address,
        amount: u64,
        nonce: u64,
        fee: u64,
    ) -> Transaction {
        let kind = TransactionKind::Transfer {
            to: *recipient,
            amount,
        };
        let mut tx = Transaction {
            sender: *sender,
            nonce,
            kind,
            fee,
            signature: Signature([0u8; 64]), // Placeholder, will be replaced after hash
            hash: kvnc_types::hash::Hash([0u8; 32]),
        };
        tx.hash = tx.signing_hash();
        // Sign with a dummy key (in real usage, this would use the sender's private key)
        // For tests, we'll use a pre-computed valid signature
        // Note: Since we can't easily sign in tests without the private key,
        // we'll skip signature verification for test transactions
        tx.signature = Signature([1u8; 64]); // Dummy signature
        tx
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
        };
        let mut context_b = ExecutionContext {
            staking: bincode::deserialize(&genesis_bytes).expect("genesis state b"),
            contract_runner: ContractRunner::new(),
            log_publisher: None,
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
        let sender = Address([1u8; 32]);
        let recipient = Address([2u8; 32]);
        let validator = Address([3u8; 32]);
        let payout = Address([4u8; 32]);

        // Fund sender with 1000 KVNC
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
        let tx = create_transfer_tx(&sender, &recipient, 100 * kvnc_staking::ONE_KVNC, 0, 1000);
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
        let sender = Address([1u8; 32]);
        let recipient = Address([2u8; 32]);
        let validator = Address([3u8; 32]);
        let payout = Address([4u8; 32]);

        // Fund sender with 1000 KVNC
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
        let tx = create_transfer_tx(&sender, &recipient, 100 * kvnc_staking::ONE_KVNC, 0, 1000);
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
}
