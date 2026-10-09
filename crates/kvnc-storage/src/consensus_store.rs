//! Consensus storage for KVNC.
//!
//! Stores DAG structure (blocks, parent/child links), commit tracker, and decided rounds.

use crate::{hash_to_bytes, BincodeSerialize, StorageError};
use kvnc_types::{
    block::{BlockReference, StatementBlock},
    hash::Hash,
    AuthorityIndex, CommittedSubDag, Round,
};
use redb::{ReadTransaction, ReadableTable, WriteTransaction};
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
        let value = table.get(key)?.ok_or_else(|| {
            ConsensusStoreError::NotFound(format!("dag block {}", hex::encode(key)))
        })?;
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
        let value = table.get(key)?.ok_or_else(|| {
            ConsensusStoreError::NotFound(format!("parents for {}", hex::encode(key)))
        })?;
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
        Ok(self
            .get_children_internal(txn, &key)?
            .into_iter()
            .map(Hash)
            .collect())
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
                // Tolerate dangling index entries: a block may have been
                // pruned without its index key fully cleaned up.
                match self.get_dag_block(txn, &Hash(hash)) {
                    Ok(block) => blocks.push(block),
                    Err(ConsensusStoreError::NotFound(_)) => continue,
                    Err(e) => return Err(e),
                }
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
    pub fn has_dag_block(
        &self,
        txn: &ReadTransaction,
        hash: &Hash,
    ) -> Result<bool, ConsensusStoreError> {
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
                let hashes: Vec<[u8; 32]> =
                    BincodeSerialize::from_bytes(&v.value()).unwrap_or_default();
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
    pub fn prune_dag_below(
        &self,
        txn: &WriteTransaction,
        min_round: Round,
    ) -> Result<u64, ConsensusStoreError> {
        let mut pruned = 0;

        // Collect blocks to prune and round/author index keys to remove
        let mut blocks_to_prune = Vec::new();
        let mut round_keys_to_remove = Vec::new();
        let mut author_keys_to_remove = Vec::new();

        {
            let table = txn.open_table(crate::tables::DAG_BY_ROUND)?;
            for entry in table.range((0u64, 0u16)..=(min_round, u16::MAX))? {
                let (key, block_hashes) = entry?;
                let hashes: Vec<[u8; 32]> = BincodeSerialize::from_bytes(&block_hashes.value())?;
                for hash in hashes {
                    blocks_to_prune.push(hash);
                }
                let (round, author) = key.value();
                round_keys_to_remove.push((round, author));
                author_keys_to_remove.push((author, round));
            }
        } // table is dropped here

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
            // Update the deleted block's parents' children lists, and drop the
            // deleted block from each surviving child's parent list so no
            // dangling parent edge survives pruning (M2).
            {
                let parents = self.get_parents_internal(&*txn, &block_hash)?;
                for parent_hash in parents {
                    let mut children = self.get_children_internal(&*txn, &parent_hash)?;
                    children.retain(|h| *h != block_hash);
                    let mut child_table = txn.open_table(crate::tables::DAG_CHILDREN)?;
                    child_table.insert(parent_hash, children.to_bytes()?)?;
                }

                let children = self.get_children_internal(&*txn, &block_hash)?;
                for child_hash in children {
                    if child_hash == block_hash {
                        continue;
                    }
                    let mut table = txn.open_table(crate::tables::DAG_PARENTS)?;
                    let existing: Vec<[u8; 32]> = table
                        .get(child_hash)?
                        .map(|v| BincodeSerialize::from_bytes(&v.value()).unwrap_or_default())
                        .unwrap_or_default();
                    let filtered: Vec<[u8; 32]> =
                        existing.into_iter().filter(|h| *h != block_hash).collect();
                    if filtered.is_empty() {
                        table.remove(child_hash)?;
                    } else {
                        table.insert(child_hash, filtered.to_bytes()?)?;
                    }
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
            for key in round_keys_to_remove {
                round_table.remove(key)?;
            }
        }

        // Clean up author index alongside the round index so the author index
        // does not grow without bound and `get_block_by_author_round` cannot
        // resolve a pruned block (M2 m2).
        {
            let mut author_table = txn.open_table(crate::tables::DAG_BY_AUTHOR)?;
            for key in author_keys_to_remove {
                author_table.remove(key)?;
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{tables, Storage};
    use kvnc_types::crypto::Signature;
    use std::collections::HashSet;
    use tempfile::tempdir;

    fn make_block(author: AuthorityIndex, round: Round, tag: &str) -> StatementBlock {
        StatementBlock {
            author,
            round,
            parents: Vec::new(),
            transactions: Vec::new(),
            statements: tag.as_bytes().to_vec(),
            signature: Signature([0u8; 64]),
            digest: Hash::new(format!("kvnc-consensus-store-test/{tag}").as_bytes()),
            merkle_root: Hash::zero(),
        }
    }

    // #5a: `get_blocks_by_round` must skip index entries whose block no longer
    // exists (e.g. a hash left behind after pruning).
    #[test]
    fn get_blocks_by_round_skips_dangling_index_entries() {
        let dir = tempdir().unwrap();
        let storage = Storage::new(dir.path().join("consensus.redb")).unwrap();
        let store = storage.consensus();

        let block_a = make_block(0, 3, "a");
        let block_b = make_block(1, 3, "b");
        let stale = Hash::new(b"kvnc-consensus-store-test/stale");

        {
            let txn = storage.begin_write().unwrap();
            store.put_dag_block(&txn, &block_a).unwrap();
            store.put_dag_block(&txn, &block_b).unwrap();

            // Inject a hash that has no corresponding block into the
            // (round=3, author=0) index entry.
            {
                let mut table = txn.open_table(tables::DAG_BY_ROUND).unwrap();
                let mut existing: Vec<[u8; 32]> = table
                    .get((3u64, 0u16))
                    .unwrap()
                    .map(|v| BincodeSerialize::from_bytes(&v.value()).unwrap_or_default())
                    .unwrap_or_default();
                existing.push(hash_to_bytes(&stale));
                table
                    .insert((3u64, 0u16), existing.to_bytes().unwrap())
                    .unwrap();
            }
            txn.commit().unwrap();
        }

        let txn = storage.begin_read().unwrap();
        let round_blocks = store.get_blocks_by_round(&txn, 3).unwrap();
        assert_eq!(round_blocks.len(), 2, "stale index entry must be skipped");
        let digests: HashSet<Hash> = round_blocks.iter().map(|block| block.digest).collect();
        assert!(digests.contains(&block_a.digest));
        assert!(digests.contains(&block_b.digest));
        assert!(!digests.contains(&stale), "missing block must not surface");
    }
}
