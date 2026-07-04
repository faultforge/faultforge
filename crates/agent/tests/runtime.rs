//! End-to-end tests of the agent fault runtime against an in-process fake
//! master (design D11): the real `run_agent` (and, for the restart test, the
//! real binary), scripted `RunFault`/`AbortFault` frames, `#!/bin/sh` fixture
//! plugins in temp catalogs, and assertions on frames *and* host effects.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::{Request, Response, Status, Streaming};

use faultforge_agent::journal::{JOURNAL_VERSION, Journal, JournalEntry};
use faultforge_agent::{AgentConfig, run_agent};
use faultforge_fault::digest::Digest;
use faultforge_fault::proto::to_wire_i32;
use faultforge_fault::state::InstanceState;
use faultforge_proto::v1::agent_service_server::{AgentService, AgentServiceServer};
use faultforge_proto::v1::{
    AbortFault, AgentMessage, ClearTaint, HeartbeatAck, InstanceReport, InstanceStatus,
    RegisterAck, RunFault, ServerMessage, agent_message, server_message,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(20);

// ===== Fake master =====

struct FakeSession {
    to_agent: mpsc::Sender<Result<ServerMessage, Status>>,
    from_agent: mpsc::Receiver<AgentMessage>,
    reader: tokio::task::JoinHandle<()>,
}

impl FakeSession {
    async fn send(&self, msg: ServerMessage) {
        self.to_agent
            .send(Ok(msg))
            .await
            .expect("session stream gone");
    }

    /// Drop the stream so the agent sees a session failure.
    fn kill(self) {
        self.reader.abort();
    }
}

struct Svc {
    heartbeat_secs: u32,
    sessions_tx: mpsc::Sender<FakeSession>,
}

#[tonic::async_trait]
impl AgentService for Svc {
    type SessionStream = ReceiverStream<Result<ServerMessage, Status>>;

    async fn session(
        &self,
        request: Request<Streaming<AgentMessage>>,
    ) -> Result<Response<Self::SessionStream>, Status> {
        let mut inbound = request.into_inner();
        let (to_agent, to_agent_rx) = mpsc::channel(256);
        let (from_tx, from_agent) = mpsc::channel(1024);
        let acks = to_agent.clone();
        let heartbeat_secs = self.heartbeat_secs;
        // Auto-ack Register/Heartbeat; record every frame for the test body.
        let reader = tokio::spawn(async move {
            while let Ok(Some(msg)) = inbound.message().await {
                let ack = match &msg.payload {
                    Some(agent_message::Payload::Register(_)) => Some(ServerMessage {
                        payload: Some(server_message::Payload::RegisterAck(RegisterAck {
                            server_time_unix_ms: 0,
                            heartbeat_interval_secs: heartbeat_secs,
                        })),
                    }),
                    Some(agent_message::Payload::Heartbeat(_)) => Some(ServerMessage {
                        payload: Some(server_message::Payload::HeartbeatAck(HeartbeatAck {
                            server_time_unix_ms: 0,
                        })),
                    }),
                    _ => None,
                };
                if let Some(ack) = ack
                    && acks.send(Ok(ack)).await.is_err()
                {
                    break;
                }
                if from_tx.send(msg).await.is_err() {
                    break;
                }
            }
        });
        let _ = self
            .sessions_tx
            .send(FakeSession {
                to_agent,
                from_agent,
                reader,
            })
            .await;
        Ok(Response::new(ReceiverStream::new(to_agent_rx)))
    }
}

struct FakeMaster {
    addr: String,
    sessions_rx: mpsc::Receiver<FakeSession>,
    server: tokio::task::JoinHandle<()>,
}

impl FakeMaster {
    async fn start(heartbeat_secs: u32) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let (sessions_tx, sessions_rx) = mpsc::channel(8);
        let server = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(AgentServiceServer::new(Svc {
                    heartbeat_secs,
                    sessions_tx,
                }))
                .serve_with_incoming(TcpListenerStream::new(listener))
                .await
                .expect("fake master server failed");
        });
        Self {
            addr,
            sessions_rx,
            server,
        }
    }

    /// The next agent session, delivered only after its `Register` frame has
    /// been consumed — which guarantees the auto-`RegisterAck` is already
    /// queued ahead of anything the test sends, matching the real master's
    /// ack-first ordering (the agent log-skips fault frames before its ack).
    async fn session(&mut self) -> FakeSession {
        let mut session = tokio::time::timeout(TEST_TIMEOUT, self.sessions_rx.recv())
            .await
            .expect("timed out waiting for an agent session")
            .expect("fake master gone");
        loop {
            let msg = tokio::time::timeout(TEST_TIMEOUT, session.from_agent.recv())
                .await
                .expect("timed out waiting for Register")
                .expect("session closed before Register");
            if matches!(msg.payload, Some(agent_message::Payload::Register(_))) {
                return session;
            }
        }
    }

    /// Stop the whole server: further connection attempts are refused.
    fn shutdown(&self) {
        self.server.abort();
    }
}

impl Drop for FakeMaster {
    fn drop(&mut self) {
        self.server.abort();
    }
}

// ===== Fixture plugins and agent bootstrap =====

/// Install a `#!/bin/sh` fixture plugin as `<name>@1` and return its digest.
fn install_plugin(root: &Path, name: &str, body: &str) -> String {
    let manifest =
        format!("name: {name}\nversion: \"1\"\nentrypoint: ./run.sh\nmax_duration_secs: 600\n");
    let script = format!("#!/bin/sh\n{body}\n");
    let dir = root.join(format!("{name}@1"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("manifest.yaml"), &manifest).unwrap();
    let script_path = dir.join("run.sh");
    std::fs::write(&script_path, &script).unwrap();
    std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    Digest::compute(manifest.as_bytes(), script.as_bytes()).to_string()
}

/// A plugin that appends to a marker on inject, removes it on cleanup, and
/// echoes one deterministic log line per command.
fn marker_plugin_body(marker: &Path) -> String {
    let marker = marker.display();
    format!(
        r#"cmd="$1"
cat > /dev/null
case "$cmd" in
  inject) echo run >> "{marker}" ;;
  cleanup) rm -f "{marker}" ;;
esac
echo '{{"ts":"2026-07-04T00:00:00Z","instance_id":"i-1","type":"log","level":"info","msg":"'"$cmd"'"}}'
exit 0"#
    )
}

struct TestBed {
    master: FakeMaster,
    plugin_root: tempfile::TempDir,
    data_dir: tempfile::TempDir,
    agent: Option<tokio::task::JoinHandle<()>>,
}

impl TestBed {
    async fn start(heartbeat_secs: u32) -> Self {
        Self {
            master: FakeMaster::start(heartbeat_secs).await,
            plugin_root: tempfile::tempdir().unwrap(),
            data_dir: tempfile::tempdir().unwrap(),
            agent: None,
        }
    }

    fn config(&self, master_loss_threshold_secs: u64, invocation_timeout_secs: u64) -> AgentConfig {
        AgentConfig {
            master_addr: self.master.addr.clone(),
            plugin_root: self.plugin_root.path().display().to_string(),
            data_dir: self.data_dir.path().display().to_string(),
            master_loss_threshold_secs,
            invocation_timeout_secs,
        }
    }

    fn spawn_agent(&mut self, master_loss_threshold_secs: u64, invocation_timeout_secs: u64) {
        let cfg = self.config(master_loss_threshold_secs, invocation_timeout_secs);
        self.agent = Some(tokio::spawn(async move {
            let _ = run_agent(cfg).await;
        }));
    }

    fn journal_path(&self, instance_id: &str) -> PathBuf {
        self.data_dir
            .path()
            .join(format!("instances/{instance_id}.json"))
    }

    fn taint_path(&self) -> PathBuf {
        self.data_dir.path().join("tainted.json")
    }
}

impl Drop for TestBed {
    fn drop(&mut self) {
        if let Some(agent) = &self.agent {
            agent.abort();
        }
    }
}

// ===== Frame helpers =====

fn run_fault(
    id: &str,
    plugin: &str,
    digest: &str,
    duration_secs: u32,
    grace_secs: u32,
) -> ServerMessage {
    ServerMessage {
        payload: Some(server_message::Payload::RunFault(RunFault {
            instance_id: id.to_string(),
            plugin_name: plugin.to_string(),
            plugin_version: "1".to_string(),
            plugin_digest: digest.to_string(),
            params_json: "{}".to_string(),
            duration_secs,
            grace_secs,
        })),
    }
}

fn abort_fault(id: &str) -> ServerMessage {
    ServerMessage {
        payload: Some(server_message::Payload::AbortFault(AbortFault {
            instance_id: id.to_string(),
        })),
    }
}

fn clear_taint() -> ServerMessage {
    ServerMessage {
        payload: Some(server_message::Payload::ClearTaint(ClearTaint {})),
    }
}

/// The next `TaintStatus` frame, skipping everything else.
async fn next_taint_status(session: &mut FakeSession) -> faultforge_proto::v1::TaintStatus {
    let deadline = tokio::time::Instant::now() + TEST_TIMEOUT;
    loop {
        let msg = tokio::time::timeout_at(deadline, session.from_agent.recv())
            .await
            .expect("timed out waiting for TaintStatus")
            .expect("session closed");
        if let Some(agent_message::Payload::TaintStatus(ts)) = msg.payload {
            return ts;
        }
    }
}

/// Receive frames until `id` reaches `state`, returning everything seen
/// (heartbeats included) so callers can assert on the full stream. The
/// deadline is overall, not per-frame — a 1s heartbeat cadence must not keep a
/// failing wait alive forever.
async fn frames_until_state(
    session: &mut FakeSession,
    id: &str,
    state: InstanceState,
) -> Vec<AgentMessage> {
    let target = to_wire_i32(state);
    let deadline = tokio::time::Instant::now() + TEST_TIMEOUT;
    let mut seen = vec![];
    loop {
        let msg = tokio::time::timeout_at(deadline, session.from_agent.recv())
            .await
            .unwrap_or_else(|_| {
                panic!("timed out waiting for {id} to reach {state:?}; saw: {seen:?}")
            })
            .expect("session closed");
        let done = matches!(
            &msg.payload,
            Some(agent_message::Payload::InstanceStatus(s))
                if s.instance_id == id && s.state == target
        );
        seen.push(msg);
        if done {
            return seen;
        }
    }
}

fn statuses_of<'a>(frames: &'a [AgentMessage], id: &str) -> Vec<&'a InstanceStatus> {
    frames
        .iter()
        .filter_map(|m| match &m.payload {
            Some(agent_message::Payload::InstanceStatus(s)) if s.instance_id == id => Some(s),
            _ => None,
        })
        .collect()
}

fn state_sequence(frames: &[AgentMessage], id: &str) -> Vec<i32> {
    statuses_of(frames, id).iter().map(|s| s.state).collect()
}

fn fault_event_lines(frames: &[AgentMessage], id: &str) -> Vec<String> {
    frames
        .iter()
        .filter_map(|m| match &m.payload {
            Some(agent_message::Payload::FaultEvent(e)) if e.instance_id == id => {
                Some(e.ndjson_line.clone())
            }
            _ => None,
        })
        .collect()
}

async fn next_report(session: &mut FakeSession) -> InstanceReport {
    let deadline = tokio::time::Instant::now() + TEST_TIMEOUT;
    loop {
        let msg = tokio::time::timeout_at(deadline, session.from_agent.recv())
            .await
            .expect("timed out waiting for InstanceReport")
            .expect("session closed");
        if let Some(agent_message::Payload::InstanceReport(report)) = msg.payload {
            return report;
        }
    }
}

fn wire(state: InstanceState) -> i32 {
    to_wire_i32(state)
}

async fn wait_for<F: Fn() -> bool>(what: &str, cond: F) {
    let deadline = std::time::Instant::now() + TEST_TIMEOUT;
    while !cond() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ===== 6.2 Happy path =====

#[tokio::test]
async fn happy_path_reaches_done_with_full_telemetry() {
    let mut bed = TestBed::start(1).await;
    let marker = bed.data_dir.path().join("marker");
    let digest = install_plugin(
        bed.plugin_root.path(),
        "fixture",
        &marker_plugin_body(&marker),
    );
    bed.spawn_agent(30, 10);
    let mut session = bed.master.session().await;

    session
        .send(run_fault("i-1", "fixture", &digest, 1, 10))
        .await;
    let frames = frames_until_state(&mut session, "i-1", InstanceState::Active).await;
    assert!(marker.exists(), "inject must have written the marker");
    assert!(
        bed.journal_path("i-1").exists(),
        "journal must exist while the fault is active"
    );

    let rest = frames_until_state(&mut session, "i-1", InstanceState::Done).await;
    assert!(!marker.exists(), "cleanup must have removed the marker");
    assert!(
        !bed.journal_path("i-1").exists(),
        "journal must be removed at terminal state"
    );

    let all: Vec<AgentMessage> = frames.into_iter().chain(rest).collect();
    assert_eq!(
        state_sequence(&all, "i-1"),
        vec![
            wire(InstanceState::Pending),
            wire(InstanceState::Preflight),
            wire(InstanceState::Injecting),
            wire(InstanceState::Active),
            wire(InstanceState::Recovering),
            wire(InstanceState::Done),
        ]
    );
    for status in statuses_of(&all, "i-1") {
        assert_eq!(status.plugin_digest, digest, "statuses carry the digest");
    }
    // Verbatim forwarding: the plugin's preflight line arrives byte-identical.
    let expected = r#"{"ts":"2026-07-04T00:00:00Z","instance_id":"i-1","type":"log","level":"info","msg":"preflight"}"#;
    let lines = fault_event_lines(&all, "i-1");
    assert!(
        lines.iter().filter(|l| *l == expected).count() >= 2,
        "install + runtime preflight lines forwarded verbatim, got: {lines:?}"
    );
}

// ===== 6.3 Failure mappings =====

#[tokio::test]
async fn preflight_exit_10_aborts_without_inject() {
    let mut bed = TestBed::start(1).await;
    let marker = bed.data_dir.path().join("marker");
    let body = format!(
        "[ \"$1\" = preflight ] && exit 10\n{}",
        marker_plugin_body(&marker)
    );
    let digest = install_plugin(bed.plugin_root.path(), "fixture", &body);
    bed.spawn_agent(30, 10);
    let mut session = bed.master.session().await;

    session
        .send(run_fault("i-1", "fixture", &digest, 5, 10))
        .await;
    let frames = frames_until_state(&mut session, "i-1", InstanceState::Aborted).await;
    assert!(!marker.exists(), "inject must never have run");
    let last = *statuses_of(&frames, "i-1").last().unwrap();
    assert!(
        last.reason.contains("preflight"),
        "reason names the failing check: {}",
        last.reason
    );
    assert!(!bed.journal_path("i-1").exists());
}

#[tokio::test]
async fn inject_exit_20_aborts_with_host_unaffected() {
    let mut bed = TestBed::start(1).await;
    let body = "cat > /dev/null\n[ \"$1\" = inject ] && exit 20\nexit 0";
    let digest = install_plugin(bed.plugin_root.path(), "fixture", body);
    bed.spawn_agent(30, 10);
    let mut session = bed.master.session().await;

    session
        .send(run_fault("i-1", "fixture", &digest, 5, 10))
        .await;
    let frames = frames_until_state(&mut session, "i-1", InstanceState::Aborted).await;
    let last = *statuses_of(&frames, "i-1").last().unwrap();
    assert!(last.reason.contains("inject failed"), "{}", last.reason);
    // No recovery ran and the journal is gone.
    assert!(!bed.journal_path("i-1").exists());
    assert!(!bed.taint_path().exists());
}

#[tokio::test]
async fn cleanup_exit_30_twice_taints_the_host_and_blocks_new_faults() {
    let mut bed = TestBed::start(1).await;
    let marker = bed.data_dir.path().join("marker");
    let body = format!(
        "cat > /dev/null\ncase \"$1\" in inject) echo run >> \"{m}\" ;; cleanup) exit 30 ;; esac\nexit 0",
        m = marker.display()
    );
    let digest = install_plugin(bed.plugin_root.path(), "fixture", &body);
    bed.spawn_agent(30, 10);
    let mut session = bed.master.session().await;

    session
        .send(run_fault("i-1", "fixture", &digest, 1, 60))
        .await;
    let frames = frames_until_state(&mut session, "i-1", InstanceState::Error).await;
    let last = *statuses_of(&frames, "i-1").last().unwrap();
    assert!(
        last.reason.contains("recovery failed after retry"),
        "{}",
        last.reason
    );
    assert!(bed.taint_path().exists(), "taint record must be persisted");
    let tainted = frames.iter().any(|m| {
        matches!(
            &m.payload,
            Some(agent_message::Payload::TaintStatus(t)) if t.tainted
        )
    });
    assert!(tainted, "TaintStatus{{tainted:true}} must be reported");

    // A tainted host refuses new faults without invoking any plugin.
    session
        .send(run_fault("i-2", "fixture", &digest, 1, 10))
        .await;
    let frames2 = frames_until_state(&mut session, "i-2", InstanceState::Aborted).await;
    let last2 = *statuses_of(&frames2, "i-2").last().unwrap();
    assert!(last2.reason.contains("tainted"), "{}", last2.reason);
    let marker_lines = std::fs::read_to_string(&marker).unwrap();
    assert_eq!(marker_lines.lines().count(), 1, "i-2 never ran inject");
}

#[tokio::test]
async fn clear_taint_lifts_the_quarantine_and_new_faults_run() {
    let mut bed = TestBed::start(1).await;
    let marker = bed.data_dir.path().join("marker");
    let bad_body = "cat > /dev/null\n[ \"$1\" = cleanup ] && exit 30\nexit 0";
    let bad_digest = install_plugin(bed.plugin_root.path(), "bad", bad_body);
    let good_digest = install_plugin(bed.plugin_root.path(), "good", &marker_plugin_body(&marker));
    bed.spawn_agent(30, 10);
    let mut session = bed.master.session().await;

    session
        .send(run_fault("i-1", "bad", &bad_digest, 1, 60))
        .await;
    frames_until_state(&mut session, "i-1", InstanceState::Error).await;
    assert!(bed.taint_path().exists());

    session.send(clear_taint()).await;
    let ts = next_taint_status(&mut session).await;
    assert!(!ts.tainted, "clear must report tainted: false");
    assert!(!bed.taint_path().exists(), "taint record must be removed");

    // The quarantine is lifted: a new fault runs a full lifecycle again.
    session
        .send(run_fault("i-2", "good", &good_digest, 1, 10))
        .await;
    frames_until_state(&mut session, "i-2", InstanceState::Done).await;
}

#[tokio::test]
async fn clear_taint_on_a_clean_host_is_harmless() {
    let mut bed = TestBed::start(1).await;
    bed.spawn_agent(30, 10);
    let mut session = bed.master.session().await;

    // Consume the reconciliation TaintStatus so the next one observed is the
    // ClearTaint answer, not the post-register report.
    let initial = next_taint_status(&mut session).await;
    assert!(!initial.tainted);

    session.send(clear_taint()).await;
    let ts = next_taint_status(&mut session).await;
    assert!(!ts.tainted);
    assert!(!bed.taint_path().exists());
}

#[tokio::test]
async fn malformed_ndjson_is_telemetry_not_failure() {
    let mut bed = TestBed::start(1).await;
    let body = "cat > /dev/null\n[ \"$1\" = inject ] && echo 'this is not json'\nexit 0";
    let digest = install_plugin(bed.plugin_root.path(), "fixture", body);
    bed.spawn_agent(30, 10);
    let mut session = bed.master.session().await;

    session
        .send(run_fault("i-1", "fixture", &digest, 1, 10))
        .await;
    let frames = frames_until_state(&mut session, "i-1", InstanceState::Done).await;
    let lines = fault_event_lines(&frames, "i-1");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("malformed plugin output") && l.contains("this is not json")),
        "malformed output surfaces as an agent-authored error log: {lines:?}"
    );
}

#[tokio::test]
async fn hung_plugin_is_killed_and_instance_errors() {
    let mut bed = TestBed::start(1).await;
    let body = "cat > /dev/null\n[ \"$1\" = inject ] && sleep 60\nexit 0";
    let digest = install_plugin(bed.plugin_root.path(), "fixture", body);
    // 3s: far below the 60s hang, comfortably above preflight latency under
    // parallel-test load (1s proved flaky there).
    bed.spawn_agent(30, 3);
    let mut session = bed.master.session().await;

    session
        .send(run_fault("i-1", "fixture", &digest, 30, 60))
        .await;
    let frames = frames_until_state(&mut session, "i-1", InstanceState::Error).await;
    let last = *statuses_of(&frames, "i-1").last().unwrap();
    assert!(last.reason.contains("invocation failed"), "{}", last.reason);
}

// ===== 6.4 Control =====

#[tokio::test]
async fn abort_fault_mid_active_recovers_and_aborts() {
    let mut bed = TestBed::start(1).await;
    let marker = bed.data_dir.path().join("marker");
    let digest = install_plugin(
        bed.plugin_root.path(),
        "fixture",
        &marker_plugin_body(&marker),
    );
    bed.spawn_agent(30, 10);
    let mut session = bed.master.session().await;

    session
        .send(run_fault("i-1", "fixture", &digest, 60, 60))
        .await;
    frames_until_state(&mut session, "i-1", InstanceState::Active).await;
    session.send(abort_fault("i-1")).await;
    let frames = frames_until_state(&mut session, "i-1", InstanceState::Aborted).await;
    assert!(
        state_sequence(&frames, "i-1").contains(&wire(InstanceState::Recovering)),
        "abort goes through recovery"
    );
    assert!(!marker.exists(), "cleanup must have removed the marker");
    assert!(!bed.journal_path("i-1").exists());
}

#[tokio::test]
async fn duplicate_run_fault_is_dropped() {
    let mut bed = TestBed::start(1).await;
    let marker = bed.data_dir.path().join("marker");
    let digest = install_plugin(
        bed.plugin_root.path(),
        "fixture",
        &marker_plugin_body(&marker),
    );
    bed.spawn_agent(30, 10);
    let mut session = bed.master.session().await;

    session
        .send(run_fault("i-1", "fixture", &digest, 2, 10))
        .await;
    session
        .send(run_fault("i-1", "fixture", &digest, 2, 10))
        .await;
    frames_until_state(&mut session, "i-1", InstanceState::Done).await;
    // Cleanup removed the marker; recreate-proof: inject ran exactly once, so
    // during the active window the marker had exactly one line. Verify via the
    // journal side effect instead: no second lifecycle means no second PENDING.
    let mut extra_pending = 0;
    while let Ok(Some(msg)) =
        tokio::time::timeout(Duration::from_millis(500), session.from_agent.recv()).await
    {
        if let Some(agent_message::Payload::InstanceStatus(s)) = &msg.payload
            && s.instance_id == "i-1"
            && s.state == wire(InstanceState::Pending)
        {
            extra_pending += 1;
        }
    }
    assert_eq!(extra_pending, 0, "no second lifecycle may start");
}

#[tokio::test]
async fn setup_rejections_abort_before_any_injection() {
    let mut bed = TestBed::start(1).await;
    let marker = bed.data_dir.path().join("marker");
    let digest = install_plugin(
        bed.plugin_root.path(),
        "fixture",
        &marker_plugin_body(&marker),
    );
    bed.spawn_agent(30, 10);
    let mut session = bed.master.session().await;

    // Unknown plugin.
    session
        .send(run_fault("i-1", "ghost", &digest, 1, 10))
        .await;
    let frames = frames_until_state(&mut session, "i-1", InstanceState::Aborted).await;
    assert!(
        statuses_of(&frames, "i-1")
            .last()
            .unwrap()
            .reason
            .contains("not in catalog"),
    );

    // Wrong digest (valid hex, wrong bytes).
    let wrong = "0".repeat(64);
    session
        .send(run_fault("i-2", "fixture", &wrong, 1, 10))
        .await;
    let frames = frames_until_state(&mut session, "i-2", InstanceState::Aborted).await;
    assert!(
        statuses_of(&frames, "i-2")
            .last()
            .unwrap()
            .reason
            .contains("digest mismatch"),
    );

    // Duration above the manifest cap (600).
    session
        .send(run_fault("i-3", "fixture", &digest, 601, 10))
        .await;
    let frames = frames_until_state(&mut session, "i-3", InstanceState::Aborted).await;
    assert!(
        statuses_of(&frames, "i-3")
            .last()
            .unwrap()
            .reason
            .contains("exceeds"),
    );

    // Undeclared parameter.
    let mut bad_params = run_fault("i-4", "fixture", &digest, 1, 10);
    if let Some(server_message::Payload::RunFault(rf)) = &mut bad_params.payload {
        rf.params_json = r#"{"undeclared": 1}"#.to_string();
    }
    session.send(bad_params).await;
    let frames = frames_until_state(&mut session, "i-4", InstanceState::Aborted).await;
    assert!(
        statuses_of(&frames, "i-4")
            .last()
            .unwrap()
            .reason
            .contains("invalid params"),
    );

    assert!(!marker.exists(), "no rejection may have reached inject");
}

// ===== 6.5 Resilience =====

#[tokio::test]
async fn brief_stream_drop_reconnects_without_self_abort() {
    let mut bed = TestBed::start(1).await;
    let marker = bed.data_dir.path().join("marker");
    let digest = install_plugin(
        bed.plugin_root.path(),
        "fixture",
        &marker_plugin_body(&marker),
    );
    bed.spawn_agent(10, 10);
    let mut session = bed.master.session().await;

    session
        .send(run_fault("i-1", "fixture", &digest, 4, 20))
        .await;
    frames_until_state(&mut session, "i-1", InstanceState::Active).await;
    session.kill();

    // The agent reconnects (1s backoff) well under the 10s threshold.
    let mut session2 = bed.master.session().await;
    let report = next_report(&mut session2).await;
    assert_eq!(report.statuses.len(), 1, "the live instance is reported");
    assert_eq!(report.statuses[0].instance_id, "i-1");
    assert_eq!(report.statuses[0].state, wire(InstanceState::Active));
    assert_eq!(report.statuses[0].plugin_digest, digest);

    let frames = frames_until_state(&mut session2, "i-1", InstanceState::Done).await;
    assert!(
        !state_sequence(&frames, "i-1").contains(&wire(InstanceState::Aborted)),
        "no self-abort under the threshold"
    );
}

#[tokio::test]
async fn sustained_master_loss_self_aborts_active_instances() {
    let mut bed = TestBed::start(1).await;
    let marker = bed.data_dir.path().join("marker");
    let digest = install_plugin(
        bed.plugin_root.path(),
        "fixture",
        &marker_plugin_body(&marker),
    );
    bed.spawn_agent(1, 10);
    let mut session = bed.master.session().await;

    session
        .send(run_fault("i-1", "fixture", &digest, 300, 300))
        .await;
    frames_until_state(&mut session, "i-1", InstanceState::Active).await;
    assert!(marker.exists());

    // Take the whole master down. Aborting the server only stops the accept
    // loop — the live h2 connection is a separate task — so the established
    // session must be killed too for the agent to actually lose the master.
    bed.master.shutdown();
    session.kill();

    // Self-abort (threshold 1s) runs abort+cleanup — observable host-side.
    wait_for("self-abort cleanup to remove the marker", || {
        !marker.exists()
    })
    .await;
    wait_for("journal removal after self-abort", || {
        !bed.journal_path("i-1").exists()
    })
    .await;
}

#[tokio::test]
async fn deadman_fires_when_inject_eats_the_grace() {
    let mut bed = TestBed::start(1).await;
    // Inject takes 3s while duration+grace is ~2s: by ACTIVE the dead-man has
    // already passed and must dominate the graceful duration stop.
    let body = "cat > /dev/null\n[ \"$1\" = inject ] && sleep 3\nexit 0";
    let digest = install_plugin(bed.plugin_root.path(), "fixture", body);
    bed.spawn_agent(30, 10);
    let mut session = bed.master.session().await;

    session
        .send(run_fault("i-1", "fixture", &digest, 1, 1))
        .await;
    let frames = frames_until_state(&mut session, "i-1", InstanceState::Error).await;
    let last = *statuses_of(&frames, "i-1").last().unwrap();
    assert!(
        last.reason.contains("dead-man"),
        "reason names the dead-man: {}",
        last.reason
    );
}

// ===== 6.6 Restart replay =====

#[tokio::test]
async fn killed_agent_replays_journal_and_recovers_on_restart() {
    let mut bed = TestBed::start(1).await;
    let marker = bed.data_dir.path().join("marker");
    let digest = install_plugin(
        bed.plugin_root.path(),
        "fixture",
        &marker_plugin_body(&marker),
    );

    // A real process so SIGKILL kills supervision the way a crash would.
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_faultforge-agent"))
        .args([
            "--master-addr",
            &bed.master.addr,
            "--plugin-root",
            &bed.plugin_root.path().display().to_string(),
            "--data-dir",
            &bed.data_dir.path().display().to_string(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();

    let mut session = bed.master.session().await;
    session
        .send(run_fault("i-1", "fixture", &digest, 300, 300))
        .await;
    frames_until_state(&mut session, "i-1", InstanceState::Active).await;
    assert!(bed.journal_path("i-1").exists());
    assert!(marker.exists());

    child.kill().unwrap();
    let _ = child.wait();

    // Restart (in-process this time) against the same data_dir.
    bed.spawn_agent(30, 10);
    let mut session2 = bed.master.session().await;
    let report = next_report(&mut session2).await;
    assert!(
        report.statuses.is_empty(),
        "replay finished before the first connect; nothing is live"
    );
    let frames = frames_until_state(&mut session2, "i-1", InstanceState::Aborted).await;
    let last = *statuses_of(&frames, "i-1").last().unwrap();
    assert!(
        last.reason.contains("agent restart"),
        "reason: {}",
        last.reason
    );
    assert!(!marker.exists(), "replay cleanup removed the marker");
    assert!(!bed.journal_path("i-1").exists(), "journal entry removed");
    assert!(!bed.taint_path().exists(), "clean recovery must not taint");
}

#[tokio::test]
async fn replay_past_the_deadman_marks_error() {
    let mut bed = TestBed::start(1).await;
    let marker = bed.data_dir.path().join("marker");
    let digest = install_plugin(
        bed.plugin_root.path(),
        "fixture",
        &marker_plugin_body(&marker),
    );
    std::fs::write(&marker, "run\n").unwrap();

    // Handcraft a journal entry whose dead-man deadline is long past.
    let journal = Journal::new(bed.data_dir.path());
    journal
        .write(&JournalEntry {
            version: JOURNAL_VERSION,
            instance_id: "i-old".to_string(),
            plugin_name: "fixture".to_string(),
            plugin_version: "1".to_string(),
            plugin_digest: digest.clone(),
            params: serde_json::Map::new(),
            started_unix_ms: 1_000_000,
            duration_secs: 1,
            grace_secs: 1,
            deadline_unix: 1_002, // 1970 — unambiguously past
        })
        .unwrap();

    bed.spawn_agent(30, 10);
    let mut session = bed.master.session().await;
    let frames = frames_until_state(&mut session, "i-old", InstanceState::Error).await;
    let statuses = statuses_of(&frames, "i-old");
    assert_eq!(
        statuses.first().unwrap().state,
        wire(InstanceState::Recovering)
    );
    assert!(!marker.exists(), "recovery still ran cleanup");
    assert!(!bed.journal_path("i-old").exists());
}
