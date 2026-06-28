## Why

FaultForge can register agents and track heartbeats, but it cannot yet inject a single fault.
Before any orchestration exists, we need the **fault execution model**: how a fault is described,
distributed to the host that needs it, and run there with a lifecycle that is safe to abort and
guaranteed to clean up. This is the agent/plugin half of the system and has **no dependency on the
master-side experiment model** — it can be specified and built first. The cross-cutting rationale
lives in [ADR-0002](../../../docs/adr/0002-fault-model-decisions.md).

This change captures the **model and behaviour** only. Per the team's docs-first approach, no Rust
is written here; implementation is sliced into smaller changes later (see `tasks.md`).

## What Changes

- Define the **plugin manifest**: a machine-readable contract declaring identity
  (`name`, `version`), `entrypoint`, `params_schema`, a `requires` block (host binaries, privileges,
  minimum resources, per-fault preconditions), and `max_duration_secs`. The integrity digest is an
  **external** reference over manifest+executable as a unit, not a field inside the manifest.
- Define **distribution**: integrity is content-addressed by `sha256`; **v1 ships the vetted
  catalog baked into the agent package** and verifies the on-disk digest before running. The
  `FetchArtifact` RPC shape is reserved in the contract; dynamic pull is **v2**.
- Define the **plugin lifecycle contract**: `preflight` (static at install, runtime before
  injection, incl. per-fault preconditions) → `inject` → `report` → `abort | cleanup`, with `abort`
  and `cleanup` as distinct paths and `cleanup` idempotent.
- Define the **fault instance** as the unit of execution and the agent as a **multi-instance
  supervisor**: one instance per (agent, action), multiple concurrent instances allowed by design,
  **no `conflicts_with`** in v1 (operator responsibility). `instance_id` is **master-minted** and
  carried in `inject`.
- Define **one agent-side safety-timer model**: experiment `duration` is authoritative; loss of the
  master beyond a threshold triggers an **agent self-abort**; a dead-man deadline (`duration +
  grace`) is the backstop; `duration <= max_duration_secs` is enforced.
- Define **recovery concrete per fault, survivable of agent death**: `tc`/`iptables` reverts owned
  by a **separate watchdog**, `kill-process` recovery = service-supervisor restart (target must be a
  restartable unit, a preflight precondition), plus an **agent-side instance journal** so a
  restarted agent re-attaches its watchdog.
- Define **failure handling**: a host whose recovery fails after retry is marked `TAINTED` and
  excluded from new work.
- Define **reporting** and **inject reconciliation** (master never re-issues `inject`; reconcile by
  state report; ambiguous ⇒ abort).

Explicit non-goals for this change (owned elsewhere or deferred — see ADR-0002 "known gaps"):

- **No experiment orchestration, targeting, or metric evaluation** — those are the `experiment-model`
  change.
- **No concrete plugin implementations** — only the contract; v1 catalog is `kill-process` + network
  faults (resource-exhaustion faults carry a known self-starvation risk, ADR-0002 §18).
- **No dynamic plugin distribution** in v1 (baked-in catalog; `FetchArtifact` transfer is v2).
- **No auth/TLS, no audit log, no full cgroup confinement, no code signing** — consciously deferred
  WIP gaps; auth (master impersonation / Chaotic Deputy) must close before untrusted-network prod.
- **No durable master-side storage** — deferred; the agent journal is the host-side safety net.

## Capabilities

### New Capabilities
- `fault-plugin-model`: the manifest, content-addressed distribution, lifecycle contract,
  multi-instance supervision, self-revert/dead-man backstop, taint-on-cleanup-failure, and
  reporting that together let an agent run a single fault safely on its host.

### Modified Capabilities
<!-- agent-registration / agent-heartbeat are not changed by this model capture. The proto
     additions (FetchArtifact, fault command/report frames) will be specified by the
     implementation slices that follow. -->

## Impact

- **`crates/proto`**: reserved `FetchArtifact` RPC shape (not implemented in v1) and new `Session`
  payload frames for fault commands/reports carrying master-minted `instance_id` (specified with
  the wire-contract task before any slice is built).
- **`crates/agent`**: baked-in catalog + digest verification, manifest parsing, lifecycle
  supervisor, single safety timer, per-fault recovery + watchdog, instance journal, report
  forwarding.
- **`crates/master`**: mints `instance_id`; `TAINTED` host state in the registry. No artifact store
  in v1.
- **Sequencing**: this capability is foundational and independent of `experiment-model`; it can be
  implemented first. The wire contract (frames + `instance_id` provenance) must be pinned before
  either change is coded.
