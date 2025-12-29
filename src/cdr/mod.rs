//! Call Detail Records (CDR) module.
//!
//! Generates CDRs for billing, analytics, and audit purposes:
//! - Message submission records
//! - Delivery receipt records
//! - Revenue/cost tracking
//! - ESME/route attribution

mod types;
mod writer;

pub use types::{Cdr, CdrType, CdrField, DeliveryInfo};
pub use writer::{CdrWriter, CdrConfig, FileCdrWriter, MemoryCdrWriter};

use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

/// CDR handle for submitting records.
#[derive(Clone)]
pub struct CdrHandle {
    tx: mpsc::Sender<Cdr>,
}

impl CdrHandle {
    /// Submit a CDR record.
    pub async fn submit(&self, cdr: Cdr) {
        if let Err(e) = self.tx.send(cdr).await {
            error!(error = %e, "failed to submit CDR");
        }
    }

    /// Try to submit without blocking.
    pub fn try_submit(&self, cdr: Cdr) {
        if let Err(e) = self.tx.try_send(cdr) {
            error!(error = %e, "failed to submit CDR (non-blocking)");
        }
    }
}

/// CDR processor that writes records to configured backends.
pub struct CdrProcessor {
    rx: mpsc::Receiver<Cdr>,
    writers: Vec<Arc<dyn CdrWriter>>,
}

impl CdrProcessor {
    /// Create new CDR processor.
    pub fn new(rx: mpsc::Receiver<Cdr>, writers: Vec<Arc<dyn CdrWriter>>) -> Self {
        Self { rx, writers }
    }

    /// Run the processor until channel is closed.
    pub async fn run(mut self) {
        info!(writers = self.writers.len(), "CDR processor started");

        while let Some(cdr) = self.rx.recv().await {
            debug!(
                cdr_type = ?cdr.cdr_type,
                message_id = %cdr.message_id,
                "processing CDR"
            );

            for writer in &self.writers {
                if let Err(e) = writer.write(&cdr).await {
                    error!(
                        writer = writer.name(),
                        error = %e,
                        "failed to write CDR"
                    );
                }
            }
        }

        info!("CDR processor stopped");
    }
}

/// Start the CDR subsystem.
pub fn start(writers: Vec<Arc<dyn CdrWriter>>) -> CdrHandle {
    let (tx, rx) = mpsc::channel(10_000);

    let processor = CdrProcessor::new(rx, writers);

    tokio::spawn(async move {
        processor.run().await;
    });

    CdrHandle { tx }
}
