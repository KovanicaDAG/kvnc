//! Error types for kvnc-types.

use thiserror::Error;

/// Errors that can occur in kvnc-types operations.
#[derive(Error, Debug)]
pub enum TypesError {
    /// Ed25519 signature verification failed.
    #[error("Invalid signature")]
    InvalidSignature,
    /// Block parent references are invalid (missing, wrong round, etc.).
    #[error("Invalid parent references")]
    InvalidParents,
    /// Block round does not match expected round.
    #[error("Round mismatch")]
    RoundMismatch,
    /// Serialization/deserialization failed.
    #[error("Serialization error: {0}")]
    Serialization(String),
    /// Other miscellaneous errors.
    #[error("Other: {0}")]
    Other(String),
}
