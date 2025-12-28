//! Message store for store-and-forward functionality.
//!
//! Provides persistence and state management for SMS messages flowing
//! through the gateway. Enables:
//!
//! - **Store-and-forward**: Persist messages before delivery for reliability
//! - **Retry management**: Track failed deliveries and schedule retries
//! - **Message tracking**: Query message state and history
//! - **Expiration**: Automatic cleanup of old messages
//!
//! # Architecture
//!
//! ```text
//! ESME → submit_sm → MessageStore (Pending)
//!                         ↓
//!                    Router/LB
//!                         ↓
//!                    MessageStore (InFlight)
//!                         ↓
//!              ┌──────────┴──────────┐
//!              ↓                     ↓
//!         Success              Failure
//!              ↓                     ↓
//!      MessageStore          MessageStore
//!       (Delivered)      (Retrying or Failed)
//! ```
//!
//! # Implementations
//!
//! - [`InMemoryStore`]: Development and testing (default)
//! - Future: RocksDB, PostgreSQL, Redis backends

mod memory;
mod types;

pub use memory::InMemoryStore;
pub use types::*;

use std::sync::Arc;
use std::time::Duration;

/// Message store trait for persistence backends.
///
/// Implementations must be thread-safe (Send + Sync).
pub trait MessageStore: Send + Sync {
    /// Store a new message. Returns the message ID.
    fn store(&self, message: StoredMessage) -> MessageId;

    /// Get a message by ID.
    fn get(&self, id: MessageId) -> Option<StoredMessage>;

    /// Update a message in place.
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

    /// Get store statistics.
    fn stats(&self) -> StoreStats;
}

/// Shared message store.
pub type SharedStore = Arc<dyn MessageStore>;

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

        // With 0 TTL, immediately expired
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
        let msg = StoredMessage::new("+258841234567", "+258821234567", b"Hello".to_vec(), "sys001", 1);

        // Should have initial Received event
        assert_eq!(msg.events.len(), 1);
        assert!(matches!(msg.events[0].event_type, EventType::Received { .. }));
    }

    #[test]
    fn test_message_lifecycle_events() {
        let mut msg = StoredMessage::new("+258", "+258", b"test".to_vec(), "sys", 1);
        let initial_events = msg.events.len();

        // Record route decision
        msg.record_route_decision(
            crate::feedback::DecisionId::new(),
            "vodacom-route",
            "vodacom",
            vec!["mtn".into()],
        );
        assert!(msg.events.len() > initial_events);

        // Record LB decision
        msg.record_lb_decision(
            crate::feedback::DecisionId::new(),
            "smsc1:2775",
            "vodacom",
            vec!["smsc2:2775".into()],
        );

        // Mark in-flight (generates Submitted + StateChange events)
        msg.mark_in_flight("vodacom", "smsc1:2775");

        // Record response
        msg.record_response(0, Some("MSG123".into()), 50_000);

        // Mark delivered
        msg.mark_delivered("MSG123");

        // Should have many events now
        assert!(msg.events.len() >= 6);

        // Check decision count
        let decisions: Vec<_> = msg.decisions().collect();
        assert_eq!(decisions.len(), 2); // route + LB
    }

    #[test]
    fn test_event_type_name() {
        assert_eq!(EventType::Expired.name(), "expired");
        assert_eq!(EventType::Pruned.name(), "pruned");
    }
}
