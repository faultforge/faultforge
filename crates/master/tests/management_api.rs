#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end test for the management API: an agent registers over gRPC and the
//! registry is then read back over REST. Both servers share a single registry,
//! mirroring `run_server`'s wiring without depending on its ephemeral-port
//! handling.

use std::sync::Arc;

use faultforge_master::{ManagementState, MasterService, Registry, new_registry, router};
use faultforge_proto::v1::{
    AgentMessage, Register, agent_message, agent_service_client::AgentServiceClient,
    agent_service_server::AgentServiceServer, server_message,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::transport::Channel;

/// Start a gRPC server and a management API server sharing one registry.
/// Returns `(grpc_url, management_base_url, registry)`.
async fn start_servers() -> (String, String, Registry) {
    let registry = new_registry();

    let grpc_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let grpc_addr = grpc_listener.local_addr().unwrap();
    let service = MasterService::new(Arc::clone(&registry), 1);
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
        registry: Arc::clone(&registry),
    });
    tokio::spawn(async move {
        axum::serve(management_listener, app).await.unwrap();
    });

    (
        format!("http://{grpc_addr}"),
        format!("http://{management_addr}"),
        registry,
    )
}

/// Register `hostname` over a fresh gRPC session and wait for the `RegisterAck`.
async fn register_agent_over_grpc(grpc_url: &str, hostname: &str) {
    let channel = Channel::from_shared(grpc_url.to_string())
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = AgentServiceClient::new(channel);

    let (tx, rx) = mpsc::channel::<AgentMessage>(32);
    let response = client.session(ReceiverStream::new(rx)).await.unwrap();
    let mut inbound = response.into_inner();

    tx.send(AgentMessage {
        payload: Some(agent_message::Payload::Register(Register {
            hostname: hostname.to_string(),
        })),
    })
    .await
    .unwrap();

    let msg = inbound.message().await.unwrap().unwrap();
    assert!(
        matches!(msg.payload, Some(server_message::Payload::RegisterAck(_))),
        "expected RegisterAck"
    );

    // The entry is inserted into the shared registry before the ack is sent and
    // is not removed when the session closes (slice 1 has no disconnect sweep),
    // so it remains readable over HTTP after `tx`/`rx` drop here.
}

#[tokio::test]
async fn management_api_exposes_registered_agent() {
    let (grpc_url, management_url, _registry) = start_servers().await;

    register_agent_over_grpc(&grpc_url, "web-01").await;

    let client = reqwest::Client::new();

    let list: Vec<serde_json::Value> = client
        .get(format!("{management_url}/agents"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.len(), 1, "expected exactly one registered agent");
    assert_eq!(list[0]["hostname"], "web-01");
    assert_eq!(list[0]["name"], "web-01");
    assert!(
        list[0]["last_seen_unix_ms"].is_i64() || list[0]["last_seen_unix_ms"].is_u64(),
        "last_seen_unix_ms must be a numeric timestamp"
    );

    let resp = client
        .get(format!("{management_url}/agents/web-01"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let agent: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(agent["hostname"], "web-01");
    assert_eq!(agent["name"], "web-01");

    let resp = client
        .get(format!("{management_url}/agents/unknown-host"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn management_api_returns_empty_array_when_no_agents() {
    let (_grpc_url, management_url, _registry) = start_servers().await;

    let list: Vec<serde_json::Value> = reqwest::Client::new()
        .get(format!("{management_url}/agents"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.is_empty(), "expected empty array when no agents");
}
