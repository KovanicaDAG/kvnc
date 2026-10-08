//! State storage for KVNC.
//!
//! Stores account balances, nonces, contract code/storage, and staking state.

use crate::{address_to_bytes, BincodeSerialize, StorageError};
use kvnc_staking::{Delegation, StakingState, TreasuryState, ValidatorInfo};
use kvnc_types::{Address, Stake};
use redb::{ReadTransaction, ReadableTable, WriteTransaction};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
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
        let value = table.get(key)?;
        Ok(value
            .map(|v| Account::from_bytes(&v.value()).unwrap_or_default())
            .unwrap_or_default())
    }

    /// Get an account, returning default if not found (write transaction variant).
    pub fn get_account_or_default_write(
        &self,
        txn: &WriteTransaction,
        address: &Address,
    ) -> Result<Account, StateStoreError> {
        let key = address_to_bytes(address);
        let table = txn.open_table(crate::tables::ACCOUNTS)?;
        let value = table.get(key)?;
        let account = value
            .map(|v| Account::from_bytes(&v.value()).unwrap_or_default())
            .unwrap_or_default();
        drop(table);
        Ok(account)
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
        let key = address_to_bytes(address);
        let table = txn.open_table(crate::tables::ACCOUNTS)?;
        let value = table.get(key)?;
        let mut account = value
            .map(|v| Account::from_bytes(&v.value()).unwrap_or_default())
            .unwrap_or_default();
        drop(table);
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
        let value = table.get(key)?;
        let account = value
            .ok_or_else(|| StateStoreError::NotFound(format!("account {}", address)))?;
        let mut account = Account::from_bytes(&account.value())?;
        // table is dropped here after account is extracted
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
        let value = table.get(key)?;
        let mut account = value
            .map(|v| Account::from_bytes(&v.value()).unwrap_or_default())
            .unwrap_or_default();
        drop(table);
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
        let value = table.get(key)?;
        let mut account = value
            .map(|v| Account::from_bytes(&v.value()).unwrap_or_default())
            .unwrap_or_default();
        drop(table);
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
        let value = table.get(key)?;
        let mut account = value
            .map(|v| Account::from_bytes(&v.value()).unwrap_or_default())
            .unwrap_or_default();
        drop(table);
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


    // ============================================================
    // Sorted KV Merkle root
    // ============================================================

    /// Compute a deterministic sorted key-value Merkle root over all
    /// state tables (accounts, contract code, contract storage,
    /// staking state). Each leaf = H(key || value) using BLAKE3.
    /// Keys are sorted lexicographically (as byte arrays) before
    /// hashing so the result is deterministic.
    pub fn compute_state_root(
        &self,
        txn: &ReadTransaction,
    ) -> Result<kvnc_types::hash::Hash, StateStoreError> {
        let mut entries: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();

        // Accounts table
        {
            let table = txn.open_table(crate::tables::ACCOUNTS)?;
            for entry in table.iter()? {
                let (key_bytes, value_bytes) = entry?;
                let mut leaf_key = key_bytes.value().to_vec();
                leaf_key.extend_from_slice(value_bytes.value());
                entries.insert(key_bytes.value().to_vec(), leaf_key);
            }
        }

        // Contract code table
        {
            let table = txn.open_table(crate::tables::CONTRACT_CODE)?;
            for entry in table.iter()? {
                let (key_bytes, value_bytes) = entry?;
                let mut leaf_key = b"code:".to_vec();
                leaf_key.extend_from_slice(key_bytes.value());
                let mut combined = leaf_key.clone();
                combined.extend_from_slice(value_bytes.value());
                entries.insert(leaf_key, combined);
            }
        }

        // Contract storage table
        {
            let table = txn.open_table(crate::tables::CONTRACT_STORAGE)?;
            for entry in table.iter()? {
                let (key_pair_bytes, value_bytes) = entry?;
                let key_pair: ([u8; 32], [u8; 32]) = bincode::deserialize(key_pair_bytes.value())?;
                let mut leaf_key = b"storage:"
                    .iter()
                    .cloned()
                    .collect::<Vec<u8>>();
                leaf_key.extend_from_slice(&key_pair.0);
                leaf_key.extend_from_slice(&key_pair.1);
                let mut combined = leaf_key.clone();
                combined.extend_from_slice(value_bytes.value());
                entries.insert(leaf_key, combined);
            }
        }

        // Staking state table
        {
            let table = txn.open_table(crate::tables::STAKING_STATE)?;
            for entry in table.iter()? {
                let (key_bytes, value_bytes) = entry?;
                let mut leaf_key = b"staking:"
                    .iter()
                    .cloned()
                    .collect::<Vec<u8>>();
                leaf_key.extend_from_slice(key_bytes.value());
                let mut combined = leaf_key.clone();
                combined.extend_from_slice(value_bytes.value());
                entries.insert(leaf_key, combined);
            }
        }

        let mut leaves: Vec<[u8; 32]> = Vec::with_capacity(entries.len());
        for (_, combined) in entries {
            let hash = kvnc_types::hash::Hash::new(&combined);
            leaves.push(hash.0);
        }
        Ok(kvnc_types::hash::Hash(merkle_root(leaves)))
    }

    /// Export a snapshot of all state data to a file using bincode.
    pub fn export_snapshot<P: AsRef<std::path::Path>>(
        &self,
        txn: &ReadTransaction,
        path: P,
    ) -> Result<(), StateStoreError> {
        use std::fs::File;
        use std::io::Write;

        #[derive(Serialize, Deserialize, Debug, Default)]
        struct SnapshotData {
            accounts: Vec<(Vec<u8>, Vec<u8>)>,
            contract_code: Vec<(Vec<u8>, Vec<u8>)>,
            contract_storage: Vec<(Vec<u8>, Vec<u8>)>,
            staking_state: Vec<(Vec<u8>, Vec<u8>)>,
        }

        let mut data = SnapshotData::default();
        {
            let table = txn.open_table(crate::tables::ACCOUNTS)?;
            for entry in table.iter()? {
                let (k, v) = entry?;
                data.accounts.push((k.value().to_vec(), v.value().to_vec()));
            }
        }
        {
            let table = txn.open_table(crate::tables::CONTRACT_CODE)?;
            for entry in table.iter()? {
                let (k, v) = entry?;
                data.contract_code.push((k.value().to_vec(), v.value().to_vec()));
            }
        }
        {
            let table = txn.open_table(crate::tables::CONTRACT_STORAGE)?;
            for entry in table.iter()? {
                let (k, v) = entry?;
                let k_bytes = bincode::serialize(&k.value()).unwrap();
                data.contract_storage.push((k_bytes, v.value().to_vec()));
            }
        }
        {
            let table = txn.open_table(crate::tables::STAKING_STATE)?;
            for entry in table.iter()? {
                let (k, v) = entry?;
                data.staking_state.push((k.value().as_bytes().to_vec(), v.value().to_vec()));
            }
        }

        let bytes = bincode::serialize(&data)?;
        let mut file = File::create(path.as_ref()).map_err(|e| {
            StateStoreError::NotFound(format!("snapshot file error: {e}"))
        })?;
        file.write_all(&bytes)
            .map_err(|e| StateStoreError::NotFound(format!("snapshot write error: {e}")))?;
        Ok(())
    }

    /// Restore a snapshot from a file. Requires a write transaction.
    pub fn import_snapshot<P: AsRef<std::path::Path>>(
        &self,
        txn: &WriteTransaction,
        path: P,
    ) -> Result<(), StateStoreError> {
        use std::fs;

        #[derive(Serialize, Deserialize, Debug, Default)]
        struct SnapshotData {
            accounts: Vec<(Vec<u8>, Vec<u8>)>,
            contract_code: Vec<(Vec<u8>, Vec<u8>)>,
            contract_storage: Vec<(Vec<u8>, Vec<u8>)>,
            staking_state: Vec<(Vec<u8>, Vec<u8>)>,
        }

        let bytes = fs::read(path.as_ref()).map_err(|e| {
            StateStoreError::NotFound(format!("snapshot read error: {e}"))
        })?;
        let data: SnapshotData = bincode::deserialize(&bytes)?;

        {
            let table = txn.open_table(crate::tables::ACCOUNTS)?;
            let keys: Vec<[u8; 32]> = table.iter()?.filter_map(|e| e.ok()).map(|(k,_)| k.value()).collect();
            drop(table);
            let mut table = txn.open_table(crate::tables::ACCOUNTS)?;
            for k in keys { table.remove(k)?; }
            for (k, v) in data.accounts {
                let k32: [u8; 32] = k.try_into().unwrap_or([0; 32]);
                table.insert(k32, v)?;
            }
        }
        {
            let table = txn.open_table(crate::tables::CONTRACT_CODE)?;
            let keys: Vec<[u8; 32]> = table.iter()?.filter_map(|e| e.ok()).map(|(k,_)| k.value()).collect();
            drop(table);
            let mut table = txn.open_table(crate::tables::CONTRACT_CODE)?;
            for k in keys { table.remove(k)?; }
            for (k, v) in data.contract_code {
                let k32: [u8; 32] = k.try_into().unwrap_or([0; 32]);
                table.insert(k32, v)?;
            }
        }
        {
            let table = txn.open_table(crate::tables::CONTRACT_STORAGE)?;
            let keys: Vec<([u8; 32], [u8; 32])> = table.iter()?.filter_map(|e| e.ok()).map(|(k,_)| {
                let b = k.value();
                bincode::deserialize(&b[..]).unwrap_or_default()
            }).collect();
            drop(table);
            let mut table = txn.open_table(crate::tables::CONTRACT_STORAGE)?;
            for k in keys { table.remove(k)?; }
            for (k, v) in data.contract_storage {
                let k_tuple: ([u8; 32], [u8; 32]) = bincode::deserialize(&k)?;
                table.insert(k_tuple, v)?;
            }
        }
        {
            let table = txn.open_table(crate::tables::STAKING_STATE)?;
            let keys: Vec<String> = table.iter()?.filter_map(|e| e.ok()).map(|(k,_)| k.value().to_string()).collect();
            drop(table);
            let mut table = txn.open_table(crate::tables::STAKING_STATE)?;
            for k in keys { table.remove(k.as_str())?; }
            for (k, v) in data.staking_state {
                table.insert("staking", v)?;
            }
        }
        Ok(())
    }
}


/// Compute binary Merkle root from sorted leaf hashes.
fn merkle_root(mut leaves: Vec<[u8; 32]>) -> [u8; 32] {
    if leaves.is_empty() {
        return [0u8; 32];
    }
    while leaves.len() > 1 {
        if leaves.len() % 2 == 1 {
            leaves.push(leaves[leaves.len() - 1]);
        }
        let mut next_level = Vec::with_capacity(leaves.len() / 2);
        for pair in leaves.chunks(2) {
            let mut combined = pair[0].to_vec();
            combined.extend_from_slice(&pair[1]);
            let hash = kvnc_types::hash::Hash::new(&combined);
            next_level.push(hash.0);
        }
        leaves = next_level;
    }
    leaves[0]
}
