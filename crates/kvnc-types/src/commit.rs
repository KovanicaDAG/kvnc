//! Commit-related types for KVNC.
//!
//! Contains types related to committed sub-DAGs and leader blocks.

use crate::block::StatementBlock;
use crate::{AuthorityIndex, Round};
use serde::{Deserialize, Serialize};

/// A committed sub-DAG ready for execution.
#[derive(Clone, Debug, Serialize, Deserialize)]
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