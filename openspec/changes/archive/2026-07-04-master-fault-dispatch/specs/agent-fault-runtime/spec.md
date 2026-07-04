# Spec Delta: Agent Fault Runtime

## MODIFIED Requirements

### Requirement: Failed recovery taints the host persistently

The agent SHALL, when `abort` or `cleanup` exits `30` and its single idempotent retry also
fails, or when replay recovery fails, write a taint record under `data_dir` (reason, timestamp,
originating `instance_id`), mark the instance `ERROR`, and report `TaintStatus` to the master.
The taint record SHALL survive agent restart. The agent SHALL NOT clear the taint on its own
initiative; it SHALL remove the record only when commanded via `ClearTaint`.

#### Scenario: Cleanup fails twice

- **WHEN** `cleanup` exits `30` and the retry exits `30` again
- **THEN** the agent SHALL persist the taint record, emit `InstanceStatus` `ERROR` for the
  instance, and send `TaintStatus { tainted: true }` with the reason

#### Scenario: Taint survives restart

- **WHEN** a tainted agent restarts
- **THEN** it SHALL still consider the host tainted and SHALL report
  `TaintStatus { tainted: true }` after registering

## ADDED Requirements

### Requirement: ClearTaint removes the taint record and reports the new state

On receiving `ClearTaint`, the agent SHALL remove its taint record and send
`TaintStatus { tainted: false }`. The operation SHALL be idempotent: `ClearTaint` on an
untainted host SHALL change nothing and still answer `TaintStatus { tainted: false }`. If the
record cannot be removed, the agent SHALL remain tainted and report
`TaintStatus { tainted: true }` with a reason naming the failure. After a successful clear the
agent SHALL accept new `RunFault` frames normally.

#### Scenario: Operator clears a tainted host

- **WHEN** a tainted agent receives `ClearTaint`
- **THEN** the agent SHALL remove the taint record, send `TaintStatus { tainted: false }`, and a
  subsequent `RunFault` SHALL be executed normally instead of being refused

#### Scenario: ClearTaint on a clean host is harmless

- **WHEN** an untainted agent receives `ClearTaint`
- **THEN** the agent SHALL change no state and SHALL send `TaintStatus { tainted: false }`

#### Scenario: Removal failure keeps the quarantine

- **WHEN** the taint record cannot be removed (e.g. filesystem error)
- **THEN** the agent SHALL keep refusing `RunFault` and SHALL send
  `TaintStatus { tainted: true }` with a reason naming the removal failure
