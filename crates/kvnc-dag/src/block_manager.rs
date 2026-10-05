//! Block manager for KVNC.
//!
//! Handles block proposal, validation, and broadcast.

use crate::DagStore;
use kvnc_crypto::{self as crypto, CryptoError};
use kvnc_types::{
    block::{BlockReference, StatementBlock},
    hash::Hash,
    transaction::Transaction,
    AuthorityIndex, Round, MAX_TXS_PER_BLOCK, SigningKey, Signature,
};
use parking_lot::RwLock;
use std::collections::VecDeque;
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
}

impl BlockManager {
    /// Create a new block manager.
    pub fn new(dag_store: Arc<DagStore>) -> Self {
        Self {
            dag_store,
            pending_txs: Arc::new(RwLock::new(VecDeque::new())),
            our_authority: RwLock::new(0), // Will be set when validator joins
            signing_key: RwLock::new(None),
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
            statements: Vec::new(), // Placeholder for votes
            signature: Signature([0; 64]), // Will be filled by sign_block
            digest,
        })
    }

    /// Sign a block with our signing key.
    fn sign_block(&self, mut block: StatementBlock) -> Result<StatementBlock, BlockManagerError> {
        let signing_key = self.signing_key.read().as_ref()
            .ok_or_else(|| BlockManagerError::Crypto(CryptoError::InvalidSignature))?.clone();
        
        let digest_bytes = block.digest.as_ref();
        let signature = crypto::sign(&signing_key, digest_bytes);
        
        block.signature = signature;
        Ok(block)
    }

    /// Validate a received block.
    pub fn validate_block(&self, block: &StatementBlock) -> Result<(), BlockManagerError> {
        // Verify signature - would need committee state in real implementation
        // For now, skip signature verification
        
        // Verify round
        if block.round == 0 {
            return Err(BlockManagerError::RoundMismatch { 
                expected: 0, 
                actual: block.round 
            });
        }

        // Verify parents exist and are from previous round
        for parent_ref in &block.parents {
            if parent_ref.round + 1 != block.round {
                return Err(BlockManagerError::ParentValidation(
                    format!("Parent round {} is not previous round {}", parent_ref.round, block.round - 1)
                ));
            }
            
            // Check parent exists
            if !self.dag_store.has_block(&parent_ref.digest)? {
                return Err(BlockManagerError::ParentValidation(
                    format!("Parent block {} not found", parent_ref.digest)
                ));
            }
        }

        // Verify digest matches
        let computed_digest = StatementBlock::compute_digest(
            block.author,
            block.round,
            &block.parents,
            &block.transactions,
        );
        
        if computed_digest != block.digest {
            return Err(BlockManagerError::InvalidBlock("Digest mismatch".to_string()));
        }

        // Verify transaction hashes (basic check)
        for tx in &block.transactions {
            if tx.hash() != tx.hash {
                return Err(BlockManagerError::InvalidBlock("Transaction hash mismatch".to_string()));
            }
        }

        Ok(())
    }

    /// Process a received block (validate and store if valid).
    pub fn process_block(&self, block: &StatementBlock) -> Result<(), BlockManagerError> {
        self.validate_block(block)?;
        self.dag_store.put_block(block)?;
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