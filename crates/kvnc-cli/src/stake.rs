//! Phase 8.2 CLI — delegation commands (delegate / claim-rewards).
//! Deterministic output; no HashMap ordering; tokenomics untouched.
//!
//! `stake` / `unstake` / `delegate` / `claim-rewards` are implemented as signed native
//! transactions that go out over the ordinary `kvnc_sendRawTransaction` path.
//! No staking-specific node RPC is introduced and private key material never leaves the machine.
//!
//! The actual command implementations are in `main.rs` using the shared
//! `sign_and_submit_self` helper for consistency with `stake`/`unstake`.
