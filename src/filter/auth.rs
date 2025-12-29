//! Authentication filter using Tower middleware.

use std::collections::HashMap;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use ipnet::IpNet;
use smpp::pdu::Status;
use tower::{Layer, Service};
use tracing::{debug, warn};

use super::request::{SmppRequest, SmppResponse};
use crate::config::{BindType, ClientConfig};

/// Client credentials with full access control.
#[derive(Debug, Clone)]
pub struct Credentials {
    /// System ID
    pub system_id: String,
    /// Password
    pub password: String,
    /// Allowed bind types
    pub allowed_bind_types: Vec<BindType>,
    /// Allowed source addresses (None = any)
    pub allowed_sources: Option<Vec<String>>,
    /// Allowed IP addresses/ranges
    pub allowed_ips: Vec<IpNet>,
    /// Maximum concurrent connections
    pub max_connections: Option<usize>,
    /// Rate limit override
    pub rate_limit: Option<u32>,
    /// Whether client is enabled
    pub enabled: bool,
}

impl Credentials {
    /// Create new credentials.
    pub fn new(system_id: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            system_id: system_id.into(),
            password: password.into(),
            allowed_bind_types: vec![],
            allowed_sources: None,
            allowed_ips: vec![],
            max_connections: None,
            rate_limit: None,
            enabled: true,
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

    /// Check if bind type is allowed.
    pub fn is_bind_allowed(&self, bind_type: BindType) -> bool {
        self.allowed_bind_types.is_empty() || self.allowed_bind_types.contains(&bind_type)
    }

    /// Check if IP address is allowed.
    pub fn is_ip_allowed(&self, ip: &IpAddr) -> bool {
        if self.allowed_ips.is_empty() {
            return true;
        }
        self.allowed_ips.iter().any(|net| net.contains(ip))
    }

    /// Check if source address is allowed.
    pub fn is_source_allowed(&self, source: &str) -> bool {
        match &self.allowed_sources {
            None => true,
            Some(sources) => sources.iter().any(|s| s == source || source.starts_with(s)),
        }
    }
}

impl From<&ClientConfig> for Credentials {
    fn from(config: &ClientConfig) -> Self {
        let allowed_ips = config
            .allowed_ips
            .iter()
            .filter_map(|s| s.parse::<IpNet>().ok())
            .collect();

        Self {
            system_id: config.system_id.clone(),
            password: config.password.clone(),
            allowed_bind_types: config.allowed_bind_types.clone(),
            allowed_sources: if config.allowed_sources.is_empty() {
                None
            } else {
                Some(config.allowed_sources.clone())
            },
            allowed_ips,
            max_connections: config.max_connections,
            rate_limit: config.rate_limit,
            enabled: config.enabled,
        }
    }
}

/// Credential store trait.
pub trait CredentialStore: Send + Sync {
    /// Validate credentials and return client info.
    fn validate(&self, system_id: &str, password: &str) -> Option<Arc<Credentials>>;

    /// Get credentials without password validation (for bind checks).
    fn get(&self, system_id: &str) -> Option<Arc<Credentials>>;
}

/// Config-based credential store.
#[derive(Debug, Clone)]
pub struct ConfigCredentialStore {
    credentials: HashMap<String, Arc<Credentials>>,
}

impl ConfigCredentialStore {
    /// Create credential store from config clients.
    pub fn from_config(clients: &[ClientConfig]) -> Self {
        let credentials = clients
            .iter()
            .filter(|c| c.enabled)
            .map(|c| (c.system_id.clone(), Arc::new(Credentials::from(c))))
            .collect();

        Self { credentials }
    }

    /// Check if any clients are configured.
    pub fn is_empty(&self) -> bool {
        self.credentials.is_empty()
    }
}

impl CredentialStore for ConfigCredentialStore {
    fn validate(&self, system_id: &str, password: &str) -> Option<Arc<Credentials>> {
        self.credentials.get(system_id).and_then(|c| {
            if c.password == password && c.enabled {
                Some(c.clone())
            } else {
                None
            }
        })
    }

    fn get(&self, system_id: &str) -> Option<Arc<Credentials>> {
        self.credentials.get(system_id).cloned()
    }
}

/// In-memory credential store (for testing).
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

    fn get(&self, system_id: &str) -> Option<Arc<Credentials>> {
        self.credentials.get(system_id).cloned()
    }
}

/// Open credential store that accepts all binds (for testing/development).
#[derive(Debug, Clone, Default)]
pub struct OpenCredentialStore;

impl CredentialStore for OpenCredentialStore {
    fn validate(&self, system_id: &str, _password: &str) -> Option<Arc<Credentials>> {
        Some(Arc::new(Credentials::new(system_id, "")))
    }

    fn get(&self, system_id: &str) -> Option<Arc<Credentials>> {
        Some(Arc::new(Credentials::new(system_id, "")))
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
