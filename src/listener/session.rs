//! SMPP session state machine.
//!
//! Handles the SMPP protocol state transitions and PDU processing.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use smpp::codec::{PduFrame, SmppCodec};
use smpp::pdu::{
    BindTransceiver, BindTransceiverResp, BindTransmitter, BindTransmitterResp,
    BindReceiver, BindReceiverResp, BindRespFields, Command, EnquireLink, EnquireLinkResp,
    GenericNack, Header, Pdu, Status, SubmitSm, SubmitSmResp, UnbindResp,
};
use smpp::TlvMap;
use smpp::Error as SmppError;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_util::codec::Framed;
use tracing::{debug, error, info, trace, warn};

use crate::forwarder::{ForwarderHandle, ForwardRequest};
use crate::telemetry::counters;

use super::connection::{Connection, ConnectionState, Stream};

/// Session error types.
#[derive(Debug, Error)]
pub enum SessionError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("SMPP error: {0}")]
    Smpp(#[from] SmppError),

    #[error("Timeout waiting for response")]
    Timeout,

    #[error("Invalid state for operation: {0}")]
    InvalidState(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Connection closed")]
    Closed,
}

/// Session state (mirrors ConnectionState but for protocol level).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Open,
    BoundTx,
    BoundRx,
    BoundTrx,
    Unbinding,
    Closed,
}

impl From<ConnectionState> for SessionState {
    fn from(state: ConnectionState) -> Self {
        match state {
            ConnectionState::Open => SessionState::Open,
            ConnectionState::BoundTx => SessionState::BoundTx,
            ConnectionState::BoundRx => SessionState::BoundRx,
            ConnectionState::BoundTrx => SessionState::BoundTrx,
            ConnectionState::Unbinding => SessionState::Unbinding,
            ConnectionState::Closed => SessionState::Closed,
        }
    }
}

/// SMPP session handler.
pub struct SmppSession {
    /// Underlying connection
    connection: Arc<Connection>,

    /// Forwarder handle for sending messages to upstream
    forwarder: ForwarderHandle,

    /// Pending requests awaiting response
    pending: HashMap<u32, PendingRequest>,

    /// EnquireLink interval (will be used for configurable keepalive)
    #[allow(dead_code)]
    enquire_link_interval: Duration,

    /// Last enquire_link sent
    last_enquire_link: Option<Instant>,
}

struct PendingRequest {
    command: Command,
    sent_at: Instant,
    response_tx: Option<oneshot::Sender<Result<PduFrame, SessionError>>>,
}

impl SmppSession {
    /// Create a new session for a connection.
    pub fn new(connection: Arc<Connection>, forwarder: ForwarderHandle) -> Self {
        Self {
            connection,
            forwarder,
            pending: HashMap::new(),
            enquire_link_interval: Duration::from_secs(30),
            last_enquire_link: None,
        }
    }

    /// Run the session until completion.
    pub async fn run(&mut self) -> Result<(), SessionError> {
        // Take the stream from the connection
        let stream = self
            .connection
            .take_stream()
            .await
            .ok_or_else(|| SessionError::Protocol("stream already taken".into()))?;

        // Create framed codec
        match stream {
            Stream::Plain(tcp) => {
                let mut framed = Framed::new(tcp, SmppCodec::new());
                self.run_loop(&mut framed).await
            }
            Stream::Tls(tls) => {
                let mut framed = Framed::new(*tls, SmppCodec::new());
                self.run_loop(&mut framed).await
            }
        }
    }

    /// Main session loop.
    async fn run_loop<T>(&mut self, framed: &mut Framed<T, SmppCodec>) -> Result<(), SessionError>
    where
        T: AsyncRead + AsyncWrite + Unpin,
    {
        let idle_timeout = self.connection.idle_timeout();
        let request_timeout = self.connection.request_timeout();

        loop {
            // Check if closing
            if self.connection.is_closing() {
                debug!("session closing");
                break;
            }

            tokio::select! {
                // Read incoming PDU
                result = timeout(idle_timeout, framed.next()) => {
                    match result {
                        Ok(Some(Ok(frame))) => {
                            self.connection.touch().await;
                            counters::pdu_received(
                                self.connection.listener(),
                                &format!("{:?}", frame.command()),
                            );

                            if let Err(e) = self.handle_pdu(framed, frame).await {
                                error!(error = %e, "PDU handling error");
                                break;
                            }
                        }
                        Ok(Some(Err(e))) => {
                            error!(error = %e, "decode error");
                            counters::pdu_error(self.connection.listener(), "decode");
                            break;
                        }
                        Ok(None) => {
                            debug!("connection closed by peer");
                            break;
                        }
                        Err(_) => {
                            // Idle timeout - send enquire_link if bound
                            if self.connection.is_bound().await {
                                if let Err(e) = self.send_enquire_link(framed).await {
                                    warn!(error = %e, "enquire_link failed");
                                    break;
                                }
                            } else {
                                debug!("idle timeout before bind");
                                break;
                            }
                        }
                    }
                }

                // Check for pending request timeouts
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    self.check_pending_timeouts(request_timeout);
                }
            }
        }

        // Clean up
        self.connection.set_state(ConnectionState::Closed).await;
        Ok(())
    }

    /// Handle an incoming PDU.
    async fn handle_pdu<T>(
        &mut self,
        framed: &mut Framed<T, SmppCodec>,
        frame: PduFrame,
    ) -> Result<(), SessionError>
    where
        T: AsyncRead + AsyncWrite + Unpin,
    {
        let state = self.connection.state().await;

        // Handle response PDUs (match with pending requests)
        if frame.is_response() {
            return self.handle_response(frame).await;
        }

        // Extract sequence and header before consuming pdu
        let sequence = frame.sequence();
        let header = frame.header;

        // Handle request PDUs based on state
        match frame.pdu {
            // Bind requests (only valid in Open state)
            Pdu::BindTransmitter(bind) => {
                if state != ConnectionState::Open {
                    return self.send_error(framed, sequence, Status::InvalidBindStatus).await;
                }
                self.handle_bind_transmitter(framed, header, bind).await
            }
            Pdu::BindReceiver(bind) => {
                if state != ConnectionState::Open {
                    return self.send_error(framed, sequence, Status::InvalidBindStatus).await;
                }
                self.handle_bind_receiver(framed, header, bind).await
            }
            Pdu::BindTransceiver(bind) => {
                if state != ConnectionState::Open {
                    return self.send_error(framed, sequence, Status::InvalidBindStatus).await;
                }
                self.handle_bind_transceiver(framed, header, bind).await
            }

            // Unbind (valid in any bound state)
            Pdu::Unbind(_) => {
                if !self.connection.is_bound().await {
                    return self.send_error(framed, sequence, Status::InvalidBindStatus).await;
                }
                self.handle_unbind(framed, header).await
            }

            // EnquireLink (valid in any bound state)
            Pdu::EnquireLink(_) => {
                self.handle_enquire_link(framed, header).await
            }

            // SubmitSm (valid in BoundTx or BoundTrx)
            Pdu::SubmitSm(submit) => {
                let state = self.connection.state().await;
                if !state.can_send() {
                    return self.send_error(framed, sequence, Status::InvalidBindStatus).await;
                }
                self.handle_submit_sm(framed, header, submit).await
            }

            // DeliverSm response (we don't expect this from client side)
            Pdu::DeliverSmResp(_) => {
                // This is a response to our deliver_sm, handled in handle_response
                Ok(())
            }

            // Generic NACK
            Pdu::GenericNack(_) => {
                warn!("received generic_nack");
                Ok(())
            }

            // Other PDUs not supported in this direction
            _ => {
                warn!(command = ?header.command, "unsupported command");
                self.send_error(framed, sequence, Status::InvalidCommandId).await
            }
        }
    }

    /// Handle bind_transmitter request.
    async fn handle_bind_transmitter<T>(
        &mut self,
        framed: &mut Framed<T, SmppCodec>,
        header: Header,
        bind: BindTransmitter,
    ) -> Result<(), SessionError>
    where
        T: AsyncRead + AsyncWrite + Unpin,
    {
        info!(
            system_id = %bind.0.system_id,
            "bind_transmitter request"
        );

        // TODO: Authenticate against configured credentials
        // For now, accept all binds

        counters::bind_received(self.connection.listener(), "transmitter");

        self.connection.set_system_id(bind.0.system_id.clone()).await;
        self.connection.set_state(ConnectionState::BoundTx).await;

        let resp = BindTransmitterResp(BindRespFields {
            system_id: "smppd".to_string(),
            tlvs: TlvMap::default(),
        });

        let resp_header = Header::with_status(Command::BindTransmitterResp, header.sequence, Status::Ok);
        self.send_pdu(framed, resp_header, Pdu::BindTransmitterResp(resp)).await?;

        counters::bind_success(self.connection.listener(), "transmitter");
        info!(system_id = %bind.0.system_id, "bound as transmitter");

        Ok(())
    }

    /// Handle bind_receiver request.
    async fn handle_bind_receiver<T>(
        &mut self,
        framed: &mut Framed<T, SmppCodec>,
        header: Header,
        bind: BindReceiver,
    ) -> Result<(), SessionError>
    where
        T: AsyncRead + AsyncWrite + Unpin,
    {
        info!(
            system_id = %bind.0.system_id,
            "bind_receiver request"
        );

        counters::bind_received(self.connection.listener(), "receiver");

        self.connection.set_system_id(bind.0.system_id.clone()).await;
        self.connection.set_state(ConnectionState::BoundRx).await;

        let resp = BindReceiverResp(BindRespFields {
            system_id: "smppd".to_string(),
            tlvs: TlvMap::default(),
        });

        let resp_header = Header::with_status(Command::BindReceiverResp, header.sequence, Status::Ok);
        self.send_pdu(framed, resp_header, Pdu::BindReceiverResp(resp)).await?;

        counters::bind_success(self.connection.listener(), "receiver");
        info!(system_id = %bind.0.system_id, "bound as receiver");

        Ok(())
    }

    /// Handle bind_transceiver request.
    async fn handle_bind_transceiver<T>(
        &mut self,
        framed: &mut Framed<T, SmppCodec>,
        header: Header,
        bind: BindTransceiver,
    ) -> Result<(), SessionError>
    where
        T: AsyncRead + AsyncWrite + Unpin,
    {
        info!(
            system_id = %bind.0.system_id,
            "bind_transceiver request"
        );

        counters::bind_received(self.connection.listener(), "transceiver");

        self.connection.set_system_id(bind.0.system_id.clone()).await;
        self.connection.set_state(ConnectionState::BoundTrx).await;

        let resp = BindTransceiverResp(BindRespFields {
            system_id: "smppd".to_string(),
            tlvs: TlvMap::default(),
        });

        let resp_header = Header::with_status(Command::BindTransceiverResp, header.sequence, Status::Ok);
        self.send_pdu(framed, resp_header, Pdu::BindTransceiverResp(resp)).await?;

        counters::bind_success(self.connection.listener(), "transceiver");
        info!(system_id = %bind.0.system_id, "bound as transceiver");

        Ok(())
    }

    /// Handle unbind request.
    async fn handle_unbind<T>(
        &mut self,
        framed: &mut Framed<T, SmppCodec>,
        header: Header,
    ) -> Result<(), SessionError>
    where
        T: AsyncRead + AsyncWrite + Unpin,
    {
        info!("unbind request");

        self.connection.set_state(ConnectionState::Unbinding).await;

        let resp_header = Header::with_status(Command::UnbindResp, header.sequence, Status::Ok);
        self.send_pdu(framed, resp_header, Pdu::UnbindResp(UnbindResp)).await?;

        counters::unbind(self.connection.listener());

        self.connection.set_state(ConnectionState::Closed).await;

        Ok(())
    }

    /// Handle enquire_link request.
    async fn handle_enquire_link<T>(
        &mut self,
        framed: &mut Framed<T, SmppCodec>,
        header: Header,
    ) -> Result<(), SessionError>
    where
        T: AsyncRead + AsyncWrite + Unpin,
    {
        trace!("enquire_link request");

        let resp_header = Header::with_status(Command::EnquireLinkResp, header.sequence, Status::Ok);
        self.send_pdu(framed, resp_header, Pdu::EnquireLinkResp(EnquireLinkResp)).await?;

        counters::enquire_link_received(self.connection.listener());

        Ok(())
    }

    /// Handle submit_sm request.
    async fn handle_submit_sm<T>(
        &mut self,
        framed: &mut Framed<T, SmppCodec>,
        header: Header,
        submit: SubmitSm,
    ) -> Result<(), SessionError>
    where
        T: AsyncRead + AsyncWrite + Unpin,
    {
        debug!(
            source = %submit.source_addr,
            dest = %submit.dest_addr,
            "submit_sm request"
        );

        counters::message_submitted(self.connection.listener());

        // Create response channel for async forwarding result
        let (response_tx, response_rx) = oneshot::channel();

        // Get client system_id
        let client_system_id = self.connection.system_id().await.unwrap_or_default();

        // Create forward request
        let forward_request = ForwardRequest {
            submit_sm: submit.clone(),
            client_system_id,
            sequence_number: header.sequence,
            response_tx,
        };

        // Send to forwarder
        if let Err(e) = self.forwarder.forward(forward_request).await {
            warn!(error = %e, "failed to send to forwarder");
            return self.send_error(framed, header.sequence, Status::SystemError).await;
        }

        // Wait for forwarding result (with timeout)
        let result = match timeout(Duration::from_secs(30), response_rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => {
                // Channel closed - forwarder was dropped
                warn!("forwarder channel closed");
                return self.send_error(framed, header.sequence, Status::SystemError).await;
            }
            Err(_) => {
                // Timeout waiting for forward result
                warn!("timeout waiting for forward result");
                return self.send_error(framed, header.sequence, Status::SystemError).await;
            }
        };

        // Send response to client - use proper status code mapping
        let status = Status::from_u32(result.command_status);

        let resp = SubmitSmResp {
            message_id: result.message_id.clone(),
            tlvs: TlvMap::default(),
        };

        let resp_header = Header::with_status(Command::SubmitSmResp, header.sequence, status);
        self.send_pdu(framed, resp_header, Pdu::SubmitSmResp(resp)).await?;

        debug!(message_id = %result.message_id, "submit_sm accepted");

        Ok(())
    }

    /// Handle a response PDU.
    async fn handle_response(&mut self, frame: PduFrame) -> Result<(), SessionError> {
        let seq = frame.sequence();

        if let Some(pending) = self.pending.remove(&seq) {
            trace!(
                sequence = seq,
                command = ?pending.command,
                latency_ms = pending.sent_at.elapsed().as_millis(),
                "response received"
            );

            if let Some(tx) = pending.response_tx {
                let _ = tx.send(Ok(frame));
            }
        } else {
            warn!(sequence = seq, "unexpected response");
        }

        Ok(())
    }

    /// Send enquire_link to keep connection alive.
    async fn send_enquire_link<T>(
        &mut self,
        framed: &mut Framed<T, SmppCodec>,
    ) -> Result<(), SessionError>
    where
        T: AsyncRead + AsyncWrite + Unpin,
    {
        let seq = self.connection.next_sequence();
        let header = Header::new(Command::EnquireLink, seq);

        self.pending.insert(seq, PendingRequest {
            command: Command::EnquireLink,
            sent_at: Instant::now(),
            response_tx: None,
        });

        self.send_pdu(framed, header, Pdu::EnquireLink(EnquireLink)).await?;
        self.last_enquire_link = Some(Instant::now());

        counters::enquire_link_sent(self.connection.listener());
        trace!("enquire_link sent");

        Ok(())
    }

    /// Send an error response.
    async fn send_error<T>(
        &mut self,
        framed: &mut Framed<T, SmppCodec>,
        sequence: u32,
        status: Status,
    ) -> Result<(), SessionError>
    where
        T: AsyncRead + AsyncWrite + Unpin,
    {
        let header = Header::with_status(Command::GenericNack, sequence, status);
        self.send_pdu(framed, header, Pdu::GenericNack(GenericNack)).await
    }

    /// Send a PDU.
    async fn send_pdu<T>(
        &mut self,
        framed: &mut Framed<T, SmppCodec>,
        header: Header,
        pdu: Pdu,
    ) -> Result<(), SessionError>
    where
        T: AsyncRead + AsyncWrite + Unpin,
    {
        framed.send((header, pdu)).await.map_err(SessionError::Smpp)?;
        counters::pdu_sent(self.connection.listener(), &format!("{:?}", header.command));
        Ok(())
    }

    /// Check for timed out pending requests.
    fn check_pending_timeouts(&mut self, timeout: Duration) {
        let now = Instant::now();
        let timed_out: Vec<u32> = self
            .pending
            .iter()
            .filter(|(_, req)| now.duration_since(req.sent_at) > timeout)
            .map(|(seq, _)| *seq)
            .collect();

        for seq in timed_out {
            if let Some(pending) = self.pending.remove(&seq) {
                warn!(
                    sequence = seq,
                    command = ?pending.command,
                    "request timed out"
                );

                if let Some(tx) = pending.response_tx {
                    let _ = tx.send(Err(SessionError::Timeout));
                }

                counters::request_timeout(self.connection.listener());
            }
        }
    }
}

// Random number generation for message IDs
mod rand {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static STATE: AtomicU64 = AtomicU64::new(0);

    pub fn random<T: FromU64>() -> T {
        // Simple xorshift64
        let mut s = STATE.load(Ordering::Relaxed);
        if s == 0 {
            s = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64;
        }
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        STATE.store(s, Ordering::Relaxed);
        T::from_u64(s)
    }

    pub trait FromU64 {
        fn from_u64(v: u64) -> Self;
    }

    impl FromU64 for u64 {
        fn from_u64(v: u64) -> Self {
            v
        }
    }
}
