# FaultForge MVP — Master/Agent Registration & Connection Lifecycle

> Motto: *break it before it breaks you.*

## Context

FaultForge is a chaos-engineering platform (à la Gremlin / Steadybit) for **bare-metal**
deployment: a central **master** (control plane) plus **agents** installed on target hosts.
Agents register with the master, stay connected, and will *eventually* receive
fault-injection commands.

**This MVP does NOT implement fault injection.** Its single job is to prove the foundation:
agents reliably register, stay connected, and show up *alive* on the master — over a channel
that already carries the future command path, so the next slice is low-risk.

## Locked decisions

| Area | Decision |
|------|----------|
| Comms model | Agent **dials** the master and holds a **persistent bidirectional stream**. Master pushes down it. (Works through NAT/firewalls.) |
| Transport | **gRPC + protobuf** via **tonic** on tokio. One bidi RPC is the whole channel. |
| Persistence | **SQLite** (via `sqlx`) behind a **storage trait** → later Postgres swap is a config change, not a rewrite. |
| Operator interface | **REST/HTTP API** (axum) **+ a thin `faultforge` CLI** that calls it. |
| Auth/TLS | **Deferred.** MVP runs **insecure**. Future: configurable — agent verifies master cert, master verifies agent cert *or* token, plus an explicit insecure mode. Channel is structured so TLS config + tonic interceptors bolt on later without rearchitecting. |
| Agent identity | **Stable internal UUID** (reconnect key + DB PK) **+ human Name** (hostname default, master-authoritative, admin-renamable). |
| Liveness | **Heartbeat over the stream + `last_seen`**; states **Connected / Stale / Disconnected**. |

## Identity model

Identity is split into two fields, because if one identifier is *both* the reconnect key *and*
admin-editable, a rename breaks reconnect matching:

- **`agent_id` (UUID):** agent self-generates on first start, persists it in its local state
  file, and sends it on **every** reconnect. It is the SQLite **primary key** and the
  reconnect-matching key. Invisible in normal use; never changes.
- **`name` (label):** **defaults to the hostname** at first registration; persisted and
  reused. **Master is the source of truth.** Admin renames it anytime via the REST API/CLI —
  purely cosmetic, never disturbs the stream. The agent host *may* seed an override via its
  local config.
- **Name-reseed authority:** the agent-reported name seeds the record **only on first
  registration**. On later reconnects the master keeps its stored name, so a reconnect never
  overwrites an admin rename with the hostname.

This delivers hostname-by-default, persisted, reused, admin-changeable — with rock-solid
reconnects.

## Architecture overview

```
   ┌─────────────────────── master (control plane) ───────────────────────┐
   │                                                                       │
   │  gRPC server (tonic)              REST API (axum)                      │
   │  Connect(stream)  ◄── agents      /api/v1/agents  ◄── faultforge CLI   │
   │        │                                  │                            │
   │        ├── in-memory Connection Registry  │  (join for status)         │
   │        │     agent_id → live stream handle│                            │
   │        └── Storage trait ──► SQLite (identity, name, os/arch, version, │
   │                                            last_seen)                  │
   └───────────────────────────────────────────────────────────────────────┘
            ▲ persistent bidi stream (agent dials in)
   ┌────────┴─────────┐   ┌──────────────────┐
   │ agent (host A)   │   │ agent (host B)   │  ...
   │ local state:     │   │  Register +      │
   │  agent_id, name  │   │  Heartbeat ↑     │
   └──────────────────┘   │  ServerMsg ↓     │
                          └──────────────────┘
```

### Two-tier state model (important)

The master keeps state in **two** places; treat them distinctly:

1. **In-memory Connection Registry:** `agent_id → live stream handle`. Source of truth for
   **Connected**. A live stream can't be persisted.
2. **Durable SQLite registry** (behind the storage trait): identity, name, os/arch, version,
   `last_seen`. The trait covers **only** this durable tier.

Consequences (encoded in behavior):
- REST `/agents` status is a **join** of both tiers.
- **On master restart** the in-memory map is empty → every agent reads **Disconnected** until
  it reconnects. (Acceptable; agents reconnect with backoff.)
- `last_seen` is a hot in-memory value, **persisted opportunistically** (on disconnect +
  periodically), **not** written to SQLite on every heartbeat.

## Protocol (one bidi RPC)

`rpc Connect(stream AgentMessage) returns (stream ServerMessage)`

- **AgentMessage (agent → master):**
  - `Register { agent_id, name, hostname, os, arch, version }`
  - `Heartbeat {}`
- **ServerMessage (master → agent):**
  - `RegisterAck { server_time, heartbeat_interval_secs }`
  - *(reserved for next slice: `Command {…}`)*

The agent's first frame on a new stream is always `Register`; the master replies `RegisterAck`
carrying the heartbeat cadence it should use. The downward direction carries only `RegisterAck`
today — but proving the master→agent path now is what de-risks fault injection later.

## Connection-lifecycle rules (this IS the MVP)

1. **Supersede on reconnect:** if `agent_id` opens a new stream while an old handle exists, the
   **new stream wins** — master drops/closes the stale handle and replaces it. Handles zombie
   (half-open) connections after an agent host reboot.
2. **Staleness sweep:** a background task flips **Connected → Stale** when
   `now - last_seen > N × heartbeat_interval` (start with N = 3). Without this sweep the
   "Stale" state doesn't exist.
3. **Disconnect:** stream close (or supersede) → **Disconnected**; persist `last_seen`.
4. **Name reseed:** agent-reported name seeds the record only on first registration (rule
   above).

## Workspace layout (Cargo workspace)

```
faultforge/
├── Cargo.toml                 # [workspace]
├── crates/
│   ├── proto/                 # .proto + tonic/prost build.rs (shared contract)
│   ├── master/                # bin: faultforge-master (gRPC + axum + storage)
│   ├── agent/                 # bin: faultforge-agent (dials, register, heartbeat, backoff)
│   └── cli/                   # bin: faultforge (operator CLI over REST)
└── docs/superpowers/specs/    # this design doc
```

A small shared `common` module may live inside `proto` (or its own crate) for types reused by
master/agent (e.g. status enum). Keep crates focused.

## Master REST API (axum, `/api/v1`)

- `GET  /agents` — list (status = join of connection registry + SQLite)
- `GET  /agents/{id}` — detail
- `PATCH /agents/{id}` — rename (`{ "name": "..." }`); master-authoritative
- `DELETE /agents/{id}` — forget/deregister from SQLite (and drop any live handle)
- `GET  /healthz` — master liveness

## CLI (`faultforge`, over REST)

- `faultforge agents list`
- `faultforge agents show <id>`
- `faultforge agents rename <id> <name>`
- `faultforge agents rm <id>`
- Global `--master-url` (default `http://127.0.0.1:<port>`).

## Configuration

- **Master:** gRPC listen addr, REST listen addr, SQLite path, heartbeat interval, stale
  multiplier N. (figment/serde + file + env.)
- **Agent:** master address, local state-file path, optional name override, optional heartbeat
  interval, reconnect backoff bounds.

## Out of scope / deferred

- **Fault injection** — the next slice; the bidi `Command` path is reserved, not built.
- **Auth / TLS** — deferred; MVP is insecure-mode only.
- **Cloned/templated images** duplicate the persisted `agent_id` → two hosts present the same
  `agent_id`. **Known gap, out of scope for MVP**; acknowledged given the bare-metal target.
- **Audit/history** of connect/disconnect events (we chose `last_seen` only, not a timeline).
- **Postgres** — trait is ready; not wired for MVP.

## Implementation outline (TDD; detailed plan via writing-plans)

1. **Workspace + `proto`** — Cargo workspace; define `.proto`; wire tonic/prost `build.rs`;
   compile generated types.
2. **Storage trait + SQLite impl** — `AgentStore` trait (CRUD + `update_last_seen` +
   `set_name`); `sqlx`/SQLite impl with migration; unit tests against a temp DB.
3. **Master gRPC `Connect`** — accept stream, handle `Register` (seed-or-update, supersede
   rule), `RegisterAck`, heartbeat → in-memory `last_seen`; connection registry map.
4. **Staleness sweep + disconnect** — background task; persist `last_seen` on disconnect.
5. **Master REST (axum)** — endpoints above; status join; `/healthz`.
6. **Agent binary** — local state (load/generate `agent_id`, resolve name), dial master,
   `Register`, heartbeat loop, reconnect with exponential backoff + jitter.
7. **CLI** — `agents` subcommands over REST.
8. **Integration test** — spin up master + one (or two) agents in-process; assert: registers
   and appears Connected; heartbeats keep it Connected; killing the agent → Stale → Disconnected;
   reconnect supersedes; rename persists and survives reconnect; second stream for same
   `agent_id` supersedes the first.

## Verification

- `cargo test --workspace` (unit + integration green).
- Manual smoke: run `faultforge-master`; run `faultforge-agent` (two on different
  hostnames/state files); `faultforge agents list` shows both **Connected**; `kill` one →
  list shows **Stale** then **Disconnected**; restart it → **Connected** again (same row);
  `faultforge agents rename <id> web-01` then restart that agent → name stays `web-01`;
  restart master → agents show **Disconnected** then reconnect to **Connected**.
- `cargo clippy --workspace -- -D warnings`, `cargo fmt --check`.
