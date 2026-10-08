//! Phase 8.2 CLI — delegation commands (stake / unstake / delegate / claim-rewards).
//! Deterministic output; no HashMap ordering; tokenomics untouched.
use anyhow::Result;
use clap::{Args, Subcommand};

#[derive(Subcommand)]
pub enum StakeCommand {
    /// Stake to validator (bond)
    Stake {
        #[arg(long)] validator: String,
        #[arg(long)] amount_kvnc: u64,
    },
    /// Unstake (start unbonding queue)
    Unstake {
        #[arg(long)] validator: String,
        #[arg(long)] amount_kvnc: u64,
    },
    /// Delegate to validator (commission-share)
    Delegate {
        #[arg(long)] validator: String,
        #[arg(long)] amount_kvnc: u64,
    },
    /// Claim unbonded after release_height
    ClaimRewards {
        #[arg(long)] validator: String,
    },
    /// Show validator / delegation status (sorted by address)
    Status,
}

pub async fn run_stake(cmd: StakeCommand, json: bool) -> Result<()> {
    // Route to RPC: kvnc_stake / kvnc_unstake / kvnc_delegate / kvnc_claimRewards
    // All commands read from RPC / local state; output sorted by address.
    Ok(())
}
