use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::broadcast;
use tracing::debug;

use crate::config::Config;
use crate::store::MessageId;
use smpp::pdu::SubmitSm;

/// Storage event for reactive notifications.
#[derive(Debug, Clone)]
pub enum StorageEvent {
    /// Message stored
    MessageStored { id: MessageId },
    /// Message state changed
    StateChanged {
        id: MessageId,
        old_state: String,
        new_state: String,
    },
    /// Message deleted
    MessageDeleted { id: MessageId },
    /// Maintenance completed
    MaintenanceCompleted { expired: u64, pruned: u64 },
}

/// Unique session identifier for routing responses back
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(u64);

impl SessionId {
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "session_{}", self.0)
    }
}

/// Message envelope for event-driven processing
#[derive(Debug, Clone)]
pub struct MessageEnvelope {
    /// Internal message ID
    pub message_id: MessageId,
    /// Session that submitted the message
    pub session_id: SessionId,
    /// Original PDU sequence number (for response correlation)
    pub sequence_number: u32,
    /// Client system_id
    pub system_id: String,
    /// The submit_sm PDU
    pub submit_sm: SubmitSm,
    /// When the message was received
    pub received_at: Instant,
}

impl MessageEnvelope {
    pub fn new(
        session_id: SessionId,
        sequence_number: u32,
        system_id: String,
        submit_sm: SubmitSm,
    ) -> Self {
        Self {
            message_id: MessageId::new(),
            session_id,
            sequence_number,
            system_id,
            submit_sm,
            received_at: Instant::now(),
        }
    }
}

/// Response envelope for routing responses back to sessions
#[derive(Debug, Clone)]
pub struct ResponseEnvelope {
    /// Target session
    pub session_id: SessionId,
    /// Original sequence number
    pub sequence_number: u32,
    /// Message ID (local or upstream)
    pub message_id: String,
    /// SMPP command status (0 = success)
    pub command_status: u32,
}

impl ResponseEnvelope {
    pub fn success(session_id: SessionId, sequence_number: u32, message_id: String) -> Self {
        Self {
            session_id,
            sequence_number,
            message_id,
            command_status: 0,
        }
    }

    pub fn error(session_id: SessionId, sequence_number: u32, message_id: String, status: u32) -> Self {
        Self {
            session_id,
            sequence_number,
            message_id,
            command_status: status,
        }
    }
}

/// Internal event types for component communication
#[derive(Debug, Clone)]
pub enum Event {
    // === Lifecycle Events ===
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

    // === Listener Events ===
    /// Listener started
    ListenerStarted { name: String, address: String },

    /// Listener stopped
    ListenerStopped { name: String },

    // === Cluster Events ===
    /// Cluster health changed
    ClusterHealthChanged {
        name: String,
        healthy_endpoints: usize,
        total_endpoints: usize,
    },

    /// Endpoint became healthy
    EndpointUp {
        cluster: String,
        endpoint: String,
    },

    /// Endpoint became unhealthy
    EndpointDown {
        cluster: String,
        endpoint: String,
        reason: String,
    },

    /// Circuit breaker opened
    CircuitBreakerOpened {
        cluster: String,
        endpoint: String,
    },

    /// Circuit breaker closed
    CircuitBreakerClosed {
        cluster: String,
        endpoint: String,
    },

    // === Session Events ===
    /// Session registered (ready to receive responses)
    SessionRegistered {
        session_id: SessionId,
        listener: String,
        peer: SocketAddr,
    },

    /// Session unregistered
    SessionUnregistered {
        session_id: SessionId,
    },

    /// Connection opened
    ConnectionOpened { listener: String, peer: String },

    /// Connection closed
    ConnectionClosed { listener: String, peer: String },

    /// Session bound
    SessionBound {
        session_id: SessionId,
        listener: String,
        system_id: String,
        bind_type: String,
    },

    /// Session unbound
    SessionUnbound {
        session_id: SessionId,
        listener: String,
        system_id: String,
    },

    /// Session authentication failed
    SessionAuthFailed {
        listener: String,
        system_id: String,
        reason: String,
    },

    // === Message Flow Events (Event-Driven Pipeline) ===
    /// Message submitted by client - triggers routing
    MessageSubmitRequest(MessageEnvelope),

    /// Message passed filters, ready for routing
    MessageAccepted {
        envelope: MessageEnvelope,
    },

    /// Message rejected by filters
    MessageRejected {
        envelope: MessageEnvelope,
        reason: String,
        status: u32,
    },

    /// Message routed to cluster - triggers forwarding
    MessageRouted {
        envelope: MessageEnvelope,
        route_name: String,
        cluster_name: String,
        fallback_clusters: Vec<String>,
    },

    /// Message forwarding to upstream started
    MessageForwarding {
        message_id: MessageId,
        cluster: String,
        endpoint: String,
        attempt: u32,
    },

    /// Message successfully delivered to upstream
    MessageDelivered {
        message_id: MessageId,
        session_id: SessionId,
        sequence_number: u32,
        smsc_message_id: String,
        latency_us: u64,
    },

    /// Message delivery failed (may retry)
    MessageFailed {
        message_id: MessageId,
        session_id: SessionId,
        sequence_number: u32,
        error_code: Option<u32>,
        reason: String,
        permanent: bool,
    },

    /// Message scheduled for retry
    MessageRetryScheduled {
        message_id: MessageId,
        attempt: u32,
        delay_ms: u64,
        reason: String,
    },

    /// Retry timer fired - triggers re-processing
    MessageRetryReady {
        message_id: MessageId,
    },

    /// Response ready to send to client session
    ResponseReady(ResponseEnvelope),

    // === Legacy Message Events (for metrics/logging) ===
    /// Message received from ESME
    MessageReceived {
        id: MessageId,
        system_id: String,
        destination: String,
    },

    /// Message submitted to upstream
    MessageSubmitted {
        id: MessageId,
        cluster: String,
        endpoint: String,
        attempt: u32,
    },

    /// Message scheduled for retry (legacy)
    MessageRetrying {
        id: MessageId,
        attempt: u32,
        delay_ms: u64,
    },

    /// Delivery receipt received from upstream
    DeliveryReceiptReceived {
        smsc_message_id: String,
        status: String,
        cluster: String,
        endpoint: String,
    },

    /// Delivery receipt ready to forward to client
    DeliveryReceiptReady {
        session_id: SessionId,
        message_id: MessageId,
        status: String,
        error_code: Option<u32>,
    },

    // === Storage Events ===
    /// Storage event (from durable store)
    Storage(StorageEvent),

    // === Metrics Events ===
    /// Throughput snapshot
    ThroughputSnapshot {
        messages_per_second: f64,
        active_connections: u64,
        pending_messages: u64,
    },
}

impl Event {
    /// Get the event category for filtering
    pub fn category(&self) -> EventCategory {
        match self {
            Event::Starting
            | Event::Ready
            | Event::ShutdownStarted
            | Event::DrainStarted
            | Event::ShutdownComplete
            | Event::ConfigReloaded(_) => EventCategory::Lifecycle,

            Event::ListenerStarted { .. } | Event::ListenerStopped { .. } => {
                EventCategory::Listener
            }

            Event::ClusterHealthChanged { .. }
            | Event::EndpointUp { .. }
            | Event::EndpointDown { .. }
            | Event::CircuitBreakerOpened { .. }
            | Event::CircuitBreakerClosed { .. } => EventCategory::Cluster,

            Event::SessionRegistered { .. }
            | Event::SessionUnregistered { .. }
            | Event::ConnectionOpened { .. }
            | Event::ConnectionClosed { .. }
            | Event::SessionBound { .. }
            | Event::SessionUnbound { .. }
            | Event::SessionAuthFailed { .. } => EventCategory::Session,

            Event::MessageSubmitRequest(_)
            | Event::MessageAccepted { .. }
            | Event::MessageRejected { .. }
            | Event::MessageRouted { .. }
            | Event::MessageForwarding { .. }
            | Event::MessageDelivered { .. }
            | Event::MessageFailed { .. }
            | Event::MessageRetryScheduled { .. }
            | Event::MessageRetryReady { .. }
            | Event::ResponseReady(_)
            | Event::MessageReceived { .. }
            | Event::MessageSubmitted { .. }
            | Event::MessageRetrying { .. }
            | Event::DeliveryReceiptReceived { .. }
            | Event::DeliveryReceiptReady { .. } => EventCategory::Message,

            Event::Storage(_) => EventCategory::Storage,

            Event::ThroughputSnapshot { .. } => EventCategory::Metrics,
        }
    }

    /// Check if this is a high-priority event that should not be dropped
    pub fn is_critical(&self) -> bool {
        matches!(
            self,
            Event::ShutdownStarted
                | Event::ConfigReloaded(_)
                | Event::EndpointDown { .. }
                | Event::CircuitBreakerOpened { .. }
                | Event::MessageSubmitRequest(_)
                | Event::ResponseReady(_)
        )
    }
}

/// Event categories for filtering subscriptions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventCategory {
    Lifecycle,
    Listener,
    Cluster,
    Session,
    Message,
    Storage,
    Metrics,
}

/// Event handler trait for reactive components
#[async_trait::async_trait]
pub trait EventHandler: Send + Sync {
    /// Handle an event
    async fn handle(&self, event: &Event);

    /// Categories this handler is interested in
    fn categories(&self) -> &[EventCategory] {
        &[] // Empty means all categories
    }
}

/// Internal event bus for component communication
///
/// Uses broadcast channels to allow multiple subscribers.
/// Components can publish events and subscribe to events they care about.
pub struct Bus {
    /// Main event channel
    tx: broadcast::Sender<Event>,
    /// Registered handlers
    handlers: tokio::sync::RwLock<Vec<Arc<dyn EventHandler>>>,
    /// Event statistics
    stats: EventStats,
}

/// Event statistics
#[derive(Debug, Default)]
pub struct EventStats {
    /// Total events published
    pub published: std::sync::atomic::AtomicU64,
    /// Events dropped (no subscribers)
    pub dropped: std::sync::atomic::AtomicU64,
    /// Events by category
    pub by_category: [std::sync::atomic::AtomicU64; 7],
}

impl Bus {
    /// Create a new event bus
    pub fn new(capacity: usize) -> Arc<Self> {
        let (tx, _) = broadcast::channel(capacity);
        Arc::new(Self {
            tx,
            handlers: tokio::sync::RwLock::new(Vec::new()),
            stats: EventStats::default(),
        })
    }

    /// Publish an event
    pub fn publish(&self, event: Event) {
        use std::sync::atomic::Ordering;

        // Update stats
        self.stats.published.fetch_add(1, Ordering::Relaxed);
        let category_idx = event.category() as usize;
        if category_idx < 7 {
            self.stats.by_category[category_idx].fetch_add(1, Ordering::Relaxed);
        }

        debug!(category = ?event.category(), "publishing event");

        // Send to broadcast channel
        if self.tx.send(event).is_err() {
            self.stats.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Publish a storage event (convenience wrapper)
    pub fn publish_storage(&self, event: StorageEvent) {
        self.publish(Event::Storage(event));
    }

    /// Subscribe to all events
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    /// Subscribe with a filter for specific categories
    pub fn subscribe_filtered(&self, categories: Vec<EventCategory>) -> FilteredSubscription {
        FilteredSubscription {
            rx: self.tx.subscribe(),
            categories,
        }
    }

    /// Register an event handler
    pub async fn register_handler(&self, handler: Arc<dyn EventHandler>) {
        let mut handlers = self.handlers.write().await;
        handlers.push(handler);
    }

    /// Get number of active subscribers
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }

    /// Get event statistics
    pub fn stats(&self) -> &EventStats {
        &self.stats
    }

    /// Spawn a reactor task that dispatches events to handlers
    pub fn spawn_reactor(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let bus = Arc::clone(self);
        let mut rx = self.tx.subscribe();

        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let handlers = bus.handlers.read().await;
                        for handler in handlers.iter() {
                            let categories = handler.categories();
                            // If handler has no categories, it gets all events
                            if categories.is_empty() || categories.contains(&event.category()) {
                                handler.handle(&event).await;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(lagged = n, "event reactor lagged behind");
                    }
                }
            }
        })
    }
}

impl Default for Bus {
    fn default() -> Self {
        let (tx, _) = broadcast::channel(4096);
        Self {
            tx,
            handlers: tokio::sync::RwLock::new(Vec::new()),
            stats: EventStats::default(),
        }
    }
}

/// Filtered subscription that only receives events from specific categories
pub struct FilteredSubscription {
    rx: broadcast::Receiver<Event>,
    categories: Vec<EventCategory>,
}

impl FilteredSubscription {
    /// Receive the next event that matches the filter
    pub async fn recv(&mut self) -> Result<Event, broadcast::error::RecvError> {
        loop {
            let event = self.rx.recv().await?;
            if self.categories.is_empty() || self.categories.contains(&event.category()) {
                return Ok(event);
            }
            // Skip events that don't match
        }
    }

    /// Try to receive an event without blocking
    pub fn try_recv(&mut self) -> Result<Event, broadcast::error::TryRecvError> {
        loop {
            let event = self.rx.try_recv()?;
            if self.categories.is_empty() || self.categories.contains(&event.category()) {
                return Ok(event);
            }
        }
    }
}

/// Shared event bus type
pub type SharedBus = Arc<Bus>;

/// Bridge that forwards storage events to the main event bus
pub struct StorageEventBridge {
    bus: SharedBus,
}

impl StorageEventBridge {
    /// Create a new storage event bridge
    pub fn new(bus: SharedBus) -> Self {
        Self { bus }
    }

    /// Spawn a task that forwards storage events
    pub fn spawn(
        self,
        mut storage_rx: broadcast::Receiver<StorageEvent>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match storage_rx.recv().await {
                    Ok(event) => {
                        self.bus.publish_storage(event);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(lagged = n, "storage event bridge lagged");
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_event_bus() {
        let bus = Bus::new(16);

        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();

        bus.publish(Event::Starting);

        assert!(matches!(rx1.recv().await.unwrap(), Event::Starting));
        assert!(matches!(rx2.recv().await.unwrap(), Event::Starting));
    }

    #[tokio::test]
    async fn test_event_categories() {
        let bus = Bus::new(16);

        let mut cluster_rx = bus.subscribe_filtered(vec![EventCategory::Cluster]);
        let mut session_rx = bus.subscribe_filtered(vec![EventCategory::Session]);

        // Publish cluster event
        bus.publish(Event::EndpointUp {
            cluster: "test".into(),
            endpoint: "127.0.0.1:2775".into(),
        });

        // Publish session event
        bus.publish(Event::SessionBound {
            session_id: SessionId::new(),
            listener: "main".into(),
            system_id: "client1".into(),
            bind_type: "transceiver".into(),
        });

        // Cluster subscriber should get cluster event
        let event = cluster_rx.try_recv().unwrap();
        assert!(matches!(event, Event::EndpointUp { .. }));

        // Session subscriber should get session event
        let event = session_rx.try_recv().unwrap();
        assert!(matches!(event, Event::SessionBound { .. }));
    }

    #[tokio::test]
    async fn test_event_stats() {
        let bus = Bus::new(16);

        let _rx = bus.subscribe();

        bus.publish(Event::Starting);
        bus.publish(Event::Ready);
        bus.publish(Event::EndpointUp {
            cluster: "test".into(),
            endpoint: "127.0.0.1:2775".into(),
        });

        use std::sync::atomic::Ordering;
        assert_eq!(bus.stats().published.load(Ordering::Relaxed), 3);
        assert_eq!(
            bus.stats().by_category[EventCategory::Lifecycle as usize].load(Ordering::Relaxed),
            2
        );
        assert_eq!(
            bus.stats().by_category[EventCategory::Cluster as usize].load(Ordering::Relaxed),
            1
        );
    }

    struct TestHandler {
        received: tokio::sync::Mutex<Vec<EventCategory>>,
    }

    #[async_trait::async_trait]
    impl EventHandler for TestHandler {
        async fn handle(&self, event: &Event) {
            let mut received = self.received.lock().await;
            received.push(event.category());
        }

        fn categories(&self) -> &[EventCategory] {
            &[EventCategory::Lifecycle]
        }
    }

    #[tokio::test]
    async fn test_event_handler() {
        let bus = Bus::new(16);

        let handler = Arc::new(TestHandler {
            received: tokio::sync::Mutex::new(Vec::new()),
        });

        bus.register_handler(handler.clone()).await;

        // Spawn reactor
        let _reactor = bus.spawn_reactor();

        // Publish events
        bus.publish(Event::Starting);
        bus.publish(Event::EndpointUp {
            cluster: "test".into(),
            endpoint: "127.0.0.1:2775".into(),
        });

        // Give reactor time to process
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Handler should only receive lifecycle events
        let received = handler.received.lock().await;
        assert_eq!(received.len(), 1);
        assert_eq!(received[0], EventCategory::Lifecycle);
    }
}
