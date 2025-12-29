//! Router processor - routes messages to appropriate clusters.
//!
//! Subscribes to: MessageAccepted
//! Publishes: MessageRouted or MessageFailed (no route)

use tracing::{debug, instrument, warn};

use crate::bootstrap::{Event, EventCategory, MessageEnvelope, ResponseEnvelope};
use crate::bootstrap::SharedBus;
use crate::bootstrap::SharedGatewayState;
use crate::store::StoredMessage;

/// Processor that routes messages to clusters
pub struct RouterProcessor {
    bus: SharedBus,
    state: SharedGatewayState,
}

impl RouterProcessor {
    pub fn new(bus: SharedBus, state: SharedGatewayState) -> Self {
        Self { bus, state }
    }

    /// Run the router processor
    pub async fn run(self) {
        let mut rx = self.bus.subscribe_filtered(vec![EventCategory::Message]);

        loop {
            match rx.recv().await {
                Ok(Event::MessageAccepted { envelope }) => {
                    self.route_message(envelope).await;
                }
                Ok(_) => {
                    // Ignore other message events
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!(lagged = n, "router processor lagged behind");
                }
            }
        }
    }

    #[instrument(skip(self), fields(message_id = %envelope.message_id))]
    async fn route_message(&self, envelope: MessageEnvelope) {
        let dest_addr = &envelope.submit_sm.dest_addr;
        let source_addr = &envelope.submit_sm.source_addr;

        debug!(dest = %dest_addr, source = %source_addr, "routing message");

        // Store the message first
        let stored = StoredMessage::new(
            source_addr,
            dest_addr,
            envelope.submit_sm.short_message.clone(),
            &envelope.system_id,
            envelope.sequence_number,
        )
        .with_data_coding(envelope.submit_sm.data_coding)
        .with_esm_class(envelope.submit_sm.esm_class)
        .with_registered_delivery(envelope.submit_sm.registered_delivery);

        self.state.storage.store(stored);

        // Route the message
        let route_result = self
            .state
            .router
            .read()
            .await
            .route(dest_addr, Some(source_addr), None);

        match route_result {
            Some(result) => {
                debug!(
                    message_id = %envelope.message_id,
                    route = %result.route_name,
                    cluster = %result.cluster_name,
                    fallbacks = result.fallback.len(),
                    "route selected"
                );

                self.bus.publish(Event::MessageRouted {
                    envelope,
                    route_name: result.route_name,
                    cluster_name: result.cluster_name,
                    fallback_clusters: result.fallback,
                });
            }
            None => {
                warn!(
                    message_id = %envelope.message_id,
                    dest = %dest_addr,
                    "no route found"
                );

                // Update store with failure
                self.state.storage.update(
                    envelope.message_id,
                    Box::new(|m| m.mark_failed(Some(0x0000000B), "no route found")),
                );

                // Publish failure event
                self.bus.publish(Event::MessageFailed {
                    message_id: envelope.message_id,
                    session_id: envelope.session_id,
                    sequence_number: envelope.sequence_number,
                    error_code: Some(0x0000000B),
                    reason: "no route found".into(),
                    permanent: true,
                });

                // Send error response
                self.bus.publish(Event::ResponseReady(ResponseEnvelope::error(
                    envelope.session_id,
                    envelope.sequence_number,
                    envelope.message_id.to_string(),
                    0x0000000B, // ESME_RINVDSTADR
                )));
            }
        }
    }
}
