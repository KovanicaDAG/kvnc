# KVNC Architecture Overview

## High-level flow

```
Network (P2P)
    │
    ▼
Mempool ──────────────────────────────────┐
    │                                     │
    ▼                                     │
DAG Block Producer (per validator)        │
    │                                     │
    ▼                                     │
Consensus (Mysticeti-style DAG)           │
    │  produces CommittedSubDag           │
    ▼                                     │
Execution Layer ◄─────────────────────────┘
    │  - apply native txs
    │  - execute WASM contracts (Wasmi)
    │  - pay leader block reward
    │  - advance treasury vesting
    ▼
State (accounts, staking, contracts)
    │
    ▼
Storage (redb / pruning / snapshots)
```

## Reward path (detail)

1. `UniversalCommitter` decides a leader → `Linearizer` produces `CommittedSubDag`
2. Node calls `ExecutionContext::execute_committed_subdag(&subdag)`
3. Inside execution:
   - transactions are applied
   - `staking.on_leader_committed(subdag.leader_author)` is called
   - reward amount = `block_reward(committed_leader_height)`
   - balance of the leader’s payout address is credited
   - treasury vesting is advanced

## Key crates

| Crate            | Responsibility                                      |
|------------------|-----------------------------------------------------|
| kvnc-types       | Core data structures                                |
| kvnc-crypto      | Signatures, hashing                                 |
| kvnc-dag         | Causal DAG store                                    |
| kvnc-consensus   | Commit rules, linearizer                            |
| kvnc-staking     | Emission, treasury, validator set                   |
| kvnc-runtime     | Wasmi WASM engine                                   |
| kvnc-execution   | State transition + reward distribution              |
| kvnc-node        | Binary that wires everything together               |

## Resource targets

- Full node comfortable on **2–4 GB RAM** VPS
- Aggressive pruning of committed DAG history
- Wasmi chosen for deterministic, low-footprint contract execution
