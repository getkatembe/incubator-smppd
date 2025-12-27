use anyhow::Result;
use std::sync::Arc;
use tokio::signal;
use tokio::sync::watch;
use tracing::info;

use crate::config::Config;

/// Main smppd server
pub struct Server {
    config: Arc<Config>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
}

impl Server {
    /// Create a new server instance
    pub fn new(config: Config) -> Result<Self> {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        Ok(Self {
            config: Arc::new(config),
            shutdown_tx,
            shutdown_rx,
        })
    }

    /// Run the server until shutdown
    pub async fn run(self) -> Result<()> {
        info!(
            listeners = self.config.listeners.len(),
            clusters = self.config.clusters.len(),
            routes = self.config.routes.len(),
            "starting smppd server"
        );

        // Log listener information
        for listener in &self.config.listeners {
            info!(
                name = %listener.name,
                address = %listener.address,
                protocol = ?listener.protocol,
                "configured listener"
            );
        }

        // Log cluster information
        for cluster in &self.config.clusters {
            if cluster.mock.is_some() {
                info!(
                    name = %cluster.name,
                    "configured mock cluster"
                );
            } else {
                info!(
                    name = %cluster.name,
                    endpoints = cluster.endpoints.len(),
                    load_balancer = ?cluster.load_balancer,
                    "configured cluster"
                );
            }
        }

        // Log route information
        for route in &self.config.routes {
            info!(
                name = %route.name,
                cluster = %route.cluster,
                priority = route.priority,
                "configured route"
            );
        }

        // Admin API info
        info!(
            address = %self.config.admin.address,
            metrics = self.config.admin.metrics,
            health = self.config.admin.health,
            "admin API configured"
        );

        info!("smppd server started, waiting for shutdown signal");

        // Wait for shutdown signal
        self.wait_for_shutdown().await;

        info!("shutdown signal received, initiating graceful shutdown");

        // Signal all tasks to shutdown
        let _ = self.shutdown_tx.send(true);

        // TODO: Wait for all listeners to drain connections

        info!("smppd server stopped");

        Ok(())
    }

    /// Wait for shutdown signal (SIGINT or SIGTERM)
    async fn wait_for_shutdown(&self) {
        let ctrl_c = async {
            signal::ctrl_c()
                .await
                .expect("failed to install Ctrl+C handler");
        };

        #[cfg(unix)]
        let terminate = async {
            signal::unix::signal(signal::unix::SignalKind::terminate())
                .expect("failed to install SIGTERM handler")
                .recv()
                .await;
        };

        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => {
                info!("received Ctrl+C");
            }
            _ = terminate => {
                info!("received SIGTERM");
            }
        }
    }

    /// Get a clone of the shutdown receiver
    pub fn shutdown_signal(&self) -> watch::Receiver<bool> {
        self.shutdown_rx.clone()
    }

    /// Get a reference to the config
    pub fn config(&self) -> &Config {
        &self.config
    }
}
