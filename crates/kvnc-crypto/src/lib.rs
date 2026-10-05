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
