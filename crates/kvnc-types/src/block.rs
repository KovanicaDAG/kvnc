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
#[derive(Clone, Serialize, Deserialize, Debug)]
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
        // TODO: properly serialize transactions
        for tx in transactions {
            data.extend_from_slice(&tx.hash().0);
        }
        Hash::new(&data)
    }
}

/// Simplified Block alias used in higher layers.
pub type Block = StatementBlock;
