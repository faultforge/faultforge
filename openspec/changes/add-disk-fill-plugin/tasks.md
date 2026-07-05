# Tasks: add-disk-fill-plugin

## 1. Crate scaffolding

- [x] 1.1 Create `crates/plugins/disk-fill` (bin `disk-fill`) mirroring the `noop-marker`
      layout; add to workspace members; deps: `faultforge-fault` (no `proto` feature),
      `serde_json`, `rustix` (fs feature)
- [x] 1.2 Write `manifest.yaml`: `disk-fill@1`, entrypoint `./disk-fill`, params
      `fill_dir: string` + `size_mib: int`, precondition `fill_dir_writable`,
      `max_duration_secs`; document the headroom formula and the fallocate-fallback in the
      manifest description
- [x] 1.3 Manifest test (as `noop-marker`'s `tests/manifest.rs`): shipped manifest parses and
      validates against the shared contract crate

## 2. Plugin core

- [x] 2.1 Argv/stdin shell: parse the five commands, read `PluginInput`, defense-in-depth
      re-validation of params (as `noop-marker` does), deterministic fill-path derivation
      `<fill_dir>/faultforge-<instance_id>.fill` with a containment check (direct child of
      `fill_dir`)
- [x] 2.2 `preflight`: install phase static checks (`size_mib >= 1`, checked byte-size
      arithmetic, absolute `fill_dir`); runtime phase adds dir-exists/is-dir, un-leakable
      writability probe (`O_TMPFILE`, fallback `O_EXCL` create + immediate unlink), the
      `statvfs` headroom check (`f_bavail`, `headroom = max(capacity/20, 64 MiB)`) — exit 10
      with a `log` event on each failure — and a `warn` `log` event (not a refusal) when
      `statfs` `f_type` is `TMPFS_MAGIC`
- [x] 2.3 `inject`: `O_CREAT|O_EXCL` create (pre-existing file → log + exit 20, untouched);
      `rustix::fs::fallocate`, on `EOPNOTSUPP`/`EINVAL` fall back to chunked zero-writes
      (8 MiB) + fsync with a `log` event announcing the fallback; verify allocation is real
      (`st_blocks × 512 >= size`, shortfall = failure); any failure after create unlinks the
      partial file before exit 20; success emits `INJECTING` then `ACTIVE`
- [x] 2.4 `report`: existence-only probe — exists → `ACTIVE`, absent → `DONE`, exit 0, no
      mutations
- [x] 2.5 `abort`/`cleanup`: remove the fill path, `ENOENT` = success (abort emits
      `RECOVERING`→`DONE`, cleanup `DONE`), other errors → log + exit 30

## 3. Golden tests

- [x] 3.1 Golden tests (as `noop-marker`'s `tests/golden.rs`) pinning: happy lifecycle
      (preflight install/runtime → inject → report ACTIVE → cleanup → report DONE), exact
      allocated size **and** block usage (`st_blocks` covers the request — sparse files must
      fail), cleanup idempotence, pre-existing-file refusal, partial-file removal on
      allocation failure, preflight refusals (relative dir, missing dir, unwritable dir,
      `size_mib < 1`, headroom breach), no probe residue after a runtime preflight, tmpfs
      `warn` event on a tmpfs-backed temp dir, traversal-hostile `fill_dir`/id handling
- [x] 3.2 `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check`
      clean

## 4. E2e coverage

- [x] 4.1 Extend `containers/Containerfile`: build `disk-fill` and install bin + manifest +
      sha256 digest into the image plugin catalog next to `noop-marker`
- [x] 4.2 Scenario: happy path — fill under the agent data volume, `podman exec stat` asserts
      exact byte size while `ACTIVE`, file gone after `COMPLETED` (`--wait` exit 0)
- [x] 4.3 Scenario: operator halt while `ACTIVE` → experiment `ABORTED`, file removed
- [x] 4.4 Scenario: preflight refusal — absurd `size_mib` → `ABORTED` (`--wait` exit 4), fill
      file never created
- [x] 4.5 Scenario: crash replay — kill+restart agent container while `ACTIVE`, journal
      replay removes the file, experiment reaches a terminal outcome
- [x] 4.6 Run `cargo test -p faultforge-e2e -- --ignored` green under rootless podman

## 5. Docs & bookkeeping

- [x] 5.1 Update `CLAUDE.md` crate table with `crates/plugins/disk-fill`
- [x] 5.2 Reference issue #62 in the PR; note the known limitations (no root-fs opt-in guard
      pending #85; fallback write loop vs the 60s invocation timeout; best-effort headroom on
      CoW filesystems — btrfs RAID-profile `statvfs` fuzziness, ZFS pool-level accounting and
      missing `fallocate`)
