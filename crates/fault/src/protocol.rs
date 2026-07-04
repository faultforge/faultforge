//! The agent↔plugin invocation protocol: commands, stdin input, NDJSON events,
//! and the exit-code table.

use serde::{Deserialize, Serialize};

use crate::state::InstanceState;

// ===== Instance identifier =====

/// A master-minted fault-instance identifier.
///
/// Opaque in v1 — the master owns its format (see the dispatch change) — but
/// wrapped per CONVENTIONS §7 so a call site cannot silently pass some other
/// string (for example a timestamp) where an instance id is expected. The wire
/// representation is a plain JSON string (`#[serde(transparent)]`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InstanceId(String);

impl InstanceId {
    /// The identifier as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for InstanceId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for InstanceId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl std::fmt::Display for InstanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ===== Commands (argv) =====

/// A lifecycle command, invoked as the single argv word `<entrypoint> <command>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginCommand {
    /// Validate preconditions without touching the host.
    Preflight,
    /// Apply the fault.
    Inject,
    /// Report the current instance state (read-only reconciliation probe).
    Report,
    /// Fast revert ("stop now, save the host").
    Abort,
    /// Orderly, idempotent revert.
    Cleanup,
}

/// Error returned when an argv word does not name a known [`PluginCommand`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("unknown plugin command: {0}")]
pub struct UnknownCommand(pub String);

impl PluginCommand {
    /// The argv word for this command.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::Inject => "inject",
            Self::Report => "report",
            Self::Abort => "abort",
            Self::Cleanup => "cleanup",
        }
    }

    /// Parse an argv word into a command.
    ///
    /// # Errors
    ///
    /// Returns [`UnknownCommand`] if `s` is not one of the five lifecycle words.
    pub fn parse(s: &str) -> Result<Self, UnknownCommand> {
        match s {
            "preflight" => Ok(Self::Preflight),
            "inject" => Ok(Self::Inject),
            "report" => Ok(Self::Report),
            "abort" => Ok(Self::Abort),
            "cleanup" => Ok(Self::Cleanup),
            other => Err(UnknownCommand(other.to_string())),
        }
    }
}

impl std::fmt::Display for PluginCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ===== stdin input =====

/// Which preflight phase an invocation belongs to.
///
/// Meaningful only for `preflight`; every other command receives `Runtime`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// The install-time static check.
    Install,
    /// The runtime check immediately before injection, and all other commands.
    Runtime,
}

/// The single JSON object every invocation receives on stdin.
///
/// Parsing is lenient (unknown fields are ignored) so a plugin built against this
/// version keeps working if a later agent adds fields; the "exactly these fields"
/// requirement binds the sending agent, not the receiving plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInput {
    /// The master-minted instance identifier.
    pub instance_id: InstanceId,
    /// Parameters, already validated against the manifest `params_schema` by the agent.
    pub params: serde_json::Map<String, serde_json::Value>,
    /// Absolute dead-man deadline (`start + duration + grace`), unix seconds. The
    /// plugin MAY record this but must not schedule its own actions against it.
    pub deadline_unix: i64,
    /// The preflight phase; `Runtime` for all non-preflight commands.
    pub phase: Phase,
}

// ===== stdout NDJSON events =====

/// Severity of a `log` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Informational.
    Info,
    /// Warning.
    Warn,
    /// Error (also the level under which captured stderr is surfaced).
    Error,
}

/// The variant-specific body of a [`PluginEvent`], tagged by the `type` field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum EventBody {
    /// A lifecycle state transition.
    Status {
        /// The state the plugin is asserting. Only [`InstanceState::plugin_emittable`]
        /// states drive the lifecycle; others are treated as telemetry only.
        state: InstanceState,
    },
    /// A free-text log line.
    Log {
        /// Severity.
        level: Level,
        /// Human-readable message.
        msg: String,
    },
}

/// One line of a plugin's NDJSON stdout.
///
/// `ts` and `instance_id` are common to every line; the `type`-tagged [`EventBody`]
/// is flattened alongside them, yielding e.g.
/// `{"ts":"…","instance_id":"…","type":"status","state":"ACTIVE"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginEvent {
    /// RFC3339 UTC timestamp (e.g. `2026-07-03T12:00:00Z`).
    pub ts: String,
    /// The instance this line belongs to; every line carries it so the agent can
    /// tag and forward telemetry.
    pub instance_id: InstanceId,
    /// The `status` or `log` payload.
    #[serde(flatten)]
    pub body: EventBody,
}

impl PluginEvent {
    /// Build a `status` event asserting `state`.
    pub fn status(
        ts: impl Into<String>,
        instance_id: impl Into<InstanceId>,
        state: InstanceState,
    ) -> Self {
        Self {
            ts: ts.into(),
            instance_id: instance_id.into(),
            body: EventBody::Status { state },
        }
    }

    /// Build a `log` event at `level` with `msg`.
    pub fn log(
        ts: impl Into<String>,
        instance_id: impl Into<InstanceId>,
        level: Level,
        msg: impl Into<String>,
    ) -> Self {
        Self {
            ts: ts.into(),
            instance_id: instance_id.into(),
            body: EventBody::Log {
                level,
                msg: msg.into(),
            },
        }
    }
}

// ===== exit codes =====

/// Exit code: the command succeeded.
pub const EXIT_SUCCESS: u8 = 0;
/// Exit code: a preflight precondition failed. The agent must not inject and must
/// report the precondition failure.
pub const EXIT_PREFLIGHT_FAILED: u8 = 10;
/// Exit code: inject failed. The instance transitions to `ABORTED` with the host
/// unaffected.
pub const EXIT_INJECT_FAILED: u8 = 20;
/// Exit code: cleanup/abort failed. The agent retries the idempotent operation
/// once and, if it fails again, marks the host `TAINTED`.
pub const EXIT_CLEANUP_FAILED: u8 = 30;

/// The agent's interpretation of a plugin's exit code — the single source of truth
/// for the exit-code table, used by plugins and the agent alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// [`EXIT_SUCCESS`].
    Success,
    /// [`EXIT_PREFLIGHT_FAILED`].
    PreflightFailed,
    /// [`EXIT_INJECT_FAILED`].
    InjectFailed,
    /// [`EXIT_CLEANUP_FAILED`].
    RecoveryFailed,
    /// Any other non-zero code — an unexpected failure of the current command.
    Unexpected(i32),
}

/// Classify a process exit code per the contract exit-code table.
///
/// Takes `i32` because the agent reads codes from `ExitStatus::code()`.
#[must_use]
pub fn classify_exit(code: i32) -> Disposition {
    if code == i32::from(EXIT_SUCCESS) {
        Disposition::Success
    } else if code == i32::from(EXIT_PREFLIGHT_FAILED) {
        Disposition::PreflightFailed
    } else if code == i32::from(EXIT_INJECT_FAILED) {
        Disposition::InjectFailed
    } else if code == i32::from(EXIT_CLEANUP_FAILED) {
        Disposition::RecoveryFailed
    } else {
        Disposition::Unexpected(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- commands -----

    #[test]
    fn command_argv_words_round_trip() {
        for cmd in [
            PluginCommand::Preflight,
            PluginCommand::Inject,
            PluginCommand::Report,
            PluginCommand::Abort,
            PluginCommand::Cleanup,
        ] {
            assert_eq!(PluginCommand::parse(cmd.as_str()).unwrap(), cmd);
        }
    }

    #[test]
    fn unknown_command_is_rejected() {
        assert_eq!(
            PluginCommand::parse("frobnicate"),
            Err(UnknownCommand("frobnicate".to_string()))
        );
    }

    // ----- input -----

    #[test]
    fn plugin_input_parses_the_spec_shape() {
        let line = r#"{"instance_id":"exp1-web01-0","params":{"marker_path":"/tmp/m"},"deadline_unix":1700000600,"phase":"install"}"#;
        let input: PluginInput = serde_json::from_str(line).unwrap();
        assert_eq!(input.instance_id.as_str(), "exp1-web01-0");
        assert_eq!(input.deadline_unix, 1_700_000_600);
        assert_eq!(input.phase, Phase::Install);
        assert_eq!(
            input.params.get("marker_path").and_then(|v| v.as_str()),
            Some("/tmp/m")
        );
    }

    #[test]
    fn plugin_input_ignores_unknown_fields() {
        // Forward-compatibility: a later agent may add fields.
        let line = r#"{"instance_id":"i","params":{},"deadline_unix":0,"phase":"runtime","future_field":true}"#;
        let input: PluginInput = serde_json::from_str(line).unwrap();
        assert_eq!(input.phase, Phase::Runtime);
    }

    // ----- events (round-trip against the exact spec lines) -----

    #[test]
    fn status_line_matches_spec_and_round_trips() {
        let event = PluginEvent::status(
            "2026-07-03T12:00:00Z",
            "exp1-web01-0",
            InstanceState::Active,
        );
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "ts": "2026-07-03T12:00:00Z",
                "instance_id": "exp1-web01-0",
                "type": "status",
                "state": "ACTIVE"
            })
        );
        let parsed: PluginEvent = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn log_line_matches_spec_and_round_trips() {
        let event = PluginEvent::log(
            "2026-07-03T12:00:00Z",
            "exp1-web01-0",
            Level::Error,
            "marker_path is not absolute",
        );
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "ts": "2026-07-03T12:00:00Z",
                "instance_id": "exp1-web01-0",
                "type": "log",
                "level": "error",
                "msg": "marker_path is not absolute"
            })
        );
        let parsed: PluginEvent = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn parsing_a_literal_status_line_from_stdout() {
        let line =
            r#"{"ts":"2026-07-03T12:00:00Z","instance_id":"i","type":"status","state":"DONE"}"#;
        let event: PluginEvent = serde_json::from_str(line).unwrap();
        assert_eq!(event.ts, "2026-07-03T12:00:00Z");
        assert_eq!(event.instance_id.as_str(), "i");
        assert_eq!(
            event.body,
            EventBody::Status {
                state: InstanceState::Done
            }
        );
    }

    // ----- exit-code table (one assertion per row) -----

    #[test]
    fn classify_exit_covers_every_row() {
        assert_eq!(classify_exit(0), Disposition::Success);
        assert_eq!(classify_exit(10), Disposition::PreflightFailed);
        assert_eq!(classify_exit(20), Disposition::InjectFailed);
        assert_eq!(classify_exit(30), Disposition::RecoveryFailed);
        assert_eq!(classify_exit(1), Disposition::Unexpected(1));
        assert_eq!(classify_exit(42), Disposition::Unexpected(42));
        assert_eq!(classify_exit(255), Disposition::Unexpected(255));
    }

    // ----- instance id -----

    #[test]
    fn instance_id_is_a_transparent_string() {
        let id = InstanceId::from("exp1-web01-0");
        assert_eq!(id.as_str(), "exp1-web01-0");
        assert_eq!(id.to_string(), "exp1-web01-0");
        // Transparent serde: the wire form is the bare string, not an object.
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"exp1-web01-0\"");
        let back: InstanceId = serde_json::from_str("\"exp1-web01-0\"").unwrap();
        assert_eq!(back, id);
    }
}
