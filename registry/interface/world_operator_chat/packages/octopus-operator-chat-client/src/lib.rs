use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use octopus_descriptors::{EeId, InterfaceId, ToolDescriptorId, parse_strict};
use octopus_ee::EeTemplateDescriptor;
use octopus_interface_transport::SpawnLifeConfig;
use octopus_interfaces::{InterfaceDescriptor, interface_is_worker_pin};
use octopus_ledger::RequiredVerifiedArtifact;
use octopus_policy::{
    AuthoredInterfaceBinding, BindingArtifactPolicy, InterfaceBindingConfig, WorkerInterfacePolicy,
};
use octopus_runtime::run_record::{ArtifactBinding, RunRecord};
use octopus_tool_registry::{ToolDescriptor, load_tool_descriptor_from_dir};
use octopus_world_terminal_client::{
    CapabilityFormationRequest, FormedCapabilityTopology, form_capability_topology,
    resolve_live_ee_policy_chain,
};
use sha2::{Digest, Sha256};

pub mod context_bindings;
pub mod collection_context;
mod maintenance_attachment;
pub mod maintenance_observer;
pub mod publication_selection;
pub mod input_intake;
mod retained_connections;
mod home_voice;
mod home_outputs;
pub mod native_run_authoring;
mod writer_runs;
pub use writer_runs::{OperatorWriterRunPlanRequest, OperatorWriterSchemaPlanRequest, plan_operator_writer_runs, plan_operator_writer_schema};
#[cfg(test)]
#[path = "../tests/support/home_delegation.rs"]
mod home_delegation;
#[cfg(test)]
#[path = "../tests/support/home_turn_controls.rs"]
mod home_turn_controls;
#[cfg(test)]
#[path = "../tests/support/retained_connections.rs"]
mod retained_connections_tests;
#[cfg(test)]
#[path = "../tests/support/input_intake.rs"]
mod input_intake_tests;
pub mod run_composition;
pub mod run_revision;
pub mod task_retirement;
pub use run_composition::{RunCompositionPlan, RunCompositionRequest, plan_run_composition};

pub const OPERATOR_CHAT_INTERFACE: &str = "world_operator_chat";
const PROVIDER_HANDOFF_COMPLETE_LABEL: &str = "provider.handoff.complete";
const PROVIDER_TRANSPORT_DISPATCH_LABEL: &str = "provider.transport.dispatch";

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorChatPlanRequest {
    pub registry_root: PathBuf,
    pub ee_instances_root: PathBuf,
    #[serde(default)]
    pub ee_policy_root: Option<PathBuf>,
    #[serde(default)]
    pub run_records_root: Option<PathBuf>,
    pub write_hash_algorithm: WriteHashAlgorithm,
    #[serde(default)]
    pub interface_private_state_root: Option<PathBuf>,
    pub chat_id: String,
    pub provider_interface_id: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub service_tier: Option<String>,
    #[serde(default)]
    pub ee_id: Option<String>,
    #[serde(default)]
    pub worker_interface_id: Option<String>,
    pub client_surface_role: String,
    pub cycle: String,
    #[serde(default)]
    pub door_host_slots: BTreeMap<String, BTreeMap<String, PathBuf>>,
    #[serde(default)]
    pub external_inputs: Vec<ExternalInputSeed>,
    #[serde(default)]
    pub external_relays: Vec<ExternalRelaySeed>,
    #[serde(default)]
    pub model_outputs: Vec<ModelOutputSeed>,
    #[serde(default)]
    pub context_artifact_bindings: Vec<ArtifactBindingSeed>,
    #[serde(default)]
    pub payload_artifact_bindings: Vec<octopus_ledger::ArtifactRef>,
    #[serde(default)]
    pub publications: Vec<octopus_context_collections::CollectionSelection>,
    #[serde(default)]
    pub selected_worker_environment_kinds: Vec<String>,
    #[serde(default)]
    pub selected_tools: Vec<SelectedToolSeed>,
    #[serde(default)]
    pub retain_current_capabilities: bool,
    #[serde(default)]
    pub existing_run_record: Option<serde_json::Value>,
    pub conversation: ConversationSeed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteHashAlgorithm {
    Blake3,
    Sha256,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalInputSeed {
    pub checkpoint_id: String,
    pub interface_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_verified_artifacts: Vec<RequiredVerifiedArtifact>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalRelaySeed {
    pub checkpoint_id: String,
    pub interface_id: String,
    pub outbound_interface_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_verified_artifacts: Vec<RequiredVerifiedArtifact>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelOutputSeed {
    pub checkpoint_id: String,
    pub interface_id: String,
    pub required_verified_artifact: RequiredVerifiedArtifact,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedToolSeed {
    pub tool: String,
    pub interface: String,
    pub worker_environment_kind: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationSeed {
    pub ledger_offset: u64,
    #[serde(default)]
    pub artifact_bindings: Vec<ArtifactBindingSeed>,
    #[serde(default)]
    pub pinned_artifact_bindings: Vec<ArtifactBindingSeed>,
    #[serde(default)]
    pub pinned_context_items: Vec<PinnedContextSeed>,
    #[serde(default)]
    pub continued_from: Option<ContinuationSeed>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedContextSeed {
    pub artifact_id: String,
    pub source_artifact_id: String,
    pub source_message_id: String,
    pub text: String,
    pub start: u64,
    pub end: u64,
    pub role: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactBindingSeed {
    pub id: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuationSeed {
    pub run_id: String,
    pub artifact_id: String,
}

#[derive(Debug, serde::Serialize)]
pub struct OperatorChatPlan {
    pub run_id: String,
    pub chat_interface_id: String,
    pub model_interface_id: String,
    pub worker_interface_id: String,
    pub client_surface_role: String,
    pub authoring_source_interface_id: String,
    pub authoring_environment_interface_id: String,
    pub model: String,
    pub reasoning_effort: String,
    pub service_tier: Option<String>,
    pub expected_before_hash_algorithm: WriteHashAlgorithm,
    pub ee_template_chain: Vec<String>,
    pub capability_doors: BTreeMap<String, String>,
    pub capability_tools: BTreeMap<String, String>,
    pub authoring_tool_id: String,
    pub worker_relays: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub publication_evidence: Vec<publication_selection::PublicationSelectionEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publication_expansion: Option<publication_selection::PublicationExpansionReceipt>,
    #[serde(skip)]
    pub authoring_profile: OctopusOsAuthoringProfile,
    #[serde(rename = "authoring_profile")]
    authored_authoring_profile: serde_json::Value,
    pub required_interface_ids: Vec<String>,
    pub required_tool_ids: Vec<String>,
    pub interface_writes: Vec<PlannedClientWrite>,
    pub run_writes: Vec<PlannedClientWrite>,
    pub removals: Vec<PlannedClientRemoval>,
    pub run_record_content: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedClientWrite {
    pub path: PathBuf,
    pub content: String,
    pub tag: String,
    pub expected_before_hash: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct PlannedClientRemoval {
    pub path: PathBuf,
    pub tag: String,
    pub files: Vec<PlannedClientRemovalFile>,
}

#[derive(Debug, serde::Serialize)]
pub struct PlannedClientRemovalFile {
    pub relative_path: PathBuf,
    pub sha256: String,
}

#[derive(Debug, serde::Deserialize)]
struct ClientConfig {
    schema_version: u32,
    interface: String,
    artifact_labels: ClientArtifactLabels,
    authoring: ClientAuthoringConfig,
    run: ClientRunConfig,
    #[serde(default)]
    home_voice: Option<home_voice::HomeVoiceConfig>,
    #[serde(default)]
    result_receiving_updates: Vec<ResultReceivingCeilingPredecessor>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultReceivingCeilingPredecessor {
    worker_template_interface: String,
    mark_outcomes: Vec<octopus_descriptors::MarkId>,
}

#[derive(Debug, serde::Deserialize)]
struct ClientArtifactLabels {
    message: String,
    turn_control: String,
    tool_call: String,
    #[serde(flatten)]
    _rest: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
struct ClientAuthoringConfig {
    source_interface: String,
    room_interface: String,
    tool: String,
    result_timeout_ms: u64,
    #[serde(flatten)]
    _rest: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
struct ClientRunConfig {
    interface_prefix: String,
    run_prefix: String,
    ee: String,
    worker: String,
    tool_call_mark: String,
    tool_call_verifier: String,
    #[serde(default)]
    tool_result_receiving: Option<ToolResultReceiving>,
    listen_env: String,
    context_artifact_bindings: Vec<ArtifactBindingSeed>,
    #[serde(flatten)]
    _rest: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolResultReceiving {
    mark: octopus_descriptors::MarkId,
    verifier: octopus_descriptors::VerifierId,
    params: serde_json::Value,
}

impl ToolResultReceiving {
    fn requirement(&self, tool: &str) -> Result<RequiredVerifiedArtifact, String> {
        if self.mark.as_str().is_empty() || self.mark.as_str().trim() != self.mark.as_str()
            || self.verifier.as_str().is_empty() || self.verifier.as_str().trim() != self.verifier.as_str()
            || !self.params.is_object()
        {
            return Err("tool_result_receiving requires exact mark/verifier ids and object params".into());
        }
        Ok(RequiredVerifiedArtifact { mark: self.mark.clone(), artifact_label: format!("{tool}.result"),
            verifier: self.verifier.clone(), params: self.params.clone() })
    }
}

struct PlannedIdentity {
    voice: Option<home_voice::PlannedVoice>,
    native_communication: Option<run_composition::CompositionCommunication>,
    chat_id: String,
    run_id: String,
    ee_id: String,
    chat_interface_id: String,
    model_interface_id: String,
    worker_interface_id: String,
    authoring_source_id: String,
    authoring_environment_id: String,
    surface_role: String,
    provider_choice_interface: String,
    worker_choice_interface: String,
    capability_doors: BTreeMap<String, String>,
    capability_tools: BTreeMap<String, String>,
    capability_tool_sources: BTreeMap<String, ToolDescriptor>,
    authoring_tool_id: String,
    code_mode_owners: BTreeMap<String, PlannedCodeModeOwner>,
}

struct ResolvedProvider {
    interface: InterfaceDescriptor,
    model: String,
    reasoning_effort: String,
    service_tier: Option<String>,
    client_operation_request_labels: Vec<String>,
}

struct ResolvedProviderModel {
    model: String,
    default_reasoning_effort: String,
    supported_reasoning_efforts: Vec<String>,
}

struct RunSelection {
    ee_id: String,
    worker_choice_interface: String,
    provider_choice_interface: String,
    provider_runtime_interface: Option<String>,
    run_id: Option<String>,
    chat_interface_id: Option<String>,
    worker_runtime_interface: Option<String>,
    surface_role: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OctopusOsInterfaceSidecar {
    schema_version: u32,
    workspace: String,
    interface: String,
    owner_interface: String,
    component: String,
    surface_role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_choice_interface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    worker_choice_interface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    authoring_profile: Option<OctopusOsAuthoringProfile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    task_authoring_profiles: Vec<OctopusOsAuthoringProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    presentation: Option<OctopusOsPresentation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    voice_context: Option<home_voice::HomeVoiceContext>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OctopusOsAuthoringProfile {
    pub ee_id: String,
    pub worker_choice_interface: String,
    pub door_host_slots: BTreeMap<String, BTreeMap<String, PathBuf>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_inputs: Vec<ExternalInputSeed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_relays: Vec<ExternalRelaySeed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_outputs: Vec<ModelOutputSeed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_artifact_bindings: Vec<ArtifactBindingSeed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub payload_artifact_bindings: Vec<octopus_ledger::ArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_worker_environment_kinds: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_tools: Vec<SelectedToolSeed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publication_evidence: Vec<publication_selection::PublicationSelectionEvidence>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OctopusOsPresentation {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    name: String,
    projects: Vec<OctopusOsProjectPresentation>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OctopusOsProjectPresentation {
    ee_id: String,
    name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    icon: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    color: String,
}

struct PreservedOctopusOsClientConfiguration {
    declaration: OctopusOsInterfaceSidecar,
    authored: serde_json::Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum BoundCheckpointRole {
    Stable,
    IndependentInput,
    OwnerInput,
    VerifiedInput,
    ResultInput,
    ProviderArtifactOutput,
    ProviderInterfaceOutput,
    ProviderToolOutput,
    ProviderTransportDispatch,
    ProviderHandoffCompletion,
    AuthoringCall,
    AuthoringResult,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundAuthoringWriter {
    owner: InterfaceId,
    source: InterfaceId,
    door: InterfaceId,
    tool: String,
    call_label: String,
    result_label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundCheckpointTemplate {
    name: String,
    role: BoundCheckpointRole,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    result_receiving: BTreeMap<String, RequiredVerifiedArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    static_artifact_bindings: Vec<octopus_descriptors::ArtifactId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner_authoring: Option<BoundAuthoringWriter>,
    interface_binding: AuthoredInterfaceBinding,
    outbound_interface_bindings: Vec<AuthoredInterfaceBinding>,
    include_context: bool,
    required_verified_artifacts: Vec<RequiredVerifiedArtifact>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundRunAuthoringTopology {
    schema_version: u32,
    run: octopus_descriptors::RunId,
    worker: InterfaceId,
    provider_interface: InterfaceId,
    authoring_source_interface: InterfaceId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    payload_artifact_bindings: Vec<octopus_ledger::ArtifactRef>,
    checkpoints: Vec<BoundCheckpointTemplate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    writer_run: Option<octopus_provider_run_authoring::WriterRunBinding>,
}

pub fn plan_operator_chat_topology(
    request: &OperatorChatPlanRequest,
) -> Result<OperatorChatPlan, String> {
    let plan = plan_operator_chat_topology_files(request, false)?;
    validate_ee_capability_allowances(&request.registry_root, &plan.ee_template_chain.iter().map(|id|octopus_descriptors::EeTemplateId::new(id)).collect::<Vec<_>>(),
        &plan.required_interface_ids, &plan.required_tool_ids)?;
    Ok(plan)
}

fn plan_operator_chat_topology_files(
    request: &OperatorChatPlanRequest,
    preserve_existing_writers: bool,
) -> Result<OperatorChatPlan, String> {
    plan_operator_chat_topology_files_for_source(request, preserve_existing_writers, None)
}

fn plan_operator_chat_topology_files_for_source(
    request: &OperatorChatPlanRequest,
    preserve_existing_writers: bool,
    native_metadata: Option<&octopus_ledger::InvocationMetadata>,
) -> Result<OperatorChatPlan, String> {
    let chat_id = safe_segment(&request.chat_id, "chat_id")?;
    let cycle = safe_segment(&request.cycle, "cycle")?;
    let registry_root = require_absolute("registry_root", &request.registry_root)?;
    let ee_instances_root = require_absolute("ee_instances_root", &request.ee_instances_root)?;
    if let Some(root) = request.interface_private_state_root.as_deref() {
        require_absolute("interface_private_state_root", root)?;
    }
    let mut external_interfaces = validate_external_routes(
        &request.external_inputs,
        &request.external_relays,
        &request.model_outputs,
    )?;
    let interface_root = registry_root.join("interface");
    let mut config = load_client_config(&interface_root)?;
    let selection = resolve_run_selection(request, &config)?;
    if request.retain_current_capabilities { config.run.context_artifact_bindings.clear(); }

    let ee_policy_root = match request.ee_policy_root.as_deref() {
        Some(root) => require_absolute("ee_policy_root", root)?,
        None => ee_instances_root,
    };
    let chain = resolve_live_ee_policy_chain(ee_policy_root, &EeId::new(&selection.ee_id))
        .map_err(|error| error.to_string())?;
    if selection.surface_role == "home" {
        if chain.len() != 1 {
            return Err("Home must live directly in the root Existence EE; select its actual root EE".into());
        }
        let path = registry_root.join("ee_template").join(chain[0].template.as_str()).join("ee_template.json");
        let template: EeTemplateDescriptor = parse_strict(&std::fs::read(&path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        if !template.bootstrap_root {
            return Err("Home requires the declared bootstrap-root EE".into());
        }
    }
    let retain_current_capabilities = preserve_existing_writers || request.retain_current_capabilities;
    if retain_current_capabilities && request.existing_run_record.is_none() {
        return Err("retained capability planning requires its existing record".into());
    }
    let current_worker = if retain_current_capabilities {
        let existing = request.existing_run_record.as_ref().ok_or("retained capability planning requires its existing record")?;
        let current = RunRecord::from_bytes(&serde_json::to_vec(existing).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
        Some(run_composition::current_run::form_current_worker(registry_root, ee_policy_root, &current,
            selection.chat_interface_id.as_deref().ok_or("existing-run planning has no client owner")?, &chat_id)?)
    } else { None };
    let topology = if let Some(current) = &current_worker { current.topology.clone() } else { form_capability_topology(
        registry_root,
        &CapabilityFormationRequest::from_live_chain(
            InterfaceId::new(&selection.worker_choice_interface),
            chain.iter().cloned(),
        ),
    )
    .map_err(|error| error.to_string())? };
    if let Some(result) = &config.run.tool_result_receiving {
        let worker = current_worker.as_ref().map(|current| current.topology.worker.as_str()).unwrap_or(&selection.worker_choice_interface);
        let policy: WorkerInterfacePolicy = serde_json::from_slice(&std::fs::read(interface_root.join(worker).join("policy.json")).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
        if !result_receiving_authorized(registry_root, &chain, &policy, result)? {
            return Err("selected worker/EE policy does not admit the tool-result correspondence mark".into());
        }
    }
    ensure_external_interfaces_admitted_by_ee(&topology, &external_interfaces)?;
    let initial_topology = select_capability_topology(
        topology.clone(),
        &config,
        &request.selected_worker_environment_kinds,
        &request.selected_tools,
    )?;
    let initial_interfaces = initial_topology.tool_bindings.keys().cloned()
        .chain(external_interfaces.iter().map(|id| InterfaceId::new(id.as_str())))
        .chain(std::iter::once(InterfaceId::new(&selection.provider_choice_interface)))
        .collect::<Vec<_>>();
    let initial_provider = resolve_provider(&interface_root, &selection.provider_choice_interface, request.model.as_deref().unwrap_or(""),
        request.reasoning_effort.as_deref().unwrap_or(""), request.service_tier.as_deref(), &initial_topology)?;
    let initial_identity = plan_identity(&config, &chat_id, &initial_provider, &initial_topology, &selection)?;
    let preserved_client_configuration = if initial_identity.owns_client() {
        preserve_octopus_os_client_configuration(&interface_root, &initial_identity.chat_interface_id, &initial_identity)?
    } else { None };
    let retained_evidence = if request.existing_run_record.is_some() {
        preserved_client_configuration.as_ref().and_then(|preserved| preserved.declaration.authoring_profile.as_ref())
            .map(|profile| profile.publication_evidence.clone()).unwrap_or_default()
    } else { Vec::new() };
    publication_selection::validate_retained_snapshots(registry_root, &retained_evidence)?;
    let original_selection = publication_selection::PublicationSelectionSeeds::from_operator(request);
    let explicit_publications = request.publications.clone();
    let publication_defaults = if request.retain_current_capabilities { Vec::new() } else {
        publication_selection::declared_publication_defaults(registry_root, initial_interfaces.iter().cloned())?
    };
    let mut publications = explicit_publications.clone();
    let mut reused_defaults = Vec::new();
    for pin in publication_defaults.iter().flat_map(|default|&default.publications) {
        if !explicit_publications.contains(pin) && retained_evidence.iter().any(|evidence| &evidence.pin == pin) {
            if !reused_defaults.contains(pin) { reused_defaults.push(pin.clone()); }
        } else { publications.push(pin.clone()); }
    }
    let mut expanded_request;
    let mut publication_writes = Vec::new();
    let mut staged_publication_artifacts = BTreeMap::new();
    let mut publication_evidence = Vec::new();
    let request = if publications.is_empty() { request } else {
        publication_selection::validate_target_read_formation(registry_root, ee_policy_root, &selection.ee_id,
            current_worker.as_ref().map(|current| current.policy.id.as_str()).unwrap_or(&selection.worker_choice_interface))?;
        let mut source_context = if request.retain_current_capabilities {
            context_bindings::retained_context_bindings(request.existing_run_record.as_ref().ok_or("retained publication selection requires its existing record")?,
                &initial_identity.model_interface_id, &request.context_artifact_bindings)?
        } else { context_bindings::declared_context_bindings(registry_root, initial_interfaces, &request.context_artifact_bindings)? };
        for binding in &config.run.context_artifact_bindings {
            if !source_context.contains(binding) { source_context.push(binding.clone()); }
        }
        let authority = match native_metadata {
            Some(metadata) => publication_selection::PublicationSourceAuthority::Native { metadata },
            None => publication_selection::PublicationSourceAuthority::Operator { context_bindings:&source_context, payload_bindings:&request.payload_artifact_bindings },
        };
        let resolved = publication_selection::resolve_publication_selection(registry_root,
            &octopus_context_collections::ScopeContext { ee_id:selection.ee_id.clone(),
                worker_interface:selection.worker_runtime_interface.clone().unwrap_or_else(|| format!("{}_{}",selection.worker_choice_interface,chat_id)) },
            &publications, publication_selection::PublicationSelectionSeeds::from_operator(request), authority, request.write_hash_algorithm)?;
        expanded_request = request.clone();
        resolved.selection.apply_operator(&mut expanded_request);
        publication_writes = resolved.artifact_writes;
        staged_publication_artifacts = resolved.staged_artifacts;
        publication_evidence = resolved.evidence;
        &expanded_request
    };
    external_interfaces = validate_external_routes(&request.external_inputs, &request.external_relays, &request.model_outputs)?;
    ensure_external_interfaces_admitted_by_ee(&topology, &external_interfaces)?;
    let topology = if publications.is_empty() { initial_topology } else {
        select_capability_topology(topology, &config, &request.selected_worker_environment_kinds, &request.selected_tools)?
    };

    let room_binding = topology
        .tool_bindings
        .get(&InterfaceId::new(&config.authoring.room_interface))
        .ok_or_else(|| {
            format!(
                "authoring room interface '{}' is absent from the formed capability topology",
                config.authoring.room_interface
            )
        })?;
    if !room_binding
        .tools
        .iter()
        .any(|tool| tool.tool.as_str() == config.authoring.tool)
    {
        return Err(format!(
            "authoring tool '{}' is absent from formed room interface '{}'",
            config.authoring.tool, config.authoring.room_interface
        ));
    }

    let provider = resolve_provider(
        &interface_root,
        &selection.provider_choice_interface,
        request.model.as_deref().unwrap_or(""),
        request.reasoning_effort.as_deref().unwrap_or(""),
        request.service_tier.as_deref(),
        &topology,
    )?;
    let mut identity = plan_identity(&config, &chat_id, &provider, &topology, &selection)?;
    if let Some(current) = &current_worker { identity.capability_tool_sources = current.tools.clone(); }
    preserve_selected_file_writer(&config, &chat_id, &topology, &request.selected_tools,
        &request.selected_worker_environment_kinds, &mut identity)?;
    identity.voice = home_voice::select(&interface_root, &config, &identity, &topology,
        request.existing_run_record.as_ref().filter(|_| current_worker.is_some()))?;
    ensure_external_interfaces_admitted_by_ee(&topology, &external_interfaces)?;
    let selected_context = if request.retain_current_capabilities {
        context_bindings::retained_context_bindings(request.existing_run_record.as_ref().ok_or("retained context selection requires its existing record")?,
            &identity.model_interface_id, &request.context_artifact_bindings)?
    } else { context_bindings::declared_context_bindings(
        registry_root,
        topology.tool_bindings.keys().cloned()
            .chain(external_interfaces.iter().map(|id| InterfaceId::new(id.as_str())))
            .chain(std::iter::once(provider.interface.id.clone())),
        &request.context_artifact_bindings,
    )? };
    plan_code_mode_owners(
        registry_root,
        &config,
        &topology,
        &mut identity,
        &selected_context,
        request.existing_run_record.as_ref(),
    )?;
    let selected_publication_evidence = publication_evidence.clone();
    publication_evidence = publication_selection::retain_publication_evidence(&retained_evidence, &publication_evidence)?;
    let requested_authoring_profile = OctopusOsAuthoringProfile {
        ee_id: selection.ee_id.clone(),
        worker_choice_interface: selection.worker_choice_interface.clone(),
        door_host_slots: request.door_host_slots.clone(),
        external_inputs: request.external_inputs.clone(),
        external_relays: request.external_relays.clone(),
        model_outputs: request.model_outputs.clone(),
        context_artifact_bindings: request.context_artifact_bindings.clone(),
        payload_artifact_bindings: request.payload_artifact_bindings.clone(),
        selected_worker_environment_kinds: request.selected_worker_environment_kinds.clone(),
        selected_tools: request.selected_tools.clone(),
        publication_evidence: publication_evidence.clone(),
    };
    let publication_expansion = if publications.is_empty() && publication_defaults.is_empty() { None } else {
        let mut requested = requested_authoring_profile.clone();
        original_selection.apply_profile(&mut requested);
        requested.publication_evidence = retained_evidence.to_vec();
        let receipt = publication_selection::PublicationExpansionReceipt { requested, publications:explicit_publications,
            defaults:publication_defaults, reused_defaults, selected:selected_publication_evidence, expanded:requested_authoring_profile.clone() };
        publication_selection::validate_publication_expansion_receipt(registry_root, &receipt)?;
        Some(receipt)
    };
    let previous_authoring_profile = if request.existing_run_record.is_some() {
        preserved_client_configuration.as_ref()
        .and_then(|preserved| preserved.declaration.authoring_profile.clone())
        .ok_or_else(|| "existing operator chat task has no durable authoring profile".to_string())?
    } else {
        requested_authoring_profile.clone()
    };
    let mut authored_authoring_profile = serde_json::to_value(&requested_authoring_profile)
        .map_err(|error| error.to_string())?;
    if let Some(preserved) = preserved_client_configuration.as_ref() {
        if let Some(profile) = preserved.declaration.authoring_profile.as_ref() {
            let previous = serde_json::to_value(profile).map_err(|error| error.to_string())?;
            preserve_unchanged_client_json(&preserved.authored["authoring_profile"], &previous, &mut authored_authoring_profile);
        }
    }
    let retained_connections = retained_connections::selected(
        request.existing_run_record.as_ref(), &identity, Some(&previous_authoring_profile),
        &request.external_inputs, &request.external_relays, &request.model_outputs,
    )?;
    let retained_policy = retained_connections::policy(
        &interface_root, &identity, &retained_connections,
    )?;
    external_interfaces.extend(retained_policy.keys().map(ToString::to_string));
    external_interfaces.sort();
    external_interfaces.dedup();
    ensure_external_interfaces_admitted_by_ee(&topology, &external_interfaces)?;
    let worker_relays: Vec<String> = topology
        .worker_relays
        .keys()
        .map(|pin| pin.as_str().to_string())
        .collect();

    let mut writes = publication_writes;
    write_code_mode_owner_artifacts(registry_root, &identity, &mut writes)?;
    let previous_owned = request
        .existing_run_record
        .as_ref()
        .map(|record| existing_private_capability_ownership(record, &identity, registry_root))
        .transpose()?;
    plan_chat_interface(
        &interface_root,
        &config,
        &identity,
        Some(&requested_authoring_profile),
        request.existing_run_record.is_some(),
        &mut writes,
    )?;
    plan_capability_doors(
        registry_root,
        &interface_root,
        &config,
        &topology,
        &identity,
        &request.door_host_slots,
        &mut writes,
    )?;
    plan_authoring_interfaces(&interface_root, &config, &topology, &identity, &mut writes)?;
    plan_independent_code_mode_authoring(
        registry_root,
        &config,
        &topology,
        &identity,
        &mut writes,
    )?;
    plan_private_tools(registry_root, &config, &topology, &identity, &mut writes)?;
    let private_worker_role_artifacts = plan_worker_pin(
        &interface_root,
        &config,
        &selection.worker_choice_interface,
        &topology,
        &identity,
        &external_interfaces,
        current_worker.as_ref().map(|current| &current.policy),
        &mut writes,
    )?;
    retained_connections::preserve_policy(&identity, &retained_policy, &mut writes)?;
    let role_dependency_workers = worker_role_dependency_interfaces(
        &interface_root,
        topology.worker_relays.keys(),
        &external_interfaces,
    )?;
    let role_artifacts = formed_worker_role_artifacts(
        &interface_root,
        &private_worker_role_artifacts,
        role_dependency_workers.iter(),
    )?;
    let retained_context_ids = if request.retain_current_capabilities {
        let existing = RunRecord::from_bytes(&serde_json::to_vec(request.existing_run_record.as_ref().ok_or("retained instruction selection requires its existing record")?).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        octopus_provider_run_authoring::provider_context_bindings(&existing, &InterfaceId::new(&identity.model_interface_id))?
            .into_iter().map(|binding| binding.id.to_string()).collect::<BTreeSet<_>>()
    } else { BTreeSet::new() };
    let mut selected_context = context_bindings::with_instruction_source_bindings_from_planned(
        registry_root,
        role_artifacts.iter().cloned()
            .chain(config.run.context_artifact_bindings.iter().map(|binding| binding.id.clone()))
            .chain(selected_context.iter().map(|binding| binding.id.clone()))
            .filter(|id| !retained_context_ids.contains(id)),
        &selected_context,
        &staged_publication_artifacts,
    )?;
    let catalogue = collection_context::plan(collection_context::PlanContext {
        registry_root, ee_instances_root: ee_policy_root, ee_id: &selection.ee_id, identity: &identity,
        bindings: config.run.context_artifact_bindings.iter()
            .chain(request.conversation.artifact_bindings.iter())
            .chain(request.conversation.pinned_artifact_bindings.iter())
            .chain(selected_context.iter()).cloned().collect(),
        native_metadata, existing: request.existing_run_record.as_ref(),
    }, &mut selected_context, &mut writes)?;
    plan_provider_interface(
        &interface_root,
        &config,
        &identity,
        &provider,
        &topology,
        &private_worker_role_artifacts,
        &catalogue.inline_ids(),
        request.interface_private_state_root.as_deref(),
        &mut writes,
    )?;
    home_voice::plan_descriptors(registry_root, &config, &topology, &identity, &mut writes)?;
    if let Some(binding) = home_outputs::plan(request, &identity, &private_worker_role_artifacts, &mut writes)? { selected_context.push(binding); }
    if let Some(binding) = native_run_authoring::plan(native_run_authoring::PlanContext {
        registry_root,
        ee_instances_root: &request.ee_instances_root,
        run_records_root: request.run_records_root.as_deref(),
        interface_private_state_root: request.interface_private_state_root.as_deref(),
        config: &config,
        identity: &identity,
        provider: &provider,
        topology: &topology,
        door_host_slots: &request.door_host_slots,
    }, &mut writes)? { selected_context.push(binding); }
    if let Some(current) = &current_worker {
        for write in &mut writes {
            let owner=write.path.parent().and_then(Path::file_name).and_then(|name|name.to_str());
            let file=write.path.file_name().and_then(|name|name.to_str());
            if owner.is_some_and(|owner|current.retained_doors.contains(owner)) && matches!(file,Some("interface.json" | "life.json")) {
                write.content=std::fs::read_to_string(&write.path).map_err(|e|e.to_string())?;
            } else if file==Some("interface.json") {
                if let Some((source,_))=identity.capability_doors.iter().find(|(_,exact)|Some(exact.as_str())==owner) {
                    let policy=topology.interface_policies.get(&InterfaceId::new(source)).ok_or("planned current capability has no formed interface policy")?;
                    let mut descriptor:InterfaceDescriptor=parse_strict(write.content.as_bytes()).map_err(|e|e.to_string())?;
                    descriptor.interface_policy.values=policy.context.clone();
                    write.content=pretty_json(&serde_json::to_value(descriptor).map_err(|e|e.to_string())?)?;
                }
            }
            if request.retain_current_capabilities && file == Some("interface.json") && write.path.exists() {
                let accepted: InterfaceDescriptor = parse_strict(&std::fs::read(&write.path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
                let mut planned: InterfaceDescriptor = parse_strict(write.content.as_bytes()).map_err(|e| e.to_string())?;
                if let Some(config) = planned.interface_driver_config.as_mut() {
                    for field in ["context_artifact_bindings", "publications"] {
                        match accepted.interface_driver_config.as_ref().and_then(|config| config.private_config.get(field)) {
                            Some(value) => { config.private_config.insert(field.into(), value.clone()); }
                            None => { config.private_config.remove(field); }
                        }
                    }
                }
                write.content = pretty_json(&serde_json::to_value(planned).map_err(|e| e.to_string())?)?;
            }
        }
    }
    let exact_interfaces = exact_topology_interface_ids(&identity, &external_interfaces);
    let mut required_interface_ids = exact_interfaces.clone();
    required_interface_ids.extend(worker_relays.iter().cloned());
    required_interface_ids.sort();
    required_interface_ids.dedup();
    let mut required_tool_ids = identity
        .capability_tools
        .values()
        .cloned()
        .chain(std::iter::once(identity.authoring_tool_id.clone()))
        .collect::<Vec<_>>();
    required_tool_ids.extend(
        identity
            .code_mode_owners
            .values()
            .map(|owner| owner.authoring_tool.clone()),
    );
    if let Some(voice) = &identity.voice { required_tool_ids.push(voice.writer.clone()); }
    required_tool_ids.sort();
    required_tool_ids.dedup();
    let run_record_content = build_chat_run_record(
        &config,
        &identity,
        &cycle,
        &topology,
        &role_artifacts,
        &selection,
        request.existing_run_record.as_ref(),
        &request.conversation,
        &request.external_inputs,
        &request.external_relays,
        &request.model_outputs,
        &selected_context,
        &request.payload_artifact_bindings,
        &provider.client_operation_request_labels,
        request
            .existing_run_record
            .as_ref()
            .map(|_| &previous_authoring_profile),
    )?;
    let run_record_content = catalogue.wire(&identity.model_interface_id, run_record_content)?;
    let run_record_content = home_outputs::wire(&identity, &role_artifacts, &selected_context, run_record_content)?;
    let (run_record_content, intake) = input_intake::wire(registry_root, &identity, &mut writes, run_record_content)?;
    let (run_record_content, mut voice_run_writes) = home_voice::wire(request, &config, &identity, &role_artifacts, &selected_context, run_record_content)?;
    let run_record_content = context_bindings::configure_tool_context(registry_root, &identity, run_record_content, request.existing_run_record.as_ref())?;
    let run_record_content = if request.retain_current_capabilities {
        context_bindings::retain_context_access(run_record_content,
            request.existing_run_record.as_ref().ok_or("retained context access requires its existing record")?, &identity.model_interface_id)?
    } else { run_record_content };
    let (run_record_content, mut maintenance_runs) = maintenance_attachment::wire(
        maintenance_attachment::AttachmentRequest {
            registry_root,
            ee_instances_root: ee_policy_root,
            run_records_root: request.run_records_root.as_deref(),
            interface_private_state_root: request.interface_private_state_root.as_deref(),
            config: &config,
            identity: &identity,
            source_input_labels: vec![config.artifact_labels.message.clone()],
            existing: request.existing_run_record.as_ref(),
            write_hash_algorithm: request.write_hash_algorithm,
        },
        run_record_content,
        &mut writes,
        &mut required_interface_ids,
        &mut required_tool_ids,
    )?;
    if let Some(intake) = intake {
        for (id, component) in [
            (&intake.interface, "authoring_source"),
            (&intake.door, "authoring_environment"),
        ] {
            plan_octopus_os_sidecar(
                &registry_root.join("interface"), id, &identity, component,
                false, None, false, &mut writes,
            )?;
        }
        required_interface_ids.extend([intake.interface, intake.door]);
        required_tool_ids.push(intake.tool);
        required_interface_ids.sort();
        required_interface_ids.dedup();
        required_tool_ids.sort();
        required_tool_ids.dedup();
    }
    let (run_record_content, mut run_writes) = if preserve_existing_writers {
        writer_runs::wire_preserving_existing(request, &identity, run_record_content)?
    } else { writer_runs::wire(request, &identity, run_record_content)? };
    run_writes.append(&mut voice_run_writes);
    run_writes.append(&mut maintenance_runs);
    let retained_paths = planned_retained_registry_paths(registry_root, &identity, &writes);
    let removals = previous_owned
        .as_ref()
        .map(|owned| plan_private_capability_removals(registry_root, owned, &retained_paths))
        .transpose()?
        .unwrap_or_default();
    remove_unchanged_writes(&mut writes)?;
    normalize_write_hash_algorithm(&mut writes, request.write_hash_algorithm)?;

    Ok(OperatorChatPlan {
        run_id: identity.run_id.clone(),
        chat_interface_id: identity.chat_interface_id.clone(),
        model_interface_id: identity.model_interface_id.clone(),
        worker_interface_id: identity.worker_interface_id.clone(),
        client_surface_role: identity.surface_role.clone(),
        authoring_source_interface_id: identity.authoring_source_id.clone(),
        authoring_environment_interface_id: identity.authoring_environment_id.clone(),
        model: provider.model.clone(),
        reasoning_effort: provider.reasoning_effort.clone(),
        service_tier: provider.service_tier.clone(),
        expected_before_hash_algorithm: request.write_hash_algorithm,
        ee_template_chain: chain
            .iter()
            .map(|layer| layer.template.as_str().to_string())
            .collect(),
        capability_doors: identity.capability_doors.clone(),
        capability_tools: identity.capability_tools.clone(),
        authoring_tool_id: identity.authoring_tool_id.clone(),
        worker_relays,
        publication_evidence,
        publication_expansion,
        authoring_profile: requested_authoring_profile,
        authored_authoring_profile,
        required_interface_ids,
        required_tool_ids,
        interface_writes: writes,
        run_writes,
        removals,
        run_record_content,
    })
}

fn load_client_config(interface_root: &Path) -> Result<ClientConfig, String> {
    load_named_client_config(interface_root, OPERATOR_CHAT_INTERFACE)
}

fn load_named_client_config(interface_root: &Path, source: &str) -> Result<ClientConfig, String> {
    safe_segment(source, "client template")?;
    let path = interface_root
        .join(source)
        .join("client.json");
    let bytes = std::fs::read(&path)
        .map_err(|error| format!("client config '{}' cannot be read: {error}", path.display()))?;
    let config: ClientConfig = serde_json::from_slice(&bytes)
        .map_err(|error| format!("client config '{}' is malformed: {error}", path.display()))?;
    if config.schema_version != 2 {
        return Err(format!(
            "client config '{}' has unsupported schema_version {}",
            path.display(),
            config.schema_version
        ));
    }
    if config.interface != source {
        return Err(format!(
            "client config '{}' belongs to interface '{}'",
            path.display(),
            config.interface
        ));
    }
    for (value, field) in [
        (&config.artifact_labels.message, "artifact_labels.message"),
        (
            &config.artifact_labels.turn_control,
            "artifact_labels.turn_control",
        ),
        (
            &config.artifact_labels.tool_call,
            "artifact_labels.tool_call",
        ),
        (
            &config.authoring.source_interface,
            "authoring.source_interface",
        ),
        (&config.authoring.room_interface, "authoring.room_interface"),
        (&config.authoring.tool, "authoring.tool"),
        (&config.run.interface_prefix, "run.interface_prefix"),
        (&config.run.run_prefix, "run.run_prefix"),
        (&config.run.ee, "run.ee"),
        (&config.run.worker, "run.worker"),
        (&config.run.tool_call_mark, "run.tool_call_mark"),
        (&config.run.tool_call_verifier, "run.tool_call_verifier"),
        (&config.run.listen_env, "run.listen_env"),
    ] {
        if value.trim().is_empty() {
            return Err(format!("client config field {field} is empty"));
        }
    }
    if config.authoring.result_timeout_ms == 0 {
        return Err("client config authoring.result_timeout_ms must be positive".to_string());
    }
    if let Some(result) = &config.run.tool_result_receiving { result.requirement(&config.authoring.tool)?; }
    let mut predecessors = BTreeSet::new();
    for prior in &config.result_receiving_updates {
        if safe_segment(&prior.worker_template_interface, "result receiving predecessor worker")? != prior.worker_template_interface {
            return Err("result receiving predecessor worker must be exact and trimmed".into());
        }
        for mark in &prior.mark_outcomes {
            if safe_policy_axis_value(mark.as_str(), "result receiving predecessor mark")? != mark.as_str()
                || matches!(mark.as_str(), "." | "..") { return Err("result receiving predecessor mark must name an exact registry folder".into()); }
        }
        if !predecessors.insert(&prior.worker_template_interface)
            || prior.mark_outcomes.iter().collect::<BTreeSet<_>>().len() != prior.mark_outcomes.len()
            || config.run.tool_result_receiving.as_ref().is_some_and(|result| prior.mark_outcomes.contains(&result.mark)) {
            return Err("result receiving predecessors require unique exact worker/mark lists without the new correspondence mark".into());
        }
    }
    Ok(config)
}

fn resolve_run_selection(
    request: &OperatorChatPlanRequest,
    config: &ClientConfig,
) -> Result<RunSelection, String> {
    let requested_surface_role = client_surface_role(&request.client_surface_role)?;
    if let Some(existing) = request.existing_run_record.as_ref() {
        let run_id = existing
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "existing chat run.id is required".to_string())?
            .to_string();
        let ee_id = existing
            .get("ee")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "existing chat run.ee is required".to_string())?
            .to_string();
        let private_worker = existing
            .get("worker")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "existing chat run.worker is required".to_string())?;
        let interface_state = existing
            .get("interface_state")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| "existing chat run.interface_state is required".to_string())?;
        let clients = interface_state
            .iter()
            .filter_map(|(interface, state)| {
                let state = state.as_object()?;
                matches!(
                    state
                        .get("client_surface_role")
                        .and_then(serde_json::Value::as_str),
                    Some("home" | "task")
                )
                .then_some((interface.as_str(), state))
            })
            .collect::<Vec<_>>();
        let [(chat_interface_id, client_state)] = clients.as_slice() else {
            return Err(format!(
                "existing chat run must contain exactly one client interface state, found {}",
                clients.len()
            ));
        };
        let worker_choice_interface = client_state
            .get("worker_choice_interface")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                format!(
                    "existing chat state '{}' does not record its worker choice",
                    chat_interface_id
                )
            })?
            .to_string();
        let provider_choice_interface = client_state
            .get("provider_choice_interface")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                format!(
                    "existing chat state '{}' does not record its provider choice",
                    chat_interface_id
                )
            })?
            .to_string();
        let provider_runtime_interface = client_state
            .get("provider_runtime_interface")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                format!(
                    "existing chat state '{}' does not record its exact provider runtime interface",
                    chat_interface_id
                )
            })?
            .to_string();
        let surface_role = client_state
            .get("client_surface_role")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                format!(
                    "existing chat state '{}' does not record its client surface role",
                    chat_interface_id
                )
            })?;
        if surface_role != requested_surface_role {
            return Err(format!(
                "existing chat surface role is '{}', not the requested '{}'",
                surface_role, requested_surface_role
            ));
        }
        if request.provider_interface_id != provider_choice_interface {
            return Err(format!(
                "existing chat provider choice is '{}', not the requested '{}'",
                provider_choice_interface, request.provider_interface_id
            ));
        }
        if request
            .ee_id
            .as_deref()
            .is_some_and(|requested| requested != ee_id)
        {
            return Err(format!(
                "existing chat run execution environment is '{ee_id}', not the requested one"
            ));
        }
        if request
            .worker_interface_id
            .as_deref()
            .is_some_and(|requested| {
                requested != worker_choice_interface && requested != private_worker
            })
        {
            return Err(format!(
                "existing chat run worker choice is '{worker_choice_interface}', not the requested one"
            ));
        }
        return Ok(RunSelection {
            ee_id,
            worker_choice_interface,
            provider_choice_interface,
            provider_runtime_interface: Some(provider_runtime_interface),
            run_id: Some(run_id),
            chat_interface_id: Some((*chat_interface_id).to_string()),
            worker_runtime_interface: Some(private_worker.to_string()),
            surface_role: requested_surface_role,
        });
    }

    Ok(RunSelection {
        ee_id: safe_segment(request.ee_id.as_deref().unwrap_or(&config.run.ee), "ee_id")?,
        worker_choice_interface: safe_segment(
            request
                .worker_interface_id
                .as_deref()
                .unwrap_or(&config.run.worker),
            "worker_interface_id",
        )?,
        provider_choice_interface: safe_segment(
            &request.provider_interface_id,
            "provider_interface_id",
        )?,
        provider_runtime_interface: None,
        run_id: None,
        chat_interface_id: None,
        worker_runtime_interface: None,
        surface_role: requested_surface_role,
    })
}

fn client_surface_role(value: &str) -> Result<String, String> {
    match value.trim() {
        "home" => Ok("home".to_string()),
        "task" => Ok("task".to_string()),
        other => Err(format!(
            "client_surface_role must be 'home' or 'task', got '{other}'"
        )),
    }
}

fn resolve_provider(
    interface_root: &Path,
    provider_interface_id: &str,
    requested_model: &str,
    requested_reasoning_effort: &str,
    requested_service_tier: Option<&str>,
    topology: &FormedCapabilityTopology,
) -> Result<ResolvedProvider, String> {
    let provider_id = safe_segment(provider_interface_id, "provider_interface_id")?;
    let interface = load_interface_descriptor(interface_root, &provider_id)?;
    if !interface.has_interface_driver() {
        return Err(format!(
            "provider interface '{}' resolves no interface driver",
            provider_id
        ));
    }
    let resolved_config = interface
        .interface_driver_config
        .clone()
        .unwrap_or_default();
    let configured_model = resolved_config
        .private_config
        .get("model")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty() && value.trim() == *value)
        .ok_or_else(|| format!("{provider_id} declares no selected private_config.model"))?
        .to_string();
    let configured_reasoning_effort = resolved_config
        .private_config
        .get("reasoning_effort")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let configured_service_tier = match resolved_config.private_config.get("service_tier") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(tier)) => Some(tier.clone()),
        Some(_) => return Err(format!("{provider_id} service tier must be a native tier id string")),
    };
    let service_tier = requested_service_tier.map(str::to_string).or(configured_service_tier);
    if service_tier.as_ref().is_some_and(|tier| tier.is_empty() || tier.trim() != tier || tier.len() > 80 || tier.chars().any(char::is_control)) {
        return Err(format!("{provider_id} service tier must be a non-empty native tier id"));
    }
    let formed_models = formed_provider_models(topology, &interface.id)?;
    let catalog = resolved_provider_model_catalog(
        &resolved_config,
        &interface.id,
        &formed_models,
        &configured_model,
        &configured_reasoning_effort,
    )?;
    let requested_model = requested_model.trim();
    let model = if requested_model.is_empty() {
        configured_model.clone()
    } else {
        if requested_model.contains(['\r', '\n', '\0']) {
            return Err(format!("{provider_id} requested model is malformed"));
        }
        requested_model.to_string()
    };
    let selected = catalog
        .iter()
        .find(|preset| preset.model == model)
        .ok_or_else(|| {
            format!("{provider_id} formed model catalog does not admit selected model '{model}'")
        })?;
    let requested_reasoning_effort = requested_reasoning_effort.trim();
    let reasoning_effort = if !requested_reasoning_effort.is_empty() {
        requested_reasoning_effort.to_string()
    } else if model == configured_model && !configured_reasoning_effort.is_empty() {
        configured_reasoning_effort
    } else {
        selected.default_reasoning_effort.clone()
    };
    if reasoning_effort.contains(['\r', '\n', '\0']) {
        return Err(format!(
            "{provider_id} requested reasoning effort is malformed"
        ));
    }
    if !selected.supported_reasoning_efforts.is_empty()
        && !selected
            .supported_reasoning_efforts
            .contains(&reasoning_effort)
    {
        return Err(format!(
            "{provider_id} model '{model}' does not support reasoning effort '{reasoning_effort}'"
        ));
    }
    if !reasoning_effort.is_empty()
        && !resolved_config.supported_reasoning_efforts.is_empty()
        && !resolved_config
            .supported_reasoning_efforts
            .contains(&reasoning_effort)
    {
        return Err(format!(
            "{provider_id} reasoning effort '{reasoning_effort}' is outside its interface ceiling"
        ));
    }
    let life = interface_root.join(interface.id.as_str()).join("life.json");
    if !life.is_file() {
        return Err(format!(
            "provider interface '{}' has no self-owned life declaration at '{}'",
            interface.id.as_str(),
            life.display()
        ));
    }
    Ok(ResolvedProvider {
        client_operation_request_labels: provider_client_operation_request_labels(
            &resolved_config.private_config,
            interface.id.as_str(),
        )?,
        interface,
        model,
        reasoning_effort,
        service_tier,
    })
}

fn formed_provider_models(
    topology: &FormedCapabilityTopology,
    provider: &InterfaceId,
) -> Result<BTreeSet<String>, String> {
    let policy = topology.interface_policies.get(provider).ok_or_else(|| {
        format!(
            "provider interface '{}' has no formed interface policy",
            provider.as_str()
        )
    })?;
    let values = policy
        .context
        .get("models")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            format!(
                "provider interface '{}' formed models must be a string array",
                provider.as_str()
            )
        })?;
    let mut models = BTreeSet::new();
    for value in values {
        let model = value
            .as_str()
            .filter(|model| {
                !model.is_empty() && model.trim() == *model && !model.contains(['\r', '\n', '\0'])
            })
            .ok_or_else(|| {
                format!(
                    "provider interface '{}' formed models contains a malformed entry",
                    provider.as_str()
                )
            })?;
        if !models.insert(model.to_string()) {
            return Err(format!(
                "provider interface '{}' formed models repeats '{model}'",
                provider.as_str()
            ));
        }
    }
    if models.is_empty() {
        return Err(format!(
            "provider interface '{}' formed models is empty",
            provider.as_str()
        ));
    }
    Ok(models)
}

fn resolved_provider_model_catalog(
    config: &octopus_interfaces::InterfaceDriverConfig,
    provider: &InterfaceId,
    formed_models: &BTreeSet<String>,
    configured_model: &str,
    configured_reasoning_effort: &str,
) -> Result<Vec<ResolvedProviderModel>, String> {
    let mut catalog = Vec::new();
    let mut seen = BTreeSet::new();
    if let Some(values) = config
        .private_config
        .get("model_catalog")
        .and_then(serde_json::Value::as_array)
    {
        for value in values {
            let value = value.as_object().ok_or_else(|| {
                format!(
                    "provider interface '{}' model_catalog contains a non-object entry",
                    provider.as_str()
                )
            })?;
            let model = value
                .get("model")
                .and_then(serde_json::Value::as_str)
                .filter(|model| {
                    !model.is_empty()
                        && model.trim() == *model
                        && !model.contains(['\r', '\n', '\0'])
                })
                .ok_or_else(|| {
                    format!(
                        "provider interface '{}' model_catalog entry has no valid model",
                        provider.as_str()
                    )
                })?;
            if !seen.insert(model.to_string()) {
                return Err(format!(
                    "provider interface '{}' model_catalog repeats '{model}'",
                    provider.as_str()
                ));
            }
            if !formed_models.contains(model) {
                continue;
            }
            let supported_reasoning_efforts = parse_model_reasoning_efforts(
                value.get("supported_reasoning_efforts"),
                provider,
                model,
            )?;
            let default_reasoning_effort = value
                .get("default_reasoning_effort")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if !default_reasoning_effort.is_empty()
                && !supported_reasoning_efforts.is_empty()
                && !supported_reasoning_efforts.contains(&default_reasoning_effort)
            {
                return Err(format!(
                    "provider interface '{}' model '{model}' default reasoning effort '{}' is outside its supported list",
                    provider.as_str(),
                    default_reasoning_effort
                ));
            }
            catalog.push(ResolvedProviderModel {
                model: model.to_string(),
                default_reasoning_effort,
                supported_reasoning_efforts,
            });
        }
    }
    if catalog.is_empty() && formed_models.contains(configured_model) {
        catalog.push(ResolvedProviderModel {
            model: configured_model.to_string(),
            default_reasoning_effort: configured_reasoning_effort.to_string(),
            supported_reasoning_efforts: config.supported_reasoning_efforts.clone(),
        });
    }
    if catalog.is_empty() {
        return Err(format!(
            "provider interface '{}' has no model catalog entry inside its formed models",
            provider.as_str()
        ));
    }
    Ok(catalog)
}

fn parse_model_reasoning_efforts(
    value: Option<&serde_json::Value>,
    provider: &InterfaceId,
    model: &str,
) -> Result<Vec<String>, String> {
    let Some(values) = value else {
        return Ok(Vec::new());
    };
    let values = values.as_array().ok_or_else(|| {
        format!(
            "provider interface '{}' model '{model}' supported_reasoning_efforts is not an array",
            provider.as_str()
        )
    })?;
    let mut efforts = BTreeSet::new();
    for value in values {
        let effort = value.as_str().or_else(|| {
            value
                .as_object()
                .and_then(|entry| entry.get("effort"))
                .and_then(serde_json::Value::as_str)
        });
        let effort = effort
            .filter(|effort| {
                !effort.is_empty()
                    && effort.trim() == *effort
                    && !effort.contains(['\r', '\n', '\0'])
            })
            .ok_or_else(|| {
                format!(
                    "provider interface '{}' model '{model}' has a malformed reasoning effort",
                    provider.as_str()
                )
            })?;
        if !efforts.insert(effort.to_string()) {
            return Err(format!(
                "provider interface '{}' model '{model}' repeats reasoning effort '{effort}'",
                provider.as_str()
            ));
        }
    }
    Ok(efforts.into_iter().collect())
}

fn provider_client_operation_request_labels(
    private_config: &serde_json::Map<String, serde_json::Value>,
    provider: &str,
) -> Result<Vec<String>, String> {
    let mut labels = BTreeSet::new();
    for key in ["provider_operations", "model_catalog_operation"] {
        let Some(value) = private_config.get(key) else {
            continue;
        };
        collect_provider_operation_request_labels(value, provider, key, &mut labels)?;
    }
    Ok(labels.into_iter().collect())
}

fn collect_provider_operation_request_labels(
    value: &serde_json::Value,
    provider: &str,
    field: &str,
    labels: &mut BTreeSet<String>,
) -> Result<(), String> {
    let Some(object) = value.as_object() else {
        return Err(format!(
            "provider interface '{provider}' private_config.{field} is not an object"
        ));
    };
    if let Some(request_label) = object.get("request_label") {
        let request_label = request_label
            .as_str()
            .filter(|label| {
                !label.is_empty()
                    && label.trim() == *label
                    && !label.contains(['\r', '\n', '\0'])
            })
            .ok_or_else(|| {
                format!(
                    "provider interface '{provider}' private_config.{field}.request_label is malformed"
                )
            })?;
        labels.insert(request_label.to_string());
        return Ok(());
    }
    for (name, child) in object {
        if child.is_object() {
            collect_provider_operation_request_labels(
                child,
                provider,
                &format!("{field}.{name}"),
                labels,
            )?;
        }
    }
    Ok(())
}

fn preserve_selected_file_writer(
    config: &ClientConfig,
    suffix: &str,
    topology: &FormedCapabilityTopology,
    tools: &[SelectedToolSeed],
    kinds: &[String],
    identity: &mut PlannedIdentity,
) -> Result<bool, String> {
    let room = topology.tool_bindings.get(&InterfaceId::new(&config.authoring.room_interface))
        .ok_or("selected worker/EE has no declared authoring room")?;
    if !room.tools.iter().any(|tool| tool.tool.as_str() == config.authoring.tool) {
        return Err("selected worker/EE has no declared authoring tool".into());
    }
    let exposed = tools.iter().any(|tool| tool.tool == config.authoring.tool)
        || room.tools.iter().any(|tool| tool.tool.as_str() == config.authoring.tool
            && kinds.iter().any(|kind| kind == tool.worker_environment_kind.as_str()));
    if exposed {
        identity.capability_tools.insert(config.authoring.tool.clone(), format!("{}__{suffix}", config.authoring.tool));
        identity.capability_doors.entry(config.authoring.room_interface.clone())
            .or_insert_with(|| format!("{}_{suffix}", config.authoring.room_interface));
    }
    Ok(exposed)
}

impl PlannedIdentity {
    fn owns_client(&self) -> bool {
        self.native_communication.as_ref().is_none_or(|routes| routes.client_template_interface_id.is_some())
    }
}

fn plan_identity(
    config: &ClientConfig,
    chat_id: &str,
    provider: &ResolvedProvider,
    topology: &FormedCapabilityTopology,
    selection: &RunSelection,
) -> Result<PlannedIdentity, String> {
    let provider_choice = provider.interface.id.as_str().to_string();
    let model_interface_id = match selection.provider_runtime_interface.as_deref() {
        Some(existing) => safe_segment(existing, "provider_runtime_interface")?,
        None => format!("{provider_choice}_{chat_id}"),
    };
    let worker_choice = safe_segment(&selection.worker_choice_interface, "run.worker")?;
    let authoring_source = safe_segment(
        &config.authoring.source_interface,
        "authoring.source_interface",
    )?;
    let authoring_room =
        safe_segment(&config.authoring.room_interface, "authoring.room_interface")?;
    let authoring_environment_id = format!("{authoring_room}_authoring_{chat_id}");
    let mut capability_doors = BTreeMap::new();
    let mut capability_tools = BTreeMap::new();
    let authoring_tool_id = format!("{}__authoring_{chat_id}", config.authoring.tool);
    for (source_interface, binding) in &topology.tool_bindings {
        let mut requires_capability_door = false;
        for tool in &binding.tools {
            let private_id = if source_interface.as_str() == config.authoring.room_interface
                && tool.tool.as_str() == config.authoring.tool
            {
                authoring_tool_id.clone()
            } else {
                requires_capability_door = true;
                format!("{}__{chat_id}", tool.tool.as_str())
            };
            if capability_tools
                .insert(tool.tool.as_str().to_string(), private_id)
                .is_some()
            {
                return Err(format!(
                    "formed tool '{}' appears under more than one exact owner",
                    tool.tool.as_str()
                ));
            }
        }
        if !requires_capability_door {
            continue;
        }
        let interface_segment = safe_segment(source_interface.as_str(), "capability interface")?;
        capability_doors.insert(
            source_interface.as_str().to_string(),
            format!("{interface_segment}_{chat_id}"),
        );
    }
    Ok(PlannedIdentity {
        voice: None,
        native_communication: None,
        chat_id: chat_id.to_string(),
        run_id: selection
            .run_id
            .clone()
            .unwrap_or_else(|| format!("{}{}", config.run.run_prefix, chat_id)),
        ee_id: selection.ee_id.clone(),
        chat_interface_id: selection
            .chat_interface_id
            .clone()
            .unwrap_or_else(|| format!("{}{}", config.run.interface_prefix, chat_id)),
        model_interface_id,
        worker_interface_id: selection
            .worker_runtime_interface
            .clone()
            .unwrap_or_else(|| format!("{worker_choice}_{chat_id}")),
        authoring_source_id: format!("{authoring_source}_{chat_id}"),
        authoring_environment_id,
        surface_role: selection.surface_role.clone(),
        provider_choice_interface: provider.interface.id.as_str().to_string(),
        worker_choice_interface: selection.worker_choice_interface.clone(),
        capability_doors,
        capability_tools,
        capability_tool_sources: BTreeMap::new(),
        authoring_tool_id,
        code_mode_owners: BTreeMap::new(),
    })
}

fn plan_chat_interface(
    interface_root: &Path,
    config: &ClientConfig,
    identity: &PlannedIdentity,
    requested_authoring_profile: Option<&OctopusOsAuthoringProfile>,
    replace_authoring_profile: bool,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    let source_interface = load_interface_descriptor(interface_root, &config.interface)?;
    let descriptor =
        sibling_descriptor_bytes(&source_interface, &identity.chat_interface_id, None, None)?;
    push_json_write(
        interface_root
            .join(&identity.chat_interface_id)
            .join("interface.json"),
        descriptor,
        format!("interface:{}", identity.chat_interface_id),
        writes,
    )?;
    plan_interface_life(
        interface_root,
        &config.interface,
        &identity.chat_interface_id,
        writes,
    )?;
    let activation = serde_json::json!({
        "schema_version": 1,
        "interface": identity.chat_interface_id,
        "activations": [
            {
                "activation_driver_ref": "app_server_transport",
                "config": {
                    "listen_env": config.run.listen_env,
                    "bind_interface_socket_when_env_unset": true,
                },
            }
        ],
    });
    push_json_write(
        interface_root
            .join(&identity.chat_interface_id)
            .join("activation.json"),
        pretty_json(&activation)?,
        format!("activation:{}", identity.chat_interface_id),
        writes,
    )?;
    plan_octopus_os_sidecar(
        interface_root,
        &identity.chat_interface_id,
        identity,
        "client",
        true,
        requested_authoring_profile,
        replace_authoring_profile,
        writes,
    )
}

fn plan_capability_doors(
    registry_root: &Path,
    interface_root: &Path,
    config: &ClientConfig,
    topology: &FormedCapabilityTopology,
    identity: &PlannedIdentity,
    requested_host_slots: &BTreeMap<String, BTreeMap<String, PathBuf>>,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    let mut consumed_host_slots = BTreeSet::new();
    for (source_interface_id, binding) in &topology.tool_bindings {
        let Some(door_id) = identity.capability_doors.get(source_interface_id.as_str()) else {
            if binding.tools.iter().all(|tool| {
                identity
                    .capability_tools
                    .get(tool.tool.as_str())
                    .is_some_and(|private| private == &identity.authoring_tool_id)
            }) {
                continue;
            }
            return Err(format!(
                "capability interface '{}' has no planned exact door",
                source_interface_id
            ));
        };
        let mut driver_config = if identity.native_communication.is_some() {
            binding.interface.interface_driver_config.clone()
        } else {
            preserve_exact_door_host_slots(interface_root, &binding.interface, door_id)?
        };
        if let Some(overrides) = requested_host_slots.get(source_interface_id.as_str()) {
            consumed_host_slots.insert(source_interface_id.as_str().to_string());
            let config = driver_config.get_or_insert_with(Default::default);
            for (slot, path) in overrides {
                let slot = safe_segment(slot, "door_host_slots slot")?;
                require_absolute("door_host_slots path", path)?;
                config.host_slots.insert(
                    slot,
                    octopus_interfaces::HostSlotOverride { path: path.clone() },
                );
            }
        }
        if let Some(owner) = identity.code_mode_owners.get(source_interface_id.as_str()) {
            driver_config.get_or_insert_default().private_config.insert("run_authoring_profile".into(), serde_json::json!({
                "file_write_tool":owner.authoring_tool,"file_write_label":config.artifact_labels.tool_call,
                "file_write_result_label":format!("{}.result",owner.authoring_tool),"result_timeout_ms":owner.recipe.result_timeout_ms
            }));
        }
        if let Some(approval) = planned_browser_upload_approval(binding, config, identity)? {
            let room = driver_config.get_or_insert_default().private_config.get_mut("browser_room")
                .and_then(serde_json::Value::as_object_mut).ok_or("planned browser upload approval lost its declared room configuration")?;
            room.insert("upload_approval".into(), serde_json::to_value(approval).map_err(|e| e.to_string())?);
        } else if let Some(room) = driver_config.as_mut().and_then(|driver| driver.private_config.get_mut("browser_room")).and_then(serde_json::Value::as_object_mut) {
            room.remove("upload_approval");
        }
        if let Some(history) = planned_browser_history_access(binding, config, identity)? {
            let room = driver_config.get_or_insert_default().private_config.get_mut("browser_room")
                .and_then(serde_json::Value::as_object_mut).ok_or("planned history lost its declared room configuration")?;
            room.insert("history_access".into(), serde_json::to_value(history).map_err(|e| e.to_string())?);
        } else if let Some(room) = driver_config.as_mut().and_then(|driver| driver.private_config.get_mut("browser_room")).and_then(serde_json::Value::as_object_mut) {
            room.remove("history_access");
        }
        if let Some(completion) = planned_browser_wait_completion(registry_root, binding, config, identity)? {
            let room = driver_config.get_or_insert_default().private_config.get_mut("browser_room")
                .and_then(serde_json::Value::as_object_mut).ok_or("planned browser wait lost its declared room configuration")?;
            room.insert("wait_completion".into(), serde_json::to_value(completion).map_err(|e| e.to_string())?);
        } else if let Some(room) = driver_config.as_mut().and_then(|driver| driver.private_config.get_mut("browser_room")).and_then(serde_json::Value::as_object_mut) {
            room.remove("wait_completion");
        }
        let descriptor =
            sibling_descriptor_bytes(&binding.interface, door_id, None, driver_config.as_ref())?;
        push_json_write(
            interface_root.join(door_id).join("interface.json"),
            descriptor,
            format!("interface:{door_id}"),
            writes,
        )?;
        plan_interface_life(
            interface_root,
            source_interface_id.as_str(),
            door_id,
            writes,
        )?;
        plan_octopus_os_sidecar(
            interface_root,
            door_id,
            identity,
            "worker_environment_door",
            false,
            None,
            false,
            writes,
        )?;
        plan_capability_sidecars(
            interface_root,
            source_interface_id.as_str(),
            door_id,
            binding,
            config,
            identity,
            writes,
        )?;
    }
    let unused = requested_host_slots
        .keys()
        .filter(|interface| !consumed_host_slots.contains(*interface))
        .cloned()
        .collect::<Vec<_>>();
    if !unused.is_empty() {
        return Err(format!(
            "door_host_slots names interfaces absent from the formed exact door topology: {}",
            unused.join(", ")
        ));
    }
    Ok(())
}

fn plan_authoring_interfaces(
    interface_root: &Path,
    config: &ClientConfig,
    topology: &FormedCapabilityTopology,
    identity: &PlannedIdentity,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    let source_interface =
        load_interface_descriptor(interface_root, &config.authoring.source_interface)?;
    let descriptor =
        sibling_descriptor_bytes(&source_interface, &identity.authoring_source_id, None, None)?;
    push_json_write(
        interface_root
            .join(&identity.authoring_source_id)
            .join("interface.json"),
        descriptor,
        format!("interface:{}", identity.authoring_source_id),
        writes,
    )?;
    plan_interface_life(
        interface_root,
        &config.authoring.source_interface,
        &identity.authoring_source_id,
        writes,
    )?;
    plan_octopus_os_sidecar(
        interface_root,
        &identity.authoring_source_id,
        identity,
        "authoring_source",
        false,
        None,
        false,
        writes,
    )?;
    let activation = serde_json::json!({
        "schema_version": 1,
        "interface": identity.authoring_source_id,
        "activations": [
            {
                "activation_driver_ref": "app_server_transport",
                "config": {
                    "listen_env": format!(
                        "OCTOPUS_INTERFACE_AUTHORING_{}_LISTEN",
                        identity.chat_id.to_uppercase()
                    ),
                    "bind_interface_socket_when_env_unset": true,
                },
            }
        ],
    });
    push_json_write(
        interface_root
            .join(&identity.authoring_source_id)
            .join("activation.json"),
        pretty_json(&activation)?,
        format!("activation:{}", identity.authoring_source_id),
        writes,
    )?;

    let room_interface =
        load_interface_descriptor(interface_root, &config.authoring.room_interface)?;
    let descriptor = sibling_descriptor_bytes(
        &room_interface,
        &identity.authoring_environment_id,
        None,
        None,
    )?;
    push_json_write(
        interface_root
            .join(&identity.authoring_environment_id)
            .join("interface.json"),
        descriptor,
        format!("interface:{}", identity.authoring_environment_id),
        writes,
    )?;
    plan_interface_life(
        interface_root,
        &config.authoring.room_interface,
        &identity.authoring_environment_id,
        writes,
    )?;
    let binding = topology
        .tool_bindings
        .get(&InterfaceId::new(&config.authoring.room_interface))
        .ok_or_else(|| {
            format!(
                "authoring room interface '{}' is absent from formed capabilities",
                config.authoring.room_interface
            )
        })?;
    plan_capability_sidecars(
        interface_root,
        &config.authoring.room_interface,
        &identity.authoring_environment_id,
        binding,
        config,
        identity,
        writes,
    )?;
    plan_octopus_os_sidecar(
        interface_root,
        &identity.authoring_environment_id,
        identity,
        "authoring_environment",
        false,
        None,
        false,
        writes,
    )
}

fn plan_capability_sidecars(
    interface_root: &Path,
    source_interface_id: &str,
    target_interface_id: &str,
    binding: &octopus_world_terminal_client::ToolCapabilityBinding,
    config: &ClientConfig,
    identity: &PlannedIdentity,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    for sidecar in &binding.sidecars {
        let source = interface_root
            .join(source_interface_id)
            .join(&sidecar.file_name);
        let content = std::fs::read_to_string(&source).map_err(|error| {
            format!(
                "capability interface '{}' is missing required sidecar '{}': {error}",
                source_interface_id, sidecar.file_name
            )
        })?;
        let content = if sidecar.file_name == "desktop.json" {
            plan_desktop_app_approval(&content, binding, config, identity)?
        } else if sidecar.file_name == "code_mode.json" {
            plan_code_mode_references(&content, source_interface_id, config, identity)?
        } else {
            content
        };
        let content = if content.ends_with('\n') {
            content
        } else {
            format!("{content}\n")
        };
        push_text_write(
            interface_root
                .join(target_interface_id)
                .join(&sidecar.file_name),
            content,
            format!("sidecar:{target_interface_id}:{}", sidecar.file_name),
            writes,
        )?;
    }
    Ok(())
}

#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeModeTaskRecipe {
    prelude_artifacts: Vec<String>,
    resume_tool: String,
    wait_tool: String,
    authoring_room: String,
    authoring_tool: String,
    notification_interface: String,
    result_timeout_ms: u64,
    #[serde(default)]
    tool_descriptions: BTreeMap<String, String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeModePreludeSource {
    id: String,
    schema_version: u32,
    module_source: String,
    export_factory: String,
    export_name: String,
    #[serde(deserialize_with = "unique_prelude_tool_roles")]
    tool_roles: BTreeMap<String, CodeModePreludeTool>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeModePreludeTool {
    tool: String,
    interface: String,
}

fn unique_prelude_tool_roles<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, CodeModePreludeTool>, D::Error> {
    struct Roles;
    impl<'de> serde::de::Visitor<'de> for Roles {
        type Value = BTreeMap<String, CodeModePreludeTool>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("distinct declared prelude tool roles")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
            let mut roles = BTreeMap::new();
            while let Some((role, tool)) = access.next_entry::<String, CodeModePreludeTool>()? {
                if roles.insert(role, tool).is_some() {
                    return Err(serde::de::Error::custom("prelude source repeats a tool role"));
                }
            }
            Ok(roles)
        }
    }
    deserializer.deserialize_map(Roles)
}

struct PlannedCodeModeSource {
    source: octopus_ledger::ArtifactRef,
    snapshot: octopus_ledger::ArtifactRef,
    source_sha256: String,
    content: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RetainedPreludeSource {
    source: octopus_ledger::ArtifactRef,
    snapshot: octopus_ledger::ArtifactRef,
    source_sha256: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RetainedPreludeSnapshot {
    id: String,
    run: String,
    source: octopus_ledger::ArtifactRef,
    source_sha256: String,
    source_content: String,
}

fn code_mode_source_bytes(
    registry: &Path, identity: &PlannedIdentity, owner: &str, source_id: &str,
    existing: Option<&serde_json::Value>,
) -> Result<Vec<u8>, String> {
    let state = match existing {
        Some(run) => run.get("interface_state").and_then(serde_json::Value::as_object)
            .ok_or("retained run has no interface state")?.get(owner),
        None => None,
    };
    let Some(state) = state else {
        return octopus_interfaces::read_artifact_carrier_file_bounded(
            &registry.join("artifact").join(source_id).join("artifact.json"), 1024 * 1024,
        ).map_err(|error| error.to_string());
    };
    let sources: Vec<RetainedPreludeSource> = serde_json::from_value(
        state.get("prelude_sources").cloned().ok_or("retained Code Mode owner has no source evidence")?,
    ).map_err(|error| error.to_string())?;
    let matches = sources.iter().filter(|source| source.source.id.as_str() == source_id).collect::<Vec<_>>();
    let [source] = matches.as_slice() else { return Err("retained Code Mode source is absent or duplicated".into()); };
    let snapshot_id = format!("{source_id}__source__{}", identity.chat_id);
    if source.snapshot.id.as_str() != snapshot_id { return Err("retained Code Mode snapshot has another owner".into()); }
    let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(
        &registry.join("artifact").join(&snapshot_id).join("artifact.json"), 8 * 1024 * 1024,
    ).map_err(|error| error.to_string())?;
    let body: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let snapshot: RetainedPreludeSnapshot = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let content = snapshot.source_content.into_bytes();
    let source_body: serde_json::Value = serde_json::from_slice(&content).map_err(|error| error.to_string())?;
    if snapshot.id != snapshot_id || snapshot.run != identity.run_id || snapshot.source != source.source
        || source.snapshot.content != octopus_ledger::ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&body))
        || source.source.content != octopus_ledger::ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&source_body))
        || snapshot.source_sha256 != source.source_sha256 || source.source_sha256 != format!("{:x}", Sha256::digest(&content))
        || content.len() > 1024 * 1024
    { return Err("retained Code Mode source differs from its exact snapshot evidence".into()); }
    Ok(content)
}

struct PlannedCodeModeOwner {
    recipe: CodeModeTaskRecipe,
    prelude_sources: Vec<PlannedCodeModeSource>,
    authoring_environment: String,
    authoring_tool: String,
    prelude: octopus_ledger::ArtifactRef,
    prelude_body: serde_json::Value,
    exports: Vec<String>,
    topology: octopus_ledger::ArtifactRef,
    topology_body: serde_json::Value,
    sidecar: serde_json::Value,
    nested_doors: BTreeSet<String>,
    nested_tools: BTreeSet<String>,
}

fn plan_code_mode_owners(
    registry_root: &Path,
    config: &ClientConfig,
    capabilities: &FormedCapabilityTopology,
    identity: &mut PlannedIdentity,
    context: &[ArtifactBindingSeed],
    existing: Option<&serde_json::Value>,
) -> Result<(), String> {
    for (source, binding) in &capabilities.tool_bindings {
        let Some(recipe) = binding
            .interface
            .interface_driver_config
            .as_ref()
            .and_then(|c| c.private_config.get("operator_chat_persistent"))
        else {
            continue;
        };
        let recipe: CodeModeTaskRecipe = serde_json::from_value(recipe.clone())
            .map_err(|e| format!("invalid declared code-mode task recipe: {e}"))?;
        let mut declared_sources = BTreeSet::new();
        let mut selected_sources = Vec::new();
        if recipe.prelude_artifacts.is_empty() {
            return Err("persistent task recipe must declare at least one prelude artifact".into());
        }
        for id in &recipe.prelude_artifacts {
            let id = safe_segment(id, "prelude artifact")?;
            if !declared_sources.insert(id.clone()) {
                return Err("persistent task recipe repeats a prelude artifact".into());
            }
            match context.iter().filter(|binding| binding.id == id).count() {
                0 => {},
                1 => selected_sources.push(id),
                _ => return Err("persistent prelude requires exactly one explicit source binding".into()),
            }
        }
        if selected_sources.is_empty() {
            continue;
        }
        if recipe.result_timeout_ms == 0
            || recipe.authoring_room != config.authoring.room_interface
            || recipe.authoring_tool != config.authoring.tool
            || recipe.notification_interface != config.interface
        {
            return Err("persistent task recipe must use its exact selected client and existing authoring capability".into());
        }
        let owner = identity
            .capability_doors
            .get(source.as_str())
            .ok_or("persistent task recipe has no formed owner door")?;
        let exact_role = |base: &str| -> Result<String, String> {
            if !binding.tools.iter().any(|t| t.tool.as_str() == base) {
                return Err(format!(
                    "persistent owner role '{base}' was not selected under '{source}'"
                ));
            }
            identity
                .capability_tools
                .get(base)
                .cloned()
                .ok_or_else(|| format!("missing exact selected role '{base}'"))
        };
        let resume = exact_role(&recipe.resume_tool)?;
        let wait = exact_role(&recipe.wait_tool)?;
        for (tool, description) in &recipe.tool_descriptions {
            exact_role(tool)?;
            if description.trim().is_empty() {
                return Err("selected persistent tool description must be non-empty".into());
            }
        }
        if resume == wait {
            return Err("persistent resume and wait roles must be distinct".into());
        }
        let sidecar_path = registry_root
            .join("interface")
            .join(source.as_str())
            .join("code_mode.json");
        let mut sidecar: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&sidecar_path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if sidecar
            .pointer("/context/mode")
            .and_then(serde_json::Value::as_str)
            != Some("isolated")
            || sidecar.get("continuation").is_some()
            || sidecar.get("handoff").is_some()
            || sidecar
                .get("routes")
                .and_then(serde_json::Value::as_array)
                .is_none_or(|r| !r.is_empty())
        {
            return Err("task prelude recipe requires an explicit isolated source without concrete task refs".into());
        }
        let max_source_bytes = sidecar.get("max_source_bytes").and_then(serde_json::Value::as_u64)
            .ok_or("selected persistent source requires its declared source byte bound")?;
        let javascript_identifier = |v: &str| {
            !v.is_empty()
                && v.bytes().enumerate().all(|(i, c)| {
                    c.is_ascii_alphabetic()
                        || c == b'_'
                        || c == b'$'
                        || (i > 0 && c.is_ascii_digit())
                })
        };
        let mut prelude_sources = Vec::new();
        let mut modules = String::new();
        let mut source_bytes = 0u64;
        let mut exports = Vec::new();
        let mut declarations = BTreeSet::new();
        let mut role_names = BTreeSet::new();
        let mut nested_tools = BTreeSet::new();
        let mut javascript_names = BTreeSet::new();
        let mut routes = Vec::new();
        let mut owner_routes = Vec::new();
        let edge = |name: String, from: &str, to: &str| {
            serde_json::json!({"name":name,"binding":{
                "inbound":AuthoredInterfaceBinding::all(InterfaceId::new(from)),
                "outbound":[AuthoredInterfaceBinding::all(InterfaceId::new(to))]
            }})
        };
        for prelude_source_id in &selected_sources {
            let bytes = code_mode_source_bytes(registry_root, identity, owner, prelude_source_id, existing)?;
            source_bytes = source_bytes.checked_add(bytes.len() as u64)
                .filter(|total| *total <= max_source_bytes)
                .ok_or("selected prelude sources exceed the room source byte bound")?;
            let body: serde_json::Value = serde_json::from_slice(&bytes)
                .map_err(|e| format!("invalid declared prelude artifact: {e}"))?;
            let prelude: CodeModePreludeSource = serde_json::from_slice(&bytes)
                .map_err(|e| format!("invalid declared prelude artifact: {e}"))?;
            if prelude.id != *prelude_source_id || prelude.schema_version != 1
                || !javascript_identifier(&prelude.export_factory)
                || !javascript_identifier(&prelude.export_name)
                || prelude.module_source.trim().is_empty()
                || prelude.tool_roles.is_empty()
                || !declarations.insert(prelude.export_factory.clone())
                || !declarations.insert(prelude.export_name.clone())
            {
                return Err("selected preludes require distinct nonempty module/factory/export declarations".into());
            }
            let mut roles = serde_json::Map::new();
            for (role, declared) in &prelude.tool_roles {
                if role.trim().is_empty() || !role_names.insert(role.clone()) {
                    return Err("selected preludes repeat a tool role or declare an empty role".into());
                }
                let Some(nested) = capabilities
                    .tool_bindings
                    .get(&InterfaceId::new(&declared.interface))
                else {
                    continue;
                };
                if !nested
                    .tools
                    .iter()
                    .any(|t| t.tool.as_str() == declared.tool)
                {
                    continue;
                }
                let tool = identity
                    .capability_tools
                    .get(&declared.tool)
                    .ok_or("prelude tool has no exact planned identity")?;
                let door = identity
                    .capability_doors
                    .get(&declared.interface)
                    .ok_or("prelude tool has no exact planned door")?;
                if door == owner {
                    return Err("prelude cannot recursively route into its owner".into());
                }
                let normalized: String = tool
                    .chars()
                    .enumerate()
                    .map(|(i, c)| {
                        if c == '_'
                            || c == '$'
                            || c.is_ascii_alphabetic()
                            || (i > 0 && c.is_ascii_digit())
                        {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect();
                if normalized.is_empty()
                    || !nested_tools.insert(tool.clone())
                    || !javascript_names.insert(normalized.clone())
                {
                    return Err("persistent prelude repeats a tool or produces colliding exact JavaScript names".into());
                }
                roles.insert(
                    role.clone(),
                    serde_json::json!({"tool":tool,"name":normalized}),
                );
                let call_name = format!("nested_call_{tool}");
                let result_name = format!("nested_result_{door}");
                routes.push(serde_json::json!({"tool":tool,"interface":door,"checkpoint":call_name,"result_checkpoint":result_name}));
                let app_approval = planned_nested_approval(registry_root, config, identity, nested, tool)?;
                owner_routes.push(serde_json::json!({"tool":tool,"call":edge(call_name,owner,door),"result":edge(result_name,door,owner),"app_approval":app_approval,"process_descriptor":planned_tool_descriptor(registry_root,config,identity,&InterfaceId::new(&declared.interface),&declared.tool)?}));
            }
            if roles.is_empty() {
                return Err("selected prelude has no formed declared tool roles".into());
            }
            modules.push_str(&prelude.module_source);
            modules.push_str(&format!("\nexport const {} = {}({});\n", prelude.export_name, prelude.export_factory, serde_json::Value::Object(roles)));
            exports.push(prelude.export_name);
            let content = octopus_ledger::ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&body));
            let source = octopus_ledger::ArtifactRef { id:octopus_descriptors::ArtifactId::new(prelude_source_id.clone()), content };
            let snapshot_id = format!("{prelude_source_id}__source__{}",identity.chat_id);
            let source_sha256 = {
                    use sha2::{Digest, Sha256};
                    format!("{:x}", Sha256::digest(&bytes))
            };
            let snapshot = serde_json::json!({"id":snapshot_id,"run":identity.run_id,"source":source,"source_sha256":source_sha256,
                "source_content":String::from_utf8(bytes).map_err(|_| "prelude source is not UTF8")?});
            prelude_sources.push(PlannedCodeModeSource {
                source,
                snapshot: octopus_ledger::ArtifactRef { id:octopus_descriptors::ArtifactId::new(snapshot_id), content:octopus_ledger::ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&snapshot)) },
                source_sha256,
                content: pretty_json(&snapshot)?,
            });
        }
        let nested_doors = owner_routes.iter().filter_map(|route| route.pointer("/call/binding/outbound/0/interface").and_then(serde_json::Value::as_str)).map(str::to_owned).collect::<BTreeSet<_>>();
        for (nested_source, nested) in &capabilities.tool_bindings {
            let Some(door) = identity.capability_doors.get(nested_source.as_str()).filter(|door| nested_doors.contains(*door)) else { continue; };
            let internal_resumes = planned_nested_resumes(registry_root, nested)?;
            for selected in &nested.tools {
                let tool = identity.capability_tools.get(selected.tool.as_str()).ok_or("selected nested tool has no exact planned identity")?;
                if internal_resumes.iter().any(|base| identity.capability_tools.get(base) == Some(tool)) || !nested_tools.insert(tool.clone()) { continue; }
                let approval = planned_nested_approval(registry_root, config, identity, nested, tool)?;
                let call_name = format!("nested_call_{tool}");
                let result_name = format!("nested_result_{door}");
                routes.push(serde_json::json!({"tool":tool,"interface":door,"checkpoint":call_name,"result_checkpoint":result_name}));
                owner_routes.push(serde_json::json!({"tool":tool,"call":edge(call_name,owner,door),"result":edge(result_name,door,owner),"app_approval":approval,"process_descriptor":planned_tool_descriptor(registry_root,config,identity,nested_source,selected.tool.as_str())?}));
            }
        }
        partition_nested_returns(&mut owner_routes, &mut routes)?;
        for previous in identity.code_mode_owners.values() {
            if !previous.nested_doors.is_disjoint(&nested_doors) { return Err("selected persistent owners cannot share one nested door".into()); }
        }
        let prelude_id = format!("artifact-code-mode-prelude__{}__{}", source, identity.chat_id);
        let prelude_body = serde_json::json!({"id":prelude_id,"run":identity.run_id,"owner_interface":owner,"schema_version":1,"module_source":modules,"exports":exports});
        let artifact = |id: String, body: &serde_json::Value| octopus_ledger::ArtifactRef {
            id: octopus_descriptors::ArtifactId::new(id),
            content: octopus_ledger::ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(
                body,
            )),
        };
        let prelude_ref = artifact(
            prelude_id,
            &prelude_body,
        );
        let authoring_environment = format!("{}_authoring_{}", recipe.authoring_room, owner);
        let authoring_tool = format!("{}__authoring_{}", recipe.authoring_tool, owner);
        if authoring_environment == identity.authoring_environment_id
            || authoring_tool == identity.authoring_tool_id
        {
            return Err(
                "persistent owner cannot share the provider's live authoring identity".into(),
            );
        }
        let topology_id = format!("artifact-code-mode-topology__{}__{}", source, identity.chat_id);
        let topology_body = serde_json::json!({"id":topology_id,"schema_version":1,"run":identity.run_id,"worker":identity.worker_interface_id,"owner_interface":owner,"prelude_artifact":prelude_ref,"file_write_tool":authoring_tool,
            "authoring_result":edge("authoring_result".into(),&authoring_environment,owner),
            "model_output":edge("model_output".into(),owner,&identity.model_interface_id),
            "notification":edge("notification".into(),owner,&identity.chat_interface_id),
            "receipt":edge("receipt".into(),owner,owner),"readiness":edge("readiness".into(),owner,owner),"routes":owner_routes,
            "coauthor":{"owner":identity.model_interface_id,"source":identity.authoring_source_id,"door":identity.authoring_environment_id,"tool":identity.authoring_tool_id,"call_label":config.artifact_labels.tool_call,"result_label":format!("{}.result",identity.authoring_tool_id)}});
        let topology_ref = artifact(
            topology_id,
            &topology_body,
        );
        if pretty_json(&prelude_body)?.len() as u64 > max_source_bytes {
            return Err("composed prelude exceeds the selected room source byte bound".into());
        }
        sidecar["context"] =
            serde_json::json!({"mode":"persistent","prelude_artifact":prelude_ref});
        sidecar["continuation"] = serde_json::json!({"mode":"admitted_artifact","resume_tool":resume,"notification_interface":identity.chat_interface_id});
        sidecar["handoff"] = serde_json::json!({"topology_artifact":topology_ref,"wait_tool":wait});
        sidecar["routes"] = serde_json::json!(routes);
        identity.code_mode_owners.insert(
            source.to_string(),
            PlannedCodeModeOwner {
                recipe,
                prelude_sources,
                authoring_environment,
                authoring_tool,
                prelude: prelude_ref,
                prelude_body,
                exports,
                topology: topology_ref,
                topology_body,
                sidecar,
                nested_doors,
                nested_tools,
            },
        );
    }
    Ok(())
}

fn plan_independent_code_mode_authoring(
    registry_root: &Path,
    config: &ClientConfig,
    capabilities: &FormedCapabilityTopology,
    identity: &PlannedIdentity,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    let interface_root = registry_root.join("interface");
    for owner in identity.code_mode_owners.values() {
        let binding = capabilities
            .tool_bindings
            .get(&InterfaceId::new(&owner.recipe.authoring_room))
            .ok_or("owner authoring door is not in the selected formed capabilities")?;
        if !binding
            .tools
            .iter()
            .any(|t| t.tool.as_str() == owner.recipe.authoring_tool)
        {
            return Err("owner authoring tool is not in the selected formed capabilities".into());
        }
        push_json_write(
            interface_root
                .join(&owner.authoring_environment)
                .join("interface.json"),
            sibling_descriptor_bytes(&binding.interface, &owner.authoring_environment, None, None)?,
            format!("interface:{}", owner.authoring_environment),
            writes,
        )?;
        plan_interface_life(
            &interface_root,
            &owner.recipe.authoring_room,
            &owner.authoring_environment,
            writes,
        )?;
        plan_capability_sidecars(
            &interface_root,
            &owner.recipe.authoring_room,
            &owner.authoring_environment,
            binding,
            config,
            identity,
            writes,
        )?;
        plan_octopus_os_sidecar(
            &interface_root,
            &owner.authoring_environment,
            identity,
            "worker_environment_door",
            false,
            None,
            false,
            writes,
        )?;
        let mut descriptor = load_tool_descriptor_from_dir(
            &registry_root.join("tool"),
            &owner.recipe.authoring_tool,
        )
        .map_err(|e| e.to_string())?
        .ok_or("owner file tool descriptor is absent")?;
        if descriptor.interface.as_str() != owner.recipe.authoring_room {
            return Err("owner file tool belongs to another door".into());
        }
        descriptor.id = ToolDescriptorId::new(&owner.authoring_tool);
        descriptor.interface = InterfaceId::new(&owner.authoring_environment);
        narrow_run_authoring_tool_authority(&mut descriptor)?;
        let content = pretty_json(&serde_json::to_value(&descriptor).map_err(|e| e.to_string())?)?;
        parse_strict::<ToolDescriptor>(content.as_bytes()).map_err(|e| e.to_string())?;
        push_json_write(
            registry_root
                .join("tool")
                .join(&owner.authoring_tool)
                .join("tool.json"),
            content,
            format!("tool:{}", owner.authoring_tool),
            writes,
        )?;
    }
    Ok(())
}

fn write_code_mode_owner_artifacts(
    registry_root: &Path,
    identity: &PlannedIdentity,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    for owner in identity.code_mode_owners.values() {
        for source in &owner.prelude_sources {
            let path = registry_root.join("artifact").join(source.snapshot.id.as_str()).join("artifact.json");
            match std::fs::read(&path) {
                Ok(bytes) if bytes == source.content.as_bytes() => {},
                Ok(_) => return Err("task-owned prelude source snapshot differs from its preserved bytes".into()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => writes.push(PlannedClientWrite {
                    path, content:source.content.clone(), tag:format!("artifact:{}",source.snapshot.id), expected_before_hash:None,
                }),
                Err(error) => return Err(format!("prelude source snapshot cannot be read: {error}")),
            }
        }
        for (artifact, body) in [
            (&owner.prelude, &owner.prelude_body),
            (&owner.topology, &owner.topology_body),
        ] {
            push_json_write(
                registry_root
                    .join("artifact")
                    .join(artifact.id.as_str())
                    .join("artifact.json"),
                pretty_json(body)?,
                format!("artifact:{}", artifact.id),
                writes,
            )?;
        }
    }
    Ok(())
}

#[derive(serde::Deserialize, serde::Serialize)]
struct CodeModeSidecarReferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    continuation: Option<CodeModeContinuationReferences>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    handoff: Option<CodeModeHandoffReferences>,
    #[serde(default)]
    routes: Vec<CodeModeRouteReferences>,
    #[serde(flatten)]
    settings: serde_json::Map<String, serde_json::Value>,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct CodeModeContinuationReferences {
    mode: String,
    resume_tool: String,
    notification_interface: Option<String>,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct CodeModeHandoffReferences {
    topology_artifact: octopus_ledger::ArtifactRef,
    wait_tool: String,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct CodeModeRouteReferences {
    tool: String,
    interface: String,
    checkpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result_checkpoint: Option<String>,
}

fn plan_code_mode_references(
    content: &str,
    source_interface: &str,
    config: &ClientConfig,
    identity: &PlannedIdentity,
) -> Result<String, String> {
    if let Some(owner) = identity.code_mode_owners.get(source_interface) {
        return pretty_json(&owner.sidecar);
    }
    let mut sidecar: CodeModeSidecarReferences = serde_json::from_str(content)
        .map_err(|e| format!("code mode sidecar is malformed: {e}"))?;
    let tool = |base: &str| {
        identity
            .capability_tools
            .get(base)
            .cloned()
            .ok_or_else(|| format!("code mode role tool '{base}' was not selected and formed"))
    };
    let door = |base: &str| {
        identity
            .capability_doors
            .get(base)
            .cloned()
            .ok_or_else(|| format!("code mode route door '{base}' was not selected and formed"))
    };
    for route in &mut sidecar.routes {
        if route.interface == source_interface {
            return Err("code mode cannot recursively route into its own door".into());
        }
        route.tool = tool(&route.tool)?;
        route.interface = door(&route.interface)?;
    }
    if let Some(continuation) = &mut sidecar.continuation {
        if continuation.mode != "admitted_artifact" {
            return Err("code mode continuation mode is unknown".into());
        }
        continuation.resume_tool = tool(&continuation.resume_tool)?;
        if let Some(interface) = &mut continuation.notification_interface {
            if interface == &config.interface {
                *interface = identity.chat_interface_id.clone();
            } else {
                *interface = door(interface)?;
            }
        }
    }
    if let Some(handoff) = &mut sidecar.handoff {
        handoff.wait_tool = tool(&handoff.wait_tool)?;
        if sidecar.continuation.is_none() {
            return Err("code mode handoff has no admitted continuation".into());
        }
    }
    if sidecar
        .settings
        .get("context")
        .and_then(|c| c.get("mode"))
        .and_then(serde_json::Value::as_str)
        == Some("persistent")
        && sidecar.handoff.is_none()
    {
        return Err("selected persistent code mode requires an explicit independently owned topology artifact and authoring contract".into());
    }
    pretty_json(&serde_json::to_value(sidecar).map_err(|e| e.to_string())?)
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct DesktopAppApprovalReferences {
    operator_interface: InterfaceId,
    resume_tool: ToolDescriptorId,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct BrowserUploadApprovalReferences {
    request_tools: Vec<ToolDescriptorId>,
    operator_interface: InterfaceId,
    resume_tool: ToolDescriptorId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    permission_policy: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct BrowserConsentAuthorityReferences {
    authority: octopus_ledger::ArtifactRef,
    client_interface: InterfaceId,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct BrowserHistoryAccessReferences {
    request_tools: Vec<ToolDescriptorId>,
    operator_interface: InterfaceId,
    resume_tool: ToolDescriptorId,
    consent_authority: BrowserConsentAuthorityReferences,
}

fn planned_browser_history_access(
    binding: &octopus_world_terminal_client::ToolCapabilityBinding,
    config: &ClientConfig,
    _identity: &PlannedIdentity,
) -> Result<Option<BrowserHistoryAccessReferences>, String> {
    let Some(room) = binding.interface.interface_driver_config.as_ref()
        .and_then(|driver| driver.private_config.get("browser_room")) else { return Ok(None); };
    let room = room.as_object().ok_or("browser_room must be its declared object")?;
    let Some(value) = room.get("history_access") else { return Ok(None); };
    let Some(mut history) = serde_json::from_value::<Option<BrowserHistoryAccessReferences>>(value.clone())
        .map_err(|e| format!("browser_room.history_access has an invalid exact reference: {e}"))? else { return Ok(None); };
    let exact = |value: &str| !value.is_empty() && value.trim() == value && !value.contains('\0');
    let mut declared = BTreeSet::new();
    if !exact(history.resume_tool.as_str()) || !exact(history.operator_interface.as_str())
        || !exact(history.consent_authority.authority.id.as_str()) || !exact(history.consent_authority.client_interface.as_str())
        || history.request_tools.is_empty() || history.request_tools.iter().any(|tool| !exact(tool.as_str())
            || tool == &history.resume_tool || !declared.insert(tool.clone())) {
        return Err("history access must declare distinct exact request/resume and consent references".into());
    }
    history.request_tools.retain(|tool| binding.tools.iter().any(|selected| selected.tool.as_str() == tool.as_str()));
    let resume_selected = binding.tools.iter().any(|tool| tool.tool.as_str() == history.resume_tool.as_str());
    if history.request_tools.is_empty() {
        if resume_selected { return Err("selected history resume has no selected request tool".into()); }
        return Ok(None);
    }
    if !resume_selected || history.consent_authority.client_interface.as_str() != config.interface {
        return Err("selected history request requires its declared companion and exact source task client".into());
    }
    Err("selected Home history consent requires explicit migration to the existing permission mechanism".into())
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct BrowserWaitCompletionReferences {
    request_tools: Vec<ToolDescriptorId>,
    completion_tool: ToolDescriptorId,
}

fn planned_browser_wait_completion(
    registry_root: &Path,
    binding: &octopus_world_terminal_client::ToolCapabilityBinding,
    config: &ClientConfig,
    identity: &PlannedIdentity,
) -> Result<Option<BrowserWaitCompletionReferences>, String> {
    let Some(room) = binding.interface.interface_driver_config.as_ref()
        .and_then(|driver| driver.private_config.get("browser_room")) else { return Ok(None); };
    let room = room.as_object().ok_or("browser_room must be its declared object")?;
    let Some(value) = room.get("wait_completion") else { return Ok(None); };
    let mut pair: BrowserWaitCompletionReferences = serde_json::from_value(value.clone())
        .map_err(|e| format!("browser_room.wait_completion is malformed: {e}"))?;
    let mut unique = BTreeSet::new();
    if pair.completion_tool.as_str().trim().is_empty() || pair.completion_tool.as_str().trim() != pair.completion_tool.as_str()
        || pair.request_tools.is_empty() || pair.request_tools.iter().any(|tool| tool.as_str().trim().is_empty()
            || tool.as_str().trim() != tool.as_str() || tool == &pair.completion_tool || !unique.insert(tool.clone())) {
        return Err("browser wait pair must declare distinct exact request and completion tools".into());
    }
    pair.request_tools.retain(|tool| binding.tools.iter().any(|selected| selected.tool.as_str() == tool.as_str()));
    let completion_selected = binding.tools.iter().any(|selected| selected.tool.as_str() == pair.completion_tool.as_str());
    if pair.request_tools.is_empty() {
        if completion_selected { return Err("selected browser completion has no selected request tool".into()); }
        return Ok(None);
    }
    if !completion_selected { return Err("selected browser wait requires its declared completion tool".into()); }
    let completion = planned_tool_descriptor(registry_root, config, identity, &binding.interface.id, pair.completion_tool.as_str())?;
    if completion.handler_config.get("operation").and_then(serde_json::Value::as_str) != Some("wait_complete") {
        return Err("declared browser completion descriptor has another operation".into());
    }
    for tool in &mut pair.request_tools {
        let request = planned_tool_descriptor(registry_root, config, identity, &binding.interface.id, tool.as_str())?;
        if request.handler_config.get("operation").and_then(serde_json::Value::as_str) != Some("wait")
            || request.process == completion.process || request.supported_worker_environment_kinds != completion.supported_worker_environment_kinds {
            return Err("browser wait pair requires independently declared processes in the same kind".into());
        }
        *tool = request.id;
    }
    pair.completion_tool = completion.id;
    Ok(Some(pair))
}

fn planned_browser_upload_approval(
    binding: &octopus_world_terminal_client::ToolCapabilityBinding,
    config: &ClientConfig,
    identity: &PlannedIdentity,
) -> Result<Option<BrowserUploadApprovalReferences>, String> {
    let Some(room) = binding.interface.interface_driver_config.as_ref()
        .and_then(|driver| driver.private_config.get("browser_room")) else { return Ok(None); };
    let room = room.as_object().ok_or("browser_room must be its declared object")?;
    let Some(value) = room.get("upload_approval") else { return Ok(None); };
    let Some(mut approval) = serde_json::from_value::<Option<BrowserUploadApprovalReferences>>(value.clone())
        .map_err(|e| format!("browser_room.upload_approval has an invalid exact reference: {e}"))? else { return Ok(None); };
    let mut declared = BTreeSet::new();
    let exact = |value: &str| !value.is_empty() && value.trim() == value && !value.contains('\0');
    if !exact(approval.operator_interface.as_str()) || !exact(approval.resume_tool.as_str())
        || approval.request_tools.is_empty() || approval.request_tools.iter().any(|tool| !exact(tool.as_str()) || tool == &approval.resume_tool || !declared.insert(tool.clone())) {
        return Err("browser upload request_tools must declare distinct exact public request tools".into());
    }
    approval.request_tools.retain(|tool| binding.tools.iter().any(|selected| selected.tool.as_str() == tool.as_str()));
    if approval.request_tools.is_empty() {
        if binding.tools.iter().any(|selected| selected.tool.as_str() == approval.resume_tool.as_str()) {
            return Err("selected upload resume has no selected request tool".into());
        }
        return Ok(None);
    }
    if !binding.tools.iter().any(|tool| tool.tool.as_str() == approval.resume_tool.as_str()) {
        return Err("selected browser upload request requires its exact client and paired resume tool".into());
    }
    if approval.operator_interface.as_str() != config.interface {
        return Err("upload approval names another source client".into());
    }
    approval.operator_interface = InterfaceId::new(&identity.chat_interface_id);
    for tool in &mut approval.request_tools {
        *tool = ToolDescriptorId::new(identity.capability_tools.get(tool.as_str()).ok_or("browser upload request has no exact planned tool identity")?);
    }
    approval.resume_tool = ToolDescriptorId::new(identity.capability_tools.get(approval.resume_tool.as_str())
        .ok_or("browser upload resume has no exact planned tool identity")?);
    Ok(Some(approval))
}

fn plan_desktop_app_approval(
    content: &str,
    binding: &octopus_world_terminal_client::ToolCapabilityBinding,
    config: &ClientConfig,
    identity: &PlannedIdentity,
) -> Result<String, String> {
    let mut desktop: serde_json::Map<String, serde_json::Value> = serde_json::from_str(content)
        .map_err(|error| {
            format!(
                "desktop.json for '{}' is malformed: {error}",
                binding.interface.id
            )
        })?;
    let Some(value) = desktop.get("app_approval") else {
        return Ok(content.to_string());
    };
    let mut references: DesktopAppApprovalReferences = serde_json::from_value(value.clone())
        .map_err(|error| format!("desktop.json.app_approval is malformed: {error}"))?;
    if references.operator_interface.as_str() != config.interface {
        return Err(format!(
            "desktop.json.app_approval.operator_interface '{}' is not the planned source client '{}'",
            references.operator_interface, config.interface
        ));
    }
    if !binding
        .tools
        .iter()
        .any(|tool| tool.tool.as_str() == references.resume_tool.as_str())
    {
        return Err(format!(
            "desktop.json.app_approval.resume_tool '{}' is not formed under exact door '{}'",
            references.resume_tool, binding.interface.id
        ));
    }
    let exact_tool = identity
        .capability_tools
        .get(references.resume_tool.as_str())
        .ok_or_else(|| {
            format!(
                "application resume tool '{}' has no planned exact identity",
                references.resume_tool
            )
        })?;
    references.operator_interface = InterfaceId::new(&identity.chat_interface_id);
    references.resume_tool = ToolDescriptorId::new(exact_tool);
    desktop.insert(
        "app_approval".to_string(),
        serde_json::to_value(references)
            .map_err(|error| format!("application approval references did not encode: {error}"))?,
    );
    pretty_json(&serde_json::Value::Object(desktop))
}

fn planned_nested_resumes(
    registry_root: &Path,
    binding: &octopus_world_terminal_client::ToolCapabilityBinding,
) -> Result<BTreeSet<String>, String> {
    if binding.sidecars.iter().any(|sidecar| sidecar.file_name == "desktop.json") {
        let path = registry_root.join("interface").join(binding.interface.id.as_str()).join("desktop.json");
        let desktop: serde_json::Value = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        return desktop.get("app_approval").map(|value| {
            serde_json::from_value::<DesktopAppApprovalReferences>(value.clone())
                .map(|approval| BTreeSet::from([approval.resume_tool.to_string()])).map_err(|e| e.to_string())
        }).transpose().map(Option::unwrap_or_default);
    }
    let Some(room) = binding.interface.interface_driver_config.as_ref()
        .and_then(|driver| driver.private_config.get("browser_room")) else { return Ok(BTreeSet::new()); };
    let room = room.as_object().ok_or("browser_room must be its declared object")?;
    let mut resumes = BTreeSet::new();
    if let Some(value) = room.get("upload_approval") {
        if let Some(approval) = serde_json::from_value::<Option<BrowserUploadApprovalReferences>>(value.clone()).map_err(|e| e.to_string())? {
            resumes.insert(approval.resume_tool.to_string());
        }
    }
    if let Some(value) = room.get("history_access") {
        if let Some(history) = serde_json::from_value::<Option<BrowserHistoryAccessReferences>>(value.clone()).map_err(|e| e.to_string())? {
            if !resumes.insert(history.resume_tool.to_string()) {
                return Err("distinct browser approval roles declare the same private resume".into());
            }
        }
    }
    Ok(resumes)
}

fn planned_nested_approval(
    registry_root: &Path,
    config: &ClientConfig,
    identity: &PlannedIdentity,
    binding: &octopus_world_terminal_client::ToolCapabilityBinding,
    exact_tool: &str,
) -> Result<Option<serde_json::Value>, String> {
    let mut choices = Vec::new();
    if binding.sidecars.iter().any(|sidecar| sidecar.file_name == "desktop.json") {
        let path = registry_root.join("interface").join(binding.interface.id.as_str()).join("desktop.json");
        let desktop: serde_json::Value = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let Some(value) = desktop.get("app_approval") else { return Ok(None); };
        let approval: DesktopAppApprovalReferences = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        if approval.operator_interface.as_str() != config.interface {
            return Err("nested GUI approval is not bound to the selected exact client".into());
        }
        let resume = identity.capability_tools.get(approval.resume_tool.as_str()).ok_or("nested GUI resume is not selected")?.clone();
        choices.push((resume, InterfaceId::new(&identity.chat_interface_id)));
    } else {
        if let Some(approval) = planned_browser_upload_approval(binding, config, identity)? {
            if approval.request_tools.iter().any(|tool| tool.as_str() == exact_tool) {
                choices.push((approval.resume_tool.to_string(), approval.operator_interface));
            }
        }
        if let Some(history) = planned_browser_history_access(binding, config, identity)? {
            if history.request_tools.iter().any(|tool| tool.as_str() == exact_tool) {
                choices.push((history.resume_tool.to_string(), history.operator_interface));
            }
        }
    }
    if choices.len() > 1 { return Err("selected request matches more than one declared approval role".into()); }
    let Some((resume, operator)) = choices.pop() else { return Ok(None); };
    let base_resumes = planned_nested_resumes(registry_root, binding)?;
    let base = base_resumes.iter().find(|base| identity.capability_tools.get(*base) == Some(&resume))
        .ok_or("nested approval has no exact declared source resume identity")?;
    if !binding.tools.iter().any(|tool| tool.tool.as_str() == base) {
        return Err("nested approval resume belongs to another selected door".into());
    }
    let descriptor = planned_tool_descriptor(registry_root, config, identity, &binding.interface.id, base)?;
    let result = serde_json::json!({"operator_interface":operator,"resume_tool":resume,"resume_descriptor":descriptor});
    Ok(Some(result))
}

fn nested_process_owner(descriptor: &ToolDescriptor) -> Result<serde_json::Value, String> {
    let [kind] = descriptor.supported_worker_environment_kinds.as_slice() else {
        return Err("selected nested tool must have one declared process kind".into());
    };
    Ok(serde_json::json!({"interface":descriptor.interface,"kind":kind,"process":descriptor.process}))
}

fn partition_nested_returns(
    owner_routes: &mut [serde_json::Value],
    routes: &mut [serde_json::Value],
) -> Result<(), String> {
    let mut groups: BTreeMap<String, (serde_json::Value, BTreeSet<String>)> = BTreeMap::new();
    for route in owner_routes.iter_mut() {
        let descriptor: ToolDescriptor = serde_json::from_value(route["process_descriptor"].clone()).map_err(|e| e.to_string())?;
        let owner = nested_process_owner(&descriptor)?;
        let name = format!("nested_result_{}", octopus_descriptors::DescriptorHash::of_canonical_json(&owner).to_hex());
        let group = groups.entry(name.clone()).or_insert_with(|| (owner.clone(), BTreeSet::new()));
        if group.0 != owner { return Err("nested process owner digest collision".into()); }
        group.1.insert(format!("{}.result", descriptor.id));
        if let Some(approval) = route.get("app_approval").filter(|value| !value.is_null()) {
            let resume: ToolDescriptor = serde_json::from_value(approval["resume_descriptor"].clone()).map_err(|e| e.to_string())?;
            if nested_process_owner(&resume)? != owner || approval["resume_tool"].as_str() != Some(resume.id.as_str()) {
                return Err("nested approval resume does not belong to its original process owner".into());
            }
            group.1.insert(format!("{}.result", resume.id));
        }
        route["result"]["name"] = serde_json::json!(name);
        let configured = routes.iter_mut().find(|configured| configured["tool"] == route["tool"])
            .ok_or("nested process route has no exact configured tool")?;
        configured["result_checkpoint"] = route["result"]["name"].clone();
    }
    let mut disjoint = BTreeSet::new();
    for (owner, names) in groups.values() {
        let door = owner["interface"].as_str().ok_or("nested owner interface is absent")?;
        if names.is_empty() || names.iter().any(|name| !disjoint.insert((door, name))) {
            return Err("nested process result names overlap or are empty".into());
        }
    }
    for route in owner_routes {
        let name = route["result"]["name"].as_str().ok_or("nested result name is absent")?;
        let names = &groups.get(name).ok_or("nested return lost its owner group")?.1;
        let policy = serde_json::json!({"mode":"allow","names":names,"namespaces":[]});
        route["result"]["binding"]["inbound"]["config"]["artifact_policy"] = policy.clone();
        route["result"]["binding"]["outbound"][0]["config"]["artifact_policy"] = policy;
    }
    Ok(())
}

fn planned_tool_descriptor(
    registry_root: &Path,
    config: &ClientConfig,
    identity: &PlannedIdentity,
    source_interface_id: &InterfaceId,
    base_tool_id: &str,
) -> Result<ToolDescriptor, String> {
    let tool_root = registry_root.join("tool");
    let private_tool_id = identity.capability_tools.get(base_tool_id).ok_or_else(|| {
        format!("formed tool '{base_tool_id}' has no planned exact identity")
    })?;
    let continuation_writer = base_tool_id == config.authoring.tool
        && source_interface_id.as_str() == config.authoring.room_interface
        && private_tool_id == &identity.authoring_tool_id;
    let exact_door = if continuation_writer {
        &identity.authoring_environment_id
    } else {
        identity
            .capability_doors
            .get(source_interface_id.as_str())
            .ok_or_else(|| {
                format!(
                    "formed tool '{base_tool_id}' owner '{}' has no exact door",
                    source_interface_id.as_str()
                )
            })?
    };
    let mut descriptor = if let Some(descriptor) = identity.capability_tool_sources.get(base_tool_id) {
        descriptor.clone()
    } else { load_tool_descriptor_from_dir(&tool_root, base_tool_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("formed tool descriptor '{base_tool_id}' is absent"))? };
    if descriptor.interface != *source_interface_id {
        return Err(format!(
            "formed tool '{base_tool_id}' belongs to '{}' instead of '{}'",
            descriptor.interface.as_str(),
            source_interface_id.as_str()
        ));
    }
    descriptor.id = ToolDescriptorId::new(private_tool_id);
    descriptor.interface = InterfaceId::new(exact_door);
    if let Some(owner) = identity.code_mode_owners.get(source_interface_id.as_str()) {
        if let Some(description) = owner.recipe.tool_descriptions.get(base_tool_id) {
            let exports = owner.exports.iter().map(|name| format!("prelude.{name}")).collect::<Vec<_>>().join(", ");
            descriptor.description = format!("{description} This task binds exactly these persistent exports: {exports}. Inspect an export's documentation() method when it is present for its source-owned API contract.");
        }
    }
    if continuation_writer {
        narrow_run_authoring_tool_authority(&mut descriptor)?;
    }
    let bytes = serde_json::to_vec(&descriptor).map_err(|e| e.to_string())?;
    parse_strict::<ToolDescriptor>(&bytes).map_err(|e| e.to_string())
}

fn plan_private_tools(
    registry_root: &Path,
    config: &ClientConfig,
    topology: &FormedCapabilityTopology,
    identity: &PlannedIdentity,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    let tool_root = registry_root.join("tool");
    let mut descriptors = Vec::new();
    for (source_interface_id, binding) in &topology.tool_bindings {
        for formed_tool in &binding.tools {
            let base_tool_id = formed_tool.tool.as_str();
            let descriptor = planned_tool_descriptor(registry_root, config, identity, source_interface_id, base_tool_id)?;
            descriptors.push(descriptor);
        }
    }
    if identity.capability_tools.get(&config.authoring.tool) != Some(&identity.authoring_tool_id) {
        let mut writer = load_tool_descriptor_from_dir(&tool_root, &config.authoring.tool)
            .map_err(|error| error.to_string())?.ok_or("continuation writer is absent")?;
        if writer.interface.as_str() != config.authoring.room_interface {
            return Err("continuation writer belongs to another room".into());
        }
        writer.id = ToolDescriptorId::new(&identity.authoring_tool_id);
        writer.interface = InterfaceId::new(&identity.authoring_environment_id);
        narrow_run_authoring_tool_authority(&mut writer)?;
        descriptors.push(writer);
    }
    for descriptor in descriptors {
            let private_tool_id = descriptor.id.as_str();
            let content = pretty_json(
                &serde_json::to_value(&descriptor)
                    .map_err(|error| format!("private tool did not encode: {error}"))?,
            )?;
            parse_strict::<ToolDescriptor>(content.as_bytes()).map_err(|error| {
                format!("private tool '{private_tool_id}' failed validation: {error}")
            })?;
            push_json_write(
                tool_root.join(private_tool_id).join("tool.json"),
                content,
                format!("tool:{private_tool_id}"),
                writes,
            )?;
    }
    Ok(())
}

fn narrow_run_authoring_tool_authority(descriptor: &mut ToolDescriptor) -> Result<(), String> {
    let authority = &mut descriptor.process.authority;
    if authority.write_argument_paths.is_empty()
        || !authority.write_root_slots.is_empty()
        || !authority.write_paths.is_empty()
        || !authority.write_env_paths.is_empty()
        || !authority.optional_write_env_paths.is_empty()
        || !authority.write_env_subpaths.is_empty()
    {
        return Err(format!(
            "run-authoring tool '{}' must expose only argument-scoped writes",
            descriptor.id
        ));
    }
    let run_records_root = BTreeSet::from(["run_records_root".to_string()]);
    for path in &mut authority.write_argument_paths {
        path.within_root_slots = run_records_root.clone();
    }
    Ok(())
}

fn plan_worker_pin(
    interface_root: &Path,
    config: &ClientConfig,
    worker_choice_interface: &str,
    topology: &FormedCapabilityTopology,
    identity: &PlannedIdentity,
    external_interfaces: &[String],
    current_policy: Option<&WorkerInterfacePolicy>,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<Vec<String>, String> {
    let source_interface = load_interface_descriptor(interface_root, current_policy.map(|policy|policy.id.as_str()).unwrap_or(worker_choice_interface))?;
    let descriptor =
        sibling_descriptor_bytes(&source_interface, &identity.worker_interface_id, None, None)?;
    push_json_write(
        interface_root
            .join(&identity.worker_interface_id)
            .join("interface.json"),
        descriptor,
        format!("interface:{}", identity.worker_interface_id),
        writes,
    )?;
    plan_octopus_os_sidecar(
        interface_root,
        &identity.worker_interface_id,
        identity,
        "worker",
        false,
        None,
        false,
        writes,
    )?;

    let policy_path = interface_root
        .join(current_policy.map(|policy|policy.id.as_str()).unwrap_or(worker_choice_interface))
        .join("policy.json");
    let policy_bytes = std::fs::read(&policy_path).map_err(|error| {
        format!(
            "worker pin '{}' has no policy ceiling at '{}': {error}",
            worker_choice_interface,
            policy_path.display()
        )
    })?;
    let mut policy_value: serde_json::Value =
        serde_json::from_slice(&policy_bytes).map_err(|error| {
            format!(
                "worker policy '{}' is malformed: {error}",
                policy_path.display()
            )
        })?;
    let policy_object = policy_value.as_object_mut().ok_or_else(|| {
        format!(
            "worker policy '{}' is not a JSON object",
            policy_path.display()
        )
    })?;
    policy_object.insert(
        "id".to_string(),
        serde_json::json!(identity.worker_interface_id),
    );
    if identity.native_communication.is_none() {
        preserve_exact_worker_role_artifacts(
            interface_root,
            &identity.worker_interface_id,
            policy_object,
        )?;
    }
    let ceiling = policy_object
        .get_mut("ceiling")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| "private worker policy ceiling is not an object".to_string())?;
    let mut exact_interfaces = exact_topology_interface_ids(identity, external_interfaces);
    exact_interfaces.extend(
        topology
            .worker_relays
            .keys()
            .map(|interface| interface.as_str().to_string()),
    );
    set_policy_axis(ceiling, "interfaces", exact_interfaces)?;
    set_policy_axis(
        ceiling,
        "tools",
        identity.capability_tools.values().cloned().chain(std::iter::once(identity.authoring_tool_id.clone())).chain(
            identity
                .code_mode_owners
                .values()
                .map(|owner| owner.authoring_tool.clone()),
        ).chain(identity.voice.iter().map(|voice| voice.writer.clone())),
    )?;
    set_policy_axis(
        ceiling,
        "wek",
        topology
            .granted_worker_environment_kinds
            .iter()
            .map(|kind| kind.as_str().to_string()),
    )?;
    let policy_content = pretty_json(&policy_value)?;
    let mut parsed: WorkerInterfacePolicy = serde_json::from_slice(policy_content.as_bytes())
        .map_err(|error| format!("private worker policy failed validation: {error}"))?;
    if parsed.id.as_str() != identity.worker_interface_id {
        return Err("private worker policy changed identity during encoding".to_string());
    }
    run_composition::remap_private_worker_policy(&mut parsed,&run_composition::private_interface_mapping(config,identity));
    if let Some(current) = current_policy {
        for id in &current.ceiling.interfaces { if !parsed.ceiling.interfaces.contains(id) { parsed.ceiling.interfaces.push(id.clone()); } }
        for id in &current.ceiling.tools { if !parsed.ceiling.tools.contains(id) { parsed.ceiling.tools.push(id.clone()); } }
        for id in &current.ceiling.wek { if !parsed.ceiling.wek.contains(id) { parsed.ceiling.wek.push(id.clone()); } }
        parsed.ceiling.interface_policy.extend(current.ceiling.interface_policy.clone());
    }
    let policy_content = pretty_json(&serde_json::to_value(&parsed).map_err(|e|e.to_string())?)?;
    push_json_write(
        interface_root
            .join(&identity.worker_interface_id)
            .join("policy.json"),
        policy_content,
        format!("policy:{}", identity.worker_interface_id),
        writes,
    )?;
    Ok(parsed
        .role_artifacts
        .iter()
        .map(|artifact| artifact.as_str().to_string())
        .collect())
}

fn formed_worker_role_artifacts<'a>(
    interface_root: &Path,
    current_worker_role_artifacts: &[String],
    target_workers: impl Iterator<Item = &'a InterfaceId>,
) -> Result<Vec<String>, String> {
    let mut role_artifacts = Vec::new();
    let mut seen = BTreeSet::new();
    for artifact in current_worker_role_artifacts {
        if artifact.trim().is_empty() {
            return Err("private worker policy contains an empty role artifact id".to_string());
        }
        if seen.insert(artifact.clone()) {
            role_artifacts.push(artifact.clone());
        }
    }

    let mut target_workers = target_workers.cloned().collect::<Vec<_>>();
    target_workers.sort();
    target_workers.dedup();
    for worker in target_workers {
        let policy_path = interface_root.join(worker.as_str()).join("policy.json");
        let policy_bytes = std::fs::read(&policy_path).map_err(|error| {
            format!(
                "formed worker relay '{}' has no policy dependency surface at '{}': {error}",
                worker.as_str(),
                policy_path.display()
            )
        })?;
        let policy =
            serde_json::from_slice::<WorkerInterfacePolicy>(&policy_bytes).map_err(|error| {
                format!(
                    "formed worker relay policy '{}' is malformed: {error}",
                    policy_path.display()
                )
            })?;
        if policy.id != worker {
            return Err(format!(
                "formed worker relay policy '{}' declares '{}' instead of exact interface '{}'",
                policy_path.display(),
                policy.id.as_str(),
                worker.as_str()
            ));
        }
        for artifact in policy.role_artifacts {
            let artifact = artifact.as_str().to_string();
            if seen.insert(artifact.clone()) {
                role_artifacts.push(artifact);
            }
        }
    }
    Ok(role_artifacts)
}

fn worker_role_dependency_interfaces<'a>(
    interface_root: &Path,
    formed_worker_relays: impl Iterator<Item = &'a InterfaceId>,
    connected_interfaces: &[String],
) -> Result<Vec<InterfaceId>, String> {
    let mut workers = formed_worker_relays.cloned().collect::<BTreeSet<_>>();
    for interface_id in connected_interfaces {
        let id = safe_segment(interface_id, "connected interface id")?;
        let path = interface_root.join(&id).join("interface.json");
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "connected interface descriptor '{}' cannot be read: {error}",
                    path.display()
                ));
            }
        };
        let descriptor = parse_strict::<InterfaceDescriptor>(&bytes).map_err(|error| {
            format!(
                "connected interface descriptor '{}' is malformed: {error}",
                path.display()
            )
        })?;
        if descriptor.id.as_str() != id {
            return Err(format!(
                "connected interface descriptor '{}' declares mismatched id '{}'",
                path.display(),
                descriptor.id
            ));
        }
        if interface_is_worker_pin(&descriptor) {
            workers.insert(descriptor.id);
        }
    }
    Ok(workers.into_iter().collect())
}

fn plan_provider_interface(
    interface_root: &Path,
    config: &ClientConfig,
    identity: &PlannedIdentity,
    provider: &ResolvedProvider,
    topology: &FormedCapabilityTopology,
    worker_role_artifacts: &[String],
    inline_context_ids: &[String],
    interface_private_state_root: Option<&Path>,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    let mut provider_interface = provider.interface.clone();
    let formed_policy = topology
        .interface_policies
        .get(&provider.interface.id)
        .ok_or_else(|| {
            format!(
                "provider interface '{}' has no exact formed interface policy",
                provider.interface.id.as_str()
            )
        })?;
    provider_interface.interface_policy.values = formed_policy.context.clone();
    let mut driver_config = provider
        .interface
        .interface_driver_config
        .clone()
        .unwrap_or_default();
    if driver_config.private_config.contains_key("instruction_artifacts") {
        driver_config.private_config.insert("worker_instruction_artifacts".into(), serde_json::json!(worker_role_artifacts));
    }
    collection_context::configure_inline_selection(&mut driver_config.private_config, inline_context_ids)?;
    if let Some(names) = driver_config.private_config.get_mut("model_tool_names") {
        let source_names: BTreeMap<String, String> = serde_json::from_value(names.clone())
            .map_err(|error| format!("model_tool_names is malformed: {error}"))?;
        let mut exact_names = BTreeMap::new();
        for (source, name) in &source_names {
            exact_names.insert(identity.capability_tools.get(source).unwrap_or(source).clone(), name.clone());
        }
        for (source, exact) in &identity.capability_tools {
            exact_names.entry(exact.clone()).or_insert_with(|| source.replace(['.', '-'], "_"));
        }
        *names = serde_json::json!(exact_names);
    }
    driver_config
        .private_config
        .insert("model".to_string(), serde_json::json!(provider.model));
    if !provider.reasoning_effort.is_empty() {
        driver_config.private_config.insert(
            "reasoning_effort".to_string(),
            serde_json::json!(provider.reasoning_effort),
        );
    } else {
        driver_config.private_config.remove("reasoning_effort");
    }
    if let Some(tier) = &provider.service_tier {
        driver_config.private_config.insert("service_tier".into(), serde_json::json!(tier));
    } else {
        driver_config.private_config.remove("service_tier");
    }
    replace_provider_hidden_tools(
        &mut driver_config.private_config,
        provider.interface.id.as_str(),
        &identity.authoring_tool_id,
        &identity.capability_tools,
    )?;
    if let Some(value) = driver_config.private_config.get_mut("run_affordances") {
        let config = value.as_object_mut().ok_or("run_affordances must be an object")?;
        for key in ["file_write_tool", "file_read_tool", "artifact_send_tool"] {
            if let Some(value) = config.get_mut(key) {
                if value.is_null() { continue; }
                let original = value.as_str().filter(|id| !id.is_empty() && id.trim() == *id)
                    .ok_or_else(|| format!("run_affordances.{key} must name an exact tool"))?;
                if let Some(exact) = identity.capability_tools.get(original) {
                    *value = serde_json::json!(exact);
                }
            }
        }
    }
    for binding in topology.tool_bindings.values() {
        for resume in planned_nested_resumes(interface_root.parent().ok_or("provider interface root has no registry parent")?, binding)? {
            let Some(exact) = identity.capability_tools.get(&resume) else { continue; };
            let hidden = driver_config.private_config.get_mut("model_hidden_tools").and_then(serde_json::Value::as_array_mut)
                .ok_or("declared private continuation requires model_hidden_tools")?;
            if !hidden.iter().any(|entry| entry.as_str() == Some(exact.as_str())) { hidden.push(serde_json::json!(exact)); }
        }
    }
    if !identity.code_mode_owners.is_empty() {
        let hidden = driver_config
            .private_config
            .get_mut("model_hidden_tools")
            .and_then(serde_json::Value::as_array_mut)
            .ok_or("persistent owner requires declared model_hidden_tools")?;
        if !hidden
            .iter()
            .any(|v| v.as_str() == Some(identity.authoring_tool_id.as_str()))
        {
            return Err(
                "owner-private authoring capability must already be hidden by provider declaration"
                    .into(),
            );
        }
        for owner in identity.code_mode_owners.values() {
            for tool in std::iter::once(&owner.authoring_tool).chain(owner.nested_tools.iter()) {
                if !hidden.iter().any(|entry| entry.as_str() == Some(tool.as_str())) { hidden.push(serde_json::json!(tool)); }
            }
        }
    }
    driver_config.private_config.remove("run_authoring");
    driver_config.private_config.insert(
        "run_authoring_profile".to_string(),
        serde_json::json!({
            "file_write_tool": identity.authoring_tool_id,
            "file_write_label": config.artifact_labels.tool_call,
            "file_write_result_label": format!("{}.result", identity.authoring_tool_id),
            "result_timeout_ms": config.authoring.result_timeout_ms,
        }),
    );
    configure_provider_private_auth_store(&mut driver_config, provider.interface.id.as_str())?;
    let shares_provider_secret_store = configure_provider_secret_owner_root(
        &mut driver_config,
        provider.interface.id.as_str(),
        &identity.model_interface_id,
        interface_private_state_root,
    )?;
    let descriptor = sibling_descriptor_bytes(
        &provider_interface,
        &identity.model_interface_id,
        None,
        Some(&driver_config),
    )?;
    push_json_write(
        interface_root
            .join(&identity.model_interface_id)
            .join("interface.json"),
        descriptor,
        format!("interface:{}", identity.model_interface_id),
        writes,
    )?;
    let mut life_root_slots = Vec::new();
    if shares_provider_secret_store {
        life_root_slots.push(("provider_secret_owner_root", false));
    }
    if life_root_slots.is_empty() {
        plan_interface_life(
            interface_root,
            provider.interface.id.as_str(),
            &identity.model_interface_id,
            writes,
        )?;
    } else {
        plan_interface_life_with_root_slot_access(
            interface_root,
            provider.interface.id.as_str(),
            &identity.model_interface_id,
            &life_root_slots,
            writes,
        )?;
    }
    plan_octopus_os_sidecar(
        interface_root,
        &identity.model_interface_id,
        identity,
        "provider",
        false,
        None,
        false,
        writes,
    )
}

fn replace_provider_hidden_tools(
    private_config: &mut serde_json::Map<String, serde_json::Value>,
    provider_interface: &str,
    exact_authoring_tool: &str,
    planned_tools: &BTreeMap<String, String>,
) -> Result<(), String> {
    let base_authoring_tool = private_config
        .get("run_authoring_profile")
        .and_then(serde_json::Value::as_object)
        .and_then(|profile| profile.get("file_write_tool"))
        .and_then(serde_json::Value::as_str)
        .filter(|tool| !tool.is_empty() && tool.trim() == *tool)
        .ok_or_else(|| {
            format!(
                "provider interface '{provider_interface}' has no valid private_config.run_authoring_profile.file_write_tool"
            )
        })?
        .to_string();
    let hidden_tools = private_config
        .get_mut("model_hidden_tools")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| {
            format!(
                "provider interface '{provider_interface}' private_config.model_hidden_tools must be an array"
            )
        })?;
    let mut source_tools = BTreeSet::new();
    let mut exact_tools = BTreeSet::new();
    let mut replaced = false;
    for (index, value) in hidden_tools.iter_mut().enumerate() {
        let current = value
            .as_str()
            .filter(|tool| !tool.is_empty() && tool.trim() == *tool)
            .ok_or_else(|| {
                format!(
                    "provider interface '{provider_interface}' private_config.model_hidden_tools[{index}] must be a non-empty trimmed string"
                )
            })?;
        if !source_tools.insert(current.to_string()) {
            return Err(format!(
                "provider interface '{provider_interface}' private_config.model_hidden_tools repeats '{current}'"
            ));
        }
        let next = if current == base_authoring_tool {
            replaced = true;
            exact_authoring_tool
        } else {
            planned_tools
                .get(current)
                .map(String::as_str)
                .unwrap_or(current)
        };
        if !exact_tools.insert(next.to_string()) {
            return Err(format!(
                "provider interface '{provider_interface}' private_config.model_hidden_tools collapses to duplicate '{next}'"
            ));
        }
        *value = serde_json::json!(next);
    }
    if !replaced {
        return Err(format!(
            "provider interface '{provider_interface}' does not hide its run-authoring tool '{base_authoring_tool}' from the model"
        ));
    }
    Ok(())
}

fn configure_provider_private_auth_store(
    driver_config: &mut octopus_interfaces::InterfaceDriverConfig,
    source_interface_id: &str,
) -> Result<(), String> {
    let Some(residual) = driver_config.residual_ambient.as_mut() else {
        return Ok(());
    };
    let residual = residual
        .as_object_mut()
        .ok_or_else(|| "provider residual_ambient must be a JSON object".to_string())?;
    let Some(store) = residual.get_mut("private_auth_store") else {
        return Ok(());
    };
    let store = store
        .as_object_mut()
        .ok_or_else(|| "private_auth_store must be a JSON object".to_string())?;
    if store.keys().any(|key| key != "auth_file_env") {
        return Err(format!(
            "selected provider '{source_interface_id}' private_auth_store must declare only auth_file_env"
        ));
    }
    let auth_file_env = store
        .get("auth_file_env")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "private_auth_store.auth_file_env must be a string".to_string())?;
    if auth_file_env != "OCTOPUS_MACHINE_CODEX_AUTH_FILE" {
        return Err(format!(
            "selected provider '{source_interface_id}' private_auth_store.auth_file_env must be 'OCTOPUS_MACHINE_CODEX_AUTH_FILE'"
        ));
    }
    Ok(())
}

fn configure_provider_secret_owner_root(
    driver_config: &mut octopus_interfaces::InterfaceDriverConfig,
    source_interface_id: &str,
    target_interface_id: &str,
    interface_private_state_root: Option<&Path>,
) -> Result<bool, String> {
    let Some(opencodex) = driver_config.private_config.get("opencodex") else {
        return Ok(false);
    };
    let opencodex = opencodex.as_object().ok_or_else(|| {
        format!(
            "provider interface '{source_interface_id}' private_config.opencodex must be an object"
        )
    })?;
    let credential_owner = opencodex
        .get("credential_owner_interface")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty() && value.trim() == *value)
        .ok_or_else(|| {
            format!(
                "provider interface '{source_interface_id}' private_config.opencodex has no exact credential owner"
            )
        })?;
    if credential_owner != source_interface_id {
        return Err(format!(
            "provider interface '{source_interface_id}' must own its OpenCodex credential boundary"
        ));
    }
    let transport = driver_config
        .transport
        .as_ref()
        .ok_or_else(|| format!("provider interface '{source_interface_id}' has no transport"))?;
    if transport.auth_secret_ref.is_none() {
        return Ok(false);
    }
    if target_interface_id == credential_owner {
        return Ok(false);
    }
    let private_root = interface_private_state_root.ok_or_else(|| {
        format!(
            "provider interface '{source_interface_id}' declares a private credential but interface_private_state_root is absent"
        )
    })?;
    require_absolute("interface_private_state_root", private_root)?;
    driver_config.host_slots.insert(
        "provider_secret_owner_root".to_string(),
        octopus_interfaces::HostSlotOverride {
            path: private_root.join(credential_owner),
        },
    );
    Ok(true)
}

fn plan_octopus_os_sidecar(
    interface_root: &Path,
    interface_id: &str,
    identity: &PlannedIdentity,
    component: &str,
    include_choices: bool,
    requested_authoring_profile: Option<&OctopusOsAuthoringProfile>,
    replace_authoring_profile: bool,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    if !identity.owns_client() {
        return Ok(());
    }
    let preserved = if include_choices {
        preserve_octopus_os_client_configuration(interface_root, interface_id, identity)?
    } else {
        None
    };
    let preserved_profile = preserved.as_ref().and_then(|value| value.declaration.authoring_profile.clone());
    let authoring_profile = match (requested_authoring_profile, preserved_profile) {
        (Some(requested), Some(preserved))
            if requested != &preserved && !replace_authoring_profile =>
        {
            return Err(format!(
                "Octopus OS client sidecar '{interface_id}' conflicts with the requested authoring profile"
            ));
        }
        (Some(requested), _) => {
            validate_octopus_os_authoring_profile(requested, Some(identity))?;
            Some(requested.clone())
        }
        (None, preserved) => preserved,
    };
    let mut presentation = preserved.as_ref().and_then(|value| value.declaration.presentation.clone());
    if include_choices {
        if let Some(title) = identity.native_communication.as_ref().and_then(|value| value.client_title.as_ref()) {
            if presentation.as_ref().is_some_and(|value| value.name != *title) {
                return Err(format!("Octopus OS client sidecar '{interface_id}' has a different authored title"));
            }
            presentation = Some(OctopusOsPresentation { name: title.clone(), projects: Vec::new() });
        }
    }
    let sidecar = OctopusOsInterfaceSidecar {
        schema_version: 1,
        workspace: "octopus_os".to_string(),
        interface: interface_id.to_string(),
        owner_interface: identity.chat_interface_id.clone(),
        component: component.to_string(),
        surface_role: identity.surface_role.clone(),
        provider_choice_interface: include_choices
            .then(|| identity.provider_choice_interface.clone()),
        worker_choice_interface: include_choices.then(|| identity.worker_choice_interface.clone()),
        authoring_profile,
        task_authoring_profiles: preserved.as_ref().map(|value| value.declaration.task_authoring_profiles.clone()).unwrap_or_default(),
        presentation,
        voice_context: include_choices.then(|| identity.voice.as_ref().map(|voice| voice.context.clone())).flatten(),
    };
    let mut encoded = serde_json::to_value(sidecar)
        .map_err(|error| format!("Octopus OS sidecar did not encode: {error}"))?;
    if let Some(preserved) = preserved {
        let previous = serde_json::to_value(preserved.declaration)
            .map_err(|error| format!("validated Octopus OS sidecar did not encode: {error}"))?;
        preserve_unchanged_client_json(&preserved.authored, &previous, &mut encoded);
    }
    push_json_write(
        interface_root.join(interface_id).join("octopus_os.json"),
        pretty_json(&encoded)?,
        format!("octopus-os:{interface_id}"),
        writes,
    )
}

fn preserve_unchanged_client_json(
    authored: &serde_json::Value,
    previous: &serde_json::Value,
    next: &mut serde_json::Value,
) {
    if previous == next {
        *next = authored.clone();
        return;
    }
    let (Some(authored), Some(previous), Some(next)) =
        (authored.as_object(), previous.as_object(), next.as_object_mut())
    else {
        return;
    };
    for (key, value) in authored {
        match (previous.get(key), next.get_mut(key)) {
            (Some(previous), Some(next)) => preserve_unchanged_client_json(value, previous, next),
            (None, None) => { next.insert(key.clone(), value.clone()); }
            _ => {}
        }
    }
}

fn preserve_octopus_os_client_configuration(
    interface_root: &Path,
    interface_id: &str,
    identity: &PlannedIdentity,
) -> Result<Option<PreservedOctopusOsClientConfiguration>, String> {
    let path = interface_root.join(interface_id).join("octopus_os.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => {
            return Err(format!(
                "Octopus OS client sidecar '{}' cannot be read: {error}",
                path.display()
            ));
        }
    };
    let sidecar: OctopusOsInterfaceSidecar = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "Octopus OS client sidecar '{}' is malformed: {error}",
            path.display()
        )
    })?;
    if sidecar.schema_version != 1
        || sidecar.workspace != "octopus_os"
        || sidecar.interface != interface_id
        || sidecar.owner_interface != interface_id
        || sidecar.component != "client"
        || sidecar.surface_role != identity.surface_role
        || sidecar.provider_choice_interface.as_deref()
            != Some(identity.provider_choice_interface.as_str())
        || sidecar.worker_choice_interface.as_deref()
            != Some(identity.worker_choice_interface.as_str())
    {
        return Err(format!(
            "Octopus OS authoring profile '{}' conflicts with its live client identity",
            path.display()
        ));
    }
    if sidecar.authoring_profile.is_none()
        && (!sidecar.task_authoring_profiles.is_empty() || sidecar.presentation.is_some())
    {
        return Err(format!(
            "Octopus OS client configuration '{}' has task configuration without its own authoring profile",
            path.display()
        ));
    }
    if let Some(profile) = sidecar.authoring_profile.as_ref() {
        validate_octopus_os_authoring_profile(profile, Some(identity))?;
    }
    if !sidecar.task_authoring_profiles.is_empty() && identity.surface_role != "home" {
        return Err(format!(
            "Octopus OS task authoring profiles '{}' do not belong to Home",
            path.display()
        ));
    }
    let mut profile_keys = BTreeSet::new();
    for profile in &sidecar.task_authoring_profiles {
        validate_octopus_os_authoring_profile(profile, None)?;
        if !profile_keys.insert((
            profile.ee_id.as_str(),
            profile.worker_choice_interface.as_str(),
        )) {
            return Err(format!(
                "Octopus OS client configuration '{}' repeats a task authoring profile",
                path.display()
            ));
        }
    }
    if let Some(presentation) = sidecar.presentation.as_ref() {
        if identity.surface_role == "task"
            && (!presentation.projects.is_empty() || presentation.name.is_empty()
                || presentation.name.chars().any(char::is_control))
        {
            return Err(format!(
                "Octopus OS task presentation '{}' must contain a name and no project presentations",
                path.display()
            ));
        }
        if !presentation.name.is_empty()
            && (presentation.name.trim() != presentation.name
                || presentation.name.chars().count() > 80)
        {
            return Err(format!(
                "Octopus OS presentation '{}' contains an invalid name",
                path.display()
            ));
        }
        let mut ee_ids = BTreeSet::new();
        for project in &presentation.projects {
            safe_segment(&project.ee_id, "presentation.projects.ee_id")?;
            if project.name.trim().is_empty()
                || project.name.chars().count() > 180
                || project.icon.chars().count() > 64
                || project.color.chars().count() > 64
                || !ee_ids.insert(project.ee_id.as_str())
            {
                return Err(format!(
                    "Octopus OS presentation '{}' contains an invalid project",
                    path.display()
                ));
            }
        }
    }
    let authored = serde_json::from_slice(&bytes)
        .map_err(|error| format!("validated Octopus OS sidecar JSON did not decode: {error}"))?;
    Ok(Some(PreservedOctopusOsClientConfiguration { declaration: sidecar, authored }))
}

fn validate_octopus_os_authoring_profile(
    profile: &OctopusOsAuthoringProfile,
    identity: Option<&PlannedIdentity>,
) -> Result<(), String> {
    safe_segment(&profile.ee_id, "authoring_profile.ee_id")?;
    publication_selection::retain_publication_evidence(&[], &profile.publication_evidence)?;
    safe_segment(
        &profile.worker_choice_interface,
        "authoring_profile.worker_choice_interface",
    )?;
    if let Some(identity) = identity {
        if profile.ee_id != identity.ee_id
            || profile.worker_choice_interface != identity.worker_choice_interface
        {
            return Err(
                "Octopus OS authoring profile conflicts with its live client identity".to_string(),
            );
        }
    }
    for (interface, slots) in &profile.door_host_slots {
        safe_segment(interface, "authoring_profile door interface")?;
        if slots.is_empty() {
            return Err(format!(
                "Octopus OS authoring profile door '{interface}' has no host slots"
            ));
        }
        for (slot, root) in slots {
            safe_segment(slot, "authoring_profile root slot")?;
            require_absolute("authoring_profile root path", root)?;
        }
    }
    validate_external_route_shapes(
        &profile.external_inputs,
        &profile.external_relays,
        &profile.model_outputs,
    )?;
    let mut artifacts = BTreeSet::new();
    for binding in &profile.context_artifact_bindings {
        context_bindings::validate_artifact_id(&binding.id)?;
        let id = binding.id.clone();
        if !artifacts.insert(id.clone()) {
            return Err(format!(
                "Octopus OS authoring profile repeats context artifact binding '{id}'"
            ));
        }
    }
    octopus_provider_run_authoring::validate_payload_artifact_selection(&profile.payload_artifact_bindings)?;
    for reference in &profile.payload_artifact_bindings {
        if artifacts.contains(reference.id.as_str()) {
            return Err("authoring profile payload selection collides with context bindings".into());
        }
    }
    let mut kinds = BTreeSet::new();
    for kind in &profile.selected_worker_environment_kinds {
        let kind = safe_segment(kind, "authoring_profile worker environment kind")?;
        if !kinds.insert(kind.clone()) {
            return Err(format!(
                "Octopus OS authoring profile repeats worker environment kind '{kind}'"
            ));
        }
    }
    let mut tools = BTreeSet::new();
    for selection in &profile.selected_tools {
        let tool = safe_policy_axis_value(&selection.tool, "authoring_profile selected tool")?;
        safe_segment(
            &selection.interface,
            "authoring_profile selected tool interface",
        )?;
        safe_segment(
            &selection.worker_environment_kind,
            "authoring_profile selected tool worker environment kind",
        )?;
        if !tools.insert(tool.clone()) {
            return Err(format!(
                "Octopus OS authoring profile repeats selected tool '{tool}'"
            ));
        }
    }
    Ok(())
}

fn plan_interface_life(
    interface_root: &Path,
    source_interface_id: &str,
    target_interface_id: &str,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    plan_interface_life_between_roots(interface_root, interface_root, source_interface_id, target_interface_id, writes)
}

fn plan_interface_life_between_roots(
    source_interface_root: &Path,
    interface_root: &Path,
    source_interface_id: &str,
    target_interface_id: &str,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    let source = source_interface_root.join(source_interface_id).join("life.json");
    let bytes = std::fs::read(&source).map_err(|error| {
        format!(
            "interface '{}' has no executable life at '{}': {error}",
            source_interface_id,
            source.display()
        )
    })?;
    let mut life: SpawnLifeConfig = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "interface life '{}' is malformed: {error}",
            source.display()
        )
    })?;
    import_owner_executables(&mut life, source_interface_id)?;
    push_json_write(
        interface_root.join(target_interface_id).join("life.json"),
        pretty_json(
            &serde_json::to_value(life)
                .map_err(|error| format!("imported interface life did not encode: {error}"))?,
        )?,
        format!("life:{target_interface_id}"),
        writes,
    )
}

fn plan_interface_life_with_root_slot_access(
    interface_root: &Path,
    source_interface_id: &str,
    target_interface_id: &str,
    root_slots: &[(&str, bool)],
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    let source = interface_root.join(source_interface_id).join("life.json");
    let bytes = std::fs::read(&source).map_err(|error| {
        format!(
            "interface '{}' has no executable life at '{}': {error}",
            source_interface_id,
            source.display()
        )
    })?;
    let mut life: SpawnLifeConfig = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "interface life '{}' is malformed: {error}",
            source.display()
        )
    })?;
    if life.processes.is_empty() {
        return Err(format!(
            "interface life '{}' contains no processes",
            source.display()
        ));
    }
    import_owner_executables(&mut life, source_interface_id)?;
    for process in &mut life.processes {
        for (root_slot, writable) in root_slots {
            process
                .authority
                .read_root_slots
                .insert((*root_slot).to_string());
            if *writable {
                process
                    .authority
                    .write_root_slots
                    .insert((*root_slot).to_string());
            }
        }
    }
    let content = pretty_json(
        &serde_json::to_value(life)
            .map_err(|error| format!("private-state-seeded life did not encode: {error}"))?,
    )?;
    push_json_write(
        interface_root.join(target_interface_id).join("life.json"),
        content,
        format!("life:{target_interface_id}"),
        writes,
    )
}

fn import_owner_executables(
    life: &mut SpawnLifeConfig,
    source_interface_id: &str,
) -> Result<(), String> {
    let source_interface_id =
        safe_segment(source_interface_id, "source interface executable owner")?;
    for process in &mut life.processes {
        let Some(relative) = process.bin.strip_prefix("owner://") else {
            continue;
        };
        let relative_path = Path::new(relative);
        if relative.is_empty()
            || relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(format!(
                "interface '{source_interface_id}' declares invalid owner executable '{}'",
                process.bin
            ));
        }
        process.bin = format!("interface://{source_interface_id}/{relative}");
    }
    Ok(())
}

fn exact_topology_interface_ids(
    identity: &PlannedIdentity,
    external_interfaces: &[String],
) -> Vec<String> {
    let mut ids = vec![
        identity.chat_interface_id.clone(),
        identity.model_interface_id.clone(),
        identity.worker_interface_id.clone(),
        identity.authoring_source_id.clone(),
        identity.authoring_environment_id.clone(),
    ];
    ids.extend(identity.capability_doors.values().cloned());
    ids.extend(
        identity
            .code_mode_owners
            .values()
            .map(|owner| owner.authoring_environment.clone()),
    );
    if let Some(voice) = &identity.voice { ids.extend(voice.interfaces().into_iter().cloned()); }
    ids.extend(home_outputs::interfaces(identity));
    ids.extend(external_interfaces.iter().cloned());
    ids.sort();
    ids.dedup();
    ids
}

fn ensure_external_interfaces_admitted_by_ee(
    topology: &FormedCapabilityTopology,
    interfaces: &[String],
) -> Result<(), String> {
    let Some(allowance) = topology.ee_interface_allowance.as_ref() else {
        return Ok(());
    };
    let allowed = allowance
        .iter()
        .map(|interface| interface.as_str())
        .collect::<BTreeSet<_>>();
    let missing = interfaces
        .iter()
        .filter(|interface| !allowed.contains(interface.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!(
        "task connections exceed the formed EE interface allowance: {}",
        missing.join(", ")
    ))
}

fn select_capability_topology(
    mut topology: FormedCapabilityTopology,
    config: &ClientConfig,
    selected_worker_environment_kinds: &[String],
    selected_tools: &[SelectedToolSeed],
) -> Result<FormedCapabilityTopology, String> {
    let granted = topology
        .granted_worker_environment_kinds
        .iter()
        .map(|kind| kind.as_str())
        .collect::<BTreeSet<_>>();
    let mut selected = BTreeSet::new();
    for kind in selected_worker_environment_kinds {
        let kind = safe_segment(kind, "selected_worker_environment_kinds")?;
        if !selected.insert(kind.clone()) {
            return Err(format!(
                "selected_worker_environment_kinds repeats '{kind}'"
            ));
        }
        if !granted.contains(kind.as_str()) {
            return Err(format!(
                "worker environment kind '{kind}' is outside the formed worker/EE grant"
            ));
        }
    }

    let granted_tools = topology
        .granted_tools
        .iter()
        .map(|tool| tool.as_str())
        .collect::<BTreeSet<_>>();
    let mut selected_tool_ids = BTreeSet::new();
    let mut selected_tool_contracts = BTreeMap::new();
    for selection in selected_tools {
        let tool = safe_policy_axis_value(&selection.tool, "selected_tools.tool")?;
        let interface = safe_segment(&selection.interface, "selected_tools.interface")?;
        let kind = safe_segment(
            &selection.worker_environment_kind,
            "selected_tools.worker_environment_kind",
        )?;
        if !selected_tool_ids.insert(tool.clone()) {
            return Err(format!("selected_tools repeats '{tool}'"));
        }
        if !granted_tools.contains(tool.as_str()) {
            return Err(format!(
                "tool '{tool}' is outside the formed worker/EE grant"
            ));
        }
        selected_tool_contracts.insert(tool, (interface, kind));
    }

    let mut used_selected = BTreeSet::new();
    let mut used_selected_tools = BTreeSet::new();
    let mut retained_kinds = BTreeSet::new();
    topology.tool_bindings.retain(|source, binding| {
        binding.tools.retain(|tool| {
            let mandatory_authoring = source.as_str() == config.authoring.room_interface
                && tool.tool.as_str() == config.authoring.tool;
            let kind = &tool.worker_environment_kind;
            let exact_tool_selected =
                selected_tool_contracts
                    .get(tool.tool.as_str())
                    .is_some_and(|(interface, kind)| {
                        interface == source.as_str()
                            && kind == tool.worker_environment_kind.as_str()
                    });
            let retained =
                mandatory_authoring || exact_tool_selected || selected.contains(kind.as_str());
            if retained && selected.contains(kind.as_str()) {
                used_selected.insert(kind.as_str().to_string());
            }
            if exact_tool_selected {
                used_selected_tools.insert(tool.tool.as_str().to_string());
            }
            retained
        });
        let binding_kinds = binding
            .tools
            .iter()
            .map(|tool| &tool.worker_environment_kind)
            .map(|kind| kind.as_str().to_string())
            .collect::<BTreeSet<_>>();
        retained_kinds.extend(binding_kinds.iter().cloned());
        binding
            .worker_environment_kinds
            .retain(|kind| binding_kinds.contains(kind.as_str()));
        binding
            .sidecars
            .retain(|sidecar| binding_kinds.contains(sidecar.worker_environment_kind.as_str()));
        !binding.tools.is_empty()
    });
    let unused = selected
        .difference(&used_selected)
        .cloned()
        .collect::<Vec<_>>();
    if !unused.is_empty() {
        return Err(format!(
            "selected worker environment kinds expose no formed tools: {}",
            unused.join(", ")
        ));
    }
    let unused_tools = selected_tool_ids
        .difference(&used_selected_tools)
        .cloned()
        .collect::<Vec<_>>();
    if !unused_tools.is_empty() {
        return Err(format!(
            "selected tools expose no formed exact capability: {}",
            unused_tools.join(", ")
        ));
    }
    topology.granted_tools = topology
        .tool_bindings
        .values()
        .flat_map(|binding| binding.tools.iter().map(|tool| tool.tool.clone()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    topology.granted_worker_environment_kinds = retained_kinds
        .into_iter()
        .map(octopus_descriptors::WorkerEnvironmentKindId::new)
        .collect();
    Ok(topology)
}

fn set_policy_axis(
    object: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
    values: impl IntoIterator<Item = String>,
) -> Result<(), String> {
    if !object.contains_key(field) {
        return Err(format!("private worker policy ceiling.{field} is absent"));
    }
    let mut exact = BTreeSet::new();
    for value in values {
        let value =
            safe_policy_axis_value(&value, &format!("private worker policy ceiling.{field}"))?;
        exact.insert(value);
    }
    object.insert(
        field.to_string(),
        serde_json::Value::Array(exact.into_iter().map(serde_json::Value::String).collect()),
    );
    Ok(())
}

fn validate_optional_policy_axis(
    values: &Option<Vec<String>>,
    field: &str,
    required: &[String],
    template_id: &str,
) -> Result<(), String> {
    let Some(values) = values else {
        return Ok(());
    };
    let allowed = values.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let missing = required
        .iter()
        .filter(|value| !allowed.contains(value.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!(
        "EE template '{template_id}' {field} does not admit exact task topology: {}",
        missing.join(", ")
    ))
}

fn validate_ee_capability_allowances(
    registry_root: &Path,
    chain: &[octopus_descriptors::EeTemplateId],
    exact_interfaces: &[String],
    exact_tools: &[String],
) -> Result<(), String> {
    for template_id in chain {
        let path = registry_root
            .join("ee_template")
            .join(template_id.as_str())
            .join("ee_template.json");
        let bytes = std::fs::read(&path).map_err(|error| {
            format!(
                "EE template policy '{}' cannot be read: {error}",
                path.display()
            )
        })?;
        let descriptor = parse_strict::<EeTemplateDescriptor>(&bytes).map_err(|error| {
            format!(
                "EE template policy '{}' is malformed: {error}",
                path.display()
            )
        })?;
        if descriptor.id != *template_id {
            return Err(format!(
                "EE template policy '{}' declares a mismatched id",
                path.display()
            ));
        }
        validate_optional_policy_axis(
            &descriptor.allowed_interfaces,
            "allowed_interfaces",
            exact_interfaces,
            template_id.as_str(),
        )?;
        validate_optional_policy_axis(
            &descriptor.allowed_tools,
            "allowed_tools",
            exact_tools,
            template_id.as_str(),
        )?;
    }
    Ok(())
}

fn capability_binding_tools(
    config: &ClientConfig,
    identity: &PlannedIdentity,
    source_interface_id: &InterfaceId,
    binding: &octopus_world_terminal_client::ToolCapabilityBinding,
) -> Result<Vec<String>, String> {
    binding
        .tools
        .iter()
        .filter(|tool| {
            !(source_interface_id.as_str() == config.authoring.room_interface
                && tool.tool.as_str() == config.authoring.tool
                && identity.capability_tools.get(tool.tool.as_str()) == Some(&identity.authoring_tool_id))
        })
        .map(|tool| {
            identity
                .capability_tools
                .get(tool.tool.as_str())
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "formed tool '{}' has no private exact identity",
                        tool.tool.as_str()
                    )
                })
        })
        .collect()
}

fn tool_call_requirement(config: &ClientConfig, tools: Vec<String>) -> serde_json::Value {
    serde_json::json!({
        "mark": config.run.tool_call_mark,
        "artifact_label": config.artifact_labels.tool_call,
        "verifier": config.run.tool_call_verifier,
        "params": { "tools": tools },
    })
}

pub(crate) fn authoring_call_params(tool: &str) -> serde_json::Value {
    serde_json::json!({
        "tools": [tool],
        "metadata_matches": [{
            "payload": "/arguments/expected_before_hash",
            "metadata": "/runtime/run_record_hash",
        }],
    })
}

fn authoring_call_requirement(config: &ClientConfig, tool: &str) -> serde_json::Value {
    let mut requirement = tool_call_requirement(config, vec![tool.to_string()]);
    requirement["params"] = authoring_call_params(tool);
    requirement
}

fn operator_chat_input_binding(
    interface: &str,
    artifact_label: &str,
) -> Result<AuthoredInterfaceBinding, String> {
    operator_chat_input_binding_names(interface, &[artifact_label.to_string()])
}

fn operator_chat_input_binding_names(
    interface: &str,
    artifact_labels: &[String],
) -> Result<AuthoredInterfaceBinding, String> {
    if artifact_labels.is_empty() {
        return Err("operator chat input artifact labels are empty".to_string());
    }
    let mut names = BTreeSet::new();
    for label in artifact_labels {
        let label = label.trim();
        if label.is_empty() || label.contains(['\r', '\n', '\0']) {
            return Err("operator chat input artifact label is malformed".to_string());
        }
        if !names.insert(label.to_string()) {
            return Err(format!(
                "operator chat input artifact label repeats '{label}'"
            ));
        }
    }
    let binding = AuthoredInterfaceBinding {
        interface: InterfaceId::new(interface),
        config: InterfaceBindingConfig {
            artifact_policy: BindingArtifactPolicy::Allow {
                names: names.into_iter().collect(),
                namespaces: Vec::new(),
            },
        },
    };
    binding
        .validate()
        .map_err(|error| format!("operator chat input artifact binding is invalid: {error}"))?;
    Ok(binding)
}

fn previous_external_route_templates(
    profile: &OctopusOsAuthoringProfile,
    model_interface_id: &str,
) -> Result<Vec<(String, BoundCheckpointTemplate)>, String> {
    let mut routes = Vec::new();
    for input in &profile.external_inputs {
        routes.push((
            input.checkpoint_id.clone(),
            bound_checkpoint_template(
                BoundCheckpointRole::Stable,
                AuthoredInterfaceBinding::all(InterfaceId::new(&input.interface_id)),
                vec![AuthoredInterfaceBinding::all(InterfaceId::new(
                    model_interface_id,
                ))],
                input.required_verified_artifacts.clone(),
            )?,
        ));
    }
    for relay in &profile.external_relays {
        routes.push((
            relay.checkpoint_id.clone(),
            bound_checkpoint_template(
                BoundCheckpointRole::IndependentInput,
                AuthoredInterfaceBinding::all(InterfaceId::new(&relay.interface_id)),
                relay
                    .outbound_interface_ids
                    .iter()
                    .map(|interface| AuthoredInterfaceBinding::all(InterfaceId::new(interface)))
                    .collect(),
                relay.required_verified_artifacts.clone(),
            )?,
        ));
    }
    for output in &profile.model_outputs {
        routes.push((
            output.checkpoint_id.clone(),
            bound_checkpoint_template(
                BoundCheckpointRole::ProviderArtifactOutput,
                AuthoredInterfaceBinding::all(InterfaceId::new(model_interface_id)),
                vec![AuthoredInterfaceBinding::all(InterfaceId::new(
                    &output.interface_id,
                ))],
                vec![output.required_verified_artifact.clone()],
            )?,
        ));
    }
    Ok(routes)
}

fn resolve_previous_external_route_checkpoints(
    profile: Option<&OctopusOsAuthoringProfile>,
    checkpoints: &[serde_json::Value],
    active: &BTreeSet<String>,
    run_id: &str,
    model_interface_id: &str,
) -> Result<BTreeSet<String>, String> {
    let Some(profile) = profile else {
        return Ok(BTreeSet::new());
    };
    let mut resolved = BTreeSet::new();
    for (authored_id, template) in previous_external_route_templates(profile, model_interface_id)? {
        let matches = checkpoints
            .iter()
            .filter_map(|checkpoint| {
                let id = checkpoint.get("id")?.as_str()?;
                active.contains(id).then_some((id, checkpoint))
            })
            .filter_map(|(id, checkpoint)| {
                match checkpoint_matches_bound_template(checkpoint, &template, run_id) {
                    Ok(true) => Some(Ok(id.to_string())),
                    Ok(false) => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .collect::<Result<Vec<_>, String>>()?;
        if matches.len() != 1 {
            return Err(format!(
                "existing external route '{authored_id}' matches {} exact active checkpoints",
                matches.len()
            ));
        }
        if !resolved.insert(matches[0].clone()) {
            return Err(format!(
                "existing external routes resolve the same checkpoint '{}'",
                matches[0]
            ));
        }
    }
    Ok(resolved)
}

fn build_chat_run_record(
    config: &ClientConfig,
    identity: &PlannedIdentity,
    cycle: &str,
    topology: &FormedCapabilityTopology,
    role_artifacts: &[String],
    selection: &RunSelection,
    existing: Option<&serde_json::Value>,
    conversation: &ConversationSeed,
    external_inputs: &[ExternalInputSeed],
    external_relays: &[ExternalRelaySeed],
    model_outputs: &[ModelOutputSeed],
    context_artifact_bindings: &[ArtifactBindingSeed],
    payload_artifact_bindings: &[octopus_ledger::ArtifactRef],
    provider_client_operation_labels: &[String],
    previous_authoring_profile: Option<&OctopusOsAuthoringProfile>,
) -> Result<String, String> {
    if let Some(existing) = existing {
        assert_existing_run_contract(existing, identity, selection)?;
    }
    let retained_connections = retained_connections::selected(
        existing, identity, previous_authoring_profile, external_inputs, external_relays, model_outputs,
    )?;
    let mut mandatory_role_artifacts = Vec::new();
    let mut seen = BTreeSet::new();
    for id in role_artifacts.iter().cloned() {
        if id.trim().is_empty() {
            return Err("role artifact binding id is empty".to_string());
        }
        if seen.insert(id.clone()) {
            mandatory_role_artifacts.push(serde_json::json!({ "id": id }));
        }
    }
    let mut context_artifacts = mandatory_role_artifacts.clone();
    let retained_history = existing_bound_run_authoring_topology(existing, identity)?
        .map(|binding| {
            let bytes = serde_json::to_vec(existing.ok_or("retained history has no run")?)
                .map_err(|error| error.to_string())?;
            let record = RunRecord::from_bytes(&bytes).map_err(|error| error.to_string())?;
            let static_ids = binding
                .checkpoints
                .iter()
                .flat_map(|template| &template.static_artifact_bindings)
                .chain(binding.payload_artifact_bindings.iter().map(|reference| &reference.id))
                .collect::<BTreeSet<_>>();
            Ok::<_, String>(
                octopus_provider_run_authoring::provider_context_bindings(
                    &record,
                    &InterfaceId::new(&identity.model_interface_id),
                )?
                .into_iter()
                .filter(|artifact| !static_ids.contains(&artifact.id))
                .collect::<Vec<_>>(),
            )
        })
        .transpose()?;
    for id in config
        .run
        .context_artifact_bindings
        .iter()
        .map(|binding| binding.id.clone())
        .chain(
            conversation
                .artifact_bindings
                .iter()
                .filter(|_| retained_history.is_none())
                .map(|binding| binding.id.clone()),
        )
        .chain(
            conversation
                .pinned_artifact_bindings
                .iter()
                .map(|binding| binding.id.clone()),
        )
        .chain(
            context_artifact_bindings
                .iter()
                .map(|binding| binding.id.clone()),
        )
        .chain(retained_connections.iter().flat_map(|connection|
            connection.template.static_artifact_bindings.iter().map(ToString::to_string)))
    {
        if id.trim().is_empty() {
            return Err("context artifact binding id is empty".to_string());
        }
        if identity.code_mode_owners.values().any(|owner| owner.prelude_sources.iter().any(|source| source.source.id.as_str() == id)) {
            continue;
        }
        if seen.insert(id.clone()) {
            context_artifacts.push(serde_json::json!({ "id": id }));
        }
    }

    octopus_provider_run_authoring::validate_payload_artifact_selection(payload_artifact_bindings)?;
    let reserved = identity.code_mode_owners.values().flat_map(|owner| {
        std::iter::once(&owner.prelude.id).chain(std::iter::once(&owner.topology.id))
            .chain(owner.prelude_sources.iter().flat_map(|source| [&source.source.id, &source.snapshot.id]))
    }).collect::<BTreeSet<_>>();
    for reference in payload_artifact_bindings {
        if seen.contains(reference.id.as_str()) || reserved.contains(&reference.id) {
            return Err(format!("payload selection '{}' collides with a context, role or prelude binding", reference.id));
        }
        seen.insert(reference.id.as_str().to_string());
        context_artifacts.push(serde_json::json!({"id":reference.id}));
    }
    for binding in retained_history.into_iter().flatten() {
        if seen.insert(binding.id.to_string()) {
            context_artifacts.push(serde_json::to_value(binding).map_err(|error| error.to_string())?);
        }
    }

    let checkpoint = |id: String,
                      inbound: Option<&str>,
                      outbound: Vec<&str>,
                      artifacts: &[serde_json::Value],
                      required: Vec<serde_json::Value>|
     -> serde_json::Value {
        let mut object = serde_json::Map::new();
        object.insert("id".to_string(), serde_json::json!(&id));
        object.insert("run".to_string(), serde_json::json!(identity.run_id));
        object.insert(
            "trace".to_string(),
            serde_json::json!(format!("trace:{}:{id}", identity.run_id)),
        );
        object.insert("epoch".to_string(), serde_json::json!(0));
        if let Some(inbound) = inbound {
            object.insert(
                "interface_binding".to_string(),
                serde_json::json!(AuthoredInterfaceBinding::all(InterfaceId::new(inbound))),
            );
        }
        object.insert(
            "outbound_interface_bindings".to_string(),
            serde_json::json!(
                outbound
                    .into_iter()
                    .map(|interface| AuthoredInterfaceBinding::all(InterfaceId::new(interface)))
                    .collect::<Vec<_>>()
            ),
        );
        object.insert(
            "artifact_bindings".to_string(),
            serde_json::Value::Array(artifacts.to_vec()),
        );
        object.insert(
            "required_verified_artifacts".to_string(),
            serde_json::Value::Array(required),
        );
        serde_json::Value::Object(object)
    };

    let input_id = format!("cp_operator_input_{cycle}");
    let turn_control_id = format!("cp_operator_turn_control_{cycle}");
    let provider_operation_id = format!("cp_operator_provider_operation_{cycle}");
    let worker_input_id = format!("cp_worker_input_{cycle}");
    let client_id = if identity.native_communication.is_some() {
        format!("cp_provider_output_{cycle}")
    } else {
        format!("cp_provider_client_{cycle}")
    };
    let authoring_call_id = format!("cp_provider_authoring_{cycle}");
    let authoring_result_id = format!("cp_authoring_provider_{cycle}");
    let mut cycle_checkpoints = Vec::new();
    if identity.owns_client() {
        let chat_input_binding =
            operator_chat_input_binding(&identity.chat_interface_id, &config.artifact_labels.message)?;
        let turn_control_binding = operator_chat_input_binding(
            &identity.chat_interface_id,
            &config.artifact_labels.turn_control,
        )?;
        let mut operator_input = checkpoint(
            input_id.clone(),
            Some(&identity.chat_interface_id),
            vec![&identity.model_interface_id],
            &context_artifacts,
            Vec::new(),
        );
        operator_input["interface_binding"] = serde_json::to_value(chat_input_binding)
            .map_err(|error| format!("operator chat input binding did not encode: {error}"))?;
        let mut turn_control_input = checkpoint(
            turn_control_id,
            Some(&identity.chat_interface_id),
            vec![&identity.model_interface_id],
            &mandatory_role_artifacts,
            Vec::new(),
        );
        turn_control_input["interface_binding"] = serde_json::to_value(turn_control_binding)
            .map_err(|error| format!("operator turn-control binding did not encode: {error}"))?;
        cycle_checkpoints.extend([operator_input, turn_control_input]);
    }
    cycle_checkpoints.push(checkpoint(
            worker_input_id.clone(),
            Some(&identity.worker_interface_id),
            vec![&identity.model_interface_id],
            &context_artifacts,
            Vec::new(),
        ));
    if let Some(client) = identity.native_communication.as_ref().filter(|_| !identity.owns_client()).and_then(|routes| routes.client_interface_id.as_deref()) {
        cycle_checkpoints.push(checkpoint(format!("cp_client_input_{cycle}"), Some(client), vec![&identity.model_interface_id], &context_artifacts, Vec::new()));
    }
    if identity.owns_client() && !provider_client_operation_labels.is_empty() {
        let provider_operation_binding = operator_chat_input_binding_names(
            &identity.chat_interface_id,
            provider_client_operation_labels,
        )?;
        let mut provider_operation_input = checkpoint(
            provider_operation_id,
            Some(&identity.chat_interface_id),
            vec![&identity.model_interface_id],
            &mandatory_role_artifacts,
            Vec::new(),
        );
        provider_operation_input["interface_binding"] =
            serde_json::to_value(provider_operation_binding).map_err(|error| {
                format!("operator provider-operation binding did not encode: {error}")
            })?;
        cycle_checkpoints.push(provider_operation_input);
    }
    for (source_interface_id, binding) in &topology.tool_bindings {
        let tools = capability_binding_tools(config, identity, source_interface_id, binding)?;
        if tools.is_empty() {
            continue;
        }
        let door_id = identity
            .capability_doors
            .get(source_interface_id.as_str())
            .ok_or_else(|| {
                format!(
                    "capability interface '{}' has no planned exact door",
                    source_interface_id
                )
            })?;
        if identity.code_mode_owners.values().any(|owner| owner.nested_doors.contains(door_id)) { continue; }
        let segment = capability_interface_segment(source_interface_id.as_str());
        let capability_input_id = format!("cp_capability_{segment}_input_{cycle}");
        let owner = identity.code_mode_owners.get(source_interface_id.as_str());
        let mut artifacts = mandatory_role_artifacts.clone();
        if let Some(owner) = owner {
            artifacts.extend([serde_json::json!({"id":owner.prelude.id}),serde_json::json!({"id":owner.topology.id})]);
        } else if config.run.tool_result_receiving.is_none() {
            cycle_checkpoints.push(checkpoint(capability_input_id,Some(door_id),vec![&identity.model_interface_id],&context_artifacts,Vec::new()));
        }
        artifacts.extend(payload_artifact_bindings.iter().map(|reference| serde_json::json!({"id":reference.id})));
        cycle_checkpoints.push(checkpoint(
            format!("cp_capability_{segment}_output_{cycle}"),
            Some(&identity.model_interface_id),
            vec![door_id],
            &artifacts,
            vec![tool_call_requirement(config, tools)],
        ));
    }
    let model_output_ids = model_outputs
        .iter()
        .map(|output| {
            if cycle_checkpoints.iter().any(|checkpoint| {
                checkpoint.get("id").and_then(serde_json::Value::as_str)
                    == Some(output.checkpoint_id.as_str())
            }) {
                return Err(format!(
                    "model output checkpoint '{}' collides with the client cycle",
                    output.checkpoint_id
                ));
            }
            cycle_checkpoints.push(checkpoint(
                output.checkpoint_id.clone(),
                Some(&identity.model_interface_id),
                vec![&output.interface_id],
                &mandatory_role_artifacts,
                vec![
                    serde_json::to_value(&output.required_verified_artifact).map_err(|error| {
                        format!("model output requirement did not encode: {error}")
                    })?,
                ],
            ));
            Ok(output.checkpoint_id.clone())
        })
        .collect::<Result<Vec<_>, String>>()?;
    let return_interfaces = match &identity.native_communication {
        Some(routes) => routes.return_interface_ids.iter().map(String::as_str)
            .chain(routes.client_interface_id.as_deref()).collect(),
        None => vec![identity.chat_interface_id.as_str()],
    };
    let mut provider_output = checkpoint(
        client_id.clone(),
        Some(&identity.model_interface_id),
        return_interfaces,
        &mandatory_role_artifacts,
        Vec::new(),
    );
    let recorded_output_id = if let Some(selection) = identity.native_communication.as_ref()
        .and_then(|routes| routes.output_selection.as_ref())
    {
        provider_output["interface_binding"]["config"]["artifact_policy"] = serde_json::json!({"mode":"allow","names":selection.return_labels,"namespaces":[]});
        let recorded_id = format!("cp_provider_recorded_{cycle}");
        let client = identity.native_communication.as_ref().and_then(|routes| routes.client_interface_id.as_deref());
        let mut recorded = checkpoint(recorded_id.clone(), Some(&identity.model_interface_id), client.into_iter().collect(), &mandatory_role_artifacts, Vec::new());
        recorded["interface_binding"]["config"]["artifact_policy"] = serde_json::json!({"mode":"allow","names":selection.record_labels,"namespaces":[]});
        cycle_checkpoints.push(recorded);
        Some(recorded_id)
    } else { None };
    cycle_checkpoints.push(provider_output);
    cycle_checkpoints.push(checkpoint(
        authoring_call_id.clone(),
        Some(&identity.authoring_source_id),
        vec![&identity.authoring_environment_id],
        &mandatory_role_artifacts,
        vec![authoring_call_requirement(config, &identity.authoring_tool_id)],
    ));
    cycle_checkpoints.push(checkpoint(
        authoring_result_id.clone(),
        Some(&identity.authoring_environment_id),
        vec![&identity.model_interface_id],
        &mandatory_role_artifacts,
        Vec::new(),
    ));
    let cycle_ids = cycle_checkpoints
        .iter()
        .filter_map(|entry| entry.get("id").and_then(serde_json::Value::as_str))
        .collect::<BTreeSet<_>>();
    let external_input_checkpoints = external_inputs
        .iter()
        .map(|input| {
            if cycle_ids.contains(input.checkpoint_id.as_str()) {
                return Err(format!(
                    "external input checkpoint '{}' collides with the client cycle",
                    input.checkpoint_id
                ));
            }
            let required = input
                .required_verified_artifacts
                .iter()
                .map(|required| {
                    serde_json::to_value(required).map_err(|error| {
                        format!(
                            "external input checkpoint '{}' requirement did not encode: {error}",
                            input.checkpoint_id
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(checkpoint(
                input.checkpoint_id.clone(),
                Some(&input.interface_id),
                vec![&identity.model_interface_id],
                &context_artifacts,
                required,
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let external_relay_checkpoints = external_relays
        .iter()
        .map(|relay| {
            if cycle_ids.contains(relay.checkpoint_id.as_str()) {
                return Err(format!(
                    "external relay checkpoint '{}' collides with the client cycle",
                    relay.checkpoint_id
                ));
            }
            let required = relay
                .required_verified_artifacts
                .iter()
                .map(|required| {
                    serde_json::to_value(required).map_err(|error| {
                        format!(
                            "external relay checkpoint '{}' requirement did not encode: {error}",
                            relay.checkpoint_id
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(checkpoint(
                relay.checkpoint_id.clone(),
                Some(&relay.interface_id),
                relay
                    .outbound_interface_ids
                    .iter()
                    .map(String::as_str)
                    .collect(),
                &mandatory_role_artifacts,
                required,
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let external_checkpoints = external_input_checkpoints
        .into_iter()
        .chain(external_relay_checkpoints)
        .collect::<Vec<_>>();

    let previous_checkpoints = match existing.and_then(|record| record.get("checkpoints")) {
        None => Vec::new(),
        Some(serde_json::Value::Array(values)) => values.clone(),
        Some(_) => return Err("existing run.checkpoints must be an array".to_string()),
    };
    let previous_active = match existing.and_then(|record| record.get("active_checkpoints")) {
        None => BTreeSet::new(),
        Some(serde_json::Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .filter(|id| !id.trim().is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| "existing active checkpoint id is malformed".to_string())
            })
            .collect::<Result<BTreeSet<_>, _>>()?,
        Some(_) => return Err("existing run.active_checkpoints must be an array".to_string()),
    };
    let mut previous_checkpoint_ids = BTreeSet::new();
    for checkpoint in &previous_checkpoints {
        let id = checkpoint
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| "existing checkpoint.id is malformed".to_string())?;
        if !previous_checkpoint_ids.insert(id.to_string()) {
            return Err(format!("existing checkpoints repeat id '{id}'"));
        }
    }
    if !previous_active.is_subset(&previous_checkpoint_ids) {
        return Err("existing active checkpoints contain ids absent from checkpoints".to_string());
    }
    let previous_client_cycle = match existing {
        None => BTreeSet::new(),
        Some(record) => record
            .get("interface_state")
            .and_then(|state| state.get(&identity.chat_interface_id))
            .and_then(|state| state.get("cycle_checkpoint_ids"))
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                format!(
                    "existing chat state '{}' does not record cycle_checkpoint_ids",
                    identity.chat_interface_id
                )
            })?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .filter(|id| !id.trim().is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| "existing client cycle checkpoint id is malformed".to_string())
            })
            .collect::<Result<BTreeSet<_>, _>>()?,
    };
    let existing_provider_state = existing
        .and_then(|record| record.get("interface_state"))
        .and_then(|state| state.get(&identity.model_interface_id))
        .and_then(serde_json::Value::as_object)
        .cloned();
    let mut provider_frontier = BTreeSet::new();
    if let Some(state) = existing_provider_state.as_ref() {
        let frontier = state
            .get("frontier_checkpoint_ids")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                "existing provider state.frontier_checkpoint_ids must be an array".to_string()
            })?;
        for value in frontier {
            let id = value
                .as_str()
                .filter(|id| !id.trim().is_empty())
                .ok_or_else(|| "existing provider frontier id is malformed".to_string())?;
            if !provider_frontier.insert(id.to_string()) {
                return Err(format!("existing provider frontier repeats id '{id}'"));
            }
        }
    }
    if !provider_frontier.is_subset(&previous_active) {
        return Err("existing provider frontier is not fully active".to_string());
    }
    let mut previous_provider_context = BTreeSet::new();
    let previous_binding = existing_bound_run_authoring_topology(existing, identity)?;
    let mut admitted_inputs = Vec::new();
    let mut admitted_input_ids = BTreeSet::new();
    if let Some(binding) = &previous_binding {
        if config.run.tool_result_receiving.is_some() && binding.checkpoints.iter().any(|output| {
            output.role == BoundCheckpointRole::ProviderToolOutput && output.result_receiving.is_empty()
                && output.outbound_interface_bindings.iter().any(|door| binding.checkpoints.iter().any(|input|
                    input.role == BoundCheckpointRole::Stable && input.interface_binding.interface == door.interface))
        }) {
            return Err("existing tool returns require checked native result-receiving adoption before task recomposition".into());
        }
        for template in binding.checkpoints.iter().filter(|template| matches!(template.role, BoundCheckpointRole::VerifiedInput | BoundCheckpointRole::ResultInput)) {
            let rows = previous_checkpoints.iter().filter(|cp| cp.get("id").and_then(serde_json::Value::as_str).is_some_and(|id| previous_active.contains(id)))
                .filter_map(|cp| match checkpoint_matches_bound_template(cp, template, &identity.run_id) { Ok(true) => Some(Ok(cp)), Ok(false) => None, Err(error) => Some(Err(error)) })
                .collect::<Result<Vec<_>, String>>()?;
            let [row] = rows.as_slice() else { return Err("retained admitted input is absent or ambiguous".into()); };
            let id = row["id"].as_str().ok_or("retained input id is malformed")?.to_string();
            if !admitted_input_ids.insert(id) { return Err("retained admitted input is claimed twice".into()); }
            admitted_inputs.push(template.clone());
        }
    }
    if let Some(binding) = previous_binding.as_ref() {
        previous_provider_context = bound_context_checkpoint_ids(
            &previous_checkpoints,
            &previous_active,
            binding,
            identity,
        )?;
    } else if existing.is_some() && !provider_frontier.is_empty() {
        return Err("existing provider frontier has no durable run authoring binding".to_string());
    }
    let previous_external_routes = resolve_previous_external_route_checkpoints(
        previous_authoring_profile,
        &previous_checkpoints,
        &previous_active,
        &identity.run_id,
        &identity.model_interface_id,
    )?;
    let replaced_ids = previous_client_cycle
        .union(&provider_frontier)
        .cloned()
        .collect::<BTreeSet<_>>()
        .union(&previous_provider_context)
        .cloned()
        .collect::<BTreeSet<_>>()
        .union(&previous_external_routes)
        .cloned()
        .collect::<BTreeSet<_>>()
        .difference(&admitted_input_ids)
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut retained = Vec::new();
    let mut observed_active = BTreeSet::new();
    for mut candidate in previous_checkpoints {
        let id = candidate
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| "existing checkpoint.id is malformed".to_string())?
            .to_string();
        if replaced_ids.contains(&id) {
            continue;
        }
        if previous_active.contains(&id) {
            if !admitted_input_ids.contains(&id) { merge_mandatory_role_bindings(&mut candidate, &mandatory_role_artifacts)?; }
            observed_active.insert(id);
            retained.push(candidate);
        }
    }
    let expected_external_active = previous_active
        .difference(&replaced_ids)
        .cloned()
        .collect::<BTreeSet<_>>();
    if observed_active != expected_external_active {
        return Err("existing active checkpoints do not resolve to retained records".to_string());
    }

    let mut next_frontier = vec![serde_json::json!(client_id)];
    next_frontier.extend(recorded_output_id.map(serde_json::Value::String));
    for (source_interface_id, binding) in &topology.tool_bindings {
        if capability_binding_tools(config, identity, source_interface_id, binding)?.is_empty() {
            continue;
        }
        let door = identity.capability_doors.get(source_interface_id.as_str()).ok_or("capability frontier has no exact door")?;
        if identity.code_mode_owners.values().any(|owner| owner.nested_doors.contains(door)) { continue; }
        let segment = capability_interface_segment(source_interface_id.as_str());
        next_frontier.push(serde_json::json!(format!(
            "cp_capability_{segment}_output_{cycle}"
        )));
    }
    next_frontier.extend(model_output_ids.into_iter().map(serde_json::Value::String));
    next_frontier.push(serde_json::json!(authoring_call_id));
    next_frontier.push(serde_json::json!(authoring_result_id));

    let mut next_binding = derive_bound_run_authoring_topology(
        identity,
        &cycle_checkpoints,
        &external_checkpoints,
        &next_frontier,
        &config.artifact_labels.message,
        &config.run.tool_call_verifier,
    )?;
    for template in admitted_inputs {
        if next_binding.checkpoints.iter().any(|next| next.name == template.name) { return Err("retained admitted input collides with a new template".into()); }
        next_binding.checkpoints.push(template);
    }

    for connection in retained_connections {
        let template = connection.template;
        if next_binding.checkpoints.iter().any(|planned| planned.name == template.name) {
            return Err("retained connection collides with a client-authored template".into());
        }
        let id = format!("cp_retained_{}_{}", safe_segment(&template.name, "retained template name")?, cycle);
        if previous_checkpoint_ids.contains(&id) || cycle_checkpoints.iter().chain(&external_checkpoints)
            .any(|cp| cp.get("id").and_then(serde_json::Value::as_str) == Some(id.as_str())) {
            return Err("retained connection collides with an existing checkpoint".into());
        }
        let mut artifacts = mandatory_role_artifacts.clone();
        for binding in connection.artifacts {
            if !artifacts.iter().any(|value| value.get("id").and_then(serde_json::Value::as_str) == Some(binding.id.as_str())) {
                artifacts.push(serde_json::to_value(binding).map_err(|e| e.to_string())?);
            }
        }
        let mut row = checkpoint(id.clone(), None, Vec::new(), &artifacts, Vec::new());
        row["interface_binding"] = serde_json::to_value(&template.interface_binding).map_err(|e| e.to_string())?;
        row["outbound_interface_bindings"] = serde_json::to_value(&template.outbound_interface_bindings).map_err(|e| e.to_string())?;
        cycle_checkpoints.push(row);
        next_frontier.push(serde_json::json!(id));
        next_binding.checkpoints.push(template);
    }

    next_binding.payload_artifact_bindings = payload_artifact_bindings.to_vec();
    if let Some(result) = &config.run.tool_result_receiving {
        for output in next_binding.checkpoints.iter_mut().filter(|cp| cp.role == BoundCheckpointRole::ProviderToolOutput) {
            let [door] = output.outbound_interface_bindings.as_slice() else { return Err("tool output requires one exact result door".into()); };
            if identity.code_mode_owners.keys().any(|source| identity.capability_doors.get(source).is_some_and(|id| id == door.interface.as_str())) { continue; }
            output.result_receiving = tool_result_requirements(output, result)?;
        }
    }
    for output in next_binding.checkpoints.iter_mut().filter(|cp| cp.role == BoundCheckpointRole::ProviderToolOutput) {
        output.static_artifact_bindings.extend(payload_artifact_bindings.iter().map(|reference| reference.id.clone()));
    }
    for (source, owner) in &identity.code_mode_owners {
        let door = identity.capability_doors.get(source).ok_or("persistent owner has no exact door")?;
        let mut ids = vec![owner.prelude.id.clone(),owner.topology.id.clone()];
        ids.extend(payload_artifact_bindings.iter().map(|reference| reference.id.clone()));
        let output = next_binding.checkpoints.iter_mut().find(|cp| cp.role == BoundCheckpointRole::ProviderToolOutput && cp.outbound_interface_bindings.len()==1 && cp.outbound_interface_bindings[0].interface.as_str()==door).ok_or("persistent owner has no exact provider call template")?;
        output.static_artifact_bindings = ids.clone();
        let mut returned = bound_checkpoint_template(BoundCheckpointRole::OwnerInput,AuthoredInterfaceBinding::all(InterfaceId::new(door)),vec![AuthoredInterfaceBinding::all(InterfaceId::new(&identity.model_interface_id))],Vec::new())?;
        returned.include_context = true;
        returned.owner_authoring = Some(BoundAuthoringWriter {
            owner: InterfaceId::new(door),source:InterfaceId::new(door),door:InterfaceId::new(&owner.authoring_environment),tool:owner.authoring_tool.clone(),call_label:config.artifact_labels.tool_call.clone(),result_label:format!("{}.result",owner.authoring_tool),
        });
        next_binding.checkpoints.push(returned);
    }

    let mut interface_state = existing
        .and_then(|record| record.get("interface_state"))
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let cycle_checkpoint_ids = cycle_checkpoints
        .iter()
        .map(|entry| {
            entry
                .get("id")
                .and_then(serde_json::Value::as_str)
                .expect("planned checkpoint carries an id")
                .to_string()
        })
        .collect::<Vec<_>>();
    if identity.owns_client() {
        interface_state.insert(
            identity.chat_interface_id.clone(),
            serde_json::json!({
                "schema_version": 1,
                "client_surface_role": identity.surface_role,
                "provider_choice_interface": identity.provider_choice_interface,
                "provider_runtime_interface": identity.model_interface_id,
                "worker_choice_interface": identity.worker_choice_interface,
                "ledger_offset": conversation.ledger_offset,
                "conversation_artifact_bindings": conversation.artifact_bindings,
                "pinned_artifact_bindings": conversation.pinned_artifact_bindings,
                "pinned_context_items": conversation.pinned_context_items,
                "continued_from": conversation.continued_from,
                "cycle_checkpoint_ids": cycle_checkpoint_ids,
                "voice_context": identity.voice.as_ref().map(|voice| &voice.context),
            }),
        );
    }
    let mut provider_state = existing_provider_state.unwrap_or_else(|| {
        serde_json::json!({
            "context_generation": 0,
            "authoring_generation": 0,
        })
        .as_object()
        .cloned()
        .expect("provider state literal is an object")
    });
    provider_state.insert(
        "frontier_checkpoint_ids".to_string(),
        serde_json::Value::Array(next_frontier),
    );
    provider_state.insert(
        "run_authoring_binding".to_string(),
        serde_json::to_value(next_binding)
            .map_err(|error| format!("run authoring binding did not encode: {error}"))?,
    );
    interface_state.insert(
        identity.model_interface_id.clone(),
        serde_json::Value::Object(provider_state),
    );

    let mut active_checkpoints = expected_external_active.into_iter().collect::<Vec<_>>();
    let mut checkpoints = retained;
    active_checkpoints.extend(
        external_checkpoints
            .iter()
            .filter_map(|entry| entry.get("id").and_then(serde_json::Value::as_str))
            .map(str::to_string),
    );
    checkpoints.extend(external_checkpoints);
    active_checkpoints.extend(cycle_checkpoint_ids);
    checkpoints.extend(cycle_checkpoints);

    let trace = match existing.and_then(|record| record.get("trace")) {
        Some(serde_json::Value::String(trace)) if !trace.trim().is_empty() => trace.clone(),
        Some(_) => return Err("existing chat run.trace must be a non-empty string".to_string()),
        None => format!("trace:{}", identity.run_id),
    };
    for (source, owner) in &identity.code_mode_owners {
        let door = identity.capability_doors.get(source).ok_or("persistent owner has no exact door")?;
        let prelude_sources = serde_json::Value::Array(owner.prelude_sources.iter().map(|source| serde_json::json!({
            "source":source.source,"snapshot":source.snapshot,"source_sha256":source.source_sha256
        })).collect());
        if let Some(state) = interface_state.get(door) {
            if state.get("prelude_sources") != Some(&prelude_sources) {
                return Err("persistent owner source artifact identities changed during task composition".into());
            }
            let installed = state.get("owned_checkpoint_ids").and_then(serde_json::Value::as_array).ok_or("persistent owner lost its exact checkpoint ownership")?;
            if installed.is_empty() || installed.iter().any(|id| !active_checkpoints.iter().any(|active| id.as_str() == Some(active.as_str())) || !checkpoints.iter().any(|cp| cp.get("id") == Some(id))) {
                return Err("persistent owner checkpoint ownership changed during task composition".into());
            }
            continue;
        }
        let owner_id = format!("cp_owner_{door}_initial");
        let authoring_id = format!("cp_owner_{door}_authoring_initial");
        let mut artifacts = mandatory_role_artifacts.clone();
        artifacts.extend([serde_json::json!({"id":owner.prelude.id}),serde_json::json!({"id":owner.topology.id})]);
        artifacts.extend(payload_artifact_bindings.iter().map(|reference| serde_json::json!({"id":reference.id})));
        checkpoints.push(checkpoint(owner_id.clone(),Some(door),vec![door],&artifacts,Vec::new()));
        checkpoints.push(checkpoint(authoring_id.clone(),Some(&owner.authoring_environment),vec![door],&artifacts,Vec::new()));
        active_checkpoints.extend([owner_id.clone(),authoring_id.clone()]);
        let mut owned_ids = vec![owner_id.clone(),authoring_id.clone()];
        let routes = owner.topology_body.get("routes").and_then(serde_json::Value::as_array).ok_or("persistent topology has no declared routes")?;
        let mut returns = BTreeMap::new();
        for route in routes {
            let result = route.get("result").ok_or("persistent route lost its return")?;
            let name = result.get("name").and_then(serde_json::Value::as_str).ok_or("persistent return has no identity")?;
            if let Some(prior) = returns.insert(name, result) {
                if prior != result { return Err("persistent routes disagree on a shared return".into()); }
            }
        }
        for (name, result) in returns {
            if checkpoints.iter().any(|cp| cp.get("id").and_then(serde_json::Value::as_str) == Some(name)) {
                return Err("persistent initial return collides with a neighboring checkpoint".into());
            }
            let mut row = checkpoint(name.into(),None,Vec::new(),&artifacts,Vec::new());
            row["trace"] = serde_json::json!(trace);
            row["interface_binding"] = result["binding"]["inbound"].clone();
            row["outbound_interface_bindings"] = result["binding"]["outbound"].clone();
            checkpoints.push(row);
            active_checkpoints.push(name.into());
            owned_ids.push(name.to_string());
        }
        interface_state.insert(door.clone(),serde_json::json!({"frontier_checkpoint_ids":[owner_id,authoring_id],"owned_checkpoint_ids":owned_ids,"prelude_sources":prelude_sources}));
    }
    let mut record = serde_json::json!({
        "id": identity.run_id,
        "ee": selection.ee_id,
        "worker": identity.worker_interface_id,
        "trace": trace,
        "status": "idle",
        "interface_state": interface_state,
        "checkpoints": checkpoints,
        "active_checkpoints": active_checkpoints,
    });
    context_bindings::bind_selected_context(
        &mut record,
        existing,
        &identity.model_interface_id,
        config.run.context_artifact_bindings.iter().chain(context_artifact_bindings),
        identity.code_mode_owners.values().flat_map(|owner| owner.prelude_sources.iter().map(|source| &source.source.id)),
    )?;
    let content = pretty_json(&record)?;
    RunRecord::from_bytes(content.as_bytes())
        .map_err(|error| format!("planned chat run record failed validation: {error}"))?;
    Ok(content)
}

fn merge_mandatory_role_bindings(
    checkpoint: &mut serde_json::Value,
    mandatory_role_artifacts: &[serde_json::Value],
) -> Result<(), String> {
    let checkpoint_id = checkpoint
        .get("id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| "retained checkpoint.id is malformed".to_string())?
        .to_string();
    let bindings = checkpoint
        .get_mut("artifact_bindings")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| {
            format!("retained checkpoint '{checkpoint_id}' artifact_bindings must be an array")
        })?;
    let mut positions = BTreeMap::new();
    for (index, value) in bindings.iter().enumerate() {
        let binding = serde_json::from_value::<ArtifactBinding>(value.clone()).map_err(|error| {
            format!(
                "retained checkpoint '{checkpoint_id}' has a malformed artifact binding: {error}"
            )
        })?;
        if positions
            .insert(binding.id.as_str().to_string(), index)
            .is_some()
        {
            return Err(format!(
                "retained checkpoint '{checkpoint_id}' repeats artifact binding '{}'",
                binding.id.as_str()
            ));
        }
    }
    for value in mandatory_role_artifacts {
        let required = serde_json::from_value::<ArtifactBinding>(value.clone())
            .map_err(|error| format!("mandatory role artifact binding is malformed: {error}"))?;
        if let Some(index) = positions.get(required.id.as_str()).copied() {
            let mut existing = serde_json::from_value::<ArtifactBinding>(bindings[index].clone())
                .map_err(|error| {
                    format!(
                        "retained checkpoint '{checkpoint_id}' has a malformed artifact binding: {error}"
                    )
                })?;
            if !existing.access.read {
                existing.access.read = true;
                bindings[index] = serde_json::to_value(existing).map_err(|error| {
                    format!(
                        "retained checkpoint '{checkpoint_id}' role binding did not encode: {error}"
                    )
                })?;
            }
        } else {
            positions.insert(required.id.as_str().to_string(), bindings.len());
            bindings.push(value.clone());
        }
    }
    Ok(())
}

fn assert_existing_run_contract(
    existing: &serde_json::Value,
    identity: &PlannedIdentity,
    selection: &RunSelection,
) -> Result<(), String> {
    if existing.get("interface_state").and_then(|state| state.get(&identity.model_interface_id))
        .is_some_and(|state| state.get("selected_consent").is_some()) {
        return Err("selected Home consent state cannot be reinterpreted as an ordinary task continuation".into());
    }
    let field = |name: &str| -> Result<&str, String> {
        existing
            .get(name)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("existing chat run.{name} is required"))
    };
    if field("id")? != identity.run_id {
        return Err(format!(
            "existing chat run identity mismatch at {}",
            identity.run_id
        ));
    }
    if field("ee")? != selection.ee_id {
        return Err(format!(
            "existing chat run execution environment does not match {}",
            selection.ee_id
        ));
    }
    if field("worker")? != identity.worker_interface_id {
        return Err(format!(
            "existing chat run worker pin does not match {}",
            identity.worker_interface_id
        ));
    }
    Ok(())
}

fn existing_bound_run_authoring_topology(
    existing: Option<&serde_json::Value>,
    identity: &PlannedIdentity,
) -> Result<Option<BoundRunAuthoringTopology>, String> {
    let Some(existing) = existing else {
        return Ok(None);
    };
    let value = existing
        .get("interface_state")
        .and_then(|state| state.get(&identity.model_interface_id))
        .and_then(|state| state.get("run_authoring_binding"));
    let Some(value) = value else {
        return Ok(None);
    };
    let binding: BoundRunAuthoringTopology = serde_json::from_value(value.clone())
        .map_err(|error| format!("existing run authoring binding is malformed: {error}"))?;
    if binding.schema_version != 2
        || binding.run.as_str() != identity.run_id
        || binding.worker.as_str() != identity.worker_interface_id
        || binding.provider_interface.as_str() != identity.model_interface_id
        || binding.authoring_source_interface.as_str() != identity.authoring_source_id
    {
        return Err("existing run authoring binding changed its exact task identity".to_string());
    }
    Ok(Some(binding))
}

fn bound_context_checkpoint_ids(
    checkpoints: &[serde_json::Value],
    active: &BTreeSet<String>,
    binding: &BoundRunAuthoringTopology,
    identity: &PlannedIdentity,
) -> Result<BTreeSet<String>, String> {
    let templates = binding
        .checkpoints
        .iter()
        .filter(|template| {
            matches!(
                template.role,
                BoundCheckpointRole::Stable | BoundCheckpointRole::IndependentInput
            )
        })
        .collect::<Vec<_>>();
    let mut ids = BTreeSet::new();
    for template in templates {
        let matches = checkpoints
            .iter()
            .filter_map(|checkpoint| {
                let id = checkpoint.get("id")?.as_str()?;
                active.contains(id).then_some((id, checkpoint))
            })
            .filter_map(|(id, checkpoint)| {
                match checkpoint_matches_bound_template(checkpoint, template, &identity.run_id) {
                    Ok(true) => Some(Ok(id.to_string())),
                    Ok(false) => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .collect::<Result<Vec<_>, String>>()?;
        if matches.len() != 1 {
            return Err(format!(
                "existing run authoring template '{}' matches {} exact active checkpoints",
                template.name,
                matches.len(),
            ));
        }
        if !ids.insert(matches[0].clone()) {
            return Err(format!(
                "existing run authoring templates resolve the same checkpoint '{}'",
                matches[0],
            ));
        }
    }
    if ids.is_empty() {
        return Err("existing run authoring binding has no context inputs".to_string());
    }
    Ok(ids)
}

fn checkpoint_matches_bound_template(
    checkpoint: &serde_json::Value,
    template: &BoundCheckpointTemplate,
    run_id: &str,
) -> Result<bool, String> {
    let object = checkpoint
        .as_object()
        .ok_or_else(|| "existing checkpoint is not an object".to_string())?;
    let inbound = object
        .get("interface_binding")
        .cloned()
        .map(serde_json::from_value::<AuthoredInterfaceBinding>)
        .transpose()
        .map_err(|error| format!("existing checkpoint inbound binding is malformed: {error}"))?;
    let outbound = serde_json::from_value::<Vec<AuthoredInterfaceBinding>>(
        object
            .get("outbound_interface_bindings")
            .cloned()
            .ok_or_else(|| "existing checkpoint outbound bindings are absent".to_string())?,
    )
    .map_err(|error| format!("existing checkpoint outbound bindings are malformed: {error}"))?;
    let required = serde_json::from_value::<Vec<RequiredVerifiedArtifact>>(
        object
            .get("required_verified_artifacts")
            .cloned()
            .ok_or_else(|| "existing checkpoint requirements are absent".to_string())?,
    )
    .map_err(|error| format!("existing checkpoint requirements are malformed: {error}"))?;
    Ok(
        object.get("run").and_then(serde_json::Value::as_str) == Some(run_id)
            && inbound.as_ref() == Some(&template.interface_binding)
            && outbound == template.outbound_interface_bindings
            && required == template.required_verified_artifacts,
    )
}

fn derive_bound_run_authoring_topology(
    identity: &PlannedIdentity,
    cycle_checkpoints: &[serde_json::Value],
    external_checkpoints: &[serde_json::Value],
    frontier: &[serde_json::Value],
    message_artifact_label: &str,
    tool_call_verifier: &str,
) -> Result<BoundRunAuthoringTopology, String> {
    let frontier = frontier
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .ok_or_else(|| "planned provider frontier contains a malformed id".to_string())
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    let mut templates = Vec::new();
    for checkpoint in cycle_checkpoints.iter().chain(external_checkpoints) {
        let id = checkpoint
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "planned checkpoint has no id".to_string())?;
        let inbound = serde_json::from_value::<AuthoredInterfaceBinding>(
            checkpoint
                .get("interface_binding")
                .cloned()
                .ok_or_else(|| format!("planned checkpoint '{id}' has no inbound binding"))?,
        )
        .map_err(|error| {
            format!("planned checkpoint '{id}' inbound binding is malformed: {error}")
        })?;
        let outbound = serde_json::from_value::<Vec<AuthoredInterfaceBinding>>(
            checkpoint
                .get("outbound_interface_bindings")
                .cloned()
                .ok_or_else(|| format!("planned checkpoint '{id}' has no outbound bindings"))?,
        )
        .map_err(|error| {
            format!("planned checkpoint '{id}' outbound bindings are malformed: {error}")
        })?;
        let required = serde_json::from_value::<Vec<RequiredVerifiedArtifact>>(
            checkpoint
                .get("required_verified_artifacts")
                .cloned()
                .ok_or_else(|| format!("planned checkpoint '{id}' has no requirements"))?,
        )
        .map_err(|error| {
            format!("planned checkpoint '{id}' requirements are malformed: {error}")
        })?;
        let role = if inbound.interface.as_str() == identity.authoring_source_id
            && outbound.len() == 1
            && outbound[0].interface.as_str() == identity.authoring_environment_id
        {
            BoundCheckpointRole::AuthoringCall
        } else if inbound.interface.as_str() == identity.authoring_environment_id
            && outbound.len() == 1
            && outbound[0].interface.as_str() == identity.model_interface_id
        {
            BoundCheckpointRole::AuthoringResult
        } else if frontier.contains(id) && inbound.interface.as_str() == identity.model_interface_id
        {
            match required.as_slice() {
                [requirement] if requirement.artifact_label == PROVIDER_HANDOFF_COMPLETE_LABEL => {
                    BoundCheckpointRole::ProviderHandoffCompletion
                }
                [requirement]
                    if requirement.artifact_label == PROVIDER_TRANSPORT_DISPATCH_LABEL =>
                {
                    BoundCheckpointRole::ProviderTransportDispatch
                }
                [requirement] if requirement.verifier.as_str() == tool_call_verifier => {
                    BoundCheckpointRole::ProviderToolOutput
                }
                [] | [_] => BoundCheckpointRole::ProviderArtifactOutput,
                _ => {
                    return Err(format!(
                        "planned provider output '{id}' has multiple requirements"
                    ));
                }
            }
        } else if identity.owns_client()
            && inbound.interface.as_str() == identity.chat_interface_id
            && outbound.len() == 1
            && outbound[0].interface.as_str() == identity.model_interface_id
        {
            if binding_admits_only(&inbound, message_artifact_label) {
                BoundCheckpointRole::Stable
            } else {
                BoundCheckpointRole::IndependentInput
            }
        } else if outbound.len() == 1
            && outbound[0].interface.as_str() == identity.model_interface_id
        {
            BoundCheckpointRole::Stable
        } else {
            continue;
        };
        templates.push(bound_checkpoint_template(
            role, inbound, outbound, required,
        )?);
    }
    let authoring_source_interface = templates
        .iter()
        .find(|template| template.role == BoundCheckpointRole::AuthoringCall)
        .map(|template| template.interface_binding.interface.clone())
        .ok_or_else(|| "planned run authoring topology has no authoring call".to_string())?;
    if !templates
        .iter()
        .any(|template| template.role == BoundCheckpointRole::AuthoringResult)
        || !templates
            .iter()
            .any(|template| template.role == BoundCheckpointRole::Stable)
        || !templates.iter().any(|template| {
            matches!(
                template.role,
                BoundCheckpointRole::ProviderArtifactOutput
                    | BoundCheckpointRole::ProviderToolOutput
                    | BoundCheckpointRole::ProviderTransportDispatch
                    | BoundCheckpointRole::ProviderHandoffCompletion
            )
        })
    {
        return Err("planned run authoring topology is incomplete".to_string());
    }
    Ok(BoundRunAuthoringTopology {
        schema_version: 2,
        run: octopus_descriptors::RunId::new(&identity.run_id),
        worker: InterfaceId::new(&identity.worker_interface_id),
        provider_interface: InterfaceId::new(&identity.model_interface_id),
        authoring_source_interface,
        payload_artifact_bindings: Vec::new(),
        writer_run: None,
        checkpoints: templates,
    })
}

fn binding_admits_only(binding: &AuthoredInterfaceBinding, artifact_label: &str) -> bool {
    matches!(
        &binding.config.artifact_policy,
        BindingArtifactPolicy::Allow { names, namespaces }
            if namespaces.is_empty()
                && names.len() == 1
                && names[0] == artifact_label
    )
}

fn tool_result_requirements(output: &BoundCheckpointTemplate, result: &ToolResultReceiving) -> Result<BTreeMap<String, RequiredVerifiedArtifact>, String> {
    let [required] = output.required_verified_artifacts.as_slice() else { return Err("tool output requires one call contract".into()); };
    let tools = required.params.get("tools").and_then(serde_json::Value::as_array).ok_or("tool output has no explicit tools")?;
    let mut receiving = BTreeMap::new();
    for tool in tools {
        let tool = tool.as_str().filter(|tool| !tool.is_empty() && tool.trim() == *tool).ok_or("tool output has an invalid tool id")?;
        if receiving.insert(tool.to_string(), result.requirement(tool)?).is_some() { return Err("tool output repeats a tool".into()); }
    }
    if receiving.is_empty() { return Err("tool output has no result obligations".into()); }
    Ok(receiving)
}

fn result_receiving_authorized(
    registry: &Path, chain: &[octopus_world_terminal_client::LiveEePolicyLayer],
    policy: &WorkerInterfacePolicy, result: &ToolResultReceiving,
) -> Result<bool, String> {
    result.requirement("declared")?;
    safe_policy_axis_value(result.mark.as_str(), "result mark")?;
    if matches!(result.mark.as_str(), "." | "..") { return Err("result mark must name an exact registry folder".into()); }
    safe_segment(result.verifier.as_str(), "result verifier")?;
    let mark: octopus_marks::MarkDescriptor = parse_strict(&std::fs::read(registry.join("mark").join(result.mark.as_str()).join("mark.json")).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
    let verifier: octopus_verifiers::VerifierDescriptor = parse_strict(&std::fs::read(registry.join("verifier").join(result.verifier.as_str()).join("verifier.json")).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
    if mark.id != result.mark || verifier.id != result.verifier { return Err("result correspondence descriptor identity differs from its exact folder".into()); }
    let mut layers = Vec::new();
    for entry in chain {
        let template: EeTemplateDescriptor = parse_strict(&std::fs::read(registry.join("ee_template").join(entry.template.as_str()).join("ee_template.json")).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
        layers.extend([template.as_policy_layer(), entry.policy_narrowing.as_policy_layer()]);
    }
    Ok(octopus_policy::form_policy(policy.ceiling_grant(), &layers, &[], Default::default()).context.mark_outcomes.contains(&result.mark))
}

fn bound_checkpoint_template(
    role: BoundCheckpointRole,
    interface_binding: AuthoredInterfaceBinding,
    outbound_interface_bindings: Vec<AuthoredInterfaceBinding>,
    required_verified_artifacts: Vec<RequiredVerifiedArtifact>,
) -> Result<BoundCheckpointTemplate, String> {
    let include_context = role == BoundCheckpointRole::Stable;
    let bytes = serde_json::to_vec(&serde_json::json!({
        "role": role,
        "interface_binding": interface_binding,
        "outbound_interface_bindings": outbound_interface_bindings,
        "include_context": include_context,
        "required_verified_artifacts": required_verified_artifacts,
    }))
    .map_err(|error| format!("bound checkpoint template did not encode: {error}"))?;
    let digest = format!("{:x}", Sha256::digest(bytes));
    Ok(BoundCheckpointTemplate {
        name: format!("derived_{}", &digest[..20]),
        role,
        result_receiving: BTreeMap::new(),
        static_artifact_bindings: Vec::new(),
        owner_authoring: None,
        interface_binding,
        outbound_interface_bindings,
        include_context,
        required_verified_artifacts,
    })
}

fn sibling_descriptor_bytes(
    source: &InterfaceDescriptor,
    sibling_id: &str,
    source_names: Option<&[String]>,
    driver_config: Option<&octopus_interfaces::InterfaceDriverConfig>,
) -> Result<String, String> {
    let mut sibling = source.clone();
    sibling.id = InterfaceId::new(sibling_id);
    if let Some(source_names) = source_names {
        sibling.source_names = source_names.to_vec();
    }
    if let Some(config) = driver_config {
        sibling.interface_driver_config = Some(config.clone());
    }
    let content = pretty_json(
        &serde_json::to_value(sibling)
            .map_err(|error| format!("private interface did not encode: {error}"))?,
    )?;
    let parsed = parse_strict::<InterfaceDescriptor>(content.as_bytes()).map_err(|error| {
        format!("private interface '{sibling_id}' failed descriptor validation: {error}")
    })?;
    if parsed.id.as_str() != sibling_id {
        return Err(format!(
            "private interface '{sibling_id}' is not a self-contained exact interface"
        ));
    }
    Ok(content)
}

fn preserve_exact_door_host_slots(
    interface_root: &Path,
    source: &InterfaceDescriptor,
    exact_id: &str,
) -> Result<Option<octopus_interfaces::InterfaceDriverConfig>, String> {
    let path = interface_root.join(exact_id).join("interface.json");
    let existing = match std::fs::read(&path) {
        Ok(bytes) => Some(
            parse_strict::<InterfaceDescriptor>(&bytes).map_err(|error| {
                format!(
                    "existing exact door descriptor '{}' is malformed: {error}",
                    path.display()
                )
            })?,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!(
                "existing exact door descriptor '{}' cannot be read: {error}",
                path.display()
            ));
        }
    };
    let Some(existing) = existing else {
        return Ok(source.interface_driver_config.clone());
    };
    if existing.id.as_str() != exact_id
        || existing.interface_driver_ref != source.interface_driver_ref
        || existing.interface_type != source.interface_type
        || existing.role != source.role
        || existing.direction != source.direction
    {
        return Err(format!(
            "existing exact door '{}' changed its executable boundary contract",
            exact_id
        ));
    }
    let Some(existing_config) = existing.interface_driver_config else {
        return Ok(source.interface_driver_config.clone());
    };
    if existing_config.host_slots.is_empty() {
        return Ok(source.interface_driver_config.clone());
    }
    let mut planned = source.interface_driver_config.clone().unwrap_or_default();
    planned.host_slots.extend(existing_config.host_slots);
    Ok(Some(planned))
}

#[derive(Default)]
struct ExistingPrivateCapabilityOwnership {
    interfaces: BTreeSet<String>,
    tools: BTreeSet<String>,
}

fn existing_private_capability_ownership(
    record: &serde_json::Value,
    identity: &PlannedIdentity,
    registry_root: &Path,
) -> Result<ExistingPrivateCapabilityOwnership, String> {
    let mut candidate_interfaces = BTreeSet::new();
    let mut requirements = Vec::new();
    if let Some(binding) = existing_bound_run_authoring_topology(Some(record), identity)? {
        for checkpoint in binding.checkpoints {
            candidate_interfaces.insert(checkpoint.interface_binding.interface);
            candidate_interfaces.extend(
                checkpoint
                    .outbound_interface_bindings
                    .into_iter()
                    .map(|binding| binding.interface),
            );
            requirements.extend(checkpoint.required_verified_artifacts);
        }
    } else {
        let interface_state = record
            .get("interface_state")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| "existing chat run.interface_state is required".to_string())?;
        let client_state = interface_state
            .get(&identity.chat_interface_id)
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| "existing chat run has no exact client state".to_string())?;
        let cycle_ids = client_state
            .get("cycle_checkpoint_ids")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "existing client cycle_checkpoint_ids must be an array".to_string())?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .filter(|id| !id.trim().is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| "existing client cycle checkpoint id is malformed".to_string())
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        let checkpoints = record
            .get("checkpoints")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "existing chat run.checkpoints must be an array".to_string())?;
        for checkpoint in checkpoints {
            let checkpoint_id = checkpoint
                .get("id")
                .and_then(serde_json::Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .ok_or_else(|| "existing checkpoint.id is malformed".to_string())?;
            if !cycle_ids.contains(checkpoint_id) {
                continue;
            }
            if let Some(inbound) = checkpoint.get("interface_binding") {
                let inbound: AuthoredInterfaceBinding = serde_json::from_value(inbound.clone())
                    .map_err(|error| {
                        format!("existing checkpoint interface binding is malformed: {error}")
                    })?;
                candidate_interfaces.insert(inbound.interface);
            }
            let outbound = serde_json::from_value::<Vec<AuthoredInterfaceBinding>>(
                checkpoint
                    .get("outbound_interface_bindings")
                    .cloned()
                    .ok_or_else(|| {
                        "existing checkpoint outbound bindings are absent".to_string()
                    })?,
            )
            .map_err(|error| {
                format!("existing checkpoint outbound bindings are malformed: {error}")
            })?;
            candidate_interfaces.extend(outbound.into_iter().map(|binding| binding.interface));
            requirements.extend(
                serde_json::from_value::<Vec<RequiredVerifiedArtifact>>(
                    checkpoint
                        .get("required_verified_artifacts")
                        .cloned()
                        .ok_or_else(|| "existing checkpoint requirements are absent".to_string())?,
                )
                .map_err(|error| {
                    format!("existing checkpoint requirements are malformed: {error}")
                })?,
            );
        }
    }
    let mut ownership = ExistingPrivateCapabilityOwnership::default();
    for interface in candidate_interfaces {
        if interface.as_str() != identity.chat_interface_id
            && interface.as_str() != identity.model_interface_id
            && interface.as_str() != identity.worker_interface_id
            && interface.as_str() != identity.authoring_source_id
            && interface.as_str() != identity.authoring_environment_id
            && attest_private_interface_owner(registry_root, interface.as_str(), identity)?
        {
            ownership.interfaces.insert(interface.as_str().to_string());
        }
    }
    for required in requirements {
        let Some(tools) = required
            .params
            .get("tools")
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        for tool in tools {
            let tool = tool
                .as_str()
                .filter(|id| !id.trim().is_empty())
                .ok_or_else(|| "existing tool requirement contains a malformed id".to_string())?;
            if tool != identity.authoring_tool_id
                && attest_private_tool_owner(registry_root, tool, identity)?
            {
                ownership.tools.insert(tool.to_string());
            }
        }
    }
    Ok(ownership)
}

fn attest_private_interface_owner(
    registry_root: &Path,
    interface_id: &str,
    identity: &PlannedIdentity,
) -> Result<bool, String> {
    let directory = registry_root.join("interface").join(interface_id);
    let descriptor = match std::fs::read(directory.join("interface.json")) {
        Ok(bytes) => parse_strict::<InterfaceDescriptor>(&bytes).map_err(|error| {
            format!("existing private interface '{interface_id}' is malformed: {error}")
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "existing private interface '{interface_id}' cannot be read: {error}"
            ));
        }
    };
    let sidecar = match std::fs::read(directory.join("octopus_os.json")) {
        Ok(bytes) => serde_json::from_slice::<OctopusOsInterfaceSidecar>(&bytes).map_err(|error| {
            format!("existing private interface '{interface_id}' owner sidecar is malformed: {error}")
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("existing private interface '{interface_id}' owner sidecar cannot be read: {error}")),
    };
    Ok(descriptor.id.as_str() == interface_id
        && sidecar.schema_version == 1
        && sidecar.workspace == "octopus_os"
        && sidecar.interface == interface_id
        && sidecar.owner_interface == identity.chat_interface_id
        && sidecar.component == "worker_environment_door"
        && sidecar.surface_role == identity.surface_role)
}

fn attest_private_tool_owner(
    registry_root: &Path,
    tool_id: &str,
    identity: &PlannedIdentity,
) -> Result<bool, String> {
    let Some(descriptor) = load_tool_descriptor_from_dir(&registry_root.join("tool"), tool_id)
        .map_err(|error| error.to_string())?
    else {
        return Ok(false);
    };
    if descriptor.id.as_str() != tool_id {
        return Ok(false);
    }
    attest_private_interface_owner(registry_root, descriptor.interface.as_str(), identity)
}

fn planned_retained_registry_paths(
    registry_root: &Path,
    identity: &PlannedIdentity,
    writes: &[PlannedClientWrite],
) -> BTreeSet<PathBuf> {
    let mut retained = writes
        .iter()
        .map(|write| write.path.clone())
        .collect::<BTreeSet<_>>();
    retained.extend(identity.capability_doors.values().flat_map(|interface| {
        ["interface.json", "life.json", "octopus_os.json"]
            .map(|file| registry_root.join("interface").join(interface).join(file))
    }));
    retained.extend(
        identity
            .capability_tools
            .values()
            .map(|tool| registry_root.join("tool").join(tool).join("tool.json")),
    );
    retained
}

fn plan_private_capability_removals(
    registry_root: &Path,
    owned: &ExistingPrivateCapabilityOwnership,
    retained_paths: &BTreeSet<PathBuf>,
) -> Result<Vec<PlannedClientRemoval>, String> {
    let mut removals = Vec::new();
    for (registry, ids) in [("interface", &owned.interfaces), ("tool", &owned.tools)] {
        for id in ids {
            safe_policy_axis_value(id, "existing private capability id")?;
            let directory = registry_root.join(registry).join(id);
            if retained_paths
                .iter()
                .any(|path| path.starts_with(&directory))
            {
                continue;
            }
            removals.push(snapshot_planned_removal(
                &directory,
                format!("{registry}:{id}"),
            )?);
        }
    }
    removals.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(removals)
}

fn snapshot_planned_removal(path: &Path, tag: String) -> Result<PlannedClientRemoval, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        format!(
            "planned removal '{}' cannot be read: {error}",
            path.display()
        )
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "planned removal '{}' is not a plain directory",
            path.display()
        ));
    }
    let mut files = Vec::new();
    for entry in std::fs::read_dir(path).map_err(|error| {
        format!(
            "planned removal '{}' cannot be listed: {error}",
            path.display()
        )
    })? {
        let entry = entry.map_err(|error| {
            format!(
                "planned removal '{}' entry cannot be read: {error}",
                path.display()
            )
        })?;
        let file_type = entry.file_type().map_err(|error| {
            format!(
                "planned removal '{}' entry type cannot be read: {error}",
                path.display()
            )
        })?;
        if !file_type.is_file() || file_type.is_symlink() {
            return Err(format!(
                "planned removal '{}' contains a non-file entry",
                path.display()
            ));
        }
        let bytes = std::fs::read(entry.path()).map_err(|error| {
            format!(
                "planned removal '{}' file cannot be read: {error}",
                entry.path().display()
            )
        })?;
        files.push(PlannedClientRemovalFile {
            relative_path: PathBuf::from(entry.file_name()),
            sha256: sha256_hex(&bytes),
        });
    }
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    if files.is_empty() {
        return Err(format!("planned removal '{}' is empty", path.display()));
    }
    Ok(PlannedClientRemoval {
        path: path.to_path_buf(),
        tag,
        files,
    })
}

fn preserve_exact_worker_role_artifacts(
    interface_root: &Path,
    exact_id: &str,
    planned: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let path = interface_root.join(exact_id).join("policy.json");
    let existing = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<WorkerInterfacePolicy>(&bytes).map_err(|error| {
            format!(
                "existing exact worker policy '{}' is malformed: {error}",
                path.display()
            )
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "existing exact worker policy '{}' cannot be read: {error}",
                path.display()
            ));
        }
    };
    if existing.id.as_str() != exact_id {
        return Err(format!(
            "existing exact worker policy '{}' changed its interface identity",
            path.display()
        ));
    }
    let additions = existing
        .role_artifacts
        .iter()
        .map(|artifact| artifact.as_str().to_string())
        .collect::<Vec<_>>();
    append_ordered_unique_ids(planned, "role_artifacts", additions.iter())
}

fn append_ordered_unique_ids<'a>(
    object: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
    additions: impl IntoIterator<Item = &'a String>,
) -> Result<(), String> {
    let values = object
        .get_mut(field)
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| format!("descriptor field '{field}' must be an array"))?;
    let mut ids = BTreeSet::new();
    for value in values.iter() {
        let id = value
            .as_str()
            .filter(|candidate| !candidate.trim().is_empty())
            .ok_or_else(|| format!("descriptor field '{field}' contains a malformed id"))?;
        if !ids.insert(id.to_string()) {
            return Err(format!(
                "descriptor field '{field}' repeats ordered id '{id}'"
            ));
        }
    }
    for id in additions {
        if id.trim().is_empty() {
            return Err(format!(
                "descriptor field '{field}' contains a malformed addition"
            ));
        }
        if ids.insert(id.clone()) {
            values.push(serde_json::Value::String(id.clone()));
        }
    }
    Ok(())
}

fn load_interface_descriptor(
    interface_root: &Path,
    interface_id: &str,
) -> Result<InterfaceDescriptor, String> {
    let id = safe_segment(interface_id, "interface id")?;
    let path = interface_root.join(&id).join("interface.json");
    let bytes = std::fs::read(&path).map_err(|error| {
        format!(
            "interface descriptor '{}' cannot be read: {error}",
            path.display()
        )
    })?;
    let descriptor = parse_strict::<InterfaceDescriptor>(&bytes).map_err(|error| {
        format!(
            "interface descriptor '{}' is malformed: {error}",
            path.display()
        )
    })?;
    if descriptor.id.as_str() != id {
        return Err(format!(
            "interface descriptor '{}' declares mismatched id '{}'",
            path.display(),
            descriptor.id
        ));
    }
    Ok(descriptor)
}

fn push_json_write(
    path: PathBuf,
    content: String,
    tag: String,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    let planned: serde_json::Value = serde_json::from_str(&content)
        .map_err(|error| format!("planned write '{}' is not JSON: {error}", path.display()))?;
    match std::fs::read(&path) {
        Ok(existing) => {
            if let Ok(current) = serde_json::from_slice::<serde_json::Value>(&existing) {
                if current == planned {
                    return Ok(());
                }
            }
            writes.push(PlannedClientWrite {
                path,
                content,
                tag,
                expected_before_hash: Some(sha256_hex(&existing)),
            });
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            writes.push(PlannedClientWrite {
                path,
                content,
                tag,
                expected_before_hash: None,
            });
            Ok(())
        }
        Err(error) => Err(format!(
            "planned write target '{}' cannot be read: {error}",
            path.display()
        )),
    }
}

fn push_text_write(
    path: PathBuf,
    content: String,
    tag: String,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    match std::fs::read(&path) {
        Ok(existing) => {
            if existing == content.as_bytes() {
                return Ok(());
            }
            writes.push(PlannedClientWrite {
                path,
                content,
                tag,
                expected_before_hash: Some(sha256_hex(&existing)),
            });
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            writes.push(PlannedClientWrite {
                path,
                content,
                tag,
                expected_before_hash: None,
            });
            Ok(())
        }
        Err(error) => Err(format!(
            "planned write target '{}' cannot be read: {error}",
            path.display()
        )),
    }
}

fn capability_interface_segment(interface: &str) -> String {
    interface
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn pretty_json(value: &serde_json::Value) -> Result<String, String> {
    serde_json::to_string_pretty(value)
        .map(|text| format!("{text}\n"))
        .map_err(|error| format!("planned JSON did not encode: {error}"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn remove_unchanged_writes(writes: &mut Vec<PlannedClientWrite>) -> Result<(), String> {
    let mut unchanged = BTreeSet::new();
    for write in writes.iter() {
        let Some(expected) = write.expected_before_hash.as_ref() else {
            continue;
        };
        let bytes = std::fs::read(&write.path).map_err(|error| {
            format!(
                "planned write target '{}' changed before final normalization: {error}",
                write.path.display()
            )
        })?;
        if sha256_hex(&bytes) != *expected {
            return Err(format!(
                "planned write target '{}' changed before final normalization",
                write.path.display()
            ));
        }
        let same_json = match (
            serde_json::from_slice::<serde_json::Value>(&bytes),
            serde_json::from_str::<serde_json::Value>(&write.content),
        ) {
            (Ok(current), Ok(planned)) => current == planned,
            _ => false,
        };
        let same_policy = if write.tag.starts_with("policy:") {
            let current: WorkerInterfacePolicy = serde_json::from_slice(&bytes)
                .map_err(|e| format!("existing worker ceiling is malformed: {e}"))?;
            let planned: WorkerInterfacePolicy = serde_json::from_str(&write.content)
                .map_err(|e| format!("planned worker ceiling is malformed: {e}"))?;
            current.id == planned.id
                && current.role_artifacts == planned.role_artifacts
                && current.ceiling.interface_policy == planned.ceiling.interface_policy
                && current.ceiling_grant() == planned.ceiling_grant()
        } else {
            false
        };
        if bytes == write.content.as_bytes() || same_json || same_policy {
            unchanged.insert(write.path.clone());
        }
    }
    writes.retain(|write| !unchanged.contains(&write.path));
    Ok(())
}

fn normalize_write_hash_algorithm(
    writes: &mut [PlannedClientWrite],
    algorithm: WriteHashAlgorithm,
) -> Result<(), String> {
    if algorithm == WriteHashAlgorithm::Sha256 {
        return Ok(());
    }
    for write in writes {
        let Some(expected) = write.expected_before_hash.as_mut() else {
            continue;
        };
        let bytes = std::fs::read(&write.path).map_err(|error| {
            format!(
                "planned write target '{}' changed before BLAKE3 formation: {error}",
                write.path.display()
            )
        })?;
        let observed_sha256 = sha256_hex(&bytes);
        if observed_sha256 != *expected {
            return Err(format!(
                "planned write target '{}' changed before BLAKE3 formation",
                write.path.display()
            ));
        }
        *expected = blake3::hash(&bytes).to_hex().to_string();
    }
    Ok(())
}

fn validate_external_routes(
    inputs: &[ExternalInputSeed],
    relays: &[ExternalRelaySeed],
    model_outputs: &[ModelOutputSeed],
) -> Result<Vec<String>, String> {
    validate_external_route_shapes(inputs, relays, model_outputs)
}

fn validate_external_route_shapes(
    inputs: &[ExternalInputSeed],
    relays: &[ExternalRelaySeed],
    model_outputs: &[ModelOutputSeed],
) -> Result<Vec<String>, String> {
    let mut checkpoints = BTreeSet::new();
    let mut sources = BTreeSet::new();
    let mut interfaces = BTreeSet::new();
    for input in inputs {
        let checkpoint = safe_segment(&input.checkpoint_id, "external_inputs.checkpoint_id")?;
        let interface = safe_segment(&input.interface_id, "external_inputs.interface_id")?;
        if !checkpoints.insert(checkpoint.clone()) {
            return Err(format!("external routes repeat checkpoint '{checkpoint}'"));
        }
        if !sources.insert(interface.clone()) {
            return Err(format!(
                "external routes repeat source interface '{interface}'"
            ));
        }
        validate_external_route_requirements(
            &input.required_verified_artifacts,
            &format!("external input checkpoint '{checkpoint}'"),
        )?;
        interfaces.insert(interface);
    }
    for relay in relays {
        let checkpoint = safe_segment(&relay.checkpoint_id, "external_relays.checkpoint_id")?;
        let interface = safe_segment(&relay.interface_id, "external_relays.interface_id")?;
        if !checkpoints.insert(checkpoint.clone()) {
            return Err(format!("external routes repeat checkpoint '{checkpoint}'"));
        }
        if !sources.insert(interface.clone()) {
            return Err(format!(
                "external routes repeat source interface '{interface}'"
            ));
        }
        if relay.outbound_interface_ids.is_empty() {
            return Err(format!(
                "external relay checkpoint '{checkpoint}' has no outbound interfaces"
            ));
        }
        validate_external_route_requirements(
            &relay.required_verified_artifacts,
            &format!("external relay checkpoint '{checkpoint}'"),
        )?;
        interfaces.insert(interface);
        let mut outbound = BTreeSet::new();
        for target in &relay.outbound_interface_ids {
            let target = safe_segment(target, "external_relays.outbound_interface_ids")?;
            if !outbound.insert(target.clone()) {
                return Err(format!(
                    "external relay checkpoint '{checkpoint}' repeats outbound interface '{target}'"
                ));
            }
            interfaces.insert(target);
        }
    }
    for output in model_outputs {
        let checkpoint = safe_segment(&output.checkpoint_id, "model_outputs.checkpoint_id")?;
        let interface = safe_segment(&output.interface_id, "model_outputs.interface_id")?;
        if !checkpoints.insert(checkpoint.clone()) {
            return Err(format!("external routes repeat checkpoint '{checkpoint}'"));
        }
        output
            .required_verified_artifact
            .validate(&format!("model output checkpoint '{checkpoint}'"))?;
        interfaces.insert(interface);
    }
    Ok(interfaces.into_iter().collect())
}

fn validate_external_route_requirements(
    requirements: &[RequiredVerifiedArtifact],
    owner: &str,
) -> Result<(), String> {
    let mut labels = BTreeSet::new();
    for required in requirements {
        required.validate(owner)?;
        if !labels.insert(required.artifact_label.as_str()) {
            return Err(format!(
                "{owner} repeats required artifact label '{}'",
                required.artifact_label
            ));
        }
    }
    Ok(())
}

fn safe_segment(value: &str, field: &str) -> Result<String, String> {
    let segment = value.trim();
    if segment.is_empty()
        || segment.len() > 96
        || !segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "{field} must be a non-empty segment of letters, digits, underscore, or hyphen"
        ));
    }
    Ok(segment.to_string())
}

fn safe_policy_axis_value(value: &str, field: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 192
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
    {
        return Err(format!(
            "{field} must contain only letters, digits, underscore, hyphen, or dot"
        ));
    }
    Ok(value.to_string())
}

fn require_absolute<'a>(field: &str, path: &'a Path) -> Result<&'a Path, String> {
    if !path.is_absolute() {
        return Err(format!("{field} must be an absolute path"));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloned_life_imports_its_source_owner_executable() {
        let mut life: SpawnLifeConfig = serde_json::from_value(serde_json::json!({
            "processes": [{
                "bin": "owner://packages/bin/octopus-app-server-activation-life",
                "io_limits": {
                    "max_control_frame_bytes": 1048576,
                    "max_stderr_bytes": 262144,
                    "max_manifest_bytes": 1048576,
                    "max_payload_bytes": 268435456
                },
                "args": [],
                "authority": {
                    "read_root_slots": [],
                    "write_root_slots": [],
                    "executable_root_slots": [],
                    "read_paths": [],
                    "write_paths": [],
                    "executable_paths": [],
                    "executable_commands": [],
                    "read_env_paths": [],
                    "write_env_paths": [],
                    "executable_env_paths": [],
                    "allow_network": false,
                    "allow_local_ipc": true,
                    "allow_application_launch": false
                },
                "read_handles": []
            }]
        }))
        .unwrap();

        import_owner_executables(&mut life, "world_operator_chat").unwrap();

        assert_eq!(
            life.processes[0].bin,
            "interface://world_operator_chat/packages/bin/octopus-app-server-activation-life"
        );
    }

    fn repository_registry_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../..")
            .canonicalize()
            .expect("repository root resolves")
    }

    #[test]
    fn exact_role_artifact_merge_preserves_instruction_order() {
        let mut planned = serde_json::json!({"role_artifacts": ["role", "grounding", "spawn"]});
        let additions = ["grounding", "role", "personal"].map(str::to_string);
        append_ordered_unique_ids(
            planned
                .as_object_mut()
                .expect("planned policy is an object"),
            "role_artifacts",
            additions.iter(),
        )
        .expect("exact-only role artifacts merge");
        assert_eq!(
            planned["role_artifacts"],
            serde_json::json!(["role", "grounding", "spawn", "personal"])
        );
    }

    #[test]
    fn retained_checkpoint_receives_readable_mandatory_role_bindings() {
        let mut checkpoint = serde_json::json!({
            "id": "cp_retained",
            "artifact_bindings": [
                {"id": "artifact-existing"},
                {
                    "id": "artifact-role-existing",
                    "access": {"read": false, "change": true}
                }
            ]
        });
        let mandatory = vec![
            serde_json::json!({"id": "artifact-role-existing"}),
            serde_json::json!({"id": "artifact-role-new"}),
        ];

        merge_mandatory_role_bindings(&mut checkpoint, &mandatory)
            .expect("retained checkpoint role bindings merge");

        let bindings = checkpoint["artifact_bindings"]
            .as_array()
            .expect("artifact bindings remain an array")
            .iter()
            .map(|value| {
                serde_json::from_value::<ArtifactBinding>(value.clone())
                    .expect("merged artifact binding parses")
            })
            .collect::<Vec<_>>();
        assert_eq!(bindings[0].id.as_str(), "artifact-existing");
        assert_eq!(bindings[1].id.as_str(), "artifact-role-existing");
        assert!(bindings[1].access.read);
        assert!(bindings[1].access.change);
        assert_eq!(bindings[2].id.as_str(), "artifact-role-new");
        assert!(bindings[2].access.read);
    }

    #[test]
    fn operator_chat_input_bindings_partition_message_and_turn_control() {
        let message =
            operator_chat_input_binding("world_operator_chat_test", "operator.message").unwrap();
        let control =
            operator_chat_input_binding("world_operator_chat_test", "operator.turn.control")
                .unwrap();
        assert!(message.admits("operator.message"));
        assert!(!message.admits("operator.turn.control"));
        assert!(control.admits("operator.turn.control"));
        assert!(!control.admits("operator.message"));
        for response in [
            "operator.approval.response",
            "operator.elicitation.response",
            "operator.user_input.response",
        ] {
            assert!(!message.admits(response));
            assert!(!control.admits(response));
        }
    }

    #[test]
    fn bound_turn_control_is_an_independent_context_free_input() {
        let identity = PlannedIdentity {
            voice: None,
            native_communication: None,
            chat_id: "chat_test".to_string(),
            run_id: "run_test".to_string(),
            ee_id: "ee_test".to_string(),
            chat_interface_id: "world_operator_chat_test".to_string(),
            model_interface_id: "world_provider_test".to_string(),
            worker_interface_id: "system_worker_test".to_string(),
            authoring_source_id: "system_authoring_source_test".to_string(),
            authoring_environment_id: "system_authoring_environment_test".to_string(),
            code_mode_owners: BTreeMap::new(),
            surface_role: "task".to_string(),
            provider_choice_interface: "world_provider_test".to_string(),
            worker_choice_interface: "system_worker_test".to_string(),
            capability_doors: BTreeMap::new(),
            capability_tools: BTreeMap::new(),
            capability_tool_sources: BTreeMap::new(),
            authoring_tool_id: "file.write__test".to_string(),
        };
        let checkpoint =
            |id: &str, inbound: AuthoredInterfaceBinding, outbound: AuthoredInterfaceBinding| {
                serde_json::json!({
                    "id": id,
                    "run": identity.run_id,
                    "trace": format!("trace:{id}"),
                    "epoch": 0,
                    "artifact_bindings": [],
                    "interface_binding": inbound,
                    "outbound_interface_bindings": [outbound],
                    "required_verified_artifacts": []
                })
            };
        let message =
            operator_chat_input_binding(&identity.chat_interface_id, "operator.message").unwrap();
        let control =
            operator_chat_input_binding(&identity.chat_interface_id, "operator.turn.control")
                .unwrap();
        let provider =
            AuthoredInterfaceBinding::all(InterfaceId::new(&identity.model_interface_id));
        let checkpoints = vec![
            checkpoint("cp_message", message, provider.clone()),
            checkpoint("cp_control", control, provider.clone()),
            checkpoint(
                "cp_authoring_call",
                AuthoredInterfaceBinding::all(InterfaceId::new(&identity.authoring_source_id)),
                AuthoredInterfaceBinding::all(InterfaceId::new(&identity.authoring_environment_id)),
            ),
            checkpoint(
                "cp_authoring_result",
                AuthoredInterfaceBinding::all(InterfaceId::new(&identity.authoring_environment_id)),
                provider.clone(),
            ),
            checkpoint(
                "cp_provider_output",
                provider,
                AuthoredInterfaceBinding::all(InterfaceId::new(&identity.chat_interface_id)),
            ),
        ];
        let topology = derive_bound_run_authoring_topology(
            &identity,
            &checkpoints,
            &[],
            &[serde_json::json!("cp_provider_output")],
            "operator.message",
            "tool_call_allowed",
        )
        .unwrap();

        let control = topology
            .checkpoints
            .iter()
            .find(|template| {
                binding_admits_only(&template.interface_binding, "operator.turn.control")
            })
            .unwrap();
        assert_eq!(control.role, BoundCheckpointRole::IndependentInput);
        assert!(!control.include_context);
        let message = topology
            .checkpoints
            .iter()
            .find(|template| binding_admits_only(&template.interface_binding, "operator.message"))
            .unwrap();
        assert_eq!(message.role, BoundCheckpointRole::Stable);
        assert!(message.include_context);
        let mut record = serde_json::json!({
            "id": identity.run_id, "ee": identity.ee_id, "worker": identity.worker_interface_id,
            "trace": "trace:context", "status": "idle",
            "active_checkpoints": checkpoints.iter().map(|row| row["id"].clone()).collect::<Vec<_>>(),
            "checkpoints": checkpoints,
            "interface_state": {&identity.model_interface_id: {"run_authoring_binding": topology}}
        });
        let source = ArtifactBindingSeed { id: "instruction-source".into() };
        context_bindings::bind_selected_context(
            &mut record, None, &identity.model_interface_id, [&source], [],
        ).unwrap();
        let control = record["checkpoints"].as_array().unwrap().iter()
            .find(|row| row["id"] == "cp_control").unwrap();
        assert_eq!(control["artifact_bindings"], serde_json::json!([{"id":"instruction-source"}]));
        let template = record["interface_state"][&identity.model_interface_id]["run_authoring_binding"]["checkpoints"]
            .as_array().unwrap().iter().find(|row| row["role"] == "independent_input").unwrap();
        assert_eq!(template["include_context"], false);
        assert_eq!(template["static_artifact_bindings"], serde_json::json!(["instruction-source"]));
    }

    #[test]
    fn provider_derived_checkpoint_identity_preserves_an_external_route() {
        let profile = OctopusOsAuthoringProfile {
            ee_id: "ee_test".to_string(),
            worker_choice_interface: "system_worker_test".to_string(),
            door_host_slots: BTreeMap::new(),
            external_inputs: vec![ExternalInputSeed {
                checkpoint_id: "cp_home_project_coordination".to_string(),
                interface_id: "system_home_project_coordination".to_string(),
                required_verified_artifacts: Vec::new(),
            }],
            external_relays: Vec::new(),
            model_outputs: Vec::new(),
            context_artifact_bindings: Vec::new(),
            payload_artifact_bindings: Vec::new(),
            selected_worker_environment_kinds: Vec::new(),
            selected_tools: Vec::new(),
            publication_evidence: Vec::new(),
        };
        let checkpoint = serde_json::json!({
            "id": "cp_context_derived_exact",
            "run": "run_test",
            "interface_binding": AuthoredInterfaceBinding::all(InterfaceId::new("system_home_project_coordination")),
            "outbound_interface_bindings": [AuthoredInterfaceBinding::all(InterfaceId::new("world_provider_test"))],
            "required_verified_artifacts": []
        });
        let active = BTreeSet::from(["cp_context_derived_exact".to_string()]);
        let resolved = resolve_previous_external_route_checkpoints(
            Some(&profile),
            &[checkpoint],
            &active,
            "run_test",
            "world_provider_test",
        )
        .expect("the route is identified by its exact edge after provider authoring");
        assert_eq!(resolved, active);
    }

    pub(super) fn boot_operator_space_ee_instances(dir: &Path) {
        let instance = |ee: &str, template: &str, parent: Option<&str>| {
            let row = octopus_ee::EeInstance {
                ee_id: EeId::new(ee),
                template: octopus_descriptors::EeTemplateId::new(template),
                parent_ee: parent.map(EeId::new),
                goal: format!("boot instance of {template}"),
                config: Default::default(),
                policy_narrowing: Default::default(),
            };
            std::fs::write(
                dir.join(format!("{ee}.json")),
                serde_json::to_vec_pretty(&row).expect("EE instance encodes"),
            )
            .expect("EE instance writes");
        };
        instance("ee_existence", "existence", None);
        instance("ee_operator_space", "operator_space", Some("ee_existence"));
    }

    #[test]
    fn exact_door_replanning_preserves_operator_host_slot_coordinates() {
        let repository = repository_registry_root();
        let source =
            load_interface_descriptor(&repository.join("registry/interface"), "system_coder_env")
                .expect("source door loads");
        let interface_root = tempfile::tempdir().expect("temporary interface registry");
        let exact_id = "system_coder_env_personal";
        let exact_dir = interface_root.path().join(exact_id);
        std::fs::create_dir_all(&exact_dir).expect("exact door directory creates");
        let mut exact = source.clone();
        exact.id = InterfaceId::new(exact_id);
        exact
            .interface_driver_config
            .get_or_insert_default()
            .host_slots
            .insert(
                "repo_root".to_string(),
                octopus_interfaces::HostSlotOverride {
                    path: PathBuf::from("/tmp/personal-project"),
                },
            );
        std::fs::write(
            exact_dir.join("interface.json"),
            serde_json::to_vec_pretty(&exact).expect("exact door encodes"),
        )
        .expect("exact door writes");

        let preserved = preserve_exact_door_host_slots(interface_root.path(), &source, exact_id)
            .expect("exact door host slots preserve")
            .expect("driver config exists");
        assert_eq!(
            preserved.host_slots["repo_root"].path,
            PathBuf::from("/tmp/personal-project")
        );
    }

    fn assert_exact_writer_detachment(plan: &OperatorChatPlan, root: &Path) {
        use serde_json::{Value, json};
        std::fs::create_dir_all(root).unwrap();
        for write in &plan.run_writes {
            std::fs::write(&write.path, &write.content).unwrap();
        }
        let mut original: Value = serde_json::from_str(&plan.run_record_content).unwrap();
        let provider = &plan.model_interface_id;
        let writer = original["interface_state"][provider]["run_authoring_binding"]
            .as_object_mut().unwrap().remove("writer_run").unwrap();
        std::fs::remove_file(root.join(format!("{}.json", writer["run"].as_str().unwrap()))).unwrap();
        let ids = writer["initial_record"]["active_checkpoints"].as_array().unwrap();
        for cp in writer["initial_record"]["checkpoints"].as_array().unwrap() {
            let mut cp = cp.clone();
            cp["run"] = json!(plan.run_id);
            original["checkpoints"].as_array_mut().unwrap().push(cp);
        }
        original["active_checkpoints"].as_array_mut().unwrap().extend(ids.clone());
        original["interface_state"][provider]["frontier_checkpoint_ids"].as_array_mut().unwrap().extend(ids.clone());
        let output = original["checkpoints"].as_array().unwrap().iter()
            .find(|cp| cp["interface_binding"]["interface"] == *provider
                && serde_json::from_value::<AuthoredInterfaceBinding>(cp["interface_binding"].clone())
                    .unwrap().admits("turn.completed")
                && cp["outbound_interface_bindings"].as_array().unwrap().iter()
                    .any(|binding| binding["interface"] == plan.chat_interface_id)).unwrap().clone();
        original["interface_state"][provider]["pending_handoff"] = json!({
            "source_movement_seq":77,"terminal":true,"items":[{
                "artifact_id":"turn.completed:exact","artifact_content_hash":"7".repeat(64),
                "artifact_label":"turn.completed","manifest_relative_path":"emitted/exact.artifact.json",
                "checkpoint":{"id":output["id"],"epoch":output["epoch"],
                    "interface_binding":output["interface_binding"],"outbound_interface_bindings":output["outbound_interface_bindings"],
                    "artifact_bindings":output["artifact_bindings"].as_array().unwrap().iter().map(|b|b["id"].clone()).collect::<Vec<_>>(),
                    "required_verified_artifacts":output["required_verified_artifacts"]}
            }],
        });
        original["interface_state"][provider]["context_generation"] = json!(16);
        let bytes = pretty_json(&original).unwrap();
        let target = root.join(format!("{}.json", plan.run_id));
        std::fs::write(&target, &bytes).unwrap();
        let mut request = OperatorWriterRunPlanRequest {
            run_records_root:root.to_path_buf(),run_id:plan.run_id.clone(),expected_before_sha256:sha256_hex(bytes.as_bytes()),
        };
        let writes = plan_operator_writer_runs(&request).unwrap();
        assert_eq!(writes.len(),2);
        assert_eq!(std::fs::read(&target).unwrap(), bytes.as_bytes(), "planning cannot mutate its source");
        let next: Value = serde_json::from_str(&writes.last().unwrap().content).unwrap();
        for key in ["pending_handoff","context_generation"] {
            assert_eq!(next["interface_state"][provider][key],original["interface_state"][provider][key]);
        }
        assert_eq!(next["interface_state"][&plan.chat_interface_id],original["interface_state"][&plan.chat_interface_id]);
        let retained: Vec<_> = original["checkpoints"].as_array().unwrap().iter().filter(|cp| !ids.contains(&cp["id"])).cloned().collect();
        assert_eq!(next["checkpoints"],json!(retained));
        request.expected_before_sha256 = "0".repeat(64);
        assert!(plan_operator_writer_runs(&request).unwrap_err().contains("changed before planning"));
        for write in writes { std::fs::write(write.path,write.content).unwrap(); }
        request.expected_before_sha256 = sha256_hex(&std::fs::read(&target).unwrap());
        assert!(plan_operator_writer_runs(&request).unwrap().is_empty());
    }

    #[test]
    fn real_registry_home_plan_exports_authoring_and_exact_policy_requirements() {
        let repository = repository_registry_root();
        let registry_root = repository.join("registry");
        let ee_dir = tempfile::tempdir().expect("temp EE instances");
        boot_operator_space_ee_instances(ee_dir.path());

        let request = OperatorChatPlanRequest {
            registry_root: registry_root.clone(),
            ee_instances_root: ee_dir.path().to_path_buf(),
            ee_policy_root: None,
            run_records_root: Some(ee_dir.path().join("runs")),
            write_hash_algorithm: WriteHashAlgorithm::Sha256,
            interface_private_state_root: Some(ee_dir.path().join("private")),
            chat_id: "planprobe0000000000000001".to_string(),
            provider_interface_id: "world_provider_ollama".to_string(),
            model: None,
            reasoning_effort: None,
            service_tier: None,
            ee_id: Some("ee_operator_space".to_string()),
            worker_interface_id: Some("system_worker_coder_max".to_string()),
            client_surface_role: "task".to_string(),
            cycle: "cycleprobe000000000000001".to_string(),
            door_host_slots: BTreeMap::new(),
            external_inputs: Vec::new(),
            external_relays: Vec::new(),
            model_outputs: Vec::new(),
            context_artifact_bindings: Vec::new(),
            payload_artifact_bindings: Vec::new(),
            selected_worker_environment_kinds: [
                "audio_device_session",
                "browser_session",
                "code_mode",
                "container_session",
                "git_worktree",
                "mcp_stdio_session",
                "process_session",
                "remote_exec_server",
                "system_room",
                "web_search",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            selected_tools: Vec::new(),
            retain_current_capabilities: false,
            existing_run_record: None,
            publications: Vec::new(),
            conversation: ConversationSeed {
                ledger_offset: 0,
                artifact_bindings: Vec::new(),
                pinned_artifact_bindings: Vec::new(),
                pinned_context_items: Vec::new(),
                continued_from: None,
            },
        };
        let plan = plan_operator_chat_topology(&request).expect("the real registry plans a chat");
        assert_exact_writer_detachment(&plan, request.run_records_root.as_ref().unwrap());
        assert_computer_use_planned_identities(&registry_root, ee_dir.path());
        assert_browser_only_persistent_plan(&registry_root, ee_dir.path());
        assert_home_and_task_browser_cu_profile(&registry_root, ee_dir.path());

        assert_eq!(
            plan.ee_template_chain,
            vec!["existence".to_string(), "operator_space".to_string()],
            "the EE chain is resolved from the live instance walk, not a declared pair"
        );
        assert_eq!(plan.client_surface_role, "task");
        assert_eq!(plan.authoring_profile.ee_id, "ee_operator_space");
        assert_eq!(
            plan.authoring_profile.worker_choice_interface,
            "system_worker_coder_max"
        );
        for exact in [
            &plan.chat_interface_id,
            &plan.model_interface_id,
            &plan.worker_interface_id,
            &plan.authoring_source_interface_id,
            &plan.authoring_environment_interface_id,
        ] {
            assert!(
                plan.required_interface_ids.contains(exact),
                "the exported policy requirement omits exact interface '{exact}'"
            );
        }
        assert!(plan.required_tool_ids.contains(&plan.authoring_tool_id));
        assert!(
            plan.capability_tools
                .values()
                .all(|tool| plan.required_tool_ids.contains(tool))
        );

        let record = RunRecord::from_bytes(plan.run_record_content.as_bytes())
            .expect("the planned record parses through the canonical run contract");
        let run_authoring_binding: BoundRunAuthoringTopology = serde_json::from_value(
            record.interface_state[&InterfaceId::new(&plan.model_interface_id)]
                ["run_authoring_binding"]
                .clone(),
        )
        .expect("a new task carries its explicit provider run-authoring binding");
        assert_eq!(run_authoring_binding.schema_version, 2);
        let writer = run_authoring_binding.writer_run.as_ref().expect("task authoring has an independent checked writer Run");
        let writer_plan = plan.run_writes.iter().find(|write| write.path.file_stem().unwrap() == writer.run.as_str()).unwrap();
        assert_eq!(serde_json::from_str::<serde_json::Value>(&writer_plan.content).unwrap(), writer.initial_record);
        assert_eq!(writer.initial_record["interface_state"][&plan.model_interface_id]["file_writer_target"]["run"], plan.run_id);
        assert!(!record.checkpoints.iter().any(|checkpoint| checkpoint.inbound_interface() == Some(&run_authoring_binding.authoring_source_interface)));
        assert_eq!(
            run_authoring_binding.provider_interface.as_str(),
            plan.model_interface_id
        );
        let operator_input = record
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.id.as_str().starts_with("cp_operator_input_"))
            .expect("operator chat input checkpoint exists");
        assert_eq!(
            operator_input
                .interface_binding
                .as_ref()
                .expect("operator input has an exact binding")
                .config
                .artifact_policy,
            BindingArtifactPolicy::Allow {
                names: vec!["operator.message".to_string()],
                namespaces: Vec::new(),
            },
            "generic chat input cannot bypass a dynamically authored verified operator response edge"
        );
        let turn_control_input = record
            .checkpoints
            .iter()
            .find(|checkpoint| {
                checkpoint
                    .id
                    .as_str()
                    .starts_with("cp_operator_turn_control_")
            })
            .expect("operator turn-control checkpoint exists");
        assert_ne!(operator_input.id, turn_control_input.id);
        let message_binding = operator_input.interface_binding.as_ref().unwrap();
        let control_binding = turn_control_input.interface_binding.as_ref().unwrap();
        assert!(message_binding.admits("operator.message"));
        assert!(!message_binding.admits("operator.turn.control"));
        assert!(control_binding.admits("operator.turn.control"));
        assert!(!control_binding.admits("operator.message"));
        assert!(
            !turn_control_input.artifact_bindings.is_empty(),
            "every checkpoint carries the worker role dependencies that formation must prove"
        );
        assert!(
            !operator_input.artifact_bindings.is_empty(),
            "ordinary operator input keeps the authored conversation context"
        );
        let requirement_tools: BTreeSet<String> = record
            .checkpoints
            .iter()
            .flat_map(|checkpoint| checkpoint.required_verified_artifacts.iter())
            .filter_map(|required| required.params.get("tools"))
            .filter_map(serde_json::Value::as_array)
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_string)
            .collect();
        let private_policy_write = plan
            .interface_writes
            .iter()
            .find(|write| write.tag == format!("policy:{}", plan.worker_interface_id))
            .expect("the private worker policy is planned");
        let private_policy: WorkerInterfacePolicy =
            serde_json::from_str(&private_policy_write.content)
                .expect("private worker ceiling parses");
        let mut model_tools: BTreeSet<String> = private_policy
            .ceiling
            .tools
            .iter()
            .map(|tool| tool.as_str().to_string())
            .collect();
        assert!(model_tools.remove(&plan.authoring_tool_id));
        assert!(writer.initial_record["checkpoints"].as_array().unwrap().iter().any(|cp|
            cp["required_verified_artifacts"].as_array().unwrap().iter().any(|requirement|
                requirement["params"]["tools"].as_array().is_some_and(|tools| tools.contains(&serde_json::json!(plan.authoring_tool_id))))),
            "the private authoring writer has its own required verifier");
        assert_eq!(
            requirement_tools, model_tools,
            "every model-visible private tool is reachable through exactly one authored door verifier"
        );
        assert!(
            plan.capability_tools
                .iter()
                .all(|(base, tool)| tool != base && tool.ends_with(&request.chat_id)),
            "every formed capability receives a private exact tool identity"
        );

        let relay_pins: BTreeSet<&str> = plan.worker_relays.iter().map(String::as_str).collect();
        for pin in [
            "system_worker_coder",
            "system_worker_coder_max",
            "system_worker_planner",
            "system_worker_memory",
            "system_worker_web",
            "system_worker_flash",
            "system_worker_computer_use",
            "system_worker_telegram",
        ] {
            assert!(
                relay_pins.contains(pin),
                "granted worker relay '{pin}' is formed and reported by the plan; got {relay_pins:?}"
            );
        }
        let current_worker_roles = private_policy
            .role_artifacts
            .iter()
            .map(|artifact| artifact.as_str().to_string())
            .collect::<Vec<_>>();
        let relay_ids = plan
            .worker_relays
            .iter()
            .map(InterfaceId::new)
            .collect::<Vec<_>>();
        let mandatory_roles = formed_worker_role_artifacts(
            &repository_registry_root().join("registry/interface"),
            &current_worker_roles,
            relay_ids.iter(),
        )
        .expect("formed role dependency closure resolves");
        for checkpoint in &record.checkpoints {
            let bindings = checkpoint
                .artifact_bindings
                .iter()
                .map(|binding| binding.id.as_str())
                .collect::<BTreeSet<_>>();
            assert!(
                mandatory_roles
                    .iter()
                    .all(|artifact| bindings.contains(artifact.as_str())),
                "checkpoint '{}' omits a mandatory formed worker role dependency; expected {mandatory_roles:?}, got {bindings:?}",
                checkpoint.id.as_str()
            );
        }
        let context_bindings: BTreeSet<&str> = record
            .checkpoints
            .iter()
            .flat_map(|checkpoint| checkpoint.artifact_bindings.iter())
            .map(|binding| binding.id.as_str())
            .collect();
        assert!(
            context_bindings.contains("artifact-spawn-literacy"),
            "the worker pin's role artifacts ground on-the-fly sub-agency in the run's context; got {context_bindings:?}"
        );
        let exact_tool_owners = plan
            .interface_writes
            .iter()
            .filter(|write| write.tag.starts_with("tool:"))
            .map(|write| {
                serde_json::from_str::<ToolDescriptor>(&write.content)
                    .expect("private tool descriptor parses")
                    .interface
            })
            .collect::<BTreeSet<_>>();
        for door in plan
            .capability_doors
            .values()
            .chain(std::iter::once(&plan.authoring_environment_interface_id))
        {
            assert!(
                exact_tool_owners.contains(&InterfaceId::new(door)),
                "private door '{door}' owns at least one exact tool"
            );
        }
        let provider_write = plan
            .interface_writes
            .iter()
            .find(|write| write.tag == format!("interface:{}", plan.model_interface_id))
            .expect("the plan authors the private provider interface");
        let provider_value: serde_json::Value =
            serde_json::from_str(&provider_write.content).expect("provider descriptor parses");
        assert_eq!(
            provider_value.get("reference"),
            None,
            "a private provider is a self-contained exact interface without inherited descriptor identity"
        );
        assert_eq!(
            provider_value["interface_driver_ref"],
            serde_json::json!("codex_app_server"),
            "the private provider uses the declared Codex harness driver"
        );
        assert_eq!(
            provider_value["interface_driver_config"]["private_config"]["opencodex"]["credential_owner_interface"],
            serde_json::json!("world_provider_ollama"),
            "the private provider preserves the exact source interface as credential owner"
        );
        assert_eq!(
            provider_value["interface_driver_config"]["private_config"]["opencodex"]["adapter"],
            serde_json::json!("openai-chat"),
            "the private provider preserves the declared OpenCodex wire adapter"
        );
        assert_eq!(
            provider_value["interface_driver_config"]["private_config"]["model"],
            serde_json::json!("qwen2.5-coder:7b"),
            "the variant's own overrides ride into the private sibling"
        );
        assert_eq!(
            provider_value["interface_driver_config"]["private_config"]["run_authoring_profile"]["file_write_tool"],
            serde_json::json!(&plan.authoring_tool_id),
            "interface-owned run authoring uses its exact hidden file tool"
        );
        assert_eq!(
            provider_value["interface_driver_config"]["private_config"]["run_authoring_profile"]["file_write_result_label"],
            serde_json::json!(format!("{}.result", plan.authoring_tool_id)),
            "interface-owned run authoring awaits the exact hidden tool result"
        );
        let browser_source: serde_json::Value = serde_json::from_slice(
            &std::fs::read(registry_root.join("interface/system_browser/interface.json")).unwrap(),
        ).unwrap();
        let upload_resume = browser_source["interface_driver_config"]["private_config"]["browser_room"]["upload_approval"]["resume_tool"]
            .as_str().expect("selected browser declares its private approval continuation");
        assert_eq!(
            provider_value["interface_driver_config"]["private_config"]["model_hidden_tools"],
            serde_json::json!([&plan.authoring_tool_id, &plan.capability_tools[upload_resume]]),
            "authoring and the exact declared approval continuation stay callable by their owners and hidden from the model"
        );
        assert_eq!(
            provider_value["interface_driver_config"]["private_config"].get("run_authoring"),
            None,
            "the exact provider derives run topology from the attested run instead of reverse-owning it"
        );
        assert!(
            plan.interface_writes
                .iter()
                .any(|write| write.tag == format!("policy:{}", plan.worker_interface_id)),
            "the plan authors the private worker pin ceiling"
        );
        assert!(
            plan.interface_writes
                .iter()
                .any(|write| write.tag == format!("life:{}", plan.model_interface_id)),
            "the exact provider carries its own executable life instead of inheriting it"
        );
        assert!(
            plan.interface_writes
                .iter()
                .all(|write| !write.tag.starts_with("worker-environment-kind:")),
            "an exact task topology must not rewrite a shared code-kind descriptor"
        );
        let authoring_tool_write = plan
            .interface_writes
            .iter()
            .find(|write| write.tag == format!("tool:{}", plan.authoring_tool_id))
            .expect("the authoring room receives an exact tool descriptor");
        let authoring_tool: ToolDescriptor = serde_json::from_str(&authoring_tool_write.content)
            .expect("planned exact tool descriptor parses");
        assert_eq!(
            authoring_tool.interface,
            InterfaceId::new(&plan.authoring_environment_interface_id)
        );
        assert_eq!(
            authoring_tool
                .supported_worker_environment_kinds
                .iter()
                .map(|kind| kind.as_str())
                .collect::<Vec<_>>(),
            vec!["system_room"]
        );
        assert!(
            authoring_tool
                .process
                .authority
                .write_argument_paths
                .iter()
                .all(|path| path.within_root_slots
                    == BTreeSet::from(["run_records_root".to_string()]))
        );

        let mut existing_record: serde_json::Value =
            serde_json::from_str(&plan.run_record_content).expect("planned record re-parses");
        existing_record["checkpoints"]
            .as_array_mut()
            .expect("checkpoints remain an array")
            .push(serde_json::json!({
                "id": "cp_external_schedule",
                "run": plan.run_id,
                "trace": format!("trace:{}:cp_external_schedule", plan.run_id),
                "epoch": 0,
                "interface_binding": AuthoredInterfaceBinding::all(InterfaceId::new("world_cron")),
                "outbound_interface_bindings": [AuthoredInterfaceBinding::all(InterfaceId::new(&plan.model_interface_id))],
                "artifact_bindings": [],
                "required_verified_artifacts": []
            }));
        existing_record["active_checkpoints"]
            .as_array_mut()
            .expect("active checkpoints remain an array")
            .push(serde_json::json!("cp_external_schedule"));
        let previous_cycle =
            existing_record["interface_state"][&plan.chat_interface_id]["cycle_checkpoint_ids"]
                .as_array()
                .expect("client cycle ids remain an array")
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<BTreeSet<_>>();
        let previous_frontier =
            existing_record["interface_state"][&plan.model_interface_id]["frontier_checkpoint_ids"]
                .as_array()
                .expect("provider frontier ids remain an array")
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<BTreeSet<_>>();
        let mut provider_frontier = Vec::new();
        let mut reauthored_active = vec![serde_json::json!("cp_external_schedule")];
        for (index, checkpoint) in existing_record["checkpoints"]
            .as_array_mut()
            .expect("checkpoints remain an array")
            .iter_mut()
            .enumerate()
        {
            let id = checkpoint["id"]
                .as_str()
                .expect("checkpoint id remains a string")
                .to_string();
            if !previous_cycle.contains(&id) {
                continue;
            }
            let next_id = if previous_frontier.contains(&id) {
                format!("cp_provider_frontier_{index}")
            } else {
                format!("cp_provider_context_{index}")
            };
            checkpoint["id"] = serde_json::json!(next_id);
            reauthored_active.push(serde_json::json!(next_id));
            if previous_frontier.contains(&id) {
                provider_frontier.push(serde_json::json!(next_id));
            }
        }
        existing_record["active_checkpoints"] = serde_json::Value::Array(reauthored_active);
        existing_record["interface_state"][&plan.model_interface_id]["frontier_checkpoint_ids"] =
            serde_json::Value::Array(provider_frontier);
        let active = existing_record["active_checkpoints"]
            .as_array()
            .expect("active checkpoints remain an array")
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<BTreeSet<_>>();
        let active_exact_client_labels = existing_record["checkpoints"]
            .as_array()
            .expect("checkpoints remain an array")
            .iter()
            .filter(|checkpoint| {
                checkpoint["id"]
                    .as_str()
                    .is_some_and(|id| active.contains(id))
            })
            .filter_map(|checkpoint| {
                serde_json::from_value::<AuthoredInterfaceBinding>(
                    checkpoint.get("interface_binding")?.clone(),
                )
                .ok()
            })
            .filter(|binding| binding.interface.as_str() == plan.chat_interface_id)
            .flat_map(|binding| match binding.config.artifact_policy {
                BindingArtifactPolicy::Allow { names, .. } => names,
                BindingArtifactPolicy::All => Vec::new(),
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            active_exact_client_labels,
            BTreeSet::from([
                "operator.message".to_string(),
                "operator.turn.control".to_string(),
                "provider.models.list".to_string(),
            ]),
            "an existing run must preserve every active exact client binding"
        );
    }

    fn assert_computer_use_planned_identities(registry_root: &Path, ee_root: &Path) {
        let request: OperatorChatPlanRequest = serde_json::from_value(serde_json::json!({
            "registry_root":registry_root,"ee_instances_root":ee_root,"run_records_root":ee_root.join("runs"),"interface_private_state_root":ee_root.join("private"),
            "write_hash_algorithm":"sha256","chat_id":"cu_exact_identity",
            "provider_interface_id":"world_provider_codex","ee_id":"ee_operator_space",
            "worker_interface_id":"system_worker_computer_use","client_surface_role":"task",
            "cycle":"cu_exact_cycle","selected_worker_environment_kinds":["desktop_session"],
            "conversation":{"ledger_offset":0}
        }))
        .unwrap();
        let plan = plan_operator_chat_topology(&request).expect("actual Computer Use task plans");
        let initial = &plan.capability_tools["gui.app_state"];
        let resume = &plan.capability_tools["gui.app_resume"];
        assert_ne!(initial, "gui.app_state");
        assert_ne!(resume, "gui.app_resume");
        let planned_json = |tag: String| -> serde_json::Value {
            serde_json::from_str(
                &plan
                    .interface_writes
                    .iter()
                    .find(|write| write.tag == tag)
                    .expect("exact planned write exists")
                    .content,
            )
            .unwrap()
        };
        let policy = planned_json(format!("policy:{}", plan.worker_interface_id));
        for tool in [initial, resume] {
            assert!(plan.required_tool_ids.contains(tool));
            assert!(
                policy["ceiling"]["tools"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!(tool))
            );
            let descriptor = planned_json(format!("tool:{tool}"));
            assert_eq!(descriptor["id"], *tool);
            assert_eq!(descriptor["interface"], plan.capability_doors["system_gui"]);
        }
        let provider = planned_json(format!("interface:{}", plan.model_interface_id));
        let hidden = provider["interface_driver_config"]["private_config"]["model_hidden_tools"]
            .as_array()
            .unwrap();
        assert!(hidden.contains(&serde_json::json!(resume)));
        assert!(!hidden.contains(&serde_json::json!(initial)));
        assert!(!hidden.contains(&serde_json::json!("gui.app_resume")));
        let desktop = planned_json(format!(
            "sidecar:{}:desktop.json",
            plan.capability_doors["system_gui"]
        ));
        assert_eq!(
            desktop["app_approval"]["operator_interface"],
            plan.chat_interface_id
        );
        assert_eq!(desktop["app_approval"]["resume_tool"], *resume);
        let source: serde_json::Value = serde_json::from_slice(
            &std::fs::read(registry_root.join("interface/system_gui/desktop.json")).unwrap(),
        )
        .unwrap();
        let mut unchanged = desktop.clone();
        unchanged["app_approval"] = source["app_approval"].clone();
        assert_eq!(unchanged, source);

        let mut private =
            serde_json::json!({"run_authoring_profile":{"file_write_tool":"file.write"},
            "model_hidden_tools":["file.write","gui.app_resume","declared.unselected"]})
            .as_object()
            .unwrap()
            .clone();
        replace_provider_hidden_tools(
            &mut private,
            "provider",
            &plan.authoring_tool_id,
            &plan.capability_tools,
        )
        .unwrap();
        assert_eq!(
            private["model_hidden_tools"],
            serde_json::json!([plan.authoring_tool_id, resume, "declared.unselected"])
        );
        private.insert(
            "model_hidden_tools".to_string(),
            serde_json::json!(["file.write", "gui.app_resume"]),
        );
        replace_provider_hidden_tools(
            &mut private,
            "provider",
            "file.write__home",
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            private["model_hidden_tools"],
            serde_json::json!(["file.write__home", "gui.app_resume"])
        );

        let interface_root = registry_root.join("interface");
        let config = load_client_config(&interface_root).unwrap();
        let chain = resolve_live_ee_policy_chain(ee_root, &EeId::new("ee_operator_space")).unwrap();
        let topology = select_capability_topology(
            form_capability_topology(
                registry_root,
                &CapabilityFormationRequest::from_live_chain(
                    InterfaceId::new("system_worker_computer_use"),
                    chain,
                ),
            )
            .unwrap(),
            &config,
            &request.selected_worker_environment_kinds,
            &[],
        )
        .unwrap();
        let binding = &topology.tool_bindings[&InterfaceId::new("system_gui")];
        let provider =
            resolve_provider(&interface_root, "world_provider_codex", "", "", None, &topology).unwrap();
        let identity = plan_identity(
            &config,
            &request.chat_id,
            &provider,
            &topology,
            &resolve_run_selection(&request, &config).unwrap(),
        )
        .unwrap();
        for invalid in [
            serde_json::json!({"resume_tool":"gui.app_resume"}),
            serde_json::json!({"operator_interface":"world_foreign","resume_tool":"gui.app_resume"}),
            serde_json::json!({"operator_interface":config.interface,"resume_tool":"file.write"}),
        ] {
            let mut candidate = source.clone();
            candidate["app_approval"] = invalid;
            assert!(
                plan_desktop_app_approval(&candidate.to_string(), binding, &config, &identity)
                    .is_err()
            );
        }

        let mut persistent_request = request;
        persistent_request.chat_id = "cu_persistent_identity".into();
        persistent_request.cycle = "cu_persistent_cycle".into();
        persistent_request.selected_worker_environment_kinds.push("code_mode".into());
        let isolated = plan_operator_chat_topology(&persistent_request).unwrap();
        let exact_json = |plan: &OperatorChatPlan, tag: String| -> serde_json::Value {
            serde_json::from_str(&plan.interface_writes.iter().find(|w| w.tag == tag).expect("exact planned source exists").content).unwrap()
        };
        let isolated_sidecar = exact_json(&isolated,format!("sidecar:{}:code_mode.json",isolated.capability_doors["system_code_mode"]));
        assert_eq!(isolated_sidecar["context"]["mode"],"isolated");
        assert!(isolated_sidecar.get("handoff").is_none());
        let standalone = std::fs::read(registry_root.join("interface/system_code_mode/code_mode.json")).unwrap();
        persistent_request.context_artifact_bindings.push(ArtifactBindingSeed { id:"artifact-code-mode-cua-prelude".into() });
        let payload_root = tempfile::tempdir().unwrap();
        let file = octopus_interfaces::InterfaceArtifactFile::write(payload_root.path().to_path_buf(), "fixture.payload", &[0xff,0x00,0x81]).unwrap();
        let content = octopus_ledger::ContentHash::of_bytes(&std::fs::read(file.manifest_path()).unwrap());
        let payload_ref = octopus_ledger::ArtifactRef { id:octopus_descriptors::ArtifactId::new(format!("fixture.payload:{}",content.to_hex())), content };
        persistent_request.payload_artifact_bindings = vec![payload_ref.clone()];
        let persistent = plan_operator_chat_topology(&persistent_request).expect("selected CU persistent owner plans through the real registry");
        assert_eq!(persistent.authoring_profile.payload_artifact_bindings, vec![payload_ref.clone()]);
        persistent_request.payload_artifact_bindings.push(payload_ref.clone());
        assert!(plan_operator_chat_topology(&persistent_request).is_err());
        persistent_request.payload_artifact_bindings = vec![octopus_ledger::ArtifactRef { id:octopus_descriptors::ArtifactId::new("artifact-code-mode-cua-prelude"), content }];
        assert!(plan_operator_chat_topology(&persistent_request).is_err());
        persistent_request.payload_artifact_bindings = vec![payload_ref.clone()];
        let owner = &persistent.capability_doors["system_code_mode"];
        let gui = &persistent.capability_doors["system_gui"];
        let sidecar = exact_json(&persistent,format!("sidecar:{owner}:code_mode.json"));
        assert_eq!(sidecar["context"]["mode"],"persistent");
        assert_eq!(sidecar["continuation"]["resume_tool"],persistent.capability_tools["code_mode.resume"]);
        let prelude = &sidecar["context"]["prelude_artifact"];
        let topology_ref = &sidecar["handoff"]["topology_artifact"];
        let owner_topology = exact_json(&persistent,format!("artifact:{}",topology_ref["id"].as_str().unwrap()));
        let prelude_body = exact_json(&persistent,format!("artifact:{}",prelude["id"].as_str().unwrap()));
        for (reference,body) in [(prelude,&prelude_body),(topology_ref,&owner_topology)] {
            let reference:octopus_ledger::ArtifactRef=serde_json::from_value(reference.clone()).unwrap();
            assert_eq!(body["id"],reference.id.as_str());
            assert_eq!(body["run"],persistent.run_id);
            assert_eq!(reference.content,octopus_ledger::ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(body)));
        }
        assert_eq!(owner_topology["prelude_artifact"],*prelude);
        assert_eq!(owner_topology["owner_interface"],*owner);
        assert_eq!(owner_topology["coauthor"]["tool"],persistent.authoring_tool_id);
        let writer_tool = owner_topology["file_write_tool"].as_str().unwrap();
        let writer = exact_json(&persistent,format!("tool:{writer_tool}"));
        assert_ne!(writer_tool,persistent.authoring_tool_id);
        assert_ne!(writer["interface"],persistent.authoring_environment_interface_id);
        assert!(persistent.required_tool_ids.iter().any(|tool| tool == writer_tool));
        let run:serde_json::Value=serde_json::from_str(&persistent.run_record_content).unwrap();
        let templates = run["interface_state"][&persistent.model_interface_id]["run_authoring_binding"]["checkpoints"].as_array().unwrap();
        let returned = templates.iter().filter(|cp| cp["role"] == "owner_input" && cp["interface_binding"]["interface"] == *owner).collect::<Vec<_>>();
        assert_eq!(returned.len(),1);
        assert_eq!(returned[0]["interface_binding"]["interface"],*owner);
        assert_eq!(returned[0]["outbound_interface_bindings"][0]["interface"],persistent.model_interface_id);
        assert_eq!(returned[0]["owner_authoring"]["tool"],writer_tool);
        assert_eq!(returned[0]["owner_authoring"]["door"],writer["interface"]);
        assert_eq!(returned[0]["include_context"], true);
        let mandatory = run["interface_state"][&persistent.model_interface_id]["run_authoring_binding"]["writer_run"]["initial_record"]["checkpoints"][0]["artifact_bindings"].as_array().unwrap();
        let mut selected_static = config.run.context_artifact_bindings.iter()
            .filter(|binding| !mandatory.iter().any(|role| role["id"] == binding.id))
            .map(|binding| serde_json::json!(binding.id)).collect::<Vec<_>>();
        selected_static.push(run["interface_state"][&persistent.model_interface_id]["collection_catalogue_adoption"]["snapshot"]["id"].clone());
        selected_static.push(serde_json::json!(format!("artifact-run-authoring-coordinates-{}", persistent_request.chat_id)));
        assert_eq!(returned[0]["static_artifact_bindings"], serde_json::json!(selected_static));
        assert_eq!(run["interface_state"][&persistent.model_interface_id]["run_authoring_binding"]["payload_artifact_bindings"], serde_json::json!([payload_ref]));
        let rows = run["checkpoints"].as_array().unwrap();
        let writer = &run["interface_state"][&persistent.model_interface_id]["run_authoring_binding"]["writer_run"];
        let authoring_row = writer["initial_record"]["checkpoints"].as_array().unwrap().iter().find(|cp| cp["interface_binding"]["interface"] == persistent.authoring_source_interface_id
            && cp["outbound_interface_bindings"][0]["interface"] == persistent.authoring_environment_interface_id).unwrap();
        let output_template = templates.iter().find(|cp| cp["role"] == "provider_tool_output"
            && cp["outbound_interface_bindings"][0]["interface"] == *owner).unwrap();
        let mut output_static = vec![prelude["id"].clone(),topology_ref["id"].clone(),serde_json::json!(payload_ref.id)];
        output_static.extend(selected_static);
        assert_eq!(output_template["static_artifact_bindings"],serde_json::json!(output_static));
        let mut exact_bindings = authoring_row["artifact_bindings"].as_array().unwrap().clone();
        exact_bindings.extend(output_template["static_artifact_bindings"].as_array().unwrap().iter().map(|id| serde_json::json!({"id":id})));
        let owner_call = rows.iter().find(|cp| cp["interface_binding"] == output_template["interface_binding"]
            && cp["outbound_interface_bindings"] == output_template["outbound_interface_bindings"]).unwrap();
        let bindings = |value: &serde_json::Value| serde_json::from_value::<Vec<ArtifactBinding>>(value.clone()).unwrap();
        assert_eq!(bindings(&owner_call["artifact_bindings"]), bindings(&serde_json::json!(exact_bindings)));
        for id in run["interface_state"][owner]["owned_checkpoint_ids"].as_array().unwrap() {
            let row = rows.iter().find(|cp| cp["id"] == *id).unwrap();
            assert_eq!(bindings(&row["artifact_bindings"]), bindings(&serde_json::json!(exact_bindings)), "owned execution checkpoint bindings differ from the exact provider tool output template");
        }
        let seed = ArtifactBindingSeed { id: "fixture.selected.context".into() };
        for inherit in [true, false] {
            let mut rebound = run.clone();
            let mut call = authoring_row.clone();
            call["run"] = rebound["id"].clone();
            rebound["active_checkpoints"].as_array_mut().unwrap().push(call["id"].clone());
            rebound["checkpoints"].as_array_mut().unwrap().push(call);
            let owned = rebound["interface_state"][owner]["owned_checkpoint_ids"].as_array().unwrap().clone();
            let templates = rebound["interface_state"][&persistent.model_interface_id]["run_authoring_binding"]["checkpoints"].as_array_mut().unwrap();
            let returned = templates.iter_mut().find(|cp| cp["role"] == "owner_input" && cp["interface_binding"]["interface"] == *owner).unwrap();
            returned["include_context"] = serde_json::json!(inherit);
            if !inherit { returned["static_artifact_bindings"] = output_template["static_artifact_bindings"].clone(); }
            let rights = serde_json::json!({"read":true,"change":true,"create":true,"delete":true});
            for row in rebound["checkpoints"].as_array_mut().unwrap().iter_mut().filter(|row| owned.contains(&row["id"])) {
                row["artifact_bindings"].as_array_mut().unwrap().iter_mut().find(|binding| binding["id"] == prelude["id"]).unwrap()["access"] = rights.clone();
            }
            context_bindings::bind_selected_context(&mut rebound, None, &persistent.model_interface_id, [&seed], []).unwrap();
            let returned = rebound["interface_state"][&persistent.model_interface_id]["run_authoring_binding"]["checkpoints"].as_array().unwrap().iter().find(|cp| cp["role"] == "owner_input" && cp["interface_binding"]["interface"] == *owner).unwrap();
            assert_eq!(returned["include_context"], inherit);
            assert!(returned["static_artifact_bindings"].as_array().unwrap().contains(&serde_json::json!(seed.id)));
            for row in rebound["checkpoints"].as_array().unwrap().iter().filter(|row| owned.contains(&row["id"])) {
                let actual = bindings(&row["artifact_bindings"]);
                assert_eq!(row["artifact_bindings"].as_array().unwrap().iter().find(|binding| binding["id"] == prelude["id"]).unwrap()["access"], rights);
                assert_eq!(actual.iter().find(|binding| binding.id.as_str() == seed.id).unwrap().access, octopus_runtime::ArtifactAccess::read_only());
            }
            let denied = rebound["checkpoints"].as_array_mut().unwrap().iter_mut().find(|row| owned.contains(&row["id"])).unwrap()["artifact_bindings"].as_array_mut().unwrap().iter_mut().find(|binding| binding["id"] == seed.id).unwrap();
            denied["access"] = serde_json::json!({"read":false,"create":true});
            assert!(context_bindings::bind_selected_context(&mut rebound, None, &persistent.model_interface_id, [&seed], []).unwrap_err().contains("cannot widen an existing denied read"));
        }
        assert!(!templates.iter().any(|cp| cp["interface_binding"]["interface"] == *gui || cp["outbound_interface_bindings"].as_array().unwrap().iter().any(|out| out["interface"] == *gui)));
        let provider = exact_json(&persistent,format!("interface:{}",persistent.model_interface_id));
        let hidden = provider["interface_driver_config"]["private_config"]["model_hidden_tools"].as_array().unwrap();
        let routes = owner_topology["routes"].as_array().unwrap();
        assert_nested_owner_returns(&persistent, &owner_topology);
        for (base,tool) in &persistent.capability_tools {
            let descriptor = exact_json(&persistent,format!("tool:{tool}"));
            if descriptor["interface"] == *gui {
                assert!(hidden.contains(&serde_json::json!(tool)));
                if base != "gui.app_resume" {
                    let route = routes.iter().find(|route| route["tool"] == *tool).expect("formed nested GUI method remains callable in the room");
                    assert_eq!(route["call"]["binding"]["outbound"][0]["interface"],*gui);
                    assert_eq!(route["app_approval"]["resume_tool"],persistent.capability_tools["gui.app_resume"]);
                    let result = &route["result"];
                    let cp = run["checkpoints"].as_array().unwrap().iter().find(|cp| cp["id"] == result["name"]).expect("nested return is initially formed");
                    assert_eq!(cp["interface_binding"],result["binding"]["inbound"]);
                    assert!(run["interface_state"][owner]["owned_checkpoint_ids"].as_array().unwrap().contains(&cp["id"]));
                    assert!(run["active_checkpoints"].as_array().unwrap().contains(&cp["id"]));
                }
            }
        }
        assert!(hidden.contains(&serde_json::json!(writer_tool)));
        assert!(hidden.contains(&serde_json::json!(persistent.capability_tools["code_mode.resume"])));
        assert!(!hidden.contains(&serde_json::json!(persistent.capability_tools["code_mode.run"])));
        let run_descriptor = exact_json(&persistent,format!("tool:{}",persistent.capability_tools["code_mode.run"]));
        assert!(run_descriptor["description"].as_str().unwrap().contains("prelude namespace"));
        assert_eq!(prelude_body["exports"],serde_json::json!(["cua"]));
        for cp in run["checkpoints"].as_array().unwrap() {
            assert!(!cp["artifact_bindings"].as_array().unwrap().iter().any(|binding| binding["id"] == "artifact-code-mode-cua-prelude"));
        }
        assert_eq!(standalone,std::fs::read(registry_root.join("interface/system_code_mode/code_mode.json")).unwrap());
    }

    fn assert_nested_owner_returns(plan: &OperatorChatPlan, topology: &serde_json::Value) {
        let exact = |tool: &str| -> ToolDescriptor {
            parse_strict(plan.interface_writes.iter().find(|write| write.tag == format!("tool:{tool}")).unwrap().content.as_bytes()).unwrap()
        };
        let routes = topology["routes"].as_array().unwrap();
        let mut groups = BTreeMap::<String, (serde_json::Value, BTreeSet<String>)>::new();
        for route in routes {
            let descriptor = exact(route["tool"].as_str().unwrap());
            assert_eq!(route["process_descriptor"], serde_json::to_value(&descriptor).unwrap());
            let process = nested_process_owner(&descriptor).unwrap();
            let name = format!("nested_result_{}", octopus_descriptors::DescriptorHash::of_canonical_json(&process).to_hex());
            assert_eq!(route["result"]["name"], name);
            let group = groups.entry(name).or_insert_with(|| (process.clone(), BTreeSet::new()));
            assert_eq!(group.0, process);
            group.1.insert(format!("{}.result", descriptor.id));
            if !route["app_approval"].is_null() {
                let resume = exact(route["app_approval"]["resume_tool"].as_str().unwrap());
                assert_eq!(route["app_approval"]["resume_descriptor"], serde_json::to_value(&resume).unwrap());
                assert_eq!(nested_process_owner(&resume).unwrap(), process);
                group.1.insert(format!("{}.result", resume.id));
            }
        }
        let mut disjoint = BTreeSet::new();
        let run: serde_json::Value = serde_json::from_str(&plan.run_record_content).unwrap();
        for (name, (owner, labels)) in groups {
            assert!(labels.iter().all(|label| disjoint.insert((owner["interface"].clone().to_string(), label.clone()))));
            let row = run["checkpoints"].as_array().unwrap().iter().find(|row| row["id"] == name).unwrap();
            let policy = serde_json::json!({"mode":"allow","names":labels,"namespaces":[]});
            for route in routes.iter().filter(|route| route["result"]["name"] == name) {
                assert_eq!(route["result"]["binding"]["inbound"]["config"]["artifact_policy"], policy);
                assert_eq!(route["result"]["binding"]["outbound"][0]["config"]["artifact_policy"], policy);
                assert_eq!(row["interface_binding"], route["result"]["binding"]["inbound"]);
                assert_eq!(row["outbound_interface_bindings"], route["result"]["binding"]["outbound"]);
            }
        }
    }

    #[test]
    fn task_agent_nested_selection_survives_model_change_and_current_amendment() {
        use crate::run_composition::{CompositionRoots, current_run::*};
        use octopus_provider_run_authoring::capability_amendment::ProviderCapabilityBridge;
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let registry = root.join("registry");
        copy_planner_descriptors(&repository_registry_root().join("registry"), &registry);
        let ee = root.join("ee"); std::fs::create_dir(&ee).unwrap(); boot_operator_space_ee_instances(&ee);
        let agent = "system_worker_template_nested_agent";
        for file in ["interface.json", "policy.json"] {
            let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(registry.join("interface/system_worker_operator_task").join(file)).unwrap()).unwrap();
            value["id"] = serde_json::json!(agent);
            if file == "policy.json" { for id in value["ceiling"]["interfaces"].as_array_mut().unwrap() {
                if id == "system_worker_operator_task" { *id = serde_json::json!(agent); }
            } }
            let path = registry.join("interface").join(agent).join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap(); std::fs::write(path, pretty_json(&value).unwrap()).unwrap();
        }
        let topology = form_capability_topology(&registry, &CapabilityFormationRequest::from_live_chain(InterfaceId::new(agent), resolve_live_ee_policy_chain(&ee, &EeId::new("ee_existence")).unwrap())).unwrap();
        let selected = topology.tool_bindings.iter().flat_map(|(interface,binding)| binding.tools.iter().map(|tool| SelectedToolSeed {
            tool:tool.tool.to_string(), interface:interface.to_string(), worker_environment_kind:tool.worker_environment_kind.to_string(),
        })).collect::<Vec<_>>();
        let mut request: OperatorChatPlanRequest = serde_json::from_value(serde_json::json!({
            "registry_root":registry,"ee_instances_root":ee,"run_records_root":root.join("runs"),"interface_private_state_root":root.join("private"),
            "write_hash_algorithm":"sha256","chat_id":"nested_agent_task","provider_interface_id":"world_provider_codex","model":"gpt-6.1-sol","reasoning_effort":"xhigh",
            "ee_id":"ee_existence","worker_interface_id":agent,"client_surface_role":"task","cycle":"first",
            "selected_worker_environment_kinds":["browser_session","code_mode","desktop_session","system_room"],"selected_tools":selected,
            "context_artifact_bindings":[{"id":"artifact-code-mode-browser-prelude"},{"id":"artifact-code-mode-cua-prelude"}],
            "conversation":{"ledger_offset":0,"artifact_bindings":[],"pinned_artifact_bindings":[],"pinned_context_items":[],"continued_from":null}
        })).unwrap();
        let install = |plan: &OperatorChatPlan| {
            for write in plan.interface_writes.iter().chain(&plan.run_writes) {
                std::fs::create_dir_all(write.path.parent().unwrap()).unwrap(); std::fs::write(&write.path,&write.content).unwrap();
            }
            std::fs::create_dir_all(root.join("runs")).unwrap();
            std::fs::write(root.join("runs").join(format!("{}.json",plan.run_id)),&plan.run_record_content).unwrap();
        };
        let initial = plan_operator_chat_topology(&request).unwrap(); install(&initial);
        let roots = CompositionRoots {registry_root:registry.clone(),ee_instances_root:ee.clone(),run_records_root:root.join("runs"),interface_private_state_root:root.join("private")};
        let provider = InterfaceId::new(&initial.model_interface_id);
        let mut current = RunRecord::from_bytes(initial.run_record_content.as_bytes()).unwrap();
        let first = current_selection(&roots,&current,&provider).unwrap().unwrap();
        assert_eq!(first.selected_tools.len(),selected.len());
        assert!(selected.iter().all(|tool|first.selected_tools.contains(tool)));
        request.existing_run_record = Some(serde_json::to_value(&current).unwrap());
        request.retain_current_capabilities = true; request.cycle = "model_change".into(); request.model = Some("gpt-6-astra".into());
        let changed = plan_operator_chat_topology(&request).unwrap();
        assert_eq!(changed.capability_tools,initial.capability_tools); assert_eq!(changed.capability_doors,initial.capability_doors);
        install(&changed); current = RunRecord::from_bytes(changed.run_record_content.as_bytes()).unwrap();
        assert_eq!(current_selection(&roots,&current,&provider).unwrap().unwrap().selected_tools,first.selected_tools);
        let door = InterfaceId::new(&changed.capability_doors["system_room"]);
        let bridge = ProviderCapabilityBridge {tool:changed.capability_tools["file.read"].clone(),correlation_id:"nested-read".into(),arguments_hash:octopus_ledger::ContentHash::of_bytes(b"{}").to_hex(),return_interface:door.clone()};
        let retained = [(changed.capability_tools["file.read"].clone(),door)];
        let amendment = plan_current_run_amendment(&roots,&current,&provider,CurrentRunRequest {amendment_key:"nested-retain".into(),selection:Some(first.clone()),authored:None,publications:vec![]},bridge.clone(),&retained).unwrap();
        assert!(amendment.validation.safe_to_apply,"{:?}",amendment.validation.regressions);
        let result_mark = current.interface_state[&provider]["run_authoring_binding"]["checkpoints"].as_array().unwrap().iter()
            .find_map(|cp|cp["result_receiving"][&bridge.tool]["mark"].as_str()).unwrap();
        let policy_path = registry.join("interface").join(current.worker.as_str()).join("policy.json");
        let policy_bytes = std::fs::read(&policy_path).unwrap();
        let mut narrowed:serde_json::Value = serde_json::from_slice(&policy_bytes).unwrap();
        narrowed["ceiling"]["mark_outcomes"].as_array_mut().unwrap().retain(|mark|mark.as_str()!=Some(result_mark));
        let current_bytes = std::fs::read(roots.run_records_root.join(format!("{}.json",current.id))).unwrap();
        let authored = AuthoredCurrentRun {expected_before_hash:octopus_ledger::ContentHash::of_bytes(&current_bytes).to_hex(),
            expected_before:serde_json::json!(current),run_record:serde_json::json!(current),writes:vec![crate::run_composition::RegistryCandidateWrite {
                path:policy_path.clone(),content:narrowed.to_string(),expected_before_hash:Some(octopus_ledger::ContentHash::of_bytes(&policy_bytes).to_hex()),
            }]};
        assert!(plan_current_run_amendment(&roots,&current,&provider,CurrentRunRequest {amendment_key:"nested-mark-denied".into(),selection:None,authored:Some(authored),publications:vec![]},bridge,&retained)
            .unwrap_err().contains("removes the bridge result mark"));
        assert_eq!(std::fs::read(policy_path).unwrap(),policy_bytes);
        let after = RunRecord::from_bytes(&serde_json::to_vec(&amendment.amendment.after).unwrap()).unwrap();
        assert_eq!(current_selection(&roots,&after,&provider).unwrap().unwrap().selected_tools,first.selected_tools);
        let owner = &changed.capability_doors["system_code_mode"];
        let owner_state = &current.interface_state[&InterfaceId::new(owner)];
        assert_eq!(after.interface_state[&InterfaceId::new(owner)],*owner_state);
        let nested = registry.join("tool").join(&changed.capability_tools["browser.navigate"]).join("tool.json");
        let before = std::fs::read(&nested).unwrap();
        let mut descriptor: serde_json::Value = serde_json::from_slice(&before).unwrap(); descriptor["description"] = serde_json::json!("Changed after accepted selection");
        std::fs::write(&nested,pretty_json(&descriptor).unwrap()).unwrap();
        assert!(current_selection(&roots,&current,&provider).unwrap_err().contains("granted exact descriptor"));
        std::fs::write(&nested,before).unwrap();
        let mut unbound = current.clone();
        for checkpoint in &mut unbound.checkpoints {
            checkpoint.artifact_bindings.retain(|artifact| !artifact.id.as_str().starts_with("artifact-code-mode-topology__"));
        }
        assert!(current_selection(&roots,&unbound,&provider).unwrap_err().contains("not readable"));
        let mut wrong_return = current.clone();
        for checkpoint in &mut wrong_return.checkpoints {
            if checkpoint.inbound_interface().is_some_and(|id| id.as_str() == changed.capability_doors["system_browser"])
                && checkpoint.outbound_interfaces().any(|id| id.as_str() == owner) {
                checkpoint.outbound_interface_bindings = vec![AuthoredInterfaceBinding::all(InterfaceId::new(owner))];
            }
        }
        assert!(current_selection(&roots,&wrong_return,&provider).unwrap_err().contains("exact accepted call and return"));
    }

    fn assert_home_and_task_browser_cu_profile(registry_root: &Path, ee_root: &Path) {
        let repo = registry_root.parent().unwrap();
        let read = |path: &str| -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(repo.join(path)).unwrap()).unwrap()
        };
        let pack = read("app/distribution/local-packages/packs/codex.json");
        let home = &pack["workspace_setup"]["home"];
        let kinds = home["selected_worker_environment_kinds"].as_array().unwrap().iter().map(|v|v.as_str().unwrap()).collect::<BTreeSet<_>>();
        assert_eq!(kinds,BTreeSet::from(["browser_session","desktop_session","code_mode","filesystem_workspace","system_room"]));
        for artifact in ["artifact-worker-role-template","artifact-grounding-system-architecture","artifact-codebase-layout","artifact-code-mode-browser-prelude","artifact-code-mode-cua-prelude"] {
            assert!(home["context_artifact_bindings"].as_array().unwrap().iter().any(|reference|reference["id"]==artifact));
        }
        for package in ["driver.browser_session", "driver.desktop_session"] {
            assert!(pack["components"].as_array().unwrap().iter().any(|component| component["package_id"] == package && component["selection"] == "included"));
            assert!(pack["workspace_setup"]["project_defaults"].as_array().unwrap().iter().any(|entry| entry["package_id"] == package && entry["default_for_new_tasks"] == true));
        }
        assert_eq!(pack["workspace_setup"]["agent_presets"].as_array().unwrap().iter().map(|preset| preset["id"].as_str().unwrap()).collect::<Vec<_>>(), vec!["computer-use"]);
        let manifest = read("app/distribution/local-packages/packages/world_operator_chat.json");
        let required = manifest["required_packages"].as_array().unwrap().iter().map(|v|v.as_str().unwrap()).collect::<BTreeSet<_>>();
        assert_eq!(required,BTreeSet::from(["driver.browser_session","driver.desktop_session","driver.filesystem_workspace","driver.system_room","driver.code_mode","artifact.codex_context","world_provider_realtime","system_memory"]));
        let desktop = read("app/distribution/local-packages/packages/driver.desktop_session.json");
        assert!(!desktop["required_packages"].as_array().unwrap().contains(&serde_json::json!("world_operator_chat")), "the client-to-desktop package dependency cannot cycle back to the client");
        let driver = read("app/distribution/local-packages/packages/driver.code_mode.json");
        assert_eq!(driver["required_packages"], serde_json::json!(["driver.system_room"]));
        for (kind, owner) in [("interface", "system_code_mode"), ("worker_environment_kind", "code_mode"), ("artifact", "artifact-code-mode-browser-prelude"), ("tool", "code_mode.run"), ("tool", "code_mode.wait"), ("tool", "code_mode.resume"), ("tool", "code_mode.cancel")] {
            assert!(driver["members"].as_array().unwrap().iter().any(|member| member["registry_kind"] == kind && member["owner"] == owner));
        }
        assert!(!driver["required_packages"].as_array().unwrap().contains(&serde_json::json!("world_operator_chat")));
        let source_policy = read("registry/interface/system_worker_operator_home/policy.json");
        assert_eq!(source_policy["role_artifacts"], serde_json::json!(["artifact-worker-role-template", "artifact-grounding-system-architecture", "artifact-worker-role-operator-home", "artifact-home-task-authoring", "artifact-spawn-literacy", "artifact-home-widget-authoring", "artifact-context-collection-guide", "artifact-memory-guide"]));
        let overlay = tempfile::tempdir().unwrap();
        copy_planner_descriptors(registry_root, overlay.path());
        let registry_root = overlay.path();
        let instruction_content = octopus_ledger::ContentHash::of_bytes(b"Home instruction source");
        let instruction_source = octopus_ledger::ArtifactRef {
            id: format!("instruction.text:{}", instruction_content.to_hex()).into(),
            content: instruction_content,
        };
        std::fs::write(
            registry_root.join("artifact/artifact-worker-role-template/artifact.json"),
            serde_json::to_vec(&serde_json::json!({"id":"artifact-worker-role-template","label":"context.reference","sources":[instruction_source]})).unwrap(),
        ).unwrap();
        let make_request = |profile: &OctopusOsAuthoringProfile, role: &str, id: &str| -> OperatorChatPlanRequest {
            serde_json::from_value(serde_json::json!({
                "registry_root":registry_root,"ee_instances_root":ee_root,"run_records_root":ee_root.join("runs"),"write_hash_algorithm":"sha256",
                "interface_private_state_root":ee_root.join("interface-private"),
                "chat_id":id,"provider_interface_id":home["provider_interface_id"],"ee_id":profile.ee_id,
                "worker_interface_id":profile.worker_choice_interface,"client_surface_role":role,"cycle":format!("{id}_cycle"),
                "door_host_slots":profile.door_host_slots,"external_inputs":profile.external_inputs,"external_relays":profile.external_relays,
                "model_outputs":profile.model_outputs,"context_artifact_bindings":profile.context_artifact_bindings,"payload_artifact_bindings":profile.payload_artifact_bindings,
                "selected_worker_environment_kinds":profile.selected_worker_environment_kinds,"selected_tools":profile.selected_tools,
                "conversation":{"ledger_offset":0,"artifact_bindings":[],"pinned_artifact_bindings":[],"pinned_context_items":[],"continued_from":null}
            })).unwrap()
        };
        let source_profile: OctopusOsAuthoringProfile = serde_json::from_value(serde_json::json!({
            "ee_id":"ee_existence","worker_choice_interface":home["worker_interface_id"],"door_host_slots":home["door_host_slots"],
            "context_artifact_bindings":home["context_artifact_bindings"],"selected_worker_environment_kinds":home["selected_worker_environment_kinds"]
        })).unwrap();
        let mut inherited = source_profile.clone();
        let mut parent = "ee_existence".to_owned();
        let mut planned_ids = BTreeSet::new();
        for (role, id) in [("home", "browser_home_pack"), ("task", "browser_project_pack"), ("task", "browser_nested_project_pack")] {
            if role == "task" {
                let ee_id = format!("ee_{id}");
                let ee = octopus_ee::EeInstance { ee_id:EeId::new(&ee_id), template:octopus_descriptors::EeTemplateId::new("project"), parent_ee:Some(EeId::new(&parent)), goal:"Declared Browser/CU profile inheritance".into(), config:Default::default(), policy_narrowing:Default::default() };
                std::fs::write(ee_root.join(format!("{ee_id}.json")), serde_json::to_vec_pretty(&ee).unwrap()).unwrap();
                inherited.ee_id = ee_id.clone();
                parent = ee_id;
            }
            validate_octopus_os_authoring_profile(&inherited, None).unwrap();
            let mut request = make_request(&inherited, role, id);
            request.ee_policy_root = Some(ee_root.to_path_buf());
            request.ee_instances_root = ee_root.join("published-ee-instances");
            let plan = plan_operator_chat_topology(&request).expect("actual Home/task chain forms the declared Browser/CU defaults");
            let provider: serde_json::Value = serde_json::from_str(&plan.interface_writes.iter()
                .find(|write| write.tag == format!("interface:{}", plan.model_interface_id)).unwrap().content).unwrap();
            let native = &provider["interface_driver_config"]["private_config"]["run_affordances"];
            assert_eq!(native["file_read_tool"], serde_json::json!(plan.capability_tools["file.read"]));
            let catalog_id = format!("artifact-run-affordances-{id}");
            let catalog: serde_json::Value = serde_json::from_str(&plan.interface_writes.iter()
                .find(|write| write.tag == format!("artifact:{catalog_id}")).unwrap().content).unwrap();
            assert_eq!(native["catalog_artifact"], serde_json::json!(octopus_ledger::ArtifactRef {
                id: catalog_id.as_str().into(),
                content: octopus_ledger::ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&catalog)),
            }));
            assert_eq!(native["roots"]["ee_instances_root"], serde_json::json!(request.ee_instances_root));
            assert_eq!(native["file_write_tool"], serde_json::json!(plan.capability_tools["file.write"]));
            assert_ne!(native["file_write_tool"], serde_json::json!(plan.authoring_tool_id));
            let authored = RunRecord::from_bytes(plan.run_record_content.as_bytes()).unwrap();
            assert!(authored.checkpoints.iter().filter(|cp| cp.interface_binding.as_ref()
                .is_some_and(|binding| binding.interface.as_str() == plan.model_interface_id))
                .any(|cp| cp.artifact_bindings.iter().any(|binding| binding.id.as_str() == catalog_id && binding.access.read)));
            if role == "home" {
                let delivery: serde_json::Value = serde_json::from_str(&plan.interface_writes.iter()
                    .find(|write| write.tag == format!("artifact:artifact-home-delivery-{id}")).unwrap().content).unwrap();
                assert_eq!(delivery["delivery"]["authoring"]["ee_instances_root"], serde_json::json!(request.ee_instances_root));
                assert!(delivery["text"].as_str().unwrap().contains(&request.ee_instances_root.display().to_string()));
                let record = RunRecord::from_bytes(plan.run_record_content.as_bytes()).unwrap();
                let topology: BoundRunAuthoringTopology = serde_json::from_value(record.interface_state[&InterfaceId::new(&plan.model_interface_id)]["run_authoring_binding"].clone()).unwrap();
                for label in ["client.surface.document", "client.action", "client.action.result", "realtime.session.update"] {
                    let template = topology.checkpoints.iter().find(|cp| cp.role == BoundCheckpointRole::IndependentInput && cp.interface_binding.admits(label)
                        && cp.outbound_interface_bindings.iter().any(|binding| binding.admits(label))).unwrap();
                    assert_eq!(template.include_context, label == "client.action.result");
                    assert!(template.static_artifact_bindings.contains(&instruction_source.id));
                    if label != "realtime.session.update" {
                        assert!(topology.checkpoints.iter().filter(|cp| cp.interface_binding.interface == template.interface_binding.interface)
                            .all(|cp| ["client.surface.document", "client.action", "client.action.result"].into_iter().all(|name| cp.interface_binding.admits(name))));
                    }
                    let checkpoint = record.checkpoints.iter().find(|cp| cp.interface_binding.as_ref() == Some(&template.interface_binding)
                        && cp.outbound_interface_bindings == template.outbound_interface_bindings).unwrap();
                    assert!(checkpoint.artifact_bindings.iter().any(|binding| binding.id == instruction_source.id && binding.access.read));
                }
            }
            assert_eq!(plan.authoring_profile, inherited);
            assert!(planned_ids.insert(plan.capability_doors["system_code_mode"].clone()));
            let gui = &plan.capability_doors["system_gui"];
            let browser = &plan.capability_doors["system_browser"];
            assert_ne!(gui,browser);
            assert!(plan.capability_tools.contains_key("gui.app_state"));
            assert!(plan.capability_tools.contains_key("browser.navigate"));
            let exact = |tag: String| -> serde_json::Value {
                serde_json::from_str(&plan.interface_writes.iter().find(|write|write.tag == tag).unwrap().content).unwrap()
            };
            let mcp_tool = exact(format!("tool:{}", plan.capability_tools["client.mcp"]));
            assert_eq!(mcp_tool["handler_config"], read("registry/tool/client.mcp/tool.json")["handler_config"]);
            let worker_policy = exact(format!("policy:{}", plan.worker_interface_id));
            let workspace = exact(format!("interface:{}", plan.capability_doors["system_workspace_env"]));
            for slot in ["workspace_root", "host_filesystem_root"] {
                assert_eq!(workspace["interface_driver_config"]["host_slots"][slot]["path"], "/");
            }
            for tool in ["workspace.read_file", "workspace.write_file", "workspace.exec", "workspace.write_stdin", "workspace.process.terminate"] {
                assert!(worker_policy["ceiling"]["tools"].as_array().unwrap().contains(&serde_json::json!(plan.capability_tools[tool])));
            }
            assert!(worker_policy["ceiling"]["tools"].as_array().unwrap().contains(&serde_json::json!(plan.capability_tools["client.mcp"])));
            let sidecar = exact(format!("octopus-os:{}", plan.chat_interface_id));
            let parsed: OctopusOsInterfaceSidecar = serde_json::from_value(sidecar.clone()).unwrap();
            assert_eq!(parsed.surface_role, role);
            assert_eq!(parsed.authoring_profile.as_ref(), Some(&inherited));
            let owner = &plan.capability_doors["system_code_mode"];
            let code_mode = exact(format!("sidecar:{owner}:code_mode.json"));
            assert_eq!(code_mode["context"]["mode"], "persistent");
            let prelude = exact(format!("artifact:{}",code_mode["context"]["prelude_artifact"]["id"].as_str().unwrap()));
            assert_eq!(prelude["exports"].as_array().unwrap().iter().map(|v|v.as_str().unwrap()).collect::<BTreeSet<_>>(),BTreeSet::from(["browser","cua"]));
            let gui_config=exact(format!("sidecar:{gui}:desktop.json"));
            assert_eq!(gui_config["app_approval"]["operator_interface"],plan.chat_interface_id);
            assert_eq!(gui_config["app_approval"]["resume_tool"],plan.capability_tools["gui.app_resume"]);
            let topology=exact(format!("artifact:{}",code_mode["handoff"]["topology_artifact"]["id"].as_str().unwrap()));
            for (initial,resume) in [("gui.app_state","gui.app_resume"),("browser.filechooser_set_files","browser.filechooser_resume")] {
                let route=topology["routes"].as_array().unwrap().iter().find(|route|route["tool"]==plan.capability_tools[initial]).unwrap();
                assert_eq!(route["app_approval"]["operator_interface"],plan.chat_interface_id);
                assert_eq!(route["app_approval"]["resume_tool"],plan.capability_tools[resume]);
            }
            assert!(prelude["module_source"].as_str().unwrap().contains("api.documentation"));
            let run_tool = exact(format!("tool:{}",plan.capability_tools["code_mode.run"]));
            assert!(run_tool["description"].as_str().unwrap().contains("prelude.browser"));
            assert!(run_tool["description"].as_str().unwrap().contains("documentation()"));
            let run:serde_json::Value = serde_json::from_str(&plan.run_record_content).unwrap();
            let provider_context = &run["checkpoints"];
            for artifact in ["artifact-worker-role-template", "artifact-grounding-system-architecture", "artifact-codebase-layout"] {
                assert!(provider_context.as_array().unwrap().iter().filter(|cp|cp["outbound_interface_bindings"].as_array().unwrap().iter().any(|binding|binding["interface"] == plan.model_interface_id)).any(|cp|cp["artifact_bindings"].as_array().unwrap().iter().any(|binding|binding["id"] == artifact)), "Home retains original model context {artifact}");
            }
            assert!(!run.to_string().contains("artifact-worker-role-web"));
            for write in plan.interface_writes.iter().chain(&plan.run_writes) {
                assert!(write.path.starts_with(registry_root) || (write.path.parent() == Some(ee_root.join("runs").as_path()) && write.tag.starts_with("run:") && write.expected_before_hash.is_none()));
                std::fs::create_dir_all(write.path.parent().unwrap()).unwrap();
                std::fs::write(&write.path,&write.content).unwrap();
            }
            let sidecar_path = registry_root.join("interface").join(&plan.chat_interface_id).join("octopus_os.json");
            let mut saved_sidecar = sidecar.clone();
            if role == "home" {
                let mut task_profile = serde_json::to_value(&inherited).unwrap();
                for field in ["external_inputs", "external_relays", "model_outputs", "selected_tools", "payload_artifact_bindings"] {
                    task_profile[field] = serde_json::json!([]);
                }
                saved_sidecar["authoring_profile"] = task_profile.clone();
                saved_sidecar["task_authoring_profiles"] = serde_json::json!([task_profile]);
                saved_sidecar["presentation"] = serde_json::json!({"name":"", "projects":[]});
                std::fs::write(&sidecar_path, pretty_json(&saved_sidecar).unwrap()).unwrap();
            }
            let mut continuation=make_request(&inherited,role,id);
            continuation.cycle=format!("{id}_next");
            let mut current_context = run.clone();
            let binding: BoundRunAuthoringTopology = serde_json::from_value(run["interface_state"][&plan.model_interface_id]["run_authoring_binding"].clone()).unwrap();
            let history = serde_json::to_value([
                ArtifactBinding { id:octopus_descriptors::ArtifactId::new("turn.interrupted.result:current"), access:Default::default() },
                ArtifactBinding { id:octopus_descriptors::ArtifactId::new("tool.result:current-tail"), access:octopus_policy::ArtifactAccess { change:true, ..Default::default() } },
            ]).unwrap();
            for checkpoint in current_context["checkpoints"].as_array_mut().unwrap() {
                if run["active_checkpoints"].as_array().unwrap().contains(&checkpoint["id"])
                    && binding.checkpoints.iter().any(|template| template.role == BoundCheckpointRole::Stable && template.include_context
                        && checkpoint_matches_bound_template(checkpoint, template, &plan.run_id).unwrap()) {
                    checkpoint["artifact_bindings"].as_array_mut().unwrap().extend(history.as_array().unwrap().clone());
                }
            }
            continuation.conversation.artifact_bindings = vec![ArtifactBindingSeed { id:"interface.artifact.output:stale".into() }];
            continuation.existing_run_record=Some(current_context);
            let next=plan_operator_chat_topology(&continuation).expect("ordinary existing Home/task continues the same selected Browser/CU path");
            let next_record = RunRecord::from_bytes(next.run_record_content.as_bytes()).unwrap();
            let context = octopus_provider_run_authoring::provider_context_bindings(&next_record, &InterfaceId::new(&plan.model_interface_id)).unwrap();
            assert_eq!(serde_json::to_value(&context[context.len()-2..]).unwrap(), history);
            assert!(!context.iter().any(|binding| binding.id.as_str() == "interface.artifact.output:stale"));
            assert_eq!(next.run_id, plan.run_id);
            assert_eq!(next.chat_interface_id, plan.chat_interface_id);
            assert_eq!(next.model_interface_id, plan.model_interface_id);
            assert_eq!(next.worker_interface_id, plan.worker_interface_id);
            assert_eq!(next.capability_doors, plan.capability_doors);
            assert_eq!(next.capability_tools, plan.capability_tools);
            assert_eq!(next.authoring_profile, inherited);
            assert_eq!(serde_json::to_value(&next).unwrap()["authoring_profile"], saved_sidecar["authoring_profile"]);
            assert!(!next.interface_writes.iter().any(|write| write.path == sidecar_path), "ordinary continuation must not rewrite unrelated client configuration");
            if role == "home" {
                use crate::run_composition::{CompositionRoots, current_run::*};
                use octopus_provider_run_authoring::capability_amendment::ProviderCapabilityBridge;
                let roots = CompositionRoots { registry_root:registry_root.to_path_buf(), ee_instances_root:request.ee_instances_root.clone(),
                    run_records_root:ee_root.join("runs"), interface_private_state_root:ee_root.join("interface-private") };
                std::fs::create_dir_all(&roots.ee_instances_root).unwrap();
                boot_operator_space_ee_instances(&roots.ee_instances_root);
                std::fs::write(roots.run_records_root.join(format!("{}.json",plan.run_id)), &plan.run_record_content).unwrap();
                let current = RunRecord::from_bytes(plan.run_record_content.as_bytes()).unwrap();
                let provider = InterfaceId::new(&plan.model_interface_id);
                let selection = current_selection(&roots,&current,&provider).unwrap().unwrap();
                for tool in ["context.discover", "context.read", "context.prepare_publication"] {
                    assert!(selection.selected_tools.iter().any(|selected| selected.tool == tool));
                }
                let voice: home_voice::HomeVoiceContext = serde_json::from_value(run["interface_state"][&plan.chat_interface_id]["voice_context"].clone()).unwrap();
                let voice_paths = [roots.run_records_root.join(format!("{}.json",voice.run_id)),
                    registry_root.join("interface").join(&voice.provider_id).join("interface.json"),
                    registry_root.join("interface").join(&voice.provider_id).join("life.json")];
                let accepted_voice = voice_paths.iter().map(|path| (path.clone(),std::fs::read(path).unwrap())).collect::<BTreeMap<_,_>>();
                let source_path = registry_root.join("interface/world_provider_realtime/interface.json");
                let source_bytes = std::fs::read(&source_path).unwrap();
                let mut source: serde_json::Value = serde_json::from_slice(&source_bytes).unwrap();
                source["interface_driver_config"]["private_config"]["session"]["audio"]["output"]["voice"] = serde_json::json!("changed source default");
                std::fs::write(&source_path,pretty_json(&source).unwrap()).unwrap();
                let mut retained = make_request(&inherited,role,id);
                retained.existing_run_record = Some(run.clone()); retained.retain_current_capabilities = true;
                let retained = plan_operator_chat_topology(&retained).unwrap();
                assert!(!retained.interface_writes.iter().any(|write| write.tag == format!("policy:{}",plan.worker_interface_id)));
                assert!(!retained.interface_writes.iter().chain(&retained.run_writes).any(|write| accepted_voice.contains_key(&write.path)));
                let door = InterfaceId::new(&plan.capability_doors["system_room"]);
                let bridge = ProviderCapabilityBridge { tool:plan.capability_tools["file.read"].clone(),correlation_id:"home-read".into(),
                    arguments_hash:octopus_ledger::ContentHash::of_bytes(b"{}").to_hex(),return_interface:door.clone() };
                let amend = |record: &RunRecord| plan_current_run_amendment(&roots,record,&provider,CurrentRunRequest {
                    amendment_key:"home-context-retained".into(),selection:Some(selection.clone()),authored:None,publications:vec![],
                },bridge.clone(),&[(plan.capability_tools["file.read"].clone(),door.clone())]);
                let preview = amend(&current).unwrap();
                assert!(preview.validation.safe_to_apply,"{:?}",preview.validation.regressions);
                assert_eq!(preview.amendment.after,amend(&current).unwrap().amendment.after);
                assert_eq!(preview.amendment.after["interface_state"][&plan.model_interface_id]["run_authoring_binding"],run["interface_state"][&plan.model_interface_id]["run_authoring_binding"]);
                assert!(!preview.dependency_writes.iter().chain(&preview.activation_writes).any(|write| accepted_voice.contains_key(&write.path)));
                for (path,bytes) in &accepted_voice { assert_eq!(&std::fs::read(path).unwrap(),bytes); }
                let policy_path = registry_root.join("interface").join(&plan.worker_interface_id).join("policy.json");
                let policy_bytes = std::fs::read(&policy_path).unwrap();
                let mut policy: serde_json::Value = serde_json::from_slice(&policy_bytes).unwrap();
                assert!(!policy["ceiling"]["interfaces"].as_array().unwrap().contains(&serde_json::json!("world_provider_realtime")));
                policy["ceiling"]["interfaces"].as_array_mut().unwrap().retain(|id| id != &serde_json::json!(voice.provider_id));
                if let Some(policies) = policy["ceiling"]["interface_policy"].as_object_mut() { policies.remove(&voice.provider_id); }
                std::fs::write(&policy_path,pretty_json(&policy).unwrap()).unwrap();
                assert!(amend(&current).unwrap_err().contains("Home voice provider is absent"));
                std::fs::write(&policy_path,policy_bytes).unwrap();
                let mut wrong = current.clone();
                wrong.interface_state.get_mut(&InterfaceId::new(&plan.chat_interface_id)).unwrap().get_mut("voice_context").unwrap()["provider_id"] = serde_json::json!("world_provider_realtime");
                assert!(amend(&wrong).unwrap_err().contains("declared source identity"));
                std::fs::write(&source_path,source_bytes).unwrap();
                continuation.context_artifact_bindings.push(ArtifactBindingSeed { id: "artifact-secret-handle-flow".into() });
                let amended = plan_operator_chat_topology(&continuation).unwrap();
                let edited: serde_json::Value = serde_json::from_str(&amended.interface_writes.iter().find(|write| write.path == sidecar_path).unwrap().content).unwrap();
                for field in ["task_authoring_profiles", "presentation"] {
                    assert_eq!(edited[field], saved_sidecar[field], "capability amendment preserves the authored {field}");
                }
                let mut expected_profile = saved_sidecar["authoring_profile"].clone();
                expected_profile["context_artifact_bindings"] = serde_json::to_value(&continuation.context_artifact_bindings).unwrap();
                assert_eq!(edited["authoring_profile"], expected_profile);
                assert_eq!(serde_json::to_value(&amended).unwrap()["authoring_profile"], expected_profile);
                saved_sidecar["task_authoring_profiles"][0]["undeclared"] = serde_json::json!(true);
                std::fs::write(&sidecar_path, pretty_json(&saved_sidecar).unwrap()).unwrap();
                assert!(plan_operator_chat_topology(&continuation).unwrap_err().contains("unknown field"));
                saved_sidecar["task_authoring_profiles"][0].as_object_mut().unwrap().remove("undeclared");
                saved_sidecar["presentation"] = serde_json::Value::Null;
                std::fs::write(&sidecar_path, pretty_json(&saved_sidecar).unwrap()).unwrap();
                continuation.context_artifact_bindings = inherited.context_artifact_bindings.clone();
                assert!(!plan_operator_chat_topology(&continuation).unwrap().interface_writes.iter().any(|write| write.path == sidecar_path));
            }
            let ee_path = ee_root.join(format!("{}.json", inherited.ee_id));
            let ee_bytes = std::fs::read(&ee_path).unwrap();
            let mut current_ee: octopus_ee::EeInstance = serde_json::from_slice(&ee_bytes).unwrap();
            current_ee.policy_narrowing.allowed_worker_environment_kinds = Some(
                source_policy["ceiling"]["wek"].as_array().unwrap().iter()
                    .filter_map(|kind| (kind != "desktop_session").then(|| kind.as_str().unwrap().to_owned()))
                    .collect(),
            );
            current_ee.policy_narrowing.allowed_tools = Some(
                source_policy["ceiling"]["tools"].as_array().unwrap().iter().filter_map(|tool| {
                    let id = tool.as_str().unwrap();
                    let descriptor = read(&format!("registry/tool/{id}/tool.json"));
                    (!descriptor["supported_worker_environment_kinds"].as_array().unwrap()
                        .contains(&serde_json::json!("desktop_session"))).then(|| id.to_owned())
                }).collect(),
            );
            std::fs::write(&ee_path, serde_json::to_vec_pretty(&current_ee).unwrap()).unwrap();
            let refused = plan_operator_chat_topology(&continuation)
                .err().expect("the same existing run cannot restore desktop outside its current EE policy");
            assert_eq!(refused, "worker environment kind 'desktop_session' is outside the formed worker/EE grant");
            std::fs::write(&ee_path, ee_bytes).unwrap();
            let mut narrowed=make_request(&inherited,role,&format!("{id}_narrowed"));
            narrowed.selected_worker_environment_kinds=vec!["browser_session".into(),"code_mode".into()];
            narrowed.door_host_slots.remove("system_workspace_env");
            narrowed.context_artifact_bindings.retain(|reference|reference.id!="artifact-code-mode-cua-prelude");
            let narrower=plan_operator_chat_topology(&narrowed).expect("explicitly deselected desktop is not restored by the default");
            assert!(!narrower.capability_doors.contains_key("system_gui"));
            inherited = plan.authoring_profile;
        }
    }

    pub(super) fn copy_planner_descriptors(source: &Path, target: &Path) {
        std::fs::create_dir_all(target).unwrap();
        for entry in std::fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let destination = target.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                // Cargo package implementations are not planner inputs; nested operation
                // catalogs and every other descriptor/sidecar directory remain byte-exact.
                if entry.file_name() != "packages" { copy_planner_descriptors(&entry.path(), &destination); }
            } else if entry.file_type().unwrap().is_file() {
                std::fs::copy(entry.path(), destination).unwrap();
            } else {
                panic!("source descriptor fixture contains a nonregular entry: {}",entry.path().display());
            }
        }
    }

    fn assert_browser_only_persistent_plan(registry_root: &Path, ee_root: &Path) {
        // Recovery reads the actual durable sidecars, not just an in-memory RunRecord.
        // Keep this source-only overlay and all planned writes for evidence; no executable runs.
        let overlay = tempfile::tempdir().unwrap().keep();
        copy_planner_descriptors(registry_root, &overlay);
        let registry_root = overlay.as_path();
        println!("browser planner source overlay retained at {}", registry_root.display());
        let mut request = OperatorChatPlanRequest {
            registry_root:registry_root.into(), ee_instances_root:ee_root.into(), ee_policy_root:None, run_records_root:Some(ee_root.join("runs")),
            write_hash_algorithm:WriteHashAlgorithm::Sha256, interface_private_state_root:Some(ee_root.join("private")),
            chat_id:"browser_persistent_identity".into(), provider_interface_id:"world_provider_codex".into(),
            model:None, reasoning_effort:None, service_tier:None, ee_id:Some("ee_operator_space".into()),
            worker_interface_id:Some("system_worker_coder_max".into()), client_surface_role:"task".into(),
            cycle:"browser_persistent_cycle".into(), door_host_slots:BTreeMap::new(),
            external_inputs:Vec::new(), external_relays:Vec::new(), model_outputs:Vec::new(),
            context_artifact_bindings:Vec::new(),
            payload_artifact_bindings:Vec::new(), publications:Vec::new(),
            selected_worker_environment_kinds:vec!["browser_session".into(),"code_mode".into()],
            selected_tools:Vec::new(), retain_current_capabilities:false, existing_run_record:None,
            conversation:ConversationSeed { ledger_offset:0, artifact_bindings:Vec::new(), pinned_artifact_bindings:Vec::new(), pinned_context_items:Vec::new(), continued_from:None },
        };
        let exact = |plan: &OperatorChatPlan, tag: String| -> serde_json::Value {
            serde_json::from_str(&plan.interface_writes.iter().find(|write|write.tag==tag).expect("exact source write").content).unwrap()
        };
        let unbound = plan_operator_chat_topology(&request).unwrap();
        let isolated = exact(&unbound,format!("sidecar:{}:code_mode.json",unbound.capability_doors["system_code_mode"]));
        assert_eq!(isolated["context"]["mode"],"isolated");
        request.selected_worker_environment_kinds.clear();
        let selected_browser_tool = |name: &str| SelectedToolSeed { tool:name.into(),interface:"system_browser".into(),worker_environment_kind:"browser_session".into() };
        request.selected_tools = vec![selected_browser_tool("browser.navigate")];
        let partial = plan_operator_chat_topology(&request).expect("non-upload partial browser selection does not require an unselected continuation");
        let partial_browser = exact(&partial,format!("interface:{}",partial.capability_doors["system_browser"]));
        assert!(partial_browser["interface_driver_config"]["private_config"]["browser_room"].get("upload_approval").is_none());
        assert!(!partial.capability_tools.contains_key("browser.filechooser_resume"));
        assert!(partial_browser["interface_driver_config"]["private_config"]["browser_room"].get("wait_completion").is_none());
        request.selected_tools.push(selected_browser_tool("browser.wait"));
        assert!(plan_operator_chat_topology(&request).is_err(), "a selected wait must include its declared independent completion");
        request.selected_tools.push(selected_browser_tool("browser.wait_complete"));
        let paired = plan_operator_chat_topology(&request).unwrap();
        let paired_browser = exact(&paired,format!("interface:{}",paired.capability_doors["system_browser"]));
        assert_eq!(paired_browser["interface_driver_config"]["private_config"]["browser_room"]["wait_completion"],serde_json::json!({"request_tools":[paired.capability_tools["browser.wait"]],"completion_tool":paired.capability_tools["browser.wait_complete"]}));
        request.selected_tools.retain(|tool| tool.tool != "browser.wait");
        assert!(plan_operator_chat_topology(&request).is_err(), "orphan completion cannot obtain a request role");
        request.selected_tools.retain(|tool| tool.tool != "browser.wait_complete");
        request.selected_tools.push(selected_browser_tool("browser.filechooser_set_files"));
        assert!(plan_operator_chat_topology(&request).is_err());
        request.selected_tools.push(selected_browser_tool("browser.filechooser_resume"));
        let selected = plan_operator_chat_topology(&request).unwrap();
        let selected_browser = exact(&selected,format!("interface:{}",selected.capability_doors["system_browser"]));
        assert_eq!(selected_browser["interface_driver_config"]["private_config"]["browser_room"]["upload_approval"]["request_tools"],serde_json::json!([selected.capability_tools["browser.filechooser_set_files"]]));
        request.selected_tools.clear();
        request.selected_worker_environment_kinds = vec!["browser_session".into(),"code_mode".into()];
        request.context_artifact_bindings.push(ArtifactBindingSeed { id:"artifact-code-mode-browser-prelude".into() });
        let plan = plan_operator_chat_topology(&request).expect("explicit browser-only persistent recipe uses existing Coder Max grants");
        assert!(!plan.capability_doors.contains_key("system_gui"));
        assert!(!plan.capability_tools.keys().any(|tool|tool.starts_with("gui.")));
        let owner = &plan.capability_doors["system_code_mode"];
        let browser = &plan.capability_doors["system_browser"];
        let sidecar = exact(&plan,format!("sidecar:{owner}:code_mode.json"));
        assert_eq!(sidecar["context"]["mode"],"persistent");
        let prelude_ref = &sidecar["context"]["prelude_artifact"];
        let topology_ref = &sidecar["handoff"]["topology_artifact"];
        let prelude = exact(&plan,format!("artifact:{}",prelude_ref["id"].as_str().unwrap()));
        let topology = exact(&plan,format!("artifact:{}",topology_ref["id"].as_str().unwrap()));
        assert_eq!(prelude["exports"],serde_json::json!(["browser"]));
        assert!(!prelude["module_source"].as_str().unwrap().contains("createCua"));
        let source_path = registry_root.join("artifact/artifact-code-mode-browser-prelude/artifact.json");
        let source_bytes = std::fs::read(&source_path).unwrap();
        let source_body:serde_json::Value = serde_json::from_slice(&source_bytes).unwrap();
        let duplicated_roles = br#"{"schema_version":1,"module_source":"function createBrowser(){}","export_factory":"createBrowser","export_name":"browser","tool_roles":{"tabs":{"tool":"browser.tabs","interface":"system_browser"},"tabs":{"tool":"browser.close","interface":"system_browser"}}}"#;
        assert!(serde_json::from_slice::<CodeModePreludeSource>(duplicated_roles).is_err());
        assert!(prelude["module_source"].as_str().unwrap().contains(source_body["module_source"].as_str().unwrap()));
        let run:serde_json::Value = serde_json::from_str(&plan.run_record_content).unwrap();
        let source = &run["interface_state"][owner]["prelude_sources"][0];
        assert_eq!(run["interface_state"][owner]["prelude_sources"].as_array().unwrap().len(),1);
        let source_ref:octopus_ledger::ArtifactRef=serde_json::from_value(source["source"].clone()).unwrap();
        assert_eq!(source_ref.id.as_str(),"artifact-code-mode-browser-prelude");
        assert_eq!(source_ref.content,octopus_ledger::ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&source_body)));
        assert_eq!(source["source_sha256"],format!("{:x}",Sha256::digest(&source_bytes)));
        let snapshot = plan.interface_writes.iter().find(|write|write.tag==format!("artifact:{}",source["snapshot"]["id"].as_str().unwrap())).unwrap();
        let snapshot_body:serde_json::Value=serde_json::from_str(&snapshot.content).unwrap();
        assert_eq!(snapshot_body["id"],source["snapshot"]["id"]);
        assert_eq!(snapshot_body["run"],plan.run_id);
        assert_eq!(snapshot_body["source"],source["source"]);
        assert_eq!(snapshot_body["source_content"].as_str().unwrap().as_bytes(),source_bytes);
        assert_eq!(snapshot_body["source_sha256"],source["source_sha256"]);
        let snapshot_ref:octopus_ledger::ArtifactRef=serde_json::from_value(source["snapshot"].clone()).unwrap();
        assert_eq!(snapshot_ref.content,octopus_ledger::ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&snapshot_body)));
        let provider = exact(&plan,format!("interface:{}",plan.model_interface_id));
        let hidden = provider["interface_driver_config"]["private_config"]["model_hidden_tools"].as_array().unwrap();
        let routes = topology["routes"].as_array().unwrap();
        let methods = plan.capability_tools.iter().filter(|(base,_)|base.starts_with("browser.")).collect::<Vec<_>>();
        let source_roles: CodeModePreludeSource = serde_json::from_slice(&source_bytes).unwrap();
        let source_browser:serde_json::Value=serde_json::from_slice(&std::fs::read(registry_root.join("interface/system_browser/interface.json")).unwrap()).unwrap();
        let source_approval=&source_browser["interface_driver_config"]["private_config"]["browser_room"]["upload_approval"];
        let resume_base=source_approval["resume_tool"].as_str().unwrap();
        let resume=&plan.capability_tools[resume_base];
        let expected_approval=serde_json::json!({"operator_interface":plan.chat_interface_id,"resume_tool":resume});
        let source_completion=&source_browser["interface_driver_config"]["private_config"]["browser_room"]["wait_completion"];
        let completion_base=source_completion["completion_tool"].as_str().unwrap();
        let completion=&plan.capability_tools[completion_base];
        let browser_descriptor=exact(&plan,format!("interface:{browser}"));
        let mut expected_config = expected_approval.clone();
        expected_config["request_tools"] = serde_json::json!(source_approval["request_tools"].as_array().unwrap().iter().map(|tool| &plan.capability_tools[tool.as_str().unwrap()]).collect::<Vec<_>>());
        expected_config["permission_policy"] = source_approval["permission_policy"].clone();
        assert_eq!(browser_descriptor["interface_driver_config"]["private_config"]["browser_room"]["upload_approval"],expected_config);
        let mut unchanged_room=browser_descriptor["interface_driver_config"]["private_config"]["browser_room"].clone();
        unchanged_room["upload_approval"]=source_approval.clone();
        assert_eq!(unchanged_room["wait_completion"],serde_json::json!({"request_tools":source_completion["request_tools"].as_array().unwrap().iter().map(|tool| &plan.capability_tools[tool.as_str().unwrap()]).collect::<Vec<_>>(),"completion_tool":completion}));
        unchanged_room["wait_completion"]=source_completion.clone();
        assert_eq!(unchanged_room,source_browser["interface_driver_config"]["private_config"]["browser_room"]);
        let mut source_tools = source_roles.tool_roles.values().map(|role| role.tool.as_str()).collect::<BTreeSet<_>>();
        assert!(source_tools.insert(resume_base),"private resume must not become an SDK export");
        assert!(source_tools.contains(completion_base), "the source facade declares its exact wait completion companion");
        assert_eq!(methods.iter().map(|(base, _)| base.as_str()).collect::<BTreeSet<_>>(), source_tools);
        assert_eq!(routes.len() + 1,methods.len());
        assert!(!routes.iter().any(|route|route["tool"]==*resume));
        let mut expected_nested_approval=expected_approval.clone();
        expected_nested_approval["resume_descriptor"]=exact(&plan,format!("tool:{resume}"));
        for (base,tool) in methods {
            assert!(hidden.contains(&serde_json::json!(tool)));
            if tool == resume { continue; }
            let route=routes.iter().find(|route|route["tool"]==*tool).expect("every exact selected browser tool stays callable");
            assert_eq!(route["call"]["binding"]["outbound"][0]["interface"],*browser);
            assert_eq!(route["result"]["binding"]["outbound"][0]["interface"],*owner);
            if source_approval["request_tools"].as_array().unwrap().iter().any(|request| request == base.as_str()) {
                assert_eq!(route["app_approval"],expected_nested_approval);
                assert_ne!(route["app_approval"]["resume_tool"],resume_base);
            } else { assert!(route["app_approval"].is_null()); }
        }
        assert_nested_owner_returns(&plan,&topology);
        let request_route=routes.iter().find(|route| route["tool"] == plan.capability_tools["browser.wait"]).unwrap();
        let completion_route=routes.iter().find(|route| route["tool"] == *completion).unwrap();
        assert_ne!(request_route["result"]["name"],completion_route["result"]["name"]);
        for base in ["browser.viewport_set","browser.viewport_reset","browser.download_arm","browser.download_wait","browser.download_cancel","browser.tab_mark","browser.filechooser_arm","browser.filechooser_wait","browser.filechooser_set_files","browser.filechooser_cancel","browser.filechooser_resume"] {
            assert!(plan.capability_tools.contains_key(base));
        }
        let templates=run["interface_state"][&plan.model_interface_id]["run_authoring_binding"]["checkpoints"].as_array().unwrap();
        assert_eq!(templates.iter().filter(|cp|cp["role"]=="owner_input" && cp["interface_binding"]["interface"]==*owner).count(),1);
        assert!(!templates.iter().any(|cp|cp["interface_binding"]["interface"]==*browser||cp["outbound_interface_bindings"].as_array().unwrap().iter().any(|out|out["interface"]==*browser)));
        for write in plan.interface_writes.iter().chain(&plan.run_writes) {
            assert!(write.path.starts_with(registry_root) || (write.path.parent() == Some(ee_root.join("runs").as_path()) && write.tag.starts_with("run:") && write.expected_before_hash.is_none()));
            std::fs::create_dir_all(write.path.parent().unwrap()).unwrap();
            std::fs::write(&write.path, &write.content).unwrap();
        }
        request.existing_run_record=Some(run.clone());
        request.cycle="browser_persistent_next".into();
        let resumed=plan_operator_chat_topology(&request).expect("replanning preserves exact source provenance and live owner pins");
        let resumed_run:serde_json::Value=serde_json::from_str(&resumed.run_record_content).unwrap();
        assert_eq!(resumed_run["interface_state"][owner]["prelude_sources"],run["interface_state"][owner]["prelude_sources"]);
        let published_path=registry_root.join("artifact").join(source["source"]["id"].as_str().unwrap()).join("artifact.json");
        let mut updated:serde_json::Value=serde_json::from_slice(&source_bytes).unwrap();
        updated["module_source"]=serde_json::json!(format!("{}\n// New package revision",updated["module_source"].as_str().unwrap()));
        std::fs::write(&published_path,updated.to_string()).unwrap();
        let retained=plan_operator_chat_topology(&request).expect("package updates preserve the existing exact prelude snapshot");
        assert!(!retained.interface_writes.iter().any(|write|write.tag==format!("artifact:{}",prelude["id"].as_str().unwrap())));
        let retained_run:serde_json::Value=serde_json::from_str(&retained.run_record_content).unwrap();
        assert_eq!(retained_run["interface_state"][owner]["prelude_sources"],run["interface_state"][owner]["prelude_sources"]);
        let snapshot_path=registry_root.join("artifact").join(source["snapshot"]["id"].as_str().unwrap()).join("artifact.json");
        let original=std::fs::read(&snapshot_path).unwrap();
        std::fs::write(&snapshot_path,b"{}").unwrap();assert!(plan_operator_chat_topology(&request).is_err());
        std::fs::remove_file(&snapshot_path).unwrap();assert!(plan_operator_chat_topology(&request).is_err());
        std::fs::write(&snapshot_path,original).unwrap();std::fs::write(published_path,&source_bytes).unwrap();
        request.existing_run_record.as_mut().unwrap()["interface_state"][owner]["prelude_sources"][0]["source_sha256"]=serde_json::json!("0".repeat(64));
        assert!(plan_operator_chat_topology(&request).is_err());
        request.existing_run_record=None;
        request.context_artifact_bindings.push(ArtifactBindingSeed { id:"artifact-code-mode-browser-prelude".into() });
        assert!(plan_operator_chat_topology(&request).is_err());
        request.context_artifact_bindings.pop();
        request.context_artifact_bindings.push(ArtifactBindingSeed { id:"artifact-code-mode-cua-prelude".into() });
        assert!(plan_operator_chat_topology(&request).is_err(),"a bound CUA source cannot obtain missing GUI roles through a browser-only task");
    }


    #[test]
    fn scheduled_task_plan_binds_external_time_private_auth_and_project_roots() {
        let repository = repository_registry_root();
        let registry_root = repository.join("registry");
        let ee_dir = tempfile::tempdir().expect("temp EE instances");
        let private_root = tempfile::tempdir().expect("temp private interface root");
        let project_root = tempfile::tempdir().expect("temp project root");
        boot_operator_space_ee_instances(ee_dir.path());
        let mut request = OperatorChatPlanRequest {
            registry_root,
            ee_instances_root: ee_dir.path().to_path_buf(),
            ee_policy_root: None,
            run_records_root: Some(ee_dir.path().join("runs")),
            write_hash_algorithm: WriteHashAlgorithm::Blake3,
            interface_private_state_root: Some(private_root.path().to_path_buf()),
            chat_id: "scheduledprobe000000000001".to_string(),
            provider_interface_id: "world_provider_codex".to_string(),
            model: Some("gpt-5.6-terra".to_string()),
            reasoning_effort: Some("ultra".to_string()),
            service_tier: Some("priority".to_string()),
            ee_id: Some("ee_operator_space".to_string()),
            worker_interface_id: Some("system_worker_coder_max".to_string()),
            client_surface_role: "task".to_string(),
            cycle: "scheduledprobe000000000001".to_string(),
            door_host_slots: BTreeMap::from([(
                "system_workspace_env".to_string(),
                BTreeMap::from([(
                    "workspace_root".to_string(),
                    project_root.path().to_path_buf(),
                )]),
            )]),
            external_inputs: vec![ExternalInputSeed {
                checkpoint_id: "cp_scheduled_probe".to_string(),
                interface_id: "world_cron".to_string(),
                required_verified_artifacts: Vec::new(),
            }],
            external_relays: vec![ExternalRelaySeed {
                checkpoint_id: "cp_project_result_probe".to_string(),
                interface_id: "system_worker_planner".to_string(),
                outbound_interface_ids: vec![
                    "system_worker_coder".to_string(),
                    "system_worker_email".to_string(),
                ],
                required_verified_artifacts: vec![RequiredVerifiedArtifact {
                    mark: octopus_descriptors::MarkId::new("task.summary.received"),
                    artifact_label: "task.summary".to_string(),
                    verifier: octopus_descriptors::VerifierId::new("artifact_exists"),
                    params: serde_json::json!({}),
                }],
            }],
            model_outputs: Vec::new(),
            context_artifact_bindings: Vec::new(),
            payload_artifact_bindings: Vec::new(),
            selected_worker_environment_kinds: vec!["filesystem_workspace".to_string()],
            selected_tools: Vec::new(),
            retain_current_capabilities: false,
            existing_run_record: None,
            publications: Vec::new(),
            conversation: ConversationSeed {
                ledger_offset: 0,
                artifact_bindings: Vec::new(),
                pinned_artifact_bindings: Vec::new(),
                pinned_context_items: Vec::new(),
                continued_from: None,
            },
        };
        let plan = plan_operator_chat_topology(&request).expect("scheduled task topology plans");
        request.model = Some("gpt-model-outside-formed-policy".to_string());
        let invalid_model = match plan_operator_chat_topology(&request) {
            Ok(_) => panic!("a model outside formed interface policy must fail closed"),
            Err(error) => error,
        };
        assert!(invalid_model.contains("formed model catalog does not admit selected model"));
        assert_eq!(
            plan.expected_before_hash_algorithm,
            WriteHashAlgorithm::Blake3
        );
        assert_eq!(plan.model, "gpt-5.6-terra");
        assert_eq!(plan.reasoning_effort, "ultra");
        assert_eq!(plan.service_tier.as_deref(), Some("priority"));
        assert_eq!(
            plan.model_interface_id,
            "world_provider_codex_scheduledprobe000000000001"
        );

        let provider: InterfaceDescriptor = serde_json::from_str(
            &plan
                .interface_writes
                .iter()
                .find(|write| write.tag == format!("interface:{}", plan.model_interface_id))
                .expect("exact provider descriptor write")
                .content,
        )
        .expect("exact provider descriptor parses");
        let provider_config = provider
            .interface_driver_config
            .as_ref()
            .expect("exact provider driver config");
        assert_eq!(
            provider_config.residual_ambient.as_ref().unwrap()["private_auth_store"]["auth_file_env"],
            "OCTOPUS_MACHINE_CODEX_AUTH_FILE"
        );
        assert!(
            !provider_config
                .host_slots
                .contains_key("private_auth_store_owner_root")
        );
        assert_eq!(
            provider_config.residual_ambient.as_ref().unwrap()["spawn"]["connect_on_ready"],
            false
        );
        assert_eq!(
            provider_config.private_config["model"],
            serde_json::json!("gpt-5.6-terra")
        );
        assert_eq!(
            provider_config.private_config["reasoning_effort"],
            serde_json::json!("ultra")
        );
        assert_eq!(provider_config.private_config["service_tier"], serde_json::json!("priority"));
        assert_eq!(
            serde_json::from_value::<BTreeSet<String>>(provider.interface_policy.values["models"].clone()).unwrap(),
            serde_json::from_value::<BTreeSet<String>>(load_interface_descriptor(&request.registry_root.join("interface"), &request.provider_interface_id)
                .expect("declared provider model ceiling")
                .interface_policy.variables["models"].default.clone()).unwrap()
        );

        let life: SpawnLifeConfig = serde_json::from_str(
            &plan
                .interface_writes
                .iter()
                .find(|write| write.tag == format!("life:{}", plan.model_interface_id))
                .expect("exact provider life write")
                .content,
        )
        .expect("exact provider life parses");
        assert!(life.processes.iter().all(|process| {
            process
                .authority
                .read_env_paths
                .contains("OCTOPUS_MACHINE_CODEX_AUTH_FILE")
                && process
                    .authority
                    .write_env_paths
                    .contains("OCTOPUS_MACHINE_CODEX_AUTH_FILE")
        }));

        let workspace_door = &plan.capability_doors["system_workspace_env"];
        let workspace: InterfaceDescriptor = serde_json::from_str(
            &plan
                .interface_writes
                .iter()
                .find(|write| write.tag == format!("interface:{workspace_door}"))
                .expect("exact direct workspace door write")
                .content,
        )
        .expect("exact direct workspace door parses");
        assert_eq!(
            workspace.interface_driver_config.unwrap().host_slots["workspace_root"].path,
            project_root.path()
        );
        let private_policy: WorkerInterfacePolicy = serde_json::from_str(
            &plan
                .interface_writes
                .iter()
                .find(|write| write.tag == format!("policy:{}", plan.worker_interface_id))
                .expect("exact worker policy write")
                .content,
        )
        .expect("exact worker policy parses");
        assert_eq!(
            private_policy
                .ceiling
                .wek
                .iter()
                .map(|kind| kind.as_str())
                .collect::<Vec<_>>(),
            vec!["filesystem_workspace", "system_room"]
        );

        let record = RunRecord::from_bytes(plan.run_record_content.as_bytes())
            .expect("scheduled task run parses");
        assert_eq!(record.status, serde_json::json!("idle"));
        let provider_operation = record
            .checkpoints
            .iter()
            .find(|checkpoint| {
                checkpoint
                    .id
                    .as_str()
                    .starts_with("cp_operator_provider_operation_")
            })
            .expect("Codex provider operations receive one independent client input");
        let provider_operation_binding = provider_operation
            .interface_binding
            .as_ref()
            .expect("provider operation has an exact client binding");
        assert_eq!(
            provider_operation_binding.interface.as_str(),
            plan.chat_interface_id
        );
        assert!(provider_operation_binding.admits("provider.models.list"));
        assert!(provider_operation_binding.admits("provider.account.read"));
        assert!(!provider_operation_binding.admits("operator.message"));
        assert!(
            !provider_operation.artifact_bindings.is_empty(),
            "provider control checkpoints carry mandatory worker role dependencies without optional conversation context"
        );
        assert_eq!(
            provider_operation
                .outbound_interface_bindings
                .iter()
                .map(|binding| binding.interface.as_str())
                .collect::<Vec<_>>(),
            vec![plan.model_interface_id.as_str()]
        );
        let checkpoint = record
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.id.as_str() == "cp_scheduled_probe")
            .expect("scheduled external checkpoint");
        assert_eq!(
            checkpoint
                .interface_binding
                .as_ref()
                .unwrap()
                .interface
                .as_str(),
            "world_cron"
        );
        assert_eq!(
            checkpoint
                .outbound_interface_bindings
                .iter()
                .map(|binding| &binding.interface)
                .cloned()
                .collect::<Vec<_>>(),
            vec![InterfaceId::new(&plan.model_interface_id)]
        );
        let relay = record
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.id.as_str() == "cp_project_result_probe")
            .expect("external relay checkpoint");
        assert_eq!(
            relay.interface_binding.as_ref().unwrap().interface.as_str(),
            "system_worker_planner"
        );
        assert_eq!(
            relay
                .outbound_interface_bindings
                .iter()
                .map(|binding| binding.interface.as_str())
                .collect::<Vec<_>>(),
            vec!["system_worker_coder", "system_worker_email"]
        );
        assert_eq!(relay.required_verified_artifacts.len(), 1);
        assert_eq!(
            relay.required_verified_artifacts[0].mark.as_str(),
            "task.summary.received"
        );
        assert_eq!(
            relay.required_verified_artifacts[0].artifact_label,
            "task.summary"
        );
        assert_eq!(
            relay.required_verified_artifacts[0].verifier.as_str(),
            "artifact_exists"
        );
        for checkpoint in &record.checkpoints {
            assert!(
                checkpoint
                    .artifact_bindings
                    .iter()
                    .any(|binding| binding.id.as_str() == "artifact-worker-role-email"),
                "checkpoint '{}' omits the role artifact of an exact external-relay worker",
                checkpoint.id.as_str()
            );
        }
        let client_sidecar: serde_json::Value = serde_json::from_str(
            &plan
                .interface_writes
                .iter()
                .find(|write| write.tag == format!("octopus-os:{}", plan.chat_interface_id))
                .expect("exact client sidecar write")
                .content,
        )
        .expect("exact client sidecar parses");
        assert_eq!(
            client_sidecar["authoring_profile"]["external_inputs"][0]["interface_id"],
            "world_cron"
        );
        assert_eq!(
            client_sidecar["authoring_profile"]["external_relays"][0]["outbound_interface_ids"][0],
            "system_worker_coder"
        );
        assert_eq!(
            client_sidecar["authoring_profile"]["external_relays"][0]["required_verified_artifacts"]
                [0]["artifact_label"],
            "task.summary"
        );
        assert!(
            plan.interface_writes
                .iter()
                .all(|write| !write.tag.starts_with("ee-policy:")),
            "a broad non-narrowing EE does not accumulate private task ids"
        );
    }
}
