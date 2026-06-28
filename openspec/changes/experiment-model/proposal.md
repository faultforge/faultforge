## Why

Injecting a fault is not the goal — running a controlled *experiment* and deciding whether the
system stayed resilient is. This change captures the master-side model: how an experiment is
defined, targeted across the fleet, orchestrated, kept safe, and judged. It answers the founding
question "how does the master decide success or failure?". It builds on the agent/plugin
[`fault-plugin-model`](../fault-plugin-model/proposal.md) and the cross-cutting decisions in
[ADR-0002](../../../docs/adr/0002-fault-model-decisions.md).

This change captures the **model and behaviour** only; no Rust is written here. Implementation is
sliced later (see `tasks.md`).

## What Changes

- Define the **experiment definition**: a set of **actions**, each binding a target group
  (selected by host **tags**) to a plugin + parameters + `duration`, plus declared metric rules and
  a blast-radius ceiling.
- Define a **write control surface**: operators **launch**, **watch live**, and **HALT** an
  experiment; HALT is the most-available operation (works even when connectors/metrics are down).
  This resolves the read-only-management-plane contradiction.
- Define **single-salvo execution**: all actions fire together; the schema leaves room for staged
  scenarios later but v1 has one implicit stage.
- Define **targeting by tags**: experiments select hosts by tag; an experiment refuses to run on a
  `TAINTED` host.
- Define the **three roles of metric rules** evaluated through **connectors** (Prometheus first):
  **precondition** (before — gate entry), **guardrail/halt** (during — continuously sampled safety
  threshold), **hypothesis** (**sampled during injection**, aggregated at VERDICT — resilience
  judgement). Smoothing window lives in the query; persistence is a separate `sustain` field; the
  master polls at a configurable `sampling_interval`; a connector outage beyond tolerance is
  fail-safe (treated as a breach).
- Define **all-or-nothing semantics with a global kill-switch** and a **precedence outcome lattice**
  where **`ERROR`/`TAINTED` dominates `ABORTED`** (a broken host is never hidden as "inconclusive").
- Define the **experiment lifecycle**: DRAFT → VALIDATE → PRECONDITIONS → INJECTING → MONITORING →
  RECOVERING → VERDICT.
- Define the **outcome model**: `RESILIENT`, `WEAKNESS_FOUND`, `ABORTED`, `ERROR` — separating
  "ran cleanly" from "hypothesis verdict"; every `ABORTED`/`ERROR` is **legible** (names host,
  instance, rule, value-vs-threshold, timestamp).
- Define **dry-run / validate**: preflights + plugin-availability + preconditions **without
  injecting**, reporting the exact hosts that would be hit.
- Define **blast radius** as a mandatory v1 control computed against **live hosts**, which makes a
  **heartbeat staleness sweep** implied v1 work.

Explicit non-goals for this change (owned elsewhere or deferred — see ADR-0002 "known gaps"):

- **No plugin execution details** — owned by `fault-plugin-model`.
- **No staged/multi-step scenarios** — single-salvo only; data model leaves room (ADR-0002 §11).
- **No durable experiment storage** — in-memory now, database later (ADR-0002 §17); the master must
  not assert a verdict it cannot back after a restart.
- **No auth/TLS and no audit log** — conscious WIP deferrals; the operator write path and the agent
  command channel are unauthenticated (master-impersonation / Chaotic Deputy), the highest-priority
  gap to close before untrusted-network prod use.
- **No tag administration UI/flow** — tags are assumed to exist (agent self-declares, operator may
  later override).
- **No real connector implementations beyond the Prometheus contract.**

## Capabilities

### New Capabilities
- `experiment-model`: experiment definition, tag-based targeting, single-salvo orchestration with a
  lifecycle state machine, three-role metric evaluation via connectors, all-or-nothing global
  kill-switch, the four-state outcome model, dry-run, and blast-radius enforcement.

### Modified Capabilities
<!-- agent-registration will need host tags and may surface TAINTED state; the concrete proto/API
     changes are specified by the implementation slices that follow. -->

## Impact

- **`crates/master`**: experiment orchestrator + lifecycle state machine, connector abstraction
  (Prometheus first), blast-radius enforcement against live hosts (implies a heartbeat staleness
  sweep), in-memory experiment store, global kill-switch, and **write endpoints** (launch / watch /
  HALT) on the management plane.
- **`crates/proto`**: host tags on `Register`; fault command/report frames carrying master-minted
  `instance_id` (shared wire contract with `fault-plugin-model`).
- **`crates/cli`**: launch / observe / **HALT** experiments — **in v1** (the control surface is the
  product, not later tooling).
- **Sequencing**: depends on `fault-plugin-model` for the agent-side execution it drives, and on the
  shared wire contract being pinned first.
