//! Admin HTTP API using Axum.
//!
//! Provides endpoints for:
//! - Health checks (/healthz, /livez, /readyz)
//! - Metrics (/metrics)
//! - Runtime stats (/stats)
//! - Store stats (/store/stats)
//! - Feedback stats (/feedback/stats)
//! - Config reload (/config/reload)

mod handlers;
mod server;

pub use handlers::{health_handler, metrics_handler, stats_handler, reload_handler};
pub use server::{AdminServer, AdminState, ReloadResult};
