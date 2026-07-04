# Proposal: master-fault-dispatch

## Why

The agent has a complete, safe fault runtime (`agent-fault-runtime`, step 2) but nothing commands
it: the master only logs the agent's fault frames, and the sole way to run a fault today is a
hand-rolled gRPC client. This change is step 3 of the four-change roadmap — the master gains
**experiment-lite** dispatch: an experiment record shaped per the `experiment-model` spec
(actions, durations, single salvo) but without metric rules/connectors and with a reduced outcome
lattice, so an operator can run, watch, and halt real faults end-to-end. Step 4
(`fault-e2e-harness`) then proves the whole loop in containers.

## What Changes

- **Experiment-lite record on the master.** A named set of actions, each binding explicit target
  hostnames (the registry has no tags yet) to a catalog plugin, params, and `duration_secs`.
  Single salvo, flat `actions` list (room for stages later, ADR-0002 §11). `grace_secs` is master
  config, not operator input. Records are **in-memory only** (ADR-0002 §17): a master restart
  loses them; agents keep hosts safe on their own.
- **Plugin catalog on the master**, loaded at startup from an on-disk catalog directory
  (`catalog_root`, same `<name>@<version>/` layout as the agent's `plugin_root`) via the existing
  `faultforge-fault` loader — digests, `params_schema`, and `max_duration_secs` come from the
  manifests, no new file format.
- **Synchronous VALIDATE at submission**: unknown plugin, invalid params, `duration >
  max_duration_secs`, unknown/disconnected/tainted target host — any failure rejects the
  experiment with every failing check named; nothing is dispatched.
- **Dispatch over `Session`**: master-minted deterministic `instance_id`s
  (`<experiment_id>:<hostname>:<action_index>`, ADR-0002 §12), one `RunFault` per (host, action),
  all fired together. Requires a per-hostname outbound-session map in the master (today no code
  can push a frame to a chosen agent).
- **Instance tracking + kill-switch + outcome.** The master tracks instance state solely from
  agent-authoritative `InstanceStatus`/`InstanceReport` frames. Any instance failure, any
  agent-side abort, any taint, or an operator halt fires the global kill-switch (`AbortFault` to
  every non-terminal instance). A master-side experiment deadline (`max(duration + grace) +
  margin`) guarantees every record resolves even if an agent vanishes. Reduced outcome lattice:
  `COMPLETED` / `ABORTED` / `ERROR`, with `ERROR` dominant and every non-`COMPLETED` outcome
  naming its cause (host, instance, reason, timestamp).
- **First write endpoints on the management plane** (still unauthenticated, per ADR-0002 known
  gaps): `POST /experiments` (run), `POST /experiments/{id}/halt`,
  `POST /agents/{hostname}/clear-taint`; plus reads `GET /experiments` and
  `GET /experiments/{id}`. **BREAKING** for the `agent-status-api` capability's "management API
  is read-only" requirement. Agent views gain a `tainted` field.
- **`ClearTaint` master→agent frame** (new proto message): taint is cleared only by operator
  command relayed through the master; the agent removes its taint file and answers with
  `TaintStatus { tainted: false }`. Amends the `agent-fault-runtime` rule "the agent never clears
  the taint itself" and the `fault-plugin-model` wire-schema requirement.
- **Operator CLI commands**: `experiment run -f <file>` (with `--wait`), `experiment list`,
  `experiment show <id>`, `experiment halt <id>`, `agents clear-taint <hostname>`. One-shot only;
  TUI additions deferred.

## Capabilities

### New Capabilities

- `master-fault-dispatch`: the master-side dispatch runtime — catalog loading, the
  experiment-lite record and validation, deterministic instance minting, dispatch and halt over
  `Session`, instance tracking and reconciliation, the kill-switch, the experiment deadline, the
  reduced outcome lattice, and the management-plane experiment/taint surface.

### Modified Capabilities

- `agent-status-api`: the "read-only management API" requirement is replaced (write endpoints
  arrive; unauthenticated-during-WIP stays); agent views gain `tainted`.
- `operator-cli`: new one-shot subcommands for experiments and taint clearing.
- `fault-plugin-model`: the wire-schema requirement gains the `ClearTaint` master→agent frame.
- `agent-fault-runtime`: taint clearing is amended — never on the agent's own initiative, only on
  `ClearTaint`; the agent removes the record and reports the new state.

## Impact

- **`crates/master`** — the bulk: catalog loading, experiment store + pure validation/outcome
  core, per-hostname session registry (supersede on reconnect), dispatch/kill-switch/deadline
  orchestration, new management handlers. New config: `catalog_root` (default
  `/usr/lib/faultforge/plugins`), `default_grace_secs` (default `10`).
- **`crates/proto`** — additive `ClearTaint` message in the `ServerMessage` oneof; regenerated
  code (wire-compatible; old agents log-and-continue).
- **`crates/agent`** — small: handle `ClearTaint` (remove taint file, emit `TaintStatus`);
  everything else untouched.
- **`crates/cli`** — new `experiment` command group and `agents clear-taint`; same
  `--master-url`/`-o` conventions.
- **`crates/fault`** — no changes expected; the master reuses the existing catalog loader and
  pure params validation (this was the point of the shared crate).
- **Operational** — the master package now carries the plugin catalog directory (including
  executables it never runs) so digests are computed from the same bytes agents have. Deployment
  docs note the management plane is now mutating and still unauthenticated: keep it on loopback
  or a trusted network (ADR-0002 known gap).
