//! File-based configuration store.
//!
//! Supports YAML, JSON, and TOML configuration files.

use async_trait::async_trait;
use std::path::PathBuf;
use tokio::sync::watch;
use tracing::{debug, info};

use super::{ConfigError, ConfigStore};
use crate::config::{loader::Format, Config};

/// File-based configuration store.
///
/// Loads and saves configuration from/to files in YAML, JSON, or TOML format.
///
/// # Example
///
/// ```ignore
/// use smppd::config::store::FileConfigStore;
///
/// let store = FileConfigStore::new("/etc/smppd/config.yaml")?;
/// let config = store.load().await?;
/// ```
pub struct FileConfigStore {
    path: PathBuf,
    format: Format,
}

impl FileConfigStore {
    /// Create a new file config store.
    ///
    /// The format is auto-detected from the file extension.
    pub fn new<P: Into<PathBuf>>(path: P) -> Result<Self, ConfigError> {
        let path = path.into();
        let format = Format::from_path(&path)
            .ok_or_else(|| ConfigError::LoadFailed(format!(
                "unsupported config format: {}",
                path.display()
            )))?;

        debug!(path = %path.display(), format = ?format, "created file config store");

        Ok(Self { path, format })
    }

    /// Create a file config store with explicit format.
    pub fn with_format<P: Into<PathBuf>>(path: P, format: Format) -> Self {
        Self {
            path: path.into(),
            format,
        }
    }

    /// Get the configuration file path.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Get the configuration format.
    pub fn format(&self) -> Format {
        self.format
    }

    /// Serialize configuration to string.
    fn serialize(&self, config: &Config) -> Result<String, ConfigError> {
        match self.format {
            Format::Yaml => serde_yaml::to_string(config)
                .map_err(|e| ConfigError::Serialization(e.to_string())),
            Format::Json => serde_json::to_string_pretty(config)
                .map_err(|e| ConfigError::Serialization(e.to_string())),
            Format::Toml => toml::to_string_pretty(config)
                .map_err(|e| ConfigError::Serialization(e.to_string())),
        }
    }
}

#[async_trait]
impl ConfigStore for FileConfigStore {
    async fn load(&self) -> Result<Config, ConfigError> {
        info!(path = %self.path.display(), "loading configuration from file");

        let config = Config::load(&self.path)
            .map_err(|e| ConfigError::LoadFailed(e.to_string()))?;

        info!(
            path = %self.path.display(),
            listeners = config.listeners.len(),
            clusters = config.clusters.len(),
            routes = config.routes.len(),
            "configuration loaded"
        );

        Ok(config)
    }

    async fn save(&self, config: &Config) -> Result<(), ConfigError> {
        info!(path = %self.path.display(), "saving configuration to file");

        // Validate before saving
        config.validate().map_err(|e| ConfigError::ValidationFailed(e.to_string()))?;

        // Serialize
        let contents = self.serialize(config)?;

        // Write atomically (write to temp file, then rename)
        let temp_path = self.path.with_extension("tmp");
        std::fs::write(&temp_path, &contents)?;
        std::fs::rename(&temp_path, &self.path)?;

        info!(path = %self.path.display(), "configuration saved");

        Ok(())
    }

    fn watch(&self) -> Option<watch::Receiver<Config>> {
        // File watching can be implemented with notify crate
        // For now, return None (not supported)
        None
    }

    fn name(&self) -> &'static str {
        "file"
    }

    fn supports_watch(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn minimal_yaml() -> &'static str {
        r#"
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
"#
    }

    fn minimal_json() -> &'static str {
        r#"{
    "listeners": [{"name": "test", "address": "0.0.0.0:2775"}],
    "clusters": [{"name": "mock", "endpoints": [], "mock": {"response": "success"}}],
    "routes": [{"name": "default", "match": {"prefix": ""}, "cluster": "mock"}]
}"#
    }

    #[tokio::test]
    async fn test_file_store_load_yaml() {
        let mut file = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(file, "{}", minimal_yaml()).unwrap();

        let store = FileConfigStore::new(file.path()).unwrap();
        let config = store.load().await.unwrap();

        assert_eq!(config.listeners[0].name, "test");
        assert_eq!(store.format(), Format::Yaml);
    }

    #[tokio::test]
    async fn test_file_store_load_json() {
        let mut file = NamedTempFile::with_suffix(".json").unwrap();
        write!(file, "{}", minimal_json()).unwrap();

        let store = FileConfigStore::new(file.path()).unwrap();
        let config = store.load().await.unwrap();

        assert_eq!(config.listeners[0].name, "test");
        assert_eq!(store.format(), Format::Json);
    }

    #[tokio::test]
    async fn test_file_store_save() {
        let file = NamedTempFile::with_suffix(".yaml").unwrap();
        let path = file.path().to_path_buf();
        drop(file); // Close the file so we can write to it

        // Write initial config
        std::fs::write(&path, minimal_yaml()).unwrap();

        let store = FileConfigStore::new(&path).unwrap();
        let mut config = store.load().await.unwrap();

        // Modify and save
        config.listeners[0].name = "saved".to_string();
        store.save(&config).await.unwrap();

        // Reload and verify
        let reloaded = store.load().await.unwrap();
        assert_eq!(reloaded.listeners[0].name, "saved");

        // Cleanup
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_file_store_unsupported_format() {
        let result = FileConfigStore::new("/etc/smppd/config.txt");
        assert!(result.is_err());
    }

    #[test]
    fn test_file_store_name() {
        let file = NamedTempFile::with_suffix(".yaml").unwrap();
        let store = FileConfigStore::new(file.path()).unwrap();
        assert_eq!(store.name(), "file");
    }

    #[test]
    fn test_file_store_no_watch() {
        let file = NamedTempFile::with_suffix(".yaml").unwrap();
        let store = FileConfigStore::new(file.path()).unwrap();
        assert!(store.watch().is_none());
        assert!(!store.supports_watch());
    }

    #[test]
    fn test_file_store_path() {
        let file = NamedTempFile::with_suffix(".yaml").unwrap();
        let expected_path = file.path().to_path_buf();
        let store = FileConfigStore::new(file.path()).unwrap();
        assert_eq!(store.path(), &expected_path);
    }

    #[test]
    fn test_file_store_with_explicit_format() {
        let store = FileConfigStore::with_format("/some/path/config", Format::Yaml);
        assert_eq!(store.format(), Format::Yaml);
    }
}
