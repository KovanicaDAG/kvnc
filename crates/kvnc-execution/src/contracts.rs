//! Contract dispatch layer — the real execution path for the four contract
//! crates (`kvnc-htlc`, `kvnc-vault`, `kvnc-multisig`, `kvnc-token`).
//!
//! Two paths share one state model:
//!
//! - **Native dispatch** ([`execute_contract_call`]): bincode-decodes the
//!   fixed ABI argument tuple, calls the crate's typed API directly through a
//!   [`ContractHost`], bincode-encodes the Ok result. Byte-for-byte identical
//!   results to the wasm path (both encode the same values with the same
//!   `bincode` configuration).
//! - **Wasmi dispatch** ([`ContractRunner::execute_wasm_call`]): compiles the
//!   crate's `wasm32-unknown-unknown` cdylib artifact and runs the same entry
//!   name through [`kvnc_runtime::Runtime`], whose `env` imports are backed by
//!   the *same* [`ContractHost`].
//!
//! ## `ContractHost` semantics (FIXED — read before changing)
//!
//! A `ContractHost` is created per call and holds:
//!
//! - an immutable context: contract address, caller, block height, timestamp;
//! - **one redb read snapshot** ([`redb::ReadTransaction`]) covering the whole
//!   call, so repeated reads are repeatable even if another writer commits
//!   mid-call (redb is MVCC: read transactions may coexist with write
//!   transactions — verified against redb 2.6 source: `begin_read` documents
//!   "Read transactions may exist concurrently with writes", and
//!   `start_write_transaction` only waits for other *write* transactions);
//! - an **in-memory overlay**: pending storage writes, touched accounts, and
//!   emitted events.
//!
//! Reads consult the overlay first, then the snapshot. Storage writes are
//! **namespaced by contract address**: the table key is
//! `(contract_addr, blake3(raw_key))` — contracts pass their raw (variable
//! length) keys, the host derives the fixed 32-byte table key with
//! [`kvnc_common::hash`], so two contracts can use the same raw key without
//! collision.
//!
//! **Commit rules:**
//!
//! - [`ContractHost::commit`] is called *only* after the entry point returned
//!   `Ok`. It flushes the overlay into a single redb write transaction
//!   (snapshot dropped first) and returns the emitted events.
//! - On a contract error (`ContractError`) the host is simply dropped:
//!   nothing — no storage write, no balance delta, no event — is committed.
//! - A failed backing read sets a *sticky* read error; `commit` then refuses
//!   to flush, so a transient storage failure can never silently corrupt a
//!   contract record (the call surfaces as [`ExecutionError::Other`]).
//!
//! Native balances are moved through the `ACCOUNTS` table
//! (`kvnc_storage::state_store::Account`); `amount > u64::MAX` maps to
//! `ContractError::Overflow`, a balance shortfall to `InsufficientBalance`.
//!
//! **Spend authorization:** [`Host::transfer`] only accepts
//! `from == contract address`; any other `from` (the caller, a third party,
//! a derived escrow address) fails with `ContractError::Unauthorized` and
//! nothing is moved. No native allowance mechanism exists yet, so flows that
//! pull funds from the caller or from per-record escrow addresses
//! (`htlc_*`, `vault_*`) are rejected until one is specified.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

use kvnc_common::{ContractError, ContractResult, Host};
use kvnc_htlc::Htlc;
use kvnc_multisig::Multisig;
use kvnc_runtime::{ExecutionConfig, Runtime};
use kvnc_storage::state_store::Account;
use kvnc_storage::{Storage, StorageError};
use kvnc_token::Token;
use kvnc_types::Address;
use kvnc_vault::Vault;
use redb::ReadTransaction;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tracing::debug;

use crate::ExecutionError;

/// The 16 dispatchable contract entry points (FIXED ABI, Lane 3a).
///
/// The native [`execute_contract_call`] dispatcher, the wasm exports of the
/// four contract crates, and `docs/CONTRACTS.md` must stay in sync with this
/// table (enforced by `dispatch_covers_all_16_entry_points`).
pub const ENTRY_POINTS: &[&str] = &[
    "htlc_create",
    "htlc_claim",
    "htlc_refund",
    "vault_create",
    "vault_claim",
    "vault_cancel",
    "multisig_create",
    "multisig_propose",
    "multisig_confirm",
    "multisig_execute",
    "token_create",
    "token_transfer",
    "token_approve",
    "token_transfer_from",
    "token_mint",
    "token_burn",
];

/// Returns whether `name` is one of the 16 dispatchable entry points.
pub fn is_entry_point(name: &str) -> bool {
    ENTRY_POINTS.contains(&name)
}

/// One event emitted during a contract call. Returned by
/// [`ContractHost::commit`] — events of a failed call are discarded with
/// everything else.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContractEvent {
    /// Event topic (e.g. `b"htlc_claimed"`).
    pub topic: Vec<u8>,
    /// Event payload.
    pub data: Vec<u8>,
}

/// Host implementation over persistent [`Storage`] with a per-call overlay.
///
/// See the module docs for the exact semantics (namespacing, snapshot,
/// commit rules).
pub struct ContractHost<'a> {
    storage: &'a Storage,
    contract: Address,
    caller: Address,
    block_height: u64,
    timestamp: u64,
    /// Read snapshot held for the whole call (repeatable reads).
    reads: ReadTransaction,
    /// Raw contract key → value; the contract address is applied at commit.
    storage_writes: BTreeMap<Vec<u8>, Vec<u8>>,
    /// Accounts touched by `transfer`/`balance_of` (balance possibly
    /// mutated), keyed by the raw 32-byte address (`kvnc_types::Address`
    /// itself does not implement `Ord`).
    accounts: BTreeMap<[u8; 32], Account>,
    events: Vec<ContractEvent>,
    /// First backing-read failure; when set, [`ContractHost::commit`] refuses
    /// to flush so a failed read can never be mistaken for "absent".
    read_error: RefCell<Option<String>>,
}

impl<'a> ContractHost<'a> {
    /// Open a host for one call against `storage`.
    ///
    /// The redb read snapshot is opened eagerly; a failure here aborts the
    /// call before any contract code runs.
    pub fn new(
        storage: &'a Storage,
        contract: Address,
        caller: Address,
        block_height: u64,
        timestamp: u64,
    ) -> Result<Self, ExecutionError> {
        let reads = storage.begin_read()?;
        Ok(Self {
            storage,
            contract,
            caller,
            block_height,
            timestamp,
            reads,
            storage_writes: BTreeMap::new(),
            accounts: BTreeMap::new(),
            events: Vec::new(),
            read_error: RefCell::new(None),
        })
    }

    /// Fixed table key for a raw contract key: `blake3(raw_key)`.
    fn namespace(raw_key: &[u8]) -> [u8; 32] {
        kvnc_common::hash(raw_key)
    }

    fn record_read_error(&self, message: String) {
        let mut slot = self.read_error.borrow_mut();
        if slot.is_none() {
            *slot = Some(message);
        }
    }

    /// Load an account through the overlay (touched accounts win) or the
    /// read snapshot. Records a sticky error on backing-read failure.
    fn load_account(&self, address: &Address) -> Result<Account, ContractError> {
        if let Some(account) = self.accounts.get(&address.0) {
            return Ok(account.clone());
        }
        match self
            .storage
            .state()
            .get_account_or_default(&self.reads, address)
        {
            Ok(account) => Ok(account),
            Err(err) => {
                self.record_read_error(format!("account read failed for {address:?}: {err}"));
                // Sticky error blocks commit; this code only aborts the call.
                Err(ContractError::Custom(500))
            }
        }
    }

    /// Events emitted so far (moved out; the host keeps working).
    pub fn take_events(&mut self) -> Vec<ContractEvent> {
        std::mem::take(&mut self.events)
    }

    /// Commit the overlay to persistent storage and return the emitted
    /// events.
    ///
    /// Called **only** on contract success. Steps:
    ///
    /// 1. Refuse if a backing read failed (sticky error).
    /// 2. Drop the read snapshot (redb allows a read transaction to stay open
    ///    across a write transaction, but the snapshot is released promptly
    ///    so it cannot pin freed pages any longer than needed).
    /// 3. Flush storage writes and touched accounts into ONE write
    ///    transaction; commit.
    pub fn commit(self) -> Result<Vec<ContractEvent>, ExecutionError> {
        if let Some(message) = self.read_error.borrow().clone() {
            return Err(ExecutionError::Other(format!(
                "contract call discarded, nothing committed: {message}"
            )));
        }
        let ContractHost {
            storage,
            contract,
            reads,
            storage_writes,
            accounts,
            events,
            ..
        } = self;
        drop(reads);
        let txn = storage.begin_write()?;
        for (raw_key, value) in storage_writes {
            storage
                .state()
                .set_storage(&txn, &contract, Self::namespace(&raw_key), value)?;
        }
        for (address, account) in accounts {
            storage
                .state()
                .set_account(&txn, &Address(address), &account)?;
        }
        txn.commit().map_err(StorageError::Commit)?;
        Ok(events)
    }
}

impl Host for ContractHost<'_> {
    fn caller(&self) -> [u8; 32] {
        self.caller.0
    }

    fn contract_address(&self) -> [u8; 32] {
        self.contract.0
    }

    fn block_height(&self) -> u64 {
        self.block_height
    }

    fn timestamp(&self) -> u64 {
        self.timestamp
    }

    fn balance_of(&self, addr: &[u8; 32]) -> u128 {
        let address = Address(*addr);
        match self.load_account(&address) {
            Ok(account) => u128::from(account.balance),
            // Sticky error is already recorded; contract sees 0 and the call
            // is discarded at commit.
            Err(_) => 0,
        }
    }

    fn transfer(&mut self, from: &[u8; 32], to: &[u8; 32], amount: u128) -> ContractResult<()> {
        // Spend authorization: a contract may only move native funds out of
        // its OWN account. Neither the caller's account nor any other address
        // (including derived escrow addresses) is spendable: there is no
        // native allowance/approval mechanism, and signing a `Call` is not an
        // approval of an amount. Rejected before any account is loaded, so
        // the overlay is untouched.
        if *from != self.contract.0 {
            return Err(ContractError::Unauthorized);
        }
        if amount > u64::MAX as u128 {
            return Err(ContractError::Overflow);
        }
        let amount = amount as u64;
        let from = Address(*from);
        let to = Address(*to);

        // Load both sides through the overlay before mutating anything.
        let mut sender = self.load_account(&from)?;
        let mut recipient = if to == from {
            // Self-transfer: validated below, no state change.
            sender.clone()
        } else {
            self.load_account(&to)?
        };

        if sender.balance < amount {
            return Err(ContractError::InsufficientBalance);
        }
        if from == to {
            return Ok(());
        }
        sender.balance -= amount;
        recipient.balance = recipient
            .balance
            .checked_add(amount)
            .ok_or(ContractError::Overflow)?;
        self.accounts.insert(from.0, sender);
        self.accounts.insert(to.0, recipient);
        Ok(())
    }

    fn emit_event(&mut self, topic: &[u8], data: &[u8]) {
        self.events.push(ContractEvent {
            topic: topic.to_vec(),
            data: data.to_vec(),
        });
    }

    fn storage_get(&self, key: &[u8]) -> Option<Vec<u8>> {
        if let Some(value) = self.storage_writes.get(key) {
            return Some(value.clone());
        }
        match self
            .storage
            .state()
            .get_storage(&self.reads, &self.contract, &Self::namespace(key))
        {
            Ok(value) => value,
            Err(err) => {
                self.record_read_error(format!("storage read failed: {err}"));
                // Treated as "absent" by the contract; sticky error blocks
                // commit, so absence is never trusted after a failed read.
                None
            }
        }
    }

    fn storage_set(&mut self, key: &[u8], value: &[u8]) {
        self.storage_writes.insert(key.to_vec(), value.to_vec());
    }
}

// ---------------------------------------------------------------------------
// Native dispatch (canonical path)
// ---------------------------------------------------------------------------

/// Execute one contract entry point natively and commit on success.
///
/// - Contract error ([`ContractError`]) → [`ExecutionError::Contract`],
///   **nothing committed** (the [`ContractHost`] is dropped uncommitted).
/// - Corrupt argument bytes → [`ExecutionError::Contract`] with
///   `InvalidInput`, mirroring what the wasm entry point returns for the same
///   input.
/// - Unknown entry name → [`ExecutionError::Other`].
/// - On success the overlay is committed and the bincode-encoded Ok result is
///   returned (identical bytes to the wasm path).
pub fn execute_contract_call(
    entry: &str,
    contract: Address,
    caller: Address,
    height: u64,
    timestamp: u64,
    args: &[u8],
    storage: &Storage,
) -> Result<Vec<u8>, ExecutionError> {
    let mut host = ContractHost::new(storage, contract, caller, height, timestamp)?;
    let out = dispatch_entry(entry, &mut host, args)?;
    let events = host.commit()?;
    log_events(&events);
    Ok(out)
}

/// Decode bincode arguments; failure maps to the same error the wasm entry
/// point reports for corrupt arguments (`InvalidInput`).
fn dec<T: DeserializeOwned>(args: &[u8]) -> Result<T, ExecutionError> {
    bincode::deserialize(args).map_err(|_| ExecutionError::Contract(ContractError::InvalidInput))
}

/// Encode an Ok result exactly like the wasm entry points (`bincode` of the
/// same value; `()` encodes to zero bytes, matching the null success return).
fn enc<T: Serialize>(value: &T) -> Result<Vec<u8>, ExecutionError> {
    bincode::serialize(value).map_err(|err| ExecutionError::Other(format!("encode failed: {err}")))
}

/// Match `entry` against [`ENTRY_POINTS`] and run the typed API.
fn dispatch_entry(
    entry: &str,
    host: &mut ContractHost<'_>,
    args: &[u8],
) -> Result<Vec<u8>, ExecutionError> {
    match entry {
        // --- kvnc-htlc -----------------------------------------------------
        "htlc_create" => {
            let (claimer, amount, hash_lock, expiry): (
                [u8; 32],
                kvnc_common::Amount,
                [u8; 32],
                kvnc_common::Timestamp,
            ) = dec(args)?;
            enc(&Htlc::create(host, claimer, amount, hash_lock, expiry)?)
        }
        "htlc_claim" => {
            let (id, preimage): ([u8; 32], Vec<u8>) = dec(args)?;
            Htlc::claim(host, id, &preimage)?;
            enc(&())
        }
        "htlc_refund" => {
            let id: [u8; 32] = dec(args)?;
            Htlc::refund(host, id)?;
            enc(&())
        }

        // --- kvnc-vault ----------------------------------------------------
        "vault_create" => {
            let (beneficiary, amount, schedule): (
                [u8; 32],
                kvnc_common::Amount,
                kvnc_vault::VestingSchedule,
            ) = dec(args)?;
            enc(&Vault::create(host, beneficiary, amount, schedule)?)
        }
        "vault_claim" => {
            let id: kvnc_vault::VaultId = dec(args)?;
            enc(&Vault::claim(host, id)?)
        }
        "vault_cancel" => {
            let id: kvnc_vault::VaultId = dec(args)?;
            Vault::cancel(host, id)?;
            enc(&())
        }

        // --- kvnc-multisig -------------------------------------------------
        "multisig_create" => {
            let (owners, threshold): (Vec<[u8; 32]>, u32) = dec(args)?;
            enc(&Multisig::create(host, owners, threshold)?)
        }
        "multisig_propose" => {
            let (id, to, amount, data): (
                kvnc_multisig::MultisigId,
                [u8; 32],
                kvnc_common::Amount,
                Vec<u8>,
            ) = dec(args)?;
            enc(&Multisig::propose(host, id, to, amount, data)?)
        }
        "multisig_confirm" => {
            let (id, tx_id): (kvnc_multisig::MultisigId, kvnc_multisig::TxId) = dec(args)?;
            Multisig::confirm(host, id, tx_id)?;
            enc(&())
        }
        "multisig_execute" => {
            let (id, tx_id): (kvnc_multisig::MultisigId, kvnc_multisig::TxId) = dec(args)?;
            Multisig::execute(host, id, tx_id)?;
            enc(&())
        }

        // --- kvnc-token ----------------------------------------------------
        "token_create" => {
            let (name, symbol, decimals, initial_supply): (
                String,
                String,
                u8,
                kvnc_common::Amount,
            ) = dec(args)?;
            Token::create(host, name, symbol, decimals, initial_supply)?;
            enc(&())
        }
        "token_transfer" => {
            let (to, amount): ([u8; 32], kvnc_common::Amount) = dec(args)?;
            Token::transfer(host, to, amount)?;
            enc(&())
        }
        "token_approve" => {
            let (spender, amount): ([u8; 32], kvnc_common::Amount) = dec(args)?;
            Token::approve(host, spender, amount)?;
            enc(&())
        }
        "token_transfer_from" => {
            let (from, to, amount): ([u8; 32], [u8; 32], kvnc_common::Amount) = dec(args)?;
            Token::transfer_from(host, from, to, amount)?;
            enc(&())
        }
        "token_mint" => {
            let (to, amount): ([u8; 32], kvnc_common::Amount) = dec(args)?;
            Token::mint(host, to, amount)?;
            enc(&())
        }
        "token_burn" => {
            let amount: kvnc_common::Amount = dec(args)?;
            Token::burn(host, amount)?;
            enc(&())
        }

        other => Err(ExecutionError::Other(format!(
            "unknown contract entry point: {other}"
        ))),
    }
}

fn log_events(events: &[ContractEvent]) {
    for event in events {
        debug!(
            target: "kvnc-execution",
            topic = %String::from_utf8_lossy(&event.topic),
            data_len = event.data.len(),
            "contract event"
        );
    }
}

// ---------------------------------------------------------------------------
// Wasmi dispatch
// ---------------------------------------------------------------------------

/// Parameters for one wasm contract call. Bundled in a struct because the
/// call has more than the usual number of inputs (clippy
/// `too_many_arguments`).
pub struct WasmCall<'a> {
    /// Raw wasm bytes of the contract (compiled once per runner and cached
    /// under `blake3(wasm)`).
    pub wasm: &'a [u8],
    /// Entry point name — must be one of [`ENTRY_POINTS`].
    pub entry: &'a str,
    /// Contract address the call runs against (storage namespace).
    pub contract: Address,
    /// Caller address (`kvnc_caller`).
    pub caller: Address,
    /// Block height (`kvnc_block_height`).
    pub height: u64,
    /// Timestamp (`kvnc_timestamp`).
    pub timestamp: u64,
    /// Bincode arguments (same encoding as the native path).
    pub args: &'a [u8],
    /// Persistent storage backing the [`ContractHost`].
    pub storage: &'a Storage,
    /// Gas / memory limits for this call.
    pub config: &'a ExecutionConfig,
}

/// Runs contract entry points, either natively or through the wasmi runtime.
///
/// Holds the compiled-module cache; native calls ignore it (the typed API is
/// linked directly). The dispatch decision is explicit: use
/// [`ContractRunner::execute_contract_call`] for the native path and
/// [`ContractRunner::execute_wasm_call`] for the wasm path — there is no
/// automatic fallback, so a wasm bug cannot silently change ledger behavior.
pub struct ContractRunner {
    runtime: Runtime,
    /// Compiled modules keyed by `blake3(wasm)`.
    /// TODO: persist across process restarts (currently per-runner, in
    /// memory only).
    modules: HashMap<[u8; 32], kvnc_runtime::Module>,
}

impl ContractRunner {
    pub fn new() -> Self {
        Self {
            runtime: Runtime::new(),
            modules: HashMap::new(),
        }
    }

    /// Native dispatch — thin wrapper over [`execute_contract_call`]
    /// (kept so callers can hold a runner instance and share its module
    /// cache across wasm calls).
    // The wrapper deliberately mirrors the free function's fixed 7-argument
    // contract ABI signature; `&self` makes 8 and trips the default lint
    // threshold. Bundling into a struct would duplicate `WasmCall` for a
    // one-line delegation.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_contract_call(
        &self,
        entry: &str,
        contract: Address,
        caller: Address,
        height: u64,
        timestamp: u64,
        args: &[u8],
        storage: &Storage,
    ) -> Result<Vec<u8>, ExecutionError> {
        execute_contract_call(entry, contract, caller, height, timestamp, args, storage)
    }

    /// Execute `call.entry` inside the wasm module via [`kvnc_runtime::Runtime`].
    ///
    /// Commit semantics match the native path: the [`ContractHost`] is
    /// committed only when the runtime returns `Ok` (including
    /// `Ok(empty)`); any [`kvnc_runtime::RuntimeError`] — contract rejection,
    /// out of gas, missing export, trap — drops the host uncommitted.
    ///
    /// Returns the output bytes and the emitted events.
    pub fn execute_wasm_call(
        &mut self,
        call: WasmCall<'_>,
    ) -> Result<(Vec<u8>, Vec<ContractEvent>), ExecutionError> {
        let key = kvnc_common::hash(call.wasm);
        if !self.modules.contains_key(&key) {
            let module = self.runtime.compile(call.wasm)?;
            self.modules.insert(key, module);
        }
        let module = &self.modules[&key];

        let mut host = ContractHost::new(
            call.storage,
            call.contract,
            call.caller,
            call.height,
            call.timestamp,
        )?;
        // Runtime errors propagate before `commit` — nothing is persisted.
        let out = self
            .runtime
            .execute(module, call.entry, call.args, call.config, &mut host)?;
        let events = host.commit()?;
        log_events(&events);
        Ok((out, events))
    }
}

impl Default for ContractRunner {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use kvnc_common::{Amount, ContractError};
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    /// Raw storage key of the kvnc-token singleton state (FIXED, see the
    /// kvnc-token header docs).
    const TOKEN_STATE_KEY: &[u8] = b"kvnc/v1/state";

    /// Walk up from this crate's manifest dir to the `[workspace]` Cargo.toml.
    ///
    /// `CARGO_MANIFEST_DIR` is `crates/kvnc-execution`, so a single `.parent()`
    /// would point at `crates/` and place the isolated wasm target dir in an
    /// un-ignored `crates/target/`. The root `target/` is git-ignored.
    fn workspace_root() -> PathBuf {
        let start = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut dir = start;
        loop {
            if let Ok(text) = std::fs::read_to_string(dir.join("Cargo.toml")) {
                if text.contains("[workspace]") {
                    return dir.to_path_buf();
                }
            }
            match dir.parent() {
                Some(parent) => dir = parent,
                None => return start.to_path_buf(),
            }
        }
    }

    fn open_storage() -> (TempDir, Storage) {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().join("test.redb")).expect("storage");
        (dir, storage)
    }

    fn addr(tag: u8) -> Address {
        Address([tag; 32])
    }

    fn fund(storage: &Storage, address: &Address, balance: u64) {
        let txn = storage.begin_write().expect("write txn");
        storage
            .state()
            .set_account(
                &txn,
                address,
                &Account {
                    balance,
                    nonce: 0,
                    code_hash: [0u8; 32],
                    code: Vec::new(),
                },
            )
            .expect("set_account");
        txn.commit().expect("commit");
    }

    fn native_balance(storage: &Storage, address: &Address) -> u64 {
        let txn = storage.begin_read().expect("read txn");
        storage
            .state()
            .get_account_or_default(&txn, address)
            .expect("account")
            .balance
    }

    /// Read the token singleton straight out of persistent storage (proves
    /// the committed bytes, independent of any dispatch path).
    fn token_state(storage: &Storage, contract: &Address) -> Token {
        let txn = storage.begin_read().expect("read txn");
        let key = kvnc_common::hash(TOKEN_STATE_KEY);
        let raw = storage
            .state()
            .get_storage(&txn, contract, &key)
            .expect("storage read")
            .expect("token singleton state after a successful call");
        bincode::deserialize(&raw).expect("token state deserializes")
    }

    fn token_create_args(supply: Amount) -> Vec<u8> {
        bincode::serialize(&("KVNC Test".to_string(), "TKT".to_string(), 9u8, supply))
            .expect("encode")
    }

    // (a) Native dispatch e2e: create → transfer, persisted across runner
    // instances, namespaced per contract address, contract error commits
    // nothing.
    #[test]
    fn native_token_dispatch_persists_across_runner_instances() {
        let (_dir, storage) = open_storage();
        let creator = addr(1);
        let recipient = addr(2);
        let contract_a = addr(10);
        let contract_b = addr(11);
        let create = token_create_args(1_000);

        // Runner instance 1 creates the token under contract A.
        let runner1 = ContractRunner::new();
        runner1
            .execute_contract_call(
                "token_create",
                contract_a,
                creator,
                1,
                1_000,
                &create,
                &storage,
            )
            .expect("token_create");

        // A FRESH runner instance must observe what instance 1 committed —
        // this is the persistence + commit proof.
        let runner2 = ContractRunner::new();
        let transfer = bincode::serialize(&(recipient.0, 400u128)).expect("encode");
        runner2
            .execute_contract_call(
                "token_transfer",
                contract_a,
                creator,
                2,
                1_100,
                &transfer,
                &storage,
            )
            .expect("token_transfer");

        let token = token_state(&storage, &contract_a);
        assert_eq!(token.balances.get(&creator.0), Some(&600));
        assert_eq!(token.balances.get(&recipient.0), Some(&400));
        assert_eq!(token.total_supply, 1_000);
        assert_eq!(token.owner, creator.0);

        // Namespacing: the same entry point under a different contract
        // address starts from a fresh singleton (it would fail with
        // AlreadyExists if state were shared across contracts).
        let runner3 = ContractRunner::new();
        runner3
            .execute_contract_call(
                "token_create",
                contract_b,
                creator,
                3,
                1_200,
                &create,
                &storage,
            )
            .expect("token_create under a second contract address");
        let token_b = token_state(&storage, &contract_b);
        assert_eq!(token_b.balances.get(&creator.0), Some(&1_000));
        // Contract A is untouched by B's writes.
        let token = token_state(&storage, &contract_a);
        assert_eq!(token.balances.get(&creator.0), Some(&600));

        // Contract error ⇒ nothing committed: an above-balance transfer must
        // fail and leave both balances exactly as they were. `creator` holds
        // 600 under contract A, so 700 is above balance.
        let over = bincode::serialize(&(recipient.0, 700u128)).expect("encode");
        let err = runner3
            .execute_contract_call(
                "token_transfer",
                contract_a,
                creator,
                4,
                1_300,
                &over,
                &storage,
            )
            .expect_err("transfer above the balance must fail");
        assert!(
            matches!(
                err,
                ExecutionError::Contract(ContractError::InsufficientBalance)
            ),
            "unexpected error: {err:?}"
        );
        let token = token_state(&storage, &contract_a);
        assert_eq!(token.balances.get(&creator.0), Some(&600));
        assert_eq!(token.balances.get(&recipient.0), Some(&400));
    }

    // (b) HTLC through native dispatch. `htlc_create` pulls funds from the
    // caller into a derived escrow address. Since `ContractHost::transfer`
    // only allows spending from the contract's own account (no native
    // allowance mechanism exists), the call is rejected and NOTHING moves.
    // When an explicit approval mechanism is specified, restore the full
    // create → claim balance-movement test here.
    #[test]
    fn native_htlc_create_rejected_without_spend_authorization() {
        let (_dir, storage) = open_storage();
        let sender = addr(1);
        let claimer = addr(2);
        let contract = addr(20);
        fund(&storage, &sender, 1_000);

        let preimage = b"kovanica-preimage".to_vec();
        let hash_lock = kvnc_common::hash(&preimage);
        let expiry = 1_000u64;

        let create = bincode::serialize(&(claimer.0, 500u128, hash_lock, expiry)).expect("encode");
        let err = execute_contract_call("htlc_create", contract, sender, 1, 100, &create, &storage)
            .expect_err("caller funds are not spendable by the contract");
        assert!(
            matches!(err, ExecutionError::Contract(ContractError::Unauthorized)),
            "unexpected error: {err:?}"
        );
        assert_eq!(native_balance(&storage, &sender), 1_000);
        assert_eq!(native_balance(&storage, &contract), 0);
    }

    // Regression: a contract could move funds out of ANY account through
    // `Host::transfer`. Draining a third party's account must fail with
    // `Unauthorized` and leave every balance unchanged, even after commit.
    #[test]
    fn contract_cannot_drain_foreign_account() {
        let (_dir, storage) = open_storage();
        let victim = addr(1);
        let caller = addr(2);
        let attacker = addr(3);
        let contract = addr(20);
        fund(&storage, &victim, 1_000);
        fund(&storage, &caller, 1_000);
        fund(&storage, &contract, 50);

        let mut host = ContractHost::new(&storage, contract, caller, 1, 100).expect("host");
        assert_eq!(
            host.transfer(&victim.0, &attacker.0, 1_000),
            Err(ContractError::Unauthorized),
            "third-party account must not be spendable"
        );
        assert_eq!(
            host.transfer(&caller.0, &attacker.0, 1_000),
            Err(ContractError::Unauthorized),
            "the caller's account is not implicitly approved either"
        );
        // The overlay was not touched by the rejected transfers.
        assert_eq!(host.balance_of(&victim.0), 1_000);
        assert_eq!(host.balance_of(&caller.0), 1_000);
        assert_eq!(host.balance_of(&attacker.0), 0);
        host.commit().expect("commit");

        assert_eq!(native_balance(&storage, &victim), 1_000);
        assert_eq!(native_balance(&storage, &caller), 1_000);
        assert_eq!(native_balance(&storage, &attacker), 0);
        assert_eq!(native_balance(&storage, &contract), 50);
    }

    // The contract may still spend its own balance (multisig payout path).
    #[test]
    fn contract_can_spend_own_account() {
        let (_dir, storage) = open_storage();
        let caller = addr(2);
        let recipient = addr(4);
        let contract = addr(20);
        fund(&storage, &contract, 500);

        let mut host = ContractHost::new(&storage, contract, caller, 1, 100).expect("host");
        host.transfer(&contract.0, &recipient.0, 200)
            .expect("own funds are spendable");
        assert_eq!(
            host.transfer(&contract.0, &recipient.0, 1_000),
            Err(ContractError::InsufficientBalance)
        );
        host.commit().expect("commit");

        assert_eq!(native_balance(&storage, &contract), 300);
        assert_eq!(native_balance(&storage, &recipient), 200);
    }

    // Every advertised entry point reaches the dispatcher: with an empty
    // argument buffer each one fails during argument decoding (InvalidInput),
    // never with "unknown entry point". This pins the native table to the
    // 16 names of the FIXED ABI.
    #[test]
    fn dispatch_covers_all_16_entry_points() {
        let (_dir, storage) = open_storage();
        let contract = addr(30);
        let caller = addr(1);
        assert_eq!(ENTRY_POINTS.len(), 16);
        assert_eq!(
            ENTRY_POINTS.iter().filter(|e| is_entry_point(e)).count(),
            16
        );
        for entry in ENTRY_POINTS {
            let err = execute_contract_call(entry, contract, caller, 1, 100, &[], &storage)
                .expect_err("empty arguments must fail decoding");
            assert!(
                matches!(err, ExecutionError::Contract(ContractError::InvalidInput)),
                "entry {entry}: unexpected error {err:?}"
            );
        }
        let err = execute_contract_call("does_not_exist", contract, caller, 1, 100, &[], &storage)
            .expect_err("unknown entry point");
        assert!(matches!(err, ExecutionError::Other(_)), "{err:?}");
        assert!(!is_entry_point("does_not_exist"));
    }

    // (d) Wasmi e2e: build kvnc-token for wasm32 inside the test (isolated
    // CARGO_TARGET_DIR), run token_create + token_transfer through the real
    // Runtime → env imports → ContractHost path, then prove the committed
    // bytes with the native path over the same storage.
    //
    // Skips gracefully (eprintln + return) when rustup or the wasm32 target
    // is unavailable; once the target is confirmed, a build failure is a real
    // failure and fails the test.
    //
    // NOTE (Lane 3a integration gap, out of this lane's scope): the `env`
    // externs in `kvnc-common/src/wasm.rs` are declared without
    // `#[link(wasm_import_module = "env")]`, so a plain `cargo build
    // --target wasm32-unknown-unknown` fails at link time with "undefined
    // symbol: kvnc_caller" (rust-lld does not auto-import bare externs).
    // The test works around it with `-C link-arg=--allow-undefined`, which
    // imports the undefined symbols from the default wasm module — `env` —
    // producing the exact import section the ABI requires (verified).
    // `cargo check --target wasm32-unknown-unknown` passes either way because
    // check does not link; the durable fix belongs in kvnc-common.
    #[test]
    fn wasmi_e2e_kvnc_token_wasm() {
        let workspace_root = workspace_root();

        // Skip: rustup missing or wasm32-unknown-unknown not installed.
        let probe = std::process::Command::new("rustup")
            .args(["target", "list", "--installed"])
            .output();
        let probe = match probe {
            Ok(output) if output.status.success() => output,
            _ => {
                eprintln!("SKIP wasmi_e2e_kvnc_token_wasm: rustup unavailable");
                return;
            }
        };
        let has_target = String::from_utf8_lossy(&probe.stdout)
            .lines()
            .any(|line| line.trim() == "wasm32-unknown-unknown");
        if !has_target {
            eprintln!(
                "SKIP wasmi_e2e_kvnc_token_wasm: wasm32-unknown-unknown target not installed"
            );
            return;
        }

        // Isolated target dir: the outer `cargo test` must not hold the lock
        // the subprocess build would wait on (target-lock deadlock).
        let target_dir = workspace_root.join("target").join("contract-wasm-e2e");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        let build = std::process::Command::new(&cargo)
            .current_dir(workspace_root)
            .env("CARGO_TARGET_DIR", &target_dir)
            // Lane 3a gap workaround — see the test doc comment above.
            .env("RUSTFLAGS", "-C link-arg=--allow-undefined")
            .args([
                "build",
                "-p",
                "kvnc-token",
                "--target",
                "wasm32-unknown-unknown",
                "--release",
            ])
            .output()
            .expect("failed to spawn cargo for the wasm contract build");
        assert!(
            build.status.success(),
            "wasm contract build failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );

        let wasm_path = target_dir
            .join("wasm32-unknown-unknown")
            .join("release")
            .join("kvnc_token.wasm");
        let wasm = std::fs::read(&wasm_path)
            .unwrap_or_else(|err| panic!("built wasm missing at {}: {err}", wasm_path.display()));
        assert!(!wasm.is_empty(), "empty wasm artifact");

        let (_dir, storage) = open_storage();
        let creator = addr(1);
        let recipient = addr(2);
        let contract = addr(40);
        let config = ExecutionConfig {
            gas_limit: 100_000_000,
            memory_limit_pages: 256, // rust-std contracts need more than the 16-page default
        };
        let mut runner = ContractRunner::new();

        // wasm path: create + transfer through the real env imports.
        let create = token_create_args(1_000);
        runner
            .execute_wasm_call(WasmCall {
                wasm: &wasm,
                entry: "token_create",
                contract,
                caller: creator,
                height: 1,
                timestamp: 1_000,
                args: &create,
                storage: &storage,
                config: &config,
            })
            .expect("wasm token_create");

        let transfer = bincode::serialize(&(recipient.0, 400u128)).expect("encode");
        runner
            .execute_wasm_call(WasmCall {
                wasm: &wasm,
                entry: "token_transfer",
                contract,
                caller: creator,
                height: 2,
                timestamp: 1_100,
                args: &transfer,
                storage: &storage,
                config: &config,
            })
            .expect("wasm token_transfer");

        // The wasm guest's kvnc_storage_set calls must have committed into
        // the namespaced table of THIS contract address.
        let token = token_state(&storage, &contract);
        assert_eq!(token.balances.get(&creator.0), Some(&600));
        assert_eq!(token.balances.get(&recipient.0), Some(&400));
        assert_eq!(token.owner, creator.0);

        // Cross-path interop: native dispatch reads and updates the exact
        // state the wasm path wrote.
        let native_transfer = bincode::serialize(&(recipient.0, 100u128)).expect("encode");
        execute_contract_call(
            "token_transfer",
            contract,
            creator,
            3,
            1_200,
            &native_transfer,
            &storage,
        )
        .expect("native token_transfer over wasm-written state");
        let token = token_state(&storage, &contract);
        assert_eq!(token.balances.get(&creator.0), Some(&500));
        assert_eq!(token.balances.get(&recipient.0), Some(&500));
    }
}
