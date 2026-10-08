//! Block and StatementBlock definitions for the DAG.

use crate::crypto::Signature;
use crate::hash::Hash;
use crate::transaction::Transaction;
use crate::{AuthorityIndex, Round};
use serde::{Deserialize, Serialize};

/// Reference to a block in the DAG (author + round + digest).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct BlockReference {
    /// Authority index of the block author.
    pub author: AuthorityIndex,
    /// Round number of the block.
    pub round: Round,
    /// Digest/hash of the block.
    pub digest: Hash,
}

/// Full block as used in the DAG (Mysticeti-style StatementBlock).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct StatementBlock {
    /// Author of this block.
    pub author: AuthorityIndex,
    /// Round number.
    pub round: Round,
    /// Parents (at least 2f+1 from previous round).
    pub parents: Vec<BlockReference>,
    /// Transactions included in this block.
    pub transactions: Vec<Transaction>,
    /// Optional: additional statements / votes.
    pub statements: Vec<u8>, // placeholder for future vote data
    /// Signature over the block content.
    pub signature: Signature,
    /// Cached digest.
    pub digest: Hash,
}

impl StatementBlock {
    /// Compute the digest of the block (without signature).
    ///
    /// The digest commits to the author, round, parent references and the
    /// Merkle root of the included transactions. It uses the block domain tag
    /// so a block digest can never collide with a transaction, state or address
    /// digest (see [`crate::hash::domain`]).
    pub fn compute_digest(
        author: AuthorityIndex,
        round: Round,
        parents: &[BlockReference],
        transactions: &[Transaction],
    ) -> Hash {
        let mut data = Vec::new();
        data.extend_from_slice(&author.to_le_bytes());
        data.extend_from_slice(&round.to_le_bytes());
        for p in parents {
            data.extend_from_slice(&p.author.to_le_bytes());
            data.extend_from_slice(&p.round.to_le_bytes());
            data.extend_from_slice(&p.digest.0);
        }
        // Commit to the transaction set via its Merkle root. The root itself
        // is domain-separated (`merkle/leaf` + `merkle/node`), and the whole
        // block digest is tagged with the block domain.
        let leaves: Vec<Hash> = transactions.iter().map(|tx| tx.hash()).collect();
        let merkle = crate::merkle::merkle_root(&leaves);
        data.extend_from_slice(&merkle.0);
        Hash::for_block(&data)
    }

    /// Merkle root over the block's transactions.
    ///
    /// Equivalent to the commitment folded into [`StatementBlock::compute_digest`];
    /// exposed so explorers / light clients can verify inclusion proofs.
    pub fn merkle_root(&self) -> Hash {
        let leaves: Vec<Hash> = self.transactions.iter().map(|tx| tx.hash()).collect();
        crate::merkle::merkle_root(&leaves)
    }

    /// Build an inclusion proof for the transaction at `index`.
    pub fn transaction_proof(&self, index: usize) -> Option<crate::merkle::MerkleProof> {
        let leaves: Vec<Hash> = self.transactions.iter().map(|tx| tx.hash()).collect();
        crate::merkle::merkle_proof(&leaves, index)
    }

    /// Verify `proof` that `tx_hash` is part of this block.
    pub fn verifies_transaction(
        &self,
        tx_hash: &Hash,
        proof: &crate::merkle::MerkleProof,
    ) -> bool {
        crate::merkle::verify_merkle_proof(&self.merkle_root(), tx_hash, proof)
    }
}

/// Simplified Block alias used in higher layers.
pub type Block = StatementBlock;
