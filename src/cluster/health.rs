//! Health checking for upstream endpoints.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use smpp::Client;
use tracing::{debug, trace};

use crate::config::HealthCheckConfig;

/// Health state result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthState {
    /// Endpoint is healthy
    Healthy,
    /// Endpoint is unhealthy with reason
    Unhealthy(String),
}

/// Health checker for an endpoint.
pub struct HealthChecker {
    /// Health check interval
    interval: Duration,

    /// Health check timeout
    timeout: Duration,

    /// Consecutive failures before marking unhealthy
    unhealthy_threshold: u32,

    /// Consecutive successes before marking healthy
    healthy_threshold: u32,

    /// Current consecutive failures
    consecutive_failures: AtomicU32,

    /// Current consecutive successes
    consecutive_successes: AtomicU32,
}

impl HealthChecker {
    /// Create a new health checker.
    pub fn new(config: &HealthCheckConfig) -> Self {
        Self {
            interval: config.interval,
            timeout: config.timeout,
            unhealthy_threshold: config.unhealthy_threshold,
            healthy_threshold: config.healthy_threshold,
            consecutive_failures: AtomicU32::new(0),
            consecutive_successes: AtomicU32::new(0),
        }
    }

    /// Get health check interval.
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Perform a health check using enquire_link.
    pub async fn check(
        &self,
        address: &str,
        system_id: &str,
        password: &str,
    ) -> HealthState {
        trace!(address = %address, "performing health check");

        // Try to connect (successful connection = healthy)
        // The Client internally handles enquire_link
        let result = tokio::time::timeout(self.timeout, async {
            // Create a temporary connection for health check
            let client = Client::new(address)
                .auth((system_id, password))
                .connect()
                .await?;

            // Check if connected
            if !client.is_connected().await {
                return Err(smpp::Error::ConnectionClosed);
            }

            // Close connection
            client.close().await?;

            Ok::<_, smpp::Error>(())
        })
        .await;

        match result {
            Ok(Ok(())) => {
                trace!(address = %address, "health check passed");
                HealthState::Healthy
            }
            Ok(Err(e)) => {
                debug!(address = %address, error = %e, "health check failed");
                HealthState::Unhealthy(e.to_string())
            }
            Err(_) => {
                debug!(address = %address, "health check timeout");
                HealthState::Unhealthy("timeout".to_string())
            }
        }
    }

    /// Record a successful health check.
    /// Returns true if endpoint should be marked healthy.
    pub fn record_success(&self) -> bool {
        self.consecutive_failures.store(0, Ordering::SeqCst);
        let successes = self.consecutive_successes.fetch_add(1, Ordering::SeqCst) + 1;
        successes >= self.healthy_threshold
    }

    /// Record a failed health check.
    /// Returns true if endpoint should be marked unhealthy.
    pub fn record_failure(&self) -> bool {
        self.consecutive_successes.store(0, Ordering::SeqCst);
        let failures = self.consecutive_failures.fetch_add(1, Ordering::SeqCst) + 1;
        failures >= self.unhealthy_threshold
    }

    /// Get consecutive failure count.
    pub fn failure_count(&self) -> u32 {
        self.consecutive_failures.load(Ordering::Relaxed)
    }

    /// Get consecutive success count.
    pub fn success_count(&self) -> u32 {
        self.consecutive_successes.load(Ordering::Relaxed)
    }
}
