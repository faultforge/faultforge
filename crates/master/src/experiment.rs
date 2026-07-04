//! The experiment-lite domain (functional core, design D1/D3/D6): definition
//! and record types, deterministic id minting, all-failures validation, the
//! kill-switch trigger, and the reduced outcome lattice
//! (`COMPLETED`/`ABORTED`/`ERROR`, `ERROR` dominant). Governing decisions:
//! ADR-0002 §11–§17.
//!
//! Everything here is pure over its inputs; the dispatch shell
//! ([`crate::dispatch`]) owns locks, sessions, and timers.

use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::Deserialize;
use serde_json::{Map, Value};

use faultforge_fault::params::validate_params;
use faultforge_fault::state::InstanceState;
use faultforge_proto::Hostname;

use crate::catalog::Catalog;

/// How long past the last possible dead-man the master waits before declaring
/// unresolved instances `ERROR`. Generous against the agent's 30s reconnect
/// backoff cap; promote to config if a deployment's timing differs (design D5).
pub const DEADLINE_MARGIN_MS: i64 = 30_000;

// ===== Operator input =====

/// A reference to a catalog plugin.
#[derive(Debug, Clone, Deserialize)]
pub struct PluginRef {
    /// The plugin name.
    pub name: String,
    /// The plugin version.
    pub version: String,
}

/// One action: a plugin bound to explicit target hosts.
#[derive(Debug, Clone, Deserialize)]
pub struct ActionDefinition {
    /// Explicit target hostnames (the registry has no tags yet).
    pub hosts: Vec<String>,
    /// The catalog plugin to run.
    pub plugin: PluginRef,
    /// Plugin parameters, validated against the manifest `params_schema`.
    pub params: Map<String, Value>,
    /// How long the fault stays active.
    pub duration_secs: u32,
}

/// The operator-submitted experiment definition. `grace_secs` is deliberately
/// absent: grace is decided by the master (ADR-0002 §5), not the operator.
#[derive(Debug, Clone, Deserialize)]
pub struct ExperimentDefinition {
    /// Human label for the experiment.
    pub name: String,
    /// Flat action list — one implicit stage, single salvo (ADR-0002 §11).
    pub actions: Vec<ActionDefinition>,
}

// ===== Ids =====

/// Master-minted experiment identifier, unique per master lifetime (which
/// matches the in-memory record's lifetime, ADR-0002 §17).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExperimentId(String);

impl ExperimentId {
    /// Mint an id from the accept time and a per-process sequence number.
    #[must_use]
    pub fn mint(now_ms: i64, seq: u64) -> Self {
        Self(format!("exp-{now_ms}-{seq}"))
    }

    /// Parse an id supplied by an operator (URL path). Any non-empty string is
    /// accepted — lookups simply miss for ids this master never minted.
    #[must_use]
    pub fn from_raw(s: &str) -> Self {
        Self(s.to_string())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ExperimentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The deterministic instance id for (experiment, agent, action) — ADR-0002
/// §12: a reported instance always correlates to the command that minted it.
#[must_use]
pub fn instance_id(experiment: &ExperimentId, hostname: &Hostname, action_index: usize) -> String {
    format!("{experiment}:{hostname}:{action_index}")
}

/// Whether `hostname` stays within the agent's instance-id charset
/// (`[A-Za-z0-9._-]` — `:` is excluded because it is the id separator).
#[must_use]
pub fn hostname_id_safe(hostname: &str) -> bool {
    !hostname.is_empty()
        && hostname
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

// ===== Validation =====

/// Everything validation needs to know about one target hostname, gathered by
/// the shell so the checks stay pure.
#[derive(Debug, Clone, Copy, Default)]
pub struct HostFacts {
    /// Present in the registry.
    pub registered: bool,
    /// Has a live `Session` right now.
    pub connected: bool,
    /// Quarantined per the agent's last `TaintStatus`.
    pub tainted: bool,
}

/// One failed VALIDATE check, named per the spec ("rejection names all
/// failures").
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationFailure {
    /// The experiment has no actions.
    #[error("experiment has no actions")]
    NoActions,
    /// An action targets no hosts.
    #[error("action {action}: no target hosts")]
    NoHosts {
        /// The offending action index.
        action: usize,
    },
    /// The referenced plugin is not in the master catalog.
    #[error("action {action}: unknown plugin {name}@{version}")]
    UnknownPlugin {
        /// The offending action index.
        action: usize,
        /// The requested plugin name.
        name: String,
        /// The requested plugin version.
        version: String,
    },
    /// The params do not conform to the manifest `params_schema`.
    #[error("action {action}: invalid params: {reason}")]
    InvalidParams {
        /// The offending action index.
        action: usize,
        /// The shared validator's message.
        reason: String,
    },
    /// The duration is zero.
    #[error("action {action}: duration_secs must be greater than 0")]
    ZeroDuration {
        /// The offending action index.
        action: usize,
    },
    /// The duration exceeds the manifest cap.
    #[error(
        "action {action}: duration {duration_secs}s exceeds plugin max_duration_secs {max_secs}s"
    )]
    DurationOverMax {
        /// The offending action index.
        action: usize,
        /// The requested duration.
        duration_secs: u32,
        /// The manifest `max_duration_secs`.
        max_secs: u32,
    },
    /// The hostname would produce an invalid instance id.
    #[error("action {action}: hostname '{hostname}' is outside the instance-id charset")]
    HostnameNotIdSafe {
        /// The offending action index.
        action: usize,
        /// The offending hostname.
        hostname: String,
    },
    /// The same hostname appears twice within one action.
    #[error("action {action}: duplicate host '{hostname}'")]
    DuplicateHost {
        /// The offending action index.
        action: usize,
        /// The duplicated hostname.
        hostname: String,
    },
    /// The target host is not registered.
    #[error("action {action}: host '{hostname}' is not registered")]
    HostUnknown {
        /// The offending action index.
        action: usize,
        /// The unknown hostname.
        hostname: String,
    },
    /// The target host has no live session.
    #[error("action {action}: host '{hostname}' has no live session")]
    HostDisconnected {
        /// The offending action index.
        action: usize,
        /// The disconnected hostname.
        hostname: String,
    },
    /// The target host is quarantined.
    #[error("action {action}: host '{hostname}' is TAINTED")]
    HostTainted {
        /// The offending action index.
        action: usize,
        /// The tainted hostname.
        hostname: String,
    },
}

/// Run every VALIDATE check and return **all** failures (spec: the rejection
/// names every failing check). An empty result means the experiment may
/// dispatch.
#[must_use]
#[allow(clippy::implicit_hasher)] // an internal lookup table, not a generic API
pub fn validate(
    definition: &ExperimentDefinition,
    catalog: &Catalog,
    hosts: &HashMap<String, HostFacts>,
) -> Vec<ValidationFailure> {
    let mut failures = vec![];
    if definition.actions.is_empty() {
        failures.push(ValidationFailure::NoActions);
    }
    for (action, def) in definition.actions.iter().enumerate() {
        if def.hosts.is_empty() {
            failures.push(ValidationFailure::NoHosts { action });
        }
        match catalog.get(&def.plugin.name, &def.plugin.version) {
            None => failures.push(ValidationFailure::UnknownPlugin {
                action,
                name: def.plugin.name.clone(),
                version: def.plugin.version.clone(),
            }),
            Some(entry) => {
                if let Err(e) = validate_params(&entry.manifest.params_schema, &def.params) {
                    failures.push(ValidationFailure::InvalidParams {
                        action,
                        reason: e.to_string(),
                    });
                }
                if def.duration_secs > entry.manifest.max_duration_secs {
                    failures.push(ValidationFailure::DurationOverMax {
                        action,
                        duration_secs: def.duration_secs,
                        max_secs: entry.manifest.max_duration_secs,
                    });
                }
            }
        }
        if def.duration_secs == 0 {
            failures.push(ValidationFailure::ZeroDuration { action });
        }
        let mut seen = HashSet::new();
        for hostname in &def.hosts {
            if !seen.insert(hostname.as_str()) {
                failures.push(ValidationFailure::DuplicateHost {
                    action,
                    hostname: hostname.clone(),
                });
                continue;
            }
            if !hostname_id_safe(hostname) {
                failures.push(ValidationFailure::HostnameNotIdSafe {
                    action,
                    hostname: hostname.clone(),
                });
                continue;
            }
            let facts = hosts.get(hostname).copied().unwrap_or_default();
            if !facts.registered {
                failures.push(ValidationFailure::HostUnknown {
                    action,
                    hostname: hostname.clone(),
                });
            } else if !facts.connected {
                failures.push(ValidationFailure::HostDisconnected {
                    action,
                    hostname: hostname.clone(),
                });
            } else if facts.tainted {
                failures.push(ValidationFailure::HostTainted {
                    action,
                    hostname: hostname.clone(),
                });
            }
        }
    }
    failures
}

// ===== The record =====

/// What determined an experiment's halt or outcome — the legibility rule
/// (ADR-0002 §15): every non-`COMPLETED` outcome names its cause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cause {
    /// The host that determined it, when one did.
    pub hostname: Option<String>,
    /// The instance that determined it, when one did.
    pub instance_id: Option<String>,
    /// What happened.
    pub reason: String,
    /// When, unix milliseconds.
    pub ts_unix_ms: i64,
}

/// The reduced outcome lattice (design D6). `Error` dominates `Aborted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExperimentOutcome {
    /// Every instance reached `DONE`.
    Completed,
    /// Clean halt: no broken host, but not every instance completed.
    Aborted,
    /// A host may be broken: an instance ended `ERROR` or a host was tainted.
    Error,
}

/// The observable experiment lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExperimentPhase {
    /// Dispatched, instances in flight.
    Running,
    /// Kill-switch fired; waiting for instances to reach terminal states.
    Halting,
    /// All instances terminal (or the deadline resolved them).
    Concluded(ExperimentOutcome),
}

/// One dispatched fault instance as the master tracks it.
#[derive(Debug, Clone)]
pub struct InstanceRecord {
    /// The deterministic instance id.
    pub instance_id: String,
    /// The target host.
    pub hostname: Hostname,
    /// Index into the definition's `actions`.
    pub action_index: usize,
    /// The state last reported by the agent (`Pending` until any report).
    pub state: InstanceState,
    /// The last reported reason (empty when none).
    pub reason: String,
    /// When the state last changed, unix milliseconds.
    pub updated_unix_ms: i64,
    /// Whether the master itself requested an abort for this instance — an
    /// `ABORTED` the master did not request is a kill-switch trigger.
    pub abort_requested: bool,
}

impl InstanceRecord {
    /// Terminal per the lifecycle contract.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            InstanceState::Done | InstanceState::Aborted | InstanceState::Error
        )
    }
}

/// A host taint the master learned of while the experiment was live.
#[derive(Debug, Clone)]
pub struct TaintedHost {
    /// The quarantined host.
    pub hostname: String,
    /// The agent-reported reason.
    pub reason: String,
    /// When it was reported, unix milliseconds.
    pub ts_unix_ms: i64,
}

/// The in-memory experiment record (ADR-0002 §17: memory only; a master
/// restart loses it while agents keep hosts safe).
#[derive(Debug, Clone)]
pub struct Experiment {
    /// Master-minted id.
    pub id: ExperimentId,
    /// The operator's definition, kept verbatim (params are read at dispatch).
    pub definition: ExperimentDefinition,
    /// Master-decided grace, applied to every instance.
    pub grace_secs: u32,
    /// Accept/dispatch time, unix milliseconds.
    pub started_unix_ms: i64,
    /// When the master force-resolves the record (design D5).
    pub deadline_unix_ms: i64,
    /// Current phase.
    pub phase: ExperimentPhase,
    /// One record per (host, action), in dispatch order.
    pub instances: Vec<InstanceRecord>,
    /// In-scope hosts reported tainted while the experiment was live.
    pub tainted_hosts: Vec<TaintedHost>,
    /// What fired the kill-switch or decided the outcome, once known.
    pub cause: Option<Cause>,
}

/// When the master force-resolves an experiment: the latest possible dead-man
/// across actions plus `margin_ms` (normally [`DEADLINE_MARGIN_MS`]).
#[must_use]
pub fn deadline_unix_ms(
    definition: &ExperimentDefinition,
    grace_secs: u32,
    now_ms: i64,
    margin_ms: i64,
) -> i64 {
    let max_window_secs = definition
        .actions
        .iter()
        .map(|a| i64::from(a.duration_secs) + i64::from(grace_secs))
        .max()
        .unwrap_or(0);
    now_ms + max_window_secs * 1000 + margin_ms
}

/// Build the record for an accepted (already validated) definition, minting
/// one instance per (host, action).
#[must_use]
pub fn mint_experiment(
    id: ExperimentId,
    definition: ExperimentDefinition,
    grace_secs: u32,
    now_ms: i64,
    deadline_margin_ms: i64,
) -> Experiment {
    let mut instances = vec![];
    for (action_index, action) in definition.actions.iter().enumerate() {
        for hostname in &action.hosts {
            // Validation already vetted the hostname; an unparsable one here
            // would be a validate/mint mismatch, so skip defensively.
            let Ok(hostname) = Hostname::parse(hostname) else {
                continue;
            };
            instances.push(InstanceRecord {
                instance_id: instance_id(&id, &hostname, action_index),
                hostname,
                action_index,
                state: InstanceState::Pending,
                reason: String::new(),
                updated_unix_ms: now_ms,
                abort_requested: false,
            });
        }
    }
    let deadline = deadline_unix_ms(&definition, grace_secs, now_ms, deadline_margin_ms);
    Experiment {
        id,
        definition,
        grace_secs,
        started_unix_ms: now_ms,
        deadline_unix_ms: deadline,
        phase: ExperimentPhase::Running,
        instances,
        tainted_hosts: vec![],
        cause: None,
    }
}

impl Experiment {
    /// The record for `instance_id`, if this experiment minted it.
    #[must_use]
    pub fn instance(&self, instance_id: &str) -> Option<&InstanceRecord> {
        self.instances.iter().find(|i| i.instance_id == instance_id)
    }

    /// Apply an agent-authoritative state report. Terminal states absorb:
    /// once an instance is terminal, later reports are ignored (the agent
    /// never regresses; replay duplicates must not either). Returns `true`
    /// if the record changed.
    pub fn apply_status(
        &mut self,
        instance_id: &str,
        state: InstanceState,
        reason: &str,
        ts_unix_ms: i64,
    ) -> bool {
        let Some(record) = self
            .instances
            .iter_mut()
            .find(|i| i.instance_id == instance_id)
        else {
            return false;
        };
        if record.is_terminal() {
            return false;
        }
        record.state = state;
        record.reason = reason.to_string();
        record.updated_unix_ms = ts_unix_ms;
        true
    }

    /// Record a taint report for a host this experiment targets.
    pub fn record_taint(&mut self, hostname: &str, reason: &str, ts_unix_ms: i64) {
        if self.tainted_hosts.iter().any(|t| t.hostname == hostname) {
            return;
        }
        self.tainted_hosts.push(TaintedHost {
            hostname: hostname.to_string(),
            reason: reason.to_string(),
            ts_unix_ms,
        });
    }

    /// Whether this experiment targets `hostname`.
    #[must_use]
    pub fn targets_host(&self, hostname: &str) -> bool {
        self.instances
            .iter()
            .any(|i| i.hostname.as_str() == hostname)
    }

    /// The kill-switch trigger, if the record now holds one (design D6): the
    /// first instance `ERROR`, the first `ABORTED` the master did not request,
    /// or the first in-scope taint. Only meaningful while `Running` — once
    /// `Halting`, the switch has already fired.
    #[must_use]
    pub fn kill_trigger(&self) -> Option<Cause> {
        if self.phase != ExperimentPhase::Running {
            return None;
        }
        for instance in &self.instances {
            let reason = match instance.state {
                InstanceState::Error => format!("instance ERROR: {}", instance.reason),
                InstanceState::Aborted if !instance.abort_requested => {
                    format!("agent-initiated abort: {}", instance.reason)
                }
                _ => continue,
            };
            return Some(Cause {
                hostname: Some(instance.hostname.to_string()),
                instance_id: Some(instance.instance_id.clone()),
                reason,
                ts_unix_ms: instance.updated_unix_ms,
            });
        }
        self.tainted_hosts.first().map(|taint| Cause {
            hostname: Some(taint.hostname.clone()),
            instance_id: None,
            reason: format!("host TAINTED: {}", taint.reason),
            ts_unix_ms: taint.ts_unix_ms,
        })
    }

    /// The outcome once every instance is terminal; `None` while any is live.
    /// Precedence per ADR-0002 §13/§15: `ERROR` (broken host possible)
    /// dominates `ABORTED`; `COMPLETED` needs every instance `DONE`.
    #[must_use]
    pub fn outcome(&self) -> Option<ExperimentOutcome> {
        if self.instances.iter().any(|i| !i.is_terminal()) {
            return None;
        }
        let any_error = self
            .instances
            .iter()
            .any(|i| i.state == InstanceState::Error)
            || !self.tainted_hosts.is_empty();
        if any_error {
            return Some(ExperimentOutcome::Error);
        }
        if self
            .instances
            .iter()
            .all(|i| i.state == InstanceState::Done)
        {
            Some(ExperimentOutcome::Completed)
        } else {
            Some(ExperimentOutcome::Aborted)
        }
    }

    /// The cause to record for `outcome` when none was recorded earlier (e.g.
    /// a clean halt already carries the operator cause; an `ERROR` outcome
    /// must name the broken host).
    #[must_use]
    pub fn outcome_cause(&self, outcome: ExperimentOutcome) -> Option<Cause> {
        match outcome {
            ExperimentOutcome::Completed => None,
            ExperimentOutcome::Error => {
                if let Some(instance) = self
                    .instances
                    .iter()
                    .find(|i| i.state == InstanceState::Error)
                {
                    return Some(Cause {
                        hostname: Some(instance.hostname.to_string()),
                        instance_id: Some(instance.instance_id.clone()),
                        reason: format!("instance ERROR: {}", instance.reason),
                        ts_unix_ms: instance.updated_unix_ms,
                    });
                }
                self.tainted_hosts.first().map(|taint| Cause {
                    hostname: Some(taint.hostname.clone()),
                    instance_id: None,
                    reason: format!("host TAINTED: {}", taint.reason),
                    ts_unix_ms: taint.ts_unix_ms,
                })
            }
            ExperimentOutcome::Aborted => self
                .instances
                .iter()
                .find(|i| i.state == InstanceState::Aborted)
                .map(|instance| Cause {
                    hostname: Some(instance.hostname.to_string()),
                    instance_id: Some(instance.instance_id.clone()),
                    reason: format!("instance ABORTED: {}", instance.reason),
                    ts_unix_ms: instance.updated_unix_ms,
                }),
        }
    }

    /// Whether the record is terminal.
    #[must_use]
    pub fn is_concluded(&self) -> bool {
        matches!(self.phase, ExperimentPhase::Concluded(_))
    }
}

// ===== Unit tests =====

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::load_catalog;
    use serde_json::json;
    use std::path::Path;

    const MANIFEST: &str = "name: fixture\nversion: \"1\"\nentrypoint: ./run.sh\n\
                            params_schema:\n  marker_path: string\nmax_duration_secs: 60\n";

    fn catalog_with_fixture() -> (tempfile::TempDir, Catalog) {
        let root = tempfile::tempdir().unwrap();
        install(root.path());
        let catalog = load_catalog(root.path());
        (root, catalog)
    }

    fn install(root: &Path) {
        let dir = root.join("fixture@1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.yaml"), MANIFEST).unwrap();
        std::fs::write(dir.join("run.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    }

    fn params() -> Map<String, Value> {
        json!({"marker_path": "/tmp/m"})
            .as_object()
            .unwrap()
            .clone()
    }

    fn action(hosts: &[&str], duration_secs: u32) -> ActionDefinition {
        ActionDefinition {
            hosts: hosts.iter().map(ToString::to_string).collect(),
            plugin: PluginRef {
                name: "fixture".into(),
                version: "1".into(),
            },
            params: params(),
            duration_secs,
        }
    }

    fn definition(actions: Vec<ActionDefinition>) -> ExperimentDefinition {
        ExperimentDefinition {
            name: "test".into(),
            actions,
        }
    }

    fn live_host() -> HostFacts {
        HostFacts {
            registered: true,
            connected: true,
            tainted: false,
        }
    }

    fn hosts(entries: &[(&str, HostFacts)]) -> HashMap<String, HostFacts> {
        entries
            .iter()
            .map(|(h, f)| ((*h).to_string(), *f))
            .collect()
    }

    // ----- ids -----

    #[test]
    fn instance_ids_are_deterministic_and_id_safe() {
        let id = ExperimentId::mint(1_000, 1);
        let host = Hostname::parse("web-01").unwrap();
        assert_eq!(instance_id(&id, &host, 0), "exp-1000-1:web-01:0");
        assert_eq!(instance_id(&id, &host, 0), instance_id(&id, &host, 0));
    }

    #[test]
    fn hostname_charset_gate() {
        assert!(hostname_id_safe("web-01.prod_x"));
        for bad in ["", "web/01", "web 01", "web:01", "host\n"] {
            assert!(!hostname_id_safe(bad), "{bad:?} must be rejected");
        }
    }

    // ----- validation -----

    #[test]
    fn valid_definition_passes() {
        let (_root, catalog) = catalog_with_fixture();
        let def = definition(vec![action(&["web-01"], 5)]);
        let failures = validate(&def, &catalog, &hosts(&[("web-01", live_host())]));
        assert!(failures.is_empty(), "{failures:?}");
    }

    #[test]
    fn all_failures_are_reported_together() {
        let (_root, catalog) = catalog_with_fixture();
        // Unknown plugin on action 0 AND a disconnected host on action 1.
        let mut bad_plugin = action(&["web-01"], 5);
        bad_plugin.plugin.name = "ghost".into();
        let def = definition(vec![bad_plugin, action(&["db-01"], 5)]);
        let facts = hosts(&[
            ("web-01", live_host()),
            (
                "db-01",
                HostFacts {
                    registered: true,
                    connected: false,
                    tainted: false,
                },
            ),
        ]);
        let failures = validate(&def, &catalog, &facts);
        assert!(
            failures
                .iter()
                .any(|f| matches!(f, ValidationFailure::UnknownPlugin { action: 0, .. }))
        );
        assert!(
            failures
                .iter()
                .any(|f| matches!(f, ValidationFailure::HostDisconnected { action: 1, .. }))
        );
        assert_eq!(failures.len(), 2);
    }

    #[test]
    fn params_and_duration_are_checked_against_the_manifest() {
        let (_root, catalog) = catalog_with_fixture();
        let mut def = definition(vec![action(&["web-01"], 61)]);
        def.actions[0].params = json!({"marker_path": 7}).as_object().unwrap().clone();
        let failures = validate(&def, &catalog, &hosts(&[("web-01", live_host())]));
        assert!(
            failures
                .iter()
                .any(|f| matches!(f, ValidationFailure::InvalidParams { .. }))
        );
        assert!(
            failures
                .iter()
                .any(|f| matches!(f, ValidationFailure::DurationOverMax { max_secs: 60, .. }))
        );
    }

    #[test]
    fn zero_duration_unknown_host_and_taint_are_rejected() {
        let (_root, catalog) = catalog_with_fixture();
        let def = definition(vec![action(&["ghost-01"], 0), action(&["tainted-01"], 5)]);
        let facts = hosts(&[(
            "tainted-01",
            HostFacts {
                registered: true,
                connected: true,
                tainted: true,
            },
        )]);
        let failures = validate(&def, &catalog, &facts);
        assert!(
            failures
                .iter()
                .any(|f| matches!(f, ValidationFailure::ZeroDuration { action: 0 }))
        );
        assert!(
            failures
                .iter()
                .any(|f| matches!(f, ValidationFailure::HostUnknown { action: 0, .. }))
        );
        assert!(
            failures
                .iter()
                .any(|f| matches!(f, ValidationFailure::HostTainted { action: 1, .. }))
        );
    }

    #[test]
    fn duplicate_host_within_action_rejected_but_across_actions_allowed() {
        let (_root, catalog) = catalog_with_fixture();
        let dup = definition(vec![action(&["web-01", "web-01"], 5)]);
        let failures = validate(&dup, &catalog, &hosts(&[("web-01", live_host())]));
        assert_eq!(
            failures,
            vec![ValidationFailure::DuplicateHost {
                action: 0,
                hostname: "web-01".into()
            }]
        );

        let across = definition(vec![action(&["web-01"], 5), action(&["web-01"], 5)]);
        assert!(
            validate(&across, &catalog, &hosts(&[("web-01", live_host())])).is_empty(),
            "one instance per action on the same host is by design (ADR-0002 §8)"
        );
    }

    #[test]
    fn empty_experiment_and_empty_hosts_are_rejected() {
        let (_root, catalog) = catalog_with_fixture();
        assert_eq!(
            validate(&definition(vec![]), &catalog, &HashMap::new()),
            vec![ValidationFailure::NoActions]
        );
        let failures = validate(&definition(vec![action(&[], 5)]), &catalog, &HashMap::new());
        assert!(failures.contains(&ValidationFailure::NoHosts { action: 0 }));
    }

    #[test]
    fn charset_violating_hostname_is_rejected() {
        let (_root, catalog) = catalog_with_fixture();
        let def = definition(vec![action(&["web:01"], 5)]);
        let failures = validate(&def, &catalog, &HashMap::new());
        assert_eq!(
            failures,
            vec![ValidationFailure::HostnameNotIdSafe {
                action: 0,
                hostname: "web:01".into()
            }]
        );
    }

    // ----- record, kill trigger, outcome -----

    fn minted(hosts_per_action: Vec<Vec<&str>>) -> Experiment {
        let actions = hosts_per_action
            .into_iter()
            .map(|hosts| action(&hosts, 5))
            .collect();
        mint_experiment(
            ExperimentId::mint(1_000, 1),
            definition(actions),
            10,
            1_000,
            DEADLINE_MARGIN_MS,
        )
    }

    #[test]
    fn mint_creates_one_instance_per_host_action_pair() {
        let exp = minted(vec![vec!["web-01", "db-01"], vec!["web-01"]]);
        let ids: Vec<_> = exp
            .instances
            .iter()
            .map(|i| i.instance_id.clone())
            .collect();
        assert_eq!(
            ids,
            vec![
                "exp-1000-1:web-01:0",
                "exp-1000-1:db-01:0",
                "exp-1000-1:web-01:1"
            ]
        );
        assert_eq!(exp.phase, ExperimentPhase::Running);
        assert_eq!(exp.deadline_unix_ms, 1_000 + 15_000 + DEADLINE_MARGIN_MS);
    }

    #[test]
    fn error_instance_triggers_the_kill_switch() {
        let mut exp = minted(vec![vec!["web-01", "db-01"]]);
        exp.apply_status("exp-1000-1:web-01:0", InstanceState::Error, "boom", 2_000);
        let cause = exp.kill_trigger().unwrap();
        assert_eq!(cause.hostname.as_deref(), Some("web-01"));
        assert_eq!(cause.instance_id.as_deref(), Some("exp-1000-1:web-01:0"));
        assert!(cause.reason.contains("boom"));
        assert_eq!(cause.ts_unix_ms, 2_000);
    }

    #[test]
    fn unrequested_abort_triggers_but_requested_does_not() {
        let mut exp = minted(vec![vec!["web-01", "db-01"]]);
        exp.apply_status(
            "exp-1000-1:web-01:0",
            InstanceState::Aborted,
            "self-abort",
            2_000,
        );
        assert!(exp.kill_trigger().is_some(), "agent-initiated abort fires");

        let mut exp = minted(vec![vec!["web-01", "db-01"]]);
        exp.instances[0].abort_requested = true;
        exp.apply_status("exp-1000-1:web-01:0", InstanceState::Aborted, "", 2_000);
        assert!(
            exp.kill_trigger().is_none(),
            "a master-requested abort is expected, not a trigger"
        );
    }

    #[test]
    fn in_scope_taint_triggers_the_kill_switch() {
        let mut exp = minted(vec![vec!["web-01"]]);
        exp.record_taint("web-01", "cleanup failed", 3_000);
        let cause = exp.kill_trigger().unwrap();
        assert!(cause.reason.contains("TAINTED"));
    }

    #[test]
    fn halting_phase_suppresses_further_triggers() {
        let mut exp = minted(vec![vec!["web-01"]]);
        exp.phase = ExperimentPhase::Halting;
        exp.apply_status("exp-1000-1:web-01:0", InstanceState::Error, "x", 2_000);
        assert!(exp.kill_trigger().is_none());
    }

    #[test]
    fn terminal_states_absorb_later_reports() {
        let mut exp = minted(vec![vec!["web-01"]]);
        assert!(exp.apply_status("exp-1000-1:web-01:0", InstanceState::Done, "", 2_000));
        assert!(!exp.apply_status("exp-1000-1:web-01:0", InstanceState::Active, "", 3_000));
        assert_eq!(exp.instances[0].state, InstanceState::Done);
    }

    #[test]
    fn outcome_is_none_while_any_instance_lives() {
        let mut exp = minted(vec![vec!["web-01", "db-01"]]);
        exp.apply_status("exp-1000-1:web-01:0", InstanceState::Done, "", 2_000);
        assert_eq!(exp.outcome(), None);
    }

    #[test]
    fn outcome_lattice_precedence() {
        // All DONE -> COMPLETED.
        let mut exp = minted(vec![vec!["web-01", "db-01"]]);
        exp.apply_status("exp-1000-1:web-01:0", InstanceState::Done, "", 2_000);
        exp.apply_status("exp-1000-1:db-01:0", InstanceState::Done, "", 2_000);
        assert_eq!(exp.outcome(), Some(ExperimentOutcome::Completed));

        // Any non-DONE without ERROR/taint -> ABORTED.
        let mut exp = minted(vec![vec!["web-01", "db-01"]]);
        exp.apply_status("exp-1000-1:web-01:0", InstanceState::Done, "", 2_000);
        exp.apply_status("exp-1000-1:db-01:0", InstanceState::Aborted, "halt", 2_000);
        assert_eq!(exp.outcome(), Some(ExperimentOutcome::Aborted));

        // ERROR dominates ABORTED.
        let mut exp = minted(vec![vec!["web-01", "db-01"]]);
        exp.apply_status("exp-1000-1:web-01:0", InstanceState::Error, "boom", 2_000);
        exp.apply_status("exp-1000-1:db-01:0", InstanceState::Aborted, "halt", 2_000);
        assert_eq!(exp.outcome(), Some(ExperimentOutcome::Error));
    }

    #[test]
    fn taint_makes_a_clean_halt_error_not_aborted() {
        // The spec scenario: operator halt, then cleanup fails and taints.
        let mut exp = minted(vec![vec!["web-01"]]);
        exp.record_taint("web-01", "cleanup failed twice", 3_000);
        exp.apply_status("exp-1000-1:web-01:0", InstanceState::Error, "taint", 3_000);
        assert_eq!(exp.outcome(), Some(ExperimentOutcome::Error));
        let cause = exp.outcome_cause(ExperimentOutcome::Error).unwrap();
        assert_eq!(cause.hostname.as_deref(), Some("web-01"));
    }

    #[test]
    fn outcome_causes_name_host_instance_reason_and_time() {
        let mut exp = minted(vec![vec!["web-01"]]);
        exp.apply_status(
            "exp-1000-1:web-01:0",
            InstanceState::Aborted,
            "operator halt",
            4_000,
        );
        let cause = exp.outcome_cause(ExperimentOutcome::Aborted).unwrap();
        assert_eq!(cause.hostname.as_deref(), Some("web-01"));
        assert_eq!(cause.instance_id.as_deref(), Some("exp-1000-1:web-01:0"));
        assert!(cause.reason.contains("operator halt"));
        assert_eq!(cause.ts_unix_ms, 4_000);
    }

    #[test]
    fn deadline_covers_the_longest_action_window() {
        let def = definition(vec![action(&["a"], 5), action(&["b"], 30)]);
        assert_eq!(
            deadline_unix_ms(&def, 10, 1_000, DEADLINE_MARGIN_MS),
            1_000 + 40_000 + DEADLINE_MARGIN_MS
        );
    }
}
