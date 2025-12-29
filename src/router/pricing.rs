//! Cost-based routing module.
//!
//! Provides pricing information and cost-aware route selection:
//! - Route pricing by country/network
//! - Least-cost routing (LCR)
//! - Quality-cost tradeoff routing
//! - Dynamic pricing updates
//! - Margin optimization

use std::collections::HashMap;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// Route pricing for a specific destination.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutePricing {
    /// Cost per message (smallest currency unit, e.g., cents)
    pub cost: i64,
    /// Currency code (ISO 4217)
    pub currency: String,
    /// Quality score (0.0 - 1.0, higher is better)
    pub quality_score: f64,
    /// Average latency in milliseconds
    pub latency_ms: u64,
    /// Delivery rate (0.0 - 1.0)
    pub delivery_rate: f64,
}

impl Default for RoutePricing {
    fn default() -> Self {
        Self {
            cost: 10,
            currency: "MZN".to_string(),
            quality_score: 0.9,
            latency_ms: 500,
            delivery_rate: 0.95,
        }
    }
}

/// Pricing table key.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct PricingKey {
    /// Cluster/route name
    pub cluster: String,
    /// Country code (ISO 3166-1 alpha-2)
    pub country: String,
    /// Network/MNO (optional)
    pub network: Option<String>,
}

impl PricingKey {
    /// Create a new pricing key.
    pub fn new(cluster: &str, country: &str, network: Option<&str>) -> Self {
        Self {
            cluster: cluster.to_string(),
            country: country.to_string(),
            network: network.map(|s| s.to_string()),
        }
    }

    /// Key without network (fallback lookup).
    pub fn without_network(&self) -> Self {
        Self {
            cluster: self.cluster.clone(),
            country: self.country.clone(),
            network: None,
        }
    }
}

/// Route pricing store.
#[derive(Debug)]
pub struct PricingStore {
    /// Pricing table: (cluster, country, network) -> pricing
    pricing: RwLock<HashMap<PricingKey, RoutePricing>>,
    /// Default pricing per cluster
    defaults: RwLock<HashMap<String, RoutePricing>>,
}

impl PricingStore {
    /// Create new pricing store.
    pub fn new() -> Self {
        Self {
            pricing: RwLock::new(HashMap::new()),
            defaults: RwLock::new(HashMap::new()),
        }
    }

    /// Set pricing for a route.
    pub fn set_pricing(&self, key: PricingKey, pricing: RoutePricing) {
        let cluster = key.cluster.clone();
        self.pricing.write().unwrap().insert(key, pricing);
        debug!(cluster = %cluster, "updated route pricing");
    }

    /// Set default pricing for a cluster.
    pub fn set_default(&self, cluster: &str, pricing: RoutePricing) {
        self.defaults
            .write()
            .unwrap()
            .insert(cluster.to_string(), pricing);
    }

    /// Get pricing for a route.
    pub fn get_pricing(&self, key: &PricingKey) -> Option<RoutePricing> {
        let pricing = self.pricing.read().unwrap();

        // Try exact match first
        if let Some(p) = pricing.get(key) {
            return Some(p.clone());
        }

        // Try without network
        if key.network.is_some() {
            if let Some(p) = pricing.get(&key.without_network()) {
                return Some(p.clone());
            }
        }

        // Fall back to cluster default
        let defaults = self.defaults.read().unwrap();
        defaults.get(&key.cluster).cloned()
    }

    /// Get cost for a route.
    pub fn get_cost(&self, cluster: &str, country: &str, network: Option<&str>) -> Option<i64> {
        let key = PricingKey::new(cluster, country, network);
        self.get_pricing(&key).map(|p| p.cost)
    }

    /// Load pricing from HashMap (for config).
    pub fn load_from_map(&self, pricing_map: HashMap<String, ClusterPricing>) {
        for (cluster, cluster_pricing) in pricing_map {
            // Set cluster default
            self.set_default(&cluster, cluster_pricing.default.clone());

            // Set country-specific pricing
            for (country, pricing) in cluster_pricing.countries {
                let key = PricingKey::new(&cluster, &country, None);
                self.set_pricing(key, pricing);
            }

            // Set network-specific pricing
            for (country, networks) in cluster_pricing.networks {
                for (network, pricing) in networks {
                    let key = PricingKey::new(&cluster, &country, Some(&network));
                    self.set_pricing(key, pricing);
                }
            }
        }

        info!(
            entries = self.pricing.read().unwrap().len(),
            "loaded pricing table"
        );
    }
}

impl Default for PricingStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Cluster pricing configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterPricing {
    /// Default pricing for this cluster
    #[serde(default)]
    pub default: RoutePricing,
    /// Country-specific pricing
    #[serde(skip_serializing_if = "HashMap::is_empty", default)]
    pub countries: HashMap<String, RoutePricing>,
    /// Network-specific pricing (country -> network -> pricing)
    #[serde(skip_serializing_if = "HashMap::is_empty", default)]
    pub networks: HashMap<String, HashMap<String, RoutePricing>>,
}

impl Default for ClusterPricing {
    fn default() -> Self {
        Self {
            default: RoutePricing::default(),
            countries: HashMap::new(),
            networks: HashMap::new(),
        }
    }
}

/// Routing strategy for cost-aware selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostRoutingStrategy {
    /// Always choose the cheapest route
    LeastCost,
    /// Choose best quality within cost threshold
    QualityFirst,
    /// Balance cost and quality
    Balanced,
    /// Maximize margin (revenue - cost)
    MaxMargin,
}

impl Default for CostRoutingStrategy {
    fn default() -> Self {
        CostRoutingStrategy::Balanced
    }
}

/// Route candidate with pricing info.
#[derive(Debug, Clone)]
pub struct RouteCandidate {
    /// Cluster/route name
    pub cluster: String,
    /// Pricing information
    pub pricing: RoutePricing,
    /// Effective cost (after any adjustments)
    pub effective_cost: i64,
    /// Combined score for ranking
    pub score: f64,
}

impl RouteCandidate {
    /// Calculate score based on strategy.
    pub fn calculate_score(&mut self, strategy: CostRoutingStrategy, revenue: Option<i64>) {
        self.score = match strategy {
            CostRoutingStrategy::LeastCost => {
                // Lower cost = higher score
                1.0 / (self.effective_cost as f64 + 1.0)
            }
            CostRoutingStrategy::QualityFirst => {
                // Quality dominates, cost is secondary
                self.pricing.quality_score * 1000.0 - (self.effective_cost as f64 / 100.0)
            }
            CostRoutingStrategy::Balanced => {
                // Weighted combination
                let quality_weight = 0.4;
                let cost_weight = 0.3;
                let delivery_weight = 0.3;

                let cost_score = 1.0 / (self.effective_cost as f64 / 10.0 + 1.0);
                quality_weight * self.pricing.quality_score
                    + cost_weight * cost_score
                    + delivery_weight * self.pricing.delivery_rate
            }
            CostRoutingStrategy::MaxMargin => {
                // Maximize margin
                if let Some(rev) = revenue {
                    (rev - self.effective_cost) as f64
                } else {
                    // Fall back to least cost if no revenue info
                    1.0 / (self.effective_cost as f64 + 1.0)
                }
            }
        };
    }
}

/// Cost-based route selector.
#[derive(Debug)]
pub struct CostRouter {
    pricing_store: PricingStore,
    default_strategy: CostRoutingStrategy,
}

impl CostRouter {
    /// Create new cost router.
    pub fn new() -> Self {
        Self {
            pricing_store: PricingStore::new(),
            default_strategy: CostRoutingStrategy::Balanced,
        }
    }

    /// Create with strategy.
    pub fn with_strategy(strategy: CostRoutingStrategy) -> Self {
        Self {
            pricing_store: PricingStore::new(),
            default_strategy: strategy,
        }
    }

    /// Get pricing store reference.
    pub fn pricing_store(&self) -> &PricingStore {
        &self.pricing_store
    }

    /// Select best route from candidates.
    pub fn select_route(
        &self,
        candidates: Vec<String>,
        country: &str,
        network: Option<&str>,
        revenue: Option<i64>,
    ) -> Option<String> {
        self.select_route_with_strategy(
            candidates,
            country,
            network,
            revenue,
            self.default_strategy,
        )
    }

    /// Select best route with specific strategy.
    pub fn select_route_with_strategy(
        &self,
        candidates: Vec<String>,
        country: &str,
        network: Option<&str>,
        revenue: Option<i64>,
        strategy: CostRoutingStrategy,
    ) -> Option<String> {
        if candidates.is_empty() {
            return None;
        }

        let mut route_candidates: Vec<RouteCandidate> = candidates
            .into_iter()
            .filter_map(|cluster| {
                let key = PricingKey::new(&cluster, country, network);
                let pricing = self
                    .pricing_store
                    .get_pricing(&key)
                    .unwrap_or_default();

                Some(RouteCandidate {
                    cluster,
                    effective_cost: pricing.cost,
                    pricing,
                    score: 0.0,
                })
            })
            .collect();

        // Calculate scores
        for candidate in &mut route_candidates {
            candidate.calculate_score(strategy, revenue);
        }

        // Sort by score (descending)
        route_candidates.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Return best candidate
        route_candidates.first().map(|c| {
            debug!(
                cluster = %c.cluster,
                cost = c.effective_cost,
                score = c.score,
                strategy = ?strategy,
                "selected route"
            );
            c.cluster.clone()
        })
    }

    /// Get ranked routes.
    pub fn rank_routes(
        &self,
        candidates: Vec<String>,
        country: &str,
        network: Option<&str>,
        revenue: Option<i64>,
    ) -> Vec<RouteCandidate> {
        let mut route_candidates: Vec<RouteCandidate> = candidates
            .into_iter()
            .filter_map(|cluster| {
                let key = PricingKey::new(&cluster, country, network);
                let pricing = self
                    .pricing_store
                    .get_pricing(&key)
                    .unwrap_or_default();

                Some(RouteCandidate {
                    cluster,
                    effective_cost: pricing.cost,
                    pricing,
                    score: 0.0,
                })
            })
            .collect();

        for candidate in &mut route_candidates {
            candidate.calculate_score(self.default_strategy, revenue);
        }

        route_candidates.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        route_candidates
    }
}

impl Default for CostRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pricing_key() {
        let key1 = PricingKey::new("vodacom", "MZ", Some("Vodacom"));
        let key2 = key1.without_network();

        assert_eq!(key1.cluster, "vodacom");
        assert_eq!(key1.network, Some("Vodacom".to_string()));
        assert_eq!(key2.network, None);
    }

    #[test]
    fn test_pricing_store() {
        let store = PricingStore::new();

        // Set default
        store.set_default(
            "vodacom",
            RoutePricing {
                cost: 10,
                currency: "MZN".to_string(),
                quality_score: 0.95,
                latency_ms: 400,
                delivery_rate: 0.98,
            },
        );

        // Set country-specific
        let key = PricingKey::new("vodacom", "ZA", None);
        store.set_pricing(
            key.clone(),
            RoutePricing {
                cost: 5,
                currency: "ZAR".to_string(),
                quality_score: 0.9,
                latency_ms: 300,
                delivery_rate: 0.97,
            },
        );

        // Test lookups
        let pricing = store.get_pricing(&key).unwrap();
        assert_eq!(pricing.cost, 5);
        assert_eq!(pricing.currency, "ZAR");

        // Test fallback to default
        let unknown_key = PricingKey::new("vodacom", "MW", None);
        let default_pricing = store.get_pricing(&unknown_key).unwrap();
        assert_eq!(default_pricing.cost, 10);
    }

    #[test]
    fn test_cost_router_least_cost() {
        let router = CostRouter::with_strategy(CostRoutingStrategy::LeastCost);

        router.pricing_store().set_default(
            "vodacom",
            RoutePricing {
                cost: 15,
                ..Default::default()
            },
        );

        router.pricing_store().set_default(
            "mtn",
            RoutePricing {
                cost: 10,
                ..Default::default()
            },
        );

        router.pricing_store().set_default(
            "movitel",
            RoutePricing {
                cost: 8,
                ..Default::default()
            },
        );

        let candidates = vec![
            "vodacom".to_string(),
            "mtn".to_string(),
            "movitel".to_string(),
        ];

        let best = router.select_route(candidates, "MZ", None, None);
        assert_eq!(best, Some("movitel".to_string()));
    }

    #[test]
    fn test_cost_router_quality_first() {
        let router = CostRouter::with_strategy(CostRoutingStrategy::QualityFirst);

        router.pricing_store().set_default(
            "vodacom",
            RoutePricing {
                cost: 20,
                quality_score: 0.99,
                ..Default::default()
            },
        );

        router.pricing_store().set_default(
            "mtn",
            RoutePricing {
                cost: 10,
                quality_score: 0.85,
                ..Default::default()
            },
        );

        let candidates = vec!["vodacom".to_string(), "mtn".to_string()];

        // Quality first should prefer vodacom despite higher cost
        let best = router.select_route(candidates, "MZ", None, None);
        assert_eq!(best, Some("vodacom".to_string()));
    }

    #[test]
    fn test_cost_router_max_margin() {
        let router = CostRouter::with_strategy(CostRoutingStrategy::MaxMargin);

        router.pricing_store().set_default(
            "vodacom",
            RoutePricing {
                cost: 10,
                ..Default::default()
            },
        );

        router.pricing_store().set_default(
            "mtn",
            RoutePricing {
                cost: 8,
                ..Default::default()
            },
        );

        let candidates = vec!["vodacom".to_string(), "mtn".to_string()];

        // With revenue of 25, mtn gives margin of 17, vodacom gives 15
        let best = router.select_route(candidates.clone(), "MZ", None, Some(25));
        assert_eq!(best, Some("mtn".to_string()));

        // With revenue of 9, mtn gives margin of 1, vodacom gives -1
        let best = router.select_route(candidates, "MZ", None, Some(9));
        assert_eq!(best, Some("mtn".to_string()));
    }

    #[test]
    fn test_rank_routes() {
        let router = CostRouter::with_strategy(CostRoutingStrategy::Balanced);

        router.pricing_store().set_default(
            "vodacom",
            RoutePricing {
                cost: 15,
                quality_score: 0.95,
                delivery_rate: 0.98,
                ..Default::default()
            },
        );

        router.pricing_store().set_default(
            "mtn",
            RoutePricing {
                cost: 10,
                quality_score: 0.85,
                delivery_rate: 0.90,
                ..Default::default()
            },
        );

        router.pricing_store().set_default(
            "movitel",
            RoutePricing {
                cost: 8,
                quality_score: 0.80,
                delivery_rate: 0.85,
                ..Default::default()
            },
        );

        let candidates = vec![
            "vodacom".to_string(),
            "mtn".to_string(),
            "movitel".to_string(),
        ];

        let ranked = router.rank_routes(candidates, "MZ", None, None);
        assert_eq!(ranked.len(), 3);

        // All should have scores
        for r in &ranked {
            assert!(r.score > 0.0);
        }
    }

    #[test]
    fn test_network_specific_pricing() {
        let store = PricingStore::new();

        // Set country default
        store.set_pricing(
            PricingKey::new("vodacom", "MZ", None),
            RoutePricing {
                cost: 10,
                ..Default::default()
            },
        );

        // Set network-specific (cheaper for Vodacom network)
        store.set_pricing(
            PricingKey::new("vodacom", "MZ", Some("Vodacom")),
            RoutePricing {
                cost: 5,
                ..Default::default()
            },
        );

        // Lookup with network
        let vodacom_key = PricingKey::new("vodacom", "MZ", Some("Vodacom"));
        assert_eq!(store.get_pricing(&vodacom_key).unwrap().cost, 5);

        // Lookup for different network falls back to country
        let mtn_key = PricingKey::new("vodacom", "MZ", Some("mcel"));
        assert_eq!(store.get_pricing(&mtn_key).unwrap().cost, 10);
    }
}
