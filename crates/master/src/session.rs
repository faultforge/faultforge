use std::sync::Arc;
use std::time::SystemTime;

use tokio::sync::mpsc;
use tonic::{Status, Streaming};
use tracing::{error, info, warn};

use faultforge_proto::Hostname;
use faultforge_proto::unix_ms;
use faultforge_proto::v1::{
    AgentMessage, HeartbeatAck, RegisterAck, ServerMessage, agent_message, server_message,
};

use crate::clock::Clock;
use crate::registry::{Registry, register_agent, update_heartbeat};

// ===== Pure frame processing =====

/// Process a Register frame. Returns Ok((hostname, reply)) on success, Err(Status) otherwise.
fn process_register(
    hostname_str: &str,
    already_registered: bool,
    now: SystemTime,
    heartbeat_interval_secs: u32,
) -> Result<(Hostname, ServerMessage), Status> {
    if already_registered {
        return Err(Status::failed_precondition(
            "duplicate register on active session",
        ));
    }
    let hostname =
        Hostname::parse(hostname_str).map_err(|e| Status::invalid_argument(e.to_string()))?;
    let reply = ServerMessage {
        payload: Some(server_message::Payload::RegisterAck(RegisterAck {
            server_time_unix_ms: unix_ms(now),
            heartbeat_interval_secs,
        })),
    };
    Ok((hostname, reply))
}

/// Process a Heartbeat frame. Returns Ok(reply) on success, Err(Status) otherwise.
fn process_heartbeat(
    hostname: Option<&Hostname>,
    now: SystemTime,
) -> Result<ServerMessage, Status> {
    match hostname {
        None => Err(Status::failed_precondition(
            "heartbeat received before register",
        )),
        Some(_) => Ok(ServerMessage {
            payload: Some(server_message::Payload::HeartbeatAck(HeartbeatAck {
                server_time_unix_ms: unix_ms(now),
            })),
        }),
    }
}

// ===== Session task =====

/// Whether the session loop should keep reading frames or terminate.
enum Flow {
    Continue,
    Stop,
}

/// Live state of one agent's bidirectional session: the shared registry/clock,
/// the outbound reply channel, and the hostname learned from the first Register.
struct Session {
    registry: Registry,
    clock: Arc<dyn Clock>,
    heartbeat_interval_secs: u32,
    tx: mpsc::Sender<Result<ServerMessage, Status>>,
    hostname: Option<Hostname>,
}

impl Session {
    async fn handle_frame(&mut self, payload: Option<agent_message::Payload>) -> Flow {
        match payload {
            Some(agent_message::Payload::Register(reg)) => self.register(reg.hostname).await,
            Some(agent_message::Payload::Heartbeat(_)) => self.heartbeat().await,
            // Fault telemetry frames carry wire schema only in this slice; log each
            // by name and keep the session open (D3: an endpoint receiving a frame it
            // does not handle must not terminate). Kept as explicit per-variant arms so
            // adding a new frame type breaks this match at compile time.
            Some(agent_message::Payload::FaultEvent(_)) => {
                warn!("ignoring FaultEvent frame (not handled in this slice)");
                Flow::Continue
            }
            Some(agent_message::Payload::InstanceStatus(_)) => {
                warn!("ignoring InstanceStatus frame (not handled in this slice)");
                Flow::Continue
            }
            Some(agent_message::Payload::InstanceReport(_)) => {
                warn!("ignoring InstanceReport frame (not handled in this slice)");
                Flow::Continue
            }
            Some(agent_message::Payload::TaintStatus(_)) => {
                warn!("ignoring TaintStatus frame (not handled in this slice)");
                Flow::Continue
            }
            // An unknown or future frame variant: prost decodes an unrecognized oneof
            // field to None. Same rule — log and keep the session open so a newer agent
            // never tears it down.
            None => {
                warn!(
                    "ignoring unknown or future agent frame \
                     (empty payload; not handled in this slice)"
                );
                Flow::Continue
            }
        }
    }

    async fn register(&mut self, hostname_str: String) -> Flow {
        let now = self.clock.now();
        match process_register(
            &hostname_str,
            self.hostname.is_some(),
            now,
            self.heartbeat_interval_secs,
        ) {
            Ok((h, reply)) => {
                register_agent(&self.registry, &h, now);
                info!(hostname = %h, "agent registered");
                self.hostname = Some(h);
                self.send(reply).await
            }
            Err(status) => self.reject(status, "register").await,
        }
    }

    async fn heartbeat(&mut self) -> Flow {
        let now = self.clock.now();
        match process_heartbeat(self.hostname.as_ref(), now) {
            Ok(reply) => {
                if let Some(h) = self.hostname.as_ref() {
                    update_heartbeat(&self.registry, h, now);
                    info!(hostname = %h, "heartbeat");
                }
                self.send(reply).await
            }
            Err(status) => self.reject(status, "heartbeat").await,
        }
    }

    /// Forward a successful reply; stop if the response stream has been dropped.
    async fn send(&self, reply: ServerMessage) -> Flow {
        if self.tx.send(Ok(reply)).await.is_err() {
            Flow::Stop
        } else {
            Flow::Continue
        }
    }

    /// Log a protocol violation, surface it to the agent, and end the session.
    async fn reject(&self, status: Status, context: &str) -> Flow {
        error!("{context} error: {status}");
        let _ = self.tx.send(Err(status)).await;
        Flow::Stop
    }
}

/// Drive one agent session to completion: read frames off `stream`, dispatch each,
/// and write replies to `tx` until the stream closes or a protocol error ends it.
///
/// Spawned by the gRPC `session` handler; owns its registry/clock handles so the
/// future is `'static` and `Send`.
pub(crate) async fn run(
    mut stream: Streaming<AgentMessage>,
    tx: mpsc::Sender<Result<ServerMessage, Status>>,
    registry: Registry,
    heartbeat_interval_secs: u32,
    clock: Arc<dyn Clock>,
) {
    let mut session = Session {
        registry,
        clock,
        heartbeat_interval_secs,
        tx,
        hostname: None,
    };

    loop {
        match stream.message().await {
            Ok(Some(msg)) => {
                if let Flow::Stop = session.handle_frame(msg.payload).await {
                    break;
                }
            }
            Ok(None) => break,
            Err(e) => {
                error!("stream error: {e}");
                let _ = session.tx.send(Err(e)).await;
                break;
            }
        }
    }
    info!("session closed for hostname: {:?}", session.hostname);
}

// ===== Unit tests =====

#[cfg(test)]
mod tests {
    use super::*;
    use faultforge_proto::Hostname;
    use faultforge_proto::v1::server_message;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn process_register_rejects_empty_hostname() {
        let t = UNIX_EPOCH + Duration::from_secs(1000);
        let result = process_register("", false, t, 5);
        assert!(result.is_err());
    }

    #[test]
    fn process_register_rejects_duplicate() {
        let t = UNIX_EPOCH + Duration::from_secs(1000);
        let result = process_register("web-01", true, t, 5);
        assert!(result.is_err());
    }

    #[test]
    fn process_register_success() {
        let t = UNIX_EPOCH + Duration::from_secs(1000);
        let result = process_register("web-01", false, t, 5);
        assert!(result.is_ok());
        let (hostname, msg) = result.unwrap();
        assert_eq!(hostname.as_str(), "web-01");
        match msg.payload {
            Some(server_message::Payload::RegisterAck(ack)) => {
                assert_eq!(ack.heartbeat_interval_secs, 5);
                assert_eq!(ack.server_time_unix_ms, faultforge_proto::unix_ms(t));
            }
            _ => panic!("expected RegisterAck"),
        }
    }

    #[test]
    fn process_heartbeat_before_register_fails() {
        let t = UNIX_EPOCH + Duration::from_secs(1000);
        let result = process_heartbeat(None, t);
        assert!(result.is_err());
    }

    #[test]
    fn process_heartbeat_success() {
        let t = UNIX_EPOCH + Duration::from_secs(1000);
        let hostname = Hostname::parse("web-01").unwrap();
        let result = process_heartbeat(Some(&hostname), t);
        assert!(result.is_ok());
        match result.unwrap().payload {
            Some(server_message::Payload::HeartbeatAck(ack)) => {
                assert_eq!(ack.server_time_unix_ms, faultforge_proto::unix_ms(t));
            }
            _ => panic!("expected HeartbeatAck"),
        }
    }
}
