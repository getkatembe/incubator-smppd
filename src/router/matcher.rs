//! Route matchers for destination/source matching.

use regex::Regex;
use std::sync::Arc;

use crate::config::{MatchCondition, TimeWindow};

/// Type of matcher
#[derive(Debug, Clone)]
pub enum MatcherKind {
    /// Always matches (catch-all)
    Any,
    /// Prefix match
    Prefix(String),
    /// Regex match
    Regex(String),
    /// Exact match
    Exact(String),
}

impl MatcherKind {
    /// Create from a MatchCondition config.
    pub fn from_condition(cond: &MatchCondition) -> Option<Self> {
        if cond.any == Some(true) {
            Some(MatcherKind::Any)
        } else if let Some(ref exact) = cond.exact {
            Some(MatcherKind::Exact(exact.clone()))
        } else if let Some(ref regex) = cond.regex {
            Some(MatcherKind::Regex(regex.clone()))
        } else if let Some(ref prefix) = cond.prefix {
            if prefix.is_empty() {
                Some(MatcherKind::Any)
            } else {
                Some(MatcherKind::Prefix(prefix.clone()))
            }
        } else {
            None
        }
    }
}

/// A compiled matcher for efficient matching.
#[derive(Clone)]
pub struct CompiledMatcher {
    kind: MatcherKind,
    regex: Option<Arc<Regex>>,
}

impl std::fmt::Debug for CompiledMatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledMatcher")
            .field("kind", &self.kind)
            .finish()
    }
}

impl CompiledMatcher {
    /// Create a new compiled matcher.
    pub fn new(kind: MatcherKind) -> Result<Self, regex::Error> {
        let regex = match &kind {
            MatcherKind::Regex(pattern) => Some(Arc::new(Regex::new(pattern)?)),
            _ => None,
        };

        Ok(Self { kind, regex })
    }

    /// Create from a MatchCondition config.
    pub fn from_condition(cond: &MatchCondition) -> Result<Option<Self>, regex::Error> {
        match MatcherKind::from_condition(cond) {
            Some(kind) => Ok(Some(Self::new(kind)?)),
            None => Ok(None),
        }
    }

    /// Check if value matches.
    pub fn matches(&self, value: &str) -> bool {
        match &self.kind {
            MatcherKind::Any => true,
            MatcherKind::Prefix(prefix) => value.starts_with(prefix),
            MatcherKind::Regex(_) => self.regex.as_ref().map(|r| r.is_match(value)).unwrap_or(false),
            MatcherKind::Exact(exact) => value == exact,
        }
    }

    /// Get the kind of matcher.
    pub fn kind(&self) -> &MatcherKind {
        &self.kind
    }
}

/// Context for route matching with all available message fields.
#[derive(Debug, Clone, Default)]
pub struct MatchContext<'a> {
    /// Destination address
    pub destination: &'a str,
    /// Source address
    pub source: Option<&'a str>,
    /// Sender ID
    pub sender_id: Option<&'a str>,
    /// ESME system_id
    pub system_id: Option<&'a str>,
    /// SMPP service_type
    pub service_type: Option<&'a str>,
    /// SMPP ESM class
    pub esm_class: Option<u8>,
    /// SMPP data coding
    pub data_coding: Option<u8>,
}

impl<'a> MatchContext<'a> {
    /// Create a new match context with just destination.
    pub fn new(destination: &'a str) -> Self {
        Self {
            destination,
            ..Default::default()
        }
    }

    /// Set source address.
    pub fn with_source(mut self, source: &'a str) -> Self {
        self.source = Some(source);
        self
    }

    /// Set sender ID.
    pub fn with_sender_id(mut self, sender_id: &'a str) -> Self {
        self.sender_id = Some(sender_id);
        self
    }

    /// Set system ID.
    pub fn with_system_id(mut self, system_id: &'a str) -> Self {
        self.system_id = Some(system_id);
        self
    }

    /// Set service type.
    pub fn with_service_type(mut self, service_type: &'a str) -> Self {
        self.service_type = Some(service_type);
        self
    }

    /// Set ESM class.
    pub fn with_esm_class(mut self, esm_class: u8) -> Self {
        self.esm_class = Some(esm_class);
        self
    }

    /// Set data coding.
    pub fn with_data_coding(mut self, data_coding: u8) -> Self {
        self.data_coding = Some(data_coding);
        self
    }
}

/// A route matcher with multiple conditions.
#[derive(Debug, Clone)]
pub struct Matcher {
    /// Destination matcher
    pub destination: Option<CompiledMatcher>,
    /// Source matcher
    pub source: Option<CompiledMatcher>,
    /// Sender ID matcher
    pub sender_id: Option<CompiledMatcher>,
    /// System ID (exact match)
    pub system_id: Option<String>,
    /// Service type (exact match)
    pub service_type: Option<String>,
    /// ESM class (exact match)
    pub esm_class: Option<u8>,
    /// Data coding (any of listed values)
    pub data_coding: Option<Vec<u8>>,
    /// Time window
    pub time_window: Option<TimeWindow>,
}

impl Matcher {
    /// Create a new matcher builder.
    pub fn builder() -> MatcherBuilder {
        MatcherBuilder::default()
    }

    /// Check if all conditions match (legacy interface).
    pub fn matches(&self, dest: &str, source: Option<&str>, sender_id: Option<&str>) -> bool {
        let ctx = MatchContext {
            destination: dest,
            source,
            sender_id,
            ..Default::default()
        };
        self.matches_context(&ctx)
    }

    /// Check if all conditions match with full context.
    pub fn matches_context(&self, ctx: &MatchContext<'_>) -> bool {
        // Check destination
        if let Some(ref m) = self.destination {
            if !m.matches(ctx.destination) {
                return false;
            }
        }

        // Check source
        if let Some(ref m) = self.source {
            if let Some(src) = ctx.source {
                if !m.matches(src) {
                    return false;
                }
            } else {
                return false;
            }
        }

        // Check sender ID
        if let Some(ref m) = self.sender_id {
            if let Some(sid) = ctx.sender_id {
                if !m.matches(sid) {
                    return false;
                }
            } else {
                return false;
            }
        }

        // Check system ID (exact match)
        if let Some(ref expected) = self.system_id {
            if let Some(actual) = ctx.system_id {
                if actual != expected {
                    return false;
                }
            } else {
                return false;
            }
        }

        // Check service type (exact match)
        if let Some(ref expected) = self.service_type {
            if let Some(actual) = ctx.service_type {
                if actual != expected {
                    return false;
                }
            } else {
                return false;
            }
        }

        // Check ESM class
        if let Some(expected) = self.esm_class {
            if let Some(actual) = ctx.esm_class {
                if actual != expected {
                    return false;
                }
            } else {
                return false;
            }
        }

        // Check data coding (any of listed values)
        if let Some(ref allowed) = self.data_coding {
            if let Some(actual) = ctx.data_coding {
                if !allowed.contains(&actual) {
                    return false;
                }
            } else {
                return false;
            }
        }

        // Check time window
        if let Some(ref window) = self.time_window {
            if !time_window_matches(window) {
                return false;
            }
        }

        true
    }
}

/// Check if current time is within the time window.
fn time_window_matches(window: &TimeWindow) -> bool {
    use chrono::{Timelike, Weekday, Datelike};

    // Parse timezone (default to UTC if invalid)
    let tz: chrono_tz::Tz = window.timezone.parse().unwrap_or(chrono_tz::UTC);
    let now = chrono::Utc::now().with_timezone(&tz);

    // Check day of week
    let day_name = match now.weekday() {
        Weekday::Mon => "Mon",
        Weekday::Tue => "Tue",
        Weekday::Wed => "Wed",
        Weekday::Thu => "Thu",
        Weekday::Fri => "Fri",
        Weekday::Sat => "Sat",
        Weekday::Sun => "Sun",
    };

    if !window.days.iter().any(|d| d.eq_ignore_ascii_case(day_name)) {
        return false;
    }

    // Parse start/end times
    let parse_time = |s: &str| -> Option<(u32, u32)> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 2 {
            return None;
        }
        let h = parts[0].parse().ok()?;
        let m = parts[1].parse().ok()?;
        Some((h, m))
    };

    let (start_h, start_m) = match parse_time(&window.start) {
        Some(t) => t,
        None => return false,
    };

    let (end_h, end_m) = match parse_time(&window.end) {
        Some(t) => t,
        None => return false,
    };

    let current_mins = now.hour() * 60 + now.minute();
    let start_mins = start_h * 60 + start_m;
    let end_mins = end_h * 60 + end_m;

    // Handle overnight windows (e.g., 22:00 - 06:00)
    if start_mins <= end_mins {
        // Normal window (e.g., 09:00 - 17:00)
        current_mins >= start_mins && current_mins < end_mins
    } else {
        // Overnight window (e.g., 22:00 - 06:00)
        current_mins >= start_mins || current_mins < end_mins
    }
}

/// Builder for Matcher.
#[derive(Default)]
pub struct MatcherBuilder {
    destination: Option<CompiledMatcher>,
    source: Option<CompiledMatcher>,
    sender_id: Option<CompiledMatcher>,
    system_id: Option<String>,
    service_type: Option<String>,
    esm_class: Option<u8>,
    data_coding: Option<Vec<u8>>,
    time_window: Option<TimeWindow>,
}

impl MatcherBuilder {
    /// Set destination matcher.
    pub fn destination(mut self, kind: MatcherKind) -> Result<Self, regex::Error> {
        self.destination = Some(CompiledMatcher::new(kind)?);
        Ok(self)
    }

    /// Set source matcher.
    pub fn source(mut self, kind: MatcherKind) -> Result<Self, regex::Error> {
        self.source = Some(CompiledMatcher::new(kind)?);
        Ok(self)
    }

    /// Set sender ID matcher.
    pub fn sender_id(mut self, kind: MatcherKind) -> Result<Self, regex::Error> {
        self.sender_id = Some(CompiledMatcher::new(kind)?);
        Ok(self)
    }

    /// Set system ID (exact match).
    pub fn system_id(mut self, id: impl Into<String>) -> Self {
        self.system_id = Some(id.into());
        self
    }

    /// Set service type (exact match).
    pub fn service_type(mut self, st: impl Into<String>) -> Self {
        self.service_type = Some(st.into());
        self
    }

    /// Set ESM class.
    pub fn esm_class(mut self, esm: u8) -> Self {
        self.esm_class = Some(esm);
        self
    }

    /// Set allowed data coding values.
    pub fn data_coding(mut self, dc: Vec<u8>) -> Self {
        self.data_coding = Some(dc);
        self
    }

    /// Set time window.
    pub fn time_window(mut self, tw: TimeWindow) -> Self {
        self.time_window = Some(tw);
        self
    }

    /// Build the matcher.
    pub fn build(self) -> Matcher {
        Matcher {
            destination: self.destination,
            source: self.source,
            sender_id: self.sender_id,
            system_id: self.system_id,
            service_type: self.service_type,
            esm_class: self.esm_class,
            data_coding: self.data_coding,
            time_window: self.time_window,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============================================================================
    // CompiledMatcher Tests
    // ============================================================================

    #[test]
    fn test_prefix_matcher() {
        let m = CompiledMatcher::new(MatcherKind::Prefix("+258".to_string())).unwrap();
        assert!(m.matches("+258841234567"));
        assert!(m.matches("+258821234567"));
        assert!(m.matches("+258861234567"));
        assert!(!m.matches("+27841234567")); // South Africa
        assert!(!m.matches("+1234567890")); // US
        assert!(!m.matches("841234567")); // No country code
    }

    #[test]
    fn test_regex_matcher() {
        let m = CompiledMatcher::new(MatcherKind::Regex(r"^\+258(84|82|86)".to_string())).unwrap();
        assert!(m.matches("+258841234567"));
        assert!(m.matches("+258821234567"));
        assert!(m.matches("+258861234567"));
        assert!(!m.matches("+258871234567"));
    }

    #[test]
    fn test_regex_mobile_operators() {
        // Match Vodacom (84), mCel (82), Movitel (86, 87)
        let m = CompiledMatcher::new(MatcherKind::Regex(r"^\+258(84|82|86|87)\d{7}$".to_string())).unwrap();

        // Valid mobile numbers
        assert!(m.matches("+258841234567"));
        assert!(m.matches("+258821234567"));
        assert!(m.matches("+258861234567"));
        assert!(m.matches("+258871234567"));

        // Invalid: wrong prefix or length
        assert!(!m.matches("+258211234567")); // Landline prefix
        assert!(!m.matches("+2588412345"));   // Too short
        assert!(!m.matches("+25884123456789")); // Too long
        assert!(!m.matches("+27841234567"));  // Wrong country
    }

    #[test]
    fn test_any_matcher() {
        let m = CompiledMatcher::new(MatcherKind::Any).unwrap();
        assert!(m.matches("anything"));
        assert!(m.matches(""));
        assert!(m.matches("+258841234567"));
    }

    #[test]
    fn test_exact_matcher() {
        let m = CompiledMatcher::new(MatcherKind::Exact("12345".to_string())).unwrap();
        assert!(m.matches("12345"));
        assert!(!m.matches("123456"));
        assert!(!m.matches("1234"));
        assert!(!m.matches("12345 ")); // Trailing space
    }

    #[test]
    fn test_matcher_kind_accessor() {
        let m = CompiledMatcher::new(MatcherKind::Prefix("+258".to_string())).unwrap();
        assert!(matches!(m.kind(), MatcherKind::Prefix(_)));
    }

    // ============================================================================
    // Matcher Builder Tests
    // ============================================================================

    #[test]
    fn test_compound_matcher() {
        let matcher = Matcher::builder()
            .destination(MatcherKind::Prefix("+258".to_string())).unwrap()
            .source(MatcherKind::Prefix("esme_".to_string())).unwrap()
            .build();

        assert!(matcher.matches("+258841234567", Some("esme_123"), None));
        assert!(!matcher.matches("+258841234567", Some("other_123"), None));
        assert!(!matcher.matches("+27841234567", Some("esme_123"), None));
    }

    #[test]
    fn test_matcher_with_source_filter() {
        let matcher = Matcher::builder()
            .destination(MatcherKind::Prefix("+258".to_string())).unwrap()
            .source(MatcherKind::Prefix("premium_".to_string())).unwrap()
            .build();

        // Both destination and source must match
        assert!(matcher.matches("+258841234567", Some("premium_client"), None));
        assert!(matcher.matches("+258841234567", Some("premium_gold"), None));

        // Wrong source
        assert!(!matcher.matches("+258841234567", Some("basic_client"), None));
        // No source when required
        assert!(!matcher.matches("+258841234567", None, None));
        // Wrong destination
        assert!(!matcher.matches("+27841234567", Some("premium_client"), None));
    }

    #[test]
    fn test_matcher_with_sender_id_filter() {
        let matcher = Matcher::builder()
            .destination(MatcherKind::Prefix("+258".to_string())).unwrap()
            .sender_id(MatcherKind::Exact("KATEMBE".to_string())).unwrap()
            .build();

        assert!(matcher.matches("+258841234567", None, Some("KATEMBE")));
        assert!(!matcher.matches("+258841234567", None, Some("OTHER")));
        assert!(!matcher.matches("+258841234567", None, None));
    }

    #[test]
    fn test_matcher_compound_all_conditions() {
        let matcher = Matcher::builder()
            .destination(MatcherKind::Prefix("+258".to_string())).unwrap()
            .source(MatcherKind::Prefix("client_".to_string())).unwrap()
            .sender_id(MatcherKind::Regex("^[A-Z]{3,10}$".to_string())).unwrap()
            .build();

        // All conditions met
        assert!(matcher.matches("+258841234567", Some("client_123"), Some("KATEMBE")));
        assert!(matcher.matches("+258841234567", Some("client_abc"), Some("SMS")));

        // One condition fails
        assert!(!matcher.matches("+27841234567", Some("client_123"), Some("KATEMBE")));
        assert!(!matcher.matches("+258841234567", Some("other_123"), Some("KATEMBE")));
        assert!(!matcher.matches("+258841234567", Some("client_123"), Some("123"))); // Numeric sender
    }

    #[test]
    fn test_matcher_destination_only() {
        let matcher = Matcher::builder()
            .destination(MatcherKind::Prefix("+258".to_string())).unwrap()
            .build();

        // Should match any source/sender_id
        assert!(matcher.matches("+258841234567", None, None));
        assert!(matcher.matches("+258841234567", Some("any_source"), None));
        assert!(matcher.matches("+258841234567", None, Some("ANY_SENDER")));
        assert!(matcher.matches("+258841234567", Some("any"), Some("ANY")));
    }

    #[test]
    fn test_matcher_no_conditions() {
        let matcher = Matcher::builder().build();

        // No conditions = match anything
        assert!(matcher.matches("anything", None, None));
        assert!(matcher.matches("", Some("source"), Some("sender")));
    }

    // ============================================================================
    // Compiled Matcher Debug Tests
    // ============================================================================

    #[test]
    fn test_compiled_matcher_debug() {
        let m = CompiledMatcher::new(MatcherKind::Prefix("+258".to_string())).unwrap();
        let debug = format!("{:?}", m);
        assert!(debug.contains("CompiledMatcher"));
        assert!(debug.contains("Prefix"));
    }

    #[test]
    fn test_matcher_debug() {
        let matcher = Matcher::builder()
            .destination(MatcherKind::Any).unwrap()
            .build();
        let debug = format!("{:?}", matcher);
        assert!(debug.contains("Matcher"));
    }
}
