# Example: HTLC Atomic Swap Flow

Pseudo-flow (replace with real CLI / RPC calls once wired).

```bash
# 1. Alice locks 100 KVNC for Bob, hash = sha256(secret), expiry in 1 hour
kvnc htlc create \
  --claimer <Bob> \
  --amount 100000000000 \
  --hash-lock <hex> \
  --expiry <unix>

# → returns swap_id

# 2. Bob claims with the secret before expiry
kvnc htlc claim \
  --id <swap_id> \
  --preimage <secret-hex>

# 3. (Alternative) If Bob never claims, Alice refunds after expiry
kvnc htlc refund --id <swap_id>
```

Same pattern works for Vault (create → wait → claim) and Multisig (create → propose → confirm → execute).
