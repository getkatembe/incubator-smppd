//! Enterprise SMS Firewall.
//!
//! nftables-inspired rule engine for SMS message filtering with:
//! - Tables, Chains, Rules hierarchy
//! - Composable conditions (And, Or, Not)
//! - Rate limiting, time-based rules
//! - Content inspection, regex matching
//! - External lists (file, HTTP, Redis)
//! - YAML configuration
//!
//! # Example Configuration
//!
//! ```yaml
//! firewall:
//!   enabled: true
//!   default_policy: accept
//!
//!   lists:
//!     blocked_senders:
//!       type: file
//!       path: /etc/smppd/lists/blocked.txt
//!       reload_interval: 60s
//!
//!   tables:
//!     - name: filter
//!       chains:
//!         - name: input
//!           type: input
//!           priority: 0
//!           policy: accept
//!           rules:
//!             - name: rate_limit
//!               conditions:
//!                 - type: rate
//!                   key: system_id
//!                   window: 1s
//!                   threshold: 1000
//!               action:
//!                 type: reject
//!                 status: 88
//!
//!             - name: block_spam
//!               conditions:
//!                 - type: content
//!                   match: keywords
//!                   list: spam_keywords
//!               action:
//!                 type: drop
//! ```

mod context;
mod engine;
mod types;

pub use context::{EvaluationContext, EvaluationResult};
pub use engine::{CompiledFirewall, FirewallStats};
pub use types::*;
