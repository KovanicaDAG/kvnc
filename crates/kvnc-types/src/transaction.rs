//! Native transaction types.

use crate::address::Address;
use crate::crypto::Signature;
use crate::hash::Hash;
use serde::{Deserialize, Serialize};

/// Kind of native transaction.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TransactionKind {
    /// Simple transfer of KVNC.
    Transfer {
        /// Recipient address.
        to: Address,
        /// Amount in base units.
        amount: u64,
    },
    /// Stake tokens.
    Stake {
        /// Amount to stake in base units.
        amount: u64,
    },
    /// Unstake tokens.
    Unstake {
        /// Amount to unstake in base units.
        amount: u64,
    },
    /// Deploy a WASM contract.
    Deploy {
        /// WASM bytecode.
        code: Vec<u8>,
        // init args later
    },
    /// Call a WASM contract.
    Call {
        /// Contract address.
        contract: Address,
        /// Method name to call.
        method: String,
        /// Encoded arguments.
        args: Vec<u8>,
        /// Gas limit for execution.
        gas_limit: u64,
    },
}

/// Full signed transaction.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transaction {
    /// Sender address.
    pub sender: Address,
    /// Sender nonce (prevents replay).
    pub nonce: u64,
    /// Transaction kind (transfer, stake, call, etc.).
    pub kind: TransactionKind,
    /// Fee in base units.
    pub fee: u64,
    /// Ed25519 signature over the transaction hash.
    pub signature: Signature,
    /// Cached hash of the transaction.
    pub hash: Hash,
}

impl Transaction {
    /// Return the cached transaction hash.
    pub fn hash(&self) -> Hash {
        self.hash
    }
}
