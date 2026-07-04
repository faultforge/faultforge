# Spec: Fault Plugin Model

## Purpose

Defines how a fault is described, distributed to a host, and run there by the agent with a safe,
recoverable lifecycle (v1 scope). This is the agent/plugin half of the fault system and is
independent of the master-side experiment model. Includes the concrete invocation contract and
wire schema for plugin communication. Governing decisions:
[ADR-0002](../../../docs/adr/0002-fault-model-decisions.md).

## Requirements

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

### Requirement: Plugins are invoked as one process per lifecycle transition

The agent SHALL invoke a plugin as `<entrypoint> <command>` with `command` one of `preflight`,
`inject`, `report`, `abort`, `cleanup` — one short-lived process per lifecycle transition. A
plugin SHALL exit after completing its command; a fault's active state SHALL NOT require a
long-running plugin process. Timing SHALL be owned by the agent and its watchdog, never by the
plugin.

#### Scenario: No long-running plugin process

- **WHEN** a plugin's `inject` command completes successfully
- **THEN** the plugin process SHALL have exited, and the fault SHALL be considered `ACTIVE` based
  on its host effect and the agent's supervision, not on any surviving plugin process

### Requirement: Plugin input is a single JSON object on stdin

Every invocation SHALL receive on stdin one JSON object with exactly: `instance_id` (string),
`params` (object conforming to the manifest `params_schema`), `deadline_unix` (integer — the
absolute dead-man deadline `start + duration + grace` computed by the agent), and `phase`
(`"install"` or `"runtime"`). `phase` SHALL be meaningful only for `preflight`; all other
commands SHALL receive `"runtime"`. The plugin MAY record `deadline_unix` but SHALL NOT schedule
its own actions against it. The agent SHALL write the JSON object and then close the plugin's
stdin (send EOF); a plugin MAY rely on reaching EOF to know its input is complete.

#### Scenario: Preflight runs in both phases

- **WHEN** the agent performs the install-time check and later the runtime check immediately
  before injection
- **THEN** `preflight` SHALL be invoked once with `phase: "install"` and once with
  `phase: "runtime"`, with otherwise identical input shape

#### Scenario: Non-preflight commands receive runtime phase

- **WHEN** the agent invokes `inject`, `report`, `abort`, or `cleanup`
- **THEN** the stdin object SHALL carry `phase: "runtime"`

### Requirement: Plugin output is NDJSON on stdout; stderr is captured but never decides success

A plugin SHALL emit newline-delimited JSON on stdout, one object per line, each carrying `ts`
(RFC3339 UTC), `instance_id`, and `type` of `status` (with `state`) or `log` (with `level` and
`msg`). Every line SHALL carry the `instance_id` so the agent can tag and forward telemetry. The
agent SHALL forward plugin lines verbatim (content unmodified) to the master. stderr output SHALL
be captured as `log` lines at `level: "error"` and SHALL NOT by itself determine success — the
exit code SHALL.

#### Scenario: Status and log lines are forwarded with the instance id

- **WHEN** a plugin emits a `status` transition and a `log` line during a command
- **THEN** each line SHALL be parseable as JSON, SHALL carry `ts`, `instance_id`, and `type`, and
  SHALL be forwarded to the master tagged with that `instance_id`

#### Scenario: stderr noise with exit 0 is still success

- **WHEN** a plugin writes to stderr but exits with code `0`
- **THEN** the command SHALL be treated as successful and the stderr content SHALL appear only as
  `level: "error"` log telemetry

### Requirement: Exit codes have fixed contract meanings

Plugin exit codes SHALL mean: `0` success; `10` preflight precondition failed — the agent SHALL
NOT inject and SHALL report the precondition failure; `20` inject failed — the instance SHALL
transition to `ABORTED` with the host unaffected; `30` cleanup/abort failed — the agent SHALL
retry the idempotent operation once and, if it fails again, mark the host `TAINTED`; any other
non-zero code SHALL be treated as an unexpected failure of the current command. The exit-code
table SHALL be defined once in the shared contract crate and used by plugins and the agent alike.

#### Scenario: Preflight precondition failure prevents injection

- **WHEN** any plugin command `preflight` exits with code `10`
- **THEN** the agent SHALL NOT invoke `inject` for that instance and SHALL report a precondition
  failure

#### Scenario: Repeated cleanup failure escalates

- **WHEN** `cleanup` exits `30` and the single retry also exits `30`
- **THEN** the host SHALL be marked `TAINTED` per the recovery-failure requirement

### Requirement: Lifecycle states separate plugin-emittable from agent-owned

The instance lifecycle states SHALL be `PENDING`, `PREFLIGHT`, `INJECTING`, `ACTIVE`,
`RECOVERING`, `DONE`, `ABORTED`, `ERROR`. A plugin MAY emit only `PREFLIGHT`, `INJECTING`,
`ACTIVE`, `RECOVERING`, `DONE` in its `status` lines; `PENDING`, `ABORTED`, and `ERROR` SHALL be
assigned solely by the agent's own state machine. `TAINTED` SHALL be a host-level quarantine
state, not an instance state. The master SHALL track instance lifecycle only from
agent-authoritative transitions, never directly from plugin-emitted lines.

#### Scenario: Plugin cannot assert an agent-owned state

- **WHEN** a plugin's stdout contains a `status` line with state `ABORTED`, `ERROR`, or `PENDING`
- **THEN** that line SHALL be treated as telemetry only (forwarded and logged) and SHALL NOT
  drive the instance's lifecycle state

### Requirement: The plugin digest is sha256 over manifest bytes then executable bytes

The integrity digest SHALL be `sha256(manifest_bytes ‖ executable_bytes)` — the exact bytes of
`manifest.yaml` immediately followed by the exact bytes of the entrypoint executable, no
separators — encoded as 64 lowercase hex characters. The computation SHALL be reproducible by an
operator with `cat manifest.yaml <entrypoint> | sha256sum`.

#### Scenario: Digest is reproducible with coreutils

- **WHEN** an operator concatenates a plugin's manifest and entrypoint files and hashes them with
  sha256
- **THEN** the result SHALL equal the digest computed by the shared contract crate for that
  plugin

#### Scenario: Any byte change invalidates the digest

- **WHEN** any byte of either the manifest or the executable changes
- **THEN** the computed digest SHALL differ from the previously referenced digest

### Requirement: The params schema is a typed, closed map of required parameters

`params_schema` SHALL be a map of parameter name to type, with v1 types `string`, `int`, and
`bool`. Every declared parameter SHALL be required (no defaults), and parameters not declared in
the schema SHALL be rejected. Validation SHALL be a pure function in the shared contract crate,
usable identically by the master before dispatch and by the agent before execution.

#### Scenario: Missing required parameter is rejected

- **WHEN** supplied params omit a parameter declared in `params_schema`
- **THEN** validation SHALL fail naming the missing parameter

#### Scenario: Undeclared parameter is rejected

- **WHEN** supplied params contain a key not present in `params_schema`
- **THEN** validation SHALL fail naming the undeclared parameter

#### Scenario: Type mismatch is rejected

- **WHEN** a parameter declared `string` is supplied as a JSON number
- **THEN** validation SHALL fail naming the parameter and expected type

### Requirement: Plugins live in a version-qualified on-disk catalog layout

An installed plugin SHALL live at `<plugin_root>/<name>@<version>/` containing `manifest.yaml`
and the entrypoint at the manifest-declared path relative to that directory. Loading a plugin
SHALL parse and validate the manifest and compute the digest from the manifest and entrypoint
bytes. Multiple versions of a plugin SHALL be installable side by side.

#### Scenario: Loading a catalog entry yields manifest and digest

- **WHEN** a plugin directory containing a valid `manifest.yaml` and entrypoint is loaded
- **THEN** the loader SHALL return the parsed manifest and the computed digest for verification
  against the externally referenced value

### Requirement: Fault control and telemetry are frames on the Session stream

The wire contract SHALL define, on the existing `Session` stream: master→agent `RunFault`
(`instance_id`, `plugin_name`, `plugin_version`, `plugin_digest`, `params_json`, `duration_secs`,
`grace_secs`) and `AbortFault` (`instance_id`); agent→master `FaultEvent` (`instance_id` plus the
verbatim plugin NDJSON line), `InstanceStatus` (agent-authoritative transition: state, timestamp,
optional reason), `InstanceReport` (reconciliation snapshot of instance statuses), and
`TaintStatus` (host quarantine state and reason). `grace_secs` SHALL be decided by the master and
carried in `RunFault`. An endpoint receiving a fault frame it does not handle SHALL log it and
continue the session rather than terminating.

#### Scenario: RunFault carries everything the agent needs

- **WHEN** the master issues a `RunFault`
- **THEN** the frame SHALL identify the instance, the plugin (name, version, digest), the
  JSON-encoded params, the duration, and the grace used for the dead-man deadline

#### Scenario: Unhandled fault frames do not kill the session

- **WHEN** a master or agent that does not yet implement fault behaviour receives a fault frame
- **THEN** it SHALL log the frame and keep the `Session` stream open
