use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use octopus_descriptors::{ArtifactId, InterfaceId};
use octopus_interfaces::InterfaceInvocationEnvelope;
use octopus_ledger::{
    ArtifactRef, ContentHash, DeliveryVerdict, InterfaceToolAffordance, InvocationMetadata,
};
use octopus_operator_chat_client::run_composition::{
    CompositionAuthoring, CompositionCommunication, CompositionRoots, CompositionSeed,
    RunCompositionPlan, RunCompositionRequest, plan_run_composition,
};
use octopus_operator_chat_client::{PlannedClientWrite, SelectedToolSeed};
use octopus_provider_run_authoring::ProviderOutgoingBinding;
use octopus_runtime::run_record::RunRecord;
use octopus_tool_handlers::ToolResultArtifact;
use octopus_tool_registry::ToolArgumentValidator;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const PEER_MESSAGE_LABEL: &str = "provider.peer.message";

#[cfg(test)]
#[path = "../tests/support/native_run_composition.rs"]
mod composition_tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerAction {
    Queue,
    Followup,
    Interrupt,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "PeerMessageWire")]
pub struct PeerMessage {
    pub action: PeerAction,
    pub control_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactRef>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PeerMessageWire {
    action: PeerAction,
    control_id: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default, deserialize_with = "deserialize_artifact_refs")]
    artifacts: Vec<ArtifactRef>,
}

impl TryFrom<PeerMessageWire> for PeerMessage {
    type Error = String;

    fn try_from(wire: PeerMessageWire) -> Result<Self, String> {
        let value = Self {
            action: wire.action,
            control_id: wire.control_id,
            text: wire.text,
            artifacts: wire.artifacts,
        };
        value.validate()?;
        Ok(value)
    }
}

impl PeerMessage {
    pub fn validate(&self) -> Result<(), String> {
        exact_string(&self.control_id, "peer control_id")?;
        if self
            .text
            .as_ref()
            .is_some_and(|text| text.trim().is_empty())
        {
            return Err("peer text must be absent or nonempty".into());
        }
        if self.action != PeerAction::Interrupt && self.text.is_none() && self.artifacts.is_empty()
        {
            return Err("queue/followup requires text or explicit artifacts".into());
        }
        distinct_refs(&self.artifacts)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOperation {
    CreateRun,
    SendMessage,
    FollowupTask,
    ListRuns,
    Wait,
    Interrupt,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCatalogEntry {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub operation: RunOperation,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCatalog {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub operations: Vec<RunCatalogEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunAffordanceConfig {
    #[serde(deserialize_with = "deserialize_artifact_ref")]
    pub catalog_artifact: ArtifactRef,
    #[serde(default)]
    pub file_write_tool: Option<String>,
    #[serde(default)]
    pub artifact_send_tool: Option<String>,
    #[serde(default)]
    pub roots: Option<CompositionRoots>,
    #[serde(default)]
    pub defaults: Option<RunDefaults>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunDefaults {
    #[serde(default)]
    pub client_template_interface_id: Option<String>,
    #[serde(default)]
    pub output_selection: Option<octopus_operator_chat_client::run_composition::CompositionOutputSelection>,
    pub ee_id: String,
    pub worker_template_interface_id: String,
    pub provider_interface_id: String,
    pub model: String,
    pub reasoning_effort: String,
    pub authoring: CompositionAuthoring,
    #[serde(default)]
    pub worker_environment_kinds: Vec<String>,
    #[serde(default)]
    pub tools: Vec<SelectedToolSeed>,
    #[serde(default)]
    pub door_host_slots: BTreeMap<String, BTreeMap<String, PathBuf>>,
    #[serde(default, deserialize_with = "deserialize_artifact_refs")]
    pub context_artifacts: Vec<ArtifactRef>,
    #[serde(default)]
    pub return_interface_ids: Vec<String>,
    #[serde(default)]
    pub return_to_source: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRunRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub output_selection: Option<octopus_operator_chat_client::run_composition::CompositionOutputSelection>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default, deserialize_with = "deserialize_artifact_refs")]
    pub artifacts: Vec<ArtifactRef>,
    #[serde(default, deserialize_with = "deserialize_artifact_refs")]
    pub context_artifacts: Vec<ArtifactRef>,
    #[serde(default)]
    pub ee_id: Option<String>,
    #[serde(default)]
    pub worker_template_interface_id: Option<String>,
    #[serde(default)]
    pub provider_interface_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub worker_environment_kinds: Option<Vec<String>>,
    #[serde(default)]
    pub tools: Option<Vec<SelectedToolSeed>>,
    #[serde(default)]
    pub door_host_slots: Option<BTreeMap<String, BTreeMap<String, PathBuf>>>,
    #[serde(default)]
    pub return_interface_ids: Option<Vec<String>>,
    #[serde(default)]
    pub client_interface_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerRequest {
    pub interface: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default, deserialize_with = "deserialize_artifact_refs")]
    pub artifacts: Vec<ArtifactRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitRequest {
    pub timeout_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeEffect {
    pub tool: String,
    pub arguments: Value,
    pub correlation_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileProof {
    pub path: PathBuf,
    pub before_hash: Option<String>,
    pub after_hash: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatedRun {
    pub run_id: String,
    pub worker_interface_id: String,
    pub provider_interface_id: String,
    pub receive_interface_id: String,
    pub return_interface_ids: Vec<String>,
    pub model: String,
    pub reasoning_effort: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativePhase {
    Writing {
        created: CreatedRun,
        files: Vec<FileProof>,
        seed: NativeEffect,
        outgoing_bindings: Vec<ProviderOutgoingBinding>,
    },
    Seeding {
        created: CreatedRun,
        files: Vec<FileProof>,
        write_receipt: Value,
    },
    Sending {
        interface: String,
        message: PeerMessage,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativePending {
    pub call_id: String,
    pub operation_name: String,
    pub source_run: String,
    pub effect: NativeEffect,
    pub state: NativePhase,
}

#[derive(Clone, Debug)]
pub enum NativeStep {
    Effect {
        effect: NativeEffect,
        pending: NativePending,
        outgoing_bindings: Vec<ProviderOutgoingBinding>,
    },
    Complete {
        result: Value,
    },
    Wait {
        request: WaitRequest,
    },
}

impl NativePending {
    pub fn advance(&self, result: &Value) -> Result<NativeStep, String> {
        let receipt: ToolResultArtifact = serde_json::from_value(result.clone())
            .map_err(|error| format!("native operation result is malformed: {error}"))?;
        if receipt.tool != self.effect.tool
            || receipt.correlation_id.as_deref() != Some(self.effect.correlation_id.as_str())
        {
            return Err("native operation result belongs to another tool or correlation".into());
        }
        if !receipt.ok {
            return Ok(NativeStep::Complete {
                result: self.failure_result(result),
            });
        }
        let body = receipt.body.ok_or("native result has no body")?;
        match &self.state {
            NativePhase::Writing {
                created,
                files,
                seed,
                outgoing_bindings,
            } => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Written {
                    result: String,
                    writes: Vec<FileProof>,
                }
                let written: Written = serde_json::from_value(body).map_err(|error| {
                    format!("native creation file evidence is malformed: {error}")
                })?;
                if written.result != "files_written" || written.writes != *files {
                    return Err(
                        "native creation file result does not prove the exact planned writes"
                            .into(),
                    );
                }
                let next = Self {
                    effect: seed.clone(),
                    state: NativePhase::Seeding {
                        created: created.clone(),
                        files: files.clone(),
                        write_receipt: result.clone(),
                    },
                    ..self.clone()
                };
                Ok(NativeStep::Effect {
                    effect: seed.clone(),
                    pending: next,
                    outgoing_bindings: outgoing_bindings.clone(),
                })
            }
            NativePhase::Seeding {
                created,
                files,
                write_receipt,
            } => {
                let verdict = parse_delivery_verdict(body)?;
                Ok(NativeStep::Complete {
                    result: json!({
                        "call_id": self.call_id, "operation": self.operation_name,
                        "created": created, "installed": true, "files": files,
                        "write_receipt": write_receipt, "seed_receipt": result,
                        "ok": matches!(verdict, DeliveryVerdict::Admitted { .. }),
                        "seed_admitted": matches!(verdict, DeliveryVerdict::Admitted { .. }),
                        "work_completed": false,
                    }),
                })
            }
            NativePhase::Sending { interface, message } => {
                let verdict = parse_delivery_verdict(body)?;
                Ok(NativeStep::Complete {
                    result: json!({
                        "call_id": self.call_id, "operation": self.operation_name,
                        "interface": interface, "control_id": message.control_id,
                        "action": message.action, "receipt": result,
                        "ok": matches!(verdict, DeliveryVerdict::Admitted { .. }),
                        "admitted": matches!(verdict, DeliveryVerdict::Admitted { .. }),
                        "recipient_action_confirmed": false, "work_completed": false,
                    }),
                })
            }
        }
    }

    fn failure_result(&self, receipt: &Value) -> Value {
        let mut result = json!({
            "call_id": self.call_id, "operation": self.operation_name,
            "ok": false, "receipt": receipt, "work_completed": false,
        });
        match &self.state {
            NativePhase::Writing { created, files, .. } => {
                result["created"] = json!(created);
                result["planned_files"] = json!(files);
                result["installed"] = Value::Null;
                result["seed_admitted"] = json!(false);
            }
            NativePhase::Seeding {
                created,
                files,
                write_receipt,
            } => {
                result["created"] = json!(created);
                result["files"] = json!(files);
                result["write_receipt"] = write_receipt.clone();
                result["installed"] = json!(true);
                result["seed_admitted"] = Value::Null;
            }
            NativePhase::Sending { interface, message } => {
                result["interface"] = json!(interface);
                result["control_id"] = json!(message.control_id);
                result["admitted"] = Value::Null;
                result["recipient_action_confirmed"] = json!(false);
            }
        }
        result
    }
}

#[derive(Clone, Debug)]
pub struct RunAffordances {
    config: RunAffordanceConfig,
    catalog: RunCatalog,
    enabled: BTreeSet<String>,
}

impl RunAffordances {
    pub fn from_invocation(
        input: &InterfaceInvocationEnvelope,
        private_config: &Value,
    ) -> Result<Option<Self>, String> {
        if private_config.is_null() {
            return Ok(None);
        }
        let private = private_config
            .as_object()
            .ok_or("provider private_config must be an object")?;
        let Some(raw) = private.get("run_affordances") else {
            return Ok(None);
        };
        let config: RunAffordanceConfig = serde_json::from_value(raw.clone())
            .map_err(|error| format!("run_affordances config is malformed: {error}"))?;
        let metadata = input.metadata().map_err(|error| error.to_string())?;
        require_readable_refs(metadata, std::slice::from_ref(&config.catalog_artifact))?;
        let mut bound = input
            .artifact_binding_payloads
            .iter()
            .filter(|bound| bound.artifact == config.catalog_artifact);
        let payload = bound
            .next()
            .ok_or("run affordance catalog payload is not exactly bound")?;
        if bound.next().is_some() {
            return Err("run affordance catalog is bound more than once".into());
        }
        let catalog: RunCatalog = serde_json::from_slice(payload.as_bytes())
            .map_err(|error| format!("bound run affordance catalog is malformed: {error}"))?;
        if catalog.id != config.catalog_artifact.id.as_str() {
            return Err("run affordance catalog self-identity differs from its bound ref".into());
        }
        exact_string(&catalog.label, "catalog label")?;
        if catalog.operations.is_empty() {
            return Err("run affordance catalog is empty".into());
        }
        for id in config
            .file_write_tool
            .iter()
            .chain(config.artifact_send_tool.iter())
        {
            exact_string(id, "configured tool")?;
        }
        if config.file_write_tool.is_some() && config.file_write_tool == config.artifact_send_tool {
            return Err("file writer and artifact sender must be different exact tools".into());
        }
        if let Some(roots) = &config.roots {
            for path in [
                &roots.registry_root,
                &roots.ee_instances_root,
                &roots.run_records_root,
                &roots.interface_private_state_root,
            ] {
                absolute_path(path)?;
            }
        }
        if let Some(defaults) = &config.defaults {
            validate_defaults(defaults)?;

        }
        let mut names = BTreeSet::new();
        let mut enabled = BTreeSet::new();
        for row in &catalog.operations {
            exact_string(&row.name, "operation name")?;
            if !row
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
            {
                return Err(
                    "operation name contains characters outside the native tool name contract"
                        .into(),
                );
            }
            if row.description.trim().is_empty() || row.description.trim() != row.description {
                return Err("operation description must be nonempty and trimmed".into());
            }
            if !names.insert(row.name.clone()) {
                return Err("run catalog repeats an operation name".into());
            }
            if metadata
                .grant
                .interface_affordance_specs
                .iter()
                .any(|tool| tool.name.as_str() == row.name)
            {
                return Err(format!(
                    "run operation '{}' collides with an ordinary tool",
                    row.name
                ));
            }
            if !row.input_schema.is_object()
                || row.input_schema.get("type").and_then(Value::as_str) != Some("object")
            {
                return Err(format!(
                    "run operation '{}' requires an object input schema",
                    row.name
                ));
            }
            ToolArgumentValidator::compile(&row.input_schema)?;
            if row.operation == RunOperation::CreateRun
                && (config.roots.is_none()
                    || config.defaults.is_none()
                    || config.file_write_tool.is_none()
                    || config.artifact_send_tool.is_none())
            {
                return Err("create_run catalog row requires roots, defaults, file_write_tool and artifact_send_tool".into());
            }
            if matches!(
                row.operation,
                RunOperation::SendMessage | RunOperation::FollowupTask | RunOperation::Interrupt
            ) && config.artifact_send_tool.is_none()
            {
                return Err("peer operation requires explicit artifact_send_tool config".into());
            }
            if operation_granted(row.operation, &config, metadata)? {
                enabled.insert(row.name.clone());
            }
        }
        Ok(Some(Self {
            config,
            catalog,
            enabled,
        }))
    }

    pub fn affordances(&self) -> Vec<InterfaceToolAffordance> {
        self.catalog
            .operations
            .iter()
            .filter(|row| self.enabled.contains(&row.name))
            .map(|row| InterfaceToolAffordance {
                name: octopus_descriptors::ToolId::new(&row.name),
                description: row.description.clone(),
                args_schema: row.input_schema.clone(),
            })
            .collect()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.enabled.contains(name)
    }

    pub fn plan_call(
        &self,
        metadata: &InvocationMetadata,
        admitted_parent: &RunRecord,
        parent_record: &RunRecord,
        call_id: &str,
        operation_name: &str,
        arguments: &Value,
    ) -> Result<NativeStep, String> {
        exact_string(call_id, "native call_id")?;
        require_readable_refs(
            metadata,
            std::slice::from_ref(&self.config.catalog_artifact),
        )?;
        let runtime = metadata
            .runtime
            .as_ref()
            .ok_or("native run operation requires runtime metadata")?;
        if runtime.run != parent_record.id
            || runtime.ee != parent_record.ee
            || runtime.worker != parent_record.worker
        {
            return Err("native run operation source record differs from runtime metadata".into());
        }
        let row = self
            .catalog
            .operations
            .iter()
            .find(|row| row.name == operation_name)
            .ok_or("native operation is absent from the bound catalog")?;
        if !self.contains(operation_name)
            || !operation_granted(row.operation, &self.config, metadata)?
        {
            return Err("native operation lacks its required ordinary tool grant".into());
        }
        ToolArgumentValidator::compile(&row.input_schema)?.validate(arguments)?;
        let control_id = call_identity(runtime.run.as_str(), call_id)?;
        match row.operation {
            RunOperation::CreateRun => {
                let args: CreateRunRequest =
                    serde_json::from_value(arguments.clone()).map_err(|e| e.to_string())?;
                let request =
                    self.composition_request(metadata, admitted_parent, &control_id, args)?;
                let plan = plan_run_composition(&request)?;
                self.creation_step(parent_record, call_id, operation_name, &control_id, plan)
            }
            RunOperation::SendMessage | RunOperation::FollowupTask | RunOperation::Interrupt => {
                let args: PeerRequest =
                    serde_json::from_value(arguments.clone()).map_err(|e| e.to_string())?;
                require_peer_target(metadata, parent_record, &args.interface, self.config.roots.as_ref().map(|roots| roots.registry_root.as_path()))?;
                require_readable_refs(metadata, &args.artifacts)?;
                let message = PeerMessage {
                    action: match row.operation {
                        RunOperation::SendMessage => PeerAction::Queue,
                        RunOperation::FollowupTask => PeerAction::Followup,
                        RunOperation::Interrupt => PeerAction::Interrupt,
                        _ => unreachable!(),
                    },
                    control_id: control_id.clone(),
                    text: args.text,
                    artifacts: args.artifacts,
                };
                message.validate()?;
                let effect = peer_effect(
                    self.sender()?,
                    &args.interface,
                    &message,
                    &format!("{control_id}:send"),
                );
                let pending = NativePending {
                    call_id: call_id.into(),
                    operation_name: operation_name.into(),
                    source_run: runtime.run.as_str().into(),
                    effect: effect.clone(),
                    state: NativePhase::Sending {
                        interface: args.interface.clone(),
                        message,
                    },
                };
                Ok(NativeStep::Effect {
                    effect,
                    pending,
                    outgoing_bindings: vec![ProviderOutgoingBinding {
                        interface: InterfaceId::new(args.interface),
                        artifact_label: PEER_MESSAGE_LABEL.into(),
                        artifact_bindings: Vec::new(),
                    }],
                })
            }
            RunOperation::ListRuns => {
                if arguments
                    .as_object()
                    .is_none_or(|object| !object.is_empty())
                {
                    return Err("list_runs expects an empty object".into());
                }
                Ok(NativeStep::Complete {
                    result: json!({
                        "source_run": {"run_id": runtime.run, "ee_id": runtime.ee,
                            "worker_interface_id": runtime.worker, "recorded_status": parent_record.status,
                            "run_record_hash": runtime.run_record_hash, "provider_activity": "not_observed"},
                    "declared_peer_interfaces": declared_peers(metadata, parent_record, self.config.roots.as_ref().map(|roots| roots.registry_root.as_path())),
                    "receive_interface_id": declared_receive_interface(parent_record, &metadata.runtime.as_ref().ok_or("list requires runtime metadata")?.interface)?,
                        "source_movement_seq": metadata.source_movement_seq,
                    }),
                })
            }
            RunOperation::Wait => {
                let request: WaitRequest =
                    serde_json::from_value(arguments.clone()).map_err(|e| e.to_string())?;
                if request.timeout_ms == 0 {
                    return Err("wait timeout_ms must be positive".into());
                }
                Ok(NativeStep::Wait { request })
            }
        }
    }

    fn sender(&self) -> Result<&str, String> {
        self.config
            .artifact_send_tool
            .as_deref()
            .ok_or_else(|| "artifact_send_tool is not configured".into())
    }

    fn composition_request(
        &self,
        metadata: &InvocationMetadata,
        parent: &RunRecord,
        identity: &str,
        args: CreateRunRequest,
    ) -> Result<RunCompositionRequest, String> {
        let defaults = self
            .config
            .defaults
            .as_ref()
            .ok_or("creation defaults are absent")?;
        require_readable_refs(metadata, &args.artifacts)?;
        let contexts = merge_refs(&defaults.context_artifacts, &args.context_artifacts)?;
        require_readable_refs(metadata, &contexts)?;
        let seed = PeerMessage {
            action: PeerAction::Followup,
            control_id: identity.into(),
            text: args.text,
            artifacts: args.artifacts.clone(),
        };
        seed.validate()?;
        let return_interfaces = match args.return_interface_ids {
            Some(ids) => ids,
            None => {
                let mut ids = defaults.return_interface_ids.clone();
                if defaults.return_to_source {
                    let endpoint = declared_receive_interface(parent, &metadata.runtime.as_ref().ok_or("return requires runtime metadata")?.interface)?;
                    if !ids.contains(&endpoint) { ids.push(endpoint); }
                }
                ids
            }
        };
        Ok(RunCompositionRequest {
            roots: self
                .config
                .roots
                .clone()
                .ok_or("creation roots are absent")?,
            metadata: metadata.clone(),
            admitted_parent_record: parent.clone(),
            identity_seed: identity.into(),
            ee_id: args.ee_id.unwrap_or_else(|| defaults.ee_id.clone()),
            worker_template_interface_id: args
                .worker_template_interface_id
                .unwrap_or_else(|| defaults.worker_template_interface_id.clone()),
            provider_interface_id: args
                .provider_interface_id
                .unwrap_or_else(|| defaults.provider_interface_id.clone()),
            model: args.model.unwrap_or_else(|| defaults.model.clone()),
            reasoning_effort: args
                .reasoning_effort
                .unwrap_or_else(|| defaults.reasoning_effort.clone()),
            authoring: defaults.authoring.clone(),
            communication: CompositionCommunication {
                client_template_interface_id: args.client_interface_id.is_none()
                    .then(|| defaults.client_template_interface_id.clone()).flatten(),
                client_title: args.title,
                receive_interface_id: format!("system_worker_{}", identity),
                return_interface_ids: return_interfaces,
                client_interface_id: args.client_interface_id,
                output_selection: args.output_selection.or_else(|| defaults.output_selection.clone()),
            },
            selected_worker_environment_kinds: args
                .worker_environment_kinds
                .unwrap_or_else(|| defaults.worker_environment_kinds.clone()),
            selected_tools: args.tools.unwrap_or_else(|| defaults.tools.clone()),
            door_host_slots: args
                .door_host_slots
                .unwrap_or_else(|| defaults.door_host_slots.clone()),
            context_artifact_bindings: Vec::new(),
            payload_artifact_bindings: merge_refs(&contexts, &args.artifacts)?,
            seed: CompositionSeed {
                label: PEER_MESSAGE_LABEL.into(),
                payload: json!(seed),
            },
        })
    }

    fn creation_step(
        &self,
        parent: &RunRecord,
        call_id: &str,
        operation_name: &str,
        identity: &str,
        plan: RunCompositionPlan,
    ) -> Result<NativeStep, String> {
        let roots = self
            .config
            .roots
            .as_ref()
            .ok_or("creation roots are absent")?;
        let parent_path = roots.run_records_root.join(format!("{}.json", parent.id));
        let mut writes = plan.descriptor_writes.clone();
        writes.extend(
            plan.required_policy_updates
                .iter()
                .map(|update| update.write.clone()),
        );
        writes.push(plan.run_write.clone());
        let (arguments, files) = file_write_batch(&writes, &parent_path)?;
        let mut role_refs = BTreeSet::new();
        for update in &plan.required_binding_updates {
            if update.run_id != parent.id.as_str()
                || update.outbound_binding.interface.as_str() != plan.receive_interface_id
            {
                return Err("composition requested an unrelated parent binding edit".into());
            }
            role_refs.extend(
                update
                    .required_artifact_bindings
                    .iter()
                    .map(|binding| ArtifactId::new(&binding.id)),
            );
        }
        let outgoing_bindings = vec![ProviderOutgoingBinding {
            interface: InterfaceId::new(&plan.receive_interface_id),
            artifact_label: plan.seed.label.clone(),
            artifact_bindings: role_refs.into_iter().collect(),
        }];
        let seed = NativeEffect {
            tool: self.sender()?.into(),
            correlation_id: format!("{identity}:seed"),
            arguments: json!({"interface": plan.seed.interface_id, "label": plan.seed.label, "payload": plan.seed.payload}),
        };
        let effect = NativeEffect {
            tool: self
                .config
                .file_write_tool
                .clone()
                .ok_or("file_write_tool is absent")?,
            arguments,
            correlation_id: format!("{identity}:write"),
        };
        let created = CreatedRun {
            run_id: plan.run_id,
            worker_interface_id: plan.worker_interface_id,
            provider_interface_id: plan.provider_interface_id,
            receive_interface_id: plan.receive_interface_id,
            return_interface_ids: plan.return_interface_ids,
            model: plan.model,
            reasoning_effort: plan.reasoning_effort,
        };
        let pending = NativePending {
            call_id: call_id.into(),
            operation_name: operation_name.into(),
            source_run: parent.id.as_str().into(),
            effect: effect.clone(),
            state: NativePhase::Writing {
                created,
                files,
                seed,
                outgoing_bindings,
            },
        };
        Ok(NativeStep::Effect {
            effect,
            pending,
            outgoing_bindings: Vec::new(),
        })
    }
}

pub fn call_identity(source_run: &str, call_id: &str) -> Result<String, String> {
    exact_string(source_run, "source run")?;
    exact_string(call_id, "native call_id")?;
    let bytes = serde_json::to_vec(&["octopus.run.affordance.v1", source_run, call_id])
        .map_err(|e| e.to_string())?;
    Ok(format!("native_{}", ContentHash::of_bytes(&bytes).to_hex()))
}

fn peer_effect(
    tool: &str,
    interface: &str,
    message: &PeerMessage,
    correlation_id: &str,
) -> NativeEffect {
    NativeEffect {
        tool: tool.into(),
        correlation_id: correlation_id.into(),
        arguments: json!({"interface": interface, "label": PEER_MESSAGE_LABEL, "payload": message}),
    }
}

fn file_write_batch(
    writes: &[PlannedClientWrite],
    parent_path: &Path,
) -> Result<(Value, Vec<FileProof>), String> {
    if writes.is_empty() {
        return Err("native creation has no planned writes".into());
    }
    let mut paths = BTreeSet::new();
    let mut proofs = Vec::with_capacity(writes.len());
    let mut entries = Vec::with_capacity(writes.len());
    for write in writes {
        absolute_path(&write.path)?;
        if write.path == parent_path || !paths.insert(write.path.clone()) {
            return Err(
                "native creation must not patch the parent run or repeat write paths".into(),
            );
        }
        if let Some(hash) = &write.expected_before_hash {
            if ContentHash::from_hex(hash).is_none_or(|parsed| parsed.to_hex() != *hash) {
                return Err("planned file preimage is not a canonical content hash".into());
            }
        }
        proofs.push(FileProof {
            path: write.path.clone(),
            before_hash: write.expected_before_hash.clone(),
            after_hash: ContentHash::of_bytes(write.content.as_bytes()).to_hex(),
            bytes: write.content.len() as u64,
        });
        entries.push(json!({"path": write.path, "content": write.content, "expected_before_hash": write.expected_before_hash}));
    }
    Ok((json!({"writes": entries}), proofs))
}

fn parse_delivery_verdict(body: Value) -> Result<DeliveryVerdict, String> {
    let verdict: DeliveryVerdict = serde_json::from_value(body.clone())
        .map_err(|e| format!("malformed artifact.send verdict: {e}"))?;
    if serde_json::to_value(&verdict).map_err(|e| e.to_string())? != body {
        return Err("artifact.send verdict contains undeclared fields".into());
    }
    Ok(verdict)
}

fn operation_granted(
    operation: RunOperation,
    config: &RunAffordanceConfig,
    metadata: &InvocationMetadata,
) -> Result<bool, String> {
    let granted = |id: &Option<String>| -> Result<bool, String> {
        let Some(id) = id else {
            return Ok(false);
        };
        if !metadata.grant.tools.contains(id) || metadata.grant.hard_blocked_tools.contains(id) {
            return Ok(false);
        }
        let mut specs = metadata
            .grant
            .interface_affordance_specs
            .iter()
            .filter(|spec| spec.name.as_str() == id);
        let spec = specs
            .next()
            .ok_or_else(|| format!("granted tool '{id}' has no formed descriptor affordance"))?;
        if specs.next().is_some() || !spec.args_schema.is_object() {
            return Err(format!(
                "granted tool '{id}' has ambiguous or malformed affordance metadata"
            ));
        }
        Ok(true)
    };
    match operation {
        RunOperation::CreateRun => Ok(granted(&config.file_write_tool)?
            && granted(&config.artifact_send_tool)?
            && metadata.grant.may_create_artifact()),
        RunOperation::SendMessage | RunOperation::FollowupTask | RunOperation::Interrupt => {
            Ok(granted(&config.artifact_send_tool)? && metadata.grant.may_create_artifact())
        }
        RunOperation::ListRuns | RunOperation::Wait => Ok(true),
    }
}

fn declared_receive_interface(record: &RunRecord, provider: &InterfaceId) -> Result<String, String> {
    let mut endpoints = Vec::new();
    for (id, state) in &record.interface_state {
        if let Some(value) = state.get("input_intake") {
            let config: octopus_operator_chat_client::input_intake::InputIntakeConfig = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
            config.validate(id)?;
            if config.run == record.id.as_str() && config.provider_interface == *provider { endpoints.push(id.to_string()); }
        }
    }
    match endpoints.as_slice() {
        [] => Ok(record.worker.to_string()),
        [endpoint] => Ok(endpoint.clone()),
        _ => Err("run has ambiguous declared input endpoints for this provider".into()),
    }
}

fn declared_peers(metadata: &InvocationMetadata, record: &RunRecord, registry: Option<&Path>) -> Vec<String> {
    metadata
        .grant
        .attached_interface_bindings
        .iter()
        .filter(|binding| binding.interface_type == "system")
        .map(|binding| binding.interface.as_str())
        .filter(|interface| require_peer_target(metadata, record, interface, registry).is_ok())
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn require_peer_target(
    metadata: &InvocationMetadata,
    record: &RunRecord,
    interface: &str,
    registry: Option<&Path>,
) -> Result<(), String> {
    exact_string(interface, "peer interface")?;
    let service = metadata.grant.attached_interface_bindings.iter().any(|bound| bound.interface.as_str() == interface && bound.interface_type == "system" && bound.role == "service");
    if service {
        let root = registry.ok_or("exact intake descriptor validation requires declared registry roots")?;
        if interface.contains('/') || interface == "." || interface == ".." { return Err("invalid exact intake interface".into()); }
        let bytes = std::fs::read(root.join("interface").join(interface).join("interface.json")).map_err(|e| e.to_string())?;
        let descriptor: octopus_interfaces::InterfaceDescriptor = octopus_descriptors::parse_strict(&bytes).map_err(|e| e.to_string())?;
        if descriptor.id.as_str() != interface || descriptor.interface_type != octopus_interfaces::InterfaceType::System || descriptor.role.0 != "service" || descriptor.interface_driver_ref != octopus_operator_chat_client::input_intake::INPUT_INTAKE_DRIVER { return Err("peer target is not the declared exact input service".into()); }
        let config: octopus_operator_chat_client::input_intake::InputIntakeConfig = serde_json::from_value(descriptor.interface_driver_config.as_ref().and_then(|c| c.private_config.get("input_intake")).cloned().ok_or("input service has no concrete declaration")?).map_err(|e| e.to_string())?;
        config.validate(&descriptor.id)?;
    }
    if !metadata.grant.interfaces.iter().any(|id| id == interface)
        || !metadata
            .grant
            .attached_interface_bindings
            .iter()
            .any(|bound| {
                bound.interface.as_str() == interface
                    && bound.interface_type == "system"
                    && (bound.role == "worker" || service)
            })
    {
        return Err(format!(
            "peer interface '{interface}' is not an exact granted outgoing System input binding"
        ));
    }
    if !record
        .checkpoints
        .iter()
        .filter(|checkpoint| record.active_checkpoints.contains(&checkpoint.id))
        .flat_map(|checkpoint| &checkpoint.outbound_interface_bindings)
        .any(|binding| {
            binding.interface.as_str() == interface
                && binding.config.artifact_policy.admits(PEER_MESSAGE_LABEL)
        })
    {
        return Err("peer message label is not admitted by the exact outgoing binding".into());
    }
    Ok(())
}

fn require_readable_refs(
    metadata: &InvocationMetadata,
    refs: &[ArtifactRef],
) -> Result<(), String> {
    distinct_refs(refs)?;
    for reference in refs {
        let mut bound = metadata
            .artifact_binding_refs
            .iter()
            .filter(|bound| bound.id == reference.id);
        if !metadata.grant.may_read_artifact(reference.id.as_str())
            || bound.next() != Some(reference)
            || bound.next().is_some()
        {
            return Err(format!(
                "artifact '{}' is not an exact readable metadata binding",
                reference.id
            ));
        }
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactArtifactRef {
    id: ArtifactId,
    content: ContentHash,
}

impl From<ExactArtifactRef> for ArtifactRef {
    fn from(reference: ExactArtifactRef) -> Self {
        Self {
            id: reference.id,
            content: reference.content,
        }
    }
}

fn deserialize_artifact_ref<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<ArtifactRef, D::Error> {
    ExactArtifactRef::deserialize(deserializer).map(Into::into)
}

fn deserialize_artifact_refs<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<ArtifactRef>, D::Error> {
    Vec::<ExactArtifactRef>::deserialize(deserializer)
        .map(|refs| refs.into_iter().map(Into::into).collect())
}

fn distinct_refs(refs: &[ArtifactRef]) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for reference in refs {
        exact_string(reference.id.as_str(), "artifact id")?;
        if !ids.insert(reference.id.as_str()) {
            return Err("artifact refs repeat a logical identity".into());
        }
    }
    Ok(())
}

fn merge_refs(first: &[ArtifactRef], second: &[ArtifactRef]) -> Result<Vec<ArtifactRef>, String> {
    distinct_refs(first)?;
    distinct_refs(second)?;
    let mut refs = first.to_vec();
    for reference in second {
        if let Some(existing) = refs.iter().find(|item| item.id == reference.id) {
            if existing != reference {
                return Err("context artifact selected at conflicting hashes".into());
            }
        } else {
            refs.push(reference.clone());
        }
    }
    Ok(refs)
}

fn exact_string(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty() || value.trim() != value || value.contains(['\r', '\n', '\0']) {
        return Err(format!("{field} must be a nonempty exact string"));
    }
    Ok(())
}

fn absolute_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(format!(
            "native composition path '{}' must be absolute without traversal",
            path.display()
        ));
    }
    Ok(())
}

fn validate_defaults(defaults: &RunDefaults) -> Result<(), String> {
    distinct_refs(&defaults.context_artifacts)?;
    if let Some(template) = &defaults.client_template_interface_id { exact_string(template, "client template")?; }
    for (field, value) in [
        ("ee_id", &defaults.ee_id),
        ("worker template", &defaults.worker_template_interface_id),
        ("provider interface", &defaults.provider_interface_id),
        ("model", &defaults.model),
        ("reasoning_effort", &defaults.reasoning_effort),
        ("authoring source", &defaults.authoring.source_interface_id),
        ("authoring room", &defaults.authoring.room_interface_id),
        ("authoring tool", &defaults.authoring.tool_id),
        ("tool call label", &defaults.authoring.tool_call_label),
        ("tool call mark", &defaults.authoring.tool_call_mark),
        ("tool call verifier", &defaults.authoring.tool_call_verifier),
    ] {
        exact_string(value, field)?;
    }
    if defaults.authoring.result_timeout_ms == 0 {
        return Err("authoring timeout must be positive".into());
    }
    for paths in defaults.door_host_slots.values() {
        for path in paths.values() {
            absolute_path(path)?;
        }
    }
    for id in defaults
        .return_interface_ids
        .iter()
        .chain(&defaults.worker_environment_kinds)
    {
        exact_string(id, "default exact selection")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_interfaces::{ArtifactBindingPayload, InterfaceArtifactFile};

    fn writing() -> NativePending {
        let effect = NativeEffect {
            tool: "exact.writer".into(),
            arguments: json!({"writes":[]}),
            correlation_id: "write-1".into(),
        };
        NativePending {
            call_id: "native-call".into(),
            operation_name: "create_work".into(),
            source_run: "source".into(),
            effect,
            state: NativePhase::Writing {
                created: CreatedRun {
                    run_id: "created".into(),
                    worker_interface_id: "system_worker_created".into(),
                    provider_interface_id: "world_provider_created".into(),
                    receive_interface_id: "system_worker_created".into(),
                    return_interface_ids: vec![],
                    model: "chosen".into(),
                    reasoning_effort: "xhigh".into(),
                },
                files: vec![FileProof {
                    path: "/tmp/native-created.json".into(),
                    before_hash: None,
                    after_hash: ContentHash::of_bytes(b"{}").to_hex(),
                    bytes: 2,
                }],
                seed: NativeEffect {
                    tool: "exact.sender".into(),
                    arguments: json!({"interface":"system_worker_created","label":PEER_MESSAGE_LABEL,"payload":{"action":"followup","control_id":"work-1","text":"task"}}),
                    correlation_id: "seed-1".into(),
                },
                outgoing_bindings: vec![ProviderOutgoingBinding {
                    interface: "system_worker_created".into(),
                    artifact_label: PEER_MESSAGE_LABEL.into(),
                    artifact_bindings: vec!["role-created".into()],
                }],
            },
        }
    }

    #[test]
    fn native_creation_replay_requires_exact_receipts_before_seed_and_never_claims_work() {
        let pending = writing();
        let NativePhase::Writing { files, .. } = &pending.state else {
            unreachable!()
        };
        let receipt = json!(ToolResultArtifact::success(
            "exact.writer",
            Some("write-1".into()),
            json!({"result":"files_written","writes":files})
        ));
        let mut wrong = receipt.clone();
        wrong["body"]["writes"][0]["after_hash"] = json!(ContentHash::of_bytes(b"wrong").to_hex());
        assert!(pending.advance(&wrong).is_err());
        wrong = receipt.clone();
        wrong["correlation_id"] = json!("foreign");
        assert!(pending.advance(&wrong).is_err());
        wrong = receipt.clone();
        wrong["body"]["writes"] = json!([]);
        assert!(pending.advance(&wrong).is_err());
        let restored: NativePending = serde_json::from_value(json!(pending)).unwrap();
        let NativeStep::Effect {
            effect,
            pending: next,
            outgoing_bindings,
        } = restored.advance(&receipt).unwrap()
        else {
            panic!("expected seed")
        };
        assert_eq!(effect.tool, "exact.sender");
        assert_eq!(next.call_id, restored.call_id);
        assert_eq!(next.operation_name, "create_work");
        assert_eq!(
            outgoing_bindings[0].artifact_bindings[0].as_str(),
            "role-created"
        );
        let next: NativePending = serde_json::from_value(json!(next)).unwrap();
        let admitted = json!(ToolResultArtifact::success(
            "exact.sender",
            Some("seed-1".into()),
            json!({"admitted":{"seq":17}})
        ));
        let NativeStep::Complete { result } = next.advance(&admitted).unwrap() else {
            panic!("expected result")
        };
        assert_eq!(result["installed"], true);
        assert_eq!(result["seed_admitted"], true);
        assert_eq!(result["work_completed"], false);
        assert_eq!(result["operation"], "create_work");
        let mut corrupt = admitted;
        corrupt["body"]["admitted"]["other"] = json!(true);
        assert!(next.advance(&corrupt).is_err());
    }

    #[test]
    fn catalog_is_exact_bound_and_never_advertises_ungranted_effects() {
        let dir = tempfile::tempdir().unwrap();
        let reference = ArtifactRef {
            id: "catalog".into(),
            content: ContentHash::of_bytes(b"catalog manifest"),
        };
        let catalog = json!({"id":"catalog","label":"provider.run.affordances","operations":[
            {"name":"queue_work","description":"Queue a message","input_schema":{"type":"object"},"operation":"send_message"},
            {"name":"declared_runs","description":"List recorded bindings","input_schema":{"type":"object"},"operation":"list_runs"}
        ]});
        let metadata: InvocationMetadata = serde_json::from_value(json!({
            "grant":{"artifact_access":{"catalog":{"read":true}},"artifact_verbs":["create"]},"artifact_binding_refs":[reference]
        })).unwrap();
        let mut input = InterfaceInvocationEnvelope {
            artifact: reference.clone(),
            artifact_file: InterfaceArtifactFile::write(dir.path(), "input", b"{}").unwrap(),
            artifact_binding_payloads: vec![ArtifactBindingPayload::new(
                reference.clone(),
                serde_json::to_vec(&catalog).unwrap(),
            )],
            metadata: Some(metadata),
        };
        let config = json!({"run_affordances":{"catalog_artifact":reference,"artifact_send_tool":"selected.sender"}});
        assert!(
            RunAffordances::from_invocation(&input, &json!({}))
                .unwrap()
                .is_none()
        );
        let allowed = RunAffordances::from_invocation(&input, &config)
            .unwrap()
            .unwrap();
        assert!(!allowed.contains("queue_work"));
        assert!(allowed.contains("declared_runs"));
        input
            .metadata
            .as_mut()
            .unwrap()
            .grant
            .tools
            .push("selected.sender".into());
        assert!(RunAffordances::from_invocation(&input, &config).is_err());
        input
            .metadata
            .as_mut()
            .unwrap()
            .grant
            .interface_affordance_specs
            .push(InterfaceToolAffordance {
                name: "selected.sender".into(),
                description: "Send".into(),
                args_schema: json!({"type":"object"}),
            });
        assert!(
            RunAffordances::from_invocation(&input, &config)
                .unwrap()
                .unwrap()
                .contains("queue_work")
        );
        input.metadata.as_mut().unwrap().artifact_binding_refs[0].content =
            ContentHash::of_bytes(b"different");
        assert!(RunAffordances::from_invocation(&input, &config).is_err());
        assert!(RunAffordances::from_invocation(&input, &json!({"run_affordances":null})).is_err());
        input.metadata.as_mut().unwrap().artifact_binding_refs[0] = reference;
        let mut malformed = catalog;
        malformed["operations"][0]["input_schema"]["properties"] = json!(7);
        input.artifact_binding_payloads[0].bytes = serde_json::to_vec(&malformed).unwrap();
        assert!(RunAffordances::from_invocation(&input, &config).is_err());
    }

    #[test]
    fn peer_contract_rejects_fake_origin_empty_work_and_unbound_targets() {
        assert!(
            serde_json::from_value::<PeerMessage>(json!({"action":"queue","control_id":"c"}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<PeerMessage>(
                json!({"action":"queue","control_id":"c","text":"hello","source_run":"forged"})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<PeerMessage>(json!({"action":"interrupt","control_id":"c"}))
                .is_ok()
        );
        assert!(serde_json::from_value::<PeerMessage>(json!({"action":"queue","control_id":"c","artifacts":[{"id":"a","content":ContentHash::of_bytes(b"a"),"source_run":"forged"}]})).is_err());
        let mut metadata: InvocationMetadata = serde_json::from_value(json!({"grant":{
            "interfaces":["system_worker_peer"],"attached_interface_bindings":[{"interface":"system_worker_peer","interface_type":"system","role":"worker","direction":"outbound"}]},
            "outbound_interface_bindings":["world_provider"],
            "checkpoint_binding_policy":{"inbound":{"interface":"system_worker_source","config":{"artifact_policy":{"mode":"all"}}},"outbound":[{"interface":"world_provider","config":{"artifact_policy":{"mode":"all"}}}]}
        })).unwrap();
        let mut record: RunRecord = serde_json::from_value(json!({
            "id":"source","ee":"scope","worker":"system_worker_source","trace":"trace","status":"running",
            "checkpoints":[{"id":"send","run":"source","trace":"trace","epoch":0,"interface_binding":{"interface":"world_provider","config":{"artifact_policy":{"mode":"all"}}},"outbound_interface_bindings":[{"interface":"system_worker_peer","config":{"artifact_policy":{"mode":"all"}}}]}],
            "active_checkpoints":["send"]
        })).unwrap();
        assert!(require_peer_target(&metadata, &record, "system_worker_peer", None).is_ok());
        let reference = ArtifactRef { id: "catalog".into(), content: ContentHash::of_bytes(b"catalog") };
        metadata.artifact_binding_refs = vec![reference.clone()];
        metadata.grant.artifact_access.insert("catalog".into(), serde_json::from_value(json!({"read":true})).unwrap());
        metadata.grant.artifact_verbs = serde_json::from_value(json!(["create"])).unwrap();
        metadata.grant.tools = vec!["selected.sender".into()];
        metadata.grant.interface_affordance_specs = vec![InterfaceToolAffordance {
            name: "selected.sender".into(), description: "Send".into(), args_schema: json!({"type":"object"}),
        }];
        metadata.runtime = Some(serde_json::from_value(json!({"run":"source","ee":"scope","worker":"system_worker_source",
            "checkpoint":"input","interface":"world_provider","epoch":0})).unwrap());
        for operation in [RunOperation::SendMessage, RunOperation::FollowupTask, RunOperation::Interrupt] {
            let affordances = RunAffordances {
                config: RunAffordanceConfig { catalog_artifact: reference.clone(), file_write_tool: None,
                    artifact_send_tool: Some("selected.sender".into()), roots: None, defaults: None },
                catalog: RunCatalog { id: "catalog".into(), label: "provider.run.affordances".into(), text: None,
                    operations: vec![RunCatalogEntry { name: "peer_action".into(), description: "Peer action".into(),
                        input_schema: json!({"type":"object"}), operation }] },
                enabled: BTreeSet::from(["peer_action".into()]),
            };
            let NativeStep::Effect { effect, outgoing_bindings, .. } = affordances.plan_call(
                &metadata, &record, &record, "call", "peer_action", &json!({"interface":"system_worker_peer","text":"hello"})
            ).unwrap() else { panic!("expected ordinary effect"); };
            assert_eq!(effect.tool, "selected.sender");
            assert_eq!(outgoing_bindings.len(), 1);
            assert_eq!(outgoing_bindings[0].interface.as_str(), "system_worker_peer");
            assert_eq!(outgoing_bindings[0].artifact_label, PEER_MESSAGE_LABEL);
            assert!(outgoing_bindings[0].artifact_bindings.is_empty());
            let mut unbound = record.clone(); unbound.active_checkpoints.clear();
            assert!(affordances.plan_call(&metadata, &record, &unbound, "call", "peer_action",
                &json!({"interface":"system_worker_peer","text":"hello"})).is_err());
        }
        record.active_checkpoints.clear();
        assert!(require_peer_target(&metadata, &record, "system_worker_peer", None).is_err());
        assert_ne!(
            call_identity("ab", "c").unwrap(),
            call_identity("a", "bc").unwrap()
        );
        assert_ne!(
            call_identity("run-a", "call").unwrap(),
            call_identity("run-b", "call").unwrap()
        );
        assert_eq!(
            call_identity("run-a", "call").unwrap(),
            call_identity("run-a", "call").unwrap()
        );
    }

    #[test]
    fn file_batch_keeps_new_file_preconditions_and_excludes_parent() {
        let write = PlannedClientWrite {
            path: "/tmp/new-run.json".into(),
            content: "{}\n".into(),
            tag: "run".into(),
            expected_before_hash: None,
        };
        let (batch, proofs) =
            file_write_batch(std::slice::from_ref(&write), Path::new("/tmp/parent.json")).unwrap();
        assert!(
            batch["writes"][0]
                .as_object()
                .unwrap()
                .contains_key("expected_before_hash")
        );
        assert!(batch["writes"][0]["expected_before_hash"].is_null());
        assert_eq!(proofs[0].bytes, 3);
        assert!(file_write_batch(std::slice::from_ref(&write), &write.path).is_err());
        assert!(file_write_batch(&[write.clone(), write], Path::new("/tmp/parent.json")).is_err());
    }
}
