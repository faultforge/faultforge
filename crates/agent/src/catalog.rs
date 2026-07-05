//! Catalog resolution for the agent: locate a plugin under `plugin_root`,
//! load and validate it, and verify its digest against the referenced value.
//!
//! Called before **every** plugin invocation (ADR-0002 §3, design D7): a digest
//! verified once at `RunFault` and trusted afterwards would let an on-disk swap
//! execute with a stale verdict.

use std::path::{Path, PathBuf};

use faultforge_fault::catalog::{CatalogError, load_plugin, plugin_dir_name};
use faultforge_fault::digest::Digest;
use faultforge_fault::manifest::{Manifest, PluginName, PluginNameError, is_valid_plugin_version};

/// Why a plugin could not be resolved and verified from the catalog.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// The wire `plugin_name` is not a valid catalog name.
    #[error("invalid plugin name: {0}")]
    Name(#[from] PluginNameError),
    /// The wire `plugin_version` is not path-safe (charset `[A-Za-z0-9._+-]`).
    #[error("invalid plugin version: {0:?}")]
    Version(String),
    /// No catalog entry directory exists for `<name>@<version>`.
    #[error("plugin not in catalog: {0}")]
    NotFound(String),
    /// The entry exists but does not load (manifest/entrypoint problems).
    #[error("plugin failed to load: {0}")]
    Load(#[from] CatalogError),
    /// The computed on-disk digest differs from the referenced one.
    #[error("digest mismatch: expected {expected}, computed {computed}")]
    DigestMismatch {
        /// The digest referenced by the master (or the journal).
        expected: Digest,
        /// The digest of the bytes actually on disk.
        computed: Digest,
    },
}

/// A digest-verified catalog plugin, ready to invoke.
#[derive(Debug, Clone)]
pub struct VerifiedPlugin {
    /// Absolute path to the entrypoint executable.
    pub entrypoint: PathBuf,
    /// The validated manifest.
    pub manifest: Manifest,
    /// The verified digest (equals the referenced value).
    pub digest: Digest,
}

/// Resolve `<name>@<version>` under `plugin_root`, load it, and verify that its
/// digest equals `expected`.
///
/// # Errors
///
/// Returns a [`ResolveError`] if the name is invalid, the entry directory is
/// missing, the plugin fails to load/validate, or the digest does not match.
pub fn resolve_verified(
    plugin_root: &Path,
    name: &str,
    version: &str,
    expected: &Digest,
) -> Result<VerifiedPlugin, ResolveError> {
    let name = PluginName::parse(name)?;
    // SEC-2: the on-disk manifest charset is enforced at load time, but the wire
    // `plugin_version` reaches `plugin_dir_name` unvalidated — a rogue master
    // could send `1/../../../../tmp/evil` to escape `plugin_root`. Reject it with
    // the manifest's own predicate before any filesystem join.
    if !is_valid_plugin_version(version) {
        return Err(ResolveError::Version(version.to_owned()));
    }
    let dir = plugin_root.join(plugin_dir_name(&name, version));
    if !dir.is_dir() {
        return Err(ResolveError::NotFound(dir.display().to_string()));
    }
    let loaded = load_plugin(&dir)?;
    if loaded.digest != *expected {
        return Err(ResolveError::DigestMismatch {
            expected: expected.clone(),
            computed: loaded.digest,
        });
    }
    let entrypoint = dir.join(&loaded.manifest.entrypoint);
    Ok(VerifiedPlugin {
        entrypoint,
        manifest: loaded.manifest,
        digest: loaded.digest,
    })
}

// ===== Unit tests =====

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    const MANIFEST: &str =
        "name: fixture\nversion: \"1\"\nentrypoint: ./run.sh\nmax_duration_secs: 60\n";
    const SCRIPT: &str = "#!/bin/sh\nexit 0\n";

    fn install(root: &Path) -> Digest {
        let dir = root.join("fixture@1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.yaml"), MANIFEST).unwrap();
        let script = dir.join("run.sh");
        std::fs::write(&script, SCRIPT).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        Digest::compute(MANIFEST.as_bytes(), SCRIPT.as_bytes())
    }

    #[test]
    fn resolves_and_verifies_a_valid_entry() {
        let root = tempfile::tempdir().unwrap();
        let digest = install(root.path());
        let plugin = resolve_verified(root.path(), "fixture", "1", &digest).unwrap();
        assert_eq!(plugin.digest, digest);
        assert!(plugin.entrypoint.ends_with("fixture@1/./run.sh"));
        assert_eq!(plugin.manifest.identity(), "fixture@1");
    }

    #[test]
    fn missing_entry_is_not_found() {
        let root = tempfile::tempdir().unwrap();
        let digest = Digest::compute(b"x", b"y");
        assert!(matches!(
            resolve_verified(root.path(), "absent", "1", &digest),
            Err(ResolveError::NotFound(_))
        ));
    }

    #[test]
    fn wrong_digest_is_a_mismatch() {
        let root = tempfile::tempdir().unwrap();
        let _ = install(root.path());
        let wrong = Digest::compute(b"something", b"else");
        assert!(matches!(
            resolve_verified(root.path(), "fixture", "1", &wrong),
            Err(ResolveError::DigestMismatch { .. })
        ));
    }

    #[test]
    fn on_disk_edit_after_reference_is_caught() {
        let root = tempfile::tempdir().unwrap();
        let digest = install(root.path());
        // Swap the entrypoint bytes after the digest was referenced.
        std::fs::write(root.path().join("fixture@1/run.sh"), "#!/bin/sh\nexit 1\n").unwrap();
        assert!(matches!(
            resolve_verified(root.path(), "fixture", "1", &digest),
            Err(ResolveError::DigestMismatch { .. })
        ));
    }

    #[test]
    fn traversal_version_is_rejected_before_filesystem_join() {
        let root = tempfile::tempdir().unwrap();
        // A valid entry is installed so the version gate must be what rejects the
        // request — not a missing directory (SEC-2: a rogue master's
        // `plugin_version` climbing out of the catalog must never reach the join).
        let digest = install(root.path());
        assert!(matches!(
            resolve_verified(root.path(), "fixture", "1/../../../../tmp/evil", &digest),
            Err(ResolveError::Version(_))
        ));
    }

    #[test]
    fn invalid_name_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let digest = Digest::compute(b"x", b"y");
        assert!(matches!(
            resolve_verified(root.path(), "Bad/Name", "1", &digest),
            Err(ResolveError::Name(_))
        ));
    }
}
