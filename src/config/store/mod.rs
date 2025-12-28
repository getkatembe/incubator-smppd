//! Configuration storage backends.
//!
//! This module provides a trait-based abstraction for configuration storage,
//! allowing different backends (file, memory, remote stores) to be used
//! interchangeably.
//!
//! # Built-in Implementations
//!
//! - [`FileConfigStore`] - File-based configuration (YAML, JSON, TOML)
//! - [`MemoryConfigStore`] - In-memory configuration (for testing)
//!
//! # Example
//!
//! ```ignore
//! use smppd::config::store::{ConfigStore, MemoryConfigStore};
//!
//! // Create a memory-based store for testing
//! let store = MemoryConfigStore::new(config);
//!
//! // Load configuration
//! let config = store.load().await?;
//!
//! // Watch for changes (if supported)
//! if let Some(mut rx) = store.watch() {
//!     tokio::spawn(async move {
//!         while rx.changed().await.is_ok() {
//!             let new_config = rx.borrow().clone();
//!             // Handle config update
//!         }
//!     });
//! }
//! ```

mod file;
mod memory;

pub use file::FileConfigStore;
pub use memory::MemoryConfigStore;

use async_trait::async_trait;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::watch;

use super::Config;

/// Configuration storage error.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Failed to load configuration.
    #[error("failed to load config: {0}")]
    LoadFailed(String),

    /// Failed to save configuration.
    #[error("failed to save config: {0}")]
    SaveFailed(String),

    /// Configuration validation failed.
    #[error("validation failed: {0}")]
    ValidationFailed(String),

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Serialization error.
    #[error("serialization error: {0}")]
    Serialization(String),

    /// Backend-specific error.
    #[error("backend error: {0}")]
    Backend(#[from] anyhow::Error),
}

/// Configuration storage backend trait.
///
/// Implement this trait to add custom configuration backends
/// (e.g., etcd, Consul, Redis, database).
#[async_trait]
pub trait ConfigStore: Send + Sync {
    /// Load configuration from storage.
    async fn load(&self) -> Result<Config, ConfigError>;

    /// Save configuration to storage.
    ///
    /// Not all backends support saving. Returns `Err(ConfigError::SaveFailed)`
    /// if the backend is read-only.
    async fn save(&self, config: &Config) -> Result<(), ConfigError>;

    /// Watch for configuration changes.
    ///
    /// Returns a receiver that emits new configurations when changes are detected.
    /// Returns `None` if the backend doesn't support watching.
    fn watch(&self) -> Option<watch::Receiver<Config>>;

    /// Get the backend name for logging and metrics.
    fn name(&self) -> &'static str;

    /// Check if this backend supports saving.
    fn supports_save(&self) -> bool {
        true
    }

    /// Check if this backend supports watching.
    fn supports_watch(&self) -> bool {
        self.watch().is_some()
    }
}

/// A boxed configuration store.
pub type BoxConfigStore = Arc<dyn ConfigStore>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_error_display() {
        let err = ConfigError::LoadFailed("file not found".to_string());
        assert!(err.to_string().contains("file not found"));

        let err = ConfigError::ValidationFailed("invalid config".to_string());
        assert!(err.to_string().contains("invalid config"));
    }
}
