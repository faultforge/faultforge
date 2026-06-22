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
}

/// Wire shape for the master's agent representation.
///
/// Used for both decoding responses and encoding CLI JSON output, so the field
/// names and timestamp units are defined exactly once.
#[derive(Debug, Serialize, Deserialize)]
struct AgentWire {
    hostname: String,
    name: String,
    last_seen_unix_ms: i64,
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(ms: u64) -> Agent {
        Agent {
            hostname: "web-01".to_string(),
            name: "web-01".to_string(),
            last_seen: UNIX_EPOCH + Duration::from_millis(ms),
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
    }
}
