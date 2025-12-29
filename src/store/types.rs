//! Types for the message store.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

// Re-export decision types for unified persistence
pub use crate::feedback::{DecisionContext, DecisionId, DecisionType};

/// Unique message identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MessageId(u64);

/// Global message ID counter (for recovery).
pub static MESSAGE_COUNTER: AtomicU64 = AtomicU64::new(0);

impl MessageId {
    /// Create a new unique message ID.
    pub fn new() -> Self {
        Self(MESSAGE_COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    /// Create a message ID from a raw value (for recovery).
    pub fn from_u64(id: u64) -> Self {
        Self(id)
    }

    /// Get the raw ID value.
    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

impl Default for MessageId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "msg_{}", self.0)
    }
}

/// Message state in the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageState {
    /// Message received, pending routing
    Pending,
    /// Message being processed (sent to upstream, awaiting response)
    InFlight,
    /// Message delivered successfully
    Delivered,
    /// Message delivery failed (permanent)
    Failed,
    /// Message expired (TTL exceeded)
    Expired,
    /// Message scheduled for retry
    Retrying,
}

impl MessageState {
    /// Check if the message is in a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Delivered | Self::Failed | Self::Expired)
    }

    /// Get the string name of this state.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InFlight => "in_flight",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
            Self::Expired => "expired",
            Self::Retrying => "retrying",
        }
    }
}

/// Message priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    /// Low priority (bulk, marketing)
    Low = 0,
    /// Normal priority (default)
    Normal = 1,
    /// High priority (transactional)
    High = 2,
    /// Critical priority (OTP, alerts)
    Critical = 3,
}

impl Default for Priority {
    fn default() -> Self {
        Self::Normal
    }
}

/// Event that occurred during message processing.
#[derive(Debug, Clone)]
pub struct MessageEvent {
    /// When the event occurred
    pub timestamp: Instant,
    /// Wall clock time
    pub wall_time: SystemTime,
    /// Event type
    pub event_type: EventType,
}

impl MessageEvent {
    /// Create a new event.
    pub fn new(event_type: EventType) -> Self {
        Self {
            timestamp: Instant::now(),
            wall_time: SystemTime::now(),
            event_type,
        }
    }
}

/// Type of event that occurred.
#[derive(Debug, Clone)]
pub enum EventType {
    /// Message was received from ESME
    Received {
        /// Source system_id
        system_id: String,
        /// Original sequence number
        sequence_number: u32,
    },
    /// Route decision was made
    RouteDecision {
        /// Decision ID for correlation
        decision_id: DecisionId,
        /// Selected route name
        route: String,
        /// Selected cluster
        cluster: String,
        /// Alternative clusters considered
        alternatives: Vec<String>,
    },
    /// Load balance decision was made
    LoadBalanceDecision {
        /// Decision ID for correlation
        decision_id: DecisionId,
        /// Selected endpoint
        endpoint: String,
        /// Cluster name
        cluster: String,
        /// Alternative endpoints considered
        alternatives: Vec<String>,
    },
    /// Message sent to upstream (submit_sm)
    Submitted {
        /// Attempt number
        attempt: u32,
        /// Target endpoint
        endpoint: String,
        /// Cluster name
        cluster: String,
    },
    /// Response received from upstream (submit_sm_resp)
    Response {
        /// SMPP command_status
        command_status: u32,
        /// SMSC message ID (if success)
        message_id: Option<String>,
        /// Latency in microseconds
        latency_us: u64,
    },
    /// Delivery receipt received (deliver_sm)
    DeliveryReceipt {
        /// Final delivery state
        state: String,
        /// Error code if failed
        error_code: Option<u32>,
    },
    /// Retry scheduled
    RetryScheduled {
        /// Retry delay
        delay_ms: u64,
        /// Reason for retry
        reason: String,
    },
    /// Fallback to another cluster
    Fallback {
        /// Decision ID for correlation
        decision_id: DecisionId,
        /// Failed cluster
        from_cluster: String,
        /// New cluster
        to_cluster: String,
        /// Remaining fallbacks
        remaining: Vec<String>,
    },
    /// State changed
    StateChange {
        /// Previous state
        from: MessageState,
        /// New state
        to: MessageState,
    },
    /// Message expired
    Expired,
    /// Message pruned from store
    Pruned,
}

impl EventType {
    /// Get the name of this event type.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Received { .. } => "received",
            Self::RouteDecision { .. } => "route_decision",
            Self::LoadBalanceDecision { .. } => "lb_decision",
            Self::Submitted { .. } => "submitted",
            Self::Response { .. } => "response",
            Self::DeliveryReceipt { .. } => "delivery_receipt",
            Self::RetryScheduled { .. } => "retry_scheduled",
            Self::Fallback { .. } => "fallback",
            Self::StateChange { .. } => "state_change",
            Self::Expired => "expired",
            Self::Pruned => "pruned",
        }
    }

    /// Check if this is a decision event.
    pub fn is_decision(&self) -> bool {
        matches!(
            self,
            Self::RouteDecision { .. } | Self::LoadBalanceDecision { .. } | Self::Fallback { .. }
        )
    }

    /// Get decision ID if this is a decision event.
    pub fn decision_id(&self) -> Option<DecisionId> {
        match self {
            Self::RouteDecision { decision_id, .. }
            | Self::LoadBalanceDecision { decision_id, .. }
            | Self::Fallback { decision_id, .. } => Some(*decision_id),
            _ => None,
        }
    }
}

/// A stored message.
#[derive(Debug, Clone)]
pub struct StoredMessage {
    /// Unique message ID
    pub id: MessageId,
    /// Current state
    pub state: MessageState,
    /// Message priority
    pub priority: Priority,
    /// Source address (sender)
    pub source: String,
    /// Destination address (recipient)
    pub destination: String,
    /// Short message text
    pub short_message: Vec<u8>,
    /// Data coding scheme
    pub data_coding: u8,
    /// ESM class (message mode/type)
    pub esm_class: u8,
    /// Registered delivery flags
    pub registered_delivery: u8,
    /// Service type
    pub service_type: Option<String>,
    /// Sender ID (source address TON/NPI encoded)
    pub sender_id: Option<String>,
    /// System ID of the submitting ESME
    pub system_id: String,
    /// Sequence number from original PDU
    pub sequence_number: u32,
    /// SMSC message ID (if assigned)
    pub smsc_message_id: Option<String>,
    /// Selected route name
    pub route: Option<String>,
    /// Target cluster name
    pub cluster: Option<String>,
    /// Target endpoint address
    pub endpoint: Option<String>,
    /// Number of delivery attempts
    pub attempts: u32,
    /// Maximum retry attempts
    pub max_attempts: u32,
    /// Time to live
    pub ttl: Duration,
    /// When the message was received
    pub created_at: Instant,
    /// Wall clock time when created
    pub created_time: SystemTime,
    /// When the message was last updated
    pub updated_at: Instant,
    /// When to retry (if in Retrying state)
    pub retry_at: Option<Instant>,
    /// Error code from last attempt
    pub last_error_code: Option<u32>,
    /// Error message from last attempt
    pub last_error: Option<String>,
    /// Event history (all decisions, state changes, attempts)
    pub events: Vec<MessageEvent>,
}

impl StoredMessage {
    /// Create a new pending message.
    pub fn new(
        source: impl Into<String>,
        destination: impl Into<String>,
        short_message: Vec<u8>,
        system_id: impl Into<String>,
        sequence_number: u32,
    ) -> Self {
        let now = Instant::now();
        let sys_id = system_id.into();
        let received_event = MessageEvent::new(EventType::Received {
            system_id: sys_id.clone(),
            sequence_number,
        });

        Self {
            id: MessageId::new(),
            state: MessageState::Pending,
            priority: Priority::Normal,
            source: source.into(),
            destination: destination.into(),
            short_message,
            data_coding: 0,
            esm_class: 0,
            registered_delivery: 0,
            service_type: None,
            sender_id: None,
            system_id: sys_id,
            sequence_number,
            smsc_message_id: None,
            route: None,
            cluster: None,
            endpoint: None,
            attempts: 0,
            max_attempts: 3,
            ttl: Duration::from_secs(3600), // 1 hour default
            created_at: now,
            created_time: SystemTime::now(),
            updated_at: now,
            retry_at: None,
            last_error_code: None,
            last_error: None,
            events: vec![received_event],
        }
    }

    /// Add an event to the message history.
    pub fn add_event(&mut self, event_type: EventType) {
        self.events.push(MessageEvent::new(event_type));
        self.updated_at = Instant::now();
    }

    /// Get all decision events.
    pub fn decisions(&self) -> impl Iterator<Item = &MessageEvent> {
        self.events.iter().filter(|e| e.event_type.is_decision())
    }

    /// Get the last response latency in microseconds.
    pub fn last_latency_us(&self) -> Option<u64> {
        self.events.iter().rev().find_map(|e| {
            if let EventType::Response { latency_us, .. } = e.event_type {
                Some(latency_us)
            } else {
                None
            }
        })
    }

    /// Set the priority.
    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    /// Set data coding.
    pub fn with_data_coding(mut self, data_coding: u8) -> Self {
        self.data_coding = data_coding;
        self
    }

    /// Set ESM class.
    pub fn with_esm_class(mut self, esm_class: u8) -> Self {
        self.esm_class = esm_class;
        self
    }

    /// Set registered delivery.
    pub fn with_registered_delivery(mut self, rd: u8) -> Self {
        self.registered_delivery = rd;
        self
    }

    /// Set service type.
    pub fn with_service_type(mut self, service_type: impl Into<String>) -> Self {
        self.service_type = Some(service_type.into());
        self
    }

    /// Set sender ID.
    pub fn with_sender_id(mut self, sender_id: impl Into<String>) -> Self {
        self.sender_id = Some(sender_id.into());
        self
    }

    /// Set TTL.
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Set max attempts.
    pub fn with_max_attempts(mut self, max: u32) -> Self {
        self.max_attempts = max;
        self
    }

    /// Check if the message has expired.
    pub fn is_expired(&self) -> bool {
        self.created_at.elapsed() > self.ttl
    }

    /// Check if more retries are allowed.
    pub fn can_retry(&self) -> bool {
        self.attempts < self.max_attempts && !self.is_expired()
    }

    /// Record a route decision.
    pub fn record_route_decision(
        &mut self,
        decision_id: DecisionId,
        route: &str,
        cluster: &str,
        alternatives: Vec<String>,
    ) {
        self.route = Some(route.to_string());
        self.cluster = Some(cluster.to_string());
        self.add_event(EventType::RouteDecision {
            decision_id,
            route: route.to_string(),
            cluster: cluster.to_string(),
            alternatives,
        });
    }

    /// Record a load balance decision.
    pub fn record_lb_decision(
        &mut self,
        decision_id: DecisionId,
        endpoint: &str,
        cluster: &str,
        alternatives: Vec<String>,
    ) {
        self.endpoint = Some(endpoint.to_string());
        self.add_event(EventType::LoadBalanceDecision {
            decision_id,
            endpoint: endpoint.to_string(),
            cluster: cluster.to_string(),
            alternatives,
        });
    }

    /// Record a fallback decision.
    pub fn record_fallback(
        &mut self,
        decision_id: DecisionId,
        from_cluster: &str,
        to_cluster: &str,
        remaining: Vec<String>,
    ) {
        self.cluster = Some(to_cluster.to_string());
        self.add_event(EventType::Fallback {
            decision_id,
            from_cluster: from_cluster.to_string(),
            to_cluster: to_cluster.to_string(),
            remaining,
        });
    }

    /// Mark as in-flight (being sent to upstream).
    pub fn mark_in_flight(&mut self, cluster: &str, endpoint: &str) {
        let old_state = self.state;
        self.state = MessageState::InFlight;
        self.cluster = Some(cluster.to_string());
        self.endpoint = Some(endpoint.to_string());
        self.attempts += 1;
        self.updated_at = Instant::now();

        self.events.push(MessageEvent::new(EventType::Submitted {
            attempt: self.attempts,
            endpoint: endpoint.to_string(),
            cluster: cluster.to_string(),
        }));

        if old_state != self.state {
            self.events.push(MessageEvent::new(EventType::StateChange {
                from: old_state,
                to: self.state,
            }));
        }
    }

    /// Record a response from upstream.
    pub fn record_response(&mut self, command_status: u32, message_id: Option<String>, latency_us: u64) {
        self.add_event(EventType::Response {
            command_status,
            message_id,
            latency_us,
        });
    }

    /// Mark as delivered.
    pub fn mark_delivered(&mut self, smsc_message_id: impl Into<String>) {
        let old_state = self.state;
        self.state = MessageState::Delivered;
        self.smsc_message_id = Some(smsc_message_id.into());
        self.updated_at = Instant::now();
        self.last_error = None;
        self.last_error_code = None;

        if old_state != self.state {
            self.events.push(MessageEvent::new(EventType::StateChange {
                from: old_state,
                to: self.state,
            }));
        }
    }

    /// Mark as failed (permanent).
    pub fn mark_failed(&mut self, error_code: Option<u32>, error: impl Into<String>) {
        let old_state = self.state;
        self.state = MessageState::Failed;
        self.last_error_code = error_code;
        self.last_error = Some(error.into());
        self.updated_at = Instant::now();

        if old_state != self.state {
            self.events.push(MessageEvent::new(EventType::StateChange {
                from: old_state,
                to: self.state,
            }));
        }
    }

    /// Mark for retry.
    pub fn mark_retry(&mut self, delay: Duration, error_code: Option<u32>, error: impl Into<String>) {
        let old_state = self.state;
        let err_str = error.into();
        self.state = MessageState::Retrying;
        self.retry_at = Some(Instant::now() + delay);
        self.last_error_code = error_code;
        self.last_error = Some(err_str.clone());
        self.updated_at = Instant::now();

        self.events.push(MessageEvent::new(EventType::RetryScheduled {
            delay_ms: delay.as_millis() as u64,
            reason: err_str,
        }));

        if old_state != self.state {
            self.events.push(MessageEvent::new(EventType::StateChange {
                from: old_state,
                to: self.state,
            }));
        }
    }

    /// Mark as expired.
    pub fn mark_expired(&mut self) {
        let old_state = self.state;
        self.state = MessageState::Expired;
        self.updated_at = Instant::now();

        self.events.push(MessageEvent::new(EventType::Expired));

        if old_state != self.state {
            self.events.push(MessageEvent::new(EventType::StateChange {
                from: old_state,
                to: self.state,
            }));
        }
    }

    /// Move from Retrying back to Pending for re-processing.
    pub fn reset_to_pending(&mut self) {
        if self.state == MessageState::Retrying {
            let old_state = self.state;
            self.state = MessageState::Pending;
            self.retry_at = None;
            self.updated_at = Instant::now();

            self.events.push(MessageEvent::new(EventType::StateChange {
                from: old_state,
                to: self.state,
            }));
        }
    }

    /// Record delivery receipt.
    pub fn record_delivery_receipt(&mut self, state: &str, error_code: Option<u32>) {
        self.add_event(EventType::DeliveryReceipt {
            state: state.to_string(),
            error_code,
        });
    }
}

/// Query filter for messages.
#[derive(Debug, Clone, Default)]
pub struct MessageQuery {
    /// Filter by state
    pub state: Option<MessageState>,
    /// Filter by system_id
    pub system_id: Option<String>,
    /// Filter by destination prefix
    pub destination_prefix: Option<String>,
    /// Filter by cluster
    pub cluster: Option<String>,
    /// Filter messages created after this time
    pub created_after: Option<Instant>,
    /// Maximum number of results
    pub limit: Option<usize>,
}

impl MessageQuery {
    /// Create a new query.
    pub fn new() -> Self {
        Self::default()
    }

    /// Filter by state.
    pub fn with_state(mut self, state: MessageState) -> Self {
        self.state = Some(state);
        self
    }

    /// Filter by system_id.
    pub fn with_system_id(mut self, system_id: impl Into<String>) -> Self {
        self.system_id = Some(system_id.into());
        self
    }

    /// Filter by destination prefix.
    pub fn with_destination_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.destination_prefix = Some(prefix.into());
        self
    }

    /// Filter by cluster.
    pub fn with_cluster(mut self, cluster: impl Into<String>) -> Self {
        self.cluster = Some(cluster.into());
        self
    }

    /// Set limit.
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }
}

/// Store statistics.
#[derive(Debug, Clone, Default)]
pub struct StoreStats {
    /// Total messages in store
    pub total: u64,
    /// Pending messages
    pub pending: u64,
    /// In-flight messages
    pub in_flight: u64,
    /// Delivered messages (may be pruned)
    pub delivered: u64,
    /// Failed messages
    pub failed: u64,
    /// Expired messages
    pub expired: u64,
    /// Retrying messages
    pub retrying: u64,
    /// Estimated memory usage in bytes
    pub memory_bytes: u64,
    /// Configured maximum messages
    pub max_messages: u64,
    /// Configured maximum memory in bytes
    pub max_memory_bytes: u64,
}

// =============================================================================
// CDR Types
// =============================================================================

/// Query filter for CDRs.
#[derive(Debug, Clone, Default)]
pub struct CdrQuery {
    /// Filter by ESME system_id
    pub esme_id: Option<String>,
    /// Filter by message ID
    pub message_id: Option<String>,
    /// Filter by destination prefix
    pub destination_prefix: Option<String>,
    /// Filter by route
    pub route: Option<String>,
    /// Filter by status code
    pub status_code: Option<u32>,
    /// Filter CDRs created after this time
    pub after: Option<SystemTime>,
    /// Filter CDRs created before this time
    pub before: Option<SystemTime>,
    /// Maximum number of results
    pub limit: Option<usize>,
}

impl CdrQuery {
    /// Create a new query.
    pub fn new() -> Self {
        Self::default()
    }

    /// Filter by ESME.
    pub fn with_esme(mut self, esme_id: impl Into<String>) -> Self {
        self.esme_id = Some(esme_id.into());
        self
    }

    /// Filter by message ID.
    pub fn with_message_id(mut self, id: impl Into<String>) -> Self {
        self.message_id = Some(id.into());
        self
    }

    /// Set limit.
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }
}

/// CDR statistics.
#[derive(Debug, Clone, Default)]
pub struct CdrStats {
    /// Total CDRs stored
    pub total: u64,
    /// CDRs by type
    pub by_type: std::collections::HashMap<String, u64>,
    /// Total revenue
    pub total_revenue: i64,
    /// Total cost
    pub total_cost: i64,
    /// Currency (if consistent)
    pub currency: Option<String>,
}

// =============================================================================
// Rate Limit Types
// =============================================================================

/// Rate limit state for persistence.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RateLimitState {
    /// Current token count
    pub tokens: u64,
    /// Last refill timestamp (Unix millis)
    pub last_refill_ms: u64,
    /// Token rate (per second)
    pub rate: u32,
    /// Maximum capacity
    pub capacity: u32,
}

impl RateLimitState {
    /// Create a new rate limit state.
    pub fn new(rate: u32, capacity: u32) -> Self {
        Self {
            tokens: capacity as u64,
            last_refill_ms: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            rate,
            capacity,
        }
    }

    /// Create from existing state.
    pub fn with_tokens(mut self, tokens: u64, last_refill_ms: u64) -> Self {
        self.tokens = tokens;
        self.last_refill_ms = last_refill_ms;
        self
    }
}

// =============================================================================
// Firewall Types
// =============================================================================

/// ESME firewall settings for persistence.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct FirewallSettings {
    /// Allowed destination patterns
    pub destination_allow: Vec<String>,
    /// Denied destination patterns
    pub destination_deny: Vec<String>,
    /// Allowed source patterns
    pub source_allow: Vec<String>,
    /// Denied source patterns
    pub source_deny: Vec<String>,
    /// Allowed sender IDs
    pub sender_allow: Vec<String>,
    /// Denied sender IDs
    pub sender_deny: Vec<String>,
    /// Blocked keywords
    pub blocked_keywords: Vec<String>,
    /// Blocked patterns (regex)
    pub blocked_patterns: Vec<String>,
    /// Time window restrictions (JSON for flexibility)
    pub time_restrictions: Option<String>,
    /// Last updated timestamp
    pub updated_at: Option<SystemTime>,
}

impl FirewallSettings {
    /// Create new empty settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a destination allow pattern.
    pub fn allow_destination(mut self, pattern: impl Into<String>) -> Self {
        self.destination_allow.push(pattern.into());
        self
    }

    /// Add a destination deny pattern.
    pub fn deny_destination(mut self, pattern: impl Into<String>) -> Self {
        self.destination_deny.push(pattern.into());
        self
    }

    /// Add a blocked keyword.
    pub fn block_keyword(mut self, keyword: impl Into<String>) -> Self {
        self.blocked_keywords.push(keyword.into());
        self
    }
}

/// Firewall event for audit logging.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FirewallEvent {
    /// Event ID
    pub id: u64,
    /// Timestamp
    pub timestamp: SystemTime,
    /// ESME system_id
    pub esme_id: String,
    /// Source address
    pub source: String,
    /// Destination address
    pub destination: String,
    /// Action taken
    pub action: FirewallAction,
    /// Rule that matched
    pub rule: Option<String>,
    /// Reason for action
    pub reason: Option<String>,
    /// Message content preview (truncated)
    pub content_preview: Option<String>,
}

/// Global firewall event counter.
pub static FIREWALL_EVENT_COUNTER: AtomicU64 = AtomicU64::new(0);

impl FirewallEvent {
    /// Create a new firewall event.
    pub fn new(
        esme_id: impl Into<String>,
        source: impl Into<String>,
        destination: impl Into<String>,
        action: FirewallAction,
    ) -> Self {
        Self {
            id: FIREWALL_EVENT_COUNTER.fetch_add(1, Ordering::Relaxed),
            timestamp: SystemTime::now(),
            esme_id: esme_id.into(),
            source: source.into(),
            destination: destination.into(),
            action,
            rule: None,
            reason: None,
            content_preview: None,
        }
    }

    /// Set the matching rule.
    pub fn with_rule(mut self, rule: impl Into<String>) -> Self {
        self.rule = Some(rule.into());
        self
    }

    /// Set the reason.
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

/// Firewall action taken.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FirewallAction {
    /// Message was allowed
    Allow,
    /// Message was blocked
    Block,
    /// Message was logged only
    Log,
    /// Message was quarantined
    Quarantine,
    /// Alert was raised
    Alert,
}

impl FirewallAction {
    /// Get action name.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Block => "block",
            Self::Log => "log",
            Self::Quarantine => "quarantine",
            Self::Alert => "alert",
        }
    }
}

/// Query filter for firewall events.
#[derive(Debug, Clone, Default)]
pub struct FirewallEventQuery {
    /// Filter by ESME system_id
    pub esme_id: Option<String>,
    /// Filter by action
    pub action: Option<FirewallAction>,
    /// Filter by destination prefix
    pub destination_prefix: Option<String>,
    /// Filter events after this time
    pub after: Option<SystemTime>,
    /// Filter events before this time
    pub before: Option<SystemTime>,
    /// Maximum number of results
    pub limit: Option<usize>,
}

impl FirewallEventQuery {
    /// Create a new query.
    pub fn new() -> Self {
        Self::default()
    }

    /// Filter by ESME.
    pub fn with_esme(mut self, esme_id: impl Into<String>) -> Self {
        self.esme_id = Some(esme_id.into());
        self
    }

    /// Filter by action.
    pub fn with_action(mut self, action: FirewallAction) -> Self {
        self.action = Some(action);
        self
    }

    /// Set limit.
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }
}
