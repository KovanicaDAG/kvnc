# KUNA Contract Examples

Worked end-to-end flows for the four contract crates. All amounts are in
**atoms** (base units): `1 KUNA = 1_000_000_000 atoms` (9 decimals).

> **CLI status.** `kvnc-cli` is currently a skeleton (`keygen`, `info`,
> `transfer` only). The commands below are the planned CLI surface for the
> contract subcommands — they map 1:1 onto the typed APIs in
> [`docs/CONTRACTS.md`](CONTRACTS.md) §3 and the wasm entry points in §4.
> Until the CLI wires them up, drive the same flows through the typed Rust
> API or `POST /api/prepare` + sign + `POST /api/submit` (see
> [`docs/ARCHITECTURE.md`](ARCHITECTURE.md)).

---

## HTLC atomic swap (KVP-104)

Alice locks KUNA for Bob; Bob claims with the preimage before expiry, or
Alice refunds after expiry.

```bash
# 1. Alice locks 100 KUNA for Bob.
#    hash-lock = BLAKE3-256(preimage) as hex; expiry = unix timestamp.
kvnc htlc create \
  --claimer <Bob-address> \
  --amount 100000000000 \
  --hash-lock <64-hex-chars> \
  --expiry <unix-seconds>

# → returns swap_id (32 bytes, hex)

# 2. Bob claims with the preimage before expiry.
kvnc htlc claim \
  --id <swap_id> \
  --preimage <hex>

# 3. (Alternative) If Bob never claims, Alice refunds after expiry.
kvnc htlc refund --id <swap_id>
```

Notes:

- `create` moves the amount into a per-record escrow address
  (`hash(b"htlc/escrow" || id)`); `claim` pays the designated claimer only.
- `hash-lock` is `kvnc_common::hash(preimage)` (BLAKE3-256).
- `refund` may be triggered by anyone once `expiry` has passed.

## Time-lock vault (KVP-105)

```bash
# Create: beneficiary can claim after an absolute unlock time.
kvnc vault create \
  --beneficiary <address> \
  --amount 50000000000 \
  --unlock-at <unix-seconds>

# Beneficiary claims the vested amount (repeat as vesting progresses).
kvnc vault claim --id <vault_id>

# Creator cancels and reclaims everything not yet claimed.
kvnc vault cancel --id <vault_id>
```

`--unlock-at` selects the `Absolute` schedule; a `--start/--end/--cliff`
form selects `Linear` vesting (see `VestingSchedule` in
[`docs/CONTRACTS.md`](CONTRACTS.md) §3).

## Multisig (KVP-101)

```bash
# Create a 2-of-3 wallet.
kvnc multisig create \
  --owners <addr1,addr2,addr3> \
  --threshold 2

# Any owner proposes (proposer auto-confirms) → returns tx_id.
kvnc multisig propose \
  --id <multisig_id> \
  --to <address> \
  --amount 1000000000

# Another owner confirms.
kvnc multisig confirm --id <multisig_id> --tx-id <tx_id>

# Anyone executes once the threshold is met; funds leave the multisig
# contract's own account.
kvnc multisig execute --id <multisig_id> --tx-id <tx_id>
```

## Token (KVP-102)

```bash
# Deploy a token; caller becomes owner + minter and receives the supply.
kvnc token create --name "My Token" --symbol MTK --decimals 9 \
  --initial-supply 1000000000000000

# Standard ERC-20-style flows.
kvnc token transfer --to <address> --amount 5000000000
kvnc token approve --spender <address> --amount 5000000000
kvnc token mint --to <address> --amount 1000000000   # minter only
kvnc token burn --amount 1000000000
```

---

## Indexer hooks

Every flow above emits events with the canonical topics defined in
`kvnc_common::events` (see [`docs/CONTRACTS.md`](CONTRACTS.md) §9). Match on
the topic bytes and decode the data payload — e.g. `htlc_created` carries the
32-byte swap id, `multisig_proposed` carries the 8-byte tx id (little-endian),
and the token topics carry empty data.
