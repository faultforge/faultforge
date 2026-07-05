## Purpose

Defines the end-to-end test harness that validates the full fault-injection lifecycle by running
the real `faultforge-master` and `faultforge-agent` binaries in rootless podman containers,
orchestrated from Rust integration tests. The suite exercises safety properties, crash recovery,
operator flows, and isolation guarantees without requiring a docker daemon or host-root privileges.

## Requirements

### Requirement: The e2e suite runs the real binaries in rootless podman containers

The end-to-end suite SHALL run the real `faultforge-master` and `faultforge-agent` binaries as
separate containers on a dedicated podman network, orchestrated from Rust tests in a dev-only
workspace crate. Containers SHALL be built by the suite from multi-stage Containerfiles
(compilation inside the builder stage), so the suite runs identically on macOS hosts (via
`podman machine`) and on Linux, with rootless podman and no docker daemon. The agent image SHALL
bake the plugin catalog (including `noop-marker` and a `hang-cleanup` fixture plugin) at the
standard `plugin_root`, and the master image SHALL carry the identical catalog for digest
agreement.

#### Scenario: Suite builds and runs without a docker daemon

- **WHEN** the suite runs on a machine with only rootless podman available
- **THEN** images build, containers run, and all scenarios execute without any docker daemon or
  host-root privileges

#### Scenario: Same suite on macOS and Linux

- **WHEN** the suite runs on a macOS host with a started `podman machine` or on a Linux host
- **THEN** the same tests pass without host-specific configuration or cross-compilation setup

### Requirement: Scenario resources are isolated and always torn down

Each scenario SHALL use uniquely named containers, network, and volumes so concurrent or
repeated runs cannot collide. Teardown SHALL remove every created resource even when the
scenario fails, and on failure the suite SHALL capture container logs into test diagnostics
before removal.

#### Scenario: Failed scenario leaves nothing behind

- **WHEN** a scenario assertion fails mid-run
- **THEN** the suite SHALL dump the master and agent container logs into the test output and
  SHALL still remove the scenario's containers, network, and volumes

### Requirement: The agent's data directory lives on a volume that survives container restart

The agent container SHALL mount a named volume at its `data_dir` so that killing and restarting
the agent container preserves the instance journal and taint record, making crash-recovery
scenarios exercise the real journal replay path.

#### Scenario: Journal survives an agent crash

- **WHEN** the agent container is killed while an instance journal entry exists and is then
  started again with the same volume
- **THEN** the restarted agent SHALL find the journal entry and run replay recovery

### Requirement: Operator flows are driven through the CLI and management API

Scenarios SHALL drive experiments and taint clearing through the operator surface — the
`faultforge` CLI binary and the master's management API on a host-mapped port — and SHALL
assert host-side ground truth (marker files, journal directory, taint record) via `podman exec`
in the agent container. The gRPC agent plane SHALL NOT be exposed to the host; agents reach the
master over the container network.

#### Scenario: CLI is part of the system under test

- **WHEN** a scenario runs an experiment
- **THEN** it SHALL submit it via the `faultforge` CLI (or the management API) against the
  mapped management address, and the CLI's exit code and output SHALL be part of the assertions

### Requirement: The suite covers the six core fault-lifecycle scenarios

The suite SHALL include at least these scenarios, each asserting the experiment outcome through
the operator surface and the host state via `podman exec`:

1. **Happy path** — a `noop-marker` experiment ends `COMPLETED`; the marker exists during
   `ACTIVE` and is removed after; the journal directory is empty afterwards.
2. **Operator halt** — halting a running experiment yields `ABORTED` with the host cleaned up.
3. **Agent crash mid-ACTIVE** — the agent container is killed and restarted with the same data
   volume; journal replay recovers the host, the instance is reported `ABORTED` after
   re-registration, and the journal entry is removed.
4. **Master-loss self-abort** — the master container is stopped mid-`ACTIVE`; after the
   configured threshold the agent recovers the host with no master present; after the master
   restarts, the agent re-registers with an empty `InstanceReport`.
5. **Dead-man backstop** — a `hang-cleanup` fault forces the dead-man deadline; the instance and
   experiment end `ERROR`.
6. **Taint and operator clear** — breaking cleanup via `chmod 000` on the marker's parent
   directory taints the host (experiment `ERROR`, `tainted: true` visible, new experiments
   targeting the host rejected); restoring permissions and issuing `agents clear-taint` untaints
   the host and a subsequent experiment ends `COMPLETED`.

#### Scenario: Suite fails when a safety property regresses

- **WHEN** any of the six scenarios' expected outcome or host ground truth does not hold
- **THEN** the corresponding test SHALL fail with the captured container logs in its output

### Requirement: E2E execution is opt-in and cannot break podman-less builds

E2E scenario tests SHALL be excluded from default test runs (`#[ignore]`), so
`cargo test --workspace` succeeds on machines without podman; the suite SHALL run via an
explicit documented invocation (`cargo test -p faultforge-e2e -- --ignored`). When explicitly
invoked without a working podman, the suite SHALL fail fast with a diagnostic naming the failed
podman command rather than skipping silently.

#### Scenario: Workspace tests pass without podman

- **WHEN** `cargo test --workspace` runs on a machine without podman
- **THEN** the e2e crate compiles, its scenarios are skipped as ignored, and the run succeeds

#### Scenario: Explicit run without podman fails loudly

- **WHEN** the suite is invoked with `-- --ignored` and podman is unavailable
- **THEN** the tests SHALL fail with an error naming the podman invocation that failed
