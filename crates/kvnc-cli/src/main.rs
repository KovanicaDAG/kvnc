//! KVNC command-line interface.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "kvnc")]
#[command(about = "Kovanica (KVNC) CLI", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate a new keypair
    Keygen,
    /// Show chain info
    Info,
    /// Submit a transfer
    Transfer { to: String, amount: u64 },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Keygen => {
            println!("TODO: generate keypair");
        }
        Commands::Info => {
            println!("KVNC blockchain – skeleton");
        }
        Commands::Transfer { to, amount } => {
            println!("TODO: transfer {amount} to {to}");
        }
    }
}
