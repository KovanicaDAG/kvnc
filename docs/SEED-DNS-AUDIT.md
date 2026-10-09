# Seed DNS / External Audit — P1 Fix (f2b05bf corrected)

Status: **P1 partially fixed** (docs/config only; live endpoint still needed for full confirmation).

## Port reconciliation (confirmed gap → resolved in docs)
- Live `/api/bootstrap` (log `@3@`, previous session): `listen 0.0.0.0:8000`; peers `seed2.kovanica.online:8000`, `seed3.kovanica.online:8000`
- Docs / env reference: `seed.kovanica.online:9000`
- **Resolution**: canonical P2P port for kvnc testnet is **8000** (live-observed). All docs / env / seed references must be updated from 9000 → 8000.
- `9000` kept only as legacy reference; no code uses it.

## Seed redundancy (3 seeds — documented)
1. `seed.kovanica.online:8000` (primary, GPS/DNS only, grey-cloud; never orange-cloud)
2. `seed2.kovanica.online:8000` (confirmed live /api/bootstrap peer)
3. `seed3.kovanica.online:8000` (confirmed live /api/bootstrap peer)
- Requirement: all 3 must resolve to origin IP (not Cloudflare proxy / CDN) for TCP 8000 plaintext.
- `KOVANICA_PEERS` default example updated: `seed.kovanica.online:8000,seed2.kovanica.online:8000,seed3.kovanica.online:8000`

## TCP / plaintext / P2P security
- Protocol: **plaintext TCP** on port 8000 (confirmed by bootstrap; no TLS/encryption at P2P layer — consistent with AGENTS.md)
- No libp2p; `libp2p_swarm` used only for peer discovery / gossip, not for TCP transport encryption
- Bootstrap seed must be DNS-only; never point peers at explorer hostnames (`explorer.kovanica.online`) for TCP 8000
- `ALLOW_RESET=0`, `FAUCET=0`, `OPERATOR=0`, `MINE=0` preserved; `KOVANICA_DATA` preserved after genesis

## External audit status
- Internal audit PASS (`AUDIT_REPORT.md`, `025ee12`); external auditor assigned (`4028067`); 2-week deadline.
- This doc update is **docs-only / config-reference**, zero consensus/staking/code change.
