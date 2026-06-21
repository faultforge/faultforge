//! Shared gRPC contract (the "schema") for FaultForge master <-> agent.
//!
//! The protobuf definitions in `proto/faultforge.proto` are compiled by
//! `build.rs` and included here under [`v1`].

pub mod v1 {
    tonic::include_proto!("faultforge.v1");
}

use std::time::{SystemTime, UNIX_EPOCH};

/// Convert a `SystemTime` to milliseconds since the Unix epoch.
///
/// The caller supplies the time (e.g. from a `Clock`), so no module reads the
/// wall clock implicitly.
pub fn unix_ms(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_millis() as i64
}

/// Validated, non-empty hostname.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hostname(String);

impl Hostname {
    pub fn new(s: &str) -> Result<Self, &'static str> {
        let s = s.trim();
        if s.is_empty() {
            Err("hostname must not be empty")
        } else {
            Ok(Self(s.to_string()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Hostname {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::{unix_ms, Hostname};
    use super::v1::{
        AgentMessage, Heartbeat, HeartbeatAck, Register, RegisterAck, ServerMessage, agent_message,
        server_message,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn unix_ms_of_epoch_is_zero() {
        assert_eq!(unix_ms(UNIX_EPOCH), 0);
    }

    #[test]
    fn unix_ms_of_now_is_positive() {
        assert!(unix_ms(SystemTime::now()) > 0);
    }

    #[test]
    fn register_message_builds_and_reads() {
        let reg = Register {
            hostname: "web-01".into(),
        };
        assert_eq!(reg.hostname, "web-01");
    }

    #[test]
    fn agent_message_register_oneof_round_trips() {
        let msg = AgentMessage {
            payload: Some(agent_message::Payload::Register(Register {
                hostname: "web-01".into(),
            })),
        };
        match msg.payload {
            Some(agent_message::Payload::Register(r)) => assert_eq!(r.hostname, "web-01"),
            other => panic!("expected Register payload, got {other:?}"),
        }
    }

    #[test]
    fn agent_message_heartbeat_oneof_round_trips() {
        let msg = AgentMessage {
            payload: Some(agent_message::Payload::Heartbeat(Heartbeat {})),
        };
        assert!(matches!(
            msg.payload,
            Some(agent_message::Payload::Heartbeat(_))
        ));
    }

    #[test]
    fn server_message_register_ack_oneof_round_trips() {
        let msg = ServerMessage {
            payload: Some(server_message::Payload::RegisterAck(RegisterAck {
                server_time_unix_ms: unix_ms(SystemTime::now()),
                heartbeat_interval_secs: 10,
            })),
        };
        match msg.payload {
            Some(server_message::Payload::RegisterAck(ack)) => {
                assert_eq!(ack.heartbeat_interval_secs, 10);
                assert!(ack.server_time_unix_ms > 0);
            }
            other => panic!("expected RegisterAck payload, got {other:?}"),
        }
    }

    #[test]
    fn server_message_heartbeat_ack_oneof_round_trips() {
        let msg = ServerMessage {
            payload: Some(server_message::Payload::HeartbeatAck(HeartbeatAck {
                server_time_unix_ms: unix_ms(SystemTime::now()),
            })),
        };
        match msg.payload {
            Some(server_message::Payload::HeartbeatAck(ack)) => {
                assert!(ack.server_time_unix_ms > 0);
            }
            other => panic!("expected HeartbeatAck payload, got {other:?}"),
        }
    }

    #[test]
    fn hostname_empty_string_is_err() {
        assert!(Hostname::new("").is_err());
    }

    #[test]
    fn hostname_whitespace_only_is_err() {
        assert!(Hostname::new("   ").is_err());
    }

    #[test]
    fn hostname_valid_is_ok() {
        let h = Hostname::new("web-01").expect("valid hostname");
        assert_eq!(h.as_str(), "web-01");
        assert_eq!(h.to_string(), "web-01");
    }

    #[test]
    fn hostname_trims_whitespace() {
        let h = Hostname::new("  web-01  ").expect("valid hostname with surrounding whitespace");
        assert_eq!(h.as_str(), "web-01");
    }
}
