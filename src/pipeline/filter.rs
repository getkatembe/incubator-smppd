//! Filter processor - applies filter chain to incoming messages.
//!
//! Subscribes to: MessageSubmitRequest
//! Publishes: MessageAccepted or MessageRejected

use tracing::{debug, instrument, warn};

use crate::bootstrap::{Event, EventCategory, MessageEnvelope, ResponseEnvelope};
use crate::bootstrap::SharedBus;
use crate::bootstrap::SharedGatewayState;

/// Processor that applies filters to incoming messages
pub struct FilterProcessor {
    bus: SharedBus,
    state: SharedGatewayState,
}

impl FilterProcessor {
    pub fn new(bus: SharedBus, state: SharedGatewayState) -> Self {
        Self { bus, state }
    }

    /// Run the filter processor
    pub async fn run(self) {
        let mut rx = self.bus.subscribe_filtered(vec![EventCategory::Message]);

        loop {
            match rx.recv().await {
                Ok(Event::MessageSubmitRequest(envelope)) => {
                    self.process_message(envelope).await;
                }
                Ok(_) => {
                    // Ignore other message events
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!(lagged = n, "filter processor lagged behind");
                }
            }
        }
    }

    #[instrument(skip(self), fields(message_id = %envelope.message_id))]
    async fn process_message(&self, envelope: MessageEnvelope) {
        debug!(
            dest = %envelope.submit_sm.dest_addr,
            source = %envelope.submit_sm.source_addr,
            "applying filters"
        );

        // TODO: Apply actual filter chain here
        // For now, accept all messages
        //
        // Future implementation:
        // 1. Auth filter - verify client has permission
        // 2. Rate limit filter - check throughput
        // 3. Firewall filter - check content/destination rules
        // 4. ESME filter - apply ESME-specific rules

        // Check basic validation
        if envelope.submit_sm.dest_addr.is_empty() {
            self.reject_message(
                envelope,
                "empty destination address".into(),
                0x0000000B, // ESME_RINVDSTADR
            );
            return;
        }

        if envelope.submit_sm.source_addr.is_empty() {
            self.reject_message(
                envelope,
                "empty source address".into(),
                0x0000000A, // ESME_RINVSRCADR
            );
            return;
        }

        // Message passed all filters
        debug!(message_id = %envelope.message_id, "message accepted by filters");

        self.bus.publish(Event::MessageAccepted { envelope });
    }

    fn reject_message(&self, envelope: MessageEnvelope, reason: String, status: u32) {
        warn!(
            message_id = %envelope.message_id,
            reason = %reason,
            "message rejected by filters"
        );

        // Publish rejection event
        self.bus.publish(Event::MessageRejected {
            envelope: envelope.clone(),
            reason: reason.clone(),
            status,
        });

        // Also publish response ready to send error back to client
        self.bus.publish(Event::ResponseReady(ResponseEnvelope::error(
            envelope.session_id,
            envelope.sequence_number,
            envelope.message_id.to_string(),
            status,
        )));
    }
}
