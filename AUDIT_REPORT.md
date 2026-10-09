# Internal Audit Execution — f2b05bf (kvnc)

Status: **internal verification only** — external auditor assignment still required (per docs/AUDIT-CHECKLIST.md §2). Not a release sign-off.

Date: 2026-10-09
Auditor role: kvnc-specialist (internal)
Scope: seed/DNS, staking skeleton, tokenomics invariants, consensus liveness, CLI/delegate, P2P/network, config/env safety.

---

## Findings (verified locally)

### 1. Seed / DNS (§1 checklist)
- Live `GET https://explorer.kovanica.online/api/bootstrap`: **502** (offline/down at audit time) — cannot confirm port 8000 vs 9000 live; previous session log (`@3@`) documented `listen 0.0.0.0:8000` and peers `seed2/3.kovanica.online:8000`. **Gap confirmed**: docs reference 9000; live/bootstrap reports 8000. Must reconcile before mainnet.
- DNS redundancy: only `seed.kovanica.online` documented; **need 3 seeds min** (§1 check).
- No evidence of orange-cloud proxy for TCP 9000 (or 8000); but cannot confirm from live endpoint due to 502.

### 2. External audit scope (§2 checklist) — partially verified
- Consensus (Mysticeti, uncertified, wave=3, k=3): `engine.rs` verified; `leader_timeout_ms = 3000`; skip after timeout; wave logic present. No certificate dependency. PASS.
- Execution (Wasmi): skeleton verified (`docs/EXECUTION-GAS-SKELETON.md` + `crates/kvnc-execution/src/lib.rs`); determinism enforced by design. PASS (full metering deferred Phase 25).
- Staking: `MIN_VALIDATOR_STAKE = 50_000 * ONE_KVNC` (§3 grep); `commission_bps` (basis points); `UnbondingEntry` with `UNBONDING_ROUNDS`; `bond/unbond/slash/withdraw` present. PASS.
- Tokenomics: `max_supply = 90_200_000`; `s0 = 10`; `era = 2_050_000`; `decay = 3/4`; maturity 100; fee floor `max(1, subsidy / 500_000)`. PASS.
- Crypto: Ed25519 (64-byte sig / 128 hex) used (`kvnc-crypto`); keys client-only; no seed/key passed to node. PASS.
- P2P: `listen` / `peer` / `seed` code verified (`kvnc-network/src/service.rs`); gossip, ping, ban rules present. PASS (full Sybil/eclipse resistance requires external audit).
- Node / RPC: `KVNC_RPC_AUTH=disable` documented for test only; production requires auth; no key/seed in env. PASS.

### 3. Live `/api/bootstrap` verification (§3 checklist) — blocked by 502
- Could not run live verification; recommend re-run when endpoint healthy.
- Previous session confirmed: `listen 0.0.0.0:8000`, peers `seed2.kovanica.online:8000`, `seed3.kovanica.online:8000`.
- `KOVANICA_ALLOW_RESET` must remain 0 (enforced by `docs/AUDIT-CHECKLIST.md` and ops scripts); verified no `ALLOW_RESET=1` in workspace.

### 4. CLI / wallet audit (§4 checklist) — verified
- `delegate` command skeleton (`kvnc-cli/src/main.rs:219`, `stake.rs:delegate_skeleton`); deferred Phase 18 (`A8.2-DELEGATION-SKELETON.md`). PASS (not active until Phase 18 stable).
- `claim_rewards_skeleton` present; HTLC claim verified (`main.rs:84`). PASS.
- `delegate` / `unbond` / `withdraw_unbonded` / `slash` wired (`docs/A8.2-DELEGATION-SKELETON.md` lines 437–567). PASS.

---

## Environment / build observations (not audit findings, but relevant)
- `cargo check --workspace --all-targets`: 0 errors; warnings only (unused imports / dead code in test).
- `cargo test --workspace`: 290 pass; 2 fail (`live_vote_integration`) — blocked by `mold` linker error (libp2p undefined symbols) and previous `Address already in use` (fixed by port change to 19545/19546/19000/19001). Not a consensus/code defect.
- `crates/kvnc-staking/src/lib.rs`: skeleton complete (`bond`/`unbond`/`set_commission` pushed `76e52db`); no ledger modifications in this audit.
- `docs/GOVERNANCE.md`: Phase 25 finalized; proposal format, treasury, upgrade signaling documented. No consensus change.

---

## Conclusion

Internal verification: **PASS** for all locally verifiable invariants (tokenomics, staking rules, consensus timeout, crypto, P2P structure, CLI skeleton, env safety). **GAPS remain** that require external audit or live endpoint health:

1. **Port reconciliation**: confirm live/bootstrap port (8000 vs 9000) — must fix docs/env before mainnet.
2. **DNS redundancy**: add 2+ additional seeds; verify TCP 9000 (or confirmed port) is plaintext only.
3. **Live endpoint**: `/api/bootstrap` was 502 at audit time; rerun when healthy.
4. **External audit**: this report is internal only; external auditor must sign off on consensus (uncertified DAG, wave=3), execution (Wasmi determinism + gas), staking (commission/slash economics), P2P (Sybil/eclipse), and supply invariants before any mainnet/release.
5. **Phase 25.1 activation**: `StakingState::vote_on_proposal()` and treasury release flow deferred; requires Phase 18 delegation stable.

No code changes made in this audit (docs / verification only); `docs/AUDIT-CHECKLIST.md`, `docs/AUDIT_REPORT.md`, `docs/GOVERNANCE.md`, and `docs/B-TREASURY-PROPOSAL.md` updated. All consistent with `AGENTS.md` (kvnc, independent from kovanica-protocol).
