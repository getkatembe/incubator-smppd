//! Multi-tenant ESME management.
//!
//! Provides ESME configuration for multi-tenant SMS gateway:
//! - ESME accounts with credentials
//! - Per-ESME rate limits
//! - Per-ESME firewall rules
//! - Per-ESME routing preferences
//! - Pricing and billing settings
//! - Usage quotas and limits

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// ESME status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EsmeStatus {
    /// ESME is active and can send messages
    Active,
    /// ESME is suspended (billing issue)
    Suspended,
    /// ESME is disabled (admin action)
    Disabled,
    /// ESME is in trial period
    Trial,
    /// ESME has exceeded quota
    QuotaExceeded,
}

impl EsmeStatus {
    /// Check if ESME can send messages.
    pub fn can_send(&self) -> bool {
        matches!(self, EsmeStatus::Active | EsmeStatus::Trial)
    }
}

/// ESME pricing tier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingTier {
    /// Tier name
    pub name: String,
    /// Base cost per message (smallest currency unit)
    pub cost_per_message: i64,
    /// Revenue per message (what ESME pays)
    pub price_per_message: i64,
    /// Currency code (ISO 4217)
    pub currency: String,
    /// Country-specific pricing overrides
    #[serde(skip_serializing_if = "HashMap::is_empty", default)]
    pub country_pricing: HashMap<String, CountryPricing>,
}

/// Country-specific pricing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CountryPricing {
    /// Cost per message
    pub cost: i64,
    /// Price per message
    pub price: i64,
}

impl Default for PricingTier {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            cost_per_message: 10,
            price_per_message: 25,
            currency: "MZN".to_string(),
            country_pricing: HashMap::new(),
        }
    }
}

/// ESME rate limit configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EsmeRateLimit {
    /// Messages per second
    pub messages_per_second: u32,
    /// Messages per minute
    pub messages_per_minute: u32,
    /// Messages per hour
    pub messages_per_hour: u32,
    /// Messages per day
    pub messages_per_day: u32,
    /// Concurrent connections allowed
    pub max_connections: u32,
    /// Window size for rate limiting
    pub window_size_secs: u64,
}

impl Default for EsmeRateLimit {
    fn default() -> Self {
        Self {
            messages_per_second: 100,
            messages_per_minute: 5000,
            messages_per_hour: 100_000,
            messages_per_day: 1_000_000,
            max_connections: 10,
            window_size_secs: 60,
        }
    }
}

/// ESME quota configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EsmeQuota {
    /// Monthly message quota (0 = unlimited)
    pub monthly_messages: u64,
    /// Monthly spend limit in smallest currency unit (0 = unlimited)
    pub monthly_spend: i64,
    /// Daily message quota (0 = unlimited)
    pub daily_messages: u64,
    /// Current monthly usage
    #[serde(skip)]
    pub current_monthly_messages: u64,
    /// Current monthly spend
    #[serde(skip)]
    pub current_monthly_spend: i64,
    /// Current daily usage
    #[serde(skip)]
    pub current_daily_messages: u64,
}

impl Default for EsmeQuota {
    fn default() -> Self {
        Self {
            monthly_messages: 0,
            monthly_spend: 0,
            daily_messages: 0,
            current_monthly_messages: 0,
            current_monthly_spend: 0,
            current_daily_messages: 0,
        }
    }
}

impl EsmeQuota {
    /// Check if quota is exceeded.
    pub fn is_exceeded(&self) -> bool {
        if self.monthly_messages > 0 && self.current_monthly_messages >= self.monthly_messages {
            return true;
        }
        if self.monthly_spend > 0 && self.current_monthly_spend >= self.monthly_spend {
            return true;
        }
        if self.daily_messages > 0 && self.current_daily_messages >= self.daily_messages {
            return true;
        }
        false
    }

    /// Get remaining monthly messages.
    pub fn remaining_monthly(&self) -> Option<u64> {
        if self.monthly_messages > 0 {
            Some(self.monthly_messages.saturating_sub(self.current_monthly_messages))
        } else {
            None
        }
    }

    /// Get remaining daily messages.
    pub fn remaining_daily(&self) -> Option<u64> {
        if self.daily_messages > 0 {
            Some(self.daily_messages.saturating_sub(self.current_daily_messages))
        } else {
            None
        }
    }
}

/// ESME routing preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EsmeRouting {
    /// Preferred cluster for this ESME
    pub preferred_cluster: Option<String>,
    /// Clusters to avoid
    #[serde(skip_serializing_if = "HashSet::is_empty", default)]
    pub blocked_clusters: HashSet<String>,
    /// Country-to-cluster routing overrides
    #[serde(skip_serializing_if = "HashMap::is_empty", default)]
    pub country_routes: HashMap<String, String>,
    /// Use ESME-specific routing priority
    pub priority: i32,
}

impl Default for EsmeRouting {
    fn default() -> Self {
        Self {
            preferred_cluster: None,
            blocked_clusters: HashSet::new(),
            country_routes: HashMap::new(),
            priority: 0,
        }
    }
}

/// ESME firewall settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EsmeFirewall {
    /// Allowed destination prefixes (None = all allowed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_allow_list: Option<HashSet<String>>,
    /// Denied destination prefixes
    #[serde(skip_serializing_if = "HashSet::is_empty", default)]
    pub destination_deny_list: HashSet<String>,
    /// Allowed sender IDs
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_allow_list: Option<HashSet<String>>,
    /// Denied sender IDs
    #[serde(skip_serializing_if = "HashSet::is_empty", default)]
    pub sender_deny_list: HashSet<String>,
    /// Allowed countries (ISO 3166-1 alpha-2)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country_allow_list: Option<HashSet<String>>,
    /// Denied countries
    #[serde(skip_serializing_if = "HashSet::is_empty", default)]
    pub country_deny_list: HashSet<String>,
    /// Content filtering enabled
    pub content_filter_enabled: bool,
    /// Block premium rate numbers
    pub block_premium_rate: bool,
}

impl Default for EsmeFirewall {
    fn default() -> Self {
        Self {
            destination_allow_list: None,
            destination_deny_list: HashSet::new(),
            sender_allow_list: None,
            sender_deny_list: HashSet::new(),
            country_allow_list: None,
            country_deny_list: HashSet::new(),
            content_filter_enabled: true,
            block_premium_rate: true,
        }
    }
}

/// ESME credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EsmeCredentials {
    /// System ID for SMPP binding
    pub system_id: String,
    /// Password (hashed in production)
    pub password: String,
    /// IP allow list (None = all IPs allowed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip_allow_list: Option<HashSet<String>>,
    /// Require TLS
    pub require_tls: bool,
}

/// Complete ESME configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Esme {
    /// Unique ESME ID
    pub id: String,
    /// Display name
    pub name: String,
    /// ESME status
    pub status: EsmeStatus,
    /// Credentials
    pub credentials: EsmeCredentials,
    /// Pricing tier
    pub pricing: PricingTier,
    /// Rate limits
    pub rate_limits: EsmeRateLimit,
    /// Quotas
    pub quota: EsmeQuota,
    /// Routing preferences
    pub routing: EsmeRouting,
    /// Firewall settings
    pub firewall: EsmeFirewall,
    /// Custom metadata
    #[serde(skip_serializing_if = "HashMap::is_empty", default)]
    pub metadata: HashMap<String, String>,
    /// Created timestamp (RFC3339)
    pub created_at: String,
    /// Last modified timestamp
    pub modified_at: String,
    /// Contact email
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact_email: Option<String>,
    /// Callback URL for delivery receipts
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_url: Option<String>,
}

impl Esme {
    /// Create a new ESME with default settings.
    pub fn new(id: &str, name: &str, system_id: &str, password: &str) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: id.to_string(),
            name: name.to_string(),
            status: EsmeStatus::Active,
            credentials: EsmeCredentials {
                system_id: system_id.to_string(),
                password: password.to_string(),
                ip_allow_list: None,
                require_tls: false,
            },
            pricing: PricingTier::default(),
            rate_limits: EsmeRateLimit::default(),
            quota: EsmeQuota::default(),
            routing: EsmeRouting::default(),
            firewall: EsmeFirewall::default(),
            metadata: HashMap::new(),
            created_at: now.clone(),
            modified_at: now,
            contact_email: None,
            callback_url: None,
        }
    }

    /// Check if ESME can send messages.
    pub fn can_send(&self) -> bool {
        self.status.can_send() && !self.quota.is_exceeded()
    }

    /// Check if destination is allowed.
    pub fn is_destination_allowed(&self, dest: &str) -> bool {
        // Check deny list first
        for prefix in &self.firewall.destination_deny_list {
            if dest.starts_with(prefix) {
                return false;
            }
        }

        // Check allow list if present
        if let Some(ref allow_list) = self.firewall.destination_allow_list {
            for prefix in allow_list {
                if dest.starts_with(prefix) {
                    return true;
                }
            }
            return false;
        }

        true
    }

    /// Check if sender ID is allowed.
    pub fn is_sender_allowed(&self, sender: &str) -> bool {
        // Check deny list first
        if self.firewall.sender_deny_list.contains(sender) {
            return false;
        }

        // Check allow list if present
        if let Some(ref allow_list) = self.firewall.sender_allow_list {
            return allow_list.contains(sender);
        }

        true
    }

    /// Check if IP is allowed.
    pub fn is_ip_allowed(&self, ip: &str) -> bool {
        if let Some(ref allow_list) = self.credentials.ip_allow_list {
            return allow_list.contains(ip);
        }
        true
    }

    /// Get price for a destination.
    pub fn get_pricing(&self, country_code: Option<&str>) -> (i64, i64) {
        if let Some(country) = country_code {
            if let Some(pricing) = self.pricing.country_pricing.get(country) {
                return (pricing.cost, pricing.price);
            }
        }
        (self.pricing.cost_per_message, self.pricing.price_per_message)
    }

    /// Get preferred cluster for routing.
    pub fn get_preferred_cluster(&self, country_code: Option<&str>) -> Option<&str> {
        if let Some(country) = country_code {
            if let Some(cluster) = self.routing.country_routes.get(country) {
                return Some(cluster);
            }
        }
        self.routing.preferred_cluster.as_deref()
    }

    /// Record message sent (for quota tracking).
    pub fn record_message(&mut self, cost: i64) {
        self.quota.current_daily_messages += 1;
        self.quota.current_monthly_messages += 1;
        self.quota.current_monthly_spend += cost;
    }
}

/// ESME store trait for abstracting storage backend.
pub trait EsmeStore: Send + Sync + std::fmt::Debug {
    /// Get ESME by ID.
    fn get(&self, id: &str) -> Option<Esme>;

    /// Get ESME by system_id (for SMPP authentication).
    fn get_by_system_id(&self, system_id: &str) -> Option<Esme>;

    /// List all esmes.
    fn list(&self) -> Vec<Esme>;

    /// Add or update ESME.
    fn save(&self, esme: Esme) -> Result<(), String>;

    /// Delete ESME.
    fn delete(&self, id: &str) -> Result<(), String>;

    /// Authenticate ESME.
    fn authenticate(&self, system_id: &str, password: &str, ip: Option<&str>) -> AuthResult;
}

/// Authentication result.
#[derive(Debug, Clone)]
pub enum AuthResult {
    /// Authentication successful
    Success(Esme),
    /// Invalid credentials
    InvalidCredentials,
    /// ESME not found
    NotFound,
    /// ESME suspended/disabled
    Suspended(EsmeStatus),
    /// IP not allowed
    IpNotAllowed,
    /// TLS required
    TlsRequired,
}

/// In-memory ESME store.
#[derive(Debug)]
pub struct MemoryEsmeStore {
    esmes: RwLock<HashMap<String, Esme>>,
    system_id_index: RwLock<HashMap<String, String>>,
}

impl MemoryEsmeStore {
    /// Create new in-memory store.
    pub fn new() -> Self {
        Self {
            esmes: RwLock::new(HashMap::new()),
            system_id_index: RwLock::new(HashMap::new()),
        }
    }

    /// Create with initial esmes.
    pub fn with_esmes(esmes: Vec<Esme>) -> Self {
        let store = Self::new();
        for esme in esmes {
            let _ = store.save(esme);
        }
        store
    }

    /// Get ESME count.
    pub fn count(&self) -> usize {
        self.esmes.read().unwrap().len()
    }

    /// Get all active esmes.
    pub fn active_esmes(&self) -> Vec<Esme> {
        self.esmes
            .read()
            .unwrap()
            .values()
            .filter(|c| c.status.can_send())
            .cloned()
            .collect()
    }

    /// Reset daily quotas (call at midnight).
    pub fn reset_daily_quotas(&self) {
        let mut esmes = self.esmes.write().unwrap();
        for esme in esmes.values_mut() {
            esme.quota.current_daily_messages = 0;
        }
        info!(count = esmes.len(), "reset daily quotas");
    }

    /// Reset monthly quotas (call at start of month).
    pub fn reset_monthly_quotas(&self) {
        let mut esmes = self.esmes.write().unwrap();
        for esme in esmes.values_mut() {
            esme.quota.current_monthly_messages = 0;
            esme.quota.current_monthly_spend = 0;
        }
        info!(count = esmes.len(), "reset monthly quotas");
    }
}

impl Default for MemoryEsmeStore {
    fn default() -> Self {
        Self::new()
    }
}

impl EsmeStore for MemoryEsmeStore {
    fn get(&self, id: &str) -> Option<Esme> {
        self.esmes.read().unwrap().get(id).cloned()
    }

    fn get_by_system_id(&self, system_id: &str) -> Option<Esme> {
        let index = self.system_id_index.read().unwrap();
        if let Some(id) = index.get(system_id) {
            return self.get(id);
        }
        None
    }

    fn list(&self) -> Vec<Esme> {
        self.esmes.read().unwrap().values().cloned().collect()
    }

    fn save(&self, esme: Esme) -> Result<(), String> {
        let id = esme.id.clone();
        let system_id = esme.credentials.system_id.clone();

        // Update index
        {
            let mut index = self.system_id_index.write().unwrap();
            // Remove old system_id if ESME exists
            if let Some(existing) = self.esmes.read().unwrap().get(&id) {
                index.remove(&existing.credentials.system_id);
            }
            index.insert(system_id.clone(), id.clone());
        }

        // Store ESME
        self.esmes.write().unwrap().insert(id.clone(), esme);

        debug!(esme_id = %id, system_id = %system_id, "saved ESME");
        Ok(())
    }

    fn delete(&self, id: &str) -> Result<(), String> {
        if let Some(esme) = self.esmes.write().unwrap().remove(id) {
            self.system_id_index
                .write()
                .unwrap()
                .remove(&esme.credentials.system_id);
            info!(esme_id = %id, "deleted ESME");
            Ok(())
        } else {
            Err(format!("ESME not found: {}", id))
        }
    }

    fn authenticate(&self, system_id: &str, password: &str, ip: Option<&str>) -> AuthResult {
        let esme = match self.get_by_system_id(system_id) {
            Some(c) => c,
            None => return AuthResult::NotFound,
        };

        // Check status
        if !esme.status.can_send() {
            return AuthResult::Suspended(esme.status);
        }

        // Check password
        if esme.credentials.password != password {
            return AuthResult::InvalidCredentials;
        }

        // Check IP
        if let Some(client_ip) = ip {
            if !esme.is_ip_allowed(client_ip) {
                return AuthResult::IpNotAllowed;
            }
        }

        AuthResult::Success(esme)
    }
}

/// ESME usage statistics.
#[derive(Debug, Clone, Default)]
pub struct EsmeUsageStats {
    /// ESME ID
    pub esme_id: String,
    /// Total messages sent
    pub total_messages: u64,
    /// Total messages delivered
    pub delivered_messages: u64,
    /// Total messages failed
    pub failed_messages: u64,
    /// Total cost
    pub total_cost: i64,
    /// Total revenue
    pub total_revenue: i64,
    /// Average latency in ms
    pub avg_latency_ms: u64,
    /// Current rate (messages per minute)
    pub current_rate: f64,
}

/// ESME manager for runtime operations.
#[derive(Debug)]
pub struct EsmeManager {
    store: Arc<dyn EsmeStore>,
    stats: RwLock<HashMap<String, EsmeUsageStats>>,
}

impl EsmeManager {
    /// Create new ESME manager.
    pub fn new(store: Arc<dyn EsmeStore>) -> Self {
        Self {
            store,
            stats: RwLock::new(HashMap::new()),
        }
    }

    /// Authenticate ESME.
    pub fn authenticate(&self, system_id: &str, password: &str, ip: Option<&str>) -> AuthResult {
        self.store.authenticate(system_id, password, ip)
    }

    /// Get ESME by ID.
    pub fn get(&self, id: &str) -> Option<Esme> {
        self.store.get(id)
    }

    /// Get ESME by system ID.
    pub fn get_by_system_id(&self, system_id: &str) -> Option<Esme> {
        self.store.get_by_system_id(system_id)
    }

    /// Record message for ESME.
    pub fn record_message(
        &self,
        esme_id: &str,
        cost: i64,
        revenue: i64,
        delivered: bool,
        latency_ms: u64,
    ) {
        let mut stats = self.stats.write().unwrap();
        let entry = stats
            .entry(esme_id.to_string())
            .or_insert_with(|| EsmeUsageStats {
                esme_id: esme_id.to_string(),
                ..Default::default()
            });

        entry.total_messages += 1;
        if delivered {
            entry.delivered_messages += 1;
        } else {
            entry.failed_messages += 1;
        }
        entry.total_cost += cost;
        entry.total_revenue += revenue;

        // Update average latency
        let total = entry.delivered_messages + entry.failed_messages;
        entry.avg_latency_ms =
            (entry.avg_latency_ms * (total - 1) + latency_ms) / total;
    }

    /// Get usage stats for ESME.
    pub fn get_stats(&self, esme_id: &str) -> Option<EsmeUsageStats> {
        self.stats.read().unwrap().get(esme_id).cloned()
    }

    /// Get all usage stats.
    pub fn all_stats(&self) -> Vec<EsmeUsageStats> {
        self.stats.read().unwrap().values().cloned().collect()
    }

    /// Get store reference.
    pub fn store(&self) -> &Arc<dyn EsmeStore> {
        &self.store
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_esme_creation() {
        let esme = Esme::new("cust1", "Test ESME", "test_user", "secret123");

        assert_eq!(esme.id, "cust1");
        assert_eq!(esme.credentials.system_id, "test_user");
        assert!(esme.status.can_send());
        assert!(esme.can_send());
    }

    #[test]
    fn test_esme_status() {
        assert!(EsmeStatus::Active.can_send());
        assert!(EsmeStatus::Trial.can_send());
        assert!(!EsmeStatus::Suspended.can_send());
        assert!(!EsmeStatus::Disabled.can_send());
        assert!(!EsmeStatus::QuotaExceeded.can_send());
    }

    #[test]
    fn test_destination_filtering() {
        let mut esme = Esme::new("c1", "Test", "sys1", "pwd");
        esme.firewall.destination_deny_list.insert("+25880".to_string());

        assert!(esme.is_destination_allowed("+258841234567"));
        assert!(!esme.is_destination_allowed("+258801234567"));
    }

    #[test]
    fn test_sender_filtering() {
        let mut esme = Esme::new("c1", "Test", "sys1", "pwd");
        esme.firewall.sender_allow_list = Some(HashSet::from([
            "KATEMBE".to_string(),
            "VODACOM".to_string(),
        ]));

        assert!(esme.is_sender_allowed("KATEMBE"));
        assert!(!esme.is_sender_allowed("UNKNOWN"));
    }

    #[test]
    fn test_quota_checking() {
        let mut quota = EsmeQuota {
            monthly_messages: 1000,
            monthly_spend: 0,
            daily_messages: 100,
            current_monthly_messages: 500,
            current_monthly_spend: 0,
            current_daily_messages: 99,
        };

        assert!(!quota.is_exceeded());
        assert_eq!(quota.remaining_monthly(), Some(500));
        assert_eq!(quota.remaining_daily(), Some(1));

        quota.current_daily_messages = 100;
        assert!(quota.is_exceeded());
    }

    #[test]
    fn test_memory_store() {
        let store = MemoryEsmeStore::new();

        let esme = Esme::new("c1", "ESME One", "sys1", "pwd1");
        store.save(esme).unwrap();

        assert_eq!(store.count(), 1);

        let retrieved = store.get("c1").unwrap();
        assert_eq!(retrieved.name, "ESME One");

        let by_sys = store.get_by_system_id("sys1").unwrap();
        assert_eq!(by_sys.id, "c1");
    }

    #[test]
    fn test_authentication() {
        let store = MemoryEsmeStore::new();

        let esme = Esme::new("c1", "Test", "test_sys", "secret");
        store.save(esme).unwrap();

        // Valid auth
        match store.authenticate("test_sys", "secret", None) {
            AuthResult::Success(c) => assert_eq!(c.id, "c1"),
            _ => panic!("expected success"),
        }

        // Invalid password
        match store.authenticate("test_sys", "wrong", None) {
            AuthResult::InvalidCredentials => {},
            _ => panic!("expected invalid credentials"),
        }

        // Unknown user
        match store.authenticate("unknown", "any", None) {
            AuthResult::NotFound => {},
            _ => panic!("expected not found"),
        }
    }

    #[test]
    fn test_ip_filtering() {
        let store = MemoryEsmeStore::new();

        let mut esme = Esme::new("c1", "Test", "sys1", "pwd");
        esme.credentials.ip_allow_list = Some(HashSet::from([
            "192.168.1.100".to_string(),
        ]));
        store.save(esme).unwrap();

        match store.authenticate("sys1", "pwd", Some("192.168.1.100")) {
            AuthResult::Success(_) => {},
            _ => panic!("expected success for allowed IP"),
        }

        match store.authenticate("sys1", "pwd", Some("10.0.0.1")) {
            AuthResult::IpNotAllowed => {},
            _ => panic!("expected IP not allowed"),
        }
    }

    #[test]
    fn test_suspended_esme() {
        let store = MemoryEsmeStore::new();

        let mut esme = Esme::new("c1", "Test", "sys1", "pwd");
        esme.status = EsmeStatus::Suspended;
        store.save(esme).unwrap();

        match store.authenticate("sys1", "pwd", None) {
            AuthResult::Suspended(status) => {
                assert_eq!(status, EsmeStatus::Suspended);
            },
            _ => panic!("expected suspended"),
        }
    }

    #[test]
    fn test_pricing() {
        let mut esme = Esme::new("c1", "Test", "sys1", "pwd");
        esme.pricing.country_pricing.insert(
            "ZA".to_string(),
            CountryPricing { cost: 5, price: 15 },
        );

        let (cost, price) = esme.get_pricing(None);
        assert_eq!(cost, 10); // default
        assert_eq!(price, 25);

        let (cost, price) = esme.get_pricing(Some("ZA"));
        assert_eq!(cost, 5);
        assert_eq!(price, 15);
    }

    #[test]
    fn test_routing_preferences() {
        let mut esme = Esme::new("c1", "Test", "sys1", "pwd");
        esme.routing.preferred_cluster = Some("vodacom".to_string());
        esme.routing.country_routes.insert("ZA".to_string(), "mtn_za".to_string());

        assert_eq!(esme.get_preferred_cluster(None), Some("vodacom"));
        assert_eq!(esme.get_preferred_cluster(Some("ZA")), Some("mtn_za"));
        assert_eq!(esme.get_preferred_cluster(Some("MW")), Some("vodacom"));
    }

    #[test]
    fn test_esme_manager() {
        let store = Arc::new(MemoryEsmeStore::new());
        let manager = EsmeManager::new(store.clone());

        let esme = Esme::new("c1", "Test", "sys1", "pwd");
        store.save(esme).unwrap();

        // Record some messages
        manager.record_message("c1", 10, 25, true, 100);
        manager.record_message("c1", 10, 25, true, 150);
        manager.record_message("c1", 10, 25, false, 200);

        let stats = manager.get_stats("c1").unwrap();
        assert_eq!(stats.total_messages, 3);
        assert_eq!(stats.delivered_messages, 2);
        assert_eq!(stats.failed_messages, 1);
        assert_eq!(stats.total_cost, 30);
        assert_eq!(stats.total_revenue, 75);
    }
}
