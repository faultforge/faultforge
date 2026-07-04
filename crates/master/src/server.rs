use std::net::SocketAddr;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};
use tracing::{info, warn};

use faultforge_proto::v1::{
    AgentMessage, ServerMessage,
    agent_service_server::{AgentService, AgentServiceServer},
};

use crate::clock::{Clock, SystemClock};
use crate::config::MasterConfig;
use crate::registry::{Registry, new_registry};

// ===== Typed error for run_server =====

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("could not resolve listen address '{0}': {1}")]
    ResolveAddr(String, std::io::Error),
    #[error("listen address resolved to no addresses: {0}")]
    NoAddresses(String),
    #[error("gRPC server failed: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("management API server failed: {0}")]
    Management(std::io::Error),
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
        let (tx, rx) = mpsc::channel(32);
        tokio::spawn(crate::session::run(
            request.into_inner(),
            tx,
            Arc::clone(&self.registry),
            self.heartbeat_interval_secs,
            Arc::clone(&self.clock),
        ));
        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

// ===== Server entry point =====

/// Resolve a listen address string to a single `SocketAddr`.
///
/// Warns and keeps the first when resolution yields multiple candidates.
///
/// # Errors
///
/// Returns `Err` if the address cannot be resolved or resolves to nothing.
async fn resolve_addr(addr: &str) -> Result<SocketAddr, ServerError> {
    let mut addrs = tokio::net::lookup_host(addr)
        .await
        .map_err(|e| ServerError::ResolveAddr(addr.to_string(), e))?
        .peekable();
    let resolved = addrs
        .next()
        .ok_or_else(|| ServerError::NoAddresses(addr.to_string()))?;
    // Warn if DNS returned multiple candidates — only the first is used.
    let remaining: Vec<_> = addrs.collect();
    if !remaining.is_empty() {
        warn!(
            chosen = %resolved,
            skipped = ?remaining,
            "listen address resolved to multiple addresses; using the first"
        );
    }
    Ok(resolved)
}

/// Start the gRPC agent plane and the HTTP management API, serving until the
/// process exits. Both servers share a single agent registry and run
/// concurrently; a failure of either stops the master.
///
/// # Errors
///
/// Returns `Err` if either listen address cannot be resolved, resolves to no
/// addresses, the gRPC transport layer fails, or the management API server
/// fails to bind or serve.
pub async fn run_server(cfg: MasterConfig) -> Result<(), ServerError> {
    let grpc_addr = resolve_addr(&cfg.listen_addr).await?;
    let management_addr = resolve_addr(&cfg.management_listen_addr).await?;

    // Single registry instance shared by both servers (one source of truth).
    let registry = new_registry();
    let service = MasterService::new(Arc::clone(&registry), cfg.heartbeat_interval_secs);

    info!(%grpc_addr, "faultforge-master gRPC agent plane listening");
    info!(%management_addr, "faultforge-master management API listening");

    let grpc = async {
        tonic::transport::Server::builder()
            .add_service(AgentServiceServer::new(service))
            .serve(grpc_addr)
            .await
            .map_err(ServerError::from)
    };

    let management = async {
        let listener = tokio::net::TcpListener::bind(management_addr)
            .await
            .map_err(ServerError::Management)?;
        axum::serve(
            listener,
            crate::management::router(crate::management::ManagementState { registry }),
        )
        .await
        .map_err(ServerError::Management)
    };

    // try_join! returns on the first error, so a failure of either server
    // stops the master.
    tokio::try_join!(grpc, management)?;

    Ok(())
}

// ===== Unit tests =====

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Clock;
    use crate::registry::new_registry;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    struct FixedClock(SystemTime);
    impl Clock for FixedClock {
        fn now(&self) -> SystemTime {
            self.0
        }
    }

    #[test]
    fn with_clock_accepts_custom_clock() {
        let t = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let registry = new_registry();
        let _service = MasterService::with_clock(registry, 5, std::sync::Arc::new(FixedClock(t)));
    }
}
