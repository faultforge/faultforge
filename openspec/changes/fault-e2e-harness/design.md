# Design: fault-e2e-harness

## Context

Step 4, the last of the roadmap fixed in `implement-fault-schema`. Everything the suite
exercises exists after step 3: the agent runtime (journal, safety triad, taint), master dispatch
(experiments, kill-switch, clear-taint), the management API, and the CLI. The harness's job is
to prove those pieces compose as *deployed processes* — separate containers, a real network,
real crashes — and to stay runnable by one command on a macOS laptop and a Linux CI runner
alike.

Constraints: rootless podman only (no docker daemon, no root on the host); `cargo test
--workspace` must remain green on machines without podman; CONVENTIONS.md applies to the
harness code itself (it is Rust in the workspace).

## Goals / Non-Goals

**Goals:**

- One command runs the full scenario set against freshly built images:
  `cargo test -p faultforge-e2e -- --ignored`.
- Every scenario asserts through operator-visible surfaces (CLI exit codes/output, management
  API) *and* host-side ground truth (marker file, journal dir, taint file via `podman exec`).
- Deterministic isolation: unique names per run, teardown on success and failure, logs dumped on
  failure.

**Non-Goals:**

- Not a performance or soak suite; scenarios use second-scale timings.
- No multi-agent fleet topologies beyond what scenarios need (single agent suffices; the fleet
  dimension is covered by step-3 integration tests with fake agents).
- No testing of deferred features (auth/TLS, dynamic distribution, metrics) and no Windows.
- Does not replace the in-process integration tests — it complements them at the process
  boundary.

## Decisions

### D1. A dev-only workspace crate driving the podman CLI directly

`crates/e2e` (`faultforge-e2e`) holds a small `podman.rs` wrapper (spawn `podman` via
`std::process::Command`, typed helpers for `build`/`run`/`exec`/`stop`/`kill`/`start`/`rm`/
`network`/`volume`, stdout/stderr captured into diagnostics) and a `topology.rs` fixture that
owns one scenario's resources and tears them down in `Drop`.

- *Alternative — testcontainers-rs*: rejected; its podman support rides the docker-compat
  socket, which is exactly the moving part that differs between macOS `podman machine` and
  rootless Linux CI. Shelling out to the `podman` CLI is the one interface guaranteed identical
  in both environments, and the wrapper is a few hundred lines the project fully controls.
- *Why a crate, not `tests/` in an existing crate*: the harness depends on nothing in the
  production crates (it drives binaries), and no production crate should grow podman machinery
  in its dev-deps.

### D2. Multi-stage Containerfiles; images built by the suite, not by hand

`Containerfile.master` and `Containerfile.agent` share a builder stage (pinned Rust image
matching `rust-toolchain.toml`) that runs `cargo build --release -p faultforge-master` /
`-p faultforge-agent`, then copy the binary into a slim runtime stage. Building *inside* the
container is what makes macOS hosts work — the host toolchain produces Mach-O, the container
needs Linux ELF, and cross-compilation setups are exactly the per-machine drift this harness
must not have. The suite builds both images once per run (a `Once`-guarded fixture step) with
`podman build`; layer caching makes repeat local runs cheap.

The agent image bakes the catalog under `/usr/lib/faultforge/plugins`: `noop-marker` (built in
the same builder stage) and the `hang-cleanup` fixture — a shell-script plugin whose `inject`
writes a marker and whose `cleanup` sleeps far past any test deadline (its manifest lives with
the Containerfiles; its digest is whatever the master's identical baked catalog computes).

### D3. The agent container runs unprivileged

The runtime stage creates an `agent` user owning `/var/lib/faultforge` and the marker workspace;
the agent process runs as that user. Reason: the taint scenario breaks cleanup by
`podman exec --user 0 chmod 000` on the marker's parent directory — if the agent ran as
container-root it would bypass the permission check (`CAP_DAC_OVERRIDE`) and cleanup would
succeed. Unprivileged-agent also matches the least-privilege posture the noop catalog actually
needs; plugins requiring root arrive with the real catalog and can revisit this.

### D4. Topology: one network, one master, one agent, journal on a named volume

Each scenario gets `ff-e2e-<run>-<scenario>`-prefixed network, containers, and volume. Master
publishes only the management port to `127.0.0.1` on the host (gRPC stays network-internal —
agents reach it by container DNS name; the CLI and reqwest reach only the management plane,
mirroring the two-plane architecture). The agent mounts the named volume at
`/var/lib/faultforge` so `podman kill` + `podman start` is a real crash-with-persistent-state.
Timings are compressed via env config: `FAULTFORGE_HEARTBEAT_INTERVAL_SECS=1` (master),
`FAULTFORGE_MASTER_LOSS_THRESHOLD_SECS=5` (agent), experiment durations of 2–10s, master
`default_grace_secs` small; every wait in the harness is a bounded poll, never a bare sleep.

### D5. Scenarios assert through the operator surface plus host ground truth

The suite drives experiments with the host-built `faultforge` CLI (a `cargo` dev-dependency on
the binary's path via `CARGO_BIN_EXE`-style resolution from the e2e crate's build script, or
`cargo run -p faultforge` fallback) against the mapped management URL — so the CLI itself is
under test — and uses `reqwest` for fine-grained polling. Ground truth on the host side comes
from `podman exec` (marker present/absent, `instances/` empty, `tainted.json` present) and
container lifecycle commands. The scenario set (normative list in the spec):

1. **Happy path** — `experiment run --wait` with `noop-marker` → exit 0 / `COMPLETED`; marker
   existed while `ACTIVE`, absent after; journal empty after.
2. **Operator halt** — long-duration run, `experiment halt` mid-`ACTIVE` → `ABORTED`, marker
   removed.
3. **Agent crash mid-`ACTIVE`** — `podman kill` the agent, `podman start` it (same volume) →
   journal replay aborts+cleans, instance `ABORTED` reported after re-register, marker gone,
   journal entry removed.
4. **Master-loss self-abort** — `podman stop` the master mid-`ACTIVE`; after the threshold the
   agent aborts on its own: marker gone, journal empty — asserted via `podman exec` with no
   master running; master restarted → agent re-registers with an empty `InstanceReport` (the
   experiment record died with the master, by design).
5. **Dead-man backstop** — `hang-cleanup` with small grace → duration stop hangs in cleanup,
   dead-man fires → instance `ERROR`, experiment `ERROR`.
6. **Taint and clear** — `chmod 000` the marker parent mid-`ACTIVE` → cleanup fails twice →
   `TaintStatus`, experiment `ERROR`, `agents show` reports `tainted: true`, a new experiment
   targeting the host is rejected at VALIDATE; `chmod 755` back, `agents clear-taint` →
   untainted, and a fresh experiment runs `COMPLETED`.

### D6. Opt-in execution; CI is a separate manual workflow

Scenario tests carry `#[ignore]` — `cargo test --workspace` on a podman-less machine compiles
the crate and runs nothing. The suite runs with `-- --ignored` (documented in README and the
crate). Missing/broken podman fails fast with an actionable message naming the command that
failed (a laptop that opted in wants a real error, not a silent skip).

CI: a new `e2e.yml` workflow on `workflow_dispatch` (owner decision: non-blocking, manual) —
ubuntu runner, rootless podman preinstalled, `cargo test -p faultforge-e2e -- --ignored`,
container logs uploaded as an artifact on failure. The existing gating workflow is untouched.
Promoting e2e to a PR gate later is a one-line trigger change plus a spec delta.

## Risks / Trade-offs

- **[Timing-sensitive scenarios can flake on slow runners]** → All waits are bounded polls
  against observable state (never sleep-and-assert); thresholds compressed but with wide
  margins (e.g. 5s threshold vs 1s heartbeat); scenarios are independent, so a rerun is cheap.
  Flake reports feed timing fixes, not `#[allow]`-style muting.
- **[In-container `cargo build` is slow on first run / cold CI cache]** → Layer-cached locally;
  in CI the manual trigger makes minutes acceptable; a cargo-registry cache mount can be added
  inside the Containerfile if it hurts.
- **[Manual CI means regressions can land unnoticed]** → Accepted by owner decision; mitigations
  are the strong in-process suites gating every PR, and the documented expectation of running
  e2e before a release/merge train. Promotion path is recorded (D6).
- **[`hang-cleanup` fixture depends on runtime internals (invocation timeout vs grace)]** → The
  fixture pins grace ≪ hang ≪ invocation timeout in its scenario config, and asserts only the
  spec-level outcome (`ERROR` via dead-man), not internal event ordering.
- **[Rootless networking differs between macOS (VM) and Linux (slirp/pasta)]** → The harness
  publishes ports only on `127.0.0.1` and talks container-to-container by DNS name on the
  dedicated network — the two patterns that behave identically in both environments.

## Migration Plan

Purely additive: a new crate (ignored-by-default tests), a containers directory, and a new
non-gating workflow. Nothing existing changes; removing the crate/workflow would fully revert.

## Open Questions

- Should the images also feed a `docker-compose`/`podman kube` dev topology for manual play?
  Out of scope here; the Containerfiles make it cheap later.
- When the first stateful plugin (`tc`/`iptables`) lands with its watchdog, the suite needs a
  privileged-agent variant and network-namespace assertions — revisit topology (D3/D4) then.
- Whether CI should also run e2e on a schedule (nightly) as a middle ground before gating —
  decide after a few weeks of manual-trigger experience.
