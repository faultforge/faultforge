# Spec Delta: Agent Status API

## REMOVED Requirements

### Requirement: Management API is read-only and unauthenticated during WIP

**Reason**: The management plane gains its first write endpoints (run/halt/clear-taint, defined
in the `master-fault-dispatch` capability), so "SHALL NOT expose any endpoint that mutates
state" no longer holds. The unauthenticated-during-WIP half survives in the replacement
requirement below; auth/TLS remains the ADR-0002 critical known gap.

**Migration**: None for existing clients — all previously existing endpoints keep their
read-only semantics; only new, additive write endpoints appear.

## ADDED Requirements

### Requirement: Management API is unauthenticated during WIP with writes limited to fault dispatch

The management API SHALL NOT require authentication or TLS during the WIP phase. Mutating
endpoints SHALL be limited to the fault-dispatch surface defined by the `master-fault-dispatch`
capability (experiment run/halt, clear-taint); agent registry entries themselves SHALL remain
unmodifiable through the API. The deployment documentation SHALL state that the management
address must not be exposed beyond a trusted network while unauthenticated.

#### Scenario: Requests are served without credentials

- **WHEN** a client sends any management request without authentication credentials
- **THEN** the master SHALL serve the request normally

#### Scenario: Registry entries are not directly mutable

- **WHEN** a client issues a mutating request against an agent path other than the documented
  fault-dispatch endpoints (e.g. `DELETE /agents/web-01`)
- **THEN** the master SHALL NOT modify any registry state in response

## MODIFIED Requirements

### Requirement: List all registered agents

The management API SHALL expose `GET /agents` returning every agent currently in the registry.
Each agent SHALL be represented as a JSON object with `hostname`, `name`, `last_seen_unix_ms`
(the raw last-seen timestamp in Unix milliseconds), and `tainted` (boolean — the host quarantine
state most recently reported by the agent's `TaintStatus`, `false` when never reported). The
endpoint SHALL return an empty JSON array when no agents are registered.

#### Scenario: Registry contains agents

- **WHEN** a client sends `GET /agents` and the registry contains one or more agents
- **THEN** the master SHALL respond with `200 OK` and a JSON array containing one object per
  registered agent, each with `hostname`, `name`, `last_seen_unix_ms`, and `tainted`

#### Scenario: Registry is empty

- **WHEN** a client sends `GET /agents` and the registry contains no agents
- **THEN** the master SHALL respond with `200 OK` and an empty JSON array

#### Scenario: Tainted host is visible

- **WHEN** an agent has reported `TaintStatus { tainted: true }` and a client sends `GET /agents`
- **THEN** that agent's object SHALL carry `tainted: true`

### Requirement: Fetch a single agent by hostname

The management API SHALL expose `GET /agents/{hostname}` returning the registry record for the
given hostname as a JSON object with `hostname`, `name`, `last_seen_unix_ms`, and `tainted`.
When no agent is registered under that hostname, the endpoint SHALL respond with `404 Not
Found`.

#### Scenario: Agent exists

- **WHEN** a client sends `GET /agents/web-01` and an agent is registered under `web-01`
- **THEN** the master SHALL respond with `200 OK` and a JSON object containing `hostname`,
  `name`, `last_seen_unix_ms`, and `tainted` for `web-01`

#### Scenario: Agent does not exist

- **WHEN** a client sends `GET /agents/unknown-host` and no agent is registered under
  `unknown-host`
- **THEN** the master SHALL respond with `404 Not Found`
