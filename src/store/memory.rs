//! In-memory storage implementation.
//!
//! Volatile storage for development and testing. All data is lost on restart.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::RwLock;
use std::time::{Duration, Instant, SystemTime};

use chrono::Utc;
use tracing::{debug, warn};

use crate::cdr::Cdr;
use crate::config::MemoryStoreConfig;
use crate::feedback::{Decision, DecisionId, DecisionStats, DecisionType, Outcome, TargetStats};

use super::types::*;
use super::{ScheduleStatus, ScheduledMessage, Storage};

/// Average message size estimate in bytes (for memory tracking).
const ESTIMATED_MESSAGE_SIZE: usize = 512;

/// Maximum decisions to keep before pruning.
const MAX_DECISIONS: usize = 100_000;

/// Maximum latency samples to keep for percentile calculations.
const MAX_LATENCY_SAMPLES: usize = 50_000;

// =============================================================================
// MemoryStorage
// =============================================================================

/// In-memory storage implementation.
///
/// Thread-safe using RwLock. Suitable for development and testing.
/// All data is lost on process restart.
pub struct MemoryStorage {
    // Message storage
    messages: RwLock<HashMap<MessageId, StoredMessage>>,
    retry_queue: RwLock<Vec<MessageId>>,
    memory_bytes: AtomicUsize,
    config: MemoryStoreConfig,

    // Feedback storage
    decisions: RwLock<HashMap<DecisionId, Decision>>,
    target_stats: RwLock<HashMap<String, TargetStatsInternal>>,
    type_stats: RwLock<HashMap<DecisionType, TypeStatsInternal>>,

    // CDR storage
    cdrs: RwLock<Vec<Cdr>>,

    // Schedule storage
    schedules: RwLock<HashMap<String, ScheduledMessage>>,

    // Rate limit storage
    rate_limits: RwLock<HashMap<String, RateLimitState>>,

    // Firewall storage
    firewall_settings: RwLock<HashMap<String, FirewallSettings>>,
    firewall_events: RwLock<Vec<FirewallEvent>>,
}

/// Internal target statistics with latency samples.
struct TargetStatsInternal {
    total: u64,
    successes: u64,
    failures: u64,
    sum_latency_us: u64,
    ema_latency_us: u64,
    latency_samples: Vec<u64>,
    last_update: Instant,
}

impl Default for TargetStatsInternal {
    fn default() -> Self {
        Self {
            total: 0,
            successes: 0,
            failures: 0,
            sum_latency_us: 0,
            ema_latency_us: 0,
            latency_samples: Vec::new(),
            last_update: Instant::now(),
        }
    }
}

/// Internal decision type statistics.
struct TypeStatsInternal {
    total: u64,
    successes: u64,
    failures: u64,
    pending: u64,
    sum_latency_us: u64,
    latency_samples: Vec<u64>,
}

impl Default for TypeStatsInternal {
    fn default() -> Self {
        Self {
            total: 0,
            successes: 0,
            failures: 0,
            pending: 0,
            sum_latency_us: 0,
            latency_samples: Vec::new(),
        }
    }
}

impl MemoryStorage {
    /// Create with default configuration.
    pub fn new() -> Self {
        Self::with_config(MemoryStoreConfig::default())
    }

    /// Create with custom configuration.
    pub fn with_config(config: MemoryStoreConfig) -> Self {
        debug!(
            max_messages = config.max_messages,
            max_memory_mb = config.max_memory_mb,
            "creating in-memory storage"
        );

        Self {
            messages: RwLock::new(HashMap::new()),
            retry_queue: RwLock::new(Vec::new()),
            memory_bytes: AtomicUsize::new(0),
            config,
            decisions: RwLock::new(HashMap::new()),
            target_stats: RwLock::new(HashMap::new()),
            type_stats: RwLock::new(HashMap::new()),
            cdrs: RwLock::new(Vec::new()),
            schedules: RwLock::new(HashMap::new()),
            rate_limits: RwLock::new(HashMap::new()),
            firewall_settings: RwLock::new(HashMap::new()),
            firewall_events: RwLock::new(Vec::new()),
        }
    }

    /// Get approximate memory usage in bytes.
    pub fn memory_usage(&self) -> usize {
        self.memory_bytes.load(Ordering::Relaxed)
    }

    fn estimate_message_size(msg: &StoredMessage) -> usize {
        ESTIMATED_MESSAGE_SIZE
            + msg.short_message.len()
            + msg.source.len()
            + msg.destination.len()
            + msg.system_id.len()
            + msg.cluster.as_ref().map(|s| s.len()).unwrap_or(0)
            + msg.endpoint.as_ref().map(|s| s.len()).unwrap_or(0)
            + msg.smsc_message_id.as_ref().map(|s| s.len()).unwrap_or(0)
            + msg.events.len() * 64
    }

    fn prune_if_needed(&self, messages: &mut HashMap<MessageId, StoredMessage>) {
        let max_bytes = self.config.max_memory_mb * 1024 * 1024;
        let current_bytes = self.memory_bytes.load(Ordering::Relaxed);

        let at_message_limit = messages.len() >= self.config.max_messages;
        let at_memory_limit = current_bytes >= max_bytes;

        if !at_message_limit && !at_memory_limit {
            return;
        }

        let prune_count =
            (messages.len() as f64 * self.config.prune_percent as f64 / 100.0) as usize;
        let prune_count = prune_count.max(1);

        warn!(
            messages = messages.len(),
            memory_mb = current_bytes / (1024 * 1024),
            prune_count,
            "storage at capacity, pruning"
        );

        let mut terminal: Vec<_> = messages
            .iter()
            .filter(|(_, m)| m.state.is_terminal())
            .map(|(k, m)| (*k, m.updated_at))
            .collect();

        terminal.sort_by_key(|(_, updated)| *updated);

        let mut removed_bytes = 0usize;
        for (id, _) in terminal.into_iter().take(prune_count) {
            if let Some(msg) = messages.remove(&id) {
                removed_bytes += Self::estimate_message_size(&msg);
            }
        }

        if removed_bytes > 0 {
            self.memory_bytes.fetch_sub(removed_bytes, Ordering::SeqCst);
        }
    }

    fn prune_decisions_if_needed(&self, decisions: &mut HashMap<DecisionId, Decision>) {
        if decisions.len() < MAX_DECISIONS {
            return;
        }

        let prune_count = MAX_DECISIONS / 10;
        let mut by_time: Vec<_> = decisions
            .iter()
            .map(|(k, v)| (*k, v.timestamp))
            .collect();
        by_time.sort_by_key(|(_, t)| *t);

        for (id, _) in by_time.into_iter().take(prune_count) {
            decisions.remove(&id);
        }

        debug!(pruned = prune_count, "pruned old decisions");
    }

    fn calculate_percentile(samples: &[u64], percentile: f64) -> u64 {
        if samples.is_empty() {
            return 0;
        }
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let idx = ((sorted.len() as f64 * percentile / 100.0) as usize).min(sorted.len() - 1);
        sorted[idx]
    }
}

impl Default for MemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl Storage for MemoryStorage {
    // -------------------------------------------------------------------------
    // Message Operations
    // -------------------------------------------------------------------------

    fn store(&self, message: StoredMessage) -> MessageId {
        let id = message.id;
        let msg_size = Self::estimate_message_size(&message);

        let mut messages = self.messages.write().unwrap();
        self.prune_if_needed(&mut messages);
        self.memory_bytes.fetch_add(msg_size, Ordering::SeqCst);
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

        if let Some(msg) = messages.remove(&id) {
            let msg_size = Self::estimate_message_size(&msg);
            self.memory_bytes.fetch_sub(msg_size, Ordering::SeqCst);

            let mut retry_queue = self.retry_queue.write().unwrap();
            retry_queue.retain(|&x| x != id);
            true
        } else {
            false
        }
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

        let mut removed_bytes = 0usize;
        for k in &to_remove {
            if let Some(msg) = messages.remove(k) {
                removed_bytes += Self::estimate_message_size(&msg);
            }
        }

        if removed_bytes > 0 {
            self.memory_bytes.fetch_sub(removed_bytes, Ordering::SeqCst);
        }

        to_remove.len() as u64
    }

    fn message_stats(&self) -> StoreStats {
        let messages = self.messages.read().unwrap();

        let mut stats = StoreStats {
            total: messages.len() as u64,
            memory_bytes: self.memory_bytes.load(Ordering::Relaxed) as u64,
            max_messages: self.config.max_messages as u64,
            max_memory_bytes: (self.config.max_memory_mb * 1024 * 1024) as u64,
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

    // -------------------------------------------------------------------------
    // Feedback Operations
    // -------------------------------------------------------------------------

    fn record_decision(&self, decision: Decision) -> DecisionId {
        let id = decision.id;
        let decision_type = decision.decision_type;
        let target = decision.target.clone();

        // Store decision
        {
            let mut decisions = self.decisions.write().unwrap();
            self.prune_decisions_if_needed(&mut decisions);
            decisions.insert(id, decision);
        }

        // Update type stats
        {
            let mut type_stats = self.type_stats.write().unwrap();
            let stats = type_stats.entry(decision_type).or_default();
            stats.total += 1;
            stats.pending += 1;
        }

        // Initialize target stats if needed
        {
            let mut target_stats = self.target_stats.write().unwrap();
            target_stats.entry(target).or_default();
        }

        id
    }

    fn record_outcome(&self, outcome: Outcome) {
        let decision = {
            let decisions = self.decisions.read().unwrap();
            decisions.get(&outcome.decision_id).cloned()
        };

        let Some(decision) = decision else {
            return;
        };

        let latency_us = outcome.latency.as_micros() as u64;

        // Update type stats
        {
            let mut type_stats = self.type_stats.write().unwrap();
            if let Some(stats) = type_stats.get_mut(&decision.decision_type) {
                stats.pending = stats.pending.saturating_sub(1);
                if outcome.success {
                    stats.successes += 1;
                } else {
                    stats.failures += 1;
                }
                stats.sum_latency_us += latency_us;

                if stats.latency_samples.len() < MAX_LATENCY_SAMPLES {
                    stats.latency_samples.push(latency_us);
                }
            }
        }

        // Update target stats
        {
            let mut target_stats = self.target_stats.write().unwrap();
            if let Some(stats) = target_stats.get_mut(&decision.target) {
                stats.total += 1;
                if outcome.success {
                    stats.successes += 1;
                } else {
                    stats.failures += 1;
                }
                stats.sum_latency_us += latency_us;
                stats.last_update = Instant::now();

                // Update EMA (alpha = 0.2)
                if stats.ema_latency_us == 0 {
                    stats.ema_latency_us = latency_us;
                } else {
                    stats.ema_latency_us = (latency_us * 2 + stats.ema_latency_us * 8) / 10;
                }

                if stats.latency_samples.len() < MAX_LATENCY_SAMPLES {
                    stats.latency_samples.push(latency_us);
                }
            }
        }
    }

    fn decision_stats(&self, decision_type: DecisionType) -> DecisionStats {
        let type_stats = self.type_stats.read().unwrap();

        type_stats
            .get(&decision_type)
            .map(|s| {
                let avg = if s.total > 0 {
                    s.sum_latency_us / s.total
                } else {
                    0
                };

                DecisionStats {
                    total: s.total,
                    successes: s.successes,
                    failures: s.failures,
                    pending: s.pending,
                    avg_latency_us: avg,
                    p50_latency_us: Self::calculate_percentile(&s.latency_samples, 50.0),
                    p95_latency_us: Self::calculate_percentile(&s.latency_samples, 95.0),
                    p99_latency_us: Self::calculate_percentile(&s.latency_samples, 99.0),
                }
            })
            .unwrap_or_default()
    }

    fn target_stats(&self, target: &str) -> TargetStats {
        let target_stats = self.target_stats.read().unwrap();

        target_stats
            .get(target)
            .map(|s| {
                let avg = if s.total > 0 {
                    s.sum_latency_us / s.total
                } else {
                    0
                };

                TargetStats {
                    name: target.to_string(),
                    total: s.total,
                    successes: s.successes,
                    failures: s.failures,
                    avg_latency_us: avg,
                    ema_latency_us: s.ema_latency_us,
                    last_update: Some(s.last_update),
                }
            })
            .unwrap_or_else(|| TargetStats::new(target))
    }

    fn all_target_stats(&self) -> Vec<(String, TargetStats)> {
        let target_stats = self.target_stats.read().unwrap();

        let mut results: Vec<_> = target_stats
            .iter()
            .map(|(name, s)| {
                let avg = if s.total > 0 {
                    s.sum_latency_us / s.total
                } else {
                    0
                };

                (
                    name.clone(),
                    TargetStats {
                        name: name.clone(),
                        total: s.total,
                        successes: s.successes,
                        failures: s.failures,
                        avg_latency_us: avg,
                        ema_latency_us: s.ema_latency_us,
                        last_update: Some(s.last_update),
                    },
                )
            })
            .collect();

        // Sort by success rate descending
        results.sort_by(|a, b| {
            b.1.success_rate()
                .partial_cmp(&a.1.success_rate())
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        results
    }

    fn prune_feedback(&self, max_age: Duration) {
        let cutoff = Instant::now() - max_age;

        let mut decisions = self.decisions.write().unwrap();
        decisions.retain(|_, d| d.timestamp >= cutoff);
    }

    // -------------------------------------------------------------------------
    // CDR Operations
    // -------------------------------------------------------------------------

    fn store_cdr(&self, cdr: Cdr) {
        let mut cdrs = self.cdrs.write().unwrap();
        // Prune if over 100k CDRs
        if cdrs.len() > 100_000 {
            cdrs.drain(0..10_000);
        }
        cdrs.push(cdr);
    }

    fn query_cdrs(&self, query: &CdrQuery) -> Vec<Cdr> {
        let cdrs = self.cdrs.read().unwrap();
        let limit = query.limit.unwrap_or(1000);

        cdrs.iter()
            .filter(|c| {
                if let Some(ref esme) = query.esme_id {
                    if &c.esme_id != esme {
                        return false;
                    }
                }
                if let Some(ref msg_id) = query.message_id {
                    if &c.message_id != msg_id {
                        return false;
                    }
                }
                if let Some(ref prefix) = query.destination_prefix {
                    if !c.dest_addr.starts_with(prefix) {
                        return false;
                    }
                }
                if let Some(ref route) = query.route {
                    if c.route.as_ref() != Some(route) {
                        return false;
                    }
                }
                if let Some(status) = query.status_code {
                    if c.status_code != status {
                        return false;
                    }
                }
                true
            })
            .take(limit)
            .cloned()
            .collect()
    }

    fn cdr_stats(&self) -> CdrStats {
        let cdrs = self.cdrs.read().unwrap();
        let mut stats = CdrStats {
            total: cdrs.len() as u64,
            ..Default::default()
        };

        for cdr in cdrs.iter() {
            let type_name = format!("{:?}", cdr.cdr_type);
            *stats.by_type.entry(type_name).or_insert(0) += 1;

            if let Some(rev) = cdr.revenue {
                stats.total_revenue += rev;
            }
            if let Some(cost) = cdr.cost {
                stats.total_cost += cost;
            }
            if stats.currency.is_none() {
                stats.currency = cdr.currency.clone();
            }
        }

        stats
    }

    fn prune_cdrs(&self, max_age: Duration) -> u64 {
        let cutoff = SystemTime::now() - max_age;
        let mut cdrs = self.cdrs.write().unwrap();
        let before = cdrs.len();

        cdrs.retain(|c| {
            c.timestamp_datetime()
                .map(|dt| SystemTime::from(dt) >= cutoff)
                .unwrap_or(true)
        });

        (before - cdrs.len()) as u64
    }

    // -------------------------------------------------------------------------
    // Schedule Operations
    // -------------------------------------------------------------------------

    fn store_schedule(&self, schedule: ScheduledMessage) -> String {
        let id = schedule.id.clone();
        let mut schedules = self.schedules.write().unwrap();
        schedules.insert(id.clone(), schedule);
        id
    }

    fn get_schedule(&self, id: &str) -> Option<ScheduledMessage> {
        self.schedules.read().unwrap().get(id).cloned()
    }

    fn update_schedule(&self, id: &str, f: Box<dyn FnOnce(&mut ScheduledMessage) + Send>) -> bool {
        let mut schedules = self.schedules.write().unwrap();
        if let Some(schedule) = schedules.get_mut(id) {
            f(schedule);
            true
        } else {
            false
        }
    }

    fn delete_schedule(&self, id: &str) -> bool {
        self.schedules.write().unwrap().remove(id).is_some()
    }

    fn get_due_schedules(&self, limit: usize) -> Vec<ScheduledMessage> {
        let now = Utc::now();
        let schedules = self.schedules.read().unwrap();

        schedules
            .values()
            .filter(|s| s.status == ScheduleStatus::Pending && s.scheduled_at <= now)
            .take(limit)
            .cloned()
            .collect()
    }

    fn get_esme_schedules(&self, esme_id: &str) -> Vec<ScheduledMessage> {
        self.schedules
            .read()
            .unwrap()
            .values()
            .filter(|s| s.esme_id == esme_id)
            .cloned()
            .collect()
    }

    // -------------------------------------------------------------------------
    // Rate Limit Operations
    // -------------------------------------------------------------------------

    fn store_rate_limit(&self, client_id: &str, state: RateLimitState) {
        self.rate_limits
            .write()
            .unwrap()
            .insert(client_id.to_string(), state);
    }

    fn get_rate_limit(&self, client_id: &str) -> Option<RateLimitState> {
        self.rate_limits.read().unwrap().get(client_id).cloned()
    }

    fn delete_rate_limit(&self, client_id: &str) -> bool {
        self.rate_limits.write().unwrap().remove(client_id).is_some()
    }

    // -------------------------------------------------------------------------
    // Firewall Operations
    // -------------------------------------------------------------------------

    fn store_firewall_settings(&self, esme_id: &str, settings: FirewallSettings) {
        self.firewall_settings
            .write()
            .unwrap()
            .insert(esme_id.to_string(), settings);
    }

    fn get_firewall_settings(&self, esme_id: &str) -> Option<FirewallSettings> {
        self.firewall_settings
            .read()
            .unwrap()
            .get(esme_id)
            .cloned()
    }

    fn store_firewall_event(&self, event: FirewallEvent) {
        let mut events = self.firewall_events.write().unwrap();
        // Prune if over 50k events
        if events.len() > 50_000 {
            events.drain(0..5_000);
        }
        events.push(event);
    }

    fn query_firewall_events(&self, query: &FirewallEventQuery) -> Vec<FirewallEvent> {
        let events = self.firewall_events.read().unwrap();
        let limit = query.limit.unwrap_or(1000);

        events
            .iter()
            .filter(|e| {
                if let Some(ref esme) = query.esme_id {
                    if &e.esme_id != esme {
                        return false;
                    }
                }
                if let Some(ref action) = query.action {
                    if &e.action != action {
                        return false;
                    }
                }
                if let Some(ref prefix) = query.destination_prefix {
                    if !e.destination.starts_with(prefix) {
                        return false;
                    }
                }
                if let Some(after) = query.after {
                    if e.timestamp < after {
                        return false;
                    }
                }
                if let Some(before) = query.before {
                    if e.timestamp > before {
                        return false;
                    }
                }
                true
            })
            .take(limit)
            .cloned()
            .collect()
    }

    fn prune_firewall_events(&self, max_age: Duration) -> u64 {
        let cutoff = SystemTime::now() - max_age;
        let mut events = self.firewall_events.write().unwrap();
        let before = events.len();
        events.retain(|e| e.timestamp >= cutoff);
        (before - events.len()) as u64
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_and_get() {
        let store = MemoryStorage::new();

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
    }

    #[test]
    fn test_update_message() {
        let store = MemoryStorage::new();

        let msg = StoredMessage::new("+258", "+258", b"test".to_vec(), "sys", 1);
        let id = msg.id;
        store.store(msg);

        store.update(
            id,
            Box::new(|m| {
                m.mark_in_flight("vodacom", "smsc1:2775");
            }),
        );

        let updated = store.get(id).unwrap();
        assert_eq!(updated.state, MessageState::InFlight);
        assert_eq!(updated.cluster.as_deref(), Some("vodacom"));
    }

    #[test]
    fn test_delete_message() {
        let store = MemoryStorage::new();

        let msg = StoredMessage::new("+258", "+258", b"test".to_vec(), "sys", 1);
        let id = msg.id;
        store.store(msg);

        assert!(store.delete(id));
        assert!(store.get(id).is_none());
    }

    #[test]
    fn test_query_by_state() {
        let store = MemoryStorage::new();

        let msg1 = StoredMessage::new("+258", "+258", b"pending".to_vec(), "sys", 1);
        let id1 = msg1.id;
        store.store(msg1);

        let msg2 = StoredMessage::new("+258", "+258", b"delivered".to_vec(), "sys", 2);
        let id2 = msg2.id;
        store.store(msg2);
        store.update(id2, Box::new(|m| m.mark_delivered("123")));

        let pending = store.query(&MessageQuery::new().with_state(MessageState::Pending));
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id1);
    }

    #[test]
    fn test_get_pending_sorted_by_priority() {
        let store = MemoryStorage::new();

        store.store(
            StoredMessage::new("+258", "+258", b"low".to_vec(), "sys", 1)
                .with_priority(Priority::Low),
        );
        store.store(
            StoredMessage::new("+258", "+258", b"high".to_vec(), "sys", 2)
                .with_priority(Priority::High),
        );
        store.store(
            StoredMessage::new("+258", "+258", b"normal".to_vec(), "sys", 3)
                .with_priority(Priority::Normal),
        );

        let pending = store.get_pending(10);
        assert_eq!(pending.len(), 3);
        assert_eq!(pending[0].priority, Priority::High);
        assert_eq!(pending[1].priority, Priority::Normal);
        assert_eq!(pending[2].priority, Priority::Low);
    }

    #[test]
    fn test_get_ready_for_retry() {
        let store = MemoryStorage::new();

        let msg = StoredMessage::new("+258", "+258", b"retry".to_vec(), "sys", 1);
        let id = msg.id;
        store.store(msg);

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
        let store = MemoryStorage::new();

        let msg = StoredMessage::new("+258", "+258", b"old".to_vec(), "sys", 1);
        let id = msg.id;
        store.store(msg);

        let expired = store.expire_old(Duration::ZERO);
        assert_eq!(expired, 1);

        let updated = store.get(id).unwrap();
        assert_eq!(updated.state, MessageState::Expired);
    }

    #[test]
    fn test_prune_terminal() {
        let store = MemoryStorage::new();

        let msg = StoredMessage::new("+258", "+258", b"done".to_vec(), "sys", 1);
        let id = msg.id;
        store.store(msg);
        store.update(id, Box::new(|m| m.mark_delivered("123")));

        let pruned = store.prune_terminal(Duration::ZERO);
        assert_eq!(pruned, 1);
        assert!(store.get(id).is_none());
    }

    #[test]
    fn test_message_stats() {
        let store = MemoryStorage::new();

        store.store(StoredMessage::new("+258", "+258", b"1".to_vec(), "sys", 1));

        let msg2 = StoredMessage::new("+258", "+258", b"2".to_vec(), "sys", 2);
        let id2 = msg2.id;
        store.store(msg2);
        store.update(id2, Box::new(|m| m.mark_delivered("123")));

        let stats = store.message_stats();
        assert_eq!(stats.total, 2);
        assert_eq!(stats.pending, 1);
        assert_eq!(stats.delivered, 1);
    }

    #[test]
    fn test_feedback_operations() {
        use crate::feedback::{Decision, DecisionContext, Outcome};

        let store = MemoryStorage::new();

        // Record a decision
        let ctx = DecisionContext::new("+258841234567");
        let decision = Decision::route("cluster1", vec!["cluster2".into()], ctx);
        let id = store.record_decision(decision);

        // Record outcome
        let outcome = Outcome::success(id, Duration::from_millis(50));
        store.record_outcome(outcome);

        // Check target stats
        let stats = store.target_stats("cluster1");
        assert_eq!(stats.total, 1);
        assert_eq!(stats.successes, 1);
        assert!(stats.avg_latency_us > 0);

        // Check decision stats
        let dec_stats = store.decision_stats(DecisionType::Route);
        assert_eq!(dec_stats.total, 1);
        assert_eq!(dec_stats.successes, 1);
    }

    #[test]
    fn test_all_target_stats_sorted() {
        use crate::feedback::{Decision, DecisionContext, Outcome};

        let store = MemoryStorage::new();

        // Create decisions for two targets
        for i in 0..5 {
            let ctx = DecisionContext::new("+258");
            let decision = Decision::route("good_cluster", vec![], ctx);
            let id = store.record_decision(decision);
            store.record_outcome(Outcome::success(id, Duration::from_millis(10)));
        }

        for i in 0..5 {
            let ctx = DecisionContext::new("+258");
            let decision = Decision::route("bad_cluster", vec![], ctx);
            let id = store.record_decision(decision);
            if i < 3 {
                store.record_outcome(Outcome::failure(
                    id,
                    Duration::from_millis(100),
                    Some(0x14),
                    Some("error".into()),
                ));
            } else {
                store.record_outcome(Outcome::success(id, Duration::from_millis(10)));
            }
        }

        let all_stats = store.all_target_stats();
        assert_eq!(all_stats.len(), 2);
        // Good cluster should be first (100% success rate)
        assert_eq!(all_stats[0].0, "good_cluster");
        assert_eq!(all_stats[1].0, "bad_cluster");
    }
}
