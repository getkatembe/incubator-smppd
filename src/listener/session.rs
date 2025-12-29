//! SMPP session state machine.
//!
//! Handles the SMPP protocol state transitions and PDU processing.
//! Uses event-driven architecture for message processing.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use smpp::codec::{PduFrame, SmppCodec};
use smpp::pdu::{
    BindReceiver, BindReceiverResp, BindRespFields, BindTransceiver, BindTransceiverResp,
    BindTransmitter, BindTransmitterResp, Command, EnquireLink, EnquireLinkResp, GenericNack,
    Header, Pdu, Status, SubmitSm, SubmitSmResp, UnbindResp,
};
use smpp::Error as SmppError;
use smpp::TlvMap;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_util::codec::Framed;
use tracing::{debug, error, info, trace, warn};

use crate::bootstrap::{Event, MessageEnvelope, ResponseEnvelope, SessionId, SharedBus};
use crate::config::BindType;
use crate::filter::{CredentialStore, OpenCredentialStore};
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
///
/// Uses event-driven architecture:
/// - Publishes MessageSubmitRequest events for submit_sm PDUs
/// - Receives ResponseEnvelope via response_rx channel
/// - Does NOT block waiting for upstream responses
pub struct SmppSession {
    /// Unique session identifier
    session_id: SessionId,

    /// Underlying connection
    connection: Arc<Connection>,

    /// Event bus for publishing events
    bus: SharedBus,

    /// Response receiver for async responses
    response_rx: mpsc::Receiver<ResponseEnvelope>,

    /// Credential store for authentication
    credentials: Arc<dyn CredentialStore>,

    /// Pending requests awaiting response (for enquire_link etc)
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
}

impl SmppSession {
    /// Create a new session for a connection.
    pub fn new(
        session_id: SessionId,
        connection: Arc<Connection>,
        bus: SharedBus,
        response_rx: mpsc::Receiver<ResponseEnvelope>,
    ) -> Self {
        Self {
            session_id,
            connection,
            bus,
            response_rx,
            credentials: Arc::new(OpenCredentialStore),
            pending: HashMap::new(),
            enquire_link_interval: Duration::from_secs(30),
            last_enquire_link: None,
        }
    }

    /// Create a new session with custom credential store.
    pub fn with_credentials(
        session_id: SessionId,
        connection: Arc<Connection>,
        bus: SharedBus,
        response_rx: mpsc::Receiver<ResponseEnvelope>,
        credentials: Arc<dyn CredentialStore>,
    ) -> Self {
        Self {
            session_id,
            connection,
            bus,
            response_rx,
            credentials,
            pending: HashMap::new(),
            enquire_link_interval: Duration::from_secs(30),
            last_enquire_link: None,
        }
    }

    /// Get the session ID
    pub fn session_id(&self) -> SessionId {
        self.session_id
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
    ///
    /// Event-driven: listens for both incoming PDUs and async responses
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
                // Read incoming PDU from client
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

                // Receive async response from pipeline
                Some(response) = self.response_rx.recv() => {
                    if let Err(e) = self.handle_async_response(framed, response).await {
                        error!(error = %e, "failed to send response to client");
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

    /// Handle async response from the pipeline
    async fn handle_async_response<T>(
        &mut self,
        framed: &mut Framed<T, SmppCodec>,
        response: ResponseEnvelope,
    ) -> Result<(), SessionError>
    where
        T: AsyncRead + AsyncWrite + Unpin,
    {
        debug!(
            sequence = response.sequence_number,
            message_id = %response.message_id,
            status = response.command_status,
            "sending async response to client"
        );

        let status = Status::from_u32(response.command_status);

        let resp = SubmitSmResp {
            message_id: response.message_id,
            tlvs: TlvMap::default(),
        };

        let resp_header = Header::with_status(Command::SubmitSmResp, response.sequence_number, status);
        self.send_pdu(framed, resp_header, Pdu::SubmitSmResp(resp)).await
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

        counters::bind_received(self.connection.listener(), "transmitter");

        // Authenticate against credential store
        let creds = match self.credentials.validate(&bind.0.system_id, &bind.0.password) {
            Some(creds) => creds,
            None => {
                warn!(
                    system_id = %bind.0.system_id,
                    "bind_transmitter authentication failed"
                );
                counters::bind_failed(self.connection.listener(), "transmitter");
                let resp_header = Header::with_status(
                    Command::BindTransmitterResp,
                    header.sequence,
                    Status::InvalidPassword,
                );
                let resp = BindTransmitterResp(BindRespFields {
                    system_id: String::new(),
                    tlvs: TlvMap::default(),
                });
                return self.send_pdu(framed, resp_header, Pdu::BindTransmitterResp(resp)).await;
            }
        };

        // Check if client is enabled
        if !creds.enabled {
            warn!(
                system_id = %bind.0.system_id,
                "bind_transmitter rejected: client disabled"
            );
            counters::bind_failed(self.connection.listener(), "transmitter");
            let resp_header = Header::with_status(
                Command::BindTransmitterResp,
                header.sequence,
                Status::BindFailed,
            );
            let resp = BindTransmitterResp(BindRespFields {
                system_id: String::new(),
                tlvs: TlvMap::default(),
            });
            return self.send_pdu(framed, resp_header, Pdu::BindTransmitterResp(resp)).await;
        }

        // Check if bind type is allowed
        if !creds.is_bind_allowed(BindType::Transmitter) {
            warn!(
                system_id = %bind.0.system_id,
                "bind_transmitter rejected: bind type not allowed"
            );
            counters::bind_failed(self.connection.listener(), "transmitter");
            let resp_header = Header::with_status(
                Command::BindTransmitterResp,
                header.sequence,
                Status::BindFailed,
            );
            let resp = BindTransmitterResp(BindRespFields {
                system_id: String::new(),
                tlvs: TlvMap::default(),
            });
            return self.send_pdu(framed, resp_header, Pdu::BindTransmitterResp(resp)).await;
        }

        // Check IP address restriction
        let peer_ip = self.connection.peer_addr().ip();
        if !creds.is_ip_allowed(&peer_ip) {
            warn!(
                system_id = %bind.0.system_id,
                peer_ip = %peer_ip,
                "bind_transmitter rejected: IP not allowed"
            );
            counters::bind_failed(self.connection.listener(), "transmitter");
            let resp_header = Header::with_status(
                Command::BindTransmitterResp,
                header.sequence,
                Status::BindFailed,
            );
            let resp = BindTransmitterResp(BindRespFields {
                system_id: String::new(),
                tlvs: TlvMap::default(),
            });
            return self.send_pdu(framed, resp_header, Pdu::BindTransmitterResp(resp)).await;
        }

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

        // Authenticate against credential store
        let creds = match self.credentials.validate(&bind.0.system_id, &bind.0.password) {
            Some(creds) => creds,
            None => {
                warn!(
                    system_id = %bind.0.system_id,
                    "bind_receiver authentication failed"
                );
                counters::bind_failed(self.connection.listener(), "receiver");
                let resp_header = Header::with_status(
                    Command::BindReceiverResp,
                    header.sequence,
                    Status::InvalidPassword,
                );
                let resp = BindReceiverResp(BindRespFields {
                    system_id: String::new(),
                    tlvs: TlvMap::default(),
                });
                return self.send_pdu(framed, resp_header, Pdu::BindReceiverResp(resp)).await;
            }
        };

        // Check if client is enabled
        if !creds.enabled {
            warn!(
                system_id = %bind.0.system_id,
                "bind_receiver rejected: client disabled"
            );
            counters::bind_failed(self.connection.listener(), "receiver");
            let resp_header = Header::with_status(
                Command::BindReceiverResp,
                header.sequence,
                Status::BindFailed,
            );
            let resp = BindReceiverResp(BindRespFields {
                system_id: String::new(),
                tlvs: TlvMap::default(),
            });
            return self.send_pdu(framed, resp_header, Pdu::BindReceiverResp(resp)).await;
        }

        // Check if bind type is allowed
        if !creds.is_bind_allowed(BindType::Receiver) {
            warn!(
                system_id = %bind.0.system_id,
                "bind_receiver rejected: bind type not allowed"
            );
            counters::bind_failed(self.connection.listener(), "receiver");
            let resp_header = Header::with_status(
                Command::BindReceiverResp,
                header.sequence,
                Status::BindFailed,
            );
            let resp = BindReceiverResp(BindRespFields {
                system_id: String::new(),
                tlvs: TlvMap::default(),
            });
            return self.send_pdu(framed, resp_header, Pdu::BindReceiverResp(resp)).await;
        }

        // Check IP address restriction
        let peer_ip = self.connection.peer_addr().ip();
        if !creds.is_ip_allowed(&peer_ip) {
            warn!(
                system_id = %bind.0.system_id,
                peer_ip = %peer_ip,
                "bind_receiver rejected: IP not allowed"
            );
            counters::bind_failed(self.connection.listener(), "receiver");
            let resp_header = Header::with_status(
                Command::BindReceiverResp,
                header.sequence,
                Status::BindFailed,
            );
            let resp = BindReceiverResp(BindRespFields {
                system_id: String::new(),
                tlvs: TlvMap::default(),
            });
            return self.send_pdu(framed, resp_header, Pdu::BindReceiverResp(resp)).await;
        }

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

        // Authenticate against credential store
        let creds = match self.credentials.validate(&bind.0.system_id, &bind.0.password) {
            Some(creds) => creds,
            None => {
                warn!(
                    system_id = %bind.0.system_id,
                    "bind_transceiver authentication failed"
                );
                counters::bind_failed(self.connection.listener(), "transceiver");
                let resp_header = Header::with_status(
                    Command::BindTransceiverResp,
                    header.sequence,
                    Status::InvalidPassword,
                );
                let resp = BindTransceiverResp(BindRespFields {
                    system_id: String::new(),
                    tlvs: TlvMap::default(),
                });
                return self.send_pdu(framed, resp_header, Pdu::BindTransceiverResp(resp)).await;
            }
        };

        // Check if client is enabled
        if !creds.enabled {
            warn!(
                system_id = %bind.0.system_id,
                "bind_transceiver rejected: client disabled"
            );
            counters::bind_failed(self.connection.listener(), "transceiver");
            let resp_header = Header::with_status(
                Command::BindTransceiverResp,
                header.sequence,
                Status::BindFailed,
            );
            let resp = BindTransceiverResp(BindRespFields {
                system_id: String::new(),
                tlvs: TlvMap::default(),
            });
            return self.send_pdu(framed, resp_header, Pdu::BindTransceiverResp(resp)).await;
        }

        // Check if bind type is allowed
        if !creds.is_bind_allowed(BindType::Transceiver) {
            warn!(
                system_id = %bind.0.system_id,
                "bind_transceiver rejected: bind type not allowed"
            );
            counters::bind_failed(self.connection.listener(), "transceiver");
            let resp_header = Header::with_status(
                Command::BindTransceiverResp,
                header.sequence,
                Status::BindFailed,
            );
            let resp = BindTransceiverResp(BindRespFields {
                system_id: String::new(),
                tlvs: TlvMap::default(),
            });
            return self.send_pdu(framed, resp_header, Pdu::BindTransceiverResp(resp)).await;
        }

        // Check IP address restriction
        let peer_ip = self.connection.peer_addr().ip();
        if !creds.is_ip_allowed(&peer_ip) {
            warn!(
                system_id = %bind.0.system_id,
                peer_ip = %peer_ip,
                "bind_transceiver rejected: IP not allowed"
            );
            counters::bind_failed(self.connection.listener(), "transceiver");
            let resp_header = Header::with_status(
                Command::BindTransceiverResp,
                header.sequence,
                Status::BindFailed,
            );
            let resp = BindTransceiverResp(BindRespFields {
                system_id: String::new(),
                tlvs: TlvMap::default(),
            });
            return self.send_pdu(framed, resp_header, Pdu::BindTransceiverResp(resp)).await;
        }

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
    ///
    /// Event-driven: publishes MessageSubmitRequest and returns immediately.
    /// Response will be sent asynchronously via handle_async_response.
    async fn handle_submit_sm<T>(
        &mut self,
        _framed: &mut Framed<T, SmppCodec>,
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

        // Get client system_id
        let client_system_id = self.connection.system_id().await.unwrap_or_default();

        // Create message envelope
        let envelope = MessageEnvelope::new(
            self.session_id,
            header.sequence,
            client_system_id,
            submit,
        );

        debug!(
            message_id = %envelope.message_id,
            session_id = %self.session_id,
            "publishing MessageSubmitRequest event"
        );

        // Publish event - response will come back asynchronously
        self.bus.publish(Event::MessageSubmitRequest(envelope));

        // Return immediately - no blocking!
        // Response will be sent via handle_async_response when the pipeline completes
        Ok(())
    }

    /// Handle a response PDU (for enquire_link responses, etc).
    async fn handle_response(&mut self, frame: PduFrame) -> Result<(), SessionError> {
        let seq = frame.sequence();

        if let Some(pending) = self.pending.remove(&seq) {
            trace!(
                sequence = seq,
                command = ?pending.command,
                latency_ms = pending.sent_at.elapsed().as_millis(),
                "response received"
            );
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

    /// Check for timed out pending requests (enquire_link, etc).
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
