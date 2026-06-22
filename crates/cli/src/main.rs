//! `FaultForge` operator CLI (`faultforge`).
//!
//! Run with a subcommand for one-shot scripting (`agents list`, `agents show`),
//! or with no subcommand on a TTY to launch the interactive TUI.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod api;
mod cli;
mod output;
mod script;
mod tui;

use std::io::IsTerminal as _;
use std::process;

use anyhow::Context as _;
use clap::Parser;

use cli::Cli;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let runtime = tokio::runtime::Runtime::new()?;

    let exit_code = match cli.command {
        Some(ref command) => runtime.block_on(script::run(&cli.global, command)),
        None => {
            if std::io::stdout().is_terminal() {
                let client = api::Client::new(&cli.global.master_url)
                    .context("failed to initialise API client")?;
                runtime.block_on(tui::run(client))?;
                script::EXIT_OK
            } else {
                // Not a TTY and no subcommand: print help to stderr and exit non-zero.
                let mut cmd = <Cli as clap::CommandFactory>::command();
                cmd.write_help(&mut std::io::stderr())?;
                eprintln!();
                script::EXIT_ERROR
            }
        }
    };

    if exit_code != 0 {
        process::exit(exit_code);
    }
    Ok(())
}
