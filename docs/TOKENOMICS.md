# KVNC Tokenomics Specification

**Version:** 0.1.0  
**Status:** Locked for implementation

---

## 1. Supply Overview

| Allocation              | Amount (KVNC)   | % of Total | Notes                                      |
|-------------------------|-----------------|------------|--------------------------------------------|
| Mining subsidy          | ≈ 82 000 000    | ~90.9 %    | Emitted via block rewards                  |
| Treasury                | 8 000 000       | ~8.9 %     | 1 M / year × 8 years, linear vesting       |
| Founder premine         | 200 000         | ~0.2 %     | Unlocked at genesis                        |
| **Total supply**        | **90 200 000**  | 100 %      | Hard cap                                   |

Decimals: **9** (1 KVNC = 1 000 000 000 base units).

---

## 2. Block Reward Schedule

- **Initial reward:** 10 KVNC  
- **Decay:** × ¾ every **2 050 000** committed leader blocks  
- Reward is paid **only** to the author of a **committed leader block** in the DAG.

### Approximate issuance per era

| Era | Reward     | Era issuance | Cumulative mining |
|-----|------------|--------------|-------------------|
| 0   | 10 KVNC    | 20.50 M      | 20.50 M           |
| 1   | 7.5 KVNC   | 15.375 M     | 35.875 M          |
| 2   | 5.625 KVNC | 11.531 M     | 47.406 M          |
| 3   | 4.21875    | 8.648 M      | 56.055 M          |
| …   | …          | …            | → ≈ 82 M          |

After ~40 eras the remaining emission becomes negligible and the mining subsidy is effectively exhausted.

---

## 3. Reward Distribution (DAG binding)

In the Mysticeti-style DAG:

1. Consensus produces a `CommittedSubDag` containing a **leader block**.
2. The execution layer calls `StakingState::on_leader_committed(leader_author)`.
3. The current `committed_leader_height` is used to look up `block_reward(height)`.
4. The reward is credited to the **payout address** of the validator that authored the leader.
5. `committed_leader_height` is incremented and treasury vesting is advanced.

This guarantees that only **finalized, ordered leader blocks** mint new tokens.

---

## 4. Treasury Vesting

- Total: **8 000 000 KVNC**
- Schedule: **1 000 000 KVNC per year** for 8 years
- Conversion: `BLOCKS_PER_YEAR = 15_768_000` (≈ 1 block / 2 s)
- Vesting is advanced automatically on every committed leader.
- The treasury address can `claim()` only the already-vested portion.

```text
year 0          year 1          year 2          …          year 8
|---------------|---------------|---------------|-----------|
0 KVNC          1 M             2 M             …          8 M (capped)
```

---

## 5. Staking Parameters

| Parameter                | Value                  |
|--------------------------|------------------------|
| Active validators        | 15 – 21                |
| Min stake to join        | 50 000 KVNC            |
| Unbonding period         | 100 000 rounds         |
| Commission               | set by validator (bps) |

Rewards go to the validator’s configured `payout_address` (defaults to the staking address). Future versions may support auto-compounding or commission splits with delegators.

---

## 6. Circulating Supply Formula

```
circulating ≈ FOUNDER_PREMINE
            + total_mining_issued
            + treasury.vested
```

Capped at `TOTAL_SUPPLY`.

---

## 7. Implementation Locations

| Component                    | Crate / Module                          |
|-----------------------------|-----------------------------------------|
| Constants & emission math   | `kvnc-staking`                          |
| TreasuryState               | `kvnc-staking`                          |
| `on_leader_committed`       | `kvnc-staking`                          |
| Reward application on commit| `kvnc-execution::ExecutionContext`      |
| CommittedSubDag             | `kvnc-consensus`                        |

### 7.1 Canonical constants (`crates/kvnc-staking/src/lib.rs`)

| Constant | Canonical value |
|----------|-----------------|
| `DECIMALS` / `ONE_KVNC` | 9 decimals, 1 KVNC = 1 000 000 000 base units |
| `TOTAL_SUPPLY` | 90 200 000 KVNC (hard cap) |
| `FOUNDER_PREMINE` | 200 000 KVNC |
| `TREASURY_TOTAL` / `TREASURY_ANNUAL` / `TREASURY_YEARS` | 8 000 000 KVNC / 1 000 000 KVNC per year / 8 years (linear) |
| `MINING_SUBSIDY_BUDGET` | 82 000 000 KVNC |
| `INITIAL_BLOCK_REWARD` (s₀) | 10 KVNC per committed leader |
| `SUBSIDY_ERA_BLOCKS` | 2 050 000 committed leaders per era |
| `DECAY_NUM` / `DECAY_DEN` | × ¾ per era |
| `BLOCKS_PER_YEAR` | 15 768 000 (treasury vesting conversion only) |
| `MIN_VALIDATOR_STAKE` | 50 000 KVNC |
| `MIN_ACTIVE_VALIDATORS` / `MAX_ACTIVE_VALIDATORS` | 15 / 21 |
| `UNBONDING_ROUNDS` | 100 000 rounds |

These values are cross-checked against the tokenomics skeleton
(`kvnc-skeleton/staking/src/lib.rs`) by the unit tests in `kvnc-staking`.

---

## 8. Future Governance Hooks

- Change `BLOCKS_PER_YEAR` (affects vesting speed)
- Adjust min stake / max validators
- Activate residual fee-burn or fee-to-stakers mechanisms once mining subsidy is low
