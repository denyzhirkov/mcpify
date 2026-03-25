pub mod commands;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "mcpify", version, about = "Config-driven MCP tool runtime")]
pub struct Cli {
    /// Path to config file
    #[arg(short, long, global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Create a minimal mcpify.yaml config
    Init,

    /// Validate the config file
    Validate,

    /// Start the MCP server
    Serve {
        /// Watch config file for changes and auto-reload
        #[arg(short, long)]
        watch: bool,
    },

    /// Reload config in a running server
    Reload,

    /// List registered tools
    List,

    /// Show tools, services, and health status
    Status,

    /// Run a tool locally
    Run {
        /// Tool name
        name: String,

        /// Input JSON
        #[arg(short, long, default_value = "{}")]
        input: String,
    },
}

pub async fn dispatch(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Commands::Init => commands::cmd_init().await,
        Commands::Validate => commands::cmd_validate(cli.config.as_deref()).await,
        Commands::Serve { watch } => commands::cmd_serve(cli.config.as_deref(), watch).await,
        Commands::Reload => commands::cmd_reload().await,
        Commands::List => commands::cmd_list(cli.config.as_deref()).await,
        Commands::Status => commands::cmd_status(cli.config.as_deref()).await,
        Commands::Run { name, input } => {
            commands::cmd_run(cli.config.as_deref(), &name, &input).await
        }
    }
}
