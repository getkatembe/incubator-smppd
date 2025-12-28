use anyhow::{Context, Result};
use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use tracing::{debug, info};

use super::types::Config;

/// Supported configuration formats
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Yaml,
    Json,
    Toml,
}

impl Format {
    /// Detect format from file extension
    pub fn from_path(path: &Path) -> Option<Self> {
        path.extension()
            .and_then(OsStr::to_str)
            .and_then(|ext| match ext.to_lowercase().as_str() {
                "yaml" | "yml" => Some(Format::Yaml),
                "json" => Some(Format::Json),
                "toml" => Some(Format::Toml),
                _ => None,
            })
    }
}

impl Config {
    /// Load configuration from a file (auto-detect format)
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();

        debug!(path = %path.display(), "loading configuration");

        let format = Format::from_path(path)
            .with_context(|| format!("unsupported config format: {}", path.display()))?;

        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read config file: {}", path.display()))?;

        let config = match format {
            Format::Yaml => Self::from_yaml(&contents)?,
            Format::Json => Self::from_json(&contents)?,
            Format::Toml => Self::from_toml(&contents)?,
        };

        info!(
            path = %path.display(),
            format = ?format,
            listeners = config.listeners.len(),
            clusters = config.clusters.len(),
            "configuration loaded"
        );

        Ok(config)
    }

    /// Parse configuration from YAML string
    pub fn from_yaml(yaml: &str) -> Result<Self> {
        let config: Config = serde_yaml::from_str(yaml)
            .context("failed to parse YAML configuration")?;

        config.validate()?;
        Ok(config)
    }

    /// Parse configuration from JSON string
    pub fn from_json(json: &str) -> Result<Self> {
        let config: Config = serde_json::from_str(json)
            .context("failed to parse JSON configuration")?;

        config.validate()?;
        Ok(config)
    }

    /// Parse configuration from TOML string
    pub fn from_toml(toml_str: &str) -> Result<Self> {
        let config: Config = toml::from_str(toml_str)
            .context("failed to parse TOML configuration")?;

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
            // Check single cluster target
            if let Some(ref cluster) = route.cluster {
                if !cluster_names.contains(cluster) {
                    anyhow::bail!(
                        "route '{}' references unknown cluster: {}",
                        route.name,
                        cluster
                    );
                }
            }

            // Check traffic split targets
            if let Some(ref splits) = route.split {
                for split in splits {
                    if !cluster_names.contains(&split.cluster) {
                        anyhow::bail!(
                            "route '{}' references unknown cluster in split: {}",
                            route.name,
                            split.cluster
                        );
                    }
                }
            }

            // Check fallback clusters
            if let Some(ref fallbacks) = route.fallback {
                for fallback in fallbacks {
                    if !cluster_names.contains(fallback) {
                        anyhow::bail!(
                            "route '{}' references unknown fallback cluster: {}",
                            route.name,
                            fallback
                        );
                    }
                }
            }

            // Validate route has either cluster or split (but not both)
            if route.cluster.is_none() && route.split.is_none() {
                anyhow::bail!(
                    "route '{}' must have either 'cluster' or 'split' target",
                    route.name
                );
            }
            if route.cluster.is_some() && route.split.is_some() {
                anyhow::bail!(
                    "route '{}' cannot have both 'cluster' and 'split' targets",
                    route.name
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

        debug!("configuration validated");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{FilterType, LoadBalancer, Protocol};

    const YAML_CONFIG: &str = r#"
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
    match:
      prefix: ""
    cluster: upstream
"#;

    const JSON_CONFIG: &str = r#"{
  "listeners": [{"name": "default", "address": "0.0.0.0:2775"}],
  "clusters": [{"name": "upstream", "endpoints": [{"address": "smsc:2775", "system_id": "user", "password": "pass"}]}],
  "routes": [{"name": "default", "match": {"prefix": ""}, "cluster": "upstream"}]
}"#;

    const TOML_CONFIG: &str = r#"
[[listeners]]
name = "default"
address = "0.0.0.0:2775"

[[clusters]]
name = "upstream"

[[clusters.endpoints]]
address = "smsc:2775"
system_id = "user"
password = "pass"

[[routes]]
name = "default"
cluster = "upstream"

[routes.match]
prefix = ""
"#;

    // ============================================================================
    // Format Detection Tests
    // ============================================================================

    #[test]
    fn test_format_detection_yaml() {
        assert_eq!(Format::from_path(Path::new("config.yaml")), Some(Format::Yaml));
        assert_eq!(Format::from_path(Path::new("config.yml")), Some(Format::Yaml));
        assert_eq!(Format::from_path(Path::new("/etc/smppd/config.yaml")), Some(Format::Yaml));
        assert_eq!(Format::from_path(Path::new("CONFIG.YAML")), Some(Format::Yaml));
    }

    #[test]
    fn test_format_detection_json() {
        assert_eq!(Format::from_path(Path::new("config.json")), Some(Format::Json));
        assert_eq!(Format::from_path(Path::new("/etc/smppd/config.json")), Some(Format::Json));
    }

    #[test]
    fn test_format_detection_toml() {
        assert_eq!(Format::from_path(Path::new("config.toml")), Some(Format::Toml));
        assert_eq!(Format::from_path(Path::new("/etc/smppd/config.toml")), Some(Format::Toml));
    }

    #[test]
    fn test_format_detection_unknown() {
        assert_eq!(Format::from_path(Path::new("config.txt")), None);
        assert_eq!(Format::from_path(Path::new("config.xml")), None);
        assert_eq!(Format::from_path(Path::new("config")), None);
    }

    // ============================================================================
    // Config Parsing Tests
    // ============================================================================

    #[test]
    fn test_yaml_config() {
        let config = Config::from_yaml(YAML_CONFIG).unwrap();
        assert_eq!(config.listeners.len(), 1);
        assert_eq!(config.clusters.len(), 1);
        assert_eq!(config.routes.len(), 1);
    }

    #[test]
    fn test_json_config() {
        let config = Config::from_json(JSON_CONFIG).unwrap();
        assert_eq!(config.listeners.len(), 1);
        assert_eq!(config.clusters.len(), 1);
    }

    #[test]
    fn test_toml_config() {
        let config = Config::from_toml(TOML_CONFIG).unwrap();
        assert_eq!(config.listeners.len(), 1);
        assert_eq!(config.clusters.len(), 1);
    }

    #[test]
    fn test_yaml_full_listener_config() {
        let yaml = r#"
listeners:
  - name: main
    address: "0.0.0.0:2775"
    protocol: smpp
    limits:
      max_connections: 5000
      idle_timeout: 120s
      request_timeout: 60s
    filters:
      - type: auth
        config:
          required: true
      - type: rate_limit
        config:
          requests_per_second: 100

clusters:
  - name: upstream
    endpoints: []
    mock:
      response: success

routes:
  - name: default
    match:
      prefix: ""
    cluster: upstream
"#;

        let config = Config::from_yaml(yaml).unwrap();
        let listener = &config.listeners[0];

        assert_eq!(listener.name, "main");
        assert_eq!(listener.protocol, Protocol::Smpp);
        assert_eq!(listener.limits.max_connections, 5000);
        assert_eq!(listener.limits.idle_timeout, std::time::Duration::from_secs(120));
        assert_eq!(listener.filters.len(), 2);
        assert_eq!(listener.filters[0].filter_type, FilterType::Auth);
        assert_eq!(listener.filters[1].filter_type, FilterType::RateLimit);
    }

    #[test]
    fn test_yaml_multiple_listeners() {
        let yaml = r#"
listeners:
  - name: plain
    address: "0.0.0.0:2775"
    protocol: smpp

  - name: secure
    address: "0.0.0.0:2776"
    protocol: smpp
    tls:
      cert: /etc/smppd/cert.pem
      key: /etc/smppd/key.pem

  - name: http
    address: "0.0.0.0:8080"
    protocol: http

clusters:
  - name: upstream
    endpoints: []
    mock:
      response: success

routes:
  - name: default
    match:
      prefix: ""
    cluster: upstream
"#;

        let config = Config::from_yaml(yaml).unwrap();
        assert_eq!(config.listeners.len(), 3);
        assert!(config.listeners[0].tls.is_none());
        assert!(config.listeners[1].tls.is_some());
        assert_eq!(config.listeners[2].protocol, Protocol::Http);
    }

    #[test]
    fn test_cluster_config_load_balancers() {
        let yaml = r#"
listeners:
  - name: default
    address: "0.0.0.0:2775"

clusters:
  - name: rr
    endpoints:
      - address: "smsc:2775"
        system_id: test
        password: test
    load_balancer: round_robin

  - name: lc
    endpoints:
      - address: "smsc:2775"
        system_id: test
        password: test
    load_balancer: least_conn

  - name: weighted
    endpoints:
      - address: "smsc:2775"
        system_id: test
        password: test
        weight: 3
    load_balancer: weighted

routes:
  - name: default
    match:
      prefix: ""
    cluster: rr
"#;

        let config = Config::from_yaml(yaml).unwrap();
        assert_eq!(config.clusters[0].load_balancer, LoadBalancer::RoundRobin);
        assert_eq!(config.clusters[1].load_balancer, LoadBalancer::LeastConn);
        assert_eq!(config.clusters[2].load_balancer, LoadBalancer::Weighted);
        assert_eq!(config.clusters[2].endpoints[0].weight, 3);
    }

    #[test]
    fn test_cluster_mock_config() {
        let yaml = r#"
listeners:
  - name: default
    address: "0.0.0.0:2775"

clusters:
  - name: mock-cluster
    endpoints: []
    mock:
      response: success
      latency: 50ms

routes:
  - name: default
    match:
      prefix: ""
    cluster: mock-cluster
"#;

        let config = Config::from_yaml(yaml).unwrap();
        assert!(config.clusters[0].mock.is_some());
        let mock = config.clusters[0].mock.as_ref().unwrap();
        assert_eq!(mock.latency, std::time::Duration::from_millis(50));
    }

    #[test]
    fn test_route_config_parsing() {
        let yaml = r#"
listeners:
  - name: default
    address: "0.0.0.0:2775"

clusters:
  - name: vodacom
    endpoints: []
    mock:
      response: success

routes:
  - name: vodacom-route
    match:
      prefix: "+25884"
    cluster: vodacom
    priority: 1

  - name: default
    match:
      prefix: ""
    cluster: vodacom
    priority: 100
"#;

        let config = Config::from_yaml(yaml).unwrap();
        assert_eq!(config.routes.len(), 2);
        assert_eq!(config.routes[0].name, "vodacom-route");
        assert_eq!(config.routes[0].match_.prefix, Some("+25884".to_string()));
        assert_eq!(config.routes[0].priority, 1);
    }

    #[test]
    fn test_route_config_with_regex() {
        let yaml = r#"
listeners:
  - name: default
    address: "0.0.0.0:2775"

clusters:
  - name: mobile
    endpoints: []
    mock:
      response: success

routes:
  - name: moz-mobile
    match:
      regex: "^\\+258(84|82|86|87)"
    cluster: mobile
    priority: 1
"#;

        let config = Config::from_yaml(yaml).unwrap();
        assert_eq!(config.routes[0].match_.regex, Some("^\\+258(84|82|86|87)".to_string()));
    }

    #[test]
    fn test_admin_and_telemetry_config() {
        let yaml = r#"
listeners:
  - name: default
    address: "0.0.0.0:2775"

clusters:
  - name: upstream
    endpoints: []
    mock:
      response: success

routes:
  - name: default
    match:
      prefix: ""
    cluster: upstream

admin:
  address: "127.0.0.1:9191"
  metrics: false
  health: true

telemetry:
  workers: 4
  json_logs: true
  log_level: debug
  otlp_endpoint: "http://localhost:4317"
  trace_sample_rate: 0.5
"#;

        let config = Config::from_yaml(yaml).unwrap();
        assert_eq!(config.admin.address.port(), 9191);
        assert!(!config.admin.metrics);
        assert!(config.admin.health);
        assert_eq!(config.telemetry.workers, 4);
        assert!(config.telemetry.json_logs);
        assert_eq!(config.telemetry.log_level, "debug");
        assert_eq!(config.telemetry.trace_sample_rate, 0.5);
    }

    // ============================================================================
    // Validation Tests
    // ============================================================================

    #[test]
    fn test_validation_requires_listener() {
        let yaml = r#"
listeners: []

clusters:
  - name: upstream
    endpoints: []
    mock:
      response: success
"#;

        let result = Config::from_yaml(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("listener"));
    }

    #[test]
    fn test_validation_duplicate_listener_names() {
        let yaml = r#"
listeners:
  - name: duplicate
    address: "0.0.0.0:2775"
  - name: duplicate
    address: "0.0.0.0:2776"

clusters:
  - name: upstream
    endpoints: []
    mock:
      response: success

routes:
  - name: default
    match:
      prefix: ""
    cluster: upstream
"#;

        let result = Config::from_yaml(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn test_validation_duplicate_cluster_names() {
        let yaml = r#"
listeners:
  - name: default
    address: "0.0.0.0:2775"

clusters:
  - name: duplicate
    endpoints: []
    mock:
      response: success
  - name: duplicate
    endpoints: []
    mock:
      response: success

routes:
  - name: default
    match:
      prefix: ""
    cluster: duplicate
"#;

        let result = Config::from_yaml(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn test_validation_route_references_unknown_cluster() {
        let yaml = r#"
listeners:
  - name: default
    address: "0.0.0.0:2775"

clusters:
  - name: existing
    endpoints: []
    mock:
      response: success

routes:
  - name: bad-route
    match:
      prefix: ""
    cluster: nonexistent
"#;

        let result = Config::from_yaml(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn test_validation_cluster_needs_endpoints_or_mock() {
        let yaml = r#"
listeners:
  - name: default
    address: "0.0.0.0:2775"

clusters:
  - name: empty
    endpoints: []

routes:
  - name: default
    match:
      prefix: ""
    cluster: empty
"#;

        let result = Config::from_yaml(yaml);
        assert!(result.is_err());
    }

    #[test]
    fn test_json_config_with_all_fields() {
        let json = r#"{
            "listeners": [
                {
                    "name": "main",
                    "address": "0.0.0.0:2775",
                    "protocol": "smpp",
                    "limits": {
                        "max_connections": 1000,
                        "idle_timeout": "60s",
                        "request_timeout": "30s"
                    }
                }
            ],
            "clusters": [
                {
                    "name": "upstream",
                    "endpoints": [],
                    "load_balancer": "round_robin",
                    "mock": {"response": "success"}
                }
            ],
            "routes": [
                {"name": "default", "match": {"prefix": ""}, "cluster": "upstream"}
            ],
            "admin": {
                "address": "0.0.0.0:9090",
                "metrics": true,
                "health": true
            }
        }"#;

        let config = Config::from_json(json).unwrap();
        assert_eq!(config.listeners[0].limits.max_connections, 1000);
        assert_eq!(config.clusters[0].load_balancer, LoadBalancer::RoundRobin);
        assert!(config.admin.metrics);
    }
}
