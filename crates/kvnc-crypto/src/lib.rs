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
