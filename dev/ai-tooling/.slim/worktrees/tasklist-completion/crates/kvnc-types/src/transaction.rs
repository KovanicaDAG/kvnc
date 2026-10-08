//! Native transaction types.

use crate::address::Address;
use crate::crypto::Signature;
use crate::hash::Hash;
use serde::{Deserialize, Serialize};

/// Kind of native transaction.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
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

    /// Encode the *signable* fields (everything except `signature` and the
    /// cached `hash`) into the canonical, deterministic byte layout.
    ///
    /// This is the single source of truth for the transaction signing payload.
    /// Every signer and verifier (CLI, node, mempool, execution, DAG validation)
    /// MUST derive the signing digest through this function, or through
    /// [`Transaction::signing_hash`] / [`Transaction::signing_hash_from`], so
    /// that signatures stay coherent across the system.
    pub fn signable_bytes(
        sender: &Address,
        nonce: u64,
        kind: &TransactionKind,
        fee: u64,
    ) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&sender.0);
        data.extend_from_slice(&nonce.to_le_bytes());
        // Serialize kind
        match kind {
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
        data.extend_from_slice(&fee.to_le_bytes());
        data
    }

    /// Compute the signing hash from raw transaction fields (without needing a
    /// full [`Transaction`]).
    ///
    /// Uses the transaction domain tag so the value cannot collide with a
    /// block, state or address digest.
    pub fn signing_hash_from(
        sender: &Address,
        nonce: u64,
        kind: &TransactionKind,
        fee: u64,
    ) -> Hash {
        Hash::for_transaction(Self::signable_bytes(sender, nonce, kind, fee))
    }

    /// Compute the signing hash for this transaction (excluding signature).
    ///
    /// Uses the transaction domain tag so the value cannot collide with a
    /// block, state or address digest.
    pub fn signing_hash(&self) -> Hash {
        Self::signing_hash_from(&self.sender, self.nonce, &self.kind, self.fee)
    }

    /// Verify the transaction signature against the sender's public key.
    pub fn verify_signature(&self) -> bool {
        // The sender address is the public key (32 bytes)
        let public_key = crate::crypto::PublicKey(self.sender.0);
        self.signature.verify(&self.signing_hash().0, &public_key)
    }
}
