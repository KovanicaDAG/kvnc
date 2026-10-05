//! State storage for KVNC.
//!
//! Stores account balances, nonces, contract code/storage, and staking state.

use crate::{address_to_bytes, BincodeSerialize, StorageError};
use kvnc_staking::{Delegation, StakingState, TreasuryState, ValidatorInfo};
use kvnc_types::{Address, Stake};
use redb::{ReadTransaction, ReadableTable, WriteTransaction};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

/// Errors specific to state store operations.
#[derive(Debug, Error)]
pub enum StateStoreError {
    #[error("Account not found: {0}")]
    NotFound(String),
    #[error("Serialization error: {0}")]
    Serialization(#[from] bincode::Error),
    #[error("Database error: {0}")]
    Database(#[from] redb::DatabaseError),
    #[error("Transaction error: {0}")]
    Transaction(#[from] redb::TransactionError),
    #[error("Table error: {0}")]
    Table(#[from] redb::TableError),
    #[error("Commit error: {0}")]
    Commit(#[from] redb::CommitError),
    #[error("Storage error: {0}")]
    Storage(#[from] redb::StorageError),
}

/// Account state in the KVNC blockchain.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Account {
    /// Account balance in base units.
    pub balance: u64,
    /// Transaction nonce (prevents replay).
    pub nonce: u64,
    /// Contract code hash (empty for EOAs).
    pub code_hash: [u8; 32],
    /// Contract code (only for contract accounts).
    pub code: Vec<u8>,
}

/// State store for accounts, contracts, and staking.
pub struct StateStore;

impl StateStore {
    /// Create a new state store, initializing tables if needed.
    pub fn new(db: &redb::Database) -> Result<Self, StorageError> {
        let write_txn = db.begin_write().map_err(StorageError::Transaction)?;
        {
            let _ = write_txn.open_table(crate::tables::ACCOUNTS)?;
            let _ = write_txn.open_table(crate::tables::CONTRACT_CODE)?;
            let _ = write_txn.open_table(crate::tables::CONTRACT_STORAGE)?;
            let _ = write_txn.open_table(crate::tables::STAKING_STATE)?;
            let _ = write_txn.open_table(crate::tables::STATE_ROOT)?;
        }
        write_txn.commit().map_err(StorageError::Commit)?;
        Ok(Self)
    }

    // ============================================================
    // Account operations
    /// Get an account by address.
    pub fn get_account(
        &self,
        txn: &ReadTransaction,
        address: &Address,
    ) -> Result<Account, StateStoreError> {
        let key = address_to_bytes(address);
        let table = txn.open_table(crate::tables::ACCOUNTS)?;
        let value = table
            .get(key)?
            .ok_or_else(|| StateStoreError::NotFound(format!("account {}", address)))?;
        Ok(Account::from_bytes(&value.value())?)
    }

    /// Get an account, returning default if not found.
    pub fn get_account_or_default(
        &self,
        txn: &ReadTransaction,
        address: &Address,
    ) -> Result<Account, StateStoreError> {
        let key = address_to_bytes(address);
        let table = txn.open_table(crate::tables::ACCOUNTS)?;
        Ok(table
            .get(key)?
            .map(|v| Account::from_bytes(&v.value()).unwrap_or_default())
            .unwrap_or_default())
    }

    /// Set an account.
    pub fn set_account(
        &self,
        txn: &WriteTransaction,
        address: &Address,
        account: &Account,
    ) -> Result<(), StateStoreError> {
        let key = address_to_bytes(address);
        let mut table = txn.open_table(crate::tables::ACCOUNTS)?;
        table.insert(key, account.to_bytes()?)?;
        Ok(())
    }

    /// Update account balance.
    pub fn add_balance(
        &self,
        txn: &WriteTransaction,
        address: &Address,
        amount: u64,
    ) -> Result<u64, StateStoreError> {
        let read_txn = txn.open_table(crate::tables::ACCOUNTS)?;
        let key = address_to_bytes(address);
        let mut account = read_txn
            .get(key)?
            .map(|v| Account::from_bytes(&v.value()).unwrap_or_default())
            .unwrap_or_default();
        drop(read_txn);
        account.balance = account.balance.saturating_add(amount);
        self.set_account(txn, address, &account)?;
        Ok(account.balance)
    }

    /// Subtract from account balance.
    pub fn sub_balance(
        &self,
        txn: &WriteTransaction,
        address: &Address,
        amount: u64,
    ) -> Result<u64, StateStoreError> {
        let key = address_to_bytes(address);
        let table = txn.open_table(crate::tables::ACCOUNTS)?;
        let mut account = table
            .get(key)?
            .ok_or_else(|| StateStoreError::NotFound(format!("account {}", address)))?;
        let mut account = Account::from_bytes(&account.value())?;
        account.balance = account.balance.saturating_sub(amount);
        self.set_account(txn, address, &account)?;
        Ok(account.balance)
    }

    /// Increment account nonce.
    pub fn increment_nonce(
        &self,
        txn: &WriteTransaction,
        address: &Address,
    ) -> Result<u64, StateStoreError> {
        let key = address_to_bytes(address);
        let table = txn.open_table(crate::tables::ACCOUNTS)?;
        let mut account = table
            .get(key)?
            .map(|v| Account::from_bytes(&v.value()).unwrap_or_default())
            .unwrap_or_default();
        account.nonce = account.nonce.saturating_add(1);
        self.set_account(txn, address, &account)?;
        Ok(account.nonce)
    }

    /// Set account nonce.
    pub fn set_nonce(
        &self,
        txn: &WriteTransaction,
        address: &Address,
        nonce: u64,
    ) -> Result<(), StateStoreError> {
        let key = address_to_bytes(address);
        let table = txn.open_table(crate::tables::ACCOUNTS)?;
        let mut account = table
            .get(key)?
            .map(|v| Account::from_bytes(&v.value()).unwrap_or_default())
            .unwrap_or_default();
        account.nonce = nonce;
        self.set_account(txn, address, &account)?;
        Ok(())
    }

    // ============================================================
    // Contract operations
    // ============================================================

    /// Deploy contract code.
    pub fn deploy_contract(
        &self,
        txn: &WriteTransaction,
        address: &Address,
        code: Vec<u8>,
    ) -> Result<[u8; 32], StateStoreError> {
        // Compute code hash
        let code_hash = blake3::hash(&code);
        let code_hash_bytes: [u8; 32] = code_hash.as_bytes().clone().try_into().unwrap();

        // Store code
        {
            let mut table = txn.open_table(crate::tables::CONTRACT_CODE)?;
            table.insert(code_hash_bytes, code.clone())?;
        }

        // Update account
        let key = address_to_bytes(address);
        let table = txn.open_table(crate::tables::ACCOUNTS)?;
        let mut account = table
            .get(key)?
            .map(|v| Account::from_bytes(&v.value()).unwrap_or_default())
            .unwrap_or_default();
        account.code_hash = code_hash_bytes;
        account.code = code;
        self.set_account(txn, address, &account)?;

        Ok(code_hash_bytes)
    }

    /// Get contract code by hash.
    pub fn get_contract_code(
        &self,
        txn: &ReadTransaction,
        code_hash: &[u8; 32],
    ) -> Result<Option<Vec<u8>>, StateStoreError> {
        let table = txn.open_table(crate::tables::CONTRACT_CODE)?;
        Ok(table.get(*code_hash)?.map(|v| v.value().to_vec()))
    }

    /// Get contract storage value.
    pub fn get_storage(
        &self,
        txn: &ReadTransaction,
        contract: &Address,
        key: &[u8; 32],
    ) -> Result<Option<Vec<u8>>, StateStoreError> {
        let contract_bytes = address_to_bytes(contract);
        let table = txn.open_table(crate::tables::CONTRACT_STORAGE)?;
        Ok(table
            .get((contract_bytes, *key))?
            .map(|v| v.value().to_vec()))
    }

    /// Set contract storage value.
    pub fn set_storage(
        &self,
        txn: &WriteTransaction,
        contract: &Address,
        key: [u8; 32],
        value: Vec<u8>,
    ) -> Result<(), StateStoreError> {
        let contract_bytes = address_to_bytes(contract);
        let mut table = txn.open_table(crate::tables::CONTRACT_STORAGE)?;
        table.insert((contract_bytes, key), value)?;
        Ok(())
    }

    // ============================================================
    // Staking state operations
    // ============================================================

    /// Save the entire staking state.
    pub fn save_staking_state(
        &self,
        txn: &WriteTransaction,
        state: &StakingState,
    ) -> Result<(), StateStoreError> {
        let mut table = txn.open_table(crate::tables::STAKING_STATE)?;
        table.insert("staking", state.to_bytes()?)?;
        Ok(())
    }

    /// Load the staking state.
    pub fn load_staking_state(
        &self,
        txn: &ReadTransaction,
    ) -> Result<StakingState, StateStoreError> {
        let table = txn.open_table(crate::tables::STAKING_STATE)?;
        let value = table
            .get("staking")?
            .ok_or_else(|| StateStoreError::NotFound("staking state".to_string()))?;
        Ok(StakingState::from_bytes(&value.value())?)
    }

    // ============================================================
    // State root operations (for snapshots/light clients)
    // ============================================================

    /// Save state root for a committed leader height.
    pub fn save_state_root(
        &self,
        txn: &WriteTransaction,
        committed_leader_height: u64,
        state_root: &kvnc_types::hash::Hash,
    ) -> Result<(), StateStoreError> {
        let mut table = txn.open_table(crate::tables::STATE_ROOT)?;
        table.insert(&committed_leader_height, state_root.0)?;
        Ok(())
    }

    /// Get state root for a committed leader height.
    pub fn get_state_root(
        &self,
        txn: &ReadTransaction,
        committed_leader_height: u64,
    ) -> Result<Option<kvnc_types::hash::Hash>, StateStoreError> {
        let table = txn.open_table(crate::tables::STATE_ROOT)?;
        Ok(table
            .get(&committed_leader_height)?
            .map(|v| kvnc_types::hash::Hash(v.value())))
    }

    /// Get latest state root.
    pub fn get_latest_state_root(
        &self,
        txn: &ReadTransaction,
    ) -> Result<Option<(u64, kvnc_types::hash::Hash)>, StateStoreError> {
        let table = txn.open_table(crate::tables::STATE_ROOT)?;
        let mut latest = None;
        for entry in table.iter()? {
            let (height, hash) = entry?;
            latest = Some((height.value(), kvnc_types::hash::Hash(hash.value())));
        }
        Ok(latest)
    }
}
