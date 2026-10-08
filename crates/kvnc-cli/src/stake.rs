//! Staking subcommands (stake / unstake / delegate / claim-rewards) skeleton.
//!
//! Uses `kvnc_getStake` and `kvnc_getValidators` for status queries.
//! Transaction submission paths remain skeleton until node RPC registers
//! `kvnc_stake` / `kvnc_unstake` / `kvnc_delegate` / `kvnc_claimRewards`.

use anyhow::Result;
use serde_json::json;

use crate::output::{print_json, print_kv, print_table};
use crate::rpc::{parse_quantity, RpcClient};
use crate::wallet::parse_address;

/// Query stake for an address (own + delegated).
pub async fn query_stake(client: &RpcClient, address: &str, json_output: bool) -> Result<()> {
    let addr = parse_address(address, "address")?;
    let value = client
        .call("kvnc_getStake", json!([addr.to_hex()]))
        .await?;
    let stake = parse_quantity(&value).unwrap_or(0);

    if json_output {
        return print_json(&json!({
            "address": addr.to_hex(),
            "stake": stake,
        }));
    }
    print_kv(&[
        ("Address", addr.to_hex()),
        ("Total stake", format!("{} atoms", stake)),
    ]);
    Ok(())
}

/// List active validators.
pub async fn query_validators(client: &RpcClient, json_output: bool) -> Result<()> {
    let value = client.call("kvnc_getValidators", json!([])).await?;
    let validators = value.as_array().cloned().unwrap_or_default();

    if json_output {
        return print_json(&json!({ "validators": validators }));
    }
    println!("Active validators: {}", validators.len());
    if !validators.is_empty() {
        let rows: Vec<Vec<String>> = validators
            .iter()
            .map(|v| {
                vec![
                    v.get("address").and_then(|a| a.as_str()).unwrap_or("-").to_string(),
                    v.get("stake").and_then(|s| s.as_str()).unwrap_or("-").to_string(),
                    v.get("commission_bps").and_then(|c| c.as_u64().map(|n| n.to_string())).unwrap_or_else(|| "-".to_string()),
                    v.get("active").and_then(|a| a.as_bool()).map(|b| b.to_string()).unwrap_or_else(|| "-".to_string()),
                ]
            })
            .collect();
        print_table(&["ADDRESS", "STAKE", "COMMISSION_BPS", "ACTIVE"], &rows);
    }
    Ok(())
}

/// Skeleton: stake amount to become validator / increase stake.
pub async fn stake_skeleton(client: &RpcClient, amount: u64, address: Option<&str>, json_output: bool) -> Result<()> {
    // Status from RPC
    let status = if let Some(addr) = address {
        let addr = parse_address(addr, "address")?;
        let v = client.call("kvnc_getStake", json!([addr.to_hex()])).await?;
        json!({ "address": addr.to_hex(), "stake": parse_quantity(&v).unwrap_or(0) })
    } else {
        json!({ "note": "provide --address to query stake" })
    };

    if json_output {
        return print_json(&json!({
            "operation": "stake",
            "intendedRpc": "kvnc_stake",
            "amount": amount,
            "status": status,
            "params": json!({ "amount": amount }),
        }));
    }
    println!("STAGE: stake (skeleton)");
    println!("  Intended RPC: kvnc_stake");
    println!("  Amount: {} atoms", amount);
    println!("  Status: {}", status);
    println!("  Note: node does not expose kvnc_stake yet; submit signed TransactionKind::Stake via mempool.");
    Ok(())
}

/// Skeleton: unstake / begin unbonding.
pub async fn unstake_skeleton(client: &RpcClient, amount: u64, address: Option<&str>, json_output: bool) -> Result<()> {
    let status = if let Some(addr) = address {
        let addr = parse_address(addr, "address")?;
        let v = client.call("kvnc_getStake", json!([addr.to_hex()])).await?;
        json!({ "address": addr.to_hex(), "stake": parse_quantity(&v).unwrap_or(0) })
    } else {
        json!({ "note": "provide --address to query stake" })
    };
    if json_output {
        return print_json(&json!({
            "operation": "unstake",
            "intendedRpc": "kvnc_unstake",
            "amount": amount,
            "status": status,
            "params": json!({ "amount": amount }),
        }));
    }
    println!("STAGE: unstake (skeleton)");
    println!("  Intended RPC: kvnc_unstake");
    println!("  Amount: {} atoms", amount);
    println!("  Status: {}", status);
    println!("  Note: unbonding period = 100_000 rounds per kvnc-staking constants.");
    Ok(())
}

/// Skeleton: delegate to validator.
pub async fn delegate_skeleton(client: &RpcClient, validator: &str, amount: u64, address: Option<&str>, json_output: bool) -> Result<()> {
    let status = if let Some(addr) = address {
        let addr = parse_address(addr, "address")?;
        let v = client.call("kvnc_getStake", json!([addr.to_hex()])).await?;
        json!({ "address": addr.to_hex(), "stake": parse_quantity(&v).unwrap_or(0) })
    } else {
        json!({ "note": "provide --address to query stake" })
    };
    // Validator list for context
    let validators = client.call("kvnc_getValidators", json!([])).await?;
    if json_output {
        return print_json(&json!({
            "operation": "delegate",
            "intendedRpc": "kvnc_delegate",
            "validator": validator,
            "amount": amount,
            "status": status,
            "validators": validators,
            "params": json!({ "validator": validator, "amount": amount }),
        }));
    }
    println!("STAGE: delegate (skeleton)");
    println!("  Intended RPC: kvnc_delegate");
    println!("  Validator: {}", validator);
    println!("  Amount: {} atoms", amount);
    println!("  Status: {}", status);
    println!("  Note: delegation pushes to StakingState::delegations; unbond via unstake.");
    Ok(())
}

/// Skeleton: claim staking rewards.
pub async fn claim_rewards_skeleton(client: &RpcClient, address: Option<&str>, json_output: bool) -> Result<()> {
    let status = if let Some(addr) = address {
        let addr = parse_address(addr, "address")?;
        let v = client.call("kvnc_getStake", json!([addr.to_hex()])).await?;
        json!({ "address": addr.to_hex(), "stake": parse_quantity(&v).unwrap_or(0), "rewards": 0 })
    } else {
        json!({ "note": "provide --address to query rewards; per-address cumulative rewards not yet tracked by node (handle_get_rewards returns 0)" })
    };
    if json_output {
        return print_json(&json!({
            "operation": "claim-rewards",
            "intendedRpc": "kvnc_claimRewards",
            "status": status,
            "params": json!({}),
        }));
    }
    println!("STAGE: claim-rewards (skeleton)");
    println!("  Intended RPC: kvnc_claimRewards");
    println!("  Status: {}", status);
    println!("  Note: node handles `kvnc_getRewards` but returns 0 until per-address accounting added.");
    Ok(())
}
