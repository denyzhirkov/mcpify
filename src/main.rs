mod adapters;
mod cli;
mod config;
mod errors;
mod mcp;
mod observability;
mod runtime;
mod supervisor;
mod template;

use clap::Parser;
use cli::Cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    cli::dispatch(cli).await
}
