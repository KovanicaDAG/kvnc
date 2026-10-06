# kvnc Contracts

Four standard contracts available on kvnc.

## 1. HTLC (Hashed Time-Lock Contract)

Atomic-swap primitive.

| Method   | Description                          |
|----------|--------------------------------------|
| `create` | Lock funds with hash + expiry        |
| `claim`  | Reveal preimage before expiry        |
| `refund` | Return funds to sender after expiry  |

## 2. Vault (Time-lock / Vesting)

| Method   | Description                          |
|----------|--------------------------------------|
| `create` | Absolute or linear vesting schedule  |
| `claim`  | Beneficiary claims currently vested amount|
| `cancel` | Creator reclaims remaining (optional)|

Supports absolute unlock and linear vesting with optional cliff.

## 3. Multisig

M-of-N wallet.

| Method    | Description                       |
|-----------|-----------------------------------|
| `create`  | Deploy with owners + threshold   |
| `propose` | Any owner proposes a tx           |
| `confirm` | Owners add confirmations          |
| `execute` | Anyone can execute once threshold |

## 4. Token (Fungible)

Minimal ERC-20 style.

| Method          | Description                |
|-----------------|----------------------------|
| `create`        | Deploy new token           |
| `transfer`      | Direct transfer            |
| `approve`       | Set allowance              |
| `transfer_from` | Spend allowance            |
| `mint`          | Minter only                |
| `burn`          | Burn from caller           |

## Events

All contracts emit simple topic events (`htlc_created`, `vault_claimed`, `multisig_executed`, `transfer`, etc.) that indexers can watch.

## Notes

- All contracts use the shared `Host` interface.
- Storage is contract-local and deterministic.
- No dependency on any external project.
