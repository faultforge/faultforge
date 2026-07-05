//! One-shot (scripting) shell: execute a single command and exit.
//!
//! This is the imperative shell: it constructs the API client, dispatches the
//! subcommand, renders the result, and maps outcomes to exit codes.
//! All decision logic (rendering, error mapping) lives in the pure modules.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::api::{ApiError, Client, Experiment};
use crate::cli::{AgentsCommand, Command, ExperimentCommand, GlobalArgs, OutputFormat};
use crate::output;

/// Exit code indicating success (including a `COMPLETED` waited experiment).
pub const EXIT_OK: i32 = 0;
/// Exit code indicating a transport or unexpected error.
pub const EXIT_ERROR: i32 = 1;
/// Exit code indicating the requested resource was not found.
pub const EXIT_NOT_FOUND: i32 = 2;
/// Exit code indicating the master rejected the experiment at VALIDATE.
pub const EXIT_VALIDATION: i32 = 3;
/// Exit code for a waited experiment that ended `ABORTED`.
pub const EXIT_EXPERIMENT_ABORTED: i32 = 4;
/// Exit code for a waited experiment that ended `ERROR`.
pub const EXIT_EXPERIMENT_ERROR: i32 = 5;

/// How often `--wait` polls the experiment.
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(500);

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
        Command::Experiment { sub } => run_experiment(&client, global, sub).await,
    }
}

async fn run_agents(client: &Client, global: &GlobalArgs, sub: &AgentsCommand) -> i32 {
    let now = SystemTime::now();
    match sub {
        AgentsCommand::List => match client.list_agents().await {
            Ok(agents) => {
                let out = output::render_agents(&agents, &global.output, now);
                emit(&out)
            }
            Err(e) => write_transport_error(&e),
        },
        AgentsCommand::Show { hostname } => match client.get_agent(hostname).await {
            Ok(agent) => {
                let out = output::render_agent(&agent, &global.output, now);
                emit(&out)
            }
            Err(ApiError::NotFound) => {
                eprintln!("error: agent not found: {hostname}");
                EXIT_NOT_FOUND
            }
            Err(e) => write_transport_error(&e),
        },
        AgentsCommand::ClearTaint { hostname } => match client.clear_taint(hostname).await {
            Ok(()) => print_action(
                &global.output,
                "clear-taint requested",
                hostname,
                "the agent confirms via its next report; verify with `agents show`",
            ),
            Err(ApiError::NotFound) => {
                eprintln!("error: agent not found: {hostname}");
                EXIT_NOT_FOUND
            }
            Err(ApiError::Conflict(reason)) => write_error(&reason),
            Err(e) => write_transport_error(&e),
        },
    }
}

async fn run_experiment(client: &Client, global: &GlobalArgs, sub: &ExperimentCommand) -> i32 {
    let now = SystemTime::now();
    match sub {
        ExperimentCommand::Run { file, wait } => {
            run_experiment_file(client, global, file, *wait).await
        }
        ExperimentCommand::List => match client.list_experiments().await {
            Ok(experiments) => {
                let out = output::render_experiments(&experiments, &global.output, now);
                emit(&out)
            }
            Err(e) => write_transport_error(&e),
        },
        ExperimentCommand::Show { id } => match client.get_experiment(id).await {
            Ok(experiment) => {
                let out = output::render_experiment(&experiment, &global.output, now);
                emit(&out)
            }
            Err(ApiError::NotFound) => {
                eprintln!("error: experiment not found: {id}");
                EXIT_NOT_FOUND
            }
            Err(e) => write_transport_error(&e),
        },
        ExperimentCommand::Halt { id } => match client.halt_experiment(id).await {
            Ok(()) => print_action(&global.output, "halt requested", id, "kill-switch fired"),
            Err(ApiError::NotFound) => {
                eprintln!("error: experiment not found: {id}");
                EXIT_NOT_FOUND
            }
            Err(ApiError::Conflict(reason)) => write_error(&reason),
            Err(e) => write_transport_error(&e),
        },
    }
}

async fn run_experiment_file(client: &Client, global: &GlobalArgs, file: &Path, wait: bool) -> i32 {
    let definition = match read_definition(file) {
        Ok(value) => value,
        Err(msg) => return write_error(&msg),
    };
    let experiment = match client.run_experiment(&definition).await {
        Ok(experiment) => experiment,
        Err(ApiError::Validation(errors)) => {
            eprintln!("error: experiment rejected by VALIDATE:");
            for error in errors {
                eprintln!("  - {error}");
            }
            return EXIT_VALIDATION;
        }
        Err(e) => return write_transport_error(&e),
    };

    if !wait {
        let out = output::render_experiment(&experiment, &global.output, SystemTime::now());
        return emit(&out);
    }

    // --wait: print only the final record so stdout stays one JSON document.
    let final_record = match wait_until_terminal(client, &experiment.id).await {
        Ok(record) => record,
        Err(e) => return write_transport_error(&e),
    };
    let out = output::render_experiment(&final_record, &global.output, SystemTime::now());
    // A closed pipe here trumps the outcome code: report the contract exit, not the outcome.
    if let Err(code) = write_line(&mut std::io::stdout().lock(), &out) {
        return code;
    }
    outcome_exit_code(final_record.outcome.as_deref())
}

/// Poll the experiment until the master reports a terminal outcome. The master
/// resolves every record by its deadline, so this loop always ends unless the
/// master itself becomes unreachable.
async fn wait_until_terminal(client: &Client, id: &str) -> Result<Experiment, ApiError> {
    loop {
        let experiment = client.get_experiment(id).await?;
        if experiment.is_terminal() {
            return Ok(experiment);
        }
        tokio::time::sleep(WAIT_POLL_INTERVAL).await;
    }
}

/// Read an experiment definition file (YAML or JSON — YAML is a superset, so
/// one parser covers both) into the JSON value the management API expects.
fn read_definition(file: &Path) -> Result<serde_json::Value, String> {
    let text = std::fs::read_to_string(file)
        .map_err(|e| format!("could not read {}: {e}", file.display()))?;
    serde_norway::from_str::<serde_json::Value>(&text)
        .map_err(|e| format!("could not parse {}: {e}", file.display()))
}

/// The exit code for a waited experiment's terminal outcome.
fn outcome_exit_code(outcome: Option<&str>) -> i32 {
    match outcome {
        Some("COMPLETED") => EXIT_OK,
        Some("ABORTED") => EXIT_EXPERIMENT_ABORTED,
        Some("ERROR") => EXIT_EXPERIMENT_ERROR,
        // A terminal record always carries an outcome; anything else is a
        // master/CLI version mismatch worth surfacing as a plain error.
        _ => EXIT_ERROR,
    }
}

/// Report an accepted action on stdout: a small JSON object for scripting, a
/// sentence for humans. Returns the exit code (see [`emit`]).
fn print_action(format: &OutputFormat, action: &str, subject: &str, note: &str) -> i32 {
    let line = match format {
        OutputFormat::Json => {
            serde_json::json!({"status": action, "subject": subject, "note": note}).to_string()
        }
        OutputFormat::Table => format!("{action}: {subject} ({note})"),
    };
    emit(&line)
}

/// Write `line` (plus a newline) to a locked stdout, returning the exit code.
///
/// Thin shell over [`write_line`] for the common case where a successful write
/// means [`EXIT_OK`].
fn emit(line: &str) -> i32 {
    match write_line(&mut std::io::stdout().lock(), line) {
        Ok(()) => EXIT_OK,
        Err(code) => code,
    }
}

/// Write `line` followed by a newline to `w`, mapping the outcome to an exit code.
///
/// A closed downstream reader (`… | head`) surfaces as [`std::io::ErrorKind::BrokenPipe`];
/// that is the normal end of a pipeline, so it maps to a silent [`EXIT_OK`] instead of the
/// panic `println!` raises (issue #22 — the exit-code contract must survive a closed pipe).
/// Any other write failure maps to [`EXIT_ERROR`].
fn write_line(w: &mut impl Write, line: &str) -> Result<(), i32> {
    match writeln!(w, "{line}") {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Err(EXIT_OK),
        Err(_) => Err(EXIT_ERROR),
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

#[cfg(test)]
mod tests {
    use super::*;

    struct AlwaysFails(std::io::ErrorKind);

    impl Write for AlwaysFails {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(self.0))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::from(self.0))
        }
    }

    #[test]
    fn write_line_appends_newline_on_success() {
        let mut buf: Vec<u8> = Vec::new();
        assert_eq!(write_line(&mut buf, "hello"), Ok(()));
        assert_eq!(buf, b"hello\n");
    }

    #[test]
    fn write_line_maps_broken_pipe_to_ok_not_panic() {
        let mut sink = AlwaysFails(std::io::ErrorKind::BrokenPipe);
        assert_eq!(write_line(&mut sink, "anything"), Err(EXIT_OK));
    }

    #[test]
    fn write_line_maps_other_io_errors_to_error_exit() {
        let mut sink = AlwaysFails(std::io::ErrorKind::PermissionDenied);
        assert_eq!(write_line(&mut sink, "anything"), Err(EXIT_ERROR));
    }

    #[test]
    fn outcome_exit_codes_are_distinct() {
        assert_eq!(outcome_exit_code(Some("COMPLETED")), EXIT_OK);
        assert_eq!(outcome_exit_code(Some("ABORTED")), EXIT_EXPERIMENT_ABORTED);
        assert_eq!(outcome_exit_code(Some("ERROR")), EXIT_EXPERIMENT_ERROR);
        assert_eq!(outcome_exit_code(None), EXIT_ERROR);
        let codes = [
            EXIT_OK,
            EXIT_ERROR,
            EXIT_NOT_FOUND,
            EXIT_VALIDATION,
            EXIT_EXPERIMENT_ABORTED,
            EXIT_EXPERIMENT_ERROR,
        ];
        let unique: std::collections::HashSet<_> = codes.iter().collect();
        assert_eq!(unique.len(), codes.len());
    }

    #[test]
    fn read_definition_accepts_yaml_and_json() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = dir.path().join("exp.yaml");
        std::fs::write(
            &yaml,
            "name: it\nactions:\n  - hosts: [web-01]\n    plugin: {name: fixture, version: \"1\"}\n    params: {}\n    duration_secs: 5\n",
        )
        .unwrap();
        let value = read_definition(&yaml).unwrap();
        assert_eq!(value["actions"][0]["hosts"][0], "web-01");
        assert_eq!(value["actions"][0]["duration_secs"], 5);

        let json = dir.path().join("exp.json");
        std::fs::write(&json, r#"{"name":"it","actions":[]}"#).unwrap();
        let value = read_definition(&json).unwrap();
        assert_eq!(value["name"], "it");
    }

    #[test]
    fn read_definition_reports_missing_file_and_bad_syntax() {
        let dir = tempfile::tempdir().unwrap();
        let missing = read_definition(&dir.path().join("nope.yaml")).unwrap_err();
        assert!(missing.contains("could not read"));

        let bad = dir.path().join("bad.yaml");
        std::fs::write(&bad, "{ not: [valid").unwrap();
        let err = read_definition(&bad).unwrap_err();
        assert!(err.contains("could not parse"));
    }
}
