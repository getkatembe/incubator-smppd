//! Session registry for event-driven response routing.
//!
//! The SessionRegistry maintains a mapping of SessionId -> response channel,
//! allowing the event-driven pipeline to route responses back to the correct
//! client session without blocking.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, RwLock};
use tracing::{debug, warn};

use crate::bootstrap::{ResponseEnvelope, SessionId};

/// Response sender for a session
pub type ResponseSender = mpsc::Sender<ResponseEnvelope>;

/// Session registry for routing responses to sessions
pub struct SessionRegistry {
    /// Map of session ID to response channel
    sessions: RwLock<HashMap<SessionId, SessionEntry>>,
}

/// Entry for a registered session
struct SessionEntry {
    /// Channel to send responses
    tx: ResponseSender,
    /// System ID (once bound)
    system_id: Option<String>,
    /// Listener name
    listener: String,
}

impl SessionRegistry {
    /// Create a new session registry
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            sessions: RwLock::new(HashMap::new()),
        })
    }

    /// Register a session and get a receiver for responses
    pub async fn register(
        &self,
        session_id: SessionId,
        listener: String,
    ) -> mpsc::Receiver<ResponseEnvelope> {
        let (tx, rx) = mpsc::channel(256);

        let entry = SessionEntry {
            tx,
            system_id: None,
            listener,
        };

        let mut sessions = self.sessions.write().await;
        sessions.insert(session_id, entry);

        debug!(%session_id, "session registered");
        rx
    }

    /// Update session with system_id after bind
    pub async fn set_system_id(&self, session_id: SessionId, system_id: String) {
        let mut sessions = self.sessions.write().await;
        if let Some(entry) = sessions.get_mut(&session_id) {
            entry.system_id = Some(system_id);
        }
    }

    /// Unregister a session
    pub async fn unregister(&self, session_id: SessionId) {
        let mut sessions = self.sessions.write().await;
        if sessions.remove(&session_id).is_some() {
            debug!(%session_id, "session unregistered");
        }
    }

    /// Send a response to a session
    pub async fn send_response(&self, response: ResponseEnvelope) -> bool {
        let sessions = self.sessions.read().await;

        if let Some(entry) = sessions.get(&response.session_id) {
            match entry.tx.try_send(response.clone()) {
                Ok(()) => {
                    debug!(
                        session_id = %response.session_id,
                        sequence = response.sequence_number,
                        "response sent to session"
                    );
                    true
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    warn!(
                        session_id = %response.session_id,
                        "session response channel full, dropping response"
                    );
                    false
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    warn!(
                        session_id = %response.session_id,
                        "session channel closed"
                    );
                    false
                }
            }
        } else {
            warn!(
                session_id = %response.session_id,
                "session not found, dropping response"
            );
            false
        }
    }

    /// Get the number of registered sessions
    pub async fn count(&self) -> usize {
        self.sessions.read().await.len()
    }

    /// Check if a session is registered
    pub async fn contains(&self, session_id: SessionId) -> bool {
        self.sessions.read().await.contains_key(&session_id)
    }
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
        }
    }
}

/// Shared session registry type
pub type SharedSessionRegistry = Arc<SessionRegistry>;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_register_unregister() {
        let registry = SessionRegistry::new();
        let session_id = SessionId::new();

        let _rx = registry.register(session_id, "test".into()).await;
        assert!(registry.contains(session_id).await);

        registry.unregister(session_id).await;
        assert!(!registry.contains(session_id).await);
    }

    #[tokio::test]
    async fn test_send_response() {
        let registry = SessionRegistry::new();
        let session_id = SessionId::new();

        let mut rx = registry.register(session_id, "test".into()).await;

        let response = ResponseEnvelope::success(session_id, 1, "msg_123".into());
        assert!(registry.send_response(response.clone()).await);

        let received = rx.recv().await.unwrap();
        assert_eq!(received.sequence_number, 1);
        assert_eq!(received.message_id, "msg_123");
    }

    #[tokio::test]
    async fn test_send_to_unknown_session() {
        let registry = SessionRegistry::new();
        let session_id = SessionId::new();

        let response = ResponseEnvelope::success(session_id, 1, "msg_123".into());
        assert!(!registry.send_response(response).await);
    }
}
