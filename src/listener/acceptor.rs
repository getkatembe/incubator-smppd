//! TCP/TLS acceptor for incoming connections.
//!
//! Event-driven: sessions register with SessionRegistry and use EventBus.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, RwLock, Semaphore};
use tokio_rustls::TlsAcceptor;
use tracing::{debug, error, info, warn, span, Level, Instrument};

use crate::bootstrap::{Event, SessionId, SharedBus, Shutdown};
use crate::config::{ListenerConfig, TlsConfig};
use crate::telemetry::counters;

use super::connection::{Connection, ConnectionId, Stream};
use super::registry::SharedSessionRegistry;
use super::session::SmppSession;

/// Listener for accepting incoming SMPP connections.
pub struct Listener {
    /// Listener name (for logging/metrics)
    name: String,

    /// Bind address
    address: SocketAddr,

    /// TLS acceptor (if TLS enabled)
    tls_acceptor: Option<TlsAcceptor>,

    /// Connection semaphore (limits max connections)
    connection_limit: Arc<Semaphore>,

    /// Connection ID generator
    next_connection_id: AtomicU64,

    /// Active connections
    connections: Arc<RwLock<HashMap<ConnectionId, Arc<Connection>>>>,

    /// Active connection count
    active_connections: Arc<AtomicUsize>,

    /// Idle timeout
    idle_timeout: Duration,

    /// Request timeout
    request_timeout: Duration,

    /// Shutdown handle
    shutdown: Arc<Shutdown>,

    /// New connection sender
    connection_tx: mpsc::Sender<Arc<Connection>>,

    /// Event bus for event-driven processing
    bus: SharedBus,

    /// Session registry for response routing
    session_registry: SharedSessionRegistry,
}

impl Listener {
    /// Create a new listener from config.
    pub fn new(
        config: &ListenerConfig,
        shutdown: Arc<Shutdown>,
        connection_tx: mpsc::Sender<Arc<Connection>>,
        bus: SharedBus,
        session_registry: SharedSessionRegistry,
    ) -> io::Result<Self> {
        let tls_acceptor = if let Some(tls_config) = &config.tls {
            Some(build_tls_acceptor(tls_config)?)
        } else {
            None
        };

        Ok(Self {
            name: config.name.clone(),
            address: config.address,
            tls_acceptor,
            connection_limit: Arc::new(Semaphore::new(config.limits.max_connections)),
            next_connection_id: AtomicU64::new(1),
            connections: Arc::new(RwLock::new(HashMap::new())),
            active_connections: Arc::new(AtomicUsize::new(0)),
            idle_timeout: config.limits.idle_timeout,
            request_timeout: config.limits.request_timeout,
            shutdown,
            connection_tx,
            bus,
            session_registry,
        })
    }

    /// Get listener name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get bind address.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// Get active connection count.
    pub fn active_connections(&self) -> usize {
        self.active_connections.load(Ordering::Relaxed)
    }

    /// Run the listener accept loop.
    pub async fn run(self: Arc<Self>) -> io::Result<()> {
        let listener = TcpListener::bind(self.address).await?;

        info!(
            listener = %self.name,
            address = %self.address,
            tls = self.tls_acceptor.is_some(),
            max_connections = self.connection_limit.available_permits(),
            "listener started"
        );

        counters::listener_started(&self.name);

        let mut shutdown_rx = self.shutdown.subscribe();

        loop {
            tokio::select! {
                biased;

                _ = shutdown_rx.changed() => {
                    info!(listener = %self.name, "listener shutting down");
                    break;
                }

                result = listener.accept() => {
                    match result {
                        Ok((stream, peer_addr)) => {
                            self.clone().handle_accept(stream, peer_addr).await;
                        }
                        Err(e) => {
                            error!(
                                listener = %self.name,
                                error = %e,
                                "accept error"
                            );
                            counters::listener_accept_error(&self.name);
                        }
                    }
                }
            }
        }

        // Drain existing connections
        self.drain_connections().await;

        info!(listener = %self.name, "listener stopped");
        Ok(())
    }

    /// Handle an accepted connection.
    async fn handle_accept(self: Arc<Self>, stream: TcpStream, peer_addr: SocketAddr) {
        // Try to acquire connection permit
        let permit = match self.connection_limit.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                warn!(
                    listener = %self.name,
                    peer = %peer_addr,
                    "connection limit reached, rejecting"
                );
                counters::listener_connection_rejected(&self.name, "limit");
                return;
            }
        };

        // Generate connection ID
        let conn_id = ConnectionId(self.next_connection_id.fetch_add(1, Ordering::SeqCst));

        let span = span!(
            Level::INFO,
            "conn",
            listener = %self.name,
            id = %conn_id,
            peer = %peer_addr
        );

        // Configure socket
        if let Err(e) = configure_socket(&stream) {
            error!(parent: &span, error = %e, "socket configuration failed");
            return;
        }

        // Handle TLS if configured
        let stream = if let Some(tls_acceptor) = &self.tls_acceptor {
            match tokio::time::timeout(
                Duration::from_secs(10),
                tls_acceptor.accept(stream),
            )
            .await
            {
                Ok(Ok(tls_stream)) => {
                    debug!(parent: &span, "TLS handshake completed");
                    counters::tls_handshake_success(&self.name);
                    Stream::Tls(Box::new(tls_stream))
                }
                Ok(Err(e)) => {
                    warn!(parent: &span, error = %e, "TLS handshake failed");
                    counters::tls_handshake_error(&self.name);
                    return;
                }
                Err(_) => {
                    warn!(parent: &span, "TLS handshake timeout");
                    counters::tls_handshake_error(&self.name);
                    return;
                }
            }
        } else {
            Stream::Plain(stream)
        };

        debug!(parent: &span, "connection accepted");
        counters::listener_connection_accepted(&self.name);

        // Track connection
        self.active_connections.fetch_add(1, Ordering::SeqCst);

        // Create connection
        let connection = Arc::new(Connection::new(
            conn_id,
            self.name.clone(),
            peer_addr,
            stream,
            self.idle_timeout,
            self.request_timeout,
            permit,
        ));

        // Store connection
        {
            let mut connections = self.connections.write().await;
            connections.insert(conn_id, connection.clone());
        }

        // Notify of new connection
        if let Err(e) = self.connection_tx.send(connection.clone()).await {
            error!(parent: &span, error = %e, "failed to send connection to handler");
        }

        // Spawn connection handler
        let listener = self.clone();
        let bus = self.bus.clone();
        let session_registry = self.session_registry.clone();
        let listener_name = self.name.clone();

        tokio::spawn(
            async move {
                // Generate session ID and register with session registry
                let session_id = SessionId::new();
                let response_rx = session_registry
                    .register(session_id, listener_name.clone())
                    .await;

                // Publish session registered event
                bus.publish(Event::SessionRegistered {
                    session_id,
                    listener: listener_name.clone(),
                    peer: peer_addr,
                });

                // Create SMPP session for this connection
                let mut session = SmppSession::new(
                    session_id,
                    connection.clone(),
                    bus.clone(),
                    response_rx,
                );

                // Run session until completion
                if let Err(e) = session.run().await {
                    debug!(error = %e, "session ended with error");
                }

                // Unregister session
                session_registry.unregister(session_id).await;
                bus.publish(Event::SessionUnregistered { session_id });

                // Remove from active connections
                {
                    let mut connections = listener.connections.write().await;
                    connections.remove(&conn_id);
                }

                listener.active_connections.fetch_sub(1, Ordering::SeqCst);
                counters::listener_connection_closed(&listener.name);
            }
            .instrument(span),
        );
    }

    /// Drain all active connections.
    async fn drain_connections(&self) {
        let connections: Vec<Arc<Connection>> = {
            let conns = self.connections.read().await;
            conns.values().cloned().collect()
        };

        if connections.is_empty() {
            return;
        }

        info!(
            listener = %self.name,
            count = connections.len(),
            "draining connections"
        );

        for conn in connections {
            conn.initiate_close().await;
        }
    }
}

/// Configure TCP socket options.
fn configure_socket(stream: &TcpStream) -> io::Result<()> {
    stream.set_nodelay(true)?;
    Ok(())
}

/// Build TLS acceptor from config.
fn build_tls_acceptor(config: &TlsConfig) -> io::Result<TlsAcceptor> {
    use std::fs::File;
    use std::io::BufReader;
    use tokio_rustls::rustls::{self};

    // Load certificate chain
    let cert_file = File::open(&config.cert)?;
    let mut cert_reader = BufReader::new(cert_file);
    let certs: Vec<_> = rustls_pemfile::certs(&mut cert_reader)
        .collect::<Result<_, _>>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    // Load private key
    let key_file = File::open(&config.key)?;
    let mut key_reader = BufReader::new(key_file);
    let key = rustls_pemfile::private_key(&mut key_reader)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no private key found"))?;

    // Build server config
    let server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    Ok(TlsAcceptor::from(Arc::new(server_config)))
}

/// Manager for all listeners.
pub struct Listeners {
    listeners: Vec<Arc<Listener>>,
    connection_rx: mpsc::Receiver<Arc<Connection>>,
}

impl Listeners {
    /// Create listeners from config.
    pub fn new(
        configs: &[ListenerConfig],
        shutdown: Arc<Shutdown>,
        bus: SharedBus,
        session_registry: SharedSessionRegistry,
    ) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel(1024);
        let mut listeners = Vec::with_capacity(configs.len());

        for config in configs {
            let listener = Listener::new(
                config,
                shutdown.clone(),
                tx.clone(),
                bus.clone(),
                session_registry.clone(),
            )?;
            listeners.push(Arc::new(listener));
        }

        Ok(Self {
            listeners,
            connection_rx: rx,
        })
    }

    /// Start all listeners.
    pub fn start(&self) {
        for listener in &self.listeners {
            let listener = listener.clone();
            let name = listener.name().to_string();
            tokio::spawn(async move {
                if let Err(e) = listener.run().await {
                    error!(
                        listener = %name,
                        error = %e,
                        "listener failed"
                    );
                }
            });
        }
    }

    /// Receive next connection.
    pub async fn accept(&mut self) -> Option<Arc<Connection>> {
        self.connection_rx.recv().await
    }

    /// Get listener count.
    pub fn len(&self) -> usize {
        self.listeners.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.listeners.is_empty()
    }
}
