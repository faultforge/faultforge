//! The fault-instance lifecycle states.

use serde::{Deserialize, Serialize};

/// The lifecycle state of a single fault instance.
///
/// The serde representation is `SCREAMING_CASE` (`"ACTIVE"`, `"PREFLIGHT"`, …) so
/// it matches the `state` field of plugin `status` lines verbatim.
///
/// States split into two groups: those a plugin MAY assert in its `status` output
/// and those only the agent's own state machine may assign — see
/// [`InstanceState::plugin_emittable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InstanceState {
    /// Agent-owned: the instance is known but no command has run yet.
    Pending,
    /// Preflight checks are running.
    Preflight,
    /// The fault is being injected.
    Injecting,
    /// The fault is active on the host.
    Active,
    /// The fault is being reverted.
    Recovering,
    /// The instance completed and the host is clean.
    Done,
    /// Agent-owned: the instance stopped before (or without) any host effect.
    Aborted,
    /// Agent-owned: an unexpected failure occurred.
    Error,
}

impl InstanceState {
    /// Returns `true` if a plugin MAY assert this state in its `status` output.
    ///
    /// Only `PREFLIGHT`, `INJECTING`, `ACTIVE`, `RECOVERING`, and `DONE` are
    /// plugin-emittable. `PENDING`, `ABORTED`, and `ERROR` are assigned solely by
    /// the agent's state machine; a plugin line claiming one of them is telemetry
    /// only and must not drive the instance's lifecycle.
    #[must_use]
    pub fn plugin_emittable(self) -> bool {
        matches!(
            self,
            Self::Preflight | Self::Injecting | Self::Active | Self::Recovering | Self::Done
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_to_screaming_case() {
        assert_eq!(
            serde_json::to_string(&InstanceState::Active).unwrap(),
            "\"ACTIVE\""
        );
        assert_eq!(
            serde_json::to_string(&InstanceState::Preflight).unwrap(),
            "\"PREFLIGHT\""
        );
        assert_eq!(
            serde_json::to_string(&InstanceState::Done).unwrap(),
            "\"DONE\""
        );
    }

    #[test]
    fn deserializes_from_screaming_case() {
        let state: InstanceState = serde_json::from_str("\"RECOVERING\"").unwrap();
        assert_eq!(state, InstanceState::Recovering);
    }

    #[test]
    fn plugin_emittable_states_are_exactly_the_five() {
        for state in [
            InstanceState::Preflight,
            InstanceState::Injecting,
            InstanceState::Active,
            InstanceState::Recovering,
            InstanceState::Done,
        ] {
            assert!(
                state.plugin_emittable(),
                "{state:?} must be plugin-emittable"
            );
        }
    }

    #[test]
    fn agent_owned_states_are_not_plugin_emittable() {
        for state in [
            InstanceState::Pending,
            InstanceState::Aborted,
            InstanceState::Error,
        ] {
            assert!(
                !state.plugin_emittable(),
                "{state:?} must be agent-owned only"
            );
        }
    }
}
