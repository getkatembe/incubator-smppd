//! Mock SMSC for testing without upstream connections.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use smpp::pdu::{Pdu, Status, SubmitSmResp};
use smpp::TlvMap;
use tokio::time::sleep;
use tracing::{debug, trace};

use crate::config::{MockConfig, MockResponse};

/// Mock SMSC that generates responses without upstream connections.
#[derive(Debug)]
pub struct MockSmsc {
    /// Response configuration
    response: MockResponse,
    /// Simulated latency
    latency: Duration,
    /// Message ID counter
    message_counter: AtomicU64,
    /// Total requests processed
    request_count: AtomicU64,
    /// Successful responses
    success_count: AtomicU64,
    /// Error responses
    error_count: AtomicU64,
}

impl MockSmsc {
    /// Create a new mock SMSC.
    pub fn new(config: &MockConfig) -> Self {
        Self {
            response: config.response.clone(),
            latency: config.latency,
            message_counter: AtomicU64::new(1),
            request_count: AtomicU64::new(0),
            success_count: AtomicU64::new(0),
            error_count: AtomicU64::new(0),
        }
    }

    /// Create a mock with default success responses.
    pub fn success() -> Self {
        Self {
            response: MockResponse::Success,
            latency: Duration::ZERO,
            message_counter: AtomicU64::new(1),
            request_count: AtomicU64::new(0),
            success_count: AtomicU64::new(0),
            error_count: AtomicU64::new(0),
        }
    }

    /// Create a mock with error responses.
    pub fn error(code: u32) -> Self {
        Self {
            response: MockResponse::Error { code },
            latency: Duration::ZERO,
            message_counter: AtomicU64::new(1),
            request_count: AtomicU64::new(0),
            success_count: AtomicU64::new(0),
            error_count: AtomicU64::new(0),
        }
    }

    /// Create a mock with random errors.
    pub fn random(error_rate: f32) -> Self {
        Self {
            response: MockResponse::Random { error_rate },
            latency: Duration::ZERO,
            message_counter: AtomicU64::new(1),
            request_count: AtomicU64::new(0),
            success_count: AtomicU64::new(0),
            error_count: AtomicU64::new(0),
        }
    }

    /// Set simulated latency.
    pub fn with_latency(mut self, latency: Duration) -> Self {
        self.latency = latency;
        self
    }

    /// Process a submit_sm request.
    pub async fn submit_sm(&self, _pdu: &Pdu) -> MockResult {
        self.request_count.fetch_add(1, Ordering::Relaxed);

        // Simulate latency
        if !self.latency.is_zero() {
            trace!(latency_ms = self.latency.as_millis(), "simulating latency");
            sleep(self.latency).await;
        }

        // Generate response based on configuration
        match &self.response {
            MockResponse::Success => {
                let message_id = self.next_message_id();
                self.success_count.fetch_add(1, Ordering::Relaxed);
                debug!(message_id = %message_id, "mock success");
                MockResult::Success {
                    message_id: message_id.clone(),
                    pdu: SubmitSmResp {
                        message_id,
                        tlvs: TlvMap::default(),
                    },
                }
            }
            MockResponse::Error { code } => {
                self.error_count.fetch_add(1, Ordering::Relaxed);
                debug!(code = *code, "mock error");
                MockResult::Error {
                    status: Status::from_u32(*code),
                }
            }
            MockResponse::Random { error_rate } => {
                // Simple random using message counter as seed
                let counter = self.message_counter.load(Ordering::Relaxed);
                let is_error = (counter % 100) < (*error_rate * 100.0) as u64;

                if is_error {
                    self.error_count.fetch_add(1, Ordering::Relaxed);
                    debug!("mock random error");
                    MockResult::Error {
                        status: Status::SystemError,
                    }
                } else {
                    let message_id = self.next_message_id();
                    self.success_count.fetch_add(1, Ordering::Relaxed);
                    debug!(message_id = %message_id, "mock random success");
                    MockResult::Success {
                        message_id: message_id.clone(),
                        pdu: SubmitSmResp {
                            message_id,
                            tlvs: TlvMap::default(),
                        },
                    }
                }
            }
        }
    }

    /// Generate next message ID.
    fn next_message_id(&self) -> String {
        let id = self.message_counter.fetch_add(1, Ordering::Relaxed);
        format!("MOCK{:016X}", id)
    }

    /// Get total request count.
    pub fn request_count(&self) -> u64 {
        self.request_count.load(Ordering::Relaxed)
    }

    /// Get success count.
    pub fn success_count(&self) -> u64 {
        self.success_count.load(Ordering::Relaxed)
    }

    /// Get error count.
    pub fn error_count(&self) -> u64 {
        self.error_count.load(Ordering::Relaxed)
    }
}

/// Result from mock SMSC.
#[derive(Debug)]
pub enum MockResult {
    /// Successful response
    Success {
        message_id: String,
        pdu: SubmitSmResp,
    },
    /// Error response
    Error {
        status: Status,
    },
}

impl MockResult {
    /// Check if successful.
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success { .. })
    }

    /// Get message ID if successful.
    pub fn message_id(&self) -> Option<&str> {
        match self {
            Self::Success { message_id, .. } => Some(message_id),
            Self::Error { .. } => None,
        }
    }

    /// Get status code.
    pub fn status(&self) -> Status {
        match self {
            Self::Success { .. } => Status::Ok,
            Self::Error { status } => *status,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smpp::pdu::SubmitSm;

    #[tokio::test]
    async fn test_mock_success() {
        let mock = MockSmsc::success();
        let pdu = Pdu::SubmitSm(SubmitSm::new("+258841234567", b"test".to_vec()));

        let result = mock.submit_sm(&pdu).await;
        assert!(result.is_success());
        assert!(result.message_id().unwrap().starts_with("MOCK"));
        assert_eq!(mock.request_count(), 1);
        assert_eq!(mock.success_count(), 1);
    }

    #[tokio::test]
    async fn test_mock_error() {
        let mock = MockSmsc::error(0x08); // System error

        let pdu = Pdu::SubmitSm(SubmitSm::new("+258841234567", b"test".to_vec()));

        let result = mock.submit_sm(&pdu).await;
        assert!(!result.is_success());
        assert_eq!(mock.error_count(), 1);
    }

    #[tokio::test]
    async fn test_mock_latency() {
        let mock = MockSmsc::success().with_latency(Duration::from_millis(10));
        let pdu = Pdu::SubmitSm(SubmitSm::new("+258841234567", b"test".to_vec()));

        let start = std::time::Instant::now();
        let _result = mock.submit_sm(&pdu).await;
        assert!(start.elapsed() >= Duration::from_millis(10));
    }
}
