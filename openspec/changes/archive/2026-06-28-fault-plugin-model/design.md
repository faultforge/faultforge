## Context

Slice 1 gives us an agent that dials the master and holds one persistent bidirectional gRPC
`Session` stream, keyed by `Hostname`. There is no fault capability. This change designs the
agent/plugin half of the fault model, **scoped to v1**. Governing decisions and rationale:
[ADR-0002](../../../docs/adr/0002-fault-model-decisions.md). No code is written in this change.

## Goals / Non-Goals

**Goals:**
- A plugin format auditable without execution and expressive enough for arbitrary faults.
- Integrity-checked, cache-friendly distribution whose dynamic-pull form can arrive later.
- A lifecycle that is always abortable and always recoverable, even if the agent dies.
- A clear unit of execution (the fault instance) and a supervisor model for the agent.

**Non-Goals (deferred for v1 with compensating controls — see ADR-0002 "known gaps"):**
- Experiment orchestration, targeting, metric evaluation, outcome verdicts (→ `experiment-model`).
- **Dynamic plugin distribution** (the `FetchArtifact` transfer + master artifact store): v1 bakes
  the vetted catalog into the agent package; the wire shape is retained for v2.
- **Auth/TLS, audit log, full cgroup confinement, plugin signing, marketplace.**

## Decisions

### Plugin = manifest + executable (ADR-0002 §1, §2)
A plugin is a directory containing a `manifest.yaml` and an executable `entrypoint`. The manifest is
the machine-readable contract. Note: the integrity **digest is not stored in the manifest body**
(a file cannot hash itself); it is an external reference covering manifest+executable as a unit
(ADR-0002 §3).

```yaml
name: kill-process
version: 1
entrypoint: ./kill-process
params_schema:
  process: string
requires:
  binaries: [systemctl]
  privileges: [root]
  preconditions: [target_is_restartable_unit]   # per-fault preflight gate
max_duration_secs: 600           # longest the plugin can safely self-revert within
```

### Distribution: content-addressed model, baked-in catalog for v1 (ADR-0002 §3, §4)
Integrity is a `sha256` over manifest+executable as a unit, referenced by the experiment and the
catalog. v1 ships the catalog **baked into the agent package**; the agent verifies the cached
plugin's digest before running. The `FetchArtifact(digest) -> stream Chunk` RPC shape is kept in the
proto but **not implemented** in v1 — dynamic pull (agent fetches a missing plugin from the master)
is v2, gated on signing + auth.

```
v1 flow:
  1. An action references a plugin name+version+digest.
  2. Agent resolves it in the baked-in catalog and verifies the on-disk digest.
  3. Digest mismatch ⇒ fail preflight; never run an unverified artifact.
```

### Lifecycle state machine (ADR-0002 §6, §7, §10)
Per fault instance:

```
 PENDING ─preflight ok─▶ PREFLIGHT ─inject─▶ INJECTING ─ok─▶ ACTIVE
                            │ fail               │ fail           │ duration / stop / silence
                            ▼                    ▼                ▼
                         ABORTED ◀────abort──────┴───────▶ RECOVERING ─ok─▶ DONE
                            │                                   │ recovery fail
                            ▼                                   ▼
                     (host unaffected)                    host TAINTED (ERROR)
```

- **Two preflights** (static at install; runtime before injection) **plus per-fault preconditions**
  (e.g. `kill-process` requires the target to be a restartable service unit).
- **`abort` vs `cleanup`** are distinct; `cleanup` is **idempotent** so it can be re-issued,
  including by the watchdog after an agent restart.

### One agent-side safety-timer model (ADR-0002 §5)
The agent runs a single timer keyed off "the deadline or the silence, whichever comes first":
```
duration                authoritative active lifetime; normal end = graceful stop + recovery
loss of master > T      agent self-aborts (does not run blind to the control plane)
dead-man = duration+grace   backstop; force recovery, instance ERROR, if nothing else fired
max_duration_secs       validation cap: duration <= max_duration_secs
```
This replaces the previously separate dead-man timer and connection-loss self-abort, whose
precedence was undefined.

### Recovery is concrete per fault and survives agent death (ADR-0002 §6)
- **`tc`/`iptables`**: revert owned by a **separate watchdog process / scheduled job**, not the
  plugin process (`tc` has no native TTL). Neither killing the plugin nor killing the agent may
  strand the change.
- **`kill-process`**: recovery = service supervisor restarts the process; *target is a restartable
  unit* is a preflight precondition.
- **Resource faults**: cgroup teardown / bounded-file removal.
- **Agent-side instance journal** (`instance_id`, digest, params, deadline → `/var/lib/faultforge`):
  a restarted agent re-attaches the watchdog and completes recovery. This is the host-side safety
  net that makes deferring master persistence acceptable.

### Fault instance as unit; agent as supervisor; concurrent per host (ADR-0002 §8)
A fault instance = one plugin + one param set + one lifecycle, identified by `instance_id`. The
agent supervises a set of instances; a host matching multiple actions runs **multiple concurrent
instances by design**. v1 implements **no `conflicts_with`** — interference between concurrent
instances is the operator's responsibility. All reports carry `instance_id`.

### Inject reconciliation; master-minted `instance_id` (ADR-0002 §12)
The master never re-issues `inject`. `instance_id` is **minted by the master**, deterministic per
`(experiment, agent, action)`, and carried in `inject`, so the master can correlate a reported
instance to the command it issued. After a brief stream blip the agent reports current state and
the master accepts it; ambiguous state ⇒ the agent aborts.

### Failure handling (ADR-0002 §9)
Recovery failure after the idempotent retry ⇒ host `TAINTED`, excluded from new experiments,
operator alerted; cleared by a human.

### Reporting
A plugin emits structured status transitions and log lines; the agent tags each with `instance_id`
and forwards to the master over `Session`.

## Risks / Trade-offs

- **Foreign code as root, no confinement in v1** (ADR-0002 §18, known gaps): bounded by digest
  integrity + first-party catalog; full cgroup confinement deferred. Resource-exhaustion faults can
  starve the agent — mitigated only by agent OS priority in v1.
- **No auth in v1** (known gaps): the agent accepts commands without authenticating the master —
  the Chaotic Deputy vector. Conscious WIP deferral; must close before untrusted-network prod use.
- **Concurrent instances can interfere** (no `conflicts_with`): operator responsibility.
- **Agent journal is the only durable fault state**: master persistence is deferred; the journal +
  watchdog must carry host recovery across agent and master restarts.
