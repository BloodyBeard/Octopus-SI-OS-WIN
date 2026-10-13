use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use octopus_descriptors::InterfaceId;
use octopus_interfaces::{
    ArtifactBindingPayload, InterfaceArtifactFile, InterfaceDescriptor,
    InterfaceInvocationEnvelope, InterfaceTransportConfig,
};
use octopus_ledger::ArtifactRef;
use octopus_model_context_render::{
    model_hidden_tool_names, model_tool_exclusions, render_model_context_block_with_hidden_tools,
};
use octopus_provider_run_authoring::{
    ProviderEmission, ProviderEmissionRoute, ProviderOutputHandoff,
};
use octopus_tool_handlers::ToolResultArtifact;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::dispatch::DispatchAdmission;

use crate::auth::{
    AuthSourceSelection, GeminiAuth, MetadataAccessTokenConfig, ServiceAccountJwtConfig,
    WorkloadIdentityFederationConfig,
};

pub const GEMINI_NATIVE_DRIVER_REF: &str = "gemini_native";

const INTERFACE_ARTIFACT_OUTPUT_LABEL: &str = "interface.artifact.output";
const TOOL_CALL_LABEL: &str = "tool.call";
const CONTEXT_REFERENCE_LABEL: &str = "context.reference";
const TURN_ERROR_LABEL: &str = "turn.error";
const PROVIDER_REASONING_LABEL: &str = "provider.reasoning";
const PROVIDER_USAGE_LABEL: &str = "provider.usage";
const SESSION_CONTEXT_FIELD: &str = "session_context";
const ABSOLUTE_MAX_RESPONSE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum GeminiOperation {
    GenerateContent,
    StreamGenerateContent,
}

impl GeminiOperation {
    fn wire(self) -> &'static str {
        match self {
            Self::GenerateContent => "gemini_generate_content",
            Self::StreamGenerateContent => "gemini_stream_generate_content",
        }
    }

    fn method(self) -> &'static str {
        match self {
            Self::GenerateContent => "generateContent",
            Self::StreamGenerateContent => "streamGenerateContent",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ValueSource {
    Descriptor,
    Environment,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum CarrierModality {
    Text,
    Image,
    Audio,
    Video,
    File,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CarrierPolicy {
    max_inline_bytes: u64,
    allowed_modalities: Vec<CarrierModality>,
    allowed_mime_types: Vec<String>,
    allowed_file_uri_schemes: Vec<String>,
    rules: Vec<CarrierRule>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum ModelCatalogConfig {
    Remote(RemoteModelCatalogConfig),
    Descriptor(DescriptorModelCatalogConfig),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteModelCatalogConfig {
    request_label: String,
    output_label: String,
    page_size: u32,
    max_pages: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DescriptorModelCatalogConfig {
    request_label: String,
    output_label: String,
    models: Vec<Value>,
}

impl ModelCatalogConfig {
    fn request_label(&self) -> &str {
        match self {
            Self::Remote(config) => &config.request_label,
            Self::Descriptor(config) => &config.request_label,
        }
    }

    fn output_label(&self) -> &str {
        match self {
            Self::Remote(config) => &config.output_label,
            Self::Descriptor(config) => &config.output_label,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FinishPolicy {
    successful_reasons: BTreeSet<String>,
    nonreplayable_reasons: BTreeSet<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryPolicyDescriptor {
    transport: bool,
    retryable_http_statuses: Vec<u16>,
    backoff_ms: Vec<u64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GeminiInvocationConfigDescriptor {
    model_hidden_tools: Vec<String>,
    process_env_names: Vec<String>,
    header_env: BTreeMap<String, String>,
    model: String,
    operation: GeminiOperation,
    base_url_source: ValueSource,
    #[serde(default)]
    base_url_allowed_host_suffixes: Vec<String>,
    auth_source: AuthSourceSelection,
    #[serde(default)]
    service_account_jwt: Option<ServiceAccountJwtConfig>,
    #[serde(default)]
    metadata_access_token: Option<MetadataAccessTokenConfig>,
    #[serde(default)]
    workload_identity_federation: Option<WorkloadIdentityFederationConfig>,
    generation_config: Map<String, Value>,
    safety_settings: Vec<Value>,
    tool_config: Option<Map<String, Value>>,
    finish_policy: FinishPolicy,
    retry: RetryPolicyDescriptor,
    model_catalog: Option<ModelCatalogConfig>,
    carrier_policy: CarrierPolicy,
    context_output_labels: Vec<String>,
    session_snapshot_labels: Vec<String>,
    #[serde(default, rename = "run_authoring_profile")]
    _run_authoring_profile: Option<Map<String, Value>>,
}

#[derive(Clone)]
struct RetryPolicy {
    transport: bool,
    retryable_http_statuses: BTreeSet<u16>,
    backoff_ms: Vec<u64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "encoding", rename_all = "snake_case", deny_unknown_fields)]
enum CarrierRule {
    JsonText {
        label: String,
        role: String,
        text_pointer: String,
    },
    GeminiContent {
        label: String,
    },
}

impl CarrierRule {
    fn label(&self) -> &str {
        match self {
            Self::JsonText { label, .. } | Self::GeminiContent { label } => label,
        }
    }
}

#[derive(Clone)]
struct GeminiConfig {
    provider_interface: InterfaceId,
    model: String,
    operation: GeminiOperation,
    generation_config: Map<String, Value>,
    safety_settings: Vec<Value>,
    tool_config: Option<Map<String, Value>>,
    model_catalog: Option<ModelCatalogConfig>,
    finish_policy: FinishPolicy,
    retry: RetryPolicy,
    carrier_policy: CarrierPolicy,
    session_snapshot_labels: BTreeSet<String>,
    private_config: Map<String, Value>,
}

pub struct GeminiNativeDriver {
    base_url: reqwest::Url,
    endpoint: reqwest::Url,
    headers: HeaderMap,
    auth: GeminiAuth,
    config: GeminiConfig,
    max_response_bytes: usize,
    http: reqwest::Client,
}

#[derive(Clone, Debug)]
pub(crate) struct GeminiFailure {
    pub class: &'static str,
    pub retryable: bool,
    pub status: Option<u16>,
    pub code: Option<String>,
    pub detail: String,
    pub response_bytes: Option<usize>,
    pub response_blake3: Option<String>,
}

impl GeminiFailure {
    pub(crate) fn contract(detail: impl Into<String>) -> Self {
        Self {
            class: "contract",
            retryable: false,
            status: None,
            code: None,
            detail: detail.into(),
            response_bytes: None,
            response_blake3: None,
        }
    }

    pub(crate) fn transport(detail: impl Into<String>, retryable: bool) -> Self {
        Self {
            class: "transport",
            retryable,
            status: None,
            code: None,
            detail: detail.into(),
            response_bytes: None,
            response_blake3: None,
        }
    }

    pub(crate) fn response_limit(detail: impl Into<String>) -> Self {
        Self {
            class: "response_limit",
            retryable: false,
            status: None,
            code: None,
            detail: detail.into(),
            response_bytes: None,
            response_blake3: None,
        }
    }

    fn endpoint(status: u16, bytes: &[u8], retryable: bool) -> Self {
        let code = serde_json::from_slice::<Value>(bytes)
            .ok()
            .and_then(|value| value.get("error").cloned())
            .and_then(|error| error.as_object().cloned())
            .and_then(|error| {
                error
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| {
                        error
                            .get("code")
                            .and_then(Value::as_i64)
                            .map(|v| v.to_string())
                    })
            });
        Self {
            class: "endpoint",
            retryable,
            status: Some(status),
            code,
            detail: format!("Gemini endpoint returned HTTP {status}"),
            response_bytes: Some(bytes.len()),
            response_blake3: Some(blake3::hash(bytes).to_hex().to_string()),
        }
    }

    fn response_contract(detail: impl Into<String>, bytes: &[u8]) -> Self {
        Self::response_contract_evidence(
            detail,
            bytes.len(),
            blake3::hash(bytes).to_hex().to_string(),
        )
    }

    fn response_contract_evidence(
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
        }
    }

    fn response_outcome(
        code: Option<String>,
        detail: impl Into<String>,
        response_bytes: usize,
        response_blake3: String,
        retryable: bool,
    ) -> Self {
        Self {
            class: "response_outcome",
            retryable,
            status: Some(200),
            code,
            detail: detail.into(),
            response_bytes: Some(response_bytes),
            response_blake3: Some(response_blake3),
        }
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

#[derive(Debug)]
struct ContextArtifact {
    id: String,
    label: String,
    value: Value,
    current: bool,
    source_artifact: ArtifactRef,
    source_movement_seq: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
struct ToolCallReplayIdentity {
    tool: String,
    provider_call_id: Option<String>,
    correlation_id: String,
    provider_part_index: usize,
    arguments: Value,
}

#[derive(Clone, Debug, PartialEq)]
struct FunctionCallGroup {
    content: Value,
    calls: Vec<ToolCallReplayIdentity>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct FunctionCallTranscript {
    groups: BTreeMap<String, FunctionCallGroup>,
    correlation_groups: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq)]
struct ToolResultRecord {
    correlation_id: String,
    tool: String,
    value: Value,
}

pub(crate) enum GeminiFanInPlan {
    Ready {
        envelope: InterfaceInvocationEnvelope,
        consumed: usize,
        source_movements: Vec<GeminiFanInSource>,
    },
    Pending {
        envelope: InterfaceInvocationEnvelope,
        accumulated: usize,
        content_group: String,
        expected: usize,
        received: usize,
        source_movements: Vec<GeminiFanInSource>,
    },
    Rejected {
        envelope: InterfaceInvocationEnvelope,
        consumed: usize,
        failure: GeminiFailure,
        source_movements: Vec<GeminiFanInSource>,
    },
}

#[derive(Clone)]
pub(crate) struct GeminiFanInEnvelope {
    pub envelope: InterfaceInvocationEnvelope,
    pub source_movement_seq: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GeminiFanInSource {
    pub artifact: ArtifactRef,
    pub source_movement_seq: Option<u64>,
}

struct GeminiResponseEnvelope {
    response: Value,
    response_bytes: usize,
    response_blake3: String,
    stream_event_count: Option<usize>,
}

struct ResponseBodyEvidence {
    bytes: usize,
    blake3: String,
}

struct GeminiStreamAccumulator {
    parts: Vec<Value>,
    function_identities: BTreeSet<String>,
    candidate_meta: Map<String, Value>,
    response_meta: Map<String, Value>,
    saw_candidate: bool,
    terminal_reason: Option<String>,
    event_count: usize,
}

struct GeminiSseDecoder {
    pending: Vec<u8>,
    accumulator: GeminiStreamAccumulator,
}

impl GeminiNativeDriver {
    pub fn from_descriptor(
        descriptor: &InterfaceDescriptor,
        activation_secrets_home: PathBuf,
    ) -> Result<Self, String> {
        if descriptor.interface_driver_ref != GEMINI_NATIVE_DRIVER_REF {
            return Err(format!(
                "provider interface '{}' declares driver '{}', expected '{}'",
                descriptor.id, descriptor.interface_driver_ref, GEMINI_NATIVE_DRIVER_REF
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
        let metadata = &driver_config.private_config;
        let config: GeminiInvocationConfigDescriptor =
            serde_json::from_value(Value::Object(metadata.clone())).map_err(|error| {
                format!(
                    "provider interface '{}' has malformed Gemini private_config: {error}",
                    descriptor.id
                )
            })?;
        let hidden_tools = strict_string_set(
            &config.model_hidden_tools,
            "Gemini private_config.model_hidden_tools",
        )?;
        let rendered_hidden_tools = model_hidden_tool_names(metadata)?
            .into_iter()
            .collect::<BTreeSet<_>>();
        if hidden_tools != rendered_hidden_tools {
            return Err(
                "Gemini model_hidden_tools changed while its typed descriptor was parsed"
                    .to_string(),
            );
        }
        let process_env_names = strict_string_set(
            &config.process_env_names,
            "Gemini private_config.process_env_names",
        )?;
        validate_string_map_values(&config.header_env, "Gemini private_config.header_env")?;
        let header_env = config.header_env.clone();
        let model = config.model.clone();
        validate_model(&model)?;
        let operation = config.operation;
        if transport.wire.as_deref() != Some(operation.wire()) {
            return Err(format!(
                "Gemini operation '{operation:?}' requires transport.wire='{}'",
                operation.wire()
            ));
        }
        let base_url_source = config.base_url_source;
        let base_url = resolve_base_url(transport, base_url_source)?;
        let base_url_allowed_host_suffixes = strict_string_set(
            &config.base_url_allowed_host_suffixes,
            "Gemini private_config.base_url_allowed_host_suffixes",
        )?;
        validate_base_url_host_gate(&base_url, base_url_source, &base_url_allowed_host_suffixes)?;
        let endpoint = build_endpoint(&base_url, &model, operation)?;
        let base_url = reqwest::Url::parse(&format!("{base_url}/"))
            .map_err(|error| format!("Gemini base URL is invalid after normalization: {error}"))?;
        let auth_source = config.auth_source;
        validate_process_env_names(
            transport,
            base_url_source,
            auth_source,
            &header_env,
            &process_env_names,
        )?;
        let auth = GeminiAuth::from_descriptor(
            transport,
            auth_source,
            activation_secrets_home,
            &descriptor.id,
            config.service_account_jwt,
            config.metadata_access_token,
            config.workload_identity_federation,
        )?;
        let headers = resolve_headers(transport, &auth.header, &header_env)?;
        let generation_config = config.generation_config;
        validate_generation_config(&generation_config)?;
        let safety_settings = config.safety_settings;
        validate_array_of_objects(&safety_settings, "Gemini private_config.safety_settings")?;
        let tool_config = config.tool_config;
        let finish_policy = config.finish_policy;
        validate_finish_policy(&finish_policy)?;
        let retry = RetryPolicy::from_descriptor(config.retry)?;
        let model_catalog = config.model_catalog;
        if let Some(config) = &model_catalog {
            validate_model_catalog(config)?;
        }
        let carrier_policy = config.carrier_policy;
        validate_carrier_policy(&carrier_policy)?;
        let context_output_labels = strict_string_set(
            &config.context_output_labels,
            "Gemini private_config.context_output_labels",
        )?;
        let session_snapshot_labels = strict_string_set(
            &config.session_snapshot_labels,
            "Gemini private_config.session_snapshot_labels",
        )?;
        if context_output_labels.is_empty() || session_snapshot_labels.is_empty() {
            return Err(
                "Gemini context_output_labels and session_snapshot_labels must not be empty"
                    .to_string(),
            );
        }
        if !session_snapshot_labels.is_subset(&context_output_labels) {
            return Err(
                "Gemini session_snapshot_labels must be a subset of context_output_labels"
                    .to_string(),
            );
        }
        let max_response_bytes = transport
            .max_response_bytes
            .ok_or_else(|| "Gemini transport.max_response_bytes is required".to_string())?;
        if max_response_bytes == 0 || max_response_bytes > ABSOLUTE_MAX_RESPONSE_BYTES {
            return Err(format!(
                "Gemini max_response_bytes must be within 1..={ABSOLUTE_MAX_RESPONSE_BYTES}"
            ));
        }
        let timeout_ms = transport
            .request_timeout_ms
            .ok_or_else(|| "Gemini transport.request_timeout_ms is required".to_string())?;
        if timeout_ms == 0 || timeout_ms > 30 * 60 * 1000 {
            return Err("Gemini request_timeout_ms must be within 1..=1800000".to_string());
        }
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(timeout_ms))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| format!("Gemini HTTP client build failed: {error}"))?;
        Ok(Self {
            base_url,
            endpoint,
            headers,
            auth,
            config: GeminiConfig {
                provider_interface: descriptor.id.clone(),
                model,
                operation,
                generation_config,
                safety_settings,
                tool_config,
                model_catalog,
                finish_policy,
                retry,
                carrier_policy,
                session_snapshot_labels,
                private_config: metadata.clone(),
            },
            max_response_bytes: max_response_bytes as usize,
            http,
        })
    }

    pub(crate) fn validate_max_attempts(&self, max_attempts: u32) -> Result<(), String> {
        if max_attempts == 0 {
            return Err("Gemini max_attempts must be positive".to_string());
        }
        let retries = max_attempts.saturating_sub(1) as usize;
        if retries > self.config.retry.backoff_ms.len() {
            return Err(format!(
                "Gemini max_attempts {max_attempts} requires {retries} retry delays, descriptor declares {}",
                self.config.retry.backoff_ms.len()
            ));
        }
        Ok(())
    }

    pub(crate) fn retry_delay(&self, completed_attempt: u32) -> Result<Duration, String> {
        let index = completed_attempt
            .checked_sub(1)
            .ok_or_else(|| "Gemini retry delay requires a completed attempt".to_string())?
            as usize;
        self.config
            .retry
            .backoff_ms
            .get(index)
            .copied()
            .map(Duration::from_millis)
            .ok_or_else(|| {
                format!("Gemini descriptor has no retry delay after attempt {completed_attempt}")
            })
    }

    pub(crate) fn enforce_formed_model_policy(
        &self,
        input: &InterfaceInvocationEnvelope,
    ) -> Result<(), GeminiFailure> {
        let manifest = input
            .read_artifact_manifest()
            .map_err(|error| GeminiFailure::contract(error.to_string()))?;
        if manifest.label == "provider.models.list" {
            return Ok(());
        }
        let models = required_formed_models(input)?;
        if !models.contains(&self.config.model) {
            return Err(GeminiFailure::contract(format!(
                "Gemini selected model '{}' is outside formed interface policy models",
                self.config.model
            )));
        }
        Ok(())
    }

    pub(crate) fn prepare_fan_in(
        &self,
        legs: &[GeminiFanInEnvelope],
    ) -> Result<GeminiFanInPlan, GeminiFailure> {
        let first = legs.first().ok_or_else(|| {
            GeminiFailure::contract("Gemini fan-in received no invocation envelopes")
        })?;
        let first_runtime = first
            .envelope
            .runtime()
            .map_err(|error| GeminiFailure::contract(error.to_string()))?;
        if first_runtime.interface != self.config.provider_interface {
            return Err(GeminiFailure::contract(format!(
                "Gemini invocation belongs to interface '{}' instead of '{}'",
                first_runtime.interface.as_str(),
                self.config.provider_interface.as_str()
            )));
        }
        let first_artifacts = context_artifacts(
            &first.envelope,
            &self.config.provider_interface,
            &self.config.session_snapshot_labels,
        )?;
        let current = current_context_artifact(&first_artifacts)?;
        if !is_tool_result(&current.label, &current.value) {
            return Ok(GeminiFanInPlan::Ready {
                envelope: first.envelope.clone(),
                consumed: 1,
                source_movements: Vec::new(),
            });
        }
        let transcript = function_call_transcript(&first_artifacts)?;
        let current_result = tool_result_record(current, &transcript)?;
        let content_group = transcript
            .correlation_groups
            .get(&current_result.correlation_id)
            .cloned()
            .ok_or_else(|| {
                GeminiFailure::contract(format!(
                    "tool result correlation_id '{}' has no declared Gemini response group",
                    current_result.correlation_id
                ))
            })?;
        let group = transcript.groups.get(&content_group).ok_or_else(|| {
            GeminiFailure::contract(format!(
                "Gemini response group '{content_group}' disappeared from its transcript"
            ))
        })?;
        let expected = group.calls.len();
        let mut results = tool_results(&first_artifacts, &transcript)?;
        validate_unresolved_function_groups(&transcript, &results, &content_group)?;
        let mut received = group
            .calls
            .iter()
            .filter(|call| results.contains_key(&call.correlation_id))
            .count();
        if received == expected {
            return Ok(GeminiFanInPlan::Ready {
                envelope: first.envelope.clone(),
                consumed: 1,
                source_movements: Vec::new(),
            });
        }
        for (index, leg) in legs.iter().enumerate().skip(1) {
            if let Err(failure) = validate_fan_in_coordinates(&first.envelope, &leg.envelope) {
                return rejected_fan_in(legs, index, failure);
            }
            let artifacts = match context_artifacts(
                &leg.envelope,
                &self.config.provider_interface,
                &self.config.session_snapshot_labels,
            ) {
                Ok(artifacts) => artifacts,
                Err(failure) => return rejected_fan_in(legs, index, failure),
            };
            let candidate_transcript = match function_call_transcript(&artifacts) {
                Ok(transcript) => transcript,
                Err(failure) => return rejected_fan_in(legs, index, failure),
            };
            if candidate_transcript.groups.get(&content_group) != Some(group) {
                return rejected_fan_in(
                    legs,
                    index,
                    GeminiFailure::contract(format!(
                        "Gemini response group '{content_group}' changed while parallel results were arriving"
                    )),
                );
            }
            let candidate_results = match tool_results(&artifacts, &candidate_transcript) {
                Ok(results) => results,
                Err(failure) => return rejected_fan_in(legs, index, failure),
            };
            if let Err(failure) = validate_unresolved_function_groups(
                &candidate_transcript,
                &candidate_results,
                &content_group,
            ) {
                return rejected_fan_in(legs, index, failure);
            }
            let candidate = match current_context_artifact(&artifacts) {
                Ok(candidate) => candidate,
                Err(failure) => return rejected_fan_in(legs, index, failure),
            };
            if !is_tool_result(&candidate.label, &candidate.value) {
                return rejected_fan_in(
                    legs,
                    index,
                    GeminiFailure::contract(format!(
                        "Gemini response group '{content_group}' was interrupted by artifact '{}' before all function results arrived",
                        candidate.label
                    )),
                );
            }
            let candidate_result = match tool_result_record(candidate, &candidate_transcript) {
                Ok(result) => result,
                Err(failure) => return rejected_fan_in(legs, index, failure),
            };
            let candidate_group = match candidate_transcript
                .correlation_groups
                .get(&candidate_result.correlation_id)
            {
                Some(group) => group,
                None => {
                    return rejected_fan_in(
                        legs,
                        index,
                        GeminiFailure::contract(format!(
                            "tool result correlation_id '{}' has no declared Gemini response group",
                            candidate_result.correlation_id
                        )),
                    );
                }
            };
            if candidate_group != &content_group {
                return rejected_fan_in(
                    legs,
                    index,
                    GeminiFailure::contract(format!(
                        "Gemini response group '{content_group}' was interrupted by result for group '{candidate_group}'"
                    )),
                );
            }
            if let Err(failure) = merge_tool_result(&mut results, candidate_result) {
                return rejected_fan_in(legs, index + 1, failure);
            }
            received = group
                .calls
                .iter()
                .filter(|call| results.contains_key(&call.correlation_id))
                .count();
            if received == expected {
                let (envelope, source_movements) = combine_fan_in_envelopes(legs, index + 1)?;
                return Ok(GeminiFanInPlan::Ready {
                    envelope,
                    consumed: index + 1,
                    source_movements,
                });
            }
        }
        let (envelope, source_movements) = combine_fan_in_envelopes(legs, legs.len())?;
        Ok(GeminiFanInPlan::Pending {
            envelope,
            accumulated: legs.len(),
            content_group,
            expected,
            received,
            source_movements,
        })
    }

    pub(crate) async fn receive_artifact_and_materialize_outputs(
        &self,
        input: InterfaceInvocationEnvelope,
        source_movement_seq: u64,
        attempt: u32,
        handoff: &ProviderOutputHandoff,
        output_root: &Path,
    ) -> Result<Vec<ProviderEmission>, GeminiFailure> {
        self.enforce_formed_model_policy(&input)?;
        let manifest = input
            .read_artifact_manifest()
            .map_err(|error| GeminiFailure::contract(error.to_string()))?;
        let credential_handle_fingerprint = self.auth.credential_handle_fingerprint();
        let dispatch = DispatchAdmission {
            handoff,
            output_root,
            provider_interface: &self.config.provider_interface,
            source_movement_seq,
            attempt,
            credential_handle_fingerprint: credential_handle_fingerprint.as_deref(),
        };
        if self
            .config
            .model_catalog
            .as_ref()
            .is_some_and(|config| manifest.label == config.request_label())
        {
            let payload = input
                .read_artifact_payload()
                .map_err(|error| GeminiFailure::contract(error.to_string()))?;
            let request: Value = serde_json::from_slice(&payload).map_err(|error| {
                GeminiFailure::contract(format!(
                    "provider.models.list payload is not JSON: {error}"
                ))
            })?;
            if !request.as_object().is_some_and(|object| object.is_empty()) {
                return Err(GeminiFailure::contract(
                    "provider.models.list payload must be an empty JSON object; catalog selection is descriptor-owned",
                ));
            }
            return self
                .materialize_model_catalog(output_root, &dispatch, &required_formed_models(&input)?)
                .await;
        }
        let selected = handoff
            .callable_tool_names(&input)
            .map_err(GeminiFailure::contract)?;
        let hidden = model_tool_exclusions(
            input
                .metadata()
                .map_err(|error| GeminiFailure::contract(error.to_string()))?,
            &selected,
            &model_hidden_tool_names(&self.config.private_config).map_err(GeminiFailure::contract)?,
        );
        let (request, model_visible_tool_names) =
            self.build_request_with_visible_tools(&input, &hidden)?;
        let response = self.call(request, &dispatch).await?;
        self.materialize_response(response, output_root, &model_visible_tool_names)
    }

    #[cfg(test)]
    fn build_request(&self, input: &InterfaceInvocationEnvelope) -> Result<Value, GeminiFailure> {
        let hidden = model_hidden_tool_names(&self.config.private_config)
            .map_err(GeminiFailure::contract)?;
        self.build_request_with_visible_tools(input, &hidden)
            .map(|(request, _)| request)
    }

    fn build_request_with_visible_tools(
        &self,
        input: &InterfaceInvocationEnvelope,
        hidden_tools: &[String],
    ) -> Result<(Value, BTreeSet<String>), GeminiFailure> {
        let (contents, system_instruction) = self.assemble_contents(input, hidden_tools)?;
        let (tools, model_visible_tool_names) = self.translate_tools(input, hidden_tools)?;
        let mut request = Map::new();
        request.insert("contents".to_string(), Value::Array(contents));
        request.insert(
            "systemInstruction".to_string(),
            json!({ "parts": [{ "text": system_instruction }] }),
        );
        if !tools.is_empty() {
            request.insert(
                "tools".to_string(),
                json!([{ "functionDeclarations": tools }]),
            );
            if let Some(tool_config) = &self.config.tool_config {
                request.insert("toolConfig".to_string(), Value::Object(tool_config.clone()));
            }
        }
        if !self.config.generation_config.is_empty() {
            request.insert(
                "generationConfig".to_string(),
                Value::Object(self.config.generation_config.clone()),
            );
        }
        if !self.config.safety_settings.is_empty() {
            request.insert(
                "safetySettings".to_string(),
                Value::Array(self.config.safety_settings.clone()),
            );
        }
        Ok((Value::Object(request), model_visible_tool_names))
    }

    fn assemble_contents(
        &self,
        input: &InterfaceInvocationEnvelope,
        hidden_tools: &[String],
    ) -> Result<(Vec<Value>, String), GeminiFailure> {
        let artifacts = context_artifacts(
            input,
            &self.config.provider_interface,
            &self.config.session_snapshot_labels,
        )?;
        let transcript = function_call_transcript(&artifacts)?;
        let results = tool_results(&artifacts, &transcript)?;
        let mut system_sections = Vec::new();
        let mut contents = Vec::new();
        let mut seen_model_groups = BTreeMap::<String, Value>::new();
        let mut emitted_result_groups = BTreeSet::new();
        let mut active_function_group: Option<String> = None;
        let mut closed_function_groups = BTreeSet::new();
        for artifact in artifacts {
            if artifact.label == CONTEXT_REFERENCE_LABEL {
                let text = artifact
                    .value
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        GeminiFailure::contract(format!(
                            "context.reference artifact '{}' has no non-empty text",
                            artifact.id
                        ))
                    })?;
                system_sections.push(text.to_string());
                continue;
            }
            if artifact.label == TOOL_CALL_LABEL
                || artifact.label == INTERFACE_ARTIFACT_OUTPUT_LABEL
            {
                let (group, content) = provider_model_content(&artifact.value)?;
                if transcript.groups.contains_key(&group) {
                    if closed_function_groups.contains(&group) {
                        return Err(GeminiFailure::contract(format!(
                            "Gemini response group '{group}' reappears after its functionResponse turn"
                        )));
                    }
                    if active_function_group
                        .as_ref()
                        .is_some_and(|active| active != &group)
                    {
                        return Err(GeminiFailure::contract(format!(
                            "Gemini response group '{group}' begins before group '{}' receives its ordered functionResponse turn",
                            active_function_group.as_deref().unwrap_or_default()
                        )));
                    }
                    active_function_group = Some(group.clone());
                }
                match seen_model_groups.get(&group) {
                    Some(previous) if previous != &content => {
                        return Err(GeminiFailure::contract(format!(
                            "provider content group '{group}' carries conflicting Gemini content"
                        )));
                    }
                    Some(_) => {}
                    None => {
                        seen_model_groups.insert(group, content.clone());
                        contents.push(content);
                    }
                }
                continue;
            }
            if is_tool_result(&artifact.label, &artifact.value) {
                let result = tool_result_record(&artifact, &transcript)?;
                let content_group = transcript
                    .correlation_groups
                    .get(&result.correlation_id)
                    .ok_or_else(|| {
                        GeminiFailure::contract(format!(
                            "tool result correlation_id '{}' has no declared Gemini response group",
                            result.correlation_id
                        ))
                    })?;
                if emitted_result_groups.insert(content_group.clone()) {
                    if active_function_group.as_deref() != Some(content_group.as_str()) {
                        return Err(GeminiFailure::contract(format!(
                            "Gemini functionResponse group '{content_group}' does not immediately follow its model functionCall content"
                        )));
                    }
                    let group = transcript.groups.get(content_group).ok_or_else(|| {
                        GeminiFailure::contract(format!(
                            "Gemini response group '{content_group}' disappeared from its transcript"
                        ))
                    })?;
                    contents.push(tool_result_group_content(group, &results)?);
                    active_function_group = None;
                    closed_function_groups.insert(content_group.clone());
                }
                continue;
            }
            if artifact.label == TURN_ERROR_LABEL {
                if let Some(group) = active_function_group.as_deref() {
                    return Err(GeminiFailure::contract(format!(
                        "artifact '{}' interrupts Gemini response group '{group}' before its functionResponse turn",
                        artifact.id
                    )));
                }
                contents.push(json!({
                    "role": "model",
                    "parts": [{ "text": render_json(&artifact.value)? }]
                }));
                continue;
            }
            let rule = self
                .config
                .carrier_policy
                .rules
                .iter()
                .find(|rule| rule.label() == artifact.label);
            if let Some(rule) = rule {
                if let Some(group) = active_function_group.as_deref() {
                    return Err(GeminiFailure::contract(format!(
                        "artifact '{}' interrupts Gemini response group '{group}' before its functionResponse turn",
                        artifact.id
                    )));
                }
                contents.push(decode_carrier(
                    rule,
                    &artifact.value,
                    &self.config.carrier_policy,
                )?);
                continue;
            }
            return Err(GeminiFailure::contract(format!(
                "artifact '{}' with label '{}' has no descriptor-declared Gemini carrier rule",
                artifact.id, artifact.label
            )));
        }
        if let Some(group) = active_function_group {
            return Err(GeminiFailure::contract(format!(
                "Gemini response group '{group}' has no ordered functionResponse turn"
            )));
        }
        if let Some(metadata) = input.metadata.as_ref().and_then(|metadata| {
            render_model_context_block_with_hidden_tools(metadata, hidden_tools)
        }) {
            system_sections.push(metadata);
        }
        if system_sections.is_empty() {
            return Err(GeminiFailure::contract(
                "Gemini provider context is missing bound context.reference instructions",
            ));
        }
        if contents.is_empty() {
            return Err(GeminiFailure::contract(
                "Gemini request contains no descriptor-admitted Content artifacts",
            ));
        }
        Ok((contents, system_sections.join("\n\n")))
    }

    fn translate_tools(
        &self,
        input: &InterfaceInvocationEnvelope,
        hidden_tools: &[String],
    ) -> Result<(Vec<Value>, BTreeSet<String>), GeminiFailure> {
        let hidden = hidden_tools.iter().cloned().collect::<BTreeSet<_>>();
        let granted = strict_string_set(
            &input
                .granted_tool_names()
                .map_err(|error| GeminiFailure::contract(error.to_string()))?,
            "Gemini formed grant tools",
        )
        .map_err(GeminiFailure::contract)?;
        let hard_blocked = strict_string_set(
            &input
                .hard_blocked_tool_names()
                .map_err(|error| GeminiFailure::contract(error.to_string()))?,
            "Gemini formed hard-blocked tools",
        )
        .map_err(GeminiFailure::contract)?;
        if let Some(tool) = granted.intersection(&hard_blocked).next() {
            return Err(GeminiFailure::contract(format!(
                "formed Gemini grant both grants and hard-blocks tool '{tool}'"
            )));
        }
        let expected_visible = granted
            .difference(&hidden)
            .cloned()
            .collect::<BTreeSet<_>>();
        let affordances = input
            .interface_affordance_specs()
            .map_err(|error| GeminiFailure::contract(error.to_string()))?;
        let mut declarations = Vec::with_capacity(affordances.len());
        let mut encoded_names = BTreeSet::new();
        let mut declared_names = BTreeSet::new();
        let mut model_visible_tool_names = BTreeSet::new();
        for (index, spec) in affordances.into_iter().enumerate() {
            let name = spec.name.as_str();
            if name.is_empty() || name.trim() != name {
                return Err(GeminiFailure::contract(format!(
                    "interface_affordance_specs[{index}] has no non-empty trimmed name"
                )));
            }
            if !declared_names.insert(name.to_string()) {
                return Err(GeminiFailure::contract(format!(
                    "interface affordance '{name}' is duplicated"
                )));
            }
            if !granted.contains(name) {
                return Err(GeminiFailure::contract(format!(
                    "interface affordance '{name}' is outside the formed tool grant"
                )));
            }
            if hidden.contains(name) {
                continue;
            }
            let encoded = encode_tool_name(name)?;
            model_visible_tool_names.insert(name.to_string());
            if !encoded_names.insert(encoded.clone()) {
                return Err(GeminiFailure::contract(format!(
                    "Gemini tool-name encoding collides at '{encoded}'"
                )));
            }
            if !spec.args_schema.is_object() {
                return Err(GeminiFailure::contract(format!(
                    "interface_affordance_specs[{index}] args_schema is not an object"
                )));
            }
            let schema = spec.args_schema;
            let mut declaration = Map::new();
            declaration.insert("name".to_string(), Value::String(encoded));
            if !spec.description.is_empty() {
                declaration.insert("description".to_string(), Value::String(spec.description));
            }
            declaration.insert("parametersJsonSchema".to_string(), schema);
            declarations.push(Value::Object(declaration));
        }
        if model_visible_tool_names != expected_visible {
            let missing = expected_visible
                .difference(&model_visible_tool_names)
                .cloned()
                .collect::<Vec<_>>();
            let unexpected = model_visible_tool_names
                .difference(&expected_visible)
                .cloned()
                .collect::<Vec<_>>();
            return Err(GeminiFailure::contract(format!(
                "Gemini model-visible affordances do not exactly match the formed tool grant: missing={missing:?}, unexpected={unexpected:?}"
            )));
        }
        Ok((declarations, model_visible_tool_names))
    }

    async fn call(
        &self,
        request: Value,
        dispatch: &DispatchAdmission<'_>,
    ) -> Result<GeminiResponseEnvelope, GeminiFailure> {
        let request_bytes = serde_json::to_vec(&request).map_err(|error| {
            GeminiFailure::contract(format!("Gemini request did not encode: {error}"))
        })?;
        let auth = self
            .auth
            .header_value(
                &self.http,
                self.config.retry.transport,
                &self.config.retry.retryable_http_statuses,
            )
            .await?;
        let mut req = self
            .http
            .post(self.endpoint.clone())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(request_bytes.clone());
        for (name, value) in &self.headers {
            req = req.header(name, value);
        }
        req = req.header(&self.auth.header, auth);
        if self.config.operation == GeminiOperation::StreamGenerateContent {
            req = req.header(reqwest::header::ACCEPT, "text/event-stream");
        } else {
            req = req.header(reqwest::header::ACCEPT, "application/json");
        }
        dispatch
            .admit("POST", "http", &self.endpoint, &request_bytes)
            .await
            .map_err(GeminiFailure::contract)?;
        let response = req.send().await.map_err(|error| {
            GeminiFailure::transport(
                format!("Gemini transport request failed: {error}"),
                self.config.retry.transport,
            )
        })?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !(200..300).contains(&status) {
            let bytes = read_bounded(
                response,
                self.max_response_bytes,
                self.config.retry.transport,
                false,
            )
            .await?;
            if status == 401 && self.auth.is_dynamic() {
                self.auth.invalidate_dynamic_token().await;
                return Err(GeminiFailure::endpoint(status, &bytes, true));
            }
            return Err(GeminiFailure::endpoint(
                status,
                &bytes,
                self.config.retry.retryable_http_statuses.contains(&status),
            ));
        }
        match self.config.operation {
            GeminiOperation::GenerateContent => {
                if !content_type.contains("json") {
                    return Err(GeminiFailure::response_contract_evidence(
                        format!(
                            "Gemini generateContent returned content-type '{content_type}', expected JSON"
                        ),
                        0,
                        blake3::hash(&[]).to_hex().to_string(),
                    ));
                }
                let bytes = read_bounded(
                    response,
                    self.max_response_bytes,
                    self.config.retry.transport,
                    false,
                )
                .await?;
                let response: Value = serde_json::from_slice(&bytes).map_err(|error| {
                    GeminiFailure::response_contract(
                        format!("Gemini generateContent response is not JSON: {error}"),
                        &bytes,
                    )
                })?;
                Ok(GeminiResponseEnvelope {
                    response,
                    response_bytes: bytes.len(),
                    response_blake3: blake3::hash(&bytes).to_hex().to_string(),
                    stream_event_count: None,
                })
            }
            GeminiOperation::StreamGenerateContent => {
                if !content_type.contains("text/event-stream") {
                    return Err(GeminiFailure::response_contract_evidence(
                        format!(
                            "Gemini streamGenerateContent returned content-type '{content_type}', expected text/event-stream"
                        ),
                        0,
                        blake3::hash(&[]).to_hex().to_string(),
                    ));
                }
                let (response, evidence, event_count) = read_gemini_sse(
                    response,
                    self.max_response_bytes,
                    self.config.retry.transport,
                )
                .await?;
                Ok(GeminiResponseEnvelope {
                    response,
                    response_bytes: evidence.bytes,
                    response_blake3: evidence.blake3,
                    stream_event_count: Some(event_count),
                })
            }
        }
    }

    async fn materialize_model_catalog(
        &self,
        output_root: &Path,
        dispatch: &DispatchAdmission<'_>,
        formed_models: &BTreeSet<String>,
    ) -> Result<Vec<ProviderEmission>, GeminiFailure> {
        let config = self.config.model_catalog.as_ref().ok_or_else(|| {
            GeminiFailure::contract("Gemini model catalog is not descriptor-enabled")
        })?;
        let config = match config {
            ModelCatalogConfig::Remote(config) => config,
            ModelCatalogConfig::Descriptor(config) => {
                return self.materialize_descriptor_model_catalog(
                    config,
                    output_root,
                    formed_models,
                );
            }
        };
        let mut models = Vec::new();
        let mut seen_names = BTreeSet::new();
        let mut pages = Vec::new();
        let mut page_token: Option<String> = None;
        for page_index in 0..config.max_pages {
            let mut url = self.base_url.join("models").map_err(|error| {
                GeminiFailure::contract(format!("Gemini models URL is invalid: {error}"))
            })?;
            {
                let mut query = url.query_pairs_mut();
                query.append_pair("pageSize", &config.page_size.to_string());
                if let Some(token) = &page_token {
                    query.append_pair("pageToken", token);
                }
            }
            let auth = self
                .auth
                .header_value(
                    &self.http,
                    self.config.retry.transport,
                    &self.config.retry.retryable_http_statuses,
                )
                .await?;
            let mut request = self.http.get(url.clone());
            for (name, value) in &self.headers {
                request = request.header(name, value);
            }
            request = request
                .header(&self.auth.header, auth)
                .header(reqwest::header::ACCEPT, "application/json");
            dispatch
                .admit("GET", "http", &url, &[])
                .await
                .map_err(GeminiFailure::contract)?;
            let response = request.send().await.map_err(|error| {
                GeminiFailure::transport(
                    format!("Gemini models transport request failed: {error}"),
                    self.config.retry.transport,
                )
            })?;
            let status = response.status().as_u16();
            let bytes = read_bounded(
                response,
                self.max_response_bytes,
                self.config.retry.transport,
                false,
            )
            .await?;
            if status == 401 && self.auth.is_dynamic() {
                self.auth.invalidate_dynamic_token().await;
                return Err(GeminiFailure::endpoint(status, &bytes, true));
            }
            if !(200..300).contains(&status) {
                return Err(GeminiFailure::endpoint(
                    status,
                    &bytes,
                    self.config.retry.retryable_http_statuses.contains(&status),
                ));
            }
            let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
                GeminiFailure::response_contract(
                    format!("Gemini models response is not JSON: {error}"),
                    &bytes,
                )
            })?;
            let object = value.as_object().ok_or_else(|| {
                GeminiFailure::response_contract("Gemini models response is not an object", &bytes)
            })?;
            let page_models = object
                .get("models")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    GeminiFailure::response_contract(
                        "Gemini models response has no models array",
                        &bytes,
                    )
                })?;
            for (model_index, model) in page_models.iter().enumerate() {
                let model_object = model.as_object().ok_or_else(|| {
                    GeminiFailure::response_contract(
                        format!(
                            "Gemini models page {page_index} item {model_index} is not an object"
                        ),
                        &bytes,
                    )
                })?;
                let name = model_object
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        GeminiFailure::response_contract(
                            format!(
                                "Gemini models page {page_index} item {model_index} has no name"
                            ),
                            &bytes,
                        )
                    })?;
                if !seen_names.insert(name.to_string()) {
                    return Err(GeminiFailure::response_contract(
                        format!("Gemini model catalog repeats '{name}'"),
                        &bytes,
                    ));
                }
                if gemini_catalog_model_allowed(name, formed_models) {
                    models.push(model.clone());
                }
            }
            pages.push(json!({
                "index": page_index,
                "status": status,
                "body": {
                    "bytes": bytes.len(),
                    "blake3": blake3::hash(&bytes).to_hex().to_string()
                }
            }));
            page_token = match object.get("nextPageToken") {
                None | Some(Value::Null) => None,
                Some(Value::String(token)) if !token.trim().is_empty() => Some(token.clone()),
                Some(Value::String(_)) => {
                    return Err(GeminiFailure::response_contract(
                        "Gemini models response carries an empty nextPageToken",
                        &bytes,
                    ));
                }
                Some(_) => {
                    return Err(GeminiFailure::response_contract(
                        "Gemini models nextPageToken is not a string",
                        &bytes,
                    ));
                }
            };
            if page_token.is_none() {
                let payload = json!({
                    "label": config.output_label,
                    "models": models,
                    "provider_evidence": {
                        "wire": "gemini_list_models",
                        "pages": pages,
                    }
                });
                return Ok(vec![materialize_emission(
                    output_root,
                    &config.output_label,
                    payload,
                    ProviderEmissionRoute::ClientProjection,
                )?]);
            }
        }
        Err(GeminiFailure::contract(format!(
            "Gemini model catalog exceeded descriptor max_pages {}",
            config.max_pages
        )))
    }

    fn materialize_descriptor_model_catalog(
        &self,
        config: &DescriptorModelCatalogConfig,
        output_root: &Path,
        formed_models: &BTreeSet<String>,
    ) -> Result<Vec<ProviderEmission>, GeminiFailure> {
        let models = config
            .models
            .iter()
            .filter(|model| {
                model
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| gemini_catalog_model_allowed(name, formed_models))
            })
            .cloned()
            .collect::<Vec<_>>();
        let catalog_bytes = serde_json::to_vec(&models).map_err(|error| {
            GeminiFailure::contract(format!(
                "Gemini descriptor model catalog did not encode: {error}"
            ))
        })?;
        let payload = json!({
            "label": config.output_label.clone(),
            "models": models,
            "provider_evidence": {
                "source": "interface_descriptor",
                "provider_interface": self.config.provider_interface.clone(),
                "catalog": {
                    "bytes": catalog_bytes.len(),
                    "blake3": blake3::hash(&catalog_bytes).to_hex().to_string()
                }
            }
        });
        Ok(vec![materialize_emission(
            output_root,
            &config.output_label,
            payload,
            ProviderEmissionRoute::ClientProjection,
        )?])
    }

    fn materialize_response(
        &self,
        envelope: GeminiResponseEnvelope,
        output_root: &Path,
        model_visible_tool_names: &BTreeSet<String>,
    ) -> Result<Vec<ProviderEmission>, GeminiFailure> {
        let root = envelope
            .response
            .as_object()
            .ok_or_else(|| GeminiFailure::contract("Gemini response root is not an object"))?;
        let usage = match root.get("usageMetadata") {
            None | Some(Value::Null) => None,
            Some(Value::Object(value)) => Some(value.clone()),
            Some(_) => {
                return Err(GeminiFailure::contract(
                    "Gemini response usageMetadata is not an object or null",
                ));
            }
        };
        let usage_value = usage.clone().map(Value::Object).unwrap_or(Value::Null);
        let candidates = match root.get("candidates") {
            Some(Value::Array(value)) => value,
            None | Some(Value::Null) => {
                let block_reason = root
                    .get("promptFeedback")
                    .and_then(Value::as_object)
                    .and_then(|feedback| feedback.get("blockReason"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                return Err(GeminiFailure::response_outcome(
                    block_reason.clone(),
                    match block_reason {
                        Some(reason) => format!("Gemini prompt was blocked with reason '{reason}'"),
                        None => "Gemini response contains no candidates".to_string(),
                    },
                    envelope.response_bytes,
                    envelope.response_blake3,
                    false,
                ));
            }
            Some(_) => {
                return Err(GeminiFailure::contract(
                    "Gemini response candidates is not an array or null",
                ));
            }
        };
        if candidates.is_empty() {
            let block_reason = root
                .get("promptFeedback")
                .and_then(Value::as_object)
                .and_then(|feedback| feedback.get("blockReason"))
                .and_then(Value::as_str)
                .map(str::to_string);
            return Err(GeminiFailure::response_outcome(
                block_reason.clone(),
                match block_reason {
                    Some(reason) => format!("Gemini prompt was blocked with reason '{reason}'"),
                    None => "Gemini response contains no candidates".to_string(),
                },
                envelope.response_bytes,
                envelope.response_blake3,
                false,
            ));
        }
        if candidates.len() != 1 {
            return Err(GeminiFailure::contract(format!(
                "Gemini response must carry exactly one candidate, received {}",
                candidates.len()
            )));
        }
        let candidate = candidates[0]
            .as_object()
            .cloned()
            .ok_or_else(|| GeminiFailure::contract("Gemini candidate is not an object"))?;
        let finish_reason = candidate
            .get("finishReason")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.trim() == *value)
            .ok_or_else(|| {
                GeminiFailure::contract("Gemini candidate has no terminal finishReason")
            })?;
        let complete = if self
            .config
            .finish_policy
            .successful_reasons
            .contains(finish_reason)
        {
            true
        } else if self
            .config
            .finish_policy
            .nonreplayable_reasons
            .contains(finish_reason)
        {
            false
        } else {
            return Err(GeminiFailure::response_outcome(
                Some(finish_reason.to_string()),
                format!("Gemini response finished as '{finish_reason}'"),
                envelope.response_bytes,
                envelope.response_blake3,
                false,
            ));
        };
        let content = candidate
            .get("content")
            .and_then(Value::as_object)
            .cloned()
            .ok_or_else(|| GeminiFailure::contract("Gemini candidate has no content object"))?;
        if content.get("role").and_then(Value::as_str) != Some("model") {
            return Err(GeminiFailure::contract(
                "Gemini candidate content.role must be 'model'",
            ));
        }
        let parts = content
            .get("parts")
            .and_then(Value::as_array)
            .ok_or_else(|| GeminiFailure::contract("Gemini content.parts is not an array"))?;
        if parts.is_empty() {
            let finish_reason = candidate
                .get("finishReason")
                .and_then(Value::as_str)
                .map(str::to_string);
            return Err(GeminiFailure::response_outcome(
                finish_reason.clone(),
                match finish_reason {
                    Some(reason) => format!(
                        "Gemini response contains no content parts and finished as '{reason}'"
                    ),
                    None => "Gemini content.parts is empty".to_string(),
                },
                envelope.response_bytes,
                envelope.response_blake3,
                false,
            ));
        }
        let content_group = root
            .get("responseId")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| envelope.response_blake3.clone());
        let provider_evidence = json!({
            "wire": self.config.operation.wire(),
            "model": self.config.model,
            "content_group": content_group,
            "finish_reason": finish_reason,
            "complete": complete,
            "nonreplayable_terminal": !complete,
            "candidate": Value::Object(candidate.clone()),
            "usage_metadata": usage_value.clone(),
            "prompt_feedback": root.get("promptFeedback").cloned().unwrap_or(Value::Null),
            "model_version": root.get("modelVersion").cloned().unwrap_or(Value::Null),
            "response_id": root.get("responseId").cloned().unwrap_or(Value::Null),
            "response_body": {
                "bytes": envelope.response_bytes,
                "blake3": envelope.response_blake3,
            },
            "stream": envelope.stream_event_count.map(|count| json!({ "event_count": count })).unwrap_or(Value::Null),
        });
        let mut text = String::new();
        let mut media = Vec::new();
        let mut reasoning = Vec::new();
        let mut function_calls = Vec::new();
        for (part_index, part) in parts.iter().enumerate() {
            let object = part.as_object().ok_or_else(|| {
                GeminiFailure::contract(format!(
                    "Gemini content.parts[{part_index}] is not an object"
                ))
            })?;
            validate_part_union(object, true)?;
            if object.get("thought").and_then(Value::as_bool) == Some(true) {
                reasoning.push(part.clone());
                continue;
            }
            if let Some(value) = object.get("text").and_then(Value::as_str) {
                text.push_str(value);
                if object.contains_key("thoughtSignature") {
                    reasoning.push(part.clone());
                }
                continue;
            }
            if object.contains_key("inlineData") || object.contains_key("fileData") {
                validate_part_carrier(object, &self.config.carrier_policy)?;
                media.push(part.clone());
                continue;
            }
            if let Some(call) = object.get("functionCall") {
                function_calls.push((part_index, call.clone()));
                continue;
            }
            return Err(GeminiFailure::contract(format!(
                "Gemini content.parts[{part_index}] uses an output variant this interface did not declare"
            )));
        }
        let mut outward = Vec::new();
        if !text.is_empty() || !media.is_empty() || !complete {
            let payload = json!({
                "text": if text.is_empty() { Value::Null } else { Value::String(text) },
                "media": media,
                "reasoning": reasoning,
                "complete": complete,
                "nonreplayable_terminal": !complete,
                "suppressed_function_calls": if complete {
                    Value::Null
                } else {
                    Value::Array(function_calls.iter().map(|(_, call)| call.clone()).collect())
                },
                "provider_evidence": provider_evidence.clone(),
            });
            outward.push(materialize_emission(
                output_root,
                INTERFACE_ARTIFACT_OUTPUT_LABEL,
                payload,
                ProviderEmissionRoute::ClientProjection,
            )?);
        }
        let mut correlation_ids = BTreeSet::new();
        for (part_index, call) in function_calls.into_iter().filter(|_| complete) {
            let call = call.as_object().ok_or_else(|| {
                GeminiFailure::contract(format!(
                    "Gemini functionCall at part {part_index} is not an object"
                ))
            })?;
            let encoded_name = call
                .get("name")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty() && value.trim() == *value)
                .ok_or_else(|| {
                    GeminiFailure::contract(format!(
                        "Gemini functionCall at part {part_index} has no valid name"
                    ))
                })?;
            let tool = decode_tool_name(encoded_name)?;
            if !model_visible_tool_names.contains(&tool) {
                return Err(GeminiFailure::contract(format!(
                    "Gemini returned tool '{tool}' outside the model-visible affordance set"
                )));
            }
            let arguments = match call.get("args") {
                None => json!({}),
                Some(Value::Object(value)) => Value::Object(value.clone()),
                Some(_) => {
                    return Err(GeminiFailure::contract(format!(
                        "Gemini functionCall '{encoded_name}' args is not an object"
                    )));
                }
            };
            let provider_call_id = match call.get("id") {
                None => None,
                Some(Value::String(value)) if !value.trim().is_empty() && value.trim() == value => {
                    Some(value.clone())
                }
                Some(_) => {
                    return Err(GeminiFailure::contract(format!(
                        "Gemini functionCall '{encoded_name}' id must be a non-empty trimmed string when present"
                    )));
                }
            };
            let correlation_id = provider_call_id.clone().unwrap_or_else(|| {
                format!("gemini_{}_{}", &envelope.response_blake3[..24], part_index)
            });
            if !correlation_ids.insert(correlation_id.clone()) {
                return Err(GeminiFailure::contract(format!(
                    "Gemini response repeats function call correlation_id '{correlation_id}'"
                )));
            }
            let payload = json!({
                "tool": tool,
                "arguments": arguments,
                "correlation_id": correlation_id,
                "provider_call_id": provider_call_id,
                "provider_part_index": part_index,
                "provider_evidence": provider_evidence.clone(),
            });
            outward.push(materialize_emission(
                output_root,
                TOOL_CALL_LABEL,
                payload,
                ProviderEmissionRoute::Tool { tool },
            )?);
        }
        if outward.is_empty() {
            let finish_reason = candidate
                .get("finishReason")
                .and_then(Value::as_str)
                .map(str::to_string);
            return Err(GeminiFailure::response_outcome(
                finish_reason.clone(),
                match finish_reason {
                    Some(reason) => format!(
                        "Gemini response produced no outward artifact and finished as '{reason}'"
                    ),
                    None => "Gemini response contains only reasoning and no outward artifact"
                        .to_string(),
                },
                envelope.response_bytes,
                envelope.response_blake3,
                false,
            ));
        }
        let mut emissions = Vec::with_capacity(outward.len() + 2);
        if !reasoning.is_empty() {
            emissions.push(materialize_emission(
                output_root,
                PROVIDER_REASONING_LABEL,
                json!({
                    "label": PROVIDER_REASONING_LABEL,
                    "parts": reasoning,
                    "provider_evidence": provider_evidence.clone(),
                }),
                ProviderEmissionRoute::ClientProjection,
            )?);
        }
        emissions.push(materialize_emission(
            output_root,
            PROVIDER_USAGE_LABEL,
            json!({
                "label": PROVIDER_USAGE_LABEL,
                "usage_metadata": usage_value,
                "model_version": root.get("modelVersion").cloned().unwrap_or(Value::Null),
                "response_id": root.get("responseId").cloned().unwrap_or(Value::Null),
                "provider_evidence": {
                    "wire": self.config.operation.wire(),
                    "finish_reason": finish_reason,
                    "complete": complete,
                    "nonreplayable_terminal": !complete,
                    "response_body": provider_evidence.get("response_body").cloned().unwrap_or(Value::Null),
                    "stream": provider_evidence.get("stream").cloned().unwrap_or(Value::Null),
                }
            }),
            ProviderEmissionRoute::ClientProjection,
        )?);
        emissions.extend(outward);
        Ok(emissions)
    }
}

pub(crate) fn materialize_turn_error(
    output_root: &Path,
    failure: &GeminiFailure,
    attempts: u32,
    source_movement_seq: u64,
) -> Result<InterfaceArtifactFile, String> {
    let payload = serde_json::to_vec(&json!({
        "label": TURN_ERROR_LABEL,
        "error": {
            "class": failure.class,
            "retryable": failure.retryable,
            "status": failure.status,
            "code": failure.code,
            "detail": failure.detail,
            "response_body": match (&failure.response_bytes, &failure.response_blake3) {
                (Some(bytes), Some(blake3)) => json!({ "bytes": bytes, "blake3": blake3 }),
                _ => Value::Null,
            },
        },
        "attempts": attempts,
        "source_movement_seq": source_movement_seq,
    }))
    .map_err(|error| format!("Gemini turn.error payload did not encode: {error}"))?;
    InterfaceArtifactFile::write(output_root, TURN_ERROR_LABEL, &payload)
        .map_err(|error| error.to_string())
}

fn required_formed_models(
    input: &InterfaceInvocationEnvelope,
) -> Result<BTreeSet<String>, GeminiFailure> {
    let policy = input
        .interface_policy()
        .map_err(|error| GeminiFailure::contract(error.to_string()))?
        .ok_or_else(|| {
            GeminiFailure::contract("Gemini invocation carries no formed interface policy")
        })?;
    let values = policy
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            GeminiFailure::contract("Gemini formed interface policy models must be a string array")
        })?;
    if values.is_empty() {
        return Err(GeminiFailure::contract(
            "Gemini formed interface policy models is empty",
        ));
    }
    let mut models = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let model = value.as_str().filter(|model| {
            !model.is_empty() && model.trim() == *model && !model.contains(['\r', '\n', '\0'])
        });
        let Some(model) = model else {
            return Err(GeminiFailure::contract(format!(
                "Gemini formed interface policy models[{index}] is not a trimmed string"
            )));
        };
        if !models.insert(model.to_string()) {
            return Err(GeminiFailure::contract(format!(
                "Gemini formed interface policy models repeats '{model}'"
            )));
        }
    }
    Ok(models)
}

fn gemini_catalog_model_allowed(name: &str, formed_models: &BTreeSet<String>) -> bool {
    formed_models.contains(name)
        || name
            .strip_prefix("models/")
            .is_some_and(|name| formed_models.contains(name))
}

fn strict_string_set(values: &[String], surface: &str) -> Result<BTreeSet<String>, String> {
    let mut output = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        if value.is_empty() || value.trim() != value {
            return Err(format!(
                "{surface}[{index}] must be a non-empty trimmed string"
            ));
        }
        if !output.insert(value.to_string()) {
            return Err(format!("{surface} repeats '{value}'"));
        }
    }
    Ok(output)
}

fn validate_string_map_values(
    values: &BTreeMap<String, String>,
    surface: &str,
) -> Result<(), String> {
    for (name, value) in values {
        if name.is_empty() || name.trim() != name || value.is_empty() || value.trim() != value {
            return Err(format!(
                "{surface}.{name} must map a trimmed header name to a non-empty trimmed environment name"
            ));
        }
    }
    Ok(())
}

fn validate_array_of_objects(values: &[Value], surface: &str) -> Result<(), String> {
    for (index, value) in values.iter().enumerate() {
        if !value.is_object() {
            return Err(format!("{surface}[{index}] must be an object"));
        }
    }
    Ok(())
}

fn validate_generation_config(config: &Map<String, Value>) -> Result<(), String> {
    if config.get("candidateCount").and_then(Value::as_u64) != Some(1) {
        return Err(
            "Gemini generation_config.candidateCount must be exactly 1 so one model decision owns the artifact edge"
                .to_string(),
        );
    }
    let max_output_tokens = config
        .get("maxOutputTokens")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0 && *value <= u32::MAX as u64)
        .ok_or_else(|| {
            "Gemini generation_config.maxOutputTokens must be an integer within 1..=4294967295"
                .to_string()
        })?;
    if config.get("maxOutputTokens") != Some(&Value::from(max_output_tokens)) {
        return Err(
            "Gemini generation_config.maxOutputTokens must use an unsigned integer JSON value"
                .to_string(),
        );
    }
    Ok(())
}

impl RetryPolicy {
    fn from_descriptor(descriptor: RetryPolicyDescriptor) -> Result<Self, String> {
        let mut retryable_http_statuses = BTreeSet::new();
        for (index, status) in descriptor.retryable_http_statuses.into_iter().enumerate() {
            if !(400..=599).contains(&status) {
                return Err(format!(
                    "Gemini retry.retryable_http_statuses[{index}] must be within 400..=599"
                ));
            }
            if !retryable_http_statuses.insert(status) {
                return Err(format!(
                    "Gemini retry.retryable_http_statuses repeats {status}"
                ));
            }
        }
        if descriptor.backoff_ms.is_empty() || descriptor.backoff_ms.len() > 32 {
            return Err("Gemini retry.backoff_ms must contain 1..=32 delays".to_string());
        }
        let mut previous = 0_u64;
        for (index, delay) in descriptor.backoff_ms.iter().copied().enumerate() {
            if delay == 0 || delay > 3_600_000 {
                return Err(format!(
                    "Gemini retry.backoff_ms[{index}] must be within 1..=3600000"
                ));
            }
            if delay < previous {
                return Err("Gemini retry.backoff_ms must be non-decreasing".to_string());
            }
            previous = delay;
        }
        Ok(Self {
            transport: descriptor.transport,
            retryable_http_statuses,
            backoff_ms: descriptor.backoff_ms,
        })
    }
}

fn validate_model(model: &str) -> Result<(), String> {
    if model.len() > 256
        || !model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(
            "Gemini model must contain only ASCII letters, digits, '.', '_' or '-' and be at most 256 bytes"
                .to_string(),
        );
    }
    Ok(())
}

fn resolve_base_url(
    transport: &InterfaceTransportConfig,
    source: ValueSource,
) -> Result<String, String> {
    let raw = match source {
        ValueSource::Descriptor => {
            if transport.base_url.trim().is_empty() {
                return Err(
                    "Gemini descriptor base_url source selected an empty base_url".to_string(),
                );
            }
            transport.base_url.trim().to_string()
        }
        ValueSource::Environment => {
            let name = transport
                .base_url_env
                .as_deref()
                .filter(|value| !value.is_empty() && value.trim() == *value)
                .ok_or_else(|| {
                    "Gemini environment base_url source requires transport.base_url_env".to_string()
                })?;
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("Gemini base URL env var '{name}' is unset or empty"))?
        }
    };
    validate_base_url(&raw)?;
    Ok(raw.trim_end_matches('/').to_string())
}

fn build_endpoint(
    base_url: &str,
    model: &str,
    operation: GeminiOperation,
) -> Result<reqwest::Url, String> {
    let suffix = if operation == GeminiOperation::StreamGenerateContent {
        format!("/models/{model}:{}?alt=sse", operation.method())
    } else {
        format!("/models/{model}:{}", operation.method())
    };
    reqwest::Url::parse(&format!("{base_url}{suffix}"))
        .map_err(|error| format!("Gemini endpoint is invalid: {error}"))
}

fn validate_base_url(raw: &str) -> Result<(), String> {
    let url =
        reqwest::Url::parse(raw).map_err(|error| format!("Gemini base_url is invalid: {error}"))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Gemini base_url cannot contain credentials, query or fragment".to_string());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "Gemini base_url has no host".to_string())?;
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    match url.scheme() {
        "https" => Ok(()),
        "http" if loopback => Ok(()),
        "http" => Err("Gemini base_url requires HTTPS outside loopback".to_string()),
        scheme => Err(format!("Gemini base_url scheme '{scheme}' is unsupported")),
    }
}

fn validate_base_url_host_gate(
    raw: &str,
    source: ValueSource,
    allowed_suffixes: &BTreeSet<String>,
) -> Result<(), String> {
    match source {
        ValueSource::Descriptor => {
            if !allowed_suffixes.is_empty() {
                return Err(
                    "Gemini descriptor base URL cannot retain environment host suffix gates"
                        .to_string(),
                );
            }
            Ok(())
        }
        ValueSource::Environment => {
            if allowed_suffixes.is_empty() {
                return Err(
                    "Gemini environment base URL requires base_url_allowed_host_suffixes"
                        .to_string(),
                );
            }
            for suffix in allowed_suffixes {
                if suffix.len() > 253
                    || suffix.starts_with('.')
                    || suffix.ends_with('.')
                    || suffix.split('.').count() < 3
                    || !suffix.bytes().all(|byte| {
                        byte.is_ascii_lowercase()
                            || byte.is_ascii_digit()
                            || matches!(byte, b'.' | b'-')
                    })
                {
                    return Err(format!(
                        "Gemini base_url_allowed_host_suffixes contains invalid suffix '{suffix}'"
                    ));
                }
            }
            let url = reqwest::Url::parse(raw)
                .map_err(|error| format!("Gemini base_url is invalid: {error}"))?;
            let host = url
                .host_str()
                .ok_or_else(|| "Gemini base_url has no host".to_string())?
                .to_ascii_lowercase();
            if !allowed_suffixes
                .iter()
                .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
            {
                return Err(format!(
                    "Gemini environment base URL host '{host}' is outside descriptor-declared host suffixes"
                ));
            }
            Ok(())
        }
    }
}

fn validate_process_env_names(
    transport: &InterfaceTransportConfig,
    base_url_source: ValueSource,
    auth_source: AuthSourceSelection,
    header_env: &BTreeMap<String, String>,
    declared: &BTreeSet<String>,
) -> Result<(), String> {
    let mut expected = BTreeSet::new();
    match base_url_source {
        ValueSource::Descriptor => {
            if transport.base_url_env.is_some() {
                return Err(
                    "Gemini descriptor base_url source cannot retain transport.base_url_env"
                        .to_string(),
                );
            }
        }
        ValueSource::Environment => {
            let name = transport
                .base_url_env
                .as_deref()
                .filter(|value| !value.is_empty() && value.trim() == *value)
                .ok_or_else(|| {
                    "Gemini environment base_url source requires transport.base_url_env".to_string()
                })?;
            expected.insert(name.to_string());
        }
    }
    match auth_source {
        AuthSourceSelection::Environment => {
            if transport.auth_secret_ref.is_some() {
                return Err(
                    "Gemini environment auth source cannot retain transport.auth_secret_ref"
                        .to_string(),
                );
            }
            let name = transport
                .auth_env_key
                .as_deref()
                .filter(|value| !value.is_empty() && value.trim() == *value)
                .ok_or_else(|| {
                    "Gemini environment auth source requires transport.auth_env_key".to_string()
                })?;
            expected.insert(name.to_string());
        }
        AuthSourceSelection::Secret => {
            if transport.auth_env_key.is_some() {
                return Err(
                    "Gemini secret auth source cannot retain transport.auth_env_key".to_string(),
                );
            }
            transport
                .auth_secret_ref
                .as_deref()
                .filter(|value| !value.is_empty() && value.trim() == *value)
                .ok_or_else(|| {
                    "Gemini secret auth source requires transport.auth_secret_ref".to_string()
                })?;
        }
        AuthSourceSelection::GoogleServiceAccountJwt
        | AuthSourceSelection::GoogleMetadataAccessToken
        | AuthSourceSelection::GoogleWorkloadIdentityFederation => {
            if transport.auth_env_key.is_some() || transport.auth_secret_ref.is_some() {
                return Err(
                    "Gemini dynamic Google auth cannot retain transport auth_env_key or auth_secret_ref"
                        .to_string(),
                );
            }
        }
    }
    expected.extend(header_env.values().cloned());
    if &expected != declared {
        return Err(format!(
            "Gemini process_env_names must exactly match selected environment sources: expected {expected:?}, received {declared:?}"
        ));
    }
    Ok(())
}

fn resolve_headers(
    transport: &InterfaceTransportConfig,
    auth_header: &HeaderName,
    header_env: &BTreeMap<String, String>,
) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::with_capacity(transport.headers.len() + header_env.len());
    for (name, value) in &transport.headers {
        let name = name
            .parse::<HeaderName>()
            .map_err(|error| format!("Gemini transport header name is invalid: {error}"))?;
        if name == auth_header || is_secret_header(&name) {
            return Err(format!(
                "Gemini secret-bearing header '{name}' must come from the explicit auth source"
            ));
        }
        let value = HeaderValue::from_str(value)
            .map_err(|error| format!("Gemini transport header value is invalid: {error}"))?;
        headers.insert(name, value);
    }
    for (name, env_name) in header_env {
        let name = name
            .parse::<HeaderName>()
            .map_err(|error| format!("Gemini header_env name is invalid: {error}"))?;
        if name == *auth_header || is_secret_header(&name) || is_secret_env_name(env_name) {
            return Err(format!(
                "Gemini header_env '{name}' -> '{env_name}' is secret-bearing and must use the explicit auth source"
            ));
        }
        if headers.contains_key(&name) {
            return Err(format!("Gemini header '{name}' is configured twice"));
        }
        let value = std::env::var(env_name)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("Gemini header env var '{env_name}' is unset or empty"))?;
        let value = HeaderValue::from_str(&value)
            .map_err(|error| format!("Gemini header env '{env_name}' is invalid: {error}"))?;
        headers.insert(name, value);
    }
    Ok(headers)
}

fn is_secret_header(name: &HeaderName) -> bool {
    let name = name.as_str().to_ascii_lowercase();
    let compact = name.replace(['-', '_'], "");
    [
        "auth",
        "bearer",
        "cookie",
        "credential",
        "key",
        "passwd",
        "password",
        "private",
        "secret",
        "session",
        "signature",
        "token",
    ]
    .iter()
    .any(|marker| compact.contains(marker))
}

fn is_secret_env_name(name: &str) -> bool {
    let name = name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_uppercase)
        .collect::<String>();
    [
        "AUTH",
        "API_KEY",
        "APIKEY",
        "ACCESS_KEY",
        "ACCESSKEY",
        "BEARER",
        "CERT",
        "COOKIE",
        "CREDENTIAL",
        "KEY",
        "PASS",
        "PRIVATE_KEY",
        "PRIVATEKEY",
        "SESSION",
        "SIGNATURE",
        "TOKEN",
        "SECRET",
    ]
    .iter()
    .map(|marker| marker.replace('_', ""))
    .any(|marker| name.contains(&marker))
}

fn validate_carrier_policy(policy: &CarrierPolicy) -> Result<(), String> {
    if policy.max_inline_bytes == 0 || policy.max_inline_bytes > 256 * 1024 * 1024 {
        return Err("Gemini carrier max_inline_bytes must be within 1..=268435456".to_string());
    }
    if policy.allowed_modalities.is_empty()
        || policy.allowed_mime_types.is_empty()
        || policy.allowed_file_uri_schemes.is_empty()
        || policy.rules.is_empty()
    {
        return Err(
            "Gemini carrier policy requires modalities, MIME types, file URI schemes and rules"
                .to_string(),
        );
    }
    let mut modalities = BTreeSet::new();
    for modality in &policy.allowed_modalities {
        if !modalities.insert(*modality) {
            return Err(format!(
                "Gemini carrier policy repeats modality '{modality:?}'"
            ));
        }
    }
    let mut mimes = BTreeSet::new();
    for mime in &policy.allowed_mime_types {
        if mime.is_empty() || mime.trim() != mime || !mime.contains('/') {
            return Err(format!(
                "Gemini carrier policy has invalid MIME pattern '{mime}'"
            ));
        }
        if !mimes.insert(mime) {
            return Err(format!(
                "Gemini carrier policy repeats MIME pattern '{mime}'"
            ));
        }
    }
    let mut schemes = BTreeSet::new();
    for scheme in &policy.allowed_file_uri_schemes {
        if scheme.is_empty()
            || scheme.trim() != scheme
            || !scheme.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'+' | b'-' | b'.')
            })
        {
            return Err(format!(
                "Gemini carrier policy has invalid URI scheme '{scheme}'"
            ));
        }
        if !schemes.insert(scheme) {
            return Err(format!(
                "Gemini carrier policy repeats URI scheme '{scheme}'"
            ));
        }
    }
    let mut labels = BTreeSet::new();
    for rule in &policy.rules {
        let label = rule.label();
        if label.is_empty() || label.trim() != label || !labels.insert(label) {
            return Err(format!(
                "Gemini carrier policy has invalid or repeated label '{label}'"
            ));
        }
        if let CarrierRule::JsonText {
            role, text_pointer, ..
        } = rule
        {
            validate_role(role).map_err(|error| format!("Gemini carrier '{label}': {error}"))?;
            if !text_pointer.starts_with('/') {
                return Err(format!(
                    "Gemini carrier '{label}' text_pointer must be an RFC 6901 JSON pointer"
                ));
            }
        }
    }
    Ok(())
}

fn validate_model_catalog(config: &ModelCatalogConfig) -> Result<(), String> {
    for (name, value) in [
        ("request_label", config.request_label()),
        ("output_label", config.output_label()),
    ] {
        if value.is_empty() || value.trim() != value {
            return Err(format!(
                "Gemini model_catalog.{name} must be non-empty and trimmed"
            ));
        }
    }
    if config.request_label() == config.output_label() {
        return Err("Gemini model catalog request and output labels must differ".to_string());
    }
    match config {
        ModelCatalogConfig::Remote(config) => {
            if config.page_size == 0 || config.page_size > 1000 {
                return Err("Gemini model_catalog.page_size must be within 1..=1000".to_string());
            }
            if config.max_pages == 0 || config.max_pages > 1024 {
                return Err("Gemini model_catalog.max_pages must be within 1..=1024".to_string());
            }
        }
        ModelCatalogConfig::Descriptor(config) => {
            if config.models.is_empty() || config.models.len() > 4096 {
                return Err(
                    "Gemini descriptor model_catalog.models must contain 1..=4096 entries"
                        .to_string(),
                );
            }
            let mut names = BTreeSet::new();
            for (index, model) in config.models.iter().enumerate() {
                let object = model.as_object().ok_or_else(|| {
                    format!("Gemini descriptor model_catalog.models[{index}] is not an object")
                })?;
                let name = object
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty() && value.trim() == *value)
                    .ok_or_else(|| {
                        format!(
                            "Gemini descriptor model_catalog.models[{index}] has no trimmed name"
                        )
                    })?;
                if !names.insert(name.to_string()) {
                    return Err(format!("Gemini descriptor model catalog repeats '{name}'"));
                }
            }
        }
    }
    Ok(())
}

fn validate_finish_policy(policy: &FinishPolicy) -> Result<(), String> {
    if policy.successful_reasons.is_empty() {
        return Err("Gemini finish_policy.successful_reasons must not be empty".to_string());
    }
    for reason in policy
        .successful_reasons
        .iter()
        .chain(policy.nonreplayable_reasons.iter())
    {
        if reason.is_empty() || reason.trim() != reason {
            return Err(
                "Gemini finish_policy reasons must be non-empty trimmed strings".to_string(),
            );
        }
    }
    if policy
        .successful_reasons
        .iter()
        .any(|reason| policy.nonreplayable_reasons.contains(reason))
    {
        return Err(
            "Gemini finish_policy successful and nonreplayable reasons must be disjoint"
                .to_string(),
        );
    }
    Ok(())
}

fn context_artifacts(
    input: &InterfaceInvocationEnvelope,
    provider_interface: &InterfaceId,
    session_snapshot_labels: &BTreeSet<String>,
) -> Result<Vec<ContextArtifact>, GeminiFailure> {
    let mut artifacts = Vec::new();
    for bound in &input.artifact_binding_payloads {
        validate_artifact_ref(&bound.artifact, "bound artifact")?;
        let bytes = bound.read_bytes().map_err(|error| {
            GeminiFailure::contract(format!(
                "bound artifact '{}' cannot be read: {error}",
                bound.artifact.id
            ))
        })?;
        let mut value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            GeminiFailure::contract(format!(
                "bound artifact '{}' is not JSON: {error}",
                bound.artifact.id
            ))
        })?;
        let bound_label = artifact_payload_label(bound.artifact.id.as_str(), &value)?;
        if let Some(snapshot) = value.get(SESSION_CONTEXT_FIELD).cloned() {
            if !session_snapshot_labels.contains(&bound_label) {
                return Err(GeminiFailure::contract(format!(
                    "bound artifact '{}' carries reserved session_context outside descriptor-declared snapshot labels",
                    bound.artifact.id
                )));
            }
            let snapshot: SessionContext = serde_json::from_value(snapshot).map_err(|error| {
                GeminiFailure::contract(format!(
                    "bound session context '{}' is malformed: {error}",
                    bound.artifact.id
                ))
            })?;
            if snapshot.schema_version != 2 {
                return Err(GeminiFailure::contract(format!(
                    "bound session context '{}' has unsupported schema_version {}",
                    bound.artifact.id, snapshot.schema_version
                )));
            }
            if &snapshot.provider_interface != provider_interface {
                return Err(GeminiFailure::contract(format!(
                    "bound session context '{}' belongs to provider '{}' instead of '{}'",
                    bound.artifact.id,
                    snapshot.provider_interface.as_str(),
                    provider_interface.as_str()
                )));
            }
            let mut snapshot_sources = BTreeSet::new();
            for (index, item) in snapshot.artifacts.into_iter().enumerate() {
                validate_artifact_ref(
                    &item.source_artifact,
                    &format!(
                        "bound session context '{}' artifact[{index}].source_artifact",
                        bound.artifact.id
                    ),
                )?;
                if item.label.is_empty() || item.label.trim() != item.label {
                    return Err(GeminiFailure::contract(format!(
                        "bound session context '{}' artifact[{index}] has an invalid label",
                        bound.artifact.id
                    )));
                }
                if item.payload.get(SESSION_CONTEXT_FIELD).is_some() {
                    return Err(GeminiFailure::contract(format!(
                        "bound session context '{}' artifact[{index}] recursively carries session_context",
                        bound.artifact.id
                    )));
                }
                let payload_label =
                    artifact_payload_label(item.source_artifact.id.as_str(), &item.payload)?;
                if payload_label != item.label {
                    return Err(GeminiFailure::contract(format!(
                        "bound session context '{}' artifact[{index}] label '{}' conflicts with source label '{}'",
                        bound.artifact.id, item.label, payload_label
                    )));
                }
                let source_key = (
                    item.source_artifact.id.as_str().to_string(),
                    item.source_artifact.content.to_hex(),
                );
                if !snapshot_sources.insert(source_key) {
                    return Err(GeminiFailure::contract(format!(
                        "bound session context '{}' repeats an artifact source at index {index}",
                        bound.artifact.id
                    )));
                }
                artifacts.push(ContextArtifact {
                    id: format!("{}#session:{index}", bound.artifact.id),
                    label: item.label,
                    value: item.payload,
                    current: false,
                    source_artifact: item.source_artifact,
                    source_movement_seq: item.source_movement_seq,
                });
            }
            value
                .as_object_mut()
                .ok_or_else(|| {
                    GeminiFailure::contract(format!(
                        "bound session context artifact '{}' is not an object",
                        bound.artifact.id
                    ))
                })?
                .remove(SESSION_CONTEXT_FIELD);
            artifacts.push(ContextArtifact {
                id: bound.artifact.id.as_str().to_string(),
                label: bound_label,
                value,
                current: false,
                source_artifact: bound.artifact.clone(),
                source_movement_seq: None,
            });
        } else {
            if session_snapshot_labels.contains(&bound_label) {
                return Err(GeminiFailure::contract(format!(
                    "bound session snapshot artifact '{}' has no session_context carrier",
                    bound.artifact.id
                )));
            }
            artifacts.push(ContextArtifact {
                id: bound.artifact.id.as_str().to_string(),
                label: bound_label,
                value,
                current: false,
                source_artifact: bound.artifact.clone(),
                source_movement_seq: None,
            });
        }
    }
    let manifest = input
        .read_artifact_manifest()
        .map_err(|error| GeminiFailure::contract(error.to_string()))?;
    let bytes = input
        .read_artifact_payload()
        .map_err(|error| GeminiFailure::contract(error.to_string()))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
        GeminiFailure::contract(format!(
            "current artifact '{}' is not JSON: {error}",
            input.artifact.id
        ))
    })?;
    validate_artifact_ref(&input.artifact, "current artifact")?;
    artifacts.push(ContextArtifact {
        id: input.artifact.id.as_str().to_string(),
        label: manifest.label,
        value,
        current: true,
        source_artifact: input.artifact.clone(),
        source_movement_seq: None,
    });
    if artifacts.iter().filter(|artifact| artifact.current).count() != 1 {
        return Err(GeminiFailure::contract(
            "Gemini invocation must contain exactly one current artifact",
        ));
    }
    let mut provenance = BTreeMap::new();
    let mut movement_provenance = BTreeMap::new();
    for artifact in &artifacts {
        let key = (
            artifact.source_artifact.id.as_str().to_string(),
            artifact.source_artifact.content.to_hex(),
        );
        if let Some((previous_label, previous_movement)) = provenance.insert(
            key.clone(),
            (artifact.label.clone(), artifact.source_movement_seq),
        ) {
            return Err(GeminiFailure::contract(format!(
                "Gemini context repeats artifact provenance for '{}' as labels '{}' and '{}' at movements {:?} and {:?}",
                artifact.source_artifact.id.as_str(),
                previous_label,
                artifact.label,
                previous_movement,
                artifact.source_movement_seq
            )));
        }
        if let Some(seq) = artifact.source_movement_seq {
            if let Some(previous) = movement_provenance.insert(seq, key.clone()) {
                return Err(GeminiFailure::contract(format!(
                    "Gemini context movement {seq} is claimed by artifact sources {previous:?} and {key:?}"
                )));
            }
        }
    }
    Ok(artifacts)
}

fn validate_artifact_ref(artifact: &ArtifactRef, surface: &str) -> Result<(), GeminiFailure> {
    let id = artifact.id.as_str();
    if id.is_empty() || id.trim() != id {
        return Err(GeminiFailure::contract(format!(
            "{surface} has an empty or untrimmed ArtifactRef id"
        )));
    }
    Ok(())
}

fn current_context_artifact(
    artifacts: &[ContextArtifact],
) -> Result<&ContextArtifact, GeminiFailure> {
    let mut current = artifacts.iter().filter(|artifact| artifact.current);
    let artifact = current
        .next()
        .ok_or_else(|| GeminiFailure::contract("Gemini invocation contains no current artifact"))?;
    if current.next().is_some() {
        return Err(GeminiFailure::contract(
            "Gemini invocation contains multiple current artifacts",
        ));
    }
    Ok(artifact)
}

fn validate_fan_in_coordinates(
    first: &InterfaceInvocationEnvelope,
    candidate: &InterfaceInvocationEnvelope,
) -> Result<(), GeminiFailure> {
    let first_runtime = first
        .runtime()
        .map_err(|error| GeminiFailure::contract(error.to_string()))?;
    let candidate_runtime = candidate
        .runtime()
        .map_err(|error| GeminiFailure::contract(error.to_string()))?;
    if first_runtime.run != candidate_runtime.run
        || first_runtime.ee != candidate_runtime.ee
        || first_runtime.worker != candidate_runtime.worker
        || first_runtime.interface != candidate_runtime.interface
        || first_runtime.epoch != candidate_runtime.epoch
        || first_runtime.run_record_hash != candidate_runtime.run_record_hash
    {
        return Err(GeminiFailure::contract(
            "Gemini parallel function results crossed runtime coordinates",
        ));
    }
    let first_metadata = first
        .metadata()
        .map_err(|error| GeminiFailure::contract(error.to_string()))?;
    let candidate_metadata = candidate
        .metadata()
        .map_err(|error| GeminiFailure::contract(error.to_string()))?;
    if first_metadata.grant != candidate_metadata.grant
        || first_metadata.outbound_interface_bindings
            != candidate_metadata.outbound_interface_bindings
        || first_metadata.required_verified_artifacts
            != candidate_metadata.required_verified_artifacts
    {
        return Err(GeminiFailure::contract(
            "Gemini parallel function results crossed formed invocation policy",
        ));
    }
    Ok(())
}

fn combine_fan_in_envelopes(
    legs: &[GeminiFanInEnvelope],
    consumed: usize,
) -> Result<(InterfaceInvocationEnvelope, Vec<GeminiFanInSource>), GeminiFailure> {
    if consumed == 0 || consumed > legs.len() {
        return Err(GeminiFailure::contract(format!(
            "Gemini fan-in cannot consume {consumed} of {} envelopes",
            legs.len()
        )));
    }
    let mut combined = legs[consumed - 1].envelope.clone();
    let mut payloads = Vec::new();
    let mut by_id = BTreeMap::<String, (String, Vec<u8>)>::new();
    let mut source_movements = Vec::with_capacity(consumed.saturating_sub(1));
    for (index, leg) in legs.iter().take(consumed).enumerate() {
        for bound in &leg.envelope.artifact_binding_payloads {
            append_fan_in_binding(&mut payloads, &mut by_id, bound.clone())?;
        }
        if index + 1 < consumed {
            let bytes = leg
                .envelope
                .read_artifact_payload()
                .map_err(|error| GeminiFailure::contract(error.to_string()))?;
            source_movements.push(GeminiFanInSource {
                artifact: leg.envelope.artifact.clone(),
                source_movement_seq: leg.source_movement_seq,
            });
            append_fan_in_binding(
                &mut payloads,
                &mut by_id,
                ArtifactBindingPayload::new(leg.envelope.artifact.clone(), bytes),
            )?;
        }
    }
    let metadata = combined.metadata.as_mut().ok_or_else(|| {
        GeminiFailure::contract("Gemini fan-in output carries no invocation metadata")
    })?;
    metadata.artifact_binding_refs = payloads
        .iter()
        .map(|binding| binding.artifact.clone())
        .collect();
    combined.artifact_binding_payloads = payloads;
    Ok((combined, source_movements))
}

fn rejected_fan_in(
    legs: &[GeminiFanInEnvelope],
    consumed: usize,
    failure: GeminiFailure,
) -> Result<GeminiFanInPlan, GeminiFailure> {
    let (envelope, source_movements) = combine_fan_in_envelopes(legs, consumed)?;
    Ok(GeminiFanInPlan::Rejected {
        envelope,
        consumed,
        failure,
        source_movements,
    })
}

fn append_fan_in_binding(
    payloads: &mut Vec<ArtifactBindingPayload>,
    by_id: &mut BTreeMap<String, (String, Vec<u8>)>,
    binding: ArtifactBindingPayload,
) -> Result<(), GeminiFailure> {
    let id = binding.artifact.id.as_str().to_string();
    let hash = binding.artifact.content.to_hex();
    let bytes = binding
        .read_bytes()
        .map_err(|error| GeminiFailure::contract(error.to_string()))?;
    match by_id.get(&id) {
        Some((known_hash, known_bytes))
            if known_hash == &hash && known_bytes.as_slice() == bytes.as_ref() =>
        {
            Ok(())
        }
        Some((known_hash, _)) => Err(GeminiFailure::contract(format!(
            "Gemini fan-in artifact id '{id}' changed content from {known_hash} to {hash}"
        ))),
        None => {
            by_id.insert(id, (hash, bytes.into_owned()));
            payloads.push(binding);
            Ok(())
        }
    }
}

fn artifact_payload_label(artifact_id: &str, value: &Value) -> Result<String, GeminiFailure> {
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
            GeminiFailure::contract(format!(
                "bound artifact '{artifact_id}' has no self-identifying label"
            ))
        })
}

fn provider_model_content(value: &Value) -> Result<(String, Value), GeminiFailure> {
    let evidence = value
        .get("provider_evidence")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            GeminiFailure::contract("provider artifact has no provider_evidence object")
        })?;
    let group = evidence
        .get("content_group")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| GeminiFailure::contract("provider_evidence has no content_group"))?;
    let content = evidence
        .get("candidate")
        .and_then(Value::as_object)
        .and_then(|candidate| candidate.get("content"))
        .cloned()
        .ok_or_else(|| GeminiFailure::contract("provider_evidence candidate has no content"))?;
    Ok((group.to_string(), content))
}

fn is_tool_result(label: &str, value: &Value) -> bool {
    label.ends_with(".result") && value.get("tool").is_some() && value.get("ok").is_some()
}

fn function_call_transcript(
    artifacts: &[ContextArtifact],
) -> Result<FunctionCallTranscript, GeminiFailure> {
    let mut grouped = BTreeMap::<String, (Value, BTreeMap<usize, ToolCallReplayIdentity>)>::new();
    let mut correlation_groups = BTreeMap::new();
    for artifact in artifacts
        .iter()
        .filter(|artifact| artifact.label == TOOL_CALL_LABEL)
    {
        let correlation_id =
            required_trimmed_string(&artifact.value, "correlation_id", "tool.call")?;
        let tool = required_trimmed_string(&artifact.value, "tool", "tool.call")?;
        let arguments = artifact
            .value
            .get("arguments")
            .filter(|value| value.is_object())
            .cloned()
            .ok_or_else(|| {
                GeminiFailure::contract(format!(
                    "tool.call '{correlation_id}' has no object arguments"
                ))
            })?;
        let provider_call_id = match artifact.value.get("provider_call_id") {
            Some(Value::Null) => None,
            Some(Value::String(value)) if !value.trim().is_empty() && value.trim() == value => {
                Some(value.clone())
            }
            Some(_) => {
                return Err(GeminiFailure::contract(format!(
                    "tool.call '{correlation_id}' provider_call_id must be a non-empty trimmed string or null"
                )));
            }
            None => {
                return Err(GeminiFailure::contract(format!(
                    "tool.call '{correlation_id}' has no provider_call_id evidence"
                )));
            }
        };
        let provider_part_index = artifact
            .value
            .get("provider_part_index")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| {
                GeminiFailure::contract(format!(
                    "tool.call '{correlation_id}' has no unsigned provider_part_index"
                ))
            })?;
        let (content_group, content) = provider_model_content(&artifact.value)?;
        let content_object = content.as_object().ok_or_else(|| {
            GeminiFailure::contract(format!(
                "Gemini response group '{content_group}' content is not an object"
            ))
        })?;
        if content_object.get("role").and_then(Value::as_str) != Some("model") {
            return Err(GeminiFailure::contract(format!(
                "Gemini response group '{content_group}' content.role must be 'model'"
            )));
        }
        let parts = content_object
            .get("parts")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                GeminiFailure::contract(format!(
                    "Gemini response group '{content_group}' content.parts is not an array"
                ))
            })?;
        let call = parts
            .get(provider_part_index)
            .and_then(Value::as_object)
            .and_then(|part| part.get("functionCall"))
            .and_then(Value::as_object)
            .ok_or_else(|| {
                GeminiFailure::contract(format!(
                    "tool.call '{correlation_id}' provider_part_index {provider_part_index} does not select a functionCall"
                ))
            })?;
        let encoded_name = call
            .get("name")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.trim() == *value)
            .ok_or_else(|| {
                GeminiFailure::contract(format!(
                    "tool.call '{correlation_id}' provider functionCall has no valid name"
                ))
            })?;
        if decode_tool_name(encoded_name)? != tool {
            return Err(GeminiFailure::contract(format!(
                "tool.call '{correlation_id}' names tool '{tool}' but provider transcript names '{encoded_name}'"
            )));
        }
        let transcript_arguments = call.get("args").cloned().unwrap_or_else(|| json!({}));
        if transcript_arguments != arguments {
            return Err(GeminiFailure::contract(format!(
                "tool.call '{correlation_id}' arguments differ from the provider transcript"
            )));
        }
        let transcript_call_id = match call.get("id") {
            None => None,
            Some(Value::String(value)) if !value.trim().is_empty() && value.trim() == value => {
                Some(value.clone())
            }
            Some(_) => {
                return Err(GeminiFailure::contract(format!(
                    "tool.call '{correlation_id}' provider transcript has an invalid id"
                )));
            }
        };
        if transcript_call_id != provider_call_id {
            return Err(GeminiFailure::contract(format!(
                "tool.call '{correlation_id}' provider_call_id differs from the provider transcript"
            )));
        }
        if correlation_groups
            .insert(correlation_id.clone(), content_group.clone())
            .is_some()
        {
            return Err(GeminiFailure::contract(format!(
                "Gemini context repeats tool.call correlation_id '{correlation_id}'"
            )));
        }
        let identity = ToolCallReplayIdentity {
            tool,
            provider_call_id,
            correlation_id,
            provider_part_index,
            arguments,
        };
        let (known_content, calls) = grouped
            .entry(content_group.clone())
            .or_insert_with(|| (content.clone(), BTreeMap::new()));
        if known_content != &content {
            return Err(GeminiFailure::contract(format!(
                "Gemini response group '{content_group}' carries conflicting model content"
            )));
        }
        if calls.insert(provider_part_index, identity).is_some() {
            return Err(GeminiFailure::contract(format!(
                "Gemini response group '{content_group}' repeats provider_part_index {provider_part_index}"
            )));
        }
    }
    let mut groups = BTreeMap::new();
    for (content_group, (content, mut indexed_calls)) in grouped {
        let parts = content
            .get("parts")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                GeminiFailure::contract(format!(
                    "Gemini response group '{content_group}' content.parts is not an array"
                ))
            })?;
        let mut calls = Vec::new();
        for (part_index, part) in parts.iter().enumerate() {
            if part
                .as_object()
                .is_some_and(|part| part.contains_key("functionCall"))
            {
                let call = indexed_calls.remove(&part_index).ok_or_else(|| {
                    GeminiFailure::contract(format!(
                        "Gemini response group '{content_group}' functionCall part {part_index} has no tool.call artifact"
                    ))
                })?;
                calls.push(call);
            }
        }
        if let Some((part_index, _)) = indexed_calls.first_key_value() {
            return Err(GeminiFailure::contract(format!(
                "Gemini response group '{content_group}' tool.call points at non-function part {part_index}"
            )));
        }
        if calls.is_empty() {
            return Err(GeminiFailure::contract(format!(
                "Gemini response group '{content_group}' has tool.call artifacts but no functionCall parts"
            )));
        }
        groups.insert(content_group, FunctionCallGroup { content, calls });
    }
    Ok(FunctionCallTranscript {
        groups,
        correlation_groups,
    })
}

fn tool_results(
    artifacts: &[ContextArtifact],
    transcript: &FunctionCallTranscript,
) -> Result<BTreeMap<String, ToolResultRecord>, GeminiFailure> {
    let mut results = BTreeMap::new();
    for artifact in artifacts
        .iter()
        .filter(|artifact| is_tool_result(&artifact.label, &artifact.value))
    {
        merge_tool_result(&mut results, tool_result_record(artifact, transcript)?)?;
    }
    Ok(results)
}

fn validate_unresolved_function_groups(
    transcript: &FunctionCallTranscript,
    results: &BTreeMap<String, ToolResultRecord>,
    active_group: &str,
) -> Result<(), GeminiFailure> {
    for (group_id, group) in &transcript.groups {
        let received = group
            .calls
            .iter()
            .filter(|call| results.contains_key(&call.correlation_id))
            .count();
        if group_id != active_group && received != group.calls.len() {
            return Err(GeminiFailure::contract(format!(
                "Gemini response group '{active_group}' is mixed with unresolved group '{group_id}' ({received} of {})",
                group.calls.len()
            )));
        }
    }
    Ok(())
}

fn merge_tool_result(
    results: &mut BTreeMap<String, ToolResultRecord>,
    result: ToolResultRecord,
) -> Result<(), GeminiFailure> {
    match results.get(&result.correlation_id) {
        Some(previous) if previous != &result => Err(GeminiFailure::contract(format!(
            "Gemini function result correlation_id '{}' carries conflicting artifacts",
            result.correlation_id
        ))),
        Some(_) => Err(GeminiFailure::contract(format!(
            "Gemini function result correlation_id '{}' is duplicated",
            result.correlation_id
        ))),
        None => {
            results.insert(result.correlation_id.clone(), result);
            Ok(())
        }
    }
}

fn tool_result_record(
    artifact: &ContextArtifact,
    transcript: &FunctionCallTranscript,
) -> Result<ToolResultRecord, GeminiFailure> {
    let tool = required_trimmed_string(&artifact.value, "tool", "tool result")?;
    let label = required_trimmed_string(&artifact.value, "label", "tool result")?;
    if artifact.label != label || label != format!("{tool}.result") {
        return Err(GeminiFailure::contract(format!(
            "tool result label '{label}' does not match artifact label '{}' and tool '{tool}'",
            artifact.label
        )));
    }
    let correlation_id = required_trimmed_string(&artifact.value, "correlation_id", "tool result")?;
    let content_group = transcript
        .correlation_groups
        .get(&correlation_id)
        .ok_or_else(|| {
            GeminiFailure::contract(format!(
                "tool result correlation_id '{correlation_id}' has no bound tool.call artifact"
            ))
        })?;
    let call = transcript
        .groups
        .get(content_group)
        .and_then(|group| {
            group
                .calls
                .iter()
                .find(|call| call.correlation_id == correlation_id)
        })
        .ok_or_else(|| {
            GeminiFailure::contract(format!(
                "tool result correlation_id '{correlation_id}' disappeared from group '{content_group}'"
            ))
        })?;
    if call.tool != tool {
        return Err(GeminiFailure::contract(format!(
            "tool result correlation_id '{correlation_id}' names tool '{tool}', bound call names '{}'",
            call.tool
        )));
    }
    tool_result_response(&artifact.value)?;
    Ok(ToolResultRecord {
        correlation_id,
        tool,
        value: artifact.value.clone(),
    })
}

fn tool_result_response(value: &Value) -> Result<Value, GeminiFailure> {
    let result: ToolResultArtifact = serde_json::from_value(value.clone()).map_err(|error| {
        GeminiFailure::contract(format!(
            "tool result is not the strict shared envelope: {error}"
        ))
    })?;
    if result.ok {
        let body = result
            .body
            .ok_or_else(|| GeminiFailure::contract("successful tool result has no body"))?;
        Ok(match body {
            Value::Object(_) => body,
            other => json!({ "output": other }),
        })
    } else {
        let error = result
            .error
            .ok_or_else(|| GeminiFailure::contract("failed tool result has no error"))?;
        Ok(json!({ "error": error }))
    }
}

fn tool_result_group_content(
    group: &FunctionCallGroup,
    results: &BTreeMap<String, ToolResultRecord>,
) -> Result<Value, GeminiFailure> {
    let mut parts = Vec::with_capacity(group.calls.len());
    for call in &group.calls {
        let result = results.get(&call.correlation_id).ok_or_else(|| {
            GeminiFailure::contract(format!(
                "Gemini response group awaits function result correlation_id '{}'",
                call.correlation_id
            ))
        })?;
        if result.tool != call.tool {
            return Err(GeminiFailure::contract(format!(
                "Gemini function result '{}' names tool '{}', expected '{}'",
                result.correlation_id, result.tool, call.tool
            )));
        }
        let mut function_response = Map::new();
        if let Some(provider_call_id) = &call.provider_call_id {
            function_response.insert("id".to_string(), Value::String(provider_call_id.clone()));
        }
        function_response.insert(
            "name".to_string(),
            Value::String(encode_tool_name(&call.tool)?),
        );
        function_response.insert("response".to_string(), tool_result_response(&result.value)?);
        parts.push(json!({ "functionResponse": Value::Object(function_response) }));
    }
    Ok(json!({ "role": "user", "parts": parts }))
}

fn required_trimmed_string(
    value: &Value,
    key: &str,
    surface: &str,
) -> Result<String, GeminiFailure> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.trim() == *value)
        .map(str::to_string)
        .ok_or_else(|| {
            GeminiFailure::contract(format!("{surface} has no non-empty trimmed '{key}'"))
        })
}

fn decode_carrier(
    rule: &CarrierRule,
    value: &Value,
    policy: &CarrierPolicy,
) -> Result<Value, GeminiFailure> {
    match rule {
        CarrierRule::JsonText {
            role, text_pointer, ..
        } => {
            let text = value
                .pointer(text_pointer)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .ok_or_else(|| {
                    GeminiFailure::contract(format!(
                        "Gemini json_text carrier has no non-empty string at '{text_pointer}'"
                    ))
                })?;
            Ok(json!({ "role": role, "parts": [{ "text": text }] }))
        }
        CarrierRule::GeminiContent { .. } => validate_content(value, policy),
    }
}

fn validate_content(value: &Value, policy: &CarrierPolicy) -> Result<Value, GeminiFailure> {
    let object = value
        .as_object()
        .ok_or_else(|| GeminiFailure::contract("gemini_content carrier is not an object"))?;
    let allowed_keys = ["role", "parts"].into_iter().collect::<BTreeSet<_>>();
    if let Some(unknown) = object
        .keys()
        .find(|key| !allowed_keys.contains(key.as_str()))
    {
        return Err(GeminiFailure::contract(format!(
            "gemini_content carrier has unknown field '{unknown}'"
        )));
    }
    let role = object
        .get("role")
        .and_then(Value::as_str)
        .ok_or_else(|| GeminiFailure::contract("gemini_content carrier has no role"))?;
    validate_role(role).map_err(GeminiFailure::contract)?;
    let parts = object
        .get("parts")
        .and_then(Value::as_array)
        .ok_or_else(|| GeminiFailure::contract("gemini_content carrier parts is not an array"))?;
    if parts.is_empty() {
        return Err(GeminiFailure::contract(
            "gemini_content carrier parts is empty",
        ));
    }
    for (index, part) in parts.iter().enumerate() {
        let part = part.as_object().ok_or_else(|| {
            GeminiFailure::contract(format!("gemini_content parts[{index}] is not an object"))
        })?;
        validate_part_union(part, false)?;
        validate_part_carrier(part, policy)?;
    }
    Ok(value.clone())
}

fn validate_role(role: &str) -> Result<(), String> {
    match role {
        "user" | "model" => Ok(()),
        _ => Err("Gemini Content role must be 'user' or 'model'".to_string()),
    }
}

fn validate_part_union(
    part: &Map<String, Value>,
    provider_output: bool,
) -> Result<(), GeminiFailure> {
    let data_keys = [
        "text",
        "inlineData",
        "functionCall",
        "functionResponse",
        "fileData",
        "executableCode",
        "codeExecutionResult",
        "toolCall",
        "toolResponse",
    ];
    let present = data_keys
        .iter()
        .filter(|key| part.contains_key(**key))
        .copied()
        .collect::<Vec<_>>();
    if present.len() != 1 {
        return Err(GeminiFailure::contract(format!(
            "Gemini Part must carry exactly one data union field, found {present:?}"
        )));
    }
    let allowed_metadata = [
        "thought",
        "thoughtSignature",
        "partMetadata",
        "mediaResolution",
        "videoMetadata",
    ];
    if let Some(unknown) = part
        .keys()
        .find(|key| !data_keys.contains(&key.as_str()) && !allowed_metadata.contains(&key.as_str()))
    {
        return Err(GeminiFailure::contract(format!(
            "Gemini Part has unknown field '{unknown}'"
        )));
    }
    if !provider_output && !matches!(present[0], "text" | "inlineData" | "fileData") {
        return Err(GeminiFailure::contract(format!(
            "descriptor carrier cannot inject Gemini '{}' parts",
            present[0]
        )));
    }
    if let Some(thought) = part.get("thought") {
        if !thought.is_boolean() {
            return Err(GeminiFailure::contract(
                "Gemini Part.thought must be boolean",
            ));
        }
    }
    if let Some(signature) = part.get("thoughtSignature") {
        let signature = signature.as_str().ok_or_else(|| {
            GeminiFailure::contract("Gemini Part.thoughtSignature must be base64 text")
        })?;
        base64::engine::general_purpose::STANDARD
            .decode(signature)
            .map_err(|_| GeminiFailure::contract("Gemini Part.thoughtSignature is not base64"))?;
    }
    Ok(())
}

fn validate_part_carrier(
    part: &Map<String, Value>,
    policy: &CarrierPolicy,
) -> Result<(), GeminiFailure> {
    if let Some(text) = part.get("text") {
        if !policy.allowed_modalities.contains(&CarrierModality::Text) {
            return Err(GeminiFailure::contract(
                "Gemini text carrier is not descriptor-admitted",
            ));
        }
        if !text.is_string() {
            return Err(GeminiFailure::contract("Gemini Part.text must be a string"));
        }
        return Ok(());
    }
    if let Some(inline) = part.get("inlineData") {
        let inline = inline
            .as_object()
            .ok_or_else(|| GeminiFailure::contract("Gemini Part.inlineData must be an object"))?;
        if inline
            .keys()
            .any(|key| !matches!(key.as_str(), "mimeType" | "data"))
        {
            return Err(GeminiFailure::contract(
                "Gemini inlineData accepts only mimeType and data",
            ));
        }
        let mime = inline
            .get("mimeType")
            .and_then(Value::as_str)
            .ok_or_else(|| GeminiFailure::contract("Gemini inlineData has no mimeType"))?;
        validate_mime(mime, policy)?;
        let data = inline
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| GeminiFailure::contract("Gemini inlineData has no base64 data"))?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|_| GeminiFailure::contract("Gemini inlineData.data is not base64"))?;
        if decoded.len() as u64 > policy.max_inline_bytes {
            return Err(GeminiFailure::contract(format!(
                "Gemini inlineData exceeds descriptor maximum {} bytes",
                policy.max_inline_bytes
            )));
        }
        return Ok(());
    }
    if let Some(file) = part.get("fileData") {
        let file = file
            .as_object()
            .ok_or_else(|| GeminiFailure::contract("Gemini Part.fileData must be an object"))?;
        if file
            .keys()
            .any(|key| !matches!(key.as_str(), "mimeType" | "fileUri"))
        {
            return Err(GeminiFailure::contract(
                "Gemini fileData accepts only mimeType and fileUri",
            ));
        }
        if let Some(mime) = file.get("mimeType") {
            validate_mime(
                mime.as_str().ok_or_else(|| {
                    GeminiFailure::contract("Gemini fileData.mimeType must be a string")
                })?,
                policy,
            )?;
        } else if !policy.allowed_modalities.contains(&CarrierModality::File) {
            return Err(GeminiFailure::contract(
                "Gemini fileData without MIME type requires descriptor-admitted file modality",
            ));
        }
        let uri = file
            .get("fileUri")
            .and_then(Value::as_str)
            .ok_or_else(|| GeminiFailure::contract("Gemini fileData has no fileUri"))?;
        let parsed = reqwest::Url::parse(uri).map_err(|error| {
            GeminiFailure::contract(format!("Gemini fileUri is invalid: {error}"))
        })?;
        if !policy
            .allowed_file_uri_schemes
            .iter()
            .any(|scheme| scheme == parsed.scheme())
        {
            return Err(GeminiFailure::contract(format!(
                "Gemini fileUri scheme '{}' is not descriptor-admitted",
                parsed.scheme()
            )));
        }
        return Ok(());
    }
    Ok(())
}

fn validate_mime(mime: &str, policy: &CarrierPolicy) -> Result<(), GeminiFailure> {
    if mime.is_empty() || mime.trim() != mime || !mime.contains('/') {
        return Err(GeminiFailure::contract(format!(
            "invalid Gemini MIME type '{mime}'"
        )));
    }
    if !policy.allowed_mime_types.iter().any(|pattern| {
        pattern == mime
            || pattern
                .strip_suffix('*')
                .is_some_and(|prefix| mime.starts_with(prefix))
    }) {
        return Err(GeminiFailure::contract(format!(
            "Gemini MIME type '{mime}' is not descriptor-admitted"
        )));
    }
    let modality = if mime.starts_with("image/") {
        CarrierModality::Image
    } else if mime.starts_with("audio/") {
        CarrierModality::Audio
    } else if mime.starts_with("video/") {
        CarrierModality::Video
    } else if mime.starts_with("text/") {
        CarrierModality::Text
    } else {
        CarrierModality::File
    };
    if !policy.allowed_modalities.contains(&modality) {
        return Err(GeminiFailure::contract(format!(
            "Gemini MIME type '{mime}' maps to non-admitted modality '{modality:?}'"
        )));
    }
    Ok(())
}

fn encode_tool_name(name: &str) -> Result<String, GeminiFailure> {
    if name.is_empty() || name.trim() != name {
        return Err(GeminiFailure::contract(
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
                return Err(GeminiFailure::contract(format!(
                    "tool name '{name}' contains a character unsupported by Gemini"
                )));
            }
        }
    }
    if encoded.len() > 128 {
        return Err(GeminiFailure::contract(format!(
            "encoded Gemini tool name '{encoded}' exceeds 128 bytes"
        )));
    }
    Ok(encoded)
}

fn decode_tool_name(encoded: &str) -> Result<String, GeminiFailure> {
    let mut output = String::with_capacity(encoded.len());
    let mut bytes = encoded.bytes();
    while let Some(byte) = bytes.next() {
        if byte != b'-' {
            if !(byte.is_ascii_alphanumeric() || byte == b'_') {
                return Err(GeminiFailure::contract(format!(
                    "Gemini function name '{encoded}' contains an invalid byte"
                )));
            }
            output.push(byte as char);
            continue;
        }
        match bytes.next() {
            Some(b'h') => output.push('-'),
            Some(b'd') => output.push('.'),
            _ => {
                return Err(GeminiFailure::contract(format!(
                    "Gemini function name '{encoded}' has an invalid escape"
                )));
            }
        }
    }
    Ok(output)
}

async fn read_bounded(
    mut response: reqwest::Response,
    maximum: usize,
    transport_retryable: bool,
    streamed: bool,
) -> Result<Vec<u8>, GeminiFailure> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(GeminiFailure::response_limit(format!(
            "Gemini response exceeds {maximum} bytes"
        )));
    }
    let mut bytes = Vec::with_capacity(
        response
            .content_length()
            .unwrap_or_default()
            .min(maximum as u64) as usize,
    );
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) if streamed && !bytes.is_empty() => {
                return Err(GeminiFailure::response_contract(
                    format!("Gemini partial stream cannot be replayed: {error}"),
                    &bytes,
                ));
            }
            Err(error) => {
                return Err(GeminiFailure::transport(
                    format!("Gemini response body read failed: {error}"),
                    transport_retryable,
                ));
            }
        };
        let next = bytes.len().checked_add(chunk.len()).ok_or_else(|| {
            GeminiFailure::response_limit("Gemini response byte count overflowed")
        })?;
        if next > maximum {
            return Err(GeminiFailure::response_limit(format!(
                "Gemini response exceeds {maximum} bytes"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn read_gemini_sse(
    mut response: reqwest::Response,
    maximum: usize,
    transport_retryable: bool,
) -> Result<(Value, ResponseBodyEvidence, usize), GeminiFailure> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(GeminiFailure::response_limit(format!(
            "Gemini response exceeds {maximum} bytes"
        )));
    }
    let mut decoder = GeminiSseDecoder::new();
    let mut bytes = 0usize;
    let mut hasher = blake3::Hasher::new();
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) if bytes > 0 => {
                return Err(GeminiFailure::response_contract_evidence(
                    format!("Gemini partial stream cannot be replayed: {error}"),
                    bytes,
                    hasher.finalize().to_hex().to_string(),
                ));
            }
            Err(error) => {
                return Err(GeminiFailure::transport(
                    format!("Gemini response body read failed: {error}"),
                    transport_retryable,
                ));
            }
        };
        bytes = bytes.checked_add(chunk.len()).ok_or_else(|| {
            GeminiFailure::response_limit("Gemini response byte count overflowed")
        })?;
        hasher.update(&chunk);
        if bytes > maximum {
            return Err(GeminiFailure::response_contract_evidence(
                format!("Gemini partial stream exceeds {maximum} bytes and cannot be replayed"),
                bytes,
                hasher.finalize().to_hex().to_string(),
            ));
        }
        if let Err(detail) = decoder.push(&chunk) {
            return Err(GeminiFailure::response_contract_evidence(
                detail,
                bytes,
                hasher.finalize().to_hex().to_string(),
            ));
        }
    }
    let digest = hasher.finalize().to_hex().to_string();
    let (response, event_count) = decoder.finish().map_err(|detail| {
        GeminiFailure::response_contract_evidence(detail, bytes, digest.clone())
    })?;
    Ok((
        response,
        ResponseBodyEvidence {
            bytes,
            blake3: digest,
        },
        event_count,
    ))
}

impl GeminiSseDecoder {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
            accumulator: GeminiStreamAccumulator::new(),
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

    fn finish(mut self) -> Result<(Value, usize), String> {
        if self.pending.iter().any(|byte| !byte.is_ascii_whitespace()) {
            let frame = std::mem::take(&mut self.pending);
            self.apply_frame(&frame)?;
        }
        let event_count = self.accumulator.event_count;
        Ok((self.accumulator.finish()?, event_count))
    }

    fn apply_frame(&mut self, frame: &[u8]) -> Result<(), String> {
        if frame.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let text = std::str::from_utf8(frame)
            .map_err(|error| format!("Gemini SSE frame is not UTF-8: {error}"))?;
        let mut data = Vec::new();
        for line in text.split(['\r', '\n']).filter(|line| !line.is_empty()) {
            if line.starts_with(':')
                || line.starts_with("event:")
                || line.starts_with("id:")
                || line.starts_with("retry:")
            {
                continue;
            }
            if let Some(value) = line.strip_prefix("data:") {
                data.push(value.strip_prefix(' ').unwrap_or(value));
            } else {
                return Err(format!(
                    "Gemini SSE contains unsupported field line '{line}'"
                ));
            }
        }
        if data.is_empty() {
            return Ok(());
        }
        let data = data.join("\n");
        if data == "[DONE]" {
            return Ok(());
        }
        let value: Value = serde_json::from_str(&data)
            .map_err(|error| format!("Gemini SSE data is not JSON: {error}"))?;
        if !value.is_object() {
            return Err("Gemini SSE event root is not an object".to_string());
        }
        self.accumulator.apply(value)
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

#[cfg(test)]
fn parse_sse_events(bytes: &[u8]) -> Result<Vec<Value>, GeminiFailure> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        GeminiFailure::response_contract(format!("Gemini SSE is not UTF-8: {error}"), bytes)
    })?;
    let normalized = text.replace("\r\n", "\n");
    let mut events = Vec::new();
    for block in normalized.split("\n\n") {
        if block.trim().is_empty() {
            continue;
        }
        let mut data = Vec::new();
        for line in block.lines() {
            if line.starts_with(':')
                || line.starts_with("event:")
                || line.starts_with("id:")
                || line.starts_with("retry:")
            {
                continue;
            }
            if let Some(value) = line.strip_prefix("data:") {
                data.push(value.strip_prefix(' ').unwrap_or(value));
            } else {
                return Err(GeminiFailure::response_contract(
                    format!("Gemini SSE contains unsupported field line '{line}'"),
                    bytes,
                ));
            }
        }
        if data.is_empty() {
            continue;
        }
        let data = data.join("\n");
        if data == "[DONE]" {
            continue;
        }
        let value: Value = serde_json::from_str(&data).map_err(|error| {
            GeminiFailure::response_contract(format!("Gemini SSE data is not JSON: {error}"), bytes)
        })?;
        if !value.is_object() {
            return Err(GeminiFailure::response_contract(
                "Gemini SSE event root is not an object",
                bytes,
            ));
        }
        events.push(value);
    }
    if events.is_empty() {
        return Err(GeminiFailure::response_contract(
            "Gemini SSE returned no JSON events",
            bytes,
        ));
    }
    Ok(events)
}

#[cfg(test)]
fn aggregate_stream_events(events: Vec<Value>, raw: &[u8]) -> Result<Value, GeminiFailure> {
    let mut accumulator = GeminiStreamAccumulator::new();
    for event in events {
        accumulator
            .apply(event)
            .map_err(|detail| GeminiFailure::response_contract(detail, raw))?;
    }
    accumulator
        .finish()
        .map_err(|detail| GeminiFailure::response_contract(detail, raw))
}

impl GeminiStreamAccumulator {
    fn new() -> Self {
        Self {
            parts: Vec::new(),
            function_identities: BTreeSet::new(),
            candidate_meta: Map::new(),
            response_meta: Map::new(),
            saw_candidate: false,
            terminal_reason: None,
            event_count: 0,
        }
    }

    fn apply(&mut self, event: Value) -> Result<(), String> {
        let event_index = self.event_count;
        self.event_count = self
            .event_count
            .checked_add(1)
            .ok_or_else(|| "Gemini SSE event count overflowed".to_string())?;
        let event = event
            .as_object()
            .ok_or_else(|| format!("Gemini SSE event {event_index} is not an object"))?;
        for key in ["modelVersion", "responseId"] {
            if let Some(value) = event.get(key) {
                if self
                    .response_meta
                    .get(key)
                    .is_some_and(|previous| previous != value)
                {
                    return Err(format!(
                        "Gemini SSE event {event_index} changes stable field '{key}'"
                    ));
                }
                self.response_meta.insert(key.to_string(), value.clone());
            }
        }
        for key in ["usageMetadata", "promptFeedback"] {
            if let Some(value) = event.get(key) {
                self.response_meta.insert(key.to_string(), value.clone());
            }
        }
        let Some(candidates) = event.get("candidates").and_then(Value::as_array) else {
            if event.contains_key("usageMetadata") || event.contains_key("promptFeedback") {
                return Ok(());
            }
            return Err(format!(
                "Gemini SSE event {event_index} has no candidates array"
            ));
        };
        if candidates.is_empty() {
            if event.contains_key("usageMetadata") || event.contains_key("promptFeedback") {
                return Ok(());
            }
            return Err(format!(
                "Gemini SSE event {event_index} has an empty candidates array"
            ));
        }
        if candidates.len() != 1 {
            return Err(format!(
                "Gemini SSE event {event_index} must contain exactly one candidate"
            ));
        }
        self.saw_candidate = true;
        let candidate = candidates[0]
            .as_object()
            .ok_or_else(|| format!("Gemini SSE candidate {event_index} is not an object"))?;
        if candidate
            .get("index")
            .is_some_and(|index| index.as_u64() != Some(0))
        {
            return Err(format!(
                "Gemini SSE event {event_index} candidate index must be 0"
            ));
        }
        if self.terminal_reason.is_some() {
            return Err(format!(
                "Gemini SSE event {event_index} arrived after terminal finishReason"
            ));
        }
        let event_finish_reason = match candidate.get("finishReason") {
            None | Some(Value::Null) => None,
            Some(Value::String(reason)) if !reason.is_empty() && reason.trim() == reason => {
                Some(reason.clone())
            }
            Some(_) => {
                return Err(format!(
                    "Gemini SSE event {event_index} has invalid finishReason"
                ));
            }
        };
        for (key, value) in candidate {
            if key != "content" {
                self.candidate_meta.insert(key.clone(), value.clone());
            }
        }
        let Some(content) = candidate.get("content").and_then(Value::as_object) else {
            if candidate.contains_key("finishReason") || event.contains_key("usageMetadata") {
                if let Some(reason) = event_finish_reason {
                    self.terminal_reason = Some(reason);
                }
                return Ok(());
            }
            return Err(format!(
                "Gemini SSE event {event_index} has no content.parts"
            ));
        };
        if content.get("role").and_then(Value::as_str) != Some("model") {
            return Err(format!(
                "Gemini SSE event {event_index} content.role must be 'model'"
            ));
        }
        let event_parts = content
            .get("parts")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("Gemini SSE event {event_index} has no content.parts"))?;
        for (part_index, part) in event_parts.iter().enumerate() {
            let object = part.as_object().ok_or_else(|| {
                format!("Gemini SSE event {event_index} part {part_index} is not an object")
            })?;
            if object.contains_key("functionCall") {
                let call = object
                    .get("functionCall")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        format!(
                            "Gemini SSE event {event_index} part {part_index} functionCall is not an object"
                        )
                    })?;
                let name = call
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty() && value.trim() == *value)
                    .ok_or_else(|| {
                        format!(
                            "Gemini SSE event {event_index} part {part_index} functionCall has no valid name"
                        )
                    })?;
                let body = serde_json::to_vec(call).map_err(|error| {
                    format!("Gemini SSE functionCall identity did not encode: {error}")
                })?;
                let provider_id = call
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty() && value.trim() == *value);
                let signature = object
                    .get("thoughtSignature")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let identity = provider_id.map_or_else(
                    || {
                        format!(
                            "part:{part_index}:name:{name}:signature:{signature}:body:{}",
                            blake3::hash(&body).to_hex()
                        )
                    },
                    |id| format!("id:{id}"),
                );
                if !self.function_identities.insert(identity) {
                    return Err(format!(
                        "Gemini SSE event {event_index} repeats an ambiguous functionCall identity"
                    ));
                }
                self.parts.push(part.clone());
            } else {
                self.parts.push(part.clone());
            }
        }
        if let Some(reason) = event_finish_reason {
            self.terminal_reason = Some(reason);
        }
        Ok(())
    }

    fn finish(mut self) -> Result<Value, String> {
        if self.event_count == 0 {
            return Err("Gemini SSE returned no JSON events".to_string());
        }
        if self.saw_candidate && self.terminal_reason.is_none() {
            return Err("Gemini SSE ended before a terminal finishReason".to_string());
        }
        if self.saw_candidate {
            self.candidate_meta.insert(
                "content".to_string(),
                json!({ "role": "model", "parts": self.parts }),
            );
            self.response_meta.insert(
                "candidates".to_string(),
                Value::Array(vec![Value::Object(self.candidate_meta)]),
            );
        } else {
            self.response_meta
                .insert("candidates".to_string(), Value::Array(Vec::new()));
        }
        Ok(Value::Object(self.response_meta))
    }
}

fn materialize_emission(
    output_root: &Path,
    label: &str,
    payload: Value,
    route: ProviderEmissionRoute,
) -> Result<ProviderEmission, GeminiFailure> {
    let bytes = serde_json::to_vec(&payload).map_err(|error| {
        GeminiFailure::contract(format!("Gemini output artifact did not encode: {error}"))
    })?;
    let artifact = InterfaceArtifactFile::write(output_root, label, &bytes)
        .map_err(|error| GeminiFailure::contract(error.to_string()))?;
    Ok(ProviderEmission { artifact, route })
}

fn render_json(value: &Value) -> Result<String, GeminiFailure> {
    serde_json::to_string(value).map_err(|error| {
        GeminiFailure::contract(format!("Gemini context JSON did not encode: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_descriptors::{ArtifactId, CheckpointId, EeId, InterfaceId, RunId};
    use octopus_interfaces::{
        ArtifactBindingPayload, GrantProjection, InvocationMetadata, RuntimeInvocationMetadata,
    };
    use octopus_ledger::{ArtifactRef, ContentHash};

    #[test]
    fn model_catalog_normalizes_gemini_names_against_formed_policy() {
        let formed = BTreeSet::from(["gemini-2.5-pro".to_string()]);
        assert!(gemini_catalog_model_allowed(
            "models/gemini-2.5-pro",
            &formed
        ));
        assert!(gemini_catalog_model_allowed("gemini-2.5-pro", &formed));
        assert!(!gemini_catalog_model_allowed(
            "models/gemini-2.5-flash",
            &formed
        ));
    }

    fn root_descriptor() -> InterfaceDescriptor {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../interface.json");
        let bytes = std::fs::read(path).unwrap();
        octopus_descriptors::parse_strict(&bytes).unwrap()
    }

    fn payload_binding(id: &str, value: Value) -> ArtifactBindingPayload {
        let bytes = serde_json::to_vec(&value).unwrap();
        ArtifactBindingPayload::new(
            ArtifactRef {
                id: ArtifactId::new(id),
                content: ContentHash::of_bytes(&bytes),
            },
            bytes,
        )
    }

    fn rewrite_bound_json(
        envelope: &mut InterfaceInvocationEnvelope,
        artifact_id: &str,
        rewrite: impl FnOnce(&mut Value),
    ) {
        let binding = envelope
            .artifact_binding_payloads
            .iter_mut()
            .find(|binding| binding.artifact.id.as_str() == artifact_id)
            .unwrap();
        let mut value: Value = serde_json::from_slice(&binding.read_bytes().unwrap()).unwrap();
        rewrite(&mut value);
        *binding = ArtifactBindingPayload::new(
            binding.artifact.clone(),
            serde_json::to_vec(&value).unwrap(),
        );
        binding.artifact.content = ContentHash::of_bytes(&binding.read_bytes().unwrap());
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
                json!({ "label": CONTEXT_REFERENCE_LABEL, "text": "grounded role" }),
            ),
        );
        let bytes = serde_json::to_vec(&current).unwrap();
        let content = ContentHash::of_bytes(&bytes);
        let root = std::env::temp_dir().join(format!(
            "octopus-gemini-test-{}-{}",
            std::process::id(),
            content.to_hex()
        ));
        let artifact_file = InterfaceArtifactFile::write(root, current_label, &bytes).unwrap();
        let names = affordances
            .iter()
            .filter_map(|value| value.get("name").and_then(Value::as_str))
            .map(str::to_string)
            .collect::<Vec<_>>();
        let affordances = affordances
            .into_iter()
            .map(|value| serde_json::from_value(value).unwrap())
            .collect();
        let mut grant = GrantProjection::default()
            .with_goal(Some("complete the Gemini turn".to_string()))
            .with_interface_affordances(names.clone(), affordances);
        grant.tools = names;
        InterfaceInvocationEnvelope {
            artifact: ArtifactRef {
                id: ArtifactId::new(format!("{current_label}:{}", &content.to_hex()[..24])),
                content,
            },
            artifact_file,
            artifact_binding_payloads: bound,
            metadata: Some(InvocationMetadata::new(grant).with_runtime(
                RuntimeInvocationMetadata {
                    run: RunId::new("run-test"),
                    ee: EeId::new("ee-test"),
                    worker: InterfaceId::new("system_worker_test"),
                    checkpoint: CheckpointId::new("checkpoint-test"),
                    interface: InterfaceId::new("world_provider_gemini"),
                    epoch: 1,
                    run_record_hash: Some("hash".to_string()),
                },
            )),
        }
    }

    fn driver() -> GeminiNativeDriver {
        GeminiNativeDriver::from_descriptor(&root_descriptor(), tempfile::tempdir().unwrap().keep())
            .unwrap()
    }

    fn native_driver_fixture() -> GeminiNativeDriver {
        let mut descriptor = root_descriptor();
        descriptor.interface_driver_ref = GEMINI_NATIVE_DRIVER_REF.into();
        let config = &mut descriptor.interface_driver_config.as_mut().unwrap().private_config;
        for field in [
            "instruction_artifacts", "worker_instruction_artifacts", "model_tool_names",
            "native_tool_namespace", "operator_continuity", "model_catalog_operation",
            "private_thread_cold_start", "model_result_projection", "client_projection_output_labels",
            "opencodex",
        ] {
            config.remove(field);
        }
        GeminiNativeDriver::from_descriptor(&descriptor, tempfile::tempdir().unwrap().keep())
            .expect("explicit native Gemini fixture")
    }

    fn fan_in_leg(
        envelope: InterfaceInvocationEnvelope,
        source_movement_seq: u64,
    ) -> GeminiFanInEnvelope {
        GeminiFanInEnvelope {
            envelope,
            source_movement_seq: Some(source_movement_seq),
        }
    }

    #[test]
    fn root_descriptor_is_one_explicit_secret_bound_native_family() {
        let descriptor = root_descriptor();
        let transport = descriptor
            .interface_driver_config
            .as_ref()
            .unwrap()
            .transport
            .as_ref()
            .unwrap();
        assert_eq!(descriptor.interface_driver_ref, GEMINI_NATIVE_DRIVER_REF);
        assert_eq!(transport.auth_env_key, None);
        assert_eq!(
            transport.auth_secret_ref.as_deref(),
            Some("secret://world_provider_gemini/GEMINI_API_KEY")
        );
        let driver = driver();
        assert_eq!(
            driver.endpoint.as_str(),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent"
        );
    }

    #[test]
    fn vertex_concrete_descriptor_reuses_only_the_native_handler_family() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../world_provider_vertex/interface.json");
        let descriptor: InterfaceDescriptor =
            octopus_descriptors::parse_strict(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(descriptor.id.as_str(), "world_provider_vertex");
        assert!(serde_json::to_value(&descriptor)
            .unwrap()
            .get("reference")
            .is_none());
        assert_eq!(descriptor.interface_driver_ref, GEMINI_NATIVE_DRIVER_REF);
        let config = descriptor.interface_driver_config.as_ref().unwrap();
        assert_eq!(
            config
                .private_config
                .get("operation")
                .and_then(Value::as_str),
            Some("stream_generate_content")
        );
        assert_eq!(config.transport.as_ref().unwrap().auth_secret_ref, None);
        assert_eq!(config.transport.as_ref().unwrap().auth_env_key, None);
        assert_eq!(
            config
                .private_config
                .get("auth_source")
                .and_then(Value::as_str),
            Some("google_service_account_jwt")
        );
        assert_eq!(
            config
                .private_config
                .get("service_account_jwt")
                .and_then(|value| value.get("credential_secret_ref"))
                .and_then(Value::as_str),
            Some("secret://world_provider_vertex/GOOGLE_SERVICE_ACCOUNT_JSON")
        );
        assert!(config.private_config.get("model_catalog").is_none());
        assert_eq!(config.private_config.get("header_env"), Some(&json!({})));
        assert_eq!(
            config
                .private_config
                .get("retry")
                .and_then(|retry| retry.get("retryable_http_statuses")),
            Some(&json!([408, 429, 500, 502, 503, 504]))
        );
    }

    #[test]
    fn request_is_native_and_keeps_exact_tool_json_schema() {
        let driver = native_driver_fixture();
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "path": { "type": "string" }
            },
            "required": ["path"]
        });
        let input = invocation(
            "interface.input",
            json!({ "prompt": "read the artifact" }),
            Vec::new(),
            vec![json!({
                "name": "artifact.read",
                "description": "Read one bound artifact",
                "input_schema": schema.clone()
            }), json!({
                "name": "candidate.read",
                "description": "Granted but not selected",
                "input_schema": {"type": "object"}
            })],
        );
        let metadata_before = serde_json::to_value(input.metadata().unwrap()).unwrap();
        let hidden = model_tool_exclusions(
            input.metadata().unwrap(),
            &BTreeSet::from(["artifact.read".into()]),
            &model_hidden_tool_names(&driver.config.private_config).unwrap(),
        );
        let (request, visible) = driver.build_request_with_visible_tools(&input, &hidden).unwrap();
        assert_eq!(visible, BTreeSet::from(["artifact.read".into()]));
        assert_eq!(request["tools"][0]["functionDeclarations"].as_array().unwrap().len(), 1);
        assert!(!request["systemInstruction"]["parts"][0]["text"].as_str().unwrap().contains("candidate.read"));
        assert_eq!(serde_json::to_value(input.metadata().unwrap()).unwrap(), metadata_before);
        assert_eq!(
            request.pointer("/contents/0/role").and_then(Value::as_str),
            Some("user")
        );
        assert_eq!(
            request
                .pointer("/contents/0/parts/0/text")
                .and_then(Value::as_str),
            Some("read the artifact")
        );
        assert!(request
            .pointer("/systemInstruction/parts/0/text")
            .and_then(Value::as_str)
            .unwrap()
            .contains("complete the Gemini turn"));
        assert_eq!(
            request.pointer("/tools/0/functionDeclarations/0/name"),
            Some(&json!("artifact-dread"))
        );
        assert_eq!(
            request.pointer("/tools/0/functionDeclarations/0/parametersJsonSchema"),
            Some(&schema)
        );
        assert!(request.get("model").is_none());
    }

    #[test]
    fn parallel_function_results_wait_for_the_complete_exact_wire_group() {
        let driver = driver();
        let model_content = json!({
            "role": "model",
            "parts": [
                {
                    "functionCall": {
                        "id": "call-1",
                        "name": "artifact-dread",
                        "args": { "id": "one" }
                    },
                    "thoughtSignature": "c2ln"
                },
                {
                    "functionCall": {
                        "id": "call-2",
                        "name": "artifact-dread",
                        "args": { "id": "two" }
                    }
                }
            ]
        });
        let provider_evidence = json!({
            "content_group": "response-1",
            "candidate": { "content": model_content }
        });
        let first_call = json!({
            "tool": "artifact.read",
            "arguments": { "id": "one" },
            "correlation_id": "call-1",
            "provider_call_id": "call-1",
            "provider_part_index": 0,
            "provider_evidence": provider_evidence.clone()
        });
        let second_call = json!({
            "tool": "artifact.read",
            "arguments": { "id": "two" },
            "correlation_id": "call-2",
            "provider_call_id": "call-2",
            "provider_part_index": 1,
            "provider_evidence": provider_evidence
        });
        let calls = vec![
            payload_binding("tool.call:call-1", first_call),
            payload_binding("tool.call:call-2", second_call),
        ];
        let first = invocation(
            "artifact.read.result",
            json!({
                "label": "artifact.read.result",
                "tool": "artifact.read",
                "correlation_id": "call-1",
                "ok": true,
                "body": { "text": "first" }
            }),
            calls.clone(),
            Vec::new(),
        );
        match driver
            .prepare_fan_in(&[fan_in_leg(first.clone(), 41)])
            .unwrap()
        {
            GeminiFanInPlan::Pending {
                content_group,
                expected,
                received,
                ..
            } => {
                assert_eq!(content_group, "response-1");
                assert_eq!(expected, 2);
                assert_eq!(received, 1);
            }
            GeminiFanInPlan::Ready { .. } => {
                panic!("one of two parallel Gemini results reached the provider")
            }
            GeminiFanInPlan::Rejected { failure, .. } => {
                panic!("parallel Gemini group was rejected: {}", failure.detail)
            }
        }
        let second = invocation(
            "artifact.read.result",
            json!({
                "label": "artifact.read.result",
                "tool": "artifact.read",
                "correlation_id": "call-2",
                "ok": true,
                "body": { "text": "second" }
            }),
            calls,
            Vec::new(),
        );
        match driver
            .prepare_fan_in(&[fan_in_leg(first.clone(), 41), fan_in_leg(first.clone(), 42)])
            .unwrap()
        {
            GeminiFanInPlan::Rejected {
                consumed, failure, ..
            } => {
                assert_eq!(consumed, 2);
                assert!(failure.detail.contains("is duplicated"));
            }
            GeminiFanInPlan::Ready { .. } | GeminiFanInPlan::Pending { .. } => {
                panic!("duplicated Gemini function result was not rejected")
            }
        }
        let first_artifact = first.artifact.clone();
        let plan = driver
            .prepare_fan_in(&[fan_in_leg(first, 41), fan_in_leg(second, 42)])
            .unwrap();
        let GeminiFanInPlan::Ready {
            envelope,
            consumed,
            source_movements,
        } = plan
        else {
            panic!("the complete Gemini function-result group remained pending")
        };
        assert_eq!(consumed, 2);
        assert_eq!(
            source_movements,
            vec![GeminiFanInSource {
                artifact: first_artifact,
                source_movement_seq: Some(41),
            }]
        );
        let request = driver.build_request(&envelope).unwrap();
        assert_eq!(
            request.pointer("/contents/0/parts/0/thoughtSignature"),
            Some(&json!("c2ln"))
        );
        assert_eq!(
            request.pointer("/contents/0/parts/1/functionCall/id"),
            Some(&json!("call-2"))
        );
        assert_eq!(
            request.pointer("/contents/1/parts/0/functionResponse/id"),
            Some(&json!("call-1"))
        );
        assert_eq!(
            request.pointer("/contents/1/parts/0/functionResponse/name"),
            Some(&json!("artifact-dread"))
        );
        assert_eq!(
            request.pointer("/contents/1/parts/0/functionResponse/response/text"),
            Some(&json!("first"))
        );
        assert_eq!(
            request.pointer("/contents/1/parts/1/functionResponse/id"),
            Some(&json!("call-2"))
        );
        assert_eq!(
            request.pointer("/contents/1/parts/1/functionResponse/response/text"),
            Some(&json!("second"))
        );
        assert_eq!(request["contents"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn session_context_v2_is_provider_bound_and_keeps_source_provenance() {
        let source_payload = json!({
            "label": CONTEXT_REFERENCE_LABEL,
            "text": "session-grounded role"
        });
        let source_bytes = serde_json::to_vec(&source_payload).unwrap();
        let source = ArtifactRef {
            id: ArtifactId::new("context.reference:session-source"),
            content: ContentHash::of_bytes(&source_bytes),
        };
        let snapshot = json!({
            "session_context": {
                "schema_version": 2,
                "provider_interface": "world_provider_gemini",
                "artifacts": [{
                    "label": CONTEXT_REFERENCE_LABEL,
                    "payload": source_payload,
                    "source_artifact": source,
                    "source_movement_seq": 17
                }]
            }
        });
        let input = invocation(
            "interface.input",
            json!({ "prompt": "continue" }),
            vec![payload_binding(
                "interface.artifact.output:snapshot",
                snapshot,
            )],
            Vec::new(),
        );
        let snapshot_labels = BTreeSet::from([INTERFACE_ARTIFACT_OUTPUT_LABEL.to_string()]);
        let artifacts = context_artifacts(
            &input,
            &InterfaceId::new("world_provider_gemini"),
            &snapshot_labels,
        )
        .unwrap();
        let session = artifacts
            .iter()
            .find(|artifact| artifact.id.contains("#session:0"))
            .unwrap();
        assert_eq!(
            session.source_artifact.id.as_str(),
            "context.reference:session-source"
        );
        assert_eq!(session.source_movement_seq, Some(17));
        let outer = artifacts
            .iter()
            .find(|artifact| artifact.id == "interface.artifact.output:snapshot")
            .unwrap();
        assert!(outer.value.get(SESSION_CONTEXT_FIELD).is_none());

        let mut mismatched = input.clone();
        rewrite_bound_json(
            &mut mismatched,
            "interface.artifact.output:snapshot",
            |value| {
                value["session_context"]["provider_interface"] = json!("world_provider_vertex");
            },
        );
        assert!(context_artifacts(
            &mismatched,
            &InterfaceId::new("world_provider_gemini"),
            &snapshot_labels,
        )
        .unwrap_err()
        .detail
        .contains("belongs to provider"));

        let mut duplicated = input.clone();
        rewrite_bound_json(
            &mut duplicated,
            "interface.artifact.output:snapshot",
            |value| {
                let artifacts = value["session_context"]["artifacts"]
                    .as_array_mut()
                    .unwrap();
                artifacts.push(artifacts[0].clone());
            },
        );
        assert!(context_artifacts(
            &duplicated,
            &InterfaceId::new("world_provider_gemini"),
            &snapshot_labels,
        )
        .unwrap_err()
        .detail
        .contains("repeats an artifact source"));

        let mut relabeled = input.clone();
        rewrite_bound_json(
            &mut relabeled,
            "interface.artifact.output:snapshot",
            |value| {
                value["session_context"]["artifacts"][0]["label"] = json!("gemini.content");
            },
        );
        assert!(context_artifacts(
            &relabeled,
            &InterfaceId::new("world_provider_gemini"),
            &snapshot_labels,
        )
        .unwrap_err()
        .detail
        .contains("conflicts with source label"));

        let mut forged_movement = input.clone();
        rewrite_bound_json(
            &mut forged_movement,
            "interface.artifact.output:snapshot",
            |value| {
                value["session_context"]["artifacts"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({
                        "label": "gemini.content",
                        "payload": { "role": "user", "parts": [{ "text": "forged" }] },
                        "source_artifact": {
                            "id": "gemini.content:forged",
                            "content": ContentHash::of_bytes(b"forged")
                        },
                        "source_movement_seq": 17
                    }));
            },
        );
        assert!(context_artifacts(
            &forged_movement,
            &InterfaceId::new("world_provider_gemini"),
            &snapshot_labels,
        )
        .unwrap_err()
        .detail
        .contains("movement 17 is claimed"));

        let mut recursive = input.clone();
        rewrite_bound_json(
            &mut recursive,
            "interface.artifact.output:snapshot",
            |value| {
                value["session_context"]["artifacts"][0]["payload"][SESSION_CONTEXT_FIELD] =
                    json!({});
            },
        );
        assert!(context_artifacts(
            &recursive,
            &InterfaceId::new("world_provider_gemini"),
            &snapshot_labels,
        )
        .unwrap_err()
        .detail
        .contains("recursively carries session_context"));

        let mut undeclared = input;
        let snapshot_binding = undeclared
            .artifact_binding_payloads
            .iter_mut()
            .find(|binding| binding.artifact.id.as_str() == "interface.artifact.output:snapshot")
            .unwrap();
        snapshot_binding.artifact.id = ArtifactId::new("foreign.snapshot:snapshot");
        assert!(context_artifacts(
            &undeclared,
            &InterfaceId::new("world_provider_gemini"),
            &snapshot_labels,
        )
        .unwrap_err()
        .detail
        .contains("outside descriptor-declared snapshot labels"));
    }

    #[test]
    fn retry_schedule_and_secret_sources_are_descriptor_owned() {
        let driver = driver();
        assert_eq!(driver.retry_delay(1).unwrap(), Duration::from_millis(1000));
        assert_eq!(driver.retry_delay(4).unwrap(), Duration::from_millis(8000));
        assert!(driver.validate_max_attempts(5).is_ok());
        assert!(driver.validate_max_attempts(6).is_err());
        assert!(driver.config.retry.retryable_http_statuses.contains(&429));
        assert!(!driver.config.retry.retryable_http_statuses.contains(&501));

        for header in [
            "Authorization",
            "Cookie",
            "Private-Token",
            "X-Amz-Credential",
            "X-Signature",
            "X-Subscription-Key",
            "X-Vault-Token",
        ] {
            let mut descriptor = root_descriptor();
            descriptor
                .interface_driver_config
                .as_mut()
                .unwrap()
                .transport
                .as_mut()
                .unwrap()
                .headers
                .insert(header.to_string(), "descriptor-value".to_string());
            let error = GeminiNativeDriver::from_descriptor(
                &descriptor,
                tempfile::tempdir().unwrap().keep(),
            )
            .err()
            .unwrap();
            assert!(error.contains("explicit auth source"), "{header}: {error}");
        }

        let mut descriptor = root_descriptor();
        let metadata = &mut descriptor
            .interface_driver_config
            .as_mut()
            .unwrap()
            .private_config;
        metadata.insert(
            "header_env".to_string(),
            json!({ "X-Tenant": "SERVICE_ACCESS_TOKEN" }),
        );
        metadata.insert(
            "process_env_names".to_string(),
            json!(["SERVICE_ACCESS_TOKEN"]),
        );
        let error =
            GeminiNativeDriver::from_descriptor(&descriptor, tempfile::tempdir().unwrap().keep())
                .err()
                .unwrap();
        assert!(error.contains("secret-bearing"));

        let mut descriptor = root_descriptor();
        descriptor
            .interface_driver_config
            .as_mut()
            .unwrap()
            .private_config
            .insert("undeclared_retry_hint".to_string(), json!(true));
        let error =
            GeminiNativeDriver::from_descriptor(&descriptor, tempfile::tempdir().unwrap().keep())
                .err()
                .unwrap();
        assert!(error.contains("unknown field `undeclared_retry_hint`"));

        let mut descriptor = root_descriptor();
        descriptor
            .interface_driver_config
            .as_mut()
            .unwrap()
            .transport
            .as_mut()
            .unwrap()
            .auth_secret_ref = Some("secret://world_provider_vertex/GEMINI_API_KEY".to_string());
        let error =
            GeminiNativeDriver::from_descriptor(&descriptor, tempfile::tempdir().unwrap().keep())
                .err()
                .unwrap();
        assert!(error.contains("must exactly match concrete provider interface"));
    }

    #[test]
    fn model_affordances_and_generation_limits_fail_closed() {
        let driver = driver();
        let input = invocation(
            "interface.input",
            json!({ "prompt": "read" }),
            Vec::new(),
            vec![json!({
                "name": "artifact.read",
                "description": "read one artifact",
                "input_schema": null
            })],
        );
        assert!(driver
            .build_request(&input)
            .unwrap_err()
            .detail
            .contains("args_schema is not an object"));

        let schema = json!({ "type": "object", "properties": {} });
        let mut ungranted = invocation(
            "interface.input",
            json!({ "prompt": "read" }),
            Vec::new(),
            vec![json!({
                "name": "artifact.read",
                "description": "read one artifact",
                "input_schema": schema.clone()
            })],
        );
        ungranted.metadata.as_mut().unwrap().grant.tools.clear();
        assert!(driver
            .build_request(&ungranted)
            .unwrap_err()
            .detail
            .contains("outside the formed tool grant"));

        let mut missing_affordance = invocation(
            "interface.input",
            json!({ "prompt": "read" }),
            Vec::new(),
            Vec::new(),
        );
        missing_affordance
            .metadata
            .as_mut()
            .unwrap()
            .grant
            .tools
            .push("artifact.read".to_string());
        assert!(driver
            .build_request(&missing_affordance)
            .unwrap_err()
            .detail
            .contains("do not exactly match"));

        let mut contradictory = invocation(
            "interface.input",
            json!({ "prompt": "read" }),
            Vec::new(),
            vec![json!({
                "name": "artifact.read",
                "description": "read one artifact",
                "input_schema": schema
            })],
        );
        contradictory
            .metadata
            .as_mut()
            .unwrap()
            .grant
            .hard_blocked_tools
            .push("artifact.read".to_string());
        assert!(driver
            .build_request(&contradictory)
            .unwrap_err()
            .detail
            .contains("both grants and hard-blocks"));

        let mut descriptor = root_descriptor();
        descriptor
            .interface_driver_config
            .as_mut()
            .unwrap()
            .private_config
            .get_mut("generation_config")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("maxOutputTokens");
        let error =
            GeminiNativeDriver::from_descriptor(&descriptor, tempfile::tempdir().unwrap().keep())
                .err()
                .unwrap();
        assert!(error.contains("maxOutputTokens"));
    }

    #[test]
    fn content_carrier_admits_text_image_audio_video_and_files() {
        let driver = driver();
        let content = json!({
            "role": "user",
            "parts": [
                { "text": "inspect" },
                { "inlineData": { "mimeType": "image/png", "data": "aW1hZ2U=" } },
                { "inlineData": { "mimeType": "audio/wav", "data": "YXVkaW8=" } },
                { "inlineData": { "mimeType": "video/mp4", "data": "dmlkZW8=" } },
                { "fileData": { "mimeType": "application/pdf", "fileUri": "https://files.test/document.pdf" } }
            ]
        });
        let input = invocation("gemini.content", content.clone(), Vec::new(), Vec::new());
        let request = driver.build_request(&input).unwrap();
        assert_eq!(request.pointer("/contents/0"), Some(&content));

        let refused = json!({
            "role": "user",
            "parts": [{
                "inlineData": { "mimeType": "application/octet-stream", "data": "AA==" }
            }]
        });
        let input = invocation("gemini.content", refused, Vec::new(), Vec::new());
        assert!(driver
            .build_request(&input)
            .unwrap_err()
            .detail
            .contains("not descriptor-admitted"));
    }

    #[test]
    fn response_materializes_text_media_multiple_calls_reasoning_and_usage_evidence() {
        let driver = driver();
        let response = json!({
            "candidates": [{
                "index": 0,
                "finishReason": "STOP",
                "safetyRatings": [],
                "content": {
                    "role": "model",
                    "parts": [
                        { "thought": true, "text": "reasoning", "thoughtSignature": "c2ln" },
                        { "text": "visible" },
                        { "inlineData": { "mimeType": "image/png", "data": "aW1hZ2U=" } },
                        { "functionCall": { "id": "call-1", "name": "artifact-dread", "args": { "id": "one" } }, "thoughtSignature": "c2ln" },
                        { "functionCall": { "id": "call-2", "name": "artifact-dread", "args": { "id": "two" } } }
                    ]
                }
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 5,
                "totalTokenCount": 15
            },
            "modelVersion": "gemini-test",
            "responseId": "response-test"
        });
        let bytes = serde_json::to_vec(&response).unwrap();
        let root = tempfile::tempdir().unwrap();
        let emissions = driver
            .materialize_response(
                GeminiResponseEnvelope {
                    response,
                    response_bytes: bytes.len(),
                    response_blake3: blake3::hash(&bytes).to_hex().to_string(),
                    stream_event_count: None,
                },
                root.path(),
                &BTreeSet::from(["artifact.read".to_string()]),
            )
            .unwrap();
        assert_eq!(emissions.len(), 5);
        assert_eq!(
            emissions[0].artifact.read_manifest().unwrap().label,
            PROVIDER_REASONING_LABEL
        );
        assert_eq!(
            emissions[1].artifact.read_manifest().unwrap().label,
            PROVIDER_USAGE_LABEL
        );
        let usage: Value =
            serde_json::from_slice(&emissions[1].artifact.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(
            usage.pointer("/usage_metadata/totalTokenCount"),
            Some(&json!(15))
        );
        let visible: Value =
            serde_json::from_slice(&emissions[2].artifact.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(visible.get("text"), Some(&json!("visible")));
        assert_eq!(
            visible.pointer("/reasoning/0/text"),
            Some(&json!("reasoning"))
        );
        assert_eq!(
            visible.pointer("/provider_evidence/usage_metadata/totalTokenCount"),
            Some(&json!(15))
        );
        let first_call: Value =
            serde_json::from_slice(&emissions[3].artifact.read_payload_bytes().unwrap()).unwrap();
        let second_call: Value =
            serde_json::from_slice(&emissions[4].artifact.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(first_call.get("correlation_id"), Some(&json!("call-1")));
        assert_eq!(second_call.get("correlation_id"), Some(&json!("call-2")));
        assert_eq!(
            first_call.pointer("/provider_evidence/candidate/content/parts/3/thoughtSignature"),
            Some(&json!("c2ln"))
        );
    }

    #[test]
    fn max_tokens_materializes_partial_evidence_without_executing_tools() {
        let driver = driver();
        let response = json!({
            "candidates": [{
                "index": 0,
                "finishReason": "MAX_TOKENS",
                "content": {
                    "role": "model",
                    "parts": [
                        { "thought": true, "text": "partial reasoning" },
                        { "text": "partial answer" },
                        { "functionCall": { "id": "unsafe-partial", "name": "artifact-dread", "args": { "id": "one" } } }
                    ]
                }
            }],
            "usageMetadata": { "totalTokenCount": 42 },
            "responseId": "partial-response"
        });
        let bytes = serde_json::to_vec(&response).unwrap();
        let root = tempfile::tempdir().unwrap();
        let emissions = driver
            .materialize_response(
                GeminiResponseEnvelope {
                    response,
                    response_bytes: bytes.len(),
                    response_blake3: blake3::hash(&bytes).to_hex().to_string(),
                    stream_event_count: None,
                },
                root.path(),
                &BTreeSet::from(["artifact.read".to_string()]),
            )
            .unwrap();
        assert_eq!(emissions.len(), 3);
        assert!(emissions
            .iter()
            .all(|emission| !matches!(emission.route, ProviderEmissionRoute::Tool { .. })));
        let visible: Value =
            serde_json::from_slice(&emissions[2].artifact.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(visible.get("complete"), Some(&json!(false)));
        assert_eq!(visible.get("nonreplayable_terminal"), Some(&json!(true)));
        assert_eq!(
            visible.pointer("/suppressed_function_calls/0/id"),
            Some(&json!("unsafe-partial"))
        );
    }

    #[test]
    fn blocked_response_without_usage_is_a_typed_provider_outcome() {
        let driver = driver();
        let response = json!({
            "promptFeedback": {
                "blockReason": "SAFETY",
                "safetyRatings": []
            },
            "modelVersion": "gemini-test"
        });
        let bytes = serde_json::to_vec(&response).unwrap();
        let root = tempfile::tempdir().unwrap();
        let failure = match driver.materialize_response(
            GeminiResponseEnvelope {
                response,
                response_bytes: bytes.len(),
                response_blake3: blake3::hash(&bytes).to_hex().to_string(),
                stream_event_count: None,
            },
            root.path(),
            &BTreeSet::new(),
        ) {
            Ok(_) => panic!("blocked Gemini response unexpectedly materialized output"),
            Err(failure) => failure,
        };
        assert_eq!(failure.class, "response_outcome");
        assert_eq!(failure.code.as_deref(), Some("SAFETY"));
        assert_eq!(failure.response_bytes, Some(bytes.len()));
        assert!(failure.response_blake3.is_some());
    }

    #[test]
    fn response_function_call_must_be_in_the_exact_visible_tool_set() {
        let driver = driver();
        let response = json!({
            "candidates": [{
                "finishReason": "STOP",
                "content": {
                    "role": "model",
                    "parts": [{
                        "functionCall": {
                            "id": "call-foreign",
                            "name": "file-dwrite",
                            "args": { "path": "forbidden" }
                        }
                    }]
                }
            }],
            "responseId": "response-foreign"
        });
        let bytes = serde_json::to_vec(&response).unwrap();
        let failure = match driver.materialize_response(
            GeminiResponseEnvelope {
                response,
                response_bytes: bytes.len(),
                response_blake3: blake3::hash(&bytes).to_hex().to_string(),
                stream_event_count: None,
            },
            tempfile::tempdir().unwrap().path(),
            &BTreeSet::from(["artifact.read".to_string()]),
        ) {
            Ok(_) => panic!("ungranted Gemini function call unexpectedly materialized"),
            Err(failure) => failure,
        };
        assert!(failure
            .detail
            .contains("outside the model-visible affordance set"));
    }

    #[test]
    fn stream_aggregation_keeps_empty_signature_parts_and_distinct_parallel_calls() {
        let raw = b"data: {\"candidates\":[{\"index\":0,\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"hello\"}]}}]}\n\ndata: {\"candidates\":[{\"index\":0,\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"\",\"thoughtSignature\":\"c2ln\"},{\"functionCall\":{\"id\":\"one\",\"name\":\"tool\",\"args\":{}}},{\"functionCall\":{\"id\":\"two\",\"name\":\"tool\",\"args\":{}}}]},\"finishReason\":\"STOP\"}],\"responseId\":\"r\"}\n\n";
        let mut decoder = GeminiSseDecoder::new();
        for chunk in raw.chunks(13) {
            decoder.push(chunk).unwrap();
        }
        let (response, event_count) = decoder.finish().unwrap();
        assert_eq!(event_count, 2);
        assert_eq!(
            response.pointer("/candidates/0/content/parts/1/thoughtSignature"),
            Some(&json!("c2ln"))
        );
        assert_eq!(
            response.pointer("/candidates/0/content/parts/2/functionCall/id"),
            Some(&json!("one"))
        );
        assert_eq!(
            response.pointer("/candidates/0/content/parts/3/functionCall/id"),
            Some(&json!("two"))
        );
    }

    #[test]
    fn stream_clean_eof_without_finish_reason_is_not_a_completed_turn() {
        let raw = b"data: {\"candidates\":[{\"index\":0,\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"partial\"}]}}]}\n\n";
        let events = parse_sse_events(raw).unwrap();
        let failure = aggregate_stream_events(events, raw).unwrap_err();
        assert!(failure.detail.contains("terminal finishReason"));
    }

    #[test]
    fn parallel_function_calls_at_the_same_event_local_index_are_preserved() {
        let raw = b"data: {\"candidates\":[{\"index\":0,\"content\":{\"role\":\"model\",\"parts\":[{\"functionCall\":{\"id\":\"one\",\"name\":\"tool\",\"args\":{\"n\":1}}}]}}]}\n\ndata: {\"candidates\":[{\"index\":0,\"content\":{\"role\":\"model\",\"parts\":[{\"functionCall\":{\"id\":\"two\",\"name\":\"tool\",\"args\":{\"n\":2}}}]},\"finishReason\":\"STOP\"}]}\n\n";
        let response = aggregate_stream_events(parse_sse_events(raw).unwrap(), raw).unwrap();
        assert_eq!(
            response.pointer("/candidates/0/content/parts/0/functionCall/id"),
            Some(&json!("one"))
        );
        assert_eq!(
            response.pointer("/candidates/0/content/parts/1/functionCall/id"),
            Some(&json!("two"))
        );
    }

    #[test]
    fn error_artifact_records_typed_evidence_without_response_body() {
        let root = tempfile::tempdir().unwrap();
        let body =
            br#"{"error":{"code":429,"status":"RESOURCE_EXHAUSTED","message":"secret echo"}}"#;
        let failure = GeminiFailure::endpoint(429, body, true);
        let artifact = materialize_turn_error(root.path(), &failure, 3, 41).unwrap();
        let value: Value = serde_json::from_slice(&artifact.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(value.pointer("/error/status"), Some(&json!(429)));
        assert_eq!(
            value.pointer("/error/code"),
            Some(&json!("RESOURCE_EXHAUSTED"))
        );
        assert_eq!(value.pointer("/attempts"), Some(&json!(3)));
        assert!(!serde_json::to_string(&value)
            .unwrap()
            .contains("secret echo"));
    }
}
