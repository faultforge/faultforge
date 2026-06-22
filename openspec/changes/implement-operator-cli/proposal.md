## Why

The master now exposes agent state over a read-only HTTP management API
(`GET /agents`, `GET /agents/{hostname}`), but the only consumer today is `curl`.
Operators need a first-class tool: an at-a-glance interactive view for humans and
a scriptable, machine-readable interface for automation. The `faultforge` CLI
crate exists as a skeleton; this change makes it a real, two-mode operator tool
that is structured to grow as the platform adds fault-injection features.

## What Changes

- Turn the `faultforge` binary from a placeholder into a working operator CLI
  that talks to the master's HTTP management API.
- Add a **one-shot (scripting) mode** with noun-verb commands `agents list` and
  `agents show <hostname>`, emitting JSON by default and a human table with
  `-o/--output table`. Exit codes reflect success / not-found / transport errors.
- Add an **interactive TUI mode** launched by running the bare command on a TTY:
  a four-region layout (persistent top health strip, left menu, right detail
  list, context-sensitive bottom shortcut bar) that polls `GET /agents` on an
  interval and on demand (`r`), with `q` to quit.
- Add a single global `--master-url` flag (full base URL, default
  `http://localhost:8069`) shared by both modes; an `https://` URL transparently
  selects TLS with no extra flag.
- Establish an internal architecture (shared HTTP client core + Elm-style pure
  Model/Update/View for the TUI) so future sections (faults, logs) and commands
  are additive rather than rewrites.
- Add dependencies: `reqwest` (rustls-tls + json) on the CLI crate; `ratatui`
  and `crossterm` to the workspace dependency table.

Out of scope for this slice (noted for later): faults/logs panels, agent
rename/remove, authentication and TLS specifics beyond accepting an `https` URL,
and config-file/environment layering for the CLI.
 
## Capabilities

### New Capabilities
- `operator-cli`: The `faultforge` command-line tool — its connection
  configuration, one-shot scripting commands (`agents list`/`show`) with JSON and
  table output, mode-selection rules (bare-on-TTY launches the TUI), and the
  interactive TUI's polling behavior and four-region layout.

### Modified Capabilities
<!-- None. The master's agent-status-api is consumed unchanged; no master-side
     requirements change in this slice. -->

## Impact

- **Code:** `crates/cli` — replaces the skeleton `main.rs` with `cli.rs`,
  `api/` (client core), `script.rs` (one-shot shell), and `tui/` (interactive
  shell + pure Model/Update/View).
- **Dependencies:** `reqwest` added to `crates/cli`; `ratatui` and `crossterm`
  added to the workspace `[workspace.dependencies]` table. `clap`, `tokio`,
  `serde`, and `serde_json` are already pinned.
- **APIs consumed (unchanged):** master management API `GET /agents` and
  `GET /agents/{hostname}` returning `{ hostname, name, last_seen_unix_ms }`.
- **Master, agent, proto crates:** unchanged.
