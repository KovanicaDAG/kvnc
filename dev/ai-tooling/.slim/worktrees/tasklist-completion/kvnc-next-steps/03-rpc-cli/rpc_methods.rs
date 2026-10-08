//! Skeleton RPC methods for the new contracts.
//! Adapt to whatever RPC framework kvnc already uses (jsonrpc, tonic, custom HTTP, etc.).

use serde::{Deserialize, Serialize};

// ---------- HTLC ----------

#[derive(Debug, Serialize, Deserialize)]
pub struct HtlcCreateParams {
    pub claimer: String,      // hex address
    pub amount: String,       // decimal string
    pub hash_lock: String,    // hex
    pub expiry: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HtlcClaimParams {
    pub id: String,
    pub preimage: String,     // hex
}

// ---------- Vault ----------

#[derive(Debug, Serialize, Deserialize)]
pub struct VaultCreateParams {
    pub beneficiary: String,
    pub amount: String,
    pub schedule: VaultScheduleDto,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum VaultScheduleDto {
    Absolute { unlock_at: u64 },
    Linear {
        start: u64,
        end: u64,
        cliff: Option<u64>,
    },
}

// ---------- Multisig ----------

#[derive(Debug, Serialize, Deserialize)]
pub struct MultisigCreateParams {
    pub owners: Vec<String>,
    pub threshold: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MultisigProposeParams {
    pub multisig_id: String,
    pub to: String,
    pub amount: String,
    pub data: Option<String>,
}

// ---------- Token ----------

#[derive(Debug, Serialize, Deserialize)]
pub struct TokenCreateParams {
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
    pub initial_supply: String,
}

// ---------- Suggested RPC surface ----------
//
// htlc_create(params) -> swap_id
// htlc_claim(params)  -> ok
// htlc_refund(id)     -> ok
//
// vault_create(params) -> vault_id
// vault_claim(id)      -> amount_claimed
// vault_cancel(id)     -> ok
//
// multisig_create(params) -> multisig_id
// multisig_propose(params) -> tx_id
// multisig_confirm(id, tx_id) -> ok
// multisig_execute(id, tx_id) -> ok
//
// token_create(params) -> contract_address
// token_transfer(to, amount)
// token_mint(to, amount)
// token_burn(amount)
// token_balance(address) -> amount
//
// Implement the actual handlers by constructing a Host and calling the contract methods.
