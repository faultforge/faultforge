# Proposal: add-disk-fill-plugin

## Why

FaultForge's entire fault pipeline (contract, agent runtime, master dispatch, e2e harness) has
so far been proven only against `noop-marker`, a fault with zero blast radius. `disk-fill`
(GitHub issue #62) is the designated first *real* fault: it consumes disk space — a genuine
host mutation — while keeping the gentlest possible revert (`rm` one file). It is the
recommended Tier 1 entry point ("build order: disk-fill → cpu-load → process-kill") and has no
prerequisites: it needs no shared helper (#82) and no contract extension (#83–#87) — the issue's
"fits existing types" claim holds against the current `faultforge-fault` schema.

## What Changes

- New crate `crates/plugins/disk-fill` (bin `disk-fill`): a fault plugin that allocates a file
  of `size_mib` MiB inside `fill_dir` to consume free space, modelling a full disk.
- Follows the `noop-marker` pattern: sync, depends only on `faultforge-fault` (never the
  `proto` feature), manifest + golden tests pin the wire behaviour.
- Deterministic state tag: the fill file itself at `<fill_dir>/faultforge-<instance_id>.fill`;
  `abort`/`cleanup` remove exactly that path, absent = success (idempotent, replay-safe).
- Allocation via `fallocate(2)` (instant, no write IO); documented fallback to a chunked write
  loop when the filesystem does not support it (tmpfs/overlayfs in containers, and the e2e
  suite, need this).
- Safety headroom: preflight refuses (exit 10) unless free space ≥ `size_mib` + a hardcoded
  headroom, so the plugin can never fill a filesystem to 100%.
- Inject-failure hygiene: a partially allocated file is unlinked before exiting 20, keeping the
  runtime's "inject failed; host unaffected" `ABORTED` reason truthful.
- New e2e scenarios in `crates/e2e` driving the real disk-fill plugin through the operator
  surface, with host ground truth via `podman exec`; the plugin is added to the container
  image's plugin catalog in `containers/Containerfile`.

Out of scope (documented as such): write-IO generation (`io-stress`, #67), an opt-in guard
refusing `fill_dir` on the root filesystem (wants optional params, #85), any contract changes.

## Capabilities

### New Capabilities

- `disk-fill-plugin`: the disk-fill fault plugin — manifest, params, two-phase preflight,
  fallocate-or-fallback inject, ownership-aware report, idempotent abort/cleanup, safety
  headroom.

### Modified Capabilities

- `fault-e2e-harness`: the suite grows a requirement to cover a real host-mutating fault
  (disk-fill) end to end — happy path, halt, preflight refusal, crash-replay — alongside the
  existing noop-marker lifecycle scenarios.

## Impact

- **New code**: `crates/plugins/disk-fill` (workspace member), its `manifest.yaml`, golden +
  manifest tests.
- **Modified**: `Cargo.toml` (workspace members), `containers/Containerfile` (build + install
  the plugin into the image catalog), `crates/e2e/tests/scenarios.rs` (new scenarios),
  `CLAUDE.md` crate table.
- **No changes** to `crates/fault`, `crates/agent`, `crates/master`, `crates/proto`, or the
  wire protocol.
- **Dependencies**: the plugin gains a `libc` (or `nix`) dependency for `fallocate(2)`/
  `statvfs(3)`; no async runtime, no tonic.
- **GitHub**: implements issue #62 under epic #18.
