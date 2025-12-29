//! Firewall types and configuration structures.
//!
//! Defines the core data structures for the SMS firewall:
//! - Chains, Rules
//! - Conditions and Actions
//! - External lists

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// Firewall configuration from YAML.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FirewallConfig {
    /// Whether firewall is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Default policy when no rules match
    #[serde(default)]
    pub default_policy: Policy,
    /// External lists configuration
    #[serde(default)]
    pub lists: HashMap<String, ExternalListConfig>,
    /// Firewall chains (evaluated in priority order)
    #[serde(default)]
    pub chains: Vec<ChainConfig>,
}

fn default_true() -> bool {
    true
}

/// Policy for chains/default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    #[default]
    Accept,
    Drop,
    Reject,
}

/// External list configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalListConfig {
    /// List type
    #[serde(rename = "type")]
    pub list_type: ListType,
    /// Path to list file
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// URL to fetch list from
    #[serde(default)]
    pub url: Option<String>,
    /// Reload interval
    #[serde(default, with = "humantime_serde")]
    pub reload_interval: Option<Duration>,
}

/// External list type.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListType {
    /// File-based list (one entry per line)
    File,
    /// HTTP URL to fetch
    Http,
    /// Redis set
    Redis,
}

/// Chain type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainType {
    /// Process incoming messages
    #[default]
    Input,
    /// Process outgoing messages
    Output,
    /// Forward/routing chain
    Forward,
}

/// Chain configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainConfig {
    /// Chain name
    pub name: String,
    /// Chain type
    #[serde(rename = "type", default)]
    pub chain_type: ChainType,
    /// Priority (lower = evaluated first)
    #[serde(default)]
    pub priority: i32,
    /// Default policy for this chain
    #[serde(default)]
    pub policy: Policy,
    /// Rules in this chain
    #[serde(default)]
    pub rules: Vec<RuleConfig>,
}

/// Rule configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleConfig {
    /// Optional rule name for logging/debugging
    #[serde(default)]
    pub name: Option<String>,
    /// Whether rule is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Conditions that must all match
    #[serde(default)]
    pub conditions: Vec<ConditionConfig>,
    /// Action to take when conditions match
    pub action: ActionConfig,
}

/// Condition configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConditionConfig {
    /// Match source address
    Source {
        #[serde(flatten)]
        matcher: StringMatcherConfig,
    },
    /// Match destination address
    Destination {
        #[serde(flatten)]
        matcher: StringMatcherConfig,
    },
    /// Match sender ID
    SenderId {
        #[serde(flatten)]
        matcher: StringMatcherConfig,
    },
    /// Match system ID
    SystemId {
        #[serde(flatten)]
        matcher: StringMatcherConfig,
    },
    /// Match message content
    Content {
        #[serde(flatten)]
        matcher: ContentMatcherConfig,
    },
    /// Match content length
    ContentLength {
        #[serde(default)]
        min: Option<usize>,
        #[serde(default)]
        max: Option<usize>,
        #[serde(default)]
        negate: bool,
    },
    /// Match ESM class
    EsmClass {
        value: u8,
        #[serde(default)]
        mask: Option<u8>,
    },
    /// Match protocol ID
    ProtocolId { value: u8 },
    /// Match data coding
    DataCoding { value: u8 },
    /// Match client IP
    ClientIp {
        #[serde(flatten)]
        matcher: IpMatcherConfig,
    },
    /// Match TLS status
    TlsEnabled { value: bool },
    /// Rate limiting condition
    Rate {
        /// Key to rate limit by (e.g., "system_id", "source", "client_ip")
        key: RateKey,
        /// Time window
        #[serde(with = "humantime_serde_required")]
        window: Duration,
        /// Maximum count in window
        threshold: u64,
    },
    /// Time-based condition
    Time {
        /// Start time (HH:MM format)
        start: String,
        /// End time (HH:MM format)
        end: String,
        /// Timezone (e.g., "Africa/Maputo")
        #[serde(default)]
        timezone: Option<String>,
    },
    /// Day of week condition
    DayOfWeek {
        /// Days (0=Sunday, 1=Monday, ..., 6=Saturday)
        days: Vec<u8>,
    },
    /// Negate a condition
    Not {
        condition: Box<ConditionConfig>,
    },
    /// All conditions must match
    And {
        conditions: Vec<ConditionConfig>,
    },
    /// Any condition must match
    Or {
        conditions: Vec<ConditionConfig>,
    },
    /// Plugin-provided condition
    Plugin {
        name: String,
        #[serde(default)]
        params: serde_yaml::Value,
    },
}

/// Key for rate limiting.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateKey {
    SystemId,
    Source,
    Destination,
    ClientIp,
    SenderId,
}

/// String matcher configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StringMatcherConfig {
    /// Match type
    #[serde(rename = "match", default)]
    pub match_type: StringMatchType,
    /// Value to match (for exact, prefix, suffix, contains)
    #[serde(default)]
    pub value: Option<String>,
    /// Pattern for regex matching
    #[serde(default)]
    pub pattern: Option<String>,
    /// List name for list matching
    #[serde(default)]
    pub list: Option<String>,
    /// Negate the match
    #[serde(default)]
    pub negate: bool,
}

/// String match type.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StringMatchType {
    #[default]
    Exact,
    Prefix,
    Suffix,
    Contains,
    Regex,
    List,
}

/// Content matcher configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentMatcherConfig {
    /// Match type
    #[serde(rename = "match", default)]
    pub match_type: ContentMatchType,
    /// Value for exact/contains
    #[serde(default)]
    pub value: Option<String>,
    /// Pattern for regex
    #[serde(default)]
    pub pattern: Option<String>,
    /// Keywords list name
    #[serde(default)]
    pub list: Option<String>,
    /// Case insensitive matching
    #[serde(default = "default_true")]
    pub case_insensitive: bool,
    /// Negate the match
    #[serde(default)]
    pub negate: bool,
}

/// Content match type.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentMatchType {
    #[default]
    Contains,
    Exact,
    Regex,
    Keywords,
}

/// IP matcher configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpMatcherConfig {
    /// Match type
    #[serde(rename = "match", default)]
    pub match_type: IpMatchType,
    /// Single IP or CIDR
    #[serde(default)]
    pub value: Option<String>,
    /// List name for list matching
    #[serde(default)]
    pub list: Option<String>,
    /// Negate the match
    #[serde(default)]
    pub negate: bool,
}

/// IP match type.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpMatchType {
    #[default]
    Exact,
    Cidr,
    List,
}

/// Action configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActionConfig {
    /// Accept the message
    Accept,
    /// Drop silently
    Drop,
    /// Reject with status
    Reject {
        /// SMPP status code
        #[serde(default = "default_reject_status")]
        status: u32,
        /// Reason message
        #[serde(default)]
        reason: Option<String>,
    },
    /// Log and continue
    Log {
        #[serde(default)]
        level: LogLevel,
        #[serde(default)]
        message: Option<String>,
        /// Action after logging (default: continue)
        #[serde(default, rename = "then")]
        then_action: Option<Box<ActionConfig>>,
    },
    /// Rate limit (reject if exceeded)
    RateLimit {
        key: RateKey,
        #[serde(with = "humantime_serde_required")]
        window: Duration,
        limit: u64,
        /// Status when limit exceeded
        #[serde(default = "default_throttle_status")]
        status: u32,
    },
    /// Modify message fields
    Modify {
        #[serde(default)]
        set_source: Option<String>,
        #[serde(default)]
        set_destination: Option<String>,
        #[serde(default)]
        set_content: Option<String>,
        #[serde(default)]
        prepend_content: Option<String>,
        #[serde(default)]
        append_content: Option<String>,
    },
    /// Jump to another chain
    Jump { chain: String },
    /// Return from chain
    Return,
    /// Quarantine message for review
    Quarantine,
    /// Delay processing
    Delay {
        #[serde(with = "humantime_serde_required")]
        duration: Duration,
    },
    /// Set metadata
    Mark {
        key: String,
        value: String,
    },
    /// Plugin-provided action
    Plugin {
        name: String,
        action: String,
        #[serde(default)]
        params: serde_yaml::Value,
    },
}

fn default_reject_status() -> u32 {
    8 // ESME_RSYSERR
}

fn default_throttle_status() -> u32 {
    88 // ESME_RTHROTTLED
}

/// Log level for logging actions.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

/// Firewall error types.
#[derive(Debug, thiserror::Error)]
pub enum FirewallError {
    #[error("configuration error: {0}")]
    ConfigError(String),

    #[error("rule evaluation error: {0}")]
    EvaluationError(String),

    #[error("list not found: {0}")]
    ListNotFound(String),

    #[error("invalid condition: {0}")]
    InvalidCondition(String),

    #[error("chain not found: {0}")]
    ChainNotFound(String),

    #[error("regex error: {0}")]
    RegexError(#[from] regex::Error),

    #[error("io error: {0}")]
    IoError(#[from] std::io::Error),
}

/// Humantime serde module for optional Duration serialization.
mod humantime_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(duration: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match duration {
            Some(d) => serializer.serialize_str(&humantime::format_duration(*d).to_string()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s: Option<String> = Option::deserialize(deserializer)?;
        match s {
            Some(s) => humantime::parse_duration(&s)
                .map(Some)
                .map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
}

/// Humantime serde module for required Duration serialization.
mod humantime_serde_required {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&humantime::format_duration(*duration).to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s: String = String::deserialize(deserializer)?;
        humantime::parse_duration(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_firewall_config_deserialize() {
        let yaml = r#"
enabled: true
default_policy: accept

lists:
  blocked_senders:
    type: file
    path: /etc/smppd/lists/blocked.txt
    reload_interval: 60s

chains:
  - name: input
    type: input
    priority: 0
    policy: accept
    rules:
      - name: block_spam
        conditions:
          - type: content
            match: keywords
            list: spam_keywords
        action:
          type: drop
"#;

        let config: FirewallConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.enabled);
        assert_eq!(config.default_policy, Policy::Accept);
        assert!(config.lists.contains_key("blocked_senders"));
        assert_eq!(config.chains.len(), 1);
        assert_eq!(config.chains[0].rules.len(), 1);
    }

    #[test]
    fn test_condition_deserialize() {
        let yaml = r#"
type: rate
key: system_id
window: 1s
threshold: 100
"#;
        let condition: ConditionConfig = serde_yaml::from_str(yaml).unwrap();
        match condition {
            ConditionConfig::Rate { key, threshold, .. } => {
                assert!(matches!(key, RateKey::SystemId));
                assert_eq!(threshold, 100);
            }
            _ => panic!("Expected Rate condition"),
        }
    }

    #[test]
    fn test_action_deserialize() {
        let yaml = r#"
type: reject
status: 88
reason: Rate limit exceeded
"#;
        let action: ActionConfig = serde_yaml::from_str(yaml).unwrap();
        match action {
            ActionConfig::Reject { status, reason } => {
                assert_eq!(status, 88);
                assert_eq!(reason, Some("Rate limit exceeded".to_string()));
            }
            _ => panic!("Expected Reject action"),
        }
    }
}
