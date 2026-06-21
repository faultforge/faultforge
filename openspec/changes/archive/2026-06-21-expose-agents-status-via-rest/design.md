## Context

The master keeps connected agents in `Registry = Arc<Mutex<HashMap<String, AgentInfo>>>`
where `AgentInfo { hostname, name, last_seen }`. Today this registry is created
*inside* `run_server` (`crates/master/src/server.rs`) and handed only to the gRPC
`MasterService`; nothing else can read it. `run_server` resolves `listen_addr`,
builds the service, and calls `tonic::transport::Server::serve(addr)` — a single
server, single port, returning `Result<(), ServerError>`.

Configuration is layered (`MasterConfig`: `listen_addr`, `heartbeat_interval_secs`)
via the `config` crate plus clap. Coding standards (CONVENTIONS.md) require a
functional core / imperative shell split, time-as-data, `thiserror` typed errors,
and no `unwrap`/`expect` outside tests.

## Goals / Non-Goals

**Goals:**
- Expose the agent registry read-only over HTTP without disturbing the gRPC plane.
- Share one registry instance between gRPC and HTTP.
- Keep the JSON mapping in a pure, unit-testable function; keep axum handlers thin.
- Stay consistent with the existing config and error-handling style.

**Non-Goals:**
- Authentication, TLS, rate limiting, or CORS (WIP — plain HTTP, open).
- Any write/command endpoint or registry mutation.
- A derived online/stale status — only the raw `last_seen` is exposed.
- Graceful shutdown coordination beyond "if one server dies, the master dies".

## Decisions

### Lift registry creation into `run_server`, pass to both servers
`new_registry()` moves to the top of `run_server`; the `Arc` is cloned into the
gRPC `MasterService` and into the HTTP layer's shared state. This is the only
change to existing wiring and keeps a single source of truth for agent state.

### Run gRPC and HTTP concurrently with `tokio::try_join!`
Both servers produce a `Future<Output = Result<(), _>>`. `try_join!` runs them
together and returns on the first error, satisfying the spec requirement that a
failure of either server stops the master. `ServerError` gains an HTTP variant
(e.g. `Http(std::io::Error)` for bind/serve failures) so the typed-error
convention holds end to end.

### The API lives in a new `management` module inside `crates/master`
Not a separate crate: a module gets the shared registry for free and matches the
"one master binary" model. The module is named `management` (not `http`) to
communicate intent — it is the operator/tooling plane, not part of the gRPC
agent plane — rather than naming it after its transport. It contains the axum
`Router`, the shared state (holding the `Registry`), the two handlers, and the
pure mapping function. Exposed via `pub use` in `lib.rs` where needed for tests.

### Pure core: `agent_view(&AgentInfo) -> AgentView`
The DTO mapping is a pure, time-free function: `AgentView { hostname, name,
last_seen_unix_ms }`, where `last_seen_unix_ms` reuses `faultforge_proto::unix_ms`
for consistency with the gRPC wire format. `AgentView` derives `serde::Serialize`.
Because the user chose raw `last_seen` over a derived status, no `now`/`Clock` is
needed in the mapping — the handler just locks the registry, maps entries, and
serializes. Handlers are the imperative shell.

### Endpoint and response shape
- `GET /agents` → `200` with `[AgentView, ...]` (empty array when none).
- `GET /agents/{hostname}` → `200` with `AgentView`, or `404` when absent.
Axum path extraction yields the hostname string; lookup is a `HashMap::get` under
the existing mutex. No `Hostname::parse` is required on the read path (lookup by
the stored string key is sufficient and avoids rejecting keys already in the map).

### Config: add `management_listen_addr`
New `MasterConfig.management_listen_addr: String` and matching optional clap flag,
with struct default `127.0.0.1:8069` set in `load_config`. Follows the existing
`set_default` / env (`FAULTFORGE_MANAGEMENT_LISTEN_ADDR`) / `set_override` chain.

## Risks / Trade-offs

- **Shared `Mutex` contention**: HTTP reads briefly lock the same mutex as gRPC
  writes. Holding the lock only for the map-and-collect (not across `.await`)
  keeps contention negligible at expected scale.
- **No graceful shutdown**: `try_join!` means one server's error aborts the other
  mid-flight. Acceptable for WIP; revisit when graceful shutdown is added.
- **Open, unauthenticated API**: a deliberate WIP trade-off; binding to
  `127.0.0.1` by default limits blast radius until auth/TLS land.
- **New dependency surface**: `axum` (+ `serde_json`) enters the workspace. Chosen
  because it shares the hyper/tower/tokio stack with tonic, minimizing new
  transitive weight.
