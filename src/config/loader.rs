use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use tracing::{debug, info};

use super::types::Config;

impl Config {
    /// Load configuration from a YAML file
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();

        debug!(path = %path.display(), "loading configuration");

        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read config file: {}", path.display()))?;

        Self::from_yaml(&contents)
            .with_context(|| format!("failed to parse config file: {}", path.display()))
    }

    /// Parse configuration from YAML string
    pub fn from_yaml(yaml: &str) -> Result<Self> {
        let config: Config = serde_yaml::from_str(yaml)
            .context("failed to parse YAML configuration")?;

        config.validate()?;

        Ok(config)
    }

    /// Validate configuration
    pub fn validate(&self) -> Result<()> {
        // Ensure at least one listener is defined
        if self.listeners.is_empty() {
            anyhow::bail!("at least one listener must be defined");
        }

        // Validate listener names are unique
        let mut listener_names = std::collections::HashSet::new();
        for listener in &self.listeners {
            if !listener_names.insert(&listener.name) {
                anyhow::bail!("duplicate listener name: {}", listener.name);
            }
        }

        // Validate cluster names are unique
        let mut cluster_names = std::collections::HashSet::new();
        for cluster in &self.clusters {
            if !cluster_names.insert(&cluster.name) {
                anyhow::bail!("duplicate cluster name: {}", cluster.name);
            }
        }

        // Validate routes reference existing clusters
        for route in &self.routes {
            if !cluster_names.contains(&route.cluster) {
                anyhow::bail!(
                    "route '{}' references unknown cluster: {}",
                    route.name,
                    route.cluster
                );
            }
        }

        // Validate each cluster has at least one endpoint (unless mock mode)
        for cluster in &self.clusters {
            if cluster.endpoints.is_empty() && cluster.mock.is_none() {
                anyhow::bail!(
                    "cluster '{}' must have at least one endpoint or mock configuration",
                    cluster.name
                );
            }
        }

        info!("configuration validated successfully");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_minimal_config() {
        let yaml = r#"
listeners:
  - name: default
    address: "0.0.0.0:2775"

clusters:
  - name: upstream
    endpoints:
      - address: "smsc.example.com:2775"
        system_id: user
        password: pass

routes:
  - name: default
    match_:
      prefix: ""
    cluster: upstream
"#;

        let config = Config::from_yaml(yaml).unwrap();
        assert_eq!(config.listeners.len(), 1);
        assert_eq!(config.clusters.len(), 1);
        assert_eq!(config.routes.len(), 1);
    }

    #[test]
    fn test_mock_cluster() {
        let yaml = r#"
listeners:
  - name: default
    address: "0.0.0.0:2775"

clusters:
  - name: mock
    endpoints: []
    mock:
      response: success
      latency: 100ms

routes:
  - name: default
    match_:
      prefix: ""
    cluster: mock
"#;

        let config = Config::from_yaml(yaml).unwrap();
        assert!(config.clusters[0].mock.is_some());
    }

    #[test]
    fn test_invalid_route_cluster() {
        let yaml = r#"
listeners:
  - name: default
    address: "0.0.0.0:2775"

clusters:
  - name: upstream
    endpoints:
      - address: "smsc.example.com:2775"
        system_id: user
        password: pass

routes:
  - name: default
    match_:
      prefix: ""
    cluster: nonexistent
"#;

        let result = Config::from_yaml(yaml);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown cluster"));
    }

    #[test]
    fn test_no_listeners() {
        let yaml = r#"
listeners: []
clusters: []
routes: []
"#;

        let result = Config::from_yaml(yaml);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("at least one listener"));
    }
}
