# KUNA Security — Threat Model (v1, Phase 20)

> Deterministic, auditable, no `kovanica-protocol` mix.

## Scope
- Consensus (Mysticeti DAG, k=3, wave=3)
- Execution (Wasmi, deterministic WASM)
- Staking / validator set (15–21, min 50k KUNA)
- P2P (plaintext TCP:8000, DNS seed only)
- Keystore (client-side only — node never receives seed)

## Hard rules (consensus-safe / ledger-safe / client-only)
- **Client-only**: keys stay in CLI / wallet; node never sees private seed.
- **No raw hex in docs**: keystore paths documented, never literal hex.
- **Encrypted keystore**: Argon2id + XChaCha20-Poly1305 (`kvnc-cli`).
- **Rate limit**: RPC write methods (`sendRawTransaction`) require bearer token (testnet open-read).
- **Memory budget**: hard cap `MemoryMax=3G` for 6h soak; RSS monitored.

## Threats (priority-ranked)
1. Key exposure (client-side leak) → encrypted keystore + client-only path.
2. Replay / nonce reuse → strict sequential nonce in mempool + domain-separated hashes (`DOMAIN_TX`, `DOMAIN_BLOCK`).
3. Consensus fork / equivocation → deterministic tie-break (lexicographically smaller digest wins); timeout skip on missing leader.
4. Double-sign / slash → `StakingState::slash` (fixed % evidence tx).
5. P2P eclipse / Sybil → DNS-only seed (`seed.kovanica.online:8000`); no orange-cloud peers.

## Fuzz targets (future)
- `StatementBlock` deserialize (`kvnc-dag`)
- `process_block` ingest (`kvnc-consensus`)
- Mempool admission (`kvnc-mempool`)

## Keystore verification checklist
- [ ] No literal hex in `README.md`, `AGENTS.md`, docs.
- [ ] `kvnc-cli` uses `keystore` subcommand (encrypted).
- [ ] Node env never contains `KVNC_SEED` or `PRIVATE_KEY`.

## Tokenomics (locked — never edited without approval)
- Total: 90.2M KUNA (`9_020_000_000_000_000` atoms)
- Decimals: 9
- Founder premine: 200k KUNA; Treasury: 8M KUNA (linear 8yr)
- Subsidy: 10 KUNA / 2.05M blocks; decay ×3/4; reward only on committed leader.
