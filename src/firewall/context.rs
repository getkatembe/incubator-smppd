//! Firewall evaluation context.
//!
//! Contains all information about a message that conditions can evaluate.

use std::collections::HashMap;
use std::net::IpAddr;

/// Context for firewall rule evaluation.
#[derive(Debug, Clone)]
pub struct EvaluationContext {
    /// Source address (sender MSISDN)
    pub source: String,
    /// Destination address (recipient MSISDN)
    pub destination: String,
    /// Sender ID (alphanumeric or numeric)
    pub sender_id: Option<String>,
    /// System ID of the bound ESME
    pub system_id: String,
    /// Message content (decoded)
    pub content: String,
    /// Content length in bytes
    pub content_length: usize,
    /// ESM class
    pub esm_class: u8,
    /// Protocol ID
    pub protocol_id: u8,
    /// Data coding scheme
    pub data_coding: u8,
    /// Client IP address
    pub client_ip: Option<IpAddr>,
    /// Whether TLS is enabled
    pub tls_enabled: bool,
    /// Service type
    pub service_type: Option<String>,
    /// Message priority
    pub priority: i32,
    /// Custom metadata
    pub metadata: HashMap<String, String>,
    /// Timestamp when message was received
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl EvaluationContext {
    /// Create a new evaluation context.
    pub fn new(
        source: &str,
        destination: &str,
        system_id: &str,
        content: &str,
    ) -> Self {
        Self {
            source: source.to_string(),
            destination: destination.to_string(),
            sender_id: None,
            system_id: system_id.to_string(),
            content: content.to_string(),
            content_length: content.len(),
            esm_class: 0,
            protocol_id: 0,
            data_coding: 0,
            client_ip: None,
            tls_enabled: false,
            service_type: None,
            priority: 0,
            metadata: HashMap::new(),
            timestamp: chrono::Utc::now(),
        }
    }

    /// Set sender ID.
    pub fn with_sender_id(mut self, sender_id: &str) -> Self {
        self.sender_id = Some(sender_id.to_string());
        self
    }

    /// Set ESM class.
    pub fn with_esm_class(mut self, esm_class: u8) -> Self {
        self.esm_class = esm_class;
        self
    }

    /// Set protocol ID.
    pub fn with_protocol_id(mut self, protocol_id: u8) -> Self {
        self.protocol_id = protocol_id;
        self
    }

    /// Set data coding.
    pub fn with_data_coding(mut self, data_coding: u8) -> Self {
        self.data_coding = data_coding;
        self
    }

    /// Set client IP.
    pub fn with_client_ip(mut self, ip: IpAddr) -> Self {
        self.client_ip = Some(ip);
        self
    }

    /// Set TLS status.
    pub fn with_tls(mut self, enabled: bool) -> Self {
        self.tls_enabled = enabled;
        self
    }

    /// Set service type.
    pub fn with_service_type(mut self, service_type: &str) -> Self {
        self.service_type = Some(service_type.to_string());
        self
    }

    /// Set priority.
    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    /// Add metadata.
    pub fn with_metadata(mut self, key: &str, value: &str) -> Self {
        self.metadata.insert(key.to_string(), value.to_string());
        self
    }

    /// Get the value for a rate limiting key.
    pub fn get_rate_key(&self, key: &super::types::RateKey) -> String {
        match key {
            super::types::RateKey::SystemId => self.system_id.clone(),
            super::types::RateKey::Source => self.source.clone(),
            super::types::RateKey::Destination => self.destination.clone(),
            super::types::RateKey::ClientIp => self
                .client_ip
                .map(|ip| ip.to_string())
                .unwrap_or_default(),
            super::types::RateKey::SenderId => self
                .sender_id
                .clone()
                .unwrap_or_else(|| self.source.clone()),
        }
    }
}

/// Result of firewall evaluation.
#[derive(Debug, Clone)]
pub enum EvaluationResult {
    /// Accept the message
    Accept,
    /// Drop silently
    Drop,
    /// Reject with status code
    Reject {
        status: u32,
        reason: Option<String>,
    },
    /// Quarantine for review
    Quarantine,
    /// Modify the message
    Modify {
        source: Option<String>,
        destination: Option<String>,
        content: Option<String>,
    },
    /// Continue to next chain/rule
    Continue,
}

impl EvaluationResult {
    /// Check if this result is terminal (stops further evaluation).
    pub fn is_terminal(&self) -> bool {
        !matches!(self, EvaluationResult::Continue | EvaluationResult::Modify { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_builder() {
        let ctx = EvaluationContext::new("+258841234567", "+258821234567", "ACME", "Hello")
            .with_sender_id("ACME")
            .with_esm_class(3)
            .with_data_coding(0)
            .with_tls(true);

        assert_eq!(ctx.source, "+258841234567");
        assert_eq!(ctx.destination, "+258821234567");
        assert_eq!(ctx.system_id, "ACME");
        assert_eq!(ctx.content, "Hello");
        assert_eq!(ctx.sender_id, Some("ACME".to_string()));
        assert_eq!(ctx.esm_class, 3);
        assert!(ctx.tls_enabled);
    }

    #[test]
    fn test_rate_key() {
        let ctx = EvaluationContext::new("+258841234567", "+258821234567", "ACME", "Hello")
            .with_sender_id("SENDER");

        assert_eq!(
            ctx.get_rate_key(&super::super::types::RateKey::SystemId),
            "ACME"
        );
        assert_eq!(
            ctx.get_rate_key(&super::super::types::RateKey::Source),
            "+258841234567"
        );
        assert_eq!(
            ctx.get_rate_key(&super::super::types::RateKey::SenderId),
            "SENDER"
        );
    }

    #[test]
    fn test_evaluation_result_terminal() {
        assert!(EvaluationResult::Accept.is_terminal());
        assert!(EvaluationResult::Drop.is_terminal());
        assert!(EvaluationResult::Reject { status: 8, reason: None }.is_terminal());
        assert!(!EvaluationResult::Continue.is_terminal());
    }
}
