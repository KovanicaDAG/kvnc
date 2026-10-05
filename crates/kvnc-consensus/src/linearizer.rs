//! Turns a committed leader + its causal history into a totally ordered list of blocks.

use crate::types::CommittedSubDag;
use kvnc_types::block::StatementBlock;

/// Linearizer responsible for producing deterministic total order.
pub struct Linearizer;

impl Linearizer {
    /// Create a new linearizer.
    pub fn new() -> Self {
        Self
    }

    /// Given a committed leader, collect and topologically sort its causal history.
    pub fn linearize(
        &self,
        leader: StatementBlock,
        history: Vec<StatementBlock>,
    ) -> CommittedSubDag {
        // TODO: proper topological sort based on parent links
        // For skeleton we just put leader last.
        let mut blocks = history;
        let leader_round = leader.round;
        let leader_author = leader.author;
        blocks.push(leader.clone());

        CommittedSubDag {
            leader,
            blocks,
            leader_round,
            leader_author,
        }
    }
}

impl Default for Linearizer {
    fn default() -> Self {
        Self::new()
    }
}
