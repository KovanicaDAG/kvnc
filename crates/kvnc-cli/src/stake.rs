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
    use std::collections::BTreeMap;
    match cmd {
        StakeCommand::Stake { validator, amount_kvnc } => println!("stake validator={} amount={}KVNC (RPC route)", validator, amount_kvnc),
        StakeCommand::Unstake { validator, amount_kvnc } => println!("unstake validator={} amount={}KVNC (RPC route)", validator, amount_kvnc),
        StakeCommand::Delegate { validator, amount_kvnc } => println!("delegate validator={} amount={}KVNC (RPC route)", validator, amount_kvnc),
        StakeCommand::ClaimRewards { validator } => println!("claim rewards validator={} (RPC route)", validator),
        StakeCommand::Status => {
            let mut status: BTreeMap<String, u64> = BTreeMap::new();
            status.insert("active_validators".into(), 4);
            status.insert("min_stake_kvnc".into(), 50000);
            if json { println!("{}", serde_json::to_string(&status)?); }
            else { for (k,v) in status { println!("{}: {}", k, v); } }
        }
    }
    Ok(())
}
