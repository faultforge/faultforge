# Proposal: implement-fault-schema

## Why

FaultForge's fault model exists only as documentation — ADR-0002 and the `fault-plugin-model` /
`experiment-model` specs are doc-only, with zero code behind them. Everything that comes next
(agent fault runtime, master dispatch, e2e) hangs off one shared contract: the plugin manifest
format, the agent↔plugin invocation protocol, the integrity digest, and the wire frames for fault
control and telemetry. This change turns that contract into code and **proves it with the first
plugin, `noop-marker`** — a zero-blast-radius fault whose only effect is a marker file's
existence, so a failing run is unambiguously a framework bug, never a `tc`/`iptables`/systemd
problem.

## What Changes

- **New shared crate `faultforge-fault`** (`crates/fault`): plugin manifest parsing and
  validation, `params_schema` validation, invocation-protocol types (stdin JSON input, NDJSON
  output events, the exit-code table), the instance lifecycle state enum, sha256 plugin digest
  computation, and the on-disk catalog layout.
- **Fault wire schema in `faultforge.proto`**: `RunFault` / `AbortFault` (master→agent) and
  `FaultEvent` / `InstanceStatus` / `InstanceReport` / `TaintStatus` (agent→master), plus an
  `InstanceState` enum. **Wire schema only** — master and agent session code gains benign
  ignore-arms for the new frames; neither plane changes behaviour in this change.
- **New crate `crates/plugins/noop-marker`**: the reference plugin binary plus its
  `manifest.yaml`, implementing `preflight` / `inject` / `report` / `abort` / `cleanup` over a
  marker file, with golden tests that drive the built binary end to end (stdin → NDJSON + exit
  code + filesystem effects).
- **Pins contract details ADR-0002 left open**: the digest's exact byte definition
  (`sha256(manifest_bytes ‖ executable_bytes)`, reproducible as
  `cat manifest.yaml <entrypoint> | sha256sum`), which lifecycle states a plugin may emit versus
  agent-owned ones, params carried as JSON, and `grace_secs` carried in `RunFault` so the master
  decides grace the same way it decides the heartbeat interval.

Explicitly **not** in this change (see roadmap): the agent does not execute plugins yet, the
master does not dispatch faults, no journal/watchdog/timer behaviour, no e2e harness.

## Capabilities

### New Capabilities

- `noop-marker-plugin`: behaviour of the reference plugin — per-command semantics for
  `preflight`/`inject`/`report`/`abort`/`cleanup`, the marker file format, idempotent cleanup,
  and the no-harm guarantee (no root, no network, no process signalling).

### Modified Capabilities

- `fault-plugin-model`: gains the concrete invocation contract (one process invocation per
  lifecycle transition, stdin JSON in, NDJSON events out, fixed exit-code meanings), the digest
  byte definition, the on-disk catalog layout, and the fault control/telemetry wire schema.

## Impact

- **Workspace**: two new members (`crates/fault`, `crates/plugins/noop-marker`); new workspace
  deps `sha2`, `hex`, `serde_norway` (maintained `serde_yaml` fork, per design D5), `humantime`,
  and `tempfile` (a runtime dep of `noop-marker`, also used by tests).
- **`crates/proto`**: `faultforge.proto` extended with new oneof variants and messages —
  backwards-compatible field additions; regenerated code.
- **`crates/master`, `crates/agent`**: exhaustive matches over the `Session` oneofs gain
  log-and-ignore arms so the workspace compiles; no behavioural change.
- **Docs**: CLAUDE.md crate table gains the two new crates.
- **No runtime impact**: existing register/heartbeat behaviour is untouched.

## Follow-up changes (roadmap)

Agreed decomposition (owner decisions: *experiment-lite* trigger; *rootless podman containers*
for e2e). Each is proposed as its own change once the previous one is applied, so its design
reflects what implementation taught us:

1. **`implement-fault-schema`** (this change) — contract + reference plugin.
2. **`agent-fault-runtime`** — the agent executes the lifecycle: instance supervisor with a pure
   state machine, plugin process runner (spawn, stdin feed, NDJSON parse, stderr capture,
   exit-code mapping), digest verification before every invocation (including recovery-time
   cleanup), two-phase preflight, instance journal under `/var/lib/faultforge`, the single
   safety timer (duration stop / master-loss self-abort / dead-man at `duration + grace`),
   watchdog-owned recovery incl. re-attach after agent restart, TAINTED persisted host-side and
   reported. Tested against an in-process fake master; no podman needed.
3. **`master-fault-dispatch`** — *experiment-lite*: an experiment record shaped per the
   `experiment-model` spec (actions, durations, single salvo) but with no metric
   rules/connectors and a reduced outcome lattice (ran-cleanly / `ABORTED` / `ERROR`);
   targeting by explicit hostname (the registry has no tags yet); master-minted deterministic
   `instance_id`s; `RunFault`/halt over `Session`; the management plane's **first write
   endpoints** (run / halt / clear-taint — still unauthenticated, per ADR-0002 known gaps);
   plugin-digest catalog file on the master; operator CLI commands.
4. **`fault-e2e-harness`** — fully automated e2e on rootless podman containers driven from Rust:
   master + agent containers with the baked-in catalog, journal on a volume; scenarios: happy
   path, operator halt, agent restart mid-`ACTIVE` (journal re-attach), master-loss self-abort,
   dead-man backstop, TAINTED via `chmod 000` + operator clear. Same harness locally (macOS via
   `podman machine`) and in Linux CI.
