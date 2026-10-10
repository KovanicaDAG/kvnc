//! BLAKE3-based hash type.

use blake3::Hasher;
use serde::{Deserialize, Serialize};
use std::fmt;

/// 32-byte BLAKE3 hash.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct Hash(pub [u8; 32]);

impl Hash {
    /// Compute BLAKE3 hash of the given data.
    pub fn new(data: impl AsRef<[u8]>) -> Self {
        let mut hasher = Hasher::new();
        hasher.update(data.as_ref());
        Self(*hasher.finalize().as_bytes())
    }

    /// Compute BLAKE3 hash with a domain key (domain separation).
    pub fn new_keyed(domain: &[u8], data: impl AsRef<[u8]>) -> Self {
        let mut key = [0u8; 32];
        let len = domain.len().min(32);
        key[..len].copy_from_slice(&domain[..len]);
        let mut hasher = Hasher::new_keyed(&key);
        hasher.update(data.as_ref());
        Self(*hasher.finalize().as_bytes())
    }

    /// Domain tag for block digests.
    pub const DOMAIN_BLOCK: &'static [u8] = b"KVNC-BLOCK-v1";
    /// Domain tag for transactions.
    pub const DOMAIN_TX: &'static [u8] = b"KUNA-TX-v1";
    /// Domain tag for digest/merkle pairing.
    pub const DOMAIN_DIGEST: &'static [u8] = b"KVNC-DIGEST-v1";
    /// Domain tag for merkle tree pairing.
    pub const DOMAIN_MERKLE: &'static [u8] = b"KVNC-MERKLE-v1";

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
