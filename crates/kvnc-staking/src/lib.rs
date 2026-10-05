//! Staking, delegation, validator set management, token emission and treasury for KVNC.
//!
//! Tokenomics (locked):
//! - Total supply: 90_200_000 KVNC
//! - Founder premine: 200_000 KVNC
//! - Treasury: 8_000_000 KVNC (1M/year × 8 years) with linear vesting
//! - Mining subsidy: ~82_000_000 KVNC
//! - Initial block reward: 10 KVNC (paid to the author of a committed leader block)
//! - Decay: × 3/4 every 2_050_000 blocks
//! - Active validators: 15–21

#![deny(unsafe_code)]

use kvnc_types::{Address, Stake};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ============================================================
// Tokenomics constants (9 decimals)
// ============================================================

/// Number of base units in 1 KVNC.
pub const DECIMALS: u32 = 9;
pub const ONE_KVNC: u64 = 1_000_000_000; // 10^9

/// Total maximum supply: 90.2 million KVNC.
pub const TOTAL_SUPPLY: u64 = 90_200_000 * ONE_KVNC;

/// Founder premine: 200_000 KVNC.
pub const FOUNDER_PREMINE: u64 = 200_000 * ONE_KVNC;

/// Treasury allocation: 8 × 1_000_000 KVNC over 8 years.
pub const TREASURY_TOTAL: u64 = 8_000_000 * ONE_KVNC;
pub const TREASURY_ANNUAL: u64 = 1_000_000 * ONE_KVNC;
pub const TREASURY_YEARS: u32 = 8;

/// Approximate number of blocks that correspond to one year.
/// Used only for treasury vesting schedule (tunable later via governance).
/// Assumption: ~1 block / 2s average → ~15_768_000 blocks/year.
pub const BLOCKS_PER_YEAR: u64 = 15_768_000;

/// Mining subsidy budget (everything that is not premine or treasury).
pub const MINING_SUBSIDY_BUDGET: u64 = TOTAL_SUPPLY - FOUNDER_PREMINE - TREASURY_TOTAL; // ~82M

/// Initial block reward: 10 KVNC.
pub const INITIAL_BLOCK_REWARD: u64 = 10 * ONE_KVNC;

/// Subsidy era length in blocks (chosen so cumulative issuance ≈ 82M).
pub const SUBSIDY_ERA_BLOCKS: u64 = 2_050_000;

/// Decay factor numerator / denominator = 3/4.
pub const DECAY_NUM: u64 = 3;
pub const DECAY_DEN: u64 = 4;

// ============================================================
// Staking constants
// ============================================================

/// Minimum stake required to become a validator (in base units).
pub const MIN_VALIDATOR_STAKE: Stake = 50_000 * ONE_KVNC; // 50 000 KVNC

/// Maximum number of active validators (target range 15-21).
pub const MAX_ACTIVE_VALIDATORS: usize = 21;

/// Unbonding period in rounds (approximate – will be mapped to time later).
pub const UNBONDING_ROUNDS: u64 = 100_000;

// ============================================================
// Emission schedule (mining rewards)
// ============================================================

/// Returns the block reward (in base units) for a given **committed leader height**.
///
/// In the DAG model the “height” is the sequential number of committed leader
/// blocks (not the raw round number). Reward starts at 10 KVNC and is
/// multiplied by 3/4 every SUBSIDY_ERA_BLOCKS committed leaders.
pub fn block_reward(committed_leader_height: u64) -> u64 {
    let era = committed_leader_height / SUBSIDY_ERA_BLOCKS;
    let mut reward = INITIAL_BLOCK_REWARD;

    for _ in 0..era {
        reward = reward.saturating_mul(DECAY_NUM) / DECAY_DEN;
        if reward == 0 {
            break;
        }
    }
    reward
}

/// Approximate cumulative mining issuance up to (but not including) `height`.
pub fn cumulative_mining_issuance(committed_leader_height: u64) -> u64 {
    let mut total = 0u64;
    let mut reward = INITIAL_BLOCK_REWARD;
    let mut remaining = committed_leader_height;

    while remaining > 0 && reward > 0 {
        let blocks_in_this_era = remaining.min(SUBSIDY_ERA_BLOCKS);
        total = total.saturating_add(reward.saturating_mul(blocks_in_this_era));
        remaining -= blocks_in_this_era;
        reward = reward.saturating_mul(DECAY_NUM) / DECAY_DEN;
    }
    total.min(MINING_SUBSIDY_BUDGET)
}

// ============================================================
// Treasury vesting
// ============================================================

/// Treasury vesting state.
///
/// 8 000 000 KVNC are released linearly over 8 years (1 000 000 KVNC per year).
/// Vesting is driven by committed-leader height (using BLOCKS_PER_YEAR as the
/// conversion factor). The treasury address is fixed at genesis.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TreasuryState {
    /// Fixed treasury address (set at genesis).
    pub treasury_address: Address,
    /// Total amount already vested and available to the treasury.
    pub vested: u64,
    /// Total amount already claimed / transferred out of the treasury.
    pub claimed: u64,
    /// Genesis committed-leader height (usually 0).
    pub start_height: u64,
}

impl TreasuryState {
    pub fn new(treasury_address: Address, start_height: u64) -> Self {
        Self {
            treasury_address,
            vested: 0,
            claimed: 0,
            start_height,
        }
    }

    /// How many full years have elapsed since start_height.
    fn years_elapsed(&self, current_height: u64) -> u32 {
        if current_height <= self.start_height {
            return 0;
        }
        let blocks = current_height - self.start_height;
        let years = (blocks / BLOCKS_PER_YEAR) as u32;
        years.min(TREASURY_YEARS)
    }

    /// Total amount that should be vested at the given committed-leader height.
    pub fn expected_vested(&self, current_height: u64) -> u64 {
        let years = self.years_elapsed(current_height);
        (years as u64)
            .saturating_mul(TREASURY_ANNUAL)
            .min(TREASURY_TOTAL)
    }

    /// Advance vesting to the given height. Returns the newly vested amount
    /// (which is added to `self.vested`).
    pub fn advance(&mut self, current_height: u64) -> u64 {
        let expected = self.expected_vested(current_height);
        let newly_vested = expected.saturating_sub(self.vested);
        self.vested = expected;
        newly_vested
    }

    /// Amount currently available to be claimed by the treasury.
    pub fn available(&self) -> u64 {
        self.vested.saturating_sub(self.claimed)
    }

    /// Claim (transfer out) up to `amount` from the available treasury balance.
    /// Returns the actual amount claimed.
    pub fn claim(&mut self, amount: u64) -> Result<u64, StakingError> {
        let avail = self.available();
        if amount == 0 || avail == 0 {
            return Ok(0);
        }
        let claimed = amount.min(avail);
        self.claimed = self.claimed.saturating_add(claimed);
        Ok(claimed)
    }
}

// ============================================================
// Reward distribution (tied to committed leader blocks)
// ============================================================

/// Result of applying a block reward for a committed leader.
#[derive(Clone, Debug)]
pub struct RewardOutcome {
    /// Authority index that proposed the leader block.
    pub leader_author: u16,
    /// Address that receives the reward (validator payout address).
    pub recipient: Address,
    /// Amount paid (in base units).
    pub amount: u64,
    /// Committed-leader height that was used for the reward calculation.
    pub height: u64,
}

/// Applies the mining reward for a newly committed leader block.
///
/// Called by the execution layer every time a `CommittedSubDag` is finalized.
/// The reward goes to the validator that authored the leader block.
///
/// `committed_leader_height` is a monotonically increasing counter of how many
/// leader blocks have been committed so far (starts at 0).
pub fn apply_leader_reward(
    committed_leader_height: u64,
    leader_author: u16,
    recipient: Address,
) -> RewardOutcome {
    let amount = block_reward(committed_leader_height);
    RewardOutcome {
        leader_author,
        recipient,
        amount,
        height: committed_leader_height,
    }
}

// ============================================================
// Staking types
// ============================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidatorInfo {
    pub address: Address,
    pub stake: Stake,
    pub commission_bps: u16, // basis points (100 = 1%)
    pub active: bool,
    /// Address that receives block rewards (defaults to `address`).
    pub payout_address: Address,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Delegation {
    pub delegator: Address,
    pub validator: Address,
    pub amount: Stake,
}

#[derive(Error, Debug)]
pub enum StakingError {
    #[error("Insufficient stake")]
    InsufficientStake,
    #[error("Validator set full")]
    ValidatorSetFull,
    #[error("Not a validator")]
    NotValidator,
    #[error("Still unbonding")]
    StillUnbonding,
    #[error("Treasury has insufficient available balance")]
    TreasuryInsufficient,
}

/// Combined staking + emission + treasury state.
#[derive(Default)]
pub struct StakingState {
    pub validators: Vec<ValidatorInfo>,
    pub delegations: Vec<Delegation>,
    pub total_staked: Stake,
    /// Monotonically increasing counter of committed leader blocks.
    pub committed_leader_height: u64,
    /// Total mining rewards issued so far.
    pub total_mining_issued: u64,
    /// Treasury vesting state (None until genesis configures it).
    pub treasury: Option<TreasuryState>,
}

impl StakingState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Configure treasury at genesis.
    pub fn init_treasury(&mut self, treasury_address: Address) {
        self.treasury = Some(TreasuryState::new(treasury_address, 0));
    }

    /// Try to activate a new validator.
    pub fn join_validator(
        &mut self,
        address: Address,
        stake: Stake,
        commission_bps: u16,
        payout_address: Option<Address>,
    ) -> Result<(), StakingError> {
        if stake < MIN_VALIDATOR_STAKE {
            return Err(StakingError::InsufficientStake);
        }
        if self.validators.iter().filter(|v| v.active).count() >= MAX_ACTIVE_VALIDATORS {
            return Err(StakingError::ValidatorSetFull);
        }

        self.validators.push(ValidatorInfo {
            address,
            stake,
            commission_bps,
            active: true,
            payout_address: payout_address.unwrap_or(address),
        });
        self.total_staked += stake;
        Ok(())
    }

    /// Look up payout address for a given authority index.
    pub fn payout_address_for(&self, authority_index: u16) -> Option<Address> {
        self.validators
            .get(authority_index as usize)
            .map(|v| v.payout_address)
    }

    /// Called by the execution layer when a leader block is committed.
    ///
    /// 1. Calculates the reward for the current `committed_leader_height`
    /// 2. Advances the height counter
    /// 3. Advances treasury vesting
    /// 4. Returns the RewardOutcome so the caller can credit the balance
    pub fn on_leader_committed(
        &mut self,
        leader_author: u16,
    ) -> Result<RewardOutcome, StakingError> {
        let height = self.committed_leader_height;
        let recipient = self
            .payout_address_for(leader_author)
            .ok_or(StakingError::NotValidator)?;

        let outcome = apply_leader_reward(height, leader_author, recipient);

        // Update global counters
        self.committed_leader_height = height.saturating_add(1);
        self.total_mining_issued = self
            .total_mining_issued
            .saturating_add(outcome.amount)
            .min(MINING_SUBSIDY_BUDGET);

        // Advance treasury vesting
        if let Some(ref mut treasury) = self.treasury {
            treasury.advance(self.committed_leader_height);
        }

        Ok(outcome)
    }

    /// Current circulating supply approximation (premine + mining issued + vested treasury).
    pub fn circulating_supply(&self) -> u64 {
        let treasury_vested = self.treasury.as_ref().map(|t| t.vested).unwrap_or(0);
        FOUNDER_PREMINE
            .saturating_add(self.total_mining_issued)
            .saturating_add(treasury_vested)
            .min(TOTAL_SUPPLY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_reward_is_10_kvnc() {
        assert_eq!(block_reward(0), 10 * ONE_KVNC);
        assert_eq!(block_reward(SUBSIDY_ERA_BLOCKS - 1), 10 * ONE_KVNC);
    }

    #[test]
    fn first_decay() {
        let r = block_reward(SUBSIDY_ERA_BLOCKS);
        assert_eq!(r, 7_500_000_000); // 7.5 KVNC
    }

    #[test]
    fn cumulative_approaches_budget() {
        let high = SUBSIDY_ERA_BLOCKS * 40;
        let issued = cumulative_mining_issuance(high);
        assert!(issued <= MINING_SUBSIDY_BUDGET);
        assert!(issued > MINING_SUBSIDY_BUDGET * 99 / 100);
    }

    #[test]
    fn treasury_vests_linearly() {
        let addr = Address::default();
        let mut treasury = TreasuryState::new(addr, 0);

        // Before first year → nothing vested
        assert_eq!(treasury.advance(BLOCKS_PER_YEAR - 1), 0);
        assert_eq!(treasury.vested, 0);

        // After exactly 1 year → 1M vested
        let newly = treasury.advance(BLOCKS_PER_YEAR);
        assert_eq!(newly, TREASURY_ANNUAL);
        assert_eq!(treasury.vested, TREASURY_ANNUAL);

        // After 8 years → full amount
        treasury.advance(BLOCKS_PER_YEAR * 8);
        assert_eq!(treasury.vested, TREASURY_TOTAL);

        // After 10 years → still capped
        treasury.advance(BLOCKS_PER_YEAR * 10);
        assert_eq!(treasury.vested, TREASURY_TOTAL);
    }

    #[test]
    fn treasury_claim() {
        let addr = Address::default();
        let mut treasury = TreasuryState::new(addr, 0);
        treasury.advance(BLOCKS_PER_YEAR * 2); // 2M vested

        assert_eq!(treasury.available(), 2 * TREASURY_ANNUAL);
        let claimed = treasury.claim(TREASURY_ANNUAL).unwrap();
        assert_eq!(claimed, TREASURY_ANNUAL);
        assert_eq!(treasury.available(), TREASURY_ANNUAL);
    }
}
