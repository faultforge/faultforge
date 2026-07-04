//! The plugin manifest: a strict, contract-shaped declaration parsed from YAML.

use std::path::{Component, Path};

use serde::Deserialize;

use crate::params::ParamsSchema;

// ===== Plugin name newtype =====

/// Why a string is not a valid [`PluginName`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PluginNameError {
    /// The name was empty.
    #[error("plugin name must not be empty")]
    Empty,
    /// The name contained a character outside `[a-z0-9-]`.
    #[error("plugin name must contain only lowercase letters, digits, and '-': {0}")]
    InvalidChar(String),
}

/// A validated plugin name: non-empty, charset `[a-z0-9-]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(try_from = "String")]
pub struct PluginName(String);

impl PluginName {
    /// Parse and validate a plugin name.
    ///
    /// # Errors
    ///
    /// Returns [`PluginNameError::Empty`] for an empty string or
    /// [`PluginNameError::InvalidChar`] for any character outside `[a-z0-9-]`.
    pub fn parse(s: &str) -> Result<Self, PluginNameError> {
        if s.is_empty() {
            return Err(PluginNameError::Empty);
        }
        if !s
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(PluginNameError::InvalidChar(s.to_string()));
        }
        Ok(Self(s.to_string()))
    }

    /// The name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for PluginName {
    type Error = PluginNameError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(&s)
    }
}

impl std::fmt::Display for PluginName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ===== Requirements block =====

/// What a plugin needs from a host. All fields default to empty so a plugin may
/// omit any it does not use.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requires {
    /// Host binaries that must be present.
    #[serde(default)]
    pub binaries: Vec<String>,
    /// Privileges the plugin needs (v1 recognizes an empty list and `root`).
    #[serde(default)]
    pub privileges: Vec<String>,
    /// Per-fault preconditions the agent checks at runtime preflight.
    #[serde(default)]
    pub preconditions: Vec<String>,
}

// ===== Manifest =====

/// Why a manifest is invalid.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// The YAML did not parse, or contained an unknown field, or a malformed name.
    #[error("failed to parse manifest: {0}")]
    Parse(#[from] serde_norway::Error),
    /// The entrypoint was an empty string.
    #[error("entrypoint must not be empty")]
    EntrypointEmpty,
    /// The entrypoint was an absolute path.
    #[error("entrypoint must be a relative path: {0}")]
    EntrypointNotRelative(String),
    /// The entrypoint contained a `..` component (would escape the plugin dir).
    #[error("entrypoint must not contain '..': {0}")]
    EntrypointEscapes(String),
    /// The version was an empty string.
    #[error("version must not be empty")]
    VersionEmpty,
    /// The version contained a character outside `[A-Za-z0-9._+-]` (e.g. a path
    /// separator, which would let `<name>@<version>` escape the catalog root).
    #[error("version must contain only letters, digits, and '.', '_', '+', '-': {0}")]
    VersionInvalidChar(String),
    /// `max_duration_secs` was zero.
    #[error("max_duration_secs must be greater than zero")]
    ZeroDuration,
}

/// The raw, shape-only manifest as deserialized from YAML, before semantic
/// validation. Private so the only way to obtain a public [`Manifest`] from YAML
/// is [`Manifest::parse`], which validates — deserializing cannot bypass the
/// invariant checks.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    name: PluginName,
    version: String,
    entrypoint: String,
    #[serde(default)]
    params_schema: ParamsSchema,
    #[serde(default)]
    requires: Requires,
    max_duration_secs: u32,
}

/// A parsed, validated plugin manifest.
///
/// Constructed only via [`Manifest::parse`]: deserialization is strict
/// (`deny_unknown_fields` on the private `RawManifest`) and the semantic
/// invariants below are always checked, so a `Manifest` value is trustworthy
/// without re-validation. The digest covers the manifest's exact bytes, so an
/// unknown key is drift, not an extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Plugin name (identity, with `version`).
    pub name: PluginName,
    /// Plugin version (identity, with `name`). A string so semver can arrive later
    /// and to match the wire `plugin_version`; validated to a path-safe charset.
    pub version: String,
    /// Path to the entrypoint executable, relative to the plugin directory.
    pub entrypoint: String,
    /// Parameter schema validated before dispatch. Defaults to empty.
    pub params_schema: ParamsSchema,
    /// What the plugin needs from a host. Defaults to nothing required.
    pub requires: Requires,
    /// The longest the plugin can safely self-revert within; must be `> 0`.
    pub max_duration_secs: u32,
}

impl Manifest {
    /// Parse and validate a manifest from YAML.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::Parse`] on malformed YAML, unknown fields, or an
    /// invalid name; or a validation error if the entrypoint is empty, absolute,
    /// or escapes the plugin directory, the version is empty or not path-safe, or
    /// `max_duration_secs` is zero.
    pub fn parse(yaml: &str) -> Result<Self, ManifestError> {
        let raw: RawManifest = serde_norway::from_str(yaml)?;
        let manifest = Self {
            name: raw.name,
            version: raw.version,
            entrypoint: raw.entrypoint,
            params_schema: raw.params_schema,
            requires: raw.requires,
            max_duration_secs: raw.max_duration_secs,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    /// Validate the semantic constraints not expressible in the type shape.
    ///
    /// # Errors
    ///
    /// See [`Manifest::parse`].
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.entrypoint.is_empty() {
            return Err(ManifestError::EntrypointEmpty);
        }
        let path = Path::new(&self.entrypoint);
        if path.is_absolute() {
            return Err(ManifestError::EntrypointNotRelative(
                self.entrypoint.clone(),
            ));
        }
        if path.components().any(|c| c == Component::ParentDir) {
            return Err(ManifestError::EntrypointEscapes(self.entrypoint.clone()));
        }
        // `version` is the other half of the plugin identity and feeds the catalog
        // directory name `<name>@<version>` (`plugin_dir_name`). Reject an empty
        // version (ambiguous identity) and any path separator or other unsafe
        // character so the identity can never escape the catalog root.
        if self.version.is_empty() {
            return Err(ManifestError::VersionEmpty);
        }
        if !self
            .version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
        {
            return Err(ManifestError::VersionInvalidChar(self.version.clone()));
        }
        if self.max_duration_secs == 0 {
            return Err(ManifestError::ZeroDuration);
        }
        Ok(())
    }

    /// The plugin identity string `<name>@<version>`.
    #[must_use]
    pub fn identity(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::ParamType;

    /// A representative manifest that exercises every field, used to test the
    /// parser here. It need not match the shipped `noop-marker/manifest.yaml`
    /// byte-for-byte — that file's conformance to this contract is tested
    /// separately by the `noop-marker` crate's own `tests/manifest.rs`.
    const NOOP_MARKER_MANIFEST: &str = r#"name: noop-marker
version: "1"
entrypoint: ./noop-marker
params_schema:
  marker_path: string
requires:
  binaries: []
  privileges: []
  preconditions:
    - marker_dir_writable
max_duration_secs: 600
"#;

    #[test]
    fn parses_the_noop_marker_manifest() {
        let m = Manifest::parse(NOOP_MARKER_MANIFEST).unwrap();
        assert_eq!(m.name.as_str(), "noop-marker");
        assert_eq!(m.version, "1");
        assert_eq!(m.entrypoint, "./noop-marker");
        assert_eq!(m.identity(), "noop-marker@1");
        assert_eq!(m.params_schema.get("marker_path"), Some(&ParamType::String));
        assert_eq!(m.params_schema.len(), 1);
        assert!(m.requires.binaries.is_empty());
        assert!(m.requires.privileges.is_empty());
        assert_eq!(m.requires.preconditions, vec!["marker_dir_writable"]);
        assert_eq!(m.max_duration_secs, 600);
    }

    #[test]
    fn unknown_field_is_rejected() {
        let yaml = format!("{NOOP_MARKER_MANIFEST}surprise: true\n");
        assert!(matches!(
            Manifest::parse(&yaml),
            Err(ManifestError::Parse(_))
        ));
    }

    #[test]
    fn absolute_entrypoint_is_rejected() {
        let yaml = "name: x\nversion: \"1\"\nentrypoint: /usr/bin/x\nmax_duration_secs: 10\n";
        assert!(matches!(
            Manifest::parse(yaml),
            Err(ManifestError::EntrypointNotRelative(_))
        ));
    }

    #[test]
    fn parent_dir_entrypoint_is_rejected() {
        let yaml = "name: x\nversion: \"1\"\nentrypoint: ../x\nmax_duration_secs: 10\n";
        assert!(matches!(
            Manifest::parse(yaml),
            Err(ManifestError::EntrypointEscapes(_))
        ));
    }

    #[test]
    fn zero_duration_is_rejected() {
        let yaml = "name: x\nversion: \"1\"\nentrypoint: ./x\nmax_duration_secs: 0\n";
        assert!(matches!(
            Manifest::parse(yaml),
            Err(ManifestError::ZeroDuration)
        ));
    }

    #[test]
    fn empty_version_is_rejected() {
        let yaml = "name: x\nversion: \"\"\nentrypoint: ./x\nmax_duration_secs: 10\n";
        assert!(matches!(
            Manifest::parse(yaml),
            Err(ManifestError::VersionEmpty)
        ));
    }

    #[test]
    fn version_with_path_separator_is_rejected() {
        // Would otherwise let `<name>@<version>` escape the catalog root.
        let yaml = "name: x\nversion: \"1/../../evil\"\nentrypoint: ./x\nmax_duration_secs: 10\n";
        assert!(matches!(
            Manifest::parse(yaml),
            Err(ManifestError::VersionInvalidChar(_))
        ));
    }

    #[test]
    fn semver_shaped_version_is_accepted() {
        let yaml =
            "name: x\nversion: \"1.2.3-rc.1+build\"\nentrypoint: ./x\nmax_duration_secs: 10\n";
        assert_eq!(Manifest::parse(yaml).unwrap().version, "1.2.3-rc.1+build");
    }

    #[test]
    fn uppercase_name_is_rejected() {
        let yaml = "name: BadName\nversion: \"1\"\nentrypoint: ./x\nmax_duration_secs: 10\n";
        // The bad name surfaces as a parse (deserialize) error via the newtype.
        assert!(matches!(
            Manifest::parse(yaml),
            Err(ManifestError::Parse(_))
        ));
    }

    #[test]
    fn minimal_manifest_without_optional_blocks_parses() {
        let yaml = "name: x\nversion: \"1\"\nentrypoint: ./x\nmax_duration_secs: 10\n";
        let m = Manifest::parse(yaml).unwrap();
        assert!(m.params_schema.is_empty());
        assert_eq!(m.requires, Requires::default());
    }
}
