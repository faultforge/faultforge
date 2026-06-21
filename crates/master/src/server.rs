use std::sync::Arc;
use std::time::SystemTime;

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};
use tracing::{error, info, warn};

use faultforge_proto::Hostname;
use faultforge_proto::unix_ms;
use faultforge_proto::v1::{
    AgentMessage, HeartbeatAck, RegisterAck, ServerMessage, agent_message,
    agent_service_server::{AgentService, AgentServiceServer},
    server_message,
};

use crate::clock::{Clock, SystemClock};
use crate::config::MasterConfig;
use crate::registry::{Registry, new_registry, register_agent, update_heartbeat};

// ===== Typed error for run_server =====

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("could not resolve listen_addr '{0}': {1}")]
    ResolveAddr(String, std::io::Error),
    #[error("listen_addr resolved to no addresses: {0}")]
    NoAddresses(String),
    #[error("gRPC server failed: {0}")]
    Transport(#[from] tonic::transport::Error),
}

// ===== Pure frame-processing functions =====

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
    let hostname = Hostname::new(hostname_str).map_err(Status::invalid_argument)?;
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

// ===== MasterService =====

pub struct MasterService {
    registry: Registry,
    heartbeat_interval_secs: u32,
    clock: Arc<dyn Clock>,
}

impl MasterService {
    pub fn new(registry: Registry, heartbeat_interval_secs: u32) -> Self {
        Self {
            registry,
            heartbeat_interval_secs,
            clock: Arc::new(SystemClock),
        }
    }

    /// Constructor for tests or custom deployments that need a specific clock.
    pub fn with_clock(
        registry: Registry,
        heartbeat_interval_secs: u32,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            registry,
            heartbeat_interval_secs,
            clock,
        }
    }
}

#[tonic::async_trait]
impl AgentService for MasterService {
    type SessionStream = ReceiverStream<Result<ServerMessage, Status>>;

    async fn session(
        &self,
        request: Request<Streaming<AgentMessage>>,
    ) -> Result<Response<Self::SessionStream>, Status> {
        let registry = Arc::clone(&self.registry);
        let heartbeat_interval_secs = self.heartbeat_interval_secs;
        let clock = Arc::clone(&self.clock);
        let mut stream = request.into_inner();
        let (tx, rx) = mpsc::channel(32);

        tokio::spawn(async move {
            let mut hostname: Option<Hostname> = None;

            loop {
                match stream.message().await {
                    Ok(Some(msg)) => match msg.payload {
                        Some(agent_message::Payload::Register(reg)) => {
                            let now = clock.now();
                            match process_register(
                                &reg.hostname,
                                hostname.is_some(),
                                now,
                                heartbeat_interval_secs,
                            ) {
                                Ok((h, reply)) => {
                                    register_agent(&registry, &h, now);
                                    info!(hostname = %h, "agent registered");
                                    hostname = Some(h);
                                    if tx.send(Ok(reply)).await.is_err() {
                                        break;
                                    }
                                }
                                Err(status) => {
                                    error!("register error: {status}");
                                    let _ = tx.send(Err(status)).await;
                                    break;
                                }
                            }
                        }
                        Some(agent_message::Payload::Heartbeat(_)) => {
                            let now = clock.now();
                            match process_heartbeat(hostname.as_ref(), now) {
                                Ok(reply) => {
                                    if let Some(h) = &hostname {
                                        update_heartbeat(&registry, h, now);
                                        info!(hostname = %h, "heartbeat");
                                    }
                                    if tx.send(Ok(reply)).await.is_err() {
                                        break;
                                    }
                                }
                                Err(status) => {
                                    error!("heartbeat error: {status}");
                                    let _ = tx.send(Err(status)).await;
                                    break;
                                }
                            }
                        }
                        None => break,
                    },
                    Ok(None) => break,
                    Err(e) => {
                        error!("stream error: {e}");
                        let _ = tx.send(Err(e)).await;
                        break;
                    }
                }
            }
            info!("session closed for hostname: {hostname:?}");
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

// ===== Server entry point =====

/// Start the gRPC server and serve until the process exits.
///
/// # Errors
///
/// Returns `Err` if the listen address cannot be resolved, resolves to no
/// addresses, or the gRPC transport layer fails.
pub async fn run_server(cfg: MasterConfig) -> Result<(), ServerError> {
    let mut addrs = tokio::net::lookup_host(&cfg.listen_addr)
        .await
        .map_err(|e| ServerError::ResolveAddr(cfg.listen_addr.clone(), e))?
        .peekable();
    let addr = addrs
        .next()
        .ok_or_else(|| ServerError::NoAddresses(cfg.listen_addr.clone()))?;
    // Warn if DNS returned multiple candidates — only the first is used.
    let remaining: Vec<_> = addrs.collect();
    if !remaining.is_empty() {
        warn!(
            chosen = %addr,
            skipped = ?remaining,
            "listen_addr resolved to multiple addresses; using the first"
        );
    }
    let registry = new_registry();
    let service = MasterService::new(registry, cfg.heartbeat_interval_secs);

    info!(%addr, "faultforge-master listening");

    tonic::transport::Server::builder()
        .add_service(AgentServiceServer::new(service))
        .serve(addr)
        .await?;

    Ok(())
}
