//! Connection pool for upstream SMSC connections.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use smpp::Client;
use tokio::sync::{Mutex, Semaphore, OwnedSemaphorePermit};
use tracing::{debug, error, info, trace, warn};

use crate::config::PoolConfig;
use crate::telemetry::counters;

/// A pooled connection to an upstream SMSC.
pub struct PooledConnection {
    /// The underlying SMPP client
    client: Client,

    /// Pool this connection belongs to
    pool: Arc<ConnectionPoolInner>,

    /// Connection acquired time
    acquired_at: Instant,

    /// Permit for this connection
    _permit: OwnedSemaphorePermit,
}

impl PooledConnection {
    /// Get the underlying client.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Get mutable reference to the underlying client.
    pub fn client_mut(&mut self) -> &mut Client {
        &mut self.client
    }

    /// Get time since connection was acquired.
    pub fn held_duration(&self) -> Duration {
        self.acquired_at.elapsed()
    }
}

impl Drop for PooledConnection {
    fn drop(&mut self) {
        // Connection is returned to pool automatically when dropped
        // The permit is released, allowing another connection to be created
        self.pool.connections_in_use.fetch_sub(1, Ordering::SeqCst);
        trace!(
            pool = %self.pool.address,
            "connection returned to pool"
        );
    }
}

/// Inner pool state.
struct ConnectionPoolInner {
    /// Cluster name
    cluster: String,

    /// Endpoint address
    address: String,

    /// SMPP credentials
    system_id: String,
    password: String,

    /// Pool configuration
    min_connections: usize,
    max_connections: usize,
    acquire_timeout: Duration,

    /// Connection semaphore
    semaphore: Arc<Semaphore>,

    /// Idle connections
    idle: Mutex<VecDeque<Client>>,

    /// Connections currently in use
    connections_in_use: AtomicUsize,

    /// Total connections created
    connections_created: AtomicUsize,
}

/// Connection pool for an endpoint.
pub struct ConnectionPool {
    inner: Arc<ConnectionPoolInner>,
}

impl ConnectionPool {
    /// Create a new connection pool.
    pub fn new(
        cluster: &str,
        address: &str,
        system_id: &str,
        password: &str,
        config: &PoolConfig,
    ) -> Self {
        Self {
            inner: Arc::new(ConnectionPoolInner {
                cluster: cluster.to_string(),
                address: address.to_string(),
                system_id: system_id.to_string(),
                password: password.to_string(),
                min_connections: config.min_connections,
                max_connections: config.max_connections,
                acquire_timeout: config.acquire_timeout,
                semaphore: Arc::new(Semaphore::new(config.max_connections)),
                idle: Mutex::new(VecDeque::new()),
                connections_in_use: AtomicUsize::new(0),
                connections_created: AtomicUsize::new(0),
            }),
        }
    }

    /// Get a connection from the pool.
    pub async fn get(&self) -> Option<PooledConnection> {
        // Try to acquire a permit with timeout
        let permit = match tokio::time::timeout(
            self.inner.acquire_timeout,
            self.inner.semaphore.clone().acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => {
                error!(
                    pool = %self.inner.address,
                    "semaphore closed"
                );
                return None;
            }
            Err(_) => {
                warn!(
                    pool = %self.inner.address,
                    "connection acquire timeout"
                );
                counters::upstream_pool_timeout(&self.inner.cluster);
                return None;
            }
        };

        // Try to get an idle connection
        let client = {
            let mut idle = self.inner.idle.lock().await;
            idle.pop_front()
        };

        let client = match client {
            Some(client) => {
                trace!(
                    pool = %self.inner.address,
                    "reusing idle connection"
                );
                client
            }
            None => {
                // Create new connection
                match self.create_connection().await {
                    Ok(client) => client,
                    Err(e) => {
                        error!(
                            pool = %self.inner.address,
                            error = %e,
                            "failed to create connection"
                        );
                        return None;
                    }
                }
            }
        };

        self.inner.connections_in_use.fetch_add(1, Ordering::SeqCst);

        Some(PooledConnection {
            client,
            pool: self.inner.clone(),
            acquired_at: Instant::now(),
            _permit: permit,
        })
    }

    /// Create a new connection.
    async fn create_connection(&self) -> Result<Client, smpp::Error> {
        debug!(
            pool = %self.inner.address,
            "creating new connection"
        );

        let client = Client::new(&self.inner.address)
            .auth((&self.inner.system_id, &self.inner.password))
            .connect()
            .await?;

        self.inner.connections_created.fetch_add(1, Ordering::SeqCst);
        counters::upstream_connection_created(&self.inner.cluster);

        info!(
            pool = %self.inner.address,
            total_created = self.inner.connections_created.load(Ordering::Relaxed),
            "connection created"
        );

        Ok(client)
    }

    /// Get current pool size.
    pub fn size(&self) -> usize {
        self.inner.max_connections - self.inner.semaphore.available_permits()
    }

    /// Get number of connections in use.
    pub fn in_use(&self) -> usize {
        self.inner.connections_in_use.load(Ordering::Relaxed)
    }

    /// Get number of idle connections.
    pub async fn idle_count(&self) -> usize {
        self.inner.idle.lock().await.len()
    }

    /// Get total connections created.
    pub fn total_created(&self) -> usize {
        self.inner.connections_created.load(Ordering::Relaxed)
    }

    /// Pre-warm the pool with min_connections.
    pub async fn warm(&self) {
        let min = self.inner.min_connections;
        if min == 0 {
            return;
        }

        info!(
            pool = %self.inner.address,
            min_connections = min,
            "warming connection pool"
        );

        for _ in 0..min {
            match self.create_connection().await {
                Ok(client) => {
                    let mut idle = self.inner.idle.lock().await;
                    idle.push_back(client);
                }
                Err(e) => {
                    warn!(
                        pool = %self.inner.address,
                        error = %e,
                        "failed to warm pool"
                    );
                    break;
                }
            }
        }
    }
}
