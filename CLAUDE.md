# FaultForge

Chaos-engineering platform for **bare-metal** hosts: a central **master** (control plane) plus
**agents** on target hosts. Motto: *break it before it breaks you.*

## Current state

Cargo workspace, 4 crates:

| Crate | Bin | Status | Role |
|-------|-----|--------|------|
| `crates/proto` | — | **built** | Shared gRPC contract; `build.rs` compiles `proto/faultforge.proto` |
| `crates/master` | `faultforge-master` | **built (slice 1)** | Two planes over one registry: gRPC agent server + read-only HTTP management API; in-memory hostname registry, layered config |
| `crates/agent` | `faultforge-agent` | **built (slice 1)** | Dials master, register/heartbeat loop, layered config |
| `crates/cli` | `faultforge` | **built** | Operator CLI — one-shot scripting (`agents list/show`) + interactive TUI |

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
```

## Coding standards
Before writing or modifying any Rust, read [CONVENTIONS.md](CONVENTIONS.md) and follow it. Treat its rules as mandatory, not advisory
Rationale: [docs/adr/0001-coding-standards.md](docs/adr/0001-coding-standards.md).

## Architecture (slice 1 — register + heartbeat)

Agent **dials** the master and holds **one persistent bidirectional gRPC `Session` stream**.
The stream is used for both registration (first frame) and periodic heartbeats. This works
through NAT/firewalls and is the same channel future fault-injection commands will use.

**Current state: single in-memory registry, no persistence.**
The master keeps `Registry = Arc<Mutex<HashMap<Hostname, AgentInfo>>>` where `AgentInfo
{ hostname, name, last_seen }` (`crates/master/src/registry.rs`). There is no SQLite store or
staleness sweep in this slice. On master restart the registry is empty; since agents don't
reconnect in this slice they must be restarted too.

**Two planes over one registry.** The master runs two independent listeners sharing the same
`Registry` handle:
- **gRPC agent plane** (`listen_addr`, default `:50051`) — the `Session` stream described above;
  the only plane agents ever touch.
- **HTTP management plane** (`management_listen_addr`, default `:8069`,
  `crates/master/src/management.rs`) — a **read-only** axum API for operators and the CLI:
  `GET /agents` (JSON array of `AgentView { hostname, name, last_seen_unix_ms }`) and
  `GET /agents/{hostname}` (`404` if absent). It never talks to agents and performs no
  fault injection — it only reads registry state. No auth/TLS during WIP. The pure core is
  `agent_view`; handlers are the imperative shell.

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
`heartbeat_interval_secs = 5`.
Agent: `master_addr` is **required** — startup fails if absent from all sources.

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
- **CLI talks to the HTTP management plane, not gRPC.** `faultforge --master-url <url>` points at
  the master's management API (default `http://localhost:8069`), e.g. `agents list` → `GET /agents`.
  Agents are the only clients of the gRPC plane.
- **CLI config is a single flag, not layered.** `faultforge --master-url <url>` is the only
  configuration mechanism for the CLI — there is no config file, no `FAULTFORGE_` env var
  layering, and no `--config` flag. This is intentional: the CLI has exactly one value to
  configure (the master URL), and the layering ceremony of master/agent is not worth adding for
  one field. Config-file/env layering can be added later without changing the URL semantics.
