//! Shared types for kvnc contracts.
//! Designed for Wasmi (deterministic WASM) + account-model host.
//! Keep this crate `no_std` friendly.
//!
//! Contents:
//! - core value types ([`Address`], [`Hash`], [`Amount`], ...) and [`ContractError`]
//! - the [`Host`] trait — the only surface contracts may use to touch ledgers/storage
//! - [`hash`] — the canonical deterministic hash (BLAKE3-256) for all derived IDs
//! - [`MemHost`] — in-memory host for tests / native fallback (no_std + alloc friendly)
//! - [`WasmHost`] — wasm32-only host over the fixed `env`-module imports (Lane 3b ABI)
//!
//! ### Feature / no_std notes
//! The crate is `no_std`-friendly (`#![cfg_attr(not(feature = "std"), no_std)]` +
//! `alloc`). `blake3` is currently built with its default (std) backend because
//! the root workspace dependency does not disable default features; flipping to
//! `default-features = false` + `std = ["blake3/std"]` requires a root
//! `Cargo.toml` change (out of scope for this lane). On `wasm32-unknown-unknown`
//! std exists, so the default feature set compiles there.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

use core::fmt;

pub mod events;
mod mem;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub use mem::{MemHost, MemShared, SharedMem};
#[cfg(target_arch = "wasm32")]
pub use wasm::WasmHost;

/// 32-byte address (account or contract).
pub type Address = [u8; 32];

/// 32-byte hash (SHA-256 / Blake3 / whatever host uses).
pub type Hash = [u8; 32];

/// Native token amount (base units). kvnc uses 9 decimals.
pub type Amount = u128;

/// Block / round height.
pub type Height = u64;

/// Timestamp in seconds (host-provided).
pub type Timestamp = u64;

/// Simple result type used across contracts.
pub type ContractResult<T> = Result<T, ContractError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    Unauthorized,
    InsufficientBalance,
    InvalidInput,
    AlreadyExists,
    NotFound,
    Expired,
    NotExpired,
    HashMismatch,
    ThresholdNotMet,
    Paused,
    Overflow,
    Custom(u32),
}

impl ContractError {
    /// Stable numeric code used by the wasm entry-point ABI.
    ///
    /// Entry points pack errors as `-1 - code()` (always within `-12..=-1`).
    /// `Custom(_)` packs as `11`; the inner `u32` is **not** conveyed through
    /// the packed code — full fidelity is only available on the typed/native
    /// path. Lane 3b decodes: error iff `-12 <= ret <= -1`.
    pub fn code(&self) -> i32 {
        match self {
            Self::Unauthorized => 0,
            Self::InsufficientBalance => 1,
            Self::InvalidInput => 2,
            Self::AlreadyExists => 3,
            Self::NotFound => 4,
            Self::Expired => 5,
            Self::NotExpired => 6,
            Self::HashMismatch => 7,
            Self::ThresholdNotMet => 8,
            Self::Paused => 9,
            Self::Overflow => 10,
            Self::Custom(_) => 11,
        }
    }
}

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized => write!(f, "unauthorized"),
            Self::InsufficientBalance => write!(f, "insufficient balance"),
            Self::InvalidInput => write!(f, "invalid input"),
            Self::AlreadyExists => write!(f, "already exists"),
            Self::NotFound => write!(f, "not found"),
            Self::Expired => write!(f, "expired"),
            Self::NotExpired => write!(f, "not yet expired"),
            Self::HashMismatch => write!(f, "hash mismatch"),
            Self::ThresholdNotMet => write!(f, "threshold not met"),
            Self::Paused => write!(f, "contract paused"),
            Self::Overflow => write!(f, "arithmetic overflow"),
            Self::Custom(c) => write!(f, "custom error {}", c),
        }
    }
}

/// Minimal host interface expected by contracts.
/// In real kvnc this is provided by the Wasmi runtime / host functions.
pub trait Host {
    fn caller(&self) -> Address;
    /// Address of the currently executing contract.
    ///
    /// **Why this was added (post-skeleton):** escrow-style contracts must move
    /// funds between the caller and *derived record addresses* (HTLC/vault
    /// escrow = `hash(domain || id)`), and the multisig must pay out from the
    /// contract's own account. The skeleton's zeroed `contract_self()` helper
    /// could not be fixed without exposing this; every `Host` impl must
    /// therefore report the executing contract's address.
    fn contract_address(&self) -> Address;
    fn block_height(&self) -> Height;
    fn timestamp(&self) -> Timestamp;
    fn balance_of(&self, addr: &Address) -> Amount;
    fn transfer(&mut self, from: &Address, to: &Address, amount: Amount) -> ContractResult<()>;
    fn emit_event(&mut self, topic: &[u8], data: &[u8]);
    // Optional: storage get/set if contracts use host storage instead of pure WASM memory
    fn storage_get(&self, key: &[u8]) -> Option<Vec<u8>>;
    fn storage_set(&mut self, key: &[u8], value: &[u8]);
}

use alloc::vec::Vec;

/// Canonical deterministic hash for all contract ID / escrow-address derivation.
///
/// **BLAKE3-256** over the input bytes (`blake3::hash`), returned as a raw
/// 32-byte digest. Every derived identifier documents its exact input encoding
/// at the call site; the house convention is:
///
/// ```text
/// hash( domain_prefix_ascii || fixed_width_fields )
/// ```
///
/// with all integers little-endian and fixed-width, and domain prefixes that
/// differ between derivation kinds (e.g. `b"htlc/id"` vs `b"htlc/escrow"`) to
/// prevent cross-domain collisions.
pub fn hash(data: &[u8]) -> [u8; 32] {
    *blake3::hash(data).as_bytes()
}
