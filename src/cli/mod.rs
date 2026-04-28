//! Top-level CLI surface. clap derive shape; four real subcommand handlers.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

pub mod config;
pub mod hooks_writer;
pub mod install;
pub mod refresh;
pub mod status;
pub mod uninstall;

#[derive(Parser, Debug)]
#[command(
    name = "carryoverd",
    version,
    about = "Zero-LLM-token context-handoff daemon"
)]
pub struct Cli {
    /// Path to a config file (overrides ~/.carryover/config.json).
    #[arg(short = 'c', long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Enable verbose output.
    #[arg(short = 'v', long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Install Carryover hooks for detected AI tools.
    Install,
    /// Re-detect tools and update installed hooks.
    Refresh,
    /// Show daemon status, ledger stats, and recent events.
    Status,
    /// Start the Carryover daemon in the foreground (future PR).
    Start,
    /// Stop the running Carryover daemon (future PR).
    Stop,
    /// Remove all installed hooks and stop the daemon.
    Uninstall {
        /// Also delete ~/.carryover/ (ledger, config, events). Off by default
        /// — your prior session history is preserved unless you ask.
        #[arg(long)]
        purge: bool,
    },
}

pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Install => install::run().context("install failed"),
        Commands::Refresh => refresh::run().context("refresh failed"),
        Commands::Status => status::run().context("status failed"),
        Commands::Start => {
            eprintln!("not yet implemented: start (lands with the systemd unit PR)");
            Ok(())
        }
        Commands::Stop => {
            eprintln!("not yet implemented: stop (lands with the systemd unit PR)");
            Ok(())
        }
        Commands::Uninstall { purge } => uninstall::run(purge).context("uninstall failed"),
    }
}
