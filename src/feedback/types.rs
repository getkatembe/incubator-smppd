//! Types for the decision feedback system.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Unique identifier for a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DecisionId(u64);

static DECISION_COUNTER: AtomicU64 = AtomicU64::new(0);

impl DecisionId {
    /// Create a new unique decision ID.
    pub fn new() -> Self {
        Self(DECISION_COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    /// Get the raw ID value.
    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

impl Default for DecisionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for DecisionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "dec_{}", self.0)
    }
}

/// Type of decision being made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecisionType {
    /// Route selection (which cluster to use)
    Route,
    /// Load balance selection (which endpoint within cluster)
    LoadBalance,
    /// Retry decision (retry after failure)
    Retry,
    /// Fallback decision (use fallback cluster after primary failure)
    Fallback,
}

impl DecisionType {
    /// Get the string name of this decision type.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Route => "route",
            Self::LoadBalance => "load_balance",
            Self::Retry => "retry",
            Self::Fallback => "fallback",
        }
    }
}

/// A decision record.
#[derive(Debug, Clone)]
pub struct Decision {
    /// Unique ID for this decision
    pub id: DecisionId,
    /// Type of decision
    pub decision_type: DecisionType,
    /// The target selected (cluster name, endpoint address, etc.)
    pub target: String,
    /// Alternative targets that were considered but not selected
    pub alternatives: Vec<String>,
    /// Context about the request (destination, source, etc.)
    pub context: DecisionContext,
    /// When the decision was made
    pub timestamp: Instant,
}

impl Decision {
    /// Create a new route decision.
    pub fn route(target: impl Into<String>, alternatives: Vec<String>, context: DecisionContext) -> Self {
        Self {
            id: DecisionId::new(),
            decision_type: DecisionType::Route,
            target: target.into(),
            alternatives,
            context,
            timestamp: Instant::now(),
        }
    }

    /// Create a new load balance decision.
    pub fn load_balance(
        target: impl Into<String>,
        alternatives: Vec<String>,
        context: DecisionContext,
    ) -> Self {
        Self {
            id: DecisionId::new(),
            decision_type: DecisionType::LoadBalance,
            target: target.into(),
            alternatives,
            context,
            timestamp: Instant::now(),
        }
    }

    /// Create a retry decision.
    pub fn retry(target: impl Into<String>, attempt: u32, context: DecisionContext) -> Self {
        let mut ctx = context;
        ctx.attempt = Some(attempt);
        Self {
            id: DecisionId::new(),
            decision_type: DecisionType::Retry,
            target: target.into(),
            alternatives: vec![],
            context: ctx,
            timestamp: Instant::now(),
        }
    }

    /// Create a fallback decision.
    pub fn fallback(
        from: impl Into<String>,
        to: impl Into<String>,
        remaining: Vec<String>,
        context: DecisionContext,
    ) -> Self {
        let mut ctx = context;
        ctx.fallback_from = Some(from.into());
        Self {
            id: DecisionId::new(),
            decision_type: DecisionType::Fallback,
            target: to.into(),
            alternatives: remaining,
            context: ctx,
            timestamp: Instant::now(),
        }
    }
}

/// Context about the request being processed.
#[derive(Debug, Clone, Default)]
pub struct DecisionContext {
    /// Message destination
    pub destination: Option<String>,
    /// Message source
    pub source: Option<String>,
    /// Sender ID
    pub sender_id: Option<String>,
    /// System ID (bound ESME)
    pub system_id: Option<String>,
    /// Route name (if route was already selected)
    pub route_name: Option<String>,
    /// Cluster name (if cluster was already selected)
    pub cluster_name: Option<String>,
    /// Retry attempt number
    pub attempt: Option<u32>,
    /// Original cluster (for fallback decisions)
    pub fallback_from: Option<String>,
}

impl DecisionContext {
    /// Create a new context with destination.
    pub fn new(destination: impl Into<String>) -> Self {
        Self {
            destination: Some(destination.into()),
            ..Default::default()
        }
    }

    /// Set the source.
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Set the sender ID.
    pub fn with_sender_id(mut self, sender_id: impl Into<String>) -> Self {
        self.sender_id = Some(sender_id.into());
        self
    }

    /// Set the system ID.
    pub fn with_system_id(mut self, system_id: impl Into<String>) -> Self {
        self.system_id = Some(system_id.into());
        self
    }

    /// Set the route name.
    pub fn with_route(mut self, route_name: impl Into<String>) -> Self {
        self.route_name = Some(route_name.into());
        self
    }

    /// Set the cluster name.
    pub fn with_cluster(mut self, cluster_name: impl Into<String>) -> Self {
        self.cluster_name = Some(cluster_name.into());
        self
    }
}

/// Outcome of a decision.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// The decision ID this outcome is for
    pub decision_id: DecisionId,
    /// Whether the operation succeeded
    pub success: bool,
    /// How long the operation took
    pub latency: Duration,
    /// Error code if failed (SMPP command_status)
    pub error_code: Option<u32>,
    /// Error message if failed
    pub error_message: Option<String>,
    /// When the outcome was recorded
    pub timestamp: Instant,
}

impl Outcome {
    /// Create a successful outcome.
    pub fn success(decision_id: DecisionId, latency: Duration) -> Self {
        Self {
            decision_id,
            success: true,
            latency,
            error_code: None,
            error_message: None,
            timestamp: Instant::now(),
        }
    }

    /// Create a failed outcome.
    pub fn failure(
        decision_id: DecisionId,
        latency: Duration,
        error_code: Option<u32>,
        error_message: Option<String>,
    ) -> Self {
        Self {
            decision_id,
            success: false,
            latency,
            error_code,
            error_message,
            timestamp: Instant::now(),
        }
    }
}

/// Aggregated statistics for a decision type.
#[derive(Debug, Clone, Default)]
pub struct DecisionStats {
    /// Total number of decisions
    pub total: u64,
    /// Number of successful outcomes
    pub successes: u64,
    /// Number of failed outcomes
    pub failures: u64,
    /// Number of decisions without outcomes yet
    pub pending: u64,
    /// Average latency in microseconds
    pub avg_latency_us: u64,
    /// P50 latency in microseconds
    pub p50_latency_us: u64,
    /// P95 latency in microseconds
    pub p95_latency_us: u64,
    /// P99 latency in microseconds
    pub p99_latency_us: u64,
}

impl DecisionStats {
    /// Calculate success rate (0.0 to 1.0).
    pub fn success_rate(&self) -> f64 {
        let completed = self.successes + self.failures;
        if completed == 0 {
            1.0 // No data, assume healthy
        } else {
            self.successes as f64 / completed as f64
        }
    }
}

/// Statistics for a specific target (endpoint, cluster, route).
#[derive(Debug, Clone, Default)]
pub struct TargetStats {
    /// Target name
    pub name: String,
    /// Total requests to this target
    pub total: u64,
    /// Successful requests
    pub successes: u64,
    /// Failed requests
    pub failures: u64,
    /// Average latency in microseconds
    pub avg_latency_us: u64,
    /// Exponential moving average of latency (for adaptive routing)
    pub ema_latency_us: u64,
    /// Last update time
    pub last_update: Option<Instant>,
}

impl TargetStats {
    /// Create new stats for a target.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    /// Calculate success rate (0.0 to 1.0).
    pub fn success_rate(&self) -> f64 {
        let total = self.successes + self.failures;
        if total == 0 {
            1.0
        } else {
            self.successes as f64 / total as f64
        }
    }

    /// Calculate a score for adaptive routing (higher is better).
    /// Combines success rate and latency.
    pub fn score(&self) -> f64 {
        let success_rate = self.success_rate();
        let latency_factor = if self.ema_latency_us == 0 {
            1.0
        } else {
            // Normalize latency: 1ms = 1.0, lower is better
            1000.0 / (self.ema_latency_us as f64).max(1.0)
        };

        // Weight success rate more heavily than latency
        success_rate * 0.7 + latency_factor.min(1.0) * 0.3
    }
}
