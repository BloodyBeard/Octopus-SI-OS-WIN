use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use octopus_github_copilot_auth::{
    redact_detail, AuthFailure, GitHubAuthDispatchAdmission, GitHubCredentialManager,
};
use octopus_interfaces::{InterfaceArtifactFile, InterfaceDescriptor, InterfaceInvocationEnvelope};
use octopus_model_context_render::{
    model_hidden_tool_names, model_tool_exclusions, render_model_context_block_with_hidden_tools,
};
use octopus_provider_run_authoring::{
    ProviderEmission, ProviderEmissionRoute, ProviderHandoffOutcome, ProviderOutputHandoff,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

pub const INTERFACE_DRIVER_REF: &str = "github_copilot_acp";

const SESSION_CONTEXT_FIELD: &str = "session_context";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcpConfig {
    command: String,
    args: Vec<String>,
    model: String,
    protocol_version: u64,
    client_info: ClientInfo,
    request_timeout_ms: u64,
    max_control_frame_bytes: usize,
    max_stderr_bytes: usize,
    max_inline_data_bytes: usize,
    retry_backoff_ms: Vec<u64>,
    allowed_input_labels: Vec<String>,
    session_replay_labels: Vec<String>,
    allowed_session_updates: Vec<String>,
    successful_stop_reasons: Vec<String>,
    nonreplayable_stop_reasons: Vec<String>,
    process_env_names: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientInfo {
    name: String,
    title: String,
    version: String,
}

#[derive(serde::Serialize)]
#[serde(deny_unknown_fields)]
struct AcpTransportDispatch<'a> {
    label: &'static str,
    provider_interface: &'a str,
    source_movement_seq: u64,
    attempt: u32,
    method: &'a str,
    transport: &'static str,
    endpoint: AcpTransportEndpoint,
    request: AcpTransportRequest,
    credential_handle_fingerprint: &'a str,
}

#[derive(serde::Serialize)]
#[serde(deny_unknown_fields)]
struct AcpTransportEndpoint {
    scheme: &'static str,
    host: &'static str,
    port: Option<u16>,
    path: &'static str,
}

#[derive(serde::Serialize)]
#[serde(deny_unknown_fields)]
struct AcpTransportRequest {
    bytes: u64,
    blake3: String,
}

struct AcpDispatchAdmission<'a> {
    handoff: &'a ProviderOutputHandoff,
    output_root: &'a Path,
    provider_interface: &'a str,
    source_movement_seq: u64,
    attempt: u32,
    credential_handle_fingerprint: &'a str,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionContext {
    schema_version: u32,
    provider_interface: String,
    artifacts: Vec<SessionContextArtifact>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionContextArtifact {
    label: String,
    payload: Value,
    source_artifact: octopus_interfaces::ArtifactRef,
    source_movement_seq: Option<u64>,
}

struct ContextEntry {
    label: String,
    payload: Value,
    current: bool,
}

#[derive(Default)]
struct AcpTurn {
    text: String,
    reasoning: String,
    tool_events: Vec<Value>,
    permission_requests: Vec<Value>,
    usage: Option<Value>,
    update_counts: Map<String, Value>,
    stop_reason: String,
}

pub struct GitHubCopilotAcpDriver {
    interface_id: String,
    config: AcpConfig,
    auth: GitHubCredentialManager,
    private_config: Map<String, Value>,
    allowed_input_labels: BTreeSet<String>,
    session_replay_labels: BTreeSet<String>,
    allowed_session_updates: BTreeSet<String>,
    successful_stop_reasons: BTreeSet<String>,
    nonreplayable_stop_reasons: BTreeSet<String>,
    private_home: PathBuf,
    working_directory: PathBuf,
}

#[derive(Clone, Debug)]
pub struct GitHubCopilotAcpFailure {
    pub class: &'static str,
    pub retryable: bool,
    pub code: String,
    pub detail: String,
    pub evidence: Option<Value>,
}

impl GitHubCopilotAcpFailure {
    pub fn protocol(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            class: "protocol",
            retryable: false,
            code: code.into(),
            detail: detail.into(),
            evidence: None,
        }
    }

    pub fn unavailable(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            class: "unavailable",
            retryable: false,
            code: code.into(),
            detail: detail.into(),
            evidence: None,
        }
    }

    pub fn interrupted(detail: impl Into<String>) -> Self {
        Self::unavailable("acp_turn_interrupted", detail)
    }

    pub fn exhausted(detail: impl Into<String>, evidence: Option<Value>) -> Self {
        Self {
            class: "transport",
            retryable: false,
            code: "retry_exhausted".to_string(),
            detail: detail.into(),
            evidence,
        }
    }

    fn transport(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            class: "transport",
            retryable: true,
            code: code.into(),
            detail: detail.into(),
            evidence: None,
        }
    }

    fn after_prompt_dispatch(mut self) -> Self {
        if self.retryable {
            let cause = self.code;
            self.class = "interrupted";
            self.retryable = false;
            self.code = "acp_turn_interrupted_after_dispatch".to_string();
            self.detail = format!(
                "GitHub Copilot ACP failed after session/prompt dispatch; the external turn was not replayed ({cause}): {}",
                self.detail
            );
        }
        self
    }
}

impl AcpDispatchAdmission<'_> {
    async fn admit(
        &self,
        method: &str,
        request_bytes: &[u8],
    ) -> Result<(), GitHubCopilotAcpFailure> {
        if self.attempt == 0 {
            return Err(GitHubCopilotAcpFailure::protocol(
                "acp_dispatch_attempt_invalid",
                "GitHub Copilot ACP transport dispatch attempt must be positive",
            ));
        }
        let payload = serde_json::to_vec(&AcpTransportDispatch {
            label: "provider.transport.dispatch",
            provider_interface: self.provider_interface,
            source_movement_seq: self.source_movement_seq,
            attempt: self.attempt,
            method,
            transport: "process",
            endpoint: AcpTransportEndpoint {
                scheme: "process",
                host: "local",
                port: None,
                path: "/session/prompt",
            },
            request: AcpTransportRequest {
                bytes: u64::try_from(request_bytes.len()).map_err(|_| {
                    GitHubCopilotAcpFailure::protocol(
                        "acp_dispatch_size_invalid",
                        "GitHub Copilot ACP request byte count exceeds u64",
                    )
                })?,
                blake3: blake3::hash(request_bytes).to_hex().to_string(),
            },
            credential_handle_fingerprint: self.credential_handle_fingerprint,
        })
        .map_err(|error| {
            GitHubCopilotAcpFailure::protocol(
                "acp_dispatch_encode_failed",
                format!("GitHub Copilot ACP transport dispatch could not be encoded: {error}"),
            )
        })?;
        let artifact =
            InterfaceArtifactFile::write(self.output_root, "provider.transport.dispatch", &payload)
                .map_err(|error| {
                    GitHubCopilotAcpFailure::protocol(
                        "acp_dispatch_materialize_failed",
                        format!(
                    "GitHub Copilot ACP transport dispatch could not be materialized: {error}"
                ),
                    )
                })?;
        match self
            .handoff
            .hand_transport_dispatch(artifact)
            .await
            .map_err(|error| {
                GitHubCopilotAcpFailure::protocol(
                    "acp_dispatch_admission_failed",
                    format!("GitHub Copilot ACP transport dispatch was not admitted: {error}"),
                )
        })? {
            ProviderHandoffOutcome::Admitted { .. } => Ok(()),
            ProviderHandoffOutcome::TerminalFailure {
                failure_seq,
                failure_class,
                ..
            } => Err(GitHubCopilotAcpFailure::protocol(
                "acp_dispatch_authoring_failed",
                format!(
                    "GitHub Copilot ACP transport dispatch stopped by terminal run-authoring failure '{failure_class}' at movement {failure_seq}"
                ),
            )),
        }
    }
}

impl From<AuthFailure> for GitHubCopilotAcpFailure {
    fn from(failure: AuthFailure) -> Self {
        Self {
            class: failure.class,
            retryable: failure.retryable,
            code: failure.code.to_string(),
            detail: failure.detail,
            evidence: failure.evidence,
        }
    }
}

impl GitHubCopilotAcpDriver {
    pub fn from_descriptor(
        descriptor: &InterfaceDescriptor,
        activation_secrets_home: PathBuf,
        interface_var_dir: PathBuf,
    ) -> Result<Self, String> {
        if descriptor.interface_driver_ref != INTERFACE_DRIVER_REF {
            return Err(format!(
                "GitHub Copilot ACP interface '{}' selects driver '{}', expected '{INTERFACE_DRIVER_REF}'",
                descriptor.id, descriptor.interface_driver_ref
            ));
        }
        let driver_config = descriptor.interface_driver_config.as_ref().ok_or_else(|| {
            format!(
                "GitHub Copilot ACP interface '{}' has no driver config",
                descriptor.id
            )
        })?;
        let config: AcpConfig = serde_json::from_value(
            driver_config
                .private_config
                .get("github_copilot_acp")
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "GitHub Copilot ACP interface '{}' has no private_config.github_copilot_acp",
                        descriptor.id
                    )
                })?,
        )
        .map_err(|error| format!("GitHub Copilot ACP config is malformed: {error}"))?;
        validate_config(&config)?;
        let declared_model = driver_config
            .private_config
            .get("model")
            .and_then(Value::as_str)
            .ok_or_else(|| "GitHub Copilot ACP private_config.model is missing".to_string())?;
        if config.model != declared_model {
            return Err(
                "GitHub Copilot ACP model must match private_config.model exactly".to_string(),
            );
        }
        let auth = GitHubCredentialManager::from_descriptor(descriptor, &activation_secrets_home)
            .map_err(|error| error.to_string())?;
        let private_home = interface_var_dir.join("private-home");
        let working_directory = interface_var_dir.join("workspace");
        Ok(Self {
            interface_id: descriptor.id.as_str().to_string(),
            allowed_input_labels: strict_set(
                &config.allowed_input_labels,
                "github_copilot_acp.allowed_input_labels",
            )?,
            session_replay_labels: strict_set(
                &config.session_replay_labels,
                "github_copilot_acp.session_replay_labels",
            )?,
            allowed_session_updates: strict_set(
                &config.allowed_session_updates,
                "github_copilot_acp.allowed_session_updates",
            )?,
            successful_stop_reasons: strict_set(
                &config.successful_stop_reasons,
                "github_copilot_acp.successful_stop_reasons",
            )?,
            nonreplayable_stop_reasons: strict_set(
                &config.nonreplayable_stop_reasons,
                "github_copilot_acp.nonreplayable_stop_reasons",
            )?,
            private_config: driver_config.private_config.clone(),
            config,
            auth,
            private_home,
            working_directory,
        })
    }

    pub fn retry_backoff(&self, failed_attempt: u32) -> Result<Duration, String> {
        let index = failed_attempt
            .checked_sub(1)
            .ok_or_else(|| "GitHub Copilot ACP retry attempt must be positive".to_string())?
            as usize;
        self.config
            .retry_backoff_ms
            .get(index)
            .copied()
            .map(Duration::from_millis)
            .ok_or_else(|| {
                format!(
                    "GitHub Copilot ACP descriptor has no retry backoff for failed attempt {failed_attempt}"
                )
            })
    }

    pub fn validate_retry_budget(&self, max_attempts: u32) -> Result<(), String> {
        let available = self.config.retry_backoff_ms.len().saturating_add(1);
        if usize::try_from(max_attempts)
            .map(|value| value > available)
            .unwrap_or(true)
        {
            return Err(format!(
                "formed max_attempts {max_attempts} exceeds GitHub Copilot ACP retry capacity {available}"
            ));
        }
        Ok(())
    }

    pub fn enforce_formed_model_policy(
        &self,
        input: &InterfaceInvocationEnvelope,
    ) -> Result<(), GitHubCopilotAcpFailure> {
        let manifest = input.read_artifact_manifest().map_err(|error| {
            GitHubCopilotAcpFailure::protocol("artifact_manifest_invalid", error.to_string())
        })?;
        if GitHubCredentialManager::handles_label(&manifest.label) {
            return Ok(());
        }
        let models = required_formed_models(input)?;
        if !models.contains(&self.config.model) {
            return Err(GitHubCopilotAcpFailure::unavailable(
                "acp_model_not_granted",
                format!(
                    "GitHub Copilot ACP selected model '{}' is outside formed interface policy models",
                    self.config.model
                ),
            ));
        }
        Ok(())
    }

    pub async fn receive_artifact_and_materialize_outputs(
        &self,
        input: InterfaceInvocationEnvelope,
        source_movement_seq: u64,
        attempt: u32,
        output_root: &Path,
        handoff: &ProviderOutputHandoff,
    ) -> Result<Vec<ProviderEmission>, GitHubCopilotAcpFailure> {
        let manifest = input.read_artifact_manifest().map_err(|error| {
            GitHubCopilotAcpFailure::protocol("artifact_manifest_invalid", error.to_string())
        })?;
        if GitHubCredentialManager::handles_label(&manifest.label) {
            let payload = input.read_artifact_payload().map_err(|error| {
                GitHubCopilotAcpFailure::protocol("auth_artifact_unreadable", error.to_string())
            })?;
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
            return Ok(vec![materialize(
                output_root,
                outcome.label,
                outcome.payload,
                ProviderEmissionRoute::ClientProjection,
            )?]);
        }
        self.enforce_formed_model_policy(&input)?;
        if !self.allowed_input_labels.contains(&manifest.label) {
            return Err(GitHubCopilotAcpFailure::protocol(
                "artifact_label_unsupported",
                format!(
                    "GitHub Copilot ACP does not accept artifact label '{}'",
                    manifest.label
                ),
            ));
        }
        let selected = handoff.callable_tool_names(&input).map_err(|error| {
            GitHubCopilotAcpFailure::protocol("callable_tool_contract_invalid", error)
        })?;
        let hidden = model_tool_exclusions(
            input.metadata().map_err(|error| {
                GitHubCopilotAcpFailure::protocol("invocation_metadata_invalid", error.to_string())
            })?,
            &selected,
            &model_hidden_tool_names(&self.private_config).map_err(|error| {
                GitHubCopilotAcpFailure::protocol("hidden_tool_config_invalid", error)
            })?,
        );
        let prompt = self.assemble_prompt(&input, &hidden)?;
        let token = self.auth.raw_github_token()?;
        let credential_handle_fingerprint = self.auth.raw_token_handle_fingerprint();
        let turn = self
            .run_acp(
                prompt,
                token,
                source_movement_seq,
                attempt,
                AcpDispatchAdmission {
                    handoff,
                    output_root,
                    provider_interface: &self.interface_id,
                    source_movement_seq,
                    attempt,
                    credential_handle_fingerprint: &credential_handle_fingerprint,
                },
            )
            .await?;
        self.materialize_turn(output_root, turn)
    }

    fn assemble_prompt(
        &self,
        input: &InterfaceInvocationEnvelope,
        hidden: &[String],
    ) -> Result<Vec<Value>, GitHubCopilotAcpFailure> {
        let mut entries = Vec::new();
        let mut sources = BTreeSet::new();
        for bound in &input.artifact_binding_payloads {
            let bytes = bound.read_bytes().map_err(|error| {
                GitHubCopilotAcpFailure::protocol(
                    "bound_artifact_unreadable",
                    format!(
                        "bound artifact '{}' cannot be read: {error}",
                        bound.artifact.id
                    ),
                )
            })?;
            let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
                GitHubCopilotAcpFailure::protocol(
                    "bound_artifact_json_invalid",
                    format!(
                        "bound artifact '{}' is not JSON: {error}",
                        bound.artifact.id
                    ),
                )
            })?;
            self.collect_value_entries(
                &mut entries,
                &mut sources,
                value,
                bound.artifact.id.as_str(),
                &bound.artifact,
                false,
            )?;
        }
        let bytes = input.read_artifact_payload().map_err(|error| {
            GitHubCopilotAcpFailure::protocol("input_artifact_unreadable", error.to_string())
        })?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            GitHubCopilotAcpFailure::protocol(
                "input_artifact_json_invalid",
                format!(
                    "current artifact '{}' is not JSON: {error}",
                    input.artifact.id
                ),
            )
        })?;
        self.collect_value_entries(
            &mut entries,
            &mut sources,
            value,
            input.artifact.id.as_str(),
            &input.artifact,
            true,
        )?;
        if entries.iter().filter(|entry| entry.current).count() != 1 {
            return Err(GitHubCopilotAcpFailure::protocol(
                "current_artifact_ambiguous",
                "GitHub Copilot ACP invocation must carry exactly one current artifact",
            ));
        }
        let current = entries.iter().find(|entry| entry.current).ok_or_else(|| {
            GitHubCopilotAcpFailure::protocol(
                "current_artifact_missing",
                "GitHub Copilot ACP current artifact is missing",
            )
        })?;
        if current.label != "interface.input" {
            return Err(GitHubCopilotAcpFailure::protocol(
                "current_artifact_label_invalid",
                format!(
                    "GitHub Copilot ACP current model turn must be interface.input, got '{}'",
                    current.label
                ),
            ));
        }
        let mut instructions = Vec::new();
        if let Some(metadata) = input.metadata.as_ref() {
            if let Some(rendered) = render_model_context_block_with_hidden_tools(metadata, hidden)
            {
                instructions.push(rendered);
            }
        }
        let mut transcript = Vec::new();
        for entry in entries.iter().filter(|entry| !entry.current) {
            match entry.label.as_str() {
                "context.reference" => {
                    let text = required_text(&entry.payload, "text", "context.reference")?;
                    instructions.push(text);
                }
                "interface.input" => transcript.push(format!(
                    "User:\n{}",
                    required_text(&entry.payload, "prompt", "interface.input")?
                )),
                "interface.artifact.output" => transcript.push(format!(
                    "Assistant:\n{}",
                    required_text(&entry.payload, "text", "interface.artifact.output")?
                )),
                "turn.error" => transcript.push(format!(
                    "Assistant error:\n{}",
                    serde_json::to_string(&entry.payload).map_err(|error| {
                        GitHubCopilotAcpFailure::protocol(
                            "history_render_failed",
                            format!("GitHub Copilot ACP history could not be rendered: {error}"),
                        )
                    })?
                )),
                other if self.session_replay_labels.contains(other) => {
                    return Err(GitHubCopilotAcpFailure::protocol(
                        "session_replay_label_unimplemented",
                        format!("GitHub Copilot ACP replay label '{other}' has no decoder"),
                    ));
                }
                other => {
                    return Err(GitHubCopilotAcpFailure::protocol(
                        "bound_artifact_label_unsupported",
                        format!("GitHub Copilot ACP cannot replay bound label '{other}'"),
                    ));
                }
            }
        }
        let prompt = required_text(&current.payload, "prompt", "interface.input")?;
        let mut text = String::new();
        if !instructions.is_empty() {
            text.push_str(&instructions.join("\n\n"));
            text.push_str("\n\n");
        }
        if !transcript.is_empty() {
            text.push_str("Prior artifact history:\n\n");
            text.push_str(&transcript.join("\n\n"));
            text.push_str("\n\nCurrent user artifact:\n");
        }
        text.push_str(&prompt);
        let mut content = vec![json!({"type": "text", "text": text})];
        if let Some(parts) = current.payload.get("content") {
            let parts = parts.as_array().ok_or_else(|| {
                GitHubCopilotAcpFailure::protocol(
                    "input_content_invalid",
                    "GitHub Copilot ACP interface.input.content must be an array",
                )
            })?;
            for (index, part) in parts.iter().enumerate() {
                content.push(self.decode_content_part(part, index)?);
            }
        }
        Ok(content)
    }

    fn collect_value_entries(
        &self,
        entries: &mut Vec<ContextEntry>,
        sources: &mut BTreeSet<String>,
        mut value: Value,
        artifact_id: &str,
        artifact_ref: &octopus_interfaces::ArtifactRef,
        current: bool,
    ) -> Result<(), GitHubCopilotAcpFailure> {
        if let Some(snapshot_value) = value.get(SESSION_CONTEXT_FIELD).cloned() {
            let snapshot: SessionContext =
                serde_json::from_value(snapshot_value).map_err(|error| {
                    GitHubCopilotAcpFailure::protocol(
                        "session_context_invalid",
                        format!("artifact '{artifact_id}' session_context is invalid: {error}"),
                    )
                })?;
            if snapshot.schema_version != 2 || snapshot.provider_interface != self.interface_id {
                return Err(GitHubCopilotAcpFailure::protocol(
                    "session_context_scope_invalid",
                    format!(
                        "artifact '{artifact_id}' session_context does not belong to exact provider '{}'",
                        self.interface_id
                    ),
                ));
            }
            for (index, item) in snapshot.artifacts.into_iter().enumerate() {
                if !self.session_replay_labels.contains(&item.label) {
                    return Err(GitHubCopilotAcpFailure::protocol(
                        "session_context_label_invalid",
                        format!(
                            "artifact '{artifact_id}' session_context[{index}] label '{}' is not descriptor-authorized",
                            item.label
                        ),
                    ));
                }
                if item.source_movement_seq.is_none() {
                    return Err(GitHubCopilotAcpFailure::protocol(
                        "session_context_movement_missing",
                        format!(
                            "artifact '{artifact_id}' session_context[{index}] has no movement provenance"
                        ),
                    ));
                }
                if item.payload.get(SESSION_CONTEXT_FIELD).is_some() {
                    return Err(GitHubCopilotAcpFailure::protocol(
                        "session_context_nested",
                        format!(
                            "artifact '{artifact_id}' session_context[{index}] nests session_context"
                        ),
                    ));
                }
                let source_key = format!(
                    "{}:{}:{}",
                    item.source_artifact.id,
                    item.source_artifact.content.to_hex(),
                    item.label
                );
                if !sources.insert(source_key.clone()) {
                    return Err(GitHubCopilotAcpFailure::protocol(
                        "session_context_source_duplicate",
                        format!("artifact '{artifact_id}' repeats session source at index {index}"),
                    ));
                }
                entries.push(ContextEntry {
                    label: item.label,
                    payload: item.payload,
                    current: false,
                });
            }
            value
                .as_object_mut()
                .ok_or_else(|| {
                    GitHubCopilotAcpFailure::protocol(
                        "artifact_payload_invalid",
                        format!("artifact '{artifact_id}' payload is not an object"),
                    )
                })?
                .remove(SESSION_CONTEXT_FIELD);
        }
        let label = value
            .get("label")
            .and_then(Value::as_str)
            .filter(|label| !label.is_empty() && label.trim() == *label)
            .map(str::to_string)
            .or_else(|| {
                artifact_id
                    .split_once(':')
                    .map(|(label, _)| label.to_string())
            })
            .ok_or_else(|| {
                GitHubCopilotAcpFailure::protocol(
                    "artifact_label_missing",
                    format!("artifact '{artifact_id}' has no self-identifying label"),
                )
            })?;
        if !self.allowed_input_labels.contains(&label)
            && !self.session_replay_labels.contains(&label)
        {
            return Err(GitHubCopilotAcpFailure::protocol(
                "artifact_label_unsupported",
                format!("GitHub Copilot ACP does not accept artifact label '{label}'"),
            ));
        }
        let source_key = format!(
            "{}:{}:{}",
            artifact_ref.id,
            artifact_ref.content.to_hex(),
            label
        );
        if !sources.insert(source_key.clone()) {
            return Err(GitHubCopilotAcpFailure::protocol(
                "artifact_source_duplicate",
                format!("GitHub Copilot ACP repeats source artifact '{artifact_id}'"),
            ));
        }
        entries.push(ContextEntry {
            label,
            payload: value,
            current,
        });
        Ok(())
    }

    fn decode_content_part(
        &self,
        part: &Value,
        index: usize,
    ) -> Result<Value, GitHubCopilotAcpFailure> {
        let object = part.as_object().ok_or_else(|| {
            GitHubCopilotAcpFailure::protocol(
                "input_content_part_invalid",
                format!("GitHub Copilot ACP content[{index}] is not an object"),
            )
        })?;
        let part_type = object.get("type").and_then(Value::as_str).ok_or_else(|| {
            GitHubCopilotAcpFailure::protocol(
                "input_content_type_missing",
                format!("GitHub Copilot ACP content[{index}] has no type"),
            )
        })?;
        match part_type {
            "text" => Ok(json!({
                "type": "text",
                "text": required_text(part, "text", &format!("content[{index}]"))?
            })),
            "image" | "audio" => {
                let data = required_text(part, "data", &format!("content[{index}]"))?;
                if data.len() > self.config.max_inline_data_bytes {
                    return Err(GitHubCopilotAcpFailure::protocol(
                        "input_content_too_large",
                        format!(
                            "GitHub Copilot ACP content[{index}] exceeds descriptor inline-data limit"
                        ),
                    ));
                }
                let mime_type = required_text(part, "mimeType", &format!("content[{index}]"))?;
                let expected_prefix = if part_type == "image" {
                    "image/"
                } else {
                    "audio/"
                };
                if !mime_type.starts_with(expected_prefix) {
                    return Err(GitHubCopilotAcpFailure::protocol(
                        "input_content_mime_invalid",
                        format!(
                            "GitHub Copilot ACP content[{index}] mimeType does not match {part_type}"
                        ),
                    ));
                }
                Ok(json!({"type": part_type, "data": data, "mimeType": mime_type}))
            }
            other => Err(GitHubCopilotAcpFailure::protocol(
                "input_content_type_unsupported",
                format!("GitHub Copilot ACP content[{index}] type '{other}' is unsupported"),
            )),
        }
    }

    async fn run_acp(
        &self,
        prompt: Vec<Value>,
        token: String,
        source_movement_seq: u64,
        attempt: u32,
        dispatch: AcpDispatchAdmission<'_>,
    ) -> Result<AcpTurn, GitHubCopilotAcpFailure> {
        let attempt_directory = format!("{source_movement_seq}-{attempt}");
        let turn_home = self.private_home.join(&attempt_directory);
        let turn_working_directory = self.working_directory.join(&attempt_directory);
        std::fs::create_dir_all(&self.private_home).map_err(|error| {
            GitHubCopilotAcpFailure::unavailable(
                "acp_private_home_root_create_failed",
                format!("GitHub Copilot ACP private home root could not be created: {error}"),
            )
        })?;
        std::fs::create_dir_all(&self.working_directory).map_err(|error| {
            GitHubCopilotAcpFailure::unavailable(
                "acp_working_directory_root_create_failed",
                format!("GitHub Copilot ACP working directory root could not be created: {error}"),
            )
        })?;
        std::fs::create_dir(&turn_home).map_err(|error| {
            GitHubCopilotAcpFailure::unavailable(
                "acp_private_home_create_failed",
                format!("GitHub Copilot ACP exact turn private home could not be created: {error}"),
            )
        })?;
        if let Err(error) = std::fs::create_dir(&turn_working_directory) {
            return match std::fs::remove_dir(&turn_home) {
                Ok(()) => Err(GitHubCopilotAcpFailure::unavailable(
                    "acp_working_directory_create_failed",
                    format!(
                        "GitHub Copilot ACP exact turn working directory could not be created: {error}"
                    ),
                )),
                Err(cleanup_error) => Err(GitHubCopilotAcpFailure::unavailable(
                    "acp_turn_directory_create_cleanup_failed",
                    format!(
                        "GitHub Copilot ACP exact turn working directory creation failed ({error}) and private home cleanup failed ({cleanup_error})"
                    ),
                )),
            };
        }
        let path = std::env::var("PATH").map_err(|_| {
            GitHubCopilotAcpFailure::unavailable(
                "acp_path_missing",
                "GitHub Copilot ACP requires descriptor-declared PATH inheritance",
            )
        })?;
        let mut command = Command::new(&self.config.command);
        command
            .args(&self.config.args)
            .current_dir(&turn_working_directory)
            .env_clear()
            .env("PATH", path)
            .env("HOME", &turn_home)
            .env("COPILOT_HOME", &turn_home)
            .env("COPILOT_GITHUB_TOKEN", token)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| {
            GitHubCopilotAcpFailure::transport(
                "acp_spawn_failed",
                format!(
                    "GitHub Copilot ACP command '{}' could not be started: {error}",
                    self.config.command
                ),
            )
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            GitHubCopilotAcpFailure::transport(
                "acp_stdin_missing",
                "GitHub Copilot ACP child exposed no stdin",
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            GitHubCopilotAcpFailure::transport(
                "acp_stdout_missing",
                "GitHub Copilot ACP child exposed no stdout",
            )
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            GitHubCopilotAcpFailure::transport(
                "acp_stderr_missing",
                "GitHub Copilot ACP child exposed no stderr",
            )
        })?;
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        let stderr_task =
            spawn_stderr_reader(stderr, stderr_tail.clone(), self.config.max_stderr_bytes);
        let mut connection = AcpConnection {
            child,
            stdin,
            stdout: BufReader::new(stdout).lines(),
            stderr_tail,
            next_id: 0,
            timeout: Duration::from_millis(self.config.request_timeout_ms),
            max_control_frame_bytes: self.config.max_control_frame_bytes,
            allowed_session_updates: &self.allowed_session_updates,
            turn: AcpTurn::default(),
        };
        let outcome = async {
            let initialize = connection
                .request(
                    "initialize",
                    json!({
                        "protocolVersion": self.config.protocol_version,
                        "clientCapabilities": {},
                        "clientInfo": {
                            "name": self.config.client_info.name,
                            "title": self.config.client_info.title,
                            "version": self.config.client_info.version
                        }
                    }),
                )
                .await?;
            validate_prompt_capabilities(&initialize, &prompt)?;
            let session = connection
                .request(
                    "session/new",
                    json!({
                        "cwd": turn_working_directory,
                        "mcpServers": []
                    }),
                )
                .await?;
            let session_id = session
                .get("sessionId")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty() && value.trim() == *value)
                .ok_or_else(|| {
                    GitHubCopilotAcpFailure::protocol(
                        "acp_session_id_missing",
                        "GitHub Copilot ACP session/new returned no sessionId",
                    )
                })?;
            let result = connection
                .request_with_dispatch(
                    "session/prompt",
                    json!({"sessionId": session_id, "prompt": prompt}),
                    &dispatch,
                )
                .await
                .map_err(GitHubCopilotAcpFailure::after_prompt_dispatch)?;
            connection.turn.stop_reason = result
                .get("stopReason")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty() && value.trim() == *value)
                .ok_or_else(|| {
                    GitHubCopilotAcpFailure::protocol(
                        "acp_stop_reason_missing",
                        "GitHub Copilot ACP session/prompt returned no stopReason",
                    )
                })?
                .to_string();
            if let Some(usage) = result.get("usage") {
                if !usage.is_null() {
                    if !usage.is_object() {
                        return Err(GitHubCopilotAcpFailure::protocol(
                            "acp_usage_invalid",
                            "GitHub Copilot ACP session/prompt usage is not an object",
                        ));
                    }
                    connection.turn.usage = Some(usage.clone());
                }
            }
            let nonreplayable_terminal = self
                .nonreplayable_stop_reasons
                .contains(&connection.turn.stop_reason);
            if nonreplayable_terminal && connection.turn.text.trim().is_empty() {
                return Err(GitHubCopilotAcpFailure::interrupted(format!(
                    "GitHub Copilot ACP ended after session/prompt dispatch with non-replayable stop reason '{}' and no answer artifact",
                    connection.turn.stop_reason
                )));
            }
            if !self
                .successful_stop_reasons
                .contains(&connection.turn.stop_reason)
                && !nonreplayable_terminal
            {
                return Err(GitHubCopilotAcpFailure::protocol(
                    "acp_stop_reason_unsupported",
                    format!(
                        "GitHub Copilot ACP ended with unsupported stop reason '{}'",
                        connection.turn.stop_reason
                    ),
                ));
            }
            if connection.turn.text.trim().is_empty() {
                return Err(GitHubCopilotAcpFailure::protocol(
                    "acp_answer_missing",
                    "GitHub Copilot ACP completed without an answer artifact",
                ));
            }
            Ok(std::mem::take(&mut connection.turn))
        }
        .await;
        let _ = connection.stdin.shutdown().await;
        let _ = connection.child.kill().await;
        let _ = connection.child.wait().await;
        stderr_task.abort();
        let cleanup_home = std::fs::remove_dir_all(&turn_home);
        let cleanup_workspace = std::fs::remove_dir_all(&turn_working_directory);
        if let Err(error) = cleanup_home {
            return Err(GitHubCopilotAcpFailure::unavailable(
                "acp_private_home_cleanup_failed",
                format!("GitHub Copilot ACP private home cleanup failed: {error}"),
            ));
        }
        if let Err(error) = cleanup_workspace {
            return Err(GitHubCopilotAcpFailure::unavailable(
                "acp_working_directory_cleanup_failed",
                format!("GitHub Copilot ACP working directory cleanup failed: {error}"),
            ));
        }
        match outcome {
            Ok(turn) => Ok(turn),
            Err(mut failure) => {
                let stderr = bounded_stderr(&connection.stderr_tail).await;
                if !stderr.is_empty() {
                    failure.evidence = Some(json!({
                        "stderr": stderr,
                        "command": self.config.command,
                        "args": self.config.args
                    }));
                }
                Err(failure)
            }
        }
    }

    fn materialize_turn(
        &self,
        output_root: &Path,
        turn: AcpTurn,
    ) -> Result<Vec<ProviderEmission>, GitHubCopilotAcpFailure> {
        let complete = self.successful_stop_reasons.contains(&turn.stop_reason);
        let nonreplayable_terminal = self.nonreplayable_stop_reasons.contains(&turn.stop_reason);
        let mut outputs = Vec::new();
        if !turn.reasoning.trim().is_empty() {
            outputs.push(materialize(
                output_root,
                "provider.reasoning",
                json!({
                    "label": "provider.reasoning",
                    "text": turn.reasoning,
                    "wire": "acp_ndjson"
                }),
                ProviderEmissionRoute::ClientProjection,
            )?);
        }
        for event in turn.tool_events {
            outputs.push(materialize(
                output_root,
                "provider.native_tool",
                json!({
                    "label": "provider.native_tool",
                    "wire": "acp_ndjson",
                    "executed_by_octopus": false,
                    "event": event
                }),
                ProviderEmissionRoute::ClientProjection,
            )?);
        }
        for request in turn.permission_requests {
            outputs.push(materialize(
                output_root,
                "operator.approval.request",
                json!({
                    "label": "operator.approval.request",
                    "wire": "acp_ndjson",
                    "outcome": "cancelled",
                    "executed_by_octopus": false,
                    "request": request
                }),
                ProviderEmissionRoute::ClientProjection,
            )?);
        }
        if let Some(usage) = turn.usage {
            outputs.push(materialize(
                output_root,
                "provider.usage",
                json!({
                    "label": "provider.usage",
                    "wire": "acp_ndjson",
                    "usage": usage
                }),
                ProviderEmissionRoute::ClientProjection,
            )?);
        }
        outputs.push(materialize(
            output_root,
            "interface.artifact.output",
            json!({
                "label": "interface.artifact.output",
                "text": turn.text,
                "provider_evidence": {
                    "wire": "acp_ndjson",
                    "protocol_version": self.config.protocol_version,
                    "stop_reason": turn.stop_reason,
                    "complete": complete,
                    "nonreplayable_terminal": nonreplayable_terminal,
                    "update_counts": turn.update_counts,
                    "command": self.config.command,
                    "args": self.config.args,
                    "model": self.config.model
                }
            }),
            ProviderEmissionRoute::ClientProjection,
        )?);
        Ok(outputs)
    }
}

fn required_formed_models(
    input: &InterfaceInvocationEnvelope,
) -> Result<BTreeSet<String>, GitHubCopilotAcpFailure> {
    let policy = input
        .interface_policy()
        .map_err(|error| {
            GitHubCopilotAcpFailure::unavailable("acp_model_policy_invalid", error.to_string())
        })?
        .ok_or_else(|| {
            GitHubCopilotAcpFailure::unavailable(
                "acp_model_policy_missing",
                "GitHub Copilot ACP invocation carries no formed interface policy",
            )
        })?;
    let values = policy
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            GitHubCopilotAcpFailure::unavailable(
                "acp_model_policy_invalid",
                "GitHub Copilot ACP formed interface policy models must be a string array",
            )
        })?;
    if values.is_empty() {
        return Err(GitHubCopilotAcpFailure::unavailable(
            "acp_model_policy_empty",
            "GitHub Copilot ACP formed interface policy models is empty",
        ));
    }
    let mut models = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let model = value.as_str().filter(|model| {
            !model.is_empty() && model.trim() == *model && !model.contains(['\r', '\n', '\0'])
        });
        let Some(model) = model else {
            return Err(GitHubCopilotAcpFailure::unavailable(
                "acp_model_policy_invalid",
                format!(
                    "GitHub Copilot ACP formed interface policy models[{index}] is not a trimmed string"
                ),
            ));
        };
        if !models.insert(model.to_string()) {
            return Err(GitHubCopilotAcpFailure::unavailable(
                "acp_model_policy_invalid",
                format!("GitHub Copilot ACP formed interface policy models repeats '{model}'"),
            ));
        }
    }
    Ok(models)
}

struct AcpConnection<'a> {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    next_id: u64,
    timeout: Duration,
    max_control_frame_bytes: usize,
    allowed_session_updates: &'a BTreeSet<String>,
    turn: AcpTurn,
}

impl AcpConnection<'_> {
    async fn request(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<Value, GitHubCopilotAcpFailure> {
        let (request_id, bytes) = self.prepare_request(method, params)?;
        self.write_encoded_frame(&bytes).await?;
        self.read_response(request_id, method).await
    }

    async fn request_with_dispatch(
        &mut self,
        method: &str,
        params: Value,
        dispatch: &AcpDispatchAdmission<'_>,
    ) -> Result<Value, GitHubCopilotAcpFailure> {
        let (request_id, bytes) = self.prepare_request(method, params)?;
        dispatch.admit(method, &bytes).await?;
        self.write_encoded_frame(&bytes).await?;
        self.read_response(request_id, method).await
    }

    fn prepare_request(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<(u64, Vec<u8>), GitHubCopilotAcpFailure> {
        self.next_id = self.next_id.checked_add(1).ok_or_else(|| {
            GitHubCopilotAcpFailure::protocol(
                "acp_request_id_overflow",
                "GitHub Copilot ACP request id overflowed",
            )
        })?;
        let request_id = self.next_id;
        let bytes = self.encode_frame(json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params
        }))?;
        Ok((request_id, bytes))
    }

    async fn read_response(
        &mut self,
        request_id: u64,
        method: &str,
    ) -> Result<Value, GitHubCopilotAcpFailure> {
        let deadline = tokio::time::Instant::now() + self.timeout;
        loop {
            let line = tokio::time::timeout_at(deadline, self.stdout.next_line())
                .await
                .map_err(|_| {
                    GitHubCopilotAcpFailure::transport(
                        "acp_request_timeout",
                        format!("GitHub Copilot ACP timed out waiting for '{method}'"),
                    )
                })?
                .map_err(|error| {
                    GitHubCopilotAcpFailure::transport(
                        "acp_stdout_read_failed",
                        format!("GitHub Copilot ACP stdout could not be read: {error}"),
                    )
                })?
                .ok_or_else(|| {
                    GitHubCopilotAcpFailure::transport(
                        "acp_process_exited",
                        format!("GitHub Copilot ACP exited while waiting for '{method}'"),
                    )
                })?;
            if line.len() > self.max_control_frame_bytes {
                return Err(GitHubCopilotAcpFailure::protocol(
                    "acp_frame_too_large",
                    "GitHub Copilot ACP control frame exceeds descriptor limit",
                ));
            }
            let message: Value = serde_json::from_str(&line).map_err(|error| {
                GitHubCopilotAcpFailure::protocol(
                    "acp_frame_json_invalid",
                    format!("GitHub Copilot ACP emitted invalid JSON: {error}"),
                )
            })?;
            if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
                return Err(GitHubCopilotAcpFailure::protocol(
                    "acp_jsonrpc_version_invalid",
                    "GitHub Copilot ACP frame does not declare jsonrpc 2.0",
                ));
            }
            if message.get("method").is_some() {
                self.handle_server_message(message).await?;
                continue;
            }
            let id = message.get("id").and_then(Value::as_u64).ok_or_else(|| {
                GitHubCopilotAcpFailure::protocol(
                    "acp_response_id_missing",
                    "GitHub Copilot ACP response has no unsigned id",
                )
            })?;
            if id != request_id {
                return Err(GitHubCopilotAcpFailure::protocol(
                    "acp_response_id_mismatch",
                    format!(
                        "GitHub Copilot ACP returned response id {id} while waiting for {request_id}"
                    ),
                ));
            }
            let has_result = message.get("result").is_some();
            let has_error = message.get("error").is_some();
            if has_result == has_error {
                return Err(GitHubCopilotAcpFailure::protocol(
                    "acp_response_shape_invalid",
                    "GitHub Copilot ACP response must carry exactly one of result or error",
                ));
            }
            if let Some(error) = message.get("error") {
                return Err(GitHubCopilotAcpFailure::protocol(
                    "acp_jsonrpc_error",
                    redact_detail(format!(
                        "GitHub Copilot ACP method '{method}' failed: {}",
                        serde_json::to_string(error)
                            .unwrap_or_else(|_| "invalid error".to_string())
                    )),
                ));
            }
            return message.get("result").cloned().ok_or_else(|| {
                GitHubCopilotAcpFailure::protocol(
                    "acp_result_missing",
                    format!("GitHub Copilot ACP method '{method}' returned no result"),
                )
            });
        }
    }

    async fn handle_server_message(
        &mut self,
        message: Value,
    ) -> Result<(), GitHubCopilotAcpFailure> {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                GitHubCopilotAcpFailure::protocol(
                    "acp_server_method_invalid",
                    "GitHub Copilot ACP server message has no method",
                )
            })?;
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
        match method {
            "session/update" => self.handle_session_update(params),
            "session/request_permission" => {
                self.turn
                    .permission_requests
                    .push(redacted_json(params.clone()));
                let id = message.get("id").cloned().ok_or_else(|| {
                    GitHubCopilotAcpFailure::protocol(
                        "acp_permission_request_id_missing",
                        "GitHub Copilot ACP permission request has no id",
                    )
                })?;
                self.write_frame(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {"outcome": {"outcome": "cancelled"}}
                }))
                .await
            }
            "fs/read_text_file" | "fs/write_text_file" => {
                let id = message.get("id").cloned().ok_or_else(|| {
                    GitHubCopilotAcpFailure::protocol(
                        "acp_fs_request_id_missing",
                        "GitHub Copilot ACP filesystem request has no id",
                    )
                })?;
                self.write_frame(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": "ACP filesystem capability is not exposed by this Octopus interface"
                    }
                }))
                .await
            }
            other => {
                let Some(id) = message.get("id").cloned() else {
                    return Err(GitHubCopilotAcpFailure::protocol(
                        "acp_notification_unsupported",
                        format!("GitHub Copilot ACP notification '{other}' is unsupported"),
                    ));
                };
                self.write_frame(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": format!("ACP client method '{other}' is not supported")
                    }
                }))
                .await
            }
        }
    }

    fn handle_session_update(&mut self, params: Value) -> Result<(), GitHubCopilotAcpFailure> {
        let update = params
            .get("update")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                GitHubCopilotAcpFailure::protocol(
                    "acp_session_update_invalid",
                    "GitHub Copilot ACP session/update has no update object",
                )
            })?;
        let kind = update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.trim() == *value)
            .ok_or_else(|| {
                GitHubCopilotAcpFailure::protocol(
                    "acp_session_update_kind_missing",
                    "GitHub Copilot ACP session/update has no sessionUpdate kind",
                )
            })?;
        if !self.allowed_session_updates.contains(kind) {
            return Err(GitHubCopilotAcpFailure::protocol(
                "acp_session_update_unsupported",
                format!("GitHub Copilot ACP update kind '{kind}' is not descriptor-authorized"),
            ));
        }
        let count = self
            .turn
            .update_counts
            .get(kind)
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .saturating_add(1);
        self.turn
            .update_counts
            .insert(kind.to_string(), Value::from(count));
        match kind {
            "agent_message_chunk" => {
                self.turn.text.push_str(&content_text(update, kind)?);
            }
            "agent_thought_chunk" => {
                self.turn.reasoning.push_str(&content_text(update, kind)?);
            }
            "tool_call" | "tool_call_update" | "plan" => {
                self.turn
                    .tool_events
                    .push(redacted_json(Value::Object(update.clone())));
            }
            "usage_update" => {
                self.turn.usage = Some(Value::Object(update.clone()));
            }
            "available_commands_update"
            | "config_options_update"
            | "current_mode_update"
            | "session_info_update"
            | "user_message_chunk" => {}
            other => {
                return Err(GitHubCopilotAcpFailure::protocol(
                    "acp_session_update_decoder_missing",
                    format!("GitHub Copilot ACP update kind '{other}' has no decoder"),
                ));
            }
        }
        Ok(())
    }

    async fn write_frame(&mut self, value: Value) -> Result<(), GitHubCopilotAcpFailure> {
        let bytes = self.encode_frame(value)?;
        self.write_encoded_frame(&bytes).await
    }

    fn encode_frame(&self, value: Value) -> Result<Vec<u8>, GitHubCopilotAcpFailure> {
        let mut bytes = serde_json::to_vec(&value).map_err(|error| {
            GitHubCopilotAcpFailure::protocol(
                "acp_frame_encode_failed",
                format!("GitHub Copilot ACP frame could not be encoded: {error}"),
            )
        })?;
        if bytes.len() > self.max_control_frame_bytes {
            return Err(GitHubCopilotAcpFailure::protocol(
                "acp_frame_too_large",
                "GitHub Copilot ACP outbound frame exceeds descriptor limit",
            ));
        }
        bytes.push(b'\n');
        Ok(bytes)
    }

    async fn write_encoded_frame(&mut self, bytes: &[u8]) -> Result<(), GitHubCopilotAcpFailure> {
        self.stdin.write_all(bytes).await.map_err(|error| {
            GitHubCopilotAcpFailure::transport(
                "acp_stdin_write_failed",
                format!("GitHub Copilot ACP stdin could not be written: {error}"),
            )
        })?;
        self.stdin.flush().await.map_err(|error| {
            GitHubCopilotAcpFailure::transport(
                "acp_stdin_flush_failed",
                format!("GitHub Copilot ACP stdin could not be flushed: {error}"),
            )
        })
    }
}

fn validate_config(config: &AcpConfig) -> Result<(), String> {
    for (surface, value) in [
        ("command", config.command.as_str()),
        ("model", config.model.as_str()),
        ("client_info.name", config.client_info.name.as_str()),
        ("client_info.title", config.client_info.title.as_str()),
        ("client_info.version", config.client_info.version.as_str()),
    ] {
        if value.is_empty() || value.trim() != value || value.contains('\r') || value.contains('\n')
        {
            return Err(format!(
                "GitHub Copilot ACP {surface} must be non-empty, trimmed, and single-line"
            ));
        }
    }
    if config.command.contains('/') || config.command.contains('\\') {
        return Err("GitHub Copilot ACP command must be an executable command name".to_string());
    }
    let required_args = vec![
        "--acp".to_string(),
        "--stdio".to_string(),
        format!("--model={}", config.model),
        "--effort=high".to_string(),
        "--available-tools=view".to_string(),
        "--disable-builtin-mcps".to_string(),
        "--disallow-temp-dir".to_string(),
        "--no-ask-user".to_string(),
        "--no-auto-update".to_string(),
        "--no-custom-instructions".to_string(),
        "--no-experimental".to_string(),
        "--no-remote".to_string(),
        "--no-remote-export".to_string(),
        "--secret-env-vars=COPILOT_GITHUB_TOKEN".to_string(),
    ];
    if config.args != required_args {
        return Err(
            "GitHub Copilot ACP args must match the exact fail-closed stdio invocation".to_string(),
        );
    }
    if config.protocol_version != 1 {
        return Err("GitHub Copilot ACP protocol_version must be 1".to_string());
    }
    if config.request_timeout_ms == 0 || config.request_timeout_ms > 1_800_000 {
        return Err("GitHub Copilot ACP request_timeout_ms must be within 1..=1800000".to_string());
    }
    if config.max_control_frame_bytes == 0
        || config.max_stderr_bytes == 0
        || config.max_inline_data_bytes == 0
    {
        return Err("GitHub Copilot ACP byte limits must be positive".to_string());
    }
    if config.retry_backoff_ms.is_empty() || config.retry_backoff_ms.contains(&0) {
        return Err("GitHub Copilot ACP retry_backoff_ms must contain positive delays".to_string());
    }
    if config.process_env_names != ["PATH"] {
        return Err(
            "GitHub Copilot ACP process_env_names must explicitly contain only PATH".to_string(),
        );
    }
    Ok(())
}

fn validate_prompt_capabilities(
    initialize: &Value,
    prompt: &[Value],
) -> Result<(), GitHubCopilotAcpFailure> {
    let uses_image = prompt
        .iter()
        .any(|item| item.get("type").and_then(Value::as_str) == Some("image"));
    let uses_audio = prompt
        .iter()
        .any(|item| item.get("type").and_then(Value::as_str) == Some("audio"));
    let capabilities = initialize
        .get("agentCapabilities")
        .and_then(|value| value.get("promptCapabilities"));
    if uses_image
        && !capabilities
            .and_then(|value| value.get("image"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return Err(GitHubCopilotAcpFailure::protocol(
            "acp_image_capability_missing",
            "GitHub Copilot ACP did not advertise image prompt capability",
        ));
    }
    if uses_audio
        && !capabilities
            .and_then(|value| value.get("audio"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return Err(GitHubCopilotAcpFailure::protocol(
            "acp_audio_capability_missing",
            "GitHub Copilot ACP did not advertise audio prompt capability",
        ));
    }
    Ok(())
}

fn strict_set(values: &[String], surface: &str) -> Result<BTreeSet<String>, String> {
    let mut set = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        if value.is_empty() || value.trim() != value {
            return Err(format!("{surface}[{index}] must be non-empty and trimmed"));
        }
        if !set.insert(value.clone()) {
            return Err(format!("{surface} repeats '{value}'"));
        }
    }
    if set.is_empty() {
        return Err(format!("{surface} must not be empty"));
    }
    Ok(set)
}

fn required_text(
    value: &Value,
    field: &str,
    surface: &str,
) -> Result<String, GitHubCopilotAcpFailure> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            GitHubCopilotAcpFailure::protocol(
                "artifact_text_missing",
                format!("GitHub Copilot ACP {surface} has no non-empty '{field}'"),
            )
        })
}

fn content_text(
    update: &Map<String, Value>,
    kind: &str,
) -> Result<String, GitHubCopilotAcpFailure> {
    let content = update
        .get("content")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            GitHubCopilotAcpFailure::protocol(
                "acp_content_invalid",
                format!("GitHub Copilot ACP {kind} has no content object"),
            )
        })?;
    if content.get("type").and_then(Value::as_str) != Some("text") {
        return Err(GitHubCopilotAcpFailure::protocol(
            "acp_content_type_unsupported",
            format!("GitHub Copilot ACP {kind} content is not text"),
        ));
    }
    content
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            GitHubCopilotAcpFailure::protocol(
                "acp_content_text_missing",
                format!("GitHub Copilot ACP {kind} content has no text"),
            )
        })
}

fn materialize(
    output_root: &Path,
    label: &str,
    payload: Value,
    route: ProviderEmissionRoute,
) -> Result<ProviderEmission, GitHubCopilotAcpFailure> {
    let bytes = serde_json::to_vec(&payload).map_err(|error| {
        GitHubCopilotAcpFailure::protocol(
            "output_encode_failed",
            format!("GitHub Copilot ACP output could not be encoded: {error}"),
        )
    })?;
    let artifact = InterfaceArtifactFile::write(output_root, label, &bytes).map_err(|error| {
        GitHubCopilotAcpFailure::protocol(
            "output_materialize_failed",
            format!("GitHub Copilot ACP output could not be materialized: {error}"),
        )
    })?;
    Ok(ProviderEmission { artifact, route })
}

pub fn materialize_failure(
    output_root: &Path,
    failure: &GitHubCopilotAcpFailure,
) -> Result<InterfaceArtifactFile, GitHubCopilotAcpFailure> {
    let payload = json!({
        "label": "turn.error",
        "error": {
            "class": failure.class,
            "retryable": failure.retryable,
            "code": failure.code,
            "detail": failure.detail,
            "evidence": failure.evidence
        }
    });
    let bytes = serde_json::to_vec(&payload).map_err(|error| {
        GitHubCopilotAcpFailure::protocol(
            "error_encode_failed",
            format!("GitHub Copilot ACP error could not be encoded: {error}"),
        )
    })?;
    InterfaceArtifactFile::write(output_root, "turn.error", &bytes).map_err(|error| {
        GitHubCopilotAcpFailure::protocol(
            "error_materialize_failed",
            format!("GitHub Copilot ACP error could not be materialized: {error}"),
        )
    })
}

fn spawn_stderr_reader(
    stderr: tokio::process::ChildStderr,
    tail: Arc<Mutex<VecDeque<String>>>,
    maximum: usize,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut bytes = 0_usize;
        while let Ok(Some(line)) = lines.next_line().await {
            let redacted = redact_detail(line);
            bytes = bytes.saturating_add(redacted.len());
            let mut guard = tail.lock().await;
            guard.push_back(redacted);
            while bytes > maximum {
                let Some(removed) = guard.pop_front() else {
                    break;
                };
                bytes = bytes.saturating_sub(removed.len());
            }
        }
    })
}

async fn bounded_stderr(tail: &Arc<Mutex<VecDeque<String>>>) -> String {
    tail.lock()
        .await
        .iter()
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

fn redacted_json(value: Value) -> Value {
    let encoded = serde_json::to_string(&value).unwrap_or_else(|_| "null".to_string());
    serde_json::from_str(&redact_detail(encoded)).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concrete_descriptor_selects_external_acp_wire() {
        let descriptor: InterfaceDescriptor =
            serde_json::from_str(include_str!("../../../interface.json")).expect("descriptor");
        let secrets = tempfile::tempdir().expect("secrets");
        let var = tempfile::tempdir().expect("var");
        GitHubCopilotAcpDriver::from_descriptor(
            &descriptor,
            secrets.path().to_path_buf(),
            var.path().to_path_buf(),
        )
        .expect("driver");
    }
}
