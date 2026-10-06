//! Skeleton: event topics the indexer should watch.

pub const HTLC_CREATED: &[u8] = b"htlc_created";
pub const HTLC_CLAIMED: &[u8] = b"htlc_claimed";
pub const HTLC_REFUNDED: &[u8] = b"htlc_refunded";

pub const VAULT_CREATED: &[u8] = b"vault_created";
pub const VAULT_CLAIMED: &[u8] = b"vault_claimed";
pub const VAULT_CANCELLED: &[u8] = b"vault_cancelled";

pub const MULTISIG_CREATED: &[u8] = b"multisig_created";
pub const MULTISIG_PROPOSED: &[u8] = b"multisig_proposed";
pub const MULTISIG_CONFIRMED: &[u8] = b"multisig_confirmed";
pub const MULTISIG_EXECUTED: &[u8] = b"multisig_executed";

pub const TOKEN_CREATED: &[u8] = b"token_created";
pub const TOKEN_TRANSFER: &[u8] = b"transfer";
pub const TOKEN_APPROVAL: &[u8] = b"approval";
pub const TOKEN_MINT: &[u8] = b"mint";
pub const TOKEN_BURN: &[u8] = b"burn";

// TODO: in your indexer, match on these topics and decode the data payload
// (currently the contracts emit the id / empty data – enrich later if needed).
