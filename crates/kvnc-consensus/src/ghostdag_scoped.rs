//! Scoped GHOSTDAG k=3 colouring.
//!
//! Runs classic GHOSTDAG only on the mergeset of a newly committed leader.
//! This keeps CPU and memory cost proportional to wave size, not full history.

use kvnc_types::block::StatementBlock;
use kvnc_types::hash::Hash;
use std::collections::{HashMap, HashSet, VecDeque};

/// Output of a scoped colouring run.
#[derive(Debug, Clone)]
pub struct ColouringResult {
    /// Blocks that were coloured blue (conflict-free set).
    pub blue: Vec<Hash>,
    /// Selected-parent relation for the blue blocks.
    pub selected_parent: HashMap<Hash, Option<Hash>>,
    /// Blue score of each blue block (useful for metrics / debugging).
    pub blue_score: HashMap<Hash, u64>,
}

impl ColouringResult {
    /// Returns the blue blocks in a deterministic topological order
    /// induced by the selected-parent relation.
    pub fn blue_ordered(&self) -> Vec<Hash> {
        // Kahn's algorithm on the selected-parent tree
        let mut in_degree: HashMap<Hash, usize> = HashMap::new();
        let mut children: HashMap<Hash, Vec<Hash>> = HashMap::new();

        for &blue_hash in &self.blue {
            in_degree.insert(blue_hash, 0);
            children.insert(blue_hash, Vec::new());
        }

        for (&blue_hash, &opt_parent) in &self.selected_parent {
            if let Some(parent) = opt_parent {
                if self.blue.iter().any(|h| h == &parent) {
                    *in_degree.get_mut(&blue_hash).unwrap() += 1;
                    children.get_mut(&parent).unwrap().push(blue_hash);
                }
            }
        }

        // Deterministic: sort roots and children by hash
        let mut roots: Vec<Hash> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(h, _)| *h)
            .collect();
        roots.sort_by_key(|h| h.0);
        for child_list in children.values_mut() {
            child_list.sort_by_key(|h| h.0);
        }

        let mut queue: VecDeque<Hash> = roots.into();
        let mut ordered = Vec::new();

        while let Some(hash) = queue.pop_front() {
            ordered.push(hash);
            if let Some(child_list) = children.get(&hash) {
                for child in child_list {
                    let deg = in_degree.get_mut(child).unwrap();
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(*child);
                    }
                }
            }
        }

        ordered
    }
}

/// Colour the given mergeset with GHOSTDAG parameter `k`.
///
/// `blocks` – the mergeset (and optionally previous tips already included)
/// `k`      – must be 3 for the current safety claims
/// `previous_tips` – tips of prior committed sub-dags (treated as already blue)
pub fn colour_mergeset(
    blocks: &[StatementBlock],
    k: usize,
    previous_tips: &[Hash],
) -> ColouringResult {
    assert_eq!(k, 3, "MysticGhost currently requires k = 3");

    // Build set of all block hashes in the mergeset for O(1) lookup
    let mut block_hashes = HashSet::with_capacity(blocks.len());
    for block in blocks {
        block_hashes.insert(block.digest);
    }

    // Build parents map: hash -> Vec<parent hashes that are in the subgraph or previous_tips>
    let mut parents_in_subgraph: HashMap<Hash, Vec<Hash>> = HashMap::with_capacity(blocks.len());
    for block in blocks {
        let mut parent_hashes = Vec::new();
        for parent_ref in &block.parents {
            if block_hashes.contains(&parent_ref.digest) || previous_tips.contains(&parent_ref.digest) {
                parent_hashes.push(parent_ref.digest);
            }
        }
        parents_in_subgraph.insert(block.digest, parent_hashes);
    }

    // Sort blocks topologically by round (ascending), then by hash for deterministic tiebreak
    let mut sorted_blocks = blocks.to_vec();
    sorted_blocks.sort_by_key(|b| (b.round, b.digest.0));

    // State for the GHOSTDAG algorithm
    let mut already_blue: HashSet<Hash> = HashSet::new();
    let mut past_cone: HashMap<Hash, HashSet<Hash>> = HashMap::new();
    let mut blue_score: HashMap<Hash, u64> = HashMap::new();
    let mut selected_parent: HashMap<Hash, Option<Hash>> = HashMap::new();
    let mut blue: Vec<Hash> = Vec::new();

    // Seed with previous_tips (they are pre-coloured blue with score 0)
    for tip in previous_tips {
        already_blue.insert(*tip);
        let mut cone = HashSet::new();
        cone.insert(*tip);
        past_cone.insert(*tip, cone);
        blue_score.insert(*tip, 0);
        selected_parent.insert(*tip, None);
        // Note: we don't add previous_tips to `blue` since they are not part of the current mergeset
    }

    // Process blocks in topological order
    for block in &sorted_blocks {
        let block_hash = block.digest;

        // Find blue parents from the subgraph
        let parent_hashes = parents_in_subgraph
            .get(&block_hash)
            .cloned()
            .unwrap_or_default();
        let blue_parents: Vec<Hash> = parent_hashes
            .iter()
            .filter(|h| already_blue.contains(h))
            .copied()
            .collect();

        // Compute ancestors of this block among already-blue blocks.
        // Since we maintain past_cone for each blue block, the blue ancestors
        // are exactly the union of the past_cones of the blue parents.
        let mut ancestors = HashSet::new();
        for parent in &blue_parents {
            if let Some(parent_cone) = past_cone.get(parent) {
                ancestors.extend(parent_cone.iter().copied());
            }
        }

        // Compute blue anticone: already-blue blocks that are incomparable with this block.
        // Since we process in topological order, block_hash cannot be ancestor of any already-blue block.
        // So we only need to check if any already-blue block is ancestor of this block.
        let mut blue_anticone = Vec::new();
        for &blue_hash in &already_blue {
            // blue_hash is ancestor of block_hash iff blue_hash is in ancestors set
            if !ancestors.contains(&blue_hash) {
                blue_anticone.push(blue_hash);
            }
        }

        if blue_anticone.len() <= k {
            // Colour BLUE
            already_blue.insert(block_hash);
            blue.push(block_hash);

            // Compute past cone: union of blue parents' past cones + self
            let mut cone = HashSet::new();
            cone.insert(block_hash);
            for parent in &blue_parents {
                if let Some(parent_cone) = past_cone.get(parent) {
                    cone.extend(parent_cone.iter().copied());
                }
            }
            past_cone.insert(block_hash, cone);

            // Select parent: blue parent with highest blue_score, tie-break by hash
            let best_parent = if blue_parents.is_empty() {
                None
            } else {
                let mut best = blue_parents[0];
                let mut best_score = blue_score[&best];
                for parent in &blue_parents[1..] {
                    let score = blue_score[parent];
                    if score > best_score || (score == best_score && parent.0 < best.0) {
                        best = *parent;
                        best_score = score;
                    }
                }
                Some(best)
            };
            selected_parent.insert(block_hash, best_parent);

            // Blue score = parent's blue score + 1 (or 0 if no blue parent)
            let score = best_parent.map(|p| blue_score[&p] + 1).unwrap_or(0);
            blue_score.insert(block_hash, score);
        } else {
            // Colour RED: do nothing (not added to already_blue, no selected_parent, no blue_score)
        }
    }

    ColouringResult {
        blue,
        selected_parent,
        blue_score,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvnc_types::block::{BlockReference, StatementBlock};
    use kvnc_types::hash::Hash;
    use kvnc_types::{AuthorityIndex, Round, Signature};

    fn make_block(
        author: AuthorityIndex,
        round: Round,
        parents: Vec<BlockReference>,
        tag: &str,
    ) -> StatementBlock {
        StatementBlock {
            author,
            round,
            parents,
            transactions: Vec::new(),
            statements: tag.as_bytes().to_vec(),
            signature: Signature([0u8; 64]),
            digest: Hash::new(format!("kvnc-test/{tag}").as_bytes()),
        }
    }

    fn block_ref(block: &StatementBlock) -> BlockReference {
        BlockReference {
            author: block.author,
            round: block.round,
            digest: block.digest,
        }
    }

    #[test]
    fn test_simple_chain_all_blue() {
        // Genesis -> block1 -> block2 -> block3 (all in chain)
        let genesis = make_block(0, 0, vec![], "genesis");
        let b1 = make_block(1, 1, vec![block_ref(&genesis)], "b1");
        let b2 = make_block(2, 2, vec![block_ref(&b1)], "b2");
        let b3 = make_block(3, 3, vec![block_ref(&b2)], "b3");

        let blocks = vec![genesis.clone(), b1.clone(), b2.clone(), b3.clone()];
        let result = colour_mergeset(&blocks, 3, &[]);

        // All should be blue
        assert_eq!(result.blue.len(), 4);
        assert!(result.blue.contains(&genesis.digest));
        assert!(result.blue.contains(&b1.digest));
        assert!(result.blue.contains(&b2.digest));
        assert!(result.blue.contains(&b3.digest));

        // Selected parent chain
        assert_eq!(result.selected_parent[&genesis.digest], None);
        assert_eq!(result.selected_parent[&b1.digest], Some(genesis.digest));
        assert_eq!(result.selected_parent[&b2.digest], Some(b1.digest));
        assert_eq!(result.selected_parent[&b3.digest], Some(b2.digest));

        // Blue scores
        assert_eq!(result.blue_score[&genesis.digest], 0);
        assert_eq!(result.blue_score[&b1.digest], 1);
        assert_eq!(result.blue_score[&b2.digest], 2);
        assert_eq!(result.blue_score[&b3.digest], 3);

        // Blue ordered should be genesis, b1, b2, b3
        let ordered = result.blue_ordered();
        assert_eq!(ordered, vec![genesis.digest, b1.digest, b2.digest, b3.digest]);
    }

    #[test]
    fn test_two_conflicting_blocks_k3_both_blue() {
        // Two blocks at same round from different authors, same parent (genesis)
        let genesis = make_block(0, 0, vec![], "genesis");
        let b1 = make_block(1, 1, vec![block_ref(&genesis)], "b1-conflict");
        let b2 = make_block(2, 1, vec![block_ref(&genesis)], "b2-conflict");

        let blocks = vec![genesis.clone(), b1.clone(), b2.clone()];
        let result = colour_mergeset(&blocks, 3, &[]);

        // Both should be blue (anticone size 1 <= 3)
        assert_eq!(result.blue.len(), 3);
        assert!(result.blue.contains(&genesis.digest));
        assert!(result.blue.contains(&b1.digest));
        assert!(result.blue.contains(&b2.digest));

        // Both have genesis as selected parent
        assert_eq!(result.selected_parent[&b1.digest], Some(genesis.digest));
        assert_eq!(result.selected_parent[&b2.digest], Some(genesis.digest));
    }

    #[test]
    fn test_five_conflicting_blocks_k3_fifth_red() {
        // Five pairwise-incomparable blocks at same round (same parent)
        let genesis = make_block(0, 0, vec![], "genesis");
        let b1 = make_block(1, 1, vec![block_ref(&genesis)], "b1");
        let b2 = make_block(2, 1, vec![block_ref(&genesis)], "b2");
        let b3 = make_block(3, 1, vec![block_ref(&genesis)], "b3");
        let b4 = make_block(4, 1, vec![block_ref(&genesis)], "b4");
        let b5 = make_block(5, 1, vec![block_ref(&genesis)], "b5");

        let blocks = vec![
            genesis.clone(),
            b1.clone(),
            b2.clone(),
            b3.clone(),
            b4.clone(),
            b5.clone(),
        ];
        let result = colour_mergeset(&blocks, 3, &[]);

        // Exactly 4 of the 5 conflicting blocks should be blue (plus genesis = 5 total)
        // The specific 4 depends on hash ordering, but exactly one should be red
        let conflicting_blocks = [b1.digest, b2.digest, b3.digest, b4.digest, b5.digest];
        let blue_conflicting: Vec<_> = result.blue.iter()
            .filter(|h| conflicting_blocks.contains(h))
            .copied()
            .collect();
        assert_eq!(blue_conflicting.len(), 4, "Exactly 4 of 5 conflicting blocks should be blue");
        assert!(result.blue.contains(&genesis.digest), "Genesis should be blue");
        assert_eq!(result.blue.len(), 5, "Total blue: genesis + 4 conflicting = 5");
    }

    #[test]
    fn test_previous_tips_seeded() {
        // Previous tips are already blue
        let tip1 = make_block(1, 5, vec![], "tip1");
        let tip2 = make_block(2, 5, vec![], "tip2");
        let previous_tips = vec![tip1.digest, tip2.digest];

        // New blocks at round 6 referencing both tips
        let b1 = make_block(3, 6, vec![block_ref(&tip1), block_ref(&tip2)], "b1");

        let blocks = vec![b1.clone()];
        let result = colour_mergeset(&blocks, 3, &previous_tips);

        // b1 should be blue, its anticone against previous tips is empty (tips are its ancestors)
        assert_eq!(result.blue.len(), 1);
        assert!(result.blue.contains(&b1.digest));
        // Selected parent should be one of the tips (both have score 0, tie-break by hash)
        let parent = result.selected_parent[&b1.digest].unwrap();
        assert!(parent == tip1.digest || parent == tip2.digest);
    }

    #[test]
    fn test_diamond_structure() {
        //    genesis
        //   /       \
        //  b1        b2
        //   \       /
        //    b3 (merges both)
        let genesis = make_block(0, 0, vec![], "genesis");
        let b1 = make_block(1, 1, vec![block_ref(&genesis)], "b1");
        let b2 = make_block(2, 1, vec![block_ref(&genesis)], "b2");
        let b3 = make_block(
            3,
            2,
            vec![block_ref(&b1), block_ref(&b2)],
            "b3",
        );

        let blocks = vec![genesis.clone(), b1.clone(), b2.clone(), b3.clone()];
        let result = colour_mergeset(&blocks, 3, &[]);

        // All should be blue
        assert_eq!(result.blue.len(), 4);
        assert!(result.blue.contains(&genesis.digest));
        assert!(result.blue.contains(&b1.digest));
        assert!(result.blue.contains(&b2.digest));
        assert!(result.blue.contains(&b3.digest));

        // b3's selected parent should be b1 or b2 (both have score 1, tie-break by hash)
        let b3_parent = result.selected_parent[&b3.digest].unwrap();
        assert!(b3_parent == b1.digest || b3_parent == b2.digest);
    }

    #[test]
    fn test_blue_ordered_is_topological() {
        // Test that blue_ordered returns a valid topological order
        let genesis = make_block(0, 0, vec![], "genesis");
        let b1 = make_block(1, 1, vec![block_ref(&genesis)], "b1");
        let b2 = make_block(2, 1, vec![block_ref(&genesis)], "b2");
        let b3 = make_block(
            3,
            2,
            vec![block_ref(&b1), block_ref(&b2)],
            "b3",
        );

        let blocks = vec![genesis.clone(), b1.clone(), b2.clone(), b3.clone()];
        let result = colour_mergeset(&blocks, 3, &[]);

        let ordered = result.blue_ordered();
        assert_eq!(ordered.len(), 4);

        // Check topological order: parents before children
        let pos: HashMap<Hash, usize> = ordered.iter().enumerate().map(|(i, h)| (*h, i)).collect();

        // genesis before b1, b2
        assert!(pos[&genesis.digest] < pos[&b1.digest]);
        assert!(pos[&genesis.digest] < pos[&b2.digest]);
        // b1 before b3
        assert!(pos[&b1.digest] < pos[&b3.digest]);
        // b2 before b3
        assert!(pos[&b2.digest] < pos[&b3.digest]);
    }

    #[test]
    fn test_disconnected_block_still_colored() {
        // Block with no parents in the set (but not genesis)
        let genesis = make_block(0, 0, vec![], "genesis");
        let b1 = make_block(1, 1, vec![block_ref(&genesis)], "b1");
        // b2 has parent that doesn't exist in our set
        let fake_parent = BlockReference {
            author: 99,
            round: 0,
            digest: Hash::new(b"fake"),
        };
        let b2 = make_block(2, 2, vec![fake_parent], "b2");

        let blocks = vec![genesis.clone(), b1.clone(), b2.clone()];
        let result = colour_mergeset(&blocks, 3, &[]);

        // b2 has no blue parents, so its anticone is all already_blue (genesis, b1) = 2 <= 3
        // So it should be blue with score 0
        assert_eq!(result.blue.len(), 3);
        assert!(result.blue.contains(&b2.digest));
        assert_eq!(result.selected_parent[&b2.digest], None);
        assert_eq!(result.blue_score[&b2.digest], 0);
    }
}