# Tasks: master-fault-dispatch

## 1. Contract groundwork (proto + agent)

- [x] 1.1 Add `ClearTaint {}` to the `ServerMessage` oneof in `faultforge.proto` (fresh field
      number); regenerate; both planes still build — agent logs it until 1.2, master never sends
      it until section 5
- [x] 1.2 Agent handles `ClearTaint`: remove the taint record idempotently, emit
      `TaintStatus { tainted: false }` (or `tainted: true` with a removal-failure reason);
      integration test via the existing fake master (tainted → clear → RunFault accepted;
      clear on clean host harmless; removal failure keeps quarantine)

## 2. Master catalog and config

- [x] 2.1 Extend `MasterConfig` with `catalog_root` (default `/usr/lib/faultforge/plugins`) and
      `default_grace_secs` (default 10) through the layered sources; config tests
- [x] 2.2 `catalog.rs` in `crates/master`: walk `catalog_root`, load entries via
      `faultforge_fault::catalog` (manifest + digest), log-and-skip broken entries, empty dir ⇒
      empty catalog; lookup by (name, version); tests with valid/broken/empty fixtures

## 3. Functional core: experiment record, validation, outcome

- [x] 3.1 Experiment-lite types: definition (`name`, `actions[{hosts, plugin, params,
      duration_secs}]`), record (id, grace, instance table, state `RUNNING`/`HALTING`/terminal,
      cause), `ExperimentId` + deterministic `instance_id` minting
      (`<experiment_id>:<hostname>:<action_index>`, charset-checked); unit tests
- [x] 3.2 Pure validation returning *all* failures: unknown plugin, params vs `params_schema`
      (shared crate fn), zero/over-max duration, unknown/disconnected/charset-violating/tainted
      host, duplicate host within an action; table-driven tests
- [x] 3.3 Pure kill-switch trigger predicate (first ERROR / unrequested ABORTED / in-scope taint
      / halt / deadline) and outcome classification (`ERROR` > `ABORTED` > `COMPLETED`, cause
      naming host/instance/reason/timestamp); table-driven tests including
      halt-then-taint ⇒ `ERROR`

## 4. Session plumbing: outbound map + frame intake

- [x] 4.1 Per-hostname session map (`Hostname → SessionHandle` wrapping the outbound sender):
      insert-supersede on `Register`, self-remove only-if-current on stream close; send-to-host
      API surfacing "no live session" as a normal outcome
- [x] 4.2 Frame intake replaces log-only arms: `InstanceStatus`/`InstanceReport` update the
      experiment store (report accepted as truth; unknown instances logged and dropped),
      `TaintStatus` updates the registry taint flag, `FaultEvent` stays log-only

## 5. Dispatch orchestration

- [x] 5.1 Accept path: validate (422 shape) → mint record → single-salvo `RunFault` fan-out over
      the session map → `RUNNING`; never re-issue `RunFault` (reconciliation is intake, 4.2)
- [x] 5.2 Kill-switch execution: on trigger, `AbortFault` to all non-terminal instances with
      live sessions, mark `HALTING`, record cause; unreachable agents tolerated
- [x] 5.3 Experiment deadline task: `start + max(duration+grace) + 30s margin`; unresolved
      instances ⇒ `ERROR` (unresolved reason) and outcome computed; no record left non-terminal
- [x] 5.4 Master restart amnesia honoured: store starts empty; unknown-instance frames logged
      (covered by 4.2 — assert in tests)

## 6. Management API

- [x] 6.1 `POST /experiments` (422 all-reasons / 201 full record), `POST /experiments/{id}/halt`
      (202/404/409), `GET /experiments`, `GET /experiments/{id}`
- [x] 6.2 `POST /agents/{hostname}/clear-taint` (404/409/202 → `ClearTaint` over session);
      `AgentView` gains `tainted` on both `GET /agents` endpoints
- [x] 6.3 Handler tests per scenario set (validation reasons, halt availability with a
      disconnected agent, clear-taint statuses, tainted visibility)

## 7. Integration tests: scripted fake agents against the real master

- [x] 7.1 Fake-agent harness: tonic client that registers, heartbeats, and plays scripted
      `InstanceStatus`/`InstanceReport`/`TaintStatus` sequences on cue; real master on
      `127.0.0.1:0` with temp catalog
- [x] 7.2 Happy path: 2 hosts × 2 actions → single salvo, deterministic ids, statuses tracked,
      all `DONE` ⇒ `COMPLETED`
- [x] 7.3 Kill-switch paths: instance `ERROR` ⇒ abort others ⇒ `ERROR`; agent self-abort ⇒
      `ABORTED`; taint mid-run ⇒ `ERROR` dominating; operator halt ⇒ `ABORTED` with cause
- [x] 7.4 Resilience: reconnect + `InstanceReport` reconciliation (no re-dispatch); vanished
      agent resolved by deadline as `ERROR`; frames for unknown instances after master restart
      logged, session stays open
- [x] 7.5 Clear-taint end-to-end: taint reported → visible in `GET /agents` → 409 when
      disconnected → 202 + `ClearTaint` → `TaintStatus false` → host targetable again

## 8. Operator CLI

- [x] 8.1 `experiment run -f <file>` (YAML/JSON → POST; print record; 422 reasons to stderr;
      `--wait` polling with distinct exit codes for `COMPLETED`/`ABORTED`/`ERROR`)
- [x] 8.2 `experiment list` / `show <id>` / `halt <id>` and `agents clear-taint <hostname>`;
      `-o json|table` honoured; HTTP-mock tests per scenario

## 9. Verification and docs

- [x] 9.1 `cargo build --workspace && cargo test --workspace && cargo clippy --workspace
      --all-targets -- -D warnings && cargo fmt --check`
- [x] 9.2 Manual smoke: master with a catalog dir holding `noop-marker`, real agent with temp
      `data_dir`; `experiment run --wait` a noop-marker fault to `COMPLETED`; halt a second run;
      chmod-break cleanup, observe taint, `agents clear-taint`
- [x] 9.3 Update CLAUDE.md (master status row, dispatch summary, new config keys, management
      plane no longer read-only + security note, CLI commands) and README
