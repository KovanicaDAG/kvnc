//! Native transaction types.

use crate::address::Address;
use crate::crypto::Signature;
use crate::hash::Hash;
use serde::{Deserialize, Serialize};

/// Kind of native transaction.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TransactionKind {
    /// Simple transfer of KVNC.
    Transfer { to: Address, amount: u64 },
    /// Stake tokens.
    Stake { amount: u64 },
    /// Unstake tokens.
    Unstake { amount: u64 },
    /// Deploy a WASM contract.
    Deploy {
        code: Vec<u8>,
        // init args later
    },
    /// Call a WASM contract.
    Call {
        contract: Address,
        method: String,
        args: Vec<u8>,
        gas_limit: u64,
    },
}

/// Full signed transaction.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transaction {
    pub sender: Address,
    pub nonce: u64,
    pub kind: TransactionKind,
    pub fee: u64,
    pub signature: Signature,
    /// Cached hash.
    pub hash: Hash,
}

impl Transaction {
    pub fn hash(&self) -> Hash {
        self.hash
    }
}
