//! Block storage for KVNC.
//!
//! Stores blocks, transactions, and provides indexing by height and hash.

use crate::{hash_to_bytes, BincodeSerialize, StorageError};
use kvnc_types::{
    block::{BlockReference, StatementBlock},
    hash::Hash,
    transaction::Transaction,
    Round,
};
use redb::{WriteTransaction, ReadTransaction, ReadableTable};
use thiserror::Error;

/// Errors specific to block store operations.
#[derive(Debug, Error)]
pub enum BlockStoreError {
    #[error("Block not found: {0}")]
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

/// Block store for persisting blocks and transactions.
pub struct BlockStore;

impl BlockStore {
    /// Create a new block store, initializing tables if needed.
    pub fn new(db: &redb::Database) -> Result<Self, StorageError> {
        let write_txn = db.begin_write().map_err(StorageError::Transaction)?;
        {
            let _ = write_txn.open_table(crate::tables::BLOCKS)?;
            let _ = write_txn.open_table(crate::tables::BLOCK_HEIGHT)?;
            let _ = write_txn.open_table(crate::tables::BLOCK_TRANSACTIONS)?;
            let _ = write_txn.open_table(crate::tables::TRANSACTION_INDEX)?;
        }
        write_txn.commit().map_err(StorageError::Commit)?;
        Ok(Self)
    }

    /// Store a block and its transactions.
    pub fn put_block(
        &self,
        txn: &WriteTransaction,
        block: &StatementBlock,
        transactions: &[Transaction],
    ) -> Result<(), BlockStoreError> {
        let block_hash = hash_to_bytes(&block.digest);
        let round = block.round;
        let author = block.author;

        // Serialize and store the block
        let block_bytes = block.to_bytes()?;
        {
            let mut table = txn.open_table(crate::tables::BLOCKS)?;
            table.insert(block_hash, block_bytes)?;
        }

        // Store block by height (round)
        {
            let mut table = txn.open_table(crate::tables::BLOCK_HEIGHT)?;
            table.insert(&round, &block_hash)?;
        }

        // Store transactions
        let tx_hashes: Vec<[u8; 32]> = transactions
            .iter()
            .map(|tx| hash_to_bytes(&tx.hash))
            .collect();
        let tx_bytes = tx_hashes.to_bytes()?;
        {
            let mut table = txn.open_table(crate::tables::BLOCK_TRANSACTIONS)?;
            table.insert(block_hash, tx_bytes)?;
        }

        // Index each transaction to this block
        {
            let mut table = txn.open_table(crate::tables::TRANSACTION_INDEX)?;
            for tx in transactions {
                let tx_hash = hash_to_bytes(&tx.hash);
                table.insert(tx_hash, block_hash)?;
            }
        }

        Ok(())
    }

    /// Get a block by its hash.
    pub fn get_block(&self, txn: &ReadTransaction, hash: &Hash) -> Result<StatementBlock, BlockStoreError> {
        let block_hash = hash_to_bytes(hash);
        let table = txn.open_table(crate::tables::BLOCKS)?;
        let value = table.get(block_hash)?
            .ok_or_else(|| BlockStoreError::NotFound(format!("block {}", hex::encode(block_hash))))?;
        Ok(StatementBlock::from_bytes(&value.value())?)
    }

    /// Get a block by round (height).
    pub fn get_block_by_height(&self, txn: &ReadTransaction, round: Round) -> Result<Option<StatementBlock>, BlockStoreError> {
        let table = txn.open_table(crate::tables::BLOCK_HEIGHT)?;
        let block_hash = match table.get(&round)? {
            Some(v) => v.value(),
            None => return Ok(None),
        };
        drop(table);
        self.get_block(txn, &Hash(block_hash)).map(Some)
    }

    /// Get blocks in a range of rounds.
    pub fn get_blocks_by_range(
        &self,
        txn: &ReadTransaction,
        start: Round,
        end: Round,
    ) -> Result<Vec<StatementBlock>, BlockStoreError> {
        let table = txn.open_table(crate::tables::BLOCK_HEIGHT)?;
        let mut blocks = Vec::new();
        for entry in table.range(start..=end)? {
            let (_, block_hash) = entry?;
            let block = self.get_block(txn, &Hash(block_hash.value()))?;
            blocks.push(block);
        }
        Ok(blocks)
    }

    /// Get all transaction hashes for a block.
    pub fn get_block_transactions(
        &self,
        txn: &ReadTransaction,
        block_hash: &Hash,
    ) -> Result<Vec<Hash>, BlockStoreError> {
        let key = hash_to_bytes(block_hash);
        let table = txn.open_table(crate::tables::BLOCK_TRANSACTIONS)?;
        let value = table.get(key)?
            .ok_or_else(|| BlockStoreError::NotFound(format!("transactions for block {}", hex::encode(key))))?;
        let tx_hashes: Vec<[u8; 32]> = BincodeSerialize::from_bytes(&value.value())?;
        Ok(tx_hashes.into_iter().map(Hash).collect())
    }

    /// Get the block hash that contains a transaction.
    pub fn get_block_for_transaction(
        &self,
        txn: &ReadTransaction,
        tx_hash: &Hash,
    ) -> Result<Option<Hash>, BlockStoreError> {
        let key = hash_to_bytes(tx_hash);
        let table = txn.open_table(crate::tables::TRANSACTION_INDEX)?;
        Ok(table.get(key)?.map(|v| Hash(v.value())))
    }

    /// Get the latest block (highest round).
    pub fn get_latest_block(&self, txn: &ReadTransaction) -> Result<Option<StatementBlock>, BlockStoreError> {
        let table = txn.open_table(crate::tables::BLOCK_HEIGHT)?;
        let mut latest_round = 0;
        let mut latest_hash = None;

        for entry in table.iter()? {
            let (round, hash) = entry?;
            if round.value() > latest_round {
                latest_round = round.value();
                latest_hash = Some(hash.value());
            }
        }

        match latest_hash {
            Some(h) => self.get_block(txn, &Hash(h)).map(Some),
            None => Ok(None),
        }
    }

    /// Check if a block exists.
    pub fn has_block(&self, txn: &ReadTransaction, hash: &Hash) -> Result<bool, BlockStoreError> {
        let key = hash_to_bytes(hash);
        let table = txn.open_table(crate::tables::BLOCKS)?;
        Ok(table.get(key)?.is_some())
    }

    /// Delete blocks below a certain round (pruning).
    pub fn prune_below(&self, txn: &WriteTransaction, min_round: Round) -> Result<u64, BlockStoreError> {
        let table = txn.open_table(crate::tables::BLOCK_HEIGHT)?;
        let mut pruned = 0;

        // Collect rounds to prune first (can't modify while iterating)
        let mut rounds_to_prune = Vec::new();
        for entry in table.range(0..min_round)? {
            let (round, block_hash) = entry?;
            rounds_to_prune.push((round.value(), block_hash.value()));
        }

        // Now delete
        for (round, block_hash) in rounds_to_prune {
            // Remove from blocks table
            {
                let mut blocks_table = txn.open_table(crate::tables::BLOCKS)?;
                blocks_table.remove(block_hash)?;
            }
            // Remove from block_height
            {
                let mut height_table = txn.open_table(crate::tables::BLOCK_HEIGHT)?;
                height_table.remove(&round)?;
            }
            // Remove from block_transactions and transaction_index
            {
                let mut tx_table = txn.open_table(crate::tables::BLOCK_TRANSACTIONS)?;
                if let Some(tx_hashes_bytes) = tx_table.get(block_hash)? {
                    let tx_hashes: Vec<[u8; 32]> = BincodeSerialize::from_bytes(&tx_hashes_bytes.value())?;
                    let mut tx_index = txn.open_table(crate::tables::TRANSACTION_INDEX)?;
                    for tx_hash in tx_hashes {
                        tx_index.remove(tx_hash)?;
                    }
                }
                tx_table.remove(block_hash)?;
            }
            pruned += 1;
        }

        Ok(pruned)
    }
}