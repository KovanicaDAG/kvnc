//! Persistent storage layer for KVNC (redb backend + pruning).
//!
//! Provides three main storage components:
//! - `BlockStore`: Blocks, transactions, and block index
//! - `StateStore`: Account balances, nonces, contract code/storage, staking state
//! - `ConsensusStore`: DAG structure, commit tracker, decided rounds

#![deny(unsafe_code)]
#![allow(clippy::result_large_err)]
#![allow(clippy::large_enum_variant)]
#![allow(clippy::borrow_deref_ref)]
#![allow(clippy::useless_conversion)]
#![allow(clippy::clone_on_copy)]
#![allow(clippy::map_flatten)]
#![allow(unused_mut)]
#![allow(unused_variables)]
#![allow(unused_imports)]

use kvnc_types::{hash::Hash, Address};
use redb::{Database, ReadTransaction, ReadableTable, TableDefinition, WriteTransaction};
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;
use tracing::warn;

pub mod block_store;
pub mod consensus_store;
pub mod state_store;

pub use block_store::{BlockStore, BlockStoreError};
pub use consensus_store::{ConsensusStore, ConsensusStoreError};
pub use state_store::{StateStore, StateStoreError};

/// Main storage handle combining all stores.
pub struct Storage {
    db: Database,
    block_store: BlockStore,
    state_store: StateStore,
    consensus_store: ConsensusStore,
}

impl Storage {
    /// Open or create a new storage at the given path.
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, StorageError> {
        let db = Database::create(path).map_err(StorageError::Database)?;
        let block_store = BlockStore::new(&db)?;
        let state_store = StateStore::new(&db)?;
        let consensus_store = ConsensusStore::new(&db)?;

        Ok(Self {
            db,
            block_store,
            state_store,
            consensus_store,
        })
    }

    /// Get a reference to the block store.
    pub fn blocks(&self) -> &BlockStore {
        &self.block_store
    }

    /// Get a mutable reference to the block store.
    pub fn blocks_mut(&mut self) -> &mut BlockStore {
        &mut self.block_store
    }

    /// Get a reference to the state store.
    pub fn state(&self) -> &StateStore {
        &self.state_store
    }

    /// Get a mutable reference to the state store.
    pub fn state_mut(&mut self) -> &mut StateStore {
        &mut self.state_store
    }

    /// Get a reference to the consensus store.
    pub fn consensus(&self) -> &ConsensusStore {
        &self.consensus_store
    }

    /// Get a mutable reference to the consensus store.
    pub fn consensus_mut(&mut self) -> &mut ConsensusStore {
        &mut self.consensus_store
    }

    /// Begin a read transaction.
    pub fn begin_read(&self) -> Result<ReadTransaction, StorageError> {
        self.db.begin_read().map_err(StorageError::Transaction)
    }

    /// Begin a write transaction.
    pub fn begin_write(&self) -> Result<WriteTransaction, StorageError> {
        self.db.begin_write().map_err(StorageError::Transaction)
    }
}

/// Errors that can occur in storage operations.
#[derive(Error, Debug)]
pub enum StorageError {
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
    #[error("Block store error: {0}")]
    BlockStore(#[from] BlockStoreError),
    #[error("State store error: {0}")]
    StateStore(#[from] StateStoreError),
    #[error("Consensus store error: {0}")]
    ConsensusStore(#[from] ConsensusStoreError),
    #[error("Serialization error: {0}")]
    Serialization(#[from] bincode::Error),
    #[error("Not found: {0}")]
    NotFound(String),
}

/// Table definitions for redb.
mod tables {
    use super::*;
    use kvnc_types::block::StatementBlock;
    use kvnc_types::hash::Hash;
    use kvnc_types::Address;

    // Block store tables
    pub const BLOCKS: TableDefinition<[u8; 32], Vec<u8>> = TableDefinition::new("blocks");
    pub const BLOCK_HEIGHT: TableDefinition<u64, [u8; 32]> = TableDefinition::new("block_height");
    pub const BLOCK_TRANSACTIONS: TableDefinition<[u8; 32], Vec<u8>> =
        TableDefinition::new("block_transactions");
    pub const TRANSACTION_INDEX: TableDefinition<[u8; 32], [u8; 32]> =
        TableDefinition::new("tx_index"); // tx_hash -> block_hash

    // State store tables
    pub const ACCOUNTS: TableDefinition<[u8; 32], Vec<u8>> = TableDefinition::new("accounts");
    pub const CONTRACT_CODE: TableDefinition<[u8; 32], Vec<u8>> =
        TableDefinition::new("contract_code");
    pub const CONTRACT_STORAGE: TableDefinition<([u8; 32], [u8; 32]), Vec<u8>> =
        TableDefinition::new("contract_storage");
    pub const STAKING_STATE: TableDefinition<&'static str, Vec<u8>> =
        TableDefinition::new("staking_state");
    pub const STATE_ROOT: TableDefinition<u64, [u8; 32]> = TableDefinition::new("state_root"); // committed_leader_height -> state_root

    // Consensus store tables
    pub const DAG_BLOCKS: TableDefinition<[u8; 32], Vec<u8>> = TableDefinition::new("dag_blocks"); // StatementBlock serialized
    pub const DAG_PARENTS: TableDefinition<[u8; 32], Vec<u8>> = TableDefinition::new("dag_parents"); // block_hash -> parent hashes
    pub const DAG_CHILDREN: TableDefinition<[u8; 32], Vec<u8>> =
        TableDefinition::new("dag_children"); // block_hash -> child hashes
    pub const DAG_BY_ROUND: TableDefinition<(u64, u16), Vec<u8>> =
        TableDefinition::new("dag_by_round"); // (round, author) -> block_hashes
    pub const DAG_BY_AUTHOR: TableDefinition<(u16, u64), Vec<u8>> =
        TableDefinition::new("dag_by_author"); // (author, round) -> block_hash
    pub const COMMITTED_LEADER_HEIGHT: TableDefinition<&'static str, u64> =
        TableDefinition::new("committed_leader_height");
    pub const DECIDED_ROUNDS: TableDefinition<u64, Vec<u8>> =
        TableDefinition::new("decided_rounds"); // round -> decided leader hashes
    pub const LAST_COMMITED: TableDefinition<&'static str, [u8; 32]> =
        TableDefinition::new("last_committed"); // "last" -> leader block hash
}

// Helper trait for bincode serialization
pub trait BincodeSerialize: Serialize + for<'de> Deserialize<'de> {
    fn to_bytes(&self) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(self)
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, bincode::Error>
    where
        Self: Sized,
    {
        bincode::deserialize(bytes)
    }
}

impl<T: Serialize + for<'de> Deserialize<'de>> BincodeSerialize for T {}

/// Serialize a Hash to bytes for table keys.
fn hash_to_bytes(hash: &Hash) -> [u8; 32] {
    hash.0
}

/// Serialize an Address to bytes for table keys.
fn address_to_bytes(addr: &Address) -> [u8; 32] {
    addr.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_storage_creation() {
        let dir = tempdir().unwrap();
        let storage = Storage::new(dir.path().join("test.db")).unwrap();
        let _txn = storage.begin_write().unwrap();
        // commit happens on transaction drop
    }
}
