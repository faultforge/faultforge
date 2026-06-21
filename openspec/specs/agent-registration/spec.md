# Spec: Agent Registration

## Purpose

Defines how an agent identifies itself to the master at startup. The agent sends a `Register` message (hostname only) as the first frame on a new `Session` stream; the master maintains an in-memory registry keyed by hostname and replies with `RegisterAck` containing the heartbeat cadence.

## Requirements

### Requirement: Agent identifies itself by hostname only
The agent SHALL send a `Register` message containing only its hostname as the first
`AgentMessage` on a new `Session` stream. The agent SHALL NOT generate or persist any other
identity (no UUIDs, no local state files).

#### Scenario: Agent sends Register as the first frame
- **WHEN** the agent establishes a `Session` stream with the master
- **THEN** the first `AgentMessage` it sends SHALL be `Register { hostname }` containing the
  agent's current hostname

### Requirement: Agent requires a configured master address
The agent SHALL require a `master_addr` configuration value (via config file, environment
variable, or CLI flag) with no built-in default. If `master_addr` is not provided through any
configuration source, the agent SHALL fail to start.

#### Scenario: Missing master address fails startup
- **WHEN** the agent is started without `master_addr` set via config file, environment, or CLI
  flag
- **THEN** the agent SHALL exit immediately with a non-zero status and an error message
  indicating `master_addr` is required

### Requirement: Master maintains an in-memory registry keyed by hostname
The master SHALL maintain an in-memory registry mapping `hostname` to a connection record
(`hostname`, `name`, `last_seen`). On receiving `Register`, the master SHALL insert a new
record or replace an existing one for that hostname.

#### Scenario: First registration creates a record
- **WHEN** the master receives `Register { hostname: "web-01" }` and no record exists for
  `"web-01"`
- **THEN** the master SHALL create a registry entry for `"web-01"` with `name` seeded to
  `"web-01"` and `last_seen` set to the current time

#### Scenario: Re-registration from the same hostname replaces the existing record
- **WHEN** the master receives `Register { hostname: "web-01" }` and a record already exists
  for `"web-01"`
- **THEN** the master SHALL replace the existing registry entry for `"web-01"` (the new
  registration supersedes the old one)

### Requirement: Master acknowledges registration with server time and heartbeat cadence
After processing a `Register` message, the master SHALL respond with `RegisterAck` containing
the current server time and the heartbeat interval the agent must use for this session.

#### Scenario: Master replies with RegisterAck
- **WHEN** the master has processed a `Register` message
- **THEN** the master SHALL send `RegisterAck { server_time_unix_ms, heartbeat_interval_secs }`
  on the same stream, where `heartbeat_interval_secs` is the master's configured heartbeat
  interval
