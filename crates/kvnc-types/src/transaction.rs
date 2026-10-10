//! Native transaction types.

use crate::address::Address;
use crate::crypto::Signature;
use crate::hash::Hash;
use serde::{Deserialize, Serialize};

/// Kind of native transaction.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum TransactionKind {
    /// Simple transfer of KUNA.
    Transfer {
        /// Recipient address.
        to: Address,
        /// Amount in base units.
        amount: u64,
    },
    /// Stake tokens (self-stake for validator).
    Stake {
        /// Amount to stake in base units.
        amount: u64,
    },
    /// Unstake tokens (begin unbonding).
    Unstake {
        /// Amount to unstake in base units.
        amount: u64,
    },
    /// Delegate stake to a validator.
    Delegate {
        /// Validator address to delegate to.
        validator: Address,
        /// Amount to delegate in base units.
        amount: u64,
    },
    /// Claim delegation rewards.
    ClaimRewards {
        /// Validator whose rewards to claim (optional, claims all if None).
        validator: Option<Address>,
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
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
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

    /// Compute the signing hash for this transaction (excluding signature).
    pub fn signing_hash(&self) -> Hash {
        let mut data = Vec::new();
        data.extend_from_slice(&self.sender.0);
        data.extend_from_slice(&self.nonce.to_le_bytes());
        // Serialize kind
        match &self.kind {
            TransactionKind::Transfer { to, amount } => {
                data.push(0);
                data.extend_from_slice(&to.0);
                data.extend_from_slice(&amount.to_le_bytes());
            }
            TransactionKind::Stake { amount } => {
                data.push(1);
                data.extend_from_slice(&amount.to_le_bytes());
            }
            TransactionKind::Unstake { amount } => {
                data.push(2);
                data.extend_from_slice(&amount.to_le_bytes());
            }
            TransactionKind::Delegate { validator, amount } => {
                data.push(5);
                data.extend_from_slice(&validator.0);
                data.extend_from_slice(&amount.to_le_bytes());
            }
            TransactionKind::ClaimRewards { validator } => {
                data.push(6);
                if let Some(v) = validator {
                    data.push(1);
                    data.extend_from_slice(&v.0);
                } else {
                    data.push(0);
                }
            }
            TransactionKind::Deploy { code } => {
                data.push(3);
                data.extend_from_slice(&(code.len() as u64).to_le_bytes());
                data.extend_from_slice(code);
            }
            TransactionKind::Call {
                contract,
                method,
                args,
                gas_limit,
            } => {
                data.push(4);
                data.extend_from_slice(&contract.0);
                data.extend_from_slice(&(method.len() as u64).to_le_bytes());
                data.extend_from_slice(method.as_bytes());
                data.extend_from_slice(&(args.len() as u64).to_le_bytes());
                data.extend_from_slice(args);
                data.extend_from_slice(&gas_limit.to_le_bytes());
            }
        }
        data.extend_from_slice(&self.fee.to_le_bytes());
        Hash::new_keyed(Hash::DOMAIN_TX, &data)
    }

    /// Verify the transaction signature against the sender's public key.
    pub fn verify_signature(&self) -> bool {
        // The sender address is the public key (32 bytes)
        let public_key = crate::crypto::PublicKey(self.sender.0);
        self.signature.verify(&self.signing_hash().0, &public_key)
    }
}
