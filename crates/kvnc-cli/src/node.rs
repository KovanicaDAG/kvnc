//! Node operations: status, balances, staking and governance.
//!
//! Status and balance are backed by real RPC methods. Staking and governance
//! RPC methods do not exist on the node yet, so those commands emit a `TODO`
//! describing the intended method name and payload.

use anyhow::Result;
use serde_json::{json, Value};

use crate::output::{print_json, print_kv, print_table};
use crate::rpc::{parse_quantity, RpcClient};
use crate::wallet::parse_address;

/// Render a JSON value as a display cell (`-` when absent).
fn cell(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => "-".to_string(),
    }
}

/// `kvnc status` — latest block, committee, and (unavailable) sync/peer info.
pub async fn status(client: &RpcClient, json_output: bool) -> Result<()> {
    let height_value = client.call("kvnc_blockNumber", json!([])).await?;
    let height = parse_quantity(&height_value)?;

    let committee_value = client.call("kvnc_getCommittee", json!([])).await?;
    let members = committee_value.as_array().cloned().unwrap_or_default();

    if json_output {
        let out = json!({
            "rpc": client.url(),
            "latestBlock": height,
            "committeeSize": members.len(),
            "committee": members,
            // No RPC method exposes these yet.
            "syncing": Value::Null,
            "peerCount": Value::Null,
        });
        return print_json(&out);
    }

    print_kv(&[
        ("RPC", client.url().to_string()),
        ("Latest block", height.to_string()),
        ("Committee size", members.len().to_string()),
        (
            "Sync status",
            "unknown (TODO: add `kvnc_syncing` RPC method)".to_string(),
        ),
        (
            "Peer count",
            "unknown (TODO: add `kvnc_peerCount` RPC method)".to_string(),
        ),
    ]);

    if !members.is_empty() {
        println!();
        let rows: Vec<Vec<String>> = members
            .iter()
            .map(|m| {
                vec![
                    cell(m.get("index")),
                    cell(m.get("address")),
                    cell(m.get("stake")),
                    cell(m.get("network_address")),
                ]
            })
            .collect();
        print_table(&["INDEX", "ADDRESS", "STAKE", "NETWORK"], &rows);
    }

    Ok(())
}

/// `kvnc balance <address>` — native KVNC balance in atoms.
pub async fn balance(client: &RpcClient, address: &str, json_output: bool) -> Result<()> {
    let address = parse_address(address, "address")?;
    let value = client
        .call("kvnc_getBalance", json!([address.to_hex()]))
        .await?;
    let balance = parse_quantity(&value)?;

    if json_output {
        return print_json(&json!({
            "address": address.to_hex(),
            "balance": balance,
        }));
    }

    print_kv(&[
        ("Address", address.to_hex()),
        ("Balance", format!("{balance} atoms")),
    ]);
    Ok(())
}

/// Print a `TODO` for an operation whose RPC method is not implemented yet.
pub fn not_implemented(
    operation: &str,
    intended_rpc: &str,
    params: Value,
    json_output: bool,
) -> Result<()> {
    if json_output {
        return print_json(&json!({
            "status": "not_implemented",
            "operation": operation,
            "intendedRpc": intended_rpc,
            "params": params,
        }));
    }

    println!("TODO: `{operation}` is not implemented yet.");
    println!("      Intended RPC method: `{intended_rpc}`");
    println!("      Intended params:     {params}");
    Ok(())
}
