#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod catalog;
pub mod clock;
pub mod config;
pub mod dispatch;
pub mod experiment;
mod lock;
pub mod management;
pub mod registry;
mod server;
mod session;
pub mod sessions;

pub use catalog::{Catalog, CatalogEntry, load_catalog};
pub use config::{Cli, MasterConfig, load_config};
pub use dispatch::{ClearTaintOutcome, Dispatcher, HaltOutcome};
pub use management::{AgentView, ManagementState, agent_view, router};
pub use registry::{AgentInfo, Registry, new_registry, set_taint};
pub use server::{MasterService, ServerError, run_server};
