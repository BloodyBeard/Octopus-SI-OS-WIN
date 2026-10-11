use std::collections::{BTreeMap, BTreeSet};

use octopus_code_mode_engine::PersistentContextConfig;
use octopus_descriptors::{Descriptor, DescriptorHash, DescriptorName, InterfaceId, WorkerEnvironmentKindId};
use octopus_interfaces::{ArtifactRef, InterfaceInvocationEnvelope};
use octopus_tool_registry::{ToolDescriptor, ToolProcessDeclaration};
use octopus_tool_door_runtime::ToolProcessEnvironment;
use octopus_worker_env::{WorkerEnvKindSpec, WorkerEnvironmentFormation};
use serde::{Deserialize, Serialize};

pub const CONFIG_FILE: &str = "code_mode.json";
const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_CELLS: usize = 64;
const MAX_ROUTES: usize = 512;
const MAX_YIELD_TIME_MS: u64 = 600_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeModeConfig {
    schema_version: u32,
    #[serde(default)]
    pub context: CodeModeContext,
    #[serde(default)]
    pub continuation: Option<CodeModeContinuation>,
    #[serde(default)]
    pub handoff: Option<CodeModeHandoff>,
    #[serde(default = "default_max_source_bytes")]
    pub max_source_bytes: usize,
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
    #[serde(default = "default_max_cells")]
    pub max_cells: usize,
    #[serde(default = "default_yield_time_ms")]
    pub default_yield_time_ms: u64,
    #[serde(default = "default_max_yield_time_ms")]
    pub max_yield_time_ms: u64,
    #[serde(default)]
    pub routes: Vec<CodeModeRoute>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeModeHandoff {
    pub topology_artifact: ArtifactRef,
    pub wait_tool: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum CodeModeContinuation {
    AdmittedArtifact {
        resume_tool: String,
        notification_interface: Option<String>,
    },
}

impl CodeModeContinuation {
    pub fn resume_tool(&self) -> &str {
        match self {
            Self::AdmittedArtifact { resume_tool, .. } => resume_tool,
        }
    }

    pub fn notification_interface(&self) -> Option<&str> {
        match self {
            Self::AdmittedArtifact {
                notification_interface,
                ..
            } => notification_interface.as_deref(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeModeRoute {
    pub tool: String,
    pub interface: String,
    pub checkpoint: String,
    #[serde(default)]
    pub result_checkpoint: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum CodeModeContext {
    #[default]
    Isolated,
    Persistent {
        prelude_artifact: ArtifactRef,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedContext {
    Isolated,
    Persistent {
        artifact: ArtifactRef,
        payload: Vec<u8>,
        engine: PersistentContextConfig,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreludeArtifact {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    run: Option<String>,
    #[serde(default)]
    owner_interface: Option<String>,
    schema_version: u32,
    module_source: String,
    exports: Vec<String>,
}

fn artifact_identity_matches(id: Option<&str>, reference: &ArtifactRef) -> bool {
    match id {
        Some(id) => id == reference.id.as_str(),
        None => reference.id.as_str().rsplit_once(':').is_some_and(|(label, hash)|
            !label.is_empty() && hash == reference.content.to_hex()),
    }
}

impl CodeModeConfig {
    pub fn resolve_context(
        &self,
        envelope: &InterfaceInvocationEnvelope,
    ) -> Result<ResolvedContext, String> {
        let CodeModeContext::Persistent { prelude_artifact } = &self.context else {
            return Ok(ResolvedContext::Isolated);
        };
        let metadata = envelope.metadata().map_err(|error| error.to_string())?;
        let refs: Vec<_> = metadata
            .artifact_binding_refs
            .iter()
            .filter(|artifact| artifact.id == prelude_artifact.id)
            .collect();
        if refs != [prelude_artifact] {
            return Err(
                "persistent code mode prelude requires its exact single admitted artifact binding"
                    .to_string(),
            );
        }
        let payloads: Vec<_> = envelope
            .artifact_binding_payloads
            .iter()
            .filter(|payload| payload.artifact.id == prelude_artifact.id)
            .collect();
        let [payload] = payloads.as_slice() else {
            return Err(
                "persistent code mode prelude requires its exact single bound payload".to_string(),
            );
        };
        if payload.artifact != *prelude_artifact || payload.bytes.len() > self.max_source_bytes {
            return Err(
                "persistent code mode prelude has a different identity or exceeds its byte limit"
                    .to_string(),
            );
        }
        let prelude: PreludeArtifact = serde_json::from_slice(&payload.bytes)
            .map_err(|error| format!("bound code mode prelude is malformed: {error}"))?;
        if prelude.schema_version != 1 {
            return Err("bound code mode prelude requires schema_version 1".to_string());
        }
        let runtime = envelope.runtime().map_err(|error| error.to_string())?;
        let owner_matches = match (&prelude.id, &prelude.run, &prelude.owner_interface) {
            (Some(_), Some(run), Some(owner)) => run == runtime.run.as_str() && owner == runtime.interface.as_str(),
            (None, None, None) => true,
            _ => false,
        };
        if !artifact_identity_matches(prelude.id.as_deref(), prelude_artifact)
            || !owner_matches
        {
            return Err("bound code mode prelude has a different declared identity or owner".into());
        }
        Ok(ResolvedContext::Persistent {
            artifact: prelude_artifact.clone(),
            payload: payload.bytes.clone(),
            engine: PersistentContextConfig {
                module_source: prelude.module_source,
                exports: prelude.exports,
                max_source_bytes: self.max_source_bytes,
                max_output_bytes: self.max_output_bytes,
            },
        })
    }

    pub fn load_formed(
        environment: &ToolProcessEnvironment,
        formation: &WorkerEnvironmentFormation,
        kind: &WorkerEnvKindSpec,
    ) -> Result<Self, String> {
        let config: Self = environment.load_formed_json_sidecar(formation, kind, CONFIG_FILE)?;
        config.validate(environment.interface.as_str())?;
        Ok(config)
    }

    pub fn route_map(&self) -> BTreeMap<String, CodeModeRoute> {
        self.routes
            .iter()
            .cloned()
            .map(|route| (route.tool.clone(), route))
            .collect()
    }

    pub fn yield_time(&self, requested: Option<u64>) -> Result<u64, String> {
        let value = requested.unwrap_or(self.default_yield_time_ms);
        if value == 0 || value > self.max_yield_time_ms {
            return Err(format!(
                "code mode yield_time must be in 1..={} ms",
                self.max_yield_time_ms
            ));
        }
        Ok(value)
    }

    pub fn validate(&self, owner_interface: &str) -> Result<(), String> {
        match (&self.context, &self.continuation) {
            (CodeModeContext::Isolated, Some(_)) => {
                return Err("isolated code mode cannot declare admitted continuation".into());
            }
            (CodeModeContext::Persistent { .. }, None) => {
                return Err(
                    "persistent code mode requires an explicit admitted-artifact continuation"
                        .into(),
                );
            }
            _ => {}
        }
        if let Some(continuation) = &self.continuation {
            validate_identity("resume tool", continuation.resume_tool())?;
            if let Some(interface) = continuation.notification_interface() {
                validate_identity("notification interface", interface)?;
                if interface == owner_interface {
                    return Err("code mode notifications cannot target the owning door".into());
                }
            }
        }
        if let Some(handoff) = &self.handoff {
            validate_identity("handoff wait tool", &handoff.wait_tool)?;
            if self.continuation.is_none()
                || self
                    .continuation
                    .as_ref()
                    .is_some_and(|c| c.resume_tool() == handoff.wait_tool)
            {
                return Err(
                    "code mode handoff requires distinct declared Wait and Resume tools".into(),
                );
            }
        }
        if self.schema_version != 1 {
            return Err(format!(
                "code mode config has schema_version {}, expected 1",
                self.schema_version
            ));
        }
        if !(1..=MAX_SOURCE_BYTES).contains(&self.max_source_bytes) {
            return Err(format!(
                "code mode max_source_bytes must be in 1..={MAX_SOURCE_BYTES}"
            ));
        }
        if !(1024..=MAX_OUTPUT_BYTES).contains(&self.max_output_bytes) {
            return Err(format!(
                "code mode max_output_bytes must be in 1024..={MAX_OUTPUT_BYTES}"
            ));
        }
        if !(1..=MAX_CELLS).contains(&self.max_cells) {
            return Err(format!("code mode max_cells must be in 1..={MAX_CELLS}"));
        }
        if self.default_yield_time_ms == 0
            || self.max_yield_time_ms == 0
            || self.default_yield_time_ms > self.max_yield_time_ms
            || self.max_yield_time_ms > MAX_YIELD_TIME_MS
        {
            return Err(format!(
                "code mode yield times must satisfy 1 <= default <= max <= {MAX_YIELD_TIME_MS}"
            ));
        }
        if self.routes.len() > MAX_ROUTES {
            return Err(format!(
                "code mode routes exceed configured maximum {MAX_ROUTES}"
            ));
        }
        let mut tools = BTreeSet::new();
        for route in &self.routes {
            validate_identity("route tool", &route.tool)?;
            validate_identity("route interface", &route.interface)?;
            validate_identity("route checkpoint", &route.checkpoint)?;
            if let Some(checkpoint) = &route.result_checkpoint {
                validate_identity("result checkpoint", checkpoint)?;
            } else if self.continuation.is_some() {
                return Err(format!(
                    "persistent route '{}' requires its exact result_checkpoint",
                    route.tool
                ));
            }
            if route.interface == owner_interface {
                return Err(format!(
                    "code mode route '{}' recursively targets its owning interface '{}'",
                    route.tool, owner_interface
                ));
            }
            if !tools.insert(route.tool.as_str()) {
                return Err(format!(
                    "code mode config repeats route for tool '{}'",
                    route.tool
                ));
            }
        }
        Ok(())
    }
}

fn validate_identity(field: &str, value: &str) -> Result<(), String> {
    if value.trim() != value || value.is_empty() || value.contains('\0') {
        return Err(format!(
            "code mode {field} must be non-empty, unpadded, and contain no NUL"
        ));
    }
    Ok(())
}

fn default_max_source_bytes() -> usize {
    1024 * 1024
}

fn default_max_output_bytes() -> usize {
    8 * 1024 * 1024
}

fn default_max_cells() -> usize {
    8
}

fn default_yield_time_ms() -> u64 {
    10_000
}

fn default_max_yield_time_ms() -> u64 {
    60_000
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerCheckpoint {
    pub name: String,
    pub binding: octopus_interfaces::CheckpointBindingPolicySnapshot,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerToolRoute {
    pub tool: String,
    pub process_descriptor: ToolDescriptor,
    pub call: OwnerCheckpoint,
    pub result: OwnerCheckpoint,
    #[serde(default)]
    pub app_approval: Option<OwnerAppApproval>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerAppApproval {
    pub operator_interface: String,
    pub resume_tool: String,
    pub resume_descriptor: ToolDescriptor,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclaredToolOwner {
    pub interface: InterfaceId,
    pub kind: WorkerEnvironmentKindId,
    pub process: ToolProcessDeclaration,
}

impl DeclaredToolOwner {
    pub fn from_descriptor(descriptor: &ToolDescriptor) -> Result<Self, String> {
        descriptor.validate()?;
        let [kind] = descriptor.supported_worker_environment_kinds.as_slice() else {
            return Err("nested tool must declare exactly one execution kind".into());
        };
        Ok(Self {
            interface: descriptor.interface.clone(),
            kind: kind.clone(),
            process: descriptor.process.clone(),
        })
    }

    pub fn return_checkpoint(&self) -> Result<String, String> {
        let value = serde_json::to_value(self).map_err(|e| e.to_string())?;
        Ok(format!("nested_result_{}", DescriptorHash::of_canonical_json(&value).to_hex()))
    }
}

fn validate_pinned_tool(
    descriptor: &ToolDescriptor,
    tool: &str,
    door: &str,
    envelope: &InterfaceInvocationEnvelope,
) -> Result<DeclaredToolOwner, String> {
    let owner = DeclaredToolOwner::from_descriptor(descriptor)?;
    let metadata = envelope.metadata().map_err(|e| e.to_string())?;
    let hash = DescriptorHash::of_canonical_json(
        &serde_json::to_value(descriptor).map_err(|e| e.to_string())?,
    );
    if descriptor.id.as_str() != tool || owner.interface.as_str() != door
        || !metadata.grant.tools.iter().any(|id| id == tool)
        || metadata.grant.hard_blocked_tools.iter().any(|id| id == tool)
        || !metadata.grant.worker_environment_kinds.iter().any(|kind| kind == owner.kind.as_str())
        || metadata.grant.formation_pin.inputs.get(&DescriptorName::new(
            format!("registry_tool_root/{tool}/tool.json"),
        )) != Some(&hash)
    {
        return Err("nested process descriptor differs from its exact granted formation source".into());
    }
    Ok(owner)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeModeOwnerTopology {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub schema_version: u32,
    pub run: String,
    pub worker: String,
    pub owner_interface: String,
    pub prelude_artifact: ArtifactRef,
    pub file_write_tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coauthor: Option<octopus_provider_run_authoring::CheckedRunWriter>,
    pub authoring_result: OwnerCheckpoint,
    pub model_output: OwnerCheckpoint,
    pub notification: Option<OwnerCheckpoint>,
    pub receipt: OwnerCheckpoint,
    pub readiness: OwnerCheckpoint,
    pub routes: Vec<OwnerToolRoute>,
}

impl CodeModeConfig {
    pub fn resolve_owner_topology(
        &self,
        envelope: &InterfaceInvocationEnvelope,
    ) -> Result<CodeModeOwnerTopology, String> {
        let handoff = self
            .handoff
            .as_ref()
            .ok_or("persistent code mode has no independently authored handoff contract")?;
        let metadata = envelope.metadata().map_err(|e| e.to_string())?;
        let refs: Vec<_> = metadata
            .artifact_binding_refs
            .iter()
            .filter(|r| r.id == handoff.topology_artifact.id)
            .collect();
        let payloads: Vec<_> = envelope
            .artifact_binding_payloads
            .iter()
            .filter(|p| p.artifact.id == handoff.topology_artifact.id)
            .collect();
        if refs != [&handoff.topology_artifact]
            || payloads.len() != 1
            || payloads[0].artifact != handoff.topology_artifact
            || payloads[0].bytes.len() > self.max_source_bytes
        {
            return Err(
                "code mode topology requires one exact admitted artifact and bounded payload"
                    .into(),
            );
        }
        let topology: CodeModeOwnerTopology =
            serde_json::from_slice(&payloads[0].bytes).map_err(|e| e.to_string())?;
        self.validate_owner_topology(&topology, envelope)?;
        Ok(topology)
    }

    pub fn validate_owner_topology(
        &self,
        topology: &CodeModeOwnerTopology,
        envelope: &InterfaceInvocationEnvelope,
    ) -> Result<(), String> {
        let runtime = envelope.runtime().map_err(|e| e.to_string())?;
        let metadata = envelope.metadata().map_err(|e| e.to_string())?;
        let CodeModeContext::Persistent { prelude_artifact } = &self.context else {
            return Err("owner handoff requires explicit persistent context".into());
        };
        if self.handoff.as_ref().is_none_or(|handoff| !artifact_identity_matches(topology.id.as_deref(), &handoff.topology_artifact))
            || topology.schema_version != 1
            || topology.run != runtime.run.as_str()
            || topology.worker != runtime.worker.as_str()
            || topology.owner_interface != runtime.interface.as_str()
            || topology.prelude_artifact != *prelude_artifact
            || topology.routes.len() != self.routes.len()
        {
            return Err(
                "code mode topology differs from its original run, owner, prelude or route set"
                    .into(),
            );
        }
        if let Some(writer) = &topology.coauthor {
            if topology.model_output.binding.outbound.len() != 1 || writer.owner != topology.model_output.binding.outbound[0].interface || writer.owner.as_str() == topology.owner_interface
                || writer.source == writer.owner || writer.source.as_str() == topology.owner_interface || writer.door == writer.owner || writer.door == writer.source
                || writer.door == topology.authoring_result.binding.inbound.interface {
                return Err("provider coauthor is not an exact independently owned authoring edge".into());
            }
            for id in [writer.owner.as_str(),writer.source.as_str(),writer.door.as_str()] {
                validate_identity("coauthor interface",id)?;
            }
            for value in [&writer.tool,&writer.call_label,&writer.result_label] { validate_identity("coauthor contract",value)?; }
            if !metadata.grant.interfaces.iter().any(|id| id == writer.owner.as_str()) || metadata.grant.hard_blocked_tools.contains(&writer.tool) { return Err("coauthor owner or writer is denied by this formation".into()); }
        }
        let continuation = self.continuation.as_ref().ok_or("missing continuation")?;
        let handoff = self.handoff.as_ref().ok_or("missing handoff")?;
        let mut tools = vec![&topology.file_write_tool, &handoff.wait_tool];
        let mut names = BTreeSet::new();
        let mut owner_returns: BTreeMap<DeclaredToolOwner, (&OwnerCheckpoint, BTreeSet<String>)> = BTreeMap::new();
        let mut boundaries = vec![
            &topology.authoring_result,
            &topology.model_output,
            &topology.receipt,
            &topology.readiness,
        ];
        if let Some(notification) = &topology.notification {
            boundaries.push(notification);
        }
        for route in &topology.routes {
            let declared = self
                .routes
                .iter()
                .find(|r| r.tool == route.tool)
                .ok_or("owner topology includes an undeclared tool")?;
            let owner = validate_pinned_tool(&route.process_descriptor, &route.tool, &declared.interface, envelope)?;
            let group = owner_returns.entry(owner.clone()).or_insert_with(|| (&route.result, BTreeSet::new()));
            if group.0 != &route.result || route.result.name != owner.return_checkpoint()? {
                return Err("nested owner return checkpoint is inconsistent with its declared process".into());
            }
            group.1.insert(format!("{}.result", route.tool));
            if route.call.name != declared.checkpoint
                || Some(route.result.name.as_str()) != declared.result_checkpoint.as_deref()
                || route.call.binding.inbound.interface.as_str() != topology.owner_interface
                || route.call.binding.outbound.len() != 1
                || route.call.binding.outbound[0].interface.as_str() != declared.interface
                || route.result.binding.inbound.interface.as_str() != declared.interface
                || route.result.binding.outbound.len() != 1
                || route.result.binding.outbound[0].interface.as_str() != topology.owner_interface
            {
                return Err(
                    "owner topology tool route differs from exact declared call and return".into(),
                );
            }
            if let Some(approval) = &route.app_approval {
                if validate_pinned_tool(&approval.resume_descriptor, &approval.resume_tool, &declared.interface, envelope)? != owner {
                    return Err("approval resume belongs to a different declared process owner".into());
                }
                group.1.insert(format!("{}.result", approval.resume_tool));
                let client = approval.operator_interface.as_str();
                if continuation.notification_interface() != Some(client)
                    || !metadata.grant.tools.contains(&approval.resume_tool)
                    || metadata
                        .grant
                        .hard_blocked_tools
                        .contains(&approval.resume_tool)
                {
                    return Err(
                        "GUI continuation owner linkage is absent from this exact grant".into(),
                    );
                }
            }
            tools.push(&route.tool);
            boundaries.push(&route.call);
            if !boundaries.iter().any(|existing| **existing == route.result) {
                boundaries.push(&route.result);
            }
            if route.result.binding.inbound.interface
                == topology.authoring_result.binding.inbound.interface
            {
                return Err("nested tool cannot share its owner-private authoring door".into());
            }
        }
        let mut result_names = BTreeSet::new();
        for (owner, (checkpoint, labels)) in owner_returns {
            if labels.is_empty() || labels.iter().any(|label| !result_names.insert((owner.interface.clone(), label.clone()))) {
                return Err("nested process owner result names are empty or overlap".into());
            }
            let expected = serde_json::json!({"mode":"allow","names":labels,"namespaces":[]});
            for binding in std::iter::once(&checkpoint.binding.inbound).chain(&checkpoint.binding.outbound) {
                if serde_json::to_value(&binding.config.artifact_policy).map_err(|e| e.to_string())? != expected {
                    return Err("nested return must admit only its exact declared owner result names".into());
                }
            }
        }
        if tools
            .into_iter()
            .chain(std::iter::once(&topology.file_write_tool))
            .any(|t| {
                !metadata.grant.tools.contains(t) || metadata.grant.hard_blocked_tools.contains(t)
            })
        {
            return Err("owner topology contains a missing or blocked exact tool".into());
        }
        for checkpoint in boundaries {
            validate_identity("owner checkpoint", &checkpoint.name)?;
            if !names.insert(&checkpoint.name) || checkpoint.binding.outbound.len() != 1 {
                return Err(
                    "owner topology checkpoint roles must be unique single-output edges".into(),
                );
            }
            for binding in
                std::iter::once(&checkpoint.binding.inbound).chain(&checkpoint.binding.outbound)
            {
                if !metadata
                    .grant
                    .interfaces
                    .contains(&binding.interface.to_string())
                {
                    return Err("owner topology contains an ungranted exact interface".into());
                }
            }
        }
        for checkpoint in [&topology.receipt, &topology.readiness] {
            if checkpoint.binding.inbound.interface.as_str() != topology.owner_interface
                || checkpoint.binding.outbound[0].interface.as_str() != topology.owner_interface
            {
                return Err("internal owner outputs must return only to the owning door".into());
            }
        }
        if topology.model_output.binding.inbound.interface.as_str() != topology.owner_interface
            || topology.authoring_result.binding.outbound[0]
                .interface
                .as_str()
                != topology.owner_interface
        {
            return Err("owner output/authoring result has another source or destination".into());
        }
        match (
            &topology.notification,
            continuation.notification_interface(),
        ) {
            (None, None) => {}
            (Some(cp), Some(target))
                if cp.binding.inbound.interface.as_str() == topology.owner_interface
                    && cp.binding.outbound[0].interface.as_str() == target => {}
            _ => return Err("notification differs from the explicit owner recipient".into()),
        }
        Ok(())
    }
}
