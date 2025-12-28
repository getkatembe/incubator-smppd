//! Rate limiting filter using Tower middleware.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

use smpp::pdu::Status;
use tokio::sync::RwLock;
use tower::{Layer, Service};
use tracing::{debug, warn};

use super::request::{SmppRequest, SmppResponse};

/// Rate limit configuration.
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Requests per second limit
    pub requests_per_second: u32,
    /// Burst size (token bucket capacity)
    pub burst_size: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            requests_per_second: 100,
            burst_size: 100,
        }
    }
}

impl RateLimitConfig {
    /// Create a new rate limit config.
    pub fn new(requests_per_second: u32) -> Self {
        Self {
            requests_per_second,
            burst_size: requests_per_second,
        }
    }

    /// Set burst size.
    pub fn with_burst(mut self, burst: u32) -> Self {
        self.burst_size = burst;
        self
    }
}

/// Token bucket rate limiter.
struct TokenBucket {
    /// Tokens available
    tokens: AtomicU64,
    /// Last refill time
    last_refill: RwLock<Instant>,
    /// Refill rate (tokens per second)
    rate: u32,
    /// Maximum tokens
    capacity: u32,
}

impl TokenBucket {
    fn new(rate: u32, capacity: u32) -> Self {
        Self {
            tokens: AtomicU64::new(capacity as u64),
            last_refill: RwLock::new(Instant::now()),
            rate,
            capacity,
        }
    }

    async fn try_acquire(&self) -> bool {
        // Refill tokens based on elapsed time
        let now = Instant::now();
        {
            let mut last = self.last_refill.write().await;
            let elapsed = now.duration_since(*last);
            let new_tokens = (elapsed.as_secs_f64() * self.rate as f64) as u64;

            if new_tokens > 0 {
                let current = self.tokens.load(Ordering::Relaxed);
                let new_total = (current + new_tokens).min(self.capacity as u64);
                self.tokens.store(new_total, Ordering::Relaxed);
                *last = now;
            }
        }

        // Try to consume a token
        loop {
            let current = self.tokens.load(Ordering::Relaxed);
            if current == 0 {
                return false;
            }

            if self
                .tokens
                .compare_exchange(current, current - 1, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return true;
            }
        }
    }

    fn available(&self) -> u64 {
        self.tokens.load(Ordering::Relaxed)
    }
}

/// Rate limiter store (per-client rate limiting).
pub struct RateLimiterStore {
    /// Per-client buckets
    buckets: RwLock<HashMap<String, Arc<TokenBucket>>>,
    /// Default config
    default_config: RateLimitConfig,
}

impl RateLimiterStore {
    /// Create a new store.
    pub fn new(default_config: RateLimitConfig) -> Self {
        Self {
            buckets: RwLock::new(HashMap::new()),
            default_config,
        }
    }

    /// Get or create a bucket for a client.
    async fn get_bucket(&self, client_id: &str) -> Arc<TokenBucket> {
        // Check if exists
        {
            let buckets = self.buckets.read().await;
            if let Some(bucket) = buckets.get(client_id) {
                return bucket.clone();
            }
        }

        // Create new bucket
        let bucket = Arc::new(TokenBucket::new(
            self.default_config.requests_per_second,
            self.default_config.burst_size,
        ));

        let mut buckets = self.buckets.write().await;
        buckets.insert(client_id.to_string(), bucket.clone());
        bucket
    }

    /// Try to acquire a token for a client.
    pub async fn try_acquire(&self, client_id: &str) -> bool {
        let bucket = self.get_bucket(client_id).await;
        bucket.try_acquire().await
    }
}

/// Rate limit layer.
#[derive(Clone)]
pub struct RateLimitLayer {
    store: Arc<RateLimiterStore>,
}

impl RateLimitLayer {
    /// Create a new rate limit layer.
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            store: Arc::new(RateLimiterStore::new(config)),
        }
    }

    /// Create with a shared store.
    pub fn with_store(store: Arc<RateLimiterStore>) -> Self {
        Self { store }
    }
}

impl<S> Layer<S> for RateLimitLayer {
    type Service = RateLimitFilter<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RateLimitFilter {
            inner,
            store: self.store.clone(),
        }
    }
}

/// Rate limit filter service.
#[derive(Clone)]
pub struct RateLimitFilter<S> {
    inner: S,
    store: Arc<RateLimiterStore>,
}

impl<S> RateLimitFilter<S> {
    /// Create a new rate limit filter.
    pub fn new(inner: S, store: Arc<RateLimiterStore>) -> Self {
        Self { inner, store }
    }
}

impl<S> Service<SmppRequest> for RateLimitFilter<S>
where
    S: Service<SmppRequest, Response = SmppResponse> + Clone + Send + 'static,
    S::Future: Send,
{
    type Response = SmppResponse;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: SmppRequest) -> Self::Future {
        let mut inner = self.inner.clone();
        let store = self.store.clone();

        Box::pin(async move {
            // Get client ID from metadata
            let client_id = request
                .meta
                .system_id
                .as_deref()
                .unwrap_or("anonymous");

            // Try to acquire a token
            if !store.try_acquire(client_id).await {
                warn!(
                    client_id = %client_id,
                    "rate limit exceeded"
                );

                return Ok(SmppResponse::reject_with_reason(
                    request.sequence,
                    Status::SystemError, // ESME_RTHROTTLED in some implementations
                    "rate limit exceeded",
                ));
            }

            debug!(
                client_id = %client_id,
                "rate limit check passed"
            );

            // Pass to next filter
            inner.call(request).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_token_bucket() {
        let bucket = TokenBucket::new(10, 5); // 10/s, burst 5

        // Should have initial capacity
        assert!(bucket.try_acquire().await);
        assert!(bucket.try_acquire().await);
        assert!(bucket.try_acquire().await);
        assert!(bucket.try_acquire().await);
        assert!(bucket.try_acquire().await);

        // Should be exhausted
        assert!(!bucket.try_acquire().await);
    }

    #[tokio::test]
    async fn test_rate_limiter_store() {
        let config = RateLimitConfig::new(100).with_burst(2);
        let store = RateLimiterStore::new(config);

        assert!(store.try_acquire("client1").await);
        assert!(store.try_acquire("client1").await);
        assert!(!store.try_acquire("client1").await);

        // Different client has separate bucket
        assert!(store.try_acquire("client2").await);
    }
}
