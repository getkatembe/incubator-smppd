use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::signal;
use tokio::sync::RwLock;
use tracing::{info, warn, error, span, Level};

use crate::admin::{AdminServer, AdminState};
use crate::config::Config;
use crate::forwarder::{self, ForwarderConfig};
use crate::listener::Listeners;
use crate::telemetry::{counters, Metrics, MetricsConfig};

use super::events::{Event, EventBus};
use super::shutdown::Shutdown;
use super::state::{GatewayState, SharedGatewayState};
use super::worker::{WorkerConfig, Workers};

/// Main smppd server
///
/// Architecture (Envoy-inspired):
/// - Main thread: signal handling, config reload, worker coordination
/// - Worker threads: connection handling (one runtime per thread)
/// - Listeners: accept incoming connections, drain on shutdown
/// - Event bus: internal component communication
/// - Shutdown: graceful drain with configurable timeout
/// - GatewayState: shared state (store, feedback, router, clusters)
pub struct Server {
    config: Arc<Config>,
    config_path: PathBuf,
    shutdown: Arc<Shutdown>,
    events: Arc<EventBus>,
    state: SharedGatewayState,
    /// Workers wrapped in RwLock for hot-reload replacement
    workers: Arc<RwLock<Option<Workers>>>,
}

impl Server {
    /// Create a new server
    pub async fn new(config: Config, config_path: PathBuf) -> Result<Self> {
        let config = Arc::new(config);
        let shutdown = Shutdown::new(config.settings.shutdown.drain_timeout);
        let events = EventBus::new(config.telemetry.event_bus_capacity);

        // Create gateway state with all shared components
        let state = Arc::new(GatewayState::new(config.clone(), shutdown.clone()).await?);

        Ok(Self {
            config,
            config_path,
            shutdown,
            events,
            state,
            workers: Arc::new(RwLock::new(None)),
        })
    }

    /// Create workers with current config
    fn create_workers(&self) -> Workers {
        let worker_config = WorkerConfig {
            workers: self.config.telemetry.workers,
            stack_size: self.config.telemetry.worker_stack_size,
            name_prefix: "smppd-worker".to_string(),
        };
        Workers::new(worker_config, self.shutdown.clone())
    }

    /// Run the server
    pub async fn run(mut self) -> Result<()> {
        let span = span!(Level::INFO, "smppd", version = env!("CARGO_PKG_VERSION"));
        let _enter = span.enter();

        self.events.publish(Event::Starting);

        info!(
            listeners = self.config.listeners.len(),
            clusters = self.config.clusters.len(),
            routes = self.config.routes.len(),
            workers = self.config.telemetry.workers,
            "starting server"
        );

        // Initialize metrics (OTEL → Prometheus)
        let metrics = Metrics::new(&MetricsConfig {
            address: self.config.admin.address,
        })?;

        // Initialize counters
        counters::init(&metrics.meter("smppd"));

        // Create admin state with reload capability
        let admin_state = Arc::new(AdminState::new(
            self.config_path.clone(),
            self.config.clone(),
            self.state.clone(),
            self.shutdown.clone(),
        ));

        // Spawn admin server FIRST (it provides /metrics endpoint)
        let admin_config = self.config.admin.clone();
        let admin_state_clone = admin_state.clone();
        let admin_shutdown = self.shutdown.clone();
        let admin_handle = tokio::spawn(async move {
            let server = AdminServer::new(&admin_config, admin_state_clone, admin_shutdown);
            if let Err(e) = server.run().await {
                error!(error = %e, "admin server failed");
            }
        });

        // Give admin server time to bind
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Skip standalone metrics server since admin server already has /metrics
        // The Prometheus exporter is already configured and will be scraped via admin server
        let _metrics = metrics;

        // Start workers
        {
            let workers = self.create_workers();
            info!(count = workers.len(), "workers started");
            *self.workers.write().await = Some(workers);
        }

        // Start the message forwarder
        let forwarder_config = ForwarderConfig::default();
        let forwarder_handle = forwarder::start(
            forwarder_config,
            self.state.clone(),
            self.shutdown.clone(),
        );
        info!("message forwarder started");

        // Start listeners for SMPP connections
        let _listeners = match Listeners::new(&self.config.listeners, self.shutdown.clone(), forwarder_handle) {
            Ok(listeners) => {
                listeners.start();
                info!(count = listeners.len(), "listeners started");
                Some(listeners)
            }
            Err(e) => {
                // Log error but continue - allows running with just admin API for testing
                error!(error = %e, "failed to start listeners (TLS cert missing?)");
                warn!("running without SMPP listeners - admin API only mode");
                None
            }
        };

        // Mark server as ready
        admin_state.set_ready(true);

        // Log configuration
        self.log_config();

        self.events.publish(Event::Ready);

        info!(
            admin = %self.config.admin.address,
            drain_timeout_secs = self.config.settings.shutdown.drain_timeout.as_secs(),
            "server ready"
        );

        // Start maintenance task (message expiration, pruning)
        let maintenance_state = self.state.clone();
        let maintenance_shutdown = self.shutdown.clone();
        let maintenance_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                if maintenance_shutdown.state() != super::shutdown::State::Running {
                    break;
                }
                // Run maintenance: expire messages older than 1 hour, prune terminal older than 24 hours
                maintenance_state.maintenance(
                    Duration::from_secs(3600),
                    Duration::from_secs(86400),
                );
            }
        });

        // Main loop: wait for signals
        self.run_signal_loop().await;

        // ============================================================
        // GRACEFUL SHUTDOWN SEQUENCE
        // 1. Stop maintenance
        // 2. Mark as not ready (health checks fail)
        // 3. Start drain (stop accepting new connections)
        // 4. Wait for existing connections to finish (with timeout)
        // 5. Shutdown workers
        // 6. Terminate
        // ============================================================

        self.events.publish(Event::ShutdownStarted);
        info!("shutdown initiated");

        // Step 1: Stop maintenance task
        maintenance_handle.abort();

        // Step 2: Mark as not ready for Kubernetes
        admin_state.set_ready(false);
        admin_state.set_healthy(false);

        // Step 3: Start drain - listeners will stop accepting new connections
        // and begin draining existing connections
        self.shutdown.start_drain();
        self.events.publish(Event::DrainStarted);

        info!(
            active_connections = self.shutdown.active_connections(),
            drain_timeout_secs = self.shutdown.drain_timeout().as_secs(),
            "draining connections"
        );

        // Step 4: Wait for connections to drain (with timeout)
        let drain_timeout = self.shutdown.drain_timeout();
        let shutdown_watch = self.shutdown.clone();
        let drain_result = tokio::time::timeout(drain_timeout, async move {
            let mut rx = shutdown_watch.subscribe();
            loop {
                // Check if all connections are drained
                if shutdown_watch.active_connections() == 0 {
                    info!("all connections drained successfully");
                    break;
                }
                // Wait for state change
                if rx.changed().await.is_err() {
                    break;
                }
                if *rx.borrow() == super::shutdown::State::Terminated {
                    break;
                }
            }
        })
        .await;

        if drain_result.is_err() {
            warn!(
                active = self.shutdown.active_connections(),
                "drain timeout exceeded, forcing shutdown"
            );
        }

        // Step 5: Shutdown workers after connections are drained
        info!("shutting down workers");
        {
            let mut workers_guard = self.workers.write().await;
            if let Some(mut workers) = workers_guard.take() {
                workers.shutdown();
            }
        }

        // Step 6: Terminate everything
        self.shutdown.terminate();

        // Stop admin server
        admin_handle.abort();

        crate::telemetry::shutdown_tracing();

        self.events.publish(Event::ShutdownComplete);
        info!("server stopped");

        Ok(())
    }

    /// Log configuration at startup
    fn log_config(&self) {
        for listener in &self.config.listeners {
            info!(
                name = %listener.name,
                address = %listener.address,
                protocol = ?listener.protocol,
                tls = listener.tls.is_some(),
                "listener"
            );
        }

        for cluster in &self.config.clusters {
            if cluster.mock.is_some() {
                info!(name = %cluster.name, mode = "mock", "cluster");
            } else {
                info!(
                    name = %cluster.name,
                    endpoints = cluster.endpoints.len(),
                    lb = ?cluster.load_balancer,
                    "cluster"
                );
            }
        }

        for route in &self.config.routes {
            let target = if let Some(ref cluster) = route.cluster {
                cluster.clone()
            } else if let Some(ref splits) = route.split {
                format!("split({})", splits.len())
            } else {
                "none".to_string()
            };
            info!(
                name = %route.name,
                target = %target,
                priority = route.priority,
                "route"
            );
        }
    }

    /// Main signal handling loop
    async fn run_signal_loop(&mut self) {
        loop {
            tokio::select! {
                _ = Self::wait_for_shutdown() => {
                    break;
                }
                _ = Self::wait_for_reload() => {
                    self.reload_config().await;
                }
            }
        }
    }

    /// Wait for shutdown signal (SIGINT/SIGTERM)
    async fn wait_for_shutdown() {
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
                info!("received SIGINT");
            }
            _ = terminate => {
                info!("received SIGTERM");
            }
        }
    }

    /// Wait for reload signal (SIGHUP)
    #[cfg(unix)]
    async fn wait_for_reload() {
        let mut sighup = signal::unix::signal(signal::unix::SignalKind::hangup())
            .expect("failed to install SIGHUP handler");
        sighup.recv().await;
    }

    #[cfg(not(unix))]
    async fn wait_for_reload() {
        std::future::pending::<()>().await
    }

    /// Reload configuration on SIGHUP
    ///
    /// Graceful reload sequence:
    /// 1. Load and validate new config
    /// 2. Drain existing connections (wait for in-flight to complete)
    /// 3. Shutdown old workers
    /// 4. Reload gateway state (router, clusters)
    /// 5. Create new workers
    /// 6. Resume accepting connections
    async fn reload_config(&mut self) {
        info!(path = %self.config_path.display(), "reloading configuration via SIGHUP");

        // Step 1: Load new config from disk
        let new_config = match Config::load(&self.config_path) {
            Ok(config) => Arc::new(config),
            Err(e) => {
                error!(error = %e, "configuration reload failed: load error");
                return;
            }
        };

        // Step 2: Drain existing connections
        // Note: For hot-reload, we use a shorter drain timeout
        let reload_drain_timeout = Duration::from_secs(10);
        let active = self.shutdown.active_connections();
        if active > 0 {
            info!(active_connections = active, "draining connections for reload");

            // Wait for connections to drain with shorter timeout
            let drain_start = std::time::Instant::now();
            while self.shutdown.active_connections() > 0 {
                if drain_start.elapsed() > reload_drain_timeout {
                    warn!(
                        remaining = self.shutdown.active_connections(),
                        "reload drain timeout, proceeding with active connections"
                    );
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }

        // Step 3: Shutdown old workers
        info!("shutting down old workers for reload");
        {
            let mut workers_guard = self.workers.write().await;
            if let Some(mut old_workers) = workers_guard.take() {
                old_workers.shutdown();
            }
        }

        // Step 4: Reload gateway state (router, clusters)
        if let Err(e) = self.state.reload(new_config.clone(), self.shutdown.clone()).await {
            error!(error = %e, "configuration reload failed: state update error");
            // Create new workers anyway to recover
        }

        // Step 5: Create new workers with updated config
        self.config = new_config.clone();
        {
            let new_workers = self.create_workers();
            info!(count = new_workers.len(), "new workers started");
            *self.workers.write().await = Some(new_workers);
        }

        info!(
            listeners = new_config.listeners.len(),
            clusters = new_config.clusters.len(),
            routes = new_config.routes.len(),
            "configuration reloaded successfully"
        );

        counters::config_reloaded();
        self.events.publish(Event::ConfigReloaded(new_config));
    }

    /// Get event bus
    pub fn events(&self) -> Arc<EventBus> {
        self.events.clone()
    }

    /// Get shutdown handle
    pub fn shutdown(&self) -> Arc<Shutdown> {
        self.shutdown.clone()
    }

    /// Get gateway state
    pub fn state(&self) -> SharedGatewayState {
        self.state.clone()
    }
}
