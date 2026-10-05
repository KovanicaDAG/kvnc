//! Commit logic (Base + Universal Committer).
//!
//! Simplified Mysticeti-style committer for the initial implementation.
//! Full pattern matching (certificate / skip) will be expanded later.

use crate::types::{CommittedSubDag, LeaderStatus};
use kvnc_types::Round;

/// Basic committer that tracks leader decisions.
pub struct BaseCommitter {
    // In a full implementation this would hold the DAG view.
    // For skeleton we keep it minimal.
}

impl BaseCommitter {
    /// Create a new base committer.
    pub fn new() -> Self {
        Self {}
    }

    /// Very simplified direct decision (placeholder).
    /// Real implementation inspects 2f+1 support patterns.
    pub fn try_direct_decide(&self, _leader_round: Round) -> LeaderStatus {
        // TODO: inspect DAG for certificate or skip pattern
        // For now always return Undecided so higher layers can drive progress in tests.
        LeaderStatus::Undecided
    }
}

impl Default for BaseCommitter {
    fn default() -> Self {
        Self::new()
    }
}

/// Universal committer that also handles indirect decisions.
pub struct UniversalCommitter {
    // last decided round etc. will live here
}

impl UniversalCommitter {
    /// Create a new universal committer.
    pub fn new() -> Self {
        Self {}
    }

    /// Attempt to produce a new committed sub-DAG.
    /// This is the main entry point called by the core loop.
    pub fn try_commit(&self /*, dag_view: &DagView */) -> Option<CommittedSubDag> {
        // Placeholder – real logic walks leaders from highest decided round
        // applying direct + indirect rules.
        None
    }
}

impl Default for UniversalCommitter {
    fn default() -> Self {
        Self::new()
    }
}
