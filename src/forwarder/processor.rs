//! Message forwarding processor.
//!
//! Receives submit_sm messages from client sessions and forwards them
//! to upstream SMSCs via the cluster/pool infrastructure.

use std::sync::Arc;
use std::time::{Duration, Instant};

use smpp::pdu::{Pdu, SubmitSm};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, instrument, warn};

use crate::bootstrap::{SharedGatewayState, ShutdownState};
use crate::cluster::PooledConnection;
use crate::feedback::{Decision, DecisionContext, Outcome};
use crate::store::{MessageId, StoredMessage};
use crate::telemetry::counters;

/// Request to forward a message.
#[derive(Debug)]
pub struct ForwardRequest {
    /// The submit_sm PDU to forward
    pub submit_sm: SubmitSm,
    /// System ID of the client that submitted this message
    pub client_system_id: String,
    /// Sequence number from the original PDU
    pub sequence_number: u32,
    /// Response channel for the submit_sm_resp
    pub response_tx: oneshot::Sender<ForwardResult>,
}

/// Result of a forward operation.
#[derive(Debug)]
pub struct ForwardResult {
    /// Message ID assigned (local or upstream)
    pub message_id: String,
    /// Command status (0 = success)
    pub command_status: u32,
    /// Error description if failed
    pub error: Option<String>,
}

impl ForwardResult {
    pub fn success(message_id: String) -> Self {
        Self {
            message_id,
            command_status: 0,
            error: None,
        }
    }

    pub fn error(message_id: String, command_status: u32, error: String) -> Self {
        Self {
            message_id,
            command_status,
            error: Some(error),
        }
    }
}

/// Main message forwarding processor.
///
/// Receives messages from client sessions and forwards them to upstream SMSCs.
pub struct MessageForwarder {
    rx: mpsc::Receiver<ForwardRequest>,
    state: SharedGatewayState,
    shutdown_rx: tokio::sync::watch::Receiver<crate::bootstrap::ShutdownState>,
    concurrency: usize,
}

impl MessageForwarder {
    pub fn new(
        rx: mpsc::Receiver<ForwardRequest>,
        state: SharedGatewayState,
        shutdown: Arc<crate::bootstrap::Shutdown>,
        concurrency: usize,
    ) -> Self {
        Self {
            rx,
            state,
            shutdown_rx: shutdown.subscribe(),
            concurrency,
        }
    }

    /// Run the forwarder, processing messages until shutdown.
    pub async fn run(mut self) -> anyhow::Result<()> {
        info!(concurrency = self.concurrency, "message forwarder started");

        // Use a semaphore to limit concurrent forwards
        let semaphore = Arc::new(tokio::sync::Semaphore::new(self.concurrency));

        loop {
            tokio::select! {
                biased;

                // Check shutdown state changes
                _ = self.shutdown_rx.changed() => {
                    if *self.shutdown_rx.borrow_and_update() != ShutdownState::Running {
                        info!("message forwarder shutting down");
                        break;
                    }
                }

                // Receive forward request
                request = self.rx.recv() => {
                    let Some(request) = request else {
                        info!("forward channel closed");
                        break;
                    };

                    // Acquire permit for concurrency limiting
                    let permit = semaphore.clone().acquire_owned().await;
                    let state = self.state.clone();

                    // Process in background task
                    tokio::spawn(async move {
                        let _permit = permit;
                        Self::process_request(request, state).await;
                    });
                }
            }
        }

        Ok(())
    }

    /// Process a single forward request.
    #[instrument(skip_all, fields(dest = %request.submit_sm.dest_addr))]
    async fn process_request(
        request: ForwardRequest,
        state: SharedGatewayState,
    ) {
        let start = Instant::now();
        let dest_addr = request.submit_sm.dest_addr.clone();
        let source_addr = request.submit_sm.source_addr.clone();

        // Step 1: Store the message
        let stored = StoredMessage::new(
            &source_addr,
            &dest_addr,
            request.submit_sm.short_message.clone(),
            &request.client_system_id,
            request.sequence_number,
        )
        .with_data_coding(request.submit_sm.data_coding)
        .with_esm_class(request.submit_sm.esm_class)
        .with_registered_delivery(request.submit_sm.registered_delivery);

        let message_id = stored.id;
        let message_id_str = message_id.to_string();

        state.store.store(stored);
        debug!(message_id = %message_id, "message stored");

        // Step 2: Route the message
        let route_result = match state.router.read().await.route(&dest_addr, Some(&source_addr), None) {
            Some(result) => result,
            None => {
                warn!(dest = %dest_addr, "no route found");
                state.store.update(
                    message_id,
                    Box::new(|m| m.mark_failed(Some(0x0000000B), "no route found")),
                );
                let _ = request.response_tx.send(ForwardResult::error(
                    message_id_str,
                    0x0000000B, // ESME_RINVDSTADR
                    "no route found".to_string(),
                ));
                counters::message_forward_failed();
                return;
            }
        };

        // Record route decision in feedback store
        let route_ctx = DecisionContext::new(&dest_addr)
            .with_source(&source_addr)
            .with_route(&route_result.route_name);
        let route_decision = Decision::route(
            &route_result.cluster_name,
            route_result.fallback.clone(),
            route_ctx,
        );
        let route_decision_id = state.feedback.record_decision(route_decision);

        debug!(
            message_id = %message_id,
            cluster = %route_result.cluster_name,
            fallbacks = route_result.fallback.len(),
            "route selected"
        );

        // Step 3: Try primary cluster, then fallbacks
        let mut clusters_to_try = vec![route_result.cluster_name.clone()];
        clusters_to_try.extend(route_result.fallback.clone());

        let mut last_error: Option<ForwardError> = None;

        for (i, cluster_name) in clusters_to_try.iter().enumerate() {
            match Self::try_forward_to_cluster(
                &state,
                cluster_name,
                message_id,
                &request.submit_sm,
            ).await {
                Ok(upstream_id) => {
                    // Success!
                    let latency = start.elapsed();
                    debug!(
                        message_id = %message_id,
                        upstream_id = %upstream_id,
                        cluster = %cluster_name,
                        latency_ms = latency.as_millis(),
                        "message forwarded successfully"
                    );

                    // Update store with upstream ID and mark in-flight
                    let cluster_for_update = cluster_name.clone();
                    let upstream_for_update = upstream_id.clone();
                    state.store.update(message_id, Box::new(move |m| {
                        m.smsc_message_id = Some(upstream_for_update);
                        m.mark_in_flight(&cluster_for_update, "upstream");
                    }));

                    // Record success in feedback
                    state.feedback.record_outcome(Outcome::success(route_decision_id, latency));
                    counters::message_forwarded();

                    let _ = request.response_tx.send(ForwardResult::success(message_id_str));
                    return;
                }
                Err(e) => {
                    warn!(
                        message_id = %message_id,
                        cluster = %cluster_name,
                        error = %e,
                        fallback_attempt = i,
                        "forward attempt failed"
                    );
                    last_error = Some(e);
                }
            }
        }

        // All clusters failed - mark for retry
        error!(
            message_id = %message_id,
            clusters_tried = clusters_to_try.len(),
            "all forward attempts failed"
        );

        let error_msg = last_error
            .as_ref()
            .map(|e| e.to_string())
            .unwrap_or_else(|| "all clusters failed".to_string());

        // Record failure in feedback
        state.feedback.record_outcome(Outcome::failure(
            route_decision_id,
            start.elapsed(),
            Some(0x00000058),
            Some(error_msg.clone()),
        ));

        // Check if retriable
        let can_retry = state.store
            .get(message_id)
            .map(|m| m.can_retry())
            .unwrap_or(false);

        if can_retry {
            // Schedule retry with backoff
            let retry_delay = Duration::from_secs(1);
            state.store.update(message_id, Box::new(move |m| {
                m.mark_retry(retry_delay, Some(0x00000058), &error_msg);
            }));
        } else {
            state.store.update(message_id, Box::new(move |m| {
                m.mark_failed(Some(0x00000058), &error_msg);
            }));
        }

        counters::message_forward_failed();

        // Still respond to client with the local message ID
        // The message will be retried in the background if can_retry
        let _ = request.response_tx.send(ForwardResult::success(message_id_str));
    }

    /// Try to forward a message to a specific cluster.
    async fn try_forward_to_cluster(
        state: &SharedGatewayState,
        cluster_name: &str,
        message_id: MessageId,
        submit_sm: &SubmitSm,
    ) -> Result<String, ForwardError> {
        // Get cluster
        let cluster = state.clusters.get(cluster_name)
            .ok_or_else(|| ForwardError::ClusterNotFound(cluster_name.to_string()))?;

        // Mock clusters are handled internally by the cluster
        if cluster.is_mock() {
            // For mock clusters, generate a synthetic message ID
            return Ok(format!("MOCK-{}", message_id));
        }

        // Get connection from pool
        let conn = cluster.get_connection().await
            .ok_or(ForwardError::NoConnection)?;

        // Send submit_sm
        let resp = Self::send_submit_sm(conn, submit_sm).await?;

        // Record response in store
        let cmd_status = resp.0;
        let msg_id = resp.1.clone();
        state.store.update(message_id, Box::new(move |m| {
            m.record_response(cmd_status, Some(msg_id), 0);
        }));

        // Check response status
        if resp.0 != 0 {
            return Err(ForwardError::SmscError {
                status: resp.0,
                message_id: resp.1,
            });
        }

        Ok(resp.1)
    }

    /// Send submit_sm to upstream SMSC.
    async fn send_submit_sm(
        conn: PooledConnection,
        submit_sm: &SubmitSm,
    ) -> Result<(u32, String), ForwardError> {
        // Create submit_sm PDU
        let pdu = Pdu::SubmitSm(submit_sm.clone());

        // Send and wait for response with timeout
        let resp = tokio::time::timeout(
            Duration::from_secs(30),
            conn.client().send_pdu(&pdu),
        )
        .await
        .map_err(|_| ForwardError::Timeout)?
        .map_err(|e| ForwardError::IoError(e.to_string()))?;

        // Extract submit_sm_resp from PduFrame
        match resp.pdu {
            Pdu::SubmitSmResp(ref r) => {
                Ok((u32::from(resp.header.status), r.message_id.clone()))
            }
            other => Err(ForwardError::UnexpectedResponse(format!("{:?}", other))),
        }
    }
}

/// Errors that can occur during forwarding.
#[derive(Debug, thiserror::Error)]
pub enum ForwardError {
    #[error("cluster not found: {0}")]
    ClusterNotFound(String),

    #[error("no healthy connection available")]
    NoConnection,

    #[error("timeout waiting for response")]
    Timeout,

    #[error("I/O error: {0}")]
    IoError(String),

    #[error("SMSC error: status={status}, message_id={message_id}")]
    SmscError { status: u32, message_id: String },

    #[error("unexpected response: {0}")]
    UnexpectedResponse(String),
}
