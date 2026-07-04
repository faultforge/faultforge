# Spec Delta: Fault Plugin Model

## MODIFIED Requirements

### Requirement: Fault control and telemetry are frames on the Session stream

The wire contract SHALL define, on the existing `Session` stream: master→agent `RunFault`
(`instance_id`, `plugin_name`, `plugin_version`, `plugin_digest`, `params_json`, `duration_secs`,
`grace_secs`) and `AbortFault` (`instance_id`); agent→master `FaultEvent` (`instance_id` plus the
verbatim plugin NDJSON line), `InstanceStatus` (agent-authoritative transition: state, timestamp,
optional reason, and the `plugin_digest` the agent verified for the instance), `InstanceReport`
(reconciliation snapshot of instance statuses), and `TaintStatus` (host quarantine state and
reason). `grace_secs` SHALL be decided by the master and carried in `RunFault`. An endpoint
receiving a fault frame it does not handle SHALL log it and continue the session rather than
terminating.

#### Scenario: RunFault carries everything the agent needs

- **WHEN** the master issues a `RunFault`
- **THEN** the frame SHALL identify the instance, the plugin (name, version, digest), the
  JSON-encoded params, the duration, and the grace used for the dead-man deadline

#### Scenario: Unhandled fault frames do not kill the session

- **WHEN** a master or agent that does not yet implement fault behaviour receives a fault frame
- **THEN** it SHALL log the frame and keep the `Session` stream open

#### Scenario: Agent-authoritative statuses identify the verified plugin

- **WHEN** the agent emits an `InstanceStatus` transition or an `InstanceReport` snapshot entry
- **THEN** the frame SHALL carry the `plugin_digest` the agent verified for that instance, so
  reconciliation and audit can tie the reported state to the exact plugin bytes
