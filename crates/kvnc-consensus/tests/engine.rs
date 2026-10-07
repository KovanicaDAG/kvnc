//! Tests for `ConsensusEngine`: construction/state, error propagation, and a
//! short live round loop (round advancement + proposing as leader).

mod common;

use common::{committee, make_engine};
use kvnc_consensus::{ConsensusConfig, ConsensusError};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Fast config so round-loop tests complete in milliseconds.
fn fast_config() -> ConsensusConfig {
    ConsensusConfig {
        round_duration_ms: 5,
        lookahead_rounds: 3,
        max_pending_rounds: 100,
    }
}

#[test]
fn test_engine_new_initial_state() {
    let (engine, _dag, manager) = make_engine(1, committee(4), ConsensusConfig::default());

    assert_eq!(engine.current_round(), 0);
    let state = engine.state();
    assert_eq!(state.our_authority, 1);
    assert_eq!(
        state.our_stake, 1,
        "committee(4) uses stake 1 per authority"
    );
    assert_eq!(state.current_round, 0);
    assert!(!state.is_leader, "no round advanced yet");
    assert!(!state.proposed);

    let c = engine.committee();
    assert_eq!(c.size(), 4);
    assert_eq!(c.quorum_threshold, 3);
    assert_eq!(c.stake_of(1), Some(1));
    assert!(
        !manager.read().signing_key_set(),
        "key is installed by start()"
    );
}

#[test]
fn test_engine_config_defaults() {
    let config = ConsensusConfig::default();
    assert_eq!(config.round_duration_ms, 2000);
    assert_eq!(config.lookahead_rounds, 3);
    assert_eq!(config.max_pending_rounds, 100);
}

#[test]
fn test_engine_process_block_ok_and_error_propagation() {
    let (engine, _dag, manager) = make_engine(0, committee(4), ConsensusConfig::default());
    let block = make_block_received(1);

    // Happy path: block is handed to the block manager, then consensus
    // registers it as a round-1 leader candidate and tries to commit.
    assert!(engine.process_block(&block).is_ok());
    assert_eq!(manager.read().process_calls(), 1);

    // Failure path: the block manager's error surfaces as ConsensusError.
    manager.read().set_fail_process(true);
    let err = engine
        .process_block(&block)
        .expect_err("must propagate error");
    assert!(
        matches!(err, ConsensusError::BlockManager(_)),
        "got {err:?}"
    );
    assert_eq!(
        manager.read().process_calls(),
        2,
        "the rejection still counted"
    );

    // ... and a successful call afterwards still works.
    manager.read().set_fail_process(false);
    assert!(engine.process_block(&block).is_ok());
    assert_eq!(manager.read().process_calls(), 3);
}

#[test]
fn test_engine_process_vote_is_ok_for_known_and_unknown_rounds() {
    let (engine, _dag, manager) = make_engine(0, committee(4), ConsensusConfig::default());
    let block = make_block_received(1);
    engine.process_block(&block).expect("process_block");

    // Vote on a registered round and on a round nobody has seen: both are
    // accepted (a vote for an unknown round is simply dropped by the committer).
    assert!(engine.process_vote(1, 0, block.digest).is_ok());
    assert!(engine.process_vote(99, 3, block.digest).is_ok());
    // The single vote alone is not a quorum, so nothing can be committed yet.
    assert_eq!(
        manager.read().process_calls(),
        1,
        "no extra block processing"
    );
}

#[test]
fn test_engine_stop_before_start_does_not_panic() {
    let (engine, _dag, manager) = make_engine(0, committee(4), fast_config());
    engine.stop();
    engine.stop(); // idempotent
    assert!(!manager.read().signing_key_set());
    assert_eq!(engine.current_round(), 0);
}

/// A single-validator committee (n = 1) is the leader every round: the round
/// loop must advance rounds, propose blocks each round, install the signing
/// key, and exit cleanly on `stop()`.
#[tokio::test]
async fn test_engine_round_loop_advances_proposes_and_stops() {
    let info = committee(1);
    assert_eq!(info.quorum_threshold, 1);
    let (engine, _dag, manager) = make_engine(0, info, fast_config());
    let manager_for_check = manager.clone();

    let engine = Arc::new(engine);
    let task_engine = Arc::clone(&engine);
    let handle = tokio::spawn(async move { task_engine.start().await });

    // Wait (generously) for the loop to install the key and propose a few
    // rounds — n = 1 means we are the leader of every round.
    let deadline = Instant::now() + Duration::from_secs(10);
    while manager_for_check.read().propose_calls() < 3
        || !manager_for_check.read().signing_key_set()
    {
        assert!(
            Instant::now() < deadline,
            "round loop made no progress: {} proposals, key installed = {}",
            manager_for_check.read().propose_calls(),
            manager_for_check.read().signing_key_set()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    engine.stop();
    let result = handle.await.expect("round-loop task panicked");
    assert!(
        result.is_ok(),
        "start() must return Ok after stop(): {result:?}"
    );
    assert!(manager_for_check.read().propose_calls() >= 3);
}

fn make_block_received(round: kvnc_types::Round) -> kvnc_types::block::StatementBlock {
    common::make_block(2, round, Vec::new(), &format!("received-{round}"))
}
