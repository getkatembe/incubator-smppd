//! Native shared library plugin loader using libloading.
//!
//! Loads native plugins compiled as shared libraries (.so on Linux, .dylib on macOS, .dll on Windows).
//!
//! # Plugin ABI
//!
//! Plugins must export the following C functions:
//!
//! ```c
//! // Required
//! const char* plugin_name();
//! const char* plugin_version();
//! int plugin_initialize(const char* config_json);
//! void plugin_shutdown();
//!
//! // Optional (filter plugins)
//! int plugin_filter(const char* context_json, char* result_json, int result_max_len);
//!
//! // Optional (route plugins)
//! int plugin_select_route(const char* context_json, const char* candidates_json,
//!                         char* result, int result_max_len);
//!
//! // Optional (hook plugins)
//! int plugin_on_hook(const char* context_json);
//! ```

use crate::plugins::{
    FilterAction, FilterContext, HookAction, HookContext, Plugin, PluginCapabilities,
    PluginConfig, PluginContext, PluginError, PluginType, RouteCandidate, RouteContext,
};
use async_trait::async_trait;
use libloading::{Library, Symbol};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, warn};

/// Native plugin loader.
pub struct NativePluginLoader;

impl NativePluginLoader {
    /// Load a native plugin from a shared library.
    pub async fn load(path: &Path, config: &PluginConfig) -> Result<Box<dyn Plugin>, PluginError> {
        debug!(path = %path.display(), "loading native plugin");

        // Safety: Loading a dynamic library is inherently unsafe.
        // We trust that the plugin follows the expected ABI.
        let library = unsafe {
            Library::new(path)
                .map_err(|e| PluginError::LoadFailed(format!("failed to load library: {}", e)))?
        };

        let library = Arc::new(library);

        // Get plugin metadata
        let name = get_plugin_string(&library, "plugin_name")
            .unwrap_or_else(|| config.name.clone());
        let version = get_plugin_string(&library, "plugin_version")
            .unwrap_or_else(|| "1.0.0".to_string());

        // Detect capabilities
        let capabilities = PluginCapabilities {
            can_filter: has_symbol(&library, "plugin_filter"),
            can_route: has_symbol(&library, "plugin_select_route"),
            can_transform: has_symbol(&library, "plugin_transform"),
            can_hook: has_symbol(&library, "plugin_on_hook"),
            can_store: false,
        };

        Ok(Box::new(NativePlugin {
            name,
            version,
            capabilities,
            library,
            config: config.clone(),
        }))
    }
}

/// Get a string from a plugin function.
fn get_plugin_string(library: &Library, func_name: &str) -> Option<String> {
    unsafe {
        let func: Symbol<unsafe extern "C" fn() -> *const c_char> =
            library.get(func_name.as_bytes()).ok()?;

        let ptr = func();
        if ptr.is_null() {
            return None;
        }

        CStr::from_ptr(ptr).to_str().ok().map(|s| s.to_string())
    }
}

/// Check if a symbol exists in the library.
fn has_symbol(library: &Library, name: &str) -> bool {
    unsafe {
        library
            .get::<*const ()>(name.as_bytes())
            .is_ok()
    }
}

/// Native plugin implementation.
pub struct NativePlugin {
    name: String,
    version: String,
    capabilities: PluginCapabilities,
    library: Arc<Library>,
    config: PluginConfig,
}

// Safety: Native plugins are expected to be thread-safe
unsafe impl Send for NativePlugin {}
unsafe impl Sync for NativePlugin {}

impl std::fmt::Debug for NativePlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativePlugin")
            .field("name", &self.name)
            .field("version", &self.version)
            .field("capabilities", &self.capabilities)
            .finish()
    }
}

#[async_trait]
impl Plugin for NativePlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn plugin_type(&self) -> PluginType {
        PluginType::Native
    }

    fn capabilities(&self) -> PluginCapabilities {
        self.capabilities.clone()
    }

    async fn initialize(&mut self, ctx: &PluginContext) -> Result<(), PluginError> {
        // Serialize config to JSON
        let config_json = serde_json::to_string(&ctx.config)
            .map_err(|e| PluginError::InitFailed(format!("failed to serialize config: {}", e)))?;

        let config_cstr = CString::new(config_json)
            .map_err(|e| PluginError::InitFailed(format!("invalid config string: {}", e)))?;

        // Call plugin_initialize
        let result = unsafe {
            let func: Symbol<unsafe extern "C" fn(*const c_char) -> i32> = self
                .library
                .get(b"plugin_initialize")
                .map_err(|_| PluginError::InitFailed("plugin_initialize not found".into()))?;

            func(config_cstr.as_ptr())
        };

        if result != 0 {
            return Err(PluginError::InitFailed(format!(
                "plugin_initialize returned error code: {}",
                result
            )));
        }

        debug!(name = %self.name, "native plugin initialized");
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), PluginError> {
        // Call plugin_shutdown if it exists
        unsafe {
            if let Ok(func) = self
                .library
                .get::<unsafe extern "C" fn()>(b"plugin_shutdown")
            {
                func();
            }
        }

        debug!(name = %self.name, "native plugin shut down");
        Ok(())
    }
}

impl NativePlugin {
    /// Execute filter function.
    pub async fn filter(&self, ctx: &FilterContext) -> Result<FilterAction, PluginError> {
        // Serialize context to JSON
        let ctx_json = serde_json::to_string(ctx)
            .map_err(|e| PluginError::ExecutionFailed(format!("failed to serialize context: {}", e)))?;

        let ctx_cstr = CString::new(ctx_json)
            .map_err(|e| PluginError::ExecutionFailed(format!("invalid context string: {}", e)))?;

        // Prepare result buffer
        let mut result_buffer = vec![0u8; 4096];

        // Call plugin_filter
        let result_code = unsafe {
            let func: Symbol<unsafe extern "C" fn(*const c_char, *mut c_char, i32) -> i32> = self
                .library
                .get(b"plugin_filter")
                .map_err(|_| PluginError::ExecutionFailed("plugin_filter not found".into()))?;

            func(
                ctx_cstr.as_ptr(),
                result_buffer.as_mut_ptr() as *mut c_char,
                result_buffer.len() as i32,
            )
        };

        // Parse result
        match result_code {
            0 => Ok(FilterAction::Continue),
            1 => Ok(FilterAction::Accept),
            2 => Ok(FilterAction::Drop),
            3 => Ok(FilterAction::Quarantine),
            4 => Ok(FilterAction::Modify),
            100..=199 => {
                // Reject with status code encoded in result
                let status = (result_code - 100) as u32;

                // Try to parse reason from result buffer
                let reason = unsafe {
                    let cstr = CStr::from_ptr(result_buffer.as_ptr() as *const c_char);
                    cstr.to_str().ok().filter(|s| !s.is_empty()).map(|s| s.to_string())
                };

                Ok(FilterAction::Reject { status, reason })
            }
            _ => {
                warn!(result_code, "unknown filter result from native plugin");
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
        // Serialize context and candidates to JSON
        let ctx_json = serde_json::to_string(ctx)
            .map_err(|e| PluginError::ExecutionFailed(format!("failed to serialize context: {}", e)))?;
        let candidates_json = serde_json::to_string(candidates)
            .map_err(|e| PluginError::ExecutionFailed(format!("failed to serialize candidates: {}", e)))?;

        let ctx_cstr = CString::new(ctx_json)
            .map_err(|e| PluginError::ExecutionFailed(format!("invalid context string: {}", e)))?;
        let candidates_cstr = CString::new(candidates_json)
            .map_err(|e| PluginError::ExecutionFailed(format!("invalid candidates string: {}", e)))?;

        // Prepare result buffer
        let mut result_buffer = vec![0u8; 256];

        // Call plugin_select_route
        let result_len = unsafe {
            let func: Symbol<
                unsafe extern "C" fn(*const c_char, *const c_char, *mut c_char, i32) -> i32,
            > = self
                .library
                .get(b"plugin_select_route")
                .map_err(|_| PluginError::ExecutionFailed("plugin_select_route not found".into()))?;

            func(
                ctx_cstr.as_ptr(),
                candidates_cstr.as_ptr(),
                result_buffer.as_mut_ptr() as *mut c_char,
                result_buffer.len() as i32,
            )
        };

        if result_len <= 0 {
            return Ok(None);
        }

        // Parse result
        let result = unsafe {
            let cstr = CStr::from_ptr(result_buffer.as_ptr() as *const c_char);
            cstr.to_str().ok().map(|s| s.to_string())
        };

        Ok(result)
    }

    /// Execute hook handler.
    pub async fn on_hook(&self, ctx: &HookContext) -> Result<HookAction, PluginError> {
        // Serialize context to JSON
        let ctx_json = serde_json::to_string(ctx)
            .map_err(|e| PluginError::ExecutionFailed(format!("failed to serialize context: {}", e)))?;

        let ctx_cstr = CString::new(ctx_json)
            .map_err(|e| PluginError::ExecutionFailed(format!("invalid context string: {}", e)))?;

        // Call plugin_on_hook
        let result_code = unsafe {
            let func: Symbol<unsafe extern "C" fn(*const c_char) -> i32> = self
                .library
                .get(b"plugin_on_hook")
                .map_err(|_| PluginError::ExecutionFailed("plugin_on_hook not found".into()))?;

            func(ctx_cstr.as_ptr())
        };

        match result_code {
            0 => Ok(HookAction::Continue),
            1 => Ok(HookAction::Stop),
            2 => Ok(HookAction::SkipPriority),
            _ => {
                warn!(result_code, "unknown hook result from native plugin");
                Ok(HookAction::Continue)
            }
        }
    }
}

impl Drop for NativePlugin {
    fn drop(&mut self) {
        // Try to call shutdown on drop
        unsafe {
            if let Ok(func) = self
                .library
                .get::<unsafe extern "C" fn()>(b"plugin_shutdown")
            {
                func();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn test_config() -> PluginConfig {
        PluginConfig {
            name: "test_native".to_string(),
            plugin_type: PluginType::Native,
            path: None,
            url: None,
            enabled: true,
            priority: 0,
            limits: Default::default(),
            config: HashMap::new(),
        }
    }

    #[test]
    fn test_native_plugin_type() {
        assert_eq!(PluginType::Native.to_string(), "native");
    }

    // Note: Actual loading tests require a compiled .so file
}
