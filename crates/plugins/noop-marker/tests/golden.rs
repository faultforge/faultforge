//! Golden tests (Phase 5): drive the built `noop-marker` binary end to end —
//! stdin JSON in, NDJSON + exit code + filesystem effects out.
//!
//! Permission-based cases (`exit 10`/`20`/`30`) are skipped under root, which
//! ignores the permission bits they rely on.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use faultforge_fault::{EventBody, InstanceState, Phase, PluginEvent, PluginInput};

// ===== 5.1 test harness =====

/// The result of one plugin invocation.
struct Run {
    events: Vec<PluginEvent>,
    exit: Option<i32>,
}

/// Spawn the built binary for `command`, feed `input_json` on stdin, and collect
/// the parsed NDJSON events and exit code. Every stdout line must parse as a
/// [`PluginEvent`] (this is the 5.6 contract-shape gate).
fn run(command: &str, input_json: &str) -> Run {
    let mut child = Command::new(env!("CARGO_BIN_EXE_noop-marker"))
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn noop-marker");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(input_json.as_bytes())
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait for noop-marker");
    let stdout = String::from_utf8(output.stdout).expect("stdout is utf-8");
    let events = stdout
        .lines()
        .map(|line| {
            serde_json::from_str::<PluginEvent>(line)
                .unwrap_or_else(|e| panic!("stdout line is not a PluginEvent: {line:?}: {e}"))
        })
        .collect();
    Run {
        events,
        exit: output.status.code(),
    }
}

/// Build a stdin `PluginInput` JSON string by serializing the shared contract
/// type, so the tests exercise the exact wire shape and any path (quotes,
/// backslashes) is JSON-escaped correctly.
fn input(marker_path: &str, phase: &str) -> String {
    let phase = match phase {
        "install" => Phase::Install,
        "runtime" => Phase::Runtime,
        other => panic!("unknown phase in test input: {other}"),
    };
    let params = serde_json::json!({ "marker_path": marker_path })
        .as_object()
        .unwrap()
        .clone();
    let input = PluginInput {
        instance_id: "exp1-web01-0".into(),
        params,
        deadline_unix: 1_700_000_600,
        phase,
    };
    serde_json::to_string(&input).unwrap()
}

/// The status states from a run, in emission order (log lines dropped).
fn states(run: &Run) -> Vec<InstanceState> {
    run.events
        .iter()
        .filter_map(|e| match e.body {
            EventBody::Status { state } => Some(state),
            EventBody::Log { .. } => None,
        })
        .collect()
}

/// True if the current process is uid 0. Permission-based cases are meaningless
/// under root, which bypasses the bits.
fn running_as_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .is_some_and(|s| s.trim() == "0")
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

// ===== 5.2 preflight =====

#[test]
fn preflight_succeeds_in_both_phases_and_leaves_no_marker() {
    for phase in ["install", "runtime"] {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker.json");
        let r = run("preflight", &input(marker.to_str().unwrap(), phase));
        assert_eq!(r.exit, Some(0), "phase {phase}");
        assert_eq!(states(&r), vec![InstanceState::Preflight], "phase {phase}");
        assert!(
            !marker.exists(),
            "preflight must not create the marker ({phase})"
        );
    }
}

#[test]
fn preflight_relative_path_is_precondition_failure() {
    let r = run("preflight", &input("relative/marker.json", "runtime"));
    assert_eq!(r.exit, Some(10));
    assert!(states(&r).is_empty());
    assert!(matches!(
        r.events.first().map(|e| &e.body),
        Some(EventBody::Log { .. })
    ));
}

#[test]
#[cfg(unix)]
fn preflight_unwritable_dir_is_precondition_failure() {
    if running_as_root() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("locked");
    std::fs::create_dir(&sub).unwrap();
    let marker = sub.join("marker.json");
    set_mode(&sub, 0o000);
    let r = run("preflight", &input(marker.to_str().unwrap(), "runtime"));
    set_mode(&sub, 0o755); // restore so the tempdir can be cleaned up
    assert_eq!(r.exit, Some(10));
}

// ===== 5.3 inject =====

#[test]
fn inject_writes_the_marker_with_contract_content() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker.json");
    let marker_str = marker.to_str().unwrap();
    let r = run("inject", &input(marker_str, "runtime"));

    assert_eq!(r.exit, Some(0));
    assert_eq!(
        states(&r),
        vec![InstanceState::Injecting, InstanceState::Active],
        "INJECTING must precede ACTIVE"
    );

    let content = std::fs::read_to_string(&marker).unwrap();
    let v: serde_json::Value = serde_json::from_str(&content).expect("marker is parseable JSON");
    assert_eq!(v["instance_id"], "exp1-web01-0");
    assert_eq!(v["plugin"], "noop-marker@1");
    assert_eq!(v["deadline_unix"], 1_700_000_600_i64);
    assert_eq!(v["params"]["marker_path"], marker_str);
    assert!(
        v["written_at_unix"].is_i64(),
        "written_at_unix must be present"
    );
}

#[test]
fn inject_with_relative_path_is_rejected_and_writes_nothing() {
    // Defense in depth: only preflight is contractually required to check the
    // path, but inject must never resolve a relative marker_path against the cwd.
    let r = run("inject", &input("relative/marker.json", "runtime"));
    assert_ne!(r.exit, Some(0), "a relative marker_path must not inject");
    assert!(
        !states(&r).contains(&InstanceState::Active),
        "no ACTIVE status for a rejected inject"
    );
}

#[test]
#[cfg(unix)]
fn inject_into_unwritable_dir_aborts_with_no_partial_marker() {
    if running_as_root() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("locked");
    std::fs::create_dir(&sub).unwrap();
    let marker = sub.join("marker.json");
    set_mode(&sub, 0o000);
    let r = run("inject", &input(marker.to_str().unwrap(), "runtime"));
    set_mode(&sub, 0o755); // restore before asserting

    assert_eq!(r.exit, Some(20));
    assert!(!marker.exists(), "no partial marker may remain");
    assert!(
        r.events
            .iter()
            .any(|e| matches!(e.body, EventBody::Log { .. })),
        "a failed inject must emit a log line naming the reason"
    );
}

// ===== 5.4 report =====

#[test]
fn report_present_marker_is_active_and_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker.json");
    run("inject", &input(marker.to_str().unwrap(), "runtime"));
    let before = std::fs::read(&marker).unwrap();

    let r = run("report", &input(marker.to_str().unwrap(), "runtime"));
    assert_eq!(r.exit, Some(0));
    assert_eq!(states(&r), vec![InstanceState::Active]);

    let after = std::fs::read(&marker).unwrap();
    assert_eq!(before, after, "report must not modify the marker");
}

#[test]
fn report_absent_marker_is_done() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker.json");
    let r = run("report", &input(marker.to_str().unwrap(), "runtime"));
    assert_eq!(r.exit, Some(0));
    assert_eq!(states(&r), vec![InstanceState::Done]);
}

#[test]
fn report_corrupt_marker_is_nonzero_and_leaves_file_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker.json");
    let corrupt = b"this is not json {";
    std::fs::write(&marker, corrupt).unwrap();

    let r = run("report", &input(marker.to_str().unwrap(), "runtime"));
    assert_ne!(r.exit, Some(0), "corrupt marker must exit non-zero");
    assert!(states(&r).is_empty());
    assert!(
        r.events
            .iter()
            .any(|e| matches!(e.body, EventBody::Log { .. })),
        "a corrupt marker must emit a log line naming the reason"
    );

    let after = std::fs::read(&marker).unwrap();
    assert_eq!(after, corrupt, "report must not modify a corrupt marker");
}

// ===== 5.5 abort / cleanup =====

#[test]
fn abort_removes_the_marker_via_recovering_done() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker.json");
    run("inject", &input(marker.to_str().unwrap(), "runtime"));
    assert!(
        marker.exists(),
        "setup: inject must create the marker to remove"
    );

    let r = run("abort", &input(marker.to_str().unwrap(), "runtime"));
    assert_eq!(r.exit, Some(0));
    assert_eq!(
        states(&r),
        vec![InstanceState::Recovering, InstanceState::Done]
    );
    assert!(!marker.exists(), "abort must remove the marker");
}

#[test]
fn cleanup_twice_is_success_twice() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker.json");
    let in_json = input(marker.to_str().unwrap(), "runtime");
    run("inject", &in_json);
    assert!(
        marker.exists(),
        "setup: inject must create the marker to remove"
    );

    let first = run("cleanup", &in_json);
    assert_eq!(first.exit, Some(0));
    assert!(!marker.exists());

    let second = run("cleanup", &in_json);
    assert_eq!(second.exit, Some(0), "cleanup is idempotent");
    assert_eq!(
        states(&second),
        vec![InstanceState::Recovering, InstanceState::Done]
    );
}

#[test]
#[cfg(unix)]
fn cleanup_with_unremovable_marker_exits_30() {
    if running_as_root() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("locked");
    std::fs::create_dir(&sub).unwrap();
    let marker = sub.join("marker.json");
    let in_json = input(marker.to_str().unwrap(), "runtime");
    run("inject", &in_json);
    assert!(marker.exists(), "setup: marker must exist before locking");

    set_mode(&sub, 0o000); // forbid removal from the directory
    let r = run("cleanup", &in_json);
    set_mode(&sub, 0o755); // restore before asserting / cleanup

    assert_eq!(r.exit, Some(30));
    assert!(
        r.events
            .iter()
            .any(|e| matches!(e.body, EventBody::Log { .. })),
        "a failed cleanup must emit a log line naming the reason"
    );
}

// ===== 5.6 contract shape =====

#[test]
fn every_emitted_line_carries_ts_and_instance_id() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker.json");
    // inject emits two status lines; the harness already proved each parses as a
    // PluginEvent — here we assert the common fields are populated.
    let r = run("inject", &input(marker.to_str().unwrap(), "runtime"));
    assert!(!r.events.is_empty());
    for event in &r.events {
        assert_eq!(event.instance_id.as_str(), "exp1-web01-0");
        assert!(
            event.ts.ends_with('Z'),
            "ts must be RFC3339 UTC: {}",
            event.ts
        );
        assert!(!event.ts.is_empty());
    }
}
