//! Event-driven message processing pipeline.
//!
//! This module implements a fully event-driven architecture where all message
//! processing stages communicate via the EventBus:
//!
//! ```text
//! Session                    Pipeline                         Upstream
//! ───────                    ────────                         ────────
//!    │                          │                                │
//!    │ MessageSubmitRequest     │                                │
//!    ├─────────────────────────>│                                │
//!    │                          │                                │
//!    │                    ┌─────┴─────┐                          │
//!    │                    │  Filter   │                          │
//!    │                    │ Processor │                          │
//!    │                    └─────┬─────┘                          │
//!    │                          │                                │
//!    │                    MessageAccepted                        │
//!    │                          │                                │
//!    │                    ┌─────┴─────┐                          │
//!    │                    │  Router   │                          │
//!    │                    │ Processor │                          │
//!    │                    └─────┬─────┘                          │
//!    │                          │                                │
//!    │                    MessageRouted                          │
//!    │                          │                                │
//!    │                    ┌─────┴─────┐                          │
//!    │                    │ Forwarder │                          │
//!    │                    │ Processor │                          │
//!    │                    └─────┬─────┘                          │
//!    │                          │                                │
//!    │                          │ submit_sm                      │
//!    │                          ├───────────────────────────────>│
//!    │                          │                                │
//!    │                          │ submit_sm_resp                 │
//!    │                          │<───────────────────────────────┤
//!    │                          │                                │
//!    │                    MessageDelivered                       │
//!    │                          │                                │
//!    │                    ┌─────┴─────┐                          │
//!    │                    │ Response  │                          │
//!    │                    │ Processor │                          │
//!    │                    └─────┬─────┘                          │
//!    │                          │                                │
//!    │     ResponseReady        │                                │
//!    │<─────────────────────────┤                                │
//!    │                          │                                │
//! ```
//!
//! Each processor subscribes to specific event types and publishes new events
//! to continue the pipeline. This enables:
//! - Non-blocking message processing
//! - Easy addition of new processing stages
//! - Decoupled components
//! - Natural retry and error handling

mod filter;
mod forwarder;
mod response;
mod router;

pub use filter::FilterProcessor;
pub use forwarder::ForwarderProcessor;
pub use response::ResponseProcessor;
pub use router::RouterProcessor;

use std::sync::Arc;
use tokio::task::JoinHandle;
use tracing::info;

use crate::bootstrap::{SharedBus, SharedGatewayState};
use crate::listener::SharedSessionRegistry;

/// Configuration for the pipeline processors
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Maximum concurrent forwards to upstream
    pub forward_concurrency: usize,
    /// Timeout for upstream responses (seconds)
    pub upstream_timeout_secs: u64,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            forward_concurrency: 100,
            upstream_timeout_secs: 30,
        }
    }
}

/// Handles for all pipeline processors
pub struct PipelineHandles {
    pub filter: JoinHandle<()>,
    pub router: JoinHandle<()>,
    pub forwarder: JoinHandle<()>,
    pub response: JoinHandle<()>,
}

/// Start all pipeline processors
pub fn start_pipeline(
    config: PipelineConfig,
    bus: SharedBus,
    state: SharedGatewayState,
    session_registry: SharedSessionRegistry,
) -> PipelineHandles {
    info!("starting event-driven pipeline");

    // Start filter processor
    let filter = FilterProcessor::new(bus.clone(), state.clone());
    let filter_handle = tokio::spawn(async move {
        filter.run().await;
        info!("filter processor stopped");
    });

    // Start router processor
    let router = RouterProcessor::new(bus.clone(), state.clone());
    let router_handle = tokio::spawn(async move {
        router.run().await;
        info!("router processor stopped");
    });

    // Start forwarder processor
    let forwarder = ForwarderProcessor::new(
        bus.clone(),
        state.clone(),
        config.forward_concurrency,
        config.upstream_timeout_secs,
    );
    let forwarder_handle = tokio::spawn(async move {
        forwarder.run().await;
        info!("forwarder processor stopped");
    });

    // Start response processor
    let response = ResponseProcessor::new(bus.clone(), session_registry);
    let response_handle = tokio::spawn(async move {
        response.run().await;
        info!("response processor stopped");
    });

    PipelineHandles {
        filter: filter_handle,
        router: router_handle,
        forwarder: forwarder_handle,
        response: response_handle,
    }
}
