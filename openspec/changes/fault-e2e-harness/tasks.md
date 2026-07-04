# Tasks: fault-e2e-harness

## 1. Crate and podman plumbing

- [ ] 1.1 New dev-only crate `crates/e2e` (`faultforge-e2e`) in the workspace; no production
      crate deps; `reqwest`/`serde_json` dev-deps
- [ ] 1.2 `podman.rs`: typed CLI wrapper (`build`, `run`, `exec`, `stop`, `kill`, `start`,
      `rm`, `network create/rm`, `volume create/rm`, `logs`) capturing stdout/stderr; fail-fast
      diagnostic naming the podman command when unavailable
- [ ] 1.3 `topology.rs`: per-scenario fixture — unique `ff-e2e-<run>-<scenario>` names, network
      + master + agent + data volume, management port mapped to `127.0.0.1`, gRPC internal-only;
      teardown in `Drop` with log dump on failure; bounded-poll helpers (no bare sleeps)

## 2. Images and fixtures

- [ ] 2.1 `Containerfile.master` / `Containerfile.agent`: shared builder stage pinned to the
      `rust-toolchain.toml` channel, release build, slim runtime stage
- [ ] 2.2 Agent image: unprivileged `agent` user owning `data_dir` and the marker workspace;
      baked catalog at `/usr/lib/faultforge/plugins` with `noop-marker` and the new
      `hang-cleanup` fixture plugin (inject writes marker; cleanup blocks); master image carries
      the identical catalog at its `catalog_root`
- [ ] 2.3 Once-guarded image build step in the suite (`podman build`); compressed-timing env
      wiring (heartbeat 1s, master-loss threshold 5s, small grace)

## 3. Scenarios

- [ ] 3.1 Happy path: CLI `experiment run --wait` (noop-marker) → exit 0 / `COMPLETED`; marker
      present during `ACTIVE`, absent after; `instances/` empty
- [ ] 3.2 Operator halt: long-duration run, CLI `experiment halt` mid-`ACTIVE` → `ABORTED`,
      marker removed
- [ ] 3.3 Agent crash mid-`ACTIVE`: `podman kill` + `podman start` (same volume) → replay
      recovery, instance `ABORTED` after re-register, marker gone, journal entry removed
- [ ] 3.4 Master-loss self-abort: `podman stop` master mid-`ACTIVE` → after threshold marker
      gone and journal empty (asserted via `podman exec`, no master running); master restarted →
      agent re-registers with empty `InstanceReport`
- [ ] 3.5 Dead-man backstop: `hang-cleanup` with small grace → instance and experiment `ERROR`
- [ ] 3.6 Taint + clear: root `podman exec chmod 000` on marker parent mid-`ACTIVE` → cleanup
      fails twice → experiment `ERROR`, `agents show` reports `tainted: true`, new experiment
      rejected at VALIDATE; `chmod 755` + CLI `agents clear-taint` → untainted; fresh experiment
      `COMPLETED`

## 4. Execution semantics and CI

- [ ] 4.1 `#[ignore]` on all scenarios; document the invocation
      (`cargo test -p faultforge-e2e -- --ignored`) in the crate and README, incl. macOS
      `podman machine` prerequisite
- [ ] 4.2 `.github/workflows/e2e.yml`: `workflow_dispatch`, ubuntu runner, rootless podman,
      runs the suite, uploads container logs as an artifact on failure; not a required check

## 5. Verification and docs

- [ ] 5.1 Full local run on macOS (`podman machine`) and one dispatched CI run — all six
      scenarios green; verify repeat runs collide with nothing and leave no resources behind
- [ ] 5.2 `cargo build --workspace && cargo test --workspace && cargo clippy --workspace
      --all-targets -- -D warnings && cargo fmt --check` stay green without podman
- [ ] 5.3 Update CLAUDE.md (e2e crate row, how to run the suite, non-blocking CI note) and
      README
