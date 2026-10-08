//! Mysticeti-style DAG consensus for KVNC.
//!
//! Implements wave-based uncertified DAG with direct/indirect commit rules.

#![deny(unsafe_code)]
#![allow(missing_docs)]
#![allow(clippy::result_large_err)]
#![allow(clippy::large_enum_variant)]
#![allow(unused_mut)]
#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(unused_variables)]

pub mod committer;
pub mod committer_mysticghost;
pub mod engine;
pub mod ghostdag_scoped;
pub mod linearizer;
pub mod metrics;
pub mod mysticghost;
pub mod types;

pub use committer::{BaseCommitter, UniversalCommitter};
pub use engine::{ConsensusConfig, ConsensusEngine, ConsensusError, ValidatorState, BlockBroadcaster, VoteBroadcaster};
pub use kvnc_types::Vote;
pub use linearizer::Linearizer;
pub use metrics::{
    metrics_text, record_block_height, record_commit_latency, record_mergeset_size_metric,
    record_mempool_size, record_peer_count, record_rss_proxy, record_colouring_duration_ms,
    record_mergeset_size, record_pruned_blocks, record_pruned_waves, registry,
    update_dag_blocks_in_memory,
};
pub use types::{
    AuthorityInfo, CommitResult, CommittedSubDag, CommitteeInfo, CommitteeInfoError, LeaderInfo,
    LeaderStatus,
};

use kvnc_types::{Round, WAVE_LENGTH};

/// Returns the wave number for a given round.
#[inline]
pub fn wave_of(round: Round) -> u64 {
    round / WAVE_LENGTH
}

/// Returns the offset inside the wave (0 = leader, 1 = vote, 2 = decide).
#[inline]
pub fn offset_in_wave(round: Round) -> u64 {
    round % WAVE_LENGTH
}

/// True if this round is a leader round.
#[inline]
pub fn is_leader_round(round: Round) -> bool {
    offset_in_wave(round) == 0
}

/// True if this round is a vote round.
#[inline]
pub fn is_vote_round(round: Round) -> bool {
    offset_in_wave(round) == 1
}
