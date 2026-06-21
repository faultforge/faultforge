## Why

`faultforge-master` and `faultforge-agent` are currently skeleton binaries (a `banner()` and a
stub `main`), and `crates/proto` defines a draft wire contract that has never been exercised end
to end. Before any fault-injection work can start, we need to prove the foundational comms
loop — an agent dialing the master, registering, and reporting liveness — over the real gRPC
bidi stream. This is the smallest vertical slice that does that.

## What Changes

- Finalize the v1 `faultforge.proto` contract for this slice:
  - `Register` carries only `hostname` (drop `agent_id`/`name`/`os`/`arch`/`version`). Hostname
    is the sole identity and reconnect key; the master computes/owns `name` and never receives
    it on the wire. **BREAKING** (draft contract — no external consumers yet).
  - Add `HeartbeatAck { server_time_unix_ms }` to the `ServerMessage` oneof.
  - Clarify `RegisterAck.heartbeat_interval_secs` as a **directive**: the agent's heartbeat loop
    must use this value.
  - Update `crates/proto/src/lib.rs` unit tests to match the trimmed `Register` and the new
    `HeartbeatAck` oneof variant.
- Implement `faultforge-master`:
  - gRPC server (tonic) implementing `AgentService::Session` as a 1:1 request/response
    transform (no background push channel needed yet).
  - In-memory connection registry only: `HashMap<hostname, ConnInfo>` behind a `Mutex`. No
    SQLite/`AgentStore` in this slice.
  - Layered config (`config` + `clap` + `serde`): `listen_addr` and `heartbeat_interval_secs`,
    both with defaults.
- Implement `faultforge-agent`:
  - Identity-stateless — reads its hostname at startup, no local persistence.
  - Dials the master, sends `Register { hostname }`, awaits `RegisterAck`, then loops sending
    `Heartbeat {}` at the master-dictated interval and awaiting `HeartbeatAck`.
  - Layered config (`config` + `clap` + `serde`): `master_addr` is mandatory (no default) and
    startup fails with a clear error if it's missing.
  - No reconnect/backoff — on connection or stream failure, log and exit. Reconnect is a
    follow-up slice.
- Update `CLAUDE.md`'s identity-model gotcha: replace the two-field (`agent_id` + `name`) model
  with hostname-only identity, and document the rename/duplicate-hostname limitation.
- Out of scope for this slice: CLI, REST/management API, SQLite persistence, rename/console,
  reconnect-with-backoff.

## Capabilities

### New Capabilities
- `agent-registration`: Agent connects to the master over the gRPC bidi `Session` stream and
  registers using its hostname as identity (first frame). Master maintains an in-memory record
  keyed by hostname, seeds `name` from `hostname`, and replies with `RegisterAck` (server time +
  heartbeat interval).
- `agent-heartbeat`: After registration, the agent periodically sends `Heartbeat` at the
  master-dictated interval; the master updates the registry entry's `last_seen` and replies with
  `HeartbeatAck`.

### Modified Capabilities
(none — greenfield specs, no existing `openspec/specs/` entries)

## Impact

- `crates/proto/proto/faultforge.proto` — trimmed `Register`, new `HeartbeatAck`, updated oneof.
- `crates/proto/src/lib.rs` — unit tests updated for the new message shapes.
- `crates/master/Cargo.toml` + `crates/master/src/main.rs` — new deps (`tokio`, `tonic`,
  `faultforge-proto`, `tracing`, `tracing-subscriber`, `config`, `clap`, `serde`); gRPC server,
  in-memory registry, config loading.
- `crates/agent/Cargo.toml` + `crates/agent/src/main.rs` — same new deps plus `tonic` transport
  client; dial/register/heartbeat loop, config loading.
- `CLAUDE.md` — identity-model gotchas rewritten for hostname-only identity.
