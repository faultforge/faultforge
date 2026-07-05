//! End-to-end fault-lifecycle scenarios (spec: the six core scenarios).
//!
//! Each is `#[ignore]` so `cargo test --workspace` stays green without podman;
//! run the suite explicitly with `cargo test -p faultforge-e2e -- --ignored`.
//! Every scenario stands up its own isolated topology, drives the operator
//! surface (CLI + management API), and checks host ground truth via
//! `podman exec` into the agent container.

// A dev-only harness: assertions panic (so Drop dumps container logs) and the
// terse unwrap/expect style is intentional here.
// `dead_code` is allowed while scenarios are added incrementally — shared
// helpers land before every scenario that consumes them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_panics_doc,
    dead_code
)]

use std::time::Duration;

use serde_json::Value;

use faultforge_e2e::harness;
use faultforge_e2e::topology::{self, AGENT_DATA_DIR, Topology};

/// The marker path the fault plugins write, under the agent's data volume.
fn marker_path() -> String {
    format!("{AGENT_DATA_DIR}/marker/marker.json")
}

/// Build an experiment definition YAML targeting the agent host.
fn experiment_yaml(
    name: &str,
    host: &str,
    plugin: &str,
    marker: &str,
    duration_secs: u32,
) -> String {
    format!(
        "name: {name}\n\
         actions:\n\
         \x20 - hosts: [{host}]\n\
         \x20   plugin: {{name: {plugin}, version: \"1\"}}\n\
         \x20   params: {{marker_path: {marker}}}\n\
         \x20   duration_secs: {duration_secs}\n"
    )
}

/// Write an experiment YAML to a unique temp file and return its path.
fn write_yaml(scenario: &str, yaml: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("ff-e2e-{}-{scenario}.yaml", harness::run_id()));
    topology::write_experiment(&path, yaml);
    path
}

/// The single experiment id the master knows (each master runs exactly one).
fn wait_for_experiment_id(topo: &Topology) -> String {
    let mut id = String::new();
    let ok = topology::poll_until(
        Duration::from_secs(15),
        Duration::from_millis(200),
        || match topo.get_json("/experiments") {
            Some(Value::Array(list)) if !list.is_empty() => {
                if let Some(found) = list[0].get("id").and_then(Value::as_str) {
                    id = found.to_string();
                    return true;
                }
                false
            }
            _ => false,
        },
    );
    assert!(ok, "master never registered an experiment");
    id
}

/// Poll until any instance of `id` reports state `state`.
fn wait_for_instance_state(topo: &Topology, id: &str, state: &str) {
    let ok = topology::poll_until(Duration::from_secs(20), Duration::from_millis(200), || {
        topo.experiment(id).is_some_and(|exp| {
            exp.get("instances")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .any(|i| i.get("state").and_then(Value::as_str) == Some(state))
        })
    });
    assert!(ok, "no instance of {id} reached {state}");
}

/// Poll until the experiment `id` is terminal, returning its outcome label.
fn wait_for_outcome(topo: &Topology, id: &str) -> String {
    let mut outcome = String::new();
    let ok = topology::poll_until(
        Duration::from_secs(45),
        Duration::from_millis(300),
        || match topo.experiment(id).and_then(|e| {
            e.get("outcome")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        }) {
            Some(label) => {
                outcome = label;
                true
            }
            None => false,
        },
    );
    assert!(ok, "experiment {id} never reached a terminal outcome");
    outcome
}

/// Submit an experiment via the CLI (no `--wait`), returning the raw CLI result
/// so the caller can assert on the exit code (acceptance or VALIDATE rejection).
fn submit(
    topo: &Topology,
    scenario: &str,
    name: &str,
    plugin: &str,
    duration_secs: u32,
) -> topology::CliResult {
    let yaml = experiment_yaml(
        name,
        topo.agent_hostname(),
        plugin,
        &marker_path(),
        duration_secs,
    );
    let file = write_yaml(scenario, &yaml);
    topo.cli(&["experiment", "run", "-f", &file.to_string_lossy()])
}

/// Submit an experiment, assert it was accepted, and block until an instance is
/// `ACTIVE`. Returns the experiment id.
fn submit_active(
    topo: &Topology,
    scenario: &str,
    name: &str,
    plugin: &str,
    duration_secs: u32,
) -> String {
    let res = submit(topo, scenario, name, plugin, duration_secs);
    assert_eq!(res.code, 0, "experiment rejected, stderr:\n{}", res.stderr);
    let id = res
        .json()
        .get("id")
        .and_then(Value::as_str)
        .expect("accepted experiment has an id")
        .to_string();
    wait_for_instance_state(topo, &id, "ACTIVE");
    id
}

/// The agent's `tainted` flag from `GET /agents/{hostname}`.
fn agent_tainted(topo: &Topology) -> bool {
    topo.get_json(&format!("/agents/{}", topo.agent_hostname()))
        .and_then(|a| a.get("tainted").and_then(Value::as_bool))
        .unwrap_or(false)
}

// ===== Scenario 3.1: happy path =====

#[test]
#[ignore = "requires rootless podman; run with -- --ignored"]
fn happy_path_completes_and_cleans_up() {
    let topo = Topology::start("happy");
    let marker = marker_path();
    let yaml = experiment_yaml(
        "happy-e2e",
        topo.agent_hostname(),
        "noop-marker",
        &marker,
        5,
    );
    let file = write_yaml("happy", &yaml);

    // Drive `experiment run --wait` off-thread so the main thread can observe the
    // marker while the instance is ACTIVE. --wait's exit code is part of the test.
    let url = topo.management_url().to_string();
    let file_arg = file.to_string_lossy().into_owned();
    let cli = std::thread::spawn(move || {
        topology::run_cli(&url, &["experiment", "run", "-f", &file_arg, "--wait"])
    });

    let id = wait_for_experiment_id(&topo);
    wait_for_instance_state(&topo, &id, "ACTIVE");
    assert!(
        topo.agent_path_exists(&marker),
        "marker must exist while the instance is ACTIVE"
    );

    let result = cli.join().expect("CLI thread panicked");
    assert_eq!(
        result.code, 0,
        "expected exit 0, stderr:\n{}",
        result.stderr
    );
    assert_eq!(
        result.json().get("outcome").and_then(Value::as_str),
        Some("COMPLETED"),
        "CLI final record:\n{}",
        result.stdout
    );

    assert!(
        !topo.agent_path_exists(&marker),
        "marker must be removed after COMPLETED"
    );
    assert!(
        topo.journal_is_empty(),
        "journal must be empty after COMPLETED"
    );
}

// ===== Scenario 3.2: operator halt =====

#[test]
#[ignore = "requires rootless podman; run with -- --ignored"]
fn operator_halt_aborts_and_cleans_up() {
    let topo = Topology::start("halt");
    let marker = marker_path();
    // Long duration so the halt lands squarely mid-ACTIVE, not near the natural end.
    let id = submit_active(&topo, "halt", "halt-e2e", "noop-marker", 30);
    assert!(
        topo.agent_path_exists(&marker),
        "marker present while ACTIVE"
    );

    let halt = topo.cli(&["experiment", "halt", &id]);
    assert_eq!(halt.code, 0, "halt rejected, stderr:\n{}", halt.stderr);

    assert_eq!(wait_for_outcome(&topo, &id), "ABORTED");
    assert!(
        !topo.agent_path_exists(&marker),
        "marker removed after halt"
    );
    assert!(topo.journal_is_empty(), "journal empty after halt");
}

// ===== Scenario 3.3: agent crash mid-ACTIVE (journal replay) =====

#[test]
#[ignore = "requires rootless podman; run with -- --ignored"]
fn agent_crash_replays_journal_and_recovers() {
    let topo = Topology::start("crash");
    let marker = marker_path();
    let id = submit_active(&topo, "crash", "crash-e2e", "noop-marker", 30);
    assert!(
        topo.agent_path_exists(&marker),
        "marker present while ACTIVE"
    );
    assert!(
        !topo.journal_is_empty(),
        "journal has the live instance entry"
    );

    // A real crash: kill the process, then restart the same container against the
    // same data volume so restart replay (abort+cleanup) runs on real journal state.
    topo.agent_kill();
    topo.agent_restart();
    topo.wait_for_registration();

    // The replayed instance is reported ABORTED (recovered before the deadline).
    assert_eq!(wait_for_outcome(&topo, &id), "ABORTED");
    assert!(
        !topo.agent_path_exists(&marker),
        "replay removed the marker"
    );
    assert!(topo.journal_is_empty(), "replay removed the journal entry");
}

// ===== Scenario 3.4: master-loss self-abort =====

#[test]
#[ignore = "requires rootless podman; run with -- --ignored"]
fn master_loss_triggers_self_abort() {
    let topo = Topology::start("masterloss");
    let marker = marker_path();
    let _id = submit_active(&topo, "masterloss", "masterloss-e2e", "noop-marker", 30);
    assert!(
        topo.agent_path_exists(&marker),
        "marker present while ACTIVE"
    );

    // Stop the master. With no master reachable, after the loss threshold the
    // agent self-aborts entirely on its own — asserted with no master running.
    topo.master_stop();
    let recovered =
        topology::poll_until(Duration::from_secs(20), Duration::from_millis(300), || {
            !topo.agent_path_exists(&marker)
        });
    assert!(
        recovered,
        "agent must self-abort and remove the marker with no master"
    );
    assert!(
        topo.journal_is_empty(),
        "self-abort removed the journal entry"
    );

    // Bring the master back: the agent re-registers, and the experiment record
    // (memory-only) died with the master, so it reports an empty InstanceReport.
    topo.master_restart();
    topo.wait_for_registration();
    let no_experiments = topology::poll_until(
        Duration::from_secs(15),
        Duration::from_millis(300),
        || matches!(topo.get_json("/experiments"), Some(Value::Array(a)) if a.is_empty()),
    );
    assert!(
        no_experiments,
        "restarted master starts with no experiments"
    );
}

// ===== Scenario 3.5: dead-man backstop =====

#[test]
#[ignore = "requires rootless podman; run with -- --ignored"]
fn hang_cleanup_hits_the_deadman_and_errors() {
    let topo = Topology::start("deadman");
    // hang-cleanup injects a marker then blocks in cleanup; the duration stop
    // hangs, the invocation timeout and dead-man drive the instance to ERROR.
    let id = submit_active(&topo, "deadman", "deadman-e2e", "hang-cleanup", 3);
    assert_eq!(wait_for_outcome(&topo, &id), "ERROR");

    let exp = topo.experiment(&id).expect("experiment record");
    let instance_errored = exp
        .get("instances")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|i| i.get("state").and_then(Value::as_str) == Some("ERROR"));
    assert!(instance_errored, "the instance itself ends ERROR");
}

// ===== Scenario 3.6: taint and operator clear =====

#[test]
#[ignore = "requires rootless podman; run with -- --ignored"]
fn taint_blocks_experiments_until_cleared() {
    let topo = Topology::start("taint");
    let marker = marker_path();
    let marker_dir = format!("{AGENT_DATA_DIR}/marker");
    let id = submit_active(&topo, "taint", "taint-e2e", "noop-marker", 8);
    assert!(
        topo.agent_path_exists(&marker),
        "marker present while ACTIVE"
    );

    // Break cleanup from outside as root: chmod 000 the marker's parent so the
    // agent (unprivileged) cannot remove the marker. Cleanup fails twice → taint.
    topo.agent_exec("0", &["chmod", "000", &marker_dir]);

    assert_eq!(wait_for_outcome(&topo, &id), "ERROR");
    let tainted = topology::poll_until(Duration::from_secs(15), Duration::from_millis(300), || {
        agent_tainted(&topo)
    });
    assert!(tainted, "the host must report tainted: true");

    // A new experiment targeting the tainted host is rejected at VALIDATE (exit 3).
    let rejected = submit(&topo, "taint-rej", "taint-rej-e2e", "noop-marker", 3);
    assert_eq!(
        rejected.code, 3,
        "tainted host must be rejected at VALIDATE, stderr:\n{}",
        rejected.stderr
    );

    // Restore permissions and clear the taint through the operator surface.
    topo.agent_exec("0", &["chmod", "755", &marker_dir]);
    let clear = topo.cli(&["agents", "clear-taint", topo.agent_hostname()]);
    assert_eq!(
        clear.code, 0,
        "clear-taint failed, stderr:\n{}",
        clear.stderr
    );
    let cleared = topology::poll_until(Duration::from_secs(15), Duration::from_millis(300), || {
        !agent_tainted(&topo)
    });
    assert!(cleared, "host must become untainted after clear-taint");

    // The host is schedulable again: a fresh experiment completes.
    let yaml = experiment_yaml(
        "taint-ok-e2e",
        topo.agent_hostname(),
        "noop-marker",
        &marker,
        3,
    );
    let file = write_yaml("taint-ok", &yaml);
    let run = topo.cli(&["experiment", "run", "-f", &file.to_string_lossy(), "--wait"]);
    assert_eq!(
        run.code, 0,
        "post-clear experiment failed, stderr:\n{}",
        run.stderr
    );
    assert_eq!(
        run.json().get("outcome").and_then(Value::as_str),
        Some("COMPLETED"),
        "post-clear record:\n{}",
        run.stdout
    );
}
