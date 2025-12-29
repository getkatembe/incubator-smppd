//! Forwarder processor - forwards messages to upstream SMSCs.
//!
//! Subscribes to: MessageRouted, MessageRetryReady
//! Publishes: MessageDelivered, MessageFailed, MessageRetryScheduled, ResponseReady

use std::sync::Arc;
use std::time::{Duration, Instant};

use smpp::pdu::Pdu;
use tokio::sync::Semaphore;
use tracing::{debug, error, instrument, warn};

use crate::bootstrap::{Event, EventCategory, MessageEnvelope, ResponseEnvelope};
use crate::bootstrap::SharedBus;
use crate::bootstrap::SharedGatewayState;
use crate::store::MessageId;
use crate::telemetry::counters;

/// Processor that forwards messages to upstream SMSCs
pub struct ForwarderProcessor {
    bus: SharedBus,
    state: SharedGatewayState,
    semaphore: Arc<Semaphore>,
    upstream_timeout: Duration,
}

impl ForwarderProcessor {
    pub fn new(
        bus: SharedBus,
        state: SharedGatewayState,
        concurrency: usize,
        timeout_secs: u64,
    ) -> Self {
        Self {
            bus,
            state,
            semaphore: Arc::new(Semaphore::new(concurrency)),
            upstream_timeout: Duration::from_secs(timeout_secs),
        }
    }

    /// Run the forwarder processor
    pub async fn run(self) {
        let mut rx = self.bus.subscribe_filtered(vec![EventCategory::Message]);

        loop {
            match rx.recv().await {
                Ok(Event::MessageRouted {
                    envelope,
                    route_name,
                    cluster_name,
                    fallback_clusters,
                }) => {
                    // Spawn forwarding task with concurrency limit
                    let permit = self.semaphore.clone().acquire_owned().await;
                    let bus = self.bus.clone();
                    let state = self.state.clone();
                    let timeout = self.upstream_timeout;

                    tokio::spawn(async move {
                        let _permit = permit;
                        Self::forward_message(
                            bus,
                            state,
                            envelope,
                            route_name,
                            cluster_name,
                            fallback_clusters,
                            timeout,
                        )
                        .await;
                    });
                }
                Ok(Event::MessageRetryReady { message_id }) => {
                    // Handle retry - fetch from store and reprocess
                    self.handle_retry(message_id).await;
                }
                Ok(_) => {
                    // Ignore other message events
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!(lagged = n, "forwarder processor lagged behind");
                }
            }
        }
    }

    #[instrument(skip(bus, state, envelope), fields(message_id = %envelope.message_id))]
    async fn forward_message(
        bus: SharedBus,
        state: SharedGatewayState,
        envelope: MessageEnvelope,
        route_name: String,
        cluster_name: String,
        fallback_clusters: Vec<String>,
        timeout: Duration,
    ) {
        let start = Instant::now();
        let message_id = envelope.message_id;

        // Try primary cluster first, then fallbacks
        let mut clusters_to_try = vec![cluster_name.clone()];
        clusters_to_try.extend(fallback_clusters.clone());

        let mut last_error: Option<String> = None;
        let mut last_error_code: Option<u32> = None;

        for (attempt, cluster_name) in clusters_to_try.iter().enumerate() {
            debug!(
                message_id = %message_id,
                cluster = %cluster_name,
                attempt = attempt + 1,
                "attempting forward to cluster"
            );

            // Publish forwarding event
            bus.publish(Event::MessageForwarding {
                message_id,
                cluster: cluster_name.clone(),
                endpoint: "pending".into(),
                attempt: (attempt + 1) as u32,
            });

            match Self::try_forward_to_cluster(&state, cluster_name, &envelope, timeout).await {
                Ok(smsc_message_id) => {
                    let latency = start.elapsed();

                    debug!(
                        message_id = %message_id,
                        smsc_message_id = %smsc_message_id,
                        latency_ms = latency.as_millis(),
                        "message forwarded successfully"
                    );

                    // Update store
                    let cluster_for_update = cluster_name.clone();
                    let smsc_id_for_update = smsc_message_id.clone();
                    state.storage.update(message_id, Box::new(move |m| {
                        m.smsc_message_id = Some(smsc_id_for_update);
                        m.mark_in_flight(&cluster_for_update, "upstream");
                    }));

                    counters::message_forwarded();

                    // Publish success
                    bus.publish(Event::MessageDelivered {
                        message_id,
                        session_id: envelope.session_id,
                        sequence_number: envelope.sequence_number,
                        smsc_message_id: smsc_message_id.clone(),
                        latency_us: latency.as_micros() as u64,
                    });

                    // Send response to client
                    bus.publish(Event::ResponseReady(ResponseEnvelope::success(
                        envelope.session_id,
                        envelope.sequence_number,
                        smsc_message_id,
                    )));

                    return;
                }
                Err((code, msg)) => {
                    warn!(
                        message_id = %message_id,
                        cluster = %cluster_name,
                        error = %msg,
                        "forward attempt failed"
                    );
                    last_error = Some(msg);
                    last_error_code = code;
                }
            }
        }

        // All clusters failed
        error!(
            message_id = %message_id,
            clusters_tried = clusters_to_try.len(),
            "all forward attempts failed"
        );

        let error_msg = last_error.unwrap_or_else(|| "all clusters failed".into());
        let error_code = last_error_code.unwrap_or(0x00000058);

        // Check if we can retry
        let can_retry = state
            .storage
            .get(message_id)
            .map(|m| m.can_retry())
            .unwrap_or(false);

        if can_retry {
            // Schedule retry
            let retry_delay = Duration::from_secs(1); // TODO: exponential backoff
            let error_msg_clone = error_msg.clone();

            state.storage.update(message_id, Box::new(move |m| {
                m.mark_retry(retry_delay, Some(error_code), &error_msg_clone);
            }));

            bus.publish(Event::MessageRetryScheduled {
                message_id,
                attempt: 1,
                delay_ms: retry_delay.as_millis() as u64,
                reason: error_msg,
            });

            // Still send success to client (message will be retried in background)
            bus.publish(Event::ResponseReady(ResponseEnvelope::success(
                envelope.session_id,
                envelope.sequence_number,
                message_id.to_string(),
            )));
        } else {
            // Permanent failure
            let error_msg_clone = error_msg.clone();
            state.storage.update(message_id, Box::new(move |m| {
                m.mark_failed(Some(error_code), &error_msg_clone);
            }));

            counters::message_forward_failed();

            bus.publish(Event::MessageFailed {
                message_id,
                session_id: envelope.session_id,
                sequence_number: envelope.sequence_number,
                error_code: Some(error_code),
                reason: error_msg,
                permanent: true,
            });

            // Send error to client
            bus.publish(Event::ResponseReady(ResponseEnvelope::error(
                envelope.session_id,
                envelope.sequence_number,
                message_id.to_string(),
                error_code,
            )));
        }
    }

    async fn try_forward_to_cluster(
        state: &SharedGatewayState,
        cluster_name: &str,
        envelope: &MessageEnvelope,
        timeout: Duration,
    ) -> Result<String, (Option<u32>, String)> {
        // Get cluster
        let cluster = state
            .clusters
            .get(cluster_name)
            .ok_or((None, format!("cluster not found: {}", cluster_name)))?;

        // Handle mock clusters
        if cluster.is_mock() {
            return Ok(format!("MOCK-{}", envelope.message_id));
        }

        // Get connection from pool
        let conn = cluster
            .get_connection()
            .await
            .ok_or((None, "no healthy connection available".into()))?;

        // Send submit_sm
        let pdu = Pdu::SubmitSm(envelope.submit_sm.clone());

        let resp = tokio::time::timeout(timeout, conn.client().send_pdu(&pdu))
            .await
            .map_err(|_| (Some(0x00000058u32), "timeout waiting for response".into()))?
            .map_err(|e| (None, format!("I/O error: {}", e)))?;

        // Extract response
        match resp.pdu {
            Pdu::SubmitSmResp(ref r) => {
                let status = u32::from(resp.header.status);
                if status == 0 {
                    Ok(r.message_id.clone())
                } else {
                    Err((Some(status), format!("SMSC error: status={}", status)))
                }
            }
            other => Err((None, format!("unexpected response: {:?}", other))),
        }
    }

    async fn handle_retry(&self, message_id: MessageId) {
        // TODO: Implement retry handling
        // 1. Fetch message from store
        // 2. Recreate envelope
        // 3. Re-route and forward
        debug!(message_id = %message_id, "retry handling not yet implemented");
    }
}
