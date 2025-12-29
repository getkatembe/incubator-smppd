//! Hook system for plugin extensibility.
//!
//! Hooks allow plugins to intercept and modify behavior at well-defined
//! points in the message processing pipeline.

use crate::store::StoredMessage;
use serde::Serialize;
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Hook points in the message processing pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum HookPoint {
    // Message lifecycle
    /// Triggered when a message is first received
    OnMessageReceived,
    /// Before the filter chain processes the message
    BeforeFilter,
    /// After the filter chain has processed the message
    AfterFilter,
    /// Before route selection
    BeforeRoute,
    /// After route selection
    AfterRoute,
    /// Before forwarding to upstream
    BeforeForward,
    /// After receiving upstream response
    AfterForward,
    /// When a delivery receipt is received
    OnDeliveryReceipt,
    /// When message delivery fails
    OnMessageFailed,
    /// When message is successfully delivered
    OnMessageDelivered,

    // System events
    /// Configuration has been reloaded
    OnConfigReload,
    /// A cluster endpoint has gone down
    OnEndpointDown,
    /// A cluster endpoint has recovered
    OnEndpointUp,
    /// Server is shutting down
    OnShutdown,
}

impl fmt::Display for HookPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HookPoint::OnMessageReceived => write!(f, "on_message_received"),
            HookPoint::BeforeFilter => write!(f, "before_filter"),
            HookPoint::AfterFilter => write!(f, "after_filter"),
            HookPoint::BeforeRoute => write!(f, "before_route"),
            HookPoint::AfterRoute => write!(f, "after_route"),
            HookPoint::BeforeForward => write!(f, "before_forward"),
            HookPoint::AfterForward => write!(f, "after_forward"),
            HookPoint::OnDeliveryReceipt => write!(f, "on_delivery_receipt"),
            HookPoint::OnMessageFailed => write!(f, "on_message_failed"),
            HookPoint::OnMessageDelivered => write!(f, "on_message_delivered"),
            HookPoint::OnConfigReload => write!(f, "on_config_reload"),
            HookPoint::OnEndpointDown => write!(f, "on_endpoint_down"),
            HookPoint::OnEndpointUp => write!(f, "on_endpoint_up"),
            HookPoint::OnShutdown => write!(f, "on_shutdown"),
        }
    }
}

/// Result of executing a hook.
#[derive(Debug, Clone)]
pub enum HookAction {
    /// Continue to next hook handler
    Continue,
    /// Stop hook chain execution
    Stop,
    /// Continue but skip remaining hooks of same priority
    SkipPriority,
}

/// Context passed to hook handlers.
#[derive(Debug, Clone, Serialize)]
pub struct HookContext {
    /// The hook point being executed
    pub hook_point: HookPoint,
    /// Message being processed (if applicable, not serialized)
    #[serde(skip)]
    pub message: Option<StoredMessage>,
    /// Selected route name (for route-related hooks)
    pub route_name: Option<String>,
    /// Selected cluster name
    pub cluster_name: Option<String>,
    /// Endpoint address (for endpoint hooks)
    pub endpoint: Option<String>,
    /// Error message (for failure hooks)
    pub error: Option<String>,
    /// Additional context data
    pub data: HashMap<String, String>,
}

impl HookContext {
    /// Create a new hook context.
    pub fn new(hook_point: HookPoint) -> Self {
        Self {
            hook_point,
            message: None,
            route_name: None,
            cluster_name: None,
            endpoint: None,
            error: None,
            data: HashMap::new(),
        }
    }

    /// Create context for message-related hook.
    pub fn for_message(hook_point: HookPoint, message: StoredMessage) -> Self {
        Self {
            hook_point,
            message: Some(message),
            route_name: None,
            cluster_name: None,
            endpoint: None,
            error: None,
            data: HashMap::new(),
        }
    }

    /// Set route information.
    pub fn with_route(mut self, route_name: &str, cluster_name: &str) -> Self {
        self.route_name = Some(route_name.to_string());
        self.cluster_name = Some(cluster_name.to_string());
        self
    }

    /// Set endpoint information.
    pub fn with_endpoint(mut self, endpoint: &str) -> Self {
        self.endpoint = Some(endpoint.to_string());
        self
    }

    /// Set error information.
    pub fn with_error(mut self, error: &str) -> Self {
        self.error = Some(error.to_string());
        self
    }

    /// Add custom data.
    pub fn with_data(mut self, key: &str, value: &str) -> Self {
        self.data.insert(key.to_string(), value.to_string());
        self
    }
}

/// Type alias for async hook handler function.
pub type HookHandlerFn = Box<
    dyn Fn(HookContext) -> Pin<Box<dyn Future<Output = Result<HookAction, super::PluginError>> + Send>>
        + Send
        + Sync,
>;

/// A registered hook handler.
pub struct HookHandler {
    /// Plugin name that registered this handler
    pub plugin_name: String,
    /// Handler priority (lower = called first)
    pub priority: i32,
    /// The handler function
    pub handler: HookHandlerFn,
}

impl fmt::Debug for HookHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HookHandler")
            .field("plugin_name", &self.plugin_name)
            .field("priority", &self.priority)
            .finish()
    }
}

/// Registry for hook handlers.
#[derive(Default)]
pub struct HookRegistry {
    handlers: RwLock<HashMap<HookPoint, Vec<HookHandler>>>,
}

impl HookRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            handlers: RwLock::new(HashMap::new()),
        }
    }

    /// Register a hook handler.
    pub async fn register(
        &self,
        hook_point: HookPoint,
        plugin_name: &str,
        priority: i32,
        handler: HookHandlerFn,
    ) {
        let mut handlers = self.handlers.write().await;
        let entry = handlers.entry(hook_point).or_default();

        entry.push(HookHandler {
            plugin_name: plugin_name.to_string(),
            priority,
            handler,
        });

        // Sort by priority (lower first)
        entry.sort_by_key(|h| h.priority);

        tracing::debug!(
            hook = %hook_point,
            plugin = plugin_name,
            priority,
            "registered hook handler"
        );
    }

    /// Unregister all handlers for a plugin.
    pub async fn unregister_plugin(&self, plugin_name: &str) {
        let mut handlers = self.handlers.write().await;

        for (_, handlers) in handlers.iter_mut() {
            handlers.retain(|h| h.plugin_name != plugin_name);
        }

        tracing::debug!(plugin = plugin_name, "unregistered all hook handlers");
    }

    /// Execute all handlers for a hook point.
    pub async fn execute(&self, ctx: HookContext) -> Result<(), super::PluginError> {
        let handlers = self.handlers.read().await;

        if let Some(hook_handlers) = handlers.get(&ctx.hook_point) {
            for handler in hook_handlers {
                let result = (handler.handler)(ctx.clone()).await?;

                match result {
                    HookAction::Continue => continue,
                    HookAction::Stop => {
                        tracing::debug!(
                            hook = %ctx.hook_point,
                            plugin = &handler.plugin_name,
                            "hook chain stopped"
                        );
                        break;
                    }
                    HookAction::SkipPriority => {
                        // Skip remaining handlers with same priority
                        // (simplified: just continue for now)
                        continue;
                    }
                }
            }
        }

        Ok(())
    }

    /// Check if any handlers are registered for a hook point.
    pub async fn has_handlers(&self, hook_point: HookPoint) -> bool {
        let handlers = self.handlers.read().await;
        handlers
            .get(&hook_point)
            .map(|h| !h.is_empty())
            .unwrap_or(false)
    }

    /// Get count of registered handlers.
    pub async fn handler_count(&self) -> usize {
        let handlers = self.handlers.read().await;
        handlers.values().map(|v| v.len()).sum()
    }

    /// Get count of handlers for a specific hook point.
    pub async fn handler_count_for(&self, hook_point: HookPoint) -> usize {
        let handlers = self.handlers.read().await;
        handlers.get(&hook_point).map(|v| v.len()).unwrap_or(0)
    }
}

impl fmt::Debug for HookRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HookRegistry").finish()
    }
}

/// Shared hook registry.
pub type SharedHookRegistry = Arc<HookRegistry>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_point_display() {
        assert_eq!(HookPoint::OnMessageReceived.to_string(), "on_message_received");
        assert_eq!(HookPoint::BeforeFilter.to_string(), "before_filter");
        assert_eq!(HookPoint::AfterRoute.to_string(), "after_route");
    }

    #[test]
    fn test_hook_context_builder() {
        let ctx = HookContext::new(HookPoint::BeforeRoute)
            .with_route("moz-route", "vodacom")
            .with_data("key", "value");

        assert_eq!(ctx.hook_point, HookPoint::BeforeRoute);
        assert_eq!(ctx.route_name, Some("moz-route".to_string()));
        assert_eq!(ctx.cluster_name, Some("vodacom".to_string()));
        assert_eq!(ctx.data.get("key"), Some(&"value".to_string()));
    }

    #[tokio::test]
    async fn test_hook_registry() {
        let registry = HookRegistry::new();

        // Register a handler
        registry
            .register(
                HookPoint::OnMessageReceived,
                "test_plugin",
                10,
                Box::new(|_ctx| Box::pin(async { Ok(HookAction::Continue) })),
            )
            .await;

        assert!(registry.has_handlers(HookPoint::OnMessageReceived).await);
        assert!(!registry.has_handlers(HookPoint::BeforeFilter).await);
        assert_eq!(registry.handler_count().await, 1);

        // Unregister
        registry.unregister_plugin("test_plugin").await;
        assert!(!registry.has_handlers(HookPoint::OnMessageReceived).await);
    }

    #[tokio::test]
    async fn test_hook_execution_order() {
        let registry = HookRegistry::new();
        let execution_order = Arc::new(RwLock::new(Vec::new()));

        // Register handlers with different priorities
        let order1 = Arc::clone(&execution_order);
        registry
            .register(
                HookPoint::BeforeRoute,
                "plugin_b",
                20,
                Box::new(move |_ctx| {
                    let order = Arc::clone(&order1);
                    Box::pin(async move {
                        order.write().await.push("B");
                        Ok(HookAction::Continue)
                    })
                }),
            )
            .await;

        let order2 = Arc::clone(&execution_order);
        registry
            .register(
                HookPoint::BeforeRoute,
                "plugin_a",
                10,
                Box::new(move |_ctx| {
                    let order = Arc::clone(&order2);
                    Box::pin(async move {
                        order.write().await.push("A");
                        Ok(HookAction::Continue)
                    })
                }),
            )
            .await;

        // Execute
        let ctx = HookContext::new(HookPoint::BeforeRoute);
        registry.execute(ctx).await.unwrap();

        // Check order (lower priority should be first)
        let order = execution_order.read().await;
        assert_eq!(*order, vec!["A", "B"]);
    }
}
