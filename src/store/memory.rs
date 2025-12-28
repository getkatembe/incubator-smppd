//! In-memory message store implementation.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use super::types::*;
use super::MessageStore;

/// Maximum messages to keep in memory.
const MAX_MESSAGES: usize = 1_000_000;

/// In-memory message store.
///
/// Thread-safe implementation using RwLock. Suitable for development
/// and single-node deployments. For production, consider RocksDB backend.
pub struct InMemoryStore {
    /// All messages indexed by ID
    messages: RwLock<HashMap<MessageId, StoredMessage>>,
    /// Messages ready for retry (sorted by retry_at)
    retry_queue: RwLock<Vec<MessageId>>,
}

impl InMemoryStore {
    /// Create a new in-memory store.
    pub fn new() -> Self {
        Self {
            messages: RwLock::new(HashMap::new()),
            retry_queue: RwLock::new(Vec::new()),
        }
    }

    /// Get the number of messages in the store.
    pub fn len(&self) -> usize {
        self.messages.read().unwrap().len()
    }

    /// Check if the store is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MessageStore for InMemoryStore {
    fn store(&self, message: StoredMessage) -> MessageId {
        let id = message.id;

        let mut messages = self.messages.write().unwrap();

        // Prune if at capacity
        if messages.len() >= MAX_MESSAGES {
            // Remove oldest terminal messages first
            let to_remove: Vec<_> = messages
                .iter()
                .filter(|(_, m)| m.state.is_terminal())
                .take(MAX_MESSAGES / 10)
                .map(|(k, _)| *k)
                .collect();

            for k in to_remove {
                messages.remove(&k);
            }
        }

        messages.insert(id, message);
        id
    }

    fn get(&self, id: MessageId) -> Option<StoredMessage> {
        self.messages.read().unwrap().get(&id).cloned()
    }

    fn update(&self, id: MessageId, f: Box<dyn FnOnce(&mut StoredMessage) + Send>) -> bool {
        let mut messages = self.messages.write().unwrap();
        if let Some(msg) = messages.get_mut(&id) {
            let old_state = msg.state;
            f(msg);

            // Update retry queue if transitioning to/from Retrying
            if old_state != msg.state {
                let mut retry_queue = self.retry_queue.write().unwrap();
                if msg.state == MessageState::Retrying {
                    if !retry_queue.contains(&id) {
                        retry_queue.push(id);
                    }
                } else {
                    retry_queue.retain(|&x| x != id);
                }
            }

            true
        } else {
            false
        }
    }

    fn delete(&self, id: MessageId) -> bool {
        let mut messages = self.messages.write().unwrap();
        let removed = messages.remove(&id).is_some();

        if removed {
            let mut retry_queue = self.retry_queue.write().unwrap();
            retry_queue.retain(|&x| x != id);
        }

        removed
    }

    fn query(&self, query: &MessageQuery) -> Vec<StoredMessage> {
        let messages = self.messages.read().unwrap();
        let limit = query.limit.unwrap_or(1000);

        messages
            .values()
            .filter(|m| {
                if let Some(state) = query.state {
                    if m.state != state {
                        return false;
                    }
                }
                if let Some(ref sys_id) = query.system_id {
                    if &m.system_id != sys_id {
                        return false;
                    }
                }
                if let Some(ref prefix) = query.destination_prefix {
                    if !m.destination.starts_with(prefix) {
                        return false;
                    }
                }
                if let Some(ref cluster) = query.cluster {
                    if m.cluster.as_ref() != Some(cluster) {
                        return false;
                    }
                }
                if let Some(after) = query.created_after {
                    if m.created_at < after {
                        return false;
                    }
                }
                true
            })
            .take(limit)
            .cloned()
            .collect()
    }

    fn get_pending(&self, limit: usize) -> Vec<StoredMessage> {
        let messages = self.messages.read().unwrap();

        let mut pending: Vec<_> = messages
            .values()
            .filter(|m| m.state == MessageState::Pending)
            .cloned()
            .collect();

        // Sort by priority (descending) then by created_at (ascending)
        pending.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.created_at.cmp(&b.created_at))
        });

        pending.into_iter().take(limit).collect()
    }

    fn get_ready_for_retry(&self) -> Vec<StoredMessage> {
        let now = Instant::now();
        let messages = self.messages.read().unwrap();

        messages
            .values()
            .filter(|m| {
                m.state == MessageState::Retrying
                    && m.retry_at.map(|t| t <= now).unwrap_or(false)
            })
            .cloned()
            .collect()
    }

    fn expire_old(&self, ttl: Duration) -> u64 {
        let cutoff = Instant::now() - ttl;
        let mut messages = self.messages.write().unwrap();
        let mut expired = 0u64;

        for msg in messages.values_mut() {
            if !msg.state.is_terminal() && msg.created_at < cutoff {
                msg.mark_expired();
                expired += 1;
            }
        }

        expired
    }

    fn prune_terminal(&self, max_age: Duration) -> u64 {
        let cutoff = Instant::now() - max_age;
        let mut messages = self.messages.write().unwrap();

        let to_remove: Vec<_> = messages
            .iter()
            .filter(|(_, m)| m.state.is_terminal() && m.updated_at < cutoff)
            .map(|(k, _)| *k)
            .collect();

        let count = to_remove.len() as u64;
        for k in to_remove {
            messages.remove(&k);
        }

        count
    }

    fn stats(&self) -> StoreStats {
        let messages = self.messages.read().unwrap();

        let mut stats = StoreStats {
            total: messages.len() as u64,
            ..Default::default()
        };

        for msg in messages.values() {
            match msg.state {
                MessageState::Pending => stats.pending += 1,
                MessageState::InFlight => stats.in_flight += 1,
                MessageState::Delivered => stats.delivered += 1,
                MessageState::Failed => stats.failed += 1,
                MessageState::Expired => stats.expired += 1,
                MessageState::Retrying => stats.retrying += 1,
            }
        }

        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_and_get() {
        let store = InMemoryStore::new();

        let msg = StoredMessage::new(
            "+258841234567",
            "+258821234567",
            b"Hello".to_vec(),
            "client1",
            1,
        );
        let id = msg.id;

        store.store(msg);

        let retrieved = store.get(id).unwrap();
        assert_eq!(retrieved.source, "+258841234567");
        assert_eq!(retrieved.destination, "+258821234567");
        assert_eq!(retrieved.short_message, b"Hello");
    }

    #[test]
    fn test_update_message() {
        let store = InMemoryStore::new();

        let msg = StoredMessage::new("+258", "+258", b"test".to_vec(), "sys", 1);
        let id = msg.id;
        store.store(msg);

        // Update to in-flight
        store.update(
            id,
            Box::new(|m| {
                m.mark_in_flight("vodacom", "smsc1:2775");
            }),
        );

        let updated = store.get(id).unwrap();
        assert_eq!(updated.state, MessageState::InFlight);
        assert_eq!(updated.cluster.as_deref(), Some("vodacom"));
        assert_eq!(updated.attempts, 1);
    }

    #[test]
    fn test_delete_message() {
        let store = InMemoryStore::new();

        let msg = StoredMessage::new("+258", "+258", b"test".to_vec(), "sys", 1);
        let id = msg.id;
        store.store(msg);

        assert!(store.delete(id));
        assert!(store.get(id).is_none());
        assert!(!store.delete(id)); // Already deleted
    }

    #[test]
    fn test_query_by_state() {
        let store = InMemoryStore::new();

        // Create messages with different states
        let msg1 = StoredMessage::new("+258", "+258", b"pending".to_vec(), "sys", 1);
        let id1 = msg1.id;
        store.store(msg1);

        let msg2 = StoredMessage::new("+258", "+258", b"delivered".to_vec(), "sys", 2);
        let id2 = msg2.id;
        store.store(msg2);
        store.update(id2, Box::new(|m| m.mark_delivered("123")));

        // Query pending
        let pending = store.query(&MessageQuery::new().with_state(MessageState::Pending));
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id1);

        // Query delivered
        let delivered = store.query(&MessageQuery::new().with_state(MessageState::Delivered));
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].id, id2);
    }

    #[test]
    fn test_get_pending_sorted_by_priority() {
        let store = InMemoryStore::new();

        // Create messages with different priorities
        let low = StoredMessage::new("+258", "+258", b"low".to_vec(), "sys", 1)
            .with_priority(Priority::Low);
        store.store(low);

        let high = StoredMessage::new("+258", "+258", b"high".to_vec(), "sys", 2)
            .with_priority(Priority::High);
        store.store(high);

        let normal = StoredMessage::new("+258", "+258", b"normal".to_vec(), "sys", 3)
            .with_priority(Priority::Normal);
        store.store(normal);

        let pending = store.get_pending(10);
        assert_eq!(pending.len(), 3);
        assert_eq!(pending[0].priority, Priority::High);
        assert_eq!(pending[1].priority, Priority::Normal);
        assert_eq!(pending[2].priority, Priority::Low);
    }

    #[test]
    fn test_get_ready_for_retry() {
        let store = InMemoryStore::new();

        let msg = StoredMessage::new("+258", "+258", b"retry".to_vec(), "sys", 1);
        let id = msg.id;
        store.store(msg);

        // Mark for retry with 0 delay (immediately ready)
        store.update(
            id,
            Box::new(|m| {
                m.mark_retry(Duration::ZERO, Some(0x14), "temp error");
            }),
        );

        let ready = store.get_ready_for_retry();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, id);
    }

    #[test]
    fn test_expire_old_messages() {
        let store = InMemoryStore::new();

        let msg = StoredMessage::new("+258", "+258", b"old".to_vec(), "sys", 1);
        let id = msg.id;
        store.store(msg);

        // Expire with 0 TTL (everything is old)
        let expired = store.expire_old(Duration::ZERO);
        assert_eq!(expired, 1);

        let updated = store.get(id).unwrap();
        assert_eq!(updated.state, MessageState::Expired);
    }

    #[test]
    fn test_prune_terminal() {
        let store = InMemoryStore::new();

        let msg = StoredMessage::new("+258", "+258", b"done".to_vec(), "sys", 1);
        let id = msg.id;
        store.store(msg);
        store.update(id, Box::new(|m| m.mark_delivered("123")));

        // Prune with 0 max_age (all terminal are old)
        let pruned = store.prune_terminal(Duration::ZERO);
        assert_eq!(pruned, 1);
        assert!(store.get(id).is_none());
    }

    #[test]
    fn test_stats() {
        let store = InMemoryStore::new();

        // Create various messages
        store.store(StoredMessage::new("+258", "+258", b"1".to_vec(), "sys", 1));

        let msg2 = StoredMessage::new("+258", "+258", b"2".to_vec(), "sys", 2);
        let id2 = msg2.id;
        store.store(msg2);
        store.update(id2, Box::new(|m| m.mark_delivered("123")));

        let msg3 = StoredMessage::new("+258", "+258", b"3".to_vec(), "sys", 3);
        let id3 = msg3.id;
        store.store(msg3);
        store.update(
            id3,
            Box::new(|m| m.mark_failed(Some(0x14), "permanent error")),
        );

        let stats = store.stats();
        assert_eq!(stats.total, 3);
        assert_eq!(stats.pending, 1);
        assert_eq!(stats.delivered, 1);
        assert_eq!(stats.failed, 1);
    }

    #[test]
    fn test_message_lifecycle() {
        let store = InMemoryStore::new();

        // Create message
        let msg = StoredMessage::new(
            "KATEMBE",
            "+258841234567",
            b"OTP: 123456".to_vec(),
            "otp_service",
            1,
        )
        .with_priority(Priority::Critical)
        .with_registered_delivery(1);

        let id = msg.id;
        store.store(msg);

        // Mark in-flight
        store.update(
            id,
            Box::new(|m| {
                m.mark_in_flight("vodacom", "smsc1.vodacom.co.mz:2775");
            }),
        );
        assert_eq!(store.get(id).unwrap().state, MessageState::InFlight);

        // Simulate temporary failure, schedule retry
        store.update(
            id,
            Box::new(|m| {
                m.mark_retry(Duration::from_secs(5), Some(0x58), "throttled");
            }),
        );
        assert_eq!(store.get(id).unwrap().state, MessageState::Retrying);
        assert_eq!(store.get(id).unwrap().attempts, 1);

        // Move back to pending for retry processing
        store.update(id, Box::new(|m| m.reset_to_pending()));
        assert_eq!(store.get(id).unwrap().state, MessageState::Pending);

        // Second attempt - success
        store.update(
            id,
            Box::new(|m| {
                m.mark_in_flight("vodacom", "smsc2.vodacom.co.mz:2775");
            }),
        );
        store.update(id, Box::new(|m| m.mark_delivered("VODA-12345")));

        let final_msg = store.get(id).unwrap();
        assert_eq!(final_msg.state, MessageState::Delivered);
        assert_eq!(final_msg.smsc_message_id.as_deref(), Some("VODA-12345"));
        assert_eq!(final_msg.attempts, 2);
    }
}
