# FaultForge

Chaos-engineering platform for **bare-metal** hosts: a central **master** (control plane) plus
**agents** on target hosts. Motto: *break it before it breaks you.*

## Current state

Cargo workspace, 4 crates. Only `proto` is implemented; the rest are **skeletons** (a `banner()`
+ stub `main`) waiting for the plan to be executed:

| Crate | Bin | Status | Role |
|-------|-----|--------|------|
| `crates/proto` | — | **built** | Shared gRPC contract; `build.rs` compiles `proto/faultforge.proto` |
| `crates/master` | `faultforge-master` | skeleton | gRPC server + axum REST + in-memory registry + SQLite store + sweep |
| `crates/agent` | `faultforge-agent` | skeleton | Dials master, register/heartbeat, reconnect w/ backoff |
| `crates/cli` | `faultforge` | skeleton | Operator CLI over the master REST API |

## Commands

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --check
cargo build -p faultforge-proto      # build a single crate
```

No system `protoc` needed — `crates/proto/build.rs` points `tonic-prost-build` at a vendored
binary (`protoc-bin-vendored`).

## Architecture (once built)

Agent **dials** the master and holds **one persistent bidirectional stream**; the master pushes
down it. This works through NAT/firewalls and is the same channel future commands will use.

**Two-tier state on the master — keep these distinct:**

1. **In-memory connection registry** (`agent_id → live stream handle`): source of truth for
   *Connected/Stale*. A live stream can't be persisted.
2. **Durable SQLite store** (behind the `AgentStore` trait): identity, name, os/arch, version,
   `last_seen`. The trait covers **only** this tier (so Postgres is a later config swap).

REST `/agents` status is a **join** of both tiers. On master restart the in-memory map is empty,
so every agent reads *Disconnected* until it reconnects. `last_seen` is hot in memory, persisted
opportunistically (on disconnect + by the sweep), **not** on every heartbeat.

**Identity is two fields:** `agent_id` (self-generated UUID, the reconnect key + DB primary key,
never changes) and `name` (human label, hostname by default, **master-authoritative**, admin-
renamable).

## Gotchas

- **gRPC RPC is named `Session`, NOT `Connect`.** tonic generates a `connect()` constructor on
  the client, so an RPC called `Connect` collides (`E0592 duplicate definitions`). Generated
  names are `client.session(...)` / server `session` + `SessionStream`.
- **Name reseed is first-registration-only.** The agent-reported name seeds the SQLite record
  only on INSERT; reconnects must NOT overwrite an admin rename with the hostname. This rule
  lives in the store's upsert (`ON CONFLICT … DO UPDATE` excludes `name`).
- **Supersede on reconnect:** a new stream for an existing `agent_id` wins — the master cancels
  and replaces the stale handle (handles zombie/half-open connections after a host reboot).
