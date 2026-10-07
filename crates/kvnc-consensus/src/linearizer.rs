//! Turns a committed leader + its causal history into a totally ordered list of blocks.

#![allow(missing_docs)]
use kvnc_types::{block::StatementBlock, hash::Hash, CommittedSubDag};
use std::collections::{HashMap, HashSet, VecDeque};

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
        // Build a map of blocks by hash for quick lookup
        let mut block_map: HashMap<Hash, StatementBlock> = HashMap::new();
        for block in &history {
            block_map.insert(block.digest, block.clone());
        }
        block_map.insert(leader.digest, leader.clone());

        // Topological sort using Kahn's algorithm
        // We need to sort blocks such that parents come before children
        let mut in_degree: HashMap<Hash, usize> = HashMap::new();
        let mut adjacency: HashMap<Hash, Vec<Hash>> = HashMap::new();

        // Initialize in-degrees and adjacency
        for block in &history {
            in_degree.insert(block.digest, 0);
            adjacency.insert(block.digest, Vec::new());
        }
        in_degree.insert(leader.digest, 0);
        adjacency.insert(leader.digest, Vec::new());

        // Build the graph: for each block, add edges from parents to this block
        for block in &history {
            for parent_ref in &block.parents {
                if block_map.contains_key(&parent_ref.digest) {
                    // Edge from parent to block
                    adjacency
                        .get_mut(&parent_ref.digest)
                        .unwrap()
                        .push(block.digest);
                    *in_degree.get_mut(&block.digest).unwrap() += 1;
                }
            }
        }

        // Also add leader's parents
        for parent_ref in &leader.parents {
            if block_map.contains_key(&parent_ref.digest) {
                adjacency
                    .get_mut(&parent_ref.digest)
                    .unwrap()
                    .push(leader.digest);
                *in_degree.get_mut(&leader.digest).unwrap() += 1;
            }
        }

        // Kahn's algorithm — deterministic ordering.
        //
        // Seed the queue from the zero-degree nodes sorted by digest, and
        // expand children in sorted order, so the output depends only on the
        // graph structure (parent/child edges + the block set), never on
        // HashMap iteration order or the order of the input `history` slice.
        // Two nodes with identical committed history must linearize
        // identically; any divergence here would fork execution.
        let mut roots: Vec<Hash> = in_degree
            .iter()
            .filter(|(_, &degree)| degree == 0)
            .map(|(hash, _)| *hash)
            .collect();
        roots.sort_by_key(|h| h.0);
        let mut queue: VecDeque<Hash> = roots.into();

        // Canonicalize adjacency lists so child expansion is deterministic.
        for children in adjacency.values_mut() {
            children.sort_by_key(|h| h.0);
        }

        let mut sorted = Vec::new();
        while let Some(hash) = queue.pop_front() {
            sorted.push(hash);
            if let Some(children) = adjacency.get(&hash) {
                for child in children {
                    let degree = in_degree.get_mut(child).unwrap();
                    *degree -= 1;
                    if *degree == 0 {
                        queue.push_back(*child);
                    }
                }
            }
        }

        // Convert sorted hashes back to blocks
        // For blocks not in the sorted list (shouldn't happen), append them
        let mut result_blocks = Vec::new();
        let mut seen = HashSet::new();
        for hash in sorted {
            if let Some(block) = block_map.get(&hash) {
                result_blocks.push(block.clone());
                seen.insert(hash);
            }
        }

        // Add any remaining blocks (shouldn't happen in a valid DAG).
        // Deterministic: sorted by (round, author, digest) rather than input order.
        let mut remaining: Vec<&StatementBlock> = history
            .iter()
            .filter(|block| !seen.contains(&block.digest))
            .collect();
        remaining.sort_by_key(|b| (b.round, b.author, b.digest.0));
        for block in remaining {
            result_blocks.push(block.clone());
        }

        // Leader should be last in topological order
        if !seen.contains(&leader.digest) {
            result_blocks.push(leader.clone());
        }

        let leader_round = leader.round;
        let leader_author = leader.author;

        CommittedSubDag {
            leader,
            blocks: result_blocks,
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
