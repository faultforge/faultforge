# Spec: Agent Status API

## Purpose

Defines the read-only HTTP/REST management API on the master that exposes the
in-memory agent registry to operators and tooling. The API is served on a
dedicated address, separate from the gRPC agent plane, and during the WIP phase
runs without authentication or TLS.

## Requirements

### Requirement: Master serves a dedicated HTTP management API
The master SHALL run an HTTP server for management, bound to a configurable
address that is separate from the gRPC `listen_addr`. The HTTP server and the
gRPC server SHALL run concurrently within the same process and share a single
agent registry.

#### Scenario: HTTP server listens on its configured address
- **WHEN** the master starts with `management_listen_addr` set to a valid address
- **THEN** the master SHALL accept HTTP requests on that address while continuing
  to serve the gRPC agent plane on `listen_addr`

#### Scenario: Failure of either server stops the master
- **WHEN** either the gRPC server or the HTTP server fails to bind or terminates
  with an error
- **THEN** the master SHALL stop and exit with a non-zero status

### Requirement: Management API uses a separately configured address
The master SHALL accept an `management_listen_addr` configuration value through the same
layered sources as other master settings (struct default, config file,
environment variable, CLI flag). When no value is supplied, the master SHALL
default `management_listen_addr` to `127.0.0.1:8069`.

#### Scenario: Default address when unconfigured
- **WHEN** the master is started with no `management_listen_addr` set in any
  configuration source
- **THEN** the master SHALL bind the HTTP management API to `127.0.0.1:8069`

#### Scenario: Configured address overrides the default
- **WHEN** `management_listen_addr` is provided via config file, environment variable,
  or CLI flag
- **THEN** the master SHALL bind the HTTP management API to that address instead
  of the default

### Requirement: List all registered agents
The management API SHALL expose `GET /agents` returning every agent currently in
the registry. Each agent SHALL be represented as a JSON object with `hostname`,
`name`, and `last_seen_unix_ms` (the raw last-seen timestamp in Unix
milliseconds). The endpoint SHALL return an empty JSON array when no agents are
registered.

#### Scenario: Registry contains agents
- **WHEN** a client sends `GET /agents` and the registry contains one or more
  agents
- **THEN** the master SHALL respond with `200 OK` and a JSON array containing one
  object per registered agent, each with `hostname`, `name`, and
  `last_seen_unix_ms`

#### Scenario: Registry is empty
- **WHEN** a client sends `GET /agents` and the registry contains no agents
- **THEN** the master SHALL respond with `200 OK` and an empty JSON array

### Requirement: Fetch a single agent by hostname
The management API SHALL expose `GET /agents/{hostname}` returning the registry
record for the given hostname as a JSON object with `hostname`, `name`, and
`last_seen_unix_ms`. When no agent is registered under that hostname, the endpoint
SHALL respond with `404 Not Found`.

#### Scenario: Agent exists
- **WHEN** a client sends `GET /agents/web-01` and an agent is registered under
  `web-01`
- **THEN** the master SHALL respond with `200 OK` and a JSON object containing
  `hostname`, `name`, and `last_seen_unix_ms` for `web-01`

#### Scenario: Agent does not exist
- **WHEN** a client sends `GET /agents/unknown-host` and no agent is registered
  under `unknown-host`
- **THEN** the master SHALL respond with `404 Not Found`

### Requirement: Management API is read-only and unauthenticated during WIP
The management API SHALL NOT expose any endpoint that mutates agent state, and
SHALL NOT require authentication or TLS during the WIP phase. All exposed
endpoints SHALL be safe HTTP reads.

#### Scenario: No write endpoints are exposed
- **WHEN** a client issues a mutating request (e.g. `POST`, `PUT`, `DELETE`) to an
  agent path
- **THEN** the master SHALL NOT modify any registry state in response

#### Scenario: Requests are served without credentials
- **WHEN** a client sends `GET /agents` without any authentication credentials
- **THEN** the master SHALL serve the request normally
