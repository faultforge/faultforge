# Tasks: implement-fault-schema

## 1. Workspace scaffolding

- [x] 1.1 Add `crates/fault` and `crates/plugins/noop-marker` to workspace members
- [x] 1.2 Add workspace deps: `sha2`, `hex`, `serde_norway` (fallback: pinned `serde_yaml 0.9` — see design D5), `humantime`; `tempfile` as dev-dep

## 2. `faultforge-fault` contract crate

- [x] 2.1 `state.rs`: `InstanceState` enum (SCREAMING_CASE serde to match `"ACTIVE"` examples), `plugin_emittable()` predicate separating plugin-emittable from agent-owned states; unit tests
- [x] 2.2 `protocol.rs`: `PluginCommand` (argv word mapping), `Phase` (`install`/`runtime`), `PluginInput`, `PluginEvent` (`status`/`log` tagged by `type`), `Level`; serde round-trip tests against the exact JSON lines from the plugin spec
- [x] 2.3 `protocol.rs`: exit-code constants (0/10/20/30) plus a pure disposition classifier implementing the exit-code table; unit tests per row including "other non-zero"
- [x] 2.4 `params.rs`: `ParamType` (`string`/`int`/`bool`), schema map type, pure `validate_params` (missing / undeclared / type-mismatch as `thiserror` variants naming the parameter); unit tests
- [x] 2.5 `manifest.rs`: `Manifest` with `deny_unknown_fields`, `PluginName` newtype (`[a-z0-9-]`, non-empty), validation (relative entrypoint, `max_duration_secs > 0`); test parses the exact noop-marker manifest from the spec
- [x] 2.6 `digest.rs`: `Digest` newtype (64 lowercase hex, parse/display) + `sha256(manifest_bytes ‖ executable_bytes)`; test vector cross-checked against `cat a b | shasum -a 256`
- [x] 2.7 `catalog.rs`: `<plugin_root>/<name>@<version>/` path convention + `load_plugin(dir)` IO shell returning parsed manifest + computed digest; error cases (missing manifest / missing entrypoint); tempdir tests
- [x] 2.8 `lib.rs` re-exports + crate-level docs; pedantic clippy clean

## 3. Fault wire schema in proto

- [x] 3.1 Extend `faultforge.proto`: `RunFault`, `AbortFault` on `ServerMessage`; `FaultEvent`, `InstanceStatus`, `InstanceReport`, `TaintStatus` on `AgentMessage`; `InstanceState` enum with `UNSPECIFIED = 0`; `FetchArtifact` shape reserved as a comment block (ADR-0002 §4)
- [x] 3.2 Add log-and-continue arms for the new oneof variants in `crates/master` session handling and `crates/agent/src/session.rs`; register/heartbeat behaviour unchanged; existing tests still green

## 4. `noop-marker` plugin binary

- [x] 4.1 Crate scaffolding `crates/plugins/noop-marker` (sync bin; deps: `faultforge-fault`, `serde_json`, `humantime`, dev `tempfile`) + `manifest.yaml` exactly per the noop-marker spec
- [x] 4.2 `main.rs` imperative shell: argv command parse, read stdin `PluginInput`, single `SystemTime::now()` passed into pure logic, NDJSON emit on stdout, exit via `ExitCode::from(u8)` with contract constants
- [x] 4.3 `preflight`: absolute-path check; `create_dir_all` for missing `dirname(marker_path)`; probe-file writability check; emits `PREFLIGHT` + exit 0, or log + exit 10 — never touches the marker
- [x] 4.4 `inject`: atomic marker write (`NamedTempFile` in target dir → write JSON → `sync_all` → `persist`); marker content `{instance_id, plugin: "noop-marker@1", deadline_unix, params, written_at_unix}`; emits `INJECTING` → `ACTIVE`; log + exit 20 on IO error
- [x] 4.5 `report`: read-only probe — parseable marker → `ACTIVE`/0, absent → `DONE`/0, corrupt → log + exit 1, file untouched
- [x] 4.6 `abort` + `cleanup` sharing one revert function: removal emits `RECOVERING` → `DONE`/0; already-absent is `DONE`/0 (idempotent, re-issuable); only a real removal failure exits 30
- [x] 4.7 Manifest self-test: shipped `manifest.yaml` loads via `faultforge-fault` and matches the declared identity, entrypoint, params schema, precondition, and `max_duration_secs`

## 5. Golden tests (drive the real binary)

- [x] 5.1 Test helpers: spawn `env!("CARGO_BIN_EXE_noop-marker")` with stdin JSON in a tempdir, collect NDJSON + exit code; skip-if-root guard for permission-based cases
- [x] 5.2 `preflight` cases: success in both phases (no marker afterwards), relative path → 10, unwritable dir → 10
- [x] 5.3 `inject` cases: success (marker content + `INJECTING`/`ACTIVE` order), unwritable dir → 20 with no partial marker
- [x] 5.4 `report` cases: present → `ACTIVE`, absent → `DONE`, corrupt → non-zero with file byte-identical
- [x] 5.5 `abort`/`cleanup` cases: abort removes (`RECOVERING` → `DONE`); cleanup twice → both exit 0; `chmod 000` dir → 30
- [x] 5.6 Contract-shape assertion: every emitted line parses as `PluginEvent` and carries `ts` + `instance_id`

## 6. Docs and quality gate

- [x] 6.1 Update CLAUDE.md: crate table gains `crates/fault` and `crates/plugins/noop-marker`; note the fault wire schema is contract-only until `agent-fault-runtime`
- [x] 6.2 `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` all green
