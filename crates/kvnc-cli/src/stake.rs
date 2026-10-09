//! Phase 8.2 CLI — delegation commands (delegate / claim-rewards).
//! Deterministic output; no HashMap ordering; tokenomics untouched.
//!
//! `stake` / `unstake` are implemented in `main.rs` as signed native
//! transactions (`TransactionKind::Stake` / `TransactionKind::Unstake`) that go
//! out over the ordinary `kvnc_sendRawTransaction` path — no staking-specific
//! node RPC is introduced and private key material never leaves the machine.
//!
//! `delegate` / `claim-rewards` have no matching `TransactionKind` variant yet,
//! so they report a clear "not yet supported" message instead of fabricating
//! success or inventing a node-side RPC method.
use anyhow::Result;
use serde_json::{json, Value};

use crate::output::print_json;
use crate::rpc::RpcClient;

/// Emit a clear unsupported-operation result for commands that cannot be
/// expressed as an existing [`kvnc_types::transaction::TransactionKind`].
fn unsupported(operation: &str, params: Value, json_output: bool) -> Result<()> {
    if json_output {
        return print_json(&json!({
            "status": "not_supported",
            "operation": operation,
            "reason": "not yet supported — requires a new TransactionKind variant and a node-side handler; no RPC exists for this",
            "params": params,
        }));
    }

    println!("`{operation}` is not yet supported.");
    println!("      Requires a new TransactionKind variant and a node-side handler.");
    println!("      Params: {params}");
    Ok(())
}

/// `kvnc delegate --validator <hex> --amount <atoms>` — no matching transaction kind.
pub async fn delegate_skeleton(
    _client: &RpcClient,
    validator: &str,
    amount: u64,
    _extra: Option<&str>,
    json_output: bool,
) -> Result<()> {
    unsupported(
        "delegate",
        json!({ "validator": validator, "amount": amount }),
        json_output,
    )
}

/// `kvnc claim-rewards` — no matching transaction kind.
pub async fn claim_rewards_skeleton(
    _client: &RpcClient,
    validator: Option<&str>,
    json_output: bool,
) -> Result<()> {
    unsupported(
        "claim-rewards",
        json!({ "validator": validator }),
        json_output,
    )
}
