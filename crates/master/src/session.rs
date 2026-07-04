use std::sync::Arc;
use std::time::SystemTime;

use tokio::sync::mpsc;
use tonic::{Status, Streaming};
use tracing::{error, info, warn};

use faultforge_fault::proto::from_wire_i32;
use faultforge_proto::Hostname;
use faultforge_proto::unix_ms;
use faultforge_proto::v1::{
    AgentMessage, HeartbeatAck, InstanceReport, InstanceStatus, RegisterAck, ServerMessage,
    TaintStatus, agent_message, server_message,
};

use faultforge_fault::state::InstanceState;

use crate::dispatch::Dispatcher;
use crate::registry::{register_agent, update_heartbeat};

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

/// Decode the wire state of one `InstanceStatus` into intake form. A frame
/// whose state does not parse (unset, or a newer peer's value) is unusable —
/// the master tracks state only from values it understands.
fn decode_status(status: &InstanceStatus) -> Result<(String, InstanceState, String, i64), String> {
    match from_wire_i32(status.state) {
        Ok(state) => Ok((
            status.instance_id.clone(),
            state,
            status.reason.clone(),
            status.ts_unix_ms,
        )),
        Err(e) => Err(format!(
            "instance {} carries unusable state: {e}",
            status.instance_id
        )),
    }
}

// ===== Session task =====

/// Whether the session loop should keep reading frames or terminate.
enum Flow {
    Continue,
    Stop,
}

/// Live state of one agent's bidirectional session: the shared dispatcher
/// (registry, sessions, experiments), the outbound reply channel, the hostname
/// learned from the first Register, and the session-map epoch owned by this
/// stream.
struct Session {
    dispatcher: Arc<Dispatcher>,
    heartbeat_interval_secs: u32,
    tx: mpsc::Sender<Result<ServerMessage, Status>>,
    hostname: Option<Hostname>,
    epoch: Option<u64>,
}

impl Session {
    async fn handle_frame(&mut self, payload: Option<agent_message::Payload>) -> Flow {
        match payload {
            Some(agent_message::Payload::Register(reg)) => self.register(reg.hostname).await,
            Some(agent_message::Payload::Heartbeat(_)) => self.heartbeat().await,
            Some(agent_message::Payload::FaultEvent(event)) => {
                // Telemetry only (design D5): operator-visible via logs, never
                // stored, never a state input.
                info!(instance_id = %event.instance_id, line = %event.ndjson_line, "fault event");
                Flow::Continue
            }
            Some(agent_message::Payload::InstanceStatus(status)) => {
                self.instance_status(&status).await;
                Flow::Continue
            }
            Some(agent_message::Payload::InstanceReport(report)) => {
                self.instance_report(report).await;
                Flow::Continue
            }
            Some(agent_message::Payload::TaintStatus(taint)) => {
                self.taint_status(&taint).await;
                Flow::Continue
            }
            // An unknown or future frame variant: prost decodes an unrecognized
            // oneof field to None. Log and keep the session open so a newer
            // agent never tears it down.
            None => {
                warn!("ignoring unknown or future agent frame (empty payload)");
                Flow::Continue
            }
        }
    }

    async fn register(&mut self, hostname_str: String) -> Flow {
        let now = self.dispatcher.clock().now();
        match process_register(
            &hostname_str,
            self.hostname.is_some(),
            now,
            self.heartbeat_interval_secs,
        ) {
            Ok((h, reply)) => {
                register_agent(self.dispatcher.registry(), &h, now);
                // Insert before the ack goes out: an agent that acts on the ack
                // immediately must already be reachable for dispatch.
                self.epoch = Some(self.dispatcher.sessions().insert(&h, self.tx.clone()));
                info!(hostname = %h, "agent registered");
                self.hostname = Some(h);
                self.send(reply).await
            }
            Err(status) => self.reject(status, "register").await,
        }
    }

    async fn heartbeat(&mut self) -> Flow {
        let now = self.dispatcher.clock().now();
        match process_heartbeat(self.hostname.as_ref(), now) {
            Ok(reply) => {
                if let Some(h) = self.hostname.as_ref() {
                    update_heartbeat(self.dispatcher.registry(), h, now);
                    info!(hostname = %h, "heartbeat");
                }
                self.send(reply).await
            }
            Err(status) => self.reject(status, "heartbeat").await,
        }
    }

    async fn instance_status(&self, status: &InstanceStatus) {
        match decode_status(status) {
            Ok((instance_id, state, reason, ts)) => {
                self.dispatcher
                    .handle_instance_status(&instance_id, state, &reason, ts)
                    .await;
            }
            Err(e) => warn!("dropping InstanceStatus: {e}"),
        }
    }

    async fn instance_report(&self, report: InstanceReport) {
        let mut statuses = vec![];
        for status in &report.statuses {
            match decode_status(status) {
                Ok(decoded) => statuses.push(decoded),
                Err(e) => warn!("dropping InstanceReport entry: {e}"),
            }
        }
        self.dispatcher.handle_instance_report(statuses).await;
    }

    async fn taint_status(&self, taint: &TaintStatus) {
        let Some(hostname) = self.hostname.as_ref() else {
            warn!("TaintStatus before Register; dropping");
            return;
        };
        self.dispatcher
            .handle_taint_status(hostname, taint.tainted, &taint.reason, taint.ts_unix_ms)
            .await;
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

/// Drive one agent session to completion: read frames off `stream`, dispatch
/// each, and write replies to `tx` until the stream closes or a protocol error
/// ends it. On exit the stream removes its own session-map entry — but only
/// while it is still the current one (supersede on reconnect, design D4).
///
/// Spawned by the gRPC `session` handler; owns its dispatcher handle so the
/// future is `'static` and `Send`.
pub(crate) async fn run(
    mut stream: Streaming<AgentMessage>,
    tx: mpsc::Sender<Result<ServerMessage, Status>>,
    dispatcher: Arc<Dispatcher>,
    heartbeat_interval_secs: u32,
) {
    let mut session = Session {
        dispatcher,
        heartbeat_interval_secs,
        tx,
        hostname: None,
        epoch: None,
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
    if let (Some(hostname), Some(epoch)) = (session.hostname.as_ref(), session.epoch) {
        session
            .dispatcher
            .sessions()
            .remove_if_current(hostname, epoch);
    }
    info!("session closed for hostname: {:?}", session.hostname);
}

// ===== Unit tests =====

#[cfg(test)]
mod tests {
    use super::*;
    use faultforge_fault::proto::to_wire_i32;
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

    #[test]
    fn decode_status_accepts_known_states_and_rejects_unset() {
        let good = InstanceStatus {
            instance_id: "exp-1:h:0".into(),
            state: to_wire_i32(InstanceState::Active),
            ts_unix_ms: 5,
            reason: "r".into(),
            plugin_digest: String::new(),
        };
        assert_eq!(
            decode_status(&good).unwrap(),
            ("exp-1:h:0".into(), InstanceState::Active, "r".into(), 5)
        );

        let unset = InstanceStatus {
            state: 0,
            ..good.clone()
        };
        assert!(decode_status(&unset).is_err());
        let unknown = InstanceStatus { state: 999, ..good };
        assert!(decode_status(&unknown).is_err());
    }
}
