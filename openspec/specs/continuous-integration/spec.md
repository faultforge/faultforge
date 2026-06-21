## Purpose

Defines the automated CI pipeline that gates every pull request and push to `main` on format,
lint, build, and test checks. There is no delivery, publish, or release stage.

## Requirements

### Requirement: Automated quality gates on pull requests and main

The system SHALL run format, lint, build, and test checks automatically via GitHub Actions on
every pull request and on every push to `main`. The pipeline SHALL fail if any check fails.
There SHALL be no delivery, publish, or release stage.

#### Scenario: Pull request triggers the pipeline

- **WHEN** a pull request is opened or updated against the repository
- **THEN** the CI workflow runs `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo build --workspace`, and `cargo test --workspace`
- **AND** the pipeline reports success only if all four checks pass

#### Scenario: Push to main triggers the pipeline

- **WHEN** a commit is pushed to the `main` branch
- **THEN** the same four checks run and gate the result

#### Scenario: Toolchain matches the pinned version

- **WHEN** the workflow sets up Rust
- **THEN** it uses the channel and components declared in `rust-toolchain.toml` (1.96 with `clippy` and `rustfmt`) rather than a hardcoded or floating version

### Requirement: Strict warning policy

The pipeline SHALL treat any compiler or Clippy warning as a build failure. Lint levels in
`Cargo.toml` MAY remain at `warn` for local developer experience, but CI SHALL escalate them to
errors.

#### Scenario: A new Clippy warning fails CI

- **WHEN** a change introduces code that triggers any Clippy lint enabled in the workspace configuration
- **THEN** the `cargo clippy --workspace --all-targets -- -D warnings` step fails and blocks the pull request

#### Scenario: Test-target lints are covered

- **WHEN** Clippy runs in CI
- **THEN** it runs with `--all-targets` so lints in unit tests, integration tests, and examples are checked, not only library and binary code

### Requirement: Lint configuration enforces no-unwrap/expect convention

The workspace lint configuration SHALL enforce CONVENTIONS.md rule #6 (no `unwrap`/`expect`
outside tests) by enabling `clippy::unwrap_used` and `clippy::expect_used`. Test code SHALL be
exempt, and the few documented production `expect` sites SHALL carry per-site allows with a
justification.

#### Scenario: Production unwrap/expect fails CI

- **WHEN** non-test production code calls `.unwrap()` or `.expect()` without a justified per-site allow
- **THEN** CI fails on the `clippy::unwrap_used` or `clippy::expect_used` lint

#### Scenario: Test code is exempt

- **WHEN** unit-test modules (`#[cfg(test)]`) or integration tests call `.unwrap()` / `.expect()`
- **THEN** CI does not fail, because those scopes carry an allow for these lints

#### Scenario: Documented impossible-panic expects are allowed

- **WHEN** a production `expect` documents why the panic is impossible and carries a per-site `#[allow(clippy::expect_used)]`
- **THEN** CI does not fail on that site
