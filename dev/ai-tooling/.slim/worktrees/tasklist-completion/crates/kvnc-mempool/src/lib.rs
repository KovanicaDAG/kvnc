//! Transaction mempool with fee-based prioritization and admission control.

#![deny(unsafe_code)]
#![allow(missing_docs)]
#![allow(clippy::result_large_err)]
#![allow(clippy::large_enum_variant)]

use kvnc_storage::{state_store::Account, Storage};
use kvnc_types::{
    hash::Hash,
    transaction::{Transaction, TransactionKind},
    Address,
};
use parking_lot::RwLock;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;
use tracing::{info, warn};

/// Base units of value a transaction moves out of the sender's account
/// (excluding the fee).
fn spend_amount(tx: &Transaction) -> u64 {
    match &tx.kind {
        TransactionKind::Transfer { amount, .. } => *amount,
        TransactionKind::Stake { amount } => *amount,
        TransactionKind::Unstake { .. }
        | TransactionKind::Deploy { .. }
        | TransactionKind::Call { .. } => 0,
    }
}

/// Total amount (value + fee) a transaction commits from the sender.
fn total_spend(tx: &Transaction) -> u128 {
    spend_amount(tx) as u128 + tx.fee as u128
}

/// Mempool implementation with fee-based prioritization.
pub struct Mempool {
    config: MempoolConfig,
    storage: Arc<Storage>,
    /// Transactions organized by fee rate (highest first).
    /// Maps fee_rate -> transactions with that fee rate.
    by_fee_rate: RwLock<BTreeMap<u64, VecDeque<Transaction>>>,
    /// All transactions by hash for quick lookup.
    by_hash: RwLock<HashMap<Hash, Transaction>>,
    /// Transactions indexed by sender and nonce for conflict resolution and
    /// per-sender ordering.
    by_sender: RwLock<HashMap<Address, BTreeMap<u64, Hash>>>,
    /// Total pending spend (value + fee) per sender.
    pending_value: RwLock<HashMap<Address, u128>>,
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
            by_sender: RwLock::new(HashMap::new()),
            pending_value: RwLock::new(HashMap::new()),
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

    /// Load the current on-chain account for `address` (defaulting to an empty
    /// account when it has never been touched).
    fn account_for(&self, address: &Address) -> Result<Account, MempoolError> {
        let txn = self.storage.begin_read()?;
        self.storage
            .state()
            .get_account_or_default(&txn, address)
            .map_err(|e| MempoolError::Storage(kvnc_storage::StorageError::from(e)))
    }

    /// Add a transaction to the mempool after admission control.
    ///
    /// A transaction is only accepted when its signature is valid, its nonce is
    /// not stale, it can cover value + fee against the current state and any
    /// already-pending spends from the same sender, and it satisfies the
    /// configured fee-rate / gas / size limits. When a transaction reuses the
    /// nonce of one already in the pool, the deterministic conflict rule keeps
    /// the transaction with the higher fee rate (ties broken by hash).
    pub fn add_transaction(&self, tx: Transaction) -> Result<(), MempoolError> {
        // Check if we already have this transaction
        if self.by_hash.read().contains_key(&tx.hash) {
            return Err(MempoolError::AlreadyExists);
        }

        // Stateless checks (signature, fee, gas, size, fee rate).
        self.validate_transaction(&tx)?;

        // 5.2 Conflict resolution: a same-sender, same-nonce transaction is a
        // direct conflict. Keep the highest fee rate, evict the loser.
        let replaced = {
            let by_sender = self.by_sender.read();
            let existing_hash = by_sender
                .get(&tx.sender)
                .and_then(|nonces| nonces.get(&tx.nonce))
                .copied();
            match existing_hash {
                Some(existing_hash) => {
                    let existing = self.by_hash.read().get(&existing_hash).cloned();
                    match existing {
                        Some(existing) => {
                            let new_rate = self.calculate_fee_rate(&tx);
                            let old_rate = self.calculate_fee_rate(&existing);
                            let new_wins = new_rate > old_rate
                                || (new_rate == old_rate && tx.hash.0 > existing.hash.0);
                            if new_wins {
                                Some(existing.hash)
                            } else {
                                return Err(MempoolError::ReplacementUnderpriced {
                                    nonce: tx.nonce,
                                    existing_rate: old_rate,
                                    new_rate,
                                });
                            }
                        }
                        None => None,
                    }
                }
                None => None,
            }
        };
        if let Some(old) = replaced {
            self.remove_transaction(&old);
        }

        // 5.1 Admission control: nonce against state and balance for
        // value + fee against on-chain funds plus pending spends from this sender.
        let account = self.account_for(&tx.sender)?;
        if tx.nonce < account.nonce {
            return Err(MempoolError::NonceTooLow {
                expected: account.nonce,
                got: tx.nonce,
            });
        }
        let pending = *self.pending_value.read().get(&tx.sender).unwrap_or(&0);
        let required = pending + total_spend(&tx);
        if (account.balance as u128) < required {
            return Err(MempoolError::InsufficientBalance {
                required,
                available: account.balance as u128,
            });
        }

        // Check if we need to evict transactions to make room.
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

        // Index by sender/nonce so ordering and future conflicts are O(log n).
        {
            let mut by_sender = self.by_sender.write();
            by_sender
                .entry(tx.sender)
                .or_default()
                .insert(tx.nonce, tx.hash);
        }

        // Track pending spend so admission can account for the in-flight chain.
        {
            let mut pending_value = self.pending_value.write();
            *pending_value.entry(tx.sender).or_insert(0) += total_spend(&tx);
        }

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
    ///
    /// Transactions are selected by fee rate, but a sender's transactions are
    /// always emitted in ascending nonce order and only when they extend the
    /// account's contiguous nonce sequence (starting from its on-chain nonce).
    /// Selected transactions are removed from the pool.
    pub fn get_next_transactions(&self, max_txs: usize) -> Vec<Transaction> {
        if max_txs == 0 {
            return Vec::new();
        }

        // Snapshot and group pending transactions by sender.
        let snapshot: Vec<Transaction> = self.by_hash.read().values().cloned().collect();
        if snapshot.is_empty() {
            return Vec::new();
        }

        let mut grouped: HashMap<Address, BTreeMap<u64, Transaction>> = HashMap::new();
        for tx in snapshot {
            grouped.entry(tx.sender).or_default().insert(tx.nonce, tx);
        }

        // Build each sender's contiguous nonce chain from its on-chain nonce.
        let mut chains: HashMap<Address, VecDeque<Transaction>> = HashMap::new();
        for (sender, nonces) in grouped {
            let base = self.account_for(&sender).map(|a| a.nonce).unwrap_or(0);
            let mut next = base;
            let mut chain = VecDeque::new();
            for (nonce, tx) in nonces {
                if nonce < next {
                    continue;
                }
                if nonce == next {
                    chain.push_back(tx);
                    next += 1;
                } else {
                    // Gap: later nonces cannot execute until the gap is filled.
                    break;
                }
            }
            if !chain.is_empty() {
                chains.insert(sender, chain);
            }
        }

        // Greedily pick the highest fee-rate ready transaction across senders.
        let mut result = Vec::new();
        while result.len() < max_txs {
            let mut best: Option<(u64, [u8; 32], Address)> = None;
            for (sender, chain) in chains.iter() {
                let Some(tx) = chain.front() else { continue };
                let rate = self.calculate_fee_rate(tx);
                let better = match &best {
                    None => true,
                    Some((best_rate, best_hash, _)) => {
                        rate > *best_rate || (rate == *best_rate && tx.hash.0 > *best_hash)
                    }
                };
                if better {
                    best = Some((rate, tx.hash.0, *sender));
                }
            }
            let Some((_, _, sender)) = best else { break };
            let chain = chains.get_mut(&sender).expect("selected sender has a chain");
            if let Some(tx) = chain.pop_front() {
                result.push(tx);
            }
            if chain.is_empty() {
                chains.remove(&sender);
            }
        }

        // Selected transactions leave the pool.
        for tx in &result {
            self.remove_transaction(&tx.hash);
        }

        result
    }

    /// Remove a transaction from every index and update accounting.
    fn remove_transaction(&self, hash: &Hash) -> Option<Transaction> {
        let tx = self.by_hash.write().remove(hash)?;

        let fee_rate = self.calculate_fee_rate(&tx);
        {
            let mut by_fee_rate = self.by_fee_rate.write();
            if let Some(bucket) = by_fee_rate.get_mut(&fee_rate) {
                bucket.retain(|candidate| candidate.hash != *hash);
                if bucket.is_empty() {
                    by_fee_rate.remove(&fee_rate);
                }
            }
        }

        {
            let mut by_sender = self.by_sender.write();
            if let Some(nonces) = by_sender.get_mut(&tx.sender) {
                nonces.remove(&tx.nonce);
                if nonces.is_empty() {
                    by_sender.remove(&tx.sender);
                }
            }
        }

        {
            let mut pending_value = self.pending_value.write();
            if let Some(pending) = pending_value.get_mut(&tx.sender) {
                *pending = pending.saturating_sub(total_spend(&tx));
                if *pending == 0 {
                    pending_value.remove(&tx.sender);
                }
            }
        }

        let tx_size = bincode::serialize(&tx).unwrap_or_default().len();
        let mut total_size = self.total_size.write();
        *total_size = total_size.saturating_sub(tx_size);

        Some(tx)
    }

    /// Remove transactions that have been included in a committed block.
    pub fn remove_committed(&self, committed_txs: &[Hash]) {
        for hash in committed_txs {
            self.remove_transaction(hash);
        }
    }

    /// Get and drain transactions in the pending propagation queue.
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

    /// Validate the stateless part of admission control.
    fn validate_transaction(&self, tx: &Transaction) -> Result<(), MempoolError> {
        // 1. Signature validity.
        if !tx.verify_signature() {
            return Err(MempoolError::InvalidSignature);
        }

        let is_stake = matches!(tx.kind, TransactionKind::Stake { .. });

        // 2. Fee-limit sanity: a non-stake transaction must pay a fee.
        if tx.fee == 0 && !is_stake {
            return Err(MempoolError::ZeroFee);
        }

        // 3. Gas sanity for contract calls.
        if let TransactionKind::Call { gas_limit, .. } = &tx.kind {
            if *gas_limit > self.config.max_gas_limit {
                return Err(MempoolError::GasLimitTooHigh);
            }
        }

        // 4. Size sanity.
        let tx_size = bincode::serialize(tx).unwrap_or_default().len();
        if tx_size > self.config.max_tx_size {
            return Err(MempoolError::TransactionTooLarge(tx_size));
        }

        // 5. Fee-rate floor (stake with zero fee is exempt).
        if !(is_stake && tx.fee == 0) {
            let tx_size = tx_size.max(1) as u64;
            let fee_rate = tx.fee / tx_size;
            if fee_rate < self.config.min_fee_rate {
                return Err(MempoolError::FeeRateBelowMinimum {
                    rate: fee_rate,
                    minimum: self.config.min_fee_rate,
                });
            }
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
        loop {
            let oldest_lowest = {
                let by_fee_rate = self.by_fee_rate.read();
                by_fee_rate
                    .values()
                    .next()
                    .and_then(|bucket| bucket.front())
                    .map(|tx| tx.hash)
            };
            let total_size = *self.total_size.read();
            if total_size < self.config.max_mempool_size {
                return Ok(());
            }
            let Some(hash) = oldest_lowest else {
                return Ok(());
            };
            if let Some(evicted) = self.remove_transaction(&hash) {
                warn!(
                    "Evicted transaction {} from mempool (below size limit)",
                    evicted.hash
                );
            } else {
                return Ok(());
            }
        }
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
    #[error("Invalid transaction signature")]
    InvalidSignature,
    #[error("Transaction nonce {got} is lower than account nonce {expected}")]
    NonceTooLow { expected: u64, got: u64 },
    #[error("Insufficient balance: required {required}, available {available}")]
    InsufficientBalance { required: u128, available: u128 },
    #[error("Fee rate {rate} atoms/byte is below the minimum {minimum}")]
    FeeRateBelowMinimum { rate: u64, minimum: u64 },
    #[error(
        "Replacement underpriced for nonce {nonce}: existing rate {existing_rate}, new rate {new_rate}"
    )]
    ReplacementUnderpriced {
        nonce: u64,
        existing_rate: u64,
        new_rate: u64,
    },
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

#[cfg(test)]
mod tests {
    use super::*;
    use kvnc_types::{PublicKey, Signature, SigningKey};
    use std::{fs, path::PathBuf};

    const STRESS_TX_COUNT: usize = 10_000;

    fn fund(storage: &Storage, address: &Address, balance: u64, nonce: u64) {
        let write = storage.begin_write().expect("write txn");
        storage
            .state()
            .set_account(
                &write,
                address,
                &Account {
                    balance,
                    nonce,
                    ..Default::default()
                },
            )
            .expect("fund account");
        write.commit().expect("commit account");
    }

    fn transfer(
        signing_key: &SigningKey,
        public_key: &PublicKey,
        nonce: u64,
        amount: u64,
        fee: u64,
    ) -> Transaction {
        let mut tx = Transaction {
            sender: Address(public_key.0),
            nonce,
            kind: TransactionKind::Transfer {
                to: Address([2u8; 32]),
                amount,
            },
            fee,
            signature: Signature([0u8; 64]),
            hash: Hash::zero(),
        };
        tx.hash = tx.signing_hash();
        tx.signature = kvnc_crypto::sign(signing_key, tx.hash.as_ref());
        tx
    }

    fn call(
        signing_key: &SigningKey,
        public_key: &PublicKey,
        nonce: u64,
        gas_limit: u64,
        fee: u64,
    ) -> Transaction {
        let mut tx = Transaction {
            sender: Address(public_key.0),
            nonce,
            kind: TransactionKind::Call {
                contract: Address([3u8; 32]),
                method: "ping".to_string(),
                args: Vec::new(),
                gas_limit,
            },
            fee,
            signature: Signature([0u8; 64]),
            hash: Hash::zero(),
        };
        tx.hash = tx.signing_hash();
        tx.signature = kvnc_crypto::sign(signing_key, tx.hash.as_ref());
        tx
    }

    fn stress_transaction(
        signing_key: &SigningKey,
        sender: Address,
        index: usize,
    ) -> Transaction {
        let mut recipient = [0u8; 32];
        recipient[..8].copy_from_slice(&(index as u64).to_be_bytes());

        let mut tx = Transaction {
            sender,
            nonce: index as u64,
            kind: TransactionKind::Transfer {
                to: Address(recipient),
                amount: index as u64 + 1,
            },
            fee: 1,
            signature: Signature([0u8; 64]),
            hash: Hash::zero(),
        };
        let size = bincode::serialize(&tx).unwrap_or_default().len() as u64;
        // Keep the fee in ten deterministic buckets, all at or above the floor.
        tx.fee = size * (1 + (index % 10) as u64);
        tx.hash = tx.signing_hash();
        tx.signature = kvnc_crypto::sign(signing_key, tx.hash.as_ref());
        tx
    }

    fn test_storage_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("kvnc-mempool-{}-{}.redb", std::process::id(), name))
    }

    fn stress_storage_path() -> PathBuf {
        test_storage_path("stress")
    }

    #[test]
    fn accepts_valid_signed_transaction() {
        let path = test_storage_path("accept-valid");
        let _ = std::fs::remove_file(&path);
        let storage = Arc::new(Storage::new(&path).expect("create test storage"));
        let (signing_key, public_key) = kvnc_crypto::generate_keypair();
        let sender = Address(public_key.0);
        fund(&storage, &sender, 1_000_000, 0);

        let pool = Mempool::new(MempoolConfig::default(), storage.clone());
        let tx = transfer(&signing_key, &public_key, 0, 42, 1_000);
        pool.add_transaction(tx.clone()).expect("valid tx admitted");

        assert!(pool.contains(&tx.hash));
        assert_eq!(pool.len(), 1);
        assert_eq!(pool.stats().tx_count, 1);
    }

    #[test]
    fn rejects_invalid_signature() {
        let path = test_storage_path("bad-sig");
        let _ = std::fs::remove_file(&path);
        let storage = Arc::new(Storage::new(&path).expect("create test storage"));
        let (signing_key, public_key) = kvnc_crypto::generate_keypair();
        let sender = Address(public_key.0);
        fund(&storage, &sender, 1_000_000, 0);

        let pool = Mempool::new(MempoolConfig::default(), storage.clone());
        let mut tx = transfer(&signing_key, &public_key, 0, 42, 1_000);
        tx.signature = Signature([0u8; 64]);

        assert!(matches!(
            pool.add_transaction(tx),
            Err(MempoolError::InvalidSignature)
        ));
        assert!(pool.is_empty());
    }

    #[test]
    fn rejects_stale_nonce() {
        let path = test_storage_path("stale-nonce");
        let _ = std::fs::remove_file(&path);
        let storage = Arc::new(Storage::new(&path).expect("create test storage"));
        let (signing_key, public_key) = kvnc_crypto::generate_keypair();
        let sender = Address(public_key.0);
        fund(&storage, &sender, 1_000_000, 7);

        let pool = Mempool::new(MempoolConfig::default(), storage.clone());
        let tx = transfer(&signing_key, &public_key, 6, 42, 1_000);

        assert!(matches!(
            pool.add_transaction(tx),
            Err(MempoolError::NonceTooLow { expected: 7, got: 6 })
        ));
    }

    #[test]
    fn rejects_insufficient_balance() {
        let path = test_storage_path("insufficient");
        let _ = std::fs::remove_file(&path);
        let storage = Arc::new(Storage::new(&path).expect("create test storage"));
        let (signing_key, public_key) = kvnc_crypto::generate_keypair();
        let sender = Address(public_key.0);
        fund(&storage, &sender, 50, 0);

        let pool = Mempool::new(MempoolConfig::default(), storage.clone());
        let tx = transfer(&signing_key, &public_key, 0, 40, 1_000);

        assert!(matches!(
            pool.add_transaction(tx),
            Err(MempoolError::InsufficientBalance { .. })
        ));
    }

    #[test]
    fn rejects_zero_fee_transfer() {
        let path = test_storage_path("zero-fee");
        let _ = std::fs::remove_file(&path);
        let storage = Arc::new(Storage::new(&path).expect("create test storage"));
        let (signing_key, public_key) = kvnc_crypto::generate_keypair();
        let sender = Address(public_key.0);
        fund(&storage, &sender, 1_000_000, 0);

        let pool = Mempool::new(MempoolConfig::default(), storage.clone());
        let tx = transfer(&signing_key, &public_key, 0, 42, 0);

        assert!(matches!(
            pool.add_transaction(tx),
            Err(MempoolError::ZeroFee)
        ));
    }

    #[test]
    fn rejects_fee_rate_below_minimum() {
        let path = test_storage_path("low-fee-rate");
        let _ = std::fs::remove_file(&path);
        let storage = Arc::new(Storage::new(&path).expect("create test storage"));
        let (signing_key, public_key) = kvnc_crypto::generate_keypair();
        let sender = Address(public_key.0);
        fund(&storage, &sender, 1_000_000_000, 0);

        let config = MempoolConfig {
            min_fee_rate: 100_000,
            ..MempoolConfig::default()
        };
        let pool = Mempool::new(config, storage.clone());
        let tx = transfer(&signing_key, &public_key, 0, 42, 1);

        assert!(matches!(
            pool.add_transaction(tx),
            Err(MempoolError::FeeRateBelowMinimum { .. })
        ));
    }

    #[test]
    fn rejects_gas_limit_too_high() {
        let path = test_storage_path("gas-limit");
        let _ = std::fs::remove_file(&path);
        let storage = Arc::new(Storage::new(&path).expect("create test storage"));
        let (signing_key, public_key) = kvnc_crypto::generate_keypair();
        let sender = Address(public_key.0);
        fund(&storage, &sender, 1_000_000_000, 0);

        let pool = Mempool::new(MempoolConfig::default(), storage.clone());
        let tx = call(&signing_key, &public_key, 0, u64::MAX, 1_000);

        assert!(matches!(
            pool.add_transaction(tx),
            Err(MempoolError::GasLimitTooHigh)
        ));
    }

    #[test]
    fn orders_same_sender_transactions_by_nonce() {
        let path = test_storage_path("nonce-order");
        let _ = std::fs::remove_file(&path);
        let storage = Arc::new(Storage::new(&path).expect("create test storage"));
        let (signing_key, public_key) = kvnc_crypto::generate_keypair();
        let sender = Address(public_key.0);
        fund(&storage, &sender, 1_000_000_000, 0);

        let pool = Mempool::new(MempoolConfig::default(), storage.clone());

        // Insert out of order, with the highest fee on the earliest nonce.
        for nonce in [2u64, 0, 1] {
            let tx = transfer(&signing_key, &public_key, nonce, 10, 500 + nonce);
            pool.add_transaction(tx).expect("admit in-pool transaction");
        }

        let selected = pool.get_next_transactions(10);
        let nonces: Vec<u64> = selected.iter().map(|tx| tx.nonce).collect();
        assert_eq!(nonces, vec![0, 1, 2]);
        assert!(pool.is_empty(), "selected transactions leave the pool");
    }

    #[test]
    fn replaces_same_nonce_with_higher_fee_rate() {
        let path = test_storage_path("replace-higher");
        let _ = std::fs::remove_file(&path);
        let storage = Arc::new(Storage::new(&path).expect("create test storage"));
        let (signing_key, public_key) = kvnc_crypto::generate_keypair();
        let sender = Address(public_key.0);
        fund(&storage, &sender, 1_000_000_000, 0);

        let pool = Mempool::new(MempoolConfig::default(), storage.clone());
        let low = transfer(&signing_key, &public_key, 0, 10, 5_000);
        let high = transfer(&signing_key, &public_key, 0, 10, 50_000);

        pool.add_transaction(low.clone()).expect("admit low fee");
        pool.add_transaction(high.clone()).expect("replace with high fee");

        assert!(!pool.contains(&low.hash), "loser evicted");
        assert!(pool.contains(&high.hash), "winner retained");
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn rejects_underpriced_replacement() {
        let path = test_storage_path("replace-underpriced");
        let _ = std::fs::remove_file(&path);
        let storage = Arc::new(Storage::new(&path).expect("create test storage"));
        let (signing_key, public_key) = kvnc_crypto::generate_keypair();
        let sender = Address(public_key.0);
        fund(&storage, &sender, 1_000_000_000, 0);

        let pool = Mempool::new(MempoolConfig::default(), storage.clone());
        let high = transfer(&signing_key, &public_key, 0, 10, 50_000);
        let low = transfer(&signing_key, &public_key, 0, 10, 5_000);

        pool.add_transaction(high.clone()).expect("admit high fee");
        assert!(matches!(
            pool.add_transaction(low),
            Err(MempoolError::ReplacementUnderpriced { .. })
        ));

        assert!(pool.contains(&high.hash));
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn stress_admits_10k_pending_transactions() {
        let db_path = stress_storage_path();
        // A previous interrupted run may have left its process-specific DB behind.
        let _ = fs::remove_file(&db_path);
        let storage = Arc::new(Storage::new(&db_path).expect("create test storage"));

        let config = MempoolConfig {
            // 64 MiB is deliberately much larger than the serialized 10k set.
            max_mempool_size: 64 * 1024 * 1024,
            max_tx_size: 1024 * 1024,
            ..MempoolConfig::default()
        };
        let pool = Mempool::new(config.clone(), storage.clone());

        let (signing_key, public_key) = kvnc_crypto::generate_keypair();
        let sender = Address(public_key.0);
        fund(&storage, &sender, u64::MAX, 0);

        let transactions: Vec<_> = (0..STRESS_TX_COUNT)
            .map(|index| stress_transaction(&signing_key, sender, index))
            .collect();
        let expected_total_size: usize = transactions
            .iter()
            .map(|tx| {
                bincode::serialize(tx)
                    .expect("serialize expected transaction")
                    .len()
            })
            .sum();

        assert!(pool.is_empty());
        for tx in &transactions {
            pool.add_transaction(tx.clone())
                .expect("admit valid stress transaction");
        }

        assert_eq!(pool.len(), STRESS_TX_COUNT);
        assert!(!pool.is_empty());
        assert!(expected_total_size < config.max_mempool_size);

        for expected in &transactions {
            assert!(pool.contains(&expected.hash), "missing {}", expected.hash);
            let actual = pool
                .get(&expected.hash)
                .expect("lookup inserted transaction");
            assert_eq!(
                bincode::serialize(&actual).expect("serialize lookup"),
                bincode::serialize(expected).expect("serialize expected")
            );
        }

        let stats = pool.stats();
        assert_eq!(stats.tx_count, STRESS_TX_COUNT);
        assert_eq!(stats.total_size, expected_total_size);
        assert!(stats.total_size <= config.max_mempool_size);
        assert_eq!(
            stats
                .fee_rates
                .iter()
                .map(|(_, count)| count)
                .sum::<usize>(),
            STRESS_TX_COUNT
        );

        // Proposal selection returns the full set in per-sender nonce order.
        let next = pool.get_next_transactions(STRESS_TX_COUNT);
        assert_eq!(next.len(), STRESS_TX_COUNT);
        let returned: HashMap<_, _> = next.into_iter().map(|tx| (tx.hash, tx)).collect();
        assert_eq!(returned.len(), STRESS_TX_COUNT);
        for expected in &transactions {
            assert!(
                returned.contains_key(&expected.hash),
                "proposal queue returned every inserted hash"
            );
        }
        assert!(pool.is_empty(), "proposal drains the pending set");

        drop(pool);
        drop(storage);
        fs::remove_file(db_path).expect("remove test storage");
    }
}