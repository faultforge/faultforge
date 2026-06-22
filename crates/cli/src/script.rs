//! One-shot (scripting) shell: execute a single command and exit.
//!
//! This is the imperative shell: it constructs the API client, dispatches the
//! subcommand, renders the result, and maps outcomes to exit codes.
//! All decision logic (rendering, error mapping) lives in the pure modules.

use std::io::Write as _;
use std::time::SystemTime;

use crate::api::{ApiError, Client};
use crate::cli::{AgentsCommand, Command, GlobalArgs};
use crate::output;

/// Exit code indicating success.
pub const EXIT_OK: i32 = 0;
/// Exit code indicating a transport or unexpected error.
pub const EXIT_ERROR: i32 = 1;
/// Exit code indicating the requested resource was not found.
pub const EXIT_NOT_FOUND: i32 = 2;

/// Execute `command` using the connection settings in `global`.
///
/// Writes rendered output to stdout on success, or a diagnostic to stderr on error.
/// Returns the appropriate exit code.
pub async fn run(global: &GlobalArgs, command: &Command) -> i32 {
    let client = match Client::new(&global.master_url) {
        Ok(c) => c,
        Err(e) => return write_transport_error(&e),
    };

    match command {
        Command::Agents { sub } => run_agents(&client, global, sub).await,
    }
}

async fn run_agents(client: &Client, global: &GlobalArgs, sub: &AgentsCommand) -> i32 {
    let now = SystemTime::now();
    match sub {
        AgentsCommand::List => match client.list_agents().await {
            Ok(agents) => {
                let out = output::render_agents(&agents, &global.output, now);
                println!("{out}");
                EXIT_OK
            }
            Err(e) => write_transport_error(&e),
        },
        AgentsCommand::Show { hostname } => match client.get_agent(hostname).await {
            Ok(agent) => {
                let out = output::render_agent(&agent, &global.output, now);
                println!("{out}");
                EXIT_OK
            }
            Err(ApiError::NotFound) => {
                eprintln!("error: agent not found: {hostname}");
                EXIT_NOT_FOUND
            }
            Err(e) => write_transport_error(&e),
        },
    }
}

/// Write an error message to stderr and return [`EXIT_ERROR`].
fn write_error(msg: &str) -> i32 {
    let _ = writeln!(std::io::stderr(), "error: {msg}");
    EXIT_ERROR
}

/// Write a transport/status error to stderr and return [`EXIT_ERROR`].
fn write_transport_error(e: &ApiError) -> i32 {
    write_error(&e.to_string())
}
