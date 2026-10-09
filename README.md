# KVNC

> A DAG-based Layer 1 blockchain written in Rust, with staking, smart contracts and a fixed, era-based emission schedule.

**Status: pre-alpha. Not audited. Not for production use. Interfaces and consensus parameters may change without notice.**

<!-- TODO: add logo from brand-assets/ -->
<!-- TODO: badges (CI, license) once CI is green -->

## Overview

KVNC (Kovanica) is a Rust workspace implementing a DAG ledger node. [TODO: 2-3 sentences: what problem it solves, how it differs from other chains, what the consensus approach is.]

## Features

- **DAG ledger**: [TODO: one line on ordering/finality]
- **Proof-of-stake validators**: staking with batch signature verification and an unbonding queue
- **Contract execution**: `kvnc-execution` with persistent contract storage
- **Verifiable state**: sorted key-value Merkle state root, with snapshot support
- **P2P networking**: `kvnc-network`
- **Typed API client**: `packages/api-client`

## Repository layout

| Path | Purpose |
| --- | --- |
| `crates/` | Core Rust crates (staking, storage, execution, network, ...) |
| `node/` | Node binary and integration tests |
| `packages/api-client/` | Client library for the node API |
| `docs/` | Documentation |
| `ops/` | Deployment and operations |
| `brand-assets/` | Logos and brand material |
| `config.example.toml` | Example node configuration |

## Quick start

### Requirements

- Rust (stable) and Cargo
- Docker and Docker Compose (optional)

### Build and test

```bash
git clone https://github.com/KovanicaDAG/kvnc.git
cd kvnc
cargo build --workspace
cargo test --workspace
```

### Run a node

```bash
cp config.example.toml config.toml
# edit config.toml, then:
cargo run -p kvnc-node -- --config config.toml
```

Or with Docker:

```bash
docker compose up --build
```

<!-- TODO: verify the exact binary name and CLI flags -->

## Tokenomics

| Parameter | Value |
| --- | --- |
| Maximum supply | 90,200,000 KVNC |
| Founder premine | 200,000 KVNC |
| Treasury | 8,000,000 KVNC |
| Initial block reward | 10 KVNC |
| Era length | 2,050,000 blocks |
| Reward decay | x 3/4 per era |
| Emitted through rewards | ~82,000,000 KVNC |

Emission per era is `era length x current reward`. Because the reward shrinks by a factor of 3/4 each era, total emission converges to 82M, and together with premine and treasury this gives the 90.2M cap.

## Roadmap

See [`TASKLIST.md`](TASKLIST.md) for detailed progress.

- [x] Staking with batch verification
- [x] State root and snapshots
- [x] Genesis initialization (premine and validator set)
- [ ] Multi-node integration test passing
- [ ] Public testnet
- [ ] Security audit
- [ ] Mainnet

## Development

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Experimental features are behind config flags and are **off by default**.

## Adresni format (kanonski)

`Address` je **raw 32-bajtni Ed25519 public key** (obavezno, jer `verify_signature`
koristi `sender` kao public key). Ljudski čitljiv zapis je:

```
kvnc <hex(32B payload ‖ 4B BLAKE3 checksum)> dag
```

Primjeri (checksum se računa nad payloadom):

```
kvnc111111111111111111111111111111111111111111111111111111111111111191f47563dag
kvnc09090909090909090909090909090909090909090909090909090909090909097a861baddag
```

- Prefix `kvnc`, suffix `dag`; tijelo je lowercase hex (64 hex payload + 8 hex checksum).
- `Display` emitira kanonski zapis; `FromStr`/`decode` verificira checksum i **prihvaća i**
  stari bare/`0x` 64-hex zapis (backward compat za genesis, keystoreove i RPC).
- CLI (`keygen`, `import`, `sign`, `transfer`, `balance`) i RPC odgovori
  (`sender`, `to`, `contract`, validator/committee adrese) koriste kanonski zapis.

### Primjer keystore izlaza (v2)

```json
{
  "version": 2,
  "address": "kvnc111111111111111111111111111111111111111111111111111111111111111191f47563dag",
  "public_key": "1111111111111111111111111111111111111111111111111111111111111111",
  "encrypted": true,
  "secret_key": "<96 hex chars: 48-byte XChaCha20-Poly1305 ciphertext>",
  "cipher": "xchacha20poly1305",
  "kdf": "argon2id",
  "kdf_memory_kib": 65536,
  "kdf_iterations": 3,
  "kdf_parallelism": 4,
  "salt": "<32 hex chars>",
  "nonce": "<48 hex chars>"
}
```

> Detalji keystore v2 formata i v1 migracije: [`docs/adr/0002-cli-wallet-keystore-v2.md`](docs/adr/0002-cli-wallet-keystore-v2.md).

## Contributing

Issues and pull requests are welcome. Please open an issue before large changes. [TODO: add CONTRIBUTING.md]

## Security

Please do not report vulnerabilities in public issues. See the repository's security policy for how to report them privately.

## License

[TODO: choose a license and add a LICENSE file]
