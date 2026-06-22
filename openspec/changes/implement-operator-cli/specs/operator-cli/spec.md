## ADDED Requirements

### Requirement: Master endpoint is configured by a single URL flag
The CLI SHALL accept a single global `--master-url` flag whose value is the full
base URL of the master's HTTP management API, including scheme, host, and port.
When the value uses the `https` scheme, the CLI SHALL connect over TLS without
any additional flag. When `--master-url` is not supplied, the CLI SHALL default
to `http://localhost:8069`. The flag SHALL apply identically to both one-shot and
interactive modes.

#### Scenario: Default endpoint when unset
- **WHEN** the CLI is run with no `--master-url` flag
- **THEN** the CLI SHALL target `http://localhost:8069` as the master management
  API base URL

#### Scenario: Explicit endpoint overrides the default
- **WHEN** the CLI is run with `--master-url http://10.0.0.5:9000`
- **THEN** the CLI SHALL send its requests to `http://10.0.0.5:9000` instead of
  the default

#### Scenario: HTTPS URL selects TLS
- **WHEN** the CLI is run with `--master-url https://master.example:8443`
- **THEN** the CLI SHALL connect to the master over TLS without requiring any
  additional flag

### Requirement: Mode is selected by presence of a subcommand and a TTY
The CLI SHALL run in one-shot mode when a subcommand is given, and SHALL launch
the interactive TUI when invoked with no subcommand on an interactive terminal.
When invoked with no subcommand and standard output is not a TTY, the CLI SHALL
NOT start the TUI; it SHALL print usage/help to standard error and exit with a
non-zero status.

#### Scenario: Subcommand runs one-shot
- **WHEN** the user runs `faultforge agents list`
- **THEN** the CLI SHALL execute the command once, write its result to standard
  output, and exit without entering the interactive interface

#### Scenario: Bare command on a TTY launches the TUI
- **WHEN** the user runs `faultforge` with no subcommand and standard output is an
  interactive terminal
- **THEN** the CLI SHALL launch the interactive TUI

#### Scenario: Bare command without a TTY does not hang
- **WHEN** the user runs `faultforge` with no subcommand and standard output is
  not a terminal (e.g. piped or redirected)
- **THEN** the CLI SHALL print help to standard error and exit with a non-zero
  status without launching the TUI

### Requirement: List agents in one-shot mode
The CLI SHALL provide an `agents list` subcommand that fetches all registered
agents from the master's `GET /agents` endpoint and writes them to standard
output. When the registry is empty, the command SHALL emit an empty result (an
empty JSON array in JSON output) and exit with a success status.

#### Scenario: Agents are listed as JSON by default
- **WHEN** the user runs `faultforge agents list` and the master returns one or
  more agents
- **THEN** the CLI SHALL write a JSON array to standard output, one object per
  agent including its hostname, name, and last-seen timestamp, and exit with a
  success status

#### Scenario: Empty registry yields an empty array
- **WHEN** the user runs `faultforge agents list` and the master returns no agents
- **THEN** the CLI SHALL write an empty JSON array to standard output and exit
  with a success status

### Requirement: Show a single agent in one-shot mode
The CLI SHALL provide an `agents show <hostname>` subcommand that fetches one
agent from the master's `GET /agents/{hostname}` endpoint. On success it SHALL
write the agent to standard output. When the master responds that no agent exists
for the hostname (HTTP 404), the CLI SHALL write a diagnostic to standard error
and exit with a non-zero status distinct from the transport-error status.

#### Scenario: Existing agent is shown
- **WHEN** the user runs `faultforge agents show web-01` and the master has an
  agent registered under `web-01`
- **THEN** the CLI SHALL write that agent's details to standard output and exit
  with a success status

#### Scenario: Unknown agent reports not found
- **WHEN** the user runs `faultforge agents show ghost-01` and the master returns
  `404 Not Found`
- **THEN** the CLI SHALL write a not-found message to standard error and exit with
  a non-zero status

### Requirement: Output format is JSON by default with a table option
One-shot commands SHALL emit JSON by default and SHALL accept a global
`-o/--output` flag selecting either `json` or `table`. JSON output SHALL be valid,
machine-parseable JSON suitable for scripting. Table output SHALL be a
human-readable table rendered to standard output.

#### Scenario: Default output is JSON
- **WHEN** the user runs `faultforge agents list` without an `--output` flag
- **THEN** the CLI SHALL produce JSON output

#### Scenario: Table output is selected explicitly
- **WHEN** the user runs `faultforge agents list -o table`
- **THEN** the CLI SHALL produce a human-readable table instead of JSON

### Requirement: Transport and protocol failures are reported with a non-zero exit
The CLI SHALL, when it cannot reach the master or receives an unexpected response
(other than a documented not-found), write a diagnostic message to standard error
and exit with a non-zero status. The CLI SHALL NOT print partial or malformed data
to standard output as if it were a successful result.

#### Scenario: Master is unreachable
- **WHEN** the user runs a one-shot command and the master cannot be reached at
  the configured `--master-url`
- **THEN** the CLI SHALL write an error describing the failure to standard error
  and exit with a non-zero status, writing nothing to standard output

### Requirement: Interactive TUI presents a four-region layout
The interactive TUI SHALL render four regions: a persistent top strip showing an
at-a-glance health overview of registered agents (name and last activity only); a
left menu listing available sections, with a single "Agents" section in this
slice; a right detail pane showing the selected section's content, which for the
Agents section SHALL be a list of agents with a status indicator, name, and a
human-relative last-seen age; and a bottom bar showing the keyboard shortcuts
available in the current view. The top strip SHALL remain visible regardless of
which menu section is selected.

#### Scenario: All four regions are rendered
- **WHEN** the interactive TUI is running
- **THEN** the CLI SHALL display the top health strip, the left menu, the right
  detail pane, and the bottom shortcut bar simultaneously

#### Scenario: Agents section lists agents with relative ages
- **WHEN** the Agents section is selected and the master has returned agents
- **THEN** the right detail pane SHALL list each agent with a status indicator,
  its name, and its last-seen time expressed as an age relative to the current
  time

### Requirement: Interactive TUI refreshes agent data by polling and on demand
The interactive TUI SHALL fetch agent data from the master on a recurring
interval while running, and SHALL also refresh immediately when the user presses
`r`. The fetch SHALL NOT block rendering or keyboard input. When a fetch fails,
the TUI SHALL keep running and surface the error without crashing.

#### Scenario: Periodic refresh
- **WHEN** the interactive TUI has been running for at least one polling interval
- **THEN** the CLI SHALL have re-fetched agent data from the master and updated
  the displayed agents

#### Scenario: Manual refresh
- **WHEN** the user presses `r` while the TUI is running
- **THEN** the CLI SHALL immediately initiate a fresh fetch of agent data

#### Scenario: Fetch failure does not crash the TUI
- **WHEN** a periodic or manual fetch fails (e.g. the master is briefly
  unreachable)
- **THEN** the TUI SHALL continue running and indicate the error rather than
  exiting

### Requirement: Interactive TUI can be exited from the keyboard
The interactive TUI SHALL exit when the user presses `q`, restoring the terminal
to its normal state on exit.

#### Scenario: Quit restores the terminal
- **WHEN** the user presses `q` while the TUI is running
- **THEN** the CLI SHALL exit the interactive interface and restore the terminal
  to its prior (cooked, non-alternate-screen) state
