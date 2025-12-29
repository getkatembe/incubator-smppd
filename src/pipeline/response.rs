//! Response processor - routes responses back to client sessions.
//!
//! Subscribes to: ResponseReady
//! Actions: Sends responses via SessionRegistry

use tracing::{debug, warn};

use crate::bootstrap::{Event, EventCategory};
use crate::bootstrap::SharedBus;
use crate::listener::SharedSessionRegistry;

/// Processor that routes responses back to client sessions
pub struct ResponseProcessor {
    bus: SharedBus,
    session_registry: SharedSessionRegistry,
}

impl ResponseProcessor {
    pub fn new(bus: SharedBus, session_registry: SharedSessionRegistry) -> Self {
        Self {
            bus,
            session_registry,
        }
    }

    /// Run the response processor
    pub async fn run(self) {
        let mut rx = self.bus.subscribe_filtered(vec![EventCategory::Message]);

        loop {
            match rx.recv().await {
                Ok(Event::ResponseReady(response)) => {
                    debug!(
                        session_id = %response.session_id,
                        sequence = response.sequence_number,
                        message_id = %response.message_id,
                        status = response.command_status,
                        "routing response to session"
                    );

                    if !self.session_registry.send_response(response.clone()).await {
                        warn!(
                            session_id = %response.session_id,
                            "failed to send response to session"
                        );
                    }
                }
                Ok(_) => {
                    // Ignore other events
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!(lagged = n, "response processor lagged behind");
                }
            }
        }
    }
}
