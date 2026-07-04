//! `noop-marker`: the reference fault plugin.
//!
//! Its only host effect is the existence of a marker file at `params.marker_path`.
//! A failing run is therefore unambiguously a framework bug, never a `tc`/systemd
//! problem. See `manifest.yaml` and the `noop-marker-plugin` spec.
//!
//! The binary is a thin imperative shell: `main` reads `SystemTime::now()` once
//! and threads it into per-command logic; filesystem effects are confined to
//! `dirname(marker_path)` (the marker, its atomic-write temp file, and the
//! preflight probe file).

use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use faultforge_fault::{
    EXIT_CLEANUP_FAILED, EXIT_INJECT_FAILED, EXIT_PREFLIGHT_FAILED, EXIT_SUCCESS, InstanceId,
    InstanceState, Level, PluginCommand, PluginEvent, PluginInput,
};
use serde::Serialize;

/// The plugin identity written into the marker file (`<name>@<version>`).
const PLUGIN_IDENTITY: &str = "noop-marker@1";

/// Generic unexpected-failure exit code (an "other non-zero" per the contract).
/// Used when stdin/argv are malformed or a `report` finds a corrupt marker.
const EXIT_UNEXPECTED: u8 = 1;

/// The on-disk marker content.
#[derive(Debug, Serialize)]
struct Marker {
    /// The master-minted instance id.
    instance_id: InstanceId,
    /// Plugin identity, always [`PLUGIN_IDENTITY`].
    plugin: String,
    /// The dead-man deadline the agent computed (unix seconds).
    deadline_unix: i64,
    /// The parameters this instance was injected with.
    params: serde_json::Map<String, serde_json::Value>,
    /// When the marker was written (unix seconds).
    written_at_unix: i64,
}

/// What a command decided: the NDJSON events to emit and the process exit code.
struct Outcome {
    events: Vec<PluginEvent>,
    exit: u8,
}

fn main() -> ExitCode {
    // Time as data: read the wall clock once, thread it into pure logic.
    let now = SystemTime::now();

    let Some(word) = std::env::args().nth(1) else {
        eprintln!("usage: noop-marker <preflight|inject|report|abort|cleanup>");
        return ExitCode::from(EXIT_UNEXPECTED);
    };
    let command = match PluginCommand::parse(&word) {
        Ok(command) => command,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::from(EXIT_UNEXPECTED);
        }
    };

    let input = match read_input() {
        Ok(input) => input,
        Err(err) => {
            eprintln!("failed to read plugin input: {err}");
            return ExitCode::from(EXIT_UNEXPECTED);
        }
    };

    let outcome = run(command, &input, now);
    emit_all(&outcome.events);
    ExitCode::from(outcome.exit)
}

/// Read and parse the single stdin JSON [`PluginInput`].
fn read_input() -> Result<PluginInput, String> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&buf).map_err(|e| e.to_string())
}

/// Write every event to stdout as one NDJSON line, in order.
fn emit_all(events: &[PluginEvent]) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    for event in events {
        // Serializing these types cannot fail; if a line somehow can't be written
        // (e.g. a closed pipe), there is nothing useful the plugin can do.
        if let Ok(line) = serde_json::to_string(event) {
            let _ = writeln!(lock, "{line}");
        }
    }
}

/// Dispatch a command over its validated input and the captured time.
fn run(command: PluginCommand, input: &PluginInput, now: SystemTime) -> Outcome {
    let Some(marker_path) = input.params.get("marker_path").and_then(|v| v.as_str()) else {
        // In production the agent validates params against the schema first; a
        // missing/mistyped marker_path here is a contract violation, not a
        // precondition failure.
        return Outcome {
            events: vec![log(
                input,
                now,
                Level::Error,
                "params.marker_path missing or not a string",
            )],
            exit: EXIT_UNEXPECTED,
        };
    };
    let marker_path = Path::new(marker_path);

    // Defense in depth: every command requires an absolute marker_path. `preflight`
    // surfaces a relative path as a precondition failure (exit 10) below; if any
    // other command is invoked with a relative path the agent skipped preflight —
    // a contract violation — so refuse rather than resolve it against the process
    // cwd (which would write the marker somewhere unintended).
    if command != PluginCommand::Preflight && !marker_path.is_absolute() {
        return fail(
            input,
            now,
            EXIT_UNEXPECTED,
            format!("marker_path is not absolute: {}", marker_path.display()),
        );
    }

    match command {
        PluginCommand::Preflight => preflight(input, now, marker_path),
        PluginCommand::Inject => inject(input, now, marker_path),
        PluginCommand::Report => report(input, now, marker_path),
        // abort and cleanup share one idempotent revert (spec: abort MAY be the
        // same logic as cleanup).
        PluginCommand::Abort | PluginCommand::Cleanup => revert(input, now, marker_path),
    }
}

// ===== commands =====

/// Validate the path and directory writability without touching the marker.
fn preflight(input: &PluginInput, now: SystemTime, marker_path: &Path) -> Outcome {
    if !marker_path.is_absolute() {
        return fail(
            input,
            now,
            EXIT_PREFLIGHT_FAILED,
            format!("marker_path is not absolute: {}", marker_path.display()),
        );
    }
    let Some(dir) = marker_path.parent() else {
        return fail(
            input,
            now,
            EXIT_PREFLIGHT_FAILED,
            "marker_path has no parent directory".to_string(),
        );
    };
    // The plugin MAY create the missing marker directory itself — but only that
    // one directory, never deeper ancestors: the no-harm contract confines writes
    // to dirname(marker_path). `create_dir` (not `create_dir_all`) creates at most
    // the final component; an already-present directory is fine, and a missing
    // grandparent surfaces as a precondition failure rather than a wider mutation.
    match std::fs::create_dir(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
        Err(e) => {
            return fail(
                input,
                now,
                EXIT_PREFLIGHT_FAILED,
                format!("cannot create marker directory {}: {e}", dir.display()),
            );
        }
    }
    // Honest writability check: create-and-remove a probe file. Permission bits
    // lie under ACLs; an actual write does not. The probe is NOT the marker, so
    // preflight stays side-effect-free with respect to the marker file.
    if let Err(e) = tempfile::NamedTempFile::new_in(dir) {
        return fail(
            input,
            now,
            EXIT_PREFLIGHT_FAILED,
            format!("marker directory {} is not writable: {e}", dir.display()),
        );
    }
    Outcome {
        events: vec![status(input, now, InstanceState::Preflight)],
        exit: EXIT_SUCCESS,
    }
}

/// Atomically write the marker, then exit; the fault's active state is purely the
/// file's existence.
fn inject(input: &PluginInput, now: SystemTime, marker_path: &Path) -> Outcome {
    let mut events = vec![status(input, now, InstanceState::Injecting)];
    match write_marker(input, now, marker_path) {
        Ok(()) => {
            events.push(status(input, now, InstanceState::Active));
            Outcome {
                events,
                exit: EXIT_SUCCESS,
            }
        }
        Err(e) => {
            events.push(log(input, now, Level::Error, format!("inject failed: {e}")));
            Outcome {
                events,
                exit: EXIT_INJECT_FAILED,
            }
        }
    }
}

/// Read-only reconciliation probe; mutates nothing.
fn report(input: &PluginInput, now: SystemTime, marker_path: &Path) -> Outcome {
    match std::fs::read(marker_path) {
        Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(_) => Outcome {
                events: vec![status(input, now, InstanceState::Active)],
                exit: EXIT_SUCCESS,
            },
            // Present but corrupt: ambiguous. Exit non-zero, leave the file alone.
            Err(e) => fail(
                input,
                now,
                EXIT_UNEXPECTED,
                format!("marker present but not parseable JSON: {e}"),
            ),
        },
        Err(e) if e.kind() == ErrorKind::NotFound => Outcome {
            events: vec![status(input, now, InstanceState::Done)],
            exit: EXIT_SUCCESS,
        },
        Err(e) => fail(
            input,
            now,
            EXIT_UNEXPECTED,
            format!("cannot read marker: {e}"),
        ),
    }
}

/// Remove the marker; idempotent (already-absent is success). Shared by `abort`
/// and `cleanup`.
fn revert(input: &PluginInput, now: SystemTime, marker_path: &Path) -> Outcome {
    let mut events = vec![status(input, now, InstanceState::Recovering)];
    match std::fs::remove_file(marker_path) {
        // Removed, or already gone: either way the host is clean.
        Ok(()) => {
            events.push(status(input, now, InstanceState::Done));
            Outcome {
                events,
                exit: EXIT_SUCCESS,
            }
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {
            events.push(status(input, now, InstanceState::Done));
            Outcome {
                events,
                exit: EXIT_SUCCESS,
            }
        }
        // A real failure to remove an existing marker.
        Err(e) => {
            events.push(log(
                input,
                now,
                Level::Error,
                format!("failed to remove marker: {e}"),
            ));
            Outcome {
                events,
                exit: EXIT_CLEANUP_FAILED,
            }
        }
    }
}

// ===== helpers =====

/// Atomically write the marker: temp file in the target dir → write → fsync →
/// rename. On any error the temp file is dropped (removed), so no partial marker
/// remains at `marker_path`.
fn write_marker(input: &PluginInput, now: SystemTime, marker_path: &Path) -> std::io::Result<()> {
    let dir = marker_path.parent().ok_or_else(|| {
        std::io::Error::new(
            ErrorKind::InvalidInput,
            "marker_path has no parent directory",
        )
    })?;
    let marker = Marker {
        instance_id: input.instance_id.clone(),
        plugin: PLUGIN_IDENTITY.to_string(),
        deadline_unix: input.deadline_unix,
        params: input.params.clone(),
        written_at_unix: unix_secs(now),
    };
    let bytes = serde_json::to_vec(&marker).map_err(std::io::Error::other)?;

    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(&bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(marker_path).map_err(|e| e.error)?;
    Ok(())
}

/// Build a single-`log`-event failure outcome with the given exit code.
fn fail(input: &PluginInput, now: SystemTime, exit: u8, msg: String) -> Outcome {
    Outcome {
        events: vec![log(input, now, Level::Error, msg)],
        exit,
    }
}

/// Build a `status` event for this instance at `now`.
fn status(input: &PluginInput, now: SystemTime, state: InstanceState) -> PluginEvent {
    PluginEvent::status(ts(now), input.instance_id.clone(), state)
}

/// Build a `log` event for this instance at `now`.
fn log(input: &PluginInput, now: SystemTime, level: Level, msg: impl Into<String>) -> PluginEvent {
    PluginEvent::log(ts(now), input.instance_id.clone(), level, msg)
}

/// Format `now` as an RFC3339 UTC timestamp with second precision (`…Z`).
fn ts(now: SystemTime) -> String {
    humantime::format_rfc3339_seconds(now).to_string()
}

/// Seconds since the Unix epoch.
fn unix_secs(now: SystemTime) -> i64 {
    #[allow(clippy::expect_used)] // a host clock predating the Unix epoch is not a real scenario
    let secs = now
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_secs();
    // Seconds since the epoch fit in i64 for ~292 billion years.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let secs = secs as i64;
    secs
}
