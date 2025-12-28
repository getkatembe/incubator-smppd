use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::debug;

use crate::config::Config;

/// Internal event types for component communication
#[derive(Debug, Clone)]
pub enum Event {
    /// Configuration reloaded (triggered by SIGHUP)
    ConfigReloaded(Arc<Config>),

    /// Server is starting
    Starting,

    /// Server is ready to accept connections
    Ready,

    /// Shutdown initiated
    ShutdownStarted,

    /// Drain period started
    DrainStarted,

    /// Shutdown complete
    ShutdownComplete,

    /// Listener started
    ListenerStarted { name: String, address: String },

    /// Listener stopped
    ListenerStopped { name: String },

    /// Cluster health changed
    ClusterHealthChanged {
        name: String,
        healthy_endpoints: usize,
        total_endpoints: usize,
    },

    /// Connection opened
    ConnectionOpened { listener: String, peer: String },

    /// Connection closed
    ConnectionClosed { listener: String, peer: String },
}

/// Internal event bus for component communication
///
/// Uses broadcast channels to allow multiple subscribers.
/// Components can publish events and subscribe to events they care about.
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl EventBus {
    /// Create a new event bus
    pub fn new(capacity: usize) -> Arc<Self> {
        let (tx, _) = broadcast::channel(capacity);
        Arc::new(Self { tx })
    }

    /// Publish an event
    pub fn publish(&self, event: Event) {
        debug!(event = ?event, "publishing event");
        // Ignore send errors (no subscribers)
        let _ = self.tx.send(event);
    }

    /// Subscribe to events
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    /// Get number of active subscribers
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        let (tx, _) = broadcast::channel(256);
        Self { tx }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_event_bus() {
        let bus = EventBus::new(16);

        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();

        bus.publish(Event::Starting);

        assert!(matches!(rx1.recv().await.unwrap(), Event::Starting));
        assert!(matches!(rx2.recv().await.unwrap(), Event::Starting));
    }
}
