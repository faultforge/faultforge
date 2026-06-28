## Context

With `fault-plugin-model` an agent can run a single fault safely on its host. This change designs
how the master turns that capability into an **experiment**: a fleet-wide, judged, safety-bounded
run (v1 scope). The founding question — "how does the master decide success or failure?" — is
answered by declared metric rules, not by whether a fault fired. Governing decisions:
[ADR-0002](../../../docs/adr/0002-fault-model-decisions.md). No code is written in this change.

## Goals / Non-Goals

**Goals:**
- An experiment definition that targets the fleet by tag and binds faults to groups.
- A **write control surface**: launch, watch, and an unconditional HALT.
- Success decided against pre-declared metric rules, with the hypothesis judged *during* injection.
- Mandatory safety: blast radius against live hosts, guardrail halts, dry-run, legible outcomes.

**Non-Goals (deferred for v1 — see ADR-0002 "known gaps"):**
- Plugin execution internals (→ `fault-plugin-model`).
- **Auth/TLS on either plane, audit log** — conscious WIP deferrals (the operator write path and the
  agent command channel are unauthenticated; this must close before untrusted-network prod use).
- Staged scenarios, durable storage, full RBAC, non-Prometheus connectors, web UI.

## Decisions

### Experiment definition: tag-targeted actions (ADR-0002 §8, §11, §16)

```yaml
experiment: db-failover-drill
blast_radius:
  max_percent: 50          # of each target group, computed against LIVE hosts
  max_hosts: 1
  max_concurrent: 1
sampling_interval: 5s
actions:                   # single salvo — all fire together
  - target: { tag: db }
    plugin: { name: kill-process, version: 1, digest: sha256:… }   # external unit digest
    params: { process: mysqld }
    duration: 120s
preconditions:             # role 1 — gate entry (before)
  - connector: prometheus
    query: 'avg_over_time(http_error_rate[1m])'
    tolerance: '< 0.01'
guardrails:                # role 2 — halt (during, continuously sampled)
  - connector: prometheus
    query: 'http_error_rate'
    tolerance: '< 0.05'
    sustain: 30s
hypothesis:                # role 3 — verdict, sampled DURING injection
  - connector: prometheus
    query: 'histogram_quantile(0.99, http_latency)'
    tolerance: '< 0.1'
```

### Write control surface with an unconditional HALT (ADR-0002 §1 product framing)
The operator can **launch**, **watch live**, and **HALT** an experiment. This resolves the earlier
contradiction (a read-only management plane vs. "the operator may trigger the kill-switch at any
time"). The management plane gains write endpoints for launch/abort. **HALT MUST be the
most-available operation**: it must succeed even when connectors are down, metrics are unreachable,
or the experiment record is degraded — it cannot depend on the machinery that may be failing.

### Experiment lifecycle (ADR-0002 §13, §14, §15)
```
 DRAFT ─validate─▶ VALIDATE ─ok─▶ PRECONDITIONS ─hold─▶ INJECTING ─▶ MONITORING ─▶ RECOVERING ─▶ VERDICT
                     │ fail          │ not healthy         │            │ guardrail/abort/instance-fail
                     ▼               ▼                     │            ▼
                  rejected      not started                │     global kill-switch ─▶ RECOVERING
                                                           └─ hypothesis sampled HERE (during injection)
```
- **VALIDATE**: schema valid, plugins resolvable, `duration <= max_duration_secs`, blast radius
  satisfiable **against live hosts**, no `TAINTED` host in scope.
- **PRECONDITIONS**: precondition rules must hold or the experiment does not start.
- **MONITORING**: guardrails sampled every `sampling_interval`; **the hypothesis is sampled here,
  during injection** (overlapping the active fault window), not after recovery.
- **VERDICT**: aggregates the during-injection hypothesis samples into the outcome.

### Three roles of metric rules via connectors (ADR-0002 §14)
Precondition (entry gate), guardrail/halt (during, continuous), hypothesis (sampled during
injection, aggregated at VERDICT). Precondition reuses the guardrail evaluator run once — near-zero
marginal cost. Smoothing **window** is in the query; **persistence** is the separate `sustain`
field. A connector unreachable beyond tolerance is **fail-safe**: treated as a guardrail breach
(abort), never as "in range".

### All-or-nothing + global kill-switch; ERROR dominates ABORTED (ADR-0002 §13, §15)
Any instance terminal failure, sustained guardrail breach, agent-initiated abort the master learns
of, connector outage beyond tolerance, or operator HALT ⇒ global kill-switch stopping every
instance. Outcome by precedence lattice:
```
ERROR        any instance ERROR / host TAINTED   (a host may be broken)   ← DOMINATES
ABORTED      clean halt, no broken host, inconclusive
WEAKNESS_FOUND  ran + recovered cleanly, hypothesis broke   (the valuable result)
RESILIENT    ran + recovered cleanly, hypothesis held
```
This fixes the prior contradiction where every `ERROR` collapsed into `ABORTED`.

### Legible outcomes (ADR-0002 §15)
Every `ABORTED`/`ERROR` names host + instance + rule + value-vs-threshold + timestamp. An abort the
operator cannot explain wastes a scarce production change window.

### Blast radius against live hosts → staleness sweep (ADR-0002 §16)
The ceiling is computed against **currently-live** hosts. Because the registry today has no
staleness sweep, a percentage over a roster that counts dead agents is dishonest; a **heartbeat
staleness sweep is implied v1 work** for this capability.

### Dry-run / validate (ADR-0002 §10)
VALIDATE + PRECONDITIONS + per-host runtime preflight + confirm every plugin is present in the
baked-in catalog, **without injecting**. Reports the exact hosts that would be hit and any blockers.

## Risks / Trade-offs

- **No auth in v1** (known gaps): the write control surface and the agent command channel are
  unauthenticated. The write path that fires prod faults plus master impersonation are the Chaotic
  Deputy vector. Conscious WIP deferral; the highest-priority gap to close.
- **All-or-nothing can over-abort**: accepted; legible outcomes make an abort a lesson, not a waste.
- **Single-salvo limits realism**: deferred by schema design, not blocked.
- **No durable experiment state**: master restart loses the record while faults may be live; the
  agent journal + host-side self-revert is the safety net, and the master must not assert a verdict
  it cannot back after restart.
- **Blast radius depends on the staleness sweep**: until the sweep lands, percentages are only as
  honest as registry liveness.
