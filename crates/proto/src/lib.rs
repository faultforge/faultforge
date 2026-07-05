//! Shared gRPC contract (the "schema") for `FaultForge` master <-> agent.
//!
//! The protobuf definitions in `proto/faultforge.proto` are compiled by
//! `build.rs` and included here under [`v1`].

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

#[allow(clippy::pedantic)]
pub mod v1 {
    tonic::include_proto!("faultforge.v1");
}

use std::time::{SystemTime, UNIX_EPOCH};

/// Convert a `SystemTime` to milliseconds since the Unix epoch.
///
/// The caller supplies the time (e.g. from a `Clock`), so no module reads the
/// wall clock implicitly.
///
/// # Panics
///
/// Panics if the system clock is set before the Unix epoch.
#[must_use]
pub fn unix_ms(t: SystemTime) -> i64 {
    // ms since epoch fits comfortably in i64 for ~292 million years; the cast is safe.
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::expect_used)] // panics only if the system clock predates the Unix epoch
    let ms = t
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_millis() as i64;
    ms
}

/// Maximum total length of a hostname, per RFC 1123 / RFC 1035.
const MAX_HOSTNAME_LEN: usize = 253;
/// Maximum length of a single dot-separated label, per RFC 1123.
const MAX_LABEL_LEN: usize = 63;

/// Error type returned by [`Hostname::parse`].
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum HostnameError {
    /// The supplied string was empty or contained only whitespace.
    #[error("hostname must not be empty")]
    Empty,
    /// The supplied string exceeded [`MAX_HOSTNAME_LEN`] bytes.
    // Carries only the length, never the input: this is the log-injection
    // guard (SEC-6), so the offending bytes must never reach a log line.
    #[error("hostname must be at most 253 bytes, got {0}")]
    TooLong(usize),
    /// A dot-separated label violated the RFC 1123 label rules.
    // Debug-formatted (`{0:?}`) so control characters in the offending label
    // are escaped rather than emitted raw — the SEC-6 log-injection guard.
    #[error(
        "hostname label {0:?} is invalid: each label must be 1-63 bytes \
         of [A-Za-z0-9-] with no leading or trailing hyphen"
    )]
    InvalidLabel(String),
}

/// Validated hostname: RFC 1123 charset and length, non-empty.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hostname(String);

/// Validate a single dot-separated hostname label against RFC 1123.
fn validate_label(label: &str) -> Result<(), HostnameError> {
    let ok = (1..=MAX_LABEL_LEN).contains(&label.len())
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(HostnameError::InvalidLabel(label.to_string()))
    }
}

impl Hostname {
    /// Parse and validate a hostname string (surrounding whitespace is trimmed).
    ///
    /// Enforces RFC-1123 DNS-name rules: total length 1..=253 bytes, one or
    /// more dot-separated labels each 1..=63 bytes drawn from `[A-Za-z0-9-]`
    /// with no leading or trailing hyphen. Lengths are measured in bytes, but
    /// the `[A-Za-z0-9-]` charset is ASCII-only, so every accepted hostname has
    /// one byte per character. This bounds identity keys and keeps control
    /// characters out of logs (SEC-6).
    ///
    /// # Errors
    ///
    /// - [`HostnameError::Empty`] if the string is empty or only whitespace.
    /// - [`HostnameError::TooLong`] if it exceeds 253 bytes.
    /// - [`HostnameError::InvalidLabel`] if any label is empty, over 63
    ///   bytes, contains a character outside `[A-Za-z0-9-]` (including
    ///   control characters), or has a leading/trailing hyphen.
    pub fn parse(s: &str) -> Result<Self, HostnameError> {
        let s = s.trim();
        if s.is_empty() {
            return Err(HostnameError::Empty);
        }
        if s.len() > MAX_HOSTNAME_LEN {
            return Err(HostnameError::TooLong(s.len()));
        }
        for label in s.split('.') {
            validate_label(label)?;
        }
        Ok(Self(s.to_string()))
    }

    #[must_use]
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
    use super::v1::{
        AgentMessage, Heartbeat, HeartbeatAck, Register, RegisterAck, ServerMessage, agent_message,
        server_message,
    };
    use super::{Hostname, HostnameError, MAX_HOSTNAME_LEN, unix_ms};
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
        assert!(Hostname::parse("").is_err());
    }

    #[test]
    fn hostname_whitespace_only_is_err() {
        assert!(Hostname::parse("   ").is_err());
    }

    #[test]
    fn hostname_valid_is_ok() {
        let h = Hostname::parse("web-01").expect("valid hostname");
        assert_eq!(h.as_str(), "web-01");
        assert_eq!(h.to_string(), "web-01");
    }

    #[test]
    fn hostname_trims_whitespace() {
        let h = Hostname::parse("  web-01  ").expect("valid hostname with surrounding whitespace");
        assert_eq!(h.as_str(), "web-01");
    }

    #[test]
    fn hostname_fqdn_is_ok() {
        let h = Hostname::parse("web-01.prod.example.com").expect("valid FQDN");
        assert_eq!(h.as_str(), "web-01.prod.example.com");
    }

    #[test]
    fn hostname_control_char_is_rejected() {
        assert_eq!(
            Hostname::parse("web\n01"),
            Err(HostnameError::InvalidLabel("web\n01".to_string()))
        );
        assert!(Hostname::parse("web\t01").is_err());
        assert!(Hostname::parse("web\x01").is_err());
    }

    #[test]
    fn hostname_over_length_is_rejected() {
        let long = "a".repeat(300);
        assert_eq!(Hostname::parse(&long), Err(HostnameError::TooLong(300)));
    }

    #[test]
    fn hostname_length_boundary() {
        // 253 = 3 labels of 63 chars + a 61-char label + 3 dots.
        let at_cap = [
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(61),
        ]
        .join(".");
        assert_eq!(at_cap.len(), MAX_HOSTNAME_LEN);
        assert!(Hostname::parse(&at_cap).is_ok());

        let over_cap = format!("{at_cap}e");
        assert_eq!(over_cap.len(), MAX_HOSTNAME_LEN + 1);
        assert!(matches!(
            Hostname::parse(&over_cap),
            Err(HostnameError::TooLong(_))
        ));
    }

    #[test]
    fn hostname_label_over_63_is_rejected() {
        let label = "a".repeat(64);
        assert_eq!(
            Hostname::parse(&label),
            Err(HostnameError::InvalidLabel(label))
        );
    }

    #[test]
    fn hostname_leading_or_trailing_hyphen_is_rejected() {
        assert!(matches!(
            Hostname::parse("-web"),
            Err(HostnameError::InvalidLabel(_))
        ));
        assert!(matches!(
            Hostname::parse("web-"),
            Err(HostnameError::InvalidLabel(_))
        ));
    }

    #[test]
    fn hostname_leading_or_trailing_dot_is_rejected() {
        // A leading/trailing/doubled dot yields an empty label.
        assert_eq!(
            Hostname::parse(".web"),
            Err(HostnameError::InvalidLabel(String::new()))
        );
        assert!(matches!(
            Hostname::parse("web."),
            Err(HostnameError::InvalidLabel(_))
        ));
        assert!(matches!(
            Hostname::parse("web..prod"),
            Err(HostnameError::InvalidLabel(_))
        ));
    }
}
