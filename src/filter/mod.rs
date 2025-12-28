//! Filter chain module using Tower middleware.
//!
//! Filters process SMPP requests before routing:
//! - Authentication
//! - Rate limiting
//! - Firewall rules
//! - Message transformation
//! - Consent/opt-out checking

mod auth;
mod chain;
mod firewall;
mod ratelimit;
mod request;

pub use auth::{AuthFilter, AuthLayer, Credentials};
pub use chain::{FilterChain, FilterChainBuilder};
pub use firewall::{
    Firewall, FirewallAction, FirewallConfig, FirewallFilter, FirewallLayer,
    FirewallResult, FirewallRule, RuleType,
};
pub use ratelimit::{RateLimitFilter, RateLimitLayer, RateLimitConfig};
pub use request::{SmppRequest, SmppResponse};
