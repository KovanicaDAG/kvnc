# Phase 25 — Governance (post-mainnet, kvnc) — finalized f2b05bf

## Active rules (hard, per AGENTS.md + tokenomics)
- Validators: 15–21 active, min stake 50 000 KVNC (`MIN_VALIDATOR_STAKE`)
- Consensus: Mysticeti-style uncertified DAG, wave = 3, k = 3
- Reward: mint only on committed `CommittedSubDag → StakingState::on_leader_committed`; initial 10 KVNC, decay ×¾ per 2 050 000 leader blocks; era budget ≈82 M KVNC within 90.2 M cap
- Treasury: 8 M KVNC (8 × 1 M linear, RFC-005 vaults via `StakingState`)
- Fee split: 75% burned / 25% producer; fee floor `max(1, subsidy / 500_000)`
- Maturity: 100 blocks

## Phase 25 — Completed skeleton (f2b05bf corrected)

### 1. Parameter-change proposal format (open → drafted)
- Proposal ID: `prop-{epoch}-{counter}`
- Fields: `target_param`, `new_value`, `justification_hash`, `stake_voted`, `proposal_deadline` (epoch + 3)
- Voting: stake-weighted, quorum = 2/3 of active validator stake, approval = >50% of staked vote
- Execution: epoch boundary only; requires Phase 18 (delegation/staking) stable

### 2. Stake-weighted on-chain voting (blocked → tracked)
- Blocked until Phase 18 delegation lifecycle is stable (`A8.2-DELEGATION-PHASE18.md` exists; skeleton done, deferred to Phase 18)
- Once unblocked: use `StakingState::vote_on_proposal()` (to be implemented Phase 18/19)
- Current placeholder: `docs/A8.2-DELEGATION-SKELETON.md`

### 3. Treasury spend via `StakingState` vaults (exists → needs proposal flow)
- `StakingState::init_treasury()` present; 8 vaults created
- Missing: proposal → vote → release flow. Add `TreasuryProposal` struct (client-only / ledger-safe, not consensus-critical yet)
- Target: Phase 25.1 after Phase 18 lock

### 4. Upgrade signaling at epoch boundary (skeleton → documented)
- Signal format: `{epoch, version_hash, activation_epoch}`
- Requires 2/3 validator agreement; activation at next epoch boundary only
- Skeleton in this doc; implementation deferred to Phase 26 (post-stable staking)

## References
- `docs/A8.2-DELEGATION-SKELETON.md` (deferred Phase 18)
- `docs/A8.2-DELEGATION-FULL.md` (full spec, Phase 18+)
- `crates/kvnc-staking/src/lib.rs` (staking skeleton pushed `76e52db`)
- Tokenomics locked: total 90 200 000 KVNC, 9 decimals
