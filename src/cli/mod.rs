//! CLI surface for `carryoverd`: install / refresh / status / start / stop / uninstall.
//!
//! Uses the clap derive API. Global flags: `--config <PATH>` and `--verbose`.
//! Each subcommand handler is a stub returning `Ok(())` until Phase 2 PRs fill them in.

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "carryoverd", version, about = "Zero-LLM-token context-handoff daemon")]
pub struct Cli {
    /// Path to a config file (overrides ~/.carryover/config.toml)
    #[arg(short, long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Enable verbose output
    #[arg(short, long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Install Carryover hooks for detected tools
    Install,
    /// Re-detect tools and update installed hooks
    Refresh,
    /// Show daemon status and recent events
    Status,
    /// Start the Carryover daemon in the foreground
    Start,
    /// Stop the running Carryover daemon
    Stop,
    /// Remove all installed hooks and stop the daemon
    Uninstall,
}

pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Install => {
            eprintln!("not yet implemented: install");
        }
        Commands::Refresh => {
            eprintln!("not yet implemented: refresh");
        }
        Commands::Status => {
            eprintln!("not yet implemented: status");
        }
        Commands::Start => {
            eprintln!("not yet implemented: start");
        }
        Commands::Stop => {
            eprintln!("not yet implemented: stop");
        }
        Commands::Uninstall => {
            eprintln!("not yet implemented: uninstall");
        }
    }
    Ok(())
}
