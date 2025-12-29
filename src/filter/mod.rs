//! Filter chain module using Tower middleware.
//!
//! Filters process SMPP requests before routing:
//! - Authentication
//! - Rate limiting
//! - Firewall rules
//! - Message transformation
//! - Consent/opt-out checking
//! - Multi-tenant ESME management

mod auth;
mod chain;
mod esme;
mod firewall;
mod ratelimit;
mod request;

pub use auth::{
    AuthFilter, AuthLayer, ConfigCredentialStore, CredentialStore, Credentials,
    InMemoryCredentialStore, OpenCredentialStore,
};
pub use chain::{FilterChain, FilterChainBuilder};
pub use esme::{
    AuthResult, Esme, EsmeCredentials, EsmeFirewall, EsmeManager,
    EsmeQuota, EsmeRateLimit, EsmeRouting, EsmeStatus, EsmeStore,
    EsmeUsageStats, CountryPricing, MemoryEsmeStore, PricingTier,
};
pub use firewall::{
    EsmeFirewallSettings, Firewall, FirewallAction, FirewallConfig, FirewallEvent,
    FirewallEventLog, FirewallFilter, FirewallLayer, FirewallResult, FirewallRule,
    ProtocolParams, RuleType, TimeWindow,
};
pub use ratelimit::{RateLimitConfig, RateLimitFilter, RateLimitLayer};
pub use request::{SmppRequest, SmppResponse};
