# Proposal: fault-e2e-harness

## Why

Steps 1–3 are each tested against in-process fakes (fake master for the agent runtime, fake
agents for master dispatch). Nothing yet proves the *real* binaries — master, agent, plugins,
CLI — running as deployed processes with real process boundaries, real restarts, and a real
network between them. The safety properties FaultForge sells (journal replay after an agent
crash, master-loss self-abort, dead-man backstop, taint quarantine) are exactly the ones fakes
cannot fully prove. This change is step 4 of the roadmap: a fully automated end-to-end suite on
rootless podman containers, driven from Rust, identical locally (macOS via `podman machine`) and
in Linux CI.

## What Changes

- **New dev-only workspace crate `crates/e2e`** (`faultforge-e2e`, never published): integration
  tests that build container images, orchestrate master + agent containers over a dedicated
  podman network, and assert through the operator surface (CLI + management API) and host-side
  effects (marker files, journal, taint file via `podman exec`).
- **Container images from multi-stage Containerfiles** (Rust builder → slim runtime), so the
  suite works unchanged on macOS hosts (no cross-compilation): a master image and an agent image
  with the **baked-in catalog** — `noop-marker` plus a `hang-cleanup` fixture plugin whose
  cleanup blocks, added to force the dead-man path.
- **Podman driven via its CLI** from Rust (no docker daemon, no testcontainers dependency);
  rootless throughout. Per-run unique names for network/containers/volumes and teardown that
  runs even when a scenario fails, dumping container logs on failure.
- **Agent journal/data on a named volume** so an agent container can be killed and restarted
  against the same `data_dir` — the journal-replay scenario as a real crash, not a simulated one.
- **The agent container runs unprivileged**, so the taint scenario can break cleanup with
  `chmod 000` from a root `podman exec` (root in the container would bypass the permission
  check via `CAP_DAC_OVERRIDE`).
- **Scenario coverage**: happy path (`COMPLETED`, marker created then removed); operator halt
  (`ABORTED`); agent crash + restart mid-`ACTIVE` (journal replay → `ABORTED`, host clean);
  master-loss self-abort (master stopped; host recovers with no master); dead-man backstop
  (`hang-cleanup` → `ERROR`); `TAINTED` via `chmod 000` (experiment `ERROR`, new experiments
  refused, `agents clear-taint` restores schedulability).
- **Opt-in execution**: e2e tests are `#[ignore]` by default, so `cargo test --workspace` stays
  green without podman; the suite runs via `cargo test -p faultforge-e2e -- --ignored`.
- **Separate, non-blocking CI workflow** (`workflow_dispatch`) running the suite on a Linux
  runner with rootless podman — it does not gate PRs (owner decision; can be promoted later).

## Capabilities

### New Capabilities

- `fault-e2e-harness`: the end-to-end verification harness — containerized topology, image
  contents, isolation/teardown rules, the mandatory scenario set, operator-surface-driven
  assertions, and opt-in execution semantics.

### Modified Capabilities

- `continuous-integration`: gains a manually triggered, non-blocking e2e workflow alongside the
  existing PR-gating pipeline.

## Impact

- **`crates/e2e`** — new crate: podman wrapper module, image build, topology fixture, scenario
  tests. Depends on `reqwest`/`serde_json` (dev), shells out to `podman` and to the host-built
  `faultforge` CLI binary.
- **Containerfiles** — `docker/` (or `containers/`) directory: `Containerfile.master`,
  `Containerfile.agent`, fixture plugin sources for `hang-cleanup`.
- **`.github/workflows/`** — new `e2e.yml` (`workflow_dispatch`); the existing gating workflow
  is untouched.
- **No production crate changes expected.** The suite consumes master-fault-dispatch's surface;
  any friction it finds becomes small follow-up deltas, not silent workarounds.
- **Prerequisites** — depends on `master-fault-dispatch` (experiments, halt, clear-taint, CLI).
  Local runs need podman ≥ 4 (macOS: `podman machine` started).
