//! Shared gateway state.
//!
//! Contains all the core components shared across the gateway:
//! - Storage (messages + feedback)
//! - Router (message routing)
//! - Clusters (upstream connections)
//! - Config (settings)

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;

use crate::cluster::Clusters;
use crate::config::Config;
use crate::router::Router;
use crate::store::{create_storage, SharedStorage};

use super::Shutdown;

/// Shared gateway state.
///
/// Passed to all components that need access to gateway-wide resources.
/// All fields are thread-safe and can be cloned cheaply.
#[derive(Clone)]
pub struct GatewayState {
    /// Unified storage (messages + feedback)
    pub storage: SharedStorage,
    /// Router (message routing decisions)
    pub router: Arc<RwLock<Router>>,
    /// Clusters (upstream SMSC connections)
    pub clusters: Arc<Clusters>,
    /// Configuration
    pub config: Arc<Config>,
}

impl GatewayState {
    /// Create a new gateway state from configuration.
    pub async fn new(config: Arc<Config>, shutdown: Arc<Shutdown>) -> anyhow::Result<Self> {
        let clusters = Arc::new(Clusters::new(&config.clusters, shutdown).await);
        let router = Router::new(&config.routes, clusters.clone())?;
        let storage = create_storage(&config.settings.store).await?;

        Ok(Self {
            storage,
            router: Arc::new(RwLock::new(router)),
            clusters,
            config,
        })
    }

    /// Reload configuration.
    pub async fn reload(
        &self,
        new_config: Arc<Config>,
        shutdown: Arc<Shutdown>,
    ) -> anyhow::Result<()> {
        let new_clusters = Arc::new(Clusters::new(&new_config.clusters, shutdown).await);
        let new_router = Router::new(&new_config.routes, new_clusters.clone())?;

        {
            let mut router = self.router.write().await;
            *router = new_router;
        }

        Ok(())
    }

    /// Get store statistics.
    pub fn store_stats(&self) -> crate::store::StoreStats {
        self.storage.message_stats()
    }

    /// Get feedback statistics for a target.
    pub fn target_stats(&self, target: &str) -> crate::feedback::TargetStats {
        self.storage.target_stats(target)
    }

    /// Run periodic maintenance tasks.
    pub fn maintenance(&self, message_ttl: Duration, prune_age: Duration) {
        self.storage.maintenance(message_ttl, prune_age);
    }
}

/// Shared gateway state handle.
pub type SharedGatewayState = Arc<GatewayState>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AdminConfig, ClusterConfig, ListenerConfig, MockConfig, Protocol, RouteConfig, RouteMatch,
        Settings, StoreConfig, TelemetryConfig,
    };

    fn test_config() -> Config {
        Config {
            listeners: vec![ListenerConfig {
                name: "default".to_string(),
                address: "0.0.0.0:2775".parse().unwrap(),
                protocol: Protocol::Smpp,
                tls: None,
                filters: vec![],
                limits: Default::default(),
            }],
            clusters: vec![ClusterConfig {
                name: "mock".to_string(),
                endpoints: vec![],
                load_balancer: Default::default(),
                health_check: Default::default(),
                pool: Default::default(),
                mock: Some(MockConfig {
                    response: Default::default(),
                    latency: Default::default(),
                }),
            }],
            routes: vec![RouteConfig {
                name: "default".to_string(),
                match_: RouteMatch {
                    destination: None,
                    source: None,
                    sender_id: None,
                    system_id: None,
                    service_type: None,
                    esm_class: None,
                    data_coding: None,
                    time_window: None,
                    prefix: Some("".to_string()),
                    regex: None,
                },
                cluster: Some("mock".to_string()),
                split: None,
                fallback: None,
                priority: 0,
            }],
            admin: AdminConfig::default(),
            settings: Settings {
                store: StoreConfig::memory(),
                ..Default::default()
            },
            telemetry: TelemetryConfig::default(),
            clients: vec![],
        }
    }

    fn test_shutdown() -> Arc<Shutdown> {
        Shutdown::new(std::time::Duration::from_secs(30))
    }

    #[tokio::test]
    async fn test_gateway_state_creation() {
        let config = Arc::new(test_config());
        let shutdown = test_shutdown();
        let state = GatewayState::new(config, shutdown).await.unwrap();

        let stats = state.store_stats();
        assert_eq!(stats.total, 0);
    }

    #[tokio::test]
    async fn test_gateway_state_store_message() {
        use crate::store::StoredMessage;

        let config = Arc::new(test_config());
        let shutdown = test_shutdown();
        let state = GatewayState::new(config, shutdown).await.unwrap();

        let msg = StoredMessage::new("SENDER", "+258841234567", b"Hello".to_vec(), "client1", 1);
        let id = state.storage.store(msg);

        let retrieved = state.storage.get(id).unwrap();
        assert_eq!(retrieved.destination, "+258841234567");

        let stats = state.store_stats();
        assert_eq!(stats.total, 1);
        assert_eq!(stats.pending, 1);
    }

    #[tokio::test]
    async fn test_gateway_state_feedback() {
        use crate::feedback::{Decision, DecisionContext, Outcome};
        use std::time::Duration;

        let config = Arc::new(test_config());
        let shutdown = test_shutdown();
        let state = GatewayState::new(config, shutdown).await.unwrap();

        let ctx = DecisionContext::new("+258841234567");
        let decision = Decision::route("mock", vec![], ctx);
        let id = state.storage.record_decision(decision);

        state
            .storage
            .record_outcome(Outcome::success(id, Duration::from_millis(50)));

        let stats = state.target_stats("mock");
        assert_eq!(stats.total, 1);
        assert_eq!(stats.successes, 1);
    }
}
