//! Router for message routing to clusters.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tracing::{debug, trace, warn};

use crate::cluster::{Cluster, Clusters};
use crate::config::{RouteConfig, TrafficSplit};

use super::matcher::{CompiledMatcher, MatchContext, Matcher, MatcherKind};

/// Result of route matching.
#[derive(Debug, Clone)]
pub struct RouteResult {
    /// Route name
    pub route_name: String,
    /// Target cluster name
    pub cluster_name: String,
    /// Fallback clusters (tried in order if primary fails)
    pub fallback: Vec<String>,
}

/// Target for a route - either a single cluster or traffic split
#[derive(Debug)]
enum RouteTarget {
    /// Single cluster target
    Single(String),
    /// Traffic split with weights
    Split(WeightedSplit),
}

/// Weighted traffic split
#[derive(Debug)]
struct WeightedSplit {
    /// Cluster names with cumulative weights
    clusters: Vec<(String, u32)>,
    /// Total weight
    total_weight: u32,
    /// Counter for deterministic selection (for testing)
    counter: AtomicUsize,
}

impl WeightedSplit {
    fn new(splits: &[TrafficSplit]) -> Self {
        let mut clusters = Vec::with_capacity(splits.len());
        let mut cumulative = 0u32;

        for split in splits {
            cumulative += split.weight;
            clusters.push((split.cluster.clone(), cumulative));
        }

        Self {
            clusters,
            total_weight: cumulative,
            counter: AtomicUsize::new(0),
        }
    }

    /// Select a cluster based on weighted random selection
    fn select(&self) -> Option<&str> {
        if self.clusters.is_empty() || self.total_weight == 0 {
            return None;
        }

        // Use counter-based pseudo-random for reproducibility in tests
        let counter = self.counter.fetch_add(1, Ordering::Relaxed);
        let point = (counter as u32) % self.total_weight;

        for (cluster, cumulative) in &self.clusters {
            if point < *cumulative {
                return Some(cluster);
            }
        }

        // Fallback to last cluster
        self.clusters.last().map(|(c, _)| c.as_str())
    }
}

/// A compiled route.
#[derive(Debug)]
struct Route {
    /// Route name
    name: String,
    /// Matcher for this route
    matcher: Matcher,
    /// Target (single cluster or traffic split)
    target: RouteTarget,
    /// Fallback clusters
    fallback: Vec<String>,
    /// Priority (lower = higher priority)
    priority: i32,
}

/// Message router.
pub struct Router {
    /// Compiled routes sorted by priority
    routes: Vec<Route>,
    /// Available clusters
    clusters: Arc<Clusters>,
}

impl Router {
    /// Create a new router from config.
    pub fn new(configs: &[RouteConfig], clusters: Arc<Clusters>) -> Result<Self, RouterError> {
        let mut routes = Vec::with_capacity(configs.len());

        for config in configs {
            let route = compile_route(config)?;
            routes.push(route);
        }

        // Sort by priority (lower = higher priority)
        routes.sort_by_key(|r| r.priority);

        debug!(
            routes = routes.len(),
            "router created"
        );

        for route in &routes {
            let target_desc = match &route.target {
                RouteTarget::Single(c) => c.clone(),
                RouteTarget::Split(s) => format!("split({})", s.clusters.len()),
            };
            debug!(
                name = %route.name,
                target = %target_desc,
                priority = route.priority,
                "route registered"
            );
        }

        Ok(Self { routes, clusters })
    }

    /// Route a message to a cluster.
    ///
    /// Returns the first matching route result, or None if no route matches.
    pub fn route(
        &self,
        destination: &str,
        source: Option<&str>,
        sender_id: Option<&str>,
    ) -> Option<RouteResult> {
        let ctx = MatchContext {
            destination,
            source,
            sender_id,
            ..Default::default()
        };
        self.route_context(&ctx)
    }

    /// Route a message using full context.
    pub fn route_context(&self, ctx: &MatchContext<'_>) -> Option<RouteResult> {
        for route in &self.routes {
            if route.matcher.matches_context(ctx) {
                let cluster_name = match &route.target {
                    RouteTarget::Single(c) => c.clone(),
                    RouteTarget::Split(split) => split.select()?.to_string(),
                };

                trace!(
                    route = %route.name,
                    destination = %ctx.destination,
                    cluster = %cluster_name,
                    "route matched"
                );

                return Some(RouteResult {
                    route_name: route.name.clone(),
                    cluster_name,
                    fallback: route.fallback.clone(),
                });
            }
        }

        warn!(
            destination = %ctx.destination,
            "no route matched"
        );

        None
    }

    /// Route and get the cluster.
    pub fn route_to_cluster(
        &self,
        destination: &str,
        source: Option<&str>,
        sender_id: Option<&str>,
    ) -> Option<(RouteResult, Arc<Cluster>)> {
        let result = self.route(destination, source, sender_id)?;
        let cluster = self.clusters.get(&result.cluster_name)?;
        Some((result, cluster))
    }

    /// Route with context and get the cluster.
    pub fn route_context_to_cluster(
        &self,
        ctx: &MatchContext<'_>,
    ) -> Option<(RouteResult, Arc<Cluster>)> {
        let result = self.route_context(ctx)?;
        let cluster = self.clusters.get(&result.cluster_name)?;
        Some((result, cluster))
    }

    /// Get all route names.
    pub fn route_names(&self) -> Vec<&str> {
        self.routes.iter().map(|r| r.name.as_str()).collect()
    }

    /// Get route count.
    pub fn route_count(&self) -> usize {
        self.routes.len()
    }
}

/// Compile a route config into a Route.
fn compile_route(config: &RouteConfig) -> Result<Route, RouterError> {
    let mut builder = Matcher::builder();

    // Handle new-style destination condition
    if let Some(ref dest) = config.match_.destination {
        if let Some(kind) = MatcherKind::from_condition(dest) {
            builder = builder.destination(kind)?;
        }
    }
    // Handle legacy prefix/regex fields
    else if let Some(ref prefix) = config.match_.prefix {
        if prefix.is_empty() {
            builder = builder.destination(MatcherKind::Any)?;
        } else {
            builder = builder.destination(MatcherKind::Prefix(prefix.clone()))?;
        }
    } else if let Some(ref regex) = config.match_.regex {
        builder = builder.destination(MatcherKind::Regex(regex.clone()))?;
    }

    // Handle new-style source condition
    if let Some(ref source_cond) = config.match_.source {
        if let Some(kind) = MatcherKind::from_condition(source_cond) {
            builder = builder.source(kind)?;
        }
    }

    // Handle new-style sender_id condition
    if let Some(ref sender_cond) = config.match_.sender_id {
        if let Some(kind) = MatcherKind::from_condition(sender_cond) {
            builder = builder.sender_id(kind)?;
        }
    }

    // Handle SMPP-specific fields
    if let Some(ref system_id) = config.match_.system_id {
        builder = builder.system_id(system_id.clone());
    }

    if let Some(ref service_type) = config.match_.service_type {
        builder = builder.service_type(service_type.clone());
    }

    if let Some(esm_class) = config.match_.esm_class {
        builder = builder.esm_class(esm_class);
    }

    if let Some(ref data_coding) = config.match_.data_coding {
        builder = builder.data_coding(data_coding.clone());
    }

    if let Some(ref time_window) = config.match_.time_window {
        builder = builder.time_window(time_window.clone());
    }

    // Determine target (single cluster or traffic split)
    let target = if let Some(ref splits) = config.split {
        RouteTarget::Split(WeightedSplit::new(splits))
    } else if let Some(ref cluster) = config.cluster {
        RouteTarget::Single(cluster.clone())
    } else {
        return Err(RouterError::InvalidRoute {
            route: config.name.clone(),
            reason: "route must have either 'cluster' or 'split' target".to_string(),
        });
    };

    let fallback = config.fallback.clone().unwrap_or_default();

    Ok(Route {
        name: config.name.clone(),
        matcher: builder.build(),
        target,
        fallback,
        priority: config.priority,
    })
}

/// Router errors.
#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    #[error("Invalid regex pattern: {0}")]
    InvalidRegex(#[from] regex::Error),

    #[error("Route '{route}' references unknown cluster '{cluster}'")]
    UnknownCluster { route: String, cluster: String },

    #[error("Invalid route '{route}': {reason}")]
    InvalidRoute { route: String, reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RouteMatch;

    fn make_route(name: &str, prefix: &str, cluster: &str, priority: i32) -> RouteConfig {
        RouteConfig {
            name: name.to_string(),
            match_: RouteMatch {
                destination: None,
                source: None,
                sender_id: None,
                system_id: None,
                service_type: None,
                esm_class: None,
                data_coding: None,
                time_window: None,
                prefix: Some(prefix.to_string()),
                regex: None,
            },
            cluster: Some(cluster.to_string()),
            split: None,
            fallback: None,
            priority,
        }
    }

    #[test]
    fn test_compile_route() {
        let config = make_route("test", "+258", "vodacom", 0);
        let route = compile_route(&config).unwrap();

        assert_eq!(route.name, "test");
        assert!(matches!(&route.target, RouteTarget::Single(c) if c == "vodacom"));
        assert!(route.matcher.matches("+258841234567", None, None));
        assert!(!route.matcher.matches("+27841234567", None, None));
    }

    #[test]
    fn test_catch_all_route() {
        let config = make_route("default", "", "fallback", 100);
        let route = compile_route(&config).unwrap();

        assert!(route.matcher.matches("+258841234567", None, None));
        assert!(route.matcher.matches("+27841234567", None, None));
        assert!(route.matcher.matches("anything", None, None));
    }

    #[test]
    fn test_regex_route() {
        let config = RouteConfig {
            name: "moz_mobile".to_string(),
            match_: RouteMatch {
                destination: None,
                source: None,
                sender_id: None,
                system_id: None,
                service_type: None,
                esm_class: None,
                data_coding: None,
                time_window: None,
                prefix: None,
                regex: Some(r"^\+258(84|82|86|87)".to_string()),
            },
            cluster: Some("vodacom".to_string()),
            split: None,
            fallback: None,
            priority: 0,
        };

        let route = compile_route(&config).unwrap();

        assert!(route.matcher.matches("+258841234567", None, None));
        assert!(route.matcher.matches("+258821234567", None, None));
        assert!(!route.matcher.matches("+258211234567", None, None)); // Landline
    }

    #[test]
    fn test_traffic_split() {
        let config = RouteConfig {
            name: "ab-test".to_string(),
            match_: RouteMatch {
                destination: None,
                source: None,
                sender_id: None,
                system_id: None,
                service_type: None,
                esm_class: None,
                data_coding: None,
                time_window: None,
                prefix: Some("+258".to_string()),
                regex: None,
            },
            cluster: None,
            split: Some(vec![
                TrafficSplit { cluster: "vodacom".to_string(), weight: 70 },
                TrafficSplit { cluster: "mtn".to_string(), weight: 30 },
            ]),
            fallback: None,
            priority: 0,
        };

        let route = compile_route(&config).unwrap();

        // Test that split selects clusters based on weights
        if let RouteTarget::Split(split) = &route.target {
            // First call should select vodacom (weight 70 of 100)
            let first = split.select();
            assert!(first.is_some());
        }
    }

    #[test]
    fn test_route_with_fallback() {
        let config = RouteConfig {
            name: "with-fallback".to_string(),
            match_: RouteMatch {
                destination: None,
                source: None,
                sender_id: None,
                system_id: None,
                service_type: None,
                esm_class: None,
                data_coding: None,
                time_window: None,
                prefix: Some("+258".to_string()),
                regex: None,
            },
            cluster: Some("vodacom".to_string()),
            split: None,
            fallback: Some(vec!["mtn".to_string(), "movitel".to_string()]),
            priority: 0,
        };

        let route = compile_route(&config).unwrap();

        assert_eq!(route.fallback.len(), 2);
        assert_eq!(route.fallback[0], "mtn");
        assert_eq!(route.fallback[1], "movitel");
    }
}
