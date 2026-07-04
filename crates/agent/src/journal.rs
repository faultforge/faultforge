//! The persisted instance journal (ADR-0002 §6, design D5): one versioned JSON
//! file per live instance under `<data_dir>/instances/`, written atomically
//! before `inject` and removed once the terminal status is queued. A restarted
//! agent replays these entries to recover the host.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The only journal format this agent writes and understands. Replay rejects
/// any other version instead of misreading it (design risk note).
pub const JOURNAL_VERSION: u32 = 1;

/// Everything replay needs to recover an instance: identity, the exact plugin
/// bytes to trust (digest), the stdin input to rebuild, and the deadline that
/// decides `ABORTED` vs `ERROR`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    /// Format version; see [`JOURNAL_VERSION`].
    pub version: u32,
    /// The master-minted instance id (also the file stem).
    pub instance_id: String,
    /// Plugin name, for catalog resolution.
    pub plugin_name: String,
    /// Plugin version, for catalog resolution.
    pub plugin_version: String,
    /// The digest verified for this instance; replay re-verifies against it.
    pub plugin_digest: String,
    /// The already-validated params, replayed into recovery invocations.
    pub params: serde_json::Map<String, serde_json::Value>,
    /// When the instance started, unix milliseconds.
    pub started_unix_ms: i64,
    /// The experiment duration.
    pub duration_secs: u32,
    /// The master-decided grace.
    pub grace_secs: u32,
    /// Absolute dead-man deadline (`start + duration + grace`), unix seconds —
    /// the same value handed to the plugin.
    pub deadline_unix: i64,
}

/// Why a journal entry could not be read.
#[derive(Debug, thiserror::Error)]
pub enum JournalReadError {
    /// The file could not be read.
    #[error("could not read journal entry {path}: {source}")]
    Read {
        /// The entry path.
        path: PathBuf,
        /// The underlying IO error.
        source: std::io::Error,
    },
    /// The file is not valid JSON or not a valid entry shape.
    #[error("corrupt journal entry {path}: {source}")]
    Corrupt {
        /// The entry path.
        path: PathBuf,
        /// The parse error.
        source: serde_json::Error,
    },
    /// The entry was written by an agent this build does not understand.
    #[error("journal entry {path} has unknown version {version}")]
    UnknownVersion {
        /// The entry path.
        path: PathBuf,
        /// The version found.
        version: u32,
    },
}

/// The journal directory handle.
#[derive(Debug, Clone)]
pub struct Journal {
    dir: PathBuf,
}

impl Journal {
    /// A journal rooted at `<data_dir>/instances/`. The directory is created on
    /// first write, not here.
    #[must_use]
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("instances"),
        }
    }

    fn entry_path(&self, instance_id: &str) -> PathBuf {
        self.dir.join(format!("{instance_id}.json"))
    }

    /// Atomically persist `entry` (temp file + rename, then the write is
    /// visible in full or not at all).
    ///
    /// # Errors
    ///
    /// Returns the IO error if the directory cannot be created or the write,
    /// sync, or rename fails.
    pub fn write(&self, entry: &JournalEntry) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let mut tmp = tempfile::NamedTempFile::new_in(&self.dir)?;
        serde_json::to_writer(&mut tmp, entry)?;
        tmp.flush()?;
        // sync before rename: a crash must never leave a visible half-entry.
        tmp.as_file().sync_all()?;
        tmp.persist(self.entry_path(&entry.instance_id))
            .map_err(|e| e.error)?;
        Ok(())
    }

    /// Remove the entry for `instance_id`. Missing files are fine — removal is
    /// idempotent, mirroring the cleanup it accompanies.
    ///
    /// # Errors
    ///
    /// Returns the IO error for anything other than the file already being gone.
    pub fn remove(&self, instance_id: &str) -> std::io::Result<()> {
        match std::fs::remove_file(self.entry_path(instance_id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// List every persisted entry, each independently readable or broken —
    /// replay must handle a corrupt entry without losing the readable ones.
    ///
    /// Temp files from interrupted writes (no `.json` suffix) are ignored: an
    /// entry that never got renamed was never relied upon, because the write
    /// happens strictly before `inject`.
    ///
    /// # Errors
    ///
    /// Returns the IO error if the directory itself cannot be listed (a missing
    /// directory is an empty journal, not an error).
    pub fn list(&self) -> std::io::Result<Vec<Result<JournalEntry, JournalReadError>>> {
        let read_dir = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e),
        };
        let mut entries = vec![];
        for dirent in read_dir {
            let path = dirent?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            entries.push(read_entry(&path));
        }
        // Deterministic replay order for tests and debuggability.
        entries.sort_by_key(|r| match r {
            Ok(e) => e.instance_id.clone(),
            Err(e) => e.to_string(),
        });
        Ok(entries)
    }
}

fn read_entry(path: &Path) -> Result<JournalEntry, JournalReadError> {
    let bytes = std::fs::read(path).map_err(|source| JournalReadError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    // Check the version before trusting the shape: a future entry may have a
    // shape this build would misread.
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|source| JournalReadError::Corrupt {
            path: path.to_path_buf(),
            source,
        })?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|v| u32::try_from(v).ok());
    match version {
        Some(JOURNAL_VERSION) => {
            serde_json::from_value(value).map_err(|source| JournalReadError::Corrupt {
                path: path.to_path_buf(),
                source,
            })
        }
        Some(other) => Err(JournalReadError::UnknownVersion {
            path: path.to_path_buf(),
            version: other,
        }),
        None => Err(JournalReadError::UnknownVersion {
            path: path.to_path_buf(),
            version: 0,
        }),
    }
}

// ===== Unit tests =====

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str) -> JournalEntry {
        JournalEntry {
            version: JOURNAL_VERSION,
            instance_id: id.to_string(),
            plugin_name: "fixture".to_string(),
            plugin_version: "1".to_string(),
            plugin_digest: "ab".repeat(32),
            params: serde_json::Map::new(),
            started_unix_ms: 1_700_000_000_000,
            duration_secs: 30,
            grace_secs: 10,
            deadline_unix: 1_700_000_040,
        }
    }

    #[test]
    fn write_then_list_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::new(dir.path());
        journal.write(&entry("a-1")).unwrap();
        journal.write(&entry("b-2")).unwrap();
        let listed = journal.list().unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].as_ref().unwrap().instance_id, "a-1");
        assert_eq!(listed[1].as_ref().unwrap().instance_id, "b-2");
    }

    #[test]
    fn remove_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::new(dir.path());
        journal.write(&entry("a-1")).unwrap();
        journal.remove("a-1").unwrap();
        journal.remove("a-1").unwrap();
        assert!(journal.list().unwrap().is_empty());
    }

    #[test]
    fn missing_directory_is_an_empty_journal() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::new(&dir.path().join("never-created"));
        assert!(journal.list().unwrap().is_empty());
    }

    #[test]
    fn unknown_version_is_rejected_not_misread() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::new(dir.path());
        journal.write(&entry("a-1")).unwrap();
        let path = dir.path().join("instances/future.json");
        std::fs::write(&path, r#"{"version": 99, "shape": "unknowable"}"#).unwrap();
        let listed = journal.list().unwrap();
        assert_eq!(listed.len(), 2);
        assert!(
            listed
                .iter()
                .any(|r| matches!(r, Err(JournalReadError::UnknownVersion { version: 99, .. })))
        );
        assert!(listed.iter().any(Result::is_ok));
    }

    #[test]
    fn corrupt_entry_is_an_error_alongside_readable_ones() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::new(dir.path());
        journal.write(&entry("good")).unwrap();
        std::fs::write(dir.path().join("instances/bad.json"), "not json at all").unwrap();
        let listed = journal.list().unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().any(Result::is_err));
        assert!(listed.iter().any(Result::is_ok));
    }

    #[test]
    fn temp_files_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::new(dir.path());
        journal.write(&entry("good")).unwrap();
        std::fs::write(dir.path().join("instances/.tmpXYZ"), "partial").unwrap();
        assert_eq!(journal.list().unwrap().len(), 1);
    }
}
