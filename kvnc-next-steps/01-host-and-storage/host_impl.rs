//! Skeleton: Real Host implementation for kvnc contracts
//!
//! Place this (or adapt it) inside your execution / runtime crate.
//! Goal: make HTLC, Vault, Multisig, Token actually runnable.

use kvnc_common::{Address, Amount, ContractError, ContractResult, Height, Host, Timestamp};

/// Concrete host that talks to the real kvnc execution environment.
/// Replace the TODO bodies with calls to your existing Wasmi host functions,
/// balance table, storage backend, and event emitter.
pub struct KvncHost<'a> {
    // TODO: hold references to:
    // - current caller
    // - block height / timestamp
    // - balance map or account state
    // - storage backend (redb / whatever)
    // - event log
    _phantom: core::marker::PhantomData<&'a ()>,
}

impl<'a> KvncHost<'a> {
    pub fn new(/* pass real runtime context here */) -> Self {
        Self {
            _phantom: core::marker::PhantomData,
        }
    }
}

impl Host for KvncHost<'_> {
    fn caller(&self) -> Address {
        // TODO: return the address that invoked the contract
        [0u8; 32]
    }

    fn block_height(&self) -> Height {
        // TODO: current committed height or round
        0
    }

    fn timestamp(&self) -> Timestamp {
        // TODO: host-provided unix timestamp
        0
    }

    fn balance_of(&self, _addr: &Address) -> Amount {
        // TODO: read from account state
        0
    }

    fn transfer(
        &mut self,
        _from: &Address,
        _to: &Address,
        _amount: Amount,
    ) -> ContractResult<()> {
        // TODO: debit / credit, check sufficient balance
        // return Err(ContractError::InsufficientBalance) on failure
        Ok(())
    }

    fn emit_event(&mut self, _topic: &[u8], _data: &[u8]) {
        // TODO: push into the execution event log
    }

    fn storage_get(&self, _key: &[u8]) -> Option<Vec<u8>> {
        // TODO: read from contract storage (prefix by contract address)
        None
    }

    fn storage_set(&mut self, _key: &[u8], _value: &[u8]) {
        // TODO: write to contract storage
    }
}

// ---------------------------------------------------------------------------
// Storage helpers for the four contracts
// ---------------------------------------------------------------------------

/// Example key layout (deterministic, collision-free)
pub mod keys {
    use kvnc_common::Address;

    pub fn htlc(id: &[u8; 32]) -> Vec<u8> {
        let mut k = b"htlc:".to_vec();
        k.extend_from_slice(id);
        k
    }

    pub fn vault(id: &[u8; 32]) -> Vec<u8> {
        let mut k = b"vault:".to_vec();
        k.extend_from_slice(id);
        k
    }

    pub fn multisig(id: &[u8; 32]) -> Vec<u8> {
        let mut k = b"msig:".to_vec();
        k.extend_from_slice(id);
        k
    }

    pub fn multisig_tx(id: &[u8; 32], tx_id: u64) -> Vec<u8> {
        let mut k = b"msigtx:".to_vec();
        k.extend_from_slice(id);
        k.extend_from_slice(&tx_id.to_le_bytes());
        k
    }

    pub fn token_state(contract: &Address) -> Vec<u8> {
        let mut k = b"token:".to_vec();
        k.extend_from_slice(contract);
        k
    }
}

/// TODO: add serialize / deserialize helpers that match the format
/// already used in kvnc (bincode, scale, custom, etc.)
