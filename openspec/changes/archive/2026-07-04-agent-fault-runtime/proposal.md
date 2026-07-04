# Proposal: agent-fault-runtime

## Why

The fault contract is code (`faultforge-fault`, wire frames, `noop-marker`) but nothing executes
faults: the agent logs `RunFault`/`AbortFault` and moves on. This change is step 2 of the agreed
four-change roadmap — the agent gains the full, safe fault-instance lifecycle mandated by
ADR-0002 ("Trust, See, Stop": never strand a host, stay observable, always stoppable), so that
`master-fault-dispatch` (step 3) has a real runtime to command.

## What Changes

- **Agent handles `RunFault` / `AbortFault`.** An instance supervisor runs many concurrent fault
  instances, each driven by a pure state machine (`PENDING → PREFLIGHT → INJECTING → ACTIVE →
  RECOVERING → DONE/ABORTED/ERROR`) with all IO at the shell.
- **Plugin process runner**: spawn `<entrypoint> <command>` from the on-disk catalog, feed the
  stdin JSON object and close stdin, parse NDJSON stdout, capture stderr as `level: "error"`
  logs, map exit codes via the shared table, forward plugin lines verbatim as `FaultEvent`, and
  emit agent-authoritative `InstanceStatus` transitions.
- **Digest verification before every invocation** — including recovery-time `cleanup` after an
  agent restart. Digest mismatch fails preflight; the plugin is never executed.
- **Two-phase preflight**: `phase: "install"` at catalog load, `phase: "runtime"` immediately
  before injection. The agent also rejects `duration_secs > max_duration_secs` as a runtime
  defense even though the master will enforce it at VALIDATE in step 3.
- **Instance journal** persisted under `/var/lib/faultforge` (configurable `data_dir`), written
  before `inject`, removed on terminal state. A restarted agent replays the journal and completes
  recovery of any live instance.
- **Single safety timer** per instance: graceful stop at `duration`, master-loss self-abort after
  a configured silence threshold, dead-man forced recovery at `duration + grace` → `ERROR`.
- **TAINTED persisted host-side** (a taint file under `data_dir`) when recovery fails after the
  idempotent retry; reported to the master via `TaintStatus` on registration and on change;
  cleared only by operator action (the clear command itself arrives in step 3).
- **Agent session becomes resilient**: the slice-1 "log and exit on stream failure" behaviour is
  replaced by reconnect-with-backoff. On re-register the agent sends an `InstanceReport`
  reconciliation snapshot; sustained loss beyond the threshold triggers self-abort of active
  instances. **BREAKING** for the `agent-heartbeat` spec (exit-on-failure requirement replaced).
- **`faultforge-fault` gains the `proto` feature** (deferred from the previous change's D1):
  `InstanceState` and event conversions between domain and wire types, so plugin binaries still
  never link tonic.
- **`InstanceStatus`/`InstanceReport` gain the plugin digest** (resolves the open question from
  `implement-fault-schema`): reconciliation snapshots identify what the agent actually ran.
  Additive proto field; wire-compatible.
- **Master stays behaviourally inert**: new agent→master frames (`FaultEvent`, `InstanceStatus`,
  `InstanceReport`, `TaintStatus`) remain log-and-continue on the master. Dispatch, experiment
  records, and management-API writes are step 3.
- **Tested against an in-process fake master** (tonic server on a local listener) plus unit tests
  of the pure state machine; no podman (e2e is step 4).

## Capabilities

### New Capabilities

- `agent-fault-runtime`: the agent-side execution runtime — instance supervision and the state
  machine's agent-owned transitions, catalog resolution + digest gate, journal format and
  recovery replay, the safety-timer triad, taint persistence and reporting, and reconciliation
  via `InstanceReport`.

### Modified Capabilities

- `agent-heartbeat`: the "agent exits on connection or stream failure without reconnecting"
  requirement is replaced by reconnect-with-backoff and master-loss tracking (the timer input for
  self-abort).
- `fault-plugin-model`: the wire-schema requirement is amended so `InstanceStatus` /
  `InstanceReport` entries carry the `plugin_digest` (audit/reconciliation), and the reserved
  note that inject is never re-issued gains the concrete agent-side reconciliation trigger
  (report on every re-register).

## Impact

- **`crates/agent`** — the bulk of the change: new modules (supervisor, state machine, plugin
  runner, journal, taint, timers) and a reworked session loop (reconnect, frame dispatch, event
  forwarding). New config keys: `plugin_root` (default `/usr/lib/faultforge/plugins`), `data_dir`
  (default `/var/lib/faultforge`), `master_loss_threshold_secs`.
- **`crates/fault`** — new `proto` feature flag with domain⇄wire conversions; possibly small
  additions to `protocol.rs` used by the runner.
- **`crates/proto`** — additive `plugin_digest` field on `InstanceStatus`; regenerated code.
- **`crates/master`** — log-and-continue arms only; no behaviour change (existing tests must
  still pass unchanged).
- **Dependencies** — no new runtime deps expected beyond what the workspace already has (tokio,
  tonic, serde_json, sha2); dev-side, the fake master reuses `faultforge-proto`.
- **Operational** — the agent now writes to `data_dir`; running as non-root in dev requires
  pointing `data_dir` at a writable path (tests use temp dirs).
