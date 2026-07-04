# Spec Delta: Agent Fault Runtime

## ADDED Requirements

### Requirement: The agent executes RunFault through a supervised lifecycle

On receiving `RunFault` for an unknown `instance_id`, the agent SHALL create a fault instance
and drive it through runtime `preflight`, `inject`, the active window, and recovery
(`cleanup`, plus `abort` when stopping early), invoking the plugin one process per transition
per the fault-plugin-model contract. The agent SHALL emit an agent-authoritative
`InstanceStatus` frame for every state transition of its own state machine. When the active
window ends at `duration` without abort or error, the agent SHALL run `cleanup` and mark the
instance `DONE`.

#### Scenario: Happy path ends DONE

- **WHEN** a `RunFault` for a catalog plugin passes runtime preflight, injects successfully, and
  reaches `duration` with no abort, error, or master loss
- **THEN** the agent SHALL invoke `cleanup`, mark the instance `DONE`, and have emitted
  `InstanceStatus` transitions covering `PREFLIGHT`, `INJECTING`, `ACTIVE`, `RECOVERING`, and
  `DONE`

#### Scenario: AbortFault stops the instance early

- **WHEN** the agent receives `AbortFault` for an instance in `ACTIVE`
- **THEN** the agent SHALL invoke `abort` and then `cleanup` and mark the instance `ABORTED`

### Requirement: RunFault for a known instance id is never re-executed

The agent SHALL treat a `RunFault` whose `instance_id` matches an existing instance (live or
already terminal within the agent's knowledge) as a duplicate: it SHALL log and drop the frame
and SHALL NOT invoke any plugin command for it.

#### Scenario: Duplicate RunFault is dropped

- **WHEN** the agent receives a second `RunFault` carrying an `instance_id` it already
  supervises
- **THEN** the agent SHALL NOT re-run preflight or inject, and the existing instance SHALL
  proceed unaffected

### Requirement: Every plugin invocation is digest-gated

Before every plugin process invocation — including `abort`/`cleanup` during restart replay —
the agent SHALL recompute the on-disk plugin digest and compare it to the digest referenced for
the instance (`RunFault.plugin_digest` or the journaled digest). On mismatch the agent SHALL
NOT execute the plugin; for a new instance it SHALL mark the instance `ABORTED` with a reason
naming the digest mismatch, and during replay it SHALL mark the host `TAINTED` (recovery cannot
be performed safely).

#### Scenario: Digest mismatch on a new RunFault

- **WHEN** the computed digest of the catalog plugin differs from `RunFault.plugin_digest`
- **THEN** the agent SHALL NOT invoke the plugin and SHALL emit `InstanceStatus` `ABORTED` with
  a digest-mismatch reason

#### Scenario: Digest mismatch during replay recovery

- **WHEN** a restarted agent finds a journaled instance whose on-disk plugin no longer matches
  the journaled digest
- **THEN** the agent SHALL NOT invoke the plugin and SHALL mark the host `TAINTED`

### Requirement: Runtime preflight failures prevent injection and abort the instance

The agent SHALL mark an instance `ABORTED` with the host unaffected, without invoking `inject`,
when: runtime `preflight` exits `10`; the requested plugin (name, version) is absent from the
catalog; `duration_secs` exceeds the manifest `max_duration_secs`; or the supplied params fail
`params_schema` validation. The emitted `InstanceStatus` reason SHALL name the failing check.

#### Scenario: Plugin missing from the catalog

- **WHEN** `RunFault` names a plugin not present under the agent's `plugin_root`
- **THEN** the agent SHALL emit `InstanceStatus` `ABORTED` with a reason naming the missing
  plugin and SHALL NOT invoke any plugin command

#### Scenario: Duration exceeds the manifest limit

- **WHEN** `RunFault.duration_secs` is greater than the catalog manifest's `max_duration_secs`
- **THEN** the agent SHALL reject the instance as `ABORTED` before runtime preflight is invoked

### Requirement: The journal is written before inject and removed at terminal state

The agent SHALL persist a per-instance journal entry under `<data_dir>/instances/` after
runtime preflight succeeds and before `inject` is invoked, containing at least the
`instance_id`, plugin name and version, plugin digest, params, start time, `duration_secs`,
`grace_secs`, the absolute `deadline_unix`, and a journal format version. The write SHALL be
atomic (temp file + rename). The entry SHALL be removed only after the instance's terminal
`InstanceStatus` has been queued for sending.

#### Scenario: Journal exists while the fault is active

- **WHEN** an instance is in `ACTIVE`
- **THEN** `<data_dir>/instances/<instance_id>.json` SHALL exist and contain the journaled
  fields including the plugin digest and `deadline_unix`

#### Scenario: Journal is gone after DONE

- **WHEN** an instance reaches `DONE`
- **THEN** its journal entry SHALL be removed

### Requirement: A restarted agent replays the journal and recovers before serving

On startup, before establishing a session, the agent SHALL read every journal entry and recover
each journaled instance by invoking `abort` then `cleanup` (digest-gated). The recovered
instance SHALL be marked `ABORTED` if recovery completes before its `deadline_unix`, or `ERROR`
if the deadline has already passed. The resulting terminal `InstanceStatus` frames SHALL be
sent to the master after the first registration, after the `InstanceReport`.

#### Scenario: Agent restarts mid-ACTIVE

- **WHEN** the agent process is killed while an instance is `ACTIVE` and the agent is restarted
  with the same `data_dir` before `deadline_unix`
- **THEN** the restarted agent SHALL invoke `abort` and `cleanup` for the journaled instance,
  mark it `ABORTED`, send its terminal `InstanceStatus` after registering, and remove the
  journal entry

#### Scenario: Replay after the dead-man deadline

- **WHEN** a restarted agent finds a journaled instance whose `deadline_unix` is already past
- **THEN** the agent SHALL still run recovery and SHALL mark the instance `ERROR`

### Requirement: One safety deadline per instance, from three causes

For each live instance the agent SHALL maintain a single next-deadline computed as the earliest
of: the graceful stop at `start + duration`; the master-loss self-abort at
`last_master_contact + master_loss_threshold_secs` (only while the master is unreachable); and
the dead-man at `deadline_unix = start + duration + grace`. The cause that fires SHALL
determine the outcome: graceful stop → `cleanup` → `DONE`; master loss → `abort` + `cleanup` →
`ABORTED`; dead-man → forced recovery → `ERROR`. `master_loss_threshold_secs` SHALL be agent
configuration with a default, not a master directive.

#### Scenario: Master loss during ACTIVE self-aborts

- **WHEN** the session is disconnected for longer than `master_loss_threshold_secs` while an
  instance is `ACTIVE`
- **THEN** the agent SHALL abort and clean up the instance, marking it `ABORTED`, without any
  master involvement

#### Scenario: Reconnection before the threshold cancels self-abort

- **WHEN** the session drops and is re-established in less than `master_loss_threshold_secs`
- **THEN** no instance SHALL be self-aborted because of the interruption

### Requirement: Failed recovery taints the host persistently

When `abort` or `cleanup` exits `30` and its single idempotent retry also fails, or replay
recovery fails, the agent SHALL write a taint record under `data_dir` (reason, timestamp,
originating `instance_id`), mark the instance `ERROR`, and report `TaintStatus` to the master.
The taint record SHALL survive agent restart. The agent SHALL NOT clear the taint itself.

#### Scenario: Cleanup fails twice

- **WHEN** `cleanup` exits `30` and the retry exits `30` again
- **THEN** the agent SHALL persist the taint record, emit `InstanceStatus` `ERROR` for the
  instance, and send `TaintStatus { tainted: true }` with the reason

#### Scenario: Taint survives restart

- **WHEN** a tainted agent restarts
- **THEN** it SHALL still consider the host tainted and SHALL report
  `TaintStatus { tainted: true }` after registering

### Requirement: A tainted host refuses new fault instances

While the taint record exists, the agent SHALL answer any `RunFault` with `InstanceStatus`
`ABORTED` (reason naming the taint) without invoking any plugin command.

#### Scenario: RunFault against a tainted host

- **WHEN** the agent holds a taint record and receives `RunFault`
- **THEN** the agent SHALL NOT invoke the plugin and SHALL emit `ABORTED` with a
  host-tainted reason

### Requirement: The agent reports state on every registration

After every `RegisterAck` the agent SHALL send: an `InstanceReport` listing all live
(non-terminal) instances — including their states and plugin digests — sending an empty report
when none are live; and a `TaintStatus` frame carrying the current taint state (tainted or
not).

#### Scenario: Reconnect during an active fault

- **WHEN** the session drops and reconnects while an instance is `ACTIVE`
- **THEN** the agent SHALL send an `InstanceReport` containing that instance's current state
  and digest immediately after re-registering

#### Scenario: Clean agent reports empty

- **WHEN** an agent with no live instances and no taint registers
- **THEN** it SHALL send an empty `InstanceReport` and `TaintStatus { tainted: false }`

### Requirement: Plugin misbehaviour is contained per invocation

The agent SHALL apply a hard timeout to every plugin invocation; on expiry it SHALL kill the
plugin process and treat the command as an unexpected failure of that command. Stdout lines
that are not valid protocol NDJSON SHALL be captured as agent-authored `log` telemetry at
`level: "error"` and SHALL NOT terminate the instance or the agent. Plugin `status` lines SHALL
never drive the agent's state machine (they are forwarded telemetry only).

#### Scenario: Hung plugin is killed

- **WHEN** a plugin command produces no exit within the invocation timeout
- **THEN** the agent SHALL kill the process and handle the command as an unexpected failure per
  the exit-code contract

#### Scenario: Malformed NDJSON is telemetry, not failure

- **WHEN** a plugin writes a non-JSON line to stdout during a successful (exit `0`) command
- **THEN** the command SHALL still be treated as successful and the line SHALL appear only as
  error-level log telemetry

### Requirement: Runtime paths are agent configuration with defaults

The agent SHALL take `plugin_root` (default `/usr/lib/faultforge/plugins`) and `data_dir`
(default `/var/lib/faultforge`) through the standard layered configuration, creating
`<data_dir>/instances/` on demand. Fault execution assumes the agent runs under a process
supervisor that restarts it; this deployment prerequisite SHALL be documented.

#### Scenario: Custom paths via configuration

- **WHEN** the agent is started with `plugin_root` and `data_dir` overridden (flag, env, or
  config file)
- **THEN** catalog resolution SHALL use the configured `plugin_root` and journals/taint SHALL be
  written under the configured `data_dir`
