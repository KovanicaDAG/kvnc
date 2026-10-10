//! Stateless admission checks applied at the network edge.
//!
//! Every gossip message is validated here before gossipsub is allowed to
//! forward it to the rest of the mesh (`validate_messages` is on, see
//! [`crate::behaviour`]). The checks are deliberately *stateless*: they only
//! answer "is this message authentic and internally consistent?". Whether a
//! vote counts or a block is acceptable for the DAG is still decided by the
//! consensus engine / block manager. Nothing here changes a consensus rule.
//!
//! * Votes: the voter must be a committee member and the Ed25519 signature
//!   must verify over [`Vote::signature_data`] under that member's key.
//! * Blocks: the digest is recomputed from the content, the merkle root must
//!   match the transactions, the author must be a committee member and the
//!   signature must verify over the digest under the author's key.

use kvnc_types::{block::StatementBlock, AuthorityIndex, PublicKey, Vote};
use std::{collections::HashMap, fmt, sync::RwLock};

/// Committee public keys indexed by authority index.
pub type AuthorityKeys = HashMap<AuthorityIndex, PublicKey>;

/// Why a gossip message failed edge validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// The voter / block author is not in the configured committee.
    UnknownAuthority(AuthorityIndex),
    /// The signature does not verify under the authority's committee key.
    BadSignature(AuthorityIndex),
    /// The block digest does not match its recomputed content digest.
    DigestMismatch,
    /// The block merkle root does not match its transactions.
    MerkleMismatch,
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Rejection::UnknownAuthority(index) => write!(f, "unknown authority {index}"),
            Rejection::BadSignature(index) => write!(f, "bad signature from authority {index}"),
            Rejection::DigestMismatch => write!(f, "block digest does not match its content"),
            Rejection::MerkleMismatch => {
                write!(f, "block merkle root does not match its transactions")
            }
        }
    }
}

/// Verify that `vote` is signed by its claimed voter under `keys`.
pub fn verify_vote(
    keys: &AuthorityKeys,
    vote: &Vote,
    ctx: &kvnc_types::SigningContext,
) -> Result<(), Rejection> {
    let key = keys
        .get(&vote.voter)
        .ok_or(Rejection::UnknownAuthority(vote.voter))?;
    kvnc_crypto::verify(key, &vote.signature_data(ctx), &vote.signature)
        .map_err(|_| Rejection::BadSignature(vote.voter))
}

/// Verify a block's digest, merkle root and author signature under `keys`.
///
/// The digest is recomputed first, so a valid signature cannot be replayed
/// over modified transactions or parents.
pub fn verify_block(keys: &AuthorityKeys, block: &StatementBlock) -> Result<(), Rejection> {
    let digest = StatementBlock::compute_digest(
        block.author,
        block.round,
        &block.parents,
        &block.transactions,
    );
    if digest != block.digest {
        return Err(Rejection::DigestMismatch);
    }
    if StatementBlock::compute_merkle_root(&block.transactions) != block.merkle_root {
        return Err(Rejection::MerkleMismatch);
    }
    let key = keys
        .get(&block.author)
        .ok_or(Rejection::UnknownAuthority(block.author))?;
    kvnc_crypto::verify_block_signature(key, &block.digest, &block.signature)
        .map_err(|_| Rejection::BadSignature(block.author))
}

/// Holder for the committee keys used by the edge checks.
///
/// Until keys are configured nothing can be authenticated, so votes and
/// blocks are neither delivered nor forwarded (fail closed).
#[derive(Default)]
pub(crate) struct GossipValidator {
    keys: RwLock<AuthorityKeys>,
}

impl GossipValidator {
    /// Replace the committee keys.
    pub(crate) fn set_keys(&self, keys: AuthorityKeys) {
        *self.keys.write().unwrap_or_else(|e| e.into_inner()) = keys;
    }

    /// `true` once at least one committee key is configured.
    pub(crate) fn is_configured(&self) -> bool {
        !self
            .keys
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }

    pub(crate) fn verify_vote(
        &self,
        vote: &Vote,
        ctx: &kvnc_types::SigningContext,
    ) -> Result<(), Rejection> {
        verify_vote(
            &self.keys.read().unwrap_or_else(|e| e.into_inner()),
            vote,
            ctx,
        )
    }

    pub(crate) fn verify_block(&self, block: &StatementBlock) -> Result<(), Rejection> {
        verify_block(&self.keys.read().unwrap_or_else(|e| e.into_inner()), block)
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    pub(crate) const TEST_CTX: kvnc_types::SigningContext =
        kvnc_types::SigningContext::new(kvnc_types::signing::chain_id::LOCAL);
    use kvnc_types::{Hash, Signature};

    /// Deterministic committee of `n` keys (seed byte = index + 1).
    pub(crate) fn committee(n: u16) -> (Vec<kvnc_types::SigningKey>, AuthorityKeys) {
        let mut signers = Vec::new();
        let mut keys = AuthorityKeys::new();
        for index in 0..n {
            let signer = kvnc_types::SigningKey::from_bytes(&[index as u8 + 1; 32]);
            keys.insert(index, PublicKey::from(signer.verifying_key()));
            signers.push(signer);
        }
        (signers, keys)
    }

    pub(crate) fn signed_vote(signer: &kvnc_types::SigningKey, voter: u16) -> Vote {
        let mut vote = Vote {
            leader_round: 3,
            leader_hash: Hash::new(b"leader"),
            voter,
            signature: Signature([0; 64]),
        };
        vote.signature = kvnc_crypto::sign(signer, &vote.signature_data(&TEST_CTX));
        vote
    }

    pub(crate) fn signed_block(signer: &kvnc_types::SigningKey, author: u16) -> StatementBlock {
        let digest = StatementBlock::compute_digest(author, 1, &[], &[]);
        StatementBlock {
            author,
            round: 1,
            parents: Vec::new(),
            transactions: Vec::new(),
            statements: Vec::new(),
            signature: kvnc_crypto::sign(signer, digest.as_ref()),
            digest,
            merkle_root: StatementBlock::compute_merkle_root(&[]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use kvnc_types::{Hash, Signature};

    #[test]
    fn valid_vote_is_accepted() {
        let (signers, keys) = committee(4);
        assert_eq!(
            verify_vote(&keys, &signed_vote(&signers[2], 2), &TEST_CTX),
            Ok(())
        );
    }

    #[test]
    fn forged_vote_signature_is_rejected() {
        let (signers, keys) = committee(4);
        let mut vote = signed_vote(&signers[1], 1);
        vote.signature = Signature([7; 64]);
        assert_eq!(
            verify_vote(&keys, &vote, &TEST_CTX),
            Err(Rejection::BadSignature(1))
        );
    }

    #[test]
    fn vote_signed_by_another_validator_is_rejected() {
        let (signers, keys) = committee(4);
        // Validator 0 signs but claims to be validator 3.
        let mut vote = signed_vote(&signers[0], 3);
        vote.signature = kvnc_crypto::sign(&signers[0], &vote.signature_data(&TEST_CTX));
        assert_eq!(
            verify_vote(&keys, &vote, &TEST_CTX),
            Err(Rejection::BadSignature(3))
        );
    }

    #[test]
    fn tampered_vote_payload_is_rejected() {
        let (signers, keys) = committee(4);
        let mut vote = signed_vote(&signers[1], 1);
        vote.leader_hash = Hash::new(b"other leader");
        assert_eq!(
            verify_vote(&keys, &vote, &TEST_CTX),
            Err(Rejection::BadSignature(1))
        );
        let mut vote = signed_vote(&signers[1], 1);
        vote.leader_round += 1;
        assert_eq!(
            verify_vote(&keys, &vote, &TEST_CTX),
            Err(Rejection::BadSignature(1))
        );
    }

    #[test]
    fn vote_from_non_member_is_rejected() {
        let (signers, keys) = committee(4);
        let vote = signed_vote(&signers[0], 9);
        assert_eq!(
            verify_vote(&keys, &vote, &TEST_CTX),
            Err(Rejection::UnknownAuthority(9))
        );
    }

    #[test]
    fn valid_block_is_accepted() {
        let (signers, keys) = committee(4);
        assert_eq!(verify_block(&keys, &signed_block(&signers[1], 1)), Ok(()));
    }

    #[test]
    fn block_with_replayed_signature_over_new_content_is_rejected() {
        let (signers, keys) = committee(4);
        let original = signed_block(&signers[1], 1);
        // Keep digest + signature, change the content.
        let mut tampered = original.clone();
        tampered.round = 2;
        assert_eq!(
            verify_block(&keys, &tampered),
            Err(Rejection::DigestMismatch)
        );
    }

    #[test]
    fn block_with_wrong_merkle_root_is_rejected() {
        let (signers, keys) = committee(4);
        let mut block = signed_block(&signers[1], 1);
        block.merkle_root = Hash::new(b"bogus");
        assert_eq!(verify_block(&keys, &block), Err(Rejection::MerkleMismatch));
    }

    #[test]
    fn block_with_bad_signature_or_unknown_author_is_rejected() {
        let (signers, keys) = committee(4);
        let mut block = signed_block(&signers[1], 1);
        block.signature = Signature([0; 64]);
        assert_eq!(verify_block(&keys, &block), Err(Rejection::BadSignature(1)));

        let block = signed_block(&signers[0], 7);
        assert_eq!(
            verify_block(&keys, &block),
            Err(Rejection::UnknownAuthority(7))
        );
    }

    #[test]
    fn unconfigured_validator_rejects_everything() {
        let (signers, _) = committee(1);
        let validator = GossipValidator::default();
        assert!(!validator.is_configured());
        assert!(validator
            .verify_vote(&signed_vote(&signers[0], 0), &TEST_CTX)
            .is_err());
        assert!(validator
            .verify_block(&signed_block(&signers[0], 0))
            .is_err());
    }
}
