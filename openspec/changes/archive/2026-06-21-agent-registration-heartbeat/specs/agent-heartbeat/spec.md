## ADDED Requirements

### Requirement: Agent sends periodic heartbeats at the master-dictated interval
After receiving `RegisterAck`, the agent SHALL send a `Heartbeat {}` message every
`heartbeat_interval_secs` (as provided in `RegisterAck`) for the lifetime of the stream. The
agent SHALL NOT use any locally configured heartbeat interval.

#### Scenario: Agent sends heartbeats on schedule
- **WHEN** the agent has received `RegisterAck { heartbeat_interval_secs: N }`
- **THEN** the agent SHALL send `Heartbeat {}` on the same stream approximately every N seconds

### Requirement: Master acknowledges each heartbeat and updates liveness
On receiving `Heartbeat`, the master SHALL update the `last_seen` of the registry entry
associated with that stream's hostname to the current time and SHALL respond with
`HeartbeatAck`.

#### Scenario: Master updates last_seen and replies
- **WHEN** the master receives `Heartbeat {}` on a stream that registered as `"web-01"`
- **THEN** the master SHALL update the `"web-01"` registry entry's `last_seen` to the current
  time and SHALL send `HeartbeatAck { server_time_unix_ms }` on the same stream

### Requirement: Agent exits on connection or stream failure without reconnecting
If the `Session` stream fails — at initial dial, while awaiting `RegisterAck`, or during the
heartbeat loop — the agent SHALL log the failure and exit. The agent SHALL NOT attempt to
reconnect or retry in this slice.

#### Scenario: Master unreachable at startup
- **WHEN** the agent cannot establish a connection to the configured `master_addr`
- **THEN** the agent SHALL log the error and exit with a non-zero status

#### Scenario: Stream failure during the heartbeat loop
- **WHEN** the `Session` stream is closed or errors while the agent is sending heartbeats
- **THEN** the agent SHALL log the error and exit without retrying
