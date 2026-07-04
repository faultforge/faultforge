# Spec: Experiment Model

## Purpose

Defines how the master turns single-host fault execution into a fleet-wide, safety-bounded,
*judged* experiment (v1 scope): definition, tag-based targeting, a write control surface,
single-salvo orchestration, three-role metric evaluation with the hypothesis judged during
injection, all-or-nothing semantics, the outcome lattice, dry-run, and blast radius. Governing
decisions: [ADR-0002](../../../docs/adr/0002-fault-model-decisions.md). Builds on
`fault-plugin-model`.

## Requirements

### Requirement: An experiment is a set of tag-targeted actions
An experiment SHALL be defined as a set of actions, each binding a target group (selected by host
tag) to a plugin, its parameters, and a `duration`. An experiment SHALL also declare its metric
rules and a blast-radius ceiling. A host matching multiple target tags SHALL receive one action per
matching group.

#### Scenario: Action binds a plugin to a tagged group
- **WHEN** an experiment defines an action targeting tag `db` with plugin `kill-process`
- **THEN** every in-scope host carrying the `db` tag SHALL be assigned a `kill-process` fault
  instance for that action

#### Scenario: Host matches multiple target groups
- **WHEN** a host carries tags matching two different actions in the experiment
- **THEN** that host SHALL receive both actions as separate concurrent fault instances

### Requirement: Operators can launch, watch, and halt experiments
The master SHALL expose a write control surface allowing an operator to launch an experiment, watch
its live state, and halt it. The halt operation SHALL trigger the global kill-switch and SHALL be
the most-available operation: it SHALL succeed even when connectors are unreachable, metrics are
unavailable, or the experiment record is degraded.

#### Scenario: Operator halts a running experiment
- **WHEN** an operator issues a halt for a running experiment
- **THEN** the master SHALL stop every fault instance in that experiment, even if metric connectors
  are unreachable at that moment

#### Scenario: Operator watches live experiment state
- **WHEN** an operator queries a running experiment
- **THEN** the master SHALL report each instance's lifecycle state and the latest guardrail and
  hypothesis samples

### Requirement: Experiments execute as a single salvo
All actions in an experiment SHALL be injected together as one salvo; the master SHALL NOT sequence
actions into ordered stages in this version. The experiment definition SHALL be structured so that
ordered stages can be added later without changing existing single-salvo definitions.

#### Scenario: All actions fire together
- **WHEN** an experiment with multiple actions starts injection
- **THEN** all actions SHALL be injected together rather than in a defined sequence

### Requirement: Experiments follow a defined lifecycle
The master SHALL drive an experiment through DRAFT → VALIDATE → PRECONDITIONS → INJECTING →
MONITORING → RECOVERING → VERDICT. VALIDATE SHALL confirm the schema is valid, plugins resolvable,
`duration <= max_duration_secs`, the blast radius satisfiable against live hosts, and no `TAINTED`
host in scope. PRECONDITIONS SHALL hold before injection begins.

#### Scenario: Validation fails
- **WHEN** an experiment references a plugin whose `max_duration_secs` is less than the action's
  `duration`
- **THEN** the experiment SHALL be rejected in VALIDATE and SHALL NOT inject

#### Scenario: A targeted host is tainted
- **WHEN** an experiment's target scope includes a `TAINTED` host
- **THEN** the experiment SHALL be rejected in VALIDATE

### Requirement: Metric rules are evaluated in three roles via connectors
The master SHALL evaluate metric rules through connectors (Prometheus first) in three roles:
**precondition** before injection (must hold or the experiment does not start), **guardrail**
continuously during MONITORING (a breach triggers the global kill-switch), and **hypothesis**. A
rule's smoothing window SHALL be expressed in its query; the duration a breach must persist before
acting SHALL be a separate `sustain` value. The master SHALL sample at a configurable
`sampling_interval`. A connector unreachable beyond a configured tolerance SHALL be treated as a
guardrail breach, never as a satisfied rule.

#### Scenario: Precondition not met
- **WHEN** a precondition rule does not hold at PRECONDITIONS
- **THEN** the experiment SHALL NOT inject

#### Scenario: Guardrail breach is sustained
- **WHEN** a guardrail rule is breached continuously for its `sustain` duration during MONITORING
- **THEN** the master SHALL trigger the global kill-switch

#### Scenario: Connector becomes unreachable
- **WHEN** a connector is unreachable for longer than the configured tolerance during MONITORING
- **THEN** the master SHALL treat it as a guardrail breach and trigger the global kill-switch

### Requirement: The hypothesis is evaluated during injection
The master SHALL sample the hypothesis rules during the injection window (overlapping MONITORING,
while faults are active), not after recovery. VERDICT SHALL aggregate the during-injection samples;
it SHALL NOT be the only point at which the hypothesis is measured.

#### Scenario: Hypothesis sampled while the fault is active
- **WHEN** an experiment is in MONITORING with faults active
- **THEN** the master SHALL sample the hypothesis rules during that window and use those samples to
  determine the verdict

#### Scenario: Hypothesis is not judged solely post-recovery
- **WHEN** the experiment reaches RECOVERING
- **THEN** the verdict SHALL be based on samples taken while faults were active, not on a single
  measurement taken after the host has recovered

### Requirement: Experiments are all-or-nothing with a global kill-switch
The master SHALL treat an experiment as atomic across the fleet. Any fault instance reaching a
terminal failure, any sustained guardrail breach, any agent-initiated abort the master learns of,
any connector outage beyond tolerance, or an operator halt SHALL trigger a global kill-switch that
stops every fault instance in the experiment. The master SHALL NOT report partial success.

#### Scenario: One instance fails
- **WHEN** any single fault instance in a running experiment reaches a terminal failure
- **THEN** the master SHALL stop all other instances in that experiment via the global kill-switch

### Requirement: Outcome separates clean execution from the hypothesis verdict, with ERROR dominating
The master SHALL classify a finished experiment as one of `RESILIENT`, `WEAKNESS_FOUND`, `ABORTED`,
or `ERROR`, by precedence: if any instance ended in `ERROR` or any host is `TAINTED`, the outcome
SHALL be `ERROR`, which dominates `ABORTED`. `ABORTED` SHALL be reserved for a clean halt with no
broken host. `RESILIENT` and `WEAKNESS_FOUND` SHALL both require that the experiment ran and
recovered cleanly, distinguished by whether the hypothesis held; `WEAKNESS_FOUND` SHALL NOT be
reported as a failure. Every `ABORTED` and `ERROR` outcome SHALL name the host, instance, rule,
value-versus-threshold, and timestamp that determined it.

#### Scenario: An instance leaves a host broken
- **WHEN** the kill-switch fires because an instance reached `ERROR` and left a host `TAINTED`
- **THEN** the experiment outcome SHALL be `ERROR` (not `ABORTED`), naming the host and instance

#### Scenario: Clean halt with no broken host
- **WHEN** the kill-switch fires from a sustained guardrail breach and every host recovered cleanly
- **THEN** the experiment outcome SHALL be `ABORTED`, naming the rule and value that tripped it

#### Scenario: Clean run, hypothesis held vs broke
- **WHEN** all faults injected and recovered cleanly
- **THEN** the outcome SHALL be `RESILIENT` if every hypothesis rule held, or `WEAKNESS_FOUND` if a
  hypothesis rule did not hold, reported as a discovered weakness rather than a failure

### Requirement: Experiments can be dry-run without injecting
The master SHALL support a dry-run that performs VALIDATE, PRECONDITIONS, and per-host runtime
preflight, and confirms every plugin is present in the baked-in catalog, **without injecting any
fault**. The dry-run SHALL report the exact hosts that would be affected and any blocking issues.

#### Scenario: Dry-run surfaces a missing requirement
- **WHEN** a dry-run finds a targeted host lacks a required binary or precondition for its assigned
  plugin
- **THEN** the master SHALL report the blocking issue and SHALL NOT inject any fault

### Requirement: Blast radius is enforced against live hosts before an experiment runs
Every experiment SHALL declare a blast-radius ceiling (maximum hosts and/or maximum percentage per
target group, and a concurrency limit). The master SHALL compute the affected host set against
currently-live hosts and SHALL refuse to start an experiment that would exceed the ceiling.

#### Scenario: Experiment exceeds the blast radius
- **WHEN** an experiment would affect more live hosts than its declared `max_hosts` or `max_percent`
- **THEN** the master SHALL refuse to start it

#### Scenario: Ceiling computed against live hosts only
- **WHEN** the master computes a percentage ceiling for a target group
- **THEN** it SHALL count only hosts considered live by the heartbeat staleness check, not stale
  registry entries
