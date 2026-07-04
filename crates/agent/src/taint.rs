//! Host quarantine (ADR-0002 §9, design D9): a `tainted.json` record under
//! `data_dir` whose presence means the host failed recovery and must not run
//! new faults. The agent writes it and reports it; it is cleared only on an
//! operator-issued `ClearTaint` relayed by the master.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Why and when the host was quarantined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaintRecord {
    /// What failed.
    pub reason: String,
    /// When, unix milliseconds.
    pub ts_unix_ms: i64,
    /// The instance whose recovery failed.
    pub instance_id: String,
}

/// The taint file handle.
#[derive(Debug, Clone)]
pub struct Taint {
    path: PathBuf,
}

impl Taint {
    /// The taint record lives at `<data_dir>/tainted.json`.
    #[must_use]
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join("tainted.json"),
        }
    }

    /// The current taint, if any. A record that exists but cannot be read still
    /// counts as tainted — an unreadable quarantine marker must fail closed,
    /// never read as "clean".
    #[must_use]
    pub fn current(&self) -> Option<TaintRecord> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                return Some(TaintRecord {
                    reason: format!("taint record unreadable: {e}"),
                    ts_unix_ms: 0,
                    instance_id: String::new(),
                });
            }
        };
        match serde_json::from_slice(&bytes) {
            Ok(record) => Some(record),
            Err(e) => Some(TaintRecord {
                reason: format!("taint record corrupt: {e}"),
                ts_unix_ms: 0,
                instance_id: String::new(),
            }),
        }
    }

    /// Persist `record` atomically. An existing record is kept — the first
    /// taint is the root cause; later failures must not overwrite it.
    ///
    /// # Errors
    ///
    /// Returns the IO error if the directory cannot be created or the write,
    /// sync, or rename fails.
    pub fn mark(&self, record: &TaintRecord) -> std::io::Result<()> {
        if self.current().is_some() {
            return Ok(());
        }
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
            let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
            serde_json::to_writer(&mut tmp, record)?;
            tmp.flush()?;
            tmp.as_file().sync_all()?;
            tmp.persist(&self.path).map_err(|e| e.error)?;
        }
        Ok(())
    }

    /// Remove the taint record. Idempotent: an absent record is already clear.
    ///
    /// Only ever called for an operator-issued `ClearTaint` — the agent never
    /// clears the quarantine on its own initiative (agent-fault-runtime spec).
    ///
    /// # Errors
    ///
    /// Returns the IO error if the record exists but cannot be removed; the
    /// host stays tainted in that case.
    pub fn clear(&self) -> std::io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

// ===== Unit tests =====

#[cfg(test)]
mod tests {
    use super::*;

    fn record(reason: &str) -> TaintRecord {
        TaintRecord {
            reason: reason.to_string(),
            ts_unix_ms: 1_700_000_000_000,
            instance_id: "i-1".to_string(),
        }
    }

    #[test]
    fn absent_file_means_clean() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Taint::new(dir.path()).current().is_none());
    }

    #[test]
    fn mark_then_current_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let taint = Taint::new(dir.path());
        taint.mark(&record("cleanup failed twice")).unwrap();
        let current = taint.current().unwrap();
        assert_eq!(current.reason, "cleanup failed twice");
        assert_eq!(current.instance_id, "i-1");
    }

    #[test]
    fn first_taint_wins() {
        let dir = tempfile::tempdir().unwrap();
        let taint = Taint::new(dir.path());
        taint.mark(&record("first failure")).unwrap();
        taint.mark(&record("second failure")).unwrap();
        assert_eq!(taint.current().unwrap().reason, "first failure");
    }

    #[test]
    fn corrupt_record_still_counts_as_tainted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tainted.json"), "garbage").unwrap();
        let current = Taint::new(dir.path()).current().unwrap();
        assert!(current.reason.contains("corrupt"));
    }

    #[test]
    fn clear_removes_the_record() {
        let dir = tempfile::tempdir().unwrap();
        let taint = Taint::new(dir.path());
        taint.mark(&record("r")).unwrap();
        taint.clear().unwrap();
        assert!(taint.current().is_none());
    }

    #[test]
    fn clear_on_a_clean_host_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Taint::new(dir.path()).clear().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn failed_clear_keeps_the_quarantine() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let taint = Taint::new(dir.path());
        taint.mark(&record("r")).unwrap();
        // A read-only parent directory makes the unlink fail.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = taint.clear();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.is_err());
        assert!(taint.current().is_some());
    }

    #[test]
    fn taint_survives_a_new_handle() {
        // The "restart" case: a fresh Taint over the same data_dir still sees it.
        let dir = tempfile::tempdir().unwrap();
        Taint::new(dir.path()).mark(&record("r")).unwrap();
        assert!(Taint::new(dir.path()).current().is_some());
    }
}
