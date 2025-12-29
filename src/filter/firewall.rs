//! SMS Firewall filter for content and sender validation.
//!
//! Provides comprehensive message filtering:
//! - MSISDN allow/deny lists
//! - Keyword/pattern blocking
//! - Sender ID validation
//! - Destination filtering
//! - Protocol ID / ESM class filtering
//! - Data coding scheme filtering
//! - Time-of-day restrictions
//! - Content pattern matching
//! - Spam detection heuristics
//! - Event logging with timestamps

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use chrono::{Datelike, NaiveTime, Weekday};
use chrono_tz::Tz;
use regex::Regex;
use smpp::pdu::{Pdu, Status};
use tower::{Layer, Service};
use tracing::{debug, info, warn};

use super::request::{SmppRequest, SmppResponse};

/// Firewall action for matched rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirewallAction {
    /// Allow the message
    Allow,
    /// Block with specific status code
    Block(Status),
    /// Log and allow (monitor mode)
    Log,
    /// Quarantine for review
    Quarantine,
    /// Modify message (e.g., strip content)
    Modify,
    /// Raise alarm and allow
    Alarm,
}

impl Default for FirewallAction {
    fn default() -> Self {
        Self::Block(Status::InvalidDestAddress)
    }
}

/// Time window for time-based rules.
#[derive(Debug, Clone)]
pub struct TimeWindow {
    /// Start time (HH:MM)
    pub start: NaiveTime,
    /// End time (HH:MM)
    pub end: NaiveTime,
    /// Days of week (empty = all days)
    pub days: HashSet<Weekday>,
    /// Timezone
    pub timezone: Tz,
}

impl TimeWindow {
    /// Create a new time window.
    pub fn new(start: &str, end: &str, timezone: &str) -> Option<Self> {
        let start = NaiveTime::parse_from_str(start, "%H:%M").ok()?;
        let end = NaiveTime::parse_from_str(end, "%H:%M").ok()?;
        let tz: Tz = timezone.parse().ok()?;

        Some(Self {
            start,
            end,
            days: HashSet::new(),
            timezone: tz,
        })
    }

    /// Add allowed days.
    pub fn with_days(mut self, days: Vec<Weekday>) -> Self {
        self.days = days.into_iter().collect();
        self
    }

    /// Check if current time is within window.
    pub fn is_active(&self) -> bool {
        use chrono::Utc;

        let now = Utc::now().with_timezone(&self.timezone);
        let current_time = now.time();
        let current_day = now.weekday();

        // Check day restriction
        if !self.days.is_empty() && !self.days.contains(&current_day) {
            return false;
        }

        // Handle overnight windows (e.g., 22:00 - 06:00)
        if self.start <= self.end {
            current_time >= self.start && current_time <= self.end
        } else {
            current_time >= self.start || current_time <= self.end
        }
    }
}

/// SMPP protocol parameters for filtering.
#[derive(Debug, Clone, Default)]
pub struct ProtocolParams {
    /// ESM class (message mode/type)
    pub esm_class: Option<u8>,
    /// Protocol ID
    pub protocol_id: Option<u8>,
    /// Data coding scheme
    pub data_coding: Option<u8>,
    /// Source address TON
    pub source_addr_ton: Option<u8>,
    /// Source address NPI
    pub source_addr_npi: Option<u8>,
    /// Destination address TON
    pub dest_addr_ton: Option<u8>,
    /// Destination address NPI
    pub dest_addr_npi: Option<u8>,
}

/// Firewall rule types.
#[derive(Debug, Clone)]
pub enum RuleType {
    /// Block specific keywords
    Keyword(String),
    /// Block regex pattern
    Pattern(Regex),
    /// Match sender ID
    SenderId(String),
    /// Match destination prefix
    DestinationPrefix(String),
    /// Match source prefix
    SourcePrefix(String),
    /// Match destination in list
    DestinationList(HashSet<String>),
    /// Match source in list
    SourceList(HashSet<String>),
    /// Match SMPP protocol parameters
    Protocol(ProtocolParams),
    /// Time-based rule
    TimeWindow(TimeWindow),
    /// Combined rules (all must match)
    All(Vec<RuleType>),
    /// Combined rules (any must match)
    Any(Vec<RuleType>),
}

impl RuleType {
    /// Check if rule matches the message.
    pub fn matches(&self, ctx: &MessageContext) -> bool {
        match self {
            Self::Keyword(kw) => ctx.content.to_lowercase().contains(&kw.to_lowercase()),
            Self::Pattern(re) => re.is_match(&ctx.content),
            Self::SenderId(id) => ctx.sender_id.to_lowercase() == id.to_lowercase(),
            Self::DestinationPrefix(prefix) => ctx.destination.starts_with(prefix),
            Self::SourcePrefix(prefix) => ctx.source.starts_with(prefix),
            Self::DestinationList(list) => list.contains(&ctx.destination),
            Self::SourceList(list) => list.contains(&ctx.source),
            Self::Protocol(params) => {
                if let Some(esm) = params.esm_class {
                    if ctx.protocol.esm_class != Some(esm) {
                        return false;
                    }
                }
                if let Some(pid) = params.protocol_id {
                    if ctx.protocol.protocol_id != Some(pid) {
                        return false;
                    }
                }
                if let Some(dcs) = params.data_coding {
                    if ctx.protocol.data_coding != Some(dcs) {
                        return false;
                    }
                }
                true
            }
            Self::TimeWindow(window) => window.is_active(),
            Self::All(rules) => rules.iter().all(|r| r.matches(ctx)),
            Self::Any(rules) => rules.iter().any(|r| r.matches(ctx)),
        }
    }
}

/// Message context for firewall evaluation.
#[derive(Debug, Clone)]
pub struct MessageContext {
    /// Message content
    pub content: String,
    /// Sender ID
    pub sender_id: String,
    /// Destination address
    pub destination: String,
    /// Source address
    pub source: String,
    /// System ID of the submitting client
    pub system_id: String,
    /// Protocol parameters
    pub protocol: ProtocolParams,
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
    /// Optional description
    pub description: Option<String>,
    /// ESME/system_id this rule applies to (None = all)
    pub esme_id: Option<String>,
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
            description: None,
            esme_id: None,
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
            description: None,
            esme_id: None,
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
            description: None,
            esme_id: None,
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
            description: None,
            esme_id: None,
        }
    }

    /// Create destination deny list rule.
    pub fn deny_destinations(name: &str, numbers: Vec<String>) -> Self {
        Self {
            name: name.to_string(),
            rule_type: RuleType::DestinationList(numbers.into_iter().collect()),
            action: FirewallAction::Block(Status::InvalidDestAddress),
            priority: 0,
            enabled: true,
            description: None,
            esme_id: None,
        }
    }

    /// Create source deny list rule.
    pub fn deny_sources(name: &str, numbers: Vec<String>) -> Self {
        Self {
            name: name.to_string(),
            rule_type: RuleType::SourceList(numbers.into_iter().collect()),
            action: FirewallAction::Block(Status::InvalidSourceAddress),
            priority: 0,
            enabled: true,
            description: None,
            esme_id: None,
        }
    }

    /// Create time-based rule.
    pub fn time_restriction(name: &str, window: TimeWindow, action: FirewallAction) -> Self {
        Self {
            name: name.to_string(),
            rule_type: RuleType::TimeWindow(window),
            action,
            priority: 0,
            enabled: true,
            description: None,
            esme_id: None,
        }
    }

    /// Create protocol-based rule.
    pub fn protocol_filter(name: &str, params: ProtocolParams, action: FirewallAction) -> Self {
        Self {
            name: name.to_string(),
            rule_type: RuleType::Protocol(params),
            action,
            priority: 0,
            enabled: true,
            description: None,
            esme_id: None,
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

    /// Set ESME restriction.
    pub fn for_esme(mut self, esme_id: &str) -> Self {
        self.esme_id = Some(esme_id.to_string());
        self
    }

    /// Set description.
    pub fn with_description(mut self, desc: &str) -> Self {
        self.description = Some(desc.to_string());
        self
    }
}

/// Firewall event for logging.
#[derive(Debug, Clone)]
pub struct FirewallEvent {
    /// Timestamp
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Event type
    pub event_type: FirewallEventType,
    /// Rule that matched (if any)
    pub rule_name: Option<String>,
    /// Action taken
    pub action: FirewallAction,
    /// Source address
    pub source: String,
    /// Destination address
    pub destination: String,
    /// Sender ID
    pub sender_id: String,
    /// ESME/system ID
    pub system_id: String,
    /// Message snippet (first 50 chars)
    pub content_preview: String,
}

/// Type of firewall event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirewallEventType {
    /// Message blocked
    Blocked,
    /// Message allowed
    Allowed,
    /// Message logged (monitor mode)
    Logged,
    /// Message quarantined
    Quarantined,
    /// Alarm raised
    Alarm,
}

/// Firewall event log.
#[derive(Debug)]
pub struct FirewallEventLog {
    events: RwLock<VecDeque<FirewallEvent>>,
    max_events: usize,
}

impl FirewallEventLog {
    /// Create new event log.
    pub fn new(max_events: usize) -> Self {
        Self {
            events: RwLock::new(VecDeque::with_capacity(max_events)),
            max_events,
        }
    }

    /// Log an event.
    pub fn log(&self, event: FirewallEvent) {
        let mut events = self.events.write().unwrap();
        if events.len() >= self.max_events {
            events.pop_front();
        }
        events.push_back(event);
    }

    /// Get recent events.
    pub fn recent(&self, count: usize) -> Vec<FirewallEvent> {
        let events = self.events.read().unwrap();
        events.iter().rev().take(count).cloned().collect()
    }

    /// Get events by type.
    pub fn by_type(&self, event_type: FirewallEventType, count: usize) -> Vec<FirewallEvent> {
        let events = self.events.read().unwrap();
        events
            .iter()
            .rev()
            .filter(|e| e.event_type == event_type)
            .take(count)
            .cloned()
            .collect()
    }

    /// Get blocked count.
    pub fn blocked_count(&self) -> usize {
        let events = self.events.read().unwrap();
        events
            .iter()
            .filter(|e| e.event_type == FirewallEventType::Blocked)
            .count()
    }

    /// Get statistics.
    pub fn stats(&self) -> FirewallEventStats {
        let events = self.events.read().unwrap();
        let mut stats = FirewallEventStats::default();

        for event in events.iter() {
            match event.event_type {
                FirewallEventType::Blocked => stats.blocked += 1,
                FirewallEventType::Allowed => stats.allowed += 1,
                FirewallEventType::Logged => stats.logged += 1,
                FirewallEventType::Quarantined => stats.quarantined += 1,
                FirewallEventType::Alarm => stats.alarms += 1,
            }
        }

        stats
    }
}

/// Event statistics.
#[derive(Debug, Clone, Default)]
pub struct FirewallEventStats {
    pub blocked: u64,
    pub allowed: u64,
    pub logged: u64,
    pub quarantined: u64,
    pub alarms: u64,
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
    /// Allowed international country codes (if blocking international)
    pub allowed_international: HashSet<String>,
    /// Maximum message length
    pub max_message_length: Option<usize>,
    /// Minimum message length
    pub min_message_length: Option<usize>,
    /// Allowed data coding schemes
    pub allowed_data_coding: Option<HashSet<u8>>,
    /// Denied ESM classes (e.g., flash SMS)
    pub denied_esm_class: HashSet<u8>,
    /// Max events in log
    pub max_log_events: usize,
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
            allowed_data_coding: None,
            denied_esm_class: HashSet::new(),
            max_log_events: 10_000,
        }
    }
}

/// Per-ESME firewall settings.
#[derive(Debug, Clone, Default)]
pub struct EsmeFirewallSettings {
    /// Destination allow list (if set, only these allowed)
    pub destination_allow_list: Option<HashSet<String>>,
    /// Destination deny list
    pub destination_deny_list: HashSet<String>,
    /// Source allow list
    pub source_allow_list: Option<HashSet<String>>,
    /// Source deny list
    pub source_deny_list: HashSet<String>,
    /// Sender ID allow list
    pub sender_allow_list: Option<HashSet<String>>,
    /// Sender ID deny list
    pub sender_deny_list: HashSet<String>,
    /// Custom rules
    pub rules: Vec<FirewallRule>,
}

/// SMS Firewall.
#[derive(Debug)]
pub struct Firewall {
    config: FirewallConfig,
    rules: Vec<FirewallRule>,
    /// Premium rate prefixes (country-specific)
    premium_prefixes: HashSet<String>,
    /// Per-ESME settings
    esme_settings: RwLock<HashMap<String, EsmeFirewallSettings>>,
    /// Event log
    event_log: Arc<FirewallEventLog>,
}

impl Firewall {
    /// Create new firewall.
    pub fn new(config: FirewallConfig) -> Self {
        // Common premium rate prefixes
        let premium_prefixes: HashSet<String> = [
            "1900", "1976", // US
            "0900", "0909", // UK
            "0906", "0908", // Germany
            "0871", "0872", "0873", // UK revenue share
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let event_log = Arc::new(FirewallEventLog::new(config.max_log_events));

        Self {
            config,
            rules: Vec::new(),
            premium_prefixes,
            esme_settings: RwLock::new(HashMap::new()),
            event_log,
        }
    }

    /// Get event log reference.
    pub fn event_log(&self) -> Arc<FirewallEventLog> {
        self.event_log.clone()
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

    /// Set ESME settings.
    pub fn set_esme_settings(&self, esme_id: &str, settings: EsmeFirewallSettings) {
        let mut cs = self.esme_settings.write().unwrap();
        cs.insert(esme_id.to_string(), settings);
    }

    /// Get ESME settings.
    pub fn get_esme_settings(&self, esme_id: &str) -> Option<EsmeFirewallSettings> {
        let cs = self.esme_settings.read().unwrap();
        cs.get(esme_id).cloned()
    }

    /// Check message against firewall rules.
    pub fn check(&self, ctx: &MessageContext) -> FirewallResult {
        if !self.config.enabled {
            return FirewallResult::allowed();
        }

        let start = Instant::now();

        // Check message length
        if let Some(max) = self.config.max_message_length {
            if ctx.content.len() > max {
                return self.log_result(ctx, FirewallResult::blocked(
                    "message_too_long",
                    Status::InvalidMessageLength,
                ));
            }
        }

        if let Some(min) = self.config.min_message_length {
            if ctx.content.len() < min {
                return self.log_result(ctx, FirewallResult::blocked(
                    "message_too_short",
                    Status::InvalidMessageLength,
                ));
            }
        }

        // Check data coding scheme
        if let Some(ref allowed_dcs) = self.config.allowed_data_coding {
            if let Some(dcs) = ctx.protocol.data_coding {
                if !allowed_dcs.contains(&dcs) {
                    return self.log_result(ctx, FirewallResult::blocked(
                        "data_coding_not_allowed",
                        Status::InvalidDestAddress,
                    ));
                }
            }
        }

        // Check ESM class
        if let Some(esm) = ctx.protocol.esm_class {
            if self.config.denied_esm_class.contains(&esm) {
                return self.log_result(ctx, FirewallResult::blocked(
                    "esm_class_denied",
                    Status::InvalidDestAddress,
                ));
            }
        }

        // Check premium rate
        if self.config.block_premium_rate {
            for prefix in &self.premium_prefixes {
                if ctx.destination.starts_with(prefix) {
                    return self.log_result(ctx, FirewallResult::blocked(
                        "premium_rate_blocked",
                        Status::InvalidDestAddress,
                    ));
                }
            }
        }

        // Check international
        if self.config.block_international && ctx.destination.starts_with('+') {
            let country_code: String = ctx.destination
                .chars()
                .skip(1)
                .take_while(|c| c.is_ascii_digit())
                .take(3)
                .collect();

            if !self.config.allowed_international.contains(&country_code) {
                return self.log_result(ctx, FirewallResult::blocked(
                    "international_blocked",
                    Status::InvalidDestAddress,
                ));
            }
        }

        // Check per-ESME settings
        if let Some(settings) = self.get_esme_settings(&ctx.system_id) {
            // Destination allow list (if set, destination must be in it)
            if let Some(ref allow_list) = settings.destination_allow_list {
                if !allow_list.contains(&ctx.destination) {
                    return self.log_result(ctx, FirewallResult::blocked(
                        "destination_not_in_allow_list",
                        Status::InvalidDestAddress,
                    ));
                }
            }

            // Destination deny list
            if settings.destination_deny_list.contains(&ctx.destination) {
                return self.log_result(ctx, FirewallResult::blocked(
                    "destination_in_deny_list",
                    Status::InvalidDestAddress,
                ));
            }

            // Source allow list
            if let Some(ref allow_list) = settings.source_allow_list {
                if !allow_list.contains(&ctx.source) {
                    return self.log_result(ctx, FirewallResult::blocked(
                        "source_not_in_allow_list",
                        Status::InvalidSourceAddress,
                    ));
                }
            }

            // Source deny list
            if settings.source_deny_list.contains(&ctx.source) {
                return self.log_result(ctx, FirewallResult::blocked(
                    "source_in_deny_list",
                    Status::InvalidSourceAddress,
                ));
            }

            // Sender allow list
            if let Some(ref allow_list) = settings.sender_allow_list {
                if !allow_list.contains(&ctx.sender_id) {
                    return self.log_result(ctx, FirewallResult::blocked(
                        "sender_not_in_allow_list",
                        Status::InvalidSourceAddress,
                    ));
                }
            }

            // Sender deny list
            if settings.sender_deny_list.contains(&ctx.sender_id) {
                return self.log_result(ctx, FirewallResult::blocked(
                    "sender_in_deny_list",
                    Status::InvalidSourceAddress,
                ));
            }

            // ESME-specific rules
            for rule in &settings.rules {
                if !rule.enabled {
                    continue;
                }
                if let Some(result) = self.apply_rule(rule, ctx) {
                    return self.log_result(ctx, result);
                }
            }
        }

        // Check global rules
        for rule in &self.rules {
            if !rule.enabled {
                continue;
            }

            // Skip ESME-specific rules that don't match
            if let Some(ref cid) = rule.esme_id {
                if cid != &ctx.system_id {
                    continue;
                }
            }

            if let Some(result) = self.apply_rule(rule, ctx) {
                return self.log_result(ctx, result);
            }
        }

        // Default action
        let result = match &self.config.default_action {
            FirewallAction::Allow => FirewallResult::allowed(),
            FirewallAction::Block(status) => FirewallResult::blocked("default_block", *status),
            _ => FirewallResult::allowed(),
        };

        debug!(
            elapsed_us = start.elapsed().as_micros() as u64,
            allowed = result.allowed,
            "firewall check complete"
        );

        self.log_result(ctx, result)
    }

    /// Apply a single rule.
    fn apply_rule(&self, rule: &FirewallRule, ctx: &MessageContext) -> Option<FirewallResult> {
        if rule.rule_type.matches(ctx) {
            debug!(
                rule = %rule.name,
                action = ?rule.action,
                "firewall rule matched"
            );

            match &rule.action {
                FirewallAction::Allow => {
                    return Some(FirewallResult::allowed());
                }
                FirewallAction::Block(status) => {
                    return Some(FirewallResult {
                        allowed: false,
                        action: rule.action.clone(),
                        matched_rule: Some(rule.name.clone()),
                        status: Some(*status),
                    });
                }
                FirewallAction::Log => {
                    info!(
                        rule = %rule.name,
                        sender_id = %ctx.sender_id,
                        destination = %ctx.destination,
                        "firewall log rule matched"
                    );
                    // Continue checking
                }
                FirewallAction::Alarm => {
                    warn!(
                        rule = %rule.name,
                        sender_id = %ctx.sender_id,
                        destination = %ctx.destination,
                        source = %ctx.source,
                        system_id = %ctx.system_id,
                        "FIREWALL ALARM: suspicious message detected"
                    );
                    // Log alarm event
                    self.event_log.log(FirewallEvent {
                        timestamp: chrono::Utc::now(),
                        event_type: FirewallEventType::Alarm,
                        rule_name: Some(rule.name.clone()),
                        action: rule.action.clone(),
                        source: ctx.source.clone(),
                        destination: ctx.destination.clone(),
                        sender_id: ctx.sender_id.clone(),
                        system_id: ctx.system_id.clone(),
                        content_preview: ctx.content.chars().take(50).collect(),
                    });
                    // Continue (allow)
                }
                FirewallAction::Quarantine => {
                    return Some(FirewallResult {
                        allowed: false,
                        action: rule.action.clone(),
                        matched_rule: Some(rule.name.clone()),
                        status: Some(Status::SystemError),
                    });
                }
                FirewallAction::Modify => {
                    // Modification is handled separately
                    return Some(FirewallResult {
                        allowed: true,
                        action: rule.action.clone(),
                        matched_rule: Some(rule.name.clone()),
                        status: None,
                    });
                }
            }
        }
        None
    }

    /// Log result to event log.
    fn log_result(&self, ctx: &MessageContext, result: FirewallResult) -> FirewallResult {
        let event_type = if result.allowed {
            match &result.action {
                FirewallAction::Log => FirewallEventType::Logged,
                _ => FirewallEventType::Allowed,
            }
        } else {
            match &result.action {
                FirewallAction::Quarantine => FirewallEventType::Quarantined,
                _ => FirewallEventType::Blocked,
            }
        };

        self.event_log.log(FirewallEvent {
            timestamp: chrono::Utc::now(),
            event_type,
            rule_name: result.matched_rule.clone(),
            action: result.action.clone(),
            source: ctx.source.clone(),
            destination: ctx.destination.clone(),
            sender_id: ctx.sender_id.clone(),
            system_id: ctx.system_id.clone(),
            content_preview: ctx.content.chars().take(50).collect(),
        });

        result
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
            let ctx = match &req.pdu {
                Pdu::SubmitSm(sm) => {
                    MessageContext {
                        content: String::from_utf8_lossy(&sm.short_message).to_string(),
                        sender_id: sm.source_addr.clone(),
                        destination: sm.dest_addr.clone(),
                        source: sm.source_addr.clone(),
                        system_id: String::new(), // TODO: pass from session
                        protocol: ProtocolParams {
                            esm_class: Some(sm.esm_class),
                            protocol_id: Some(sm.protocol_id),
                            data_coding: Some(sm.data_coding),
                            source_addr_ton: Some(sm.source_addr_ton),
                            source_addr_npi: Some(sm.source_addr_npi),
                            dest_addr_ton: Some(sm.dest_addr_ton),
                            dest_addr_npi: Some(sm.dest_addr_npi),
                        },
                    }
                }
                _ => {
                    // Non-submit PDUs pass through
                    return inner.call(req).await;
                }
            };

            // Check firewall
            let result = firewall.check(&ctx);

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

    fn test_ctx(content: &str, sender: &str, dest: &str) -> MessageContext {
        MessageContext {
            content: content.to_string(),
            sender_id: sender.to_string(),
            destination: dest.to_string(),
            source: sender.to_string(),
            system_id: "test_esme".to_string(),
            protocol: ProtocolParams::default(),
        }
    }

    #[test]
    fn test_keyword_blocking() {
        let mut firewall = Firewall::new(FirewallConfig::default());
        firewall.add_rule(FirewallRule::block_keyword("spam", "free money"));

        let ctx = test_ctx("Get FREE MONEY now!", "SENDER", "+1234567890");
        let result = firewall.check(&ctx);
        assert!(!result.allowed);
        assert_eq!(result.matched_rule, Some("spam".to_string()));
    }

    #[test]
    fn test_pattern_blocking() {
        let mut firewall = Firewall::new(FirewallConfig::default());
        firewall.add_rule(FirewallRule::block_pattern("urls", r"https?://\S+").unwrap());

        let ctx = test_ctx("Visit https://spam.com for deals", "SENDER", "+1234567890");
        let result = firewall.check(&ctx);
        assert!(!result.allowed);
    }

    #[test]
    fn test_sender_blocking() {
        let mut firewall = Firewall::new(FirewallConfig::default());
        firewall.add_rule(FirewallRule::block_sender("blocked_sender", "SPAMMER"));

        let ctx = test_ctx("Hello", "SPAMMER", "+1234567890");
        assert!(!firewall.check(&ctx).allowed);

        let ctx = test_ctx("Hello", "LEGITCO", "+1234567890");
        assert!(firewall.check(&ctx).allowed);
    }

    #[test]
    fn test_destination_deny_list() {
        let mut firewall = Firewall::new(FirewallConfig::default());
        firewall.add_rule(FirewallRule::deny_destinations(
            "blocked_numbers",
            vec!["+1234567890".to_string(), "+0987654321".to_string()],
        ));

        let ctx = test_ctx("Hello", "SENDER", "+1234567890");
        assert!(!firewall.check(&ctx).allowed);

        let ctx = test_ctx("Hello", "SENDER", "+1111111111");
        assert!(firewall.check(&ctx).allowed);
    }

    #[test]
    fn test_esme_settings() {
        let firewall = Firewall::new(FirewallConfig::default());

        // Set ESME-specific deny list
        let mut settings = EsmeFirewallSettings::default();
        settings.destination_deny_list.insert("+blocked_number".to_string());
        firewall.set_esme_settings("test_esme", settings);

        let ctx = test_ctx("Hello", "SENDER", "+blocked_number");
        assert!(!firewall.check(&ctx).allowed);

        // Different ESME should not be affected
        let mut ctx2 = test_ctx("Hello", "SENDER", "+blocked_number");
        ctx2.system_id = "other_esme".to_string();
        assert!(firewall.check(&ctx2).allowed);
    }

    #[test]
    fn test_esme_allow_list() {
        let firewall = Firewall::new(FirewallConfig::default());

        // Set ESME-specific allow list (only these numbers allowed)
        let mut settings = EsmeFirewallSettings::default();
        settings.destination_allow_list = Some(
            vec!["+allowed_number".to_string()].into_iter().collect()
        );
        firewall.set_esme_settings("test_esme", settings);

        // Allowed number passes
        let ctx = test_ctx("Hello", "SENDER", "+allowed_number");
        assert!(firewall.check(&ctx).allowed);

        // Non-allowed number blocked
        let ctx = test_ctx("Hello", "SENDER", "+other_number");
        assert!(!firewall.check(&ctx).allowed);
    }

    #[test]
    fn test_premium_rate_blocking() {
        let config = FirewallConfig {
            block_premium_rate: true,
            ..Default::default()
        };
        let firewall = Firewall::new(config);

        let ctx = test_ctx("Hello", "SENDER", "0900123456");
        let result = firewall.check(&ctx);
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
        let ctx = test_ctx(&"a".repeat(200), "SENDER", "+1234567890");
        assert!(!firewall.check(&ctx).allowed);

        // Empty
        let ctx = test_ctx("", "SENDER", "+1234567890");
        assert!(!firewall.check(&ctx).allowed);

        // Valid
        let ctx = test_ctx("Hello", "SENDER", "+1234567890");
        assert!(firewall.check(&ctx).allowed);
    }

    #[test]
    fn test_esm_class_filtering() {
        let mut config = FirewallConfig::default();
        config.denied_esm_class.insert(0x40); // Flash SMS

        let firewall = Firewall::new(config);

        let mut ctx = test_ctx("Hello", "SENDER", "+1234567890");
        ctx.protocol.esm_class = Some(0x40);

        assert!(!firewall.check(&ctx).allowed);
    }

    #[test]
    fn test_rule_priority() {
        let mut firewall = Firewall::new(FirewallConfig::default());

        // Low priority block
        firewall.add_rule(
            FirewallRule::block_keyword("block_all", "test").with_priority(0),
        );

        // High priority allow
        firewall.add_rule(FirewallRule {
            name: "allow_vip".to_string(),
            rule_type: RuleType::SenderId("VIP".to_string()),
            action: FirewallAction::Allow,
            priority: 100,
            enabled: true,
            description: None,
            esme_id: None,
        });

        // VIP sender should pass even with blocked keyword
        let ctx = test_ctx("test message", "VIP", "+1234567890");
        assert!(firewall.check(&ctx).allowed);

        // Non-VIP should be blocked
        let ctx = test_ctx("test message", "REGULAR", "+1234567890");
        assert!(!firewall.check(&ctx).allowed);
    }

    #[test]
    fn test_event_logging() {
        let firewall = Firewall::new(FirewallConfig::default());

        // Trigger some checks
        let ctx = test_ctx("Hello", "SENDER", "+1234567890");
        firewall.check(&ctx);

        let stats = firewall.event_log().stats();
        assert_eq!(stats.allowed, 1);
    }

    #[test]
    fn test_combined_rules() {
        let mut firewall = Firewall::new(FirewallConfig::default());

        // Block messages that contain "promo" AND are to premium numbers
        firewall.add_rule(FirewallRule {
            name: "promo_premium".to_string(),
            rule_type: RuleType::All(vec![
                RuleType::Keyword("promo".to_string()),
                RuleType::DestinationPrefix("0900".to_string()),
            ]),
            action: FirewallAction::Block(Status::InvalidDestAddress),
            priority: 10,
            enabled: true,
            description: None,
            esme_id: None,
        });

        // Should block: contains promo AND premium number
        let ctx = test_ctx("Get this promo deal!", "SENDER", "0900123456");
        assert!(!firewall.check(&ctx).allowed);

        // Should allow: contains promo but not premium
        let ctx = test_ctx("Get this promo deal!", "SENDER", "+1234567890");
        assert!(firewall.check(&ctx).allowed);
    }
}
