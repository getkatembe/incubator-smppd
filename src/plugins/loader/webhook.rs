//! HTTP webhook plugin loader.
//!
//! Implements plugins as HTTP endpoints that receive JSON payloads
//! and respond with actions. Useful for:
//! - Integrating with external systems
//! - Language-agnostic plugin development
//! - Cloud function integration

use crate::plugins::{
    FilterAction, FilterContext, HookAction, HookContext, Plugin, PluginCapabilities,
    PluginConfig, PluginContext, PluginError, PluginType, RouteCandidate, RouteContext,
};
use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, warn};

/// Webhook plugin implementation.
pub struct WebhookPlugin {
    name: String,
    url: String,
    client: Client,
    config: PluginConfig,
    timeout: Duration,
    retry_count: u32,
    events: Vec<String>,
}

impl std::fmt::Debug for WebhookPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookPlugin")
            .field("name", &self.name)
            .field("url", &self.url)
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl WebhookPlugin {
    /// Create a new webhook plugin.
    pub fn new(
        name: &str,
        url: &str,
        config: &PluginConfig,
    ) -> Result<Box<dyn Plugin>, PluginError> {
        let timeout_ms = config.limits.timeout_ms;
        let timeout = Duration::from_millis(timeout_ms);

        let client = Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| PluginError::LoadFailed(format!("failed to create HTTP client: {}", e)))?;

        // Extract events from config
        let events: Vec<String> = config
            .config
            .get("events")
            .and_then(|v| v.as_sequence())
            .map(|seq| {
                seq.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let retry_count = config
            .config
            .get("retry_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(3) as u32;

        Ok(Box::new(Self {
            name: name.to_string(),
            url: url.to_string(),
            client,
            config: config.clone(),
            timeout,
            retry_count,
            events,
        }))
    }

    /// Send webhook request with retry.
    async fn send_request<T: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        endpoint: &str,
        payload: &T,
    ) -> Result<R, PluginError> {
        let url = format!("{}/{}", self.url.trim_end_matches('/'), endpoint);

        let mut last_error = None;

        for attempt in 0..=self.retry_count {
            if attempt > 0 {
                // Exponential backoff
                let delay = Duration::from_millis(100 * 2u64.pow(attempt - 1));
                tokio::time::sleep(delay).await;
                debug!(
                    name = %self.name,
                    attempt,
                    "retrying webhook request"
                );
            }

            match self
                .client
                .post(&url)
                .json(payload)
                .send()
                .await
            {
                Ok(response) => {
                    if response.status().is_success() {
                        return response.json::<R>().await.map_err(|e| {
                            PluginError::WebhookError(format!("failed to parse response: {}", e))
                        });
                    } else if response.status() == StatusCode::TOO_MANY_REQUESTS {
                        warn!(
                            name = %self.name,
                            status = %response.status(),
                            "webhook rate limited"
                        );
                        last_error = Some(PluginError::WebhookError("rate limited".into()));
                        continue;
                    } else if response.status().is_server_error() {
                        last_error = Some(PluginError::WebhookError(format!(
                            "server error: {}",
                            response.status()
                        )));
                        continue;
                    } else {
                        return Err(PluginError::WebhookError(format!(
                            "webhook returned status {}",
                            response.status()
                        )));
                    }
                }
                Err(e) => {
                    if e.is_timeout() {
                        last_error = Some(PluginError::Timeout(self.timeout));
                    } else if e.is_connect() {
                        last_error = Some(PluginError::WebhookError(format!(
                            "connection failed: {}",
                            e
                        )));
                    } else {
                        last_error = Some(PluginError::WebhookError(format!("request failed: {}", e)));
                    }
                    continue;
                }
            }
        }

        Err(last_error.unwrap_or_else(|| PluginError::WebhookError("unknown error".into())))
    }
}

#[async_trait]
impl Plugin for WebhookPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &str {
        "1.0.0"
    }

    fn plugin_type(&self) -> PluginType {
        PluginType::Webhook
    }

    fn capabilities(&self) -> PluginCapabilities {
        PluginCapabilities {
            can_filter: true,
            can_route: true,
            can_transform: true,
            can_hook: !self.events.is_empty(),
            can_store: false,
        }
    }

    async fn initialize(&mut self, ctx: &PluginContext) -> Result<(), PluginError> {
        // Optionally ping the webhook to verify connectivity
        if ctx.config.get("verify_on_init").and_then(|v| v.as_bool()).unwrap_or(false) {
            let payload = WebhookPing {
                event: "ping".to_string(),
                plugin_name: self.name.clone(),
            };

            match self.send_request::<_, WebhookPingResponse>("ping", &payload).await {
                Ok(response) if response.ok => {
                    debug!(name = %self.name, "webhook verified");
                }
                Ok(_) => {
                    warn!(name = %self.name, "webhook ping returned not ok");
                }
                Err(e) => {
                    warn!(name = %self.name, error = %e, "webhook ping failed");
                }
            }
        }

        debug!(name = %self.name, url = %self.url, "webhook plugin initialized");
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), PluginError> {
        debug!(name = %self.name, "webhook plugin shut down");
        Ok(())
    }
}

impl WebhookPlugin {
    /// Execute filter via webhook.
    pub async fn filter(&self, ctx: &FilterContext) -> Result<FilterAction, PluginError> {
        let payload = FilterRequest {
            source: ctx.source.clone(),
            destination: ctx.destination.clone(),
            sender_id: ctx.sender_id.clone(),
            system_id: ctx.system_id.clone(),
            content: ctx.content.clone(),
            esm_class: ctx.esm_class,
            data_coding: ctx.data_coding,
            client_ip: ctx.client_ip.clone(),
            tls_enabled: ctx.tls_enabled,
            metadata: ctx.metadata.clone(),
        };

        let response: FilterResponse = self.send_request("filter", &payload).await?;

        match response.action.as_str() {
            "continue" => Ok(FilterAction::Continue),
            "accept" => Ok(FilterAction::Accept),
            "drop" => Ok(FilterAction::Drop),
            "quarantine" => Ok(FilterAction::Quarantine),
            "modify" => Ok(FilterAction::Modify),
            "reject" => Ok(FilterAction::Reject {
                status: response.status.unwrap_or(8),
                reason: response.reason,
            }),
            _ => {
                warn!(action = %response.action, "unknown filter action from webhook");
                Ok(FilterAction::Continue)
            }
        }
    }

    /// Execute route selection via webhook.
    pub async fn select_route(
        &self,
        ctx: &RouteContext,
        candidates: &[RouteCandidate],
    ) -> Result<Option<String>, PluginError> {
        let payload = RouteRequest {
            destination: ctx.destination.clone(),
            source: ctx.source.clone(),
            system_id: ctx.system_id.clone(),
            priority: ctx.priority,
            candidates: candidates
                .iter()
                .map(|c| RouteCandidateDto {
                    route_name: c.route_name.clone(),
                    cluster_name: c.cluster_name.clone(),
                    priority: c.priority,
                    weight: c.weight,
                    quality_score: c.quality_score,
                    cost: c.cost,
                    latency_ms: c.latency_ms,
                })
                .collect(),
        };

        let response: RouteResponse = self.send_request("select_route", &payload).await?;

        Ok(response.selected_cluster)
    }

    /// Execute hook via webhook.
    pub async fn on_hook(&self, ctx: &HookContext) -> Result<HookAction, PluginError> {
        // Check if this hook is in our events list
        let hook_name = ctx.hook_point.to_string();
        if !self.events.is_empty() && !self.events.iter().any(|e| e == &hook_name) {
            return Ok(HookAction::Continue);
        }

        let payload = HookRequest {
            hook_point: hook_name,
            route_name: ctx.route_name.clone(),
            cluster_name: ctx.cluster_name.clone(),
            endpoint: ctx.endpoint.clone(),
            error: ctx.error.clone(),
            data: ctx.data.clone(),
        };

        let response: HookResponse = self.send_request("hook", &payload).await?;

        match response.action.as_deref() {
            Some("stop") => Ok(HookAction::Stop),
            Some("skip_priority") => Ok(HookAction::SkipPriority),
            _ => Ok(HookAction::Continue),
        }
    }
}

// Request/Response DTOs

#[derive(Debug, Serialize)]
struct WebhookPing {
    event: String,
    plugin_name: String,
}

#[derive(Debug, Deserialize)]
struct WebhookPingResponse {
    ok: bool,
}

#[derive(Debug, Serialize)]
struct FilterRequest {
    source: String,
    destination: String,
    sender_id: String,
    system_id: String,
    content: String,
    esm_class: u8,
    data_coding: u8,
    client_ip: Option<String>,
    tls_enabled: bool,
    metadata: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct FilterResponse {
    action: String,
    status: Option<u32>,
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct RouteRequest {
    destination: String,
    source: Option<String>,
    system_id: Option<String>,
    priority: i32,
    candidates: Vec<RouteCandidateDto>,
}

#[derive(Debug, Serialize)]
struct RouteCandidateDto {
    route_name: String,
    cluster_name: String,
    priority: i32,
    weight: u32,
    quality_score: Option<f64>,
    cost: Option<i64>,
    latency_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct RouteResponse {
    selected_cluster: Option<String>,
}

#[derive(Debug, Serialize)]
struct HookRequest {
    hook_point: String,
    route_name: Option<String>,
    cluster_name: Option<String>,
    endpoint: Option<String>,
    error: Option<String>,
    data: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct HookResponse {
    action: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> PluginConfig {
        PluginConfig {
            name: "test_webhook".to_string(),
            plugin_type: PluginType::Webhook,
            path: None,
            url: Some("http://localhost:8080".to_string()),
            enabled: true,
            priority: 0,
            limits: Default::default(),
            config: HashMap::new(),
        }
    }

    #[test]
    fn test_webhook_creation() {
        let config = test_config();
        let plugin = WebhookPlugin::new("test", "http://localhost:8080", &config).unwrap();
        assert_eq!(plugin.name(), "test");
        assert_eq!(plugin.plugin_type(), PluginType::Webhook);
    }

    #[test]
    fn test_capabilities() {
        let config = test_config();
        let plugin = WebhookPlugin::new("test", "http://localhost:8080", &config).unwrap();
        let caps = plugin.capabilities();
        assert!(caps.can_filter);
        assert!(caps.can_route);
        assert!(!caps.can_hook); // No events configured
    }
}
