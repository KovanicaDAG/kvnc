//! Account address type.

use crate::crypto::PublicKey;
use serde::{Deserialize, Serialize};
use std::fmt;

/// 32-byte account address.
///
/// KVNC uses an **identity** address model: an account address is exactly the
/// 32 raw bytes of the owner's Ed25519 public key. This is what allows
/// [`crate::Transaction::verify_signature`] to authenticate a transaction from
/// its `sender` field alone, with no state lookup.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct Address(pub [u8; 32]);

impl Address {
    /// Derive an address from a public key.
    ///
    /// Identity mapping: `address.0 == public_key.0`. Must not hash — see the
    /// type-level note above.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_is_identity_of_public_key_bytes() {
        // The address model is identity: proof that `verify_signature` can treat
        // the sender address as the signer's public key.
        for seed in 0u8..8 {
            let pk = PublicKey::from_bytes([seed; 32]);
            let addr = Address::from_public_key(&pk);
            assert_eq!(addr.0, pk.0);
            assert_eq!(&addr.0, pk.as_bytes());
        }
    }

    #[test]
    fn from_public_key_is_deterministic() {
        let pk = PublicKey::from_bytes([42u8; 32]);
        assert_eq!(Address::from_public_key(&pk), Address::from_public_key(&pk));
    }
}
