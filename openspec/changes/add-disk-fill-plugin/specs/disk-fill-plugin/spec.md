# Spec: disk-fill Plugin

## Purpose

Defines `disk-fill`, the first host-mutating fault plugin: it consumes free space on a target
filesystem by allocating a single file, modelling a full disk. The blast radius is one file at
a deterministic path; revert is its removal. The plugin guarantees it can never fill a
filesystem to 100%.

## ADDED Requirements

### Requirement: disk-fill ships a fixed manifest and follows the plugin contract

The catalog SHALL include a plugin named `disk-fill`, version `1`, entrypoint `./disk-fill`,
`params_schema` of exactly `fill_dir: string` (an absolute directory path) and
`size_mib: int`, `requires` declaring no binaries, no privileges, and the single precondition
`fill_dir_writable`, and a `max_duration_secs` limit. The plugin SHALL be buildable from the
workspace (`crates/plugins/disk-fill`) with its `manifest.yaml` alongside, SHALL depend only on
the `faultforge-fault` contract crate (never its `proto` feature, no async runtime), and SHALL
implement all five lifecycle commands with the contract's stdin/stdout/exit-code protocol.

#### Scenario: Shipped manifest conforms to the contract

- **WHEN** the shipped `manifest.yaml` is parsed and validated by the shared contract crate
- **THEN** it SHALL parse without unknown fields and declare exactly the identity, entrypoint,
  params schema, requirements, and duration limit above

### Requirement: The fill file is the state tag at a deterministic id-derived path

The fault's entire host effect SHALL be one file at
`<fill_dir>/faultforge-<instance_id>.fill`. The path SHALL be recomputed from
`(instance_id, params)` alone on every invocation — no breadcrumb, no plugin-side state — so
that a cold-start `abort`/`cleanup` (journal replay) can revert without any memory of the
inject.

#### Scenario: Path is stable across invocations

- **WHEN** any two lifecycle commands run with the same `instance_id` and `fill_dir`
- **THEN** both SHALL resolve the same fill-file path, a direct child of `fill_dir`

### Requirement: preflight validates statically in install phase and probes the host in runtime phase

In both phases `preflight` SHALL fail with exit `10` (after a `log` event naming the reason)
when `size_mib < 1`, when the byte size `size_mib × 1048576` overflows checked 64-bit
arithmetic, or when `fill_dir` is not an absolute path. In the `runtime` phase it SHALL
additionally fail with exit `10` when `fill_dir` does not exist or is not a directory, when a
file already exists at the fill path (a leftover surfaces as a loud precondition failure
instead of a mid-experiment inject failure), when `fill_dir` is not writable by the current
uid — verified by an honest write probe, not by permission bits — or when the headroom
requirement (below) is not met. The probe SHALL be
un-leakable by construction: an anonymous `O_TMPFILE` file where the filesystem supports it,
otherwise an exclusively created probe file unlinked immediately after open, so a crashed
preflight leaves nothing named behind. In the `runtime` phase `preflight` SHALL also emit a
`warn`-level `log` event when the target filesystem is tmpfs (filling tmpfs consumes memory,
not disk); this SHALL NOT affect the exit code. On success it SHALL emit `status: PREFLIGHT`
and exit `0`. Apart from the transient probe, `preflight` SHALL NOT mutate the host in either
phase, and SHALL NOT create the fill file.

#### Scenario: Install phase is static only

- **WHEN** `preflight` runs with `phase: install` and well-formed params
- **THEN** it SHALL exit `0` without touching `fill_dir` at all

#### Scenario: Missing fill_dir is a runtime precondition failure

- **WHEN** `preflight` runs with `phase: runtime` and `fill_dir` does not exist
- **THEN** it SHALL emit a `log` event and exit `10`, and no fill file SHALL exist afterwards

#### Scenario: Unwritable fill_dir is a runtime precondition failure

- **WHEN** `fill_dir` exists but the probe-file write fails
- **THEN** `preflight` SHALL emit a `log` event and exit `10`

#### Scenario: Successful runtime preflight leaves no trace

- **WHEN** `preflight` succeeds with `phase: runtime`
- **THEN** it SHALL emit `status: PREFLIGHT`, exit `0`, and neither the probe file nor the
  fill file SHALL exist

#### Scenario: tmpfs target is warned about, not refused

- **WHEN** `preflight` runs with `phase: runtime` and `fill_dir` resides on tmpfs
- **THEN** it SHALL emit a `warn` `log` event naming the memory-not-disk consequence and
  otherwise proceed with the normal checks and exit code

### Requirement: The headroom guarantee prevents filling a filesystem to 100%

`preflight` (runtime phase) SHALL refuse with exit `10` unless the target filesystem reports
`available >= size_mib × 1048576 + headroom`, where `headroom = max(capacity / 20, 64 MiB)`
(5% of capacity with a 64 MiB floor). Availability SHALL be measured as blocks available to
unprivileged users (`f_bavail`), not total free blocks. The headroom SHALL NOT be
operator-configurable.

#### Scenario: Fill that would breach headroom is refused

- **WHEN** the filesystem has free space greater than `size_mib` but less than
  `size_mib + headroom`
- **THEN** `preflight` SHALL emit a `log` event naming the shortfall and exit `10`

#### Scenario: Fill within headroom is allowed

- **WHEN** available space is at least `size_mib + headroom`
- **THEN** `preflight` SHALL exit `0`

### Requirement: inject allocates the fill file and leaves nothing behind on failure

`inject` SHALL create the fill file with create-exclusive semantics and allocate exactly
`size_mib × 1048576` bytes, preferring `fallocate(2)`; when the filesystem does not support
`fallocate`, it SHALL fall back to appending zeroed chunks until the target size is reached,
announcing the fallback via a `log` event. Allocation SHALL be verified, not trusted: after
allocating by either mechanism, `inject` SHALL require the file's block usage
(`st_blocks × 512`) to be at least the requested byte size, and SHALL treat a shortfall as an
allocation failure — a sparse or unreserved file would make the fault a silent no-op. On
success it SHALL emit `status: INJECTING` then `status: ACTIVE` and exit `0`, the process
exiting immediately — the fault's active state SHALL be purely the allocated file's
existence. If the file already exists, `inject` SHALL emit a `log` event and exit `20`
without removing it. On any failure after creating the file (including `ENOSPC` from the
preflight-to-inject race and the block-usage verification), it SHALL remove the partial file
before exiting `20`, so that exit `20` always means the host is unaffected.

#### Scenario: Successful allocation

- **WHEN** `inject` succeeds
- **THEN** the fill file SHALL exist at the deterministic path with exactly the requested byte
  size and with block usage covering it, and stdout SHALL contain `INJECTING` followed by
  `ACTIVE` status events

#### Scenario: Unbacked allocation is a failure, not a silent no-op

- **WHEN** the filesystem reports allocation success but the file's block usage does not
  cover the requested size
- **THEN** `inject` SHALL emit a `log` event, remove the file, and exit `20`

#### Scenario: Allocation failure removes the partial file

- **WHEN** allocation fails partway (e.g. the filesystem runs out of space mid-fill)
- **THEN** `inject` SHALL emit a `log` event and exit `20`, and no fill file (partial or
  otherwise) SHALL remain

#### Scenario: Pre-existing fill file is refused, not clobbered

- **WHEN** a file already exists at the fill path when `inject` runs
- **THEN** `inject` SHALL emit a `log` event and exit `20`, and the existing file SHALL be
  untouched

### Requirement: report is a read-only existence probe

`report` SHALL emit `status: ACTIVE` when the fill file exists and `status: DONE` when it does
not, exiting `0` in both cases, and SHALL NOT create, modify, or remove anything. Ownership is
established by the id-derived filename; `report` SHALL NOT inspect file content.

#### Scenario: Present fill file reports ACTIVE

- **WHEN** `report` runs while the fill file exists
- **THEN** it SHALL emit `status: ACTIVE` and exit `0`

#### Scenario: Absent fill file reports DONE

- **WHEN** `report` runs and the fill file does not exist
- **THEN** it SHALL emit `status: DONE` and exit `0`

### Requirement: abort and cleanup remove the fill file idempotently

`abort` and `cleanup` SHALL remove the fill file at the deterministic path, treating an
already-absent file as success (`abort` emits `status: RECOVERING` then `status: DONE`;
`cleanup` emits `status: DONE`), and exit `0`. A real removal failure (any error other than
absence) SHALL emit a `log` event and exit `30`, leaving retry-then-taint to the agent
runtime. Neither command SHALL touch any other path.

#### Scenario: Cleanup twice in a row is success twice

- **WHEN** `cleanup` runs twice consecutively
- **THEN** both invocations SHALL exit `0` and the fill file SHALL NOT exist afterwards

#### Scenario: Abort removes the fill file

- **WHEN** `abort` runs while the fill file exists
- **THEN** the file SHALL be removed and the command SHALL exit `0`

#### Scenario: Real removal failure is exit 30

- **WHEN** removal fails for a reason other than the file being absent
- **THEN** the command SHALL emit a `log` event and exit `30`

### Requirement: disk-fill's host effects are confined to the fill directory

Across all five commands, the plugin SHALL create or remove only the fill file and the
transient preflight probe, both residing directly in `fill_dir` (the probe is anonymous or
immediately unlinked). It SHALL NOT write outside `fill_dir`, SHALL NOT follow the composed
path out of `fill_dir` (path traversal), and SHALL NOT spawn processes or touch host
services.

#### Scenario: Filesystem effects are confined to fill_dir

- **WHEN** any lifecycle command runs
- **THEN** every path the plugin creates or removes SHALL be a direct child of `fill_dir`
