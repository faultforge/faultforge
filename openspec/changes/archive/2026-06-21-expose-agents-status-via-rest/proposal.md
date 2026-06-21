## Why

The master tracks every connected agent in an in-memory registry, but that state
is invisible from outside the process — there is no way for an operator (or the
future CLI) to ask "which agents are registered and when were they last seen?".
We need a read-only management surface now so the rest of the platform has
something to build against, even while the project is still in WIP.

## What Changes

- Add a **second server** to the master: an HTTP management API, separate from the
  gRPC agent plane and bound to its own address.
- Expose the agent registry read-only over REST:
  - `GET /agents` — list all registered agents.
  - `GET /agents/{hostname}` — fetch one agent; `404` when unknown.
  - Each agent is returned as `{ hostname, name, last_seen_unix_ms }` (raw
    `last_seen`, no derived online/stale status yet).
- Add `management_listen_addr` to the master configuration (layered like the
  existing settings; default `127.0.0.1:8069`).
- Run the gRPC and HTTP servers concurrently; the registry is created once and
  shared between them.

Explicit non-goals for this slice (intentional, not oversights):

- **No authentication** — the management API is open.
- **No TLS** — plain HTTP only, to avoid certificate churn during WIP.
- **Read-only** — no commands, no mutation of agent state.
- **No staleness/online status** — only the raw `last_seen` timestamp is exposed;
  interpretation is left to the client until a staleness sweep exists.

## Capabilities

### New Capabilities
- `agent-status-api`: Read-only HTTP/REST management API on the master that exposes
  the agent registry (list and fetch-by-hostname), served on a dedicated address
  without authentication or TLS.

### Modified Capabilities
<!-- None. agent-registration and agent-heartbeat populate the registry but their
     requirements do not change; this capability only reads what they write. -->

## Impact

- **Crate `crates/master`**:
  - New module for the HTTP API (axum router, shared state, handlers, and a pure
    `AgentInfo -> AgentView` mapping).
  - `run_server` refactored: lift `new_registry()` out so the registry is shared,
    and run both servers concurrently via `tokio::try_join!`. `ServerError` gains
    a variant for HTTP transport failures.
  - `MasterConfig` / `Cli` gain `management_listen_addr` (default `127.0.0.1:8069`).
- **Dependencies**: add `axum` (and `serde_json`; `serde` is already present) to
  the workspace and to `crates/master`.
- **No changes** to `crates/proto`, `crates/agent`, or the gRPC contract.
