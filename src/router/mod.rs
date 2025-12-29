//! Message routing module.
//!
//! Routes messages to appropriate clusters based on:
//! - Destination prefix/regex
//! - Source address
//! - Sender ID
//! - System ID (bound ESME)
//! - Service type
//! - ESM class (binary/flash SMS)
//! - Data coding
//! - Time window
//! - Cost/pricing
//! - Custom Lua rules

mod matcher;
mod pricing;
mod router;

pub use matcher::{CompiledMatcher, MatchContext, Matcher, MatcherKind};
pub use pricing::{
    ClusterPricing, CostRouter, CostRoutingStrategy, PricingKey, PricingStore,
    RouteCandidate, RoutePricing,
};
pub use router::{RouteResult, Router, RouterError};
