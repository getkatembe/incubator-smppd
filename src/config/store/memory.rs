//! In-memory configuration store.
//!
//! Useful for testing and programmatic configuration.

use async_trait::async_trait;
use std::sync::RwLock;
use tokio::sync::watch;

use super::{ConfigError, ConfigStore};
use crate::config::Config;

/// In-memory configuration store.
///
/// Stores configuration in memory and supports:
/// - Loading and saving
/// - Watching for changes
/// - Programmatic updates
///
/// # Example
///
/// ```ignore
/// use smppd::config::store::MemoryConfigStore;
///
/// let store = MemoryConfigStore::new(config);
///
/// // Update config programmatically
/// store.update(new_config)?;
///
/// // Watch for changes
/// let mut rx = store.watch().unwrap();
/// rx.changed().await?;
/// let current = rx.borrow().clone();
/// ```
pub struct MemoryConfigStore {
    config: RwLock<Config>,
    watch_tx: watch::Sender<Config>,
    watch_rx: watch::Receiver<Config>,
}

impl MemoryConfigStore {
    /// Create a new memory config store with initial configuration.
    pub fn new(config: Config) -> Self {
        let (watch_tx, watch_rx) = watch::channel(config.clone());
        Self {
            config: RwLock::new(config),
            watch_tx,
            watch_rx,
        }
    }

    /// Update the configuration.
    ///
    /// Validates the new configuration before applying.
    /// Notifies all watchers of the change.
    pub fn update(&self, config: Config) -> Result<(), ConfigError> {
        // Validate the new config
        config.validate().map_err(|e| ConfigError::ValidationFailed(e.to_string()))?;

        // Update the stored config
        {
            let mut guard = self.config.write().unwrap();
            *guard = config.clone();
        }

        // Notify watchers
        let _ = self.watch_tx.send(config);

        Ok(())
    }

    /// Get the current configuration.
    pub fn current(&self) -> Config {
        self.config.read().unwrap().clone()
    }

    /// Create a memory store from YAML string.
    pub fn from_yaml(yaml: &str) -> Result<Self, ConfigError> {
        let config = Config::from_yaml(yaml).map_err(|e| ConfigError::LoadFailed(e.to_string()))?;
        Ok(Self::new(config))
    }

    /// Create a memory store from JSON string.
    pub fn from_json(json: &str) -> Result<Self, ConfigError> {
        let config = Config::from_json(json).map_err(|e| ConfigError::LoadFailed(e.to_string()))?;
        Ok(Self::new(config))
    }
}

#[async_trait]
impl ConfigStore for MemoryConfigStore {
    async fn load(&self) -> Result<Config, ConfigError> {
        Ok(self.current())
    }

    async fn save(&self, config: &Config) -> Result<(), ConfigError> {
        self.update(config.clone())
    }

    fn watch(&self) -> Option<watch::Receiver<Config>> {
        Some(self.watch_rx.clone())
    }

    fn name(&self) -> &'static str {
        "memory"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AdminConfig, Settings, TelemetryConfig};

    fn minimal_config() -> Config {
        Config {
            listeners: vec![crate::config::ListenerConfig {
                name: "default".to_string(),
                address: "0.0.0.0:2775".parse().unwrap(),
                protocol: crate::config::Protocol::Smpp,
                tls: None,
                filters: vec![],
                limits: Default::default(),
            }],
            clusters: vec![crate::config::ClusterConfig {
                name: "mock".to_string(),
                endpoints: vec![],
                load_balancer: Default::default(),
                health_check: Default::default(),
                pool: Default::default(),
                mock: Some(crate::config::MockConfig {
                    response: Default::default(),
                    latency: Default::default(),
                }),
            }],
            routes: vec![crate::config::RouteConfig {
                name: "default".to_string(),
                match_: crate::config::RouteMatch {
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
            settings: Settings::default(),
            telemetry: TelemetryConfig::default(),
        }
    }

    #[tokio::test]
    async fn test_memory_store_load() {
        let config = minimal_config();
        let store = MemoryConfigStore::new(config.clone());

        let loaded = store.load().await.unwrap();
        assert_eq!(loaded.listeners.len(), 1);
        assert_eq!(loaded.listeners[0].name, "default");
    }

    #[tokio::test]
    async fn test_memory_store_save() {
        let config = minimal_config();
        let store = MemoryConfigStore::new(config);

        // Create a modified config
        let mut new_config = store.load().await.unwrap();
        new_config.listeners[0].name = "updated".to_string();

        store.save(&new_config).await.unwrap();

        let loaded = store.load().await.unwrap();
        assert_eq!(loaded.listeners[0].name, "updated");
    }

    #[tokio::test]
    async fn test_memory_store_watch() {
        let config = minimal_config();
        let store = MemoryConfigStore::new(config);

        let mut rx = store.watch().unwrap();

        // Initial value
        let initial = rx.borrow().clone();
        assert_eq!(initial.listeners[0].name, "default");

        // Update config
        let mut new_config = store.load().await.unwrap();
        new_config.listeners[0].name = "watched".to_string();
        store.save(&new_config).await.unwrap();

        // Check the watch received the update
        rx.changed().await.unwrap();
        let updated = rx.borrow().clone();
        assert_eq!(updated.listeners[0].name, "watched");
    }

    #[test]
    fn test_memory_store_name() {
        let config = minimal_config();
        let store = MemoryConfigStore::new(config);
        assert_eq!(store.name(), "memory");
    }

    #[test]
    fn test_memory_store_current() {
        let config = minimal_config();
        let store = MemoryConfigStore::new(config);

        let current = store.current();
        assert_eq!(current.listeners[0].name, "default");
    }

    #[test]
    fn test_memory_store_from_yaml() {
        let yaml = r#"
listeners:
  - name: test
    address: "0.0.0.0:2775"

clusters:
  - name: mock
    endpoints: []
    mock:
      response: success

routes:
  - name: default
    match:
      prefix: ""
    cluster: mock
"#;

        let store = MemoryConfigStore::from_yaml(yaml).unwrap();
        let config = store.current();
        assert_eq!(config.listeners[0].name, "test");
    }

    #[test]
    fn test_memory_store_validation() {
        let config = minimal_config();
        let store = MemoryConfigStore::new(config);

        // Try to save invalid config (no listeners)
        let invalid = Config {
            listeners: vec![],
            clusters: vec![],
            routes: vec![],
            admin: AdminConfig::default(),
            settings: Settings::default(),
            telemetry: TelemetryConfig::default(),
        };

        let result = store.update(invalid);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConfigError::ValidationFailed(_)));
    }
}
