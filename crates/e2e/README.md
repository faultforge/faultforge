# faultforge-e2e

The end-to-end verification harness (roadmap step 4). It builds container images
and drives the **real** `faultforge-master`, `faultforge-agent`, and `faultforge`
CLI binaries as separate rootless-podman containers — proving the safety
properties that in-process fakes cannot: journal replay after a real agent crash,
master-loss self-abort, the dead-man backstop, and taint quarantine.

This crate is **dev-only** (never published). Its scenarios live in
`tests/scenarios.rs` and are `#[ignore]` by default, so `cargo test --workspace`
stays green on machines without podman.

## Running

```bash
# macOS only: start the podman VM first.
podman machine start

# Run the full suite (builds images on first run; cached afterwards).
cargo test -p faultforge-e2e -- --ignored

# A single scenario, streaming logs:
cargo test -p faultforge-e2e -- --ignored --nocapture happy_path
```

Requirements: rootless **podman ≥ 4** (no docker daemon, no host root). If podman
is missing or broken, the suite fails fast with a diagnostic naming the failed
command rather than skipping silently. The same tests pass on macOS
(`podman machine`) and on Linux — images compile inside the builder stage, so no
cross-compilation setup is needed.

## What it does

- **Images** (`containers/Containerfile`): one shared `builder` stage compiles all
  binaries and assembles the plugin catalog **once**, so the catalog bytes — and
  therefore the plugin digest the master sends and the agent verifies — are
  identical across the master and agent images. The agent runs **unprivileged** so
  the taint scenario can break cleanup with a root `chmod 000`.
- **Topology** (`src/topology.rs`): each scenario gets a uniquely named network,
  master, agent, and data volume (`ff-e2e-<run>-<scenario>-*`). The master's
  management port is mapped to an ephemeral `127.0.0.1` port; the gRPC plane stays
  container-internal. Everything is torn down in `Drop` even on failure, dumping
  container logs first. Every wait is a bounded poll — never a bare sleep.
- **Scenarios** (`tests/scenarios.rs`): happy path, operator halt, agent crash +
  journal replay, master-loss self-abort, dead-man backstop, and taint + clear.
  Each asserts the outcome through the CLI / management API **and** host ground
  truth via `podman exec`.

## CI

The `.github/workflows/e2e.yml` workflow runs this suite on a Linux runner with
rootless podman. It is **manual** (`workflow_dispatch`) and **non-blocking** — it
does not gate pull requests. On failure it uploads each container's logs (written
to `$FF_E2E_LOG_DIR`) as a build artifact.
