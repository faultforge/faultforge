//! Domain ⇄ wire conversions for the gRPC contract, behind the `proto` feature.
//!
//! Only endpoints that already link tonic (agent, master) enable this feature;
//! plugin binaries keep the crate lean by leaving it off.

use crate::state::InstanceState;

/// Why a wire state value could not be converted to a domain [`InstanceState`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WireStateError {
    /// The wire value was `INSTANCE_STATE_UNSPECIFIED` — a sender that did not
    /// set the field. The domain has no "unset" state on purpose.
    #[error("instance state is unspecified")]
    Unspecified,
    /// The raw integer did not name any known enum value (a newer peer).
    #[error("unknown instance state value: {0}")]
    Unknown(i32),
}

impl From<InstanceState> for faultforge_proto::v1::InstanceState {
    fn from(state: InstanceState) -> Self {
        match state {
            InstanceState::Pending => Self::Pending,
            InstanceState::Preflight => Self::Preflight,
            InstanceState::Injecting => Self::Injecting,
            InstanceState::Active => Self::Active,
            InstanceState::Recovering => Self::Recovering,
            InstanceState::Done => Self::Done,
            InstanceState::Aborted => Self::Aborted,
            InstanceState::Error => Self::Error,
        }
    }
}

impl TryFrom<faultforge_proto::v1::InstanceState> for InstanceState {
    type Error = WireStateError;

    fn try_from(state: faultforge_proto::v1::InstanceState) -> Result<Self, WireStateError> {
        match state {
            faultforge_proto::v1::InstanceState::Unspecified => Err(WireStateError::Unspecified),
            faultforge_proto::v1::InstanceState::Pending => Ok(Self::Pending),
            faultforge_proto::v1::InstanceState::Preflight => Ok(Self::Preflight),
            faultforge_proto::v1::InstanceState::Injecting => Ok(Self::Injecting),
            faultforge_proto::v1::InstanceState::Active => Ok(Self::Active),
            faultforge_proto::v1::InstanceState::Recovering => Ok(Self::Recovering),
            faultforge_proto::v1::InstanceState::Done => Ok(Self::Done),
            faultforge_proto::v1::InstanceState::Aborted => Ok(Self::Aborted),
            faultforge_proto::v1::InstanceState::Error => Ok(Self::Error),
        }
    }
}

/// The wire integer for a domain state, as carried in `InstanceStatus.state`.
#[must_use]
pub fn to_wire_i32(state: InstanceState) -> i32 {
    faultforge_proto::v1::InstanceState::from(state).into()
}

/// Parse the raw `InstanceStatus.state` integer into a domain state.
///
/// # Errors
///
/// Returns [`WireStateError::Unknown`] for an integer outside the enum (a newer
/// peer's value) or [`WireStateError::Unspecified`] for the unset zero value.
pub fn from_wire_i32(raw: i32) -> Result<InstanceState, WireStateError> {
    let wire = faultforge_proto::v1::InstanceState::try_from(raw)
        .map_err(|_| WireStateError::Unknown(raw))?;
    InstanceState::try_from(wire)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [InstanceState; 8] = [
        InstanceState::Pending,
        InstanceState::Preflight,
        InstanceState::Injecting,
        InstanceState::Active,
        InstanceState::Recovering,
        InstanceState::Done,
        InstanceState::Aborted,
        InstanceState::Error,
    ];

    #[test]
    fn every_domain_state_round_trips_through_the_wire() {
        for state in ALL {
            assert_eq!(from_wire_i32(to_wire_i32(state)).unwrap(), state);
        }
    }

    #[test]
    fn unspecified_is_rejected() {
        assert_eq!(from_wire_i32(0), Err(WireStateError::Unspecified));
    }

    #[test]
    fn unknown_wire_value_is_rejected() {
        assert_eq!(from_wire_i32(999), Err(WireStateError::Unknown(999)));
    }
}
