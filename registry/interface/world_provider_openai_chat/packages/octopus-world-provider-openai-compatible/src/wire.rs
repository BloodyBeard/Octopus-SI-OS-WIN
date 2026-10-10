use std::collections::BTreeMap;
use std::sync::Arc;

use octopus_azure_auth::AzureAuthFailureEvidence;
use octopus_provider_credential_pool::CredentialSelection;
use octopus_secrets::{LocalFileKeyringStore, SecretsBackendKind, SecretsManager};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::config::{
    parse_secret_ref, AuthConfig, AuthSource, CredentialAttemptStart, ProviderConfig,
    ResponseConfig, StreamAccumulation,
};
use crate::dispatch::DispatchAdmission;

#[derive(Clone, Debug, Serialize)]
pub struct ProviderErrorEvidence {
    pub class: &'static str,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<EndpointEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<BodyEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authentication: Option<Box<AzureAuthFailureEvidence>>,
}

#[derive(Debug)]
pub enum ProviderDriverError {
    Transport(Box<ProviderErrorEvidence>),
    Retryable(Box<ProviderErrorEvidence>),
    Protocol(Box<ProviderErrorEvidence>),
    FailedClosed(Box<ProviderErrorEvidence>),
    AuthenticatedRefreshRequired(Box<ProviderErrorEvidence>),
}

#[derive(Clone, Debug, Serialize)]
pub struct EndpointEvidence {
    pub scheme: String,
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub path: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct HttpEvidence {
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub identifiers: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BodyEvidence {
    pub bytes: usize,
    pub blake3: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct StreamEvidence {
    pub requested: bool,
    pub data_events: usize,
    pub transport_chunks: usize,
    pub done: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProviderResponseEvidence {
    pub endpoint: EndpointEvidence,
    pub http: HttpEvidence,
    pub body: BodyEvidence,
    pub stream: StreamEvidence,
}

pub(crate) struct WireOutcome {
    pub response: Value,
    pub evidence: ProviderResponseEvidence,
}

impl ProviderDriverError {
    pub fn transport(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Transport(Box::new(ProviderErrorEvidence::new(
            "transport",
            code,
            message,
        )))
    }

    pub fn protocol(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Protocol(Box::new(ProviderErrorEvidence::new(
            "protocol", code, message,
        )))
    }

    pub fn failed_closed(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::FailedClosed(Box::new(ProviderErrorEvidence::new(
            "failed_closed",
            code,
            message,
        )))
    }

    pub fn retryable_failure(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Retryable(Box::new(ProviderErrorEvidence::new(
            "retryable",
            code,
            message,
        )))
    }

    pub fn evidence(&self) -> &ProviderErrorEvidence {
        match self {
            Self::Transport(evidence)
            | Self::Retryable(evidence)
            | Self::Protocol(evidence)
            | Self::FailedClosed(evidence)
            | Self::AuthenticatedRefreshRequired(evidence) => evidence,
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(self, Self::Transport(_) | Self::Retryable(_))
    }

    pub fn authenticated_refresh_required(&self) -> bool {
        matches!(self, Self::AuthenticatedRefreshRequired(_))
    }
}

impl std::fmt::Display for ProviderDriverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let evidence = self.evidence();
        write!(f, "{}: {}", evidence.code, evidence.message)
    }
}

impl std::error::Error for ProviderDriverError {}

impl ProviderErrorEvidence {
    pub fn new(class: &'static str, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            class,
            code: code.into(),
            message: message.into(),
            endpoint: None,
            http: None,
            body: None,
            authentication: None,
        }
    }

    fn with_endpoint(mut self, endpoint: EndpointEvidence) -> Self {
        self.endpoint = Some(endpoint);
        self
    }

    fn with_response(mut self, http: HttpEvidence, body: BodyEvidence) -> Self {
        self.http = Some(http);
        self.body = Some(body);
        self
    }

    fn with_authentication(mut self, authentication: AzureAuthFailureEvidence) -> Self {
        self.authentication = Some(Box::new(authentication));
        self
    }
}

pub(crate) async fn execute_chat_completion(
    client: &reqwest::Client,
    config: &ProviderConfig,
    request_body: &Map<String, Value>,
    model: &str,
    messages: &[Value],
    tools: &[Value],
    dispatch: &DispatchAdmission<'_>,
) -> Result<WireOutcome, ProviderDriverError> {
    let mut body = Value::Object(request_body.clone());
    body["model"] = Value::String(model.to_string());
    body["messages"] = Value::Array(messages.to_vec());
    body["stream"] = Value::Bool(config.request.stream);
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools.to_vec());
    }
    let request_bytes = serde_json::to_vec(&body).map_err(|error| {
        ProviderDriverError::protocol(
            "request_json_encode_failed",
            format!("provider request could not be encoded: {error}"),
        )
    })?;
    let endpoint = endpoint_evidence(&config.endpoint);
    let mut request = client
        .post(config.endpoint.clone())
        .headers(config.headers.clone())
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(request_bytes.clone());
    if let Some(auth) = &config.auth {
        let value = auth_header_value(auth, dispatch.credential, &endpoint).await?;
        request = request.header(auth.header.clone(), value);
    }
    dispatch
        .admit("POST", "http", &config.endpoint, &request_bytes)
        .await
        .map_err(|error| {
            ProviderDriverError::failed_closed("transport_dispatch_not_admitted", error)
        })?;
    start_physical_credential_attempt(config, dispatch.credential)?;
    let response = request.send().await.map_err(|error| {
        ProviderDriverError::Transport(Box::new(
            ProviderErrorEvidence::new(
                "transport",
                "request_send_failed",
                format!("provider request could not be sent: {error}"),
            )
            .with_endpoint(endpoint.clone()),
        ))
    })?;
    let status = response.status();
    let headers = response.headers().clone();
    let http = http_evidence(status.as_u16(), &headers);
    let (bytes, transport_chunks) = read_bounded(response, config.max_response_bytes)
        .await
        .map_err(|failure| {
            let evidence = ProviderErrorEvidence::new(
                if config.request.stream && failure.transport_chunks > 0 {
                    "failed_closed"
                } else {
                    "transport"
                },
                if config.request.stream && failure.transport_chunks > 0 {
                    "partial_stream_replay_forbidden"
                } else {
                    "response_read_failed"
                },
                failure.message,
            )
            .with_endpoint(endpoint.clone())
            .with_response(http.clone(), body_evidence(&failure.bytes));
            if config.request.stream && failure.transport_chunks > 0 {
                ProviderDriverError::FailedClosed(Box::new(evidence))
            } else {
                ProviderDriverError::Transport(Box::new(evidence))
            }
        })?;
    let body_evidence = body_evidence(&bytes);
    if status.as_u16() == 401 && invalidate_azure_auth_on_unauthorized(config.auth.as_ref()).await {
        return Err(ProviderDriverError::AuthenticatedRefreshRequired(Box::new(
            ProviderErrorEvidence::new(
                "authentication",
                "provider_authentication_refresh_required",
                "provider rejected the descriptor-selected Azure Entra token with HTTP 401",
            )
            .with_endpoint(endpoint)
            .with_response(http, body_evidence),
        )));
    }
    if !status.is_success() {
        let evidence = ProviderErrorEvidence::new(
            if config.request.retryable_statuses.contains(&status.as_u16()) {
                "transport"
            } else {
                "protocol"
            },
            "provider_http_error",
            format!("provider returned HTTP {}", status.as_u16()),
        )
        .with_endpoint(endpoint)
        .with_response(http, body_evidence);
        return Err(
            if config.request.retryable_statuses.contains(&status.as_u16()) {
                ProviderDriverError::Transport(Box::new(evidence))
            } else {
                ProviderDriverError::Protocol(Box::new(evidence))
            },
        );
    }
    let (response, stream) = if config.request.stream {
        parse_sse_response(
            &bytes,
            config.request.require_done,
            &config.response,
            transport_chunks,
        )?
    } else {
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            ProviderDriverError::Protocol(Box::new(
                ProviderErrorEvidence::new(
                    "protocol",
                    "response_json_invalid",
                    format!("provider response is not valid JSON: {error}"),
                )
                .with_endpoint(endpoint.clone())
                .with_response(http.clone(), body_evidence.clone()),
            ))
        })?;
        if value.get("error").is_some() {
            return Err(ProviderDriverError::Protocol(Box::new(
                ProviderErrorEvidence::new(
                    "protocol",
                    "provider_error_payload",
                    format!(
                        "provider returned an error payload with HTTP {}",
                        status.as_u16()
                    ),
                )
                .with_endpoint(endpoint)
                .with_response(http, body_evidence),
            )));
        }
        (
            value,
            StreamEvidence {
                requested: false,
                data_events: 0,
                transport_chunks,
                done: false,
            },
        )
    };
    Ok(WireOutcome {
        response,
        evidence: ProviderResponseEvidence {
            endpoint,
            http,
            body: body_evidence,
            stream,
        },
    })
}

pub(crate) async fn execute_model_catalog(
    client: &reqwest::Client,
    config: &ProviderConfig,
    dispatch: &DispatchAdmission<'_>,
) -> Result<WireOutcome, ProviderDriverError> {
    let endpoint_url = config.models_endpoint.as_ref().ok_or_else(|| {
        ProviderDriverError::failed_closed(
            "model_catalog_unsupported",
            "this provider descriptor declares no model catalog operation",
        )
    })?;
    let endpoint = endpoint_evidence(endpoint_url);
    let mut request = client
        .get(endpoint_url.clone())
        .headers(config.headers.clone());
    if let Some(auth) = &config.auth {
        let value = auth_header_value(auth, dispatch.credential, &endpoint).await?;
        request = request.header(auth.header.clone(), value);
    }
    dispatch
        .admit("GET", "http", endpoint_url, &[])
        .await
        .map_err(|error| {
            ProviderDriverError::failed_closed("transport_dispatch_not_admitted", error)
        })?;
    start_physical_credential_attempt(config, dispatch.credential)?;
    let response = request.send().await.map_err(|error| {
        ProviderDriverError::Transport(Box::new(
            ProviderErrorEvidence::new(
                "transport",
                "model_catalog_send_failed",
                format!("provider model catalog request could not be sent: {error}"),
            )
            .with_endpoint(endpoint.clone()),
        ))
    })?;
    let status = response.status();
    let headers = response.headers().clone();
    let http = http_evidence(status.as_u16(), &headers);
    let (bytes, transport_chunks) = read_bounded(response, config.max_response_bytes)
        .await
        .map_err(|failure| {
            ProviderDriverError::Transport(Box::new(
                ProviderErrorEvidence::new(
                    "transport",
                    "model_catalog_read_failed",
                    failure.message,
                )
                .with_endpoint(endpoint.clone())
                .with_response(http.clone(), body_evidence(&failure.bytes)),
            ))
        })?;
    let body = body_evidence(&bytes);
    if status.as_u16() == 401 && invalidate_azure_auth_on_unauthorized(config.auth.as_ref()).await {
        return Err(ProviderDriverError::AuthenticatedRefreshRequired(Box::new(
            ProviderErrorEvidence::new(
                "authentication",
                "provider_authentication_refresh_required",
                "provider model catalog rejected the descriptor-selected Azure Entra token with HTTP 401",
            )
            .with_endpoint(endpoint)
            .with_response(http, body),
        )));
    }
    if !status.is_success() {
        let evidence = ProviderErrorEvidence::new(
            if config.request.retryable_statuses.contains(&status.as_u16()) {
                "transport"
            } else {
                "protocol"
            },
            "model_catalog_http_error",
            format!("provider returned HTTP {}", status.as_u16()),
        )
        .with_endpoint(endpoint)
        .with_response(http, body);
        return Err(
            if config.request.retryable_statuses.contains(&status.as_u16()) {
                ProviderDriverError::Transport(Box::new(evidence))
            } else {
                ProviderDriverError::Protocol(Box::new(evidence))
            },
        );
    }
    let response: Value = serde_json::from_slice(&bytes).map_err(|error| {
        ProviderDriverError::Protocol(Box::new(
            ProviderErrorEvidence::new(
                "protocol",
                "model_catalog_json_invalid",
                format!("provider model catalog is not valid JSON: {error}"),
            )
            .with_endpoint(endpoint.clone())
            .with_response(http.clone(), body.clone()),
        ))
    })?;
    if response.get("error").is_some() {
        return Err(ProviderDriverError::Protocol(Box::new(
            ProviderErrorEvidence::new(
                "protocol",
                "model_catalog_error_payload",
                format!(
                    "provider returned an error payload with HTTP {}",
                    status.as_u16()
                ),
            )
            .with_endpoint(endpoint)
            .with_response(http, body),
        )));
    }
    Ok(WireOutcome {
        response,
        evidence: ProviderResponseEvidence {
            endpoint,
            http,
            body,
            stream: StreamEvidence {
                requested: false,
                data_events: 0,
                transport_chunks,
                done: false,
            },
        },
    })
}

async fn read_bounded(
    mut response: reqwest::Response,
    maximum: usize,
) -> Result<(Vec<u8>, usize), BoundedReadFailure> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(BoundedReadFailure {
            message: format!("provider response exceeds {maximum} bytes"),
            bytes: Vec::new(),
            transport_chunks: 0,
        });
    }
    let mut bytes = Vec::with_capacity(
        response
            .content_length()
            .unwrap_or_default()
            .min(maximum as u64) as usize,
    );
    let mut chunks = 0_usize;
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) => {
                return Err(BoundedReadFailure {
                    message: format!("provider response body read failed: {error}"),
                    bytes,
                    transport_chunks: chunks,
                });
            }
        };
        chunks += 1;
        let Some(next) = bytes.len().checked_add(chunk.len()) else {
            return Err(BoundedReadFailure {
                message: "provider response byte count overflowed".to_string(),
                bytes,
                transport_chunks: chunks,
            });
        };
        if next > maximum {
            return Err(BoundedReadFailure {
                message: format!("provider response exceeds {maximum} bytes"),
                bytes,
                transport_chunks: chunks,
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((bytes, chunks))
}

struct BoundedReadFailure {
    message: String,
    bytes: Vec<u8>,
    transport_chunks: usize,
}

fn parse_sse_response(
    bytes: &[u8],
    require_done: bool,
    response_config: &ResponseConfig,
    transport_chunks: usize,
) -> Result<(Value, StreamEvidence), ProviderDriverError> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        ProviderDriverError::protocol(
            "stream_utf8_invalid",
            format!("provider SSE stream is not UTF-8: {error}"),
        )
    })?;
    let events = sse_data_events(text)?;
    let mut accumulator = StreamAccumulator::default();
    let mut data_events = 0_usize;
    let mut done = false;
    for data in events {
        if data.trim() == "[DONE]" {
            if done {
                return Err(ProviderDriverError::protocol(
                    "stream_done_duplicate",
                    "provider SSE stream emitted [DONE] more than once",
                ));
            }
            done = true;
            continue;
        }
        if done {
            return Err(ProviderDriverError::protocol(
                "stream_data_after_done",
                "provider SSE stream emitted data after [DONE]",
            ));
        }
        let chunk: Value = serde_json::from_str(&data).map_err(|error| {
            ProviderDriverError::protocol(
                "stream_event_json_invalid",
                format!("provider SSE data event is not valid JSON: {error}"),
            )
        })?;
        if chunk.get("error").is_some() {
            return Err(ProviderDriverError::protocol(
                "provider_stream_error",
                "provider SSE event reported an error",
            ));
        }
        data_events += 1;
        accumulator.push(&chunk, response_config)?;
    }
    if require_done && !done {
        return Err(ProviderDriverError::protocol(
            "stream_done_missing",
            "provider SSE stream closed without [DONE]",
        ));
    }
    Ok((
        accumulator.finish()?,
        StreamEvidence {
            requested: true,
            data_events,
            transport_chunks,
            done,
        },
    ))
}

fn sse_data_events(text: &str) -> Result<Vec<String>, ProviderDriverError> {
    let mut events = Vec::new();
    let mut data_lines = Vec::new();
    for raw_line in text.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() {
            if !data_lines.is_empty() {
                events.push(data_lines.join("\n"));
                data_lines.clear();
            }
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.strip_prefix(' ').unwrap_or(data).to_string());
        }
    }
    if !data_lines.is_empty() {
        events.push(data_lines.join("\n"));
    }
    if events.is_empty() {
        return Err(ProviderDriverError::protocol(
            "stream_data_missing",
            "provider SSE response contains no data events",
        ));
    }
    Ok(events)
}

#[derive(Default)]
struct StreamAccumulator {
    id: Option<String>,
    model: Option<String>,
    created: Option<Value>,
    content: String,
    content_parts: Vec<Value>,
    reasoning: Map<String, Value>,
    reasoning_details: Map<String, Value>,
    refusal: String,
    tool_calls: BTreeMap<u64, StreamToolCall>,
    finish_reason: Option<Value>,
    usage: Option<Value>,
    top_level: BTreeMap<String, TopLevelStreamValue>,
}

enum TopLevelStreamValue {
    Identical(Value),
    UniqueArrayItems {
        items: Vec<Value>,
        saw_array: bool,
        saw_null: bool,
    },
}

#[derive(Default)]
struct StreamToolCall {
    id: String,
    call_type: String,
    name: String,
    arguments: String,
    extra: Map<String, Value>,
}

impl StreamAccumulator {
    fn push(
        &mut self,
        chunk: &Value,
        response_config: &ResponseConfig,
    ) -> Result<(), ProviderDriverError> {
        merge_optional_string(&mut self.id, chunk.get("id"), "stream response id")?;
        merge_optional_string(&mut self.model, chunk.get("model"), "stream response model")?;
        if self.created.is_none() {
            self.created = chunk.get("created").cloned();
        }
        if let Some(usage) = chunk.get("usage").filter(|value| !value.is_null()) {
            if !usage.is_object() {
                return Err(ProviderDriverError::protocol(
                    "stream_usage_invalid",
                    "provider stream usage must be an object",
                ));
            }
            self.usage = Some(usage.clone());
        }
        for declared in &response_config.top_level_artifacts {
            if let Some(value) = chunk.get(&declared.field) {
                self.push_top_level(declared, value)?;
            }
        }
        let choices = match chunk.get("choices") {
            None => return Ok(()),
            Some(Value::Array(choices)) => choices,
            Some(_) => {
                return Err(ProviderDriverError::protocol(
                    "stream_choices_invalid",
                    "provider stream choices must be an array",
                ));
            }
        };
        if choices.is_empty() {
            return Ok(());
        }
        if choices.len() != 1 || choices[0].get("index").and_then(Value::as_u64) != Some(0) {
            return Err(ProviderDriverError::protocol(
                "stream_choice_count_invalid",
                "provider family supports exactly one streamed choice with index 0",
            ));
        }
        let choice = &choices[0];
        if self.finish_reason.is_some() {
            return Err(ProviderDriverError::protocol(
                "stream_choice_after_finish",
                "provider stream emitted a choice after its terminal finish reason",
            ));
        }
        if let Some(reason) = choice.get("finish_reason").filter(|value| !value.is_null()) {
            if !reason
                .as_str()
                .is_some_and(|reason| !reason.trim().is_empty() && reason.trim() == reason)
            {
                return Err(ProviderDriverError::protocol(
                    "stream_finish_reason_invalid",
                    "provider stream finish_reason must be a non-empty trimmed string",
                ));
            }
            self.finish_reason = Some(reason.clone());
        }
        let Some(delta) = choice.get("delta") else {
            return Ok(());
        };
        let delta = delta.as_object().ok_or_else(|| {
            ProviderDriverError::protocol("stream_delta_invalid", "stream delta must be an object")
        })?;
        append_content(
            &mut self.content,
            &mut self.content_parts,
            delta.get("content"),
        )?;
        append_optional_string(&mut self.refusal, delta.get("refusal"), "stream refusal")?;
        for field in &response_config.reasoning_fields {
            append_dynamic_field(&mut self.reasoning, field, delta.get(field))?;
        }
        for field in &response_config.reasoning_detail_fields {
            append_dynamic_field(&mut self.reasoning_details, field, delta.get(field))?;
        }
        if let Some(tool_calls) = delta.get("tool_calls") {
            let calls = tool_calls.as_array().ok_or_else(|| {
                ProviderDriverError::protocol(
                    "stream_tool_calls_invalid",
                    "stream delta tool_calls must be an array",
                )
            })?;
            for call in calls {
                self.push_tool_call(call)?;
            }
        }
        Ok(())
    }

    fn push_top_level(
        &mut self,
        declared: &crate::config::TopLevelArtifactConfig,
        value: &Value,
    ) -> Result<(), ProviderDriverError> {
        if !declared.accepts(value) {
            return Err(ProviderDriverError::protocol(
                "stream_top_level_field_shape_invalid",
                format!(
                    "provider stream top-level field '{}' does not match its descriptor-declared JSON value types",
                    declared.field
                ),
            ));
        }
        match declared.stream_accumulation {
            StreamAccumulation::Identical => match self.top_level.get(&declared.field) {
                None => {
                    self.top_level.insert(
                        declared.field.clone(),
                        TopLevelStreamValue::Identical(value.clone()),
                    );
                }
                Some(TopLevelStreamValue::Identical(previous)) if previous == value => {}
                Some(TopLevelStreamValue::Identical(_)) => {
                    return Err(ProviderDriverError::protocol(
                        "stream_top_level_field_conflict",
                        format!(
                            "provider stream repeated top-level field '{}' with conflicting values",
                            declared.field
                        ),
                    ));
                }
                Some(TopLevelStreamValue::UniqueArrayItems { .. }) => {
                    return Err(ProviderDriverError::protocol(
                        "stream_top_level_accumulation_mismatch",
                        format!(
                            "provider stream top-level field '{}' changed accumulation contract",
                            declared.field
                        ),
                    ));
                }
            },
            StreamAccumulation::UniqueArrayItems => {
                let accumulated =
                    self.top_level
                        .entry(declared.field.clone())
                        .or_insert_with(|| TopLevelStreamValue::UniqueArrayItems {
                            items: Vec::new(),
                            saw_array: false,
                            saw_null: false,
                        });
                let TopLevelStreamValue::UniqueArrayItems {
                    items,
                    saw_array,
                    saw_null,
                } = accumulated
                else {
                    return Err(ProviderDriverError::protocol(
                        "stream_top_level_accumulation_mismatch",
                        format!(
                            "provider stream top-level field '{}' changed accumulation contract",
                            declared.field
                        ),
                    ));
                };
                match value {
                    Value::Null => *saw_null = true,
                    Value::Array(values) => {
                        *saw_array = true;
                        for value in values {
                            if !items.contains(value) {
                                items.push(value.clone());
                            }
                        }
                    }
                    _ => {
                        return Err(ProviderDriverError::protocol(
                            "stream_top_level_field_shape_invalid",
                            format!(
                                "provider stream top-level field '{}' cannot use unique_array_items with this JSON type",
                                declared.field
                            ),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn push_tool_call(&mut self, value: &Value) -> Result<(), ProviderDriverError> {
        let object = value.as_object().ok_or_else(|| {
            ProviderDriverError::protocol(
                "stream_tool_call_invalid",
                "stream tool call delta must be an object",
            )
        })?;
        let index = object.get("index").and_then(Value::as_u64).ok_or_else(|| {
            ProviderDriverError::protocol(
                "stream_tool_call_index_missing",
                "stream tool call delta has no unsigned index",
            )
        })?;
        let target = self.tool_calls.entry(index).or_default();
        append_optional_string(&mut target.id, object.get("id"), "stream tool call id")?;
        append_optional_string(
            &mut target.call_type,
            object.get("type"),
            "stream tool call type",
        )?;
        if let Some(function) = object.get("function") {
            let function = function.as_object().ok_or_else(|| {
                ProviderDriverError::protocol(
                    "stream_tool_function_invalid",
                    "stream tool call function must be an object",
                )
            })?;
            append_optional_string(
                &mut target.name,
                function.get("name"),
                "stream tool function name",
            )?;
            append_optional_string(
                &mut target.arguments,
                function.get("arguments"),
                "stream tool function arguments",
            )?;
        }
        for (key, value) in object {
            if !matches!(key.as_str(), "index" | "id" | "type" | "function") {
                target.extra.insert(key.clone(), value.clone());
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<Value, ProviderDriverError> {
        let content = if !self.content_parts.is_empty() {
            let mut parts = self.content_parts;
            if !self.content.is_empty() {
                parts.push(json!({ "type": "text", "text": self.content }));
            }
            Value::Array(parts)
        } else if self.content.is_empty() {
            Value::Null
        } else {
            Value::String(self.content)
        };
        let mut message = Map::new();
        message.insert("role".to_string(), Value::String("assistant".to_string()));
        message.insert("content".to_string(), content);
        if !self.refusal.is_empty() {
            message.insert("refusal".to_string(), Value::String(self.refusal));
        }
        for (key, value) in self.reasoning {
            message.insert(key, value);
        }
        for (key, value) in self.reasoning_details {
            message.insert(key, value);
        }
        if !self.tool_calls.is_empty() {
            let mut calls = Vec::with_capacity(self.tool_calls.len());
            for (expected, (index, call)) in self.tool_calls.into_iter().enumerate() {
                if index != expected as u64 {
                    return Err(ProviderDriverError::protocol(
                        "stream_tool_call_index_gap",
                        "stream tool call indexes must be contiguous from zero",
                    ));
                }
                if call.id.is_empty() || call.call_type.is_empty() || call.name.is_empty() {
                    return Err(ProviderDriverError::protocol(
                        "stream_tool_call_incomplete",
                        "stream ended before a tool call id, type, and function name completed",
                    ));
                }
                let mut value = Map::new();
                value.insert("id".to_string(), Value::String(call.id));
                value.insert("type".to_string(), Value::String(call.call_type));
                value.insert(
                    "function".to_string(),
                    json!({ "name": call.name, "arguments": call.arguments }),
                );
                value.extend(call.extra);
                calls.push(Value::Object(value));
            }
            message.insert("tool_calls".to_string(), Value::Array(calls));
        }
        let mut response = Map::new();
        if let Some(id) = self.id {
            response.insert("id".to_string(), Value::String(id));
        }
        if let Some(model) = self.model {
            response.insert("model".to_string(), Value::String(model));
        }
        if let Some(created) = self.created {
            response.insert("created".to_string(), created);
        }
        response.insert(
            "choices".to_string(),
            Value::Array(vec![json!({
                "index": 0,
                "message": Value::Object(message),
                "finish_reason": self.finish_reason.unwrap_or(Value::Null)
            })]),
        );
        if let Some(usage) = self.usage {
            response.insert("usage".to_string(), usage);
        }
        for (field, value) in self.top_level {
            let value = match value {
                TopLevelStreamValue::Identical(value) => value,
                TopLevelStreamValue::UniqueArrayItems {
                    items,
                    saw_array,
                    saw_null,
                } => {
                    if saw_array {
                        Value::Array(items)
                    } else if saw_null {
                        Value::Null
                    } else {
                        return Err(ProviderDriverError::protocol(
                            "stream_top_level_field_empty",
                            format!(
                                "provider stream top-level field '{field}' produced no accumulated value"
                            ),
                        ));
                    }
                }
            };
            response.insert(field, value);
        }
        Ok(Value::Object(response))
    }
}

fn append_content(
    text: &mut String,
    parts: &mut Vec<Value>,
    value: Option<&Value>,
) -> Result<(), ProviderDriverError> {
    match value {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(value)) => {
            text.push_str(value);
            Ok(())
        }
        Some(Value::Array(values)) => {
            parts.extend(values.iter().cloned());
            Ok(())
        }
        Some(_) => Err(ProviderDriverError::protocol(
            "stream_content_invalid",
            "stream content delta must be string, array, or null",
        )),
    }
}

fn append_dynamic_field(
    target: &mut Map<String, Value>,
    name: &str,
    value: Option<&Value>,
) -> Result<(), ProviderDriverError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(());
    };
    match value {
        Value::String(fragment) => match target.get_mut(name) {
            None => {
                target.insert(name.to_string(), Value::String(fragment.clone()));
            }
            Some(Value::String(existing)) => existing.push_str(fragment),
            Some(_) => {
                return Err(ProviderDriverError::protocol(
                    "stream_reasoning_shape_changed",
                    format!("stream reasoning field '{name}' changed shape"),
                ));
            }
        },
        Value::Array(fragment) => match target.get_mut(name) {
            None => {
                target.insert(name.to_string(), Value::Array(fragment.clone()));
            }
            Some(Value::Array(existing)) => existing.extend(fragment.iter().cloned()),
            Some(_) => {
                return Err(ProviderDriverError::protocol(
                    "stream_reasoning_shape_changed",
                    format!("stream reasoning field '{name}' changed shape"),
                ));
            }
        },
        other => {
            if target.insert(name.to_string(), other.clone()).is_some() {
                return Err(ProviderDriverError::protocol(
                    "stream_reasoning_value_repeated",
                    format!("stream reasoning field '{name}' repeated a non-appendable value"),
                ));
            }
        }
    }
    Ok(())
}

fn append_optional_string(
    target: &mut String,
    value: Option<&Value>,
    surface: &str,
) -> Result<(), ProviderDriverError> {
    match value {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(value)) => {
            target.push_str(value);
            Ok(())
        }
        Some(_) => Err(ProviderDriverError::protocol(
            "stream_string_invalid",
            format!("{surface} must be a string or null"),
        )),
    }
}

fn merge_optional_string(
    target: &mut Option<String>,
    value: Option<&Value>,
    surface: &str,
) -> Result<(), ProviderDriverError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(());
    };
    let value = value.as_str().ok_or_else(|| {
        ProviderDriverError::protocol(
            "stream_identity_invalid",
            format!("{surface} must be a string"),
        )
    })?;
    if let Some(existing) = target {
        if existing != value {
            return Err(ProviderDriverError::protocol(
                "stream_identity_changed",
                format!("{surface} changed during one response"),
            ));
        }
    } else {
        *target = Some(value.to_string());
    }
    Ok(())
}

fn start_physical_credential_attempt(
    config: &ProviderConfig,
    selected: Option<&CredentialSelection>,
) -> Result<(), ProviderDriverError> {
    let now_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| {
            ProviderDriverError::failed_closed(
                "credential_pool_clock_invalid",
                format!("provider credential clock is before Unix epoch: {error}"),
            )
        })?
        .as_millis();
    let now_unix_ms = u64::try_from(now_unix_ms).map_err(|_| {
        ProviderDriverError::failed_closed(
            "credential_pool_clock_invalid",
            "provider credential clock exceeds u64",
        )
    })?;
    let started = config
        .start_credential_attempt(selected, now_unix_ms)
        .map_err(|error| {
            ProviderDriverError::failed_closed("credential_pool_state_invalid", error)
        })?;
    match (started, selected) {
        (CredentialAttemptStart::Unmanaged, None) => Ok(()),
        (CredentialAttemptStart::Selected(started), Some(selected)) if &started == selected => {
            Ok(())
        }
        (
            CredentialAttemptStart::Exhausted {
                next_available_at_unix_ms,
            },
            _,
        ) => {
            let availability = next_available_at_unix_ms.map_or_else(
                || "no future availability is recorded".to_string(),
                |timestamp| format!("next availability is at Unix millisecond {timestamp}"),
            );
            Err(ProviderDriverError::failed_closed(
                "credential_pool_exhausted",
                format!(
                    "exact-interface credential pool has no available credential; {availability}"
                ),
            ))
        }
        _ => Err(ProviderDriverError::failed_closed(
            "credential_pool_state_invalid",
            "provider physical credential attempt diverged from its selected candidate",
        )),
    }
}

async fn auth_header_value(
    auth: &AuthConfig,
    credential: Option<&CredentialSelection>,
    endpoint: &EndpointEvidence,
) -> Result<HeaderValue, ProviderDriverError> {
    let secret = match &auth.source {
        AuthSource::Environment(name) => {
            if credential.is_some() {
                return Err(ProviderDriverError::failed_closed(
                    "credential_pool_selection_invalid",
                    "provider environment auth received a credential-pool selection",
                ));
            }
            std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    ProviderDriverError::FailedClosed(Box::new(
                        ProviderErrorEvidence::new(
                            "failed_closed",
                            "auth_unavailable",
                            format!("provider auth env var '{name}' is unset or empty"),
                        )
                        .with_endpoint(endpoint.clone()),
                    ))
                })?
        }
        AuthSource::Secret {
            octopus_home,
            scope,
            name,
        } => {
            if credential.is_some() {
                return Err(ProviderDriverError::failed_closed(
                    "credential_pool_selection_invalid",
                    "provider static secret auth received a credential-pool selection",
                ));
            }
            load_scoped_secret(octopus_home, scope, name, endpoint)?
        }
        AuthSource::AzureEntra(manager) => {
            if credential.is_some() {
                return Err(ProviderDriverError::failed_closed(
                    "credential_pool_selection_invalid",
                    "provider Azure Entra auth received a credential-pool selection",
                ));
            }
            manager.access_token().await.map_err(|error| {
                ProviderDriverError::FailedClosed(Box::new(
                    ProviderErrorEvidence::new(
                        "authentication",
                        "authentication_unavailable",
                        error.detail.clone(),
                    )
                    .with_endpoint(endpoint.clone())
                    .with_authentication(error.evidence()),
                ))
            })?
        }
        AuthSource::CredentialPool { octopus_home, pool } => {
            let selected = credential.ok_or_else(|| {
                ProviderDriverError::failed_closed(
                    "credential_pool_selection_missing",
                    "provider credential pool has no selected credential for this attempt",
                )
            })?;
            let (scope, name) = parse_secret_ref(selected.secret_ref()).map_err(|error| {
                ProviderDriverError::failed_closed("credential_pool_selection_invalid", error)
            })?;
            if scope.name() != Some(pool.interface()) {
                return Err(ProviderDriverError::failed_closed(
                    "credential_pool_selection_invalid",
                    "provider credential selection is outside its exact interface",
                ));
            }
            load_scoped_secret(octopus_home, &scope, &name, endpoint)?
        }
    };
    HeaderValue::from_str(&format!("{}{secret}", auth.value_prefix)).map_err(|_| {
        ProviderDriverError::FailedClosed(Box::new(
            ProviderErrorEvidence::new(
                "failed_closed",
                "auth_header_invalid",
                "provider secret cannot form a valid auth header value",
            )
            .with_endpoint(endpoint.clone()),
        ))
    })
}

fn load_scoped_secret(
    octopus_home: &std::path::Path,
    scope: &octopus_secrets::SecretScope,
    name: &octopus_secrets::SecretName,
    endpoint: &EndpointEvidence,
) -> Result<String, ProviderDriverError> {
    SecretsManager::new_with_keyring_store(
        octopus_home.to_path_buf(),
        SecretsBackendKind::Local,
        Arc::new(LocalFileKeyringStore::new(
            octopus_home.join("secrets").join("keyring"),
        )),
    )
    .get(scope, name)
    .map_err(|error| {
        ProviderDriverError::FailedClosed(Box::new(
            ProviderErrorEvidence::new(
                "failed_closed",
                "auth_unavailable",
                format!(
                    "provider secret '{scope_name}/{name}' could not be loaded: {error}",
                    scope_name = scope.name().unwrap_or("global")
                ),
            )
            .with_endpoint(endpoint.clone()),
        ))
    })?
    .filter(|value| !value.trim().is_empty())
    .ok_or_else(|| {
        ProviderDriverError::FailedClosed(Box::new(
            ProviderErrorEvidence::new(
                "failed_closed",
                "auth_unavailable",
                format!(
                    "provider secret '{scope_name}/{name}' is not configured",
                    scope_name = scope.name().unwrap_or("global")
                ),
            )
            .with_endpoint(endpoint.clone()),
        ))
    })
}

async fn invalidate_azure_auth_on_unauthorized(auth: Option<&AuthConfig>) -> bool {
    if let Some(AuthConfig {
        source: AuthSource::AzureEntra(manager),
        ..
    }) = auth
    {
        manager.invalidate().await;
        return true;
    }
    false
}

pub(crate) async fn refresh_azure_authentication(
    config: &ProviderConfig,
) -> Result<(), ProviderDriverError> {
    let Some(AuthConfig {
        source: AuthSource::AzureEntra(manager),
        ..
    }) = config.auth.as_ref()
    else {
        return Err(ProviderDriverError::failed_closed(
            "authentication_refresh_unavailable",
            "provider requested Azure Entra refresh without descriptor-selected Azure Entra auth",
        ));
    };
    manager.access_token().await.map(|_| ()).map_err(|error| {
        ProviderDriverError::FailedClosed(Box::new(
            ProviderErrorEvidence::new(
                "authentication",
                "authentication_refresh_failed",
                error.detail.clone(),
            )
            .with_endpoint(endpoint_evidence(&config.endpoint))
            .with_authentication(error.evidence()),
        ))
    })
}

fn endpoint_evidence(url: &reqwest::Url) -> EndpointEvidence {
    EndpointEvidence {
        scheme: url.scheme().to_string(),
        host: url.host_str().unwrap_or_default().to_string(),
        port: url.port(),
        path: url.path().to_string(),
    }
}

fn http_evidence(status: u16, headers: &HeaderMap) -> HttpEvidence {
    let content_type = headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let mut identifiers = BTreeMap::new();
    for name in [
        "x-request-id",
        "request-id",
        "openai-request-id",
        "cf-ray",
        "retry-after",
    ] {
        if let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) {
            identifiers.insert(name.to_string(), value.to_string());
        }
    }
    HttpEvidence {
        status,
        content_type,
        identifiers,
    }
}

fn body_evidence(bytes: &[u8]) -> BodyEvidence {
    BodyEvidence {
        bytes: bytes.len(),
        blake3: blake3::hash(bytes).to_hex().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::config::{
        JsonValueType, ResponseConfig, StreamAccumulation, TopLevelArtifactConfig,
    };

    #[test]
    fn streamed_reasoning_parallel_tools_and_usage_are_accumulated() {
        let bytes = br#"data: {"id":"response-1","model":"model-1","choices":[{"index":0,"delta":{"reasoning":"why ","content":"answer ","tool_calls":[{"index":0,"id":"call-a","type":"function","function":{"name":"artifact__read","arguments":"{\"id\":"}},{"index":1,"id":"call-b","type":"function","function":{"name":"artifact__list","arguments":"{"}}]}}]}

data: {"id":"response-1","model":"model-1","choices":[{"index":0,"delta":{"reasoning":"now","content":"done","tool_calls":[{"index":0,"function":{"arguments":"\"a\"}"}},{"index":1,"function":{"arguments":"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":7,"total_tokens":17}}

data: [DONE]

"#;
        let response_config = ResponseConfig {
            reasoning_fields: vec!["reasoning".to_string()],
            reasoning_detail_fields: vec!["reasoning_details".to_string()],
            successful_finish_reasons: BTreeSet::from([
                "stop".to_string(),
                "tool_calls".to_string(),
            ]),
            nonreplayable_finish_reasons: BTreeSet::from(["length".to_string()]),
            top_level_artifacts: Vec::new(),
        };
        let (response, evidence) = parse_sse_response(bytes, true, &response_config, 3).unwrap();
        let message = &response["choices"][0]["message"];
        assert_eq!(message["content"], "answer done");
        assert_eq!(message["reasoning"], "why now");
        assert_eq!(
            message["tool_calls"][0]["function"]["arguments"],
            "{\"id\":\"a\"}"
        );
        assert_eq!(message["tool_calls"][1]["function"]["arguments"], "{}");
        assert_eq!(response["usage"]["total_tokens"], 17);
        assert_eq!(evidence.data_events, 2);
        assert_eq!(evidence.transport_chunks, 3);
        assert!(evidence.done);
    }

    #[test]
    fn streamed_top_level_arrays_accumulate_unique_raw_items() {
        let bytes = br#"data: {"id":"response-2","model":"sonar","citations":["https://one.test"],"search_results":[{"url":"https://one.test"}],"choices":[{"index":0,"delta":{"content":"grounded "}}]}

data: {"id":"response-2","model":"sonar","citations":["https://one.test","https://two.test"],"search_results":[{"url":"https://two.test"}],"choices":[{"index":0,"delta":{"content":"answer"},"finish_reason":"stop"}]}

data: [DONE]

"#;
        let field = |name: &str, label: &str| TopLevelArtifactConfig {
            field: name.to_string(),
            artifact_label: label.to_string(),
            required: true,
            value_types: BTreeSet::from([JsonValueType::Array, JsonValueType::Null]),
            stream_accumulation: StreamAccumulation::UniqueArrayItems,
        };
        let response_config = ResponseConfig {
            reasoning_fields: Vec::new(),
            reasoning_detail_fields: Vec::new(),
            successful_finish_reasons: BTreeSet::from(["stop".to_string()]),
            nonreplayable_finish_reasons: BTreeSet::from(["length".to_string()]),
            top_level_artifacts: vec![
                field("citations", "provider.citations"),
                field("search_results", "provider.search_results"),
            ],
        };
        let (response, _) = parse_sse_response(bytes, true, &response_config, 2).unwrap();
        assert_eq!(
            response["citations"],
            json!(["https://one.test", "https://two.test"])
        );
        assert_eq!(
            response["search_results"],
            json!([
                {"url": "https://one.test"},
                {"url": "https://two.test"}
            ])
        );
    }

    #[test]
    fn provider_error_body_is_hash_evidence_not_durable_payload() {
        let body = body_evidence(br#"{"error":{"message":"Bearer secret-value"}}"#);
        let encoded = serde_json::to_string(&body).unwrap();
        assert!(!encoded.contains("secret-value"));
        assert!(!encoded.contains("Bearer"));
        assert!(encoded.contains("blake3"));
    }

    #[test]
    fn authenticated_refresh_signal_is_not_a_general_retry() {
        let error = ProviderDriverError::AuthenticatedRefreshRequired(Box::new(
            ProviderErrorEvidence::new(
                "authentication",
                "provider_authentication_refresh_required",
                "HTTP 401",
            )
            .with_response(
                HttpEvidence {
                    status: 401,
                    content_type: Some("application/json".to_string()),
                    identifiers: BTreeMap::new(),
                },
                body_evidence(b"Bearer secret-value"),
            ),
        ));
        assert!(error.authenticated_refresh_required());
        assert!(!error.retryable());
        let encoded = serde_json::to_string(error.evidence()).unwrap();
        assert!(encoded.contains("\"status\":401"));
        assert!(!encoded.contains("secret-value"));
        assert!(!encoded.contains("Bearer"));
    }
}
