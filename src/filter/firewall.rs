//! SMS Firewall filter for content and sender validation.
//!
//! Provides:
//! - Keyword/pattern blocking
//! - Sender ID validation
//! - Destination blacklisting
//! - Content pattern matching
//! - Spam detection heuristics

use std::collections::HashSet;
use std::sync::Arc;

use regex::Regex;
use smpp::pdu::{Pdu, Status};
use tower::{Layer, Service};
use tracing::{debug, warn};

use super::request::{SmppRequest, SmppResponse};

/// Firewall action for matched rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirewallAction {
    /// Allow the message
    Allow,
    /// Block with specific status code
    Block(Status),
    /// Log and allow
    Log,
    /// Quarantine for review
    Quarantine,
}

impl Default for FirewallAction {
    fn default() -> Self {
        Self::Block(Status::InvalidDestAddress)
    }
}

/// Firewall rule types.
#[derive(Debug, Clone)]
pub enum RuleType {
    /// Block specific keywords
    Keyword(String),
    /// Block regex pattern
    Pattern(Regex),
    /// Block sender ID
    SenderId(String),
    /// Block destination prefix
    DestinationPrefix(String),
    /// Block source prefix
    SourcePrefix(String),
}

impl RuleType {
    /// Check if rule matches the message.
    pub fn matches(&self, content: &str, sender_id: &str, destination: &str, source: &str) -> bool {
        match self {
            Self::Keyword(kw) => content.to_lowercase().contains(&kw.to_lowercase()),
            Self::Pattern(re) => re.is_match(content),
            Self::SenderId(id) => sender_id.to_lowercase() == id.to_lowercase(),
            Self::DestinationPrefix(prefix) => destination.starts_with(prefix),
            Self::SourcePrefix(prefix) => source.starts_with(prefix),
        }
    }
}

/// Firewall rule.
#[derive(Debug, Clone)]
pub struct FirewallRule {
    /// Rule name/identifier
    pub name: String,
    /// Rule type
    pub rule_type: RuleType,
    /// Action to take
    pub action: FirewallAction,
    /// Rule priority (higher = checked first)
    pub priority: i32,
    /// Is rule enabled
    pub enabled: bool,
}

impl FirewallRule {
    /// Create keyword blocking rule.
    pub fn block_keyword(name: &str, keyword: &str) -> Self {
        Self {
            name: name.to_string(),
            rule_type: RuleType::Keyword(keyword.to_string()),
            action: FirewallAction::Block(Status::InvalidDestAddress),
            priority: 0,
            enabled: true,
        }
    }

    /// Create pattern blocking rule.
    pub fn block_pattern(name: &str, pattern: &str) -> Option<Self> {
        Regex::new(pattern).ok().map(|re| Self {
            name: name.to_string(),
            rule_type: RuleType::Pattern(re),
            action: FirewallAction::Block(Status::InvalidDestAddress),
            priority: 0,
            enabled: true,
        })
    }

    /// Create sender ID blocking rule.
    pub fn block_sender(name: &str, sender_id: &str) -> Self {
        Self {
            name: name.to_string(),
            rule_type: RuleType::SenderId(sender_id.to_string()),
            action: FirewallAction::Block(Status::InvalidSourceAddress),
            priority: 0,
            enabled: true,
        }
    }

    /// Create destination prefix blocking rule.
    pub fn block_destination(name: &str, prefix: &str) -> Self {
        Self {
            name: name.to_string(),
            rule_type: RuleType::DestinationPrefix(prefix.to_string()),
            action: FirewallAction::Block(Status::InvalidDestAddress),
            priority: 0,
            enabled: true,
        }
    }

    /// Set action.
    pub fn with_action(mut self, action: FirewallAction) -> Self {
        self.action = action;
        self
    }

    /// Set priority.
    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }
}

/// Firewall configuration.
#[derive(Debug, Clone)]
pub struct FirewallConfig {
    /// Enable firewall
    pub enabled: bool,
    /// Default action for unmatched messages
    pub default_action: FirewallAction,
    /// Block premium rate numbers
    pub block_premium_rate: bool,
    /// Block international destinations
    pub block_international: bool,
    /// Allowed international prefixes (if blocking international)
    pub allowed_international: HashSet<String>,
    /// Maximum message length
    pub max_message_length: Option<usize>,
    /// Minimum message length
    pub min_message_length: Option<usize>,
}

impl Default for FirewallConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_action: FirewallAction::Allow,
            block_premium_rate: true,
            block_international: false,
            allowed_international: HashSet::new(),
            max_message_length: None,
            min_message_length: None,
        }
    }
}

/// SMS Firewall.
#[derive(Debug)]
pub struct Firewall {
    config: FirewallConfig,
    rules: Vec<FirewallRule>,
    /// Premium rate prefixes (country-specific)
    premium_prefixes: HashSet<String>,
}

impl Firewall {
    /// Create new firewall.
    pub fn new(config: FirewallConfig) -> Self {
        // Common premium rate prefixes
        let premium_prefixes: HashSet<String> = [
            "1900", "1976", // US
            "0900", "0909", // UK
            "0906", "0908", // Germany
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        Self {
            config,
            rules: Vec::new(),
            premium_prefixes,
        }
    }

    /// Add a rule.
    pub fn add_rule(&mut self, rule: FirewallRule) {
        self.rules.push(rule);
        // Sort by priority descending
        self.rules.sort_by(|a, b| b.priority.cmp(&a.priority));
    }

    /// Add multiple rules.
    pub fn with_rules(mut self, rules: Vec<FirewallRule>) -> Self {
        for rule in rules {
            self.add_rule(rule);
        }
        self
    }

    /// Check message against firewall rules.
    pub fn check(
        &self,
        content: &str,
        sender_id: &str,
        destination: &str,
        source: &str,
    ) -> FirewallResult {
        if !self.config.enabled {
            return FirewallResult::allowed();
        }

        // Check message length
        if let Some(max) = self.config.max_message_length {
            if content.len() > max {
                return FirewallResult::blocked(
                    "message_too_long",
                    Status::InvalidMessageLength,
                );
            }
        }

        if let Some(min) = self.config.min_message_length {
            if content.len() < min {
                return FirewallResult::blocked(
                    "message_too_short",
                    Status::InvalidMessageLength,
                );
            }
        }

        // Check premium rate
        if self.config.block_premium_rate {
            for prefix in &self.premium_prefixes {
                if destination.starts_with(prefix) {
                    return FirewallResult::blocked(
                        "premium_rate_blocked",
                        Status::InvalidDestAddress,
                    );
                }
            }
        }

        // Check international
        if self.config.block_international && destination.starts_with('+') {
            // Extract country code
            let country_code: String = destination
                .chars()
                .skip(1)
                .take_while(|c| c.is_ascii_digit())
                .take(3)
                .collect();

            if !self.config.allowed_international.contains(&country_code) {
                return FirewallResult::blocked(
                    "international_blocked",
                    Status::InvalidDestAddress,
                );
            }
        }

        // Check rules
        for rule in &self.rules {
            if !rule.enabled {
                continue;
            }

            if rule.rule_type.matches(content, sender_id, destination, source) {
                debug!(
                    rule = %rule.name,
                    action = ?rule.action,
                    "firewall rule matched"
                );

                match &rule.action {
                    FirewallAction::Allow => {
                        return FirewallResult::allowed();
                    }
                    FirewallAction::Block(status) => {
                        return FirewallResult {
                            allowed: false,
                            action: rule.action.clone(),
                            matched_rule: Some(rule.name.clone()),
                            status: Some(*status),
                        };
                    }
                    FirewallAction::Log => {
                        warn!(
                            rule = %rule.name,
                            sender_id = sender_id,
                            destination = destination,
                            "firewall log rule matched"
                        );
                        // Continue checking
                    }
                    FirewallAction::Quarantine => {
                        return FirewallResult {
                            allowed: false,
                            action: rule.action.clone(),
                            matched_rule: Some(rule.name.clone()),
                            status: Some(Status::SystemError),
                        };
                    }
                }
            }
        }

        // Default action
        match &self.config.default_action {
            FirewallAction::Allow => FirewallResult::allowed(),
            FirewallAction::Block(status) => FirewallResult::blocked("default_block", *status),
            _ => FirewallResult::allowed(),
        }
    }
}

/// Firewall check result.
#[derive(Debug, Clone)]
pub struct FirewallResult {
    /// Is message allowed
    pub allowed: bool,
    /// Action taken
    pub action: FirewallAction,
    /// Matched rule name
    pub matched_rule: Option<String>,
    /// Status code for blocked messages
    pub status: Option<Status>,
}

impl FirewallResult {
    /// Create allowed result.
    pub fn allowed() -> Self {
        Self {
            allowed: true,
            action: FirewallAction::Allow,
            matched_rule: None,
            status: None,
        }
    }

    /// Create blocked result.
    pub fn blocked(rule: &str, status: Status) -> Self {
        Self {
            allowed: false,
            action: FirewallAction::Block(status),
            matched_rule: Some(rule.to_string()),
            status: Some(status),
        }
    }
}

/// Tower layer for firewall.
#[derive(Clone)]
pub struct FirewallLayer {
    firewall: Arc<Firewall>,
}

impl FirewallLayer {
    /// Create new firewall layer.
    pub fn new(firewall: Arc<Firewall>) -> Self {
        Self { firewall }
    }
}

impl<S> Layer<S> for FirewallLayer {
    type Service = FirewallFilter<S>;

    fn layer(&self, inner: S) -> Self::Service {
        FirewallFilter {
            inner,
            firewall: self.firewall.clone(),
        }
    }
}

/// Firewall filter service.
#[derive(Clone)]
pub struct FirewallFilter<S> {
    inner: S,
    firewall: Arc<Firewall>,
}

impl<S> Service<SmppRequest> for FirewallFilter<S>
where
    S: Service<SmppRequest, Response = SmppResponse> + Clone + Send + 'static,
    S::Future: Send,
{
    type Response = SmppResponse;
    type Error = S::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: SmppRequest) -> Self::Future {
        let firewall = self.firewall.clone();
        let mut inner = self.inner.clone();
        let sequence = req.sequence;

        Box::pin(async move {
            // Extract message details from PDU
            let (content, sender_id, destination, source) = match &req.pdu {
                Pdu::SubmitSm(sm) => {
                    let content = String::from_utf8_lossy(&sm.short_message).to_string();
                    let sender_id = sm.source_addr.clone();
                    let destination = sm.dest_addr.clone();
                    let source = sm.source_addr.clone();
                    (content, sender_id, destination, source)
                }
                _ => {
                    // Non-submit PDUs pass through
                    return inner.call(req).await;
                }
            };

            // Check firewall
            let result = firewall.check(&content, &sender_id, &destination, &source);

            if result.allowed {
                inner.call(req).await
            } else {
                debug!(
                    rule = ?result.matched_rule,
                    status = ?result.status,
                    "message blocked by firewall"
                );

                Ok(SmppResponse::Reject {
                    sequence,
                    status: result.status.unwrap_or(Status::InvalidDestAddress),
                    reason: result.matched_rule,
                })
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keyword_blocking() {
        let mut firewall = Firewall::new(FirewallConfig::default());
        firewall.add_rule(FirewallRule::block_keyword("spam", "free money"));

        let result = firewall.check("Get FREE MONEY now!", "SENDER", "+1234567890", "+0987654321");
        assert!(!result.allowed);
        assert_eq!(result.matched_rule, Some("spam".to_string()));
    }

    #[test]
    fn test_pattern_blocking() {
        let mut firewall = Firewall::new(FirewallConfig::default());
        firewall.add_rule(
            FirewallRule::block_pattern("urls", r"https?://\S+").unwrap()
        );

        let result = firewall.check(
            "Visit https://spam.com for deals",
            "SENDER",
            "+1234567890",
            "+0987654321",
        );
        assert!(!result.allowed);
    }

    #[test]
    fn test_sender_blocking() {
        let mut firewall = Firewall::new(FirewallConfig::default());
        firewall.add_rule(FirewallRule::block_sender("blocked_sender", "SPAMMER"));

        let result = firewall.check("Hello", "SPAMMER", "+1234567890", "+0987654321");
        assert!(!result.allowed);

        let result = firewall.check("Hello", "LEGITCO", "+1234567890", "+0987654321");
        assert!(result.allowed);
    }

    #[test]
    fn test_destination_blocking() {
        let mut firewall = Firewall::new(FirewallConfig::default());
        firewall.add_rule(FirewallRule::block_destination("premium", "1900"));

        let result = firewall.check("Hello", "SENDER", "19001234567", "+0987654321");
        assert!(!result.allowed);
    }

    #[test]
    fn test_premium_rate_blocking() {
        let config = FirewallConfig {
            block_premium_rate: true,
            ..Default::default()
        };
        let firewall = Firewall::new(config);

        let result = firewall.check("Hello", "SENDER", "0900123456", "+0987654321");
        assert!(!result.allowed);
        assert_eq!(result.matched_rule, Some("premium_rate_blocked".to_string()));
    }

    #[test]
    fn test_message_length() {
        let config = FirewallConfig {
            max_message_length: Some(160),
            min_message_length: Some(1),
            ..Default::default()
        };
        let firewall = Firewall::new(config);

        // Too long
        let long_msg = "a".repeat(200);
        let result = firewall.check(&long_msg, "SENDER", "+1234567890", "+0987654321");
        assert!(!result.allowed);

        // Empty
        let result = firewall.check("", "SENDER", "+1234567890", "+0987654321");
        assert!(!result.allowed);

        // Valid
        let result = firewall.check("Hello", "SENDER", "+1234567890", "+0987654321");
        assert!(result.allowed);
    }

    #[test]
    fn test_rule_priority() {
        let mut firewall = Firewall::new(FirewallConfig::default());

        // Low priority block
        firewall.add_rule(
            FirewallRule::block_keyword("block_all", "test")
                .with_priority(0)
        );

        // High priority allow
        firewall.add_rule(FirewallRule {
            name: "allow_vip".to_string(),
            rule_type: RuleType::SenderId("VIP".to_string()),
            action: FirewallAction::Allow,
            priority: 100,
            enabled: true,
        });

        // VIP sender should pass even with blocked keyword
        let result = firewall.check("test message", "VIP", "+1234567890", "+0987654321");
        assert!(result.allowed);

        // Non-VIP should be blocked
        let result = firewall.check("test message", "REGULAR", "+1234567890", "+0987654321");
        assert!(!result.allowed);
    }
}
