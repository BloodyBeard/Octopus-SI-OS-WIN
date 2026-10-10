use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use octopus_github_copilot_auth::{
    api_token_secret_ref, install_api_token_for_transport, AuthFailure,
    GitHubAuthDispatchAdmission, GitHubCredentialManager,
};
use octopus_interfaces::{
    InterfaceArtifactFile, InterfaceBoundaryError, InterfaceDescriptor, InterfaceInvocationEnvelope,
};
use octopus_provider_run_authoring::{
    ProviderEmission, ProviderEmissionRoute, ProviderOutputHandoff,
};
use octopus_world_provider_anthropic::AnthropicMessagesDriver;
use octopus_world_provider_responses::{OpenAiResponsesDriver, ResponsesFailure};
use serde::Deserialize;
use serde_json::{json, Value};

pub const RESPONSES_INTERFACE_DRIVER_REF: &str = "github_copilot_responses";
pub const ANTHROPIC_INTERFACE_DRIVER_REF: &str = "github_copilot_anthropic_messages";

const MODELS_LIST_LABEL: &str = "provider.models.list";
const MODELS_CATALOG_LABEL: &str = "provider.models.catalog";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CopilotConfig {
    model: String,
    retry_backoff_ms: Vec<u64>,
    catalog_filter: CopilotCatalogFilter,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CopilotCatalogFilter {
    required_endpoint: String,
    required_capability_type: String,
}

struct CopilotSupport {
    auth: GitHubCredentialManager,
    retry_backoff: Vec<Duration>,
    catalog_filter: CopilotCatalogFilter,
    model: String,
    wire: &'static str,
}

pub struct GitHubCopilotResponsesDriver {
    support: CopilotSupport,
    responses: OpenAiResponsesDriver,
}

pub struct GitHubCopilotAnthropicDriver {
    support: CopilotSupport,
    anthropic: AnthropicMessagesDriver,
}

#[derive(Clone, Debug)]
pub struct GitHubCopilotFailure {
    pub class: &'static str,
    pub retryable: bool,
    pub status: Option<u16>,
    pub code: Option<String>,
    pub detail: String,
    pub evidence: Option<Value>,
}

impl GitHubCopilotFailure {
    pub fn contract(detail: impl Into<String>) -> Self {
        Self {
            class: "contract",
            retryable: false,
            status: None,
            code: None,
            detail: detail.into(),
            evidence: None,
        }
    }

    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self {
            class: "unavailable",
            retryable: false,
            status: None,
            code: None,
            detail: detail.into(),
            evidence: None,
        }
    }

    pub fn interrupted(detail: impl Into<String>) -> Self {
        Self {
            class: "interrupted",
            retryable: false,
            status: None,
            code: None,
            detail: detail.into(),
            evidence: None,
        }
    }

    pub fn exhausted(detail: impl Into<String>, evidence: Option<Value>) -> Self {
        Self {
            class: "transport",
            retryable: false,
            status: None,
            code: Some("retry_exhausted".to_string()),
            detail: detail.into(),
            evidence,
        }
    }
}

impl From<ResponsesFailure> for GitHubCopilotFailure {
    fn from(failure: ResponsesFailure) -> Self {
        Self {
            class: failure.class,
            retryable: failure.retryable,
            status: failure.status,
            code: failure.code,
            detail: failure.detail,
            evidence: failure.evidence,
        }
    }
}

impl From<AuthFailure> for GitHubCopilotFailure {
    fn from(failure: AuthFailure) -> Self {
        Self {
            class: failure.class,
            retryable: failure.retryable,
            status: failure
                .evidence
                .as_ref()
                .and_then(|value| value.get("status"))
                .and_then(Value::as_u64)
                .and_then(|status| u16::try_from(status).ok()),
            code: Some(failure.code.to_string()),
            detail: failure.detail,
            evidence: failure.evidence,
        }
    }
}

impl From<InterfaceBoundaryError> for GitHubCopilotFailure {
    fn from(failure: InterfaceBoundaryError) -> Self {
        match failure {
            InterfaceBoundaryError::Protocol(detail) => Self {
                class: "protocol",
                retryable: false,
                status: None,
                code: None,
                detail,
                evidence: None,
            },
            InterfaceBoundaryError::FailedClosed(detail) => Self {
                class: "unavailable",
                retryable: false,
                status: None,
                code: None,
                detail,
                evidence: None,
            },
            InterfaceBoundaryError::Transport(detail) => Self {
                class: "transport",
                retryable: true,
                status: None,
                code: None,
                detail,
                evidence: None,
            },
        }
    }
}

fn required_formed_models(
    input: &InterfaceInvocationEnvelope,
) -> Result<BTreeSet<String>, GitHubCopilotFailure> {
    let policy = input
        .interface_policy()
        .map_err(|error| {
            GitHubCopilotFailure::unavailable(format!(
                "GitHub Copilot formed interface policy is invalid: {error}"
            ))
        })?
        .ok_or_else(|| {
            GitHubCopilotFailure::unavailable(
                "GitHub Copilot invocation carries no formed interface policy",
            )
        })?;
    let values = policy
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            GitHubCopilotFailure::unavailable(
                "GitHub Copilot formed interface policy models must be a string array",
            )
        })?;
    if values.is_empty() {
        return Err(GitHubCopilotFailure::unavailable(
            "GitHub Copilot formed interface policy models is empty",
        ));
    }
    let mut models = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let model = value.as_str().filter(|model| {
            !model.is_empty() && model.trim() == *model && !model.contains(['\r', '\n', '\0'])
        });
        let Some(model) = model else {
            return Err(GitHubCopilotFailure::unavailable(format!(
                "GitHub Copilot formed interface policy models[{index}] is not a trimmed string"
            )));
        };
        if !models.insert(model.to_string()) {
            return Err(GitHubCopilotFailure::unavailable(format!(
                "GitHub Copilot formed interface policy models repeats '{model}'"
            )));
        }
    }
    Ok(models)
}

impl CopilotSupport {
    fn from_descriptor(
        descriptor: &InterfaceDescriptor,
        activation_secrets_home: &Path,
        expected_driver_ref: &'static str,
        expected_endpoint: &'static str,
        wire: &'static str,
    ) -> Result<Self, String> {
        if descriptor.interface_driver_ref != expected_driver_ref {
            return Err(format!(
                "GitHub Copilot interface '{}' selects driver '{}', expected '{expected_driver_ref}'",
                descriptor.id, descriptor.interface_driver_ref
            ));
        }
        let driver_config = descriptor.interface_driver_config.as_ref().ok_or_else(|| {
            format!(
                "GitHub Copilot interface '{}' has no driver config",
                descriptor.id
            )
        })?;
        let transport = driver_config.transport.as_ref().ok_or_else(|| {
            format!(
                "GitHub Copilot interface '{}' has no transport config",
                descriptor.id
            )
        })?;
        if transport.wire.as_deref() != Some(expected_driver_ref) {
            return Err(format!(
                "GitHub Copilot interface '{}' transport.wire must be '{expected_driver_ref}'",
                descriptor.id
            ));
        }
        if transport
            .headers
            .keys()
            .any(|name| name.eq_ignore_ascii_case("Copilot-Vision-Request"))
        {
            return Err(
                "GitHub Copilot descriptors cannot declare an unconditional Copilot-Vision-Request header"
                    .to_string(),
            );
        }
        let config: CopilotConfig = serde_json::from_value(
            driver_config
                .private_config
                .get("github_copilot")
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "GitHub Copilot interface '{}' has no private_config.github_copilot",
                        descriptor.id
                    )
                })?,
        )
        .map_err(|error| format!("GitHub Copilot config is malformed: {error}"))?;
        validate_config(&config, expected_endpoint)?;
        let declared_model = driver_config
            .private_config
            .get("model")
            .and_then(Value::as_str)
            .ok_or_else(|| "GitHub Copilot private_config.model is missing".to_string())?;
        if config.model != declared_model {
            return Err(
                "GitHub Copilot model must be identical in github_copilot.model and private_config.model"
                    .to_string(),
            );
        }
        let auth = GitHubCredentialManager::from_descriptor(descriptor, activation_secrets_home)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            auth,
            retry_backoff: config
                .retry_backoff_ms
                .into_iter()
                .map(Duration::from_millis)
                .collect(),
            catalog_filter: config.catalog_filter,
            model: config.model,
            wire,
        })
    }

    fn adjusted_descriptor(
        &self,
        descriptor: &InterfaceDescriptor,
        driver_ref: &str,
    ) -> Result<InterfaceDescriptor, String> {
        let api_secret_ref = api_token_secret_ref(descriptor).map_err(|error| error.to_string())?;
        let mut adjusted = install_api_token_for_transport(descriptor, &api_secret_ref)
            .map_err(|error| error.to_string())?;
        adjusted.interface_driver_ref = driver_ref.to_string();
        adjusted
            .interface_driver_config
            .as_mut()
            .and_then(|config| config.transport.as_mut())
            .ok_or_else(|| "GitHub Copilot adjusted transport disappeared".to_string())?
            .wire = Some(driver_ref.to_string());
        Ok(adjusted)
    }

    fn retry_backoff(&self, failed_attempt: u32) -> Result<Duration, String> {
        let index = failed_attempt
            .checked_sub(1)
            .ok_or_else(|| "GitHub Copilot retry attempt must be positive".to_string())?
            as usize;
        self.retry_backoff.get(index).copied().ok_or_else(|| {
            format!(
                "GitHub Copilot descriptor has no retry backoff for failed attempt {failed_attempt}"
            )
        })
    }

    fn validate_retry_budget(&self, max_attempts: u32) -> Result<(), String> {
        let available = self.retry_backoff.len().saturating_add(1);
        if usize::try_from(max_attempts)
            .map(|value| value > available)
            .unwrap_or(true)
        {
            return Err(format!(
                "formed max_attempts {max_attempts} exceeds GitHub Copilot retry capacity {available}"
            ));
        }
        Ok(())
    }

    async fn handle_auth_or_prepare(
        &self,
        input: &InterfaceInvocationEnvelope,
        source_movement_seq: u64,
        attempt: u32,
        handoff: &ProviderOutputHandoff,
        output_root: &Path,
    ) -> Result<Option<Vec<ProviderEmission>>, GitHubCopilotFailure> {
        let manifest = input
            .read_artifact_manifest()
            .map_err(|error| GitHubCopilotFailure::contract(error.to_string()))?;
        if GitHubCredentialManager::handles_label(&manifest.label) {
            let payload = input
                .read_artifact_payload()
                .map_err(|error| GitHubCopilotFailure::contract(error.to_string()))?;
            let credential_handle_fingerprint = self.auth.raw_token_handle_fingerprint();
            let dispatch = GitHubAuthDispatchAdmission {
                handoff,
                output_root,
                provider_interface: self.auth.interface_id(),
                source_movement_seq,
                attempt,
                credential_handle_fingerprint: &credential_handle_fingerprint,
            };
            let outcome = self
                .auth
                .handle_artifact_with_dispatch(&manifest.label, &payload, &dispatch)
                .await?;
            let bytes = serde_json::to_vec(&outcome.payload).map_err(|error| {
                GitHubCopilotFailure::contract(format!(
                    "GitHub auth output could not be encoded: {error}"
                ))
            })?;
            let artifact = InterfaceArtifactFile::write(output_root, outcome.label, &bytes)
                .map_err(|error| {
                    GitHubCopilotFailure::contract(format!(
                        "GitHub auth output could not be materialized: {error}"
                    ))
                })?;
            return Ok(Some(vec![ProviderEmission {
                artifact,
                route: ProviderEmissionRoute::ClientProjection,
            }]));
        }
        self.auth.ensure_copilot_api_token().await?;
        Ok(None)
    }

    fn filter_model_catalog(
        &self,
        emissions: Vec<ProviderEmission>,
        output_root: &Path,
        formed_models: &BTreeSet<String>,
    ) -> Result<Vec<ProviderEmission>, GitHubCopilotFailure> {
        if emissions.len() != 1 {
            return Err(GitHubCopilotFailure::contract(format!(
                "GitHub Copilot model catalog produced {} artifacts instead of one",
                emissions.len()
            )));
        }
        let emission = emissions
            .into_iter()
            .next()
            .ok_or_else(|| GitHubCopilotFailure::contract("model catalog artifact disappeared"))?;
        if emission.route != ProviderEmissionRoute::ClientProjection {
            return Err(GitHubCopilotFailure::contract(
                "GitHub Copilot model catalog was not routed to the client projection",
            ));
        }
        let manifest = emission
            .artifact
            .read_manifest()
            .map_err(|error| GitHubCopilotFailure::contract(error.to_string()))?;
        if manifest.label != MODELS_CATALOG_LABEL {
            return Err(GitHubCopilotFailure::contract(format!(
                "GitHub Copilot model catalog produced artifact label '{}' instead of '{MODELS_CATALOG_LABEL}'",
                manifest.label
            )));
        }
        let bytes = emission
            .artifact
            .read_payload_bytes()
            .map_err(|error| GitHubCopilotFailure::contract(error.to_string()))?;
        let mut payload: Value = serde_json::from_slice(&bytes).map_err(|error| {
            GitHubCopilotFailure::contract(format!(
                "GitHub Copilot model catalog artifact is not JSON: {error}"
            ))
        })?;
        let object = payload.as_object_mut().ok_or_else(|| {
            GitHubCopilotFailure::contract("GitHub Copilot model catalog artifact is not an object")
        })?;
        if object.get("label").and_then(Value::as_str) != Some(MODELS_CATALOG_LABEL) {
            return Err(GitHubCopilotFailure::contract(
                "GitHub Copilot model catalog payload has an invalid label",
            ));
        }
        let models = object
            .remove("models")
            .and_then(|value| value.as_array().cloned())
            .ok_or_else(|| {
                GitHubCopilotFailure::contract(
                    "GitHub Copilot model catalog payload has no models array",
                )
            })?;
        let input_count = models.len();
        let filtered = models
            .into_iter()
            .enumerate()
            .filter_map(|(index, model)| {
                match self.catalog_model_is_supported(&model, index, formed_models) {
                    Ok(true) => Some(Ok(model)),
                    Ok(false) => None,
                    Err(failure) => Some(Err(failure)),
                }
            })
            .collect::<Result<Vec<_>, GitHubCopilotFailure>>()?;
        if filtered.len() != 1 {
            return Err(GitHubCopilotFailure::contract(format!(
                "GitHub Copilot exact catalog did not contain exactly one '{}' model",
                self.model
            )));
        }
        object.insert("models".to_string(), Value::Array(filtered));
        object.insert(
            "interface_filter_evidence".to_string(),
            json!({
                "wire": self.wire,
                "required_model": self.model,
                "required_endpoint": self.catalog_filter.required_endpoint,
                "required_capability_type": self.catalog_filter.required_capability_type,
                "input_model_count": input_count,
                "admitted_model_count": 1
            }),
        );
        let filtered_bytes = serde_json::to_vec(&payload).map_err(|error| {
            GitHubCopilotFailure::contract(format!(
                "GitHub Copilot filtered model catalog could not be encoded: {error}"
            ))
        })?;
        let artifact =
            InterfaceArtifactFile::write(output_root, MODELS_CATALOG_LABEL, &filtered_bytes)
                .map_err(|error| {
                    GitHubCopilotFailure::contract(format!(
                        "GitHub Copilot filtered model catalog could not be materialized: {error}"
                    ))
                })?;
        Ok(vec![ProviderEmission {
            artifact,
            route: ProviderEmissionRoute::ClientProjection,
        }])
    }

    fn catalog_model_is_supported(
        &self,
        model: &Value,
        index: usize,
        formed_models: &BTreeSet<String>,
    ) -> Result<bool, GitHubCopilotFailure> {
        let object = model.as_object().ok_or_else(|| {
            GitHubCopilotFailure::contract(format!(
                "GitHub Copilot model catalog item {index} is not an object"
            ))
        })?;
        let model_id = object
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.trim() == *value)
            .ok_or_else(|| {
                GitHubCopilotFailure::contract(format!(
                    "GitHub Copilot model catalog item {index} has no valid id"
                ))
            })?;
        if model_id != self.model || !formed_models.contains(model_id) {
            return Ok(false);
        }
        if let Some(enabled) = object.get("model_picker_enabled") {
            let enabled = enabled.as_bool().ok_or_else(|| {
                GitHubCopilotFailure::contract(format!(
                    "GitHub Copilot model catalog item {index} has a non-boolean model_picker_enabled"
                ))
            })?;
            if !enabled {
                return Ok(false);
            }
        }
        let capability_type = object
            .get("capabilities")
            .and_then(Value::as_object)
            .and_then(|capabilities| capabilities.get("type"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                GitHubCopilotFailure::contract(format!(
                    "GitHub Copilot model catalog item {index} has no capabilities.type"
                ))
            })?;
        if capability_type != self.catalog_filter.required_capability_type {
            return Ok(false);
        }
        let endpoints = object
            .get("supported_endpoints")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                GitHubCopilotFailure::contract(format!(
                    "GitHub Copilot model catalog item {index} has no supported_endpoints array"
                ))
            })?;
        for (endpoint_index, endpoint) in endpoints.iter().enumerate() {
            let endpoint = endpoint.as_str().filter(|value| {
                !value.is_empty() && value.trim() == *value && !value.contains(['\r', '\n'])
            });
            let Some(endpoint) = endpoint else {
                return Err(GitHubCopilotFailure::contract(format!(
                    "GitHub Copilot model catalog item {index} has invalid supported_endpoints[{endpoint_index}]"
                )));
            };
            if endpoint == self.catalog_filter.required_endpoint {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl GitHubCopilotResponsesDriver {
    pub fn from_descriptor(
        descriptor: &InterfaceDescriptor,
        activation_secrets_home: PathBuf,
    ) -> Result<Self, String> {
        let support = CopilotSupport::from_descriptor(
            descriptor,
            &activation_secrets_home,
            RESPONSES_INTERFACE_DRIVER_REF,
            "/responses",
            "openai_responses",
        )?;
        let mut adjusted = support.adjusted_descriptor(descriptor, "openai_responses_native")?;
        adjusted
            .interface_driver_config
            .as_mut()
            .ok_or_else(|| "GitHub Copilot adjusted driver config disappeared".to_string())?
            .private_config
            .insert(
                "auth_source".to_string(),
                Value::String("secret".to_string()),
            );
        let responses = OpenAiResponsesDriver::from_descriptor(&adjusted, activation_secrets_home)?;
        Ok(Self { support, responses })
    }

    pub fn retry_backoff(&self, failed_attempt: u32) -> Result<Duration, String> {
        self.support.retry_backoff(failed_attempt)
    }

    pub fn validate_retry_budget(&self, max_attempts: u32) -> Result<(), String> {
        self.support.validate_retry_budget(max_attempts)
    }

    pub fn enforce_formed_model_policy(
        &self,
        input: &InterfaceInvocationEnvelope,
    ) -> Result<(), GitHubCopilotFailure> {
        let manifest = input
            .read_artifact_manifest()
            .map_err(|error| GitHubCopilotFailure::contract(error.to_string()))?;
        if GitHubCredentialManager::handles_label(&manifest.label) {
            return Ok(());
        }
        self.responses
            .enforce_formed_model_policy(input)
            .map_err(Into::into)
    }

    pub async fn receive_artifact_and_materialize_outputs(
        &self,
        input: InterfaceInvocationEnvelope,
        source_movement_seq: u64,
        attempt: u32,
        handoff: &ProviderOutputHandoff,
        output_root: &Path,
    ) -> Result<Vec<ProviderEmission>, GitHubCopilotFailure> {
        let is_model_catalog = input.read_artifact_manifest()?.label == MODELS_LIST_LABEL;
        let formed_models = is_model_catalog
            .then(|| required_formed_models(&input))
            .transpose()?;
        if let Some(outputs) = self
            .support
            .handle_auth_or_prepare(&input, source_movement_seq, attempt, handoff, output_root)
            .await?
        {
            return Ok(outputs);
        }
        let emissions = self
            .responses
            .receive_artifact_and_materialize_outputs(
                input,
                source_movement_seq,
                attempt,
                handoff,
                output_root,
            )
            .await
            .map_err(GitHubCopilotFailure::from)?;
        if is_model_catalog {
            self.support.filter_model_catalog(
                emissions,
                output_root,
                formed_models.as_ref().ok_or_else(|| {
                    GitHubCopilotFailure::contract(
                        "GitHub Copilot formed model catalog policy disappeared",
                    )
                })?,
            )
        } else {
            Ok(emissions)
        }
    }
}

impl GitHubCopilotAnthropicDriver {
    pub fn from_descriptor(
        descriptor: &InterfaceDescriptor,
        activation_secrets_home: PathBuf,
    ) -> Result<Self, String> {
        let support = CopilotSupport::from_descriptor(
            descriptor,
            &activation_secrets_home,
            ANTHROPIC_INTERFACE_DRIVER_REF,
            "/v1/messages",
            "anthropic_messages",
        )?;
        let adjusted = support.adjusted_descriptor(descriptor, "anthropic_messages")?;
        let anthropic =
            AnthropicMessagesDriver::from_descriptor(&adjusted, activation_secrets_home)?;
        Ok(Self { support, anthropic })
    }

    pub fn retry_backoff(&self, failed_attempt: u32) -> Result<Duration, String> {
        self.support.retry_backoff(failed_attempt)
    }

    pub fn validate_retry_budget(&self, max_attempts: u32) -> Result<(), String> {
        self.support.validate_retry_budget(max_attempts)
    }

    pub fn enforce_formed_model_policy(
        &self,
        input: &InterfaceInvocationEnvelope,
    ) -> Result<(), GitHubCopilotFailure> {
        let manifest = input
            .read_artifact_manifest()
            .map_err(|error| GitHubCopilotFailure::contract(error.to_string()))?;
        if GitHubCredentialManager::handles_label(&manifest.label) {
            return Ok(());
        }
        self.anthropic
            .enforce_formed_model_policy(input)
            .map_err(Into::into)
    }

    pub fn tool_name_prefix(&self) -> Option<&str> {
        self.anthropic.tool_name_prefix()
    }

    pub async fn receive_artifact_and_materialize_outputs(
        &self,
        input: InterfaceInvocationEnvelope,
        source_movement_seq: u64,
        attempt: u32,
        handoff: &ProviderOutputHandoff,
        output_root: &Path,
    ) -> Result<Vec<ProviderEmission>, GitHubCopilotFailure> {
        let is_model_catalog = input.read_artifact_manifest()?.label == MODELS_LIST_LABEL;
        let formed_models = is_model_catalog
            .then(|| required_formed_models(&input))
            .transpose()?;
        if let Some(outputs) = self
            .support
            .handle_auth_or_prepare(&input, source_movement_seq, attempt, handoff, output_root)
            .await?
        {
            return Ok(outputs);
        }
        let emissions = self
            .anthropic
            .receive_artifact_and_materialize_outputs(
                input,
                source_movement_seq,
                attempt,
                handoff,
                output_root,
            )
            .await
            .map_err(GitHubCopilotFailure::from)?;
        if is_model_catalog {
            self.support.filter_model_catalog(
                emissions,
                output_root,
                formed_models.as_ref().ok_or_else(|| {
                    GitHubCopilotFailure::contract(
                        "GitHub Copilot formed model catalog policy disappeared",
                    )
                })?,
            )
        } else {
            Ok(emissions)
        }
    }
}

pub fn materialize_responses_failure(
    output_root: &Path,
    failure: &GitHubCopilotFailure,
) -> Result<InterfaceArtifactFile, GitHubCopilotFailure> {
    let responses = ResponsesFailure {
        class: failure.class,
        retryable: failure.retryable,
        status: failure.status,
        code: failure.code.clone(),
        detail: failure.detail.clone(),
        evidence: failure.evidence.clone(),
    };
    octopus_world_provider_responses::materialize_provider_turn_error(output_root, &responses)
        .map_err(Into::into)
}

pub fn materialize_anthropic_failure(
    output_root: &Path,
    failure: &GitHubCopilotFailure,
) -> Result<InterfaceArtifactFile, GitHubCopilotFailure> {
    materialize_responses_failure(output_root, failure)
}

fn validate_config(config: &CopilotConfig, expected_endpoint: &str) -> Result<(), String> {
    if config.model.is_empty()
        || config.model.trim() != config.model
        || config.model.contains('\r')
        || config.model.contains('\n')
    {
        return Err("GitHub Copilot model must be non-empty, trimmed, and single-line".to_string());
    }
    if config.retry_backoff_ms.is_empty() || config.retry_backoff_ms.contains(&0) {
        return Err("GitHub Copilot retry_backoff_ms must contain positive delays".to_string());
    }
    if config.catalog_filter.required_endpoint != expected_endpoint {
        return Err(format!(
            "GitHub Copilot catalog_filter.required_endpoint must be '{expected_endpoint}'"
        ));
    }
    if config.catalog_filter.required_capability_type != "chat" {
        return Err(
            "GitHub Copilot catalog_filter.required_capability_type must be 'chat'".to_string(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(path: &str) -> InterfaceDescriptor {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("interface registry root");
        let bytes = std::fs::read(root.join(path).join("interface.json")).expect("descriptor");
        octopus_descriptors::parse_strict(&bytes).expect("strict descriptor")
    }

    fn catalog_policy_input(models: Value) -> InterfaceInvocationEnvelope {
        let bytes = b"{}";
        let artifact_file = InterfaceArtifactFile::write(
            tempfile::tempdir().expect("catalog input root").keep(),
            MODELS_LIST_LABEL,
            bytes,
        )
        .expect("catalog input");
        let mut grant = octopus_interfaces::GrantProjection::default();
        grant.interface_policy.insert(
            "world_provider_github_copilot".to_string(),
            std::collections::BTreeMap::from([("models".to_string(), models)]),
        );
        InterfaceInvocationEnvelope {
            artifact: octopus_ledger::ArtifactRef {
                id: octopus_descriptors::ArtifactId::new("catalog-policy-input"),
                content: octopus_ledger::ContentHash::of_bytes(bytes),
            },
            artifact_file,
            artifact_binding_payloads: Vec::new(),
            metadata: Some(
                octopus_interfaces::InvocationMetadata::new(grant).with_runtime(
                    octopus_interfaces::RuntimeInvocationMetadata {
                        run: octopus_interfaces::RunId::new("run-catalog-policy"),
                        ee: octopus_descriptors::EeId::new("ee-catalog-policy"),
                        worker: octopus_descriptors::InterfaceId::new("system_worker_test"),
                        checkpoint: octopus_descriptors::CheckpointId::new(
                            "checkpoint-catalog-policy",
                        ),
                        interface: octopus_descriptors::InterfaceId::new(
                            "world_provider_github_copilot",
                        ),
                        epoch: 1,
                        run_record_hash: None,
                    },
                ),
            ),
        }
    }

    #[test]
    fn catalog_policy_fails_closed_before_private_auth() {
        assert_eq!(
            required_formed_models(&catalog_policy_input(json!(["gpt-5.4"])))
                .expect("formed models"),
            BTreeSet::from(["gpt-5.4".to_string()])
        );
        assert!(matches!(
            required_formed_models(&catalog_policy_input(json!([]))),
            Err(GitHubCopilotFailure { class: "unavailable", detail, .. })
                if detail.contains("models is empty")
        ));
        assert!(matches!(
            required_formed_models(&catalog_policy_input(json!(["gpt-5.4", "gpt-5.4"]))),
            Err(GitHubCopilotFailure { class: "unavailable", detail, .. })
                if detail.contains("repeats")
        ));
    }

    #[test]
    fn exact_responses_descriptor_selects_responses_only() {
        let descriptor = descriptor("world_provider_github_copilot");
        let secrets = tempfile::tempdir().expect("secrets");
        let driver = GitHubCopilotResponsesDriver::from_descriptor(
            &descriptor,
            secrets.path().to_path_buf(),
        )
        .expect("responses driver");
        let admitted = json!({
            "id": "gpt-5.4",
            "model_picker_enabled": true,
            "supported_endpoints": ["/responses"],
            "capabilities": {"type": "chat"}
        });
        let wrong_model = json!({
            "id": "gpt-5.3-codex",
            "model_picker_enabled": true,
            "supported_endpoints": ["/responses"],
            "capabilities": {"type": "chat"}
        });
        assert!(driver
            .support
            .catalog_model_is_supported(&admitted, 0, &BTreeSet::from(["gpt-5.4".to_string()]),)
            .expect("catalog"));
        assert!(!driver
            .support
            .catalog_model_is_supported(&wrong_model, 1, &BTreeSet::from(["gpt-5.4".to_string()]),)
            .expect("catalog"));
        assert!(!driver
            .support
            .catalog_model_is_supported(
                &admitted,
                2,
                &BTreeSet::from(["gpt-5.3-codex".to_string()]),
            )
            .expect("catalog"));
    }

    #[test]
    fn exact_anthropic_descriptor_selects_messages_only() {
        let descriptor = descriptor("world_provider_github_copilot_anthropic");
        let secrets = tempfile::tempdir().expect("secrets");
        let driver = GitHubCopilotAnthropicDriver::from_descriptor(
            &descriptor,
            secrets.path().to_path_buf(),
        )
        .expect("anthropic driver");
        let admitted = json!({
            "id": "claude-sonnet-4.6",
            "model_picker_enabled": true,
            "supported_endpoints": ["/v1/messages"],
            "capabilities": {"type": "chat"}
        });
        let wrong_wire = json!({
            "id": "claude-sonnet-4.6",
            "model_picker_enabled": true,
            "supported_endpoints": ["/chat/completions"],
            "capabilities": {"type": "chat"}
        });
        assert!(driver
            .support
            .catalog_model_is_supported(
                &admitted,
                0,
                &BTreeSet::from(["claude-sonnet-4.6".to_string()]),
            )
            .expect("catalog"));
        assert!(!driver
            .support
            .catalog_model_is_supported(
                &wrong_wire,
                1,
                &BTreeSet::from(["claude-sonnet-4.6".to_string()]),
            )
            .expect("catalog"));
        assert!(!driver
            .support
            .catalog_model_is_supported(
                &admitted,
                2,
                &BTreeSet::from(["claude-opus-4.6".to_string()]),
            )
            .expect("catalog"));
    }

    #[test]
    fn responses_descriptor_does_not_claim_unproven_vision() {
        let descriptor = descriptor("world_provider_github_copilot");
        let config = descriptor.interface_driver_config.expect("driver config");
        let content_types = config.private_config["responses_api"]["carrier_policy"]
            ["allowed_content_types"]
            .as_array()
            .expect("content types");
        assert!(!content_types
            .iter()
            .any(|value| value.as_str() == Some("input_image")));
        let transport = config.transport.expect("transport");
        assert!(!transport
            .headers
            .keys()
            .any(|name| name.eq_ignore_ascii_case("Copilot-Vision-Request")));
    }
}
