use clap::Parser;
use config::{Config, Environment, File};
use serde::Deserialize;

// ===== Config =====

/// Default catalog root for baked-in plugins (ADR-0002 §4).
pub const DEFAULT_PLUGIN_ROOT: &str = "/usr/lib/faultforge/plugins";
/// Default state directory for the instance journal and taint record (ADR-0002 §6).
pub const DEFAULT_DATA_DIR: &str = "/var/lib/faultforge";
/// Default master-loss self-abort threshold. Agent-side config, not a master
/// directive: it must keep working exactly when the master is unreachable.
pub const DEFAULT_MASTER_LOSS_THRESHOLD_SECS: u64 = 30;
/// Default hard cap on a single plugin invocation. Plugins are one-shot commands
/// by contract; a manifest-level knob can arrive compatibly when a slow plugin
/// exists (design D7).
pub const DEFAULT_INVOCATION_TIMEOUT_SECS: u64 = 60;

fn default_plugin_root() -> String {
    DEFAULT_PLUGIN_ROOT.to_string()
}

fn default_data_dir() -> String {
    DEFAULT_DATA_DIR.to_string()
}

fn default_master_loss_threshold_secs() -> u64 {
    DEFAULT_MASTER_LOSS_THRESHOLD_SECS
}

fn default_invocation_timeout_secs() -> u64 {
    DEFAULT_INVOCATION_TIMEOUT_SECS
}

#[derive(Debug, Deserialize, Clone)]
pub struct AgentConfig {
    pub master_addr: String,
    /// Root of the on-disk plugin catalog (`<plugin_root>/<name>@<version>/`).
    #[serde(default = "default_plugin_root")]
    pub plugin_root: String,
    /// State directory: `<data_dir>/instances/` journal and `<data_dir>/tainted.json`.
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
    /// How long the master may be unreachable before active instances self-abort.
    #[serde(default = "default_master_loss_threshold_secs")]
    pub master_loss_threshold_secs: u64,
    /// Hard cap on a single plugin invocation before it is killed.
    #[serde(default = "default_invocation_timeout_secs")]
    pub invocation_timeout_secs: u64,
}

#[derive(Parser)]
#[command(name = "faultforge-agent")]
pub struct Cli {
    #[arg(long, env = "FAULTFORGE_CONFIG")]
    pub config: Option<String>,
    #[arg(long)]
    pub master_addr: Option<String>,
    #[arg(long)]
    pub plugin_root: Option<String>,
    #[arg(long)]
    pub data_dir: Option<String>,
    #[arg(long)]
    pub master_loss_threshold_secs: Option<u64>,
}

/// Load agent configuration from all sources (file, env, CLI flags).
///
/// # Errors
///
/// Returns `Err` if the configuration file cannot be read, a value cannot be
/// parsed, or `master_addr` is absent from all sources.
pub fn load_config(cli: &Cli) -> Result<AgentConfig, config::ConfigError> {
    // master_addr has no default — try_deserialize fails if absent from all sources.
    let mut builder = Config::builder();

    builder = builder.add_source(
        File::with_name(cli.config.as_deref().unwrap_or("faultforge-agent"))
            .required(cli.config.is_some()),
    );

    // No .separator() so field names with underscores are matched literally.
    builder = builder.add_source(Environment::with_prefix("FAULTFORGE").try_parsing(true));

    if let Some(addr) = &cli.master_addr {
        builder = builder.set_override("master_addr", addr.as_str())?;
    }
    if let Some(root) = &cli.plugin_root {
        builder = builder.set_override("plugin_root", root.as_str())?;
    }
    if let Some(dir) = &cli.data_dir {
        builder = builder.set_override("data_dir", dir.as_str())?;
    }
    if let Some(secs) = cli.master_loss_threshold_secs {
        builder = builder.set_override("master_loss_threshold_secs", secs)?;
    }

    builder.build()?.try_deserialize()
}

// ===== Unit tests =====

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(args: &[&str]) -> Cli {
        Cli::parse_from(std::iter::once("faultforge-agent").chain(args.iter().copied()))
    }

    #[test]
    fn missing_master_addr_fails() {
        let result = Config::builder()
            .build()
            .unwrap()
            .try_deserialize::<AgentConfig>();
        assert!(
            result.is_err(),
            "must fail when master_addr is not provided"
        );
    }

    #[test]
    fn runtime_paths_default_when_absent() {
        let cfg = load_config(&cli(&["--master-addr", "http://localhost:50051"])).unwrap();
        assert_eq!(cfg.plugin_root, DEFAULT_PLUGIN_ROOT);
        assert_eq!(cfg.data_dir, DEFAULT_DATA_DIR);
        assert_eq!(
            cfg.master_loss_threshold_secs,
            DEFAULT_MASTER_LOSS_THRESHOLD_SECS
        );
        assert_eq!(cfg.invocation_timeout_secs, DEFAULT_INVOCATION_TIMEOUT_SECS);
    }

    #[test]
    fn cli_flags_override_runtime_paths() {
        let cfg = load_config(&cli(&[
            "--master-addr",
            "http://localhost:50051",
            "--plugin-root",
            "/opt/plugins",
            "--data-dir",
            "/tmp/ff-state",
            "--master-loss-threshold-secs",
            "7",
        ]))
        .unwrap();
        assert_eq!(cfg.plugin_root, "/opt/plugins");
        assert_eq!(cfg.data_dir, "/tmp/ff-state");
        assert_eq!(cfg.master_loss_threshold_secs, 7);
    }
}
