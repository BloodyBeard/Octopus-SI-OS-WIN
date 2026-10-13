use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use octopus_descriptors::InterfaceId;
use octopus_interfaces::{
    InterfaceArtifactFile, InterfaceDescriptor, InterfaceInvocationEnvelope,
    InterfaceTransportConfig,
};
use octopus_ledger::ArtifactRef;
use octopus_model_context_render::{
    model_hidden_tool_names, model_tool_exclusions, model_visible_affordances,
    render_model_context_block_with_hidden_tools,
};
use octopus_provider_run_authoring::{
    ProviderEmission, ProviderEmissionRoute, ProviderOutputHandoff,
};
use octopus_tool_handlers::ToolResultArtifact;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Method, StatusCode, Url};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::dispatch::DispatchAdmission;
use tokio::sync::Mutex;

use crate::auth::{aws_percent_encode, AuthFailure, AuthorizationConfig, AwsAuthorizer};
use crate::eventstream::{decode_messages, EventStreamMessage};

pub const INTERFACE_DRIVER_REF: &str = "aws_bedrock_converse";

const CONTEXT_REFERENCE_LABEL: &str = "context.reference";
const INTERFACE_OUTPUT_LABEL: &str = "interface.artifact.output";
const TOOL_CALL_LABEL: &str = "tool.call";
const PROVIDER_REASONING_LABEL: &str = "provider.reasoning";
const PROVIDER_USAGE_LABEL: &str = "provider.usage";
const PROVIDER_TRANSPORT_LABEL: &str = "provider.transport.evidence";
const PROVIDER_ERROR_LABEL: &str = "provider.error";
const TOOL_RESULTS_PENDING_LABEL: &str = "provider.tool_results.pending";
const SESSION_CONTEXT_FIELD: &str = "session_context";
const ABSOLUTE_MAX_RESPONSE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum BedrockOperation {
    Converse,
    ConverseStream,
}

impl BedrockOperation {
    fn wire(self) -> &'static str {
        match self {
            Self::Converse => "aws_bedrock_converse",
            Self::ConverseStream => "aws_bedrock_converse_stream",
        }
    }

    fn path(self, paths: &ConversePaths) -> &str {
        match self {
            Self::Converse => &paths.converse,
            Self::ConverseStream => &paths.converse_stream,
        }
    }

    fn streamed(self) -> bool {
        matches!(self, Self::ConverseStream)
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversePaths {
    converse: String,
    converse_stream: String,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ReasoningReplay {
    Verbatim,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CarrierPolicy {
    max_inline_bytes: u64,
    max_images_per_message: usize,
    max_documents_per_message: usize,
    max_image_bytes: u64,
    max_document_bytes: u64,
    allowed_image_formats: Vec<String>,
    allowed_document_formats: Vec<String>,
    rules: Vec<CarrierRule>,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "encoding", rename_all = "snake_case", deny_unknown_fields)]
enum CarrierRule {
    JsonText {
        label: String,
        role: String,
        text_pointer: String,
    },
    BedrockMessage {
        label: String,
    },
}

impl CarrierRule {
    fn label(&self) -> &str {
        match self {
            Self::JsonText { label, .. } | Self::BedrockMessage { label } => label,
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountLimits {
    max_in_flight: u32,
    requests_per_window: usize,
    window_ms: u64,
    max_request_bytes: usize,
    max_messages: usize,
    max_content_blocks: usize,
    max_tools: usize,
    max_tool_input_bytes: usize,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryConfig {
    transport: bool,
    auth_transport: bool,
    base_delay_ms: u64,
    max_delay_ms: u64,
    retryable_statuses: Vec<u16>,
    retryable_error_codes: Vec<String>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelCatalogConfig {
    request_label: String,
    output_label: String,
    base_url: String,
    signing_name: String,
    operations: Vec<CatalogOperation>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CatalogOperationKind {
    FoundationModels,
    InferenceProfiles,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogOperation {
    kind: CatalogOperationKind,
    method: String,
    path: String,
    max_pages: usize,
    page_size: Option<u32>,
    type_equals: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BedrockOptions {
    operation: BedrockOperation,
    region: String,
    signing_name: String,
    paths: ConversePaths,
    authorization: AuthorizationConfig,
    inference_config: Map<String, Value>,
    request_fields: Map<String, Value>,
    tools_enabled: bool,
    tool_choice: Option<Value>,
    reasoning_replay: ReasoningReplay,
    input_content_types: Vec<String>,
    response_content_types: Vec<String>,
    carrier_policy: CarrierPolicy,
    account_limits: AccountLimits,
    retry: RetryConfig,
    model_catalog: Option<ModelCatalogConfig>,
}

#[derive(Clone)]
struct DriverConfig {
    interface_id: String,
    model: String,
    options: BedrockOptions,
    private_config: Map<String, Value>,
    input_content_types: BTreeSet<String>,
    response_content_types: BTreeSet<String>,
}

pub struct BedrockConverseDriver {
    endpoint: Url,
    headers: HeaderMap,
    authorizer: AwsAuthorizer,
    catalog: Option<CatalogRuntime>,
    config: DriverConfig,
    max_response_bytes: usize,
    http: Client,
    limiter: RequestLimiter,
}

struct CatalogRuntime {
    config: ModelCatalogConfig,
    base_url: Url,
    authorizer: AwsAuthorizer,
}

#[derive(Clone)]
struct RequestLimiter {
    limit: usize,
    window: Duration,
    state: Arc<Mutex<VecDeque<Instant>>>,
}

#[derive(Clone, Debug)]
pub struct BedrockFailure {
    pub class: &'static str,
    pub retryable: bool,
    pub status: Option<u16>,
    pub code: Option<String>,
    pub detail: String,
    pub response_bytes: Option<usize>,
    pub response_blake3: Option<String>,
    pub provider_error: Option<Box<Value>>,
}

struct ProviderResponse {
    model: String,
    stop_reason: String,
    content: Vec<Value>,
    usage: Value,
    metrics: Value,
    additional_fields: Option<Value>,
    transport: TransportEvidence,
}

struct TransportEvidence {
    operation: BedrockOperation,
    status: u16,
    request_bytes: usize,
    request_blake3: String,
    response_bytes: usize,
    response_blake3: String,
    event_count: usize,
    event_types: BTreeMap<String, usize>,
}

struct ResponseBodyEvidence {
    bytes: usize,
    blake3: String,
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
    current: bool,
}

struct AssembledTurn {
    system: Vec<Value>,
    messages: Vec<Value>,
}

enum TurnAssembly {
    Ready(AssembledTurn),
    AwaitingToolResults(Vec<PendingToolGroup>),
}

struct PendingToolGroup {
    provider_turn_id: String,
    expected: usize,
    received: usize,
    pending_correlation_ids: Vec<String>,
}

struct BedrockToolGroup {
    output_index: usize,
    call_ids: Vec<String>,
    call_specs: BTreeMap<String, (String, Value)>,
    call_indexes: BTreeMap<String, usize>,
    result_indexes: BTreeMap<String, usize>,
}

struct StreamAccumulator {
    role: Option<String>,
    blocks: BTreeMap<usize, StreamBlock>,
    stopped_blocks: BTreeSet<usize>,
    stop_reason: Option<String>,
    usage: Option<Value>,
    metrics: Option<Value>,
    additional_fields: Option<Value>,
    message_started: bool,
    message_stopped: bool,
    metadata_seen: bool,
    event_types: BTreeMap<String, usize>,
}

struct EventStreamFrameDecoder {
    pending: Vec<u8>,
    accumulator: StreamAccumulator,
    frame_count: usize,
}

enum StreamBlock {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        input: String,
        server_type: Option<String>,
    },
    Reasoning {
        text: String,
        signature: String,
        redacted: Vec<u8>,
    },
    Image {
        format: String,
        bytes: Vec<u8>,
    },
    ToolResult {
        id: String,
        status: Option<String>,
        result_type: Option<String>,
        content: Vec<Value>,
    },
    Citations {
        text: String,
        citations: Vec<Value>,
    },
}

impl BedrockFailure {
    fn contract(detail: impl Into<String>) -> Self {
        Self {
            class: "contract",
            retryable: false,
            status: None,
            code: None,
            detail: detail.into(),
            response_bytes: None,
            response_blake3: None,
            provider_error: None,
        }
    }

    fn transport(detail: impl Into<String>, retryable: bool) -> Self {
        Self {
            class: "transport",
            retryable,
            status: None,
            code: None,
            detail: detail.into(),
            response_bytes: None,
            response_blake3: None,
            provider_error: None,
        }
    }

    fn auth(failure: AuthFailure, retryable: bool) -> Self {
        Self {
            class: "auth",
            retryable,
            status: None,
            code: None,
            detail: failure.detail,
            response_bytes: None,
            response_blake3: None,
            provider_error: Some(Box::new(failure.evidence)),
        }
    }

    fn endpoint(status: u16, code: Option<String>, bytes: &[u8], retryable: bool) -> Self {
        let provider_error = serde_json::from_slice::<Value>(bytes)
            .ok()
            .filter(Value::is_object);
        let message = provider_error
            .as_ref()
            .and_then(|value| value.get("message").or_else(|| value.get("Message")))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        Self {
            class: "endpoint",
            retryable,
            status: Some(status),
            code,
            detail: match message {
                Some(message) => format!("Bedrock endpoint returned HTTP {status}: {message}"),
                None => format!("Bedrock endpoint returned HTTP {status}"),
            },
            response_bytes: Some(bytes.len()),
            response_blake3: Some(blake3::hash(bytes).to_hex().to_string()),
            provider_error: provider_error.map(Box::new),
        }
    }

    fn response(detail: impl Into<String>, bytes: &[u8]) -> Self {
        Self::response_evidence(
            detail,
            bytes.len(),
            blake3::hash(bytes).to_hex().to_string(),
        )
    }

    fn response_evidence(
        detail: impl Into<String>,
        response_bytes: usize,
        response_blake3: String,
    ) -> Self {
        Self {
            class: "response_contract",
            retryable: false,
            status: Some(200),
            code: None,
            detail: detail.into(),
            response_bytes: Some(response_bytes),
            response_blake3: Some(response_blake3),
            provider_error: None,
        }
    }

    fn stream_event_evidence(
        event_type: &str,
        detail: impl Into<String>,
        provider_error: Option<Value>,
        response_bytes: usize,
        response_blake3: String,
        retryable: bool,
    ) -> Self {
        Self {
            class: "stream_event",
            retryable,
            status: Some(200),
            code: Some(event_type.to_string()),
            detail: detail.into(),
            response_bytes: Some(response_bytes),
            response_blake3: Some(response_blake3),
            provider_error: provider_error.map(Box::new),
        }
    }

    fn partial_stream(
        detail: impl Into<String>,
        response_bytes: usize,
        response_blake3: String,
    ) -> Self {
        Self {
            class: "interrupted",
            retryable: false,
            status: Some(200),
            code: Some("partial_stream_replay_forbidden".to_string()),
            detail: detail.into(),
            response_bytes: Some(response_bytes),
            response_blake3: Some(response_blake3),
            provider_error: None,
        }
    }
}

impl BedrockConverseDriver {
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
        if transport.base_url_env.is_some()
            || transport.auth_env_key.is_some()
            || transport.auth_secret_ref.is_some()
            || transport.auth_header.is_some()
            || transport.auth_value_prefix.is_some()
        {
            return Err(
                "Bedrock transport must use only bedrock_converse.authorization; generic auth/base_url fallback fields are forbidden"
                    .to_string(),
            );
        }
        let options: BedrockOptions = serde_json::from_value(
            driver_config
                .private_config
                .get("bedrock_converse")
                .cloned()
                .ok_or_else(|| "Bedrock private_config.bedrock_converse is missing".to_string())?,
        )
        .map_err(|error| format!("Bedrock descriptor config is malformed: {error}"))?;
        validate_options(&options)?;
        if transport.wire.as_deref() != Some(options.operation.wire()) {
            return Err(format!(
                "Bedrock operation '{:?}' requires transport.wire='{}'",
                options.operation,
                options.operation.wire()
            ));
        }
        let process_env_names =
            metadata_string_array(&driver_config.private_config, "process_env_names")?;
        let model = metadata_string(&driver_config.private_config, "model")?;
        if model.len() > 2048 {
            return Err("Bedrock model exceeds 2048 bytes".to_string());
        }
        let base_url = validate_https_base_url(&transport.base_url, "Bedrock base_url")?;
        let endpoint =
            build_model_endpoint(&base_url, options.operation.path(&options.paths), &model)?;
        let headers = static_headers(transport)?;
        let expected_accept = if options.operation.streamed() {
            "application/vnd.amazon.eventstream"
        } else {
            "application/json"
        };
        if headers
            .get(reqwest::header::ACCEPT)
            .and_then(|value| value.to_str().ok())
            != Some(expected_accept)
        {
            return Err(format!(
                "Bedrock operation '{:?}' requires transport accept={expected_accept}",
                options.operation
            ));
        }
        let timeout_ms = transport
            .request_timeout_ms
            .ok_or_else(|| "Bedrock transport.request_timeout_ms is required".to_string())?;
        if timeout_ms == 0 {
            return Err("Bedrock request_timeout_ms must be positive".to_string());
        }
        let max_response_bytes = transport
            .max_response_bytes
            .ok_or_else(|| "Bedrock transport.max_response_bytes is required".to_string())?;
        if max_response_bytes == 0 || max_response_bytes > ABSOLUTE_MAX_RESPONSE_BYTES {
            return Err(format!(
                "Bedrock max_response_bytes must be within 1..={ABSOLUTE_MAX_RESPONSE_BYTES}"
            ));
        }
        let http = Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(timeout_ms))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| format!("Bedrock HTTP client build failed: {error}"))?;
        let authorizer = AwsAuthorizer::new(
            options.authorization.clone(),
            descriptor.id.as_str(),
            options.region.clone(),
            options.signing_name.clone(),
            activation_secrets_home,
            http.clone(),
            &process_env_names,
        )?;
        let catalog = options
            .model_catalog
            .as_ref()
            .map(|config| -> Result<CatalogRuntime, String> {
                let base_url =
                    validate_https_base_url(&config.base_url, "Bedrock model_catalog.base_url")?;
                let catalog_authorizer =
                    authorizer.for_endpoint(options.region.clone(), config.signing_name.clone())?;
                Ok(CatalogRuntime {
                    config: config.clone(),
                    base_url,
                    authorizer: catalog_authorizer,
                })
            })
            .transpose()?;
        let input_content_types = validate_content_types(
            &options.input_content_types,
            &[
                "text",
                "image",
                "document",
                "reasoningContent",
                "toolUse",
                "toolResult",
            ],
            "input_content_types",
        )?;
        let response_content_types = validate_content_types(
            &options.response_content_types,
            &[
                "text",
                "image",
                "reasoningContent",
                "toolUse",
                "toolResult",
                "citationsContent",
            ],
            "response_content_types",
        )?;
        let limiter = RequestLimiter {
            limit: options.account_limits.requests_per_window,
            window: Duration::from_millis(options.account_limits.window_ms),
            state: Arc::new(Mutex::new(VecDeque::new())),
        };
        Ok(Self {
            endpoint,
            headers,
            authorizer,
            catalog,
            config: DriverConfig {
                interface_id: descriptor.id.as_str().to_string(),
                model,
                options,
                private_config: driver_config.private_config.clone(),
                input_content_types,
                response_content_types,
            },
            max_response_bytes: max_response_bytes as usize,
            http,
            limiter,
        })
    }

    pub fn account_max_in_flight(&self) -> u32 {
        self.config.options.account_limits.max_in_flight
    }

    pub fn enforce_formed_model_policy(
        &self,
        input: &InterfaceInvocationEnvelope,
    ) -> Result<(), BedrockFailure> {
        if input
            .read_artifact_manifest()
            .map_err(|error| BedrockFailure::contract(error.to_string()))?
            .label
            == "provider.models.list"
        {
            return Ok(());
        }
        let models = required_formed_models(input)?;
        if !models.contains(&self.config.model) {
            return Err(BedrockFailure::contract(format!(
                "Bedrock selected model '{}' is outside formed interface policy models",
                self.config.model
            )));
        }
        Ok(())
    }

    pub fn retry_delay(&self, failed_attempt: u32) -> Duration {
        let multiplier = u64::from(failed_attempt.max(1));
        let delay = self
            .config
            .options
            .retry
            .base_delay_ms
            .saturating_mul(multiplier)
            .min(self.config.options.retry.max_delay_ms);
        Duration::from_millis(delay)
    }

    pub async fn receive_artifact_and_materialize_outputs(
        &self,
        input: InterfaceInvocationEnvelope,
        source_movement_seq: u64,
        attempt: u32,
        handoff: &ProviderOutputHandoff,
        output_root: &Path,
    ) -> Result<Vec<ProviderEmission>, BedrockFailure> {
        self.enforce_formed_model_policy(&input)?;
        let current_bytes = input
            .read_artifact_payload()
            .map_err(|error| BedrockFailure::contract(error.to_string()))?;
        let current: Value = serde_json::from_slice(&current_bytes).map_err(|error| {
            BedrockFailure::contract(format!("Bedrock input artifact is not JSON: {error}"))
        })?;
        let manifest = input
            .read_artifact_manifest()
            .map_err(|error| BedrockFailure::contract(error.to_string()))?;
        let dispatch = DispatchAdmission {
            handoff,
            output_root,
            provider_interface: &self.config.interface_id,
            source_movement_seq,
            attempt,
        };
        if self
            .catalog
            .as_ref()
            .is_some_and(|catalog| manifest.label == catalog.config.request_label)
        {
            return self
                .materialize_model_catalog(
                    &current,
                    output_root,
                    &dispatch,
                    &required_formed_models(&input)?,
                )
                .await;
        }
        if manifest.label == "provider.models.list" && self.catalog.is_none() {
            return Err(BedrockFailure::contract(
                "this Bedrock descriptor declares no model_catalog endpoint",
            ));
        }
        let selected = handoff
            .callable_tool_names(&input)
            .map_err(BedrockFailure::contract)?;
        let mut declared_hidden = model_hidden_tool_names(&self.config.private_config)
            .map_err(BedrockFailure::contract)?;
        if !self.config.options.tools_enabled {
            declared_hidden.extend(selected.iter().cloned());
        }
        let hidden_tools = model_tool_exclusions(
            input
                .metadata()
                .map_err(|error| BedrockFailure::contract(error.to_string()))?,
            &selected,
            &declared_hidden,
        );
        let (tools, visible_tool_names) = self.translate_tools(&input, &hidden_tools)?;
        let assembled = match assemble_turn(&input, &current, &self.config, &hidden_tools)? {
            TurnAssembly::Ready(assembled) => assembled,
            TurnAssembly::AwaitingToolResults(groups) => {
                return materialize_pending_tool_results(output_root, groups);
            }
        };
        let mut body = self.config.options.request_fields.clone();
        body.insert("messages".to_string(), Value::Array(assembled.messages));
        body.insert(
            "inferenceConfig".to_string(),
            Value::Object(self.config.options.inference_config.clone()),
        );
        if !assembled.system.is_empty() {
            body.insert("system".to_string(), Value::Array(assembled.system));
        }
        if !tools.is_empty() {
            let mut tool_config = Map::new();
            tool_config.insert("tools".to_string(), Value::Array(tools));
            if let Some(choice) = &self.config.options.tool_choice {
                tool_config.insert("toolChoice".to_string(), choice.clone());
            }
            body.insert("toolConfig".to_string(), Value::Object(tool_config));
        } else if self.config.options.tool_choice.is_some() {
            return Err(BedrockFailure::contract(
                "Bedrock tool_choice is declared but the formed grant exposes no tools",
            ));
        }
        let bytes = serde_json::to_vec(&Value::Object(body)).map_err(|error| {
            BedrockFailure::contract(format!("Bedrock request JSON failed to encode: {error}"))
        })?;
        if bytes.len() > self.config.options.account_limits.max_request_bytes {
            return Err(BedrockFailure::contract(format!(
                "Bedrock request is {} bytes, above descriptor limit {}",
                bytes.len(),
                self.config.options.account_limits.max_request_bytes
            )));
        }
        let response = self.call(&bytes, &dispatch).await?;
        translate_response(
            response,
            output_root,
            &self.config.response_content_types,
            &visible_tool_names,
        )
    }

    fn translate_tools(
        &self,
        input: &InterfaceInvocationEnvelope,
        hidden_tools: &[String],
    ) -> Result<(Vec<Value>, BTreeSet<String>), BedrockFailure> {
        if !self.config.options.tools_enabled {
            return Ok((Vec::new(), BTreeSet::new()));
        }
        let specs = input
            .interface_affordance_specs()
            .map_err(|error| BedrockFailure::contract(error.to_string()))?;
        let visible =
            model_visible_affordances(specs, hidden_tools).map_err(BedrockFailure::contract)?;
        translate_tool_specs(
            &visible,
            self.config.options.account_limits.max_tools,
            self.config.options.account_limits.max_tool_input_bytes,
        )
    }
}

fn required_formed_models(
    input: &InterfaceInvocationEnvelope,
) -> Result<BTreeSet<String>, BedrockFailure> {
    let policy = input
        .interface_policy()
        .map_err(|error| BedrockFailure::contract(error.to_string()))?
        .ok_or_else(|| {
            BedrockFailure::contract("Bedrock invocation carries no formed interface policy")
        })?;
    let values = policy
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            BedrockFailure::contract(
                "Bedrock formed interface policy models must be a string array",
            )
        })?;
    if values.is_empty() {
        return Err(BedrockFailure::contract(
            "Bedrock formed interface policy models is empty",
        ));
    }
    let mut models = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let model = value.as_str().filter(|model| {
            !model.is_empty() && model.trim() == *model && !model.contains(['\r', '\n', '\0'])
        });
        let Some(model) = model else {
            return Err(BedrockFailure::contract(format!(
                "Bedrock formed interface policy models[{index}] is not a trimmed string"
            )));
        };
        if !models.insert(model.to_string()) {
            return Err(BedrockFailure::contract(format!(
                "Bedrock formed interface policy models repeats '{model}'"
            )));
        }
    }
    Ok(models)
}

fn catalog_model_allowed(id: &str, formed_models: &BTreeSet<String>) -> bool {
    formed_models.contains(id)
}

fn translate_tool_specs(
    visible: &[Value],
    max_tools: usize,
    max_tool_input_bytes: usize,
) -> Result<(Vec<Value>, BTreeSet<String>), BedrockFailure> {
    if visible.len() > max_tools {
        return Err(BedrockFailure::contract(format!(
            "formed grant exposes {} tools, above Bedrock descriptor limit {}",
            visible.len(),
            max_tools
        )));
    }
    let mut tools = Vec::with_capacity(visible.len());
    let mut visible_tool_names = BTreeSet::new();
    for (index, spec) in visible.iter().enumerate() {
        let name = spec.get("name").and_then(Value::as_str).ok_or_else(|| {
            BedrockFailure::contract(format!("interface_affordance_specs[{index}] has no name"))
        })?;
        let encoded = encode_tool_name(name)?;
        if !visible_tool_names.insert(name.to_string()) {
            return Err(BedrockFailure::contract(format!(
                "formed grant repeats tool '{name}'"
            )));
        }
        let schema = spec.get("input_schema").cloned().ok_or_else(|| {
            BedrockFailure::contract(format!(
                "interface_affordance_specs[{index}] '{name}' has no input_schema"
            ))
        })?;
        if !schema.is_object() {
            return Err(BedrockFailure::contract(format!(
                "tool '{name}' input_schema is not an object"
            )));
        }
        let schema_bytes = serde_json::to_vec(&schema).map_err(|error| {
            BedrockFailure::contract(format!("tool '{name}' schema did not encode: {error}"))
        })?;
        if schema_bytes.len() > max_tool_input_bytes {
            return Err(BedrockFailure::contract(format!(
                "tool '{name}' schema is {} bytes, above descriptor limit {}",
                schema_bytes.len(),
                max_tool_input_bytes
            )));
        }
        tools.push(json!({
            "toolSpec": {
                "name": encoded,
                "description": spec.get("description").and_then(Value::as_str).unwrap_or(""),
                "inputSchema": {"json": schema}
            }
        }));
    }
    Ok((tools, visible_tool_names))
}

impl BedrockConverseDriver {
    async fn call(
        &self,
        body: &[u8],
        dispatch: &DispatchAdmission<'_>,
    ) -> Result<ProviderResponse, BedrockFailure> {
        self.limiter.acquire().await;
        let headers = self
            .authorizer
            .authorize(&Method::POST, &self.endpoint, body, self.headers.clone())
            .await
            .map_err(|failure| {
                let retryable = failure.retryable && self.config.options.retry.auth_transport;
                BedrockFailure::auth(failure, retryable)
            })?;
        let request = self
            .http
            .request(Method::POST, self.endpoint.clone())
            .headers(headers)
            .body(body.to_vec());
        let credential_handle_fingerprint = self.authorizer.credential_handle_fingerprint().await;
        dispatch
            .admit(
                "POST",
                "http",
                &self.endpoint,
                body,
                credential_handle_fingerprint.as_deref(),
            )
            .await
            .map_err(BedrockFailure::contract)?;
        let response = request.send().await.map_err(|error| {
            BedrockFailure::transport(
                format!("Bedrock transport to {} failed: {error}", self.endpoint),
                self.config.options.retry.transport,
            )
        })?;
        let status = response.status();
        let response_headers = response.headers().clone();
        let content_type = response_headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !status.is_success() {
            let bytes = read_bounded(
                response,
                self.max_response_bytes,
                self.config.options.retry.transport,
                false,
            )
            .await?;
            let code = endpoint_error_code(&response_headers, &bytes);
            if self.authorizer.uses_cached_credentials()
                && cached_credentials_rejected(status, code.as_deref())
            {
                self.authorizer.invalidate_cached_credentials().await;
                return Err(BedrockFailure::endpoint(
                    status.as_u16(),
                    code,
                    &bytes,
                    true,
                ));
            }
            let retryable =
                endpoint_failure_retryable(&self.config.options.retry, status, code.as_deref());
            return Err(BedrockFailure::endpoint(
                status.as_u16(),
                code,
                &bytes,
                retryable,
            ));
        }
        let request_blake3 = blake3::hash(body).to_hex().to_string();
        if self.config.options.operation.streamed() {
            if !content_type.contains("application/vnd.amazon.eventstream") {
                return Err(BedrockFailure::response_evidence(
                    format!(
                        "Bedrock ConverseStream returned content-type '{content_type}', expected application/vnd.amazon.eventstream"
                    ),
                    0,
                    blake3::hash(&[]).to_hex().to_string(),
                ));
            }
            read_bedrock_stream(
                response,
                self.max_response_bytes,
                &self.config,
                TransportEvidence {
                    operation: self.config.options.operation,
                    status: status.as_u16(),
                    request_bytes: body.len(),
                    request_blake3,
                    response_bytes: 0,
                    response_blake3: String::new(),
                    event_count: 0,
                    event_types: BTreeMap::new(),
                },
            )
            .await
        } else {
            if !content_type.contains("application/json") {
                return Err(BedrockFailure::response_evidence(
                    format!(
                        "Bedrock Converse returned content-type '{content_type}', expected application/json"
                    ),
                    0,
                    blake3::hash(&[]).to_hex().to_string(),
                ));
            }
            let bytes = read_bounded(
                response,
                self.max_response_bytes,
                self.config.options.retry.transport,
                false,
            )
            .await?;
            let response_blake3 = blake3::hash(&bytes).to_hex().to_string();
            parse_converse_response(
                &bytes,
                &self.config,
                TransportEvidence {
                    operation: self.config.options.operation,
                    status: status.as_u16(),
                    request_bytes: body.len(),
                    request_blake3,
                    response_bytes: bytes.len(),
                    response_blake3,
                    event_count: 0,
                    event_types: BTreeMap::new(),
                },
            )
        }
    }

    async fn materialize_model_catalog(
        &self,
        request: &Value,
        output_root: &Path,
        dispatch: &DispatchAdmission<'_>,
        formed_models: &BTreeSet<String>,
    ) -> Result<Vec<ProviderEmission>, BedrockFailure> {
        let catalog = self.catalog.as_ref().ok_or_else(|| {
            BedrockFailure::contract("this Bedrock descriptor declares no model_catalog endpoint")
        })?;
        let object = request.as_object().ok_or_else(|| {
            BedrockFailure::contract("provider.models.list payload must be an object")
        })?;
        if object.len() != 1
            || object.get("label").and_then(Value::as_str)
                != Some(catalog.config.request_label.as_str())
        {
            return Err(BedrockFailure::contract(
                "provider.models.list payload must contain only its matching label",
            ));
        }
        let mut operations = Vec::new();
        let mut models = Vec::new();
        let mut model_keys = BTreeSet::new();
        for operation in &catalog.config.operations {
            let mut next_token: Option<String> = None;
            let mut pages = Vec::new();
            for page in 0..operation.max_pages {
                let mut url = resolve_control_path(&catalog.base_url, &operation.path)?;
                if matches!(operation.kind, CatalogOperationKind::InferenceProfiles) {
                    let mut query = url.query_pairs_mut();
                    if let Some(page_size) = operation.page_size {
                        query.append_pair("maxResults", &page_size.to_string());
                    }
                    if let Some(kind) = &operation.type_equals {
                        query.append_pair("type", kind);
                    }
                    if let Some(token) = &next_token {
                        query.append_pair("nextToken", token);
                    }
                }
                let method = Method::from_bytes(operation.method.as_bytes()).map_err(|error| {
                    BedrockFailure::contract(format!(
                        "model catalog method '{}' is invalid: {error}",
                        operation.method
                    ))
                })?;
                self.limiter.acquire().await;
                let headers = catalog
                    .authorizer
                    .authorize(&method, &url, b"", HeaderMap::new())
                    .await
                    .map_err(|failure| {
                        let retryable =
                            failure.retryable && self.config.options.retry.auth_transport;
                        BedrockFailure::auth(failure, retryable)
                    })?;
                let transport_request = self
                    .http
                    .request(method.clone(), url.clone())
                    .headers(headers);
                let credential_handle_fingerprint =
                    catalog.authorizer.credential_handle_fingerprint().await;
                dispatch
                    .admit(
                        method.as_str(),
                        "http",
                        &url,
                        &[],
                        credential_handle_fingerprint.as_deref(),
                    )
                    .await
                    .map_err(BedrockFailure::contract)?;
                let response = transport_request.send().await.map_err(|error| {
                    BedrockFailure::transport(
                        format!("Bedrock catalog request to {url} failed: {error}"),
                        self.config.options.retry.transport,
                    )
                })?;
                let status = response.status();
                let response_headers = response.headers().clone();
                let bytes = read_bounded(
                    response,
                    self.max_response_bytes,
                    self.config.options.retry.transport,
                    false,
                )
                .await?;
                let code = endpoint_error_code(&response_headers, &bytes);
                if catalog.authorizer.uses_cached_credentials()
                    && cached_credentials_rejected(status, code.as_deref())
                {
                    catalog.authorizer.invalidate_cached_credentials().await;
                    return Err(BedrockFailure::endpoint(
                        status.as_u16(),
                        code,
                        &bytes,
                        true,
                    ));
                }
                if !status.is_success() {
                    let retryable = endpoint_failure_retryable(
                        &self.config.options.retry,
                        status,
                        code.as_deref(),
                    );
                    return Err(BedrockFailure::endpoint(
                        status.as_u16(),
                        code,
                        &bytes,
                        retryable,
                    ));
                }
                let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
                    BedrockFailure::response(
                        format!("Bedrock catalog response is not JSON: {error}"),
                        &bytes,
                    )
                })?;
                let extracted = extract_catalog_models(operation.kind, &value)?;
                for model in extracted {
                    let id = required_string(&model, "id", "normalized catalog model")?;
                    let catalog_kind =
                        required_string(&model, "catalog_kind", "normalized catalog model")?;
                    if !model_keys.insert((catalog_kind.clone(), id.clone())) {
                        return Err(BedrockFailure::contract(format!(
                            "Bedrock catalog repeats {catalog_kind} id '{id}' across pages"
                        )));
                    }
                    if catalog_model_allowed(&id, formed_models) {
                        models.push(model);
                    }
                }
                next_token = value
                    .get("nextToken")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .filter(|token| !token.is_empty());
                pages.push(json!({
                    "page": page + 1,
                    "status": status.as_u16(),
                    "response_bytes": bytes.len(),
                    "response_blake3": blake3::hash(&bytes).to_hex().to_string(),
                    "body": value
                }));
                if next_token.is_none() {
                    break;
                }
            }
            if next_token.is_some() {
                return Err(BedrockFailure::contract(format!(
                    "Bedrock catalog operation '{:?}' exceeded max_pages={} while nextToken remained",
                    operation.kind, operation.max_pages
                )));
            }
            operations.push(json!({
                "kind": operation.kind.wire_name(),
                "method": operation.method,
                "path": operation.path,
                "pages": pages
            }));
        }
        let payload = json!({
            "label": catalog.config.output_label,
            "region": self.config.options.region,
            "models": models,
            "raw_operations": operations
        });
        Ok(vec![materialize_emission(
            output_root,
            &catalog.config.output_label,
            payload,
            ProviderEmissionRoute::ClientProjection,
        )?])
    }
}

impl RequestLimiter {
    async fn acquire(&self) {
        loop {
            let wait = {
                let now = Instant::now();
                let mut state = self.state.lock().await;
                while state
                    .front()
                    .is_some_and(|instant| now.duration_since(*instant) >= self.window)
                {
                    state.pop_front();
                }
                if state.len() < self.limit {
                    state.push_back(now);
                    None
                } else {
                    state
                        .front()
                        .map(|first| self.window.saturating_sub(now.duration_since(*first)))
                }
            };
            match wait {
                None => return,
                Some(duration) => tokio::time::sleep(duration).await,
            }
        }
    }
}

fn validate_options(options: &BedrockOptions) -> Result<(), String> {
    for (name, value) in [
        ("region", options.region.as_str()),
        ("signing_name", options.signing_name.as_str()),
    ] {
        if value.is_empty() || value.trim() != value {
            return Err(format!("Bedrock {name} must be non-empty and trimmed"));
        }
    }
    for (name, template) in [
        ("paths.converse", options.paths.converse.as_str()),
        (
            "paths.converse_stream",
            options.paths.converse_stream.as_str(),
        ),
    ] {
        if !template.starts_with('/') || template.matches("{model}").count() != 1 {
            return Err(format!(
                "Bedrock {name} must be an absolute path containing exactly one '{{model}}'"
            ));
        }
        if template.contains('?') || template.contains('#') {
            return Err(format!("Bedrock {name} must not contain query or fragment"));
        }
    }
    if options
        .inference_config
        .get("maxTokens")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .is_none()
    {
        return Err("Bedrock inference_config.maxTokens must be a positive integer".to_string());
    }
    for reserved in ["messages", "system", "inferenceConfig", "toolConfig"] {
        if options.request_fields.contains_key(reserved) {
            return Err(format!(
                "Bedrock request_fields must not override driver-owned '{reserved}'"
            ));
        }
    }
    validate_carrier_policy(&options.carrier_policy)?;
    let limits = &options.account_limits;
    if limits.max_in_flight == 0
        || limits.requests_per_window == 0
        || limits.window_ms == 0
        || limits.max_request_bytes == 0
        || limits.max_messages == 0
        || limits.max_content_blocks == 0
        || limits.max_tools == 0
        || limits.max_tool_input_bytes == 0
    {
        return Err("Bedrock account_limits values must all be positive".to_string());
    }
    let retry = &options.retry;
    if retry.base_delay_ms == 0 || retry.max_delay_ms < retry.base_delay_ms {
        return Err("Bedrock retry delays require 0 < base_delay_ms <= max_delay_ms".to_string());
    }
    validate_unique_u16(&retry.retryable_statuses, "retryable_statuses")?;
    if retry
        .retryable_statuses
        .iter()
        .any(|status| matches!(status, 401 | 403))
    {
        return Err(
            "Bedrock retryable_statuses must not include authentication statuses 401 or 403"
                .to_string(),
        );
    }
    validate_unique_strings(&retry.retryable_error_codes, "retryable_error_codes")?;
    if let Some(catalog) = &options.model_catalog {
        validate_catalog(catalog)?;
    }
    Ok(())
}

fn validate_carrier_policy(policy: &CarrierPolicy) -> Result<(), String> {
    if policy.max_inline_bytes == 0 {
        return Err("Bedrock carrier_policy.max_inline_bytes must be positive".to_string());
    }
    if policy.max_images_per_message == 0
        || policy.max_documents_per_message == 0
        || policy.max_image_bytes == 0
        || policy.max_document_bytes == 0
        || policy.max_image_bytes > policy.max_inline_bytes
        || policy.max_document_bytes > policy.max_inline_bytes
    {
        return Err(
            "Bedrock carrier media limits must be positive and within max_inline_bytes".to_string(),
        );
    }
    validate_unique_strings(&policy.allowed_image_formats, "allowed_image_formats")?;
    validate_unique_strings(&policy.allowed_document_formats, "allowed_document_formats")?;
    if policy.allowed_image_formats.is_empty() || policy.allowed_document_formats.is_empty() {
        return Err("Bedrock image and document format lists must be non-empty".to_string());
    }
    if policy.rules.is_empty() {
        return Err("Bedrock carrier_policy.rules must not be empty".to_string());
    }
    let mut labels = BTreeSet::new();
    for (index, rule) in policy.rules.iter().enumerate() {
        let label = rule.label();
        if label.is_empty() || label.trim() != label || !labels.insert(label.to_string()) {
            return Err(format!(
                "Bedrock carrier rule[{index}] has an invalid or duplicate label"
            ));
        }
        if let CarrierRule::JsonText {
            role, text_pointer, ..
        } = rule
        {
            if !matches!(role.as_str(), "user" | "assistant") {
                return Err(format!(
                    "Bedrock carrier rule[{index}] role must be user or assistant"
                ));
            }
            if !text_pointer.starts_with('/') {
                return Err(format!(
                    "Bedrock carrier rule[{index}] text_pointer must be a JSON Pointer"
                ));
            }
        }
    }
    Ok(())
}

fn validate_catalog(config: &ModelCatalogConfig) -> Result<(), String> {
    for (name, value) in [
        ("request_label", config.request_label.as_str()),
        ("output_label", config.output_label.as_str()),
        ("signing_name", config.signing_name.as_str()),
    ] {
        if value.is_empty() || value.trim() != value {
            return Err(format!(
                "Bedrock model_catalog.{name} must be non-empty and trimmed"
            ));
        }
    }
    validate_https_base_url(&config.base_url, "Bedrock model_catalog.base_url")?;
    if config.operations.is_empty() {
        return Err("Bedrock model_catalog.operations must not be empty".to_string());
    }
    let mut kinds = BTreeSet::new();
    for operation in &config.operations {
        let kind = format!("{:?}", operation.kind);
        if !kinds.insert(kind) {
            return Err("Bedrock model_catalog repeats an operation kind".to_string());
        }
        if operation.method != "GET"
            || !operation.path.starts_with('/')
            || operation.path.contains('?')
            || operation.path.contains('#')
            || operation.max_pages == 0
        {
            return Err(format!(
                "Bedrock model_catalog operation '{:?}' must declare GET, an absolute plain path, and positive max_pages",
                operation.kind
            ));
        }
        match operation.kind {
            CatalogOperationKind::FoundationModels => {
                if operation.max_pages != 1
                    || operation.page_size.is_some()
                    || operation.type_equals.is_some()
                {
                    return Err(
                        "foundation_models must declare max_pages=1 and no pagination fields"
                            .to_string(),
                    );
                }
            }
            CatalogOperationKind::InferenceProfiles => {
                if operation
                    .page_size
                    .is_some_and(|value| !(1..=1000).contains(&value))
                {
                    return Err("inference_profiles page_size must be within 1..=1000".to_string());
                }
                if operation
                    .type_equals
                    .as_deref()
                    .is_some_and(|value| !matches!(value, "SYSTEM_DEFINED" | "APPLICATION"))
                {
                    return Err(
                        "inference_profiles type_equals must be SYSTEM_DEFINED or APPLICATION"
                            .to_string(),
                    );
                }
            }
        }
    }
    Ok(())
}

fn validate_unique_u16(values: &[u16], surface: &str) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for value in values {
        if !(100..=599).contains(value) {
            return Err(format!(
                "Bedrock {surface} contains invalid HTTP status {value}"
            ));
        }
        if !seen.insert(*value) {
            return Err(format!("Bedrock {surface} repeats {value}"));
        }
    }
    Ok(())
}

fn validate_unique_strings(values: &[String], surface: &str) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for value in values {
        if value.is_empty() || value.trim() != value || !seen.insert(value.as_str()) {
            return Err(format!(
                "Bedrock {surface} contains an empty, untrimmed, or duplicate value"
            ));
        }
    }
    Ok(())
}

fn validate_content_types(
    values: &[String],
    supported: &[&str],
    surface: &str,
) -> Result<BTreeSet<String>, String> {
    let mut result = BTreeSet::new();
    for value in values {
        if !supported.contains(&value.as_str()) {
            return Err(format!("Bedrock {surface} contains unsupported '{value}'"));
        }
        if !result.insert(value.clone()) {
            return Err(format!("Bedrock {surface} repeats '{value}'"));
        }
    }
    if result.is_empty() {
        return Err(format!("Bedrock {surface} must not be empty"));
    }
    Ok(result)
}

fn static_headers(transport: &InterfaceTransportConfig) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    for (name, value) in &transport.headers {
        let normalized = name.to_ascii_lowercase();
        if matches!(
            normalized.as_str(),
            "authorization"
                | "proxy-authorization"
                | "host"
                | "content-length"
                | "x-amz-content-sha256"
                | "x-amz-date"
                | "x-amz-security-token"
                | "cookie"
                | "set-cookie"
                | "api-key"
                | "x-api-key"
        ) || normalized.ends_with("-api-key")
            || normalized.contains("credential")
        {
            return Err(format!(
                "Bedrock transport header '{name}' is protocol-owned or secret-bearing"
            ));
        }
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|error| format!("Bedrock header name is invalid: {error}"))?;
        let value = HeaderValue::from_str(value)
            .map_err(|error| format!("Bedrock header '{name}' is invalid: {error}"))?;
        headers.insert(name, value);
    }
    if headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        != Some("application/json")
    {
        return Err("Bedrock transport must declare content-type=application/json".to_string());
    }
    Ok(headers)
}

fn validate_https_base_url(value: &str, surface: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|error| format!("{surface} is invalid: {error}"))?;
    if url.scheme() != "https"
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(format!(
            "{surface} must be an HTTPS URL without credentials, query, or fragment"
        ));
    }
    Ok(url)
}

fn build_model_endpoint(base: &Url, template: &str, model: &str) -> Result<Url, String> {
    let encoded = aws_percent_encode(model.as_bytes());
    let path = template.replace("{model}", &encoded);
    resolve_control_path(base, &path).map_err(|failure| failure.detail)
}

fn resolve_control_path(base: &Url, path: &str) -> Result<Url, BedrockFailure> {
    let mut url = base.clone();
    let base_path = base.path().trim_end_matches('/');
    url.set_path(&format!("{base_path}{path}"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn metadata_string(metadata: &Map<String, Value>, key: &str) -> Result<String, String> {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.trim() == *value)
        .map(str::to_string)
        .ok_or_else(|| format!("Bedrock private_config.{key} is missing or malformed"))
}

fn metadata_string_array(metadata: &Map<String, Value>, key: &str) -> Result<Vec<String>, String> {
    metadata
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("Bedrock private_config.{key} must be an array"))?
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .filter(|value| !value.is_empty() && value.trim() == *value)
                .map(str::to_string)
                .ok_or_else(|| format!("Bedrock {key}[{index}] is malformed"))
        })
        .collect()
}

fn assemble_turn(
    input: &InterfaceInvocationEnvelope,
    current: &Value,
    config: &DriverConfig,
    hidden_tools: &[String],
) -> Result<TurnAssembly, BedrockFailure> {
    match config.options.reasoning_replay {
        ReasoningReplay::Verbatim => {}
    }
    let mut artifacts = Vec::new();
    let mut session_sources = BTreeSet::new();
    for bound in &input.artifact_binding_payloads {
        let bytes = bound.read_bytes().map_err(|error| {
            BedrockFailure::contract(format!(
                "bound artifact '{}' cannot be read: {error}",
                bound.artifact.id
            ))
        })?;
        let mut value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            BedrockFailure::contract(format!(
                "bound artifact '{}' is not JSON: {error}",
                bound.artifact.id
            ))
        })?;
        if let Some(snapshot) = value.get(SESSION_CONTEXT_FIELD) {
            let snapshot: SessionContext =
                serde_json::from_value(snapshot.clone()).map_err(|error| {
                    BedrockFailure::contract(format!(
                        "bound session context '{}' is malformed: {error}",
                        bound.artifact.id
                    ))
                })?;
            if snapshot.schema_version != 2 {
                return Err(BedrockFailure::contract(format!(
                    "bound session context '{}' has unsupported schema_version {}",
                    bound.artifact.id, snapshot.schema_version
                )));
            }
            if snapshot.provider_interface.as_str() != config.interface_id.as_str() {
                return Err(BedrockFailure::contract(format!(
                    "bound session context '{}' belongs to provider '{}' instead of '{}'",
                    bound.artifact.id,
                    snapshot.provider_interface.as_str(),
                    config.interface_id
                )));
            }
            for (index, item) in snapshot.artifacts.into_iter().enumerate() {
                if item.label.is_empty() || item.label.trim() != item.label {
                    return Err(BedrockFailure::contract(format!(
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
                    return Err(BedrockFailure::contract(format!(
                        "bound session context '{}' repeats an artifact source across the invocation at index {index}",
                        bound.artifact.id
                    )));
                }
                if item.payload.get(SESSION_CONTEXT_FIELD).is_some() {
                    return Err(BedrockFailure::contract(format!(
                        "bound session context '{}' artifact[{index}] nests the reserved carrier",
                        bound.artifact.id
                    )));
                }
                let _ = item.source_movement_seq;
                artifacts.push(ContextArtifact {
                    id: format!("{}#session:{index}", bound.artifact.id),
                    label: item.label,
                    value: item.payload,
                    current: false,
                });
            }
            let label = payload_label(bound.artifact.id.as_str(), &value)
                .filter(|label| !label.is_empty() && label.trim() == label)
                .ok_or_else(|| {
                    BedrockFailure::contract(format!(
                        "bound session context '{}' outer artifact has no valid label",
                        bound.artifact.id
                    ))
                })?;
            value
                .as_object_mut()
                .ok_or_else(|| {
                    BedrockFailure::contract(format!(
                        "bound session context '{}' outer artifact is not an object",
                        bound.artifact.id
                    ))
                })?
                .remove(SESSION_CONTEXT_FIELD)
                .ok_or_else(|| {
                    BedrockFailure::contract(format!(
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
            if !session_sources.insert(source_key) {
                return Err(BedrockFailure::contract(format!(
                    "bound session context '{}' repeats its outer artifact source",
                    bound.artifact.id
                )));
            }
            artifacts.push(ContextArtifact {
                id: bound.artifact.id.to_string(),
                label,
                value,
                current: false,
            });
        } else if let Some(label) = payload_label(bound.artifact.id.as_str(), &value) {
            let source_key = format!(
                "{}:{}:{}",
                bound.artifact.id,
                bound.artifact.content.to_hex(),
                label
            );
            if !session_sources.insert(source_key) {
                return Err(BedrockFailure::contract(format!(
                    "bound artifact '{}' repeats an artifact source across the invocation",
                    bound.artifact.id
                )));
            }
            artifacts.push(ContextArtifact {
                id: bound.artifact.id.to_string(),
                label,
                value,
                current: false,
            });
        }
    }
    let manifest = input
        .read_artifact_manifest()
        .map_err(|error| BedrockFailure::contract(error.to_string()))?;
    artifacts.push(ContextArtifact {
        id: input.artifact.id.to_string(),
        label: manifest.label,
        value: current.clone(),
        current: true,
    });

    let mut groups = BTreeMap::new();
    let mut calls = BTreeMap::new();
    let mut results = BTreeMap::new();
    for (index, artifact) in artifacts.iter().enumerate() {
        if artifact.label == INTERFACE_OUTPUT_LABEL {
            let blocks = artifact
                .value
                .get("content_blocks")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    BedrockFailure::contract(
                        "interface.artifact.output has no content_blocks array",
                    )
                })?;
            let mut call_ids = Vec::new();
            let mut call_specs = BTreeMap::new();
            for block in blocks {
                let Some(tool) = block.get("toolUse") else {
                    continue;
                };
                if tool.get("type").and_then(Value::as_str) == Some("server_tool_use") {
                    continue;
                }
                let correlation_id = required_string(tool, "toolUseId", "assistant toolUse")?;
                let encoded_name = required_string(tool, "name", "assistant toolUse")?;
                let arguments = tool
                    .get("input")
                    .filter(|value| value.is_object())
                    .cloned()
                    .ok_or_else(|| {
                        BedrockFailure::contract("assistant toolUse input is not an object")
                    })?;
                if call_specs
                    .insert(
                        correlation_id.clone(),
                        (decode_tool_name(&encoded_name)?, arguments),
                    )
                    .is_some()
                {
                    return Err(BedrockFailure::contract(format!(
                        "assistant output repeats toolUseId '{correlation_id}'"
                    )));
                }
                call_ids.push(correlation_id);
            }
            if !call_ids.is_empty() {
                let provider_turn_id =
                    required_string(&artifact.value, "provider_turn_id", INTERFACE_OUTPUT_LABEL)?;
                if groups
                    .insert(
                        provider_turn_id.clone(),
                        BedrockToolGroup {
                            output_index: index,
                            call_ids,
                            call_specs,
                            call_indexes: BTreeMap::new(),
                            result_indexes: BTreeMap::new(),
                        },
                    )
                    .is_some()
                {
                    return Err(BedrockFailure::contract(format!(
                        "Bedrock context repeats provider_turn_id '{provider_turn_id}'"
                    )));
                }
            }
        } else if artifact.label == TOOL_CALL_LABEL {
            let correlation_id =
                required_string(&artifact.value, "correlation_id", TOOL_CALL_LABEL)?;
            let provider_turn_id =
                required_string(&artifact.value, "provider_turn_id", TOOL_CALL_LABEL)?;
            if calls
                .insert(correlation_id.clone(), (provider_turn_id, index))
                .is_some()
            {
                return Err(BedrockFailure::contract(format!(
                    "Bedrock context repeats tool.call correlation_id '{correlation_id}'"
                )));
            }
        } else if is_tool_result(&artifact.label, &artifact.value) {
            let correlation_id = required_string(&artifact.value, "correlation_id", "tool result")?;
            if results.insert(correlation_id.clone(), index).is_some() {
                return Err(BedrockFailure::contract(format!(
                    "Bedrock context repeats tool result correlation_id '{correlation_id}'"
                )));
            }
        }
    }
    for (correlation_id, (provider_turn_id, call_index)) in &calls {
        let group = groups.get_mut(provider_turn_id).ok_or_else(|| {
            BedrockFailure::contract(format!(
                "tool.call correlation_id '{correlation_id}' has no assistant provider turn '{provider_turn_id}'"
            ))
        })?;
        if !group.call_ids.contains(correlation_id) {
            return Err(BedrockFailure::contract(format!(
                "tool.call correlation_id '{correlation_id}' is absent from assistant provider turn '{provider_turn_id}'"
            )));
        }
        let (wire_tool, wire_arguments) =
            group.call_specs.get(correlation_id).ok_or_else(|| {
                BedrockFailure::contract(format!(
                    "assistant provider turn '{provider_turn_id}' lost toolUse '{correlation_id}'"
                ))
            })?;
        let call = &artifacts[*call_index].value;
        let artifact_tool = required_string(call, "tool", TOOL_CALL_LABEL)?;
        let artifact_arguments = call
            .get("arguments")
            .filter(|value| value.is_object())
            .ok_or_else(|| BedrockFailure::contract("tool.call arguments are not an object"))?;
        if &artifact_tool != wire_tool || artifact_arguments != wire_arguments {
            return Err(BedrockFailure::contract(format!(
                "tool.call correlation_id '{correlation_id}' does not match its assistant toolUse"
            )));
        }
        if group
            .call_indexes
            .insert(correlation_id.clone(), *call_index)
            .is_some()
        {
            return Err(BedrockFailure::contract(format!(
                "assistant provider turn '{provider_turn_id}' repeats tool.call '{correlation_id}'"
            )));
        }
    }
    for (provider_turn_id, group) in &groups {
        let mut unique = BTreeSet::new();
        for correlation_id in &group.call_ids {
            if !unique.insert(correlation_id) {
                return Err(BedrockFailure::contract(format!(
                    "assistant provider turn '{provider_turn_id}' repeats toolUseId '{correlation_id}'"
                )));
            }
            if !group.call_indexes.contains_key(correlation_id) {
                return Err(BedrockFailure::contract(format!(
                    "assistant provider turn '{provider_turn_id}' has no emitted tool.call for '{correlation_id}'"
                )));
            }
        }
    }
    for (correlation_id, result_index) in &results {
        let (provider_turn_id, _) = calls.get(correlation_id).ok_or_else(|| {
            BedrockFailure::contract(format!(
                "tool result correlation_id '{correlation_id}' has no preceding tool.call"
            ))
        })?;
        let group = groups.get_mut(provider_turn_id).ok_or_else(|| {
            BedrockFailure::contract(format!(
                "tool result correlation_id '{correlation_id}' resolved to missing provider turn '{provider_turn_id}'"
            ))
        })?;
        let call_index = calls
            .get(correlation_id)
            .map(|(_, index)| *index)
            .ok_or_else(|| {
                BedrockFailure::contract(format!(
                    "tool result correlation_id '{correlation_id}' lost its tool.call"
                ))
            })?;
        let call_tool = required_string(&artifacts[call_index].value, "tool", TOOL_CALL_LABEL)?;
        let result_tool = required_string(&artifacts[*result_index].value, "tool", "tool result")?;
        if result_tool != call_tool {
            return Err(BedrockFailure::contract(format!(
                "tool result correlation_id '{correlation_id}' names tool '{result_tool}' instead of '{call_tool}'"
            )));
        }
        group
            .result_indexes
            .insert(correlation_id.clone(), *result_index);
    }
    let mut pending = Vec::new();
    for (provider_turn_id, group) in &groups {
        if group.result_indexes.len() == group.call_ids.len() {
            continue;
        }
        pending.push(PendingToolGroup {
            provider_turn_id: provider_turn_id.clone(),
            expected: group.call_ids.len(),
            received: group.result_indexes.len(),
            pending_correlation_ids: group
                .call_ids
                .iter()
                .filter(|correlation_id| !group.result_indexes.contains_key(*correlation_id))
                .cloned()
                .collect(),
        });
    }
    if !pending.is_empty() {
        let current = artifacts
            .last()
            .ok_or_else(|| BedrockFailure::contract("Bedrock context is empty"))?;
        if !is_tool_result(&current.label, &current.value) {
            return Err(BedrockFailure::contract(
                "Bedrock context contains an unresolved parallel tool group but the current artifact is not a tool result",
            ));
        }
        let current_correlation =
            required_string(&current.value, "correlation_id", "current tool result")?;
        let current_group = calls
            .get(&current_correlation)
            .map(|(provider_turn_id, _)| provider_turn_id);
        if pending.len() != 1
            || current_group != pending.first().map(|group| &group.provider_turn_id)
        {
            return Err(BedrockFailure::contract(
                "Bedrock context contains ambiguous unresolved parallel tool groups",
            ));
        }
        return Ok(TurnAssembly::AwaitingToolResults(pending));
    }

    let mut grouped_indexes = BTreeSet::new();
    let mut groups_by_output = BTreeMap::new();
    for (provider_turn_id, group) in &groups {
        if groups_by_output
            .insert(group.output_index, provider_turn_id.clone())
            .is_some()
        {
            return Err(BedrockFailure::contract(
                "Bedrock provider turns share one assistant output position",
            ));
        }
        grouped_indexes.insert(group.output_index);
        grouped_indexes.extend(group.call_indexes.values().copied());
        grouped_indexes.extend(group.result_indexes.values().copied());
    }

    let mut system = Vec::new();
    if let Some(block) = render_model_context_block_with_hidden_tools(
        input
            .metadata()
            .map_err(|error| BedrockFailure::contract(error.to_string()))?,
        hidden_tools,
    ) {
        system.push(json!({"text": block}));
    }
    let mut messages = Vec::new();
    for (index, artifact) in artifacts.iter().enumerate() {
        if let Some(provider_turn_id) = groups_by_output.get(&index) {
            let group = groups.get(provider_turn_id).ok_or_else(|| {
                BedrockFailure::contract(format!(
                    "ordered provider turn '{provider_turn_id}' has no context group"
                ))
            })?;
            let blocks = artifact
                .value
                .get("content_blocks")
                .and_then(Value::as_array)
                .cloned()
                .ok_or_else(|| {
                    BedrockFailure::contract(
                        "interface.artifact.output has no content_blocks array",
                    )
                })?;
            for block in &blocks {
                validate_content_block(
                    block,
                    &config.input_content_types,
                    &config.options.carrier_policy,
                    "assistant replay",
                )?;
            }
            push_message(&mut messages, "assistant", blocks)?;
            let result_blocks = group
                .call_ids
                .iter()
                .map(|correlation_id| {
                    let result_index =
                        group.result_indexes.get(correlation_id).ok_or_else(|| {
                            BedrockFailure::contract(format!(
                            "provider turn '{provider_turn_id}' lost tool result '{correlation_id}'"
                        ))
                        })?;
                    tool_result_block(
                        &artifacts[*result_index].value,
                        &config.options.carrier_policy,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            push_message(&mut messages, "user", result_blocks)?;
            continue;
        }
        if grouped_indexes.contains(&index) {
            continue;
        }
        match artifact.label.as_str() {
            CONTEXT_REFERENCE_LABEL => {
                let text = artifact
                    .value
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        BedrockFailure::contract(format!(
                            "context.reference artifact '{}' has no text",
                            artifact.id
                        ))
                    })?;
                system.push(json!({"text": text}));
            }
            TOOL_CALL_LABEL => {
                return Err(BedrockFailure::contract(
                    "Bedrock context contains an ungrouped tool.call artifact",
                ));
            }
            INTERFACE_OUTPUT_LABEL => {
                let blocks = artifact
                    .value
                    .get("content_blocks")
                    .and_then(Value::as_array)
                    .cloned()
                    .ok_or_else(|| {
                        BedrockFailure::contract(
                            "interface.artifact.output has no content_blocks array",
                        )
                    })?;
                for block in &blocks {
                    validate_content_block(
                        block,
                        &config.input_content_types,
                        &config.options.carrier_policy,
                        "assistant replay",
                    )?;
                }
                push_message(&mut messages, "assistant", blocks)?;
            }
            PROVIDER_REASONING_LABEL | PROVIDER_USAGE_LABEL | PROVIDER_TRANSPORT_LABEL => {}
            PROVIDER_ERROR_LABEL => {
                push_message(
                    &mut messages,
                    "assistant",
                    vec![json!({"text": render_json(&artifact.value)?})],
                )?;
            }
            TOOL_RESULTS_PENDING_LABEL => {}
            label if is_tool_result(label, &artifact.value) => {
                return Err(BedrockFailure::contract(
                    "Bedrock context contains an ungrouped tool result artifact",
                ));
            }
            label => {
                if let Some(rule) = config
                    .options
                    .carrier_policy
                    .rules
                    .iter()
                    .find(|rule| rule.label() == label)
                {
                    let (role, blocks) = apply_carrier_rule(
                        rule,
                        &artifact.value,
                        &config.input_content_types,
                        &config.options.carrier_policy,
                    )?;
                    push_message(&mut messages, &role, blocks)?;
                } else if artifact.current {
                    return Err(BedrockFailure::contract(format!(
                        "current artifact label '{label}' has no Bedrock carrier rule"
                    )));
                }
            }
        }
    }
    if messages.is_empty() {
        return Err(BedrockFailure::contract(
            "Bedrock turn assembled no messages",
        ));
    }
    if messages.len() > config.options.account_limits.max_messages {
        return Err(BedrockFailure::contract(format!(
            "Bedrock turn has {} messages, above descriptor limit {}",
            messages.len(),
            config.options.account_limits.max_messages
        )));
    }
    for (message_index, message) in messages.iter().enumerate() {
        for (block_index, block) in message
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            validate_tool_input_size(
                block,
                config.options.account_limits.max_tool_input_bytes,
                &format!("request message[{message_index}].content[{block_index}]"),
            )
            .map_err(BedrockFailure::contract)?;
        }
    }
    let blocks = messages
        .iter()
        .filter_map(|message| message.get("content").and_then(Value::as_array))
        .map(Vec::len)
        .sum::<usize>();
    if blocks > config.options.account_limits.max_content_blocks {
        return Err(BedrockFailure::contract(format!(
            "Bedrock turn has {blocks} content blocks, above descriptor limit {}",
            config.options.account_limits.max_content_blocks
        )));
    }
    if messages
        .first()
        .and_then(|value| value.get("role"))
        .and_then(Value::as_str)
        != Some("user")
        || messages
            .last()
            .and_then(|value| value.get("role"))
            .and_then(Value::as_str)
            != Some("user")
    {
        return Err(BedrockFailure::contract(
            "Bedrock Converse messages must begin and end with a user turn",
        ));
    }
    validate_request_message_media(&messages, &config.options.carrier_policy)?;
    validate_tool_order(&messages)?;
    Ok(TurnAssembly::Ready(AssembledTurn { system, messages }))
}

fn validate_request_message_media(
    messages: &[Value],
    policy: &CarrierPolicy,
) -> Result<(), BedrockFailure> {
    for (index, message) in messages.iter().enumerate() {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| BedrockFailure::contract("Bedrock request message has no role"))?;
        let content = message
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| BedrockFailure::contract("Bedrock request message has no content"))?;
        let mut images = 0_usize;
        let mut documents = 0_usize;
        let top_level_documents = content
            .iter()
            .filter(|block| block.get("document").is_some())
            .count();
        for block in content {
            count_media_blocks(block, &mut images, &mut documents);
        }
        if role != "user" && (images != 0 || documents != 0) {
            return Err(BedrockFailure::contract(format!(
                "Bedrock request message[{index}] carries image/document content outside a user turn"
            )));
        }
        if top_level_documents != 0 && !content.iter().any(|block| block.get("text").is_some()) {
            return Err(BedrockFailure::contract(format!(
                "Bedrock request message[{index}] carries a document without a text block"
            )));
        }
        if images > policy.max_images_per_message {
            return Err(BedrockFailure::contract(format!(
                "Bedrock request message[{index}] carries {images} images, above descriptor limit {}",
                policy.max_images_per_message
            )));
        }
        if documents > policy.max_documents_per_message {
            return Err(BedrockFailure::contract(format!(
                "Bedrock request message[{index}] carries {documents} documents, above descriptor limit {}",
                policy.max_documents_per_message
            )));
        }
    }
    Ok(())
}

fn count_media_blocks(value: &Value, images: &mut usize, documents: &mut usize) {
    if value.get("image").is_some() {
        *images = images.saturating_add(1);
    }
    if value.get("document").is_some() {
        *documents = documents.saturating_add(1);
    }
    if let Some(content) = value
        .pointer("/toolResult/content")
        .and_then(Value::as_array)
    {
        for block in content {
            count_media_blocks(block, images, documents);
        }
    }
}

fn apply_carrier_rule(
    rule: &CarrierRule,
    value: &Value,
    allowed: &BTreeSet<String>,
    policy: &CarrierPolicy,
) -> Result<(String, Vec<Value>), BedrockFailure> {
    match rule {
        CarrierRule::JsonText {
            role, text_pointer, ..
        } => {
            let text = value
                .pointer(text_pointer)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    BedrockFailure::contract(format!(
                        "Bedrock json_text carrier has no text at '{text_pointer}'"
                    ))
                })?;
            let block = json!({"text": text});
            validate_content_block(&block, allowed, policy, "json_text carrier")?;
            Ok((role.clone(), vec![block]))
        }
        CarrierRule::BedrockMessage { .. } => {
            let role = value
                .get("role")
                .and_then(Value::as_str)
                .filter(|role| matches!(*role, "user" | "assistant"))
                .ok_or_else(|| {
                    BedrockFailure::contract(
                        "bedrock_message carrier role must be user or assistant",
                    )
                })?;
            let content = value
                .get("content")
                .and_then(Value::as_array)
                .filter(|content| !content.is_empty())
                .cloned()
                .ok_or_else(|| {
                    BedrockFailure::contract(
                        "bedrock_message carrier content must be a non-empty array",
                    )
                })?;
            for block in &content {
                validate_content_block(block, allowed, policy, "bedrock_message carrier")?;
            }
            Ok((role.to_string(), content))
        }
    }
}

fn validate_content_block(
    block: &Value,
    allowed: &BTreeSet<String>,
    policy: &CarrierPolicy,
    surface: &str,
) -> Result<(), BedrockFailure> {
    let object = block.as_object().ok_or_else(|| {
        BedrockFailure::contract(format!("Bedrock {surface} content block is not an object"))
    })?;
    if object.len() != 1 {
        return Err(BedrockFailure::contract(format!(
            "Bedrock {surface} content block must contain exactly one union member"
        )));
    }
    let (kind, value) = object.iter().next().unwrap();
    if !allowed.contains(kind) {
        return Err(BedrockFailure::contract(format!(
            "Bedrock {surface} content type '{kind}' is not descriptor-admitted"
        )));
    }
    match kind.as_str() {
        "text" => {
            if value.as_str().filter(|text| !text.is_empty()).is_none() {
                return Err(BedrockFailure::contract(format!(
                    "Bedrock {surface} text block is empty or malformed"
                )));
            }
        }
        "image" => validate_image(value, policy, surface)?,
        "document" => validate_document(value, policy, surface)?,
        "reasoningContent" => validate_reasoning(value, policy, surface)?,
        "toolUse" => validate_tool_use(value, surface)?,
        "toolResult" => validate_tool_result(value, policy, surface)?,
        "citationsContent" => validate_citations_content(value, surface)?,
        other => {
            return Err(BedrockFailure::contract(format!(
                "Bedrock {surface} content type '{other}' has no implemented contract"
            )));
        }
    }
    Ok(())
}

fn validate_citations_content(value: &Value, surface: &str) -> Result<(), BedrockFailure> {
    let citations = value.as_object().ok_or_else(|| {
        BedrockFailure::contract(format!(
            "Bedrock {surface} citationsContent is not an object"
        ))
    })?;
    ensure_map_keys(citations, &["content", "citations"], "citationsContent")
        .map_err(BedrockFailure::contract)?;
    if let Some(content) = citations.get("content") {
        let content = content.as_array().ok_or_else(|| {
            BedrockFailure::contract(format!(
                "Bedrock {surface} citationsContent.content is not an array"
            ))
        })?;
        for (index, generated) in content.iter().enumerate() {
            let generated = generated.as_object().ok_or_else(|| {
                BedrockFailure::contract(format!(
                    "Bedrock {surface} citationsContent.content[{index}] is not an object"
                ))
            })?;
            if generated.len() != 1
                || generated
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                    .is_none()
            {
                return Err(BedrockFailure::contract(format!(
                    "Bedrock {surface} citationsContent.content[{index}] must contain one non-empty text member"
                )));
            }
        }
    }
    if let Some(items) = citations.get("citations") {
        let items = items.as_array().ok_or_else(|| {
            BedrockFailure::contract(format!(
                "Bedrock {surface} citationsContent.citations is not an array"
            ))
        })?;
        for (index, citation) in items.iter().enumerate() {
            validate_citation(citation, surface, index)?;
        }
    }
    if citations
        .get("content")
        .and_then(Value::as_array)
        .map_or(true, Vec::is_empty)
        && citations
            .get("citations")
            .and_then(Value::as_array)
            .map_or(true, Vec::is_empty)
    {
        return Err(BedrockFailure::contract(format!(
            "Bedrock {surface} citationsContent is empty"
        )));
    }
    Ok(())
}

fn validate_citation(value: &Value, surface: &str, index: usize) -> Result<(), BedrockFailure> {
    let citation = value.as_object().ok_or_else(|| {
        BedrockFailure::contract(format!(
            "Bedrock {surface} citation[{index}] is not an object"
        ))
    })?;
    ensure_map_keys(
        citation,
        &["title", "source", "location", "sourceContent"],
        "citation",
    )
    .map_err(BedrockFailure::contract)?;
    for key in ["title", "source"] {
        if citation
            .get(key)
            .is_some_and(|value| value.as_str().is_none())
        {
            return Err(BedrockFailure::contract(format!(
                "Bedrock {surface} citation[{index}].{key} is malformed"
            )));
        }
    }
    if let Some(location) = citation.get("location") {
        let location = location.as_object().ok_or_else(|| {
            BedrockFailure::contract(format!(
                "Bedrock {surface} citation[{index}].location is not an object"
            ))
        })?;
        if location.len() != 1 || !location.values().all(Value::is_object) {
            return Err(BedrockFailure::contract(format!(
                "Bedrock {surface} citation[{index}].location must contain one object union member"
            )));
        }
    }
    if let Some(content) = citation.get("sourceContent") {
        let content = content.as_array().ok_or_else(|| {
            BedrockFailure::contract(format!(
                "Bedrock {surface} citation[{index}].sourceContent is not an array"
            ))
        })?;
        for (content_index, item) in content.iter().enumerate() {
            let item = item.as_object().ok_or_else(|| {
                BedrockFailure::contract(format!(
                    "Bedrock {surface} citation[{index}].sourceContent[{content_index}] is not an object"
                ))
            })?;
            if item.len() != 1 || item.get("text").and_then(Value::as_str).is_none() {
                return Err(BedrockFailure::contract(format!(
                    "Bedrock {surface} citation[{index}].sourceContent[{content_index}] must contain one text member"
                )));
            }
        }
    }
    Ok(())
}

fn validate_image(
    value: &Value,
    policy: &CarrierPolicy,
    surface: &str,
) -> Result<(), BedrockFailure> {
    let image = value.as_object().ok_or_else(|| {
        BedrockFailure::contract(format!("Bedrock {surface} image block is not an object"))
    })?;
    ensure_map_keys(image, &["format", "source"], "image block")
        .map_err(BedrockFailure::contract)?;
    let format = image.get("format").and_then(Value::as_str).ok_or_else(|| {
        BedrockFailure::contract(format!("Bedrock {surface} image has no format"))
    })?;
    if !policy
        .allowed_image_formats
        .iter()
        .any(|allowed| allowed == format)
    {
        return Err(BedrockFailure::contract(format!(
            "Bedrock {surface} image format '{format}' is not descriptor-admitted"
        )));
    }
    validate_image_source(
        image.get("source"),
        policy.max_image_bytes,
        &format!("{surface} image"),
    )
}

fn validate_document(
    value: &Value,
    policy: &CarrierPolicy,
    surface: &str,
) -> Result<(), BedrockFailure> {
    let document = value.as_object().ok_or_else(|| {
        BedrockFailure::contract(format!("Bedrock {surface} document block is not an object"))
    })?;
    ensure_map_keys(
        document,
        &["name", "source", "format", "context", "citations"],
        "document block",
    )
    .map_err(BedrockFailure::contract)?;
    let name = document
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            BedrockFailure::contract(format!("Bedrock {surface} document has no name"))
        })?;
    validate_document_name(name, surface)?;
    if let Some(format) = document.get("format") {
        let format = format.as_str().ok_or_else(|| {
            BedrockFailure::contract(format!("Bedrock {surface} document format is malformed"))
        })?;
        if !policy
            .allowed_document_formats
            .iter()
            .any(|allowed| allowed == format)
        {
            return Err(BedrockFailure::contract(format!(
                "Bedrock {surface} document format '{format}' is not descriptor-admitted"
            )));
        }
    }
    if let Some(context) = document.get("context") {
        if context.as_str().is_none() {
            return Err(BedrockFailure::contract(format!(
                "Bedrock {surface} document context is malformed"
            )));
        }
    }
    if let Some(citations) = document.get("citations") {
        let citations = citations.as_object().ok_or_else(|| {
            BedrockFailure::contract(format!("Bedrock {surface} document citations is malformed"))
        })?;
        ensure_map_keys(citations, &["enabled"], "document citations")
            .map_err(BedrockFailure::contract)?;
        if citations.get("enabled").and_then(Value::as_bool).is_none() {
            return Err(BedrockFailure::contract(format!(
                "Bedrock {surface} document citations is malformed"
            )));
        }
    }
    validate_document_source(
        document.get("source"),
        policy.max_document_bytes,
        &format!("{surface} document"),
    )
}

fn validate_image_source(
    value: Option<&Value>,
    maximum_inline: u64,
    surface: &str,
) -> Result<(), BedrockFailure> {
    let source = value.and_then(Value::as_object).ok_or_else(|| {
        BedrockFailure::contract(format!("Bedrock {surface} source is missing or malformed"))
    })?;
    if source.len() != 1 {
        return Err(BedrockFailure::contract(format!(
            "Bedrock {surface} source must contain exactly one union member"
        )));
    }
    if let Some(encoded) = source.get("bytes") {
        let encoded = encoded.as_str().ok_or_else(|| {
            BedrockFailure::contract(format!("Bedrock {surface} bytes are not base64 text"))
        })?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|error| {
                BedrockFailure::contract(format!("Bedrock {surface} base64 is invalid: {error}"))
            })?;
        if decoded.is_empty() || decoded.len() as u64 > maximum_inline {
            return Err(BedrockFailure::contract(format!(
                "Bedrock {surface} inline bytes must be within 1..={maximum_inline}"
            )));
        }
        return Ok(());
    }
    if let Some(location) = source.get("s3Location") {
        let location = location.as_object().ok_or_else(|| {
            BedrockFailure::contract(format!("Bedrock {surface} s3Location is malformed"))
        })?;
        ensure_map_keys(location, &["uri", "bucketOwner"], "s3Location")
            .map_err(BedrockFailure::contract)?;
        let uri = location
            .get("uri")
            .and_then(Value::as_str)
            .filter(|uri| uri.starts_with("s3://") && uri.len() > 5)
            .ok_or_else(|| {
                BedrockFailure::contract(format!(
                    "Bedrock {surface} s3Location.uri must be an s3:// URI"
                ))
            })?;
        if uri.len() > 1024 {
            return Err(BedrockFailure::contract(format!(
                "Bedrock {surface} s3Location.uri exceeds 1024 bytes"
            )));
        }
        if let Some(owner) = location.get("bucketOwner") {
            let owner = owner.as_str().ok_or_else(|| {
                BedrockFailure::contract(format!("Bedrock {surface} bucketOwner is malformed"))
            })?;
            if owner.len() != 12 || !owner.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(BedrockFailure::contract(format!(
                    "Bedrock {surface} bucketOwner must contain 12 digits"
                )));
            }
        }
        return Ok(());
    }
    Err(BedrockFailure::contract(format!(
        "Bedrock {surface} source must be bytes or s3Location"
    )))
}

fn validate_document_source(
    value: Option<&Value>,
    maximum_inline: u64,
    surface: &str,
) -> Result<(), BedrockFailure> {
    let source = value.and_then(Value::as_object).ok_or_else(|| {
        BedrockFailure::contract(format!("Bedrock {surface} source is missing or malformed"))
    })?;
    if source.len() != 1 {
        return Err(BedrockFailure::contract(format!(
            "Bedrock {surface} source must contain exactly one union member"
        )));
    }
    if source.contains_key("bytes") || source.contains_key("s3Location") {
        return validate_image_source(value, maximum_inline, surface);
    }
    if let Some(text) = source.get("text") {
        let text = text
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                BedrockFailure::contract(format!("Bedrock {surface} text source is malformed"))
            })?;
        if text.len() as u64 > maximum_inline {
            return Err(BedrockFailure::contract(format!(
                "Bedrock {surface} text source exceeds {maximum_inline} bytes"
            )));
        }
        return Ok(());
    }
    if let Some(content) = source.get("content") {
        let content = content
            .as_array()
            .filter(|content| !content.is_empty())
            .ok_or_else(|| {
                BedrockFailure::contract(format!(
                    "Bedrock {surface} structured content must be a non-empty array"
                ))
            })?;
        let mut total = 0_u64;
        for (index, block) in content.iter().enumerate() {
            let block = block.as_object().ok_or_else(|| {
                BedrockFailure::contract(format!(
                    "Bedrock {surface} structured content[{index}] is not an object"
                ))
            })?;
            let text = if block.len() == 1 {
                block.get("text").and_then(Value::as_str)
            } else {
                None
            }
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                BedrockFailure::contract(format!(
                    "Bedrock {surface} structured content[{index}] must contain one non-empty text member"
                ))
            })?;
            total = total.checked_add(text.len() as u64).ok_or_else(|| {
                BedrockFailure::contract(format!(
                    "Bedrock {surface} structured content size overflowed"
                ))
            })?;
        }
        if total > maximum_inline {
            return Err(BedrockFailure::contract(format!(
                "Bedrock {surface} structured content exceeds {maximum_inline} bytes"
            )));
        }
        return Ok(());
    }
    Err(BedrockFailure::contract(format!(
        "Bedrock {surface} source must be bytes, s3Location, text, or content"
    )))
}

fn validate_document_name(name: &str, surface: &str) -> Result<(), BedrockFailure> {
    if name.is_empty() || name.len() > 200 || name.contains("  ") {
        return Err(BedrockFailure::contract(format!(
            "Bedrock {surface} document name violates length or spacing constraints"
        )));
    }
    if !name.chars().all(|character| {
        character.is_alphanumeric()
            || character.is_whitespace()
            || matches!(character, '-' | '(' | ')' | '[' | ']')
    }) {
        return Err(BedrockFailure::contract(format!(
            "Bedrock {surface} document name contains unsupported characters"
        )));
    }
    Ok(())
}

fn validate_reasoning(
    value: &Value,
    _policy: &CarrierPolicy,
    surface: &str,
) -> Result<(), BedrockFailure> {
    let reasoning = value.as_object().ok_or_else(|| {
        BedrockFailure::contract(format!("Bedrock {surface} reasoningContent is malformed"))
    })?;
    if reasoning.len() != 1 {
        return Err(BedrockFailure::contract(format!(
            "Bedrock {surface} reasoningContent must contain exactly one union member"
        )));
    }
    if let Some(text) = reasoning.get("reasoningText") {
        let text = text.as_object().ok_or_else(|| {
            BedrockFailure::contract(format!("Bedrock {surface} reasoningText is malformed"))
        })?;
        ensure_map_keys(text, &["text", "signature"], "reasoningText")
            .map_err(BedrockFailure::contract)?;
        required_map_string(text, "text", "reasoningText")?;
        if let Some(signature) = text.get("signature") {
            if signature
                .as_str()
                .filter(|value| !value.is_empty())
                .is_none()
            {
                return Err(BedrockFailure::contract(format!(
                    "Bedrock {surface} reasoning signature is malformed"
                )));
            }
        }
        return Ok(());
    }
    if let Some(redacted) = reasoning.get("redactedContent") {
        let encoded = redacted.as_str().ok_or_else(|| {
            BedrockFailure::contract(format!(
                "Bedrock {surface} redactedContent is not base64 text"
            ))
        })?;
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|error| {
                BedrockFailure::contract(format!(
                    "Bedrock {surface} redactedContent base64 is invalid: {error}"
                ))
            })?;
        return Ok(());
    }
    Err(BedrockFailure::contract(format!(
        "Bedrock {surface} reasoningContent has no supported member"
    )))
}

fn validate_tool_use(value: &Value, surface: &str) -> Result<(), BedrockFailure> {
    let tool = value.as_object().ok_or_else(|| {
        BedrockFailure::contract(format!("Bedrock {surface} toolUse is malformed"))
    })?;
    ensure_map_keys(tool, &["toolUseId", "name", "input", "type"], "toolUse")
        .map_err(BedrockFailure::contract)?;
    let id = required_map_string(tool, "toolUseId", "toolUse")?;
    validate_tool_id(&id, "toolUseId")?;
    let name = required_map_string(tool, "name", "toolUse")?;
    decode_tool_name(&name)?;
    if !tool.get("input").is_some_and(Value::is_object) {
        return Err(BedrockFailure::contract(format!(
            "Bedrock {surface} toolUse.input is not an object"
        )));
    }
    if let Some(server_type) = tool.get("type") {
        if server_type.as_str() != Some("server_tool_use") {
            return Err(BedrockFailure::contract(format!(
                "Bedrock {surface} toolUse.type is unsupported"
            )));
        }
    }
    Ok(())
}

fn validate_tool_result(
    value: &Value,
    policy: &CarrierPolicy,
    surface: &str,
) -> Result<(), BedrockFailure> {
    let result = value.as_object().ok_or_else(|| {
        BedrockFailure::contract(format!("Bedrock {surface} toolResult is malformed"))
    })?;
    ensure_map_keys(
        result,
        &["toolUseId", "content", "status", "type"],
        "toolResult",
    )
    .map_err(BedrockFailure::contract)?;
    let id = required_map_string(result, "toolUseId", "toolResult")?;
    validate_tool_id(&id, "toolResult.toolUseId")?;
    if let Some(status) = result.get("status") {
        if !matches!(status.as_str(), Some("success" | "error")) {
            return Err(BedrockFailure::contract(format!(
                "Bedrock {surface} toolResult.status is unsupported"
            )));
        }
    }
    let content = result
        .get("content")
        .and_then(Value::as_array)
        .filter(|content| !content.is_empty())
        .ok_or_else(|| {
            BedrockFailure::contract(format!(
                "Bedrock {surface} toolResult.content must be non-empty"
            ))
        })?;
    for item in content {
        validate_tool_result_content(item, policy, surface)?;
    }
    Ok(())
}

fn validate_tool_result_content(
    item: &Value,
    policy: &CarrierPolicy,
    surface: &str,
) -> Result<(), BedrockFailure> {
    let object = item.as_object().ok_or_else(|| {
        BedrockFailure::contract(format!("Bedrock {surface} tool result item is malformed"))
    })?;
    if object.len() != 1 {
        return Err(BedrockFailure::contract(format!(
            "Bedrock {surface} tool result item must contain one union member"
        )));
    }
    let (kind, value) = object.iter().next().unwrap();
    match kind.as_str() {
        "text" => {
            if value.as_str().filter(|value| !value.is_empty()).is_none() {
                return Err(BedrockFailure::contract(format!(
                    "Bedrock {surface} tool result text is malformed"
                )));
            }
        }
        "json" => {}
        "image" => validate_image(value, policy, surface)?,
        "document" => validate_document(value, policy, surface)?,
        other => {
            return Err(BedrockFailure::contract(format!(
                "Bedrock {surface} tool result type '{other}' is unsupported"
            )));
        }
    }
    Ok(())
}

fn tool_result_block(value: &Value, policy: &CarrierPolicy) -> Result<Value, BedrockFailure> {
    let result: ToolResultArtifact = serde_json::from_value(value.clone()).map_err(|error| {
        BedrockFailure::contract(format!(
            "tool result is not the strict shared envelope: {error}"
        ))
    })?;
    let correlation_id = result
        .correlation_id
        .as_deref()
        .ok_or_else(|| BedrockFailure::contract("tool result has no correlation_id"))?;
    validate_tool_id(&correlation_id, "tool result correlation_id")?;
    let content = if result.ok {
        let body = result
            .body
            .as_ref()
            .ok_or_else(|| BedrockFailure::contract("successful tool result has no body"))?;
        if let Some(content) = body.get("content").and_then(Value::as_array) {
            for item in content {
                validate_tool_result_content(item, policy, "tool result")?;
            }
            content.clone()
        } else {
            vec![json!({"json": body})]
        }
    } else {
        vec![json!({
            "json": {"error": result.error.as_ref().ok_or_else(|| {
                BedrockFailure::contract("failed tool result has no error")
            })?}
        })]
    };
    Ok(json!({
        "toolResult": {
            "toolUseId": correlation_id,
            "content": content,
            "status": if result.ok { "success" } else { "error" }
        }
    }))
}

fn push_message(
    messages: &mut Vec<Value>,
    role: &str,
    blocks: Vec<Value>,
) -> Result<(), BedrockFailure> {
    if blocks.is_empty() {
        return Err(BedrockFailure::contract(format!(
            "Bedrock cannot append empty {role} content"
        )));
    }
    if let Some(last) = messages.last_mut() {
        if last.get("role").and_then(Value::as_str) == Some(role) {
            let content = last
                .get_mut("content")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| {
                    BedrockFailure::contract("accumulated Bedrock message content is malformed")
                })?;
            content.extend(blocks);
            return Ok(());
        }
    }
    messages.push(json!({"role": role, "content": blocks}));
    Ok(())
}

fn validate_tool_order(messages: &[Value]) -> Result<(), BedrockFailure> {
    let mut known = BTreeSet::new();
    let mut resolved = BTreeSet::new();
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let blocks = message
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| BedrockFailure::contract("Bedrock message content is malformed"))?;
        for block in blocks {
            if let Some(tool) = block.get("toolUse") {
                let id = required_string(tool, "toolUseId", "toolUse")?;
                if !known.insert(id.clone()) {
                    return Err(BedrockFailure::contract(format!(
                        "Bedrock conversation repeats toolUseId '{id}'"
                    )));
                }
            }
            if let Some(result) = block.get("toolResult") {
                if role != "user" {
                    return Err(BedrockFailure::contract(
                        "Bedrock toolResult must be carried by a user message",
                    ));
                }
                let id = required_string(result, "toolUseId", "toolResult")?;
                if !known.contains(&id) {
                    return Err(BedrockFailure::contract(format!(
                        "Bedrock toolResult '{id}' has no preceding toolUse"
                    )));
                }
                if !resolved.insert(id.clone()) {
                    return Err(BedrockFailure::contract(format!(
                        "Bedrock conversation repeats toolResult '{id}'"
                    )));
                }
            }
        }
    }
    Ok(())
}

fn encode_tool_name(name: &str) -> Result<String, BedrockFailure> {
    if name.is_empty() || name.trim() != name {
        return Err(BedrockFailure::contract(
            "tool name must be non-empty and trimmed",
        ));
    }
    let mut encoded = String::with_capacity(name.len());
    for byte in name.bytes() {
        match byte {
            b'-' => encoded.push_str("-h"),
            b'.' => encoded.push_str("-d"),
            byte if byte.is_ascii_alphanumeric() || byte == b'_' => encoded.push(byte as char),
            _ => {
                return Err(BedrockFailure::contract(format!(
                    "tool name '{name}' contains a byte unsupported by Bedrock"
                )));
            }
        }
    }
    if encoded.len() > 64 {
        return Err(BedrockFailure::contract(format!(
            "encoded Bedrock tool name '{encoded}' exceeds 64 bytes"
        )));
    }
    Ok(encoded)
}

fn decode_tool_name(encoded: &str) -> Result<String, BedrockFailure> {
    if encoded.is_empty() || encoded.len() > 64 {
        return Err(BedrockFailure::contract(
            "Bedrock tool name length is outside 1..=64",
        ));
    }
    let mut output = String::with_capacity(encoded.len());
    let mut bytes = encoded.bytes();
    while let Some(byte) = bytes.next() {
        if byte != b'-' {
            if !(byte.is_ascii_alphanumeric() || byte == b'_') {
                return Err(BedrockFailure::contract(format!(
                    "Bedrock tool name '{encoded}' contains an invalid byte"
                )));
            }
            output.push(byte as char);
            continue;
        }
        match bytes.next() {
            Some(b'h') => output.push('-'),
            Some(b'd') => output.push('.'),
            _ => {
                return Err(BedrockFailure::contract(format!(
                    "Bedrock tool name '{encoded}' has an invalid escape"
                )));
            }
        }
    }
    Ok(output)
}

fn validate_tool_id(value: &str, surface: &str) -> Result<(), BedrockFailure> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-'))
    {
        return Err(BedrockFailure::contract(format!(
            "Bedrock {surface} violates the tool-use id contract"
        )));
    }
    Ok(())
}

fn payload_label(artifact_id: &str, value: &Value) -> Option<String> {
    value
        .get("label")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.trim() == *value)
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

fn required_string(value: &Value, key: &str, surface: &str) -> Result<String, BedrockFailure> {
    let object = value
        .as_object()
        .ok_or_else(|| BedrockFailure::contract(format!("Bedrock {surface} is not an object")))?;
    required_map_string(object, key, surface)
}

fn required_map_string(
    value: &Map<String, Value>,
    key: &str,
    surface: &str,
) -> Result<String, BedrockFailure> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.trim() == *value)
        .map(str::to_string)
        .ok_or_else(|| {
            BedrockFailure::contract(format!(
                "Bedrock {surface} has no non-empty trimmed '{key}'"
            ))
        })
}

fn render_json(value: &Value) -> Result<String, BedrockFailure> {
    serde_json::to_string(value).map_err(|error| {
        BedrockFailure::contract(format!("Bedrock context failed to encode: {error}"))
    })
}

async fn read_bounded(
    mut response: reqwest::Response,
    maximum: usize,
    retryable: bool,
    streamed: bool,
) -> Result<Vec<u8>, BedrockFailure> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(BedrockFailure::contract(format!(
            "Bedrock response content-length exceeds {maximum} bytes"
        )));
    }
    let mut bytes = Vec::new();
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) if streamed && !bytes.is_empty() => {
                return Err(BedrockFailure::transport(
                    format!("Bedrock partial stream cannot be replayed: {error}"),
                    false,
                ));
            }
            Err(error) => {
                return Err(BedrockFailure::transport(
                    format!("Bedrock response stream failed: {error}"),
                    retryable,
                ));
            }
        };
        let next = bytes.len().checked_add(chunk.len()).ok_or_else(|| {
            BedrockFailure::contract("Bedrock response size overflowed host width")
        })?;
        if next > maximum {
            return Err(BedrockFailure::contract(format!(
                "Bedrock response exceeds {maximum} bytes"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn read_bedrock_stream(
    mut response: reqwest::Response,
    maximum: usize,
    config: &DriverConfig,
    mut transport: TransportEvidence,
) -> Result<ProviderResponse, BedrockFailure> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(BedrockFailure::contract(format!(
            "Bedrock response content-length exceeds {maximum} bytes"
        )));
    }
    let mut decoder = EventStreamFrameDecoder::new();
    let mut bytes = 0usize;
    let mut hasher = blake3::Hasher::new();
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) if bytes > 0 => {
                return Err(BedrockFailure::partial_stream(
                    format!("Bedrock partial stream cannot be replayed: {error}"),
                    bytes,
                    hasher.finalize().to_hex().to_string(),
                ));
            }
            Err(error) => {
                return Err(BedrockFailure::transport(
                    format!("Bedrock response stream failed: {error}"),
                    config.options.retry.transport,
                ));
            }
        };
        bytes = bytes.checked_add(chunk.len()).ok_or_else(|| {
            BedrockFailure::contract("Bedrock response size overflowed host width")
        })?;
        hasher.update(&chunk);
        if bytes > maximum {
            return Err(BedrockFailure::partial_stream(
                format!("Bedrock partial stream exceeds {maximum} bytes and cannot be replayed"),
                bytes,
                hasher.finalize().to_hex().to_string(),
            ));
        }
        let evidence = ResponseBodyEvidence {
            bytes,
            blake3: hasher.clone().finalize().to_hex().to_string(),
        };
        decoder.push(&chunk, maximum, &evidence)?;
    }
    let evidence = ResponseBodyEvidence {
        bytes,
        blake3: hasher.finalize().to_hex().to_string(),
    };
    transport.response_bytes = evidence.bytes;
    transport.response_blake3.clone_from(&evidence.blake3);
    decoder.finish(config, transport, &evidence)
}

impl EventStreamFrameDecoder {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
            accumulator: StreamAccumulator::new(),
            frame_count: 0,
        }
    }

    fn push(
        &mut self,
        bytes: &[u8],
        maximum_frame_bytes: usize,
        evidence: &ResponseBodyEvidence,
    ) -> Result<(), BedrockFailure> {
        self.pending.extend_from_slice(bytes);
        let mut consumed = 0usize;
        loop {
            let remaining = &self.pending[consumed..];
            if remaining.len() < 4 {
                break;
            }
            let total_length = u32::from_be_bytes(remaining[..4].try_into().unwrap()) as usize;
            if total_length < 16 || total_length > maximum_frame_bytes {
                return Err(BedrockFailure::response_evidence(
                    format!(
                        "AWS EventStream frame length {total_length} is outside 16..={maximum_frame_bytes}"
                    ),
                    evidence.bytes,
                    evidence.blake3.clone(),
                ));
            }
            if remaining.len() < total_length {
                break;
            }
            let frame = remaining[..total_length].to_vec();
            let messages = decode_messages(&frame, maximum_frame_bytes).map_err(|detail| {
                BedrockFailure::response_evidence(
                    format!("Bedrock ConverseStream framing failed: {detail}"),
                    evidence.bytes,
                    evidence.blake3.clone(),
                )
            })?;
            if messages.len() != 1 {
                return Err(BedrockFailure::response_evidence(
                    "Bedrock incremental EventStream frame decoded to multiple messages",
                    evidence.bytes,
                    evidence.blake3.clone(),
                ));
            }
            apply_eventstream_message(
                messages.into_iter().next().unwrap(),
                &mut self.accumulator,
                evidence,
            )?;
            self.frame_count = self.frame_count.checked_add(1).ok_or_else(|| {
                BedrockFailure::response_evidence(
                    "Bedrock EventStream frame count overflowed",
                    evidence.bytes,
                    evidence.blake3.clone(),
                )
            })?;
            consumed = consumed.checked_add(total_length).ok_or_else(|| {
                BedrockFailure::response_evidence(
                    "Bedrock EventStream cursor overflowed",
                    evidence.bytes,
                    evidence.blake3.clone(),
                )
            })?;
        }
        if consumed > 0 {
            self.pending.drain(..consumed);
        }
        Ok(())
    }

    fn finish(
        self,
        config: &DriverConfig,
        transport: TransportEvidence,
        evidence: &ResponseBodyEvidence,
    ) -> Result<ProviderResponse, BedrockFailure> {
        if !self.pending.is_empty() {
            return Err(BedrockFailure::response_evidence(
                format!(
                    "AWS EventStream ends with an incomplete {} byte frame",
                    self.pending.len()
                ),
                evidence.bytes,
                evidence.blake3.clone(),
            ));
        }
        if self.frame_count == 0 {
            return Err(BedrockFailure::response_evidence(
                "AWS EventStream response contains no frames",
                evidence.bytes,
                evidence.blake3.clone(),
            ));
        }
        self.accumulator
            .finish(config, transport)
            .map_err(|detail| {
                BedrockFailure::response_evidence(detail, evidence.bytes, evidence.blake3.clone())
            })
    }
}

fn apply_eventstream_message(
    message: EventStreamMessage,
    accumulator: &mut StreamAccumulator,
    evidence: &ResponseBodyEvidence,
) -> Result<(), BedrockFailure> {
    let message_type = message
        .string_header(":message-type")
        .map_err(|detail| {
            BedrockFailure::response_evidence(detail, evidence.bytes, evidence.blake3.clone())
        })?
        .ok_or_else(|| {
            BedrockFailure::response_evidence(
                "Bedrock ConverseStream frame has no :message-type",
                evidence.bytes,
                evidence.blake3.clone(),
            )
        })?;
    match message_type {
        "event" => {
            require_event_json_content_type(&message).map_err(|detail| {
                BedrockFailure::response_evidence(detail, evidence.bytes, evidence.blake3.clone())
            })?;
            let event_type = message
                .string_header(":event-type")
                .map_err(|detail| {
                    BedrockFailure::response_evidence(
                        detail,
                        evidence.bytes,
                        evidence.blake3.clone(),
                    )
                })?
                .ok_or_else(|| {
                    BedrockFailure::response_evidence(
                        "Bedrock event frame has no :event-type",
                        evidence.bytes,
                        evidence.blake3.clone(),
                    )
                })?;
            let value = parse_event_payload(&message, event_type).map_err(|detail| {
                BedrockFailure::response_evidence(detail, evidence.bytes, evidence.blake3.clone())
            })?;
            accumulator.apply(event_type, &value).map_err(|detail| {
                BedrockFailure::response_evidence(detail, evidence.bytes, evidence.blake3.clone())
            })
        }
        "exception" => {
            require_event_json_content_type(&message).map_err(|detail| {
                BedrockFailure::response_evidence(detail, evidence.bytes, evidence.blake3.clone())
            })?;
            let event_type = message
                .string_header(":exception-type")
                .map_err(|detail| {
                    BedrockFailure::response_evidence(
                        detail,
                        evidence.bytes,
                        evidence.blake3.clone(),
                    )
                })?
                .ok_or_else(|| {
                    BedrockFailure::response_evidence(
                        "Bedrock exception frame has no :exception-type",
                        evidence.bytes,
                        evidence.blake3.clone(),
                    )
                })?;
            let payload = parse_event_payload(&message, event_type).map_err(|detail| {
                BedrockFailure::response_evidence(detail, evidence.bytes, evidence.blake3.clone())
            })?;
            let detail = format!(
                "Bedrock ConverseStream emitted '{event_type}': {}",
                serde_json::to_string(&payload).map_err(|error| {
                    BedrockFailure::response_evidence(
                        format!("Bedrock stream exception did not encode: {error}"),
                        evidence.bytes,
                        evidence.blake3.clone(),
                    )
                })?
            );
            Err(BedrockFailure::stream_event_evidence(
                event_type,
                detail,
                Some(payload),
                evidence.bytes,
                evidence.blake3.clone(),
                false,
            ))
        }
        "error" => {
            let event_type = message
                .string_header(":error-code")
                .map_err(|detail| {
                    BedrockFailure::response_evidence(
                        detail,
                        evidence.bytes,
                        evidence.blake3.clone(),
                    )
                })?
                .ok_or_else(|| {
                    BedrockFailure::response_evidence(
                        "Bedrock error frame has no :error-code",
                        evidence.bytes,
                        evidence.blake3.clone(),
                    )
                })?;
            let detail = message
                .string_header(":error-message")
                .map_err(|error| {
                    BedrockFailure::response_evidence(
                        error,
                        evidence.bytes,
                        evidence.blake3.clone(),
                    )
                })?
                .unwrap_or("AWS EventStream error frame has no error message");
            Err(BedrockFailure::stream_event_evidence(
                event_type,
                format!("Bedrock ConverseStream emitted '{event_type}': {detail}"),
                None,
                evidence.bytes,
                evidence.blake3.clone(),
                false,
            ))
        }
        other => Err(BedrockFailure::response_evidence(
            format!("Bedrock ConverseStream message type '{other}' is unsupported"),
            evidence.bytes,
            evidence.blake3.clone(),
        )),
    }
}

fn endpoint_error_code(headers: &HeaderMap, bytes: &[u8]) -> Option<String> {
    headers
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(':').next())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            serde_json::from_slice::<Value>(bytes)
                .ok()
                .and_then(|value| {
                    value
                        .get("__type")
                        .or_else(|| value.get("code"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .and_then(|value| value.rsplit('#').next().map(str::to_string))
        })
}

fn cached_credentials_rejected(status: StatusCode, code: Option<&str>) -> bool {
    status == StatusCode::UNAUTHORIZED
        || status == StatusCode::FORBIDDEN
            && code.is_some_and(|code| {
                code.starts_with("ExpiredToken")
                    || matches!(code, "InvalidClientTokenId" | "UnrecognizedClientException")
            })
}

fn endpoint_failure_retryable(retry: &RetryConfig, status: StatusCode, code: Option<&str>) -> bool {
    !matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
        && (retry.retryable_statuses.contains(&status.as_u16())
            || code.is_some_and(|code| {
                retry
                    .retryable_error_codes
                    .iter()
                    .any(|candidate| candidate == code)
            }))
}

fn parse_converse_response(
    bytes: &[u8],
    config: &DriverConfig,
    transport: TransportEvidence,
) -> Result<ProviderResponse, BedrockFailure> {
    let value: Value = serde_json::from_slice(bytes).map_err(|error| {
        BedrockFailure::response(
            format!("Bedrock Converse response is not JSON: {error}"),
            bytes,
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        BedrockFailure::response("Bedrock Converse response is not an object", bytes)
    })?;
    let output = object
        .get("output")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            BedrockFailure::response("Bedrock Converse response has no output object", bytes)
        })?;
    if output.len() != 1 || !output.contains_key("message") {
        return Err(BedrockFailure::response(
            "Bedrock Converse output must contain only the message union member",
            bytes,
        ));
    }
    let message = output
        .get("message")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            BedrockFailure::response("Bedrock Converse output.message is malformed", bytes)
        })?;
    ensure_map_keys(message, &["role", "content"], "Converse output.message")
        .map_err(|detail| BedrockFailure::response(detail, bytes))?;
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return Err(BedrockFailure::response(
            "Bedrock Converse output.message role is not assistant",
            bytes,
        ));
    }
    let content = message
        .get("content")
        .and_then(Value::as_array)
        .filter(|content| !content.is_empty())
        .cloned()
        .ok_or_else(|| {
            BedrockFailure::response(
                "Bedrock Converse output.message content is not an array",
                bytes,
            )
        })?;
    if content.len() > config.options.account_limits.max_content_blocks {
        return Err(BedrockFailure::response(
            format!(
                "Bedrock Converse returned {} content blocks, above descriptor limit {}",
                content.len(),
                config.options.account_limits.max_content_blocks
            ),
            bytes,
        ));
    }
    for (index, block) in content.iter().enumerate() {
        validate_content_block(
            block,
            &config.response_content_types,
            &config.options.carrier_policy,
            &format!("response content[{index}]"),
        )
        .map_err(|failure| BedrockFailure::response(failure.detail, bytes))?;
        validate_tool_input_size(
            block,
            config.options.account_limits.max_tool_input_bytes,
            &format!("response content[{index}]"),
        )
        .map_err(|detail| BedrockFailure::response(detail, bytes))?;
    }
    let stop_reason = object
        .get("stopReason")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.trim() == *value)
        .ok_or_else(|| {
            BedrockFailure::response("Bedrock Converse response has no stopReason", bytes)
        })?
        .to_string();
    let usage = object
        .get("usage")
        .cloned()
        .ok_or_else(|| BedrockFailure::response("Bedrock Converse response has no usage", bytes))?;
    validate_usage(&usage).map_err(|detail| BedrockFailure::response(detail, bytes))?;
    let metrics = object.get("metrics").cloned().ok_or_else(|| {
        BedrockFailure::response("Bedrock Converse response has no metrics", bytes)
    })?;
    validate_metrics(&metrics).map_err(|detail| BedrockFailure::response(detail, bytes))?;
    let additional = object
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "output" | "stopReason" | "usage" | "metrics"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<Map<String, Value>>();
    Ok(ProviderResponse {
        model: config.model.clone(),
        stop_reason,
        content,
        usage,
        metrics,
        additional_fields: (!additional.is_empty()).then_some(Value::Object(additional)),
        transport,
    })
}

fn require_event_json_content_type(message: &EventStreamMessage) -> Result<(), String> {
    let content_type = message.string_header(":content-type")?;
    if content_type != Some("application/json") {
        return Err(format!(
            "Bedrock ConverseStream frame has content type {:?}, expected application/json",
            content_type
        ));
    }
    Ok(())
}

fn parse_event_payload(message: &EventStreamMessage, event_type: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_slice(&message.payload).map_err(|error| {
        format!("Bedrock ConverseStream '{event_type}' payload is not JSON: {error}")
    })?;
    if !value.is_object() {
        return Err(format!(
            "Bedrock ConverseStream '{event_type}' payload is not an object"
        ));
    }
    Ok(value)
}

impl StreamAccumulator {
    fn new() -> Self {
        Self {
            role: None,
            blocks: BTreeMap::new(),
            stopped_blocks: BTreeSet::new(),
            stop_reason: None,
            usage: None,
            metrics: None,
            additional_fields: None,
            message_started: false,
            message_stopped: false,
            metadata_seen: false,
            event_types: BTreeMap::new(),
        }
    }

    fn apply(&mut self, event_type: &str, value: &Value) -> Result<(), String> {
        *self.event_types.entry(event_type.to_string()).or_insert(0) += 1;
        if self.metadata_seen {
            return Err(format!(
                "Bedrock ConverseStream emitted '{event_type}' after metadata"
            ));
        }
        match event_type {
            "messageStart" => self.message_start(value),
            "contentBlockStart" => self.content_block_start(value),
            "contentBlockDelta" => self.content_block_delta(value),
            "contentBlockStop" => self.content_block_stop(value),
            "messageStop" => self.message_stop(value),
            "metadata" => self.metadata(value),
            other => Err(format!(
                "Bedrock ConverseStream event type '{other}' is unsupported"
            )),
        }
    }

    fn message_start(&mut self, value: &Value) -> Result<(), String> {
        if self.message_started || !self.blocks.is_empty() || self.message_stopped {
            return Err("Bedrock ConverseStream repeats or misorders messageStart".to_string());
        }
        ensure_object_keys(value, &["role"], "messageStart")?;
        let role = required_plain_string(value, "role", "messageStart")?;
        if role != "assistant" {
            return Err(format!(
                "Bedrock ConverseStream messageStart role '{role}' is not assistant"
            ));
        }
        self.role = Some(role);
        self.message_started = true;
        Ok(())
    }

    fn content_block_start(&mut self, value: &Value) -> Result<(), String> {
        self.require_open_message("contentBlockStart")?;
        ensure_object_keys(value, &["start", "contentBlockIndex"], "contentBlockStart")?;
        let index = stream_index(value, "contentBlockStart")?;
        if self.blocks.contains_key(&index) || self.stopped_blocks.contains(&index) {
            return Err(format!(
                "Bedrock ConverseStream repeats content block index {index}"
            ));
        }
        let start = value
            .get("start")
            .and_then(Value::as_object)
            .ok_or_else(|| "Bedrock contentBlockStart.start is not an object".to_string())?;
        if start.len() != 1 {
            return Err(
                "Bedrock contentBlockStart.start must contain one union member".to_string(),
            );
        }
        let (kind, body) = start.iter().next().unwrap();
        let block = match kind.as_str() {
            "toolUse" => {
                let body = body
                    .as_object()
                    .ok_or_else(|| "Bedrock toolUse start is not an object".to_string())?;
                ensure_map_keys(body, &["toolUseId", "name", "type"], "toolUse start")?;
                let id = required_map_plain_string(body, "toolUseId", "toolUse start")?;
                let name = required_map_plain_string(body, "name", "toolUse start")?;
                validate_tool_id(&id, "stream toolUseId").map_err(|failure| failure.detail)?;
                decode_tool_name(&name).map_err(|failure| failure.detail)?;
                let server_type = optional_map_string(body, "type", "toolUse start")?;
                if server_type
                    .as_deref()
                    .is_some_and(|value| value != "server_tool_use")
                {
                    return Err("Bedrock toolUse start has unsupported type".to_string());
                }
                StreamBlock::ToolUse {
                    id,
                    name,
                    input: String::new(),
                    server_type,
                }
            }
            "image" => {
                let body = body
                    .as_object()
                    .ok_or_else(|| "Bedrock image start is not an object".to_string())?;
                ensure_map_keys(body, &["format"], "image start")?;
                StreamBlock::Image {
                    format: required_map_plain_string(body, "format", "image start")?,
                    bytes: Vec::new(),
                }
            }
            "toolResult" => {
                let body = body
                    .as_object()
                    .ok_or_else(|| "Bedrock toolResult start is not an object".to_string())?;
                ensure_map_keys(body, &["toolUseId", "status", "type"], "toolResult start")?;
                let id = required_map_plain_string(body, "toolUseId", "toolResult start")?;
                validate_tool_id(&id, "stream toolResult.toolUseId")
                    .map_err(|failure| failure.detail)?;
                let status = optional_map_string(body, "status", "toolResult start")?;
                if status
                    .as_deref()
                    .is_some_and(|value| !matches!(value, "success" | "error"))
                {
                    return Err("Bedrock toolResult start has unsupported status".to_string());
                }
                StreamBlock::ToolResult {
                    id,
                    status,
                    result_type: optional_map_string(body, "type", "toolResult start")?,
                    content: Vec::new(),
                }
            }
            other => {
                return Err(format!(
                    "Bedrock contentBlockStart union member '{other}' is unsupported"
                ));
            }
        };
        self.blocks.insert(index, block);
        Ok(())
    }

    fn content_block_delta(&mut self, value: &Value) -> Result<(), String> {
        self.require_open_message("contentBlockDelta")?;
        ensure_object_keys(value, &["delta", "contentBlockIndex"], "contentBlockDelta")?;
        let index = stream_index(value, "contentBlockDelta")?;
        if self.stopped_blocks.contains(&index) {
            return Err(format!(
                "Bedrock contentBlockDelta references stopped index {index}"
            ));
        }
        let delta = value
            .get("delta")
            .and_then(Value::as_object)
            .ok_or_else(|| "Bedrock contentBlockDelta.delta is not an object".to_string())?;
        if delta.len() != 1 {
            return Err(
                "Bedrock contentBlockDelta.delta must contain one union member".to_string(),
            );
        }
        let (kind, fragment) = delta.iter().next().unwrap();
        match kind.as_str() {
            "text" => {
                let fragment = fragment
                    .as_str()
                    .ok_or_else(|| "Bedrock text delta is not a string".to_string())?;
                match self.blocks.get_mut(&index) {
                    Some(StreamBlock::Text(text)) => text.push_str(fragment),
                    Some(StreamBlock::Citations { text, .. }) => text.push_str(fragment),
                    Some(_) => {
                        return Err(format!(
                            "Bedrock text delta conflicts with block index {index}"
                        ));
                    }
                    None => {
                        self.blocks
                            .insert(index, StreamBlock::Text(fragment.to_string()));
                    }
                }
            }
            "toolUse" => {
                let fragment = fragment
                    .as_object()
                    .ok_or_else(|| "Bedrock toolUse delta is not an object".to_string())?;
                ensure_map_keys(fragment, &["input"], "toolUse delta")?;
                let input = required_map_string_allow_empty(fragment, "input", "toolUse delta")?;
                match self.blocks.get_mut(&index) {
                    Some(StreamBlock::ToolUse { input: current, .. }) => current.push_str(&input),
                    _ => {
                        return Err(format!(
                            "Bedrock toolUse delta has no matching start at index {index}"
                        ));
                    }
                }
            }
            "reasoningContent" => {
                let fragment = fragment
                    .as_object()
                    .ok_or_else(|| "Bedrock reasoningContent delta is not an object".to_string())?;
                if fragment.len() != 1 {
                    return Err(
                        "Bedrock reasoningContent delta must contain one union member".to_string(),
                    );
                }
                self.blocks
                    .entry(index)
                    .or_insert_with(|| StreamBlock::Reasoning {
                        text: String::new(),
                        signature: String::new(),
                        redacted: Vec::new(),
                    });
                let StreamBlock::Reasoning {
                    text,
                    signature,
                    redacted,
                } = self.blocks.get_mut(&index).unwrap()
                else {
                    return Err(format!(
                        "Bedrock reasoningContent delta conflicts with block index {index}"
                    ));
                };
                let (member, value) = fragment.iter().next().unwrap();
                match member.as_str() {
                    "text" => text.push_str(value.as_str().ok_or_else(|| {
                        "Bedrock reasoningContent.text delta is not a string".to_string()
                    })?),
                    "signature" => signature.push_str(value.as_str().ok_or_else(|| {
                        "Bedrock reasoningContent.signature delta is not a string".to_string()
                    })?),
                    "redactedContent" => {
                        let encoded = value.as_str().ok_or_else(|| {
                            "Bedrock reasoningContent.redactedContent is not base64 text"
                                .to_string()
                        })?;
                        let decoded = base64::engine::general_purpose::STANDARD
                            .decode(encoded)
                            .map_err(|error| {
                                format!("Bedrock reasoningContent.redactedContent is invalid base64: {error}")
                            })?;
                        redacted.extend_from_slice(&decoded);
                    }
                    other => {
                        return Err(format!(
                            "Bedrock reasoningContent delta member '{other}' is unsupported"
                        ));
                    }
                }
            }
            "citation" => {
                let citation = fragment
                    .as_object()
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| "Bedrock citation delta is not a non-empty object".to_string())?
                    .clone();
                let prior = self.blocks.remove(&index);
                let block = match prior {
                    None => StreamBlock::Citations {
                        text: String::new(),
                        citations: vec![Value::Object(citation)],
                    },
                    Some(StreamBlock::Text(text)) => StreamBlock::Citations {
                        text,
                        citations: vec![Value::Object(citation)],
                    },
                    Some(StreamBlock::Citations {
                        text,
                        mut citations,
                    }) => {
                        citations.push(Value::Object(citation));
                        StreamBlock::Citations { text, citations }
                    }
                    Some(other) => {
                        self.blocks.insert(index, other);
                        return Err(format!(
                            "Bedrock citation delta conflicts with block index {index}"
                        ));
                    }
                };
                self.blocks.insert(index, block);
            }
            "image" => {
                let image = fragment
                    .as_object()
                    .ok_or_else(|| "Bedrock image delta is not an object".to_string())?;
                ensure_map_keys(image, &["source", "error"], "image delta")?;
                if let Some(error) = image.get("error") {
                    return Err(format!(
                        "Bedrock image delta carries an error: {}",
                        serde_json::to_string(error).map_err(|failure| failure.to_string())?
                    ));
                }
                let source = image
                    .get("source")
                    .and_then(Value::as_object)
                    .ok_or_else(|| "Bedrock image delta has no source object".to_string())?;
                if source.len() != 1 || !source.contains_key("bytes") {
                    return Err("Bedrock image delta source must contain only bytes".to_string());
                }
                let encoded = source
                    .get("bytes")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "Bedrock image delta bytes are not base64 text".to_string())?;
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|error| {
                        format!("Bedrock image delta bytes are invalid base64: {error}")
                    })?;
                match self.blocks.get_mut(&index) {
                    Some(StreamBlock::Image { bytes, .. }) => bytes.extend_from_slice(&decoded),
                    _ => {
                        return Err(format!(
                            "Bedrock image delta has no matching start at index {index}"
                        ));
                    }
                }
            }
            "toolResult" => {
                let parts = fragment
                    .as_array()
                    .ok_or_else(|| "Bedrock toolResult delta is not an array".to_string())?;
                let Some(StreamBlock::ToolResult { content, .. }) = self.blocks.get_mut(&index)
                else {
                    return Err(format!(
                        "Bedrock toolResult delta has no matching start at index {index}"
                    ));
                };
                for part in parts {
                    append_tool_result_delta(content, part)?;
                }
            }
            other => {
                return Err(format!(
                    "Bedrock contentBlockDelta union member '{other}' is unsupported"
                ));
            }
        }
        Ok(())
    }

    fn content_block_stop(&mut self, value: &Value) -> Result<(), String> {
        self.require_open_message("contentBlockStop")?;
        ensure_object_keys(value, &["contentBlockIndex"], "contentBlockStop")?;
        let index = stream_index(value, "contentBlockStop")?;
        if !self.blocks.contains_key(&index) {
            return Err(format!(
                "Bedrock contentBlockStop references unknown index {index}"
            ));
        }
        if !self.stopped_blocks.insert(index) {
            return Err(format!("Bedrock contentBlockStop repeats index {index}"));
        }
        Ok(())
    }

    fn message_stop(&mut self, value: &Value) -> Result<(), String> {
        self.require_open_message("messageStop")?;
        if self.blocks.len() != self.stopped_blocks.len() {
            return Err(
                "Bedrock messageStop arrived before every content block stopped".to_string(),
            );
        }
        ensure_object_keys(
            value,
            &["stopReason", "additionalModelResponseFields"],
            "messageStop",
        )?;
        self.stop_reason = Some(required_plain_string(value, "stopReason", "messageStop")?);
        if let Some(additional) = value.get("additionalModelResponseFields") {
            self.add_additional("additionalModelResponseFields", additional.clone())?;
        }
        self.message_stopped = true;
        Ok(())
    }

    fn metadata(&mut self, value: &Value) -> Result<(), String> {
        if !self.message_stopped || self.metadata_seen {
            return Err("Bedrock ConverseStream metadata is repeated or misordered".to_string());
        }
        ensure_object_keys(
            value,
            &[
                "usage",
                "metrics",
                "trace",
                "performanceConfig",
                "serviceTier",
            ],
            "metadata",
        )?;
        let usage = value
            .get("usage")
            .cloned()
            .ok_or_else(|| "Bedrock ConverseStream metadata has no usage".to_string())?;
        validate_usage(&usage)?;
        let metrics = value
            .get("metrics")
            .cloned()
            .ok_or_else(|| "Bedrock ConverseStream metadata has no metrics".to_string())?;
        validate_metrics(&metrics)?;
        self.usage = Some(usage);
        self.metrics = Some(metrics);
        if let Some(trace) = value.get("trace") {
            self.add_additional("trace", trace.clone())?;
        }
        if let Some(performance) = value.get("performanceConfig") {
            self.add_additional("performanceConfig", performance.clone())?;
        }
        if let Some(service_tier) = value.get("serviceTier") {
            self.add_additional("serviceTier", service_tier.clone())?;
        }
        self.metadata_seen = true;
        Ok(())
    }

    fn require_open_message(&self, event: &str) -> Result<(), String> {
        if !self.message_started || self.message_stopped {
            return Err(format!(
                "Bedrock ConverseStream '{event}' occurred outside an open message"
            ));
        }
        Ok(())
    }

    fn add_additional(&mut self, key: &str, value: Value) -> Result<(), String> {
        let additional = self
            .additional_fields
            .get_or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or_else(|| "Bedrock stream additional fields state is malformed".to_string())?;
        if additional.insert(key.to_string(), value).is_some() {
            return Err(format!(
                "Bedrock ConverseStream repeats additional field '{key}'"
            ));
        }
        Ok(())
    }

    fn finish(
        self,
        config: &DriverConfig,
        mut transport: TransportEvidence,
    ) -> Result<ProviderResponse, String> {
        if !self.message_started || !self.message_stopped || !self.metadata_seen {
            return Err(
                "Bedrock ConverseStream did not complete messageStart/messageStop/metadata"
                    .to_string(),
            );
        }
        if self.role.as_deref() != Some("assistant") {
            return Err("Bedrock ConverseStream has no assistant role".to_string());
        }
        let mut content = Vec::with_capacity(self.blocks.len());
        for (expected, (index, block)) in self.blocks.into_iter().enumerate() {
            if index != expected {
                return Err(format!(
                    "Bedrock ConverseStream content indexes are not contiguous at {expected}"
                ));
            }
            if !self.stopped_blocks.contains(&index) {
                return Err(format!(
                    "Bedrock ConverseStream content index {index} never stopped"
                ));
            }
            let value = finish_stream_block(block, index)?;
            validate_content_block(
                &value,
                &config.response_content_types,
                &config.options.carrier_policy,
                &format!("stream response content[{index}]"),
            )
            .map_err(|failure| failure.detail)?;
            validate_tool_input_size(
                &value,
                config.options.account_limits.max_tool_input_bytes,
                &format!("stream response content[{index}]"),
            )?;
            content.push(value);
        }
        if content.is_empty() {
            return Err("Bedrock ConverseStream produced no content blocks".to_string());
        }
        if content.len() > config.options.account_limits.max_content_blocks {
            return Err(format!(
                "Bedrock ConverseStream returned {} content blocks, above descriptor limit {}",
                content.len(),
                config.options.account_limits.max_content_blocks
            ));
        }
        transport.event_count = self.event_types.values().sum();
        transport.event_types = self.event_types;
        Ok(ProviderResponse {
            model: config.model.clone(),
            stop_reason: self
                .stop_reason
                .ok_or_else(|| "Bedrock ConverseStream has no stop reason".to_string())?,
            content,
            usage: self
                .usage
                .ok_or_else(|| "Bedrock ConverseStream has no usage".to_string())?,
            metrics: self
                .metrics
                .ok_or_else(|| "Bedrock ConverseStream has no metrics".to_string())?,
            additional_fields: self.additional_fields,
            transport,
        })
    }
}

fn finish_stream_block(block: StreamBlock, index: usize) -> Result<Value, String> {
    match block {
        StreamBlock::Text(text) => {
            if text.is_empty() {
                return Err(format!("Bedrock stream text block {index} is empty"));
            }
            Ok(json!({"text": text}))
        }
        StreamBlock::ToolUse {
            id,
            name,
            input,
            server_type,
        } => {
            let input = if input.is_empty() {
                Value::Object(Map::new())
            } else {
                serde_json::from_str(&input).map_err(|error| {
                    format!("Bedrock stream tool input at index {index} is invalid JSON: {error}")
                })?
            };
            if !input.is_object() {
                return Err(format!(
                    "Bedrock stream tool input at index {index} is not an object"
                ));
            }
            let mut body = Map::new();
            body.insert("toolUseId".to_string(), Value::String(id));
            body.insert("name".to_string(), Value::String(name));
            body.insert("input".to_string(), input);
            if let Some(server_type) = server_type {
                body.insert("type".to_string(), Value::String(server_type));
            }
            Ok(json!({"toolUse": body}))
        }
        StreamBlock::Reasoning {
            text,
            signature,
            redacted,
        } => {
            if !redacted.is_empty() {
                if !text.is_empty() || !signature.is_empty() {
                    return Err(format!(
                        "Bedrock stream reasoning block {index} mixes redacted and text content"
                    ));
                }
                return Ok(json!({
                    "reasoningContent": {
                        "redactedContent": base64::engine::general_purpose::STANDARD.encode(redacted)
                    }
                }));
            }
            if text.is_empty() {
                return Err(format!(
                    "Bedrock stream reasoning block {index} has no text"
                ));
            }
            let mut reasoning = Map::new();
            reasoning.insert("text".to_string(), Value::String(text));
            if !signature.is_empty() {
                reasoning.insert("signature".to_string(), Value::String(signature));
            }
            Ok(json!({"reasoningContent": {"reasoningText": reasoning}}))
        }
        StreamBlock::Image { format, bytes } => {
            if bytes.is_empty() {
                return Err(format!("Bedrock stream image block {index} is empty"));
            }
            Ok(json!({
                "image": {
                    "format": format,
                    "source": {
                        "bytes": base64::engine::general_purpose::STANDARD.encode(bytes)
                    }
                }
            }))
        }
        StreamBlock::ToolResult {
            id,
            status,
            result_type,
            content,
        } => {
            if content.is_empty() {
                return Err(format!(
                    "Bedrock stream toolResult block {index} has no content"
                ));
            }
            let mut body = Map::new();
            body.insert("toolUseId".to_string(), Value::String(id));
            body.insert("content".to_string(), Value::Array(content));
            if let Some(status) = status {
                body.insert("status".to_string(), Value::String(status));
            }
            if let Some(result_type) = result_type {
                body.insert("type".to_string(), Value::String(result_type));
            }
            Ok(json!({"toolResult": body}))
        }
        StreamBlock::Citations { text, citations } => {
            if text.is_empty() && citations.is_empty() {
                return Err(format!(
                    "Bedrock stream citations block {index} has no content"
                ));
            }
            let content = if text.is_empty() {
                Vec::new()
            } else {
                vec![json!({"text": text})]
            };
            Ok(json!({
                "citationsContent": {
                    "content": content,
                    "citations": citations
                }
            }))
        }
    }
}

fn append_tool_result_delta(content: &mut Vec<Value>, value: &Value) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| "Bedrock toolResult delta item is not an object".to_string())?;
    if object.len() != 1 {
        return Err("Bedrock toolResult delta item must contain one union member".to_string());
    }
    let (kind, value) = object.iter().next().unwrap();
    match kind.as_str() {
        "text" => {
            let fragment = value
                .as_str()
                .ok_or_else(|| "Bedrock toolResult text delta is not a string".to_string())?;
            if let Some(last) = content.last_mut().and_then(Value::as_object_mut) {
                if let Some(Value::String(text)) = last.get_mut("text") {
                    text.push_str(fragment);
                    return Ok(());
                }
            }
            content.push(json!({"text": fragment}));
        }
        "json" => content.push(json!({"json": value})),
        other => {
            return Err(format!(
                "Bedrock toolResult delta member '{other}' is unsupported"
            ));
        }
    }
    Ok(())
}

fn stream_index(value: &Value, surface: &str) -> Result<usize, String> {
    value
        .get("contentBlockIndex")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| format!("Bedrock {surface}.contentBlockIndex is malformed"))
}

fn ensure_object_keys(value: &Value, allowed: &[&str], surface: &str) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("Bedrock {surface} is not an object"))?;
    ensure_map_keys(object, allowed, surface)
}

fn ensure_map_keys(
    object: &Map<String, Value>,
    allowed: &[&str],
    surface: &str,
) -> Result<(), String> {
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!(
            "Bedrock {surface} contains unsupported field '{key}'"
        ));
    }
    Ok(())
}

fn required_plain_string(value: &Value, key: &str, surface: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.trim() == *value)
        .map(str::to_string)
        .ok_or_else(|| format!("Bedrock {surface}.{key} is missing or malformed"))
}

fn required_map_plain_string(
    value: &Map<String, Value>,
    key: &str,
    surface: &str,
) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.trim() == *value)
        .map(str::to_string)
        .ok_or_else(|| format!("Bedrock {surface}.{key} is missing or malformed"))
}

fn required_map_string_allow_empty(
    value: &Map<String, Value>,
    key: &str,
    surface: &str,
) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("Bedrock {surface}.{key} is missing or malformed"))
}

fn optional_map_string(
    value: &Map<String, Value>,
    key: &str,
    surface: &str,
) -> Result<Option<String>, String> {
    match value.get(key) {
        None => Ok(None),
        Some(Value::String(value)) if !value.is_empty() && value.trim() == value => {
            Ok(Some(value.clone()))
        }
        Some(_) => Err(format!("Bedrock {surface}.{key} is malformed")),
    }
}

fn validate_usage(value: &Value) -> Result<(), String> {
    let usage = value
        .as_object()
        .ok_or_else(|| "Bedrock usage is not an object".to_string())?;
    for key in ["inputTokens", "outputTokens", "totalTokens"] {
        if usage.get(key).and_then(Value::as_u64).is_none() {
            return Err(format!("Bedrock usage.{key} is missing or malformed"));
        }
    }
    for key in ["cacheReadInputTokens", "cacheWriteInputTokens"] {
        if usage.get(key).is_some_and(|value| value.as_u64().is_none()) {
            return Err(format!("Bedrock usage.{key} is malformed"));
        }
    }
    if let Some(details) = usage.get("cacheDetails") {
        let details = details
            .as_array()
            .ok_or_else(|| "Bedrock usage.cacheDetails is not an array".to_string())?;
        for (index, detail) in details.iter().enumerate() {
            let detail = detail
                .as_object()
                .ok_or_else(|| format!("Bedrock usage.cacheDetails[{index}] is not an object"))?;
            ensure_map_keys(detail, &["inputTokens", "ttl"], "usage cacheDetails")?;
            if detail.get("inputTokens").and_then(Value::as_u64).is_none()
                || detail.get("ttl").and_then(Value::as_str).is_none()
            {
                return Err(format!("Bedrock usage.cacheDetails[{index}] is malformed"));
            }
        }
    }
    if let Some(key) = usage.keys().find(|key| {
        !matches!(
            key.as_str(),
            "inputTokens"
                | "outputTokens"
                | "totalTokens"
                | "cacheReadInputTokens"
                | "cacheWriteInputTokens"
                | "cacheDetails"
        )
    }) {
        return Err(format!("Bedrock usage contains unsupported field '{key}'"));
    }
    Ok(())
}

fn validate_metrics(value: &Value) -> Result<(), String> {
    let metrics = value
        .as_object()
        .ok_or_else(|| "Bedrock metrics is not an object".to_string())?;
    if metrics
        .get("latencyMs")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value >= 0.0)
        .is_none()
    {
        return Err("Bedrock metrics.latencyMs is missing or malformed".to_string());
    }
    Ok(())
}

fn validate_tool_input_size(block: &Value, maximum: usize, surface: &str) -> Result<(), String> {
    let Some(input) = block.pointer("/toolUse/input") else {
        return Ok(());
    };
    let bytes = serde_json::to_vec(input)
        .map_err(|error| format!("Bedrock {surface} tool input did not encode: {error}"))?;
    if bytes.len() > maximum {
        return Err(format!(
            "Bedrock {surface} tool input is {} bytes, above descriptor limit {maximum}",
            bytes.len()
        ));
    }
    Ok(())
}

fn translate_response(
    response: ProviderResponse,
    output_root: &Path,
    allowed_content_types: &BTreeSet<String>,
    visible_tool_names: &BTreeSet<String>,
) -> Result<Vec<ProviderEmission>, BedrockFailure> {
    let mut tool_calls = Vec::new();
    let mut text_parts = Vec::new();
    let mut reasoning_blocks = Vec::new();
    for (index, block) in response.content.iter().enumerate() {
        let object = block.as_object().ok_or_else(|| {
            BedrockFailure::contract(format!(
                "Bedrock response content[{index}] is not an object"
            ))
        })?;
        if object.len() != 1 {
            return Err(BedrockFailure::contract(format!(
                "Bedrock response content[{index}] must contain one union member"
            )));
        }
        let (kind, value) = object.iter().next().unwrap();
        if !allowed_content_types.contains(kind) {
            return Err(BedrockFailure::contract(format!(
                "Bedrock response content[{index}] type '{kind}' is not descriptor-admitted"
            )));
        }
        match kind.as_str() {
            "text" => text_parts.push(value.as_str().unwrap().to_string()),
            "reasoningContent" => reasoning_blocks.push(block.clone()),
            "toolUse" => {
                let tool = value.as_object().ok_or_else(|| {
                    BedrockFailure::contract(format!(
                        "Bedrock response toolUse[{index}] is not an object"
                    ))
                })?;
                if tool.get("type").and_then(Value::as_str) == Some("server_tool_use") {
                    continue;
                }
                let correlation_id = required_map_string(tool, "toolUseId", "response toolUse")?;
                let encoded_name = required_map_string(tool, "name", "response toolUse")?;
                let arguments = tool
                    .get("input")
                    .filter(|value| value.is_object())
                    .cloned()
                    .ok_or_else(|| {
                        BedrockFailure::contract(format!(
                            "Bedrock response toolUse[{index}].input is not an object"
                        ))
                    })?;
                let tool = decode_tool_name(&encoded_name)?;
                if !visible_tool_names.contains(&tool) {
                    return Err(BedrockFailure::contract(format!(
                        "Bedrock model called undeclared tool '{tool}'"
                    )));
                }
                tool_calls.push((correlation_id, tool, arguments));
            }
            "citationsContent" => {
                if let Some(content) = value.get("content").and_then(Value::as_array) {
                    for generated in content {
                        if let Some(text) = generated.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                text_parts.push(text.to_string());
                            }
                        }
                    }
                }
            }
            "image" | "toolResult" => {}
            other => {
                return Err(BedrockFailure::contract(format!(
                    "Bedrock response content type '{other}' has no output translation"
                )));
            }
        }
    }
    let provider_turn_id = response.transport.response_blake3.clone();
    let transport_payload = json!({
        "label": PROVIDER_TRANSPORT_LABEL,
        "model": response.model,
        "stop_reason": response.stop_reason,
        "operation": match response.transport.operation {
            BedrockOperation::Converse => "converse",
            BedrockOperation::ConverseStream => "converse_stream",
        },
        "status": response.transport.status,
        "request_bytes": response.transport.request_bytes,
        "request_blake3": response.transport.request_blake3,
        "response_bytes": response.transport.response_bytes,
        "response_blake3": response.transport.response_blake3,
        "event_count": response.transport.event_count,
        "event_types": response.transport.event_types,
        "metrics": response.metrics,
        "provider_fields": response.additional_fields,
    });
    let mut emissions = vec![materialize_emission(
        output_root,
        PROVIDER_TRANSPORT_LABEL,
        transport_payload,
        ProviderEmissionRoute::ClientProjection,
    )?];
    emissions.push(materialize_emission(
        output_root,
        PROVIDER_USAGE_LABEL,
        json!({
            "label": PROVIDER_USAGE_LABEL,
            "model": response.model,
            "usage": response.usage,
        }),
        ProviderEmissionRoute::ClientProjection,
    )?);
    if !reasoning_blocks.is_empty() {
        emissions.push(materialize_emission(
            output_root,
            PROVIDER_REASONING_LABEL,
            json!({
                "label": PROVIDER_REASONING_LABEL,
                "model": response.model,
                "blocks": reasoning_blocks,
            }),
            ProviderEmissionRoute::ClientProjection,
        )?);
    }
    emissions.push(materialize_emission(
        output_root,
        INTERFACE_OUTPUT_LABEL,
        json!({
            "label": INTERFACE_OUTPUT_LABEL,
            "provider_turn_id": provider_turn_id,
            "model": response.model,
            "stop_reason": response.stop_reason,
            "text": if text_parts.is_empty() {
                Value::Null
            } else {
                Value::String(text_parts.join("\n"))
            },
            "content_blocks": response.content,
        }),
        ProviderEmissionRoute::ClientProjection,
    )?);
    for (correlation_id, tool, arguments) in tool_calls {
        emissions.push(materialize_emission(
            output_root,
            TOOL_CALL_LABEL,
            json!({
                "label": TOOL_CALL_LABEL,
                "provider_turn_id": provider_turn_id,
                "tool": tool,
                "arguments": arguments,
                "correlation_id": correlation_id,
            }),
            ProviderEmissionRoute::Tool { tool },
        )?);
    }
    Ok(emissions)
}

fn materialize_pending_tool_results(
    output_root: &Path,
    groups: Vec<PendingToolGroup>,
) -> Result<Vec<ProviderEmission>, BedrockFailure> {
    let groups = groups
        .into_iter()
        .map(|group| {
            json!({
                "provider_turn_id": group.provider_turn_id,
                "expected": group.expected,
                "received": group.received,
                "pending_correlation_ids": group.pending_correlation_ids,
            })
        })
        .collect::<Vec<_>>();
    Ok(vec![materialize_emission(
        output_root,
        TOOL_RESULTS_PENDING_LABEL,
        json!({
            "label": TOOL_RESULTS_PENDING_LABEL,
            "groups": groups,
        }),
        ProviderEmissionRoute::ClientProjection,
    )?])
}

fn materialize_emission(
    output_root: &Path,
    label: &str,
    payload: Value,
    route: ProviderEmissionRoute,
) -> Result<ProviderEmission, BedrockFailure> {
    let bytes = serde_json::to_vec(&payload).map_err(|error| {
        BedrockFailure::contract(format!("Bedrock output did not encode: {error}"))
    })?;
    let artifact = InterfaceArtifactFile::write(output_root, label, &bytes)
        .map_err(|error| BedrockFailure::contract(error.to_string()))?;
    Ok(ProviderEmission { artifact, route })
}

pub fn materialize_provider_error(
    output_root: &Path,
    class: &str,
    detail: &str,
    attempts: Option<u32>,
    failure: Option<&BedrockFailure>,
) -> Result<InterfaceArtifactFile, String> {
    let class = class.trim();
    let detail = detail.trim();
    if class.is_empty() || detail.is_empty() {
        return Err("Bedrock provider error class and detail are required".to_string());
    }
    let payload = serde_json::to_vec(&json!({
        "label": PROVIDER_ERROR_LABEL,
        "class": class,
        "detail": detail,
        "attempts": attempts,
        "provider_failure": failure.map(|failure| json!({
            "class": failure.class,
            "retryable": failure.retryable,
            "status": failure.status,
            "code": failure.code,
            "detail": failure.detail,
            "response_bytes": failure.response_bytes,
            "response_blake3": failure.response_blake3,
            "provider_error": failure.provider_error,
        })),
    }))
    .map_err(|error| format!("Bedrock provider.error did not encode: {error}"))?;
    InterfaceArtifactFile::write(output_root, PROVIDER_ERROR_LABEL, &payload)
        .map_err(|error| error.to_string())
}

fn extract_catalog_models(
    kind: CatalogOperationKind,
    value: &Value,
) -> Result<Vec<Value>, BedrockFailure> {
    let (array_name, id_name, kind_name) = match kind {
        CatalogOperationKind::FoundationModels => ("modelSummaries", "modelId", "foundation_model"),
        CatalogOperationKind::InferenceProfiles => (
            "inferenceProfileSummaries",
            "inferenceProfileId",
            "inference_profile",
        ),
    };
    let values = value
        .get(array_name)
        .and_then(Value::as_array)
        .ok_or_else(|| {
            BedrockFailure::contract(format!(
                "Bedrock catalog response has no '{array_name}' array"
            ))
        })?;
    let mut seen = BTreeSet::new();
    let mut models = Vec::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        let object = value.as_object().ok_or_else(|| {
            BedrockFailure::contract(format!(
                "Bedrock catalog {array_name}[{index}] is not an object"
            ))
        })?;
        let id = object
            .get(id_name)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                BedrockFailure::contract(format!(
                    "Bedrock catalog {array_name}[{index}] has no '{id_name}'"
                ))
            })?;
        if !seen.insert(id.to_string()) {
            return Err(BedrockFailure::contract(format!(
                "Bedrock catalog repeats {kind_name} id '{id}'"
            )));
        }
        let mut normalized = object.clone();
        normalized.insert("id".to_string(), Value::String(id.to_string()));
        normalized.insert(
            "catalog_kind".to_string(),
            Value::String(kind_name.to_string()),
        );
        models.push(Value::Object(normalized));
    }
    Ok(models)
}

impl CatalogOperationKind {
    fn wire_name(self) -> &'static str {
        match self {
            Self::FoundationModels => "foundation_models",
            Self::InferenceProfiles => "inference_profiles",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::TokenSource;
    use octopus_descriptors::{ArtifactId, CheckpointId, EeId, RunId};
    use octopus_interfaces::{
        ArtifactBindingPayload, GrantProjection, InvocationMetadata, RuntimeInvocationMetadata,
    };
    use octopus_ledger::ContentHash;

    #[test]
    fn model_catalog_is_intersected_with_formed_policy() {
        let formed = BTreeSet::from(["model-a".to_string()]);
        assert!(catalog_model_allowed("model-a", &formed));
        assert!(!catalog_model_allowed("model-b", &formed));
    }

    fn descriptor() -> InterfaceDescriptor {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../interface.json");
        let bytes = std::fs::read(path).unwrap();
        octopus_descriptors::parse_strict(&bytes).unwrap()
    }

    fn exact_bedrock_descriptors() -> Vec<(PathBuf, InterfaceDescriptor)> {
        let interface_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let mut descriptors = std::fs::read_dir(interface_root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.join("interface.json").is_file())
            .filter_map(|path| {
                let bytes = std::fs::read(path.join("interface.json")).unwrap();
                let value: Value = serde_json::from_slice(&bytes).unwrap();
                (value.get("interface_driver_ref").and_then(Value::as_str)
                    == Some(INTERFACE_DRIVER_REF))
                .then(|| (path, octopus_descriptors::parse_strict(&bytes).unwrap()))
            })
            .collect::<Vec<_>>();
        descriptors.sort_by(|left, right| left.0.cmp(&right.0));
        descriptors
    }

    fn event_frame(event_type: &str, payload: Value) -> Vec<u8> {
        let payload = serde_json::to_vec(&payload).unwrap();
        let mut headers = Vec::new();
        for (name, value) in [
            (":message-type", "event"),
            (":event-type", event_type),
            (":content-type", "application/json"),
        ] {
            headers.push(name.len() as u8);
            headers.extend_from_slice(name.as_bytes());
            headers.push(7);
            headers.extend_from_slice(&(value.len() as u16).to_be_bytes());
            headers.extend_from_slice(value.as_bytes());
        }
        let total = 16 + headers.len() + payload.len();
        let mut frame = Vec::with_capacity(total);
        frame.extend_from_slice(&(total as u32).to_be_bytes());
        frame.extend_from_slice(&(headers.len() as u32).to_be_bytes());
        frame.extend_from_slice(&crc32fast::hash(&frame[..8]).to_be_bytes());
        frame.extend_from_slice(&headers);
        frame.extend_from_slice(&payload);
        let crc = crc32fast::hash(&frame);
        frame.extend_from_slice(&crc.to_be_bytes());
        frame
    }

    #[test]
    fn credential_refresh_retries_only_proven_aws_auth_rejections() {
        assert!(cached_credentials_rejected(StatusCode::UNAUTHORIZED, None));
        for code in [
            "ExpiredToken",
            "ExpiredTokenException",
            "InvalidClientTokenId",
            "UnrecognizedClientException",
        ] {
            assert!(cached_credentials_rejected(
                StatusCode::FORBIDDEN,
                Some(code)
            ));
        }
        for code in [
            None,
            Some("AccessDeniedException"),
            Some("InvalidSignatureException"),
            Some("SignatureDoesNotMatch"),
        ] {
            assert!(!cached_credentials_rejected(StatusCode::FORBIDDEN, code));
        }
        assert!(!cached_credentials_rejected(
            StatusCode::BAD_REQUEST,
            Some("ExpiredTokenException")
        ));
        let retry = RetryConfig {
            transport: true,
            auth_transport: true,
            base_delay_ms: 1,
            max_delay_ms: 1,
            retryable_statuses: vec![403, 503],
            retryable_error_codes: vec!["AccessDeniedException".to_string()],
        };
        assert!(!endpoint_failure_retryable(
            &retry,
            StatusCode::FORBIDDEN,
            Some("AccessDeniedException")
        ));
        assert!(endpoint_failure_retryable(
            &retry,
            StatusCode::SERVICE_UNAVAILABLE,
            None
        ));
    }

    #[test]
    fn exact_bedrock_interfaces_lift_from_own_descriptor_and_life() {
        let interfaces = exact_bedrock_descriptors();
        assert!(interfaces.len() >= 2);
        let secrets = tempfile::tempdir().unwrap();
        let mut saw_sigv4 = false;
        let mut saw_bearer = false;
        for (folder, descriptor) in interfaces {
            assert_eq!(
                folder.file_name().and_then(|name| name.to_str()),
                Some(descriptor.id.as_str())
            );
            assert!(serde_json::to_value(&descriptor)
                .unwrap()
                .get("reference")
                .is_none());
            let life_bytes = std::fs::read(folder.join("life.json")).unwrap();
            let life: octopus_interface_transport::SpawnLifeConfig =
                serde_json::from_slice(&life_bytes).unwrap();
            assert_eq!(life.processes.len(), 1);
            let process = &life.processes[0];
            assert!(process
                .coordinate_root_slots
                .contains("activation_secrets_home"));
            assert!(process.authority.allow_network);
            let driver =
                BedrockConverseDriver::from_descriptor(&descriptor, secrets.path().to_path_buf())
                    .unwrap();
            let process_env_names = driver.config.private_config["process_env_names"]
                .as_array()
                .unwrap();
            match &driver.config.options.authorization {
                AuthorizationConfig::Sigv4 { .. } => {
                    assert!(driver.authorizer.uses_cached_credentials());
                    saw_sigv4 = true;
                }
                AuthorizationConfig::Bearer {
                    token: TokenSource::SecretHandle { secret_ref },
                } => {
                    let prefix = format!("secret://{}/", descriptor.id);
                    assert!(secret_ref.starts_with(&prefix));
                    assert!(secret_ref.len() > prefix.len());
                    assert!(process_env_names.is_empty());
                    assert!(process.authority.optional_read_env_paths.is_empty());
                    assert!(!driver.authorizer.uses_cached_credentials());
                    saw_bearer = true;
                }
                AuthorizationConfig::Bearer { .. } => {
                    panic!("Bedrock bearer interface must use its exact-scoped secret handle")
                }
            }
        }
        assert!(saw_sigv4);
        assert!(saw_bearer);
    }

    fn input(value: Value) -> InterfaceInvocationEnvelope {
        let bytes = serde_json::to_vec(&value).unwrap();
        let label = value.get("label").and_then(Value::as_str).unwrap();
        let root = std::env::temp_dir().join(format!(
            "octopus-bedrock-fan-in-{}-{}",
            std::process::id(),
            blake3::hash(&bytes).to_hex()
        ));
        let artifact_file = InterfaceArtifactFile::write(&root, label, &bytes).unwrap();
        let role = json!({"label": CONTEXT_REFERENCE_LABEL, "text": "grounded role"});
        let role_bytes = serde_json::to_vec(&role).unwrap();
        InterfaceInvocationEnvelope {
            artifact: ArtifactRef {
                id: ArtifactId::new("input:test"),
                content: ContentHash::of_bytes(&bytes),
            },
            artifact_file,
            artifact_binding_payloads: vec![ArtifactBindingPayload::new(
                ArtifactRef {
                    id: ArtifactId::new("context.reference:test"),
                    content: ContentHash::of_bytes(&role_bytes),
                },
                role_bytes,
            )],
            metadata: Some(
                InvocationMetadata::new(GrantProjection::default()).with_runtime(
                    RuntimeInvocationMetadata {
                        run: RunId::new("run-test"),
                        ee: EeId::new("ee-test"),
                        worker: InterfaceId::new("system_worker_test"),
                        checkpoint: CheckpointId::new("checkpoint-test"),
                        interface: InterfaceId::new("world_provider_bedrock"),
                        epoch: 1,
                        run_record_hash: Some("hash".to_string()),
                    },
                ),
            ),
        }
    }

    fn bind_json(envelope: &mut InterfaceInvocationEnvelope, id: &str, value: Value) {
        let bytes = serde_json::to_vec(&value).unwrap();
        envelope
            .artifact_binding_payloads
            .push(ArtifactBindingPayload::new(
                ArtifactRef {
                    id: ArtifactId::new(id),
                    content: ContentHash::of_bytes(&bytes),
                },
                bytes,
            ));
    }

    fn session_item(id: &str, label: &str, payload: Value, seq: u64) -> Value {
        let bytes = serde_json::to_vec(&payload).unwrap();
        json!({
            "label": label,
            "payload": payload,
            "source_artifact": ArtifactRef {
                id: ArtifactId::new(id),
                content: ContentHash::of_bytes(&bytes),
            },
            "source_movement_seq": seq,
        })
    }

    #[test]
    fn missing_tool_input_schema_fails_closed() {
        let failure = translate_tool_specs(
            &[json!({"name": "artifact.read", "description": "read"})],
            8,
            4096,
        )
        .unwrap_err();
        assert_eq!(failure.class, "contract");
        assert!(failure.detail.contains("has no input_schema"));
    }

    #[test]
    fn cloned_interface_secret_scopes_must_be_rebased() {
        let mut cloned = descriptor();
        cloned.id = InterfaceId::new("world_provider_bedrock_clone");
        let metadata = &mut cloned
            .interface_driver_config
            .as_mut()
            .unwrap()
            .private_config;
        metadata.insert("process_env_names".to_string(), json!([]));
        *metadata
            .get_mut("bedrock_converse")
            .and_then(Value::as_object_mut)
            .unwrap()
            .get_mut("authorization")
            .unwrap() = json!({
            "scheme": "sigv4",
            "credentials": {
                "kind": "secret_handles",
                "access_key_ref": "secret://world_provider_bedrock/AWS_ACCESS_KEY_ID",
                "secret_key_ref": "secret://world_provider_bedrock/AWS_SECRET_ACCESS_KEY",
                "session_token_ref": "secret://world_provider_bedrock/AWS_SESSION_TOKEN"
            }
        });
        let secret_home = tempfile::tempdir().unwrap();
        let error =
            BedrockConverseDriver::from_descriptor(&cloned, secret_home.path().to_path_buf())
                .err()
                .unwrap();
        assert!(error.contains("exact interface scope"));
        *cloned
            .interface_driver_config
            .as_mut()
            .unwrap()
            .private_config
            .get_mut("bedrock_converse")
            .and_then(Value::as_object_mut)
            .unwrap()
            .get_mut("authorization")
            .unwrap() = json!({
            "scheme": "sigv4",
            "credentials": {
                "kind": "secret_handles",
                "access_key_ref": "secret://world_provider_bedrock_clone/AWS_ACCESS_KEY_ID",
                "secret_key_ref": "secret://world_provider_bedrock_clone/AWS_SECRET_ACCESS_KEY",
                "session_token_ref": "secret://world_provider_bedrock_clone/AWS_SESSION_TOKEN"
            }
        });
        BedrockConverseDriver::from_descriptor(&cloned, secret_home.path().to_path_buf()).unwrap();
    }

    #[test]
    fn session_snapshot_replays_prior_history_and_its_outer_artifact_once() {
        let secrets = tempfile::tempdir().unwrap();
        let driver =
            BedrockConverseDriver::from_descriptor(&descriptor(), secrets.path().to_path_buf())
                .unwrap();
        let prior = json!({"label": "interface.input", "prompt": "ONE"});
        let mut envelope = input(json!({"label": "interface.input", "prompt": "THREE"}));
        bind_json(
            &mut envelope,
            "interface.artifact.output:turn-2",
            json!({
                "label": INTERFACE_OUTPUT_LABEL,
                "provider_turn_id": "turn-2",
                "content_blocks": [{"text": "TWO"}],
                "session_context": {
                    "schema_version": 2,
                    "provider_interface": "world_provider_bedrock",
                    "artifacts": [
                        session_item("interface.input:one", "interface.input", prior, 10)
                    ]
                }
            }),
        );
        let assembled = assemble_turn(
            &envelope,
            &json!({"label": "interface.input", "prompt": "THREE"}),
            &driver.config,
            &[],
        )
        .unwrap();
        let TurnAssembly::Ready(turn) = assembled else {
            panic!("ordinary snapshot history must be ready");
        };
        assert_eq!(turn.messages.len(), 3);
        assert_eq!(turn.messages[0]["role"], "user");
        assert_eq!(turn.messages[0]["content"][0]["text"], "ONE");
        assert_eq!(turn.messages[1]["role"], "assistant");
        assert_eq!(turn.messages[1]["content"][0]["text"], "TWO");
        assert_eq!(turn.messages[2]["role"], "user");
        assert_eq!(turn.messages[2]["content"][0]["text"], "THREE");
        assert_eq!(
            turn.messages
                .iter()
                .filter(|message| message["role"] == "assistant")
                .count(),
            1
        );
    }

    #[test]
    fn stream_preserves_reasoning_signature_and_parallel_tool_correlation() {
        let secrets = tempfile::tempdir().unwrap();
        let driver =
            BedrockConverseDriver::from_descriptor(&descriptor(), secrets.path().to_path_buf())
                .unwrap();
        let mut bytes = Vec::new();
        for (event_type, payload) in [
            ("messageStart", json!({"role": "assistant"})),
            (
                "contentBlockDelta",
                json!({"contentBlockIndex": 0, "delta": {"reasoningContent": {"text": "plan"}}}),
            ),
            (
                "contentBlockDelta",
                json!({"contentBlockIndex": 0, "delta": {"reasoningContent": {"signature": "sig-1"}}}),
            ),
            ("contentBlockStop", json!({"contentBlockIndex": 0})),
            (
                "contentBlockStart",
                json!({"contentBlockIndex": 1, "start": {"toolUse": {"toolUseId": "tool-1", "name": "artifact-dread"}}}),
            ),
            (
                "contentBlockStart",
                json!({"contentBlockIndex": 2, "start": {"toolUse": {"toolUseId": "tool-2", "name": "artifact-dsearch"}}}),
            ),
            (
                "contentBlockDelta",
                json!({"contentBlockIndex": 1, "delta": {"toolUse": {"input": "{\"selector\":"}}}),
            ),
            (
                "contentBlockDelta",
                json!({"contentBlockIndex": 2, "delta": {"toolUse": {"input": "{\"query\":\"q\"}"}}}),
            ),
            (
                "contentBlockDelta",
                json!({"contentBlockIndex": 1, "delta": {"toolUse": {"input": "\"all\"}"}}}),
            ),
            ("contentBlockStop", json!({"contentBlockIndex": 2})),
            ("contentBlockStop", json!({"contentBlockIndex": 1})),
            (
                "messageStop",
                json!({"stopReason": "tool_use", "additionalModelResponseFields": {"provider": "kept"}}),
            ),
            (
                "metadata",
                json!({"usage": {"inputTokens": 11, "outputTokens": 7, "totalTokens": 18}, "metrics": {"latencyMs": 41}}),
            ),
        ] {
            bytes.extend(event_frame(event_type, payload));
        }
        let mut decoder = EventStreamFrameDecoder::new();
        let mut accepted = Vec::new();
        for chunk in bytes.chunks(11) {
            accepted.extend_from_slice(chunk);
            let evidence = ResponseBodyEvidence {
                bytes: accepted.len(),
                blake3: blake3::hash(&accepted).to_hex().to_string(),
            };
            decoder.push(chunk, bytes.len(), &evidence).unwrap();
        }
        let evidence = ResponseBodyEvidence {
            bytes: accepted.len(),
            blake3: blake3::hash(&accepted).to_hex().to_string(),
        };
        let response = decoder
            .finish(
                &driver.config,
                TransportEvidence {
                    operation: BedrockOperation::ConverseStream,
                    status: 200,
                    request_bytes: 3,
                    request_blake3: "request".to_string(),
                    response_bytes: evidence.bytes,
                    response_blake3: evidence.blake3.clone(),
                    event_count: 0,
                    event_types: BTreeMap::new(),
                },
                &evidence,
            )
            .unwrap();
        assert_eq!(response.content.len(), 3);
        assert_eq!(
            response.content[0]
                .pointer("/reasoningContent/reasoningText/signature")
                .and_then(Value::as_str),
            Some("sig-1")
        );
        assert_eq!(
            response.content[1]
                .pointer("/toolUse/input/selector")
                .and_then(Value::as_str),
            Some("all")
        );
        assert_eq!(
            response.content[2]
                .pointer("/toolUse/input/query")
                .and_then(Value::as_str),
            Some("q")
        );
        assert_eq!(response.transport.event_count, 13);
        assert_eq!(
            response
                .additional_fields
                .as_ref()
                .and_then(|value| value.pointer("/additionalModelResponseFields/provider"))
                .and_then(Value::as_str),
            Some("kept")
        );
    }

    #[test]
    fn parallel_tool_results_wait_and_replay_as_one_bedrock_user_turn() {
        let secrets = tempfile::tempdir().unwrap();
        let driver =
            BedrockConverseDriver::from_descriptor(&descriptor(), secrets.path().to_path_buf())
                .unwrap();
        let initial = json!({"label": "interface.input", "prompt": "inspect"});
        let output = json!({
            "label": INTERFACE_OUTPUT_LABEL,
            "provider_turn_id": "turn-1",
            "model": "test",
            "stop_reason": "tool_use",
            "text": "working",
            "content_blocks": [
                {"text": "working"},
                {"toolUse": {"toolUseId": "call-1", "name": "artifact-dread", "input": {"selector": "a"}}},
                {"toolUse": {"toolUseId": "call-2", "name": "artifact-dlist", "input": {}}}
            ]
        });
        let call_one = json!({
            "label": TOOL_CALL_LABEL,
            "provider_turn_id": "turn-1",
            "tool": "artifact.read",
            "arguments": {"selector": "a"},
            "correlation_id": "call-1"
        });
        let call_two = json!({
            "label": TOOL_CALL_LABEL,
            "provider_turn_id": "turn-1",
            "tool": "artifact.list",
            "arguments": {},
            "correlation_id": "call-2"
        });
        let result_one = json!({
            "label": "artifact.read.result",
            "tool": "artifact.read",
            "correlation_id": "call-1",
            "ok": true,
            "body": {"content": "first"}
        });
        let result_two = json!({
            "label": "artifact.list.result",
            "tool": "artifact.list",
            "correlation_id": "call-2",
            "ok": true,
            "body": {"content": "second"}
        });

        let mut partial = input(result_one.clone());
        bind_json(&mut partial, "input:initial", initial.clone());
        bind_json(&mut partial, "output:turn-1", output.clone());
        bind_json(&mut partial, "tool.call:call-1", call_one.clone());
        bind_json(&mut partial, "tool.call:call-2", call_two.clone());
        let pending = assemble_turn(&partial, &result_one, &driver.config, &[]).unwrap();
        let TurnAssembly::AwaitingToolResults(groups) = pending else {
            panic!("one of two Bedrock tool results must not invoke the model");
        };
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].expected, 2);
        assert_eq!(groups[0].received, 1);
        assert_eq!(groups[0].pending_correlation_ids, ["call-2"]);

        let snapshot = json!({
            "label": TOOL_RESULTS_PENDING_LABEL,
            "groups": [{
                "provider_turn_id": "turn-1",
                "expected": 2,
                "received": 1,
                "pending_correlation_ids": ["call-2"]
            }],
            "session_context": {
                "schema_version": 2,
                "provider_interface": "world_provider_bedrock",
                "artifacts": [
                    session_item("input:initial", "interface.input", initial, 10),
                    session_item("output:turn-1", INTERFACE_OUTPUT_LABEL, output, 11),
                    session_item("tool.call:call-1", TOOL_CALL_LABEL, call_one, 12),
                    session_item("tool.call:call-2", TOOL_CALL_LABEL, call_two, 13),
                    session_item("artifact.read.result:call-1", "artifact.read.result", result_one, 14)
                ]
            }
        });
        let mut complete = input(result_two.clone());
        bind_json(
            &mut complete,
            "provider.tool_results.pending:snapshot",
            snapshot,
        );
        let ready = assemble_turn(&complete, &result_two, &driver.config, &[]).unwrap();
        let TurnAssembly::Ready(ready) = ready else {
            panic!("the complete Bedrock tool-result group must invoke the model");
        };
        assert_eq!(ready.messages.len(), 3);
        assert_eq!(ready.messages[1]["role"], "assistant");
        assert_eq!(ready.messages[2]["role"], "user");
        assert_eq!(ready.messages[2]["content"].as_array().unwrap().len(), 2);
        assert_eq!(
            ready.messages[2]["content"][0]["toolResult"]["toolUseId"],
            "call-1"
        );
        assert_eq!(
            ready.messages[2]["content"][1]["toolResult"]["toolUseId"],
            "call-2"
        );
    }

    #[test]
    fn life_descriptor_grants_the_artifact_handoff_roots() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../life.json");
        let value: octopus_interface_transport::SpawnLifeConfig =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(value.processes.len(), 1);
        let process = &value.processes[0];
        process.authority.validate().unwrap();
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
        assert!(process.authority.allow_local_ipc);
    }
}
