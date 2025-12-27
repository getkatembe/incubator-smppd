use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use tracing::{info, Level};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use smppd::bootstrap::Server;
use smppd::config::Config;

#[derive(Parser, Debug)]
#[command(name = "smppd")]
#[command(author, version, about = "High-performance SMPP proxy and router")]
struct Args {
    /// Path to config file
    #[arg(short, long, value_name = "FILE")]
    config: PathBuf,

    /// Log level (trace, debug, info, warn, error)
    #[arg(short, long, default_value = "info")]
    log_level: Level,

    /// Log format (json, pretty)
    #[arg(long, default_value = "pretty")]
    log_format: LogFormat,

    /// Validate config and exit
    #[arg(long)]
    validate: bool,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum LogFormat {
    Json,
    Pretty,
}

fn init_logging(level: Level, format: LogFormat) {
    let filter = EnvFilter::from_default_env()
        .add_directive(level.into());

    match format {
        LogFormat::Json => {
            tracing_subscriber::registry()
                .with(filter)
                .with(fmt::layer().json())
                .init();
        }
        LogFormat::Pretty => {
            tracing_subscriber::registry()
                .with(filter)
                .with(fmt::layer().pretty())
                .init();
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    init_logging(args.log_level, args.log_format);

    info!(
        version = env!("CARGO_PKG_VERSION"),
        config = %args.config.display(),
        "starting smppd"
    );

    // Load configuration
    let config = Config::load(&args.config)?;

    info!(
        listeners = config.listeners.len(),
        clusters = config.clusters.len(),
        "configuration loaded"
    );

    // Validate only mode
    if args.validate {
        info!("configuration is valid");
        return Ok(());
    }

    // Create and run server
    let server = Server::new(config)?;
    server.run().await?;

    Ok(())
}
