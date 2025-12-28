//! Message forwarder module.
//!
//! Handles the complete message lifecycle:
//! 1. Receive submit_sm from client session
//! 2. Store message in message store
//! 3. Route to appropriate cluster
//! 4. Forward to upstream SMSC
//! 5. Handle responses and update state
//! 6. Process retries and fallbacks
//! 7. Handle delivery receipts

mod dlr;
mod processor;
mod retry;

pub use dlr::{DeliveryReceipt, DeliveryStatus, DlrEvent, DlrHandle, DlrProcessor};
pub use processor::{ForwardRequest, ForwardResult, MessageForwarder};
pub use retry::RetryProcessor;

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{error, info};

use crate::bootstrap::{SharedGatewayState, Shutdown};

/// Configuration for the forwarder.
#[derive(Debug, Clone)]
pub struct ForwarderConfig {
    /// Channel buffer size for incoming messages
    pub channel_size: usize,
    /// How many messages to process concurrently
    pub concurrency: usize,
    /// Retry check interval
    pub retry_interval: Duration,
    /// Base retry delay
    pub base_retry_delay: Duration,
    /// Max retry delay (for exponential backoff)
    pub max_retry_delay: Duration,
}

impl Default for ForwarderConfig {
    fn default() -> Self {
        Self {
            channel_size: 10_000,
            concurrency: 100,
            retry_interval: Duration::from_secs(1),
            base_retry_delay: Duration::from_secs(1),
            max_retry_delay: Duration::from_secs(60),
        }
    }
}

/// Handle to send messages to the forwarder.
#[derive(Clone)]
pub struct ForwarderHandle {
    tx: mpsc::Sender<ForwardRequest>,
}

impl ForwarderHandle {
    /// Send a message to be forwarded.
    pub async fn forward(&self, request: ForwardRequest) -> Result<(), mpsc::error::SendError<ForwardRequest>> {
        self.tx.send(request).await
    }

    /// Try to send without blocking.
    pub fn try_forward(&self, request: ForwardRequest) -> Result<(), mpsc::error::TrySendError<ForwardRequest>> {
        self.tx.try_send(request)
    }
}

/// Start the forwarder subsystem.
///
/// Returns a handle for sending messages to be forwarded.
pub fn start(
    config: ForwarderConfig,
    state: SharedGatewayState,
    shutdown: Arc<Shutdown>,
) -> ForwarderHandle {
    let (tx, rx) = mpsc::channel(config.channel_size);

    // Start the main message processor
    let processor = MessageForwarder::new(
        rx,
        state.clone(),
        shutdown.clone(),
        config.concurrency,
    );

    tokio::spawn(async move {
        if let Err(e) = processor.run().await {
            error!(error = %e, "message forwarder failed");
        }
        info!("message forwarder stopped");
    });

    // Start the retry processor
    let retry_processor = RetryProcessor::new(
        state.clone(),
        shutdown.clone(),
        config.retry_interval,
        config.base_retry_delay,
        config.max_retry_delay,
    );

    tokio::spawn(async move {
        retry_processor.run().await;
        info!("retry processor stopped");
    });

    ForwarderHandle { tx }
}
