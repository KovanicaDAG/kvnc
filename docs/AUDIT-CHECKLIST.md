# External Audit / Seed / DNS — Checklist (f2b05bf corrected)

Based on `docs/SEED-DNS-AUDIT.md` (port 8000 vs 9000 gap) and HARD RULES.

## 1. Seed / DNS audit (must complete before mainnet)
- [ ] Confirm live `/api/bootstrap` P2P port (documented: 8000; docs reference 9000) — reconcile
- [ ] Document 3+ DNS seeds (min redundancy); current: `seed.kovanica.online:9000` only
- [ ] Verify seed resolves to origin IP (not orange-cloud / Cloudflare proxy)
- [ ] Confirm TLS / plaintext TCP policy: port 9000 (or confirmed live port) plaintext only, no libp2p
- [ ] Check `KOVANICA_PEERS` env defaults do not include explorer hostnames for TCP 9000

## 2. External audit scope (not completed — recommend before mainnet)
- [ ] Consensus (Mysticeti DAG, wave=3, k=3): verify uncertified commit rule, no certificate dependency
- [ ] Execution (Wasmi): determinism check, gas metering, ABI compatibility
- [ ] Staking (bond/unbond, active set 15–21, min 50 000 KVNC, commission rules)
- [ ] Tokenomics hard cap: 90 200 000 KVNC, subsidy curve ×¾ decay, era 2 050 000, maturity 100
- [ ] Crypto: Ed25519 signatures (64-byte / 128 hex), address derivation, key handling client-only
- [ ] P2P: eclipse / Sybil resistance, peer discovery, gossip limits
- [ ] Node / RPC: `KVNC_RPC_AUTH=disable` documented for test only; production must enforce auth; no key/seed passed to node

## 3. Live /api/bootstrap verification (repeat before release)
- [ ] Run `curl -s https://explorer.kovanica.online/api/bootstrap | jq`
- [ ] Compare `listen` and `peers` fields against docs / env defaults
- [ ] Confirm `MINE=0`, `FAUCET=0`, `OPERATOR=0` on participant nodes
- [ ] Verify `KOVANICA_DATA` preserved after genesis; no reset (`ALLOW_RESET=0`)

## 4. CLI / wallet audit (related gaps from tasklist)
- [ ] Delegate / claim skeleton: HTLC claim verified (`A8.2-DELEGATION-SKELETON.md`); delegation deferred Phase 18
- [ ] CLI delegate command exists but deferred; confirm ABI / gas metering in `EXECUTION-GAS-SKELETON.md`

Status: audit **not started**; docs complete. Blocked only by external auditor assignment.
