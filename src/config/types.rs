use serde::Deserialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

/// Root configuration for smppd
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Listeners accept incoming connections
    #[serde(default)]
    pub listeners: Vec<ListenerConfig>,

    /// Clusters define upstream SMSC groups
    #[serde(default)]
    pub clusters: Vec<ClusterConfig>,

    /// Routes define message routing rules
    #[serde(default)]
    pub routes: Vec<RouteConfig>,

    /// Admin API configuration
    #[serde(default)]
    pub admin: AdminConfig,

    /// Global settings
    #[serde(default)]
    pub settings: Settings,
}

/// Listener configuration
#[derive(Debug, Clone, Deserialize)]
pub struct ListenerConfig {
    /// Listener name (for logging/metrics)
    pub name: String,

    /// Bind address
    pub address: SocketAddr,

    /// Protocol type
    #[serde(default)]
    pub protocol: Protocol,

    /// TLS configuration
    pub tls: Option<TlsConfig>,

    /// Filter chain for this listener
    #[serde(default)]
    pub filters: Vec<FilterConfig>,

    /// Connection limits
    #[serde(default)]
    pub limits: ConnectionLimits,
}

/// Protocol type for listeners
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Smpp,
    Http,
    Grpc,
}

/// TLS configuration
#[derive(Debug, Clone, Deserialize)]
pub struct TlsConfig {
    /// Path to certificate file (PEM)
    pub cert: String,

    /// Path to private key file (PEM)
    pub key: String,

    /// Path to CA certificate for client auth
    pub ca: Option<String>,

    /// Require client certificate
    #[serde(default)]
    pub require_client_cert: bool,
}

/// Filter configuration
#[derive(Debug, Clone, Deserialize)]
pub struct FilterConfig {
    /// Filter type
    #[serde(rename = "type")]
    pub filter_type: FilterType,

    /// Filter-specific configuration
    #[serde(default)]
    pub config: HashMap<String, serde_yaml::Value>,
}

/// Available filter types
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FilterType {
    Auth,
    RateLimit,
    Firewall,
    Transform,
    Credit,
    Otp,
    Consent,
    Dlt,
    SenderId,
    Lua,
}

/// Connection limits
#[derive(Debug, Clone, Deserialize)]
pub struct ConnectionLimits {
    /// Maximum concurrent connections
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,

    /// Connection idle timeout
    #[serde(default = "default_idle_timeout", with = "humantime_serde")]
    pub idle_timeout: Duration,

    /// Request timeout
    #[serde(default = "default_request_timeout", with = "humantime_serde")]
    pub request_timeout: Duration,
}

impl Default for ConnectionLimits {
    fn default() -> Self {
        Self {
            max_connections: default_max_connections(),
            idle_timeout: default_idle_timeout(),
            request_timeout: default_request_timeout(),
        }
    }
}

fn default_max_connections() -> usize {
    10000
}

fn default_idle_timeout() -> Duration {
    Duration::from_secs(60)
}

fn default_request_timeout() -> Duration {
    Duration::from_secs(30)
}

/// Cluster configuration
#[derive(Debug, Clone, Deserialize)]
pub struct ClusterConfig {
    /// Cluster name
    pub name: String,

    /// Upstream endpoints
    pub endpoints: Vec<EndpointConfig>,

    /// Load balancer type
    #[serde(default)]
    pub load_balancer: LoadBalancer,

    /// Health check configuration
    #[serde(default)]
    pub health_check: HealthCheckConfig,

    /// Connection pool settings
    #[serde(default)]
    pub pool: PoolConfig,

    /// Mock mode - return configured responses
    #[serde(default)]
    pub mock: Option<MockConfig>,
}

/// Upstream endpoint configuration
#[derive(Debug, Clone, Deserialize)]
pub struct EndpointConfig {
    /// Endpoint address (host:port)
    pub address: String,

    /// SMPP system_id
    pub system_id: String,

    /// SMPP password
    pub password: String,

    /// System type
    #[serde(default)]
    pub system_type: String,

    /// Weight for weighted load balancing
    #[serde(default = "default_weight")]
    pub weight: u32,

    /// TLS configuration
    pub tls: Option<TlsConfig>,
}

fn default_weight() -> u32 {
    1
}

/// Load balancer type
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoadBalancer {
    #[default]
    RoundRobin,
    LeastConn,
    Weighted,
    Random,
}

/// Health check configuration
#[derive(Debug, Clone, Deserialize)]
pub struct HealthCheckConfig {
    /// Health check interval
    #[serde(default = "default_health_interval", with = "humantime_serde")]
    pub interval: Duration,

    /// Health check timeout
    #[serde(default = "default_health_timeout", with = "humantime_serde")]
    pub timeout: Duration,

    /// Unhealthy threshold
    #[serde(default = "default_unhealthy_threshold")]
    pub unhealthy_threshold: u32,

    /// Healthy threshold
    #[serde(default = "default_healthy_threshold")]
    pub healthy_threshold: u32,
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            interval: default_health_interval(),
            timeout: default_health_timeout(),
            unhealthy_threshold: default_unhealthy_threshold(),
            healthy_threshold: default_healthy_threshold(),
        }
    }
}

fn default_health_interval() -> Duration {
    Duration::from_secs(30)
}

fn default_health_timeout() -> Duration {
    Duration::from_secs(5)
}

fn default_unhealthy_threshold() -> u32 {
    3
}

fn default_healthy_threshold() -> u32 {
    2
}

/// Connection pool configuration
#[derive(Debug, Clone, Deserialize)]
pub struct PoolConfig {
    /// Minimum connections per endpoint
    #[serde(default = "default_min_connections")]
    pub min_connections: usize,

    /// Maximum connections per endpoint
    #[serde(default = "default_max_connections_pool")]
    pub max_connections: usize,

    /// Connection acquire timeout
    #[serde(default = "default_acquire_timeout", with = "humantime_serde")]
    pub acquire_timeout: Duration,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            min_connections: default_min_connections(),
            max_connections: default_max_connections_pool(),
            acquire_timeout: default_acquire_timeout(),
        }
    }
}

fn default_min_connections() -> usize {
    1
}

fn default_max_connections_pool() -> usize {
    10
}

fn default_acquire_timeout() -> Duration {
    Duration::from_secs(5)
}

/// Mock response configuration
#[derive(Debug, Clone, Deserialize)]
pub struct MockConfig {
    /// Response type
    #[serde(default)]
    pub response: MockResponse,

    /// Simulated latency
    #[serde(default, with = "humantime_serde")]
    pub latency: Duration,
}

/// Mock response type
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MockResponse {
    #[default]
    Success,
    Error { code: u32 },
    Random { error_rate: f32 },
}

/// Route configuration
#[derive(Debug, Clone, Deserialize)]
pub struct RouteConfig {
    /// Route name
    pub name: String,

    /// Match conditions
    pub match_: RouteMatch,

    /// Target cluster
    pub cluster: String,

    /// Route priority (lower = higher priority)
    #[serde(default)]
    pub priority: i32,
}

/// Route matching rules
#[derive(Debug, Clone, Deserialize)]
pub struct RouteMatch {
    /// Prefix match on destination
    pub prefix: Option<String>,

    /// Regex match on destination
    pub regex: Option<String>,

    /// Source address match
    pub source: Option<String>,

    /// Sender ID match
    pub sender_id: Option<String>,
}

/// Admin API configuration
#[derive(Debug, Clone, Deserialize)]
pub struct AdminConfig {
    /// HTTP API address
    #[serde(default = "default_admin_address")]
    pub address: SocketAddr,

    /// Enable metrics endpoint
    #[serde(default = "default_true")]
    pub metrics: bool,

    /// Enable health endpoint
    #[serde(default = "default_true")]
    pub health: bool,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            address: default_admin_address(),
            metrics: true,
            health: true,
        }
    }
}

fn default_admin_address() -> SocketAddr {
    "0.0.0.0:9090".parse().unwrap()
}

fn default_true() -> bool {
    true
}

/// Global settings
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Settings {
    /// Worker threads (0 = auto)
    #[serde(default)]
    pub workers: usize,

    /// Enable structured JSON logging
    #[serde(default)]
    pub json_logs: bool,

    /// Log level
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// OTLP endpoint for distributed tracing
    pub otlp_endpoint: Option<String>,

    /// Trace sample rate (0.0 - 1.0)
    #[serde(default = "default_sample_rate")]
    pub trace_sample_rate: f64,

    /// Shutdown configuration
    #[serde(default)]
    pub shutdown: ShutdownConfig,

    /// Enable hot reload
    #[serde(default = "default_true")]
    pub hot_reload: bool,
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_sample_rate() -> f64 {
    1.0
}

/// Shutdown configuration (Envoy-style)
#[derive(Debug, Clone, Deserialize)]
pub struct ShutdownConfig {
    /// Drain timeout - how long to wait for connections to drain
    #[serde(default = "default_drain_timeout", with = "humantime_serde")]
    pub drain_timeout: Duration,

    /// Parent shutdown timeout - max time for entire shutdown
    #[serde(default = "default_parent_shutdown_timeout", with = "humantime_serde")]
    pub parent_shutdown_timeout: Duration,
}

impl Default for ShutdownConfig {
    fn default() -> Self {
        Self {
            drain_timeout: default_drain_timeout(),
            parent_shutdown_timeout: default_parent_shutdown_timeout(),
        }
    }
}

fn default_drain_timeout() -> Duration {
    Duration::from_secs(30)
}

fn default_parent_shutdown_timeout() -> Duration {
    Duration::from_secs(60)
}

/// Humantime serde support module
mod humantime_serde {
    use serde::{self, Deserialize, Deserializer};
    use std::time::Duration;

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        humantime::parse_duration(&s).map_err(serde::de::Error::custom)
    }
}
