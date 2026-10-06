# Agent Prompt: Integrate kvnc Contract & Tokenomics Skeleton

You are working inside the **kvnc** repository root.

A clean, independent skeleton has been uploaded to the project root (or into a folder called `kvnc-skeleton/`).  
It contains only the following new pieces and has **zero dependency** on any other project (especially not kovanica-protocol).

## Skeleton contents

```
kvnc-skeleton/   (or the files may already be at root)
├── common/                  # shared types + Host trait
├── contracts/
│   ├── htlc/                # Hashed Time-Lock Contract
│   ├── vault/               # Time-lock + linear vesting vault
│   ├── multisig/            # M-of-N multisig
│   └── token/               # Standardized fungible token
└── staking/
    └── src/lib.rs           # Tokenomics constants + pure functions (aligned numbers)
```

## Your task

Integrate this skeleton cleanly into the existing kvnc codebase. Follow these rules strictly:

### 1. Do NOT mix with any other project
- Never import, copy, or reference anything from kovanica-protocol or any other external Kovanica repo.
- Keep all new code self-contained inside kvnc.

### 2. Placement
Preferred structure after integration:

```
kvnc/
├── crates/
│   ├── kvnc-common/         ← from skeleton/common
│   ├── kvnc-htlc/           ← from skeleton/contracts/htlc
│   ├── kvnc-vault/          ← from skeleton/contracts/vault
│   ├── kvnc-multisig/       ← from skeleton/contracts/multisig
│   ├── kvnc-token/          ← from skeleton/contracts/token
│   ├── kvnc-staking/        ← existing crate – merge tokenomics into it
│   ├── kvnc-execution/      ← existing – call on_leader_committed here
│   └── ... (other existing crates)
```

- Move the skeleton crates into `crates/` (or the equivalent workspace members location).
- Update the root `Cargo.toml` workspace members list.
- Fix all relative paths in the new `Cargo.toml` files.

### 3. Host trait
The contracts depend on a `Host` trait (defined in `kvnc-common`).

- Implement this trait for the real Wasmi / execution environment that already exists in kvnc.
- Required methods: `caller`, `block_height`, `timestamp`, `balance_of`, `transfer`, `emit_event`, `storage_get`, `storage_set`.
- If the existing runtime already has equivalent host functions, adapt the trait implementation to them. Do not invent a second host system.

### 4. Storage
All contracts currently have placeholder `store` / `load` methods.

- Replace them with real persistent storage using whatever storage abstraction kvnc already uses (redb, custom key-value, etc.).
- Prefer deterministic serialization (e.g. bincode, scale, or a simple custom format already used in the project).

### 5. Tokenomics alignment (`staking/src/lib.rs` in skeleton)
This file contains pure constants and functions that align emission + treasury with the well-known numbers:

- Total supply 90 200 000 KVNC
- 200 000 premine
- 8 000 000 treasury (linear 1 M / year)
- Initial reward 10 KVNC
- Era length 2 050 000 committed leaders
- Decay × 3/4

**Actions:**
- Merge the constants and pure functions (`block_reward`, `treasury_vested`, `circulating_supply`, `StakingState` helpers) into the existing `kvnc-staking` crate.
- Make sure `ExecutionContext` (or equivalent) calls `on_leader_committed` on every committed leader block and credits the leader’s payout address.
- Do **not** change the Mysticeti consensus or DAG logic — only the reward / vesting math.

### 6. Contract entry points
Each contract has `#[no_mangle] extern "C"` stubs.

- Wire them into the existing Wasmi runtime / dispatcher so they can be called as contract methods.
- Keep the public API of each contract stable:
  - HTLC: `create`, `claim`, `refund`
  - Vault: `create`, `claim`, `cancel`
  - Multisig: `create`, `propose`, `confirm`, `execute`
  - Token: `create`, `transfer`, `approve`, `transfer_from`, `mint`, `burn`

### 7. Testing & quality
- Add basic unit tests for the pure tokenomics functions (reward schedule, vesting amounts).
- Ensure the new crates compile with `cargo check --workspace`.
- Keep everything `no_std`-friendly where the rest of kvnc expects it.
- Use deterministic collections (`BTreeMap` / `BTreeSet`) — already done in the skeleton.

### 8. Documentation
- Update or create a short `docs/CONTRACTS.md` that lists the four contracts and their entry points.
- Mention the tokenomics constants in the existing tokenomics / staking docs if they exist.

## Success criteria
- All five skeleton pieces are integrated.
- Workspace builds cleanly.
- No references to any external Kovanica project.
- Tokenomics numbers and reward path are consistent with the constants in the skeleton.
- Contracts are callable through the existing Wasmi execution path.

## Style
- Prefer clarity and auditability over cleverness.
- Keep diffs focused — do not refactor unrelated parts of the codebase.
- When in doubt, leave a clear `// TODO:` comment rather than inventing incomplete behaviour.

Start by inspecting the current workspace layout and the existing `kvnc-staking` + execution crates, then propose the exact file moves and `Cargo.toml` changes before applying them.
