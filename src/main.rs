use anyhow::Result;
use clap::{Parser, Subcommand};

use k_ruoka_mcp::{alko_mcp, grocery_mcp, login, mcp, s_kaupat_mcp};

#[derive(Parser)]
#[command(
    name = "k-ruoka-mcp",
    about = "MCP server for Finnish grocery catalogue and K-Ruoka cart access"
)]
struct Cli {
    /// Defaults to `serve`.
    ///
    /// Being an MCP server is the whole job, and clients spawn it by bare command --
    /// `{"command": "uvx", "args": ["k-ruoka-mcp"]}` passes no subcommand at all. A
    /// usage error there is unhelpful and hard to see, since the client only ever
    /// reads stdout. `login` stays explicit because it is the interactive one.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the original K-Ruoka cart MCP server over stdio.
    Serve,
    /// Run the unified K-Ruoka + S-Kaupat + Alko MCP server over stdio.
    ServeGrocery,
    /// Run the read-only Alko catalogue MCP server over stdio.
    ServeAlko,
    /// Run the read-only S-Kaupat catalogue MCP server over stdio.
    ServeSKaupat,
    /// Open a visible browser and wait while you sign in to K-Plussa by hand.
    Login {
        /// Chrome remote-debugging port, for reaching the browser over an SSH
        /// tunnel when the machine has no display.
        #[arg(long, default_value_t = 9222)]
        port: u16,
        /// Store to probe for a signed-in account while waiting.
        #[arg(long, default_value = login::DEFAULT_PROBE_STORE)]
        store_id: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command.unwrap_or(Command::Serve) {
        Command::Serve => mcp::serve().await,
        Command::ServeGrocery => grocery_mcp::serve().await,
        Command::ServeAlko => alko_mcp::serve().await,
        Command::ServeSKaupat => s_kaupat_mcp::serve().await,
        Command::Login { port, store_id } => login::run(port, &store_id).await,
    }
}
