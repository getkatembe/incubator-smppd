use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use tracing::info;

use smppd::bootstrap::Server;
use smppd::config::Config;
use smppd::telemetry::{init_tracing, TracingConfig};

#[derive(Parser, Debug)]
#[command(name = "smppd")]
#[command(author, version, about = "High-performance SMPP proxy and router")]
struct Args {
    /// Path to config file
    #[arg(short, long, value_name = "FILE")]
    config: PathBuf,

    /// Validate config and exit
    #[arg(long)]
    validate: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Load configuration first (to get log settings)
    let config = Config::load(&args.config)?;

    // Initialize tracing with config-based settings
    let tracing_config = TracingConfig {
        service_name: "smppd".to_string(),
        log_level: config.telemetry.log_level.clone(),
        json_logs: config.telemetry.json_logs,
        otlp_endpoint: config.telemetry.otlp_endpoint.clone(),
        sample_rate: config.telemetry.trace_sample_rate,
    };

    init_tracing(&tracing_config)?;

    info!(
        version = env!("CARGO_PKG_VERSION"),
        config = %args.config.display(),
        "starting smppd"
    );

    info!(
        listeners = config.listeners.len(),
        clusters = config.clusters.len(),
        routes = config.routes.len(),
        "configuration loaded"
    );

    // Validate only mode
    if args.validate {
        info!("configuration is valid");
        return Ok(());
    }

    // Create and run server
    let server = Server::new(config, args.config).await?;
    server.run().await?;

    Ok(())
}
