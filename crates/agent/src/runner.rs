//! The plugin process runner: a sync IO shell around one invocation of
//! `<entrypoint> <command>` (design D7).
//!
//! The runner owns process mechanics only — spawn, stdin feed + EOF, NDJSON
//! stdout split into well-formed/malformed lines, stderr capture, a hard
//! timeout, exit classification. What an outcome *means* for the instance is
//! decided by [`crate::machine`], never here.

use std::io::{Read, Write as _};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use faultforge_fault::protocol::{Disposition, PluginCommand, PluginEvent, PluginInput};

/// One line of plugin stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdoutLine {
    /// A well-formed protocol event; `raw` is forwarded verbatim to the master.
    Event {
        /// The exact line as printed, unmodified.
        raw: String,
        /// Its parsed form (used to detect agent-owned state assertions).
        event: PluginEvent,
    },
    /// A line that did not parse as a protocol event — telemetry, never fatal.
    Malformed {
        /// The exact line as printed.
        raw: String,
    },
}

/// How the invocation ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvocationOutcome {
    /// The process exited with a code; classified per the contract table.
    Exited(Disposition),
    /// The process outlived the hard timeout and was killed.
    TimedOut,
    /// The process was terminated by a signal (no exit code).
    Signalled,
    /// The process could not be spawned or its pipes failed.
    Failed(String),
}

/// Everything one invocation produced.
#[derive(Debug)]
pub struct Invocation {
    /// Stdout, split line-by-line into well-formed events and malformed noise.
    pub lines: Vec<StdoutLine>,
    /// Captured stderr, in full.
    pub stderr: String,
    /// How the process ended.
    pub outcome: InvocationOutcome,
}

/// How often the wait loop polls `try_wait` while enforcing the timeout.
const WAIT_POLL: Duration = Duration::from_millis(25);

/// Run `<entrypoint> <command>` to completion: write `input` as one JSON object
/// to stdin and close it, capture stdout/stderr, enforce `timeout` (kill on
/// expiry), and classify the exit code.
///
/// Never returns an error: every failure mode is an [`InvocationOutcome`], so
/// the caller has exactly one result shape to turn into a machine event.
#[must_use]
pub fn invoke(
    entrypoint: &Path,
    command: PluginCommand,
    input: &PluginInput,
    timeout: Duration,
) -> Invocation {
    let stdin_payload = match serde_json::to_vec(input) {
        Ok(bytes) => bytes,
        Err(e) => return failed(format!("could not encode plugin input: {e}")),
    };

    let mut child = match Command::new(entrypoint)
        .arg(command.as_str())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return failed(format!("spawn failed: {e}")),
    };

    // A plugin may exit without reading stdin (e.g. a bad argv); a broken pipe
    // here is the plugin's business, not an invocation failure.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&stdin_payload);
        // Dropping stdin sends EOF — the contract's "input is complete" signal.
    }

    // Readers must run while we wait: a plugin that fills a pipe buffer would
    // otherwise deadlock against our wait loop.
    let stdout_reader = spawn_pipe_reader(child.stdout.take());
    let stderr_reader = spawn_pipe_reader(child.stderr.take());

    let status = wait_with_timeout(&mut child, timeout);

    let stdout = finish_pipe_reader(stdout_reader);
    let stderr = finish_pipe_reader(stderr_reader);

    Invocation {
        lines: split_stdout(&stdout),
        stderr,
        outcome: status,
    }
}

fn failed(reason: String) -> Invocation {
    Invocation {
        lines: vec![],
        stderr: String::new(),
        outcome: InvocationOutcome::Failed(reason),
    }
}

/// A pipe being drained in a background thread. The buffer is shared so the
/// caller can take a snapshot even when EOF never arrives — an orphaned
/// grandchild of a killed plugin can hold the pipe open indefinitely, and the
/// runner must not block on it.
struct PipeCapture {
    buf: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    eof_rx: std::sync::mpsc::Receiver<()>,
}

/// How long to wait for pipe EOF after the child has been reaped. Generous for
/// a well-behaved plugin (EOF is immediate once every writer is gone) and short
/// enough that a pipe held open by an orphan cannot wedge the instance.
const PIPE_EOF_GRACE: Duration = Duration::from_millis(500);

fn spawn_pipe_reader(pipe: Option<impl Read + Send + 'static>) -> Option<PipeCapture> {
    pipe.map(|mut pipe| {
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (eof_tx, eof_rx) = std::sync::mpsc::channel();
        let writer = std::sync::Arc::clone(&buf);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut guard = writer
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        guard.extend_from_slice(&chunk[..n]);
                    }
                }
            }
            let _ = eof_tx.send(());
        });
        PipeCapture { buf, eof_rx }
    })
}

fn finish_pipe_reader(capture: Option<PipeCapture>) -> String {
    let Some(capture) = capture else {
        return String::new();
    };
    let _ = capture.eof_rx.recv_timeout(PIPE_EOF_GRACE);
    let bytes = capture
        .buf
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> InvocationOutcome {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return status.code().map_or(InvocationOutcome::Signalled, |code| {
                    InvocationOutcome::Exited(faultforge_fault::protocol::classify_exit(code))
                });
            }
            Ok(None) => {
                if started.elapsed() >= timeout {
                    // Kill and reap; a kill error means the process is already
                    // gone, which the final wait resolves either way.
                    let _ = child.kill();
                    let _ = child.wait();
                    return InvocationOutcome::TimedOut;
                }
                std::thread::sleep(WAIT_POLL);
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return InvocationOutcome::Failed(format!("wait failed: {e}"));
            }
        }
    }
}

fn split_stdout(stdout: &str) -> Vec<StdoutLine> {
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| match serde_json::from_str::<PluginEvent>(line) {
            Ok(event) => StdoutLine::Event {
                raw: line.to_string(),
                event,
            },
            Err(_) => StdoutLine::Malformed {
                raw: line.to_string(),
            },
        })
        .collect()
}

// ===== Unit tests =====

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn input() -> PluginInput {
        PluginInput {
            instance_id: "test-1".into(),
            params: serde_json::Map::new(),
            deadline_unix: 1_700_000_600,
            phase: faultforge_fault::protocol::Phase::Runtime,
        }
    }

    /// Write an executable `#!/bin/sh` fixture script and return its path.
    fn script(dir: &Path, body: &str) -> std::path::PathBuf {
        let path = dir.join("plugin.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn run(body: &str) -> Invocation {
        let dir = tempfile::tempdir().unwrap();
        let entrypoint = script(dir.path(), body);
        invoke(
            &entrypoint,
            PluginCommand::Inject,
            &input(),
            Duration::from_secs(5),
        )
    }

    #[test]
    fn success_exit_and_ndjson_lines_are_captured() {
        let inv = run(concat!(
            r#"echo '{"ts":"2026-07-04T00:00:00Z","instance_id":"test-1","type":"status","state":"ACTIVE"}'"#,
            "\nexit 0",
        ));
        assert_eq!(inv.outcome, InvocationOutcome::Exited(Disposition::Success));
        assert_eq!(inv.lines.len(), 1);
        assert!(matches!(&inv.lines[0], StdoutLine::Event { event, .. }
            if matches!(event.body, faultforge_fault::protocol::EventBody::Status { .. })));
    }

    #[test]
    fn raw_line_is_preserved_verbatim() {
        let line = r#"{"ts":"2026-07-04T00:00:00Z","instance_id":"test-1","type":"log","level":"info","msg":"hi"}"#;
        let inv = run(&format!("echo '{line}'"));
        match &inv.lines[0] {
            StdoutLine::Event { raw, .. } => assert_eq!(raw, line),
            other @ StdoutLine::Malformed { .. } => panic!("expected event, got {other:?}"),
        }
    }

    #[test]
    fn malformed_lines_are_split_out_not_fatal() {
        let inv = run(concat!(
            "echo 'this is not json'\n",
            r#"echo '{"ts":"2026-07-04T00:00:00Z","instance_id":"test-1","type":"log","level":"info","msg":"ok"}'"#,
            "\nexit 0",
        ));
        assert_eq!(inv.outcome, InvocationOutcome::Exited(Disposition::Success));
        assert!(
            matches!(&inv.lines[0], StdoutLine::Malformed { raw } if raw == "this is not json")
        );
        assert!(matches!(&inv.lines[1], StdoutLine::Event { .. }));
    }

    #[test]
    fn stderr_is_captured_and_does_not_decide_success() {
        let inv = run("echo 'warning noise' >&2\nexit 0");
        assert_eq!(inv.outcome, InvocationOutcome::Exited(Disposition::Success));
        assert_eq!(inv.stderr.trim(), "warning noise");
    }

    #[test]
    fn contract_exit_codes_classify() {
        assert_eq!(
            run("exit 10").outcome,
            InvocationOutcome::Exited(Disposition::PreflightFailed)
        );
        assert_eq!(
            run("exit 20").outcome,
            InvocationOutcome::Exited(Disposition::InjectFailed)
        );
        assert_eq!(
            run("exit 30").outcome,
            InvocationOutcome::Exited(Disposition::RecoveryFailed)
        );
        assert_eq!(
            run("exit 7").outcome,
            InvocationOutcome::Exited(Disposition::Unexpected(7))
        );
    }

    #[test]
    fn stdin_json_reaches_the_plugin_and_eof_terminates_it() {
        // `cat` echoes stdin and exits only at EOF — proving both the payload
        // delivery and the close-after-write contract.
        let inv = run("cat");
        assert_eq!(inv.outcome, InvocationOutcome::Exited(Disposition::Success));
        // The whole input is one malformed "line" from the protocol's viewpoint
        // (it is PluginInput, not PluginEvent) — that is fine for this test.
        let all: String = inv
            .lines
            .iter()
            .map(|l| match l {
                StdoutLine::Event { raw, .. } | StdoutLine::Malformed { raw } => raw.as_str(),
            })
            .collect();
        assert!(all.contains("\"instance_id\":\"test-1\""));
        assert!(all.contains("\"deadline_unix\":1700000600"));
    }

    #[test]
    fn hung_plugin_is_killed_on_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let entrypoint = script(dir.path(), "sleep 60");
        let started = Instant::now();
        let inv = invoke(
            &entrypoint,
            PluginCommand::Inject,
            &input(),
            Duration::from_millis(200),
        );
        assert_eq!(inv.outcome, InvocationOutcome::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn missing_entrypoint_is_a_failed_invocation() {
        let inv = invoke(
            Path::new("/nonexistent/plugin"),
            PluginCommand::Inject,
            &input(),
            Duration::from_secs(1),
        );
        assert!(matches!(inv.outcome, InvocationOutcome::Failed(_)));
    }

    #[test]
    fn plugin_that_ignores_stdin_still_runs() {
        // Exits without reading stdin: the broken pipe must not fail the invocation.
        let inv = run("exit 0");
        assert_eq!(inv.outcome, InvocationOutcome::Exited(Disposition::Success));
    }
}
