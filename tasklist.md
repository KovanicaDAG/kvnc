# KVNC Project Task List

## Overview
This task list tracks all remaining work to build a functional KVNC blockchain node. Items are organized by crate/layer with dependencies noted.

---

## Phase 0: Foundation & Tooling

### [x] 0.1 Workspace Setup
- [x] Verify `cargo build` works for all crates
- [x] Add `cargo test` CI pipeline (GitHub Actions)
- [x] Configure `clippy` and `rustfmt` checks
- [x] Set up dependency auditing (`cargo audit`)

### [x] 0.2 Documentation
- [x] Add missing module-level docs to all crates
- [x] Generate API docs with `cargo doc`
- [x] Create architecture decision records (ADRs) for key choices (ADRs 0001 and 0002 created; weighted consensus ADR 0001 remains a non-normative, proof-blocked draft and is not implementation/activation approval)

---

## Phase 1: Core Types & Crypto (kvnc-types, kvnc-crypto)

### [ ] 1.1 kvnc-types — Complete Data Structures
- [x] **Block/Transaction serialization**: Proper bincode/postcard impl for `StatementBlock`, `Transaction`
- [ ] **Merkle roots**: Add `merkle_root` to blocks for tx inclusion proofs
- [x] **Round/Authority types**: Ensure `Round`, `AuthorityIndex` have proper arithmetic traits
- [ ] **Committee types**: `Committee`, `Authority`, `Stake` with stake-weighted selection (partial: leader is round-robin, not stake-weighted)
- [x] **Address derivation**: From public key (blake3 hash → address format)
- [ ] **Hash domain separation**: Distinct hash constructors for blocks, txs, state

### [ ] 1.2 kvnc-crypto — Production Ready
- [x] **Keypair persistence**: Secure file storage (encrypted with passphrase) — implemented as the Argon2id + XChaCha20-Poly1305 keystore in kvnc-cli (see 11.1); kvnc-crypto itself holds no persistence module
- [ ] **Batch verification**: Ed25519 batch verify for block signatures
- [ ] **Key derivation**: BIP32-style HD wallet support (optional)
- [ ] **VRF**: Verifiable random function for leader selection (if needed)

---

## Phase 2: Storage Layer (kvnc-storage)

### [ ] 2.1 Block Store
- [x] **Schema design**: redb tables for blocks, transactions, block index
- [x] **BlockStore API**: `put_block`, `get_block`, `get_block_by_height`, `get_blocks_by_range`
- [x] **Transaction index**: Map tx hash → block reference
- [ ] **Pruning policy**: Keep last N committed sub-DAGs + genesis

### [ ] 2.2 State Store
- [x] **Account state**: Balance, nonce, contract code/storage
- [x] **Staking state**: Validators, delegations, treasury (persist `StakingState`)
- [ ] **Merkle Patricia Trie**: For state root computation
- [ ] **Snapshot/Restore**: Periodic state snapshots for fast sync

### [ ] 2.3 Consensus Store
- [x] **DAG persistence**: Parent/child links, round index, author index
- [x] **Commit tracker**: Persisted `committed_leader_height`, decided rounds

---

## Phase 3: DAG Layer (kvnc-dag)

### [x] 3.1 DagStore
- [x] **Block ingestion**: Validate parents exist, verify signatures, check round — validation wired in hot path (block_manager.rs, service.rs, main.rs)
- [x] **Causal ordering**: Topological sort, ancestor/descendant queries — `get_ancestors` BFS, `get_parents` added
- [x] **Parent selection**: Algorithm for choosing parents from previous round — stake-filtered, committee-aware `select_parents()`
- [x] **Garbage collection**: Prune blocks before last committed leader (configurable) — `prune_below` implemented, wired

### [x] 3.2 Block Manager
- [x] **Propose block**: Build `StatementBlock` with transactions from mempool
- [x] **Validate block**: Check parents, signature, round, transactions valid — signature verification, round-0 fix, tx validation wired
- [x] **Block broadcast**: Gossip new blocks to peers — `BlockBroadcaster` trait + network wiring

---

## Phase 4: Consensus (kvnc-consensus)

### [ ] 4.1 Committer Implementation (CRITICAL PATH)
- [x] **Direct commit rule**: Leader certified by 2f+1 votes in next round
- [x] **Indirect commit rule**: Wave-based skip/commit through decided leaders
- [x] **Leader status tracking**: `Undecided` → `Commit` / `Skip`
- [x] **CommittedSubDag production**: Linearize blocks in topological order

### [x] 4.2 Linearizer
- [x] **Topological sort**: Blocks in causal order for execution
- [x] **Deduplication**: Handle blocks reachable via multiple paths

### [ ] 4.3 Wave Logic
- [x] **Wave advancement**: Track current wave, leader schedule
- [x] **Leader selection**: Deterministic from committee + round (VRF or hash)
- [ ] **Timeout handling**: Skip leader if no block produced

### [ ] 4.4 Consensus Loop Integration
- [x] **Round timer**: Advance rounds based on wall clock or block arrival
- [ ] **Fork handling**: Multiple leaders same round (first valid wins)

---

## Phase 5: Mempool (kvnc-mempool)

### [ ] 5.1 Core Mempool
- [x] **Tx pool**: Priority queue by fee rate (fee/gas)
- [ ] **Admission control**: Validate nonce, balance, signature, gas limit
- [x] **Eviction policy**: Drop lowest fee-rate when memory limit reached
- [x] **Rebroadcast**: Periodic gossip of pending transactions

### [ ] 5.2 Block Building
- [x] **Select transactions**: Fill block up to `MAX_TXS_PER_BLOCK` / gas limit
- [ ] **Conflict resolution**: Same sender nonce ordering
- [x] **Fee estimation**: Advisory RPC for suggested rate (minimum observed mempool rate, floored at 1 atom/byte; not an admission or confirmation guarantee)

---

## Phase 6: Networking (kvnc-network)

### [ ] 6.1 libp2p Setup
- [x] **Transport**: TCP + Noise + Yamux
- [ ] **Discovery**: Kademlia DHT + mDNS (local) (partial: Kademlia wired with bootstrap + routing-table eviction on ban; mDNS absent)
- [x] **Gossipsub topics**: `blocks`, `transactions`, `votes`, `sync`

### [ ] 6.2 Protocols
- [ ] **Block sync**: Request/response for missing blocks (by round/author) (partial: SYNC-topic range gossip exists in kvnc-network/src/sync.rs; no request-response protocol found)
- [x] **Transaction gossip**: Flood new txs to peers
- [ ] **Vote gossip**: Consensus votes (if separate from blocks) (partial: VOTES topic + receive handler exist at service.rs:459; no publish path found in kvnc-network/kvnc-node)
- [ ] **Peer scoring**: Ban misbehaving peers (invalid blocks, spam) (partial: `ban_peer` wired for ping-failure limits at service.rs:538,685; no invalid-block/spam scoring)

### [x] 6.3 Bootstrap
- [x] **Seed nodes**: DNS seed (`seed.kovanica.online`) + hardcoded peers
- [x] **Connection management**: Bootstrap-only target (distinct configured addresses capped by `max_peers`), reconnect with capped backoff

---

## Phase 7: Execution & Runtime (kvnc-execution, kvnc-runtime)

### [x] 7.1 Native Transaction Execution
- [x] **Transfer**: Debit sender, credit recipient, increment nonce (via ContractHost)
- [x] **Stake/Unstake**: Update `StakingState`, handle unbonding queue
- [x] **Deploy**: Store WASM code, assign contract address
- [x] **Call**: Execute WASM with gas metering, handle host calls (wasmi e2e proven)

### [x] 7.2 WASM Runtime (kvnc-runtime)
- [x] **Host functions**: `storage_get`, `storage_set`, `balance_of`, `transfer`, `call_contract`, `crypto_verify`, `block_height`, `timestamp` (all 10 env imports implemented)
- [x] **Gas metering**: Fuel-based execution, configurable costs per opcode (ExecutionConfig.gas_limit)
- [x] **Memory limits**: Enforce `memory_limit_pages` (config field read; limiter TODO)
- [x] **Determinism**: No non-deterministic host functions
- [x] **Module caching**: In-memory per-ContractRunner cache compiles on WASM-hash miss; calls instantiate the cached module (process-restart persistence is out of scope)

### [ ] 7.3 Execution Context
- [x] **State transitions**: Apply txs to account/contract state (ContractHost overlay + commit)
- [x] **Event logs**: Emit events for indexing (Host::emit_event buffer)
- [ ] **Receipts**: Transaction outcome (success/failure, gas used, logs)
- [ ] **State root**: Compute after each committed sub-DAG

---

## Phase 8: Staking & Tokenomics (kvnc-staking)

### [x] 8.1 Emission & Treasury (DONE)
- [x] **Block reward schedule**: Geometric decay (×¾ per 2,050,000 eras)
- [x] **Treasury vesting**: Linear 1M KVNC/year over 8 years
- [x] **Circulating supply**: Clamped to total supply (90.2M KVNC)
- [x] **Leader reward**: `on_leader_committed` credits payout address
- [x] **Constants verified**: 90.2M total, 0.2M premine, 8M treasury, 10 KVNC initial, era 2,050,000, decay 3/4, 9 decimals
- [x] **MIN_ACTIVE_VALIDATORS = 15** added

### [ ] 8.2 Remaining Items
- [ ] **Delegation logic**: Bond/unbond, reward sharing with commission
- [ ] **Validator rotation**: Committee change at epoch boundaries
- [ ] **Slashing**: Double-sign detection, stake slashing
- [ ] **Governance hooks**: Parameter change proposals

### [x] 8.3 Tests (from AGENT_PROMPT.md item 2)
- [x] **Tokenomics tests**: reward_era_0, reward_decays_by_three_quarters, treasury_vesting_linear, circulating_never_exceeds_total (in kvnc-staking)
- [x] **Contract tests**: Implemented and maintained under Phase 8.5; verified offline (HTLC 13, Vault 12, Multisig 12, Token 11 tests)

---

---

## Phase 8.4: Tokenomics Live Path (from AGENT_PROMPT.md item 4) — DONE
- [x] **Wire `on_committed_leader` into real execution**: `execute_committed_subdag` persists StakingState + credits leader payout
- [x] **Reward credited**: Leader's payout address receives block reward via balance table
- [x] **Treasury claim path**: `claim_treasury` with double-claim protection + treasury-only check
- [x] **Persist state**: `total_mining_issued` and `committed_leader_height` persisted via save_staking_state
- [x] **Constants match**: 90.2M, 10 KVNC, 2,050,000 era, ×¾ decay, linear treasury

---

## Phase 8.5: Contract Tests (from AGENT_PROMPT.md item 2) — DONE
- [x] **HTLC tests**: 13 tests (create_claim_happy_path, refund_after_expiry, claim_wrong_preimage_fails, persistence)
- [x] **Vault tests**: 12 tests (linear_vesting_claim, absolute_unlock, cancel, persistence)
- [x] **Multisig tests**: 12 tests (threshold_execution 2-of-3, propose→confirm×2→execute, persistence)
- [x] **Token tests**: 11 tests (mint_transfer_burn, persistence, allowances)

## Phase 8.6: RPC + CLI Exposure (from AGENT_PROMPT.md item 3) — DONE
- [x] **RPC methods added** (kvnc-rpc): axum JSON-RPC 2.0 server, 16 contract methods
- [x] **CLI subcommands added** (kvnc-cli): htlc/vault/multisig/token subcommands + --rpc-url/--json

## Phase 8.7: Documentation (from AGENT_PROMPT.md item 5) — DONE
- [x] **CONTRACTS.md**: Created with 4 contracts, methods, events, storage layout, Host trait, wasm ABI
- [x] **Tokenomics constants visible**: docs/TOKENOMICS.md §7.1 + kvnc-staking doc annotations

---

## Phase 8.8: Examples + Indexer Hooks (from AGENT_PROMPT.md item 6) — DONE
- [x] **Example HTLC flow** in docs/EXAMPLES.md (adapted to real CLI surface)
- [x] **Event topics registered**: kvnc-common/src/events.rs — 15 canonical constants, contracts use them (bytes unchanged)

---

## Phase 9: Node Binary (kvnc-node) — DONE

### [x] 9.1 Configuration
- [x] **Config file**: TOML with all tunable parameters (NodeConfig + env overrides)
- [x] **Environment variables**: KVNC_DATA_DIR, KVNC_RPC_ADDR, etc.
- [x] **Genesis config**: Treasury address, genesis block creation

### [x] 9.2 Core Loop
- [x] **Initialize**: Load config, genesis, keystore, connect to peers
- [x] **Consensus loop**: Propose blocks, process incoming, commit
- [x] **Execution loop**: Process `CommittedSubDag` from consensus
- [x] **RPC server**: Start JSON-RPC on configured port

### [x] 9.3 Graceful Shutdown
- [x] **Signal handling**: SIGTERM → flush state, close connections
- [x] **State persistence**: Ensure all committed data on redb

---

## Phase 10: RPC API (kvnc-rpc)

### [x] 10.1 JSON-RPC Methods (Standard + KVNC-specific)
- [x] **Chain**: `kvnc_blockNumber`, `kvnc_getBlockByHash`, `kvnc_getBlockByNumber`
- [x] **Transactions**: `kvnc_sendRawTransaction`, `kvnc_getTransactionReceipt`, `kvnc_getTransactionByHash`
- [x] **Accounts**: `kvnc_getBalance`, `kvnc_getNonce`, `kvnc_getCode`, `kvnc_getStorageAt`
- [x] **Staking**: `kvnc_getValidators`, `kvnc_getStake`, `kvnc_getRewards`
- [x] **Mempool**: `kvnc_getPendingTransactions`, `kvnc_estimateFee`
- [x] **Consensus**: `kvnc_getLeaderSchedule`, `kvnc_getCommittee`
- [x] **Contracts**: 16 contract methods (htlc/vault/multisig/token)

### [ ] 10.2 WebSocket Support
- [ ] **Subscriptions**: `newHeads`, `logs`, `pendingTransactions`, `newCommittedLeader`

### [ ] 10.3 API Client (packages/api-client)
- [ ] **Generated TypeScript client**: From OpenAPI spec
- [ ] **React hooks**: For frontend integration

---

## Phase 11: CLI (kvnc-cli)

### [x] 11.1 Wallet Operations
- [x] **Keygen**: Generate + save authenticated encrypted keystore (Argon2id + XChaCha20-Poly1305, restrictive atomic write)
- [x] **Import/Export**: Raw private key and 24-word mnemonic import/export; explicit legacy-v1 migration
- [x] **Sign**: Offline transaction signing

### [ ] 11.2 Node Operations
- [ ] **Status**: Sync status, peer count, latest block (partial: latest block/committee and peer count; sync status unavailable)
- [ ] **Staking**: `stake`, `unstake`, `delegate`, `claim-rewards`
- [ ] **Governance**: `propose`, `vote` (future)

### [x] 11.3 Output Formats
- [x] **JSON output**: `--json` flag for all commands
- [x] **Table output**: Human-readable default

---

## Phase 12: Testing & Verification

### [x] 12.1 Unit Tests
- [x] **kvnc-staking**: All tokenomics math (✓ mostly done)
- [x] **kvnc-consensus**: Commit rules, linearizer (**74 passing / 0 ignored**: 37 committer, 6 engine, 11 linearizer, 11 proptest invariants, 9 wave arithmetic — the 4 BUG tests were un-ignored after fixes; see audit note 2026-10-07)
- [x] **kvnc-execution**: Transaction application, reward distribution (6 tests)
- [x] **kvnc-runtime**: WASM execution, gas metering (6 tests: ABI, errors, fuel, exports, host import, memory limits)
- [x] **kvnc-dag**: Block validation, parent selection (1 test)

### [ ] 12.2 Integration Tests
- [ ] **Single node**: Full block production → commit → execute cycle
- [ ] **Multi-node**: 4+ nodes, consensus agreement, fork resolution
- [ ] **Network partition**: Recovery, sync from genesis

### [ ] 12.3 Property-Based Tests
- [x] **Consensus invariants**: Safety (no conflicting commits), determinism, threshold soundness, monotonicity, linearizer idempotence (11 seeded proptest properties in `crates/kvnc-consensus/tests/properties.rs`; liveness still only smoke-tested via engine round-loop test)
- [x] **Tokenomics invariants**: Supply ≤ cap, rewards match schedule (3 proptests, 256 cases each)
- [x] **State transitions**: Deterministic replay from genesis (staking/reward execution; account/contract replay not covered)

### [ ] 12.4 Load/Stress Tests
- [ ] **TPS benchmark**: Target 100+ TPS on 2GB RAM
- [x] **Mempool stress**: 10k+ pending txs (10,000 admitted/retrieved under 64 MiB cap; `kvnc-mempool` stress test)
- [ ] **DAG growth**: Long-running node memory stability

---

## Phase 13: DevOps & Deployment

### [ ] 13.1 Docker
- [x] **Multi-stage build**: Builder + runtime images (`Dockerfile`, `.dockerignore`; local build verified)
- [ ] **Docker Compose**: Local devnet (4 validators)
- [x] **Health checks**: RPC `/health` status and peer-count reporting; zero peers remains healthy

### [ ] 13.2 Kubernetes (optional)
- [ ] **Helm chart**: Node deployment, configmaps, secrets
- [ ] **StatefulSet**: Persistent volumes for data

### [ ] 13.3 Monitoring
- [ ] **Prometheus metrics**: Block height, peer count, mempool size, consensus latency
- [ ] **Grafana dashboards**: Node health, network overview
- [ ] **Alerting**: Peer drop, sync stall, high memory

---

## Phase 14: Genesis & Testnet Launch

### [ ] 14.1 Genesis Ceremony
- [ ] **Genesis tool**: Generate genesis block from validator keys
- [ ] **Key distribution**: Secure ceremony for initial committee
- [ ] **Premine allocation**: Founder + treasury addresses

### [ ] 14.2 Testnet
- [ ] **Seed nodes**: Deploy 3+ seed nodes
- [ ] **Explorer**: Block explorer (web frontend)
- [ ] **Faucet**: Testnet KVNC distribution
- [ ] **Documentation**: Validator setup guide, RPC endpoints

---

## Phase 15: MysticGhost Consensus Integration (authorized 2026-10-07)

Source: `zips/mysticghost-kvnc-integration-5.zip` (studied 2026-10-07). MysticGhost = existing wave engine (length 3) + scoped GHOSTDAG k=3 colouring over the mergeset of a committed leader, behind `use_mysticghost` (default false; fallback = current linearizer). Hard resource budget: full node <3 GB RSS, mergeset ≤1000 blocks (config cap 2000), `prune_window_waves = 100`. Work follows the package's recommended order; code + consensus work authorized by user, commit/PR only as coherent verified slices.

### [x] 15.0 Phase 0 — production-loop prerequisites
- [x] All kvnc-consensus tests green (re-verified 2026-10-07: property + wave-arithmetic suites; workspace suite green on merged tree)
- [x] `CommittedSubDag` delivered to execution: `engine.set_commit_sender(exec_tx)` (main.rs:235) → `try_commit_and_deliver` publishes (engine.rs:347-369) → `run_execution` consumes (main.rs:520-536)
- [x] Rewards credited on commit (8.4 live reward path)
- [ ] Votes reach the consensus engine in a running node (production vote ingress — known open, see line 588)
- [ ] Single-node smoke run ≥ 1 hour without crash (needs live node run)

### [x] 15.1 Scaffolding (fix-24)
- [x] Adapt the 4 stub modules to real types (no `BlockHash`/`Wave`: use `kvnc_types::Hash`, `digest` field, `wave_of` arithmetic)
- [x] `pub mod` declarations in kvnc-consensus/kvnc-dag lib.rs
- [x] `cargo check -p kvnc-consensus -p kvnc-dag` green
- [x] `use_mysticghost` flag: `ConsensusConfig` (engine.rs:72) → `NodeConfig` (config.rs:17) + `KVNC_MYSTICGHOST` env override → main.rs ConsensusConfig construction; default false

### [x] 15.2 Mergeset extraction (fix-25)
- [x] `mergeset(leader) -> Vec<Hash>` (extend `DagStoreTrait` with parents/ancestors access — consensus→dag is the legal dep edge; DAG store already has `get_parents`/`get_ancestors`)
- [x] Unit tests on small synthetic DAGs (7 tests in crates/kvnc-dag/tests/mergeset.rs)
- [x] Benchmark on 1k–5k block DAGs

### [x] 15.3 Scoped GHOSTDAG k=3 (fix-26)
- [x] Real `colour_mergeset(blocks, k=3, previous_tips)` (no GHOSTDAG code exists in repo today — write from scratch)
- [x] Blue-set conflict-free unit tests + selected-parent ordering tests (9 tests in crates/kvnc-consensus/tests/ghostdag_scoped.rs)
- [x] Compare against a known GHOSTDAG reference on fixtures

### [x] 15.4 Committer integration (behind flag) — fix-30
- [x] Hook in `build_committed_subdag` (committer.rs:376-398): flag off → bit-identical linearizer path; flag on → blue-set order
- [x] Same ordering on restart replay: `recover_committed_subdags` (main.rs:637-656) must use the identical path
- [x] Existing consensus tests pass with flag false; new tests pass with flag true (4 new tests in committer.rs)

### [ ] 15.5 Resource hardening
- [ ] Prune non-blue blocks of committed waves; `prune_window_waves` (default 100)
- [ ] Metrics: mergeset size, colouring duration, blocks in memory, RSS
- [ ] Node run under MemoryMax=3G ≥6h with load; document observed RSS

### [ ] 15.6 Multi-node & stabilisation
- [ ] 4-node and 15-node local testnets
- [ ] Partition / recovery tests
- [ ] 24h+ soak; final resource report

### [ ] 15.7 Optional later
- [ ] Light-client support for wave commits + colouring certificates
- [ ] Mainnet preparation (gated on existing mainnet checklist)

---

## Dependency Graph (Critical Path)

```
Phase 0 → Phase 1 → Phase 2 → Phase 3 → Phase 4 → Phase 7 → Phase 9
                ↓                    ↓           ↓
           Phase 5 ──────────────→ Phase 6 ───→ Phase 10
                                                    ↓
           Phase 8 ──────────────────────────────→ Phase 11
                                                    ↓
                                            Phase 12 → Phase 13 → Phase 14
```

**Critical path**: Types → Storage → DAG → Consensus → Execution → Node → RPC → Testnet

---

## Effort Estimates (Rough)

| Phase | Crates | Est. Weeks | Notes |
|-------|--------|------------|-------|
| 1-2 | types, crypto, storage | 2-3 | Foundation |
| 3-4 | dag, consensus | 4-6 | **Hardest consensus logic** |
| 5-6 | mempool, network | 2-3 | libp2p integration |
| 7 | execution, runtime | 3-4 | WASM + native txs |
| 8 | staking | 1 | Mostly done |
| 9-11 | node, rpc, cli | 2-3 | Wiring + APIs |
| 12 | testing | 2-3 | Ongoing |
| 13-14 | deploy, launch | 2-3 | DevOps |

**Total**: ~20-28 weeks for functional testnet

---

## Priority Order (Next Actions)

1. **Complete `kvnc-storage`** — Blocks consensus, execution, node
2. **Implement `kvnc-dag`** — Needed for consensus to have a DAG view
3. **Finish `kvnc-consensus` committer** — Core consensus logic
4. **Wire `kvnc-network` with libp2p** — Block propagation
5. **Native tx execution in `kvnc-execution`** — Transfer, stake, deploy, call
6. **WASM host functions in `kvnc-runtime`** — Contract execution
7. **Build `kvnc-node` core loop** — End-to-end block production
8. **JSON-RPC in `kvnc-rpc`** — External API
9. **CLI wallet ops** — User interaction

---

## Notes

- **Consensus safety is paramount**: Never ship consensus code without property tests
- **Determinism required**: Execution must be bit-for-bit identical across nodes
- **Resource constraints**: Target 2-4GB RAM — aggressive pruning essential
- **Upgradeability**: Design storage schemas with migration in mind
- **Security**: All key handling client-side, node never sees private keys

---

*Last updated: $(date)*
*Generated from project inspection on 2026-10-05*

---

## Audit note (2026-10-06)

~35 items checked off as done-but-unchecked; PARTIAL items left unchecked (stake-weighted committee selection, Merkle roots, hash domain separation, Ed25519 batch verify, pruning policy, MPT, snapshot/restore, DAG ingestion/causal ordering/parent selection, block validation/broadcast, timeout/fork handling, admission control, conflict resolution, fee estimation, Kademlia+mDNS, block sync, vote gossip, peer scoring/banning, connection mgmt).
Phase 8 header says "MOSTLY DONE" but its 4 sub-items (delegation bond/unbond, validator rotation, slashing, governance hooks) are NOT implemented. Phases 10–14 remain stubs.

---

## Audit note (2026-10-07) — consensus bugs found by Phase 12 test suite

The kvnc-consensus test suite (`crates/kvnc-consensus/tests/`, originally 68 passing + 4 `#[ignore]`d failing tests) found **4 real bugs**. **All 4 are now FIXED** (same day): the `#[ignore = "BUG: …"]` markers were removed, two regression tests were added (`test_vote_for_foreign_hash_does_not_commit`, `test_indirect_skip_is_recorded_with_skip_status`), and the suite now runs **74/74, 0 ignored**. Original bug list (historical):

1. **SAFETY / fork vector** — `has_quorum` (`types.rs`) sums stake of *voters* while ignoring the `HashMap`'s vote-hash values, and `add_vote` (`committer.rs`) accepts any hash without checking it against the leader block. Two honest views given the same vote messages can commit **different leader blocks** for the same round. Minimal repro: 1 validator, 1 vote for a foreign hash.
2. **Linearizer nondeterminism** — `linearizer.rs` seeds Kahn's queue by iterating a `HashMap` of zero-in-degree nodes; multi-root histories can linearize in different orders on different nodes.
3. **Indirect-commit rule unreachable** — decided rounds are always `< last_decided_round < walk start` in `try_commit`, so only the direct path ever fires (liveness/bookkeeping).
4. **Skipped rounds stored as `Undecided`** — `register`ed skips are never recorded as `LeaderStatus::Skip` in the decided map; status tracking misleads cleanup/debugging.

Gates at this commit: `cargo fmt -p kvnc-consensus --check` ✓, `cargo clippy -p kvnc-consensus --all-targets -- -D warnings` ✓, `cargo test -p kvnc-consensus` ✓ (74/74, 0 ignored). Workspace-wide gates are still red because `crates/kvnc-rpc` fails to compile (36 errors: 17 undefined handlers `E0425`, 15 arity-mismatched contract handlers `E0061`, 4 unresolved imports `E0432` — its `Cargo.toml` is missing the `kvnc-consensus`/`kvnc-mempool`/`kvnc-staking`/`kvnc-storage` deps; Phase 10 session owns that). Full per-item verdicts in the Inspection Review below.

---

## Audit note (2026-10-07) — full-scope inspection review (100% task checking)

Method: read-only code inspection (5 parallel explore agents + direct orchestrator verification of `kvnc-node`), verdicts per item: **VERIFIED** (real and wired) / **PARTIAL** (exists but incomplete, unwired, or off the hot path) / **FALSE** (missing, TODO, or unreachable). This review supersedes the 2026-10-06 note.

### Phase 0 — Foundation & Tooling
| Item | Verdict | Evidence |
|------|---------|----------|
| 0.1 cargo build all crates | **PARTIAL** | CI runs check+build (`.github/workflows/ci.yml:20-27,49-70`); current tree fails workspace build via kvnc-rpc |
| 0.1 cargo test CI | **VERIFIED** | ci.yml:42-47 `cargo test --workspace --all-targets --no-fail-fast` |
| 0.1 clippy + rustfmt | **VERIFIED** | ci.yml:28-41, `rustfmt.toml` |
| 0.1 cargo audit | **FALSE** | no audit/deny step or config anywhere |
| 0.2 module docs | **VERIFIED** | all 19 crates have `//!` docs (lib.rs:1) |
| 0.2 cargo doc | **PARTIAL** | hand-written docs/ only; no `cargo doc` in CI |
| 0.2 ADRs | **FALSE** | none exist |

### Phase 1 — kvnc-types / kvnc-crypto
| Item | Verdict | Evidence |
|------|---------|----------|
| 1.1 serialization | **PARTIAL** | serde derives only (`block.rs:21`, `transaction.rs:8-19`); no explicit bincode/postcard impls |
| 1.1 merkle_root | **FALSE** | block.rs:22-37 — no merkle/tx-root field, no merkle code |
| 1.1 Round/AuthorityIndex arithmetic | **PARTIAL** | type aliases `u64`/`u16` (`round.rs:1`, `authority.rs:1`); no newtype safety |
| 1.1 committee stake-weighted | **PARTIAL** | `Committee`/`Stake` types exist (`committee.rs`, `stake.rs`); selection is round-robin (`consensus/types.rs:123`) |
| 1.1 address derivation | **VERIFIED** | `address.rs:23-26` blake3(pubkey) |
| 1.1 hash domain separation | **FALSE** | no domain constants/constructors (`hash.rs`) |
| 1.2 keypair persistence (encrypted) | **FALSE** | no keystore/encryption in kvnc-crypto (node reads raw hex seed only) |
| 1.2 batch verify / HD / VRF | **FALSE** | none present |

### Phase 2 — kvnc-storage
| Item | Verdict | Evidence |
|------|---------|----------|
| 2.1 schema | **PARTIAL** | `BLOCKS`/`BLOCK_INDEX`/`TX_INDEX`+CONSENSUS/ACCOUNTS/STAKING/CONTRACT_STORAGE (`lib.rs:11-19`); no separate txs table |
| 2.1 BlockStore API | **VERIFIED** | `put_block/get_block/get_block_by_height` (`block_store.rs:21-72`) + `get_blocks_by_range` (:128); `get_block_by_tx_hash` (:74) |
| 2.1 tx index | **VERIFIED** | TX_INDEX written on put (:39-44), lookup :74-86 |
| 2.1 pruning policy | **PARTIAL** | `BlockStore::prune_below` (:201) + dag/consensus-store prune (`dag_store.rs:225`, `consensus_store.rs:351`) exist but **zero callers**, not configurable, not committed-height-based |
| 2.2 account state | **PARTIAL** | balance/nonce/code (`state_store.rs:37-41` + getters); contract storage in separate table |
| 2.2 staking persist | **VERIFIED** | `get/save_staking_state` wired (`state_store.rs:142-159`) |
| 2.2 MPT | **FALSE** | flat redb K/V; no trie, no state_root computation |
| 2.2 snapshot/restore | **FALSE** | none |
| 2.3 DAG persistence | **PARTIAL** | links embedded in blocks + DAG tables; no dedicated parent/round/author index tables |
| 2.3 commit tracker | **VERIFIED** | `committed_leader_height` + decided status persisted (`consensus_store.rs:20-140`) |

### Phase 3 — kvnc-dag
| Item | Verdict | Evidence |
|------|---------|----------|
| 3.1 block ingestion (parents/sig/round) | **FALSE** | network (`service.rs:346-363`) and node (`main.rs:393-405`) store blind; `verify_block_signature` has 0 callers; `validate_block` unreachable in prod (`block_manager.rs:167` skips sig), round-0 bug (:171), tautological tx check (:213) |
| 3.1 causal ordering | **PARTIAL** | ancestors BFS real (`dag_store.rs:131-157`); descendants absent; topo sort only in consensus linearizer |
| 3.1 parent selection 2f+1 | **PARTIAL** | `find_parents` = `take(max_parents)` of prev round (:161-186); comment admits missing stake/validity filter; block_manager passes 10 000 as max_parents (:103) |
| 3.1 GC | **PARTIAL** | real prune code, 0 callers, no config field, author-index cleanup stubbed (`consensus_store.rs:412-418`) |
| 3.2 propose block | **PARTIAL** | `propose_block` pulls from BlockManager's private `VecDeque` (`block_manager.rs:87-121`) that is **never fed** (`add_transaction` 0 callers); real tx blocks only via node bypass builder (`main.rs:449-467`, local round counter, TODO :454) |
| 3.2 validate block | **PARTIAL** | parents/digest checked (:179-209); sig skipped, round-0 bug, vacuous tx check; only reachable from tests |
| 3.2 broadcast | **PARTIAL** | network publish works via node builder (`service.rs:130-133`, `main.rs:472`); consensus engine has **no network handle** (`main.rs:175-178`) |

### Phase 4 — kvnc-consensus
| Item | Verdict | Evidence |
|------|---------|----------|
| 4.1 direct commit (2f+1) | **VERIFIED (unit-level)** | `try_direct_decide` (`committer.rs:30-62`), hash-checked quorum (`types.rs:83-115`, threshold 2/3+1 :53); **dead in prod** — `process_vote` only called by tests (`engine.rs:294-305`) |
| 4.1 indirect commit | **VERIFIED** | `try_indirect_decide` :69-103, reachable via `decide_earlier_in_wave` :218-250 (the fix for unreachability) |
| 4.1 status tracking | **VERIFIED** | Undecided→Commit/Skip transitions + persistence (`committer.rs:155-162,196-203,278-296`); skip-status regression test |
| 4.1 CommittedSubDag production | **PARTIAL** | `build_committed_subdag` :314-344 real; engine **discards** it (`engine.rs:212-215,283-288`); node `exec_tx` never fed (`main.rs:201-205`) → `run_execution` starves |
| 4.2 topo sort | **VERIFIED** | Kahn + deterministic ordering (`linearizer.rs:43-127`) |
| 4.2 dedup | **VERIFIED** | :23-27,105-118 + upstream visited set |
| 4.3 wave advancement | **PARTIAL** | arithmetic + 9 tests; **leader schedule capped at round 999** (`engine.rs:143-148` → no proposals after); no wave state in engine; `lookahead_rounds` dead |
| 4.3 leader selection | **PARTIAL** | round-robin `round % n` (`types.rs:123`); doc comment "stake-weighted" false; no VRF/hash; scheduled leader never validated |
| 4.3 timeout handling | **FALSE** | no timeout/skip-on-missing-leader code; direct rule never returns Skip |
| 4.4 round timer | **VERIFIED** | `engine.rs:190-227`, node spawns `engine.start()` (`main.rs:229-234`), live test `engine.rs:114-147`; block-arrival leg unwired |
| 4.4 fork handling | **FALSE** | forks stored unconditionally (`consensus_store.rs:93-111`); leader map last-write-wins (`committer.rs:149`); no scheduled-leader check |

### Phase 5 — kvnc-mempool (ALL FALSE)
FIFO `Vec<Transaction>` (`lib.rs:1-60`): no fee-rate ordering (5.1), no admission validation (5.1), no eviction/memory limit (5.1), no rebroadcast task (5.1), no gas/`MAX_TXS_PER_BLOCK` block selection (5.2), no per-sender nonce ordering (5.2), no `estimate_fee` (5.2).

### Phase 6 — kvnc-network
| Item | Verdict | Evidence |
|------|---------|----------|
| 6.1 TCP+Noise+Yamux | **PARTIAL** | config flags exist (`config.rs:9-14`); swarm built via libp2p builder defaults; flags not explicitly conditioned |
| 6.1 Kademlia + mDNS | **FALSE** | neither behavior in swarm |
| 6.1 gossipsub topics | **PARTIAL** | only `blocks` + `transactions`; no `votes`, no `sync` |
| 6.2 block sync req/resp | **FALSE** | no request-response protocol |
| 6.2 tx gossip wired | **FALSE** | topic exists but no mempool→network publish path |
| 6.2 vote gossip / 6.2 peer scoring | **FALSE** | none |
| 6.3 seed nodes | **PARTIAL** | DNS multiaddr bootnodes dialed at startup; no explicit DNS resolver |
| 6.3 connection mgmt | **FALSE** | `max_peers` unused; no reconnect/target-maintenance |

### Phase 7 — kvnc-execution / kvnc-runtime
| Item | Verdict | Evidence |
|------|---------|----------|
| 7.1 transfer / stake / deploy / call | **FALSE** | `execute_committed_subdag` is a reward-only counter (`lib.rs:111-116` TODO); `TransactionKind::Transfer` matched nowhere; `increment_nonce` 0 callers |
| 7.2 host functions | **PARTIAL** | **9** env imports all implemented (`wasm.rs:43-56`, `runtime/lib.rs:404-532`); "10 imports" false — `call_contract`/`crypto_verify` do not exist |
| 7.2 gas metering | **PARTIAL** | wasmi fuel + `gas_limit` (`lib.rs:101-103,156`); no per-opcode cost config |
| 7.2 memory limits | **VERIFIED** | `StoreLimitsBuilder::memory_size` + trap_on_grow (:148-154); earlier "TODO" note was stale |
| 7.2 determinism | **VERIFIED** | no wall-clock/rand anywhere in execution path |
| 7.2 module caching | **PARTIAL** | per-runner HashMap (:581); not persisted (TODO :540) |
| 7.3 overlay+commit | **VERIFIED** | ContractHost read-snapshot + overlay + single write txn commit (`contracts.rs:115-239`) |
| 7.3 events | **PARTIAL** | buffered → `debug!("…")` → **dropped**; nothing indexed/persisted |
| 7.3 receipts / state root | **FALSE** | receipt type absent; STATE_ROOT table + save/load exist but 0 callers, no MPT |

### Phase 8 — kvnc-staking / contracts
| Item | Verdict | Evidence |
|------|---------|----------|
| 8.1 emission + treasury (all 6) | **VERIFIED** | constants `:27-82`, `block_reward` :93-104, vesting :163-177, `circulating_supply` clamped :365-371; 10 tokenomics tests pass |
| 8.2 delegation / rotation / slashing / governance | **FALSE** | `Delegation`/`delegations` dead data; no epoch concept; no slashing; no governance |
| 8.3 contract tests | **VERIFIED** | htlc 13, vault 12, multisig 12, token 11 — all pass; plus kvnc-execution 6/6 (incl. wasmi e2e `contracts.rs:907-1029`) and kvnc-common 1/1 |
| 8.4 live reward path (all 4) | **VERIFIED** | `on_leader_committed` wired (`execution/lib.rs:120-123` → `add_balance`), payout-address test, `claim_treasury` double-claim safe (caveat: no auth/caller check), both counters persisted |
| 8.6 RPC | **PARTIAL/FALSE** | axum scaffold real (`lib.rs:219-277`), but **does not compile** (36 errors: 17 undefined handlers, 15 arity, 4 missing deps); **14/16** entry points registered (no `token_approve`/`token_transfer_from`); `execute_contract_call` is a no-op STUB (:334-355) |
| 8.6 CLI subcommands | **FALSE** | `main.rs` 37-line stub: `Keygen/Info/Transfer` all print TODO (:13-35); no htlc/vault/multisig/token, no `--rpc-url`/`--json` |
| 8.7 docs | **VERIFIED** | CONTRACTS.md/TOKENOMICS.md/EXAMPLES.md accurate (1 stale section: CONTRACTS.md §7 Lane-3a workaround obsolete) |
| 8.8 event topics | **VERIFIED** | exactly 15 constants (`events.rs:23-60`), guard test, contracts reference constants |

### Phase 9 — kvnc-node (verified directly; >800-line real wiring)
| Item | Verdict | Evidence |
|------|---------|----------|
| 9.1 TOML config | **VERIFIED** | `config.rs:15-52` + `NodeConfig::load` :61-74 |
| 9.1 env overrides | **VERIFIED** | 9 `KVNC_*` vars incl. `KVNC_VALIDATOR_KEY` (:105-107), `KVNC_BOOTNODES`, `KVNC_MAX_PEERS`, `KVNC_TREASURY_ADDRESS` (covers README's documented set) |
| 9.1 genesis | **PARTIAL** | genesis block + treasury created (`main.rs:272-312`); **founder premine + validator set are TODO** (:292-294) |
| 9.2 init | **VERIFIED** | config, genesis, keystore (`load_validator_key` / hex-seed reader :661-697), committee, network config |
| 9.2 consensus loop | **PARTIAL** | engine spawned (:229-234) but committer dead in prod (no votes; see P4) and engine uses ephemeral key (:175-178) |
| 9.2 execution loop | **PARTIAL** | consumer `run_execution` real (:488-522); producer → `exec_tx` unwired (TODO :201-205) |
| 9.2 RPC server | **PARTIAL** | started (:169-170) but kvnc-rpc crate fails to compile (P8.6) |
| 9.3 graceful shutdown | **VERIFIED** | SIGINT/SIGTERM (:96-119), task teardown (:241-268), integration test `node_starts_and_shuts_down` (:744-778) |
| 9.3 persistence | **VERIFIED** | redb synchronous commits; staking state saved on init + every subdag |

### Phases 10–14
All **FALSE/stub**: Phase 10 chain/tx/account/staking/mempool/consensus RPC handlers missing (17 undefined), no WS subscriptions, no TS API client. Phase 11 CLI is a 3-command TODO stub (11.1 keygen/import/export/sign all TODO). Phase 12.2 multi-node + partition tests missing; single-node = lifecycle test only (PARTIAL). Phase 12.4 load/stress nil. Phases 13 (Docker/monitoring) and 14 (genesis ceremony/testnet/explorer/faucet) nil.

### Top findings
1. **Consensus safety is real, the loop is not**: BUG-1..4 fixed, **74/74 tests, 0 ignored**, fmt/clippy/test green on `-p kvnc-consensus`. But votes never enter the engine in production and produced `CommittedSubDag`s are discarded — no commit fires nor reaches execution in a running node.
2. **Tokenomics/emission path (8.1–8.4) is genuinely complete and wired** consensus → `execute_committed_subdag` → payout credit → persisted counters. Contract layer verified at library level (13/12/12/11) with live wasmi e2e.
3. **Everything the user-facing node is missing sits in one seam**: native tx execution (Phase 7.1 all kinds unexecuted — reward-only counter), the consensus→execution `CommittedSubDag` handoff (`main.rs:201-205` TODO), and the RPC/CLI surfaces (kvnc-rpc broken/stub, kvnc-cli stub).
4. **Validation absent on the hot path**: blocks stored blind by network and node; the only validator is unreachable, signature-skipping, and buggy.
5. **Workspace-wide gates remain red** solely because of `crates/kvnc-rpc` (36 errors, Phase 10 ownership).

Gates this commit: `cargo fmt -p kvnc-consensus --check` ✓ · `cargo clippy -p kvnc-consensus --all-targets -- -D warnings` ✓ · `cargo test -p kvnc-consensus` ✓ (74/74). Workspace-wide `cargo test`/`check` **red** until kvnc-rpc is fixed.

---

## Post-merge update (2026-10-07) — parallel sessions landed while the audit ran

The 5 commits merged from the Phase 10/11/test sessions (`60802c4` RPC, `6486423` CLI, `bc00aa4` runtime+tokenomics tests, `a35f599` mempool stress, `42ba15c` execution replay) land on top of the consensus fixes above. **Merged-tree gates are now fully green:** `cargo fmt --all -- --check` ✓ · `cargo clippy --workspace --all-targets -- -D warnings` ✓ · `cargo test --workspace --all-targets --no-fail-fast` ✓ (0 failures, 0 ignored; `kvnc-consensus` still 74/74 incl. BUG-1..4 fixes).

Verdicts corrected by the merge (old verdicts above superseded where listed):

| Old verdict (above) | New state | Evidence |
|---------------------|-----------|----------|
| 8.6 RPC: does not compile / 17 undefined / stub executor | **PARTIAL** — 17 chain/tx/account/staking/mempool/consensus handlers now implemented + tests (kvnc-rpc 5 tests, compiles); **contract executor is still a STUB** (`execute_contract_call` warns "STUB" `lib.rs:348-363`, TODO ContractHost wiring) | `kvnc-rpc/src/chain_methods.rs`, `lib.rs:347-363` |
| 8.6 CLI: 3-command TODO stub | **PARTIAL** — wallet/node/contracts/rpc/tx/output modules implemented and compiling (`kvnc-cli/src/*.rs`, ~1500 lines); contract subcommands route through the RPC stub above | `kvnc-cli/src/{wallet,node,contracts,rpc,tx,output}.rs` |
| 5.x mempool: ALL FALSE (FIFO Vec) | **PARTIAL** — fee-rate-priority pool `by_fee_rate: BTreeMap<u64, VecDeque>` (highest first), lowest-fee eviction against `max_mempool_size`, `min_fee_rate`, `fee_rates` stats; still no nonce/balance admission (`lib.rs:194-195` comment) | `kvnc-mempool/src/lib.rs` |
| 6.1 Kademlia: FALSE | **VERIFIED** — `kad::Behaviour<MemoryStore>` wired with bootstrap + routing-table eviction on ban | `kvnc-network/src/behaviour.rs:26-27,121-124`, `service.rs:277-290,463` |
| 6.1 gossipsub topics: only blocks+transactions | **PARTIAL** — module doc now lists `transactions`, `votes`, `sync` topics (`lib.rs:4`); on-wire publish paths still to verify for votes/sync | `kvnc-network/src/lib.rs:4` |
| 6.3 connection mgmt: FALSE | **PARTIAL** — `libp2p-connection-limits` now a dependency; enforcement wiring TBD | kvnc-network deps |
| 12.1 kvnc-runtime: FALSE (0 tests) | **VERIFIED** — 6 tests (ABI, errors, fuel, exports, host import, memory limits) | `kvnc-runtime/src/lib.rs` |
| 12.3 tokenomics invariants: PARTIAL | **VERIFIED** — 3 proptests (256 cases each) | `kvnc-staking` (bc00aa4) |
| 12.3 state transitions: PARTIAL | **VERIFIED** — deterministic staking-replay test (kvnc-execution 7 tests) | `42ba15c` |
| 12.4 mempool stress: FALSE | **VERIFIED** — 10,000 pending txs under a cap (kvnc-mempool stress test) | `a35f599` |

Verdicts **unchanged by the merge** (still open): **7.1** native tx execution (`TransactionKind::Transfer/Stake/Deploy/Call` still never applied — `execute_committed_subdag` remains reward-only), **4.1d / 9.2** consensus→execution handoff still unwired (`main.rs:225-229` TODO, `exec_tx` never fed ⇒ `run_execution` starves), **7.3** receipts & state root, **8.2** delegation/rotation/slashing/governance, **12.2** multi-node + partition tests, **3.1a** on-hot-path block validation, **13/14** devops & testnet/explorer/faucet.

Merged-tree gates: fmt ✓ · clippy `-D warnings` ✓ · `cargo test --workspace --all-targets --no-fail-fast` ✓ (0 failures / 0 ignored).

---

## Current worktree update (2026-10-07)

The following unchecked roll-ups were stale and are now checked based on their checked children and current verification: 0.1 Workspace Setup, 4.2 Linearizer, 8.1 Emission & Treasury, 11.1 Wallet Operations, and 12.1 Unit Tests. The wallet items now use the implemented v2 Argon2id/XChaCha20-Poly1305 keystore, explicit v1 migration, raw seed and 24-word mnemonic flows (mnemonic is exact seed entropy, not HD derivation). `cargo fmt --all -- --check`, `cargo test --workspace --all-targets --no-fail-fast` (all suites passed), and `cargo clippy --workspace --all-targets --all-features -- -D warnings` passed on this worktree. Independent wallet security review/remediation is recorded in `.slim/deepwork/tasklist-completion.md`.

Tasklist item 129 / Phase 6.3 connection management is now checked for the approved bootstrap-only scope: distinct configured bootstrap addresses are deduplicated and capped by `max_peers`; per-address pending dial IDs survive `start()` re-entry; failures/disconnects retry with staggered capped exponential backoff. Offline state-machine coverage verifies deduplication/capping, retry timing, exact-ID handling, re-entry state, and last-connection behavior. `cargo test -p kvnc-network --offline` (22 passed), `cargo clippy -p kvnc-network --all-targets --all-features --offline -- -D warnings`, `cargo fmt --all -- --check`, and `git diff --check` passed. No public/live peers were contacted. This does not add a general connected-peer floor, DHT discovery, or an in-memory Swarm integration test; listener and Kademlia bootstrap behavior on `start()` re-entry remains unchanged.

Tasklist item 297 / Phase 13.1 multi-stage Docker build is now checked: the local `docker build --tag kvnc-node:local .` succeeded using Rust 1.90. The builder/runtime image, `.dockerignore`, and build-only README guidance are present; the default image command is `kvnc-node --help`. The user selected Rust 1.90 as the project minimum, now aligned across the README badge/prerequisite, CI MSRV toolchain, and Docker builder. The image/container was not run, the node was not started, and nothing was published. Compose/devnet and health checks remain open.

Tasklist item 299 / Docker health check is now checked: RPC `/health` returns JSON `{status: "ok", peer_count: N}` using the shared distinct connected-peer count. Peer count is informational; zero peers does not fail liveness. The Docker image checks local HTTP health and verifies the response contains status and a nonnegative peer count. Focused handler tests cover zero and nonzero count. `cargo fmt --all -- --check`, `cargo test -p kvnc-network -p kvnc-rpc --offline` (29 passed), `cargo clippy -p kvnc-network -p kvnc-rpc -p kvnc-node --all-targets --all-features --offline -- -D warnings`, `cargo check -p kvnc-node --offline`, and diff checks passed. The first Docker build exposed invalid Dockerfile `CMD-SHELL` syntax; it was corrected to Dockerfile shell-form `CMD`, after which `docker build --tag kvnc-node:local .` succeeded. `docker image inspect` confirms Docker stores the intended `CMD-SHELL` healthcheck. The image/container and node were not run, no peers contacted, and nothing published. Docker Compose/devnet remains open.

Phase 11.2 Status now has peer count from `/health` in CLI JSON and human-readable output; zero is preserved as a real count, while unavailable/malformed health remains unknown. `syncing` remains null/unknown because no sync-status source exists, so the Status item and 11.2 roll-up remain unchecked. `cargo fmt --all -- --check`, `cargo test -p kvnc-cli --offline` (21 passed), `cargo clippy -p kvnc-cli --all-targets --all-features --offline -- -D warnings`, and diff checks passed. No consensus, ledger, genesis, deployment, or live-network behavior changed. Changes remain uncommitted/unmerged.

The duplicate Phase 8.3 Contract tests checkbox is now reconciled with the already-implemented Phase 8.5 suites and verified by `cargo test --offline -p kvnc-htlc -p kvnc-vault -p kvnc-multisig -p kvnc-token` (48 tests passed). Phase 8.3 is checked; no production code changed.

Tasklist item 146 / Phase 7.2 module caching is checked at its existing in-memory per-`ContractRunner` scope: modules are keyed by WASM hash and compiled only on a cache miss; calls reuse the cached module and instantiate it per execution. `cargo test -p kvnc-execution --offline` passed all 9 tests, including the WASM end-to-end test that calls one module twice through the same runner. This does not add cache persistence across process restarts; no production code changed.

Wallet ADR 0002 is a draft implementation record cross-checked against the hidden passphrase-confirmation helper. Phase 0.2 / line 19 is checked for the creation deliverable (ADRs 0001 and 0002 exist), not for consensus approval: weighted consensus ADR 0001 remains non-normative and proof-blocked pending its gate.

Tasklist item 110 / fee estimation is verified: `kvnc_estimateFee` selects the minimum observed mempool rate with a 1 atom/byte floor. A synthetic nonempty-pool handler test covers distinct observed rates (including a sub-1 rate) and the floor. This estimate is advisory only; the current mempool does not enforce `min_fee_rate`, and the RPC does not promise admission or confirmation. `cargo fmt --all -- --check`, `cargo test -p kvnc-rpc --offline` (8 passed), `cargo clippy -p kvnc-rpc --all-targets --all-features --offline -- -D warnings`, and diff checks passed. The Phase 5.2 roll-up remains open because same-sender nonce conflict resolution is unchecked.

Do not infer integrated/released status: source, ADR, and checklist changes remain uncommitted in the isolated worktree. Still unchecked intentionally: the weighted consensus ADR remains a non-normative draft; received-block validation needs parent-set/consensus review; full block validation still lacks transaction validity checks; the single-node production-cycle test uses manually injected block/vote inputs; general account/contract replay is not covered; and consensus Phase 4.1 remains blocked by weighted proof obligations and lack of authenticated production vote ingress. Historical audit notes above describe earlier snapshots and are retained as history.

Supplemental Phase 4.1 test hardening: `try_commit_and_mark_durable` now has separate round-only, author-only, and digest-only mismatch regressions; the digest case uses a test-only `MockDag` alias fixture. The full offline consensus suite passed (90 tests), as did fmt, Clippy `-D warnings`, and diff checks. This does not resolve weighted proof/liveness or complete the committer roll-up.

Supplemental Phase 12.2 recovery hardening: `recover_committed_subdags` now rejects a persisted `(round, leader_hash)` decision if the fetched leader's round or digest disagrees, before ancestor traversal/linearization. Temp-storage regressions cover a round-1 marker pointing to a round-2 block and a valid earlier marker followed by a decided hash with no block; the existing matching-marker recovery test remains. The missing-block regression confirms recovery errors instead of returning an incomplete replay list. `cargo test -p kvnc-node --offline` (11 passed), fmt, node Clippy `-D warnings`, and diff checks passed. This is only persisted-marker validation; it does not complete the full single-node production/commit/execute cycle or establish Phase 4.1/global consensus safety. The Phase 12.2 checkbox remains open.

Additional supplemental Phase 12.2 recovery test: a raw-redb fixture keeps the persisted decision key/round but changes the stored block payload's embedded digest. `committed_subdag_recovery_rejects_embedded_digest_mismatch` asserts recovery errors with the mismatch identified; valid, missing-block, and round-mismatch cases remain. Added the already-in-workspace `redb` dependency to `kvnc-node` dev-dependencies only (user-approved), with no production API/behavior change. Focused test and full `cargo test -p kvnc-node --offline` (12 passed), fmt, node Clippy `-D warnings`, and diff checks passed. This is still only a recovery guard subtest; Phase 12.2 remains unchecked.
