# Execution / Gas Skeleton (AGENTS.md C — partial, existing)
- Wasmi runtime: `kvnc-runtime` (crate present)
- `gas_limit` / `gas_used` fields present (`kvnc-execution/src/lib.rs` 107, 387)
- Missing: ABI spec for contract calls, gas pricing per opcode/byte, deterministic metering audit trail.
- Determinism: Wasmi config must set `fuel` consistently; verify no `random`/`time` sources.
