//! Admin HTTP server.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    routing::{get, post},
    Router,
};
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tracing::info;

use crate::bootstrap::{SharedGatewayState, Shutdown};
use crate::config::{AdminConfig, Config};
use crate::store::StoreStats;
use crate::feedback::TargetStats;
use crate::telemetry::counters;

use super::handlers::{
    health_handler, live_handler, metrics_handler, ready_handler, stats_handler,
    reload_handler, store_stats_handler, feedback_stats_handler, ClusterStats,
};

/// Admin server state.
pub struct AdminState {
    /// Server start time
    start_time: Instant,
    /// Is the server healthy
    healthy: AtomicBool,
    /// Is the server ready
    ready: AtomicBool,
    /// Active connections
    active_connections: AtomicU64,
    /// Total connections
    total_connections: AtomicU64,
    /// Bound connections
    bound_connections: AtomicU64,
    /// Messages submitted
    messages_submitted: AtomicU64,
    /// Messages delivered
    messages_delivered: AtomicU64,
    /// Messages failed
    messages_failed: AtomicU64,
    /// Config file path (for reload)
    config_path: PathBuf,
    /// Gateway state (for reload and stats)
    gateway_state: SharedGatewayState,
    /// Shutdown handle (for reload)
    shutdown: Arc<Shutdown>,
    /// Current config (RwLock for updates)
    config: Arc<RwLock<Arc<Config>>>,
    /// Reload count
    reload_count: AtomicU64,
}

impl AdminState {
    /// Create new admin state with reload capability.
    pub fn new(
        config_path: PathBuf,
        config: Arc<Config>,
        gateway_state: SharedGatewayState,
        shutdown: Arc<Shutdown>,
    ) -> Self {
        Self {
            start_time: Instant::now(),
            healthy: AtomicBool::new(true),
            ready: AtomicBool::new(false),
            active_connections: AtomicU64::new(0),
            total_connections: AtomicU64::new(0),
            bound_connections: AtomicU64::new(0),
            messages_submitted: AtomicU64::new(0),
            messages_delivered: AtomicU64::new(0),
            messages_failed: AtomicU64::new(0),
            config_path,
            gateway_state,
            shutdown,
            config: Arc::new(RwLock::new(config)),
            reload_count: AtomicU64::new(0),
        }
    }

    /// Get uptime.
    pub fn uptime(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Check if healthy.
    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Relaxed)
    }

    /// Set health status.
    pub fn set_healthy(&self, healthy: bool) {
        self.healthy.store(healthy, Ordering::Relaxed);
    }

    /// Check if ready.
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }

    /// Set ready status.
    pub fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Relaxed);
    }

    /// Get active connections.
    pub fn active_connections(&self) -> u64 {
        self.active_connections.load(Ordering::Relaxed)
    }

    /// Increment active connections.
    pub fn inc_active_connections(&self) {
        self.active_connections.fetch_add(1, Ordering::Relaxed);
        self.total_connections.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement active connections.
    pub fn dec_active_connections(&self) {
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
    }

    /// Get total connections.
    pub fn total_connections(&self) -> u64 {
        self.total_connections.load(Ordering::Relaxed)
    }

    /// Get bound connections.
    pub fn bound_connections(&self) -> u64 {
        self.bound_connections.load(Ordering::Relaxed)
    }

    /// Increment bound connections.
    pub fn inc_bound_connections(&self) {
        self.bound_connections.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement bound connections.
    pub fn dec_bound_connections(&self) {
        self.bound_connections.fetch_sub(1, Ordering::Relaxed);
    }

    /// Get messages submitted.
    pub fn messages_submitted(&self) -> u64 {
        self.messages_submitted.load(Ordering::Relaxed)
    }

    /// Increment messages submitted.
    pub fn inc_messages_submitted(&self) {
        self.messages_submitted.fetch_add(1, Ordering::Relaxed);
    }

    /// Get messages delivered.
    pub fn messages_delivered(&self) -> u64 {
        self.messages_delivered.load(Ordering::Relaxed)
    }

    /// Increment messages delivered.
    pub fn inc_messages_delivered(&self) {
        self.messages_delivered.fetch_add(1, Ordering::Relaxed);
    }

    /// Get messages failed.
    pub fn messages_failed(&self) -> u64 {
        self.messages_failed.load(Ordering::Relaxed)
    }

    /// Increment messages failed.
    pub fn inc_messages_failed(&self) {
        self.messages_failed.fetch_add(1, Ordering::Relaxed);
    }

    /// Get message rate (per second).
    pub fn message_rate(&self) -> f64 {
        let total = self.messages_submitted();
        let elapsed = self.uptime().as_secs_f64();
        if elapsed > 0.0 {
            total as f64 / elapsed
        } else {
            0.0
        }
    }

    /// Get cluster stats.
    pub fn cluster_stats(&self) -> Vec<ClusterStats> {
        // Get from gateway state clusters
        self.gateway_state
            .clusters
            .names()
            .iter()
            .filter_map(|name| {
                self.gateway_state.clusters.get(name).map(|cluster| ClusterStats {
                    name: name.to_string(),
                    endpoints: cluster.endpoint_count(),
                    healthy_endpoints: cluster.healthy_count(),
                    active_connections: 0, // TODO: track per-cluster connections
                })
            })
            .collect()
    }

    /// Reload configuration from disk.
    ///
    /// Returns Ok with the new config if successful, Err with the error message otherwise.
    pub async fn reload_config(&self) -> Result<ReloadResult, String> {
        info!(path = %self.config_path.display(), "reloading configuration via admin API");

        // Load new config from disk
        let new_config = Config::load(&self.config_path)
            .map_err(|e| format!("failed to load config: {}", e))?;

        let new_config = Arc::new(new_config);

        // Reload gateway state (router, clusters)
        self.gateway_state
            .reload(new_config.clone(), self.shutdown.clone())
            .await
            .map_err(|e| format!("failed to reload gateway state: {}", e))?;

        // Update stored config
        {
            let mut config = self.config.write().await;
            *config = new_config.clone();
        }

        // Increment reload count
        let count = self.reload_count.fetch_add(1, Ordering::Relaxed) + 1;
        counters::config_reloaded();

        info!(
            listeners = new_config.listeners.len(),
            clusters = new_config.clusters.len(),
            routes = new_config.routes.len(),
            reload_count = count,
            "configuration reloaded successfully"
        );

        Ok(ReloadResult {
            success: true,
            message: "Configuration reloaded successfully".to_string(),
            reload_count: count,
            listeners: new_config.listeners.len(),
            clusters: new_config.clusters.len(),
            routes: new_config.routes.len(),
        })
    }

    /// Get reload count.
    pub fn reload_count(&self) -> u64 {
        self.reload_count.load(Ordering::Relaxed)
    }

    /// Get store statistics.
    pub fn store_stats(&self) -> StoreStats {
        self.gateway_state.store_stats()
    }

    /// Get feedback statistics for all targets.
    pub fn all_feedback_stats(&self) -> Vec<(String, TargetStats)> {
        self.gateway_state.feedback.all_target_stats()
    }

    /// Get current config.
    pub async fn current_config(&self) -> Arc<Config> {
        self.config.read().await.clone()
    }
}

/// Result of a config reload operation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReloadResult {
    pub success: bool,
    pub message: String,
    pub reload_count: u64,
    pub listeners: usize,
    pub clusters: usize,
    pub routes: usize,
}

/// Admin HTTP server.
pub struct AdminServer {
    config: AdminConfig,
    state: Arc<AdminState>,
    shutdown: Arc<Shutdown>,
}

impl AdminServer {
    /// Create a new admin server.
    pub fn new(config: &AdminConfig, state: Arc<AdminState>, shutdown: Arc<Shutdown>) -> Self {
        Self {
            config: config.clone(),
            state,
            shutdown,
        }
    }

    /// Build the router.
    fn build_router(&self) -> Router {
        Router::new()
            // Kubernetes-style health endpoints
            .route("/healthz", get(health_handler))
            .route("/livez", get(live_handler))
            .route("/readyz", get(ready_handler))
            // Metrics and stats
            .route("/stats", get(stats_handler))
            .route("/metrics", get(metrics_handler))
            // Store and feedback stats
            .route("/store/stats", get(store_stats_handler))
            .route("/feedback/stats", get(feedback_stats_handler))
            // Config management
            .route("/config/reload", post(reload_handler))
            .with_state(self.state.clone())
    }

    /// Run the admin server.
    pub async fn run(self) -> std::io::Result<()> {
        let router = self.build_router();
        let addr = self.config.address;

        info!(address = %addr, "starting admin server");

        let listener = TcpListener::bind(addr).await?;
        let mut shutdown_rx = self.shutdown.subscribe();

        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.changed().await;
                info!("admin server shutting down");
            })
            .await?;

        Ok(())
    }
}
