use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use octopus_azure_auth::AzureEntraCredentialManager;
use octopus_interfaces::{InterfaceDescriptor, InterfaceTransportConfig};
use octopus_provider_credential_pool::{
    CredentialFailure, CredentialPool, CredentialPoolAttempt, CredentialPoolConfig,
    CredentialPoolDecision, CredentialSelection,
};
use octopus_secrets::{SecretName, SecretScope};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::Url;
use serde::Deserialize;
use serde_json::{Map, Value};

pub(crate) const CANONICAL_WIRE: &str = "openai_chat_completions";
pub(crate) const MODELS_LIST_LABEL: &str = "provider.models.list";
pub(crate) const MODELS_CATALOG_LABEL: &str = "provider.models.catalog";

#[derive(Clone)]
pub(crate) struct ProviderConfig {
    pub endpoint: Url,
    pub models_endpoint: Option<Url>,
    pub headers: HeaderMap,
    pub auth: Option<AuthConfig>,
    pub model_selection: ProviderModelSelection,
    pub request: RequestConfig,
    pub response: ResponseConfig,
    pub model_profiles: BTreeMap<String, ProviderModelProfile>,
    pub retry: RetryConfig,
    pub model_catalog: Option<ModelCatalogConfig>,
    pub input_modalities: BTreeSet<InputModality>,
    pub max_response_bytes: usize,
    pub request_timeout: Duration,
}

pub(crate) enum CredentialAttemptStart {
    Unmanaged,
    Selected(CredentialSelection),
    Exhausted {
        next_available_at_unix_ms: Option<u64>,
    },
}

fn map_credential_attempt(attempt: CredentialPoolAttempt) -> CredentialAttemptStart {
    match attempt {
        CredentialPoolAttempt::Selected(selected) => CredentialAttemptStart::Selected(selected),
        CredentialPoolAttempt::Exhausted {
            next_available_at_unix_ms,
        } => CredentialAttemptStart::Exhausted {
            next_available_at_unix_ms,
        },
    }
}

#[derive(Clone)]
pub(crate) enum ProviderModelSelection {
    Literal(String),
    Environment(String),
}

#[derive(Clone)]
pub(crate) struct AuthConfig {
    pub header: HeaderName,
    pub value_prefix: String,
    pub source: AuthSource,
}

#[derive(Clone)]
pub(crate) enum AuthSource {
    Environment(String),
    Secret {
        octopus_home: PathBuf,
        scope: SecretScope,
        name: SecretName,
    },
    AzureEntra(Box<AzureEntraCredentialManager>),
    CredentialPool {
        octopus_home: PathBuf,
        pool: Arc<CredentialPool>,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FamilyConfig {
    pub request: RequestConfig,
    pub response: ResponseConfig,
    #[serde(default)]
    pub model_catalog: Option<ModelCatalogConfig>,
    pub input_modalities: BTreeSet<InputModality>,
    #[serde(default)]
    pub model_profiles: BTreeMap<String, ProviderModelProfile>,
    #[serde(default)]
    pub header_env: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProviderModelProfile {
    #[serde(default)]
    pub request: Option<RequestConfig>,
    #[serde(default)]
    pub response: Option<ResponseConfig>,
    #[serde(default)]
    pub input_modalities: Option<BTreeSet<InputModality>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelCatalogConfig {
    pub request_label: String,
    pub output_label: String,
    pub path: String,
    #[serde(default = "default_models_pointer")]
    pub models_pointer: String,
    #[serde(default = "default_model_id_pointer")]
    pub id_pointer: String,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransportDefaults {
    request_timeout_ms: u64,
    max_response_bytes: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequestConfig {
    pub path: String,
    pub stream: bool,
    pub require_done: bool,
    pub message_content: MessageContentMode,
    #[serde(default)]
    pub model_tools: ModelToolsMode,
    #[serde(default)]
    pub tool_message_content: ToolMessageContentMode,
    #[serde(default)]
    pub system_content_part_fields: Map<String, Value>,
    pub retryable_statuses: BTreeSet<u16>,
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    #[serde(default)]
    pub body: Map<String, Value>,
    #[serde(default)]
    pub artifact_body: Option<ArtifactBodyConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArtifactBodyConfig {
    pub field: String,
    pub allowed_keys: BTreeSet<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResponseConfig {
    pub reasoning_fields: Vec<String>,
    pub reasoning_detail_fields: Vec<String>,
    pub successful_finish_reasons: BTreeSet<String>,
    pub nonreplayable_finish_reasons: BTreeSet<String>,
    #[serde(default)]
    pub top_level_artifacts: Vec<TopLevelArtifactConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TopLevelArtifactConfig {
    pub field: String,
    pub artifact_label: String,
    pub required: bool,
    pub value_types: BTreeSet<JsonValueType>,
    pub stream_accumulation: StreamAccumulation,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetryConfig {
    pub backoff_ms: Vec<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InputModality {
    Text,
    ImageUrl,
    InputAudio,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MessageContentMode {
    String,
    Parts,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ModelToolsMode {
    #[default]
    Native,
    Disabled,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolMessageContentMode {
    #[default]
    Preserve,
    TextOnly,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub(crate) enum JsonValueType {
    Null,
    Boolean,
    Number,
    String,
    Array,
    Object,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StreamAccumulation {
    Identical,
    UniqueArrayItems,
}

impl ProviderConfig {
    pub fn from_descriptor(
        descriptor: &InterfaceDescriptor,
        secret_home: Option<&Path>,
        interface_var_dir: Option<&Path>,
    ) -> Result<Self, String> {
        let interface_config = descriptor.interface_driver_config.as_ref().ok_or_else(|| {
            format!(
                "provider interface '{}' has no interface_driver_config",
                descriptor.id
            )
        })?;
        let transport = interface_config.transport.as_ref().ok_or_else(|| {
            format!(
                "provider interface '{}' declares no interface_driver_config.transport",
                descriptor.id
            )
        })?;
        if transport.wire.as_deref() != Some(CANONICAL_WIRE) {
            return Err(format!(
                "provider interface '{}' must declare transport.wire '{}' for this family",
                descriptor.id, CANONICAL_WIRE
            ));
        }
        let family_value = interface_config
            .private_config
            .get("openai_compatible")
            .cloned()
            .ok_or_else(|| {
                format!(
                    "provider interface '{}' has no private_config.openai_compatible object",
                    descriptor.id
                )
            })?;
        let family: FamilyConfig = serde_json::from_value(family_value).map_err(|error| {
            format!(
                "provider interface '{}' openai_compatible config is malformed: {error}",
                descriptor.id
            )
        })?;
        family.validate()?;
        let retry: RetryConfig = serde_json::from_value(
            interface_config
                .private_config
                .get("openai_compatible_retry")
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "provider interface '{}' has no private_config.openai_compatible_retry object",
                        descriptor.id
                    )
                })?,
        )
        .map_err(|error| {
            format!(
                "provider interface '{}' openai_compatible_retry config is malformed: {error}",
                descriptor.id
            )
        })?;
        retry.validate()?;
        let transport_defaults = interface_config
            .private_config
            .get("openai_compatible_transport_defaults")
            .cloned()
            .map(serde_json::from_value::<TransportDefaults>)
            .transpose()
            .map_err(|error| {
                format!(
                    "provider interface '{}' openai_compatible_transport_defaults is malformed: {error}",
                    descriptor.id
                )
            })?;
        if let Some(defaults) = transport_defaults {
            defaults.validate()?;
        }
        let endpoint = resolve_endpoint(transport, &family.request)?;
        let models_endpoint = family
            .model_catalog
            .as_ref()
            .map(|catalog| resolve_relative_endpoint(transport, &catalog.path, &BTreeMap::new()))
            .transpose()?;
        let headers = resolve_headers(transport, &family.header_env)?;
        let credential_pool = resolve_credential_pool(
            descriptor,
            &interface_config.private_config,
            secret_home,
            interface_var_dir,
        )?;
        let auth = resolve_auth(descriptor, transport, secret_home, credential_pool)?;
        if let Some(auth) = &auth {
            if headers.contains_key(&auth.header) {
                return Err(format!(
                    "provider interface '{}' configures auth header '{}' twice",
                    descriptor.id, auth.header
                ));
            }
        }
        let model_selection = resolve_provider_model_selection(&interface_config.private_config)?;
        let max_response_bytes = transport
            .max_response_bytes
            .or(transport_defaults.map(|defaults| defaults.max_response_bytes))
            .ok_or_else(|| "provider transport requires finite max_response_bytes".to_string())?
            .try_into()
            .map_err(|_| "provider max_response_bytes exceeds host width".to_string())?;
        if max_response_bytes == 0 {
            return Err("provider max_response_bytes must be positive".to_string());
        }
        let request_timeout_ms = transport
            .request_timeout_ms
            .or(transport_defaults.map(|defaults| defaults.request_timeout_ms))
            .ok_or_else(|| "provider transport requires request_timeout_ms".to_string())?;
        if request_timeout_ms == 0 {
            return Err("provider request_timeout_ms must be positive".to_string());
        }
        Ok(Self {
            endpoint,
            models_endpoint,
            headers,
            auth,
            model_selection,
            request: family.request,
            response: family.response,
            model_profiles: family.model_profiles,
            retry,
            model_catalog: family.model_catalog,
            input_modalities: family.input_modalities,
            max_response_bytes,
            request_timeout: Duration::from_millis(request_timeout_ms),
        })
    }

    pub fn resolve_provider_model(&self) -> Result<String, String> {
        match &self.model_selection {
            ProviderModelSelection::Literal(model) => Ok(model.clone()),
            ProviderModelSelection::Environment(name) => required_env(name, "provider model"),
        }
    }

    pub fn for_provider_model(&self, model: &str) -> Result<Self, String> {
        if self.model_profiles.is_empty() {
            return Ok(self.clone());
        }
        let profile = self.model_profiles.get(model).ok_or_else(|| {
            format!("openai_compatible selected model '{model}' has no exact model_profiles entry")
        })?;
        let mut resolved = self.clone();
        if let Some(request) = &profile.request {
            resolved.request = request.clone();
        }
        if let Some(response) = &profile.response {
            resolved.response = response.clone();
        }
        if let Some(input_modalities) = &profile.input_modalities {
            resolved.input_modalities = input_modalities.clone();
        }
        resolved.model_profiles.clear();
        Ok(resolved)
    }

    pub fn start_credential_attempt(
        &self,
        selected: Option<&CredentialSelection>,
        now_unix_ms: u64,
    ) -> Result<CredentialAttemptStart, String> {
        match self.credential_pool() {
            Some(pool) => pool
                .start_attempt(selected, now_unix_ms)
                .map(map_credential_attempt),
            None if selected.is_some() => {
                Err("provider received a credential-pool selection without a pool".to_string())
            }
            None => Ok(CredentialAttemptStart::Unmanaged),
        }
    }

    pub fn select_credential_candidate(
        &self,
        selected: Option<&CredentialSelection>,
        now_unix_ms: u64,
    ) -> Result<CredentialAttemptStart, String> {
        match self.credential_pool() {
            Some(pool) => pool
                .select_candidate(selected, now_unix_ms)
                .map(map_credential_attempt),
            None if selected.is_some() => {
                Err("provider received a credential-pool selection without a pool".to_string())
            }
            None => Ok(CredentialAttemptStart::Unmanaged),
        }
    }

    pub fn credential_pool_manages_status(&self, status: u16) -> bool {
        self.credential_pool()
            .is_some_and(|pool| pool.manages_status(status))
    }

    pub fn record_credential_success(
        &self,
        selected: Option<&CredentialSelection>,
        now_unix_ms: u64,
    ) -> Result<(), String> {
        match (self.credential_pool(), selected) {
            (Some(pool), Some(selected)) => pool.record_success(selected, now_unix_ms),
            (Some(_), None) => {
                Err("provider credential-pool success has no selected credential".to_string())
            }
            (None, Some(_)) => {
                Err("provider credential success refers to an undeclared pool".to_string())
            }
            (None, None) => Ok(()),
        }
    }

    pub fn record_credential_failure(
        &self,
        selected: Option<&CredentialSelection>,
        failure: CredentialFailure,
        now_unix_ms: u64,
    ) -> Result<Option<CredentialPoolDecision>, String> {
        match (self.credential_pool(), selected) {
            (Some(pool), Some(selected)) => pool
                .record_failure(selected, failure, now_unix_ms)
                .map(Some),
            (Some(_), None) => {
                Err("provider credential-pool failure has no selected credential".to_string())
            }
            (None, Some(_)) => {
                Err("provider credential failure refers to an undeclared pool".to_string())
            }
            (None, None) => Ok(None),
        }
    }

    pub fn credential_handle_fingerprint(
        &self,
        selected: Option<&CredentialSelection>,
    ) -> Result<Option<String>, String> {
        match (self.auth.as_ref().map(|auth| &auth.source), selected) {
            (Some(AuthSource::CredentialPool { .. }), Some(selected)) => {
                Ok(Some(selected.fingerprint().to_string()))
            }
            (Some(AuthSource::CredentialPool { .. }), None) => {
                Err("provider credential pool has no selected credential".to_string())
            }
            (Some(AuthSource::Secret { scope, name, .. }), None) => {
                let reference = format!("secret://{}/{}", scope.name().unwrap_or("global"), name);
                Ok(Some(
                    blake3::hash(reference.as_bytes()).to_hex().to_string(),
                ))
            }
            (Some(AuthSource::AzureEntra(manager)), None) => {
                Ok(Some(manager.credential_source_fingerprint().to_string()))
            }
            (_, Some(_)) => {
                Err("provider credential selection is outside its auth source".to_string())
            }
            _ => Ok(None),
        }
    }

    fn credential_pool(&self) -> Option<&CredentialPool> {
        let AuthSource::CredentialPool { pool, .. } = &self.auth.as_ref()?.source else {
            return None;
        };
        Some(pool)
    }

    pub fn resolve_request_body(&self, artifact: &Value) -> Result<Map<String, Value>, String> {
        let mut body = self.request.body.clone();
        let Some(config) = &self.request.artifact_body else {
            return Ok(body);
        };
        let Some(value) = artifact.get(&config.field) else {
            return Ok(body);
        };
        let values = value.as_object().ok_or_else(|| {
            format!(
                "openai_compatible artifact field '{}' must be an object",
                config.field
            )
        })?;
        for (key, value) in values {
            if !config.allowed_keys.contains(key) {
                return Err(format!(
                    "openai_compatible artifact field '{}' may not set request key '{key}'",
                    config.field
                ));
            }
            body.insert(key.clone(), value.clone());
        }
        Ok(body)
    }
}

impl TransportDefaults {
    fn validate(self) -> Result<(), String> {
        if self.request_timeout_ms == 0 || self.max_response_bytes == 0 {
            return Err(
                "openai_compatible transport defaults must be positive finite values".to_string(),
            );
        }
        Ok(())
    }
}

impl FamilyConfig {
    fn validate(&self) -> Result<(), String> {
        if !self.input_modalities.contains(&InputModality::Text) {
            return Err("openai_compatible input_modalities must include text".to_string());
        }
        self.request.validate()?;
        self.response.validate()?;
        if let Some(catalog) = &self.model_catalog {
            catalog.validate()?;
        }
        for (model, profile) in &self.model_profiles {
            require_trimmed(model, "openai_compatible.model_profiles key")?;
            if model.contains(['\r', '\n', '\0']) {
                return Err(
                    "openai_compatible.model_profiles keys must be single-line model names"
                        .to_string(),
                );
            }
            profile.validate(model)?;
        }
        for (header, env_name) in &self.header_env {
            let header = parse_header_name(header, "openai_compatible.header_env")?;
            reject_secret_bearing_generic_header(&header, "openai_compatible.header_env")?;
            require_trimmed(env_name, "openai_compatible.header_env value")?;
        }
        Ok(())
    }
}

impl ProviderModelProfile {
    fn validate(&self, model: &str) -> Result<(), String> {
        if let Some(request) = &self.request {
            request.validate().map_err(|error| {
                format!("openai_compatible.model_profiles['{model}'].request: {error}")
            })?;
        }
        if let Some(response) = &self.response {
            response.validate().map_err(|error| {
                format!("openai_compatible.model_profiles['{model}'].response: {error}")
            })?;
        }
        if let Some(modalities) = &self.input_modalities {
            if !modalities.contains(&InputModality::Text) {
                return Err(format!(
                    "openai_compatible.model_profiles['{model}'].input_modalities must include text"
                ));
            }
        }
        Ok(())
    }
}

impl ModelCatalogConfig {
    fn validate(&self) -> Result<(), String> {
        require_trimmed(
            &self.request_label,
            "openai_compatible.model_catalog.request_label",
        )?;
        require_trimmed(
            &self.output_label,
            "openai_compatible.model_catalog.output_label",
        )?;
        if self.request_label == self.output_label {
            return Err(
                "openai_compatible model catalog request and output labels must differ".to_string(),
            );
        }
        if self.request_label != MODELS_LIST_LABEL || self.output_label != MODELS_CATALOG_LABEL {
            return Err(format!(
                "openai_compatible model catalog labels must be '{MODELS_LIST_LABEL}' and '{MODELS_CATALOG_LABEL}'"
            ));
        }
        validate_relative_path(&self.path, "openai_compatible.model_catalog.path")?;
        validate_json_pointer(
            &self.models_pointer,
            "openai_compatible.model_catalog.models_pointer",
        )?;
        validate_json_pointer(
            &self.id_pointer,
            "openai_compatible.model_catalog.id_pointer",
        )
    }
}

impl RequestConfig {
    fn validate(&self) -> Result<(), String> {
        require_trimmed(&self.path, "openai_compatible.request.path")?;
        if self.path.starts_with('/')
            || self.path.contains('?')
            || self.path.contains('#')
            || self.path.split('/').any(|part| matches!(part, "." | ".."))
        {
            return Err(
                "openai_compatible.request.path must be a safe relative URL path".to_string(),
            );
        }
        for status in &self.retryable_statuses {
            if !(100..=599).contains(status) || (200..=399).contains(status) {
                return Err(format!(
                    "openai_compatible retryable status '{status}' is invalid"
                ));
            }
        }
        for (key, value) in &self.query {
            require_trimmed(key, "openai_compatible.request.query key")?;
            if key.contains(['&', '=', '#']) {
                return Err(format!(
                    "openai_compatible request query key '{key}' is invalid"
                ));
            }
            require_trimmed(value, "openai_compatible.request.query value")?;
        }
        if !self.system_content_part_fields.is_empty()
            && self.message_content != MessageContentMode::Parts
        {
            return Err(
                "openai_compatible.request.system_content_part_fields requires message_content 'parts'"
                    .to_string(),
            );
        }
        for key in self.system_content_part_fields.keys() {
            validate_json_field_name(key, "openai_compatible.request.system_content_part_fields")?;
            if matches!(key.as_str(), "type" | "text") {
                return Err(format!(
                    "openai_compatible.request.system_content_part_fields may not override family-owned '{key}'"
                ));
            }
        }
        for reserved in ["model", "messages", "tools", "stream"] {
            if self.body.contains_key(reserved) {
                return Err(format!(
                    "openai_compatible.request.body may not override family-owned '{reserved}'"
                ));
            }
        }
        if let Some(config) = &self.artifact_body {
            validate_json_field_name(
                &config.field,
                "openai_compatible.request.artifact_body.field",
            )?;
            if config.allowed_keys.is_empty() {
                return Err(
                    "openai_compatible.request.artifact_body.allowed_keys must not be empty"
                        .to_string(),
                );
            }
            for key in &config.allowed_keys {
                validate_request_body_key(
                    key,
                    "openai_compatible.request.artifact_body.allowed_keys",
                )?;
            }
        }
        Ok(())
    }
}

impl ResponseConfig {
    fn validate(&self) -> Result<(), String> {
        validate_field_names(&self.reasoning_fields, "reasoning_fields")?;
        validate_field_names(&self.reasoning_detail_fields, "reasoning_detail_fields")?;
        validate_finish_reasons(
            &self.successful_finish_reasons,
            "successful_finish_reasons",
            false,
        )?;
        validate_finish_reasons(
            &self.nonreplayable_finish_reasons,
            "nonreplayable_finish_reasons",
            true,
        )?;
        if !self
            .successful_finish_reasons
            .is_disjoint(&self.nonreplayable_finish_reasons)
        {
            return Err(
                "openai_compatible response finish-reason sets must be disjoint".to_string(),
            );
        }
        let mut fields = BTreeSet::new();
        let mut labels = BTreeSet::new();
        for (index, artifact) in self.top_level_artifacts.iter().enumerate() {
            artifact.validate(index)?;
            if !fields.insert(&artifact.field) {
                return Err(format!(
                    "openai_compatible.response.top_level_artifacts repeats field '{}'",
                    artifact.field
                ));
            }
            if !labels.insert(&artifact.artifact_label) {
                return Err(format!(
                    "openai_compatible.response.top_level_artifacts repeats artifact_label '{}'",
                    artifact.artifact_label
                ));
            }
        }
        Ok(())
    }
}

impl TopLevelArtifactConfig {
    fn validate(&self, index: usize) -> Result<(), String> {
        let surface = format!("openai_compatible.response.top_level_artifacts[{index}]");
        validate_json_field_name(&self.field, &format!("{surface}.field"))?;
        if matches!(
            self.field.as_str(),
            "id" | "model" | "created" | "object" | "choices" | "usage" | "error"
        ) {
            return Err(format!(
                "{surface}.field '{}' is owned by the Chat Completions family",
                self.field
            ));
        }
        validate_artifact_label(&self.artifact_label, &format!("{surface}.artifact_label"))?;
        if matches!(
            self.artifact_label.as_str(),
            "interface.artifact.output"
                | "tool.call"
                | "provider.tool_results.pending"
                | "turn.error"
                | MODELS_LIST_LABEL
                | MODELS_CATALOG_LABEL
        ) {
            return Err(format!(
                "{surface}.artifact_label '{}' collides with a family-owned artifact label",
                self.artifact_label
            ));
        }
        if self.value_types.is_empty() {
            return Err(format!("{surface}.value_types must not be empty"));
        }
        if self.stream_accumulation == StreamAccumulation::UniqueArrayItems
            && (!self.value_types.contains(&JsonValueType::Array)
                || self
                    .value_types
                    .iter()
                    .any(|value| !matches!(value, JsonValueType::Array | JsonValueType::Null)))
        {
            return Err(format!(
                "{surface}.stream_accumulation 'unique_array_items' requires value_types containing array and optionally null"
            ));
        }
        Ok(())
    }

    pub fn accepts(&self, value: &Value) -> bool {
        self.value_types.contains(&JsonValueType::of(value))
    }
}

impl JsonValueType {
    fn of(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(_) => Self::Boolean,
            Value::Number(_) => Self::Number,
            Value::String(_) => Self::String,
            Value::Array(_) => Self::Array,
            Value::Object(_) => Self::Object,
        }
    }
}

impl RetryConfig {
    fn validate(&self) -> Result<(), String> {
        if self.backoff_ms.is_empty() {
            return Err("openai_compatible retry.backoff_ms must not be empty".to_string());
        }
        if self.backoff_ms.contains(&0) {
            return Err(
                "openai_compatible retry.backoff_ms must contain only positive delays".to_string(),
            );
        }
        Ok(())
    }
}

fn resolve_endpoint(
    transport: &InterfaceTransportConfig,
    request: &RequestConfig,
) -> Result<Url, String> {
    resolve_relative_endpoint(transport, &request.path, &request.query)
}

fn resolve_relative_endpoint(
    transport: &InterfaceTransportConfig,
    relative_path: &str,
    query: &BTreeMap<String, String>,
) -> Result<Url, String> {
    let direct = transport.base_url.trim();
    let env_name = transport
        .base_url_env
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let base = match (direct.is_empty(), env_name) {
        (false, None) => direct.to_string(),
        (true, Some(name)) => required_env(name, "provider endpoint")?,
        (false, Some(name)) => {
            return Err(format!(
                "provider transport declares both base_url and base_url_env '{name}'"
            ));
        }
        (true, None) => {
            return Err(
                "provider transport requires exactly one of base_url or base_url_env".to_string(),
            );
        }
    };
    let mut url = validate_base_url(&base)?;
    let mut path = url.path().trim_end_matches('/').to_string();
    path.push('/');
    path.push_str(relative_path.trim_matches('/'));
    url.set_path(&path);
    {
        let mut pairs = url.query_pairs_mut();
        for (name, value) in query {
            pairs.append_pair(name, value);
        }
    }
    Ok(url)
}

fn validate_relative_path(path: &str, surface: &str) -> Result<(), String> {
    require_trimmed(path, surface)?;
    if path.starts_with('/')
        || path.contains('?')
        || path.contains('#')
        || path.split('/').any(|part| matches!(part, "." | ".."))
    {
        return Err(format!("{surface} must be a safe relative URL path"));
    }
    Ok(())
}

fn resolve_headers(
    transport: &InterfaceTransportConfig,
    header_env: &BTreeMap<String, String>,
) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::with_capacity(transport.headers.len() + header_env.len());
    for (name, value) in &transport.headers {
        let name = parse_header_name(name, "provider transport header")?;
        reject_secret_bearing_generic_header(&name, "provider transport header")?;
        let value = HeaderValue::from_str(value)
            .map_err(|_| format!("provider transport header '{name}' has an invalid value"))?;
        headers.insert(name, value);
    }
    for (name, env_name) in header_env {
        let name = parse_header_name(name, "openai_compatible.header_env")?;
        reject_secret_bearing_generic_header(&name, "openai_compatible.header_env")?;
        if headers.contains_key(&name) {
            return Err(format!("provider header '{name}' is configured twice"));
        }
        let value = required_env(env_name, &format!("provider header '{name}'"))?;
        let value = HeaderValue::from_str(&value)
            .map_err(|_| format!("provider header env '{env_name}' has an invalid value"))?;
        headers.insert(name, value);
    }
    Ok(headers)
}

fn resolve_credential_pool(
    descriptor: &InterfaceDescriptor,
    metadata: &Map<String, Value>,
    secret_home: Option<&Path>,
    interface_var_dir: Option<&Path>,
) -> Result<Option<Arc<CredentialPool>>, String> {
    let Some(value) = metadata.get("credential_pool").cloned() else {
        return Ok(None);
    };
    let config: CredentialPoolConfig = serde_json::from_value(value).map_err(|error| {
        format!(
            "provider interface '{}' credential_pool config is malformed: {error}",
            descriptor.id
        )
    })?;
    let _ = secret_home.ok_or_else(|| {
        "provider credential_pool requires activation_secrets_home root slot".to_string()
    })?;
    let interface_var_dir = interface_var_dir.ok_or_else(|| {
        "provider credential_pool requires an exact interface var directory".to_string()
    })?;
    CredentialPool::open(
        descriptor.id.as_str(),
        config,
        interface_var_dir.join("credential_pool").join("state.json"),
    )
    .map(Arc::new)
    .map(Some)
}

fn resolve_auth(
    descriptor: &InterfaceDescriptor,
    transport: &InterfaceTransportConfig,
    secret_home: Option<&Path>,
    credential_pool: Option<Arc<CredentialPool>>,
) -> Result<Option<AuthConfig>, String> {
    let env_name = transport
        .auth_env_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let secret_ref = transport
        .auth_secret_ref
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let azure_entra = match secret_home {
        Some(secret_home) => AzureEntraCredentialManager::from_descriptor(descriptor, secret_home)
            .map_err(|error| error.to_string())?,
        None if descriptor
            .interface_driver_config
            .as_ref()
            .is_some_and(|config| config.private_config.contains_key("azure_entra")) =>
        {
            return Err(
                "provider Azure Entra auth requires activation_secrets_home root slot".to_string(),
            );
        }
        None => None,
    };
    let source = if let Some(pool) = credential_pool {
        if env_name.is_some() || secret_ref.is_some() || azure_entra.is_some() {
            return Err(
                "provider credential_pool cannot be combined with another auth source".to_string(),
            );
        }
        AuthSource::CredentialPool {
            octopus_home: secret_home
                .ok_or_else(|| {
                    "provider credential_pool requires activation_secrets_home root slot"
                        .to_string()
                })?
                .to_path_buf(),
            pool,
        }
    } else {
        match (env_name, secret_ref, azure_entra) {
            (None, None, Some(manager)) => AuthSource::AzureEntra(Box::new(manager)),
            (None, None, None) => {
                if transport.auth_header.is_some() || transport.auth_value_prefix.is_some() {
                    return Err(
                        "provider transport declares auth header fields without an auth source"
                            .to_string(),
                    );
                }
                return Ok(None);
            }
            (Some(name), None, None) => AuthSource::Environment(name.to_string()),
            (None, Some(reference), None) => {
                let (scope, name) = parse_secret_ref(reference)?;
                if scope.name() != Some(descriptor.id.as_str()) {
                    return Err(format!(
                        "provider interface '{}' auth_secret_ref must use its exact interface scope",
                        descriptor.id
                    ));
                }
                let octopus_home = secret_home.ok_or_else(|| {
                    "provider auth_secret_ref requires activation_secrets_home root slot"
                        .to_string()
                })?;
                AuthSource::Secret {
                    octopus_home: octopus_home.to_path_buf(),
                    scope,
                    name,
                }
            }
            (_, _, Some(_)) => {
                return Err(
                    "provider Azure Entra auth cannot declare auth_env_key or auth_secret_ref"
                        .to_string(),
                );
            }
            (Some(_), Some(_), None) => {
                return Err(
                    "provider transport may declare only one of auth_env_key or auth_secret_ref"
                        .to_string(),
                );
            }
        }
    };
    let header = transport
        .auth_header
        .as_deref()
        .ok_or_else(|| "provider auth source requires explicit auth_header".to_string())?;
    let header = parse_header_name(header, "provider auth_header")?;
    let value_prefix = transport
        .auth_value_prefix
        .as_ref()
        .ok_or_else(|| "provider auth source requires explicit auth_value_prefix".to_string())?
        .clone();
    HeaderValue::from_str(&value_prefix)
        .map_err(|_| "provider auth_value_prefix is not a valid header fragment".to_string())?;
    Ok(Some(AuthConfig {
        header,
        value_prefix,
        source,
    }))
}

fn resolve_provider_model_selection(
    metadata: &Map<String, Value>,
) -> Result<ProviderModelSelection, String> {
    let literal = metadata
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let env_name = metadata
        .get("model_env")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match (literal, env_name) {
        (Some(model), None) => Ok(ProviderModelSelection::Literal(model.to_string())),
        (None, Some(name)) => Ok(ProviderModelSelection::Environment(name.to_string())),
        (Some(_), Some(_)) => {
            Err("provider interface declares both model and model_env".to_string())
        }
        (None, None) => Err(
            "provider interface must declare exactly one of private_config.model or model_env"
                .to_string(),
        ),
    }
}

pub(crate) fn parse_secret_ref(reference: &str) -> Result<(SecretScope, SecretName), String> {
    let path = reference
        .strip_prefix("secret://")
        .ok_or_else(|| "provider auth_secret_ref must start with secret://".to_string())?;
    let (scope, name) = path
        .split_once('/')
        .ok_or_else(|| "provider auth_secret_ref must be secret://<scope>/<name>".to_string())?;
    if name.contains('/') {
        return Err("provider auth_secret_ref must name one scoped secret".to_string());
    }
    Ok((
        SecretScope::named(scope.to_string()).map_err(|error| error.to_string())?,
        SecretName::new(name).map_err(|error| error.to_string())?,
    ))
}

fn validate_base_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|error| format!("provider base_url is invalid: {error}"))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("provider base_url cannot contain credentials, query or fragment".to_string());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "provider base_url has no host".to_string())?;
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    match url.scheme() {
        "https" => Ok(url),
        "http" if loopback => Ok(url),
        "http" => Err("provider base_url requires HTTPS outside loopback".to_string()),
        scheme => Err(format!(
            "provider base_url scheme '{scheme}' is unsupported"
        )),
    }
}

pub(crate) fn validate_image_url(raw: &str) -> Result<(), String> {
    let value = raw.trim();
    if value != raw || value.is_empty() {
        return Err("image URL must be non-empty and trimmed".to_string());
    }
    if let Some(encoded) = value.strip_prefix("data:image/") {
        let (metadata, data) = encoded
            .split_once(',')
            .ok_or_else(|| "image data URL has no payload separator".to_string())?;
        if !metadata.ends_with(";base64") {
            return Err("image data URL must carry base64 bytes".to_string());
        }
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|_| "image data URL has invalid base64 bytes".to_string())?;
        return Ok(());
    }
    let url = Url::parse(value).map_err(|error| format!("image URL is invalid: {error}"))?;
    if url.scheme() != "https" || url.host_str().is_none() {
        return Err("remote image URL must use HTTPS".to_string());
    }
    Ok(())
}

pub(crate) fn validate_audio_data(data: &str, format: &str) -> Result<(), String> {
    require_trimmed(format, "input_audio.format")?;
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| "input_audio.data has invalid base64 bytes".to_string())?;
    Ok(())
}

fn parse_header_name(value: &str, surface: &str) -> Result<HeaderName, String> {
    require_trimmed(value, surface)?;
    HeaderName::from_bytes(value.as_bytes())
        .map_err(|_| format!("{surface} '{value}' is not a valid HTTP header name"))
}

fn reject_secret_bearing_generic_header(name: &HeaderName, surface: &str) -> Result<(), String> {
    if matches!(
        name.as_str(),
        "authorization"
            | "proxy-authorization"
            | "x-api-key"
            | "api-key"
            | "x-auth-token"
            | "x-access-token"
            | "x-goog-api-key"
            | "x-amz-security-token"
            | "cookie"
            | "set-cookie"
    ) {
        return Err(format!(
            "{surface} '{}' is credential-bearing and must use the explicit auth source fields",
            name.as_str()
        ));
    }
    Ok(())
}

fn required_env(name: &str, surface: &str) -> Result<String, String> {
    require_trimmed(name, &format!("{surface} env name"))?;
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{surface} env var '{name}' is unset or empty"))
}

fn validate_field_names(values: &[String], surface: &str) -> Result<(), String> {
    let mut unique = BTreeSet::new();
    for value in values {
        require_trimmed(value, surface)?;
        if value.contains('.') || !unique.insert(value) {
            return Err(format!("openai_compatible.response.{surface} is invalid"));
        }
    }
    Ok(())
}

fn validate_finish_reasons(
    values: &BTreeSet<String>,
    surface: &str,
    empty_allowed: bool,
) -> Result<(), String> {
    if values.is_empty() && !empty_allowed {
        return Err(format!(
            "openai_compatible.response.{surface} must not be empty"
        ));
    }
    for value in values {
        require_trimmed(value, &format!("openai_compatible.response.{surface}"))?;
    }
    Ok(())
}

fn require_trimmed(value: &str, surface: &str) -> Result<(), String> {
    if value.is_empty() || value.trim() != value {
        return Err(format!("{surface} must be non-empty and trimmed"));
    }
    Ok(())
}

fn validate_json_field_name(value: &str, surface: &str) -> Result<(), String> {
    require_trimmed(value, surface)?;
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return Err(format!(
            "{surface} must contain only ASCII letters, digits, '_' or '-'"
        ));
    }
    Ok(())
}

fn validate_artifact_label(value: &str, surface: &str) -> Result<(), String> {
    require_trimmed(value, surface)?;
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.'))
    {
        return Err(format!(
            "{surface} must contain only ASCII letters, digits, '_', '-' or '.'"
        ));
    }
    Ok(())
}

fn validate_json_pointer(value: &str, surface: &str) -> Result<(), String> {
    if !value.is_empty() && !value.starts_with('/') {
        return Err(format!("{surface} must be empty or start with '/'"));
    }
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'~' {
            let escaped = bytes.get(index + 1).copied();
            if !matches!(escaped, Some(b'0' | b'1')) {
                return Err(format!("{surface} contains an invalid '~' escape"));
            }
            index += 2;
        } else {
            index += 1;
        }
    }
    Ok(())
}

fn default_models_pointer() -> String {
    "/data".to_string()
}

fn default_model_id_pointer() -> String {
    "/id".to_string()
}

fn validate_request_body_key(value: &str, surface: &str) -> Result<(), String> {
    validate_json_field_name(value, surface)?;
    if matches!(value, "model" | "messages" | "tools" | "stream") {
        return Err(format!(
            "{surface} may not target family-owned request key '{value}'"
        ));
    }
    Ok(())
}
