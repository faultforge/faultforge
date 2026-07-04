#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests of master fault dispatch against scripted fake agents
//! (design D9): the real master (gRPC + management on ephemeral ports), tonic
//! clients that register and play back recorded status sequences, and HTTP
//! assertions over the management surface.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};

use faultforge_fault::digest::Digest;
use faultforge_fault::proto::to_wire_i32;
use faultforge_fault::state::InstanceState;
use faultforge_master::clock::SystemClock;
use faultforge_master::{
    Dispatcher, ManagementState, MasterService, load_catalog, new_registry, router,
};
use faultforge_proto::v1::agent_service_client::AgentServiceClient;
use faultforge_proto::v1::agent_service_server::AgentServiceServer;
use faultforge_proto::v1::{
    AbortFault, AgentMessage, Heartbeat, InstanceReport, InstanceStatus, Register, RunFault,
    ServerMessage, TaintStatus, agent_message, server_message,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(20);

// ===== Master under test =====

struct TestMaster {
    grpc_url: String,
    management_url: String,
    _catalog_dir: tempfile::TempDir,
    client: reqwest::Client,
}

/// Install a minimal catalog plugin (`fixture@1`, empty params schema) and
/// return its digest string.
fn install_fixture(root: &Path) -> String {
    let manifest = "name: fixture\nversion: \"1\"\nentrypoint: ./run.sh\nmax_duration_secs: 600\n";
    let script = "#!/bin/sh\nexit 0\n";
    let dir = root.join("fixture@1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("manifest.yaml"), manifest).unwrap();
    std::fs::write(dir.join("run.sh"), script).unwrap();
    Digest::compute(manifest.as_bytes(), script.as_bytes()).to_string()
}

async fn start_master(grace_secs: u32, deadline_margin_ms: i64) -> (TestMaster, String) {
    let catalog_dir = tempfile::tempdir().unwrap();
    let digest = install_fixture(catalog_dir.path());
    let catalog = load_catalog(catalog_dir.path());
    let registry = new_registry();
    let dispatcher = Arc::new(
        Dispatcher::new(
            catalog,
            Arc::clone(&registry),
            grace_secs,
            Arc::new(SystemClock),
        )
        .with_deadline_margin_ms(deadline_margin_ms),
    );

    let grpc_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let grpc_addr = grpc_listener.local_addr().unwrap();
    let service = MasterService::new(Arc::clone(&dispatcher), 1);
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(AgentServiceServer::new(service))
            .serve_with_incoming(TcpListenerStream::new(grpc_listener))
            .await
            .unwrap();
    });

    let management_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let management_addr = management_listener.local_addr().unwrap();
    let app = router(ManagementState {
        registry,
        dispatcher,
    });
    tokio::spawn(async move {
        axum::serve(management_listener, app).await.unwrap();
    });

    (
        TestMaster {
            grpc_url: format!("http://{grpc_addr}"),
            management_url: format!("http://{management_addr}"),
            _catalog_dir: catalog_dir,
            client: reqwest::Client::new(),
        },
        digest,
    )
}

impl TestMaster {
    async fn post(&self, path: &str, body: Option<Value>) -> (reqwest::StatusCode, Value) {
        let mut req = self.client.post(format!("{}{path}", self.management_url));
        if let Some(body) = body {
            req = req.json(&body);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status();
        let body = resp.json().await.unwrap_or(Value::Null);
        (status, body)
    }

    async fn get(&self, path: &str) -> (reqwest::StatusCode, Value) {
        let resp = self
            .client
            .get(format!("{}{path}", self.management_url))
            .send()
            .await
            .unwrap();
        let status = resp.status();
        let body = resp.json().await.unwrap_or(Value::Null);
        (status, body)
    }

    async fn run_experiment(&self, definition: Value) -> (reqwest::StatusCode, Value) {
        self.post("/experiments", Some(definition)).await
    }

    /// Poll `GET /experiments/{id}` until its `state` equals `state`.
    async fn wait_for_state(&self, id: &str, state: &str) -> Value {
        let deadline = std::time::Instant::now() + TEST_TIMEOUT;
        loop {
            let (status, body) = self.get(&format!("/experiments/{id}")).await;
            assert_eq!(status, reqwest::StatusCode::OK, "experiment {id} missing");
            if body["state"] == state {
                return body;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {id} to reach {state}; last: {body}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

/// A one-action definition targeting `hosts` with the fixture plugin.
fn definition(hosts: &[&str], duration_secs: u32) -> Value {
    json!({
        "name": "it",
        "actions": [{
            "hosts": hosts,
            "plugin": {"name": "fixture", "version": "1"},
            "params": {},
            "duration_secs": duration_secs,
        }],
    })
}

// ===== Fake agent =====

struct FakeAgent {
    tx: mpsc::Sender<AgentMessage>,
    inbound: tonic::Streaming<ServerMessage>,
}

impl FakeAgent {
    /// Connect, register as `hostname`, and consume the `RegisterAck`.
    async fn register(grpc_url: &str, hostname: &str) -> Self {
        let channel = tonic::transport::Channel::from_shared(grpc_url.to_string())
            .unwrap()
            .connect()
            .await
            .unwrap();
        let mut client = AgentServiceClient::new(channel);
        let (tx, rx) = mpsc::channel::<AgentMessage>(64);
        let mut inbound = client
            .session(ReceiverStream::new(rx))
            .await
            .unwrap()
            .into_inner();
        tx.send(AgentMessage {
            payload: Some(agent_message::Payload::Register(Register {
                hostname: hostname.to_string(),
            })),
        })
        .await
        .unwrap();
        let ack = tokio::time::timeout(TEST_TIMEOUT, inbound.message())
            .await
            .expect("timed out waiting for RegisterAck")
            .unwrap()
            .unwrap();
        assert!(matches!(
            ack.payload,
            Some(server_message::Payload::RegisterAck(_))
        ));
        Self { tx, inbound }
    }

    async fn recv(&mut self) -> ServerMessage {
        tokio::time::timeout(TEST_TIMEOUT, self.inbound.message())
            .await
            .expect("timed out waiting for a server frame")
            .unwrap()
            .expect("stream closed")
    }

    async fn expect_run_fault(&mut self) -> RunFault {
        loop {
            match self.recv().await.payload {
                Some(server_message::Payload::RunFault(run)) => return run,
                Some(server_message::Payload::HeartbeatAck(_)) => {}
                other => panic!("expected RunFault, got {other:?}"),
            }
        }
    }

    async fn expect_abort_fault(&mut self) -> AbortFault {
        loop {
            match self.recv().await.payload {
                Some(server_message::Payload::AbortFault(abort)) => return abort,
                Some(server_message::Payload::HeartbeatAck(_)) => {}
                other => panic!("expected AbortFault, got {other:?}"),
            }
        }
    }

    async fn expect_clear_taint(&mut self) {
        loop {
            match self.recv().await.payload {
                Some(server_message::Payload::ClearTaint(_)) => return,
                Some(server_message::Payload::HeartbeatAck(_)) => {}
                other => panic!("expected ClearTaint, got {other:?}"),
            }
        }
    }

    /// Assert no `RunFault` arrives within `window` (heartbeat acks may).
    async fn expect_no_run_fault(&mut self, window: Duration) {
        let result = tokio::time::timeout(window, self.inbound.message()).await;
        if let Ok(Ok(Some(msg))) = result {
            assert!(
                !matches!(msg.payload, Some(server_message::Payload::RunFault(_))),
                "unexpected RunFault: {msg:?}"
            );
        }
    }

    async fn send(&self, msg: AgentMessage) {
        self.tx.send(msg).await.unwrap();
    }

    async fn send_status(&self, instance_id: &str, state: InstanceState, reason: &str) {
        self.send(AgentMessage {
            payload: Some(agent_message::Payload::InstanceStatus(status(
                instance_id,
                state,
                reason,
            ))),
        })
        .await;
    }

    async fn send_report(&self, statuses: Vec<InstanceStatus>) {
        self.send(AgentMessage {
            payload: Some(agent_message::Payload::InstanceReport(InstanceReport {
                statuses,
            })),
        })
        .await;
    }

    async fn send_taint(&self, tainted: bool, reason: &str) {
        self.send(AgentMessage {
            payload: Some(agent_message::Payload::TaintStatus(TaintStatus {
                tainted,
                reason: reason.to_string(),
                ts_unix_ms: 1,
            })),
        })
        .await;
    }

    /// Walk an instance through the full happy lifecycle to `DONE`.
    async fn complete_instance(&self, instance_id: &str) {
        for state in [
            InstanceState::Preflight,
            InstanceState::Injecting,
            InstanceState::Active,
            InstanceState::Recovering,
            InstanceState::Done,
        ] {
            self.send_status(instance_id, state, "").await;
        }
    }
}

fn status(instance_id: &str, state: InstanceState, reason: &str) -> InstanceStatus {
    InstanceStatus {
        instance_id: instance_id.to_string(),
        state: to_wire_i32(state),
        ts_unix_ms: 1,
        reason: reason.to_string(),
        plugin_digest: String::new(),
    }
}

// ===== 7.2 Happy path =====

#[tokio::test]
async fn happy_path_two_hosts_two_actions_completes() {
    let (master, digest) = start_master(7, 30_000).await;
    let mut web = FakeAgent::register(&master.grpc_url, "web-01").await;
    let mut db = FakeAgent::register(&master.grpc_url, "db-01").await;

    let def = json!({
        "name": "happy",
        "actions": [
            {"hosts": ["web-01", "db-01"], "plugin": {"name": "fixture", "version": "1"},
             "params": {}, "duration_secs": 5},
            {"hosts": ["web-01"], "plugin": {"name": "fixture", "version": "1"},
             "params": {}, "duration_secs": 3},
        ],
    });
    let (code, body) = master.run_experiment(def).await;
    assert_eq!(code, reqwest::StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().unwrap().to_string();
    assert_eq!(body["state"], "RUNNING");
    assert_eq!(body["instances"].as_array().unwrap().len(), 3);

    // Deterministic ids; single salvo carrying digest, params, and grace.
    let web_0 = web.expect_run_fault().await;
    assert_eq!(web_0.instance_id, format!("{id}:web-01:0"));
    assert_eq!(web_0.plugin_digest, digest);
    assert_eq!(web_0.params_json, "{}");
    assert_eq!(web_0.duration_secs, 5);
    assert_eq!(web_0.grace_secs, 7);
    let web_1 = web.expect_run_fault().await;
    assert_eq!(web_1.instance_id, format!("{id}:web-01:1"));
    assert_eq!(web_1.duration_secs, 3);
    let db_0 = db.expect_run_fault().await;
    assert_eq!(db_0.instance_id, format!("{id}:db-01:0"));

    web.complete_instance(&web_0.instance_id).await;
    web.complete_instance(&web_1.instance_id).await;
    db.complete_instance(&db_0.instance_id).await;

    let body = master.wait_for_state(&id, "COMPLETED").await;
    assert_eq!(body["outcome"], "COMPLETED");
    assert!(body["cause"].is_null(), "clean completion has no cause");
    for instance in body["instances"].as_array().unwrap() {
        assert_eq!(instance["state"], "DONE");
    }

    // The summary list shows the concluded run.
    let (_, list) = master.get("/experiments").await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["state"], "COMPLETED");
}

// ===== 6.3 / VALIDATE =====

#[tokio::test]
async fn invalid_experiment_is_rejected_with_all_reasons_and_nothing_dispatched() {
    let (master, _digest) = start_master(5, 30_000).await;
    let mut web = FakeAgent::register(&master.grpc_url, "web-01").await;

    let def = json!({
        "name": "bad",
        "actions": [
            {"hosts": ["web-01"], "plugin": {"name": "ghost", "version": "1"},
             "params": {}, "duration_secs": 5},
            {"hosts": ["absent-01"], "plugin": {"name": "fixture", "version": "1"},
             "params": {}, "duration_secs": 5},
        ],
    });
    let (code, body) = master.run_experiment(def).await;
    assert_eq!(code, reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let errors: Vec<String> = body["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap().to_string())
        .collect();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors.iter().any(|e| e.contains("unknown plugin ghost@1")));
    assert!(
        errors
            .iter()
            .any(|e| e.contains("'absent-01' is not registered"))
    );

    // No record was created and nothing reached the connected agent.
    let (_, list) = master.get("/experiments").await;
    assert!(list.as_array().unwrap().is_empty());
    web.expect_no_run_fault(Duration::from_millis(300)).await;
}

// ===== 7.3 Kill-switch paths =====

#[tokio::test]
async fn instance_error_aborts_the_rest_and_outcome_is_error() {
    let (master, _digest) = start_master(5, 30_000).await;
    let mut web = FakeAgent::register(&master.grpc_url, "web-01").await;
    let mut db = FakeAgent::register(&master.grpc_url, "db-01").await;

    let (code, body) = master
        .run_experiment(definition(&["web-01", "db-01"], 60))
        .await;
    assert_eq!(code, reqwest::StatusCode::CREATED);
    let id = body["id"].as_str().unwrap().to_string();
    let web_run = web.expect_run_fault().await;
    let db_run = db.expect_run_fault().await;

    web.send_status(
        &web_run.instance_id,
        InstanceState::Error,
        "cleanup exploded",
    )
    .await;

    // The kill-switch reaches the healthy host.
    let abort = db.expect_abort_fault().await;
    assert_eq!(abort.instance_id, db_run.instance_id);
    db.send_status(
        &db_run.instance_id,
        InstanceState::Aborted,
        "abort requested",
    )
    .await;

    let body = master.wait_for_state(&id, "ERROR").await;
    assert_eq!(body["outcome"], "ERROR");
    assert_eq!(body["cause"]["hostname"], "web-01");
    assert_eq!(body["cause"]["instance_id"], web_run.instance_id.as_str());
    assert!(
        body["cause"]["reason"]
            .as_str()
            .unwrap()
            .contains("cleanup exploded")
    );
}

#[tokio::test]
async fn agent_self_abort_halts_the_experiment_as_aborted() {
    let (master, _digest) = start_master(5, 30_000).await;
    let mut web = FakeAgent::register(&master.grpc_url, "web-01").await;
    let mut db = FakeAgent::register(&master.grpc_url, "db-01").await;

    let (_, body) = master
        .run_experiment(definition(&["web-01", "db-01"], 60))
        .await;
    let id = body["id"].as_str().unwrap().to_string();
    let web_run = web.expect_run_fault().await;
    let db_run = db.expect_run_fault().await;

    // An abort the master never requested (e.g. master-loss self-abort).
    web.send_status(&web_run.instance_id, InstanceState::Aborted, "self-abort")
        .await;
    db.expect_abort_fault().await;
    db.send_status(
        &db_run.instance_id,
        InstanceState::Aborted,
        "abort requested",
    )
    .await;

    let body = master.wait_for_state(&id, "ABORTED").await;
    assert_eq!(body["outcome"], "ABORTED");
    assert!(
        body["cause"]["reason"]
            .as_str()
            .unwrap()
            .contains("agent-initiated abort")
    );
}

#[tokio::test]
async fn taint_mid_run_kills_and_dominates_as_error() {
    let (master, _digest) = start_master(5, 30_000).await;
    let mut web = FakeAgent::register(&master.grpc_url, "web-01").await;

    let (_, body) = master.run_experiment(definition(&["web-01"], 60)).await;
    let id = body["id"].as_str().unwrap().to_string();
    let run = web.expect_run_fault().await;
    web.send_status(&run.instance_id, InstanceState::Active, "")
        .await;

    web.send_taint(true, "cleanup failed twice").await;
    let abort = web.expect_abort_fault().await;
    assert_eq!(abort.instance_id, run.instance_id);
    // Even a clean-looking ABORTED cannot rescue a tainted host from ERROR.
    web.send_status(&run.instance_id, InstanceState::Aborted, "abort requested")
        .await;

    let body = master.wait_for_state(&id, "ERROR").await;
    assert_eq!(body["outcome"], "ERROR");
    assert!(
        body["cause"]["reason"]
            .as_str()
            .unwrap()
            .contains("TAINTED")
    );

    // 6.3: the quarantine is visible on the agent views.
    let (_, agents) = master.get("/agents").await;
    assert_eq!(agents[0]["hostname"], "web-01");
    assert_eq!(agents[0]["tainted"], true);
}

#[tokio::test]
async fn operator_halt_is_most_available_and_concludes_aborted() {
    let (master, _digest) = start_master(5, 30_000).await;
    let mut web = FakeAgent::register(&master.grpc_url, "web-01").await;
    // A second target that is registered but disconnected by halt time.
    {
        let ghost = FakeAgent::register(&master.grpc_url, "ghost-01").await;
        drop(ghost);
    }
    // Wait until the master noticed the closed session.
    let deadline = std::time::Instant::now() + TEST_TIMEOUT;
    loop {
        let (code, _) = master.post("/agents/ghost-01/clear-taint", None).await;
        if code == reqwest::StatusCode::CONFLICT {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut ghost = FakeAgent::register(&master.grpc_url, "ghost-01").await;

    let (code, body) = master
        .run_experiment(definition(&["web-01", "ghost-01"], 60))
        .await;
    assert_eq!(code, reqwest::StatusCode::CREATED);
    let id = body["id"].as_str().unwrap().to_string();
    let web_run = web.expect_run_fault().await;
    let ghost_run = ghost.expect_run_fault().await;
    // ghost-01 vanishes mid-experiment; halt must still succeed.
    drop(ghost);

    let (code, _) = master.post(&format!("/experiments/{id}/halt"), None).await;
    assert_eq!(code, reqwest::StatusCode::ACCEPTED);
    let abort = web.expect_abort_fault().await;
    assert_eq!(abort.instance_id, web_run.instance_id);
    web.send_status(
        &web_run.instance_id,
        InstanceState::Aborted,
        "abort requested",
    )
    .await;

    // The vanished host's instance resolves via a late report (reconnect path);
    // send it from a fresh session to prove reconciliation still applies.
    let ghost = FakeAgent::register(&master.grpc_url, "ghost-01").await;
    ghost
        .send_status(&ghost_run.instance_id, InstanceState::Aborted, "self-abort")
        .await;

    let body = master.wait_for_state(&id, "ABORTED").await;
    assert_eq!(body["outcome"], "ABORTED");
    assert!(
        body["cause"]["reason"]
            .as_str()
            .unwrap()
            .contains("operator halt")
    );

    // Halting a concluded experiment conflicts; unknown ids are not found.
    let (code, _) = master.post(&format!("/experiments/{id}/halt"), None).await;
    assert_eq!(code, reqwest::StatusCode::CONFLICT);
    let (code, _) = master.post("/experiments/exp-nope/halt", None).await;
    assert_eq!(code, reqwest::StatusCode::NOT_FOUND);
}

// ===== 7.4 Resilience =====

#[tokio::test]
async fn reconnect_reconciles_by_report_without_re_dispatch() {
    let (master, _digest) = start_master(5, 30_000).await;
    let mut web = FakeAgent::register(&master.grpc_url, "web-01").await;

    let (_, body) = master.run_experiment(definition(&["web-01"], 60)).await;
    let id = body["id"].as_str().unwrap().to_string();
    let run = web.expect_run_fault().await;
    web.send_status(&run.instance_id, InstanceState::Active, "")
        .await;
    drop(web);

    // Fresh session: the report is accepted as truth and nothing is re-sent.
    let mut web = FakeAgent::register(&master.grpc_url, "web-01").await;
    web.send_report(vec![status(&run.instance_id, InstanceState::Active, "")])
        .await;
    web.expect_no_run_fault(Duration::from_millis(300)).await;

    let (_, body) = master.get(&format!("/experiments/{id}")).await;
    assert_eq!(body["instances"][0]["state"], "ACTIVE");

    web.send_status(&run.instance_id, InstanceState::Recovering, "")
        .await;
    web.send_status(&run.instance_id, InstanceState::Done, "")
        .await;
    let body = master.wait_for_state(&id, "COMPLETED").await;
    assert_eq!(body["outcome"], "COMPLETED");
}

#[tokio::test]
async fn vanished_agent_is_resolved_by_the_deadline_as_error() {
    // duration 1s + grace 1s + margin 500ms: the record must resolve itself.
    let (master, _digest) = start_master(1, 500).await;
    let web = FakeAgent::register(&master.grpc_url, "web-01").await;

    let (_, body) = master.run_experiment(definition(&["web-01"], 1)).await;
    let id = body["id"].as_str().unwrap().to_string();
    drop(web); // never acknowledges anything

    let body = master.wait_for_state(&id, "ERROR").await;
    assert_eq!(body["outcome"], "ERROR");
    assert_eq!(
        body["instances"][0]["reason"],
        "unresolved at experiment deadline"
    );
    assert!(
        body["cause"]["reason"]
            .as_str()
            .unwrap()
            .contains("unresolved at experiment deadline")
    );
}

#[tokio::test]
async fn unknown_instance_frames_are_logged_and_session_survives() {
    // A fresh master stands in for one that restarted mid-experiment: the
    // store is empty and incoming frames reference instances it never minted.
    let (master, _digest) = start_master(5, 30_000).await;
    let mut web = FakeAgent::register(&master.grpc_url, "web-01").await;

    web.send_status("exp-999-9:web-01:0", InstanceState::Active, "")
        .await;
    web.send_report(vec![status(
        "exp-999-9:web-01:0",
        InstanceState::Active,
        "",
    )])
    .await;

    // The session still acks heartbeats and the store stays empty.
    web.send(AgentMessage {
        payload: Some(agent_message::Payload::Heartbeat(Heartbeat {})),
    })
    .await;
    loop {
        if let Some(server_message::Payload::HeartbeatAck(_)) = web.recv().await.payload {
            break;
        }
    }
    let (_, list) = master.get("/experiments").await;
    assert!(list.as_array().unwrap().is_empty());
    let (code, _) = master.get("/experiments/exp-999-9").await;
    assert_eq!(code, reqwest::StatusCode::NOT_FOUND);
}

// ===== 7.5 Clear-taint end-to-end =====

#[tokio::test]
async fn clear_taint_round_trip_restores_targetability() {
    let (master, _digest) = start_master(5, 30_000).await;
    let mut web = FakeAgent::register(&master.grpc_url, "web-01").await;

    web.send_taint(true, "cleanup failed twice").await;
    let deadline = std::time::Instant::now() + TEST_TIMEOUT;
    loop {
        let (_, agents) = master.get("/agents").await;
        if agents[0]["tainted"] == true {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // A tainted target fails VALIDATE.
    let (code, body) = master.run_experiment(definition(&["web-01"], 5)).await;
    assert_eq!(code, reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["errors"][0].as_str().unwrap().contains("TAINTED"));

    // Unknown host -> 404.
    let (code, _) = master.post("/agents/nope-01/clear-taint", None).await;
    assert_eq!(code, reqwest::StatusCode::NOT_FOUND);

    // Clear: 202, the agent receives the frame, answers, and the flag drops.
    let (code, _) = master.post("/agents/web-01/clear-taint", None).await;
    assert_eq!(code, reqwest::StatusCode::ACCEPTED);
    web.expect_clear_taint().await;
    web.send_taint(false, "").await;
    let deadline = std::time::Instant::now() + TEST_TIMEOUT;
    loop {
        let (_, agents) = master.get("/agents").await;
        if agents[0]["tainted"] == false {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The host is targetable again.
    let (code, _) = master.run_experiment(definition(&["web-01"], 5)).await;
    assert_eq!(code, reqwest::StatusCode::CREATED);
    web.expect_run_fault().await;

    // Disconnected host -> 409 (registered, no live session).
    drop(web);
    let deadline = std::time::Instant::now() + TEST_TIMEOUT;
    loop {
        let (code, _) = master.post("/agents/web-01/clear-taint", None).await;
        if code == reqwest::StatusCode::CONFLICT {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
