use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

/// Root configuration for smppd
#[derive(Debug, Clone, Serialize, Deserialize)]
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

    /// Client credentials for ESME authentication
    #[serde(default)]
    pub clients: Vec<ClientConfig>,

    /// Admin API configuration
    #[serde(default)]
    pub admin: AdminConfig,

    /// Global settings
    #[serde(default)]
    pub settings: Settings,

    /// Telemetry configuration (workers, metrics, tracing)
    #[serde(default)]
    pub telemetry: TelemetryConfig,
}

/// Client (ESME) configuration for authentication
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    /// System ID (username)
    pub system_id: String,

    /// Password
    pub password: String,

    /// Optional system type
    #[serde(default)]
    pub system_type: Option<String>,

    /// Allowed bind types (transmitter, receiver, transceiver)
    #[serde(default)]
    pub allowed_bind_types: Vec<BindType>,

    /// Allowed source addresses/sender IDs
    #[serde(default)]
    pub allowed_sources: Vec<String>,

    /// Allowed IP addresses/ranges (CIDR notation)
    #[serde(default)]
    pub allowed_ips: Vec<String>,

    /// Maximum concurrent connections
    #[serde(default)]
    pub max_connections: Option<usize>,

    /// Rate limit (messages per second)
    #[serde(default)]
    pub rate_limit: Option<u32>,

    /// Whether this client is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Bind type for SMPP connections
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindType {
    Transmitter,
    Receiver,
    Transceiver,
}

fn default_true() -> bool {
    true
}

/// Listener configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Smpp,
    Http,
    Grpc,
}

/// TLS configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterConfig {
    /// Filter type
    #[serde(rename = "type")]
    pub filter_type: FilterType,

    /// Filter-specific configuration
    #[serde(default)]
    pub config: HashMap<String, serde_yaml::Value>,
}

/// Available filter types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoadBalancer {
    #[default]
    RoundRobin,
    LeastConn,
    Weighted,
    Random,
    /// Power of Two Choices - picks two random endpoints, chooses the one with fewer connections
    P2c,
    /// Consistent hashing - session affinity by destination/source
    ConsistentHash,
    /// Weighted least connections - combines weight and connection count
    WeightedLeastConn,
}

/// Health check configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockConfig {
    /// Response type
    #[serde(default)]
    pub response: MockResponse,

    /// Simulated latency
    #[serde(default, with = "humantime_serde")]
    pub latency: Duration,
}

/// Mock response type
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MockResponse {
    #[default]
    Success,
    Error { code: u32 },
    Random { error_rate: f32 },
}

/// Route configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteConfig {
    /// Route name
    pub name: String,

    /// Match conditions
    #[serde(rename = "match")]
    pub match_: RouteMatch,

    /// Target cluster (mutually exclusive with split)
    #[serde(default)]
    pub cluster: Option<String>,

    /// Traffic split configuration (mutually exclusive with cluster)
    #[serde(default)]
    pub split: Option<Vec<TrafficSplit>>,

    /// Fallback clusters (tried in order if primary fails)
    #[serde(default)]
    pub fallback: Option<Vec<String>>,

    /// Route priority (lower = higher priority)
    #[serde(default)]
    pub priority: i32,
}

impl RouteConfig {
    /// Get the primary cluster name for backward compatibility
    pub fn primary_cluster(&self) -> Option<&str> {
        self.cluster.as_deref()
    }
}

/// Traffic split for A/B testing or gradual rollouts
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrafficSplit {
    /// Target cluster
    pub cluster: String,

    /// Weight for this cluster (relative to total)
    #[serde(default = "default_weight")]
    pub weight: u32,
}

/// Route matching rules
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteMatch {
    /// Destination matching condition
    #[serde(default)]
    pub destination: Option<MatchCondition>,

    /// Source matching condition
    #[serde(default)]
    pub source: Option<MatchCondition>,

    /// Sender ID matching condition
    #[serde(default)]
    pub sender_id: Option<MatchCondition>,

    /// System ID match (bound ESME system_id)
    #[serde(default)]
    pub system_id: Option<String>,

    /// Service type match (SMPP service_type field)
    #[serde(default)]
    pub service_type: Option<String>,

    /// ESM class match (for binary/flash SMS routing)
    #[serde(default)]
    pub esm_class: Option<u8>,

    /// Data coding match (encoding-based routing)
    #[serde(default)]
    pub data_coding: Option<Vec<u8>>,

    /// Time window for time-based routing
    #[serde(default)]
    pub time_window: Option<TimeWindow>,

    // Legacy fields for backward compatibility
    /// Prefix match on destination (legacy, use destination.prefix instead)
    #[serde(default)]
    pub prefix: Option<String>,

    /// Regex match on destination (legacy, use destination.regex instead)
    #[serde(default)]
    pub regex: Option<String>,
}

/// Flexible matching condition supporting multiple match types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchCondition {
    /// Prefix match
    #[serde(default)]
    pub prefix: Option<String>,

    /// Regex match
    #[serde(default)]
    pub regex: Option<String>,

    /// Exact match
    #[serde(default)]
    pub exact: Option<String>,

    /// Catch-all (matches any value)
    #[serde(default)]
    pub any: Option<bool>,
}

impl MatchCondition {
    /// Create a prefix matcher
    pub fn prefix(value: impl Into<String>) -> Self {
        Self {
            prefix: Some(value.into()),
            regex: None,
            exact: None,
            any: None,
        }
    }

    /// Create a regex matcher
    pub fn regex(pattern: impl Into<String>) -> Self {
        Self {
            prefix: None,
            regex: Some(pattern.into()),
            exact: None,
            any: None,
        }
    }

    /// Create an exact matcher
    pub fn exact(value: impl Into<String>) -> Self {
        Self {
            prefix: None,
            regex: None,
            exact: Some(value.into()),
            any: None,
        }
    }

    /// Create a catch-all matcher
    pub fn any() -> Self {
        Self {
            prefix: None,
            regex: None,
            exact: None,
            any: Some(true),
        }
    }
}

/// Time window for time-based routing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeWindow {
    /// Start time in HH:MM format
    pub start: String,

    /// End time in HH:MM format
    pub end: String,

    /// Days of week (Mon, Tue, Wed, Thu, Fri, Sat, Sun)
    #[serde(default = "default_all_days")]
    pub days: Vec<String>,

    /// Timezone (e.g., "Africa/Maputo", "UTC")
    #[serde(default = "default_timezone")]
    pub timezone: String,
}

fn default_all_days() -> Vec<String> {
    vec![
        "Mon".to_string(),
        "Tue".to_string(),
        "Wed".to_string(),
        "Thu".to_string(),
        "Fri".to_string(),
        "Sat".to_string(),
        "Sun".to_string(),
    ]
}

fn default_timezone() -> String {
    "UTC".to_string()
}

/// Admin API configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Global settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Shutdown configuration
    #[serde(default)]
    pub shutdown: ShutdownConfig,

    /// Message store configuration
    #[serde(default)]
    pub store: StoreConfig,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            shutdown: ShutdownConfig::default(),
            store: StoreConfig::default(),
        }
    }
}

/// Storage backend type.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageBackend {
    /// In-memory storage (volatile, for testing)
    Memory,
    /// Fjall (pure Rust LSM-tree, default for production)
    #[default]
    Fjall,
}

/// Message store configuration with polymorphic backend configs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreConfig {
    /// Storage backend type
    #[serde(default)]
    pub backend: StorageBackend,

    /// In-memory store configuration (used when backend = memory)
    #[serde(default)]
    pub memory: MemoryStoreConfig,

    /// Fjall store configuration (used when backend = fjall)
    #[serde(default)]
    pub fjall: FjallStoreConfig,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            backend: StorageBackend::default(),
            memory: MemoryStoreConfig::default(),
            fjall: FjallStoreConfig::default(),
        }
    }
}

impl StoreConfig {
    /// Get the backend name for logging.
    pub fn backend_name(&self) -> &'static str {
        match self.backend {
            StorageBackend::Memory => "memory",
            StorageBackend::Fjall => "fjall",
        }
    }

    /// Create in-memory store config for testing.
    pub fn memory() -> Self {
        Self {
            backend: StorageBackend::Memory,
            ..Default::default()
        }
    }

    /// Create fjall store config with defaults.
    pub fn fjall() -> Self {
        Self {
            backend: StorageBackend::Fjall,
            ..Default::default()
        }
    }
}

/// In-memory store configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStoreConfig {
    /// Maximum number of messages to keep in store.
    #[serde(default = "default_max_messages")]
    pub max_messages: usize,

    /// Maximum memory usage in megabytes (approximate).
    #[serde(default = "default_max_memory_mb")]
    pub max_memory_mb: usize,

    /// Percentage of messages to prune when at capacity (1-50).
    #[serde(default = "default_prune_percent")]
    pub prune_percent: u8,
}

impl Default for MemoryStoreConfig {
    fn default() -> Self {
        Self {
            max_messages: default_max_messages(),
            max_memory_mb: default_max_memory_mb(),
            prune_percent: default_prune_percent(),
        }
    }
}

fn default_max_memory_mb() -> usize {
    512
}

/// Fjall (LSM-tree) store configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FjallStoreConfig {
    /// Data directory path. If not set or "./data", uses XDG/FHS defaults.
    #[serde(default)]
    pub path: Option<std::path::PathBuf>,

    /// Maximum number of messages to keep in store.
    #[serde(default = "default_max_messages")]
    pub max_messages: usize,

    /// Block cache size in megabytes.
    #[serde(default = "default_block_cache_mb")]
    pub block_cache_mb: usize,

    /// Write buffer size in megabytes.
    #[serde(default = "default_write_buffer_mb")]
    pub write_buffer_mb: usize,

    /// Enable fsync on each write (slower but safer).
    #[serde(default)]
    pub fsync: bool,

    /// Compression enabled.
    #[serde(default = "default_true")]
    pub compression: bool,
}

impl Default for FjallStoreConfig {
    fn default() -> Self {
        Self {
            path: None,
            max_messages: default_max_messages(),
            block_cache_mb: default_block_cache_mb(),
            write_buffer_mb: default_write_buffer_mb(),
            fsync: false,
            compression: true,
        }
    }
}

fn default_block_cache_mb() -> usize {
    64
}

fn default_write_buffer_mb() -> usize {
    32
}

fn default_max_messages() -> usize {
    1_000_000
}

fn default_prune_percent() -> u8 {
    10
}

/// Telemetry configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryConfig {
    /// Worker threads (0 = num_cpus)
    #[serde(default)]
    pub workers: usize,

    /// Worker thread stack size (bytes)
    #[serde(default = "default_worker_stack_size")]
    pub worker_stack_size: usize,

    /// Bus capacity
    #[serde(default = "default_bus_capacity")]
    pub bus_capacity: usize,

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
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            workers: 0,
            worker_stack_size: default_worker_stack_size(),
            bus_capacity: default_bus_capacity(),
            json_logs: false,
            log_level: default_log_level(),
            otlp_endpoint: None,
            trace_sample_rate: default_sample_rate(),
        }
    }
}

fn default_worker_stack_size() -> usize {
    2 * 1024 * 1024 // 2MB
}

fn default_bus_capacity() -> usize {
    1024
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_sample_rate() -> f64 {
    1.0
}

/// Shutdown configuration (Envoy-style)
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    use serde::{self, Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        humantime::format_duration(*duration).to_string().serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        humantime::parse_duration(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============================================================================
    // Default Values Tests
    // ============================================================================

    #[test]
    fn test_default_connection_limits() {
        let limits = ConnectionLimits::default();
        assert_eq!(limits.max_connections, 10000);
        assert_eq!(limits.idle_timeout, Duration::from_secs(60));
        assert_eq!(limits.request_timeout, Duration::from_secs(30));
    }

    #[test]
    fn test_default_health_check_config() {
        let hc = HealthCheckConfig::default();
        assert_eq!(hc.interval, Duration::from_secs(30));
        assert_eq!(hc.timeout, Duration::from_secs(5));
        assert_eq!(hc.unhealthy_threshold, 3);
        assert_eq!(hc.healthy_threshold, 2);
    }

    #[test]
    fn test_default_pool_config() {
        let pool = PoolConfig::default();
        assert_eq!(pool.min_connections, 1);
        assert_eq!(pool.max_connections, 10);
        assert_eq!(pool.acquire_timeout, Duration::from_secs(5));
    }

    #[test]
    fn test_default_admin_config() {
        let admin = AdminConfig::default();
        assert_eq!(admin.address.port(), 9090);
        assert!(admin.metrics);
        assert!(admin.health);
    }

    #[test]
    fn test_default_telemetry_config() {
        let telemetry = TelemetryConfig::default();
        assert_eq!(telemetry.workers, 0);
        assert!(!telemetry.json_logs);
        assert_eq!(telemetry.log_level, "info");
        assert_eq!(telemetry.trace_sample_rate, 1.0);
    }

    #[test]
    fn test_default_shutdown_config() {
        let shutdown = ShutdownConfig::default();
        assert_eq!(shutdown.drain_timeout, Duration::from_secs(30));
        assert_eq!(shutdown.parent_shutdown_timeout, Duration::from_secs(60));
    }

    #[test]
    fn test_default_weight() {
        assert_eq!(default_weight(), 1);
    }

    // ============================================================================
    // Enum Tests
    // ============================================================================

    #[test]
    fn test_protocol_default() {
        assert_eq!(Protocol::default(), Protocol::Smpp);
    }

    #[test]
    fn test_load_balancer_default() {
        assert_eq!(LoadBalancer::default(), LoadBalancer::RoundRobin);
    }

    #[test]
    fn test_mock_response_default() {
        assert!(matches!(MockResponse::default(), MockResponse::Success));
    }

    // ============================================================================
    // Filter Type Tests
    // ============================================================================

    #[test]
    fn test_filter_types_exist() {
        // Ensure all filter types are available
        let _auth = FilterType::Auth;
        let _rate = FilterType::RateLimit;
        let _fw = FilterType::Firewall;
        let _tx = FilterType::Transform;
        let _credit = FilterType::Credit;
        let _otp = FilterType::Otp;
        let _consent = FilterType::Consent;
        let _dlt = FilterType::Dlt;
        let _sender = FilterType::SenderId;
        let _lua = FilterType::Lua;
    }
}
