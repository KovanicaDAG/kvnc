# KVNC Contract Layer (Lane 3b)

The four contract crates — `kvnc-htlc`, `kvnc-vault`, `kvnc-multisig`,
`kvnc-token` — are ordinary Rust libraries with a **typed API** and, on
`wasm32-unknown-unknown`, a fixed set of `extern "C"` **entry-point exports**.
Lane 3b wires them into the real execution path through
`kvnc-execution::contracts`; `kvnc-runtime` provides the gas-metered Wasmi
interpreter and the `env` host functions that back the ABI.

Two paths share **one** host state model (`ContractHost`) and therefore one set
of commit semantics:

| Path | Entry point | Runs |
|------|-------------|------|
| Native (canonical) | `execute_contract_call(entry, …)` | calls the typed Rust API directly |
| Wasmi | `ContractRunner::execute_wasm_call(WasmCall { … })` | instantiates the compiled `.wasm`, calls the exported entry point |

Both bincode-encode arguments and results identically, so a wasm call and a
native call over the same storage are byte-for-byte interchangeable
(cross-path interop is covered by the `wasmi_e2e_kvnc_token_wasm` test).

> Canonical tokenomics constants (supply cap, maturity, fee split, treasury
> vesting) live in [`docs/TOKENOMICS.md`](TOKENOMICS.md) §7.1 and
> `crates/kvnc-staking/src/lib.rs`. This document does not duplicate them.

---

## 1. Core types (`kvnc-common`)

| Type | Definition |
|------|------------|
| `Address` | `[u8; 32]` |
| `Hash` | `[u8; 32]` |
| `Amount` | `u128` (native base units, 9 decimals) |
| `Height` / `Timestamp` | `u64` |
| `ContractResult<T>` | `Result<T, ContractError>` |

`ContractError` and its stable ABI codes:

| Code | Variant | Code | Variant |
|-----:|---------|-----:|---------|
| 0 | `Unauthorized` | 6 | `NotExpired` |
| 1 | `InsufficientBalance` | 7 | `HashMismatch` |
| 2 | `InvalidInput` | 8 | `ThresholdNotMet` |
| 3 | `AlreadyExists` | 9 | `Paused` |
| 4 | `NotFound` | 10 | `Overflow` |
| 5 | `Expired` | 11 | `Custom(u32)` |

`hash(bytes)` is **BLAKE3-256** and is the canonical deterministic hash for all
derived IDs, escrow addresses, and storage namespacing.

## 2. The `Host` trait

Contracts never touch the ledger directly; every effect goes through
`kvnc_common::Host`:

```rust
pub trait Host {
    fn caller(&self) -> Address;                 // msg.sender
    fn contract_address(&self) -> Address;       // address(this)
    fn block_height(&self) -> Height;
    fn timestamp(&self) -> Timestamp;
    fn balance_of(&self, addr: &Address) -> Amount;
    fn transfer(&mut self, from: &Address, to: &Address, amount: Amount)
        -> ContractResult<()>;
    fn emit_event(&mut self, topic: &[u8], data: &[u8]);
    fn storage_get(&self, key: &[u8]) -> Option<Vec<u8>>;
    fn storage_set(&mut self, key: &[u8], value: &[u8]);
}
```

**Why `contract_address()` exists.** Escrow-style contracts must move funds
between the caller and *derived record addresses* (HTLC/vault escrow =
`hash(domain || id)`) and the multisig must pay out from the contract's own
account. The skeleton's zeroed `contract_self()` helper could not express that,
so every `Host` implementation must report the executing contract's address.

Implementations:

- `MemHost` (`kvnc-common::mem`) — in-memory host over a shared
  `Rc<RefCell<…>>`; used for native unit tests.
- `WasmHost` (`kvnc-common::wasm`, `wasm32` only) — forwards to the `env`
  imports below.
- `ContractHost` (`kvnc-execution::contracts`) — the production host over
  `kvnc-storage` with a per-call overlay (see §5).

## 3. Contract typed APIs

### kvnc-htlc (KVP-104)

| Function | Arguments | Result |
|----------|-----------|--------|
| `Htlc::create` | `claimer: Address, amount: Amount, hash_lock: Hash, expiry: Timestamp` | `SwapId` = `[u8; 32]` |
| `Htlc::claim` | `id: SwapId, preimage: &[u8]` | `()` |
| `Htlc::refund` | `id: SwapId` | `()` |
| `escrow_address(id)` | `&SwapId` | `Address` |

`create` moves `amount` from the caller into the per-record escrow address.
`claim` pays the designated claimer on a matching preimage before `expiry`;
`refund` returns funds to the sender after `expiry`.

### kvnc-vault (KVP-105)

| Function | Arguments | Result |
|----------|-----------|--------|
| `Vault::create` | `beneficiary: Address, amount: Amount, schedule: VestingSchedule` | `VaultId` = `[u8; 32]` |
| `Vault::claim` | `id: VaultId` | `Amount` (newly vested) |
| `Vault::cancel` | `id: VaultId` | `()` |
| `escrow_address(id)` | `&VaultId` | `Address` |

`VestingSchedule`:

```rust
enum VestingSchedule {
    Absolute { unlock_at: Timestamp },
    Linear { start: Timestamp, end: Timestamp, cliff: Option<Timestamp> },
}
```

### kvnc-multisig (KVP-101)

| Function | Arguments | Result |
|----------|-----------|--------|
| `Multisig::create` | `owners: Vec<Address>, threshold: u32` | `MultisigId` = `[u8; 32]` |
| `Multisig::propose` | `id: MultisigId, to: Address, amount: Amount, data: Vec<u8>` | `TxId` = `u64` |
| `Multisig::confirm` | `id: MultisigId, tx_id: TxId` | `()` |
| `Multisig::execute` | `id: MultisigId, tx_id: TxId` | `()` |

Singleton per contract address; the proposer auto-confirms; `execute` requires
`threshold` confirmations and moves the native balance.

### kvnc-token (KVP-102)

| Function | Arguments | Result |
|----------|-----------|--------|
| `Token::create` | `name: String, symbol: String, decimals: u8, initial_supply: Amount` | `()` |
| `Token::transfer` | `to: Address, amount: Amount` | `()` |
| `Token::approve` | `spender: Address, amount: Amount` | `()` |
| `Token::transfer_from` | `from: Address, to: Address, amount: Amount` | `()` |
| `Token::mint` | `to: Address, amount: Amount` | `()` |
| `Token::burn` | `amount: Amount` | `()` |

Singleton per contract address; caller becomes owner + minter. Token balances
are internal to the token record — they do **not** touch native account
balances.

## 4. Dispatch entry points (the 16 names)

`kvnc-execution::contracts::ENTRY_POINTS` is the authoritative list. The native
dispatcher, the wasm exports, and this table must stay in sync (enforced by the
`dispatch_covers_all_16_entry_points` test). Arguments and results are
**bincode 1.x** encodings of the tuples below.

| Entry | bincode args | bincode result |
|-------|--------------|----------------|
| `htlc_create` | `(Address, Amount, Hash, Timestamp)` | `SwapId` |
| `htlc_claim` | `(SwapId, Vec<u8>)` | `()` |
| `htlc_refund` | `(SwapId,)` | `()` |
| `vault_create` | `(Address, Amount, VestingSchedule)` | `VaultId` |
| `vault_claim` | `(VaultId,)` | `Amount` |
| `vault_cancel` | `(VaultId,)` | `()` |
| `multisig_create` | `(Vec<Address>, u32)` | `MultisigId` |
| `multisig_propose` | `(MultisigId, Address, Amount, Vec<u8>)` | `TxId` |
| `multisig_confirm` | `(MultisigId, TxId)` | `()` |
| `multisig_execute` | `(MultisigId, TxId)` | `()` |
| `token_create` | `(String, String, u8, Amount)` | `()` |
| `token_transfer` | `(Address, Amount)` | `()` |
| `token_approve` | `(Address, Amount)` | `()` |
| `token_transfer_from` | `(Address, Address, Amount)` | `()` |
| `token_mint` | `(Address, Amount)` | `()` |
| `token_burn` | `(Amount,)` | `()` |

## 5. `ContractHost` semantics and storage layout

`ContractHost` (in `kvnc-execution::contracts`) is created **per call** and
holds:

- an immutable context — contract address, caller, block height, timestamp;
- one redb `ReadTransaction` snapshot for the whole call (repeatable reads;
  redb is MVCC, so a read transaction may coexist with a concurrent writer);
- an in-memory overlay — pending storage writes, touched accounts, emitted
  events;
- a sticky read-error slot: a failed backing read makes `commit()` refuse to
  flush, so a transient storage failure can never be mistaken for "absent".

**Commit rules.** `commit()` runs only after the entry point returned `Ok`. It
drops the snapshot, opens one `WriteTransaction`, flushes all storage writes and
touched accounts, and commits. On any `ContractError`, runtime error, or trap
the host is dropped uncommitted: no storage write, no balance delta, and no
event is persisted.

**Native balances** use `kvnc_storage::state_store::Account.balance` (`u64`).
An `Amount > u64::MAX` maps to `ContractError::Overflow`; a shortfall to
`InsufficientBalance`. `balance_of` and `transfer` read/write through the
overlay.

### Raw contract keys

Contracts pass **raw, variable-length** keys. Current keys:

| Contract | Key | Size |
|----------|-----|------|
| `kvnc-htlc` | `b"kvnc/v1/htlc/" ++ id` | 13 + 32 |
| `kvnc-vault` | `b"kvnc/v1/vault/" ++ id` | 14 + 32 |
| `kvnc-multisig` | `b"kvnc/v1/state"` (singleton) | 13 |
| `kvnc-multisig` | `b"kvnc/v1/mtx/" ++ tx_id.to_le_bytes()` | 12 + 8 |
| `kvnc-token` | `b"kvnc/v1/state"` (singleton) | 13 |

### Namespacing (fixed)

The redb `CONTRACT_STORAGE` table key is a fixed `([u8; 32], [u8; 32])` pair
`(contract_addr, table_key)`, but contract keys are variable length. The host
therefore derives the table key as:

```text
table_key = blake3(raw_key)          // kvnc_common::hash
```

`contract_addr` is the first tuple element, so two contracts can use the same
raw key without collision. `blake3` collisions within one contract are
negligible (256-bit).

## 6. Wasm ABI (FIXED)

### Imports (module `"env"`)

```text
kvnc_caller(out_ptr: i32)                            // writes 32 bytes
kvnc_contract_address(out_ptr: i32)                  // writes 32 bytes
kvnc_block_height() -> i64
kvnc_timestamp() -> i64
kvnc_balance_of(ptr: i32, len: i32) -> i64           // u64 balance as i64; -1 = error
kvnc_transfer(fp, fl, tp, tl: i32, amount: i64) -> i32   // 0 = ok
kvnc_storage_get(kp, kl, out_ptr: i32, out_cap: i32) -> i32
kvnc_storage_set(kp, kl, vp, vl: i32) -> i32         // 0 = ok
kvnc_emit_event(tp, tl, dp, dl: i32)
```

`kvnc_storage_get` is two-step: `out_cap = 0` returns the needed length (or `-1`
when absent); a subsequent call with a sufficient `out_cap` returns the length,
and a too-small `out_cap` returns `-2`.

### Exports

- `kvnc_alloc(len: i32) -> i32` / `kvnc_dealloc(ptr: i32, len: i32)` — guest
  allocator used by the host to place arguments and to release results.
- The 16 entry points, each `(args_ptr: i32, args_len: i32) -> i64`.

### Return packing

```text
success:  ((out_ptr as u32 as i64) << 32) | (out_len as u32 as i64)
          out_len == 0  ⇒ null pointer; nothing to read or free
error:    -(1 + ContractError::code())      // always within -12..=-1
```

Arguments and results are bincode. Buffers with `out_len > 0` are
guest-allocated; the host **must** call `kvnc_dealloc(ptr, len)` for both its
argument buffer and the result buffer. `kvnc-runtime::Runtime::execute` decodes
the packed `i64`, maps `-12..=-1` to `RuntimeError::Contract(code)`, and enforces
`ExecutionConfig::gas_limit` (fuel) and `memory_limit_pages` (store limiter).

## 7. Building a contract wasm

```sh
cargo build -p kvnc-token --target wasm32-unknown-unknown --release
```

This requires the `wasm32-unknown-unknown` target
(`rustup target add wasm32-unknown-unknown`).

> **Known Lane 3a gap.** The `env` externs in `crates/kvnc-common/src/wasm.rs`
> are declared without `#[link(wasm_import_module = "env")]`, so a plain
> `cargo build --target wasm32-unknown-unknown` currently fails at **link** time
> (`undefined symbol: kvnc_caller`, …) — rust-lld does not auto-import bare
> externs. `cargo check --target wasm32-unknown-unknown` passes because check
> does not link. Until the attribute is added to `kvnc-common`, build with:
>
> ```sh
> RUSTFLAGS="-C link-arg=--allow-undefined" \
>   cargo build -p kvnc-token --target wasm32-unknown-unknown --release
> ```
>
> which imports the undefined symbols from the default wasm module (`env`),
> producing the exact import section this ABI requires. The
> `wasmi_e2e_kvnc_token_wasm` test uses this workaround.

## 8. Module cache

`ContractRunner` keeps a `HashMap<blake3(wasm), Module>` so a wasm module is
compiled once per runner. The cache is in-memory only — persisting it across
process restarts is a `TODO` (see `contracts.rs`).
