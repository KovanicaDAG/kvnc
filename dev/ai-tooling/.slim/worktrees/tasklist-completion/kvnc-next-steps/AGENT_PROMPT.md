# Agent Prompt: Next Steps for kvnc Contracts & Tokenomics

You are working inside the **kvnc** repository.

The contract + tokenomics skeleton has already been integrated.  
Now implement the six follow-up items below. Work in order. Keep everything independent — do **not** pull any code or ideas from other Kovanica projects.

## Folder reference

A supporting skeleton is available (or will be placed) at:

```
kvnc-next-steps/
├── 01-host-and-storage/
├── 02-tests/
├── 03-rpc-cli/
├── 04-tokenomics-live/
├── 05-docs/
├── 06-examples-indexer/
└── AGENT_PROMPT.md          ← this file
```

Use the files inside as starting points and adapt them to the real codebase.

---

### 1. Host + Storage (make contracts runnable)

- Implement the real `Host` trait (see `01-host-and-storage/host_impl.rs`).
- Connect it to the existing Wasmi runtime, balance table, storage backend and event log.
- Replace every placeholder `store` / `load` in the four contracts with real persistent storage using the key layout suggested in the same file.
- Goal: the four contracts can be instantiated and called end-to-end.

### 2. Tests

- Add the unit tests from `02-tests/tokenomics_tests.rs` (reward schedule, vesting, circulating supply).
- Implement the contract test skeletons in `02-tests/contract_tests.rs` using an in-memory Host.
- All new tests must pass with `cargo test`.

### 3. RPC + CLI exposure

- Add the RPC method shapes from `03-rpc-cli/rpc_methods.rs` to the existing RPC surface.
- Add matching CLI subcommands (see `03-rpc-cli/cli_commands.rs`).
- Keep the API minimal and consistent with the rest of the node.

### 4. Tokenomics live path

- Wire `on_committed_leader` into the real execution path (see `04-tokenomics-live/execution_hook.rs`).
- Guarantee that every committed leader credits the leader’s payout address.
- Implement the treasury claim path (only vested amount, protected).
- Persist `total_mining_issued` and `committed_leader_height`.

### 5. Documentation

- Add or update `docs/CONTRACTS.md` using the content in `05-docs/CONTRACTS.md`.
- Update any existing tokenomics / staking documentation so the constants (90.2 M, 10 KVNC, 2 050 000 era, ×¾, linear treasury) are visible.

### 6. Examples + Indexer hooks

- Add a short example flow (see `06-examples-indexer/example_htlc_flow.md`).
- Register the event topics from `06-examples-indexer/indexer_events.rs` so a future indexer can pick them up.

---

## Rules

- Do **not** introduce dependencies on any other Kovanica repository.
- Prefer small, focused commits / changes.
- When something is not yet clear in the existing codebase, leave a clear `// TODO:` instead of guessing.
- After each major item, run `cargo check --workspace` and the new tests.

## Success criteria

- Contracts are callable through the real Host + storage.
- Tokenomics rewards and vesting run on every committed leader.
- Basic RPC/CLI surface exists.
- Tests pass.
- Docs and example are present.

Start by examining the current execution, staking and storage crates, then implement item 1.
