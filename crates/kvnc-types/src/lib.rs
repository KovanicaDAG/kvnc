//! Core types for the Kovanica (KVNC) blockchain.
//!
//! This crate contains all fundamental data structures used across the system:
//! blocks, transactions, addresses, rounds, committee, etc.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod address;
pub mod block;
pub mod committee;
pub mod crypto;
pub mod error;
pub mod hash;
pub mod transaction;

pub use address::Address;
pub use block::{Block, BlockReference, StatementBlock};
pub use committee::{Authority, Committee, Stake};
pub use crypto::{PublicKey, SigningKey, Signature};
pub use error::TypesError;
pub use hash::Hash;
pub use transaction::{Transaction, TransactionKind};

/// Logical round number in the DAG.
pub type Round = u64;

/// Unique authority index within the current committee.
pub type AuthorityIndex = u16;

/// Wave length used by the consensus (Mysticeti-style = 3).
pub const WAVE_LENGTH: u64 = 3;

/// Maximum number of transactions per block (tunable).
pub const MAX_TXS_PER_BLOCK: usize = 10_000;

/// Genesis round.
pub const GENESIS_ROUND: Round = 0;
