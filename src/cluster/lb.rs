//! Load balancing strategies for upstream endpoints.
//!
//! Extensible design following Open-Closed Principle:
//! - Define your own load balancer by implementing the `LoadBalancer` trait
//! - Register custom load balancers with the `LoadBalancerRegistry`

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::config::EndpointConfig;

use super::endpoint::Endpoint;

/// Load balancer trait.
///
/// Implement this trait to create custom load balancing strategies.
///
/// # Example
///
/// ```ignore
/// struct MyLoadBalancer;
///
/// impl LoadBalancer for MyLoadBalancer {
///     fn name(&self) -> &'static str {
///         "my_custom_lb"
///     }
///
///     fn select<'a>(&self, endpoints: &[&'a Arc<Endpoint>]) -> Option<&'a Arc<Endpoint>> {
///         // Your custom selection logic
///         endpoints.first().copied()
///     }
/// }
/// ```
pub trait LoadBalancer: Send + Sync {
    /// Returns the name of this load balancer strategy.
    fn name(&self) -> &'static str;

    /// Select an endpoint from the available endpoints.
    fn select<'a>(&self, endpoints: &[&'a Arc<Endpoint>]) -> Option<&'a Arc<Endpoint>>;

    /// Called when an endpoint completes a request successfully.
    /// Override to track success metrics for adaptive load balancing.
    fn on_success(&self, _endpoint: &Endpoint) {}

    /// Called when an endpoint request fails.
    /// Override to track failure metrics for adaptive load balancing.
    fn on_failure(&self, _endpoint: &Endpoint) {}

    /// Called periodically to allow the load balancer to update its state.
    /// Override for time-based rebalancing strategies.
    fn tick(&self) {}
}

/// Round-robin load balancer.
///
/// Distributes requests evenly across all endpoints in order.
pub struct RoundRobin {
    counter: AtomicUsize,
}

impl RoundRobin {
    /// Create a new round-robin load balancer.
    pub fn new() -> Self {
        Self {
            counter: AtomicUsize::new(0),
        }
    }
}

impl Default for RoundRobin {
    fn default() -> Self {
        Self::new()
    }
}

impl LoadBalancer for RoundRobin {
    fn name(&self) -> &'static str {
        "round_robin"
    }

    fn select<'a>(&self, endpoints: &[&'a Arc<Endpoint>]) -> Option<&'a Arc<Endpoint>> {
        if endpoints.is_empty() {
            return None;
        }

        let idx = self.counter.fetch_add(1, Ordering::Relaxed) % endpoints.len();
        Some(endpoints[idx])
    }
}

/// Weighted load balancer.
///
/// Distributes requests based on endpoint weights.
/// Higher weight = more requests.
pub struct Weighted {
    weights: Vec<u32>,
    total_weight: u32,
    counter: AtomicUsize,
}

impl Weighted {
    /// Create a new weighted load balancer.
    pub fn new(endpoints: &[EndpointConfig]) -> Self {
        let weights: Vec<u32> = endpoints.iter().map(|e| e.weight).collect();
        let total_weight = weights.iter().sum();

        Self {
            weights,
            total_weight,
            counter: AtomicUsize::new(0),
        }
    }
}

impl LoadBalancer for Weighted {
    fn name(&self) -> &'static str {
        "weighted"
    }

    fn select<'a>(&self, endpoints: &[&'a Arc<Endpoint>]) -> Option<&'a Arc<Endpoint>> {
        if endpoints.is_empty() || self.total_weight == 0 {
            return None;
        }

        // Weighted round-robin using modulo
        let counter = self.counter.fetch_add(1, Ordering::Relaxed);
        let target = (counter as u32) % self.total_weight;

        let mut accumulated = 0u32;
        for (i, endpoint) in endpoints.iter().enumerate() {
            let weight = if i < self.weights.len() {
                self.weights[i]
            } else {
                1
            };

            accumulated += weight;
            if target < accumulated {
                return Some(endpoint);
            }
        }

        // Fallback to first endpoint
        endpoints.first().copied()
    }
}

/// Least connections load balancer.
///
/// Routes requests to the endpoint with the fewest active connections.
pub struct LeastConnections;

impl LeastConnections {
    /// Create a new least-connections load balancer.
    pub fn new() -> Self {
        Self
    }
}

impl Default for LeastConnections {
    fn default() -> Self {
        Self::new()
    }
}

impl LoadBalancer for LeastConnections {
    fn name(&self) -> &'static str {
        "least_conn"
    }

    fn select<'a>(&self, endpoints: &[&'a Arc<Endpoint>]) -> Option<&'a Arc<Endpoint>> {
        if endpoints.is_empty() {
            return None;
        }

        // Select endpoint with fewest active connections
        endpoints
            .iter()
            .min_by_key(|ep| ep.active_connections())
            .copied()
    }
}

/// Random load balancer.
///
/// Randomly selects an endpoint for each request.
pub struct Random {
    counter: AtomicUsize,
}

impl Random {
    /// Create a new random load balancer.
    pub fn new() -> Self {
        Self {
            counter: AtomicUsize::new(0),
        }
    }
}

impl Default for Random {
    fn default() -> Self {
        Self::new()
    }
}

impl LoadBalancer for Random {
    fn name(&self) -> &'static str {
        "random"
    }

    fn select<'a>(&self, endpoints: &[&'a Arc<Endpoint>]) -> Option<&'a Arc<Endpoint>> {
        if endpoints.is_empty() {
            return None;
        }

        // Simple pseudo-random using xorshift
        let mut s = self.counter.fetch_add(1, Ordering::Relaxed);
        if s == 0 {
            s = 1;
        }
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        self.counter.store(s, Ordering::Relaxed);

        let idx = s % endpoints.len();
        Some(endpoints[idx])
    }
}

/// Consistent hash load balancer.
///
/// Uses consistent hashing for session affinity. Messages with the same
/// key (destination, source, or sender ID) always go to the same endpoint.
pub struct ConsistentHash {
    /// Hash ring: (hash, endpoint_index) sorted by hash
    ring: Vec<(u64, usize)>,
    /// Number of virtual nodes per endpoint
    replicas: usize,
    /// Random counter for fallback selection
    counter: AtomicUsize,
}

impl ConsistentHash {
    /// Create a new consistent hash load balancer.
    pub fn new(endpoints: &[EndpointConfig], replicas: usize) -> Self {
        let replicas = if replicas == 0 { 100 } else { replicas };
        let mut ring = Vec::with_capacity(endpoints.len() * replicas);

        for (idx, endpoint) in endpoints.iter().enumerate() {
            for i in 0..replicas {
                // Create virtual node key
                let key = format!("{}:{}", endpoint.address, i);
                let hash = Self::hash(&key);
                ring.push((hash, idx));
            }
        }

        // Sort by hash for binary search
        ring.sort_by_key(|&(h, _)| h);

        Self {
            ring,
            replicas,
            counter: AtomicUsize::new(0),
        }
    }

    /// Hash a string key using FNV-1a.
    fn hash(key: &str) -> u64 {
        const FNV_OFFSET: u64 = 14695981039346656037;
        const FNV_PRIME: u64 = 1099511628211;

        let mut hash = FNV_OFFSET;
        for byte in key.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }

    /// Select an endpoint index by key.
    pub fn select_by_key(&self, key: &str) -> Option<usize> {
        if self.ring.is_empty() {
            return None;
        }

        let hash = Self::hash(key);

        // Binary search for the first node with hash >= key hash
        match self.ring.binary_search_by_key(&hash, |&(h, _)| h) {
            Ok(idx) => Some(self.ring[idx].1),
            Err(idx) => {
                // Wrap around if we're past the last node
                let idx = if idx >= self.ring.len() { 0 } else { idx };
                Some(self.ring[idx].1)
            }
        }
    }
}

impl Default for ConsistentHash {
    fn default() -> Self {
        Self::new(&[], 100)
    }
}

impl LoadBalancer for ConsistentHash {
    fn name(&self) -> &'static str {
        "consistent_hash"
    }

    fn select<'a>(&self, endpoints: &[&'a Arc<Endpoint>]) -> Option<&'a Arc<Endpoint>> {
        if endpoints.is_empty() {
            return None;
        }

        // If we have a configured ring, use it; otherwise fall back to random
        if self.ring.is_empty() {
            let idx = self.counter.fetch_add(1, Ordering::Relaxed) % endpoints.len();
            return Some(endpoints[idx]);
        }

        // Use first endpoint's address as key (in practice, you'd use message destination)
        // This is a fallback - actual key selection happens in the cluster layer
        let idx = self.counter.fetch_add(1, Ordering::Relaxed) % endpoints.len();
        Some(endpoints[idx])
    }
}

/// Weighted least connections load balancer.
///
/// Combines endpoint weights with active connection counts.
/// Selects the endpoint with the lowest (active_connections / weight) ratio.
pub struct WeightedLeastConn {
    weights: Vec<u32>,
}

impl WeightedLeastConn {
    /// Create a new weighted least connections load balancer.
    pub fn new(endpoints: &[EndpointConfig]) -> Self {
        let weights: Vec<u32> = endpoints.iter().map(|e| e.weight.max(1)).collect();
        Self { weights }
    }
}

impl Default for WeightedLeastConn {
    fn default() -> Self {
        Self::new(&[])
    }
}

impl LoadBalancer for WeightedLeastConn {
    fn name(&self) -> &'static str {
        "weighted_least_conn"
    }

    fn select<'a>(&self, endpoints: &[&'a Arc<Endpoint>]) -> Option<&'a Arc<Endpoint>> {
        if endpoints.is_empty() {
            return None;
        }

        // Select endpoint with lowest (connections / weight) ratio
        endpoints
            .iter()
            .enumerate()
            .min_by(|(i, ep_a), (j, ep_b)| {
                let weight_a = self.weights.get(*i).copied().unwrap_or(1) as usize;
                let weight_b = self.weights.get(*j).copied().unwrap_or(1) as usize;

                // Calculate weighted score (lower is better)
                // Multiply by weight denominator to avoid floating point
                let score_a = ep_a.active_connections() * weight_b;
                let score_b = ep_b.active_connections() * weight_a;

                score_a.cmp(&score_b)
            })
            .map(|(_, ep)| *ep)
    }
}

/// Power of Two Choices load balancer.
///
/// Randomly selects two endpoints and picks the one with fewer connections.
/// Provides better distribution than pure random with minimal overhead.
pub struct PowerOfTwoChoices {
    counter: AtomicUsize,
}

impl PowerOfTwoChoices {
    /// Create a new P2C load balancer.
    pub fn new() -> Self {
        Self {
            counter: AtomicUsize::new(0),
        }
    }

    fn random(&self) -> usize {
        let mut s = self.counter.fetch_add(1, Ordering::Relaxed);
        if s == 0 {
            s = 1;
        }
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        self.counter.store(s, Ordering::Relaxed);
        s
    }
}

impl Default for PowerOfTwoChoices {
    fn default() -> Self {
        Self::new()
    }
}

impl LoadBalancer for PowerOfTwoChoices {
    fn name(&self) -> &'static str {
        "p2c"
    }

    fn select<'a>(&self, endpoints: &[&'a Arc<Endpoint>]) -> Option<&'a Arc<Endpoint>> {
        match endpoints.len() {
            0 => None,
            1 => Some(endpoints[0]),
            n => {
                // Pick two random endpoints
                let i = self.random() % n;
                let j = (self.random() % (n - 1) + i + 1) % n;

                // Return the one with fewer connections
                let a = endpoints[i];
                let b = endpoints[j];

                if a.active_connections() <= b.active_connections() {
                    Some(a)
                } else {
                    Some(b)
                }
            }
        }
    }
}

/// Load balancer factory function type.
pub type LoadBalancerFactory = Box<dyn Fn(&[EndpointConfig]) -> Box<dyn LoadBalancer> + Send + Sync>;

/// Registry for load balancer factories.
///
/// Allows registration of custom load balancer implementations.
pub struct LoadBalancerRegistry {
    factories: std::collections::HashMap<String, LoadBalancerFactory>,
}

impl LoadBalancerRegistry {
    /// Create a new registry with default load balancers.
    pub fn new() -> Self {
        let mut registry = Self {
            factories: std::collections::HashMap::new(),
        };

        // Register built-in load balancers
        registry.register("round_robin", Box::new(|_| Box::new(RoundRobin::new())));
        registry.register("least_conn", Box::new(|_| Box::new(LeastConnections::new())));
        registry.register("random", Box::new(|_| Box::new(Random::new())));
        registry.register("p2c", Box::new(|_| Box::new(PowerOfTwoChoices::new())));
        registry.register("weighted", Box::new(|endpoints| Box::new(Weighted::new(endpoints))));
        registry.register("consistent_hash", Box::new(|endpoints| Box::new(ConsistentHash::new(endpoints, 100))));
        registry.register("weighted_least_conn", Box::new(|endpoints| Box::new(WeightedLeastConn::new(endpoints))));

        registry
    }

    /// Register a custom load balancer factory.
    pub fn register(&mut self, name: &str, factory: LoadBalancerFactory) {
        self.factories.insert(name.to_string(), factory);
    }

    /// Create a load balancer by name.
    pub fn create(&self, name: &str, endpoints: &[EndpointConfig]) -> Option<Box<dyn LoadBalancer>> {
        self.factories.get(name).map(|f| f(endpoints))
    }

    /// Get available load balancer names.
    pub fn available(&self) -> Vec<&str> {
        self.factories.keys().map(|s| s.as_str()).collect()
    }
}

impl Default for LoadBalancerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============================================================================
    // Load Balancer Registry Tests
    // ============================================================================

    #[test]
    fn test_registry_has_defaults() {
        let registry = LoadBalancerRegistry::new();
        let available = registry.available();
        assert!(available.contains(&"round_robin"));
        assert!(available.contains(&"least_conn"));
        assert!(available.contains(&"random"));
        assert!(available.contains(&"weighted"));
        assert!(available.contains(&"p2c"));
        assert!(available.contains(&"consistent_hash"));
        assert!(available.contains(&"weighted_least_conn"));
    }

    #[test]
    fn test_registry_create_round_robin() {
        let registry = LoadBalancerRegistry::new();
        let lb = registry.create("round_robin", &[]);
        assert!(lb.is_some());
        assert_eq!(lb.unwrap().name(), "round_robin");
    }

    #[test]
    fn test_registry_create_least_conn() {
        let registry = LoadBalancerRegistry::new();
        let lb = registry.create("least_conn", &[]);
        assert!(lb.is_some());
        assert_eq!(lb.unwrap().name(), "least_conn");
    }

    #[test]
    fn test_registry_create_random() {
        let registry = LoadBalancerRegistry::new();
        let lb = registry.create("random", &[]);
        assert!(lb.is_some());
        assert_eq!(lb.unwrap().name(), "random");
    }

    #[test]
    fn test_registry_create_p2c() {
        let registry = LoadBalancerRegistry::new();
        let lb = registry.create("p2c", &[]);
        assert!(lb.is_some());
        assert_eq!(lb.unwrap().name(), "p2c");
    }

    #[test]
    fn test_registry_create_weighted() {
        let registry = LoadBalancerRegistry::new();
        let endpoints = vec![
            EndpointConfig {
                address: "smsc1:2775".to_string(),
                system_id: "test".to_string(),
                password: "test".to_string(),
                system_type: String::new(),
                weight: 2,
                tls: None,
            },
            EndpointConfig {
                address: "smsc2:2775".to_string(),
                system_id: "test".to_string(),
                password: "test".to_string(),
                system_type: String::new(),
                weight: 1,
                tls: None,
            },
        ];
        let lb = registry.create("weighted", &endpoints);
        assert!(lb.is_some());
        assert_eq!(lb.unwrap().name(), "weighted");
    }

    #[test]
    fn test_registry_create_unknown() {
        let registry = LoadBalancerRegistry::new();
        let lb = registry.create("unknown_lb", &[]);
        assert!(lb.is_none());
    }

    #[test]
    fn test_registry_custom_lb() {
        struct CustomLb;
        impl LoadBalancer for CustomLb {
            fn name(&self) -> &'static str { "custom" }
            fn select<'a>(&self, endpoints: &[&'a Arc<Endpoint>]) -> Option<&'a Arc<Endpoint>> {
                endpoints.first().copied()
            }
        }

        let mut registry = LoadBalancerRegistry::new();
        registry.register("custom", Box::new(|_| Box::new(CustomLb)));

        let lb = registry.create("custom", &[]);
        assert!(lb.is_some());
        assert_eq!(lb.unwrap().name(), "custom");
    }

    // ============================================================================
    // Load Balancer Name Tests
    // ============================================================================

    #[test]
    fn test_round_robin_name() {
        let lb = RoundRobin::new();
        assert_eq!(LoadBalancer::name(&lb), "round_robin");
    }

    #[test]
    fn test_least_connections_name() {
        let lb = LeastConnections::new();
        assert_eq!(LoadBalancer::name(&lb), "least_conn");
    }

    #[test]
    fn test_random_name() {
        let lb = Random::new();
        assert_eq!(LoadBalancer::name(&lb), "random");
    }

    #[test]
    fn test_p2c_name() {
        let lb = PowerOfTwoChoices::new();
        assert_eq!(LoadBalancer::name(&lb), "p2c");
    }

    #[test]
    fn test_weighted_name() {
        let lb = Weighted::new(&[]);
        assert_eq!(LoadBalancer::name(&lb), "weighted");
    }

    // ============================================================================
    // Default Implementation Tests
    // ============================================================================

    #[test]
    fn test_round_robin_default() {
        let lb: RoundRobin = Default::default();
        assert_eq!(lb.name(), "round_robin");
    }

    #[test]
    fn test_least_connections_default() {
        let lb: LeastConnections = Default::default();
        assert_eq!(lb.name(), "least_conn");
    }

    #[test]
    fn test_random_default() {
        let lb: Random = Default::default();
        assert_eq!(lb.name(), "random");
    }

    #[test]
    fn test_p2c_default() {
        let lb: PowerOfTwoChoices = Default::default();
        assert_eq!(lb.name(), "p2c");
    }

    #[test]
    fn test_registry_default() {
        let registry: LoadBalancerRegistry = Default::default();
        assert!(registry.available().len() >= 5);
    }

    // ============================================================================
    // Trait Method Tests
    // ============================================================================

    #[test]
    fn test_on_success_noop() {
        // Default trait implementation should not panic
        let lb = RoundRobin::new();
        // Note: we can't easily test on_success without an Endpoint,
        // but we can verify the trait compiles with default methods
    }

    #[test]
    fn test_tick_noop() {
        // Default trait implementation should not panic
        let lb = RoundRobin::new();
        lb.tick();
    }
}
