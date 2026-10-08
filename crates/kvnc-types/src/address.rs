//! Account address type.

use crate::crypto::PublicKey;
use serde::{Deserialize, Serialize};
use std::fmt;

/// 32-byte account address (derived from public key).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct Address(pub [u8; 32]);

impl Address {
    /// Derive address from a public key.
    ///
    /// KVNC uses the raw 32-byte Ed25519 public key as the account address so
    /// that [`crate::transaction::Transaction::verify_signature`] can verify the
    /// signature directly against `sender` (which it treats as the public key).
    pub fn from_public_key(pk: &PublicKey) -> Self {
        Self(pk.0)
    }

    /// Return the address as a hex string.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Bech32-style human readable (simplified for now).
    pub fn to_bech32(&self) -> String {
        format!("kvnc1{}", &self.to_hex()[..40])
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_bech32())
    }
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Address({})", self.to_hex())
    }
}
