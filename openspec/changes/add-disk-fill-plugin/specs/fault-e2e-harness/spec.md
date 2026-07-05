# Delta: fault-e2e-harness — real-fault (disk-fill) coverage

## ADDED Requirements

### Requirement: The suite covers a real host-mutating fault end to end

Beyond the noop-marker lifecycle scenarios, the suite SHALL drive the `disk-fill` plugin — a
fault whose injection genuinely mutates the host filesystem — through the operator surface,
asserting host ground truth via `podman exec` against the agent container. The `disk-fill`
binary and manifest SHALL be installed into the container image's plugin catalog by the same
image build (`containers/Containerfile`) that installs `noop-marker`. The scenarios SHALL
cover at least:

1. **Happy path**: an experiment fills a directory on the agent's data volume; while the
   instance is `ACTIVE` the fill file exists at its deterministic path with exactly the
   requested byte size; after `COMPLETED` (CLI `--wait` exit `0`) the file is gone.
2. **Operator halt**: halting the experiment while `ACTIVE` ends it `ABORTED` and removes the
   fill file.
3. **Preflight refusal**: a `size_mib` far beyond the container filesystem's free space ends
   the experiment `ABORTED` (CLI `--wait` exit `4`) and the fill file never existed.
4. **Crash replay**: killing and restarting the agent container while `ACTIVE` replays the
   journal and removes the fill file.

#### Scenario: Fill file is host ground truth for the active window

- **WHEN** the happy-path scenario observes the instance `ACTIVE`
- **THEN** `podman exec` SHALL confirm the fill file exists with exactly
  `size_mib × 1048576` bytes, and after the experiment reports `COMPLETED` it SHALL confirm
  the file is absent

#### Scenario: Refused preflight leaves the host untouched

- **WHEN** the preflight-refusal scenario completes
- **THEN** the experiment SHALL end `ABORTED` with CLI exit `4` and `podman exec` SHALL
  confirm no fill file was ever created

#### Scenario: Replay reverts the real fault

- **WHEN** the agent container is killed while a disk-fill instance is `ACTIVE` and then
  restarted
- **THEN** journal replay SHALL remove the fill file and the experiment SHALL reach a
  terminal outcome
