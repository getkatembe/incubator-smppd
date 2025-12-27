use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::signal;
use tokio::sync::watch;
use tracing::{info, warn, error, span, Level};

use crate::config::{Config, ConfigWatcher};
use crate::telemetry::{MetricsConfig, MetricsServer};

use super::shutdown::ShutdownManager;
use super::worker::{WorkerConfig, WorkerPool};

/// Main smppd server (Envoy-inspired architecture)
///
/// Components:
/// - Main thread: signal handling, config reload, worker management
/// - Worker threads: connection handling (one runtime per thread)
/// - Shutdown manager: graceful drain with configurable timeout
/// - Config watcher: hot reload on SIGHUP or file change
/// - Metrics server: Prometheus endpoint on admin port
pub struct Server {
    /// Configuration
    config: Arc<Config>,

    /// Config file path (for hot reload)
    config_path: PathBuf,

    /// Shutdown manager
    shutdown: Arc<ShutdownManager>,
}

impl Server {
    /// Create a new server instance
    pub fn new(config: Config, config_path: PathBuf) -> Result<Self> {
        let shutdown = ShutdownManager::new(config.settings.shutdown.drain_timeout);

        Ok(Self {
            config: Arc::new(config),
            config_path,
            shutdown,
        })
    }

    /// Run the server until shutdown
    pub async fn run(self) -> Result<()> {
        let span = span!(Level::INFO, "smppd", version = env!("CARGO_PKG_VERSION"));
        let _enter = span.enter();

        info!(
            listeners = self.config.listeners.len(),
            clusters = self.config.clusters.len(),
            routes = self.config.routes.len(),
            workers = self.config.settings.workers,
            "starting smppd server"
        );

        // Start metrics server
        let metrics_config = MetricsConfig {
            address: self.config.admin.address,
            ..Default::default()
        };

        let metrics_server = MetricsServer::new(&metrics_config)?;

        // Spawn metrics server task
        let metrics_handle = tokio::spawn(async move {
            if let Err(e) = metrics_server.serve().await {
                error!(error = %e, "metrics server failed");
            }
        });

        // Start worker pool
        let worker_config = WorkerConfig {
            workers: self.config.settings.workers,
            ..Default::default()
        };

        let mut worker_pool = WorkerPool::new(worker_config, self.shutdown.clone());

        info!(
            workers = worker_pool.len(),
            "worker pool started"
        );

        // Start config watcher if hot reload enabled
        let config_watcher_handle = if self.config.settings.hot_reload {
            let watcher = ConfigWatcher::new(&self.config_path, (*self.config).clone())?;
            let (shutdown_tx, shutdown_rx) = watch::channel(false);

            let handle = tokio::spawn(async move {
                watcher.run(shutdown_rx).await;
            });

            Some((handle, shutdown_tx))
        } else {
            None
        };

        // Log listener information
        for listener in &self.config.listeners {
            info!(
                name = %listener.name,
                address = %listener.address,
                protocol = ?listener.protocol,
                tls = listener.tls.is_some(),
                "listener configured"
            );
        }

        // Log cluster information
        for cluster in &self.config.clusters {
            if cluster.mock.is_some() {
                info!(
                    name = %cluster.name,
                    mode = "mock",
                    "cluster configured"
                );
            } else {
                info!(
                    name = %cluster.name,
                    endpoints = cluster.endpoints.len(),
                    load_balancer = ?cluster.load_balancer,
                    "cluster configured"
                );
            }
        }

        // Log route information
        for route in &self.config.routes {
            info!(
                name = %route.name,
                cluster = %route.cluster,
                priority = route.priority,
                "route configured"
            );
        }

        info!(
            admin_address = %self.config.admin.address,
            metrics = self.config.admin.metrics,
            health = self.config.admin.health,
            hot_reload = self.config.settings.hot_reload,
            drain_timeout_secs = self.config.settings.shutdown.drain_timeout.as_secs(),
            "smppd server started"
        );

        // Record startup metrics
        metrics::counter!("smppd.server.starts").increment(1);

        // Wait for shutdown signal
        self.wait_for_shutdown().await;

        info!("shutdown signal received, starting graceful shutdown");

        // Start drain period
        self.shutdown.start_drain();

        // Wait for drain or timeout
        let drain_timeout = self.config.settings.shutdown.drain_timeout;
        let drain_result = tokio::time::timeout(drain_timeout, async {
            let mut rx = self.shutdown.subscribe();
            while *rx.borrow() != super::shutdown::ShutdownState::Terminated {
                if rx.changed().await.is_err() {
                    break;
                }
            }
        })
        .await;

        if drain_result.is_err() {
            warn!(
                active_connections = self.shutdown.active_connections(),
                "drain timeout reached, forcing shutdown"
            );
        }

        // Force terminate if not already
        self.shutdown.terminate();

        // Stop config watcher
        if let Some((handle, shutdown_tx)) = config_watcher_handle {
            let _ = shutdown_tx.send(true);
            let _ = handle.await;
        }

        // Stop worker pool
        worker_pool.shutdown();

        // Stop metrics server
        metrics_handle.abort();

        // Flush tracing
        crate::telemetry::shutdown_tracing();

        info!("smppd server stopped");

        Ok(())
    }

    /// Wait for shutdown signal (SIGINT, SIGTERM, or SIGHUP for reload)
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
                info!("received SIGINT (Ctrl+C)");
            }
            _ = terminate => {
                info!("received SIGTERM");
            }
        }
    }

    /// Get shutdown manager
    pub fn shutdown_manager(&self) -> Arc<ShutdownManager> {
        self.shutdown.clone()
    }
}
