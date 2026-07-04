use ::config::{Config, Environment, File};
use clap::Parser;
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct MasterConfig {
    pub listen_addr: String,
    pub management_listen_addr: String,
    pub heartbeat_interval_secs: u32,
    pub catalog_root: String,
    pub default_grace_secs: u32,
}

#[derive(Parser)]
#[command(name = "faultforge-master")]
pub struct Cli {
    #[arg(long, env = "FAULTFORGE_CONFIG")]
    pub config: Option<String>,
    #[arg(long)]
    pub listen_addr: Option<String>,
    #[arg(long)]
    pub management_listen_addr: Option<String>,
    #[arg(long)]
    pub heartbeat_interval_secs: Option<u32>,
    #[arg(long)]
    pub catalog_root: Option<String>,
    #[arg(long)]
    pub default_grace_secs: Option<u32>,
}

/// Load master configuration from all sources (file, env, CLI flags).
///
/// # Errors
///
/// Returns `Err` if the configuration file cannot be read, a value cannot be
/// parsed, or `heartbeat_interval_secs` is 0.
pub fn load_config(cli: &Cli) -> Result<MasterConfig, ::config::ConfigError> {
    let mut builder = Config::builder()
        .set_default("listen_addr", "127.0.0.1:50051")?
        .set_default("management_listen_addr", "127.0.0.1:8069")?
        .set_default("heartbeat_interval_secs", 5_i64)?
        .set_default("catalog_root", "/usr/lib/faultforge/plugins")?
        .set_default("default_grace_secs", 10_i64)?;

    builder = builder.add_source(
        File::with_name(cli.config.as_deref().unwrap_or("faultforge-master"))
            .required(cli.config.is_some()),
    );

    // No .separator() so field names with underscores are matched literally.
    builder = builder.add_source(Environment::with_prefix("FAULTFORGE").try_parsing(true));

    if let Some(addr) = &cli.listen_addr {
        builder = builder.set_override("listen_addr", addr.as_str())?;
    }
    if let Some(addr) = &cli.management_listen_addr {
        builder = builder.set_override("management_listen_addr", addr.as_str())?;
    }
    if let Some(secs) = cli.heartbeat_interval_secs {
        builder = builder.set_override("heartbeat_interval_secs", i64::from(secs))?;
    }
    if let Some(root) = &cli.catalog_root {
        builder = builder.set_override("catalog_root", root.as_str())?;
    }
    if let Some(secs) = cli.default_grace_secs {
        builder = builder.set_override("default_grace_secs", i64::from(secs))?;
    }

    let cfg: MasterConfig = builder.build()?.try_deserialize()?;
    if cfg.heartbeat_interval_secs == 0 {
        return Err(::config::ConfigError::Message(
            "heartbeat_interval_secs must be greater than 0".into(),
        ));
    }
    // Zero grace would make the dead-man fire the instant the duration ends,
    // turning every graceful stop into a race (ADR-0002 §5).
    if cfg.default_grace_secs == 0 {
        return Err(::config::ConfigError::Message(
            "default_grace_secs must be greater than 0".into(),
        ));
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli() -> Cli {
        Cli {
            config: None,
            listen_addr: None,
            management_listen_addr: None,
            heartbeat_interval_secs: None,
            catalog_root: None,
            default_grace_secs: None,
        }
    }

    #[test]
    fn master_config_loads_defaults() {
        let cfg = load_config(&cli()).unwrap();
        assert_eq!(cfg.listen_addr, "127.0.0.1:50051");
        assert_eq!(cfg.management_listen_addr, "127.0.0.1:8069");
        assert_eq!(cfg.heartbeat_interval_secs, 5);
        assert_eq!(cfg.catalog_root, "/usr/lib/faultforge/plugins");
        assert_eq!(cfg.default_grace_secs, 10);
    }

    #[test]
    fn master_config_override_takes_precedence() {
        let cfg = load_config(&Cli {
            listen_addr: Some("0.0.0.0:9090".into()),
            management_listen_addr: Some("0.0.0.0:9091".into()),
            heartbeat_interval_secs: Some(30),
            catalog_root: Some("/opt/plugins".into()),
            default_grace_secs: Some(20),
            ..cli()
        })
        .unwrap();
        assert_eq!(cfg.listen_addr, "0.0.0.0:9090");
        assert_eq!(cfg.management_listen_addr, "0.0.0.0:9091");
        assert_eq!(cfg.heartbeat_interval_secs, 30);
        assert_eq!(cfg.catalog_root, "/opt/plugins");
        assert_eq!(cfg.default_grace_secs, 20);
    }

    #[test]
    fn zero_heartbeat_interval_is_rejected() {
        let err = load_config(&Cli {
            heartbeat_interval_secs: Some(0),
            ..cli()
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("heartbeat_interval_secs"),
            "expected message mentioning heartbeat_interval_secs, got: {err}"
        );
    }

    #[test]
    fn zero_grace_is_rejected() {
        let err = load_config(&Cli {
            default_grace_secs: Some(0),
            ..cli()
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("default_grace_secs"),
            "expected message mentioning default_grace_secs, got: {err}"
        );
    }
}
