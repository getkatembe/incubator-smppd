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
//!          Storage ←─────────────────┘
//!              ↓
//!         Statistics → Adaptive Algorithms
//! ```
//!
//! Feedback operations are part of the unified [`Storage`](crate::store::Storage) trait.

pub mod types;

pub use types::*;

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
