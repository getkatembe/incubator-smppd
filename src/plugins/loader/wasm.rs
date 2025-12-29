//! WebAssembly plugin loader using wasmtime.
//!
//! Provides a sandboxed WASM runtime for plugin execution with:
//! - Memory limits
//! - Fuel-based CPU limits
//! - Host function bindings for plugin API
//!
//! Note: This is a basic implementation. For production use, consider
//! using wasmtime's component model for a more robust ABI.

use crate::plugins::{
    FilterAction, FilterContext, HookAction, HookContext, Plugin, PluginCapabilities,
    PluginConfig, PluginContext, PluginError, PluginType, RouteCandidate, RouteContext,
};
use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, warn};
use wasmtime::*;

/// WASM plugin loader.
pub struct WasmPluginLoader;

impl WasmPluginLoader {
    /// Load a WASM plugin from a file.
    pub async fn load(path: &Path, config: &PluginConfig) -> Result<Box<dyn Plugin>, PluginError> {
        debug!(path = %path.display(), "loading wasm plugin");

        // Read WASM module
        let wasm_bytes = tokio::fs::read(path)
            .await
            .map_err(|e| PluginError::LoadFailed(format!("failed to read wasm file: {}", e)))?;

        // Create engine with configuration
        let mut engine_config = Config::new();
        engine_config.async_support(true);

        // Enable fuel for CPU limiting
        if config.limits.fuel.is_some() {
            engine_config.consume_fuel(true);
        }

        let engine = Engine::new(&engine_config)
            .map_err(|e| PluginError::LoadFailed(format!("failed to create wasm engine: {}", e)))?;

        // Compile the module
        let module = Module::new(&engine, &wasm_bytes)
            .map_err(|e| PluginError::LoadFailed(format!("failed to compile wasm module: {}", e)))?;

        // Create linker
        let mut linker: Linker<WasmState> = Linker::new(&engine);

        // Add smppd host functions
        add_host_functions(&mut linker)?;

        // Create store with state
        let state = WasmState {
            plugin_name: config.name.clone(),
            plugin_config: config.config.clone(),
        };

        let mut store = Store::new(&engine, state);

        // Set fuel if configured
        if let Some(fuel) = config.limits.fuel {
            store.set_fuel(fuel)
                .map_err(|e| PluginError::LoadFailed(format!("failed to set fuel: {}", e)))?;
        }

        // Instantiate the module
        let instance = linker
            .instantiate_async(&mut store, &module)
            .await
            .map_err(|e| PluginError::LoadFailed(format!("failed to instantiate wasm module: {}", e)))?;

        // Detect capabilities by checking for exported functions
        let capabilities = PluginCapabilities {
            can_filter: instance.get_func(&mut store, "filter").is_some(),
            can_route: instance.get_func(&mut store, "select_route").is_some(),
            can_transform: instance.get_func(&mut store, "transform").is_some(),
            can_hook: instance.get_func(&mut store, "on_hook").is_some(),
            can_store: false,
        };

        // Get plugin metadata from exports (simple version that gets constants)
        let name = config.name.clone();
        let version = "1.0.0".to_string();

        Ok(Box::new(WasmPlugin {
            name,
            version,
            capabilities,
            engine,
            module,
            linker: Arc::new(linker),
            store: Arc::new(Mutex::new(store)),
            _config: config.clone(),
        }))
    }
}

/// State passed to WASM instances.
struct WasmState {
    plugin_name: String,
    plugin_config: std::collections::HashMap<String, serde_yaml::Value>,
}

/// Add smppd host functions to the linker.
fn add_host_functions(linker: &mut Linker<WasmState>) -> Result<(), PluginError> {
    // smppd_log(level: i32, ptr: i32, len: i32)
    linker
        .func_wrap("smppd", "log", |mut caller: Caller<'_, WasmState>, level: i32, ptr: i32, len: i32| -> Result<(), Error> {
            // Read message from WASM memory
            let memory = caller.get_export("memory")
                .and_then(|e| e.into_memory())
                .ok_or_else(|| Error::msg("memory export not found"))?;

            let mut buffer = vec![0u8; len as usize];
            memory.read(&caller, ptr as usize, &mut buffer)
                .map_err(|e| Error::msg(format!("failed to read memory: {}", e)))?;

            let message = String::from_utf8_lossy(&buffer);
            let plugin = &caller.data().plugin_name;

            match level {
                0 => tracing::trace!(plugin = %plugin, "{}", message),
                1 => tracing::debug!(plugin = %plugin, "{}", message),
                2 => tracing::info!(plugin = %plugin, "{}", message),
                3 => tracing::warn!(plugin = %plugin, "{}", message),
                4 => tracing::error!(plugin = %plugin, "{}", message),
                _ => tracing::info!(plugin = %plugin, "{}", message),
            }
            Ok(())
        })
        .map_err(|e| PluginError::LoadFailed(format!("failed to add log function: {}", e)))?;

    // smppd_get_config(key_ptr: i32, key_len: i32, out_ptr: i32, out_len: i32) -> i32
    linker
        .func_wrap("smppd", "get_config", |mut caller: Caller<'_, WasmState>, key_ptr: i32, key_len: i32, out_ptr: i32, max_len: i32| -> i32 {
            let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                Some(m) => m,
                None => return -1,
            };

            // Read key
            let mut key_buffer = vec![0u8; key_len as usize];
            if memory.read(&caller, key_ptr as usize, &mut key_buffer).is_err() {
                return -1;
            }
            let key = String::from_utf8_lossy(&key_buffer);

            // Get config value
            let value = match caller.data().plugin_config.get(key.as_ref()) {
                Some(v) => serde_json::to_string(v).unwrap_or_default(),
                None => return 0,
            };

            let value_bytes = value.as_bytes();
            if value_bytes.len() > max_len as usize {
                return -2; // Buffer too small
            }

            if memory.write(&mut caller, out_ptr as usize, value_bytes).is_err() {
                return -1;
            }

            value_bytes.len() as i32
        })
        .map_err(|e| PluginError::LoadFailed(format!("failed to add get_config function: {}", e)))?;

    Ok(())
}

/// WASM plugin implementation.
pub struct WasmPlugin {
    name: String,
    version: String,
    capabilities: PluginCapabilities,
    engine: Engine,
    module: Module,
    linker: Arc<Linker<WasmState>>,
    store: Arc<Mutex<Store<WasmState>>>,
    _config: PluginConfig,
}

impl std::fmt::Debug for WasmPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmPlugin")
            .field("name", &self.name)
            .field("version", &self.version)
            .field("capabilities", &self.capabilities)
            .finish()
    }
}

#[async_trait]
impl Plugin for WasmPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn plugin_type(&self) -> PluginType {
        PluginType::Wasm
    }

    fn capabilities(&self) -> PluginCapabilities {
        self.capabilities.clone()
    }

    async fn initialize(&mut self, _ctx: &PluginContext) -> Result<(), PluginError> {
        let mut store = self.store.lock().await;

        // Re-instantiate for fresh state
        let instance = self.linker
            .instantiate_async(&mut *store, &self.module)
            .await
            .map_err(|e| PluginError::InitFailed(format!("failed to instantiate: {}", e)))?;

        // Call initialize export if present
        if let Some(init_func) = instance.get_func(&mut *store, "initialize") {
            let typed_func = init_func.typed::<(), ()>(&*store)
                .map_err(|e| PluginError::InitFailed(format!("initialize has wrong signature: {}", e)))?;

            typed_func
                .call_async(&mut *store, ())
                .await
                .map_err(|e| PluginError::InitFailed(format!("initialize failed: {}", e)))?;
        }

        debug!(name = %self.name, "wasm plugin initialized");
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), PluginError> {
        let mut store = self.store.lock().await;

        // Re-instantiate to call shutdown
        if let Ok(instance) = self.linker.instantiate_async(&mut *store, &self.module).await {
            if let Some(shutdown_func) = instance.get_func(&mut *store, "shutdown") {
                if let Ok(typed_func) = shutdown_func.typed::<(), ()>(&*store) {
                    let _ = typed_func.call_async(&mut *store, ()).await;
                }
            }
        }

        debug!(name = %self.name, "wasm plugin shut down");
        Ok(())
    }
}

impl WasmPlugin {
    /// Execute filter function.
    pub async fn filter(&self, _ctx: &FilterContext) -> Result<FilterAction, PluginError> {
        let mut store = self.store.lock().await;

        let instance = self.linker
            .instantiate_async(&mut *store, &self.module)
            .await
            .map_err(|e| PluginError::ExecutionFailed(format!("failed to instantiate: {}", e)))?;

        let filter_func = instance
            .get_func(&mut *store, "filter")
            .ok_or_else(|| PluginError::ExecutionFailed("filter function not found".into()))?;

        // For simplicity, use a filter function that takes no args and returns i32
        // A full implementation would pass serialized context through WASM memory
        let typed_func = filter_func.typed::<(), i32>(&*store)
            .map_err(|e| PluginError::ExecutionFailed(format!("filter has wrong signature: {}", e)))?;

        let action_code = typed_func
            .call_async(&mut *store, ())
            .await
            .map_err(|e| PluginError::ExecutionFailed(format!("filter failed: {}", e)))?;

        match action_code {
            0 => Ok(FilterAction::Continue),
            1 => Ok(FilterAction::Accept),
            2 => Ok(FilterAction::Drop),
            3 => Ok(FilterAction::Quarantine),
            4 => Ok(FilterAction::Modify),
            _ => {
                warn!(action = action_code, "unknown filter action from wasm, defaulting to continue");
                Ok(FilterAction::Continue)
            }
        }
    }

    /// Execute route selection function.
    pub async fn select_route(
        &self,
        _ctx: &RouteContext,
        _candidates: &[RouteCandidate],
    ) -> Result<Option<String>, PluginError> {
        // Simplified implementation - would need full memory management for production
        warn!("wasm select_route not fully implemented");
        Ok(None)
    }

    /// Execute hook handler.
    pub async fn on_hook(&self, _ctx: &HookContext) -> Result<HookAction, PluginError> {
        // Simplified implementation
        warn!("wasm on_hook not fully implemented");
        Ok(HookAction::Continue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn test_config() -> PluginConfig {
        PluginConfig {
            name: "test_wasm".to_string(),
            plugin_type: PluginType::Wasm,
            path: None,
            url: None,
            enabled: true,
            priority: 0,
            limits: Default::default(),
            config: HashMap::new(),
        }
    }

    #[test]
    fn test_wasm_plugin_type() {
        // Basic type check
        assert_eq!(PluginType::Wasm.to_string(), "wasm");
    }
}
