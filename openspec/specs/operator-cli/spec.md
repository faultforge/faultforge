# Spec: Operator CLI

## Purpose

Defines the command-line interface for operators to interact with FaultForge: discovering agents,
running and monitoring experiments, and managing host quarantine.

## Requirements

### Requirement: Run an experiment from a definition file

The CLI SHALL provide an `experiment run -f <file>` subcommand that reads an experiment
definition from a YAML or JSON file, submits it via `POST /experiments`, and on acceptance
writes the created record (including the experiment id and minted instance ids) to standard
output. On validation rejection (`422`) the CLI SHALL write every reported failing check to
standard error and exit non-zero. With `--wait` the CLI SHALL poll the experiment until it is
terminal and exit `0` only for `COMPLETED`, with distinct non-zero statuses for `ABORTED` and
`ERROR`.

#### Scenario: Accepted experiment prints the record

- **WHEN** the user runs `faultforge experiment run -f exp.yaml` and the master accepts the
  experiment
- **THEN** the CLI SHALL write the created record with its experiment id to standard output and
  exit with a success status

#### Scenario: Validation failure lists every check

- **WHEN** the master rejects the submission naming two failing checks
- **THEN** the CLI SHALL write both checks to standard error and exit with a non-zero status

#### Scenario: Wait gates on the outcome

- **WHEN** the user runs `faultforge experiment run -f exp.yaml --wait` and the experiment ends
  `ERROR`
- **THEN** the CLI SHALL exit with the non-zero status designated for `ERROR`, distinct from the
  `ABORTED` status

### Requirement: List, show, and halt experiments

The CLI SHALL provide `experiment list` (`GET /experiments`), `experiment show <id>`
(`GET /experiments/{id}`, `404` reported as not-found with a distinct non-zero exit), and
`experiment halt <id>` (`POST /experiments/{id}/halt`). `experiment show` SHALL display the
experiment state, per-instance states with reasons, and — when terminal — the outcome and its
recorded cause. These subcommands SHALL honour the global `--master-url` and `-o/--output`
conventions.

#### Scenario: Show a running experiment

- **WHEN** the user runs `faultforge experiment show exp-100-1` while it is running
- **THEN** the CLI SHALL write the experiment state and each instance's current lifecycle state
  to standard output

#### Scenario: Halt requests the kill-switch

- **WHEN** the user runs `faultforge experiment halt exp-100-1` for a running experiment
- **THEN** the CLI SHALL report the halt as accepted and exit with a success status

### Requirement: Clear a host taint

The CLI SHALL provide an `agents clear-taint <hostname>` subcommand calling
`POST /agents/{hostname}/clear-taint`. On `202` it SHALL report the clear as requested (noting
the effect is confirmed by the agent's subsequent report); on `404` or `409` it SHALL write the
diagnostic to standard error and exit non-zero.

#### Scenario: Clear-taint accepted

- **WHEN** the user runs `faultforge agents clear-taint web-01` and the master responds `202`
- **THEN** the CLI SHALL report the request as accepted and exit with a success status

#### Scenario: Host not connected

- **WHEN** the master responds `409` because the host has no live session
- **THEN** the CLI SHALL write the conflict diagnostic to standard error and exit with a
  non-zero status
