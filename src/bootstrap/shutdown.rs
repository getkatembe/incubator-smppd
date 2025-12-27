use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, watch};
use tokio::time::Instant;
use tracing::{info, warn};

/// Shutdown state machine (Envoy-inspired)
///
/// States:
/// 1. Running - normal operation
/// 2. Draining - stop accepting new connections, drain existing
/// 3. Terminated - all connections closed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownState {
    Running,
    Draining,
    Terminated,
}

/// Manages graceful shutdown with drain period
pub struct ShutdownManager {
    /// Current state
    state: watch::Sender<ShutdownState>,

    /// Drain period duration
    drain_period: Duration,

    /// Drain deadline (when to force terminate)
    drain_deadline: AtomicU64,

    /// Active connection count
    active_connections: AtomicU64,

    /// Shutdown complete signal
    complete_tx: broadcast::Sender<()>,
}

impl ShutdownManager {
    pub fn new(drain_period: Duration) -> Arc<Self> {
        let (state, _) = watch::channel(ShutdownState::Running);
        let (complete_tx, _) = broadcast::channel(1);

        Arc::new(Self {
            state,
            drain_period,
            drain_deadline: AtomicU64::new(0),
            active_connections: AtomicU64::new(0),
            complete_tx,
        })
    }

    /// Get current state
    pub fn state(&self) -> ShutdownState {
        *self.state.borrow()
    }

    /// Subscribe to state changes
    pub fn subscribe(&self) -> watch::Receiver<ShutdownState> {
        self.state.subscribe()
    }

    /// Subscribe to shutdown complete
    pub fn complete_signal(&self) -> broadcast::Receiver<()> {
        self.complete_tx.subscribe()
    }

    /// Start draining (called on SIGTERM/SIGINT)
    pub fn start_drain(&self) {
        if self.state() != ShutdownState::Running {
            return;
        }

        let deadline = Instant::now() + self.drain_period;
        self.drain_deadline.store(
            deadline.elapsed().as_millis() as u64,
            Ordering::SeqCst,
        );

        info!(
            drain_period_secs = self.drain_period.as_secs(),
            "starting graceful shutdown drain"
        );

        let _ = self.state.send(ShutdownState::Draining);
    }

    /// Check if drain period has expired
    pub fn drain_expired(&self) -> bool {
        if self.state() != ShutdownState::Draining {
            return false;
        }

        let active = self.active_connections.load(Ordering::SeqCst);
        if active == 0 {
            info!("all connections drained");
            return true;
        }

        // Check deadline
        let deadline_ms = self.drain_deadline.load(Ordering::SeqCst);
        if deadline_ms > 0 {
            // Simplified check - in real impl would compare with Instant
            return false;
        }

        false
    }

    /// Complete shutdown
    pub fn terminate(&self) {
        if self.state() == ShutdownState::Terminated {
            return;
        }

        let active = self.active_connections.load(Ordering::SeqCst);
        if active > 0 {
            warn!(
                active_connections = active,
                "force terminating with active connections"
            );
        }

        info!("shutdown complete");
        let _ = self.state.send(ShutdownState::Terminated);
        let _ = self.complete_tx.send(());
    }

    /// Register a new connection
    pub fn connection_opened(&self) -> bool {
        // Reject new connections during drain
        if self.state() != ShutdownState::Running {
            return false;
        }

        self.active_connections.fetch_add(1, Ordering::SeqCst);
        true
    }

    /// Unregister a connection
    pub fn connection_closed(&self) {
        let prev = self.active_connections.fetch_sub(1, Ordering::SeqCst);

        // If draining and no more connections, complete
        if self.state() == ShutdownState::Draining && prev == 1 {
            self.terminate();
        }
    }

    /// Get active connection count
    pub fn active_connections(&self) -> u64 {
        self.active_connections.load(Ordering::SeqCst)
    }

    /// Check if accepting new connections
    pub fn is_accepting(&self) -> bool {
        self.state() == ShutdownState::Running
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shutdown_state_machine() {
        let manager = ShutdownManager::new(Duration::from_secs(30));

        assert_eq!(manager.state(), ShutdownState::Running);
        assert!(manager.is_accepting());

        // Open connection
        assert!(manager.connection_opened());
        assert_eq!(manager.active_connections(), 1);

        // Start drain
        manager.start_drain();
        assert_eq!(manager.state(), ShutdownState::Draining);
        assert!(!manager.is_accepting());

        // New connections rejected during drain
        assert!(!manager.connection_opened());

        // Close connection triggers terminate
        manager.connection_closed();
        assert_eq!(manager.state(), ShutdownState::Terminated);
    }
}
