//! The `FaultForge` agent: registers with the master over one persistent
//! `Session` stream and executes fault instances against the on-disk plugin
//! catalog with a safe, recoverable lifecycle (ADR-0002).
//!
//! # Module map
//!
//! - [`config`] — layered configuration (file → env → flags).
//! - [`machine`] — pure core: the per-instance state machine and safety timer.
//! - [`runner`] — IO shell: one plugin process invocation.
//! - [`catalog`] — IO shell: catalog resolution + digest verification.
//! - [`journal`] — IO shell: the persisted instance journal.
//! - [`taint`] — IO shell: the host quarantine record.
//! - [`supervisor`] — IO shell: instance tasks, effect execution, replay.
//! - [`session`] — IO shell: the reconnecting full-duplex master session.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod catalog;
pub mod config;
pub mod journal;
pub mod machine;
pub mod runner;
pub mod session;
pub mod supervisor;
pub mod taint;

pub use config::{AgentConfig, Cli, load_config};
pub use session::{AgentError, run_agent};
