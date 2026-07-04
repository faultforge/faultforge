# Spec Delta: Continuous Integration

## ADDED Requirements

### Requirement: A manual, non-blocking e2e workflow runs the container suite

The repository SHALL provide a separate GitHub Actions workflow, triggered manually
(`workflow_dispatch`), that runs the end-to-end container suite
(`cargo test -p faultforge-e2e -- --ignored`) on a Linux runner using rootless podman. This
workflow SHALL NOT be a required check and SHALL NOT gate pull requests or pushes to `main`. On
failure the workflow SHALL upload the captured container logs as a build artifact.

#### Scenario: Operator triggers the e2e workflow

- **WHEN** a maintainer dispatches the e2e workflow
- **THEN** it builds the images, runs all e2e scenarios with rootless podman on the Linux
  runner, and reports success only if every scenario passes

#### Scenario: E2E does not block pull requests

- **WHEN** a pull request is opened or updated
- **THEN** the e2e workflow is not triggered and its status is not required for merging

#### Scenario: Failure artifacts aid diagnosis

- **WHEN** a dispatched e2e run fails
- **THEN** the workflow uploads the captured container logs as an artifact of that run
