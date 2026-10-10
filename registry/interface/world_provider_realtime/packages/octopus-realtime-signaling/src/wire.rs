use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{Sink, SinkExt, StreamExt};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, LOCATION};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest, protocol::WebSocketConfig, Message,
};
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

use crate::auth::Credential;
use crate::{LiveProtocol, RealtimeDoorConfig};

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Clone)]
pub struct LiveClient {
    config: RealtimeDoorConfig,
    http: reqwest::Client,
    sideband_tls: Option<Arc<rustls::ClientConfig>>,
    credential: Credential,
    maximum: usize,
    timeout: Duration,
}

pub struct OpenedCall {
    pub call_id: String,
    pub answer_sdp: String,
    pub response_bytes: usize,
    pub response_blake3: String,
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CreationDisposition {
    NotCreated,
    Unconfirmed,
    Created,
}

#[derive(Clone, Debug)]
pub struct CallCreationFailure {
    pub error: String,
    pub creation_disposition: CreationDisposition,
    pub call_id: Option<String>,
}

impl CallCreationFailure {
    pub(crate) fn not_created(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            creation_disposition: CreationDisposition::NotCreated,
            call_id: None,
        }
    }

    pub(crate) fn unconfirmed(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            creation_disposition: CreationDisposition::Unconfirmed,
            call_id: None,
        }
    }

    pub(crate) fn created(error: impl Into<String>, call_id: String) -> Self {
        Self {
            error: error.into(),
            creation_disposition: CreationDisposition::Created,
            call_id: Some(call_id),
        }
    }

    fn with_error(mut self, error: impl Into<String>) -> Self {
        self.error = error.into();
        self
    }
}

pub struct SidebandEvent {
    pub call_id: String,
    pub event: Value,
}

pub struct SidebandCommand {
    pub frames: Vec<Value>,
    pub close: bool,
    pub completion: oneshot::Sender<Result<bool, String>>,
}

impl LiveClient {
    pub async fn connect(config: &RealtimeDoorConfig) -> Result<Self, String> {
        validate_endpoint(&config.transport.base_url, false)?;
        let sideband_url = validate_endpoint(&config.private.sideband_url, true)?;
        if config.transport.auth_env_key.is_some()
            || config.transport.auth_secret_ref.is_some()
            || config.transport.auth_header.is_some()
            || config.transport.auth_value_prefix.is_some()
            || config.transport.base_url_env.is_some()
        {
            return Err(
                "Live transport authentication belongs to explicit private_config.auth".into(),
            );
        }
        let timeout_ms = config
            .transport
            .request_timeout_ms
            .ok_or("Live request_timeout_ms is required")?;
        let maximum = config
            .transport
            .max_response_bytes
            .ok_or("Live max_response_bytes is required")?;
        if !(1..=300_000).contains(&timeout_ms) || !(1024..=16_777_216).contains(&maximum) {
            return Err("Live transport limits are outside supported bounds".into());
        }
        if config.private.protocol == LiveProtocol::OpenaiLive
            && matches!(
                config.private.auth,
                crate::auth::AuthConfig::CodexMachine { .. }
            )
        {
            return Err("public Live requires explicitly declared API bearer authorization".into());
        }
        let sideband_tls = if sideband_url.scheme() == "wss" {
            Some(Arc::new(sideband_tls_config()?))
        } else {
            None
        };
        let credential = config.private.auth.resolve().await?;
        let mut headers = HeaderMap::new();
        for (name, value) in &config.transport.headers {
            let name =
                HeaderName::from_bytes(name.as_bytes()).map_err(|_| "invalid Live header name")?;
            if matches!(
                name.as_str(),
                "authorization"
                    | "proxy-authorization"
                    | "cookie"
                    | "set-cookie"
                    | "chatgpt-account-id"
            ) {
                return Err("Live credentials must not appear in tracked headers".into());
            }
            if headers
                .insert(
                    name,
                    HeaderValue::from_str(value).map_err(|_| "invalid Live header value")?,
                )
                .is_some()
            {
                return Err("duplicate Live header".into());
            }
        }
        if config.private.protocol == LiveProtocol::CodexLive
            && headers
                .get("openai-alpha")
                .and_then(|value| value.to_str().ok())
                != Some("quicksilver=v2")
        {
            return Err("codex_live requires declared OpenAI-Alpha: quicksilver=v2".into());
        }
        headers.extend(credential.headers.clone());
        let timeout = Duration::from_millis(timeout_ms);
        let http = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(timeout)
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .default_headers(headers)
            .build()
            .map_err(|_| "could not create Live HTTP client")?;
        Ok(Self {
            config: config.clone(),
            http,
            sideband_tls,
            credential,
            maximum: maximum as usize,
            timeout,
        })
    }

    pub fn owner(&self) -> &str {
        &self.credential.owner
    }

    pub async fn open_call(
        &self,
        sdp: &str,
        session: &Value,
    ) -> Result<OpenedCall, CallCreationFailure> {
        let mut url = validate_endpoint(&self.config.transport.base_url, false)
            .map_err(CallCreationFailure::not_created)?;
        let request = match self.config.private.protocol {
            LiveProtocol::CodexLive => {
                let mut session = session.clone();
                if let Some(items) = session
                    .get_mut("initial_items")
                    .and_then(Value::as_array_mut)
                {
                    for item in items {
                        let content_type =
                            if item.get("role").and_then(Value::as_str) == Some("assistant") {
                                "output_text"
                            } else {
                                "input_text"
                            };
                        if let Some(parts) = item.get_mut("content").and_then(Value::as_array_mut) {
                            for part in parts {
                                part["type"] = json!(content_type);
                            }
                        }
                    }
                }
                url.query_pairs_mut()
                    .append_pair("intent", "quicksilver")
                    .append_pair("architecture", "avas");
                self.http
                    .post(url)
                    .json(&json!({"sdp":sdp,"session":session}))
            }
            LiveProtocol::OpenaiLive => {
                let mut session = session.clone();
                if let Some(items) = session
                    .as_object_mut()
                    .and_then(|object| object.remove("initial_items"))
                {
                    session["input"] = items;
                }
                self.http
                    .post(url)
                    .json(&json!({"session":session,"transport":{"type":"webrtc","sdp":sdp}}))
            }
        };
        let mut response = request.send().await.map_err(|_| {
            CallCreationFailure::unconfirmed(
                "Live call creation transport failed; outcome unknown; offer will not be retried",
            )
        })?;
        let status = response.status();
        let headers = response.headers().clone();
        let mut failure = if status.is_client_error() && status.as_u16() != 408 {
            CallCreationFailure::not_created("")
        } else {
            CallCreationFailure::unconfirmed("")
        };
        if status.is_success() && self.config.private.protocol == LiveProtocol::CodexLive {
            let location = headers
                .get(LOCATION)
                .ok_or_else(|| {
                    failure
                        .clone()
                        .with_error("Live response has no Location call identity")
                })?
                .to_str()
                .map_err(|_| {
                    failure
                        .clone()
                        .with_error("Live response Location is malformed")
                })?;
            let id = decode_call_id(location).map_err(|error| failure.clone().with_error(error))?;
            if self.credential.contains_secret(id.as_bytes()) {
                return Err(failure.with_error("Live response reflected authentication material"));
            }
            failure = CallCreationFailure::created("", id);
            if self.credential.contains_secret(location.as_bytes()) {
                return Err(failure.with_error("Live response reflected authentication material"));
            }
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.maximum as u64)
        {
            return Err(failure.with_error("Live call response exceeds the declared size limit"));
        }
        let mut bytes = Vec::new();
        while let Some(part) = response.chunk().await.map_err(|_| {
            failure
                .clone()
                .with_error("Live call response was interrupted; outcome unknown")
        })? {
            if part.len() > self.maximum.saturating_sub(bytes.len()) {
                return Err(
                    failure.with_error("Live call response exceeds the declared size limit")
                );
            }
            bytes.extend_from_slice(&part);
        }
        if !status.is_success() {
            return Err(failure.with_error(self.http_refusal(
                "call",
                status.as_u16(),
                &headers,
                &bytes,
            )));
        }
        let answer_sdp = match self.config.private.protocol {
            LiveProtocol::CodexLive => std::str::from_utf8(&bytes)
                .map_err(|_| failure.clone().with_error("Live SDP answer is not UTF-8"))?
                .to_string(),
            LiveProtocol::OpenaiLive => {
                let body: Value = serde_json::from_slice(&bytes).map_err(|_| {
                    failure
                        .clone()
                        .with_error("Live session response is not JSON")
                })?;
                let id = body
                    .pointer("/session/id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        failure
                            .clone()
                            .with_error("Live session response has no session identity")
                    })?;
                validate_call_id(id).map_err(|error| failure.clone().with_error(error))?;
                if self.credential.contains_secret(id.as_bytes()) {
                    return Err(
                        failure.with_error("Live response reflected authentication material")
                    );
                }
                failure = CallCreationFailure::created("", id.to_string());
                if body.pointer("/transport/type").and_then(Value::as_str) != Some("webrtc") {
                    return Err(failure.with_error("Live session response has no WebRTC transport"));
                }
                body.pointer("/transport/sdp")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        failure
                            .clone()
                            .with_error("Live session response has no SDP answer")
                    })?
                    .to_string()
            }
        };
        if self.credential.contains_secret(&bytes)
            || self.credential.contains_secret(answer_sdp.as_bytes())
        {
            return Err(failure.with_error("Live response reflected authentication material"));
        }
        if !answer_sdp.starts_with("v=0") || answer_sdp.contains('\0') {
            return Err(failure.with_error("Live response is not an SDP answer"));
        }
        Ok(OpenedCall {
            call_id: failure
                .call_id
                .clone()
                .ok_or_else(|| failure.with_error("Live response has no call identity"))?,
            answer_sdp,
            response_bytes: bytes.len(),
            response_blake3: blake3::hash(&bytes).to_hex().to_string(),
        })
    }

    pub(crate) fn http_refusal(
        &self,
        operation: &str,
        status: u16,
        headers: &HeaderMap,
        bytes: &[u8],
    ) -> String {
        let mut detail = serde_json::Map::new();
        if let Some(id) = headers
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
        {
            if !id.is_empty()
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                && !self.credential.contains_secret(id.as_bytes())
            {
                detail.insert("request_id".into(), json!(id));
            }
        }
        let body = (bytes.len() <= self.maximum)
            .then(|| serde_json::from_slice::<Value>(bytes).ok())
            .flatten();
        if self.credential.contains_secret(bytes)
            || body.as_ref().is_some_and(|value| {
                self.credential
                    .contains_secret(value.to_string().as_bytes())
            })
        {
            detail.insert(
                "detail".into(),
                json!("provider error reflected authentication material; body withheld"),
            );
        } else if let Some(body) = body {
            let error = body
                .get("error")
                .or_else(|| body.get("detail"))
                .unwrap_or(&body);
            if let Some(message) = error.as_str() {
                detail.insert("message".into(), json!(bounded_error_text(message, 512)));
            } else {
                for (key, limit) in [
                    ("code", 128),
                    ("type", 128),
                    ("param", 128),
                    ("message", 512),
                ] {
                    if let Some(value) = error.get(key).and_then(Value::as_str) {
                        if !self.credential.contains_secret(value.as_bytes()) {
                            detail.insert(key.into(), json!(bounded_error_text(value, limit)));
                        }
                    }
                }
            }
        }
        if detail.is_empty() {
            format!("Live {operation} refused: HTTP {status}; no safe structured error detail")
        } else {
            format!(
                "Live {operation} refused: HTTP {status}; {}",
                Value::Object(detail)
            )
        }
    }

    async fn socket(&self, call_id: &str) -> Result<Socket, String> {
        validate_call_id(call_id)?;
        let mut url = validate_endpoint(&self.config.private.sideband_url, true)?;
        url.path_segments_mut()
            .map_err(|_| "Live sideband URL has no path")?
            .pop_if_empty()
            .push(call_id);
        if self.config.private.protocol == LiveProtocol::OpenaiLive {
            url.path_segments_mut()
                .map_err(|_| "Live sideband URL has no path")?
                .push("attach");
        }
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|_| "invalid Live WebSocket request")?;
        for (name, value) in &self.config.transport.headers {
            let name =
                HeaderName::from_bytes(name.as_bytes()).map_err(|_| "invalid sideband header")?;
            request.headers_mut().insert(
                name,
                HeaderValue::from_str(value).map_err(|_| "invalid sideband header")?,
            );
        }
        request
            .headers_mut()
            .extend(self.credential.headers.clone());
        let config = WebSocketConfig::default()
            .max_message_size(Some(self.maximum))
            .max_frame_size(Some(self.maximum));
        let (socket, _) = tokio::time::timeout(
            self.timeout,
            tokio_tungstenite::connect_async_tls_with_config(
                request,
                Some(config),
                false,
                self.sideband_tls.clone().map(Connector::Rustls),
            ),
        )
        .await
        .map_err(|_| "Live sideband attach timed out")?
        .map_err(|error| match error {
            tokio_tungstenite::tungstenite::Error::Http(response) => self.http_refusal(
                "sideband attach",
                response.status().as_u16(),
                response.headers(),
                response.body().as_deref().unwrap_or_default(),
            ),
            _ => "Live sideband attach transport failed".into(),
        })?;
        Ok(socket)
    }

    pub async fn attach(
        &self,
        call_id: &str,
        events: mpsc::Sender<SidebandEvent>,
    ) -> Result<mpsc::Sender<SidebandCommand>, String> {
        let socket = self.socket(call_id).await?;
        let (tx, rx) = mpsc::channel(16);
        let client = self.clone();
        let call_id = call_id.to_string();
        tokio::spawn(async move {
            client.run_sideband(call_id, socket, rx, events).await;
        });
        Ok(tx)
    }

    async fn run_sideband(
        mut self,
        call_id: String,
        mut socket: Socket,
        mut commands: mpsc::Receiver<SidebandCommand>,
        events: mpsc::Sender<SidebandEvent>,
    ) {
        let mut attempts = 0_u32;
        let mut auth_changes = tokio::time::interval(Duration::from_secs(1));
        auth_changes.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let connected_at = tokio::time::Instant::now();
            let disconnected = loop {
                tokio::select! {
                    biased;
                    command = commands.recv() => {
                        let Some(mut command) = command else { let _ = tokio::time::timeout(self.timeout, socket.close(None)).await; return; };
                        if command.completion.is_closed() && !command.close { continue; }
                        let mut error = write_command_frames(
                            &mut socket, command.frames, command.close,
                            &mut command.completion, self.timeout,
                        ).await.err();
                        if command.close {
                            let deadline = tokio::time::Instant::now() + self.timeout;
                            if error.is_none() {
                                match tokio::time::timeout_at(deadline, socket.send(Message::Text(json!({"type":"session.close"}).to_string().into()))).await {
                                    Ok(Ok(())) => {},
                                    _ => error = Some("Live call close transport failed; remote closure is unconfirmed".into()),
                                }
                            }
                            let remote_closed = if error.is_none() {
                                tokio::time::timeout_at(deadline, async {
                                    loop {
                                        match socket.next().await {
                                            Some(Ok(Message::Close(_))) => break false,
                                            Some(Ok(Message::Text(text))) => {
                                                if let Some(event) = self.decode_event(&text) {
                                                    if is_media_event(&event) { continue; }
                                                    let closed = event.get("type").and_then(Value::as_str) == Some("session.closed");
                                                    if events.send(SidebandEvent { call_id: call_id.clone(), event }).await.is_err() { break false; }
                                                    if closed { break true; }
                                                }
                                            }
                                            Some(Ok(_)) => {},
                                            _ => break false,
                                        }
                                    }
                                }).await.unwrap_or(false)
                            } else { false };
                            let _ = events.send(SidebandEvent {call_id:call_id.clone(),event:json!({"type":"octopus.transport.stopped","provider_close_sent":error.is_none(),"remote_closed":remote_closed})}).await;
                            let _ = command.completion.send(error.map_or(Ok(remote_closed), Err));
                            let _ = tokio::time::timeout(self.timeout, socket.close(None)).await;
                            return;
                        }
                        let failed = error.is_some();
                        let _ = command.completion.send(error.map_or(Ok(false), Err));
                        if failed { break "update_transport_lost"; }
                    }
                    _ = auth_changes.tick() => {
                        let checked = match self.config.private.auth.changed(&self.credential) {
                            Ok(false) => continue,
                            Ok(true) => self.config.private.auth.resolve().await,
                            Err(error) => Err(error),
                        };
                        match checked {
                            Ok(credential) if credential.owner == self.credential.owner => { self.credential = credential; },
                            _ => {
                                let sent = tokio::time::timeout(self.timeout, socket.send(Message::Text(json!({"type":"session.close"}).to_string().into()))).await;
                                let _ = tokio::time::timeout(self.timeout, socket.close(None)).await;
                                let _ = events.send(SidebandEvent {call_id,event:json!({"type":"octopus.transport.failed","reason":"authentication_changed_or_unavailable","provider_close_sent":matches!(sent, Ok(Ok(()))),"remote_closed":false})}).await;
                                return;
                            }
                        }
                    }
                    message = socket.next() => {
                        match message {
                            Some(Ok(Message::Text(text))) => {
                                let Some(event) = self.decode_event(&text) else {
                                    let _ = events.send(SidebandEvent {call_id, event:json!({"type":"octopus.transport.failed","reason":"invalid_or_sensitive_provider_event"})}).await;
                                    let _ = tokio::time::timeout(self.timeout, socket.close(None)).await; return;
                                };
                                if is_media_event(&event) { continue; }
                                let closed = event.get("type").and_then(Value::as_str) == Some("session.closed");
                                if events.send(SidebandEvent {call_id:call_id.clone(),event}).await.is_err() { let _ = tokio::time::timeout(self.timeout, socket.close(None)).await; return; }
                                if closed { let _ = tokio::time::timeout(self.timeout, socket.close(None)).await; return; }
                            }
                            Some(Ok(Message::Ping(bytes))) => { if !matches!(tokio::time::timeout(self.timeout, socket.send(Message::Pong(bytes))).await, Ok(Ok(()))) { break "ping_transport_lost"; } },
                            Some(Ok(Message::Close(Some(frame)))) if frame.code == tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Normal => {
                                let _ = events.send(SidebandEvent {call_id,event:json!({"type":"octopus.transport.stopped","provider_close_sent":false,"remote_closed":false,"socket_closed":true})}).await; return;
                            }
                            Some(Ok(Message::Pong(_))) => {},
                            Some(Ok(Message::Binary(_))) => { break "unexpected_binary_sideband_frame"; },
                            _ => break "sideband_transport_lost",
                        }
                    }
                }
            };
            if connected_at.elapsed() >= Duration::from_secs(30) {
                attempts = 0;
            }
            attempts += 1;
            if attempts > self.config.private.reconnect_attempts {
                let _ = events
                    .send(SidebandEvent {
                        call_id,
                        event: json!({"type":"octopus.transport.failed","reason":disconnected}),
                    })
                    .await;
                return;
            }
            if events
                .send(SidebandEvent {
                    call_id: call_id.clone(),
                    event: json!({"type":"octopus.transport.reconnecting","attempt":attempts}),
                })
                .await
                .is_err()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(
                (200_u64 << attempts.min(5)).min(5_000),
            ))
            .await;
            let refreshed = match Self::connect(&self.config).await {
                Ok(client) if client.owner() == self.owner() => client,
                _ => {
                    let _ = events.send(SidebandEvent {call_id,event:json!({"type":"octopus.transport.failed","reason":"authentication_changed_or_unavailable"})}).await;
                    return;
                }
            };
            match refreshed.socket(&call_id).await {
                Ok(connected) => {
                    self = refreshed;
                    socket = connected;
                    if events
                        .send(SidebandEvent {
                            call_id: call_id.clone(),
                            event: json!({"type":"octopus.transport.connected"}),
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Err(error) => {
                    let _ = events
                        .send(SidebandEvent {
                            call_id,
                            event: json!({"type":"octopus.transport.failed","reason":error}),
                        })
                        .await;
                    return;
                }
            }
        }
    }

    fn decode_event(&self, text: &str) -> Option<Value> {
        if self.credential.contains_secret(text.as_bytes()) {
            return None;
        }
        let value = decode_provider_event(text)?;
        if self
            .credential
            .contains_secret(value.to_string().as_bytes())
        {
            return None;
        }
        Some(value)
    }
}

fn decode_provider_event(text: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(text).ok()?;
    let event_type = value.get("type")?.as_str()?;
    if matches!(
        event_type,
        "octopus.transport.reconnecting"
            | "octopus.transport.connected"
            | "octopus.transport.failed"
            | "octopus.transport.stopped"
    ) {
        return None;
    }
    if let Some(id) = value.get("event_id") {
        crate::validate_identity(id.as_str()?, "provider event identity").ok()?;
    }
    Some(value)
}

async fn write_command_frames<S: Sink<Message> + Unpin>(
    socket: &mut S,
    frames: Vec<Value>,
    close: bool,
    completion: &mut oneshot::Sender<Result<bool, String>>,
    timeout: Duration,
) -> Result<(), String> {
    let writing = tokio::time::timeout(timeout, async {
        for frame in frames {
            socket
                .send(Message::Text(frame.to_string().into()))
                .await
                .map_err(|_| "Live update transport failed; injection outcome unknown; update will not be replayed".to_string())?;
        }
        Ok(())
    });
    tokio::pin!(writing);
    let outcome = if close {
        writing.await
    } else {
        tokio::select! {
            biased;
            _ = completion.closed() => return Err("Live update command cancelled; injection outcome unknown; update will not be replayed".into()),
            result = &mut writing => result,
        }
    };
    outcome.map_err(|_| "Live update transport timed out; injection outcome unknown; update will not be replayed".to_string())?
}

fn sideband_tls_config() -> Result<rustls::ClientConfig, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    if roots.is_empty() {
        return Err("Live sideband TLS has no usable native trust roots".into());
    }
    rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map(|builder| builder.with_root_certificates(roots).with_no_client_auth())
    .map_err(|_| "Live sideband TLS protocol initialization failed".into())
}

fn bounded_error_text(value: &str, maximum: usize) -> String {
    let mut text = String::new();
    for character in value.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if text.len() + character.len_utf8() > maximum {
            text.push_str("...");
            break;
        }
        text.push(character);
    }
    text
}

fn is_media_event(event: &Value) -> bool {
    matches!(
        event.get("type").and_then(Value::as_str),
        Some(
            "output_audio.delta"
                | "input_audio.append"
                | "session.output_audio.delta"
                | "session.input_audio.append"
        )
    )
}

pub fn validate_endpoint(value: &str, websocket: bool) -> Result<reqwest::Url, String> {
    if value.is_empty() || value.trim() != value {
        return Err("Live endpoint must be nonempty and trimmed".into());
    }
    let url = reqwest::Url::parse(value).map_err(|_| "invalid Live endpoint")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port() == Some(0)
    {
        return Err(
            "Live endpoint must not contain credentials, query, fragment or port zero".into(),
        );
    }
    let (secure, insecure) = if websocket {
        ("wss", "ws")
    } else {
        ("https", "http")
    };
    let host = url.host_str().ok_or("Live endpoint has no host")?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
    if url.scheme() != secure && !(url.scheme() == insecure && loopback) {
        return Err("Live endpoint requires TLS outside lexical loopback".into());
    }
    Ok(url)
}

fn validate_call_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 256
        || value == "."
        || value == ".."
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err("invalid provider call identity".into());
    }
    Ok(())
}

pub fn decode_call_id(location: &str) -> Result<String, String> {
    let path = location.split('?').next().ok_or("empty Location")?;
    let value = path
        .rsplit('/')
        .find(|part| !part.is_empty())
        .ok_or("Location has no call identity")?;
    validate_call_id(value)?;
    let uuid = value.len() == 36
        && value.char_indices().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        });
    if !(value.starts_with("rtc_") && value.len() > 4) && !uuid {
        return Err("Location has no supported call identity".into());
    }
    Ok(value.into())
}

#[cfg(test)]
#[path = "../tests/support/realtime_writes.rs"]
mod tests;
