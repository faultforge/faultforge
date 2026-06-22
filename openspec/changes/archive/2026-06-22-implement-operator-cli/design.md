## Context

The `faultforge` CLI crate is a compiling skeleton (`main.rs` prints a banner).
The master exposes a read-only HTTP management API (`agent-status-api` spec) on
`127.0.0.1:8069` by default:

```
GET /agents            -> 200 [{ hostname, name, last_seen_unix_ms }, ...]  ([] if empty)
GET /agents/{hostname} -> 200 { hostname, name, last_seen_unix_ms } | 404
```

This is the CLI's only backend. The agent-facing gRPC `Session` stream is not
used by the CLI. There is no push/streaming channel for management data, so the
TUI must poll.

Constraints come from `CONVENTIONS.md` (ADR-0001): imperative shell / functional
core; time is data (pure functions take `now: SystemTime`, only the shell reads
the wall clock); `thiserror` in library code and `anyhow` only at binary entry;
no `unwrap`/`expect` outside tests; pedantic clippy denied in CI. The workspace
pins the gRPC/tokio/clap/serde stack centrally in `[workspace.dependencies]`.

## Goals / Non-Goals

**Goals:**
- One binary, two modes (one-shot scripting + interactive TUI), sharing a single
  typed HTTP client core so transport/model logic is written once.
- An internal structure where adding a future section (faults, logs) or command
  is additive — a new enum variant and its arms — not a rewrite.
- A TUI whose decision logic is pure and unit-testable per CONVENTIONS, with I/O
  (HTTP, terminal, clock) confined to a thin shell.
- Consistency with the existing crates' conventions and dependency pinning.

**Non-Goals:**
- No fault-injection, logs, rename, or remove features (menu has one section).
- No authentication or TLS configuration beyond accepting an `https://` URL.
- No config-file or environment-variable layering for the CLI (single flag).
- No persistence or caching of agent data between runs.

## Decisions

### Single crate, layered modules (not a separate client library crate)
Keep everything in `crates/cli`; enforce the shell/core seam with module
boundaries rather than crate boundaries.

```
crates/cli/src/
  main.rs     # anyhow entry: parse clap, build tokio runtime, select mode
  cli.rs      # clap types: GlobalArgs { master_url, output }, Command enum
  api/
    mod.rs    # Client { base_url, http }: async list_agents(), get_agent(host)
    model.rs  # Agent domain type; Deserialize from the master's JSON
    error.rs  # thiserror ApiError::{ Transport, NotFound, Decode, Status }
  output.rs   # pure render: agents -> JSON string | table string
  script.rs   # one-shot shell: run a Command via Client, render, map exit code
  tui/
    mod.rs    # shell: crossterm raw/alt-screen, async event loop, runs Effects
    model.rs  # pure Model { agents, screen, selected, loading, last_error, last_fetch }
    update.rs # pure update(Model, Msg, now) -> (Model, Option<Effect>)
    view.rs   # pure view(&Model, now) -> ratatui frame (4 regions)
```

*Rationale:* a second crate adds packaging ceremony for no isolation benefit at
this size; modules give the same testability. Revisit only if another binary
needs to embed the client.

*Alternative considered:* `faultforge-client` lib + `faultforge-cli` bin —
rejected as premature.

### TUI follows the Elm architecture (Model / Update / View)
`update` and `view` are pure and synchronous; the only async/I/O lives in
`tui/mod.rs`. Both keypresses and fetch completions are `Msg`s:

```
Msg    = Key(KeyEvent) | Tick | AgentsLoaded(Result<Vec<Agent>, ApiError>)
Effect = FetchAgents
update(Model, Msg, now) -> (Model, Option<Effect>)   // pure, no I/O, no async
```

The shell loop: render `view(&model, now)`; await the next event (a crossterm key
or a tick from an interval); call `update`; if it returns `Effect::FetchAgents`,
spawn the HTTP call and feed its result back as `AgentsLoaded`. `now` is read once
per loop turn in the shell and passed into `update`/`view`.

*Rationale:* this is the canonical realization of CONVENTIONS #1–#3 for a UI — the
state machine is testable with a fixed `now` and synthetic `Msg`s, no terminal or
runtime required. *Alternative:* ad-hoc mutable widget state with I/O inline —
rejected; it violates the shell/core rule and is hard to test.

### `Screen` enum is the UI spine
`Model.screen: Screen` (today only `Screen::Agents`) routes three things: the left
menu highlight, the right detail pane (`view` matches on `screen`), and the bottom
shortcut bar. The top health strip and the fetch loop are screen-independent.
Adding a section later = one `Screen` variant + its `view` arm + its `shortcuts`
arm; nothing else changes.

### Relative time ("last seen Ns ago") is pure and computed client-side
The master returns `last_seen_unix_ms` (absolute). The "Ns ago" string is derived
in `view`/`output` from `(now, last_seen)` — both passed in. No module under the
core reads the wall clock. *Rationale:* CONVENTIONS #2; makes rendering tests
deterministic.

### HTTP via `reqwest` (async, rustls-tls, json)
`reqwest` is already a master dev-dependency and pairs naturally with tokio, which
the TUI loop needs anyway. `rustls-tls` lets an `https://` master URL "just work"
with no system OpenSSL dependency. `ApiError` (thiserror) maps: connection/timeout
→ `Transport`, HTTP 404 → `NotFound`, other non-2xx → `Status`, body parse →
`Decode`. The 404→`NotFound` mapping is what lets `agents show` exit with a
distinct not-found code.

*Alternatives:* `ureq` (blocking, simpler) — rejected because the TUI needs
non-blocking fetches; `native-tls` — rejected to avoid a system TLS dependency on
bare-metal hosts.

### Mode selection
`main` parses with clap. If a subcommand is present → `script::run`. If absent and
`std::io::stdout().is_terminal()` (std `IsTerminal`) → `tui::run`. If absent and
not a TTY → print help to stderr, exit non-zero. *Rationale:* a bare command that
gets piped must never block on an interactive loop.

### Exit codes
`0` success; `2` not-found (`agents show` on 404); `1` transport/other errors.
Distinct codes let scripts distinguish "no such agent" from "master down".

### Output rendering is pure
`output.rs` takes already-fetched `Vec<Agent>` / `Agent` plus the format and
returns a `String`; the shell prints it. JSON via `serde_json`; table hand-rolled
or via a small table helper. Keeping it pure keeps formatting unit-testable.

### Dependencies and pinning
Add `ratatui` and `crossterm` to `[workspace.dependencies]` (pinned as a pair,
like the tonic stack). Add `reqwest { default-features = false, features =
["rustls-tls", "json"] }` to `crates/cli`. `clap`, `tokio`, `serde`,
`serde_json` are already pinned and reused.

## Risks / Trade-offs

- **Polling, not push** → The management API is request/response only, so the TUI
  polls every few seconds. *Mitigation:* keep the interval modest (e.g. 3s) plus
  manual `r`; revisit if a streaming/SSE management endpoint is added later.
- **CLI config diverges from master/agent (single flag, no file/env layering)** →
  inconsistent across binaries. *Mitigation:* deliberate for a single value;
  record the divergence in the proposal and a short CLAUDE.md note so it reads as
  intentional. Layering can be added later without changing the URL semantics.
- **`is_terminal()` heuristic** can mis-detect unusual environments (some CI
  PTYs). *Mitigation:* a subcommand always forces one-shot; document that
  scripts should pass an explicit subcommand rather than rely on TTY detection.
- **Terminal restoration on panic** → a panic mid-TUI could leave the terminal in
  raw/alt-screen state. *Mitigation:* restore via a guard/`Drop` (or panic hook)
  in `tui/mod.rs` so teardown runs on both normal exit and unwinding.
- **`no_std`-style purity vs. ratatui types** → keeping `view` "pure" while it
  builds ratatui widgets is purity-of-no-I/O, not zero-dependency. *Accepted:* the
  goal is testability/no I/O, which holds since widgets are values.

## Migration Plan

Additive: only `crates/cli` source and the workspace dependency table change. No
master/agent/proto changes, no data migration. Rollback = revert the crate; the
skeleton binary is replaced wholesale. Verified by `cargo build/test/clippy/fmt`
across the workspace plus a manual run against a locally running master.

## Open Questions

- Table renderer: hand-rolled vs. a small dependency (e.g. `comfy-table`)? Lean
  hand-rolled for two columns to avoid a dependency; revisit if columns grow.
- Polling interval and whether it should be flag-configurable later (out of scope
  now; fixed constant this slice).
