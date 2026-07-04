# Spec: Master Fault Dispatch

## Purpose

Defines the master control plane's fault dispatch orchestration, experiment validation, and lifecycle management. This is the master-side counterpart to the agent-side fault runtime and together they form the complete fault injection system.

## Requirements

### Requirement: The master loads a plugin catalog from disk at startup

The master SHALL load its plugin catalog at startup from a configurable `catalog_root`
directory (default `/usr/lib/faultforge/plugins`) using the shared `faultforge-fault` catalog
layout (`<name>@<version>/` with `manifest.yaml` and entrypoint), obtaining each plugin's parsed
manifest and computed digest. A catalog entry that fails to load SHALL be logged and skipped
without preventing master startup. A missing or empty `catalog_root` SHALL yield an empty
catalog, not a startup failure.

#### Scenario: Catalog entry resolves with manifest and digest

- **WHEN** the master starts with a valid plugin at `<catalog_root>/noop-marker@0.1.0/`
- **THEN** experiments referencing `noop-marker` `0.1.0` SHALL validate against that manifest's
  `params_schema` and `max_duration_secs`, and dispatch SHALL carry the digest computed from the
  on-disk manifest and entrypoint bytes

#### Scenario: Broken catalog entry does not stop the control plane

- **WHEN** one directory under `catalog_root` contains an invalid manifest
- **THEN** the master SHALL log the failure, skip that entry, start normally, and serve
  experiments using the remaining valid entries

### Requirement: An experiment-lite is a named set of hostname-targeted actions

The master SHALL accept an experiment defined as a `name` and a flat list of `actions`, each
binding explicit target `hosts` (hostnames), a plugin (`name`, `version`), `params`, and
`duration_secs`. The master SHALL supply `grace_secs` from its configuration
(`default_grace_secs`); the operator SHALL NOT set it per experiment. All actions SHALL be
injected together as a single salvo. A host appearing in multiple actions SHALL receive one
fault instance per action, concurrently. Experiment-lite SHALL NOT include metric rules,
connectors, tag targeting, blast-radius ceilings, or staged sequencing; its record shape SHALL
remain extendable to those without breaking existing definitions.

#### Scenario: One salvo across hosts and actions

- **WHEN** an accepted experiment defines two actions, targeting `web-01` and both `web-01` and
  `db-01` respectively
- **THEN** the master SHALL dispatch all three fault instances together, with `web-01` receiving
  two concurrent instances (one per action)

### Requirement: Experiments are validated synchronously and rejected without dispatch

On submission the master SHALL validate the experiment and, on any failure, reject it naming
every failing check, dispatching nothing. Validation SHALL fail when: a referenced plugin
(`name`, `version`) is absent from the master catalog; `params` fail the manifest
`params_schema` (shared pure validation); `duration_secs` is zero or exceeds the manifest
`max_duration_secs`; a target hostname is not registered, has no live session, or falls outside
the instance-id charset; a target host is `TAINTED` per the master's current knowledge; or a
hostname is duplicated within a single action's `hosts`.

#### Scenario: Rejection names all failures

- **WHEN** an experiment references an unknown plugin and also targets a disconnected host
- **THEN** the master SHALL reject it reporting both failures, and no agent SHALL receive any
  frame for it

#### Scenario: Tainted target blocks the whole experiment

- **WHEN** any target host is `TAINTED` at submission time
- **THEN** the experiment SHALL be rejected and nothing SHALL be dispatched to any host

### Requirement: Accepted experiments dispatch master-minted deterministic instances

For each (target host, action) pair of an accepted experiment the master SHALL mint
`instance_id = <experiment_id>:<hostname>:<action_index>` — deterministic per (experiment,
agent, action) — and send one `RunFault` on that host's live session carrying the instance id,
plugin name/version, the catalog-computed digest, JSON-encoded params, `duration_secs`, and the
master-decided `grace_secs`. The master SHALL NOT re-issue `RunFault` for an instance under any
circumstances.

#### Scenario: Deterministic ids correlate reports to commands

- **WHEN** experiment `exp-100-1` dispatches action `0` to `web-01`
- **THEN** the `RunFault` SHALL carry `instance_id` `exp-100-1:web-01:0`, and any later
  `InstanceStatus` or `InstanceReport` entry with that id SHALL be attributed to that experiment
  and action

#### Scenario: Inject is never retried

- **WHEN** a session drops and reconnects after a `RunFault` was sent but before any
  `InstanceStatus` arrived
- **THEN** the master SHALL NOT send a second `RunFault` for that instance and SHALL reconcile
  from the agent's post-registration `InstanceReport`

### Requirement: The master tracks instance state only from agent-authoritative frames

The master SHALL update per-instance state exclusively from `InstanceStatus` frames and SHALL
accept `InstanceReport` snapshots as the truth for the instances they list. `FaultEvent` frames
SHALL be treated as telemetry (logged) and SHALL never drive instance state. Frames referencing
instances the master does not know SHALL be logged and dropped without failing the session.

#### Scenario: Reconciliation accepts reported state

- **WHEN** an agent re-registers mid-experiment and its `InstanceReport` shows an instance
  `ACTIVE`
- **THEN** the master SHALL record that instance as `ACTIVE` without re-dispatching anything

#### Scenario: Unknown instance frames are tolerated

- **WHEN** the master receives an `InstanceStatus` for an instance id it has no record of (e.g.
  after a master restart)
- **THEN** the master SHALL log and drop the frame and keep the session open

### Requirement: Any failure or halt triggers the global kill-switch

The master SHALL fire the experiment's kill-switch on the first of: any instance reaching
`ERROR`; any instance reaching `ABORTED` that the master did not request; `TaintStatus
{ tainted: true }` from an in-scope host; an operator halt; or experiment-deadline expiry.
Firing the kill-switch SHALL send `AbortFault` for every non-terminal instance whose host has a
live session, mark the experiment `HALTING`, and record the triggering host, instance, reason,
and timestamp. Halt SHALL be most-available: it SHALL succeed even when some target agents are
unreachable (their agent-side self-abort governs the host meanwhile).

#### Scenario: One instance fails, the rest are stopped

- **WHEN** one instance of a running two-host experiment reports `ERROR`
- **THEN** the master SHALL send `AbortFault` for the other non-terminal instance and mark the
  experiment `HALTING`

#### Scenario: Halt with a disconnected agent still succeeds

- **WHEN** an operator halts an experiment while one target agent has no live session
- **THEN** the halt SHALL be accepted, `AbortFault` SHALL go to every reachable non-terminal
  instance, and the experiment SHALL still resolve by its deadline at the latest

### Requirement: Every experiment resolves by a master-side deadline

The master SHALL arm one deadline per experiment at `start + max(duration_secs + grace_secs
over actions) + margin`. Any instance still non-terminal at the deadline SHALL be marked
`ERROR` with a reason naming the unresolved deadline, and the experiment SHALL then resolve to
its outcome. No experiment record SHALL remain non-terminal past its deadline.

#### Scenario: Vanished agent cannot hang the record

- **WHEN** an agent disconnects during injection and never reconnects
- **THEN** at the experiment deadline the master SHALL mark that agent's non-terminal instances
  `ERROR` (unresolved) and resolve the experiment

### Requirement: Outcome is COMPLETED, ABORTED, or ERROR with ERROR dominant and causes named

When every instance is terminal (or at the deadline) the master SHALL classify the experiment:
`ERROR` if any instance ended `ERROR` or any in-scope host became `TAINTED` during the run;
otherwise `ABORTED` if any instance did not reach `DONE`; otherwise `COMPLETED`. `ERROR` SHALL
dominate `ABORTED`. Every `ABORTED` and `ERROR` outcome SHALL name the host, instance, reason,
and timestamp that determined it. The lattice SHALL be extendable to the full `experiment-model`
outcomes (`RESILIENT`/`WEAKNESS_FOUND`) by splitting `COMPLETED` when hypotheses arrive.

#### Scenario: Taint makes the outcome ERROR, not ABORTED

- **WHEN** the kill-switch fired from an operator halt but one host's cleanup then failed and
  tainted the host
- **THEN** the outcome SHALL be `ERROR` naming that host and instance, not `ABORTED`

#### Scenario: Clean full run completes

- **WHEN** every instance of an experiment reaches `DONE`
- **THEN** the outcome SHALL be `COMPLETED`

### Requirement: The management plane gains experiment and taint endpoints

The management API SHALL expose: `POST /experiments` (validate; on failure `422` naming every
failing check; on success `201` with the full record including minted ids); `POST
/experiments/{id}/halt` (`202` accepted, `404` unknown, `409` already terminal); `GET
/experiments` (summaries); `GET /experiments/{id}` (full record: per-instance states, reasons,
timestamps, experiment state, outcome and its cause; `404` unknown); and `POST
/agents/{hostname}/clear-taint`. These endpoints SHALL remain unauthenticated during WIP
(ADR-0002 known gap) and the deployment documentation SHALL state they must not be exposed
beyond a trusted network.

#### Scenario: Invalid experiment returns 422 with all reasons

- **WHEN** a client POSTs an experiment failing two validation checks
- **THEN** the master SHALL respond `422` with both checks named and SHALL create no record

#### Scenario: Live experiment is observable

- **WHEN** a client GETs `/experiments/{id}` for a running experiment
- **THEN** the response SHALL include each instance's current lifecycle state and the experiment
  state (`RUNNING` or `HALTING`)

### Requirement: Clear-taint relays the operator's command to the agent

`POST /agents/{hostname}/clear-taint` SHALL respond `404` when the hostname is not registered
and `409` when it has no live session. Otherwise the master SHALL send `ClearTaint` on that
host's session and respond `202`; the registry's taint flag SHALL be updated only by the agent's
subsequent `TaintStatus` frame, never assumed by the master.

#### Scenario: Clear-taint round-trip

- **WHEN** an operator clears the taint of a connected, tainted host
- **THEN** the master SHALL send `ClearTaint`, respond `202`, and after the agent's
  `TaintStatus { tainted: false }` arrives the host SHALL be shown untainted and be targetable
  again

#### Scenario: Disconnected host cannot be cleared

- **WHEN** an operator clears the taint of a registered host with no live session
- **THEN** the master SHALL respond `409` and send nothing

### Requirement: Experiment records are in-memory only and restart loses them

Experiment records SHALL be held in memory (ADR-0002 §17). After a master restart the master
SHALL report no experiments, SHALL log-and-drop frames referencing unknown instances, and SHALL
NOT assert any outcome for experiments it no longer knows. Host safety during and after the
restart SHALL rest on the agent-side runtime (self-abort, journal replay), not on the master.

#### Scenario: Master restarts mid-experiment

- **WHEN** the master restarts while an experiment is running
- **THEN** `GET /experiments` SHALL return an empty list, reconnecting agents' frames SHALL be
  logged without error, and no outcome SHALL ever be reported for the lost experiment
