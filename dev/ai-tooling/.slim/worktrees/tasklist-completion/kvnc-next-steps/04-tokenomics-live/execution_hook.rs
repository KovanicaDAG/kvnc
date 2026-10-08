//! Skeleton: wire tokenomics into the live execution path.
//!
//! This belongs in kvnc-execution (or equivalent).

use kvnc_staking::{block_reward, StakingState}; // adjust path

/// Called by the consensus → execution bridge on every committed leader.
pub fn on_committed_leader(
    staking: &mut StakingState,
    leader_payout_address: [u8; 32],
    // balances: &mut BalanceTable,   // your real balance store
) {
    let reward = staking.on_leader_committed(leader_payout_address);

    if reward > 0 {
        // TODO: credit leader_payout_address with `reward`
        // balances.credit(&leader_payout_address, reward);
    }

    // Treasury vesting is advanced automatically inside on_leader_committed
    // (via committed_leader_height). No extra work needed here.
}

/// Optional: expose a way for the treasury authority to claim vested tokens.
pub fn claim_treasury(
    staking: &mut StakingState,
    // balances: &mut BalanceTable,
    amount: u128,
    treasury_address: [u8; 32],
) -> Result<(), &'static str> {
    staking
        .claim_treasury(amount)
        .map_err(|_| "insufficient vested amount")?;

    // TODO: credit treasury_address with `amount`
    // balances.credit(&treasury_address, amount);
    Ok(())
}

// ---------------------------------------------------------------------------
// Checklist for the agent
// ---------------------------------------------------------------------------
// [ ] on_committed_leader is called from the real linearizer / execution entry point
// [ ] reward is actually credited to the leader’s payout address
// [ ] treasury claim path exists and is protected (only treasury key)
// [ ] total_mining_issued and committed_leader_height are persisted
// [ ] numbers match the constants in the skeleton (90.2 M, 10 KVNC, etc.)
