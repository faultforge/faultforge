//! The session layer (imperative shell, design D8): a full-duplex `Session`
//! stream inside a reconnect-with-backoff loop. One writer drains the shared
//! outbound channel; the reader dispatches acks, fault frames, and unknown
//! frames (log-and-continue). Every successful registration is followed by the
//! reconciliation sequence: `InstanceReport`, `TaintStatus`, then any queued
//! replay outcomes.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::{mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;
use tonic::Request;
use tracing::{debug, error, info, warn};

use faultforge_proto::v1::{
    AgentMessage, Heartbeat, Register, ServerMessage, agent_message,
    agent_service_client::AgentServiceClient, server_message,
};
use faultforge_proto::{Hostname, unix_ms};

use crate::config::AgentConfig;
use crate::journal::Journal;
use crate::supervisor::{ConnState, RuntimeCtx, Supervisor, replay_journal, taint_status_frame};
use crate::taint::Taint;

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

/// A short static label for a server frame, for logging frames the agent skips.
///
/// The match is exhaustive on purpose: adding a `ServerMessage` variant to the
/// proto breaks it at compile time, forcing a decision about the new frame.
fn server_frame_label(payload: Option<&server_message::Payload>) -> &'static str {
    match payload {
        Some(server_message::Payload::RegisterAck(_)) => "RegisterAck",
        Some(server_message::Payload::HeartbeatAck(_)) => "HeartbeatAck",
        Some(server_message::Payload::RunFault(_)) => "RunFault",
        Some(server_message::Payload::AbortFault(_)) => "AbortFault",
        Some(server_message::Payload::ClearTaint(_)) => "ClearTaint",
        None => "unknown/future (empty payload)",
    }
}

/// Exponential reconnect backoff: 1s doubling to a 30s cap, forever.
fn next_backoff(current: Duration) -> Duration {
    (current * 2).min(Duration::from_secs(30))
}

// ===== Agent entry point =====

/// Connect to the master and run forever: register, heartbeat, execute fault
/// frames, reconnect with backoff on any stream failure. Journaled instances
/// are recovered before the first connection attempt (design D5/D8).
///
/// # Errors
///
/// Returns `Err` only for unrecoverable startup problems (unreadable hostname,
/// invalid master address). Connection and stream failures are retried forever.
pub async fn run_agent(cfg: AgentConfig) -> Result<(), AgentError> {
    let raw_hostname = hostname::get()?;
    let hostname =
        Hostname::parse(&raw_hostname.to_string_lossy()).map_err(|_| AgentError::EmptyHostname)?;
    let master_addr = normalize_master_addr(&cfg.master_addr);

    let data_dir = PathBuf::from(&cfg.data_dir);
    let journal = Journal::new(&data_dir);
    let taint = Taint::new(&data_dir);
    let plugin_root = PathBuf::from(&cfg.plugin_root);
    let invocation_timeout = Duration::from_secs(cfg.invocation_timeout_secs);

    // Recover journaled instances before any session exists, so the first
    // InstanceReport is truthful and replay outcomes follow it (design D8).
    let mut startup_frames = {
        let (root, journal, taint) = (plugin_root.clone(), journal.clone(), taint.clone());
        tokio::task::spawn_blocking(move || {
            replay_journal(
                &root,
                &journal,
                &taint,
                invocation_timeout,
                SystemTime::now(),
            )
        })
        .await
        .unwrap_or_else(|e| {
            error!(error = %e, "journal replay task failed; continuing without replay");
            vec![]
        })
    };

    let (out_tx, mut out_rx) = mpsc::channel::<AgentMessage>(256);
    let (conn_tx, conn_rx) = watch::channel(ConnState::Lost {
        since_unix_ms: unix_ms(SystemTime::now()),
    });
    let ctx = Arc::new(RuntimeCtx {
        plugin_root,
        journal,
        taint: taint.clone(),
        out: out_tx,
        conn: conn_rx,
        loss_threshold: Duration::from_secs(cfg.master_loss_threshold_secs),
        invocation_timeout,
    });
    let mut supervisor = Supervisor::new(Arc::clone(&ctx));

    let mut backoff = Duration::from_secs(1);
    let mut carry: Option<AgentMessage> = None;

    info!(hostname = %hostname, master = %master_addr, "agent starting");
    loop {
        let result = run_session(
            &master_addr,
            &hostname,
            &mut supervisor,
            &mut out_rx,
            &conn_tx,
            &taint,
            &mut startup_frames,
            &mut carry,
            &mut backoff,
        )
        .await;
        let err = match result {
            Ok(()) => return Ok(()),
            Err(SessionEnd::Fatal(e)) => return Err(e),
            Err(SessionEnd::Retry(e)) => e,
        };
        // Preserve the original loss instant across failed attempts: the
        // self-abort threshold counts from when contact was lost, not from the
        // most recent retry.
        if matches!(*conn_tx.borrow(), ConnState::Connected) {
            let _ = conn_tx.send(ConnState::Lost {
                since_unix_ms: unix_ms(SystemTime::now()),
            });
        }
        warn!(error = %err, backoff_secs = backoff.as_secs(), "session ended; reconnecting");
        tokio::time::sleep(backoff).await;
        backoff = next_backoff(backoff);
    }
}

/// How a session attempt ended: retry (the normal case) or give up.
enum SessionEnd {
    /// Reconnect after backoff.
    Retry(AgentError),
    /// Unrecoverable (invalid master address URI).
    Fatal(AgentError),
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    master_addr: &str,
    hostname: &Hostname,
    supervisor: &mut Supervisor,
    out_rx: &mut mpsc::Receiver<AgentMessage>,
    conn_tx: &watch::Sender<ConnState>,
    taint: &Taint,
    startup_frames: &mut Vec<AgentMessage>,
    carry: &mut Option<AgentMessage>,
    backoff: &mut Duration,
) -> Result<(), SessionEnd> {
    let channel = tonic::transport::Channel::from_shared(master_addr.to_string())
        .map_err(|e| {
            SessionEnd::Fatal(AgentError::UnexpectedMessage(format!(
                "invalid master address URI: {e}"
            )))
        })?
        .connect()
        .await
        .map_err(|e| SessionEnd::Retry(e.into()))?;

    let mut client = AgentServiceClient::new(channel);
    let (grpc_tx, grpc_rx) = mpsc::channel::<AgentMessage>(64);
    let response = client
        .session(Request::new(ReceiverStream::new(grpc_rx)))
        .await
        .map_err(|e| SessionEnd::Retry(e.into()))?;
    let mut inbound = response.into_inner();

    send_frame(
        &grpc_tx,
        AgentMessage {
            payload: Some(agent_message::Payload::Register(Register {
                hostname: hostname.to_string(),
            })),
        },
        carry,
    )
    .await?;

    // Await RegisterAck, log-skipping anything else (mixed-version safety).
    let interval_secs = loop {
        let msg = recv_frame(&mut inbound).await?;
        if matches!(msg.payload, Some(server_message::Payload::RegisterAck(_))) {
            break interpret_register_ack(msg.payload).map_err(SessionEnd::Retry)?;
        }
        warn!(
            frame = server_frame_label(msg.payload.as_ref()),
            "ignoring frame while awaiting RegisterAck"
        );
    };
    info!(heartbeat_interval_secs = interval_secs, "registered");
    *backoff = Duration::from_secs(1);
    let _ = conn_tx.send(ConnState::Connected);

    // Reconciliation sequence (agent-fault-runtime spec): report, taint, then
    // any queued replay outcomes and a frame stranded by the previous session.
    let now_ms = unix_ms(SystemTime::now());
    send_frame(&grpc_tx, supervisor.report_frame(now_ms), carry).await?;
    send_frame(
        &grpc_tx,
        taint_status_frame(taint.current().as_ref(), now_ms),
        carry,
    )
    .await?;
    while !startup_frames.is_empty() {
        let frame = startup_frames.remove(0);
        send_frame_keeping(&grpc_tx, frame, startup_frames).await?;
    }
    if let Some(frame) = carry.take() {
        send_frame(&grpc_tx, frame, carry).await?;
    }

    let mut heartbeat = tokio::time::interval(Duration::from_secs(u64::from(interval_secs)));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    heartbeat.tick().await; // the immediate first tick; the cadence starts now

    loop {
        tokio::select! {
            frame = inbound.message() => {
                let msg = match frame {
                    Ok(Some(msg)) => msg,
                    Ok(None) => return Err(SessionEnd::Retry(AgentError::StreamClosed(
                        "server closed the stream".to_string(),
                    ))),
                    Err(status) => return Err(SessionEnd::Retry(status.into())),
                };
                dispatch(msg, supervisor);
            }
            _ = heartbeat.tick() => {
                send_frame(
                    &grpc_tx,
                    AgentMessage {
                        payload: Some(agent_message::Payload::Heartbeat(Heartbeat {})),
                    },
                    carry,
                )
                .await?;
            }
            maybe = out_rx.recv() => {
                // The runtime ctx holds a sender for the agent's lifetime, so
                // None means the process is tearing down.
                let msg = maybe.ok_or_else(|| SessionEnd::Retry(AgentError::ChannelClosed))?;
                send_frame(&grpc_tx, msg, carry).await?;
            }
        }
    }
}

/// Send on the session stream; on failure keep `frame` in `carry` so it is
/// re-sent after the reconnect (reconciliation covers anything still lost).
async fn send_frame(
    grpc_tx: &mpsc::Sender<AgentMessage>,
    frame: AgentMessage,
    carry: &mut Option<AgentMessage>,
) -> Result<(), SessionEnd> {
    if let Err(e) = grpc_tx.send(frame).await {
        // Heartbeats and Register are cheap to lose; everything else is worth
        // one retry after reconnecting.
        if !matches!(
            e.0.payload,
            Some(agent_message::Payload::Heartbeat(_) | agent_message::Payload::Register(_))
        ) {
            *carry = Some(e.0);
        }
        return Err(SessionEnd::Retry(AgentError::StreamClosed(
            "outbound stream is gone".to_string(),
        )));
    }
    Ok(())
}

/// Like [`send_frame`] but pushes the unsent frame back onto `pending`
/// (used while draining the startup queue).
async fn send_frame_keeping(
    grpc_tx: &mpsc::Sender<AgentMessage>,
    frame: AgentMessage,
    pending: &mut Vec<AgentMessage>,
) -> Result<(), SessionEnd> {
    if let Err(e) = grpc_tx.send(frame).await {
        pending.insert(0, e.0);
        return Err(SessionEnd::Retry(AgentError::StreamClosed(
            "outbound stream is gone".to_string(),
        )));
    }
    Ok(())
}

async fn recv_frame(
    inbound: &mut tonic::Streaming<ServerMessage>,
) -> Result<ServerMessage, SessionEnd> {
    match inbound.message().await {
        Ok(Some(msg)) => Ok(msg),
        Ok(None) => Err(SessionEnd::Retry(AgentError::StreamClosed(
            "server closed the stream".to_string(),
        ))),
        Err(status) => Err(SessionEnd::Retry(status.into())),
    }
}

fn dispatch(msg: ServerMessage, supervisor: &mut Supervisor) {
    match msg.payload {
        Some(server_message::Payload::RunFault(run)) => {
            info!(instance_id = %run.instance_id, plugin = %run.plugin_name, "RunFault received");
            supervisor.handle_run_fault(run);
        }
        Some(server_message::Payload::AbortFault(abort)) => {
            info!(instance_id = %abort.instance_id, "AbortFault received");
            supervisor.handle_abort_fault(&abort.instance_id);
        }
        Some(server_message::Payload::ClearTaint(_)) => {
            info!("ClearTaint received");
            supervisor.handle_clear_taint();
        }
        Some(server_message::Payload::HeartbeatAck(ack)) => {
            debug!(server_time = ack.server_time_unix_ms, "heartbeat ack");
        }
        other => {
            // Log-and-continue for anything unexpected, including unknown
            // future variants (empty payload) — mixed-version safety (D3 of
            // implement-fault-schema).
            warn!(
                frame = server_frame_label(other.as_ref()),
                "ignoring unhandled server frame; keeping session open"
            );
        }
    }
}

// ===== Unit tests =====

#[cfg(test)]
mod tests {
    use super::*;
    use faultforge_proto::v1::{HeartbeatAck, RegisterAck};

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
    fn unexpected_payload_is_error() {
        assert!(matches!(
            interpret_register_ack(None),
            Err(AgentError::UnexpectedMessage(_))
        ));
    }

    #[test]
    fn heartbeat_ack_is_labelled() {
        let payload = Some(server_message::Payload::HeartbeatAck(HeartbeatAck {
            server_time_unix_ms: 42,
        }));
        assert_eq!(server_frame_label(payload.as_ref()), "HeartbeatAck");
        assert_eq!(server_frame_label(None), "unknown/future (empty payload)");
    }

    #[test]
    fn backoff_doubles_and_caps_at_thirty_seconds() {
        let mut backoff = Duration::from_secs(1);
        let mut seen = vec![];
        for _ in 0..7 {
            seen.push(backoff.as_secs());
            backoff = next_backoff(backoff);
        }
        assert_eq!(seen, vec![1, 2, 4, 8, 16, 30, 30]);
    }
}
