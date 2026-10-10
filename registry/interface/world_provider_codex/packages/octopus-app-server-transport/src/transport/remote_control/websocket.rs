use super::CurrentRemoteControlEnrollment;
use super::RemoteControlEnrollmentSelection;
use super::RemoteControlPairingPersistenceKey;
use super::desired_state::RemoteControlDesiredState;
use super::protocol::ClientEnvelope;
use super::protocol::ClientEvent;
use super::protocol::ClientId;
use super::protocol::RemoteControlTarget;
use super::protocol::ServerEnvelope;
use super::protocol::StreamId;
use super::remote_control_status_with_connection_status;
use super::same_remote_control_enrollment;
use super::segment::ClientSegmentObservation;
use super::segment::ClientSegmentReassembler;
use super::segment::REMOTE_CONTROL_SEGMENT_MAX_BYTES;
use super::segment::split_server_envelope_for_transport;
use crate::home_dir::backoff;
use crate::rustls_provider::ensure_rustls_crypto_provider;
use crate::transport::TransportEvent;
use crate::transport::remote_control::auth::RemoteControlConnectionAuth;
use crate::transport::remote_control::auth::load_remote_control_auth;
use crate::transport::remote_control::auth::recover_remote_control_auth;
use crate::transport::remote_control::client_tracker::ClientTracker;
use crate::transport::remote_control::client_tracker::REMOTE_CONTROL_IDLE_SWEEP_INTERVAL;
use crate::transport::remote_control::enroll::RemoteControlEnrollment;
use crate::transport::remote_control::enroll::enroll_remote_control_server;
use crate::transport::remote_control::enroll::format_headers;
use crate::transport::remote_control::enroll::preview_remote_control_response_body;
use crate::transport::remote_control::enroll::refresh_remote_control_server;
use axum::http::HeaderValue;
use base64::Engine;
use futures::SinkExt;
use futures::StreamExt;
use futures::stream::SplitSink;
use futures::stream::SplitStream;
use octopus_appserver_proto::RemoteControlConnectionStatus;
use octopus_appserver_proto::RemoteControlStatusChangedNotification;
use octopus_login::AuthManager;
use octopus_login::UnauthorizedRecovery;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::io;
use std::io::ErrorKind;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_util::sync::CancellationToken;

use tracing::error;
use tracing::info;
use tracing::warn;

pub(super) const REMOTE_CONTROL_PROTOCOL_VERSION: &str = "3";
pub(super) const REMOTE_CONTROL_INSTALLATION_ID_HEADER: &str = "x-codex-installation-id";
const REMOTE_CONTROL_SUBSCRIBE_CURSOR_HEADER: &str = "x-codex-subscribe-cursor";
const REMOTE_CONTROL_WEBSOCKET_PING_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(10);
const REMOTE_CONTROL_WEBSOCKET_PONG_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(60);
const REMOTE_CONTROL_ACCOUNT_ID_RETRY_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(1);
const REMOTE_CONTROL_RECONNECT_BACKOFF_CAP: std::time::Duration =
    std::time::Duration::from_secs(30);
const REMOTE_CONTROL_WEBSOCKET_CONNECT_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(30);
const REMOTE_CONTROL_CONNECTION_SHUTDOWN_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(5);
const REMOTE_APP_SERVER_NOT_FOUND_DETAIL: &str = "Remote app server not found";

struct BoundedOutboundBuffer {
    buffer_by_stream: HashMap<(ClientId, StreamId), VecDeque<ServerEnvelope>>,
    used_tx: watch::Sender<usize>,
}

impl BoundedOutboundBuffer {
    fn new() -> (Self, watch::Receiver<usize>) {
        let (used_tx, used_rx) = watch::channel(0);
        let buffer = Self {
            buffer_by_stream: HashMap::new(),
            used_tx,
        };
        (buffer, used_rx)
    }

    fn insert(&mut self, server_envelope: &ServerEnvelope) {
        self.buffer_by_stream
            .entry((
                server_envelope.client_id.clone(),
                server_envelope.stream_id.clone(),
            ))
            .or_default()
            .push_back(server_envelope.clone());
        self.used_tx.send_modify(|used| *used += 1);
    }

    fn ack(
        &mut self,
        client_id: &ClientId,
        stream_id: &StreamId,
        acked_seq_id: u64,
        acked_segment_id: Option<usize>,
    ) {
        let key = (client_id.clone(), stream_id.clone());
        let Some(buffer) = self.buffer_by_stream.get_mut(&key) else {
            return;
        };
        let acked_cursor = (acked_seq_id, acked_segment_id.unwrap_or(usize::MAX));
        buffer.retain(|server_envelope| {
            let envelope_cursor = (
                server_envelope.seq_id,
                server_envelope.event.segment_id().unwrap_or_default(),
            );
            let is_acked = envelope_cursor <= acked_cursor;
            if is_acked {
                self.used_tx.send_modify(|used| *used -= 1);
            }
            !is_acked
        });
        if buffer.is_empty() {
            self.buffer_by_stream.remove(&key);
        }
    }

    fn server_envelopes(&self) -> impl Iterator<Item = &ServerEnvelope> {
        self.buffer_by_stream
            .values()
            .flat_map(|buffer| buffer.iter())
    }
}

struct WebsocketState {
    outbound_buffer: BoundedOutboundBuffer,
    subscribe_cursor: Option<String>,
    next_seq_id_by_stream: HashMap<(ClientId, StreamId), u64>,
    last_completed_client_chunk_seq_id_by_stream: HashMap<(ClientId, Option<StreamId>), u64>,
    client_segment_reassembler: ClientSegmentReassembler,
}

impl WebsocketState {
    fn observe_client_message(
        &mut self,
        client_envelope: ClientEnvelope,
        wire_size_bytes: usize,
    ) -> ClientSegmentObservation {
        let client_message_key = Self::client_message_key(&client_envelope);
        if let Some((key, seq_id)) = client_message_key.as_ref()
            && self
                .last_completed_client_chunk_seq_id_by_stream
                .get(key)
                .is_some_and(|last_seq_id| last_seq_id >= seq_id)
        {
            return ClientSegmentObservation::Dropped;
        }
        if let (
            Some((_, seq_id)),
            Some(stream_id),
            ClientEvent::ClientMessageChunk { segment_id, .. },
        ) = (
            client_message_key.as_ref(),
            client_envelope.stream_id.as_ref(),
            &client_envelope.event,
        ) && self.client_segment_reassembler.should_ignore_chunk(
            &client_envelope.client_id,
            stream_id,
            *seq_id,
            *segment_id,
        ) {
            return ClientSegmentObservation::Dropped;
        }
        if client_message_key.is_some() && wire_size_bytes > REMOTE_CONTROL_SEGMENT_MAX_BYTES {
            warn!(
                client_id = client_envelope.client_id.0.as_str(),
                "dropping oversized segmented remote-control client envelope"
            );
            if let Some(stream_id) = client_envelope.stream_id.as_ref() {
                self.client_segment_reassembler
                    .invalidate_stream(&client_envelope.client_id, stream_id);
            }
            return ClientSegmentObservation::Dropped;
        }

        self.client_segment_reassembler.observe(client_envelope)
    }

    fn record_client_message_delivery(
        &mut self,
        client_envelope: &ClientEnvelope,
        client_message_key: Option<((ClientId, Option<StreamId>), u64)>,
    ) {
        if let Some(cursor) = client_envelope.cursor.as_deref() {
            self.subscribe_cursor = Some(cursor.to_string());
        }
        if let Some((key, seq_id)) = client_message_key {
            self.last_completed_client_chunk_seq_id_by_stream
                .insert(key, seq_id);
        }
        if let ClientEvent::Ack { segment_id } = &client_envelope.event
            && let Some(acked_seq_id) = client_envelope.seq_id
            && let Some(stream_id) = client_envelope.stream_id.as_ref()
        {
            self.outbound_buffer.ack(
                &client_envelope.client_id,
                stream_id,
                acked_seq_id,
                *segment_id,
            );
        }
    }

    fn invalidate_client_message_stream(&mut self, client_id: &ClientId, stream_id: &StreamId) {
        self.last_completed_client_chunk_seq_id_by_stream
            .remove(&(client_id.clone(), Some(stream_id.clone())));
    }

    fn invalidate_client_message_client(&mut self, client_id: &ClientId) {
        self.last_completed_client_chunk_seq_id_by_stream
            .retain(|(cursor_client_id, _), _| cursor_client_id != client_id);
    }

    fn client_message_key(
        client_envelope: &ClientEnvelope,
    ) -> Option<((ClientId, Option<StreamId>), u64)> {
        let seq_id = match (&client_envelope.event, client_envelope.seq_id) {
            (ClientEvent::ClientMessageChunk { .. }, Some(seq_id)) => seq_id,
            _ => return None,
        };
        Some((
            (
                client_envelope.client_id.clone(),
                client_envelope.stream_id.clone(),
            ),
            seq_id,
        ))
    }
}

pub(crate) struct RemoteControlWebsocket {
    remote_control_url: String,
    installation_id: String,
    server_name: String,
    remote_control_target: Option<RemoteControlTarget>,
    auth_manager: Arc<AuthManager>,
    status_publisher: RemoteControlStatusPublisher,
    shutdown_token: CancellationToken,
    reconnect_attempt: u64,
    auth_recovery: UnauthorizedRecovery,
    auth_change_rx: watch::Receiver<u64>,
    current_enrollment: CurrentRemoteControlEnrollment,
    pairing_persistence_key: RemoteControlPairingPersistenceKey,
    client_tracker: Arc<Mutex<ClientTracker>>,
    state: Arc<Mutex<WebsocketState>>,
    server_event_rx: Arc<Mutex<mpsc::Receiver<super::QueuedServerEnvelope>>>,
    used_rx: watch::Receiver<usize>,
    desired_state_tx: Arc<watch::Sender<RemoteControlDesiredState>>,
    desired_state_rx: watch::Receiver<RemoteControlDesiredState>,
}

pub(crate) struct RemoteControlWebsocketConfig {
    pub(crate) remote_control_url: String,
    pub(crate) installation_id: String,
    pub(crate) remote_control_target: Option<RemoteControlTarget>,
    pub(crate) server_name: String,
}

pub(super) struct RemoteControlAuthContext<'a> {
    auth_manager: &'a Arc<AuthManager>,
    auth_recovery: &'a mut UnauthorizedRecovery,
    auth_change_rx: &'a mut watch::Receiver<u64>,
}

struct RemoteControlEnrollmentAuthContext<'a, 'b> {
    auth: &'a RemoteControlConnectionAuth,
    recovery: &'a mut RemoteControlAuthContext<'b>,
}

enum ConnectOutcome {
    Connected(Box<WebSocketStream<MaybeTlsStream<TcpStream>>>),
    Disabled,
    Shutdown,
}

#[derive(Debug, Clone, Copy)]
enum ConnectionEndReason {
    Shutdown,
    Disabled,
    EnabledWatchClosed,
    ConnectionWorkerStopped,
}

pub(super) struct RemoteControlChannels {
    pub(super) transport_event_tx: mpsc::Sender<TransportEvent>,
    pub(super) status_publisher: RemoteControlStatusPublisher,
    pub(super) current_enrollment: CurrentRemoteControlEnrollment,
    pub(super) pairing_persistence_key: RemoteControlPairingPersistenceKey,
}

#[derive(Clone)]
pub(super) struct RemoteControlStatusPublisher {
    tx: watch::Sender<RemoteControlStatusChangedNotification>,
}

impl RemoteControlStatusPublisher {
    pub(super) fn new(tx: watch::Sender<RemoteControlStatusChangedNotification>) -> Self {
        Self { tx }
    }

    fn status(&self) -> RemoteControlStatusChangedNotification {
        self.tx.borrow().clone()
    }

    fn publish_status(&self, connection_status: RemoteControlConnectionStatus) {
        let mut status_change = None;
        self.tx.send_if_modified(|status| {
            let next_status =
                remote_control_status_with_connection_status(status, connection_status);
            if *status == next_status {
                return false;
            }

            status_change = Some((status.clone(), next_status.clone()));
            *status = next_status;
            true
        });
        if let Some((previous_status, next_status)) = status_change {
            info!(
                previous_status = ?previous_status.status,
                next_status = ?next_status.status,
                previous_environment_id = ?previous_status.environment_id,
                next_environment_id = ?next_status.environment_id,
                installation_id = %next_status.installation_id,
                server_name = %next_status.server_name,
                "remote control websocket status changed"
            );
        }
    }

    pub(super) fn publish_environment_id(&self, environment_id: Option<String>) {
        let mut status_change = None;
        self.tx.send_if_modified(|status| {
            if status.status == RemoteControlConnectionStatus::Disabled {
                return false;
            }
            let next_status = RemoteControlStatusChangedNotification {
                status: status.status,
                server_name: status.server_name.clone(),
                installation_id: status.installation_id.clone(),
                environment_id,
            };
            if *status == next_status {
                return false;
            }

            status_change = Some((status.clone(), next_status.clone()));
            *status = next_status;
            true
        });
        if let Some((previous_status, next_status)) = status_change {
            info!(
                status = ?next_status.status,
                previous_environment_id = ?previous_status.environment_id,
                next_environment_id = ?next_status.environment_id,
                installation_id = %next_status.installation_id,
                server_name = %next_status.server_name,
                "remote control websocket environment changed"
            );
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct RemoteControlConnectOptions<'a> {
    installation_id: &'a str,
    server_name: &'a str,
    subscribe_cursor: Option<&'a str>,
    desired_state_tx: &'a watch::Sender<RemoteControlDesiredState>,
}

impl RemoteControlWebsocket {
    pub(crate) fn new(
        config: RemoteControlWebsocketConfig,
        auth_manager: Arc<AuthManager>,
        channels: RemoteControlChannels,
        shutdown_token: CancellationToken,
        desired_state_tx: Arc<watch::Sender<RemoteControlDesiredState>>,
    ) -> Self {
        let shutdown_token = shutdown_token.child_token();
        let (server_event_tx, server_event_rx) = mpsc::channel(super::CHANNEL_CAPACITY);
        let client_tracker = ClientTracker::new(
            server_event_tx,
            channels.transport_event_tx,
            &shutdown_token,
        );
        let (outbound_buffer, used_rx) = BoundedOutboundBuffer::new();
        let auth_recovery = auth_manager.unauthorized_recovery();
        let auth_change_rx = auth_manager.auth_change_receiver();

        let desired_state_rx = desired_state_tx.subscribe();
        Self {
            remote_control_url: config.remote_control_url,
            installation_id: config.installation_id,
            server_name: config.server_name,
            remote_control_target: config.remote_control_target,
            auth_manager,
            status_publisher: channels.status_publisher,
            shutdown_token,
            reconnect_attempt: 0,
            auth_recovery,
            auth_change_rx,
            current_enrollment: channels.current_enrollment,
            pairing_persistence_key: channels.pairing_persistence_key,
            client_tracker: Arc::new(Mutex::new(client_tracker)),
            state: Arc::new(Mutex::new(WebsocketState {
                outbound_buffer,
                subscribe_cursor: None,
                next_seq_id_by_stream: HashMap::new(),
                last_completed_client_chunk_seq_id_by_stream: HashMap::new(),
                client_segment_reassembler: ClientSegmentReassembler::default(),
            })),
            server_event_rx: Arc::new(Mutex::new(server_event_rx)),
            used_rx,
            desired_state_tx,
            desired_state_rx,
        }
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "remote-control client shutdown must serialize tracker state"
    )]
    pub(crate) async fn run(
        mut self,
        app_server_client_name_rx: Option<oneshot::Receiver<String>>,
    ) {
        info!(
            remote_control_url = %self.remote_control_url,
            installation_id = %self.installation_id,
            server_name = %self.server_name,
            initial_desired_state = ?*self.desired_state_rx.borrow(),
            "app-server remote control websocket loop started"
        );
        let app_server_client_name = match self
            .wait_for_app_server_client_name(app_server_client_name_rx)
            .await
        {
            Ok(app_server_client_name) => app_server_client_name,
            Err(_) => {
                warn!(
                    remote_control_url = %self.remote_control_url,
                    installation_id = %self.installation_id,
                    server_name = %self.server_name,
                    shutdown_requested = self.shutdown_token.is_cancelled(),
                    "app-server remote control websocket loop stopped before client name was ready"
                );
                self.client_tracker.lock().await.shutdown().await;
                return;
            }
        };
        self.pairing_persistence_key
            .send_replace(app_server_client_name.clone());

        loop {
            if !self.wait_until_enabled().await {
                info!(
                    remote_control_url = %self.remote_control_url,
                    installation_id = %self.installation_id,
                    server_name = %self.server_name,
                    shutdown_requested = self.shutdown_token.is_cancelled(),
                    current_status = ?self.status_publisher.status().status,
                    "app-server remote control websocket loop exiting while waiting for enablement"
                );
                break;
            }

            let status = self.status_publisher.status();
            info!(
                remote_control_url = %self.remote_control_url,
                installation_id = %self.installation_id,
                server_name = %self.server_name,
                reconnect_attempt = self.reconnect_attempt.saturating_add(1),
                current_status = ?status.status,
                environment_id = ?status.environment_id,
                "starting app-server remote control websocket connection cycle"
            );
            let shutdown_token = self.shutdown_token.child_token();
            let websocket_connection = match self
                .connect(&shutdown_token, app_server_client_name.as_deref())
                .await
            {
                ConnectOutcome::Connected(websocket_connection) => *websocket_connection,
                ConnectOutcome::Disabled => {
                    self.status_publisher
                        .publish_status(RemoteControlConnectionStatus::Disabled);
                    continue;
                }
                ConnectOutcome::Shutdown => break,
            };

            let connection_end_reason = self
                .run_connection(websocket_connection, shutdown_token)
                .await;
            let status = self.status_publisher.status();
            info!(
                remote_control_url = %self.remote_control_url,
                installation_id = %self.installation_id,
                server_name = %self.server_name,
                connection_end_reason = ?connection_end_reason,
                current_status = ?status.status,
                environment_id = ?status.environment_id,
                desired_state = ?*self.desired_state_rx.borrow(),
                "app-server remote control websocket connection cycle ended"
            );
        }

        self.client_tracker.lock().await.shutdown().await;
        info!(
            remote_control_url = %self.remote_control_url,
            installation_id = %self.installation_id,
            server_name = %self.server_name,
            shutdown_requested = self.shutdown_token.is_cancelled(),
            "app-server remote control websocket loop exited"
        );
    }

    async fn wait_for_app_server_client_name(
        &self,
        app_server_client_name_rx: Option<oneshot::Receiver<String>>,
    ) -> Result<Option<String>, ()> {
        match app_server_client_name_rx {
            Some(app_server_client_name_rx) => {
                tokio::select! {
                    _ = self.shutdown_token.cancelled() => Err(()),
                    app_server_client_name = app_server_client_name_rx => match app_server_client_name {
                        Ok(app_server_client_name) => Ok(Some(app_server_client_name)),
                        Err(_) => Err(()),
                    },
                }
            }
            None => Ok(None),
        }
    }

    async fn wait_until_enabled(&mut self) -> bool {
        tokio::select! {
            _ = self.shutdown_token.cancelled() => false,
            desired_state = self.desired_state_rx.wait_for(|state| state.is_enabled()) => desired_state.is_ok(),
        }
    }

    async fn connect(
        &mut self,
        shutdown_token: &CancellationToken,
        app_server_client_name: Option<&str>,
    ) -> ConnectOutcome {
        self.status_publisher
            .publish_status(RemoteControlConnectionStatus::Connecting);
        let remote_control_target = match self.remote_control_target.as_ref() {
            Some(remote_control_target) => remote_control_target.clone(),
            None => match super::protocol::normalize_remote_control_url(&self.remote_control_url) {
                Ok(remote_control_target) => {
                    self.remote_control_target = Some(remote_control_target.clone());
                    remote_control_target
                }
                Err(err) => {
                    self.status_publisher
                        .publish_status(RemoteControlConnectionStatus::Errored);
                    warn!("remote control is enabled but the URL is invalid: {err}");
                    tokio::select! {
                        _ = shutdown_token.cancelled() => return ConnectOutcome::Shutdown,
                        changed = self.desired_state_rx.wait_for(|state| !state.is_enabled()) => {
                            if changed.is_err() {
                                return ConnectOutcome::Shutdown;
                            }
                            return ConnectOutcome::Disabled;
                        }
                    }
                }
            },
        };

        loop {
            let subscribe_cursor = self.state.lock().await.subscribe_cursor.clone();
            let enrollment = self.current_enrollment.snapshot();
            info!(
                websocket_url = %remote_control_target.websocket_url,
                installation_id = %self.installation_id,
                server_name = %self.server_name,
                reconnect_attempt = self.reconnect_attempt.saturating_add(1),
                has_enrollment = enrollment.is_some(),
                server_id = ?enrollment.as_ref().map(|enrollment| enrollment.server_id.as_str()),
                environment_id = ?enrollment.as_ref().map(|enrollment| enrollment.environment_id.as_str()),
                subscribe_cursor_present = subscribe_cursor.is_some(),
                app_server_client_name = ?app_server_client_name,
                "connecting to app-server remote control websocket"
            );
            let connect_options = RemoteControlConnectOptions {
                installation_id: &self.installation_id,
                server_name: &self.server_name,
                subscribe_cursor: subscribe_cursor.as_deref(),
                desired_state_tx: &self.desired_state_tx,
            };
            let auth_context = RemoteControlAuthContext {
                auth_manager: &self.auth_manager,
                auth_recovery: &mut self.auth_recovery,
                auth_change_rx: &mut self.auth_change_rx,
            };
            let mut disabled_rx = self.desired_state_rx.clone();
            let connect_result = tokio::select! {
                _ = shutdown_token.cancelled() => return ConnectOutcome::Shutdown,
                changed = disabled_rx.wait_for(|state| !state.is_enabled()) => {
                    if changed.is_err() {
                        return ConnectOutcome::Shutdown;
                    }
                    return ConnectOutcome::Disabled;
                }
                connect_result = async {
                    connect_remote_control_websocket(
                        &remote_control_target,
                        auth_context,
                        &self.current_enrollment,
                        connect_options,
                        &self.status_publisher,
                    )
                    .await
                } => connect_result,
            };

            match connect_result {
                Ok((websocket_connection, response)) => {
                    if !self.desired_state_rx.borrow().is_enabled() {
                        return ConnectOutcome::Disabled;
                    }
                    self.reconnect_attempt = 0;
                    self.auth_recovery = self.auth_manager.unauthorized_recovery();
                    self.status_publisher
                        .publish_status(RemoteControlConnectionStatus::Connected);
                    let enrollment = self.current_enrollment.snapshot();
                    info!(
                        websocket_url = %remote_control_target.websocket_url,
                        installation_id = %self.installation_id,
                        server_name = %self.server_name,
                        server_id = ?enrollment.as_ref().map(|enrollment| enrollment.server_id.as_str()),
                        environment_id = ?enrollment.as_ref().map(|enrollment| enrollment.environment_id.as_str()),
                        subscribe_cursor_present = subscribe_cursor.is_some(),
                        response_headers = %format_headers(response.headers()),
                        "connected to app-server remote control websocket"
                    );
                    return ConnectOutcome::Connected(Box::new(websocket_connection));
                }
                Err(err) => {
                    if !self.desired_state_rx.borrow().is_enabled() {
                        return ConnectOutcome::Disabled;
                    }
                    let reconnect_delay = if err.kind() == ErrorKind::WouldBlock {
                        REMOTE_CONTROL_ACCOUNT_ID_RETRY_INTERVAL
                    } else {
                        self.status_publisher
                            .publish_status(RemoteControlConnectionStatus::Errored);
                        let reconnect_attempt = self.reconnect_attempt.saturating_add(1);
                        let (reconnect_delay, reconnect_backoff_reset) =
                            next_reconnect_delay(&mut self.reconnect_attempt);
                        let enrollment = self.current_enrollment.snapshot();
                        warn!(
                            websocket_url = %remote_control_target.websocket_url,
                            installation_id = %self.installation_id,
                            server_name = %self.server_name,
                            error = %err,
                            error_kind = ?err.kind(),
                            reconnect_attempt,
                            reconnect_delay = ?reconnect_delay,
                            reconnect_backoff_reset,
                            has_enrollment = enrollment.is_some(),
                            server_id = ?enrollment.as_ref().map(|enrollment| enrollment.server_id.as_str()),
                            environment_id = ?enrollment.as_ref().map(|enrollment| enrollment.environment_id.as_str()),
                            subscribe_cursor_present = subscribe_cursor.is_some(),
                            "failed to connect to app-server remote control websocket"
                        );
                        if reconnect_backoff_reset {
                            info!(
                                reconnect_backoff_cap = ?REMOTE_CONTROL_RECONNECT_BACKOFF_CAP,
                                "reset app-server remote control websocket reconnect backoff after cap"
                            );
                        }
                        reconnect_delay
                    };
                    tokio::select! {
                        _ = shutdown_token.cancelled() => return ConnectOutcome::Shutdown,
                        changed = self.desired_state_rx.wait_for(|state| !state.is_enabled()) => {
                            if changed.is_err() {
                                return ConnectOutcome::Shutdown;
                            }
                            return ConnectOutcome::Disabled;
                        }
                        changed = self.auth_change_rx.changed() => {
                            if changed.is_err() {
                                return ConnectOutcome::Shutdown;
                            }
                            self.auth_recovery = self.auth_manager.unauthorized_recovery();
                            self.reconnect_attempt = 0;
                            info!("retrying app-server remote control websocket after auth changed");
                        }
                        _ = tokio::time::sleep(reconnect_delay) => {}
                    }
                }
            }
        }
    }

    async fn run_connection(
        &self,
        websocket_connection: WebSocketStream<MaybeTlsStream<TcpStream>>,
        shutdown_token: CancellationToken,
    ) -> ConnectionEndReason {
        let (websocket_writer, websocket_reader) = websocket_connection.split();
        let mut join_set = tokio::task::JoinSet::new();

        join_set.spawn(Self::run_server_writer(
            self.state.clone(),
            self.server_event_rx.clone(),
            self.used_rx.clone(),
            websocket_writer,
            REMOTE_CONTROL_WEBSOCKET_PING_INTERVAL,
            shutdown_token.clone(),
        ));
        join_set.spawn(Self::run_websocket_reader(
            self.client_tracker.clone(),
            self.state.clone(),
            websocket_reader,
            REMOTE_CONTROL_WEBSOCKET_PONG_TIMEOUT,
            shutdown_token.clone(),
        ));

        let mut desired_state_rx = self.desired_state_rx.clone();
        let connection_end_reason = tokio::select! {
            _ = shutdown_token.cancelled() => ConnectionEndReason::Shutdown,
            changed = desired_state_rx.wait_for(|state| !state.is_enabled()) => {
                if changed.is_ok() {
                    self.status_publisher
                        .publish_status(RemoteControlConnectionStatus::Disabled);
                    ConnectionEndReason::Disabled
                } else {
                    ConnectionEndReason::EnabledWatchClosed
                }
            }
            _ = join_set.join_next() => ConnectionEndReason::ConnectionWorkerStopped,
        };
        shutdown_token.cancel();

        Self::join_connection_workers(&mut join_set, REMOTE_CONTROL_CONNECTION_SHUTDOWN_TIMEOUT)
            .await;
        connection_end_reason
    }

    async fn join_connection_workers(
        join_set: &mut tokio::task::JoinSet<()>,
        shutdown_timeout: std::time::Duration,
    ) {
        if tokio::time::timeout(shutdown_timeout, Self::drain_join_set(join_set))
            .await
            .is_ok()
        {
            return;
        }

        warn!(
            shutdown_timeout = ?shutdown_timeout,
            remaining_workers = join_set.len(),
            "timed out waiting for remote control connection workers to stop; aborting"
        );
        join_set.abort_all();
        Self::drain_join_set(join_set).await;
    }

    async fn drain_join_set(join_set: &mut tokio::task::JoinSet<()>) {
        while join_set.join_next().await.is_some() {}
    }

    async fn run_server_writer(
        state: Arc<Mutex<WebsocketState>>,
        server_event_rx: Arc<Mutex<mpsc::Receiver<super::QueuedServerEnvelope>>>,
        used_rx: watch::Receiver<usize>,
        websocket_writer: SplitSink<
            WebSocketStream<MaybeTlsStream<TcpStream>>,
            tungstenite::Message,
        >,
        ping_interval: std::time::Duration,
        shutdown_token: CancellationToken,
    ) {
        let result = Self::run_server_writer_inner(
            state,
            server_event_rx,
            used_rx,
            websocket_writer,
            ping_interval,
            shutdown_token,
        )
        .await;
        if let Err(err) = result {
            warn!("remote control websocket writer disconnected, err: {err}");
        } else {
            warn!("remote control websocket writer was stopped");
        }
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "remote-control server event receiver is shared across reconnects"
    )]
    async fn run_server_writer_inner(
        state: Arc<Mutex<WebsocketState>>,
        server_event_rx: Arc<Mutex<mpsc::Receiver<super::QueuedServerEnvelope>>>,
        mut used_rx: watch::Receiver<usize>,
        mut websocket_writer: SplitSink<
            WebSocketStream<MaybeTlsStream<TcpStream>>,
            tungstenite::Message,
        >,
        ping_interval: std::time::Duration,
        shutdown_token: CancellationToken,
    ) -> io::Result<()> {
        let server_envelopes = state
            .lock()
            .await
            .outbound_buffer
            .server_envelopes()
            .cloned()
            .collect::<Vec<_>>();
        for server_envelope in server_envelopes {
            let payload = match serde_json::to_string(&server_envelope) {
                Ok(payload) => payload,
                Err(err) => {
                    error!("failed to serialize remote-control server event: {err}");
                    continue;
                }
            };
            tokio::select! {
                _ = shutdown_token.cancelled() => return Ok(()),
                send_result = websocket_writer.send(tungstenite::Message::Text(payload.into())) => {
                    if let Err(err) = send_result {
                        return Err(io::Error::other(err));
                    }
                }
            };
        }

        let mut ping_interval =
            tokio::time::interval_at(tokio::time::Instant::now() + ping_interval, ping_interval);
        ping_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut server_event_rx = server_event_rx.lock().await;
        loop {
            let outbound_has_capacity = *used_rx.borrow() < super::CHANNEL_CAPACITY;
            let queued_server_envelope = tokio::select! {
                _ = shutdown_token.cancelled() => return Ok(()),
                _ = ping_interval.tick() => {
                    tokio::select! {
                        _ = shutdown_token.cancelled() => return Ok(()),
                        send_result = websocket_writer.send(tungstenite::Message::Ping(Vec::new().into())) => {
                            if let Err(err) = send_result {
                                return Err(io::Error::other(err));
                            }
                        }
                    };
                    continue;
                }
                wait_result = used_rx.changed(), if !outbound_has_capacity =>
                {
                    if wait_result.is_err() {
                        return Err(io::Error::new(
                            ErrorKind::UnexpectedEof,
                            "outbound buffer usage channel closed",
                        ));
                    }
                    continue;
                }
                recv_result = server_event_rx.recv(), if outbound_has_capacity => {
                    match recv_result {
                        Some(queued_server_envelope) => queued_server_envelope,
                        None => {
                            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "server event channel closed"));
                        }
                    }
                }
            };
            let (payloads, write_complete_tx) = {
                let mut state = state.lock().await;
                let seq_key = (
                    queued_server_envelope.client_id.clone(),
                    queued_server_envelope.stream_id.clone(),
                );
                let seq_id = *state
                    .next_seq_id_by_stream
                    .entry(seq_key.clone())
                    .or_insert(1);

                let server_envelope = ServerEnvelope {
                    event: queued_server_envelope.event,
                    client_id: queued_server_envelope.client_id,
                    seq_id,
                    stream_id: queued_server_envelope.stream_id,
                };
                let server_envelopes = match split_server_envelope_for_transport(server_envelope) {
                    Ok(server_envelopes) => server_envelopes,
                    Err(err) => {
                        error!("failed to split remote-control server event: {err}");
                        continue;
                    }
                };
                let mut payloads = Vec::with_capacity(server_envelopes.len());
                for server_envelope in server_envelopes {
                    let payload = match serde_json::to_string(&server_envelope) {
                        Ok(payload) => payload,
                        Err(err) => {
                            error!("failed to serialize remote-control server event: {err}");
                            continue;
                        }
                    };
                    state.outbound_buffer.insert(&server_envelope);
                    payloads.push(payload);
                }
                state
                    .next_seq_id_by_stream
                    .insert(seq_key, seq_id.saturating_add(1));

                (payloads, queued_server_envelope.write_complete_tx)
            };

            for payload in payloads {
                tokio::select! {
                    _ = shutdown_token.cancelled() => return Ok(()),
                    send_result = websocket_writer.send(tungstenite::Message::Text(payload.into())) => {
                        if let Err(err) = send_result {
                            return Err(io::Error::other(err));
                        }
                    }
                }
            }
            if let Some(write_complete_tx) = write_complete_tx {
                let _ = write_complete_tx.send(());
            }
        }
    }

    async fn run_websocket_reader(
        client_tracker: Arc<Mutex<ClientTracker>>,
        state: Arc<Mutex<WebsocketState>>,
        websocket_reader: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
        pong_timeout: std::time::Duration,
        shutdown_token: CancellationToken,
    ) {
        let result = Self::run_websocket_reader_inner(
            client_tracker,
            state,
            websocket_reader,
            pong_timeout,
            shutdown_token,
        )
        .await;
        if let Err(err) = result {
            warn!("remote control websocket reader disconnected, err: {err}");
        } else {
            warn!("remote control websocket reader was stopped");
        }
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "remote-control client tracking must stay serialized while processing inbound events"
    )]
    async fn run_websocket_reader_inner(
        client_tracker: Arc<Mutex<ClientTracker>>,
        state: Arc<Mutex<WebsocketState>>,
        mut websocket_reader: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
        pong_timeout: std::time::Duration,
        shutdown_token: CancellationToken,
    ) -> io::Result<()> {
        let mut client_tracker = client_tracker.lock().await;
        let mut idle_sweep_interval = tokio::time::interval(REMOTE_CONTROL_IDLE_SWEEP_INTERVAL);
        idle_sweep_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let pong_deadline = tokio::time::sleep(pong_timeout);
        tokio::pin!(pong_deadline);

        loop {
            let incoming_message = tokio::select! {
                _ = shutdown_token.cancelled() => return Ok(()),
                _ = &mut pong_deadline => {
                    return Err(io::Error::new(
                        ErrorKind::TimedOut,
                        "remote control websocket pong timeout",
                    ));
                }
                client_key = client_tracker.bookkeep_join_set() => {
                    let Some(client_key) = client_key else {
                        continue;
                    };
                    if client_tracker.close_client(&client_key).await.is_err() {
                        return Ok(());
                    }
                    state
                        .lock()
                        .await
                        .client_segment_reassembler
                        .invalidate_stream(&client_key.0, &client_key.1);
                    state
                        .lock()
                        .await
                        .invalidate_client_message_stream(&client_key.0, &client_key.1);
                    continue;
                }
                _ = idle_sweep_interval.tick() => {
                    match client_tracker.close_expired_clients().await {
                        Ok(client_keys) => {
                            let mut websocket_state = state.lock().await;
                            for (client_id, stream_id) in client_keys {
                                websocket_state
                                    .client_segment_reassembler
                                    .invalidate_stream(&client_id, &stream_id);
                                websocket_state
                                    .invalidate_client_message_stream(&client_id, &stream_id);
                            }
                        }
                        Err(_) => return Ok(()),
                    }
                    continue;
                }
                incoming_message = websocket_reader.next() => {
                    match incoming_message {
                        Some(incoming_message) => incoming_message,
                        None => return Err(io::Error::new(ErrorKind::UnexpectedEof, "websocket stream ended")),
                    }
                }
            };
            let (client_envelope, wire_size_bytes) = match incoming_message {
                Ok(tungstenite::Message::Text(text)) => {
                    let wire_size_bytes = text.len();
                    match serde_json::from_str::<ClientEnvelope>(&text) {
                        Ok(client_envelope) => (client_envelope, wire_size_bytes),
                        Err(err) => {
                            warn!("failed to deserialize remote-control client event: {err}");
                            continue;
                        }
                    }
                }
                Ok(tungstenite::Message::Pong(_)) => {
                    pong_deadline
                        .as_mut()
                        .reset(tokio::time::Instant::now() + pong_timeout);
                    continue;
                }
                Ok(tungstenite::Message::Ping(_)) | Ok(tungstenite::Message::Frame(_)) => continue,
                Ok(tungstenite::Message::Binary(_)) => {
                    warn!("dropping unsupported binary remote-control websocket message");
                    continue;
                }
                Ok(tungstenite::Message::Close(_)) => {
                    return Err(io::Error::new(
                        ErrorKind::ConnectionAborted,
                        "websocket disconnected",
                    ));
                }
                Err(err) => {
                    return Err(io::Error::new(
                        ErrorKind::InvalidData,
                        format!("failed to read from websocket: {err}"),
                    ));
                }
            };

            let client_message_key = WebsocketState::client_message_key(&client_envelope);
            let observation = {
                let mut websocket_state = state.lock().await;
                websocket_state.observe_client_message(client_envelope, wire_size_bytes)
            };
            let client_envelope = match observation {
                ClientSegmentObservation::Forward(client_envelope) => *client_envelope,
                ClientSegmentObservation::Pending | ClientSegmentObservation::Dropped => continue,
            };

            let closed_client =
                matches!(&client_envelope.event, ClientEvent::ClientClosed).then(|| {
                    (
                        client_envelope.client_id.clone(),
                        client_envelope.stream_id.clone(),
                    )
                });
            let delivered_client_envelope = client_envelope.clone();
            if client_tracker
                .handle_message(client_envelope)
                .await
                .is_err()
            {
                return Ok(());
            }
            state
                .lock()
                .await
                .record_client_message_delivery(&delivered_client_envelope, client_message_key);
            if let Some((client_id, stream_id)) = closed_client {
                let mut websocket_state = state.lock().await;
                if let Some(stream_id) = stream_id {
                    websocket_state
                        .client_segment_reassembler
                        .invalidate_stream(&client_id, &stream_id);
                    websocket_state.invalidate_client_message_stream(&client_id, &stream_id);
                } else {
                    websocket_state
                        .client_segment_reassembler
                        .invalidate_client(&client_id);
                    websocket_state.invalidate_client_message_client(&client_id);
                }
            }
        }
    }
}

fn set_remote_control_header(
    headers: &mut tungstenite::http::HeaderMap,
    name: &'static str,
    value: &str,
) -> io::Result<()> {
    let header_value = HeaderValue::from_str(value).map_err(|err| {
        io::Error::new(
            ErrorKind::InvalidInput,
            format!("invalid remote control header `{name}`: {err}"),
        )
    })?;
    headers.insert(name, header_value);
    Ok(())
}

fn build_remote_control_websocket_request(
    websocket_url: &str,
    enrollment: &RemoteControlEnrollment,
    installation_id: &str,
    subscribe_cursor: Option<&str>,
) -> io::Result<tungstenite::http::Request<()>> {
    let mut request = websocket_url.into_client_request().map_err(|err| {
        io::Error::new(
            ErrorKind::InvalidInput,
            format!("invalid remote control websocket URL `{websocket_url}`: {err}"),
        )
    })?;
    let headers = request.headers_mut();
    set_remote_control_header(headers, "x-codex-server-id", &enrollment.server_id)?;
    set_remote_control_header(
        headers,
        "x-codex-name",
        &base64::engine::general_purpose::STANDARD.encode(&enrollment.server_name),
    )?;
    set_remote_control_header(
        headers,
        "x-codex-protocol-version",
        REMOTE_CONTROL_PROTOCOL_VERSION,
    )?;
    set_remote_control_header(
        headers,
        "authorization",
        &format!(
            "Bearer {}",
            enrollment
                .remote_control_token
                .as_deref()
                .ok_or_else(|| io::Error::other("missing remote control server token"))?
        ),
    )?;
    set_remote_control_header(
        headers,
        REMOTE_CONTROL_INSTALLATION_ID_HEADER,
        installation_id,
    )?;
    if let Some(subscribe_cursor) = subscribe_cursor {
        set_remote_control_header(
            headers,
            REMOTE_CONTROL_SUBSCRIBE_CURSOR_HEADER,
            subscribe_cursor,
        )?;
    }
    Ok(request)
}

fn next_reconnect_delay(reconnect_attempt: &mut u64) -> (std::time::Duration, bool) {
    let reconnect_delay = backoff(*reconnect_attempt).min(REMOTE_CONTROL_RECONNECT_BACKOFF_CAP);
    let reconnect_backoff_reset = reconnect_delay == REMOTE_CONTROL_RECONNECT_BACKOFF_CAP;
    *reconnect_attempt = if reconnect_backoff_reset {
        0
    } else {
        (*reconnect_attempt).saturating_add(1)
    };
    (reconnect_delay, reconnect_backoff_reset)
}

pub(super) async fn connect_remote_control_websocket(
    remote_control_target: &RemoteControlTarget,
    mut auth_context: RemoteControlAuthContext<'_>,
    current_enrollment: &CurrentRemoteControlEnrollment,
    connect_options: RemoteControlConnectOptions<'_>,
    status_publisher: &RemoteControlStatusPublisher,
) -> io::Result<(
    WebSocketStream<MaybeTlsStream<TcpStream>>,
    tungstenite::http::Response<()>,
)> {
    ensure_rustls_crypto_provider();

    let (auth, enrollment) = {
        let mut current_enrollment = current_enrollment.lock().await;
        let auth = prepare_remote_control_enrollment(
            remote_control_target,
            &mut auth_context,
            &mut current_enrollment,
            connect_options,
            status_publisher,
        )
        .await?;
        let enrollment = current_enrollment.as_ref().cloned().ok_or_else(|| {
            io::Error::other("missing remote control enrollment after enrollment step")
        })?;
        (auth, enrollment)
    };
    let request = build_remote_control_websocket_request(
        &remote_control_target.websocket_url,
        &enrollment,
        connect_options.installation_id,
        connect_options.subscribe_cursor,
    )?;

    let websocket_connect_result = tokio::time::timeout(
        REMOTE_CONTROL_WEBSOCKET_CONNECT_TIMEOUT,
        connect_async(request),
    )
    .await
    .map_err(|_| {
        io::Error::new(
            ErrorKind::TimedOut,
            format!(
                "timed out connecting to remote control websocket at `{}` after {:?}",
                remote_control_target.websocket_url, REMOTE_CONTROL_WEBSOCKET_CONNECT_TIMEOUT
            ),
        )
    })?;

    match websocket_connect_result {
        Ok((websocket_stream, response)) => Ok((websocket_stream, response.map(|_| ()))),
        Err(err) => {
            match &err {
                tungstenite::Error::Http(response)
                    if websocket_response_reports_missing_remote_app_server(response) =>
                {
                    info!(
                        "remote control websocket returned HTTP 404; replacing stale enrollment: websocket_url={}, account_id={}, server_id={}, environment_id={}",
                        remote_control_target.websocket_url,
                        auth.account_id,
                        enrollment.server_id,
                        enrollment.environment_id
                    );
                    replace_remote_control_enrollment_if_matches(
                        remote_control_target,
                        RemoteControlEnrollmentAuthContext {
                            auth: &auth,
                            recovery: &mut auth_context,
                        },
                        current_enrollment,
                        &enrollment,
                        connect_options,
                        status_publisher,
                    )
                    .await?;
                }
                tungstenite::Error::Http(response) if response.status().as_u16() == 404 => {
                    let response_body = response
                        .body()
                        .as_deref()
                        .map(preview_remote_control_response_body)
                        .unwrap_or_else(|| "<missing>".to_string());
                    warn!(
                        websocket_url = %remote_control_target.websocket_url,
                        account_id = %auth.account_id,
                        server_id = %enrollment.server_id,
                        environment_id = %enrollment.environment_id,
                        response_status = %response.status(),
                        response_headers = %format_headers(response.headers()),
                        response_body = %response_body,
                        "remote control websocket returned unrecognized HTTP 404; preserving enrollment before retry"
                    );
                }
                tungstenite::Error::Http(response)
                    if matches!(response.status().as_u16(), 401 | 403) =>
                {
                    clear_remote_control_server_token_if_matches(current_enrollment, &enrollment)
                        .await?;
                    return Err(io::Error::other(format!(
                        "remote control websocket auth failed with HTTP {}; refreshing server token before reconnect",
                        response.status()
                    )));
                }
                _ => {}
            }
            Err(io::Error::other(
                format_remote_control_websocket_connect_error(
                    &remote_control_target.websocket_url,
                    &err,
                ),
            ))
        }
    }
}

async fn prepare_remote_control_enrollment(
    remote_control_target: &RemoteControlTarget,
    auth_context: &mut RemoteControlAuthContext<'_>,
    enrollment: &mut Option<RemoteControlEnrollment>,
    connect_options: RemoteControlConnectOptions<'_>,
    status_publisher: &RemoteControlStatusPublisher,
) -> io::Result<RemoteControlConnectionAuth> {
    let auth = match load_remote_control_auth(auth_context.auth_manager).await {
        Ok(auth) => auth,
        Err(err) => {
            if err.kind() == ErrorKind::PermissionDenied {
                *enrollment = None;
                status_publisher.publish_environment_id(None);
            }
            return Err(err);
        }
    };
    let enrollment_account_id = enrollment.as_ref().map(|enrollment| &enrollment.account_id);
    if enrollment_account_id.is_some_and(|account_id| account_id != &auth.account_id) {
        info!(
            "clearing in-memory remote control enrollment because account id changed: websocket_url={}, previous_account_id={:?}, current_account_id={:?}",
            remote_control_target.websocket_url,
            enrollment
                .as_ref()
                .map(|enrollment| enrollment.account_id.as_str()),
            auth.account_id
        );
        *enrollment = None;
        status_publisher.publish_environment_id(None);
        if !connect_options.desired_state_tx.borrow().is_enabled() {
            return Err(io::Error::new(
                ErrorKind::Interrupted,
                "remote control disabled after account changed",
            ));
        }
    }
    if let Some(enrollment) = enrollment.as_mut() {
        enrollment.remote_control_target = remote_control_target.clone();
    }

    if let Some(enrollment) = enrollment.as_ref() {
        status_publisher.publish_environment_id(Some(enrollment.environment_id.clone()));
    }

    enroll_remote_control_server_for_current_run(
        remote_control_target,
        RemoteControlEnrollmentAuthContext {
            auth: &auth,
            recovery: auth_context,
        },
        enrollment,
        connect_options,
        status_publisher,
        RemoteControlEnrollmentSelection::ReuseOrCreate,
    )
    .await?;

    if enrollment
        .as_ref()
        .ok_or_else(|| io::Error::other("missing remote control enrollment after enrollment step"))?
        .should_refresh_server_token()
    {
        let enrollment_ref = enrollment.as_ref().ok_or_else(|| {
            io::Error::other("missing remote control enrollment after enrollment step")
        })?;
        let server_id = enrollment_ref.server_id.clone();
        let environment_id = enrollment_ref.environment_id.clone();

        info!(
            "refreshing remote control server token: websocket_url={}, refresh_url={}, account_id={}, server_id={}, environment_id={}",
            remote_control_target.websocket_url,
            remote_control_target.refresh_url,
            auth.account_id,
            server_id,
            environment_id
        );
        let enrollment_ref = enrollment.as_mut().ok_or_else(|| {
            io::Error::other("missing remote control enrollment before server refresh")
        })?;
        match refresh_remote_control_server(&auth, connect_options.installation_id, enrollment_ref)
            .await
        {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {
                info!(
                    "remote control server refresh returned HTTP 404; replacing stale enrollment: websocket_url={}, account_id={}, server_id={}, environment_id={}",
                    remote_control_target.websocket_url, auth.account_id, server_id, environment_id
                );
                enroll_remote_control_server_for_current_run(
                    remote_control_target,
                    RemoteControlEnrollmentAuthContext {
                        auth: &auth,
                        recovery: auth_context,
                    },
                    enrollment,
                    connect_options,
                    status_publisher,
                    RemoteControlEnrollmentSelection::ReplaceExisting,
                )
                .await?;
            }
            Err(err)
                if err.kind() == ErrorKind::PermissionDenied
                    && recover_remote_control_auth(
                        auth_context.auth_recovery,
                        auth_context.auth_change_rx,
                    )
                    .await =>
            {
                return Err(io::Error::other(format!(
                    "{err}; retrying after auth recovery"
                )));
            }
            Err(err) => return Err(err),
        }
    }

    Ok(auth)
}

fn websocket_response_reports_missing_remote_app_server(
    response: &tungstenite::http::Response<Option<Vec<u8>>>,
) -> bool {
    response.status().as_u16() == 404
        && response.body().as_deref().is_some_and(|body| {
            serde_json::from_slice::<serde_json::Value>(body).is_ok_and(|body| {
                body.get("detail").and_then(serde_json::Value::as_str)
                    == Some(REMOTE_APP_SERVER_NOT_FOUND_DETAIL)
            })
        })
}

async fn replace_remote_control_enrollment_if_matches(
    remote_control_target: &RemoteControlTarget,
    auth_context: RemoteControlEnrollmentAuthContext<'_, '_>,
    current_enrollment: &CurrentRemoteControlEnrollment,
    enrollment: &RemoteControlEnrollment,
    connect_options: RemoteControlConnectOptions<'_>,
    status_publisher: &RemoteControlStatusPublisher,
) -> io::Result<()> {
    let mut current_enrollment = current_enrollment.lock().await;
    if !current_enrollment
        .as_ref()
        .is_some_and(|current| same_remote_control_enrollment(current, enrollment))
    {
        return Ok(());
    }
    enroll_remote_control_server_for_current_run(
        remote_control_target,
        auth_context,
        &mut current_enrollment,
        connect_options,
        status_publisher,
        RemoteControlEnrollmentSelection::ReplaceExisting,
    )
    .await
}

async fn clear_remote_control_server_token_if_matches(
    current_enrollment: &CurrentRemoteControlEnrollment,
    enrollment: &RemoteControlEnrollment,
) -> io::Result<()> {
    let mut current_enrollment = current_enrollment.lock().await;
    current_enrollment
        .as_mut()
        .filter(|current| same_remote_control_enrollment(current, enrollment))
        .ok_or_else(|| {
            io::Error::other("missing remote control enrollment after websocket auth failure")
        })?
        .clear_server_token();
    Ok(())
}

async fn enroll_remote_control_server_for_current_run(
    remote_control_target: &RemoteControlTarget,
    auth_context: RemoteControlEnrollmentAuthContext<'_, '_>,
    enrollment: &mut Option<RemoteControlEnrollment>,
    connect_options: RemoteControlConnectOptions<'_>,
    status_publisher: &RemoteControlStatusPublisher,
    selection: RemoteControlEnrollmentSelection,
) -> io::Result<()> {
    match selection {
        RemoteControlEnrollmentSelection::ReuseOrCreate => {
            if enrollment.is_some() {
                return Ok(());
            }
        }
        RemoteControlEnrollmentSelection::ReplaceExisting => {}
    }
    if !connect_options.desired_state_tx.borrow().is_enabled() {
        return Err(io::Error::new(
            ErrorKind::Interrupted,
            "remote control disabled before enrollment",
        ));
    }

    info!(
        "creating new remote control enrollment: websocket_url={}, enroll_url={}, account_id={}",
        remote_control_target.websocket_url,
        remote_control_target.enroll_url,
        auth_context.auth.account_id
    );
    let new_enrollment = match enroll_remote_control_server(
        remote_control_target,
        auth_context.auth,
        connect_options.installation_id,
        connect_options.server_name,
    )
    .await
    {
        Ok(new_enrollment) => new_enrollment,
        Err(err)
            if err.kind() == ErrorKind::PermissionDenied
                && recover_remote_control_auth(
                    auth_context.recovery.auth_recovery,
                    auth_context.recovery.auth_change_rx,
                )
                .await =>
        {
            return Err(io::Error::other(format!(
                "{err}; retrying after auth recovery"
            )));
        }
        Err(err) => return Err(err),
    };
    if !connect_options.desired_state_tx.borrow().is_enabled() {
        return Err(io::Error::new(
            ErrorKind::Interrupted,
            "remote control disabled during enrollment",
        ));
    }
    info!(
        "created new remote control enrollment: websocket_url={}, account_id={}, server_id={}, environment_id={}",
        remote_control_target.websocket_url,
        new_enrollment.account_id,
        new_enrollment.server_id,
        new_enrollment.environment_id
    );
    status_publisher.publish_environment_id(Some(new_enrollment.environment_id.clone()));
    *enrollment = Some(new_enrollment);
    Ok(())
}

fn format_remote_control_websocket_connect_error(
    websocket_url: &str,
    err: &tungstenite::Error,
) -> String {
    let mut message =
        format!("failed to connect app-server remote control websocket `{websocket_url}`: {err}");
    let tungstenite::Error::Http(response) = err else {
        return message;
    };

    message.push_str(&format!(", {}", format_headers(response.headers())));
    if let Some(body) = response.body().as_ref()
        && !body.is_empty()
    {
        let body_preview = preview_remote_control_response_body(body);
        message.push_str(&format!(", body: {body_preview}"));
    }

    message
}
