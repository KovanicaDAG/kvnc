# B — Treasury Proposal Skeleton (kvnc-staking)

Status: skeleton (client/ledger-safe), deferred to Phase 25.1 after Phase 18 stable.

## Struct proposal
```rust
pub struct TreasuryProposal {
    pub proposal_id: String,      // "prop-{epoch}-{counter}"
    pub target_vault_index: u8,   // 0..7 (8 vaults)
    pub amount_atoms: u64,         // atoms
    pub recipient: Address,
    pub justification_hash: [u8; 32],
    pub proposal_deadline: u64,    // epoch
    pub stake_voted_for: u64,
    pub stake_voted_against: u64,
    pub status: ProposalStatus,
}

pub enum ProposalStatus {
    Open,
    Voting,
    Approved,
    Rejected,
    Executed,
}
```

## Flow (not consensus-critical yet)
1. Submit proposal (`StakingState::submit_treasury_proposal`) — client-only.
2. Stake-weighted vote (`vote_for` / `vote_against`) — requires Phase 18 delegation stable.
3. Quorum: 2/3 active validator stake; approval >50% voted.
4. Execution: epoch boundary only; release from vault index.

## References
- `docs/GOVERNANCE.md` (Phase 25, treasury section)
- `crates/kvnc-staking/src/lib.rs` (staking skeleton, `76e52db`)
- `docs/A8.2-DELEGATION-SKELETON.md` (Phase 18 deferred)
