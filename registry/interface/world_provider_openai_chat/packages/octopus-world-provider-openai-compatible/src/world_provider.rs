use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use octopus_descriptors::InterfaceId;
use octopus_interfaces::{
    InterfaceArtifactFile, InterfaceDescriptor, InterfaceInvocationEnvelope,
    RuntimeInvocationMetadata,
};
use octopus_ledger::ArtifactRef;
use octopus_model_context_render::{
    model_hidden_tool_names, model_visible_affordances,
    render_model_context_block_with_hidden_tools,
};
use octopus_provider_credential_pool::{
    CredentialFailure, CredentialPoolDecision, CredentialSelection,
};
use octopus_provider_run_authoring::{
    ProviderEmission, ProviderEmissionRoute, ProviderOutputHandoff,
};
use octopus_tool_handlers::ToolResultArtifact;
use serde_json::{json, Map, Value};

use crate::config::{
    validate_audio_data, validate_image_url, CredentialAttemptStart, InputModality,
    MessageContentMode, ModelCatalogConfig, ModelToolsMode, ProviderConfig, ToolMessageContentMode,
    MODELS_LIST_LABEL,
};
use crate::dispatch::DispatchAdmission;
use crate::wire::{
    execute_chat_completion, execute_model_catalog, refresh_azure_authentication,
    ProviderResponseEvidence,
};
pub use crate::wire::{ProviderDriverError, ProviderErrorEvidence};

pub const WORLD_PROVIDER_INTERFACE_DRIVER_REF: &str = "openai_compatible";
pub const INTERFACE_INPUT_LABEL: &str = "interface.input";

const INTERFACE_ARTIFACT_OUTPUT_LABEL: &str = "interface.artifact.output";
const TOOL_CALL_LABEL: &str = "tool.call";
const CONTEXT_REFERENCE_LABEL: &str = "context.reference";
const TURN_ERROR_LABEL: &str = "turn.error";
const TOOL_RESULTS_PENDING_LABEL: &str = "provider.tool_results.pending";
const SESSION_CONTEXT_FIELD: &str = "session_context";

pub(crate) enum PreparedProviderTurn {
    Local(Vec<ProviderEmission>),
    Network(PreparedNetworkTurn),
}

pub(crate) enum PreparedNetworkTurn {
    ModelCatalog { formed_models: BTreeSet<String> },
    ChatCompletion(Box<PreparedChatCompletion>),
}

pub(crate) struct PreparedChatCompletion {
    config: ProviderConfig,
    request_body: Map<String, Value>,
    model: String,
    messages: Vec<Value>,
    tools: Vec<Value>,
    model_visible_tool_names: BTreeSet<String>,
}

pub struct WorldProviderInterfaceDriver {
    interface_id: InterfaceId,
    config: ProviderConfig,
    private_config: Map<String, Value>,
    http: reqwest::Client,
}

impl WorldProviderInterfaceDriver {
    pub fn from_descriptor(descriptor: &InterfaceDescriptor) -> Result<Self, String> {
        Self::from_descriptor_with_roots(descriptor, None, None)
    }

    pub fn from_descriptor_with_secret_home(
        descriptor: &InterfaceDescriptor,
        secret_home: Option<&Path>,
    ) -> Result<Self, String> {
        Self::from_descriptor_with_roots(descriptor, secret_home, None)
    }

    pub fn from_descriptor_with_roots(
        descriptor: &InterfaceDescriptor,
        secret_home: Option<&Path>,
        interface_var_dir: Option<&Path>,
    ) -> Result<Self, String> {
        if descriptor.interface_driver_ref != WORLD_PROVIDER_INTERFACE_DRIVER_REF {
            return Err(format!(
                "provider interface '{}' resolves driver '{}', not '{}'",
                descriptor.id, descriptor.interface_driver_ref, WORLD_PROVIDER_INTERFACE_DRIVER_REF
            ));
        }
        let config = ProviderConfig::from_descriptor(descriptor, secret_home, interface_var_dir)?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(config.request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| format!("provider HTTP client build failed: {error}"))?;
        let private_config = descriptor
            .interface_driver_config
            .as_ref()
            .map(|config| config.private_config.clone())
            .ok_or_else(|| {
                format!(
                    "provider interface '{}' has no interface_driver_config",
                    descriptor.id
                )
            })?;
        Ok(Self {
            interface_id: descriptor.id.clone(),
            config,
            private_config,
            http,
        })
    }

    pub fn interface_driver_ref(&self) -> &str {
        WORLD_PROVIDER_INTERFACE_DRIVER_REF
    }

    pub fn validate_retry_budget(&self, max_attempts: u32) -> Result<(), String> {
        let available = self
            .config
            .retry
            .backoff_ms
            .len()
            .checked_add(1)
            .ok_or_else(|| "provider retry schedule length overflowed".to_string())?;
        let maximum = u32::try_from(available)
            .map_err(|_| "provider retry schedule exceeds u32".to_string())?;
        if max_attempts > maximum {
            return Err(format!(
                "formed max_attempts {max_attempts} exceeds descriptor retry schedule capacity {maximum}"
            ));
        }
        Ok(())
    }

    pub fn enforce_formed_model_policy(
        &self,
        input: &InterfaceInvocationEnvelope,
    ) -> Result<(), ProviderDriverError> {
        let manifest = input.read_artifact_manifest().map_err(|error| {
            ProviderDriverError::protocol(
                "input_artifact_manifest_invalid",
                format!("provider input artifact manifest cannot be read: {error}"),
            )
        })?;
        if manifest.label == MODELS_LIST_LABEL {
            return Ok(());
        }
        self.formed_model_config(input).map(|_| ())
    }

    pub fn retry_backoff(&self, failed_attempt: u32) -> Result<std::time::Duration, String> {
        let index = failed_attempt
            .checked_sub(1)
            .ok_or_else(|| "provider retry attempt must be positive".to_string())?;
        let index = usize::try_from(index)
            .map_err(|_| "provider retry attempt exceeds host width".to_string())?;
        let delay = self
            .config
            .retry
            .backoff_ms
            .get(index)
            .copied()
            .ok_or_else(|| {
                format!(
                    "provider descriptor has no retry backoff for failed attempt {failed_attempt}"
                )
            })?;
        Ok(std::time::Duration::from_millis(delay))
    }

    pub async fn refresh_authentication(&self) -> Result<(), ProviderDriverError> {
        refresh_azure_authentication(&self.config).await
    }

    pub fn select_credential_candidate(
        &self,
        selected: Option<&CredentialSelection>,
        now_unix_ms: u64,
    ) -> Result<Option<CredentialSelection>, ProviderDriverError> {
        let start = self
            .config
            .select_credential_candidate(selected, now_unix_ms)
            .map_err(|error| {
                ProviderDriverError::failed_closed("credential_pool_state_invalid", error)
            })?;
        credential_attempt_selection(start)
    }

    pub fn start_credential_attempt(
        &self,
        selected: Option<&CredentialSelection>,
        now_unix_ms: u64,
    ) -> Result<Option<CredentialSelection>, ProviderDriverError> {
        let start = self
            .config
            .start_credential_attempt(selected, now_unix_ms)
            .map_err(|error| {
                ProviderDriverError::failed_closed("credential_pool_state_invalid", error)
            })?;
        credential_attempt_selection(start)
    }

    pub fn record_credential_success(
        &self,
        selected: Option<&CredentialSelection>,
        now_unix_ms: u64,
    ) -> Result<(), ProviderDriverError> {
        self.config
            .record_credential_success(selected, now_unix_ms)
            .map_err(|error| {
                ProviderDriverError::failed_closed("credential_pool_state_invalid", error)
            })
    }

    pub fn credential_failure_decision(
        &self,
        selected: Option<&CredentialSelection>,
        error: &ProviderDriverError,
        now_unix_ms: u64,
    ) -> Result<Option<CredentialPoolDecision>, ProviderDriverError> {
        let Some(status) = error.evidence().http.as_ref().map(|http| http.status) else {
            return Ok(None);
        };
        if !self.config.credential_pool_manages_status(status) {
            return Ok(None);
        }
        self.config
            .record_credential_failure(
                selected,
                CredentialFailure {
                    status,
                    reason_code: error.evidence().code.clone(),
                    reset_at_unix_ms: retry_after_unix_ms(error.evidence(), now_unix_ms),
                },
                now_unix_ms,
            )
            .map_err(|error| {
                ProviderDriverError::failed_closed("credential_pool_state_invalid", error)
            })
    }

    pub async fn receive_artifact_and_materialize_outputs(
        &self,
        input: InterfaceInvocationEnvelope,
        source_movement_seq: u64,
        attempt: u32,
        credential: Option<&CredentialSelection>,
        handoff: &ProviderOutputHandoff,
        output_root: &Path,
    ) -> Result<Vec<ProviderEmission>, ProviderDriverError> {
        let callable_tools = handoff.callable_tool_names(&input)
            .map_err(|error| ProviderDriverError::failed_closed("provider_tool_topology_invalid", error))?;
        match self.prepare_artifact_and_materialize_local_outputs(input, output_root, &callable_tools)? {
            PreparedProviderTurn::Local(outputs) => Ok(outputs),
            PreparedProviderTurn::Network(prepared) => {
                self.execute_prepared_network_turn(
                    &prepared,
                    source_movement_seq,
                    attempt,
                    credential,
                    handoff,
                    output_root,
                )
                .await
            }
        }
    }

    pub(crate) fn prepare_artifact_and_materialize_local_outputs(
        &self,
        input: InterfaceInvocationEnvelope,
        output_root: &Path,
        callable_tools: &BTreeSet<String>,
    ) -> Result<PreparedProviderTurn, ProviderDriverError> {
        let request_bytes = input.read_artifact_payload().map_err(|error| {
            ProviderDriverError::protocol(
                "input_artifact_unreadable",
                format!("provider input artifact cannot be read: {error}"),
            )
        })?;
        let request: Value = serde_json::from_slice(&request_bytes).map_err(|error| {
            ProviderDriverError::protocol(
                "input_artifact_json_invalid",
                format!("provider input artifact is not valid JSON: {error}"),
            )
        })?;
        invocation_runtime_metadata(&input)?;
        let manifest = input.read_artifact_manifest().map_err(|error| {
            ProviderDriverError::protocol(
                "input_artifact_manifest_invalid",
                format!("provider input artifact manifest cannot be read: {error}"),
            )
        })?;
        if manifest.label == MODELS_LIST_LABEL {
            if !request.as_object().is_some_and(Map::is_empty) {
                return Err(ProviderDriverError::protocol(
                    "model_catalog_request_invalid",
                    "provider model catalog request payload must be an empty JSON object",
                ));
            }
            if self.config.model_catalog.is_none() {
                return Err(ProviderDriverError::failed_closed(
                    "model_catalog_unsupported",
                    "this provider descriptor declares no model catalog operation",
                ));
            }
            return Ok(PreparedProviderTurn::Network(
                PreparedNetworkTurn::ModelCatalog {
                    formed_models: required_formed_models(&input)?,
                },
            ));
        }
        let (model, model_config) = self.formed_model_config(&input)?;
        let request_body = model_config
            .resolve_request_body(&request)
            .map_err(|error| {
                ProviderDriverError::protocol("provider_request_options_invalid", error)
            })?;
        let hidden_tools = model_hidden_tool_names(&self.private_config)
            .map_err(|error| ProviderDriverError::protocol("hidden_tool_config_invalid", error))?;
        let hidden_tools = octopus_model_context_render::model_tool_exclusions(
            input.metadata().map_err(|error| ProviderDriverError::failed_closed("invocation_metadata_invalid", error.to_string()))?,
            callable_tools, &hidden_tools);
        let (tools, model_visible_tool_names, hidden_tools) =
            resolve_model_tool_projection(&input, hidden_tools, model_config.request.model_tools)?;
        let messages = match assemble_messages(
            &input,
            &request,
            &model_config,
            &hidden_tools,
            self.interface_id.as_str(),
        )? {
            MessageAssembly::Ready(messages) => messages,
            MessageAssembly::AwaitingToolResults(groups) => {
                return materialize_pending_tool_results(output_root, groups)
                    .map(PreparedProviderTurn::Local);
            }
        };
        Ok(PreparedProviderTurn::Network(
            PreparedNetworkTurn::ChatCompletion(Box::new(PreparedChatCompletion {
                config: model_config,
                request_body,
                model,
                messages,
                tools,
                model_visible_tool_names,
            })),
        ))
    }

    fn formed_model_config(
        &self,
        input: &InterfaceInvocationEnvelope,
    ) -> Result<(String, ProviderConfig), ProviderDriverError> {
        let model = self.config.resolve_provider_model().map_err(|error| {
            ProviderDriverError::failed_closed("provider_model_unavailable", error)
        })?;
        let models = required_formed_models(input)?;
        if !models.contains(&model) {
            return Err(ProviderDriverError::failed_closed(
                "provider_model_not_granted",
                format!(
                    "provider-selected model '{model}' is outside formed interface policy models"
                ),
            ));
        }
        let config = self.config.for_provider_model(&model).map_err(|error| {
            ProviderDriverError::failed_closed("provider_model_unavailable", error)
        })?;
        Ok((model, config))
    }

    pub(crate) async fn execute_prepared_network_turn(
        &self,
        prepared: &PreparedNetworkTurn,
        source_movement_seq: u64,
        attempt: u32,
        credential: Option<&CredentialSelection>,
        handoff: &ProviderOutputHandoff,
        output_root: &Path,
    ) -> Result<Vec<ProviderEmission>, ProviderDriverError> {
        let credential_handle_fingerprint = self
            .config
            .credential_handle_fingerprint(credential)
            .map_err(|error| {
            ProviderDriverError::failed_closed("credential_pool_selection_invalid", error)
        })?;
        let dispatch = DispatchAdmission {
            handoff,
            output_root,
            provider_interface: &self.interface_id,
            source_movement_seq,
            attempt,
            credential,
            credential_handle_fingerprint: credential_handle_fingerprint.as_deref(),
        };
        let prepared = match prepared {
            PreparedNetworkTurn::ModelCatalog { formed_models } => {
                return self
                    .materialize_model_catalog(output_root, &dispatch, formed_models)
                    .await;
            }
            PreparedNetworkTurn::ChatCompletion(prepared) => prepared,
        };
        let outcome = execute_chat_completion(
            &self.http,
            &prepared.config,
            &prepared.request_body,
            &prepared.model,
            &prepared.messages,
            &prepared.tools,
            &dispatch,
        )
        .await?;
        let translated = parse_chat_completion_into_neutral(
            &outcome.response,
            &outcome.evidence,
            &prepared.config,
            &prepared.model_visible_tool_names,
        )?;
        if translated.artifacts.is_empty() {
            return Err(ProviderDriverError::protocol(
                "provider_turn_empty",
                "provider translated a turn without emitted artifacts",
            ));
        }
        let mut emissions = Vec::with_capacity(translated.artifacts.len());
        for artifact in translated.artifacts {
            let output_bytes = serde_json::to_vec(&artifact.neutral_output).map_err(|error| {
                ProviderDriverError::protocol(
                    "output_artifact_encode_failed",
                    format!("provider output could not be encoded: {error}"),
                )
            })?;
            let file = InterfaceArtifactFile::write(output_root, &artifact.label, &output_bytes)
                .map_err(|error| {
                    ProviderDriverError::protocol(
                        "output_artifact_materialize_failed",
                        format!("provider output could not be materialized: {error}"),
                    )
                })?;
            emissions.push(ProviderEmission {
                artifact: file,
                route: artifact.route,
            });
        }
        Ok(emissions)
    }

    async fn materialize_model_catalog(
        &self,
        output_root: &Path,
        dispatch: &DispatchAdmission<'_>,
        formed_models: &BTreeSet<String>,
    ) -> Result<Vec<ProviderEmission>, ProviderDriverError> {
        let catalog = self.config.model_catalog.as_ref().ok_or_else(|| {
            ProviderDriverError::failed_closed(
                "model_catalog_unsupported",
                "this provider descriptor declares no model catalog operation",
            )
        })?;
        let outcome = execute_model_catalog(&self.http, &self.config, dispatch).await?;
        let data = filter_model_catalog(
            normalize_model_catalog(&outcome.response, catalog)?,
            catalog,
            formed_models,
        );
        let payload = serde_json::to_vec(&json!({
            "label": catalog.output_label,
            "models": data,
            "provider_response": outcome.response,
            "transport": outcome.evidence,
        }))
        .map_err(|error| {
            ProviderDriverError::protocol(
                "model_catalog_encode_failed",
                format!("provider model catalog output could not be encoded: {error}"),
            )
        })?;
        let artifact = InterfaceArtifactFile::write(output_root, &catalog.output_label, &payload)
            .map_err(|error| {
            ProviderDriverError::protocol(
                "model_catalog_materialize_failed",
                format!("provider model catalog output could not be materialized: {error}"),
            )
        })?;
        Ok(vec![ProviderEmission {
            artifact,
            route: ProviderEmissionRoute::ClientProjection,
        }])
    }
}

fn required_formed_models(
    input: &InterfaceInvocationEnvelope,
) -> Result<BTreeSet<String>, ProviderDriverError> {
    let policy = input
        .interface_policy()
        .map_err(|error| {
            ProviderDriverError::failed_closed("formed_model_policy_invalid", error.to_string())
        })?
        .ok_or_else(|| {
            ProviderDriverError::failed_closed(
                "formed_model_policy_missing",
                "provider invocation carries no formed interface policy",
            )
        })?;
    let values = policy
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ProviderDriverError::failed_closed(
                "formed_model_policy_invalid",
                "formed interface policy models must be a string array",
            )
        })?;
    if values.is_empty() {
        return Err(ProviderDriverError::failed_closed(
            "formed_model_policy_empty",
            "formed interface policy models is empty",
        ));
    }
    let mut models = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let model = value.as_str().filter(|model| {
            !model.is_empty() && model.trim() == *model && !model.contains(['\r', '\n', '\0'])
        });
        let Some(model) = model else {
            return Err(ProviderDriverError::failed_closed(
                "formed_model_policy_invalid",
                format!("formed interface policy models[{index}] is not a trimmed string"),
            ));
        };
        if !models.insert(model.to_string()) {
            return Err(ProviderDriverError::failed_closed(
                "formed_model_policy_invalid",
                format!("formed interface policy models repeats '{model}'"),
            ));
        }
    }
    Ok(models)
}

fn credential_attempt_selection(
    attempt: CredentialAttemptStart,
) -> Result<Option<CredentialSelection>, ProviderDriverError> {
    match attempt {
        CredentialAttemptStart::Unmanaged => Ok(None),
        CredentialAttemptStart::Selected(selected) => Ok(Some(selected)),
        CredentialAttemptStart::Exhausted {
            next_available_at_unix_ms,
        } => {
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
    }
}

fn retry_after_unix_ms(evidence: &ProviderErrorEvidence, now_unix_ms: u64) -> Option<u64> {
    evidence
        .http
        .as_ref()?
        .identifiers
        .get("retry-after")?
        .parse::<u64>()
        .ok()?
        .checked_mul(1_000)
        .and_then(|delay| now_unix_ms.checked_add(delay))
}

fn normalize_model_catalog(
    response: &Value,
    config: &ModelCatalogConfig,
) -> Result<Vec<Value>, ProviderDriverError> {
    let models = response
        .pointer(&config.models_pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ProviderDriverError::protocol(
                "model_catalog_models_missing",
                format!(
                    "provider model catalog response has no array at JSON pointer '{}'",
                    config.models_pointer
                ),
            )
        })?;
    let mut ids = BTreeSet::new();
    for (index, model) in models.iter().enumerate() {
        let id = model
            .pointer(&config.id_pointer)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty() && value.trim() == *value)
            .ok_or_else(|| {
                ProviderDriverError::protocol(
                    "model_catalog_id_missing",
                    format!(
                        "provider model catalog item[{index}] has no valid id at JSON pointer '{}'",
                        config.id_pointer
                    ),
                )
            })?;
        if !ids.insert(id) {
            return Err(ProviderDriverError::protocol(
                "model_catalog_id_duplicate",
                format!("provider model catalog repeats id '{id}'"),
            ));
        }
    }
    Ok(models.clone())
}

fn filter_model_catalog(
    models: Vec<Value>,
    config: &ModelCatalogConfig,
    formed_models: &BTreeSet<String>,
) -> Vec<Value> {
    models
        .into_iter()
        .filter(|model| {
            model
                .pointer(&config.id_pointer)
                .and_then(Value::as_str)
                .is_some_and(|id| formed_models.contains(id))
        })
        .collect()
}

pub fn materialize_provider_turn_error(
    output_root: &Path,
    evidence: &ProviderErrorEvidence,
    attempts: u32,
    source_movement_seq: u64,
) -> Result<InterfaceArtifactFile, ProviderDriverError> {
    if evidence.message.trim().is_empty() {
        return Err(ProviderDriverError::protocol(
            "turn_error_message_empty",
            "provider turn error message must be non-empty",
        ));
    }
    let payload = serde_json::to_vec(&json!({
        "label": TURN_ERROR_LABEL,
        "message": evidence.message,
        "error": evidence,
        "attempts": attempts,
        "source_movement_seq": source_movement_seq,
    }))
    .map_err(|error| {
        ProviderDriverError::protocol(
            "turn_error_encode_failed",
            format!("provider turn error artifact did not encode: {error}"),
        )
    })?;
    InterfaceArtifactFile::write(output_root, TURN_ERROR_LABEL, &payload).map_err(|error| {
        ProviderDriverError::protocol(
            "turn_error_materialize_failed",
            format!("provider turn error artifact did not materialize: {error}"),
        )
    })
}

fn materialize_pending_tool_results(
    output_root: &Path,
    groups: Vec<PendingToolGroup>,
) -> Result<Vec<ProviderEmission>, ProviderDriverError> {
    let groups = groups
        .into_iter()
        .map(|group| {
            json!({
                "provider_message_id": group.provider_message_id,
                "expected": group.expected,
                "received": group.received,
                "pending_correlation_ids": group.pending_correlation_ids,
            })
        })
        .collect::<Vec<_>>();
    let payload = serde_json::to_vec(&json!({
        "label": TOOL_RESULTS_PENDING_LABEL,
        "groups": groups,
    }))
    .map_err(|error| {
        ProviderDriverError::protocol(
            "tool_results_pending_encode_failed",
            format!("provider tool-result fan-in state could not be encoded: {error}"),
        )
    })?;
    let artifact = InterfaceArtifactFile::write(output_root, TOOL_RESULTS_PENDING_LABEL, &payload)
        .map_err(|error| {
            ProviderDriverError::protocol(
                "tool_results_pending_materialize_failed",
                format!("provider tool-result fan-in state could not be materialized: {error}"),
            )
        })?;
    Ok(vec![ProviderEmission {
        artifact,
        route: ProviderEmissionRoute::ClientProjection,
    }])
}

struct TranslatedTurn {
    artifacts: Vec<TranslatedArtifact>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FinishDisposition {
    Complete,
    NonreplayableTerminal,
}

struct TranslatedArtifact {
    label: String,
    neutral_output: Value,
    route: ProviderEmissionRoute,
}

struct ContextArtifact {
    id: String,
    label: Option<String>,
    value: Value,
    current: bool,
}

enum MessageAssembly {
    Ready(Vec<Value>),
    AwaitingToolResults(Vec<PendingToolGroup>),
}

struct PendingToolGroup {
    provider_message_id: String,
    expected: usize,
    received: usize,
    pending_correlation_ids: Vec<String>,
}

#[derive(Default)]
struct ProviderTurnContext {
    output_index: Option<usize>,
    calls: Vec<(String, usize)>,
    results: BTreeMap<String, usize>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionContext {
    schema_version: u32,
    provider_interface: InterfaceId,
    artifacts: Vec<SessionContextArtifact>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionContextArtifact {
    label: String,
    payload: Value,
    source_artifact: ArtifactRef,
    source_movement_seq: Option<u64>,
}

fn assemble_messages(
    input: &InterfaceInvocationEnvelope,
    current: &Value,
    config: &ProviderConfig,
    hidden_tools: &[String],
    interface_id: &str,
) -> Result<MessageAssembly, ProviderDriverError> {
    let mut artifacts = Vec::with_capacity(input.artifact_binding_payloads.len() + 1);
    let mut session_sources = BTreeSet::new();
    for bound in &input.artifact_binding_payloads {
        let bytes = bound.read_bytes().map_err(|error| {
            ProviderDriverError::protocol(
                "bound_artifact_unreadable",
                format!(
                    "bound artifact '{}' cannot be read: {error}",
                    bound.artifact.id.as_str()
                ),
            )
        })?;
        let mut value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            ProviderDriverError::protocol(
                "bound_artifact_json_invalid",
                format!(
                    "bound artifact '{}' is not JSON: {error}",
                    bound.artifact.id.as_str()
                ),
            )
        })?;
        if let Some(snapshot) = value.get(SESSION_CONTEXT_FIELD) {
            let snapshot: SessionContext =
                serde_json::from_value(snapshot.clone()).map_err(|error| {
                    ProviderDriverError::protocol(
                        "session_context_invalid",
                        format!(
                            "bound session context '{}' is malformed: {error}",
                            bound.artifact.id.as_str()
                        ),
                    )
                })?;
            if snapshot.schema_version != 2 {
                return Err(ProviderDriverError::protocol(
                    "session_context_version_unsupported",
                    format!(
                        "bound session context '{}' has unsupported schema_version {}",
                        bound.artifact.id.as_str(),
                        snapshot.schema_version
                    ),
                ));
            }
            if snapshot.provider_interface.as_str() != interface_id {
                return Err(ProviderDriverError::protocol(
                    "session_context_provider_mismatch",
                    format!(
                        "bound session context '{}' belongs to provider '{}' instead of '{}'",
                        bound.artifact.id.as_str(),
                        snapshot.provider_interface.as_str(),
                        interface_id
                    ),
                ));
            }
            for (index, item) in snapshot.artifacts.into_iter().enumerate() {
                if item.label.is_empty() || item.label.trim() != item.label {
                    return Err(ProviderDriverError::protocol(
                        "session_context_label_empty",
                        format!(
                            "bound session context '{}' artifact[{index}] has an invalid label",
                            bound.artifact.id.as_str()
                        ),
                    ));
                }
                let source_key = format!(
                    "{}:{}:{}",
                    item.source_artifact.id.as_str(),
                    item.source_artifact.content.to_hex(),
                    item.label
                );
                if !session_sources.insert(source_key) {
                    return Err(ProviderDriverError::protocol(
                        "session_context_source_duplicate",
                        format!(
                            "bound session context '{}' repeats an artifact source across the invocation at index {index}",
                            bound.artifact.id.as_str()
                        ),
                    ));
                }
                if item.payload.get(SESSION_CONTEXT_FIELD).is_some() {
                    return Err(ProviderDriverError::protocol(
                        "session_context_nested",
                        format!(
                            "bound session context '{}' artifact[{index}] nests the reserved carrier",
                            bound.artifact.id.as_str()
                        ),
                    ));
                }
                let _ = item.source_movement_seq;
                artifacts.push(ContextArtifact {
                    id: format!("{}#session:{index}", bound.artifact.id.as_str()),
                    label: Some(item.label),
                    value: item.payload,
                    current: false,
                });
            }
            let label = artifact_payload_label(bound.artifact.id.as_str(), &value)
                .filter(|label| !label.is_empty() && label.trim() == label)
                .ok_or_else(|| {
                    ProviderDriverError::protocol(
                        "session_context_outer_label_invalid",
                        format!(
                            "bound session context '{}' outer artifact has no valid label",
                            bound.artifact.id.as_str()
                        ),
                    )
                })?;
            value
                .as_object_mut()
                .ok_or_else(|| {
                    ProviderDriverError::protocol(
                        "session_context_outer_invalid",
                        format!(
                            "bound session context '{}' outer artifact is not an object",
                            bound.artifact.id.as_str()
                        ),
                    )
                })?
                .remove(SESSION_CONTEXT_FIELD)
                .ok_or_else(|| {
                    ProviderDriverError::protocol(
                        "session_context_outer_missing",
                        format!(
                            "bound session context '{}' lost its reserved carrier",
                            bound.artifact.id.as_str()
                        ),
                    )
                })?;
            let source_key = format!(
                "{}:{}:{}",
                bound.artifact.id.as_str(),
                bound.artifact.content.to_hex(),
                label
            );
            if !session_sources.insert(source_key) {
                return Err(ProviderDriverError::protocol(
                    "session_context_source_duplicate",
                    format!(
                        "bound session context '{}' repeats its outer artifact source",
                        bound.artifact.id.as_str()
                    ),
                ));
            }
            artifacts.push(ContextArtifact {
                id: bound.artifact.id.as_str().to_string(),
                label: Some(label),
                value,
                current: false,
            });
        } else {
            let label = artifact_payload_label(bound.artifact.id.as_str(), &value);
            if let Some(label) = label.as_deref() {
                let source_key = format!(
                    "{}:{}:{}",
                    bound.artifact.id.as_str(),
                    bound.artifact.content.to_hex(),
                    label
                );
                if !session_sources.insert(source_key) {
                    return Err(ProviderDriverError::protocol(
                        "session_context_source_duplicate",
                        format!(
                            "bound artifact '{}' repeats an artifact source across the invocation",
                            bound.artifact.id.as_str()
                        ),
                    ));
                }
            }
            artifacts.push(ContextArtifact {
                id: bound.artifact.id.as_str().to_string(),
                label,
                value,
                current: false,
            });
        }
    }
    let manifest = input.read_artifact_manifest().map_err(|error| {
        ProviderDriverError::protocol(
            "input_manifest_invalid",
            format!("provider input manifest is invalid: {error}"),
        )
    })?;
    artifacts.push(ContextArtifact {
        id: input.artifact.id.as_str().to_string(),
        label: Some(manifest.label),
        value: current.clone(),
        current: true,
    });

    let mut groups: BTreeMap<String, ProviderTurnContext> = BTreeMap::new();
    let mut calls: BTreeMap<String, (String, usize)> = BTreeMap::new();
    let mut results = BTreeMap::new();
    for (index, artifact) in artifacts.iter().enumerate() {
        if artifact.label.as_deref() == Some(INTERFACE_ARTIFACT_OUTPUT_LABEL) {
            let provider_message_id = required_trimmed_string(
                &artifact.value,
                "provider_message_id",
                INTERFACE_ARTIFACT_OUTPUT_LABEL,
            )?;
            let group = groups.entry(provider_message_id.clone()).or_default();
            if group.output_index.replace(index).is_some() {
                return Err(ProviderDriverError::protocol(
                    "provider_message_output_duplicate",
                    format!(
                        "provider context repeats assistant output for provider_message_id '{provider_message_id}'"
                    ),
                ));
            }
        } else if artifact.label.as_deref() == Some(TOOL_CALL_LABEL) {
            let correlation_id =
                required_trimmed_string(&artifact.value, "correlation_id", "tool.call")?;
            let provider_message_id =
                required_trimmed_string(&artifact.value, "provider_message_id", "tool.call")?;
            if calls
                .insert(correlation_id.clone(), (provider_message_id.clone(), index))
                .is_some()
            {
                return Err(ProviderDriverError::protocol(
                    "tool_call_correlation_duplicate",
                    format!("provider context repeats tool.call correlation_id '{correlation_id}'"),
                ));
            }
            groups
                .entry(provider_message_id)
                .or_default()
                .calls
                .push((correlation_id, index));
        } else if is_tool_result_artifact(artifact.label.as_deref(), &artifact.value) {
            let correlation_id =
                required_trimmed_string(&artifact.value, "correlation_id", "tool result")?;
            if results.insert(correlation_id.clone(), index).is_some() {
                return Err(ProviderDriverError::protocol(
                    "tool_result_correlation_duplicate",
                    format!(
                        "provider context repeats tool result correlation_id '{correlation_id}'"
                    ),
                ));
            }
        }
    }
    for (correlation_id, result_index) in &results {
        let Some((provider_message_id, _)) = calls.get(correlation_id) else {
            return Err(ProviderDriverError::protocol(
                "tool_result_call_missing",
                format!(
                    "tool result correlation_id '{correlation_id}' has no bound tool.call artifact"
                ),
            ));
        };
        let group = groups.get_mut(provider_message_id).ok_or_else(|| {
            ProviderDriverError::protocol(
                "provider_message_group_missing",
                format!(
                    "tool result correlation_id '{correlation_id}' resolved to a missing provider message group"
                ),
            )
        })?;
        group.results.insert(correlation_id.clone(), *result_index);
    }
    let mut pending = Vec::new();
    for (provider_message_id, group) in &groups {
        if group.calls.is_empty() || group.calls.len() == group.results.len() {
            continue;
        }
        let pending_correlation_ids = group
            .calls
            .iter()
            .filter(|(correlation_id, _)| !group.results.contains_key(correlation_id))
            .map(|(correlation_id, _)| correlation_id.clone())
            .collect::<Vec<_>>();
        pending.push(PendingToolGroup {
            provider_message_id: provider_message_id.clone(),
            expected: group.calls.len(),
            received: group.results.len(),
            pending_correlation_ids,
        });
    }
    if !pending.is_empty() {
        let current_is_tool_result = artifacts.last().is_some_and(|artifact| {
            is_tool_result_artifact(artifact.label.as_deref(), &artifact.value)
        });
        if !current_is_tool_result {
            return Err(ProviderDriverError::failed_closed(
                "tool_result_group_incomplete",
                "provider context contains an unresolved parallel tool-call group but the current artifact is not a tool result",
            ));
        }
        return Ok(MessageAssembly::AwaitingToolResults(pending));
    }
    let mut groups_by_first = BTreeMap::new();
    let mut grouped_indexes = BTreeSet::new();
    for (provider_message_id, group) in &mut groups {
        group.calls.sort_by_key(|(_, call_index)| *call_index);
        let first = group
            .output_index
            .into_iter()
            .chain(group.calls.iter().map(|(_, call_index)| *call_index))
            .min()
            .ok_or_else(|| {
                ProviderDriverError::protocol(
                    "provider_message_group_empty",
                    format!("provider_message_id '{provider_message_id}' has no replay artifacts"),
                )
            })?;
        if groups_by_first
            .insert(first, provider_message_id.clone())
            .is_some()
        {
            return Err(ProviderDriverError::protocol(
                "provider_message_order_ambiguous",
                "provider message groups share their first artifact position",
            ));
        }
        if let Some(output_index) = group.output_index {
            grouped_indexes.insert(output_index);
        }
        for (correlation_id, call_index) in &group.calls {
            grouped_indexes.insert(*call_index);
            let result_index = group.results.get(correlation_id).ok_or_else(|| {
                ProviderDriverError::protocol(
                    "tool_result_group_incomplete",
                    format!("tool call '{correlation_id}' lost its completed result"),
                )
            })?;
            grouped_indexes.insert(*result_index);
        }
    }

    let mut system_sections = Vec::new();
    let mut conversation = Vec::new();
    for (index, artifact) in artifacts.iter().enumerate() {
        if artifact.label.as_deref() == Some(CONTEXT_REFERENCE_LABEL) {
            let text = artifact
                .value
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())
                .ok_or_else(|| {
                    ProviderDriverError::protocol(
                        "context_reference_text_missing",
                        format!(
                            "context.reference artifact '{}' has no non-empty text",
                            artifact.id
                        ),
                    )
                })?;
            system_sections.push(text.to_string());
            continue;
        }
        if let Some(provider_message_id) = groups_by_first.get(&index) {
            let group = groups.get(provider_message_id).ok_or_else(|| {
                ProviderDriverError::protocol(
                    "provider_message_group_missing",
                    format!(
                        "ordered provider_message_id '{provider_message_id}' has no context group"
                    ),
                )
            })?;
            conversation.push(provider_turn_message(group, &artifacts)?);
            for (correlation_id, _) in &group.calls {
                let result_index = group.results.get(correlation_id).ok_or_else(|| {
                    ProviderDriverError::protocol(
                        "tool_result_group_incomplete",
                        format!(
                            "provider message group lost tool result correlation_id '{correlation_id}'"
                        ),
                    )
                })?;
                conversation.push(tool_result_message(
                    &artifacts[*result_index].value,
                    config,
                )?);
            }
            continue;
        }
        if grouped_indexes.contains(&index)
            || artifact.label.as_deref() == Some(TOOL_CALL_LABEL)
            || is_tool_result_artifact(artifact.label.as_deref(), &artifact.value)
            || artifact.label.as_deref() == Some(TOOL_RESULTS_PENDING_LABEL)
        {
            continue;
        }
        if let Some(message) = model_message_from_artifact(
            artifact.label.as_deref(),
            &artifact.value,
            artifact.current,
            config,
        )? {
            conversation.push(message);
        } else if !artifact.current {
            system_sections.push(format!(
                "Bound Octopus artifact {}:\n{}",
                artifact.id,
                render_json(&artifact.value)?
            ));
        }
    }
    if system_sections.is_empty() {
        return Err(ProviderDriverError::failed_closed(
            "worker_context_missing",
            "provider worker context is missing: bind at least one context.reference artifact",
        ));
    }
    if let Some(metadata) = input
        .metadata
        .as_ref()
        .and_then(|metadata| render_model_context_block_with_hidden_tools(metadata, hidden_tools))
    {
        system_sections.push(metadata);
    }
    let system_text = system_sections.join("\n\n");
    let system_content = match config.request.message_content {
        MessageContentMode::String => Value::String(system_text),
        MessageContentMode::Parts => {
            let mut part = config.request.system_content_part_fields.clone();
            part.insert("type".to_string(), Value::String("text".to_string()));
            part.insert("text".to_string(), Value::String(system_text));
            Value::Array(vec![Value::Object(part)])
        }
    };
    let mut messages = Vec::with_capacity(conversation.len() + 1);
    messages.push(json!({ "role": "system", "content": system_content }));
    messages.extend(conversation);
    Ok(MessageAssembly::Ready(messages))
}

fn model_message_from_artifact(
    label: Option<&str>,
    value: &Value,
    current: bool,
    config: &ProviderConfig,
) -> Result<Option<Value>, ProviderDriverError> {
    if label == Some(INTERFACE_ARTIFACT_OUTPUT_LABEL) {
        let content = value
            .get("content")
            .cloned()
            .or_else(|| value.get("text").cloned())
            .ok_or_else(|| {
                ProviderDriverError::protocol(
                    "provider_output_content_missing",
                    "provider output context artifact has no content or text",
                )
            })?;
        let mut message = json!({ "role": "assistant", "content": content });
        apply_replay_fields(&mut message, value)?;
        return Ok(Some(message));
    }
    if label == Some(TURN_ERROR_LABEL)
        || label == Some("operator.approval.request")
        || label == Some("operator.elicitation.request")
    {
        return Ok(Some(json!({
            "role": "assistant",
            "content": render_json(value)?
        })));
    }
    if current
        && matches!(
            label,
            Some("operator.approval.response")
                | Some("operator.elicitation.response")
                | Some("operator.user_input.response")
        )
    {
        return Ok(Some(json!({
            "role": "user",
            "content": render_json(value)?
        })));
    }
    if current {
        return Ok(Some(json!({
            "role": "user",
            "content": user_content(value, config)?
        })));
    }
    if let Some(prompt) = value
        .get("prompt")
        .and_then(Value::as_str)
        .filter(|prompt| !prompt.trim().is_empty())
    {
        return Ok(Some(json!({ "role": "user", "content": prompt })));
    }
    if let Some(task) = value.get("task") {
        return Ok(Some(json!({
            "role": "user",
            "content": match task {
                Value::String(task) => task.clone(),
                other => render_json(other)?,
            }
        })));
    }
    Ok(None)
}

fn user_content(value: &Value, config: &ProviderConfig) -> Result<Value, ProviderDriverError> {
    if let Some(items) = value.get("input_items") {
        return normalize_input_items(items, config);
    }
    if let Some(content) = value.get("content") {
        return normalize_wire_content(content, config);
    }
    if let Some(prompt) = value
        .get("prompt")
        .and_then(Value::as_str)
        .filter(|prompt| !prompt.trim().is_empty())
    {
        return Ok(match config.request.message_content {
            MessageContentMode::String => Value::String(prompt.to_string()),
            MessageContentMode::Parts => {
                Value::Array(vec![json!({ "type": "text", "text": prompt })])
            }
        });
    }
    if let Some(task) = value.get("task") {
        let text = match task {
            Value::String(task) => task.clone(),
            other => render_json(other)?,
        };
        return Ok(Value::String(text));
    }
    Ok(Value::String(render_json(value)?))
}

fn normalize_input_items(
    value: &Value,
    config: &ProviderConfig,
) -> Result<Value, ProviderDriverError> {
    let items = value.as_array().ok_or_else(|| {
        ProviderDriverError::protocol("input_items_invalid", "input_items must be an array")
    })?;
    let mut parts = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let item_type = item.get("type").and_then(Value::as_str).ok_or_else(|| {
            ProviderDriverError::protocol(
                "input_item_type_missing",
                format!("input_items[{index}] has no string type"),
            )
        })?;
        match item_type {
            "text" => {
                require_modality(config, InputModality::Text, "text")?;
                let text = item.get("text").and_then(Value::as_str).ok_or_else(|| {
                    ProviderDriverError::protocol(
                        "input_text_missing",
                        format!("input_items[{index}] text item has no string text"),
                    )
                })?;
                if !text.is_empty() {
                    parts.push(json!({ "type": "text", "text": text }));
                }
            }
            "image" => {
                require_modality(config, InputModality::ImageUrl, "image_url")?;
                let url = item.get("url").and_then(Value::as_str).ok_or_else(|| {
                    ProviderDriverError::protocol(
                        "input_image_url_missing",
                        format!("input_items[{index}] image item has no string url"),
                    )
                })?;
                validate_image_url(url).map_err(|error| {
                    ProviderDriverError::protocol("input_image_url_invalid", error)
                })?;
                let mut image = json!({ "url": url });
                if let Some(detail) = item.get("detail").filter(|value| !value.is_null()) {
                    image["detail"] = detail.clone();
                }
                parts.push(json!({ "type": "image_url", "image_url": image }));
            }
            "localImage" | "local_image" => {
                return Err(ProviderDriverError::failed_closed(
                    "local_image_not_artifact_native",
                    format!(
                        "input_items[{index}] carries a local path; materialize image bytes as a data URL or HTTPS artifact before crossing the provider interface"
                    ),
                ));
            }
            "inputAudio" | "input_audio" | "audio" => {
                require_modality(config, InputModality::InputAudio, "input_audio")?;
                let source = item.get("input_audio").unwrap_or(item);
                let data = source.get("data").and_then(Value::as_str).ok_or_else(|| {
                    ProviderDriverError::protocol(
                        "input_audio_data_missing",
                        format!("input_items[{index}] audio item has no string data"),
                    )
                })?;
                let format = source
                    .get("format")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        ProviderDriverError::protocol(
                            "input_audio_format_missing",
                            format!("input_items[{index}] audio item has no string format"),
                        )
                    })?;
                validate_audio_data(data, format)
                    .map_err(|error| ProviderDriverError::protocol("input_audio_invalid", error))?;
                parts.push(json!({
                    "type": "input_audio",
                    "input_audio": { "data": data, "format": format }
                }));
            }
            _ => parts.push(json!({
                "type": "text",
                "text": render_json(item)?
            })),
        }
    }
    if parts.is_empty() {
        return Err(ProviderDriverError::protocol(
            "input_items_empty",
            "input_items produced no model content parts",
        ));
    }
    Ok(match config.request.message_content {
        MessageContentMode::Parts => Value::Array(parts),
        MessageContentMode::String if parts.iter().all(|part| part["type"] == "text") => {
            Value::String(
                parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        }
        MessageContentMode::String => Value::Array(parts),
    })
}

fn normalize_wire_content(
    value: &Value,
    config: &ProviderConfig,
) -> Result<Value, ProviderDriverError> {
    match value {
        Value::String(text) => {
            require_modality(config, InputModality::Text, "text")?;
            Ok(Value::String(text.clone()))
        }
        Value::Array(parts) => {
            let mut normalized = Vec::with_capacity(parts.len());
            for (index, part) in parts.iter().enumerate() {
                let kind = part.get("type").and_then(Value::as_str).ok_or_else(|| {
                    ProviderDriverError::protocol(
                        "content_part_type_missing",
                        format!("content[{index}] has no string type"),
                    )
                })?;
                match kind {
                    "text" => {
                        require_modality(config, InputModality::Text, "text")?;
                        let text = part.get("text").and_then(Value::as_str).ok_or_else(|| {
                            ProviderDriverError::protocol(
                                "content_text_missing",
                                format!("content[{index}] text part has no string text"),
                            )
                        })?;
                        normalized.push(json!({ "type": "text", "text": text }));
                    }
                    "image_url" => {
                        require_modality(config, InputModality::ImageUrl, "image_url")?;
                        let image = part.get("image_url").ok_or_else(|| {
                            ProviderDriverError::protocol(
                                "content_image_url_missing",
                                format!("content[{index}] image_url part has no image_url"),
                            )
                        })?;
                        let url = image
                            .as_str()
                            .or_else(|| image.get("url").and_then(Value::as_str))
                            .ok_or_else(|| {
                                ProviderDriverError::protocol(
                                    "content_image_url_invalid",
                                    format!("content[{index}] image_url has no string URL"),
                                )
                            })?;
                        validate_image_url(url).map_err(|error| {
                            ProviderDriverError::protocol("content_image_url_invalid", error)
                        })?;
                        normalized.push(part.clone());
                    }
                    "input_audio" => {
                        require_modality(config, InputModality::InputAudio, "input_audio")?;
                        let audio = part.get("input_audio").ok_or_else(|| {
                            ProviderDriverError::protocol(
                                "content_input_audio_missing",
                                format!("content[{index}] input_audio part has no input_audio"),
                            )
                        })?;
                        let data = audio.get("data").and_then(Value::as_str).ok_or_else(|| {
                            ProviderDriverError::protocol(
                                "content_input_audio_data_missing",
                                format!("content[{index}] input_audio has no string data"),
                            )
                        })?;
                        let format =
                            audio.get("format").and_then(Value::as_str).ok_or_else(|| {
                                ProviderDriverError::protocol(
                                    "content_input_audio_format_missing",
                                    format!("content[{index}] input_audio has no string format"),
                                )
                            })?;
                        validate_audio_data(data, format).map_err(|error| {
                            ProviderDriverError::protocol("content_input_audio_invalid", error)
                        })?;
                        normalized.push(part.clone());
                    }
                    other => {
                        return Err(ProviderDriverError::protocol(
                            "content_part_unsupported",
                            format!("content[{index}] type '{other}' is unsupported"),
                        ));
                    }
                }
            }
            Ok(Value::Array(normalized))
        }
        _ => Err(ProviderDriverError::protocol(
            "content_shape_invalid",
            "content must be a string or array",
        )),
    }
}

fn provider_turn_message(
    group: &ProviderTurnContext,
    artifacts: &[ContextArtifact],
) -> Result<Value, ProviderDriverError> {
    let mut indexes = group
        .output_index
        .into_iter()
        .chain(group.calls.iter().map(|(_, call_index)| *call_index));
    let representative_index = indexes.next().ok_or_else(|| {
        ProviderDriverError::protocol(
            "provider_message_group_empty",
            "provider message group has no output or tool-call artifact",
        )
    })?;
    let representative = &artifacts[representative_index].value;
    let provider_context = representative
        .get("provider_context")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            ProviderDriverError::protocol(
                "provider_replay_context_missing",
                "provider message artifact has no provider_context object",
            )
        })?;
    let replay_content = provider_context
        .get("content")
        .cloned()
        .unwrap_or(Value::Null);
    let replay_fields = provider_context
        .get("replay")
        .cloned()
        .unwrap_or_else(|| json!({}));
    for index in indexes {
        let context = artifacts[index]
            .value
            .get("provider_context")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                ProviderDriverError::protocol(
                    "provider_replay_context_missing",
                    "provider message artifact has no provider_context object",
                )
            })?;
        if context.get("content").cloned().unwrap_or(Value::Null) != replay_content
            || context.get("replay").cloned().unwrap_or_else(|| json!({})) != replay_fields
        {
            return Err(ProviderDriverError::protocol(
                "parallel_tool_context_mismatch",
                "provider message artifacts disagree about their wire replay context",
            ));
        }
    }
    let content = if group.calls.is_empty() && replay_content.is_null() {
        group
            .output_index
            .and_then(|index| artifacts[index].value.get("content").cloned())
            .unwrap_or(Value::Null)
    } else {
        replay_content
    };
    let mut calls = Vec::with_capacity(group.calls.len());
    for (_, call_index) in &group.calls {
        let value = &artifacts[*call_index].value;
        let tool = required_trimmed_string(value, "tool", "tool.call")?;
        let correlation_id = required_trimmed_string(value, "correlation_id", "tool.call")?;
        let arguments = value.get("arguments").cloned().unwrap_or_else(|| json!({}));
        let arguments = serde_json::to_string(&arguments).map_err(|error| {
            ProviderDriverError::protocol(
                "tool_arguments_encode_failed",
                format!("tool.call arguments could not be encoded: {error}"),
            )
        })?;
        calls.push(json!({
            "id": correlation_id,
            "type": "function",
            "function": {
                "name": encode_tool_name(&tool),
                "arguments": arguments
            }
        }));
    }
    let mut message = json!({ "role": "assistant", "content": content });
    if !calls.is_empty() {
        message["tool_calls"] = Value::Array(calls);
    }
    apply_replay_fields(&mut message, representative)?;
    Ok(message)
}

fn tool_result_message(
    value: &Value,
    config: &ProviderConfig,
) -> Result<Value, ProviderDriverError> {
    let result: ToolResultArtifact = serde_json::from_value(value.clone()).map_err(|error| {
        ProviderDriverError::protocol(
            "tool_result_malformed",
            format!("tool result is not the strict shared envelope: {error}"),
        )
    })?;
    let correlation_id = result.correlation_id.as_deref().ok_or_else(|| {
        ProviderDriverError::protocol(
            "tool_result_correlation_missing",
            "tool result has no correlation_id",
        )
    })?;
    let content = if result.ok {
        let body = result.body.as_ref().ok_or_else(|| {
            ProviderDriverError::protocol(
                "tool_result_body_missing",
                "successful tool result has no body",
            )
        })?;
        if let Some(content) = body.get("content") {
            normalize_wire_content(content, config)?
        } else {
            Value::String(render_json(body)?)
        }
    } else {
        let error = serde_json::to_value(result.error.as_ref().ok_or_else(|| {
            ProviderDriverError::protocol(
                "tool_result_error_missing",
                "failed tool result has no error",
            )
        })?)
        .map_err(|error| {
            ProviderDriverError::protocol(
                "tool_result_error_encode_failed",
                format!("tool result error could not be rendered: {error}"),
            )
        })?;
        Value::String(render_json(&error)?)
    };
    let content = match config.request.tool_message_content {
        ToolMessageContentMode::Preserve => content,
        ToolMessageContentMode::TextOnly => tool_content_as_text(content)?,
    };
    Ok(json!({
        "role": "tool",
        "tool_call_id": correlation_id,
        "content": content
    }))
}

fn tool_content_as_text(content: Value) -> Result<Value, ProviderDriverError> {
    let Value::Array(parts) = content else {
        return match content {
            Value::String(_) => Ok(content),
            other => Ok(Value::String(render_json(&other)?)),
        };
    };
    let mut text = Vec::new();
    for part in parts {
        let rendered = match &part {
            Value::String(value) => Some(value.clone()),
            Value::Object(value) => {
                let part_type = value.get("type").and_then(Value::as_str);
                if matches!(part_type, Some("text" | "input_text")) {
                    value
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                } else {
                    None
                }
            }
            _ => None,
        };
        text.push(match rendered {
            Some(rendered) => rendered,
            None => render_json(&part)?,
        });
    }
    Ok(Value::String(text.join("\n\n")))
}

fn parse_chat_completion_into_neutral(
    response: &Value,
    transport: &ProviderResponseEvidence,
    config: &ProviderConfig,
    model_visible_tool_names: &BTreeSet<String>,
) -> Result<TranslatedTurn, ProviderDriverError> {
    let choices = response
        .get("choices")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ProviderDriverError::protocol(
                "response_choices_missing",
                "chat/completions response has no choices array",
            )
        })?;
    if choices.len() != 1 {
        return Err(ProviderDriverError::protocol(
            "response_choice_count_invalid",
            format!(
                "chat/completions response must carry exactly one choice, got {}",
                choices.len()
            ),
        ));
    }
    let choice = &choices[0];
    let finish_disposition = classify_finish_reason(choice.get("finish_reason"), &config.response)?;
    let complete = finish_disposition == FinishDisposition::Complete;
    let nonreplayable_terminal = finish_disposition == FinishDisposition::NonreplayableTerminal;
    let message = choice
        .get("message")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            ProviderDriverError::protocol(
                "response_message_missing",
                "chat/completions response has no choices[0].message object",
            )
        })?;
    let content = normalize_response_content(message.get("content"))?;
    let visible_text = visible_response_text(content.as_ref());
    let refusal = message
        .get("refusal")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    let mut replay = Map::new();
    let mut reasoning = Map::new();
    for field in &config.response.reasoning_fields {
        if let Some(value) = message.get(field).filter(|value| !value.is_null()) {
            replay.insert(field.clone(), value.clone());
            reasoning.insert(field.clone(), value.clone());
        }
    }
    let mut reasoning_details = Map::new();
    for field in &config.response.reasoning_detail_fields {
        if let Some(value) = message.get(field).filter(|value| !value.is_null()) {
            replay.insert(field.clone(), value.clone());
            reasoning_details.insert(field.clone(), value.clone());
        }
    }
    let response_id = response
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            let bytes = serde_json::to_vec(&Value::Object(message.clone())).unwrap_or_default();
            format!("message:{}", blake3::hash(&bytes).to_hex())
        });
    let response_meta = json!({
        "id": response.get("id").cloned().unwrap_or(Value::Null),
        "model": response.get("model").cloned().unwrap_or(Value::Null),
        "created": response.get("created").cloned().unwrap_or(Value::Null),
        "finish_reason": choice.get("finish_reason").cloned().unwrap_or(Value::Null),
    });
    let usage = normalize_usage(response.get("usage"))?;
    let mut provider_context = json!({
        "content": content.clone().unwrap_or(Value::Null),
        "replay": replay,
        "reasoning": reasoning,
        "reasoning_details": reasoning_details,
        "response": response_meta,
        "usage": usage,
        "transport": transport,
        "complete": complete,
        "nonreplayable_terminal": nonreplayable_terminal,
    });
    if nonreplayable_terminal {
        provider_context
            .as_object_mut()
            .expect("provider context is an object")
            .insert("accepted_terminal_response".to_string(), response.clone());
    }
    let mut artifacts = Vec::new();
    if content.is_some() || refusal.is_some() || nonreplayable_terminal {
        let text = visible_text
            .filter(|value| !value.is_empty())
            .or_else(|| refusal.clone())
            .unwrap_or_else(|| {
                if nonreplayable_terminal {
                    String::new()
                } else {
                    render_json(content.as_ref().unwrap_or(&Value::Null)).unwrap_or_default()
                }
            });
        artifacts.push(TranslatedArtifact {
            label: INTERFACE_ARTIFACT_OUTPUT_LABEL.to_string(),
            neutral_output: json!({
                "label": INTERFACE_ARTIFACT_OUTPUT_LABEL,
                "text": text,
                "content": content.unwrap_or_else(|| Value::String(refusal.clone().unwrap_or_default())),
                "refusal": refusal,
                "provider_message_id": response_id.clone(),
                "provider_context": provider_context,
                "complete": complete,
                "nonreplayable_terminal": nonreplayable_terminal,
                "partial_tool_calls_routed": if nonreplayable_terminal { Some(false) } else { None },
            }),
            route: ProviderEmissionRoute::ClientProjection,
        });
    }
    if nonreplayable_terminal {
        return Ok(TranslatedTurn { artifacts });
    }
    if let Some(tool_calls) = message.get("tool_calls") {
        let tool_calls = tool_calls.as_array().ok_or_else(|| {
            ProviderDriverError::protocol(
                "response_tool_calls_invalid",
                "chat/completions message.tool_calls must be an array",
            )
        })?;
        if tool_calls.is_empty() {
            return Err(ProviderDriverError::protocol(
                "response_tool_calls_empty",
                "chat/completions returned an empty tool_calls array",
            ));
        }
        let mut ids = BTreeSet::new();
        for (index, call) in tool_calls.iter().enumerate() {
            let call_id = call
                .get("id")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    ProviderDriverError::protocol(
                        "response_tool_call_id_missing",
                        format!("tool_calls[{index}] has no non-empty id"),
                    )
                })?;
            if !ids.insert(call_id) {
                return Err(ProviderDriverError::protocol(
                    "response_tool_call_id_duplicate",
                    format!("tool call id '{call_id}' is duplicated"),
                ));
            }
            let call_type = call
                .get("type")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    ProviderDriverError::protocol(
                        "response_tool_call_type_missing",
                        format!("tool_calls[{index}] has no non-empty type"),
                    )
                })?;
            if call_type != "function" {
                return Err(ProviderDriverError::protocol(
                    "response_tool_call_type_unsupported",
                    format!("tool_calls[{index}] type '{call_type}' is unsupported"),
                ));
            }
            let function = call
                .get("function")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    ProviderDriverError::protocol(
                        "response_tool_function_missing",
                        format!("tool_calls[{index}] has no function object"),
                    )
                })?;
            let encoded_name = function
                .get("name")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    ProviderDriverError::protocol(
                        "response_tool_name_missing",
                        format!("tool_calls[{index}].function has no non-empty name"),
                    )
                })?;
            let tool = decode_tool_name(encoded_name)?;
            if !model_visible_tool_names.contains(&tool) {
                return Err(ProviderDriverError::failed_closed(
                    "response_tool_not_model_visible",
                    format!(
                        "chat/completions returned tool '{tool}' outside the model-visible affordance set"
                    ),
                ));
            }
            let arguments = match function.get("arguments") {
                None | Some(Value::Null) => json!({}),
                Some(Value::String(arguments)) if arguments.trim().is_empty() => json!({}),
                Some(Value::String(arguments)) => {
                    serde_json::from_str(arguments).map_err(|error| {
                        ProviderDriverError::protocol(
                            "response_tool_arguments_invalid",
                            format!("tool_calls[{index}] arguments are not valid JSON: {error}"),
                        )
                    })?
                }
                Some(Value::Object(arguments)) => Value::Object(arguments.clone()),
                Some(_) => {
                    return Err(ProviderDriverError::protocol(
                        "response_tool_arguments_shape_invalid",
                        format!("tool_calls[{index}] arguments must be a JSON string or object"),
                    ));
                }
            };
            artifacts.push(TranslatedArtifact {
                label: TOOL_CALL_LABEL.to_string(),
                neutral_output: json!({
                    "label": TOOL_CALL_LABEL,
                    "tool": tool.clone(),
                    "arguments": arguments,
                    "correlation_id": call_id,
                    "provider_message_id": response_id.clone(),
                    "provider_context": provider_context,
                }),
                route: ProviderEmissionRoute::Tool { tool },
            });
        }
    }
    for extra in translate_top_level_artifacts(
        response,
        transport,
        &response_id,
        &response_meta,
        &config.response,
    )? {
        artifacts.push(extra);
    }
    if artifacts.is_empty() {
        return Err(ProviderDriverError::failed_closed(
            "response_payload_empty",
            "chat/completions returned neither usable content, refusal, nor tool calls",
        ));
    }
    Ok(TranslatedTurn { artifacts })
}

fn translate_top_level_artifacts(
    response: &Value,
    transport: &ProviderResponseEvidence,
    response_id: &str,
    response_meta: &Value,
    config: &crate::config::ResponseConfig,
) -> Result<Vec<TranslatedArtifact>, ProviderDriverError> {
    let mut artifacts = Vec::with_capacity(config.top_level_artifacts.len());
    for declared in &config.top_level_artifacts {
        let Some(raw) = response.get(&declared.field) else {
            if declared.required {
                return Err(ProviderDriverError::protocol(
                    "response_top_level_field_missing",
                    format!(
                        "chat/completions response has no descriptor-required top-level field '{}'",
                        declared.field
                    ),
                ));
            }
            continue;
        };
        if !declared.accepts(raw) {
            return Err(ProviderDriverError::protocol(
                "response_top_level_field_shape_invalid",
                format!(
                    "chat/completions top-level field '{}' does not match its descriptor-declared JSON value types",
                    declared.field
                ),
            ));
        }
        let raw_bytes = serde_json::to_vec(raw).map_err(|error| {
            ProviderDriverError::protocol(
                "response_top_level_field_encode_failed",
                format!(
                    "chat/completions top-level field '{}' could not be encoded for evidence: {error}",
                    declared.field
                ),
            )
        })?;
        artifacts.push(TranslatedArtifact {
            label: declared.artifact_label.clone(),
            neutral_output: json!({
                "label": declared.artifact_label,
                "provider_message_id": response_id,
                "provider_field": declared.field,
                "raw": raw,
                "raw_evidence": {
                    "json_bytes": raw_bytes.len(),
                    "json_blake3": blake3::hash(&raw_bytes).to_hex().to_string(),
                    "response_body": transport.body,
                    "stream": transport.stream,
                },
                "provider_context": {
                    "response": response_meta,
                    "transport": transport,
                }
            }),
            route: ProviderEmissionRoute::ClientProjection,
        });
    }
    Ok(artifacts)
}

fn classify_finish_reason(
    value: Option<&Value>,
    config: &crate::config::ResponseConfig,
) -> Result<FinishDisposition, ProviderDriverError> {
    let reason = value
        .and_then(Value::as_str)
        .filter(|reason| !reason.is_empty() && reason.trim() == *reason)
        .ok_or_else(|| {
            ProviderDriverError::protocol(
                "response_finish_reason_missing",
                "chat/completions response has no non-empty terminal finish_reason",
            )
        })?;
    if config.successful_finish_reasons.contains(reason) {
        return Ok(FinishDisposition::Complete);
    }
    if config.nonreplayable_finish_reasons.contains(reason) {
        return Ok(FinishDisposition::NonreplayableTerminal);
    }
    Err(ProviderDriverError::failed_closed(
        "response_finish_reason_unsupported",
        format!("chat/completions response ended with undeclared finish_reason '{reason}'"),
    ))
}

fn normalize_response_content(value: Option<&Value>) -> Result<Option<Value>, ProviderDriverError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.is_empty() => Ok(None),
        Some(Value::String(value)) => Ok(Some(Value::String(value.clone()))),
        Some(Value::Array(parts)) if parts.is_empty() => Ok(None),
        Some(Value::Array(parts)) => {
            for (index, part) in parts.iter().enumerate() {
                if !part.is_object() && !part.is_string() {
                    return Err(ProviderDriverError::protocol(
                        "response_content_part_invalid",
                        format!("response content part {index} is not an object or string"),
                    ));
                }
            }
            Ok(Some(Value::Array(parts.clone())))
        }
        Some(_) => Err(ProviderDriverError::protocol(
            "response_content_shape_invalid",
            "response message content must be a string, array, or null",
        )),
    }
}

fn visible_response_text(content: Option<&Value>) -> Option<String> {
    match content {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(parts)) => {
            let text = parts
                .iter()
                .filter_map(|part| {
                    part.as_str().or_else(|| {
                        part.get("text")
                            .and_then(Value::as_str)
                            .or_else(|| part.get("content").and_then(Value::as_str))
                    })
                })
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

fn normalize_usage(value: Option<&Value>) -> Result<Value, ProviderDriverError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(Value::Null);
    };
    let usage = value.as_object().ok_or_else(|| {
        ProviderDriverError::protocol("response_usage_invalid", "response usage must be an object")
    })?;
    Ok(json!({
        "input_tokens": first_u64(usage, &["prompt_tokens", "input_tokens"]),
        "output_tokens": first_u64(usage, &["completion_tokens", "output_tokens"]),
        "total_tokens": first_u64(usage, &["total_tokens"]),
        "cached_input_tokens": usage.get("prompt_tokens_details").and_then(|v| v.get("cached_tokens")).and_then(Value::as_u64),
        "reasoning_tokens": usage.get("completion_tokens_details").and_then(|v| v.get("reasoning_tokens")).and_then(Value::as_u64),
        "raw": value,
    }))
}

fn first_u64(values: &Map<String, Value>, names: &[&str]) -> Option<u64> {
    names
        .iter()
        .find_map(|name| values.get(*name).and_then(Value::as_u64))
}

fn apply_replay_fields(message: &mut Value, artifact: &Value) -> Result<(), ProviderDriverError> {
    let Some(replay) = artifact
        .get("provider_context")
        .and_then(|context| context.get("replay"))
    else {
        return Ok(());
    };
    let replay = replay.as_object().ok_or_else(|| {
        ProviderDriverError::protocol(
            "provider_replay_invalid",
            "provider_context.replay must be an object",
        )
    })?;
    let target = message
        .as_object_mut()
        .expect("provider replay target message is an object");
    for (name, value) in replay {
        if matches!(name.as_str(), "role" | "content" | "tool_calls") {
            return Err(ProviderDriverError::protocol(
                "provider_replay_field_reserved",
                format!("provider replay field '{name}' is reserved"),
            ));
        }
        target.insert(name.clone(), value.clone());
    }
    Ok(())
}

fn translate_tools_from_invocation(
    input: &InterfaceInvocationEnvelope,
    hidden_tools: &[String],
) -> Result<(Vec<Value>, BTreeSet<String>), ProviderDriverError> {
    let specs = input.interface_affordance_specs().map_err(|error| {
        ProviderDriverError::protocol(
            "affordance_specs_invalid",
            format!("provider affordance specs are invalid: {error}"),
        )
    })?;
    let visible = model_visible_affordances(specs, hidden_tools)
        .map_err(|error| ProviderDriverError::protocol("affordance_specs_invalid", error))?;
    let mut tools = Vec::with_capacity(visible.len());
    let mut model_visible_tool_names = BTreeSet::new();
    for (index, tool) in visible.iter().enumerate() {
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                ProviderDriverError::protocol(
                    "affordance_name_missing",
                    format!("interface_affordance_specs[{index}] has no non-empty name"),
                )
            })?;
        let description = tool
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let parameters = match tool.get("input_schema") {
            None => json!({ "type": "object", "properties": {} }),
            Some(schema) if schema.is_object() => schema.clone(),
            Some(_) => {
                return Err(ProviderDriverError::protocol(
                    "affordance_schema_invalid",
                    format!("interface_affordance_specs[{index}] input_schema is not an object"),
                ));
            }
        };
        if !model_visible_tool_names.insert(name.to_string()) {
            return Err(ProviderDriverError::protocol(
                "affordance_name_duplicate",
                format!("model-visible affordance '{name}' is duplicated"),
            ));
        }
        tools.push(json!({
            "type": "function",
            "function": {
                "name": encode_tool_name(name),
                "description": description,
                "parameters": parameters,
            }
        }));
    }
    Ok((tools, model_visible_tool_names))
}

type ModelToolProjection = (Vec<Value>, BTreeSet<String>, Vec<String>);

fn resolve_model_tool_projection(
    input: &InterfaceInvocationEnvelope,
    mut hidden_tools: Vec<String>,
    mode: ModelToolsMode,
) -> Result<ModelToolProjection, ProviderDriverError> {
    let (tools, model_visible_tool_names) = translate_tools_from_invocation(input, &hidden_tools)?;
    if mode == ModelToolsMode::Native {
        return Ok((tools, model_visible_tool_names, hidden_tools));
    }
    hidden_tools.extend(model_visible_tool_names);
    Ok((Vec::new(), BTreeSet::new(), hidden_tools))
}

fn require_modality(
    config: &ProviderConfig,
    modality: InputModality,
    name: &str,
) -> Result<(), ProviderDriverError> {
    if config.input_modalities.contains(&modality) {
        Ok(())
    } else {
        Err(ProviderDriverError::failed_closed(
            "input_modality_denied",
            format!("provider descriptor does not admit '{name}' input"),
        ))
    }
}

fn invocation_runtime_metadata(
    input: &InterfaceInvocationEnvelope,
) -> Result<&RuntimeInvocationMetadata, ProviderDriverError> {
    input
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.runtime.as_ref())
        .ok_or_else(|| {
            ProviderDriverError::protocol(
                "runtime_metadata_missing",
                "provider interface requires runtime metadata beside the artifact",
            )
        })
}

fn is_tool_result_artifact(label: Option<&str>, value: &Value) -> bool {
    label.is_some_and(|label| label.ends_with(".result"))
        && value.get("tool").is_some()
        && value.get("ok").is_some()
}

fn artifact_payload_label(artifact_id: &str, value: &Value) -> Option<String> {
    value
        .get("label")
        .and_then(Value::as_str)
        .filter(|label| !label.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            artifact_id
                .split_once(':')
                .map(|(label, _)| label)
                .filter(|label| !label.trim().is_empty())
                .map(str::to_string)
        })
}

fn required_trimmed_string(
    value: &Value,
    key: &str,
    surface: &str,
) -> Result<String, ProviderDriverError> {
    optional_trimmed_string(value, key, surface)?.ok_or_else(|| {
        ProviderDriverError::protocol(
            "required_string_missing",
            format!("{surface} has no non-empty trimmed '{key}'"),
        )
    })
}

fn optional_trimmed_string(
    value: &Value,
    key: &str,
    surface: &str,
) -> Result<Option<String>, ProviderDriverError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() && value.trim() == value => {
            Ok(Some(value.clone()))
        }
        Some(_) => Err(ProviderDriverError::protocol(
            "optional_string_invalid",
            format!("{surface}.{key} must be a non-empty trimmed string when present"),
        )),
    }
}

fn render_json(value: &Value) -> Result<String, ProviderDriverError> {
    serde_json::to_string(value).map_err(|error| {
        ProviderDriverError::protocol(
            "context_json_render_failed",
            format!("provider context artifact could not be rendered: {error}"),
        )
    })
}

fn encode_tool_name(name: &str) -> String {
    let mut output = String::with_capacity(name.len());
    for character in name.chars() {
        match character {
            '-' => output.push_str("-h"),
            '.' => output.push_str("-d"),
            other => output.push(other),
        }
    }
    output
}

fn decode_tool_name(encoded: &str) -> Result<String, ProviderDriverError> {
    let mut output = String::with_capacity(encoded.len());
    let mut characters = encoded.chars();
    while let Some(character) = characters.next() {
        if character != '-' {
            output.push(character);
            continue;
        }
        match characters.next() {
            Some('h') => output.push('-'),
            Some('d') => output.push('.'),
            other => {
                return Err(ProviderDriverError::protocol(
                    "tool_name_escape_invalid",
                    format!("native tool name '{encoded}' has malformed escape ending {other:?}"),
                ));
            }
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::sync::Mutex;

    use super::*;
    use octopus_descriptors::{ArtifactId, CheckpointId, EeId, InterfaceId, RunId};
    use octopus_interfaces::{
        ArtifactBindingPayload, GrantProjection, InterfaceArtifactFile,
        InterfaceInvocationEnvelope, InvocationMetadata, RuntimeInvocationMetadata,
    };
    use octopus_ledger::{ArtifactRef, ContentHash};

    static ENVIRONMENT: Mutex<()> = Mutex::new(());

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

    fn descriptor(model: &str, stream: bool, modalities: &[&str]) -> InterfaceDescriptor {
        let json = json!({
            "id": "world_provider_test",
            "interface_type": "world",
            "role": "provider",
            "direction": "bidirectional",
            "source_names": ["test"],
            "interface_driver_ref": WORLD_PROVIDER_INTERFACE_DRIVER_REF,
            "interface_driver_config": {
                "private_config": {
                    "model": model,
                    "model_hidden_tools": [],
                    "process_env_names": [],
                    "openai_compatible": {
                        "request": {
                            "path": "chat/completions",
                            "stream": stream,
                            "require_done": stream,
                            "message_content": "parts",
                            "retryable_statuses": [408, 429, 500, 502, 503, 504],
                            "body": { "parallel_tool_calls": true }
                        },
                        "response": {
                            "reasoning_fields": ["reasoning", "reasoning_content"],
                            "reasoning_detail_fields": ["reasoning_details"],
                            "successful_finish_reasons": ["stop", "tool_calls"],
                            "nonreplayable_finish_reasons": ["length"]
                        },
                        "input_modalities": modalities
                    },
                    "openai_compatible_retry": { "backoff_ms": [10, 20, 40, 80] },
                    "context_output_labels": ["interface.artifact.output", "tool.call", "provider.tool_results.pending", "turn.error"],
                    "session_snapshot_labels": ["interface.artifact.output", "provider.tool_results.pending", "turn.error"]
                },
                "transport": {
                    "base_url": "http://127.0.0.1:11434/v1",
                    "wire": "openai_chat_completions",
                    "request_timeout_ms": 5000,
                    "max_response_bytes": 1048576
                }
            }
        });
        octopus_descriptors::parse_strict(&serde_json::to_vec(&json).unwrap()).unwrap()
    }

    fn input(value: Value) -> InterfaceInvocationEnvelope {
        let root = std::env::temp_dir().join(format!(
            "octopus-openai-compatible-test-{}-{}",
            std::process::id(),
            blake3::hash(&serde_json::to_vec(&value).unwrap()).to_hex()
        ));
        let bytes = serde_json::to_vec(&value).unwrap();
        let content = ContentHash::of_bytes(&bytes);
        let label = value
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or(INTERFACE_INPUT_LABEL);
        let artifact_file = InterfaceArtifactFile::write(root, label, &bytes).unwrap();
        let role = json!({ "label": CONTEXT_REFERENCE_LABEL, "text": "grounded role" });
        let role_bytes = serde_json::to_vec(&role).unwrap();
        let role_hash = ContentHash::of_bytes(&role_bytes);
        let mut grant = GrantProjection::default();
        grant.interface_policy.insert(
            "world_provider_test".to_string(),
            BTreeMap::from([(
                "models".to_string(),
                json!([
                    "custom-model",
                    "reasoning-model",
                    "sonar",
                    "test-model",
                    "text-model",
                    "tool-model",
                    "vision-model"
                ]),
            )]),
        );
        InterfaceInvocationEnvelope {
            artifact: ArtifactRef {
                id: ArtifactId::new("input:test"),
                content,
            },
            artifact_file,
            artifact_binding_payloads: vec![ArtifactBindingPayload::new(
                ArtifactRef {
                    id: ArtifactId::new("context.reference:test"),
                    content: role_hash,
                },
                role_bytes,
            )],
            metadata: Some(InvocationMetadata::new(grant).with_runtime(
                RuntimeInvocationMetadata {
                    run: RunId::new("run-test"),
                    ee: EeId::new("ee-test"),
                    worker: InterfaceId::new("system_worker_test"),
                    checkpoint: CheckpointId::new("checkpoint-test"),
                    interface: InterfaceId::new("world_provider_test"),
                    epoch: 1,
                    run_record_hash: Some("hash".to_string()),
                },
            )),
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
        let source_artifact = ArtifactRef {
            id: ArtifactId::new(id),
            content: ContentHash::of_bytes(&bytes),
        };
        json!({
            "label": label,
            "payload": payload,
            "source_artifact": source_artifact,
            "source_movement_seq": seq,
        })
    }

    fn evidence() -> ProviderResponseEvidence {
        ProviderResponseEvidence {
            endpoint: crate::wire::EndpointEvidence {
                scheme: "https".to_string(),
                host: "provider.test".to_string(),
                port: None,
                path: "/v1/chat/completions".to_string(),
            },
            http: crate::wire::HttpEvidence {
                status: 200,
                content_type: Some("application/json".to_string()),
                identifiers: BTreeMap::new(),
            },
            body: crate::wire::BodyEvidence {
                bytes: 100,
                blake3: "hash".to_string(),
            },
            stream: crate::wire::StreamEvidence {
                requested: true,
                data_events: 4,
                transport_chunks: 2,
                done: true,
            },
        }
    }

    fn perplexity_driver() -> WorldProviderInterfaceDriver {
        let interface_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let descriptor: InterfaceDescriptor = octopus_descriptors::parse_strict(
            &std::fs::read(interface_root.join("world_provider_perplexity/interface.json"))
                .unwrap(),
        )
        .unwrap();
        WorldProviderInterfaceDriver::from_descriptor_with_secret_home(
            &descriptor,
            Some(&std::env::temp_dir()),
        )
        .unwrap()
    }

    #[test]
    fn perplexity_descriptor_and_life_are_exact_self_contained_contracts() {
        let interface_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let folder = interface_root.join("world_provider_perplexity");
        let descriptor: InterfaceDescriptor = octopus_descriptors::parse_strict(
            &std::fs::read(folder.join("interface.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(descriptor.id.as_str(), "world_provider_perplexity");
        assert!(serde_json::to_value(&descriptor)
            .unwrap()
            .get("reference")
            .is_none());
        assert_eq!(
            descriptor.interface_driver_ref,
            WORLD_PROVIDER_INTERFACE_DRIVER_REF
        );
        let driver = WorldProviderInterfaceDriver::from_descriptor_with_secret_home(
            &descriptor,
            Some(&std::env::temp_dir()),
        )
        .unwrap();
        assert_eq!(driver.config.endpoint.scheme(), "https");
        assert_eq!(driver.config.endpoint.host_str(), Some("api.perplexity.ai"));
        assert_eq!(driver.config.endpoint.path(), "/chat/completions");
        assert_eq!(driver.config.request.model_tools, ModelToolsMode::Disabled);
        assert!(driver.config.model_catalog.is_none());
        assert_eq!(driver.config.response.top_level_artifacts.len(), 4);
        let request_body = driver
            .config
            .resolve_request_body(&json!({
                "provider_request": {
                    "search_domain_filter": ["example.test"],
                    "return_related_questions": true
                }
            }))
            .unwrap();
        assert_eq!(request_body["stream_mode"], "full");
        assert_eq!(request_body["search_domain_filter"][0], "example.test");
        assert!(driver
            .config
            .resolve_request_body(&json!({
                "provider_request": {"tool_choice": "auto"}
            }))
            .is_err());
        assert_eq!(
            descriptor
                .interface_driver_config
                .as_ref()
                .unwrap()
                .private_config["model_catalog"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
        let life: octopus_interface_transport::SpawnLifeConfig =
            serde_json::from_slice(&std::fs::read(folder.join("life.json")).unwrap()).unwrap();
        assert_eq!(life.processes.len(), 1);
        assert_eq!(
            life.processes[0].bin,
            "target/debug/octopus-world-provider-openai-compatible-life"
        );
        assert!(life.processes[0]
            .coordinate_root_slots
            .contains("activation_secrets_home"));
    }

    #[test]
    fn descriptor_selected_top_level_fields_become_hashed_ordinary_artifacts() {
        let driver = perplexity_driver();
        let response = json!({
            "id": "answer-1",
            "model": "sonar",
            "created": 1,
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "grounded"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5},
            "citations": ["https://example.test/source"],
            "search_results": [{"title": "Source", "url": "https://example.test/source"}],
            "images": null,
            "related_questions": ["What next?"]
        });
        let parsed = parse_chat_completion_into_neutral(
            &response,
            &evidence(),
            &driver.config,
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(parsed.artifacts.len(), 5);
        let citations = parsed
            .artifacts
            .iter()
            .find(|artifact| artifact.label == "provider.citations")
            .unwrap();
        assert_eq!(citations.route, ProviderEmissionRoute::ClientProjection);
        assert_eq!(
            citations.neutral_output["raw"],
            json!(["https://example.test/source"])
        );
        let raw = serde_json::to_vec(&citations.neutral_output["raw"]).unwrap();
        assert_eq!(
            citations.neutral_output["raw_evidence"]["json_blake3"],
            blake3::hash(&raw).to_hex().to_string()
        );
        assert_eq!(
            citations.neutral_output["raw_evidence"]["response_body"]["blake3"],
            "hash"
        );
    }

    #[test]
    fn top_level_field_presence_and_shape_fail_closed_as_declared() {
        let mut driver = perplexity_driver();
        let response = json!({
            "id": "answer-2",
            "model": "sonar",
            "created": 2,
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "grounded"},
                "finish_reason": "stop"
            }]
        });
        let optional = parse_chat_completion_into_neutral(
            &response,
            &evidence(),
            &driver.config,
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(optional.artifacts.len(), 1);

        let mut malformed = response.clone();
        malformed["citations"] = json!({"url": "https://example.test/source"});
        let error = match parse_chat_completion_into_neutral(
            &malformed,
            &evidence(),
            &driver.config,
            &BTreeSet::new(),
        ) {
            Ok(_) => panic!("malformed descriptor-selected field must fail closed"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("response_top_level_field_shape_invalid"));

        driver.config.response.top_level_artifacts[0].required = true;
        let error = match parse_chat_completion_into_neutral(
            &response,
            &evidence(),
            &driver.config,
            &BTreeSet::new(),
        ) {
            Ok(_) => panic!("missing required descriptor-selected field must fail closed"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("response_top_level_field_missing"));
    }

    #[test]
    fn disabled_model_tools_are_cut_from_api_and_text_projection() {
        let mut envelope = input(json!({"prompt": "research"}));
        envelope.metadata.as_mut().unwrap().grant = GrantProjection::default()
            .with_interface_affordances(
                vec!["artifact.read".to_string()],
                vec![octopus_interfaces::InterfaceToolAffordance {
                    name: octopus_descriptors::ToolId::new("artifact.read"),
                    description: "read an artifact".to_string(),
                    args_schema: json!({"type": "object"}),
                }],
            );
        let (tools, names, hidden) =
            resolve_model_tool_projection(&envelope, Vec::new(), ModelToolsMode::Disabled).unwrap();
        assert!(tools.is_empty());
        assert!(names.is_empty());
        assert_eq!(hidden, ["artifact.read"]);
    }

    #[test]
    fn descriptor_declared_request_options_admit_only_allowlisted_artifact_fields() {
        let interface_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let descriptor: InterfaceDescriptor = octopus_descriptors::parse_strict(
            &std::fs::read(interface_root.join("world_provider_openrouter/interface.json"))
                .unwrap(),
        )
        .unwrap();
        let secret_home = std::env::temp_dir().join("octopus-provider-request-options");
        let interface_var = secret_home.join("state");
        let driver = WorldProviderInterfaceDriver::from_descriptor_with_roots(
            &descriptor,
            Some(&secret_home),
            Some(&interface_var),
        )
        .unwrap();
        let request = json!({
            "prompt": "inspect",
            "provider_request": {
                "temperature": 0.25,
                "provider": {"order": ["anthropic", "openai"]}
            }
        });
        let body = driver.config.resolve_request_body(&request).unwrap();
        assert_eq!(body["temperature"], json!(0.25));
        assert_eq!(body["provider"]["order"][0], "anthropic");

        let denied = json!({
            "prompt": "inspect",
            "provider_request": {"unlisted_option": true}
        });
        let error = driver.config.resolve_request_body(&denied).unwrap_err();
        assert!(error.contains("may not set request key 'unlisted_option'"));
        let _ = std::fs::remove_dir_all(secret_home);
    }

    #[test]
    fn descriptor_catalog_pointers_support_top_level_string_lists() {
        let config = ModelCatalogConfig {
            request_label: MODELS_LIST_LABEL.to_string(),
            output_label: crate::config::MODELS_CATALOG_LABEL.to_string(),
            path: "models".to_string(),
            models_pointer: String::new(),
            id_pointer: String::new(),
        };
        let models = normalize_model_catalog(&json!(["model-a", "model-b"]), &config).unwrap();
        assert_eq!(models, vec![json!("model-a"), json!("model-b")]);
        assert_eq!(
            filter_model_catalog(models, &config, &BTreeSet::from(["model-b".to_string()])),
            vec![json!("model-b")]
        );
        assert!(normalize_model_catalog(&json!(["model-a", "model-a"]), &config).is_err());
    }

    #[test]
    fn text_only_tool_messages_preserve_non_text_parts_as_json() {
        let content = tool_content_as_text(json!([
            {"type": "text", "text": "caption"},
            {"type": "image_url", "image_url": {"url": "https://example.test/image.png"}}
        ]))
        .unwrap();
        let content = content.as_str().unwrap();
        assert!(content.starts_with("caption\n\n"));
        assert!(content.contains("https://example.test/image.png"));
        assert!(content.contains("image_url"));
    }

    #[test]
    fn multimodal_terminal_items_become_native_parts() {
        let descriptor = descriptor("vision-model", true, &["text", "image_url", "input_audio"]);
        let driver = WorldProviderInterfaceDriver::from_descriptor(&descriptor).unwrap();
        let envelope = input(json!({
            "prompt": "inspect",
            "input_items": [
                { "type": "text", "text": "inspect this" },
                { "type": "image", "url": "data:image/png;base64,aGVsbG8=" }
            ]
        }));
        let messages = assemble_messages(
            &envelope,
            &json!({
                "prompt": "inspect",
                "input_items": [
                    { "type": "text", "text": "inspect this" },
                    { "type": "image", "url": "data:image/png;base64,aGVsbG8=" }
                ]
            }),
            &driver.config,
            &[],
            driver.interface_id.as_str(),
        )
        .unwrap();
        let MessageAssembly::Ready(messages) = messages else {
            panic!("multimodal user input must be ready for the provider");
        };
        assert_eq!(messages[1]["content"][0]["type"], "text");
        assert_eq!(messages[1]["content"][1]["type"], "image_url");
    }

    #[test]
    fn session_snapshot_replays_prior_history_and_its_outer_artifact_once() {
        let descriptor = descriptor("text-model", true, &["text"]);
        let driver = WorldProviderInterfaceDriver::from_descriptor(&descriptor).unwrap();
        let prior = json!({"label": "operator.message", "prompt": "ONE"});
        let mut envelope = input(json!({"prompt": "THREE"}));
        bind_json(
            &mut envelope,
            "interface.artifact.output:message-2",
            json!({
                "label": INTERFACE_ARTIFACT_OUTPUT_LABEL,
                "content": "TWO",
                "provider_message_id": "message-2",
                "provider_context": {"content": "TWO", "replay": {}},
                "session_context": {
                    "schema_version": 2,
                    "provider_interface": "world_provider_test",
                    "artifacts": [
                        session_item("operator.message:one", "operator.message", prior, 10)
                    ]
                }
            }),
        );
        let assembled = assemble_messages(
            &envelope,
            &json!({"prompt": "THREE"}),
            &driver.config,
            &[],
            driver.interface_id.as_str(),
        )
        .unwrap();
        let MessageAssembly::Ready(messages) = assembled else {
            panic!("ordinary snapshot history must be ready");
        };
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[1], json!({"role": "user", "content": "ONE"}));
        assert_eq!(messages[2], json!({"role": "assistant", "content": "TWO"}));
        assert_eq!(messages[3]["role"], "user");
        assert_eq!(messages[3]["content"][0]["type"], "text");
        assert_eq!(messages[3]["content"][0]["text"], "THREE");
        assert_eq!(
            messages
                .iter()
                .filter(|message| message["content"] == "TWO")
                .count(),
            1
        );
    }

    #[test]
    fn mixed_parallel_turn_waits_for_full_fan_in_and_replays_one_assistant_message() {
        let descriptor = descriptor("tool-model", true, &["text"]);
        let driver = WorldProviderInterfaceDriver::from_descriptor(&descriptor).unwrap();
        let provider_context = json!({
            "content": "working",
            "replay": { "reasoning_content": "trace" }
        });
        let output = json!({
            "label": INTERFACE_ARTIFACT_OUTPUT_LABEL,
            "content": "working",
            "provider_message_id": "message-1",
            "provider_context": provider_context
        });
        let call_one = json!({
            "label": TOOL_CALL_LABEL,
            "tool": "artifact.read",
            "arguments": { "id": "a" },
            "correlation_id": "call-1",
            "provider_message_id": "message-1",
            "provider_context": provider_context
        });
        let call_two = json!({
            "label": TOOL_CALL_LABEL,
            "tool": "artifact.list",
            "arguments": {},
            "correlation_id": "call-2",
            "provider_message_id": "message-1",
            "provider_context": provider_context
        });
        let result_one = json!({
            "label": "artifact.read.result",
            "tool": "artifact.read",
            "correlation_id": "call-1",
            "ok": true,
            "body": { "content": "first" }
        });
        let result_two = json!({
            "label": "artifact.list.result",
            "tool": "artifact.list",
            "correlation_id": "call-2",
            "ok": true,
            "body": { "content": "second" }
        });

        let mut partial = input(result_one.clone());
        bind_json(&mut partial, "output:message-1", output.clone());
        bind_json(&mut partial, "tool.call:call-1", call_one.clone());
        bind_json(&mut partial, "tool.call:call-2", call_two.clone());
        let pending = assemble_messages(
            &partial,
            &result_one,
            &driver.config,
            &[],
            driver.interface_id.as_str(),
        )
        .unwrap();
        let MessageAssembly::AwaitingToolResults(groups) = pending else {
            panic!("one of two parallel tool results must not invoke the model");
        };
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].expected, 2);
        assert_eq!(groups[0].received, 1);
        assert_eq!(groups[0].pending_correlation_ids, ["call-2"]);

        let mut complete = input(result_two.clone());
        let snapshot = json!({
            "label": TOOL_RESULTS_PENDING_LABEL,
            "groups": [{
                "provider_message_id": "message-1",
                "expected": 2,
                "received": 1,
                "pending_correlation_ids": ["call-2"]
            }],
            "session_context": {
                "schema_version": 2,
                "provider_interface": "world_provider_test",
                "artifacts": [
                    session_item("output:message-1", INTERFACE_ARTIFACT_OUTPUT_LABEL, output, 11),
                    session_item("tool.call:call-1", TOOL_CALL_LABEL, call_one, 12),
                    session_item("tool.call:call-2", TOOL_CALL_LABEL, call_two, 13),
                    session_item("artifact.read.result:call-1", "artifact.read.result", result_one, 14)
                ]
            }
        });
        bind_json(
            &mut complete,
            "provider.tool_results.pending:snapshot",
            snapshot,
        );
        let ready = assemble_messages(
            &complete,
            &result_two,
            &driver.config,
            &[],
            driver.interface_id.as_str(),
        )
        .unwrap();
        let MessageAssembly::Ready(messages) = ready else {
            panic!("the complete parallel tool-result group must invoke the model");
        };
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"], "working");
        assert_eq!(messages[1]["reasoning_content"], "trace");
        assert_eq!(messages[1]["tool_calls"].as_array().unwrap().len(), 2);
        assert_eq!(messages[2]["tool_call_id"], "call-1");
        assert_eq!(messages[3]["tool_call_id"], "call-2");
        assert_eq!(
            messages
                .iter()
                .filter(|message| message["role"] == "assistant")
                .count(),
            1
        );
    }

    #[test]
    fn reasoning_usage_content_and_parallel_tools_are_preserved() {
        let descriptor = descriptor("reasoning-model", true, &["text"]);
        let driver = WorldProviderInterfaceDriver::from_descriptor(&descriptor).unwrap();
        let parsed = parse_chat_completion_into_neutral(
            &json!({
                "id": "response-7",
                "model": "reasoning-model",
                "choices": [{
                    "message": {
                        "content": "working",
                        "reasoning_content": "private trace",
                        "reasoning_details": [{"type":"summary","text":"summary"}],
                        "tool_calls": [
                            {"id":"call-1","type":"function","function":{"name":"fs-dread","arguments":"{\"path\":\"a\"}"}},
                            {"id":"call-2","type":"function","function":{"name":"fs-dwrite","arguments":{"path":"b"}}}
                        ]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": {
                    "prompt_tokens": 11,
                    "completion_tokens": 7,
                    "total_tokens": 18,
                    "completion_tokens_details": {"reasoning_tokens": 4}
                }
            }),
            &evidence(),
            &driver.config,
            &BTreeSet::from(["fs.read".to_string(), "fs.write".to_string()]),
        )
        .unwrap();
        assert_eq!(parsed.artifacts.len(), 3);
        assert_eq!(parsed.artifacts[0].label, INTERFACE_ARTIFACT_OUTPUT_LABEL);
        assert_eq!(parsed.artifacts[1].neutral_output["tool"], "fs.read");
        assert_eq!(parsed.artifacts[2].neutral_output["tool"], "fs.write");
        assert_eq!(
            parsed.artifacts[1].neutral_output["provider_message_id"],
            "response-7"
        );
        assert_eq!(
            parsed.artifacts[1].neutral_output["provider_context"]["usage"]["reasoning_tokens"],
            4
        );
    }

    #[test]
    fn terminal_finish_reasons_follow_descriptor_policy() {
        let descriptor = descriptor("reasoning-model", false, &["text"]);
        let driver = WorldProviderInterfaceDriver::from_descriptor(&descriptor).unwrap();
        let response = |finish_reason: &str| {
            json!({
                "id": "response-finish",
                "choices": [{
                    "message": { "content": "partial" },
                    "finish_reason": finish_reason
                }]
            })
        };
        let complete = parse_chat_completion_into_neutral(
            &response("stop"),
            &evidence(),
            &driver.config,
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(complete.artifacts[0].neutral_output["complete"], true);
        assert_eq!(
            complete.artifacts[0].neutral_output["nonreplayable_terminal"],
            false
        );
        let nonreplayable_response = json!({
            "id": "response-finish",
            "choices": [{
                "message": {
                    "content": "partial",
                    "reasoning_content": "partial reasoning",
                    "tool_calls": [{
                        "id": "partial-call",
                        "type": "function",
                        "function": {"name": "never-routed", "arguments": "{\"x\":"}
                    }]
                },
                "finish_reason": "length"
            }],
            "usage": {"prompt_tokens": 4, "completion_tokens": 8, "total_tokens": 12}
        });
        let nonreplayable = parse_chat_completion_into_neutral(
            &nonreplayable_response,
            &evidence(),
            &driver.config,
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(nonreplayable.artifacts.len(), 1);
        assert_eq!(
            nonreplayable.artifacts[0].route,
            ProviderEmissionRoute::ClientProjection
        );
        assert_eq!(nonreplayable.artifacts[0].neutral_output["text"], "partial");
        assert_eq!(nonreplayable.artifacts[0].neutral_output["complete"], false);
        assert_eq!(
            nonreplayable.artifacts[0].neutral_output["nonreplayable_terminal"],
            true
        );
        assert_eq!(
            nonreplayable.artifacts[0].neutral_output["partial_tool_calls_routed"],
            false
        );
        assert_eq!(
            nonreplayable.artifacts[0].neutral_output["provider_context"]["reasoning"]
                ["reasoning_content"],
            "partial reasoning"
        );
        assert_eq!(
            nonreplayable.artifacts[0].neutral_output["provider_context"]["usage"]["total_tokens"],
            12
        );
        assert_eq!(
            nonreplayable.artifacts[0].neutral_output["provider_context"]
                ["accepted_terminal_response"]["choices"][0]["message"]["tool_calls"][0]["id"],
            "partial-call"
        );
        let unknown = match parse_chat_completion_into_neutral(
            &response("content_filter"),
            &evidence(),
            &driver.config,
            &BTreeSet::new(),
        ) {
            Err(error) => error,
            Ok(_) => panic!("undeclared finish reason must fail closed"),
        };
        assert!(matches!(unknown, ProviderDriverError::FailedClosed(_)));
    }

    #[test]
    fn local_image_paths_fail_closed_at_artifact_boundary() {
        let descriptor = descriptor("vision-model", false, &["text", "image_url"]);
        let driver = WorldProviderInterfaceDriver::from_descriptor(&descriptor).unwrap();
        let error = user_content(
            &json!({"input_items":[{"type":"localImage","path":"/tmp/a.png"}]}),
            &driver.config,
        )
        .unwrap_err();
        assert!(matches!(error, ProviderDriverError::FailedClosed(_)));
    }

    #[test]
    fn shipped_openai_compatible_interfaces_are_self_contained() {
        let state_root = std::env::temp_dir().join(format!(
            "octopus-openai-compatible-descriptors-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let interface_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let mut interfaces = std::fs::read_dir(interface_root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_dir() && path.join("interface.json").is_file())
            .filter_map(|path| {
                let descriptor: InterfaceDescriptor = octopus_descriptors::parse_strict(
                    &std::fs::read(path.join("interface.json")).unwrap(),
                )
                .unwrap();
                (descriptor.interface_driver_ref == WORLD_PROVIDER_INTERFACE_DRIVER_REF)
                    .then_some((path, descriptor))
            })
            .collect::<Vec<_>>();
        interfaces.sort_by(|left, right| left.0.cmp(&right.0));
        assert!(!interfaces.is_empty());
        for (folder, descriptor) in interfaces {
            let id = folder.file_name().unwrap().to_str().unwrap();
            assert_eq!(descriptor.id.as_str(), id);
            assert!(serde_json::to_value(&descriptor)
                .unwrap()
                .get("reference")
                .is_none());
            assert!(folder.join("life.json").is_file());
            assert_eq!(
                descriptor.interface_driver_ref,
                WORLD_PROVIDER_INTERFACE_DRIVER_REF
            );
            assert!(descriptor
                .interface_driver_config
                .as_ref()
                .unwrap()
                .private_config
                .get("process_env_names")
                .is_some_and(Value::is_array));
            assert!(descriptor
                .interface_driver_config
                .as_ref()
                .unwrap()
                .transport
                .as_ref()
                .unwrap()
                .auth_env_key
                .is_none());
            if descriptor
                .interface_driver_config
                .as_ref()
                .unwrap()
                .private_config
                .get("process_env_names")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
            {
                WorldProviderInterfaceDriver::from_descriptor_with_roots(
                    &descriptor,
                    Some(&state_root),
                    Some(&state_root.join(id)),
                )
                .unwrap();
            }
        }
        let _ = std::fs::remove_dir_all(state_root);
    }

    #[test]
    fn exact_model_profiles_select_complete_provider_private_wire_config() {
        let interface_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let descriptor: InterfaceDescriptor = octopus_descriptors::parse_strict(
            &std::fs::read(interface_root.join("world_provider_openrouter/interface.json"))
                .unwrap(),
        )
        .unwrap();
        let state_root = std::env::temp_dir().join(format!(
            "octopus-openrouter-model-profiles-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let interface_var_dir = state_root.join("world_provider_openrouter");
        std::fs::create_dir_all(&interface_var_dir).unwrap();
        let driver = WorldProviderInterfaceDriver::from_descriptor_with_roots(
            &descriptor,
            Some(&state_root),
            Some(&interface_var_dir),
        )
        .unwrap();
        let claude = driver
            .config
            .for_provider_model("anthropic/claude-sonnet-4.6")
            .unwrap();
        let gpt = driver.config.for_provider_model("openai/gpt-5.5").unwrap();
        let claude_keys = &claude.request.artifact_body.as_ref().unwrap().allowed_keys;
        let gpt_keys = &gpt.request.artifact_body.as_ref().unwrap().allowed_keys;
        assert!(claude_keys.contains("verbosity"));
        assert!(!claude_keys.contains("reasoning"));
        assert!(gpt_keys.contains("reasoning"));
        assert!(gpt_keys.contains("verbosity"));
        assert!(claude.model_profiles.is_empty());
        assert!(gpt.model_profiles.is_empty());
        assert!(driver.config.for_provider_model("unformed-model").is_err());
        let _ = std::fs::remove_dir_all(state_root);
    }

    #[test]
    fn credential_pool_is_exact_interface_auth_and_counts_real_attempts() {
        let mut descriptor = descriptor("test-model", true, &["text"]);
        let config = descriptor.interface_driver_config.as_mut().unwrap();
        config.private_config.insert(
            "credential_pool".to_string(),
            json!({
                "strategy": "fill_first",
                "credentials": [
                    {
                        "id": "primary",
                        "secret_ref": "secret://world_provider_test/PRIMARY_KEY",
                        "priority": 0
                    },
                    {
                        "id": "secondary",
                        "secret_ref": "secret://world_provider_test/SECONDARY_KEY",
                        "priority": 1
                    }
                ],
                "failure_rules": {
                    "429": {
                        "same_credential_retries": 1,
                        "cooldown_ms": 60000
                    }
                }
            }),
        );
        let transport = config.transport.as_mut().unwrap();
        transport.auth_header = Some("Authorization".to_string());
        transport.auth_value_prefix = Some("Bearer ".to_string());
        let root = std::env::temp_dir().join(format!(
            "octopus-provider-pool-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let driver = WorldProviderInterfaceDriver::from_descriptor_with_roots(
            &descriptor,
            Some(&root),
            Some(&root.join("var")),
        )
        .unwrap();
        let provider_context = json!({
            "content": "working",
            "replay": { "reasoning_content": "trace" }
        });
        let output = json!({
            "label": INTERFACE_ARTIFACT_OUTPUT_LABEL,
            "content": "working",
            "provider_message_id": "message-1",
            "provider_context": provider_context
        });
        let call_one = json!({
            "label": TOOL_CALL_LABEL,
            "tool": "artifact.read",
            "arguments": { "id": "a" },
            "correlation_id": "call-1",
            "provider_message_id": "message-1",
            "provider_context": provider_context
        });
        let call_two = json!({
            "label": TOOL_CALL_LABEL,
            "tool": "artifact.list",
            "arguments": {},
            "correlation_id": "call-2",
            "provider_message_id": "message-1",
            "provider_context": provider_context
        });
        let result_one = json!({
            "label": "artifact.read.result",
            "tool": "artifact.read",
            "correlation_id": "call-1",
            "ok": true,
            "body": { "content": "first" }
        });
        let mut partial = input(result_one);
        bind_json(&mut partial, "output:message-1", output);
        bind_json(&mut partial, "tool.call:call-1", call_one);
        bind_json(&mut partial, "tool.call:call-2", call_two);
        assert!(matches!(
            driver
                .prepare_artifact_and_materialize_local_outputs(partial, &root.join("local"), &BTreeSet::from(["artifact.read".into()]))
                .unwrap(),
            PreparedProviderTurn::Local(_)
        ));
        let state: Value = serde_json::from_slice(
            &std::fs::read(root.join("var/credential_pool/state.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(state["entries"]["primary"]["request_count"], 0);
        assert_eq!(state["entries"]["secondary"]["request_count"], 0);
        let candidate = driver
            .select_credential_candidate(None, 10)
            .unwrap()
            .unwrap();
        let state: Value = serde_json::from_slice(
            &std::fs::read(root.join("var/credential_pool/state.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(state["entries"]["primary"]["request_count"], 0);
        let primary = driver
            .start_credential_attempt(Some(&candidate), 10)
            .unwrap()
            .unwrap();
        assert_eq!(primary.id(), "primary");
        assert_eq!(
            driver
                .config
                .credential_handle_fingerprint(Some(&primary))
                .unwrap()
                .unwrap()
                .len(),
            64
        );
        driver
            .record_credential_success(Some(&primary), 20)
            .unwrap();

        let mut duplicate = descriptor.clone();
        duplicate
            .interface_driver_config
            .as_mut()
            .unwrap()
            .transport
            .as_mut()
            .unwrap()
            .auth_secret_ref = Some("secret://world_provider_test/PRIMARY_KEY".to_string());
        let error = WorldProviderInterfaceDriver::from_descriptor_with_roots(
            &duplicate,
            Some(&root),
            Some(&root.join("duplicate")),
        )
        .err()
        .unwrap();
        assert!(error.contains("cannot be combined"));

        let mut cloned = descriptor;
        cloned.id = InterfaceId::new("world_provider_test_clone");
        let error = WorldProviderInterfaceDriver::from_descriptor_with_roots(
            &cloned,
            Some(&root),
            Some(&root.join("clone")),
        )
        .err()
        .unwrap();
        assert!(error.contains("must equal exact interface"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn custom_endpoint_and_auth_modes_are_strict_descriptor_contracts() {
        let _lock = ENVIRONMENT.lock().unwrap();
        let _restore = EnvironmentRestore::set(&[
            ("CUSTOM_OPENAI_BASE_URL", "http://127.0.0.1:18080/v1"),
            ("CUSTOM_OPENAI_MODEL", "custom-model"),
        ]);
        let interface_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let resolved: InterfaceDescriptor = octopus_descriptors::parse_strict(
            &std::fs::read(interface_root.join("world_provider_custom/interface.json")).unwrap(),
        )
        .unwrap();
        let metadata = &resolved
            .interface_driver_config
            .as_ref()
            .unwrap()
            .private_config;
        assert_eq!(metadata.get("model"), Some(&Value::Null));
        assert_eq!(
            metadata.get("model_env").and_then(Value::as_str),
            Some("CUSTOM_OPENAI_MODEL")
        );
        let driver = WorldProviderInterfaceDriver::from_descriptor_with_secret_home(
            &resolved,
            Some(&std::env::temp_dir()),
        )
        .unwrap();
        assert!(driver.config.auth.is_none());
        assert_eq!(
            driver.config.resolve_provider_model().unwrap(),
            "custom-model"
        );

        let mut generic_secret_header = resolved.clone();
        generic_secret_header
            .interface_driver_config
            .as_mut()
            .unwrap()
            .transport
            .as_mut()
            .unwrap()
            .headers
            .insert("Authorization".to_string(), "Bearer raw-secret".to_string());
        assert!(
            WorldProviderInterfaceDriver::from_descriptor_with_secret_home(
                &generic_secret_header,
                Some(&std::env::temp_dir()),
            )
            .err()
            .unwrap()
            .contains("credential-bearing")
        );

        let mut orphan_header = resolved.clone();
        orphan_header
            .interface_driver_config
            .as_mut()
            .unwrap()
            .transport
            .as_mut()
            .unwrap()
            .auth_header = Some("Authorization".to_string());
        assert!(
            WorldProviderInterfaceDriver::from_descriptor_with_secret_home(
                &orphan_header,
                Some(&std::env::temp_dir()),
            )
            .err()
            .unwrap()
            .contains("without an auth source")
        );

        let mut duplicate = resolved.clone();
        let duplicate_transport = duplicate
            .interface_driver_config
            .as_mut()
            .unwrap()
            .transport
            .as_mut()
            .unwrap();
        duplicate_transport.auth_env_key = Some("CUSTOM_OPENAI_API_KEY".to_string());
        duplicate_transport.auth_secret_ref =
            Some("secret://world_provider_custom/CUSTOM_OPENAI_API_KEY".to_string());
        duplicate_transport.auth_header = Some("Authorization".to_string());
        duplicate_transport.auth_value_prefix = Some("Bearer ".to_string());
        assert!(
            WorldProviderInterfaceDriver::from_descriptor_with_secret_home(
                &duplicate,
                Some(&std::env::temp_dir()),
            )
            .err()
            .unwrap()
            .contains("only one of auth_env_key or auth_secret_ref")
        );

        let remote: InterfaceDescriptor = octopus_descriptors::parse_strict(
            &std::fs::read(interface_root.join("world_provider_kilocode/interface.json")).unwrap(),
        )
        .unwrap();
        let mut missing_header = remote.clone();
        missing_header
            .interface_driver_config
            .as_mut()
            .unwrap()
            .transport
            .as_mut()
            .unwrap()
            .auth_header = None;
        assert!(
            WorldProviderInterfaceDriver::from_descriptor_with_secret_home(
                &missing_header,
                Some(&std::env::temp_dir()),
            )
            .err()
            .unwrap()
            .contains("explicit auth_header")
        );
        let mut missing_prefix = remote;
        missing_prefix
            .interface_driver_config
            .as_mut()
            .unwrap()
            .transport
            .as_mut()
            .unwrap()
            .auth_value_prefix = None;
        assert!(
            WorldProviderInterfaceDriver::from_descriptor_with_secret_home(
                &missing_prefix,
                Some(&std::env::temp_dir()),
            )
            .err()
            .unwrap()
            .contains("explicit auth_value_prefix")
        );
    }

    #[test]
    fn cloned_interface_secret_scope_must_be_rebased() {
        let interface_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let mut cloned: InterfaceDescriptor = octopus_descriptors::parse_strict(
            &std::fs::read(interface_root.join("world_provider_kilocode/interface.json")).unwrap(),
        )
        .unwrap();
        cloned.id = InterfaceId::new("world_provider_kilocode_clone");
        let secret_home = std::env::temp_dir();
        let error = WorldProviderInterfaceDriver::from_descriptor_with_secret_home(
            &cloned,
            Some(&secret_home),
        )
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
            Some("secret://world_provider_kilocode_clone/KILOCODE_API_KEY".to_string());
        WorldProviderInterfaceDriver::from_descriptor_with_secret_home(&cloned, Some(&secret_home))
            .unwrap();
    }
}
