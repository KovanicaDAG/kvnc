//! Standard chain / transaction / account / staking / mempool / consensus
//! JSON-RPC methods.
//!
//! These handlers are backed by the real node state carried in [`RpcState`]:
//! the redb [`Storage`](kvnc_storage::Storage) for blocks/accounts/consensus,
//! the [`Mempool`](kvnc_mempool::Mempool) for pending transactions, the shared
//! [`StakingState`](kvnc_staking::StakingState) for validator queries and the
//! [`CommitteeInfo`](kvnc_consensus::CommitteeInfo) for leader selection.
//!
//! Response shapes follow the Ethereum JSON-RPC conventions where a KVNC
//! equivalent exists: 32-byte values and byte blobs are `0x`-prefixed hex
//! strings, while integer quantities (height, balance, nonce, fee, stake) are
//! `0x`-prefixed minimal hex ("quantity") strings.

use crate::{RpcError, RpcState};
use kvnc_storage::BlockStoreError;
use kvnc_types::{
    block::StatementBlock,
    hash::Hash,
    transaction::{Transaction, TransactionKind},
    Address,
};
use serde_json::{json, Value};

/// Default number of rounds returned by `kvnc_getLeaderSchedule` when the
/// caller does not specify a count.
const DEFAULT_LEADER_SCHEDULE_LEN: u64 = 16;

/// Default suggested fee rate (atoms per byte) when the mempool is empty.
const DEFAULT_FEE_RATE: u64 = 1;

// ============================================================================
// Helpers
// ============================================================================

/// Format a byte slice as a `0x`-prefixed hex string.
fn to_hex(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

/// Format an integer as an Ethereum-style minimal hex "quantity".
fn quantity<T: std::fmt::LowerHex>(value: T) -> Value {
    Value::String(format!("0x{value:x}"))
}

/// Normalise JSON-RPC `params` into a positional argument list.
///
/// Supports the Ethereum-style array form (`[a, b]`), a bare scalar
/// (`"0x..."`) and the empty/absent form (`null`).
fn params_vec(params: &Value) -> Vec<Value> {
    match params {
        Value::Array(items) => items.clone(),
        Value::Null => Vec::new(),
        other => vec![other.clone()],
    }
}

/// Borrow the `index`-th positional argument, or fail with a clear error.
fn required<'a>(args: &'a [Value], index: usize, what: &str) -> Result<&'a Value, RpcError> {
    args.get(index)
        .ok_or_else(|| RpcError::InvalidParams(format!("missing parameter: {what}")))
}

/// Decode a `0x`-prefixed (or bare) hex string into exactly 32 bytes.
fn parse_bytes32(value: &Value, what: &str) -> Result<[u8; 32], RpcError> {
    let raw = value
        .as_str()
        .ok_or_else(|| RpcError::InvalidParams(format!("{what}: expected a hex string")))?;
    let raw = raw.strip_prefix("0x").unwrap_or(raw);
    let bytes = hex::decode(raw)
        .map_err(|e| RpcError::InvalidParams(format!("{what}: invalid hex: {e}")))?;
    if bytes.len() != 32 {
        return Err(RpcError::InvalidParams(format!(
            "{what}: expected 32 bytes, got {}",
            bytes.len()
        )));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Parse a block/transaction hash parameter.
fn parse_hash(value: &Value, what: &str) -> Result<Hash, RpcError> {
    Ok(Hash(parse_bytes32(value, what)?))
}

/// Parse an account address parameter.
fn parse_address(value: &Value, what: &str) -> Result<Address, RpcError> {
    let raw = value
        .as_str()
        .ok_or_else(|| RpcError::InvalidParams(format!("{what}: expected an address string")))?;
    let raw = raw.trim();
    // Canonical `kvnc…dag` encoding (checksum verified) or a raw 32-byte hex.
    if raw.starts_with(kvnc_types::address::ADDRESS_PREFIX)
        || raw.ends_with(kvnc_types::address::ADDRESS_SUFFIX)
    {
        return raw
            .parse::<Address>()
            .map_err(|e| RpcError::InvalidParams(format!("{what}: {e}")));
    }
    Ok(Address(parse_bytes32(value, what)?))
}

/// Parse an integer parameter accepted as either a JSON number or a
/// `0x`-prefixed quantity string.
fn parse_u64(value: &Value, what: &str) -> Result<u64, RpcError> {
    if let Some(n) = value.as_u64() {
        return Ok(n);
    }
    if let Some(raw) = value.as_str() {
        let raw = raw.strip_prefix("0x").unwrap_or(raw);
        return u64::from_str_radix(raw, 16)
            .map_err(|e| RpcError::InvalidParams(format!("{what}: invalid number: {e}")));
    }
    Err(RpcError::InvalidParams(format!(
        "{what}: expected an integer"
    )))
}

/// Parse a contract storage slot.
///
/// Accepts either a full 32-byte hex key or a small integer (Ethereum-style
/// slot number) which is left-padded big-endian into 32 bytes.
fn parse_storage_key(value: &Value, what: &str) -> Result<[u8; 32], RpcError> {
    if value.is_number() || value.as_str().map(|s| s.len() <= 18).unwrap_or(false) {
        if let Ok(slot) = parse_u64(value, what) {
            let mut key = [0u8; 32];
            key[24..].copy_from_slice(&slot.to_be_bytes());
            return Ok(key);
        }
    }
    parse_bytes32(value, what)
}

/// Map an internal storage/state error onto the JSON-RPC internal-error code.
fn internal<E: std::fmt::Display>(e: E) -> RpcError {
    RpcError::InternalError(e.to_string())
}

/// Serialise a transaction kind into a tagged JSON object.
fn kind_to_json(kind: &TransactionKind) -> Value {
    match kind {
        TransactionKind::Transfer { to, amount } => json!({
            "type": "transfer",
            "to": to_hex(&to.0),
            "amount": quantity(*amount),
        }),
        TransactionKind::Stake { amount } => json!({
            "type": "stake",
            "amount": quantity(*amount),
        }),
        TransactionKind::Unstake { amount } => json!({
            "type": "unstake",
            "amount": quantity(*amount),
        }),
        TransactionKind::Deploy { code } => json!({
            "type": "deploy",
            "code": to_hex(code),
        }),
        TransactionKind::Call {
            contract,
            method,
            args,
            gas_limit,
        } => json!({
            "type": "call",
            "contract": to_hex(&contract.0),
            "method": method,
            "args": to_hex(args),
            "gas_limit": quantity(*gas_limit),
        }),
    }
}

/// Serialise a transaction into JSON (without block context).
pub(crate) fn transaction_to_json(tx: &Transaction) -> Value {
    json!({
        "hash": to_hex(&tx.hash.0),
        "sender": to_hex(&tx.sender.0),
        "nonce": quantity(tx.nonce),
        "fee": quantity(tx.fee),
        "kind": kind_to_json(&tx.kind),
        "signature": to_hex(&tx.signature.0),
    })
}

/// Serialise a block into JSON.
pub(crate) fn block_to_json(block: &StatementBlock) -> Value {
    json!({
        "hash": to_hex(&block.digest.0),
        "digest": to_hex(&block.digest.0),
        "author": block.author,
        "number": quantity(block.round),
        "round": quantity(block.round),
        "parents": block
            .parents
            .iter()
            .map(|p| to_hex(&p.digest.0))
            .collect::<Vec<_>>(),
        "transactions": block
            .transactions
            .iter()
            .map(transaction_to_json)
            .collect::<Vec<_>>(),
        "statements": to_hex(&block.statements),
        "signature": to_hex(&block.signature.0),
    })
}

// ============================================================================
// Chain methods
// ============================================================================

/// `kvnc_blockNumber` — latest committed leader height.
pub async fn handle_block_number(_params: Value, state: RpcState) -> Result<Value, RpcError> {
    let txn = state.storage.begin_read().map_err(internal)?;
    let height = state
        .storage
        .consensus()
        .get_committed_leader_height(&txn)
        .map_err(internal)?;
    Ok(quantity(height))
}

/// `kvnc_getBlockByHash(hash, full_transactions)` — block by digest.
pub async fn handle_get_block_by_hash(params: Value, state: RpcState) -> Result<Value, RpcError> {
    let args = params_vec(&params);
    let hash = parse_hash(required(&args, 0, "block hash")?, "block hash")?;

    let txn = state.storage.begin_read().map_err(internal)?;
    match state.storage.blocks().get_block(&txn, &hash) {
        Ok(block) => Ok(block_to_json(&block)),
        Err(BlockStoreError::NotFound(_)) => Ok(Value::Null),
        Err(e) => Err(internal(e)),
    }
}

/// `kvnc_getBlockByNumber(number, full_transactions)` — block by height/round.
pub async fn handle_get_block_by_number(params: Value, state: RpcState) -> Result<Value, RpcError> {
    let args = params_vec(&params);
    let number = parse_u64(required(&args, 0, "block number")?, "block number")?;

    let txn = state.storage.begin_read().map_err(internal)?;
    match state
        .storage
        .blocks()
        .get_block_by_height(&txn, number)
        .map_err(internal)?
    {
        Some(block) => Ok(block_to_json(&block)),
        None => Ok(Value::Null),
    }
}

// ============================================================================
// Transaction methods
// ============================================================================

/// `kvnc_sendRawTransaction(raw_hex)` — decode and submit a signed tx.
pub async fn handle_send_raw_transaction(
    params: Value,
    state: RpcState,
) -> Result<Value, RpcError> {
    let args = params_vec(&params);
    let raw = required(&args, 0, "raw transaction")?
        .as_str()
        .ok_or_else(|| RpcError::InvalidParams("raw transaction: expected a hex string".into()))?;
    let raw = raw.strip_prefix("0x").unwrap_or(raw);
    let bytes = hex::decode(raw)
        .map_err(|e| RpcError::InvalidParams(format!("raw transaction: invalid hex: {e}")))?;
    let tx: Transaction = bincode::deserialize(&bytes)
        .map_err(|e| RpcError::InvalidParams(format!("raw transaction: invalid encoding: {e}")))?;

    let hash = tx.hash;
    let event_tx = tx.clone();
    state
        .mempool
        .add_transaction(tx)
        .map_err(|e| RpcError::ExecutionError(e.to_string()))?;
    state.events.publish_pending_transaction(&event_tx);

    Ok(json!(to_hex(&hash.0)))
}

/// `kvnc_getTransactionReceipt(hash)` — receipt for a mined transaction.
pub async fn handle_get_transaction_receipt(
    params: Value,
    state: RpcState,
) -> Result<Value, RpcError> {
    let args = params_vec(&params);
    let hash = parse_hash(required(&args, 0, "transaction hash")?, "transaction hash")?;

    let txn = state.storage.begin_read().map_err(internal)?;
    let Some(block_hash) = state
        .storage
        .blocks()
        .get_block_for_transaction(&txn, &hash)
        .map_err(internal)?
    else {
        return Ok(Value::Null);
    };

    let block = state
        .storage
        .blocks()
        .get_block(&txn, &block_hash)
        .map_err(internal)?;
    let index = block.transactions.iter().position(|t| t.hash == hash);

    Ok(json!({
        "transactionHash": to_hex(&hash.0),
        "blockHash": to_hex(&block_hash.0),
        "blockNumber": quantity(block.round),
        "transactionIndex": index.map(|i| quantity(i as u64)).unwrap_or(Value::Null),
        "status": "0x1",
        "success": true,
    }))
}

/// `kvnc_getTransactionByHash(hash)` — transaction with block context.
pub async fn handle_get_transaction_by_hash(
    params: Value,
    state: RpcState,
) -> Result<Value, RpcError> {
    let args = params_vec(&params);
    let hash = parse_hash(required(&args, 0, "transaction hash")?, "transaction hash")?;

    let txn = state.storage.begin_read().map_err(internal)?;
    let Some(block_hash) = state
        .storage
        .blocks()
        .get_block_for_transaction(&txn, &hash)
        .map_err(internal)?
    else {
        return Ok(Value::Null);
    };

    let block = state
        .storage
        .blocks()
        .get_block(&txn, &block_hash)
        .map_err(internal)?;
    let Some(index) = block.transactions.iter().position(|t| t.hash == hash) else {
        return Ok(Value::Null);
    };
    let tx = &block.transactions[index];

    let mut value = transaction_to_json(tx);
    if let Value::Object(ref mut map) = value {
        map.insert("blockHash".into(), json!(to_hex(&block_hash.0)));
        map.insert("blockNumber".into(), quantity(block.round));
        map.insert("transactionIndex".into(), quantity(index as u64));
    }
    Ok(value)
}

// ============================================================================
// Account methods
// ============================================================================

/// `kvnc_getBalance(address)` — native KVNC balance in atoms.
pub async fn handle_get_balance(params: Value, state: RpcState) -> Result<Value, RpcError> {
    let args = params_vec(&params);
    let address = parse_address(required(&args, 0, "address")?, "address")?;

    let txn = state.storage.begin_read().map_err(internal)?;
    let account = state
        .storage
        .state()
        .get_account_or_default(&txn, &address)
        .map_err(internal)?;
    Ok(quantity(account.balance))
}

/// `kvnc_getNonce(address)` — account nonce.
pub async fn handle_get_nonce(params: Value, state: RpcState) -> Result<Value, RpcError> {
    let args = params_vec(&params);
    let address = parse_address(required(&args, 0, "address")?, "address")?;

    let txn = state.storage.begin_read().map_err(internal)?;
    let account = state
        .storage
        .state()
        .get_account_or_default(&txn, &address)
        .map_err(internal)?;
    Ok(quantity(account.nonce))
}

/// `kvnc_getCode(address)` — contract bytecode as hex.
pub async fn handle_get_code(params: Value, state: RpcState) -> Result<Value, RpcError> {
    let args = params_vec(&params);
    let address = parse_address(required(&args, 0, "address")?, "address")?;

    let txn = state.storage.begin_read().map_err(internal)?;
    let account = state
        .storage
        .state()
        .get_account_or_default(&txn, &address)
        .map_err(internal)?;

    let code = if account.code_hash == [0u8; 32] {
        Vec::new()
    } else {
        state
            .storage
            .state()
            .get_contract_code(&txn, &account.code_hash)
            .map_err(internal)?
            .unwrap_or_default()
    };
    Ok(json!(to_hex(&code)))
}

/// `kvnc_getStorageAt(address, key)` — raw contract storage slot as hex.
pub async fn handle_get_storage_at(params: Value, state: RpcState) -> Result<Value, RpcError> {
    let args = params_vec(&params);
    let address = parse_address(required(&args, 0, "address")?, "address")?;
    let key = parse_storage_key(required(&args, 1, "storage key")?, "storage key")?;

    let txn = state.storage.begin_read().map_err(internal)?;
    let value = state
        .storage
        .state()
        .get_storage(&txn, &address, &key)
        .map_err(internal)?
        .unwrap_or_default();
    Ok(json!(to_hex(&value)))
}

// ============================================================================
// Staking methods
// ============================================================================

/// `kvnc_getValidators()` — active validator set.
pub async fn handle_get_validators(_params: Value, state: RpcState) -> Result<Value, RpcError> {
    let staking = state.staking.read().await;
    let validators: Vec<Value> = staking
        .validators
        .iter()
        .map(|v| {
            json!({
                "address": to_hex(&v.address.0),
                "stake": quantity(v.stake),
                "commission_bps": v.commission_bps,
                "active": v.active,
                "payout_address": to_hex(&v.payout_address.0),
            })
        })
        .collect();
    Ok(Value::Array(validators))
}

/// `kvnc_getStake(address)` — total stake attributable to an address.
///
/// Sums the address's own validator stake (if any) and any delegations it
/// has made.
pub async fn handle_get_stake(params: Value, state: RpcState) -> Result<Value, RpcError> {
    let args = params_vec(&params);
    let address = parse_address(required(&args, 0, "address")?, "address")?;

    let staking = state.staking.read().await;
    let own_stake: u64 = staking
        .validators
        .iter()
        .filter(|v| v.address == address)
        .map(|v| v.stake)
        .sum();
    let delegated: u64 = staking
        .delegations
        .iter()
        .filter(|d| d.delegator == address)
        .map(|d| d.amount)
        .sum();
    Ok(quantity(own_stake.saturating_add(delegated)))
}

/// `kvnc_getRewards(address)` — cumulative rewards for an address.
pub async fn handle_get_rewards(params: Value, state: RpcState) -> Result<Value, RpcError> {
    let args = params_vec(&params);
    let _address = parse_address(required(&args, 0, "address")?, "address")?;

    // TODO: `StakingState` tracks only the aggregate `total_mining_issued`
    // counter; per-address cumulative rewards are not recorded yet. Return 0
    // until reward accounting is added to the staking state.
    let _ = state;
    Ok(quantity(0u128))
}

// ============================================================================
// Mempool methods
// ============================================================================

/// `kvnc_getPendingTransactions()` — hashes of transactions in the mempool.
pub async fn handle_get_pending_transactions(
    _params: Value,
    state: RpcState,
) -> Result<Value, RpcError> {
    // TODO: `Mempool` exposes `get(hash)`, `contains(hash)` and aggregate
    // `stats()`, but no non-destructive listing of pending hashes. Return an
    // empty list until such an accessor exists.
    let _ = state;
    Ok(Value::Array(Vec::new()))
}

/// `kvnc_estimateFee()` — suggested fee rate in atoms per byte.
pub async fn handle_estimate_fee(_params: Value, state: RpcState) -> Result<Value, RpcError> {
    let stats = state.mempool.stats();
    let rate = stats
        .fee_rates
        .iter()
        .map(|(rate, _)| *rate)
        .min()
        .unwrap_or(DEFAULT_FEE_RATE)
        .max(DEFAULT_FEE_RATE);
    Ok(quantity(rate))
}

// ============================================================================
// Consensus methods
// ============================================================================

/// `kvnc_getLeaderSchedule(count)` — upcoming round leaders.
pub async fn handle_get_leader_schedule(params: Value, state: RpcState) -> Result<Value, RpcError> {
    let args = params_vec(&params);
    let count = match args.first() {
        Some(value) => parse_u64(value, "count")?,
        None => DEFAULT_LEADER_SCHEDULE_LEN,
    };

    let start = {
        let txn = state.storage.begin_read().map_err(internal)?;
        state
            .storage
            .consensus()
            .get_committed_leader_height(&txn)
            .map_err(internal)?
            .saturating_add(1)
    };

    let committee = &state.committee;
    let schedule: Vec<Value> = (0..count)
        .map(|offset| {
            let round = start.saturating_add(offset);
            let leader_index = committee.leader(round);
            let leader_address = committee
                .get_by_index(leader_index)
                .map(|a| to_hex(&a.address.0))
                .unwrap_or_default();
            json!({
                "round": quantity(round),
                "leader_index": leader_index,
                "leader_address": leader_address,
            })
        })
        .collect();
    Ok(Value::Array(schedule))
}

/// `kvnc_getCommittee()` — current consensus committee.
pub async fn handle_get_committee(_params: Value, state: RpcState) -> Result<Value, RpcError> {
    let committee = &state.committee;
    let members: Vec<Value> = committee
        .authorities()
        .iter()
        .map(|a| {
            json!({
                "index": a.index,
                "address": to_hex(&a.address.0),
                "stake": quantity(a.stake),
                "network_address": a.network_address,
            })
        })
        .collect();
    Ok(Value::Array(members))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvnc_consensus::{AuthorityInfo, CommitteeInfo};
    use kvnc_mempool::{Mempool, MempoolConfig};
    use kvnc_staking::{StakingState, MIN_VALIDATOR_STAKE};
    use kvnc_storage::Storage;
    use kvnc_types::crypto::PublicKey;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn hex32(byte: u8) -> String {
        format!("0x{}", hex::encode([byte; 32]))
    }

    fn test_state(storage: Arc<Storage>) -> RpcState {
        let authority = AuthorityInfo {
            index: 0,
            stake: MIN_VALIDATOR_STAKE,
            public_key: PublicKey([7u8; 32]),
            address: Address([9u8; 32]),
            network_address: "127.0.0.1:9000".to_string(),
        };
        RpcState {
            mempool: Arc::new(Mempool::new(MempoolConfig::default(), storage.clone())),
            storage,
            staking: Arc::new(RwLock::new(StakingState::new())),
            committee: CommitteeInfo::try_new(0, vec![authority]).expect("test committee is valid"),
            peer_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            events: crate::EventBus::new(),
        }
    }

    fn open_storage() -> (tempfile::TempDir, Arc<Storage>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Arc::new(Storage::new(dir.path().join("state.db")).expect("storage"));
        (dir, storage)
    }

    #[test]
    fn parses_hash_and_address_hex() {
        let value = Value::String(hex32(0xab));
        assert_eq!(parse_hash(&value, "hash").unwrap(), Hash([0xab; 32]));
        assert_eq!(
            parse_address(&value, "address").unwrap(),
            Address([0xab; 32])
        );

        assert!(parse_hash(&Value::String("0x1234".into()), "hash").is_err());
        assert!(parse_hash(&Value::String("not-hex".into()), "hash").is_err());
    }

    #[test]
    fn parses_quantities_in_decimal_and_hex() {
        assert_eq!(parse_u64(&json!(42), "n").unwrap(), 42);
        assert_eq!(parse_u64(&json!("0x2a"), "n").unwrap(), 42);
        assert!(parse_u64(&json!("nope"), "n").is_err());
        assert_eq!(quantity(0u64), json!("0x0"));
        assert_eq!(quantity(255u64), json!("0xff"));
    }

    #[test]
    fn parses_storage_key_as_slot_or_hash() {
        let mut expected = [0u8; 32];
        expected[31] = 5;
        assert_eq!(parse_storage_key(&json!(5), "key").unwrap(), expected);
        assert_eq!(
            parse_storage_key(&Value::String(hex32(0x01)), "key").unwrap(),
            [0x01u8; 32]
        );
    }

    #[test]
    fn normalises_params_forms() {
        assert_eq!(params_vec(&json!([1, 2])).len(), 2);
        assert_eq!(params_vec(&json!("x")), vec![json!("x")]);
        assert!(params_vec(&Value::Null).is_empty());
    }

    #[tokio::test]
    async fn query_handlers_read_real_storage() {
        let (_dir, storage) = open_storage();
        let state = test_state(storage);

        assert_eq!(
            handle_block_number(Value::Null, state.clone())
                .await
                .unwrap(),
            json!("0x0")
        );
        assert_eq!(
            handle_get_balance(json!([hex32(0x11)]), state.clone())
                .await
                .unwrap(),
            json!("0x0")
        );
        assert_eq!(
            handle_get_nonce(json!([hex32(0x11)]), state.clone())
                .await
                .unwrap(),
            json!("0x0")
        );
        assert_eq!(
            handle_get_code(json!([hex32(0x11)]), state.clone())
                .await
                .unwrap(),
            json!("0x")
        );
        assert_eq!(
            handle_get_block_by_hash(json!([hex32(0x22)]), state.clone())
                .await
                .unwrap(),
            Value::Null
        );

        let committee = handle_get_committee(Value::Null, state.clone())
            .await
            .unwrap();
        assert_eq!(committee.as_array().unwrap().len(), 1);
        assert_eq!(committee[0]["address"], json!(hex32(0x09)));

        let schedule = handle_get_leader_schedule(Value::Null, state.clone())
            .await
            .unwrap();
        assert_eq!(
            schedule.as_array().unwrap().len(),
            DEFAULT_LEADER_SCHEDULE_LEN as usize
        );

        let validators = handle_get_validators(Value::Null, state.clone())
            .await
            .unwrap();
        assert!(validators.as_array().unwrap().is_empty());

        let pending = handle_get_pending_transactions(Value::Null, state.clone())
            .await
            .unwrap();
        assert!(pending.as_array().unwrap().is_empty());

        assert_eq!(
            handle_estimate_fee(Value::Null, state).await.unwrap(),
            json!("0x1")
        );
    }

    #[tokio::test]
    async fn estimate_fee_uses_minimum_observed_rate_and_floor() {
        use kvnc_storage::state_store::Account;
        use kvnc_types::crypto::{Signature, SigningKey};

        let (_dir, storage) = open_storage();
        let state = test_state(storage.clone());

        // Two distinct funded senders so both fee-rate samples are admissible
        // (a valid signature is required now that addresses are public keys).
        let (sk_a, pk_a) = kvnc_crypto::generate_keypair();
        let (sk_b, pk_b) = kvnc_crypto::generate_keypair();
        let sender_a = Address::from_public_key(&pk_a);
        let sender_b = Address::from_public_key(&pk_b);

        let txn = storage.begin_write().unwrap();
        {
            let st = storage.state();
            let account = Account {
                balance: 1_000_000,
                nonce: 0,
                code_hash: [0; 32],
                code: Vec::new(),
            };
            st.set_account(&txn, &sender_a, &account).unwrap();
            st.set_account(&txn, &sender_b, &account).unwrap();
        }
        txn.commit().unwrap();

        let make_signed = |sender: Address, to: Address, fee: u64, sk: &SigningKey| {
            let mut tx = Transaction {
                sender,
                nonce: 0,
                kind: TransactionKind::Transfer { to, amount: 1 },
                fee,
                signature: Signature([0; 64]),
                hash: Hash::zero(),
            };
            let signing_hash = tx.signing_hash();
            tx.signature = kvnc_crypto::sign(sk, signing_hash.as_ref());
            tx.hash = signing_hash;
            tx
        };

        let low_rate_tx = make_signed(sender_a, Address([2; 32]), 1, &sk_a);
        let serialized_size = bincode::serialize(&low_rate_tx).unwrap().len() as u64;
        let higher_rate_tx = make_signed(sender_b, Address([3; 32]), serialized_size * 4, &sk_b);

        state
            .mempool
            .add_transaction(low_rate_tx)
            .expect("admit nonzero-fee transaction");
        state
            .mempool
            .add_transaction(higher_rate_tx)
            .expect("admit higher-fee transaction");

        assert_eq!(
            state.mempool.stats().fee_rates,
            vec![(0, 1), (4, 1)],
            "fixture must contain distinct observed rates"
        );
        assert_eq!(
            handle_estimate_fee(Value::Null, state).await.unwrap(),
            json!("0x1"),
            "the minimum observed zero rate is advisory and floored at 1 atom/byte"
        );
    }
}
