# TASKLIST.md Drift Report — 2026-10-08

**Source of truth:** code inspection of `main` @ `69e07c4` (not the tasklist checkboxes).
**Purpose:** find items marked `[ ]` or `[~]` that are actually done, and items marked `[x]` that are not.

---

## ✅ Actually DONE — marked `[ ]` or `[~]` in tasklist

| Phase | Tasklist line | Marked | Reality | Evidence |
|-------|---------------|--------|---------|----------|
| 1.1 | 32 | `[~]` | `[x]` | Stake-weighted leader selection in `kvnc-types/src/types.rs` + `CommitteeInfo::leader_for_round` |
| 1.1 | 34 | `[x]` | `[x]` | Hash domain separation: `kvnc-types/src/hash.rs` has `domain` module with `BLOCK`, `TX`, `STATE`, `VOTE`, `MERKLE_LEAF`, `MERKLE_NODE` tags |
| 1.1 | 30 | `[x]` | `[x]` | Merkle root on `StatementBlock` — `kvnc-types/src/block.rs::compute_merkle_root` + validation in `block_manager.rs` |
| 1.2 | 46 | `[x]` | `[x]` | `verify_batch` returns `Result<bool, CryptoError>` (bad sig = `Err`), not `Ok(false)` — `kvnc-crypto/src/lib.rs:72-102` |
| 2.2 | 69 | `[~]` | `[x]` | Sorted KV Merkle for state root — `kvnc-storage/src/state_store.rs:571` (`merkle_root` fn) + `compute_state_root` |
| 2.2 | 70 | `[x]` | `[x]` | Snapshot/Restore — `state_store.rs` has `export_snapshot` / `import_snapshot` |
| 4.3 | 121 | `[x]` | `[x]` | Timeout handling: `engine.rs` `timeout_factor` + `register_skip` + `LeaderStatus::Skip` (lines 319-327) |
| 4.4 | 125 | `[x]` | `[x]` | Fork handling: lexicographic min-digest wins — `committer.rs:182`, test `engine.rs:1454-1478` |
| 5.1 | 140 | `[x]` | `[x]` | Admission control (nonce, balance, sig, gas, zero-fee reject) — `kvnc-mempool/src/lib.rs` |
| 5.2 | 146 | `[x]` | `[x]` | Conflict resolution: same sender nonce ordering — `mempool` select + `block_manager` |
| 6.1 | 166 | `[~]` | `[~]` | Kademlia wired, mDNS absent — **accurate** |
| 7.2 | 194 | `[~]` | `[~]` | Memory limits config read, limiter TODO — **accurate** |
| 8.2 | 214 | `[x]` | `[x]` | Delegation: `delegate`, `unbond`, `withdraw_unbonded`, reward sharing — `kvnc-staking/src/lib.rs:470-652` |
| 8.2 | 215 | `[x]` | `[x]` | Validator rotation: `rotate_epoch` at `EPOCH_ROUNDS` (2,050,000) — `lib.rs:429-435` + test |
| 8.2 | 216 | `[x]` | `[x]` | Slashing: `slash(DoubleSignEvidence)` 500 bps — `lib.rs:653-682` + test |
| 8.3-8.8 | 220 | `[x]` | `[x]` | Tests exist: `delegate_unbond_wait_withdraw_and_slash_reduces_stake`, `epoch_rotation_at_subsidy_era_boundary`, `slash_is_deterministic_fixed_500bps` |
| 9 | 234 | `[~]` | `[~]` | Genesis without full ceremony — **accurate** |
| 10 | 246 | `[~]` | `[~]` | `logs` subscription accepted, event source not fully wired — **accurate** |
| 11 | 260 | `[ ]` | `[~]` | `kvnc-cli status` exists, `sync`/`peers` not implemented — **partially done** |
| 11 | 261 | `[ ]` | `[~]` | CLI stake commands exist as **skeletons** returning `not_implemented` (node RPCs missing) — `kvnc-cli/src/stake.rs` |
| 12 | 276 | `[~]` | `[~]` | Property tests safety OK, liveness smoke only — **accurate** |
| 13 | 291 | `[ ]` | `[ ]` | Prometheus metrics NOT exposed — **accurate** |
| 14 | 303 | `[~]` | `[~]` | Genesis tool: `StakingState::genesis` exists, CLI tool incomplete — **accurate** |
| 15.5 | 320 | `[~]` | `[~]` | Resource hardening OK, 6h soak open — **accurate** |
| 16.1 | 336 | `[x]` | `[x]` | Committee from staking state — `build_committee` in `main.rs:1030-1100` + tests |
| 16.2 | 338 | `[ ]` | `[ ]` | Round from consensus tip — **NOT DONE** (block builder uses local counter) |
| 16.3 | 340 | `[ ]` | `[ ]` | Live TCP vote→commit→execute — **NOT DONE** |
| 16.4 | 342 | `[ ]` | `[~]` | Mempool admission on gossip/sendRawTransaction — **partial** (admission exists, not proven on live path) |
| 16.5 | 343 | `[ ]` | `[ ]` | Conflict-aware block building on live path — **NOT DONE** |
| 16.6 | 344 | `[ ]` | `[ ]` | 4-node Docker quorum identical commits — **NOT DONE** |
| 17 | 364-368 | `[ ]` | `[x]` | **All Phase 17 items are DONE** (see 4.3, 4.4, 1.1, 1.2 above) — tasklist is stale |
| 18 | 380-384 | `[ ]` | `[x]` | **All Phase 18 items are DONE** (see 8.2 above) — tasklist is stale |
| 19 | 397-401 | `[ ]` | `[ ]` | State Merkle/snapshot/fast-sync/light-client — **NOT DONE** |
| 20 | 413-417 | `[ ]` | `[ ]` | Security/fuzzing/RPC auth/soak/keystore — **NOT DONE** |
| 21 | 430-433 | `[ ]` | `[ ]` | Observability — **NOT DONE** |
| 22-25 | 445-498 | `[ ]` | `[ ]` | Dev platform/explorer/testnet/governance — **NOT DONE** |

---

## ❌ Marked `[x]` but NOT production-wired

| Phase | Tasklist line | Marked | Reality |
|-------|---------------|--------|---------|
| 11 | 258-263 | `[x]` / `[ ]` mixed | CLI wallet done; node ops + stake commands are **skeletons only** |
| 12 | 274-275 | `[x]` | Unit + in-process integration tests pass; **multi-process** quorum not verified |
| 13 | 289 | `[x]` | Docker compose exists but **no 4-validator quorum proven** (16.6 open) |

---

## 📋 Recommended tasklist corrections

**Promote to `[x]` (with notes):**
- 1.1 lines 30, 32, 34
- 1.2 line 46
- 2.2 lines 69, 70
- 4.3 line 121, 4.4 line 125
- 5.1 line 140, 5.2 line 146
- 8.2 lines 214, 215, 216
- 8.3-8.8 line 220
- 16.1 line 336

**Promote to `[~]` (partial):**
- 11 line 260 (status works; sync/peers missing)
- 11 line 261 (skeletons exist; node RPCs missing)

**Demote to `[ ]` (not production-wired):**
- 12 lines 274-275 (multi-process quorum not proven)
- 13 line 289 (compose exists but 16.6 open)

**Remove Phase 17 entirely** — it's a duplicate of Phase 4 items already done.

**Remove Phase 18 entirely** — it's a duplicate of Phase 8.2 items already done.

---

## Actual critical path (2026-10-08)

1. **16.2** — Round from consensus tip (watch channel from engine)
2. **16.3** — Live TCP vote → commit → execute (multi-process test)
3. **16.4/16.5** — Mempool admission + conflict-aware building on live path
4. **16.6** — 4-node Docker quorum with identical commits
5. **15.6** — MysticGhost 4-node soak (after 16.6)
6. **20-21** — Security + metrics
7. **14/24** — Genesis + public testnet

Everything else (Phases 17, 18, 19) depends on 16.2-16.6 or is already done.