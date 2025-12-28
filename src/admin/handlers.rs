//! Admin API handlers.

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use prometheus::{Encoder, TextEncoder};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::server::ReloadResult;
use super::AdminState;

/// Health check response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// Health check handler.
pub async fn health_handler(State(state): State<Arc<AdminState>>) -> impl IntoResponse {
    let response = HealthResponse {
        status: if state.is_healthy() { "healthy" } else { "unhealthy" }.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };

    if state.is_healthy() {
        (StatusCode::OK, Json(response))
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(response))
    }
}

/// Stats response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsResponse {
    pub uptime_seconds: u64,
    pub connections: ConnectionStats,
    pub messages: MessageStats,
    pub clusters: Vec<ClusterStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionStats {
    pub active: u64,
    pub total: u64,
    pub bound: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageStats {
    pub submitted: u64,
    pub delivered: u64,
    pub failed: u64,
    pub rate_per_second: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterStats {
    pub name: String,
    pub endpoints: usize,
    pub healthy_endpoints: usize,
    pub active_connections: u64,
}

/// Stats handler.
pub async fn stats_handler(State(state): State<Arc<AdminState>>) -> impl IntoResponse {
    let response = StatsResponse {
        uptime_seconds: state.uptime().as_secs(),
        connections: ConnectionStats {
            active: state.active_connections(),
            total: state.total_connections(),
            bound: state.bound_connections(),
        },
        messages: MessageStats {
            submitted: state.messages_submitted(),
            delivered: state.messages_delivered(),
            failed: state.messages_failed(),
            rate_per_second: state.message_rate(),
        },
        clusters: state.cluster_stats(),
    };

    Json(response)
}

/// Metrics handler (Prometheus format).
pub async fn metrics_handler() -> impl IntoResponse {
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    let mut buffer = Vec::new();

    match encoder.encode(&metric_families, &mut buffer) {
        Ok(()) => {
            let output = String::from_utf8(buffer).unwrap_or_default();
            (
                StatusCode::OK,
                [("content-type", "text/plain; charset=utf-8")],
                output,
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [("content-type", "text/plain; charset=utf-8")],
            format!("Error encoding metrics: {}", e),
        ),
    }
}

/// Ready handler (for Kubernetes).
pub async fn ready_handler(State(state): State<Arc<AdminState>>) -> impl IntoResponse {
    if state.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

/// Live handler (for Kubernetes).
pub async fn live_handler() -> impl IntoResponse {
    StatusCode::OK
}

/// Config reload handler.
///
/// POST /config/reload - Reload configuration from disk
pub async fn reload_handler(State(state): State<Arc<AdminState>>) -> impl IntoResponse {
    match state.reload_config().await {
        Ok(result) => (StatusCode::OK, Json(result)),
        Err(error) => {
            let result = ReloadResult {
                success: false,
                message: error,
                reload_count: state.reload_count(),
                listeners: 0,
                clusters: 0,
                routes: 0,
            };
            (StatusCode::INTERNAL_SERVER_ERROR, Json(result))
        }
    }
}

/// Store stats response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreStatsResponse {
    pub total: u64,
    pub pending: u64,
    pub in_flight: u64,
    pub delivered: u64,
    pub failed: u64,
    pub expired: u64,
    pub retrying: u64,
}

/// Store stats handler.
///
/// GET /store/stats - Get message store statistics
pub async fn store_stats_handler(State(state): State<Arc<AdminState>>) -> impl IntoResponse {
    let stats = state.store_stats();
    let response = StoreStatsResponse {
        total: stats.total,
        pending: stats.pending,
        in_flight: stats.in_flight,
        delivered: stats.delivered,
        failed: stats.failed,
        expired: stats.expired,
        retrying: stats.retrying,
    };
    Json(response)
}

/// Feedback stats for a target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetFeedbackStats {
    pub target: String,
    pub total: u64,
    pub successes: u64,
    pub failures: u64,
    pub success_rate: f64,
    pub avg_latency_us: u64,
    pub ema_latency_us: u64,
    pub score: f64,
}

/// Feedback stats response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackStatsResponse {
    pub targets: Vec<TargetFeedbackStats>,
}

/// Feedback stats handler.
///
/// GET /feedback/stats - Get decision feedback statistics
pub async fn feedback_stats_handler(State(state): State<Arc<AdminState>>) -> impl IntoResponse {
    let all_stats = state.all_feedback_stats();
    let targets: Vec<TargetFeedbackStats> = all_stats
        .into_iter()
        .map(|(target, stats)| TargetFeedbackStats {
            target,
            total: stats.total,
            successes: stats.successes,
            failures: stats.failures,
            success_rate: stats.success_rate(),
            avg_latency_us: stats.avg_latency_us,
            ema_latency_us: stats.ema_latency_us,
            score: stats.score(),
        })
        .collect();

    Json(FeedbackStatsResponse { targets })
}
