//! Shared fault contract for `FaultForge`: the single source of truth for the
//! plugin manifest, parameter schema, agent↔plugin invocation protocol, instance
//! lifecycle states, integrity digest, and on-disk catalog layout.
//!
//! The crate is sync and free of tonic/tokio so plugin binaries stay lean;
//! `proto ⇄ domain` conversions land behind a `proto` feature flag on this crate
//! (change 2), so `noop-marker` never links the gRPC stack.
//!
//! # Module map
//!
//! - [`manifest`] — manifest types, strict YAML parsing, and validation.
//! - [`params`] — the typed parameter schema and its pure validator.
//! - [`protocol`] — commands, stdin input, NDJSON events, and the exit-code table.
//! - [`state`] — the instance lifecycle states.
//! - [`digest`] — the integrity digest newtype and its computation.
//! - [`catalog`] — the on-disk layout and the plugin-loading IO shell.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod catalog;
pub mod digest;
pub mod manifest;
pub mod params;
pub mod protocol;
pub mod state;

pub use catalog::{CatalogError, LoadedPlugin, MANIFEST_FILENAME, load_plugin, plugin_dir_name};
pub use digest::{Digest, DigestError};
pub use manifest::{Manifest, ManifestError, PluginName, PluginNameError, Requires};
pub use params::{ParamType, ParamsError, ParamsSchema, validate_params};
pub use protocol::{
    Disposition, EXIT_CLEANUP_FAILED, EXIT_INJECT_FAILED, EXIT_PREFLIGHT_FAILED, EXIT_SUCCESS,
    EventBody, InstanceId, Level, Phase, PluginCommand, PluginEvent, PluginInput, UnknownCommand,
    classify_exit,
};
pub use state::InstanceState;
