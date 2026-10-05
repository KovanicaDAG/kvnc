//! Error types for kvnc-types.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum TypesError {
    #[error("Invalid signature")]
    InvalidSignature,
    #[error("Invalid parent references")]
    InvalidParents,
    #[error("Round mismatch")]
    RoundMismatch,
    #[error("Serialization error: {0}")]
    Serialization(String),
    #[error("Other: {0}")]
    Other(String),
}
