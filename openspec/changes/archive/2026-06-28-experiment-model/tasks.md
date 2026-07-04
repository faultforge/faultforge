# Tasks

> This change captures the **model and behaviour** only (docs-first). No Rust is written here.
> The work below is the anticipated implementation breakdown into later, independently shippable
> slices — intentionally unchecked and unscheduled until the model is ratified. Scope reflects the
> v1 decisions in [ADR-0002](../../../docs/adr/0002-fault-model-decisions.md). Depends on
> `fault-plugin-model` and the shared wire contract.

## 1. Targeting foundation

- [ ] 1.1 Add host tags to `Register` and the registry; agent self-declares tags from config
- [ ] 1.2 Heartbeat **staleness sweep** so liveness is real (required by blast radius)
- [ ] 1.3 Surface tags and `TAINTED` state on the management API

## 2. Experiment definition + validation

- [ ] 2.1 Experiment schema (actions, blast_radius, sampling_interval, preconditions, guardrails,
      hypothesis) with versioning
- [ ] 2.2 VALIDATE: schema, plugin resolution (baked-in catalog), `duration <= max_duration_secs`,
      no `TAINTED` in scope
- [ ] 2.3 Blast-radius computation **against live hosts** + enforcement per target group and
      concurrency

## 3. Connectors + metric evaluation

- [ ] 3.1 Connector abstraction; Prometheus implementation (instant + range queries)
- [ ] 3.2 Rule evaluation with `tolerance`, `sustain`, and fail-safe-on-outage handling
- [ ] 3.3 Polling at `sampling_interval`; **hypothesis sampled during injection**, aggregated at
      VERDICT

## 4. Orchestrator + lifecycle

- [ ] 4.1 Experiment lifecycle state machine (DRAFT→VALIDATE→PRECONDITIONS→INJECTING→MONITORING→
      RECOVERING→VERDICT)
- [ ] 4.2 Single-salvo inject + recover-all
- [ ] 4.3 Global kill-switch (instance terminal failure, sustained guardrail breach, agent abort,
      connector outage, operator HALT)
- [ ] 4.4 Outcome lattice with **ERROR/TAINTED dominating ABORTED**; legible outcomes (host /
      instance / rule / value-vs-threshold / timestamp)
- [ ] 4.5 In-memory experiment store; master must not assert a verdict it can't back after restart

## 5. Write control surface (v1 — the product)

- [ ] 5.1 Management-plane write endpoints: launch / watch-live / **HALT**
- [ ] 5.2 HALT as the most-available operation (succeeds when connectors/metrics are down)
- [ ] 5.3 CLI: launch / observe / HALT

## 6. Dry-run

- [ ] 6.1 VALIDATE + PRECONDITIONS + runtime preflight + plugin-availability check, no injection
- [ ] 6.2 Report exact affected hosts and blocking issues

## 7. Verification

- [ ] 7.1 Unit tests for pure lifecycle transitions, rule evaluation, blast-radius math (live
      hosts), outcome lattice
- [ ] 7.2 Integration test: define → dry-run → run → force a guardrail breach → assert global
      kill-switch + correct outcome (ABORTED vs ERROR precedence)
- [ ] 7.3 POC: the flagship single-host drill (kill-process mysqld) end to end, incl. manual HALT
- [ ] 7.4 `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace`

## Deferred to later (recorded, not in this scope)

- [ ] Auth/TLS on both planes (agent-plane master authentication is the highest-priority gap) + audit log
- [ ] Staged / multi-stage scenarios; durable experiment storage / master HA
- [ ] Full RBAC / SSO / multi-tenancy; non-Prometheus connectors; web UI
