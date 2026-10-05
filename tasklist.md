# KVNC Project Task List

## Overview
This task list tracks all remaining work to build a functional KVNC blockchain node. Items are organized by crate/layer with dependencies noted.

---

## Phase 0: Foundation & Tooling

### [ ] 0.1 Workspace Setup
- [ ] Verify `cargo build` works for all crates
- [ ] Add `cargo test` CI pipeline (GitHub Actions)
- [ ] Configure `clippy` and `rustfmt` checks
- [ ] Set up dependency auditing (`cargo audit`)

### [ ] 0.2 Documentation
- [ ] Add missing module-level docs to all crates
- [ ] Generate API docs with `cargo doc`
- [ ] Create architecture decision records (ADRs) for key choices

---

## Phase 1: Core Types & Crypto (kvnc-types, kvnc-crypto)

### [ ] 1.1 kvnc-types — Complete Data Structures
- [ ] **Block/Transaction serialization**: Proper bincode/postcard impl for `StatementBlock`, `Transaction`
- [ ] **Merkle roots**: Add `merkle_root` to blocks for tx inclusion proofs
- [ ] **Round/Authority types**: Ensure `Round`, `AuthorityIndex` have proper arithmetic traits
- [ ] **Committee types**: `Committee`, `Authority`, `Stake` with stake-weighted selection
- [ ] **Address derivation**: From public key (blake3 hash → address format)
- [ ] **Hash domain separation**: Distinct hash constructors for blocks, txs, state

### [ ] 1.2 kvnc-crypto — Production Ready
- [ ] **Keypair persistence**: Secure file storage (encrypted with passphrase)
- [ ] **Batch verification**: Ed25519 batch verify for block signatures
- [ ] **Key derivation**: BIP32-style HD wallet support (optional)
- [ ] **VRF**: Verifiable random function for leader selection (if needed)

---

## Phase 2: Storage Layer (kvnc-storage)

### [ ] 2.1 Block Store
- [ ] **Schema design**: redb tables for blocks, transactions, block index
- [ ] **BlockStore API**: `put_block`, `get_block`, `get_block_by_height`, `get_blocks_by_range`
- [ ] **Transaction index**: Map tx hash → block reference
- [ ] **Pruning policy**: Keep last N committed sub-DAGs + genesis

### [ ] 2.2 State Store
- [ ] **Account state**: Balance, nonce, contract code/storage
- [ ] **Staking state**: Validators, delegations, treasury (persist `StakingState`)
- [ ] **Merkle Patricia Trie**: For state root computation
- [ ] **Snapshot/Restore**: Periodic state snapshots for fast sync

### [ ] 2.3 Consensus Store
- [ ] **DAG persistence**: Parent/child links, round index, author index
- [ ] **Commit tracker**: Persisted `committed_leader_height`, decided rounds

---

## Phase 3: DAG Layer (kvnc-dag)

### [ ] 3.1 DagStore
- [ ] **Block ingestion**: Validate parents exist, verify signatures, check round
- [ ] **Causal ordering**: Topological sort, ancestor/descendant queries
- [ ] **Parent selection**: Algorithm for choosing 2f+1 parents from previous round
- [ ] **Garbage collection**: Prune blocks before last committed leader (configurable)

### [ ] 3.2 Block Manager
- [ ] **Propose block**: Build `StatementBlock` with transactions from mempool
- [ ] **Validate block**: Check parents, signature, round, transactions valid
- [ ] **Block broadcast**: Gossip new blocks to peers

---

## Phase 4: Consensus (kvnc-consensus)

### [ ] 4.1 Committer Implementation (CRITICAL PATH)
- [ ] **Direct commit rule**: Leader certified by 2f+1 votes in next round
- [ ] **Indirect commit rule**: Wave-based skip/commit through decided leaders
- [ ] **Leader status tracking**: `Undecided` → `Commit` / `Skip`
- [ ] **CommittedSubDag production**: Linearize blocks in topological order

### [ ] 4.2 Linearizer
- [ ] **Topological sort**: Blocks in causal order for execution
- [ ] **Deduplication**: Handle blocks reachable via multiple paths

### [ ] 4.3 Wave Logic
- [ ] **Wave advancement**: Track current wave, leader schedule
- [ ] **Leader selection**: Deterministic from committee + round (VRF or hash)
- [ ] **Timeout handling**: Skip leader if no block produced

### [ ] 4.4 Consensus Loop Integration
- [ ] **Round timer**: Advance rounds based on wall clock or block arrival
- [ ] **Fork handling**: Multiple leaders same round (first valid wins)

---

## Phase 5: Mempool (kvnc-mempool)

### [ ] 5.1 Core Mempool
- [ ] **Tx pool**: Priority queue by fee rate (fee/gas)
- [ ] **Admission control**: Validate nonce, balance, signature, gas limit
- [ ] **Eviction policy**: Drop lowest fee-rate when memory limit reached
- [ ] **Rebroadcast**: Periodic gossip of pending transactions

### [ ] 5.2 Block Building
- [ ] **Select transactions**: Fill block up to `MAX_TXS_PER_BLOCK` / gas limit
- [ ] **Conflict resolution**: Same sender nonce ordering
- [ ] **Fee estimation**: RPC endpoint for suggested fee rates

---

## Phase 6: Networking (kvnc-network)

### [ ] 6.1 libp2p Setup
- [ ] **Transport**: TCP + Noise + Yamux
- [ ] **Discovery**: Kademlia DHT + mDNS (local)
- [ ] **Gossipsub topics**: `blocks`, `transactions`, `votes`, `sync`

### [ ] 6.2 Protocols
- [ ] **Block sync**: Request/response for missing blocks (by round/author)
- [ ] **Transaction gossip**: Flood new txs to peers
- [ ] **Vote gossip**: Consensus votes (if separate from blocks)
- [ ] **Peer scoring**: Ban misbehaving peers (invalid blocks, spam)

### [ ] 6.3 Bootstrap
- [ ] **Seed nodes**: DNS seed (`seed.kovanica.online`) + hardcoded peers
- [ ] **Connection management**: Target peer count, reconnect logic

---

## Phase 7: Execution & Runtime (kvnc-execution, kvnc-runtime)

### [ ] 7.1 Native Transaction Execution
- [ ] **Transfer**: Debit sender, credit recipient, increment nonce
- [ ] **Stake/Unstake**: Update `StakingState`, handle unbonding queue
- [ ] **Deploy**: Store WASM code, assign contract address
- [ ] **Call**: Execute WASM with gas metering, handle host calls

### [ ] 7.2 WASM Runtime (kvnc-runtime)
- [ ] **Host functions**: `storage_get`, `storage_set`, `balance_of`, `transfer`, `call_contract`, `crypto_verify`, `block_height`, `timestamp`
- [ ] **Gas metering**: Fuel-based execution, configurable costs per opcode
- [ ] **Memory limits**: Enforce `memory_limit_pages`
- [ ] **Determinism**: No non-deterministic host functions
- [ ] **Module caching**: Compile once, instantiate many

### [ ] 7.3 Execution Context
- [ ] **State transitions**: Apply txs to account/contract state
- [ ] **Event logs**: Emit events for indexing
- [ ] **Receipts**: Transaction outcome (success/failure, gas used, logs)
- [ ] **State root**: Compute after each committed sub-DAG

---

## Phase 8: Staking & Tokenomics (kvnc-staking) — MOSTLY DONE

### [ ] 8.1 Remaining Items
- [ ] **Delegation logic**: Bond/unbond, reward sharing with commission
- [ ] **Validator rotation**: Committee change at epoch boundaries
- [ ] **Slashing**: Double-sign detection, stake slashing (future)
- [ ] **Governance hooks**: Parameter change proposals (future)

---

## Phase 9: Node Binary (kvnc-node)

### [ ] 9.1 Configuration
- [ ] **Config file**: TOML with all tunable parameters
- [ ] **Environment variables**: Override config for deployment
- [ ] **Genesis config**: Committee, treasury address, premine accounts

### [ ] 9.2 Core Loop
- [ ] **Initialize**: Load config, genesis, keystore, connect to peers
- [ ] **Consensus loop**: Propose blocks, process incoming, commit
- [ ] **Execution loop**: Process `CommittedSubDag` from consensus
- [ ] **RPC server**: Start JSON-RPC on configured port

### [ ] 9.3 Graceful Shutdown
- [ ] **Signal handling**: SIGTERM → flush state, close connections
- [ ] **State persistence**: Ensure all committed data on disk

---

## Phase 10: RPC API (kvnc-rpc)

### [ ] 10.1 JSON-RPC Methods (Standard + KVNC-specific)
- [ ] **Chain**: `kvnc_blockNumber`, `kvnc_getBlockByHash`, `kvnc_getBlockByNumber`
- [ ] **Transactions**: `kvnc_sendRawTransaction`, `kvnc_getTransactionReceipt`, `kvnc_getTransactionByHash`
- [ ] **Accounts**: `kvnc_getBalance`, `kvnc_getNonce`, `kvnc_getCode`, `kvnc_getStorageAt`
- [ ] **Staking**: `kvnc_getValidators`, `kvnc_getStake`, `kvnc_getRewards`
- [ ] **Mempool**: `kvnc_getPendingTransactions`, `kvnc_estimateFee`
- [ ] **Consensus**: `kvnc_getLeaderSchedule`, `kvnc_getCommittee`

### [ ] 10.2 WebSocket Support
- [ ] **Subscriptions**: `newHeads`, `logs`, `pendingTransactions`, `newCommittedLeader`

### [ ] 10.3 API Client (packages/api-client)
- [ ] **Generated TypeScript client**: From OpenAPI spec
- [ ] **React hooks**: For frontend integration

---

## Phase 11: CLI (kvnc-cli)

### [ ] 11.1 Wallet Operations
- [ ] **Keygen**: Generate + save encrypted keystore
- [ ] **Import/Export**: Private key, mnemonic
- [ ] **Sign**: Offline transaction signing

### [ ] 11.2 Node Operations
- [ ] **Status**: Sync status, peer count, latest block
- [ ] **Staking**: `stake`, `unstake`, `delegate`, `claim-rewards`
- [ ] **Governance**: `propose`, `vote` (future)

### [ ] 11.3 Output Formats
- [ ] **JSON output**: `--json` flag for all commands
- [ ] **Table output**: Human-readable default

---

## Phase 12: Testing & Verification

### [ ] 12.1 Unit Tests
- [ ] **kvnc-staking**: All tokenomics math (✓ mostly done)
- [ ] **kvnc-consensus**: Commit rules, linearizer
- [ ] **kvnc-execution**: Transaction application, reward distribution
- [ ] **kvnc-runtime**: WASM execution, gas metering
- [ ] **kvnc-dag**: Block validation, parent selection

### [ ] 12.2 Integration Tests
- [ ] **Single node**: Full block production → commit → execute cycle
- [ ] **Multi-node**: 4+ nodes, consensus agreement, fork resolution
- [ ] **Network partition**: Recovery, sync from genesis

### [ ] 12.3 Property-Based Tests
- [ ] **Consensus invariants**: Safety (no conflicting commits), liveness (progress)
- [ ] **Tokenomics invariants**: Supply ≤ cap, rewards match schedule
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