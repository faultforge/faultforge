## 1. Dependencies

- [x] 1.1 Add `axum` and `serde_json` to `[workspace.dependencies]` in the root `Cargo.toml`
- [x] 1.2 Add `axum` and `serde_json` to `crates/master/Cargo.toml` (serde is already present)

## 2. Configuration

- [x] 2.1 Add `management_listen_addr: String` to `MasterConfig` in `crates/master/src/config.rs`
- [x] 2.2 Add the optional `--http-listen-addr` clap flag to `Cli`
- [x] 2.3 Wire the layered source in `load_config`: default `127.0.0.1:8069`, env `FAULTFORGE_MANAGEMENT_LISTEN_ADDR`, and CLI override
- [x] 2.4 Add a unit test asserting the default and override for `management_listen_addr`

## 3. HTTP module (pure core + shell)

- [x] 3.1 Create `crates/master/src/management.rs` with an `AgentView { hostname, name, last_seen_unix_ms }` DTO deriving `serde::Serialize`
- [x] 3.2 Implement the pure `agent_view(&AgentInfo) -> AgentView` mapping using `faultforge_proto::unix_ms`; add a unit test for it
- [x] 3.3 Define shared HTTP state holding the `Registry`
- [x] 3.4 Implement the `GET /agents` handler (locks registry, maps all entries, returns `200` with a JSON array — empty array when none)
- [x] 3.5 Implement the `GET /agents/{hostname}` handler (`200` with `AgentView`, `404` when absent)
- [x] 3.6 Build the axum `Router` wiring both routes to the shared state
- [x] 3.7 Expose the module via `pub use`/`pub mod` in `crates/master/src/lib.rs` as needed for tests

## 4. Server wiring

- [x] 4.1 Add an HTTP transport variant to `ServerError` in `crates/master/src/server.rs`
- [x] 4.2 Refactor `run_server`: lift `new_registry()` so the `Arc` is shared, clone into `MasterService` and into HTTP state
- [x] 4.3 Resolve `management_listen_addr` and start the axum server; run gRPC and HTTP concurrently via `tokio::try_join!` so a failure of either stops the master
- [x] 4.4 Log the HTTP listen address on startup (mirroring the existing gRPC startup log)

## 5. Verification

- [x] 5.1 Add an integration test (in `crates/master/tests/`) that starts the master, registers an agent over gRPC, and asserts `GET /agents` and `GET /agents/{hostname}` return the agent, plus `404` for an unknown hostname
- [x] 5.2 Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace`
- [x] 5.3 Manually verify with `curl http://127.0.0.1:8069/agents` against a running master with a connected agent
