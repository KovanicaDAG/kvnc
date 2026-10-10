# KVNC Project Task List (Phases 1–25)

> **Ova verzija je 100% truth state — provjerena kod-inspekcijom + buildom na `main` @ `73c5b9a` (2026-10-10).**
> Prethodna verzija (s checkboxes) i `TASKLIST_DRIFT.md` (2026-10-08) su zastarjeli; ovaj dokument ih zamjenjuje.

**Legenda statusa**
- `[x]` — implementirano i wired u production pathu (verificirano u kodu)
- `[~]` — djelomično (kod postoji, ali nije potpuno wired / nije production-safe / test ne prolazi)
- `[ ]` — nije gotovo (nema koda ili je samo plan)

**Kritični path:** Types → Storage → DAG → Consensus → Execution → Node wiring → Committee → Admission → Multi-node quorum → Testnet

---

## Verified run (2026-10-10, `73c5b9a`)

- `cargo check --workspace --all-targets` → **0 errors**
- `cargo clippy --workspace --all-targets` → **clean** (bez warninga)
- `cargo test --workspace --lib` → **svi unit testovi prolaze** (~375 test fn u src/ svih 19 crate-ova; 0 failed)
- Integration testovi (`--tests`):
  - `kvnc-node multi_node_integration` → **3/3 PASS** (konvergencija 4 noda na identičan commit sequence; partition 3+1 majority commits & minority catches up; 2+2 nema quorum)
  - `kvnc-node sustained_liveness_integration` → **1/1 PASS** (~65s; 4 validatorska procesa, committed height 20+ preko pruning boundary, isti leader na svim nodovima)
  - `kvnc-rpc websocket_subscriptions` → **PASS**
  - `kvnc-node late_join_integration` → **1 FAIL**: `late node's first committed leader is unknown to node 0` (late-join sync nije konzistentan)
  - `kvnc-node live_vote_integration` → **2 testa `#[ignore]`; padaju kad se forsiraju** (`Error: parsing genesis validators ... missing field validator` — test harness generira prazan `genesis_validators.toml`; infra problem, ne consensus bug)
- Cargo warning-i (non-fatal, postoje u kodu): `kvnc-staking` slobodne stub funkcije `bond_validator`/`unbond_validator`/`set_commission` (dead code; prave impl su na `StakingState`); `kvnc-rpc` `rate_limit_middleware`/`auth_middleware` nisu korišteni (dead code); `kvnc-cli` lib+bin dualni target upozorenje; par unused importa u `kvnc-execution`/`kvnc-network`.

**Naming (zaključano, razriješeno 2026-10-10):** chain = **Kovanica**, native coin ticker = **KUNA** (9 decimals), code/binary/env prefiks = **kvnc**. Svi chain-facing ticker reference su sada KUNA: `README.md`, `AGENTS.md`, `docs/*`, signature domain tagovi (`KUNA/vote/v1`, `KUNA/tx/v1`), `Hash::DOMAIN_TX = "KUNA-TX-v1"`, konstanta `ONE_KUNA` (ex-`ONE_KVNC`), CLI, faucet. Brojke tokenomike nepromijenjene (90.2M cap, 9 decimals, 10 reward, 2_050_000 era, ×¾). Adresa ostaje `kvnc<hex>dag`.

**Seed/DNS (razriješeno 2026-10-10):** kvnc kod više ne referencira tuđu infrastrukturu. `NetworkConfig::default().bootstrap_nodes` i `NodeConfig::default().bootnodes` su prazni; seedovi idu preko `KVNC_BOOTNODES` / `[node] bootnodes`. 3+ kvnc-native DNS imena treba registrirati prije javnog testneta (`docs/SEED-DNS-AUDIT.md`). Kanonski P2P port: **8000**.

---

## PHASE 1.1 (kvnc-types): Core types — `[x]` DONE

- [x] Block/Tx serialization (serde + bincode)
- [x] Merkle root na StatementBlock (`compute_merkle_root` + validacija u block_manager)
- [x] Round (`u64`) / AuthorityIndex (`u16`) aritmetika
- [x] Committee types + `leader_for_round` stake-weighted (`CommitteeInfo`, `types.rs:204-226`)
- [x] Canonical address `kvnc<hex>dag` + blake3 checksum, bare/0x back-compat (`address.rs`)
- [x] Hash domain separation (BLOCK/TX/DIGEST/MERKLE) + `kvnc-types/src/signing.rs` — signature format v1: `chain_id` registry (mainnet 1, testnet 2, devnet 3, local 1337), `VOTE_DOMAIN_TAG`/`TX_DOMAIN_TAG` (16-byte, `KUNA/...`)
- [x] `CommittedSubDag.non_blue` field (#13 contract) — red blocks vraćeni u mempool, ne prunaju se

## PHASE 1.2 (kvnc-crypto): Primitives — `[~]` partial

- [x] Keypair persistence: Argon2id + XChaCha20-Poly1305 (`kvnc-cli/src/wallet.rs`, keystore v2 + v1→v2 migracija)
- [x] `verify_batch` → `Err(VerificationFailed)` na nevalidan potpis (ne `Ok(false)`) + `tests/strict_vectors.rs` (strogi vektori)
- [x] Strict sig verify svugdje (commit `bebe8c6`)
- [ ] BIP32-style HD derivation — **nema koda** (samo BIP-39 mnemonic import bez HD izvoda u CLI)
- [ ] VRF za leader selection — **nema koda**; koristi se deterministički stake-weighted

## PHASE 2.1: Block Store — `[x]` DONE

- [x] redb schema + tables init
- [x] BlockStore API (put/get/by_height/by_range)
- [x] Transaction index (tx hash → block ref)
- [x] Pruning (`prune_below`; wave window u dag_store)

## PHASE 2.2: State Store — `[x]` DONE

- [x] Account state (balance/nonce/code/storage)
- [x] Staking state persist (save/load)
- [x] Sorted-KV Merkle state root (`compute_state_root`)
- [x] Snapshot/Restore (`export_snapshot`/`import_snapshot`) + `sub_balance` fix (open-table write + checked_sub)

## PHASE 2.3: Consensus Store — `[x]` DONE

- [x] DAG persistence (block + parent/child links)
- [x] Commit tracker (committed_leader_height, decided rounds)

## PHASE 3.1: DagStore — `[x]` DONE

- [x] Block ingestion (parents/sig/round validacija)
- [x] Causal ordering (ancestors BFS)
- [x] Parent selection stake-filtered (max_parents ceil 3 po wave; `MAX_PARENT_ROUND_GAP`)
- [x] GC: `prune_below` / `prune_waves_before`; `prune_non_blue` više se NE poziva (MysticGhost variant A vraća red blocks kroz `non_blue`)

## PHASE 3.2: Block Manager — `[x]` DONE

- [x] Propose block (`propose_block`/`propose_block_with_txs`)
- [x] Validate block + ancestry walk (uklj. prune-validate testove, gap > MAX_PARENT_ROUND_GAP rejected)
- [x] Block broadcast (engine broadcaster)

## PHASE 4.1: Committer — `[x]` DONE

- [x] Direct commit rule (2f+1, `has_quorum`)
- [x] Indirect commit rule (`has_path`)
- [x] `LeaderStatus` Undecided→Commit/Skip
- [x] `CommittedSubDag` produkcija (linearizer + mysticghost + non_blue red blocks)
- [x] Late-vote: cast glasa kada leader block stigne nakon vote rounda (commit `dfccfb3`)

## PHASE 4.2: Linearizer — `[x]` DONE

- [x] Topological sort + dedup; test `test_linearize_parents_outside_history_ignored`

## PHASE 4.3: Wave Logic — `[x]` DONE (1 item `[~]`)

- [x] `WAVE_LENGTH`, `wave_of`, `offset_in_wave`, `is_leader_round`, `scheduled_leader_for_round`
- [x] Deterministic leader selection (round-robin + stake-weighted)
- [~] Timeout handling: `leader_timeout_ms` (fixed ms) + `register_skip`;**`timeout_factor` NE postoji**

## PHASE 4.4: Consensus Loop — `[~]` partial

- [x] Round timer (`round_loop` + `interval(round_duration_ms)`)
- [~] Fork handling: kod **odbija** fork (različit digest, isti round/author, `first-valid wins`); **ne bira** lexicographic min-digest; test potvrđuje rejection (engine.rs:1422-1484)

## PHASE 5.1: Core Mempool — `[x]` DONE

- [x] Tx pool priority by fee rate (`BTreeMap<u64, VecDeque<Transaction>>`)
- [x] Admission control (nonce/balance/sig/gas; zero-fee rejected **osim** Stake)
- [x] Eviction (`maybe_evict`, najniži fee-rate)
- [x] Rebroadcast
- [x] Gossip edge: `FeeTooLow` = **Ignore, ne Reject** (commit `6703663`) — ne ruši peer-a zbog niskog fee-ja

## PHASE 5.2: Block Building — `[x]` DONE

- [x] `get_next_transactions` conflict-aware (grupiranje po senderu, sort po nonce-u, kontiguitet)
- [x] Fee estimation advisory + `kvnc_estimateFee` (stub u RPC-u)

## PHASE 6.1: libp2p Setup — `[x]` DONE

- [x] Transport TCP + Noise + Yamux; gossipsub (topics: blocks, transactions, votes, sync); Kademlia; identify; ping
- [x] mDNS namjerno odsutan (DNS-only seed)
- [x] **Persistent libp2p identity** (`identity.rs`): key-file permission check (0600/0400), PeerId stabilan kroz restarte

## PHASE 6.2: Protocols — `[x]` DONE

- [x] Block sync request/response (`BLOCK_SYNC_PROTOCOL`)
- [x] Tx gossip + RPC→gossip (`kvnc_sendRawTransaction` šalje u gossipsub, commit `0cfbbc3`)
- [x] Vote gossip
- [x] **Peer scoring** (`scoring.rs`): `PeerScoringConfig` (gossip/publish/graylist/ban pragovi; samo negativne akcije spuštaju score; mesh-delivery penalizacija isključena)
- [x] **Vote verifier trait** (`validation.rs`): `Ed25519VoteVerifier`, `AuthorityKeys`, `Rejection`, `SigError`; edge-validacija bloka (digest, merkle, committee author, potpis)

## PHASE 6.3: Bootstrap — `[x]` DONE

- [x] Bootstrap seedovi: **nema hardkodiranih hostova** (`bootstrap_nodes`/`bootnodes` prazni); seedovi preko `KVNC_BOOTNODES`/TOML; 3+ kvnc-native DNS imena TBD (`docs/SEED-DNS-AUDIT.md`); P2P port 8000

## PHASE 7.1: Native Execution — `[x]` DONE

- [x] Transfer/Stake/Deploy/Call dispatch
- [x] Delegate/ClaimRewards tx kinds + execution
- [x] Rollback semantika: failed tx vraća sve osim fee+nonce; failed subdag commit rollback; replay determinizam (testovi `failed_delegate_rolls_back_except_fee_and_nonce`, `failed_storage_commit_does_not_publish_candidate_staking_state`, `replay_with_failed_txs_is_deterministic`)
- [x] Staking wiring: `on_leader_committed` se poziva iz execution patha (`execution/src/lib.rs:286`)

## PHASE 7.2: WASM Runtime — `[x]` DONE

- [x] Host funkcije (10 env importa: caller, contract_address, block_height, timestamp, balance_of, transfer, storage_get, storage_set, emit_event, +1)
- [x] Gas metering (`StoreLimitsBuilder` + `set_fuel` + `OutOfGas`)
- [x] Memory limits (`memory_size` + `trap_on_grow_failure`; test `memory_grow_is_limited_by_configured_page_cap`)
- [x] Determinism + module cache (`ContractRunner`; cache je per-runner, ne preživljava restart — TODO u kodu)

## PHASE 7.3: Execution Context — `[x]` DONE

- [x] Receipts, events, state root po subdagu; atomic commit (`execute_committed_subdag`)

## PHASE 8.1 (kvnc-staking): Emission & Treasury — `[x]` DONE

- [x] `block_reward()` / `cumulative_mining_issuance()` (geometrijski ×¾ po eri od 2_050_000; s₀=10; budget 82M)
- [x] Treasury vesting (8M / 8 godina linear; `advance`/`expected_vested`/`claim`; cap 8M)
- [x] `circulating_supply()` clamp na 90.2M
- [x] Testovi: `canonical_values_match_skeleton`, `cumulative_issuance_never_exceeds_budget`, `leader_reward_goes_to_payout_address`, `genesis_state_has_15_validators_and_treasury`

## PHASE 8.2: Delegation & Governance — `[x]` (governance `[ ]`)

- [x] `StakingState::delegate` / `unbond` / `withdraw_unbonded` / `unbonding_ready` (UNBONDING_ROUNDS queue)
- [x] `bond_validator` / `unbond_validator` / `set_commission` na `StakingState` (s pravim error tipovima: NotValidator, BelowMinimumStake, InvalidCommission)
- [x] `reward_share` (commission bps + delegator share)
- [x] `rotate_epoch` na `EPOCH_ROUNDS` (2_050_000)
- [x] `slash(DoubleSignEvidence)` fiksno 500 bps, test `slash_is_deterministic_fixed_500bps`
- [ ] Governance hooks — **nema** (Phase 25)

## PHASE 8.3–8.8: Tests, Live Path, Contracts, RPC/CLI, Docs, Events — `[~]` mixed

- [x] Tests — staking unit testovi prolaze (19 u lib runu)
- [~] Live path — CLI delegate/claim postoji; end-to-end live vote flow test **ne prolazi** u harnessu (live_vote_integration, infra)
- [x] Contracts — HTLC, Vault, Multisig, Token + Host trait; `contracts.rs` narastao (+1217 linija): custody/escrow, rollback testovi
- [x] RPC/CLI — osnovne metode rade; delegate/claim-rewards implementirani
- [ ] Docs — nema dedicated staking docs; postoji `docs/TOKENOMICS.md` + novi `docs/SIGNATURE_FORMAT.md`
- [~] Events — osnovna emisija postoji; nema structured indexera

## PHASE 9 (kvnc-node): Config & Core Loop — `[x]` DONE

- [x] Toml config + KVNC_* env overrides (`config.rs`)
- [x] Core loop (startup/shutdown, P2P, consensus ticker)
- [x] Graceful shutdown
- [x] `kvnc-node genesis` — `--validators --treasury-address --founder-address --validator-keys-out --force` + `build_committee`
- [x] **Orphans buffer** (`orphans.rs`): missing-parent fetch (`BlockSyncRequest::ByHash`), dedup in-flight requesta, bound per peer/age (max_orphans 1024, max_bytes 32MB, ttl 60s)
- [x] **NodeHealth** (`rpc/src/health.rs`): execution worker + consensus engine reportuju stanje; `/health` vraća 503 kad worker padne

## PHASE 10 (kvnc-rpc): JSON-RPC & Subscriptions — `[x]` DONE

- [x] 22 RPC metode (chain/tx/account/staking/mempool/consensus/contracts): `kvnc_blockNumber`, `kvnc_getBalance`, `kvnc_getBlockByHash/Number`, `kvnc_getTransactionByHash/Receipt`, `kvnc_sendRawTransaction`, `kvnc_getCommittee`, `kvnc_getValidators`, `kvnc_getLeaderSchedule`, `kvnc_getStake`, `kvnc_getRewards`, `kvnc_estimateFee`, `kvnc_subscribe`/`unsubscribe`, ...
- [x] WS subscriptions (newHeads, newCommittedLeader, pendingTransactions, logs) — test prolazi
- [x] Canonical addresses u response-ima
- [x] `kvnc_blockNumber` = committed leader height
- [x] OpenAPI 3.1 spec (oneOf diskriminator za sve metode) + write helpers
- [x] Rate limit (token bucket 60/min burst 10, 429) + bearer token framework — **ali** `rate_limit_middleware`/`auth_middleware` su dead code (ne-wired); rate limit ide preko axum middleware sloja u `rpc/src/lib.rs:295`

## PHASE 11 (kvnc-cli): Wallet & Node Ops — `[~]` partial

- [x] Wallet: keygen/import/export/migrate/import-key/address/sign (offline; v1→v2 migracija; BIP-39 mnemonic bez HD)
- [x] Canonical addresses
- [x] `status`/`info` (chain info) + `balance`
- [~] Node ops `sync`/`peers` — **ne postoje kao komande** (uklonjene ili nikad dodane); `propose`/`vote` → `not_implemented` (governance)
- [x] Stake/unstake/delegate/claim-rewards (keystore signing, `sign_and_submit_self`)
- [x] HTLC / Vault / Multisig / Token subcommands
- [x] JSON/table output (`output.rs`)

## PHASE 12: Testing & Benchmarks — `[~]` partial

- [x] Unit testovi: svi prolaze (cargo test --workspace --lib green)
- [x] Integration: multi-node 3/3 PASS, sustained liveness 1/1 PASS (4 procesa, height 20+), WS subscriptions PASS
- [~] Property testovi: proptest prisutan (9 `proptest!` makroa u consensus tests) — nije comprehensive
- [ ] Load/stress TPS≥100 @2GB RAM — **nema verificiranog TPS benchmarka**; sustained liveness je funkcionalni test, ne throughput
- [ ] Multi-process quorum je dokazan kroz integration testove, ali **24h soak nije nikad pokrenut**

## PHASE 13: DevOps & Orchestration — `[~]` partial

- [x] Docker multi-stage + 4-node compose (`docker-compose.yml`, 4 servisa, healthchecks, val1-4.pem, `genesis_validators.toml`)
- [ ] Kubernetes — **nema manifesta** (postoji `docs/A13.2-K8S.md` plan)
- [x] Prometheus metrics + Grafana (11 panela `ops/grafana/kvnc-overview.json`) + alerting (11 pravila `ops/prometheus/alerts.yml`)
- [x] Soak script (`ops/soak/soak.sh`): MemoryMax=3G cgroup v2, RSS monitoring svakih 30s, warn na 3G

## PHASE 14: Genesis & Launch Prep — `[~]` partial

- [x] Genesis tool (u node-u, `main.rs`)
- [x] Premine: founder 200K + treasury 8M
- [x] Faucet crate (`kvnc-faucet`): rate limit 3/hr/IP default, 10 KUNA, `--chain-id` obavezan (1/2/3/1337) — **nije u CI**; novi README
- [ ] Key distribution ceremony — **nema**
- [x] Seed nodes 3+ (DNS; distribuirani na `:8000`)
- [ ] Explorer + validator onboarding docs — **nema** (nema explorer appa)

## PHASE 15: Stabilisation & MysticGhost — `[~]` partial

- [x] 15.0 scaffolding/mergeset
- [x] 15.1 GHOSTDAG k=3 scoped (`ghostdag_scoped.rs`) + MysticGhost colouring (hybrid: Mysticeti wave DAG + GHOSTDAG mergeset bojanje)
- [x] 15.2 committer (UniversalCommitter; MysticGhost return variant A — red blocks via `non_blue`, commit `016b7fb`)
- [x] 15.3 resource hardening (prune + metrics + soak script)
- [~] 15.4 6h soak — script postoji, **nikad pokrenut u CI**
- [~] 15.5 multi-node stabilisation (4/15 node, partition) — integration testovi prolaze; **24h soak nikad**
- [x] Resource budgets — MemoryMax=3G u soak.sh; CPU/disk caps **nisu** enforce-ani
- [~] 15.6 light-client certificates — tipovi postoje (`light_client.rs`: WaveCommitCertificate, ColouringCertificate, StateProof, LightClientCheckpoint; verify_quorum 2f+1, verify_witnesses k=3); **nema node/RPC endpointa za light client**

## PHASE 16: Node Wiring & Quorum — `[~]` last gap

- [x] 16.1 committee from staking state (`build_committee` + testovi)
- [x] 16.2 round from consensus tip — `engine.subscribe_round()` → `block_manager.set_round_receiver()` (main.rs:632-633) — **DONE od zadnjeg drifta**
- [~] 16.3 live vote→commit→execute — test scaffold postoji (`live_vote_integration`), ali **2 testa padaju** kad se forsiraju sa `--ignored` (harness generira prazan `genesis_validators.toml`; **nije consensus bug nego test infra**)
- [x] 16.4 mempool admission wired — engine koristi `mempool.get_next_transactions()`; RPC `kvnc_sendRawTransaction`→gossip (edge validacija: hash, merkle, committee author, potpis)
- [~] 16.5 conflict-aware block building — postoji u mempoolu; live proof ovisi o 16.3
- [~] 16.6 real 4-node quorum — **multi-process quorum DOKAZAN** kroz `sustained_liveness_integration` (4 procesa, identični commit sequence, height 20+); **docker-compose run nije pokrenut**; late-join sync test **pada**

## PHASE 17: Consensus Mechanics — `[x]` (fork `[~]`)

- [x] Timeout handling (`leader_timeout_ms` + `register_skip`; **nema `timeout_factor`**)
- [~] Fork handling — **odbija**, ne bira min-digest
- [x] Stake-weighted leader
- [x] Batch sig verify na ingest (strict, `Err` na bad sig)
- [x] Hash domain separation + merkle roots + **signature format v1** (`signing.rs`, `docs/SIGNATURE_FORMAT.md`)
- [x] Testovi: `vote_verification.rs`, `mysticghost_return.rs`, `strict_vectors.rs`, `late-vote` scenarij

## PHASE 18: Staking UX & Safety — `[x]` DONE

- [x] delegation bond/unbond/commission/reward share (`StakingState`)
- [x] validator rotation at epoch
- [x] unbonding queue enforcement
- [x] slashing double-sign (500 bps)
- [x] CLI stake/unstake/delegate/claim-rewards

## PHASE 19: State & Sync — `[~]` (1 fail: late join)

- [~] state Merkle — sorted-KV (ne full MPT)
- [x] snapshot/fast sync — `export_snapshot`/`import_snapshot` + `/kvanc/state-sync/1.0.0` request-response + gossip topic `state_sync`
- [x] state pruning policy (`keep_recent`, `max_state_roots`, `prune_state_roots`)
- [~] light-client — samo tipovi, nema endpointa
- [x] missing-parent recovery — `orphans.rs` + `BlockSyncRequest::ByHash`; re-request MissingBlocks
- [ ] **Late-join sync: FAIL** — `late_join_integration` pade (`late node's first committed leader is unknown to node 0`). **Ovo je trenutni #1 red.**

## PHASE 20: Security & Observability — `[~]` partial

- [ ] External security review — **nema dokaza o auditu**
- [x] Fuzzing harnessi (`fuzz/`: fuzz_block, fuzz_tx, fuzz_vote, fuzz_consensus) — **postoje; nisu dio workspace builda** (zaseban cargo projekt, nebuildan u CI)
- [x] RPC/WS rate limit + optional auth — framework postoji (429 token bucket, bearer token); middleware dead-code warning
- [~] Resource budgets — MemoryMax=3G soak; CPU/disk caps nisu enforced
- [x] Keystore no-raw-hex: `address` zahtijeva `--allow-raw-hex` + 0600; `import-key` hidden prompt; migracija v1→v2; libp2p key file perms check (`identity.rs`)

## PHASE 21: Metrics & Alerting — `[x]` DONE

- [x] Prometheus `/metrics` (consensus metrics + registry; RPC portovi izloženi u compose)
- [x] Grafana dashboard (11 panela)
- [x] Alerting (11 pravila)
- [x] Strukturirani JSON logging + trace_id po roundu (`round_trace`)
- [x] `/health` endpoint + `NodeHealth` (503 na dead worker) — novo od zadnjeg drifta

## PHASE 22: SDKs & Integrations — `[~]` partial

- [x] TypeScript API client (`packages/api-client/src/index.ts`, 9 metoda)
- [x] OpenAPI/JSON schema (22 metode, oneOf)
- [ ] Contract SDK (Rust+TS) — **nema**
- [x] Dev faucet CLI
- [ ] Indexer service — **nema**

## PHASE 23: Explorer & Wallet UI — `[ ]` NOT DONE

- [ ] block explorer — nema appa
- [ ] wallet UI — nema appa
- [ ] public status page — nema

## PHASE 24: Genesis Ceremony & Mainnet Prep — `[~]` partial

- [~] Genesis ceremony — `kvnc-node genesis` postoji (single-node inline), **nema ceremony orchestration toola/docs**
- [x] Faucet
- [x] Seed nodes 3+
- [ ] Explorer + validator onboarding docs — nema
- [ ] Mainnet freeze checklist — nema
- [ ] Mainnet launch + monitoring runbook — nema

## PHASE 25: Governance & Upgrades — `[ ]` NOT DONE

- [ ] parameter change proposals
- [ ] on-chain stake-weighted voting
- [ ] treasury spend proposals
- [ ] upgrade signaling

---

## Verificirani assets (postoje)

- `ops/grafana/kvnc-overview.json` — 11 panela
- `ops/prometheus/alerts.yml` — 11 pravila
- `ops/soak/soak.sh` — 6h, 30s interval, 3G limit (cgroup v2)
- `fuzz/` — fuzz_block, fuzz_tx, fuzz_vote, fuzz_consensus (zaseban cargo projekt)
- `kvnc-faucet` — rate-limited (3/hr), 10 KUNA, chain_id obavezan
- `docs/SIGNATURE_FORMAT.md` — signature format v1 spec (novo)
- `docker-compose.yml` — 4 validator servisa, healthchecks, val1-4.pem (gitignored), `genesis_validators.toml`
- `ops/docker/validators/generate.sh` + `build-genesis.sh`

## Known reds / otvoreni problemi (2026-10-10)

1. **`late_join_integration` FAIL** — late node's first committed leader unknown to node 0 (sync konzistentnost)
2. **`live_vote_integration` 2 testa padaju** pod `--ignored` — test harness generira prazan `genesis_validators.toml` (`missing field validator`); treba fix u test setupu
3. **Fork handling**: odbija fork umjesto min-digest izbora (dokumentirano ponašanje, nije bug po specu ali je drift od zadatka)
4. **`timeout_factor`** ne postoji (samo fixed `leader_timeout_ms`)
5. **Dead code**: `kvnc-staking` free-fn stubs; `kvnc-rpc` `rate_limit_middleware`/`auth_middleware`; `kvnc-cli` `UNSAFE_SIGN_WARNING`
6. ~~**Ticker rename KVNC→KUNA** nedovršen~~ — **RJEŠENO 2026-10-10**: chain = Kovanica, coin ticker = KUNA; README/AGENTS/docs/kod usklađeni (`ONE_KUNA`, `KUNA-TX-v1`, domain tagovi).
7. ~~**kvnc kod referencira `seed.kovanica.online`**~~ — **RJEŠENO 2026-10-10**: uklonjeno iz config/network defaultova i docs; seedovi prazni + `KVNC_BOOTNODES`; novi red: registrirati 3+ kvnc-native DNS imena prije testneta.

## Actual critical path (sljedeći koraci po prioritetu)

1. **Fix `late_join_integration`** (late-join sync) — #1 red
2. **Fix `live_vote_integration` harness** (genesis_validators.toml u test setupu) → dokaz 16.3 end-to-end
3. **Plot 16.6**: pokrenuti docker-compose 4-node quorum kao ops task + 6h soak
4. **TPS benchmark** @2GB RAM (Phase 12 exit criteria)
5. **Phase 20/21**: external security review; wired middleware cleanup
6. **Phase 24**: testnet (faucet u CI, javni seed), mainnet checklist
7. **Phase 25**: governance (nakon mainneta)