//! #13: return transactions from non-blue (red) blocks of a committed
//! sub-DAG to the mempool.
//!
//! The authoritative helper is Consensus's `kvnc_consensus::non_blue_transactions(dag, subdag)`,
//! which is not published yet. Until it is, [`LocalNonBlue`] is a local
//! stand-in behind the [`NonBlueSource`] adapter.
//!
//! SWAP POINT: when the helper lands, replace the body of
//! [`LocalNonBlue::non_blue_transactions`] with
//! `kvnc_consensus::non_blue_transactions(dag, subdag)` (or add a
//! `ConsensusNonBlue` impl and change the one constructor call in
//! `run_node`, see `NonBlueSource` usage in `main.rs`). Nothing else changes.

use std::{collections::HashSet, sync::Arc};

use kvnc_consensus::CommittedSubDag;
use kvnc_dag::DagStore;
use kvnc_mempool::{Mempool, ReinsertOutcome, ReinsertReport};
use kvnc_types::Transaction;
use tracing::{debug, info, warn};

/// Source of the transactions a committed sub-DAG returns to the pool.
pub trait NonBlueSource: Send + Sync {
    /// Transactions carried only by the sub-DAG's non-blue blocks
    /// (`subdag.non_blue`), not by any of its blue blocks.
    fn non_blue_transactions(&self, dag: &DagStore, subdag: &CommittedSubDag) -> Vec<Transaction>;
}

/// Local stand-in for `kvnc_consensus::non_blue_transactions`.
pub struct LocalNonBlue;

impl NonBlueSource for LocalNonBlue {
    fn non_blue_transactions(&self, dag: &DagStore, subdag: &CommittedSubDag) -> Vec<Transaction> {
        // SWAP POINT: `kvnc_consensus::non_blue_transactions(dag, subdag)`.
        let blue: HashSet<_> = subdag
            .blocks
            .iter()
            .flat_map(|b| b.transactions.iter().map(|t| t.hash))
            .collect();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for reference in &subdag.non_blue {
            match dag.get_block(&reference.digest) {
                Ok(block) => {
                    for tx in block.transactions {
                        if !blue.contains(&tx.hash) && seen.insert(tx.hash) {
                            out.push(tx);
                        }
                    }
                }
                Err(e) => {
                    warn!(digest = %reference.digest, error = %e, "non-blue block unavailable")
                }
            }
        }
        out
    }
}

/// Mempool handle for the execution worker: committed txs are pruned from
/// `mempool`; with `returned` set, non-blue txs are handed back after commit.
pub struct ExecPool {
    pub mempool: Arc<Mempool>,
    pub returned: Option<ReturnedTxs>,
}

/// What execution needs to hand returned transactions to the mempool.
#[derive(Clone)]
pub struct ReturnedTxs {
    pub dag: Arc<DagStore>,
    pub source: Arc<dyn NonBlueSource>,
}

impl ReturnedTxs {
    /// Call after the sub-DAG's state is durably committed. No gossip and no
    /// peer penalty: [`Mempool::reinsert_returned`] keeps them local.
    pub fn reinsert(&self, mempool: &Mempool, subdag: &CommittedSubDag) -> Vec<ReinsertReport> {
        let txs = self.source.non_blue_transactions(&self.dag, subdag);
        if txs.is_empty() {
            return Vec::new();
        }
        let report = mempool.reinsert_returned(txs);
        let accepted = report
            .iter()
            .filter(|r| r.outcome == ReinsertOutcome::Accepted)
            .count();
        for r in &report {
            debug!(hash = %r.hash, outcome = ?r.outcome, "returned non-blue transaction");
        }
        info!(
            round = subdag.leader_round,
            returned = report.len(),
            accepted,
            "reinserted non-blue transactions"
        );
        report
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use kvnc_storage::{state_store::Account, Storage};
    use kvnc_types::{
        block::{BlockReference, StatementBlock},
        crypto::Signature,
        Address, Hash, SigningContext, TransactionKind,
    };

    pub const CTX: SigningContext = SigningContext::new(kvnc_types::signing::chain_id::LOCAL);

    /// A funded sender and a validly signed transfer from it.
    pub fn funded_tx(storage: &Storage, nonce: u64) -> Transaction {
        let (sk, pk) = kvnc_crypto::generate_keypair();
        let sender = Address::from_public_key(&pk);
        let txn = storage.begin_write().unwrap();
        storage
            .state()
            .set_account(
                &txn,
                &sender,
                &Account {
                    balance: 1_000_000,
                    nonce,
                    code_hash: [0; 32],
                    code: Vec::new(),
                },
            )
            .unwrap();
        txn.commit().unwrap();
        let mut tx = Transaction {
            sender,
            nonce,
            kind: TransactionKind::Transfer {
                to: Address([5; 32]),
                amount: 1,
            },
            fee: 10_000,
            signature: Signature([0; 64]),
            hash: Hash::zero(),
        };
        let h = tx.signing_hash(&CTX);
        tx.signature = kvnc_crypto::sign(&sk, h.as_ref());
        tx.hash = h;
        tx
    }

    pub fn block(author: u32, round: u64, txs: Vec<Transaction>) -> StatementBlock {
        let digest = StatementBlock::compute_digest(author as _, round, &[], &txs);
        StatementBlock {
            author: author as _,
            round,
            parents: Vec::new(),
            transactions: txs,
            statements: Vec::new(),
            signature: Signature([0; 64]),
            digest,
            merkle_root: Default::default(),
        }
    }

    pub fn reference(b: &StatementBlock) -> BlockReference {
        BlockReference {
            author: b.author,
            round: b.round,
            digest: b.digest,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use kvnc_mempool::MempoolConfig;
    use kvnc_storage::Storage;

    #[test]
    fn local_adapter_returns_only_red_only_transactions_and_reinserts_them() {
        let dir = tempfile::tempdir().unwrap();
        let dag =
            Arc::new(DagStore::new(Storage::new(dir.path().join("dag.redb")).unwrap()).unwrap());
        let state = Arc::new(Storage::new(dir.path().join("state.redb")).unwrap());
        let shared = funded_tx(&state, 0); // in a blue and a red block
        let red_only = funded_tx(&state, 0);
        let red = block(
            1,
            2,
            vec![shared.clone(), red_only.clone(), red_only.clone()],
        );
        dag.put_block(&red).unwrap();
        let leader = block(0, 3, vec![shared.clone()]);
        let subdag = CommittedSubDag {
            blocks: vec![leader.clone()],
            leader,
            leader_round: 3,
            leader_author: 0,
            non_blue: vec![reference(&red)],
        };

        let txs = LocalNonBlue.non_blue_transactions(&dag, &subdag);
        assert_eq!(
            txs.iter().map(|t| t.hash).collect::<Vec<_>>(),
            vec![red_only.hash]
        );

        let mempool = Mempool::new(MempoolConfig::default(), state, CTX);
        let returned = ReturnedTxs {
            dag,
            source: Arc::new(LocalNonBlue),
        };
        let report = returned.reinsert(&mempool, &subdag);
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].outcome, ReinsertOutcome::Accepted);
        assert!(mempool.contains(&red_only.hash));
        assert!(mempool.get_pending_propagation().is_empty());
    }
}
