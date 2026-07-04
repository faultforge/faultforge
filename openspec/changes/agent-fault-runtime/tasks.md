# Tasks: agent-fault-runtime

## 1. Contract groundwork (proto + fault crate)

- [x] 1.1 Add `plugin_digest` to `InstanceStatus` in `faultforge.proto` (fresh field number;
      `InstanceReport` inherits via `repeated InstanceStatus`); rebuild and fix nothing else —
      master/agent arms only log
- [x] 1.2 Add the `proto` feature to `faultforge-fault`: `From`/`TryFrom` between
      `state::InstanceState` and the wire enum, feature-gated so `noop-marker` still builds
      without tonic; unit tests for the round-trip including `UNSPECIFIED`/unknown
- [x] 1.3 `InstanceId` newtype already exists in `faultforge-fault::protocol` (added by
      implement-fault-schema) and is used throughout the protocol types; empty-id rejection
      happens at the agent's RunFault boundary instead of in the type (changing the type to
      validating serde would break the plugin-side transparent parse)

## 2. Agent config and scaffolding

- [x] 2.1 Extend `AgentConfig` with `plugin_root` (default `/usr/lib/faultforge/plugins`),
      `data_dir` (default `/var/lib/faultforge`), `master_loss_threshold_secs` (default 30),
      wired through the existing layered sources; config tests
- [x] 2.2 Create module skeletons `machine.rs`, `runner.rs`, `journal.rs`, `taint.rs`,
      `supervisor.rs`, `catalog.rs` in `crates/agent/src` with doc-comments stating each
      module's core/shell role

## 3. Functional core: the instance state machine

- [x] 3.1 Define `Event`, `Effect`, and machine state types per design D2; implement
      `step(state, event, now) -> (state, Vec<Effect>)` covering the full lifecycle:
      preflight/inject/active/recovering, abort path, exit-code mappings (10/20/30/other),
      cleanup retry-once-then-taint, duration stop, master-loss self-abort, dead-man `ERROR`
- [x] 3.2 Table-driven unit tests for every transition and ordering rule (agent-owned states
      never enterable from plugin events; duplicate events harmless; terminal states absorb)
- [x] 3.3 Pure deadline function: earliest of duration end / master-loss / dead-man with cause
      tagging (design D4); unit tests including reconnect re-arming

## 4. Imperative shells: runner, journal, taint, catalog

- [x] 4.1 `runner.rs`: sync invocation of `<entrypoint> <command>` — stdin JSON + EOF, NDJSON
      stdout parse (well-formed vs malformed split), stderr capture, hard 60s timeout with
      kill, exit classification via `faultforge_fault::protocol`; tests with script fixtures
- [x] 4.2 `catalog.rs`: resolve `(name, version)` under `plugin_root` via
      `faultforge_fault::catalog`, digest verification against an expected digest, and
      install-phase preflight at first resolution; tests incl. digest-mismatch and missing
      plugin
- [x] 4.3 `journal.rs`: versioned per-instance JSON files under `<data_dir>/instances/`,
      atomic write (temp + rename), list/read/remove, unknown-version rejection; tests
- [x] 4.4 `taint.rs`: atomic write of `tainted.json`, presence check, read of reason; never
      deleted by agent code; tests

## 5. Supervisor and session rework

- [x] 5.1 `supervisor.rs`: actor owning instance handles; spawn instance task on `RunFault`
      (unknown id), drop duplicates, route `AbortFault`; instance task drives `machine::step`,
      executes effects (runner via `spawn_blocking`, journal, taint, status/event emission into
      the shared outbound channel), `select!`s abort signal + single armed timer
- [x] 5.2 Rework `session.rs` to full-duplex: writer task draining one shared mpsc; reader
      dispatching acks vs fault frames vs unknown (log-and-continue preserved); heartbeat loop
      unchanged in cadence semantics
- [x] 5.3 Reconnect loop with exponential backoff (1s→30s cap, retry forever) around
      connect/register/pump; publish `watch<ConnState>` transitions consumed by instance timers
- [x] 5.4 Post-register sequence: `InstanceReport` of live instances (empty allowed) +
      `TaintStatus` (both values), then queued replay statuses; taint refusal of `RunFault`
- [x] 5.5 Startup replay before first connect: list journal, digest-gate, `abort`+`cleanup`,
      `ABORTED`/`ERROR` by `deadline_unix`, queue statuses, remove entries; taint on replay
      failure

## 6. Integration tests (fake master + fixture plugins)

- [x] 6.1 Test harness: in-process tonic `AgentService` fake master on `127.0.0.1:0` —
      scriptable frames out, recorded frames in, drop-stream-on-cue; helper to build temp
      catalogs from `#!/bin/sh` fixture scripts with computed digests
- [x] 6.2 Happy path: RunFault → statuses PREFLIGHT…DONE, FaultEvents forwarded verbatim,
      journal created then removed
- [x] 6.3 Failure mappings: exit 10 (no inject, ABORTED+reason), exit 20 (ABORTED), exit 30
      twice (ERROR + taint file + TaintStatus), malformed NDJSON (success + error logs), hung
      plugin (timeout kill → unexpected-failure handling)
- [x] 6.4 Control: AbortFault mid-ACTIVE → abort+cleanup → ABORTED; duplicate RunFault dropped;
      RunFault while tainted → ABORTED without invocation; unknown plugin / bad digest /
      duration > max_duration_secs / bad params → ABORTED pre-inject
- [x] 6.5 Resilience: stream drop + reconnect < threshold (no self-abort, fresh register +
      truthful InstanceReport); sustained loss > threshold (self-abort ABORTED); dead-man with
      short duration+grace (forced recovery, ERROR)
- [x] 6.6 Restart replay: kill agent mid-ACTIVE, restart with same `data_dir` → abort+cleanup,
      ABORTED status after register, journal removed; variant with past `deadline_unix` → ERROR

## 7. Verification and docs

- [x] 7.1 `cargo build --workspace && cargo test --workspace && cargo clippy --workspace
      --all-targets -- -D warnings && cargo fmt --check`; confirm master tests pass untouched
      and `noop-marker` still builds without tonic in its tree (`cargo tree -p noop-marker`)
- [x] 7.2 Manual smoke: master + agent with a temp `data_dir`/`plugin_root` holding
      `noop-marker`; verify register, report, and log-only master handling of new frames
- [x] 7.3 Update CLAUDE.md (agent status/role row, new config keys, "contract-only" note removed,
      gotchas: reconnect replaces exit-on-failure, supervisor deployment prerequisite) and the
      README if it states agent behaviour
