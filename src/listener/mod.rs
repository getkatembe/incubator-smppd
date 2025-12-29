//! Listener module for accepting incoming SMPP connections.
//!
//! Envoy-inspired architecture:
//! - Listeners bind to addresses and accept connections
//! - Filter chains process connections
//! - Connections are assigned to worker threads
//! - Sessions are registered for event-driven response routing

mod acceptor;
mod connection;
mod registry;
mod session;

pub use acceptor::{Listener, Listeners};
pub use connection::{Connection, ConnectionId};
pub use registry::{SessionRegistry, SharedSessionRegistry};
pub use session::{SmppSession, SessionState};
