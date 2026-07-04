# Delta: Fault Plugin Model — concrete invocation contract and wire schema

## ADDED Requirements

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
