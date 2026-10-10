//! Signing context for signature format v1 (see `docs/SIGNATURE_FORMAT.md`).
//!
//! Every signed message (vote or transaction) commits to the network
//! (`chain_id`). Votes also commit to the committee `epoch`. The verifier must
//! build the context from its **own** configuration, never from a network
//! message.

use serde::{Deserialize, Serialize};

/// Registered `chain_id` values (see the registry in `docs/SIGNATURE_FORMAT.md`).
pub mod chain_id {
    /// Mainnet.
    pub const MAINNET: u64 = 1;
    /// Public testnet.
    pub const TESTNET: u64 = 2;
    /// Devnet.
    pub const DEVNET: u64 = 3;
    /// Local development network.
    pub const LOCAL: u64 = 1337;

    /// Whether `id` is in the registry.
    pub fn is_registered(id: u64) -> bool {
        matches!(id, MAINNET | TESTNET | DEVNET | LOCAL)
    }
}

/// Domain tag (16 bytes, `0x00`-padded) of vote `signature_data` v1.
pub const VOTE_DOMAIN_TAG: [u8; 16] = domain_tag16(b"KUNA/vote/v1");
/// Domain tag (16 bytes, `0x00`-padded) of the transaction signing preimage v1.
pub const TX_DOMAIN_TAG: [u8; 16] = domain_tag16(b"KUNA/tx/v1");

/// `DomainTag16(s)`: ASCII `s` right-padded with `0x00` to exactly 16 bytes.
///
/// Panics at compile time (const evaluation) if `s` is longer than 15 bytes, so
/// every tag ends in at least one `0x00`.
pub const fn domain_tag16(s: &[u8]) -> [u8; 16] {
    assert!(s.len() <= 15, "domain tag must be at most 15 bytes");
    let mut out = [0u8; 16];
    let mut i = 0;
    while i < s.len() {
        out[i] = s[i];
        i += 1;
    }
    out
}

/// Network context every v1 signature commits to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SigningContext {
    /// Network identifier (`u64` LE in the signed bytes). See [`chain_id`].
    pub chain_id: u64,
    /// Committee epoch (`0` until epochs are live). Only votes commit to it.
    pub epoch: u64,
}

impl SigningContext {
    /// Context for `chain_id` at epoch `0`.
    pub const fn new(chain_id: u64) -> Self {
        Self { chain_id, epoch: 0 }
    }

    /// Same network, given committee epoch.
    pub const fn with_epoch(self, epoch: u64) -> Self {
        Self { epoch, ..self }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_16_bytes_zero_padded() {
        assert_eq!(&VOTE_DOMAIN_TAG[..12], b"KUNA/vote/v1");
        assert!(VOTE_DOMAIN_TAG[12..].iter().all(|b| *b == 0));
        assert_eq!(&TX_DOMAIN_TAG[..10], b"KUNA/tx/v1");
        assert!(TX_DOMAIN_TAG[10..].iter().all(|b| *b == 0));
    }

    #[test]
    fn registry() {
        for id in [1, 2, 3, 1337] {
            assert!(chain_id::is_registered(id));
        }
        assert!(!chain_id::is_registered(0));
        assert!(!chain_id::is_registered(4));
    }
}
