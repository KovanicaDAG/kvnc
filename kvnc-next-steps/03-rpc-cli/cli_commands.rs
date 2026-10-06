//! Skeleton CLI commands (clap-style).
//! Add these subcommands to your existing kvnc-cli.

/*
Example clap structure (pseudo):

#[derive(Subcommand)]
enum Commands {
    // ... existing commands ...

    /// HTLC operations
    Htlc {
        #[command(subcommand)]
        cmd: HtlcCmd,
    },
    Vault {
        #[command(subcommand)]
        cmd: VaultCmd,
    },
    Multisig {
        #[command(subcommand)]
        cmd: MultisigCmd,
    },
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
}

#[derive(Subcommand)]
enum HtlcCmd {
    Create {
        claimer: String,
        amount: String,
        hash_lock: String,
        expiry: u64,
    },
    Claim {
        id: String,
        preimage: String,
    },
    Refund {
        id: String,
    },
}

// Similar for VaultCmd, MultisigCmd, TokenCmd
*/

// TODO: implement the match arms that talk to the node via RPC or direct Host.
