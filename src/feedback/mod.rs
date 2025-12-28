//! Decision feedback system for adaptive routing and load balancing.
//!
//! Records all gateway decisions (routing, load balancing, retries, fallbacks)
//! and their outcomes to enable future self-improvement.
//!
//! # Architecture
//!
//! ```text
//! Request → Decision → Execution → Outcome
//!              ↓                      ↓
//!         DecisionStore ←────────────┘
//!              ↓
//!         Statistics → Adaptive Algorithms
//! ```

mod memory;
mod types;

pub use memory::InMemoryFeedback;
pub use types::*;

use std::sync::Arc;
use std::time::Duration;

/// Feedback store for recording decisions and outcomes.
///
/// Implementations should be thread-safe and lock-free where possible.
pub trait FeedbackStore: Send + Sync {
    /// Record a decision being made. Returns a decision ID for correlation.
    fn record_decision(&self, decision: Decision) -> DecisionId;

    /// Record the outcome of a decision.
    fn record_outcome(&self, outcome: Outcome);

    /// Get statistics for a specific decision type.
    fn stats(&self, decision_type: DecisionType) -> DecisionStats;

    /// Get statistics for a specific target (endpoint, cluster, route).
    fn target_stats(&self, target: &str) -> TargetStats;

    /// Get all targets with their stats, sorted by success rate.
    fn all_target_stats(&self) -> Vec<(String, TargetStats)>;

    /// Prune old data (called periodically).
    fn prune(&self, max_age: Duration);
}

/// Shared feedback store.
pub type SharedFeedback = Arc<dyn FeedbackStore>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decision_id_display() {
        let id = DecisionId::new();
        let s = format!("{}", id);
        assert!(s.starts_with("dec_"));
    }

    #[test]
    fn test_decision_type_name() {
        assert_eq!(DecisionType::Route.name(), "route");
        assert_eq!(DecisionType::LoadBalance.name(), "load_balance");
        assert_eq!(DecisionType::Retry.name(), "retry");
        assert_eq!(DecisionType::Fallback.name(), "fallback");
    }
}
