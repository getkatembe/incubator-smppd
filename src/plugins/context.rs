//! Plugin execution contexts.
//!
//! Contexts provide plugins with information about the current state
//! and allow them to interact with the system.

use crate::store::StoredMessage;
use serde::Serialize;
use std::collections::HashMap;

/// Context provided to plugins during initialization.
#[derive(Debug, Clone)]
pub struct PluginContext {
    /// Plugin name
    pub plugin_name: String,
    /// Plugin configuration (from YAML)
    pub config: HashMap<String, serde_yaml::Value>,
    /// Base directory for plugin files
    pub plugin_dir: Option<std::path::PathBuf>,
    /// Whether debug mode is enabled
    pub debug: bool,
}

impl PluginContext {
    /// Create a new plugin context.
    pub fn new(plugin_name: &str) -> Self {
        Self {
            plugin_name: plugin_name.to_string(),
            config: HashMap::new(),
            plugin_dir: None,
            debug: false,
        }
    }

    /// Set plugin configuration.
    pub fn with_config(mut self, config: HashMap<String, serde_yaml::Value>) -> Self {
        self.config = config;
        self
    }

    /// Set plugin directory.
    pub fn with_plugin_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.plugin_dir = Some(dir);
        self
    }

    /// Enable debug mode.
    pub fn with_debug(mut self, debug: bool) -> Self {
        self.debug = debug;
        self
    }

    /// Get a config value as string.
    pub fn get_string(&self, key: &str) -> Option<String> {
        self.config.get(key).and_then(|v| v.as_str()).map(String::from)
    }

    /// Get a config value as i64.
    pub fn get_i64(&self, key: &str) -> Option<i64> {
        self.config.get(key).and_then(|v| v.as_i64())
    }

    /// Get a config value as f64.
    pub fn get_f64(&self, key: &str) -> Option<f64> {
        self.config.get(key).and_then(|v| v.as_f64())
    }

    /// Get a config value as bool.
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.config.get(key).and_then(|v| v.as_bool())
    }
}

/// Context for filter plugin execution.
#[derive(Debug, Clone, Serialize)]
pub struct FilterContext {
    /// The message being filtered (not serialized - use extracted fields)
    #[serde(skip)]
    pub message: StoredMessage,
    /// Source address
    pub source: String,
    /// Destination address
    pub destination: String,
    /// Sender ID
    pub sender_id: String,
    /// System ID of the bound client
    pub system_id: String,
    /// Message content (decoded)
    pub content: String,
    /// ESM class
    pub esm_class: u8,
    /// Protocol ID
    pub protocol_id: u8,
    /// Data coding
    pub data_coding: u8,
    /// Client IP address
    pub client_ip: Option<String>,
    /// Whether connection uses TLS
    pub tls_enabled: bool,
    /// Custom metadata that plugins can modify
    pub metadata: HashMap<String, String>,
    /// Whether the message has been modified
    pub modified: bool,
}

impl FilterContext {
    /// Create a new filter context from a stored message.
    pub fn from_message(message: StoredMessage, system_id: &str) -> Self {
        Self {
            source: message.source.clone(),
            destination: message.destination.clone(),
            sender_id: message.source.clone(),
            content: String::from_utf8_lossy(&message.short_message).to_string(),
            esm_class: message.esm_class,
            protocol_id: 0,
            data_coding: message.data_coding,
            system_id: system_id.to_string(),
            client_ip: None,
            tls_enabled: false,
            metadata: HashMap::new(),
            modified: false,
            message,
        }
    }

    /// Set ESM class.
    pub fn with_esm_class(mut self, esm_class: u8) -> Self {
        self.esm_class = esm_class;
        self
    }

    /// Set protocol ID.
    pub fn with_protocol_id(mut self, protocol_id: u8) -> Self {
        self.protocol_id = protocol_id;
        self
    }

    /// Set data coding.
    pub fn with_data_coding(mut self, data_coding: u8) -> Self {
        self.data_coding = data_coding;
        self
    }

    /// Set client IP.
    pub fn with_client_ip(mut self, ip: &str) -> Self {
        self.client_ip = Some(ip.to_string());
        self
    }

    /// Set TLS status.
    pub fn with_tls(mut self, enabled: bool) -> Self {
        self.tls_enabled = enabled;
        self
    }

    /// Add metadata.
    pub fn with_metadata(mut self, key: &str, value: &str) -> Self {
        self.metadata.insert(key.to_string(), value.to_string());
        self
    }

    /// Mark as modified.
    pub fn mark_modified(&mut self) {
        self.modified = true;
    }

    /// Modify the destination.
    pub fn set_destination(&mut self, dest: &str) {
        self.destination = dest.to_string();
        self.message.destination = dest.to_string();
        self.modified = true;
    }

    /// Modify the source.
    pub fn set_source(&mut self, src: &str) {
        self.source = src.to_string();
        self.message.source = src.to_string();
        self.modified = true;
    }

    /// Modify the content.
    pub fn set_content(&mut self, content: &str) {
        self.content = content.to_string();
        self.message.short_message = content.as_bytes().to_vec();
        self.modified = true;
    }
}

/// Context for route plugin execution.
#[derive(Debug, Clone, Serialize)]
pub struct RouteContext {
    /// Destination address
    pub destination: String,
    /// Source address
    pub source: Option<String>,
    /// Sender ID
    pub sender_id: Option<String>,
    /// System ID of the bound client
    pub system_id: Option<String>,
    /// Service type
    pub service_type: Option<String>,
    /// ESM class
    pub esm_class: Option<u8>,
    /// Data coding
    pub data_coding: Option<u8>,
    /// Message priority
    pub priority: i32,
    /// Custom metadata
    pub metadata: HashMap<String, String>,
}

impl RouteContext {
    /// Create a new route context.
    pub fn new(destination: &str) -> Self {
        Self {
            destination: destination.to_string(),
            source: None,
            sender_id: None,
            system_id: None,
            service_type: None,
            esm_class: None,
            data_coding: None,
            priority: 0,
            metadata: HashMap::new(),
        }
    }

    /// Set source.
    pub fn with_source(mut self, source: &str) -> Self {
        self.source = Some(source.to_string());
        self
    }

    /// Set sender ID.
    pub fn with_sender_id(mut self, sender_id: &str) -> Self {
        self.sender_id = Some(sender_id.to_string());
        self
    }

    /// Set system ID.
    pub fn with_system_id(mut self, system_id: &str) -> Self {
        self.system_id = Some(system_id.to_string());
        self
    }

    /// Set service type.
    pub fn with_service_type(mut self, service_type: &str) -> Self {
        self.service_type = Some(service_type.to_string());
        self
    }

    /// Set ESM class.
    pub fn with_esm_class(mut self, esm_class: u8) -> Self {
        self.esm_class = Some(esm_class);
        self
    }

    /// Set data coding.
    pub fn with_data_coding(mut self, data_coding: u8) -> Self {
        self.data_coding = Some(data_coding);
        self
    }

    /// Set priority.
    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    /// Add metadata.
    pub fn with_metadata(mut self, key: &str, value: &str) -> Self {
        self.metadata.insert(key.to_string(), value.to_string());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_context() {
        let mut config = HashMap::new();
        config.insert("strategy".to_string(), serde_yaml::Value::String("balanced".to_string()));
        config.insert("threshold".to_string(), serde_yaml::Value::Number(100.into()));

        let ctx = PluginContext::new("test")
            .with_config(config)
            .with_debug(true);

        assert_eq!(ctx.plugin_name, "test");
        assert_eq!(ctx.get_string("strategy"), Some("balanced".to_string()));
        assert_eq!(ctx.get_i64("threshold"), Some(100));
        assert!(ctx.debug);
    }

    #[test]
    fn test_route_context() {
        let ctx = RouteContext::new("+258841234567")
            .with_source("+258821234567")
            .with_system_id("ACME")
            .with_priority(10);

        assert_eq!(ctx.destination, "+258841234567");
        assert_eq!(ctx.source, Some("+258821234567".to_string()));
        assert_eq!(ctx.system_id, Some("ACME".to_string()));
        assert_eq!(ctx.priority, 10);
    }
}
