//! SMPP request/response types for filter chain.

use std::collections::HashMap;
use std::time::Instant;

use smpp::pdu::{Pdu, Status};

/// Metadata attached to a request.
#[derive(Debug, Clone, Default)]
pub struct RequestMeta {
    /// Request start time
    pub start_time: Option<Instant>,
    /// System ID of the client
    pub system_id: Option<String>,
    /// Source address
    pub source_addr: Option<String>,
    /// Custom attributes
    pub attributes: HashMap<String, String>,
}

impl RequestMeta {
    /// Create new metadata with current timestamp.
    pub fn new() -> Self {
        Self {
            start_time: Some(Instant::now()),
            ..Default::default()
        }
    }

    /// Set system ID.
    pub fn with_system_id(mut self, system_id: impl Into<String>) -> Self {
        self.system_id = Some(system_id.into());
        self
    }

    /// Set source address.
    pub fn with_source_addr(mut self, source_addr: impl Into<String>) -> Self {
        self.source_addr = Some(source_addr.into());
        self
    }

    /// Set a custom attribute.
    pub fn set_attribute(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.attributes.insert(key.into(), value.into());
    }

    /// Get a custom attribute.
    pub fn get_attribute(&self, key: &str) -> Option<&str> {
        self.attributes.get(key).map(|s| s.as_str())
    }

    /// Get elapsed time since request start.
    pub fn elapsed(&self) -> Option<std::time::Duration> {
        self.start_time.map(|t| t.elapsed())
    }
}

/// SMPP request wrapper for filter chain.
#[derive(Debug, Clone)]
pub struct SmppRequest {
    /// The PDU being processed
    pub pdu: Pdu,
    /// Sequence number
    pub sequence: u32,
    /// Request metadata
    pub meta: RequestMeta,
}

impl SmppRequest {
    /// Create a new request.
    pub fn new(pdu: Pdu, sequence: u32) -> Self {
        Self {
            pdu,
            sequence,
            meta: RequestMeta::new(),
        }
    }

    /// Create a new request with metadata.
    pub fn with_meta(pdu: Pdu, sequence: u32, meta: RequestMeta) -> Self {
        Self {
            pdu,
            sequence,
            meta,
        }
    }

    /// Get destination address (for submit_sm).
    pub fn destination(&self) -> Option<&str> {
        match &self.pdu {
            Pdu::SubmitSm(sm) => Some(&sm.dest_addr),
            _ => None,
        }
    }

    /// Get source address (for submit_sm).
    pub fn source(&self) -> Option<&str> {
        match &self.pdu {
            Pdu::SubmitSm(sm) => Some(&sm.source_addr),
            _ => None,
        }
    }

    /// Get the message content (for submit_sm).
    pub fn message(&self) -> Option<&[u8]> {
        match &self.pdu {
            Pdu::SubmitSm(sm) => Some(&sm.short_message),
            _ => None,
        }
    }
}

/// SMPP response from filter chain.
#[derive(Debug, Clone)]
pub enum SmppResponse {
    /// Continue processing (pass to next filter or upstream)
    Continue(SmppRequest),
    /// Reject the request with status
    Reject {
        sequence: u32,
        status: Status,
        reason: Option<String>,
    },
    /// Return a successful response (skip upstream)
    Success {
        sequence: u32,
        pdu: Pdu,
    },
}

impl SmppResponse {
    /// Create a continue response.
    pub fn continue_with(request: SmppRequest) -> Self {
        Self::Continue(request)
    }

    /// Create a reject response.
    pub fn reject(sequence: u32, status: Status) -> Self {
        Self::Reject {
            sequence,
            status,
            reason: None,
        }
    }

    /// Create a reject response with reason.
    pub fn reject_with_reason(sequence: u32, status: Status, reason: impl Into<String>) -> Self {
        Self::Reject {
            sequence,
            status,
            reason: Some(reason.into()),
        }
    }

    /// Check if response is continue.
    pub fn is_continue(&self) -> bool {
        matches!(self, Self::Continue(_))
    }

    /// Check if response is reject.
    pub fn is_reject(&self) -> bool {
        matches!(self, Self::Reject { .. })
    }
}
