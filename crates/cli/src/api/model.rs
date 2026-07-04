//! Domain types for the management API.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// A registered agent as returned by the master's management API.
///
/// Serializes and deserializes through [`AgentWire`], the single source of
/// truth for the on-the-wire shape `{ hostname, name, last_seen_unix_ms }` and
/// the millisecond ⇄ [`SystemTime`] conversion (in both directions).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "AgentWire", from = "AgentWire")]
pub struct Agent {
    /// Hostname — the agent's sole identity key.
    pub hostname: String,
    /// Human-readable display name (seeded from hostname on first registration).
    pub name: String,
    /// Last-seen timestamp, converted to/from the wire `last_seen_unix_ms` field.
    pub last_seen: SystemTime,
    /// Host quarantine state the agent last reported.
    pub tainted: bool,
}

/// Wire shape for the master's agent representation.
///
/// Used for both decoding responses and encoding CLI JSON output, so the field
/// names and timestamp units are defined exactly once. `tainted` defaults to
/// `false` so the CLI still reads pre-dispatch masters that omit the field.
#[derive(Debug, Serialize, Deserialize)]
struct AgentWire {
    hostname: String,
    name: String,
    last_seen_unix_ms: i64,
    #[serde(default)]
    tainted: bool,
}

impl From<AgentWire> for Agent {
    fn from(w: AgentWire) -> Self {
        // A negative epoch offset is not representable; clamp to the epoch.
        let last_seen = u64::try_from(w.last_seen_unix_ms)
            .map_or(UNIX_EPOCH, |ms| UNIX_EPOCH + Duration::from_millis(ms));
        Self {
            hostname: w.hostname,
            name: w.name,
            last_seen,
            tainted: w.tainted,
        }
    }
}

impl From<Agent> for AgentWire {
    fn from(a: Agent) -> Self {
        // Times before the epoch (clock skew) saturate to 0; overflow to i64::MAX.
        let last_seen_unix_ms = a
            .last_seen
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        Self {
            hostname: a.hostname,
            name: a.name,
            last_seen_unix_ms,
            tainted: a.tainted,
        }
    }
}

// ===== Experiments =====

/// What determined an experiment's halt or outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// One dispatched fault instance as reported by the master.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_field_names)] // field names mirror the wire JSON exactly
pub struct Instance {
    /// The master-minted deterministic instance id.
    pub instance_id: String,
    /// The target host.
    pub hostname: String,
    /// Index into the experiment's actions.
    pub action_index: usize,
    /// The agent-reported lifecycle state (`"PENDING"`, `"ACTIVE"`, …). Kept
    /// as a string so newer masters with new states still render.
    pub state: String,
    /// The last reported reason (empty when none).
    pub reason: String,
    /// When the state last changed, unix milliseconds.
    pub updated_unix_ms: i64,
}

/// A full experiment record from `GET /experiments/{id}` / `POST /experiments`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Experiment {
    /// The master-minted experiment id.
    pub id: String,
    /// The operator's label.
    pub name: String,
    /// `"RUNNING"`, `"HALTING"`, or a terminal outcome label.
    pub state: String,
    /// Present exactly when the experiment is terminal.
    pub outcome: Option<String>,
    /// The master-decided grace applied to every instance.
    pub grace_secs: u32,
    /// Dispatch time, unix milliseconds.
    pub started_unix_ms: i64,
    /// When the master force-resolves the record.
    pub deadline_unix_ms: i64,
    /// What determined the halt/outcome, once known.
    pub cause: Option<Cause>,
    /// One entry per (host, action).
    pub instances: Vec<Instance>,
}

impl Experiment {
    /// Whether the experiment has reached a terminal outcome.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.outcome.is_some()
    }
}

/// One row of `GET /experiments`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentSummary {
    /// The master-minted experiment id.
    pub id: String,
    /// The operator's label.
    pub name: String,
    /// `"RUNNING"`, `"HALTING"`, or a terminal outcome label.
    pub state: String,
    /// Present exactly when the experiment is terminal.
    pub outcome: Option<String>,
    /// Dispatch time, unix milliseconds.
    pub started_unix_ms: i64,
    /// Number of fault instances.
    pub instances: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(ms: u64) -> Agent {
        Agent {
            hostname: "web-01".to_string(),
            name: "web-01".to_string(),
            last_seen: UNIX_EPOCH + Duration::from_millis(ms),
            tainted: false,
        }
    }

    #[test]
    fn agent_round_trips_through_json() {
        let original = agent(1000);
        let json = serde_json::to_string(&original).expect("serialize");
        let decoded: Agent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, decoded);
    }

    #[test]
    fn serialized_json_uses_wire_field_names() {
        let json = serde_json::to_value(agent(1000)).expect("serialize");
        assert_eq!(json["hostname"], "web-01");
        assert_eq!(json["last_seen_unix_ms"], 1000);
        assert_eq!(json["tainted"], false);
    }

    #[test]
    fn agent_without_tainted_field_defaults_to_clean() {
        // Compatibility: a pre-dispatch master omits `tainted`.
        let json = r#"{"hostname":"web-01","name":"web-01","last_seen_unix_ms":1000}"#;
        let agent: Agent = serde_json::from_str(json).expect("valid agent JSON");
        assert!(!agent.tainted);
    }

    #[test]
    fn experiment_decodes_from_management_json() {
        let json = r#"{
            "id": "exp-1000-1", "name": "it", "state": "ERROR", "outcome": "ERROR",
            "grace_secs": 10, "started_unix_ms": 1000, "deadline_unix_ms": 46000,
            "cause": {"hostname": "web-01", "instance_id": "exp-1000-1:web-01:0",
                      "reason": "instance ERROR: boom", "ts_unix_ms": 2000},
            "instances": [{"instance_id": "exp-1000-1:web-01:0", "hostname": "web-01",
                           "action_index": 0, "state": "ERROR", "reason": "boom",
                           "updated_unix_ms": 2000}]
        }"#;
        let experiment: Experiment = serde_json::from_str(json).expect("valid experiment JSON");
        assert!(experiment.is_terminal());
        assert_eq!(experiment.instances[0].state, "ERROR");
        assert_eq!(
            experiment.cause.unwrap().hostname.as_deref(),
            Some("web-01")
        );
    }
}
