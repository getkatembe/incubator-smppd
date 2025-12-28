//! Connection handling for SMPP sessions.

use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use tokio::net::TcpStream;
use tokio::sync::{Mutex, OwnedSemaphorePermit, RwLock};
use tokio_rustls::server::TlsStream;
use tracing::debug;

/// Unique connection identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId(pub u64);

impl fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Connection established, awaiting bind
    Open,
    /// Bound as transmitter
    BoundTx,
    /// Bound as receiver
    BoundRx,
    /// Bound as transceiver
    BoundTrx,
    /// Unbind in progress
    Unbinding,
    /// Connection closed
    Closed,
}

impl fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectionState::Open => write!(f, "OPEN"),
            ConnectionState::BoundTx => write!(f, "BOUND_TX"),
            ConnectionState::BoundRx => write!(f, "BOUND_RX"),
            ConnectionState::BoundTrx => write!(f, "BOUND_TRX"),
            ConnectionState::Unbinding => write!(f, "UNBINDING"),
            ConnectionState::Closed => write!(f, "CLOSED"),
        }
    }
}

impl ConnectionState {
    /// Check if this state allows receiving messages.
    pub fn can_receive(&self) -> bool {
        matches!(self, Self::BoundRx | Self::BoundTrx)
    }

    /// Check if this state allows sending messages.
    pub fn can_send(&self) -> bool {
        matches!(self, Self::BoundTx | Self::BoundTrx)
    }
}

/// Stream type for the connection.
pub enum Stream {
    /// Plain TCP connection
    Plain(TcpStream),
    /// TLS-encrypted connection
    Tls(Box<TlsStream<TcpStream>>),
}

impl Stream {
    /// Get the underlying TcpStream for socket options.
    pub fn tcp_stream(&self) -> Option<&TcpStream> {
        match self {
            Stream::Plain(s) => Some(s),
            Stream::Tls(s) => Some(s.get_ref().0),
        }
    }
}

/// An SMPP connection.
pub struct Connection {
    /// Connection ID
    id: ConnectionId,

    /// Listener name this connection belongs to
    listener: String,

    /// Peer address
    peer_addr: SocketAddr,

    /// Connection state
    state: RwLock<ConnectionState>,

    /// System ID (set after bind)
    system_id: RwLock<Option<String>>,

    /// Sequence number generator
    sequence: AtomicU32,

    /// Underlying stream (taken by session)
    stream: Mutex<Option<Stream>>,

    /// Idle timeout
    idle_timeout: Duration,

    /// Request timeout
    request_timeout: Duration,

    /// Last activity timestamp
    last_activity: RwLock<Instant>,

    /// Close flag
    closing: AtomicBool,

    /// Connection permit (released on drop)
    _permit: OwnedSemaphorePermit,

    /// Creation timestamp
    created_at: Instant,
}

impl Connection {
    /// Create a new connection.
    pub fn new(
        id: ConnectionId,
        listener: String,
        peer_addr: SocketAddr,
        stream: Stream,
        idle_timeout: Duration,
        request_timeout: Duration,
        permit: OwnedSemaphorePermit,
    ) -> Self {
        Self {
            id,
            listener,
            peer_addr,
            state: RwLock::new(ConnectionState::Open),
            system_id: RwLock::new(None),
            sequence: AtomicU32::new(1),
            stream: Mutex::new(Some(stream)),
            idle_timeout,
            request_timeout,
            last_activity: RwLock::new(Instant::now()),
            closing: AtomicBool::new(false),
            _permit: permit,
            created_at: Instant::now(),
        }
    }

    /// Get connection ID.
    pub fn id(&self) -> ConnectionId {
        self.id
    }

    /// Get listener name.
    pub fn listener(&self) -> &str {
        &self.listener
    }

    /// Get peer address.
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }

    /// Get connection state.
    pub async fn state(&self) -> ConnectionState {
        *self.state.read().await
    }

    /// Set connection state.
    pub async fn set_state(&self, state: ConnectionState) {
        let from = self.state().await;
        debug!(id = %self.id, from = ?from, to = ?state, "state transition");
        *self.state.write().await = state;
    }

    /// Get system ID (after bind).
    pub async fn system_id(&self) -> Option<String> {
        self.system_id.read().await.clone()
    }

    /// Set system ID.
    pub async fn set_system_id(&self, system_id: String) {
        *self.system_id.write().await = Some(system_id);
    }

    /// Generate next sequence number.
    pub fn next_sequence(&self) -> u32 {
        self.sequence.fetch_add(1, Ordering::SeqCst)
    }

    /// Take the underlying stream (for session to use).
    pub async fn take_stream(&self) -> Option<Stream> {
        self.stream.lock().await.take()
    }

    /// Check if connection is bound.
    pub async fn is_bound(&self) -> bool {
        matches!(
            self.state().await,
            ConnectionState::BoundTx | ConnectionState::BoundRx | ConnectionState::BoundTrx
        )
    }

    /// Check if connection is closing.
    pub fn is_closing(&self) -> bool {
        self.closing.load(Ordering::Relaxed)
    }

    /// Update last activity timestamp.
    pub async fn touch(&self) {
        *self.last_activity.write().await = Instant::now();
    }

    /// Get time since last activity.
    pub async fn idle_time(&self) -> Duration {
        self.last_activity.read().await.elapsed()
    }

    /// Check if connection is idle.
    pub async fn is_idle(&self) -> bool {
        self.idle_time().await > self.idle_timeout
    }

    /// Get idle timeout.
    pub fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    /// Get request timeout.
    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    /// Get connection uptime.
    pub fn uptime(&self) -> Duration {
        self.created_at.elapsed()
    }

    /// Initiate connection close.
    pub async fn initiate_close(&self) {
        self.closing.store(true, Ordering::SeqCst);
        self.set_state(ConnectionState::Closed).await;
    }
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connection")
            .field("id", &self.id)
            .field("listener", &self.listener)
            .field("peer_addr", &self.peer_addr)
            .finish()
    }
}
