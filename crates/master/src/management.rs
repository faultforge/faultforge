//! The HTTP management plane: registry reads plus the first write endpoints
//! (experiment run/halt, clear-taint — `master-fault-dispatch`). Writes are
//! limited to the fault-dispatch surface; registry entries themselves stay
//! unmodifiable over HTTP.
//!
//! The pure core is the view mapping ([`agent_view`], [`experiment_view`]);
//! handlers are the imperative shell over the shared registry and dispatcher.
//! No authentication or TLS during WIP (ADR-0002 known gap): the management
//! address must not be exposed beyond a trusted network.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::Serialize;

use faultforge_proto::{Hostname, unix_ms};

use crate::dispatch::{ClearTaintOutcome, Dispatcher, HaltOutcome};
use crate::experiment::{
    Cause, Experiment, ExperimentDefinition, ExperimentId, ExperimentOutcome, ExperimentPhase,
    InstanceRecord,
};
use crate::registry::{AgentInfo, Registry};

// ===== Agent views =====

/// JSON view of a single registry entry returned by the management API.
///
/// `last_seen_unix_ms` is the raw last-seen timestamp in Unix milliseconds; no
/// derived online/stale status is computed in this slice. `tainted` is the
/// quarantine state the agent last reported.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct AgentView {
    pub hostname: String,
    pub name: String,
    pub last_seen_unix_ms: i64,
    pub tainted: bool,
}

/// Map a registry entry to its JSON view. Pure and time-free: the timestamp is
/// taken from the stored `last_seen`, not from the wall clock.
#[must_use]
pub fn agent_view(info: &AgentInfo) -> AgentView {
    AgentView {
        hostname: info.hostname.to_string(),
        name: info.name.clone(),
        last_seen_unix_ms: unix_ms(info.last_seen),
        tainted: info.tainted,
    }
}

// ===== Experiment views =====

/// JSON view of what determined a halt or outcome (the legibility rule).
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct CauseView {
    pub hostname: Option<String>,
    pub instance_id: Option<String>,
    pub reason: String,
    pub ts_unix_ms: i64,
}

/// JSON view of one dispatched fault instance.
#[derive(Debug, Serialize)]
pub struct InstanceView {
    pub instance_id: String,
    pub hostname: String,
    pub action_index: usize,
    /// The lifecycle state as reported by the agent (`"PENDING"` until any
    /// report), serialized in the wire spelling (`"ACTIVE"`, …).
    pub state: faultforge_fault::state::InstanceState,
    pub reason: String,
    pub updated_unix_ms: i64,
}

/// JSON view of a full experiment record.
#[derive(Debug, Serialize)]
pub struct ExperimentView {
    pub id: String,
    pub name: String,
    /// `"RUNNING"`, `"HALTING"`, or the terminal outcome
    /// (`"COMPLETED"`/`"ABORTED"`/`"ERROR"`).
    pub state: String,
    /// Present exactly when the experiment is terminal.
    pub outcome: Option<String>,
    pub grace_secs: u32,
    pub started_unix_ms: i64,
    pub deadline_unix_ms: i64,
    pub cause: Option<CauseView>,
    pub instances: Vec<InstanceView>,
}

/// JSON view of one row in `GET /experiments`.
#[derive(Debug, Serialize)]
pub struct ExperimentSummary {
    pub id: String,
    pub name: String,
    pub state: String,
    pub outcome: Option<String>,
    pub started_unix_ms: i64,
    pub instances: usize,
}

fn outcome_label(outcome: ExperimentOutcome) -> &'static str {
    match outcome {
        ExperimentOutcome::Completed => "COMPLETED",
        ExperimentOutcome::Aborted => "ABORTED",
        ExperimentOutcome::Error => "ERROR",
    }
}

fn phase_labels(phase: &ExperimentPhase) -> (String, Option<String>) {
    match phase {
        ExperimentPhase::Running => ("RUNNING".to_string(), None),
        ExperimentPhase::Halting => ("HALTING".to_string(), None),
        ExperimentPhase::Concluded(outcome) => {
            let label = outcome_label(*outcome).to_string();
            (label.clone(), Some(label))
        }
    }
}

fn cause_view(cause: &Cause) -> CauseView {
    CauseView {
        hostname: cause.hostname.clone(),
        instance_id: cause.instance_id.clone(),
        reason: cause.reason.clone(),
        ts_unix_ms: cause.ts_unix_ms,
    }
}

fn instance_view(record: &InstanceRecord) -> InstanceView {
    InstanceView {
        instance_id: record.instance_id.clone(),
        hostname: record.hostname.to_string(),
        action_index: record.action_index,
        state: record.state,
        reason: record.reason.clone(),
        updated_unix_ms: record.updated_unix_ms,
    }
}

/// Map an experiment record to its full JSON view. Pure.
#[must_use]
pub fn experiment_view(experiment: &Experiment) -> ExperimentView {
    let (state, outcome) = phase_labels(&experiment.phase);
    ExperimentView {
        id: experiment.id.to_string(),
        name: experiment.definition.name.clone(),
        state,
        outcome,
        grace_secs: experiment.grace_secs,
        started_unix_ms: experiment.started_unix_ms,
        deadline_unix_ms: experiment.deadline_unix_ms,
        cause: experiment.cause.as_ref().map(cause_view),
        instances: experiment.instances.iter().map(instance_view).collect(),
    }
}

/// Map an experiment record to its list-row view. Pure.
#[must_use]
pub fn experiment_summary(experiment: &Experiment) -> ExperimentSummary {
    let (state, outcome) = phase_labels(&experiment.phase);
    ExperimentSummary {
        id: experiment.id.to_string(),
        name: experiment.definition.name.clone(),
        state,
        outcome,
        started_unix_ms: experiment.started_unix_ms,
        instances: experiment.instances.len(),
    }
}

/// The `422` body for a rejected experiment: every failing check, named.
#[derive(Debug, Serialize)]
pub struct ValidationErrors {
    pub errors: Vec<String>,
}

// ===== State and handlers =====

/// Shared state for the management handlers: the agent registry (reads) and
/// the dispatcher (experiment and taint operations), both shared with the
/// gRPC plane.
#[derive(Clone)]
pub struct ManagementState {
    pub registry: Registry,
    pub dispatcher: Arc<Dispatcher>,
}

/// `GET /agents` — list every registered agent as a JSON array (empty when none).
#[allow(clippy::needless_pass_by_value)] // axum requires extractors taken by value
async fn list_agents(State(state): State<ManagementState>) -> Json<Vec<AgentView>> {
    #[allow(clippy::expect_used)]
    // mutex poison means a previous thread panicked; propagating is correct
    let mut views: Vec<AgentView> = state
        .registry
        .lock()
        .expect("registry lock poisoned")
        .values()
        .map(agent_view)
        .collect();
    // Stable, hostname-sorted order: HashMap iteration is nondeterministic, so
    // without this the fleet view and CLI reshuffle between requests.
    views.sort_by(|a, b| a.hostname.cmp(&b.hostname));
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

/// `POST /experiments` — VALIDATE synchronously: `422` naming every failing
/// check, or `201` with the full record (dispatch already under way).
async fn run_experiment(
    State(state): State<ManagementState>,
    Json(definition): Json<ExperimentDefinition>,
) -> Result<(StatusCode, Json<ExperimentView>), (StatusCode, Json<ValidationErrors>)> {
    match state.dispatcher.start_experiment(definition).await {
        Ok(experiment) => Ok((StatusCode::CREATED, Json(experiment_view(&experiment)))),
        Err(failures) => Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ValidationErrors {
                errors: failures.iter().map(ToString::to_string).collect(),
            }),
        )),
    }
}

/// `GET /experiments` — summaries of every record this master's lifetime holds.
async fn list_experiments(State(state): State<ManagementState>) -> Json<Vec<ExperimentSummary>> {
    Json(
        state
            .dispatcher
            .list_experiments()
            .iter()
            .map(experiment_summary)
            .collect(),
    )
}

/// `GET /experiments/{id}` — the full record, or `404`.
async fn get_experiment(
    State(state): State<ManagementState>,
    Path(id): Path<String>,
) -> Result<Json<ExperimentView>, StatusCode> {
    state
        .dispatcher
        .get_experiment(&ExperimentId::from_raw(&id))
        .map(|experiment| Json(experiment_view(&experiment)))
        .ok_or(StatusCode::NOT_FOUND)
}

/// `POST /experiments/{id}/halt` — most-available: `202` even when some
/// agents are unreachable; `404` unknown id; `409` already terminal.
async fn halt_experiment(
    State(state): State<ManagementState>,
    Path(id): Path<String>,
) -> StatusCode {
    match state
        .dispatcher
        .halt(&ExperimentId::from_raw(&id), "management API")
        .await
    {
        HaltOutcome::NotFound => StatusCode::NOT_FOUND,
        HaltOutcome::AlreadyConcluded => StatusCode::CONFLICT,
        HaltOutcome::Accepted => StatusCode::ACCEPTED,
    }
}

/// `POST /agents/{hostname}/clear-taint` — relay the operator's command:
/// `404` unknown host, `409` no live session, `202` sent (the effect is
/// confirmed by the agent's answering `TaintStatus`).
async fn clear_taint(
    State(state): State<ManagementState>,
    Path(hostname): Path<String>,
) -> StatusCode {
    let Ok(hostname) = Hostname::parse(&hostname) else {
        return StatusCode::NOT_FOUND;
    };
    match state.dispatcher.clear_taint(&hostname).await {
        ClearTaintOutcome::UnknownHost => StatusCode::NOT_FOUND,
        ClearTaintOutcome::NotConnected => StatusCode::CONFLICT,
        ClearTaintOutcome::Sent => StatusCode::ACCEPTED,
    }
}

/// Build the management API router wired to the shared state.
pub fn router(state: ManagementState) -> Router {
    Router::new()
        .route("/agents", get(list_agents))
        .route("/agents/{hostname}", get(get_agent))
        .route("/agents/{hostname}/clear-taint", post(clear_taint))
        .route("/experiments", get(list_experiments).post(run_experiment))
        .route("/experiments/{id}", get(get_experiment))
        .route("/experiments/{id}/halt", post(halt_experiment))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::experiment::{ActionDefinition, PluginRef, mint_experiment};
    use crate::registry::new_registry;
    use faultforge_fault::state::InstanceState;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn agent_view_maps_fields_and_timestamp() {
        let info = AgentInfo {
            hostname: Hostname::parse("web-01").unwrap(),
            name: "web-01".to_string(),
            last_seen: UNIX_EPOCH + Duration::from_secs(1),
            tainted: true,
        };
        let view = agent_view(&info);
        assert_eq!(view.hostname, "web-01");
        assert_eq!(view.name, "web-01");
        assert_eq!(view.last_seen_unix_ms, 1_000);
        assert!(view.tainted);
    }

    #[test]
    fn experiment_view_exposes_states_outcome_and_cause() {
        let definition = ExperimentDefinition {
            name: "exp".into(),
            actions: vec![ActionDefinition {
                hosts: vec!["web-01".into()],
                plugin: PluginRef {
                    name: "fixture".into(),
                    version: "1".into(),
                },
                params: serde_json::Map::new(),
                duration_secs: 5,
            }],
        };
        let mut experiment = mint_experiment(
            ExperimentId::from_raw("exp-1"),
            definition,
            10,
            1_000,
            crate::experiment::DEADLINE_MARGIN_MS,
        );
        let view = experiment_view(&experiment);
        assert_eq!(view.state, "RUNNING");
        assert_eq!(view.outcome, None);
        assert_eq!(view.instances.len(), 1);
        assert_eq!(view.instances[0].state, InstanceState::Pending);

        experiment.apply_status("exp-1:web-01:0", InstanceState::Error, "boom", 2_000);
        experiment.phase = ExperimentPhase::Concluded(ExperimentOutcome::Error);
        experiment.cause = experiment.outcome_cause(ExperimentOutcome::Error);
        let view = experiment_view(&experiment);
        assert_eq!(view.state, "ERROR");
        assert_eq!(view.outcome.as_deref(), Some("ERROR"));
        let cause = view.cause.unwrap();
        assert_eq!(cause.hostname.as_deref(), Some("web-01"));
        assert!(cause.reason.contains("boom"));

        let summary = experiment_summary(&experiment);
        assert_eq!(summary.state, "ERROR");
        assert_eq!(summary.instances, 1);
    }

    #[test]
    fn instance_state_serializes_in_wire_spelling() {
        let json = serde_json::to_string(&InstanceState::Active).unwrap();
        assert_eq!(json, "\"ACTIVE\"");
    }

    #[test]
    fn management_state_clone_shares_registry() {
        let registry = new_registry();
        let dispatcher = Arc::new(Dispatcher::new(
            crate::catalog::Catalog::default(),
            Arc::clone(&registry),
            10,
            Arc::new(crate::clock::SystemClock),
        ));
        let state = ManagementState {
            registry: Arc::clone(&registry),
            dispatcher,
        };
        let clone = state.clone();
        assert!(Arc::ptr_eq(&state.registry, &clone.registry));
        assert!(Arc::ptr_eq(&state.dispatcher, &clone.dispatcher));
    }

    #[tokio::test]
    async fn list_agents_returns_hostname_sorted_order() {
        let registry = new_registry();
        let now = UNIX_EPOCH + Duration::from_secs(1);
        for host in ["web-03", "web-01", "web-02"] {
            crate::registry::register_agent(&registry, &Hostname::parse(host).unwrap(), now);
        }
        let dispatcher = Arc::new(Dispatcher::new(
            crate::catalog::Catalog::default(),
            Arc::clone(&registry),
            10,
            Arc::new(crate::clock::SystemClock),
        ));
        let state = ManagementState {
            registry,
            dispatcher,
        };
        let Json(views) = list_agents(State(state)).await;
        let hostnames: Vec<&str> = views.iter().map(|v| v.hostname.as_str()).collect();
        assert_eq!(hostnames, ["web-01", "web-02", "web-03"]);
    }
}
