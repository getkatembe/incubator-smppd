use anyhow::Result;
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use opentelemetry::metrics::MeterProvider;
use opentelemetry_prometheus::exporter;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use prometheus::{Encoder, Registry, TextEncoder};
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tracing::info;

/// Metrics configuration
#[derive(Debug, Clone)]
pub struct MetricsConfig {
    /// HTTP endpoint address
    pub address: SocketAddr,
}

/// Dependency status for readiness checks
#[derive(Debug, Clone, Serialize)]
pub struct DependencyStatus {
    pub name: String,
    pub healthy: bool,
    pub message: Option<String>,
}

/// Admin state for health/stats endpoints
#[derive(Debug)]
pub struct AdminState {
    start_time: Instant,
    healthy: AtomicBool,
    ready: AtomicBool,
    active_connections: AtomicU64,
    total_connections: AtomicU64,
    messages_submitted: AtomicU64,
    messages_delivered: AtomicU64,
    messages_failed: AtomicU64,
    /// Upstream cluster health status
    cluster_health: std::sync::RwLock<std::collections::HashMap<String, bool>>,
    /// Required clusters that must be healthy for readiness
    required_clusters: std::sync::RwLock<Vec<String>>,
}

impl AdminState {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            healthy: AtomicBool::new(true),
            ready: AtomicBool::new(true), // Ready by default if no dependencies registered
            active_connections: AtomicU64::new(0),
            total_connections: AtomicU64::new(0),
            messages_submitted: AtomicU64::new(0),
            messages_delivered: AtomicU64::new(0),
            messages_failed: AtomicU64::new(0),
            cluster_health: std::sync::RwLock::new(std::collections::HashMap::new()),
            required_clusters: std::sync::RwLock::new(Vec::new()),
        }
    }

    pub fn uptime_secs(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }

    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Relaxed)
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }

    pub fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Relaxed);
    }

    pub fn inc_connections(&self) {
        self.active_connections.fetch_add(1, Ordering::Relaxed);
        self.total_connections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec_connections(&self) {
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn inc_submitted(&self) {
        self.messages_submitted.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_delivered(&self) {
        self.messages_delivered.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_failed(&self) {
        self.messages_failed.fetch_add(1, Ordering::Relaxed);
    }

    /// Register a required cluster for readiness checks
    pub fn register_cluster(&self, cluster: &str) {
        {
            let mut required = self.required_clusters.write().unwrap();
            if !required.contains(&cluster.to_string()) {
                required.push(cluster.to_string());
            }
            // Initially mark as unhealthy
            let mut health = self.cluster_health.write().unwrap();
            health.entry(cluster.to_string()).or_insert(false);
        }
        // Update readiness state
        self.update_readiness();
    }

    /// Update cluster health status
    pub fn set_cluster_health(&self, cluster: &str, healthy: bool) {
        let mut health = self.cluster_health.write().unwrap();
        health.insert(cluster.to_string(), healthy);
        drop(health);
        // Recalculate overall readiness
        self.update_readiness();
    }

    /// Check if all required clusters are healthy
    fn update_readiness(&self) {
        let required = self.required_clusters.read().unwrap();
        let health = self.cluster_health.read().unwrap();

        // If no required clusters, we're ready
        if required.is_empty() {
            self.ready.store(true, Ordering::Relaxed);
            return;
        }

        // Check all required clusters are healthy
        let all_healthy = required.iter().all(|c| *health.get(c).unwrap_or(&false));
        self.ready.store(all_healthy, Ordering::Relaxed);
    }

    /// Get dependency status for readiness response
    pub fn get_dependency_status(&self) -> Vec<DependencyStatus> {
        let required = self.required_clusters.read().unwrap();
        let health = self.cluster_health.read().unwrap();

        required
            .iter()
            .map(|name| {
                let healthy = *health.get(name).unwrap_or(&false);
                DependencyStatus {
                    name: name.clone(),
                    healthy,
                    message: if healthy {
                        None
                    } else {
                        Some("cluster unhealthy or unreachable".to_string())
                    },
                }
            })
            .collect()
    }

    /// Mark service as ready (for initial startup without clusters)
    pub fn mark_ready(&self) {
        self.ready.store(true, Ordering::Relaxed);
    }
}

impl Default for AdminState {
    fn default() -> Self {
        Self::new()
    }
}

/// OTEL Metrics with Prometheus exporter
pub struct Metrics {
    registry: Registry,
    meter_provider: SdkMeterProvider,
    address: SocketAddr,
    admin_state: Arc<AdminState>,
}

impl Metrics {
    /// Create metrics with OTEL → Prometheus pipeline
    pub fn new(config: &MetricsConfig) -> Result<Arc<Self>> {
        let registry = Registry::new();

        let exporter = exporter()
            .with_registry(registry.clone())
            .build()?;

        let meter_provider = SdkMeterProvider::builder()
            .with_reader(exporter)
            .build();

        // Register as global meter provider
        opentelemetry::global::set_meter_provider(meter_provider.clone());

        info!(
            address = %config.address,
            "OTEL metrics configured with Prometheus exporter"
        );

        Ok(Arc::new(Self {
            registry,
            meter_provider,
            address: config.address,
            admin_state: Arc::new(AdminState::new()),
        }))
    }

    /// Get admin state for updating metrics from other components
    pub fn admin_state(&self) -> Arc<AdminState> {
        self.admin_state.clone()
    }

    /// Get meter provider
    pub fn meter_provider(&self) -> &SdkMeterProvider {
        &self.meter_provider
    }

    /// Get a meter for recording metrics
    pub fn meter(&self, name: &'static str) -> opentelemetry::metrics::Meter {
        self.meter_provider.meter(name)
    }

    /// Render metrics in Prometheus format
    pub fn render(&self) -> String {
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();

        let mut buffer = Vec::new();
        encoder.encode(&metric_families, &mut buffer).unwrap();

        String::from_utf8(buffer).unwrap()
    }

    /// Start the metrics HTTP server
    pub async fn serve(self: Arc<Self>) -> Result<()> {
        let metrics = self.clone();
        let admin_state = self.admin_state.clone();

        let app = Router::new()
            // Prometheus metrics
            .route("/metrics", get(move || {
                let m = metrics.clone();
                async move { m.render() }
            }))
            // Kubernetes-style health endpoints
            .route("/healthz", get(healthz_handler))
            .route("/livez", get(livez_handler))
            .route("/readyz", get(readyz_handler))
            // Stats endpoint
            .route("/stats", get(stats_handler))
            // Legacy endpoints
            .route("/health", get(|| async { "OK" }))
            .route("/ready", get(|| async { "OK" }))
            .with_state(admin_state);

        let listener = tokio::net::TcpListener::bind(self.address).await?;

        info!(address = %self.address, "metrics server started");

        axum::serve(listener, app).await?;

        Ok(())
    }
}

// ============================================================================
// Admin API Response Types
// ============================================================================

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: String,
    version: String,
}

#[derive(Debug, Serialize)]
struct StatsResponse {
    uptime_seconds: u64,
    connections: ConnectionStats,
    messages: MessageStats,
}

#[derive(Debug, Serialize)]
struct ConnectionStats {
    active: u64,
    total: u64,
}

#[derive(Debug, Serialize)]
struct MessageStats {
    submitted: u64,
    delivered: u64,
    failed: u64,
}

// ============================================================================
// Admin API Handlers
// ============================================================================

async fn healthz_handler(State(state): State<Arc<AdminState>>) -> impl IntoResponse {
    let response = HealthResponse {
        status: if state.is_healthy() { "healthy".to_string() } else { "unhealthy".to_string() },
        version: env!("CARGO_PKG_VERSION").to_string(),
    };

    if state.is_healthy() {
        (StatusCode::OK, Json(response))
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(response))
    }
}

async fn livez_handler() -> impl IntoResponse {
    StatusCode::OK
}

/// Readiness response with dependency details
#[derive(Debug, Serialize)]
struct ReadinessResponse {
    ready: bool,
    dependencies: Vec<DependencyStatus>,
}

async fn readyz_handler(State(state): State<Arc<AdminState>>) -> impl IntoResponse {
    let dependencies = state.get_dependency_status();
    let ready = state.is_ready();

    let response = ReadinessResponse {
        ready,
        dependencies,
    };

    if ready {
        (StatusCode::OK, Json(response))
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(response))
    }
}

async fn stats_handler(State(state): State<Arc<AdminState>>) -> impl IntoResponse {
    let response = StatsResponse {
        uptime_seconds: state.uptime_secs(),
        connections: ConnectionStats {
            active: state.active_connections.load(Ordering::Relaxed),
            total: state.total_connections.load(Ordering::Relaxed),
        },
        messages: MessageStats {
            submitted: state.messages_submitted.load(Ordering::Relaxed),
            delivered: state.messages_delivered.load(Ordering::Relaxed),
            failed: state.messages_failed.load(Ordering::Relaxed),
        },
    };
    Json(response)
}

impl Drop for Metrics {
    fn drop(&mut self) {
        if let Err(e) = self.meter_provider.shutdown() {
            tracing::warn!(error = %e, "failed to shutdown meter provider");
        }
    }
}

/// Comprehensive SMPP metrics with smpp_* prefix
/// Organized by category for full observability
pub mod counters {
    use opentelemetry::metrics::{Counter, Gauge, Histogram};
    use opentelemetry::KeyValue;
    use std::sync::OnceLock;

    // ============================================================================
    // LISTENER METRICS
    // ============================================================================

    static LISTENER_CONNECTIONS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static LISTENER_CONNECTIONS_ACTIVE: OnceLock<Gauge<i64>> = OnceLock::new();
    static LISTENER_BINDS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static LISTENER_BIND_DURATION: OnceLock<Histogram<f64>> = OnceLock::new();
    static LISTENER_SESSION_DURATION: OnceLock<Histogram<f64>> = OnceLock::new();
    static LISTENER_UNBINDS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();

    // ============================================================================
    // UPSTREAM/CLUSTER METRICS
    // ============================================================================

    static UPSTREAM_CONNECTIONS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static UPSTREAM_CONNECTIONS_ACTIVE: OnceLock<Gauge<i64>> = OnceLock::new();
    static UPSTREAM_POOL_SIZE: OnceLock<Gauge<i64>> = OnceLock::new();
    static UPSTREAM_POOL_AVAILABLE: OnceLock<Gauge<i64>> = OnceLock::new();
    static UPSTREAM_POOL_WAITING: OnceLock<Gauge<i64>> = OnceLock::new();
    static UPSTREAM_POOL_ACQUIRE_DURATION: OnceLock<Histogram<f64>> = OnceLock::new();
    static UPSTREAM_HEALTH_STATUS: OnceLock<Gauge<i64>> = OnceLock::new();
    static UPSTREAM_HEALTH_CHECKS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static UPSTREAM_REQUEST_DURATION: OnceLock<Histogram<f64>> = OnceLock::new();
    static UPSTREAM_ERRORS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static UPSTREAM_RECONNECTS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static UPSTREAM_CIRCUIT_BREAKER_STATE: OnceLock<Gauge<i64>> = OnceLock::new();

    // ============================================================================
    // ROUTE METRICS
    // ============================================================================

    static ROUTE_MATCHES_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static ROUTE_NO_MATCH_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static ROUTE_EVALUATION_DURATION: OnceLock<Histogram<f64>> = OnceLock::new();

    // ============================================================================
    // PDU/PROTOCOL METRICS
    // ============================================================================

    static PDU_RECEIVED_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static PDU_SENT_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static PDU_ERRORS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static PDU_SIZE_BYTES: OnceLock<Histogram<f64>> = OnceLock::new();
    static PDU_PARSE_DURATION: OnceLock<Histogram<f64>> = OnceLock::new();

    // ============================================================================
    // MESSAGE METRICS
    // ============================================================================

    static MESSAGES_SUBMITTED_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static MESSAGES_DELIVERED_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static MESSAGES_FAILED_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static MESSAGES_EXPIRED_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static MESSAGE_PARTS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static MESSAGE_SUBMIT_DURATION: OnceLock<Histogram<f64>> = OnceLock::new();
    static MESSAGE_SIZE_BYTES: OnceLock<Histogram<f64>> = OnceLock::new();
    static MESSAGE_QUEUE_DEPTH: OnceLock<Gauge<i64>> = OnceLock::new();
    static MESSAGE_QUEUE_LATENCY: OnceLock<Histogram<f64>> = OnceLock::new();

    // ============================================================================
    // DLR (DELIVERY RECEIPT) METRICS
    // ============================================================================

    static DLR_RECEIVED_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static DLR_LATENCY: OnceLock<Histogram<f64>> = OnceLock::new();
    static DLR_PENDING: OnceLock<Gauge<i64>> = OnceLock::new();

    // ============================================================================
    // THROUGHPUT METRICS
    // ============================================================================

    static THROUGHPUT_TPS: OnceLock<Gauge<f64>> = OnceLock::new();
    static THROUGHPUT_BYTES_IN: OnceLock<Counter<u64>> = OnceLock::new();
    static THROUGHPUT_BYTES_OUT: OnceLock<Counter<u64>> = OnceLock::new();

    // ============================================================================
    // WINDOW/FLOW CONTROL METRICS
    // ============================================================================

    static WINDOW_SIZE: OnceLock<Gauge<i64>> = OnceLock::new();
    static WINDOW_PENDING: OnceLock<Gauge<i64>> = OnceLock::new();
    static WINDOW_FULL_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static THROTTLE_EVENTS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static BACKPRESSURE_EVENTS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();

    // ============================================================================
    // SESSION STATE METRICS
    // ============================================================================

    static SESSION_STATE: OnceLock<Gauge<i64>> = OnceLock::new();
    static SESSION_TRANSITIONS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static ENQUIRE_LINK_SENT_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static ENQUIRE_LINK_RECEIVED_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static ENQUIRE_LINK_TIMEOUT_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static ENQUIRE_LINK_LATENCY: OnceLock<Histogram<f64>> = OnceLock::new();

    // ============================================================================
    // ERROR BREAKDOWN METRICS
    // ============================================================================

    static ERRORS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static TIMEOUT_ERRORS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static PROTOCOL_ERRORS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static NETWORK_ERRORS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static INVALID_PDU_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();

    // ============================================================================
    // FILTER/MIDDLEWARE METRICS
    // ============================================================================

    static FILTER_EXECUTIONS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static FILTER_REJECTIONS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static FILTER_DURATION: OnceLock<Histogram<f64>> = OnceLock::new();
    static FILTER_ERRORS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();

    // ============================================================================
    // RATE LIMITING METRICS
    // ============================================================================

    static RATE_LIMIT_ALLOWED_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static RATE_LIMIT_DENIED_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static RATE_LIMIT_CURRENT: OnceLock<Gauge<f64>> = OnceLock::new();
    static RATE_LIMIT_CAPACITY: OnceLock<Gauge<f64>> = OnceLock::new();

    // ============================================================================
    // AUTH METRICS
    // ============================================================================

    static AUTH_ATTEMPTS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static AUTH_SUCCESS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static AUTH_FAILURE_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static AUTH_DURATION: OnceLock<Histogram<f64>> = OnceLock::new();

    // ============================================================================
    // TLS METRICS
    // ============================================================================

    static TLS_HANDSHAKES_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static TLS_HANDSHAKE_DURATION: OnceLock<Histogram<f64>> = OnceLock::new();
    static TLS_ERRORS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static TLS_CONNECTIONS_ACTIVE: OnceLock<Gauge<i64>> = OnceLock::new();

    // ============================================================================
    // ENCODING METRICS
    // ============================================================================

    static ENCODING_CONVERSIONS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static ENCODING_ERRORS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();

    // ============================================================================
    // CREDIT/BILLING METRICS
    // ============================================================================

    static CREDITS_CHECKED_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static CREDITS_DEDUCTED_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static CREDITS_INSUFFICIENT_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static CREDITS_BALANCE: OnceLock<Gauge<f64>> = OnceLock::new();

    // ============================================================================
    // SENDER ID METRICS
    // ============================================================================

    static SENDER_ID_LOOKUPS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static SENDER_ID_REWRITES_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static SENDER_ID_BLOCKED_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();

    // ============================================================================
    // SYSTEM/SERVER METRICS
    // ============================================================================

    static SERVER_INFO: OnceLock<Gauge<i64>> = OnceLock::new();
    static SERVER_START_TIME: OnceLock<Gauge<i64>> = OnceLock::new();
    static CONFIG_RELOADS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static CONFIG_RELOAD_ERRORS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static WORKERS_ACTIVE: OnceLock<Gauge<i64>> = OnceLock::new();
    static SHUTDOWN_DRAIN_CONNECTIONS: OnceLock<Gauge<i64>> = OnceLock::new();

    // ============================================================================
    // RESOURCE METRICS
    // ============================================================================

    static BUFFER_POOL_SIZE: OnceLock<Gauge<i64>> = OnceLock::new();
    static BUFFER_POOL_AVAILABLE: OnceLock<Gauge<i64>> = OnceLock::new();
    static BUFFER_ALLOCATIONS_TOTAL: OnceLock<Counter<u64>> = OnceLock::new();
    static PENDING_RESPONSES: OnceLock<Gauge<i64>> = OnceLock::new();
    static ACTIVE_TASKS: OnceLock<Gauge<i64>> = OnceLock::new();

    /// Initialize all metrics
    pub fn init(meter: &opentelemetry::metrics::Meter) {
        // ========================================================================
        // Listener metrics
        // ========================================================================
        let _ = LISTENER_CONNECTIONS_TOTAL.set(
            meter.u64_counter("smpp_listener_connections_total")
                .with_description("Total connections by listener and status")
                .build(),
        );
        let _ = LISTENER_CONNECTIONS_ACTIVE.set(
            meter.i64_gauge("smpp_listener_connections_active")
                .with_description("Currently active connections per listener")
                .build(),
        );
        let _ = LISTENER_BINDS_TOTAL.set(
            meter.u64_counter("smpp_listener_binds_total")
                .with_description("Bind operations by type and result")
                .build(),
        );
        let _ = LISTENER_BIND_DURATION.set(
            meter.f64_histogram("smpp_listener_bind_duration_seconds")
                .with_description("Bind handshake duration")
                .build(),
        );
        let _ = LISTENER_SESSION_DURATION.set(
            meter.f64_histogram("smpp_listener_session_duration_seconds")
                .with_description("Session lifetime")
                .build(),
        );
        let _ = LISTENER_UNBINDS_TOTAL.set(
            meter.u64_counter("smpp_listener_unbinds_total")
                .with_description("Unbind operations by reason")
                .build(),
        );

        // ========================================================================
        // Upstream metrics
        // ========================================================================
        let _ = UPSTREAM_CONNECTIONS_TOTAL.set(
            meter.u64_counter("smpp_upstream_connections_total")
                .with_description("Total upstream connections")
                .build(),
        );
        let _ = UPSTREAM_CONNECTIONS_ACTIVE.set(
            meter.i64_gauge("smpp_upstream_connections_active")
                .with_description("Active upstream connections")
                .build(),
        );
        let _ = UPSTREAM_POOL_SIZE.set(
            meter.i64_gauge("smpp_upstream_pool_size")
                .with_description("Connection pool size")
                .build(),
        );
        let _ = UPSTREAM_POOL_AVAILABLE.set(
            meter.i64_gauge("smpp_upstream_pool_available")
                .with_description("Available pool connections")
                .build(),
        );
        let _ = UPSTREAM_POOL_WAITING.set(
            meter.i64_gauge("smpp_upstream_pool_waiting")
                .with_description("Requests waiting for pool connection")
                .build(),
        );
        let _ = UPSTREAM_POOL_ACQUIRE_DURATION.set(
            meter.f64_histogram("smpp_upstream_pool_acquire_duration_seconds")
                .with_description("Time to acquire connection from pool")
                .build(),
        );
        let _ = UPSTREAM_HEALTH_STATUS.set(
            meter.i64_gauge("smpp_upstream_health_status")
                .with_description("Health status (1=healthy, 0=unhealthy)")
                .build(),
        );
        let _ = UPSTREAM_HEALTH_CHECKS_TOTAL.set(
            meter.u64_counter("smpp_upstream_health_checks_total")
                .with_description("Health check results")
                .build(),
        );
        let _ = UPSTREAM_REQUEST_DURATION.set(
            meter.f64_histogram("smpp_upstream_request_duration_seconds")
                .with_description("Request latency to upstream")
                .build(),
        );
        let _ = UPSTREAM_ERRORS_TOTAL.set(
            meter.u64_counter("smpp_upstream_errors_total")
                .with_description("Upstream errors by type")
                .build(),
        );
        let _ = UPSTREAM_RECONNECTS_TOTAL.set(
            meter.u64_counter("smpp_upstream_reconnects_total")
                .with_description("Reconnection attempts")
                .build(),
        );
        let _ = UPSTREAM_CIRCUIT_BREAKER_STATE.set(
            meter.i64_gauge("smpp_upstream_circuit_breaker_state")
                .with_description("Circuit breaker state (0=closed, 1=open, 2=half-open)")
                .build(),
        );

        // ========================================================================
        // Route metrics
        // ========================================================================
        let _ = ROUTE_MATCHES_TOTAL.set(
            meter.u64_counter("smpp_route_matches_total")
                .with_description("Messages matched per route")
                .build(),
        );
        let _ = ROUTE_NO_MATCH_TOTAL.set(
            meter.u64_counter("smpp_route_no_match_total")
                .with_description("Messages with no matching route")
                .build(),
        );
        let _ = ROUTE_EVALUATION_DURATION.set(
            meter.f64_histogram("smpp_route_evaluation_duration_seconds")
                .with_description("Route evaluation time")
                .build(),
        );

        // ========================================================================
        // PDU metrics
        // ========================================================================
        let _ = PDU_RECEIVED_TOTAL.set(
            meter.u64_counter("smpp_pdu_received_total")
                .with_description("PDUs received by command")
                .build(),
        );
        let _ = PDU_SENT_TOTAL.set(
            meter.u64_counter("smpp_pdu_sent_total")
                .with_description("PDUs sent by command")
                .build(),
        );
        let _ = PDU_ERRORS_TOTAL.set(
            meter.u64_counter("smpp_pdu_errors_total")
                .with_description("PDU errors by status")
                .build(),
        );
        let _ = PDU_SIZE_BYTES.set(
            meter.f64_histogram("smpp_pdu_size_bytes")
                .with_description("PDU size in bytes")
                .build(),
        );
        let _ = PDU_PARSE_DURATION.set(
            meter.f64_histogram("smpp_pdu_parse_duration_seconds")
                .with_description("PDU parsing time")
                .build(),
        );

        // ========================================================================
        // Message metrics
        // ========================================================================
        let _ = MESSAGES_SUBMITTED_TOTAL.set(
            meter.u64_counter("smpp_messages_submitted_total")
                .with_description("Messages submitted")
                .build(),
        );
        let _ = MESSAGES_DELIVERED_TOTAL.set(
            meter.u64_counter("smpp_messages_delivered_total")
                .with_description("Messages delivered")
                .build(),
        );
        let _ = MESSAGES_FAILED_TOTAL.set(
            meter.u64_counter("smpp_messages_failed_total")
                .with_description("Messages failed by reason")
                .build(),
        );
        let _ = MESSAGES_EXPIRED_TOTAL.set(
            meter.u64_counter("smpp_messages_expired_total")
                .with_description("Messages expired")
                .build(),
        );
        let _ = MESSAGE_PARTS_TOTAL.set(
            meter.u64_counter("smpp_message_parts_total")
                .with_description("Message parts (concatenated)")
                .build(),
        );
        let _ = MESSAGE_SUBMIT_DURATION.set(
            meter.f64_histogram("smpp_message_submit_duration_seconds")
                .with_description("Message submit latency")
                .build(),
        );
        let _ = MESSAGE_SIZE_BYTES.set(
            meter.f64_histogram("smpp_message_size_bytes")
                .with_description("Message payload size")
                .build(),
        );
        let _ = MESSAGE_QUEUE_DEPTH.set(
            meter.i64_gauge("smpp_message_queue_depth")
                .with_description("Messages in queue")
                .build(),
        );
        let _ = MESSAGE_QUEUE_LATENCY.set(
            meter.f64_histogram("smpp_message_queue_latency_seconds")
                .with_description("Time spent in queue")
                .build(),
        );

        // ========================================================================
        // DLR metrics
        // ========================================================================
        let _ = DLR_RECEIVED_TOTAL.set(
            meter.u64_counter("smpp_dlr_received_total")
                .with_description("Delivery receipts by status")
                .build(),
        );
        let _ = DLR_LATENCY.set(
            meter.f64_histogram("smpp_dlr_latency_seconds")
                .with_description("Time from submit to DLR")
                .build(),
        );
        let _ = DLR_PENDING.set(
            meter.i64_gauge("smpp_dlr_pending")
                .with_description("Messages awaiting DLR")
                .build(),
        );

        // ========================================================================
        // Throughput metrics
        // ========================================================================
        let _ = THROUGHPUT_TPS.set(
            meter.f64_gauge("smpp_throughput_messages_per_second")
                .with_description("Current TPS")
                .build(),
        );
        let _ = THROUGHPUT_BYTES_IN.set(
            meter.u64_counter("smpp_throughput_bytes_received_total")
                .with_description("Total bytes received")
                .build(),
        );
        let _ = THROUGHPUT_BYTES_OUT.set(
            meter.u64_counter("smpp_throughput_bytes_sent_total")
                .with_description("Total bytes sent")
                .build(),
        );

        // ========================================================================
        // Window/Flow control metrics
        // ========================================================================
        let _ = WINDOW_SIZE.set(
            meter.i64_gauge("smpp_window_size")
                .with_description("Configured window size")
                .build(),
        );
        let _ = WINDOW_PENDING.set(
            meter.i64_gauge("smpp_window_pending")
                .with_description("Pending requests in window")
                .build(),
        );
        let _ = WINDOW_FULL_TOTAL.set(
            meter.u64_counter("smpp_window_full_total")
                .with_description("Times window became full")
                .build(),
        );
        let _ = THROTTLE_EVENTS_TOTAL.set(
            meter.u64_counter("smpp_throttle_events_total")
                .with_description("Throttling events")
                .build(),
        );
        let _ = BACKPRESSURE_EVENTS_TOTAL.set(
            meter.u64_counter("smpp_backpressure_events_total")
                .with_description("Backpressure events")
                .build(),
        );

        // ========================================================================
        // Session state metrics
        // ========================================================================
        let _ = SESSION_STATE.set(
            meter.i64_gauge("smpp_session_state")
                .with_description("Session state (0=open,1=binding,2=bound,3=unbinding,4=closed)")
                .build(),
        );
        let _ = SESSION_TRANSITIONS_TOTAL.set(
            meter.u64_counter("smpp_session_transitions_total")
                .with_description("Session state transitions")
                .build(),
        );
        let _ = ENQUIRE_LINK_SENT_TOTAL.set(
            meter.u64_counter("smpp_enquire_link_sent_total")
                .with_description("Enquire link requests sent")
                .build(),
        );
        let _ = ENQUIRE_LINK_RECEIVED_TOTAL.set(
            meter.u64_counter("smpp_enquire_link_received_total")
                .with_description("Enquire link requests received")
                .build(),
        );
        let _ = ENQUIRE_LINK_TIMEOUT_TOTAL.set(
            meter.u64_counter("smpp_enquire_link_timeout_total")
                .with_description("Enquire link timeouts")
                .build(),
        );
        let _ = ENQUIRE_LINK_LATENCY.set(
            meter.f64_histogram("smpp_enquire_link_latency_seconds")
                .with_description("Enquire link round-trip time")
                .build(),
        );

        // ========================================================================
        // Error breakdown metrics
        // ========================================================================
        let _ = ERRORS_TOTAL.set(
            meter.u64_counter("smpp_errors_total")
                .with_description("Total errors by category")
                .build(),
        );
        let _ = TIMEOUT_ERRORS_TOTAL.set(
            meter.u64_counter("smpp_timeout_errors_total")
                .with_description("Timeout errors by type")
                .build(),
        );
        let _ = PROTOCOL_ERRORS_TOTAL.set(
            meter.u64_counter("smpp_protocol_errors_total")
                .with_description("Protocol errors")
                .build(),
        );
        let _ = NETWORK_ERRORS_TOTAL.set(
            meter.u64_counter("smpp_network_errors_total")
                .with_description("Network errors")
                .build(),
        );
        let _ = INVALID_PDU_TOTAL.set(
            meter.u64_counter("smpp_invalid_pdu_total")
                .with_description("Invalid PDUs received")
                .build(),
        );

        // ========================================================================
        // Filter/middleware metrics
        // ========================================================================
        let _ = FILTER_EXECUTIONS_TOTAL.set(
            meter.u64_counter("smpp_filter_executions_total")
                .with_description("Filter executions by name")
                .build(),
        );
        let _ = FILTER_REJECTIONS_TOTAL.set(
            meter.u64_counter("smpp_filter_rejections_total")
                .with_description("Filter rejections by name and reason")
                .build(),
        );
        let _ = FILTER_DURATION.set(
            meter.f64_histogram("smpp_filter_duration_seconds")
                .with_description("Filter execution time")
                .build(),
        );
        let _ = FILTER_ERRORS_TOTAL.set(
            meter.u64_counter("smpp_filter_errors_total")
                .with_description("Filter errors")
                .build(),
        );

        // ========================================================================
        // Rate limiting metrics
        // ========================================================================
        let _ = RATE_LIMIT_ALLOWED_TOTAL.set(
            meter.u64_counter("smpp_rate_limit_allowed_total")
                .with_description("Requests allowed by rate limiter")
                .build(),
        );
        let _ = RATE_LIMIT_DENIED_TOTAL.set(
            meter.u64_counter("smpp_rate_limit_denied_total")
                .with_description("Requests denied by rate limiter")
                .build(),
        );
        let _ = RATE_LIMIT_CURRENT.set(
            meter.f64_gauge("smpp_rate_limit_current")
                .with_description("Current rate")
                .build(),
        );
        let _ = RATE_LIMIT_CAPACITY.set(
            meter.f64_gauge("smpp_rate_limit_capacity")
                .with_description("Rate limit capacity")
                .build(),
        );

        // ========================================================================
        // Auth metrics
        // ========================================================================
        let _ = AUTH_ATTEMPTS_TOTAL.set(
            meter.u64_counter("smpp_auth_attempts_total")
                .with_description("Authentication attempts")
                .build(),
        );
        let _ = AUTH_SUCCESS_TOTAL.set(
            meter.u64_counter("smpp_auth_success_total")
                .with_description("Successful authentications")
                .build(),
        );
        let _ = AUTH_FAILURE_TOTAL.set(
            meter.u64_counter("smpp_auth_failure_total")
                .with_description("Failed authentications by reason")
                .build(),
        );
        let _ = AUTH_DURATION.set(
            meter.f64_histogram("smpp_auth_duration_seconds")
                .with_description("Authentication duration")
                .build(),
        );

        // ========================================================================
        // TLS metrics
        // ========================================================================
        let _ = TLS_HANDSHAKES_TOTAL.set(
            meter.u64_counter("smpp_tls_handshakes_total")
                .with_description("TLS handshakes by result")
                .build(),
        );
        let _ = TLS_HANDSHAKE_DURATION.set(
            meter.f64_histogram("smpp_tls_handshake_duration_seconds")
                .with_description("TLS handshake duration")
                .build(),
        );
        let _ = TLS_ERRORS_TOTAL.set(
            meter.u64_counter("smpp_tls_errors_total")
                .with_description("TLS errors by type")
                .build(),
        );
        let _ = TLS_CONNECTIONS_ACTIVE.set(
            meter.i64_gauge("smpp_tls_connections_active")
                .with_description("Active TLS connections")
                .build(),
        );

        // ========================================================================
        // Encoding metrics
        // ========================================================================
        let _ = ENCODING_CONVERSIONS_TOTAL.set(
            meter.u64_counter("smpp_encoding_conversions_total")
                .with_description("Encoding conversions (GSM7, UCS2, etc)")
                .build(),
        );
        let _ = ENCODING_ERRORS_TOTAL.set(
            meter.u64_counter("smpp_encoding_errors_total")
                .with_description("Encoding errors")
                .build(),
        );

        // ========================================================================
        // Credit/billing metrics
        // ========================================================================
        let _ = CREDITS_CHECKED_TOTAL.set(
            meter.u64_counter("smpp_credits_checked_total")
                .with_description("Credit checks")
                .build(),
        );
        let _ = CREDITS_DEDUCTED_TOTAL.set(
            meter.u64_counter("smpp_credits_deducted_total")
                .with_description("Credits deducted")
                .build(),
        );
        let _ = CREDITS_INSUFFICIENT_TOTAL.set(
            meter.u64_counter("smpp_credits_insufficient_total")
                .with_description("Insufficient credit rejections")
                .build(),
        );
        let _ = CREDITS_BALANCE.set(
            meter.f64_gauge("smpp_credits_balance")
                .with_description("Current credit balance")
                .build(),
        );

        // ========================================================================
        // Sender ID metrics
        // ========================================================================
        let _ = SENDER_ID_LOOKUPS_TOTAL.set(
            meter.u64_counter("smpp_sender_id_lookups_total")
                .with_description("Sender ID lookups")
                .build(),
        );
        let _ = SENDER_ID_REWRITES_TOTAL.set(
            meter.u64_counter("smpp_sender_id_rewrites_total")
                .with_description("Sender ID rewrites")
                .build(),
        );
        let _ = SENDER_ID_BLOCKED_TOTAL.set(
            meter.u64_counter("smpp_sender_id_blocked_total")
                .with_description("Blocked sender IDs")
                .build(),
        );

        // ========================================================================
        // System metrics
        // ========================================================================
        let _ = SERVER_INFO.set(
            meter.i64_gauge("smpp_server_info")
                .with_description("Server information")
                .build(),
        );
        let _ = SERVER_START_TIME.set(
            meter.i64_gauge("smpp_server_start_time_seconds")
                .with_description("Server start time (unix timestamp)")
                .build(),
        );
        let _ = CONFIG_RELOADS_TOTAL.set(
            meter.u64_counter("smpp_config_reloads_total")
                .with_description("Configuration reloads")
                .build(),
        );
        let _ = CONFIG_RELOAD_ERRORS_TOTAL.set(
            meter.u64_counter("smpp_config_reload_errors_total")
                .with_description("Configuration reload failures")
                .build(),
        );
        let _ = WORKERS_ACTIVE.set(
            meter.i64_gauge("smpp_workers_active")
                .with_description("Active worker threads")
                .build(),
        );
        let _ = SHUTDOWN_DRAIN_CONNECTIONS.set(
            meter.i64_gauge("smpp_shutdown_drain_connections")
                .with_description("Connections draining during shutdown")
                .build(),
        );

        // ========================================================================
        // Resource metrics
        // ========================================================================
        let _ = BUFFER_POOL_SIZE.set(
            meter.i64_gauge("smpp_buffer_pool_size")
                .with_description("Buffer pool size")
                .build(),
        );
        let _ = BUFFER_POOL_AVAILABLE.set(
            meter.i64_gauge("smpp_buffer_pool_available")
                .with_description("Available buffers")
                .build(),
        );
        let _ = BUFFER_ALLOCATIONS_TOTAL.set(
            meter.u64_counter("smpp_buffer_allocations_total")
                .with_description("Buffer allocations")
                .build(),
        );
        let _ = PENDING_RESPONSES.set(
            meter.i64_gauge("smpp_pending_responses")
                .with_description("Pending response callbacks")
                .build(),
        );
        let _ = ACTIVE_TASKS.set(
            meter.i64_gauge("smpp_active_tasks")
                .with_description("Active async tasks")
                .build(),
        );

        // Set server info
        if let Some(g) = SERVER_INFO.get() {
            g.record(1, &[KeyValue::new("version", env!("CARGO_PKG_VERSION"))]);
        }
        if let Some(g) = SERVER_START_TIME.get() {
            g.record(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64,
                &[],
            );
        }
    }

    // ============================================================================
    // LISTENER RECORDING FUNCTIONS
    // ============================================================================

    pub fn listener_connection_opened(listener: &str) {
        if let Some(c) = LISTENER_CONNECTIONS_TOTAL.get() {
            c.add(1, &[kv("listener", listener), kv("status", "accepted")]);
        }
    }

    pub fn listener_connection_rejected(listener: &str, reason: &str) {
        if let Some(c) = LISTENER_CONNECTIONS_TOTAL.get() {
            c.add(1, &[kv("listener", listener), kv("status", "rejected"), kv("reason", reason)]);
        }
    }

    pub fn listener_connections_active_set(listener: &str, count: i64) {
        if let Some(g) = LISTENER_CONNECTIONS_ACTIVE.get() {
            g.record(count, &[kv("listener", listener)]);
        }
    }

    pub fn listener_bind(listener: &str, bind_type: &str, result: &str, duration_secs: f64) {
        if let Some(c) = LISTENER_BINDS_TOTAL.get() {
            c.add(1, &[kv("listener", listener), kv("bind_type", bind_type), kv("result", result)]);
        }
        if let Some(h) = LISTENER_BIND_DURATION.get() {
            h.record(duration_secs, &[kv("listener", listener), kv("bind_type", bind_type)]);
        }
    }

    pub fn listener_session_closed(listener: &str, duration_secs: f64) {
        if let Some(h) = LISTENER_SESSION_DURATION.get() {
            h.record(duration_secs, &[kv("listener", listener)]);
        }
    }

    pub fn listener_unbind(listener: &str, reason: &str) {
        if let Some(c) = LISTENER_UNBINDS_TOTAL.get() {
            c.add(1, &[kv("listener", listener), kv("reason", reason)]);
        }
    }

    // ============================================================================
    // UPSTREAM RECORDING FUNCTIONS
    // ============================================================================

    pub fn upstream_connection_opened(cluster: &str, endpoint: &str) {
        if let Some(c) = UPSTREAM_CONNECTIONS_TOTAL.get() {
            c.add(1, &[kv("cluster", cluster), kv("endpoint", endpoint), kv("status", "connected")]);
        }
    }

    pub fn upstream_connection_failed(cluster: &str, endpoint: &str, reason: &str) {
        if let Some(c) = UPSTREAM_CONNECTIONS_TOTAL.get() {
            c.add(1, &[kv("cluster", cluster), kv("endpoint", endpoint), kv("status", "failed"), kv("reason", reason)]);
        }
    }

    pub fn upstream_connections_active_set(cluster: &str, count: i64) {
        if let Some(g) = UPSTREAM_CONNECTIONS_ACTIVE.get() {
            g.record(count, &[kv("cluster", cluster)]);
        }
    }

    pub fn upstream_pool_update(cluster: &str, size: i64, available: i64, waiting: i64) {
        if let Some(g) = UPSTREAM_POOL_SIZE.get() { g.record(size, &[kv("cluster", cluster)]); }
        if let Some(g) = UPSTREAM_POOL_AVAILABLE.get() { g.record(available, &[kv("cluster", cluster)]); }
        if let Some(g) = UPSTREAM_POOL_WAITING.get() { g.record(waiting, &[kv("cluster", cluster)]); }
    }

    pub fn upstream_pool_acquire(cluster: &str, duration_secs: f64) {
        if let Some(h) = UPSTREAM_POOL_ACQUIRE_DURATION.get() {
            h.record(duration_secs, &[kv("cluster", cluster)]);
        }
    }

    pub fn upstream_health_set(cluster: &str, endpoint: &str, healthy: bool) {
        if let Some(g) = UPSTREAM_HEALTH_STATUS.get() {
            g.record(if healthy { 1 } else { 0 }, &[kv("cluster", cluster), kv("endpoint", endpoint)]);
        }
    }

    /// Record upstream health check (accepts bool for success)
    pub fn upstream_health_check(cluster: &str, endpoint: &str, success: bool) {
        if let Some(c) = UPSTREAM_HEALTH_CHECKS_TOTAL.get() {
            c.add(1, &[kv("cluster", cluster), kv("endpoint", endpoint), kv("result", if success { "success" } else { "failure" })]);
        }
    }

    pub fn upstream_request_duration(cluster: &str, command: &str, duration_secs: f64) {
        if let Some(h) = UPSTREAM_REQUEST_DURATION.get() {
            h.record(duration_secs, &[kv("cluster", cluster), kv("command", command)]);
        }
    }

    pub fn upstream_error(cluster: &str, error_type: &str) {
        if let Some(c) = UPSTREAM_ERRORS_TOTAL.get() {
            c.add(1, &[kv("cluster", cluster), kv("type", error_type)]);
        }
    }

    pub fn upstream_reconnect(cluster: &str, endpoint: &str, attempt: u32) {
        if let Some(c) = UPSTREAM_RECONNECTS_TOTAL.get() {
            c.add(1, &[kv("cluster", cluster), kv("endpoint", endpoint), kv("attempt", &attempt.to_string())]);
        }
    }

    pub fn upstream_circuit_breaker(cluster: &str, state: &str) {
        let state_val = match state {
            "closed" => 0,
            "open" => 1,
            "half-open" => 2,
            _ => -1,
        };
        if let Some(g) = UPSTREAM_CIRCUIT_BREAKER_STATE.get() {
            g.record(state_val, &[kv("cluster", cluster)]);
        }
    }

    // ============================================================================
    // ROUTE RECORDING FUNCTIONS
    // ============================================================================

    pub fn route_matched(route: &str, cluster: &str) {
        if let Some(c) = ROUTE_MATCHES_TOTAL.get() {
            c.add(1, &[kv("route", route), kv("cluster", cluster)]);
        }
    }

    pub fn route_no_match(destination: &str) {
        if let Some(c) = ROUTE_NO_MATCH_TOTAL.get() {
            c.add(1, &[kv("destination_prefix", destination)]);
        }
    }

    pub fn route_evaluation(duration_secs: f64, route_count: usize) {
        if let Some(h) = ROUTE_EVALUATION_DURATION.get() {
            h.record(duration_secs, &[kv("routes_evaluated", &route_count.to_string())]);
        }
    }

    // ============================================================================
    // PDU RECORDING FUNCTIONS
    // ============================================================================

    /// Record PDU received (simplified - just listener and command)
    pub fn pdu_received(listener: &str, command: &str) {
        if let Some(c) = PDU_RECEIVED_TOTAL.get() {
            c.add(1, &[kv("listener", listener), kv("command", command)]);
        }
    }

    /// Record PDU received with size
    pub fn pdu_received_with_size(listener: &str, command: &str, size_bytes: u64) {
        pdu_received(listener, command);
        if let Some(h) = PDU_SIZE_BYTES.get() {
            h.record(size_bytes as f64, &[kv("direction", "in"), kv("command", command)]);
        }
    }

    /// Record PDU sent (simplified - just listener and command)
    pub fn pdu_sent(listener: &str, command: &str) {
        if let Some(c) = PDU_SENT_TOTAL.get() {
            c.add(1, &[kv("listener", listener), kv("command", command)]);
        }
    }

    /// Record PDU sent with size
    pub fn pdu_sent_with_size(cluster: &str, command: &str, size_bytes: u64) {
        if let Some(c) = PDU_SENT_TOTAL.get() {
            c.add(1, &[kv("cluster", cluster), kv("command", command)]);
        }
        if let Some(h) = PDU_SIZE_BYTES.get() {
            h.record(size_bytes as f64, &[kv("direction", "out"), kv("command", command)]);
        }
    }

    /// Record PDU error (simplified - just listener and type)
    pub fn pdu_error(listener: &str, error_type: &str) {
        if let Some(c) = PDU_ERRORS_TOTAL.get() {
            c.add(1, &[kv("listener", listener), kv("type", error_type)]);
        }
    }

    /// Record PDU error with details
    pub fn pdu_error_with_status(command: &str, status_code: u32, status_name: &str) {
        if let Some(c) = PDU_ERRORS_TOTAL.get() {
            c.add(1, &[kv("command", command), kv("status_code", &status_code.to_string()), kv("status", status_name)]);
        }
    }

    pub fn pdu_parse(duration_secs: f64, command: &str) {
        if let Some(h) = PDU_PARSE_DURATION.get() {
            h.record(duration_secs, &[kv("command", command)]);
        }
    }

    // ============================================================================
    // MESSAGE RECORDING FUNCTIONS
    // ============================================================================

    /// Record message submitted (simplified - just listener)
    pub fn message_submitted(listener: &str) {
        if let Some(c) = MESSAGES_SUBMITTED_TOTAL.get() {
            c.add(1, &[kv("listener", listener)]);
        }
    }

    /// Record message submitted with details
    pub fn message_submitted_with_details(cluster: &str, source: &str, dest_prefix: &str, size_bytes: u64) {
        if let Some(c) = MESSAGES_SUBMITTED_TOTAL.get() {
            c.add(1, &[kv("cluster", cluster), kv("source", source), kv("destination_prefix", dest_prefix)]);
        }
        if let Some(h) = MESSAGE_SIZE_BYTES.get() {
            h.record(size_bytes as f64, &[kv("cluster", cluster)]);
        }
    }

    pub fn message_delivered(listener: &str) {
        if let Some(c) = MESSAGES_DELIVERED_TOTAL.get() {
            c.add(1, &[kv("listener", listener)]);
        }
    }

    pub fn message_failed(cluster: &str, reason: &str) {
        if let Some(c) = MESSAGES_FAILED_TOTAL.get() {
            c.add(1, &[kv("cluster", cluster), kv("reason", reason)]);
        }
    }

    pub fn message_expired(cluster: &str) {
        if let Some(c) = MESSAGES_EXPIRED_TOTAL.get() {
            c.add(1, &[kv("cluster", cluster)]);
        }
    }

    pub fn message_parts(cluster: &str, parts: u64) {
        if let Some(c) = MESSAGE_PARTS_TOTAL.get() {
            c.add(parts, &[kv("cluster", cluster)]);
        }
    }

    pub fn message_submit_duration(cluster: &str, duration_secs: f64) {
        if let Some(h) = MESSAGE_SUBMIT_DURATION.get() {
            h.record(duration_secs, &[kv("cluster", cluster)]);
        }
    }

    pub fn message_queue_update(queue: &str, depth: i64) {
        if let Some(g) = MESSAGE_QUEUE_DEPTH.get() {
            g.record(depth, &[kv("queue", queue)]);
        }
    }

    pub fn message_queue_latency(queue: &str, latency_secs: f64) {
        if let Some(h) = MESSAGE_QUEUE_LATENCY.get() {
            h.record(latency_secs, &[kv("queue", queue)]);
        }
    }

    // ============================================================================
    // DLR RECORDING FUNCTIONS
    // ============================================================================

    pub fn dlr_received(status: &str, cluster: &str) {
        if let Some(c) = DLR_RECEIVED_TOTAL.get() {
            c.add(1, &[kv("status", status), kv("cluster", cluster)]);
        }
    }

    pub fn dlr_latency(cluster: &str, latency_secs: f64) {
        if let Some(h) = DLR_LATENCY.get() {
            h.record(latency_secs, &[kv("cluster", cluster)]);
        }
    }

    pub fn dlr_pending_set(count: i64) {
        if let Some(g) = DLR_PENDING.get() {
            g.record(count, &[]);
        }
    }

    /// Record orphaned DLR (no matching message in store)
    pub fn dlr_orphaned(cluster: &str) {
        if let Some(c) = DLR_RECEIVED_TOTAL.get() {
            c.add(1, &[kv("cluster", cluster), kv("status", "orphaned")]);
        }
    }

    /// Record DLR forwarded to client
    pub fn dlr_forwarded(system_id: &str) {
        if let Some(c) = DLR_RECEIVED_TOTAL.get() {
            c.add(1, &[kv("system_id", system_id), kv("status", "forwarded")]);
        }
    }

    /// Record message delivered via DLR (simple - no listener needed)
    pub fn message_delivered_dlr() {
        if let Some(c) = MESSAGES_DELIVERED_TOTAL.get() {
            c.add(1, &[kv("source", "dlr")]);
        }
    }

    /// Record message failed via DLR
    pub fn message_failed_dlr() {
        if let Some(c) = MESSAGES_FAILED_TOTAL.get() {
            c.add(1, &[kv("reason", "dlr_failed")]);
        }
    }

    // ============================================================================
    // THROUGHPUT RECORDING FUNCTIONS
    // ============================================================================

    pub fn throughput_set(tps: f64) {
        if let Some(g) = THROUGHPUT_TPS.get() {
            g.record(tps, &[]);
        }
    }

    pub fn bytes_received(listener: &str, bytes: u64) {
        if let Some(c) = THROUGHPUT_BYTES_IN.get() {
            c.add(bytes, &[kv("listener", listener)]);
        }
    }

    pub fn bytes_sent(cluster: &str, bytes: u64) {
        if let Some(c) = THROUGHPUT_BYTES_OUT.get() {
            c.add(bytes, &[kv("cluster", cluster)]);
        }
    }

    // ============================================================================
    // WINDOW/FLOW CONTROL RECORDING FUNCTIONS
    // ============================================================================

    pub fn window_update(session: &str, size: i64, pending: i64) {
        if let Some(g) = WINDOW_SIZE.get() { g.record(size, &[kv("session", session)]); }
        if let Some(g) = WINDOW_PENDING.get() { g.record(pending, &[kv("session", session)]); }
    }

    pub fn window_full(session: &str) {
        if let Some(c) = WINDOW_FULL_TOTAL.get() {
            c.add(1, &[kv("session", session)]);
        }
    }

    pub fn throttle_event(source: &str, reason: &str) {
        if let Some(c) = THROTTLE_EVENTS_TOTAL.get() {
            c.add(1, &[kv("source", source), kv("reason", reason)]);
        }
    }

    pub fn backpressure_event(component: &str) {
        if let Some(c) = BACKPRESSURE_EVENTS_TOTAL.get() {
            c.add(1, &[kv("component", component)]);
        }
    }

    // ============================================================================
    // SESSION STATE RECORDING FUNCTIONS
    // ============================================================================

    pub fn session_state_set(session: &str, state: &str) {
        let state_val = match state {
            "open" => 0, "binding" => 1, "bound" => 2, "unbinding" => 3, "closed" => 4,
            _ => -1,
        };
        if let Some(g) = SESSION_STATE.get() {
            g.record(state_val, &[kv("session", session)]);
        }
    }

    pub fn session_transition(session: &str, from: &str, to: &str) {
        if let Some(c) = SESSION_TRANSITIONS_TOTAL.get() {
            c.add(1, &[kv("session", session), kv("from", from), kv("to", to)]);
        }
    }

    pub fn enquire_link_sent(session: &str) {
        if let Some(c) = ENQUIRE_LINK_SENT_TOTAL.get() {
            c.add(1, &[kv("session", session)]);
        }
    }

    pub fn enquire_link_received(session: &str) {
        if let Some(c) = ENQUIRE_LINK_RECEIVED_TOTAL.get() {
            c.add(1, &[kv("session", session)]);
        }
    }

    pub fn enquire_link_timeout(session: &str) {
        if let Some(c) = ENQUIRE_LINK_TIMEOUT_TOTAL.get() {
            c.add(1, &[kv("session", session)]);
        }
    }

    pub fn enquire_link_latency(session: &str, latency_secs: f64) {
        if let Some(h) = ENQUIRE_LINK_LATENCY.get() {
            h.record(latency_secs, &[kv("session", session)]);
        }
    }

    // ============================================================================
    // ERROR RECORDING FUNCTIONS
    // ============================================================================

    pub fn error(category: &str, error_type: &str) {
        if let Some(c) = ERRORS_TOTAL.get() {
            c.add(1, &[kv("category", category), kv("type", error_type)]);
        }
    }

    pub fn timeout_error(operation: &str, timeout_type: &str) {
        if let Some(c) = TIMEOUT_ERRORS_TOTAL.get() {
            c.add(1, &[kv("operation", operation), kv("type", timeout_type)]);
        }
    }

    pub fn protocol_error(command: &str, error: &str) {
        if let Some(c) = PROTOCOL_ERRORS_TOTAL.get() {
            c.add(1, &[kv("command", command), kv("error", error)]);
        }
    }

    pub fn network_error(endpoint: &str, error: &str) {
        if let Some(c) = NETWORK_ERRORS_TOTAL.get() {
            c.add(1, &[kv("endpoint", endpoint), kv("error", error)]);
        }
    }

    pub fn invalid_pdu(listener: &str, reason: &str) {
        if let Some(c) = INVALID_PDU_TOTAL.get() {
            c.add(1, &[kv("listener", listener), kv("reason", reason)]);
        }
    }

    // ============================================================================
    // FILTER RECORDING FUNCTIONS
    // ============================================================================

    pub fn filter_executed(filter: &str, result: &str, duration_secs: f64) {
        if let Some(c) = FILTER_EXECUTIONS_TOTAL.get() {
            c.add(1, &[kv("filter", filter), kv("result", result)]);
        }
        if let Some(h) = FILTER_DURATION.get() {
            h.record(duration_secs, &[kv("filter", filter)]);
        }
    }

    pub fn filter_rejected(filter: &str, reason: &str) {
        if let Some(c) = FILTER_REJECTIONS_TOTAL.get() {
            c.add(1, &[kv("filter", filter), kv("reason", reason)]);
        }
    }

    pub fn filter_error(filter: &str, error: &str) {
        if let Some(c) = FILTER_ERRORS_TOTAL.get() {
            c.add(1, &[kv("filter", filter), kv("error", error)]);
        }
    }

    // ============================================================================
    // RATE LIMIT RECORDING FUNCTIONS
    // ============================================================================

    pub fn rate_limit_allowed(key: &str) {
        if let Some(c) = RATE_LIMIT_ALLOWED_TOTAL.get() {
            c.add(1, &[kv("key", key)]);
        }
    }

    pub fn rate_limit_denied(key: &str, limit: &str) {
        if let Some(c) = RATE_LIMIT_DENIED_TOTAL.get() {
            c.add(1, &[kv("key", key), kv("limit", limit)]);
        }
    }

    pub fn rate_limit_update(key: &str, current: f64, capacity: f64) {
        if let Some(g) = RATE_LIMIT_CURRENT.get() { g.record(current, &[kv("key", key)]); }
        if let Some(g) = RATE_LIMIT_CAPACITY.get() { g.record(capacity, &[kv("key", key)]); }
    }

    // ============================================================================
    // AUTH RECORDING FUNCTIONS
    // ============================================================================

    pub fn auth_attempt(listener: &str, system_id: &str) {
        if let Some(c) = AUTH_ATTEMPTS_TOTAL.get() {
            c.add(1, &[kv("listener", listener), kv("system_id", system_id)]);
        }
    }

    pub fn auth_success(listener: &str, system_id: &str, duration_secs: f64) {
        if let Some(c) = AUTH_SUCCESS_TOTAL.get() {
            c.add(1, &[kv("listener", listener), kv("system_id", system_id)]);
        }
        if let Some(h) = AUTH_DURATION.get() {
            h.record(duration_secs, &[kv("listener", listener), kv("result", "success")]);
        }
    }

    pub fn auth_failure(listener: &str, system_id: &str, reason: &str, duration_secs: f64) {
        if let Some(c) = AUTH_FAILURE_TOTAL.get() {
            c.add(1, &[kv("listener", listener), kv("system_id", system_id), kv("reason", reason)]);
        }
        if let Some(h) = AUTH_DURATION.get() {
            h.record(duration_secs, &[kv("listener", listener), kv("result", "failure")]);
        }
    }

    // ============================================================================
    // TLS RECORDING FUNCTIONS
    // ============================================================================

    pub fn tls_handshake(listener: &str, result: &str, duration_secs: f64) {
        if let Some(c) = TLS_HANDSHAKES_TOTAL.get() {
            c.add(1, &[kv("listener", listener), kv("result", result)]);
        }
        if let Some(h) = TLS_HANDSHAKE_DURATION.get() {
            h.record(duration_secs, &[kv("listener", listener)]);
        }
    }

    pub fn tls_error(listener: &str, error_type: &str) {
        if let Some(c) = TLS_ERRORS_TOTAL.get() {
            c.add(1, &[kv("listener", listener), kv("type", error_type)]);
        }
    }

    pub fn tls_connections_active_set(listener: &str, count: i64) {
        if let Some(g) = TLS_CONNECTIONS_ACTIVE.get() {
            g.record(count, &[kv("listener", listener)]);
        }
    }

    // ============================================================================
    // ENCODING RECORDING FUNCTIONS
    // ============================================================================

    pub fn encoding_conversion(from: &str, to: &str) {
        if let Some(c) = ENCODING_CONVERSIONS_TOTAL.get() {
            c.add(1, &[kv("from", from), kv("to", to)]);
        }
    }

    pub fn encoding_error(encoding: &str, error: &str) {
        if let Some(c) = ENCODING_ERRORS_TOTAL.get() {
            c.add(1, &[kv("encoding", encoding), kv("error", error)]);
        }
    }

    // ============================================================================
    // CREDIT RECORDING FUNCTIONS
    // ============================================================================

    pub fn credit_check(account: &str, result: &str) {
        if let Some(c) = CREDITS_CHECKED_TOTAL.get() {
            c.add(1, &[kv("account", account), kv("result", result)]);
        }
    }

    pub fn credit_deduct(account: &str, amount: u64) {
        if let Some(c) = CREDITS_DEDUCTED_TOTAL.get() {
            c.add(amount, &[kv("account", account)]);
        }
    }

    pub fn credit_insufficient(account: &str) {
        if let Some(c) = CREDITS_INSUFFICIENT_TOTAL.get() {
            c.add(1, &[kv("account", account)]);
        }
    }

    pub fn credit_balance_set(account: &str, balance: f64) {
        if let Some(g) = CREDITS_BALANCE.get() {
            g.record(balance, &[kv("account", account)]);
        }
    }

    // ============================================================================
    // SENDER ID RECORDING FUNCTIONS
    // ============================================================================

    pub fn sender_id_lookup(sender_id: &str, result: &str) {
        if let Some(c) = SENDER_ID_LOOKUPS_TOTAL.get() {
            c.add(1, &[kv("sender_id", sender_id), kv("result", result)]);
        }
    }

    pub fn sender_id_rewrite(original: &str, rewritten: &str) {
        if let Some(c) = SENDER_ID_REWRITES_TOTAL.get() {
            c.add(1, &[kv("original", original), kv("rewritten", rewritten)]);
        }
    }

    pub fn sender_id_blocked(sender_id: &str, reason: &str) {
        if let Some(c) = SENDER_ID_BLOCKED_TOTAL.get() {
            c.add(1, &[kv("sender_id", sender_id), kv("reason", reason)]);
        }
    }

    // ============================================================================
    // SYSTEM RECORDING FUNCTIONS
    // ============================================================================

    pub fn config_reloaded() {
        if let Some(c) = CONFIG_RELOADS_TOTAL.get() {
            c.add(1, &[]);
        }
    }

    pub fn config_reload_error(reason: &str) {
        if let Some(c) = CONFIG_RELOAD_ERRORS_TOTAL.get() {
            c.add(1, &[kv("reason", reason)]);
        }
    }

    pub fn workers_active_set(count: i64) {
        if let Some(g) = WORKERS_ACTIVE.get() {
            g.record(count, &[]);
        }
    }

    pub fn shutdown_drain_set(connections: i64) {
        if let Some(g) = SHUTDOWN_DRAIN_CONNECTIONS.get() {
            g.record(connections, &[]);
        }
    }

    // ============================================================================
    // RESOURCE RECORDING FUNCTIONS
    // ============================================================================

    pub fn buffer_pool_update(size: i64, available: i64) {
        if let Some(g) = BUFFER_POOL_SIZE.get() { g.record(size, &[]); }
        if let Some(g) = BUFFER_POOL_AVAILABLE.get() { g.record(available, &[]); }
    }

    pub fn buffer_allocated() {
        if let Some(c) = BUFFER_ALLOCATIONS_TOTAL.get() {
            c.add(1, &[]);
        }
    }

    pub fn pending_responses_set(count: i64) {
        if let Some(g) = PENDING_RESPONSES.get() {
            g.record(count, &[]);
        }
    }

    pub fn active_tasks_set(count: i64) {
        if let Some(g) = ACTIVE_TASKS.get() {
            g.record(count, &[]);
        }
    }

    // ============================================================================
    // FORWARDER CONVENIENCE FUNCTIONS
    // ============================================================================

    /// Record message forwarded successfully
    pub fn message_forwarded() {
        if let Some(c) = MESSAGES_DELIVERED_TOTAL.get() {
            c.add(1, &[kv("result", "forwarded")]);
        }
    }

    /// Record message forward failed
    pub fn message_forward_failed() {
        if let Some(c) = MESSAGES_FAILED_TOTAL.get() {
            c.add(1, &[kv("reason", "forward_failed")]);
        }
    }

    /// Record message retried
    pub fn message_retried() {
        if let Some(c) = MESSAGES_SUBMITTED_TOTAL.get() {
            c.add(1, &[kv("type", "retry")]);
        }
    }

    /// Record message failed (simple version - no cluster/reason specified)
    pub fn message_failed_simple() {
        if let Some(c) = MESSAGES_FAILED_TOTAL.get() {
            c.add(1, &[kv("reason", "failed")]);
        }
    }

    /// Record message expired (simple version - no cluster specified)
    pub fn message_expired_simple() {
        if let Some(c) = MESSAGES_EXPIRED_TOTAL.get() {
            c.add(1, &[]);
        }
    }

    // ============================================================================
    // CONVENIENCE FUNCTIONS (simplified signatures)
    // ============================================================================

    /// Record listener started
    pub fn listener_started(_listener: &str) {
        // Just log, no specific metric for this
    }

    /// Record listener accept error
    pub fn listener_accept_error(listener: &str) {
        if let Some(c) = LISTENER_CONNECTIONS_TOTAL.get() {
            c.add(1, &[kv("listener", listener), kv("status", "error")]);
        }
    }

    /// Record TLS handshake success
    pub fn tls_handshake_success(listener: &str) {
        tls_handshake(listener, "success", 0.0);
    }

    /// Record TLS handshake error
    pub fn tls_handshake_error(listener: &str) {
        tls_handshake(listener, "error", 0.0);
    }

    /// Record listener connection accepted
    pub fn listener_connection_accepted(listener: &str) {
        listener_connection_opened(listener);
    }

    /// Record listener connection closed
    pub fn listener_connection_closed(listener: &str) {
        if let Some(c) = LISTENER_CONNECTIONS_TOTAL.get() {
            c.add(1, &[kv("listener", listener), kv("status", "closed")]);
        }
    }

    /// Record bind received (simplified)
    pub fn bind_received(listener: &str, bind_type: &str) {
        if let Some(c) = LISTENER_BINDS_TOTAL.get() {
            c.add(1, &[kv("listener", listener), kv("bind_type", bind_type), kv("result", "received")]);
        }
    }

    /// Record bind success (simplified)
    pub fn bind_success(listener: &str, bind_type: &str) {
        if let Some(c) = LISTENER_BINDS_TOTAL.get() {
            c.add(1, &[kv("listener", listener), kv("bind_type", bind_type), kv("result", "success")]);
        }
    }

    /// Record unbind (simplified)
    pub fn unbind(listener: &str) {
        listener_unbind(listener, "client");
    }

    /// Record request timeout
    pub fn request_timeout(listener: &str) {
        if let Some(c) = ERRORS_TOTAL.get() {
            c.add(1, &[kv("listener", listener), kv("type", "timeout")]);
        }
    }

    /// Record upstream health changed
    pub fn upstream_health_changed(cluster: &str, endpoint: &str, healthy: bool) {
        upstream_health_set(cluster, endpoint, healthy);
    }

    /// Record upstream pool timeout
    pub fn upstream_pool_timeout(cluster: &str) {
        if let Some(c) = UPSTREAM_ERRORS_TOTAL.get() {
            c.add(1, &[kv("cluster", cluster), kv("type", "pool_timeout")]);
        }
    }

    /// Record upstream connection created
    pub fn upstream_connection_created(cluster: &str) {
        if let Some(c) = UPSTREAM_CONNECTIONS_TOTAL.get() {
            c.add(1, &[kv("cluster", cluster), kv("status", "created")]);
        }
    }

    /// Record upstream no healthy endpoints
    pub fn upstream_no_healthy_endpoints(cluster: &str) {
        if let Some(c) = UPSTREAM_ERRORS_TOTAL.get() {
            c.add(1, &[kv("cluster", cluster), kv("type", "no_healthy_endpoints")]);
        }
    }

    /// Record upstream connection acquired
    pub fn upstream_connection_acquired(cluster: &str) {
        if let Some(c) = UPSTREAM_CONNECTIONS_TOTAL.get() {
            c.add(1, &[kv("cluster", cluster), kv("status", "acquired")]);
        }
    }

    // Helper function to create KeyValue
    #[inline]
    fn kv(key: &'static str, value: &str) -> KeyValue {
        KeyValue::new(key, value.to_string())
    }
}
