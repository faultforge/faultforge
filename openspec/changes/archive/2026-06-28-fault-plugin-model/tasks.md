# Tasks

> This change captures the **model and behaviour** only (docs-first). No Rust is written here.
> The work below is the anticipated implementation breakdown into later, independently shippable
> slices — intentionally unchecked and unscheduled until the model is ratified. Scope reflects the
> v1 decisions in [ADR-0002](../../../docs/adr/0002-fault-model-decisions.md).

## 1. Wire contract (must be pinned FIRST — owned jointly with `experiment-model`)

- [ ] 1.1 Define `Session` fault frames: inject / abort / cleanup commands and instance state / log
      reports, all carrying the master-minted `instance_id`
- [ ] 1.2 Define `instance_id` provenance: minted by master, deterministic per `(experiment, agent,
      action)`, carried in `inject`
- [ ] 1.3 Reserve the `FetchArtifact(digest) -> stream Chunk` RPC shape (NOT implemented in v1)
- [ ] 1.4 Define the manifest schema (versioned) and the external unit-digest scheme

## 2. Plugin catalog + integrity (v1: baked-in)

- [ ] 2.1 Bake the vetted v1 catalog (`kill-process` + network faults) into the agent package
- [ ] 2.2 Verify the on-disk plugin digest (over manifest+executable) before running; reject on
      mismatch
- [ ] 2.3 Manifest parsing + `params_schema` validation + static preflight
- [ ] 2.4 Add `TAINTED` host state to the registry and exclude tainted hosts from targeting

## 3. Agent: instance supervisor + lifecycle

- [ ] 3.1 Fault-instance state machine (PENDING→PREFLIGHT→INJECTING→ACTIVE→RECOVERING→DONE, plus
      ABORTED/ERROR)
- [ ] 3.2 Runtime preflight before injection, including per-fault preconditions (e.g. target is a
      restartable service unit for `kill-process`)
- [ ] 3.3 Distinct `abort` and `cleanup` paths; idempotent `cleanup` with retry
- [ ] 3.4 Single safety-timer model: duration end, master-loss self-abort, dead-man backstop;
      enforce `duration <= max_duration_secs`
- [ ] 3.5 Multi-instance supervision with master-minted `instance_id` (no `conflicts_with` in v1)
- [ ] 3.6 State reconciliation after stream interruption (report, do not re-inject; abort if
      ambiguous)

## 4. Recovery (concrete per fault, survives agent death)

- [ ] 4.1 Watchdog process/job owning `tc`/`iptables` reverts independent of the plugin process
- [ ] 4.2 `kill-process` recovery via service-supervisor restart
- [ ] 4.3 Agent-side instance journal (`instance_id`, digest, params, deadline → `/var/lib/faultforge`)
- [ ] 4.4 On agent restart: read journal, re-attach watchdog, complete recovery
- [ ] 4.5 Taint host on recovery failure + operator alert

## 5. Reporting

- [ ] 5.1 Capture plugin status transitions and logs, tag with `instance_id`, forward to master

## 6. Verification (per slice)

- [ ] 6.1 Unit tests for pure lifecycle transitions, timer model, manifest/param validation
- [ ] 6.2 Integration test: assign a catalog plugin, verify digest, inject, abort, recover
- [ ] 6.3 POC: kill the master while a fault is live; assert the host self-recovers via the timer +
      watchdog
- [ ] 6.4 `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace`

## Deferred to v2 (recorded, not in this scope)

- [ ] Dynamic plugin distribution: implement `FetchArtifact` streaming + master artifact store
- [ ] Full cgroup v2 confinement of fault instances; enable resource-exhaustion faults (cpu/mem/disk)
- [ ] Plugin signing (cosign/sigstore); third-party / marketplace plugins
