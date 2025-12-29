//! Cluster module for managing upstream SMSC connections.
//!
//! Envoy-inspired architecture:
//! - Clusters group upstream endpoints
//! - Connection pools manage persistent connections
//! - Health checks monitor endpoint availability
//! - Load balancers distribute requests
//! - Circuit breakers protect against cascading failures

mod circuit_breaker;
mod endpoint;
mod health;
mod lb;
mod mock;
mod pool;

pub use circuit_breaker::{CircuitBreaker, CircuitState};
pub use endpoint::{Endpoint, EndpointState};
pub use health::{HealthChecker, HealthState};
pub use lb::{LoadBalancer, LoadBalancerRegistry, RoundRobin, Weighted, LeastConnections, Random, PowerOfTwoChoices, ConsistentHash, WeightedLeastConn};
pub use mock::{MockSmsc, MockResult};
pub use pool::{ConnectionPool, PooledConnection};

use std::collections::HashMap;
use std::sync::Arc;

use tracing::{info, warn};

use crate::bootstrap::Shutdown;
use crate::config::{ClusterConfig, LoadBalancer as LbType};
use crate::telemetry::counters;

/// A cluster of upstream SMSC endpoints.
pub struct Cluster {
    /// Cluster name
    name: String,

    /// Endpoints in this cluster
    endpoints: Vec<Arc<Endpoint>>,

    /// Load balancer
    load_balancer: Box<dyn LoadBalancer>,

    /// Whether this is a mock cluster
    is_mock: bool,

    /// Shutdown handle
    shutdown: Arc<Shutdown>,
}

impl Cluster {
    /// Create a new cluster from config.
    pub async fn new(config: &ClusterConfig, shutdown: Arc<Shutdown>) -> Self {
        let endpoints: Vec<Arc<Endpoint>> = config
            .endpoints
            .iter()
            .enumerate()
            .map(|(i, ep_config)| {
                Arc::new(Endpoint::new(
                    &config.name,
                    i,
                    ep_config,
                    &config.pool,
                    &config.health_check,
                ))
            })
            .collect();

        let load_balancer: Box<dyn LoadBalancer> = match config.load_balancer {
            LbType::RoundRobin => Box::new(RoundRobin::new()),
            LbType::LeastConn => Box::new(LeastConnections::new()),
            LbType::Weighted => Box::new(Weighted::new(&config.endpoints)),
            LbType::Random => Box::new(Random::new()),
            LbType::P2c => Box::new(PowerOfTwoChoices::new()),
            LbType::ConsistentHash => Box::new(ConsistentHash::new(&config.endpoints, 100)),
            LbType::WeightedLeastConn => Box::new(WeightedLeastConn::new(&config.endpoints)),
        };

        info!(
            cluster = %config.name,
            endpoints = endpoints.len(),
            lb = ?config.load_balancer,
            "cluster created"
        );

        Self {
            name: config.name.clone(),
            endpoints,
            load_balancer,
            is_mock: config.mock.is_some(),
            shutdown,
        }
    }

    /// Get cluster name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get number of endpoints.
    pub fn endpoint_count(&self) -> usize {
        self.endpoints.len()
    }

    /// Check if this is a mock cluster.
    pub fn is_mock(&self) -> bool {
        self.is_mock
    }

    /// Start health checking for all endpoints.
    pub fn start_health_checks(&self) {
        for endpoint in &self.endpoints {
            let endpoint = endpoint.clone();
            let shutdown = self.shutdown.clone();

            tokio::spawn(async move {
                endpoint.run_health_check(shutdown).await;
            });
        }
    }

    /// Get a connection from the cluster.
    pub async fn get_connection(&self) -> Option<PooledConnection> {
        // Get healthy endpoints
        let healthy: Vec<&Arc<Endpoint>> = self
            .endpoints
            .iter()
            .filter(|ep| ep.is_healthy())
            .collect();

        if healthy.is_empty() {
            warn!(cluster = %self.name, "no healthy endpoints");
            counters::upstream_no_healthy_endpoints(&self.name);
            return None;
        }

        // Select endpoint using load balancer
        let selected = self.load_balancer.select(&healthy)?;

        // Get connection from endpoint's pool
        match selected.get_connection().await {
            Some(conn) => {
                counters::upstream_connection_acquired(&self.name);
                Some(conn)
            }
            None => {
                warn!(
                    cluster = %self.name,
                    endpoint = %selected.address(),
                    "failed to get connection"
                );
                counters::upstream_connection_failed(&self.name, selected.address(), "pool_exhausted");
                None
            }
        }
    }

    /// Get all endpoints.
    pub fn endpoints(&self) -> &[Arc<Endpoint>] {
        &self.endpoints
    }

    /// Get healthy endpoint count.
    pub fn healthy_count(&self) -> usize {
        self.endpoints.iter().filter(|ep| ep.is_healthy()).count()
    }

    /// Get total active connections across all endpoints.
    pub fn active_connections(&self) -> usize {
        self.endpoints.iter().map(|ep| ep.active_connections()).sum()
    }
}

/// Manager for all clusters.
pub struct Clusters {
    clusters: HashMap<String, Arc<Cluster>>,
}

impl Clusters {
    /// Create clusters from config.
    pub async fn new(configs: &[ClusterConfig], shutdown: Arc<Shutdown>) -> Self {
        let mut clusters = HashMap::new();

        for config in configs {
            let cluster = Arc::new(Cluster::new(config, shutdown.clone()).await);
            clusters.insert(config.name.clone(), cluster);
        }

        Self { clusters }
    }

    /// Start all health checks.
    pub fn start_health_checks(&self) {
        for cluster in self.clusters.values() {
            cluster.start_health_checks();
        }
    }

    /// Get a cluster by name.
    pub fn get(&self, name: &str) -> Option<Arc<Cluster>> {
        self.clusters.get(name).cloned()
    }

    /// Get all cluster names.
    pub fn names(&self) -> Vec<&str> {
        self.clusters.keys().map(|s| s.as_str()).collect()
    }

    /// Get cluster count.
    pub fn len(&self) -> usize {
        self.clusters.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.clusters.is_empty()
    }
}
