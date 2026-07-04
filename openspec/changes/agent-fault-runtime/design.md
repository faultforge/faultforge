# Design: agent-fault-runtime

## Context

Step 2 of the four-change roadmap fixed in `implement-fault-schema`. The contract exists and is
proven (`faultforge-fault` + `noop-marker` golden tests); the wire frames exist and both planes
log-and-continue on them. This change gives the agent the runtime: it must consume the contract
*unchanged* (any friction found here becomes a small delta spec, per the previous change's risk
note) and expose exactly the surface `master-fault-dispatch` (step 3) will drive.

Constraints: CONVENTIONS.md is mandatory — functional core / imperative shell, time as data
(`Clock`/`now` injected at the shell), thiserror, newtypes, pedantic clippy, no unwrap/expect.
ADR-0002 decisions 5–10, 12 govern behaviour. Master behaviour must not change (its tests pass
untouched).

## Goals / Non-Goals

**Goals:**

- The agent executes the full instance lifecycle for any catalog plugin, supervised, concurrent,
  and recoverable — provable end-to-end with an in-process fake master and script fixture plugins.
- Every safety property of ADR-0002 that lives host-side is real after this change: digest gate,
  two-phase preflight, journal + restart recovery, the one-timer triad, TAINTED quarantine.
- The session layer survives master loss: reconnect with backoff, reconciliation by
  `InstanceReport`, self-abort on sustained silence.

**Non-Goals:**

- No master dispatch, no experiment record, no management-API writes, no operator clear-taint
  command (step 3 — the taint *file* can be removed by hand meanwhile).
- No metric rules/connectors, no blast radius (step 3+).
- No podman/e2e harness (step 4).
- No standalone OS-level watchdog process (see D6 — deferred to the first stateful plugin).
- No `FetchArtifact`; the catalog is on-disk, baked in (ADR-0002 §4).

## Decisions

### D1. Runtime lives in `crates/agent` as sibling modules, not a new crate

New modules: `catalog.rs` (thin wrapper over `faultforge_fault::catalog` resolution + install
preflight), `machine.rs` (pure per-instance state machine), `runner.rs` (plugin process shell),
`journal.rs` (persistence shell + pure (de)serialization), `taint.rs`, `supervisor.rs`
(instance actor orchestration), and a reworked `session.rs`.

- *Alternative — a `faultforge-agent-runtime` crate*: rejected; nothing else consumes it, and the
  session/supervisor coupling (shared outbound channel, connectivity signal) would force a wide
  public API for one consumer.

### D2. Pure state machine: `step(state, event, now) -> (state', Vec<Effect>)`

`machine.rs` is the functional core. Inputs are `Event`s (`RunReceived`, `PreflightOk`,
`PreflightFailed(code)`, `InjectOk`, `InjectFailed(code)`, `DurationElapsed`, `AbortRequested`,
`MasterLost`, `DeadmanReached`, `CleanupOk`, `CleanupFailed`, …); outputs are `Effect`s
(`Invoke(PluginCommand)`, `EmitStatus(InstanceState, reason)`, `WriteJournal`, `RemoveJournal`,
`MarkTainted(reason)`, `ArmTimer(deadline)`). The per-instance task in `supervisor.rs` is the
imperative shell: it performs effects, turns their results into the next events, and never
decides state itself.

- *Why*: every ADR-0002 ordering rule ("retry cleanup once, then taint", "dead-man dominates",
  "plugin may not assert agent-owned states") becomes a table-driven unit test with no IO, no
  tokio, no sleeps.
- *Alternative — state implicit in an async control-flow function*: rejected; the abort/timer
  interleavings are exactly where implicit state machines grow bugs, and CONVENTIONS demands the
  core be pure.

### D3. Supervisor = actor with one tokio task per instance

`Supervisor` owns `HashMap<InstanceId, InstanceHandle>` and receives commands from the session
dispatcher over an mpsc. `RunFault` for an unknown id spawns an instance task; `AbortFault`
signals the task; `RunFault` for a *known* id (any state) is logged and dropped — inject is never
retried (ADR-0002 §12), and master-minted deterministic ids make the duplicate detectable. The
instance task drives `machine::step`, runs blocking plugin invocations via
`tokio::task::spawn_blocking` (the runner is sync `std::process`), and `select!`s over the abort
signal and the armed timer. Terminal state ⇒ the task emits its final `InstanceStatus`, removes
the journal, and the supervisor drops the handle.

- Instance ids: a newtype `InstanceId` (non-empty string, master-minted opaque token) in
  `faultforge-fault`, mirroring the `Hostname` pattern.

### D4. One deadline, three causes: the timer is `min()` over live constraints

Per instance the shell arms exactly one sleep at
`min(start + duration, master_loss_deadline, start + duration + grace)` where
`master_loss_deadline = last_seen_master + master_loss_threshold` is recomputed whenever
connectivity changes. Which constraint fired decides the event (`DurationElapsed` /
`MasterLost` / `DeadmanReached`) — "the deadline or the silence, whichever comes first"
(ADR-0002 §5) is literally one `min`. Connectivity is a `tokio::sync::watch<ConnState>`
published by the session layer (`Connected{since}` / `Lost{since}`); instance tasks re-arm on
every change notification.

- `master_loss_threshold_secs` is **agent config** (default 30): the spec says "a configured
  threshold", and unlike heartbeat cadence or grace it must keep working *while the master is
  unreachable*, so it cannot be a master directive delivered per-session.
- Wall-clock (`SystemTime`) is read once per shell step and passed into the core; the dead-man
  compares against `deadline_unix` exactly as handed to the plugin.

### D5. Journal: one JSON file per instance, written before `inject`, atomic

`data_dir/instances/<instance_id>.json` with `{instance_id, plugin_name, plugin_version,
plugin_digest, params_json, started_unix, duration_secs, grace_secs, deadline_unix}` — the
fields ADR-0002 §6 names, plus what recovery needs to rebuild the stdin object. Written via
temp-file + rename (same pattern as noop-marker's inject) *after* runtime preflight succeeds and
*before* `inject` is invoked: a crash between journal-write and inject recovers a no-op instance
(cleanup is idempotent), whereas the reverse order could strand an injected fault with no record
— the failure mode the journal exists to prevent. Removed only after the terminal
`InstanceStatus` is queued.

**Restart replay:** on startup, before opening the session, the agent lists
`data_dir/instances/`, and for each entry re-verifies the plugin digest and drives
`abort` → `cleanup` (both digest-gated). A journaled instance that outlived its agent is
*ambiguous by definition* — the supervision that made it safe is gone — so it is aborted, per
ADR-0002 §12, not resumed: `ABORTED` if recovered before `deadline_unix`, `ERROR` if the
dead-man had already passed. Replay outcomes are terminal, so they do not appear in the
`InstanceReport` (which lists live instances only); they are queued and sent as ordinary
`InstanceStatus` frames right after the first registration, and each journal entry is removed
once its status frame is queued.

- *Alternative — resume supervision of a live instance across restart*: rejected for v1. Between
  death and restart nothing supervised the fault; "re-attach and complete recovery"
  (ADR-0002 §6) promises a safe host, not a continued experiment. Resumption can be revisited
  when a real use case demands it.
- *Alternative — one journal file with all instances*: rejected; per-instance files make
  add/remove atomic without read-modify-write locking.

### D6. The watchdog in this change is journal-replay + in-process dead-man; a standalone watchdog process is deferred

ADR-0002 §6 requires stateful reverts (`tc`/`iptables`) to survive both plugin *and* agent
death via a separate watchdog process. This change ships the mechanism that survives agent
*restart* (journal + replay, D5) and agent *malfunction while alive* (dead-man timer, D4). It
does **not** spawn a detached OS-level watchdog: the only plugin that exists is `noop-marker`,
whose "fault" is inert by construction, so a detached revert job would be untestable scaffolding
with nothing real to revert. The deployment assumption — the agent runs under a supervisor
(`systemd Restart=always`), so "agent death" is a gap of seconds before replay runs — is
documented in the spec as an explicit prerequisite. The standalone watchdog lands with the first
stateful plugin (`tc`/`iptables`, in the catalog change after step 3), designed against a real
revert.

- *Risk accepted consciously*: an agent that dies and is never restarted leaves an active fault
  governed by nothing. With `noop-marker` the blast radius of that gap is zero; shipping
  `tc`/`iptables` without closing it is what the deferral gate forbids.

### D7. Runner: sync `std::process` shell around the existing protocol types

`runner.rs` invokes `<abs entrypoint> <command>` with `PluginInput` serialized to stdin (then
stdin closed), collects stdout NDJSON incrementally and stderr fully, applies a hard invocation
timeout (kill + `ERROR`-classified result) so a hung plugin cannot wedge its instance task, and
returns `InvocationResult { exit: ExitClass, events: Vec<PluginEvent>, malformed: Vec<String>,
stderr: String }`. Classification reuses `faultforge_fault::protocol` (exit table, event
parsing). Forwarding rules applied by the shell: every well-formed stdout line goes to the
master verbatim as `FaultEvent` (spec requires content-unmodified); malformed lines and stderr
are wrapped as agent-authored `log` events at `level: "error"`; plugin `status` lines naming
agent-owned states are forwarded but never fed to the machine (two-track telemetry, D3 of the
previous change).

- Invocation timeout: fixed at 60s for v1 (not per-manifest); plugins are one-shot commands, and
  a manifest knob can be added compatibly when a slow plugin exists.
- Digest verification (`load_plugin` + compare to `RunFault.plugin_digest`) runs **before every
  invocation**, including each replay-time `abort`/`cleanup` — verifying once at RunFault and
  trusting the disk afterwards would let an on-disk swap execute with a stale verdict.

### D8. Session goes full-duplex and reconnects; reconciliation on every register

`session.rs` splits into a writer (single mpsc drained into the stream — supervisor and
heartbeat share it) and a reader that dispatches frames: acks to the heartbeat loop, fault
frames to the supervisor, unknown frames logged (unchanged rule). The whole
connect→register→pump lifecycle wraps in a reconnect loop with exponential backoff (1s doubling
to a 30s cap, retrying forever); connectivity transitions publish to the `watch` channel that
feeds D4. Immediately after each `RegisterAck` the agent sends `InstanceReport` listing all
*live* (non-terminal) instances — state, digest, timestamps; the master accepts reported state
(ADR-0002 §12); an empty report is sent too, so the master can distinguish "nothing running"
from "no report yet". Instances absent from the report are terminal-or-never-ran; deterministic
master-minted ids let step 3 reconcile that side.

- **BREAKING for `agent-heartbeat`**: the slice-1 "log and exit, never reconnect" requirement is
  removed and replaced. A fault-supervising agent that exits on a stream blip would orphan
  active instances — the opposite of its job.
- Startup ordering: journal replay (D5) completes before the first connect, so the first
  `InstanceReport` is truthful and replay outcomes follow it as `InstanceStatus` frames.

### D9. Taint: presence-of-file under `data_dir`, reported on every register and on change

`data_dir/tainted.json` (`{reason, ts_unix_ms, instance_id}`) is written when cleanup/abort
fails after its single idempotent retry (exit 30 path) or when replay recovery fails. Written
atomically; never removed by the agent — clearing is an operator action (step 3 adds the
command; until then, deleting the file by hand). While tainted the agent **refuses `RunFault`**:
the instance is answered with agent-owned `ABORTED` (`reason: "host tainted"`) without invoking
the plugin — defense in depth under the master-side exclusion that arrives in step 3.
`TaintStatus` is sent after every `RegisterAck` (both values — `tainted: false` is information)
and immediately when taint is acquired.

### D10. Proto & contract deltas: additive digest field, `proto` feature, `InstanceId` newtype

- `InstanceStatus` gains `plugin_digest` (new field number; `InstanceReport` repeats
  `InstanceStatus`, so the snapshot inherits it) — resolves the previous change's open question
  in favour of carrying it: replay-time and reconciliation statuses should say *what binary* the
  verdict is about, and the field is free on the wire.
- `faultforge-fault` gains the `proto` feature (planned in the previous D1): `From`/`TryFrom`
  between `state::InstanceState` and the wire enum, gated so plugin binaries never link tonic.
- Master: no behaviour change; its existing log-and-continue arms already cover the frames, and
  the new field is invisible to code that only logs.

### D11. Testing: fake master in-process, fixture plugins as scripts

- **Fake master**: a tonic `AgentService` on `127.0.0.1:0` inside the test, scriptable
  (send `RunFault`/`AbortFault`, record received frames, drop the stream on cue). Drives the real
  `run_agent` — the same binary path production uses. Covers: happy path to `DONE`, abort, exit
  10/20/30 mappings, taint refusal, duplicate `RunFault` dropped, reconnect + `InstanceReport`,
  master-loss self-abort (short threshold), dead-man (short duration+grace), restart replay
  (run agent, kill mid-`ACTIVE`, restart against same `data_dir`, assert cleanup + statuses).
- **Fixture plugins**: `#!/bin/sh` scripts written into temp catalogs by the tests, digests
  computed with `faultforge_fault::digest` — each failure mode is a three-line script (exit 10,
  exit 20, exit 30 twice, malformed NDJSON, sleep-forever for the invocation timeout). The real
  `noop-marker` already golden-tests the contract from the plugin side; runtime tests need
  *misbehaving* plugins, which scripts express directly.
- **Machine tests**: pure table-driven tests in `machine.rs` for every transition and ordering
  rule — no tokio.

## Risks / Trade-offs

- **[Agent death with no supervisor restart leaves a live fault ungoverned]** → Accepted for
  this change (D6): only `noop-marker` exists, blast radius zero; documented deployment
  prerequisite (`Restart=always`); the standalone watchdog is gated on the first stateful
  plugin.
- **[Wall-clock jumps skew the dead-man]** → `deadline_unix` is absolute by contract (the plugin
  received it), so wall-clock is the honest reference; NTP steps large enough to matter also
  break the operator's mental model of `duration`. Accepted; monotonic time is used for backoff
  and intervals where absolute time isn't contractual.
- **[Blocking runner on `spawn_blocking` could exhaust the pool under many instances]** → Bounded
  by realistic v1 concurrency (a handful of instances per host); each invocation is short-lived
  by contract and hard-timeboxed (D7).
- **[Reconnect loop can flap against a half-up master]** → Backoff caps at 30s; registration is
  idempotent (supersede-on-reconnect already specified in slice 1); reconciliation is
  report-based so no double-inject is possible regardless of flap count.
- **[Fixture-script plugins assume a POSIX shell]** → Fine: targets are bare-metal Linux (and
  macOS dev machines); CI is Linux. Same assumption `noop-marker`'s chmod-based tests already
  make.
- **[Journal schema will need migration once real plugins add state]** → The journal is
  single-host, short-lived (lifetime of an instance), and versioned implicitly by the agent
  binary that wrote it; a `version` field is included from day one so replay can reject files it
  doesn't understand instead of misreading them.

## Migration Plan

Additive on the wire (`plugin_digest` field number is fresh; old masters ignore it, and this
master only logs). Agent behaviour changes are the feature. No data migration — `data_dir`
starts empty on first run; the agent creates `instances/` on demand. Deployment note: agents
should run under a process supervisor before any stateful plugin ships (D6).

## Open Questions

- Should `master_loss_threshold_secs` eventually become a master directive with an agent-side
  floor (master tunes it, agent enforces a minimum it can honour offline)? Revisit in step 3
  when the master gets config surface for experiments.
- Whether replay should distinguish "crash before inject ever ran" (journal present, marker of
  inject-start absent) and skip straight to journal removal. v1 runs idempotent cleanup
  unconditionally — harmless but noisier. Decide when journals gain a phase marker, if ever.
