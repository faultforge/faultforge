# Design: implement-fault-schema

## Context

ADR-0002 and the `fault-plugin-model` spec define the fault model behaviourally but leave the
concrete contract unpinned: no byte-level digest definition, no invocation protocol encoding, no
wire frames, no code. This change is the schema step of a four-change roadmap (see proposal):
everything here must be consumable unchanged by `agent-fault-runtime` (change 2),
`master-fault-dispatch` (change 3), and `fault-e2e-harness` (change 4). The reference plugin
`noop-marker` is built alongside the contract so the contract is proven by golden tests, not just
declared.

Constraints: CONVENTIONS.md is mandatory (functional core / imperative shell, time as data,
thiserror, newtypes, pedantic clippy, no unwrap/expect). Existing planes (register/heartbeat)
must not change behaviour.

## Goals / Non-Goals

**Goals:**

- One shared crate that is the single source of truth for manifest, params, invocation protocol,
  lifecycle states, digest, and catalog layout.
- The complete fault wire schema in `faultforge.proto`, so changes 2 and 3 add behaviour without
  reshaping the proto.
- A fully working, fully golden-tested `noop-marker` binary — the executable fixture of the
  contract.

**Non-Goals:**

- No agent execution of plugins, no journal/timer/watchdog, no TAINTED behaviour (change 2).
- No master dispatch, no catalog file on the master, no management-API writes (change 3).
- No packaging/distribution of the catalog onto hosts, no e2e (change 4).
- No `FetchArtifact` implementation (ADR-0002 §4: v2).

## Decisions

### D1. Shared contract crate `crates/fault` (package `faultforge-fault`)

Module layout per CONVENTIONS §9: `manifest.rs` (manifest types + YAML parsing + validation),
`params.rs` (schema language + pure `validate_params`), `protocol.rs` (`PluginCommand`, `Phase`,
`PluginInput`, `PluginEvent`, exit-code table + pure classifier), `state.rs` (`InstanceState`),
`digest.rs` (`Digest` newtype + computation), `catalog.rs` (on-disk layout + loading shell).
The crate is sync, non-async, and free of tonic/tokio so plugin binaries stay lean.

- *Alternative — put types in `faultforge-proto`*: rejected; the proto crate is generated wire
  code, while this crate holds domain types, validation, and IO-free logic testable without gRPC.
- *Deferred*: `proto ⇄ domain` conversions for `InstanceState` land in change 2 behind a `proto`
  feature flag on `faultforge-fault`, so `noop-marker` never links the gRPC stack.

### D2. Digest = `sha256(manifest_bytes ‖ executable_bytes)`

Plain concatenation — manifest first, entrypoint executable second, no framing — encoded as
lowercase hex in a `Digest` newtype (64 chars, parse-validated). An operator reproduces it with
coreutils: `cat manifest.yaml noop-marker | sha256sum`.

- *Alternative — length-prefixed framing* (kills theoretical boundary-shift malleability):
  rejected. The digest provides integrity pinning, not authenticity (ADR-0002 §3 places safety
  on the vetted catalog); bytes shifted across the boundary break YAML or ELF parsing anyway;
  coreutils reproducibility is worth more than closing a non-attack.
- *Alternative — digest inside the manifest*: already rejected by ADR-0002 §3 (a file cannot
  hash itself).

### D3. Wire schema: extend the `Session` oneofs; no new RPC

The `Session` stream is the designated channel for fault commands (slice-1 architecture note).
New oneof variants are appended with fresh field numbers:

```proto
// ServerMessage additions
RunFault   run_fault   = 3;   // instance_id, plugin_name, plugin_version, plugin_digest,
                              // params_json, duration_secs, grace_secs
AbortFault abort_fault = 4;   // instance_id

// AgentMessage additions
FaultEvent     fault_event     = 3;  // instance_id + verbatim NDJSON line
InstanceStatus instance_status = 4;  // agent-authoritative transition: state, ts_unix_ms, reason
InstanceReport instance_report = 5;  // reconciliation snapshot: repeated InstanceStatus
TaintStatus    taint_status    = 6;  // tainted, reason, ts_unix_ms
```

plus `enum InstanceState { UNSPECIFIED, PENDING, PREFLIGHT, INJECTING, ACTIVE, RECOVERING, DONE,
ABORTED, ERROR }`.

- **Two-track telemetry.** `FaultEvent` carries the plugin's NDJSON line *verbatim* (the plugin
  spec requires verbatim forwarding; operators get full fidelity). `InstanceStatus` is the
  agent's own state machine speaking — the master tracks lifecycle from this track only. A buggy
  or malicious plugin can therefore never assert `ABORTED`/`ERROR`/`DONE` into the master's
  bookkeeping by printing JSON.
- **`grace_secs` rides in `RunFault`.** The master decides grace exactly like it decides
  `heartbeat_interval_secs` — a directive, not agent config. The agent computes
  `deadline_unix = start + duration + grace` and hands it to the plugin.
- **`params_json` is a JSON string**, not `map<string,string>`: keeps the wire compatible with
  future non-string param types; both ends validate against `params_schema` with the same pure
  function.
- **`FetchArtifact` is reserved as a documented comment block** in the proto (shape:
  `FetchArtifact(digest) -> stream Chunk`), not a declared RPC. Declaring it would force an
  `UNIMPLEMENTED` stub on the master and advertise surface that ADR-0002 §4 explicitly defers.
- **Consequence:** adding oneof variants breaks the exhaustive matches in
  `crates/master/src/server.rs` / `crates/agent/src/session.rs`. This change adds
  log-and-continue arms. That compile-time forcing is a feature: changes 2/3 cannot forget a
  frame. Requirement: an endpoint receiving a fault frame it does not handle logs and continues
  the session — this also covers mixed-version rollouts permanently.

### D4. Params schema language: typed, required, closed

`params_schema` is a map `param_name -> type`, v1 types `string | int | bool`. All declared
params are required, no defaults, undeclared params are rejected. Validation is one pure
function used later by both master (before dispatch) and agent (before run).

- *Alternative — JSON Schema*: rejected for v1; a full schema language is unneeded for a vetted
  first-party catalog and would dominate the audit surface of every manifest.
- *Alternative — `string` only* (all noop-marker needs): rejected; `int`/`bool` cost one match
  arm each now and their absence would force a schema-language change in the first real plugin
  (`kill-process` needs at least a signal/int).

### D5. Manifest parsing: strict serde, maintained YAML crate

`Manifest` derives serde with `deny_unknown_fields` — the manifest is a contract, and unknown
keys are typos or drift, not extensions (the digest covers exact bytes anyway). Validation
beyond shape (non-empty name charset `[a-z0-9-]`, relative entrypoint, `max_duration_secs > 0`)
lives in `Manifest::validate`.

YAML via **`serde_norway`** (maintained drop-in fork of the archived `serde_yaml`; identical
API). Fallback if it proves unsuitable at implementation time: pin `serde_yaml 0.9` (frozen but
stable and ubiquitous). The `config` crate's YAML support is not reused here — manifests need
direct, strict serde derives, not layered config semantics.

### D6. On-disk catalog layout

`<plugin_root>/<name>@<version>/` containing `manifest.yaml` plus the entrypoint at the
manifest-declared relative path. `catalog.rs` provides the path convention and
`load_plugin(dir) -> LoadedPlugin { manifest, digest }` (reads both files, computes the digest —
the IO shell around pure parsing/hashing). Version-qualified directories allow side-by-side
versions; the agent's default `plugin_root` (`/usr/lib/faultforge/plugins`) becomes agent config
in change 2.

### D7. `noop-marker`: lean sync binary, honest checks, atomic writes

- **Crate** `crates/plugins/noop-marker`, plain sync std + `faultforge-fault` + `serde_json` +
  `humantime` + `tempfile`. No tokio/tonic: the plugin runs one command and exits.
- **Time as data** (CONVENTIONS §2): `main` reads `SystemTime::now()` once and passes it in; the
  command logic is pure over `(command, input, now)` with FS effects in thin shell functions.
  RFC3339 `ts` via `humantime::format_rfc3339_seconds` — exactly the spec's `Z`-suffixed format,
  one tiny dep instead of chrono.
- **Preflight writability check = probe file**: create-and-remove a temp file in
  `dirname(marker_path)` (creating the missing directory first is allowed by the plugin spec).
  Permission-bit inspection lies under ACLs; the probe is the honest check. The probe is not the
  marker file, so preflight stays side-effect-free w.r.t. the marker.
- **Atomic inject**: `tempfile::NamedTempFile` in the marker's directory → write JSON →
  `sync_all` → `persist` (rename). Marker content:
  `{instance_id, plugin: "noop-marker@1", deadline_unix, params, written_at_unix}`.
- **Exit codes**: `std::process::ExitCode::from(u8)` with the constants from
  `faultforge-fault::protocol`; `report` on a corrupt marker exits `1` (generic unexpected —
  the agent will treat the instance as ambiguous per ADR-0002 §12).
- **Golden tests**: integration tests in the crate drive the real binary via
  `env!("CARGO_BIN_EXE_noop-marker")` + `std::process::Command` in `tempfile` dirs — no extra
  test framework. Permission-based cases (`exit 10` unwritable dir, `exit 20`, `exit 30` via
  `chmod 000`) are skipped when running as root (root ignores permission bits).

## Risks / Trade-offs

- **[Contract designed before its runtime]** Change 2 may discover friction in the protocol
  types. → Mitigation: `noop-marker`'s golden tests are executable fixtures of the contract;
  only one plugin exists, so a delta spec in change 2 is cheap. The proto is additive either
  way.
- **[Concatenation digest is theoretically malleable at the file boundary]** → Accepted
  deliberately (see D2): integrity-pinning only; safety rests on the vetted catalog per
  ADR-0002 §3; reproducibility with `cat | sha256sum` is an operator feature.
- **[`serde_norway` is a fork]** → Fallback path documented (D5); the manifest surface is tiny,
  switching YAML crates later is a one-file change in `faultforge-fault`.
- **[New oneof variants force edits in master/agent]** → Intended; the arms are
  log-and-continue only, covered by existing tests still passing.
- **[Root-skipped permission tests]** Golden tests for exit 10/20/30 silently skip under root.
  → Acceptable: CI and dev machines run unprivileged; the e2e change re-tests TAINTED semantics
  in containers as non-root.

## Migration Plan

Purely additive — two new crates, appended proto fields, ignore-arms. No deployment or data
migration; existing agents/masters interoperate (unknown oneof variants decode to an empty
payload, which is logged and skipped).

## Open Questions

- `requires.privileges` vocabulary: v1 recognizes an empty list and `root`; anything richer
  (capabilities, specific users) waits for the first plugin that needs it (likely
  `kill-process` in the catalog change after the roadmap).
- Whether `InstanceReport` should also carry per-instance `plugin_digest` for reconciliation
  auditing — decide in change 2 when the journal shape is fixed; adding a field is
  wire-compatible.
