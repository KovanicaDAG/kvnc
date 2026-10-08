# KVNC Project Task List (Phases 1–25)

**Cilj:** funkcionalan, siguran, multi-validator DAG node → javni testnet → mainnet.

**Princip:** najmanji ispravan korak. Ne implementiraj opciju dok nije potrebna. Svaki item ima *minimalni put* (tips) i *exit criteria*.

**Legenda statusa**
- `[x]` — implementirano i wired u production pathu
- `[~]` — djelomično (postoji kod, nije potpuno wired / nije production-safe)
- `[ ]` — nije gotovo

**Critical path:** Types → Storage → DAG → Consensus → Execution → Node wiring → Committee → Admission → Multi-node quorum → Testnet

---

## Phase 1: Core Types & Crypto (`kvnc-types`, `kvnc-crypto`)

### 1.1 kvnc-types — Complete Data Structures
- [x] Block/Transaction serialization (serde + bincode/postcard path)
- [x] **Merkle roots** — `merkle_root` na `StatementBlock` za tx inclusion proofs
- [x] Round / AuthorityIndex arithmetic traits
- [~] Committee types — postoje; leader selection **stake-weighted completed** (17.1, types.rs)
- [x] Address derivation (blake3 pubkey → address)
- [x] **Hash domain separation** — odvojeni domain tagovi za block / tx / state / vote

**Tips**
- Merkle: binary Merkle nad `tx.hash` listom; prazan block → fixed zero root. Jedna funkcija `merkle_root(hashes: &[Hash]) -> Hash`.
- Domain separation: `blake3::Hasher::new_keyed(b"KVNC-BLOCK-v1")` (ili `hash_with_prefix(b"block", bytes)`). Nikad isti hash path za različite objekte.
- Stake-weighted leader: `leader = cumulative_stake_select(round_seed, committee)`. Zadrži round-robin kao fallback iza feature flaga dok testovi ne prođu.

**Exit:** svi hash constructori imaju domain; block ima `merkle_root`; unit testovi za prazan/1/3 tx.

### 1.2 kvnc-crypto — Production Ready
- [x] Keypair persistence (Argon2id + XChaCha20-Poly1305 keystore u `kvnc-cli`)
- [x] **Batch verification** — Ed25519 batch verify za block signaturee
- [ ] Key derivation — BIP32-style HD (optional, low priority)
- [ ] VRF za leader selection (samo ako stake-weighted + randomness zahtijeva)

**Tips**
- Batch: `ed25519_dalek::verify_batch`. Grupiraj signaturee po roundu (jedan batch po waveu). Fallback na single verify ako batch faila (dijagnostika).
- Ne stavljaj keystore u `kvnc-crypto` — ostavi u CLI/node. Crypto crate ostaje pure.

**Exit:** `verify_batch(blocks)` API + test s 64 signaturea.

---

## Phase 2: Storage Layer (`kvnc-storage`)

### 2.1 Block Store
- [x] Schema (redb tables)
- [x] BlockStore API (`put/get/by_height/by_range`)
- [x] Transaction index (tx hash → block ref)
- [x] Pruning policy (non-blue + wave window)

### 2.2 State Store
- [x] Account state (balance, nonce, code/storage)
- [x] Staking state persist
- [~] **Merkle Patricia Trie** (sorted KV Merkle — minimalni put zadovoljen) (ili jednostavniji Merkle map) za state root
- [x] **Snapshot / Restore** za fast sync

**Tips**
- Ne kreći full MPT odmah. **Minimalni put:** sorted key-value Merkle (leaf = `H(key||value)`, unutarnji = `H(left||right)`). State root = root tog stabla nakon commita.
- Snapshot: periodični `redb` copy-on-write ili export `STATE_ROOT` + flat account dump. Restore = load snapshot + replay od heighta.
- Pruning statea odvoji od DAG pruninga — drugačiji retention (npr. zadnjih N committed heights).

**Exit:** `compute_state_root()` determinističan; snapshot round-trip test.

### 2.3 Consensus Store
- [x] DAG persistence
- [x] Commit tracker (`committed_leader_height`, decided rounds)

---

## Phase 3: DAG Layer (`kvnc-dag`)

### 3.1 DagStore
- [x] Block ingestion (parents, sig, round) — wired
- [x] Causal ordering (ancestors BFS)
- [x] Parent selection (stake-filtered, committee-aware)
- [x] Garbage collection (`prune_below`, non-blue)

### 3.2 Block Manager
- [x] Propose block
- [x] Validate block
- [x] Block broadcast

**Tips**
- Sve validation mora biti na **hot pathu** (network ingest + local propose). Nikad “store blind then validate later”.
- Parent selection: prvo filtriraj valid + same-round-prev, zatim stake weight, zatim `take(max_parents)`.

**Exit:** invalid block nikad ne uđe u store; unit test za missing parent / bad sig / wrong round.

---

## Phase 4: Consensus (`kvnc-consensus`)

### 4.1 Committer (CRITICAL)
- [x] Direct commit rule (2f+1)
- [x] Indirect commit rule
- [x] Leader status (Undecided → Commit / Skip)
- [x] CommittedSubDag production

### 4.2 Linearizer
- [x] Topological sort
- [x] Deduplication

### 4.3 Wave Logic
- [x] Wave advancement + leader schedule
- [x] Leader selection (deterministic)
- [x] **Timeout handling** (engine.rs: timeout_factor + register_skip) — skip leader ako nema bloka u roku

### 4.4 Consensus Loop
- [x] Round timer
- [x] **Fork handling** (engine.rs: lexicographic min-digest wins) — više leadera isti round → first valid wins (deterministički)

**Tips**
- Timeout: wall-clock timer po roundu; ako do `round_duration_ms * factor` nema leader blocka → `LeaderStatus::Skip` i idi dalje. Jedan config: `leader_timeout_ms`.
- Fork: pri `process_block` za isti `(round, author)` zadrži **lexicographically smaller digest** (ili first-seen + persist first). Mora biti bit-identical na svim nodeovima → bolje **manji digest wins**, ne first-seen.
- Nikad ne committaj na osnovu vote hasha koji ne odgovara poznatom leader blocku (već fixed BUG-1).

**Exit:** 74+ consensus testova zeleni; timeout + fork unit testovi; property test “no conflicting commits”.

---

## Phase 5: Mempool (`kvnc-mempool`)

### 5.1 Core Mempool
- [x] Tx pool (priority by fee rate)
- [x] **Admission control** — nonce, balance, signature, gas limit
- [x] Eviction policy
- [x] Rebroadcast

### 5.2 Block Building
- [x] Select transactions
- [x] **Conflict resolution** — same sender nonce ordering
- [x] Fee estimation (advisory)

**Tips**
- Admission (minimalno):
  1. sig valid
  2. `nonce == account.nonce` **ili** `nonce == account.nonce + k` unutar malog windowa (opcionalno)
  3. `balance >= value + fee`
  4. `gas_limit <= MAX_GAS`
- Za v1: **strict sequential nonce only** (`nonce == current`). Jednostavnije, manje edge caseova.
- Conflict: pri selectu sortiraj po `(sender, nonce)` zatim fee; odbaci kasnije nonce ako raniji nedostaje.

**Exit:** invalid tx odbijen; two txs same sender ispravno poredani u bloku; stress 10k txs OK.

---

## Phase 6: Networking (`kvnc-network`)

### 6.1 libp2p Setup
- [x] Transport (TCP + Noise + Yamux)
- [~] Discovery — Kademlia wired; **mDNS absent**
- [x] Gossipsub topics

### 6.2 Protocols
- [x] Block sync (request/response)
- [x] Transaction gossip
- [x] Vote gossip
- [x] Peer scoring / ban

### 6.3 Bootstrap
- [x] Seed nodes + connection management (bootstrap-only scope)

**Tips**
- mDNS samo za local dev; mainnet = DNS seeds + hardcode. Ne blokiraj release na mDNS.
- Vote/block broadcast: već wired preko `NetworkCommand`. Provjeri da `run_network` ne radi re-listen churn (TODO u main.rs) — dugoročno `start(&self)` ili dedicated handle.

**Exit:** 2 nodea razmijene block + vote; ban nakon N invalid blockova.

---

## Phase 7: Execution & Runtime (`kvnc-execution`, `kvnc-runtime`)

### 7.1 Native Transaction Execution
- [x] Transfer / Stake / Deploy / Call

### 7.2 WASM Runtime
- [x] Host functions (10 env imports)
- [x] Gas metering
- [~] Memory limits (config read; limiter TODO)
- [x] Determinism + module caching

### 7.3 Execution Context
- [x] State transitions, events, receipts, state root table

**Tips**
- Memory limiter: wasmi `StoreLimitsBuilder::new().memory_pages(n).build()`. Jedan red u config pathu.
- Svaki host call mora biti determinističan (nema system time osim block timestamp iz konteksta).

**Exit:** memory_limit enforce test; deterministic replay test.

---

## Phase 8: Staking & Tokenomics (`kvnc-staking`)

### 8.1 Emission & Treasury
- [x] Reward schedule, treasury vesting, circulating clamp, leader credit, constants

### 8.2 Remaining Lifecycle
- [x] **Delegation** (StakingState::delegate/unbond/slash) — bond/unbond, commission, reward sharing
- [x] **Validator rotation** (EPOCH_ROUNDS) — committee na epoch boundary
- [x] **Slashing** (DoubleSignEvidence) — double-sign detection + slash
- [ ] **Governance hooks** — parameter proposals (može Phase 25)

### 8.3–8.8
- [x] Tests, live path, contracts, RPC/CLI, docs, events

**Tips**
- Delegation v1: `delegate(validator, amount)` povećava stake; reward = proportional share nakon commission. Unbond → queue s `unbonding_period` rounds.
- Rotation: jednom po `EPOCH_ROUNDS` rebuild committee iz top N stake (15–21). Ne rotiraj mid-wave.
- Slashing v1: samo evidence tx “double sign” (dva različita blocka isti round/author s validnim sigovima) → fixed % slash + tombstone.

**Exit:** delegate/unbond e2e test; epoch rotation mijenja committee; double-sign slash test.

---

## Phase 9: Node Binary (`kvnc-node`) — DONE core

- [x] Config, core loop, graceful shutdown
- [~] Genesis još bez full premine/validator set ceremony

**Tips**
- Premine ostavi za Phase 24 genesis tool. Za dev: treasury + jedan validator iz configa.

---

## Phase 10: RPC API (`kvnc-rpc`)

- [x] JSON-RPC methods (chain/tx/account/staking/mempool/consensus/contracts)
- [x] WebSocket subscriptions
- [ ] **API client** (TypeScript) — Phase 22
- [~] `logs` subscription accepted ali event source nije fully wired

**Tips**
- Logs: publish iz `ExecutionContext` event buffera u `EventBus` pri commit. Jedan `publish_logs(receipts)`.

**Exit:** WS `logs` emitira stvarne evente nakon tx.

---

## Phase 11: CLI (`kvnc-cli`)

- [x] Wallet (keygen, import/export, sign)
- [ ] **Node operations** — status, sync, peers
- [ ] **Staking commands** — stake/unstake/delegate/claim
- [ ] Governance commands (Phase 25)
- [x] JSON / table output

**Tips**
- Status = tanki RPC wrapper: `kvnc_blockNumber` + `/health` peer_count + committee. Ne parsaj logove.

**Exit:** `kvnc-cli status` i `kvnc-cli stake` rade protiv local nodea.

---

## Phase 12: Testing & Verification

- [x] Unit tests (consensus 74/74, execution, runtime, …)
- [x] Integration (single + multi-node in-process)
- [~] Property tests (safety OK; liveness smoke only)
- [ ] **Load/stress** — TPS ≥100 na 2GB RAM; long-run DAG memory stability

**Tips**
- TPS bench: local 4-node, samo transfer txs, mjeri committed txs/sec kroz 60s. Ne optimiziraj prije mjerenja.
- Soak: 6h s `MemoryMax=3G` (systemd/docker). Logiraj RSS svakih 60s.

**Exit:** bench skripta u `ops/`; soak report u docs.

---

## Phase 13: DevOps & Deployment

- [x] Docker multi-stage + compose 4-node + health
- [ ] Kubernetes (optional Helm/StatefulSet)
- [ ] **Prometheus metrics** + Grafana + alerting

**Tips**
- Metrics prvo iz `kvnc-consensus/metrics.rs` + peer_count + mempool size + block height. Expose `/metrics` na RPC portu ili zasebnom portu.
- K8s tek kad testnet traži; compose je dovoljan za dev.

**Exit:** `/metrics` scrapeable; jedan Grafana JSON dashboard u `ops/`.

---

## Phase 14: Genesis & Testnet Launch

- [~] Genesis tool (validator keys → genesis block) — **24.1 genesis state completed** (`StakingState::genesis`)
- [ ] Key distribution ceremony
- [ ] Premine allocation (founder + treasury)
- [ ] Seed nodes (3+)
- [ ] Explorer + faucet + validator docs

**Tips**
- Genesis tool = CLI subcommand: učitaj `validators.json` + allocations → upiši genesis state + genesis block digest. Deterministički.
- Faucet: rate-limited RPC koji šalje fixed iznos s faucet keystorea. Jednostavan Axum service pored nodea.

**Exit:** svježi node od genesis filea synca s seedovima; faucet daje test KVNC.

---

## Phase 15: MysticGhost Consensus Integration

- [x] 15.0–15.4 scaffolding, mergeset, GHOSTDAG k=3, committer behind flag
- [~] 15.5 Resource hardening (prune + metrics OK; **6h soak open**)
- [ ] 15.6 Multi-node stabilisation (4/15 node, partition, 24h soak)
- [ ] 15.7 Light-client certificates (optional)

**Tips**
- Default `use_mysticghost = false` dok 15.6 ne prođe. Flag on samo u testnet experimental. **PHASE 0-4 DONE**
- Mergeset cap 1000 (config max 2000). Preko → fallback linearizer ili reject (documentiraj).

**Exit:** flag-on 4-node identical commits; RSS < 3GB pod loadom.

---

## Phase 16: Production Wiring (CRITICAL PATH)

Ovo je **trenutni bottleneck** — kod postoji, quorum još nije production-real.

- [x] **16.1 Committee from staking state**  (done: build_committee / CommitteeInfo / tests verified)
  `build_committee` čita aktivne validatore iz `StakingState`, ne hardcoda authority 0.
- [ ] **16.2 Round from consensus tip**  
  Block builder koristi `engine.current_round()` (ili watch channel), ne lokalni counter.
- [ ] **16.3 Vote ingress verified on live node**  
  Integration test: 2 procesa, stvarni TCP, vote → commit → execute.
- [ ] **16.4 Mempool admission** (vidi 5.1) wired na `sendRawTransaction` i gossip ingest.
- [ ] **16.5 Conflict-aware block building** (vidi 5.2).
- [ ] **16.6 Real 4-node quorum**  
  Docker compose s 4 različita validator keya, committee size 4, 2f+1 = 3. Identitarian committed leader sequence.

**Tips**
- 16.1 minimalno:
  ```text
  load StakingState → top N by stake (≥ MIN_VALIDATOR_STAKE)
  → AuthorityInfo { index, pubkey, stake, address }
  → CommitteeInfo
  ```
  Indexi stabilni unutar epohe (sort by pubkey pa dodijeli index).
- 16.2: `watch::channel<Round>` koji engine updatea; builder čita `borrow()`.
- 16.6: generiraj 4 keystorea u `ops/docker/validators/`; svaki node svoj `KVNC_VALIDATOR_KEY`.

**Exit:** `docker compose up` → 4 nodea proizvode iste committed heights; RPC na bilo kojem pokazuje isti `blockNumber` nakon N roundova.

---

## Phase 17: Consensus Completeness

- [ ] Timeout handling (4.3)
- [ ] Fork handling (4.4)
- [ ] Stake-weighted leader selection (1.1 / 4.3)
- [ ] Batch sig verify na ingest (1.2)
- [ ] Hash domain separation + merkle roots (1.1)

**Tips**
- Radi redom: domain separation → merkle → batch verify → timeout → fork → stake-weighted leader. Svaki ima izoliran test.
- Stake-weighted leader uključi tek kad je committee iz stakinga (16.1) stabilan.

**Exit:** svi Phase 4 open itemi zatvoreni; property testovi i dalje zeleni.

---

## Phase 18: Staking Lifecycle

- [ ] Delegation bond/unbond + commission + reward share
- [ ] Validator rotation at epoch
- [ ] Unbonding queue enforcement
- [ ] Slashing (double-sign)
- [ ] CLI: stake / unstake / delegate / claim-rewards

**Tips**
- Storage: `Delegation { delegator, validator, amount, pending_unbond }`.
- Reward distribution: pri `on_leader_committed` podijeli reward validatoru + delegatorima po shareu. Jedna funkcija, jedan test.
- Epoch rotation u istom commit pathu kad `height % EPOCH == 0`.

**Exit:** e2e: delegate → wait rewards → unbond → wait period → withdraw; slash smanjuje stake.

---

## Phase 19: State & Sync Hardening

- [ ] State Merkle (2.2)
- [ ] Snapshot / fast sync
- [ ] State pruning policy
- [ ] Light-client: wave commit + colouring certificate (15.7)
- [ ] Full missing-parent recovery via block-sync (ne samo vote re-delivery)

**Tips**
- Fast sync v1: download snapshot at height H + block headers H..tip + verify state root. Ne streamaj cijeli DAG od genesis za nove nodeove.
- Block-sync: kad `process_block` vidi missing parent → enqueue `ByHash` request. Rate-limit requests.

**Exit:** novi node od snapshot+sync dohvaća tip; light client verificira jedan commit certificate.

---

## Phase 20: Security & Hardening

- [ ] External security review (crypto, consensus, keystore)
- [ ] Fuzzing: tx decode, consensus ingest, mempool
- [ ] RPC/WS rate limit + optional auth token
- [ ] Resource budgets: 6h+ MemoryMax=3G soak, CPU, disk caps
- [ ] Keystore: no raw hex in production docs; enforce encrypted keystore path

**Tips**
- Fuzz: `cargo fuzz` na `StatementBlock` deserialize + `process_block`. Počni s 1 corpus iz unit testova.
- RPC auth v1: shared bearer token u headeru za write metode (`sendRawTransaction`). Read može ostati open na testnetu.
- Dokumentiraj threat model u `docs/SECURITY.md` (1–2 stranice).

**Exit:** fuzz 24h bez crasha; soak report; SECURITY.md.

---

## Phase 21: Observability

- [ ] Prometheus: block height, peers, mempool size, commit latency, mergeset size, RSS proxy
- [ ] Grafana dashboards (JSON u `ops/grafana/`)
- [ ] Alerting rules: peer drop, sync stall, commit lag, high memory
- [ ] Structured logging + optional trace id per round

**Tips**
- Koristi postojeći `prometheus-client` iz workspace deps. `/metrics` text exposition.
- Alert v1 = Prometheus rules YAML; ne gradi vlastiti alerter.

**Exit:** scrape + dashboard pokazuje live commit rate.

---

## Phase 22: Developer Platform

- [ ] TypeScript API client (iz OpenAPI ili ručno tipizirani fetch wrapper)
- [ ] OpenAPI/JSON schema za RPC metode
- [ ] Contract SDK (Rust + TS) deploy/call helpers
- [ ] Dev faucet CLI
- [ ] Indexer service skeleton (sluša WS events → SQL/Postgres)

**Tips**
- TS client: generiraj tipove iz liste metoda u `docs/RPC.md`. Ne čekaj savršen OpenAPI.
- Indexer v1: jedan process, `subscribe newCommittedLeader` + insert rows. Dovoljno za explorer.

**Exit:** `npm pack` client poziva `kvnc_getBalance`; indexer puni `blocks` tablicu.

---

## Phase 23: Explorer & UX

- [ ] Block explorer (blocks, txs, accounts, validators)
- [ ] Simple wallet UI (send, stake, contract call)
- [ ] Public status page (network health)

**Tips**
- Explorer = Next.js/static + TS client + indexer API. Ne čitaj direktno redb iz browsera.
- Wallet UI može biti CLI-first duže; web wallet tek nakon encrypted keystore stabilnosti.

**Exit:** javni URL pokazuje latest blocks s testneta.

---

## Phase 24: Testnet → Mainnet

- [ ] Genesis ceremony tool + docs
- [ ] Public testnet (seeds, faucet, explorer, validator guide)
- [ ] Validator onboarding + incentives
- [ ] Mainnet freeze checklist (params, audits, bug bounty)
- [ ] Mainnet launch + monitoring runbook

**Tips**
- Checklist (minimalno):  
  - svi Phase 16–20 exit criteria  
  - vanjski audit ili barem public bug bounty  
  - genesis file signed by N founders  
  - rollback plan (ne upgrade storage schema na dan launcha)
- Parametri (era, decay, min stake) **zaledi** tjedan dana prije mainneta.

**Exit:** mainnet tip napreduje; monitoring zelen; incident runbook postoji.

---

## Phase 25: Governance (post-mainnet)

- [ ] Parameter change proposals
- [ ] On-chain voting (stake-weighted)
- [ ] Treasury spend proposals
- [ ] Upgrade signaling

**Tips**
- v1 governance = off-chain signal + manual parameter file u novom genesis/upgrade epoch. On-chain voting tek kad je staking lifecycle (18) stabilan.
- Ne miješaj governance s consensus safety — parameter changes samo na epoch boundary.

**Exit:** jedan uspješan parameter change na testnetu kroz formalan proces.

---

## Dependency Graph

```
1 Types/Crypto ──┐
2 Storage ───────┼─► 3 DAG ─► 4 Consensus ─► 7 Execution ─► 9 Node
5 Mempool ───────┤         │                      │
6 Network ───────┘         │                      ▼
                           └──────────────► 10 RPC / 11 CLI
8 Staking ──────────────────────────────────► 18 Lifecycle
15 MysticGhost ──► 17 Completeness
16 Production Wiring ──► 19 Sync ──► 20 Security ──► 21 Obs
22 Dev Platform ──► 23 Explorer ──► 14/24 Testnet/Mainnet ──► 25 Gov
```

**Critical path sada:** **16 → 17 → 18 → 20 → 24**

---

## Priority Order (Next Actions)

1. **Phase 16.1–16.6** — committee, round sync, live votes, admission, 4-node quorum  
2. **Phase 5 admission + conflict** — bez toga nema sigurnog mempoola  
3. **Phase 17 timeout/fork** — consensus completeness  
4. **Phase 18 delegation + rotation** — realan validator set  
5. **Phase 15.5/15.6 soak** — MysticGhost under load  
6. **Phase 20–21** — security + metrics prije javnog testneta  
7. **Phase 14/24** — genesis + public testnet  
8. Ostalo (22–23–25) paralelno s testnetom

---

## Global Tips & Tricks (najjednostavnije a točno)

1. **Jedna istina u storeu** — ako RPC i consensus čitaju različite DB fileove, eksplicitno documentiraj što ide gdje; dugoročno izloži `DagStore` storage za block RPC.
2. **Flag > rewrite** — nove putanje (MysticGhost, stake-weighted leader) iza config flaga s default off.
3. **Test prije grepa** — svaki BUG-fix ide s regression testom koji pada bez fixa.
4. **Determinizam** — nikad `HashMap` iteracija za ordering; sort keys; fixed RNG seed u testovima.
5. **Resource budget** — svaki novi cache ima hard cap (mempool bytes, mergeset size, prune window).
6. **Minimalni genesis** — treasury + validator set iz filea; nemoj graditi ceremony UI prije toola.
7. **Commit slice** — jedan PR = jedan exit criterion (npr. “admission control only”).
8. **Ne optimiziraj TPS prije 16.6** — prvo correctness quorum, onda bench.
9. **Write path auth, read path open** (testnet) — manje trenja za explorer/faucet.
10. **Checklist u PR opisu** — `cargo test -p <crate>`, fmt, clippy `-D warnings`, ručni smoke ako dira node.

---

## Effort Snapshot (preostalo, grubo)

| Phase | Fokus | Est. | Ovisnost |
|-------|--------|------|----------|
| 16 | Production wiring | 1–2 tjedna | — |
| 17 | Consensus completeness | 1–2 tjedna | 16 |
| 18 | Staking lifecycle | 2–3 tjedna | 16 |
| 19 | State/sync | 2–3 tjedna | 16 |
| 20 | Security | 2 tjedna + audit | 16–18 |
| 21 | Observability | 1 tjedan | 16 |
| 22–23 | Dev + UX | 3–5 tjedana | 16, 10 |
| 14/24 | Testnet/Mainnet | 2–4 tjedna | 16–21 |
| 25 | Governance | 2+ tjedna | 18, 24 |

**Do javnog testneta:** ~8–14 tjedana fokusa na 16→21→14.  
**Do mainneta:** + audit + soak + freeze.

---

*Generirano iz code inspection + postojećeg tasklist.md (repo KovanicaDAG/kvnc, 2026-10-08).*  
*Status markeri odražavaju stanje koda u trenutku pisanja; pri implementaciji re-verificiraj prije checkoffa.*
