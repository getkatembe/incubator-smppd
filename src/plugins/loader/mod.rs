//! Plugin loaders for different plugin types.
//!
//! Each loader is responsible for:
//! - Loading plugin code from disk/network
//! - Creating a sandboxed execution environment
//! - Bridging between Rust and the plugin runtime

#[cfg(feature = "lua")]
pub mod lua;

pub mod webhook;

#[cfg(feature = "wasm")]
pub mod wasm;

#[cfg(feature = "native")]
pub mod native;
