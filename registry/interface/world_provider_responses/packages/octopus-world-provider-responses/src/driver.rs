use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use octopus_azure_auth::{AzureAuthFailure, AzureEntraCredentialManager};
use octopus_descriptors::InterfaceId;
use octopus_interfaces::{
    InterfaceArtifactFile, InterfaceDescriptor, InterfaceInvocationEnvelope,
    InterfaceTransportConfig,
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
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::dispatch::DispatchAdmission;
use crate::websocket::{
    ResponsesWebSocketConfig, ResponsesWebSocketFailure, ResponsesWebSocketTransport,
    WebSocketFailureKind, WebSocketFailureStage,
};

pub const INTERFACE_DRIVER_REF: &str = "openai_responses_native";

const CONTEXT_REFERENCE_LABEL: &str = "context.reference";
const INTERFACE_ARTIFACT_OUTPUT_LABEL: &str = "interface.artifact.output";
const TOOL_CALL_LABEL: &str = "tool.call";
const TURN_ERROR_LABEL: &str = "turn.error";
const TRANSPORT_EVIDENCE_LABEL: &str = "provider.transport.evidence";
const USAGE_EVIDENCE_LABEL: &str = "provider.usage";
const REASONING_EVIDENCE_LABEL: &str = "provider.reasoning";
const NATIVE_TOOL_EVIDENCE_LABEL: &str = "provider.native_tool";
const TOOL_RESULTS_PENDING_LABEL: &str = "provider.tool_results.pending";
const MODELS_LIST_LABEL: &str = "provider.models.list";
const SESSION_CONTEXT_FIELD: &str = "session_context";
const ABSOLUTE_MAX_RESPONSE_BYTES: u64 = 512 * 1024 * 1024;
const ABSOLUTE_MAX_INLINE_FILE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum HttpMethod {
    Post,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ResponseMode {
    Sse,
    Json,
    Websocket,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SequenceNumbers {
    RequiredMonotonic,
    OptionalMonotonic,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ValueSource {
    Descriptor,
    Environment,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum AuthSourceSelection {
    None,
    Secret,
    Environment,
    AzureEntra,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CacheMode {
    StaticPrefixBlake3,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ForeignEncryptedReplay {
    Drop,
    Reject,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationConfig {
    method: HttpMethod,
    path: String,
    response_mode: ResponseMode,
    sequence_numbers: SequenceNumbers,
    #[serde(default)]
    websocket: Option<ResponsesWebSocketConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheConfig {
    mode: CacheMode,
    field: String,
    prefix: String,
    digest_characters: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryConfig {
    backoff_ms: u64,
    max_backoff_ms: u64,
    http_statuses: Vec<u16>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelCatalogConfig {
    path: String,
    request_label: String,
    output_label: String,
    models_pointer: String,
    id_pointer: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FunctionToolsConfig {
    #[serde(default)]
    strict: Option<bool>,
    #[serde(default)]
    tool_choice: Option<Value>,
    #[serde(default)]
    parallel_tool_calls: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayConfig {
    scope: String,
    encrypted_reasoning: bool,
    encrypted_content: bool,
    assistant_messages: bool,
    native_tool_items: bool,
    foreign_encrypted: ForeignEncryptedReplay,
    strip_item_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CarrierPolicy {
    max_inline_file_bytes: u64,
    allowed_roles: Vec<String>,
    allowed_input_item_types: Vec<String>,
    allowed_content_types: Vec<String>,
    allowed_image_details: Vec<String>,
    allowed_file_details: Vec<String>,
    allowed_image_url_schemes: Vec<String>,
    allowed_file_url_schemes: Vec<String>,
    rules: Vec<CarrierRule>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "encoding", rename_all = "snake_case", deny_unknown_fields)]
enum CarrierRule {
    JsonText {
        label: String,
        role: String,
        text_pointer: String,
    },
    ResponsesItems {
        label: String,
        items_pointer: String,
    },
}

impl CarrierRule {
    fn label(&self) -> &str {
        match self {
            Self::JsonText { label, .. } | Self::ResponsesItems { label, .. } => label,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponsesOptions {
    operation: OperationConfig,
    request_fields: Map<String, Value>,
    #[serde(default)]
    cache: Option<CacheConfig>,
    retry: RetryConfig,
    #[serde(default)]
    model_catalog: Option<ModelCatalogConfig>,
    function_tools: FunctionToolsConfig,
    native_tools: Vec<Value>,
    native_tool_types: Vec<String>,
    native_tool_output_types: Vec<String>,
    response_item_types: Vec<String>,
    response_content_types: Vec<String>,
    replay: ReplayConfig,
    #[serde(default)]
    model_profiles: BTreeMap<String, ResponsesModelProfile>,
    carrier_policy: CarrierPolicy,
    evidence_headers: Vec<String>,
    usage_required: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponsesModelProfile {
    #[serde(default)]
    request_fields: Option<Map<String, Value>>,
    #[serde(default)]
    cache: Option<CacheConfig>,
    #[serde(default)]
    replay: Option<ReplayConfig>,
}

#[derive(Clone, Debug)]
struct ResponsesModelOptions {
    request_fields: Map<String, Value>,
    cache: Option<CacheConfig>,
    replay: ReplayConfig,
}

#[derive(Clone)]
enum AuthSource {
    None,
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
    header: Option<HeaderName>,
    value_prefix: String,
    source: AuthSource,
}

pub struct OpenAiResponsesDriver {
    interface_id: String,
    endpoint: reqwest::Url,
    models_endpoint: Option<reqwest::Url>,
    headers: HeaderMap,
    evidence_headers: Vec<HeaderName>,
    auth: ProviderAuth,
    model: String,
    options: ResponsesOptions,
    model_options: Result<ResponsesModelOptions, String>,
    private_config: Map<String, Value>,
    native_tool_output_types: BTreeSet<String>,
    response_item_types: BTreeSet<String>,
    response_content_types: BTreeSet<String>,
    allowed_roles: BTreeSet<String>,
    allowed_input_item_types: BTreeSet<String>,
    allowed_content_types: BTreeSet<String>,
    allowed_image_details: BTreeSet<String>,
    allowed_file_details: BTreeSet<String>,
    allowed_image_url_schemes: BTreeSet<String>,
    allowed_file_url_schemes: BTreeSet<String>,
    strip_item_ids: BTreeSet<String>,
    max_response_bytes: usize,
    client: reqwest::Client,
    websocket: Option<ResponsesWebSocketTransport>,
}

#[derive(Clone, Debug)]
pub struct ResponsesFailure {
    pub class: &'static str,
    pub retryable: bool,
    pub status: Option<u16>,
    pub code: Option<String>,
    pub detail: String,
    pub evidence: Option<Value>,
}

impl ResponsesFailure {
    pub(crate) fn contract(detail: impl Into<String>) -> Self {
        Self {
            class: "contract",
            retryable: false,
            status: None,
            code: None,
            detail: detail.into(),
            evidence: None,
        }
    }
    fn transport(detail: impl Into<String>) -> Self {
        Self {
            class: "transport",
            retryable: true,
            status: None,
            code: None,
            detail: detail.into(),
            evidence: None,
        }
    }

    fn response_limit(detail: impl Into<String>) -> Self {
        Self {
            class: "response_limit",
            retryable: false,
            status: None,
            code: None,
            detail: detail.into(),
            evidence: None,
        }
    }

    fn partial_stream(detail: impl Into<String>, bytes: &[u8]) -> Self {
        Self::partial_stream_evidence(
            detail,
            bytes.len(),
            blake3::hash(bytes).to_hex().to_string(),
        )
    }

    fn partial_stream_evidence(
        detail: impl Into<String>,
        response_bytes: usize,
        response_blake3: String,
    ) -> Self {
        Self {
            class: "interrupted",
            retryable: false,
            status: None,
            code: Some("partial_stream_replay_forbidden".to_string()),
            detail: detail.into(),
            evidence: Some(json!({
                "response_bytes": response_bytes,
                "response_blake3": response_blake3,
            })),
        }
    }

    fn endpoint(status: u16, retryable: bool, headers: Value, bytes: &[u8]) -> Self {
        let parsed = serde_json::from_slice::<Value>(bytes).ok();
        let error = parsed
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(Value::as_object);
        let code = error
            .and_then(|value| value.get("code"))
            .and_then(json_scalar_string);
        let message = error
            .and_then(|value| value.get("message"))
            .and_then(json_scalar_string)
            .unwrap_or_else(|| format!("Responses endpoint returned HTTP {status}"));
        Self {
            class: "endpoint",
            retryable,
            status: Some(status),
            code,
            detail: message,
            evidence: Some(json!({
                "headers": headers,
                "response_bytes": bytes.len(),
                "response_blake3": blake3::hash(bytes).to_hex().to_string(),
                "raw_response": raw_bytes_evidence(bytes),
            })),
        }
    }

    fn response_contract(
        detail: impl Into<String>,
        status: u16,
        headers: Value,
        bytes: &[u8],
    ) -> Self {
        Self::response_contract_evidence(
            detail,
            status,
            headers,
            bytes.len(),
            blake3::hash(bytes).to_hex().to_string(),
            Some(bytes),
        )
    }

    fn response_contract_evidence(
        detail: impl Into<String>,
        status: u16,
        headers: Value,
        response_bytes: usize,
        response_blake3: String,
        raw_response: Option<&[u8]>,
    ) -> Self {
        let mut evidence = json!({
            "headers": headers,
            "response_bytes": response_bytes,
            "response_blake3": response_blake3,
        });
        if let Some(bytes) = raw_response {
            evidence["raw_response"] = raw_bytes_evidence(bytes);
        }
        Self {
            class: "response_contract",
            retryable: false,
            status: Some(status),
            code: None,
            detail: detail.into(),
            evidence: Some(evidence),
        }
    }

    fn response_outcome_evidence(
        code: Option<String>,
        detail: impl Into<String>,
        status: u16,
        headers: Value,
        response_bytes: usize,
        response_blake3: String,
        raw_response: Option<&[u8]>,
    ) -> Self {
        let mut evidence = json!({
            "headers": headers,
            "response_bytes": response_bytes,
            "response_blake3": response_blake3,
        });
        if let Some(bytes) = raw_response {
            evidence["raw_response"] = raw_bytes_evidence(bytes);
        }
        Self {
            class: "response_outcome",
            retryable: false,
            status: Some(status),
            code,
            detail: detail.into(),
            evidence: Some(evidence),
        }
    }

    pub(crate) fn interrupted(detail: impl Into<String>) -> Self {
        Self {
            class: "interrupted",
            retryable: false,
            status: None,
            code: None,
            detail: detail.into(),
            evidence: None,
        }
    }

    pub(crate) fn unavailable(detail: impl Into<String>) -> Self {
        Self {
            class: "unavailable",
            retryable: false,
            status: None,
            code: None,
            detail: detail.into(),
            evidence: None,
        }
    }

    pub(crate) fn exhausted(detail: impl Into<String>, evidence: Option<Value>) -> Self {
        Self {
            class: "transport",
            retryable: false,
            status: None,
            code: None,
            detail: detail.into(),
            evidence,
        }
    }

    fn authenticated_refresh_required(status: u16, headers: Value, bytes: &[u8]) -> Self {
        Self {
            class: "authentication",
            retryable: false,
            status: Some(status),
            code: Some("provider_authentication_refresh_required".to_string()),
            detail:
                "Responses endpoint rejected the descriptor-selected Azure Entra token with HTTP 401"
                    .to_string(),
            evidence: Some(json!({
                "headers": headers,
                "response_bytes": bytes.len(),
                "response_blake3": blake3::hash(bytes).to_hex().to_string(),
            })),
        }
    }

    fn authentication(error: AzureAuthFailure) -> Self {
        let evidence = Some(json!(error.evidence()));
        Self {
            class: error.class,
            retryable: false,
            status: error.status,
            code: Some(error.code.to_string()),
            detail: error.detail,
            evidence,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ResponsesDriverFailure {
    Failure(ResponsesFailure),
    AuthenticatedRefreshRequired(ResponsesFailure),
}

impl ResponsesDriverFailure {
    pub(crate) fn into_failure(self) -> ResponsesFailure {
        match self {
            Self::Failure(failure) | Self::AuthenticatedRefreshRequired(failure) => failure,
        }
    }
}

impl From<ResponsesFailure> for ResponsesDriverFailure {
    fn from(failure: ResponsesFailure) -> Self {
        Self::Failure(failure)
    }
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

struct ContextArtifact {
    id: String,
    label: String,
    value: Value,
    source_artifact: ArtifactRef,
    source_movement_seq: Option<u64>,
    current: bool,
}

struct ToolCatalog {
    declarations: Vec<Value>,
    by_wire_name: BTreeMap<String, String>,
    hidden_tool_names: Vec<String>,
}

#[derive(Debug)]
struct AssembledInput {
    instructions: String,
    items: Vec<Value>,
}

#[derive(Debug)]
enum InputAssembly {
    Ready(AssembledInput),
    Awaiting(PendingToolResults),
}

struct ResponsesEnvelope {
    response: Value,
    request_bytes: Vec<u8>,
    response_evidence: ResponseBodyEvidence,
    status: u16,
    headers: Value,
    stream_events: Vec<Value>,
}

struct ResponseBodyEvidence {
    bytes: usize,
    blake3: String,
    raw: Option<Vec<u8>>,
}

struct ParsedStream {
    response: Value,
    events: Vec<Value>,
    terminal_type: String,
}

struct ReplayGroup {
    first_ordinal: usize,
    items: BTreeMap<usize, Value>,
}

struct ToolCallIdentity {
    response_id: String,
    output_index: usize,
    tool: String,
    call_id: String,
    wire_name: String,
}

struct ToolResultIdentity {
    artifact_ordinal: usize,
    item: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingToolCall {
    output_index: usize,
    call_id: String,
    tool: String,
}

#[derive(Debug)]
struct PendingToolResults {
    response_id: String,
    generation: u64,
    calls: Vec<PendingToolCall>,
    received_call_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingToolResultsArtifact {
    label: String,
    provider_interface: InterfaceId,
    response_id: String,
    generation: u64,
    calls: Vec<PendingToolCallArtifact>,
    received_call_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingToolCallArtifact {
    output_index: usize,
    call_id: String,
    tool: String,
}

struct ToolFanInState {
    outputs_by_response: BTreeMap<String, Vec<Value>>,
    pending: Option<PendingToolResults>,
}

struct ResponsesStreamAccumulator {
    protocol: &'static str,
    sequence_mode: SequenceNumbers,
    last_sequence: Option<u64>,
    events: Vec<Value>,
    items: BTreeMap<usize, Value>,
    terminal: Option<(String, Value)>,
}

struct ResponsesSseDecoder {
    pending: Vec<u8>,
    frame_index: usize,
    accumulator: ResponsesStreamAccumulator,
}

impl OpenAiResponsesDriver {
    pub fn from_descriptor(
        descriptor: &InterfaceDescriptor,
        activation_secrets_home: PathBuf,
    ) -> Result<Self, String> {
        if descriptor.interface_driver_ref != INTERFACE_DRIVER_REF {
            return Err(format!(
                "provider interface '{}' declares driver '{}', expected '{INTERFACE_DRIVER_REF}'",
                descriptor.id, descriptor.interface_driver_ref
            ));
        }
        let driver_config = descriptor.interface_driver_config.as_ref().ok_or_else(|| {
            format!(
                "provider interface '{}' has no interface_driver_config",
                descriptor.id
            )
        })?;
        let transport = driver_config.transport.as_ref().ok_or_else(|| {
            format!(
                "provider interface '{}' has no interface_driver_config.transport",
                descriptor.id
            )
        })?;
        if transport.wire.as_deref() != Some(INTERFACE_DRIVER_REF) {
            return Err(format!(
                "provider interface '{}' transport.wire must be '{INTERFACE_DRIVER_REF}'",
                descriptor.id
            ));
        }
        let metadata = &driver_config.private_config;
        let base_url_source: ValueSource = metadata_enum(metadata, "base_url_source")?;
        let auth_source: AuthSourceSelection = metadata_enum(metadata, "auth_source")?;
        validate_process_env_names(metadata, transport, base_url_source, auth_source)?;
        let model = resolve_model(metadata)?;
        let options: ResponsesOptions = serde_json::from_value(
            metadata
                .get("responses_api")
                .cloned()
                .ok_or_else(|| "Responses private_config.responses_api is missing".to_string())?,
        )
        .map_err(|error| format!("Responses responses_api config is malformed: {error}"))?;
        validate_options(&options)?;
        let model_options = options.for_provider_model(&model);
        let base_url = resolve_base_url(transport, base_url_source)?;
        let endpoint = resolve_endpoint(&base_url, &options.operation.path)?;
        let models_endpoint = options
            .model_catalog
            .as_ref()
            .map(|config| resolve_endpoint(&base_url, &config.path))
            .transpose()?;
        let auth = resolve_auth(descriptor, transport, auth_source, activation_secrets_home)?;
        let headers = resolve_headers(transport, auth.header.as_ref())?;
        let evidence_headers = resolve_evidence_headers(&options.evidence_headers)?;
        let request_timeout_ms = transport
            .request_timeout_ms
            .ok_or_else(|| "Responses transport.request_timeout_ms is required".to_string())?;
        if request_timeout_ms == 0 || request_timeout_ms > 30 * 60 * 1000 {
            return Err("Responses request_timeout_ms must be within 1..=1800000".to_string());
        }
        let max_response_bytes = transport
            .max_response_bytes
            .ok_or_else(|| "Responses transport.max_response_bytes is required".to_string())?;
        if max_response_bytes == 0 || max_response_bytes > ABSOLUTE_MAX_RESPONSE_BYTES {
            return Err(format!(
                "Responses max_response_bytes must be within 1..={ABSOLUTE_MAX_RESPONSE_BYTES}"
            ));
        }
        let websocket = match (
            options.operation.response_mode,
            options.operation.websocket.as_ref(),
        ) {
            (ResponseMode::Websocket, Some(config)) => Some(
                ResponsesWebSocketTransport::new(
                    endpoint.clone(),
                    config.clone(),
                    max_response_bytes as usize,
                )
                .map_err(|error| format!("Responses websocket config is invalid: {error}"))?,
            ),
            (ResponseMode::Websocket, None) => {
                return Err(
                    "Responses websocket response_mode requires operation.websocket".to_string(),
                );
            }
            (ResponseMode::Sse | ResponseMode::Json, Some(_)) => {
                return Err(
                    "Responses operation.websocket is only valid for websocket response_mode"
                        .to_string(),
                );
            }
            (ResponseMode::Sse | ResponseMode::Json, None) => None,
        };
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(request_timeout_ms))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| format!("Responses HTTP client build failed: {error}"))?;
        let native_tool_output_types = strict_string_set(
            &options.native_tool_output_types,
            "responses_api.native_tool_output_types",
        )?;
        let response_item_types = strict_string_set(
            &options.response_item_types,
            "responses_api.response_item_types",
        )?;
        let response_content_types = strict_string_set(
            &options.response_content_types,
            "responses_api.response_content_types",
        )?;
        let allowed_roles = strict_string_set(
            &options.carrier_policy.allowed_roles,
            "responses_api.carrier_policy.allowed_roles",
        )?;
        let allowed_input_item_types = strict_string_set(
            &options.carrier_policy.allowed_input_item_types,
            "responses_api.carrier_policy.allowed_input_item_types",
        )?;
        let allowed_content_types = strict_string_set(
            &options.carrier_policy.allowed_content_types,
            "responses_api.carrier_policy.allowed_content_types",
        )?;
        let allowed_image_details = strict_string_set(
            &options.carrier_policy.allowed_image_details,
            "responses_api.carrier_policy.allowed_image_details",
        )?;
        let allowed_file_details = strict_string_set(
            &options.carrier_policy.allowed_file_details,
            "responses_api.carrier_policy.allowed_file_details",
        )?;
        let allowed_image_url_schemes = strict_string_set(
            &options.carrier_policy.allowed_image_url_schemes,
            "responses_api.carrier_policy.allowed_image_url_schemes",
        )?;
        let allowed_file_url_schemes = strict_string_set(
            &options.carrier_policy.allowed_file_url_schemes,
            "responses_api.carrier_policy.allowed_file_url_schemes",
        )?;
        let strip_item_ids = strict_string_set(
            &options.replay.strip_item_ids,
            "responses_api.replay.strip_item_ids",
        )?;
        Ok(Self {
            interface_id: descriptor.id.as_str().to_string(),
            endpoint,
            models_endpoint,
            headers,
            evidence_headers,
            auth,
            model,
            options,
            model_options,
            private_config: metadata.clone(),
            native_tool_output_types,
            response_item_types,
            response_content_types,
            allowed_roles,
            allowed_input_item_types,
            allowed_content_types,
            allowed_image_details,
            allowed_file_details,
            allowed_image_url_schemes,
            allowed_file_url_schemes,
            strip_item_ids,
            max_response_bytes: max_response_bytes as usize,
            client,
            websocket,
        })
    }

    pub fn enforce_formed_model_policy(
        &self,
        input: &InterfaceInvocationEnvelope,
    ) -> Result<(), ResponsesFailure> {
        let manifest = input
            .read_artifact_manifest()
            .map_err(|error| ResponsesFailure::contract(error.to_string()))?;
        if manifest.label == MODELS_LIST_LABEL {
            return Ok(());
        }
        let models = required_formed_models(input)?;
        if !models.contains(&self.model) {
            return Err(ResponsesFailure::contract(format!(
                "Responses selected model '{}' is outside formed interface policy models",
                self.model
            )));
        }
        self.model_options.as_ref().map_err(|error| {
            ResponsesFailure::contract(format!("Responses provider model is unavailable: {error}"))
        })?;
        Ok(())
    }

    pub(crate) fn retry_backoff(&self, attempt: u32) -> Duration {
        let multiplier = u64::from(attempt.max(1));
        let milliseconds = self
            .options
            .retry
            .backoff_ms
            .saturating_mul(multiplier)
            .min(self.options.retry.max_backoff_ms);
        Duration::from_millis(milliseconds)
    }

    pub(crate) async fn refresh_authentication(&self) -> Result<(), ResponsesFailure> {
        self.auth.refresh_azure_authentication().await
    }

    pub async fn receive_artifact_and_materialize_outputs(
        &self,
        input: InterfaceInvocationEnvelope,
        source_movement_seq: u64,
        attempt: u32,
        handoff: &ProviderOutputHandoff,
        output_root: &Path,
    ) -> Result<Vec<ProviderEmission>, ResponsesFailure> {
        self.receive_artifact_and_materialize_outputs_with_auth_refresh(
            input,
            source_movement_seq,
            attempt,
            handoff,
            output_root,
        )
        .await
        .map_err(ResponsesDriverFailure::into_failure)
    }

    pub(crate) async fn receive_artifact_and_materialize_outputs_with_auth_refresh(
        &self,
        input: InterfaceInvocationEnvelope,
        source_movement_seq: u64,
        attempt: u32,
        handoff: &ProviderOutputHandoff,
        output_root: &Path,
    ) -> Result<Vec<ProviderEmission>, ResponsesDriverFailure> {
        self.enforce_formed_model_policy(&input)?;
        let manifest = input
            .read_artifact_manifest()
            .map_err(|error| ResponsesFailure::contract(error.to_string()))?;
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
                .materialize_model_catalog(&input, output_root, &dispatch)
                .await;
        }
        let tools = self.translate_tools(&input)?;
        let assembled = match self.prepare_input(&input, source_movement_seq, &tools)? {
            InputAssembly::Ready(assembled) => assembled,
            InputAssembly::Awaiting(pending) => {
                return self
                    .materialize_pending_tool_results(output_root, pending)
                    .map_err(Into::into);
            }
        };
        let request = self.build_request(&assembled, &tools)?;
        let envelope = self.call(request, &dispatch).await?;
        self.materialize_response(envelope, output_root, &tools)
            .map_err(Into::into)
    }

    fn build_request(
        &self,
        assembled: &AssembledInput,
        tools: &ToolCatalog,
    ) -> Result<Value, ResponsesFailure> {
        let model_options = self.provider_model_options()?;
        let mut body = model_options.request_fields.clone();
        body.insert("model".to_string(), Value::String(self.model.clone()));
        body.insert(
            "instructions".to_string(),
            Value::String(assembled.instructions.clone()),
        );
        body.insert("input".to_string(), Value::Array(assembled.items.clone()));
        let mut declarations = tools.declarations.clone();
        declarations.extend(self.options.native_tools.clone());
        if !declarations.is_empty() {
            body.insert("tools".to_string(), Value::Array(declarations.clone()));
            if let Some(tool_choice) = &self.options.function_tools.tool_choice {
                body.insert("tool_choice".to_string(), tool_choice.clone());
            }
            if let Some(parallel_tool_calls) = self.options.function_tools.parallel_tool_calls {
                body.insert(
                    "parallel_tool_calls".to_string(),
                    Value::Bool(parallel_tool_calls),
                );
            }
        }
        body.insert(
            "stream".to_string(),
            Value::Bool(matches!(
                self.options.operation.response_mode,
                ResponseMode::Sse | ResponseMode::Websocket
            )),
        );
        if let Some(cache) = &model_options.cache {
            let cache_key = self.cache_key(cache, &assembled.instructions, &declarations)?;
            body.insert(cache.field.clone(), Value::String(cache_key));
        }
        Ok(Value::Object(body))
    }

    fn cache_key(
        &self,
        cache: &CacheConfig,
        instructions: &str,
        tools: &[Value],
    ) -> Result<String, ResponsesFailure> {
        let source = serde_json::to_vec(&json!({
            "model": self.model,
            "instructions": instructions,
            "tools": tools,
        }))
        .map_err(|error| {
            ResponsesFailure::contract(format!("cache source did not encode: {error}"))
        })?;
        match cache.mode {
            CacheMode::StaticPrefixBlake3 => {
                let digest = blake3::hash(&source).to_hex().to_string();
                Ok(format!(
                    "{}{}",
                    cache.prefix,
                    &digest[..cache.digest_characters]
                ))
            }
        }
    }

    async fn call(
        &self,
        request: Value,
        dispatch: &DispatchAdmission<'_>,
    ) -> Result<ResponsesEnvelope, ResponsesDriverFailure> {
        match self.options.operation.response_mode {
            ResponseMode::Sse | ResponseMode::Json => self.call_http(request, dispatch).await,
            ResponseMode::Websocket => self
                .call_websocket(request, dispatch)
                .await
                .map_err(Into::into),
        }
    }

    async fn call_http(
        &self,
        request: Value,
        dispatch: &DispatchAdmission<'_>,
    ) -> Result<ResponsesEnvelope, ResponsesDriverFailure> {
        let request_bytes = serde_json::to_vec(&request).map_err(|error| {
            ResponsesFailure::contract(format!("Responses request did not encode: {error}"))
        })?;
        let mut builder = match self.options.operation.method {
            HttpMethod::Post => self.client.post(self.endpoint.clone()),
        };
        builder = builder.headers(self.headers.clone());
        if let Some((header, value)) = self.auth.header_pair().await? {
            builder = builder.header(header, value);
        }
        builder = builder
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(
                reqwest::header::ACCEPT,
                match self.options.operation.response_mode {
                    ResponseMode::Sse => "text/event-stream",
                    ResponseMode::Json => "application/json",
                    ResponseMode::Websocket => {
                        return Err(ResponsesFailure::contract(
                            "Responses websocket mode reached the HTTP transport",
                        )
                        .into());
                    }
                },
            )
            .body(request_bytes.clone());
        dispatch
            .admit("POST", "http", &self.endpoint, &request_bytes)
            .await
            .map_err(ResponsesFailure::unavailable)?;
        let response = builder.send().await.map_err(|error| {
            ResponsesFailure::transport(format!(
                "Responses transport request to {} failed: {error}",
                self.endpoint
            ))
        })?;
        let status = response.status().as_u16();
        let response_headers = capture_response_headers(response.headers(), &self.evidence_headers);
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !(200..300).contains(&status) {
            let response_bytes = read_bounded(response, self.max_response_bytes, false).await?;
            if status == 401 && self.auth.invalidate_azure_authentication().await {
                return Err(ResponsesDriverFailure::AuthenticatedRefreshRequired(
                    ResponsesFailure::authenticated_refresh_required(
                        status,
                        response_headers,
                        &response_bytes,
                    ),
                ));
            }
            return Err(ResponsesFailure::endpoint(
                status,
                self.options.retry.http_statuses.contains(&status),
                response_headers,
                &response_bytes,
            )
            .into());
        }
        match self.options.operation.response_mode {
            ResponseMode::Sse if !content_type.contains("text/event-stream") => {
                let response_bytes = read_bounded(response, self.max_response_bytes, false).await?;
                return Err(ResponsesFailure::response_contract(
                    format!(
                        "Responses descriptor requires SSE but endpoint returned content-type '{content_type}'"
                    ),
                    status,
                    response_headers,
                    &response_bytes,
                )
                .into());
            }
            ResponseMode::Json if !content_type.contains("json") => {
                let response_bytes = read_bounded(response, self.max_response_bytes, false).await?;
                return Err(ResponsesFailure::response_contract(
                    format!(
                        "Responses descriptor requires JSON but endpoint returned content-type '{content_type}'"
                    ),
                    status,
                    response_headers,
                    &response_bytes,
                )
                .into());
            }
            _ => {}
        }
        let (final_response, stream_events, terminal_type, response_evidence) =
            match self.options.operation.response_mode {
                ResponseMode::Sse => {
                    let (parsed, evidence) = read_responses_sse(
                        response,
                        self.max_response_bytes,
                        self.options.operation.sequence_numbers,
                        status,
                        &response_headers,
                    )
                    .await?;
                    (
                        parsed.response,
                        parsed.events,
                        Some(parsed.terminal_type),
                        evidence,
                    )
                }
                ResponseMode::Json => {
                    let response_bytes =
                        read_bounded(response, self.max_response_bytes, false).await?;
                    let value: Value =
                        serde_json::from_slice(&response_bytes).map_err(|error| {
                            ResponsesFailure::response_contract(
                                format!("Responses JSON body is malformed: {error}"),
                                status,
                                response_headers.clone(),
                                &response_bytes,
                            )
                        })?;
                    let evidence = ResponseBodyEvidence {
                        bytes: response_bytes.len(),
                        blake3: blake3::hash(&response_bytes).to_hex().to_string(),
                        raw: Some(response_bytes),
                    };
                    (value, Vec::new(), None, evidence)
                }
                ResponseMode::Websocket => {
                    return Err(ResponsesFailure::contract(
                        "Responses websocket mode reached the HTTP response decoder",
                    )
                    .into());
                }
            };
        validate_terminal_response(
            &final_response,
            terminal_type.as_deref(),
            status,
            response_headers.clone(),
            &response_evidence,
        )?;
        Ok(ResponsesEnvelope {
            response: final_response,
            request_bytes,
            response_evidence,
            status,
            headers: response_headers,
            stream_events,
        })
    }

    async fn call_websocket(
        &self,
        mut request: Value,
        dispatch: &DispatchAdmission<'_>,
    ) -> Result<ResponsesEnvelope, ResponsesFailure> {
        let config =
            self.options.operation.websocket.as_ref().ok_or_else(|| {
                ResponsesFailure::contract("Responses websocket config is missing")
            })?;
        let object = request.as_object_mut().ok_or_else(|| {
            ResponsesFailure::contract("Responses websocket request is not an object")
        })?;
        if object.contains_key("type") {
            return Err(ResponsesFailure::contract(
                "Responses websocket request already contains protocol-owned type",
            ));
        }
        object.insert(
            "type".to_string(),
            Value::String(config.request_event_type.clone()),
        );
        let request_bytes = serde_json::to_vec(&request).map_err(|error| {
            ResponsesFailure::contract(format!(
                "Responses websocket request did not encode: {error}"
            ))
        })?;
        let mut headers = self.headers.clone();
        if let Some((header, value)) = self.auth.header_pair().await? {
            headers.insert(header, value);
        }
        let transport = self.websocket.as_ref().ok_or_else(|| {
            ResponsesFailure::contract("Responses websocket transport was not configured")
        })?;
        dispatch
            .admit(
                "response.create",
                "websocket",
                transport.endpoint(),
                &request_bytes,
            )
            .await
            .map_err(ResponsesFailure::unavailable)?;
        let exchange = transport
            .exchange(request_bytes, headers)
            .await
            .map_err(|failure| self.map_websocket_failure(failure))?;
        let response_headers = capture_response_headers(&exchange.headers, &self.evidence_headers);
        if let Some(error_event) = exchange
            .events
            .iter()
            .find(|event| event.get("type").and_then(Value::as_str) == Some("error"))
        {
            return Err(websocket_error_outcome(
                error_event,
                exchange.status,
                response_headers,
                &exchange.response_bytes,
            ));
        }
        let parsed =
            parse_websocket_events(&exchange.events, self.options.operation.sequence_numbers)
                .map_err(|detail| {
                    ResponsesFailure::response_contract(
                        detail,
                        exchange.status,
                        response_headers.clone(),
                        &exchange.response_bytes,
                    )
                })?;
        let response_evidence = ResponseBodyEvidence {
            bytes: exchange.response_bytes.len(),
            blake3: blake3::hash(&exchange.response_bytes).to_hex().to_string(),
            raw: Some(exchange.response_bytes),
        };
        validate_terminal_response(
            &parsed.response,
            Some(&parsed.terminal_type),
            exchange.status,
            response_headers.clone(),
            &response_evidence,
        )?;
        Ok(ResponsesEnvelope {
            response: parsed.response,
            request_bytes: exchange.request_bytes,
            response_evidence,
            status: exchange.status,
            headers: response_headers,
            stream_events: parsed.events,
        })
    }

    fn map_websocket_failure(&self, failure: ResponsesWebSocketFailure) -> ResponsesFailure {
        let headers = capture_response_headers(&failure.headers, &self.evidence_headers);
        let retryable = failure.stage == WebSocketFailureStage::BeforeRequest
            && failure.kind == WebSocketFailureKind::Transport
            && failure.status.map_or(true, |status| {
                self.options.retry.http_statuses.contains(&status)
            });
        let class = match failure.kind {
            WebSocketFailureKind::Transport => "transport",
            WebSocketFailureKind::ResponseLimit => "response_limit",
            WebSocketFailureKind::ResponseContract => "response_contract",
        };
        let detail = if failure.stage == WebSocketFailureStage::AfterRequest {
            format!(
                "{}; websocket resume is unsupported and this observable turn was not replayed",
                failure.detail
            )
        } else {
            failure.detail
        };
        ResponsesFailure {
            class,
            retryable,
            status: failure.status,
            code: None,
            detail,
            evidence: Some(json!({
                "transport": "websocket",
                "stage": match failure.stage {
                    WebSocketFailureStage::BeforeRequest => "before_request",
                    WebSocketFailureStage::AfterRequest => "after_request",
                },
                "headers": headers,
                "response_bytes": failure.bytes.len(),
                "response_blake3": blake3::hash(&failure.bytes).to_hex().to_string(),
                "raw_response": raw_bytes_evidence(&failure.bytes),
            })),
        }
    }

    async fn materialize_model_catalog(
        &self,
        input: &InterfaceInvocationEnvelope,
        output_root: &Path,
        dispatch: &DispatchAdmission<'_>,
    ) -> Result<Vec<ProviderEmission>, ResponsesDriverFailure> {
        let formed_models = required_formed_models(input)?;
        let config = self.options.model_catalog.as_ref().ok_or_else(|| {
            ResponsesFailure::contract(
                "provider.models.list was sent to a Responses descriptor with no model_catalog endpoint",
            )
        })?;
        if config.request_label != MODELS_LIST_LABEL {
            return Err(ResponsesFailure::contract(format!(
                "Responses model_catalog request_label '{}' does not match '{MODELS_LIST_LABEL}'",
                config.request_label
            ))
            .into());
        }
        let bytes = input
            .read_artifact_payload()
            .map_err(|error| ResponsesFailure::contract(error.to_string()))?;
        let request: Value = serde_json::from_slice(&bytes).map_err(|error| {
            ResponsesFailure::contract(format!("provider.models.list payload is not JSON: {error}"))
        })?;
        if !request.as_object().is_some_and(Map::is_empty) {
            return Err(ResponsesFailure::contract(
                "provider.models.list payload must be an empty object; the endpoint is descriptor-owned",
            )
            .into());
        }
        let endpoint = self.models_endpoint.as_ref().ok_or_else(|| {
            ResponsesFailure::contract("Responses model catalog endpoint did not resolve")
        })?;
        let mut builder = self
            .client
            .get(endpoint.clone())
            .headers(self.headers.clone());
        if let Some((header, value)) = self.auth.header_pair().await? {
            builder = builder.header(header, value);
        }
        dispatch
            .admit("GET", "http", endpoint, &[])
            .await
            .map_err(ResponsesFailure::unavailable)?;
        let response = builder
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|error| {
                ResponsesFailure::transport(format!(
                    "Responses model catalog request to {endpoint} failed: {error}"
                ))
            })?;
        let status = response.status().as_u16();
        let headers = capture_response_headers(response.headers(), &self.evidence_headers);
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let response_bytes = read_bounded(response, self.max_response_bytes, false).await?;
        if status == 401 && self.auth.invalidate_azure_authentication().await {
            return Err(ResponsesDriverFailure::AuthenticatedRefreshRequired(
                ResponsesFailure::authenticated_refresh_required(status, headers, &response_bytes),
            ));
        }
        if !(200..300).contains(&status) {
            return Err(ResponsesFailure::endpoint(
                status,
                self.options.retry.http_statuses.contains(&status),
                headers,
                &response_bytes,
            )
            .into());
        }
        if !content_type.contains("json") {
            return Err(ResponsesFailure::response_contract(
                format!(
                    "Responses model catalog returned content-type '{content_type}' instead of JSON"
                ),
                status,
                headers,
                &response_bytes,
            )
            .into());
        }
        let value: Value = serde_json::from_slice(&response_bytes).map_err(|error| {
            ResponsesFailure::response_contract(
                format!("Responses model catalog body is not JSON: {error}"),
                status,
                headers.clone(),
                &response_bytes,
            )
        })?;
        let models = value
            .pointer(&config.models_pointer)
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ResponsesFailure::response_contract(
                    format!(
                        "Responses model catalog has no array at '{}'",
                        config.models_pointer
                    ),
                    status,
                    headers.clone(),
                    &response_bytes,
                )
            })?;
        let mut ids = BTreeSet::new();
        for (index, model) in models.iter().enumerate() {
            let id = model
                .pointer(&config.id_pointer)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty() && value.trim() == *value)
                .ok_or_else(|| {
                    ResponsesFailure::response_contract(
                        format!(
                            "Responses model catalog item {index} has no id at '{}'",
                            config.id_pointer
                        ),
                        status,
                        headers.clone(),
                        &response_bytes,
                    )
                })?;
            if !ids.insert(id.to_string()) {
                return Err(ResponsesFailure::response_contract(
                    format!("Responses model catalog repeats id '{id}'"),
                    status,
                    headers,
                    &response_bytes,
                )
                .into());
            }
        }
        let models = filter_model_catalog(models, &config.id_pointer, &formed_models);
        Ok(vec![materialize_emission(
            output_root,
            &config.output_label,
            json!({
                "label": config.output_label,
                "models": models,
                "provider_evidence": {
                    "endpoint": endpoint.as_str(),
                    "status": status,
                    "headers": headers,
                    "response_bytes": response_bytes.len(),
                    "response_blake3": blake3::hash(&response_bytes).to_hex().to_string(),
                    "raw_response": raw_bytes_evidence(&response_bytes),
                }
            }),
            ProviderEmissionRoute::ClientProjection,
        )?])
    }

    fn translate_tools(
        &self,
        input: &InterfaceInvocationEnvelope,
    ) -> Result<ToolCatalog, ResponsesFailure> {
        let hidden =
            model_hidden_tool_names(&self.private_config).map_err(ResponsesFailure::contract)?;
        let affordances = input
            .interface_affordance_specs()
            .map_err(|error| ResponsesFailure::contract(error.to_string()))?;
        let visible =
            model_visible_affordances(affordances, &hidden).map_err(ResponsesFailure::contract)?;
        let mut declarations = Vec::with_capacity(visible.len());
        let mut by_wire_name = BTreeMap::new();
        for (index, spec) in visible.into_iter().enumerate() {
            let object = spec.as_object().ok_or_else(|| {
                ResponsesFailure::contract(format!(
                    "interface_affordance_specs[{index}] is not an object"
                ))
            })?;
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty() && value.trim() == *value)
                .ok_or_else(|| {
                    ResponsesFailure::contract(format!(
                        "interface_affordance_specs[{index}] has no non-empty trimmed name"
                    ))
                })?;
            let wire_name = encode_tool_name(name)?;
            if by_wire_name
                .insert(wire_name.clone(), name.to_string())
                .is_some()
            {
                return Err(ResponsesFailure::contract(format!(
                    "Responses tool-name encoding collides at '{wire_name}'"
                )));
            }
            let parameters = match object.get("input_schema") {
                None => json!({"type": "object", "properties": {}}),
                Some(value) if value.is_object() => value.clone(),
                Some(_) => {
                    return Err(ResponsesFailure::contract(format!(
                        "interface_affordance_specs[{index}] '{name}' input_schema is not an object"
                    )));
                }
            };
            let mut declaration = Map::new();
            declaration.insert("type".to_string(), Value::String("function".to_string()));
            declaration.insert("name".to_string(), Value::String(wire_name));
            declaration.insert(
                "description".to_string(),
                Value::String(
                    object
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                ),
            );
            if let Some(strict) = self.options.function_tools.strict {
                declaration.insert("strict".to_string(), Value::Bool(strict));
            }
            declaration.insert("parameters".to_string(), parameters);
            declarations.push(Value::Object(declaration));
        }
        Ok(ToolCatalog {
            declarations,
            by_wire_name,
            hidden_tool_names: hidden,
        })
    }

    #[cfg(test)]
    fn assemble_input(
        &self,
        input: &InterfaceInvocationEnvelope,
        tools: &ToolCatalog,
    ) -> Result<AssembledInput, ResponsesFailure> {
        let artifacts = context_artifacts(input, &self.interface_id, 1)?;
        let tool_calls = self.tool_call_identities(&artifacts, tools)?;
        let groups = self.replay_groups(&artifacts)?;
        let fan_in = self.tool_fan_in(&artifacts, &tool_calls)?;
        if let Some(pending) = fan_in.pending {
            return Err(ResponsesFailure::contract(format!(
                "response '{}' is awaiting {} of {} function_call_output artifacts",
                pending.response_id,
                pending
                    .calls
                    .len()
                    .saturating_sub(pending.received_call_ids.len()),
                pending.calls.len()
            )));
        }
        self.assemble_ready_input(
            input,
            tools,
            &artifacts,
            &groups,
            &fan_in.outputs_by_response,
        )
    }

    fn prepare_input(
        &self,
        input: &InterfaceInvocationEnvelope,
        source_movement_seq: u64,
        tools: &ToolCatalog,
    ) -> Result<InputAssembly, ResponsesFailure> {
        let artifacts = context_artifacts(input, &self.interface_id, source_movement_seq)?;
        let tool_calls = self.tool_call_identities(&artifacts, tools)?;
        let groups = self.replay_groups(&artifacts)?;
        let fan_in = self.tool_fan_in(&artifacts, &tool_calls)?;
        if let Some(pending) = fan_in.pending {
            return Ok(InputAssembly::Awaiting(pending));
        }
        Ok(InputAssembly::Ready(self.assemble_ready_input(
            input,
            tools,
            &artifacts,
            &groups,
            &fan_in.outputs_by_response,
        )?))
    }

    fn assemble_ready_input(
        &self,
        input: &InterfaceInvocationEnvelope,
        tools: &ToolCatalog,
        artifacts: &[ContextArtifact],
        groups: &BTreeMap<String, ReplayGroup>,
        outputs_by_response: &BTreeMap<String, Vec<Value>>,
    ) -> Result<AssembledInput, ResponsesFailure> {
        let current = artifacts
            .iter()
            .find(|artifact| artifact.current)
            .ok_or_else(|| ResponsesFailure::contract("Responses current artifact is missing"))?;
        if current.source_artifact != input.artifact || current.source_movement_seq.is_none() {
            return Err(ResponsesFailure::contract(
                "Responses current artifact lost its admitted source provenance",
            ));
        }
        let mut instructions = Vec::new();
        let mut items = Vec::new();
        for (ordinal, artifact) in artifacts.iter().enumerate() {
            if let Some((response_id, group)) = groups
                .iter()
                .find(|(_, group)| group.first_ordinal == ordinal)
            {
                for item in group.items.values() {
                    if let Some(item) = self.normalize_replay_item(item)? {
                        items.push(item);
                    }
                }
                if let Some(outputs) = outputs_by_response.get(response_id) {
                    items.extend(outputs.iter().cloned());
                }
            }
            if is_provider_replay_label(&artifact.label) {
                continue;
            }
            if artifact.label == CONTEXT_REFERENCE_LABEL {
                let text = artifact
                    .value
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        ResponsesFailure::contract(format!(
                            "context.reference artifact '{}' has no non-empty text",
                            artifact.id
                        ))
                    })?;
                instructions.push(text.to_string());
                continue;
            }
            if is_tool_result(&artifact.label, &artifact.value) {
                continue;
            }
            if artifact.label == TOOL_RESULTS_PENDING_LABEL {
                continue;
            }
            if artifact.label == TURN_ERROR_LABEL {
                items.push(json!({
                    "type": "message",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": render_json(&artifact.value)?
                    }]
                }));
                continue;
            }
            let rule = self
                .options
                .carrier_policy
                .rules
                .iter()
                .find(|rule| rule.label() == artifact.label)
                .ok_or_else(|| {
                    ResponsesFailure::contract(format!(
                        "artifact '{}' with label '{}' has no descriptor-declared Responses carrier rule",
                        artifact.id, artifact.label
                    ))
                })?;
            items.extend(self.decode_carrier(rule, &artifact.value)?);
        }
        if let Some(metadata) = input.metadata.as_ref() {
            if let Some(rendered) =
                render_model_context_block_with_hidden_tools(metadata, &tools.hidden_tool_names)
            {
                instructions.push(rendered);
            }
        }
        if instructions.is_empty() {
            return Err(ResponsesFailure::contract(
                "Responses context is missing bound context.reference instructions",
            ));
        }
        if items.is_empty() {
            return Err(ResponsesFailure::contract(
                "Responses request has no descriptor-admitted input items",
            ));
        }
        validate_function_call_order(&items)?;
        Ok(AssembledInput {
            instructions: instructions.join("\n\n"),
            items,
        })
    }

    fn tool_fan_in(
        &self,
        artifacts: &[ContextArtifact],
        calls: &BTreeMap<String, ToolCallIdentity>,
    ) -> Result<ToolFanInState, ResponsesFailure> {
        let mut calls_by_response: BTreeMap<String, BTreeMap<usize, String>> = BTreeMap::new();
        for (call_id, call) in calls {
            let previous = calls_by_response
                .entry(call.response_id.clone())
                .or_default()
                .insert(call.output_index, call_id.clone());
            if let Some(previous) = previous {
                return Err(ResponsesFailure::contract(format!(
                    "response '{}' repeats function_call output_index {} for call_ids '{}' and '{}'",
                    call.response_id, call.output_index, previous, call_id
                )));
            }
        }

        let mut results = BTreeMap::new();
        let mut pending_artifacts: BTreeMap<String, Vec<(usize, PendingToolResultsArtifact)>> =
            BTreeMap::new();
        for (artifact_ordinal, artifact) in artifacts.iter().enumerate() {
            if is_tool_result(&artifact.label, &artifact.value) {
                let correlation_id =
                    required_trimmed_string(&artifact.value, "correlation_id", "tool result")?;
                let item = self.tool_result_item(&artifact.value, calls)?;
                if results
                    .insert(
                        correlation_id.clone(),
                        ToolResultIdentity {
                            artifact_ordinal,
                            item,
                        },
                    )
                    .is_some()
                {
                    return Err(ResponsesFailure::contract(format!(
                        "Responses context repeats tool result correlation_id '{correlation_id}'"
                    )));
                }
            } else if artifact.label == TOOL_RESULTS_PENDING_LABEL {
                let pending: PendingToolResultsArtifact =
                    serde_json::from_value(artifact.value.clone()).map_err(|error| {
                        ResponsesFailure::contract(format!(
                            "{TOOL_RESULTS_PENDING_LABEL} artifact is malformed: {error}"
                        ))
                    })?;
                if pending.label != TOOL_RESULTS_PENDING_LABEL {
                    return Err(ResponsesFailure::contract(format!(
                        "pending tool-result artifact label '{}' does not match '{TOOL_RESULTS_PENDING_LABEL}'",
                        pending.label
                    )));
                }
                if pending.provider_interface.as_str() != self.interface_id {
                    return Err(ResponsesFailure::contract(format!(
                        "pending tool-result artifact belongs to provider '{}' instead of '{}'",
                        pending.provider_interface.as_str(),
                        self.interface_id
                    )));
                }
                if pending.response_id.is_empty()
                    || pending.response_id.trim() != pending.response_id
                    || pending.generation == 0
                {
                    return Err(ResponsesFailure::contract(
                        "pending tool-result artifact has invalid response_id or generation",
                    ));
                }
                pending_artifacts
                    .entry(pending.response_id.clone())
                    .or_default()
                    .push((artifact_ordinal, pending));
            }
        }

        for correlation_id in results.keys() {
            if !calls.contains_key(correlation_id) {
                return Err(ResponsesFailure::contract(format!(
                    "tool result correlation_id '{correlation_id}' has no bound function_call"
                )));
            }
        }

        for (response_id, markers) in &mut pending_artifacts {
            let call_indexes = calls_by_response.get(response_id).ok_or_else(|| {
                ResponsesFailure::contract(format!(
                    "pending tool-result response_id '{response_id}' has no bound function_call group"
                ))
            })?;
            markers.sort_by_key(|(ordinal, _)| *ordinal);
            let expected_calls = call_indexes
                .iter()
                .map(|(output_index, call_id)| {
                    let call = calls.get(call_id).ok_or_else(|| {
                        ResponsesFailure::contract(format!(
                            "response '{response_id}' lost function_call '{call_id}'"
                        ))
                    })?;
                    Ok(PendingToolCall {
                        output_index: *output_index,
                        call_id: call_id.clone(),
                        tool: call.tool.clone(),
                    })
                })
                .collect::<Result<Vec<_>, ResponsesFailure>>()?;
            for (marker_index, (artifact_ordinal, marker)) in markers.iter().enumerate() {
                let generation = u64::try_from(marker_index + 1).map_err(|_| {
                    ResponsesFailure::contract("pending tool-result generation overflowed")
                })?;
                if marker.generation != generation {
                    return Err(ResponsesFailure::contract(format!(
                        "response '{response_id}' pending generations are not contiguous at {generation}"
                    )));
                }
                let marker_calls = marker
                    .calls
                    .iter()
                    .map(|call| {
                        if call.call_id.is_empty()
                            || call.call_id.trim() != call.call_id
                            || call.tool.is_empty()
                            || call.tool.trim() != call.tool
                        {
                            return Err(ResponsesFailure::contract(format!(
                                "response '{response_id}' pending call has invalid identity"
                            )));
                        }
                        Ok(PendingToolCall {
                            output_index: call.output_index,
                            call_id: call.call_id.clone(),
                            tool: call.tool.clone(),
                        })
                    })
                    .collect::<Result<Vec<_>, ResponsesFailure>>()?;
                if marker_calls != expected_calls {
                    return Err(ResponsesFailure::contract(format!(
                        "response '{response_id}' pending call set does not match its function_call group"
                    )));
                }
                let received = expected_calls
                    .iter()
                    .filter(|call| {
                        results
                            .get(&call.call_id)
                            .is_some_and(|result| result.artifact_ordinal < *artifact_ordinal)
                    })
                    .map(|call| call.call_id.clone())
                    .collect::<Vec<_>>();
                if marker.received_call_ids != received {
                    return Err(ResponsesFailure::contract(format!(
                        "response '{response_id}' pending received_call_ids do not match session provenance"
                    )));
                }
            }
        }

        let incomplete = calls_by_response
            .iter()
            .filter(|(_, group)| group.values().any(|call_id| !results.contains_key(call_id)))
            .map(|(response_id, _)| response_id.clone())
            .collect::<Vec<_>>();
        if incomplete.len() > 1 {
            return Err(ResponsesFailure::contract(format!(
                "Responses context mixes unresolved function_call groups: {}",
                incomplete.join(", ")
            )));
        }

        if let Some(response_id) = incomplete.first() {
            let current = artifacts
                .iter()
                .find(|artifact| artifact.current)
                .ok_or_else(|| {
                    ResponsesFailure::contract("Responses current artifact is missing")
                })?;
            if !is_tool_result(&current.label, &current.value) {
                return Err(ResponsesFailure::contract(format!(
                    "response '{response_id}' is awaiting function_call_output artifacts but the current artifact is not a tool result"
                )));
            }
            let current_call_id =
                required_trimmed_string(&current.value, "correlation_id", "tool result")?;
            let current_call = calls.get(&current_call_id).ok_or_else(|| {
                ResponsesFailure::contract(format!(
                    "current tool result correlation_id '{current_call_id}' has no bound function_call"
                ))
            })?;
            if &current_call.response_id != response_id {
                return Err(ResponsesFailure::contract(format!(
                    "current tool result belongs to response '{}' while unresolved response '{}' is active",
                    current_call.response_id, response_id
                )));
            }
            let call_indexes = calls_by_response.get(response_id).ok_or_else(|| {
                ResponsesFailure::contract(format!(
                    "response '{response_id}' lost its function_call group"
                ))
            })?;
            let calls = call_indexes
                .iter()
                .map(|(output_index, call_id)| {
                    let call = calls.get(call_id).ok_or_else(|| {
                        ResponsesFailure::contract(format!(
                            "response '{response_id}' lost function_call '{call_id}'"
                        ))
                    })?;
                    Ok(PendingToolCall {
                        output_index: *output_index,
                        call_id: call_id.clone(),
                        tool: call.tool.clone(),
                    })
                })
                .collect::<Result<Vec<_>, ResponsesFailure>>()?;
            let received_call_ids = calls
                .iter()
                .filter(|call| results.contains_key(&call.call_id))
                .map(|call| call.call_id.clone())
                .collect::<Vec<_>>();
            let prior_generations = pending_artifacts
                .get(response_id)
                .map(Vec::len)
                .unwrap_or(0);
            let generation = u64::try_from(prior_generations)
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| {
                    ResponsesFailure::contract("pending tool-result generation overflowed")
                })?;
            return Ok(ToolFanInState {
                outputs_by_response: BTreeMap::new(),
                pending: Some(PendingToolResults {
                    response_id: response_id.clone(),
                    generation,
                    calls,
                    received_call_ids,
                }),
            });
        }

        let mut outputs_by_response = BTreeMap::new();
        for (response_id, call_indexes) in calls_by_response {
            let outputs = call_indexes
                .values()
                .map(|call_id| {
                    results
                        .get(call_id)
                        .map(|result| result.item.clone())
                        .ok_or_else(|| {
                            ResponsesFailure::contract(format!(
                                "response '{response_id}' lost function_call_output '{call_id}'"
                            ))
                        })
                })
                .collect::<Result<Vec<_>, ResponsesFailure>>()?;
            outputs_by_response.insert(response_id, outputs);
        }
        Ok(ToolFanInState {
            outputs_by_response,
            pending: None,
        })
    }

    fn materialize_pending_tool_results(
        &self,
        output_root: &Path,
        pending: PendingToolResults,
    ) -> Result<Vec<ProviderEmission>, ResponsesFailure> {
        let calls = pending
            .calls
            .iter()
            .map(|call| {
                json!({
                    "output_index": call.output_index,
                    "call_id": call.call_id,
                    "tool": call.tool,
                })
            })
            .collect::<Vec<_>>();
        Ok(vec![materialize_emission(
            output_root,
            TOOL_RESULTS_PENDING_LABEL,
            json!({
                "label": TOOL_RESULTS_PENDING_LABEL,
                "provider_interface": self.interface_id,
                "response_id": pending.response_id,
                "generation": pending.generation,
                "calls": calls,
                "received_call_ids": pending.received_call_ids,
            }),
            ProviderEmissionRoute::ClientProjection,
        )?])
    }

    fn replay_groups(
        &self,
        artifacts: &[ContextArtifact],
    ) -> Result<BTreeMap<String, ReplayGroup>, ResponsesFailure> {
        let mut groups = BTreeMap::new();
        for (ordinal, artifact) in artifacts.iter().enumerate() {
            if !is_provider_replay_label(&artifact.label) {
                continue;
            }
            let response_id =
                required_trimmed_string(&artifact.value, "response_id", &artifact.label)?;
            let entries = artifact
                .value
                .get("response_items")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    ResponsesFailure::contract(format!(
                        "{} artifact has no response_items array",
                        artifact.label
                    ))
                })?;
            let group = groups
                .entry(response_id.clone())
                .or_insert_with(|| ReplayGroup {
                    first_ordinal: ordinal,
                    items: BTreeMap::new(),
                });
            group.first_ordinal = group.first_ordinal.min(ordinal);
            for (entry_index, entry) in entries.iter().enumerate() {
                let output_index = entry
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or_else(|| {
                        ResponsesFailure::contract(format!(
                            "{} response_items[{entry_index}] has no valid output_index",
                            artifact.label
                        ))
                    })?;
                let item = entry
                    .get("item")
                    .filter(|value| value.is_object())
                    .ok_or_else(|| {
                        ResponsesFailure::contract(format!(
                            "{} response_items[{entry_index}] has no item object",
                            artifact.label
                        ))
                    })?;
                let expected_hash = entry
                    .get("raw_blake3")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        ResponsesFailure::contract(format!(
                            "{} response_items[{entry_index}] has no raw_blake3",
                            artifact.label
                        ))
                    })?;
                let actual_hash = value_blake3(item)?;
                if expected_hash != actual_hash {
                    return Err(ResponsesFailure::contract(format!(
                        "{} response_items[{entry_index}] hash mismatch",
                        artifact.label
                    )));
                }
                if group.items.insert(output_index, item.clone()).is_some() {
                    return Err(ResponsesFailure::contract(format!(
                        "response '{response_id}' repeats item at output_index {output_index}"
                    )));
                }
            }
        }
        let mut first_ordinals = BTreeSet::new();
        for (response_id, group) in &groups {
            for (expected, actual) in group.items.keys().copied().enumerate() {
                if expected != actual {
                    return Err(ResponsesFailure::contract(format!(
                        "response '{response_id}' replay output indexes are not contiguous at {expected}"
                    )));
                }
            }
            if !first_ordinals.insert(group.first_ordinal) {
                return Err(ResponsesFailure::contract(format!(
                    "response '{response_id}' shares its first replay ordinal with another response group"
                )));
            }
        }
        Ok(groups)
    }

    fn tool_call_identities(
        &self,
        artifacts: &[ContextArtifact],
        tools: &ToolCatalog,
    ) -> Result<BTreeMap<String, ToolCallIdentity>, ResponsesFailure> {
        let mut identities = BTreeMap::new();
        for artifact in artifacts
            .iter()
            .filter(|artifact| artifact.label == TOOL_CALL_LABEL)
        {
            let response_id =
                required_trimmed_string(&artifact.value, "response_id", TOOL_CALL_LABEL)?;
            let correlation_id =
                required_trimmed_string(&artifact.value, "correlation_id", TOOL_CALL_LABEL)?;
            let tool = required_trimmed_string(&artifact.value, "tool", TOOL_CALL_LABEL)?;
            let call_id =
                required_trimmed_string(&artifact.value, "provider_call_id", TOOL_CALL_LABEL)?;
            let entries = artifact
                .value
                .get("response_items")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    ResponsesFailure::contract("tool.call has no response_items array")
                })?;
            if entries.len() != 1 {
                return Err(ResponsesFailure::contract(
                    "tool.call must carry exactly one response item",
                ));
            }
            let raw_item = entries[0]
                .get("item")
                .filter(|value| value.is_object())
                .ok_or_else(|| ResponsesFailure::contract("tool.call response item is missing"))?;
            let output_index = entries[0]
                .get("output_index")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| {
                    ResponsesFailure::contract("tool.call response item has no valid output_index")
                })?;
            let wire_name = required_trimmed_string(raw_item, "name", "function_call item")?;
            if tools.by_wire_name.get(&wire_name) != Some(&tool) {
                return Err(ResponsesFailure::contract(format!(
                    "tool.call '{correlation_id}' names '{tool}' but raw Responses item names undeclared wire tool '{wire_name}'"
                )));
            }
            if required_trimmed_string(raw_item, "call_id", "function_call item")? != call_id {
                return Err(ResponsesFailure::contract(format!(
                    "tool.call '{correlation_id}' provider_call_id does not match its raw item"
                )));
            }
            if correlation_id != call_id {
                return Err(ResponsesFailure::contract(format!(
                    "tool.call correlation_id '{correlation_id}' must equal Responses call_id '{call_id}'"
                )));
            }
            if identities
                .insert(
                    correlation_id.clone(),
                    ToolCallIdentity {
                        response_id,
                        output_index,
                        tool,
                        call_id,
                        wire_name,
                    },
                )
                .is_some()
            {
                return Err(ResponsesFailure::contract(format!(
                    "Responses context repeats tool.call correlation_id '{correlation_id}'"
                )));
            }
        }
        Ok(identities)
    }

    fn decode_carrier(
        &self,
        rule: &CarrierRule,
        value: &Value,
    ) -> Result<Vec<Value>, ResponsesFailure> {
        match rule {
            CarrierRule::JsonText {
                role, text_pointer, ..
            } => {
                let text = value
                    .pointer(text_pointer)
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        ResponsesFailure::contract(format!(
                            "Responses json_text carrier has no non-empty string at '{text_pointer}'"
                        ))
                    })?;
                let item = json!({
                    "type": "message",
                    "role": role,
                    "content": [{
                        "type": if role == "assistant" { "output_text" } else { "input_text" },
                        "text": text,
                    }]
                });
                Ok(vec![self.normalize_carrier_item(&item)?])
            }
            CarrierRule::ResponsesItems { items_pointer, .. } => {
                let items = value
                    .pointer(items_pointer)
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        ResponsesFailure::contract(format!(
                            "Responses items carrier has no array at '{items_pointer}'"
                        ))
                    })?;
                if items.is_empty() {
                    return Err(ResponsesFailure::contract(
                        "Responses items carrier array is empty",
                    ));
                }
                items
                    .iter()
                    .map(|item| self.normalize_carrier_item(item))
                    .collect()
            }
        }
    }

    fn normalize_carrier_item(&self, item: &Value) -> Result<Value, ResponsesFailure> {
        let item_type = required_trimmed_string(item, "type", "Responses input item")?;
        if !self.allowed_input_item_types.contains(&item_type) {
            return Err(ResponsesFailure::contract(format!(
                "Responses carrier item type '{item_type}' is not descriptor-admitted"
            )));
        }
        match item_type.as_str() {
            "message" => self.normalize_message_item(item, false),
            _ => Err(ResponsesFailure::contract(format!(
                "Responses carrier item type '{item_type}' has no implemented validator"
            ))),
        }
    }

    fn normalize_replay_item(&self, item: &Value) -> Result<Option<Value>, ResponsesFailure> {
        let replay = &self.provider_model_options()?.replay;
        let item_type = required_trimmed_string(item, "type", "Responses replay item")?;
        if !self.response_item_types.contains(&item_type) {
            return Err(ResponsesFailure::contract(format!(
                "Responses replay item type '{item_type}' is not descriptor-declared"
            )));
        }
        match item_type.as_str() {
            "message" => {
                if !replay.assistant_messages {
                    return Ok(None);
                }
                Ok(Some(self.normalize_message_item(item, true)?))
            }
            "reasoning" => self.normalize_reasoning_replay(item),
            "function_call" => Ok(Some(normalize_function_call_item(
                item,
                self.strip_item_ids.contains("function_call"),
                false,
            )?)),
            other if self.native_tool_output_types.contains(other) => {
                if !replay.native_tool_items {
                    return Ok(None);
                }
                let mut replay = item.clone();
                if self.strip_item_ids.contains(other) {
                    replay
                        .as_object_mut()
                        .ok_or_else(|| {
                            ResponsesFailure::contract("native replay item is not an object")
                        })?
                        .remove("id");
                }
                Ok(Some(replay))
            }
            other => Err(ResponsesFailure::contract(format!(
                "Responses replay item type '{other}' is declared but not executable by this family"
            ))),
        }
    }

    fn normalize_message_item(
        &self,
        item: &Value,
        replay: bool,
    ) -> Result<Value, ResponsesFailure> {
        let object = item
            .as_object()
            .ok_or_else(|| ResponsesFailure::contract("Responses message item is not an object"))?;
        let allowed = if replay {
            ["type", "id", "role", "status", "phase", "content"]
                .into_iter()
                .collect::<BTreeSet<_>>()
        } else {
            ["type", "role", "content"]
                .into_iter()
                .collect::<BTreeSet<_>>()
        };
        if let Some(unknown) = object.keys().find(|key| !allowed.contains(key.as_str())) {
            return Err(ResponsesFailure::contract(format!(
                "Responses message item has unknown field '{unknown}'"
            )));
        }
        let role = required_trimmed_string(item, "role", "Responses message item")?;
        if !self.allowed_roles.contains(&role) {
            return Err(ResponsesFailure::contract(format!(
                "Responses message role '{role}' is not descriptor-admitted"
            )));
        }
        if replay && role != "assistant" {
            return Err(ResponsesFailure::contract(
                "provider replay message must have assistant role",
            ));
        }
        let content = item
            .get("content")
            .ok_or_else(|| ResponsesFailure::contract("Responses message item has no content"))?;
        let content = if let Some(text) = content.as_str() {
            if text.is_empty() {
                return Err(ResponsesFailure::contract(
                    "Responses message string content is empty",
                ));
            }
            vec![json!({
                "type": if role == "assistant" { "output_text" } else { "input_text" },
                "text": text,
            })]
        } else {
            let parts = content.as_array().ok_or_else(|| {
                ResponsesFailure::contract("Responses message content must be a string or array")
            })?;
            if parts.is_empty() {
                return Err(ResponsesFailure::contract(
                    "Responses message content array is empty",
                ));
            }
            parts
                .iter()
                .enumerate()
                .map(|(index, part)| self.normalize_content_part(part, &role, replay, index))
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut normalized = Map::new();
        normalized.insert("type".to_string(), Value::String("message".to_string()));
        if replay && !self.strip_item_ids.contains("message") {
            if let Some(id) = item
                .get("id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty() && value.trim() == *value)
            {
                normalized.insert("id".to_string(), Value::String(id.to_string()));
            }
        }
        normalized.insert("role".to_string(), Value::String(role));
        if replay {
            if let Some(status_value) = item.get("status") {
                let status = status_value
                    .as_str()
                    .filter(|value| matches!(*value, "in_progress" | "completed" | "incomplete"));
                if let Some(status) = status {
                    normalized.insert("status".to_string(), Value::String(status.to_string()));
                } else if !status_value.is_null() {
                    return Err(ResponsesFailure::contract(
                        "Responses replay message status is invalid",
                    ));
                }
            }
            if let Some(phase) = item.get("phase") {
                let phase = phase
                    .as_str()
                    .filter(|value| matches!(*value, "commentary" | "final_answer"))
                    .ok_or_else(|| {
                        ResponsesFailure::contract("Responses replay message phase is invalid")
                    })?;
                normalized.insert("phase".to_string(), Value::String(phase.to_string()));
            }
        }
        normalized.insert("content".to_string(), Value::Array(content));
        Ok(Value::Object(normalized))
    }

    fn normalize_content_part(
        &self,
        part: &Value,
        role: &str,
        replay: bool,
        index: usize,
    ) -> Result<Value, ResponsesFailure> {
        let encrypted_replay = self.provider_model_options()?.replay.encrypted_content;
        let object = part.as_object().ok_or_else(|| {
            ResponsesFailure::contract(format!(
                "Responses message content[{index}] is not an object"
            ))
        })?;
        let part_type = required_trimmed_string(part, "type", "Responses content part")?;
        if replay && self.response_content_types.contains(&part_type) {
            return normalize_output_content_part(object, &part_type, encrypted_replay);
        }
        if !self.allowed_content_types.contains(&part_type) {
            return Err(ResponsesFailure::contract(format!(
                "Responses content type '{part_type}' is not descriptor-admitted"
            )));
        }
        match part_type.as_str() {
            "input_text" | "output_text" => {
                let expected = if role == "assistant" {
                    "output_text"
                } else {
                    "input_text"
                };
                if part_type != expected {
                    return Err(ResponsesFailure::contract(format!(
                        "Responses role '{role}' requires '{expected}', not '{part_type}'"
                    )));
                }
                let text = required_string_allow_empty(part, "text", "Responses text part")?;
                ensure_only_fields(object, &["type", "text"], "Responses text part")?;
                Ok(json!({"type": expected, "text": text}))
            }
            "input_image" => {
                if role != "user" {
                    return Err(ResponsesFailure::contract(
                        "Responses input_image is admitted only on user messages",
                    ));
                }
                ensure_only_fields(
                    object,
                    &["type", "image_url", "file_id", "detail"],
                    "Responses input_image",
                )?;
                let image_url = optional_trimmed_string(part, "image_url")?;
                let file_id = optional_trimmed_string(part, "file_id")?;
                if image_url.is_some() == file_id.is_some() {
                    return Err(ResponsesFailure::contract(
                        "Responses input_image must declare exactly one of image_url or file_id",
                    ));
                }
                if let Some(url) = &image_url {
                    validate_url_scheme(url, &self.allowed_image_url_schemes, "input_image")?;
                }
                let detail = optional_trimmed_string(part, "detail")?;
                if let Some(detail) = &detail {
                    if !self.allowed_image_details.contains(detail) {
                        return Err(ResponsesFailure::contract(format!(
                            "Responses image detail '{detail}' is not descriptor-admitted"
                        )));
                    }
                }
                let mut normalized = Map::new();
                normalized.insert("type".to_string(), Value::String("input_image".to_string()));
                if let Some(value) = image_url {
                    normalized.insert("image_url".to_string(), Value::String(value));
                }
                if let Some(value) = file_id {
                    normalized.insert("file_id".to_string(), Value::String(value));
                }
                if let Some(value) = detail {
                    normalized.insert("detail".to_string(), Value::String(value));
                }
                Ok(Value::Object(normalized))
            }
            "input_file" => {
                if role == "assistant" {
                    return Err(ResponsesFailure::contract(
                        "Responses input_file is not admitted on assistant messages",
                    ));
                }
                ensure_only_fields(
                    object,
                    &[
                        "type",
                        "file_data",
                        "file_id",
                        "file_url",
                        "filename",
                        "detail",
                    ],
                    "Responses input_file",
                )?;
                let file_data = optional_trimmed_string(part, "file_data")?;
                let file_id = optional_trimmed_string(part, "file_id")?;
                let file_url = optional_trimmed_string(part, "file_url")?;
                let sources = [file_data.is_some(), file_id.is_some(), file_url.is_some()]
                    .into_iter()
                    .filter(|present| *present)
                    .count();
                if sources != 1 {
                    return Err(ResponsesFailure::contract(
                        "Responses input_file must declare exactly one of file_data, file_id or file_url",
                    ));
                }
                if let Some(data) = &file_data {
                    validate_inline_file_data(
                        data,
                        self.options.carrier_policy.max_inline_file_bytes,
                    )?;
                    if optional_trimmed_string(part, "filename")?.is_none() {
                        return Err(ResponsesFailure::contract(
                            "Responses input_file.file_data requires filename",
                        ));
                    }
                }
                if let Some(url) = &file_url {
                    validate_url_scheme(url, &self.allowed_file_url_schemes, "input_file")?;
                }
                if let Some(detail) = optional_trimmed_string(part, "detail")? {
                    if !self.allowed_file_details.contains(&detail) {
                        return Err(ResponsesFailure::contract(format!(
                            "Responses file detail '{detail}' is not descriptor-admitted"
                        )));
                    }
                }
                let mut normalized = object.clone();
                normalized.insert("type".to_string(), Value::String("input_file".to_string()));
                Ok(Value::Object(normalized))
            }
            "encrypted_content" => {
                if role != "assistant" || !encrypted_replay {
                    return Err(ResponsesFailure::contract(
                        "Responses encrypted_content is not admitted on this carrier",
                    ));
                }
                ensure_only_fields(
                    object,
                    &["type", "encrypted_content"],
                    "Responses encrypted_content",
                )?;
                Ok(json!({
                    "type": "encrypted_content",
                    "encrypted_content": required_trimmed_string(part, "encrypted_content", "Responses encrypted_content")?,
                }))
            }
            other => Err(ResponsesFailure::contract(format!(
                "Responses content type '{other}' has no implemented validator"
            ))),
        }
    }

    fn normalize_reasoning_replay(&self, item: &Value) -> Result<Option<Value>, ResponsesFailure> {
        let replay = &self.provider_model_options()?.replay;
        if !replay.encrypted_reasoning {
            return Ok(None);
        }
        let object = item
            .as_object()
            .ok_or_else(|| ResponsesFailure::contract("reasoning replay item is not an object"))?;
        ensure_only_fields(
            object,
            &[
                "type",
                "id",
                "status",
                "summary",
                "content",
                "encrypted_content",
                "replay_scope",
            ],
            "Responses reasoning replay item",
        )?;
        validate_optional_item_status(item, "Responses reasoning replay item")?;
        let Some(encrypted) = optional_trimmed_string(item, "encrypted_content")? else {
            return Ok(None);
        };
        let scope = optional_trimmed_string(item, "replay_scope")?;
        if scope.as_deref() != Some(replay.scope.as_str()) {
            return match replay.foreign_encrypted {
                ForeignEncryptedReplay::Drop => Ok(None),
                ForeignEncryptedReplay::Reject => Err(ResponsesFailure::contract(format!(
                    "encrypted reasoning replay scope {:?} does not match descriptor scope '{}'",
                    scope, replay.scope
                ))),
            };
        }
        let summary = normalize_reasoning_parts(item.get("summary"), "summary_text")?;
        let content = normalize_reasoning_parts(item.get("content"), "reasoning_text")?;
        let mut normalized = Map::new();
        normalized.insert("type".to_string(), Value::String("reasoning".to_string()));
        if !self.strip_item_ids.contains("reasoning") {
            if let Some(id) = optional_trimmed_string(item, "id")? {
                normalized.insert("id".to_string(), Value::String(id));
            }
        }
        normalized.insert("summary".to_string(), Value::Array(summary));
        if !content.is_empty() {
            normalized.insert("content".to_string(), Value::Array(content));
        }
        normalized.insert("encrypted_content".to_string(), Value::String(encrypted));
        Ok(Some(Value::Object(normalized)))
    }

    fn tool_result_item(
        &self,
        value: &Value,
        calls: &BTreeMap<String, ToolCallIdentity>,
    ) -> Result<Value, ResponsesFailure> {
        let result: ToolResultArtifact =
            serde_json::from_value(value.clone()).map_err(|error| {
                ResponsesFailure::contract(format!(
                    "tool result is not the strict shared envelope: {error}"
                ))
            })?;
        let correlation_id = result
            .correlation_id
            .as_deref()
            .ok_or_else(|| ResponsesFailure::contract("tool result has no correlation_id"))?;
        let call = calls.get(correlation_id).ok_or_else(|| {
            ResponsesFailure::contract(format!(
                "tool result correlation_id '{correlation_id}' has no bound tool.call artifact"
            ))
        })?;
        if call.tool != result.tool {
            return Err(ResponsesFailure::contract(format!(
                "tool result correlation_id '{correlation_id}' names '{}', bound call names '{}'",
                result.tool, call.tool
            )));
        }
        if encode_tool_name(&result.tool)? != call.wire_name {
            return Err(ResponsesFailure::contract(format!(
                "tool result '{correlation_id}' cannot reproduce its Responses wire name"
            )));
        }
        let output = if result.ok {
            let body = result
                .body
                .as_ref()
                .ok_or_else(|| ResponsesFailure::contract("successful tool result has no body"))?;
            if let Some(content) = body.get("content") {
                if let Some(text) = content.as_str() {
                    Value::String(text.to_string())
                } else {
                    let parts = content.as_array().ok_or_else(|| {
                        ResponsesFailure::contract(
                            "tool result body.content must be a string or content array",
                        )
                    })?;
                    let normalized = parts
                        .iter()
                        .enumerate()
                        .map(|(index, part)| {
                            self.normalize_content_part(part, "user", false, index)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    Value::Array(normalized)
                }
            } else {
                Value::String(render_json(body)?)
            }
        } else {
            let error = result
                .error
                .as_ref()
                .ok_or_else(|| ResponsesFailure::contract("failed tool result has no error"))?;
            Value::String(render_json(&json!({"error": error}))?)
        };
        Ok(json!({
            "type": "function_call_output",
            "call_id": call.call_id,
            "output": output,
        }))
    }

    fn materialize_response(
        &self,
        envelope: ResponsesEnvelope,
        output_root: &Path,
        tools: &ToolCatalog,
    ) -> Result<Vec<ProviderEmission>, ResponsesFailure> {
        let model_options = self.provider_model_options()?;
        let response = envelope.response.as_object().ok_or_else(|| {
            ResponsesFailure::contract("Responses terminal response is not an object")
        })?;
        let response_id = response
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.trim() == *value)
            .ok_or_else(|| ResponsesFailure::contract("Responses terminal response has no id"))?
            .to_string();
        let response_model = response
            .get("model")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.trim() == *value)
            .ok_or_else(|| ResponsesFailure::contract("Responses terminal response has no model"))?
            .to_string();
        let output = response
            .get("output")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ResponsesFailure::contract("Responses terminal response has no output array")
            })?;
        if output.is_empty() {
            return Err(ResponsesFailure::contract(
                "Responses terminal response output array is empty",
            ));
        }
        let usage = match response.get("usage") {
            Some(Value::Object(value)) => Some(value.clone()),
            None | Some(Value::Null) if !self.options.usage_required => None,
            None | Some(Value::Null) => {
                return Err(ResponsesFailure::contract(
                    "Responses terminal response is missing descriptor-required usage",
                ));
            }
            Some(_) => {
                return Err(ResponsesFailure::contract(
                    "Responses terminal response usage is not an object or null",
                ));
            }
        };
        let mut message_entries = Vec::new();
        let mut reasoning_entries = Vec::new();
        let mut native_entries = Vec::new();
        let mut tool_entries = Vec::new();
        let mut answer_text = Vec::new();
        let mut refusals = Vec::new();
        let mut reasoning_summaries = Vec::new();
        let mut reasoning_content = Vec::new();
        let mut call_ids = BTreeSet::new();
        for (output_index, item) in output.iter().enumerate() {
            let item_type = required_trimmed_string(item, "type", "Responses output item")?;
            if !self.response_item_types.contains(&item_type) {
                return Err(ResponsesFailure::contract(format!(
                    "Responses output item type '{item_type}' is not descriptor-declared"
                )));
            }
            let entry = response_item_evidence(output_index, item)?;
            match item_type.as_str() {
                "message" => {
                    let normalized = self.normalize_output_message(item)?;
                    extract_output_message_text(&normalized, &mut answer_text, &mut refusals)?;
                    message_entries.push(response_item_evidence(output_index, &normalized)?);
                }
                "reasoning" => {
                    let normalized = self.normalize_reasoning_output(item)?;
                    extract_reasoning_text(
                        &normalized,
                        &mut reasoning_summaries,
                        &mut reasoning_content,
                    )?;
                    reasoning_entries.push(response_item_evidence(output_index, &normalized)?);
                }
                "function_call" => {
                    let call_id = required_trimmed_string(item, "call_id", "function_call item")?;
                    if !call_ids.insert(call_id.clone()) {
                        return Err(ResponsesFailure::contract(format!(
                            "Responses output repeats function call_id '{call_id}'"
                        )));
                    }
                    let wire_name = required_trimmed_string(item, "name", "function_call item")?;
                    let tool = tools.by_wire_name.get(&wire_name).ok_or_else(|| {
                        ResponsesFailure::contract(format!(
                            "Responses model called undeclared function '{wire_name}'"
                        ))
                    })?;
                    let arguments_text =
                        required_trimmed_string(item, "arguments", "function_call item")?;
                    let arguments: Value =
                        serde_json::from_str(&arguments_text).map_err(|error| {
                            ResponsesFailure::contract(format!(
                                "Responses function '{wire_name}' arguments are not JSON: {error}"
                            ))
                        })?;
                    if !arguments.is_object() {
                        return Err(ResponsesFailure::contract(format!(
                            "Responses function '{wire_name}' arguments are not an object"
                        )));
                    }
                    let normalized = normalize_function_call_item(item, false, true)?;
                    tool_entries.push((
                        tool.clone(),
                        call_id,
                        arguments,
                        response_item_evidence(output_index, &normalized)?,
                        normalized
                            .get("id")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    ));
                }
                other if self.native_tool_output_types.contains(other) => {
                    native_entries.push((other.to_string(), entry));
                }
                other => {
                    return Err(ResponsesFailure::contract(format!(
                        "Responses output item type '{other}' is declared but has no executable materialization"
                    )));
                }
            }
        }
        let request_hash = blake3::hash(&envelope.request_bytes).to_hex().to_string();
        let transport_endpoint = self
            .websocket
            .as_ref()
            .map(|transport| transport.endpoint().as_str())
            .unwrap_or_else(|| self.endpoint.as_str());
        let transport = materialize_emission(
            output_root,
            TRANSPORT_EVIDENCE_LABEL,
            json!({
                "label": TRANSPORT_EVIDENCE_LABEL,
                "response_id": response_id,
                "requested_model": self.model,
                "response_model": response_model,
                "operation": {
                    "method": "post",
                    "endpoint": transport_endpoint,
                    "response_mode": match self.options.operation.response_mode {
                        ResponseMode::Sse => "sse",
                        ResponseMode::Json => "json",
                        ResponseMode::Websocket => "websocket",
                    }
                },
                "status": envelope.status,
                "headers": envelope.headers,
                "request_bytes": envelope.request_bytes.len(),
                "request_blake3": request_hash,
                "raw_request": raw_bytes_evidence(&envelope.request_bytes),
                "response_bytes": envelope.response_evidence.bytes,
                "response_blake3": envelope.response_evidence.blake3,
                "raw_response": envelope.response_evidence.raw.as_deref().map(raw_bytes_evidence),
                "stream_events": envelope.stream_events,
            }),
            ProviderEmissionRoute::ClientProjection,
        )?;
        let mut emissions = vec![transport];
        if let Some(usage) = usage {
            let usage_value = Value::Object(usage);
            emissions.push(materialize_emission(
                output_root,
                USAGE_EVIDENCE_LABEL,
                json!({
                    "label": USAGE_EVIDENCE_LABEL,
                    "response_id": response_id,
                    "model": response_model,
                    "usage": usage_value,
                    "raw_blake3": value_blake3(&usage_value)?,
                }),
                ProviderEmissionRoute::ClientProjection,
            )?);
        }
        if !reasoning_entries.is_empty() {
            emissions.push(materialize_emission(
                output_root,
                REASONING_EVIDENCE_LABEL,
                json!({
                    "label": REASONING_EVIDENCE_LABEL,
                    "response_id": response_id,
                    "model": response_model,
                    "replay_scope": model_options.replay.scope,
                    "summary_text": reasoning_summaries,
                    "reasoning_text": reasoning_content,
                    "response_items": reasoning_entries,
                }),
                ProviderEmissionRoute::ClientProjection,
            )?);
        }
        for (item_type, entry) in native_entries {
            emissions.push(materialize_emission(
                output_root,
                NATIVE_TOOL_EVIDENCE_LABEL,
                json!({
                    "label": NATIVE_TOOL_EVIDENCE_LABEL,
                    "response_id": response_id,
                    "model": response_model,
                    "native_tool_output_type": item_type,
                    "response_items": [entry],
                }),
                ProviderEmissionRoute::ClientProjection,
            )?);
        }
        emissions.push(materialize_emission(
            output_root,
            INTERFACE_ARTIFACT_OUTPUT_LABEL,
            json!({
                "label": INTERFACE_ARTIFACT_OUTPUT_LABEL,
                "response_id": response_id,
                "model": response_model,
                "status": "completed",
                "text": if answer_text.is_empty() { Value::Null } else { Value::String(answer_text.join("\n")) },
                "refusals": refusals,
                "response_items": message_entries,
            }),
            ProviderEmissionRoute::ClientProjection,
        )?);
        for (tool, call_id, arguments, response_item, response_item_id) in tool_entries {
            emissions.push(materialize_emission(
                output_root,
                TOOL_CALL_LABEL,
                json!({
                    "label": TOOL_CALL_LABEL,
                    "response_id": response_id,
                    "model": response_model,
                    "tool": tool,
                    "arguments": arguments,
                    "correlation_id": call_id,
                    "provider_call_id": call_id,
                    "response_item_id": response_item_id,
                    "response_items": [response_item],
                }),
                ProviderEmissionRoute::Tool { tool },
            )?);
        }
        Ok(emissions)
    }

    fn normalize_output_message(&self, item: &Value) -> Result<Value, ResponsesFailure> {
        let encrypted_replay = self.provider_model_options()?.replay.encrypted_content;
        let object = item.as_object().ok_or_else(|| {
            ResponsesFailure::contract("Responses output message is not an object")
        })?;
        let allowed = ["type", "id", "role", "status", "phase", "content"]
            .into_iter()
            .collect::<BTreeSet<_>>();
        if let Some(unknown) = object.keys().find(|key| !allowed.contains(key.as_str())) {
            return Err(ResponsesFailure::contract(format!(
                "Responses output message has unknown field '{unknown}'"
            )));
        }
        if required_trimmed_string(item, "role", "Responses output message")? != "assistant" {
            return Err(ResponsesFailure::contract(
                "Responses output message role must be assistant",
            ));
        }
        validate_optional_item_status(item, "Responses output message")?;
        let content = item
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ResponsesFailure::contract("Responses output message has no content array")
            })?;
        if content.is_empty() {
            return Err(ResponsesFailure::contract(
                "Responses output message content array is empty",
            ));
        }
        let normalized_content = content
            .iter()
            .enumerate()
            .map(|(index, part)| {
                let part = part.as_object().ok_or_else(|| {
                    ResponsesFailure::contract(format!(
                        "Responses output message content[{index}] is not an object"
                    ))
                })?;
                let part_type = part
                    .get("type")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty() && value.trim() == *value)
                    .ok_or_else(|| {
                        ResponsesFailure::contract(format!(
                            "Responses output message content[{index}] has no type"
                        ))
                    })?;
                if !self.response_content_types.contains(part_type) {
                    return Err(ResponsesFailure::contract(format!(
                        "Responses output content type '{part_type}' is not descriptor-declared"
                    )));
                }
                normalize_output_content_part(part, part_type, encrypted_replay)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut normalized = object.clone();
        normalized.insert("content".to_string(), Value::Array(normalized_content));
        Ok(Value::Object(normalized))
    }

    fn normalize_reasoning_output(&self, item: &Value) -> Result<Value, ResponsesFailure> {
        let replay = &self.provider_model_options()?.replay;
        let object = item.as_object().ok_or_else(|| {
            ResponsesFailure::contract("Responses reasoning output is not an object")
        })?;
        ensure_only_fields(
            object,
            &[
                "type",
                "id",
                "status",
                "summary",
                "content",
                "encrypted_content",
            ],
            "Responses reasoning output",
        )?;
        validate_optional_item_status(item, "Responses reasoning output")?;
        let summary = normalize_reasoning_parts(item.get("summary"), "summary_text")?;
        let content = normalize_reasoning_parts(item.get("content"), "reasoning_text")?;
        let encrypted = optional_trimmed_string(item, "encrypted_content")?;
        if encrypted.is_some() && !replay.encrypted_reasoning {
            return Err(ResponsesFailure::contract(
                "Responses returned encrypted reasoning but descriptor replay disables it",
            ));
        }
        let mut normalized = object.clone();
        normalized.insert("summary".to_string(), Value::Array(summary));
        if item.get("content").is_some() {
            normalized.insert("content".to_string(), Value::Array(content));
        }
        normalized.insert(
            "replay_scope".to_string(),
            Value::String(replay.scope.clone()),
        );
        Ok(Value::Object(normalized))
    }
}

fn required_formed_models(
    input: &InterfaceInvocationEnvelope,
) -> Result<BTreeSet<String>, ResponsesFailure> {
    let policy = input
        .interface_policy()
        .map_err(|error| ResponsesFailure::contract(error.to_string()))?
        .ok_or_else(|| {
            ResponsesFailure::contract("Responses invocation carries no formed interface policy")
        })?;
    let values = policy
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ResponsesFailure::contract(
                "Responses formed interface policy models must be a string array",
            )
        })?;
    if values.is_empty() {
        return Err(ResponsesFailure::contract(
            "Responses formed interface policy models is empty",
        ));
    }
    let mut models = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let model = value.as_str().filter(|model| {
            !model.is_empty() && model.trim() == *model && !model.contains(['\r', '\n', '\0'])
        });
        let Some(model) = model else {
            return Err(ResponsesFailure::contract(format!(
                "Responses formed interface policy models[{index}] is not a trimmed string"
            )));
        };
        if !models.insert(model.to_string()) {
            return Err(ResponsesFailure::contract(format!(
                "Responses formed interface policy models repeats '{model}'"
            )));
        }
    }
    Ok(models)
}

fn filter_model_catalog(
    models: &[Value],
    id_pointer: &str,
    formed_models: &BTreeSet<String>,
) -> Vec<Value> {
    models
        .iter()
        .filter(|model| {
            model
                .pointer(id_pointer)
                .and_then(Value::as_str)
                .is_some_and(|id| formed_models.contains(id))
        })
        .cloned()
        .collect()
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
            AuthSource::None | AuthSource::Environment(_) => None,
        }
    }

    async fn header_pair(&self) -> Result<Option<(HeaderName, HeaderValue)>, ResponsesFailure> {
        let secret = match &self.source {
            AuthSource::None => return Ok(None),
            AuthSource::Environment(name) => std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    ResponsesFailure::unavailable(format!(
                        "Responses auth env var '{name}' is unset or empty"
                    ))
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
                ResponsesFailure::unavailable(format!(
                    "Responses secret '{}/{}' could not be loaded: {error}",
                    scope.name().unwrap_or("global"),
                    name
                ))
            })?
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                ResponsesFailure::unavailable(format!(
                    "Responses secret '{}/{}' is not configured",
                    scope.name().unwrap_or("global"),
                    name
                ))
            })?,
            AuthSource::AzureEntra(manager) => manager
                .access_token()
                .await
                .map_err(ResponsesFailure::authentication)?,
        };
        let header = self.header.clone().ok_or_else(|| {
            ResponsesFailure::contract("Responses authenticated transport has no auth header")
        })?;
        let value =
            HeaderValue::from_str(&format!("{}{secret}", self.value_prefix)).map_err(|_| {
                ResponsesFailure::unavailable(
                    "Responses secret cannot form a valid auth header value",
                )
            })?;
        Ok(Some((header, value)))
    }

    async fn invalidate_azure_authentication(&self) -> bool {
        if let AuthSource::AzureEntra(manager) = &self.source {
            manager.invalidate().await;
            return true;
        }
        false
    }

    async fn refresh_azure_authentication(&self) -> Result<(), ResponsesFailure> {
        let AuthSource::AzureEntra(manager) = &self.source else {
            return Err(ResponsesFailure::contract(
                "Responses requested Azure Entra refresh without descriptor-selected Azure Entra auth",
            ));
        };
        manager
            .access_token()
            .await
            .map(|_| ())
            .map_err(ResponsesFailure::authentication)
    }
}

pub fn materialize_provider_turn_error(
    output_root: &Path,
    failure: &ResponsesFailure,
) -> Result<InterfaceArtifactFile, ResponsesFailure> {
    if failure.detail.trim().is_empty() {
        return Err(ResponsesFailure::contract(
            "Responses failure detail must be non-empty",
        ));
    }
    let payload = serde_json::to_vec(&json!({
        "label": TURN_ERROR_LABEL,
        "class": failure.class,
        "retryable": failure.retryable,
        "status": failure.status,
        "code": failure.code,
        "message": failure.detail,
        "evidence": failure.evidence,
    }))
    .map_err(|error| {
        ResponsesFailure::contract(format!("Responses turn.error did not encode: {error}"))
    })?;
    InterfaceArtifactFile::write(output_root, TURN_ERROR_LABEL, &payload)
        .map_err(|error| ResponsesFailure::contract(error.to_string()))
}

impl ResponsesOptions {
    fn base_model_options(&self) -> ResponsesModelOptions {
        ResponsesModelOptions {
            request_fields: self.request_fields.clone(),
            cache: self.cache.clone(),
            replay: self.replay.clone(),
        }
    }

    fn apply_model_profile(&self, profile: &ResponsesModelProfile) -> ResponsesModelOptions {
        let mut resolved = self.base_model_options();
        if let Some(request_fields) = &profile.request_fields {
            resolved.request_fields = request_fields.clone();
        }
        if let Some(cache) = &profile.cache {
            resolved.cache = Some(cache.clone());
        }
        if let Some(replay) = &profile.replay {
            resolved.replay = replay.clone();
        }
        resolved
    }

    fn for_provider_model(&self, model: &str) -> Result<ResponsesModelOptions, String> {
        if self.model_profiles.is_empty() {
            return Ok(self.base_model_options());
        }
        self.model_profiles
            .get(model)
            .map(|profile| self.apply_model_profile(profile))
            .ok_or_else(|| {
                format!("responses_api selected model '{model}' has no exact model_profiles entry")
            })
    }
}

impl OpenAiResponsesDriver {
    fn provider_model_options(&self) -> Result<&ResponsesModelOptions, ResponsesFailure> {
        self.model_options.as_ref().map_err(|error| {
            ResponsesFailure::contract(format!("Responses provider model is unavailable: {error}"))
        })
    }
}

fn validate_options(options: &ResponsesOptions) -> Result<(), String> {
    validate_path(&options.operation.path, "responses_api.operation.path")?;
    if options.operation.method != HttpMethod::Post {
        return Err("Responses operation.method must be post".to_string());
    }
    if let Some(cache) = &options.cache {
        if cache.field != "prompt_cache_key" {
            return Err(
                "Responses cache.field must be the native prompt_cache_key request field"
                    .to_string(),
            );
        }
        if cache.prefix.is_empty()
            || cache.prefix.trim() != cache.prefix
            || !(8..=64).contains(&cache.digest_characters)
        {
            return Err(
                "Responses cache requires a non-empty trimmed prefix and digest_characters within 8..=64"
                    .to_string(),
            );
        }
    }
    if options.retry.backoff_ms == 0
        || options.retry.max_backoff_ms < options.retry.backoff_ms
        || options.retry.http_statuses.is_empty()
    {
        return Err(
            "Responses retry requires positive backoff, max_backoff >= backoff and explicit statuses"
                .to_string(),
        );
    }
    let mut retry_statuses = BTreeSet::new();
    for status in &options.retry.http_statuses {
        if !(400..=599).contains(status) || !retry_statuses.insert(*status) {
            return Err(format!(
                "Responses retry.http_statuses contains invalid or repeated status {status}"
            ));
        }
    }
    let protected = [
        "model",
        "instructions",
        "input",
        "tools",
        "tool_choice",
        "parallel_tool_calls",
        "stream",
        "type",
        "prompt_cache_key",
    ];
    if let Some(key) = protected
        .iter()
        .find(|key| options.request_fields.contains_key(**key))
    {
        return Err(format!(
            "Responses request_fields cannot override driver-owned '{key}'"
        ));
    }
    if options
        .request_fields
        .get("store")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err("Responses request_fields.store must be boolean when declared".to_string());
    }
    if options.replay.encrypted_reasoning {
        let includes_encrypted = options
            .request_fields
            .get("include")
            .and_then(Value::as_array)
            .is_some_and(|values| {
                values
                    .iter()
                    .any(|value| value.as_str() == Some("reasoning.encrypted_content"))
            });
        if !includes_encrypted {
            return Err(
                "Responses encrypted reasoning replay requires request_fields.include to declare reasoning.encrypted_content"
                    .to_string(),
            );
        }
    }
    if let Some(tool_choice) = &options.function_tools.tool_choice {
        match tool_choice {
            Value::String(value) if matches!(value.as_str(), "auto" | "required" | "none") => {}
            Value::Object(_) => {}
            _ => {
                return Err(
                    "Responses function_tools.tool_choice must be auto, required, none or an object"
                        .to_string(),
                );
            }
        }
    }
    let native_types = strict_string_set(
        &options.native_tool_types,
        "responses_api.native_tool_types",
    )?;
    let native_outputs = strict_string_set(
        &options.native_tool_output_types,
        "responses_api.native_tool_output_types",
    )?;
    let response_types = strict_string_set(
        &options.response_item_types,
        "responses_api.response_item_types",
    )?;
    for required in ["message", "reasoning", "function_call"] {
        if !response_types.contains(required) {
            return Err(format!(
                "Responses response_item_types must declare '{required}'"
            ));
        }
    }
    if !native_outputs.is_subset(&response_types) {
        return Err(
            "Responses native_tool_output_types must be a subset of response_item_types"
                .to_string(),
        );
    }
    for (index, tool) in options.native_tools.iter().enumerate() {
        let object = tool
            .as_object()
            .ok_or_else(|| format!("Responses native_tools[{index}] must be an object"))?;
        let tool_type = object
            .get("type")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.trim() == *value)
            .ok_or_else(|| format!("Responses native_tools[{index}] has no type"))?;
        if tool_type == "function" || !native_types.contains(tool_type) {
            return Err(format!(
                "Responses native_tools[{index}] type '{tool_type}' is not declared by native_tool_types"
            ));
        }
    }
    if options.native_tools.is_empty() && (!native_types.is_empty() || !native_outputs.is_empty()) {
        return Err(
            "Responses native tool type/output declarations require at least one native_tools entry"
                .to_string(),
        );
    }
    if !options.native_tools.is_empty() && (native_types.is_empty() || native_outputs.is_empty()) {
        return Err(
            "Responses native_tools require explicit native tool and output type declarations"
                .to_string(),
        );
    }
    let response_content = strict_string_set(
        &options.response_content_types,
        "responses_api.response_content_types",
    )?;
    if !response_content.contains("output_text") || !response_content.contains("refusal") {
        return Err(
            "Responses response_content_types must declare output_text and refusal".to_string(),
        );
    }
    if options.replay.scope.is_empty() || options.replay.scope.trim() != options.replay.scope {
        return Err("Responses replay.scope must be non-empty and trimmed".to_string());
    }
    let strip_ids = strict_string_set(
        &options.replay.strip_item_ids,
        "responses_api.replay.strip_item_ids",
    )?;
    if !strip_ids.is_subset(&response_types) {
        return Err(
            "Responses replay.strip_item_ids must be a subset of response_item_types".to_string(),
        );
    }
    validate_carrier_policy(&options.carrier_policy)?;
    if let Some(config) = &options.model_catalog {
        validate_model_catalog(config)?;
    }
    strict_string_set(&options.evidence_headers, "responses_api.evidence_headers")?;
    for (model, profile) in &options.model_profiles {
        if model.is_empty() || model.trim() != model || model.contains(['\r', '\n', '\0']) {
            return Err(
                "responses_api.model_profiles keys must be non-empty trimmed single-line model names"
                    .to_string(),
            );
        }
        validate_responses_model_options(&options.apply_model_profile(profile), &response_types)
            .map_err(|error| format!("responses_api.model_profiles['{model}']: {error}"))?;
    }
    Ok(())
}

fn validate_responses_model_options(
    options: &ResponsesModelOptions,
    response_types: &BTreeSet<String>,
) -> Result<(), String> {
    if let Some(cache) = &options.cache {
        if cache.field != "prompt_cache_key" {
            return Err(
                "Responses cache.field must be the native prompt_cache_key request field"
                    .to_string(),
            );
        }
        if cache.prefix.is_empty()
            || cache.prefix.trim() != cache.prefix
            || !(8..=64).contains(&cache.digest_characters)
        {
            return Err(
                "Responses cache requires a non-empty trimmed prefix and digest_characters within 8..=64"
                    .to_string(),
            );
        }
    }
    let protected = [
        "model",
        "instructions",
        "input",
        "tools",
        "tool_choice",
        "parallel_tool_calls",
        "stream",
        "type",
        "prompt_cache_key",
    ];
    if let Some(key) = protected
        .iter()
        .find(|key| options.request_fields.contains_key(**key))
    {
        return Err(format!(
            "Responses request_fields cannot override driver-owned '{key}'"
        ));
    }
    if options
        .request_fields
        .get("store")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err("Responses request_fields.store must be boolean when declared".to_string());
    }
    if options.replay.encrypted_reasoning {
        let includes_encrypted = options
            .request_fields
            .get("include")
            .and_then(Value::as_array)
            .is_some_and(|values| {
                values
                    .iter()
                    .any(|value| value.as_str() == Some("reasoning.encrypted_content"))
            });
        if !includes_encrypted {
            return Err(
                "Responses encrypted reasoning replay requires request_fields.include to declare reasoning.encrypted_content"
                    .to_string(),
            );
        }
    }
    if options.replay.scope.is_empty() || options.replay.scope.trim() != options.replay.scope {
        return Err("Responses replay.scope must be non-empty and trimmed".to_string());
    }
    let strip_ids = strict_string_set(
        &options.replay.strip_item_ids,
        "responses_api.replay.strip_item_ids",
    )?;
    if !strip_ids.is_subset(response_types) {
        return Err(
            "Responses replay.strip_item_ids must be a subset of response_item_types".to_string(),
        );
    }
    Ok(())
}

fn validate_carrier_policy(policy: &CarrierPolicy) -> Result<(), String> {
    if policy.max_inline_file_bytes == 0
        || policy.max_inline_file_bytes > ABSOLUTE_MAX_INLINE_FILE_BYTES
    {
        return Err(format!(
            "Responses max_inline_file_bytes must be within 1..={ABSOLUTE_MAX_INLINE_FILE_BYTES}"
        ));
    }
    let roles = strict_string_set(&policy.allowed_roles, "carrier_policy.allowed_roles")?;
    if !roles.is_subset(
        &["user", "assistant", "system", "developer"]
            .into_iter()
            .map(str::to_string)
            .collect(),
    ) {
        return Err("Responses carrier policy declares an unsupported role".to_string());
    }
    let item_types = strict_string_set(
        &policy.allowed_input_item_types,
        "carrier_policy.allowed_input_item_types",
    )?;
    if !item_types.is_subset(&["message"].into_iter().map(str::to_string).collect()) {
        return Err("Responses carrier policy declares an unsupported input item type".to_string());
    }
    let content_types = strict_string_set(
        &policy.allowed_content_types,
        "carrier_policy.allowed_content_types",
    )?;
    if !content_types.is_subset(
        &[
            "input_text",
            "output_text",
            "input_image",
            "input_file",
            "encrypted_content",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
    ) {
        return Err("Responses carrier policy declares an unsupported content type".to_string());
    }
    let details = strict_string_set(
        &policy.allowed_image_details,
        "carrier_policy.allowed_image_details",
    )?;
    if !details.is_subset(
        &["auto", "low", "high"]
            .into_iter()
            .map(str::to_string)
            .collect(),
    ) {
        return Err("Responses carrier policy declares an unsupported image detail".to_string());
    }
    let file_details = strict_string_set(
        &policy.allowed_file_details,
        "carrier_policy.allowed_file_details",
    )?;
    if !file_details.is_subset(
        &["auto", "low", "high"]
            .into_iter()
            .map(str::to_string)
            .collect(),
    ) {
        return Err("Responses carrier policy declares an unsupported file detail".to_string());
    }
    validate_uri_schemes(
        &policy.allowed_image_url_schemes,
        "carrier_policy.allowed_image_url_schemes",
    )?;
    validate_uri_schemes(
        &policy.allowed_file_url_schemes,
        "carrier_policy.allowed_file_url_schemes",
    )?;
    if policy.rules.is_empty() {
        return Err("Responses carrier policy requires at least one rule".to_string());
    }
    let mut labels = BTreeSet::new();
    for rule in &policy.rules {
        let label = rule.label();
        if label.is_empty() || label.trim() != label || !labels.insert(label.to_string()) {
            return Err(format!(
                "Responses carrier policy has invalid or repeated label '{label}'"
            ));
        }
        match rule {
            CarrierRule::JsonText {
                role, text_pointer, ..
            } => {
                if !roles.contains(role) {
                    return Err(format!(
                        "Responses json_text carrier '{label}' role '{role}' is not admitted"
                    ));
                }
                validate_json_pointer(text_pointer, "text_pointer")?;
            }
            CarrierRule::ResponsesItems { items_pointer, .. } => {
                validate_json_pointer(items_pointer, "items_pointer")?;
            }
        }
    }
    Ok(())
}

fn validate_model_catalog(config: &ModelCatalogConfig) -> Result<(), String> {
    validate_path(&config.path, "responses_api.model_catalog.path")?;
    for (name, value) in [
        ("request_label", config.request_label.as_str()),
        ("output_label", config.output_label.as_str()),
    ] {
        if value.is_empty() || value.trim() != value {
            return Err(format!(
                "Responses model_catalog.{name} must be non-empty and trimmed"
            ));
        }
    }
    if config.request_label == config.output_label {
        return Err("Responses model catalog request and output labels must differ".to_string());
    }
    validate_json_pointer(&config.models_pointer, "models_pointer")?;
    validate_json_pointer(&config.id_pointer, "id_pointer")?;
    Ok(())
}

fn validate_json_pointer(value: &str, field: &str) -> Result<(), String> {
    if !value.starts_with('/') || value.trim() != value {
        return Err(format!(
            "Responses {field} must be a trimmed RFC 6901 JSON pointer"
        ));
    }
    Ok(())
}

fn validate_path(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty()
        || value.trim() != value
        || !value.starts_with('/')
        || value.contains('?')
        || value.contains('#')
    {
        return Err(format!(
            "Responses {field} must be a trimmed absolute path without query or fragment"
        ));
    }
    Ok(())
}

fn strict_string_set(values: &[String], field: &str) -> Result<BTreeSet<String>, String> {
    let mut output = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        if value.is_empty() || value.trim() != value {
            return Err(format!(
                "Responses {field}[{index}] must be non-empty and trimmed"
            ));
        }
        if !output.insert(value.clone()) {
            return Err(format!("Responses {field} repeats '{value}'"));
        }
    }
    Ok(output)
}

fn validate_uri_schemes(values: &[String], field: &str) -> Result<(), String> {
    let values = strict_string_set(values, field)?;
    if values.is_empty() {
        return Err(format!("Responses {field} must not be empty"));
    }
    for value in values {
        if !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.')
        }) {
            return Err(format!("Responses {field} has invalid scheme '{value}'"));
        }
    }
    Ok(())
}

fn metadata_enum<T: for<'de> Deserialize<'de>>(
    metadata: &Map<String, Value>,
    key: &str,
) -> Result<T, String> {
    serde_json::from_value(
        metadata
            .get(key)
            .cloned()
            .ok_or_else(|| format!("Responses private_config.{key} is missing"))?,
    )
    .map_err(|error| format!("Responses private_config.{key} is malformed: {error}"))
}

fn validate_process_env_names(
    metadata: &Map<String, Value>,
    transport: &InterfaceTransportConfig,
    base_source: ValueSource,
    auth_source: AuthSourceSelection,
) -> Result<(), String> {
    let values = metadata
        .get("process_env_names")
        .and_then(Value::as_array)
        .ok_or_else(|| "Responses private_config.process_env_names must be an array".to_string())?;
    let mut declared = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let name = value
            .as_str()
            .filter(|name| !name.is_empty() && name.trim() == *name)
            .ok_or_else(|| {
                format!("Responses process_env_names[{index}] must be a non-empty trimmed string")
            })?;
        if !declared.insert(name.to_string()) {
            return Err(format!("Responses process_env_names repeats '{name}'"));
        }
    }
    let mut expected = BTreeSet::new();
    match base_source {
        ValueSource::Descriptor => {
            if transport
                .base_url_env
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            {
                return Err(
                    "Responses descriptor base_url_source cannot declare base_url_env".to_string(),
                );
            }
        }
        ValueSource::Environment => {
            let name = transport
                .base_url_env
                .as_deref()
                .filter(|value| !value.is_empty() && value.trim() == *value)
                .ok_or_else(|| {
                    "Responses environment base_url_source requires base_url_env".to_string()
                })?;
            if !transport.base_url.is_empty() {
                return Err(
                    "Responses environment base_url_source requires an empty descriptor base_url"
                        .to_string(),
                );
            }
            expected.insert(name.to_string());
        }
    }
    match auth_source {
        AuthSourceSelection::None => {
            if transport.auth_env_key.is_some() || transport.auth_secret_ref.is_some() {
                return Err(
                    "Responses none auth_source cannot declare auth_env_key or auth_secret_ref"
                        .to_string(),
                );
            }
        }
        AuthSourceSelection::Secret => {
            if transport.auth_env_key.is_some() {
                return Err("Responses secret auth_source cannot declare auth_env_key".to_string());
            }
        }
        AuthSourceSelection::Environment => {
            let name = transport
                .auth_env_key
                .as_deref()
                .filter(|value| !value.is_empty() && value.trim() == *value)
                .ok_or_else(|| {
                    "Responses environment auth_source requires auth_env_key".to_string()
                })?;
            if transport.auth_secret_ref.is_some() {
                return Err(
                    "Responses environment auth_source cannot declare auth_secret_ref".to_string(),
                );
            }
            expected.insert(name.to_string());
        }
        AuthSourceSelection::AzureEntra => {
            if transport.auth_env_key.is_some() || transport.auth_secret_ref.is_some() {
                return Err(
                    "Responses Azure Entra auth_source cannot declare auth_env_key or auth_secret_ref"
                        .to_string(),
                );
            }
        }
    }
    if let Some(name) = metadata
        .get("model_env")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.trim() == *value)
    {
        expected.insert(name.to_string());
    }
    if declared != expected {
        return Err(format!(
            "Responses process_env_names {declared:?} must exactly match selected transport env names {expected:?}"
        ));
    }
    Ok(())
}

fn resolve_base_url(
    transport: &InterfaceTransportConfig,
    source: ValueSource,
) -> Result<String, String> {
    let raw = match source {
        ValueSource::Descriptor => {
            if transport.base_url.is_empty() {
                return Err(
                    "Responses descriptor base_url_source requires transport.base_url".to_string(),
                );
            }
            transport.base_url.clone()
        }
        ValueSource::Environment => {
            let name = transport.base_url_env.as_deref().ok_or_else(|| {
                "Responses environment base_url_source has no base_url_env".to_string()
            })?;
            std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| format!("Responses base URL env var '{name}' is unset or empty"))?
        }
    };
    validate_base_url(&raw)?;
    Ok(raw.trim_end_matches('/').to_string())
}

fn validate_base_url(raw: &str) -> Result<(), String> {
    if raw.is_empty() || raw.trim() != raw || raw.ends_with('/') {
        return Err(
            "Responses base_url must be non-empty, trimmed and have no trailing slash".to_string(),
        );
    }
    let url = reqwest::Url::parse(raw)
        .map_err(|error| format!("Responses base_url is invalid: {error}"))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Responses base_url cannot contain credentials, query or fragment".to_string());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "Responses base_url has no host".to_string())?;
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    match url.scheme() {
        "https" => Ok(()),
        "http" if loopback => Ok(()),
        "http" => Err("Responses base_url requires HTTPS outside loopback".to_string()),
        scheme => Err(format!(
            "Responses base_url scheme '{scheme}' is unsupported"
        )),
    }
}

fn resolve_endpoint(base_url: &str, path: &str) -> Result<reqwest::Url, String> {
    validate_path(path, "endpoint path")?;
    reqwest::Url::parse(&format!("{base_url}{path}"))
        .map_err(|error| format!("Responses endpoint is invalid: {error}"))
}

fn resolve_auth(
    descriptor: &InterfaceDescriptor,
    transport: &InterfaceTransportConfig,
    source: AuthSourceSelection,
    activation_secrets_home: PathBuf,
) -> Result<ProviderAuth, String> {
    if source == AuthSourceSelection::None {
        if transport.auth_header.is_some()
            || transport.auth_value_prefix.is_some()
            || transport.auth_env_key.is_some()
            || transport.auth_secret_ref.is_some()
        {
            return Err(
                "Responses none auth_source cannot declare auth header, prefix, env or secret fields"
                    .to_string(),
            );
        }
        return Ok(ProviderAuth {
            header: None,
            value_prefix: String::new(),
            source: AuthSource::None,
        });
    }
    let header_name = transport
        .auth_header
        .as_deref()
        .filter(|value| !value.is_empty() && value.trim() == *value)
        .ok_or_else(|| "Responses transport.auth_header is required".to_string())?;
    let header = HeaderName::from_bytes(header_name.as_bytes())
        .map_err(|error| format!("Responses auth_header is invalid: {error}"))?;
    let value_prefix = transport
        .auth_value_prefix
        .as_ref()
        .ok_or_else(|| "Responses transport.auth_value_prefix is required".to_string())?
        .clone();
    if value_prefix.contains('\r') || value_prefix.contains('\n') {
        return Err("Responses auth_value_prefix contains a line break".to_string());
    }
    let source = match source {
        AuthSourceSelection::None => unreachable!(),
        AuthSourceSelection::Secret => {
            if transport.auth_env_key.is_some() {
                return Err("Responses secret auth_source cannot declare auth_env_key".to_string());
            }
            let reference = transport
                .auth_secret_ref
                .as_deref()
                .filter(|value| !value.is_empty() && value.trim() == *value)
                .ok_or_else(|| {
                    "Responses secret auth_source requires auth_secret_ref".to_string()
                })?;
            let (scope, name) = parse_secret_ref(reference)?;
            if scope.name() != Some(descriptor.id.as_str()) {
                return Err(format!(
                    "Responses interface '{}' auth_secret_ref must use its exact interface scope",
                    descriptor.id
                ));
            }
            AuthSource::Secret {
                octopus_home: activation_secrets_home,
                scope,
                name,
            }
        }
        AuthSourceSelection::Environment => {
            if transport.auth_secret_ref.is_some() {
                return Err(
                    "Responses environment auth_source cannot declare auth_secret_ref".to_string(),
                );
            }
            let name = transport
                .auth_env_key
                .as_deref()
                .filter(|value| !value.is_empty() && value.trim() == *value)
                .ok_or_else(|| {
                    "Responses environment auth_source requires auth_env_key".to_string()
                })?;
            AuthSource::Environment(name.to_string())
        }
        AuthSourceSelection::AzureEntra => {
            if transport.auth_secret_ref.is_some() || transport.auth_env_key.is_some() {
                return Err(
                    "Responses Azure Entra auth_source cannot declare auth_secret_ref or auth_env_key"
                        .to_string(),
                );
            }
            let manager =
                AzureEntraCredentialManager::from_descriptor(descriptor, &activation_secrets_home)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| {
                        "Responses azure_entra auth_source requires private_config.azure_entra"
                            .to_string()
                    })?;
            AuthSource::AzureEntra(Box::new(manager))
        }
    };
    Ok(ProviderAuth {
        header: Some(header),
        value_prefix,
        source,
    })
}

fn resolve_model(metadata: &Map<String, Value>) -> Result<String, String> {
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
                format!("Responses model env var '{name}' is unset, empty or untrimmed")
            }),
        (Some(_), Some(_)) => {
            Err("Responses descriptor cannot declare both model and model_env".to_string())
        }
        (None, None) => {
            Err("Responses descriptor must declare exactly one of model or model_env".to_string())
        }
    }
}

fn parse_secret_ref(reference: &str) -> Result<(SecretScope, SecretName), String> {
    let path = reference
        .strip_prefix("secret://")
        .ok_or_else(|| "Responses auth_secret_ref must start with secret://".to_string())?;
    let (scope, name) = path
        .split_once('/')
        .ok_or_else(|| "Responses auth_secret_ref must be secret://<scope>/<name>".to_string())?;
    if name.contains('/') {
        return Err("Responses auth_secret_ref must name one scoped secret".to_string());
    }
    Ok((
        SecretScope::named(scope.to_string()).map_err(|error| error.to_string())?,
        SecretName::new(name).map_err(|error| error.to_string())?,
    ))
}

fn resolve_headers(
    transport: &InterfaceTransportConfig,
    auth_header: Option<&HeaderName>,
) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    for (name, value) in &transport.headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|error| format!("Responses header name is invalid: {error}"))?;
        if auth_header.is_some_and(|auth_header| name == *auth_header) || is_secret_header(&name) {
            return Err(format!(
                "Responses secret-bearing header '{name}' must come from the declared auth surface"
            ));
        }
        if name == reqwest::header::CONTENT_TYPE || name == reqwest::header::ACCEPT {
            return Err(format!(
                "Responses protocol-owned header '{name}' cannot be a static transport header"
            ));
        }
        let value = HeaderValue::from_str(value)
            .map_err(|error| format!("Responses header value is invalid: {error}"))?;
        headers.insert(name, value);
    }
    Ok(headers)
}

fn is_secret_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str().to_ascii_lowercase().as_str(),
        "authorization" | "proxy-authorization" | "x-api-key" | "api-key" | "cookie"
    )
}

fn resolve_evidence_headers(values: &[String]) -> Result<Vec<HeaderName>, String> {
    let mut output = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let name = HeaderName::from_bytes(value.as_bytes())
            .map_err(|error| format!("Responses evidence_headers[{index}] is invalid: {error}"))?;
        if is_secret_header(&name) || name == reqwest::header::SET_COOKIE {
            return Err(format!(
                "Responses evidence_headers cannot capture secret-bearing '{name}'"
            ));
        }
        if !seen.insert(name.as_str().to_string()) {
            return Err(format!("Responses evidence_headers repeats '{name}'"));
        }
        output.push(name);
    }
    Ok(output)
}

fn capture_response_headers(headers: &HeaderMap, names: &[HeaderName]) -> Value {
    let mut captured = Map::new();
    for name in names {
        if let Some(value) = headers.get(name) {
            if let Ok(value) = value.to_str() {
                captured.insert(name.as_str().to_string(), Value::String(value.to_string()));
            }
        }
    }
    Value::Object(captured)
}

async fn read_bounded(
    mut response: reqwest::Response,
    maximum: usize,
    streamed: bool,
) -> Result<Vec<u8>, ResponsesFailure> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(ResponsesFailure::response_limit(format!(
            "Responses body exceeds descriptor maximum {maximum} bytes"
        )));
    }
    let mut bytes = Vec::new();
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) if streamed && !bytes.is_empty() => {
                return Err(ResponsesFailure::partial_stream(
                    format!("Responses stream body read failed after bytes arrived: {error}"),
                    &bytes,
                ));
            }
            Err(error) => {
                return Err(ResponsesFailure::transport(format!(
                    "Responses body read failed: {error}"
                )));
            }
        };
        let next = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| ResponsesFailure::response_limit("Responses body size overflowed"))?;
        if next > maximum {
            return Err(ResponsesFailure::response_limit(format!(
                "Responses body exceeds descriptor maximum {maximum} bytes"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn read_responses_sse(
    mut response: reqwest::Response,
    maximum: usize,
    sequence_mode: SequenceNumbers,
    status: u16,
    headers: &Value,
) -> Result<(ParsedStream, ResponseBodyEvidence), ResponsesFailure> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(ResponsesFailure::response_limit(format!(
            "Responses body exceeds descriptor maximum {maximum} bytes"
        )));
    }
    let mut decoder = ResponsesSseDecoder::new(sequence_mode);
    let mut bytes = 0usize;
    let mut hasher = blake3::Hasher::new();
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) if bytes > 0 => {
                return Err(ResponsesFailure::partial_stream_evidence(
                    format!("Responses stream body read failed after bytes arrived: {error}"),
                    bytes,
                    hasher.finalize().to_hex().to_string(),
                ));
            }
            Err(error) => {
                return Err(ResponsesFailure::transport(format!(
                    "Responses body read failed: {error}"
                )));
            }
        };
        bytes = bytes
            .checked_add(chunk.len())
            .ok_or_else(|| ResponsesFailure::response_limit("Responses body size overflowed"))?;
        hasher.update(&chunk);
        if bytes > maximum {
            return Err(ResponsesFailure::partial_stream_evidence(
                format!("Responses SSE exceeds descriptor maximum {maximum} bytes"),
                bytes,
                hasher.finalize().to_hex().to_string(),
            ));
        }
        if let Err(detail) = decoder.push(&chunk) {
            return Err(ResponsesFailure::response_contract_evidence(
                detail,
                status,
                headers.clone(),
                bytes,
                hasher.finalize().to_hex().to_string(),
                None,
            ));
        }
    }
    let digest = hasher.finalize().to_hex().to_string();
    let parsed = decoder.finish().map_err(|detail| {
        ResponsesFailure::response_contract_evidence(
            detail,
            status,
            headers.clone(),
            bytes,
            digest.clone(),
            None,
        )
    })?;
    Ok((
        parsed,
        ResponseBodyEvidence {
            bytes,
            blake3: digest,
            raw: None,
        },
    ))
}

fn context_artifacts(
    input: &InterfaceInvocationEnvelope,
    interface_id: &str,
    source_movement_seq: u64,
) -> Result<Vec<ContextArtifact>, ResponsesFailure> {
    let mut artifacts = Vec::new();
    let mut session_sources = BTreeSet::new();
    for bound in &input.artifact_binding_payloads {
        let mut value: Value = serde_json::from_slice(bound.as_bytes()).map_err(|error| {
            ResponsesFailure::contract(format!(
                "bound artifact '{}' is not JSON: {error}",
                bound.artifact.id
            ))
        })?;
        if let Some(snapshot) = value.get(SESSION_CONTEXT_FIELD).cloned() {
            let snapshot: SessionContext = serde_json::from_value(snapshot).map_err(|error| {
                ResponsesFailure::contract(format!(
                    "bound session context '{}' is malformed: {error}",
                    bound.artifact.id
                ))
            })?;
            if snapshot.schema_version != 2 {
                return Err(ResponsesFailure::contract(format!(
                    "bound session context '{}' has unsupported schema_version {}",
                    bound.artifact.id, snapshot.schema_version
                )));
            }
            if snapshot.provider_interface.as_str() != interface_id {
                return Err(ResponsesFailure::contract(format!(
                    "bound session context '{}' belongs to provider '{}' instead of '{}'",
                    bound.artifact.id,
                    snapshot.provider_interface.as_str(),
                    interface_id
                )));
            }
            for (index, item) in snapshot.artifacts.into_iter().enumerate() {
                if item.label.is_empty() || item.label.trim() != item.label {
                    return Err(ResponsesFailure::contract(format!(
                        "bound session context '{}' artifact[{index}] has an invalid label",
                        bound.artifact.id
                    )));
                }
                let source_key = format!(
                    "{}:{}:{}",
                    item.source_artifact.id.as_str(),
                    item.source_artifact.content.to_hex(),
                    item.label
                );
                if !session_sources.insert(source_key) {
                    return Err(ResponsesFailure::contract(format!(
                        "bound session context '{}' repeats an artifact source across the invocation at index {index}",
                        bound.artifact.id
                    )));
                }
                if item.payload.get(SESSION_CONTEXT_FIELD).is_some() {
                    return Err(ResponsesFailure::contract(format!(
                        "bound session context '{}' artifact[{index}] nests reserved session context",
                        bound.artifact.id
                    )));
                }
                artifacts.push(ContextArtifact {
                    id: format!("{}#session:{index}", bound.artifact.id),
                    label: item.label,
                    value: item.payload,
                    source_artifact: item.source_artifact,
                    source_movement_seq: item.source_movement_seq,
                    current: false,
                });
            }
            value
                .as_object_mut()
                .ok_or_else(|| {
                    ResponsesFailure::contract(format!(
                        "bound session context '{}' outer payload is not an object",
                        bound.artifact.id
                    ))
                })?
                .remove(SESSION_CONTEXT_FIELD);
        }
        let label = artifact_payload_label(bound.artifact.id.as_str(), &value)?;
        let source_key = format!(
            "{}:{}:{}",
            bound.artifact.id.as_str(),
            bound.artifact.content.to_hex(),
            label
        );
        if !session_sources.insert(source_key) {
            return Err(ResponsesFailure::contract(format!(
                "bound context repeats outer artifact source '{}'",
                bound.artifact.id
            )));
        }
        artifacts.push(ContextArtifact {
            id: bound.artifact.id.as_str().to_string(),
            label,
            value,
            source_artifact: bound.artifact.clone(),
            source_movement_seq: None,
            current: false,
        });
    }
    let manifest = input
        .read_artifact_manifest()
        .map_err(|error| ResponsesFailure::contract(error.to_string()))?;
    let bytes = input
        .read_artifact_payload()
        .map_err(|error| ResponsesFailure::contract(error.to_string()))?;
    let mut value: Value = serde_json::from_slice(&bytes).map_err(|error| {
        ResponsesFailure::contract(format!(
            "current artifact '{}' is not JSON: {error}",
            input.artifact.id
        ))
    })?;
    if let Some(snapshot) = value.get(SESSION_CONTEXT_FIELD).cloned() {
        let snapshot: SessionContext = serde_json::from_value(snapshot).map_err(|error| {
            ResponsesFailure::contract(format!(
                "current session context '{}' is malformed: {error}",
                input.artifact.id
            ))
        })?;
        if snapshot.schema_version != 2 {
            return Err(ResponsesFailure::contract(format!(
                "current session context '{}' has unsupported schema_version {}",
                input.artifact.id, snapshot.schema_version
            )));
        }
        if snapshot.provider_interface.as_str() != interface_id {
            return Err(ResponsesFailure::contract(format!(
                "current session context '{}' belongs to provider '{}' instead of '{}'",
                input.artifact.id,
                snapshot.provider_interface.as_str(),
                interface_id
            )));
        }
        for (index, item) in snapshot.artifacts.into_iter().enumerate() {
            if item.label.is_empty() || item.label.trim() != item.label {
                return Err(ResponsesFailure::contract(format!(
                    "current session context '{}' artifact[{index}] has an invalid label",
                    input.artifact.id
                )));
            }
            if item.payload.get(SESSION_CONTEXT_FIELD).is_some() {
                return Err(ResponsesFailure::contract(format!(
                    "current session context '{}' artifact[{index}] nests reserved session context",
                    input.artifact.id
                )));
            }
            let source_key = format!(
                "{}:{}:{}",
                item.source_artifact.id.as_str(),
                item.source_artifact.content.to_hex(),
                item.label
            );
            if !session_sources.insert(source_key) {
                return Err(ResponsesFailure::contract(format!(
                    "current session context '{}' repeats an artifact source at index {index}",
                    input.artifact.id
                )));
            }
            artifacts.push(ContextArtifact {
                id: format!("{}#session:{index}", input.artifact.id),
                label: item.label,
                value: item.payload,
                source_artifact: item.source_artifact,
                source_movement_seq: item.source_movement_seq,
                current: false,
            });
        }
        value
            .as_object_mut()
            .ok_or_else(|| {
                ResponsesFailure::contract(format!(
                    "current session context '{}' outer payload is not an object",
                    input.artifact.id
                ))
            })?
            .remove(SESSION_CONTEXT_FIELD);
    }
    let current_key = format!(
        "{}:{}:{}",
        input.artifact.id.as_str(),
        input.artifact.content.to_hex(),
        manifest.label
    );
    if !session_sources.insert(current_key) {
        return Err(ResponsesFailure::contract(format!(
            "current artifact '{}' is already present in session history",
            input.artifact.id
        )));
    }
    artifacts.push(ContextArtifact {
        id: input.artifact.id.as_str().to_string(),
        label: manifest.label,
        value,
        source_artifact: input.artifact.clone(),
        source_movement_seq: Some(source_movement_seq),
        current: true,
    });
    if artifacts.iter().filter(|artifact| artifact.current).count() != 1 {
        return Err(ResponsesFailure::contract(
            "Responses invocation must contain exactly one current artifact",
        ));
    }
    Ok(artifacts)
}

fn artifact_payload_label(artifact_id: &str, value: &Value) -> Result<String, ResponsesFailure> {
    value
        .get("label")
        .and_then(Value::as_str)
        .filter(|label| !label.is_empty() && label.trim() == *label)
        .map(str::to_string)
        .or_else(|| {
            artifact_id
                .split_once(':')
                .map(|(label, _)| label)
                .filter(|label| !label.is_empty() && label.trim() == *label)
                .map(str::to_string)
        })
        .ok_or_else(|| {
            ResponsesFailure::contract(format!(
                "bound artifact '{artifact_id}' has no self-identifying label"
            ))
        })
}

fn is_provider_replay_label(label: &str) -> bool {
    matches!(
        label,
        INTERFACE_ARTIFACT_OUTPUT_LABEL
            | REASONING_EVIDENCE_LABEL
            | NATIVE_TOOL_EVIDENCE_LABEL
            | TOOL_CALL_LABEL
    )
}

fn is_tool_result(label: &str, value: &Value) -> bool {
    label.ends_with(".result") && value.get("tool").is_some() && value.get("ok").is_some()
}

fn normalize_function_call_item(
    item: &Value,
    strip_id: bool,
    preserve_status: bool,
) -> Result<Value, ResponsesFailure> {
    let object = item
        .as_object()
        .ok_or_else(|| ResponsesFailure::contract("function_call item is not an object"))?;
    ensure_only_fields(
        object,
        &["type", "id", "status", "call_id", "name", "arguments"],
        "Responses function_call item",
    )?;
    let call_id = required_trimmed_string(item, "call_id", "function_call item")?;
    let name = required_trimmed_string(item, "name", "function_call item")?;
    let arguments = required_trimmed_string(item, "arguments", "function_call item")?;
    let parsed: Value = serde_json::from_str(&arguments).map_err(|error| {
        ResponsesFailure::contract(format!(
            "Responses function_call '{name}' arguments are not JSON: {error}"
        ))
    })?;
    if !parsed.is_object() {
        return Err(ResponsesFailure::contract(format!(
            "Responses function_call '{name}' arguments are not an object"
        )));
    }
    let mut normalized = Map::new();
    normalized.insert(
        "type".to_string(),
        Value::String("function_call".to_string()),
    );
    if !strip_id {
        if let Some(id) = optional_trimmed_string(item, "id")? {
            normalized.insert("id".to_string(), Value::String(id));
        }
    }
    if let Some(status) = item.get("status") {
        let status = status
            .as_str()
            .filter(|value| matches!(*value, "in_progress" | "completed" | "incomplete"))
            .ok_or_else(|| ResponsesFailure::contract("function_call status is invalid"))?;
        if preserve_status {
            normalized.insert("status".to_string(), Value::String(status.to_string()));
        }
    }
    normalized.insert("call_id".to_string(), Value::String(call_id));
    normalized.insert("name".to_string(), Value::String(name));
    normalized.insert("arguments".to_string(), Value::String(arguments));
    Ok(Value::Object(normalized))
}

fn normalize_output_content_part(
    object: &Map<String, Value>,
    part_type: &str,
    encrypted_content_enabled: bool,
) -> Result<Value, ResponsesFailure> {
    match part_type {
        "output_text" => {
            let allowed = ["type", "text", "annotations", "logprobs"]
                .into_iter()
                .collect::<BTreeSet<_>>();
            if let Some(unknown) = object.keys().find(|key| !allowed.contains(key.as_str())) {
                return Err(ResponsesFailure::contract(format!(
                    "Responses output_text has unknown field '{unknown}'"
                )));
            }
            let text = object
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| ResponsesFailure::contract("Responses output_text has no text"))?;
            let mut normalized = object.clone();
            normalized.insert("text".to_string(), Value::String(text.to_string()));
            Ok(Value::Object(normalized))
        }
        "refusal" => {
            ensure_only_fields(object, &["type", "refusal"], "Responses refusal")?;
            let refusal = object
                .get("refusal")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| ResponsesFailure::contract("Responses refusal has no text"))?;
            Ok(json!({"type": "refusal", "refusal": refusal}))
        }
        "encrypted_content" => {
            if !encrypted_content_enabled {
                return Err(ResponsesFailure::contract(
                    "Responses encrypted content is disabled by replay descriptor",
                ));
            }
            ensure_only_fields(
                object,
                &["type", "encrypted_content"],
                "Responses encrypted content",
            )?;
            let encrypted = object
                .get("encrypted_content")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    ResponsesFailure::contract("Responses encrypted content has no payload")
                })?;
            Ok(json!({
                "type": "encrypted_content",
                "encrypted_content": encrypted,
            }))
        }
        other => Err(ResponsesFailure::contract(format!(
            "Responses output content type '{other}' has no validator"
        ))),
    }
}

fn normalize_reasoning_parts(
    value: Option<&Value>,
    canonical_type: &str,
) -> Result<Vec<Value>, ResponsesFailure> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let values = value.as_array().ok_or_else(|| {
        ResponsesFailure::contract(format!(
            "Responses reasoning {canonical_type} parts are not an array"
        ))
    })?;
    values
        .iter()
        .enumerate()
        .map(|(index, part)| {
            let object = part.as_object().ok_or_else(|| {
                ResponsesFailure::contract(format!(
                    "Responses reasoning part {index} is not an object"
                ))
            })?;
            ensure_only_fields(object, &["type", "text"], "Responses reasoning part")?;
            let part_type = object.get("type").and_then(Value::as_str).ok_or_else(|| {
                ResponsesFailure::contract("Responses reasoning part has no type")
            })?;
            let accepted = match canonical_type {
                "summary_text" => matches!(part_type, "summary_text" | "text"),
                "reasoning_text" => matches!(part_type, "reasoning_text" | "text"),
                _ => false,
            };
            if !accepted {
                return Err(ResponsesFailure::contract(format!(
                    "Responses reasoning part type '{part_type}' is invalid for {canonical_type}"
                )));
            }
            let text = object.get("text").and_then(Value::as_str).ok_or_else(|| {
                ResponsesFailure::contract("Responses reasoning part has no text")
            })?;
            Ok(json!({"type": canonical_type, "text": text}))
        })
        .collect()
}

fn extract_output_message_text(
    item: &Value,
    text: &mut Vec<String>,
    refusals: &mut Vec<String>,
) -> Result<(), ResponsesFailure> {
    let content = item
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| ResponsesFailure::contract("normalized output message has no content"))?;
    for part in content {
        match part.get("type").and_then(Value::as_str) {
            Some("output_text") => {
                let value = part
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ResponsesFailure::contract("output_text has no text"))?;
                if !value.is_empty() {
                    text.push(value.to_string());
                }
            }
            Some("refusal") => refusals.push(required_trimmed_string(
                part,
                "refusal",
                "Responses refusal",
            )?),
            Some("encrypted_content") => {}
            Some(other) => {
                return Err(ResponsesFailure::contract(format!(
                    "normalized output message has unsupported part '{other}'"
                )));
            }
            None => {
                return Err(ResponsesFailure::contract(
                    "normalized output message part has no type",
                ));
            }
        }
    }
    Ok(())
}

fn extract_reasoning_text(
    item: &Value,
    summaries: &mut Vec<String>,
    content: &mut Vec<String>,
) -> Result<(), ResponsesFailure> {
    if let Some(parts) = item.get("summary").and_then(Value::as_array) {
        for part in parts {
            summaries.push(required_string_allow_empty(
                part,
                "text",
                "reasoning summary",
            )?);
        }
    }
    if let Some(parts) = item.get("content").and_then(Value::as_array) {
        for part in parts {
            content.push(required_string_allow_empty(
                part,
                "text",
                "reasoning content",
            )?);
        }
    }
    Ok(())
}

fn validate_function_call_order(items: &[Value]) -> Result<(), ResponsesFailure> {
    let mut calls = BTreeSet::new();
    let mut outputs = BTreeSet::new();
    for (index, item) in items.iter().enumerate() {
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                let call_id = required_trimmed_string(item, "call_id", "function_call")?;
                if outputs.contains(&call_id) || !calls.insert(call_id.clone()) {
                    return Err(ResponsesFailure::contract(format!(
                        "Responses input[{index}] repeats or reorders function_call '{call_id}'"
                    )));
                }
            }
            Some("function_call_output") => {
                let call_id = required_trimmed_string(item, "call_id", "function_call_output")?;
                if !calls.contains(&call_id) || !outputs.insert(call_id.clone()) {
                    return Err(ResponsesFailure::contract(format!(
                        "Responses input[{index}] function_call_output '{call_id}' has no unique preceding call"
                    )));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn encode_tool_name(name: &str) -> Result<String, ResponsesFailure> {
    if name.is_empty() || name.trim() != name {
        return Err(ResponsesFailure::contract(
            "Responses tool name must be non-empty and trimmed",
        ));
    }
    let mut encoded = String::with_capacity(name.len());
    for byte in name.bytes() {
        match byte {
            b'-' => encoded.push_str("-h"),
            b'.' => encoded.push_str("-d"),
            byte if byte.is_ascii_alphanumeric() || byte == b'_' => encoded.push(byte as char),
            _ => {
                return Err(ResponsesFailure::contract(format!(
                    "Responses tool name '{name}' contains an unsupported character"
                )));
            }
        }
    }
    if encoded.len() > 64 {
        return Err(ResponsesFailure::contract(format!(
            "encoded Responses tool name '{encoded}' exceeds 64 bytes"
        )));
    }
    Ok(encoded)
}

fn ensure_only_fields(
    object: &Map<String, Value>,
    allowed: &[&str],
    surface: &str,
) -> Result<(), ResponsesFailure> {
    if let Some(unknown) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(ResponsesFailure::contract(format!(
            "{surface} has unknown field '{unknown}'"
        )));
    }
    Ok(())
}

fn required_trimmed_string(
    value: &Value,
    key: &str,
    surface: &str,
) -> Result<String, ResponsesFailure> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.trim() == *value)
        .map(str::to_string)
        .ok_or_else(|| {
            ResponsesFailure::contract(format!("{surface} has no non-empty trimmed '{key}'"))
        })
}

fn required_string_allow_empty(
    value: &Value,
    key: &str,
    surface: &str,
) -> Result<String, ResponsesFailure> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ResponsesFailure::contract(format!("{surface} has no string '{key}'")))
}

fn optional_trimmed_string(value: &Value, key: &str) -> Result<Option<String>, ResponsesFailure> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() && value.trim() == value => {
            Ok(Some(value.clone()))
        }
        Some(_) => Err(ResponsesFailure::contract(format!(
            "Responses field '{key}' must be a non-empty trimmed string or null"
        ))),
    }
}

fn validate_optional_item_status(value: &Value, surface: &str) -> Result<(), ResponsesFailure> {
    match value.get("status") {
        None => Ok(()),
        Some(Value::String(status))
            if matches!(status.as_str(), "in_progress" | "completed" | "incomplete") =>
        {
            Ok(())
        }
        Some(_) => Err(ResponsesFailure::contract(format!(
            "{surface} status is invalid"
        ))),
    }
}

fn validate_url_scheme(
    raw: &str,
    allowed: &BTreeSet<String>,
    surface: &str,
) -> Result<(), ResponsesFailure> {
    let scheme = raw
        .split_once(':')
        .map(|(scheme, _)| scheme)
        .ok_or_else(|| {
            ResponsesFailure::contract(format!("Responses {surface} URL has no scheme"))
        })?;
    if !allowed.contains(scheme) {
        return Err(ResponsesFailure::contract(format!(
            "Responses {surface} URL scheme '{scheme}' is not descriptor-admitted"
        )));
    }
    if scheme != "data" {
        let url = reqwest::Url::parse(raw).map_err(|error| {
            ResponsesFailure::contract(format!("Responses {surface} URL is invalid: {error}"))
        })?;
        if url.username() != "" || url.password().is_some() {
            return Err(ResponsesFailure::contract(format!(
                "Responses {surface} URL cannot contain credentials"
            )));
        }
    }
    Ok(())
}

fn validate_inline_file_data(raw: &str, maximum: u64) -> Result<(), ResponsesFailure> {
    let encoded = if let Some(rest) = raw.strip_prefix("data:") {
        let (metadata, encoded) = rest.split_once(',').ok_or_else(|| {
            ResponsesFailure::contract("Responses file_data data URL has no comma")
        })?;
        if !metadata.split(';').any(|part| part == "base64") {
            return Err(ResponsesFailure::contract(
                "Responses file_data data URL is not base64 encoded",
            ));
        }
        encoded
    } else {
        raw
    };
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| ResponsesFailure::contract("Responses file_data is not valid base64"))?;
    if decoded.len() as u64 > maximum {
        return Err(ResponsesFailure::contract(format!(
            "Responses file_data exceeds descriptor maximum {maximum} bytes"
        )));
    }
    Ok(())
}

fn render_json(value: &Value) -> Result<String, ResponsesFailure> {
    serde_json::to_string(value)
        .map_err(|error| ResponsesFailure::contract(format!("JSON did not encode: {error}")))
}

fn value_blake3(value: &Value) -> Result<String, ResponsesFailure> {
    let bytes = serde_json::to_vec(value).map_err(|error| {
        ResponsesFailure::contract(format!("value did not encode for hashing: {error}"))
    })?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn response_item_evidence(output_index: usize, item: &Value) -> Result<Value, ResponsesFailure> {
    Ok(json!({
        "output_index": output_index,
        "item": item,
        "raw_blake3": value_blake3(item)?,
    }))
}

fn raw_bytes_evidence(bytes: &[u8]) -> Value {
    match std::str::from_utf8(bytes) {
        Ok(value) => json!({"encoding": "utf8", "body": value}),
        Err(_) => json!({
            "encoding": "base64",
            "body": base64::engine::general_purpose::STANDARD.encode(bytes),
        }),
    }
}

fn json_scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if !value.trim().is_empty() => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

impl ResponsesSseDecoder {
    fn new(sequence_mode: SequenceNumbers) -> Self {
        Self {
            pending: Vec::new(),
            frame_index: 0,
            accumulator: ResponsesStreamAccumulator {
                protocol: "SSE",
                sequence_mode,
                last_sequence: None,
                events: Vec::new(),
                items: BTreeMap::new(),
                terminal: None,
            },
        }
    }

    fn push(&mut self, bytes: &[u8]) -> Result<(), String> {
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

    fn finish(mut self) -> Result<ParsedStream, String> {
        if self.pending.iter().any(|byte| !byte.is_ascii_whitespace()) {
            let frame = std::mem::take(&mut self.pending);
            self.apply_frame(&frame)?;
        }
        self.accumulator.finish()
    }

    fn apply_frame(&mut self, frame: &[u8]) -> Result<(), String> {
        if frame.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let text = std::str::from_utf8(frame).map_err(|error| {
            format!(
                "Responses SSE frame {} is not UTF-8: {error}",
                self.frame_index
            )
        })?;
        let frame_index = self.frame_index;
        self.frame_index = self
            .frame_index
            .checked_add(1)
            .ok_or_else(|| "Responses SSE frame count overflowed".to_string())?;
        let mut event_name = None;
        let mut event_id = None;
        let mut retry = None;
        let mut data_lines = Vec::new();
        for line in text.split(['\r', '\n']).filter(|line| !line.is_empty()) {
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "event" => event_name = Some(value.to_string()),
                "data" => data_lines.push(value),
                "id" => event_id = Some(value.to_string()),
                "retry" => {
                    retry = Some(value.parse::<u64>().map_err(|_| {
                        format!("Responses SSE frame {frame_index} has invalid retry")
                    })?)
                }
                "" => {}
                other => {
                    return Err(format!(
                        "Responses SSE frame {frame_index} has unsupported field '{other}'"
                    ));
                }
            }
        }
        if data_lines.is_empty() {
            return Err(format!("Responses SSE frame {frame_index} carries no data"));
        }
        let data = data_lines.join("\n");
        if data == "[DONE]" {
            if self.accumulator.terminal.is_none() {
                return Err(
                    "Responses SSE emitted [DONE] before a terminal response event".to_string(),
                );
            }
            return Ok(());
        }
        if self.accumulator.terminal.is_some() {
            return Err(format!(
                "Responses SSE carries data after its terminal event at frame {frame_index}"
            ));
        }
        let value: Value = serde_json::from_str(&data).map_err(|error| {
            format!("Responses SSE frame {frame_index} data is not JSON: {error}")
        })?;
        let event_type = value
            .get("type")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.trim() == *value)
            .ok_or_else(|| format!("Responses SSE frame {frame_index} has no type"))?;
        if let Some(name) = event_name.as_deref() {
            if name != event_type {
                return Err(format!(
                    "Responses SSE frame {frame_index} event '{name}' does not match payload type '{event_type}'"
                ));
            }
        }
        self.accumulator.apply(event_type, &value)?;
        self.accumulator.events.push(json!({
            "frame_index": frame_index,
            "event": event_name,
            "id": event_id,
            "retry": retry,
            "type": event_type,
            "sequence_number": value.get("sequence_number"),
            "data_bytes": data.len(),
            "data_blake3": blake3::hash(data.as_bytes()).to_hex().to_string(),
        }));
        Ok(())
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

fn parse_websocket_events(
    events: &[Value],
    sequence_mode: SequenceNumbers,
) -> Result<ParsedStream, String> {
    let mut accumulator = ResponsesStreamAccumulator {
        protocol: "websocket",
        sequence_mode,
        last_sequence: None,
        events: Vec::new(),
        items: BTreeMap::new(),
        terminal: None,
    };
    for (frame_index, value) in events.iter().enumerate() {
        if accumulator.terminal.is_some() {
            return Err(format!(
                "Responses websocket carries data after its terminal event at frame {frame_index}"
            ));
        }
        let event_type = value
            .get("type")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.trim() == *value)
            .ok_or_else(|| format!("Responses websocket frame {frame_index} has no valid type"))?;
        accumulator.apply(event_type, value)?;
        let data = serde_json::to_vec(value).map_err(|error| {
            format!("Responses websocket frame {frame_index} did not encode: {error}")
        })?;
        accumulator.events.push(json!({
            "frame_index": frame_index,
            "type": event_type,
            "sequence_number": value.get("sequence_number"),
            "data_bytes": data.len(),
            "data_blake3": blake3::hash(&data).to_hex().to_string(),
        }));
    }
    accumulator.finish()
}

impl ResponsesStreamAccumulator {
    fn apply(&mut self, event_type: &str, value: &Value) -> Result<(), String> {
        let sequence = value.get("sequence_number").and_then(Value::as_u64);
        match self.sequence_mode {
            SequenceNumbers::RequiredMonotonic => {
                let sequence = sequence.ok_or_else(|| {
                    format!(
                        "Responses {} event '{event_type}' has no sequence_number",
                        self.protocol
                    )
                })?;
                self.accept_sequence(sequence, event_type)?;
            }
            SequenceNumbers::OptionalMonotonic => {
                if let Some(sequence) = sequence {
                    self.accept_sequence(sequence, event_type)?;
                } else if value.get("sequence_number").is_some() {
                    return Err(format!(
                        "Responses {} event '{event_type}' has malformed sequence_number",
                        self.protocol
                    ));
                }
            }
        }
        match event_type {
            "response.output_item.added" | "response.output_item.done" => {
                let output_index = event_usize(value, "output_index", event_type)?;
                let item = value
                    .get("item")
                    .filter(|value| value.is_object())
                    .ok_or_else(|| format!("{event_type} has no item object"))?;
                if event_type == "response.output_item.added"
                    && self.items.insert(output_index, item.clone()).is_some()
                {
                    return Err(format!(
                        "Responses {} repeats output_item.added index {output_index}",
                        self.protocol
                    ));
                }
                if event_type == "response.output_item.done" {
                    self.items.insert(output_index, item.clone());
                }
            }
            "response.content_part.added" | "response.content_part.done" => {
                let output_index = event_usize(value, "output_index", event_type)?;
                let content_index = event_usize(value, "content_index", event_type)?;
                let part = value
                    .get("part")
                    .filter(|value| value.is_object())
                    .ok_or_else(|| format!("{event_type} has no part object"))?;
                set_message_part(
                    &mut self.items,
                    output_index,
                    content_index,
                    event_item_id(value),
                    part.clone(),
                )?;
            }
            "response.output_text.delta" => {
                apply_message_text_event(&mut self.items, value, "output_text", "delta", true)?;
            }
            "response.output_text.done" => {
                apply_message_text_event(&mut self.items, value, "output_text", "text", false)?;
            }
            "response.refusal.delta" => {
                apply_message_text_event(&mut self.items, value, "refusal", "delta", true)?;
            }
            "response.refusal.done" => {
                apply_message_text_event(&mut self.items, value, "refusal", "refusal", false)?;
            }
            "response.function_call_arguments.delta" => {
                apply_function_input_event(
                    &mut self.items,
                    value,
                    "function_call",
                    "arguments",
                    "delta",
                    true,
                )?;
            }
            "response.function_call_arguments.done" => {
                apply_function_input_event(
                    &mut self.items,
                    value,
                    "function_call",
                    "arguments",
                    "arguments",
                    false,
                )?;
            }
            "response.custom_tool_call_input.delta" => {
                apply_function_input_event(
                    &mut self.items,
                    value,
                    "custom_tool_call",
                    "input",
                    "delta",
                    true,
                )?;
            }
            "response.custom_tool_call_input.done" => {
                apply_function_input_event(
                    &mut self.items,
                    value,
                    "custom_tool_call",
                    "input",
                    "input",
                    false,
                )?;
            }
            "response.reasoning_summary_part.added" | "response.reasoning_summary_part.done" => {
                let output_index = event_usize(value, "output_index", event_type)?;
                let summary_index = event_usize(value, "summary_index", event_type)?;
                let part = value
                    .get("part")
                    .filter(|value| value.is_object())
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "summary_text", "text": ""}));
                set_reasoning_part(
                    &mut self.items,
                    output_index,
                    "summary",
                    summary_index,
                    event_item_id(value),
                    part,
                )?;
            }
            "response.reasoning_summary_text.delta" => {
                apply_reasoning_text_event(
                    &mut self.items,
                    value,
                    "summary",
                    "summary_index",
                    "summary_text",
                    "delta",
                    true,
                )?;
            }
            "response.reasoning_summary_text.done" => {
                apply_reasoning_text_event(
                    &mut self.items,
                    value,
                    "summary",
                    "summary_index",
                    "summary_text",
                    "text",
                    false,
                )?;
            }
            "response.reasoning_text.delta" => {
                apply_reasoning_text_event(
                    &mut self.items,
                    value,
                    "content",
                    "content_index",
                    "reasoning_text",
                    "delta",
                    true,
                )?;
            }
            "response.reasoning_text.done" => {
                apply_reasoning_text_event(
                    &mut self.items,
                    value,
                    "content",
                    "content_index",
                    "reasoning_text",
                    "text",
                    false,
                )?;
            }
            "response.completed" | "response.failed" | "response.incomplete" => {
                let response = value
                    .get("response")
                    .filter(|value| value.is_object())
                    .ok_or_else(|| format!("{event_type} has no response object"))?;
                self.terminal = Some((event_type.to_string(), response.clone()));
            }
            "error" => {
                self.terminal = Some((event_type.to_string(), value.clone()));
            }
            other if other.starts_with("response.") => {}
            other => {
                return Err(format!(
                    "Responses {} event type '{other}' is outside the Responses protocol",
                    self.protocol
                ));
            }
        }
        Ok(())
    }

    fn accept_sequence(&mut self, sequence: u64, event_type: &str) -> Result<(), String> {
        if self
            .last_sequence
            .is_some_and(|previous| sequence <= previous)
        {
            return Err(format!(
                "Responses {} event '{event_type}' sequence_number {sequence} is not strictly increasing",
                self.protocol
            ));
        }
        self.last_sequence = Some(sequence);
        Ok(())
    }

    fn finish(mut self) -> Result<ParsedStream, String> {
        let (terminal_type, mut response) = self
            .terminal
            .take()
            .ok_or_else(|| format!("Responses {} ended before a terminal event", self.protocol))?;
        if terminal_type == "response.completed" {
            let output = response
                .get("output")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if output.is_empty() {
                if self.items.is_empty() {
                    return Err(format!(
                        "Responses {} completed without terminal or reconstructed output items",
                        self.protocol
                    ));
                }
                let mut reconstructed = Vec::with_capacity(self.items.len());
                for (expected, (actual, item)) in self.items.into_iter().enumerate() {
                    if expected != actual {
                        return Err(format!(
                            "Responses {} output indexes are not contiguous at {expected}",
                            self.protocol
                        ));
                    }
                    reconstructed.push(item);
                }
                response
                    .as_object_mut()
                    .ok_or_else(|| "Responses terminal response is not an object".to_string())?
                    .insert("output".to_string(), Value::Array(reconstructed));
            } else if !self.items.is_empty() {
                for (index, streamed) in &self.items {
                    let terminal = output.get(*index).ok_or_else(|| {
                        format!("Responses terminal output omits streamed output index {index}")
                    })?;
                    let streamed_type = streamed.get("type").and_then(Value::as_str);
                    let terminal_type = terminal.get("type").and_then(Value::as_str);
                    if streamed_type != terminal_type {
                        return Err(format!(
                            "Responses terminal output index {index} type {terminal_type:?} conflicts with streamed type {streamed_type:?}"
                        ));
                    }
                    let streamed_id = streamed.get("id").and_then(Value::as_str);
                    let terminal_id = terminal.get("id").and_then(Value::as_str);
                    if streamed_id.is_some() && terminal_id.is_some() && streamed_id != terminal_id
                    {
                        return Err(format!(
                            "Responses terminal output index {index} id conflicts with streamed id"
                        ));
                    }
                }
            }
        }
        Ok(ParsedStream {
            response,
            events: self.events,
            terminal_type,
        })
    }
}

fn event_usize(value: &Value, field: &str, event_type: &str) -> Result<usize, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| format!("{event_type} has no valid {field}"))
}

fn event_item_id(value: &Value) -> Option<String> {
    value
        .get("item_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.trim() == *value)
        .map(str::to_string)
}

fn ensure_message_item(
    items: &mut BTreeMap<usize, Value>,
    output_index: usize,
    item_id: Option<String>,
) -> Result<&mut Map<String, Value>, String> {
    let item = items.entry(output_index).or_insert_with(|| {
        json!({
            "type": "message",
            "id": item_id,
            "role": "assistant",
            "status": "in_progress",
            "content": []
        })
    });
    let object = item
        .as_object_mut()
        .ok_or_else(|| format!("Responses streamed item {output_index} is not an object"))?;
    if object.get("type").and_then(Value::as_str) != Some("message") {
        return Err(format!(
            "Responses streamed item {output_index} is not a message"
        ));
    }
    if object.get("id").is_none() {
        if let Some(item_id) = item_id {
            object.insert("id".to_string(), Value::String(item_id));
        }
    }
    Ok(object)
}

fn ensure_reasoning_item(
    items: &mut BTreeMap<usize, Value>,
    output_index: usize,
    item_id: Option<String>,
) -> Result<&mut Map<String, Value>, String> {
    let item = items.entry(output_index).or_insert_with(|| {
        json!({
            "type": "reasoning",
            "id": item_id,
            "status": "in_progress",
            "summary": [],
            "content": []
        })
    });
    let object = item
        .as_object_mut()
        .ok_or_else(|| format!("Responses streamed item {output_index} is not an object"))?;
    if object.get("type").and_then(Value::as_str) != Some("reasoning") {
        return Err(format!(
            "Responses streamed item {output_index} is not reasoning"
        ));
    }
    if object.get("id").is_none() {
        if let Some(item_id) = item_id {
            object.insert("id".to_string(), Value::String(item_id));
        }
    }
    Ok(object)
}

fn set_message_part(
    items: &mut BTreeMap<usize, Value>,
    output_index: usize,
    content_index: usize,
    item_id: Option<String>,
    part: Value,
) -> Result<(), String> {
    let item = ensure_message_item(items, output_index, item_id)?;
    let content = item
        .entry("content".to_string())
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| format!("Responses message {output_index} content is not an array"))?;
    while content.len() <= content_index {
        content.push(Value::Null);
    }
    if !content[content_index].is_null() && content[content_index] != part {
        return Err(format!(
            "Responses stream conflicts at message {output_index} content {content_index}"
        ));
    }
    content[content_index] = part;
    Ok(())
}

fn apply_message_text_event(
    items: &mut BTreeMap<usize, Value>,
    value: &Value,
    part_type: &str,
    source_field: &str,
    append: bool,
) -> Result<(), String> {
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("response content event");
    let output_index = event_usize(value, "output_index", event_type)?;
    let content_index = event_usize(value, "content_index", event_type)?;
    let fragment = value
        .get(source_field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{event_type} has no string {source_field}"))?;
    let item = ensure_message_item(items, output_index, event_item_id(value))?;
    let content = item
        .entry("content".to_string())
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| format!("Responses message {output_index} content is not an array"))?;
    while content.len() <= content_index {
        content.push(Value::Null);
    }
    if content[content_index].is_null() {
        let mut created = Map::new();
        created.insert("type".to_string(), Value::String(part_type.to_string()));
        created.insert(
            if part_type == "refusal" {
                "refusal".to_string()
            } else {
                "text".to_string()
            },
            Value::String(String::new()),
        );
        content[content_index] = Value::Object(created);
    }
    let part = content[content_index]
        .as_object_mut()
        .ok_or_else(|| format!("Responses message part {content_index} is not an object"))?;
    if part.get("type").and_then(Value::as_str) != Some(part_type) {
        return Err(format!(
            "Responses message part {content_index} type conflicts with {part_type} event"
        ));
    }
    let target_field = if part_type == "refusal" {
        "refusal"
    } else {
        "text"
    };
    update_stream_string(part, target_field, fragment, append)?;
    Ok(())
}

fn apply_function_input_event(
    items: &mut BTreeMap<usize, Value>,
    value: &Value,
    item_type: &str,
    target_field: &str,
    source_field: &str,
    append: bool,
) -> Result<(), String> {
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("response function event");
    let output_index = event_usize(value, "output_index", event_type)?;
    let item_id = event_item_id(value);
    let item = items.entry(output_index).or_insert_with(|| {
        let mut created = Map::new();
        created.insert("type".to_string(), Value::String(item_type.to_string()));
        if let Some(item_id) = item_id {
            created.insert("id".to_string(), Value::String(item_id));
        }
        if let Some(call_id) = value.get("call_id").cloned() {
            created.insert("call_id".to_string(), call_id);
        }
        if let Some(name) = value.get("name").cloned() {
            created.insert("name".to_string(), name);
        }
        created.insert(target_field.to_string(), Value::String(String::new()));
        created.insert(
            "status".to_string(),
            Value::String("in_progress".to_string()),
        );
        Value::Object(created)
    });
    let object = item
        .as_object_mut()
        .ok_or_else(|| format!("Responses streamed item {output_index} is not an object"))?;
    if object.get("type").and_then(Value::as_str) != Some(item_type) {
        return Err(format!(
            "Responses streamed item {output_index} conflicts with {item_type} event"
        ));
    }
    for field in ["call_id", "name"] {
        if let Some(field_value) = value
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            object.insert(field.to_string(), Value::String(field_value.to_string()));
        }
    }
    let fragment = value
        .get(source_field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{event_type} has no string {source_field}"))?;
    update_stream_string(object, target_field, fragment, append)?;
    Ok(())
}

fn set_reasoning_part(
    items: &mut BTreeMap<usize, Value>,
    output_index: usize,
    field: &str,
    part_index: usize,
    item_id: Option<String>,
    part: Value,
) -> Result<(), String> {
    let item = ensure_reasoning_item(items, output_index, item_id)?;
    let parts = item
        .entry(field.to_string())
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| format!("Responses reasoning {field} is not an array"))?;
    while parts.len() <= part_index {
        parts.push(Value::Null);
    }
    if !parts[part_index].is_null() && parts[part_index] != part {
        return Err(format!(
            "Responses stream conflicts at reasoning {output_index} {field} {part_index}"
        ));
    }
    parts[part_index] = part;
    Ok(())
}

fn apply_reasoning_text_event(
    items: &mut BTreeMap<usize, Value>,
    value: &Value,
    field: &str,
    index_field: &str,
    part_type: &str,
    source_field: &str,
    append: bool,
) -> Result<(), String> {
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("response reasoning event");
    let output_index = event_usize(value, "output_index", event_type)?;
    let part_index = event_usize(value, index_field, event_type)?;
    let fragment = value
        .get(source_field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{event_type} has no string {source_field}"))?;
    let item = ensure_reasoning_item(items, output_index, event_item_id(value))?;
    let parts = item
        .entry(field.to_string())
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| format!("Responses reasoning {field} is not an array"))?;
    while parts.len() <= part_index {
        parts.push(Value::Null);
    }
    if parts[part_index].is_null() {
        parts[part_index] = json!({"type": part_type, "text": ""});
    }
    let part = parts[part_index]
        .as_object_mut()
        .ok_or_else(|| format!("Responses reasoning part {part_index} is not an object"))?;
    if part.get("type").and_then(Value::as_str) != Some(part_type) {
        return Err(format!(
            "Responses reasoning part {part_index} conflicts with {part_type} event"
        ));
    }
    update_stream_string(part, "text", fragment, append)?;
    Ok(())
}

fn update_stream_string(
    object: &mut Map<String, Value>,
    field: &str,
    fragment: &str,
    append: bool,
) -> Result<(), String> {
    if append {
        match object.get_mut(field) {
            Some(Value::String(value)) => value.push_str(fragment),
            Some(_) => return Err(format!("Responses stream field '{field}' is not a string")),
            None => {
                object.insert(field.to_string(), Value::String(fragment.to_string()));
            }
        }
    } else {
        object.insert(field.to_string(), Value::String(fragment.to_string()));
    }
    Ok(())
}

fn validate_terminal_response(
    response: &Value,
    terminal_type: Option<&str>,
    status: u16,
    headers: Value,
    evidence: &ResponseBodyEvidence,
) -> Result<(), ResponsesFailure> {
    let object = response.as_object().ok_or_else(|| {
        ResponsesFailure::response_contract_evidence(
            "Responses terminal payload is not an object",
            status,
            headers.clone(),
            evidence.bytes,
            evidence.blake3.clone(),
            evidence.raw.as_deref(),
        )
    })?;
    let response_status = object
        .get("status")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or(match terminal_type {
            Some("response.completed") => "completed",
            Some("response.failed") | Some("error") => "failed",
            Some("response.incomplete") => "incomplete",
            _ => "",
        });
    let effective_terminal = terminal_type.unwrap_or(match response_status {
        "completed" => "response.completed",
        "failed" | "cancelled" => "response.failed",
        "incomplete" => "response.incomplete",
        _ => "response.unknown",
    });
    match effective_terminal {
        "response.completed" if response_status == "completed" => Ok(()),
        "response.failed" | "error" => {
            let error = object.get("error").unwrap_or(response);
            let code = error.get("code").and_then(json_scalar_string);
            let message = error
                .get("message")
                .and_then(json_scalar_string)
                .unwrap_or_else(|| "Responses API returned a failed response".to_string());
            Err(ResponsesFailure::response_outcome_evidence(
                code,
                message,
                status,
                headers,
                evidence.bytes,
                evidence.blake3.clone(),
                evidence.raw.as_deref(),
            ))
        }
        "response.incomplete" => {
            let reason = object
                .get("incomplete_details")
                .and_then(Value::as_object)
                .and_then(|details| details.get("reason"))
                .and_then(json_scalar_string);
            Err(ResponsesFailure::response_outcome_evidence(
                reason.clone(),
                reason
                    .map(|value| format!("Responses API returned an incomplete response: {value}"))
                    .unwrap_or_else(|| {
                        "Responses API returned an incomplete response without a reason".to_string()
                    }),
                status,
                headers,
                evidence.bytes,
                evidence.blake3.clone(),
                evidence.raw.as_deref(),
            ))
        }
        other => Err(ResponsesFailure::response_contract_evidence(
            format!(
                "Responses terminal event '{other}' conflicts with response status '{response_status}'"
            ),
            status,
            headers,
            evidence.bytes,
            evidence.blake3.clone(),
            evidence.raw.as_deref(),
        )),
    }
}

fn websocket_error_outcome(
    event: &Value,
    handshake_status: u16,
    headers: Value,
    bytes: &[u8],
) -> ResponsesFailure {
    let error = event.get("error").unwrap_or(event);
    let status = event
        .get("status")
        .or_else(|| event.get("status_code"))
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok());
    let code = error.get("code").and_then(json_scalar_string);
    let detail = error
        .get("message")
        .and_then(json_scalar_string)
        .unwrap_or_else(|| "Responses websocket returned an error event".to_string());
    ResponsesFailure {
        class: "response_outcome",
        retryable: false,
        status,
        code,
        detail,
        evidence: Some(json!({
            "handshake_status": handshake_status,
            "headers": headers,
            "response_bytes": bytes.len(),
            "response_blake3": blake3::hash(bytes).to_hex().to_string(),
            "raw_response": raw_bytes_evidence(bytes),
        })),
    }
}

fn materialize_emission(
    output_root: &Path,
    label: &str,
    payload: Value,
    route: ProviderEmissionRoute,
) -> Result<ProviderEmission, ResponsesFailure> {
    let bytes = serde_json::to_vec(&payload).map_err(|error| {
        ResponsesFailure::contract(format!("Responses output did not encode: {error}"))
    })?;
    let artifact = InterfaceArtifactFile::write(output_root, label, &bytes)
        .map_err(|error| ResponsesFailure::contract(error.to_string()))?;
    Ok(ProviderEmission { artifact, route })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::Mutex;

    use octopus_descriptors::{ArtifactId, CheckpointId, EeId, InterfaceId, RunId};
    use octopus_interfaces::{
        ArtifactBindingPayload, GrantProjection, InvocationMetadata, RuntimeInvocationMetadata,
    };
    use octopus_ledger::{ArtifactRef, ContentHash};

    static ENVIRONMENT: Mutex<()> = Mutex::new(());

    #[test]
    fn model_catalog_is_intersected_with_formed_policy() {
        let models = vec![
            json!({"identity": {"id": "model-a"}}),
            json!({"identity": {"id": "model-b"}}),
        ];
        assert_eq!(
            filter_model_catalog(
                &models,
                "/identity/id",
                &BTreeSet::from(["model-a".to_string()]),
            ),
            vec![json!({"identity": {"id": "model-a"}})]
        );
    }

    struct EnvironmentRestore(Vec<(String, Option<OsString>)>);

    impl EnvironmentRestore {
        fn set(values: &[(&str, &str)]) -> Self {
            let previous = values
                .iter()
                .map(|(name, _)| ((*name).to_string(), std::env::var_os(name)))
                .collect();
            for (name, value) in values {
                unsafe {
                    std::env::set_var(name, value);
                }
            }
            Self(previous)
        }
    }

    impl Drop for EnvironmentRestore {
        fn drop(&mut self) {
            for (name, value) in self.0.drain(..) {
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }

    fn interface_registry_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("interface registry root")
            .to_path_buf()
    }

    fn descriptor(name: &str) -> InterfaceDescriptor {
        let bytes = std::fs::read(interface_registry_root().join(name).join("interface.json"))
            .expect("interface descriptor reads");
        octopus_descriptors::parse_strict(&bytes).expect("interface descriptor parses strictly")
    }

    fn resolved_descriptor(name: &str) -> InterfaceDescriptor {
        descriptor(name)
    }

    fn driver(name: &str) -> OpenAiResponsesDriver {
        OpenAiResponsesDriver::from_descriptor(
            &resolved_descriptor(name),
            tempfile::tempdir().expect("temporary secrets home").keep(),
        )
        .expect("Responses driver resolves")
    }

    fn driver_error(descriptor: &InterfaceDescriptor) -> String {
        match OpenAiResponsesDriver::from_descriptor(
            descriptor,
            tempfile::tempdir().expect("temporary secrets home").keep(),
        ) {
            Ok(_) => panic!("incomplete Responses descriptor unexpectedly resolved"),
            Err(error) => error,
        }
    }

    fn payload_binding(id: &str, value: Value) -> ArtifactBindingPayload {
        let bytes = serde_json::to_vec(&value).expect("binding payload encodes");
        ArtifactBindingPayload::new(
            ArtifactRef {
                id: ArtifactId::new(id),
                content: ContentHash::of_bytes(&bytes),
            },
            bytes,
        )
    }

    fn invocation(
        current_label: &str,
        current: Value,
        mut bound: Vec<ArtifactBindingPayload>,
        affordances: Vec<Value>,
    ) -> InterfaceInvocationEnvelope {
        bound.insert(
            0,
            payload_binding(
                "context.reference:test",
                json!({
                    "label": CONTEXT_REFERENCE_LABEL,
                    "text": "Treat bound artifacts as the complete model context."
                }),
            ),
        );
        let bytes = serde_json::to_vec(&current).expect("current payload encodes");
        let content = ContentHash::of_bytes(&bytes);
        let artifact_file = InterfaceArtifactFile::write(
            tempfile::tempdir()
                .expect("temporary invocation root")
                .keep(),
            current_label,
            &bytes,
        )
        .expect("current artifact materializes");
        let names = affordances
            .iter()
            .filter_map(|value| value.get("name").and_then(Value::as_str))
            .map(str::to_string)
            .collect::<Vec<_>>();
        let affordances = affordances
            .into_iter()
            .map(|value| serde_json::from_value(value).expect("tool affordance is typed"))
            .collect();
        let grant = GrantProjection::default()
            .with_goal(Some("complete the Responses turn".to_string()))
            .with_interface_affordances(names, affordances);
        InterfaceInvocationEnvelope {
            artifact: ArtifactRef {
                id: ArtifactId::new(format!("{current_label}:test")),
                content,
            },
            artifact_file,
            artifact_binding_payloads: bound,
            metadata: Some(InvocationMetadata::new(grant).with_runtime(
                RuntimeInvocationMetadata {
                    run: RunId::new("run-responses-test"),
                    ee: EeId::new("ee-responses-test"),
                    worker: InterfaceId::new("system_worker_test"),
                    checkpoint: CheckpointId::new("checkpoint-responses-test"),
                    interface: InterfaceId::new("world_provider_responses"),
                    epoch: 1,
                    run_record_hash: Some("run-record-hash".to_string()),
                },
            )),
        }
    }

    fn response_entry(output_index: usize, item: Value) -> Value {
        json!({
            "output_index": output_index,
            "raw_blake3": value_blake3(&item).expect("response item hashes"),
            "item": item,
        })
    }

    fn test_affordances() -> Vec<Value> {
        vec![json!({
            "name": "artifact.read",
            "description": "Read one bound artifact",
            "input_schema": {
                "type": "object",
                "properties": {"id": {"type": "string"}},
                "required": ["id"],
                "additionalProperties": false
            }
        })]
    }

    fn function_call_artifact(
        response_id: &str,
        output_index: usize,
        call_id: &str,
        argument: &str,
    ) -> Value {
        let item = json!({
            "type": "function_call",
            "id": format!("fc_{call_id}"),
            "status": "completed",
            "call_id": call_id,
            "name": "artifact-dread",
            "arguments": serde_json::to_string(&json!({"id": argument})).expect("arguments encode")
        });
        json!({
            "label": TOOL_CALL_LABEL,
            "response_id": response_id,
            "tool": "artifact.read",
            "arguments": {"id": argument},
            "correlation_id": call_id,
            "provider_call_id": call_id,
            "response_item_id": format!("fc_{call_id}"),
            "response_items": [response_entry(output_index, item)]
        })
    }

    fn tool_result_payload(call_id: &str, content: &str) -> Value {
        json!({
            "label": "artifact.read.result",
            "tool": "artifact.read",
            "correlation_id": call_id,
            "ok": true,
            "body": {"content": content}
        })
    }

    fn session_snapshot_binding(mut entries: Vec<(&str, Value)>) -> ArtifactBindingPayload {
        let (outer_label, mut outer_payload) = entries
            .pop()
            .expect("session snapshot has an outer artifact");
        let artifacts = entries
            .into_iter()
            .enumerate()
            .map(|(index, (label, payload))| {
                let bytes = serde_json::to_vec(&payload).expect("session payload encodes");
                json!({
                    "label": label,
                    "payload": payload,
                    "source_artifact": ArtifactRef {
                        id: ArtifactId::new(format!("session-source-{index}")),
                        content: ContentHash::of_bytes(&bytes),
                    },
                    "source_movement_seq": u64::try_from(index + 10).expect("movement seq fits")
                })
            })
            .collect::<Vec<_>>();
        outer_payload
            .as_object_mut()
            .expect("session snapshot outer payload is an object")
            .insert(
                SESSION_CONTEXT_FIELD.to_string(),
                json!({
                    "schema_version": 2,
                    "provider_interface": "world_provider_responses",
                    "artifacts": artifacts
                }),
            );
        payload_binding(&format!("{outer_label}:snapshot"), outer_payload)
    }

    fn sse_body(events: Vec<Value>) -> Vec<u8> {
        let mut body = String::new();
        for (index, mut event) in events.into_iter().enumerate() {
            event
                .as_object_mut()
                .expect("SSE event is an object")
                .insert("sequence_number".to_string(), json!(index + 1));
            body.push_str("data: ");
            body.push_str(&serde_json::to_string(&event).expect("SSE event encodes"));
            body.push_str("\n\n");
        }
        body.push_str("data: [DONE]\n\n");
        body.into_bytes()
    }

    #[test]
    fn native_sse_reconstructs_reasoning_parallel_calls_and_text() {
        let events = vec![
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "type": "reasoning",
                    "id": "rs_1",
                    "status": "in_progress",
                    "summary": [],
                    "content": []
                }
            }),
            json!({
                "type": "response.reasoning_summary_text.delta",
                "output_index": 0,
                "summary_index": 0,
                "item_id": "rs_1",
                "delta": "Plan"
            }),
            json!({
                "type": "response.reasoning_summary_text.done",
                "output_index": 0,
                "summary_index": 0,
                "item_id": "rs_1",
                "text": "Plan"
            }),
            json!({
                "type": "response.reasoning_text.delta",
                "output_index": 0,
                "content_index": 0,
                "item_id": "rs_1",
                "delta": "hidden"
            }),
            json!({
                "type": "response.reasoning_text.done",
                "output_index": 0,
                "content_index": 0,
                "item_id": "rs_1",
                "text": "hidden"
            }),
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "reasoning",
                    "id": "rs_1",
                    "status": "completed",
                    "summary": [{"type": "summary_text", "text": "Plan"}],
                    "content": [{"type": "reasoning_text", "text": "hidden"}],
                    "encrypted_content": "encrypted-rs-1"
                }
            }),
            json!({
                "type": "response.output_item.added",
                "output_index": 1,
                "item": {
                    "type": "function_call",
                    "id": "fc_1",
                    "status": "in_progress",
                    "call_id": "call_1",
                    "name": "artifact-dread",
                    "arguments": ""
                }
            }),
            json!({
                "type": "response.function_call_arguments.delta",
                "output_index": 1,
                "item_id": "fc_1",
                "call_id": "call_1",
                "name": "artifact-dread",
                "delta": "{\"id\":"
            }),
            json!({
                "type": "response.function_call_arguments.delta",
                "output_index": 1,
                "item_id": "fc_1",
                "delta": "\"one\"}"
            }),
            json!({
                "type": "response.function_call_arguments.done",
                "output_index": 1,
                "item_id": "fc_1",
                "arguments": "{\"id\":\"one\"}"
            }),
            json!({
                "type": "response.output_item.done",
                "output_index": 1,
                "item": {
                    "type": "function_call",
                    "id": "fc_1",
                    "status": "completed",
                    "call_id": "call_1",
                    "name": "artifact-dread",
                    "arguments": "{\"id\":\"one\"}"
                }
            }),
            json!({
                "type": "response.output_item.added",
                "output_index": 2,
                "item": {
                    "type": "function_call",
                    "id": "fc_2",
                    "status": "completed",
                    "call_id": "call_2",
                    "name": "artifact-dread",
                    "arguments": "{\"id\":\"two\"}"
                }
            }),
            json!({
                "type": "response.output_item.done",
                "output_index": 2,
                "item": {
                    "type": "function_call",
                    "id": "fc_2",
                    "status": "completed",
                    "call_id": "call_2",
                    "name": "artifact-dread",
                    "arguments": "{\"id\":\"two\"}"
                }
            }),
            json!({
                "type": "response.output_item.added",
                "output_index": 3,
                "item": {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "status": "in_progress",
                    "content": []
                }
            }),
            json!({
                "type": "response.output_text.delta",
                "output_index": 3,
                "content_index": 0,
                "item_id": "msg_1",
                "delta": "done"
            }),
            json!({
                "type": "response.output_text.done",
                "output_index": 3,
                "content_index": 0,
                "item_id": "msg_1",
                "text": "done"
            }),
            json!({
                "type": "response.output_item.done",
                "output_index": 3,
                "item": {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{"type": "output_text", "text": "done"}]
                }
            }),
            json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_1",
                    "object": "response",
                    "status": "completed",
                    "model": "gpt-test",
                    "output": [],
                    "usage": {"input_tokens": 10, "output_tokens": 8, "total_tokens": 18}
                }
            }),
        ];
        let events = events
            .into_iter()
            .enumerate()
            .map(|(index, mut event)| {
                event
                    .as_object_mut()
                    .expect("Responses event is an object")
                    .insert("sequence_number".to_string(), json!(index + 1));
                event
            })
            .collect::<Vec<_>>();
        let websocket = parse_websocket_events(&events, SequenceNumbers::RequiredMonotonic)
            .expect("native Responses websocket reconstructs");
        let body = sse_body(events);
        let mut decoder = ResponsesSseDecoder::new(SequenceNumbers::RequiredMonotonic);
        for chunk in body.chunks(19) {
            decoder.push(chunk).expect("Responses SSE chunk parses");
        }
        let parsed = decoder.finish().expect("native Responses SSE reconstructs");
        assert_eq!(websocket.response, parsed.response);
        let output = parsed
            .response
            .get("output")
            .and_then(Value::as_array)
            .expect("reconstructed output array");
        assert_eq!(output.len(), 4);
        assert_eq!(output[0].pointer("/summary/0/text"), Some(&json!("Plan")));
        assert_eq!(output[0].pointer("/content/0/text"), Some(&json!("hidden")));
        assert_eq!(output[1].get("call_id"), Some(&json!("call_1")));
        assert_eq!(output[2].get("call_id"), Some(&json!("call_2")));
        assert_eq!(output[3].pointer("/content/0/text"), Some(&json!("done")));
        assert_eq!(parsed.terminal_type, "response.completed");
        assert!(parsed.events.iter().all(|event| {
            event
                .get("data_blake3")
                .and_then(Value::as_str)
                .is_some_and(|value| value.len() == 64)
        }));
    }

    #[test]
    fn carriers_and_replay_preserve_media_files_and_tool_correlation() {
        let driver = driver("world_provider_responses");
        let affordances = vec![
            json!({
                "name": "artifact.read",
                "description": "Read one bound artifact",
                "input_schema": {
                    "type": "object",
                    "properties": {"id": {"type": "string"}},
                    "required": ["id"],
                    "additionalProperties": false
                }
            }),
            json!({
                "name": "file.write",
                "description": "Interface-owned run authoring tool",
                "input_schema": {"type": "object"}
            }),
        ];
        let input = invocation(
            "responses.input",
            json!({
                "items": [{
                    "type": "message",
                    "role": "user",
                    "content": [
                        {"type": "input_text", "text": "inspect every carrier"},
                        {
                            "type": "input_image",
                            "image_url": "data:image/png;base64,aW1hZ2U=",
                            "detail": "high"
                        },
                        {
                            "type": "input_file",
                            "file_data": "aGVsbG8=",
                            "filename": "note.txt"
                        },
                        {"type": "input_file", "file_id": "file_123"},
                        {
                            "type": "input_file",
                            "file_url": "https://files.example/document.pdf",
                            "detail": "high"
                        }
                    ]
                }]
            }),
            Vec::new(),
            affordances.clone(),
        );
        let tools = driver.translate_tools(&input).expect("tools translate");
        assert_eq!(tools.declarations.len(), 1);
        assert_eq!(
            tools.declarations[0].get("name"),
            Some(&json!("artifact-dread"))
        );
        let assembled = driver
            .assemble_input(&input, &tools)
            .expect("input assembles");
        assert!(!assembled.instructions.contains("file.write"));
        assert_eq!(
            assembled.items[0].pointer("/content/1/type"),
            Some(&json!("input_image"))
        );
        assert_eq!(
            assembled.items[0].pointer("/content/2/filename"),
            Some(&json!("note.txt"))
        );
        assert_eq!(
            assembled.items[0].pointer("/content/3/file_id"),
            Some(&json!("file_123"))
        );
        assert_eq!(
            assembled.items[0].pointer("/content/4/file_url"),
            Some(&json!("https://files.example/document.pdf"))
        );
        assert_eq!(
            assembled.items[0].pointer("/content/4/detail"),
            Some(&json!("high"))
        );
        let request = driver
            .build_request(&assembled, &tools)
            .expect("request builds");
        assert!(request
            .get("prompt_cache_key")
            .and_then(Value::as_str)
            .is_some_and(|value| value.starts_with("pck_") && value.len() == 28));

        let call_item = json!({
            "type": "function_call",
            "id": "fc_replay",
            "status": "completed",
            "call_id": "call_replay",
            "name": "artifact-dread",
            "arguments": "{\"id\":\"artifact-1\"}"
        });
        let tool_call = json!({
            "label": TOOL_CALL_LABEL,
            "response_id": "resp_replay",
            "tool": "artifact.read",
            "arguments": {"id": "artifact-1"},
            "correlation_id": "call_replay",
            "provider_call_id": "call_replay",
            "response_item_id": "fc_replay",
            "response_items": [response_entry(0, call_item)]
        });
        let result_input = invocation(
            "artifact.read.result",
            json!({
                "label": "artifact.read.result",
                "tool": "artifact.read",
                "correlation_id": "call_replay",
                "ok": true,
                "body": {"content": "bound result"}
            }),
            vec![payload_binding("tool.call:replay", tool_call)],
            affordances,
        );
        let result_tools = driver
            .translate_tools(&result_input)
            .expect("result tools translate");
        let replay = driver
            .assemble_input(&result_input, &result_tools)
            .expect("correlated replay assembles");
        assert_eq!(replay.items.len(), 2);
        assert_eq!(replay.items[0].get("type"), Some(&json!("function_call")));
        assert_eq!(replay.items[0].get("call_id"), Some(&json!("call_replay")));
        assert!(replay.items[0].get("id").is_none());
        assert!(replay.items[0].get("status").is_none());
        assert_eq!(
            replay.items[1],
            json!({
                "type": "function_call_output",
                "call_id": "call_replay",
                "output": "bound result"
            })
        );
        let reasoning = driver
            .normalize_replay_item(&json!({
                "type": "reasoning",
                "id": "rs_replay",
                "status": "completed",
                "summary": [{"type": "summary_text", "text": "summary"}],
                "content": [{"type": "reasoning_text", "text": "private"}],
                "encrypted_content": "encrypted-replay",
                "replay_scope": "openai-responses-api"
            }))
            .expect("reasoning replay validates")
            .expect("reasoning replay remains available");
        assert!(reasoning.get("id").is_none());
        assert!(reasoning.get("status").is_none());
        assert!(reasoning.get("replay_scope").is_none());
        assert_eq!(
            reasoning.get("encrypted_content"),
            Some(&json!("encrypted-replay"))
        );
        assert!(driver
            .normalize_replay_item(&json!({
                "type": "reasoning",
                "status": "completed",
                "summary": [{"type": "summary_text", "text": "summary only"}],
                "replay_scope": "openai-responses-api"
            }))
            .expect("summary-only reasoning validates")
            .is_none());
    }

    #[test]
    fn session_snapshot_prior_history_keeps_outer_current_once_with_provenance() {
        let prior_payload = json!({"label": "interface.input", "prompt": "prior"});
        let prior_bytes = serde_json::to_vec(&prior_payload).expect("prior payload encodes");
        let input = invocation(
            "interface.input",
            json!({
                "label": "interface.input",
                "prompt": "latest",
                "session_context": {
                    "schema_version": 2,
                    "provider_interface": "world_provider_responses",
                    "artifacts": [{
                        "label": "interface.input",
                        "payload": prior_payload,
                        "source_artifact": ArtifactRef {
                            id: ArtifactId::new("interface.input:prior"),
                            content: ContentHash::of_bytes(&prior_bytes),
                        },
                        "source_movement_seq": 41
                    }]
                }
            }),
            Vec::new(),
            test_affordances(),
        );
        let artifacts = context_artifacts(&input, "world_provider_responses", 77)
            .expect("session snapshot expands");
        let messages = artifacts
            .iter()
            .filter(|artifact| artifact.label == "interface.input")
            .collect::<Vec<_>>();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].value.get("prompt"), Some(&json!("prior")));
        assert_eq!(messages[0].source_movement_seq, Some(41));
        assert!(!messages[0].current);
        assert_eq!(messages[1].value.get("prompt"), Some(&json!("latest")));
        assert!(messages[1].value.get(SESSION_CONTEXT_FIELD).is_none());
        assert_eq!(messages[1].source_artifact, input.artifact);
        assert_eq!(messages[1].source_movement_seq, Some(77));
        assert!(messages[1].current);
    }

    #[tokio::test]
    async fn parallel_function_outputs_wait_and_replay_in_output_index_order() {
        let driver = driver("world_provider_responses");
        let affordances = test_affordances();
        let reasoning_item = json!({
            "type": "reasoning",
            "status": "completed",
            "summary": [{"type": "summary_text", "text": "read both"}],
            "content": [{"type": "reasoning_text", "text": "private"}],
            "encrypted_content": "encrypted-parallel-reasoning",
            "replay_scope": "openai-responses-api"
        });
        let reasoning = json!({
            "label": REASONING_EVIDENCE_LABEL,
            "response_id": "resp_parallel",
            "model": "gpt-test",
            "replay_scope": "openai-responses-api",
            "summary_text": ["read both"],
            "reasoning_text": ["private"],
            "response_items": [response_entry(0, reasoning_item)]
        });
        let call_one = function_call_artifact("resp_parallel", 1, "call_one", "one");
        let call_two = function_call_artifact("resp_parallel", 2, "call_two", "two");
        let first_result = tool_result_payload("call_two", "second arrived first");
        let partial = invocation(
            "artifact.read.result",
            first_result.clone(),
            vec![
                payload_binding("provider.reasoning:parallel", reasoning.clone()),
                payload_binding("tool.call:one", call_one.clone()),
                payload_binding("tool.call:two", call_two.clone()),
            ],
            affordances.clone(),
        );
        let output_root = tempfile::tempdir().expect("pending output root");
        let tools = driver
            .translate_tools(&partial)
            .expect("partial tools translate");
        let pending_state = match driver
            .prepare_input(&partial, 1, &tools)
            .expect("partial fan-in prepares")
        {
            InputAssembly::Awaiting(pending) => pending,
            InputAssembly::Ready(_) => panic!("partial fan-in unexpectedly became ready"),
        };
        let emissions = driver
            .materialize_pending_tool_results(output_root.path(), pending_state)
            .expect("partial fan-in materializes without provider transport");
        assert_eq!(emissions.len(), 1);
        assert_eq!(emissions[0].route, ProviderEmissionRoute::ClientProjection);
        let pending: Value = serde_json::from_slice(
            &emissions[0]
                .artifact
                .read_payload_bytes()
                .expect("pending payload reads"),
        )
        .expect("pending payload parses");
        assert_eq!(
            pending.get("label"),
            Some(&json!(TOOL_RESULTS_PENDING_LABEL))
        );
        assert_eq!(pending.get("generation"), Some(&json!(1)));
        assert_eq!(pending.get("received_call_ids"), Some(&json!(["call_two"])));

        let completed = invocation(
            "artifact.read.result",
            tool_result_payload("call_one", "first arrived second"),
            vec![session_snapshot_binding(vec![
                (REASONING_EVIDENCE_LABEL, reasoning),
                (TOOL_CALL_LABEL, call_one),
                (TOOL_CALL_LABEL, call_two),
                ("artifact.read.result", first_result),
                (TOOL_RESULTS_PENDING_LABEL, pending),
            ])],
            affordances,
        );
        let tools = driver
            .translate_tools(&completed)
            .expect("completed tools translate");
        let InputAssembly::Ready(assembled) = driver
            .prepare_input(&completed, 1, &tools)
            .expect("complete fan-in assembles")
        else {
            panic!("complete fan-in remained pending");
        };
        assert_eq!(
            assembled
                .items
                .iter()
                .map(|item| item.get("type").and_then(Value::as_str).unwrap_or(""))
                .collect::<Vec<_>>(),
            vec![
                "reasoning",
                "function_call",
                "function_call",
                "function_call_output",
                "function_call_output"
            ]
        );
        assert_eq!(
            assembled.items[0].get("encrypted_content"),
            Some(&json!("encrypted-parallel-reasoning"))
        );
        assert_eq!(assembled.items[1].get("call_id"), Some(&json!("call_one")));
        assert_eq!(assembled.items[2].get("call_id"), Some(&json!("call_two")));
        assert_eq!(assembled.items[3].get("call_id"), Some(&json!("call_one")));
        assert_eq!(assembled.items[4].get("call_id"), Some(&json!("call_two")));
    }

    #[test]
    fn function_output_groups_reject_unknown_duplicate_and_mixed_results() {
        let driver = driver("world_provider_responses");
        let affordances = test_affordances();
        let call_one = function_call_artifact("resp_one", 0, "call_one", "one");
        let call_two = function_call_artifact("resp_one", 1, "call_two", "two");

        let unknown = invocation(
            "artifact.read.result",
            tool_result_payload("call_unknown", "unknown"),
            vec![payload_binding("tool.call:one", call_one.clone())],
            affordances.clone(),
        );
        let tools = driver
            .translate_tools(&unknown)
            .expect("unknown tools translate");
        assert!(driver
            .prepare_input(&unknown, 1, &tools)
            .expect_err("unknown result must fail")
            .detail
            .contains("no bound tool.call artifact"));

        let duplicate = invocation(
            "artifact.read.result",
            tool_result_payload("call_two", "two"),
            vec![
                payload_binding("tool.call:one", call_one.clone()),
                payload_binding("tool.call:two", call_two.clone()),
                payload_binding(
                    "artifact.read.result:first",
                    tool_result_payload("call_one", "one"),
                ),
                payload_binding(
                    "artifact.read.result:duplicate",
                    tool_result_payload("call_one", "one again"),
                ),
            ],
            affordances.clone(),
        );
        let tools = driver
            .translate_tools(&duplicate)
            .expect("duplicate tools translate");
        assert!(driver
            .prepare_input(&duplicate, 1, &tools)
            .expect_err("duplicate result must fail")
            .detail
            .contains("repeats tool result correlation_id"));

        let mixed = invocation(
            "artifact.read.result",
            tool_result_payload("call_one", "one"),
            vec![
                payload_binding("tool.call:one", call_one),
                payload_binding("tool.call:two", call_two),
                payload_binding(
                    "tool.call:three",
                    function_call_artifact("resp_two", 0, "call_three", "three"),
                ),
                payload_binding(
                    "tool.call:four",
                    function_call_artifact("resp_two", 1, "call_four", "four"),
                ),
                payload_binding(
                    "artifact.read.result:three",
                    tool_result_payload("call_three", "three"),
                ),
            ],
            affordances,
        );
        let tools = driver
            .translate_tools(&mixed)
            .expect("mixed tools translate");
        assert!(driver
            .prepare_input(&mixed, 1, &tools)
            .expect_err("mixed unresolved groups must fail")
            .detail
            .contains("mixes unresolved function_call groups"));
    }

    #[test]
    fn xai_native_items_are_durable_before_the_answer() {
        let driver = driver("world_provider_xai");
        let response = json!({
            "id": "resp_xai",
            "object": "response",
            "status": "completed",
            "model": "grok-4.5",
            "output": [
                {
                    "type": "x_search_call",
                    "id": "xs_1",
                    "status": "completed",
                    "action": {"query": "Octopus"}
                },
                {
                    "type": "code_interpreter_call",
                    "id": "ci_1",
                    "status": "completed",
                    "code": "print(1)",
                    "outputs": [{"type": "logs", "logs": "1"}]
                },
                {
                    "type": "message",
                    "id": "msg_xai",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{"type": "output_text", "text": "finished"}]
                }
            ],
            "usage": {"input_tokens": 4, "output_tokens": 5, "total_tokens": 9}
        });
        let response_bytes = serde_json::to_vec(&response).expect("response encodes");
        let root = tempfile::tempdir().expect("temporary emission root");
        let emissions = driver
            .materialize_response(
                ResponsesEnvelope {
                    response,
                    request_bytes: br#"{"model":"grok-4.5"}"#.to_vec(),
                    response_evidence: ResponseBodyEvidence {
                        bytes: response_bytes.len(),
                        blake3: blake3::hash(&response_bytes).to_hex().to_string(),
                        raw: Some(response_bytes),
                    },
                    status: 200,
                    headers: json!({"x-request-id": "request-xai"}),
                    stream_events: vec![json!({"type": "response.completed"})],
                },
                root.path(),
                &ToolCatalog {
                    declarations: Vec::new(),
                    by_wire_name: BTreeMap::new(),
                    hidden_tool_names: Vec::new(),
                },
            )
            .expect("xAI output materializes");
        let labels = emissions
            .iter()
            .map(|emission| {
                emission
                    .artifact
                    .read_manifest()
                    .expect("emission manifest reads")
                    .label
            })
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            vec![
                TRANSPORT_EVIDENCE_LABEL,
                USAGE_EVIDENCE_LABEL,
                NATIVE_TOOL_EVIDENCE_LABEL,
                NATIVE_TOOL_EVIDENCE_LABEL,
                INTERFACE_ARTIFACT_OUTPUT_LABEL,
            ]
        );
        for emission in &emissions[2..4] {
            let payload: Value = serde_json::from_slice(
                &emission
                    .artifact
                    .read_payload_bytes()
                    .expect("native evidence payload reads"),
            )
            .expect("native evidence payload parses");
            assert!(payload
                .pointer("/response_items/0/raw_blake3")
                .and_then(Value::as_str)
                .is_some_and(|value| value.len() == 64));
            let item = payload
                .pointer("/response_items/0/item")
                .expect("native evidence preserves the raw item");
            let replay = driver
                .normalize_replay_item(item)
                .expect("native replay validates")
                .expect("native replay remains available");
            assert!(replay.get("id").is_none());
        }
    }

    #[tokio::test]
    async fn unauthenticated_transport_and_omitted_cache_controls_stay_off_the_wire() {
        let driver = driver("world_provider_ollama_responses");
        assert_eq!(
            driver.endpoint.as_str(),
            "http://localhost:11434/v1/responses"
        );
        assert_eq!(driver.model, "qwen3:8b");
        assert!(driver
            .auth
            .header_pair()
            .await
            .expect("none auth")
            .is_none());
        assert!(driver.options.cache.is_none());
        assert!(driver.options.function_tools.strict.is_none());
        assert!(driver.options.function_tools.tool_choice.is_none());
        assert!(driver.options.function_tools.parallel_tool_calls.is_none());
        let input = invocation(
            "interface.input",
            json!({"prompt": "Use the declared tool once."}),
            Vec::new(),
            test_affordances(),
        );
        let tools = driver.translate_tools(&input).expect("tools translate");
        assert_eq!(tools.declarations.len(), 1);
        assert!(tools.declarations[0].get("strict").is_none());
        let request = driver
            .build_request(
                &AssembledInput {
                    instructions: "Use only declared affordances.".to_string(),
                    items: vec![json!({
                        "type": "message",
                        "role": "user",
                        "content": [{"type": "input_text", "text": "Read it."}]
                    })],
                },
                &tools,
            )
            .expect("Ollama request builds");
        for field in [
            "store",
            "prompt_cache_key",
            "tool_choice",
            "parallel_tool_calls",
            "previous_response_id",
            "conversation",
            "truncation",
        ] {
            assert!(request.get(field).is_none(), "unexpected field {field}");
        }
        assert_eq!(request.get("stream"), Some(&Value::Bool(true)));
        assert!(request.get("tools").is_some_and(Value::is_array));
    }

    #[test]
    fn none_auth_and_optional_cache_combinations_fail_closed() {
        let mut none_with_header = resolved_descriptor("world_provider_ollama_responses");
        let transport = none_with_header
            .interface_driver_config
            .as_mut()
            .expect("driver config")
            .transport
            .as_mut()
            .expect("transport");
        transport.auth_header = Some("Authorization".to_string());
        transport.auth_value_prefix = Some("Bearer ".to_string());
        assert!(driver_error(&none_with_header).contains("none auth_source"));

        let mut invalid_cache = resolved_descriptor("world_provider_ollama_responses");
        invalid_cache
            .interface_driver_config
            .as_mut()
            .expect("driver config")
            .private_config
            .get_mut("responses_api")
            .and_then(Value::as_object_mut)
            .expect("Responses config")
            .insert(
                "cache".to_string(),
                json!({
                    "mode": "static_prefix_blake3",
                    "field": "unsupported_cache_key",
                    "prefix": "bad_",
                    "digest_characters": 24
                }),
            );
        assert!(driver_error(&invalid_cache).contains("prompt_cache_key"));
    }

    #[test]
    fn exact_ollama_and_custom_responses_descriptors_construct_with_strict_life() {
        let _environment = ENVIRONMENT.lock().expect("environment lock");
        let _restore = EnvironmentRestore::set(&[
            (
                "CUSTOM_RESPONSES_BASE_URL",
                "https://responses.example.test",
            ),
            ("CUSTOM_RESPONSES_MODEL", "custom-model"),
        ]);
        let ollama = resolved_descriptor("world_provider_ollama_responses");
        let custom = resolved_descriptor("world_provider_custom_responses_bearer");
        for descriptor in [&ollama, &custom] {
            assert!(serde_json::to_value(&descriptor)
                .unwrap()
                .get("reference")
                .is_none());
            assert_eq!(descriptor.interface_driver_ref, INTERFACE_DRIVER_REF);
        }
        let custom_driver = OpenAiResponsesDriver::from_descriptor(
            &custom,
            tempfile::tempdir().expect("temporary secrets home").keep(),
        )
        .expect("custom Responses driver resolves");
        assert_eq!(
            custom_driver.endpoint.as_str(),
            "https://responses.example.test/v1/responses"
        );
        assert_eq!(custom_driver.model, "custom-model");
        assert!(custom_driver.options.cache.is_none());
        assert!(matches!(
            custom_driver.auth.source,
            AuthSource::Secret { .. }
        ));

        for name in [
            "world_provider_ollama_responses",
            "world_provider_custom_responses_bearer",
        ] {
            let bytes = std::fs::read(interface_registry_root().join(name).join("life.json"))
                .expect("life descriptor reads");
            let life: octopus_interface_transport::SpawnLifeConfig =
                serde_json::from_slice(&bytes).expect("life descriptor parses strictly");
            assert_eq!(life.processes.len(), 1);
            let process = &life.processes[0];
            assert!(process
                .coordinate_root_slots
                .contains("interface_hand_in_root"));
            assert!(process
                .coordinate_root_slots
                .contains("app_server_control_root"));
            assert!(process
                .authority
                .write_root_slots
                .contains("interface_hand_in_root"));
            assert!(process.authority.allow_network);
            assert!(process.authority.allow_local_ipc);
        }
    }

    #[test]
    fn exact_model_profiles_select_responses_cache_replay_and_request_fields() {
        let driver = driver("world_provider_opencode_zen_responses");
        let codex = driver.options.for_provider_model("gpt-5.3-codex").unwrap();
        let gpt = driver.options.for_provider_model("gpt-5.4").unwrap();
        assert_eq!(
            codex.request_fields.get("max_output_tokens"),
            Some(&json!(128000))
        );
        assert_eq!(
            gpt.request_fields.get("max_output_tokens"),
            Some(&json!(128000))
        );
        assert_eq!(codex.cache.as_ref().unwrap().prefix, "oczg53c_");
        assert_eq!(gpt.cache.as_ref().unwrap().prefix, "oczg54_");
        assert_eq!(codex.replay.scope, "opencode-zen-gpt-5-3-codex-responses");
        assert_eq!(gpt.replay.scope, "opencode-zen-gpt-5-4-responses");
        assert!(driver.options.for_provider_model("unformed-model").is_err());
    }

    #[test]
    fn concrete_responses_interfaces_and_life_contract_fail_closed() {
        let family = resolved_descriptor("world_provider_responses");
        let family_driver = driver("world_provider_responses");
        assert_eq!(
            family_driver.endpoint.as_str(),
            "https://api.openai.com/v1/responses"
        );
        assert_eq!(family_driver.model, "gpt-5.6-sol");
        assert_eq!(
            family_driver
                .options
                .cache
                .as_ref()
                .expect("family cache")
                .field,
            "prompt_cache_key"
        );
        assert!(family_driver.options.usage_required);
        for name in [
            "world_provider_openai",
            "world_provider_openai_responses_websocket",
            "world_provider_xai",
        ] {
            let resolved = resolved_descriptor(name);
            assert!(serde_json::to_value(&resolved)
                .unwrap()
                .get("reference")
                .is_none());
            assert_eq!(resolved.interface_driver_ref, INTERFACE_DRIVER_REF);
            let transport = resolved
                .interface_driver_config
                .as_ref()
                .expect("resolved driver config")
                .transport
                .as_ref()
                .expect("resolved transport");
            assert!(transport
                .auth_secret_ref
                .as_deref()
                .is_some_and(|value| value.starts_with("secret://")));
            assert!(transport.auth_env_key.is_none());
            let sibling_driver = driver(name);
            assert_eq!(
                sibling_driver
                    .options
                    .cache
                    .as_ref()
                    .expect("sibling cache")
                    .field,
                "prompt_cache_key"
            );
            assert!(sibling_driver.options.usage_required);
        }
        let websocket_driver = driver("world_provider_openai_responses_websocket");
        assert_eq!(
            websocket_driver.options.operation.response_mode,
            ResponseMode::Websocket
        );
        assert_eq!(
            websocket_driver
                .websocket
                .as_ref()
                .expect("websocket transport resolves")
                .endpoint()
                .as_str(),
            "wss://api.openai.com/v1/responses"
        );
        let before_request = websocket_driver.map_websocket_failure(ResponsesWebSocketFailure {
            kind: WebSocketFailureKind::Transport,
            stage: WebSocketFailureStage::BeforeRequest,
            status: Some(503),
            headers: Box::new(HeaderMap::new()),
            bytes: Vec::new(),
            detail: "connect refused".to_string(),
        });
        assert!(before_request.retryable);
        let after_request = websocket_driver.map_websocket_failure(ResponsesWebSocketFailure {
            kind: WebSocketFailureKind::Transport,
            stage: WebSocketFailureStage::AfterRequest,
            status: Some(101),
            headers: Box::new(HeaderMap::new()),
            bytes: b"partial".to_vec(),
            detail: "connection closed".to_string(),
        });
        assert!(!after_request.retryable);
        assert!(after_request.detail.contains("was not replayed"));
        let xai_driver = driver("world_provider_xai");
        assert!(xai_driver
            .native_tool_output_types
            .contains("x_search_call"));
        assert!(xai_driver
            .native_tool_output_types
            .contains("code_interpreter_call"));

        let mut missing_timeout = family.clone();
        missing_timeout
            .interface_driver_config
            .as_mut()
            .expect("driver config")
            .transport
            .as_mut()
            .expect("transport")
            .request_timeout_ms = None;
        assert!(driver_error(&missing_timeout).contains("request_timeout_ms is required"));

        let mut missing_header = family.clone();
        missing_header
            .interface_driver_config
            .as_mut()
            .expect("driver config")
            .transport
            .as_mut()
            .expect("transport")
            .auth_header = None;
        assert!(driver_error(&missing_header).contains("auth_header is required"));

        let mut missing_prefix = family.clone();
        missing_prefix
            .interface_driver_config
            .as_mut()
            .expect("driver config")
            .transport
            .as_mut()
            .expect("transport")
            .auth_value_prefix = None;
        assert!(driver_error(&missing_prefix).contains("auth_value_prefix is required"));

        let mut missing_base = family.clone();
        missing_base
            .interface_driver_config
            .as_mut()
            .expect("driver config")
            .transport
            .as_mut()
            .expect("transport")
            .base_url = String::new();
        assert!(driver_error(&missing_base).contains("requires transport.base_url"));

        let mut missing_endpoint = family.clone();
        missing_endpoint
            .interface_driver_config
            .as_mut()
            .expect("driver config")
            .private_config
            .get_mut("responses_api")
            .and_then(Value::as_object_mut)
            .and_then(|value| value.get_mut("operation"))
            .and_then(Value::as_object_mut)
            .expect("operation config")
            .insert("path".to_string(), Value::String(String::new()));
        assert!(driver_error(&missing_endpoint).contains("operation.path"));

        let life_bytes = std::fs::read(
            interface_registry_root()
                .join("world_provider_responses")
                .join("life.json"),
        )
        .expect("life descriptor reads");
        let life: octopus_interface_transport::SpawnLifeConfig =
            serde_json::from_slice(&life_bytes).expect("life descriptor parses strictly");
        assert_eq!(life.processes.len(), 1);
        let process = &life.processes[0];
        assert!(process
            .coordinate_root_slots
            .contains("interface_hand_in_root"));
        assert!(process
            .coordinate_root_slots
            .contains("app_server_control_root"));
        assert!(process
            .authority
            .write_root_slots
            .contains("interface_hand_in_root"));
        assert!(process.authority.allow_network);
        assert!(process.authority.allow_local_ipc);
        assert_eq!(
            process.inherit_env_from_interface,
            ["/interface_driver_config/private_config/process_env_names".to_string()]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn authenticated_refresh_signal_is_redacted_and_not_retryable() {
        let failure = ResponsesFailure::authenticated_refresh_required(
            401,
            json!({"x-request-id": "request-1"}),
            b"Bearer secret-value",
        );
        assert!(!failure.retryable);
        let encoded = serde_json::to_string(&failure.evidence).unwrap();
        assert!(encoded.contains("response_blake3"));
        assert!(!encoded.contains("secret-value"));
        assert!(!encoded.contains("Bearer"));
        assert!(matches!(
            ResponsesDriverFailure::AuthenticatedRefreshRequired(failure),
            ResponsesDriverFailure::AuthenticatedRefreshRequired(_)
        ));
    }

    #[test]
    fn cloned_interface_secret_scope_must_be_rebased() {
        let mut cloned = resolved_descriptor("world_provider_responses");
        cloned.id = InterfaceId::new("world_provider_responses_clone");
        let secret_home = tempfile::tempdir().unwrap();
        let error =
            OpenAiResponsesDriver::from_descriptor(&cloned, secret_home.path().to_path_buf())
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
            Some("secret://world_provider_responses_clone/RESPONSES_API_KEY".to_string());
        OpenAiResponsesDriver::from_descriptor(&cloned, secret_home.path().to_path_buf()).unwrap();
    }
}
