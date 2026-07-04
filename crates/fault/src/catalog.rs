//! The on-disk catalog layout and the IO shell that loads a plugin from it.

use std::path::{Path, PathBuf};

use crate::digest::Digest;
use crate::manifest::{Manifest, ManifestError, PluginName};

/// The manifest filename inside every catalog entry directory.
pub const MANIFEST_FILENAME: &str = "manifest.yaml";

/// Why loading a plugin from a catalog directory failed.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    /// The `manifest.yaml` could not be read (e.g. the entry directory is missing).
    #[error("could not read manifest at {path}: {source}")]
    ReadManifest {
        /// The manifest path that could not be read.
        path: PathBuf,
        /// The underlying IO error.
        source: std::io::Error,
    },
    /// The manifest bytes were not valid UTF-8.
    #[error("manifest at {path} is not valid UTF-8")]
    ManifestNotUtf8 {
        /// The offending manifest path.
        path: PathBuf,
    },
    /// The manifest was read but did not parse or validate.
    #[error("invalid manifest at {path}: {source}")]
    Manifest {
        /// The manifest path.
        path: PathBuf,
        /// The parse/validation error.
        source: ManifestError,
    },
    /// The entrypoint executable could not be read.
    #[error("could not read entrypoint at {path}: {source}")]
    ReadEntrypoint {
        /// The entrypoint path that could not be read.
        path: PathBuf,
        /// The underlying IO error.
        source: std::io::Error,
    },
}

/// A plugin loaded from the catalog: its validated manifest and computed digest.
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    /// The parsed, validated manifest.
    pub manifest: Manifest,
    /// The digest computed from the manifest and entrypoint bytes, for
    /// verification against the externally-referenced value.
    pub digest: Digest,
}

/// The catalog directory name for a plugin: `<name>@<version>`.
///
/// Version-qualified so multiple versions install side by side.
#[must_use]
pub fn plugin_dir_name(name: &PluginName, version: &str) -> String {
    format!("{name}@{version}")
}

/// Load and validate the plugin in `dir`, computing its digest.
///
/// `dir` is a catalog entry directory containing [`MANIFEST_FILENAME`] and the
/// entrypoint at the manifest-declared relative path. The digest is computed over
/// the exact manifest bytes followed by the exact entrypoint bytes.
///
/// # Errors
///
/// Returns a [`CatalogError`] if the manifest cannot be read, is not UTF-8, fails
/// to parse/validate, or the entrypoint cannot be read.
pub fn load_plugin(dir: &Path) -> Result<LoadedPlugin, CatalogError> {
    let manifest_path = dir.join(MANIFEST_FILENAME);
    let manifest_bytes =
        std::fs::read(&manifest_path).map_err(|source| CatalogError::ReadManifest {
            path: manifest_path.clone(),
            source,
        })?;
    let manifest_str =
        std::str::from_utf8(&manifest_bytes).map_err(|_| CatalogError::ManifestNotUtf8 {
            path: manifest_path.clone(),
        })?;
    let manifest = Manifest::parse(manifest_str).map_err(|source| CatalogError::Manifest {
        path: manifest_path.clone(),
        source,
    })?;

    let entrypoint_path = dir.join(&manifest.entrypoint);
    let mut entrypoint =
        std::fs::File::open(&entrypoint_path).map_err(|source| CatalogError::ReadEntrypoint {
            path: entrypoint_path.clone(),
            source,
        })?;

    // Stream the executable into the hasher rather than buffering it — plugin
    // binaries can be large and this digest is recomputed before every invocation.
    let digest = Digest::compute_streaming(&manifest_bytes, &mut entrypoint).map_err(|source| {
        CatalogError::ReadEntrypoint {
            path: entrypoint_path,
            source,
        }
    })?;
    Ok(LoadedPlugin { manifest, digest })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str =
        "name: noop-marker\nversion: \"1\"\nentrypoint: ./noop-marker\nmax_duration_secs: 600\n";

    /// Write a valid catalog entry into a fresh tempdir; return the dir and the
    /// digest an operator would compute over its two files.
    fn valid_entry() -> (tempfile::TempDir, Digest) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MANIFEST_FILENAME), MANIFEST).unwrap();
        std::fs::write(dir.path().join("noop-marker"), b"#!/bin/sh\ntrue\n").unwrap();
        let expected = Digest::compute(MANIFEST.as_bytes(), b"#!/bin/sh\ntrue\n");
        (dir, expected)
    }

    #[test]
    fn plugin_dir_name_is_name_at_version() {
        let name = PluginName::parse("noop-marker").unwrap();
        assert_eq!(plugin_dir_name(&name, "1"), "noop-marker@1");
    }

    #[test]
    fn loads_manifest_and_computes_digest() {
        let (dir, expected) = valid_entry();
        let loaded = load_plugin(dir.path()).unwrap();
        assert_eq!(loaded.manifest.identity(), "noop-marker@1");
        assert_eq!(loaded.digest, expected);
    }

    #[test]
    fn missing_manifest_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            load_plugin(dir.path()),
            Err(CatalogError::ReadManifest { .. })
        ));
    }

    #[test]
    fn missing_entrypoint_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MANIFEST_FILENAME), MANIFEST).unwrap();
        // No entrypoint file written.
        assert!(matches!(
            load_plugin(dir.path()),
            Err(CatalogError::ReadEntrypoint { .. })
        ));
    }

    #[test]
    fn invalid_manifest_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(MANIFEST_FILENAME),
            "not: [a, valid, manifest",
        )
        .unwrap();
        assert!(matches!(
            load_plugin(dir.path()),
            Err(CatalogError::Manifest { .. } | CatalogError::ManifestNotUtf8 { .. })
        ));
    }
}
