use clap::Parser;
use mcpify::cli::{self, Cli};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    cli::dispatch(cli).await
}
