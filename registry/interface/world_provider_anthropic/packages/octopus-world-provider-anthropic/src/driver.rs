use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use octopus_azure_auth::{AzureAuthFailure, AzureEntraCredentialManager};
use octopus_descriptors::{InterfaceId, RunId};
use octopus_interfaces::{
    InterfaceArtifactFile, InterfaceBoundaryError as DriverError, InterfaceDescriptor,
    InterfaceInvocationEnvelope, InterfaceTransportConfig,
};
use octopus_ledger::ArtifactRef;
use octopus_model_context_render::{
    model_hidden_tool_names, model_visible_affordances,
    render_model_context_block_with_hidden_tools,
};
use octopus_provider_run_authoring::{
    ProviderEmission, ProviderEmissionRoute, ProviderOutputHandoff,
};
use octopus_secrets::{
    LocalFileKeyringStore, SecretName, SecretScope, SecretsBackendKind, SecretsManager,
};
use octopus_tool_handlers::ToolResultArtifact;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Response, Url};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::dispatch::DispatchAdmission;

pub const INTERFACE_DRIVER_REF: &str = "anthropic_messages";

pub const CONTEXT_REFERENCE_LABEL: &str = "context.reference";
const INTERFACE_ARTIFACT_OUTPUT_LABEL: &str = "interface.artifact.output";
const TOOL_CALL_LABEL: &str = "tool.call";
const TURN_ERROR_LABEL: &str = "turn.error";
const TRANSPORT_EVIDENCE_LABEL: &str = "provider.transport.evidence";
const USAGE_EVIDENCE_LABEL: &str = "provider.usage";
const REASONING_EVIDENCE_LABEL: &str = "provider.reasoning";
const MODELS_LIST_LABEL: &str = "provider.models.list";
const MODELS_CATALOG_LABEL: &str = "provider.models.catalog";
const SESSION_CONTEXT_FIELD: &str = "session_context";
pub const TOOL_RESULTS_PENDING_LABEL: &str = "provider.tool_results.pending";

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AnthropicMessagesOptions {
    messages_path: String,
    #[serde(default)]
    models_path: Option<String>,
    max_tokens: u64,
    stream: bool,
    input_content_types: Vec<String>,
    response_content_types: Vec<String>,
    thinking_replay: ThinkingReplay,
    #[serde(default)]
    system_prefix: Option<String>,
    #[serde(default)]
    tool_name_prefix: Option<String>,
    #[serde(default)]
    request_fields: Map<String, Value>,
    #[serde(default)]
    model_profiles: BTreeMap<String, AnthropicModelProfile>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct AnthropicModelProfile {
    #[serde(default)]
    max_tokens: Option<u64>,
    #[serde(default)]
    input_content_types: Option<Vec<String>>,
    #[serde(default)]
    response_content_types: Option<Vec<String>>,
    #[serde(default)]
    thinking_replay: Option<ThinkingReplay>,
    #[serde(default)]
    request_fields: Option<Map<String, Value>>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryPolicy {
    retryable_http_statuses: Vec<u16>,
    backoff_ms: Vec<u64>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct FinishPolicy {
    successful_stop_reasons: Vec<String>,
    nonreplayable_stop_reasons: Vec<String>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ThinkingReplay {
    Verbatim,
    UnsignedOnly,
    Drop,
}

#[derive(Clone)]
enum AuthSource {
    Environment(String),
    Secret {
        octopus_home: PathBuf,
        scope: SecretScope,
        name: SecretName,
    },
    AzureEntra(Box<AzureEntraCredentialManager>),
}

#[derive(Clone)]
struct ProviderAuth {
    header: HeaderName,
    value_prefix: String,
    source: AuthSource,
}

pub struct AnthropicMessagesDriver {
    interface_id: InterfaceId,
    endpoint: Url,
    models_endpoint: Option<Url>,
    headers: HeaderMap,
    auth: ProviderAuth,
    options: AnthropicMessagesOptions,
    private_config: Map<String, Value>,
    retryable_http_statuses: BTreeSet<u16>,
    retry_backoff: Vec<Duration>,
    successful_stop_reasons: BTreeSet<String>,
    nonreplayable_stop_reasons: BTreeSet<String>,
    max_response_bytes: usize,
    client: Client,
}

struct ProviderResponse {
    id: String,
    model: String,
    stop_reason: Option<String>,
    content: Vec<Value>,
    usage: Value,
    transport: TransportEvidence,
}

struct AcceptedProviderResponse {
    response: ProviderResponse,
    complete: bool,
}

struct TransportEvidence {
    streamed: bool,
    status: u16,
    response_bytes: usize,
    response_blake3: String,
    event_count: usize,
    event_types: BTreeMap<String, usize>,
}

struct ResponseBodyEvidence {
    bytes: usize,
    blake3: String,
}

#[derive(Debug)]
struct ContextArtifact {
    id: String,
    label: Option<String>,
    value: Value,
    current: bool,
    source_artifact: ArtifactRef,
    source_movement_seq: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionContext {
    schema_version: u32,
    provider_interface: InterfaceId,
    artifacts: Vec<SessionContextArtifact>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionContextArtifact {
    label: String,
    payload: Value,
    source_artifact: ArtifactRef,
    source_movement_seq: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicToolCallIdentity {
    pub correlation_id: String,
    pub tool: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicToolGroupIdentity {
    pub response_id: String,
    pub source_artifact: ArtifactRef,
    pub source_movement_seq: Option<u64>,
    pub calls: Vec<AnthropicToolCallIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AnthropicToolResultIdentity {
    correlation_id: String,
    tool: String,
    source_artifact: ArtifactRef,
    source_movement_seq: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PendingToolGroupState {
    pub schema_version: u32,
    pub provider_interface: InterfaceId,
    pub run: RunId,
    pub group: AnthropicToolGroupIdentity,
    received: Vec<AnthropicToolResultIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingToolGroupArtifact {
    label: String,
    state: PendingToolGroupState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_context: Option<Value>,
}

#[derive(Clone, Debug)]
struct AnthropicToolResultRecord {
    identity: AnthropicToolResultIdentity,
    index: usize,
}

#[derive(Clone, Debug)]
struct AnthropicToolGroup {
    identity: AnthropicToolGroupIdentity,
    results: BTreeMap<String, AnthropicToolResultRecord>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AuthenticatedRefreshEvidence {
    pub status: u16,
    pub response_bytes: usize,
    pub response_blake3: String,
    pub surface: &'static str,
}

#[derive(Debug)]
pub(crate) enum AnthropicDriverFailure {
    Boundary(DriverError),
    AuthenticatedRefreshRequired(AuthenticatedRefreshEvidence),
    Authentication(AzureAuthFailure),
}

impl AnthropicDriverFailure {
    pub(crate) fn authenticated_refresh_required(&self) -> bool {
        matches!(self, Self::AuthenticatedRefreshRequired(_))
    }

    pub(crate) fn into_boundary(self) -> DriverError {
        match self {
            Self::Boundary(error) => error,
            Self::AuthenticatedRefreshRequired(evidence) => DriverError::Protocol(format!(
                "anthropic_messages {} rejected the descriptor-selected Azure Entra token with HTTP {} (bytes={} blake3={})",
                evidence.surface,
                evidence.status,
                evidence.response_bytes,
                evidence.response_blake3
            )),
            Self::Authentication(error) => DriverError::FailedClosed(error.to_string()),
        }
    }
}

impl From<DriverError> for AnthropicDriverFailure {
    fn from(error: DriverError) -> Self {
        Self::Boundary(error)
    }
}

type AnthropicToolGroups = (
    BTreeMap<String, AnthropicToolGroup>,
    BTreeMap<String, String>,
);

#[derive(Debug)]
enum AnthropicFanInPlan {
    Ready,
    Pending(PendingToolGroupState),
}

struct AssembledTurn {
    system: Vec<Value>,
    messages: Vec<Value>,
}

struct StreamAccumulator {
    id: Option<String>,
    model: Option<String>,
    stop_reason: Option<String>,
    blocks: BTreeMap<usize, StreamBlock>,
    usage: Map<String, Value>,
    event_count: usize,
    event_types: BTreeMap<String, usize>,
    message_started: bool,
    message_delta_seen: bool,
    message_stopped: bool,
}

struct AnthropicSseDecoder {
    pending: Vec<u8>,
    frame_index: usize,
    accumulator: StreamAccumulator,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum StreamBlockKind {
    Text,
    Thinking,
    RedactedThinking,
    ToolUse,
}

struct StreamBlock {
    kind: StreamBlockKind,
    value: Value,
    partial_tool_input: String,
    closed: bool,
}

impl AnthropicMessagesDriver {
    pub fn from_descriptor(
        descriptor: &InterfaceDescriptor,
        activation_secrets_home: PathBuf,
    ) -> Result<Self, String> {
        if descriptor.interface_driver_ref != INTERFACE_DRIVER_REF {
            return Err(format!(
                "provider interface '{}' selects driver '{}', not '{INTERFACE_DRIVER_REF}'",
                descriptor.id, descriptor.interface_driver_ref
            ));
        }
        let config = descriptor.interface_driver_config.as_ref().ok_or_else(|| {
            format!(
                "provider interface '{}' resolves no interface_driver_config",
                descriptor.id
            )
        })?;
        let transport = config.transport.as_ref().ok_or_else(|| {
            format!(
                "provider interface '{}' resolves no interface_driver_config.transport",
                descriptor.id
            )
        })?;
        if transport.wire.as_deref() != Some(INTERFACE_DRIVER_REF) {
            return Err(format!(
                "provider interface '{}' transport.wire must be '{INTERFACE_DRIVER_REF}'",
                descriptor.id
            ));
        }
        validate_process_env_names(&config.private_config, transport)?;
        let options_value = config
            .private_config
            .get("anthropic_messages")
            .cloned()
            .ok_or_else(|| {
                format!(
                    "provider interface '{}' has no private_config.anthropic_messages object",
                    descriptor.id
                )
            })?;
        let options: AnthropicMessagesOptions =
            serde_json::from_value(options_value).map_err(|error| {
                format!(
                    "provider interface '{}' anthropic_messages config is malformed: {error}",
                    descriptor.id
                )
            })?;
        options.validate()?;
        let retry_policy: RetryPolicy = parse_metadata_config(
            &config.private_config,
            "retry_policy",
            descriptor.id.as_str(),
        )?;
        retry_policy.validate()?;
        let finish_policy: FinishPolicy = parse_metadata_config(
            &config.private_config,
            "finish_policy",
            descriptor.id.as_str(),
        )?;
        finish_policy.validate()?;
        validate_declared_types(
            &options.input_content_types,
            &["text", "image", "document"],
            "input_content_types",
        )?;
        validate_declared_types(
            &options.response_content_types,
            &["text", "thinking", "redacted_thinking", "tool_use"],
            "response_content_types",
        )?;
        let retryable_http_statuses = retry_policy
            .retryable_http_statuses
            .iter()
            .copied()
            .collect();
        let retry_backoff = retry_policy
            .backoff_ms
            .iter()
            .copied()
            .map(Duration::from_millis)
            .collect();
        let successful_stop_reasons = finish_policy
            .successful_stop_reasons
            .iter()
            .cloned()
            .collect();
        let nonreplayable_stop_reasons = finish_policy
            .nonreplayable_stop_reasons
            .iter()
            .cloned()
            .collect();
        let endpoint = resolve_endpoint(transport, &options.messages_path)?;
        let models_endpoint = options
            .models_path
            .as_deref()
            .map(|path| resolve_endpoint(transport, path))
            .transpose()?;
        let headers = resolve_static_headers(transport)?;
        let auth = resolve_auth(descriptor, transport, activation_secrets_home)?;
        let timeout_ms = transport.request_timeout_ms.ok_or_else(|| {
            "anthropic_messages transport requires request_timeout_ms".to_string()
        })?;
        if timeout_ms == 0 {
            return Err("anthropic_messages request_timeout_ms must be positive".to_string());
        }
        let max_response_bytes: usize = transport
            .max_response_bytes
            .ok_or_else(|| {
                "anthropic_messages transport requires finite max_response_bytes".to_string()
            })?
            .try_into()
            .map_err(|_| "anthropic_messages max_response_bytes exceeds host width".to_string())?;
        if max_response_bytes == 0 {
            return Err("anthropic_messages max_response_bytes must be positive".to_string());
        }
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(timeout_ms))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| format!("anthropic_messages HTTP client build failed: {error}"))?;
        Ok(Self {
            interface_id: descriptor.id.clone(),
            endpoint,
            models_endpoint,
            headers,
            auth,
            options,
            private_config: config.private_config.clone(),
            retryable_http_statuses,
            retry_backoff,
            successful_stop_reasons,
            nonreplayable_stop_reasons,
            max_response_bytes,
            client,
        })
    }

    pub(crate) fn validate_attempt_budget(&self, max_attempts: u32) -> Result<(), String> {
        let retries = max_attempts.saturating_sub(1) as usize;
        if retries > self.retry_backoff.len() {
            return Err(format!(
                "anthropic_messages formed max_attempts {max_attempts} requires {retries} backoff entries but the descriptor declares {}",
                self.retry_backoff.len()
            ));
        }
        Ok(())
    }

    pub fn enforce_formed_model_policy(
        &self,
        input: &InterfaceInvocationEnvelope,
    ) -> Result<(), DriverError> {
        if input.read_artifact_manifest()?.label == MODELS_LIST_LABEL {
            return Ok(());
        }
        self.formed_model_options(input).map(|_| ())
    }

    pub(crate) fn retry_backoff(&self, failed_attempt: u32) -> Result<Duration, String> {
        let index = failed_attempt
            .checked_sub(1)
            .ok_or_else(|| "anthropic_messages retry attempt must be positive".to_string())?
            as usize;
        self.retry_backoff.get(index).copied().ok_or_else(|| {
            format!(
                "anthropic_messages descriptor has no backoff for failed attempt {failed_attempt}"
            )
        })
    }

    pub fn tool_name_prefix(&self) -> Option<&str> {
        self.options.tool_name_prefix.as_deref()
    }

    pub async fn receive_artifact_and_materialize_outputs(
        &self,
        input: InterfaceInvocationEnvelope,
        source_movement_seq: u64,
        attempt: u32,
        handoff: &ProviderOutputHandoff,
        output_root: &Path,
    ) -> Result<Vec<ProviderEmission>, DriverError> {
        self.receive_artifact_and_materialize_outputs_with_auth_refresh(
            input,
            source_movement_seq,
            attempt,
            handoff,
            output_root,
        )
        .await
        .map_err(AnthropicDriverFailure::into_boundary)
    }

    pub(crate) async fn receive_artifact_and_materialize_outputs_with_auth_refresh(
        &self,
        input: InterfaceInvocationEnvelope,
        source_movement_seq: u64,
        attempt: u32,
        handoff: &ProviderOutputHandoff,
        output_root: &Path,
    ) -> Result<Vec<ProviderEmission>, AnthropicDriverFailure> {
        let current_bytes = input.read_artifact_payload()?;
        let current: Value = serde_json::from_slice(&current_bytes).map_err(|error| {
            DriverError::Protocol(format!(
                "anthropic_messages input artifact is not valid JSON: {error}"
            ))
        })?;
        let manifest = input.read_artifact_manifest()?;
        let credential_handle_fingerprint = self.auth.credential_handle_fingerprint();
        let dispatch = DispatchAdmission {
            handoff,
            output_root,
            provider_interface: &self.interface_id,
            source_movement_seq,
            attempt,
            credential_handle_fingerprint: credential_handle_fingerprint.as_deref(),
        };
        if manifest.label == MODELS_LIST_LABEL {
            return self
                .model_catalog(
                    &current,
                    output_root,
                    &dispatch,
                    &required_formed_models(&input)?,
                )
                .await;
        }
        let (model, options) = self.formed_model_options(&input)?;
        let input_content_types = validate_declared_types(
            &options.input_content_types,
            &["text", "image", "document"],
            "input_content_types",
        )
        .map_err(DriverError::FailedClosed)?;
        let response_content_types = validate_declared_types(
            &options.response_content_types,
            &["text", "thinking", "redacted_thinking", "tool_use"],
            "response_content_types",
        )
        .map_err(DriverError::FailedClosed)?;
        let runtime = input.runtime()?;
        if runtime.interface != self.interface_id {
            return Err(DriverError::Protocol(format!(
                "anthropic_messages invocation belongs to interface '{}' instead of '{}'",
                runtime.interface, self.interface_id
            ))
            .into());
        }
        let artifacts =
            context_artifacts(&input, &current, &self.interface_id, source_movement_seq)?;
        match plan_tool_result_fan_in(
            &artifacts,
            &self.interface_id,
            &runtime.run,
            options.tool_name_prefix.as_deref(),
        )? {
            AnthropicFanInPlan::Ready => {}
            AnthropicFanInPlan::Pending(state) => {
                return Ok(vec![materialize_pending_tool_group(output_root, state)?]);
            }
        }
        let hidden_tools =
            model_hidden_tool_names(&self.private_config).map_err(DriverError::Protocol)?;
        let (tools, model_visible_tool_names) =
            translate_tools(&input, &hidden_tools, options.tool_name_prefix.as_deref())?;
        let mut assembled = assemble_turn(
            &input,
            &artifacts,
            &input_content_types,
            options.thinking_replay,
            &hidden_tools,
            options.tool_name_prefix.as_deref(),
        )?;
        if let Some(prefix) = options.system_prefix.as_deref() {
            assembled
                .system
                .insert(0, json!({"type": "text", "text": prefix}));
        }
        let mut body = options.request_fields.clone();
        body.insert("model".to_string(), Value::String(model));
        body.insert("messages".to_string(), Value::Array(assembled.messages));
        body.insert(
            "max_tokens".to_string(),
            Value::Number(options.max_tokens.into()),
        );
        body.insert("stream".to_string(), Value::Bool(options.stream));
        if !assembled.system.is_empty() {
            body.insert("system".to_string(), Value::Array(assembled.system));
        }
        if !tools.is_empty() {
            body.insert("tools".to_string(), Value::Array(tools));
        } else if body.contains_key("tool_choice") {
            return Err(DriverError::Protocol(
                "anthropic_messages request_fields declares tool_choice but the formed grant exposes no tools"
                    .to_string(),
            )
            .into());
        }
        let response = self.call(Value::Object(body), &dispatch).await?;
        translate_response(
            response,
            output_root,
            &response_content_types,
            &model_visible_tool_names,
            options.tool_name_prefix.as_deref(),
        )
        .map_err(Into::into)
    }

    fn formed_model_options(
        &self,
        input: &InterfaceInvocationEnvelope,
    ) -> Result<(String, AnthropicMessagesOptions), DriverError> {
        let model = resolve_model(&self.private_config)?;
        let models = required_formed_models(input)?;
        if !models.contains(&model) {
            return Err(DriverError::FailedClosed(format!(
                "anthropic_messages selected model '{model}' is outside formed interface policy models"
            )));
        }
        let options = self
            .options
            .for_provider_model(&model)
            .map_err(DriverError::FailedClosed)?;
        Ok((model, options))
    }

    pub(crate) async fn refresh_authentication(&self) -> Result<(), AnthropicDriverFailure> {
        self.auth.refresh_azure_authentication().await
    }

    async fn call(
        &self,
        body: Value,
        dispatch: &DispatchAdmission<'_>,
    ) -> Result<AcceptedProviderResponse, AnthropicDriverFailure> {
        let request_bytes = serde_json::to_vec(&body).map_err(|error| {
            DriverError::Protocol(format!(
                "anthropic_messages request did not encode: {error}"
            ))
        })?;
        let mut request = self
            .client
            .post(self.endpoint.clone())
            .headers(self.headers.clone())
            .header(self.auth.header.clone(), self.auth.header_value().await?);
        request = request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(request_bytes.clone());
        dispatch
            .admit("POST", "http", &self.endpoint, &request_bytes)
            .await
            .map_err(DriverError::FailedClosed)?;
        let response = request.send().await.map_err(|error| {
            DriverError::Transport(format!(
                "anthropic_messages transport error to {}: {error}",
                self.endpoint
            ))
        })?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if status.as_u16() == 401 && self.auth.invalidate_azure_authentication().await {
            let bytes = read_bounded(response, self.max_response_bytes, false).await?;
            let digest = blake3::hash(&bytes).to_hex().to_string();
            return Err(AnthropicDriverFailure::AuthenticatedRefreshRequired(
                AuthenticatedRefreshEvidence {
                    status: status.as_u16(),
                    response_bytes: bytes.len(),
                    response_blake3: digest,
                    surface: "messages endpoint",
                },
            ));
        }
        if !status.is_success() {
            let bytes = read_bounded(response, self.max_response_bytes, false).await?;
            let digest = blake3::hash(&bytes).to_hex().to_string();
            return Err(self
                .http_status_error(status.as_u16(), bytes.len(), &digest, "messages")
                .into());
        }
        if self.options.stream {
            if !content_type.contains("text/event-stream") {
                return Err(DriverError::Protocol(format!(
                    "anthropic_messages descriptor requires streaming but endpoint returned content-type '{content_type}'"
                ))
                .into());
            }
            let (mut parsed, evidence) =
                read_anthropic_stream(response, self.max_response_bytes).await?;
            parsed.transport = TransportEvidence {
                streamed: true,
                status: status.as_u16(),
                response_bytes: evidence.bytes,
                response_blake3: evidence.blake3,
                event_count: parsed.transport.event_count,
                event_types: parsed.transport.event_types,
            };
            self.accept_response(parsed).map_err(Into::into)
        } else {
            if !content_type.contains("json") {
                return Err(DriverError::Protocol(format!(
                    "anthropic_messages descriptor requires JSON but endpoint returned content-type '{content_type}'"
                ))
                .into());
            }
            let bytes = read_bounded(response, self.max_response_bytes, false).await?;
            let digest = blake3::hash(&bytes).to_hex().to_string();
            self.accept_response(parse_json_response(&bytes, status.as_u16(), digest)?)
                .map_err(Into::into)
        }
    }

    async fn model_catalog(
        &self,
        request: &Value,
        output_root: &Path,
        dispatch: &DispatchAdmission<'_>,
        formed_models: &BTreeSet<String>,
    ) -> Result<Vec<ProviderEmission>, AnthropicDriverFailure> {
        let object = request.as_object().ok_or_else(|| {
            DriverError::Protocol("provider.models.list payload must be an object".to_string())
        })?;
        if object.len() != 1
            || object.get("label").and_then(Value::as_str) != Some(MODELS_LIST_LABEL)
        {
            return Err(DriverError::Protocol(
                "provider.models.list payload must contain only its matching label".to_string(),
            )
            .into());
        }
        let endpoint = self.models_endpoint.as_ref().ok_or_else(|| {
            DriverError::FailedClosed(
                "this anthropic_messages descriptor declares no models_path".to_string(),
            )
        })?;
        let request = self
            .client
            .get(endpoint.clone())
            .headers(self.headers.clone())
            .header(self.auth.header.clone(), self.auth.header_value().await?);
        dispatch
            .admit("GET", "http", endpoint, &[])
            .await
            .map_err(DriverError::FailedClosed)?;
        let response = request.send().await.map_err(|error| {
            DriverError::Transport(format!(
                "anthropic_messages model catalog transport error to {endpoint}: {error}"
            ))
        })?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let bytes = read_bounded(response, self.max_response_bytes, false).await?;
        let digest = blake3::hash(&bytes).to_hex().to_string();
        if status.as_u16() == 401 && self.auth.invalidate_azure_authentication().await {
            return Err(AnthropicDriverFailure::AuthenticatedRefreshRequired(
                AuthenticatedRefreshEvidence {
                    status: status.as_u16(),
                    response_bytes: bytes.len(),
                    response_blake3: digest,
                    surface: "model catalog endpoint",
                },
            ));
        }
        if !status.is_success() {
            return Err(self
                .http_status_error(status.as_u16(), bytes.len(), &digest, "model catalog")
                .into());
        }
        if !content_type.contains("json") {
            return Err(DriverError::Protocol(format!(
                "anthropic_messages model catalog returned content-type '{content_type}'"
            ))
            .into());
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            DriverError::Protocol(format!(
                "anthropic_messages model catalog is not JSON: {error}"
            ))
        })?;
        let data = value.get("data").and_then(Value::as_array).ok_or_else(|| {
            DriverError::Protocol("anthropic_messages model catalog has no data array".to_string())
        })?;
        let mut ids = BTreeSet::new();
        for (index, model) in data.iter().enumerate() {
            let id = required_string(model, "id", "model catalog entry")?;
            if !ids.insert(id.clone()) {
                return Err(DriverError::Protocol(format!(
                    "anthropic_messages model catalog repeats id '{id}' at data[{index}]"
                ))
                .into());
            }
        }
        let models = filter_model_catalog(data, formed_models);
        Ok(vec![materialize_emission(
            output_root,
            MODELS_CATALOG_LABEL,
            json!({
                "label": MODELS_CATALOG_LABEL,
                "models": models,
                "first_id": value.get("first_id"),
                "last_id": value.get("last_id"),
                "has_more": value.get("has_more"),
                "evidence": {
                    "status": status.as_u16(),
                    "response_bytes": bytes.len(),
                    "response_blake3": digest,
                }
            }),
            ProviderEmissionRoute::ClientProjection,
        )?])
    }

    fn http_status_error(
        &self,
        status: u16,
        response_bytes: usize,
        digest: &str,
        surface: &str,
    ) -> DriverError {
        let message = format!(
            "anthropic_messages {surface} returned HTTP {status} (bytes={response_bytes} blake3={digest})"
        );
        if self.retryable_http_statuses.contains(&status) {
            DriverError::Transport(message)
        } else {
            DriverError::Protocol(message)
        }
    }

    fn accept_response(
        &self,
        response: ProviderResponse,
    ) -> Result<AcceptedProviderResponse, DriverError> {
        let reason = response.stop_reason.as_deref().ok_or_else(|| {
            DriverError::Protocol(
                "anthropic_messages terminal response has no stop_reason".to_string(),
            )
        })?;
        if self.successful_stop_reasons.contains(reason) {
            return Ok(AcceptedProviderResponse {
                response,
                complete: true,
            });
        }
        if self.nonreplayable_stop_reasons.contains(reason) {
            return Ok(AcceptedProviderResponse {
                response,
                complete: false,
            });
        }
        Err(DriverError::Protocol(format!(
            "anthropic_messages returned stop_reason '{reason}' outside the descriptor finish policy"
        )))
    }
}

fn required_formed_models(
    input: &InterfaceInvocationEnvelope,
) -> Result<BTreeSet<String>, DriverError> {
    let policy = input.interface_policy()?.ok_or_else(|| {
        DriverError::FailedClosed(
            "anthropic_messages invocation carries no formed interface policy".to_string(),
        )
    })?;
    let values = policy
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            DriverError::FailedClosed(
                "anthropic_messages formed interface policy models must be a string array"
                    .to_string(),
            )
        })?;
    if values.is_empty() {
        return Err(DriverError::FailedClosed(
            "anthropic_messages formed interface policy models is empty".to_string(),
        ));
    }
    let mut models = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let model = value.as_str().filter(|model| {
            !model.is_empty() && model.trim() == *model && !model.contains(['\r', '\n', '\0'])
        });
        let Some(model) = model else {
            return Err(DriverError::FailedClosed(format!(
                "anthropic_messages formed interface policy models[{index}] is not a trimmed string"
            )));
        };
        if !models.insert(model.to_string()) {
            return Err(DriverError::FailedClosed(format!(
                "anthropic_messages formed interface policy models repeats '{model}'"
            )));
        }
    }
    Ok(models)
}

fn filter_model_catalog(data: &[Value], formed_models: &BTreeSet<String>) -> Vec<Value> {
    data.iter()
        .filter(|model| {
            model
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| formed_models.contains(id))
        })
        .cloned()
        .collect()
}

impl AnthropicMessagesOptions {
    fn for_provider_model(&self, model: &str) -> Result<Self, String> {
        if self.model_profiles.is_empty() {
            return Ok(self.clone());
        }
        let profile = self.model_profiles.get(model).ok_or_else(|| {
            format!("anthropic_messages selected model '{model}' has no exact model_profiles entry")
        })?;
        let mut resolved = self.clone();
        if let Some(max_tokens) = profile.max_tokens {
            resolved.max_tokens = max_tokens;
        }
        if let Some(input_content_types) = &profile.input_content_types {
            resolved.input_content_types = input_content_types.clone();
        }
        if let Some(response_content_types) = &profile.response_content_types {
            resolved.response_content_types = response_content_types.clone();
        }
        if let Some(thinking_replay) = profile.thinking_replay {
            resolved.thinking_replay = thinking_replay;
        }
        if let Some(request_fields) = &profile.request_fields {
            resolved.request_fields = request_fields.clone();
        }
        resolved.model_profiles.clear();
        resolved.validate()?;
        Ok(resolved)
    }

    fn validate(&self) -> Result<(), String> {
        if self.messages_path.trim() != self.messages_path
            || !self.messages_path.starts_with('/')
            || self.messages_path.contains('?')
            || self.messages_path.contains('#')
        {
            return Err(
                "anthropic_messages.messages_path must be a trimmed absolute URL path without query or fragment"
                    .to_string(),
            );
        }
        if let Some(path) = self.models_path.as_deref() {
            if path.trim() != path
                || !path.starts_with('/')
                || path.contains('?')
                || path.contains('#')
            {
                return Err(
                    "anthropic_messages.models_path must be a trimmed absolute URL path without query or fragment"
                        .to_string(),
                );
            }
        }
        if self.max_tokens == 0 {
            return Err("anthropic_messages.max_tokens must be positive".to_string());
        }
        if self
            .system_prefix
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.trim() != value || value.contains('\0'))
        {
            return Err(
                "anthropic_messages.system_prefix must be a non-empty trimmed string".to_string(),
            );
        }
        if self.tool_name_prefix.as_ref().is_some_and(|value| {
            value.is_empty()
                || value.trim() != value
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        }) {
            return Err(
                "anthropic_messages.tool_name_prefix must contain only ASCII letters, digits and underscores"
                    .to_string(),
            );
        }
        let protected = [
            "model",
            "messages",
            "system",
            "tools",
            "max_tokens",
            "stream",
        ];
        if let Some(key) = protected
            .iter()
            .find(|key| self.request_fields.contains_key(**key))
        {
            return Err(format!(
                "anthropic_messages.request_fields cannot override driver-owned '{key}'"
            ));
        }
        for (model, profile) in &self.model_profiles {
            if model.is_empty() || model.trim() != model || model.contains(['\r', '\n', '\0']) {
                return Err(
                    "anthropic_messages.model_profiles keys must be non-empty trimmed single-line model names"
                        .to_string(),
                );
            }
            let mut resolved = self.clone();
            if let Some(max_tokens) = profile.max_tokens {
                resolved.max_tokens = max_tokens;
            }
            if let Some(input_content_types) = &profile.input_content_types {
                resolved.input_content_types = input_content_types.clone();
            }
            if let Some(response_content_types) = &profile.response_content_types {
                resolved.response_content_types = response_content_types.clone();
            }
            if let Some(thinking_replay) = profile.thinking_replay {
                resolved.thinking_replay = thinking_replay;
            }
            if let Some(request_fields) = &profile.request_fields {
                resolved.request_fields = request_fields.clone();
            }
            resolved.model_profiles.clear();
            resolved.validate().map_err(|error| {
                format!("anthropic_messages.model_profiles['{model}']: {error}")
            })?;
            validate_declared_types(
                &resolved.input_content_types,
                &["text", "image", "document"],
                "input_content_types",
            )
            .map_err(|error| format!("anthropic_messages.model_profiles['{model}']: {error}"))?;
            validate_declared_types(
                &resolved.response_content_types,
                &["text", "thinking", "redacted_thinking", "tool_use"],
                "response_content_types",
            )
            .map_err(|error| format!("anthropic_messages.model_profiles['{model}']: {error}"))?;
        }
        Ok(())
    }
}

impl RetryPolicy {
    fn validate(&self) -> Result<(), String> {
        let mut statuses = BTreeSet::new();
        for (index, status) in self.retryable_http_statuses.iter().copied().enumerate() {
            if !(400..=599).contains(&status) {
                return Err(format!(
                    "anthropic_messages.retry_policy.retryable_http_statuses[{index}] must be an HTTP error status"
                ));
            }
            if !statuses.insert(status) {
                return Err(format!(
                    "anthropic_messages.retry_policy.retryable_http_statuses repeats {status}"
                ));
            }
        }
        if self.backoff_ms.is_empty() {
            return Err("anthropic_messages.retry_policy.backoff_ms must not be empty".to_string());
        }
        for (index, delay) in self.backoff_ms.iter().copied().enumerate() {
            if delay == 0 {
                return Err(format!(
                    "anthropic_messages.retry_policy.backoff_ms[{index}] must be positive"
                ));
            }
        }
        Ok(())
    }
}

impl FinishPolicy {
    fn validate(&self) -> Result<(), String> {
        let successful = validate_stop_reasons(
            &self.successful_stop_reasons,
            "successful_stop_reasons",
            false,
        )?;
        let nonreplayable = validate_stop_reasons(
            &self.nonreplayable_stop_reasons,
            "nonreplayable_stop_reasons",
            true,
        )?;
        if let Some(reason) = successful.intersection(&nonreplayable).next() {
            return Err(format!(
                "anthropic_messages.finish_policy stop reason '{reason}' is both successful and non-replayable"
            ));
        }
        Ok(())
    }
}

fn validate_stop_reasons(
    values: &[String],
    field: &str,
    allow_empty: bool,
) -> Result<BTreeSet<String>, String> {
    if values.is_empty() && !allow_empty {
        return Err(format!(
            "anthropic_messages.finish_policy.{field} must not be empty"
        ));
    }
    let mut reasons = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        if value.is_empty() || value.trim() != value {
            return Err(format!(
                "anthropic_messages.finish_policy.{field}[{index}] must be a non-empty trimmed string"
            ));
        }
        if !reasons.insert(value.clone()) {
            return Err(format!(
                "anthropic_messages.finish_policy.{field} repeats '{value}'"
            ));
        }
    }
    Ok(reasons)
}

impl ProviderAuth {
    fn credential_handle_fingerprint(&self) -> Option<String> {
        match &self.source {
            AuthSource::Secret { scope, name, .. } => {
                let reference = format!("secret://{}/{}", scope.name().unwrap_or("global"), name);
                Some(blake3::hash(reference.as_bytes()).to_hex().to_string())
            }
            AuthSource::AzureEntra(manager) => {
                Some(manager.credential_source_fingerprint().to_string())
            }
            AuthSource::Environment(_) => None,
        }
    }

    async fn header_value(&self) -> Result<HeaderValue, AnthropicDriverFailure> {
        let secret = match &self.source {
            AuthSource::Environment(name) => std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    AnthropicDriverFailure::Boundary(DriverError::FailedClosed(format!(
                        "anthropic_messages auth env var '{name}' is unset or empty"
                    )))
                })?,
            AuthSource::Secret {
                octopus_home,
                scope,
                name,
            } => SecretsManager::new_with_keyring_store(
                octopus_home.clone(),
                SecretsBackendKind::Local,
                Arc::new(LocalFileKeyringStore::new(
                    octopus_home.join("secrets").join("keyring"),
                )),
            )
            .get(scope, name)
            .map_err(|error| {
                AnthropicDriverFailure::Boundary(DriverError::FailedClosed(format!(
                    "anthropic_messages secret '{}/{}' could not be loaded: {error}",
                    scope.name().unwrap_or("global"),
                    name
                )))
            })?
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                AnthropicDriverFailure::Boundary(DriverError::FailedClosed(format!(
                    "anthropic_messages secret '{}/{}' is not configured",
                    scope.name().unwrap_or("global"),
                    name
                )))
            })?,
            AuthSource::AzureEntra(manager) => manager
                .access_token()
                .await
                .map_err(AnthropicDriverFailure::Authentication)?,
        };
        HeaderValue::from_str(&format!("{}{secret}", self.value_prefix)).map_err(|_| {
            AnthropicDriverFailure::Boundary(DriverError::FailedClosed(
                "anthropic_messages secret cannot form a valid header value".to_string(),
            ))
        })
    }

    async fn invalidate_azure_authentication(&self) -> bool {
        if let AuthSource::AzureEntra(manager) = &self.source {
            manager.invalidate().await;
            return true;
        }
        false
    }

    async fn refresh_azure_authentication(&self) -> Result<(), AnthropicDriverFailure> {
        let AuthSource::AzureEntra(manager) = &self.source else {
            return Err(AnthropicDriverFailure::Boundary(
                DriverError::FailedClosed(
                    "anthropic_messages requested Azure Entra refresh without descriptor-selected Azure Entra auth"
                        .to_string(),
                ),
            ));
        };
        manager
            .access_token()
            .await
            .map(|_| ())
            .map_err(AnthropicDriverFailure::Authentication)
    }
}

pub fn materialize_provider_turn_error(
    output_root: &Path,
    class: &str,
    message: &str,
) -> Result<InterfaceArtifactFile, DriverError> {
    materialize_provider_turn_failure(output_root, class, None, message, None, None)
}

pub fn materialize_provider_turn_typed_error(
    output_root: &Path,
    class: &str,
    code: &str,
    message: &str,
    evidence: Option<Value>,
) -> Result<InterfaceArtifactFile, DriverError> {
    let code = code.trim();
    if code.is_empty() {
        return Err(DriverError::Protocol(
            "provider turn error code must be non-empty".to_string(),
        ));
    }
    materialize_provider_turn_failure(output_root, class, Some(code), message, evidence, None)
}

pub(crate) fn materialize_provider_turn_failure(
    output_root: &Path,
    class: &str,
    code: Option<&str>,
    message: &str,
    evidence: Option<Value>,
    attempts: Option<u32>,
) -> Result<InterfaceArtifactFile, DriverError> {
    let class = class.trim();
    let message = message.trim();
    if class.is_empty() || message.is_empty() {
        return Err(DriverError::Protocol(
            "provider turn error class and message must be non-empty".to_string(),
        ));
    }
    let payload = serde_json::to_vec(&json!({
        "label": TURN_ERROR_LABEL,
        "class": class,
        "code": code,
        "message": message,
        "evidence": evidence,
        "attempts": attempts,
    }))
    .map_err(|error| DriverError::Protocol(format!("turn.error did not encode: {error}")))?;
    InterfaceArtifactFile::write(output_root, TURN_ERROR_LABEL, &payload)
        .map_err(|error| DriverError::Protocol(error.to_string()))
}

fn resolve_endpoint(
    transport: &InterfaceTransportConfig,
    messages_path: &str,
) -> Result<Url, String> {
    let base = if let Some(name) = transport
        .base_url_env
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                format!("anthropic_messages base URL env var '{name}' is unset or empty")
            })?
    } else {
        transport.base_url.clone()
    };
    validate_base_url(&base)?;
    Url::parse(&format!("{}{}", base.trim_end_matches('/'), messages_path))
        .map_err(|error| format!("anthropic_messages endpoint is invalid: {error}"))
}

fn validate_base_url(raw: &str) -> Result<(), String> {
    let url = Url::parse(raw)
        .map_err(|error| format!("anthropic_messages base_url is invalid: {error}"))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "anthropic_messages base_url cannot contain credentials, query or fragment".to_string(),
        );
    }
    let host = url
        .host_str()
        .ok_or_else(|| "anthropic_messages base_url has no host".to_string())?;
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    match url.scheme() {
        "https" => Ok(()),
        "http" if loopback => Ok(()),
        "http" => Err("anthropic_messages base_url requires HTTPS outside loopback".to_string()),
        scheme => Err(format!(
            "anthropic_messages base_url scheme '{scheme}' is unsupported"
        )),
    }
}

fn resolve_static_headers(transport: &InterfaceTransportConfig) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    for (name, value) in &transport.headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|error| format!("anthropic_messages header name is invalid: {error}"))?;
        if is_secret_header(&name) {
            return Err(format!(
                "anthropic_messages secret-bearing header '{}' must come from auth_secret_ref or auth_env_key",
                name
            ));
        }
        let value = HeaderValue::from_str(value)
            .map_err(|error| format!("anthropic_messages header value is invalid: {error}"))?;
        headers.insert(name, value);
    }
    if !headers.contains_key("anthropic-version") {
        return Err(
            "anthropic_messages transport.headers must declare anthropic-version".to_string(),
        );
    }
    Ok(headers)
}

fn is_secret_header(name: &HeaderName) -> bool {
    let name = name.as_str().to_ascii_lowercase();
    matches!(
        name.as_str(),
        "authorization"
            | "proxy-authorization"
            | "cookie"
            | "set-cookie"
            | "api-key"
            | "x-api-key"
            | "x-auth-token"
            | "anthropic-auth-token"
    ) || name.ends_with("-api-key")
        || name.ends_with("-access-token")
        || name.ends_with("-auth-token")
        || name.contains("secret")
}

fn resolve_auth(
    descriptor: &InterfaceDescriptor,
    transport: &InterfaceTransportConfig,
    activation_secrets_home: PathBuf,
) -> Result<ProviderAuth, String> {
    let header_name = transport
        .auth_header
        .as_deref()
        .ok_or_else(|| "anthropic_messages transport.auth_header must be declared".to_string())?;
    let header = HeaderName::from_bytes(header_name.as_bytes())
        .map_err(|error| format!("anthropic_messages auth_header is invalid: {error}"))?;
    let value_prefix = transport.auth_value_prefix.clone().ok_or_else(|| {
        "anthropic_messages transport.auth_value_prefix must be declared".to_string()
    })?;
    let secret_ref = transport
        .auth_secret_ref
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let env_key = transport
        .auth_env_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let azure_entra =
        AzureEntraCredentialManager::from_descriptor(descriptor, &activation_secrets_home)
            .map_err(|error| error.to_string())?;
    if secret_ref.is_some() && env_key.is_some() {
        return Err(
            "anthropic_messages transport cannot declare both auth_secret_ref and auth_env_key"
                .to_string(),
        );
    }
    let source = if let Some(manager) = azure_entra {
        if secret_ref.is_some() || env_key.is_some() {
            return Err(
                "anthropic_messages Azure Entra auth cannot declare auth_secret_ref or auth_env_key"
                    .to_string(),
            );
        }
        AuthSource::AzureEntra(Box::new(manager))
    } else if let Some(reference) = secret_ref {
        let (scope, name) = parse_secret_ref(reference)?;
        if scope.name() != Some(descriptor.id.as_str()) {
            return Err(format!(
                "anthropic_messages interface '{}' auth_secret_ref must use its exact interface scope",
                descriptor.id
            ));
        }
        AuthSource::Secret {
            octopus_home: activation_secrets_home,
            scope,
            name,
        }
    } else if let Some(name) = env_key {
        AuthSource::Environment(name.to_string())
    } else {
        return Err(
            "anthropic_messages transport requires auth_secret_ref or auth_env_key".to_string(),
        );
    };
    Ok(ProviderAuth {
        header,
        value_prefix,
        source,
    })
}

fn parse_secret_ref(reference: &str) -> Result<(SecretScope, SecretName), String> {
    let path = reference.strip_prefix("secret://").ok_or_else(|| {
        "anthropic_messages auth_secret_ref must start with secret://".to_string()
    })?;
    let (scope, name) = path.split_once('/').ok_or_else(|| {
        "anthropic_messages auth_secret_ref must be secret://<scope>/<name>".to_string()
    })?;
    if name.contains('/') {
        return Err("anthropic_messages auth_secret_ref must name one scoped secret".to_string());
    }
    Ok((
        SecretScope::named(scope.to_string()).map_err(|error| error.to_string())?,
        SecretName::new(name).map_err(|error| error.to_string())?,
    ))
}

fn validate_declared_types(
    values: &[String],
    allowed: &[&str],
    field: &str,
) -> Result<BTreeSet<String>, String> {
    if values.is_empty() {
        return Err(format!("anthropic_messages.{field} must not be empty"));
    }
    let mut result = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        if value.trim() != value || !allowed.contains(&value.as_str()) {
            return Err(format!(
                "anthropic_messages.{field}[{index}] declares unsupported type '{value}'"
            ));
        }
        if !result.insert(value.clone()) {
            return Err(format!("anthropic_messages.{field} repeats type '{value}'"));
        }
    }
    Ok(result)
}

fn validate_process_env_names(
    metadata: &Map<String, Value>,
    transport: &InterfaceTransportConfig,
) -> Result<(), String> {
    let values = metadata
        .get("process_env_names")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            "anthropic_messages private_config.process_env_names must be an array".to_string()
        })?;
    let mut declared = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let name = value
            .as_str()
            .filter(|name| !name.is_empty() && name.trim() == *name)
            .ok_or_else(|| {
                format!(
                    "anthropic_messages process_env_names[{index}] must be a non-empty trimmed string"
                )
            })?;
        if !declared.insert(name.to_string()) {
            return Err(format!(
                "anthropic_messages process_env_names repeats '{name}'"
            ));
        }
    }
    let expected = [
        transport.auth_env_key.as_deref(),
        transport.base_url_env.as_deref(),
        metadata.get("model_env").and_then(Value::as_str),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .map(str::to_string)
    .collect::<BTreeSet<_>>();
    if declared != expected {
        return Err(format!(
            "anthropic_messages process_env_names {:?} must exactly match transport env names {:?}",
            declared, expected
        ));
    }
    Ok(())
}

fn resolve_model(metadata: &Map<String, Value>) -> Result<String, DriverError> {
    let literal = metadata
        .get("model")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.trim() == *value);
    let env_name = metadata
        .get("model_env")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.trim() == *value);
    match (literal, env_name) {
        (Some(model), None) => Ok(model.to_string()),
        (None, Some(name)) => std::env::var(name)
            .ok()
            .filter(|value| !value.is_empty() && value.trim() == *value)
            .ok_or_else(|| {
                DriverError::FailedClosed(format!(
                    "anthropic_messages model env var '{name}' is unset, empty or untrimmed"
                ))
            }),
        (Some(_), Some(_)) => Err(DriverError::FailedClosed(
            "anthropic_messages descriptor cannot declare both model and model_env".to_string(),
        )),
        (None, None) => Err(DriverError::FailedClosed(
            "anthropic_messages descriptor must declare exactly one of model or model_env"
                .to_string(),
        )),
    }
}

fn parse_metadata_config<T: DeserializeOwned>(
    metadata: &Map<String, Value>,
    key: &str,
    interface: &str,
) -> Result<T, String> {
    let value = metadata.get(key).cloned().ok_or_else(|| {
        format!("provider interface '{interface}' has no private_config.{key} object")
    })?;
    serde_json::from_value(value).map_err(|error| {
        format!("provider interface '{interface}' private_config.{key} is malformed: {error}")
    })
}

async fn read_bounded(
    mut response: Response,
    maximum: usize,
    streamed: bool,
) -> Result<Vec<u8>, DriverError> {
    validate_response_content_length(response.content_length(), maximum)?;
    let mut bytes = Vec::new();
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) if streamed && !bytes.is_empty() => {
                return Err(DriverError::FailedClosed(format!(
                    "anthropic_messages partial stream cannot be replayed: {error}"
                )));
            }
            Err(error) => {
                return Err(DriverError::Transport(format!(
                    "anthropic_messages response read failed: {error}"
                )));
            }
        };
        let next = bytes.len().checked_add(chunk.len()).ok_or_else(|| {
            if streamed {
                DriverError::FailedClosed(
                    "anthropic_messages partial stream size overflowed".to_string(),
                )
            } else {
                DriverError::Transport("anthropic_messages response size overflowed".to_string())
            }
        })?;
        if next > maximum {
            return Err(if streamed {
                DriverError::FailedClosed(format!(
                    "anthropic_messages partial stream exceeds {maximum} bytes and cannot be replayed"
                ))
            } else {
                DriverError::Transport(format!(
                    "anthropic_messages response exceeds {maximum} bytes"
                ))
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn read_anthropic_stream(
    mut response: Response,
    maximum: usize,
) -> Result<(ProviderResponse, ResponseBodyEvidence), DriverError> {
    validate_response_content_length(response.content_length(), maximum)?;
    let mut decoder = AnthropicSseDecoder::new();
    let mut bytes = 0usize;
    let mut hasher = blake3::Hasher::new();
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) if bytes > 0 => {
                return Err(DriverError::FailedClosed(format!(
                    "anthropic_messages partial stream cannot be replayed after {bytes} bytes (blake3={}): {error}",
                    hasher.finalize().to_hex()
                )));
            }
            Err(error) => {
                return Err(DriverError::Transport(format!(
                    "anthropic_messages response read failed: {error}"
                )));
            }
        };
        bytes = bytes.checked_add(chunk.len()).ok_or_else(|| {
            DriverError::FailedClosed(
                "anthropic_messages partial stream size overflowed".to_string(),
            )
        })?;
        if bytes > maximum {
            return Err(DriverError::FailedClosed(format!(
                "anthropic_messages partial stream exceeds {maximum} bytes and cannot be replayed"
            )));
        }
        hasher.update(&chunk);
        decoder.push(&chunk)?;
    }
    let response = decoder.finish()?;
    Ok((
        response,
        ResponseBodyEvidence {
            bytes,
            blake3: hasher.finalize().to_hex().to_string(),
        },
    ))
}

fn validate_response_content_length(
    content_length: Option<u64>,
    maximum: usize,
) -> Result<(), DriverError> {
    if content_length.is_some_and(|length| length > maximum as u64) {
        return Err(DriverError::FailedClosed(format!(
            "anthropic_messages response exceeds {maximum} bytes and cannot be replayed"
        )));
    }
    Ok(())
}

fn translate_tools(
    input: &InterfaceInvocationEnvelope,
    hidden_tools: &[String],
    tool_name_prefix: Option<&str>,
) -> Result<(Vec<Value>, BTreeSet<String>), DriverError> {
    let visible = model_visible_affordances(input.interface_affordance_specs()?, hidden_tools)
        .map_err(DriverError::Protocol)?;
    let mut tools = Vec::with_capacity(visible.len());
    let mut model_visible_tool_names = BTreeSet::new();
    for (index, tool) in visible.iter().enumerate() {
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                DriverError::Protocol(format!(
                    "interface_affordance_specs[{index}] has no non-empty name"
                ))
            })?;
        let input_schema = tool
            .get("input_schema")
            .cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
        if !input_schema.is_object() {
            return Err(DriverError::Protocol(format!(
                "interface_affordance_specs[{index}] '{name}' input_schema is not an object"
            )));
        }
        if !model_visible_tool_names.insert(name.to_string()) {
            return Err(DriverError::Protocol(format!(
                "model-visible affordance '{name}' is duplicated"
            )));
        }
        tools.push(json!({
            "name": encode_tool_name(name, tool_name_prefix),
            "description": tool.get("description").and_then(Value::as_str).unwrap_or(""),
            "input_schema": input_schema,
        }));
    }
    Ok((tools, model_visible_tool_names))
}

fn context_artifacts(
    input: &InterfaceInvocationEnvelope,
    current: &Value,
    provider_interface: &InterfaceId,
    source_movement_seq: u64,
) -> Result<Vec<ContextArtifact>, DriverError> {
    let mut artifacts = Vec::with_capacity(input.artifact_binding_payloads.len() + 1);
    let mut seen_sources = BTreeSet::new();
    for bound in &input.artifact_binding_payloads {
        let mut value: Value = serde_json::from_slice(bound.as_bytes()).map_err(|error| {
            DriverError::Protocol(format!(
                "bound artifact '{}' is not JSON: {error}",
                bound.artifact.id
            ))
        })?;
        if let Some(snapshot) = value.get(SESSION_CONTEXT_FIELD) {
            let snapshot: SessionContext =
                serde_json::from_value(snapshot.clone()).map_err(|error| {
                    DriverError::Protocol(format!(
                        "bound session context '{}' is malformed: {error}",
                        bound.artifact.id
                    ))
                })?;
            if snapshot.schema_version != 2 {
                return Err(DriverError::Protocol(format!(
                    "bound session context '{}' has unsupported schema_version {}",
                    bound.artifact.id, snapshot.schema_version
                )));
            }
            if &snapshot.provider_interface != provider_interface {
                return Err(DriverError::Protocol(format!(
                    "bound session context '{}' belongs to provider '{}' instead of '{}'",
                    bound.artifact.id, snapshot.provider_interface, provider_interface
                )));
            }
            let mut snapshot_sources = BTreeSet::new();
            for (index, item) in snapshot.artifacts.into_iter().enumerate() {
                if item.label.trim() != item.label || item.label.is_empty() {
                    return Err(DriverError::Protocol(format!(
                        "bound session context '{}' artifact[{index}] has invalid label",
                        bound.artifact.id
                    )));
                }
                let source_key = format!(
                    "{}:{}:{}",
                    item.source_artifact.id,
                    item.source_artifact.content.to_hex(),
                    item.label
                );
                if !snapshot_sources.insert(source_key.clone()) {
                    return Err(DriverError::Protocol(format!(
                        "bound session context '{}' repeats an artifact source at index {index}",
                        bound.artifact.id
                    )));
                }
                if !seen_sources.insert(source_key) {
                    return Err(DriverError::Protocol(format!(
                        "bound session context '{}' repeats an artifact source across the invocation at index {index}",
                        bound.artifact.id
                    )));
                }
                if item.payload.get(SESSION_CONTEXT_FIELD).is_some() {
                    return Err(DriverError::Protocol(format!(
                        "bound session context '{}' artifact[{index}] nests the reserved carrier",
                        bound.artifact.id
                    )));
                }
                artifacts.push(ContextArtifact {
                    id: format!("{}#session:{index}", bound.artifact.id),
                    label: Some(item.label),
                    value: item.payload,
                    current: false,
                    source_artifact: item.source_artifact,
                    source_movement_seq: item.source_movement_seq,
                });
            }
            let label = payload_label(bound.artifact.id.as_str(), &value)
                .filter(|label| !label.is_empty() && label.trim() == label)
                .ok_or_else(|| {
                    DriverError::Protocol(format!(
                        "bound session context '{}' outer artifact has no valid label",
                        bound.artifact.id
                    ))
                })?;
            value
                .as_object_mut()
                .ok_or_else(|| {
                    DriverError::Protocol(format!(
                        "bound session context '{}' outer artifact is not an object",
                        bound.artifact.id
                    ))
                })?
                .remove(SESSION_CONTEXT_FIELD)
                .ok_or_else(|| {
                    DriverError::Protocol(format!(
                        "bound session context '{}' lost its reserved carrier",
                        bound.artifact.id
                    ))
                })?;
            let source_key = format!(
                "{}:{}:{}",
                bound.artifact.id,
                bound.artifact.content.to_hex(),
                label
            );
            if !seen_sources.insert(source_key) {
                return Err(DriverError::Protocol(format!(
                    "bound session context '{}' repeats its outer artifact source",
                    bound.artifact.id
                )));
            }
            artifacts.push(ContextArtifact {
                id: bound.artifact.id.to_string(),
                label: Some(label),
                value,
                current: false,
                source_artifact: bound.artifact.clone(),
                source_movement_seq: None,
            });
        } else {
            let label = payload_label(bound.artifact.id.as_str(), &value);
            if let Some(label) = label.as_deref() {
                let source_key = format!(
                    "{}:{}:{}",
                    bound.artifact.id,
                    bound.artifact.content.to_hex(),
                    label
                );
                if !seen_sources.insert(source_key) {
                    return Err(DriverError::Protocol(format!(
                        "bound artifact '{}' repeats an artifact source across the invocation",
                        bound.artifact.id
                    )));
                }
            }
            artifacts.push(ContextArtifact {
                id: bound.artifact.id.to_string(),
                label,
                value,
                current: false,
                source_artifact: bound.artifact.clone(),
                source_movement_seq: None,
            });
        }
    }
    let manifest = input.read_artifact_manifest()?;
    artifacts.push(ContextArtifact {
        id: input.artifact.id.to_string(),
        label: Some(manifest.label),
        value: current.clone(),
        current: true,
        source_artifact: input.artifact.clone(),
        source_movement_seq: Some(source_movement_seq),
    });
    Ok(artifacts)
}

fn plan_tool_result_fan_in(
    artifacts: &[ContextArtifact],
    provider_interface: &InterfaceId,
    run: &RunId,
    tool_name_prefix: Option<&str>,
) -> Result<AnthropicFanInPlan, DriverError> {
    let (groups, correlation_groups) = anthropic_tool_groups(artifacts, tool_name_prefix)?;
    validate_pending_frontiers(artifacts, &groups, provider_interface, run)?;
    let current = artifacts
        .iter()
        .filter(|artifact| artifact.current)
        .collect::<Vec<_>>();
    if current.len() != 1 {
        return Err(DriverError::Protocol(format!(
            "anthropic_messages context has {} current artifacts instead of one",
            current.len()
        )));
    }
    let current = current[0];
    let current_correlation = if current
        .label
        .as_deref()
        .is_some_and(|label| is_tool_result(label, &current.value))
    {
        Some(required_string(
            &current.value,
            "correlation_id",
            "current tool result",
        )?)
    } else {
        None
    };
    let unresolved = groups
        .iter()
        .filter(|(_, group)| group.results.len() != group.identity.calls.len())
        .map(|(response_id, _)| response_id.clone())
        .collect::<Vec<_>>();
    let Some(correlation_id) = current_correlation else {
        if unresolved.is_empty() {
            return Ok(AnthropicFanInPlan::Ready);
        }
        return Err(DriverError::FailedClosed(format!(
            "anthropic_messages unresolved tool group '{}' was interrupted by current artifact '{}'",
            unresolved.join(","),
            current.label.as_deref().unwrap_or("<unlabeled>")
        )));
    };
    let response_id = correlation_groups.get(&correlation_id).ok_or_else(|| {
        DriverError::Protocol(format!(
            "current tool result correlation_id '{correlation_id}' has no Anthropic tool_use group"
        ))
    })?;
    let group = groups.get(response_id).ok_or_else(|| {
        DriverError::Protocol(format!(
            "Anthropic tool group '{response_id}' disappeared from the context transcript"
        ))
    })?;
    if group.results.len() == group.identity.calls.len() {
        return Ok(AnthropicFanInPlan::Ready);
    }
    if unresolved.len() != 1 || unresolved[0] != *response_id {
        return Err(DriverError::Protocol(format!(
            "current tool result for Anthropic group '{response_id}' is mixed with unresolved groups '{}'",
            unresolved.join(",")
        )));
    }
    let received = group
        .identity
        .calls
        .iter()
        .filter_map(|call| {
            group
                .results
                .get(&call.correlation_id)
                .map(|record| record.identity.clone())
        })
        .collect::<Vec<_>>();
    if received.is_empty() || received.len() >= group.identity.calls.len() {
        return Err(DriverError::Protocol(format!(
            "Anthropic group '{response_id}' formed an invalid partial frontier"
        )));
    }
    Ok(AnthropicFanInPlan::Pending(PendingToolGroupState {
        schema_version: 1,
        provider_interface: provider_interface.clone(),
        run: run.clone(),
        group: group.identity.clone(),
        received,
    }))
}

fn anthropic_tool_groups(
    artifacts: &[ContextArtifact],
    tool_name_prefix: Option<&str>,
) -> Result<AnthropicToolGroups, DriverError> {
    let mut groups = BTreeMap::new();
    let mut correlation_groups = BTreeMap::new();
    for artifact in artifacts {
        if artifact.label.as_deref() != Some(INTERFACE_ARTIFACT_OUTPUT_LABEL) {
            continue;
        }
        let Some(blocks) = artifact
            .value
            .get("content_blocks")
            .and_then(Value::as_array)
        else {
            continue;
        };
        let mut calls = Vec::new();
        for block in blocks {
            if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let correlation_id = required_string(block, "id", "assistant tool_use block")?;
            let tool = decode_tool_name(
                &required_string(block, "name", "assistant tool_use block")?,
                tool_name_prefix,
            )?;
            let arguments = block
                .get("input")
                .filter(|value| value.is_object())
                .cloned()
                .ok_or_else(|| {
                    DriverError::Protocol(
                        "anthropic_messages assistant tool_use input is not an object".to_string(),
                    )
                })?;
            calls.push(AnthropicToolCallIdentity {
                correlation_id,
                tool,
                arguments,
            });
        }
        if calls.is_empty() {
            continue;
        }
        let response_id = required_string(
            &artifact.value,
            "response_id",
            INTERFACE_ARTIFACT_OUTPUT_LABEL,
        )?;
        if groups.contains_key(&response_id) {
            return Err(DriverError::Protocol(format!(
                "anthropic_messages repeats assistant tool group response_id '{response_id}'"
            )));
        }
        for call in &calls {
            if correlation_groups
                .insert(call.correlation_id.clone(), response_id.clone())
                .is_some()
            {
                return Err(DriverError::Protocol(format!(
                    "anthropic_messages repeats tool_use id '{}'",
                    call.correlation_id
                )));
            }
        }
        groups.insert(
            response_id.clone(),
            AnthropicToolGroup {
                identity: AnthropicToolGroupIdentity {
                    response_id,
                    source_artifact: artifact.source_artifact.clone(),
                    source_movement_seq: artifact.source_movement_seq,
                    calls,
                },
                results: BTreeMap::new(),
            },
        );
    }
    let mut emitted_calls = BTreeSet::new();
    for artifact in artifacts {
        if artifact.label.as_deref() != Some(TOOL_CALL_LABEL) {
            continue;
        }
        let correlation_id = required_string(&artifact.value, "correlation_id", TOOL_CALL_LABEL)?;
        let response_id = correlation_groups.get(&correlation_id).ok_or_else(|| {
            DriverError::Protocol(format!(
                "tool.call correlation_id '{correlation_id}' has no native Anthropic tool_use block"
            ))
        })?;
        let group = groups.get(response_id).ok_or_else(|| {
            DriverError::Protocol(format!(
                "tool.call correlation_id '{correlation_id}' resolved to missing Anthropic group '{response_id}'"
            ))
        })?;
        let expected = group
            .identity
            .calls
            .iter()
            .find(|call| call.correlation_id == correlation_id)
            .ok_or_else(|| {
                DriverError::Protocol(format!(
                    "Anthropic group '{response_id}' has no call '{correlation_id}'"
                ))
            })?;
        let tool = required_string(&artifact.value, "tool", TOOL_CALL_LABEL)?;
        let arguments = artifact
            .value
            .get("arguments")
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        if tool != expected.tool || arguments != expected.arguments {
            return Err(DriverError::Protocol(format!(
                "tool.call correlation_id '{correlation_id}' differs from its native Anthropic tool_use block"
            )));
        }
        if !emitted_calls.insert(correlation_id.clone()) {
            return Err(DriverError::Protocol(format!(
                "anthropic_messages repeats tool.call correlation_id '{correlation_id}'"
            )));
        }
    }
    for (correlation_id, response_id) in &correlation_groups {
        if !emitted_calls.contains(correlation_id) {
            return Err(DriverError::Protocol(format!(
                "Anthropic group '{response_id}' has no emitted tool.call for correlation_id '{correlation_id}'"
            )));
        }
    }
    for (index, artifact) in artifacts.iter().enumerate() {
        let Some(label) = artifact.label.as_deref() else {
            continue;
        };
        if !is_tool_result(label, &artifact.value) {
            continue;
        }
        let correlation_id = required_string(&artifact.value, "correlation_id", "tool result")?;
        let response_id = correlation_groups.get(&correlation_id).ok_or_else(|| {
            DriverError::Protocol(format!(
                "tool result correlation_id '{correlation_id}' has no native Anthropic tool_use group"
            ))
        })?;
        let group = groups.get_mut(response_id).ok_or_else(|| {
            DriverError::Protocol(format!(
                "tool result correlation_id '{correlation_id}' resolved to missing Anthropic group '{response_id}'"
            ))
        })?;
        let expected = group
            .identity
            .calls
            .iter()
            .find(|call| call.correlation_id == correlation_id)
            .ok_or_else(|| {
                DriverError::Protocol(format!(
                    "Anthropic group '{response_id}' has no expected result '{correlation_id}'"
                ))
            })?;
        let tool = required_string(&artifact.value, "tool", "tool result")?;
        let declared_label = required_string(&artifact.value, "label", "tool result")?;
        if tool != expected.tool || declared_label != format!("{tool}.result") {
            return Err(DriverError::Protocol(format!(
                "tool result correlation_id '{correlation_id}' does not match its native Anthropic call"
            )));
        }
        let record = AnthropicToolResultRecord {
            identity: AnthropicToolResultIdentity {
                correlation_id: correlation_id.clone(),
                tool,
                source_artifact: artifact.source_artifact.clone(),
                source_movement_seq: artifact.source_movement_seq,
            },
            index,
        };
        if group
            .results
            .insert(correlation_id.clone(), record)
            .is_some()
        {
            return Err(DriverError::Protocol(format!(
                "anthropic_messages repeats tool result correlation_id '{correlation_id}'"
            )));
        }
    }
    Ok((groups, correlation_groups))
}

fn validate_pending_frontiers(
    artifacts: &[ContextArtifact],
    groups: &BTreeMap<String, AnthropicToolGroup>,
    provider_interface: &InterfaceId,
    run: &RunId,
) -> Result<(), DriverError> {
    let mut previous = BTreeMap::<String, BTreeSet<String>>::new();
    for (index, artifact) in artifacts.iter().enumerate() {
        if artifact.label.as_deref() != Some(TOOL_RESULTS_PENDING_LABEL) {
            continue;
        }
        let carrier: PendingToolGroupArtifact = serde_json::from_value(artifact.value.clone())
            .map_err(|error| {
                DriverError::Protocol(format!(
                    "anthropic_messages pending tool frontier is malformed: {error}"
                ))
            })?;
        if carrier.label != TOOL_RESULTS_PENDING_LABEL
            || carrier.state.schema_version != 1
            || &carrier.state.provider_interface != provider_interface
            || &carrier.state.run != run
        {
            return Err(DriverError::Protocol(
                "anthropic_messages pending tool frontier has mismatched identity".to_string(),
            ));
        }
        let response_id = carrier.state.group.response_id.clone();
        let group = groups.get(&response_id).ok_or_else(|| {
            DriverError::Protocol(format!(
                "pending Anthropic frontier names unknown response_id '{response_id}'"
            ))
        })?;
        if !pending_group_matches_replayed_group(&carrier.state.group, &group.identity) {
            return Err(DriverError::Protocol(format!(
                "pending Anthropic frontier for '{response_id}' changed immutable group provenance"
            )));
        }
        let expected = group
            .identity
            .calls
            .iter()
            .filter_map(|call| {
                group
                    .results
                    .get(&call.correlation_id)
                    .and_then(|result| (result.index < index).then(|| result.identity.clone()))
            })
            .collect::<Vec<_>>();
        if carrier.state.received != expected || expected.len() >= group.identity.calls.len() {
            return Err(DriverError::Protocol(format!(
                "pending Anthropic frontier for '{response_id}' does not match preceding result provenance"
            )));
        }
        let received = expected
            .iter()
            .map(|result| result.correlation_id.clone())
            .collect::<BTreeSet<_>>();
        if let Some(prior) = previous.get(&response_id) {
            if !prior.is_subset(&received) || prior.len() >= received.len() {
                return Err(DriverError::Protocol(format!(
                    "pending Anthropic frontier for '{response_id}' did not advance monotonically"
                )));
            }
        }
        previous.insert(response_id, received);
    }
    Ok(())
}

pub fn pending_group_matches_replayed_group(
    pending: &AnthropicToolGroupIdentity,
    replayed: &AnthropicToolGroupIdentity,
) -> bool {
    pending.response_id == replayed.response_id
        && pending.source_artifact == replayed.source_artifact
        && pending.calls == replayed.calls
        && match (pending.source_movement_seq, replayed.source_movement_seq) {
            (None, Some(_)) => true,
            (pending, replayed) => pending == replayed,
        }
}

fn materialize_pending_tool_group(
    output_root: &Path,
    state: PendingToolGroupState,
) -> Result<ProviderEmission, DriverError> {
    materialize_emission(
        output_root,
        TOOL_RESULTS_PENDING_LABEL,
        serde_json::to_value(PendingToolGroupArtifact {
            label: TOOL_RESULTS_PENDING_LABEL.to_string(),
            state,
            session_context: None,
        })
        .map_err(|error| {
            DriverError::Protocol(format!(
                "anthropic_messages pending tool frontier did not encode: {error}"
            ))
        })?,
        ProviderEmissionRoute::ClientProjection,
    )
}

pub fn pending_group_state(value: &Value) -> Result<PendingToolGroupState, String> {
    let carrier: PendingToolGroupArtifact = serde_json::from_value(value.clone())
        .map_err(|error| format!("Anthropic pending frontier is malformed: {error}"))?;
    if carrier.label != TOOL_RESULTS_PENDING_LABEL || carrier.state.schema_version != 1 {
        return Err("Anthropic pending frontier has unsupported identity".to_string());
    }
    Ok(carrier.state)
}

pub fn current_tool_group_identity(
    input: &InterfaceInvocationEnvelope,
    provider_interface: &InterfaceId,
    run: &RunId,
    source_movement_seq: u64,
    tool_name_prefix: Option<&str>,
) -> Result<Option<AnthropicToolGroupIdentity>, String> {
    let payload = input
        .read_artifact_payload()
        .map_err(|error| error.to_string())?;
    let current: Value = serde_json::from_slice(&payload)
        .map_err(|error| format!("Anthropic input artifact is not JSON: {error}"))?;
    let manifest = input
        .read_artifact_manifest()
        .map_err(|error| error.to_string())?;
    if !is_tool_result(&manifest.label, &current) {
        return Ok(None);
    }
    let runtime = input.runtime().map_err(|error| error.to_string())?;
    if &runtime.interface != provider_interface {
        return Err(format!(
            "Anthropic invocation belongs to interface '{}' instead of '{}'",
            runtime.interface, provider_interface
        ));
    }
    if &runtime.run != run {
        return Err(format!(
            "Anthropic invocation belongs to run '{}' instead of '{}'",
            runtime.run, run
        ));
    }
    let artifacts = context_artifacts(input, &current, provider_interface, source_movement_seq)
        .map_err(|error| error.to_string())?;
    let (groups, correlations) =
        anthropic_tool_groups(&artifacts, tool_name_prefix).map_err(|error| error.to_string())?;
    validate_pending_frontiers(&artifacts, &groups, provider_interface, run)
        .map_err(|error| error.to_string())?;
    let correlation_id = required_string(&current, "correlation_id", "current tool result")
        .map_err(|error| error.to_string())?;
    let response_id = correlations.get(&correlation_id).ok_or_else(|| {
        format!("current tool result correlation_id '{correlation_id}' has no Anthropic tool group")
    })?;
    groups
        .get(response_id)
        .map(|group| Some(group.identity.clone()))
        .ok_or_else(|| format!("Anthropic tool group '{response_id}' is absent"))
}

fn assemble_turn(
    input: &InterfaceInvocationEnvelope,
    artifacts: &[ContextArtifact],
    allowed_content: &BTreeSet<String>,
    thinking_replay: ThinkingReplay,
    hidden_tools: &[String],
    tool_name_prefix: Option<&str>,
) -> Result<AssembledTurn, DriverError> {
    let mut system = Vec::new();
    let mut messages = Vec::new();
    let mut captured_tool_ids = BTreeSet::new();
    let mut known_tool_ids = BTreeSet::new();
    for artifact in artifacts {
        if artifact.label.as_deref() == Some(TOOL_CALL_LABEL) {
            known_tool_ids.insert(required_string(
                &artifact.value,
                "correlation_id",
                TOOL_CALL_LABEL,
            )?);
        }
        if artifact.label.as_deref() == Some(INTERFACE_ARTIFACT_OUTPUT_LABEL) {
            if let Some(blocks) = artifact
                .value
                .get("content_blocks")
                .and_then(Value::as_array)
            {
                for block in blocks {
                    if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                        captured_tool_ids.insert(required_string(
                            block,
                            "id",
                            "assistant tool_use block",
                        )?);
                    }
                }
            }
        }
    }

    for artifact in artifacts {
        match artifact.label.as_deref() {
            Some(CONTEXT_REFERENCE_LABEL) => {
                let text = artifact
                    .value
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        DriverError::Protocol(format!(
                            "context.reference artifact '{}' has no non-empty text",
                            artifact.id
                        ))
                    })?;
                system.push(json!({"type": "text", "text": text}));
            }
            Some(TOOL_CALL_LABEL) => {
                let correlation_id =
                    required_string(&artifact.value, "correlation_id", TOOL_CALL_LABEL)?;
                if !captured_tool_ids.contains(&correlation_id) {
                    push_message(
                        &mut messages,
                        "assistant",
                        vec![tool_use_block(&artifact.value, tool_name_prefix)?],
                    )?;
                }
            }
            Some(label) if is_tool_result(label, &artifact.value) => {
                let tool_use_id =
                    required_string(&artifact.value, "correlation_id", "tool result")?;
                if !known_tool_ids.contains(&tool_use_id)
                    && !captured_tool_ids.contains(&tool_use_id)
                {
                    return Err(DriverError::Protocol(format!(
                        "tool result correlation_id '{tool_use_id}' has no bound tool.call"
                    )));
                }
                push_message(
                    &mut messages,
                    "user",
                    vec![tool_result_block(&artifact.value, allowed_content)?],
                )?;
            }
            Some(INTERFACE_ARTIFACT_OUTPUT_LABEL) => {
                let blocks = assistant_blocks(&artifact.value, allowed_content, thinking_replay)?;
                if !blocks.is_empty() {
                    push_message(&mut messages, "assistant", blocks)?;
                }
            }
            Some(TURN_ERROR_LABEL)
            | Some("operator.approval.request")
            | Some("operator.elicitation.request") => {
                push_message(
                    &mut messages,
                    "assistant",
                    vec![json!({"type": "text", "text": render_json(&artifact.value)?})],
                )?;
            }
            Some(TOOL_RESULTS_PENDING_LABEL) => {}
            _ => {
                if let Some((role, blocks)) = explicit_message(&artifact.value, allowed_content)? {
                    if role == "system" {
                        system.extend(blocks);
                    } else {
                        push_message(&mut messages, role, blocks)?;
                    }
                } else if let Some(prompt) = artifact
                    .value
                    .get("prompt")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                {
                    push_message(
                        &mut messages,
                        "user",
                        vec![json!({"type": "text", "text": prompt})],
                    )?;
                } else if let Some(task) = artifact.value.get("task") {
                    let text = task
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or(render_json(task)?);
                    push_message(
                        &mut messages,
                        "user",
                        vec![json!({"type": "text", "text": text})],
                    )?;
                } else if artifact.current {
                    push_message(
                        &mut messages,
                        "user",
                        vec![json!({"type": "text", "text": render_json(&artifact.value)?})],
                    )?;
                } else {
                    system.push(json!({
                        "type": "text",
                        "text": format!("Bound Octopus artifact {}:\n{}", artifact.id, render_json(&artifact.value)?),
                    }));
                }
            }
        }
    }
    if let Some(metadata) = input
        .metadata
        .as_ref()
        .and_then(|metadata| render_model_context_block_with_hidden_tools(metadata, hidden_tools))
    {
        system.push(json!({"type": "text", "text": metadata}));
    }
    if system.is_empty() {
        return Err(DriverError::FailedClosed(
            "anthropic_messages context is missing bound system instructions".to_string(),
        ));
    }
    if messages.is_empty() {
        return Err(DriverError::FailedClosed(
            "anthropic_messages turn has no user/assistant/tool content".to_string(),
        ));
    }
    validate_tool_order(&messages)?;
    Ok(AssembledTurn { system, messages })
}

fn explicit_message<'a>(
    value: &'a Value,
    allowed: &BTreeSet<String>,
) -> Result<Option<(&'a str, Vec<Value>)>, DriverError> {
    let message = value.get("message").unwrap_or(value);
    let Some(role) = message.get("role").and_then(Value::as_str) else {
        return Ok(None);
    };
    if !matches!(role, "system" | "user" | "assistant") {
        return Err(DriverError::Protocol(format!(
            "anthropic_messages explicit message role '{role}' is unsupported"
        )));
    }
    let content = message.get("content").ok_or_else(|| {
        DriverError::Protocol("anthropic_messages explicit message has no content".to_string())
    })?;
    let blocks = normalize_content(content, allowed, role)?;
    if role == "system"
        && blocks
            .iter()
            .any(|block| block.get("type") != Some(&Value::String("text".to_string())))
    {
        return Err(DriverError::Protocol(
            "anthropic_messages system content accepts text blocks only".to_string(),
        ));
    }
    Ok(Some((role, blocks)))
}

fn normalize_content(
    content: &Value,
    allowed: &BTreeSet<String>,
    surface: &str,
) -> Result<Vec<Value>, DriverError> {
    if let Some(text) = content.as_str() {
        if text.is_empty() {
            return Err(DriverError::Protocol(format!(
                "anthropic_messages {surface} text content is empty"
            )));
        }
        return Ok(vec![json!({"type": "text", "text": text})]);
    }
    let blocks = content.as_array().ok_or_else(|| {
        DriverError::Protocol(format!(
            "anthropic_messages {surface} content must be string or array"
        ))
    })?;
    if blocks.is_empty() {
        return Err(DriverError::Protocol(format!(
            "anthropic_messages {surface} content array is empty"
        )));
    }
    blocks
        .iter()
        .enumerate()
        .map(|(index, block)| normalize_input_block(block, allowed, surface, index))
        .collect()
}

fn normalize_input_block(
    block: &Value,
    allowed: &BTreeSet<String>,
    surface: &str,
    index: usize,
) -> Result<Value, DriverError> {
    let block_type = block.get("type").and_then(Value::as_str).ok_or_else(|| {
        DriverError::Protocol(format!(
            "anthropic_messages {surface} content[{index}] has no type"
        ))
    })?;
    if !allowed.contains(block_type) {
        return Err(DriverError::Protocol(format!(
            "anthropic_messages {surface} content[{index}] type '{block_type}' is not declared by input_content_types"
        )));
    }
    match block_type {
        "text" => Ok(json!({
            "type": "text",
            "text": required_string(block, "text", "text block")?,
        })),
        "image" | "document" => {
            let source = block
                .get("source")
                .filter(|value| value.is_object())
                .ok_or_else(|| {
                    DriverError::Protocol(format!(
                        "anthropic_messages {block_type} block has no source object"
                    ))
                })?;
            Ok(json!({"type": block_type, "source": source}))
        }
        _ => Err(DriverError::Protocol(format!(
            "anthropic_messages input block type '{block_type}' is unsupported"
        ))),
    }
}

fn assistant_blocks(
    value: &Value,
    allowed: &BTreeSet<String>,
    replay: ThinkingReplay,
) -> Result<Vec<Value>, DriverError> {
    if let Some(blocks) = value.get("content_blocks").and_then(Value::as_array) {
        let mut result = Vec::new();
        let mut has_wire_content = false;
        for (index, block) in blocks.iter().enumerate() {
            let block_type = block.get("type").and_then(Value::as_str).ok_or_else(|| {
                DriverError::Protocol(format!("assistant content_blocks[{index}] has no type"))
            })?;
            match block_type {
                "text" => {
                    has_wire_content = true;
                    result.push(json!({
                        "type": "text",
                        "text": required_string(block, "text", "assistant text block")?,
                    }));
                }
                "tool_use" => {
                    has_wire_content = true;
                    result.push(json!({
                        "type": "tool_use",
                        "id": required_string(block, "id", "assistant tool_use block")?,
                        "name": required_string(block, "name", "assistant tool_use block")?,
                        "input": block.get("input").filter(|value| value.is_object()).cloned().ok_or_else(|| DriverError::Protocol("assistant tool_use input is not an object".to_string()))?,
                    }));
                }
                "thinking" => match replay {
                    ThinkingReplay::Drop => {}
                    ThinkingReplay::UnsignedOnly if block.get("signature").is_some() => {}
                    ThinkingReplay::UnsignedOnly | ThinkingReplay::Verbatim => {
                        let mut clean = json!({
                            "type": "thinking",
                            "thinking": required_string(block, "thinking", "assistant thinking block")?,
                        });
                        if let Some(signature) = block.get("signature").and_then(Value::as_str) {
                            clean["signature"] = Value::String(signature.to_string());
                        }
                        result.push(clean);
                    }
                },
                "redacted_thinking" => match replay {
                    ThinkingReplay::Verbatim => result.push(json!({
                        "type": "redacted_thinking",
                        "data": required_string(block, "data", "assistant redacted_thinking block")?,
                    })),
                    ThinkingReplay::UnsignedOnly | ThinkingReplay::Drop => {}
                },
                other if allowed.contains(other) => {
                    return Err(DriverError::Protocol(format!(
                        "assistant replay block type '{other}' is declared but not supported"
                    )));
                }
                other => {
                    return Err(DriverError::Protocol(format!(
                        "assistant replay block type '{other}' is not declared"
                    )));
                }
            }
        }
        if !has_wire_content {
            return Ok(Vec::new());
        }
        if result.is_empty() {
            return Err(DriverError::Protocol(
                "assistant replay content became empty under declared thinking_replay policy"
                    .to_string(),
            ));
        }
        return Ok(result);
    }
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            DriverError::Protocol(
                "interface.artifact.output has neither content_blocks nor non-empty text"
                    .to_string(),
            )
        })?;
    Ok(vec![json!({"type": "text", "text": text})])
}

fn tool_use_block(value: &Value, tool_name_prefix: Option<&str>) -> Result<Value, DriverError> {
    let tool = required_string(value, "tool", TOOL_CALL_LABEL)?;
    let correlation_id = required_string(value, "correlation_id", TOOL_CALL_LABEL)?;
    let input = value
        .get("arguments")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    Ok(json!({
        "type": "tool_use",
        "id": correlation_id,
        "name": encode_tool_name(&tool, tool_name_prefix),
        "input": input,
    }))
}

fn tool_result_block(value: &Value, allowed: &BTreeSet<String>) -> Result<Value, DriverError> {
    let result: ToolResultArtifact = serde_json::from_value(value.clone()).map_err(|error| {
        DriverError::Protocol(format!(
            "tool result is not the strict shared envelope: {error}"
        ))
    })?;
    let correlation_id = result
        .correlation_id
        .as_deref()
        .ok_or_else(|| DriverError::Protocol("tool result has no correlation_id".to_string()))?;
    let content = if result.ok {
        let body = result.body.as_ref().ok_or_else(|| {
            DriverError::Protocol("successful tool result has no body".to_string())
        })?;
        if let Some(content) = body.get("content") {
            Value::Array(normalize_content(content, allowed, "tool_result")?)
        } else {
            Value::String(render_json(body)?)
        }
    } else {
        let error =
            serde_json::to_value(result.error.as_ref().ok_or_else(|| {
                DriverError::Protocol("failed tool result has no error".to_string())
            })?)
            .map_err(|error| {
                DriverError::Protocol(format!("tool result error could not be rendered: {error}"))
            })?;
        Value::String(render_json(&error)?)
    };
    Ok(json!({
        "type": "tool_result",
        "tool_use_id": correlation_id,
        "content": content,
        "is_error": !result.ok,
    }))
}

fn push_message(
    messages: &mut Vec<Value>,
    role: &str,
    blocks: Vec<Value>,
) -> Result<(), DriverError> {
    if blocks.is_empty() {
        return Err(DriverError::Protocol(format!(
            "anthropic_messages cannot append empty {role} content"
        )));
    }
    if let Some(last) = messages.last_mut() {
        if last.get("role").and_then(Value::as_str) == Some(role) {
            let content = last
                .get_mut("content")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| {
                    DriverError::Protocol(
                        "anthropic_messages accumulated message content is not an array"
                            .to_string(),
                    )
                })?;
            content.extend(blocks);
            return Ok(());
        }
    }
    messages.push(json!({"role": role, "content": blocks}));
    Ok(())
}

fn validate_tool_order(messages: &[Value]) -> Result<(), DriverError> {
    let mut known = BTreeSet::new();
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let blocks = message
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                DriverError::Protocol(
                    "anthropic_messages message content is not an array".to_string(),
                )
            })?;
        let mut saw_non_result = false;
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("tool_use") => {
                    known.insert(required_string(block, "id", "tool_use block")?);
                }
                Some("tool_result") => {
                    if role != "user" || saw_non_result {
                        return Err(DriverError::Protocol(
                            "tool_result blocks must lead a user message".to_string(),
                        ));
                    }
                    let id = required_string(block, "tool_use_id", "tool_result block")?;
                    if !known.contains(&id) {
                        return Err(DriverError::Protocol(format!(
                            "tool_result '{id}' has no preceding tool_use"
                        )));
                    }
                }
                Some(_) => saw_non_result = true,
                None => {
                    return Err(DriverError::Protocol(
                        "anthropic_messages content block has no type".to_string(),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn payload_label(artifact_id: &str, value: &Value) -> Option<String> {
    value
        .get("label")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            artifact_id
                .split_once(':')
                .map(|(label, _)| label.to_string())
        })
}

fn is_tool_result(label: &str, value: &Value) -> bool {
    label.ends_with(".result") && value.get("tool").is_some() && value.get("ok").is_some()
}

fn required_string(value: &Value, key: &str, surface: &str) -> Result<String, DriverError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.trim() == *value)
        .map(str::to_string)
        .ok_or_else(|| {
            DriverError::Protocol(format!(
                "anthropic_messages {surface} has no non-empty trimmed '{key}'"
            ))
        })
}

fn render_json(value: &Value) -> Result<String, DriverError> {
    serde_json::to_string(value).map_err(|error| {
        DriverError::Protocol(format!(
            "anthropic_messages context did not encode: {error}"
        ))
    })
}

fn encode_tool_name(name: &str, prefix: Option<&str>) -> String {
    let mut encoded = String::with_capacity(prefix.map_or(0, str::len) + name.len());
    if let Some(prefix) = prefix {
        encoded.push_str(prefix);
    }
    for character in name.chars() {
        match character {
            '-' => encoded.push_str("-h"),
            '.' => encoded.push_str("-d"),
            other => encoded.push(other),
        }
    }
    encoded
}

fn decode_tool_name(encoded: &str, prefix: Option<&str>) -> Result<String, DriverError> {
    let encoded = match prefix {
        Some(prefix) => encoded.strip_prefix(prefix).ok_or_else(|| {
            DriverError::Protocol(format!(
                "anthropic_messages tool name '{encoded}' does not carry descriptor prefix '{prefix}'"
            ))
        })?,
        None => encoded,
    };
    let mut decoded = String::with_capacity(encoded.len());
    let mut characters = encoded.chars();
    while let Some(character) = characters.next() {
        if character != '-' {
            decoded.push(character);
            continue;
        }
        match characters.next() {
            Some('h') => decoded.push('-'),
            Some('d') => decoded.push('.'),
            other => {
                return Err(DriverError::Protocol(format!(
                    "anthropic_messages tool name '{encoded}' has malformed escape {other:?}"
                )));
            }
        }
    }
    Ok(decoded)
}

fn parse_json_response(
    bytes: &[u8],
    status: u16,
    digest: String,
) -> Result<ProviderResponse, DriverError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|error| {
        DriverError::Protocol(format!("anthropic_messages response is not JSON: {error}"))
    })?;
    response_from_value(
        &value,
        TransportEvidence {
            streamed: false,
            status,
            response_bytes: bytes.len(),
            response_blake3: digest,
            event_count: 0,
            event_types: BTreeMap::new(),
        },
    )
}

fn response_from_value(
    value: &Value,
    transport: TransportEvidence,
) -> Result<ProviderResponse, DriverError> {
    let id = required_string(value, "id", "response")?;
    let model = required_string(value, "model", "response")?;
    let content = value
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| {
            DriverError::Protocol("anthropic_messages response has no content array".to_string())
        })?;
    let stop_reason = match value.get("stop_reason") {
        None | Some(Value::Null) => None,
        Some(Value::String(reason)) if !reason.trim().is_empty() => Some(reason.clone()),
        Some(_) => {
            return Err(DriverError::Protocol(
                "anthropic_messages stop_reason is malformed".to_string(),
            ));
        }
    };
    if content.is_empty() && stop_reason.is_none() {
        return Err(DriverError::Protocol(
            "anthropic_messages response has neither content nor stop_reason".to_string(),
        ));
    }
    let usage = value.get("usage").cloned().unwrap_or_else(|| json!({}));
    if !usage.is_object() {
        return Err(DriverError::Protocol(
            "anthropic_messages response usage is not an object".to_string(),
        ));
    }
    Ok(ProviderResponse {
        id,
        model,
        stop_reason,
        content,
        usage,
        transport,
    })
}

impl AnthropicSseDecoder {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
            frame_index: 0,
            accumulator: StreamAccumulator::new(),
        }
    }

    fn push(&mut self, bytes: &[u8]) -> Result<(), DriverError> {
        self.pending.extend_from_slice(bytes);
        let mut consumed = 0usize;
        while let Some((frame_end, delimiter_end)) = find_sse_boundary(&self.pending[consumed..]) {
            let frame_end = consumed + frame_end;
            let delimiter_end = consumed + delimiter_end;
            let frame = self.pending[consumed..frame_end].to_vec();
            self.apply_frame(&frame)?;
            consumed = delimiter_end;
        }
        if consumed > 0 {
            self.pending.drain(..consumed);
        }
        Ok(())
    }

    fn finish(mut self) -> Result<ProviderResponse, DriverError> {
        if self.pending.iter().any(|byte| !byte.is_ascii_whitespace()) {
            let frame = std::mem::take(&mut self.pending);
            self.apply_frame(&frame)?;
        }
        self.accumulator.finish()
    }

    fn apply_frame(&mut self, frame: &[u8]) -> Result<(), DriverError> {
        if frame.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let text = std::str::from_utf8(frame).map_err(|error| {
            DriverError::Protocol(format!(
                "anthropic_messages SSE frame {} is not UTF-8: {error}",
                self.frame_index
            ))
        })?;
        let mut event_name = None;
        let mut data = String::new();
        for line in text.split(['\r', '\n']).filter(|line| !line.is_empty()) {
            if let Some(value) = line.strip_prefix("event:") {
                event_name = Some(value.trim().to_string());
            } else if let Some(value) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value.trim_start());
            }
        }
        let frame_index = self.frame_index;
        self.frame_index = self.frame_index.checked_add(1).ok_or_else(|| {
            DriverError::Protocol("anthropic_messages SSE frame count overflowed".to_string())
        })?;
        if data == "[DONE]" {
            return Err(DriverError::Protocol(
                "anthropic_messages SSE uses message_stop, not a [DONE] sentinel".to_string(),
            ));
        }
        if data.is_empty() {
            return Err(DriverError::Protocol(format!(
                "anthropic_messages SSE frame {frame_index} has no data"
            )));
        }
        let value: Value = serde_json::from_str(&data).map_err(|error| {
            DriverError::Protocol(format!(
                "anthropic_messages SSE frame {frame_index} data is not JSON: {error}"
            ))
        })?;
        let payload_type = required_string(&value, "type", "SSE event")?;
        if event_name
            .as_deref()
            .is_some_and(|name| name != payload_type)
        {
            return Err(DriverError::Protocol(format!(
                "anthropic_messages SSE event name {:?} does not match payload type '{payload_type}'",
                event_name
            )));
        }
        self.accumulator.apply(&payload_type, &value)
    }
}

fn find_sse_boundary(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut index = 0usize;
    while index < bytes.len() {
        if let Some(first) = sse_line_ending(bytes, index) {
            if let Some(second) = sse_line_ending(bytes, first) {
                return Some((index, second));
            }
            index = first;
        } else {
            index += 1;
        }
    }
    None
}

fn sse_line_ending(bytes: &[u8], index: usize) -> Option<usize> {
    match bytes.get(index) {
        Some(b'\n') => Some(index + 1),
        Some(b'\r') if bytes.get(index + 1) == Some(&b'\n') => Some(index + 2),
        Some(b'\r') => Some(index + 1),
        _ => None,
    }
}

impl StreamAccumulator {
    fn new() -> Self {
        Self {
            id: None,
            model: None,
            stop_reason: None,
            blocks: BTreeMap::new(),
            usage: Map::new(),
            event_count: 0,
            event_types: BTreeMap::new(),
            message_started: false,
            message_delta_seen: false,
            message_stopped: false,
        }
    }

    fn apply(&mut self, event_type: &str, value: &Value) -> Result<(), DriverError> {
        if self.message_stopped {
            return Err(DriverError::Protocol(format!(
                "anthropic_messages stream event '{event_type}' appears after message_stop"
            )));
        }
        self.event_count += 1;
        *self.event_types.entry(event_type.to_string()).or_insert(0) += 1;
        if event_type != "message_start" && event_type != "error" && !self.message_started {
            return Err(DriverError::Protocol(format!(
                "anthropic_messages stream event '{event_type}' appears before message_start"
            )));
        }
        match event_type {
            "ping" => Ok(()),
            "error" => Err(DriverError::Protocol(
                "anthropic_messages stream reported an error event".to_string(),
            )),
            "message_start" => {
                if self.message_started {
                    return Err(DriverError::Protocol(
                        "anthropic_messages stream repeats message_start".to_string(),
                    ));
                }
                let message = value
                    .get("message")
                    .filter(|value| value.is_object())
                    .ok_or_else(|| {
                        DriverError::Protocol("message_start has no message object".to_string())
                    })?;
                self.id = Some(required_string(message, "id", "message_start.message")?);
                self.model = Some(required_string(message, "model", "message_start.message")?);
                merge_usage(&mut self.usage, message.get("usage"))?;
                self.message_started = true;
                Ok(())
            }
            "content_block_start" => {
                if self.message_delta_seen {
                    return Err(DriverError::Protocol(
                        "content_block_start appears after message_delta".to_string(),
                    ));
                }
                let index = required_index(value)?;
                if index != self.blocks.len() {
                    return Err(DriverError::Protocol(format!(
                        "anthropic_messages stream content block index {index} is not the next contiguous index {}",
                        self.blocks.len()
                    )));
                }
                let block = value
                    .get("content_block")
                    .filter(|value| value.is_object())
                    .cloned()
                    .ok_or_else(|| {
                        DriverError::Protocol(
                            "content_block_start has no content_block object".to_string(),
                        )
                    })?;
                self.blocks.insert(index, StreamBlock::start(block, index)?);
                Ok(())
            }
            "content_block_delta" => {
                if self.message_delta_seen {
                    return Err(DriverError::Protocol(
                        "content_block_delta appears after message_delta".to_string(),
                    ));
                }
                let index = required_index(value)?;
                let delta = value
                    .get("delta")
                    .filter(|value| value.is_object())
                    .ok_or_else(|| {
                        DriverError::Protocol("content_block_delta has no delta object".to_string())
                    })?;
                let delta_type = required_string(delta, "type", "content_block_delta.delta")?;
                let block = self.blocks.get_mut(&index).ok_or_else(|| {
                    DriverError::Protocol(format!(
                        "content_block_delta references unknown index {index}"
                    ))
                })?;
                block.apply_delta(&delta_type, delta, index)?;
                Ok(())
            }
            "content_block_stop" => {
                if self.message_delta_seen {
                    return Err(DriverError::Protocol(
                        "content_block_stop appears after message_delta".to_string(),
                    ));
                }
                let index = required_index(value)?;
                self.blocks
                    .get_mut(&index)
                    .ok_or_else(|| {
                        DriverError::Protocol(format!(
                            "content_block_stop references unknown index {index}"
                        ))
                    })?
                    .close(index)?;
                Ok(())
            }
            "message_delta" => {
                if self.message_delta_seen {
                    return Err(DriverError::Protocol(
                        "anthropic_messages stream repeats message_delta".to_string(),
                    ));
                }
                if self.blocks.values().any(|block| !block.closed) {
                    return Err(DriverError::Protocol(
                        "message_delta appears before every content block is closed".to_string(),
                    ));
                }
                let delta = value
                    .get("delta")
                    .filter(|value| value.is_object())
                    .ok_or_else(|| {
                        DriverError::Protocol("message_delta has no delta object".to_string())
                    })?;
                self.stop_reason = Some(required_string(
                    delta,
                    "stop_reason",
                    "message_delta.delta",
                )?);
                merge_usage(&mut self.usage, value.get("usage"))?;
                self.message_delta_seen = true;
                Ok(())
            }
            "message_stop" => {
                if !self.message_delta_seen {
                    return Err(DriverError::Protocol(
                        "message_stop appears before terminal message_delta".to_string(),
                    ));
                }
                if self.blocks.values().any(|block| !block.closed) {
                    return Err(DriverError::Protocol(
                        "message_stop appears before every content block is closed".to_string(),
                    ));
                }
                self.message_stopped = true;
                Ok(())
            }
            other => Err(DriverError::Protocol(format!(
                "anthropic_messages stream event type '{other}' is unsupported"
            ))),
        }
    }

    fn finish(self) -> Result<ProviderResponse, DriverError> {
        if !self.message_started || !self.message_stopped {
            return Err(DriverError::Protocol(
                "anthropic_messages stream did not complete message_start/message_stop".to_string(),
            ));
        }
        if !self.message_delta_seen {
            return Err(DriverError::Protocol(
                "anthropic_messages stream ended without terminal message_delta".to_string(),
            ));
        }
        if self.blocks.values().any(|block| !block.closed) {
            return Err(DriverError::Protocol(
                "anthropic_messages stream ended with open content blocks".to_string(),
            ));
        }
        let mut content = Vec::with_capacity(self.blocks.len());
        for (expected, (index, block)) in self.blocks.into_iter().enumerate() {
            if index != expected {
                return Err(DriverError::Protocol(format!(
                    "anthropic_messages stream content indexes are not contiguous at {expected}"
                )));
            }
            content.push(block.value);
        }
        if content.is_empty() && self.stop_reason.is_none() {
            return Err(DriverError::Protocol(
                "anthropic_messages stream produced neither content nor stop_reason".to_string(),
            ));
        }
        Ok(ProviderResponse {
            id: self.id.ok_or_else(|| {
                DriverError::Protocol("anthropic_messages stream has no message id".to_string())
            })?,
            model: self.model.ok_or_else(|| {
                DriverError::Protocol("anthropic_messages stream has no model".to_string())
            })?,
            stop_reason: self.stop_reason,
            content,
            usage: Value::Object(self.usage),
            transport: TransportEvidence {
                streamed: true,
                status: 200,
                response_bytes: 0,
                response_blake3: String::new(),
                event_count: self.event_count,
                event_types: self.event_types,
            },
        })
    }
}

impl StreamBlock {
    fn start(value: Value, index: usize) -> Result<Self, DriverError> {
        let block_type = required_string(&value, "type", "content_block_start.content_block")?;
        let kind = match block_type.as_str() {
            "text" => {
                string_field(&value, "text", "content_block_start text block")?;
                StreamBlockKind::Text
            }
            "thinking" => {
                string_field(&value, "thinking", "content_block_start thinking block")?;
                if value.get("signature").is_some() {
                    string_field(&value, "signature", "content_block_start thinking block")?;
                }
                StreamBlockKind::Thinking
            }
            "redacted_thinking" => {
                required_string(
                    &value,
                    "data",
                    "content_block_start redacted_thinking block",
                )?;
                StreamBlockKind::RedactedThinking
            }
            "tool_use" => {
                required_string(&value, "id", "content_block_start tool_use block")?;
                required_string(&value, "name", "content_block_start tool_use block")?;
                if !value.get("input").is_some_and(Value::is_object) {
                    return Err(DriverError::Protocol(format!(
                        "content_block_start tool_use block at index {index} has no input object"
                    )));
                }
                StreamBlockKind::ToolUse
            }
            other => {
                return Err(DriverError::Protocol(format!(
                    "anthropic_messages stream content block type '{other}' is unsupported"
                )));
            }
        };
        Ok(Self {
            kind,
            value,
            partial_tool_input: String::new(),
            closed: false,
        })
    }

    fn apply_delta(
        &mut self,
        delta_type: &str,
        delta: &Value,
        index: usize,
    ) -> Result<(), DriverError> {
        if self.closed {
            return Err(DriverError::Protocol(format!(
                "content_block_delta references closed index {index}"
            )));
        }
        match (self.kind, delta_type) {
            (StreamBlockKind::Text, "text_delta") => {
                append_block_string(&mut self.value, "text", delta, "text")
            }
            (StreamBlockKind::Thinking, "thinking_delta") => {
                append_block_string(&mut self.value, "thinking", delta, "thinking")
            }
            (StreamBlockKind::Thinking, "signature_delta") => {
                append_block_string(&mut self.value, "signature", delta, "signature")
            }
            (StreamBlockKind::ToolUse, "input_json_delta") => {
                let partial = delta
                    .get("partial_json")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        DriverError::Protocol(
                            "input_json_delta has no partial_json string".to_string(),
                        )
                    })?;
                self.partial_tool_input.push_str(partial);
                Ok(())
            }
            (_, other) => Err(DriverError::Protocol(format!(
                "anthropic_messages stream delta type '{other}' is incompatible with content block index {index}"
            ))),
        }
    }

    fn close(&mut self, index: usize) -> Result<(), DriverError> {
        if self.closed {
            return Err(DriverError::Protocol(format!(
                "content_block_stop repeats closed index {index}"
            )));
        }
        match self.kind {
            StreamBlockKind::Text => {
                required_string(&self.value, "text", "completed text block")?;
            }
            StreamBlockKind::Thinking => {
                required_string(&self.value, "thinking", "completed thinking block")?;
                if self.value.get("signature").is_some() {
                    required_string(&self.value, "signature", "completed thinking block")?;
                }
            }
            StreamBlockKind::RedactedThinking => {
                required_string(&self.value, "data", "completed redacted_thinking block")?;
            }
            StreamBlockKind::ToolUse => {
                if !self.partial_tool_input.is_empty() {
                    let input: Value = serde_json::from_str(&self.partial_tool_input).map_err(
                        |error| {
                            DriverError::Protocol(format!(
                                "anthropic_messages tool input at index {index} is invalid JSON: {error}"
                            ))
                        },
                    )?;
                    if !input.is_object() {
                        return Err(DriverError::Protocol(format!(
                            "anthropic_messages tool input at index {index} is not an object"
                        )));
                    }
                    self.value["input"] = input;
                } else if !self.value.get("input").is_some_and(Value::is_object) {
                    return Err(DriverError::Protocol(format!(
                        "anthropic_messages tool input at index {index} is not an object"
                    )));
                }
            }
        }
        self.closed = true;
        Ok(())
    }
}

fn string_field<'a>(value: &'a Value, key: &str, surface: &str) -> Result<&'a str, DriverError> {
    value.get(key).and_then(Value::as_str).ok_or_else(|| {
        DriverError::Protocol(format!(
            "anthropic_messages {surface} has no string '{key}'"
        ))
    })
}

fn required_index(value: &Value) -> Result<usize, DriverError> {
    value
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .ok_or_else(|| DriverError::Protocol("SSE content event has invalid index".to_string()))
}

fn append_block_string(
    block: &mut Value,
    target: &str,
    delta: &Value,
    source: &str,
) -> Result<(), DriverError> {
    let fragment = delta
        .get(source)
        .and_then(Value::as_str)
        .ok_or_else(|| DriverError::Protocol(format!("SSE delta has no '{source}' string")))?;
    match block.get_mut(target) {
        Some(Value::String(current)) => current.push_str(fragment),
        Some(_) => {
            return Err(DriverError::Protocol(format!(
                "SSE block field '{target}' is not a string"
            )));
        }
        None => block[target] = Value::String(fragment.to_string()),
    }
    Ok(())
}

fn merge_usage(target: &mut Map<String, Value>, usage: Option<&Value>) -> Result<(), DriverError> {
    let Some(usage) = usage else {
        return Ok(());
    };
    let usage = usage.as_object().ok_or_else(|| {
        DriverError::Protocol("anthropic_messages stream usage is not an object".to_string())
    })?;
    for (key, value) in usage {
        target.insert(key.clone(), value.clone());
    }
    Ok(())
}

fn translate_response(
    accepted: AcceptedProviderResponse,
    output_root: &Path,
    allowed: &BTreeSet<String>,
    model_visible_tool_names: &BTreeSet<String>,
    tool_name_prefix: Option<&str>,
) -> Result<Vec<ProviderEmission>, DriverError> {
    let AcceptedProviderResponse { response, complete } = accepted;
    let nonreplayable_terminal = !complete;
    let mut tool_calls = Vec::new();
    let mut suppressed_tool_uses = Vec::new();
    let mut text_parts = Vec::new();
    let mut reasoning_blocks = Vec::new();
    for (index, block) in response.content.iter().enumerate() {
        let block_type = block.get("type").and_then(Value::as_str).ok_or_else(|| {
            DriverError::Protocol(format!(
                "anthropic_messages response content[{index}] has no type"
            ))
        })?;
        if !allowed.contains(block_type) {
            return Err(DriverError::Protocol(format!(
                "anthropic_messages response content[{index}] type '{block_type}' is not declared by response_content_types"
            )));
        }
        match block_type {
            "text" => text_parts.push(required_string(block, "text", "response text block")?),
            "thinking" => {
                required_string(block, "thinking", "response thinking block")?;
                reasoning_blocks.push(block.clone());
            }
            "redacted_thinking" => {
                required_string(block, "data", "response redacted_thinking block")?;
                reasoning_blocks.push(block.clone());
            }
            "tool_use" => {
                let id = required_string(block, "id", "response tool_use block")?;
                let encoded_name = required_string(block, "name", "response tool_use block")?;
                let input = block
                    .get("input")
                    .filter(|value| value.is_object())
                    .cloned()
                    .ok_or_else(|| {
                        DriverError::Protocol(format!(
                            "anthropic_messages response tool_use[{index}] input is not an object"
                        ))
                    })?;
                if complete {
                    let tool = decode_tool_name(&encoded_name, tool_name_prefix)?;
                    if !model_visible_tool_names.contains(&tool) {
                        return Err(DriverError::Protocol(format!(
                            "anthropic_messages returned tool '{tool}' outside the model-visible affordance set"
                        )));
                    }
                    tool_calls.push((id, tool, input));
                } else {
                    suppressed_tool_uses.push(block.clone());
                }
            }
            other => {
                return Err(DriverError::Protocol(format!(
                    "anthropic_messages response block type '{other}' is unsupported"
                )));
            }
        }
    }
    let transport_payload = json!({
        "label": TRANSPORT_EVIDENCE_LABEL,
        "response_id": response.id,
        "model": response.model,
        "stop_reason": response.stop_reason,
        "complete": complete,
        "nonreplayable_terminal": nonreplayable_terminal,
        "streamed": response.transport.streamed,
        "status": response.transport.status,
        "response_bytes": response.transport.response_bytes,
        "response_blake3": response.transport.response_blake3,
        "event_count": response.transport.event_count,
        "event_types": response.transport.event_types,
    });
    let mut emissions = vec![materialize_emission(
        output_root,
        TRANSPORT_EVIDENCE_LABEL,
        transport_payload,
        ProviderEmissionRoute::ClientProjection,
    )?];
    if response
        .usage
        .as_object()
        .is_some_and(|usage| !usage.is_empty())
    {
        emissions.push(materialize_emission(
            output_root,
            USAGE_EVIDENCE_LABEL,
            json!({
                "label": USAGE_EVIDENCE_LABEL,
                "response_id": response.id,
                "model": response.model,
                "usage": response.usage,
                "complete": complete,
                "nonreplayable_terminal": nonreplayable_terminal,
            }),
            ProviderEmissionRoute::ClientProjection,
        )?);
    }
    if !reasoning_blocks.is_empty() {
        emissions.push(materialize_emission(
            output_root,
            REASONING_EVIDENCE_LABEL,
            json!({
                "label": REASONING_EVIDENCE_LABEL,
                "response_id": response.id,
                "model": response.model,
                "blocks": reasoning_blocks,
                "complete": complete,
                "nonreplayable_terminal": nonreplayable_terminal,
            }),
            ProviderEmissionRoute::ClientProjection,
        )?);
    }
    if !text_parts.is_empty() || !tool_calls.is_empty() || nonreplayable_terminal {
        let content_blocks = if complete {
            response.content
        } else {
            response
                .content
                .into_iter()
                .filter(|block| block.get("type").and_then(Value::as_str) != Some("tool_use"))
                .collect()
        };
        emissions.push(materialize_emission(
            output_root,
            INTERFACE_ARTIFACT_OUTPUT_LABEL,
            json!({
                "label": INTERFACE_ARTIFACT_OUTPUT_LABEL,
                "response_id": response.id,
                "model": response.model,
                "stop_reason": response.stop_reason,
                "complete": complete,
                "nonreplayable_terminal": nonreplayable_terminal,
                "text": if text_parts.is_empty() { Value::Null } else { Value::String(text_parts.join("\n")) },
                "content_blocks": content_blocks,
                "suppressed_tool_uses": if complete { Value::Null } else { Value::Array(suppressed_tool_uses) },
            }),
            ProviderEmissionRoute::ClientProjection,
        )?);
    }
    for (correlation_id, tool, arguments) in tool_calls {
        emissions.push(materialize_emission(
            output_root,
            TOOL_CALL_LABEL,
            json!({
                "label": TOOL_CALL_LABEL,
                "tool": tool,
                "arguments": arguments,
                "correlation_id": correlation_id,
            }),
            ProviderEmissionRoute::Tool { tool },
        )?);
    }
    Ok(emissions)
}

fn materialize_emission(
    output_root: &Path,
    label: &str,
    payload: Value,
    route: ProviderEmissionRoute,
) -> Result<ProviderEmission, DriverError> {
    let bytes = serde_json::to_vec(&payload).map_err(|error| {
        DriverError::Protocol(format!("anthropic_messages output did not encode: {error}"))
    })?;
    let artifact = InterfaceArtifactFile::write(output_root, label, &bytes)
        .map_err(|error| DriverError::Protocol(error.to_string()))?;
    Ok(ProviderEmission { artifact, route })
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_descriptors::ArtifactId;
    use octopus_interfaces::ArtifactBindingPayload;
    use octopus_ledger::ContentHash;

    fn descriptor(path: &Path) -> InterfaceDescriptor {
        let bytes = std::fs::read(path).expect("descriptor reads");
        octopus_descriptors::parse_strict(&bytes).expect("descriptor parses strictly")
    }

    fn source(id: &str) -> ArtifactRef {
        ArtifactRef {
            id: ArtifactId::new(id),
            content: ContentHash::of_bytes(id.as_bytes()),
        }
    }

    #[test]
    fn model_catalog_is_intersected_with_formed_policy() {
        let data = vec![json!({"id": "model-a"}), json!({"id": "model-b"})];
        assert_eq!(
            filter_model_catalog(&data, &BTreeSet::from(["model-b".to_string()])),
            vec![json!({"id": "model-b"})]
        );
    }

    #[test]
    fn oversized_response_header_is_terminal() {
        assert!(matches!(
            validate_response_content_length(Some(1025), 1024),
            Err(DriverError::FailedClosed(_))
        ));
        assert!(validate_response_content_length(Some(1024), 1024).is_ok());
        assert!(validate_response_content_length(None, 1024).is_ok());
    }

    fn context_artifact(
        id: &str,
        label: &str,
        value: Value,
        current: bool,
        movement: u64,
    ) -> ContextArtifact {
        ContextArtifact {
            id: id.to_string(),
            label: Some(label.to_string()),
            value,
            current,
            source_artifact: source(id),
            source_movement_seq: Some(movement),
        }
    }

    fn two_call_context(first_current: bool) -> Vec<ContextArtifact> {
        vec![
            context_artifact(
                "context",
                CONTEXT_REFERENCE_LABEL,
                json!({"label": CONTEXT_REFERENCE_LABEL, "text": "system"}),
                false,
                1,
            ),
            context_artifact(
                "response",
                INTERFACE_ARTIFACT_OUTPUT_LABEL,
                json!({
                    "label": INTERFACE_ARTIFACT_OUTPUT_LABEL,
                    "response_id": "msg-tools",
                    "model": "m",
                    "stop_reason": "tool_use",
                    "content_blocks": [
                        {"type": "tool_use", "id": "call-a", "name": "artifact-dread", "input": {"id": "a"}},
                        {"type": "tool_use", "id": "call-b", "name": "file-dwrite", "input": {"path": "b"}}
                    ]
                }),
                false,
                2,
            ),
            context_artifact(
                "call-a",
                TOOL_CALL_LABEL,
                json!({
                    "label": TOOL_CALL_LABEL,
                    "tool": "artifact.read",
                    "arguments": {"id": "a"},
                    "correlation_id": "call-a"
                }),
                false,
                3,
            ),
            context_artifact(
                "call-b",
                TOOL_CALL_LABEL,
                json!({
                    "label": TOOL_CALL_LABEL,
                    "tool": "file.write",
                    "arguments": {"path": "b"},
                    "correlation_id": "call-b"
                }),
                false,
                4,
            ),
            context_artifact(
                "result-a",
                "artifact.read.result",
                json!({
                    "label": "artifact.read.result",
                    "tool": "artifact.read",
                    "correlation_id": "call-a",
                    "ok": true,
                    "body": {"value": "a"}
                }),
                first_current,
                5,
            ),
        ]
    }

    #[test]
    fn parallel_tool_results_rearm_then_form_one_canonical_user_turn() {
        let provider = InterfaceId::new("world_provider_anthropic");
        let run = RunId::new("run-anthropic");
        let mut first = two_call_context(true);
        let mut pending = match plan_tool_result_fan_in(&first, &provider, &run, None).unwrap() {
            AnthropicFanInPlan::Pending(state) => state,
            AnthropicFanInPlan::Ready => panic!("one of two results cannot invoke the provider"),
        };
        assert_eq!(pending.received.len(), 1);
        pending.group.source_movement_seq = None;
        first.last_mut().unwrap().current = false;
        first.push(context_artifact(
            "pending",
            TOOL_RESULTS_PENDING_LABEL,
            serde_json::to_value(PendingToolGroupArtifact {
                label: TOOL_RESULTS_PENDING_LABEL.to_string(),
                state: pending,
                session_context: None,
            })
            .unwrap(),
            false,
            6,
        ));
        first.push(context_artifact(
            "result-b",
            "file.write.result",
            json!({
                "label": "file.write.result",
                "tool": "file.write",
                "correlation_id": "call-b",
                "ok": true,
                "body": {"value": "b"}
            }),
            true,
            7,
        ));
        assert!(matches!(
            plan_tool_result_fan_in(&first, &provider, &run, None).unwrap(),
            AnthropicFanInPlan::Ready
        ));
        let output = tempfile::tempdir().unwrap();
        let artifact = InterfaceArtifactFile::write(
            output.path(),
            "file.write.result",
            &serde_json::to_vec(&first.last().unwrap().value).unwrap(),
        )
        .unwrap();
        let envelope = InterfaceInvocationEnvelope {
            artifact: source("result-b"),
            artifact_file: artifact,
            artifact_binding_payloads: Vec::new(),
            metadata: None,
        };
        let allowed = ["text", "image", "document"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let assembled =
            assemble_turn(&envelope, &first, &allowed, ThinkingReplay::Drop, &[], None).unwrap();
        let results = assembled
            .messages
            .last()
            .unwrap()
            .get("content")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["tool_use_id"], "call-a");
        assert_eq!(results[1]["tool_use_id"], "call-b");
    }

    #[test]
    fn parallel_tool_result_frontier_rejects_duplicate_unknown_and_mixed_groups() {
        let provider = InterfaceId::new("world_provider_anthropic");
        let run = RunId::new("run-anthropic");
        let mut duplicate = two_call_context(false);
        let mut repeated = duplicate.last().unwrap().value.clone();
        repeated["body"] = json!({"value": "different"});
        duplicate.push(context_artifact(
            "result-a-duplicate",
            "artifact.read.result",
            repeated,
            true,
            6,
        ));
        assert!(plan_tool_result_fan_in(&duplicate, &provider, &run, None)
            .unwrap_err()
            .to_string()
            .contains("repeats tool result"));

        let mut unknown = two_call_context(false);
        unknown.push(context_artifact(
            "result-unknown",
            "artifact.read.result",
            json!({
                "label": "artifact.read.result",
                "tool": "artifact.read",
                "correlation_id": "call-unknown",
                "ok": true,
                "body": {}
            }),
            true,
            6,
        ));
        assert!(plan_tool_result_fan_in(&unknown, &provider, &run, None)
            .unwrap_err()
            .to_string()
            .contains("no native Anthropic tool_use group"));

        let mut mixed = two_call_context(false);
        mixed.extend([
            context_artifact(
                "response-other",
                INTERFACE_ARTIFACT_OUTPUT_LABEL,
                json!({
                    "label": INTERFACE_ARTIFACT_OUTPUT_LABEL,
                    "response_id": "msg-other",
                    "content_blocks": [
                        {"type": "tool_use", "id": "call-c", "name": "artifact-dread", "input": {"id": "c"}},
                        {"type": "tool_use", "id": "call-d", "name": "artifact-dread", "input": {"id": "d"}}
                    ]
                }),
                false,
                6,
            ),
            context_artifact(
                "call-c",
                TOOL_CALL_LABEL,
                json!({"label": TOOL_CALL_LABEL, "tool": "artifact.read", "arguments": {"id": "c"}, "correlation_id": "call-c"}),
                false,
                7,
            ),
            context_artifact(
                "call-d",
                TOOL_CALL_LABEL,
                json!({"label": TOOL_CALL_LABEL, "tool": "artifact.read", "arguments": {"id": "d"}, "correlation_id": "call-d"}),
                false,
                8,
            ),
            context_artifact(
                "result-c",
                "artifact.read.result",
                json!({"label": "artifact.read.result", "tool": "artifact.read", "correlation_id": "call-c", "ok": true, "body": {}}),
                true,
                9,
            ),
        ]);
        assert!(plan_tool_result_fan_in(&mixed, &provider, &run, None)
            .unwrap_err()
            .to_string()
            .contains("mixed with unresolved groups"));
    }

    #[test]
    fn session_context_v2_requires_exact_provider_and_provenance() {
        let root = tempfile::tempdir().unwrap();
        let current_value = json!({"prompt": "hello"});
        let current_file = InterfaceArtifactFile::write(
            root.path(),
            "operator.input",
            &serde_json::to_vec(&current_value).unwrap(),
        )
        .unwrap();
        let provider = InterfaceId::new("world_provider_anthropic");
        let bound_source = source("bound-context");
        let snapshot = |provider_interface: &str| {
            json!({
                "label": INTERFACE_ARTIFACT_OUTPUT_LABEL,
                "text": "assistant output",
                "session_context": {
                    "schema_version": 2,
                    "provider_interface": provider_interface,
                    "artifacts": [{
                        "label": CONTEXT_REFERENCE_LABEL,
                        "payload": {"label": CONTEXT_REFERENCE_LABEL, "text": "system"},
                        "source_artifact": bound_source.clone(),
                        "source_movement_seq": 4
                    }]
                }
            })
        };
        let envelope = |snapshot: Value| InterfaceInvocationEnvelope {
            artifact: source("current"),
            artifact_file: current_file.clone(),
            artifact_binding_payloads: vec![ArtifactBindingPayload::new(
                source("snapshot"),
                serde_json::to_vec(&snapshot).unwrap(),
            )],
            metadata: None,
        };
        let valid = envelope(snapshot(provider.as_str()));
        let artifacts = context_artifacts(&valid, &current_value, &provider, 5).unwrap();
        assert_eq!(artifacts.len(), 3);
        assert_eq!(
            artifacts[1].label.as_deref(),
            Some(INTERFACE_ARTIFACT_OUTPUT_LABEL)
        );
        assert_eq!(artifacts[1].source_artifact, source("snapshot"));
        assert_eq!(artifacts[1].source_movement_seq, None);
        assert!(artifacts[1].value.get(SESSION_CONTEXT_FIELD).is_none());
        assert_eq!(artifacts[2].source_movement_seq, Some(5));
        let foreign = envelope(snapshot("world_provider_foreign"));
        assert!(context_artifacts(&foreign, &current_value, &provider, 5)
            .unwrap_err()
            .to_string()
            .contains("belongs to provider"));
    }

    #[test]
    fn stream_preserves_interleaved_thinking_and_parallel_tools() {
        let body = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-1\",\"model\":\"m\",\"usage\":{\"input_tokens\":3}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"plan\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"artifact-dread\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"id\\\":1}\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t2\",\"name\":\"file-dwrite\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"x\\\"}\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":9}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
        );
        let mut decoder = AnthropicSseDecoder::new();
        for chunk in body.as_bytes().chunks(17) {
            decoder.push(chunk).expect("stream chunk parses");
        }
        let parsed = decoder.finish().expect("stream parses");
        assert_eq!(parsed.content.len(), 3);
        assert_eq!(parsed.content[0]["thinking"], "plan");
        assert_eq!(parsed.content[0]["signature"], "sig");
        assert_eq!(parsed.content[1]["input"], json!({"id": 1}));
        assert_eq!(parsed.content[2]["input"], json!({"path": "x"}));
        assert_eq!(parsed.usage["input_tokens"], 3);
        assert_eq!(parsed.usage["output_tokens"], 9);
    }

    #[test]
    fn stream_state_machine_rejects_closed_or_terminal_reentry() {
        let start = json!({
            "type": "message_start",
            "message": {"id": "msg-1", "model": "m", "usage": {}}
        });
        let block_start = json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": "ready"}
        });
        let block_stop = json!({"type": "content_block_stop", "index": 0});
        let text_delta = json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": " late"}
        });

        let mut closed = StreamAccumulator::new();
        closed.apply("message_start", &start).unwrap();
        closed.apply("content_block_start", &block_start).unwrap();
        closed.apply("content_block_stop", &block_stop).unwrap();
        assert!(closed
            .apply("content_block_delta", &text_delta)
            .unwrap_err()
            .to_string()
            .contains("closed index"));
        assert!(closed
            .apply("content_block_stop", &block_stop)
            .unwrap_err()
            .to_string()
            .contains("repeats closed"));

        let mut open = StreamAccumulator::new();
        open.apply("message_start", &start).unwrap();
        open.apply("content_block_start", &block_start).unwrap();
        assert!(open
            .apply(
                "message_delta",
                &json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "end_turn"},
                    "usage": {}
                })
            )
            .unwrap_err()
            .to_string()
            .contains("before every content block is closed"));

        let mut terminal = StreamAccumulator::new();
        terminal.apply("message_start", &start).unwrap();
        terminal.apply("content_block_start", &block_start).unwrap();
        terminal.apply("content_block_stop", &block_stop).unwrap();
        terminal
            .apply(
                "message_delta",
                &json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "end_turn"},
                    "usage": {}
                }),
            )
            .unwrap();
        terminal
            .apply("message_stop", &json!({"type": "message_stop"}))
            .unwrap();
        assert!(terminal
            .apply("ping", &json!({"type": "ping"}))
            .unwrap_err()
            .to_string()
            .contains("after message_stop"));
    }

    #[test]
    fn thinking_only_response_is_evidence_without_wire_transcript_output() {
        let output = tempfile::tempdir().unwrap();
        let response = ProviderResponse {
            id: "msg-reasoning".to_string(),
            model: "m".to_string(),
            stop_reason: Some("end_turn".to_string()),
            content: vec![json!({
                "type": "thinking",
                "thinking": "private reasoning",
                "signature": "sig"
            })],
            usage: json!({}),
            transport: TransportEvidence {
                streamed: false,
                status: 200,
                response_bytes: 1,
                response_blake3: "hash".to_string(),
                event_count: 0,
                event_types: BTreeMap::new(),
            },
        };
        let allowed = ["thinking".to_string()].into_iter().collect();
        let emissions = translate_response(
            AcceptedProviderResponse {
                response,
                complete: true,
            },
            output.path(),
            &allowed,
            &BTreeSet::new(),
            None,
        )
        .unwrap();
        let labels = emissions
            .iter()
            .map(|emission| emission.artifact.read_manifest().unwrap().label)
            .collect::<BTreeSet<_>>();
        assert!(labels.contains(REASONING_EVIDENCE_LABEL));
        assert!(!labels.contains(INTERFACE_ARTIFACT_OUTPUT_LABEL));

        let replay = assistant_blocks(
            &json!({"content_blocks": [{
                "type": "thinking",
                "thinking": "private reasoning",
                "signature": "sig"
            }]}),
            &allowed,
            ThinkingReplay::Verbatim,
        )
        .unwrap();
        assert!(replay.is_empty());
    }

    #[test]
    fn exact_model_profiles_select_anthropic_limits_and_modalities() {
        let interface_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let concrete =
            descriptor(&interface_root.join("world_provider_opencode_go_anthropic/interface.json"));
        let secrets_home = tempfile::tempdir().unwrap();
        let driver =
            AnthropicMessagesDriver::from_descriptor(&concrete, secrets_home.path().to_path_buf())
                .unwrap();
        let minimax = driver.options.for_provider_model("minimax-m3").unwrap();
        let qwen = driver.options.for_provider_model("qwen3.7-max").unwrap();
        assert_eq!(minimax.max_tokens, 131_072);
        assert!(minimax
            .input_content_types
            .iter()
            .any(|content_type| content_type == "image"));
        assert_eq!(qwen.max_tokens, 65_536);
        assert!(!qwen
            .input_content_types
            .iter()
            .any(|content_type| content_type == "image"));
        assert!(minimax.model_profiles.is_empty());
        assert!(qwen.model_profiles.is_empty());
        assert!(driver.options.for_provider_model("unformed-model").is_err());
    }

    #[test]
    fn max_tokens_materializes_incomplete_evidence_without_executing_tools() {
        let interface_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let concrete = descriptor(&interface_root.join("world_provider_anthropic/interface.json"));
        let secrets_home = tempfile::tempdir().unwrap();
        let driver =
            AnthropicMessagesDriver::from_descriptor(&concrete, secrets_home.path().to_path_buf())
                .unwrap();
        let accepted = driver
            .accept_response(ProviderResponse {
                id: "msg-partial".to_string(),
                model: "m".to_string(),
                stop_reason: Some("max_tokens".to_string()),
                content: vec![
                    json!({"type": "thinking", "thinking": "partial reasoning"}),
                    json!({"type": "text", "text": "partial answer"}),
                    json!({"type": "tool_use", "id": "partial-call", "name": "not-granted", "input": {"path": "x"}}),
                ],
                usage: json!({"output_tokens": 64000}),
                transport: TransportEvidence {
                    streamed: true,
                    status: 200,
                    response_bytes: 42,
                    response_blake3: "body-hash".to_string(),
                    event_count: 7,
                    event_types: BTreeMap::new(),
                },
            })
            .unwrap();
        assert!(!accepted.complete);
        let output = tempfile::tempdir().unwrap();
        let allowed = ["text", "thinking", "tool_use"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let emissions =
            translate_response(accepted, output.path(), &allowed, &BTreeSet::new(), None).unwrap();
        let mut labels = BTreeSet::new();
        for emission in emissions {
            let manifest = emission.artifact.read_manifest().unwrap();
            labels.insert(manifest.label.clone());
            let payload: Value =
                serde_json::from_slice(&emission.artifact.read_payload_bytes().unwrap()).unwrap();
            assert_eq!(payload.get("complete"), Some(&json!(false)));
            assert_eq!(payload.get("nonreplayable_terminal"), Some(&json!(true)));
            if manifest.label == INTERFACE_ARTIFACT_OUTPUT_LABEL {
                assert_eq!(payload["text"], "partial answer");
                assert_eq!(payload["suppressed_tool_uses"].as_array().unwrap().len(), 1);
                assert!(payload["content_blocks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|block| block["type"] != "tool_use"));
            }
        }
        assert!(labels.contains(TRANSPORT_EVIDENCE_LABEL));
        assert!(labels.contains(USAGE_EVIDENCE_LABEL));
        assert!(labels.contains(REASONING_EVIDENCE_LABEL));
        assert!(labels.contains(INTERFACE_ARTIFACT_OUTPUT_LABEL));
        assert!(!labels.contains(TOOL_CALL_LABEL));
    }

    #[test]
    fn replay_policy_is_descriptor_data() {
        let payload = json!({
            "content_blocks": [
                {"type": "thinking", "thinking": "signed", "signature": "sig"},
                {"type": "thinking", "thinking": "unsigned"},
                {"type": "tool_use", "id": "t", "name": "artifact-dread", "input": {}}
            ]
        });
        let allowed = ["text", "thinking", "redacted_thinking", "tool_use"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let verbatim = assistant_blocks(&payload, &allowed, ThinkingReplay::Verbatim).unwrap();
        let unsigned = assistant_blocks(&payload, &allowed, ThinkingReplay::UnsignedOnly).unwrap();
        assert_eq!(verbatim.len(), 3);
        assert_eq!(unsigned.len(), 2);
        assert_eq!(unsigned[0]["thinking"], "unsigned");
    }

    #[test]
    fn tool_name_encoding_is_reversible() {
        for name in ["artifact.read", "file.write", "custom-tool"] {
            assert_eq!(
                decode_tool_name(&encode_tool_name(name, None), None).unwrap(),
                name
            );
            assert_eq!(
                decode_tool_name(&encode_tool_name(name, Some("mcp__")), Some("mcp__")).unwrap(),
                name
            );
        }
        assert!(decode_tool_name("artifact-dread", Some("mcp__")).is_err());
    }

    #[test]
    fn auth_source_is_exactly_one_declared_surface() {
        let descriptor_with_auth = |auth: Value, process_env_names: Value| {
            let mut transport = json!({
                "base_url": "http://localhost:7777",
                "wire": "anthropic_messages",
                "auth_header": "x-api-key",
                "auth_value_prefix": "",
                "headers": {"anthropic-version": "2023-06-01"},
                "request_timeout_ms": 1000,
                "max_response_bytes": 1024
            });
            transport
                .as_object_mut()
                .expect("transport object")
                .extend(auth.as_object().expect("auth object").clone());
            let value = json!({
                "id": "world_provider_auth_test",
                "interface_type": "world",
                "role": "provider",
                "direction": "bidirectional",
                "source_names": ["auth_test"],
                "interface_driver_ref": "anthropic_messages",
                "interface_driver_config": {
                    "private_config": {
                        "process_env_names": process_env_names,
                        "model": "m",
                        "retry_policy": {
                            "retryable_http_statuses": [429, 500],
                            "backoff_ms": [10, 20, 40, 80]
                        },
                        "finish_policy": {
                            "successful_stop_reasons": ["end_turn", "tool_use"],
                            "nonreplayable_stop_reasons": ["max_tokens"]
                        },
                        "anthropic_messages": {
                            "messages_path": "/v1/messages",
                            "max_tokens": 1,
                            "stream": false,
                            "input_content_types": ["text"],
                            "response_content_types": ["text", "tool_use"],
                            "thinking_replay": "drop",
                            "request_fields": {}
                        }
                    },
                    "transport": transport
                }
            });
            octopus_descriptors::parse_strict::<InterfaceDescriptor>(
                &serde_json::to_vec(&value).unwrap(),
            )
            .expect("auth test descriptor parses")
        };
        let home = tempfile::tempdir().unwrap();
        let env_only = descriptor_with_auth(
            json!({"auth_env_key": "OCTOPUS_ANTHROPIC_TEST_KEY"}),
            json!(["OCTOPUS_ANTHROPIC_TEST_KEY"]),
        );
        AnthropicMessagesDriver::from_descriptor(&env_only, home.path().to_path_buf())
            .expect("env-only auth resolves");
        let ambiguous = descriptor_with_auth(
            json!({
                "auth_env_key": "OCTOPUS_ANTHROPIC_TEST_KEY",
                "auth_secret_ref": "secret://auth-test/key"
            }),
            json!(["OCTOPUS_ANTHROPIC_TEST_KEY"]),
        );
        let error =
            match AnthropicMessagesDriver::from_descriptor(&ambiguous, home.path().to_path_buf()) {
                Ok(_) => panic!("ambiguous auth must fail closed"),
                Err(error) => error,
            };
        assert!(error.contains("cannot declare both"));
    }

    #[test]
    fn static_secret_headers_are_rejected_as_descriptor_data() {
        for header in [
            "authorization",
            "proxy-authorization",
            "cookie",
            "x-api-key",
            "x-service-access-token",
            "x-client-secret",
        ] {
            let transport: InterfaceTransportConfig = serde_json::from_value(json!({
                "base_url": "https://example.invalid",
                "headers": {
                    (header): "must-not-live-in-a-descriptor",
                    "anthropic-version": "2023-06-01"
                }
            }))
            .unwrap();
            let error = resolve_static_headers(&transport).unwrap_err();
            assert!(error.contains("secret-bearing header"), "{header}: {error}");
        }
    }

    #[test]
    fn anthropic_wire_interfaces_are_self_contained_without_secret_literals() {
        let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let interface_root = crate_root
            .ancestors()
            .nth(3)
            .expect("interface registry root");
        let family = descriptor(&interface_root.join("world_provider_anthropic/interface.json"));
        let family_config = family
            .interface_driver_config
            .as_ref()
            .expect("family driver config");
        assert!(family_config.supported_reasoning_efforts.is_empty());
        assert!(!family_config
            .private_config
            .contains_key("reasoning_effort"));
        assert_eq!(family_config.private_config["model"], "claude-sonnet-4-6");
        assert!(family_config.private_config["context_output_labels"]
            .as_array()
            .unwrap()
            .contains(&Value::String(TOOL_RESULTS_PENDING_LABEL.to_string())));
        assert!(family_config.private_config["session_snapshot_labels"]
            .as_array()
            .unwrap()
            .contains(&Value::String(TOOL_RESULTS_PENDING_LABEL.to_string())));
        let secrets_home = tempfile::tempdir().expect("temporary secrets home");
        let family_driver =
            AnthropicMessagesDriver::from_descriptor(&family, secrets_home.path().to_path_buf())
                .expect("family driver config resolves");
        assert_eq!(
            family_driver.options.request_fields["thinking"]["type"],
            "adaptive"
        );
        assert_eq!(
            family_driver.options.request_fields["thinking"]["display"],
            "summarized"
        );
        assert_eq!(
            family_driver.options.request_fields["output_config"]["effort"],
            "medium"
        );
        assert_eq!(family_driver.options.max_tokens, 64000);
        assert_eq!(family_driver.retry_backoff.len(), 4);
        assert!(family_driver.retryable_http_statuses.contains(&429));
        assert!(family_driver.successful_stop_reasons.contains("tool_use"));
        assert!(family_driver
            .nonreplayable_stop_reasons
            .contains("max_tokens"));
        for entry in std::fs::read_dir(interface_root).expect("interface registry reads") {
            let folder = entry.expect("interface registry entry reads").path();
            let descriptor_path = folder.join("interface.json");
            if !descriptor_path.is_file() {
                continue;
            }
            let bytes = std::fs::read(&descriptor_path).expect("interface descriptor reads");
            let value: Value =
                serde_json::from_slice(&bytes).expect("interface descriptor is JSON");
            if value.get("interface_driver_ref").and_then(Value::as_str)
                != Some(INTERFACE_DRIVER_REF)
            {
                continue;
            }
            let concrete: InterfaceDescriptor =
                octopus_descriptors::parse_strict(&bytes).expect("descriptor parses strictly");
            assert_eq!(
                folder.file_name().and_then(|name| name.to_str()),
                Some(concrete.id.as_str())
            );
            assert!(serde_json::to_value(&concrete)
                .unwrap()
                .get("reference")
                .is_none());
            assert_eq!(concrete.interface_driver_ref, INTERFACE_DRIVER_REF);
            let resolved_config = concrete
                .interface_driver_config
                .as_ref()
                .expect("concrete driver config");
            assert!(resolved_config.supported_reasoning_efforts.is_empty());
            assert!(!resolved_config
                .private_config
                .contains_key("reasoning_effort"));
            assert!(resolved_config.private_config["session_snapshot_labels"]
                .as_array()
                .unwrap()
                .contains(&Value::String(TOOL_RESULTS_PENDING_LABEL.to_string())));
            let transport = resolved_config
                .transport
                .as_ref()
                .expect("resolved sibling transport");
            assert!(transport.auth_env_key.is_none());
            assert!(
                transport
                    .auth_secret_ref
                    .as_deref()
                    .is_some_and(|value| value.starts_with("secret://"))
                    || transport.auth_env_key.is_some()
                    || resolved_config.private_config.contains_key("azure_entra")
            );
            let life_bytes = std::fs::read(folder.join("life.json"))
                .expect("exact Anthropic-wire life descriptor reads");
            let life: octopus_interface_transport::SpawnLifeConfig =
                serde_json::from_slice(&life_bytes).expect("life descriptor parses strictly");
            assert_eq!(life.processes.len(), 1);
            if transport.auth_env_key.is_none()
                && !resolved_config.private_config.contains_key("model_env")
            {
                AnthropicMessagesDriver::from_descriptor(
                    &concrete,
                    secrets_home.path().to_path_buf(),
                )
                .expect("environment-independent concrete driver config resolves");
            }
        }
        let life_bytes = std::fs::read(
            interface_root
                .join("world_provider_anthropic")
                .join("life.json"),
        )
        .expect("life descriptor reads");
        let life: octopus_interface_transport::SpawnLifeConfig =
            serde_json::from_slice(&life_bytes).expect("life descriptor parses strictly");
        assert_eq!(life.processes.len(), 1);
        let process = &life.processes[0];
        assert!(process
            .coordinate_root_slots
            .contains("activation_secrets_home"));
        assert_eq!(
            process.inherit_env_from_interface,
            ["/interface_driver_config/private_config/process_env_names".to_string()]
                .into_iter()
                .collect()
        );
        assert!(process
            .authority
            .read_root_slots
            .contains("activation_secrets_home"));
    }

    #[test]
    fn authenticated_refresh_signal_is_typed_and_separate_from_transport_retry() {
        let failure =
            AnthropicDriverFailure::AuthenticatedRefreshRequired(AuthenticatedRefreshEvidence {
                status: 401,
                response_bytes: 19,
                response_blake3: blake3::hash(b"Bearer secret-value").to_hex().to_string(),
                surface: "messages endpoint",
            });
        assert!(failure.authenticated_refresh_required());
        let AnthropicDriverFailure::AuthenticatedRefreshRequired(evidence) = failure else {
            unreachable!()
        };
        let encoded = serde_json::to_string(&evidence).unwrap();
        assert!(encoded.contains("\"status\":401"));
        assert!(!encoded.contains("secret-value"));
        assert!(!encoded.contains("Bearer"));
    }

    #[test]
    fn cloned_interface_secret_scope_must_be_rebased() {
        let interface_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let mut cloned =
            descriptor(&interface_root.join("world_provider_anthropic/interface.json"));
        cloned.id = InterfaceId::new("world_provider_anthropic_clone");
        let secret_home = tempfile::tempdir().unwrap();
        let error =
            AnthropicMessagesDriver::from_descriptor(&cloned, secret_home.path().to_path_buf())
                .err()
                .unwrap();
        assert!(error.contains("exact interface scope"));
        cloned
            .interface_driver_config
            .as_mut()
            .unwrap()
            .transport
            .as_mut()
            .unwrap()
            .auth_secret_ref =
            Some("secret://world_provider_anthropic_clone/ANTHROPIC_API_KEY".to_string());
        AnthropicMessagesDriver::from_descriptor(&cloned, secret_home.path().to_path_buf())
            .unwrap();
    }
}
