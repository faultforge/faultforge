# Spec: noop-marker Plugin

## ADDED Requirements

### Requirement: noop-marker is the reference plugin with a fixed manifest

The catalog SHALL include a plugin named `noop-marker`, version `1`, entrypoint `./noop-marker`,
`params_schema` of exactly `marker_path: string` (an absolute path), `requires` declaring no
binaries, no privileges, and the single precondition `marker_dir_writable`, and
`max_duration_secs: 600`. The plugin SHALL be buildable from the workspace
(`crates/plugins/noop-marker`) with its `manifest.yaml` alongside. The fault it injects SHALL be
solely the existence of a marker file at `marker_path`.

#### Scenario: Shipped manifest conforms to the contract

- **WHEN** the shipped `manifest.yaml` is parsed and validated by the shared contract crate
- **THEN** it SHALL parse without unknown fields and declare exactly the identity, entrypoint,
  schema, requirements, and duration limit above

### Requirement: preflight validates the path and directory writability without touching the marker

`preflight` SHALL fail with exit `10` (after emitting a `log` line naming the reason) when
`params.marker_path` is not an absolute path, or when `dirname(marker_path)` is not writable by
the current uid. It MAY create a missing `dirname(marker_path)`. Writability SHALL be verified by
an honest probe (creating and removing a temporary file), not by inspecting permission bits. On
success it SHALL emit `status: PREFLIGHT` and exit `0`. `preflight` SHALL NOT create or modify
the marker file in either phase.

#### Scenario: Relative marker path is a precondition failure

- **WHEN** `preflight` receives `marker_path` that is not absolute
- **THEN** it SHALL emit a `log` line and exit `10`, and no marker SHALL exist afterwards

#### Scenario: Unwritable marker directory is a precondition failure

- **WHEN** `dirname(marker_path)` exists but is not writable by the current uid
- **THEN** `preflight` SHALL emit a `log` line and exit `10`

#### Scenario: Successful preflight leaves no marker

- **WHEN** `preflight` succeeds in either phase (`install` or `runtime`)
- **THEN** it SHALL emit `status: PREFLIGHT`, exit `0`, and the marker file SHALL NOT exist

### Requirement: inject atomically writes the marker and exits immediately

`inject` SHALL write the marker file atomically: a temporary file in the same directory, fsynced,
then renamed to `marker_path`. The marker content SHALL be JSON with `instance_id`, `plugin`
(`"noop-marker@1"`), `deadline_unix`, `params`, and `written_at_unix`. On success it SHALL emit
`status: INJECTING` then `status: ACTIVE` and exit `0`, with the process exiting immediately — the
fault's active state SHALL be purely the file's existence. On any IO error it SHALL emit a `log`
line and exit `20`.

#### Scenario: Marker is written atomically with the contract content

- **WHEN** `inject` succeeds
- **THEN** `marker_path` SHALL contain parseable JSON with the instance id, plugin identity
  `noop-marker@1`, the deadline, the params, and the write timestamp, and stdout SHALL contain
  `INJECTING` followed by `ACTIVE` status lines

#### Scenario: Write failure aborts cleanly

- **WHEN** `inject` cannot write the marker (e.g. the directory becomes unwritable)
- **THEN** it SHALL emit a `log` line and exit `20`, and no partial marker file SHALL remain at
  `marker_path`

### Requirement: report is a read-only reconciliation probe

`report` SHALL read `marker_path` and mutate nothing: if the file is present and parseable it
SHALL emit `status: ACTIVE` and exit `0`; if the file is absent it SHALL emit `status: DONE` and
exit `0`; if the file is present but unparseable it SHALL emit a `log` line and exit non-zero so
the agent treats the instance as ambiguous.

#### Scenario: Present marker reports ACTIVE

- **WHEN** `report` runs while a valid marker exists
- **THEN** it SHALL emit `status: ACTIVE`, exit `0`, and the marker SHALL be byte-identical
  afterwards

#### Scenario: Absent marker reports DONE

- **WHEN** `report` runs and the marker file does not exist
- **THEN** it SHALL emit `status: DONE` and exit `0`

#### Scenario: Corrupt marker is ambiguous

- **WHEN** `report` runs and the marker file exists but is not parseable JSON
- **THEN** it SHALL emit a `log` line, exit non-zero, and SHALL NOT modify or remove the file

### Requirement: abort and cleanup remove the marker; cleanup is idempotent

`abort` SHALL remove `marker_path` immediately, emitting `status: RECOVERING` then
`status: DONE` and exiting `0`; it MAY be implemented as the same revert logic as `cleanup`.
`cleanup` SHALL remove `marker_path` and SHALL be idempotent: an already-absent marker is success
(`status: DONE`, exit `0`), safe to re-issue any number of times, including by the watchdog after
an agent restart. Only a real failure to remove an existing file SHALL exit `30`.

#### Scenario: Cleanup twice in a row is success twice

- **WHEN** `cleanup` runs once (removing the marker) and then runs again
- **THEN** both invocations SHALL exit `0` and the second SHALL emit `status: DONE` without
  error

#### Scenario: Abort removes the marker via the fast path

- **WHEN** `abort` runs while the marker exists
- **THEN** the marker SHALL be gone afterwards and stdout SHALL contain `RECOVERING` then `DONE`

#### Scenario: Real removal failure is exit 30

- **WHEN** `cleanup` runs while the marker exists but its directory forbids removal (e.g. mode
  `000`)
- **THEN** it SHALL emit a `log` line and exit `30`

### Requirement: noop-marker can never harm a host

The plugin SHALL require no root, open no network connections, signal no processes, and touch no
cgroups. The only filesystem paths it writes SHALL be within `dirname(marker_path)`: the marker
itself, the temporary file used for its atomic write, and the preflight probe file. Every
command SHALL be safe to run repeatedly on a production host.

#### Scenario: Filesystem effects are confined to the marker directory

- **WHEN** any lifecycle command of `noop-marker` runs
- **THEN** no path outside `dirname(marker_path)` SHALL be created, modified, or removed
