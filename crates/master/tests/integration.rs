#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use faultforge_master::{MasterService, Registry, new_registry};
use faultforge_proto::Hostname;
use faultforge_proto::v1::{
    AgentMessage, Heartbeat, InstanceState, InstanceStatus, Register, ServerMessage, agent_message,
    agent_service_client::AgentServiceClient, agent_service_server::AgentServiceServer,
    server_message,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::Code;
use tonic::transport::Channel;

async fn start_test_server(heartbeat_interval_secs: u32) -> (String, Registry) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let registry = new_registry();
    let service = MasterService::new(Arc::clone(&registry), heartbeat_interval_secs);

    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(AgentServiceServer::new(service))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    (format!("http://{addr}"), registry)
}

// ===== Test helpers =====
//
// A two-level seam over the gRPC handshake. `connect_and_open_session` is the
// raw base used by tests that drive the protocol by hand (sending a heartbeat or
// a malformed register as the very first frame, where no `RegisterAck` arrives).
// `connect_and_register` layers a successful registration on top for the common
// case. Both return the live stream so callers can keep sending frames.

/// Construct a validated `Hostname` for registry assertions.
fn hostname(s: &str) -> Hostname {
    Hostname::parse(s).unwrap()
}

/// Build a `Register` agent frame for `hostname`.
fn register_frame(hostname: &str) -> AgentMessage {
    AgentMessage {
        payload: Some(agent_message::Payload::Register(Register {
            hostname: hostname.to_string(),
        })),
    }
}

/// Build a `Heartbeat` agent frame.
fn heartbeat_frame() -> AgentMessage {
    AgentMessage {
        payload: Some(agent_message::Payload::Heartbeat(Heartbeat {})),
    }
}

/// Dial the master and open a bidirectional `Session` stream, returning the
/// outbound sender and the inbound message stream.
async fn connect_and_open_session(
    addr: &str,
) -> (mpsc::Sender<AgentMessage>, tonic::Streaming<ServerMessage>) {
    let channel = Channel::from_shared(addr.to_string())
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = AgentServiceClient::new(channel);
    let (tx, rx) = mpsc::channel::<AgentMessage>(32);
    let inbound = client
        .session(ReceiverStream::new(rx))
        .await
        .unwrap()
        .into_inner();
    (tx, inbound)
}

/// Open a session and complete a successful registration for `hostname`,
/// consuming the `RegisterAck`. Returns the live stream so the caller can send
/// further frames (heartbeats, a superseding register, …).
async fn connect_and_register(
    addr: &str,
    hostname: &str,
) -> (mpsc::Sender<AgentMessage>, tonic::Streaming<ServerMessage>) {
    let (tx, mut inbound) = connect_and_open_session(addr).await;
    tx.send(register_frame(hostname)).await.unwrap();
    let ack = inbound.message().await.unwrap().unwrap();
    assert!(
        matches!(ack.payload, Some(server_message::Payload::RegisterAck(_))),
        "expected RegisterAck after registering {hostname}"
    );
    (tx, inbound)
}

#[tokio::test]
async fn agent_registers_and_heartbeats_update_last_seen() {
    let (addr, registry) = start_test_server(1).await;
    let (tx, mut inbound) = connect_and_register(&addr, "web-01").await;

    let seen_after_register = {
        let reg = registry.lock().unwrap();
        let entry = reg
            .get(&hostname("web-01"))
            .expect("entry must exist after Register");
        assert_eq!(entry.name, "web-01");
        entry.last_seen
    };

    tokio::time::sleep(tokio::time::Duration::from_millis(2)).await;
    tx.send(heartbeat_frame()).await.unwrap();

    let msg = inbound.message().await.unwrap().unwrap();
    assert!(
        matches!(msg.payload, Some(server_message::Payload::HeartbeatAck(_))),
        "expected HeartbeatAck"
    );

    let seen_after_heartbeat = registry
        .lock()
        .unwrap()
        .get(&hostname("web-01"))
        .unwrap()
        .last_seen;
    assert!(
        seen_after_heartbeat >= seen_after_register,
        "last_seen must be updated by heartbeat"
    );
}

#[tokio::test]
async fn heartbeat_before_register_fails_with_precondition() {
    let (addr, _registry) = start_test_server(1).await;
    let (tx, mut inbound) = connect_and_open_session(&addr).await;

    tx.send(heartbeat_frame()).await.unwrap();

    let err = inbound
        .message()
        .await
        .expect_err("expected an error status, not a successful message");
    assert_eq!(
        err.code(),
        Code::FailedPrecondition,
        "expected FailedPrecondition, got {:?}: {}",
        err.code(),
        err.message()
    );
}

#[tokio::test]
async fn second_register_supersedes_first_entry() {
    let (addr, registry) = start_test_server(1).await;

    // First connection — kept alive so the supersede happens while it is still open.
    let (_tx1, _inbound1) = connect_and_register(&addr, "web-01").await;

    let first_seen = registry
        .lock()
        .unwrap()
        .get(&hostname("web-01"))
        .unwrap()
        .last_seen;

    // Small delay so SystemTime advances
    tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;

    let (_tx2, _inbound2) = connect_and_register(&addr, "web-01").await;

    let reg = registry.lock().unwrap();
    assert_eq!(
        reg.len(),
        1,
        "registry must have exactly one entry for web-01"
    );
    let entry = reg.get(&hostname("web-01")).unwrap();
    assert_eq!(entry.name, "web-01");
    assert!(
        entry.last_seen >= first_seen,
        "second Register must have refreshed last_seen"
    );
}

#[tokio::test]
async fn duplicate_register_on_same_stream_is_rejected() {
    let (addr, registry) = start_test_server(1).await;
    let (tx, mut inbound) = connect_and_register(&addr, "web-01").await;

    tx.send(register_frame("db-01")).await.unwrap();
    let err = inbound
        .message()
        .await
        .expect_err("expected an error for duplicate Register");
    assert_eq!(
        err.code(),
        Code::FailedPrecondition,
        "expected FailedPrecondition for duplicate Register, got {:?}: {}",
        err.code(),
        err.message()
    );

    assert!(
        registry.lock().unwrap().get(&hostname("db-01")).is_none(),
        "db-01 must not appear in the registry after a rejected duplicate Register"
    );
}

// D3: an agent→master fault telemetry frame the master does not handle in this
// slice is logged and ignored; the Session stream stays open and still acks.
#[tokio::test]
async fn fault_frame_does_not_kill_session() {
    let (addr, _registry) = start_test_server(1).await;
    let (tx, mut inbound) = connect_and_register(&addr, "web-01").await;

    // Send an InstanceStatus frame — valid wire, but unhandled behaviour here.
    tx.send(AgentMessage {
        payload: Some(agent_message::Payload::InstanceStatus(InstanceStatus {
            instance_id: "exp1-web01-0".to_string(),
            state: InstanceState::Active as i32,
            ts_unix_ms: 1,
            reason: String::new(),
        })),
    })
    .await
    .unwrap();

    tx.send(heartbeat_frame()).await.unwrap();
    let msg = inbound.message().await.unwrap().unwrap();
    assert!(
        matches!(msg.payload, Some(server_message::Payload::HeartbeatAck(_))),
        "session must survive an unhandled fault frame and keep acking heartbeats"
    );
}

// D3 / forward compatibility: a frame whose oneof payload is empty — exactly how
// prost decodes a variant added by a newer agent than this master — must be logged
// and skipped, not close the session.
#[tokio::test]
async fn unknown_frame_does_not_kill_session() {
    let (addr, _registry) = start_test_server(1).await;
    let (tx, mut inbound) = connect_and_register(&addr, "web-01").await;

    // An AgentMessage with no known payload stands in for a future frame variant
    // this master build does not recognize (prost decodes it to `payload: None`).
    tx.send(AgentMessage { payload: None }).await.unwrap();

    tx.send(heartbeat_frame()).await.unwrap();
    let msg = inbound.message().await.unwrap().unwrap();
    assert!(
        matches!(msg.payload, Some(server_message::Payload::HeartbeatAck(_))),
        "session must survive an unknown/future frame and keep acking heartbeats"
    );
}

#[tokio::test]
async fn register_with_empty_hostname_is_rejected() {
    let (addr, registry) = start_test_server(1).await;
    let (tx, mut inbound) = connect_and_open_session(&addr).await;

    tx.send(register_frame("")).await.unwrap();

    let err = inbound
        .message()
        .await
        .expect_err("expected an error for empty hostname");
    assert_eq!(
        err.code(),
        Code::InvalidArgument,
        "expected InvalidArgument for empty hostname, got {:?}: {}",
        err.code(),
        err.message()
    );

    assert!(
        registry.lock().unwrap().is_empty(),
        "registry must remain empty after rejected Register"
    );
}
