//! The dispatch orchestrator (imperative shell, design D5/D6): owns the
//! in-memory experiment store, accepts and fans out experiments, tracks
//! instance state from agent-authoritative frames, fires the kill-switch, and
//! resolves every record by its deadline. All decisions come from the pure
//! core in [`crate::experiment`]; this module only locks, sends, and sleeps.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::{info, warn};

use faultforge_fault::state::InstanceState;
use faultforge_proto::v1::{AbortFault, ClearTaint, RunFault, ServerMessage, server_message};
use faultforge_proto::{Hostname, unix_ms};

use crate::catalog::Catalog;
use crate::clock::Clock;
use crate::experiment::{
    Cause, DEADLINE_MARGIN_MS, Experiment, ExperimentDefinition, ExperimentId, ExperimentPhase,
    HostFacts, ValidationFailure, mint_experiment, validate,
};
use crate::lock::lock_poison_free;
use crate::registry::{Registry, set_taint};
use crate::sessions::SessionMap;

/// How `POST /experiments/{id}/halt` resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltOutcome {
    /// No experiment with that id (this master's lifetime).
    NotFound,
    /// The experiment is already terminal; nothing to halt.
    AlreadyConcluded,
    /// The kill-switch fired (or was already firing).
    Accepted,
}

/// How `POST /agents/{hostname}/clear-taint` resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearTaintOutcome {
    /// The hostname is not in the registry.
    UnknownHost,
    /// Registered but no live session — the agent must remove its own taint
    /// file, so there is nobody to deliver the command to.
    NotConnected,
    /// `ClearTaint` was sent; the registry updates when `TaintStatus` returns.
    Sent,
}

/// Master-side dispatch state: catalog, registry, live sessions, and the
/// in-memory experiment store (ADR-0002 §17 — a restart forgets it all).
pub struct Dispatcher {
    catalog: Catalog,
    registry: Registry,
    sessions: SessionMap,
    store: Mutex<HashMap<ExperimentId, Experiment>>,
    seq: AtomicU64,
    default_grace_secs: u32,
    deadline_margin_ms: i64,
    clock: Arc<dyn Clock>,
}

impl Dispatcher {
    /// A dispatcher over `catalog` and `registry` with an empty store.
    #[must_use]
    pub fn new(
        catalog: Catalog,
        registry: Registry,
        default_grace_secs: u32,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            catalog,
            registry,
            sessions: SessionMap::default(),
            store: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(1),
            default_grace_secs,
            deadline_margin_ms: DEADLINE_MARGIN_MS,
            clock,
        }
    }

    /// Override the deadline margin (tests compress it; deployments normally
    /// keep [`DEADLINE_MARGIN_MS`]).
    #[must_use]
    pub fn with_deadline_margin_ms(mut self, margin_ms: i64) -> Self {
        self.deadline_margin_ms = margin_ms;
        self
    }

    /// The live-session map (the gRPC plane inserts/removes entries).
    #[must_use]
    pub fn sessions(&self) -> &SessionMap {
        &self.sessions
    }

    /// The shared agent registry.
    #[must_use]
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// The clock timestamps and deadlines are read from.
    #[must_use]
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    fn lock_store(&self) -> std::sync::MutexGuard<'_, HashMap<ExperimentId, Experiment>> {
        lock_poison_free(&self.store)
    }

    // ===== Accept path (design D5, single salvo) =====

    /// Validate `definition` and, if clean, mint the record and dispatch one
    /// `RunFault` per (host, action) as a single salvo.
    ///
    /// # Errors
    ///
    /// Returns every failed VALIDATE check; nothing was dispatched.
    pub async fn start_experiment(
        self: &Arc<Self>,
        definition: ExperimentDefinition,
    ) -> Result<Experiment, Vec<ValidationFailure>> {
        let facts = self.gather_host_facts(&definition);
        let failures = validate(&definition, &self.catalog, &facts);
        if !failures.is_empty() {
            return Err(failures);
        }

        let now_ms = unix_ms(self.clock.now());
        let id = ExperimentId::mint(now_ms, self.seq.fetch_add(1, Ordering::Relaxed));
        let experiment = mint_experiment(
            id.clone(),
            definition,
            self.default_grace_secs,
            now_ms,
            self.deadline_margin_ms,
        );
        let snapshot = experiment.clone();
        self.lock_store().insert(id.clone(), experiment);

        info!(experiment = %id, instances = snapshot.instances.len(), "experiment dispatching");
        self.send_run_faults(&snapshot).await;
        self.spawn_deadline_task(id, snapshot.deadline_unix_ms);
        Ok(snapshot)
    }

    fn gather_host_facts(&self, definition: &ExperimentDefinition) -> HashMap<String, HostFacts> {
        let registry = lock_poison_free(&self.registry);
        let mut facts = HashMap::new();
        for action in &definition.actions {
            for host in &action.hosts {
                if facts.contains_key(host) {
                    continue;
                }
                let Ok(hostname) = Hostname::parse(host) else {
                    continue; // stays unregistered in the facts -> HostUnknown
                };
                let entry = registry.get(&hostname);
                facts.insert(
                    host.clone(),
                    HostFacts {
                        registered: entry.is_some(),
                        connected: self.sessions.is_connected(&hostname),
                        tainted: entry.is_some_and(|info| info.tainted),
                    },
                );
            }
        }
        facts
    }

    /// The single salvo: one `RunFault` per minted instance. Never re-issued
    /// (ADR-0002 §12) — a send that fails here is reconciled by the agent's
    /// report or resolved by the deadline, not retried.
    async fn send_run_faults(&self, experiment: &Experiment) {
        for instance in &experiment.instances {
            let action = &experiment.definition.actions[instance.action_index];
            let Some(entry) = self
                .catalog
                .get(&action.plugin.name, &action.plugin.version)
            else {
                // Validation resolved the plugin moments ago; the catalog is
                // immutable, so this arm is unreachable in practice.
                warn!(instance = %instance.instance_id, "plugin vanished from catalog; skipping");
                continue;
            };
            let params_json = match serde_json::to_string(&action.params) {
                Ok(json) => json,
                Err(e) => {
                    warn!(instance = %instance.instance_id, error = %e,
                        "could not encode params; skipping dispatch");
                    continue;
                }
            };
            let frame = ServerMessage {
                payload: Some(server_message::Payload::RunFault(RunFault {
                    instance_id: instance.instance_id.clone(),
                    plugin_name: action.plugin.name.clone(),
                    plugin_version: action.plugin.version.clone(),
                    plugin_digest: entry.digest.as_str().to_string(),
                    params_json,
                    duration_secs: action.duration_secs,
                    grace_secs: experiment.grace_secs,
                })),
            };
            self.send_to_host(&instance.hostname, frame, "RunFault")
                .await;
        }
    }

    async fn send_to_host(&self, hostname: &Hostname, frame: ServerMessage, label: &str) {
        let Some(tx) = self.sessions.sender(hostname) else {
            // Normal, non-fatal: the agent's own safety net governs the host
            // and the experiment deadline bounds the record (design D5).
            warn!(host = %hostname, frame = label, "no live session; frame not delivered");
            return;
        };
        if tx.send(Ok(frame)).await.is_err() {
            warn!(host = %hostname, frame = label, "session closed while sending frame");
        }
    }

    fn spawn_deadline_task(self: &Arc<Self>, id: ExperimentId, deadline_unix_ms: i64) {
        let dispatcher = Arc::clone(self);
        tokio::spawn(async move {
            let now_ms = unix_ms(dispatcher.clock.now());
            let wait = u64::try_from(deadline_unix_ms - now_ms).unwrap_or(0);
            tokio::time::sleep(Duration::from_millis(wait)).await;
            dispatcher.resolve_deadline(&id).await;
        });
    }

    // ===== Frame intake (agent-authoritative, design D5) =====

    /// Apply one `InstanceStatus` frame. Frames for unknown instances (e.g.
    /// minted by a previous master life) are logged and dropped.
    pub async fn handle_instance_status(
        &self,
        instance_id: &str,
        state: InstanceState,
        reason: &str,
        ts_unix_ms: i64,
    ) {
        let Some(experiment_id) = owning_experiment_id(instance_id) else {
            warn!(instance = %instance_id, "InstanceStatus with unparseable id; dropping");
            return;
        };
        let aborts = {
            let mut store = self.lock_store();
            let Some(experiment) = store.get_mut(&experiment_id) else {
                warn!(instance = %instance_id,
                    "InstanceStatus for unknown experiment (restarted master?); dropping");
                return;
            };
            if !experiment.apply_status(instance_id, state, reason, ts_unix_ms) {
                return;
            }
            react_to_update(experiment)
        };
        self.send_aborts(aborts).await;
    }

    /// Accept an `InstanceReport` snapshot as the truth for the instances it
    /// lists (ADR-0002 §12 — reconciliation, never re-inject).
    pub async fn handle_instance_report(
        &self,
        statuses: Vec<(String, InstanceState, String, i64)>,
    ) {
        for (instance_id, state, reason, ts_unix_ms) in statuses {
            self.handle_instance_status(&instance_id, state, &reason, ts_unix_ms)
                .await;
        }
    }

    /// Apply a `TaintStatus` frame from `hostname`: update the registry flag
    /// and, on a new taint, trip the kill-switch of every live experiment
    /// targeting that host.
    pub async fn handle_taint_status(
        &self,
        hostname: &Hostname,
        tainted: bool,
        reason: &str,
        ts_unix_ms: i64,
    ) {
        set_taint(&self.registry, hostname, tainted);
        if !tainted {
            return;
        }
        let aborts = {
            let mut store = self.lock_store();
            let mut aborts = vec![];
            for experiment in store.values_mut() {
                if experiment.is_concluded() || !experiment.targets_host(hostname.as_str()) {
                    continue;
                }
                experiment.record_taint(hostname.as_str(), reason, ts_unix_ms);
                aborts.extend(react_to_update(experiment));
            }
            aborts
        };
        self.send_aborts(aborts).await;
    }

    async fn send_aborts(&self, aborts: Vec<(Hostname, String)>) {
        for (hostname, instance_id) in aborts {
            let frame = ServerMessage {
                payload: Some(server_message::Payload::AbortFault(AbortFault {
                    instance_id,
                })),
            };
            self.send_to_host(&hostname, frame, "AbortFault").await;
        }
    }

    // ===== Operator surface =====

    /// Fire the kill-switch for an operator halt. Most-available by design:
    /// unreachable agents are skipped (their self-abort governs the host) and
    /// the deadline still bounds the record.
    pub async fn halt(&self, id: &ExperimentId, reason: &str) -> HaltOutcome {
        let now_ms = unix_ms(self.clock.now());
        let (outcome, aborts) = {
            let mut store = self.lock_store();
            let Some(experiment) = store.get_mut(id) else {
                return HaltOutcome::NotFound;
            };
            if experiment.is_concluded() {
                return HaltOutcome::AlreadyConcluded;
            }
            let cause = Cause {
                hostname: None,
                instance_id: None,
                reason: format!("operator halt: {reason}"),
                ts_unix_ms: now_ms,
            };
            let aborts = fire_kill_switch(experiment, cause);
            conclude_if_done(experiment);
            (HaltOutcome::Accepted, aborts)
        };
        info!(experiment = %id, "operator halt accepted");
        self.send_aborts(aborts).await;
        outcome
    }

    /// Force-resolve the record at its deadline (design D5): every instance
    /// still non-terminal becomes `ERROR` (unresolved) and the experiment
    /// concludes. Reachable stragglers still get an `AbortFault`, defensively.
    async fn resolve_deadline(&self, id: &ExperimentId) {
        let now_ms = unix_ms(self.clock.now());
        let aborts = {
            let mut store = self.lock_store();
            let Some(experiment) = store.get_mut(id) else {
                return;
            };
            if experiment.is_concluded() {
                return;
            }
            let unresolved: Vec<(Hostname, String)> = experiment
                .instances
                .iter()
                .filter(|i| !i.is_terminal())
                .map(|i| (i.hostname.clone(), i.instance_id.clone()))
                .collect();
            warn!(experiment = %id, unresolved = unresolved.len(),
                "experiment deadline expired; marking unresolved instances ERROR");
            if experiment.cause.is_none() {
                experiment.cause = unresolved.first().map(|(hostname, instance_id)| Cause {
                    hostname: Some(hostname.to_string()),
                    instance_id: Some(instance_id.clone()),
                    reason: "unresolved at experiment deadline".to_string(),
                    ts_unix_ms: now_ms,
                });
            }
            for (_, instance_id) in &unresolved {
                experiment.apply_status(
                    instance_id,
                    InstanceState::Error,
                    "unresolved at experiment deadline",
                    now_ms,
                );
            }
            conclude_if_done(experiment);
            unresolved
        };
        self.send_aborts(aborts).await;
    }

    /// Relay an operator's clear-taint to the agent behind `hostname`'s live
    /// session (design D7). The registry flag is updated only by the agent's
    /// answering `TaintStatus`, never assumed here.
    ///
    /// # Panics
    ///
    /// Panics if the registry mutex is poisoned.
    pub async fn clear_taint(&self, hostname: &Hostname) -> ClearTaintOutcome {
        {
            let registry = lock_poison_free(&self.registry);
            if !registry.contains_key(hostname) {
                return ClearTaintOutcome::UnknownHost;
            }
        }
        let Some(tx) = self.sessions.sender(hostname) else {
            return ClearTaintOutcome::NotConnected;
        };
        let frame = ServerMessage {
            payload: Some(server_message::Payload::ClearTaint(ClearTaint {})),
        };
        if tx.send(Ok(frame)).await.is_err() {
            return ClearTaintOutcome::NotConnected;
        }
        info!(host = %hostname, "ClearTaint relayed to agent");
        ClearTaintOutcome::Sent
    }

    // ===== Read model =====

    /// Snapshot of one experiment, if this master's lifetime minted it.
    #[must_use]
    pub fn get_experiment(&self, id: &ExperimentId) -> Option<Experiment> {
        self.lock_store().get(id).cloned()
    }

    /// Snapshots of every experiment this master's lifetime minted.
    #[must_use]
    pub fn list_experiments(&self) -> Vec<Experiment> {
        let mut experiments: Vec<Experiment> = self.lock_store().values().cloned().collect();
        experiments.sort_by(|a, b| {
            (a.started_unix_ms, a.id.as_str().to_string())
                .cmp(&(b.started_unix_ms, b.id.as_str().to_string()))
        });
        experiments
    }
}

/// The experiment that minted `instance_id` (`<experiment_id>:<host>:<idx>`).
fn owning_experiment_id(instance_id: &str) -> Option<ExperimentId> {
    let (experiment, rest) = instance_id.split_once(':')?;
    if experiment.is_empty() || rest.is_empty() {
        return None;
    }
    Some(ExperimentId::from_raw(experiment))
}

/// After any record change: fire the kill-switch if the core says so, then
/// conclude if every instance is terminal. Returns the abort targets to send
/// once the store lock is released.
fn react_to_update(experiment: &mut Experiment) -> Vec<(Hostname, String)> {
    let aborts = match experiment.kill_trigger() {
        Some(cause) => fire_kill_switch(experiment, cause),
        None => vec![],
    };
    conclude_if_done(experiment);
    aborts
}

/// Fire the kill-switch (pure over the record): record the first cause, enter
/// `HALTING`, and mark+collect every non-terminal instance not already asked
/// to abort. The caller sends the frames after releasing the store lock.
fn fire_kill_switch(experiment: &mut Experiment, cause: Cause) -> Vec<(Hostname, String)> {
    if experiment.is_concluded() {
        return vec![];
    }
    if experiment.cause.is_none() {
        info!(experiment = %experiment.id, reason = %cause.reason, "kill-switch fired");
        experiment.cause = Some(cause);
    }
    experiment.phase = ExperimentPhase::Halting;
    experiment
        .instances
        .iter_mut()
        .filter(|i| !i.is_terminal() && !i.abort_requested)
        .map(|i| {
            i.abort_requested = true;
            (i.hostname.clone(), i.instance_id.clone())
        })
        .collect()
}

/// Conclude the record once every instance is terminal, filling the cause for
/// the legibility rule when none was recorded on the way.
fn conclude_if_done(experiment: &mut Experiment) {
    if experiment.is_concluded() {
        return;
    }
    let Some(outcome) = experiment.outcome() else {
        return;
    };
    if experiment.cause.is_none() {
        experiment.cause = experiment.outcome_cause(outcome);
    }
    info!(experiment = %experiment.id, outcome = ?outcome, "experiment concluded");
    experiment.phase = ExperimentPhase::Concluded(outcome);
}

// ===== Unit tests =====

#[cfg(test)]
mod tests {
    use super::*;
    use crate::experiment::{ActionDefinition, ExperimentOutcome, PluginRef};

    fn minted_two_hosts() -> Experiment {
        let definition = ExperimentDefinition {
            name: "t".into(),
            actions: vec![ActionDefinition {
                hosts: vec!["web-01".into(), "db-01".into()],
                plugin: PluginRef {
                    name: "fixture".into(),
                    version: "1".into(),
                },
                params: serde_json::Map::new(),
                duration_secs: 5,
            }],
        };
        mint_experiment(
            ExperimentId::mint(1_000, 1),
            definition,
            10,
            1_000,
            DEADLINE_MARGIN_MS,
        )
    }

    #[test]
    fn owning_experiment_id_parses_the_prefix() {
        assert_eq!(
            owning_experiment_id("exp-1000-1:web-01:0"),
            Some(ExperimentId::from_raw("exp-1000-1"))
        );
        assert_eq!(owning_experiment_id("no-separator"), None);
        assert_eq!(owning_experiment_id(":host:0"), None);
    }

    #[test]
    fn kill_switch_marks_and_collects_only_live_unrequested_instances() {
        let mut exp = minted_two_hosts();
        exp.apply_status("exp-1000-1:web-01:0", InstanceState::Error, "boom", 2_000);
        let cause = exp.kill_trigger().unwrap();
        let aborts = fire_kill_switch(&mut exp, cause);
        // The ERROR instance is terminal; only db-01's instance gets an abort.
        assert_eq!(aborts.len(), 1);
        assert_eq!(aborts[0].1, "exp-1000-1:db-01:0");
        assert_eq!(exp.phase, ExperimentPhase::Halting);
        assert!(exp.cause.is_some());

        // Firing again collects nothing new (abort is never re-requested).
        let again = fire_kill_switch(
            &mut exp,
            Cause {
                hostname: None,
                instance_id: None,
                reason: "second".into(),
                ts_unix_ms: 3_000,
            },
        );
        assert!(again.is_empty());
        assert!(
            exp.cause.as_ref().unwrap().reason.contains("boom"),
            "the first cause wins"
        );
    }

    #[test]
    fn conclude_fills_outcome_and_cause() {
        let mut exp = minted_two_hosts();
        exp.apply_status("exp-1000-1:web-01:0", InstanceState::Done, "", 2_000);
        conclude_if_done(&mut exp);
        assert_eq!(
            exp.phase,
            ExperimentPhase::Running,
            "one instance still live"
        );

        exp.apply_status(
            "exp-1000-1:db-01:0",
            InstanceState::Aborted,
            "halted",
            2_500,
        );
        conclude_if_done(&mut exp);
        assert_eq!(
            exp.phase,
            ExperimentPhase::Concluded(ExperimentOutcome::Aborted)
        );
        assert!(exp.cause.as_ref().unwrap().reason.contains("halted"));
    }
}
