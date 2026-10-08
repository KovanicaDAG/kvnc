//! Account address type.

use crate::crypto::PublicKey;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Human-readable prefix of a canonical KVNC address string.
pub const ADDRESS_PREFIX: &str = "kvnc";
/// Human-readable suffix of a canonical KVNC address string.
pub const ADDRESS_SUFFIX: &str = "dag";
/// Size of the address payload in bytes (raw public key).
const ADDRESS_PAYLOAD_LEN: usize = 32;
/// Size of the trailing blake3 checksum in bytes.
const ADDRESS_CHECKSUM_LEN: usize = 4;

/// Errors produced while parsing an [`Address`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AddressError {
    /// The string did not start with [`ADDRESS_PREFIX`] and end with
    /// [`ADDRESS_SUFFIX`].
    #[error("address must start with `{ADDRESS_PREFIX}` and end with `{ADDRESS_SUFFIX}`")]
    MissingAffix,
    /// The payload was not valid hexadecimal.
    #[error("address payload is not valid hex")]
    InvalidHex,
    /// The decoded payload had the wrong length.
    #[error("address payload must be {ADDRESS_PAYLOAD_LEN} bytes, got {0}")]
    InvalidLength(usize),
    /// The trailing checksum did not match the payload.
    #[error("address checksum mismatch")]
    ChecksumMismatch,
}

/// 32-byte account address (raw Ed25519 public key).
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

    /// Return the address as a bare hex string (no affixes).
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Canonical human-readable encoding: `kvnc<hex>dag` where `<hex>` is the
    /// 32-byte payload followed by its 4-byte blake3 checksum.
    pub fn encode(&self) -> String {
        let mut bytes = [0u8; ADDRESS_PAYLOAD_LEN + ADDRESS_CHECKSUM_LEN];
        bytes[..ADDRESS_PAYLOAD_LEN].copy_from_slice(&self.0);
        bytes[ADDRESS_PAYLOAD_LEN..].copy_from_slice(&Self::checksum(&self.0));
        format!("{ADDRESS_PREFIX}{}{ADDRESS_SUFFIX}", hex::encode(bytes))
    }

    /// Decode a canonical `kvnc<hex>dag` string, verifying the checksum.
    pub fn decode(value: &str) -> Result<Self, AddressError> {
        let body = value
            .strip_prefix(ADDRESS_PREFIX)
            .and_then(|rest| rest.strip_suffix(ADDRESS_SUFFIX))
            .ok_or(AddressError::MissingAffix)?;
        let bytes = hex::decode(body).map_err(|_| AddressError::InvalidHex)?;
        if bytes.len() != ADDRESS_PAYLOAD_LEN + ADDRESS_CHECKSUM_LEN {
            return Err(AddressError::InvalidLength(bytes.len()));
        }
        let mut payload = [0u8; ADDRESS_PAYLOAD_LEN];
        payload.copy_from_slice(&bytes[..ADDRESS_PAYLOAD_LEN]);
        if bytes[ADDRESS_PAYLOAD_LEN..] != Self::checksum(&payload) {
            return Err(AddressError::ChecksumMismatch);
        }
        Ok(Self(payload))
    }

    fn checksum(payload: &[u8; ADDRESS_PAYLOAD_LEN]) -> [u8; ADDRESS_CHECKSUM_LEN] {
        let digest = blake3::hash(payload);
        let mut checksum = [0u8; ADDRESS_CHECKSUM_LEN];
        checksum.copy_from_slice(&digest.as_bytes()[..ADDRESS_CHECKSUM_LEN]);
        checksum
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.encode())
    }
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Address({})", self.to_hex())
    }
}

impl FromStr for Address {
    type Err = AddressError;

    /// Parse either the canonical `kvnc<hex>dag` form (checksum verified) or a
    /// bare/`0x`-prefixed 32-byte hex string for backward compatibility.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        if value.starts_with(ADDRESS_PREFIX) || value.ends_with(ADDRESS_SUFFIX) {
            return Self::decode(value);
        }
        let raw = value.strip_prefix("0x").unwrap_or(value);
        let bytes = hex::decode(raw).map_err(|_| AddressError::InvalidHex)?;
        if bytes.len() != ADDRESS_PAYLOAD_LEN {
            return Err(AddressError::InvalidLength(bytes.len()));
        }
        let mut payload = [0u8; ADDRESS_PAYLOAD_LEN];
        payload.copy_from_slice(&bytes);
        Ok(Self(payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Address {
        let mut bytes = [0u8; 32];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = i as u8;
        }
        Address(bytes)
    }

    #[test]
    fn canonical_encoding_round_trips() {
        let address = sample();
        let encoded = address.encode();
        assert!(encoded.starts_with(ADDRESS_PREFIX));
        assert!(encoded.ends_with(ADDRESS_SUFFIX));
        assert_eq!(Address::decode(&encoded).unwrap(), address);
        assert_eq!(encoded.parse::<Address>().unwrap(), address);
        assert_eq!(address.to_string(), encoded);
    }

    #[test]
    fn checksum_rejects_tampering() {
        let address = sample();
        let mut encoded = address.encode();
        // Flip the last payload hex nibble (just before the checksum + suffix).
        let flip_at = encoded.len() - ADDRESS_SUFFIX.len() - (ADDRESS_CHECKSUM_LEN * 2) - 1;
        let replacement = if &encoded[flip_at..flip_at + 1] == "0" {
            "1"
        } else {
            "0"
        };
        encoded.replace_range(flip_at..flip_at + 1, replacement);
        assert!(matches!(
            Address::decode(&encoded),
            Err(AddressError::ChecksumMismatch)
        ));
    }

    #[test]
    fn affix_and_length_errors() {
        assert_eq!(Address::decode("deadbeef"), Err(AddressError::MissingAffix));
        assert!(matches!(
            Address::decode("kvnc00dag"),
            Err(AddressError::InvalidLength(_))
        ));
        assert!(matches!(
            Address::decode("kvncZZdag"),
            Err(AddressError::InvalidHex)
        ));
    }

    #[test]
    fn raw_hex_is_accepted_for_backward_compatibility() {
        let address = sample();
        let hex = address.to_hex();
        assert_eq!(hex.parse::<Address>().unwrap(), address);
        assert_eq!(format!("0x{hex}").parse::<Address>().unwrap(), address);
        assert!(matches!(
            "0x1234".parse::<Address>(),
            Err(AddressError::InvalidLength(_))
        ));
    }

    #[test]
    fn display_debug_are_distinct() {
        let address = sample();
        assert_eq!(
            format!("{address:?}"),
            format!("Address({})", address.to_hex())
        );
        assert_eq!(format!("{address}"), address.encode());
    }
}
