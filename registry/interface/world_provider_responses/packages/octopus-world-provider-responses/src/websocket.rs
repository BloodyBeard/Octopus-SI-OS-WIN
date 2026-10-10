use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use reqwest::header::HeaderMap;
use serde::Deserialize;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WebSocketUrlScheme {
    HttpToWsHttpsToWss,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WebSocketReconnectPolicy {
    BeforeRequestOnly,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WebSocketResumePolicy {
    UnsupportedFailClosed,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResponsesWebSocketConfig {
    pub url_scheme: WebSocketUrlScheme,
    pub request_event_type: String,
    pub connect_timeout_ms: u64,
    pub write_timeout_ms: u64,
    pub idle_timeout_ms: u64,
    pub keepalive_interval_ms: u64,
    pub keepalive_pong_timeout_ms: u64,
    pub read_buffer_bytes: usize,
    pub write_buffer_bytes: usize,
    pub max_write_buffer_bytes: usize,
    pub max_message_bytes: usize,
    pub max_frame_bytes: usize,
    pub connection_reuse: bool,
    pub reconnect_policy: WebSocketReconnectPolicy,
    pub resume_policy: WebSocketResumePolicy,
}

impl ResponsesWebSocketConfig {
    pub(crate) fn validate(&self, max_response_bytes: usize) -> Result<(), String> {
        if self.reconnect_policy != WebSocketReconnectPolicy::BeforeRequestOnly {
            return Err(
                "Responses websocket reconnect_policy must be before_request_only".to_string(),
            );
        }
        if self.resume_policy != WebSocketResumePolicy::UnsupportedFailClosed {
            return Err(
                "Responses websocket resume_policy must be unsupported_fail_closed".to_string(),
            );
        }
        if self.request_event_type != "response.create" {
            return Err(
                "Responses websocket.request_event_type must be 'response.create'".to_string(),
            );
        }
        for (field, value) in [
            ("connect_timeout_ms", self.connect_timeout_ms),
            ("write_timeout_ms", self.write_timeout_ms),
            ("idle_timeout_ms", self.idle_timeout_ms),
            ("keepalive_interval_ms", self.keepalive_interval_ms),
            ("keepalive_pong_timeout_ms", self.keepalive_pong_timeout_ms),
        ] {
            if value == 0 || value > 30 * 60 * 1000 {
                return Err(format!(
                    "Responses websocket.{field} must be within 1..=1800000"
                ));
            }
        }
        if self.keepalive_interval_ms >= self.idle_timeout_ms {
            return Err(
                "Responses websocket.keepalive_interval_ms must be below idle_timeout_ms"
                    .to_string(),
            );
        }
        if self.keepalive_pong_timeout_ms >= self.idle_timeout_ms {
            return Err(
                "Responses websocket.keepalive_pong_timeout_ms must be below idle_timeout_ms"
                    .to_string(),
            );
        }
        if self.keepalive_pong_timeout_ms >= self.keepalive_interval_ms {
            return Err(
                "Responses websocket.keepalive_pong_timeout_ms must be below keepalive_interval_ms"
                    .to_string(),
            );
        }
        if self.read_buffer_bytes == 0 || self.read_buffer_bytes > self.max_message_bytes {
            return Err(
                "Responses websocket.read_buffer_bytes must be within 1..=max_message_bytes"
                    .to_string(),
            );
        }
        if self.write_buffer_bytes == 0 || self.write_buffer_bytes > self.max_message_bytes {
            return Err(
                "Responses websocket.write_buffer_bytes must be within 1..=max_message_bytes"
                    .to_string(),
            );
        }
        let required_write_buffer = self
            .write_buffer_bytes
            .checked_add(self.max_message_bytes)
            .ok_or_else(|| {
                "Responses websocket write buffer limit calculation overflowed".to_string()
            })?;
        if self.max_write_buffer_bytes < required_write_buffer {
            return Err(
                "Responses websocket.max_write_buffer_bytes must cover write_buffer_bytes plus one maximum request message"
                    .to_string(),
            );
        }
        if self.max_frame_bytes == 0
            || self.max_message_bytes == 0
            || self.max_frame_bytes > self.max_message_bytes
            || self.max_message_bytes > max_response_bytes
        {
            return Err(
                "Responses websocket frame/message limits must satisfy 0 < frame <= message <= max_response_bytes"
                    .to_string(),
            );
        }
        Ok(())
    }

    fn tungstenite_config(&self) -> WebSocketConfig {
        WebSocketConfig::default()
            .read_buffer_size(self.read_buffer_bytes)
            .write_buffer_size(self.write_buffer_bytes)
            .max_write_buffer_size(self.max_write_buffer_bytes)
            .max_message_size(Some(self.max_message_bytes))
            .max_frame_size(Some(self.max_frame_bytes))
            .accept_unmasked_frames(false)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WebSocketFailureStage {
    BeforeRequest,
    AfterRequest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WebSocketFailureKind {
    Transport,
    ResponseLimit,
    ResponseContract,
}

#[derive(Debug)]
pub(crate) struct ResponsesWebSocketFailure {
    pub kind: WebSocketFailureKind,
    pub stage: WebSocketFailureStage,
    pub status: Option<u16>,
    pub headers: Box<HeaderMap>,
    pub bytes: Vec<u8>,
    pub detail: String,
}

pub(crate) struct ResponsesWebSocketExchange {
    pub status: u16,
    pub headers: HeaderMap,
    pub request_bytes: Vec<u8>,
    pub response_bytes: Vec<u8>,
    pub events: Vec<Value>,
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Connection {
    socket: Socket,
    status: u16,
    headers: HeaderMap,
    probe_generation: u64,
}

pub(crate) struct ResponsesWebSocketTransport {
    endpoint: reqwest::Url,
    config: ResponsesWebSocketConfig,
    max_response_bytes: usize,
    connection: Mutex<Option<Connection>>,
}

impl ResponsesWebSocketTransport {
    pub(crate) fn new(
        endpoint: reqwest::Url,
        config: ResponsesWebSocketConfig,
        max_response_bytes: usize,
    ) -> Result<Self, String> {
        config.validate(max_response_bytes)?;
        let endpoint = websocket_endpoint(endpoint, config.url_scheme)?;
        Ok(Self {
            endpoint,
            config,
            max_response_bytes,
            connection: Mutex::new(None),
        })
    }

    pub(crate) fn endpoint(&self) -> &reqwest::Url {
        &self.endpoint
    }

    pub(crate) async fn exchange(
        &self,
        request_bytes: Vec<u8>,
        headers: HeaderMap,
    ) -> Result<ResponsesWebSocketExchange, ResponsesWebSocketFailure> {
        if request_bytes.len() > self.config.max_message_bytes {
            return Err(ResponsesWebSocketFailure {
                kind: WebSocketFailureKind::ResponseLimit,
                stage: WebSocketFailureStage::BeforeRequest,
                status: None,
                headers: Box::new(HeaderMap::new()),
                bytes: Vec::new(),
                detail: format!(
                    "Responses websocket request exceeds descriptor maximum {} bytes",
                    self.config.max_message_bytes
                ),
            });
        }
        let mut slot = self.connection.lock().await;
        if let Some(connection) = slot.as_mut() {
            if self.preflight(connection).await.is_err() {
                *slot = None;
            }
        }
        if slot.is_none() {
            *slot = Some(self.connect(headers).await?);
        }
        let connection = slot.as_mut().ok_or_else(|| ResponsesWebSocketFailure {
            kind: WebSocketFailureKind::Transport,
            stage: WebSocketFailureStage::BeforeRequest,
            status: None,
            headers: Box::new(HeaderMap::new()),
            bytes: Vec::new(),
            detail: "Responses websocket connection was not established".to_string(),
        })?;
        let result = self.run_exchange(connection, request_bytes).await;
        match result {
            Ok(exchange) => {
                if !self.config.connection_reuse {
                    if let Some(mut connection) = slot.take() {
                        let _ = tokio::time::timeout(
                            Duration::from_millis(self.config.write_timeout_ms),
                            connection.socket.close(None),
                        )
                        .await;
                    }
                }
                Ok(exchange)
            }
            Err(failure) => {
                *slot = None;
                Err(failure)
            }
        }
    }

    async fn connect(&self, headers: HeaderMap) -> Result<Connection, ResponsesWebSocketFailure> {
        let request = build_websocket_request(&self.endpoint, headers).map_err(|error| {
            ResponsesWebSocketFailure {
                kind: WebSocketFailureKind::ResponseContract,
                stage: WebSocketFailureStage::BeforeRequest,
                status: None,
                headers: Box::new(HeaderMap::new()),
                bytes: Vec::new(),
                detail: error,
            }
        })?;
        let connected = tokio::time::timeout(
            Duration::from_millis(self.config.connect_timeout_ms),
            tokio_tungstenite::connect_async_tls_with_config(
                request,
                Some(self.config.tungstenite_config()),
                false,
                None,
            ),
        )
        .await
        .map_err(|_| ResponsesWebSocketFailure {
            kind: WebSocketFailureKind::Transport,
            stage: WebSocketFailureStage::BeforeRequest,
            status: None,
            headers: Box::new(HeaderMap::new()),
            bytes: Vec::new(),
            detail: format!(
                "Responses websocket connect timed out after {} ms",
                self.config.connect_timeout_ms
            ),
        })?
        .map_err(|error| map_connect_error(error, self.max_response_bytes))?;
        let (socket, response) = connected;
        Ok(Connection {
            socket,
            status: response.status().as_u16(),
            headers: response.headers().clone(),
            probe_generation: 0,
        })
    }

    async fn preflight(&self, connection: &mut Connection) -> Result<(), String> {
        connection.probe_generation = connection.probe_generation.saturating_add(1);
        let payload = format!("octopus-responses-{}", connection.probe_generation).into_bytes();
        write_with_timeout(
            &mut connection.socket,
            Message::Ping(payload.clone().into()),
            self.config.write_timeout_ms,
        )
        .await
        .map_err(|error| error.to_string())?;
        let deadline =
            Instant::now() + Duration::from_millis(self.config.keepalive_pong_timeout_ms);
        loop {
            let wait = deadline.saturating_duration_since(Instant::now());
            if wait.is_zero() {
                return Err("Responses websocket preflight pong timed out".to_string());
            }
            let message = tokio::time::timeout(wait, connection.socket.next())
                .await
                .map_err(|_| "Responses websocket preflight pong timed out".to_string())?
                .ok_or_else(|| "Responses websocket closed during preflight".to_string())?
                .map_err(|error| format!("Responses websocket preflight failed: {error}"))?;
            match message {
                Message::Pong(received) if received.as_ref() == payload.as_slice() => return Ok(()),
                Message::Ping(received) => {
                    write_with_timeout(
                        &mut connection.socket,
                        Message::Pong(received),
                        self.config.write_timeout_ms,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                }
                Message::Pong(_) => {}
                Message::Text(_) | Message::Binary(_) | Message::Close(_) | Message::Frame(_) => {
                    return Err(
                        "Responses websocket carried an unexpected frame before response.create"
                            .to_string(),
                    );
                }
            }
        }
    }

    async fn run_exchange(
        &self,
        connection: &mut Connection,
        request_bytes: Vec<u8>,
    ) -> Result<ResponsesWebSocketExchange, ResponsesWebSocketFailure> {
        let request_text = String::from_utf8(request_bytes.clone()).map_err(|error| {
            ResponsesWebSocketFailure {
                kind: WebSocketFailureKind::ResponseContract,
                stage: WebSocketFailureStage::BeforeRequest,
                status: None,
                headers: Box::new(HeaderMap::new()),
                bytes: Vec::new(),
                detail: format!("Responses websocket request is not UTF-8: {error}"),
            }
        })?;
        write_with_timeout(
            &mut connection.socket,
            Message::Text(request_text.into()),
            self.config.write_timeout_ms,
        )
        .await
        .map_err(|error| ResponsesWebSocketFailure {
            kind: WebSocketFailureKind::Transport,
            stage: WebSocketFailureStage::AfterRequest,
            status: Some(connection.status),
            headers: Box::new(connection.headers.clone()),
            bytes: Vec::new(),
            detail: error,
        })?;
        let mut response_bytes = Vec::new();
        let mut events = Vec::new();
        let mut last_received = Instant::now();
        let mut next_ping =
            last_received + Duration::from_millis(self.config.keepalive_interval_ms);
        let mut pong_deadline: Option<Instant> = None;
        loop {
            let idle_deadline = last_received + Duration::from_millis(self.config.idle_timeout_ms);
            let deadline = pong_deadline
                .map(|deadline| deadline.min(idle_deadline).min(next_ping))
                .unwrap_or_else(|| idle_deadline.min(next_ping));
            let wait = deadline.saturating_duration_since(Instant::now());
            let polled = tokio::time::timeout(wait, connection.socket.next()).await;
            let message = match polled {
                Ok(Some(Ok(message))) => message,
                Ok(Some(Err(error))) => {
                    return Err(after_request_transport_failure(
                        connection,
                        response_bytes,
                        format!("Responses websocket read failed: {error}"),
                    ));
                }
                Ok(None) => {
                    return Err(after_request_transport_failure(
                        connection,
                        response_bytes,
                        "Responses websocket closed before a terminal event".to_string(),
                    ));
                }
                Err(_) => {
                    let now = Instant::now();
                    if pong_deadline.is_some_and(|deadline| now >= deadline) {
                        return Err(after_request_transport_failure(
                            connection,
                            response_bytes,
                            "Responses websocket keepalive pong timed out".to_string(),
                        ));
                    }
                    if now >= idle_deadline {
                        return Err(after_request_transport_failure(
                            connection,
                            response_bytes,
                            format!(
                                "Responses websocket idle timeout after {} ms",
                                self.config.idle_timeout_ms
                            ),
                        ));
                    }
                    if now >= next_ping {
                        let payload = format!("octopus-responses-keepalive-{}", events.len());
                        write_with_timeout(
                            &mut connection.socket,
                            Message::Ping(payload.into_bytes().into()),
                            self.config.write_timeout_ms,
                        )
                        .await
                        .map_err(|error| {
                            after_request_transport_failure(
                                connection,
                                response_bytes.clone(),
                                error,
                            )
                        })?;
                        pong_deadline = Some(
                            now + Duration::from_millis(self.config.keepalive_pong_timeout_ms),
                        );
                        next_ping = now + Duration::from_millis(self.config.keepalive_interval_ms);
                    }
                    continue;
                }
            };
            last_received = Instant::now();
            pong_deadline = None;
            next_ping = last_received + Duration::from_millis(self.config.keepalive_interval_ms);
            match message {
                Message::Text(text) => {
                    let raw = text.as_bytes();
                    append_bounded(
                        &mut response_bytes,
                        raw,
                        self.max_response_bytes,
                        connection,
                    )?;
                    append_bounded(
                        &mut response_bytes,
                        b"\n",
                        self.max_response_bytes,
                        connection,
                    )?;
                    let value: Value =
                        serde_json::from_slice(raw).map_err(|error| ResponsesWebSocketFailure {
                            kind: WebSocketFailureKind::ResponseContract,
                            stage: WebSocketFailureStage::AfterRequest,
                            status: Some(connection.status),
                            headers: Box::new(connection.headers.clone()),
                            bytes: response_bytes.clone(),
                            detail: format!("Responses websocket text frame is not JSON: {error}"),
                        })?;
                    let event_type = value
                        .get("type")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty() && value.trim() == *value)
                        .ok_or_else(|| ResponsesWebSocketFailure {
                            kind: WebSocketFailureKind::ResponseContract,
                            stage: WebSocketFailureStage::AfterRequest,
                            status: Some(connection.status),
                            headers: Box::new(connection.headers.clone()),
                            bytes: response_bytes.clone(),
                            detail: "Responses websocket event has no valid type".to_string(),
                        })?;
                    let terminal = matches!(
                        event_type,
                        "response.completed" | "response.failed" | "response.incomplete" | "error"
                    );
                    events.push(value);
                    if terminal {
                        return Ok(ResponsesWebSocketExchange {
                            status: connection.status,
                            headers: connection.headers.clone(),
                            request_bytes,
                            response_bytes,
                            events,
                        });
                    }
                }
                Message::Ping(payload) => {
                    write_with_timeout(
                        &mut connection.socket,
                        Message::Pong(payload),
                        self.config.write_timeout_ms,
                    )
                    .await
                    .map_err(|error| {
                        after_request_transport_failure(connection, response_bytes.clone(), error)
                    })?;
                }
                Message::Pong(_) => {}
                Message::Binary(_) => {
                    return Err(ResponsesWebSocketFailure {
                        kind: WebSocketFailureKind::ResponseContract,
                        stage: WebSocketFailureStage::AfterRequest,
                        status: Some(connection.status),
                        headers: Box::new(connection.headers.clone()),
                        bytes: response_bytes,
                        detail: "Responses websocket returned an unexpected binary frame"
                            .to_string(),
                    });
                }
                Message::Close(frame) => {
                    return Err(after_request_transport_failure(
                        connection,
                        response_bytes,
                        format!("Responses websocket closed before a terminal event: {frame:?}"),
                    ));
                }
                Message::Frame(_) => {
                    return Err(ResponsesWebSocketFailure {
                        kind: WebSocketFailureKind::ResponseContract,
                        stage: WebSocketFailureStage::AfterRequest,
                        status: Some(connection.status),
                        headers: Box::new(connection.headers.clone()),
                        bytes: response_bytes,
                        detail: "Responses websocket returned an unexpected raw frame".to_string(),
                    });
                }
            }
        }
    }
}

fn websocket_endpoint(
    mut endpoint: reqwest::Url,
    policy: WebSocketUrlScheme,
) -> Result<reqwest::Url, String> {
    match policy {
        WebSocketUrlScheme::HttpToWsHttpsToWss => {
            let scheme = match endpoint.scheme() {
                "http" => "ws",
                "https" => "wss",
                scheme => {
                    return Err(format!(
                        "Responses websocket URL cannot map endpoint scheme '{scheme}'"
                    ));
                }
            };
            endpoint
                .set_scheme(scheme)
                .map_err(|_| "Responses websocket URL scheme mapping failed".to_string())?;
        }
    }
    Ok(endpoint)
}

fn build_websocket_request(
    endpoint: &reqwest::Url,
    headers: HeaderMap,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, String> {
    let mut request = endpoint
        .as_str()
        .into_client_request()
        .map_err(|error| format!("Responses websocket request could not be built: {error}"))?;
    request.headers_mut().extend(headers);
    Ok(request)
}

async fn write_with_timeout(
    socket: &mut Socket,
    message: Message,
    timeout_ms: u64,
) -> Result<(), String> {
    tokio::time::timeout(Duration::from_millis(timeout_ms), socket.send(message))
        .await
        .map_err(|_| format!("Responses websocket write timed out after {timeout_ms} ms"))?
        .map_err(|error| format!("Responses websocket write failed: {error}"))
}

fn append_bounded(
    destination: &mut Vec<u8>,
    bytes: &[u8],
    maximum: usize,
    connection: &Connection,
) -> Result<(), ResponsesWebSocketFailure> {
    let next =
        destination
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| ResponsesWebSocketFailure {
                kind: WebSocketFailureKind::ResponseLimit,
                stage: WebSocketFailureStage::AfterRequest,
                status: Some(connection.status),
                headers: Box::new(connection.headers.clone()),
                bytes: destination.clone(),
                detail: "Responses websocket aggregate size overflowed".to_string(),
            })?;
    if next > maximum {
        return Err(ResponsesWebSocketFailure {
            kind: WebSocketFailureKind::ResponseLimit,
            stage: WebSocketFailureStage::AfterRequest,
            status: Some(connection.status),
            headers: Box::new(connection.headers.clone()),
            bytes: destination.clone(),
            detail: format!(
                "Responses websocket aggregate exceeds descriptor maximum {maximum} bytes"
            ),
        });
    }
    destination.extend_from_slice(bytes);
    Ok(())
}

fn after_request_transport_failure(
    connection: &Connection,
    bytes: Vec<u8>,
    detail: String,
) -> ResponsesWebSocketFailure {
    ResponsesWebSocketFailure {
        kind: WebSocketFailureKind::Transport,
        stage: WebSocketFailureStage::AfterRequest,
        status: Some(connection.status),
        headers: Box::new(connection.headers.clone()),
        bytes,
        detail,
    }
}

fn map_connect_error(error: WsError, maximum: usize) -> ResponsesWebSocketFailure {
    match error {
        WsError::Http(response) => {
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            let bytes = response.body().clone().unwrap_or_default();
            if bytes.len() > maximum {
                ResponsesWebSocketFailure {
                    kind: WebSocketFailureKind::ResponseLimit,
                    stage: WebSocketFailureStage::BeforeRequest,
                    status: Some(status),
                    headers: Box::new(headers),
                    bytes: Vec::new(),
                    detail: format!(
                        "Responses websocket handshake body exceeds descriptor maximum {maximum} bytes"
                    ),
                }
            } else {
                ResponsesWebSocketFailure {
                    kind: WebSocketFailureKind::Transport,
                    stage: WebSocketFailureStage::BeforeRequest,
                    status: Some(status),
                    headers: Box::new(headers),
                    bytes,
                    detail: format!("Responses websocket handshake returned HTTP {status}"),
                }
            }
        }
        other => ResponsesWebSocketFailure {
            kind: WebSocketFailureKind::Transport,
            stage: WebSocketFailureStage::BeforeRequest,
            status: None,
            headers: Box::new(HeaderMap::new()),
            bytes: Vec::new(),
            detail: format!("Responses websocket connect failed: {other}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderValue, AUTHORIZATION};
    use serde_json::json;
    use tokio::net::TcpListener;

    fn config() -> ResponsesWebSocketConfig {
        ResponsesWebSocketConfig {
            url_scheme: WebSocketUrlScheme::HttpToWsHttpsToWss,
            request_event_type: "response.create".to_string(),
            connect_timeout_ms: 1000,
            write_timeout_ms: 1000,
            idle_timeout_ms: 5000,
            keepalive_interval_ms: 1000,
            keepalive_pong_timeout_ms: 500,
            read_buffer_bytes: 4096,
            write_buffer_bytes: 4096,
            max_write_buffer_bytes: 131072,
            max_message_bytes: 65536,
            max_frame_bytes: 16384,
            connection_reuse: true,
            reconnect_policy: WebSocketReconnectPolicy::BeforeRequestOnly,
            resume_policy: WebSocketResumePolicy::UnsupportedFailClosed,
        }
    }

    #[test]
    fn descriptor_websocket_limits_and_url_mapping_are_strict() {
        let config = config();
        config.validate(131072).expect("websocket config validates");
        let endpoint = websocket_endpoint(
            reqwest::Url::parse("https://api.openai.com/v1/responses").expect("endpoint parses"),
            config.url_scheme,
        )
        .expect("websocket endpoint resolves");
        assert_eq!(endpoint.as_str(), "wss://api.openai.com/v1/responses");

        let mut invalid = config;
        invalid.max_frame_bytes = invalid.max_message_bytes + 1;
        assert!(invalid.validate(131072).is_err());
    }

    #[tokio::test]
    async fn native_exchange_preserves_url_headers_and_response_create_frames() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("websocket listener binds");
        let address = listener.local_addr().expect("listener address resolves");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("websocket client connects");
            let mut websocket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("websocket handshake succeeds");
            let request = websocket
                .next()
                .await
                .expect("response.create frame arrives")
                .expect("response.create frame reads")
                .into_text()
                .expect("response.create frame is text");
            let request: Value =
                serde_json::from_str(&request).expect("response.create frame parses");
            assert_eq!(request.get("type"), Some(&json!("response.create")));
            websocket
                .send(Message::Text(
                    json!({
                        "type": "response.completed",
                        "sequence_number": 1,
                        "response": {
                            "id": "resp_ws",
                            "status": "completed",
                            "model": "gpt-test",
                            "output": [{
                                "type": "message",
                                "role": "assistant",
                                "content": [{"type": "output_text", "text": "done"}]
                            }],
                            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("terminal frame sends");
        });
        let mut config = config();
        config.connection_reuse = false;
        let transport = ResponsesWebSocketTransport::new(
            reqwest::Url::parse(&format!("http://{address}/v1/responses"))
                .expect("local endpoint parses"),
            config,
            131072,
        )
        .expect("websocket transport resolves");
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer test-secret"),
        );
        headers.insert("x-provider-header", HeaderValue::from_static("descriptor"));
        let request = build_websocket_request(transport.endpoint(), headers.clone())
            .expect("websocket request builds");
        assert_eq!(request.uri().path(), "/v1/responses");
        assert_eq!(
            request
                .headers()
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer test-secret")
        );
        assert_eq!(
            request
                .headers()
                .get("x-provider-header")
                .and_then(|value| value.to_str().ok()),
            Some("descriptor")
        );
        let exchange = transport
            .exchange(
                serde_json::to_vec(&json!({"type": "response.create", "stream": true}))
                    .expect("request encodes"),
                headers,
            )
            .await
            .expect("websocket exchange completes");
        assert_eq!(exchange.status, 101);
        assert_eq!(exchange.events.len(), 1);
        assert_eq!(
            exchange.events[0].get("type"),
            Some(&json!("response.completed"))
        );
        server.await.expect("websocket server completes");
    }
}
