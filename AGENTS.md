# kvnc – OpenCode Agent Instructions

> **Project identity (NE MJEŠAJ S DRUGIM PROJEKTIMA)**

**kvnc** is a clean, from-scratch Rust Layer-1 blockchain.

| Layer              | Choice                                      |
|--------------------|---------------------------------------------|
| Consensus          | Mysticeti-style uncertified DAG (wave = 3)  |
| Execution          | Wasmi (deterministic WASM)                  |
| Account model      | Account-based + staked validators           |
| Validators         | 15–21 active, min stake 50 000 KVNC         |
| Resource target    | Comfortable on 2–4 GB RAM VPS               |
| Native token       | KVNC (9 decimals)                           |

## HARD RULES (NEKRŠIVA)

1. **NE** uvozi, kopiraj, referenciraj ili miješaj bilo kakav kod/dizajn iz `kovanica-protocol`, starijih Kovanica repozitorija ili bilo kojeg drugog lanca.
2. Ovo je **nezavisan** codebase. Sve odluke moraju biti konzistentne s ovim dokumentom i postojećim crate-ovima.
3. Preferiraj **male, fokusirane, auditabilne** promjene. Jedan PR = jedna jasna namjera.
4. Ne diraj tokenomics parametre bez eksplicitnog odobrenja.
5. Ciljaj uvijek na 2–4 GB RAM footprint.

## Tokenomics (zaključano)

| Parameter                  | Value                                          |
|---------------------------|------------------------------------------------|
| Total supply              | **90 200 000 KVNC**                            |
| Decimals                  | 9                                              |
| Founder premine           | 200 000 KVNC                                   |
| Treasury                  | 8 000 000 KVNC (1 M/year × 8 years, linear)    |
| Mining subsidy budget     | ≈ 82 000 000 KVNC                              |
| Initial block reward      | **10 KVNC** (paid to committed leader author)  |
| Subsidy era length        | **2 050 000** committed leader blocks          |
| Decay                     | **× 3/4** each era                             |
| Active validators         | 15–21                                          |
| Min validator stake       | 50 000 KVNC                                    |

Reward se mint-a **samo** na committed leader (CommittedSubDag → StakingState::on_leader_committed).

## Workspace structure (poštuj postojeće)

```
crates/
├── kvnc-types
├── kvnc-crypto
├── kvnc-storage
├── kvnc-dag
├── kvnc-consensus
├── kvnc-mempool
├── kvnc-network
├── kvnc-runtime          # Wasmi
├── kvnc-execution
├── kvnc-staking
├── kvnc-node
├── kvnc-cli
└── kvnc-rpc
```

Contracts (već implementirani): HTLC, Vault, Multisig, Token + Host trait.

## Coding standards

- Rust 2021 / 2024 edition po Cargo.toml
- Prefer `thiserror` + `anyhow` gdje ima smisla
- Determinističko ponašanje svugdje (Wasmi, consensus, rewards)
- Unit + integration testovi za svaku novu logiku
- Ne dodavaj nepotrebne dependencies
- Dokumentiraj javne API-je (rustdoc)
- Imena: snake_case, jasna, bez prefiksa "kovanica"

## Preferred next focus areas (prioritet)

A. Network & Node hardening (P2P, sync, snapshot/pruning, genesis)
B. Staking & Validator lifecycle (bond/unbond, active set, commission)
C. Execution & Contract UX (gas metering, ABI, deployment)
D. Observability (logging, metrics, indexer hooks)
E. Testnet readiness (multi-node, faucet, chaos)

## Kako raditi s Grok-om

- Grok = arhitekt + reviewer + planer
- OpenCode (ti) = implementator (file edit + shell + testovi)
- Kad dobiješ plan od Grok-a → izvrši ga u ovom sessionu
- Nakon implementacije vrati diff / test output za review

## Zabranjeno

- Miješanje s kovanica-protocol
- Promjena tokenomics bez odobrenja
- Veliki refaktori bez plana
- Dodavanje novih crate-ova bez opravdanja
- Non-deterministički kod u consensus/execution putu
