use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use clap::Parser;
use config::{Config, Environment, File};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};
use tracing::{error, info, warn};

use faultforge_proto::unix_ms;
use faultforge_proto::v1::{
    AgentMessage, HeartbeatAck, RegisterAck, ServerMessage, agent_message,
    agent_service_server::{AgentService, AgentServiceServer},
    server_message,
};

// ===== Config =====

#[derive(Debug, Deserialize, Clone)]
pub struct MasterConfig {
    pub listen_addr: String,
    pub heartbeat_interval_secs: u32,
}

#[derive(Parser)]
#[command(name = "faultforge-master")]
pub struct Cli {
    #[arg(long, env = "FAULTFORGE_CONFIG")]
    pub config: Option<String>,
    #[arg(long)]
    pub listen_addr: Option<String>,
    #[arg(long)]
    pub heartbeat_interval_secs: Option<u32>,
}

pub fn load_config(cli: &Cli) -> Result<MasterConfig, config::ConfigError> {
    let mut builder = Config::builder()
        .set_default("listen_addr", "127.0.0.1:50051")?
        .set_default("heartbeat_interval_secs", 5_i64)?;

    builder = builder.add_source(
        File::with_name(cli.config.as_deref().unwrap_or("faultforge-master"))
            .required(cli.config.is_some()),
    );

    // Env vars: FAULTFORGE_LISTEN_ADDR, FAULTFORGE_HEARTBEAT_INTERVAL_SECS
    // No .separator() so field names with underscores are matched literally.
    builder = builder.add_source(Environment::with_prefix("FAULTFORGE").try_parsing(true));

    if let Some(addr) = &cli.listen_addr {
        builder = builder.set_override("listen_addr", addr.as_str())?;
    }
    if let Some(secs) = cli.heartbeat_interval_secs {
        builder = builder.set_override("heartbeat_interval_secs", secs as i64)?;
    }

    let cfg: MasterConfig = builder.build()?.try_deserialize()?;
    if cfg.heartbeat_interval_secs == 0 {
        return Err(config::ConfigError::Message(
            "heartbeat_interval_secs must be greater than 0".into(),
        ));
    }
    Ok(cfg)
}

// ===== Registry =====

#[derive(Debug, Clone)]
pub struct AgentConnectionInfo {
    pub hostname: String,
    pub name: String,
    pub last_seen: SystemTime,
}

pub type Registry = Arc<Mutex<HashMap<String, AgentConnectionInfo>>>;

pub fn new_registry() -> Registry {
    Arc::new(Mutex::new(HashMap::new()))
}

fn register_agent(registry: &Registry, hostname: &str) {
    let hostname = hostname.to_string();
    let info = AgentConnectionInfo {
        name: hostname.clone(),
        hostname: hostname.clone(),
        last_seen: SystemTime::now(),
    };
    registry.lock().unwrap().insert(hostname, info);
}

fn update_heartbeat(registry: &Registry, hostname: &str) {
    if let Some(entry) = registry.lock().unwrap().get_mut(hostname) {
        entry.last_seen = SystemTime::now();
    }
}

// ===== Service =====

pub struct MasterService {
    registry: Registry,
    heartbeat_interval_secs: u32,
}

impl MasterService {
    pub fn new(registry: Registry, heartbeat_interval_secs: u32) -> Self {
        Self {
            registry,
            heartbeat_interval_secs,
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
        let mut stream = request.into_inner();
        let (tx, rx) = mpsc::channel(32);

        tokio::spawn(async move {
            let mut hostname: Option<String> = None;

            loop {
                match stream.message().await {
                    Ok(Some(msg)) => match msg.payload {
                        Some(agent_message::Payload::Register(reg)) => {
                            // Guard: only one Register is allowed per stream.
                            if hostname.is_some() {
                                error!("duplicate Register on active session; closing stream");
                                let _ = tx
                                    .send(Err(Status::failed_precondition(
                                        "duplicate register on active session",
                                    )))
                                    .await;
                                break;
                            }

                            let h = reg.hostname.trim().to_string();
                            if h.is_empty() {
                                error!("Register with empty hostname; closing stream");
                                let _ = tx
                                    .send(Err(Status::invalid_argument(
                                        "hostname must not be empty",
                                    )))
                                    .await;
                                break;
                            }

                            hostname = Some(h.clone());
                            register_agent(&registry, &h);
                            info!(hostname = %h, "agent registered");
                            let reply = ServerMessage {
                                payload: Some(server_message::Payload::RegisterAck(RegisterAck {
                                    server_time_unix_ms: unix_ms(SystemTime::now()),
                                    heartbeat_interval_secs,
                                })),
                            };
                            if tx.send(Ok(reply)).await.is_err() {
                                break;
                            }
                        }
                        Some(agent_message::Payload::Heartbeat(_)) => match &hostname {
                            Some(h) => {
                                update_heartbeat(&registry, h);
                                info!(hostname = %h, "heartbeat");
                                let reply = ServerMessage {
                                    payload: Some(server_message::Payload::HeartbeatAck(
                                        HeartbeatAck {
                                            server_time_unix_ms: unix_ms(SystemTime::now()),
                                        },
                                    )),
                                };
                                if tx.send(Ok(reply)).await.is_err() {
                                    break;
                                }
                            }
                            None => {
                                error!("heartbeat received before register; closing stream");
                                let _ = tx
                                    .send(Err(Status::failed_precondition(
                                        "heartbeat received before register",
                                    )))
                                    .await;
                                break;
                            }
                        },
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

pub async fn run_server(cfg: MasterConfig) -> Result<(), Box<dyn std::error::Error>> {
    let mut addrs = tokio::net::lookup_host(&cfg.listen_addr).await?.peekable();
    let addr = addrs
        .next()
        .ok_or_else(|| format!("could not resolve listen_addr: {}", cfg.listen_addr))?;
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

// ===== Unit tests =====

#[cfg(test)]
mod tests {
    use super::*;

    // Task 2.3: MasterConfig defaults
    #[test]
    fn master_config_loads_defaults() {
        let cfg: MasterConfig = Config::builder()
            .set_default("listen_addr", "127.0.0.1:50051")
            .unwrap()
            .set_default("heartbeat_interval_secs", 5_i64)
            .unwrap()
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();
        assert_eq!(cfg.listen_addr, "127.0.0.1:50051");
        assert_eq!(cfg.heartbeat_interval_secs, 5);
    }

    // Task 2.3: overrides take precedence
    #[test]
    fn master_config_override_takes_precedence() {
        let cfg: MasterConfig = Config::builder()
            .set_default("listen_addr", "127.0.0.1:50051")
            .unwrap()
            .set_default("heartbeat_interval_secs", 5_i64)
            .unwrap()
            .set_override("listen_addr", "0.0.0.0:9090")
            .unwrap()
            .set_override("heartbeat_interval_secs", 30_i64)
            .unwrap()
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();
        assert_eq!(cfg.listen_addr, "0.0.0.0:9090");
        assert_eq!(cfg.heartbeat_interval_secs, 30);
    }

    // heartbeat_interval_secs = 0 must be rejected by load_config
    #[test]
    fn zero_heartbeat_interval_is_rejected() {
        let cli = Cli {
            config: None,
            listen_addr: None,
            heartbeat_interval_secs: Some(0),
        };
        let err = load_config(&cli).unwrap_err();
        assert!(
            err.to_string().contains("heartbeat_interval_secs"),
            "expected message mentioning heartbeat_interval_secs, got: {err}"
        );
    }

    // Task 3.2: first Register inserts entry with name seeded from hostname
    #[test]
    fn first_register_seeds_name_from_hostname() {
        let registry = new_registry();
        register_agent(&registry, "web-01");
        let reg = registry.lock().unwrap();
        let entry = reg.get("web-01").unwrap();
        assert_eq!(entry.hostname, "web-01");
        assert_eq!(entry.name, "web-01");
    }

    // Task 3.3: second Register for same hostname replaces (supersedes) the entry
    #[test]
    fn second_register_supersedes_first() {
        let registry = new_registry();
        register_agent(&registry, "web-01");
        let first_seen = registry.lock().unwrap().get("web-01").unwrap().last_seen;

        std::thread::sleep(std::time::Duration::from_millis(2));
        register_agent(&registry, "web-01");

        let reg = registry.lock().unwrap();
        assert_eq!(reg.len(), 1, "registry must have exactly one entry");
        let entry = reg.get("web-01").unwrap();
        assert!(entry.last_seen >= first_seen, "last_seen must be refreshed");
    }
}
