//! Filter chain builder.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tower::{Layer, Service, ServiceBuilder};

use super::auth::CredentialStore;
use super::ratelimit::{RateLimitConfig, RateLimitLayer};
use super::request::{SmppRequest, SmppResponse};

/// Terminal service that returns Continue response.
#[derive(Clone, Copy)]
pub struct PassthroughService;

impl Service<SmppRequest> for PassthroughService {
    type Response = SmppResponse;
    type Error = std::convert::Infallible;
    type Future = std::future::Ready<Result<SmppResponse, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: SmppRequest) -> Self::Future {
        std::future::ready(Ok(SmppResponse::continue_with(request)))
    }
}

/// Filter chain configuration.
#[derive(Default)]
pub struct FilterChainBuilder {
    /// Enable authentication
    auth_store: Option<Arc<dyn CredentialStore>>,
    /// Rate limiting config
    rate_limit: Option<RateLimitConfig>,
}

impl FilterChainBuilder {
    /// Create a new filter chain builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable authentication.
    pub fn with_auth(mut self, store: Arc<dyn CredentialStore>) -> Self {
        self.auth_store = Some(store);
        self
    }

    /// Enable rate limiting.
    pub fn with_rate_limit(mut self, config: RateLimitConfig) -> Self {
        self.rate_limit = Some(config);
        self
    }

    /// Build the filter chain.
    ///
    /// Returns a boxed service that can be used to process requests.
    pub fn build(self) -> FilterChain {
        // Start with passthrough
        let builder = ServiceBuilder::new();

        // Add rate limiting (innermost, applied last)
        let chain: Box<dyn CloneService> = if let Some(config) = self.rate_limit {
            Box::new(RateLimitLayer::new(config).layer(PassthroughService))
        } else {
            Box::new(PassthroughService)
        };

        FilterChain { inner: chain }
    }
}

/// Trait for cloneable services.
pub trait CloneService: Send + Sync {
    fn call_boxed(
        &self,
        request: SmppRequest,
    ) -> Pin<Box<dyn Future<Output = Result<SmppResponse, BoxError>> + Send>>;
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

impl<S> CloneService for S
where
    S: Service<SmppRequest, Response = SmppResponse> + Clone + Send + Sync + 'static,
    S::Error: Into<BoxError>,
    S::Future: Send + 'static,
{
    fn call_boxed(
        &self,
        request: SmppRequest,
    ) -> Pin<Box<dyn Future<Output = Result<SmppResponse, BoxError>> + Send>> {
        let mut svc = self.clone();
        Box::pin(async move { svc.call(request).await.map_err(Into::into) })
    }
}

/// The built filter chain.
pub struct FilterChain {
    inner: Box<dyn CloneService>,
}

impl FilterChain {
    /// Process a request through the filter chain.
    pub async fn process(&self, request: SmppRequest) -> Result<SmppResponse, BoxError> {
        self.inner.call_boxed(request).await
    }
}

impl Clone for FilterChain {
    fn clone(&self) -> Self {
        // TODO: Implement proper cloning
        FilterChainBuilder::new().build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smpp::pdu::Pdu;

    #[tokio::test]
    async fn test_passthrough() {
        let chain = FilterChainBuilder::new().build();
        let request = SmppRequest::new(Pdu::EnquireLink(smpp::pdu::EnquireLink), 1);
        let response = chain.process(request).await.unwrap();
        assert!(response.is_continue());
    }

    #[tokio::test]
    async fn test_with_rate_limit() {
        let chain = FilterChainBuilder::new()
            .with_rate_limit(RateLimitConfig::new(100))
            .build();

        let request = SmppRequest::new(Pdu::EnquireLink(smpp::pdu::EnquireLink), 1);
        let response = chain.process(request).await.unwrap();
        assert!(response.is_continue());
    }
}
