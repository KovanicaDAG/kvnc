//! Node operations: status, balances, staking and governance.
//!
//! Status and balance are backed by node APIs. Staking and governance RPC
//! methods do not exist on the node yet, so those commands emit a `TODO`
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

fn status_json(rpc: &str, height: u64, members: &[Value], peer_count: Option<usize>) -> Value {
    json!({
        "rpc": rpc,
        "latestBlock": height,
        "committeeSize": members.len(),
        "committee": members,
        // Sync state is unknown: the node exposes no sync-status RPC.
        "syncing": Value::Null,
        "peerCount": peer_count,
    })
}

fn status_rows(
    rpc: &str,
    height: u64,
    committee_size: usize,
    peer_count: Option<usize>,
) -> Vec<(&'static str, String)> {
    vec![
        ("RPC", rpc.to_string()),
        ("Latest block", height.to_string()),
        ("Committee size", committee_size.to_string()),
        (
            "Sync status",
            "unknown (no sync-status RPC available)".to_string(),
        ),
        (
            "Peer count",
            peer_count
                .map(|count| count.to_string())
                .unwrap_or_else(|| "unknown (health unavailable)".to_string()),
        ),
    ]
}

/// `kvnc status` — latest block, committee, peer count, and sync status when available.
pub async fn status(client: &RpcClient, json_output: bool) -> Result<()> {
    let height_value = client.call("kvnc_blockNumber", json!([])).await?;
    let height = parse_quantity(&height_value)?;

    let committee_value = client.call("kvnc_getCommittee", json!([])).await?;
    let members = committee_value.as_array().cloned().unwrap_or_default();
    let peer_count = client.health_peer_count().await;

    if json_output {
        let out = status_json(client.url(), height, &members, peer_count);
        return print_json(&out);
    }

    print_kv(&status_rows(
        client.url(),
        height,
        members.len(),
        peer_count,
    ));

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_json_and_human_rows_show_zero_and_nonzero_peer_counts() {
        let members = vec![];
        for count in [0, 4] {
            let output = status_json("http://localhost:8545/rpc", 12, &members, Some(count));
            assert_eq!(output["peerCount"], json!(count));
            let rows = status_rows("http://localhost:8545/rpc", 12, 0, Some(count));
            assert_eq!(
                rows.iter().find(|(key, _)| *key == "Peer count").unwrap().1,
                count.to_string()
            );
            assert_eq!(output["syncing"], Value::Null);
        }
    }

    #[test]
    fn missing_peer_count_stays_unknown_in_json_and_human_rows() {
        let output = status_json("http://localhost:8545/rpc", 12, &[], None);
        assert_eq!(output["peerCount"], Value::Null);
        assert_eq!(output["syncing"], Value::Null);
        let rows = status_rows("http://localhost:8545/rpc", 12, 0, None);
        assert_eq!(
            rows.iter().find(|(key, _)| *key == "Peer count").unwrap().1,
            "unknown (health unavailable)"
        );
    }
}
