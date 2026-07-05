//! Golden tests: drive the built `disk-fill` binary end to end — stdin JSON in,
//! NDJSON + exit code + filesystem effects out.
//!
//! Permission-based cases (`exit 10`/`30`) are skipped under root, which
//! ignores the permission bits they rely on. Forced mid-allocation failure
//! (ENOSPC) needs a size-capped filesystem and is exercised only by the pure
//! `allocation_backed`/`abandon_inject` unit coverage plus the e2e suite.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Command, Stdio};

use faultforge_fault::{EventBody, InstanceState, Level, Phase, PluginEvent, PluginInput};

const MIB: u64 = 1024 * 1024;

/// The instance id used across the tests — deliberately the production shape
/// `<experiment>:<hostname>:<index>`, so the `:` filename case is always on.
const INSTANCE_ID: &str = "exp1:web01:0";

/// The deterministic fill-file path the plugin derives for [`INSTANCE_ID`].
fn fill_path(dir: &Path) -> std::path::PathBuf {
    dir.join(format!("faultforge-{INSTANCE_ID}.fill"))
}

/// The result of one plugin invocation.
struct Run {
    events: Vec<PluginEvent>,
    exit: Option<i32>,
}

/// Spawn the built binary for `command`, feed `input_json` on stdin, and collect
/// the parsed NDJSON events and exit code. Every stdout line must parse as a
/// [`PluginEvent`] (the contract-shape gate).
fn run(command: &str, input_json: &str) -> Run {
    let mut child = Command::new(env!("CARGO_BIN_EXE_disk-fill"))
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn disk-fill");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(input_json.as_bytes())
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait for disk-fill");
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
/// type, so the tests exercise the exact wire shape.
fn input_for(instance_id: &str, fill_dir: &str, size_mib: i64, phase: Phase) -> String {
    let params = serde_json::json!({ "fill_dir": fill_dir, "size_mib": size_mib })
        .as_object()
        .unwrap()
        .clone();
    let input = PluginInput {
        instance_id: instance_id.into(),
        params,
        deadline_unix: 1_700_000_600,
        phase,
    };
    serde_json::to_string(&input).unwrap()
}

fn input(fill_dir: &str, size_mib: i64, phase: Phase) -> String {
    input_for(INSTANCE_ID, fill_dir, size_mib, phase)
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

/// True if the run emitted a `log` event at `level`.
fn has_log(run: &Run, level: Level) -> bool {
    run.events
        .iter()
        .any(|e| matches!(e.body, EventBody::Log { level: l, .. } if l == level))
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

// ===== preflight =====

#[test]
fn preflight_succeeds_in_both_phases_and_leaves_no_trace() {
    for phase in [Phase::Install, Phase::Runtime] {
        let dir = tempfile::tempdir().unwrap();
        let r = run("preflight", &input(dir.path().to_str().unwrap(), 1, phase));
        assert_eq!(r.exit, Some(0), "phase {phase:?}");
        assert_eq!(
            states(&r),
            vec![InstanceState::Preflight],
            "phase {phase:?}"
        );
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "preflight must leave nothing behind ({phase:?})"
        );
    }
}

#[test]
fn preflight_install_phase_is_static_only() {
    // A fill_dir that does not exist: runtime refuses, install does not care.
    let r = run(
        "preflight",
        &input("/nonexistent/faultforge-e2e", 1, Phase::Install),
    );
    assert_eq!(r.exit, Some(0));
    assert_eq!(states(&r), vec![InstanceState::Preflight]);
}

#[test]
fn preflight_missing_dir_is_runtime_precondition_failure() {
    let r = run(
        "preflight",
        &input("/nonexistent/faultforge-e2e", 1, Phase::Runtime),
    );
    assert_eq!(r.exit, Some(10));
    assert!(states(&r).is_empty());
    assert!(has_log(&r, Level::Error));
}

#[test]
fn preflight_relative_dir_is_precondition_failure() {
    for phase in [Phase::Install, Phase::Runtime] {
        let r = run("preflight", &input("relative/dir", 1, phase));
        assert_eq!(r.exit, Some(10), "phase {phase:?}");
        assert!(states(&r).is_empty());
    }
}

#[test]
fn preflight_nonpositive_size_is_precondition_failure() {
    let dir = tempfile::tempdir().unwrap();
    for size in [0, -3] {
        let r = run(
            "preflight",
            &input(dir.path().to_str().unwrap(), size, Phase::Install),
        );
        assert_eq!(r.exit, Some(10), "size {size}");
    }
}

#[test]
fn preflight_overflowing_size_is_precondition_failure() {
    let dir = tempfile::tempdir().unwrap();
    let r = run(
        "preflight",
        &input(dir.path().to_str().unwrap(), i64::MAX, Phase::Install),
    );
    assert_eq!(r.exit, Some(10));
}

#[test]
fn preflight_headroom_breach_is_refused() {
    // Petabytes: no test machine satisfies size + headroom, and the message
    // must name the shortfall rather than fail on arithmetic.
    let dir = tempfile::tempdir().unwrap();
    let r = run(
        "preflight",
        &input(dir.path().to_str().unwrap(), 1 << 40, Phase::Runtime),
    );
    assert_eq!(r.exit, Some(10));
    assert!(has_log(&r, Level::Error));
}

#[test]
#[cfg(unix)]
fn preflight_unwritable_dir_is_runtime_precondition_failure() {
    if running_as_root() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("locked");
    std::fs::create_dir(&sub).unwrap();
    set_mode(&sub, 0o555);
    let r = run(
        "preflight",
        &input(sub.to_str().unwrap(), 1, Phase::Runtime),
    );
    set_mode(&sub, 0o755); // restore so the tempdir can be cleaned up
    assert_eq!(r.exit, Some(10));
}

#[test]
fn preflight_preexisting_fill_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(fill_path(dir.path()), b"leftover").unwrap();
    let r = run(
        "preflight",
        &input(dir.path().to_str().unwrap(), 1, Phase::Runtime),
    );
    assert_eq!(r.exit, Some(10));
}

#[test]
#[cfg(target_os = "linux")]
fn preflight_on_tmpfs_warns_but_allows() {
    let shm = Path::new("/dev/shm");
    if !shm.is_dir() {
        eprintln!("skipping: /dev/shm not present");
        return;
    }
    let dir = tempfile::tempdir_in(shm).unwrap();
    let r = run(
        "preflight",
        &input(dir.path().to_str().unwrap(), 1, Phase::Runtime),
    );
    assert_eq!(r.exit, Some(0), "tmpfs is allowed");
    assert!(
        has_log(&r, Level::Warn),
        "tmpfs must be flagged: fill consumes memory, not disk"
    );
    assert_eq!(states(&r), vec![InstanceState::Preflight]);
}

// ===== inject =====

#[test]
fn inject_allocates_the_exact_size_backed_by_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let size_mib = 8_i64;
    let r = run(
        "inject",
        &input(dir.path().to_str().unwrap(), size_mib, Phase::Runtime),
    );
    assert_eq!(r.exit, Some(0));
    assert_eq!(
        states(&r),
        vec![InstanceState::Injecting, InstanceState::Active],
        "INJECTING must precede ACTIVE"
    );

    let meta = std::fs::metadata(fill_path(dir.path())).expect("fill file exists");
    let size = 8 * MIB;
    assert_eq!(meta.len(), size, "exact requested length");
    assert!(
        meta.blocks() * 512 >= size,
        "allocation must be backed by blocks, got {} of {size}",
        meta.blocks() * 512
    );
}

#[test]
fn inject_preexisting_file_is_refused_and_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = fill_path(dir.path());
    let foreign = b"not ours to clobber";
    std::fs::write(&path, foreign).unwrap();

    let r = run(
        "inject",
        &input(dir.path().to_str().unwrap(), 1, Phase::Runtime),
    );
    assert_eq!(r.exit, Some(20));
    assert!(!states(&r).contains(&InstanceState::Active));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        foreign,
        "a pre-existing file must survive a refused inject byte-identical"
    );
}

#[test]
#[cfg(unix)]
fn inject_into_unwritable_dir_fails_with_no_file() {
    if running_as_root() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("locked");
    std::fs::create_dir(&sub).unwrap();
    set_mode(&sub, 0o555);
    let r = run("inject", &input(sub.to_str().unwrap(), 1, Phase::Runtime));
    set_mode(&sub, 0o755); // restore before asserting

    assert_eq!(r.exit, Some(20));
    assert!(!fill_path(&sub).exists(), "no fill file may remain");
    assert!(has_log(&r, Level::Error));
}

#[test]
fn inject_with_traversal_hostile_id_is_rejected() {
    // The agent's boundary validation refuses such ids; this pins the
    // defense-in-depth layer behind it.
    let dir = tempfile::tempdir().unwrap();
    let r = run(
        "inject",
        &input_for("../evil", dir.path().to_str().unwrap(), 1, Phase::Runtime),
    );
    assert_ne!(r.exit, Some(0));
    assert!(!states(&r).contains(&InstanceState::Active));
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "nothing may be created for a hostile id"
    );
}

// ===== report =====

#[test]
fn report_present_fill_file_is_active_and_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let in_json = input(dir.path().to_str().unwrap(), 1, Phase::Runtime);
    run("inject", &in_json);
    let before = std::fs::metadata(fill_path(dir.path())).unwrap();

    let r = run("report", &in_json);
    assert_eq!(r.exit, Some(0));
    assert_eq!(states(&r), vec![InstanceState::Active]);

    let after = std::fs::metadata(fill_path(dir.path())).unwrap();
    assert_eq!(before.len(), after.len(), "report must not modify the file");
}

#[test]
fn report_absent_fill_file_is_done() {
    let dir = tempfile::tempdir().unwrap();
    let r = run(
        "report",
        &input(dir.path().to_str().unwrap(), 1, Phase::Runtime),
    );
    assert_eq!(r.exit, Some(0));
    assert_eq!(states(&r), vec![InstanceState::Done]);
}

// ===== abort / cleanup =====

#[test]
fn abort_removes_the_fill_file_via_recovering_done() {
    let dir = tempfile::tempdir().unwrap();
    let in_json = input(dir.path().to_str().unwrap(), 1, Phase::Runtime);
    run("inject", &in_json);
    assert!(fill_path(dir.path()).exists(), "setup: inject must fill");

    let r = run("abort", &in_json);
    assert_eq!(r.exit, Some(0));
    assert_eq!(
        states(&r),
        vec![InstanceState::Recovering, InstanceState::Done]
    );
    assert!(
        !fill_path(dir.path()).exists(),
        "abort must remove the file"
    );
}

#[test]
fn cleanup_twice_is_success_twice() {
    let dir = tempfile::tempdir().unwrap();
    let in_json = input(dir.path().to_str().unwrap(), 1, Phase::Runtime);
    run("inject", &in_json);
    assert!(fill_path(dir.path()).exists(), "setup: inject must fill");

    let first = run("cleanup", &in_json);
    assert_eq!(first.exit, Some(0));
    assert_eq!(states(&first), vec![InstanceState::Done]);
    assert!(!fill_path(dir.path()).exists());

    let second = run("cleanup", &in_json);
    assert_eq!(second.exit, Some(0), "cleanup is idempotent");
    assert_eq!(states(&second), vec![InstanceState::Done]);
}

#[test]
#[cfg(unix)]
fn cleanup_with_unremovable_fill_file_exits_30() {
    if running_as_root() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("locked");
    std::fs::create_dir(&sub).unwrap();
    let in_json = input(sub.to_str().unwrap(), 1, Phase::Runtime);
    run("inject", &in_json);
    assert!(fill_path(&sub).exists(), "setup: fill must exist");

    set_mode(&sub, 0o555); // forbid removal from the directory
    let r = run("cleanup", &in_json);
    set_mode(&sub, 0o755); // restore before asserting / cleanup

    assert_eq!(r.exit, Some(30));
    assert!(has_log(&r, Level::Error));
}

// ===== contract shape =====

#[test]
fn every_emitted_line_carries_ts_and_instance_id() {
    let dir = tempfile::tempdir().unwrap();
    let r = run(
        "inject",
        &input(dir.path().to_str().unwrap(), 1, Phase::Runtime),
    );
    assert!(!r.events.is_empty());
    for event in &r.events {
        assert_eq!(event.instance_id.as_str(), INSTANCE_ID);
        assert!(
            event.ts.ends_with('Z'),
            "ts must be RFC3339 UTC: {}",
            event.ts
        );
    }
}
