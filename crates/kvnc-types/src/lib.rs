//! Core types for the Kovanica (KVNC) blockchain.
//!
//! This crate contains all fundamental data structures used across the system:
//! blocks, transactions, addresses, rounds, committee, etc.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod address;
pub mod block;
pub mod commit;
pub mod committee;
pub mod crypto;
pub mod error;
pub mod hash;
pub mod light_client;
pub mod transaction;
pub mod vote;

pub use address::Address;
pub use block::{Block, BlockReference, StatementBlock};
pub use commit::CommittedSubDag;
pub use committee::{Authority, Committee, Stake};
pub use crypto::{PublicKey, Signature, SigningKey};
pub use error::TypesError;
pub use hash::Hash;
pub use light_client::{
    ColouringCertificate, LightClientCheckpoint, StateProof, WaveCommitCertificate,
};
pub use transaction::{Transaction, TransactionKind};
pub use vote::Vote;

/// Logical round number in the DAG.
pub type Round = u64;

/// Unique authority index within the current committee.
pub type AuthorityIndex = u16;

/// Wave length used by the consensus (Mysticeti-style = 3).
pub const WAVE_LENGTH: u64 = 3;

/// Maximum allowed gap, in rounds, between a block and any of its parents.
///
/// Parent selection walks backwards from `round - 1` until it finds the most
/// recent populated round; validation rejects any parent further back than
/// this, keeping the DAG causally connected without unbounded edges.
pub const MAX_PARENT_ROUND_GAP: u64 = 300;

/// Maximum number of transactions per block (tunable).
pub const MAX_TXS_PER_BLOCK: usize = 10_000;

/// Genesis round.
pub const GENESIS_ROUND: Round = 0;
