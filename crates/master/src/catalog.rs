//! The master's view of the plugin catalog (design D2): the same on-disk
//! layout the agent uses, loaded once at startup with the shared
//! `faultforge-fault` loader, so digests, params schemas, and duration caps
//! come from the exact bytes agents have baked in — no separate file format
//! that could drift.

use std::collections::HashMap;
use std::path::Path;

use tracing::{info, warn};

use faultforge_fault::catalog::load_plugin;
use faultforge_fault::digest::Digest;
use faultforge_fault::manifest::Manifest;

/// One loadable plugin: its validated manifest and computed digest.
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    /// The parsed, validated manifest.
    pub manifest: Manifest,
    /// The digest over the on-disk manifest and entrypoint bytes; carried in
    /// `RunFault` so the agent verifies the same bytes before executing.
    pub digest: Digest,
}

/// The catalog the master validates and dispatches against, keyed by
/// `(name, version)`. Immutable after load (hot reload is deferred).
#[derive(Debug, Default)]
pub struct Catalog {
    entries: HashMap<(String, String), CatalogEntry>,
}

impl Catalog {
    /// The entry for `(name, version)`, if present.
    #[must_use]
    pub fn get(&self, name: &str, version: &str) -> Option<&CatalogEntry> {
        self.entries.get(&(name.to_string(), version.to_string()))
    }

    /// Number of loaded plugins.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when no plugin loaded (every experiment will fail VALIDATE).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Load every plugin under `catalog_root`.
///
/// A broken entry (unreadable directory, invalid manifest, missing entrypoint,
/// or a directory name that does not match the manifest identity) is logged
/// and skipped — one bad plugin must not take down the control plane. A
/// missing or empty root yields an empty catalog, not an error.
#[must_use]
pub fn load_catalog(catalog_root: &Path) -> Catalog {
    let mut entries = HashMap::new();
    let read_dir = match std::fs::read_dir(catalog_root) {
        Ok(read_dir) => read_dir,
        Err(e) => {
            warn!(root = %catalog_root.display(), error = %e,
                "catalog root unreadable; starting with an empty catalog");
            return Catalog { entries };
        }
    };
    for dir_entry in read_dir {
        let path = match dir_entry {
            Ok(entry) => entry.path(),
            Err(e) => {
                warn!(root = %catalog_root.display(), error = %e,
                    "skipping unreadable catalog directory entry");
                continue;
            }
        };
        if !path.is_dir() {
            continue;
        }
        match load_plugin(&path) {
            Ok(loaded) => {
                // The agent resolves plugins by the `<name>@<version>` directory
                // name, so an entry whose directory disagrees with its manifest
                // identity would validate here and then fail on every agent.
                let identity = loaded.manifest.identity();
                if path.file_name().and_then(|n| n.to_str()) != Some(identity.as_str()) {
                    warn!(dir = %path.display(), identity = %identity,
                        "skipping catalog entry: directory name does not match manifest identity");
                    continue;
                }
                info!(plugin = %identity, digest = %loaded.digest.as_str(), "catalog plugin loaded");
                entries.insert(
                    (
                        loaded.manifest.name.as_str().to_string(),
                        loaded.manifest.version.clone(),
                    ),
                    CatalogEntry {
                        manifest: loaded.manifest,
                        digest: loaded.digest,
                    },
                );
            }
            Err(e) => {
                warn!(dir = %path.display(), error = %e, "skipping broken catalog entry");
            }
        }
    }
    Catalog { entries }
}

// ===== Unit tests =====

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str =
        "name: fixture\nversion: \"1\"\nentrypoint: ./run.sh\nmax_duration_secs: 60\n";

    fn install(root: &Path, dir_name: &str) -> Digest {
        let dir = root.join(dir_name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.yaml"), MANIFEST).unwrap();
        std::fs::write(dir.join("run.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        Digest::compute(MANIFEST.as_bytes(), b"#!/bin/sh\nexit 0\n")
    }

    #[test]
    fn loads_a_valid_entry_with_manifest_and_digest() {
        let root = tempfile::tempdir().unwrap();
        let digest = install(root.path(), "fixture@1");
        let catalog = load_catalog(root.path());
        assert_eq!(catalog.len(), 1);
        let entry = catalog.get("fixture", "1").unwrap();
        assert_eq!(entry.digest, digest);
        assert_eq!(entry.manifest.max_duration_secs, 60);
    }

    #[test]
    fn broken_entry_is_skipped_and_the_rest_load() {
        let root = tempfile::tempdir().unwrap();
        install(root.path(), "fixture@1");
        let broken = root.path().join("broken@1");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("manifest.yaml"), "not: [valid").unwrap();
        let catalog = load_catalog(root.path());
        assert_eq!(catalog.len(), 1);
        assert!(catalog.get("broken", "1").is_none());
    }

    #[test]
    fn dir_name_mismatching_identity_is_skipped() {
        let root = tempfile::tempdir().unwrap();
        install(root.path(), "other-name@2");
        assert!(load_catalog(root.path()).is_empty());
    }

    #[test]
    fn missing_root_yields_an_empty_catalog() {
        let root = tempfile::tempdir().unwrap();
        let catalog = load_catalog(&root.path().join("does-not-exist"));
        assert!(catalog.is_empty());
    }

    #[test]
    fn plain_files_in_the_root_are_ignored() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("README"), "hi").unwrap();
        assert!(load_catalog(root.path()).is_empty());
    }

    #[test]
    fn unknown_version_is_not_found() {
        let root = tempfile::tempdir().unwrap();
        install(root.path(), "fixture@1");
        let catalog = load_catalog(root.path());
        assert!(catalog.get("fixture", "2").is_none());
    }
}
