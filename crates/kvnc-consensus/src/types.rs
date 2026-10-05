//! Consensus-specific types.

use kvnc_types::{block::StatementBlock, AuthorityIndex, Round};
use serde::{Deserialize, Serialize};

/// Status of a leader slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeaderStatus {
    /// Decision not yet possible.
    Undecided,
    /// Should be committed.
    Commit,
    /// Should be skipped.
    Skip,
}

/// A committed sub-DAG ready for execution.
#[derive(Clone, Debug)]
pub struct CommittedSubDag {
    /// The leader block that triggered this commit.
    pub leader: StatementBlock,
    /// All blocks in causal history that are now committed (in topological order).
    pub blocks: Vec<StatementBlock>,
    /// Round of the leader.
    pub leader_round: Round,
    /// Authority that proposed the leader.
    pub leader_author: AuthorityIndex,
}
