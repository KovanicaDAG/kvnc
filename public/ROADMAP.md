# KVNC Roadmap

**Kovanica (KVNC)** is a high-performance, deterministic DAG blockchain built in Rust.  
This roadmap describes what is done, what is in progress, and what comes next on the path from a working node to a public network.

| | |
|---|---|
| **Consensus** | Mysticeti-style uncertified DAG (wave length 3) + optional scoped GHOSTDAG k=3 (MysticGhost) |
| **Smart contracts** | Deterministic WASM (Wasmi) |
| **Native token** | KVNC (9 decimals) · max supply 90.2M |
| **Target hardware** | 2–4 GB RAM VPS |
| **Status** | Active development · local multi-node runnable · public testnet not yet launched |

---

## Vision

Ship a lean, auditable DAG chain that:

- Finalizes via a clear commit rule (not probabilistic longest-chain)
- Runs smart contracts with bit-for-bit deterministic execution
- Fits commodity hardware without sacrificing safety
- Keeps the validator and user surface small and explicit

We prioritise **correctness and determinism** over feature count. Throughput targets come after multi-validator quorum is proven.

---

## Where we are

### Done

| Area | What shipped |
|------|----------------|
| **Core protocol** | Types, crypto (Ed25519 + BLAKE3), redb storage, DAG store, block manager |
| **Consensus** | Wave committer, linearizer, leader schedule, property tests (safety) |
| **MysticGhost** | Optional GHOSTDAG k=3 colouring over committed mergesets (feature-flagged) |
| **Execution** | Native transfer / stake / deploy / call · Wasmi host · receipts · state roots |
| **Tokenomics** | Geometric emission, treasury vesting, live leader rewards |
| **Contracts** | HTLC, vault, multisig, fungible token — tests, RPC, CLI |
| **Node** | Full binary: config, genesis bootstrap, P2P, consensus loop, execution, RPC |
| **API** | JSON-RPC 2.0 (HTTP + WebSocket) · chain, tx, account, staking, mempool, contracts |
| **CLI** | Encrypted keystore, sign, contract subcommands |
| **Ops** | Multi-stage Docker · 4-node Compose · health endpoint |

### In progress / partial

- Production **committee** wiring from live staking state (not a single hardcoded authority)
- **Mempool admission** (nonce, balance, signature checks on every path)
- Consensus **timeout** and **fork** handling
- Long-run resource soak under a hard memory budget
- Stake **delegation**, **rotation**, and **slashing**

### Not started (public network)

- Genesis ceremony tooling and public testnet
- Block explorer, faucet, validator onboarding docs
- External security review and bug bounty
- Mainnet parameter freeze and launch

---

## Roadmap phases

Phases are sequential where safety depends on prior work. Parallel work is called out.

### Phase A — Production wiring *(current focus)*

Make a real multi-validator quorum work out of the box.

- Committee built from staking state (15–21 active validators)
- Block production rounds driven by consensus tip
- Votes and blocks verified on live networked nodes
- Mempool admission and nonce-ordered block building
- 4-node Docker quorum with identical commit sequences

**Success:** `docker compose up` yields four validators that agree on committed leaders under partition and recovery.

### Phase B — Consensus completeness

Close remaining consensus edge cases.

- Leader timeout → skip
- Deterministic fork choice when multiple leaders share a round
- Stake-weighted leader selection (replacing round-robin where appropriate)
- Batch signature verification and hash domain separation

**Success:** Full consensus suite green; no conflicting commits under adversarial vote injection.

### Phase C — Staking lifecycle

Validators and delegators as first-class economic actors.

- Bond / unbond / delegate with commission
- Epoch-bound committee rotation
- Unbonding period enforcement
- Double-sign slashing
- CLI: stake, unstake, delegate, claim rewards

**Success:** End-to-end stake → reward → unbond → withdraw; slash reduces stake on proven double-sign.

### Phase D — State, sync, and light clients

Make joining and verifying the network cheap.

- Canonical state commitment (Merkle structure)
- Snapshots and fast sync
- Robust missing-parent recovery over the existing block-sync protocol
- Optional light-client certificates for wave commits

**Success:** A fresh node reaches tip from snapshot + sync; a light client verifies a commit proof.

### Phase E — Security and hardening

- External review of crypto, consensus, and keystore paths
- Fuzzing of decode and ingest paths
- RPC rate limits and optional write authentication
- Multi-hour soak under ≤3 GB RSS
- Encrypted keystore as the only supported production key path

**Success:** Documented soak report, fuzz campaign without critical crashes, public security notes.

### Phase F — Observability

- Prometheus metrics (height, peers, mempool, commit latency, resource proxies)
- Grafana dashboards and basic alerts (peer drop, sync stall, memory)

**Success:** Operators can scrape `/metrics` and see commit rate and peer health.

### Phase G — Developer platform

- Typed API client (TypeScript)
- Minimal OpenAPI / schema for RPC
- Contract helpers (deploy / call)
- Dev faucet
- Event indexer skeleton (WebSocket → store)

**Success:** Third-party scripts can read balances and submit transfers against a local node.

### Phase H — Explorer and user surface

- Block explorer (blocks, transactions, accounts, validators)
- Simple wallet flows (send, stake, contract interaction)
- Public network status page

**Success:** A public URL shows live testnet activity.

### Phase I — Public testnet

- Genesis tool and documented ceremony
- Seed nodes, faucet, explorer, validator guide
- Onboarding path for independent validators

**Success:** External operators run validators and users obtain test KVNC from the faucet.

### Phase J — Mainnet

- Parameter freeze
- Audit / bounty outcomes addressed
- Launch checklist and incident runbook
- Mainnet genesis and monitoring

**Success:** Mainnet tip advances under production monitoring; rollback and upgrade policy published.

### Phase K — Governance *(post-mainnet)*

- Parameter proposals and stake-weighted signalling
- Treasury spend process
- Coordinated upgrade signalling

**Success:** At least one parameter change executed through the published process on testnet, then mainnet.

---

## Tokenomics (locked)

| Parameter | Value |
|-----------|--------|
| Max supply | 90.2M KVNC |
| Founder premine | 0.2M KVNC |
| Treasury (vested) | 8M KVNC (linear over 8 years) |
| Mining budget | ~82M KVNC |
| Initial subsidy | 10 KVNC / committed leader |
| Era length | 2,050,000 committed leaders |
| Decay | ×¾ per era |
| Decimals | 9 |
| Active validators | 15–21 |
| Min validator stake | 50,000 KVNC |

Emission and treasury math are implemented and covered by tests. Delegation and slashing economics complete in Phase C.

---

## Principles

1. **Safety before speed** — no throughput claims until multi-node commit agreement is routine.
2. **Determinism** — same inputs, same state root on every honest node.
3. **Small surface** — prefer one clear mechanism over three optional ones.
4. **Feature flags** — experimental consensus paths (e.g. MysticGhost) stay off by default until soak tests pass.
5. **Commodity hardware** — design and prune for 2–4 GB RAM class machines.
6. **Explicit upgrades** — storage and consensus changes are versioned and documented; no silent fork.

---

## How to follow along

- **Code:** [github.com/KovanicaDAG/kvnc](https://github.com/KovanicaDAG/kvnc)
- **Detailed engineering checklist:** `tasklist.md` in the repository
- **Local devnet:** `docker compose up --build` (see `ops/docker/`)
- **Build:** Rust 1.90+ · `cargo build --release --workspace`

Progress updates land as merged PRs and checklist updates in-repo. This roadmap will be revised when a phase completes or scope materially changes.

---

## Disclaimer

This document is a planning view, not a promise of delivery dates.  
Estimates shift with audit findings, consensus edge cases, and operational lessons from testnet.  
Nothing here is an offer of securities or a guarantee of mainnet timing.

---

*Last updated: 2026-10-08*
