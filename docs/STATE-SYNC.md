# Phase 19 — State & Sync Hardening (kvnc)

## Done
- `compute_state_root()` — sorted KV Merkle (`BTreeMap` + `blake3`); deterministic.
- Snapshot (`export_snapshot` / `import_snapshot`) — roundtrip test `test_snapshot_roundtrip` (redb file).
- Pruning — `DagStore::prune_below` (non-blue + wave window) wired.
- Block-sync request (`ByHash` enqueue) — partial (`kvnc-dag/src/block_sync.rs`).

## Minimal next
- Fast sync: download snapshot at height H + replay headers H..tip; verify `STATE_ROOT`.
- Light client: wave commit + colouring certificate (`commit.rs`, `WAVE_LENGTH=3`).
- Missing-parent recovery: `process_block` → `ByHash` request (rate-limited).

## Exit (Phase 19)
- New node from snapshot reaches tip; `compute_state_root()` matches committed `STATE_ROOT` table.
