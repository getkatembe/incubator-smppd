use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, watch};
use tracing::{info, warn};

/// Shutdown state machine (Envoy-inspired)
///
/// States:
/// 1. Running - normal operation
/// 2. Draining - stop accepting new connections, drain existing
/// 3. Terminated - all connections closed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Running,
    Draining,
    Terminated,
}

/// Graceful shutdown coordinator
///
/// Manages the shutdown lifecycle with configurable drain period.
/// Tracks active connections and coordinates orderly termination.
pub struct Shutdown {
    state: watch::Sender<State>,
    drain_timeout: Duration,
    active_connections: AtomicU64,
    complete_tx: broadcast::Sender<()>,
}

impl Shutdown {
    /// Create a new shutdown coordinator
    pub fn new(drain_timeout: Duration) -> Arc<Self> {
        let (state, _) = watch::channel(State::Running);
        let (complete_tx, _) = broadcast::channel(1);

        Arc::new(Self {
            state,
            drain_timeout,
            active_connections: AtomicU64::new(0),
            complete_tx,
        })
    }

    /// Get current state
    pub fn state(&self) -> State {
        *self.state.borrow()
    }

    /// Subscribe to state changes
    pub fn subscribe(&self) -> watch::Receiver<State> {
        self.state.subscribe()
    }

    /// Subscribe to shutdown complete
    pub fn complete_signal(&self) -> broadcast::Receiver<()> {
        self.complete_tx.subscribe()
    }

    /// Get drain timeout
    pub fn drain_timeout(&self) -> Duration {
        self.drain_timeout
    }

    /// Start draining (called on SIGTERM/SIGINT)
    pub fn start_drain(&self) {
        if self.state() != State::Running {
            return;
        }

        info!(
            drain_timeout_secs = self.drain_timeout.as_secs(),
            active_connections = self.active_connections(),
            "starting graceful shutdown drain"
        );

        self.state.send_replace(State::Draining);
    }

    /// Complete shutdown
    pub fn terminate(&self) {
        if self.state() == State::Terminated {
            return;
        }

        let active = self.active_connections();
        if active > 0 {
            warn!(
                active_connections = active,
                "force terminating with active connections"
            );
        }

        info!("shutdown complete");
        self.state.send_replace(State::Terminated);
        let _ = self.complete_tx.send(());
    }

    /// Register a new connection (returns false if draining/terminated)
    pub fn connection_opened(&self) -> bool {
        if self.state() != State::Running {
            return false;
        }

        self.active_connections.fetch_add(1, Ordering::SeqCst);
        true
    }

    /// Unregister a connection
    pub fn connection_closed(&self) {
        let prev = self.active_connections.fetch_sub(1, Ordering::SeqCst);

        // If draining and no more connections, complete
        if self.state() == State::Draining && prev == 1 {
            self.terminate();
        }
    }

    /// Get active connection count
    pub fn active_connections(&self) -> u64 {
        self.active_connections.load(Ordering::SeqCst)
    }

    /// Check if accepting new connections
    pub fn is_accepting(&self) -> bool {
        self.state() == State::Running
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shutdown_state_machine() {
        let shutdown = Shutdown::new(Duration::from_secs(30));

        assert_eq!(shutdown.state(), State::Running);
        assert!(shutdown.is_accepting());

        // Open connection
        assert!(shutdown.connection_opened());
        assert_eq!(shutdown.active_connections(), 1);

        // Start drain
        shutdown.start_drain();
        assert_eq!(shutdown.state(), State::Draining);
        assert!(!shutdown.is_accepting());

        // New connections rejected during drain
        assert!(!shutdown.connection_opened());

        // Close connection triggers terminate
        shutdown.connection_closed();
        assert_eq!(shutdown.state(), State::Terminated);
    }
}
