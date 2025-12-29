//! Firewall rule evaluation engine.
//!
//! Evaluates conditions and executes actions for firewall rules.

use super::context::{EvaluationContext, EvaluationResult};
use super::types::*;
use chrono::{Datelike, Timelike};
use regex::Regex;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info, trace, warn};

/// Compiled firewall for efficient evaluation.
pub struct CompiledFirewall {
    /// Whether firewall is enabled
    enabled: bool,
    /// Default policy
    default_policy: Policy,
    /// Compiled chains (sorted by priority)
    chains: Vec<CompiledChain>,
    /// External lists
    lists: Arc<RwLock<HashMap<String, Vec<String>>>>,
    /// Rate limiting state
    rate_limits: Arc<RwLock<HashMap<String, RateLimitState>>>,
    /// Rule hit counters
    rule_counters: Arc<RwLock<HashMap<String, AtomicU64>>>,
}

/// Compiled chain.
struct CompiledChain {
    name: String,
    chain_type: ChainType,
    priority: i32,
    policy: Policy,
    rules: Vec<CompiledRule>,
}

/// Compiled rule.
struct CompiledRule {
    name: Option<String>,
    enabled: bool,
    conditions: Vec<CompiledCondition>,
    action: ActionConfig,
    counter: AtomicU64,
}

/// Compiled condition for efficient evaluation.
enum CompiledCondition {
    Source(StringMatcher),
    Destination(StringMatcher),
    SenderId(StringMatcher),
    SystemId(StringMatcher),
    Content(ContentMatcher),
    ContentLength { min: Option<usize>, max: Option<usize>, negate: bool },
    EsmClass { value: u8, mask: Option<u8> },
    ProtocolId { value: u8 },
    DataCoding { value: u8 },
    ClientIp(IpMatcher),
    TlsEnabled { value: bool },
    Rate { key: RateKey, window: Duration, threshold: u64 },
    Time { start_hour: u32, start_min: u32, end_hour: u32, end_min: u32, timezone: Option<String> },
    DayOfWeek { days: Vec<u8> },
    Not(Box<CompiledCondition>),
    And(Vec<CompiledCondition>),
    Or(Vec<CompiledCondition>),
}

/// String matcher.
enum StringMatcher {
    Exact { value: String, negate: bool },
    Prefix { value: String, negate: bool },
    Suffix { value: String, negate: bool },
    Contains { value: String, negate: bool },
    Regex { pattern: Regex, negate: bool },
    List { list_name: String, negate: bool },
}

/// Content matcher.
enum ContentMatcher {
    Contains { value: String, case_insensitive: bool, negate: bool },
    Exact { value: String, case_insensitive: bool, negate: bool },
    Regex { pattern: Regex, negate: bool },
    Keywords { list_name: String, case_insensitive: bool, negate: bool },
}

/// IP matcher.
enum IpMatcher {
    Exact { ip: IpAddr, negate: bool },
    Cidr { network: ipnet::IpNet, negate: bool },
    List { list_name: String, negate: bool },
}

/// Rate limit state for a key.
struct RateLimitState {
    count: u64,
    window_start: Instant,
}

impl CompiledFirewall {
    /// Create a new compiled firewall from configuration.
    pub fn from_config(config: &FirewallConfig) -> Result<Self, FirewallError> {
        let mut chains = Vec::new();

        for chain_config in &config.chains {
            let mut rules = Vec::new();

            for rule_config in &chain_config.rules {
                let conditions = rule_config
                    .conditions
                    .iter()
                    .map(Self::compile_condition)
                    .collect::<Result<Vec<_>, _>>()?;

                rules.push(CompiledRule {
                    name: rule_config.name.clone(),
                    enabled: rule_config.enabled,
                    conditions,
                    action: rule_config.action.clone(),
                    counter: AtomicU64::new(0),
                });
            }

            chains.push(CompiledChain {
                name: chain_config.name.clone(),
                chain_type: chain_config.chain_type,
                priority: chain_config.priority,
                policy: chain_config.policy,
                rules,
            });
        }

        // Sort chains by priority
        chains.sort_by_key(|c| c.priority);

        Ok(Self {
            enabled: config.enabled,
            default_policy: config.default_policy,
            chains,
            lists: Arc::new(RwLock::new(HashMap::new())),
            rate_limits: Arc::new(RwLock::new(HashMap::new())),
            rule_counters: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Compile a condition from configuration.
    fn compile_condition(config: &ConditionConfig) -> Result<CompiledCondition, FirewallError> {
        match config {
            ConditionConfig::Source { matcher } => {
                Ok(CompiledCondition::Source(Self::compile_string_matcher(matcher)?))
            }
            ConditionConfig::Destination { matcher } => {
                Ok(CompiledCondition::Destination(Self::compile_string_matcher(matcher)?))
            }
            ConditionConfig::SenderId { matcher } => {
                Ok(CompiledCondition::SenderId(Self::compile_string_matcher(matcher)?))
            }
            ConditionConfig::SystemId { matcher } => {
                Ok(CompiledCondition::SystemId(Self::compile_string_matcher(matcher)?))
            }
            ConditionConfig::Content { matcher } => {
                Ok(CompiledCondition::Content(Self::compile_content_matcher(matcher)?))
            }
            ConditionConfig::ContentLength { min, max, negate } => {
                Ok(CompiledCondition::ContentLength {
                    min: *min,
                    max: *max,
                    negate: *negate,
                })
            }
            ConditionConfig::EsmClass { value, mask } => {
                Ok(CompiledCondition::EsmClass { value: *value, mask: *mask })
            }
            ConditionConfig::ProtocolId { value } => {
                Ok(CompiledCondition::ProtocolId { value: *value })
            }
            ConditionConfig::DataCoding { value } => {
                Ok(CompiledCondition::DataCoding { value: *value })
            }
            ConditionConfig::ClientIp { matcher } => {
                Ok(CompiledCondition::ClientIp(Self::compile_ip_matcher(matcher)?))
            }
            ConditionConfig::TlsEnabled { value } => {
                Ok(CompiledCondition::TlsEnabled { value: *value })
            }
            ConditionConfig::Rate { key, window, threshold } => {
                Ok(CompiledCondition::Rate {
                    key: *key,
                    window: *window,
                    threshold: *threshold,
                })
            }
            ConditionConfig::Time { start, end, timezone } => {
                let (start_hour, start_min) = Self::parse_time(start)?;
                let (end_hour, end_min) = Self::parse_time(end)?;
                Ok(CompiledCondition::Time {
                    start_hour,
                    start_min,
                    end_hour,
                    end_min,
                    timezone: timezone.clone(),
                })
            }
            ConditionConfig::DayOfWeek { days } => {
                Ok(CompiledCondition::DayOfWeek { days: days.clone() })
            }
            ConditionConfig::Not { condition } => {
                Ok(CompiledCondition::Not(Box::new(Self::compile_condition(condition)?)))
            }
            ConditionConfig::And { conditions } => {
                let compiled: Result<Vec<_>, _> = conditions
                    .iter()
                    .map(Self::compile_condition)
                    .collect();
                Ok(CompiledCondition::And(compiled?))
            }
            ConditionConfig::Or { conditions } => {
                let compiled: Result<Vec<_>, _> = conditions
                    .iter()
                    .map(Self::compile_condition)
                    .collect();
                Ok(CompiledCondition::Or(compiled?))
            }
            ConditionConfig::Plugin { .. } => {
                // Plugin conditions are evaluated at runtime
                Err(FirewallError::InvalidCondition(
                    "plugin conditions not yet supported".into(),
                ))
            }
        }
    }

    fn compile_string_matcher(config: &StringMatcherConfig) -> Result<StringMatcher, FirewallError> {
        match config.match_type {
            StringMatchType::Exact => {
                let value = config.value.clone().ok_or_else(|| {
                    FirewallError::InvalidCondition("exact match requires value".into())
                })?;
                Ok(StringMatcher::Exact { value, negate: config.negate })
            }
            StringMatchType::Prefix => {
                let value = config.value.clone().ok_or_else(|| {
                    FirewallError::InvalidCondition("prefix match requires value".into())
                })?;
                Ok(StringMatcher::Prefix { value, negate: config.negate })
            }
            StringMatchType::Suffix => {
                let value = config.value.clone().ok_or_else(|| {
                    FirewallError::InvalidCondition("suffix match requires value".into())
                })?;
                Ok(StringMatcher::Suffix { value, negate: config.negate })
            }
            StringMatchType::Contains => {
                let value = config.value.clone().ok_or_else(|| {
                    FirewallError::InvalidCondition("contains match requires value".into())
                })?;
                Ok(StringMatcher::Contains { value, negate: config.negate })
            }
            StringMatchType::Regex => {
                let pattern = config.pattern.clone().ok_or_else(|| {
                    FirewallError::InvalidCondition("regex match requires pattern".into())
                })?;
                let regex = Regex::new(&pattern)?;
                Ok(StringMatcher::Regex { pattern: regex, negate: config.negate })
            }
            StringMatchType::List => {
                let list_name = config.list.clone().ok_or_else(|| {
                    FirewallError::InvalidCondition("list match requires list name".into())
                })?;
                Ok(StringMatcher::List { list_name, negate: config.negate })
            }
        }
    }

    fn compile_content_matcher(config: &ContentMatcherConfig) -> Result<ContentMatcher, FirewallError> {
        match config.match_type {
            ContentMatchType::Contains => {
                let value = config.value.clone().ok_or_else(|| {
                    FirewallError::InvalidCondition("contains match requires value".into())
                })?;
                Ok(ContentMatcher::Contains {
                    value,
                    case_insensitive: config.case_insensitive,
                    negate: config.negate,
                })
            }
            ContentMatchType::Exact => {
                let value = config.value.clone().ok_or_else(|| {
                    FirewallError::InvalidCondition("exact match requires value".into())
                })?;
                Ok(ContentMatcher::Exact {
                    value,
                    case_insensitive: config.case_insensitive,
                    negate: config.negate,
                })
            }
            ContentMatchType::Regex => {
                let pattern = config.pattern.clone().ok_or_else(|| {
                    FirewallError::InvalidCondition("regex match requires pattern".into())
                })?;
                let regex = if config.case_insensitive {
                    Regex::new(&format!("(?i){}", pattern))?
                } else {
                    Regex::new(&pattern)?
                };
                Ok(ContentMatcher::Regex { pattern: regex, negate: config.negate })
            }
            ContentMatchType::Keywords => {
                let list_name = config.list.clone().ok_or_else(|| {
                    FirewallError::InvalidCondition("keywords match requires list name".into())
                })?;
                Ok(ContentMatcher::Keywords {
                    list_name,
                    case_insensitive: config.case_insensitive,
                    negate: config.negate,
                })
            }
        }
    }

    fn compile_ip_matcher(config: &IpMatcherConfig) -> Result<IpMatcher, FirewallError> {
        match config.match_type {
            IpMatchType::Exact => {
                let value = config.value.clone().ok_or_else(|| {
                    FirewallError::InvalidCondition("exact IP match requires value".into())
                })?;
                let ip: IpAddr = value.parse().map_err(|_| {
                    FirewallError::InvalidCondition(format!("invalid IP address: {}", value))
                })?;
                Ok(IpMatcher::Exact { ip, negate: config.negate })
            }
            IpMatchType::Cidr => {
                let value = config.value.clone().ok_or_else(|| {
                    FirewallError::InvalidCondition("CIDR match requires value".into())
                })?;
                let network: ipnet::IpNet = value.parse().map_err(|_| {
                    FirewallError::InvalidCondition(format!("invalid CIDR: {}", value))
                })?;
                Ok(IpMatcher::Cidr { network, negate: config.negate })
            }
            IpMatchType::List => {
                let list_name = config.list.clone().ok_or_else(|| {
                    FirewallError::InvalidCondition("list IP match requires list name".into())
                })?;
                Ok(IpMatcher::List { list_name, negate: config.negate })
            }
        }
    }

    fn parse_time(time_str: &str) -> Result<(u32, u32), FirewallError> {
        let parts: Vec<&str> = time_str.split(':').collect();
        if parts.len() != 2 {
            return Err(FirewallError::InvalidCondition(format!(
                "invalid time format: {}",
                time_str
            )));
        }
        let hour: u32 = parts[0].parse().map_err(|_| {
            FirewallError::InvalidCondition(format!("invalid hour: {}", parts[0]))
        })?;
        let min: u32 = parts[1].parse().map_err(|_| {
            FirewallError::InvalidCondition(format!("invalid minute: {}", parts[1]))
        })?;
        Ok((hour, min))
    }

    /// Load an external list.
    pub async fn load_list(&self, name: &str, entries: Vec<String>) {
        let mut lists = self.lists.write().await;
        lists.insert(name.to_string(), entries);
        debug!(list = name, count = lists.get(name).map(|l| l.len()).unwrap_or(0), "loaded external list");
    }

    /// Evaluate the firewall for a message.
    pub async fn evaluate(&self, ctx: &EvaluationContext) -> EvaluationResult {
        if !self.enabled {
            return EvaluationResult::Accept;
        }

        trace!(
            source = %ctx.source,
            destination = %ctx.destination,
            system_id = %ctx.system_id,
            "evaluating firewall"
        );

        // Evaluate input chains (already sorted by priority)
        for chain in &self.chains {
            if chain.chain_type != ChainType::Input {
                continue;
            }

            let result = self.evaluate_chain(chain, ctx).await;
            if result.is_terminal() {
                return result;
            }
        }

        // Apply default policy
        match self.default_policy {
            Policy::Accept => EvaluationResult::Accept,
            Policy::Drop => EvaluationResult::Drop,
            Policy::Reject => EvaluationResult::Reject {
                status: 8,
                reason: None,
            },
        }
    }

    async fn evaluate_chain(&self, chain: &CompiledChain, ctx: &EvaluationContext) -> EvaluationResult {
        for rule in &chain.rules {
            if !rule.enabled {
                continue;
            }

            // Check all conditions
            let matches = self.evaluate_conditions(&rule.conditions, ctx).await;

            if matches {
                rule.counter.fetch_add(1, Ordering::Relaxed);

                if let Some(ref name) = rule.name {
                    debug!(rule = %name, "rule matched");
                }

                // Execute action
                return self.execute_action(&rule.action, ctx).await;
            }
        }

        // Apply chain policy
        match chain.policy {
            Policy::Accept => EvaluationResult::Accept,
            Policy::Drop => EvaluationResult::Drop,
            Policy::Reject => EvaluationResult::Reject {
                status: 8,
                reason: None,
            },
        }
    }

    async fn evaluate_conditions(
        &self,
        conditions: &[CompiledCondition],
        ctx: &EvaluationContext,
    ) -> bool {
        for condition in conditions {
            if !self.evaluate_condition(condition, ctx).await {
                return false;
            }
        }
        true
    }

    async fn evaluate_condition(&self, condition: &CompiledCondition, ctx: &EvaluationContext) -> bool {
        match condition {
            CompiledCondition::Source(matcher) => {
                self.match_string(matcher, &ctx.source).await
            }
            CompiledCondition::Destination(matcher) => {
                self.match_string(matcher, &ctx.destination).await
            }
            CompiledCondition::SenderId(matcher) => {
                if let Some(sender_id) = &ctx.sender_id {
                    self.match_string(matcher, sender_id).await
                } else {
                    false
                }
            }
            CompiledCondition::SystemId(matcher) => {
                self.match_string(matcher, &ctx.system_id).await
            }
            CompiledCondition::Content(matcher) => {
                self.match_content(matcher, &ctx.content).await
            }
            CompiledCondition::ContentLength { min, max, negate } => {
                let len = ctx.content_length;
                let in_range = min.map(|m| len >= m).unwrap_or(true)
                    && max.map(|m| len <= m).unwrap_or(true);
                if *negate { !in_range } else { in_range }
            }
            CompiledCondition::EsmClass { value, mask } => {
                let actual = ctx.esm_class;
                match mask {
                    Some(m) => (actual & m) == (*value & m),
                    None => actual == *value,
                }
            }
            CompiledCondition::ProtocolId { value } => ctx.protocol_id == *value,
            CompiledCondition::DataCoding { value } => ctx.data_coding == *value,
            CompiledCondition::ClientIp(matcher) => {
                if let Some(ip) = ctx.client_ip {
                    self.match_ip(matcher, ip).await
                } else {
                    false
                }
            }
            CompiledCondition::TlsEnabled { value } => ctx.tls_enabled == *value,
            CompiledCondition::Rate { key, window, threshold } => {
                self.check_rate_limit(key, *window, *threshold, ctx).await
            }
            CompiledCondition::Time { start_hour, start_min, end_hour, end_min, timezone } => {
                self.check_time(*start_hour, *start_min, *end_hour, *end_min, timezone.as_deref())
            }
            CompiledCondition::DayOfWeek { days } => {
                let now = chrono::Utc::now();
                let day = now.weekday().num_days_from_sunday() as u8;
                days.contains(&day)
            }
            CompiledCondition::Not(inner) => {
                !Box::pin(self.evaluate_condition(inner, ctx)).await
            }
            CompiledCondition::And(conditions) => {
                for c in conditions {
                    if !Box::pin(self.evaluate_condition(c, ctx)).await {
                        return false;
                    }
                }
                true
            }
            CompiledCondition::Or(conditions) => {
                for c in conditions {
                    if Box::pin(self.evaluate_condition(c, ctx)).await {
                        return true;
                    }
                }
                false
            }
        }
    }

    async fn match_string(&self, matcher: &StringMatcher, value: &str) -> bool {
        match matcher {
            StringMatcher::Exact { value: pattern, negate } => {
                let matches = value == pattern;
                if *negate { !matches } else { matches }
            }
            StringMatcher::Prefix { value: pattern, negate } => {
                let matches = value.starts_with(pattern);
                if *negate { !matches } else { matches }
            }
            StringMatcher::Suffix { value: pattern, negate } => {
                let matches = value.ends_with(pattern);
                if *negate { !matches } else { matches }
            }
            StringMatcher::Contains { value: pattern, negate } => {
                let matches = value.contains(pattern);
                if *negate { !matches } else { matches }
            }
            StringMatcher::Regex { pattern, negate } => {
                let matches = pattern.is_match(value);
                if *negate { !matches } else { matches }
            }
            StringMatcher::List { list_name, negate } => {
                let lists = self.lists.read().await;
                let matches = lists
                    .get(list_name)
                    .map(|list| list.iter().any(|entry| entry == value))
                    .unwrap_or(false);
                if *negate { !matches } else { matches }
            }
        }
    }

    async fn match_content(&self, matcher: &ContentMatcher, content: &str) -> bool {
        match matcher {
            ContentMatcher::Contains { value, case_insensitive, negate } => {
                let matches = if *case_insensitive {
                    content.to_lowercase().contains(&value.to_lowercase())
                } else {
                    content.contains(value)
                };
                if *negate { !matches } else { matches }
            }
            ContentMatcher::Exact { value, case_insensitive, negate } => {
                let matches = if *case_insensitive {
                    content.to_lowercase() == value.to_lowercase()
                } else {
                    content == value
                };
                if *negate { !matches } else { matches }
            }
            ContentMatcher::Regex { pattern, negate } => {
                let matches = pattern.is_match(content);
                if *negate { !matches } else { matches }
            }
            ContentMatcher::Keywords { list_name, case_insensitive, negate } => {
                let lists = self.lists.read().await;
                let matches = lists.get(list_name).map(|keywords| {
                    let check_content = if *case_insensitive {
                        content.to_lowercase()
                    } else {
                        content.to_string()
                    };
                    keywords.iter().any(|kw| {
                        let check_kw = if *case_insensitive {
                            kw.to_lowercase()
                        } else {
                            kw.clone()
                        };
                        check_content.contains(&check_kw)
                    })
                }).unwrap_or(false);
                if *negate { !matches } else { matches }
            }
        }
    }

    async fn match_ip(&self, matcher: &IpMatcher, ip: IpAddr) -> bool {
        match matcher {
            IpMatcher::Exact { ip: expected, negate } => {
                let matches = ip == *expected;
                if *negate { !matches } else { matches }
            }
            IpMatcher::Cidr { network, negate } => {
                let matches = network.contains(&ip);
                if *negate { !matches } else { matches }
            }
            IpMatcher::List { list_name, negate } => {
                let lists = self.lists.read().await;
                let matches = lists.get(list_name).map(|list| {
                    let ip_str = ip.to_string();
                    list.iter().any(|entry| entry == &ip_str)
                }).unwrap_or(false);
                if *negate { !matches } else { matches }
            }
        }
    }

    async fn check_rate_limit(
        &self,
        key: &RateKey,
        window: Duration,
        threshold: u64,
        ctx: &EvaluationContext,
    ) -> bool {
        let rate_key = format!("{:?}:{}", key, ctx.get_rate_key(key));
        let now = Instant::now();

        let mut rate_limits = self.rate_limits.write().await;
        let state = rate_limits.entry(rate_key).or_insert_with(|| RateLimitState {
            count: 0,
            window_start: now,
        });

        // Reset if window expired
        if now.duration_since(state.window_start) >= window {
            state.count = 0;
            state.window_start = now;
        }

        state.count += 1;
        state.count > threshold
    }

    fn check_time(
        &self,
        start_hour: u32,
        start_min: u32,
        end_hour: u32,
        end_min: u32,
        _timezone: Option<&str>,
    ) -> bool {
        let now = chrono::Utc::now();
        let current_minutes = now.hour() * 60 + now.minute();
        let start_minutes = start_hour * 60 + start_min;
        let end_minutes = end_hour * 60 + end_min;

        if start_minutes <= end_minutes {
            current_minutes >= start_minutes && current_minutes <= end_minutes
        } else {
            // Crosses midnight
            current_minutes >= start_minutes || current_minutes <= end_minutes
        }
    }

    async fn execute_action(&self, action: &ActionConfig, ctx: &EvaluationContext) -> EvaluationResult {
        match action {
            ActionConfig::Accept => EvaluationResult::Accept,
            ActionConfig::Drop => EvaluationResult::Drop,
            ActionConfig::Reject { status, reason } => EvaluationResult::Reject {
                status: *status,
                reason: reason.clone(),
            },
            ActionConfig::Log { level, message, then_action } => {
                let msg = message.clone().unwrap_or_else(|| {
                    format!("Firewall: {} -> {}", ctx.source, ctx.destination)
                });
                match level {
                    LogLevel::Trace => trace!("{}", msg),
                    LogLevel::Debug => debug!("{}", msg),
                    LogLevel::Info => info!("{}", msg),
                    LogLevel::Warn => warn!("{}", msg),
                    LogLevel::Error => tracing::error!("{}", msg),
                }
                if let Some(then_action) = then_action {
                    Box::pin(self.execute_action(then_action, ctx)).await
                } else {
                    EvaluationResult::Continue
                }
            }
            ActionConfig::Quarantine => EvaluationResult::Quarantine,
            ActionConfig::Modify { set_source, set_destination, set_content, .. } => {
                EvaluationResult::Modify {
                    source: set_source.clone(),
                    destination: set_destination.clone(),
                    content: set_content.clone(),
                }
            }
            ActionConfig::Mark { key, value } => {
                debug!(key = %key, value = %value, "marking message");
                EvaluationResult::Continue
            }
            _ => EvaluationResult::Continue,
        }
    }

    /// Get statistics about firewall operation.
    pub async fn get_stats(&self) -> FirewallStats {
        let mut rule_hits = HashMap::new();
        for chain in &self.chains {
            for rule in &chain.rules {
                if let Some(ref name) = rule.name {
                    rule_hits.insert(name.clone(), rule.counter.load(Ordering::Relaxed));
                }
            }
        }

        FirewallStats {
            enabled: self.enabled,
            chains: self.chains.len(),
            rules: self.chains.iter().map(|c| c.rules.len()).sum(),
            lists: self.lists.read().await.len(),
            rule_hits,
        }
    }
}

/// Firewall statistics.
#[derive(Debug, Clone)]
pub struct FirewallStats {
    pub enabled: bool,
    pub chains: usize,
    pub rules: usize,
    pub lists: usize,
    pub rule_hits: HashMap<String, u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_simple_firewall() {
        let config = FirewallConfig {
            enabled: true,
            default_policy: Policy::Accept,
            lists: HashMap::new(),
            chains: vec![ChainConfig {
                name: "input".to_string(),
                chain_type: ChainType::Input,
                priority: 0,
                policy: Policy::Accept,
                rules: vec![RuleConfig {
                    name: Some("block_spam".to_string()),
                    enabled: true,
                    conditions: vec![ConditionConfig::Content {
                        matcher: ContentMatcherConfig {
                            match_type: ContentMatchType::Contains,
                            value: Some("SPAM".to_string()),
                            pattern: None,
                            list: None,
                            case_insensitive: true,
                            negate: false,
                        },
                    }],
                    action: ActionConfig::Drop,
                }],
            }],
        };

        let firewall = CompiledFirewall::from_config(&config).unwrap();

        // Test message without spam
        let ctx = EvaluationContext::new("+258841234567", "+258821234567", "ACME", "Hello");
        let result = firewall.evaluate(&ctx).await;
        assert!(matches!(result, EvaluationResult::Accept));

        // Test message with spam
        let ctx = EvaluationContext::new("+258841234567", "+258821234567", "ACME", "This is SPAM message");
        let result = firewall.evaluate(&ctx).await;
        assert!(matches!(result, EvaluationResult::Drop));
    }
}
