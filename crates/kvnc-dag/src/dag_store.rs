//! DAG store for KVNC.
//!
#![allow(missing_docs)]
//! Provides high-level operations on the DAG structure using the storage layer.

use kvnc_storage::Storage;
use kvnc_types::{
    block::{BlockReference, StatementBlock},
    hash::Hash,
    AuthorityIndex, Round,
};
use redb::CommitError;
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
