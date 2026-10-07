//! Block manager for KVNC.
//!
#![allow(missing_docs)]
//! Handles block proposal, validation, and broadcast.

use crate::DagStore;
use kvnc_crypto::{self as crypto, CryptoError};
use kvnc_types::{
    block::{BlockReference, StatementBlock},
    crypto::PublicKey,
    hash::Hash,
    transaction::Transaction,
    AuthorityIndex, Round, Signature, SigningKey, MAX_TXS_PER_BLOCK,
};
use parking_lot::RwLock;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use thiserror::Error;
use tracing::{debug, info, warn};

/// Errors specific to block manager operations.
#[derive(Error, Debug)]
pub enum BlockManagerError {
    #[error("Invalid block: {0}")]
    InvalidBlock(String),
    #[error("DAG store error: {0}")]
    DagStore(#[from] crate::DagStoreError),
    #[error("Crypto error: {0}")]
    Crypto(#[from] CryptoError),
    #[error("Not enough transactions for block")]
    NotEnoughTransactions,
    #[error("Round mismatch: expected {expected}, got {actual}")]
    RoundMismatch { expected: Round, actual: Round },
    #[error("Parent validation failed: {0}")]
    ParentValidation(String),
}

/// Block manager for proposing and validating blocks.
pub struct BlockManager {
    dag_store: Arc<DagStore>,
    /// Pending transactions waiting to be included in blocks.
    pending_txs: Arc<RwLock<VecDeque<Transaction>>>,
    /// The authority index of this validator.
    our_authority: RwLock<AuthorityIndex>,
    /// Our signing key for block signing.
    signing_key: RwLock<Option<SigningKey>>,
    authority_keys: RwLock<HashMap<AuthorityIndex, PublicKey>>,
}

impl BlockManager {
    /// Create a new block manager.
    pub fn new(dag_store: Arc<DagStore>) -> Self {
        Self {
            dag_store,
            pending_txs: Arc::new(RwLock::new(VecDeque::new())),
            our_authority: RwLock::new(0), // Will be set when validator joins
            signing_key: RwLock::new(None),
            authority_keys: RwLock::new(HashMap::new()),
        }
    }

    /// Set our authority index.
    pub fn set_authority(&self, authority: AuthorityIndex) {
        *self.our_authority.write() = authority;
    }

    /// Set our signing key.
    pub fn set_signing_key(&self, key: SigningKey) {
        *self.signing_key.write() = Some(key);
    }

    /// Set the committee public keys used to authenticate block authors.
    pub fn set_authority_keys(&self, keys: HashMap<AuthorityIndex, PublicKey>) {
        *self.authority_keys.write() = keys;
    }

    /// Get our authority index.
    pub fn our_authority(&self) -> AuthorityIndex {
        *self.our_authority.read()
    }

    /// Add a transaction to the pending pool.
    pub fn add_transaction(&self, tx: Transaction) {
        let mut pending = self.pending_txs.write();
        pending.push_back(tx);
    }

    /// Add multiple transactions to the pending pool.
    pub fn add_transactions(&self, txs: Vec<Transaction>) {
        let mut pending = self.pending_txs.write();
        pending.extend(txs);
    }

    /// Get the next transactions for block proposal (up to MAX_TXS_PER_BLOCK).
    pub fn get_next_transactions(&self) -> Vec<Transaction> {
        let mut pending = self.pending_txs.write();
        let mut txs = Vec::new();
        for _ in 0..MAX_TXS_PER_BLOCK {
            if let Some(tx) = pending.pop_front() {
                txs.push(tx);
            } else {
                break;
            }
        }
        txs
    }

    /// Propose a new block for the given round.
    pub fn propose_block(&self, round: Round) -> Result<StatementBlock, BlockManagerError> {
        // Get parent blocks from previous round
        let parents = self.dag_store.find_parents(round, MAX_TXS_PER_BLOCK)?;

        // Get transactions
        let transactions = self.get_next_transactions();
        if transactions.is_empty() && round > 0 {
            // Allow empty blocks for consensus rounds
        }

        // Create the block (without signature first)
        let block = self.create_block(round, parents, transactions)?;

        // Sign the block
        let signed_block = self.sign_block(block)?;

        // Store the block
        self.dag_store.put_block(&signed_block)?;

        Ok(signed_block)
    }

    /// Create a block without signing.
    fn create_block(
        &self,
        round: Round,
        parents: Vec<BlockReference>,
        transactions: Vec<Transaction>,
    ) -> Result<StatementBlock, BlockManagerError> {
        // Compute digest
        let digest = StatementBlock::compute_digest(
            *self.our_authority.read(),
            round,
            &parents,
            &transactions,
        );

        Ok(StatementBlock {
            author: *self.our_authority.read(),
            round,
            parents,
            transactions,
            statements: Vec::new(),        // Placeholder for votes
            signature: Signature([0; 64]), // Will be filled by sign_block
            digest,
        })
    }

    /// Sign a block with our signing key.
    fn sign_block(&self, mut block: StatementBlock) -> Result<StatementBlock, BlockManagerError> {
        let signing_key = self
            .signing_key
            .read()
            .as_ref()
            .ok_or_else(|| BlockManagerError::Crypto(CryptoError::InvalidSignature))?
            .clone();

        let digest_bytes = block.digest.as_ref();
        let signature = crypto::sign(&signing_key, digest_bytes);

        block.signature = signature;
        Ok(block)
    }

    /// Validate a received block.
    pub fn validate_block(&self, block: &StatementBlock) -> Result<(), BlockManagerError> {
        // Verify round
        if block.round == 0 {
            return Err(BlockManagerError::RoundMismatch {
                expected: 0,
                actual: block.round,
            });
        }

        self.validate_block_content(block)?;

        self.validate_parent_references(block)?;

        // Authenticate the full causal history. This also protects against
        // blocks written by older ingress paths or pre-existing invalid data.
        let mut pending: Vec<Hash> = block.parents.iter().map(|parent| parent.digest).collect();
        let mut visited = std::collections::HashSet::new();
        while let Some(ancestor_hash) = pending.pop() {
            if !visited.insert(ancestor_hash) {
                continue;
            }
            let ancestor = self.dag_store.get_block(&ancestor_hash)?;
            if ancestor.round == 0 {
                self.validate_canonical_genesis(&ancestor)?;
            } else {
                self.validate_block_content(&ancestor)?;
                self.validate_parent_references(&ancestor)?;
            }
            pending.extend(ancestor.parents.iter().map(|parent| parent.digest));
        }

        Ok(())
    }

    fn validate_block_content(&self, block: &StatementBlock) -> Result<(), BlockManagerError> {
        if block.transactions.len() > MAX_TXS_PER_BLOCK {
            return Err(BlockManagerError::InvalidBlock(
                "too many transactions".to_string(),
            ));
        }

        let computed_digest = StatementBlock::compute_digest(
            block.author,
            block.round,
            &block.parents,
            &block.transactions,
        );
        if computed_digest != block.digest {
            return Err(BlockManagerError::InvalidBlock(
                "Digest mismatch".to_string(),
            ));
        }

        let public_key = self
            .authority_keys
            .read()
            .get(&block.author)
            .copied()
            .ok_or_else(|| {
                BlockManagerError::InvalidBlock(format!("unknown block author {}", block.author))
            })?;
        crypto::verify_block_signature(&public_key, &block.digest, &block.signature)?;
        Ok(())
    }

    /// Validate that round zero is the genesis block constructed by the node.
    /// Its digest is derived from its contents rather than a hard-coded hash.
    fn validate_canonical_genesis(&self, block: &StatementBlock) -> Result<(), BlockManagerError> {
        let canonical_digest = StatementBlock::compute_digest(0, 0, &[], &[]);
        if block.author != 0
            || block.round != 0
            || !block.parents.is_empty()
            || !block.transactions.is_empty()
            || !block.statements.is_empty()
            || block.signature != Signature([0; 64])
            || block.digest != canonical_digest
        {
            return Err(BlockManagerError::InvalidBlock(
                "invalid canonical genesis block".to_string(),
            ));
        }
        Ok(())
    }

    /// Check that each referenced parent exists and its round and author match
    /// the reference, and that the edge advances exactly one round.
    fn validate_parent_references(&self, block: &StatementBlock) -> Result<(), BlockManagerError> {
        for parent_ref in &block.parents {
            if parent_ref.round.checked_add(1) != Some(block.round) {
                return Err(BlockManagerError::ParentValidation(format!(
                    "Parent round {} is not previous round {}",
                    parent_ref.round,
                    block.round.saturating_sub(1)
                )));
            }

            let parent = self.dag_store.get_block(&parent_ref.digest).map_err(|_| {
                BlockManagerError::ParentValidation(format!(
                    "Parent block {} not found",
                    parent_ref.digest
                ))
            })?;
            if parent.author != parent_ref.author || parent.round != parent_ref.round {
                return Err(BlockManagerError::ParentValidation(format!(
                    "Parent reference metadata does not match block {}",
                    parent_ref.digest
                )));
            }
        }
        Ok(())
    }

    /// Process a received block (validate and store if valid).
    pub fn process_block(&self, block: &StatementBlock) -> Result<(), BlockManagerError> {
        self.validate_block(block)?;
        if !self.dag_store.has_block(&block.digest)? {
            self.dag_store.put_block(block)?;
        }
        Ok(())
    }

    /// Get pending transaction count.
    pub fn pending_count(&self) -> usize {
        self.pending_txs.read().len()
    }

    /// Clear pending transactions (e.g., after they've been included in a committed block).
    pub fn clear_pending(&self) {
        self.pending_txs.write().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn received_block_requires_committee_signature_and_matching_digest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = kvnc_storage::Storage::new(dir.path().join("dag.redb")).unwrap();
        let store = Arc::new(DagStore::new(storage).unwrap());
        let manager = BlockManager::new(store);
        let (key, public_key) = crypto::generate_keypair();
        manager.set_authority_keys(HashMap::from([(0, public_key)]));

        let digest = StatementBlock::compute_digest(0, 1, &[], &[]);
        let valid = StatementBlock {
            author: 0,
            round: 1,
            parents: Vec::new(),
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: crypto::sign(&key, digest.as_ref()),
            digest,
        };
        manager.validate_block(&valid).expect("valid signed block");

        let mut bad_signature = valid.clone();
        bad_signature.signature = Signature([0; 64]);
        assert!(manager.validate_block(&bad_signature).is_err());

        let mut bad_digest = valid;
        bad_digest.digest = Hash::zero();
        assert!(manager.validate_block(&bad_digest).is_err());

        let mut unknown_author = bad_digest;
        unknown_author.author = 9;
        unknown_author.digest = StatementBlock::compute_digest(9, 1, &[], &[]);
        assert!(manager.validate_block(&unknown_author).is_err());
    }

    #[test]
    fn valid_child_cannot_commit_a_pre_stored_invalid_ancestor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = kvnc_storage::Storage::new(dir.path().join("dag.redb")).unwrap();
        let store = Arc::new(DagStore::new(storage).unwrap());
        let manager = BlockManager::new(store.clone());
        let (key, public_key) = crypto::generate_keypair();
        manager.set_authority_keys(HashMap::from([(0, public_key)]));

        let parent_digest = StatementBlock::compute_digest(0, 1, &[], &[]);
        let invalid_parent = StatementBlock {
            author: 0,
            round: 1,
            parents: Vec::new(),
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: Signature([0; 64]),
            digest: parent_digest,
        };
        // Simulate the current network service's store-before-event behavior.
        store.put_block(&invalid_parent).unwrap();

        let parent_ref = BlockReference {
            author: invalid_parent.author,
            round: invalid_parent.round,
            digest: invalid_parent.digest,
        };
        let parents = vec![parent_ref];
        let child_digest = StatementBlock::compute_digest(0, 2, &parents, &[]);
        let child = StatementBlock {
            author: 0,
            round: 2,
            parents,
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: crypto::sign(&key, child_digest.as_ref()),
            digest: child_digest,
        };

        assert!(manager.validate_block(&child).is_err());
    }

    #[test]
    fn rejects_noncanonical_round_zero_ancestor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = kvnc_storage::Storage::new(dir.path().join("dag.redb")).unwrap();
        let store = Arc::new(DagStore::new(storage).unwrap());
        let manager = BlockManager::new(store.clone());
        let (key, public_key) = crypto::generate_keypair();
        manager.set_authority_keys(HashMap::from([(0, public_key)]));

        let mut malformed_genesis = StatementBlock {
            author: 0,
            round: 0,
            parents: Vec::new(),
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: Signature([0; 64]),
            digest: StatementBlock::compute_digest(0, 0, &[], &[]),
        };
        // Keep the stored key and declared contents internally consistent but
        // violate the canonical genesis content invariant.
        malformed_genesis.author = 1;
        malformed_genesis.digest = StatementBlock::compute_digest(1, 0, &[], &[]);
        store.put_block(&malformed_genesis).unwrap();

        let parents = vec![BlockReference {
            author: malformed_genesis.author,
            round: malformed_genesis.round,
            digest: malformed_genesis.digest,
        }];
        let digest = StatementBlock::compute_digest(0, 1, &parents, &[]);
        let child = StatementBlock {
            author: 0,
            round: 1,
            parents,
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: crypto::sign(&key, digest.as_ref()),
            digest,
        };

        assert!(manager.validate_block(&child).is_err());
    }

    #[test]
    fn rejects_nested_ancestor_with_invalid_parent_round_metadata() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = kvnc_storage::Storage::new(dir.path().join("dag.redb")).unwrap();
        let store = Arc::new(DagStore::new(storage).unwrap());
        let manager = BlockManager::new(store.clone());
        let (key, public_key) = crypto::generate_keypair();
        manager.set_authority_keys(HashMap::from([(0, public_key)]));

        let genesis = StatementBlock {
            author: 0,
            round: 0,
            parents: Vec::new(),
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: Signature([0; 64]),
            digest: StatementBlock::compute_digest(0, 0, &[], &[]),
        };
        store.put_block(&genesis).unwrap();

        let genesis_parent = vec![BlockReference {
            author: genesis.author,
            round: genesis.round,
            digest: genesis.digest,
        }];
        let valid_digest = StatementBlock::compute_digest(0, 1, &genesis_parent, &[]);
        let valid_child = StatementBlock {
            author: 0,
            round: 1,
            parents: genesis_parent,
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: crypto::sign(&key, valid_digest.as_ref()),
            digest: valid_digest,
        };
        manager
            .validate_block(&valid_child)
            .expect("canonical genesis is accepted as an ancestor");

        // The block's own digest and signature are valid, but its causal edge
        // falsely claims that genesis is at round one.
        let bad_parent_ref = BlockReference {
            author: genesis.author,
            round: 1,
            digest: genesis.digest,
        };
        let bad_parents = vec![bad_parent_ref];
        let bad_digest = StatementBlock::compute_digest(0, 1, &bad_parents, &[]);
        let bad_ancestor = StatementBlock {
            author: 0,
            round: 1,
            parents: bad_parents,
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: crypto::sign(&key, bad_digest.as_ref()),
            digest: bad_digest,
        };
        store.put_block(&bad_ancestor).unwrap();

        let parents = vec![BlockReference {
            author: bad_ancestor.author,
            round: bad_ancestor.round,
            digest: bad_ancestor.digest,
        }];
        let digest = StatementBlock::compute_digest(0, 2, &parents, &[]);
        let child = StatementBlock {
            author: 0,
            round: 2,
            parents,
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: crypto::sign(&key, digest.as_ref()),
            digest,
        };

        assert!(manager.validate_block(&child).is_err());
    }
}
