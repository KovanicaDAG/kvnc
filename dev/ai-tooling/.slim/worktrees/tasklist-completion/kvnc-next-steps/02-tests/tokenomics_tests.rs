//! Unit tests for the aligned tokenomics functions.
//! Drop into kvnc-staking or a dedicated tests module.

#[cfg(test)]
mod tests {
    use super::*; // or use the real path to block_reward, treasury_vested, etc.

    // These constants must match what you put in staking
    const ONE_KVNC: u128 = 1_000_000_000;
    const INITIAL_REWARD: u128 = 10 * ONE_KVNC;
    const ERA_LENGTH: u64 = 2_050_000;
    const TREASURY_TOTAL: u128 = 8_000_000 * ONE_KVNC;
    const BLOCKS_PER_YEAR: u64 = 15_768_000;

    #[test]
    fn reward_era_0() {
        assert_eq!(block_reward(0), INITIAL_REWARD);
        assert_eq!(block_reward(ERA_LENGTH - 1), INITIAL_REWARD);
    }

    #[test]
    fn reward_decays_by_three_quarters() {
        let era1 = block_reward(ERA_LENGTH);
        assert_eq!(era1, INITIAL_REWARD * 3 / 4);

        let era2 = block_reward(ERA_LENGTH * 2);
        assert_eq!(era2, INITIAL_REWARD * 3 / 4 * 3 / 4);
    }

    #[test]
    fn treasury_vesting_linear() {
        assert_eq!(treasury_vested(0), 0);
        assert_eq!(treasury_vested(BLOCKS_PER_YEAR), 1_000_000 * ONE_KVNC);
        assert_eq!(treasury_vested(BLOCKS_PER_YEAR * 8), TREASURY_TOTAL);
        assert_eq!(treasury_vested(BLOCKS_PER_YEAR * 100), TREASURY_TOTAL); // capped
    }

    #[test]
    fn circulating_never_exceeds_total() {
        let total = 90_200_000 * ONE_KVNC;
        let circ = circulating_supply(total, u64::MAX);
        assert!(circ <= total);
    }
}
