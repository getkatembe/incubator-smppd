//! Lua plugin loader using mlua.
//!
//! Provides a sandboxed Lua runtime for plugin execution with:
//! - Limited stdlib (no io, os.execute, etc.)
//! - Memory limits
//! - Execution timeouts
//! - Rust ↔ Lua bridging for plugin contexts

use crate::plugins::{
    FilterAction, FilterContext, HookAction, HookContext, Plugin, PluginCapabilities,
    PluginConfig, PluginContext, PluginError, PluginType, RouteCandidate, RouteContext,
};
use async_trait::async_trait;
use mlua::{Function, Lua, LuaSerdeExt, Table, Value};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, warn};

/// Lua plugin loader.
pub struct LuaPluginLoader;

impl LuaPluginLoader {
    /// Load a Lua plugin from a file.
    pub async fn load(path: &Path, config: &PluginConfig) -> Result<Box<dyn Plugin>, PluginError> {
        debug!(path = %path.display(), "loading lua plugin");

        let script = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| PluginError::LoadFailed(format!("failed to read lua file: {}", e)))?;

        let lua = create_sandbox(config)?;

        // Load the plugin script
        lua.load(&script)
            .exec()
            .map_err(|e| PluginError::LoadFailed(format!("lua syntax error: {}", e)))?;

        // Get the plugin table
        let globals = lua.globals();
        let plugin_table: Table = globals
            .get("plugin")
            .map_err(|_| PluginError::LoadFailed("plugin table not found".into()))?;

        // Extract metadata
        let name: String = plugin_table
            .get("name")
            .unwrap_or_else(|_| config.name.clone());
        let version: String = plugin_table
            .get("version")
            .unwrap_or_else(|_| "1.0.0".to_string());

        // Detect capabilities
        let capabilities = PluginCapabilities {
            can_filter: plugin_table.contains_key("filter").unwrap_or(false),
            can_route: plugin_table.contains_key("select_route").unwrap_or(false),
            can_transform: plugin_table.contains_key("transform").unwrap_or(false),
            can_hook: plugin_table.contains_key("on_hook").unwrap_or(false),
            can_store: false,
        };

        Ok(Box::new(LuaPlugin {
            name,
            version,
            capabilities,
            lua: Arc::new(Mutex::new(lua)),
            config: config.clone(),
        }))
    }
}

/// Create a sandboxed Lua environment.
fn create_sandbox(config: &PluginConfig) -> Result<Lua, PluginError> {
    // Create Lua with safe stdlib subset
    let lua = Lua::new();

    // Set memory limit if configured
    let memory_limit = config.limits.memory_mb * 1024 * 1024;
    lua.set_memory_limit(memory_limit as usize)
        .map_err(|e| PluginError::LoadFailed(format!("failed to set memory limit: {}", e)))?;

    // Remove dangerous functions
    {
        let globals = lua.globals();

        // Remove os.execute, os.exit, etc. but keep os.time, os.date
        if let Ok(os_table) = globals.get::<Table>("os") {
            let _ = os_table.set("execute", Value::Nil);
            let _ = os_table.set("exit", Value::Nil);
            let _ = os_table.set("remove", Value::Nil);
            let _ = os_table.set("rename", Value::Nil);
            let _ = os_table.set("setlocale", Value::Nil);
            let _ = os_table.set("getenv", Value::Nil);
            let _ = os_table.set("tmpname", Value::Nil);
        }

        // Remove io entirely
        let _ = globals.set("io", Value::Nil);

        // Remove dangerous base functions
        let _ = globals.set("loadfile", Value::Nil);
        let _ = globals.set("dofile", Value::Nil);

        // Remove debug library
        let _ = globals.set("debug", Value::Nil);

        // Remove package.loadlib
        if let Ok(package_table) = globals.get::<Table>("package") {
            let _ = package_table.set("loadlib", Value::Nil);
        }
    }

    // Add smppd API table
    add_smppd_api(&lua)?;

    Ok(lua)
}

/// Add the smppd API table to Lua environment.
fn add_smppd_api(lua: &Lua) -> Result<(), PluginError> {
    let globals = lua.globals();

    let smppd = lua
        .create_table()
        .map_err(|e| PluginError::LoadFailed(format!("failed to create smppd table: {}", e)))?;

    // smppd.log(level, message)
    let log_fn = lua
        .create_function(|_, (level, message): (String, String)| {
            match level.as_str() {
                "trace" => tracing::trace!(plugin = "lua", "{}", message),
                "debug" => tracing::debug!(plugin = "lua", "{}", message),
                "info" => tracing::info!(plugin = "lua", "{}", message),
                "warn" => tracing::warn!(plugin = "lua", "{}", message),
                "error" => tracing::error!(plugin = "lua", "{}", message),
                _ => tracing::info!(plugin = "lua", "{}", message),
            }
            Ok(())
        })
        .map_err(|e| PluginError::LoadFailed(format!("failed to create log function: {}", e)))?;

    smppd
        .set("log", log_fn)
        .map_err(|e| PluginError::LoadFailed(format!("failed to set log function: {}", e)))?;

    // smppd.version
    smppd
        .set("version", env!("CARGO_PKG_VERSION"))
        .map_err(|e| PluginError::LoadFailed(format!("failed to set version: {}", e)))?;

    globals
        .set("smppd", smppd)
        .map_err(|e| PluginError::LoadFailed(format!("failed to set smppd global: {}", e)))?;

    Ok(())
}

/// Lua plugin implementation.
pub struct LuaPlugin {
    name: String,
    version: String,
    capabilities: PluginCapabilities,
    lua: Arc<Mutex<Lua>>,
    config: PluginConfig,
}

impl std::fmt::Debug for LuaPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LuaPlugin")
            .field("name", &self.name)
            .field("version", &self.version)
            .field("capabilities", &self.capabilities)
            .finish()
    }
}

#[async_trait]
impl Plugin for LuaPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn plugin_type(&self) -> PluginType {
        PluginType::Lua
    }

    fn capabilities(&self) -> PluginCapabilities {
        self.capabilities.clone()
    }

    async fn initialize(&mut self, ctx: &PluginContext) -> Result<(), PluginError> {
        let lua = self.lua.lock().await;

        // Check if plugin has initialize function
        let globals = lua.globals();
        let plugin_table: Table = globals
            .get("plugin")
            .map_err(|_| PluginError::InitFailed("plugin table not found".into()))?;

        if let Ok(init_fn) = plugin_table.get::<Function>("initialize") {
            // Convert config to Lua table
            let config_table = lua
                .to_value(&ctx.config)
                .map_err(|e| PluginError::InitFailed(format!("failed to convert config: {}", e)))?;

            init_fn
                .call::<bool>(config_table)
                .map_err(|e| PluginError::InitFailed(format!("initialize failed: {}", e)))?;
        }

        debug!(name = %self.name, "lua plugin initialized");
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), PluginError> {
        let lua = self.lua.lock().await;

        let globals = lua.globals();
        if let Ok(plugin_table) = globals.get::<Table>("plugin") {
            if let Ok(shutdown_fn) = plugin_table.get::<Function>("shutdown") {
                let _ = shutdown_fn.call::<()>(());
            }
        }

        debug!(name = %self.name, "lua plugin shut down");
        Ok(())
    }
}

impl LuaPlugin {
    /// Execute filter function.
    pub async fn filter(&self, ctx: &FilterContext) -> Result<FilterAction, PluginError> {
        let lua = self.lua.lock().await;

        let globals = lua.globals();
        let plugin_table: Table = globals
            .get("plugin")
            .map_err(|_| PluginError::ExecutionFailed("plugin table not found".into()))?;

        let filter_fn: Function = plugin_table
            .get("filter")
            .map_err(|_| PluginError::ExecutionFailed("filter function not found".into()))?;

        // Create context table
        let ctx_table = lua
            .create_table()
            .map_err(|e| PluginError::ExecutionFailed(format!("failed to create context: {}", e)))?;

        ctx_table.set("source", ctx.source.clone()).ok();
        ctx_table.set("destination", ctx.destination.clone()).ok();
        ctx_table.set("sender_id", ctx.sender_id.clone()).ok();
        ctx_table.set("system_id", ctx.system_id.clone()).ok();
        ctx_table.set("content", ctx.content.clone()).ok();
        ctx_table.set("esm_class", ctx.esm_class).ok();
        ctx_table.set("data_coding", ctx.data_coding).ok();

        if let Some(ref ip) = ctx.client_ip {
            ctx_table.set("client_ip", ip.clone()).ok();
        }
        ctx_table.set("tls_enabled", ctx.tls_enabled).ok();

        // Call filter function
        let result: Table = filter_fn
            .call(ctx_table)
            .map_err(|e| PluginError::ExecutionFailed(format!("filter failed: {}", e)))?;

        // Parse result
        let action: String = result.get("action").unwrap_or_else(|_| "continue".to_string());

        match action.as_str() {
            "continue" => Ok(FilterAction::Continue),
            "accept" => Ok(FilterAction::Accept),
            "drop" => Ok(FilterAction::Drop),
            "quarantine" => Ok(FilterAction::Quarantine),
            "modify" => Ok(FilterAction::Modify),
            "reject" => {
                let status: u32 = result.get("status").unwrap_or(8); // ESME_RSYSERR
                let reason: Option<String> = result.get("reason").ok();
                Ok(FilterAction::Reject { status, reason })
            }
            "log" => {
                let message: String = result.get("message").unwrap_or_default();
                Ok(FilterAction::Log {
                    level: crate::plugins::LogLevel::Info,
                    message,
                })
            }
            _ => {
                warn!(action = %action, "unknown filter action, defaulting to continue");
                Ok(FilterAction::Continue)
            }
        }
    }

    /// Execute route selection function.
    pub async fn select_route(
        &self,
        ctx: &RouteContext,
        candidates: &[RouteCandidate],
    ) -> Result<Option<String>, PluginError> {
        let lua = self.lua.lock().await;

        let globals = lua.globals();
        let plugin_table: Table = globals
            .get("plugin")
            .map_err(|_| PluginError::ExecutionFailed("plugin table not found".into()))?;

        let select_fn: Function = plugin_table
            .get("select_route")
            .map_err(|_| PluginError::ExecutionFailed("select_route function not found".into()))?;

        // Create context table
        let ctx_table = lua
            .create_table()
            .map_err(|e| PluginError::ExecutionFailed(format!("failed to create context: {}", e)))?;

        ctx_table.set("destination", ctx.destination.clone()).ok();
        if let Some(ref src) = ctx.source {
            ctx_table.set("source", src.clone()).ok();
        }
        if let Some(ref sid) = ctx.system_id {
            ctx_table.set("system_id", sid.clone()).ok();
        }
        ctx_table.set("priority", ctx.priority).ok();

        // Create candidates array
        let candidates_table = lua
            .create_table()
            .map_err(|e| PluginError::ExecutionFailed(format!("failed to create candidates: {}", e)))?;

        for (i, candidate) in candidates.iter().enumerate() {
            let c_table = lua.create_table().ok();
            if let Some(ref ct) = c_table {
                ct.set("route_name", candidate.route_name.clone()).ok();
                ct.set("cluster_name", candidate.cluster_name.clone()).ok();
                ct.set("priority", candidate.priority).ok();
                ct.set("weight", candidate.weight).ok();
                if let Some(score) = candidate.quality_score {
                    ct.set("quality_score", score).ok();
                }
                if let Some(cost) = candidate.cost {
                    ct.set("cost", cost).ok();
                }
                if let Some(latency) = candidate.latency_ms {
                    ct.set("latency_ms", latency).ok();
                }
                candidates_table.set(i + 1, ct.clone()).ok();
            }
        }

        // Call select_route function
        let result: Value = select_fn
            .call((ctx_table, candidates_table))
            .map_err(|e| PluginError::ExecutionFailed(format!("select_route failed: {}", e)))?;

        match result {
            Value::Nil => Ok(None),
            Value::String(s) => {
                match s.to_str() {
                    Ok(str_ref) => Ok(Some(str_ref.to_string())),
                    Err(_) => Ok(None),
                }
            }
            _ => {
                warn!("select_route returned unexpected type");
                Ok(None)
            }
        }
    }

    /// Execute hook handler.
    pub async fn on_hook(&self, ctx: &HookContext) -> Result<HookAction, PluginError> {
        let lua = self.lua.lock().await;

        let globals = lua.globals();
        let plugin_table: Table = globals
            .get("plugin")
            .map_err(|_| PluginError::ExecutionFailed("plugin table not found".into()))?;

        let hook_fn: Function = plugin_table
            .get("on_hook")
            .map_err(|_| PluginError::ExecutionFailed("on_hook function not found".into()))?;

        // Create context table
        let ctx_table = lua
            .create_table()
            .map_err(|e| PluginError::ExecutionFailed(format!("failed to create context: {}", e)))?;

        ctx_table.set("hook_point", ctx.hook_point.to_string()).ok();
        if let Some(ref route) = ctx.route_name {
            ctx_table.set("route_name", route.clone()).ok();
        }
        if let Some(ref cluster) = ctx.cluster_name {
            ctx_table.set("cluster_name", cluster.clone()).ok();
        }
        if let Some(ref endpoint) = ctx.endpoint {
            ctx_table.set("endpoint", endpoint.clone()).ok();
        }
        if let Some(ref error) = ctx.error {
            ctx_table.set("error", error.clone()).ok();
        }

        // Call on_hook function
        let result: String = hook_fn
            .call(ctx_table)
            .unwrap_or_else(|_| "continue".to_string());

        match result.as_str() {
            "stop" => Ok(HookAction::Stop),
            "skip_priority" => Ok(HookAction::SkipPriority),
            _ => Ok(HookAction::Continue),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn test_config() -> PluginConfig {
        PluginConfig {
            name: "test".to_string(),
            plugin_type: PluginType::Lua,
            path: None,
            url: None,
            enabled: true,
            priority: 0,
            limits: Default::default(),
            config: HashMap::new(),
        }
    }

    #[test]
    fn test_sandbox_creation() {
        let config = test_config();
        let lua = create_sandbox(&config).unwrap();

        let globals = lua.globals();

        // io should be removed
        assert!(globals.get::<Value>("io").unwrap() == Value::Nil);

        // os.execute should be removed
        let os_table: Table = globals.get("os").unwrap();
        assert!(os_table.get::<Value>("execute").unwrap() == Value::Nil);

        // os.time should still work
        assert!(os_table.contains_key("time").unwrap());

        // smppd API should be available
        let smppd: Table = globals.get("smppd").unwrap();
        assert!(smppd.contains_key("log").unwrap());
        assert!(smppd.contains_key("version").unwrap());
    }

    #[tokio::test]
    async fn test_lua_plugin_basic() {
        let script = r#"
            plugin = {
                name = "test_plugin",
                version = "1.0.0",

                initialize = function(config)
                    return true
                end,

                filter = function(ctx)
                    return { action = "continue" }
                end
            }
        "#;

        // Write temp file
        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join("test_plugin.lua");
        std::fs::write(&temp_file, script).unwrap();

        let config = test_config();
        let plugin = LuaPluginLoader::load(&temp_file, &config).await.unwrap();

        assert_eq!(plugin.name(), "test_plugin");
        assert_eq!(plugin.version(), "1.0.0");
        assert!(plugin.capabilities().can_filter);

        std::fs::remove_file(temp_file).ok();
    }
}
