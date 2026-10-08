//! BLAKE3-based hash type with domain separation.
//!
//! Every digest that identifies a distinct kind of object (block, transaction,
//! state node, address, …) is produced through a domain-separated constructor
//! so that a digest from one domain can never equal a digest from another for
//! any (even adversarially chosen) pair of preimages.
//!
//! The encoding is:
//!
//! ```text
//! digest = BLAKE3( tag || 0x00 || payload )
//! ```
//!
//! where `tag` is one of the constants in [`domain`]. The `0x00` separator
//! prevents two adjacent tags from being concatenated into an ambiguous prefix
//! (e.g. `b"kvnc/tx"` + `pay` never collides with `b"kvnc/txa"` + `y`).

use blake3::Hasher;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Domain-separation tags.
///
/// Tags are part of the consensus/wire format: changing or reordering the byte
/// contents of a tag changes every digest derived under it. Add new tags, never
/// mutate existing ones.
pub mod domain {
    /// Domain tag for block digests (see [`super::Hash::for_block`]).
    pub const BLOCK: &[u8] = b"kvnc/block";
    /// Domain tag for transaction hashes (see [`super::Hash::for_transaction`]).
    pub const TRANSACTION: &[u8] = b"kvnc/tx";
    /// Domain tag for ledger/state commitments (see [`super::Hash::for_state`]).
    pub const STATE: &[u8] = b"kvnc/state";
    /// Domain tag for consensus votes (see [`super::Hash::for_vote`]).
    pub const VOTE: &[u8] = b"kvnc/vote";
    /// Domain tag for account/contract address derivation
    /// (see [`super::Hash::for_address`]).
    pub const ADDRESS: &[u8] = b"kvnc/address";
    /// Domain tag for a Merkle tree leaf.
    pub const MERKLE_LEAF: &[u8] = b"kvnc/merkle/leaf";
    /// Domain tag for an internal Merkle tree node.
    pub const MERKLE_NODE: &[u8] = b"kvnc/merkle/node";
}

/// 32-byte BLAKE3 hash.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct Hash(pub [u8; 32]);

impl Hash {
    /// Compute the un-domained BLAKE3 hash of the given data.
    ///
    /// Prefer the domain-specific constructors ([`Hash::for_block`],
    /// [`Hash::for_transaction`], …) for all object identifiers. This method is
    /// retained for ad-hoc/local hashing (mempool maps, cache keys, tests) where
    /// cross-domain collision resistance is irrelevant.
    pub fn new(data: impl AsRef<[u8]>) -> Self {
        let mut hasher = Hasher::new();
        hasher.update(data.as_ref());
        Self(*hasher.finalize().as_bytes())
    }

    /// Compute a domain-separated BLAKE3 hash: `BLAKE3(tag || 0x00 || data)`.
    ///
    /// `tag` should be one of the constants in [`domain`]. Callers may pass a
    /// static tag directly; dynamic tags are supported but must remain globally
    /// unique per object kind to preserve the separation guarantee.
    pub fn new_domain(tag: &[u8], data: impl AsRef<[u8]>) -> Self {
        let mut hasher = Hasher::new();
        hasher.update(tag);
        hasher.update(&[0u8]);
        hasher.update(data.as_ref());
        Self(*hasher.finalize().as_bytes())
    }

    /// Domain-separated hash for a [`crate::StatementBlock`] digest.
    pub fn for_block(data: impl AsRef<[u8]>) -> Self {
        Self::new_domain(domain::BLOCK, data)
    }

    /// Domain-separated hash for a [`crate::Transaction`].
    pub fn for_transaction(data: impl AsRef<[u8]>) -> Self {
        Self::new_domain(domain::TRANSACTION, data)
    }

    /// Domain-separated hash for a ledger/state commitment.
    pub fn for_state(data: impl AsRef<[u8]>) -> Self {
        Self::new_domain(domain::STATE, data)
    }

    /// Domain-separated hash for a consensus vote.
    pub fn for_vote(data: impl AsRef<[u8]>) -> Self {
        Self::new_domain(domain::VOTE, data)
    }

    /// Domain-separated hash for an account/contract address derivation.
    pub fn for_address(data: impl AsRef<[u8]>) -> Self {
        Self::new_domain(domain::ADDRESS, data)
    }

    /// Domain-separated hash for a Merkle tree leaf.
    pub fn for_merkle_leaf(data: impl AsRef<[u8]>) -> Self {
        Self::new_domain(domain::MERKLE_LEAF, data)
    }

    /// Domain-separated hash for an internal Merkle tree node.
    pub fn for_merkle_node(data: impl AsRef<[u8]>) -> Self {
        Self::new_domain(domain::MERKLE_NODE, data)
    }

    /// Zero hash (useful for genesis).
    pub fn zero() -> Self {
        Self([0u8; 32])
    }

    /// Convert to hex string.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", &self.to_hex()[..16]) // short form
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash({})", self.to_hex())
    }
}

impl AsRef<[u8]> for Hash {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_separation_prevents_cross_domain_collisions() {
        // The same payload hashed under different domains must differ.
        let payload = b"identical payload";
        let block = Hash::for_block(payload);
        let tx = Hash::for_transaction(payload);
        let state = Hash::for_state(payload);
        let vote = Hash::for_vote(payload);
        let addr = Hash::for_address(payload);

        let all = [block, tx, state, vote, addr];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "distinct domains collided");
            }
        }
        // And none equals the plain, un-domained hash.
        assert_ne!(block, Hash::new(payload));
    }

    #[test]
    fn separator_prevents_ambiguous_concatenation() {
        // b"kvnc/tx" + b"a" must not equal b"kvnc/txa" + b"".
        let a = Hash::new_domain(b"kvnc/tx", b"a");
        let b = Hash::new_domain(b"kvnc/txa", b"");
        assert_ne!(a, b);
    }

    #[test]
    fn domain_hash_is_deterministic() {
        assert_eq!(Hash::for_block(b"x"), Hash::for_block(b"x"));
        assert_ne!(Hash::for_block(b"x"), Hash::for_block(b"y"));
    }
}