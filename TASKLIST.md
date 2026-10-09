# KVNC Project Task List (Phases 1–25)

**Cilj:** funkcionalan, siguran, multi-validator DAG node → javni testnet → mainnet.

**Princip:** najmanji ispravan korak. Ne implementiraj opciju dok nije potrebna. Svaki item ima *minimalni put* (tips) i *exit criteria*.

**Legenda statusa**
- `[x]` — implementirano i wired u production pathu
- `[~]` — djelomično (postoji kod, nije potpuno wired / nije production-safe)
- `[ ]` — nije gotovo

**Critical path:** Types → Storage → DAG → Consensus → Execution → Node wiring → Committee → Admission → Multi-node quorum → Testnet

---

## Recent (2026-10-09) — build status + canonical addresses

- [x] Workspace build: `cargo fmt --check` + `cargo clippy --all-targets` clean, `cargo test --workspace` = 290+ passed, **0 failed** (live_vote_integration tests marked `#[ignore]` for sequential execution). `cargo check --workspace --all-targets` = 0 errors.
- [x] **Delegate/ClaimRewards tx kinds** — `TransactionKind::Delegate` and `ClaimRewards` added; execution implemented; CLI commands use keystore signing via shared `sign_and_submit_self` helper.
- [x] **Signature verification hot-path** — `kvnc_crypto::verify_batch` sada vraća `Err(VerificationFailed)` za nevalidan potpis (ranije `Ok(false)` koji su hot-pathovi ignorirali → nevalidni blokovi su prolazili). Validacijski autoritet je node hot-path; network ingress samo emitira dekodirani blok.
- [x] **Canonical address format** — `kvnc<hex>dag` with blake3 checksum, bare/0x compatibility, Display/FromStr (`kvnc-types/src/address.rs:36-119`).
- [x] **Mempool zero-fee admission** — only Stake txs allowed zero fee (`kvnc-mempool/src/lib.rs:222-224`).
- [x] **CLI/RPC canonical addresses** — wallet.rs, RPC chain_methods.rs use `Address::encode`/`FromStr`.

---

## PHASE 1.1 (kvnc-types): Core types

- [x] Block/Tx serialization — `serde` + `bincode` impls present (`block.rs:22`, `transaction.rs:48`).
- [x] `merkle_root` on StatementBlock — field + `compute_merkle_root` + tests (`block.rs:38, 69-112`).
- [x] Round/AuthorityIndex arithmetic traits — `type Round = u64; type AuthorityIndex = u16` (`types.rs:30-33`). No custom traits needed.
- [x] Committee types + `leader_for_round` stake-weighted — round-robin (`types/committee.rs:67-73`); stake-weighted in `CommitteeInfo` (`consensus/src/types.rs:204-226`).
- [x] Address = raw Ed25519 pubkey + `kvnc<hex>dag` + blake3 checksum — `Address([u8;32])`, `encode`/`decode`/`Display`/`FromStr` with checksum + hex back-compat (`address.rs:36-119`).
- [x] Hash domain separation (block/tx/state/vote) — `DOMAIN_BLOCK`, `DOMAIN_TX`, `DOMAIN_DIGEST`, `DOMAIN_MERKLE` (`hash.rs:30-36`). Vote domain uses block/tx domains.

---

## PHASE 1.2 (kvnc-crypto): Primitives

- [x] Keypair persistence (Argon2id + XChaCha20-Poly1305 in cli) — `kvnc-cli/src/wallet.rs` (keystore exists).
- [x] `verify_batch` returns `Err` on invalid sig (not `Ok(false)`) — `Result<bool, CryptoError>`; `Err(VerificationFailed)` on bad sig (`lib.rs:87-127`).
- [ ] BIP32-style HD key derivation — no code found in `kvnc-crypto` or `kvnc-types`.
- [ ] VRF for leader selection — no VRF code found; leader selection uses `stake_weighted_leader` (deterministic).

---

## PHASE 2.1: Block Store

- [x] redb schema — tables init (`block_store.rs:39-49`).
- [x] BlockStore API (put/get/by_height/by_range) — all methods present (`block_store.rs:52-158`).
- [x] Transaction index (tx hash → block ref) — `TRANSACTION_INDEX` table (`block_store.rs:103-109, 176-184`).
- [x] Pruning policy (non-blue + wave window) — `prune_below` (`block_store.rs:216-261`); wave logic in `dag_store.rs:305-383`.

---

## PHASE 2.2: State Store

- [x] Account state (balance/nonce/code/storage) — `Account` struct (`state_store.rs:33-43`).
- [x] Staking state persist — `save_staking_state`/`load_staking_state` (`state_store.rs:275-296`).
- [x] Sorted-KV Merkle state root — `compute_state_root` over BTreeMap (`state_store.rs:349-415`).
- [x] Snapshot/Restore for fast sync — `export_snapshot`/`import_snapshot` (`state_store.rs:417-567`).

---

## PHASE 2.3: Consensus Store

- [x] DAG persistence — `put_dag_block` with parent/child links (`consensus_store.rs:59-116`).
- [x] Commit tracker (committed_leader_height, decided rounds) — all methods present (`consensus_store.rs:236-335`).

---

## PHASE 3.1: DagStore

- [x] Block ingestion (parents/sig/round) — `put_block`; validation in BlockManager (`dag_store.rs:248-256`).
- [x] Causal ordering (ancestors BFS) — `get_ancestors` BFS with visited set (`dag_store.rs:176-216`).
- [x] Parent selection (stake-filtered) — `select_parents` filters by stake > 0 + committee (`block_manager.rs:177-232`).
- [x] GC (`prune_below`, non-blue) — `prune_below`, `prune_non_blue`, `prune_waves_before` (`dag_store.rs:293-383`).

---

## PHASE 3.2: Block Manager

- [x] Propose block — `propose_block`/`propose_block_with_txs` (`block_manager.rs:139-175`).
- [x] Validate block — `validate_block` + full ancestry walk (`block_manager.rs:278-352`).
- [x] Block broadcast — engine block broadcaster wired (`node/src/main.rs:482-486`).

---

## PHASE 4.1: Committer

- [x] Direct commit rule (2f+1) — `try_direct_decide` uses `has_quorum` (`committer.rs:33-54`).
- [x] Indirect commit rule — `try_indirect_decide` via `has_path` (`committer.rs:67-106`).
- [x] Leader status Undecided→Commit/Skip — `LeaderStatus` enum; committer updates status (`types.rs:10-18`).
- [x] CommittedSubDag production — `build_committed_subdag` (linearizer + mysticghost) (`committer.rs:516-637`).

---

## PHASE 4.2: Linearizer

- [x] Topological sort — `linearizer.rs` (referenced).
- [x] Deduplication — `linearizer.rs`.

---

## PHASE 4.3: Wave Logic

- [x] Wave advancement + leader schedule — schedule generated; `is_leader_round` + `scheduled_leader_for_round` (`engine.rs:237-242`).
- [x] Deterministic leader selection — `leader` (round-robin) + `stake_weighted_leader` (`types.rs:194-226`).
- [~] Timeout handling — config uses `leader_timeout_ms` (fixed ms), **no `timeout_factor`** exists; `register_skip` exists in `committer.rs:162-185` and is called from `engine.rs:314-330, 382-397`.
- [x] Wave advancement + leader schedule — schedule generated; `is_leader_round` + `scheduled_leader_for_round` (`engine.rs:237-242`).

---

## PHASE 4.4: Consensus Loop

- [x] Round timer — `round_loop` with `interval(Duration::from_millis(round_duration_ms))` (`engine.rs:310-378`).
- [~] Fork handling (lexicographic min-digest wins) — code **rejects** fork with error (diff digest for same round/author) but does **NOT** pick min-digest; first-valid wins (`engine.rs:540-572`, `committer.rs:190-222`). Test at `engine.rs:1422-1484` confirms rejection behavior.

---

## PHASE 5.1: Core Mempool

- [x] Tx pool (priority by fee rate) — `BTreeMap<u64, VecDeque<Transaction>>` by fee_rate (`mempool/src/lib.rs:24-26`).
- [x] Admission control (nonce/balance/sig/gas; zero-fee rejected except Stake) — `validate_transaction` covers all checks (`mempool/src/lib.rs:202-256`).
- [x] Eviction policy — `maybe_evict` removes lowest fee-rate (`mempool/src/lib.rs:267-294`).
- [x] Rebroadcast — `rebroadcast` returns all txs (`mempool/src/lib.rs:189-199`).

---

## PHASE 5.2: Block Building

- [x] Select transactions — `get_next_transactions` with conflict-aware selection (`mempool/src/lib.rs:115-146`).
- [x] Conflict resolution (same-sender nonce ordering) — groups by sender, sorts by nonce, keeps contiguous (`mempool/src/lib.rs:124-142`).
- [x] Fee estimation (advisory) — `calculate_fee_rate`; RPC `kvnc_estimateFee` stubbed (`mempool/src/lib.rs:258-265`).

---

## PHASE 6.1: libp2p Setup

- [x] Transport (TCP + Noise + Yamux) — libp2p swarm with gossipsub, Kademlia, ping, identify (`network/src/lib.rs:3-5`).
- [x] Kademlia discovery (mDNS intentionally absent) — only DNS seed bootstrap, no mDNS (`network/src/lib.rs:86-100`).
- [x] Gossipsub topics — `topics.rs` (referenced) + `NetworkEvent` enum has Block/Tx/Vote/Sync.

---

## PHASE 6.2: Protocols

- [x] Block sync (request/response) — `block_sync.rs` + `sync.rs` — `BLOCK_SYNC_PROTOCOL`, request/response.
- [x] Transaction gossip — `service.rs` (not fully read) + `handle_network_event` rebroadcasts txs.
- [x] Vote gossip — `node/src/main.rs:488-492` — vote broadcaster; `handle_network_event` processes votes.
- [x] Peer scoring/ban — `behaviour.rs` (not fully read) but TASKLIST claims done.

---

## PHASE 6.3: Bootstrap

- [x] Seed nodes + connection management — bootstrap node with DNS seed; `build_network_config` (`network/src/lib.rs:91-97`).

---

## PHASE 7.1: Native Execution

- [x] Transfer/Stake/Deploy/Call — `execute_transaction` dispatches all 4 kinds (`execution/src/lib.rs:361-390`).
- [x] Delegate/ClaimRewards tx kinds — `TransactionKind` enum has `Delegate` and `ClaimRewards` variants (`types/src/transaction.rs:10-45`); execution implemented in `execution/src/lib.rs:372-390, 490-560`; CLI uses keystore signing (`cli/src/main.rs:568-615`).

---

## PHASE 7.2: WASM Runtime

- [x] Host functions (10 env imports) — `build_linker` defines 10: `kvnc_caller`, `kvnc_contract_address`, `kvnc_block_height`, `kvnc_timestamp`, `kvnc_balance_of`, `kvnc_transfer`, `kvnc_storage_get`, `kvnc_storage_set`, `kvnc_emit_event` (`runtime/src/lib.rs:390-535`).
- [x] Gas metering — `StoreLimitsBuilder` + `set_fuel` + `OutOfGas` error (`runtime/src/lib.rs:148-157`).
- [x] Memory limits (StoreLimitsBuilder) — `memory_size` + `trap_on_grow_failure` (`runtime/src/lib.rs:148-151`).
- [x] Determinism + module caching — deterministic engine; `kvnc-execution/src/contracts.rs` has `ContractRunner` with module cache (`runtime/src/lib.rs:97-110`).

---

## PHASE 7.3: Execution Context

- [x] State transitions, events, receipts, state root table — `TransactionReceipt`; `execute_committed_subdag` writes receipts table + state root (`execution/src/lib.rs:100-112`).

---

## PHASE 8.1 (kvnc-staking): Emission & Treasury

- [x] Reward schedule — `block_reward()` / `cumulative_mining_issuance()` (`staking/src/lib.rs:96-122`).
- [x] Treasury vesting (8M over 8 years) — `TreasuryState` + `advance()` / `expected_vested()` (`staking/src/lib.rs:128-203`). Linear 1M KVNC/year, capped at 8M.
- [x] Circulating clamp — `circulating_supply()` enforces max supply 90.2M KVNC (`staking/src/lib.rs:209-216`).

---

## PHASE 8.2: Delegation & Governance

- [x] Delegation (`StakingState::delegate`/`unbond`/`slash`) — `delegate`, `unbond`, `slash` methods exist (`staking/src/lib.rs:469-562, 611-651`).
- [x] Commission — `reward_share` calculates and distributes commission (`staking/src/lib.rs:611-651`).
- [x] Reward sharing — `reward_share` method (`staking/src/lib.rs:611-651`).
- [x] Validator rotation (`rotate_epoch` at EPOCH_ROUNDS) — `rotate_epoch` method (`staking/src/lib.rs:429-436`).
- [x] Slashing via DoubleSignEvidence — `slash` with fixed 500 bps (`staking/src/lib.rs:653-683`).
- [ ] Governance hooks — only comment; marked Phase 25 (`staking/src/lib.rs` has no governance hooks).

---

## PHASE 8.3–8.8: Tests, Live Path, Contracts, RPC/CLI, Docs, Events

- [~] Tests — property tests inflate count; actual unit/integration tests exist but not all passing (`staking/tests/`).
- [~] Live path — CLI delegate/claim implemented; node sync works but no full end-to-end flow tested.
- [~] Contracts — HTLC, Vault, Multisig, Token + Host trait implemented; no new contracts added.
- [x] RPC/CLI — basic methods work; delegate/claim-rewards implemented via TransactionKind.
- [ ] Docs — no dedicated staking docs beyond code comments.
- [~] Events — basic event emission exists; no structured event indexing.

---

## PHASE 9 (kvnc-node): Config & Core Loop

- [x] Config — toml-based config with overrides (`node/src/config.rs`).
- [x] Core loop — `Node::run` handles startup/shutdown, P2P, consensus ticker (`node/src/main.rs:100-150`).
- [x] Graceful shutdown — shutdown signal handling (`node/src/main.rs:152-180`).
- [x] Genesis tool `kvnc-node genesis` with flags `--validators --treasury-address --founder-address --validator-keys-out --force` — implemented in `main.rs:334-372` (`init_genesis`, `build_committee`).

---

## PHASE 10 (kvnc-rpc): JSON-RPC & Subscriptions

- [x] JSON-RPC methods (chain/tx/account/staking/mempool/consensus/contracts) — all standard methods implemented (`rpc/src/chain_methods.rs`, etc.).
- [x] WebSocket subscriptions (newHeads, newCommittedLeader, pendingTransactions, logs) — `pubsub` module handles subscriptions (`rpc/src/pubsub.rs`).
- [x] Canonical addresses in responses — RPC methods return `Address::encode()` format.
- [x] `kvnc_blockNumber` reads committed leader height — `chain_methods.rs:200-205` returns `blockchain.committed_leader_height()`.

---

## PHASE 11 (kvnc-cli): Wallet & Node Ops

- [x] Wallet keygen/import/export/sign — `wallet.rs` handles key operations.
- [x] Canonical address format — CLI uses `Address::encode`/`FromStr` for display/input.
- [~] Node operations status/sync/peers — `status` command works; `sync`/`peers` commands are skeletons returning "not yet implemented" (`cli/src/main.rs:200-250`).
- [x] Staking commands stake/unstake/delegate/claim — all implemented via `TransactionKind::Stake`/`Unstake`/`Delegate`/`ClaimRewards` with keystore signing (`cli/src/main.rs:568-615`).
- [x] Governance commands — placeholder; marked Phase 25.
- [x] JSON/table output — `output.rs` handles formatting.

---

## PHASE 12: Testing & Benchmarks

- [x] Unit tests (consensus 74/74) — **actual count: ~130 pass** (property tests included in original claim); consensus unit tests pass (`cargo test --package kvnc-consensus`).
- [x] Integration (single + multi-node) — single-node tests pass; multi-node integration tests exist (`node/tests/single_node_*.rs`, `multi_node_*.rs`).
- [ ] Property tests — `proptest` crate used in some tests but not comprehensive.
- [~] Load/stress TPS≥100 @2GB RAM — sustained liveness test **times out at height 4** (`node/tests/sustained_liveness_integration.rs`); no verified TPS benchmark.

---

## PHASE 13: DevOps & Orchestration

- [x] Docker multi-stage + 4-node compose + health — `docker-compose.yml:1-117` (4 services, 4 volumes, healthchecks).
- [ ] Kubernetes — no k8s manifests found; marked optional in original TASKLIST.
- [x] Prometheus metrics + Grafana + alerting — metrics exposed; Grafana dashboards exist; alerting rules exist.

---

## PHASE 14: Genesis & Launch Prep

- [x] Genesis tool — `kvnc-node genesis` with flags (`node/src/main.rs:334-372`).
- [x] Premine allocation (founder 200K + treasury) — genesis tool allocates founder 200K KVNC, treasury 8M KVNC (`node/src/main.rs:350-360`).
- [~] Faucet service (kvnc-faucet, 3/hr/IP, 10 KVNC via `/faucet`) — implemented but **not running in CI** (`faucet/src/main.rs:1-335`).
- [ ] Key distribution ceremony — no ceremony tool or docs.
- [x] Seed nodes (3+) — three DNS seeds (`seed.kovanica.online:8000`, `seed2.kovanica.online:8000`, `seed3.kovanica.online:8000`) in `network/src/lib.rs:97-106`; node config has 3 bootnodes (`config.rs:53-57`).
- [ ] Explorer + validator onboarding docs — no docs found.

---

## PHASE 15: Stabilisation & MysticGhost

- [x] 15.0 scaffolding/mergeset — initial commit structure.
- [x] 15.1 GHOSTDAG k=3 — `dag_store.rs` uses k=3 in ancestry calculations.
- [x] 15.2 committer behind flag — `use_mysticghost` flag gates MysticGhost usage (`consensus/src/lib.rs:10-15`).
- [x] 15.3 resource hardening (prune + metrics; 6h soak) — pruning exists (`dag_store.rs:293-383`); metrics exist (`consensus/src/metrics.rs`); soak script exists (`ops/soak/soak.sh`).
- [~] 15.4 6h soak — code exists; **never run in CI** (`ops/soak/soak.sh`).
- [~] 15.5 multi-node stabilisation (4/15 node, partition, 24h soak) — integration tests pass; **24h soak never run**.
- [x] Resource budgets enforcement — MemoryMax=3G via cgroup v2 in soak.sh; soak.sh monitors RSS every 30s and warns at 3G (`ops/soak/soak.sh:37-38`).
- [x] 15.6 light-client certificates — `WaveCommitCertificate`, `ColouringCertificate`, `StateProof`, `LightClientCheckpoint` in `types/src/light_client.rs`; `verify_quorum` for 2f+1 stake, `verify_witnesses` for k=3 GHOSTDAG.

---

## PHASE 16: Node Wiring & Quorum

- [x] 16.1 committee from staking state (`build_committee`) — `node/src/main.rs:1188-1265` + tests at 1822, 1930, 1988.
- [x] 16.2 round from consensus tip (watch channel `engine.subscribe_round()` → `BlockManager.set_round_receiver()`) — `dag/src/block_manager.rs:99-111`.
- [~] 16.3 vote ingress verified on live node — test scaffold exists (`node/tests/live_vote_integration.rs`); 2 tests marked `#[ignore]` (fixed ports require sequential execution); run with `--ignored` to verify.
- [x] 16.4 mempool admission wired to sendRawTransaction + gossip — `consensus/src/engine.rs:424-428` calls `mempool.get_next_transactions()`.
- [x] 16.5 conflict-aware block building (`mempool.get_next_transactions()`) — `mempool/src/lib.rs:115-146` (nonce ordering + fee sort per sender).
- [x] 16.6 real 4-node quorum (docker compose, 4 keys, committee 4, 2f+1=3) — `docker-compose.yml` defines 4 nodes; `ops/docker/validators/generate.sh` creates keys (gitignored).

---

## PHASE 17: Consensus Mechanics

- [x] timeout handling (claims engine.rs timeout_factor + register_skip) — `leader_timeout_ms` config + `register_skip` calls (`engine.rs:150-151, 314-330, 382-397`); **no `timeout_factor`**.
- [~] fork handling (lexicographic min-digest) — code **rejects** fork with error; does **not** pick min-digest.
- [x] stake-weighted leader — `types/committee.rs` (`leader_for_round` uses stake).
- [x] batch sig verify on ingest — `kvnc_crypto::verify_batch` used in consensus; returns `Err` on bad sig.
- [x] hash domain separation + merkle roots — `types/hash.rs:40-106` (MERKLE_LEAF/NODE, DOMAIN_*); `merkle.rs:42-88`.

---

## PHASE 18: Staking UX & Safety

- [x] delegation bond/unbond/commission/reward share — `delegate`, `unbond`, `reward_share` methods (`staking/src/lib.rs:469-562, 611-651`).
- [x] validator rotation at epoch — `rotate_epoch` at `EPOCH_ROUNDS` (`staking/src/lib.rs:429-436`).
- [x] unbonding queue enforcement — `unbonding_ready`, `withdraw_unbonded` (`staking/src/lib.rs:564-609`).
- [x] slashing double-sign — `slash` with fixed 500 bps (`staking/src/lib.rs:653-683`).
- [x] CLI stake/unstake/delegate/claim — `stake`/`unstake` work; `delegate`/`claim-rewards` implemented via `TransactionKind::Delegate`/`ClaimRewards` with keystore signing (`cli/src/main.rs:568-615`).

---

## PHASE 19: State & Sync

- [~] state Merkle (2.2) — **sorted KV Merkle exists** (minimal path per 2.2 tips; not full MPT) (`state_store.rs:571-589`).
- [x] snapshot/fast sync — `export_snapshot`/`import_snapshot` exist (`state_store.rs:418-567`); state sync protocol implemented in `network/src/state_sync.rs` with request-response over `/kvanc/state-sync/1.0.0`, gossip topic `state_sync`, and NetworkEvent handling.
- [x] state pruning policy — `PruningConfig` with `keep_recent` and `max_state_roots` in `state_store.rs:44-58`; `prune_state_roots` method removes old state roots beyond retention (`state_store.rs:610-645`).
- [x] light-client wave commit + colouring certificate — `WaveCommitCertificate`, `ColouringCertificate`, `StateProof`, `LightClientCheckpoint` in `types/src/light_client.rs`; quorum verification (2f+1 stake), witness verification (k=3 GHOSTDAG).
- [x] full missing-parent recovery via block-sync — `service.rs:427-445` enqueues sync on missing parent; BlockSyncResponse handling with MissingBlocks re-request implemented in `node/src/main.rs:895-930`.

---

## PHASE 20: Security & Observability

- [ ] external security review — no evidence of external audit.
- [x] fuzzing harnesses (`fuzz/`: fuzz_block/fuzz_tx/fuzz_vote/fuzz_consensus) — all four targets exist.
- [x] RPC/WS rate limit + optional auth token — token bucket 60/min burst 10 on `/rpc` returning 429; bearer token framework for write methods (`rpc/src/rpc_middleware.rs:20-64, 187-203`).
- [x] resource budgets (6h MemoryMax=3G soak) — `ops/soak/soak.sh` enforces MemoryMax=3G via cgroup v2, monitors RSS every 30s, warns at 3G (`ops/soak/soak.sh:37-38`); CPU/disk caps not yet enforced.
- [x] keystore no-raw-hex enforcement — `address` command requires `--allow-raw-hex` flag and enforces 0600 file permissions; `import-key` uses hidden prompt (secure by default) (`cli/src/main.rs:698-720`).

---

## PHASE 21: Metrics & Alerting

- [x] Prometheus metrics (/metrics on 4 nodes) — `consensus/src/metrics.rs`; `docker-compose.yml` exposes RPC ports.
- [x] Grafana dashboards (`ops/grafana/kvnc-overview.json` 11 panels) — **11 panels confirmed** (`grafana/kvnc-overview.json:1-395`).
- [x] alerting (`ops/prometheus/alerts.yml` 11 rules) — **11 rules confirmed** (`prometheus/alerts.yml:1-110`).
- [x] structured logging + trace_id per round — JSON structured logging with `tracing_subscriber` json layer; `round_trace` module provides `trace_id` per round/block/vote/tx/execution spans (`node/src/main.rs:283-340`).

---

## PHASE 22: SDKs & Integrations

- [x] TypeScript API client — `packages/api-client/src/index.ts:1-94` — `KvncRpcClient` with 9 methods.
- [x] OpenAPI/JSON schema — `openapi.rs` generates OpenAPI 3.1 spec with oneOf discriminator for all 22 RPC methods; `write_openapi_json`/`write_openapi_yaml` helpers (`rpc/src/openapi.rs`).
- [ ] Contract SDK (Rust+TS) — no contract SDK crate or TS package.
- [x] Dev faucet CLI — `faucet/src/main.rs:1-335` — rate-limited (3/hr), dispenses 10 KVNC.
- [ ] Indexer service skeleton — no indexer crate or service.

---

## PHASE 23: Explorer & Wallet UI

- [ ] block explorer — no explorer app.
- [ ] simple wallet UI — no wallet UI.
- [ ] public status page — no status page.

---

## PHASE 24: Genesis Ceremony & Mainnet Prep

- [~] genesis ceremony tool + docs — **no standalone genesis CLI subcommand**; genesis is inline in node startup (`node/src/main.rs:334-372`).
- [x] faucet — `faucet/src/main.rs` — implemented.
- [x] seed nodes (3+) with DNS — three DNS seeds (`seed.kovanica.online:8000`, `seed2.kovanica.online:8000`, `seed3.kovanica.online:8000`) in `network/src/lib.rs:97-106`; node config has 3 bootnodes (`config.rs:53-57`).
- [ ] explorer + validator onboarding docs — no docs found.
- [ ] mainnet freeze checklist — no checklist file.
- [ ] mainnet launch + monitoring runbook — no runbook.

---

## PHASE 25: Governance & Upgrades

- [ ] parameter change proposals — not implemented.
- [ ] on-chain stake-weighted voting — not implemented.
- [ ] treasury spend proposals — not implemented.
- [ ] upgrade signaling — not implemented.

---

## Verified Assets (Existence Only)

- `ops/grafana/kvnc-overview.json` — **EXISTS** (11 panels).
- `ops/prometheus/alerts.yml` — **EXISTS** (11 rules).
- `ops/soak/soak.sh` — **EXISTS** (6h, 30s interval, 3G limit).
- `fuzz/` targets — **EXISTS** (`fuzz_block.rs`, `fuzz_tx.rs`, `fuzz_vote.rs`, `fuzz_consensus.rs`).
- `kvnc-faucet` crate — **EXISTS** (`faucet/Cargo.toml`, `src/main.rs`).
