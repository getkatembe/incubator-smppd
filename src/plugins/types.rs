//! Plugin system types and configuration.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// Plugin type enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginType {
    /// Lua script plugin (embedded mlua runtime)
    Lua,
    /// WebAssembly module (wasmtime runtime)
    Wasm,
    /// Native shared library (.so/.dll/.dylib)
    Native,
    /// HTTP webhook endpoint
    Webhook,
}

impl std::fmt::Display for PluginType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PluginType::Lua => write!(f, "lua"),
            PluginType::Wasm => write!(f, "wasm"),
            PluginType::Native => write!(f, "native"),
            PluginType::Webhook => write!(f, "webhook"),
        }
    }
}

/// Plugin capabilities flags.
#[derive(Debug, Clone, Default)]
pub struct PluginCapabilities {
    /// Can process messages through filter chain
    pub can_filter: bool,
    /// Can participate in route selection
    pub can_route: bool,
    /// Can transform/modify messages
    pub can_transform: bool,
    /// Implements MessageStore trait
    pub can_store: bool,
    /// Can handle hooks
    pub can_hook: bool,
}

/// Plugin resource limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginLimits {
    /// Maximum memory usage in MB (for WASM)
    pub memory_mb: u64,
    /// Maximum execution time per call in ms
    pub timeout_ms: u64,
    /// Maximum CPU fuel (for WASM)
    pub fuel: Option<u64>,
}

impl Default for PluginLimits {
    fn default() -> Self {
        Self {
            memory_mb: 64,
            timeout_ms: 1000,
            fuel: None,
        }
    }
}

/// Configuration for a single plugin instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfig {
    /// Unique plugin name
    pub name: String,
    /// Plugin type
    #[serde(rename = "type")]
    pub plugin_type: PluginType,
    /// Path to plugin file (for lua, wasm, native)
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// URL for webhook plugins
    #[serde(default)]
    pub url: Option<String>,
    /// Whether plugin is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Plugin priority (lower = higher priority)
    #[serde(default)]
    pub priority: i32,
    /// Resource limits
    #[serde(default)]
    pub limits: PluginLimits,
    /// Plugin-specific configuration
    #[serde(default)]
    pub config: HashMap<String, serde_yaml::Value>,
}

fn default_true() -> bool {
    true
}

/// Global plugins configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginsConfig {
    /// Base directory for plugin files
    #[serde(default)]
    pub directory: Option<PathBuf>,
    /// Plugin instances
    #[serde(default)]
    pub instances: Vec<PluginConfig>,
}

/// Plugin error types.
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("plugin not found: {0}")]
    NotFound(String),

    #[error("failed to load plugin: {0}")]
    LoadFailed(String),

    #[error("plugin initialization failed: {0}")]
    InitFailed(String),

    #[error("plugin execution failed: {0}")]
    ExecutionFailed(String),

    #[error("plugin timeout after {0:?}")]
    Timeout(Duration),

    #[error("plugin memory limit exceeded")]
    MemoryLimitExceeded,

    #[error("invalid plugin configuration: {0}")]
    InvalidConfig(String),

    #[error("hook not supported: {0}")]
    HookNotSupported(String),

    #[error("lua error: {0}")]
    LuaError(String),

    #[error("wasm error: {0}")]
    WasmError(String),

    #[error("native plugin error: {0}")]
    NativeError(String),

    #[error("webhook error: {0}")]
    WebhookError(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Filter action returned by filter plugins.
#[derive(Debug, Clone)]
pub enum FilterAction {
    /// Continue processing (pass to next filter)
    Continue,
    /// Accept the message
    Accept,
    /// Reject the message with status code
    Reject { status: u32, reason: Option<String> },
    /// Drop silently
    Drop,
    /// Log and continue
    Log { level: LogLevel, message: String },
    /// Quarantine for review
    Quarantine,
    /// Modify the message context
    Modify,
}

/// Log level for plugin logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// Route candidate for route selection plugins.
#[derive(Debug, Clone, Serialize)]
pub struct RouteCandidate {
    /// Route name
    pub route_name: String,
    /// Target cluster name
    pub cluster_name: String,
    /// Route priority
    pub priority: i32,
    /// Weight for load balancing
    pub weight: u32,
    /// Fallback clusters
    pub fallback: Vec<String>,
    /// Quality score (0.0-1.0)
    pub quality_score: Option<f64>,
    /// Cost per message (cents)
    pub cost: Option<i64>,
    /// Average latency in ms
    pub latency_ms: Option<u64>,
    /// Additional metadata
    pub metadata: HashMap<String, serde_yaml::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_type_display() {
        assert_eq!(PluginType::Lua.to_string(), "lua");
        assert_eq!(PluginType::Wasm.to_string(), "wasm");
        assert_eq!(PluginType::Native.to_string(), "native");
        assert_eq!(PluginType::Webhook.to_string(), "webhook");
    }

    #[test]
    fn test_plugin_config_deserialize() {
        let yaml = r#"
            name: test_plugin
            type: lua
            path: plugins/test.lua
            enabled: true
            priority: 10
            config:
              key1: value1
              key2: 42
        "#;

        let config: PluginConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.name, "test_plugin");
        assert_eq!(config.plugin_type, PluginType::Lua);
        assert_eq!(config.path, Some(PathBuf::from("plugins/test.lua")));
        assert!(config.enabled);
        assert_eq!(config.priority, 10);
    }

    #[test]
    fn test_plugin_limits_default() {
        let limits = PluginLimits::default();
        assert_eq!(limits.memory_mb, 64);
        assert_eq!(limits.timeout_ms, 1000);
        assert!(limits.fuel.is_none());
    }
}
