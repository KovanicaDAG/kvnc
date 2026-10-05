# Kovanica (KVNC) – Lightweight DAG Blockchain

Rust implementation of a public permissionless blockchain.

- **Consensus**: Mysticeti-style uncertified DAG (wave length 3)
- **Smart Contracts**: Wasmi (deterministic WASM interpreter)
- **Token**: Native KVNC with emission schedule + treasury vesting
- **Validators**: Target 15–21 active
- **Resource target**: Comfortable on 2–4 GB RAM VPS

## Tokenomics (locked)

| Parameter                  | Value                                          |
|---------------------------|------------------------------------------------|
| **Total supply**          | **90 200 000 KVNC**                            |
| Decimals                  | 9                                              |
| Founder premine           | 200 000 KVNC                                   |
| Treasury                  | 8 000 000 KVNC (1 M/year × 8 years, linear)    |
| Mining subsidy budget     | ≈ 82 000 000 KVNC                              |
| Initial block reward      | **10 KVNC** (paid to committed leader author)  |
| Subsidy era length        | **2 050 000** committed leader blocks          |
| Decay                     | **× 3/4** each era                             |
| Active validators         | 15–21                                          |
| Min validator stake       | 50 000 KVNC                                    |

Full specification: [docs/TOKENOMICS.md](docs/TOKENOMICS.md)

## Reward binding to DAG

Block rewards are minted **only** when consensus finalizes a leader block:

```
CommittedSubDag (leader)
    → ExecutionContext::execute_committed_subdag()
        → StakingState::on_leader_committed(leader_author)
            → credit payout_address with block_reward(height)
            → advance treasury vesting
```

## Workspace Structure

```
kvnc/
├── Cargo.toml
├── README.md
├── docs/
│   ├── TOKENOMICS.md
│   └── ARCHITECTURE.md
└── crates/
    ├── kvnc-types
    ├── kvnc-crypto
    ├── kvnc-storage
    ├── kvnc-dag
    ├── kvnc-consensus
    ├── kvnc-mempool
    ├── kvnc-network
    ├── kvnc-runtime
    ├── kvnc-execution      ← reward application
    ├── kvnc-staking        ← emission + treasury vesting
    ├── kvnc-node
    ├── kvnc-cli
    └── kvnc-rpc
```

## Quick Start

```bash
cd kvnc
cargo build
cargo test -p kvnc-staking
```

## Design Decisions

| Decision              | Choice                                      |
|-----------------------|---------------------------------------------|
| Network type          | Public permissionless                       |
| Consensus             | DAG (Mysticeti-style)                       |
| Smart contracts       | Wasmi (deterministic)                       |
| Active validators     | 15–21                                       |
| Token supply model    | Emission schedule + premine + treasury      |
| Reward trigger        | Committed leader blocks only                |
| Target hardware       | Ordinary VPS 2–4 GB RAM                     |

## Documentation

- [TOKENOMICS.md](docs/TOKENOMICS.md) – supply, emission, treasury vesting, reward path
- [ARCHITECTURE.md](docs/ARCHITECTURE.md) – high-level component flow

## License

Apache-2.0
