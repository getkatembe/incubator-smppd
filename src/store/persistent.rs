//! Persistent storage using fjall (pure Rust LSM-tree).
//!
//! Durable storage for production use. All data survives restarts.
//! Enables compute/storage separation for stateless compute nodes.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::Utc;
use fjall::{Config, Keyspace, PartitionCreateOptions, PartitionHandle};
use serde::{Deserialize, Serialize};

use crate::cdr::Cdr;
use crate::feedback::{Decision, DecisionId, DecisionStats, DecisionType, Outcome, TargetStats};

use super::types::*;
use super::{ScheduleStatus, ScheduledMessage, Storage};

// =============================================================================
// PersistentStorage
// =============================================================================

/// Persistent storage using fjall LSM-tree.
///
/// All writes are immediately persisted to disk. Thread-safe and lock-free
/// for concurrent access from multiple compute nodes.
pub struct PersistentStorage {
    keyspace: Keyspace,
    messages: PartitionHandle,
    messages_by_state: PartitionHandle,
    decisions: PartitionHandle,
    outcomes: PartitionHandle,
    target_stats: PartitionHandle,
    #[allow(dead_code)]
    metadata: PartitionHandle,
    // New partitions
    cdrs: PartitionHandle,
    rate_limits: PartitionHandle,
    firewall_settings: PartitionHandle,
    firewall_events: PartitionHandle,
    // In-memory only (ScheduledMessage contains StoredMessage with Instant which isn't serializable)
    schedules: RwLock<HashMap<String, ScheduledMessage>>,
}

// =============================================================================
// Serializable Types
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedMessage {
    id: u64,
    state: u8,
    priority: u8,
    source: String,
    destination: String,
    short_message: Vec<u8>,
    data_coding: u8,
    esm_class: u8,
    registered_delivery: u8,
    service_type: Option<String>,
    sender_id: Option<String>,
    system_id: String,
    sequence_number: u32,
    smsc_message_id: Option<String>,
    route: Option<String>,
    cluster: Option<String>,
    endpoint: Option<String>,
    attempts: u32,
    max_attempts: u32,
    ttl_secs: u64,
    created_at_epoch_ms: u64,
    updated_at_epoch_ms: u64,
    retry_at_epoch_ms: Option<u64>,
    last_error_code: Option<u32>,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedDecision {
    id: u64,
    decision_type: u8,
    target: String,
    alternatives: Vec<String>,
    destination: Option<String>,
    source: Option<String>,
    system_id: Option<String>,
    route_name: Option<String>,
    cluster_name: Option<String>,
    attempt: Option<u32>,
    fallback_from: Option<String>,
    timestamp_epoch_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedOutcome {
    decision_id: u64,
    success: bool,
    latency_us: u64,
    error_code: Option<u32>,
    error_message: Option<String>,
    timestamp_epoch_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedTargetStats {
    name: String,
    total: u64,
    successes: u64,
    failures: u64,
    avg_latency_us: u64,
    ema_latency_us: u64,
    last_update_epoch_ms: u64,
}

// =============================================================================
// Implementation
// =============================================================================

impl PersistentStorage {
    /// Open or create persistent storage at the given path.
    pub async fn open(path: &Path) -> anyhow::Result<Arc<Self>> {
        std::fs::create_dir_all(path)?;

        let keyspace = Config::new(path).open()?;

        let messages = keyspace.open_partition("messages", PartitionCreateOptions::default())?;
        let messages_by_state =
            keyspace.open_partition("messages_by_state", PartitionCreateOptions::default())?;
        let decisions = keyspace.open_partition("decisions", PartitionCreateOptions::default())?;
        let outcomes = keyspace.open_partition("outcomes", PartitionCreateOptions::default())?;
        let target_stats =
            keyspace.open_partition("target_stats", PartitionCreateOptions::default())?;
        let metadata = keyspace.open_partition("metadata", PartitionCreateOptions::default())?;

        let cdrs = keyspace.open_partition("cdrs", PartitionCreateOptions::default())?;
        let rate_limits =
            keyspace.open_partition("rate_limits", PartitionCreateOptions::default())?;
        let firewall_settings =
            keyspace.open_partition("firewall_settings", PartitionCreateOptions::default())?;
        let firewall_events =
            keyspace.open_partition("firewall_events", PartitionCreateOptions::default())?;

        let store = Arc::new(Self {
            keyspace,
            messages,
            messages_by_state,
            decisions,
            outcomes,
            target_stats,
            metadata,
            cdrs,
            rate_limits,
            firewall_settings,
            firewall_events,
            schedules: RwLock::new(HashMap::new()),
        });

        store.recover_counters()?;

        tracing::info!(
            path = %path.display(),
            messages = store.count_messages(),
            "persistent storage opened"
        );

        Ok(store)
    }

    fn recover_counters(&self) -> anyhow::Result<()> {
        use std::sync::atomic::Ordering;

        let mut max_msg_id = 0u64;
        for item in self.messages.iter() {
            let (key, _) = item?;
            if let Ok(key_str) = std::str::from_utf8(&key) {
                if let Some(id_str) = key_str.strip_prefix("msg_") {
                    if let Ok(id) = id_str.parse::<u64>() {
                        max_msg_id = max_msg_id.max(id);
                    }
                }
            }
        }

        let mut max_dec_id = 0u64;
        for item in self.decisions.iter() {
            let (key, _) = item?;
            if let Ok(key_str) = std::str::from_utf8(&key) {
                if let Some(id_str) = key_str.strip_prefix("dec_") {
                    if let Ok(id) = id_str.parse::<u64>() {
                        max_dec_id = max_dec_id.max(id);
                    }
                }
            }
        }

        crate::store::types::MESSAGE_COUNTER.store(max_msg_id + 1, Ordering::SeqCst);
        crate::feedback::types::DECISION_COUNTER.store(max_dec_id + 1, Ordering::SeqCst);

        tracing::debug!(max_msg_id, max_dec_id, "recovered ID counters");
        Ok(())
    }

    fn count_messages(&self) -> usize {
        self.messages.len().unwrap_or(0)
    }

    fn now_epoch_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    fn persist_message(&self, msg: &StoredMessage) -> anyhow::Result<()> {
        let persisted = PersistedMessage {
            id: msg.id.as_u64(),
            state: msg.state as u8,
            priority: msg.priority as u8,
            source: msg.source.clone(),
            destination: msg.destination.clone(),
            short_message: msg.short_message.clone(),
            data_coding: msg.data_coding,
            esm_class: msg.esm_class,
            registered_delivery: msg.registered_delivery,
            service_type: msg.service_type.clone(),
            sender_id: msg.sender_id.clone(),
            system_id: msg.system_id.clone(),
            sequence_number: msg.sequence_number,
            smsc_message_id: msg.smsc_message_id.clone(),
            route: msg.route.clone(),
            cluster: msg.cluster.clone(),
            endpoint: msg.endpoint.clone(),
            attempts: msg.attempts,
            max_attempts: msg.max_attempts,
            ttl_secs: msg.ttl.as_secs(),
            created_at_epoch_ms: msg
                .created_time
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            updated_at_epoch_ms: Self::now_epoch_ms(),
            retry_at_epoch_ms: None,
            last_error_code: msg.last_error_code,
            last_error: msg.last_error.clone(),
        };

        let key = format!("msg_{}", msg.id.as_u64());
        let value = serde_json::to_vec(&persisted)?;
        self.messages.insert(key.as_bytes(), &value)?;

        let state_key = format!("state_{}_{}", msg.state as u8, msg.id.as_u64());
        self.messages_by_state
            .insert(state_key.as_bytes(), &msg.id.as_u64().to_be_bytes())?;

        Ok(())
    }

    fn load_message(&self, id: MessageId) -> Option<StoredMessage> {
        let key = format!("msg_{}", id.as_u64());
        let value = self.messages.get(key.as_bytes()).ok()??;
        let persisted: PersistedMessage = serde_json::from_slice(&value).ok()?;
        Some(self.persisted_to_stored(persisted))
    }

    fn persisted_to_stored(&self, p: PersistedMessage) -> StoredMessage {
        let created_time = UNIX_EPOCH + Duration::from_millis(p.created_at_epoch_ms);
        let now = std::time::Instant::now();

        StoredMessage {
            id: MessageId::from_u64(p.id),
            state: match p.state {
                0 => MessageState::Pending,
                1 => MessageState::InFlight,
                2 => MessageState::Delivered,
                3 => MessageState::Failed,
                4 => MessageState::Expired,
                5 => MessageState::Retrying,
                _ => MessageState::Pending,
            },
            priority: match p.priority {
                0 => Priority::Low,
                1 => Priority::Normal,
                2 => Priority::High,
                3 => Priority::Critical,
                _ => Priority::Normal,
            },
            source: p.source,
            destination: p.destination,
            short_message: p.short_message,
            data_coding: p.data_coding,
            esm_class: p.esm_class,
            registered_delivery: p.registered_delivery,
            service_type: p.service_type,
            sender_id: p.sender_id,
            system_id: p.system_id,
            sequence_number: p.sequence_number,
            smsc_message_id: p.smsc_message_id,
            route: p.route,
            cluster: p.cluster,
            endpoint: p.endpoint,
            attempts: p.attempts,
            max_attempts: p.max_attempts,
            ttl: Duration::from_secs(p.ttl_secs),
            created_at: now,
            created_time,
            updated_at: now,
            retry_at: p.retry_at_epoch_ms.map(|_| now),
            last_error_code: p.last_error_code,
            last_error: p.last_error,
            events: vec![],
        }
    }

    fn get_by_state(&self, state: MessageState, limit: usize) -> Vec<StoredMessage> {
        let prefix = format!("state_{}_", state as u8);
        let mut results = Vec::new();

        for item in self.messages_by_state.prefix(prefix.as_bytes()) {
            if results.len() >= limit {
                break;
            }
            if let Ok((_, value)) = item {
                if value.len() == 8 {
                    let id = u64::from_be_bytes(value[..8].try_into().unwrap());
                    if let Some(msg) = self.load_message(MessageId::from_u64(id)) {
                        results.push(msg);
                    }
                }
            }
        }

        results
    }

    fn remove_state_index(&self, old_state: MessageState, id: MessageId) {
        let state_key = format!("state_{}_{}", old_state as u8, id.as_u64());
        let _ = self.messages_by_state.remove(state_key.as_bytes());
    }

    fn persist_target_stats(&self, stats: &TargetStats) {
        let persisted = PersistedTargetStats {
            name: stats.name.clone(),
            total: stats.total,
            successes: stats.successes,
            failures: stats.failures,
            avg_latency_us: stats.avg_latency_us,
            ema_latency_us: stats.ema_latency_us,
            last_update_epoch_ms: Self::now_epoch_ms(),
        };

        let key = format!("target_{}", stats.name);
        if let Ok(value) = serde_json::to_vec(&persisted) {
            let _ = self.target_stats.insert(key.as_bytes(), &value);
        }
    }

    fn load_target_stats(&self, name: &str) -> TargetStats {
        let key = format!("target_{}", name);
        if let Ok(Some(value)) = self.target_stats.get(key.as_bytes()) {
            if let Ok(persisted) = serde_json::from_slice::<PersistedTargetStats>(&value) {
                return TargetStats {
                    name: persisted.name,
                    total: persisted.total,
                    successes: persisted.successes,
                    failures: persisted.failures,
                    avg_latency_us: persisted.avg_latency_us,
                    ema_latency_us: persisted.ema_latency_us,
                    last_update: None,
                };
            }
        }
        TargetStats::new(name)
    }
}

impl Storage for PersistentStorage {
    // -------------------------------------------------------------------------
    // Message Operations
    // -------------------------------------------------------------------------

    fn store(&self, message: StoredMessage) -> MessageId {
        let id = message.id;
        if let Err(e) = self.persist_message(&message) {
            tracing::error!(error = %e, "failed to persist message");
        }
        id
    }

    fn get(&self, id: MessageId) -> Option<StoredMessage> {
        self.load_message(id)
    }

    fn update(&self, id: MessageId, f: Box<dyn FnOnce(&mut StoredMessage) + Send>) -> bool {
        if let Some(mut msg) = self.load_message(id) {
            let old_state = msg.state;
            f(&mut msg);
            let new_state = msg.state;

            if old_state != new_state {
                self.remove_state_index(old_state, id);
            }

            if let Err(e) = self.persist_message(&msg) {
                tracing::error!(error = %e, "failed to update message");
                return false;
            }
            true
        } else {
            false
        }
    }

    fn delete(&self, id: MessageId) -> bool {
        if let Some(msg) = self.load_message(id) {
            self.remove_state_index(msg.state, id);
            let key = format!("msg_{}", id.as_u64());
            self.messages.remove(key.as_bytes()).is_ok()
        } else {
            false
        }
    }

    fn query(&self, query: &MessageQuery) -> Vec<StoredMessage> {
        let limit = query.limit.unwrap_or(1000);

        if let Some(state) = query.state {
            return self.get_by_state(state, limit);
        }

        let mut results = Vec::new();
        for item in self.messages.iter() {
            if results.len() >= limit {
                break;
            }
            if let Ok((_, value)) = item {
                if let Ok(persisted) = serde_json::from_slice::<PersistedMessage>(&value) {
                    let msg = self.persisted_to_stored(persisted);

                    if let Some(ref sys_id) = query.system_id {
                        if msg.system_id != *sys_id {
                            continue;
                        }
                    }
                    if let Some(ref prefix) = query.destination_prefix {
                        if !msg.destination.starts_with(prefix) {
                            continue;
                        }
                    }
                    if let Some(ref cluster) = query.cluster {
                        if msg.cluster.as_ref() != Some(cluster) {
                            continue;
                        }
                    }

                    results.push(msg);
                }
            }
        }

        results
    }

    fn get_pending(&self, limit: usize) -> Vec<StoredMessage> {
        self.get_by_state(MessageState::Pending, limit)
    }

    fn get_ready_for_retry(&self) -> Vec<StoredMessage> {
        self.get_by_state(MessageState::Retrying, 1000)
    }

    fn expire_old(&self, ttl: Duration) -> u64 {
        let mut expired = 0u64;
        let now = Self::now_epoch_ms();
        let ttl_ms = ttl.as_millis() as u64;

        for item in self.messages.iter() {
            if let Ok((_, value)) = item {
                if let Ok(persisted) = serde_json::from_slice::<PersistedMessage>(&value) {
                    if persisted.state == MessageState::Pending as u8
                        && now.saturating_sub(persisted.created_at_epoch_ms) > ttl_ms
                    {
                        let id = MessageId::from_u64(persisted.id);
                        self.remove_state_index(MessageState::Pending, id);

                        let mut msg = self.persisted_to_stored(persisted);
                        msg.mark_expired();
                        let _ = self.persist_message(&msg);
                        expired += 1;
                    }
                }
            }
        }

        expired
    }

    fn prune_terminal(&self, max_age: Duration) -> u64 {
        let mut pruned = 0u64;
        let now = Self::now_epoch_ms();
        let max_age_ms = max_age.as_millis() as u64;

        let mut to_delete = Vec::new();

        for item in self.messages.iter() {
            if let Ok((key, value)) = item {
                if let Ok(persisted) = serde_json::from_slice::<PersistedMessage>(&value) {
                    let state = match persisted.state {
                        2 => MessageState::Delivered,
                        3 => MessageState::Failed,
                        4 => MessageState::Expired,
                        _ => continue,
                    };

                    if now.saturating_sub(persisted.updated_at_epoch_ms) > max_age_ms {
                        to_delete.push((key.to_vec(), persisted.id, state));
                    }
                }
            }
        }

        for (key, id, state) in to_delete {
            self.remove_state_index(state, MessageId::from_u64(id));
            let _ = self.messages.remove(&key);
            pruned += 1;
        }

        pruned
    }

    fn message_stats(&self) -> StoreStats {
        let mut stats = StoreStats::default();

        for item in self.messages.iter() {
            if let Ok((_, value)) = item {
                if let Ok(persisted) = serde_json::from_slice::<PersistedMessage>(&value) {
                    stats.total += 1;
                    match persisted.state {
                        0 => stats.pending += 1,
                        1 => stats.in_flight += 1,
                        2 => stats.delivered += 1,
                        3 => stats.failed += 1,
                        4 => stats.expired += 1,
                        5 => stats.retrying += 1,
                        _ => {}
                    }
                }
            }
        }

        stats
    }

    // -------------------------------------------------------------------------
    // Feedback Operations
    // -------------------------------------------------------------------------

    fn record_decision(&self, decision: Decision) -> DecisionId {
        let id = decision.id;

        let persisted = PersistedDecision {
            id: id.as_u64(),
            decision_type: decision.decision_type as u8,
            target: decision.target,
            alternatives: decision.alternatives,
            destination: decision.context.destination,
            source: decision.context.source,
            system_id: decision.context.system_id,
            route_name: decision.context.route_name,
            cluster_name: decision.context.cluster_name,
            attempt: decision.context.attempt,
            fallback_from: decision.context.fallback_from,
            timestamp_epoch_ms: Self::now_epoch_ms(),
        };

        let key = format!("dec_{}", id.as_u64());
        if let Ok(value) = serde_json::to_vec(&persisted) {
            let _ = self.decisions.insert(key.as_bytes(), &value);
        }

        id
    }

    fn record_outcome(&self, outcome: Outcome) {
        let persisted = PersistedOutcome {
            decision_id: outcome.decision_id.as_u64(),
            success: outcome.success,
            latency_us: outcome.latency.as_micros() as u64,
            error_code: outcome.error_code,
            error_message: outcome.error_message,
            timestamp_epoch_ms: Self::now_epoch_ms(),
        };

        let key = format!("out_{}", outcome.decision_id.as_u64());
        if let Ok(value) = serde_json::to_vec(&persisted) {
            let _ = self.outcomes.insert(key.as_bytes(), &value);
        }

        // Update target stats
        let dec_key = format!("dec_{}", outcome.decision_id.as_u64());
        if let Ok(Some(dec_value)) = self.decisions.get(dec_key.as_bytes()) {
            if let Ok(decision) = serde_json::from_slice::<PersistedDecision>(&dec_value) {
                let mut stats = self.load_target_stats(&decision.target);
                stats.total += 1;
                if outcome.success {
                    stats.successes += 1;
                } else {
                    stats.failures += 1;
                }

                let latency_us = outcome.latency.as_micros() as u64;
                if stats.ema_latency_us == 0 {
                    stats.ema_latency_us = latency_us;
                } else {
                    stats.ema_latency_us = (latency_us * 2 + stats.ema_latency_us * 8) / 10;
                }

                stats.avg_latency_us =
                    (stats.avg_latency_us * (stats.total - 1) + latency_us) / stats.total;

                self.persist_target_stats(&stats);
            }
        }
    }

    fn decision_stats(&self, _decision_type: DecisionType) -> DecisionStats {
        DecisionStats::default()
    }

    fn target_stats(&self, target: &str) -> TargetStats {
        self.load_target_stats(target)
    }

    fn all_target_stats(&self) -> Vec<(String, TargetStats)> {
        let mut results = Vec::new();

        for item in self.target_stats.prefix(b"target_") {
            if let Ok((_, value)) = item {
                if let Ok(persisted) = serde_json::from_slice::<PersistedTargetStats>(&value) {
                    let stats = TargetStats {
                        name: persisted.name.clone(),
                        total: persisted.total,
                        successes: persisted.successes,
                        failures: persisted.failures,
                        avg_latency_us: persisted.avg_latency_us,
                        ema_latency_us: persisted.ema_latency_us,
                        last_update: None,
                    };
                    results.push((persisted.name, stats));
                }
            }
        }

        results.sort_by(|a, b| {
            b.1.success_rate()
                .partial_cmp(&a.1.success_rate())
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        results
    }

    fn prune_feedback(&self, max_age: Duration) {
        let now = Self::now_epoch_ms();
        let max_age_ms = max_age.as_millis() as u64;

        let mut to_delete = Vec::new();
        for item in self.decisions.iter() {
            if let Ok((key, value)) = item {
                if let Ok(persisted) = serde_json::from_slice::<PersistedDecision>(&value) {
                    if now.saturating_sub(persisted.timestamp_epoch_ms) > max_age_ms {
                        to_delete.push(key.to_vec());
                    }
                }
            }
        }
        for key in to_delete {
            let _ = self.decisions.remove(&key);
        }

        let mut to_delete = Vec::new();
        for item in self.outcomes.iter() {
            if let Ok((key, value)) = item {
                if let Ok(persisted) = serde_json::from_slice::<PersistedOutcome>(&value) {
                    if now.saturating_sub(persisted.timestamp_epoch_ms) > max_age_ms {
                        to_delete.push(key.to_vec());
                    }
                }
            }
        }
        for key in to_delete {
            let _ = self.outcomes.remove(&key);
        }
    }

    fn get_recoverable_messages(&self) -> Vec<StoredMessage> {
        let mut messages = self.get_by_state(MessageState::Pending, usize::MAX);
        messages.extend(self.get_by_state(MessageState::InFlight, usize::MAX));
        messages.extend(self.get_by_state(MessageState::Retrying, usize::MAX));
        messages
    }

    fn flush(&self) -> anyhow::Result<()> {
        self.keyspace.persist(fjall::PersistMode::SyncAll)?;
        Ok(())
    }

    // -------------------------------------------------------------------------
    // CDR Operations
    // -------------------------------------------------------------------------

    fn store_cdr(&self, cdr: Cdr) {
        let key = format!("cdr_{}_{}", cdr.timestamp, cdr.message_id);
        if let Ok(value) = serde_json::to_vec(&cdr) {
            let _ = self.cdrs.insert(key.as_bytes(), &value);
        }
    }

    fn query_cdrs(&self, query: &CdrQuery) -> Vec<Cdr> {
        let limit = query.limit.unwrap_or(1000);
        let mut results = Vec::new();

        for item in self.cdrs.iter() {
            if results.len() >= limit {
                break;
            }
            if let Ok((_, value)) = item {
                if let Ok(cdr) = serde_json::from_slice::<Cdr>(&value) {
                    if let Some(ref esme) = query.esme_id {
                        if &cdr.esme_id != esme {
                            continue;
                        }
                    }
                    if let Some(ref msg_id) = query.message_id {
                        if &cdr.message_id != msg_id {
                            continue;
                        }
                    }
                    if let Some(ref prefix) = query.destination_prefix {
                        if !cdr.dest_addr.starts_with(prefix) {
                            continue;
                        }
                    }
                    if let Some(ref route) = query.route {
                        if cdr.route.as_ref() != Some(route) {
                            continue;
                        }
                    }
                    if let Some(status) = query.status_code {
                        if cdr.status_code != status {
                            continue;
                        }
                    }
                    results.push(cdr);
                }
            }
        }

        results
    }

    fn cdr_stats(&self) -> CdrStats {
        let mut stats = CdrStats::default();

        for item in self.cdrs.iter() {
            if let Ok((_, value)) = item {
                if let Ok(cdr) = serde_json::from_slice::<Cdr>(&value) {
                    stats.total += 1;
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
            }
        }

        stats
    }

    fn prune_cdrs(&self, max_age: Duration) -> u64 {
        let cutoff = SystemTime::now() - max_age;

        let mut to_delete = Vec::new();
        let mut pruned = 0u64;

        for item in self.cdrs.iter() {
            if let Ok((key, value)) = item {
                if let Ok(cdr) = serde_json::from_slice::<Cdr>(&value) {
                    // Use timestamp_datetime() to parse RFC3339 string
                    if let Some(dt) = cdr.timestamp_datetime() {
                        if SystemTime::from(dt) < cutoff {
                            to_delete.push(key.to_vec());
                        }
                    }
                }
            }
        }

        for key in to_delete {
            if self.cdrs.remove(&key).is_ok() {
                pruned += 1;
            }
        }

        pruned
    }

    // -------------------------------------------------------------------------
    // Schedule Operations
    // -------------------------------------------------------------------------

    fn store_schedule(&self, schedule: ScheduledMessage) -> String {
        let id = schedule.id.clone();
        self.schedules.write().unwrap().insert(id.clone(), schedule);
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
        self.schedules
            .read()
            .unwrap()
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
        let key = format!("rl_{}", client_id);
        if let Ok(value) = serde_json::to_vec(&state) {
            let _ = self.rate_limits.insert(key.as_bytes(), &value);
        }
    }

    fn get_rate_limit(&self, client_id: &str) -> Option<RateLimitState> {
        let key = format!("rl_{}", client_id);
        let value = self.rate_limits.get(key.as_bytes()).ok()??;
        serde_json::from_slice(&value).ok()
    }

    fn delete_rate_limit(&self, client_id: &str) -> bool {
        let key = format!("rl_{}", client_id);
        self.rate_limits.remove(key.as_bytes()).is_ok()
    }

    // -------------------------------------------------------------------------
    // Firewall Operations
    // -------------------------------------------------------------------------

    fn store_firewall_settings(&self, esme_id: &str, settings: FirewallSettings) {
        let key = format!("fwsettings_{}", esme_id);
        if let Ok(value) = serde_json::to_vec(&settings) {
            let _ = self.firewall_settings.insert(key.as_bytes(), &value);
        }
    }

    fn get_firewall_settings(&self, esme_id: &str) -> Option<FirewallSettings> {
        let key = format!("fwsettings_{}", esme_id);
        let value = self.firewall_settings.get(key.as_bytes()).ok()??;
        serde_json::from_slice(&value).ok()
    }

    fn store_firewall_event(&self, event: FirewallEvent) {
        let epoch_ms = event
            .timestamp
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let key = format!("fwevent_{}_{}", epoch_ms, event.id);
        if let Ok(value) = serde_json::to_vec(&event) {
            let _ = self.firewall_events.insert(key.as_bytes(), &value);
        }
    }

    fn query_firewall_events(&self, query: &FirewallEventQuery) -> Vec<FirewallEvent> {
        let limit = query.limit.unwrap_or(1000);
        let mut results = Vec::new();

        for item in self.firewall_events.iter() {
            if results.len() >= limit {
                break;
            }
            if let Ok((_, value)) = item {
                if let Ok(event) = serde_json::from_slice::<FirewallEvent>(&value) {
                    if let Some(ref esme) = query.esme_id {
                        if &event.esme_id != esme {
                            continue;
                        }
                    }
                    if let Some(ref action) = query.action {
                        if &event.action != action {
                            continue;
                        }
                    }
                    if let Some(ref prefix) = query.destination_prefix {
                        if !event.destination.starts_with(prefix) {
                            continue;
                        }
                    }
                    if let Some(after) = query.after {
                        if event.timestamp < after {
                            continue;
                        }
                    }
                    if let Some(before) = query.before {
                        if event.timestamp > before {
                            continue;
                        }
                    }
                    results.push(event);
                }
            }
        }

        results
    }

    fn prune_firewall_events(&self, max_age: Duration) -> u64 {
        let cutoff = SystemTime::now() - max_age;
        let mut to_delete = Vec::new();
        let mut pruned = 0u64;

        for item in self.firewall_events.iter() {
            if let Ok((key, value)) = item {
                if let Ok(event) = serde_json::from_slice::<FirewallEvent>(&value) {
                    if event.timestamp < cutoff {
                        to_delete.push(key.to_vec());
                    }
                }
            }
        }

        for key in to_delete {
            if self.firewall_events.remove(&key).is_ok() {
                pruned += 1;
            }
        }

        pruned
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn create_test_store() -> (Arc<PersistentStorage>, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let store = PersistentStorage::open(temp_dir.path()).await.unwrap();
        (store, temp_dir)
    }

    #[tokio::test]
    async fn test_store_and_get_message() {
        let (store, _temp) = create_test_store().await;

        let msg = StoredMessage::new("SENDER", "+258841234567", b"Hello".to_vec(), "sys1", 1);
        let id = msg.id;

        store.store(msg);

        let retrieved = store.get(id).unwrap();
        assert_eq!(retrieved.destination, "+258841234567");
        assert_eq!(retrieved.source, "SENDER");
    }

    #[tokio::test]
    async fn test_update_message() {
        let (store, _temp) = create_test_store().await;

        let msg = StoredMessage::new("SRC", "DST", b"test".to_vec(), "sys1", 1);
        let id = msg.id;
        store.store(msg);

        let updated = store.update(
            id,
            Box::new(|m| {
                m.mark_in_flight("cluster1", "endpoint1");
            }),
        );
        assert!(updated);

        let retrieved = store.get(id).unwrap();
        assert_eq!(retrieved.state, MessageState::InFlight);
    }

    #[tokio::test]
    async fn test_delete_message() {
        let (store, _temp) = create_test_store().await;

        let msg = StoredMessage::new("SRC", "DST", b"test".to_vec(), "sys1", 1);
        let id = msg.id;
        store.store(msg);

        assert!(store.get(id).is_some());
        assert!(store.delete(id));
        assert!(store.get(id).is_none());
    }

    #[tokio::test]
    async fn test_get_pending() {
        let (store, _temp) = create_test_store().await;

        for i in 0..3 {
            let msg = StoredMessage::new("SRC", &format!("DST{}", i), b"test".to_vec(), "sys1", i);
            store.store(msg);
        }

        let pending = store.get_pending(10);
        assert_eq!(pending.len(), 3);
    }

    #[tokio::test]
    async fn test_message_stats() {
        let (store, _temp) = create_test_store().await;

        let msg1 = StoredMessage::new("SRC", "DST1", b"test".to_vec(), "sys1", 1);
        store.store(msg1);

        let msg2 = StoredMessage::new("SRC", "DST2", b"test".to_vec(), "sys1", 2);
        let id2 = msg2.id;
        store.store(msg2);
        store.update(id2, Box::new(|m| m.mark_in_flight("cluster", "endpoint")));

        let msg3 = StoredMessage::new("SRC", "DST3", b"test".to_vec(), "sys1", 3);
        let id3 = msg3.id;
        store.store(msg3);
        store.update(id3, Box::new(|m| m.mark_delivered("MSG123")));

        let stats = store.message_stats();
        assert_eq!(stats.total, 3);
        assert_eq!(stats.pending, 1);
        assert_eq!(stats.in_flight, 1);
        assert_eq!(stats.delivered, 1);
    }

    #[tokio::test]
    async fn test_persistence_across_restarts() {
        let temp_dir = TempDir::new().unwrap();

        let id = {
            let store = PersistentStorage::open(temp_dir.path()).await.unwrap();
            let msg = StoredMessage::new("SRC", "DST", b"persistent".to_vec(), "sys1", 1);
            let id = msg.id;
            store.store(msg);
            store.flush().unwrap();
            id
        };

        {
            let store = PersistentStorage::open(temp_dir.path()).await.unwrap();
            let retrieved = store.get(id).unwrap();
            assert_eq!(retrieved.short_message, b"persistent");
        }
    }

    #[tokio::test]
    async fn test_recoverable_messages() {
        let (store, _temp) = create_test_store().await;

        let msg1 = StoredMessage::new("SRC", "DST1", b"test".to_vec(), "sys1", 1);
        store.store(msg1);

        let mut msg2 = StoredMessage::new("SRC", "DST2", b"test".to_vec(), "sys1", 2);
        msg2.mark_in_flight("cluster", "endpoint");
        store.store(msg2);

        let mut msg3 = StoredMessage::new("SRC", "DST3", b"test".to_vec(), "sys1", 3);
        msg3.mark_delivered("MSG123");
        store.store(msg3);

        let recoverable = store.get_recoverable_messages();
        assert_eq!(recoverable.len(), 2);
    }

    #[tokio::test]
    async fn test_feedback_operations() {
        use crate::feedback::{Decision, DecisionContext, Outcome};

        let (store, _temp) = create_test_store().await;

        let ctx = DecisionContext::new("+258841234567");
        let decision = Decision::route("cluster1", vec!["cluster2".into()], ctx);
        let decision_id = store.record_decision(decision);

        let outcome = Outcome::success(decision_id, Duration::from_millis(50));
        store.record_outcome(outcome);

        let stats = store.target_stats("cluster1");
        assert_eq!(stats.total, 1);
        assert_eq!(stats.successes, 1);
        assert!(stats.avg_latency_us > 0);
    }

    #[tokio::test]
    async fn test_target_stats_persistence() {
        use crate::feedback::{Decision, DecisionContext, Outcome};

        let temp_dir = TempDir::new().unwrap();

        {
            let store = PersistentStorage::open(temp_dir.path()).await.unwrap();

            for i in 0..5 {
                let ctx = DecisionContext::new("+258841234567");
                let decision = Decision::route("cluster1", vec![], ctx);
                let id = store.record_decision(decision);
                let outcome = Outcome::success(id, Duration::from_millis(50 + i * 10));
                store.record_outcome(outcome);
            }

            store.flush().unwrap();
        }

        {
            let store = PersistentStorage::open(temp_dir.path()).await.unwrap();
            let stats = store.target_stats("cluster1");
            assert_eq!(stats.total, 5);
            assert_eq!(stats.successes, 5);
        }
    }

    #[tokio::test]
    async fn test_counter_recovery() {
        let temp_dir = TempDir::new().unwrap();

        let (initial_msg_id, initial_dec_id) = {
            let store = PersistentStorage::open(temp_dir.path()).await.unwrap();

            let msg = StoredMessage::new("SRC", "DST", b"test".to_vec(), "sys1", 1);
            let msg_id = msg.id.as_u64();
            store.store(msg);

            let ctx = crate::feedback::DecisionContext::new("DST");
            let decision = crate::feedback::Decision::route("cluster", vec![], ctx);
            let dec_id = decision.id.as_u64();
            store.record_decision(decision);

            store.flush().unwrap();
            (msg_id, dec_id)
        };

        {
            let _store = PersistentStorage::open(temp_dir.path()).await.unwrap();

            let msg = StoredMessage::new("SRC", "DST2", b"test2".to_vec(), "sys1", 2);
            let new_msg_id = msg.id.as_u64();
            assert!(new_msg_id > initial_msg_id);

            let ctx = crate::feedback::DecisionContext::new("DST2");
            let decision = crate::feedback::Decision::route("cluster2", vec![], ctx);
            let new_dec_id = decision.id.as_u64();
            assert!(new_dec_id > initial_dec_id);
        }
    }
}
