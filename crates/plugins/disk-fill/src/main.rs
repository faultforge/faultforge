//! `disk-fill`: consume free space on a filesystem (the first host-mutating
//! fault, issue #62).
//!
//! The fault's entire host effect is one file at the deterministic path
//! `<fill_dir>/faultforge-<instance_id>.fill`, recomputable from
//! `(instance_id, params)` alone so a cold-start revert needs no memory of the
//! inject. Allocation prefers `fallocate(2)` (instant, no write IO) and falls
//! back to chunked zero-writes where the filesystem lacks it. The plugin can
//! never fill a filesystem to 100%: preflight demands
//! `size + max(capacity/20, 64 MiB)` available. See `manifest.yaml` and the
//! `disk-fill-plugin` spec.
//!
//! The binary is a thin imperative shell: `main` reads `SystemTime::now()`
//! once and threads it into per-command logic; the space math and path
//! derivation are pure functions unit-tested below.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::SystemTime;

use faultforge_fault::{
    EXIT_CLEANUP_FAILED, EXIT_INJECT_FAILED, EXIT_PREFLIGHT_FAILED, EXIT_SUCCESS, InstanceId,
    InstanceState, Level, Phase, PluginCommand, PluginEvent, PluginInput,
};

/// Generic unexpected-failure exit code (an "other non-zero" per the contract).
/// Used for malformed stdin/argv and contract violations (params or an
/// instance id that the agent's own validation should have refused).
const EXIT_UNEXPECTED: u8 = 1;

const MIB: u64 = 1024 * 1024;

/// Headroom floor: even on a tiny filesystem at least this much stays free.
const HEADROOM_FLOOR_BYTES: u64 = 64 * MIB;

/// Headroom scale: 1/20th (5%) of filesystem capacity.
const HEADROOM_CAPACITY_DIVISOR: u64 = 20;

/// Chunk size for the no-`fallocate` fallback write loop.
const WRITE_CHUNK_BYTES: usize = 8 * 1024 * 1024;

/// `TMPFS_MAGIC` from `linux/magic.h`: filling tmpfs consumes memory, not disk,
/// which preflight surfaces as a warning.
#[cfg(target_os = "linux")]
const TMPFS_MAGIC: i64 = 0x0102_1994;

/// What a command decided: the NDJSON events to emit and the process exit code.
struct Outcome {
    events: Vec<PluginEvent>,
    exit: u8,
}

fn main() -> ExitCode {
    // Time as data: read the wall clock once, thread it into pure logic.
    let now = SystemTime::now();

    let Some(word) = std::env::args().nth(1) else {
        eprintln!("usage: disk-fill <preflight|inject|report|abort|cleanup>");
        return ExitCode::from(EXIT_UNEXPECTED);
    };
    let command = match PluginCommand::parse(&word) {
        Ok(command) => command,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::from(EXIT_UNEXPECTED);
        }
    };

    let input = match read_input() {
        Ok(input) => input,
        Err(err) => {
            eprintln!("failed to read plugin input: {err}");
            return ExitCode::from(EXIT_UNEXPECTED);
        }
    };

    let outcome = run(command, &input, now);
    emit_all(&outcome.events);
    ExitCode::from(outcome.exit)
}

/// Read and parse the single stdin JSON [`PluginInput`].
fn read_input() -> Result<PluginInput, String> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&buf).map_err(|e| e.to_string())
}

/// Write every event to stdout as one NDJSON line, in order.
fn emit_all(events: &[PluginEvent]) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    for event in events {
        // Serializing these types cannot fail; if a line somehow can't be written
        // (e.g. a closed pipe), there is nothing useful the plugin can do.
        if let Ok(line) = serde_json::to_string(event) {
            let _ = writeln!(lock, "{line}");
        }
    }
}

/// Dispatch a command over its validated input and the captured time.
fn run(command: PluginCommand, input: &PluginInput, now: SystemTime) -> Outcome {
    // In production the agent validates params against the schema first; a
    // missing/mistyped param here is a contract violation, not a precondition
    // failure.
    let Some(fill_dir) = input.params.get("fill_dir").and_then(|v| v.as_str()) else {
        return fail(
            input,
            now,
            EXIT_UNEXPECTED,
            "params.fill_dir missing or not a string".to_string(),
        );
    };
    let Some(size_mib) = input
        .params
        .get("size_mib")
        .and_then(serde_json::Value::as_i64)
    else {
        return fail(
            input,
            now,
            EXIT_UNEXPECTED,
            "params.size_mib missing or not an integer".to_string(),
        );
    };
    let fill_dir = Path::new(fill_dir);

    // Defense in depth: a relative fill_dir is a precondition failure (exit 10)
    // in preflight; any other command receiving one means the agent skipped
    // preflight — a contract violation — so refuse rather than resolve it
    // against the process cwd.
    if !fill_dir.is_absolute() {
        let exit = if command == PluginCommand::Preflight {
            EXIT_PREFLIGHT_FAILED
        } else {
            EXIT_UNEXPECTED
        };
        return fail(
            input,
            now,
            exit,
            format!("fill_dir is not absolute: {}", fill_dir.display()),
        );
    }

    // The agent's boundary validation confines instance ids to a filename-safe
    // charset; an id that composes to anything but a direct child of fill_dir
    // is a contract violation for every command, preflight included.
    let fill_path = match fill_file_path(fill_dir, &input.instance_id) {
        Ok(path) => path,
        Err(msg) => return fail(input, now, EXIT_UNEXPECTED, msg),
    };

    match command {
        PluginCommand::Preflight => preflight(input, now, fill_dir, &fill_path, size_mib),
        PluginCommand::Inject => inject(input, now, &fill_path, size_mib),
        PluginCommand::Report => report(input, now, &fill_path),
        PluginCommand::Abort => revert(input, now, &fill_path, RevertKind::Abort),
        PluginCommand::Cleanup => revert(input, now, &fill_path, RevertKind::Cleanup),
    }
}

// ===== commands =====

/// Static param checks in both phases; host probes (directory, writability,
/// headroom, tmpfs warning) only in `runtime`. Apart from the transient
/// anonymous probe, preflight never mutates the host.
fn preflight(
    input: &PluginInput,
    now: SystemTime,
    fill_dir: &Path,
    fill_path: &Path,
    size_mib: i64,
) -> Outcome {
    let size = match size_bytes(size_mib) {
        Ok(size) => size,
        Err(msg) => return fail(input, now, EXIT_PREFLIGHT_FAILED, msg),
    };

    let mut events = Vec::new();
    if input.phase == Phase::Runtime {
        match std::fs::metadata(fill_dir) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return fail(
                    input,
                    now,
                    EXIT_PREFLIGHT_FAILED,
                    format!("fill_dir is not a directory: {}", fill_dir.display()),
                );
            }
            Err(e) => {
                return fail(
                    input,
                    now,
                    EXIT_PREFLIGHT_FAILED,
                    format!("fill_dir {} is not usable: {e}", fill_dir.display()),
                );
            }
        }
        if fill_path.exists() {
            return fail(
                input,
                now,
                EXIT_PREFLIGHT_FAILED,
                format!(
                    "fill file already exists: {} — was this instance already injected?",
                    fill_path.display()
                ),
            );
        }
        if let Err(msg) = probe_writable(fill_dir) {
            return fail(input, now, EXIT_PREFLIGHT_FAILED, msg);
        }
        if let Err(msg) = check_headroom(fill_dir, size) {
            return fail(input, now, EXIT_PREFLIGHT_FAILED, msg);
        }
        if is_tmpfs(fill_dir) {
            events.push(log(
                input,
                now,
                Level::Warn,
                format!(
                    "{} is tmpfs: this fill consumes memory, not disk",
                    fill_dir.display()
                ),
            ));
        }
    }

    events.push(status(input, now, InstanceState::Preflight));
    Outcome {
        events,
        exit: EXIT_SUCCESS,
    }
}

/// Create the fill file exclusively, allocate it, verify the allocation is
/// backed by real blocks, then exit; the fault's active state is purely the
/// file's existence. Any failure after the create removes the partial file so
/// exit 20 always means the host is unaffected.
fn inject(input: &PluginInput, now: SystemTime, fill_path: &Path, size_mib: i64) -> Outcome {
    let size = match size_bytes(size_mib) {
        Ok(size) => size,
        // Preflight already vetted the size; reaching inject with a bad one is
        // a contract violation, and nothing has touched the host yet.
        Err(msg) => return fail(input, now, EXIT_UNEXPECTED, msg),
    };

    let mut events = vec![status(input, now, InstanceState::Injecting)];

    let file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(fill_path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            // Not ours to clobber: inject runs at most once per instance, so a
            // file already at this path is an anomaly worth preserving.
            events.push(log(
                input,
                now,
                Level::Error,
                format!(
                    "fill file already exists, refusing to claim it: {}",
                    fill_path.display()
                ),
            ));
            return Outcome {
                events,
                exit: EXIT_INJECT_FAILED,
            };
        }
        Err(e) => {
            events.push(log(
                input,
                now,
                Level::Error,
                format!("cannot create fill file {}: {e}", fill_path.display()),
            ));
            return Outcome {
                events,
                exit: EXIT_INJECT_FAILED,
            };
        }
    };

    let mechanism = match allocate(&file, size) {
        Ok(mechanism) => mechanism,
        Err(e) => {
            return abandon_inject(
                input,
                now,
                events,
                fill_path,
                format!("allocation failed: {e}"),
            );
        }
    };

    // Trust nothing: some filesystems report fallocate success without
    // reserving blocks, and a sparse fill would be a silent no-op fault.
    match file.metadata() {
        Ok(meta) if allocation_backed(meta.len(), meta.blocks(), size) => {}
        Ok(meta) => {
            return abandon_inject(
                input,
                now,
                events,
                fill_path,
                format!(
                    "allocation is not backed by blocks ({} of {size} bytes) — refusing a silent no-op fault",
                    meta.blocks().saturating_mul(512)
                ),
            );
        }
        Err(e) => {
            return abandon_inject(
                input,
                now,
                events,
                fill_path,
                format!("cannot verify allocation: {e}"),
            );
        }
    }

    if mechanism == Mechanism::WriteLoop {
        events.push(log(
            input,
            now,
            Level::Warn,
            "fallocate unavailable on this filesystem; filled by chunked writes",
        ));
    }
    events.push(status(input, now, InstanceState::Active));
    Outcome {
        events,
        exit: EXIT_SUCCESS,
    }
}

/// Read-only reconciliation probe: ownership is the id-derived filename, so
/// existence alone answers it; content is never inspected.
fn report(input: &PluginInput, now: SystemTime, fill_path: &Path) -> Outcome {
    match std::fs::metadata(fill_path) {
        Ok(_) => Outcome {
            events: vec![status(input, now, InstanceState::Active)],
            exit: EXIT_SUCCESS,
        },
        Err(e) if e.kind() == ErrorKind::NotFound => Outcome {
            events: vec![status(input, now, InstanceState::Done)],
            exit: EXIT_SUCCESS,
        },
        Err(e) => fail(
            input,
            now,
            EXIT_UNEXPECTED,
            format!("cannot stat fill file: {e}"),
        ),
    }
}

/// Which revert path is running; they share the removal but differ in the
/// telemetry the spec pins (`abort` announces `RECOVERING` first).
#[derive(Clone, Copy, PartialEq, Eq)]
enum RevertKind {
    Abort,
    Cleanup,
}

/// Remove the fill file; idempotent (already-absent is success). Retry
/// ownership on real failures belongs to the agent runtime, not here.
fn revert(input: &PluginInput, now: SystemTime, fill_path: &Path, kind: RevertKind) -> Outcome {
    let mut events = Vec::new();
    if kind == RevertKind::Abort {
        events.push(status(input, now, InstanceState::Recovering));
    }
    match std::fs::remove_file(fill_path) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => {
            events.push(log(
                input,
                now,
                Level::Error,
                format!("failed to remove fill file: {e}"),
            ));
            return Outcome {
                events,
                exit: EXIT_CLEANUP_FAILED,
            };
        }
    }
    events.push(status(input, now, InstanceState::Done));
    Outcome {
        events,
        exit: EXIT_SUCCESS,
    }
}

// ===== allocation =====

/// How the fill file's space was obtained.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mechanism {
    // Only the Linux `allocate` constructs this; the dev-platform build still
    // needs the variant so the match sites stay platform-independent.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Fallocate,
    WriteLoop,
}

/// Allocate `size` bytes in `file`: `fallocate(2)` where the platform and
/// filesystem support it, otherwise the chunked write loop. The final
/// `sync_all` in the write path also forces delayed allocation to materialise
/// so the caller's `st_blocks` verification measures reality.
#[cfg(target_os = "linux")]
fn allocate(file: &File, size: u64) -> std::io::Result<Mechanism> {
    use rustix::io::Errno;
    match rustix::fs::fallocate(file, rustix::fs::FallocateFlags::empty(), 0, size) {
        Ok(()) => {
            file.sync_all()?;
            Ok(Mechanism::Fallocate)
        }
        Err(Errno::OPNOTSUPP | Errno::INVAL | Errno::NOSYS) => {
            write_zeroes(file, size)?;
            Ok(Mechanism::WriteLoop)
        }
        Err(e) => Err(e.into()),
    }
}

/// Non-Linux fallback (the dev platform): no `fallocate(2)`, always the write
/// loop. Production agents target Linux.
#[cfg(not(target_os = "linux"))]
fn allocate(file: &File, size: u64) -> std::io::Result<Mechanism> {
    write_zeroes(file, size)?;
    Ok(Mechanism::WriteLoop)
}

/// Append zeroed chunks until `size` bytes are written, then fsync.
fn write_zeroes(mut file: &File, size: u64) -> std::io::Result<()> {
    let chunk = vec![0_u8; WRITE_CHUNK_BYTES];
    let mut remaining = size;
    while remaining > 0 {
        let len = usize::try_from(remaining.min(chunk.len() as u64))
            .map_err(|_| std::io::Error::other("chunk length exceeds usize"))?;
        file.write_all(&chunk[..len])?;
        remaining -= len as u64;
    }
    file.sync_all()
}

/// Push a failure log, remove the partial fill file, and exit 20. Removal keeps
/// the machine's "inject failed; host unaffected" reason truthful; a removal
/// failure at this point (vanishingly unlikely right after a successful create)
/// is logged so the operator knows the host needs a manual look.
fn abandon_inject(
    input: &PluginInput,
    now: SystemTime,
    mut events: Vec<PluginEvent>,
    fill_path: &Path,
    msg: String,
) -> Outcome {
    events.push(log(input, now, Level::Error, msg));
    match std::fs::remove_file(fill_path) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => {
            events.push(log(
                input,
                now,
                Level::Error,
                format!(
                    "could not remove partial fill file {}: {e}",
                    fill_path.display()
                ),
            ));
        }
    }
    Outcome {
        events,
        exit: EXIT_INJECT_FAILED,
    }
}

// ===== host probes =====

/// Honest writability check: an actual write, not permission bits (which lie
/// under ACLs). `tempfile_in` is un-leakable by construction — `O_TMPFILE`
/// (anonymous) on Linux, create+immediate-unlink elsewhere — so a crashed
/// preflight leaves nothing named behind.
fn probe_writable(fill_dir: &Path) -> Result<(), String> {
    let mut probe = tempfile::tempfile_in(fill_dir)
        .map_err(|e| format!("fill_dir {} is not writable: {e}", fill_dir.display()))?;
    probe
        .write_all(b"faultforge disk-fill probe")
        .map_err(|e| format!("fill_dir {} is not writable: {e}", fill_dir.display()))
}

/// The headroom guarantee: refuse unless `available >= size + headroom`.
/// Measured with `f_bavail` (blocks available to unprivileged users), the
/// conservative bound whether or not the plugin runs as root.
fn check_headroom(fill_dir: &Path, size: u64) -> Result<(), String> {
    let vfs = rustix::fs::statvfs(fill_dir)
        .map_err(|e| format!("cannot statvfs {}: {e}", fill_dir.display()))?;
    let available = vfs.f_bavail.saturating_mul(vfs.f_frsize);
    let capacity = vfs.f_blocks.saturating_mul(vfs.f_frsize);
    if headroom_allows(size, available, capacity) {
        Ok(())
    } else {
        Err(format!(
            "refusing to fill: {size} bytes requested but only {available} available and \
             {headroom} headroom must stay free (capacity {capacity})",
            headroom = required_headroom(capacity)
        ))
    }
}

#[cfg(target_os = "linux")]
fn is_tmpfs(fill_dir: &Path) -> bool {
    rustix::fs::statfs(fill_dir).is_ok_and(|fs| fs.f_type == TMPFS_MAGIC)
}

#[cfg(not(target_os = "linux"))]
fn is_tmpfs(_fill_dir: &Path) -> bool {
    false
}

// ===== pure core =====

/// The deterministic fill-file path, verified to be a direct child of
/// `fill_dir` (defense in depth on top of the agent's id-charset guarantee).
fn fill_file_path(fill_dir: &Path, instance_id: &InstanceId) -> Result<PathBuf, String> {
    let name = format!("faultforge-{instance_id}.fill");
    let path = fill_dir.join(&name);
    let contained = path.parent() == Some(fill_dir)
        && path.file_name().and_then(|f| f.to_str()) == Some(name.as_str());
    if contained {
        Ok(path)
    } else {
        Err(format!(
            "instance id composes a fill path outside fill_dir: {instance_id}"
        ))
    }
}

/// The requested size in bytes; `size_mib` must be >= 1 and must not overflow.
fn size_bytes(size_mib: i64) -> Result<u64, String> {
    let mib = u64::try_from(size_mib).ok().filter(|&m| m >= 1);
    mib.and_then(|m| m.checked_mul(MIB))
        .ok_or_else(|| format!("size_mib must be >= 1 and addressable in bytes, got {size_mib}"))
}

/// The space that must stay free after the fill: 5% of capacity, floored at
/// [`HEADROOM_FLOOR_BYTES`]. Deliberately not operator-configurable.
fn required_headroom(capacity: u64) -> u64 {
    (capacity / HEADROOM_CAPACITY_DIVISOR).max(HEADROOM_FLOOR_BYTES)
}

/// True if `available` covers the fill plus the headroom that must survive it.
fn headroom_allows(size: u64, available: u64, capacity: u64) -> bool {
    size.checked_add(required_headroom(capacity))
        .is_some_and(|need| available >= need)
}

/// True if the allocation is real: the file has exactly the requested length
/// and its block usage covers it (a sparse file passes the length test but
/// consumes nothing).
fn allocation_backed(len: u64, blocks_512: u64, size: u64) -> bool {
    len == size && blocks_512.saturating_mul(512) >= size
}

// ===== event helpers =====

/// Build a single-`log`-event failure outcome with the given exit code.
fn fail(input: &PluginInput, now: SystemTime, exit: u8, msg: String) -> Outcome {
    Outcome {
        events: vec![log(input, now, Level::Error, msg)],
        exit,
    }
}

/// Build a `status` event for this instance at `now`.
fn status(input: &PluginInput, now: SystemTime, state: InstanceState) -> PluginEvent {
    PluginEvent::status(ts(now), input.instance_id.clone(), state)
}

/// Build a `log` event for this instance at `now`.
fn log(input: &PluginInput, now: SystemTime, level: Level, msg: impl Into<String>) -> PluginEvent {
    PluginEvent::log(ts(now), input.instance_id.clone(), level, msg)
}

/// Format `now` as an RFC3339 UTC timestamp with second precision (`…Z`).
fn ts(now: SystemTime) -> String {
    humantime::format_rfc3339_seconds(now).to_string()
}

// ===== pure-core tests =====

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn headroom_is_five_percent_with_a_floor() {
        // 10 TiB capacity: 5% = 512 GiB wins over the floor.
        let ten_tib = 10 * 1024 * 1024 * MIB;
        assert_eq!(required_headroom(ten_tib), ten_tib / 20);
        // 1 GiB capacity: 5% = ~51 MiB, the 64 MiB floor wins.
        assert_eq!(required_headroom(1024 * MIB), HEADROOM_FLOOR_BYTES);
        assert_eq!(required_headroom(0), HEADROOM_FLOOR_BYTES);
    }

    #[test]
    fn headroom_boundary_is_exact() {
        let capacity = 100 * 1024 * MIB; // 100 GiB → headroom = 5 GiB
        let headroom = required_headroom(capacity);
        let size = 10 * 1024 * MIB;
        assert!(headroom_allows(size, size + headroom, capacity));
        assert!(!headroom_allows(size, size + headroom - 1, capacity));
    }

    #[test]
    fn headroom_overflow_refuses() {
        assert!(!headroom_allows(u64::MAX, u64::MAX, u64::MAX));
    }

    #[test]
    fn size_bytes_bounds() {
        assert_eq!(size_bytes(1).unwrap(), MIB);
        assert!(size_bytes(0).is_err());
        assert!(size_bytes(-5).is_err());
        assert!(size_bytes(i64::MAX).is_err(), "byte size must not overflow");
    }

    #[test]
    fn allocation_backed_rejects_sparse_and_truncated() {
        let size = 8 * MIB;
        assert!(allocation_backed(size, size / 512, size));
        assert!(!allocation_backed(size, 0, size), "sparse file");
        assert!(!allocation_backed(size - 1, size / 512, size), "short file");
        // Filesystems may round block usage up, never down past the data.
        assert!(allocation_backed(size, size / 512 + 8, size));
    }

    #[test]
    fn fill_path_is_a_direct_child_with_the_id_embedded() {
        let dir = Path::new("/var/lib/faultforge");
        let path = fill_file_path(dir, &InstanceId::from("exp1:web01:0")).unwrap();
        assert_eq!(
            path,
            Path::new("/var/lib/faultforge/faultforge-exp1:web01:0.fill")
        );
    }

    #[test]
    fn traversal_hostile_id_is_refused() {
        let dir = Path::new("/var/lib/faultforge");
        // The agent's boundary validation refuses such ids; this is the
        // defense-in-depth layer behind it.
        assert!(fill_file_path(dir, &InstanceId::from("../../etc/cron.d/x")).is_err());
        assert!(fill_file_path(dir, &InstanceId::from("a/b")).is_err());
    }
}
