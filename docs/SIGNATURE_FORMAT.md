# KUNA Signature Format v1 (SPEC — FINAL)

Status: **FINAL spec**. Implemented in `kvnc-types`, `kvnc-crypto`, `kvnc-cli` and `kvnc-faucet`
(branch `bot/exec-foundation/sig-v1-migration`); the golden vectors in §4 are unit tests there.
The other crates still call the old API and **do not compile until their owners switch**, see
[Call-site checklist](#call-site-checklist). All crates must switch in the same release.
Owner: Exec-Foundation (kvnc-types / kvnc-crypto). Format decisions: Main.
Address format (`kvnc…dag`) and `docs/TOKENOMICS.md` are not affected. Domain tags use the ticker **KUNA**. Crate, binary and address names keep `kvnc`.

## Goals

1. **Domain separation**: a vote signature can never be valid as a transaction signature, and the reverse is also true.
2. **Replay protection across networks**: every signed message commits to `chain_id`.
3. **Voter binding**: the vote commits to the voter's index and the epoch, so a signature cannot be reassigned to another committee slot or epoch.
4. **Single format change**: everything that changes the signed bytes, including `Call.value`, lands in v1 at once.

## Conventions

- All integers are **little-endian**, fixed width, unsigned.
- `DomainTag16(s)`: the ASCII string `s` right-padded with `0x00` to exactly **16 bytes**. Tags have a fixed width, so no length prefix is needed and the parsing is unambiguous. All tags are ≤ 15 bytes, so each one ends in at least one `0x00`.
- Signature scheme: **Ed25519** (`ed25519-dalek` 2.x, RFC 8032), pubkey = the raw 32 bytes, which are also the address bytes.
- Hash: **BLAKE3 keyed mode** (`blake3 =1.5.1`), as in `Hash::new_keyed(domain, data)`: key = the domain bytes right-padded with `0x00` to 32 bytes.

### chain_id: `u64` LE (chosen) vs 32-byte genesis hash

**`chain_id: u64` LE** was chosen because:
- wallets, the CLI and hardware/offline signers can configure and show it without access to the genesis file (`--chain-id 2`)
- it is cheap (8 bytes) in every vote, and votes are the hottest signed message
- it does not change on a re-genesis of the same network (testnet resets), which is the desired behaviour for tooling. A genesis hash would silently break every pre-built signer after each reset.

The downside (two networks could choose the same number) is handled by a registry
in this document. The node MUST refuse to start if the configured `chain_id` does
not match the one in its genesis config.

Registry (**final**): `1` = mainnet, `2` = testnet, `3` = devnet, `1337` = local. The test vectors below use `chain_id = 2`.

## 1. Vote `signature_data` v1 (74 bytes, signed directly)

| Offset | Size | Field          | Encoding                                  |
|-------:|-----:|----------------|-------------------------------------------|
| 0      | 16   | domain tag     | `DomainTag16("KUNA/vote/v1")`             |
| 16     | 8    | chain_id       | u64 LE                                    |
| 24     | 8    | epoch          | u64 LE (committee epoch; `0` until epochs are live) |
| 32     | 2    | voter          | u16 LE (`AuthorityIndex` = u16)           |
| 34     | 8    | leader_round   | u64 LE (`Round` = u64)                    |
| 42     | 32   | leader_hash    | raw 32 bytes                              |

`signature = Ed25519.sign(sk_voter, signature_data)` (the 74 bytes are signed without prehashing).
The verifier MUST rebuild the bytes from its **own** `chain_id` and current `epoch`, and take
`voter` from the message, then look up `pubkey = committee[epoch][voter]`. It must never
take chain_id or epoch from the network message.

The API contract is now `verify_vote_signature(ctx: &SigningContext, vote: &Vote, pubkey: &PublicKey) -> Result<(), SigError>`
(`SigningContext { chain_id, epoch }` lives in `kvnc_types::signing`, together with the `chain_id` registry constants).

Current PROVISIONAL format for comparison: `leader_round u64 LE ‖ leader_hash` (40 bytes).

## 2. Transaction signing hash v1

Preimage:

| Size | Field        | Encoding                                       |
|-----:|--------------|------------------------------------------------|
| 16   | domain tag   | `DomainTag16("KUNA/tx/v1")`                    |
| 8    | chain_id     | u64 LE                                         |
| 32   | sender       | `Address.0` (raw pubkey)                        |
| 8    | nonce        | u64 LE                                         |
| var  | kind         | see below (unchanged from PROVISIONAL)         |
| 8    | fee          | u64 LE                                         |

`kind` encoding (byte-for-byte identical to the current `Transaction::signing_hash`,
`crates/kvnc-types/src/transaction.rs` ~L83-L135; the tag values are **not** in declaration order):

| Variant        | tag u8 | Fields after tag |
|----------------|-------:|------------------|
| Transfer       | 0      | `to` 32 B, `amount` u64 LE |
| Stake          | 1      | `amount` u64 LE |
| Unstake        | 2      | `amount` u64 LE |
| Deploy         | 3      | `len(code)` u64 LE, `code` bytes |
| Call           | 4      | `contract` 32 B, `value` u64 LE (new in v1, see §3), `len(method)` u64 LE, `method` UTF-8 bytes, `len(args)` u64 LE, `args`, `gas_limit` u64 LE |
| Delegate       | 5      | `validator` 32 B, `amount` u64 LE |
| ClaimRewards   | 6      | `0x00` (None) or `0x01` + `validator` 32 B |

```
signing_hash = BLAKE3_keyed(key = pad32("KUNA-TX-v1"), preimage)   // = Hash::new_keyed(b"KUNA-TX-v1", preimage)
signature    = Ed25519.sign(sk_sender, signing_hash)                // 32-byte message
```

The keyed-hash key changes from the PROVISIONAL `Hash::DOMAIN_TX = "KVNC-TX-v1"` to **`"KUNA-TX-v1"`** (ticker
rename KVNC → KUNA). The migration must update `Hash::DOMAIN_TX`. The in-preimage tag `KUNA/tx/v1` versions the
*layout*, so a future v2 changes the tag without touching the hashing primitives. `Transaction.hash`
stays equal to `signing_hash` (checked in `kvnc-dag/src/block_manager.rs:358`).

Current PROVISIONAL format for comparison: the same preimage **without** the first 24 bytes (tag + chain_id)
and without `Call.value`. In v1 `kind` is byte-identical to PROVISIONAL **except** `Call`, which gains `value`.

## 3. `value: u64` in `TransactionKind::Call` (decided: IN v1)

Requested by Exec-Execution and decided by Main. `Call` carries a signed `value: u64`, the native KUNA
transferred to the contract. Position: right after `contract`, before `method`, 8 bytes u64 LE, **always present**
(`0` when unused). There is no optional flag, so the encoding has a fixed shape. Adding the field to
`TransactionKind::Call` is part of the v1 migration.

## 4. Test vectors

Generated with a throwaway Rust program (`blake3 =1.5.1`, `ed25519-dalek 2.1`).
Key: seed = `07` × 32 →
pubkey/sender = `ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c`.
Ed25519 is deterministic, so the signatures can be reproduced.

### V1: Vote (chain_id=2, epoch=7, voter=3, leader_round=42, leader_hash=`11`×32)
```
len      = 74
preimage = 4b554e412f766f74652f7631000000000200000000000000070000000000000003002a000000000000001111111111111111111111111111111111111111111111111111111111111111
sig      = 04edabde7c75345b9c4e12a793b2a4f05948c6edb5cbd197056fd68ef87d2a4c5311ff5df6eb6322c8fc229097079410873af37dceb4b43af66722005415a70b
```

### T1: Transfer (chain_id=2, nonce=5, to=`22`×32, amount=1000000, fee=1000)
```
len          = 113
preimage     = 4b554e412f74782f76310000000000000200000000000000ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c050000000000000000222222222222222222222222222222222222222222222222222222222222222240420f0000000000e803000000000000
signing_hash = 4acde0e0a120d6c03eaa197c082238fefadaf36f4f40bc1364ae8b3faa067798
sig          = 2413f8c3a1508e2b1b2809c5cc9cc74982118d47e8a8e9afaab2d2719349724265b8f80d67359b579001b9c4845723edb9530372a79da9f4093d2f7228820d0e
```

### T2: Call, value=0 (chain_id=2, nonce=6, contract=`33`×32, value=0, method="transfer", args=010203, gas_limit=100000, fee=10)
```
len          = 148
preimage     = 4b554e412f74782f76310000000000000200000000000000ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c0600000000000000043333333333333333333333333333333333333333333333333333333333333333000000000000000008000000000000007472616e736665720300000000000000010203a0860100000000000a00000000000000
signing_hash = 439e1830573896f46dc642cedb37605c1e478880f417f0cfae5126eaca48026a
sig          = 0ff9438c1ea9e8a70e3b491d873c712e8e7d0339b637b00def16d42eea4eed3301454ef10f61b958bf37c95351a89f01270fe194ab8fa046b45373ee54e47f07
```

### T3: Call, value=500 (otherwise identical to T2)
```
len          = 148
preimage     = 4b554e412f74782f76310000000000000200000000000000ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c0600000000000000043333333333333333333333333333333333333333333333333333333333333333f40100000000000008000000000000007472616e736665720300000000000000010203a0860100000000000a00000000000000
signing_hash = 9b76e7fc940641c6e11226b537841ace9ceb000a3943f6b3923b3cf2c6f4f794
sig          = ce4e4d5dcc49159d2b6f72faa3210fe4db5b040e93eadbe315a159afe61f32ff66fc0abf9591c902500a1ce4e02d3816dbae80b8f1721725f03ed52b119f6608
```

When the spec is implemented, these vectors become golden tests in `kvnc-types`.

## Migration

This is a hard fork of the signed bytes. Every node must switch at the same release, with no dual-acceptance
window (accepting both formats would bring back the cross-chain replay that v1 removes).
These must change **in the same release**:

- **kvnc-types** (Foundation): `Vote::signature_data(ctx)`, `Transaction::signing_hash(ctx)`, `SigningContext { chain_id, epoch }`, `Hash::DOMAIN_TX` → `"KUNA-TX-v1"`, golden vectors. `TransactionKind::Call.value: u64`.
- **kvnc-crypto** (Foundation): `verify_vote_signature(ctx, vote, pubkey)`.
- **kvnc-consensus** (Consensus): `engine.rs:510` signs `vote.signature_data()`. `engine.rs:532` has a **duplicate** `vote_signature_data(leader_round, leader_hash)` helper with the old encoding. It must be deleted, not updated, so there is only one encoder. Vote verification must use the kvnc-crypto API.
- **kvnc-execution** (Execution): signing-hash callers (`lib.rs:904`), removal of the test-signature shortcut (`lib.rs:304-307`), and `Call.value` semantics (transfer `value` to the contract; insufficient balance = reject).
- **Open PRs with vote signature verification: #3 (consensus) and #4 (network)** must verify against v1 `signature_data` (via the kvnc-crypto API with `SigningContext`) before or together with the migration. Merging them with the PROVISIONAL 40-byte encoding would add a third encoder to remove.
- **kvnc-network / kvnc-rpc / kvnc-node** (Network): votes and txs on the wire stay the same shape except for `Call.value`. `chain_id` must be in node config and checked against genesis, and RPC `chain_methods.rs:795` must verify with the context.
- **kvnc-mempool** (`lib.rs:410,451`), **kvnc-dag** (`block_manager.rs:358`), **kvnc-cli**, **kvnc-faucet** (`main.rs:341`), and test helpers in `kvnc-node/tests/live_vote_integration.rs:215`: pass the `chain_id`.
- Genesis/config: add `chain_id`. Existing devnets must be reset (old signatures do not verify).

### Call-site checklist

Implemented API (kvnc-types / kvnc-crypto): `Vote::signature_data(&ctx)`, `Transaction::signing_hash(&ctx)`,
`Transaction::verify_signature(&ctx)`, `kvnc_crypto::verify_vote_signature(&ctx, &vote, &pubkey)`,
`TransactionKind::Call { contract, value, method, args, gas_limit }`, `Hash::DOMAIN_TX = "KUNA-TX-v1"`.
`SigningContext::new(chain_id).with_epoch(epoch)` builds a context; the verifier must create it from its **own**
config and epoch. `kvnc-cli` and `kvnc-faucet` need `--chain-id` (no default).
Line numbers are against `main` at the time of the migration PR.

| Owner | File:line | Change |
|-------|-----------|--------|
| Consensus | `kvnc-consensus/src/engine.rs:510` | sign `vote.signature_data(&ctx)` |
| Consensus | `engine.rs:532` | delete the duplicate `vote_signature_data` encoder |
| Consensus | `engine.rs:636` | verify via `verify_vote_signature(&ctx, ..)` |
| Consensus | `engine.rs:1058`, `tests/common/mod.rs:71` | test signers use a ctx |
| Network | `kvnc-network/src/validation.rs:53,138,182`, `service.rs:1392` | verify and sign with ctx; `chain_id` in config |
| Network | `kvnc-node/src/main.rs:2003`, `kvnc-node/tests/live_vote_integration.rs:233` | ctx; check `chain_id` against genesis |
| Network | `kvnc-rpc/src/chain_methods.rs:173,814` | `Call.value` field; `signing_hash(&ctx)` |
| Network | `kvnc-mempool/src/lib.rs:272,282,682,723`, `kvnc-dag/src/block_manager.rs:358` | `signing_hash(&ctx)` / `verify_signature(&ctx)` |
| Execution | `kvnc-execution/src/lib.rs:381,904` | `Call.value` semantics; `signing_hash(&ctx)` |
