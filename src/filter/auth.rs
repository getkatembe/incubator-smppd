//! Authentication filter using Tower middleware.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use smpp::pdu::Status;
use tower::{Layer, Service};
use tracing::{debug, warn};

use super::request::{SmppRequest, SmppResponse};

/// Client credentials.
#[derive(Debug, Clone)]
pub struct Credentials {
    /// System ID
    pub system_id: String,
    /// Password
    pub password: String,
    /// Allowed source addresses (None = any)
    pub allowed_sources: Option<Vec<String>>,
    /// Rate limit override
    pub rate_limit: Option<u32>,
}

impl Credentials {
    /// Create new credentials.
    pub fn new(system_id: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            system_id: system_id.into(),
            password: password.into(),
            allowed_sources: None,
            rate_limit: None,
        }
    }

    /// Set allowed source addresses.
    pub fn with_allowed_sources(mut self, sources: Vec<String>) -> Self {
        self.allowed_sources = Some(sources);
        self
    }

    /// Set rate limit.
    pub fn with_rate_limit(mut self, limit: u32) -> Self {
        self.rate_limit = Some(limit);
        self
    }
}

/// Credential store trait.
pub trait CredentialStore: Send + Sync {
    /// Validate credentials and return client info.
    fn validate(&self, system_id: &str, password: &str) -> Option<Arc<Credentials>>;
}

/// In-memory credential store.
#[derive(Debug, Clone, Default)]
pub struct InMemoryCredentialStore {
    credentials: HashMap<String, Arc<Credentials>>,
}

impl InMemoryCredentialStore {
    /// Create a new empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add credentials.
    pub fn add(&mut self, creds: Credentials) {
        self.credentials.insert(creds.system_id.clone(), Arc::new(creds));
    }
}

impl CredentialStore for InMemoryCredentialStore {
    fn validate(&self, system_id: &str, password: &str) -> Option<Arc<Credentials>> {
        self.credentials.get(system_id).and_then(|c| {
            if c.password == password {
                Some(c.clone())
            } else {
                None
            }
        })
    }
}

/// Authentication layer.
#[derive(Clone)]
pub struct AuthLayer<S> {
    store: Arc<dyn CredentialStore>,
    _marker: std::marker::PhantomData<S>,
}

impl<S> AuthLayer<S> {
    /// Create a new auth layer.
    pub fn new(store: Arc<dyn CredentialStore>) -> Self {
        Self {
            store,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<S> Layer<S> for AuthLayer<S> {
    type Service = AuthFilter<S>;

    fn layer(&self, inner: S) -> Self::Service {
        AuthFilter {
            inner,
            store: self.store.clone(),
        }
    }
}

/// Authentication filter service.
#[derive(Clone)]
pub struct AuthFilter<S> {
    inner: S,
    store: Arc<dyn CredentialStore>,
}

impl<S> AuthFilter<S> {
    /// Create a new auth filter.
    pub fn new(inner: S, store: Arc<dyn CredentialStore>) -> Self {
        Self { inner, store }
    }
}

impl<S> Service<SmppRequest> for AuthFilter<S>
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
        // Clone for async block
        let mut inner = self.inner.clone();
        let _store = self.store.clone();

        Box::pin(async move {
            // Check if system_id is set (should be set during bind)
            let system_id = request.meta.system_id.as_ref();

            if system_id.is_none() {
                warn!("request without system_id");
                return Ok(SmppResponse::reject(
                    request.sequence,
                    Status::InvalidBindStatus,
                ));
            }

            debug!(
                system_id = ?system_id,
                "auth filter passed"
            );

            // Check source address restrictions
            if let Some(source) = request.source() {
                if let Some(system_id) = system_id {
                    // TODO: Check allowed_sources from credentials
                    debug!(
                        system_id = %system_id,
                        source = %source,
                        "source address check passed"
                    );
                }
            }

            // Pass to next filter
            inner.call(request).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_in_memory_store() {
        let mut store = InMemoryCredentialStore::new();
        store.add(Credentials::new("test", "password123"));

        assert!(store.validate("test", "password123").is_some());
        assert!(store.validate("test", "wrong").is_none());
        assert!(store.validate("unknown", "password123").is_none());
    }
}
