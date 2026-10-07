//! Unit tests for the committer: committee thresholds, direct/indirect
//! decision rules, and the `UniversalCommitter` commit loop.
//!
//! Committee sizes follow the code's thresholds:
//! `quorum = floor(2 * total_stake / 3) + 1` (2f+1),
//! `validity = floor(total_stake / 3) + 1` (f+1).

mod common;

use common::{
    block_ref, committee, committee_with_stakes, drive_try_commit, genesis, leader_info,
    make_block, voter_stake, MockDag,
};
use kvnc_consensus::committer::BaseCommitter;
use kvnc_consensus::{
    AuthorityInfo, CommittedSubDag, CommitteeInfo, CommitteeInfoError, LeaderInfo, LeaderStatus,
    UniversalCommitter,
};
use kvnc_types::hash::Hash;
use kvnc_types::{Address, AuthorityIndex, PublicKey, Round};
use std::collections::HashMap;

fn authority(index: AuthorityIndex, stake: u64, key_byte: u8) -> AuthorityInfo {
    AuthorityInfo {
        index,
        stake,
        public_key: PublicKey([key_byte; 32]),
        address: Address([key_byte; 32]),
        network_address: format!("127.0.0.1:{}", key_byte),
    }
}

// ---------------------------------------------------------------------------
// CommitteeInfo: thresholds, quorum checks, leader selection
// ---------------------------------------------------------------------------

#[test]
fn test_committee_quorum_formula_various_sizes() {
    // Equal stake 1 per authority: quorum = floor(2n/3)+1, validity = floor(n/3)+1.
    for n in 1u16..=12 {
        let c = committee(n);
        let total = n as u64;
        assert_eq!(c.total_stake(), total, "n={n}");
        assert_eq!(c.quorum_threshold(), (total * 2) / 3 + 1, "n={n}");
        assert_eq!(c.validity_threshold(), total / 3 + 1, "n={n}");
        assert!(
            c.quorum_threshold() > total / 2,
            "n={n}: quorum must be > half"
        );
        assert!(c.quorum_threshold() <= total, "n={n}");
    }
}

#[test]
fn test_committee_rejects_empty_authority_set() {
    assert_eq!(
        CommitteeInfo::try_new(0, Vec::new()).unwrap_err(),
        CommitteeInfoError::EmptyCommittee
    );
}

#[test]
fn test_committee_rejects_zero_stake() {
    let err = CommitteeInfo::try_new(0, vec![authority(8, 0, 1)]).unwrap_err();
    assert_eq!(err, CommitteeInfoError::ZeroStake { index: 8 });
}

#[test]
fn test_committee_rejects_duplicate_authority_index() {
    let err = CommitteeInfo::try_new(0, vec![authority(3, 1, 1), authority(3, 1, 2)]).unwrap_err();
    assert_eq!(
        err,
        CommitteeInfoError::DuplicateAuthorityIndex { index: 3 }
    );
}

#[test]
fn test_committee_rejects_duplicate_public_key() {
    let err = CommitteeInfo::try_new(0, vec![authority(1, 1, 5), authority(2, 1, 5)]).unwrap_err();
    assert_eq!(err, CommitteeInfoError::DuplicatePublicKey);
}

#[test]
fn test_committee_rejects_total_stake_overflow() {
    let err =
        CommitteeInfo::try_new(0, vec![authority(1, u64::MAX, 1), authority(2, 1, 2)]).unwrap_err();
    assert_eq!(err, CommitteeInfoError::StakeOverflow);
}

#[test]
fn test_committee_accepts_near_max_stake_and_computes_thresholds_safely() {
    let c = CommitteeInfo::try_new(42, vec![authority(11, u64::MAX, 1)])
        .expect("u64::MAX total stake is valid");
    assert_eq!(c.epoch(), 42);
    assert_eq!(c.authorities().len(), 1);
    assert_eq!(c.total_stake(), u64::MAX);
    assert_eq!(c.quorum_threshold(), 12_297_829_382_473_034_411);
    assert_eq!(c.validity_threshold(), 6_148_914_691_236_517_206);
}

#[test]
fn test_committee_has_quorum_boundary_4_validators() {
    // 4 authorities, f = 1, 2f+1 = 3 votes.
    let c = committee(4);
    assert_eq!(c.quorum_threshold(), 3);
    assert_eq!(c.validity_threshold(), 2);

    let h = Hash::new("vote");
    let mk = |voters: &[AuthorityIndex]| -> HashMap<AuthorityIndex, Hash> {
        voters.iter().map(|&i| (i, h)).collect()
    };
    assert!(
        c.has_quorum(&mk(&[0, 1, 2]), Some(&h)),
        "3 of 4 must be quorum"
    );
    assert!(c.has_quorum(&mk(&[1, 2, 3]), Some(&h)));
    assert!(
        !c.has_quorum(&mk(&[0, 1]), Some(&h)),
        "2 of 4 (2f) must NOT be quorum"
    );
    assert!(!c.has_quorum(&mk(&[0]), Some(&h)));
    assert!(!c.has_quorum(&HashMap::new(), Some(&h)));
}

#[test]
fn test_committee_has_validity_boundary_4_validators() {
    let c = committee(4);
    let h = Hash::new("vote");
    let mk = |voters: &[AuthorityIndex]| -> HashMap<AuthorityIndex, Hash> {
        voters.iter().map(|&i| (i, h)).collect()
    };
    assert!(
        c.has_validity(&mk(&[0, 1]), Some(&h)),
        "2 of 4 = f+1 validity"
    );
    assert!(
        !c.has_validity(&mk(&[0]), Some(&h)),
        "1 of 4 below validity"
    );
}

#[test]
fn test_committee_stake_weighted_thresholds() {
    // Stakes [3, 1, 1, 1]: total 6, quorum = 5, validity = 3.
    let c = committee_with_stakes(&[3, 1, 1, 1]);
    assert_eq!(c.total_stake(), 6);
    assert_eq!(c.quorum_threshold(), 5);
    assert_eq!(c.validity_threshold(), 3);

    let h = Hash::new("vote");
    let mk = |voters: &[AuthorityIndex]| -> HashMap<AuthorityIndex, Hash> {
        voters.iter().map(|&i| (i, h)).collect()
    };
    assert!(c.has_quorum(&mk(&[0, 1, 2]), Some(&h)), "3+1+1 = 5 >= 5");
    assert!(!c.has_quorum(&mk(&[0, 1]), Some(&h)), "3+1 = 4 < 5");
    assert!(c.has_quorum(&mk(&[0, 1, 3]), Some(&h)));
    assert!(!c.has_quorum(&mk(&[1, 2, 3]), Some(&h)), "1+1+1 = 3 < 5");
    assert_eq!(c.stake_of(0), Some(3));
    assert_eq!(c.stake_of(9), None);
}

#[test]
fn test_committee_leader_is_round_robin_and_deterministic() {
    let c = committee(4);
    // Leader is selected by round-robin over authority indices.
    for round in 0..16u64 {
        let expected = (round as usize % 4) as AuthorityIndex;
        assert_eq!(c.leader(round), expected, "round {round}");
    }
    // Deterministic: same round -> same leader on repeated calls.
    for round in 0..32u64 {
        assert_eq!(c.leader(round), c.leader(round));
    }
}

#[test]
fn test_votes_from_unknown_authorities_ignored() {
    let c = committee(4);
    let h = Hash::new("vote");
    // 2 known + 2 unknown voters: only the 2 known count -> no quorum.
    let votes: HashMap<AuthorityIndex, Hash> =
        [(0u16, h), (1, h), (99, h), (100, h)].into_iter().collect();
    assert_eq!(voter_stake(&c, &votes), 2);
    assert!(!c.has_quorum(&votes, Some(&h)));
    // 3 known + unknowns: quorum holds.
    let votes: HashMap<AuthorityIndex, Hash> =
        [(0u16, h), (1, h), (2, h), (99, h)].into_iter().collect();
    assert_eq!(voter_stake(&c, &votes), 3);
    assert!(c.has_quorum(&votes, Some(&h)));
}

// ---------------------------------------------------------------------------
// BaseCommitter::try_direct_decide
// ---------------------------------------------------------------------------

#[test]
fn test_direct_decide_2f_plus_1_votes_commits() {
    let c = committee(4);
    let base = BaseCommitter::new(c.clone());
    let dag = MockDag::new();
    let block = make_block(1, 3, vec![block_ref(&genesis())], "leader-3");
    let votes: Vec<(AuthorityIndex, Hash)> =
        vec![(0, block.digest), (1, block.digest), (2, block.digest)];

    let status = base.try_direct_decide(
        &dag,
        &leader_info(3, 1, Some(block.digest), LeaderStatus::Undecided, &votes),
    );
    assert_eq!(
        status,
        LeaderStatus::Commit,
        "3 of 4 votes = 2f+1 => commit"
    );
    // Sanity: the exact stake that triggered it.
    let li = leader_info(3, 1, Some(block.digest), LeaderStatus::Undecided, &votes);
    assert_eq!(voter_stake(&c, &li.votes), c.quorum_threshold());
}

#[test]
fn test_direct_decide_2f_votes_does_not_commit() {
    let base = BaseCommitter::new(committee(4));
    let dag = MockDag::new();
    let block = make_block(1, 3, vec![block_ref(&genesis())], "leader-3");
    // 2 of 4 = 2f votes: must NOT commit (below quorum).
    let votes = vec![(0, block.digest), (1, block.digest)];
    let status = base.try_direct_decide(
        &dag,
        &leader_info(3, 1, Some(block.digest), LeaderStatus::Undecided, &votes),
    );
    assert_eq!(status, LeaderStatus::Undecided);
    assert_ne!(status, LeaderStatus::Commit);
    assert_ne!(status, LeaderStatus::Skip, "direct rule never skips");
}

/// Exhaustive: for a 4-validator committee, every subset of voters either
/// commits (exactly 2f+1 = 3 or more voters) or stays undecided.
#[test]
fn test_direct_decide_exhaustive_vote_subsets_4_validators() {
    let base = BaseCommitter::new(committee(4));
    let dag = MockDag::new();
    let block = make_block(2, 6, vec![block_ref(&genesis())], "leader-6");
    let h = block.digest;

    for mask in 0u32..(1 << 4) {
        let votes: Vec<(AuthorityIndex, Hash)> = (0..4u16)
            .filter(|i| mask & (1 << i) != 0)
            .map(|i| (i, h))
            .collect();
        let status = base.try_direct_decide(
            &dag,
            &leader_info(6, 2, Some(h), LeaderStatus::Undecided, &votes),
        );
        let expected = if votes.len() >= 3 {
            LeaderStatus::Commit
        } else {
            LeaderStatus::Undecided
        };
        assert_eq!(
            status,
            expected,
            "subset {mask:04b} ({} votes) -> {status:?}",
            votes.len()
        );
    }
}

/// Round 0 (genesis round) is exempt from direct commit even with full quorum.
#[test]
fn test_direct_decide_round_zero_never_commits() {
    let base = BaseCommitter::new(committee(4));
    let dag = MockDag::new();
    let g = genesis();
    let votes: Vec<(AuthorityIndex, Hash)> =
        vec![(0, g.digest), (1, g.digest), (2, g.digest), (3, g.digest)];
    let status = base.try_direct_decide(
        &dag,
        &leader_info(0, 0, Some(g.digest), LeaderStatus::Undecided, &votes),
    );
    assert_eq!(status, LeaderStatus::Undecided, "round 0 is exempt");
}

#[test]
fn test_direct_decide_no_votes_is_undecided() {
    let base = BaseCommitter::new(committee(4));
    let dag = MockDag::new();
    let block = make_block(0, 3, vec![block_ref(&genesis())], "leader-3");
    let status = base.try_direct_decide(
        &dag,
        &leader_info(3, 0, Some(block.digest), LeaderStatus::Undecided, &[]),
    );
    assert_eq!(status, LeaderStatus::Undecided);
}

/// Validity without quorum must not decide anything on the direct path.
#[test]
fn test_direct_decide_never_returns_skip() {
    let base = BaseCommitter::new(committee(4));
    let dag = MockDag::new();
    let block = make_block(0, 9, vec![block_ref(&genesis())], "leader-9");
    // 2 votes = validity threshold (f+1) but below quorum (2f+1).
    for voters in [vec![0u16, 1], vec![2, 3], vec![0, 3]] {
        let votes: Vec<(AuthorityIndex, Hash)> =
            voters.into_iter().map(|v| (v, block.digest)).collect();
        let status = base.try_direct_decide(
            &dag,
            &leader_info(9, 0, Some(block.digest), LeaderStatus::Undecided, &votes),
        );
        assert_eq!(status, LeaderStatus::Undecided);
    }
}

/// Stake-weighted quorum: exactly-quorum stake commits, one stake less does not.
#[test]
fn test_direct_decide_stake_weighted_boundary() {
    let c = committee_with_stakes(&[3, 1, 1, 1]); // total 6, quorum 5
    let base = BaseCommitter::new(c);
    let dag = MockDag::new();
    let block = make_block(0, 3, vec![block_ref(&genesis())], "leader-3");

    // Exactly quorum (3+1+1 = 5): commits.
    let votes = vec![(0, block.digest), (1, block.digest), (2, block.digest)];
    assert_eq!(
        base.try_direct_decide(
            &dag,
            &leader_info(3, 0, Some(block.digest), LeaderStatus::Undecided, &votes)
        ),
        LeaderStatus::Commit
    );
    // One below (3+1 = 4): undecided.
    let votes = vec![(0, block.digest), (1, block.digest)];
    assert_eq!(
        base.try_direct_decide(
            &dag,
            &leader_info(3, 0, Some(block.digest), LeaderStatus::Undecided, &votes)
        ),
        LeaderStatus::Undecided
    );
    // Many small voters below quorum (1+1+1 = 3): undecided.
    let votes = vec![(1, block.digest), (2, block.digest), (3, block.digest)];
    assert_eq!(
        base.try_direct_decide(
            &dag,
            &leader_info(3, 0, Some(block.digest), LeaderStatus::Undecided, &votes)
        ),
        LeaderStatus::Undecided
    );
}

// ---------------------------------------------------------------------------
// BaseCommitter::try_indirect_decide
// ---------------------------------------------------------------------------

/// Fixture: round-3 block `P3` and round-5 block `P5` (child of P3), genesis
/// at round 0. Returns (base, dag, P3, P5).
fn indirect_fixture(
    earlier_tag: &str,
) -> (
    BaseCommitter,
    MockDag,
    kvnc_types::block::StatementBlock,
    kvnc_types::block::StatementBlock,
) {
    let g = genesis();
    let p3 = make_block(0, 3, vec![block_ref(&g)], earlier_tag);
    let p5 = make_block(1, 5, vec![block_ref(&p3)], "p5");
    let dag = MockDag::with_blocks([g, p3.clone(), p5.clone()]);
    (BaseCommitter::new(committee(4)), dag, p3, p5)
}

fn decided_map(entries: &[(Round, LeaderInfo)]) -> HashMap<Round, LeaderInfo> {
    entries.iter().cloned().collect()
}

/// Later leader in the same wave is committed AND causally connected =>
/// earlier leader is indirectly committed.
#[test]
fn test_indirect_decide_commit_when_connected() {
    let (base, dag, p3, p5) = indirect_fixture("p3");
    let decided = decided_map(&[(
        5,
        leader_info(5, 1, Some(p5.digest), LeaderStatus::Commit, &[]),
    )]);

    let status = base.try_indirect_decide(
        &dag,
        &leader_info(3, 0, Some(p3.digest), LeaderStatus::Undecided, &[]),
        &decided,
    );
    assert_eq!(status, LeaderStatus::Commit);
}

/// Later leader in the same wave is committed but the earlier block is NOT in
/// its causal history => earlier leader must be skipped.
#[test]
fn test_indirect_decide_skip_when_not_connected() {
    // Parallel round-3 block (same round, different block) — not an ancestor
    // of P5.
    let (base, dag, _p3, p5) = indirect_fixture("p3");
    let unrelated = make_block(2, 3, vec![block_ref(&genesis())], "p3-parallel");
    // The unrelated block must exist in the store for the ancestry query.
    dag.put(unrelated.clone());

    let decided = decided_map(&[(
        5,
        leader_info(5, 1, Some(p5.digest), LeaderStatus::Commit, &[]),
    )]);

    let status = base.try_indirect_decide(
        &dag,
        &leader_info(3, 2, Some(unrelated.digest), LeaderStatus::Undecided, &[]),
        &decided,
    );
    assert_eq!(status, LeaderStatus::Skip);
}

/// Only rounds *within the same wave* are consulted: a committed leader in the
/// next wave does not decide this one.
#[test]
fn test_indirect_decide_next_wave_not_considered() {
    let (base, dag, p3, _p5) = indirect_fixture("p3");
    let g = genesis();
    let p6 = make_block(1, 6, vec![block_ref(&g)], "p6-next-wave");
    let decided = decided_map(&[(
        6,
        leader_info(6, 1, Some(p6.digest), LeaderStatus::Commit, &[]),
    )]);

    let status = base.try_indirect_decide(
        &dag,
        &leader_info(3, 0, Some(p3.digest), LeaderStatus::Undecided, &[]),
        &decided,
    );
    assert_eq!(
        status,
        LeaderStatus::Undecided,
        "round 6 is in wave 2, round 3 is in wave 1"
    );
}

/// A later leader that was decided as Skip does not decide this round; the
/// decision stays Undecided until a *committed* later leader appears.
#[test]
fn test_indirect_decide_skipped_later_leader_is_ignored() {
    let (base, dag, p3, p5) = indirect_fixture("p3");
    let decided = decided_map(&[(
        5,
        leader_info(5, 1, Some(p5.digest), LeaderStatus::Skip, &[]),
    )]);

    let status = base.try_indirect_decide(
        &dag,
        &leader_info(3, 0, Some(p3.digest), LeaderStatus::Undecided, &[]),
        &decided,
    );
    assert_eq!(status, LeaderStatus::Undecided);
}

/// No decided leaders at all => Undecided.
#[test]
fn test_indirect_decide_no_decided_leaders_is_undecided() {
    let (base, dag, p3, _p5) = indirect_fixture("p3");
    let status = base.try_indirect_decide(
        &dag,
        &leader_info(3, 0, Some(p3.digest), LeaderStatus::Undecided, &[]),
        &HashMap::new(),
    );
    assert_eq!(status, LeaderStatus::Undecided);
}

/// A committed later leader without a block hash cannot be path-checked, so
/// the earlier leader is skipped (nothing to connect through).
#[test]
fn test_indirect_decide_committed_later_leader_without_block_is_skip() {
    let (base, dag, p3, _p5) = indirect_fixture("p3");
    let decided = decided_map(&[(5, leader_info(5, 1, None, LeaderStatus::Commit, &[]))]);

    let status = base.try_indirect_decide(
        &dag,
        &leader_info(3, 0, Some(p3.digest), LeaderStatus::Undecided, &[]),
        &decided,
    );
    assert_eq!(status, LeaderStatus::Skip);
}

/// An earlier leader with no block of its own is skipped when the wave moves on.
#[test]
fn test_indirect_decide_blockless_earlier_leader_is_skip() {
    let (base, dag, _p3, p5) = indirect_fixture("p3");
    let decided = decided_map(&[(
        5,
        leader_info(5, 1, Some(p5.digest), LeaderStatus::Commit, &[]),
    )]);

    let status =
        base.try_direct_decide(&dag, &leader_info(3, 0, None, LeaderStatus::Undecided, &[]));
    assert_eq!(status, LeaderStatus::Undecided, "direct rule cannot decide");

    let status = base.try_indirect_decide(
        &dag,
        &leader_info(3, 0, None, LeaderStatus::Undecided, &[]),
        &decided,
    );
    assert_eq!(status, LeaderStatus::Skip);
}

/// A round-4 leader (offset 1) only looks at round 5 (rest of its wave);
/// round 6 belongs to the next wave and must be ignored.
#[test]
fn test_indirect_decide_wave_window_bounds() {
    let (base, dag, _p3, p5) = indirect_fixture("p3");
    let g = genesis();
    let p4 = make_block(3, 4, vec![block_ref(&g)], "p4-parallel");
    dag.put(p4.clone());
    let p6 = make_block(1, 6, vec![block_ref(&g)], "p6");
    let undecided_leader = leader_info(4, 3, Some(p4.digest), LeaderStatus::Undecided, &[]);

    // Only round 6 decided => out of window for round 4 (wave 1 = rounds 3..5).
    let decided = decided_map(&[(
        6,
        leader_info(6, 1, Some(p6.digest), LeaderStatus::Commit, &[]),
    )]);
    assert_eq!(
        base.try_indirect_decide(&dag, &undecided_leader, &decided),
        LeaderStatus::Undecided
    );

    // Round 5 decided Commit and in window for round 4 => P4 is not in P5's
    // causal history, so the window decides Skip (a decision, not undecided).
    let decided = decided_map(&[(
        5,
        leader_info(5, 1, Some(p5.digest), LeaderStatus::Commit, &[]),
    )]);
    assert_eq!(
        base.try_indirect_decide(&dag, &undecided_leader, &decided),
        LeaderStatus::Skip
    );
}

// ---------------------------------------------------------------------------
// UniversalCommitter: votes, bookkeeping, try_commit loop
// ---------------------------------------------------------------------------

#[test]
fn test_add_vote_below_quorum_stays_undecided() {
    let committer = UniversalCommitter::new(committee(4));
    let block = make_block(1, 3, vec![block_ref(&genesis())], "leader-3");
    committer.update_leader(leader_info(
        3,
        1,
        Some(block.digest),
        LeaderStatus::Undecided,
        &[],
    ));

    assert!(!committer.add_vote(3, 0, block.digest));
    assert!(
        !committer.add_vote(3, 1, block.digest),
        "2 of 4 is 2f, not quorum"
    );
    let leader = committer.get_leader(3).expect("leader registered");
    assert_eq!(leader.status, LeaderStatus::Undecided);
    assert_eq!(leader.votes.len(), 2);
}

#[test]
fn test_add_vote_quorum_marks_commit_and_returns_true() {
    let committer = UniversalCommitter::new(committee(4));
    let block = make_block(1, 3, vec![block_ref(&genesis())], "leader-3");
    committer.update_leader(leader_info(
        3,
        1,
        Some(block.digest),
        LeaderStatus::Undecided,
        &[],
    ));

    assert!(!committer.add_vote(3, 0, block.digest));
    assert!(!committer.add_vote(3, 1, block.digest));
    assert!(
        committer.add_vote(3, 2, block.digest),
        "3rd of 4 votes crosses the 2f+1 quorum"
    );
    let leader = committer.get_leader(3).expect("leader registered");
    assert_eq!(leader.status, LeaderStatus::Commit);

    // Adding a vote for an unknown round must not panic and reports no quorum.
    assert!(!committer.add_vote(42, 0, block.digest));
}

#[test]
fn test_update_get_leader_roundtrip() {
    let committer = UniversalCommitter::new(committee(4));
    let block = make_block(2, 6, vec![block_ref(&genesis())], "leader-6");
    let info = leader_info(
        6,
        2,
        Some(block.digest),
        LeaderStatus::Undecided,
        &[(0, block.digest)],
    );
    committer.update_leader(info);

    let got = committer.get_leader(6).expect("round 6 exists");
    assert_eq!(got.round, 6);
    assert_eq!(got.author, 2);
    assert_eq!(got.block_hash, Some(block.digest));
    assert_eq!(got.votes.len(), 1);
    assert!(committer.get_leader(7).is_none());
    assert_eq!(committer.get_all_leaders().len(), 1);
}

#[test]
fn test_cleanup_old_leaders_retains_cutoff_and_above() {
    let committer = UniversalCommitter::new(committee(4));
    for round in 1..=10u64 {
        committer.update_leader(leader_info(
            round,
            round as AuthorityIndex % 4,
            None,
            LeaderStatus::Undecided,
            &[],
        ));
    }
    committer.cleanup_old_leaders(5);
    let remaining = committer.get_all_leaders();
    assert_eq!(remaining.len(), 6, "rounds 5..=10 must survive");
    for round in 5..=10u64 {
        assert!(remaining.contains_key(&round), "round {round} pruned");
    }
    for round in 1..5u64 {
        assert!(
            !remaining.contains_key(&round),
            "round {round} should be gone"
        );
    }
    committer.cleanup_old_leaders(5);
    assert_eq!(committer.last_decided_round(), 0);
    assert!(committer.get_all_decided_leaders().is_empty());
}

#[test]
fn test_try_commit_with_no_leaders_returns_none() {
    let committer = UniversalCommitter::new(committee(4));
    let dag = MockDag::new();
    assert!(committer.try_commit(&dag).is_none());
    assert_eq!(committer.last_decided_round(), 0);
    assert!(committer.get_all_decided_leaders().is_empty());
}

#[test]
fn durable_commit_cannot_publish_an_unregistered_subdag() {
    let fabricated = make_block(1, 3, vec![block_ref(&genesis())], "fabricated");
    let dag = MockDag::with_blocks([fabricated]);
    let committer = UniversalCommitter::new(committee(4));

    assert!(committer
        .try_commit_and_mark_durable(&dag)
        .expect("no eligible commit")
        .is_none());
    assert_eq!(committer.last_decided_round(), 0);
    assert!(committer.get_all_decided_leaders().is_empty());
    assert!(dag.decisions().is_empty());
}

#[test]
fn durable_commit_rejects_registered_round_mismatch_with_leader_block() {
    let g = genesis();
    let actual_leader = make_block(2, 6, vec![block_ref(&g)], "wrong-round-leader");
    let dag = MockDag::with_blocks([g, actual_leader.clone()]);
    let committer = UniversalCommitter::new(committee(4));
    // The author matches, but quorum is registered for round 3 while the
    // matching digest resolves to a round-6 block.
    committer.update_leader(leader_info(
        3,
        2,
        Some(actual_leader.digest),
        LeaderStatus::Undecided,
        &[],
    ));
    for voter in [0, 1, 2] {
        committer.add_vote(3, voter, actual_leader.digest);
    }

    assert!(committer
        .try_commit_and_mark_durable(&dag)
        .expect("metadata mismatch is rejected without a storage error")
        .is_none());
    assert!(dag.decisions().is_empty());
    assert_eq!(committer.last_decided_round(), 0);
    assert!(committer.get_all_decided_leaders().is_empty());
}

#[test]
fn durable_commit_rejects_registered_author_mismatch_with_leader_block() {
    let g = genesis();
    let actual_leader = make_block(2, 3, vec![block_ref(&g)], "wrong-author-leader");
    let dag = MockDag::with_blocks([g, actual_leader.clone()]);
    let committer = UniversalCommitter::new(committee(4));
    // The round matches, but quorum is registered for author 1 while the
    // matching digest resolves to an author-2 block.
    committer.update_leader(leader_info(
        3,
        1,
        Some(actual_leader.digest),
        LeaderStatus::Undecided,
        &[],
    ));
    for voter in [0, 1, 2] {
        committer.add_vote(3, voter, actual_leader.digest);
    }

    assert!(committer
        .try_commit_and_mark_durable(&dag)
        .expect("metadata mismatch is rejected without a storage error")
        .is_none());
    assert!(dag.decisions().is_empty());
    assert_eq!(committer.last_decided_round(), 0);
    assert!(committer.get_all_decided_leaders().is_empty());
}

#[test]
fn durable_commit_rejects_digest_alias_for_matching_round_and_author() {
    let g = genesis();
    let actual_leader = make_block(2, 3, vec![block_ref(&g)], "aliased-leader");
    let alias_digest = Hash::new("kvnc-test/leader-digest-alias");
    let dag = MockDag::with_blocks([g, actual_leader.clone()]);
    dag.alias_digest(alias_digest, actual_leader.digest);

    let committer = UniversalCommitter::new(committee(4));
    committer.update_leader(leader_info(
        3,
        2,
        Some(alias_digest),
        LeaderStatus::Undecided,
        &[],
    ));
    for voter in [0, 1, 2] {
        committer.add_vote(3, voter, alias_digest);
    }

    assert!(committer
        .try_commit_and_mark_durable(&dag)
        .expect("digest mismatch is rejected without a storage error")
        .is_none());
    assert!(dag.decisions().is_empty());
    assert!(committer.get_all_decided_leaders().is_empty());
    assert_eq!(committer.last_decided_round(), 0);
}

#[test]
fn durable_commit_persists_before_publication_and_retries_failures() {
    let g = genesis();
    let leader = make_block(1, 3, vec![block_ref(&g)], "durable-leader");
    let dag = MockDag::with_blocks([g, leader.clone()]);
    let committer = std::sync::Arc::new(UniversalCommitter::new(committee(4)));
    committer.update_leader(leader_info(
        3,
        1,
        Some(leader.digest),
        LeaderStatus::Undecided,
        &[],
    ));
    for voter in [0, 1, 2] {
        committer.add_vote(3, voter, leader.digest);
    }

    dag.set_fail_next_decision_mark();
    assert!(committer.try_commit_and_mark_durable(&dag).is_err());
    assert_eq!(committer.last_decided_round(), 0);
    assert!(committer.get_all_decided_leaders().is_empty());
    assert!(dag.decisions().is_empty());

    let observer_committer = std::sync::Arc::clone(&committer);
    dag.set_decision_mark_observer(move || {
        assert_eq!(observer_committer.last_decided_round(), 0);
        assert!(observer_committer.get_all_decided_leaders().is_empty());
    });
    let committed = committer
        .try_commit_and_mark_durable(&dag)
        .expect("decision mark retry succeeds")
        .expect("eligible leader remains retryable");
    assert_eq!(committed.leader_round, 3);
    assert_eq!(dag.decisions(), vec![(3, leader.digest)]);
    assert_eq!(committer.last_decided_round(), 3);
    assert_eq!(
        committer.get_all_decided_leaders()[&3].status,
        LeaderStatus::Commit
    );

    assert!(committer
        .try_commit_and_mark_durable(&dag)
        .expect("repeated drive is harmless")
        .is_none());
    assert_eq!(dag.decisions(), vec![(3, leader.digest)]);
}

/// Full direct-commit path: quorum votes + leader block in the store produce a
/// committed sub-DAG whose blocks are in topological order (genesis first,
/// leader last) and the round is marked decided exactly once.
#[test]
fn test_try_commit_direct_commit_end_to_end() {
    let g = genesis();
    let leader_block = make_block(1, 3, vec![block_ref(&g)], "leader-3");
    let dag = MockDag::with_blocks([g, leader_block.clone()]);

    let committer = UniversalCommitter::new(committee(4));
    committer.update_leader(leader_info(
        3,
        1,
        Some(leader_block.digest),
        LeaderStatus::Undecided,
        &[],
    ));
    for voter in [0u16, 1, 2] {
        committer.add_vote(3, voter, leader_block.digest);
    }

    let subdag = committer
        .try_commit_and_mark_durable(&dag)
        .expect("persist commit")
        .expect("quorum + present block must commit");
    assert_eq!(subdag.leader_round, 3);
    assert_eq!(subdag.leader_author, 1);
    assert_eq!(subdag.leader.digest, leader_block.digest);
    let digests: Vec<Hash> = subdag.blocks.iter().map(|b| b.digest).collect();
    assert_eq!(
        digests,
        vec![Hash::new("kvnc-test/genesis"), leader_block.digest],
        "genesis first, leader last"
    );

    assert_eq!(committer.last_decided_round(), 3);
    let decided = committer.get_all_decided_leaders();
    assert_eq!(
        decided.get(&3).map(|l| l.status),
        Some(LeaderStatus::Commit)
    );

    // Idempotence: driving again must not re-commit round 3 or produce anything.
    assert!(
        committer.try_commit(&dag).is_none(),
        "round 3 already decided"
    );
    assert_eq!(committer.get_all_decided_leaders().len(), 1);
    assert_eq!(
        committer.get_all_decided_leaders()[&3].status,
        LeaderStatus::Commit
    );
}

/// Rounds that never reach quorum are passed over; a later certified round
/// commits. Threshold soundness: the un-certified round must never be marked
/// Commit.
#[test]
fn test_try_commit_passes_over_uncertified_round() {
    let g = genesis();
    let b1 = make_block(0, 1, vec![block_ref(&g)], "b1");
    let b3 = make_block(1, 3, vec![block_ref(&b1)], "b3");
    let dag = MockDag::with_blocks([g, b1, b3.clone()]);

    let committer = UniversalCommitter::new(committee(4));
    // Round 1: registered, only 1 vote (below quorum).
    committer.update_leader(leader_info(
        1,
        0,
        Some(Hash::new("kvnc-test/b1")),
        LeaderStatus::Undecided,
        &[],
    ));
    committer.add_vote(1, 0, Hash::new("kvnc-test/b1"));
    // Round 3: registered with full quorum.
    committer.update_leader(leader_info(
        3,
        1,
        Some(b3.digest),
        LeaderStatus::Undecided,
        &[],
    ));
    for voter in [0u16, 1, 2] {
        committer.add_vote(3, voter, b3.digest);
    }

    let subdag = committer
        .try_commit_and_mark_durable(&dag)
        .expect("persist commit")
        .expect("round 3 has quorum");
    assert_eq!(subdag.leader_round, 3);
    assert_eq!(committer.last_decided_round(), 3);

    // Safety: round 1 never got quorum, so it must never be marked Commit.
    let decided = committer.get_all_decided_leaders();
    assert_ne!(
        decided.get(&1).map(|l| l.status),
        Some(LeaderStatus::Commit),
        "round 1 had only 1 of 4 votes"
    );
    // And no round is ever both committed and skipped.
    for (round, info) in &decided {
        assert_ne!(
            info.status,
            LeaderStatus::Skip,
            "round {round} committed without ever being skipped"
        );
    }
}

/// Round 0 is exempt from commits even with full quorum (genesis handling).
#[test]
fn test_try_commit_round_zero_never_committed() {
    let g = genesis();
    let dag = MockDag::with_blocks([g.clone()]);
    let committer = UniversalCommitter::new(committee(4));
    committer.update_leader(leader_info(
        0,
        0,
        Some(g.digest),
        LeaderStatus::Undecided,
        &[],
    ));
    for voter in 0..4u16 {
        committer.add_vote(0, voter, g.digest);
    }
    assert!(committer.try_commit(&dag).is_none());
    assert_eq!(committer.last_decided_round(), 0);
    assert!(committer.get_all_decided_leaders().is_empty());
}

/// Quorum without a stored leader block must not commit anything, must not
/// mark the round decided (so it can be retried), and must commit once the
/// block shows up.
#[test]
fn test_try_commit_missing_leader_block_stalls_then_commits() {
    let g = genesis();
    let leader_block = make_block(1, 3, vec![block_ref(&g)], "leader-3");
    let dag = MockDag::with_blocks([g]); // leader block deliberately absent

    let committer = UniversalCommitter::new(committee(4));
    committer.update_leader(leader_info(
        3,
        1,
        Some(leader_block.digest),
        LeaderStatus::Undecided,
        &[],
    ));
    for voter in [0u16, 1, 2] {
        committer.add_vote(3, voter, leader_block.digest);
    }

    assert!(
        committer.try_commit(&dag).is_none(),
        "cannot commit a block the store does not have"
    );
    assert_eq!(committer.last_decided_round(), 0, "must stay retryable");
    assert!(committer.get_all_decided_leaders().is_empty());

    // Block arrives -> the same state now commits.
    dag.put(leader_block.clone());
    let subdag = committer
        .try_commit_and_mark_durable(&dag)
        .expect("persist commit")
        .expect("retry succeeds");
    assert_eq!(subdag.leader_round, 3);
    assert_eq!(committer.last_decided_round(), 3);
}

/// Safety: re-registering (e.g. re-receiving) an already-decided round must
/// not cause a second commitment of that round.
#[test]
fn test_try_commit_no_recommit_after_leader_update() {
    let g = genesis();
    let leader_block = make_block(1, 3, vec![block_ref(&g)], "leader-3");
    let dag = MockDag::with_blocks([g, leader_block.clone()]);

    let committer = UniversalCommitter::new(committee(4));
    committer.update_leader(leader_info(
        3,
        1,
        Some(leader_block.digest),
        LeaderStatus::Undecided,
        &[],
    ));
    for voter in [0u16, 1, 2] {
        committer.add_vote(3, voter, leader_block.digest);
    }
    let first = committer
        .try_commit_and_mark_durable(&dag)
        .expect("persist commit")
        .expect("first commit");
    assert_eq!(first.leader_round, 3);

    // The engine calls `update_leader` for every block it sees — even for
    // rounds already decided. That must not reopen the decision.
    committer.update_leader(leader_info(
        3,
        1,
        Some(leader_block.digest),
        LeaderStatus::Undecided,
        &[],
    ));
    assert!(
        committer.try_commit(&dag).is_none(),
        "no re-commit of round 3"
    );
    let decided = committer.get_all_decided_leaders();
    assert_eq!(decided[&3].status, LeaderStatus::Commit);
    assert_eq!(decided.len(), 1);
}

/// Determinism: two committers fed identical inputs must produce identical
/// committed sub-DAG sequences (a divergent node would fork the chain).
#[test]
fn test_try_commit_deterministic_across_committers() {
    fn build() -> (UniversalCommitter, MockDag) {
        let g = genesis();
        let b3 = make_block(1, 3, vec![block_ref(&g)], "b3");
        let b5 = make_block(2, 5, vec![block_ref(&b3)], "b5");
        let dag = MockDag::with_blocks([g, b3.clone(), b5.clone()]);
        let committer = UniversalCommitter::new(committee(4));
        for (round, author, block) in [(3u64, 1u16, &b3), (5, 2, &b5)] {
            committer.update_leader(leader_info(
                round,
                author,
                Some(block.digest),
                LeaderStatus::Undecided,
                &[],
            ));
            for voter in 0..4u16 {
                committer.add_vote(round, voter, block.digest);
            }
        }
        (committer, dag)
    }

    let (c1, d1) = build();
    let (c2, d2) = build();
    let out1 = drive_try_commit(&c1, &d1);
    let out2 = drive_try_commit(&c2, &d2);

    assert_eq!(out1.len(), 2, "rounds 3 and 5 both commit");
    assert_eq!(
        digest_seq(&out1),
        digest_seq(&out2),
        "identical inputs must yield byte-identical commit sequences"
    );
    assert_eq!(c1.last_decided_round(), c2.last_decided_round());
}

/// Monotonicity of the committed sequence: rounds strictly increase and each
/// round is committed at most once across repeated `try_commit` calls.
#[test]
fn test_try_commit_committed_rounds_strictly_increase() {
    let g = genesis();
    let mut parents = block_ref(&g);
    let mut dag_blocks = vec![g];
    let mut blocks = Vec::new();
    for round in [3u64, 6, 9, 12] {
        let b = make_block(
            (round % 4) as AuthorityIndex,
            round,
            vec![parents],
            &format!("b{round}"),
        );
        parents = block_ref(&b);
        dag_blocks.push(b.clone());
        blocks.push(b);
    }
    let dag = MockDag::with_blocks(dag_blocks);

    let committer = UniversalCommitter::new(committee(4));
    for b in &blocks {
        committer.update_leader(leader_info(
            b.round,
            b.author,
            Some(b.digest),
            LeaderStatus::Undecided,
            &[],
        ));
        for voter in 0..4u16 {
            committer.add_vote(b.round, voter, b.digest);
        }
    }

    let committed = drive_try_commit(&committer, &dag);
    let rounds: Vec<Round> = committed.iter().map(|s| s.leader_round).collect();
    assert_eq!(rounds, vec![3, 6, 9, 12]);
    for pair in rounds.windows(2) {
        assert!(
            pair[0] < pair[1],
            "committed rounds must strictly increase: {rounds:?}"
        );
    }
    // Watermark equals the last committed round; every committed round is
    // tracked as Commit exactly once.
    assert_eq!(committer.last_decided_round(), 12);
    let decided = committer.get_all_decided_leaders();
    assert_eq!(decided.len(), 4);
    for round in rounds {
        assert_eq!(decided[&round].status, LeaderStatus::Commit);
    }
}

// ---------------------------------------------------------------------------
// BUG (disabled): indirect commit rule is unreachable from `try_commit`
// ---------------------------------------------------------------------------

/// Regression: `UniversalCommitter` must apply the indirect commit rule for
/// earlier leaders of a wave when a later leader of the same wave commits.
///
/// `try_commit` only walks rounds *after* the last decided round, so the
/// indirect rule (which needs a *later* decided leader in the same wave) is
/// applied by `decide_earlier_in_wave` at commit time: when round 5 commits,
/// round 3 (same wave, causally connected) is decided Commit. Without the
/// sweep this round never entered `decided_leaders` (the historical BUG).
///
/// Note: round 3's *block* is also committed as part of round 5's causal
/// history, so this is bookkeeping/liveness — but `try_indirect_decide` and
/// the skip bookkeeping must be exercised, not dead code.
#[test]
fn test_try_commit_indirect_commit_through_later_committed_leader() {
    let g = genesis();
    let b3 = make_block(0, 3, vec![block_ref(&g)], "b3-no-quorum");
    let b5 = make_block(1, 5, vec![block_ref(&b3)], "b5-quorum");
    let dag = MockDag::with_blocks([g, b3.clone(), b5.clone()]);

    let committer = UniversalCommitter::new(committee(4));
    // Round 3: block exists, connected to round 5, but no direct quorum.
    committer.update_leader(leader_info(
        3,
        0,
        Some(b3.digest),
        LeaderStatus::Undecided,
        &[],
    ));
    committer.add_vote(3, 0, b3.digest); // 1 of 4 — no quorum
                                         // Round 5: full quorum, causally descends from round 3.
    committer.update_leader(leader_info(
        5,
        1,
        Some(b5.digest),
        LeaderStatus::Undecided,
        &[],
    ));
    for voter in 0..4u16 {
        committer.add_vote(5, voter, b5.digest);
    }

    let committed = drive_try_commit(&committer, &dag);
    assert!(!committed.is_empty(), "round 5 must commit");

    let decided = committer.get_all_decided_leaders();
    let round3 = decided.get(&3);
    assert!(
        round3.is_some(),
        "BUG: round 3 (same wave as committed round 5, causally connected) was never decided; \
         indirect rule unreachable, got decided rounds {:?}",
        decided.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        round3.map(|l| l.status),
        Some(LeaderStatus::Commit),
        "connected to the committed round-5 leader => indirect Commit"
    );
}

// Helper: digest sequence of a commit run.
fn digest_seq(subdags: &[CommittedSubDag]) -> Vec<Vec<Hash>> {
    subdags
        .iter()
        .map(|s| s.blocks.iter().map(|b| b.digest).collect())
        .collect()
}

// ---------------------------------------------------------------------------
// Regressions: BUG-1 (foreign-hash votes must not commit the leader block)
// and BUG-4 (indirect skips recorded with Skip status)
// ---------------------------------------------------------------------------

#[test]
fn test_vote_for_foreign_hash_does_not_commit() {
    let g = genesis();
    let block = make_block(0, 1, vec![block_ref(&g)], "leader");
    let dag = MockDag::with_blocks([g, block.clone()]);

    let committer = UniversalCommitter::new(committee(4));
    committer.update_leader(leader_info(
        1,
        block.author,
        Some(block.digest),
        LeaderStatus::Undecided,
        &[],
    ));

    // All 4 authorities vote, but every vote is for a *different* block.
    for voter in 0..4u16 {
        committer.add_vote(1, voter, Hash::new(format!("foreign-{voter}")));
    }

    let committed = drive_try_commit(&committer, &dag);
    assert!(
        committed.is_empty(),
        "votes for a foreign block must never commit the leader block"
    );
    assert_eq!(
        committer.get_leader(1).unwrap().status,
        LeaderStatus::Undecided,
        "leader stays undecided when quorum votes target another block"
    );
}

#[test]
fn test_indirect_skip_is_recorded_with_skip_status() {
    let g = genesis();
    // Round 3 leader block exists but is NOT causally connected to the
    // round 5 leader (round 5 does not reference it).
    let b3 = make_block(0, 3, vec![block_ref(&g)], "b3-unconnected");
    let b5 = make_block(1, 5, vec![block_ref(&g)], "b5-quorum");
    let dag = MockDag::with_blocks([g, b3.clone(), b5.clone()]);

    let committer = UniversalCommitter::new(committee(4));
    committer.update_leader(leader_info(
        3,
        0,
        Some(b3.digest),
        LeaderStatus::Undecided,
        &[],
    ));
    committer.add_vote(3, 0, b3.digest); // 1 of 4 — no direct quorum
    committer.update_leader(leader_info(
        5,
        1,
        Some(b5.digest),
        LeaderStatus::Undecided,
        &[],
    ));
    for voter in 0..4u16 {
        committer.add_vote(5, voter, b5.digest);
    }

    let committed = drive_try_commit(&committer, &dag);
    assert!(!committed.is_empty(), "round 5 must commit");

    let decided = committer.get_all_decided_leaders();
    assert_eq!(
        decided.get(&3).map(|l| l.status),
        Some(LeaderStatus::Skip),
        "unconnected earlier leader must be recorded as Skip, got {:?}",
        decided.get(&3).map(|l| l.status)
    );
    assert_eq!(
        decided.get(&5).map(|l| l.status),
        Some(LeaderStatus::Commit),
        "round 5 committed"
    );
}
