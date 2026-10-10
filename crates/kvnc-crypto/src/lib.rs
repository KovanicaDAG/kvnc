//! Cryptographic operations for KVNC.
//! Signing, verification, key generation.

#![deny(unsafe_code)]

use ed25519_dalek::{verify_batch as dalek_verify_batch, Signature as DalekSignature};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use kvnc_types::crypto::{PublicKey, Signature};
use kvnc_types::hash::Hash;
use rand::rngs::OsRng;
use thiserror::Error;

use std::sync::RwLock;

static VALIDATOR_KEYS: RwLock<Vec<VerifyingKey>> = RwLock::new(Vec::new());

pub fn set_validator_keys(keys: Vec<VerifyingKey>) {
    *VALIDATOR_KEYS.write().unwrap() = keys;
}

/// Set validator keys from `kvnc_types::crypto::PublicKey` (converts to dalek VerifyingKey).
pub fn set_validator_keys_from_public(keys: Vec<PublicKey>) {
    let verifying_keys: Vec<VerifyingKey> = keys
        .into_iter()
        .map(|pk| VerifyingKey::from_bytes(&pk.0).expect("invalid validator public key"))
        .collect();
    *VALIDATOR_KEYS.write().unwrap() = verifying_keys;
}

fn get_validator_key(author: u16) -> Option<VerifyingKey> {
    let keys = VALIDATOR_KEYS.read().unwrap();
    keys.get(author as usize).cloned()
}

#[derive(Error, Debug)]
pub enum CryptoError {
    #[error("Invalid public key")]
    InvalidPublicKey,
    #[error("Invalid signature")]
    InvalidSignature,
    #[error("Signature verification failed")]
    VerificationFailed,
}

/// Generate a new ed25519 keypair.
pub fn generate_keypair() -> (SigningKey, PublicKey) {
    let signing_key = SigningKey::generate(&mut OsRng);
    let verifying_key = signing_key.verifying_key();
    let public_key = PublicKey::from(verifying_key);
    (signing_key, public_key)
}

/// Sign a message (usually a block digest or transaction hash).
pub fn sign(signing_key: &SigningKey, message: &[u8]) -> Signature {
    let sig = signing_key.sign(message);
    Signature::from(sig)
}

/// Verify a signature.
pub fn verify(
    public_key: &PublicKey,
    message: &[u8],
    signature: &Signature,
) -> Result<(), CryptoError> {
    let vk = VerifyingKey::from_bytes(&public_key.0).map_err(|_| CryptoError::InvalidPublicKey)?;
    let sig = ed25519_dalek::Signature::from_bytes(&signature.0);
    vk.verify(message, &sig)
        .map_err(|_| CryptoError::VerificationFailed)
}

/// Convenience: verify block signature.
pub fn verify_block_signature(
    public_key: &PublicKey,
    digest: &Hash,
    signature: &Signature,
) -> Result<(), CryptoError> {
    verify(public_key, digest.as_ref(), signature)
}

/// Errors returned by the vote signature API ([`verify_vote_signature`]).
#[derive(Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigError {
    /// The public key bytes are not a valid Ed25519 point (or are a weak /
    /// small-order key rejected by strict verification).
    #[error("invalid public key")]
    InvalidPublicKey,
    /// The signature does not verify over the vote's canonical bytes under
    /// the given public key.
    #[error("vote signature verification failed")]
    VerificationFailed,
}

/// Verify a consensus vote signature.
///
/// API contract (owned by Exec-Foundation):
/// - The message is exactly [`Vote::signature_data`](kvnc_types::Vote::signature_data),
///   the canonical vote encoding (`leader_round` u64 LE || `leader_hash`,
///   40 bytes). The same bytes must be passed to [`sign`] by the producer.
/// - `vote.signature` is checked with Ed25519 **strict** verification
///   (`VerifyingKey::verify_strict`): rejects non-canonical `S`, and weak /
///   small-order public keys. Signatures produced by [`sign`] always pass.
/// - The caller is responsible for mapping `vote.voter` to the right
///   committee `pubkey`; `voter` is *not* covered by the signature in the
///   current (provisional) format.
///
/// The vote byte format is **provisional** pending the format agreement led
/// by Main; this function will follow `Vote::signature_data` if that changes.
pub fn verify_vote_signature(vote: &kvnc_types::Vote, pubkey: &PublicKey) -> Result<(), SigError> {
    let vk = VerifyingKey::from_bytes(&pubkey.0).map_err(|_| SigError::InvalidPublicKey)?;
    let sig = DalekSignature::from_bytes(&vote.signature.0);
    vk.verify_strict(&vote.signature_data(), &sig)
        .map_err(|_| SigError::VerificationFailed)
}

/// Batch verify block signatures, grouped by round (wave).
/// Uses ed25519_dalek::verify_batch. Falls back to individual verify if batch fails.
/// Deterministic — same input always yields same result.
///
/// Returns `Ok(true)` iff every signature is valid; an invalid signature is a
/// hard error (`CryptoError::VerificationFailed`) so callers using `?`/`Err`
/// matching reject the block.
pub fn verify_batch(blocks: &[kvnc_types::block::StatementBlock]) -> Result<bool, CryptoError> {
    use std::collections::BTreeMap;

    if blocks.is_empty() {
        return Ok(true);
    }

    // Group by round / wave for deterministic ordering.
    let mut groups: BTreeMap<u64, Vec<&kvnc_types::block::StatementBlock>> = BTreeMap::new();
    for b in blocks {
        groups.entry(b.round).or_default().push(b);
    }

    for (_round, group) in groups {
        let signatures: Vec<DalekSignature> = group
            .iter()
            .map(|b| DalekSignature::from_bytes(&b.signature.0))
            .collect();
        let verifying_keys: Vec<VerifyingKey> = group
            .iter()
            .map(|b| get_validator_key(b.author).ok_or(CryptoError::InvalidPublicKey))
            .collect::<Result<Vec<_>, _>>()?;
        let msg_refs: Vec<&[u8]> = group.iter().map(|b| b.digest.as_ref()).collect();
        // Borrow signatures as slices for batch call; owned sigs kept for fallback.
        let batch_result = dalek_verify_batch(&msg_refs, &signatures, &verifying_keys);

        if batch_result.is_ok() {
            continue;
        }

        // Fallback to single verification.
        for b in group {
            let vk = get_validator_key(b.author).ok_or(CryptoError::InvalidPublicKey)?;
            let sig = DalekSignature::from_bytes(&b.signature.0);
            if vk.verify(b.digest.as_ref(), &sig).is_err() {
                return Err(CryptoError::VerificationFailed);
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use kvnc_types::block::StatementBlock;
    use kvnc_types::hash::Hash;
    use rand::rngs::OsRng;

    #[test]
    fn verify_batch_4_signatures() {
        let mut csprng = OsRng;
        let mut keys: Vec<(SigningKey, VerifyingKey)> = Vec::with_capacity(4);
        for _ in 0..4 {
            let sk = SigningKey::generate(&mut csprng);
            keys.push((sk.clone(), sk.verifying_key()));
        }

        set_validator_keys(keys.iter().map(|(_, vk)| *vk).collect());

        let msg = b"batch-test-4";
        let mut blocks: Vec<StatementBlock> = Vec::with_capacity(4);
        for (i, (sk, _vk)) in keys.iter().enumerate() {
            let digest = Hash::new_keyed(Hash::DOMAIN_BLOCK, msg);
            let sig = sk.sign(digest.as_ref());
            blocks.push(StatementBlock {
                author: i as u16,
                round: (i as u64) % 2,
                parents: vec![],
                transactions: vec![],
                statements: vec![],
                signature: Signature::from(sig),
                digest,
                merkle_root: Default::default(),
            });
        }
        assert!(verify_batch(&blocks).unwrap());
    }
}

#[cfg(test)]
mod vote_sig_tests {
    use super::*;
    use kvnc_types::hash::Hash;
    use kvnc_types::Vote;

    fn signed_vote(sk: &SigningKey) -> Vote {
        let mut vote = Vote {
            leader_round: 42,
            leader_hash: Hash::new("leader-block"),
            voter: 3,
            signature: Signature([0u8; 64]),
        };
        vote.signature = sign(sk, &vote.signature_data());
        vote
    }

    #[test]
    fn valid_vote_signature_ok() {
        let (sk, pk) = generate_keypair();
        let vote = signed_vote(&sk);
        assert_eq!(verify_vote_signature(&vote, &pk), Ok(()));
    }

    #[test]
    fn tampered_round_fails() {
        let (sk, pk) = generate_keypair();
        let mut vote = signed_vote(&sk);
        vote.leader_round += 1;
        assert_eq!(
            verify_vote_signature(&vote, &pk),
            Err(SigError::VerificationFailed)
        );
    }

    #[test]
    fn tampered_hash_fails() {
        let (sk, pk) = generate_keypair();
        let mut vote = signed_vote(&sk);
        vote.leader_hash.0[0] ^= 0x01;
        assert_eq!(
            verify_vote_signature(&vote, &pk),
            Err(SigError::VerificationFailed)
        );
    }

    #[test]
    fn tampered_signature_fails() {
        let (sk, pk) = generate_keypair();
        let mut vote = signed_vote(&sk);
        vote.signature.0[10] ^= 0x01;
        assert_eq!(
            verify_vote_signature(&vote, &pk),
            Err(SigError::VerificationFailed)
        );
    }

    #[test]
    fn wrong_key_fails() {
        let (sk, _pk) = generate_keypair();
        let (_other_sk, other_pk) = generate_keypair();
        let vote = signed_vote(&sk);
        assert_eq!(
            verify_vote_signature(&vote, &other_pk),
            Err(SigError::VerificationFailed)
        );
    }

    #[test]
    fn invalid_public_key_rejected() {
        let (sk, _pk) = generate_keypair();
        let vote = signed_vote(&sk);
        // y = 2 does not decompress to a curve point.
        let mut bad = [0u8; 32];
        bad[0] = 2;
        let res = verify_vote_signature(&vote, &PublicKey(bad));
        assert!(
            matches!(
                res,
                Err(SigError::InvalidPublicKey) | Err(SigError::VerificationFailed)
            ),
            "got {res:?}"
        );
    }

    #[test]
    fn small_order_public_key_rejected() {
        // Identity point (small order): must never verify under strict mode.
        let mut identity = [0u8; 32];
        identity[0] = 1;
        let vote = Vote {
            leader_round: 1,
            leader_hash: Hash::new("x"),
            voter: 0,
            signature: Signature([0u8; 64]),
        };
        assert!(verify_vote_signature(&vote, &PublicKey(identity)).is_err());
    }
}
