use anyhow::Result;
use axum::{routing::get, Router};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use std::net::SocketAddr;
use tracing::info;

/// Metrics configuration
#[derive(Debug, Clone)]
pub struct MetricsConfig {
    /// Prometheus metrics endpoint address
    pub address: SocketAddr,

    /// Metric prefix
    pub prefix: String,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            address: "0.0.0.0:9090".parse().unwrap(),
            prefix: "smppd".to_string(),
        }
    }
}

/// Prometheus metrics server
pub struct MetricsServer {
    handle: PrometheusHandle,
    address: SocketAddr,
}

impl MetricsServer {
    /// Create and install metrics recorder
    pub fn new(config: &MetricsConfig) -> Result<Self> {
        let builder = PrometheusBuilder::new();

        let handle = builder
            .install_recorder()
            .map_err(|e| anyhow::anyhow!("failed to install metrics recorder: {}", e))?;

        // Register standard metrics
        register_standard_metrics();

        info!(
            address = %config.address,
            "metrics server configured"
        );

        Ok(Self {
            handle,
            address: config.address,
        })
    }

    /// Start the metrics HTTP server
    pub async fn serve(self) -> Result<()> {
        let app = Router::new()
            .route("/metrics", get(move || {
                let handle = self.handle.clone();
                async move { handle.render() }
            }))
            .route("/health", get(|| async { "OK" }))
            .route("/ready", get(|| async { "OK" }));

        let listener = tokio::net::TcpListener::bind(self.address).await?;

        info!(address = %self.address, "metrics server started");

        axum::serve(listener, app).await?;

        Ok(())
    }

    /// Get metrics handle
    pub fn handle(&self) -> PrometheusHandle {
        self.handle.clone()
    }
}

/// Register standard smppd metrics
fn register_standard_metrics() {
    // Server metrics
    metrics::describe_counter!(
        "smppd.connections.total",
        "Total number of connections"
    );
    metrics::describe_gauge!(
        "smppd.connections.active",
        "Number of active connections"
    );

    // Message metrics
    metrics::describe_counter!(
        "smppd.messages.total",
        "Total number of messages processed"
    );
    metrics::describe_counter!(
        "smppd.messages.submit_sm",
        "Number of submit_sm messages"
    );
    metrics::describe_counter!(
        "smppd.messages.deliver_sm",
        "Number of deliver_sm messages"
    );

    // Latency metrics
    metrics::describe_histogram!(
        "smppd.request.duration_seconds",
        "Request processing duration in seconds"
    );

    // Upstream metrics
    metrics::describe_counter!(
        "smppd.upstream.requests.total",
        "Total upstream requests"
    );
    metrics::describe_counter!(
        "smppd.upstream.errors.total",
        "Total upstream errors"
    );
    metrics::describe_gauge!(
        "smppd.upstream.connections.active",
        "Active upstream connections"
    );

    // Config reload metrics
    metrics::describe_counter!(
        "smppd.config.reloads",
        "Number of configuration reloads"
    );
    metrics::describe_counter!(
        "smppd.config.reload_failures",
        "Number of failed configuration reloads"
    );
}

/// Record a connection opened
pub fn record_connection_opened(listener: &str) {
    metrics::counter!("smppd.connections.total", "listener" => listener.to_string())
        .increment(1);
    metrics::gauge!("smppd.connections.active", "listener" => listener.to_string())
        .increment(1.0);
}

/// Record a connection closed
pub fn record_connection_closed(listener: &str) {
    metrics::gauge!("smppd.connections.active", "listener" => listener.to_string())
        .decrement(1.0);
}

/// Record message processed
pub fn record_message(command: &str, cluster: &str, status: &str) {
    metrics::counter!(
        "smppd.messages.total",
        "command" => command.to_string(),
        "cluster" => cluster.to_string(),
        "status" => status.to_string()
    )
    .increment(1);
}

/// Record request latency
pub fn record_latency(command: &str, duration_secs: f64) {
    metrics::histogram!(
        "smppd.request.duration_seconds",
        "command" => command.to_string()
    )
    .record(duration_secs);
}
