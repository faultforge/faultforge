//! Shared gRPC contract (the "schema") for FaultForge master <-> agent.
//!
//! The protobuf definitions in `proto/faultforge.proto` are compiled by
//! `build.rs` and included here under [`v1`].

pub mod v1 {
    tonic::include_proto!("faultforge.v1");
}

use std::time::{SystemTime, UNIX_EPOCH};

/// Current wall-clock time in milliseconds since the Unix epoch.
pub fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::now_unix_ms;
    use super::v1::{
        AgentMessage, Heartbeat, Register, RegisterAck, ServerMessage, agent_message,
        server_message,
    };

    #[test]
    fn now_unix_ms_is_positive() {
        assert!(now_unix_ms() > 0);
    }

    #[test]
    fn register_message_builds_and_reads() {
        let reg = Register {
            agent_id: "uuid-1234".into(),
            name: "web-01".into(),
            hostname: "web-01.local".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            version: "0.1.0".into(),
        };
        assert_eq!(reg.agent_id, "uuid-1234");
        assert_eq!(reg.name, "web-01");
    }

    #[test]
    fn agent_message_register_oneof_round_trips() {
        let msg = AgentMessage {
            payload: Some(agent_message::Payload::Register(Register {
                agent_id: "a1".into(),
                name: "n".into(),
                hostname: "h".into(),
                os: "linux".into(),
                arch: "x86_64".into(),
                version: "0.1.0".into(),
            })),
        };
        match msg.payload {
            Some(agent_message::Payload::Register(r)) => assert_eq!(r.agent_id, "a1"),
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
                server_time_unix_ms: now_unix_ms(),
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
}
