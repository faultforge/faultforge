## 1. Dependencies and crate setup

- [x] 1.1 Add `ratatui` and `crossterm` to `[workspace.dependencies]` in the root `Cargo.toml`, pinned as a pair (matching the existing pinning style)
- [x] 1.2 Add dependencies to `crates/cli/Cargo.toml`: `reqwest { default-features = false, features = ["rustls-tls", "json"] }`, `ratatui`, `crossterm`, plus `clap`, `tokio`, `serde`, `serde_json`, `thiserror`, `anyhow` from the workspace
- [x] 1.3 Add `#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]` to the crate root and confirm `cargo build -p faultforge-cli` succeeds with the new deps

## 2. API client core (`api/`)

- [x] 2.1 `api/model.rs`: define the `Agent` domain type and derive `Deserialize` to map the master's `{ hostname, name, last_seen_unix_ms }` JSON; store `last_seen` as a `SystemTime`/timestamp suitable for relative-age math
- [x] 2.2 `api/error.rs`: define `ApiError` with `thiserror` — `Transport`, `NotFound`, `Status`, `Decode` variants
- [x] 2.3 `api/mod.rs`: define `Client { base_url, http }` with `new(base_url)`; implement async `list_agents() -> Result<Vec<Agent>, ApiError>` (GET `/agents`) and `get_agent(hostname) -> Result<Agent, ApiError>` (GET `/agents/{hostname}`), mapping HTTP 404 → `NotFound`, other non-2xx → `Status`, connect/timeout → `Transport`, body parse → `Decode`
- [x] 2.4 Unit-test the 404→`NotFound` and decode-error mappings where feasible without a live server (e.g. pure mapping helpers)

## 3. CLI surface and mode selection

- [x] 3.1 `cli.rs`: define clap `GlobalArgs { master_url (default http://localhost:8069), output (json|table, default json) }` and a `Command` enum with `Agents` subcommands `List` and `Show { hostname }`
- [x] 3.2 `main.rs`: anyhow entry — parse clap, build the tokio runtime; if a subcommand is present call `script::run`; else if `stdout().is_terminal()` call `tui::run`; else print help to stderr and exit non-zero
- [x] 3.3 Map results to exit codes: `0` success, `2` not-found, `1` transport/other; ensure errors print to stderr and never emit partial data to stdout

## 4. Output rendering (`output.rs`)

- [x] 4.1 Implement a pure `render` that takes agents (or one agent) plus the chosen format and `now`, returning a `String`: JSON via `serde_json`, and a human table (status, name, last-seen age)
- [x] 4.2 Implement the pure relative-age helper (`now`, `last_seen` -> "Ns ago") and unit-test it with fixed `now` values
- [x] 4.3 Unit-test JSON and table rendering for the empty, single, and multi-agent cases

## 5. One-shot shell (`script.rs`)

- [x] 5.1 Implement `run(global, command) -> exit code`: construct `Client`, dispatch `agents list` / `agents show`, render via `output.rs`, write to stdout
- [x] 5.2 Handle `agents show` 404 by writing a not-found diagnostic to stderr and returning the not-found exit code
- [x] 5.3 Handle transport/`Status` errors by writing a diagnostic to stderr and returning the error exit code, writing nothing to stdout

## 6. TUI pure core (`tui/model.rs`, `tui/update.rs`, `tui/view.rs`)

- [x] 6.1 `tui/model.rs`: define `Model { agents, screen: Screen, selected, loading, last_error, last_fetch }`, the `Screen` enum (only `Agents`), `Msg` (`Key`, `Tick`, `AgentsLoaded(Result)`), and `Effect` (`FetchAgents`)
- [x] 6.2 `tui/update.rs`: pure `update(Model, Msg, now) -> (Model, Option<Effect>)` — handle `q` quit signal, `r` → `FetchAgents`, up/down selection, `Tick` → `FetchAgents`, and `AgentsLoaded` updating agents or `last_error`
- [x] 6.3 `tui/view.rs`: pure `view(&Model, now)` building the four regions — top health strip (name + last activity), left menu (driven by `Screen`), right Agents detail list (status dot, name, relative age), bottom shortcut bar (driven by `Screen`)
- [x] 6.4 Unit-test `update`: quit, manual refresh emits `FetchAgents`, selection bounds, `AgentsLoaded(Ok)` and `AgentsLoaded(Err)` transitions — all with a fixed `now` and synthetic `Msg`s

## 7. TUI shell (`tui/mod.rs`)

- [x] 7.1 Implement terminal setup/teardown (raw mode + alternate screen) with a guard that restores the terminal on both normal exit and panic
- [x] 7.2 Implement the async event loop: read `now`, render `view`, await the next crossterm key event or interval tick, call `update`, and on `Effect::FetchAgents` spawn the HTTP call and feed `AgentsLoaded` back
- [x] 7.3 Wire the polling interval (fixed constant, e.g. 3s) and the manual `r` refresh; ensure a failed fetch surfaces `last_error` without exiting
- [x] 7.4 Exit cleanly on `q`, restoring the terminal

## 8. Verification and docs

- [x] 8.1 `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` all pass
- [x] 8.2 Manual smoke test against a locally running master: `agents list` (json + `-o table`), `agents show <host>` for an existing and a missing host (verify exit codes), and the bare-command TUI showing/refreshing agents and quitting cleanly
- [x] 8.3 Update `CLAUDE.md` (CLI crate row → built) and add a short note recording the deliberate single-`--master-url`, no-file/env-layering divergence from master/agent config
