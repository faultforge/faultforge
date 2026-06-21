use std::sync::Arc;

use faultforge_master::{MasterService, Registry, new_registry};
use faultforge_proto::v1::{
    AgentMessage, Heartbeat, Register, agent_message, agent_service_client::AgentServiceClient,
    agent_service_server::AgentServiceServer, server_message,
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

// Task 6.1: agent registers, registry entry appears with name == hostname, heartbeat updates last_seen
#[tokio::test]
async fn agent_registers_and_heartbeats_update_last_seen() {
    let (addr, registry) = start_test_server(1).await;

    let channel = Channel::from_shared(addr).unwrap().connect().await.unwrap();
    let mut client = AgentServiceClient::new(channel);

    let (tx, rx) = mpsc::channel::<AgentMessage>(32);
    let response = client.session(ReceiverStream::new(rx)).await.unwrap();
    let mut inbound = response.into_inner();

    // Register
    tx.send(AgentMessage {
        payload: Some(agent_message::Payload::Register(Register {
            hostname: "web-01".to_string(),
        })),
    })
    .await
    .unwrap();

    let msg = inbound.message().await.unwrap().unwrap();
    assert!(
        matches!(msg.payload, Some(server_message::Payload::RegisterAck(_))),
        "expected RegisterAck"
    );

    // Verify registry entry: name == hostname
    let seen_after_register = {
        let reg = registry.lock().unwrap();
        let entry = reg.get("web-01").expect("entry must exist after Register");
        assert_eq!(entry.name, "web-01");
        entry.last_seen
    };

    // Send a heartbeat and verify last_seen advances
    tokio::time::sleep(tokio::time::Duration::from_millis(2)).await;
    tx.send(AgentMessage {
        payload: Some(agent_message::Payload::Heartbeat(Heartbeat {})),
    })
    .await
    .unwrap();

    let msg = inbound.message().await.unwrap().unwrap();
    assert!(
        matches!(msg.payload, Some(server_message::Payload::HeartbeatAck(_))),
        "expected HeartbeatAck"
    );

    let seen_after_heartbeat = registry.lock().unwrap().get("web-01").unwrap().last_seen;
    assert!(
        seen_after_heartbeat >= seen_after_register,
        "last_seen must be updated by heartbeat"
    );
}

// Task 6.3: heartbeat sent before register → FailedPrecondition status, stream closes
#[tokio::test]
async fn heartbeat_before_register_fails_with_precondition() {
    let (addr, _registry) = start_test_server(1).await;

    let channel = Channel::from_shared(addr).unwrap().connect().await.unwrap();
    let mut client = AgentServiceClient::new(channel);

    let (tx, rx) = mpsc::channel::<AgentMessage>(32);
    let response = client.session(ReceiverStream::new(rx)).await.unwrap();
    let mut inbound = response.into_inner();

    // Send Heartbeat as the very first frame, skipping Register.
    tx.send(AgentMessage {
        payload: Some(agent_message::Payload::Heartbeat(Heartbeat {})),
    })
    .await
    .unwrap();

    let result = inbound.message().await;
    let err = result.expect_err("expected an error status, not a successful message");
    assert_eq!(
        err.code(),
        Code::FailedPrecondition,
        "expected FailedPrecondition, got {:?}: {}",
        err.code(),
        err.message()
    );
}

// Task 6.2: second Register for same hostname on a new stream supersedes the first entry
#[tokio::test]
async fn second_register_supersedes_first_entry() {
    let (addr, registry) = start_test_server(1).await;

    // First connection
    let (tx1, rx1) = mpsc::channel::<AgentMessage>(32);
    let ch1 = Channel::from_shared(addr.clone())
        .unwrap()
        .connect()
        .await
        .unwrap();
    let resp1 = AgentServiceClient::new(ch1)
        .session(ReceiverStream::new(rx1))
        .await
        .unwrap();
    let mut inbound1 = resp1.into_inner();

    tx1.send(AgentMessage {
        payload: Some(agent_message::Payload::Register(Register {
            hostname: "web-01".to_string(),
        })),
    })
    .await
    .unwrap();
    inbound1.message().await.unwrap(); // consume RegisterAck

    let first_seen = registry.lock().unwrap().get("web-01").unwrap().last_seen;

    // Small delay so SystemTime advances
    tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;

    // Second connection — same hostname
    let (tx2, rx2) = mpsc::channel::<AgentMessage>(32);
    let ch2 = Channel::from_shared(addr).unwrap().connect().await.unwrap();
    let resp2 = AgentServiceClient::new(ch2)
        .session(ReceiverStream::new(rx2))
        .await
        .unwrap();
    let mut inbound2 = resp2.into_inner();

    tx2.send(AgentMessage {
        payload: Some(agent_message::Payload::Register(Register {
            hostname: "web-01".to_string(),
        })),
    })
    .await
    .unwrap();
    inbound2.message().await.unwrap(); // consume RegisterAck

    // Registry must still have exactly one entry; last_seen updated by second registration
    let reg = registry.lock().unwrap();
    assert_eq!(
        reg.len(),
        1,
        "registry must have exactly one entry for web-01"
    );
    let entry = reg.get("web-01").unwrap();
    assert_eq!(entry.name, "web-01");
    assert!(
        entry.last_seen >= first_seen,
        "second Register must have refreshed last_seen"
    );
}

// Duplicate Register on the same stream → FailedPrecondition; second hostname never enters registry.
#[tokio::test]
async fn duplicate_register_on_same_stream_is_rejected() {
    let (addr, registry) = start_test_server(1).await;

    let channel = Channel::from_shared(addr).unwrap().connect().await.unwrap();
    let mut client = AgentServiceClient::new(channel);

    let (tx, rx) = mpsc::channel::<AgentMessage>(32);
    let response = client.session(ReceiverStream::new(rx)).await.unwrap();
    let mut inbound = response.into_inner();

    // First Register — should succeed.
    tx.send(AgentMessage {
        payload: Some(agent_message::Payload::Register(Register {
            hostname: "web-01".to_string(),
        })),
    })
    .await
    .unwrap();
    let msg = inbound.message().await.unwrap().unwrap();
    assert!(
        matches!(msg.payload, Some(server_message::Payload::RegisterAck(_))),
        "expected RegisterAck for first Register"
    );

    // Second Register on the same stream — must be rejected.
    tx.send(AgentMessage {
        payload: Some(agent_message::Payload::Register(Register {
            hostname: "db-01".to_string(),
        })),
    })
    .await
    .unwrap();
    let result = inbound.message().await;
    let err = result.expect_err("expected an error for duplicate Register");
    assert_eq!(
        err.code(),
        Code::FailedPrecondition,
        "expected FailedPrecondition for duplicate Register, got {:?}: {}",
        err.code(),
        err.message()
    );

    // "db-01" must never have been inserted into the registry.
    assert!(
        registry.lock().unwrap().get("db-01").is_none(),
        "db-01 must not appear in the registry after a rejected duplicate Register"
    );
}

// Register with empty hostname → InvalidArgument; registry stays empty.
#[tokio::test]
async fn register_with_empty_hostname_is_rejected() {
    let (addr, registry) = start_test_server(1).await;

    let channel = Channel::from_shared(addr).unwrap().connect().await.unwrap();
    let mut client = AgentServiceClient::new(channel);

    let (tx, rx) = mpsc::channel::<AgentMessage>(32);
    let response = client.session(ReceiverStream::new(rx)).await.unwrap();
    let mut inbound = response.into_inner();

    tx.send(AgentMessage {
        payload: Some(agent_message::Payload::Register(Register {
            hostname: "".to_string(),
        })),
    })
    .await
    .unwrap();

    let result = inbound.message().await;
    let err = result.expect_err("expected an error for empty hostname");
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
