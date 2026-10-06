//! Transaction mempool with fee-based prioritization.

#![deny(unsafe_code)]
#![allow(missing_docs)]
#![allow(clippy::result_large_err)]
#![allow(clippy::large_enum_variant)]

use kvnc_storage::Storage;
use kvnc_types::{
    hash::Hash,
    transaction::{Transaction, TransactionKind},
};
use parking_lot::RwLock;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;
use tracing::{info, warn};

/// Mempool implementation with fee-based prioritization.
pub struct Mempool {
    config: MempoolConfig,
    #[allow(dead_code)]
    storage: Arc<Storage>,
    /// Transactions organized by fee rate (highest first).
    /// Maps fee_rate -> transactions with that fee rate.
    by_fee_rate: RwLock<BTreeMap<u64, VecDeque<Transaction>>>,
    /// All transactions by hash for quick lookup.
    by_hash: RwLock<HashMap<Hash, Transaction>>,
    /// Transactions in the pending queue (not yet propagated).
    pending_propagation: RwLock<VecDeque<Hash>>,
    /// Last time we rebroadcast transactions.
    last_rebroadcast: RwLock<Instant>,
    /// Total size in bytes.
    total_size: RwLock<usize>,
}

impl Mempool {
    /// Create a new mempool.
    pub fn new(config: MempoolConfig, storage: Arc<Storage>) -> Self {
        Self {
            config,
            storage,
            by_fee_rate: RwLock::new(BTreeMap::new()),
            by_hash: RwLock::new(HashMap::new()),
            pending_propagation: RwLock::new(VecDeque::new()),
            last_rebroadcast: RwLock::new(Instant::now()),
            total_size: RwLock::new(0),
        }
    }

    /// Get the number of transactions in the mempool.
    pub fn len(&self) -> usize {
        self.by_hash.read().len()
    }

    /// Check if the mempool is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get a transaction by hash.
    pub fn get(&self, hash: &Hash) -> Option<Transaction> {
        self.by_hash.read().get(hash).cloned()
    }

    /// Check if a transaction exists in the mempool.
    pub fn contains(&self, hash: &Hash) -> bool {
        self.by_hash.read().contains_key(hash)
    }

    /// Add a transaction to the mempool.
    pub fn add_transaction(&self, tx: Transaction) -> Result<(), MempoolError> {
        // Check if we already have this transaction
        if self.by_hash.read().contains_key(&tx.hash) {
            return Err(MempoolError::AlreadyExists);
        }

        // Validate the transaction
        self.validate_transaction(&tx)?;

        // Check if we need to evict transactions to make room
        self.maybe_evict()?;

        // Calculate fee rate
        let fee_rate = self.calculate_fee_rate(&tx);

        // Add to by_fee_rate (highest fee rate first)
        {
            let mut by_fee_rate = self.by_fee_rate.write();
            by_fee_rate
                .entry(fee_rate)
                .or_default()
                .push_back(tx.clone());
        }

        // Add to by_hash
        self.by_hash.write().insert(tx.hash, tx.clone());

        // Add to pending propagation
        self.pending_propagation.write().push_back(tx.hash);

        // Update total size
        let tx_size = bincode::serialize(&tx).unwrap_or_default().len();
        *self.total_size.write() += tx_size;

        info!(
            "Added transaction {} to mempool (fee_rate={}, size={})",
            tx.hash, fee_rate, tx_size
        );

        Ok(())
    }

    /// Get the next transactions for block proposal (up to MAX_TXS_PER_BLOCK).
    pub fn get_next_transactions(&self, max_txs: usize) -> Vec<Transaction> {
        let mut by_fee_rate = self.by_fee_rate.write();
        let mut result = Vec::new();

        // Iterate from highest fee rate to lowest
        for (_, txs) in by_fee_rate.iter_mut().rev() {
            while let Some(tx) = txs.pop_front() {
                if result.len() >= max_txs {
                    break;
                }
                result.push(tx);
            }
            if result.len() >= max_txs {
                break;
            }
        }

        result
    }

    /// Remove transactions that have been included in a committed block.
    pub fn remove_committed(&self, committed_txs: &[Hash]) {
        let mut by_fee_rate = self.by_fee_rate.write();
        let mut by_hash = self.by_hash.write();
        let mut total_size = self.total_size.write();

        for hash in committed_txs {
            if let Some(tx) = by_hash.remove(hash) {
                // Remove from by_fee_rate
                let fee_rate = self.calculate_fee_rate(&tx);
                if let Some(txs) = by_fee_rate.get_mut(&fee_rate) {
                    txs.retain(|t| t.hash != *hash);
                    if txs.is_empty() {
                        by_fee_rate.remove(&fee_rate);
                    }
                }

                // Update total size
                let tx_size = bincode::serialize(&tx).unwrap_or_default().len();
                *total_size = total_size.saturating_sub(tx_size);
            }
        }
    }

    /// Get transactions ready for propagation.
    pub fn get_pending_propagation(&self) -> Vec<Hash> {
        let mut pending = self.pending_propagation.write();
        let mut result = Vec::new();
        while let Some(hash) = pending.pop_front() {
            result.push(hash);
        }
        result
    }

    /// Mark transactions as propagated.
    pub fn mark_propagated(&self, hashes: &[Hash]) {
        // Already removed from pending_propagation when we called get_pending_propagation
        // This is a no-op but kept for API consistency
        let _ = hashes;
    }

    /// Rebroadcast pending transactions (called periodically).
    pub fn rebroadcast(&self) -> Vec<Transaction> {
        let mut last_rebroadcast = self.last_rebroadcast.write();
        if last_rebroadcast.elapsed() < self.config.rebroadcast_interval {
            return Vec::new();
        }
        *last_rebroadcast = Instant::now();

        // Return all transactions for rebroadcast
        self.by_hash.read().values().cloned().collect()
    }

    /// Validate a transaction before adding to mempool.
    fn validate_transaction(&self, tx: &Transaction) -> Result<(), MempoolError> {
        // Verify signature
        // In a real implementation, this would verify the signature against the sender's public key
        // For now, we skip signature verification in the mempool

        // Check nonce is valid (would need to check against storage)
        // For now, we skip nonce verification in the mempool

        // Check fee is reasonable (not zero for non-stake transactions)
        let is_stake = matches!(tx.kind, TransactionKind::Stake { .. });
        if tx.fee == 0 && !is_stake {
            return Err(MempoolError::ZeroFee);
        }

        // Check gas limit is reasonable
        if let TransactionKind::Call { gas_limit, .. } = &tx.kind {
            if *gas_limit > self.config.max_gas_limit {
                return Err(MempoolError::GasLimitTooHigh);
            }
        }

        // Check transaction size
        let tx_size = bincode::serialize(tx).unwrap_or_default().len();
        if tx_size > self.config.max_tx_size {
            return Err(MempoolError::TransactionTooLarge(tx_size));
        }

        Ok(())
    }

    /// Calculate fee rate (fee per byte).
    fn calculate_fee_rate(&self, tx: &Transaction) -> u64 {
        let tx_size = bincode::serialize(tx).unwrap_or_default().len();
        if tx_size == 0 {
            return 0;
        }
        tx.fee / tx_size as u64
    }

    /// Evict low fee-rate transactions if mempool is full.
    fn maybe_evict(&self) -> Result<(), MempoolError> {
        let total_size = *self.total_size.read();
        if total_size >= self.config.max_mempool_size {
            // Evict lowest fee-rate transactions until we're under the limit
            let mut by_fee_rate = self.by_fee_rate.write();
            let mut by_hash = self.by_hash.write();
            let mut total_size = self.total_size.write();

            while *total_size >= self.config.max_mempool_size && !by_fee_rate.is_empty() {
                // Get the lowest fee rate
                if let Some(entry) = by_fee_rate.first_entry() {
                    let (lowest_fee_rate, mut txs) = entry.remove_entry();
                    if let Some(tx) = txs.pop_front() {
                        by_hash.remove(&tx.hash);
                        let tx_size = bincode::serialize(&tx).unwrap_or_default().len();
                        *total_size = total_size.saturating_sub(tx_size);

                        warn!(
                            "Evicted transaction {} from mempool (fee_rate={})",
                            tx.hash, lowest_fee_rate
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// Get mempool statistics.
    pub fn stats(&self) -> MempoolStats {
        let by_hash = self.by_hash.read();
        let by_fee_rate = self.by_fee_rate.read();
        let total_size = *self.total_size.read();

        let mut fee_rates = Vec::new();
        for (fee_rate, txs) in by_fee_rate.iter() {
            fee_rates.push((*fee_rate, txs.len()));
        }

        MempoolStats {
            tx_count: by_hash.len(),
            total_size,
            fee_rates,
        }
    }
}

/// Mempool configuration.
#[derive(Clone, Debug)]
pub struct MempoolConfig {
    /// Maximum size of the mempool in bytes.
    pub max_mempool_size: usize,
    /// Maximum size of a single transaction in bytes.
    pub max_tx_size: usize,
    /// Maximum gas limit for contract calls.
    pub max_gas_limit: u64,
    /// Interval for rebroadcasting transactions.
    pub rebroadcast_interval: std::time::Duration,
    /// Minimum fee rate for transactions (fee per byte).
    pub min_fee_rate: u64,
}

impl Default for MempoolConfig {
    fn default() -> Self {
        Self {
            max_mempool_size: 100 * 1024 * 1024, // 100 MB
            max_tx_size: 1024 * 1024,            // 1 MB
            max_gas_limit: 10_000_000,
            rebroadcast_interval: std::time::Duration::from_secs(30),
            min_fee_rate: 1,
        }
    }
}

/// Mempool statistics.
#[derive(Clone, Debug)]
pub struct MempoolStats {
    pub tx_count: usize,
    pub total_size: usize,
    pub fee_rates: Vec<(u64, usize)>, // (fee_rate, count)
}

/// Errors that can occur in mempool operations.
#[derive(Debug, thiserror::Error)]
pub enum MempoolError {
    #[error("Transaction already exists in mempool")]
    AlreadyExists,
    #[error("Transaction validation failed: {0}")]
    Validation(String),
    #[error("Transaction has zero fee")]
    ZeroFee,
    #[error("Gas limit too high")]
    GasLimitTooHigh,
    #[error("Transaction too large: {0} bytes")]
    TransactionTooLarge(usize),
    #[error("Mempool is full")]
    Full,
    #[error("Crypto error: {0}")]
    Crypto(#[from] kvnc_crypto::CryptoError),
    #[error("Storage error: {0}")]
    Storage(#[from] kvnc_storage::StorageError),
    #[error("Serialization error: {0}")]
    Serialization(#[from] bincode::Error),
}
