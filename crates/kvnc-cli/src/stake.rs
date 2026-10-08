//! Phase 8.2 CLI — delegation commands (stake / unstake / delegate / claim-rewards).
//! Deterministic output; no HashMap ordering; tokenomics untouched.
//!
//! NOTE: the node-side staking RPC methods (`kvnc_stake`, `kvnc_unstake`,
//! `kvnc_delegate`, `kvnc_claimRewards`) are not implemented yet, so these
//! commands report `not_implemented` instead of silently faking success.
use anyhow::Result;
use serde_json::json;

use crate::node::not_implemented;
use crate::rpc::RpcClient;

/// `kvnc stake --amount <atoms>` — node-side `kvnc_stake` RPC not implemented yet.
pub async fn stake_skeleton(
    _client: &RpcClient,
    amount: u64,
    validator: Option<&str>,
    json_output: bool,
) -> Result<()> {
    not_implemented(
        "stake",
        "kvnc_stake",
        json!({ "amount": amount, "validator": validator }),
        json_output,
    )
}

/// `kvnc unstake --amount <atoms>` — node-side `kvnc_unstake` RPC not implemented yet.
pub async fn unstake_skeleton(
    _client: &RpcClient,
    amount: u64,
    validator: Option<&str>,
    json_output: bool,
) -> Result<()> {
    not_implemented(
        "unstake",
        "kvnc_unstake",
        json!({ "amount": amount, "validator": validator }),
        json_output,
    )
}

/// `kvnc delegate --validator <hex> --amount <atoms>` — `kvnc_delegate` RPC not implemented yet.
pub async fn delegate_skeleton(
    _client: &RpcClient,
    validator: &str,
    amount: u64,
    _extra: Option<&str>,
    json_output: bool,
) -> Result<()> {
    not_implemented(
        "delegate",
        "kvnc_delegate",
        json!({ "validator": validator, "amount": amount }),
        json_output,
    )
}

/// `kvnc claim-rewards` — node-side `kvnc_claimRewards` RPC not implemented yet.
pub async fn claim_rewards_skeleton(
    _client: &RpcClient,
    validator: Option<&str>,
    json_output: bool,
) -> Result<()> {
    not_implemented(
        "claim-rewards",
        "kvnc_claimRewards",
        json!({ "validator": validator }),
        json_output,
    )
}
