//! CDR type definitions.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Type of CDR record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CdrType {
    /// Message submission (MT)
    Submit,
    /// Delivery receipt
    Delivery,
    /// Mobile originated message
    MobileOriginated,
    /// Query response
    Query,
}

/// Call Detail Record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cdr {
    /// CDR type
    pub cdr_type: CdrType,

    /// Unique message ID
    pub message_id: String,

    /// SMSC message ID (from upstream)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smsc_message_id: Option<String>,

    /// Timestamp when record was created (RFC3339 string)
    pub timestamp: String,

    /// ESME/system ID
    pub esme_id: String,

    /// Source address (sender)
    pub source_addr: String,

    /// Destination address (recipient)
    pub dest_addr: String,

    /// Message content (may be truncated/hashed for privacy)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,

    /// Message length in bytes
    pub message_length: u32,

    /// Number of message segments
    pub segments: u32,

    /// Data coding scheme
    pub data_coding: u8,

    /// ESM class
    pub esm_class: u8,

    /// Route/cluster used
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,

    /// Endpoint used
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,

    /// Delivery status
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery_status: Option<String>,

    /// Delivery info (for DLR records)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery_info: Option<DeliveryInfo>,

    /// SMPP status code
    pub status_code: u32,

    /// Status description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,

    /// Processing latency in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,

    /// Cost (in smallest currency unit, e.g., cents)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<i64>,

    /// Revenue (in smallest currency unit)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revenue: Option<i64>,

    /// Currency code (ISO 4217)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,

    /// Destination country code (ISO 3166-1 alpha-2)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country_code: Option<String>,

    /// Destination network/MNO
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,

    /// MCC (Mobile Country Code)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcc: Option<String>,

    /// MNC (Mobile Network Code)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mnc: Option<String>,

    /// Was message ported (MNP lookup result)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ported: Option<bool>,

    /// Firewall result
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firewall_result: Option<String>,

    /// Firewall rule matched
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firewall_rule: Option<String>,

    /// Rate limit applied
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limited: Option<bool>,

    /// Retry count
    pub retry_count: u32,

    /// Listener name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listener: Option<String>,

    /// Connection ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<u64>,

    /// Client IP address
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_ip: Option<String>,

    /// TLS used
    pub tls: bool,

    /// SMPP protocol version
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smpp_version: Option<String>,

    /// Custom fields
    #[serde(skip_serializing_if = "HashMap::is_empty", default)]
    pub custom: HashMap<String, String>,
}

impl Cdr {
    /// Create a new submission CDR.
    pub fn submit(
        message_id: &str,
        esme_id: &str,
        source_addr: &str,
        dest_addr: &str,
    ) -> Self {
        Self {
            cdr_type: CdrType::Submit,
            message_id: message_id.to_string(),
            smsc_message_id: None,
            timestamp: Utc::now().to_rfc3339(),
            esme_id: esme_id.to_string(),
            source_addr: source_addr.to_string(),
            dest_addr: dest_addr.to_string(),
            content: None,
            message_length: 0,
            segments: 1,
            data_coding: 0,
            esm_class: 0,
            route: None,
            endpoint: None,
            delivery_status: None,
            delivery_info: None,
            status_code: 0,
            status_message: None,
            latency_ms: None,
            cost: None,
            revenue: None,
            currency: None,
            country_code: None,
            network: None,
            mcc: None,
            mnc: None,
            ported: None,
            firewall_result: None,
            firewall_rule: None,
            rate_limited: None,
            retry_count: 0,
            listener: None,
            connection_id: None,
            client_ip: None,
            tls: false,
            smpp_version: None,
            custom: HashMap::new(),
        }
    }

    /// Create a delivery CDR.
    pub fn delivery(
        message_id: &str,
        smsc_message_id: &str,
        status: &str,
    ) -> Self {
        let mut cdr = Self::submit(message_id, "", "", "");
        cdr.cdr_type = CdrType::Delivery;
        cdr.smsc_message_id = Some(smsc_message_id.to_string());
        cdr.delivery_status = Some(status.to_string());
        cdr
    }

    /// Set content (with optional truncation).
    pub fn with_content(mut self, content: &str, max_len: Option<usize>) -> Self {
        self.message_length = content.len() as u32;
        if let Some(max) = max_len {
            if content.len() > max {
                self.content = Some(format!("{}...", &content[..max]));
            } else {
                self.content = Some(content.to_string());
            }
        } else {
            self.content = Some(content.to_string());
        }
        self
    }

    /// Set route information.
    pub fn with_route(mut self, route: &str, endpoint: Option<&str>) -> Self {
        self.route = Some(route.to_string());
        self.endpoint = endpoint.map(|s| s.to_string());
        self
    }

    /// Set pricing.
    pub fn with_pricing(mut self, cost: i64, revenue: i64, currency: &str) -> Self {
        self.cost = Some(cost);
        self.revenue = Some(revenue);
        self.currency = Some(currency.to_string());
        self
    }

    /// Set network information.
    pub fn with_network(mut self, country: &str, mcc: &str, mnc: &str, network: &str) -> Self {
        self.country_code = Some(country.to_string());
        self.mcc = Some(mcc.to_string());
        self.mnc = Some(mnc.to_string());
        self.network = Some(network.to_string());
        self
    }

    /// Set latency.
    pub fn with_latency(mut self, ms: u64) -> Self {
        self.latency_ms = Some(ms);
        self
    }

    /// Set status.
    pub fn with_status(mut self, code: u32, message: Option<&str>) -> Self {
        self.status_code = code;
        self.status_message = message.map(|s| s.to_string());
        self
    }

    /// Add custom field.
    pub fn with_custom(mut self, key: &str, value: &str) -> Self {
        self.custom.insert(key.to_string(), value.to_string());
        self
    }

    /// Calculate margin.
    pub fn margin(&self) -> Option<i64> {
        match (self.revenue, self.cost) {
            (Some(r), Some(c)) => Some(r - c),
            _ => None,
        }
    }

    /// Convert to CSV line.
    pub fn to_csv_line(&self) -> String {
        let fields = vec![
            format!("{:?}", self.cdr_type),
            self.message_id.clone(),
            self.smsc_message_id.clone().unwrap_or_default(),
            self.timestamp.clone(),
            self.esme_id.clone(),
            self.source_addr.clone(),
            self.dest_addr.clone(),
            self.message_length.to_string(),
            self.segments.to_string(),
            self.route.clone().unwrap_or_default(),
            self.endpoint.clone().unwrap_or_default(),
            self.delivery_status.clone().unwrap_or_default(),
            self.status_code.to_string(),
            self.latency_ms.map(|l| l.to_string()).unwrap_or_default(),
            self.cost.map(|c| c.to_string()).unwrap_or_default(),
            self.revenue.map(|r| r.to_string()).unwrap_or_default(),
            self.currency.clone().unwrap_or_default(),
            self.country_code.clone().unwrap_or_default(),
            self.network.clone().unwrap_or_default(),
        ];

        // Escape commas and quotes
        fields
            .into_iter()
            .map(|f| {
                if f.contains(',') || f.contains('"') || f.contains('\n') {
                    format!("\"{}\"", f.replace('"', "\"\""))
                } else {
                    f
                }
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    /// CSV header.
    pub fn csv_header() -> &'static str {
        "type,message_id,smsc_message_id,timestamp,esme_id,source_addr,dest_addr,message_length,segments,route,endpoint,delivery_status,status_code,latency_ms,cost,revenue,currency,country_code,network"
    }

    /// Get timestamp as DateTime.
    pub fn timestamp_datetime(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&self.timestamp)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
    }
}

/// Delivery information for DLR CDRs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryInfo {
    /// Submit timestamp (RFC3339)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submit_date: Option<String>,
    /// Delivery timestamp (RFC3339)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_date: Option<String>,
    /// Delivery latency in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery_latency_secs: Option<u64>,
    /// Error code from network
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<u32>,
    /// Error description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_text: Option<String>,
}

/// CDR field for custom extraction/filtering.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CdrField {
    MessageId,
    SmscMessageId,
    Timestamp,
    EsmeId,
    SourceAddr,
    DestAddr,
    Route,
    Endpoint,
    DeliveryStatus,
    StatusCode,
    Cost,
    Revenue,
    CountryCode,
    Network,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_submit_cdr() {
        let cdr = Cdr::submit("msg123", "esme1", "SENDER", "+1234567890")
            .with_content("Hello world", Some(50))
            .with_route("vodacom", Some("smsc1.vodacom:2775"))
            .with_pricing(100, 150, "MZN");

        assert_eq!(cdr.cdr_type, CdrType::Submit);
        assert_eq!(cdr.message_id, "msg123");
        assert_eq!(cdr.esme_id, "esme1");
        assert_eq!(cdr.margin(), Some(50));
    }

    #[test]
    fn test_delivery_cdr() {
        let cdr = Cdr::delivery("msg123", "smsc456", "DELIVRD");

        assert_eq!(cdr.cdr_type, CdrType::Delivery);
        assert_eq!(cdr.smsc_message_id, Some("smsc456".to_string()));
        assert_eq!(cdr.delivery_status, Some("DELIVRD".to_string()));
    }

    #[test]
    fn test_csv_output() {
        let cdr = Cdr::submit("msg123", "esme1", "SENDER", "+258841234567")
            .with_route("vodacom", None);

        let csv = cdr.to_csv_line();
        assert!(csv.contains("msg123"));
        assert!(csv.contains("esme1"));
    }

    #[test]
    fn test_csv_escaping() {
        let cdr = Cdr::submit("msg,123", "cust\"1", "SENDER", "+258841234567");
        let csv = cdr.to_csv_line();
        assert!(csv.contains("\"msg,123\""));
        assert!(csv.contains("\"cust\"\"1\""));
    }

    #[test]
    fn test_json_serialization() {
        let cdr = Cdr::submit("msg1", "esme1", "SENDER", "+258841234567");
        let json = serde_json::to_string(&cdr).unwrap();
        assert!(json.contains("\"message_id\":\"msg1\""));
    }
}
