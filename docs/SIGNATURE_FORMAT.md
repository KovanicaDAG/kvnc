# KVNC Signature Format v1 (SPEC — PROPOSED)

Status: **PROPOSED spec, not implemented.** The signing code on `main`
(`Vote::signature_data`, `Transaction::signing_hash`) is unchanged and remains
**PROVISIONAL** until all crates listed in [Migration](#migration) switch together.
Owner: Exec-Foundation (kvnc-types / kvnc-crypto). Format decisions: Main.
Address format (`kvnc…dag`) and `docs/TOKENOMICS.md` are not affected.

## Goals

1. **Domain separation**: a vote signature can never be valid as a transaction signature, and the reverse is also true.
2. **Replay protection across networks**: every signed message commits to `chain_id`.
3. **Voter binding**: the vote commits to the voter's index and the epoch, so a signature cannot be reassigned to another committee slot or epoch.
4. **Single format change**: everything that changes the signed bytes, including the open `Call.value` question, lands in v1 at once.

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

Proposed registry (**open for Main**): `1` = mainnet, `2` = public testnet,
`3` = devnet, `1337` = local/dev. The test vectors below use `chain_id = 2`.

## 1. Vote `signature_data` v1 (74 bytes, signed directly)

| Offset | Size | Field          | Encoding                                  |
|-------:|-----:|----------------|-------------------------------------------|
| 0      | 16   | domain tag     | `DomainTag16("KVNC/vote/v1")`             |
| 16     | 8    | chain_id       | u64 LE                                    |
| 24     | 8    | epoch          | u64 LE (committee epoch; `0` until epochs are live) |
| 32     | 2    | voter          | u16 LE (`AuthorityIndex` = u16)           |
| 34     | 8    | leader_round   | u64 LE (`Round` = u64)                    |
| 42     | 32   | leader_hash    | raw 32 bytes                              |

`signature = Ed25519.sign(sk_voter, signature_data)` (the 74 bytes are signed without prehashing).
The verifier MUST rebuild the bytes from its **own** `chain_id` and current `epoch`, and take
`voter` from the message, then look up `pubkey = committee[epoch][voter]`. It must never
take chain_id or epoch from the network message.

The API contract `verify_vote_signature(vote: &Vote, pubkey: &PublicKey) -> Result<(), SigError>`
(PR #9) will then take `chain_id` and `epoch` from a context argument, e.g.
`verify_vote_signature(ctx: &SigningContext, vote, pubkey)`. That signature change is part of the v1 switch.

Current PROVISIONAL format for comparison: `leader_round u64 LE ‖ leader_hash` (40 bytes).

## 2. Transaction signing hash v1

Preimage:

| Size | Field        | Encoding                                       |
|-----:|--------------|------------------------------------------------|
| 16   | domain tag   | `DomainTag16("KVNC/tx/v1")`                    |
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
| Call           | 4      | `contract` 32 B, [`value` u64 LE, *open, see §3*], `len(method)` u64 LE, `method` UTF-8 bytes, `len(args)` u64 LE, `args`, `gas_limit` u64 LE |
| Delegate       | 5      | `validator` 32 B, `amount` u64 LE |
| ClaimRewards   | 6      | `0x00` (None) or `0x01` + `validator` 32 B |

```
signing_hash = BLAKE3_keyed(key = pad32("KVNC-TX-v1"), preimage)   // = Hash::new_keyed(Hash::DOMAIN_TX, preimage)
signature    = Ed25519.sign(sk_sender, signing_hash)                // 32-byte message
```

The keyed-hash key (`Hash::DOMAIN_TX`) is kept as it is. The in-preimage tag `KVNC/tx/v1` versions the
*layout*, so a future v2 changes the tag without touching the hashing primitives. `Transaction.hash`
stays equal to `signing_hash` (checked in `kvnc-dag/src/block_manager.rs:358`).

Current PROVISIONAL format for comparison: the same preimage **without** the first 24 bytes (tag + chain_id)
and without `Call.value`.

## 3. Open: `value: u64` in `TransactionKind::Call` (Exec-Execution request)

Exec-Execution has asked for a signed `value: u64` (native KVNC sent with a contract call).
**Proposal:** include it in v1 so the signed format changes only once. Position: right after
`contract`, before `method`, fixed 8 bytes u64 LE, always present (`0` when there is no transfer).
It has no optional flag, so the encoding stays fixed-shape.
**Status: OPEN for Main's decision.** Vectors are given both with and without it.

## 4. Test vectors

Generated with a throwaway Rust program (`blake3 =1.5.1`, `ed25519-dalek 2.1`).
Key: seed = `07` × 32 →
pubkey/sender = `ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c`.
Ed25519 is deterministic, so the signatures can be reproduced.

### V1: Vote (chain_id=2, epoch=7, voter=3, leader_round=42, leader_hash=`11`×32)
```
len      = 74
preimage = 4b564e432f766f74652f7631000000000200000000000000070000000000000003002a000000000000001111111111111111111111111111111111111111111111111111111111111111
sig      = 379fbdd70d376ef255a8e5cbd02fb5ab0eeedad2a960ab4c09640d3d535f84fd331bfcced53ccb2303c494526b7809b66099179073fbf23b62757d79ea2c0602
```

### T1: Transfer (chain_id=2, nonce=5, to=`22`×32, amount=1000000, fee=1000)
```
len          = 113
preimage     = 4b564e432f74782f76310000000000000200000000000000ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c050000000000000000222222222222222222222222222222222222222222222222222222222222222240420f0000000000e803000000000000
signing_hash = 0aae8c1832e09bf7da6f897d20b8bc6a017aaeefd50ea10015218a5c4a54c21d
sig          = c24cd02ec98045a34e4b3102a1cafe70883a83763d5915528554b54797b139df1768cad71220e78bcb6702b109f37bb79513c1f768bacab4bf6e96f4e16cdf06
```

### T2: Call WITHOUT value (chain_id=2, nonce=6, contract=`33`×32, method="transfer", args=010203, gas_limit=100000, fee=10)
```
len          = 140
preimage     = 4b564e432f74782f76310000000000000200000000000000ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c060000000000000004333333333333333333333333333333333333333333333333333333333333333308000000000000007472616e736665720300000000000000010203a0860100000000000a00000000000000
signing_hash = 29440c1f5bdf3088f33761473c82d39227f0dbbf94fc234ad6996dbab62af429
sig          = 4835ac6bced827c7ca14d0642d7f84a7ab51efda72335a8d8ca0a9f40565f7d9d10691d9f17c791f16bffb89bd512f86f5db09c37a4c2d3190203ad87df74e06
```

### T3: Call WITH value=500 (proposal §3; otherwise identical to T2)
```
len          = 148
preimage     = 4b564e432f74782f76310000000000000200000000000000ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c0600000000000000043333333333333333333333333333333333333333333333333333333333333333f40100000000000008000000000000007472616e736665720300000000000000010203a0860100000000000a00000000000000
signing_hash = 616a96c5e4568b1e165670cd9c4afb4c8d2dd603dbf5b61142f7187a93d29df7
sig          = 48b532f19c6867e21907f829614864ca5cc54baf0177eab1dc4bbbc48b192d48507acdbd97731621c76498e33fb68ccfc37979cd0f17a959a14b94d159606c0b
```

When the spec is implemented, these vectors become golden tests in `kvnc-types`.

## Migration

This is a hard fork of the signed bytes. Every node must switch at the same release, with no dual-acceptance
window (accepting both formats would bring back the cross-chain replay that v1 removes).
These must change **in the same release**:

- **kvnc-types** (Foundation): `Vote::signature_data(ctx)`, `Transaction::signing_hash(ctx)`, `SigningContext { chain_id, epoch }`, golden vectors. If §3 is accepted: `TransactionKind::Call.value`.
- **kvnc-crypto** (Foundation): `verify_vote_signature(ctx, vote, pubkey)`.
- **kvnc-consensus** (Consensus): `engine.rs:510` signs `vote.signature_data()`. `engine.rs:532` has a **duplicate** `vote_signature_data(leader_round, leader_hash)` helper with the old encoding. It must be deleted, not updated, so there is only one encoder. Vote verification must use the kvnc-crypto API.
- **kvnc-execution** (Execution): signing-hash callers (`lib.rs:904`), removal of the test-signature shortcut (`lib.rs:304-307`), and `Call.value` semantics if accepted.
- **kvnc-network / kvnc-rpc / kvnc-node** (Network): votes and txs on the wire stay the same shape except for `Call.value`. `chain_id` must be in node config and checked against genesis, and RPC `chain_methods.rs:795` must verify with the context.
- **kvnc-mempool** (`lib.rs:410,451`), **kvnc-dag** (`block_manager.rs:358`), **kvnc-cli**, **kvnc-faucet** (`main.rs:341`), and test helpers in `kvnc-node/tests/live_vote_integration.rs:215`: pass the `chain_id`.
- Genesis/config: add `chain_id`. Existing devnets must be reset (old signatures do not verify).
