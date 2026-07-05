//! `FaultForge` end-to-end harness (step 4).
//!
//! A dev-only crate that builds container images and drives the real
//! `faultforge-master`, `faultforge-agent`, and `faultforge` CLI binaries as
//! separate rootless-podman containers, asserting the fault lifecycle through
//! the operator surface (CLI + management API) and host ground truth
//! (`podman exec`). The scenarios themselves live in `tests/scenarios.rs` and
//! are `#[ignore]` by default; run them with
//! `cargo test -p faultforge-e2e -- --ignored`.
//!
//! See `openspec/changes/fault-e2e-harness/` for the design and spec.

// This crate is never published and exists only to run scenarios; a broken
// podman substrate should abort loudly. Panics/unwraps/expects are the intended
// failure mode here, so the workspace no-unwrap lints and the panic-doc/must-use
// pedantic lints (nearly every helper panics on infrastructure failure by
// design) do not apply.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_panics_doc,
    clippy::must_use_candidate
)]

pub mod harness;
pub mod podman;
pub mod topology;
