use ::config::{Config, Environment, File};
use clap::Parser;
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct MasterConfig {
    pub listen_addr: String,
    pub heartbeat_interval_secs: u32,
}

#[derive(Parser)]
#[command(name = "faultforge-master")]
pub struct Cli {
    #[arg(long, env = "FAULTFORGE_CONFIG")]
    pub config: Option<String>,
    #[arg(long)]
    pub listen_addr: Option<String>,
    #[arg(long)]
    pub heartbeat_interval_secs: Option<u32>,
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
        .set_default("heartbeat_interval_secs", 5_i64)?;

    builder = builder.add_source(
        File::with_name(cli.config.as_deref().unwrap_or("faultforge-master"))
            .required(cli.config.is_some()),
    );

    // Env vars: FAULTFORGE_LISTEN_ADDR, FAULTFORGE_HEARTBEAT_INTERVAL_SECS
    // No .separator() so field names with underscores are matched literally.
    builder = builder.add_source(Environment::with_prefix("FAULTFORGE").try_parsing(true));

    if let Some(addr) = &cli.listen_addr {
        builder = builder.set_override("listen_addr", addr.as_str())?;
    }
    if let Some(secs) = cli.heartbeat_interval_secs {
        builder = builder.set_override("heartbeat_interval_secs", i64::from(secs))?;
    }

    let cfg: MasterConfig = builder.build()?.try_deserialize()?;
    if cfg.heartbeat_interval_secs == 0 {
        return Err(::config::ConfigError::Message(
            "heartbeat_interval_secs must be greater than 0".into(),
        ));
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_config_loads_defaults() {
        let cfg: MasterConfig = Config::builder()
            .set_default("listen_addr", "127.0.0.1:50051")
            .unwrap()
            .set_default("heartbeat_interval_secs", 5_i64)
            .unwrap()
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();
        assert_eq!(cfg.listen_addr, "127.0.0.1:50051");
        assert_eq!(cfg.heartbeat_interval_secs, 5);
    }

    #[test]
    fn master_config_override_takes_precedence() {
        let cfg: MasterConfig = Config::builder()
            .set_default("listen_addr", "127.0.0.1:50051")
            .unwrap()
            .set_default("heartbeat_interval_secs", 5_i64)
            .unwrap()
            .set_override("listen_addr", "0.0.0.0:9090")
            .unwrap()
            .set_override("heartbeat_interval_secs", 30_i64)
            .unwrap()
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();
        assert_eq!(cfg.listen_addr, "0.0.0.0:9090");
        assert_eq!(cfg.heartbeat_interval_secs, 30);
    }

    #[test]
    fn zero_heartbeat_interval_is_rejected() {
        let cli = Cli {
            config: None,
            listen_addr: None,
            heartbeat_interval_secs: Some(0),
        };
        let err = load_config(&cli).unwrap_err();
        assert!(
            err.to_string().contains("heartbeat_interval_secs"),
            "expected message mentioning heartbeat_interval_secs, got: {err}"
        );
    }
}
