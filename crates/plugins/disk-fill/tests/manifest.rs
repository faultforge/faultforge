//! The shipped `manifest.yaml` loads via `faultforge-fault` and matches the
//! declared contract (identity, entrypoint, params schema, precondition,
//! duration limit).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::Command;

use faultforge_fault::{Manifest, ParamType};

#[test]
fn shipped_manifest_conforms_to_contract() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("manifest.yaml");
    let yaml =
        std::fs::read_to_string(&path).expect("manifest.yaml must be present alongside the crate");
    let manifest = Manifest::parse(&yaml).expect("shipped manifest must parse and validate");

    assert_eq!(manifest.name.as_str(), "disk-fill");
    assert_eq!(manifest.version, "1");
    assert_eq!(manifest.identity(), "disk-fill@1");
    assert_eq!(manifest.entrypoint, "./disk-fill");

    assert_eq!(manifest.params_schema.len(), 2);
    assert_eq!(
        manifest.params_schema.get("fill_dir"),
        Some(&ParamType::String)
    );
    assert_eq!(
        manifest.params_schema.get("size_mib"),
        Some(&ParamType::Int)
    );

    assert!(manifest.requires.binaries.is_empty());
    assert!(manifest.requires.privileges.is_empty());
    assert_eq!(manifest.requires.preconditions, vec!["fill_dir_writable"]);

    assert_eq!(manifest.max_duration_secs, 3600);
}

/// Compute `cat <manifest> <binary> | sha256` with coreutils, trying `sha256sum`
/// (Linux) then `shasum -a 256` (macOS). Returns `None` if neither tool exists.
#[cfg(unix)]
fn coreutils_sha256(manifest: &Path, binary: &Path) -> Option<String> {
    for tool in ["sha256sum", "shasum -a 256"] {
        let cmd = format!(
            "cat '{}' '{}' | {tool}",
            manifest.display(),
            binary.display()
        );
        let output = Command::new("sh").arg("-c").arg(&cmd).output().ok()?;
        if output.status.success() {
            let stdout = String::from_utf8(output.stdout).ok()?;
            if let Some(hex) = stdout.split_whitespace().next()
                && hex.len() == 64
            {
                return Some(hex.to_string());
            }
        }
    }
    None
}

/// The digest `faultforge-fault` computes over the *real* shipped manifest and
/// built binary equals what an operator gets from `cat manifest.yaml disk-fill
/// | sha256sum` (spec: "Digest is reproducible with coreutils").
#[test]
#[cfg(unix)]
fn shipped_plugin_digest_reproduces_with_coreutils() {
    let manifest_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("manifest.yaml");
    let binary_src = Path::new(env!("CARGO_BIN_EXE_disk-fill"));

    // Assemble a catalog entry (manifest + entrypoint co-located) in a tempdir.
    let dir = tempfile::tempdir().unwrap();
    let manifest_dst = dir.path().join("manifest.yaml");
    let binary_dst = dir.path().join("disk-fill");
    std::fs::copy(&manifest_src, &manifest_dst).unwrap();
    std::fs::copy(binary_src, &binary_dst).unwrap();

    let loaded = faultforge_fault::load_plugin(dir.path()).expect("shipped plugin loads");

    let Some(coreutils_digest) = coreutils_sha256(&manifest_dst, &binary_dst) else {
        eprintln!("skipping: neither sha256sum nor shasum is available");
        return;
    };
    assert_eq!(
        loaded.digest.as_str(),
        coreutils_digest,
        "load_plugin digest must reproduce `cat manifest.yaml disk-fill | sha256sum`"
    );
}
