# Design: add-disk-fill-plugin

## Context

The fault pipeline is proven end to end, but only against `noop-marker`, whose "fault" is a
marker file's existence. `disk-fill` (issue #62) is the first plugin whose injection genuinely
mutates the host: it consumes free disk space. The revert stays trivial (`rm` one file), which
is exactly why the Tier 1 epic (#18) recommends it as the first real plugin.

Constraints inherited from the platform:

- The `faultforge-fault` contract is fixed for this change: five argv commands, stdin
  `PluginInput` JSON, NDJSON events on stdout, the 0/10/20/30 exit-code table. Params are a
  closed schema of `string`/`int`/`bool`, every declared param required, no defaults (#85/#86
  are deliberately deferred).
- Plugins are sync binaries; no tokio/tonic, never the `proto` feature.
- The agent re-verifies the catalog digest before every invocation and enforces an invocation
  timeout (default 60s) per command.
- Two-phase preflight: the machine invokes `preflight` twice per instance (`install` phase,
  then `runtime`) with full params both times; `Phase` arrives in `PluginInput`.
- Instance ids are validated at the agent boundary to `[A-Za-z0-9._:-]` with no `.`/`..`
  components, so they are safe filename fragments on Linux filesystems.
- Journal replay after an agent restart issues `abort` + `cleanup` (never `inject`), so revert
  must be idempotent and must succeed from `(instance_id, params)` alone.
- De-facto preflight rule (pinned by `noop-marker`, pending doc reconciliation in #35):
  transient probes (create-and-remove) are allowed; persistent mutations are not.

## Goals / Non-Goals

**Goals:**

- A production-quality `disk-fill` plugin crate (`crates/plugins/disk-fill`, bin `disk-fill`)
  implementing the full five-command contract with golden tests, mirroring `noop-marker`'s
  test discipline.
- Never wedge a host: a hard headroom guarantee (the plugin refuses to fill a filesystem to
  100%), truthful `ABORTED`/"host unaffected" semantics on inject failure, revert that cannot
  fail into taint under normal filesystem semantics.
- E2e coverage in `crates/e2e` driving the real plugin through master → agent → host, with
  ground truth observed via `podman exec`.

**Non-Goals:**

- Write-IO generation (`dd`-style load) — that is `io-stress` (#67); this plugin prefers
  allocation without IO.
- A root-filesystem opt-in guard (`fill_dir` on `/` refused unless opted in) — wants optional
  params (#85); recorded as a known limitation instead.
- Multiple fill files, percentage-based sizing, or recurring re-fill — out of scope for T1.
- Any change to `crates/fault`, the agent runtime, the master, or the wire protocol.

## Decisions

### D1 — Allocation: `fallocate(2)` first, chunked write loop as documented fallback

`fallocate(2)` allocates real blocks instantly with no write IO — the exact "pure space
exhaustion" the issue asks for. Not every filesystem supports it (some network and exotic
filesystems return `EOPNOTSUPP`/`EINVAL`), and the e2e containers must work regardless of the
storage driver backing the volume, so on those errnos the plugin falls back to appending
zeroed chunks (8 MiB buffer) until the target size is reached, then `fsync`s. The fallback is
reported via a `log` event so an operator can tell which mechanism ran.

Alternatives considered:

- `ftruncate`/sparse file — rejected: allocates nothing, the disk never actually fills.
- `posix_fallocate` — rejected: glibc silently emulates unsupported filesystems with a slow
  byte-per-block write; we want the fallback explicit, observable, and chunked.
- write loop only — rejected: needless IO and wall-clock on the 99% case; a large fill could
  hit the agent's invocation timeout for no reason.

Consequence of the invocation timeout: on a no-`fallocate` filesystem, very large fills may
not complete within the agent's 60s budget. Documented in the manifest description; the
fallback exists for container/e2e-sized fills, not multi-terabyte ones.

**Allocation is verified, not trusted.** Some filesystems (notably FUSE-backed and network
ones) return success from `fallocate` without actually reserving blocks. After allocating —
by either mechanism — inject `fstat`s the file and requires `st_blocks × 512 >= size`;
a shortfall is treated as an allocation failure (unlink + exit 20). `st_size` alone proves
nothing: a sparse file has the right size and consumes no space, which would make the fault a
silent no-op.

### D2 — State tag: the fill file itself at a deterministic, id-derived path

`<fill_dir>/faultforge-<instance_id>.fill`. The path is recomputable from
`(instance_id, params)` alone, which makes cold-start revert trivial: `abort` and `cleanup`
both remove exactly that path, and an absent file is success (idempotent, replay-safe).

Because the filename *embeds the instance id*, ownership is established by construction —
unlike `noop-marker`, where the operator chooses `marker_path` and cross-instance confusion is
possible (issue #34). No content inspection is needed or useful (a `fallocate`d file has no
meaningful content): `report` checks existence only — present → `ACTIVE`, absent → `DONE`.
`abort`/`cleanup` remove the path unconditionally; any file at that exact name is ours by
construction.

### D3 — Safety headroom: hardcoded `max(5% of capacity, 64 MiB)`

Preflight (runtime phase) refuses with exit 10 unless
`available >= size_mib + max(capacity/20, 64 MiB)`. A percentage scales the guarantee to big
disks; the 64 MiB floor keeps it meaningful on small ones (and keeps small e2e volumes
usable). The headroom cannot be a param: the schema has no optional/defaulted params, and
making every operator pass a safety constant invites `headroom: 0`. The constant and formula
are documented in the manifest description.

Space is measured with `statvfs`'s `f_bavail` (blocks available to unprivileged users), not
`f_bfree`: it is the conservative bound and stays correct whether the plugin runs as root or
not.

Alternatives: required `headroom_mib` param (rejected — safety constants must not be
operator-erasable); fixed 512 MiB (rejected — unusable on small filesystems, weak on 10 TiB
ones).

### D4 — Two-phase preflight split: static in `install`, host probes in `runtime`

- `install` phase: pure param validation — `size_mib >= 1`, checked `size_mib × 1 MiB`
  arithmetic (overflow → exit 10), `fill_dir` absolute, and the composed fill path stays a
  direct child of `fill_dir` (defense in depth on top of the agent's id charset guarantee).
- `runtime` phase: everything from `install`, plus `fill_dir` exists and is a directory,
  writability via a transient probe (the honest check under ACLs, per the `noop-marker`
  precedent and the direction of #35), and the D3 headroom check.

The probe prefers `O_TMPFILE`: an anonymous file that never has a name in the directory and
vanishes with its descriptor, so a crashed preflight cannot leak it. Filesystems without
`O_TMPFILE` support fall back to `O_EXCL` create of a unique
`.faultforge-probe-<instance_id>` name, unlinked immediately after open (so even on the
fallback path the window for a leak is the syscall pair, not the whole command). Either way
the probe is the only host touch preflight makes.

**tmpfs is flagged, not refused.** Filling tmpfs consumes RAM, not disk — an operator
pointing `fill_dir` at `/tmp` on a tmpfs distro would silently get a memory fault instead of
a disk fault. Runtime preflight checks the filesystem's `statfs` `f_type` and emits a `warn`
`log` event when it is `TMPFS_MAGIC`. A warning, not exit 10: filling tmpfs deliberately is a
legitimate experiment; doing it unknowingly is the footgun, and the event removes the
"unknowingly".

### D5 — Inject hygiene: `O_EXCL` create, unlink on any failure past create

`inject` creates the fill file with `O_CREAT|O_EXCL`. The agent guarantees inject is issued at
most once per instance (duplicate `RunFault` ids are dropped; replay never re-injects), so a
pre-existing file at the tag path is an anomaly: inject exits 20 **without** unlinking it.

After a successful create, any failure (ENOSPC mid-allocation — the preflight check is
inherently TOCTOU — fallocate errno, write-loop error) unlinks the partial file before exiting
20. This keeps the machine's `ABORTED` reason "inject failed; host unaffected" truthful, which
matters: an inject failure kill-switches the whole experiment, and the operator must be able
to trust that the host needs no manual cleanup.

### D6 — Syscall access via `rustix`

`fallocate` and `statvfs` need a syscall crate. `rustix` provides safe, direct wrappers
(`rustix::fs::fallocate`, `rustix::fs::statvfs`) with no `unsafe` in our code, which keeps the
pedantic-clippy, no-unsafe posture of the workspace. Alternatives: raw `libc` (rejected:
`unsafe` blocks in a safety-critical plugin), `nix` (workable, but `rustix` is the leaner
dependency for exactly two calls).

### D7 — Revert failure semantics

`abort`/`cleanup`: `remove_file` where `ENOENT` is success; any other error (e.g. `EROFS`,
permission loss) is a real removal failure → exit 30, which lets the agent's
retry-cleanup-once-then-taint policy do its job. No retry logic inside the plugin — retry
ownership belongs to the runtime (ADR-0002).

### D8 — E2e scenarios extend the existing harness patterns

Four scenarios in `crates/e2e/tests/scenarios.rs` (all `#[ignore]`, rootless podman), reusing
the existing topology/wait helpers with the fill path under the agent's data volume:

1. **Happy path** — submit with `--wait`; while `ACTIVE`, `podman exec stat` shows the fill
   file at the exact expected byte size; after `COMPLETED` (exit 0) the file is gone.
2. **Operator halt** — halt while `ACTIVE`; experiment `ABORTED`, file removed.
3. **Preflight refusal** — `size_mib` far beyond the container filesystem's free space;
   experiment ends `ABORTED` (`--wait` exit 4), and the fill file never existed.
4. **Crash replay** — kill the agent container while `ACTIVE`, restart it; journal replay
   removes the file before reconnect completes.

`containers/Containerfile` builds `disk-fill` alongside `noop-marker` and installs it into the
image's plugin catalog (same layout + sha256 digest flow).

## Risks / Trade-offs

- **[TOCTOU on free space]** Space can vanish between preflight and inject → D5 unlinks the
  partial file and exits 20; the experiment aborts loudly with the host untouched.
- **[Fallback write loop vs 60s invocation timeout]** On no-`fallocate` filesystems a huge
  fill can time out; the agent kills the invocation and the machine recovers →
  documented limitation; the runtime's recover path removes the partial file via
  `abort`/`cleanup` (absent-or-present, removal is idempotent).
- **[Concurrent fills on one filesystem]** Two instances can each pass preflight and jointly
  breach the headroom → accepted for T1; blast-radius coordination is a master-side concern
  (#61), not a plugin-side one. The headroom still bounds each single instance.
- **[`:` in filenames]** Instance ids contain `:` (`<exp>:<host>:<idx>`), which is legal on
  Linux filesystems but hostile to FAT/exFAT → accepted: agents target bare-metal Linux; the
  data dir and fill dirs are expected to be native filesystems.
- **[Headroom formula is a constant]** Some operator will want to fill closer to the edge →
  deliberate: a safety constant that params cannot erase. Revisit only with #85 (optional
  params) and a conscious opt-in design.
- **[btrfs/ZFS space accounting]** `statvfs` on btrfs is approximate under RAID profiles
  (free space depends on the profile of future allocations), and ZFS reports pool-level
  numbers that CoW semantics can invalidate; ZFS also lacks `fallocate` entirely (the write
  loop covers allocation, and the D1 `st_blocks` verification catches CoW filesystems where
  even written blocks may not be accounted as expected) → accepted and documented in the
  manifest description as a known limitation: on CoW filesystems the headroom check is best
  effort. Not fixable plugin-side; the honest move is saying so.

## Migration Plan

Purely additive: a new workspace member and new catalog entries. No deploy or rollback steps
beyond including/excluding the plugin directory from a host's catalog. Nothing existing
changes behaviour.

## Open Questions

None — the headroom formula (D3) and the fallback policy (D1) are the two judgment calls, and
both are decided above; revisit them only if e2e sizing proves awkward.
