# Spec: Agent Heartbeat

## Purpose

Defines the heartbeat protocol between agent and master after initial registration. The agent uses the interval dictated by the master (via `RegisterAck`) to send periodic `Heartbeat` messages on the active `Session` stream; the master updates liveness records and responds with `HeartbeatAck`.
## Requirements
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

### Requirement: Agent reconnects with backoff and tracks master loss

If the `Session` stream fails — at initial dial, while awaiting `RegisterAck`, or during the
heartbeat loop — the agent SHALL log the failure and retry the full connect-and-register
sequence with exponential backoff (capped), retrying indefinitely. The agent SHALL track the
time since master contact was lost and SHALL expose connectivity transitions to the fault
runtime so the master-loss self-abort threshold can be enforced. Each successful reconnection
SHALL be a fresh registration followed by the reconciliation report defined in
`agent-fault-runtime`.

#### Scenario: Master unreachable at startup

- **WHEN** the agent cannot establish a connection to the configured `master_addr`
- **THEN** the agent SHALL log the error and keep retrying with backoff instead of exiting

#### Scenario: Stream failure during the heartbeat loop

- **WHEN** the `Session` stream is closed or errors while the agent is sending heartbeats
- **THEN** the agent SHALL record the moment contact was lost, retry with backoff, and register
  again on the new stream once the master is reachable

