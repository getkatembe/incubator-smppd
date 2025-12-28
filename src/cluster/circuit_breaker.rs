//! Circuit breaker pattern for protecting upstream endpoints.
//!
//! States:
//! - Closed: Normal operation, requests pass through
//! - Open: Failures exceeded threshold, requests rejected
//! - HalfOpen: Testing if endpoint recovered

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

/// Circuit breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation
    Closed,
    /// Requests blocked
    Open,
    /// Testing recovery
    HalfOpen,
}

/// Circuit breaker configuration.
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Failure threshold before opening
    pub failure_threshold: u32,

    /// Success threshold before closing (in half-open state)
    pub success_threshold: u32,

    /// Time to wait before transitioning from open to half-open
    pub open_timeout: Duration,

    /// Sliding window size for failure counting
    pub window_size: Duration,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            success_threshold: 2,
            open_timeout: Duration::from_secs(30),
            window_size: Duration::from_secs(60),
        }
    }
}

/// Circuit breaker for an endpoint.
pub struct CircuitBreaker {
    /// Configuration
    config: CircuitBreakerConfig,

    /// Current state
    state: RwLock<CircuitState>,

    /// Consecutive failures
    failures: AtomicU32,

    /// Consecutive successes (in half-open state)
    successes: AtomicU32,

    /// Time when circuit opened
    opened_at: RwLock<Option<Instant>>,

    /// Total times circuit opened
    open_count: AtomicU64,
}

impl CircuitBreaker {
    /// Create a new circuit breaker with default config.
    pub fn new() -> Self {
        Self::with_config(CircuitBreakerConfig::default())
    }

    /// Create a new circuit breaker with custom config.
    pub fn with_config(config: CircuitBreakerConfig) -> Self {
        Self {
            config,
            state: RwLock::new(CircuitState::Closed),
            failures: AtomicU32::new(0),
            successes: AtomicU32::new(0),
            opened_at: RwLock::new(None),
            open_count: AtomicU64::new(0),
        }
    }

    /// Get current state.
    pub fn state(&self) -> CircuitState {
        *self.state.read().unwrap()
    }

    /// Check if circuit is closed (allowing requests).
    pub fn is_closed(&self) -> bool {
        self.state() == CircuitState::Closed
    }

    /// Check if circuit is open (blocking requests).
    pub fn is_open(&self) -> bool {
        self.state() == CircuitState::Open
    }

    /// Check if a request should be allowed.
    pub fn allow_request(&self) -> bool {
        let state = self.state();

        match state {
            CircuitState::Closed => true,
            CircuitState::HalfOpen => true, // Allow test requests
            CircuitState::Open => {
                // Check if we should transition to half-open
                if self.should_attempt_reset() {
                    self.transition_to_half_open();
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Record a successful request.
    pub fn record_success(&self) {
        let state = self.state();

        match state {
            CircuitState::Closed => {
                // Reset failure count on success
                self.failures.store(0, Ordering::SeqCst);
            }
            CircuitState::HalfOpen => {
                let successes = self.successes.fetch_add(1, Ordering::SeqCst) + 1;
                if successes >= self.config.success_threshold {
                    self.transition_to_closed();
                }
            }
            CircuitState::Open => {
                // Shouldn't happen, but reset just in case
            }
        }
    }

    /// Record a failed request.
    pub fn record_failure(&self) {
        let state = self.state();

        match state {
            CircuitState::Closed => {
                let failures = self.failures.fetch_add(1, Ordering::SeqCst) + 1;
                if failures >= self.config.failure_threshold {
                    self.transition_to_open();
                }
            }
            CircuitState::HalfOpen => {
                // Any failure in half-open goes back to open
                self.transition_to_open();
            }
            CircuitState::Open => {
                // Already open
            }
        }
    }

    /// Check if we should attempt to reset (transition to half-open).
    fn should_attempt_reset(&self) -> bool {
        if let Some(opened_at) = *self.opened_at.read().unwrap() {
            opened_at.elapsed() >= self.config.open_timeout
        } else {
            false
        }
    }

    /// Transition to closed state.
    fn transition_to_closed(&self) {
        let mut state = self.state.write().unwrap();
        if *state != CircuitState::Closed {
            info!("circuit breaker closed");
            *state = CircuitState::Closed;
            self.failures.store(0, Ordering::SeqCst);
            self.successes.store(0, Ordering::SeqCst);
            *self.opened_at.write().unwrap() = None;
        }
    }

    /// Transition to open state.
    fn transition_to_open(&self) {
        let mut state = self.state.write().unwrap();
        if *state != CircuitState::Open {
            warn!("circuit breaker opened");
            *state = CircuitState::Open;
            self.successes.store(0, Ordering::SeqCst);
            *self.opened_at.write().unwrap() = Some(Instant::now());
            self.open_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Transition to half-open state.
    fn transition_to_half_open(&self) {
        let mut state = self.state.write().unwrap();
        if *state == CircuitState::Open {
            debug!("circuit breaker half-open");
            *state = CircuitState::HalfOpen;
            self.successes.store(0, Ordering::SeqCst);
        }
    }

    /// Get failure count.
    pub fn failure_count(&self) -> u32 {
        self.failures.load(Ordering::Relaxed)
    }

    /// Get total times circuit has opened.
    pub fn open_count(&self) -> u64 {
        self.open_count.load(Ordering::Relaxed)
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_starts_closed() {
        let cb = CircuitBreaker::new();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.allow_request());
    }

    #[test]
    fn test_circuit_opens_after_failures() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            ..Default::default()
        };
        let cb = CircuitBreaker::with_config(config);

        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.allow_request());
    }

    #[test]
    fn test_success_resets_failures() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            ..Default::default()
        };
        let cb = CircuitBreaker::with_config(config);

        cb.record_failure();
        cb.record_failure();
        cb.record_success();

        assert_eq!(cb.failure_count(), 0);
        assert_eq!(cb.state(), CircuitState::Closed);
    }
}
