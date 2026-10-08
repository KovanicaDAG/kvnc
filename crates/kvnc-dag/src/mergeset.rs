//! Mergeset extraction helper for MysticGhost.
//!
//! The mergeset of a committed leader is the set of all blocks that are
//! reachable from the leader and have not yet been included in any previous
//! CommittedSubDag.
//!
//! This module provides a clean API; the real implementation should reuse
//! the existing reachability / ancestor indexes of the DAG store.

use crate::dag_store::DagStore;
use kvnc_types::hash::Hash;
use std::collections::{HashSet, VecDeque};
use std::sync::LazyLock;

/// Trait that the DAG store should implement for mergeset computation.
pub trait DagReachability {
    /// Returns true if `ancestor` is in the past cone of `block` (or equal).
    fn is_ancestor(&self, ancestor: &Hash, block: &Hash) -> bool;

    /// Direct parents of a block.
    fn parents(&self, block: &Hash) -> Vec<Hash>;

    /// All blocks that have already been committed in previous sub-dags.
    fn already_committed(&self) -> &HashSet<Hash>;

    /// Get a block by hash (for reading round info).
    fn get_block(
        &self,
        hash: &Hash,
    ) -> Result<kvnc_types::block::StatementBlock, crate::DagStoreError>;

    /// Get the last committed leader's round, if any.
    fn committed_leader_round(&self) -> Result<Option<u64>, crate::DagStoreError>;
}

/// Compute the mergeset of `leader`.
///
/// Simple BFS / DFS implementation. For production, replace with an
/// indexed version that uses the reachability oracle.
pub fn compute_mergeset<D: DagReachability>(
    dag: &D,
    leader: &Hash,
) -> Result<Vec<Hash>, crate::DagStoreError> {
    let mut result = Vec::new();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();

    let committed_leader_round = dag.committed_leader_round()?;

    queue.push_back(*leader);
    visited.insert(*leader);

    while let Some(current) = queue.pop_front() {
        let block = dag.get_block(&current)?;

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

        for parent in dag.parents(&current) {
            if visited.insert(parent) {
                queue.push_back(parent);
            }
        }
    }

    // The classic mergeset also includes blocks that point *to* the leader
    // but are not yet committed. Depending on your exact DAG store API you
    // may need a second pass over children / tips. Adjust accordingly.
    Ok(result)
}

impl DagReachability for DagStore {
    fn is_ancestor(&self, ancestor: &Hash, block: &Hash) -> bool {
        // Use get_ancestors with min_round = 0 to check reachability
        self.get_ancestors(block, 0)
            .map(|ancestors| ancestors.contains(ancestor))
            .unwrap_or(false)
    }

    fn parents(&self, block: &Hash) -> Vec<Hash> {
        self.get_parents(block).unwrap_or_default()
    }

    fn already_committed(&self) -> &HashSet<Hash> {
        // This is a placeholder - in reality we'd maintain a committed set.
        // For now, we compute committed blocks on-the-fly in compute_mergeset
        // using committed_leader_round.
        static EMPTY: LazyLock<HashSet<Hash>> = LazyLock::new(HashSet::new);
        &EMPTY
    }

    fn get_block(
        &self,
        hash: &Hash,
    ) -> Result<kvnc_types::block::StatementBlock, crate::DagStoreError> {
        DagStore::get_block(self, hash)
    }

    fn committed_leader_round(&self) -> Result<Option<u64>, crate::DagStoreError> {
        match self.get_last_committed()? {
            Some(last_committed_hash) => {
                let block = self.get_block(&last_committed_hash)?;
                Ok(Some(block.round))
            }
            None => Ok(None), // No committed leaders yet - don't stop
        }
    }
}

#[cfg(test)]
mod tests {
    // Add synthetic DAG tests here once the real store is available.
}
