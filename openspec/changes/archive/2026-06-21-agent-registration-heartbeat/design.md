## Context

`crates/proto` already defines a draft `faultforge.proto` (a `Session` bidi RPC with
`Register`/`Heartbeat`/`RegisterAck`), but `crates/master` and `crates/agent` are skeleton
binaries with empty `Cargo.toml`s. This change is the first real implementation: it finalizes
the v1 wire contract for register+heartbeat and implements both sides end to end, in-memory,
single-binary-each, no CLI/REST/SQLite.

The earlier design draft (now superseded) proposed a self-generated `agent_id` (UUID) plus a
master-owned `name`, persisted by the agent across restarts. That model was dropped in favor of
hostname-only identity (see Decisions below) before any code existed, so there is no migration
from a prior implementation — only from the proto draft and from `CLAUDE.md`'s description of
that older model.

## Goals / Non-Goals

**Goals:**
- Agent dials the master over the gRPC bidi `Session` stream, sends `Register { hostname }`,
  and receives `RegisterAck { server_time_unix_ms, heartbeat_interval_secs }`.
- Agent then sends `Heartbeat {}` every `heartbeat_interval_secs` and receives `HeartbeatAck`.
- Master keeps an in-memory, hostname-keyed registry of connected agents with `last_seen`.
- Both binaries get minimal, fail-fast configuration (master: listen address + heartbeat
  interval; agent: master address).

**Non-Goals:**
- No SQLite / `AgentStore` / durable persistence (any tier).
- No REST API, no `faultforge` CLI, no rename/management console.
- No reconnect-with-backoff — connection or stream failure is terminal for the agent process in
  this slice.
- No staleness sweep, no Connected/Stale/Disconnected status model — `last_seen` is tracked but
  nothing reads or acts on it yet.
- No server-pushed messages outside of direct replies (no `Command` path) — every
  `ServerMessage` is a reply to a specific `AgentMessage`.

## Decisions

### 1. Identity = hostname only (no `agent_id`, no local agent state)
The agent sends only `hostname` in `Register`. The master's registry is keyed by `hostname`;
`name` is a master-owned field seeded from `hostname` on first sight (no rename mechanism
exists yet, so in practice `name == hostname` for the lifetime of this slice).

**Alternative considered**: self-generated `agent_id` (UUID) persisted in a local state file,
with `name` as a separate mutable label (the original design-doc model, also reflected in the
current `CLAUDE.md`). Rejected for this slice because it requires agent-side persistence
(state-file location, format, generation-on-first-run) purely to support a rename/management
flow that doesn't exist yet.

**Known limitation** (to be documented in `CLAUDE.md`): renaming or reimaging a host to a new
hostname produces a *new*, unrelated registry entry — there is no continuity with the old one,
and duplicate hostnames in the fleet are not detected or disambiguated. Acceptable while there's
no rename/console; revisit (e.g. reintroduce a persisted per-host identifier) if this becomes an
operational problem.

### 2. `RegisterAck.heartbeat_interval_secs` is a directive, not a hint
The master decides the cadence (from its own config) and the agent must use the value it
receives in `RegisterAck`. The agent has no independent heartbeat-interval setting.

**Alternative considered**: agent-local `--heartbeat-interval` config, with the master's value
as a default/override. Rejected — two sources of truth for the same number invites drift; a
single master-side knob is simpler to operate across a fleet.

### 3. Master state: single in-memory map, no channels
`Arc<Mutex<HashMap<String /* hostname */, ConnInfo>>>` where `ConnInfo { hostname, name,
last_seen }`. The `Session` handler is a synchronous 1:1 transform over the stream: read one
`AgentMessage`, update the map under the lock, write the corresponding `ServerMessage`. No
`mpsc` channel or background writer task is introduced, because nothing in this slice needs to
push a message to an agent except as a direct reply.

**Trade-off accepted**: when the `Command` path is added later, the handler will need to be
reworked to support master-initiated pushes (typically via a per-connection `mpsc` channel
feeding the response stream). That rework is explicitly deferred, not designed around now.

**Trade-off accepted**: the registry is wiped on master restart. Since agents don't reconnect in
this slice, a master restart effectively requires every agent process to be restarted too. This
is consistent with "Non-Goals" (no reconnect) and is an accepted operational characteristic of
this slice, not a bug to fix here.

### 4. Config: `config` (config-rs) + `clap` (derive) + `serde`, fail-fast
Both binaries build a config struct via layered sources — struct defaults < optional config
file (path via `--config` flag / env var) < environment variables < CLI flags — then call
`try_deserialize`. A field with no default and not `Option<T>` causes `try_deserialize` to
return `Err`, which the binary turns into a startup error and non-zero exit.

- **Master fields**: `listen_addr` (default `127.0.0.1:50051`), `heartbeat_interval_secs`
  (default `5`).
- **Agent fields**: `master_addr` (no default — required).

**Alternative considered**: `figment`. Equivalent capability; `config` (config-rs) chosen per
project preference. No functional difference for this slice.

### 5. Proto changes are additive/trimming only, no new RPC
Only `Register` (trimmed to `hostname`) and the `ServerMessage` oneof (new `HeartbeatAck`
variant) change. The `Session` RPC shape, `Heartbeat {}`, and `RegisterAck`'s fields are
unchanged from the existing draft.

## Risks / Trade-offs

- **[Risk] Hostname collisions / renames create disjoint identities** → Mitigated by
  documenting the limitation in `CLAUDE.md`; revisit if it causes real operational pain.
- **[Risk] Master restart silently drops all agents (no reconnect)** → Accepted for this slice;
  reconnect-with-backoff is the natural next slice and was scoped out deliberately (see
  Non-Goals).
- **[Risk] 1:1 stream transform can't support future server-initiated pushes** → Accepted;
  the `Command` path will require a follow-up rework of the `Session` handler (per-connection
  channel). Not designed around speculatively here.
- **[Trade-off] No persistence at all means every master restart loses `last_seen` history** →
  Acceptable; nothing reads `last_seen` yet beyond the in-memory record itself.
