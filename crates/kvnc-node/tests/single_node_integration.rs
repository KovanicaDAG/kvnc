//! Single-node end-to-end integration test.
//!
//! Drives one in-process consensus engine (no sockets, no async timing) from
//! block production → commit → execution and asserts:
//!
//! 1. a leader is committed for a small committee with fast rounds;
//! 2. the `CommittedSubDag` is delivered to the commit consumer;
//! 3. the committed sub-DAG ordering is bit-identical to the pure linearizer
//!    baseline when `use_mysticghost = false`;
//! 4. `use_mysticghost = true` still yields a valid, non-empty sub-DAG;
//! 5. rewards are credited (and persisted in the ledger) on commit via the
//!    real execution/staking reward path.
//!
//! The consensus `common` helpers live in the sibling crate's test tree and are
//! included here by path so there is a single source of truth for `MockDag`,
//! `MockBlockManager`, and the block/committee builders.

#[path = "../../kvnc-consensus/tests/common/mod.rs"]
mod common;

use common::{block_ref, committee, genesis, make_block, make_engine, MockDag};
use kvnc_consensus::engine::DagStoreTrait;
use kvnc_consensus::{ConsensusConfig, Linearizer};
use kvnc_execution::ExecutionContext;
use kvnc_staking::MIN_VALIDATOR_STAKE;
use kvnc_storage::Storage;
use kvnc_types::block::StatementBlock;
use kvnc_types::{Address, CommittedSubDag, PublicKey};
use std::sync::Arc;

/// Fast, deterministic config: rounds only matter for the async loop, which the
/// test avoids entirely by driving `process_block` / `process_vote` directly.
fn engine_config(use_mysticghost: bool) -> ConsensusConfig {
    ConsensusConfig {
        round_duration_ms: 1,
        lookahead_rounds: 3,
        max_pending_rounds: 100,
        use_mysticghost,
        prune_window_waves: 100,
    }
}

struct SingleNodeRun {
    subdag: CommittedSubDag,
    dag: Arc<MockDag>,
}

/// Build an in-process engine + committer, feed a canonical chain that reaches
/// a leader round, then cast exactly a quorum of votes. Returns the sub-DAG
/// delivered to the commit consumer.
fn drive_single_node(use_mysticghost: bool) -> SingleNodeRun {
    let (engine, dag, _manager) = make_engine(0, committee(4), engine_config(use_mysticghost));

    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    engine.set_commit_sender(sender);

    let g = genesis();
    engine.process_block(&g).expect("genesis accepted");

    // WAVE_LENGTH = 3, so round 3 is a leader slot; CommitteeInfo::leader(3) = 3
    // for a 4-member committee. Build the canonical causal chain.
    let b1 = make_block(1, 1, vec![block_ref(&g)], "b1");
    let b2 = make_block(2, 2, vec![block_ref(&b1)], "b2");
    let leader = make_block(3, 3, vec![block_ref(&b2)], "leader-3");
    for block in [&b1, &b2, &leader] {
        engine.process_block(block).expect("accepted block");
    }

    // Quorum for committee(4) is 2f+1 = 3. Votes are cast without any runtime or
    // sockets, so the commit is fully deterministic.
    for voter in [0u16, 1, 2] {
        engine
            .process_vote(3, voter, leader.digest)
            .expect("vote accepted");
    }

    let subdag = receiver
        .try_recv()
        .expect("committed sub-DAG delivered to the commit consumer");
    assert_eq!(subdag.leader.digest, leader.digest);
    assert_eq!(subdag.leader_round, 3);
    assert_eq!(subdag.leader_author, 3);

    SingleNodeRun { subdag, dag }
}

/// Execute a committed sub-DAG through the real reward path with four
/// authorities registered as validators. Returns the credited balance and the
/// payout address that received it.
fn execute_and_credit(subdag: &CommittedSubDag) -> (u64, Address) {
    let dir = tempfile::tempdir().expect("tempdir");
    let storage = Storage::new(dir.path().join("single-node.redb")).expect("storage");
    let mut ctx = ExecutionContext::new();

    let mut payouts = Vec::new();
    for i in 0..4u8 {
        let payout = Address([100 + i; 32]);
        ctx.staking
            .join_validator(Address([i + 1; 32]), MIN_VALIDATOR_STAKE, 0, Some(payout), Some(PublicKey([i + 1; 32])))
            .expect("genesis validator");
        payouts.push(payout);
    }

    let result = ctx
        .execute_committed_subdag(subdag, &storage)
        .expect("committed sub-DAG executes");
    let reward = result.reward.expect("reward credited on commit");
    assert!(reward.amount > 0, "reward amount must be positive");
    assert_eq!(
        reward.recipient,
        payouts[subdag.leader_author as usize],
        "reward must go to the leader's payout address"
    );

    // The credit must be visible in persistent storage.
    let txn = storage.begin_read().expect("read txn");
    let balance = storage
        .state()
        .get_account_or_default(&txn, &reward.recipient)
        .expect("account")
        .balance;
    assert_eq!(balance, reward.amount, "reward persisted to the ledger");

    (balance, reward.recipient)
}

/// Requirements 1–5 for the production (linearizer) path.
#[test]
fn single_node_commit_delivers_and_rewards_linearizer_path() {
    let run = drive_single_node(false);

    // (5) the delivered ordering is bit-identical to the pure linearizer
    //     baseline over the same causal history.
    let ancestor_hashes = run
        .dag
        .get_ancestors(&run.subdag.leader.digest, 0)
        .expect("ancestors");
    let history: Vec<StatementBlock> = ancestor_hashes
        .iter()
        .map(|h| run.dag.get_block(h).expect("ancestor present"))
        .collect();
    let baseline = Linearizer::new().linearize(run.subdag.leader.clone(), history);
    assert_eq!(
        run.subdag.blocks, baseline.blocks,
        "committed ordering must be bit-identical to the linearizer baseline"
    );

    // (2)+(3) sub-DAG delivered and non-trivial.
    assert!(run.subdag.blocks.len() >= 2, "history + leader expected");
    assert_eq!(
        run.subdag.blocks.last().map(|b| b.digest),
        Some(run.subdag.leader.digest),
        "leader is last in topological order"
    );

    // (4) rewards credited and persisted on commit.
    let (balance, recipient) = execute_and_credit(&run.subdag);
    assert_eq!(recipient, Address([103; 32]));
    assert!(balance > 0);
}

/// `use_mysticghost = true` still yields a valid, non-empty sub-DAG and the same
/// reward path credits the leader.
#[test]
fn single_node_commit_delivers_and_rewards_mysticghost_path() {
    let run = drive_single_node(true);

    assert!(
        !run.subdag.blocks.is_empty(),
        "MysticGhost commit must produce a non-empty sub-DAG"
    );
    assert!(
        run.subdag
            .blocks
            .iter()
            .any(|b| b.digest == run.subdag.leader.digest),
        "leader must be present in the MysticGhost sub-DAG"
    );
    assert_eq!(
        run.subdag.blocks.last().map(|b| b.digest),
        Some(run.subdag.leader.digest)
    );

    let (_balance, recipient) = execute_and_credit(&run.subdag);
    assert_eq!(
        recipient,
        Address([103; 32]),
        "leader 3 has payout addr 103"
    );
}