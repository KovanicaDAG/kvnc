//! DAG store for KVNC.
//!
#![allow(missing_docs)]
//! Provides high-level operations on the DAG structure using the storage layer.

use kvnc_storage::{hash_to_bytes, tables, BincodeSerialize, ConsensusStoreError, Storage};
use kvnc_types::{
    block::{BlockReference, StatementBlock},
    hash::Hash,
    AuthorityIndex, Round,
};
use redb::{CommitError, ReadTransaction, ReadableTable, TableError, WriteTransaction};
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use thiserror::Error;

/// Errors specific to DAG store operations.
#[derive(Error, Debug)]
pub enum DagStoreError {
    #[error("Block not found: {0}")]
    NotFound(String),
    #[error("Storage error: {0}")]
    Storage(#[from] kvnc_storage::StorageError),
    #[error("Consensus store error: {0}")]
    ConsensusStore(#[from] kvnc_storage::ConsensusStoreError),
    #[error("Block store error: {0}")]
    BlockStore(#[from] kvnc_storage::BlockStoreError),
    #[error("State store error: {0}")]
    StateStore(#[from] kvnc_storage::StateStoreError),
    #[error("Commit error: {0}")]
    Commit(#[from] CommitError),
    #[error("Table error: {0}")]
    Table(#[from] TableError),
    #[error("Redb storage error: {0}")]
    RedbStorage(#[from] redb::StorageError),
    #[error("Serialization error: {0}")]
    Serialization(#[from] bincode::Error),
}

/// DAG store for high-level DAG operations.
#[derive(Clone)]
pub struct DagStore {
    storage: Arc<Storage>,
}

impl DagStore {
    /// Create a new DAG store.
    pub fn new(storage: Storage) -> Result<Self, DagStoreError> {
        Ok(Self {
            storage: Arc::new(storage),
        })
    }

    /// Get a block by hash.
    pub fn get_block(&self, hash: &Hash) -> Result<StatementBlock, DagStoreError> {
        let txn = self.storage.begin_read()?;
        Ok(self.storage.consensus().get_dag_block(&txn, hash)?)
    }

    /// Get multiple blocks by their hashes.
    /// Returns only the blocks that are found (missing blocks are silently skipped).
    pub fn get_blocks(&self, hashes: &[Hash]) -> Result<Vec<StatementBlock>, DagStoreError> {
        let txn = self.storage.begin_read()?;
        let mut blocks = Vec::with_capacity(hashes.len());
        for hash in hashes {
            if let Ok(block) = self.storage.consensus().get_dag_block(&txn, hash) {
                blocks.push(block);
            }
        }
        Ok(blocks)
    }

    /// Check if a block exists.
    pub fn has_block(&self, hash: &Hash) -> Result<bool, DagStoreError> {
        let txn = self.storage.begin_read()?;
        Ok(self.storage.consensus().has_dag_block(&txn, hash)?)
    }

    /// Get parent hashes for a block.
    pub fn get_parents(&self, hash: &Hash) -> Result<Vec<Hash>, DagStoreError> {
        let txn = self.storage.begin_read()?;
        Ok(self.storage.consensus().get_parents(&txn, hash)?)
    }

    /// Get child hashes for a block.
    pub fn get_children(&self, hash: &Hash) -> Result<Vec<Hash>, DagStoreError> {
        let txn = self.storage.begin_read()?;
        // get_children requires a write transaction in the current implementation
        // For read-only access, we need to use a different approach
        // For now, we'll use a write transaction but only read
        let txn = self.storage.begin_write()?;
        let children = self.storage.consensus().get_children(&txn, hash)?;
        txn.commit()?;
        Ok(children)
    }

    /// Get all blocks for a given round.
    pub fn get_blocks_by_round(&self, round: Round) -> Result<Vec<StatementBlock>, DagStoreError> {
        let txn = self.storage.begin_read()?;
        Ok(self.storage.consensus().get_blocks_by_round(&txn, round)?)
    }

    /// Get block for a specific author and round.
    pub fn get_block_by_author_round(
        &self,
        author: AuthorityIndex,
        round: Round,
    ) -> Result<Option<StatementBlock>, DagStoreError> {
        let txn = self.storage.begin_read()?;
        Ok(self
            .storage
            .consensus()
            .get_block_by_author_round(&txn, author, round)?)
    }

    /// Get the committed leader height.
    pub fn get_committed_leader_height(&self) -> Result<u64, DagStoreError> {
        let txn = self.storage.begin_read()?;
        Ok(self.storage.consensus().get_committed_leader_height(&txn)?)
    }

    /// Get the last committed leader block hash.
    pub fn get_last_committed(&self) -> Result<Option<Hash>, DagStoreError> {
        let txn = self.storage.begin_read()?;
        Ok(self.storage.consensus().get_last_committed(&txn)?)
    }

    /// Check if a round is decided.
    pub fn is_round_decided(&self, round: Round) -> Result<bool, DagStoreError> {
        let txn = self.storage.begin_read()?;
        Ok(self.storage.consensus().is_round_decided(&txn, round)?)
    }

    /// Get decided leader hashes for a round.
    pub fn get_decided_leaders(&self, round: Round) -> Result<Vec<Hash>, DagStoreError> {
        let txn = self.storage.begin_read()?;
        Ok(self.storage.consensus().get_decided_leaders(&txn, round)?)
    }

    /// Get all decided rounds up to a maximum.
    pub fn get_decided_rounds(&self, max_round: Round) -> Result<Vec<Round>, DagStoreError> {
        let txn = self.storage.begin_read()?;
        Ok(self
            .storage
            .consensus()
            .get_decided_rounds(&txn, max_round)?)
    }

    /// Get ancestors of a block up to a certain round (for parent selection).
    pub fn get_ancestors(&self, hash: &Hash, min_round: Round) -> Result<Vec<Hash>, DagStoreError> {
        let mut ancestors = Vec::new();
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(*hash);

        while let Some(current) = queue.pop_front() {
            if visited.contains(&current) {
                continue;
            }
            visited.insert(current);

            let parents = self.get_parents(&current)?;
            for parent in parents {
                if parent.0 == current.0 {
                    continue;
                }
                let block = self.get_block(&parent)?;
                if block.round >= min_round {
                    ancestors.push(parent);
                    queue.push_back(parent);
                }
            }
        }

        Ok(ancestors)
    }

    /// Find blocks that can serve as parents for a new block at the given round.
    /// Returns up to `max_parents` blocks from the previous round that are valid parents.
    pub fn find_parents(
        &self,
        round: Round,
        max_parents: usize,
    ) -> Result<Vec<BlockReference>, DagStoreError> {
        if round == 0 {
            return Ok(Vec::new());
        }

        let prev_round = round - 1;
        let blocks = self.get_blocks_by_round(prev_round)?;

        // For now, return all blocks from previous round as parent references
        // In a full implementation, we'd filter by stake weight and validity
        let parents: Vec<BlockReference> = blocks
            .into_iter()
            .take(max_parents)
            .map(|b| BlockReference {
                author: b.author,
                round: b.round,
                digest: b.digest,
            })
            .collect();

        Ok(parents)
    }

    /// Store a new DAG block.
    pub fn put_block(&self, block: &StatementBlock) -> Result<(), DagStoreError> {
        if self.has_block(&block.digest)? {
            return Ok(());
        }
        let txn = self.storage.begin_write()?;
        self.storage.consensus().put_dag_block(&txn, block)?;
        txn.commit()?;
        Ok(())
    }

    /// Mark a round as decided with a leader hash.
    pub fn mark_round_decided(
        &self,
        round: Round,
        leader_hash: &Hash,
    ) -> Result<(), DagStoreError> {
        if self
            .get_decided_leaders(round)?
            .iter()
            .any(|existing| existing == leader_hash)
        {
            return Ok(());
        }
        let txn = self.storage.begin_write()?;
        self.storage
            .consensus()
            .mark_round_decided(&txn, round, leader_hash)?;
        txn.commit()?;
        Ok(())
    }

    /// Increment the committed leader height and set last committed.
    pub fn commit_leader(&self, leader_hash: &Hash) -> Result<u64, DagStoreError> {
        let txn = self.storage.begin_write()?;
        let height = self
            .storage
            .consensus()
            .increment_committed_leader_height(&txn)?;
        self.storage
            .consensus()
            .set_last_committed(&txn, leader_hash)?;
        txn.commit()?;
        Ok(height)
    }

    /// Prune DAG blocks below a certain round.
    pub fn prune_below(&self, min_round: Round) -> Result<u64, DagStoreError> {
        let txn = self.storage.begin_write()?;
        let pruned = self.storage.consensus().prune_dag_below(&txn, min_round)?;
        txn.commit()?;
        Ok(pruned)
    }

    /// Prune non-blue blocks from a committed wave.
    ///
    /// Deletes all blocks from the committed wave that are NOT in the blue set.
    /// Keeps blue blocks and previous tips needed for future colouring.
    pub fn prune_non_blue(
        &self,
        blue_hashes: &[Hash],
        committed_wave: u64,
    ) -> Result<u64, DagStoreError> {
        use std::collections::HashSet;

        let txn = self.storage.begin_write()?;

        // Convert blue_hashes to a HashSet for O(1) lookup
        let blue_set: HashSet<Hash> = blue_hashes.iter().copied().collect();

        // Calculate round range for the committed wave
        let wave_length = kvnc_types::WAVE_LENGTH;
        let wave_start = committed_wave * wave_length;
        let wave_end = wave_start + wave_length - 1;

        // Collect all blocks in the committed wave
        let round_table = txn.open_table(tables::DAG_BY_ROUND)?;
        let mut blocks_to_check = Vec::new();

        for entry in round_table.range((wave_start, 0u16)..=(wave_end, u16::MAX))? {
            let (_, block_hashes) = entry?;
            let hashes: Vec<[u8; 32]> = BincodeSerialize::from_bytes(&block_hashes.value())?;
            for hash in hashes {
                blocks_to_check.push(Hash(hash));
            }
        }
        // Drop the table to release the borrow on txn
        drop(round_table);

        // Determine which blocks to prune (those in the wave but not blue)
        let mut blocks_to_prune = Vec::new();
        for hash in blocks_to_check {
            if !blue_set.contains(&hash) {
                blocks_to_prune.push(hash);
            }
        }

        // Delete non-blue blocks and their links
        let mut pruned = 0;
        for block_hash in blocks_to_prune {
            self.delete_block_internal(&txn, &block_hash)?;
            pruned += 1;
        }

        txn.commit()?;
        Ok(pruned)
    }

    /// Prune all blocks from waves before the given wave minus the prune window.
    ///
    /// Deletes ALL blocks (blue and red) from waves < `wave - prune_window_waves`.
    /// Updates committed_leader_height accordingly.
    /// Must not break recovery (recover_committed_subdags only needs decided-round index).
    pub fn prune_waves_before(
        &self,
        wave: u64,
        prune_window_waves: u64,
    ) -> Result<u64, DagStoreError> {
        let txn = self.storage.begin_write()?;

        let wave_length = kvnc_types::WAVE_LENGTH;

        // Calculate the minimum wave to keep
        let min_wave_to_keep = wave.saturating_sub(prune_window_waves);

        // Calculate the maximum round to prune (last round of the wave before min_wave_to_keep)
        // If min_wave_to_keep is 0, we prune nothing (no waves before wave 0)
        let max_round_to_prune = if min_wave_to_keep == 0 {
            // No waves to prune - return 0 without calling prune_dag_below
            txn.commit()?;
            return Ok(0);
        } else {
            // Last round of wave (min_wave_to_keep - 1)
            min_wave_to_keep * wave_length - 1
        };

        // Prune all blocks below or equal to this round
        let pruned = self
            .storage
            .consensus()
            .prune_dag_below(&txn, max_round_to_prune)?;

        // The committed_leader_height counter is not changed here because
        // it represents the total number of committed leaders ever, not just recent ones.
        // The decided_rounds index is what recovery uses, and it's not touched by pruning.

        txn.commit()?;
        Ok(pruned)
    }

    /// Internal helper to delete a single block and all its links.
    fn delete_block_internal(
        &self,
        txn: &WriteTransaction,
        block_hash: &Hash,
    ) -> Result<(), DagStoreError> {
        let key = hash_to_bytes(block_hash);

        // Remove from dag_blocks
        {
            let mut table = txn.open_table(tables::DAG_BLOCKS)?;
            table.remove(key)?;
        }

        // Remove from dag_parents and get parents for child cleanup
        let parents: Vec<[u8; 32]> = {
            let mut table = txn.open_table(tables::DAG_PARENTS)?;
            let value = table.get(key)?;
            let parents = value
                .as_ref()
                .map(|v| BincodeSerialize::from_bytes(&v.value()).unwrap_or_default())
                .unwrap_or_default();
            // Drop the guard before removing
            drop(value);
            table.remove(key)?;
            parents
        };

        // Update parents' children lists
        for parent_hash in parents {
            let mut children: Vec<[u8; 32]> = {
                let table = txn.open_table(tables::DAG_CHILDREN)?;
                let value = table.get(parent_hash)?;
                value
                    .map(|v| BincodeSerialize::from_bytes(&v.value()).unwrap_or_default())
                    .unwrap_or_default()
            };
            children.retain(|h| *h != key);
            {
                let mut table = txn.open_table(tables::DAG_CHILDREN)?;
                table.insert(parent_hash, children.to_bytes()?)?;
            }
        }

        // Remove from dag_children
        {
            let mut table = txn.open_table(tables::DAG_CHILDREN)?;
            table.remove(key)?;
        }

        // Remove from dag_by_round (handled by the caller's round range scan)

        Ok(())
    }

    /// Get the mergeset for a leader block.
    ///
    /// The mergeset is the set of all blocks reachable from the leader that
    /// have not yet been included in any previously committed sub-DAG.
    /// We compute this by BFS from the leader via parent links, stopping when
    /// we reach a block whose round is <= the last committed leader's round
    /// (or more precisely, when the round is already decided).
    pub fn mergeset(&self, leader: &Hash) -> Result<Vec<Hash>, DagStoreError> {
        let mut result = Vec::new();
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();

        // Get the last committed leader's round to know where to stop.
        // If there's no committed leader yet, we don't stop at any round.
        let committed_leader_round = match self.get_last_committed()? {
            Some(last_committed_hash) => {
                let block = self.get_block(&last_committed_hash)?;
                Some(block.round)
            }
            None => None, // No committed leaders yet - don't stop
        };

        queue.push_back(*leader);
        visited.insert(*leader);

        while let Some(current) = queue.pop_front() {
            let block = self.get_block(&current)?;

            // If there's a committed leader and this block's round is <= committed_leader_round,
            // it's already in a previous sub-DAG. Don't include it and don't traverse further.
            if current != *leader {
                if let Some(committed_round) = committed_leader_round {
                    if block.round <= committed_round {
                        continue;
                    }
                }
            }

            result.push(current);

            // Traverse parents
            for parent in self.get_parents(&current)? {
                if visited.insert(parent) {
                    queue.push_back(parent);
                }
            }
        }

        Ok(result)
    }
}
