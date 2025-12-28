//! Retry processor for failed messages.
//!
//! Periodically scans the message store for messages that need to be retried
//! and resubmits them through the forwarding pipeline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use smpp::pdu::{Pdu, SubmitSm};
use tracing::{debug, info, warn};

use crate::bootstrap::{SharedGatewayState, ShutdownState};
use crate::feedback::{Decision, DecisionContext, Outcome};
use crate::store::{MessageState, StoredMessage};
use crate::telemetry::counters;

/// Maximum retry attempts before giving up.
const MAX_RETRIES: u32 = 5;

/// Retry processor that handles message retry logic.
pub struct RetryProcessor {
    state: SharedGatewayState,
    shutdown: Arc<crate::bootstrap::Shutdown>,
    retry_interval: Duration,
    base_delay: Duration,
    max_delay: Duration,
}

impl RetryProcessor {
    pub fn new(
        state: SharedGatewayState,
        shutdown: Arc<crate::bootstrap::Shutdown>,
        retry_interval: Duration,
        base_delay: Duration,
        max_delay: Duration,
    ) -> Self {
        Self {
            state,
            shutdown,
            retry_interval,
            base_delay,
            max_delay,
        }
    }

    /// Run the retry processor until shutdown.
    pub async fn run(self) {
        info!(
            interval_secs = self.retry_interval.as_secs(),
            base_delay_secs = self.base_delay.as_secs(),
            max_delay_secs = self.max_delay.as_secs(),
            "retry processor started"
        );

        let mut interval = tokio::time::interval(self.retry_interval);
        let mut shutdown_rx = self.shutdown.subscribe();

        loop {
            tokio::select! {
                biased;

                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow_and_update() != ShutdownState::Running {
                        info!("retry processor shutting down");
                        break;
                    }
                }

                _ = interval.tick() => {
                    self.process_retries().await;
                }
            }
        }
    }

    /// Process all messages ready for retry.
    async fn process_retries(&self) {
        let store = &self.state.store;

        // Get messages in Retrying state that are ready for retry
        let ready_for_retry = store.get_ready_for_retry();

        if ready_for_retry.is_empty() {
            return;
        }

        debug!(count = ready_for_retry.len(), "processing retries");

        for msg in ready_for_retry {
            self.retry_message(msg).await;
        }
    }

    /// Retry a single message.
    async fn retry_message(&self, msg: StoredMessage) {
        let message_id = msg.id;
        let store = &self.state.store;

        // Check if still in Retrying state (may have changed since we got the list)
        let current = match store.get(message_id) {
            Some(m) => m,
            None => {
                warn!(message_id = %message_id, "message not found for retry");
                return;
            }
        };

        if current.state != MessageState::Retrying {
            debug!(
                message_id = %message_id,
                state = ?current.state,
                "message no longer needs retry"
            );
            return;
        }

        // Check max retries
        if current.attempts >= MAX_RETRIES {
            info!(
                message_id = %message_id,
                retry_count = current.attempts,
                "max retries exceeded, marking as failed"
            );
            store.update(message_id, Box::new(|m| {
                m.mark_failed(Some(0x00000058), "max retries exceeded");
            }));
            counters::message_failed_simple();
            return;
        }

        // Check expiry
        if current.is_expired() {
            info!(message_id = %message_id, "message expired during retry");
            store.update(message_id, Box::new(|m| m.mark_expired()));
            counters::message_expired_simple();
            return;
        }

        // Get routing info
        let route_result = match self.state.router.read().await.route(&current.destination, Some(&current.source), None) {
            Some(r) => r,
            None => {
                warn!(message_id = %message_id, dest = %current.destination, "no route for retry");
                store.update(message_id, Box::new(|m| {
                    m.mark_failed(Some(0x0000000B), "no route found");
                }));
                counters::message_failed_simple();
                return;
            }
        };

        // Record retry decision
        let retry_ctx = DecisionContext::new(&current.destination)
            .with_source(&current.source)
            .with_route(&route_result.route_name);
        let retry_decision = Decision::retry(
            &route_result.cluster_name,
            current.attempts + 1,
            retry_ctx,
        );
        let retry_decision_id = self.state.feedback.record_decision(retry_decision);

        // Try to forward again
        let mut clusters_to_try = vec![route_result.cluster_name.clone()];
        clusters_to_try.extend(route_result.fallback.clone());

        let start = Instant::now();

        for cluster_name in &clusters_to_try {
            match self.try_forward_retry(&current, cluster_name).await {
                Ok(upstream_id) => {
                    info!(
                        message_id = %message_id,
                        upstream_id = %upstream_id,
                        cluster = %cluster_name,
                        retry_count = current.attempts + 1,
                        "retry successful"
                    );

                    // Update store
                    let cluster_for_update = cluster_name.clone();
                    let upstream_for_update = upstream_id.clone();
                    store.update(message_id, Box::new(move |m| {
                        m.smsc_message_id = Some(upstream_for_update);
                        m.mark_in_flight(&cluster_for_update, "upstream");
                    }));

                    // Record success
                    self.state.feedback.record_outcome(Outcome::success(retry_decision_id, start.elapsed()));
                    counters::message_retried();
                    return;
                }
                Err(e) => {
                    debug!(
                        message_id = %message_id,
                        cluster = %cluster_name,
                        error = %e,
                        "retry attempt failed"
                    );
                }
            }
        }

        // All clusters failed - schedule next retry with exponential backoff
        let next_delay = self.calculate_backoff(current.attempts);
        let next_delay_clone = next_delay;

        // Record failure
        self.state.feedback.record_outcome(Outcome::failure(
            retry_decision_id,
            start.elapsed(),
            Some(0x00000058),
            Some("all clusters failed".to_string()),
        ));

        store.update(message_id, Box::new(move |m| {
            m.mark_retry(next_delay_clone, Some(0x00000058), "all clusters failed");
        }));

        warn!(
            message_id = %message_id,
            attempt = current.attempts + 1,
            next_retry_secs = next_delay.as_secs(),
            "retry failed, scheduling next attempt"
        );
    }

    /// Calculate exponential backoff delay.
    fn calculate_backoff(&self, attempts: u32) -> Duration {
        let delay = self.base_delay.as_millis() as u64 * 2u64.pow(attempts);
        let capped = delay.min(self.max_delay.as_millis() as u64);
        Duration::from_millis(capped)
    }

    /// Try to forward a message to a specific cluster (retry path).
    async fn try_forward_retry(
        &self,
        msg: &StoredMessage,
        cluster_name: &str,
    ) -> Result<String, RetryError> {
        // Get cluster
        let cluster = self.state.clusters.get(cluster_name)
            .ok_or_else(|| RetryError::ClusterNotFound(cluster_name.to_string()))?;

        // Mock clusters - return synthetic ID
        if cluster.is_mock() {
            return Ok(format!("MOCK-RETRY-{}", msg.id));
        }

        // Get connection from pool
        let conn = cluster.get_connection().await
            .ok_or(RetryError::NoConnection)?;

        // Reconstruct submit_sm from stored message
        let submit_sm = self.build_submit_sm(msg);

        // Send submit_sm
        let pdu = Pdu::SubmitSm(submit_sm);

        let resp = tokio::time::timeout(
            Duration::from_secs(30),
            conn.client().send_pdu(&pdu),
        )
        .await
        .map_err(|_| RetryError::Timeout)?
        .map_err(|e| RetryError::IoError(e.to_string()))?;

        // Extract response
        match resp.pdu {
            Pdu::SubmitSmResp(ref r) => {
                let status = u32::from(resp.header.status);
                if status != 0 {
                    return Err(RetryError::SmscError { status });
                }
                Ok(r.message_id.clone())
            }
            other => {
                Err(RetryError::UnexpectedResponse(format!("{:?}", other)))
            }
        }
    }

    /// Build submit_sm from stored message.
    fn build_submit_sm(&self, msg: &StoredMessage) -> SubmitSm {
        SubmitSm {
            service_type: msg.service_type.clone().unwrap_or_default(),
            source_addr_ton: 0,
            source_addr_npi: 0,
            source_addr: msg.source.clone(),
            dest_addr_ton: 0,
            dest_addr_npi: 0,
            dest_addr: msg.destination.clone(),
            esm_class: msg.esm_class,
            protocol_id: 0,
            priority_flag: 0,
            schedule_delivery_time: String::new(),
            validity_period: String::new(),
            registered_delivery: msg.registered_delivery,
            replace_if_present_flag: 0,
            data_coding: msg.data_coding,
            sm_default_msg_id: 0,
            short_message: msg.short_message.clone(),
            tlvs: smpp::TlvMap::default(),
        }
    }
}

/// Errors during retry operations.
#[derive(Debug, thiserror::Error)]
pub enum RetryError {
    #[error("cluster not found: {0}")]
    ClusterNotFound(String),

    #[error("no healthy connection available")]
    NoConnection,

    #[error("timeout")]
    Timeout,

    #[error("I/O error: {0}")]
    IoError(String),

    #[error("SMSC error: status={status}")]
    SmscError { status: u32 },

    #[error("unexpected response: {0}")]
    UnexpectedResponse(String),
}
