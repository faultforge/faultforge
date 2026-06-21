use clap::Parser;
use config::{Config, Environment, File};
use serde::Deserialize;

// ===== Config =====

#[derive(Debug, Deserialize, Clone)]
pub struct AgentConfig {
    pub master_addr: String,
}

#[derive(Parser)]
#[command(name = "faultforge-agent")]
pub struct Cli {
    #[arg(long, env = "FAULTFORGE_CONFIG")]
    pub config: Option<String>,
    #[arg(long)]
    pub master_addr: Option<String>,
}

pub fn load_config(cli: &Cli) -> Result<AgentConfig, config::ConfigError> {
    // master_addr has no default — try_deserialize fails if absent from all sources.
    let mut builder = Config::builder();

    builder = builder.add_source(
        File::with_name(cli.config.as_deref().unwrap_or("faultforge-agent"))
            .required(cli.config.is_some()),
    );

    // Env var: FAULTFORGE_MASTER_ADDR
    // No .separator() so field names with underscores are matched literally.
    builder = builder.add_source(Environment::with_prefix("FAULTFORGE").try_parsing(true));

    if let Some(addr) = &cli.master_addr {
        builder = builder.set_override("master_addr", addr.as_str())?;
    }

    builder.build()?.try_deserialize()
}

// ===== Unit tests =====

#[cfg(test)]
mod tests {
    use super::*;

    // Task 4.3: AgentConfig loading fails with clear error when master_addr is absent
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
}
