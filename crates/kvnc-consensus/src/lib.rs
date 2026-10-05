//! Mysticeti-style DAG consensus for KVNC.
//!
//! Implements wave-based uncertified DAG with direct/indirect commit rules.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod committer;
pub mod engine;
pub mod linearizer;
pub mod types;

pub use committer::{BaseCommitter, UniversalCommitter};
pub use engine::{ConsensusConfig, ConsensusEngine, ConsensusError, ValidatorState};
pub use linearizer::Linearizer;
pub use types::{
    AuthorityInfo, CommitResult, CommitteeInfo, CommittedSubDag, LeaderInfo, LeaderStatus,
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