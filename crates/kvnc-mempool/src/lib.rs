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
        // Phase 16.5: conflict-aware selection — collect, filter contiguous nonce per sender, sort by fee
        let mut all: Vec<Transaction> = Vec::new();
        for txs in by_fee_rate.values_mut() {
            while let Some(tx) = txs.pop_front() {
                all.push(tx);
            }
        }
        let mut by_sender: std::collections::BTreeMap<String, Vec<Transaction>> =
            std::collections::BTreeMap::new();
        for tx in all {
            by_sender
                .entry(format!("{:?}", tx.sender))
                .or_default()
                .push(tx);
        }
        let mut result = Vec::new();
        for (_, mut txs) in by_sender {
            txs.sort_by_key(|tx| tx.nonce);
            let mut expected = txs.first().map(|t| t.nonce).unwrap_or(0);
            for tx in txs {
                if tx.nonce == expected {
                    result.push(tx);
                    expected += 1;
                }
            }
        }
        result.sort_by(|a, b| b.fee.cmp(&a.fee).then_with(|| a.sender.0.cmp(&b.sender.0)));
        result.truncate(max_txs);
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
        // 1. Verify signature
        if !tx.verify_signature() {
            return Err(MempoolError::InvalidSignature);
        }

        // 2. Load sender account from storage
        let txn = self.storage.begin_read()?;
        let state = self.storage.state();
        let account = state.get_account_or_default(&txn, &tx.sender)?;

        // 3. Check nonce (strict sequential for v1)
        if tx.nonce != account.nonce {
            return Err(MempoolError::InvalidNonce {
                expected: account.nonce,
                got: tx.nonce,
            });
        }

        // 4. Reject zero-fee transactions except stake/delegate (fee-free by design).
        if tx.fee == 0 && !matches!(tx.kind, TransactionKind::Stake { .. } | TransactionKind::Delegate { .. }) {
            return Err(MempoolError::ZeroFee);
        }

        // 5. Check balance sufficient for fee + immediate value
        let immediate_value = match &tx.kind {
            TransactionKind::Transfer { amount, .. } => *amount,
            TransactionKind::Stake { amount } => *amount,
            TransactionKind::Unstake { .. } => 0, // funds not returned until unbonding period ends
            TransactionKind::Delegate { amount, .. } => *amount,
            TransactionKind::ClaimRewards { .. } => 0,
            TransactionKind::Deploy { .. } => 0,
            TransactionKind::Call { .. } => 0,
        };
        let required = immediate_value.saturating_add(tx.fee);
        if account.balance < required {
            return Err(MempoolError::InsufficientBalance {
                balance: account.balance,
                required,
            });
        }

        // 6. Check gas limit (already partially done, but move here for completeness)
        if let TransactionKind::Call { gas_limit, .. } = &tx.kind {
            if *gas_limit > self.config.max_gas_limit {
                return Err(MempoolError::GasLimitTooHigh);
            }
        }

        // 7. Check transaction size (already done, but keep)
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
    #[error("Invalid signature")]
    InvalidSignature,
    #[error("Invalid nonce: expected {expected}, got {got}")]
    InvalidNonce { expected: u64, got: u64 },
    #[error("Transaction has zero fee")]
    ZeroFee,
    #[error("Insufficient balance: {balance} required {required}")]
    InsufficientBalance { balance: u64, required: u64 },
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
    #[error("State store error: {0}")]
    StateStore(#[from] kvnc_storage::StateStoreError),
    #[error("Serialization error: {0}")]
    Serialization(#[from] bincode::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    use kvnc_storage::state_store::Account;
    use kvnc_storage::Storage;
    use kvnc_types::crypto::{PublicKey, SigningKey};
    use kvnc_types::{Address, Signature};
    use std::{fs, path::PathBuf};

    const STRESS_TX_COUNT: usize = 10_000;

    fn build_signed_tx(
        sender: Address,
        nonce: u64,
        kind: TransactionKind,
        fee: u64,
        signing_key: &SigningKey,
    ) -> Transaction {
        let kind_for_tx = kind.clone();
        let mut tx = Transaction {
            sender,
            nonce,
            kind: kind_for_tx,
            fee,
            signature: Signature([0; 64]),
            hash: Hash::zero(),
        };

        let signing_hash = tx.signing_hash();
        let signature = kvnc_crypto::sign(signing_key, signing_hash.as_ref());

        tx.signature = signature;
        tx.hash = signing_hash;
        tx
    }

    fn stress_keypair() -> (SigningKey, PublicKey) {
        let (sk, vk) = kvnc_crypto::generate_keypair();
        let pk = vk;
        (sk, pk)
    }

    fn stress_transaction(
        index: usize,
        (signing_key, _public_key): (SigningKey, PublicKey),
    ) -> Transaction {
        let mut recipient = [0; 32];
        recipient[..8].copy_from_slice(&(index as u64).to_be_bytes());

        // Use a consistent sender address pattern (based on index for deterministic behavior)
        // All transactions will have nonce = 0 since they're added to mempool simultaneously
        let sender = Address::from_public_key(&_public_key);
        let kind = TransactionKind::Transfer {
            to: Address(recipient),
            amount: index as u64 + 1,
        };

        // Build signed transaction - nonce = 0 since all are submitted simultaneously
        let fee = 1;
        let mut tx = build_signed_tx(sender, 0, kind.clone(), fee, &signing_key);

        // Keep the fee rate in ten deterministic buckets while ensuring every
        // transaction has a non-zero fee and the same serialized size.
        let size = bincode::serialize(&tx)
            .expect("serialize stress transaction")
            .len();
        tx.fee = size as u64 * (1 + (index % 10) as u64);

        // Re-sign with updated fee since fee is part of the signing hash
        let signing_hash = tx.signing_hash();
        tx.signature = kvnc_crypto::sign(&signing_key, signing_hash.as_ref());
        tx.hash = signing_hash;
        tx
    }

    fn stress_storage_path() -> PathBuf {
        std::env::temp_dir().join(format!("kvnc-mempool-stress-{}.redb", std::process::id()))
    }

    #[test]
    fn stress_admits_10k_pending_transactions() {
        let db_path = stress_storage_path();
        // A previous interrupted run may have left its process-specific DB behind.
        let _ = fs::remove_file(&db_path);
        let storage = Arc::new(Storage::new(&db_path).expect("create test storage"));

        // Conflict-aware selection (Phase 16.5) only inlines a contiguous nonce
        // sequence per sender, so a 10k stress pool must use 10k distinct
        // senders (each at nonce 0) to exercise the full selection path.
        let keypairs: Vec<(SigningKey, PublicKey)> =
            (0..STRESS_TX_COUNT).map(|_| stress_keypair()).collect();

        // Set up one funded account per sender.
        let write_txn = storage.begin_write().unwrap();
        {
            let state = storage.state();
            for (_, public_key) in &keypairs {
                let sender = Address::from_public_key(public_key);
                let account = Account {
                    balance: 1_000_000,
                    nonce: 0,
                    code_hash: [0; 32],
                    code: Vec::new(),
                };
                state.set_account(&write_txn, &sender, &account).unwrap();
            }
        }
        write_txn.commit().unwrap();

        let config = MempoolConfig {
            // 64 MiB is deliberately much larger than the serialized 10k set.
            max_mempool_size: 64 * 1024 * 1024,
            max_tx_size: 1024 * 1024,
            ..MempoolConfig::default()
        };
        let pool = Mempool::new(config.clone(), storage.clone());

        let transactions: Vec<_> = (0..STRESS_TX_COUNT)
            .map(|i| stress_transaction(i, keypairs[i].clone()))
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

        // This consumes the fee-priority queue, so keep it after the lookup and
        // accounting assertions. The returned transactions must be the full set.
        let next = pool.get_next_transactions(STRESS_TX_COUNT);
        assert_eq!(next.len(), STRESS_TX_COUNT);
        let returned: HashMap<_, _> = next.into_iter().map(|tx| (tx.hash, tx)).collect();
        assert_eq!(returned.len(), STRESS_TX_COUNT);
        for expected in &transactions {
            let actual = returned
                .get(&expected.hash)
                .expect("proposal queue returned every inserted hash");
            assert_eq!(
                bincode::serialize(actual).expect("serialize queued transaction"),
                bincode::serialize(expected).expect("serialize expected")
            );
        }

        drop(pool);
        drop(storage);
        fs::remove_file(db_path).expect("remove test storage");
    }

    #[test]
    fn test_transaction_validation_edge_cases() {
        use kvnc_crypto::generate_keypair;
        use kvnc_types::transaction::TransactionKind;

        let db_path = std::env::temp_dir().join(format!(
            "kvnc-mempool-validation-test-{}.redb",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db_path);
        let storage = Arc::new(Storage::new(&db_path).expect("create test storage"));

        // Create a test account with known balance and nonce
        let (signing_key, public_key) = generate_keypair();
        let sender = Address::from_public_key(&public_key);

        // Set up account in storage with balance = 1000 and nonce = 5
        let txn = storage.begin_write().unwrap();
        {
            let state = storage.state();
            let account = Account {
                balance: 1000,
                nonce: 5,
                code_hash: [0; 32],
                code: Vec::new(),
            };
            state.set_account(&txn, &sender, &account).unwrap();
        }
        txn.commit().unwrap();

        let config = MempoolConfig::default();
        let pool = Mempool::new(config, storage.clone());

        // Test 1: Valid transaction should pass
        let valid_tx = build_signed_tx(
            sender,
            5, // matches account nonce
            TransactionKind::Transfer {
                to: Address([2u8; 32]),
                amount: 100,
            },
            10, // fee
            &signing_key,
        );

        assert!(
            pool.add_transaction(valid_tx.clone()).is_ok(),
            "Valid transaction accepted"
        );

        // Test 2: Transaction with invalid signature should be rejected
        // (distinct recipient so its hash is not a duplicate of `valid_tx`)
        let mut invalid_sig_tx = build_signed_tx(
            sender,
            5,
            TransactionKind::Transfer {
                to: Address([9u8; 32]),
                amount: 100,
            },
            10,
            &signing_key,
        );
        invalid_sig_tx.signature = Signature([0; 64]); // Invalid signature
        assert!(
            matches!(
                pool.add_transaction(invalid_sig_tx.clone()),
                Err(MempoolError::InvalidSignature)
            ),
            "Transaction with invalid signature should be rejected"
        );

        // Test 3: Transaction with wrong nonce (too low) should be rejected
        let low_nonce_tx = build_signed_tx(
            sender,
            4, // Less than account nonce (5)
            TransactionKind::Transfer {
                to: Address([2u8; 32]),
                amount: 100,
            },
            10,
            &signing_key,
        );
        assert!(
            matches!(
                pool.add_transaction(low_nonce_tx.clone()),
                Err(MempoolError::InvalidNonce {
                    expected: 5,
                    got: 4
                })
            ),
            "Transaction with nonce too low should be rejected"
        );

        // Test 4: Transaction with wrong nonce (too high) should be rejected
        let high_nonce_tx = build_signed_tx(
            sender,
            6, // Greater than account nonce (5)
            TransactionKind::Transfer {
                to: Address([2u8; 32]),
                amount: 100,
            },
            10,
            &signing_key,
        );
        assert!(
            matches!(
                pool.add_transaction(high_nonce_tx.clone()),
                Err(MempoolError::InvalidNonce {
                    expected: 5,
                    got: 6
                })
            ),
            "Transaction with nonce too high should be rejected"
        );

        // Test 5: Transaction with insufficient balance should be rejected
        let insufficient_balance_tx = build_signed_tx(
            sender,
            5, // matches account nonce
            TransactionKind::Transfer {
                to: Address([3u8; 32]),
                amount: 1000, // Would require 1000 + 10 fee = 1010, but balance is only 1000
            },
            10, // fee
            &signing_key,
        );
        assert!(
            matches!(
                pool.add_transaction(insufficient_balance_tx.clone()),
                Err(MempoolError::InsufficientBalance {
                    balance: 1000,
                    required: 1010
                })
            ),
            "Transaction with insufficient balance should be rejected"
        );

        // Test 6: Transaction with zero fee (non-stake) should be rejected
        let zero_fee_tx = build_signed_tx(
            sender,
            5, // matches account nonce
            TransactionKind::Transfer {
                to: Address([4u8; 32]),
                amount: 50,
            },
            0, // zero fee
            &signing_key,
        );
        assert!(
            matches!(
                pool.add_transaction(zero_fee_tx.clone()),
                Err(MempoolError::ZeroFee)
            ),
            "Transaction with zero fee (non-stake) should be rejected"
        );

        // Test 7: Stake transaction with zero fee should be allowed
        let stake_tx = build_signed_tx(
            sender,
            5, // matches account nonce
            TransactionKind::Stake { amount: 50 },
            0, // zero fee allowed for stake
            &signing_key,
        );
        assert!(
            pool.add_transaction(stake_tx).is_ok(),
            "Stake transaction with zero fee should be accepted"
        );

        // Clean up
        drop(pool);
        drop(storage);
        let _ = std::fs::remove_file(db_path);
    }
}
