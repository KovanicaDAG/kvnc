<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/KovanicaDAG/kvnc/main/KVNCa-logo.JPG" width="180">
    <source media="(prefers-color-scheme: light)" srcset="https://raw.githubusercontent.com/KovanicaDAG/kvnc/main/KVNCa-logo.JPG" width="180">
    <img alt="KVNC Logo" src="https://raw.githubusercontent.com/KovanicaDAG/kvnc/main/KVNCa-logo.JPG" width="180">
  </picture>
</p>

<h1 align="center">KVNC (Kovanica)</h1>

<p align="center">
  <strong>A high-performance, deterministic DAG blockchain built in Rust</strong>
</p>

<p align="center">
  <a href="https://github.com/kovanica/kvnc/actions/workflows/ci.yml"><img alt="Build Status" src="https://img.shields.io/github/actions/workflow/status/kovanica/kvnc/ci.yml?branch=main&style=flat-square"></a>
  <a href="https://github.com/kovanica/kvnc/blob/main/LICENSE"><img alt="License" src="https://img.shields.io/github/license/kovanica/kvnc?style=flat-square"></a>
  <a href="https://www.rust-lang.org/"><img alt="Rust Version" src="https://img.shields.io/badge/rust-1.85+-orange?style=flat-square&logo=rust"></a>
  <a href="https://crates.io/crates/kvnc-types"><img alt="Crates.io" src="https://img.shields.io/crates/v/kvnc-types?style=flat-square"></a>
  <a href="https://discord.gg/kovanica"><img alt="Discord" src="https://img.shields.io/discord/123456789?label=discord&style=flat-square&logo=discord&color=5865F2"></a>
  <a href="https://docs.kovanica.online"><img alt="Docs" src="https://img.shields.io/badge/docs-online-blue?style=flat-square&logo=readthedocs"></a>
</p>

---

## Overview

**KVNC (Kovanica)** is a next-generation blockchain protocol implementing a **Mysticeti-style uncertified DAG consensus** with deterministic WASM smart contracts. Designed for high throughput and low resource consumption, KVNC runs comfortably on commodity VPS hardware (2–4 GB RAM) while maintaining strong decentralization guarantees.

| Property | Value |
|----------|-------|
| **Consensus** | Mysticeti-style uncertified DAG (wave length 3) |
| **Smart Contracts** | Wasmi (deterministic WASM interpreter) |
| **Native Token** | KVNC (9 decimals) |
| **Active Validators** | 15–21 (configurable) |
| **Target Hardware** | 2–4 GB RAM VPS |
| **P2P Transport** | libp2p (TCP/TLS, Noise, Yamux, GossipSub) |
| **RPC** | JSON-RPC 2.0 over HTTP + WebSocket |

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              KVNC Node                                      │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐    ┌──────────┐  │
│  │   JSON-RPC   │    │   Network    │    │  Mempool     │    │ Consensus│  │
│  │   Server     │◄───│   Service    │───►│  (kvnc-      │───►│  Engine  │  │
│  │  (kvnc-rpc)  │    │ (kvnc-netwk) │    │  mempool)    │    │(kvnc-    │  │
│  └──────────────┘    └──────────────┘    └──────────────┘    │ consensus)│  │
│         ▲                   ▲                    │            └─────┬─────┘  │
│         │                   │                    │                  │        │
│         ▼                   ▼                    ▼                  ▼        │
│  ┌──────────────────────────────────────────────────────────────────────┐   │
│  │                        Storage Layer (redb)                           │   │
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌────────────┐  │   │
│  │  │ DagStore    │  │ StateStore  │  │ BlockStore  │  │Consensus   │  │   │
│  │  │ (kvnc-dag)  │  │ (kvnc-stor) │  │ (kvnc-dag)  │  │Store       │  │   │
│  │  └─────────────┘  └─────────────┘  └─────────────┘  └────────────┘  │   │
│  └──────────────────────────────────────────────────────────────────────┘   │
│                                    │                                        │
│                                    ▼                                        │
│  ┌──────────────────────────────────────────────────────────────────────┐   │
│  │                      Execution Layer                                  │   │
│  │  ┌────────────┐  ┌──────────┐  ┌────────┐  ┌────────┐  ┌────────┐  │   │
│  │  │ Execution  │  │ HTLC     │  │ Vault  │  │Multisig│  │ Token  │  │   │
│  │  │ Context    │  │ (kvnc-   │  │(kvnc-  │  │(kvnc-  │  │(kvnc-  │  │   │
│  │  │ (kvnc-exec)│  │ htlc)    │  │ vault) │  │multisig)│  │ token) │  │   │
│  │  └────────────┘  └──────────┘  └────────┘  └────────┘  └────────┘  │   │
│  │         │                                                     │       │   │
│  │         ▼                                                     ▼       │   │
│  │  ┌────────────────────────────────────────────────────────────────┐   │   │
│  │  │              Wasmi Runtime (deterministic WASM)                │   │   │
│  │  │  kvnc-contracts  │  kvnc-common  │  kvnc-types  │  kvnc-crypto│   │   │
│  │  └────────────────────────────────────────────────────────────────┘   │   │
│  └──────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Crate Structure (22 Crates)

| Crate | Purpose |
|-------|---------|
| `kvnc-types` | Core types: blocks, transactions, addresses, rounds, committee, crypto primitives |
| `kvnc-crypto` | Ed25519 (dalek), BLAKE3 hashing, key generation, signatures |
| `kvnc-storage` | redb-backed storage: `DagStore`, `StateStore`, `BlockStore`, consensus indices |
| `kvnc-dag` | DAG block management, parent selection, block proposals, commitment tracking |
| `kvnc-consensus` | Mysticeti engine: wave-based commit, leader election, linearizer, committers |
| `kvnc-mempool` | Transaction pool with prioritization, deduplication, gossip integration |
| `kvnc-network` | libp2p service: block/tx gossip, peer discovery, sync protocol |
| `kvnc-runtime` | Wasmi host, contract dispatcher, gas metering, syscall interface |
| `kvnc-execution` | Execution context, state transitions, event emission, contract calls |
| `kvnc-staking` | Validator set, delegation, emission schedule, treasury vesting |
| `kvnc-node` | Full node binary: wiring, config, lifecycle, task orchestration |
| `kvnc-cli` | Command-line interface: wallet, staking, node management |
| `kvnc-rpc` | JSON-RPC 2.0 server (HTTP + WS), method registry, error handling |
| `kvnc-common` | Shared: `Host` trait, `WasmHost`, `MemHost`, events, error codes |
| `kvnc-htlc` | Hashed Time-Lock Contracts (atomic swaps) — native balance escrow |
| `kvnc-vault` | Time-lock / vesting vaults — linear, cliff, absolute schedules |
| `kvnc-multisig` | M-of-N multisignature wallets — propose/confirm/execute flow |
| `kvnc-token` | Standardized fungible token (ERC-20-like) — mint/burn/transfer/approve |
| `kvnc-contracts` | Aggregator crate re-exporting all built-in contract entry points |

---

## Tokenomics (Locked)

All values are canonical and cross-checked against the reference skeleton implementation.

| Parameter | Value | Notes |
|-----------|-------|-------|
| **Total Supply** | 90,200,000 KVNC | Hard cap: `90_200_000 * 10^9` atoms |
| **Founder Premine** | 200,000 KVNC | Allocated at genesis |
| **Treasury** | 8,000,000 KVNC | 1M KVNC/year × 8 years, linear vesting |
| **Mining Subsidy Budget** | ~82,000,000 KVNC | `TOTAL_SUPPLY - PREMINE - TREASURY` |
| **Initial Block Reward** | 10 KVNC | Per committed leader block |
| **Subsidy Era Length** | 2,050,000 committed leaders | ~13 months at 2s block time |
| **Decay Factor** | ×3/4 per era | Geometric decay |
| **Decimals** | 9 | 1 KVNC = 1,000,000,000 atoms |
| **Min Validator Stake** | 50,000 KVNC | Entry threshold |
| **Active Validator Range** | 15–21 | `MIN_ACTIVE_VALIDATORS` / `MAX_ACTIVE_VALIDATORS` |
| **Unbonding Period** | 100,000 rounds | ~2.3 days at 2s rounds |

### Emission Schedule

```text
Era 0 (heights 0 – 2,049,999)      : 10.000 KVNC / block
Era 1 (heights 2,050,000 – 4,099,999):  7.500 KVNC / block  (×3/4)
Era 2 (heights 4,100,000 – 6,149,999):  5.625 KVNC / block  (×3/4)
Era 3 (heights 6,150,000 – 8,199,999):  4.219 KVNC / block  (×3/4)
...
```

Cumulative mining issuance approaches **82M KVNC** asymptotically. The treasury vests **1M KVNC/year** linearly over 8 years based on committed-leader height (using `BLOCKS_PER_YEAR = 15,768,000` ≈ 2s blocks).

---

## Quick Start

### Prerequisites

- **Rust 1.85+** (install via [rustup](https://rustup.rs/))
- **Linux/macOS/Windows** (WSL2 recommended on Windows)

### Build from Source

```bash
# Clone the repository
git clone https://github.com/kovanica/kvnc.git
cd kvnc

# Build all crates (release profile with LTO)
cargo build --release --workspace

# Verify the build
./target/release/kvnc-node --help
./target/release/kvnc-cli --help
```

### Run a Development Node

```bash
# Generate a config file
./target/release/kvnc-node --generate-config > config.toml

# Edit config.toml as needed (bootnodes, RPC port, etc.)
# For local testing, defaults work out of the box

# Start the node
./target/release/kvnc-node --config config.toml
```

The node will:
1. Create the data directory (`./data` by default)
2. Initialize genesis state (treasury, validator set)
3. Start P2P networking on `0.0.0.0:9000`
4. Start JSON-RPC on `127.0.0.1:8545`
5. Begin consensus participation

### Docker (Coming Soon)

```bash
# Pull and run
docker run -d \
  -p 8545:8545 \
  -p 9000:9000 \
  -v kvnc-data:/data \
  ghcr.io/kovanica/kvnc-node:latest
```

---

## JSON-RPC API Reference

KVNC implements **JSON-RPC 2.0** over HTTP (WebSocket support planned). All endpoints are served at `/rpc`.

### Chain Methods

| Method | Description | Params | Returns |
|--------|-------------|--------|---------|
| `kvnc_blockNumber` | Latest committed leader height | `[]` | `Quantity` |
| `kvnc_getBlockByHash` | Get block by hash | `[Hash, Boolean]` | `Block` |
| `kvnc_getBlockByNumber` | Get block by height | `[Quantity, Boolean]` | `Block` |

### Transaction Methods

| Method | Description | Params | Returns |
|--------|-------------|--------|---------|
| `kvnc_sendRawTransaction` | Submit signed transaction | `[HexString]` | `Hash` |
| `kvnc_getTransactionReceipt` | Get transaction receipt | `[Hash]` | `TxReceipt` |
| `kvnc_getTransactionByHash` | Get transaction by hash | `[Hash]` | `Transaction` |

### Account Methods

| Method | Description | Params | Returns |
|--------|-------------|--------|---------|
| `kvnc_getBalance` | Native KVNC balance | `[Address]` | `Quantity` |
| `kvnc_getNonce` | Account nonce | `[Address]` | `Quantity` |
| `kvnc_getCode` | Contract bytecode | `[Address]` | `HexString` |
| `kvnc_getStorageAt` | Contract storage slot | `[Address, Quantity]` | `HexString` |

### Contract Methods (Built-in)

#### HTLC (Atomic Swaps)

```bash
# Create HTLC
curl -X POST http://localhost:8545/rpc \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "htlc_create",
    "params": {
      "claimer": "0x1234...",
      "amount": "1000000000",
      "hash_lock": "0xabcdef...",
      "expiry": 1700000000
    },
    "id": 1
  }'

# Claim HTLC (with preimage)
curl -X POST http://localhost:8545/rpc \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "htlc_claim",
    "params": {
      "id": "0xabcdef...",
      "preimage": "0xdeadbeef..."
    },
    "id": 2
  }'

# Refund HTLC (after expiry)
curl -X POST http://localhost:8545/rpc \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "htlc_refund",
    "params": { "id": "0xabcdef..." },
    "id": 3
  }'
```

| Method | Params | Returns |
|--------|--------|---------|
| `htlc_create` | `{claimer, amount, hash_lock, expiry}` | `{swap_id}` |
| `htlc_claim` | `{id, preimage}` | `{ok: true}` |
| `htlc_refund` | `{id}` | `{ok: true}` |

#### Vault (Time-lock / Vesting)

| Method | Params | Returns |
|--------|--------|---------|
| `vault_create` | `{beneficiary, amount, schedule}` | `{vault_id}` |
| `vault_claim` | `{id}` | `{amount_claimed}` |
| `vault_cancel` | `{id}` | `{ok: true}` |

**Schedule formats:**
```json
// Absolute unlock
{ "Absolute": { "unlock_at": 1700000000 } }

// Linear vesting
{ "Linear": { "start": 1700000000, "end": 1800000000, "cliff": 1705000000 } }
```

#### Multisig (M-of-N)

| Method | Params | Returns |
|--------|--------|---------|
| `multisig_create` | `{owners: [Address], threshold: u32}` | `{multisig_id}` |
| `multisig_propose` | `{multisig_id, to, amount, data}` | `{tx_id}` |
| `multisig_confirm` | `{id, tx_id}` | `{ok: true}` |
| `multisig_execute` | `{id, tx_id}` | `{ok: true}` |

#### Token (ERC-20-like)

| Method | Params | Returns |
|--------|--------|---------|
| `token_create` | `{name, symbol, decimals, initial_supply}` | `{contract_address}` |
| `token_transfer` | `{to, amount}` | `{ok: true}` |
| `token_approve` | `{spender, amount}` | `{ok: true}` |
| `token_transfer_from` | `{from, to, amount}` | `{ok: true}` |
| `token_mint` | `{to, amount}` | `{ok: true}` |
| `token_burn` | `{amount}` | `{ok: true}` |
| `token_balance` | `{address}` | `{amount}` |

### Staking Methods

| Method | Description | Params | Returns |
|--------|-------------|--------|---------|
| `kvnc_getValidators` | Active validator set | `[]` | `[ValidatorInfo]` |
| `kvnc_getStake` | Stake for address | `[Address]` | `Quantity` |
| `kvnc_getRewards` | Pending rewards | `[Address]` | `Quantity` |

### Mempool Methods

| Method | Description | Params | Returns |
|--------|-------------|--------|---------|
| `kvnc_getPendingTransactions` | Pending transaction hashes | `[]` | `[Hash]` |
| `kvnc_estimateFee` | Estimated fee for next block | `[]` | `Quantity` |

### Consensus Methods

| Method | Description | Params | Returns |
|--------|-------------|--------|---------|
| `kvnc_getLeaderSchedule` | Upcoming leader schedule | `[Quantity]` | `[LeaderInfo]` |
| `kvnc_getCommittee` | Current committee info | `[]` | `CommitteeInfo` |

---

## Configuration

The node is configured via TOML (`config.toml`) with environment variable overrides.

```toml
# config.toml
[data_dir]
path = "./data"

[network]
listen_addr = "0.0.0.0:9000"
bootnodes = [
  "/dns4/seed1.kovanica.online/tcp/9000",
  "/dns4/seed2.kovanica.online/tcp/9000"
]
max_peers = 50

[rpc]
addr = "127.0.0.1"
port = 8545

[consensus]
round_duration_ms = 2000

[validator]
key_path = "./validator.key"  # 32-byte hex seed
```

**Environment overrides:** `KVNC_DATA_DIR`, `KVNC_LISTEN_ADDR`, `KVNC_RPC_ADDR`, `KVNC_RPC_PORT`, `KVNC_ROUND_DURATION_MS`, `KVNC_VALIDATOR_KEY`.

---

## Development

### Running Tests

```bash
# All tests (unit + integration)
cargo test --workspace

# Specific crate
cargo test -p kvnc-consensus
cargo test -p kvnc-staking

# With output
cargo test --workspace -- --nocapture
```

### Code Quality

```bash
# Format
cargo fmt --all --check

# Lint
cargo clippy --workspace -- -D warnings

# Check without building
cargo check --workspace
```

### Project Structure

```
kvnc/
├── Cargo.toml                 # Workspace root
├── crates/
│   ├── kvnc-types/            # Core types
│   ├── kvnc-crypto/           # Cryptography
│   ├── kvnc-storage/          # redb storage layer
│   ├── kvnc-dag/              # DAG block management
│   ├── kvnc-consensus/        # Mysticeti consensus engine
│   ├── kvnc-mempool/          # Transaction pool
│   ├── kvnc-network/          # libp2p networking
│   ├── kvnc-runtime/          # Wasmi runtime
│   ├── kvnc-execution/        # Execution context
│   ├── kvnc-staking/          # Staking + tokenomics
│   ├── kvnc-node/             # Full node binary
│   ├── kvnc-cli/              # CLI wallet/tools
│   ├── kvnc-rpc/              # JSON-RPC server
│   ├── kvnc-common/           # Shared traits & hosts
│   ├── kvnc-htlc/             # HTLC contracts
│   ├── kvnc-vault/            # Vault contracts
│   ├── kvnc-multisig/         # Multisig contracts
│   ├── kvnc-token/            # Token contracts
│   └── kvnc-contracts/        # Contract aggregator
├── kvnc-next-steps/           # Experimental / WIP
└── docs/                      # Documentation
```

---

## Consensus Deep Dive

KVNC uses a **Mysticeti-style uncertified DAG** consensus with **wave length = 3**.

### Wave Structure

Each wave consists of 3 rounds:
- **Round 0 (Leader)**: Authorities propose blocks
- **Round 1 (Vote)**: Authorities vote on leader blocks
- **Round 2 (Decide)**: Commitment certificates formed

### Commit Rules

- **Direct Commit**: A leader block with ≥ 2f+1 votes in its wave
- **Indirect Commit**: A leader block becomes committed when a later leader block (in a subsequent wave) directly commits and references it

### Validator Set

- **Target**: 15–21 active validators
- **Stake-weighted**: Selection proportional to stake
- **Committee rotation**: At epoch boundaries (configurable)

---

## Smart Contract Platform

### Wasmi Runtime

KVNC uses **Wasmi** (not Wasmtime) for deterministic WASM execution:
- **No JIT** — pure interpreter, identical results across architectures
- **Gas metering** — per-instruction cost accounting
- **Fuel limit** — hard cap per transaction
- **Host functions** — storage, crypto, balance transfers, events

### Contract Model

- **Account-based** with contract addresses derived from deployment tx
- **Storage** namespaced per contract (`kvnc/v1/<contract>/*`)
- **Entry points** called via bincode-encoded tuples
- **Events** emitted via host, indexed off-chain

### Built-in Contracts

| Contract | Type | Use Case |
|----------|------|----------|
| HTLC | Native escrow | Atomic cross-chain swaps |
| Vault | Native escrow | Time-lock, vesting, cliffs |
| Multisig | Native | M-of-N treasury management |
| Token | Storage-only | ERC-20-like fungible assets |

---

## Running a Validator

1. **Generate a validator key** (32-byte hex seed):
   ```bash
   cargo run --release -p kvnc-cli -- key generate > validator.key
   chmod 600 validator.key
   ```

2. **Fund the validator address** with ≥ 50,000 KVNC (minimum stake)

3. **Configure `config.toml`**:
   ```toml
   [validator]
   key_path = "./validator.key"
   ```

4. **Join the validator set** via governance or genesis ceremony

5. **Monitor** via RPC: `kvnc_getValidators`, `kvnc_getStake`, `kvnc_getRewards`

---

## Links

| Resource | URL |
|----------|-----|
| **Documentation** | https://docs.kovanica.online |
| **Explorer (Testnet)** | https://testnet.kovanica.online |
| **Explorer (Mainnet)** | https://mainnet.kovanica.online |
| **API Reference** | https://docs.kovanica.online/api |
| **Discord** | https://discord.gg/kovanica |
| **Twitter/X** | https://x.com/kovanica |
| **GitHub** | https://github.com/kovanica/kvnc |

---

## License

KVNC is dual-licensed under **MIT OR Apache-2.0** at your option.

- [LICENSE-MIT](LICENSE-MIT)
- [LICENSE-APACHE](LICENSE-APACHE)

---

## Contributing

We welcome contributions! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

**TL;DR:**
1. Fork the repo
2. Create a feature branch (`git checkout -b feat/amazing-feature`)
3. Run `cargo fmt --all && cargo clippy --workspace`
4. Add tests for new functionality
5. Submit a PR with a clear description

---

<p align="center">
  <sub>Built with ❤️ by the KVNC Team and contributors</sub>
</p>