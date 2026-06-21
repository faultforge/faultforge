use clap::Parser;
use faultforge_master::{Cli, load_config, run_server};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cfg = match load_config(&cli) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("configuration error: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = run_server(cfg).await {
        tracing::error!("fatal: {e}");
        std::process::exit(1);
    }
}
