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
//! - Custom Lua rules

mod matcher;
mod router;

pub use matcher::{CompiledMatcher, MatchContext, Matcher, MatcherKind};
pub use router::{RouteResult, Router, RouterError};
