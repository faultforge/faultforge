## 1. Proto: finalize v1 register/heartbeat contract

- [x] 1.1 Trim `Register` in `crates/proto/proto/faultforge.proto` to `string hostname = 1;`
      only (remove `agent_id`, `name`, `os`, `arch`, `version`)
- [x] 1.2 Add `HeartbeatAck { int64 server_time_unix_ms = 1; }` and add
      `HeartbeatAck heartbeat_ack = 2;` to the `ServerMessage` oneof
- [x] 1.3 Update `crates/proto/src/lib.rs` unit tests for the trimmed `Register` and the new
      `HeartbeatAck` oneof variant (remove tests referencing removed fields)
- [x] 1.4 `cargo build -p faultforge-proto` to confirm codegen compiles with the new shapes

## 2. Master: config

- [x] 2.1 Add `tokio`, `tonic`, `faultforge-proto`, `tracing`, `tracing-subscriber`, `config`,
      `clap`, `serde` to `crates/master/Cargo.toml`
- [x] 2.2 Define a `MasterConfig` struct (`listen_addr: String` default `127.0.0.1:50051`,
      `heartbeat_interval_secs: u32` default `5`) loaded via layered `config` sources
      (defaults < optional file < env < CLI via `clap`)
- [x] 2.3 Unit test: `MasterConfig` loads defaults with no sources present; env/CLI overrides
      take precedence

## 3. Master: in-memory registry + Session handler

- [x] 3.1 Define `ConnInfo { hostname: String, name: String, last_seen: SystemTime }` and a
      registry type `Arc<Mutex<HashMap<String, ConnInfo>>>`
- [x] 3.2 Unit test: first `Register` for a hostname inserts a record with `name` seeded from
      `hostname`
- [x] 3.3 Unit test: a second `Register` for the same hostname replaces the existing record
      (supersede)
- [x] 3.4 Implement `AgentService::Session`: on `Register`, update the registry and reply
      `RegisterAck { server_time_unix_ms, heartbeat_interval_secs }` using the configured
      interval
- [x] 3.5 Implement `Heartbeat` handling: update the registry entry's `last_seen` and reply
      `HeartbeatAck { server_time_unix_ms }`
- [x] 3.6 Wire `main.rs`: load `MasterConfig`, start the tonic server on `listen_addr` with
      the `AgentService`, init `tracing-subscriber`

## 4. Agent: config

- [x] 4.1 Add `tokio`, `tonic`, `faultforge-proto`, `tracing`, `tracing-subscriber`, `config`,
      `clap`, `serde` to `crates/agent/Cargo.toml`
- [x] 4.2 Define an `AgentConfig` struct with mandatory `master_addr: String` (no default),
      loaded via the same layered `config` sources as the master
- [x] 4.3 Unit test: `AgentConfig` loading fails with a clear error when `master_addr` is not
      provided by any source

## 5. Agent: dial, register, heartbeat loop

- [x] 5.1 Read the local hostname at startup (no persisted state)
- [x] 5.2 Dial the configured `master_addr`, open the `Session` stream, send
      `Register { hostname }` as the first frame
- [x] 5.3 Await `RegisterAck`, extract `heartbeat_interval_secs`
- [x] 5.4 Heartbeat loop: send `Heartbeat {}` every `heartbeat_interval_secs`, await
      `HeartbeatAck` each time
- [x] 5.5 On dial failure, registration failure, or any stream error during the heartbeat loop:
      log the error and exit with a non-zero status (no reconnect/retry)
- [x] 5.6 Wire `main.rs`: load `AgentConfig`, init `tracing-subscriber`, run the
      dial/register/heartbeat flow

## 6. Integration tests

- [x] 6.1 In-process test: start the master's gRPC server and an agent client; assert the
      agent registers (registry entry appears with `name == hostname`) and heartbeats update
      `last_seen`
- [x] 6.2 In-process test: a second `Register` for the same hostname on a new stream
      supersedes the first registry entry

## 7. Documentation

- [x] 7.1 Update `CLAUDE.md`'s identity-model section: replace the two-field
      (`agent_id` + `name`) model with hostname-only identity, including the documented
      rename/duplicate-hostname limitation
- [x] 7.2 Update or remove the "Name reseed is first-registration-only" and "Supersede on
      reconnect" gotchas to match the hostname-keyed, in-memory, no-rename-console model

## 8. Verification

- [x] 8.1 `cargo test --workspace`
- [x] 8.2 `cargo clippy --workspace -- -D warnings`
- [x] 8.3 `cargo fmt --check`
- [x] 8.4 Manual smoke: run `faultforge-master`, run `faultforge-agent` pointed at it, confirm
      logs show `Register` → `RegisterAck` → repeated `Heartbeat`/`HeartbeatAck`
