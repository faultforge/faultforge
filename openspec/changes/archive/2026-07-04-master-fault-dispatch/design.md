# Design: master-fault-dispatch

## Context

Step 3 of the four-change roadmap. The agent runtime (step 2) is live and its behaviour is
frozen: duplicate `RunFault` ids are dropped, inject is never re-issued, reconciliation is by
`InstanceReport`, taint is host-persisted and never self-cleared. This change builds the master
half against that surface, deliberately *below* the full `experiment-model` spec: no metric
rules, no connectors, no tag targeting, no blast radius — those stay in `experiment-model` as
the future target. Where experiment-lite and `experiment-model` overlap (actions, durations,
single salvo, kill-switch, `ERROR` dominance, legible outcomes), the shapes must match so the
full model later extends rather than replaces.

Constraints: CONVENTIONS.md (functional core / imperative shell, time as data, thiserror,
newtypes, pedantic clippy); ADR-0002 decisions 11–17; agent behaviour must not change except the
`ClearTaint` handler.

## Goals / Non-Goals

**Goals:**

- An operator can run a real fault on a chosen registered host, watch instance states live, halt
  the experiment, and clear a taint — via HTTP and CLI, no hand-rolled gRPC client.
- Every experiment record resolves to `COMPLETED` / `ABORTED` / `ERROR` in bounded time, with
  the cause named, even if agents disconnect mid-run.
- The master's view of instance state comes exclusively from agent-authoritative frames
  (ADR-0002 §12): no state invented from dispatch attempts, no lifecycle driven by `FaultEvent`.

**Non-Goals:**

- No metric rules, connectors, preconditions, guardrails, hypothesis, or verdicts
  (`RESILIENT`/`WEAKNESS_FOUND` are unreachable in lite — there is no hypothesis to judge).
- No tag targeting, no blast-radius ceiling (explicit hostnames *are* the operator's blast
  radius), no dry-run, no staged scenarios.
- No persistence of experiment records (ADR-0002 §17) and no audit log (known gap).
- No auth/TLS on either plane (ADR-0002 known gap — deployment docs say loopback/trusted net).
- No TUI additions; CLI is one-shot commands only.

## Decisions

### D1. Experiment-lite record: `experiment-model` shape, hostname targets, in-memory store

The operator submits `{ name, actions: [ { hosts: [hostname, ...], plugin: { name, version },
params: {...}, duration_secs } ] }`. The master adds: `experiment_id`, `grace_secs` (from config
`default_grace_secs`, ADR-0002: grace is decided by the master like heartbeat cadence),
per-instance records, timestamps, state, and outcome. `actions` stays a flat list under one
implicit stage and `hosts` is a list so tag-resolution can replace explicit hostnames later
without reshaping the record. A host may appear in multiple actions (one concurrent instance per
action, ADR-0002 §8); a duplicate host *within* one action is rejected at VALIDATE as sloppy
input.

Storage is `Arc<Mutex<HashMap<ExperimentId, Experiment>>>` beside the registry — deliberately
the same in-memory pattern (ADR-0002 §17). A master restart forgets experiments while agents
keep hosts safe (self-abort on master loss); after a restart the master answers
`GET /experiments` with an empty list and logs frames referencing unknown instances. It never
asserts an outcome it cannot back.

- *Alternative — SQLite now*: rejected; ADR-0002 §17 defers it, and the host-side safety net is
  exactly what makes that acceptable. Adding persistence here would smuggle in the biggest
  deferred design decision as a side effect.

### D2. Master catalog = on-disk catalog directory, loaded with the existing `faultforge-fault` loader

Master config gains `catalog_root` (default `/usr/lib/faultforge/plugins`, same layout as the
agent's `plugin_root`). At startup the master walks `<catalog_root>/<name>@<version>/`, loading
each entry with `faultforge_fault::catalog` — parsed manifest (`params_schema`,
`max_duration_secs`) plus the computed digest, i.e. everything VALIDATE and `RunFault` minting
need, with zero new file format and no drift from what agents have baked in (both packages ship
the same first-party catalog). An entry that fails to load (bad manifest, unreadable entrypoint)
is logged and skipped — one broken plugin must not take down the control plane; an empty or
missing `catalog_root` yields an empty catalog and every experiment fails VALIDATE with "unknown
plugin". The catalog is read once at startup; hot reload is deferred until plugins change more
often than masters restart.

- *Alternative — standalone digest file (original sketch)*: rejected with the owner; it
  duplicates manifest data into a hand-maintained file that can drift from the agent package,
  and needs generation tooling the loader already replaces.

### D3. Ids: opaque `experiment_id`, deterministic `instance_id`

`experiment_id` is minted at accept time from the injected clock + a per-process counter
(`exp-<unix_ms>-<seq>`) — unique per master lifetime, which matches the record's in-memory
lifetime; no uuid dependency needed. `instance_id` is `<experiment_id>:<hostname>:<action_index>`
— deterministic per (experiment, agent, action) exactly as ADR-0002 §12 requires, so a reported
instance always correlates to the command that minted it and duplicate dispatch is structurally
impossible. The format stays within the agent's id charset (`[A-Za-z0-9._:-]`); VALIDATE rejects
any target hostname containing characters outside it (defense in depth — real hostnames already
comply).

### D4. Per-hostname outbound-session map in the master

Dispatch needs "send this frame to that agent". The gRPC layer gains a session map
`Arc<Mutex<HashMap<Hostname, SessionHandle>>>` where `SessionHandle` wraps the stream's outbound
sender; `Register` inserts (superseding any previous handle — the newest stream wins, aligning
the existing supersede-on-reconnect registry rule with dispatch), and the handler removes its
own entry only if it is still the current one when the stream closes. Sending to a host without
a live handle is a normal, non-fatal outcome surfaced to the caller (halt continues with the
other instances; clear-taint returns 409).

- *Why not merge into `Registry`*: `AgentInfo` is management-plane read model; the session map
  is control-plane plumbing with a different lifecycle (entry dies with the stream, not with
  staleness). Two maps, one owner each, no lock coupling.

### D5. Instance tracking, reconciliation, and the experiment deadline

The master updates per-instance state only from `InstanceStatus` frames and accepts
`InstanceReport` snapshots as truth on re-register (ADR-0002 §12). `FaultEvent` lines are logged
(operator-visible via master logs) but stored nowhere and never drive state. Frames referencing
instances the master does not know (previous master life, manual gRPC clients) are logged and
dropped.

Every experiment arms one master-side deadline at `start + max_over_actions(duration_secs +
grace_secs) + margin` (constant 30s margin — covers reconnect backoff and replay; promote to
config if a real deployment needs it). Any instance still non-terminal at the deadline is marked
`ERROR` with reason `unresolved at experiment deadline` — by then the agent's own dead-man has
either fired and the report was lost, or the agent is gone; either way the operator must treat
the host as suspect. The deadline guarantees the record always resolves, which the CLI `--wait`
and the e2e harness (step 4) rely on.

### D6. Kill-switch and the reduced outcome lattice

Per ADR-0002 §13 the experiment is all-or-nothing. The kill-switch fires on the first of: any
instance reaching `ERROR`; any instance reaching `ABORTED` that the master did not itself
request (agent-initiated abort); `TaintStatus { tainted: true }` from an in-scope host; operator
halt; deadline expiry. Firing means: send `AbortFault` for every non-terminal instance over live
sessions (missing sessions are fine — that agent's self-abort covers it), mark the experiment
`HALTING`, and record the trigger (host, instance, reason, timestamp) as the experiment's cause.

Outcome, computed when the last instance is terminal (or at the deadline), by precedence:

- `ERROR` — any instance `ERROR`, or any in-scope host tainted during the run. Dominates.
- `ABORTED` — no `ERROR`/taint, but not every instance reached `DONE` (operator halt, agent
  self-abort, agent-side rejection). Clean halt, inconclusive.
- `COMPLETED` — every instance `DONE`. This is experiment-lite's "ran cleanly"; the
  `RESILIENT`/`WEAKNESS_FOUND` split arrives with the hypothesis in the full `experiment-model`.

Every non-`COMPLETED` outcome carries its recorded cause — the ADR §15 legibility rule, minus
the metric-rule fields that do not exist yet. Observable experiment states are `RUNNING`,
`HALTING`, and the three outcomes.

The outcome computation and kill-switch trigger predicate are pure functions over the experiment
record (`crates/master`, functional core); the dispatcher task is the imperative shell.

### D7. Clear-taint is an operator command relayed to the agent

`POST /agents/{hostname}/clear-taint` → 404 if the hostname is not registered, 409 if no live
session (the agent must actively remove its taint file — the master cannot do it remotely), else
the master sends the new `ClearTaint {}` frame down that session and returns `202 Accepted`. The
frame is empty: the target is the session it travels on. The agent removes `tainted.json`
(idempotent — already-clean answers the same way) and emits `TaintStatus { tainted: false }`,
which updates the registry's taint flag; the operator observes the effect via `GET /agents`.
This keeps "a human clears the taint" (ADR-0002 §9) while closing the step-2 gap where the only
mechanism was deleting the file by hand.

- *Why asynchronous (202)*: the management plane never blocks on agent round-trips; the same
  pattern all dispatch follows. The CLI prints "requested — verify with `agents show`".

### D8. Management surface and CLI

New management endpoints (axum, same read-model conventions as `agent_view`):

- `POST /experiments` — VALIDATE synchronously; `422` with *all* failing checks named, or `201`
  with the full record (minted ids included) and dispatch already under way.
- `POST /experiments/{id}/halt` — most-available: fires the kill-switch, `202`; `404` unknown
  id; `409` only if already terminal.
- `GET /experiments` / `GET /experiments/{id}` — list summaries / full record with per-instance
  states, reasons, timestamps, and the outcome cause.
- `POST /agents/{hostname}/clear-taint` — D7.
- `GET /agents` / `GET /agents/{hostname}` — gain `tainted: bool`.

The "read-only management API" requirement of `agent-status-api` is replaced by "unauthenticated
during WIP, writes limited to the fault-dispatch surface" — auth/TLS remains the ADR-0002
critical known gap and must land before untrusted-network exposure.

CLI (one-shot only): `experiment run -f <file>` reads YAML or JSON (serde_yaml already in the
workspace via manifests; the file mirrors the POST body), prints the created record, and with
`--wait` polls until terminal — exit 0 only for `COMPLETED`, distinct non-zero for
`ABORTED`/`ERROR`, making the CLI scriptable as a chaos gate. `experiment list` / `show <id>` /
`halt <id>` and `agents clear-taint <hostname>` map 1:1 to the endpoints; all honour
`--master-url` and `-o json|table`.

### D9. Testing: scripted fake agents against the real master

Mirror of step 2's fake-master pattern: tests spin the real master (gRPC + management on
`127.0.0.1:0`) and connect *scripted fake agents* — thin tonic clients that register, heartbeat,
and play back recorded `InstanceStatus`/`InstanceReport`/`TaintStatus` sequences on cue. HTTP
assertions drive the management surface. Covers: catalog load (good/broken entries), every
VALIDATE rejection, happy dispatch to multi-host multi-action, kill-switch on `ERROR`/self-abort
/taint/halt, deadline expiry with a vanished agent, reconciliation on re-register, outcome
lattice precedence, clear-taint 404/409/202 + `TaintStatus` round-trip, and master restart
amnesia (frames for unknown instances logged, not crashed). Outcome/kill-switch pure functions
get table-driven unit tests. The CLI reuses its existing HTTP-mock test approach for the new
commands. Real end-to-end (real agent + real plugins in containers) is step 4, not this change.

## Risks / Trade-offs

- **[Write endpoints on an unauthenticated plane]** → Accepted, owner decision recorded in
  ADR-0002 known gaps (this is the Chaotic Deputy shape). Mitigation until auth lands: default
  bind stays `127.0.0.1`, docs say never expose the management port beyond a trusted network.
- **[Master restart mid-experiment orphans the record]** → Accepted per ADR-0002 §17: agents
  self-abort on master loss, replay on restart, and report truthfully on reconnect; the master
  logs unknown-instance frames. The operator sees the experiment vanish — documented behaviour.
- **[Deadline margin is a constant]** → 30s is generous against a 30s reconnect-backoff cap; a
  config knob is trivial to add compatibly if a deployment's timing differs.
- **[`HALTING` can linger while an agent is unreachable]** → Bounded by the experiment deadline;
  the agent's own master-loss self-abort means the host recovers long before the record does.
- **[Catalog on the master can differ from an agent's baked catalog]** → Digest verification on
  the agent already fails closed (`ABORTED`, digest-mismatch reason), and the outcome names it.
  Same-package distribution makes this an operational error, not a silent one.
- **[In-memory experiment map grows unboundedly]** → Records are small and per-master-lifetime;
  a cap/eviction is premature. Revisit with persistence (ADR-0002 §17).

## Migration Plan

Wire: `ClearTaint` is a new oneof arm — old agents log-and-continue (existing rule), old masters
never send it. Management plane: purely additive endpoints plus a new `tainted` field on agent
views (additive JSON). CLI: new subcommands only. No data migration; `catalog_root` defaults to
the same path as the agent's `plugin_root`, so single-host dev setups work unchanged.

## Open Questions

- Should `experiment run --wait` stream `FaultEvent` telemetry once the master retains any of
  it? Deferred with event storage itself (currently log-only).
- Does `HALTING` deserve sub-causes in the API (operator vs kill-switch vs deadline) beyond the
  recorded cause string? Decide when the TUI grows an experiments view.
- When tags land in the registry, does `hosts` become a selector union (explicit + tag), or does
  a separate `target` object replace it? The flat list was chosen to keep both doors open.
