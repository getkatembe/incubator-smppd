//! In-memory feedback store implementation.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use super::types::*;
use super::FeedbackStore;

/// EMA decay factor (0.1 = slow adaptation, 0.3 = fast adaptation)
const EMA_ALPHA: f64 = 0.2;

/// Maximum decisions to keep in memory
const MAX_DECISIONS: usize = 100_000;

/// In-memory feedback store.
///
/// Thread-safe implementation using RwLock for concurrent access.
/// Suitable for single-node deployments. For clustered deployments,
/// consider implementing a distributed store (Redis, etc.).
pub struct InMemoryFeedback {
    /// Pending decisions awaiting outcomes
    decisions: RwLock<HashMap<DecisionId, Decision>>,
    /// Per-target statistics
    target_stats: RwLock<HashMap<String, TargetStatsInternal>>,
    /// Per-decision-type statistics
    type_stats: RwLock<HashMap<DecisionType, TypeStatsInternal>>,
    /// Latency samples for percentile calculation
    latency_samples: RwLock<Vec<u64>>,
}

/// Internal target stats with mutable fields
#[derive(Debug, Clone)]
struct TargetStatsInternal {
    name: String,
    total: u64,
    successes: u64,
    failures: u64,
    sum_latency_us: u64,
    ema_latency_us: f64,
    last_update: Option<Instant>,
}

impl TargetStatsInternal {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            total: 0,
            successes: 0,
            failures: 0,
            sum_latency_us: 0,
            ema_latency_us: 0.0,
            last_update: None,
        }
    }

    fn to_stats(&self) -> TargetStats {
        let avg = if self.total > 0 {
            self.sum_latency_us / self.total
        } else {
            0
        };

        TargetStats {
            name: self.name.clone(),
            total: self.total,
            successes: self.successes,
            failures: self.failures,
            avg_latency_us: avg,
            ema_latency_us: self.ema_latency_us as u64,
            last_update: self.last_update,
        }
    }

    fn record_outcome(&mut self, success: bool, latency_us: u64) {
        self.total += 1;
        if success {
            self.successes += 1;
        } else {
            self.failures += 1;
        }
        self.sum_latency_us += latency_us;

        // Update EMA
        if self.ema_latency_us == 0.0 {
            self.ema_latency_us = latency_us as f64;
        } else {
            self.ema_latency_us = EMA_ALPHA * latency_us as f64
                + (1.0 - EMA_ALPHA) * self.ema_latency_us;
        }

        self.last_update = Some(Instant::now());
    }
}

/// Internal type stats
#[derive(Debug, Clone, Default)]
struct TypeStatsInternal {
    total: u64,
    successes: u64,
    failures: u64,
    pending: u64,
    sum_latency_us: u64,
}

impl TypeStatsInternal {
    fn to_stats(&self, latency_samples: &[u64]) -> DecisionStats {
        let avg = if self.successes + self.failures > 0 {
            self.sum_latency_us / (self.successes + self.failures)
        } else {
            0
        };

        // Calculate percentiles from samples
        let (p50, p95, p99) = calculate_percentiles(latency_samples);

        DecisionStats {
            total: self.total,
            successes: self.successes,
            failures: self.failures,
            pending: self.pending,
            avg_latency_us: avg,
            p50_latency_us: p50,
            p95_latency_us: p95,
            p99_latency_us: p99,
        }
    }
}

fn calculate_percentiles(samples: &[u64]) -> (u64, u64, u64) {
    if samples.is_empty() {
        return (0, 0, 0);
    }

    let mut sorted: Vec<u64> = samples.to_vec();
    sorted.sort_unstable();

    let len = sorted.len();
    let p50_idx = (len as f64 * 0.50) as usize;
    let p95_idx = (len as f64 * 0.95) as usize;
    let p99_idx = (len as f64 * 0.99) as usize;

    (
        sorted.get(p50_idx.min(len - 1)).copied().unwrap_or(0),
        sorted.get(p95_idx.min(len - 1)).copied().unwrap_or(0),
        sorted.get(p99_idx.min(len - 1)).copied().unwrap_or(0),
    )
}

impl InMemoryFeedback {
    /// Create a new in-memory feedback store.
    pub fn new() -> Self {
        Self {
            decisions: RwLock::new(HashMap::new()),
            target_stats: RwLock::new(HashMap::new()),
            type_stats: RwLock::new(HashMap::new()),
            latency_samples: RwLock::new(Vec::with_capacity(10_000)),
        }
    }

    /// Get the number of pending decisions.
    pub fn pending_count(&self) -> usize {
        self.decisions.read().unwrap().len()
    }

    /// Get the number of tracked targets.
    pub fn target_count(&self) -> usize {
        self.target_stats.read().unwrap().len()
    }
}

impl Default for InMemoryFeedback {
    fn default() -> Self {
        Self::new()
    }
}

impl FeedbackStore for InMemoryFeedback {
    fn record_decision(&self, decision: Decision) -> DecisionId {
        let id = decision.id;
        let decision_type = decision.decision_type;

        // Store the decision
        {
            let mut decisions = self.decisions.write().unwrap();

            // Prune if too many decisions
            if decisions.len() >= MAX_DECISIONS {
                // Remove oldest 10%
                let to_remove: Vec<_> = decisions
                    .iter()
                    .take(MAX_DECISIONS / 10)
                    .map(|(k, _)| *k)
                    .collect();
                for k in to_remove {
                    decisions.remove(&k);
                }
            }

            decisions.insert(id, decision);
        }

        // Update type stats (increment pending)
        {
            let mut type_stats = self.type_stats.write().unwrap();
            let stats = type_stats.entry(decision_type).or_default();
            stats.total += 1;
            stats.pending += 1;
        }

        id
    }

    fn record_outcome(&self, outcome: Outcome) {
        let decision = {
            let mut decisions = self.decisions.write().unwrap();
            decisions.remove(&outcome.decision_id)
        };

        let Some(decision) = decision else {
            // Decision not found (already processed or pruned)
            return;
        };

        let latency_us = outcome.latency.as_micros() as u64;

        // Update target stats
        {
            let mut target_stats = self.target_stats.write().unwrap();
            let stats = target_stats
                .entry(decision.target.clone())
                .or_insert_with(|| TargetStatsInternal::new(&decision.target));
            stats.record_outcome(outcome.success, latency_us);
        }

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
            }
        }

        // Store latency sample
        {
            let mut samples = self.latency_samples.write().unwrap();
            if samples.len() >= 100_000 {
                // Keep last 50k samples
                samples.drain(0..50_000);
            }
            samples.push(latency_us);
        }
    }

    fn stats(&self, decision_type: DecisionType) -> DecisionStats {
        let type_stats = self.type_stats.read().unwrap();
        let samples = self.latency_samples.read().unwrap();

        type_stats
            .get(&decision_type)
            .map(|s| s.to_stats(&samples))
            .unwrap_or_default()
    }

    fn target_stats(&self, target: &str) -> TargetStats {
        let stats = self.target_stats.read().unwrap();
        stats
            .get(target)
            .map(|s| s.to_stats())
            .unwrap_or_else(|| TargetStats::new(target))
    }

    fn all_target_stats(&self) -> Vec<(String, TargetStats)> {
        let stats = self.target_stats.read().unwrap();
        let mut result: Vec<_> = stats
            .iter()
            .map(|(k, v)| (k.clone(), v.to_stats()))
            .collect();

        // Sort by success rate descending
        result.sort_by(|a, b| {
            b.1.success_rate()
                .partial_cmp(&a.1.success_rate())
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        result
    }

    fn prune(&self, max_age: Duration) {
        let cutoff = Instant::now() - max_age;

        // Prune old pending decisions
        {
            let mut decisions = self.decisions.write().unwrap();
            decisions.retain(|_, d| d.timestamp > cutoff);
        }

        // Reset pending counts in type stats
        {
            let decisions = self.decisions.read().unwrap();
            let mut type_stats = self.type_stats.write().unwrap();

            for (_, stats) in type_stats.iter_mut() {
                stats.pending = 0;
            }

            for (_, decision) in decisions.iter() {
                if let Some(stats) = type_stats.get_mut(&decision.decision_type) {
                    stats.pending += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_decision_and_outcome() {
        let store = InMemoryFeedback::new();

        let ctx = DecisionContext::new("+258841234567");
        let decision = Decision::route("vodacom", vec!["mtn".into()], ctx);
        let id = store.record_decision(decision);

        assert_eq!(store.pending_count(), 1);

        // Record success
        let outcome = Outcome::success(id, Duration::from_millis(50));
        store.record_outcome(outcome);

        assert_eq!(store.pending_count(), 0);

        // Check target stats
        let stats = store.target_stats("vodacom");
        assert_eq!(stats.total, 1);
        assert_eq!(stats.successes, 1);
        assert_eq!(stats.failures, 0);
        assert!(stats.avg_latency_us >= 50_000); // 50ms in microseconds
    }

    #[test]
    fn test_decision_type_stats() {
        let store = InMemoryFeedback::new();

        // Record 3 route decisions
        for i in 0..3 {
            let ctx = DecisionContext::new(format!("+25884{}", i));
            let decision = Decision::route("vodacom", vec![], ctx);
            let id = store.record_decision(decision);

            let outcome = if i < 2 {
                Outcome::success(id, Duration::from_millis(10))
            } else {
                Outcome::failure(id, Duration::from_millis(100), Some(0x14), None)
            };
            store.record_outcome(outcome);
        }

        let stats = store.stats(DecisionType::Route);
        assert_eq!(stats.total, 3);
        assert_eq!(stats.successes, 2);
        assert_eq!(stats.failures, 1);
        assert_eq!(stats.pending, 0);
    }

    #[test]
    fn test_ema_latency() {
        let store = InMemoryFeedback::new();

        // Record several decisions with varying latencies
        let latencies = [10, 20, 30, 40, 50]; // ms

        for lat in latencies {
            let ctx = DecisionContext::new("+258841234567");
            let decision = Decision::load_balance("endpoint1", vec![], ctx);
            let id = store.record_decision(decision);
            store.record_outcome(Outcome::success(id, Duration::from_millis(lat)));
        }

        let stats = store.target_stats("endpoint1");

        // EMA should be weighted toward recent values
        // With alpha=0.2, after [10,20,30,40,50]:
        // EMA starts at 10, then:
        // 0.2*20 + 0.8*10 = 12
        // 0.2*30 + 0.8*12 = 15.6
        // 0.2*40 + 0.8*15.6 = 20.48
        // 0.2*50 + 0.8*20.48 = 26.384
        // In microseconds: ~26384
        assert!(stats.ema_latency_us > 20_000);
        assert!(stats.ema_latency_us < 35_000);
    }

    #[test]
    fn test_target_score() {
        let store = InMemoryFeedback::new();

        // High success rate, low latency
        for _ in 0..10 {
            let ctx = DecisionContext::new("+258");
            let decision = Decision::load_balance("fast_endpoint", vec![], ctx);
            let id = store.record_decision(decision);
            store.record_outcome(Outcome::success(id, Duration::from_micros(500)));
        }

        // Low success rate, high latency
        for i in 0..10 {
            let ctx = DecisionContext::new("+258");
            let decision = Decision::load_balance("slow_endpoint", vec![], ctx);
            let id = store.record_decision(decision);
            if i < 5 {
                store.record_outcome(Outcome::success(id, Duration::from_millis(100)));
            } else {
                store.record_outcome(Outcome::failure(
                    id,
                    Duration::from_millis(500),
                    Some(0x14),
                    None,
                ));
            }
        }

        let fast_stats = store.target_stats("fast_endpoint");
        let slow_stats = store.target_stats("slow_endpoint");

        // Fast endpoint should have higher score
        assert!(fast_stats.score() > slow_stats.score());
        assert!(fast_stats.success_rate() > 0.99);
        assert!((slow_stats.success_rate() - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_all_target_stats_sorted() {
        let store = InMemoryFeedback::new();

        // Create targets with different success rates
        let targets = [("best", 10, 0), ("medium", 7, 3), ("worst", 3, 7)];

        for (name, successes, failures) in targets {
            for _ in 0..successes {
                let decision = Decision::load_balance(name, vec![], DecisionContext::default());
                let id = store.record_decision(decision);
                store.record_outcome(Outcome::success(id, Duration::from_millis(10)));
            }
            for _ in 0..failures {
                let decision = Decision::load_balance(name, vec![], DecisionContext::default());
                let id = store.record_decision(decision);
                store.record_outcome(Outcome::failure(id, Duration::from_millis(10), None, None));
            }
        }

        let all = store.all_target_stats();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].0, "best");
        assert_eq!(all[1].0, "medium");
        assert_eq!(all[2].0, "worst");
    }

    #[test]
    fn test_prune_old_decisions() {
        let store = InMemoryFeedback::new();

        // Record a decision but don't complete it
        let decision = Decision::route("vodacom", vec![], DecisionContext::default());
        store.record_decision(decision);

        assert_eq!(store.pending_count(), 1);

        // Prune with 0 duration (everything is old)
        store.prune(Duration::ZERO);

        assert_eq!(store.pending_count(), 0);
    }

    #[test]
    fn test_percentile_calculation() {
        let samples: Vec<u64> = (1..=100).collect();
        let (p50, p95, p99) = calculate_percentiles(&samples);

        // For 100 elements [1,2,3...100]:
        // p50 = index 50 = value 51
        // p95 = index 95 = value 96
        // p99 = index 99 = value 100
        assert_eq!(p50, 51);
        assert_eq!(p95, 96);
        assert_eq!(p99, 100);
    }

    #[test]
    fn test_decision_context_builder() {
        let ctx = DecisionContext::new("+258841234567")
            .with_source("client1")
            .with_sender_id("KATEMBE")
            .with_system_id("sys001")
            .with_route("vodacom-route")
            .with_cluster("vodacom");

        assert_eq!(ctx.destination.as_deref(), Some("+258841234567"));
        assert_eq!(ctx.source.as_deref(), Some("client1"));
        assert_eq!(ctx.sender_id.as_deref(), Some("KATEMBE"));
        assert_eq!(ctx.system_id.as_deref(), Some("sys001"));
        assert_eq!(ctx.route_name.as_deref(), Some("vodacom-route"));
        assert_eq!(ctx.cluster_name.as_deref(), Some("vodacom"));
    }
}
