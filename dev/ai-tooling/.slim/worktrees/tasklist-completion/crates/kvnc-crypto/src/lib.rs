//! Cryptographic operations for KVNC.
//! Signing, verification, key generation.

#![deny(unsafe_code)]

use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use kvnc_types::crypto::{PublicKey, Signature};
use kvnc_types::hash::Hash;
use rand::rngs::OsRng;
use thiserror::Error;

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

/// Error returned by [`verify_batch`].
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum BatchVerifyError {
    /// The batch contained no entries.
    #[error("batch verification requires at least one entry")]
    Empty,
    /// One or more entries failed verification, identified by position.
    #[error("signature verification failed at indices {0:?}")]
    InvalidSignatures(Vec<usize>),
}

/// Verify a batch of Ed25519 signatures.
///
/// Each entry is a `(public_key, message, signature)` triple. The batch
/// verifies only if **every** entry is valid; otherwise the error lists the
/// indices of the entries that failed, so callers can attribute the failure to
/// a specific block/vote author.
///
/// # Batch strategy
///
/// This helper verifies each entry independently and aggregates the results.
/// It is the correctness-oriented batch API for KVNC: it always evaluates every
/// entry (no short-circuit that would let one bad signature hide others) and
/// returns all failures. Randomized multi-scalar batch verification
/// (`ed25519_dalek::VerifyingKey::verify_batch`) is deliberately **not** used
/// here because its `batch` feature pulls in the `merlin` dependency, which is
/// not present in the workspace lockfile; enabling it is a candidate follow-up
/// once the dependency is approved.
pub fn verify_batch(entries: &[(&PublicKey, &[u8], &Signature)]) -> Result<(), BatchVerifyError> {
    if entries.is_empty() {
        return Err(BatchVerifyError::Empty);
    }
    let mut failed = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let (public_key, message, signature) = *entry;
        if verify(public_key, message, signature).is_err() {
            failed.push(index);
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(BatchVerifyError::InvalidSignatures(failed))
    }
}

/// Verify a batch of block (or vote) signatures over `Hash` digests.
///
/// Convenience wrapper over [`verify_batch`] for the common case where the
/// signed message is a 32-byte digest such as a block digest or leader hash.
pub fn verify_block_signatures(
    entries: &[(&PublicKey, &Hash, &Signature)],
) -> Result<(), BatchVerifyError> {
    if entries.is_empty() {
        return Err(BatchVerifyError::Empty);
    }
    let mut failed = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        let (public_key, digest, signature) = *entry;
        if verify(public_key, digest.as_ref(), signature).is_err() {
            failed.push(i);
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(BatchVerifyError::InvalidSignatures(failed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Entry {
        public_key: PublicKey,
        message: Vec<u8>,
        signature: Signature,
    }

    fn valid_entries(n: usize) -> Vec<Entry> {
        (0..n)
            .map(|i| {
                let (sk, pk) = generate_keypair();
                let msg = format!("batch-message-{i}").into_bytes();
                let sig = sign(&sk, &msg);
                Entry {
                    public_key: pk,
                    message: msg,
                    signature: sig,
                }
            })
            .collect()
    }

    #[test]
    fn all_valid_batch_verifies() {
        let entries = valid_entries(6);
        let refs: Vec<(&PublicKey, &[u8], &Signature)> = entries
            .iter()
            .map(|e| (&e.public_key, e.message.as_slice(), &e.signature))
            .collect();
        assert_eq!(verify_batch(&refs), Ok(()));
    }

    #[test]
    fn batch_with_one_invalid_signature_fails_and_identifies_it() {
        let mut entries = valid_entries(4);
        // Corrupt the message of entry 2 so its signature no longer matches.
        entries[2].message.extend_from_slice(b"-tampered");
        let refs: Vec<(&PublicKey, &[u8], &Signature)> = entries
            .iter()
            .map(|e| (&e.public_key, e.message.as_slice(), &e.signature))
            .collect();
        assert_eq!(
            verify_batch(&refs),
            Err(BatchVerifyError::InvalidSignatures(vec![2]))
        );
    }

    #[test]
    fn batch_with_multiple_invalid_signatures_reports_all() {
        let mut entries = valid_entries(5);
        entries[0].message.push(0xff);
        entries[3].message.push(0xee);
        let refs: Vec<(&PublicKey, &[u8], &Signature)> = entries
            .iter()
            .map(|e| (&e.public_key, e.message.as_slice(), &e.signature))
            .collect();
        assert_eq!(
            verify_batch(&refs),
            Err(BatchVerifyError::InvalidSignatures(vec![0, 3]))
        );
    }

    #[test]
    fn empty_batch_is_rejected() {
        assert_eq!(verify_batch(&[]), Err(BatchVerifyError::Empty));
        assert_eq!(verify_block_signatures(&[]), Err(BatchVerifyError::Empty));
    }

    #[test]
    fn block_signature_batch_verifies_and_detects_bad_digest() {
        let digests: Vec<Hash> = (0..3u8).map(|i| Hash::for_block([i; 32])).collect();
        let owners: Vec<(SigningKey, PublicKey, Signature)> = digests
            .iter()
            .map(|d| {
                let (sk, pk) = generate_keypair();
                let sig = sign(&sk, d.as_ref());
                (sk, pk, sig)
            })
            .collect();

        let refs: Vec<(&PublicKey, &Hash, &Signature)> = owners
            .iter()
            .zip(digests.iter())
            .map(|((_, pk, sig), d)| (pk, d, sig))
            .collect();
        assert_eq!(verify_block_signatures(&refs), Ok(()));

        // Swap entry 1's digest: its signature must no longer verify.
        let bad_digest = Hash::for_block([0xAB; 32]);
        let bad_refs: Vec<(&PublicKey, &Hash, &Signature)> = owners
            .iter()
            .zip(digests.iter())
            .enumerate()
            .map(|(i, ((_, pk, sig), d))| {
                if i == 1 {
                    (pk, &bad_digest, sig)
                } else {
                    (pk, d, sig)
                }
            })
            .collect();
        assert_eq!(
            verify_block_signatures(&bad_refs),
            Err(BatchVerifyError::InvalidSignatures(vec![1]))
        );
    }
}
