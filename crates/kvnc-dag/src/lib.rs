//! DAG store, causal ordering and block manager.

#![deny(unsafe_code)]
#![warn(missing_docs)]

use kvnc_storage::Storage;
use kvnc_types::{
    block::StatementBlock,
    hash::Hash,
    AuthorityIndex, Round,
};
use std::sync::Arc;
use thiserror::Error;
use tracing::{debug, info, warn};

pub mod block_manager;
pub mod dag_store;

pub use block_manager::{BlockManager, BlockManagerError};
pub use dag_store::{DagStore, DagStoreError};

/// Main DAG handle combining store and manager.
pub struct Dag {
    store: DagStore,
    manager: BlockManager,
}

impl Dag {
    /// Create a new DAG with the given storage.
    pub fn new(storage: Storage) -> Result<Self, DagError> {
        let store = DagStore::new(storage)?;
        let manager = BlockManager::new(Arc::new(store.clone()));
        Ok(Self { store, manager })
    }

    /// Get a reference to the DAG store.
    pub fn store(&self) -> &DagStore {
        &self.store
    }

    /// Get a reference to the block manager.
    pub fn manager(&self) -> &BlockManager {
        &self.manager
    }
}

/// Errors that can occur in DAG operations.
#[derive(Error, Debug)]
pub enum DagError {
    #[error("Store error: {0}")]
    Store(#[from] DagStoreError),
    #[error("Manager error: {0}")]
    Manager(#[from] BlockManagerError),
    #[error("Storage error: {0}")]
    Storage(#[from] kvnc_storage::StorageError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvnc_storage::Storage;
    use tempfile::tempdir;

    #[test]
    fn test_dag_creation() {
        let dir = tempdir().unwrap();
        let storage = Storage::new(dir.path().join("test.db")).unwrap();
        let _dag = Dag::new(storage).unwrap();
    }
}