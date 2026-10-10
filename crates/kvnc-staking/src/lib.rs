//! Staking, delegation, validator set management, token emission and treasury for Kovanica (KUNA).
//!
//! Tokenomics (locked):
//! - Total supply: 90_200_000 KUNA
//! - Founder premine: 200_000 KUNA
//! - Treasury: 8_000_000 KUNA (1M/year × 8 years) with linear vesting
//! - Mining subsidy: ~82_000_000 KUNA
//! - Initial block reward: 10 KUNA (paid to the author of a committed leader block)
//! - Decay: × 3/4 every 2_050_000 blocks
//! - Active validators: 15–21
//!
//! All canonical numbers above are shared with the tokenomics skeleton
//! (`kvnc-skeleton/staking/src/lib.rs`); this crate keeps them as `u64`
//! (the storage wire format). Cross-checked by the unit tests in this file.

#![deny(unsafe_code)]

use kvnc_types::{Address, PublicKey, Stake};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ============================================================
// Tokenomics constants (9 decimals)
// ============================================================

/// Ticker / display symbol of the native currency (display only; not
/// persisted, not part of any hash or signature domain).
pub const TICKER: &str = "KUNA";

/// Number of base units in 1 KUNA.
pub const DECIMALS: u32 = 9;
pub const ONE_KUNA: u64 = 1_000_000_000; // 10^9

/// Total maximum supply: 90.2 million KUNA.
pub const TOTAL_SUPPLY: u64 = 90_200_000 * ONE_KUNA;

/// Founder premine: 200_000 KUNA.
pub const FOUNDER_PREMINE: u64 = 200_000 * ONE_KUNA;

/// Treasury allocation: 8 × 1_000_000 KUNA over 8 years.
pub const TREASURY_TOTAL: u64 = 8_000_000 * ONE_KUNA;
pub const TREASURY_ANNUAL: u64 = 1_000_000 * ONE_KUNA;
pub const TREASURY_YEARS: u32 = 8;

/// Approximate number of blocks that correspond to one year.
/// Used only for treasury vesting schedule (tunable later via governance).
/// Assumption: ~1 block / 2s average → ~15_768_000 blocks/year.
pub const BLOCKS_PER_YEAR: u64 = 15_768_000;

/// Mining subsidy budget (everything that is not premine or treasury).
pub const MINING_SUBSIDY_BUDGET: u64 = TOTAL_SUPPLY - FOUNDER_PREMINE - TREASURY_TOTAL; // ~82M

/// Initial block reward: 10 KUNA.
///
/// (Skeleton name: `INITIAL_REWARD` — same value.)
pub const INITIAL_BLOCK_REWARD: u64 = 10 * ONE_KUNA;

/// Subsidy era length in blocks (chosen so cumulative issuance ≈ 82M).
///
/// (Skeleton name: `ERA_LENGTH` — same value: 2_050_000.)
pub const SUBSIDY_ERA_BLOCKS: u64 = 2_050_000;

/// Decay factor numerator / denominator = 3/4.
pub const DECAY_NUM: u64 = 3;
pub const DECAY_DEN: u64 = 4;

// ============================================================
// Staking constants
// ============================================================

/// Minimum stake required to become a validator (in base units).
pub const MIN_VALIDATOR_STAKE: Stake = 50_000 * ONE_KUNA; // 50 000 KUNA

/// Minimum number of active validators (target range 15–21).
///
/// The set is considered under-provisioned below this size; new validators
/// should be admitted while active count < `MAX_ACTIVE_VALIDATORS`.
pub const MIN_ACTIVE_VALIDATORS: u32 = 15;

/// Maximum number of active validators (target range 15-21).
pub const MAX_ACTIVE_VALIDATORS: usize = 21;

/// Unbonding period in rounds (approximate – will be mapped to time later).
///
/// (Skeleton name: `UNBONDING_PERIOD` — same value: 100_000 rounds.)
pub const UNBONDING_ROUNDS: u64 = 100_000;

/// Epoch rotation length (same as subsidy era for simplicity, linked to commitment height).
pub const EPOCH_ROUNDS: u64 = SUBSIDY_ERA_BLOCKS; // 2_050_000

// ============================================================
// Emission schedule (mining rewards)
// ============================================================

/// Returns the block reward (in base units) for a given **committed leader height**.
///
/// In the DAG model the “height” is the sequential number of committed leader
/// blocks (not the raw round number). Reward starts at 10 KUNA and is
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
/// 8 000 000 KUNA are released linearly over 8 years (1 000 000 KUNA per year).
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

    /// Check how much is currently claimable from the treasury.
    pub fn claimable(&self) -> u64 {
        self.available()
    }
}

// ============================================================
// Reward distribution (tied to committed leader blocks)
// ============================================================

/// Result of applying a block reward for a committed leader.
#[derive(Clone, Debug, Serialize, Deserialize)]
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
    /// Public key for consensus voting (optional for backward compatibility;
    /// required for active validators in the committee).
    #[serde(default)]
    pub public_key: Option<PublicKey>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Delegation {
    pub delegator: Address,
    pub validator: Address,
    pub amount: Stake,
}

/// Fixed slash percentage (basis points); 500 bps = 5%.
pub const SLASH_PCT_BPS: u16 = 500;

/// Unbonding queue entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UnbondingEntry {
    pub delegator: Address,
    pub validator: Address,
    pub amount: Stake,
    /// Committed-leader height at which the unbond becomes withdrawable.
    pub release_height: u64,
}

/// Double-sign evidence (skeleton — fixed % slash applied).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DoubleSignEvidence {
    pub validator: Address,
    pub height: u64,
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
    #[error("No delegation found")]
    NoDelegation,
    #[error("Unbonding not complete")]
    UnbondNotComplete,
    #[error("Evidence already processed")]
    EvidenceProcessed,
    #[error("Treasury has insufficient available balance")]
    TreasuryInsufficient,
    #[error("Validator already registered")]
    ValidatorExists,
    #[error("Commission must be <= 10000 bps")]
    InvalidCommission,
    #[error("Remaining self-stake would be below MIN_VALIDATOR_STAKE")]
    BelowMinimumStake,
}

/// Combined staking + emission + treasury state.
#[derive(Default, Serialize, Deserialize)]
pub struct StakingState {
    pub validators: Vec<ValidatorInfo>,
    pub delegations: Vec<Delegation>,
    pub total_staked: Stake,
    /// Monotonically increasing counter of committed leader blocks.
    pub committed_leader_height: u64,
    /// Total mining rewards issued so far.
    pub total_mining_issued: u64,
    /// Unbonding queue: delegations being withdrawn.
    pub unbonding_queue: Vec<UnbondingEntry>,
    /// Treasury vesting state (None until genesis configures it).
    pub treasury: Option<TreasuryState>,
}

impl StakingState {
    pub fn new() -> Self {
        Self {
            validators: Vec::new(),
            delegations: Vec::new(),
            total_staked: 0,
            committed_leader_height: 0,
            total_mining_issued: 0,
            unbonding_queue: Vec::new(),
            treasury: None,
        }
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
        public_key: Option<PublicKey>,
    ) -> Result<(), StakingError> {
        if stake < MIN_VALIDATOR_STAKE {
            return Err(StakingError::InsufficientStake);
        }
        if commission_bps > 10_000 {
            return Err(StakingError::InvalidCommission);
        }
        if self.validators.iter().any(|v| v.address == address) {
            return Err(StakingError::ValidatorExists);
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
            public_key,
        });
        self.total_staked += stake;
        Ok(())
    }

    /// Validators in consensus committee order: active with
    /// `stake >= MIN_VALIDATOR_STAKE`, top [`MAX_ACTIVE_VALIDATORS`] by stake
    /// (ties by address), then sorted by address. The position in this list
    /// is the authority index. Mirrors `kvnc-node::build_committee` exactly.
    pub fn committee_order(&self) -> Vec<&ValidatorInfo> {
        let mut active: Vec<&ValidatorInfo> = self
            .validators
            .iter()
            .filter(|v| v.active && v.stake >= MIN_VALIDATOR_STAKE)
            .collect();
        active.sort_by(|a, b| {
            b.stake
                .cmp(&a.stake)
                .then_with(|| a.address.0.cmp(&b.address.0))
        });
        active.truncate(MAX_ACTIVE_VALIDATORS);
        active.sort_by_key(|v| v.address.0);
        active
    }

    /// Validator behind a consensus authority index (identity-based: the
    /// committee is ordered by validator address/key, not by insertion
    /// position in `validators`).
    pub fn authority_validator(&self, authority_index: u16) -> Option<&ValidatorInfo> {
        self.committee_order()
            .get(authority_index as usize)
            .copied()
    }

    /// Look up payout address for a given authority index.
    pub fn payout_address_for(&self, authority_index: u16) -> Option<Address> {
        self.authority_validator(authority_index)
            .map(|v| v.payout_address)
    }

    /// Total amount delegated to `validator` by delegators.
    pub fn delegated_to(&self, validator: Address) -> Stake {
        self.delegations
            .iter()
            .filter(|d| d.validator == validator)
            .map(|d| d.amount)
            .fold(0, Stake::saturating_add)
    }

    /// Validator's own bonded stake (`stake` minus delegations).
    pub fn self_stake(&self, validator: Address) -> Stake {
        self.validators
            .iter()
            .find(|v| v.address == validator)
            .map(|v| v.stake.saturating_sub(self.delegated_to(validator)))
            .unwrap_or(0)
    }

    /// Add `amount` of self-stake to an existing active validator.
    pub fn bond_validator(&mut self, address: Address, amount: Stake) -> Result<(), StakingError> {
        if amount == 0 {
            return Err(StakingError::InsufficientStake);
        }
        let v = self
            .validators
            .iter_mut()
            .find(|v| v.address == address && v.active)
            .ok_or(StakingError::NotValidator)?;
        v.stake = v.stake.saturating_add(amount);
        self.total_staked = self.total_staked.saturating_add(amount);
        Ok(())
    }

    /// Start unbonding `amount` of a validator's own stake. The remaining
    /// self-stake must be 0 (validator deactivated) or `>= MIN_VALIDATOR_STAKE`.
    /// Funds go to the unbonding queue (`UNBONDING_ROUNDS`).
    pub fn unbond_validator(
        &mut self,
        address: Address,
        amount: Stake,
    ) -> Result<(), StakingError> {
        if amount == 0 {
            return Err(StakingError::InsufficientStake);
        }
        let own = self.self_stake(address);
        let v_idx = self
            .validators
            .iter()
            .position(|v| v.address == address)
            .ok_or(StakingError::NotValidator)?;
        if amount > own {
            return Err(StakingError::InsufficientStake);
        }
        let remaining = own - amount;
        if remaining != 0 && remaining < MIN_VALIDATOR_STAKE {
            return Err(StakingError::BelowMinimumStake);
        }
        let v = &mut self.validators[v_idx];
        v.stake -= amount;
        if remaining == 0 {
            v.active = false;
        }
        self.total_staked = self.total_staked.saturating_sub(amount);
        self.unbonding_queue.push(UnbondingEntry {
            delegator: address,
            validator: address,
            amount,
            release_height: self
                .committed_leader_height
                .saturating_add(UNBONDING_ROUNDS),
        });
        Ok(())
    }

    /// Set a validator's commission (basis points, `<= 10_000`).
    pub fn set_commission(
        &mut self,
        address: Address,
        commission_bps: u16,
    ) -> Result<(), StakingError> {
        if commission_bps > 10_000 {
            return Err(StakingError::InvalidCommission);
        }
        let v = self
            .validators
            .iter_mut()
            .find(|v| v.address == address)
            .ok_or(StakingError::NotValidator)?;
        v.commission_bps = commission_bps;
        Ok(())
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

    /// Claim available treasury funds.
    /// Returns the amount actually claimed (may be less than requested if not enough vested).
    pub fn claim_treasury(&mut self, amount: u64) -> Result<u64, StakingError> {
        let treasury = self
            .treasury
            .as_mut()
            .ok_or(StakingError::TreasuryInsufficient)?;
        let claimed = treasury.claim(amount)?;
        Ok(claimed)
    }

    /// Rotate epoch at `EPOCH_ROUNDS` boundary (linked to committed-leader height).
    /// Returns the new epoch number (starts at 0, increments every `EPOCH_ROUNDS`).
    pub fn rotate_epoch(&mut self) -> u64 {
        let epoch = self.committed_leader_height / EPOCH_ROUNDS;
        // Epoch rotation preserves validator set, unbonding queue, and treasury;
        // future phases may apply stake re-balancing or new admission here.
        epoch
    }

    /// Minimal genesis staking state (Phase 24.1): 15 active validators at MIN_VALIDATOR_STAKE,
    /// treasury configured, height 0. Used by genesis / testnet initialization.
    pub fn genesis(min_validators: u32) -> Self {
        let mut state = Self::new();
        let treasury_addr = Address([0xFF; 32]); // fixed genesis treasury
        state.init_treasury(treasury_addr);
        // Admit minimum active validators deterministically (sorted by address byte)
        for i in 0..min_validators {
            let byte = (i as u8 + 1) % 255; // avoid zero address collision
            let validator = Address([byte; 32]);
            state
                .join_validator(validator, MIN_VALIDATOR_STAKE, 0, None, None)
                .expect("genesis validator admission");
        }
        state
    }

    /// Check how much treasury is currently claimable.
    pub fn treasury_claimable(&self) -> u64 {
        self.treasury.as_ref().map(|t| t.claimable()).unwrap_or(0)
    }

    /// Get the treasury address if configured.
    pub fn treasury_address(&self) -> Option<Address> {
        self.treasury.as_ref().map(|t| t.treasury_address)
    }

    // ============================================================
    // Delegation + unbonding (Phase 8.2)
    // ============================================================

    /// Delegate `amount` from `delegator` to `validator`.
    pub fn delegate(
        &mut self,
        delegator: Address,
        validator: Address,
        amount: Stake,
    ) -> Result<(), StakingError> {
        if amount == 0 {
            return Ok(());
        }
        let v_idx = self
            .validators
            .iter()
            .position(|v| v.address == validator && v.active)
            .ok_or(StakingError::NotValidator)?;
        self.validators[v_idx].stake = self.validators[v_idx].stake.saturating_add(amount);
        self.delegations.push(Delegation {
            delegator,
            validator,
            amount,
        });
        self.total_staked = self.total_staked.saturating_add(amount);
        Ok(())
    }

    /// Start unbonding `amount` for `delegator` from `validator`.
    pub fn unbond(
        &mut self,
        delegator: Address,
        validator: Address,
        amount: Stake,
    ) -> Result<(), StakingError> {
        if amount == 0 {
            return Ok(());
        }
        let mut remaining = amount;
        let mut updates: Vec<(usize, u64, bool)> = Vec::new(); // (idx, take, full_consumed)
        for (i, d) in self.delegations.iter().enumerate() {
            if d.delegator == delegator && d.validator == validator && remaining > 0 {
                let take = remaining.min(d.amount);
                remaining -= take;
                updates.push((i, take, take == d.amount));
                if remaining == 0 {
                    break;
                }
            }
        }
        // Apply validator stake reduction and delegation adjustments.
        for (idx, take, full) in &updates {
            if let Some(v_idx) = self.validators.iter().position(|v| v.address == validator) {
                self.validators[v_idx].stake = self.validators[v_idx].stake.saturating_sub(*take);
            }
            self.total_staked = self.total_staked.saturating_sub(*take);
            if !full {
                self.delegations[*idx].amount -= *take;
            }
        }
        if remaining > 0 {
            // Undo validator stake changes (simplified skeleton: assume match)
            for (idx, take, full) in &updates {
                if let Some(v_idx) = self.validators.iter().position(|v| v.address == validator) {
                    self.validators[v_idx].stake =
                        self.validators[v_idx].stake.saturating_add(*take);
                }
                self.total_staked = self.total_staked.saturating_add(*take);
                if !full {
                    self.delegations[*idx].amount += *take;
                }
            }
            return Err(StakingError::NoDelegation);
        }
        // Remove fully consumed delegations (highest index first).
        let to_remove: Vec<usize> = updates
            .iter()
            .filter(|(_, _, f)| *f)
            .map(|(i, _, _)| *i)
            .collect();
        let mut sorted = to_remove;
        sorted.sort_by(|a, b| b.cmp(a));
        sorted.dedup();
        for idx in sorted {
            self.delegations.remove(idx);
        }
        let release_height = self
            .committed_leader_height
            .saturating_add(UNBONDING_ROUNDS);
        self.unbonding_queue.push(UnbondingEntry {
            delegator,
            validator,
            amount,
            release_height,
        });
        Ok(())
    }

    /// Return unbonding entries ready for withdrawal at current `committed_leader_height`.
    /// Deterministic: sorted by (delegator, validator, release_height).
    pub fn unbonding_ready(&self) -> Vec<&UnbondingEntry> {
        let current = self.committed_leader_height;
        let mut ready: Vec<&UnbondingEntry> = self
            .unbonding_queue
            .iter()
            .filter(|e| e.release_height <= current)
            .collect();
        ready.sort_by(|a, b| {
            a.delegator
                .0
                .cmp(&b.delegator.0)
                .then(a.validator.0.cmp(&b.validator.0))
                .then(a.release_height.cmp(&b.release_height))
        });
        ready
    }

    /// Withdraw from unbonding queue once `release_height` passed.
    pub fn withdraw_unbonded(
        &mut self,
        delegator: Address,
        validator: Address,
    ) -> Result<u64, StakingError> {
        let current = self.committed_leader_height;
        let mut withdrawn = 0u64;
        let mut to_remove = Vec::new();
        for (i, entry) in self.unbonding_queue.iter().enumerate() {
            if entry.delegator == delegator
                && entry.validator == validator
                && entry.release_height <= current
            {
                withdrawn = withdrawn.saturating_add(entry.amount);
                to_remove.push(i);
            }
        }
        if withdrawn == 0 {
            return Err(StakingError::UnbondNotComplete);
        }
        to_remove.sort_by(|a, b| b.cmp(a));
        for idx in to_remove {
            self.unbonding_queue.remove(idx);
        }
        Ok(withdrawn)
    }

    /// Reward sharing (commission-based, deterministic, exact).
    ///
    /// The validator takes `commission_bps` of `reward_amount`; the rest is
    /// split pro-rata over the validator's self-stake and every delegation.
    /// Integer-division dust goes to the validator, so the shares always sum
    /// to exactly `reward_amount`. Output is sorted by address and merged
    /// (one entry per address). Unknown validator → empty.
    pub fn reward_share(
        &self,
        validator_address: Address,
        reward_amount: u64,
        commission_bps: u16,
    ) -> Vec<(Address, u64)> {
        if !self
            .validators
            .iter()
            .any(|v| v.address == validator_address)
        {
            return Vec::new();
        }
        let commission_bps = u128::from(commission_bps.min(10_000));
        let reward = u128::from(reward_amount);
        let validator_cut = reward * commission_bps / 10_000;
        let pool = reward - validator_cut;
        let own = u128::from(self.self_stake(validator_address));
        let mut stakes: Vec<(Address, u128)> = self
            .delegations
            .iter()
            .filter(|d| d.validator == validator_address && d.amount > 0)
            .map(|d| (d.delegator, u128::from(d.amount)))
            .collect();
        let total: u128 = own + stakes.iter().map(|(_, a)| a).sum::<u128>();
        let mut merged: std::collections::BTreeMap<[u8; 32], u128> = Default::default();
        let mut distributed = 0u128;
        if total > 0 {
            stakes.sort_by_key(|(a, _)| a.0);
            for (addr, stake) in stakes {
                let share = (pool * stake).checked_div(total).unwrap_or(0);
                distributed += share;
                *merged.entry(addr.0).or_default() += share;
            }
        }
        // Commission + own pro-rata share + rounding dust.
        *merged.entry(validator_address.0).or_default() += reward - distributed;
        merged
            .into_iter()
            .map(|(a, v)| (Address(a), v as u64))
            .collect()
    }

    /// Process double-sign evidence: apply fixed % slash to validator stake and delegations.
    /// Security audit (20.1): slash is deterministic — fixed 500 bps, sum is commutative,
    /// so delegation processing order does not affect total slashed amount.
    ///
    /// `validator.stake` already includes delegated stake, so the cut is
    /// computed once on the self-stake and once per delegation (never twice
    /// on delegated stake). Returns the total amount removed from stake; the
    /// caller decides its destination (execution credits it to the treasury
    /// account). Duplicate
    /// evidence must be rejected by the caller (`EvidenceProcessed`), as the
    /// processed-evidence set is persisted outside `StakingState`.
    pub fn slash(&mut self, evidence: DoubleSignEvidence) -> Result<u64, StakingError> {
        let v_idx = self
            .validators
            .iter()
            .position(|v| v.address == evidence.validator)
            .ok_or(StakingError::NotValidator)?;
        let cut = |x: u64| (u128::from(x) * u128::from(SLASH_PCT_BPS) / 10_000) as u64;
        let own_slash = cut(self.self_stake(evidence.validator));
        let mut delegated_slash = 0u64;
        for d in self.delegations.iter_mut() {
            if d.validator == evidence.validator && d.amount > 0 {
                let d_slash = cut(d.amount);
                d.amount -= d_slash;
                delegated_slash = delegated_slash.saturating_add(d_slash);
            }
        }
        let total = own_slash.saturating_add(delegated_slash);
        self.validators[v_idx].stake = self.validators[v_idx].stake.saturating_sub(total);
        self.total_staked = self.total_staked.saturating_sub(total);
        Ok(total)
    }

    /// Deterministic id of a piece of double-sign evidence
    /// (`validator || height_le`).
    pub fn evidence_id(evidence: &DoubleSignEvidence) -> [u8; 40] {
        let mut id = [0u8; 40];
        id[..32].copy_from_slice(&evidence.validator.0);
        id[32..].copy_from_slice(&evidence.height.to_le_bytes());
        id
    }
}

// ============================================================
// Staking lifecycle skeleton (AGENTS.md B — bond/unbond/commission)
// ============================================================
/// Bond stake to become / remain an active validator (skeleton — needs storage integration).
pub fn bond_validator(_address: Address, _amount: u64) -> Result<(), &'static str> {
    // TODO: verify >= MIN_VALIDATOR_STAKE, update StakingState, emit event
    Ok(())
}
/// Unbond after UNBONDING_ROUNDS (skeleton — needs storage + time mapping).
pub fn unbond_validator(_address: Address) -> Result<(), &'static str> {
    // TODO: enforce unbonding period, release to account
    Ok(())
}
/// Commission rate (0-100%) for validator rewards (skeleton).
pub fn set_commission(_address: Address, _percent: u8) -> Result<(), &'static str> {
    // TODO: validate percent <= 100, save to staking state
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn ticker_is_kuna() {
        assert_eq!(TICKER, "KUNA");
        assert_eq!(DECIMALS, 9);
        assert_eq!(ONE_KUNA, 1_000_000_000);
    }

    #[test]
    fn initial_reward_is_10_kuna() {
        assert_eq!(block_reward(0), 10 * ONE_KUNA);
        assert_eq!(block_reward(SUBSIDY_ERA_BLOCKS - 1), 10 * ONE_KUNA);
    }

    #[test]
    fn first_decay() {
        let r = block_reward(SUBSIDY_ERA_BLOCKS);
        assert_eq!(r, 7_500_000_000); // 7.5 KUNA
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

    // ------------------------------------------------------------
    // Skeleton parity (`kvnc-skeleton/staking/src/lib.rs`)
    // ------------------------------------------------------------

    /// Reference copy of the skeleton's `block_reward`, computed in `u128`.
    /// The canonical `block_reward` above is `u64`; they must agree.
    fn skeleton_block_reward(committed_leader_height: u64) -> u64 {
        let era = committed_leader_height / SUBSIDY_ERA_BLOCKS;
        let mut reward: u128 = 10 * 1_000_000_000;
        for _ in 0..era {
            reward = reward.saturating_mul(3) / 4;
            if reward == 0 {
                break;
            }
        }
        reward as u64
    }

    /// Reference copy of the skeleton's `treasury_vested(h)`:
    /// linear 1M KUNA/year over 8 years, measured from height 0.
    fn skeleton_treasury_vested(committed_leader_height: u64) -> u64 {
        let years = committed_leader_height / BLOCKS_PER_YEAR;
        let vested = years.saturating_mul(TREASURY_ANNUAL);
        vested.min(TREASURY_TOTAL)
    }

    /// Independent schedule oracle: each era applies the rational decay to the
    /// preceding integer reward and rounds down to whole atoms at that era.
    /// The property bounds the era count, so this u128 calculation cannot
    /// overflow and does not rely on the production constants or function.
    fn reference_block_reward(era: u64) -> u64 {
        let mut reward = 10_000_000_000u128;
        for _ in 0..era {
            reward = reward * 3 / 4;
        }
        reward as u64
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn block_reward_matches_independent_geometric_schedule(
            era in 0u64..=100,
            offset in 0u64..SUBSIDY_ERA_BLOCKS,
        ) {
            let height = era * SUBSIDY_ERA_BLOCKS + offset;

            prop_assert_eq!(block_reward(height), reference_block_reward(era));
        }

        #[test]
        fn cumulative_issuance_is_bounded_for_every_u64_height(height in any::<u64>()) {
            prop_assert!(
                cumulative_mining_issuance(height) <= MINING_SUBSIDY_BUDGET,
                "height {height}"
            );
        }

        #[test]
        fn committed_leader_rewards_keep_circulating_supply_under_cap(
            start_height in prop_oneof![
                Just(0u64),
                Just(SUBSIDY_ERA_BLOCKS - 1),
                Just(8 * BLOCKS_PER_YEAR),
                Just(100 * SUBSIDY_ERA_BLOCKS),
                0u64..=(100 * SUBSIDY_ERA_BLOCKS),
            ],
            validator_count in 1usize..=4,
            payout_seed in any::<u8>(),
            authors in prop::collection::vec(any::<u8>(), 1..64),
        ) {
            let mut state = StakingState::new();
            state.init_treasury(Address([payout_seed; 32]));

            for index in 0..validator_count {
                let address_byte = payout_seed.wrapping_add(index as u8);
                let validator = Address([address_byte; 32]);
                let payout = Address([address_byte.wrapping_add(128); 32]);
                state
                    .join_validator(validator, MIN_VALIDATOR_STAKE, 0, Some(payout), None)
                    .expect("generated validator setup is valid");
            }

            // Seed a schedule-consistent state at an arbitrary height. The
            // exact 100-era case exercises supply close to the cap after the
            // treasury has fully vested.
            state.committed_leader_height = start_height;
            state.total_mining_issued = cumulative_mining_issuance(start_height);
            state
                .treasury
                .as_mut()
                .expect("treasury was initialized")
                .advance(start_height);
            prop_assert!(state.circulating_supply() <= TOTAL_SUPPLY);

            for author in authors {
                let authority_index = (author as usize % validator_count) as u16;
                state
                    .on_leader_committed(authority_index)
                    .expect("generated authority index exists");
                prop_assert!(
                    state.circulating_supply() <= TOTAL_SUPPLY,
                    "height {}",
                    state.committed_leader_height
                );
            }
        }
    }

    #[test]
    fn canonical_values_match_skeleton() {
        assert_eq!(DECIMALS, 9);
        assert_eq!(ONE_KUNA, 1_000_000_000);
        assert_eq!(TOTAL_SUPPLY, 90_200_000 * ONE_KUNA);
        assert_eq!(FOUNDER_PREMINE, 200_000 * ONE_KUNA);
        assert_eq!(TREASURY_TOTAL, 8_000_000 * ONE_KUNA);
        assert_eq!(TREASURY_ANNUAL, 1_000_000 * ONE_KUNA);
        assert_eq!(MINING_SUBSIDY_BUDGET, 82_000_000 * ONE_KUNA);
        assert_eq!(INITIAL_BLOCK_REWARD, 10 * ONE_KUNA); // skeleton INITIAL_REWARD
        assert_eq!(SUBSIDY_ERA_BLOCKS, 2_050_000); // skeleton ERA_LENGTH
        assert_eq!((DECAY_NUM, DECAY_DEN), (3, 4));
        assert_eq!(BLOCKS_PER_YEAR, 15_768_000);
        assert_eq!(MIN_VALIDATOR_STAKE, 50_000 * ONE_KUNA);
        assert_eq!(MIN_ACTIVE_VALIDATORS, 15);
        assert_eq!(MAX_ACTIVE_VALIDATORS, 21);
        assert_eq!(UNBONDING_ROUNDS, 100_000); // skeleton UNBONDING_PERIOD
    }

    #[test]
    fn reward_schedule_matches_skeleton() {
        // Era 0 → 10 KUNA, boundary of era 0 → 10 KUNA,
        // era 1 → 7.5 KUNA after the 3/4 decay, plus later eras.
        let heights = [
            0u64,
            SUBSIDY_ERA_BLOCKS - 1,
            SUBSIDY_ERA_BLOCKS,
            2 * SUBSIDY_ERA_BLOCKS,
            3 * SUBSIDY_ERA_BLOCKS + 12_345,
        ];
        for h in heights {
            assert_eq!(block_reward(h), skeleton_block_reward(h), "height {h}");
        }
        assert_eq!(block_reward(0), 10 * ONE_KUNA);
        assert_eq!(block_reward(SUBSIDY_ERA_BLOCKS), 7_500_000_000); // 7.5 KUNA
    }

    #[test]
    fn treasury_vesting_matches_skeleton() {
        let treasury = TreasuryState::new(Address::default(), 0);
        let heights = [
            0u64,
            BLOCKS_PER_YEAR - 1,
            BLOCKS_PER_YEAR,
            BLOCKS_PER_YEAR + 1,
            7 * BLOCKS_PER_YEAR + BLOCKS_PER_YEAR / 2,
            8 * BLOCKS_PER_YEAR,
            9 * BLOCKS_PER_YEAR,
            100 * BLOCKS_PER_YEAR,
        ];
        for h in heights {
            assert_eq!(
                treasury.expected_vested(h),
                skeleton_treasury_vested(h),
                "height {h}"
            );
        }
    }

    #[test]
    fn cumulative_issuance_never_exceeds_budget() {
        // Naive per-height sum of block_reward over the first 3 full eras
        // must agree with cumulative_mining_issuance and stay in budget.
        let three_eras = 3 * SUBSIDY_ERA_BLOCKS;
        let naive: u64 = (0..three_eras).map(block_reward).sum();
        assert_eq!(naive, cumulative_mining_issuance(three_eras));
        assert!(naive <= MINING_SUBSIDY_BUDGET);

        // Full schedule: sum each era's reward until it decays to zero.
        // The geometric series 10 KUNA × 2_050_000 × 1/(1 − 3/4) equals
        // exactly 82M KUNA; floor-truncation keeps the real sum just below.
        let mut naive_full = 0u64;
        let mut era = 0u64;
        loop {
            let reward = block_reward(era * SUBSIDY_ERA_BLOCKS);
            if reward == 0 {
                break;
            }
            naive_full = naive_full.saturating_add(reward.saturating_mul(SUBSIDY_ERA_BLOCKS));
            era += 1;
        }
        assert!(naive_full <= MINING_SUBSIDY_BUDGET);
        assert!(naive_full > MINING_SUBSIDY_BUDGET * 99 / 100);

        // Spot checks at various heights, including a pathological one.
        for h in [
            0u64,
            SUBSIDY_ERA_BLOCKS,
            40 * SUBSIDY_ERA_BLOCKS,
            200 * SUBSIDY_ERA_BLOCKS,
            u64::MAX,
        ] {
            assert!(
                cumulative_mining_issuance(h) <= MINING_SUBSIDY_BUDGET,
                "height {h}"
            );
        }
    }

    #[test]
    fn leader_reward_goes_to_payout_address() {
        let mut state = StakingState::new();
        let validator = Address([1u8; 32]);
        let payout = Address([2u8; 32]);
        state
            .join_validator(validator, MIN_VALIDATOR_STAKE, 0, Some(payout), None)
            .unwrap();

        let outcome = state.on_leader_committed(0).unwrap();
        assert_eq!(outcome.recipient, payout);
        assert_ne!(outcome.recipient, validator);
        assert_eq!(outcome.amount, block_reward(0));
        assert_eq!(outcome.height, 0);
        assert_eq!(state.total_mining_issued, outcome.amount);
        assert_eq!(state.committed_leader_height, 1);

        // Unknown authority index must not mint.
        assert!(matches!(
            state.on_leader_committed(9),
            Err(StakingError::NotValidator)
        ));
    }

    #[test]
    fn delegate_unbond_wait_withdraw_and_slash_reduces_stake() {
        let mut state = StakingState::new();
        let validator = Address([1u8; 32]);
        let delegator = Address([2u8; 32]);
        state
            .join_validator(validator, MIN_VALIDATOR_STAKE, 0, None, None)
            .unwrap();
        let delegate_amount = 10_000 * ONE_KUNA;
        state
            .delegate(delegator, validator, delegate_amount)
            .unwrap();
        assert_eq!(state.delegations.len(), 1);

        state.unbond(delegator, validator, delegate_amount).unwrap();
        assert_eq!(state.delegations.len(), 0);
        assert_eq!(state.unbonding_queue.len(), 1);

        assert!(matches!(
            state.withdraw_unbonded(delegator, validator),
            Err(StakingError::UnbondNotComplete)
        ));

        let entry = state.unbonding_queue[0].clone();
        state.committed_leader_height = entry.release_height;
        let withdrawn = state.withdraw_unbonded(delegator, validator).unwrap();
        assert_eq!(withdrawn, delegate_amount);
        assert!(state.unbonding_queue.is_empty());

        // Slash reduces validator stake (fixed %) — fresh state for isolation
        let mut slash_state = StakingState::new();
        slash_state
            .join_validator(validator, MIN_VALIDATOR_STAKE, 0, None, None)
            .unwrap();
        slash_state
            .delegate(delegator, validator, delegate_amount)
            .unwrap();
        let evidence = DoubleSignEvidence {
            validator,
            height: 1,
        };
        let slashed = slash_state.slash(evidence).unwrap();
        assert!(slashed > 0);
        assert!(slash_state.total_staked < MIN_VALIDATOR_STAKE.saturating_add(delegate_amount));
    }

    #[test]
    fn epoch_rotation_at_subsidy_era_boundary() {
        let mut state = StakingState::new();
        assert_eq!(state.rotate_epoch(), 0);

        state.committed_leader_height = SUBSIDY_ERA_BLOCKS - 1;
        assert_eq!(state.rotate_epoch(), 0);

        state.committed_leader_height = SUBSIDY_ERA_BLOCKS;
        assert_eq!(state.rotate_epoch(), 1);

        state.committed_leader_height = 2 * SUBSIDY_ERA_BLOCKS;
        assert_eq!(state.rotate_epoch(), 2);
    }

    #[test]
    fn unbonding_ready_sorted_deterministic() {
        let mut state = StakingState::new();
        state
            .join_validator(Address([1u8; 32]), MIN_VALIDATOR_STAKE, 0, None, None)
            .unwrap();
        state
            .delegate(Address([2u8; 32]), Address([1u8; 32]), 5_000 * ONE_KUNA)
            .unwrap();
        state
            .unbond(Address([2u8; 32]), Address([1u8; 32]), 5_000 * ONE_KUNA)
            .unwrap();

        // Before release → none ready.
        assert!(state.unbonding_ready().is_empty());

        state.committed_leader_height = UNBONDING_ROUNDS;
        let ready = state.unbonding_ready();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].amount, 5_000 * ONE_KUNA);
    }

    #[test]
    fn slash_is_deterministic_fixed_500bps() {
        let mut state = StakingState::new();
        let validator = Address([3u8; 32]);
        state
            .join_validator(validator, MIN_VALIDATOR_STAKE, 0, None, None)
            .unwrap();
        let evidence = DoubleSignEvidence {
            validator,
            height: 1,
        };
        let total_first = state.slash(evidence.clone()).unwrap();
        // Re-apply to fresh validator with same stake (simulated second occurrence)
        let mut state2 = StakingState::new();
        state2
            .join_validator(validator, MIN_VALIDATOR_STAKE, 0, None, None)
            .unwrap();
        let total_second = state2.slash(evidence.clone()).unwrap();
        assert_eq!(
            total_first, total_second,
            "slash amount must be deterministic for same evidence"
        );
        assert_eq!(total_first, MIN_VALIDATOR_STAKE / 20); // 500 bps = 5%
    }

    #[test]
    fn genesis_state_has_15_validators_and_treasury() {
        let state = StakingState::genesis(MIN_ACTIVE_VALIDATORS);
        assert_eq!(state.validators.len(), MIN_ACTIVE_VALIDATORS as usize);
        assert!(state
            .validators
            .iter()
            .all(|v| v.active && v.stake == MIN_VALIDATOR_STAKE));
        assert!(state.treasury.is_some());
        assert_eq!(state.committed_leader_height, 0);
        assert_eq!(
            state.total_staked,
            MIN_VALIDATOR_STAKE * MIN_ACTIVE_VALIDATORS as u64
        );
    }

    #[test]
    fn staking_state_bincode_roundtrip() {
        let mut state = StakingState::new();
        state.init_treasury(Address([42u8; 32]));

        for i in 0..3u8 {
            let addr = Address([i + 1; 32]);
            let payout = Address([i + 11; 32]);
            state
                .join_validator(addr, MIN_VALIDATOR_STAKE, 0, Some(payout), None)
                .unwrap();
        }

        let bytes = bincode::serialize(&state).expect("serialize");
        let decoded: StakingState = bincode::deserialize(&bytes).expect("deserialize");

        assert_eq!(state.validators.len(), decoded.validators.len());
        for (a, b) in state.validators.iter().zip(decoded.validators.iter()) {
            assert_eq!(a.address, b.address);
            assert_eq!(a.stake, b.stake);
            assert_eq!(a.public_key, b.public_key);
        }
        assert_eq!(
            state.committed_leader_height,
            decoded.committed_leader_height
        );
        assert_eq!(state.total_mining_issued, decoded.total_mining_issued);
    }
    // ---- staking wiring -------------------------------------------------

    fn a(b: u8) -> Address {
        Address([b; 32])
    }

    #[test]
    fn join_validator_rejects_duplicates() {
        let mut st = StakingState::new();
        st.join_validator(a(1), MIN_VALIDATOR_STAKE, 0, None, None)
            .unwrap();
        let err = st.join_validator(a(1), MIN_VALIDATOR_STAKE, 0, None, None);
        assert!(matches!(err, Err(StakingError::ValidatorExists)));
        assert_eq!(st.validators.len(), 1);
        assert_eq!(st.total_staked, MIN_VALIDATOR_STAKE);
    }

    #[test]
    fn bond_unbond_validator_uses_queue_and_unbonding_rounds() {
        let mut st = StakingState::new();
        st.join_validator(a(1), MIN_VALIDATOR_STAKE, 0, None, None)
            .unwrap();
        st.bond_validator(a(1), 10 * ONE_KUNA).unwrap();
        assert_eq!(st.self_stake(a(1)), MIN_VALIDATOR_STAKE + 10 * ONE_KUNA);
        // Leaving 1..MIN-1 of self-stake is rejected.
        assert!(matches!(
            st.unbond_validator(a(1), 20 * ONE_KUNA),
            Err(StakingError::BelowMinimumStake)
        ));
        st.unbond_validator(a(1), 10 * ONE_KUNA).unwrap();
        assert_eq!(st.unbonding_queue[0].release_height, UNBONDING_ROUNDS);
        assert!(matches!(
            st.withdraw_unbonded(a(1), a(1)),
            Err(StakingError::UnbondNotComplete)
        ));
        st.committed_leader_height = UNBONDING_ROUNDS;
        assert_eq!(st.withdraw_unbonded(a(1), a(1)).unwrap(), 10 * ONE_KUNA);
        // Full exit deactivates.
        st.unbond_validator(a(1), MIN_VALIDATOR_STAKE).unwrap();
        assert!(!st.validators[0].active);
        assert_eq!(st.total_staked, 0);
    }

    #[test]
    fn set_commission_validates() {
        let mut st = StakingState::new();
        st.join_validator(a(1), MIN_VALIDATOR_STAKE, 0, None, None)
            .unwrap();
        assert!(st.set_commission(a(1), 10_001).is_err());
        st.set_commission(a(1), 1_000).unwrap();
        assert_eq!(st.validators[0].commission_bps, 1_000);
    }

    // Old behaviour: authority index was the insertion position.
    #[test]
    fn authority_index_maps_by_identity_not_position() {
        let mut st = StakingState::new();
        for b in [3u8, 1, 2] {
            st.join_validator(a(b), MIN_VALIDATOR_STAKE, 0, Some(a(b + 10)), None)
                .unwrap();
        }
        assert_eq!(st.payout_address_for(0), Some(a(11)));
        assert_eq!(st.payout_address_for(1), Some(a(12)));
        assert_eq!(st.payout_address_for(2), Some(a(13)));
        assert_eq!(st.payout_address_for(3), None);
    }

    // Old behaviour: self-stake earned nothing and the shares did not sum to
    // the reward.
    #[test]
    fn reward_share_is_exact_and_pays_commission_and_delegators() {
        let mut st = StakingState::new();
        st.join_validator(a(1), MIN_VALIDATOR_STAKE, 1_000, None, None)
            .unwrap();
        let alone = st.reward_share(a(1), 10 * ONE_KUNA, 1_000);
        assert_eq!(alone, vec![(a(1), 10 * ONE_KUNA)]);
        st.delegate(a(5), a(1), MIN_VALIDATOR_STAKE).unwrap();
        let shares = st.reward_share(a(1), 10 * ONE_KUNA, 1_000);
        let sum: u64 = shares.iter().map(|(_, x)| x).sum();
        assert_eq!(sum, 10 * ONE_KUNA);
        // 10% commission, rest split 50/50.
        assert_eq!(shares, vec![(a(1), 5_500_000_000), (a(5), 4_500_000_000)]);
    }

    // Old behaviour: delegated stake was cut twice (inside validator.stake
    // and again per delegation), desynchronising total_staked.
    #[test]
    fn slash_cuts_delegated_stake_once() {
        let mut st = StakingState::new();
        st.join_validator(a(1), MIN_VALIDATOR_STAKE, 0, None, None)
            .unwrap();
        st.delegate(a(5), a(1), 10_000 * ONE_KUNA).unwrap();
        let slashed = st
            .slash(DoubleSignEvidence {
                validator: a(1),
                height: 7,
            })
            .unwrap();
        assert_eq!(slashed, 3_000 * ONE_KUNA); // 5% of 60k
        assert_eq!(st.validators[0].stake, 57_000 * ONE_KUNA);
        assert_eq!(st.total_staked, 57_000 * ONE_KUNA);
        assert_eq!(st.self_stake(a(1)), 47_500 * ONE_KUNA);
        assert_eq!(st.delegations[0].amount, 9_500 * ONE_KUNA);
    }

    #[derive(Debug, Clone)]
    enum Op {
        Join(u8),
        Delegate(u8, u8, u64),
        Unbond(u8, u8, u64),
        Leader(u16),
        Slash(u8),
        Advance,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0u8..4).prop_map(Op::Join),
            (10u8..13, 0u8..4, 1u64..20_000).prop_map(|(d, v, x)| Op::Delegate(d, v, x)),
            (10u8..13, 0u8..4, 1u64..20_000).prop_map(|(d, v, x)| Op::Unbond(d, v, x)),
            (0u16..4).prop_map(Op::Leader),
            (0u8..4).prop_map(Op::Slash),
            Just(Op::Advance),
        ]
    }

    proptest! {
        // Conservation: liquid + staked + unbonding + treasury (slashed stake)
        // == premine + mining issued, and everything stays <= the cap.
        #[test]
        fn supply_never_exceeds_cap(ops in proptest::collection::vec(op(), 1..60)) {
            let mut st = StakingState::new();
            st.init_treasury(a(0xFF));
            let mut liquid: u128 = u128::from(FOUNDER_PREMINE);
            let mut treasury: u128 = 0;
            let mut slashed_evidence = std::collections::BTreeSet::new();
            for o in ops {
                match o {
                    Op::Join(v) => {
                        if liquid >= u128::from(MIN_VALIDATOR_STAKE)
                            && st.join_validator(a(v + 1), MIN_VALIDATOR_STAKE, 500, None, None).is_ok() {
                            liquid -= u128::from(MIN_VALIDATOR_STAKE);
                        }
                    }
                    Op::Delegate(d, v, x) => {
                        let x = x * ONE_KUNA;
                        if liquid >= u128::from(x) && st.delegate(a(d), a(v + 1), x).is_ok() {
                            liquid -= u128::from(x);
                        }
                    }
                    Op::Unbond(d, v, x) => { let _ = st.unbond(a(d), a(v + 1), x * ONE_KUNA); }
                    Op::Leader(i) => {
                        if let Some(v) = st.authority_validator(i).cloned() {
                            let before = st.total_mining_issued;
                            let out = st.on_leader_committed(i).unwrap();
                            let shares = st.reward_share(v.address, out.amount, v.commission_bps);
                            let paid: u64 = shares.iter().map(|(_, x)| x).sum();
                            prop_assert_eq!(paid, out.amount);
                            prop_assert_eq!(st.total_mining_issued - before, out.amount);
                            liquid += u128::from(paid);
                        }
                    }
                    Op::Slash(v) => {
                        let ev = DoubleSignEvidence { validator: a(v + 1), height: st.committed_leader_height };
                        if slashed_evidence.insert(StakingState::evidence_id(&ev)) {
                            if let Ok(x) = st.slash(ev) { treasury += u128::from(x); }
                        }
                    }
                    Op::Advance => {
                        st.committed_leader_height += UNBONDING_ROUNDS;
                        let pairs: Vec<_> = st.unbonding_ready().iter().map(|e| (e.delegator, e.validator)).collect();
                        for (d, v) in pairs {
                            if let Ok(x) = st.withdraw_unbonded(d, v) { liquid += u128::from(x); }
                        }
                    }
                }
                let queued: u128 = st.unbonding_queue.iter().map(|e| u128::from(e.amount)).sum();
                let stakes: u128 = st.validators.iter().map(|v| u128::from(v.stake)).sum();
                prop_assert_eq!(stakes, u128::from(st.total_staked));
                for v in &st.validators {
                    prop_assert!(v.stake >= st.delegated_to(v.address));
                }
                let accounted = liquid + u128::from(st.total_staked) + queued + treasury;
                prop_assert_eq!(accounted, u128::from(FOUNDER_PREMINE) + u128::from(st.total_mining_issued));
                prop_assert!(accounted + u128::from(TREASURY_TOTAL) <= u128::from(TOTAL_SUPPLY));
            }
        }
    }
}
