//! Domain-separated binary Merkle tree over a list of leaves.
//!
//! The tree is used to commit a block to the set of transactions it contains
//! and to provide inclusion proofs for light clients / block explorers.
//!
//! # Encoding
//!
//! - Each input leaf `L` is lifted to `Hash::for_merkle_leaf(L)`.
//! - An internal node over `(left, right)` is `Hash::for_merkle_node(left ||
//!   right)`.
//! - Levels are constructed pairwise, bottom-up. If a level has an odd number
//!   of nodes, the final node is duplicated (Bitcoin-style) so every node has a
//!   right sibling.
//! - The root of an empty tree is [`Hash::zero`], and the root of a single-leaf
//!   tree is that leaf's lifted hash.
//!
//! Leaf and internal-node hashing use **distinct domain tags**, so a leaf can
//! never be confused with an internal node (second-preimage resistance).

use crate::hash::Hash;
use serde::{Deserialize, Serialize};

/// A Merkle inclusion proof for one leaf.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MerkleProof {
    /// Index of the proven leaf within the original (prover) leaf list.
    pub index: usize,
    /// Sibling hashes, ordered bottom-up from the leaf level to the root.
    pub siblings: Vec<Hash>,
}

impl MerkleProof {
    /// Verify this proof against `root` for `leaf`.
    pub fn verify(&self, root: &Hash, leaf: &Hash) -> bool {
        verify_merkle_proof(root, leaf, self)
    }
}

/// Compute the root of the Merkle tree over `leaves`.
///
/// An empty list yields [`Hash::zero`].
pub fn merkle_root(leaves: &[Hash]) -> Hash {
    if leaves.is_empty() {
        return Hash::zero();
    }
    let mut nodes: Vec<Hash> = leaves.iter().map(Hash::for_merkle_leaf).collect();
    while nodes.len() > 1 {
        nodes = next_level(&nodes);
    }
    nodes[0]
}

/// Build an inclusion proof for `leaves[index]`.
///
/// Returns `None` when `index` is out of range (including for an empty list).
pub fn merkle_proof(leaves: &[Hash], index: usize) -> Option<MerkleProof> {
    if index >= leaves.len() {
        return None;
    }
    let mut nodes: Vec<Hash> = leaves.iter().map(Hash::for_merkle_leaf).collect();
    let mut idx = index;
    let mut siblings = Vec::new();
    while nodes.len() > 1 {
        let sibling_idx = idx ^ 1;
        // Odd level: the last node has no right sibling, so it is duplicated.
        let sibling = if sibling_idx < nodes.len() {
            nodes[sibling_idx]
        } else {
            nodes[nodes.len() - 1]
        };
        siblings.push(sibling);
        nodes = next_level(&nodes);
        idx /= 2;
    }
    Some(MerkleProof { index, siblings })
}

/// Verify an inclusion proof for `leaf` under `root`.
pub fn verify_merkle_proof(root: &Hash, leaf: &Hash, proof: &MerkleProof) -> bool {
    let mut h = Hash::for_merkle_leaf(leaf);
    let mut idx = proof.index;
    for sibling in &proof.siblings {
        let combined: Vec<u8> = if idx.is_multiple_of(2) {
            [h.as_ref(), sibling.as_ref()].concat()
        } else {
            [sibling.as_ref(), h.as_ref()].concat()
        };
        h = Hash::for_merkle_node(combined);
        idx /= 2;
    }
    h == *root
}

/// Combine one level (pairwise), duplicating the final node when the count is
/// odd.
fn next_level(nodes: &[Hash]) -> Vec<Hash> {
    let mut out = Vec::with_capacity(nodes.len().div_ceil(2));
    for pair in nodes.chunks(2) {
        let left = pair[0];
        let right = if pair.len() == 2 { pair[1] } else { pair[0] };
        out.push(Hash::for_merkle_node(
            [left.as_ref(), right.as_ref()].concat(),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(n: u8) -> Hash {
        Hash([n; 32])
    }

    #[test]
    fn empty_tree_has_zero_root() {
        assert_eq!(merkle_root(&[]), Hash::zero());
        assert!(merkle_proof(&[], 0).is_none());
    }

    #[test]
    fn single_leaf_root_is_lifted_leaf() {
        let l = leaf(7);
        assert_eq!(merkle_root(&[l]), Hash::for_merkle_leaf(l));
    }

    #[test]
    fn proofs_verify_for_every_index_and_size() {
        for n in 1..=16usize {
            let leaves: Vec<Hash> = (0..n as u8).map(leaf).collect();
            let root = merkle_root(&leaves);
            for (i, l) in leaves.iter().enumerate() {
                let proof = merkle_proof(&leaves, i).expect("index in range");
                assert!(
                    proof.verify(&root, l),
                    "proof failed for n={n}, index={i}"
                );
            }
        }
    }

    #[test]
    fn wrong_leaf_is_rejected() {
        let leaves: Vec<Hash> = (0..5u8).map(leaf).collect();
        let root = merkle_root(&leaves);
        let proof = merkle_proof(&leaves, 2).unwrap();
        assert!(proof.verify(&root, &leaves[2]));
        assert!(!proof.verify(&root, &leaf(99)));
    }

    #[test]
    fn root_is_order_sensitive() {
        let a = [leaf(1), leaf(2)];
        let b = [leaf(2), leaf(1)];
        assert_ne!(merkle_root(&a), merkle_root(&b));
    }
}