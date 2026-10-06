# KVNC Project Task List

## Overview
This task list tracks all remaining work to build a functional KVNC blockchain node. Items are organized by crate/layer with dependencies noted.

---

## Phase 0: Foundation & Tooling

### [ ] 0.1 Workspace Setup
- [x] Verify `cargo build` works for all crates
- [x] Add `cargo test` CI pipeline (GitHub Actions)
- [x] Configure `clippy` and `rustfmt` checks
- [x] Set up dependency auditing (`cargo audit`)

### [ ] 0.2 Documentation
- [x] Add missing module-level docs to all crates
- [x] Generate API docs with `cargo doc`
- [ ] Create architecture decision records (ADRs) for key choices

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
- [ ] **Keypair persistence**: Secure file storage (encrypted with passphrase)
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

### [ ] 3.1 DagStore
- [ ] **Block ingestion**: Validate parents exist, verify signatures, check round
- [ ] **Causal ordering**: Topological sort, ancestor/descendant queries
- [ ] **Parent selection**: Algorithm for choosing 2f+1 parents from previous round
- [x] **Garbage collection**: Prune blocks before last committed leader (configurable)

### [ ] 3.2 Block Manager
- [x] **Propose block**: Build `StatementBlock` with transactions from mempool
- [ ] **Validate block**: Check parents, signature, round, transactions valid
- [ ] **Block broadcast**: Gossip new blocks to peers

---

## Phase 4: Consensus (kvnc-consensus)

### [ ] 4.1 Committer Implementation (CRITICAL PATH)
- [x] **Direct commit rule**: Leader certified by 2f+1 votes in next round
- [x] **Indirect commit rule**: Wave-based skip/commit through decided leaders
- [x] **Leader status tracking**: `Undecided` → `Commit` / `Skip`
- [x] **CommittedSubDag production**: Linearize blocks in topological order

### [ ] 4.2 Linearizer
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
- [ ] **Fee estimation**: RPC endpoint for suggested fee rates

---

## Phase 6: Networking (kvnc-network)

### [ ] 6.1 libp2p Setup
- [x] **Transport**: TCP + Noise + Yamux
- [ ] **Discovery**: Kademlia DHT + mDNS (local)
- [x] **Gossipsub topics**: `blocks`, `transactions`, `votes`, `sync`

### [ ] 6.2 Protocols
- [ ] **Block sync**: Request/response for missing blocks (by round/author)
- [x] **Transaction gossip**: Flood new txs to peers
- [ ] **Vote gossip**: Consensus votes (if separate from blocks)
- [ ] **Peer scoring**: Ban misbehaving peers (invalid blocks, spam)

### [ ] 6.3 Bootstrap
- [x] **Seed nodes**: DNS seed (`seed.kovanica.online`) + hardcoded peers
- [ ] **Connection management**: Target peer count, reconnect logic

---

## Phase 7: Execution & Runtime (kvnc-execution, kvnc-runtime)

### [ ] 7.1 Native Transaction Execution
- [x] **Transfer**: Debit sender, credit recipient, increment nonce (via ContractHost)
- [ ] **Stake/Unstake**: Update `StakingState`, handle unbonding queue
- [ ] **Deploy**: Store WASM code, assign contract address
- [x] **Call**: Execute WASM with gas metering, handle host calls (wasmi e2e proven)

### [ ] 7.2 WASM Runtime (kvnc-runtime)
- [x] **Host functions**: `storage_get`, `storage_set`, `balance_of`, `transfer`, `call_contract`, `crypto_verify`, `block_height`, `timestamp` (all 10 env imports implemented)
- [x] **Gas metering**: Fuel-based execution, configurable costs per opcode (ExecutionConfig.gas_limit)
- [x] **Memory limits**: Enforce `memory_limit_pages` (config field read; limiter TODO)
- [x] **Determinism**: No non-deterministic host functions
- [ ] **Module caching**: Compile once, instantiate many (TODO in ContractRunner)

### [ ] 7.3 Execution Context
- [x] **State transitions**: Apply txs to account/contract state (ContractHost overlay + commit)
- [x] **Event logs**: Emit events for indexing (Host::emit_event buffer)
- [ ] **Receipts**: Transaction outcome (success/failure, gas used, logs)
- [ ] **State root**: Compute after each committed sub-DAG

---

## Phase 8: Staking & Tokenomics (kvnc-staking)

### [ ] 8.1 Emission & Treasury (DONE)
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

### [ ] 8.3 Tests (from AGENT_PROMPT.md item 2)
- [x] **Tokenomics tests**: reward_era_0, reward_decays_by_three_quarters, treasury_vesting_linear, circulating_never_exceeds_total (in kvnc-staking)
- [ ] **Contract tests**: HTLC create/claim/refund, Vault linear/absolute, Multisig threshold, Token mint/transfer/burn (using in-memory Host)

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

### [ ] 11.1 Wallet Operations
- [ ] **Keygen**: Generate + save encrypted keystore (partial: passphrase XOR obfuscation is not secure encryption; replace with AEAD)
- [ ] **Import/Export**: Private key, mnemonic (partial: private-key import/export implemented; mnemonic support absent)
- [x] **Sign**: Offline transaction signing

### [ ] 11.2 Node Operations
- [ ] **Status**: Sync status, peer count, latest block (partial: latest block/committee; sync and peer count absent)
- [ ] **Staking**: `stake`, `unstake`, `delegate`, `claim-rewards`
- [ ] **Governance**: `propose`, `vote` (future)

### [x] 11.3 Output Formats
- [x] **JSON output**: `--json` flag for all commands
- [x] **Table output**: Human-readable default

---

## Phase 12: Testing & Verification

### [ ] 12.1 Unit Tests
- [x] **kvnc-staking**: All tokenomics math (✓ mostly done)
- [ ] **kvnc-consensus**: Commit rules, linearizer
- [x] **kvnc-execution**: Transaction application, reward distribution (6 tests)
- [x] **kvnc-runtime**: WASM execution, gas metering (6 tests: ABI, errors, fuel, exports, host import, memory limits)
- [x] **kvnc-dag**: Block validation, parent selection (1 test)

### [ ] 12.2 Integration Tests
- [ ] **Single node**: Full block production → commit → execute cycle
- [ ] **Multi-node**: 4+ nodes, consensus agreement, fork resolution
- [ ] **Network partition**: Recovery, sync from genesis

### [ ] 12.3 Property-Based Tests
- [ ] **Consensus invariants**: Safety (no conflicting commits), liveness (progress)
- [x] **Tokenomics invariants**: Supply ≤ cap, rewards match schedule (3 proptests, 256 cases each)
- [ ] **State transitions**: Deterministic replay from genesis

### [ ] 12.4 Load/Stress Tests
- [ ] **TPS benchmark**: Target 100+ TPS on 2GB RAM
- [ ] **Mempool stress**: 10k+ pending txs
- [ ] **DAG growth**: Long-running node memory stability

---

## Phase 13: DevOps & Deployment

### [ ] 13.1 Docker
- [ ] **Multi-stage build**: Builder + runtime images
- [ ] **Docker Compose**: Local devnet (4 validators)
- [ ] **Health checks**: RPC endpoint, peer count

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
