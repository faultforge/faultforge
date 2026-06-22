//! Clap command-line surface: global arguments and the command enum.

use clap::{Args, Parser, Subcommand, ValueEnum};

/// `FaultForge` operator CLI — break it before it breaks you.
#[derive(Debug, Parser)]
#[command(name = "faultforge", version)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Arguments shared by all subcommands and the interactive TUI.
#[derive(Debug, Args, Clone)]
pub struct GlobalArgs {
    /// Base URL of the master's HTTP management API.
    ///
    /// An `https://` URL enables TLS with no additional flags.
    #[arg(long, default_value = "http://localhost:8069", global = true)]
    pub master_url: String,

    /// Output format for one-shot commands.
    #[arg(short, long, default_value = "json", global = true)]
    pub output: OutputFormat,
}

/// One-shot output format.
#[derive(Debug, Clone, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Machine-readable JSON.
    Json,
    /// Human-readable table.
    Table,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage registered agents.
    Agents {
        #[command(subcommand)]
        sub: AgentsCommand,
    },
}

/// Subcommands for the `agents` noun.
#[derive(Debug, Subcommand)]
pub enum AgentsCommand {
    /// List all registered agents.
    List,
    /// Show details for a single agent.
    Show {
        /// The agent hostname to look up.
        hostname: String,
    },
}
