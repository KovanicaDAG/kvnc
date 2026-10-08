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
        Hash::new_keyed(Hash::DOMAIN_BLOCK, &data)
    }

    /// Binary Merkle root over the included transaction hashes.
    /// Empty transaction list yields a fixed zero hash.
    pub fn merkle_root(&self) -> Hash {
        if self.transactions.is_empty() {
            return Hash::zero();
        }
        let mut current: Vec<Hash> = self.transactions.iter().map(|tx| tx.hash()).collect();
        while current.len() > 1 {
            let mut next = Vec::with_capacity((current.len() + 1) / 2);
            for chunk in current.chunks(2) {
                if chunk.len() == 2 {
                    let mut data = Vec::with_capacity(64);
                    data.extend_from_slice(chunk[0].as_ref());
                    data.extend_from_slice(chunk[1].as_ref());
                    next.push(Hash::new_keyed(Hash::DOMAIN_MERKLE, &data));
                } else {
                    next.push(chunk[0]);
                }
            }
            current = next;
        }
        current[0]
    }
}

/// Simplified Block alias used in higher layers.
pub type Block = StatementBlock;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::Address;
    use crate::crypto::Signature;

    fn dummy_tx(hash: Hash) -> Transaction {
        Transaction {
            sender: Address([0; 32]),
            nonce: 0,
            kind: crate::transaction::TransactionKind::Transfer { to: Address([1; 32]), amount: 100 },
            fee: 0,
            signature: Signature([0; 64]),
            hash,
        }
    }

    #[test]
    fn merkle_empty_is_zero() {
        let block = StatementBlock {
            author: 0,
            round: 0,
            parents: vec![],
            transactions: vec![],
            statements: vec![],
            signature: Signature([0; 64]),
            digest: Hash::zero(),
        };
        assert_eq!(block.merkle_root(), Hash::zero());
    }

    #[test]
    fn merkle_single_is_hash() {
        let h = Hash::new_keyed(Hash::DOMAIN_TX, b"tx1");
        let block = StatementBlock {
            author: 0,
            round: 1,
            parents: vec![],
            transactions: vec![dummy_tx(h)],
            statements: vec![],
            signature: Signature([0; 64]),
            digest: Hash::zero(),
        };
        assert_eq!(block.merkle_root(), h);
    }

    #[test]
    fn merkle_three_transactions() {
        let h1 = Hash::new_keyed(Hash::DOMAIN_TX, b"tx-a");
        let h2 = Hash::new_keyed(Hash::DOMAIN_TX, b"tx-b");
        let h3 = Hash::new_keyed(Hash::DOMAIN_TX, b"tx-c");
        let block = StatementBlock {
            author: 0,
            round: 2,
            parents: vec![],
            transactions: vec![dummy_tx(h1), dummy_tx(h2), dummy_tx(h3)],
            statements: vec![],
            signature: Signature([0; 64]),
            digest: Hash::zero(),
        };
        let root = block.merkle_root();
        assert_ne!(root, Hash::zero());
        assert_eq!(block.merkle_root(), root);
    }

    #[test]
    fn domain_separation_block_vs_tx() {
        let data = b"same-data";
        let block_hash = Hash::new_keyed(Hash::DOMAIN_BLOCK, data);
        let tx_hash = Hash::new_keyed(Hash::DOMAIN_TX, data);
        assert_ne!(block_hash, tx_hash, "domain tags must separate hashes");
    }

    #[test]
    fn domain_separation_digest_vs_merkle() {
        let data = b"pair";
        let d = Hash::new_keyed(Hash::DOMAIN_DIGEST, data);
        let m = Hash::new_keyed(Hash::DOMAIN_MERKLE, data);
        assert_ne!(d, m);
    }
}
