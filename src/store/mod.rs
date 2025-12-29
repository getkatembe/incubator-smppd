//! Unified storage for all gateway state.
//!
//! All persistent gateway state is managed through the [`Storage`] trait:
//! - **Messages**: Store-and-forward, retry management, state tracking
//! - **Feedback**: Decision recording and outcome tracking for adaptive routing
//! - **CDRs**: Call Detail Records for billing and audit
//! - **Schedules**: Scheduled message delivery
//! - **Rate Limits**: Token bucket state for rate limiting
//! - **Firewall**: ESME firewall settings and event logs
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────┐
//! │                           Storage                                 │
//! │  ┌───────────┐  ┌──────────┐  ┌─────┐  ┌──────────┐  ┌────────┐ │
//! │  │ Messages  │  │ Feedback │  │ CDR │  │ Schedule │  │  Rate  │ │
//! │  │           │  │          │  │     │  │          │  │ Limits │ │
//! │  └───────────┘  └──────────┘  └─────┘  └──────────┘  └────────┘ │
//! │  ┌───────────────────────────────────────────────────────────┐  │
//! │  │                       Firewall                             │  │
//! │  └───────────────────────────────────────────────────────────┘  │
//! └──────────────────────────────────────────────────────────────────┘
//!                                │
//!              ┌─────────────────┼─────────────────┐
//!              ▼                 ▼                 ▼
//!        ┌──────────┐      ┌──────────┐     ┌──────────────┐
//!        │  Memory  │      │  Fjall   │     │  (Future)    │
//!        │  (dev)   │      │ (prod)   │     │ PostgreSQL   │
//!        └──────────┘      └──────────┘     └──────────────┘
//! ```
//!
//! # Implementations
//!
//! - [`MemoryStorage`]: In-memory, volatile - for development/testing
//! - [`PersistentStorage`]: Fjall-backed, durable - for production

mod factory;
mod memory;
mod persistent;
pub mod scheduler;
pub mod types;

pub use factory::create_storage;
pub use memory::MemoryStorage;
pub use persistent::PersistentStorage;
pub use scheduler::{
    RecurrenceFrequency, RecurrenceRule, ScheduleStatus, ScheduledMessage, Scheduler,
    SchedulerConfig, SchedulerError, SchedulerHandle, SchedulerStats,
};
pub use types::*;

use std::sync::Arc;
use std::time::Duration;

use crate::cdr::Cdr;
use crate::feedback::{Decision, DecisionStats, Outcome, TargetStats};

// =============================================================================
// Storage Trait
// =============================================================================

/// Unified storage trait for all gateway state.
///
/// Combines message storage and feedback tracking into a single interface.
/// All implementations must be thread-safe (Send + Sync).
///
/// # Design Rationale
///
/// A unified trait ensures:
/// - Consistent API across backends
/// - Atomic operations on related data (messages + decisions)
/// - Simpler configuration and initialization
/// - Single recovery path on restart
pub trait Storage: Send + Sync {
    // -------------------------------------------------------------------------
    // Message Operations
    // -------------------------------------------------------------------------

    /// Store a new message. Returns the message ID.
    fn store(&self, message: StoredMessage) -> MessageId;

    /// Get a message by ID.
    fn get(&self, id: MessageId) -> Option<StoredMessage>;

    /// Update a message in place using a closure.
    fn update(&self, id: MessageId, f: Box<dyn FnOnce(&mut StoredMessage) + Send>) -> bool;

    /// Delete a message by ID.
    fn delete(&self, id: MessageId) -> bool;

    /// Query messages matching criteria.
    fn query(&self, query: &MessageQuery) -> Vec<StoredMessage>;

    /// Get pending messages, sorted by priority and age.
    fn get_pending(&self, limit: usize) -> Vec<StoredMessage>;

    /// Get messages ready for retry (retry_at <= now).
    fn get_ready_for_retry(&self) -> Vec<StoredMessage>;

    /// Expire messages older than TTL.
    fn expire_old(&self, ttl: Duration) -> u64;

    /// Prune terminal messages older than max_age.
    fn prune_terminal(&self, max_age: Duration) -> u64;

    /// Get message store statistics.
    fn message_stats(&self) -> StoreStats;

    // -------------------------------------------------------------------------
    // Feedback Operations
    // -------------------------------------------------------------------------

    /// Record a routing/load-balancing decision. Returns decision ID.
    fn record_decision(&self, decision: Decision) -> DecisionId;

    /// Record the outcome of a decision.
    fn record_outcome(&self, outcome: Outcome);

    /// Get statistics for a decision type.
    fn decision_stats(&self, decision_type: DecisionType) -> DecisionStats;

    /// Get statistics for a specific target (endpoint, cluster, route).
    fn target_stats(&self, target: &str) -> TargetStats;

    /// Get all targets with their stats, sorted by success rate.
    fn all_target_stats(&self) -> Vec<(String, TargetStats)>;

    /// Prune old feedback data.
    fn prune_feedback(&self, max_age: Duration);

    // -------------------------------------------------------------------------
    // CDR Operations
    // -------------------------------------------------------------------------

    /// Store a CDR record.
    fn store_cdr(&self, cdr: Cdr);

    /// Query CDRs by criteria.
    fn query_cdrs(&self, query: &CdrQuery) -> Vec<Cdr>;

    /// Get CDR statistics.
    fn cdr_stats(&self) -> CdrStats;

    /// Prune old CDRs.
    fn prune_cdrs(&self, max_age: Duration) -> u64;

    // -------------------------------------------------------------------------
    // Schedule Operations
    // -------------------------------------------------------------------------

    /// Store a scheduled message.
    fn store_schedule(&self, schedule: ScheduledMessage) -> String;

    /// Get a schedule by ID.
    fn get_schedule(&self, id: &str) -> Option<ScheduledMessage>;

    /// Update a schedule.
    fn update_schedule(&self, id: &str, f: Box<dyn FnOnce(&mut ScheduledMessage) + Send>) -> bool;

    /// Delete a schedule.
    fn delete_schedule(&self, id: &str) -> bool;

    /// Get schedules due for delivery.
    fn get_due_schedules(&self, limit: usize) -> Vec<ScheduledMessage>;

    /// Get all schedules for an ESME.
    fn get_esme_schedules(&self, esme_id: &str) -> Vec<ScheduledMessage>;

    // -------------------------------------------------------------------------
    // Rate Limit Operations
    // -------------------------------------------------------------------------

    /// Store rate limit state for a client.
    fn store_rate_limit(&self, client_id: &str, state: RateLimitState);

    /// Get rate limit state for a client.
    fn get_rate_limit(&self, client_id: &str) -> Option<RateLimitState>;

    /// Delete rate limit state.
    fn delete_rate_limit(&self, client_id: &str) -> bool;

    // -------------------------------------------------------------------------
    // Firewall Operations
    // -------------------------------------------------------------------------

    /// Store ESME firewall settings.
    fn store_firewall_settings(&self, esme_id: &str, settings: FirewallSettings);

    /// Get ESME firewall settings.
    fn get_firewall_settings(&self, esme_id: &str) -> Option<FirewallSettings>;

    /// Store a firewall event.
    fn store_firewall_event(&self, event: FirewallEvent);

    /// Query firewall events.
    fn query_firewall_events(&self, query: &FirewallEventQuery) -> Vec<FirewallEvent>;

    /// Prune old firewall events.
    fn prune_firewall_events(&self, max_age: Duration) -> u64;

    // -------------------------------------------------------------------------
    // Recovery & Maintenance
    // -------------------------------------------------------------------------

    /// Get messages that need recovery after restart (pending, in-flight, retrying).
    fn get_recoverable_messages(&self) -> Vec<StoredMessage> {
        let mut messages = self.query(&MessageQuery {
            state: Some(MessageState::Pending),
            ..Default::default()
        });
        messages.extend(self.query(&MessageQuery {
            state: Some(MessageState::InFlight),
            ..Default::default()
        }));
        messages.extend(self.query(&MessageQuery {
            state: Some(MessageState::Retrying),
            ..Default::default()
        }));
        messages
    }

    /// Run periodic maintenance (expiration, pruning).
    fn maintenance(&self, message_ttl: Duration, prune_age: Duration) {
        let expired = self.expire_old(message_ttl);
        if expired > 0 {
            tracing::debug!(expired, "expired old messages");
        }

        let pruned = self.prune_terminal(prune_age);
        if pruned > 0 {
            tracing::debug!(pruned, "pruned terminal messages");
        }

        self.prune_feedback(prune_age);
    }

    /// Flush pending writes to disk (no-op for in-memory).
    fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Shared storage handle.
pub type SharedStorage = Arc<dyn Storage>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_state_terminal() {
        assert!(!MessageState::Pending.is_terminal());
        assert!(!MessageState::InFlight.is_terminal());
        assert!(MessageState::Delivered.is_terminal());
        assert!(MessageState::Failed.is_terminal());
        assert!(MessageState::Expired.is_terminal());
        assert!(!MessageState::Retrying.is_terminal());
    }

    #[test]
    fn test_message_state_name() {
        assert_eq!(MessageState::Pending.name(), "pending");
        assert_eq!(MessageState::InFlight.name(), "in_flight");
        assert_eq!(MessageState::Delivered.name(), "delivered");
        assert_eq!(MessageState::Failed.name(), "failed");
        assert_eq!(MessageState::Expired.name(), "expired");
        assert_eq!(MessageState::Retrying.name(), "retrying");
    }

    #[test]
    fn test_priority_ordering() {
        assert!(Priority::Critical > Priority::High);
        assert!(Priority::High > Priority::Normal);
        assert!(Priority::Normal > Priority::Low);
    }

    #[test]
    fn test_message_expiry() {
        let msg = StoredMessage::new("+258", "+258", b"test".to_vec(), "sys", 1)
            .with_ttl(Duration::ZERO);
        assert!(msg.is_expired());
    }

    #[test]
    fn test_message_can_retry() {
        let mut msg = StoredMessage::new("+258", "+258", b"test".to_vec(), "sys", 1)
            .with_max_attempts(2);

        assert!(msg.can_retry());
        msg.attempts = 1;
        assert!(msg.can_retry());
        msg.attempts = 2;
        assert!(!msg.can_retry());
    }

    #[test]
    fn test_message_events_recorded() {
        let msg =
            StoredMessage::new("+258841234567", "+258821234567", b"Hello".to_vec(), "sys001", 1);
        assert_eq!(msg.events.len(), 1);
        assert!(matches!(msg.events[0].event_type, EventType::Received { .. }));
    }

    #[test]
    fn test_message_lifecycle_events() {
        let mut msg = StoredMessage::new("+258", "+258", b"test".to_vec(), "sys", 1);
        let initial_events = msg.events.len();

        msg.record_route_decision(
            crate::feedback::DecisionId::new(),
            "vodacom-route",
            "vodacom",
            vec!["mtn".into()],
        );
        assert!(msg.events.len() > initial_events);

        msg.record_lb_decision(
            crate::feedback::DecisionId::new(),
            "smsc1:2775",
            "vodacom",
            vec!["smsc2:2775".into()],
        );

        msg.mark_in_flight("vodacom", "smsc1:2775");
        msg.record_response(0, Some("MSG123".into()), 50_000);
        msg.mark_delivered("MSG123");

        assert!(msg.events.len() >= 6);

        let decisions: Vec<_> = msg.decisions().collect();
        assert_eq!(decisions.len(), 2);
    }

    #[test]
    fn test_event_type_name() {
        assert_eq!(EventType::Expired.name(), "expired");
        assert_eq!(EventType::Pruned.name(), "pruned");
    }
}
