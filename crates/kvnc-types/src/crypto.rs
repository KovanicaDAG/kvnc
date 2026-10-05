//! Cryptographic primitives re-exports and wrappers.

use ed25519_dalek::{Signature as DalekSignature, VerifyingKey};
use serde::de::Visitor;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// Ed25519 public key wrapper.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct PublicKey(pub [u8; 32]);

impl PublicKey {
    /// Return the raw 32-byte public key.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Create a PublicKey from raw 32 bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

/// Ed25519 signature wrapper.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Signature(pub [u8; 64]);

impl Signature {
    /// Return the raw 64-byte signature.
    pub fn as_bytes(&self) -> &[u8; 64] {
        &self.0
    }
}

impl Serialize for Signature {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for Signature {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct SigVisitor;
        impl<'de> Visitor<'de> for SigVisitor {
            type Value = Signature;
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("64-byte Ed25519 signature")
            }
            fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if v.len() != 64 {
                    return Err(E::invalid_length(v.len(), &"64 bytes"));
                }
                let mut arr = [0u8; 64];
                arr.copy_from_slice(v);
                Ok(Signature(arr))
            }
        }
        deserializer.deserialize_bytes(SigVisitor)
    }
}

// Convenience conversions
impl From<VerifyingKey> for PublicKey {
    fn from(vk: VerifyingKey) -> Self {
        Self(vk.to_bytes())
    }
}

impl From<DalekSignature> for Signature {
    fn from(sig: DalekSignature) -> Self {
        Self(sig.to_bytes())
    }
}
