# Parallel Implementation Plan — Phases 15.6, 20-21, 14/24

## Phase 15.6: MysticGhost 4-Node Soak
- [ ] Enable `use_mysticghost=true` in docker-compose.yml
- [ ] Run in-process partition tests (3+1, 2+2) with MysticGhost enabled
- [ ] 24h soak test with `MemoryMax=3G` under load
- [ ] Verify RSS < 3GB, identical commits across partitions

## Phase 20-21: Security & Observability
- [ ] **Prometheus `/metrics` endpoint** on RPC port
  - [ ] Add `prometheus-client` metrics to consensus, mempool, network, RPC
  - [ ] Expose `/metrics` route in RPC server
  - [ ] Metrics: block_height, committed_leader_height, peers, mempool_size, commit_latency, mergeset_size, RSS
- [ ] **Grafana dashboard JSON** in `ops/grafana/`
- [ ] **Alerting rules** YAML: peer_drop, sync_stall, commit_lag, high_memory
- [ ] **Fuzzing harnesses** (cargo fuzz)
  - [ ] `StatementBlock` deserialize
  - [ ] `process_block` / `process_vote`
  - [ ] mempool admission
- [ ] **RPC rate limits** + optional bearer token for write methods

## Phase 14/24: Genesis Ceremony & Public Testnet
- [ ] **Genesis ceremony CLI tool** (`kvnc-node genesis`)
  - [ ] Input: validators.json + allocations
  - [ ] Output: genesis block + staking state + genesis.json
  - [ ] Deterministic
- [ ] **Faucet service** (rate-limited, standalone Axum)
- [ ] **Seed nodes** (3+ DNS seeds)
- [ ] **Validator onboarding docs**
- [ ] **Explorer** (Phase 23, later)

---

## Execution Order (within parallel lanes)

### Lane 1: Observability (Phase 21) — START FIRST
1. Add metrics to consensus engine (committed_leader_height, round, commit_latency, mergeset_size)
2. Add metrics to mempool (size, fee_rate_distribution)
3. Add metrics to network (peer_count, bytes_in/out)
4. Add `/metrics` route to RPC server
5. Create Grafana dashboard JSON
6. Create Prometheus alerting rules

### Lane 2: Genesis Ceremony (Phase 14) — START SECOND
1. Extend `kvnc-node genesis` subcommand with full ceremony logic
2. Add faucet service binary
3. Update docker-compose with faucet + seed nodes

### Lane 3: Security (Phase 20) — START THIRD
1. Add cargo-fuzz harnesses
2. Add RPC rate limiting middleware
3. Add optional bearer token auth for write methods

### Lane 4: MysticGhost (Phase 15.6) — START FOURTH
1. Enable `use_mysticghost=true` in docker-compose
2. Run multi_node_integration with MysticGhost
3. 24h soak test

---

## Current Blockers
- None. All prerequisites met (4-node quorum operational).