# FaultForge

Chaos-engineering platform for **bare-metal** hosts: a central **master** (control plane) plus
**agents** on target hosts. Motto: *break it before it breaks you.*

## Current state

Cargo workspace, 6 crates:

| Crate | Bin | Status | Role |
|-------|-----|--------|------|
| `crates/proto` | — | **built** | Shared gRPC contract; `build.rs` compiles `proto/faultforge.proto`. Carries the fault wire schema (`RunFault`/`AbortFault`, `FaultEvent`/`InstanceStatus`/`InstanceReport`/`TaintStatus`, `InstanceState`) |
| `crates/fault` | — | **built** | Shared fault contract (`faultforge-fault`): manifest parsing/validation, params schema, agent↔plugin invocation protocol (stdin JSON, NDJSON events, exit-code table), lifecycle states, sha256 digest, catalog layout. Sync, no tonic/tokio; the optional `proto` feature adds domain⇄wire `InstanceState` conversions (agent/master only — plugin binaries never enable it) |
| `crates/master` | `faultforge-master` | **built (slice 3)** | Two planes over one registry: gRPC agent server + HTTP management API (reads **and** the fault-dispatch writes); in-memory hostname registry + experiment store, on-disk plugin catalog, layered config |
| `crates/agent` | `faultforge-agent` | **built (slice 2)** | Dials master with reconnect+backoff, register/heartbeat, and the **fault runtime**: executes fault instances from the on-disk catalog with a journal, safety timers, and taint quarantine |
| `crates/cli` | `faultforge` | **built** | Operator CLI — one-shot scripting (`agents list/show/clear-taint`, `experiment run/list/show/halt`) + interactive TUI |
| `crates/plugins/noop-marker` | `noop-marker` | **built** | Reference fault plugin: zero-blast-radius fault whose only effect is a marker file's existence. Proves the `faultforge-fault` contract with golden tests |
| `crates/e2e` | — (tests) | **built (slice 4)** | Dev-only end-to-end harness (`faultforge-e2e`): builds container images and drives the **real** master/agent/CLI binaries as separate rootless-podman containers, asserting the fault lifecycle through the operator surface + host ground truth (`podman exec`). Never published; scenarios are `#[ignore]` so `cargo test --workspace` needs no podman |

**Fault injection works end to end (`master-fault-dispatch`, slice 3).** The agent owns the full
instance lifecycle (`agent-fault-runtime`): `RunFault`/`AbortFault`/`ClearTaint` handling, a pure
per-instance state machine (`crates/agent/src/machine.rs`), digest verification before *every*
plugin invocation, two-phase preflight, an instance journal with restart replay, the one-timer
safety triad (duration stop / master-loss self-abort / dead-man), and persistent `TAINTED`
quarantine. The master dispatches **experiment-lite** records: explicit-hostname targeting,
single salvo, no metric rules/connectors, reduced outcome lattice `COMPLETED`/`ABORTED`/`ERROR`
(`ERROR` dominates). Operators drive it via the management API or the CLI
(`experiment run -f file.yaml [--wait]`). The full `experiment-model` (tags, guardrails,
hypotheses, `RESILIENT`/`WEAKNESS_FOUND`) is still future work.

## Commands

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo build -p faultforge-proto      # build a single crate
```

No system `protoc` needed — `crates/proto/build.rs` points `tonic-prost-build` at a vendored
binary (`protoc-bin-vendored`).

**End-to-end suite (`crates/e2e`, slice 4).** The container scenarios are `#[ignore]`, so the
commands above run **nothing** from `crates/e2e` and need no podman. Run them explicitly with
rootless podman (macOS: `podman machine start` first):

```bash
cargo test -p faultforge-e2e -- --ignored
```

The suite builds its images from `containers/Containerfile` on first run (cached after). A
**manual, non-blocking** `.github/workflows/e2e.yml` (`workflow_dispatch`) runs it in CI — it
does **not** gate PRs; `ci.yml` remains the required check.

### Running the binaries

```bash
# Master — gRPC on :50051, HTTP management on :8069 (both default to 127.0.0.1)
cargo run -p faultforge-master
cargo run -p faultforge-master -- --listen-addr 0.0.0.0:50051 --management-listen-addr 0.0.0.0:8069

# Agent — master_addr is REQUIRED (flag, FAULTFORGE_MASTER_ADDR env, or --config file)
cargo run -p faultforge-agent -- --master-addr http://127.0.0.1:50051

# CLI — talks to the HTTP management API (default http://localhost:8069)
cargo run -p faultforge -- agents list
cargo run -p faultforge -- agents show <hostname>
cargo run -p faultforge -- experiment run -f exp.yaml --wait
cargo run -p faultforge -- experiment halt <id>
cargo run -p faultforge -- agents clear-taint <hostname>
```

## Coding standards
Before writing or modifying any Rust, read [CONVENTIONS.md](CONVENTIONS.md) and follow it. Treat its rules as mandatory, not advisory
Rationale: [docs/adr/0001-coding-standards.md](docs/adr/0001-coding-standards.md).

**Comments explain WHY, never WHAT** ([CONVENTIONS.md §4](CONVENTIONS.md#4-comments-explain-why-never-what)):
a comment is justified only when it explains *why*, adds context the code can't show (spec/ADR
reference, caller invariant, non-obvious consequence), or flags a hack/gotcha. Never write a comment
that narrates what the next lines do — the code should be self-explanatory; if it isn't, fix the
code, don't annotate it. `///` doc-comments on public items are the exception: they are required API
documentation and are kept.

## Architecture

Agent **dials** the master and holds **one persistent bidirectional gRPC `Session` stream**.
The stream carries registration (first frame), periodic heartbeats, and all fault control and
telemetry frames. This works through NAT/firewalls. Since `agent-fault-runtime` the session is
**full-duplex and self-healing**: a reader dispatches inbound frames, a writer drains one shared
outbound channel, and the whole connect→register→pump lifecycle sits in a reconnect loop with
exponential backoff (1s→30s cap, retries forever). Every successful registration is followed by
the reconciliation sequence: `InstanceReport` (live instances; empty is meaningful),
`TaintStatus` (both values), then any queued replay outcomes.

**Agent fault runtime (slice 2).** Functional core / imperative shell throughout:
- `machine.rs` — the pure per-instance state machine `step(state, event) -> (state, effects)`
  plus the pure one-deadline function (`min` of duration end / master-loss threshold / dead-man).
  All ADR-0002 ordering rules (retry-cleanup-once-then-taint, dead-man dominance, two-track
  telemetry) live here as table-tested transitions.
- `supervisor.rs` — one tokio task per instance executes effects: digest-gated plugin
  invocations (re-verified before **every** spawn, including replay), journal writes, status
  emission. Duplicate `RunFault` ids are dropped (inject is never re-issued). Restart replay
  recovers journaled instances (`abort`+`cleanup`) *before* the first connect: `ABORTED` if
  before `deadline_unix`, `ERROR` past it.
- `runner.rs` — sync process shell: stdin JSON + EOF, NDJSON stdout split, stderr capture, hard
  invocation timeout (default 60s) with kill.
- `journal.rs` / `taint.rs` — atomic (temp+rename) versioned records under `data_dir`; the taint
  file fails closed (unreadable = tainted) and is never removed by the agent.

**Current state: in-memory registry + in-memory experiment store, no persistence.**
The master keeps `Registry = Arc<Mutex<HashMap<Hostname, AgentInfo>>>` where `AgentInfo
{ hostname, name, last_seen, tainted }` (`crates/master/src/registry.rs`) and an experiment
store inside `Dispatcher` (`crates/master/src/dispatch.rs`). There is no SQLite store or
staleness sweep. On master restart both are empty (ADR-0002 §17): agents reconnect and
re-register; running experiments are forgotten — the agents' own safety net (self-abort,
journal replay) keeps hosts safe, and frames for unknown instances are logged and dropped.

**Master fault dispatch (slice 3).** Functional core / imperative shell again:
- `experiment.rs` — pure experiment-lite domain: definition/record types, deterministic
  `instance_id = <experiment_id>:<hostname>:<action_index>` (ADR-0002 §12), all-failures
  validation, kill-switch trigger, outcome lattice.
- `dispatch.rs` — the orchestration shell (`Dispatcher`): accept → validate → mint → single-salvo
  `RunFault` fan-out; intake of agent-authoritative `InstanceStatus`/`InstanceReport`/`TaintStatus`
  frames (`FaultEvent` is log-only); kill-switch (`AbortFault` to all non-terminal instances) on
  the first ERROR / unrequested abort / in-scope taint / halt; a per-experiment deadline
  (`start + max(duration+grace) + 30s`) so no record stays non-terminal.
- `catalog.rs` — loads the plugin catalog from `catalog_root` at startup with the shared
  `faultforge-fault` loader (same on-disk layout as the agent's `plugin_root`; broken entries are
  logged and skipped).
- `sessions.rs` — per-hostname outbound session map (insert-supersede on register, remove
  only-if-current on stream close) so dispatch can push frames to a chosen agent.

**Two planes over one registry.** The master runs two independent listeners sharing the same
`Registry` handle:
- **gRPC agent plane** (`listen_addr`, default `:50051`) — the `Session` stream described above;
  the only plane agents ever touch.
- **HTTP management plane** (`management_listen_addr`, default `:8069`,
  `crates/master/src/management.rs`) — an axum API for operators and the CLI. Reads:
  `GET /agents` (`AgentView { hostname, name, last_seen_unix_ms, tainted }`),
  `GET /agents/{hostname}`, `GET /experiments`, `GET /experiments/{id}`. Writes (the
  fault-dispatch surface, `master-fault-dispatch`): `POST /experiments` (validate → `422` with
  *all* failing checks, or `201` + dispatch), `POST /experiments/{id}/halt` (`202`/`404`/`409`),
  `POST /agents/{hostname}/clear-taint` (`202`/`404`/`409`). **No auth/TLS during WIP — never
  expose the management port beyond a trusted network (ADR-0002 known gap).**

**Identity is hostname only (slice 1).**
The agent sends only `Register { hostname }` — no UUID, no local state file. The master keys
the registry by hostname and seeds `name` from hostname on first sight. There is no rename
mechanism yet (`name == hostname` for the lifetime of this slice).

**Known limitation:** renaming or reimaging a host to a new hostname produces a new, unrelated
registry entry — there is no continuity with the old one, and duplicate hostnames are not
detected. Revisit (e.g. reintroduce a persisted per-host UUID) when a rename/console is added.

**`RegisterAck.heartbeat_interval_secs` is a directive, not a hint.** The master decides the
cadence; the agent must use exactly the value received in `RegisterAck`. The agent has no
independent heartbeat-interval setting.

## Config

Both binaries use layered sources (lowest → highest priority):

1. Struct defaults (coded in the binary)
2. Optional config file (`--config` / `FAULTFORGE_CONFIG`)
3. Environment variables (prefix `FAULTFORGE_`, e.g. `FAULTFORGE_LISTEN_ADDR`)
4. CLI flags

Master defaults: `listen_addr = 127.0.0.1:50051`, `management_listen_addr = 127.0.0.1:8069`,
`heartbeat_interval_secs = 5`, `catalog_root = /usr/lib/faultforge/plugins`,
`default_grace_secs = 10` (grace is a master decision carried in `RunFault`, like heartbeat
cadence).
Agent: `master_addr` is **required** — startup fails if absent from all sources. Runtime keys
(all defaulted): `plugin_root = /usr/lib/faultforge/plugins`, `data_dir = /var/lib/faultforge`,
`master_loss_threshold_secs = 30`, `invocation_timeout_secs = 60` (env/file only, no CLI flag).
Running as non-root in dev requires pointing `data_dir` at a writable path.

## Gotchas

- **gRPC RPC is named `Session`, NOT `Connect`.** tonic generates a `connect()` constructor on
  the client, so an RPC called `Connect` collides (`E0592 duplicate definitions`). Generated
  names are `client.session(...)` / server `session` + `SessionStream`.
- **Hostname is the sole identity key (slice 1).** The old two-field model (`agent_id` UUID +
  master-owned `name`) described in earlier design docs was dropped before any code existed.
  There is no `agent_id` field, no local agent state file, and no admin-rename feature yet.
- **Supersede on reconnect (in-memory only):** a new `Register` for an existing hostname
  replaces the registry entry (`HashMap::insert`). The first stream's task continues running
  until it closes naturally — the master does not actively cancel it in this slice.
- **The agent reconnects; it no longer exits on stream failure.** The slice-1 "log and exit"
  requirement was REMOVED by `agent-fault-runtime`: an agent supervising live faults must
  survive stream blips. Deployments that relied on exit-to-restart still work — the agent just
  handles reconnection itself.
- **`master_loss_threshold_secs` is agent config, deliberately NOT a master directive** (unlike
  heartbeat cadence and `grace_secs`): it must keep working exactly when the master is
  unreachable, so it cannot be delivered per-session.
- **Deployment prerequisite: run the agent under a process supervisor** (`systemd
  Restart=always`). Journal replay recovers instances after a restart, but an agent that dies
  and is never restarted leaves an active fault ungoverned — a standalone OS-level watchdog is
  deferred until the first stateful plugin (`tc`/`iptables`) lands (design D6 of
  `agent-fault-runtime`).
- **Instance ids are validated at the agent boundary** (charset `[A-Za-z0-9._:-]`, no `.`/`..`)
  because they become journal file stems; an unusable id is logged and dropped, not aborted (no
  addressable status can be emitted for it).
- **CLI talks to the HTTP management plane, not gRPC.** `faultforge --master-url <url>` points at
  the master's management API (default `http://localhost:8069`), e.g. `agents list` → `GET /agents`.
  Agents are the only clients of the gRPC plane.
- **CLI exit codes are part of the contract:** `0` OK/`COMPLETED`, `1` transport/unexpected, `2`
  not found, `3` rejected at VALIDATE, `4` waited experiment `ABORTED`, `5` waited experiment
  `ERROR` — `experiment run --wait` is usable as a scripted chaos gate.
- **Taint is cleared only via `ClearTaint`** (operator → management API → master relays over the
  host's live `Session`; the agent removes `tainted.json` and answers with `TaintStatus`). The
  agent never clears it on its own; the master never assumes it — the registry flag updates only
  from the agent's report. Clearing a disconnected host returns `409`.
- **The experiment deadline margin is a constant** (`DEADLINE_MARGIN_MS = 30s`) with a
  `Dispatcher::with_deadline_margin_ms` override used by tests to compress timing.
- **CLI config is a single flag, not layered.** `faultforge --master-url <url>` is the only
  configuration mechanism for the CLI — there is no config file, no `FAULTFORGE_` env var
  layering, and no `--config` flag. This is intentional: the CLI has exactly one value to
  configure (the master URL), and the layering ceremony of master/agent is not worth adding for
  one field. Config-file/env layering can be added later without changing the URL semantics.
