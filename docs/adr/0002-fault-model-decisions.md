# ADR-0002: Fault Model Decisions

**Date:** 2026-06-28
**Status:** Accepted

---

## Context

FaultForge so far does register + heartbeat only (slice 1): the master tracks agents in an
in-memory registry, agents hold one persistent bidirectional gRPC `Session` stream, and a
read-only HTTP management plane exposes registry state. There is no fault injection yet.

This ADR records the cross-cutting decisions for the **fault model** — the heart of the product —
**scoped to a deliberately centered, prod-ready v1**. The detailed, behaviour-level specs live in
two OpenSpec changes that this ADR governs:

- `fault-plugin-model` — the agent/plugin side: how a fault is described, distributed, and run on
  a single host with a safe lifecycle.
- `experiment-model` — the master side: how experiments are orchestrated and how success/failure
  is decided.

The decisions were reached through an exploration session and a two-persona design debate (a
bare-metal SRE/buyer and a security/quality reviewer). The guiding principle for v1 is **"Trust,
See, Stop"**: a fault must never strand a host, a running experiment must be observable, and an
operator must be able to halt it. Where v1 omits something, it is recorded as a **deliberate
deferral with a compensating control**, not an oversight.

The overarching design tension is bare-metal safety: FaultForge runs supplied binaries, often as
root, on production hosts. Every decision is biased toward *recoverability* over flexibility — a
fault that cannot be cleaned up is worse than a fault that cannot be expressed. The single most
relevant cautionary tale is Chaos Mesh's *Chaotic Deputy* (CVE-2025-59358, 2025): an
**unauthenticated control server** let an attacker issue `killProcess` and take over a cluster.
This shapes the "known gaps" section below.

---

## Decision

### 1. A plugin is a manifest + an executable, not a pure declaration and not WASM

A fault plugin is a **declarative manifest** plus an **executable artifact** (an ELF binary or a
script) that the agent runs according to a fixed lifecycle contract. Rejected alternatives: a pure
declarative engine (too limited; kills "bring your own fault") and a WASM sandbox (chaos needs
syscalls and root-level host effects; a sandbox fights the purpose). The price — running foreign
code as root — is bounded by decisions 3 and 18 and acknowledged in "known gaps".

### 2. The manifest is a machine-readable contract that declares what the plugin needs

The manifest declares identity (`name`, `version`), `entrypoint`, a `params_schema` for validating
parameters before dispatch, a `requires` block (host binaries, privileges, minimum resources,
and any per-fault precondition such as *target is a restartable service unit*), and
`max_duration_secs`. This lets the master and an operator **audit a plugin without executing it**,
and makes preflight data-driven.

### 3. Integrity by a single digest over manifest+executable; trust by first-party catalog + operator

A plugin's **integrity** is a `sha256` digest computed over the manifest and executable **as a
unit**. The manifest body does **not** contain its own digest (a file cannot hash itself); the
digest is an external reference (in the experiment definition and the catalog), so `requires`,
`max_duration_secs`, and the entrypoint are all covered. The agent verifies the digest before
trusting any plugin. Integrity is not safety: a verified digest of a malicious root binary is
faithfully malicious. **Safety** therefore rests on a **first-party / built-in vetted catalog**
plus the operator's explicit, audited install — not on digest pinning. Third-party plugins and
signing are deferred (see "known gaps").

### 4. Content-addressed distribution model retained in the wire contract; v1 ships a baked-in catalog

The architecture keeps the content-addressed model and a dedicated `FetchArtifact(digest) -> stream
Chunk` RPC **shape** in the proto, so dynamic pull can arrive later without reshaping anything.
**For v1, the vetted catalog is baked into the agent package**; the master does not run an artifact
store and the streaming transfer is not built. Digest pinning and on-disk verification are retained
(the agent still verifies the cached plugin's digest before running). Dynamic distribution — the
"agent notices it lacks a plugin and pulls it from the master" flow — is **v2**, gated on a catalog
large enough to justify it plus signing and auth. Rationale: in v1, before signing and auth, a
dynamic remote-code-delivery channel to root on prod is attack surface, not a feature.

### 5. One agent-side safety-timer model (duration authoritative; silence ⇒ self-abort; dead-man backstop)

The agent owns a **single** timer model rather than two overlapping ones:

- **`duration`** (set by the experiment) is authoritative for how long a fault is active. The
  normal end is a graceful stop followed by recovery.
- **Loss of the master** beyond a short threshold triggers an **agent-initiated self-abort** —
  the agent does not keep a fault running while blind to the control plane.
- A **dead-man deadline** at `duration + grace` is the backstop: if neither a graceful stop nor a
  self-abort has fired by then, the agent forces recovery and marks the instance `ERROR`.
- **`max_duration_secs`** (manifest) is the longest the plugin can safely self-revert within;
  `duration <= max_duration_secs` is enforced at VALIDATE.

These are one coherent mechanism keyed off "the deadline or the silence, whichever comes first",
not separate features with undefined precedence.

### 6. Recovery is concrete per fault and survives agent death

A uniform lifecycle is *not* enough when faults have radically different revert semantics. v1
specifies revert **per fault**, and makes it survive the agent process dying:

- **Stateful host changes (`tc`, `iptables`)**: the revert is owned by a **separate watchdog
  process / scheduled job**, not the plugin process (`tc` has no native TTL). Killing the plugin
  must not be required to revert, and killing the agent must not strand the change.
- **`kill-process`**: "un-kill" is impossible, so recovery = ensuring the service supervisor
  restarts the process. Therefore *"the target is a restartable service unit"* is a **preflight
  precondition** (decision 2/10), not an assumption.
- **Resource faults (cgroup teardown, bounded-file removal)**: revert by tearing down the
  agent-created cgroup / removing the bounded fill file.

An **agent-side instance journal** (`instance_id`, plugin digest, params, deadline) persisted to
`/var/lib/faultforge` lets a **restarted agent re-attach its watchdog and complete recovery** —
this is the host-side safety net that makes deferring master persistence (decision 17) acceptable.

### 7. `abort` and `cleanup` are distinct lifecycle paths; `cleanup` is idempotent

`abort` = fast, brutal "stop now, save the host". `cleanup` = orderly teardown. Distinct contract
methods (a simple plugin may implement `abort` via its own `cleanup`). `cleanup` MUST be
idempotent so it can be re-issued — including by the watchdog after agent restart (decision 6).

### 8. The unit of execution is a fault instance; the agent supervises many; concurrent per host allowed

A **fault instance** is one run of one plugin with one parameter set, with its own lifecycle and
`instance_id`. The agent supervises a *set* of instances. A host matching multiple target groups
runs **multiple concurrent instances by design** (one per action). **v1 does not implement
`conflicts_with` / resource-exclusion**: interference between concurrent instances on one host
(e.g. two faults skewing each other's metrics) is the **operator's responsibility**. This keeps the
agent simple at the cost of operator discipline.

### 9. Cleanup failure quarantines the host (`TAINTED`)

If recovery fails even after the idempotent retry, the host is marked `TAINTED`, excluded from new
experiments, and an operator is alerted. A human clears the taint.

### 10. `preflight` runs twice, plus per-fault preconditions

Static check at install (artifact present, executable, manifest valid). Runtime check immediately
before injection (required binaries, privileges, resources, **and per-fault preconditions** such as
"target is a restartable service unit" for `kill-process`). A fault is never injected on a host
that fails runtime preflight.

### 11. Experiments are single-salvo in v1; the schema leaves room for stages

All faults fire together; no ordered multi-stage scenario engine yet. `actions` is a flat list
under one implicit stage so staged scenarios can be added later without reshaping the data.

### 12. Inject is never retried; reconciliation is by state report; `instance_id` is master-minted

The master MUST NOT re-issue `inject`. A stream blip too brief for a self-abort is reconciled by
the agent **reporting current instance state**, which the master accepts. Ambiguous state ⇒ the
agent aborts. To make reconciliation well-defined, **`instance_id` is minted by the master**,
deterministic per `(experiment, agent, action)`, and carried in the `inject` command — so the
master can always correlate a reported instance to the command it issued, and a never-acked inject
to a subsequently-reported state. Double-injection is structurally impossible.

### 13. All-or-nothing experiment semantics; `ERROR`/`TAINTED` dominates `ABORTED`

The experiment is atomic across the fleet. Any instance reaching a terminal failure, a sustained
guardrail breach, an agent-initiated abort the master learns of, a connector outage beyond
tolerance, or an operator command ⇒ a **global kill-switch** stops every instance. The final
classification follows a **precedence lattice**: if any instance ended in `ERROR`/`TAINTED` (a host
may be broken), the experiment outcome is **`ERROR`**, which **dominates** `ABORTED`. `ABORTED` is
reserved for a clean halt with no broken host. This closes the prior contradiction where every
`ERROR` was swallowed into `ABORTED` and the most safety-critical outcome was unreachable.

### 14. Success is decided by metric rules in three roles; the hypothesis is evaluated *during* injection

FaultForge decides success against pre-declared metric rules evaluated through **connectors**
(Prometheus first), in three roles:

- **Precondition** (before) — entry gate; do not inject into an already-sick system. Reuses the
  guardrail evaluator run once, so its marginal cost is near zero.
- **Guardrail / halt** (during, continuously sampled) — safety threshold; breach ⇒ global
  kill-switch.
- **Hypothesis / verdict** — **sampled *during* the injection window** (overlapping MONITORING),
  not after recovery; VERDICT only aggregates. Evaluating after RECOVERING would measure whether
  the system *recovers*, not whether it *tolerates* the fault — the wrong question.

A metric's smoothing **window** is expressed in the query (`avg_over_time(...[30s])`); how long a
breach must **persist** is a separate `sustain` field. The master polls at a configurable
`sampling_interval`. A connector unreachable beyond tolerance is **fail-safe**: treated as a
guardrail breach (abort), never as "in range".

### 15. The experiment outcome separates "ran cleanly" from "hypothesis verdict"

- `RESILIENT` — ran and cleaned up cleanly, hypothesis held. 🟢
- `WEAKNESS_FOUND` — ran and cleaned up cleanly, hypothesis broke. 🟡 The *valuable* result, not a
  failure.
- `ABORTED` — a guardrail or connectivity loss forced a clean halt; inconclusive.
- `ERROR` — a plugin/agent malfunction or recovery failure left a host `TAINTED`. Dominates
  `ABORTED` (decision 13).

Every `ABORTED`/`ERROR` MUST be **legible**: it names the host, instance, rule, value-vs-threshold,
and timestamp. An abort you cannot explain wastes a scarce production change window.

### 16. Blast radius is a first-class v1 safety control, computed against live hosts

Every experiment declares a ceiling (max hosts and/or max percentage per target group, plus a
concurrency limit). The master refuses to exceed it, enforced at VALIDATE. The ceiling MUST be
computed against **currently-live** hosts — a percentage over a registry that still counts dead
agents is a lie. This couples blast radius to a **heartbeat staleness sweep**, which today does not
exist; see "known gaps".

### 17. Master experiment state is in-memory now, database-backed later

A master restart mid-experiment loses the experiment record while faults may be live on agents.
The host-side safety net (decisions 5 and 6: one-timer self-abort + watchdog + agent journal)
keeps hosts safe meanwhile; the master MUST NOT assert a verdict it cannot back after a restart.
Durable storage (likely SQLite) is deferred.

### 18. v1 fault catalog: `kill-process` + network faults done right; resource-exhaustion carries a known risk

v1 ships a **small, vetted catalog** with concrete agent-death-survivable recovery (decision 6):
`kill-process` and network faults (`tc`/`iptables` latency, loss). **Resource-exhaustion faults
(cpu / memory / disk)** remain in the architecture but carry a real hazard: they can starve the
very agent that must abort them, undermining the recovery guarantee. v1 mitigation is **agent OS
priority protection** (`nice` / `oom_score_adj`) as a cheap partial guard; **full cgroup v2
confinement** of fault instances is deferred to v2. Until confinement lands, resource-exhaustion
faults are "use with eyes open", not "prod-ready" in the same sense as the core catalog.

---

## Known gaps — deliberately deferred for v1 (each an explicit risk, not an oversight)

These were debated and consciously deferred. They are recorded here so they are not mistaken for
"done", and several MUST be addressed before FaultForge is exposed on a real production network.

- **Authentication / TLS on both planes — CRITICAL, deferred for WIP.** Today both planes are
  unauthenticated. The lethal vector is **master impersonation**: an agent executes root effects on
  whatever it accepts `inject` from, so anyone who can reach the agent channel and speak the proto
  is root on the host (this is exactly Chaotic Deputy). The intended floor — *agent authenticates
  the master* (server-side TLS with a pinned CA), ideally mutual TLS + an enrollment token on the
  agent plane, and TLS + a bearer token on the operator plane — is **not in v1 by owner decision**.
  This must be closed before any untrusted-network production use.
- **Append-only audit log** ("who killed what, where, when, did it clean up") — deferred. Cheap to
  add later (file/syslog append); distinct from the deferred experiment DB.
- **Full cgroup v2 confinement** of fault instances — deferred (decision 18); v1 uses only OS
  priority protection.
- **Dynamic plugin distribution** (the `FetchArtifact` transfer + master artifact store),
  **plugin signing** (cosign/sigstore), and a **plugin marketplace / BYO-fault** — deferred
  (decision 4); compensating control is the baked-in first-party catalog + digest pinning.
- **Heartbeat staleness sweep** — implied by decision 16; until it exists, blast-radius percentages
  are only as honest as the registry's liveness.
- **Master persistence / HA, staged scenarios, `conflicts_with`, non-Prometheus connectors, web
  UI** — deferred with compensating controls (host-side safety net, single-salvo, operator
  responsibility, CLI).

---

## Consequences

- The agent is a supervisor of independent fault instances, each with a single coherent safety
  timer (decisions 5, 8) and a persisted journal (6); recovery is specified per fault and survives
  the agent dying.
- The master is kept simple by single-salvo (11) and all-or-nothing (13); the outcome lattice now
  surfaces broken hosts as `ERROR` instead of hiding them (13, 15).
- "Success" is a property of declared metric rules evaluated *during* injection (14), making
  FaultForge an experiment platform, not just a fault injector.
- v1 is **smaller in premature infrastructure** (no artifact store, no dynamic distribution) and
  **more honest about recovery and observability** than the first draft — but it ships with auth,
  audit, and full confinement consciously deferred. Those deferrals are the line items to revisit
  before broad production exposure.
- Every "prod-ready" claim in v1 is intended to be **provable in a one-host POC**: dry-run,
  one-host blast radius, live observability, during-injection hypothesis, one-button HALT, and
  proven self-recovery when the master is killed.
