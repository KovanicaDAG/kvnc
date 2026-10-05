//! KVNC full node binary.

use clap::Parser;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "kvnc-node")]
#[command(about = "Kovanica (KVNC) full node", long_about = None)]
struct Args {
    /// Path to config file
    #[arg(short, long, default_value = "config.toml")]
    config: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    info!("Starting KVNC node with config: {}", args.config);

    // TODO: load committee, start network, consensus loop, RPC…

    info!("KVNC node skeleton started successfully");
    Ok(())
}
