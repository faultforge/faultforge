# Spec Delta: Agent Heartbeat

## REMOVED Requirements

### Requirement: Agent exits on connection or stream failure without reconnecting

**Reason**: A fault-supervising agent that exits on a stream blip would orphan active fault
instances — the opposite of the recoverability guarantee. Slice-1 behaviour is replaced by
reconnect-with-backoff plus master-loss tracking (which feeds the self-abort timer in the
`agent-fault-runtime` capability).

**Migration**: None operationally; the agent now retries instead of exiting. Deployments that
relied on exit-to-restart (e.g. systemd unit restarts as the reconnect mechanism) simply see the
agent handle reconnection itself.

## ADDED Requirements

### Requirement: Agent reconnects with backoff and tracks master loss

If the `Session` stream fails — at initial dial, while awaiting `RegisterAck`, or during the
heartbeat loop — the agent SHALL log the failure and retry the full connect-and-register
sequence with exponential backoff (capped), retrying indefinitely. The agent SHALL track the
time since master contact was lost and SHALL expose connectivity transitions to the fault
runtime so the master-loss self-abort threshold can be enforced. Each successful reconnection
SHALL be a fresh registration followed by the reconciliation report defined in
`agent-fault-runtime`.

#### Scenario: Master unreachable at startup

- **WHEN** the agent cannot establish a connection to the configured `master_addr`
- **THEN** the agent SHALL log the error and keep retrying with backoff instead of exiting

#### Scenario: Stream failure during the heartbeat loop

- **WHEN** the `Session` stream is closed or errors while the agent is sending heartbeats
- **THEN** the agent SHALL record the moment contact was lost, retry with backoff, and register
  again on the new stream once the master is reachable
