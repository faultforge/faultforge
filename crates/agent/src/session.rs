use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Request;
use tracing::{error, info};

use faultforge_proto::Hostname;
use faultforge_proto::v1::{
    AgentMessage, Heartbeat, Register, agent_message, agent_service_client::AgentServiceClient,
    server_message,
};

use crate::config::AgentConfig;

// ===== Error type =====

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("could not read local hostname: {0}")]
    Hostname(#[from] std::io::Error),
    #[error("local hostname is empty")]
    EmptyHostname,
    #[error("gRPC transport error: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("gRPC status error: {0}")]
    Status(#[from] tonic::Status),
    #[error("master sent heartbeat_interval_secs = 0; refusing to flood")]
    ZeroHeartbeatInterval,
    #[error("unexpected message: {0}")]
    UnexpectedMessage(String),
    #[error("stream closed: {0}")]
    StreamClosed(String),
    #[error("outbound channel closed")]
    ChannelClosed,
}

// ===== Pure helper functions =====

fn normalize_master_addr(s: &str) -> String {
    if s.contains("://") {
        s.to_string()
    } else {
        format!("http://{s}")
    }
}

fn interpret_register_ack(payload: Option<server_message::Payload>) -> Result<u32, AgentError> {
    match payload {
        Some(server_message::Payload::RegisterAck(ack)) => {
            if ack.heartbeat_interval_secs == 0 {
                return Err(AgentError::ZeroHeartbeatInterval);
            }
            Ok(ack.heartbeat_interval_secs)
        }
        other => Err(AgentError::UnexpectedMessage(format!(
            "expected RegisterAck, got {other:?}"
        ))),
    }
}

fn interpret_heartbeat_ack(payload: Option<server_message::Payload>) -> Result<i64, AgentError> {
    match payload {
        Some(server_message::Payload::HeartbeatAck(ack)) => Ok(ack.server_time_unix_ms),
        other => Err(AgentError::UnexpectedMessage(format!(
            "unexpected message during heartbeat: {other:?}"
        ))),
    }
}

// ===== Agent logic =====

/// Connect to the master and run the register/heartbeat loop forever.
///
/// # Errors
///
/// Returns `Err` if the local hostname cannot be read, the master address is
/// invalid, the gRPC connection fails, or the session stream is closed
/// unexpectedly.
pub async fn run_agent(cfg: AgentConfig) -> Result<(), AgentError> {
    // Read local hostname at startup (no persisted state).
    let raw_hostname = hostname::get()?;
    let hostname =
        Hostname::parse(&raw_hostname.to_string_lossy()).map_err(|_| AgentError::EmptyHostname)?;

    let master_addr = normalize_master_addr(&cfg.master_addr);

    info!(hostname = %hostname, master = %master_addr, "connecting to master");

    // Dial and open Session stream.
    let channel = tonic::transport::Channel::from_shared(master_addr)
        .map_err(|e| AgentError::UnexpectedMessage(format!("invalid master address URI: {e}")))?
        .connect()
        .await
        .map_err(|e| {
            error!("dial failed: {e}");
            e
        })?;

    let mut client = AgentServiceClient::new(channel);

    let (tx, rx) = mpsc::channel::<AgentMessage>(32);
    let response = client
        .session(Request::new(ReceiverStream::new(rx)))
        .await
        .map_err(|e| {
            error!("session failed: {e}");
            e
        })?;

    let mut inbound = response.into_inner();

    // Send Register as first frame.
    tx.send(AgentMessage {
        payload: Some(agent_message::Payload::Register(Register {
            hostname: hostname.to_string(),
        })),
    })
    .await
    .map_err(|_| AgentError::ChannelClosed)?;

    // Await RegisterAck, extract heartbeat_interval_secs.
    let msg = inbound
        .message()
        .await?
        .ok_or_else(|| AgentError::StreamClosed("stream closed before RegisterAck".to_string()))?;

    let interval_secs = interpret_register_ack(msg.payload)?;
    info!(heartbeat_interval_secs = interval_secs, "registered");

    // Heartbeat loop — send Heartbeat every interval_secs, await HeartbeatAck.
    let interval = std::time::Duration::from_secs(u64::from(interval_secs));
    loop {
        tokio::time::sleep(interval).await;

        tx.send(AgentMessage {
            payload: Some(agent_message::Payload::Heartbeat(Heartbeat {})),
        })
        .await
        .map_err(|_| {
            error!("outbound channel closed during heartbeat loop");
            AgentError::ChannelClosed
        })?;

        let msg = inbound.message().await?.ok_or_else(|| {
            error!("stream closed during heartbeat loop");
            AgentError::StreamClosed("stream closed during heartbeat loop".to_string())
        })?;

        let server_time = interpret_heartbeat_ack(msg.payload)?;
        info!(server_time = server_time, "heartbeat ack");
    }
}

// ===== Unit tests =====

#[cfg(test)]
mod tests {
    use super::*;
    use faultforge_proto::v1::{HeartbeatAck, RegisterAck, server_message};

    #[test]
    fn normalize_with_scheme_is_unchanged() {
        assert_eq!(
            normalize_master_addr("http://localhost:50051"),
            "http://localhost:50051"
        );
    }

    #[test]
    fn normalize_without_scheme_prepends_http() {
        assert_eq!(
            normalize_master_addr("localhost:50051"),
            "http://localhost:50051"
        );
    }

    #[test]
    fn register_ack_extracts_interval() {
        let payload = Some(server_message::Payload::RegisterAck(RegisterAck {
            server_time_unix_ms: 1000,
            heartbeat_interval_secs: 5,
        }));
        assert_eq!(interpret_register_ack(payload).unwrap(), 5);
    }

    #[test]
    fn register_ack_zero_interval_is_error() {
        let payload = Some(server_message::Payload::RegisterAck(RegisterAck {
            server_time_unix_ms: 1000,
            heartbeat_interval_secs: 0,
        }));
        assert!(matches!(
            interpret_register_ack(payload),
            Err(AgentError::ZeroHeartbeatInterval)
        ));
    }

    #[test]
    fn heartbeat_ack_extracts_server_time() {
        let payload = Some(server_message::Payload::HeartbeatAck(HeartbeatAck {
            server_time_unix_ms: 42000,
        }));
        assert_eq!(interpret_heartbeat_ack(payload).unwrap(), 42000);
    }

    #[test]
    fn unexpected_payload_is_error() {
        assert!(matches!(
            interpret_register_ack(None),
            Err(AgentError::UnexpectedMessage(_))
        ));
    }
}
