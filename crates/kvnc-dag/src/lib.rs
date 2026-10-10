//! DAG store, causal ordering and block manager.

#![deny(unsafe_code)]
#![allow(missing_docs)]
#![allow(clippy::result_large_err)]
#![allow(clippy::large_enum_variant)]
#![allow(unused_imports)]
#![allow(unused_variables)]

use kvnc_storage::Storage;
use kvnc_types::{block::StatementBlock, hash::Hash, AuthorityIndex, Round};
use std::sync::Arc;
use thiserror::Error;
use tracing::{debug, info, warn};

pub mod block_manager;
pub mod dag_store;
pub mod mergeset;

pub use block_manager::{BlockManager, BlockManagerError};
pub use dag_store::{DagStore, DagStoreError};

/// Main DAG handle combining store and manager.
pub struct Dag {
    store: DagStore,
    manager: BlockManager,
}

impl Dag {
    /// Create a new DAG with the given storage.
    // TODO(owner): source chain_id/epoch from node config
    pub fn new(
        storage: Storage,
        signing_ctx: kvnc_types::SigningContext,
    ) -> Result<Self, DagError> {
        let store = DagStore::new(storage)?;
        let manager = BlockManager::new(Arc::new(store.clone()), signing_ctx);
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
        let _dag = Dag::new(
            storage,
            kvnc_types::SigningContext::new(kvnc_types::signing::chain_id::LOCAL),
        )
        .unwrap();
    }
}
