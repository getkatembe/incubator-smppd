//! Upstream SMSC endpoint.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::bootstrap::Shutdown;
use crate::config::{EndpointConfig, HealthCheckConfig, PoolConfig};
use crate::telemetry::counters;

use super::circuit_breaker::CircuitBreaker;
use super::health::{HealthChecker, HealthState};
use super::pool::{ConnectionPool, PooledConnection};

/// State of an endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointState {
    /// Endpoint is healthy and accepting connections
    Healthy,
    /// Endpoint is unhealthy (failed health checks)
    Unhealthy,
    /// Endpoint is draining (not accepting new connections)
    Draining,
}

/// An upstream SMSC endpoint.
pub struct Endpoint {
    /// Cluster name
    cluster: String,

    /// Endpoint index within cluster (for logging/debugging)
    #[allow(dead_code)]
    index: usize,

    /// Endpoint address (host:port)
    address: String,

    /// SMPP credentials
    system_id: String,
    password: String,
    #[allow(dead_code)]
    system_type: String,

    /// Connection weight for weighted load balancing
    weight: u32,

    /// Connection pool
    pool: ConnectionPool,

    /// Health checker
    health: HealthChecker,

    /// Circuit breaker
    circuit_breaker: CircuitBreaker,

    /// Current state
    state: RwLock<EndpointState>,

    /// Active connection count
    active_connections: AtomicUsize,

    /// Total requests served
    total_requests: AtomicUsize,

    /// Is healthy flag (for fast access)
    healthy: AtomicBool,
}

impl Endpoint {
    /// Create a new endpoint.
    pub fn new(
        cluster: &str,
        index: usize,
        config: &EndpointConfig,
        pool_config: &PoolConfig,
        health_config: &HealthCheckConfig,
    ) -> Self {
        Self {
            cluster: cluster.to_string(),
            index,
            address: config.address.clone(),
            system_id: config.system_id.clone(),
            password: config.password.clone(),
            system_type: config.system_type.clone(),
            weight: config.weight,
            pool: ConnectionPool::new(
                cluster,
                &config.address,
                &config.system_id,
                &config.password,
                pool_config,
            ),
            health: HealthChecker::new(health_config),
            circuit_breaker: CircuitBreaker::new(),
            state: RwLock::new(EndpointState::Healthy),
            active_connections: AtomicUsize::new(0),
            total_requests: AtomicUsize::new(0),
            healthy: AtomicBool::new(true),
        }
    }

    /// Get endpoint address.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Get endpoint weight.
    pub fn weight(&self) -> u32 {
        self.weight
    }

    /// Get active connection count.
    pub fn active_connections(&self) -> usize {
        self.active_connections.load(Ordering::Relaxed)
    }

    /// Get total requests served.
    pub fn total_requests(&self) -> usize {
        self.total_requests.load(Ordering::Relaxed)
    }

    /// Check if endpoint is healthy.
    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Relaxed) && self.circuit_breaker.is_closed()
    }

    /// Get current state.
    pub async fn state(&self) -> EndpointState {
        *self.state.read().await
    }

    /// Set endpoint state.
    async fn set_state(&self, state: EndpointState) {
        let old_state = *self.state.read().await;
        if old_state != state {
            *self.state.write().await = state;
            self.healthy.store(state == EndpointState::Healthy, Ordering::SeqCst);

            info!(
                cluster = %self.cluster,
                endpoint = %self.address,
                from = ?old_state,
                to = ?state,
                "endpoint state changed"
            );

            counters::upstream_health_changed(&self.cluster, &self.address, state == EndpointState::Healthy);
        }
    }

    /// Get a connection from the pool.
    pub async fn get_connection(&self) -> Option<PooledConnection> {
        if !self.is_healthy() {
            return None;
        }

        // Check circuit breaker
        if !self.circuit_breaker.allow_request() {
            debug!(
                cluster = %self.cluster,
                endpoint = %self.address,
                "circuit breaker open"
            );
            return None;
        }

        match self.pool.get().await {
            Some(conn) => {
                self.active_connections.fetch_add(1, Ordering::SeqCst);
                self.total_requests.fetch_add(1, Ordering::SeqCst);
                Some(conn)
            }
            None => {
                self.circuit_breaker.record_failure();
                None
            }
        }
    }

    /// Return a connection to the pool.
    pub fn return_connection(&self, success: bool) {
        self.active_connections.fetch_sub(1, Ordering::SeqCst);

        if success {
            self.circuit_breaker.record_success();
        } else {
            self.circuit_breaker.record_failure();
        }
    }

    /// Run health check loop.
    pub async fn run_health_check(self: Arc<Self>, shutdown: Arc<Shutdown>) {
        let mut shutdown_rx = shutdown.subscribe();
        let interval = self.health.interval();

        info!(
            cluster = %self.cluster,
            endpoint = %self.address,
            interval_secs = interval.as_secs(),
            "starting health checks"
        );

        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    debug!(
                        cluster = %self.cluster,
                        endpoint = %self.address,
                        "stopping health checks"
                    );
                    break;
                }
                _ = tokio::time::sleep(interval) => {
                    self.perform_health_check().await;
                }
            }
        }
    }

    /// Perform a single health check.
    async fn perform_health_check(&self) {
        let result = self.health.check(&self.address, &self.system_id, &self.password).await;

        match result {
            HealthState::Healthy => {
                if self.health.record_success() {
                    self.set_state(EndpointState::Healthy).await;
                }
            }
            HealthState::Unhealthy(ref reason) => {
                warn!(
                    cluster = %self.cluster,
                    endpoint = %self.address,
                    reason = %reason,
                    "health check failed"
                );

                if self.health.record_failure() {
                    self.set_state(EndpointState::Unhealthy).await;
                }
            }
        }

        let success = matches!(result, HealthState::Healthy);
        counters::upstream_health_check(&self.cluster, &self.address, success);
    }
}
