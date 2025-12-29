//! Plugin system for smppd.
//!
//! Provides a unified interface for extending smppd functionality through:
//! - Lua scripts (embedded mlua runtime)
//! - WebAssembly modules (wasmtime runtime)
//! - Native shared libraries (.so/.dll/.dylib)
//! - HTTP webhooks
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────┐
//! │                  PluginManager                       │
//! │  ┌─────────────────────────────────────────────────┐│
//! │  │              HookRegistry                        ││
//! │  │  OnMessageReceived → [handler1, handler2, ...]  ││
//! │  │  BeforeRoute → [handler1, ...]                  ││
//! │  └─────────────────────────────────────────────────┘│
//! │  ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌───────────┐ │
//! │  │   Lua   │ │  WASM   │ │ Native  │ │  Webhook  │ │
//! │  │ Plugins │ │ Plugins │ │ Plugins │ │  Plugins  │ │
//! │  └─────────┘ └─────────┘ └─────────┘ └───────────┘ │
//! └─────────────────────────────────────────────────────┘
//! ```
//!
//! # Example
//!
//! ```yaml
//! plugins:
//!   instances:
//!     - name: cost_router
//!       type: lua
//!       path: plugins/cost_router.lua
//!       config:
//!         strategy: balanced
//! ```

pub mod context;
pub mod hooks;
pub mod loader;
pub mod types;

pub use context::{FilterContext, PluginContext, RouteContext};
pub use hooks::{HookAction, HookContext, HookPoint, HookRegistry, SharedHookRegistry};
pub use types::{
    FilterAction, LogLevel, PluginCapabilities, PluginConfig, PluginError, PluginLimits,
    PluginType, PluginsConfig, RouteCandidate,
};

use async_trait::async_trait;
use std::collections::HashMap;
use std::fmt::Debug;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Core plugin trait that all plugins must implement.
#[async_trait]
pub trait Plugin: Send + Sync + Debug {
    /// Get the plugin name.
    fn name(&self) -> &str;

    /// Get the plugin version.
    fn version(&self) -> &str;

    /// Get the plugin type.
    fn plugin_type(&self) -> PluginType;

    /// Get plugin capabilities.
    fn capabilities(&self) -> PluginCapabilities;

    /// Initialize the plugin.
    async fn initialize(&mut self, ctx: &PluginContext) -> Result<(), PluginError>;

    /// Shutdown the plugin.
    async fn shutdown(&mut self) -> Result<(), PluginError>;
}

/// Filter plugin trait for message filtering.
#[async_trait]
pub trait FilterPlugin: Plugin {
    /// Process a message through the filter.
    async fn filter(&self, ctx: &mut FilterContext) -> Result<FilterAction, PluginError>;
}

/// Route plugin trait for route selection.
#[async_trait]
pub trait RoutePlugin: Plugin {
    /// Select a route from candidates.
    async fn select_route(
        &self,
        ctx: &RouteContext,
        candidates: &[RouteCandidate],
    ) -> Result<Option<String>, PluginError>;
}

/// Transform plugin trait for message modification.
#[async_trait]
pub trait TransformPlugin: Plugin {
    /// Transform a message.
    async fn transform(&self, ctx: &mut FilterContext) -> Result<(), PluginError>;
}

/// Plugin wrapper that holds the actual plugin implementation.
pub struct PluginInstance {
    /// The plugin implementation
    pub plugin: Box<dyn Plugin>,
    /// Plugin configuration
    pub config: PluginConfig,
    /// Whether the plugin is enabled
    pub enabled: bool,
}

impl Debug for PluginInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginInstance")
            .field("name", &self.config.name)
            .field("type", &self.config.plugin_type)
            .field("enabled", &self.enabled)
            .finish()
    }
}

/// Plugin manager for loading and managing plugins.
pub struct PluginManager {
    /// Loaded plugins
    plugins: RwLock<HashMap<String, PluginInstance>>,
    /// Hook registry
    hooks: Arc<HookRegistry>,
    /// Filter plugins (cached for performance)
    filter_plugins: RwLock<Vec<Arc<dyn FilterPlugin>>>,
    /// Route plugins (cached for performance)
    route_plugins: RwLock<Vec<Arc<dyn RoutePlugin>>>,
    /// Plugin directory
    plugin_dir: Option<std::path::PathBuf>,
}

impl PluginManager {
    /// Create a new plugin manager.
    pub fn new() -> Self {
        Self {
            plugins: RwLock::new(HashMap::new()),
            hooks: Arc::new(HookRegistry::new()),
            filter_plugins: RwLock::new(Vec::new()),
            route_plugins: RwLock::new(Vec::new()),
            plugin_dir: None,
        }
    }

    /// Create with plugin directory.
    pub fn with_plugin_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.plugin_dir = Some(dir);
        self
    }

    /// Get the hook registry.
    pub fn hooks(&self) -> &Arc<HookRegistry> {
        &self.hooks
    }

    /// Load plugins from configuration.
    pub async fn load_from_config(&self, config: &PluginsConfig) -> Result<(), PluginError> {
        let base_dir = config.directory.clone().or_else(|| self.plugin_dir.clone());

        for plugin_config in &config.instances {
            if !plugin_config.enabled {
                debug!(name = %plugin_config.name, "skipping disabled plugin");
                continue;
            }

            match self.load_plugin(plugin_config, base_dir.as_deref()).await {
                Ok(_) => info!(name = %plugin_config.name, "loaded plugin"),
                Err(e) => {
                    error!(name = %plugin_config.name, error = %e, "failed to load plugin");
                    // Continue loading other plugins
                }
            }
        }

        Ok(())
    }

    /// Load a single plugin.
    pub async fn load_plugin(
        &self,
        config: &PluginConfig,
        base_dir: Option<&Path>,
    ) -> Result<(), PluginError> {
        debug!(name = %config.name, plugin_type = %config.plugin_type, "loading plugin");

        let plugin: Box<dyn Plugin> = match config.plugin_type {
            PluginType::Lua => {
                let path = self.resolve_path(&config.path, base_dir)?;
                loader::lua::LuaPluginLoader::load(&path, config).await?
            }
            PluginType::Wasm => {
                // WASM support will be added later
                return Err(PluginError::LoadFailed(
                    "WASM plugins not yet implemented".into(),
                ));
            }
            PluginType::Native => {
                // Native support will be added later
                return Err(PluginError::LoadFailed(
                    "Native plugins not yet implemented".into(),
                ));
            }
            PluginType::Webhook => {
                let url = config
                    .url
                    .as_ref()
                    .ok_or_else(|| PluginError::InvalidConfig("webhook requires url".into()))?;
                loader::webhook::WebhookPlugin::new(&config.name, url, config)?
            }
        };

        let mut ctx = PluginContext::new(&config.name).with_config(config.config.clone());
        if let Some(dir) = base_dir {
            ctx = ctx.with_plugin_dir(dir.to_path_buf());
        }

        let mut plugin = plugin;
        plugin.initialize(&ctx).await?;

        let instance = PluginInstance {
            plugin,
            config: config.clone(),
            enabled: true,
        };

        let mut plugins = self.plugins.write().await;
        plugins.insert(config.name.clone(), instance);

        info!(name = %config.name, "plugin initialized");
        Ok(())
    }

    /// Unload a plugin.
    pub async fn unload_plugin(&self, name: &str) -> Result<(), PluginError> {
        let mut plugins = self.plugins.write().await;

        if let Some(mut instance) = plugins.remove(name) {
            instance.plugin.shutdown().await?;
            self.hooks.unregister_plugin(name).await;
            info!(name = name, "plugin unloaded");
            Ok(())
        } else {
            Err(PluginError::NotFound(name.to_string()))
        }
    }

    /// Reload a plugin.
    pub async fn reload_plugin(&self, name: &str) -> Result<(), PluginError> {
        let config = {
            let plugins = self.plugins.read().await;
            plugins
                .get(name)
                .map(|i| i.config.clone())
                .ok_or_else(|| PluginError::NotFound(name.to_string()))?
        };

        self.unload_plugin(name).await?;
        self.load_plugin(&config, self.plugin_dir.as_deref()).await
    }

    /// Execute a hook.
    pub async fn execute_hook(&self, ctx: HookContext) -> Result<(), PluginError> {
        self.hooks.execute(ctx).await
    }

    /// Get plugin count.
    pub async fn plugin_count(&self) -> usize {
        self.plugins.read().await.len()
    }

    /// Check if a plugin is loaded.
    pub async fn has_plugin(&self, name: &str) -> bool {
        self.plugins.read().await.contains_key(name)
    }

    /// Get plugin names.
    pub async fn plugin_names(&self) -> Vec<String> {
        self.plugins.read().await.keys().cloned().collect()
    }

    /// Shutdown all plugins.
    pub async fn shutdown(&self) -> Result<(), PluginError> {
        let mut plugins = self.plugins.write().await;

        for (name, instance) in plugins.iter_mut() {
            if let Err(e) = instance.plugin.shutdown().await {
                warn!(name = name, error = %e, "error shutting down plugin");
            }
        }

        plugins.clear();
        info!("all plugins shut down");
        Ok(())
    }

    /// Resolve plugin path.
    fn resolve_path(
        &self,
        path: &Option<std::path::PathBuf>,
        base_dir: Option<&Path>,
    ) -> Result<std::path::PathBuf, PluginError> {
        let path = path
            .as_ref()
            .ok_or_else(|| PluginError::InvalidConfig("plugin path required".into()))?;

        if path.is_absolute() {
            Ok(path.clone())
        } else if let Some(base) = base_dir {
            Ok(base.join(path))
        } else {
            Ok(path.clone())
        }
    }
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Debug for PluginManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginManager")
            .field("plugin_dir", &self.plugin_dir)
            .finish()
    }
}

/// Shared plugin manager.
pub type SharedPluginManager = Arc<PluginManager>;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_plugin_manager_new() {
        let manager = PluginManager::new();
        assert_eq!(manager.plugin_count().await, 0);
    }

    #[tokio::test]
    async fn test_plugin_manager_with_dir() {
        let manager = PluginManager::new().with_plugin_dir("/tmp/plugins".into());
        assert_eq!(
            manager.plugin_dir,
            Some(std::path::PathBuf::from("/tmp/plugins"))
        );
    }

    #[tokio::test]
    async fn test_hook_execution() {
        let manager = PluginManager::new();

        // Register a hook
        manager
            .hooks()
            .register(
                HookPoint::OnMessageReceived,
                "test_plugin",
                10,
                Box::new(|_ctx| Box::pin(async { Ok(HookAction::Continue) })),
            )
            .await;

        // Execute the hook
        let ctx = HookContext::new(HookPoint::OnMessageReceived);
        manager.execute_hook(ctx).await.unwrap();

        assert!(manager.hooks().has_handlers(HookPoint::OnMessageReceived).await);
    }
}
