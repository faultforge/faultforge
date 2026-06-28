# Spec: Fault Plugin Model

## Purpose

Defines how a fault is described, distributed to a host, and run there by the agent with a safe,
recoverable lifecycle (v1 scope). This is the agent/plugin half of the fault system and is
independent of the master-side experiment model. Governing decisions:
[ADR-0002](../../../../docs/adr/0002-fault-model-decisions.md).

## ADDED Requirements

### Requirement: A plugin is described by a machine-readable manifest
A plugin SHALL consist of a manifest plus an executable entrypoint. The manifest SHALL declare the
plugin identity (`name`, `version`), the `entrypoint`, a `params_schema`, a `requires` block
(required host binaries, privileges, minimum resources, and any per-fault preconditions), and
`max_duration_secs`. The manifest SHALL be sufficient to audit a plugin without executing it. The
integrity digest SHALL NOT be stored in the manifest body.

#### Scenario: Manifest declares requirements that are auditable before execution
- **WHEN** an operator inspects a plugin manifest
- **THEN** the required host binaries, privileges, resources, and per-fault preconditions SHALL be
  visible without running the plugin

#### Scenario: Parameters are validated against the schema before dispatch
- **WHEN** an experiment supplies parameters for a plugin
- **THEN** the parameters SHALL be validated against the manifest `params_schema` and rejected
  before injection if they do not conform

### Requirement: Plugin integrity is a digest over manifest and executable as a unit
A plugin's integrity SHALL be a `sha256` digest computed over the manifest and executable together,
referenced externally (in the experiment definition and the catalog). Before running a plugin, the
agent SHALL verify the on-disk plugin's digest against the referenced value and SHALL reject and not
execute a plugin whose digest does not match. Digest verification provides integrity only; plugin
safety SHALL rest on the vetted first-party catalog, not on the digest.

#### Scenario: On-disk plugin fails digest verification
- **WHEN** the agent is about to run a plugin whose computed digest does not match the referenced
  digest
- **THEN** the agent SHALL fail preflight and SHALL NOT execute the plugin

### Requirement: v1 distributes plugins as a baked-in catalog; dynamic pull is deferred
In v1 the vetted plugin catalog SHALL be delivered as part of the agent package; the agent SHALL
resolve assigned plugins from that catalog. The master SHALL NOT be required to run an artifact
store in v1. The content-addressed `FetchArtifact` RPC shape SHALL be reserved in the contract for
a later version but SHALL NOT be relied upon for v1 operation.

#### Scenario: Agent resolves an assigned plugin from the baked-in catalog
- **WHEN** an agent is assigned a plugin present in its baked-in catalog
- **THEN** the agent SHALL use the catalog plugin after verifying its digest, without contacting
  any artifact store

### Requirement: Plugins implement a fixed lifecycle with two preflights and per-fault preconditions
A plugin SHALL implement a lifecycle of `preflight`, `inject`, `report`, `abort`, and `cleanup`.
`preflight` SHALL be checked statically at install and again at runtime immediately before
injection, including any per-fault preconditions declared in `requires`. A fault SHALL NOT be
injected on a host that fails runtime `preflight`. `abort` and `cleanup` SHALL be distinct
operations, and `cleanup` SHALL be idempotent.

#### Scenario: Per-fault precondition fails at runtime preflight
- **WHEN** a `kill-process` plugin's runtime preflight finds the target is not a restartable service
  unit
- **THEN** the agent SHALL NOT inject the fault and SHALL report the precondition failure

#### Scenario: Cleanup is re-issued after a failure
- **WHEN** `cleanup` is invoked more than once for the same fault instance
- **THEN** the result SHALL be equivalent to invoking it once, with no additional side effects

### Requirement: The fault instance is the unit of execution and the agent supervises many
A fault instance SHALL be one run of one plugin with one parameter set, identified by an
`instance_id` minted by the master and carried in the `inject` command. An agent SHALL supervise
multiple concurrent fault instances; a host matching multiple actions SHALL run one instance per
action concurrently. v1 SHALL NOT implement conflict detection between concurrent instances on a
host. All reports and state for an instance SHALL be tagged with its `instance_id`.

#### Scenario: A host matches multiple target groups
- **WHEN** an agent is assigned more than one action because its host matches multiple target groups
- **THEN** the agent SHALL run one fault instance per action concurrently, each with its own
  lifecycle and master-minted `instance_id`

### Requirement: A single agent-side safety timer governs duration, silence, and the dead-man backstop
The experiment `duration` SHALL be authoritative for how long a fault is active, ending normally
with a graceful stop followed by recovery. The agent SHALL self-abort an active instance if it
loses the master for longer than a configured threshold. The agent SHALL arm a dead-man deadline at
`duration + grace` that, if reached before any other stop, SHALL force recovery and mark the
instance `ERROR`. The system SHALL enforce `duration <= max_duration_secs`.

#### Scenario: Master is lost during an active fault
- **WHEN** an agent loses contact with the master for longer than the configured threshold while a
  fault instance is active
- **THEN** the agent SHALL self-abort that instance and recover the host

#### Scenario: Dead-man deadline reached
- **WHEN** the dead-man deadline (`duration + grace`) is reached before any graceful stop or
  self-abort
- **THEN** the agent SHALL force recovery and mark the instance `ERROR`

#### Scenario: Requested duration exceeds the plugin's safe limit
- **WHEN** an experiment requests a `duration` greater than the plugin's `max_duration_secs`
- **THEN** the experiment SHALL be rejected before injection

### Requirement: Recovery is concrete per fault and survives agent death
Recovery SHALL be specified per fault and SHALL NOT depend on the plugin process or the agent
process staying alive. Reverts for stateful host changes (e.g. `tc`, `iptables`) SHALL be owned by a
separate watchdog process or scheduled job. `kill-process` recovery SHALL rely on the service
supervisor restarting the target. The agent SHALL persist an instance journal (`instance_id`,
plugin digest, params, deadline) so that a restarted agent re-attaches the watchdog and completes
recovery.

#### Scenario: Agent restarts while a fault is active
- **WHEN** the agent process restarts while a fault instance is active
- **THEN** the restarted agent SHALL read its instance journal, re-attach the watchdog, and complete
  recovery of the affected host

#### Scenario: A network fault must revert without the plugin process
- **WHEN** a `tc`/`iptables` fault is active and its plugin process exits
- **THEN** the configured host change SHALL still be reverted by the watchdog at the deadline

### Requirement: Recovery failure quarantines the host
When recovery fails even after an idempotent retry, the agent's host SHALL be marked `TAINTED`,
SHALL be excluded from new experiments, and an operator SHALL be alerted. The taint SHALL be cleared
only by an operator.

#### Scenario: Recovery cannot restore the host
- **WHEN** recovery fails after retry for a fault instance
- **THEN** the host SHALL be marked `TAINTED` and excluded from being targeted by new experiments
  until an operator clears it

### Requirement: Inject is never retried; reconciliation is by state report
The master SHALL NOT re-issue `inject` for a fault instance. After a transient stream interruption,
the agent SHALL report the current state of its instances and the master SHALL accept the reported
state rather than re-injecting. If an instance's state is ambiguous, the agent SHALL abort it.

#### Scenario: Brief stream interruption during an active fault
- **WHEN** the `Session` stream briefly drops and reconnects while a fault instance is active
- **THEN** the agent SHALL report the instance's current state and the master SHALL accept it
  without issuing a second `inject`

#### Scenario: Instance state is ambiguous after interruption
- **WHEN** the agent cannot determine an instance's state after an interruption
- **THEN** the agent SHALL abort that instance

### Requirement: Plugins report status and logs through the agent
A plugin SHALL emit structured status transitions and log output during its run. The agent SHALL
forward this telemetry to the master, tagged with the originating `instance_id`.

#### Scenario: A plugin produces logs while active
- **WHEN** a running plugin emits log lines or a status transition
- **THEN** the agent SHALL forward them to the master tagged with the instance's `instance_id`
