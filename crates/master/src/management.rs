//! Read-only management API exposing the agent registry over HTTP.
//!
//! This is the operator/tooling plane, deliberately separate from the gRPC
//! agent plane: it never talks to agents and serves no core fault-injection
//! function — it only reads registry state for humans and the future CLI.
//!
//! The pure core is [`agent_view`], which maps a registry entry to its JSON
//! representation. The handlers are the imperative shell: they lock the shared
//! registry, map entries, and serialize. No authentication or TLS is applied
//! during the WIP phase (see the change proposal).

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};
use serde::Serialize;

use faultforge_proto::{Hostname, unix_ms};

use crate::registry::{AgentInfo, Registry};

/// JSON view of a single registry entry returned by the management API.
///
/// `last_seen_unix_ms` is the raw last-seen timestamp in Unix milliseconds; no
/// derived online/stale status is computed in this slice.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct AgentView {
    pub hostname: String,
    pub name: String,
    pub last_seen_unix_ms: i64,
}

/// Map a registry entry to its JSON view. Pure and time-free: the timestamp is
/// taken from the stored `last_seen`, not from the wall clock.
#[must_use]
pub fn agent_view(info: &AgentInfo) -> AgentView {
    AgentView {
        hostname: info.hostname.to_string(),
        name: info.name.clone(),
        last_seen_unix_ms: unix_ms(info.last_seen),
    }
}

/// Shared state for the management handlers: a handle to the agent registry
/// shared with the gRPC plane.
#[derive(Clone)]
pub struct ManagementState {
    pub registry: Registry,
}

/// `GET /agents` — list every registered agent as a JSON array (empty when none).
#[allow(clippy::needless_pass_by_value)] // axum requires extractors taken by value
async fn list_agents(State(state): State<ManagementState>) -> Json<Vec<AgentView>> {
    #[allow(clippy::expect_used)]
    // mutex poison means a previous thread panicked; propagating is correct
    let views = state
        .registry
        .lock()
        .expect("registry lock poisoned")
        .values()
        .map(agent_view)
        .collect();
    Json(views)
}

/// `GET /agents/{hostname}` — fetch one agent, or `404` when no entry exists.
///
/// The path segment is parsed into a [`Hostname`] before lookup, so a malformed
/// (e.g. blank) hostname yields `404` rather than ever matching a stored key.
#[allow(clippy::needless_pass_by_value)] // axum requires extractors taken by value
async fn get_agent(
    State(state): State<ManagementState>,
    Path(hostname): Path<String>,
) -> Result<Json<AgentView>, StatusCode> {
    let hostname = Hostname::parse(&hostname).map_err(|_| StatusCode::NOT_FOUND)?;
    #[allow(clippy::expect_used)]
    // mutex poison means a previous thread panicked; propagating is correct
    let view = state
        .registry
        .lock()
        .expect("registry lock poisoned")
        .get(&hostname)
        .map(agent_view);
    view.map(Json).ok_or(StatusCode::NOT_FOUND)
}

/// Build the management API router wired to the shared registry state.
pub fn router(state: ManagementState) -> Router {
    Router::new()
        .route("/agents", get(list_agents))
        .route("/agents/{hostname}", get(get_agent))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::new_registry;
    use faultforge_proto::Hostname;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn agent_view_maps_fields_and_timestamp() {
        let info = AgentInfo {
            hostname: Hostname::parse("web-01").unwrap(),
            name: "web-01".to_string(),
            last_seen: UNIX_EPOCH + Duration::from_secs(1),
        };
        let view = agent_view(&info);
        assert_eq!(view.hostname, "web-01");
        assert_eq!(view.name, "web-01");
        assert_eq!(view.last_seen_unix_ms, 1_000);
    }

    #[test]
    fn management_state_clone_shares_registry() {
        let registry = new_registry();
        let state = ManagementState {
            registry: std::sync::Arc::clone(&registry),
        };
        let clone = state.clone();
        assert!(std::sync::Arc::ptr_eq(&state.registry, &clone.registry));
    }
}
