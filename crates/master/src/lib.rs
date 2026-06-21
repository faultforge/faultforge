#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod clock;
pub mod config;
pub mod registry;
mod server;

pub use config::{Cli, MasterConfig, load_config};
pub use registry::{AgentInfo, Registry, new_registry};
pub use server::{MasterService, ServerError, run_server};
