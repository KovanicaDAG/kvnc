//! Consensus storage for KVNC.
//!
//! Stores DAG structure (blocks, parent/child links), commit tracker, and decided rounds.

use crate::{hash_to_bytes, BincodeSerialize, StorageError};
use kvnc_types::{
    block::{BlockReference, StatementBlock},
    hash::Hash,
    AuthorityIndex, Round,
    CommittedSubDag,
};
use redb::{WriteTransaction, ReadTransaction, ReadableTable};
use thiserror::Error;

/// Errors specific to consensus store operations.
#[derive(Debug, Error)]
pub enum ConsensusStoreError {
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

/// Consensus store for DAG structure and commit tracking.
pub struct ConsensusStore;

impl ConsensusStore {
    /// Create a new consensus store, initializing tables if needed.
    pub fn new(db: &redb::Database) -> Result<Self, StorageError> {
        let write_txn = db.begin_write()?;
        {
            let _ = write_txn.open_table(crate::tables::DAG_BLOCKS)?;
            let _ = write_txn.open_table(crate::tables::DAG_PARENTS)?;
            let _ = write_txn.open_table(crate::tables::DAG_CHILDREN)?;
            let _ = write_txn.open_table(crate::tables::DAG_BY_ROUND)?;
            let _ = write_txn.open_table(crate::tables::DAG_BY_AUTHOR)?;
            let _ = write_txn.open_table(crate::tables::COMMITTED_LEADER_HEIGHT)?;
            let _ = write_txn.open_table(crate::tables::DECIDED_ROUNDS)?;
            let _ = write_txn.open_table(crate::tables::LAST_COMMITED)?;
        }
        write_txn.commit()?;
        Ok(Self)
    }

    // ============================================================
    // DAG Block operations
    // ============================================================

    /// Store a DAG block with its parent/child links.
    pub fn put_dag_block(
        &self,
        txn: &WriteTransaction,
        block: &StatementBlock,
    ) -> Result<(), ConsensusStoreError> {
        let block_hash = hash_to_bytes(&block.digest);
        let round = block.round;
        let author = block.author;

        // Store the block
        {
            let mut table = txn.open_table(crate::tables::DAG_BLOCKS)?;
            table.insert(block_hash, block.to_bytes()?)?;
        }

        // Store parent links
        let parent_hashes: Vec<[u8; 32]> = block
            .parents
            .iter()
            .map(|p| hash_to_bytes(&p.digest))
            .collect();
        {
            let mut table = txn.open_table(crate::tables::DAG_PARENTS)?;
            table.insert(block_hash, parent_hashes.to_bytes()?)?;
        }

        // Update child links for each parent
        for parent_hash in &parent_hashes {
            let mut children = self.get_children_internal(&*txn, parent_hash)?;
            children.push(block_hash);
            let mut table = txn.open_table(crate::tables::DAG_CHILDREN)?;
            table.insert(*parent_hash, children.to_bytes()?)?;
        }

        // Index by round
        {
            let mut table = txn.open_table(crate::tables::DAG_BY_ROUND)?;
            let mut existing: Vec<[u8; 32]> = table
                .get((round, author))?
                .map(|v| BincodeSerialize::from_bytes(&v.value()).unwrap_or_default())
                .unwrap_or_default();
            existing.push(block_hash);
            table.insert((round, author), existing.to_bytes()?)?;
        }

        // Index by author
        {
            let mut table = txn.open_table(crate::tables::DAG_BY_AUTHOR)?;
            let mut existing: Vec<[u8; 32]> = table
                .get((author, round))?
                .map(|v| BincodeSerialize::from_bytes(&v.value()).unwrap_or_default())
                .unwrap_or_default();
            existing.push(block_hash);
            table.insert((author, round), existing.to_bytes()?)?;
        }

        Ok(())
    }

    /// Get a DAG block by hash.
    pub fn get_dag_block(
        &self,
        txn: &ReadTransaction,
        hash: &Hash,
    ) -> Result<StatementBlock, ConsensusStoreError> {
        let key = hash_to_bytes(hash);
        let table = txn.open_table(crate::tables::DAG_BLOCKS)?;
        let value = table.get(key)?
            .ok_or_else(|| ConsensusStoreError::NotFound(format!("dag block {}", hex::encode(key))))?;
        Ok(StatementBlock::from_bytes(&value.value())?)
    }

    /// Get parent hashes for a block.
    pub fn get_parents(
        &self,
        txn: &ReadTransaction,
        hash: &Hash,
    ) -> Result<Vec<Hash>, ConsensusStoreError> {
        let key = hash_to_bytes(hash);
        let table = txn.open_table(crate::tables::DAG_PARENTS)?;
        let value = table.get(key)?
            .ok_or_else(|| ConsensusStoreError::NotFound(format!("parents for {}", hex::encode(key))))?;
        let parent_hashes: Vec<[u8; 32]> = BincodeSerialize::from_bytes(&value.value())?;
        Ok(parent_hashes.into_iter().map(Hash).collect())
    }

    /// Get child hashes for a block (internal helper).
    fn get_children_internal(
        &self,
        txn: &WriteTransaction,
        hash: &[u8; 32],
    ) -> Result<Vec<[u8; 32]>, ConsensusStoreError> {
        let table = txn.open_table(crate::tables::DAG_CHILDREN)?;
        let value = table.get(*hash)?;
        Ok(value
            .map(|v| BincodeSerialize::from_bytes(&v.value()).unwrap_or_default())
            .unwrap_or_default())
    }

    /// Get child hashes for a block.
    pub fn get_children(
        &self,
        txn: &WriteTransaction,
        hash: &Hash,
    ) -> Result<Vec<Hash>, ConsensusStoreError> {
        let key = hash_to_bytes(hash);
        Ok(self.get_children_internal(txn, &key)?.into_iter().map(Hash).collect())
    }

    /// Get all blocks for a given round.
    pub fn get_blocks_by_round(
        &self,
        txn: &ReadTransaction,
        round: Round,
    ) -> Result<Vec<StatementBlock>, ConsensusStoreError> {
        let table = txn.open_table(crate::tables::DAG_BY_ROUND)?;
        let mut blocks = Vec::new();

        for entry in table.range((round, 0u16)..=(round, u16::MAX))? {
            let (_, block_hashes) = entry?;
            let hashes: Vec<[u8; 32]> = BincodeSerialize::from_bytes(&block_hashes.value())?;
            for hash in hashes {
                blocks.push(self.get_dag_block(txn, &Hash(hash))?);
            }
        }
        Ok(blocks)
    }

    /// Get block for a specific author and round.
    pub fn get_block_by_author_round(
        &self,
        txn: &ReadTransaction,
        author: AuthorityIndex,
        round: Round,
    ) -> Result<Option<StatementBlock>, ConsensusStoreError> {
        let table = txn.open_table(crate::tables::DAG_BY_AUTHOR)?;
        let hashes: Option<Vec<[u8; 32]>> = table
            .get((author, round))?
            .map(|v| BincodeSerialize::from_bytes(&v.value()).ok())
            .flatten();

        match hashes {
            Some(hashes) if !hashes.is_empty() => {
                // Return the first (should only be one per author per round)
                Ok(Some(self.get_dag_block(txn, &Hash(hashes[0]))?))
            }
            _ => Ok(None),
        }
    }

    /// Check if a DAG block exists.
    pub fn has_dag_block(&self, txn: &ReadTransaction, hash: &Hash) -> Result<bool, ConsensusStoreError> {
        let key = hash_to_bytes(hash);
        let table = txn.open_table(crate::tables::DAG_BLOCKS)?;
        Ok(table.get(key)?.is_some())
    }

    // ============================================================
    // Commit tracker operations
    // ============================================================

    /// Get the current committed leader height.
    pub fn get_committed_leader_height(
        &self,
        txn: &ReadTransaction,
    ) -> Result<u64, ConsensusStoreError> {
        let table = txn.open_table(crate::tables::COMMITTED_LEADER_HEIGHT)?;
        Ok(table.get("height")?.map(|v| v.value()).unwrap_or(0))
    }

    /// Set the committed leader height.
    pub fn set_committed_leader_height(
        &self,
        txn: &WriteTransaction,
        height: u64,
    ) -> Result<(), ConsensusStoreError> {
        let mut table = txn.open_table(crate::tables::COMMITTED_LEADER_HEIGHT)?;
        table.insert("height", &height)?;
        Ok(())
    }

    /// Increment the committed leader height.
    pub fn increment_committed_leader_height(
        &self,
        txn: &WriteTransaction,
    ) -> Result<u64, ConsensusStoreError> {
        let table = txn.open_table(crate::tables::COMMITTED_LEADER_HEIGHT)?;
        let height = table.get("height")?.map(|v| v.value()).unwrap_or(0) + 1;
        drop(table);
        let mut table = txn.open_table(crate::tables::COMMITTED_LEADER_HEIGHT)?;
        table.insert("height", &height)?;
        Ok(height)
    }

    /// Get the last committed leader block hash.
    pub fn get_last_committed(
        &self,
        txn: &ReadTransaction,
    ) -> Result<Option<Hash>, ConsensusStoreError> {
        let table = txn.open_table(crate::tables::LAST_COMMITED)?;
        Ok(table.get("last")?.map(|v| Hash(v.value())))
    }

    /// Set the last committed leader block hash.
    pub fn set_last_committed(
        &self,
        txn: &WriteTransaction,
        hash: &Hash,
    ) -> Result<(), ConsensusStoreError> {
        let mut table = txn.open_table(crate::tables::LAST_COMMITED)?;
        table.insert("last", hash_to_bytes(hash))?;
        Ok(())
    }

    // ============================================================
    // Decided rounds operations
    // ============================================================

    /// Mark a round as decided with a leader hash.
    pub fn mark_round_decided(
        &self,
        txn: &WriteTransaction,
        round: Round,
        leader_hash: &Hash,
    ) -> Result<(), ConsensusStoreError> {
        let mut table = txn.open_table(crate::tables::DECIDED_ROUNDS)?;
        let mut existing: Vec<[u8; 32]> = table
            .get(round)?
            .map(|v| BincodeSerialize::from_bytes(&v.value()).unwrap_or_default())
            .unwrap_or_default();
        existing.push(hash_to_bytes(leader_hash));
        table.insert(round, existing.to_bytes()?)?;
        Ok(())
    }

    /// Check if a round is decided.
    pub fn is_round_decided(
        &self,
        txn: &ReadTransaction,
        round: Round,
    ) -> Result<bool, ConsensusStoreError> {
        let table = txn.open_table(crate::tables::DECIDED_ROUNDS)?;
        Ok(table.get(round)?.is_some())
    }

    /// Get decided leader hashes for a round.
    pub fn get_decided_leaders(
        &self,
        txn: &ReadTransaction,
        round: Round,
    ) -> Result<Vec<Hash>, ConsensusStoreError> {
        let table = txn.open_table(crate::tables::DECIDED_ROUNDS)?;
        Ok(table
            .get(round)?
            .map(|v| {
                let hashes: Vec<[u8; 32]> = BincodeSerialize::from_bytes(&v.value()).unwrap_or_default();
                hashes.into_iter().map(Hash).collect()
            })
            .unwrap_or_default())
    }

    /// Get all decided rounds up to a maximum.
    pub fn get_decided_rounds(
        &self,
        txn: &ReadTransaction,
        max_round: Round,
    ) -> Result<Vec<Round>, ConsensusStoreError> {
        let table = txn.open_table(crate::tables::DECIDED_ROUNDS)?;
        let mut rounds = Vec::new();
        for entry in table.range(0..=max_round)? {
            let (round, _) = entry?;
            rounds.push(round.value());
        }
        Ok(rounds)
    }

    // ============================================================
    // Pruning
    // ============================================================

    /// Prune DAG blocks below a certain round (keep only recent history).
    pub fn prune_dag_below(&self, txn: &WriteTransaction, min_round: Round) -> Result<u64, ConsensusStoreError> {
        let table = txn.open_table(crate::tables::DAG_BY_ROUND)?;
        let mut pruned = 0;

        // Collect blocks to prune
        let mut blocks_to_prune = Vec::new();
        for entry in table.range((0u64, 0u16)..=(min_round, u16::MAX))? {
            let (_, block_hashes) = entry?;
            let hashes: Vec<[u8; 32]> = BincodeSerialize::from_bytes(&block_hashes.value())?;
            for hash in hashes {
                blocks_to_prune.push(hash);
            }
        }

        // Delete blocks and their links
        for block_hash in blocks_to_prune {
            // Remove from dag_blocks
            {
                let mut table = txn.open_table(crate::tables::DAG_BLOCKS)?;
                table.remove(block_hash)?;
            }
            // Remove from dag_parents
            {
                let mut table = txn.open_table(crate::tables::DAG_PARENTS)?;
                table.remove(block_hash)?;
            }
            // Remove from dag_children (need to update parents' children lists)
            {
                let parents = self.get_parents_internal(&*txn, &block_hash)?;
                for parent_hash in parents {
                    let mut children = self.get_children_internal(&*txn, &parent_hash)?;
                    children.retain(|h| *h != block_hash);
                    let mut child_table = txn.open_table(crate::tables::DAG_CHILDREN)?;
                    child_table.insert(parent_hash, children.to_bytes()?)?;
                }
            }
            // Remove from dag_children
            {
                let mut table = txn.open_table(crate::tables::DAG_CHILDREN)?;
                table.remove(block_hash)?;
            }
            pruned += 1;
        }

        // Clean up round index
        {
            let mut round_table = txn.open_table(crate::tables::DAG_BY_ROUND)?;
            let mut keys_to_remove = Vec::new();
            for entry in round_table.range((0u64, 0u16)..=(min_round, u16::MAX))? {
                let (key, _) = entry?;
                keys_to_remove.push(key.value());
            }
            for key in keys_to_remove {
                round_table.remove(key)?;
            }
        }

        // Clean up author index
        {
            let mut author_table = txn.open_table(crate::tables::DAG_BY_AUTHOR)?;
            // We need to scan all entries and remove those with round < min_round
            // For efficiency, we'd need a separate index by round, but for now we skip this
            // In production, we'd maintain a round->author index
        }

        Ok(pruned)
    }

    /// Internal helper to get parents without Hash wrapper.
    fn get_parents_internal(
        &self,
        txn: &WriteTransaction,
        hash: &[u8; 32],
    ) -> Result<Vec<[u8; 32]>, ConsensusStoreError> {
        let table = txn.open_table(crate::tables::DAG_PARENTS)?;
        let value = table.get(*hash)?;
        Ok(value
            .map(|v| BincodeSerialize::from_bytes(&v.value()).unwrap_or_default())
            .unwrap_or_default())
    }
}