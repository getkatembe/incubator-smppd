//! Admin API integration tests
//!
//! Tests for /healthz, /readyz, /livez, /stats, /metrics endpoints
//!
//! Run with: cargo test --test admin_api -- --test-threads=1

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use std::time::Duration;

use reqwest::StatusCode;
use serde::Deserialize;

/// Port allocator for tests
static PORT: AtomicU16 = AtomicU16::new(19100);

fn next_port() -> u16 {
    PORT.fetch_add(1, Ordering::SeqCst)
}

/// Health response
#[derive(Debug, Deserialize)]
struct HealthResponse {
    status: String,
    version: String,
}

/// Readiness response
#[derive(Debug, Deserialize)]
struct ReadinessResponse {
    ready: bool,
    dependencies: Vec<DependencyStatus>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct DependencyStatus {
    name: String,
    healthy: bool,
    message: Option<String>,
}

/// Stats response
#[derive(Debug, Deserialize)]
struct StatsResponse {
    uptime_seconds: u64,
    connections: ConnectionStats,
    messages: MessageStats,
}

#[derive(Debug, Deserialize)]
struct ConnectionStats {
    active: u64,
    total: u64,
}

#[derive(Debug, Deserialize)]
struct MessageStats {
    submitted: u64,
    delivered: u64,
    failed: u64,
}

/// Test fixture that starts the server on a unique port
struct TestServer {
    handle: tokio::task::JoinHandle<()>,
    admin_state: Arc<smppd::telemetry::AdminState>,
    base_url: String,
}

impl TestServer {
    async fn start() -> Self {
        use smppd::telemetry::{Metrics, MetricsConfig};

        let port = next_port();
        let config = MetricsConfig {
            address: format!("127.0.0.1:{}", port).parse().unwrap(),
        };

        let metrics = Metrics::new(&config).unwrap();
        let admin_state = metrics.admin_state();

        let handle = tokio::spawn(async move {
            let _ = metrics.serve().await;
        });

        // Wait for server to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        Self {
            handle,
            admin_state,
            base_url: format!("http://127.0.0.1:{}", port),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

#[tokio::test]
async fn test_healthz_returns_healthy() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(server.url("/healthz"))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), StatusCode::OK);

    let body: HealthResponse = resp.json().await.expect("invalid json");
    assert_eq!(body.status, "healthy");
    assert!(!body.version.is_empty());
}

#[tokio::test]
async fn test_livez_returns_ok() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(server.url("/livez"))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_readyz_returns_ready_with_no_dependencies() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(server.url("/readyz"))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), StatusCode::OK);

    let body: ReadinessResponse = resp.json().await.expect("invalid json");
    assert!(body.ready);
    assert!(body.dependencies.is_empty());
}

#[tokio::test]
async fn test_stats_returns_valid_stats() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(server.url("/stats"))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), StatusCode::OK);

    let body: StatsResponse = resp.json().await.expect("invalid json");
    assert!(body.uptime_seconds < 60); // Should be very small since just started
    assert_eq!(body.connections.active, 0);
    assert_eq!(body.connections.total, 0);
    assert_eq!(body.messages.submitted, 0);
    assert_eq!(body.messages.delivered, 0);
    assert_eq!(body.messages.failed, 0);
}

#[tokio::test]
async fn test_metrics_returns_prometheus_format() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(server.url("/metrics"))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.text().await.expect("body");
    // Should contain prometheus metrics or be empty at startup
    assert!(body.contains("smpp_") || body.is_empty() || body.contains("# HELP") || body.contains("# TYPE"));
}

#[tokio::test]
async fn test_readyz_with_unhealthy_dependency() {
    let server = TestServer::start().await;

    // Register a cluster and mark it as unhealthy
    server.admin_state.register_cluster("test-cluster");
    server.admin_state.set_cluster_health("test-cluster", false);

    let client = reqwest::Client::new();
    let resp = client
        .get(server.url("/readyz"))
        .send()
        .await
        .expect("request failed");

    // Should return 503 when dependencies are unhealthy
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    let body: ReadinessResponse = resp.json().await.expect("invalid json");
    assert!(!body.ready);
    assert_eq!(body.dependencies.len(), 1);
    assert_eq!(body.dependencies[0].name, "test-cluster");
    assert!(!body.dependencies[0].healthy);
}

#[tokio::test]
async fn test_readyz_becomes_ready_when_all_healthy() {
    let server = TestServer::start().await;

    // Register clusters (initially unhealthy)
    server.admin_state.register_cluster("cluster-a");
    server.admin_state.register_cluster("cluster-b");

    let client = reqwest::Client::new();

    // Initially not ready (clusters not healthy)
    let resp = client
        .get(server.url("/readyz"))
        .send()
        .await
        .expect("request failed");
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    // Mark both healthy
    server.admin_state.set_cluster_health("cluster-a", true);
    server.admin_state.set_cluster_health("cluster-b", true);

    // Now should be ready
    let resp = client
        .get(server.url("/readyz"))
        .send()
        .await
        .expect("request failed");
    assert_eq!(resp.status(), StatusCode::OK);

    let body: ReadinessResponse = resp.json().await.expect("invalid json");
    assert!(body.ready);
    assert_eq!(body.dependencies.len(), 2);
}
