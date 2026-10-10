//! Committee and Authority definitions.

use crate::crypto::PublicKey;
use crate::Address;
use serde::{Deserialize, Serialize};

/// Stake amount (in smallest units of KUNA).
pub type Stake = u64;

/// Single authority (validator).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Authority {
    /// Index of this authority in the committee.
    pub index: u16,
    /// Ed25519 public key for block signing.
    pub public_key: PublicKey,
    /// Account address (derived from public key).
    pub address: Address,
    /// Stake amount in base units.
    pub stake: Stake,
    /// Network address (multiaddr or host:port) for P2P.
    pub network_address: String,
}

/// Current committee (validator set).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Committee {
    /// Epoch number for this committee.
    pub epoch: u64,
    /// List of authorities in this committee.
    pub authorities: Vec<Authority>,
    /// Total stake of all authorities.
    pub total_stake: Stake,
    /// Quorum threshold (2f+1 stake).
    pub quorum_threshold: Stake,
    /// Validity threshold (f+1 stake).
    pub validity_threshold: Stake,
}

impl Committee {
    /// Create a new committee and compute thresholds.
    pub fn new(epoch: u64, authorities: Vec<Authority>) -> Self {
        let total_stake: Stake = authorities.iter().map(|a| a.stake).sum();
        // Classic BFT: quorum = 2f+1 → floor(2/3 * total) + 1
        let quorum_threshold = (total_stake * 2) / 3 + 1;
        let validity_threshold = total_stake / 3 + 1;

        Self {
            epoch,
            authorities,
            total_stake,
            quorum_threshold,
            validity_threshold,
        }
    }

    /// Return the number of authorities in the committee.
    pub fn size(&self) -> usize {
        self.authorities.len()
    }

    /// Look up an authority by its index.
    pub fn get_by_index(&self, index: u16) -> Option<&Authority> {
        self.authorities.iter().find(|a| a.index == index)
    }

    /// Stake-weighted leader election (simple version).
    pub fn leader(&self, round: u64) -> u16 {
        let seed = round;
        // Simple deterministic selection – replace with VRF later
        let idx = (seed as usize) % self.authorities.len();
        self.authorities[idx].index
    }
}
