//! Per-scenario container topology: one network, one master, one agent, and an
//! agent data volume — uniquely named, torn down in [`Drop`] even on failure,
//! with container logs dumped when a scenario panics (spec: isolation + always
//! teardown; design D4).
//!
//! Every wait here is a bounded poll against observable state ([`poll_until`]),
//! never a bare sleep-and-assert, so scenarios stay robust on slow runners.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::harness;
use crate::podman;

/// The agent's `data_dir` inside the container (matches the image + volume mount).
pub const AGENT_DATA_DIR: &str = "/var/lib/faultforge";

/// Poll `check` every `interval` until it returns `true` or `timeout` elapses.
/// Returns whether the condition held. This is the only waiting primitive the
/// harness uses — sleeps exist solely as the poll cadence.
pub fn poll_until<F: FnMut() -> bool>(timeout: Duration, interval: Duration, mut check: F) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if check() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(interval);
    }
}

/// The captured result of a `faultforge` CLI invocation.
#[derive(Debug, Clone)]
pub struct CliResult {
    /// Process exit code (the CLI's documented exit-code contract).
    pub code: i32,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
}

impl CliResult {
    /// Parse stdout as JSON (the CLI's default output format).
    ///
    /// # Panics
    ///
    /// Panics if stdout is not valid JSON — a scenario asserting on the record
    /// wants the raw output in the failure.
    #[must_use]
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("CLI stdout was not JSON ({e}):\n{}", self.stdout))
    }
}

/// One scenario's containerized topology.
pub struct Topology {
    scenario: String,
    net: String,
    master: String,
    agent: String,
    volume: String,
    agent_hostname: String,
    management_url: String,
    http: reqwest::blocking::Client,
}

impl Topology {
    /// Stand up a fresh network + master + agent for `scenario` and block until
    /// the agent has registered with the master.
    ///
    /// # Panics
    ///
    /// Panics (dumping any logs via [`Drop`]) if any podman step fails or the
    /// master/agent do not become ready within the bounded timeouts.
    #[must_use]
    pub fn start(scenario: &str) -> Self {
        let images = harness::images();
        let run = harness::run_id();
        let prefix = format!("ff-e2e-{run}-{scenario}");
        let net = format!("{prefix}-net");
        let master = format!("{prefix}-master");
        let agent = format!("{prefix}-agent");
        let volume = format!("{prefix}-data");
        // A fixed, id-safe agent hostname: each master only knows its own agent.
        let agent_hostname = "agent".to_string();

        podman::network_create(&net).expect("create network");
        podman::volume_create(&volume).expect("create volume");

        let mut topo = Self {
            scenario: scenario.to_string(),
            net,
            master,
            agent,
            volume,
            agent_hostname,
            management_url: String::new(),
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(3))
                .build()
                .expect("build http client"),
        };

        topo.start_master(&images.master);
        topo.start_agent(&images.agent);
        topo.wait_for_registration();
        topo
    }

    fn start_master(&mut self, image: &str) {
        podman::run_detached(&[
            "--name".into(),
            self.master.clone(),
            "--network".into(),
            self.net.clone(),
            // Ephemeral host port on loopback: fixed ports would collide across
            // parallel scenarios (advisor note). gRPC stays network-internal.
            "-p".into(),
            "127.0.0.1::8069".into(),
            image.into(),
        ])
        .expect("run master container");

        let mapped = poll_port(&self.master);
        self.management_url = format!("http://{mapped}");

        let ready = poll_until(Duration::from_secs(30), Duration::from_millis(200), || {
            self.get_json("/agents").is_some()
        });
        assert!(ready, "master management API never became ready");
    }

    fn start_agent(&mut self, image: &str) {
        podman::run_detached(&[
            "--name".into(),
            self.agent.clone(),
            "--hostname".into(),
            self.agent_hostname.clone(),
            "--network".into(),
            self.net.clone(),
            "-e".into(),
            format!("FAULTFORGE_MASTER_ADDR=http://{}:50051", self.master),
            "-v".into(),
            format!("{}:{AGENT_DATA_DIR}", self.volume),
            image.into(),
        ])
        .expect("run agent container");
    }

    /// Block until the master reports the agent registered.
    pub fn wait_for_registration(&self) {
        let registered = poll_until(Duration::from_secs(30), Duration::from_millis(200), || {
            self.agent_is_registered()
        });
        assert!(registered, "agent never registered with the master");
    }

    fn agent_is_registered(&self) -> bool {
        let Some(agents) = self.get_json("/agents") else {
            return false;
        };
        agents
            .as_array()
            .into_iter()
            .flatten()
            .any(|a| a.get("hostname").and_then(Value::as_str) == Some(&self.agent_hostname))
    }

    /// The agent's hostname (its sole identity key), for experiment targeting.
    #[must_use]
    pub fn agent_hostname(&self) -> &str {
        &self.agent_hostname
    }

    /// The master container name (for lifecycle steps like stop/start).
    #[must_use]
    pub fn master_container(&self) -> &str {
        &self.master
    }

    /// The agent container name (for lifecycle steps like kill/start).
    #[must_use]
    pub fn agent_container(&self) -> &str {
        &self.agent
    }

    // ===== Container lifecycle (scenario steps) =====

    /// Kill the agent container (a real crash mid-instance).
    pub fn agent_kill(&self) {
        podman::kill(&self.agent).unwrap_or_else(|e| panic!("kill agent: {e}"));
    }

    /// Restart the agent container against the same volume (journal replay).
    pub fn agent_restart(&self) {
        podman::start(&self.agent).unwrap_or_else(|e| panic!("restart agent: {e}"));
    }

    /// Gracefully stop the master container (master-loss scenario).
    pub fn master_stop(&self) {
        podman::stop(&self.master, 2).unwrap_or_else(|e| panic!("stop master: {e}"));
    }

    /// Restart the master container (same published port preserved).
    pub fn master_restart(&self) {
        podman::start(&self.master).unwrap_or_else(|e| panic!("restart master: {e}"));
    }

    // ===== HTTP (management API) =====

    /// GET `path` and return the parsed JSON body, or `None` if the request
    /// failed or returned a non-success status.
    #[must_use]
    pub fn get_json(&self, path: &str) -> Option<Value> {
        let url = format!("{}{path}", self.management_url);
        let resp = self.http.get(&url).send().ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.json().ok()
    }

    /// Fetch one experiment record, or `None` until it is queryable.
    #[must_use]
    pub fn experiment(&self, id: &str) -> Option<Value> {
        self.get_json(&format!("/experiments/{id}"))
    }

    // ===== CLI (part of the system under test) =====

    /// This topology's management API URL (for driving the CLI off-thread).
    #[must_use]
    pub fn management_url(&self) -> &str {
        &self.management_url
    }

    /// Run the `faultforge` CLI against this topology's management URL.
    ///
    /// # Panics
    ///
    /// Panics if the CLI process cannot be spawned.
    #[must_use]
    pub fn cli(&self, args: &[&str]) -> CliResult {
        run_cli(&self.management_url, args)
    }

    // ===== Host ground truth (podman exec into the agent) =====

    /// Whether `path` exists inside the agent container (checked as root so a
    /// `chmod 000` parent in the taint scenario cannot hide a present marker).
    #[must_use]
    pub fn agent_path_exists(&self, path: &str) -> bool {
        match podman::exec(&self.agent, Some("0"), &["test", "-e", path]) {
            Ok(_) => true,
            // `test` exits 1 when the path is absent — a normal answer, not an error.
            Err(podman::PodmanError::Exit { code: 1, .. }) => false,
            Err(e) => panic!("agent exec `test -e {path}` failed: {e}"),
        }
    }

    /// Whether the agent's instance journal directory holds no `*.json` entries
    /// (an empty or absent `instances/` both count as "no live instances").
    #[must_use]
    pub fn journal_is_empty(&self) -> bool {
        let dir = format!("{AGENT_DATA_DIR}/instances");
        let script = format!("ls {dir}/*.json 2>/dev/null | wc -l");
        match podman::exec(&self.agent, Some("0"), &["sh", "-c", &script]) {
            Ok(out) => out.stdout.trim() == "0",
            Err(e) => panic!("agent exec journal check failed: {e}"),
        }
    }

    /// Run an arbitrary command inside the agent container as `--user user`
    /// (e.g. `chmod 000` a directory as root). Returns the captured output.
    ///
    /// # Panics
    ///
    /// Panics if the command exits non-zero.
    pub fn agent_exec(&self, user: &str, cmd: &[&str]) -> podman::Output {
        podman::exec(&self.agent, Some(user), cmd)
            .unwrap_or_else(|e| panic!("agent exec {cmd:?} failed: {e}"))
    }
}

impl Drop for Topology {
    fn drop(&mut self) {
        // On failure, capture both containers' logs *before* removing them:
        // stream to the test output for local runs, and write files under the
        // log dir so CI can upload them as an artifact (spec: failure artifacts).
        if std::thread::panicking() {
            let master_logs = podman::logs(&self.master);
            let agent_logs = podman::logs(&self.agent);
            eprintln!(
                "\n=== e2e scenario '{}' FAILED — container logs follow ===",
                self.scenario
            );
            eprintln!("--- MASTER ({}) ---\n{master_logs}", self.master);
            eprintln!("--- AGENT ({}) ---\n{agent_logs}", self.agent);
            self.write_failure_logs(&master_logs, &agent_logs);
        }
        podman::rm_container(&self.agent);
        podman::rm_container(&self.master);
        podman::rm_volume(&self.volume);
        podman::rm_network(&self.net);
    }
}

impl Topology {
    /// Write captured logs to `$FF_E2E_LOG_DIR` (or the OS temp dir) so a CI run
    /// can upload them as a build artifact. Best-effort: failures are ignored.
    fn write_failure_logs(&self, master_logs: &str, agent_logs: &str) {
        let dir = std::env::var_os("FF_E2E_LOG_DIR").map_or_else(std::env::temp_dir, Into::into);
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let _ = std::fs::write(
            dir.join(format!("{}-master.log", self.scenario)),
            master_logs,
        );
        let _ = std::fs::write(dir.join(format!("{}-agent.log", self.scenario)), agent_logs);
    }
}

/// Poll `podman port <container> 8069` until the host mapping appears.
fn poll_port(container: &str) -> String {
    let mut mapped = String::new();
    let ok = poll_until(
        Duration::from_secs(10),
        Duration::from_millis(200),
        || match podman::port(container, 8069) {
            Ok(p) if !p.is_empty() => {
                mapped = p;
                true
            }
            _ => false,
        },
    );
    assert!(ok, "master never published its management port");
    mapped
}

/// Run the `faultforge` CLI against `management_url` with `args`. A free
/// function (not a `Topology` method) so a scenario can drive `--wait` from a
/// background thread while the main thread polls host state.
///
/// # Panics
///
/// Panics if the CLI process cannot be spawned.
#[must_use]
pub fn run_cli(management_url: &str, args: &[&str]) -> CliResult {
    let cli = harness::faultforge_cli();
    let output = Command::new(cli)
        .arg("--master-url")
        .arg(management_url)
        .args(args)
        .output()
        .expect("spawn faultforge CLI");
    CliResult {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Write an experiment definition YAML to `path`.
///
/// # Panics
///
/// Panics if the file cannot be written.
pub fn write_experiment(path: &Path, yaml: &str) {
    std::fs::write(path, yaml).unwrap_or_else(|e| panic!("write experiment file: {e}"));
}
