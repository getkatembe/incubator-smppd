//! Delivery Receipt (DLR) handler.
//!
//! Handles incoming delivery receipts from upstream SMSCs:
//! 1. Receives deliver_sm (DLR) from upstream connections
//! 2. Parses DLR format to extract message_id, status, error codes
//! 3. Correlates with stored messages via smsc_message_id
//! 4. Updates message store state (Delivered/Failed)
//! 5. Forwards DLRs to original client sessions

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use smpp::pdu::DeliverSm;
use smpp::{Dlr, DlrStatus};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

use crate::bootstrap::{SharedGatewayState, ShutdownState};
use crate::store::MessageState;
use crate::telemetry::counters;

/// A parsed delivery receipt.
#[derive(Debug, Clone)]
pub struct DeliveryReceipt {
    /// Original SMSC message ID
    pub smsc_message_id: String,
    /// Delivery status
    pub status: DeliveryStatus,
    /// Error code (if failed)
    pub error_code: Option<u32>,
    /// Submit date
    pub submit_date: Option<String>,
    /// Done date
    pub done_date: Option<String>,
    /// Optional text
    pub text: Option<String>,
    /// Raw DLR text
    pub raw: String,
    /// Source address from deliver_sm
    pub source_addr: String,
    /// Destination address from deliver_sm
    pub dest_addr: String,
}

/// Delivery status enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryStatus {
    /// Message is still being delivered
    Enroute,
    /// Message delivered successfully
    Delivered,
    /// Message expired before delivery
    Expired,
    /// Message was deleted
    Deleted,
    /// Message undeliverable
    Undeliverable,
    /// Message accepted by SMSC
    Accepted,
    /// Message rejected
    Rejected,
    /// Message skipped
    Skipped,
    /// Unknown status
    Unknown,
}

impl DeliveryStatus {
    /// Check if this is a final status.
    pub fn is_final(&self) -> bool {
        matches!(
            self,
            Self::Delivered | Self::Expired | Self::Deleted | Self::Undeliverable | Self::Rejected
        )
    }

    /// Check if this indicates success.
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Delivered)
    }

    /// Check if this indicates failure.
    pub fn is_failure(&self) -> bool {
        matches!(
            self,
            Self::Expired | Self::Deleted | Self::Undeliverable | Self::Rejected
        )
    }

    /// Convert to string for logging/storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Enroute => "ENROUTE",
            Self::Delivered => "DELIVRD",
            Self::Expired => "EXPIRED",
            Self::Deleted => "DELETED",
            Self::Undeliverable => "UNDELIV",
            Self::Accepted => "ACCEPTD",
            Self::Rejected => "REJECTD",
            Self::Skipped => "SKIPPED",
            Self::Unknown => "UNKNOWN",
        }
    }
}

impl From<DlrStatus> for DeliveryStatus {
    fn from(status: DlrStatus) -> Self {
        match status {
            DlrStatus::Enroute => Self::Enroute,
            DlrStatus::Delivered => Self::Delivered,
            DlrStatus::Expired => Self::Expired,
            DlrStatus::Deleted => Self::Deleted,
            DlrStatus::Undeliverable => Self::Undeliverable,
            DlrStatus::Accepted => Self::Accepted,
            DlrStatus::Rejected => Self::Rejected,
            DlrStatus::Skipped => Self::Skipped,
            DlrStatus::Unknown => Self::Unknown,
        }
    }
}

impl From<u8> for DeliveryStatus {
    fn from(v: u8) -> Self {
        match v {
            1 => Self::Enroute,
            2 => Self::Delivered,
            3 => Self::Expired,
            4 => Self::Deleted,
            5 => Self::Undeliverable,
            6 => Self::Accepted,
            8 => Self::Rejected,
            9 => Self::Skipped,
            _ => Self::Unknown,
        }
    }
}

/// Handle for sending DLRs to the processor.
#[derive(Clone)]
pub struct DlrHandle {
    tx: mpsc::Sender<DlrEvent>,
}

impl DlrHandle {
    /// Send a DLR event for processing.
    pub async fn send(&self, event: DlrEvent) -> Result<(), mpsc::error::SendError<DlrEvent>> {
        self.tx.send(event).await
    }

    /// Try to send without blocking.
    pub fn try_send(&self, event: DlrEvent) -> Result<(), mpsc::error::TrySendError<DlrEvent>> {
        self.tx.try_send(event)
    }
}

/// DLR event from upstream connection.
#[derive(Debug)]
pub struct DlrEvent {
    /// The deliver_sm PDU
    pub deliver_sm: DeliverSm,
    /// Cluster name the DLR came from
    pub cluster: String,
    /// Endpoint address
    pub endpoint: String,
    /// When the DLR was received
    pub received_at: Instant,
}

/// Client session registration for DLR forwarding.
pub struct ClientSession {
    /// System ID of the client
    pub system_id: String,
    /// Channel to send DLRs to the client
    pub tx: mpsc::Sender<DeliverSm>,
}

/// DLR processor.
///
/// Handles correlation and forwarding of delivery receipts.
pub struct DlrProcessor {
    rx: mpsc::Receiver<DlrEvent>,
    state: SharedGatewayState,
    shutdown_rx: tokio::sync::watch::Receiver<ShutdownState>,
    /// Registered client sessions for DLR forwarding
    /// Key: system_id
    sessions: Arc<RwLock<HashMap<String, mpsc::Sender<DeliverSm>>>>,
}

impl DlrProcessor {
    pub fn new(
        rx: mpsc::Receiver<DlrEvent>,
        state: SharedGatewayState,
        shutdown: Arc<crate::bootstrap::Shutdown>,
    ) -> Self {
        Self {
            rx,
            state,
            shutdown_rx: shutdown.subscribe(),
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get a handle to register client sessions.
    pub fn sessions(&self) -> Arc<RwLock<HashMap<String, mpsc::Sender<DeliverSm>>>> {
        self.sessions.clone()
    }

    /// Run the DLR processor until shutdown.
    pub async fn run(mut self) {
        info!("DLR processor started");

        loop {
            tokio::select! {
                biased;

                _ = self.shutdown_rx.changed() => {
                    if *self.shutdown_rx.borrow_and_update() != ShutdownState::Running {
                        info!("DLR processor shutting down");
                        break;
                    }
                }

                event = self.rx.recv() => {
                    let Some(event) = event else {
                        info!("DLR channel closed");
                        break;
                    };

                    self.process_dlr(event).await;
                }
            }
        }
    }

    /// Process a single DLR event.
    async fn process_dlr(&self, event: DlrEvent) {
        let deliver = &event.deliver_sm;

        // Parse the DLR
        let dlr = match self.parse_dlr(deliver) {
            Some(dlr) => dlr,
            None => {
                // Not a DLR, might be a regular MO message
                debug!(
                    source = %deliver.source_addr,
                    dest = %deliver.dest_addr,
                    "received non-DLR deliver_sm"
                );
                counters::dlr_received(&event.cluster, "mo");
                return;
            }
        };

        debug!(
            smsc_message_id = %dlr.smsc_message_id,
            status = %dlr.status.as_str(),
            cluster = %event.cluster,
            "DLR received"
        );

        counters::dlr_received(&event.cluster, dlr.status.as_str());

        // Step 1: Find the stored message by smsc_message_id
        let message = self.find_message_by_smsc_id(&dlr.smsc_message_id);

        let Some(message) = message else {
            warn!(
                smsc_message_id = %dlr.smsc_message_id,
                "DLR for unknown message"
            );
            counters::dlr_orphaned(&event.cluster);
            return;
        };

        let message_id = message.id;
        let client_system_id = message.system_id.clone();

        // Step 2: Update message store based on DLR status
        if dlr.status.is_final() {
            let status_str = dlr.status.as_str().to_string();
            let error_code = dlr.error_code;

            self.state.store.update(message_id, Box::new(move |m| {
                // Record the DLR event
                m.record_delivery_receipt(&status_str, error_code);

                // Update state based on DLR status
                if dlr.status.is_success() {
                    m.state = MessageState::Delivered;
                } else if dlr.status.is_failure() {
                    m.state = MessageState::Failed;
                    m.last_error_code = error_code;
                    m.last_error = Some(format!("DLR: {}", status_str));
                }
            }));

            if dlr.status.is_success() {
                info!(
                    message_id = %message_id,
                    smsc_message_id = %dlr.smsc_message_id,
                    "message delivered"
                );
                counters::message_delivered_dlr();
            } else {
                warn!(
                    message_id = %message_id,
                    smsc_message_id = %dlr.smsc_message_id,
                    status = %dlr.status.as_str(),
                    error = ?dlr.error_code,
                    "message delivery failed"
                );
                counters::message_failed_dlr();
            }
        }

        // Step 3: Forward DLR to client if session is registered
        self.forward_to_client(&client_system_id, event.deliver_sm.clone()).await;
    }

    /// Parse a deliver_sm as a DLR.
    fn parse_dlr(&self, deliver: &DeliverSm) -> Option<DeliveryReceipt> {
        // Check if this is a DLR (esm_class bit 2 set, or parse text format)
        if !deliver.is_delivery_receipt() {
            return None;
        }

        let text = String::from_utf8_lossy(&deliver.short_message);

        // Try to parse standard DLR format:
        // id:MSGID sub:001 dlvrd:001 submit date:YYMMDDHHMMSS done date:YYMMDDHHMMSS stat:DELIVRD err:000 text:...
        if let Some(dlr) = Dlr::parse(&text) {
            return Some(DeliveryReceipt {
                smsc_message_id: dlr.message_id,
                status: DeliveryStatus::from(dlr.status),
                error_code: dlr.error_code.and_then(|s| s.parse().ok()),
                submit_date: dlr.submit_date,
                done_date: dlr.done_date,
                text: dlr.text,
                raw: text.to_string(),
                source_addr: deliver.source_addr.clone(),
                dest_addr: deliver.dest_addr.clone(),
            });
        }

        // Try TLV-based DLR (receipted_message_id TLV)
        if let Some(msg_id) = deliver.tlvs.get_string(0x001E_u16) {
            // 0x001E = receipted_message_id
            let state = deliver.tlvs.get_u8(0x0427_u16).unwrap_or(7); // 0x0427 = message_state
            return Some(DeliveryReceipt {
                smsc_message_id: msg_id,
                status: DeliveryStatus::from(state),
                error_code: deliver.tlvs.get_u8(0x0423_u16).map(|e| e as u32), // 0x0423 = network_error_code
                submit_date: None,
                done_date: None,
                text: None,
                raw: text.to_string(),
                source_addr: deliver.source_addr.clone(),
                dest_addr: deliver.dest_addr.clone(),
            });
        }

        None
    }

    /// Find a stored message by its SMSC message ID.
    fn find_message_by_smsc_id(&self, smsc_id: &str) -> Option<crate::store::StoredMessage> {
        // Query the store for messages with this smsc_message_id
        // Since MessageQuery doesn't have smsc_message_id filter, we need to iterate
        // This could be optimized with an index

        // Get all in-flight messages (most likely to have recent DLRs)
        let query = crate::store::MessageQuery::new()
            .with_state(MessageState::InFlight)
            .with_limit(1000);

        let messages = self.state.store.query(&query);

        for msg in messages {
            if msg.smsc_message_id.as_deref() == Some(smsc_id) {
                return Some(msg);
            }
        }

        // Also check retrying messages
        let query = crate::store::MessageQuery::new()
            .with_state(MessageState::Retrying)
            .with_limit(1000);

        let messages = self.state.store.query(&query);

        for msg in messages {
            if msg.smsc_message_id.as_deref() == Some(smsc_id) {
                return Some(msg);
            }
        }

        None
    }

    /// Forward DLR to the original client session.
    async fn forward_to_client(&self, system_id: &str, deliver: DeliverSm) {
        let sessions = self.sessions.read().await;

        if let Some(tx) = sessions.get(system_id) {
            if let Err(e) = tx.try_send(deliver) {
                warn!(
                    system_id = %system_id,
                    error = %e,
                    "failed to forward DLR to client"
                );
            } else {
                debug!(system_id = %system_id, "DLR forwarded to client");
                counters::dlr_forwarded(system_id);
            }
        } else {
            debug!(
                system_id = %system_id,
                "no registered session for DLR forwarding"
            );
        }
    }
}

/// Start the DLR processor.
///
/// Returns a handle for sending DLRs and the sessions registry.
pub fn start(
    state: SharedGatewayState,
    shutdown: Arc<crate::bootstrap::Shutdown>,
) -> (DlrHandle, Arc<RwLock<HashMap<String, mpsc::Sender<DeliverSm>>>>) {
    let (tx, rx) = mpsc::channel(10_000);

    let processor = DlrProcessor::new(rx, state, shutdown);
    let sessions = processor.sessions();

    tokio::spawn(async move {
        processor.run().await;
        info!("DLR processor stopped");
    });

    (DlrHandle { tx }, sessions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_delivery_status_from_u8() {
        assert_eq!(DeliveryStatus::from(1u8), DeliveryStatus::Enroute);
        assert_eq!(DeliveryStatus::from(2u8), DeliveryStatus::Delivered);
        assert_eq!(DeliveryStatus::from(3u8), DeliveryStatus::Expired);
        assert_eq!(DeliveryStatus::from(5u8), DeliveryStatus::Undeliverable);
        assert_eq!(DeliveryStatus::from(99u8), DeliveryStatus::Unknown);
    }

    #[test]
    fn test_delivery_status_is_final() {
        assert!(DeliveryStatus::Delivered.is_final());
        assert!(DeliveryStatus::Expired.is_final());
        assert!(DeliveryStatus::Undeliverable.is_final());
        assert!(!DeliveryStatus::Enroute.is_final());
        assert!(!DeliveryStatus::Accepted.is_final());
    }

    #[test]
    fn test_delivery_status_success_failure() {
        assert!(DeliveryStatus::Delivered.is_success());
        assert!(!DeliveryStatus::Expired.is_success());

        assert!(DeliveryStatus::Expired.is_failure());
        assert!(DeliveryStatus::Undeliverable.is_failure());
        assert!(!DeliveryStatus::Delivered.is_failure());
    }
}
