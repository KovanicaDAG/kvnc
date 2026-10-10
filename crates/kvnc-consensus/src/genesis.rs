//! State sync support: recognise the genesis committed sub-DAG.

use kvnc_types::block::StatementBlock;
use kvnc_types::hash::Hash;
use kvnc_types::{CommittedSubDag, GENESIS_ROUND};

/// Digest of the canonical genesis block (author 0, round 0, no parents,
/// no transactions, no statements). The same value the node writes at
/// genesis and `BlockManager::validate_canonical_genesis` enforces.
fn canonical_genesis_digest() -> Hash {
    StatementBlock::compute_digest(0, GENESIS_ROUND, &[], &[])
}

/// True iff `subdag` is the genesis sub-DAG: the committed sub-DAG that
/// delivers the canonical genesis block.
///
/// Pure function of the sub-DAG content (no DAG access, no I/O), so the answer
/// is the same live and on restart replay of a stored sub-DAG, even after the
/// DAG was pruned.
///
/// The check: `blocks ∪ non_blue ∪ {leader}` contains a reference with round
/// [`GENESIS_ROUND`] (0) **and** the canonical genesis digest
/// (`StatementBlock::compute_digest(0, 0, &[], &[])`). Checking the digest as
/// well as the round means a forged round-0 block can't qualify (and
/// `validate_block` already rejects any non-canonical round-0 block).
///
/// - If the first leader is skipped, the genesis block is delivered by the
///   first sub-DAG committed at a later round, and that one returns `true`.
/// - Red (`non_blue`) blocks count: a genesis block coloured red still marks
///   the genesis sub-DAG.
/// - Later sub-DAGs never contain genesis again (committed blocks are never
///   re-delivered), so they return `false`.
///
/// Used as the per-call `is_genesis_subdag` input of Execution's state sync
/// gate (`StartupSyncState::allows`).
pub fn is_genesis_subdag(subdag: &CommittedSubDag) -> bool {
    let genesis = canonical_genesis_digest();
    let is_genesis = |round, digest: &Hash| round == GENESIS_ROUND && *digest == genesis;
    is_genesis(subdag.leader.round, &subdag.leader.digest)
        || subdag.blocks.iter().any(|b| is_genesis(b.round, &b.digest))
        || subdag
            .non_blue
            .iter()
            .any(|r| is_genesis(r.round, &r.digest))
}
