use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use octopus_code_mode_room::config::{
    CodeModeConfig, CodeModeContext, CodeModeOwnerTopology, OwnerCheckpoint,
};
use octopus_code_mode_room::continuation::{
    self, ApplicationDecision, ApplicationProjection, ApplicationRetirement, InstalledHandoff,
    PendingApplication, ReturnBoundary,
};
use octopus_descriptors::CheckpointId;
use octopus_interfaces::InterfaceArtifactFile;
use octopus_ledger::ContentHash;
use octopus_provider_run_authoring::{CheckedRunEdit, RunAuthoringProfile};
use octopus_tool_door_runtime::notify::{RecursiveMode, Watcher};
use octopus_tool_door_runtime::{
    DeliveryVerdict, InterfaceLiftContext, LedgerTailReader, ToolDoorOutputCallback,
    ToolDoorOutputSource,
};
use octopus_tool_handlers::{ToolCallArtifact, ToolResultArtifact};
use serde_json::{Value, json};

pub struct OwnerHandoff {
    states: tokio::sync::Mutex<BTreeMap<PathBuf, OwnerState>>,
    changed: Arc<tokio::sync::Notify>,
}

struct OwnerState {
    config: CodeModeConfig,
    topology: CodeModeOwnerTopology,
    expected_hash: String,
    authority: ToolDoorOutputSource,
    initial_source: octopus_ledger::Movement,
    idle: bool,
    generated_artifacts: Vec<octopus_provider_run_authoring::generated_artifacts::GeneratedArtifact>,
}

enum ApplicationChange {
    Request(PendingApplication),
    Retire(PendingApplication),
}

impl ApplicationChange {
    fn application(&self) -> &PendingApplication {
        match self { Self::Request(application) | Self::Retire(application) => application }
    }
    fn retiring(&self) -> bool { matches!(self, Self::Retire(_)) }
}

impl OwnerHandoff {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            states: tokio::sync::Mutex::new(BTreeMap::new()),
            changed: Arc::new(tokio::sync::Notify::new()),
        })
    }

    pub fn callback(self: &Arc<Self>) -> ToolDoorOutputCallback {
        let owner = Arc::clone(self);
        Arc::new(move |ctx, source, artifact| {
            let owner = Arc::clone(&owner);
            Box::pin(async move { owner.output(&ctx, source, artifact).await })
        })
    }

    async fn output(
        &self,
        ctx: &InterfaceLiftContext,
        source: ToolDoorOutputSource,
        artifact: InterfaceArtifactFile,
    ) -> Result<DeliveryVerdict, String> {
        let config: CodeModeConfig = serde_json::from_slice(
            &octopus_interfaces::read_artifact_carrier_file_bounded(
                &ctx.interface_registry_dir().join("code_mode.json"),
                1024 * 1024,
            )
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        config.validate(ctx.owner_interface().as_str())?;
        if matches!(config.context, CodeModeContext::Isolated) {
            return ctx.owner_hand_arrival(artifact).await;
        }
        let topology = config.resolve_owner_topology(&source.envelope)?;
        let formation = source
            .formation
            .as_ref()
            .ok_or("owner output has no exact formed room")?;
        let root = octopus_tool_door_runtime::tool_door_durable_state_dir(
            ctx,
            &formation.worker_environment_id,
        )?;
        let runtime = source.envelope.runtime().map_err(|e| e.to_string())?;
        let mut states = self.states.lock().await;
        let state = states.entry(root.clone()).or_insert_with(|| OwnerState {
            config: config.clone(),
            topology: topology.clone(),
            expected_hash: runtime.run_record_hash.clone().unwrap_or_default(),
            authority: source.clone(),
            initial_source: source.movement.clone(),
            idle: false,
            generated_artifacts: Vec::new(),
        });
        if state.config != config || state.topology != topology {
            return Err("code mode output changed its admitted owner topology".into());
        }
        if let Some(installed) = continuation::reconcile_checked_provider_advance(ctx,&root,&config,None)? {
            state.expected_hash = installed.after_hash;
        }
        let source_hash = runtime
            .run_record_hash
            .as_deref()
            .ok_or("owner input has no pinned run bytes")?;
        if source_hash != state.expected_hash && source.movement != state.initial_source {
            let installed: InstalledHandoff =
                continuation::read_record(&root.join(format!("{source_hash}.installed.json")))?;
            if installed.after_hash != source_hash {
                return Err("owner output source is not an admitted installed snapshot".into());
            }
        }
        state.idle = false;
        let captured = artifact
            .read_bounded(1024 * 1024, config.max_output_bytes as u64)
            .map_err(|e| e.to_string())?;
        let artifact_ref = continuation::artifact_ref(&artifact)?;
        if captured.manifest.label == "code_mode.artifact_boundary" {
            let limits = ctx.owner_io_limits()?;
            let evidence = octopus_provider_run_authoring::CheckedRunEvidence {
                movements_path:ctx.owner_movements_path(),ledger_index_path:ctx.owner_ledger_index_path(),blobs_dir:ctx.owner_blobs_dir(),
                max_control_frame_bytes:limits.max_control_frame_bytes,max_manifest_bytes:limits.max_manifest_bytes,max_payload_bytes:limits.max_payload_bytes,
            };
            let generated = octopus_provider_run_authoring::generated_artifacts::admitted_generated_artifacts(&evidence,&source.movement,&source.envelope)?;
            if generated.is_empty() { return Err("artifact boundary has no admitted generated artifacts".into()); }
            for output in generated {
                if let Some(old) = state.generated_artifacts.iter().find(|old| old.artifact.id == output.artifact.id) {
                    if old != &output { return Err("generated artifact identity changed".into()); }
                } else { state.generated_artifacts.push(output); }
            }
            let edge = state.topology.receipt.clone();
            self.install(ctx,&root,state,&edge,&artifact_ref.content.to_hex(),&captured.manifest.label,None).await?;
            continuation::record_internal_receipt(&root,&artifact_ref,source.movement.seq.0)?;
            return ctx.owner_hand_arrival(artifact).await;
        }
        if captured.manifest.label == "code_mode.application_boundary" {
            let body: Value =
                serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())?;
            let key = body
                .get("pending_digest")
                .and_then(Value::as_str)
                .ok_or("application boundary has no exact pending call")?;
            let application = continuation::pending_application(&root, key)?;
            if body.get("request_id").and_then(Value::as_str)
                != Some(application.request.request_id.as_str())
            {
                return Err("application boundary has another request identity".into());
            }
            let phase = body
                .get("phase")
                .and_then(Value::as_str)
                .ok_or("application boundary has no phase")?;
            return match phase {
                "request" => {
                    if application.request_artifact != source.envelope.artifact {
                        return Err(
                            "application projection is not from its original admitted GUI result"
                                .into(),
                        );
                    }
                    let edge = state.topology.notification.clone().ok_or("application request has no exact operator output")?;
                    let label = "operator.approval.request";
                    let projection = InterfaceArtifactFile::write(root.join("application-artifacts"), label,
                        &serde_json::to_vec(&application.request.projection()).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
                    self.install(ctx,&root,state,&edge,&artifact_ref.content.to_hex(),label,Some(ApplicationChange::Request(application.clone()))).await?;
                    ctx.owner_hand_arrival(projection).await
                }
                "retire" => {
                    let decision: ApplicationDecision = continuation::read_record(
                        &root.join(format!("{key}.application-decision.json")),
                    )?;
                    if decision.artifact != source.envelope.artifact
                        || decision.source_seq != source.movement.seq.0
                    {
                        return Err("application retirement is not from its original admitted operator decision".into());
                    }
                    let edge = state.topology.receipt.clone();
                    self.install(
                        ctx,
                        &root,
                        state,
                        &edge,
                        &artifact_ref.content.to_hex(),
                        "code_mode.application_boundary",
                        Some(ApplicationChange::Retire(application)),
                    )
                    .await?;
                    let receipt = InterfaceArtifactFile::write(root.join("application-artifacts"),"code_mode.application_boundary",&serde_json::to_vec(&json!({"correlation_id":format!("code-mode-decision:{}:{key}",formation.worker_environment_id),"request_id":decision.request_id,"phase":"retired"})).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
                    continuation::record_internal_receipt(
                        &root,
                        &continuation::artifact_ref(&receipt)?,
                        source.movement.seq.0,
                    )?;
                    ctx.owner_hand_arrival(receipt).await
                }
                _ => Err("unknown application boundary phase".into()),
            };
        }
        let mut terminal = false;
        let delivered_artifact = artifact.clone();
        let edge = if captured.manifest.label == "tool.call" {
            let call: ToolCallArtifact =
                serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())?;
            let pending = continuation::prepared_call_for_output(&root, &call, formation)?;
            state
                .topology
                .routes
                .iter()
                .find(|r| r.tool == pending.route().tool)
                .ok_or("nested call has no authored owner route")?
                .call
                .clone()
        } else if captured.manifest.label == "code_mode.notification" {
            let notification: Value =
                serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())?;
            if notification
                .get("context_generation")
                .and_then(Value::as_str)
                != Some(continuation::context_generation(&root)?.as_str())
            {
                return Err("notification has another context generation".into());
            }
            state
                .topology
                .notification
                .clone()
                .ok_or("notification has no authored output")?
        } else {
            let result: ToolResultArtifact =
                serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())?;
            if result.label != captured.manifest.label {
                return Err("owner result label mismatch".into());
            }
            terminal = true;
            let original: Value = serde_json::from_slice(
                &source
                    .envelope
                    .read_artifact_payload_bounded(config.max_output_bytes as u64)
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let internal = source
                .envelope
                .read_artifact_manifest()
                .map_err(|e| e.to_string())?
                .label
                != "tool.call";
            if internal {
                continuation::record_internal_receipt(
                    &root,
                    &continuation::artifact_ref(&delivered_artifact)?,
                    source.movement.seq.0,
                )?;
                state.topology.receipt.clone()
            } else {
                if original.get("tool").and_then(Value::as_str) != Some(result.tool.as_str())
                    || original.get("correlation_id").and_then(Value::as_str)
                        != result.correlation_id.as_deref()
                {
                    return Err(
                        "outer output does not match its original admitted model call".into(),
                    );
                }
                state.topology.model_output.clone()
            }
        };
        self.install(
            ctx,
            &root,
            state,
            &edge,
            &artifact_ref.content.to_hex(),
            &captured.manifest.label,
            None,
        )
        .await?;
        let verdict = ctx.owner_hand_arrival(delivered_artifact).await?;
        if terminal && matches!(verdict, DeliveryVerdict::Admitted { .. }) {
            let readiness = state.topology.readiness.clone();
            self.install(
                ctx,
                &root,
                state,
                &readiness,
                &format!("{}-idle", artifact_ref.content.to_hex()),
                "code_mode.frontier_ready",
                None,
            )
            .await?;
            state.idle = true;
            self.changed.notify_one();
        }
        Ok(verdict)
    }

    async fn install(
        &self,
        ctx: &InterfaceLiftContext,
        root: &Path,
        state: &mut OwnerState,
        edge: &OwnerCheckpoint,
        emission: &str,
        artifact_label: &str,
        application_change: Option<ApplicationChange>,
    ) -> Result<(), String> {
        loop {
        if ctx.owner_cancel().is_cancelled() { return Err("code mode authoring cancelled".into()); }
        if let Some(installed) = continuation::reconcile_checked_provider_advance(ctx,root,&state.config,None)? {
            state.expected_hash = installed.after_hash;
        }
        let descriptor = ctx.owner_descriptor()?;
        let private = &descriptor
            .interface_driver_config
            .as_ref()
            .ok_or("code mode door has no private config")?
            .private_config;
        let profile: RunAuthoringProfile = serde_json::from_value(
            private
                .get("run_authoring_profile")
                .cloned()
                .ok_or("code mode door has no independent checked authoring profile")?,
        )
        .map_err(|e| e.to_string())?;
        if profile.file_write_tool != state.topology.file_write_tool
            || profile.result_timeout_ms == 0
        {
            return Err("owner profile differs from admitted topology".into());
        }
        let run = &state.topology.run;
        if Path::new(run).components().count() != 1 || run == "." || run == ".." {
            return Err("malformed owner run id".into());
        }
        let run_path = ctx
            .owner_root_slot("run_records_root")?
            .canonicalize().map_err(|error| error.to_string())?
            .join(format!("{run}.json"));
        let current = octopus_interfaces::read_artifact_carrier_file_bounded(
            &run_path,
            ctx.owner_io_limits()?.max_payload_bytes,
        )
        .map_err(|e| e.to_string())?;
        if ContentHash::of_bytes(&current).to_hex() != state.expected_hash {
            return Err(
                "code mode authoring refuses a neighbor or external run-record change".into(),
            );
        }
        let before: Value = serde_json::from_slice(&current).map_err(|e| e.to_string())?;
        let mut next = before.clone();
        let owner = &state.topology.owner_interface;
        let previous_raw = before.pointer(&format!("/interface_state/{owner}/output_artifact_checkpoints"))
            .map(|value|serde_json::from_value::<Vec<String>>(value.clone()).map_err(|e|e.to_string())).transpose()?.unwrap_or_default();
        let recorded_generated = before.pointer(&format!("/interface_state/{owner}/generated_artifacts"))
            .map(|value|serde_json::from_value::<Vec<octopus_provider_run_authoring::generated_artifacts::GeneratedArtifact>>(value.clone()).map_err(|e|e.to_string())).transpose()?.unwrap_or_default();
        for output in recorded_generated {
            if let Some(existing) = state.generated_artifacts.iter().find(|candidate|candidate.artifact.id == output.artifact.id) {
                if existing != &output { return Err("recorded generated artifact changed".into()); }
            } else { state.generated_artifacts.push(output); }
        }
        let owned = next
            .get("interface_state")
            .and_then(|s| s.get(owner))
            .and_then(|s| s.get("frontier_checkpoint_ids"))
            .and_then(Value::as_array)
            .ok_or("selected workflow did not author this owner's independent frontier")?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or("owner frontier contains non-string id")
            })
            .collect::<Result<Vec<_>, _>>()?;
        let generation = continuation::digest(
            &json!({"before":state.expected_hash,"emission":emission,"edge":edge,"source_seq":state.authority.movement.seq.0}),
        )?;
        let prefix = format!("code_mode_{generation}");
        let output_id = format!("{prefix}_{}", edge.name);
        let result_id = format!("{prefix}_{}", state.topology.authoring_result.name);
        let mut bindings = state
            .authority
            .envelope
            .metadata()
            .map_err(|e| e.to_string())?
            .artifact_binding_refs
            .iter()
            .map(|r| json!({"id":r.id}))
            .collect::<Vec<_>>();
        let role_bindings = octopus_provider_run_authoring::generated_artifacts::mandatory_role_bindings(
            &state.authority.envelope.metadata().map_err(|e| e.to_string())?.grant,
        )?;
        if role_bindings.iter().any(|role| !bindings.iter().any(|binding| binding.get("id") == Some(&json!(role.id)))) {
            return Err("owner output lost an admitted mandatory role artifact".into());
        }
        for output in &state.generated_artifacts {
            if !bindings.iter().any(|binding| binding.get("id") == Some(&json!(output.artifact.id))) {
                bindings.push(json!({"id":output.artifact.id}));
            }
        }
        let trace = next.get("trace").cloned().ok_or("owner run has no trace")?;
        let row = |id: &str, checkpoint: &OwnerCheckpoint| {
            let selected = if checkpoint == &state.topology.model_output || state.topology.notification.as_ref() == Some(checkpoint) {
                bindings.iter().filter(|binding|!state.generated_artifacts.iter().any(|output|binding.get("id") == Some(&json!(output.artifact.id)))).cloned().collect::<Vec<_>>()
            } else { bindings.clone() };
            json!({"id":id,"run":run,"trace":trace,"epoch":0,"interface_binding":checkpoint.binding.inbound,"outbound_interface_bindings":checkpoint.binding.outbound,"artifact_bindings":selected,"required_verified_artifacts":[]})
        };
        let mut paired_to_remove = Vec::new();
        if let Some(ApplicationChange::Retire(application)) = &application_change {
            let projection: ApplicationProjection =
                continuation::read_record(&root.join(format!(
                    "{}.{}.application-projected.json",
                    application.pending_digest, state.expected_hash
                )))?;
            paired_to_remove = projection.checkpoints;
        }
        let mut paired_to_add = Vec::new();
        if let Some(change) = &application_change {
            if !change.retiring() {
                let application = change.application();
                application.request.validate_bound_files(state.authority.envelope.metadata().map_err(|e| e.to_string())?)?;
                let route = state.topology.routes.iter().find(|route| route.tool == application.request.original.tool)
                    .ok_or("application has no exact declared route")?;
                let client = state.topology.notification.as_ref().ok_or("application has no exact task client")?.binding.outbound[0].interface.clone();
                let owner_interface = state.topology.receipt.binding.outbound[0].interface.clone();
                let door = route.call.binding.outbound[0].interface.clone();
                application.request.validate_operator_interface(&client)?;
                if door != application.request.original.door_interface || application.operator_interface != client {
                    return Err("application approval would target another exact client/tool owner".into());
                }
                let exact = |interface: &octopus_descriptors::InterfaceId, label: &str| json!({"interface":interface,
                    "config":{"artifact_policy":{"mode":"allow","names":[label],"namespaces":[]}}});
                for (suffix,mark,verifier) in [("approved","operator.approval","operator_approval"),("denied","operator.input.provided","operator_input")] {
                    if !state.authority.envelope.metadata().map_err(|e| e.to_string())?.grant.mark_outcomes.iter().any(|m| m == mark) {
                        return Err("application mark is absent from this formed owner grant".into());
                    }
                    let id = format!("application_{}_{}",application.pending_digest,suffix);
                    let from = &application.operator_interface;
                    let targets = vec![exact(&owner_interface,"operator.approval.response"), exact(&door,"operator.approval.response")];
                    paired_to_add.push(json!({"id":id,"run":run,"trace":trace,"epoch":0,"interface_binding":exact(from,"operator.approval.response"),
                        "outbound_interface_bindings":targets,"artifact_bindings":bindings,"required_verified_artifacts":[{"mark":mark,"artifact_label":"operator.approval.response",
                        "verifier":verifier,"params":application.request.verification_params()}]}));
                }
            }
        }
        let checkpoints = next
            .get_mut("checkpoints")
            .and_then(Value::as_array_mut)
            .ok_or("owner run checkpoints are absent")?;
        checkpoints.retain(|checkpoint| !checkpoint.get("id").and_then(Value::as_str).is_some_and(|id|previous_raw.iter().any(|old|old == id)));
        for id in &owned {
            let existing = checkpoints
                .iter()
                .find(|cp| cp.get("id").and_then(Value::as_str) == Some(id.as_str()))
                .ok_or("owned frontier names a missing checkpoint")?;
            let inbound = existing
                .pointer("/interface_binding/interface")
                .and_then(Value::as_str)
                .ok_or("owned frontier checkpoint has no inbound identity")?;
            if inbound != owner
                && inbound
                    != state
                        .topology
                        .authoring_result
                        .binding
                        .inbound
                        .interface
                        .as_str()
            {
                return Err("owner frontier attempted to replace a neighboring checkpoint".into());
            }
        }
        for id in &paired_to_remove {
            if !checkpoints
                .iter()
                .any(|cp| cp.get("id").and_then(Value::as_str) == Some(id.as_str()))
            {
                return Err("application retirement lost an exact paired checkpoint".into());
            }
        }
        checkpoints.retain(|cp| {
            !cp.get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| paired_to_remove.iter().any(|old| old == id))
        });
        for cp in &paired_to_add {
            if checkpoints.iter().any(|old| old.get("id") == cp.get("id")) {
                return Err("application approval was already projected".into());
            }
            checkpoints.push(cp.clone());
        }
        checkpoints.retain(|cp| {
            !cp.get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| owned.iter().any(|old| old == id))
        });
        checkpoints.push(row(&output_id, edge));
        checkpoints.push(row(&result_id, &state.topology.authoring_result));
        let pending = continuation::prepared_calls(root)?;
        let mut raw_checkpoints = Vec::new();
        let mut pending_owners = std::collections::BTreeSet::new();
        let mut return_epochs = BTreeMap::new();
        for call in &pending {
            let owner = call.process_owner().ok_or("pending call has no immutable declared process owner")?;
            if !pending_owners.insert(owner) {
                return Err("multiple unresolved calls share one declared process owner".into());
            }
            let route = state
                .topology
                .routes
                .iter()
                .find(|r| r.tool == call.route().tool)
                .ok_or("pending call lost its authored route")?;
            if octopus_code_mode_room::config::DeclaredToolOwner::from_descriptor(&route.process_descriptor)? != *owner {
                return Err("pending return changed its immutable process owner".into());
            }
            let raw_id = format!("nested_artifact_{}",call.pending_digest());
            let expected = row(&route.result.name, &route.result);
            if let Some(existing) = checkpoints
                .iter_mut()
                .find(|cp| cp.get("id").and_then(Value::as_str) == Some(route.result.name.as_str()))
            {
                let mut retained = expected.clone();
                retained["artifact_bindings"] = existing["artifact_bindings"].clone();
                retained["epoch"] = existing["epoch"].clone();
                if existing != &retained {
                    return Err("pending return checkpoint was changed outside its owner".into());
                }
                if !previous_raw.contains(&raw_id)
                    || matches!(&application_change, Some(ApplicationChange::Request(application))
                        if application.pending_digest == call.pending_digest()) {
                    existing["epoch"] = json!(existing["epoch"].as_u64()
                        .ok_or("pending return checkpoint has no epoch")?
                        .checked_add(1).ok_or("pending return checkpoint epoch overflow")?);
                }
                return_epochs.insert(route.result.name.clone(), existing["epoch"].as_u64()
                    .ok_or("pending return checkpoint has no epoch")?);
            } else {
                checkpoints.push(expected);
                return_epochs.insert(route.result.name.clone(), 0);
            }
            let raw_binding = octopus_provider_run_authoring::generated_artifacts::output_artifact_binding(route.result.binding.inbound.interface.clone(), &call.correlation())?;
            let raw = json!({"id":raw_id,"run":run,"trace":trace,"epoch":0,"interface_binding":raw_binding,"outbound_interface_bindings":[],"artifact_bindings":role_bindings,"required_verified_artifacts":[]});
            if let Some(existing) = checkpoints.iter().find(|cp| cp.get("id") == raw.get("id")) {
                if existing != &raw { return Err("pending raw artifact checkpoint changed".into()); }
            } else { checkpoints.push(raw); }
            raw_checkpoints.push(raw_id);
        }
        let active = next
            .get_mut("active_checkpoints")
            .and_then(Value::as_array_mut)
            .ok_or("owner run active frontier is absent")?;
        active.retain(|value|!value.as_str().is_some_and(|id|previous_raw.iter().any(|old|old == id)));
        active.retain(|cp| {
            !cp.as_str()
                .is_some_and(|id| owned.iter().any(|old| old == id))
        });
        active.retain(|cp| {
            !cp.as_str()
                .is_some_and(|id| paired_to_remove.iter().any(|old| old == id))
        });
        for cp in &paired_to_add {
            active.push(
                cp.get("id")
                    .cloned()
                    .ok_or("application checkpoint lost its identity")?,
            );
        }
        active.push(json!(output_id));
        active.push(json!(result_id));
        for call in &pending {
            let id = call
                .route()
                .result_checkpoint
                .as_deref()
                .ok_or("pending call has no result checkpoint")?;
            if !active.iter().any(|v| v.as_str() == Some(id)) {
                active.push(json!(id));
            }
        }
        for id in &raw_checkpoints {
            if !active.iter().any(|value| value.as_str() == Some(id)) { active.push(json!(id)); }
        }
        next.get_mut("interface_state")
            .and_then(|s| s.get_mut(owner))
            .and_then(Value::as_object_mut)
            .ok_or("owner state disappeared")?
            .insert(
                "frontier_checkpoint_ids".into(),
                json!([output_id, result_id]),
            );
        let mut complete_owned = next.get("interface_state").and_then(|s| s.get(owner)).and_then(|s| s.get("owned_checkpoint_ids")).and_then(Value::as_array).map(|ids| ids.iter().map(|id| id.as_str().map(str::to_owned).ok_or("invalid owned checkpoint identity")).collect::<Result<std::collections::BTreeSet<_>,_>>()).transpose()?.unwrap_or_default();
        complete_owned.retain(|id| !owned.contains(id) && !paired_to_remove.contains(id));
        complete_owned.retain(|id|!previous_raw.contains(id));
        complete_owned.extend([output_id.clone(),result_id.clone()]);
        complete_owned.extend(paired_to_add.iter().filter_map(|cp| cp.get("id").and_then(Value::as_str)).map(str::to_owned));
        complete_owned.extend(pending.iter().filter_map(|call| call.route().result_checkpoint.clone()));
        complete_owned.extend(raw_checkpoints.iter().cloned());
        let owner_state = next.get_mut("interface_state").and_then(|s|s.get_mut(owner)).and_then(Value::as_object_mut).ok_or("owner state disappeared")?;
        owner_state.insert("output_artifact_checkpoints".into(),json!(raw_checkpoints));
        owner_state.insert("generated_artifacts".into(),json!(state.generated_artifacts));
        next.get_mut("interface_state").and_then(|s| s.get_mut(owner)).and_then(Value::as_object_mut).ok_or("owner state disappeared")?.insert("owned_checkpoint_ids".into(),json!(complete_owned));
        let active_ids = next
            .get("active_checkpoints")
            .and_then(Value::as_array)
            .ok_or("active frontier disappeared")?;
        let active_owner_outputs = next
            .get("checkpoints")
            .and_then(Value::as_array)
            .ok_or("checkpoints disappeared")?
            .iter()
            .filter(|cp| {
                active_ids.iter().any(|id| Some(id) == cp.get("id"))
                    && cp
                        .pointer("/interface_binding/interface")
                        .and_then(Value::as_str)
                        == Some(owner.as_str())
            })
            .collect::<Vec<_>>();
        let mut admitted_outputs = Vec::new();
        for checkpoint in active_owner_outputs {
            let binding: octopus_interfaces::CheckpointBindingPolicySnapshot = serde_json::from_value(json!({
                "inbound":checkpoint.get("interface_binding").ok_or("owner output lost its binding")?,
                "outbound":checkpoint.get("outbound_interface_bindings").ok_or("owner output lost its recipients")?
            })).map_err(|e|e.to_string())?;
            if binding.inbound.admits(artifact_label) && binding.outbound.iter().all(|binding| binding.admits(artifact_label)) { admitted_outputs.push(checkpoint); }
        }
        if admitted_outputs.len() != 1
            || admitted_outputs[0].get("id").and_then(Value::as_str) != Some(output_id.as_str())
        {
            return Err("code mode output would fan out beyond its single authored output".into());
        }
        let correlation = format!(
            "code-mode-authoring:{}:{generation}",
            state
                .authority
                .formation
                .as_ref()
                .ok_or("authoring lost room formation")?
                .worker_environment_id
        );
        let edit = CheckedRunEdit::prepare(run_path.clone(), &current, next, correlation)?;
        for call in &pending {
            let route = state
                .topology
                .routes
                .iter()
                .find(|r| r.tool == call.route().tool)
                .ok_or("pending route disappeared")?;
            continuation::prepare_return(
                root,
                call,
                &ReturnBoundary {
                    checkpoint: route.result.name.clone(),
                    epoch: *return_epochs.get(&route.result.name)
                        .ok_or("pending return checkpoint lost its authored epoch")?,
                    run_record_hash: edit.after_hash.clone(),
                    binding: route.result.binding.clone(),
                },
            )?;
        }
        if edge == &state.topology.readiness {
            continuation::create_record(
                &root.join(format!("{}.wake-pin.json", edit.after_hash)),
                &ReturnBoundary {
                    checkpoint: output_id.clone(),
                    epoch: 0,
                    run_record_hash: edit.after_hash.clone(),
                    binding: edge.binding.clone(),
                },
            )?;
        }
        let result_boundary = ReturnBoundary {
            checkpoint: result_id.clone(),
            epoch: 0,
            run_record_hash: edit.after_hash.clone(),
            binding: state.topology.authoring_result.binding.clone(),
        };
        continuation::prepare_authoring_result(
            root,
            &edit.correlation_id,
            &profile.file_write_tool,
            &profile.file_write_result_label,
            &result_boundary,
            &edit.path,
            &edit.before_hash,
            &edit.after_hash,
            edit.content.len(),
        )?;
        continuation::create_record(&root.join(format!("{generation}.edit.json")), &edit)?;
        let artifact = edit.materialize(&root.join("authoring-artifacts"), &profile)?;
        let call_artifact = continuation::artifact_ref(&artifact)?;
        let file_door = state
            .topology
            .authoring_result
            .binding
            .inbound
            .interface
            .clone();
        let mut reader = LedgerTailReader::open_named(ctx, "code-mode-owner-authoring")?;
        let verdict = ctx
            .owner_hand_system_emission(artifact, &state.authority.movement, file_door.clone())
            .await?;
        let DeliveryVerdict::Admitted { seq: call_seq } = verdict else {
            return Err(format!("code mode authoring was refused: {verdict:?}"));
        };
        let cancel = ctx.owner_cancel();
        let timeout = std::time::Duration::from_millis(profile.result_timeout_ms);
        let limits = ctx.owner_io_limits()?;
        let evidence = octopus_provider_run_authoring::CheckedRunEvidence {
            movements_path: ctx.owner_movements_path(), ledger_index_path: ctx.owner_ledger_index_path(), blobs_dir: ctx.owner_blobs_dir(),
            max_control_frame_bytes: limits.max_control_frame_bytes, max_manifest_bytes: limits.max_manifest_bytes, max_payload_bytes: limits.max_payload_bytes,
        };
        let writer = octopus_provider_run_authoring::CheckedRunWriter {
            owner: ctx.owner_interface().clone(), source: ctx.owner_interface().clone(), door: file_door.clone(),
            tool: profile.file_write_tool.clone(), call_label: profile.file_write_label.clone(), result_label: profile.file_write_result_label.clone(),
        };
        let result = tokio::time::timeout(timeout, async {
            loop {
                if let Some(refused) = octopus_provider_run_authoring::checked_failed_run_write(&evidence, &writer, &edit, call_seq)? {
                    continuation::create_record(&root.join(format!("{generation}.refused.json")), &refused)?;
                    return Ok(None);
                }
                let rows = reader.read_new_uncommitted_limit(256)?;
                for own in &rows {
                    let envelope = reader.envelope(own)?;
                    let body: Value = serde_json::from_slice(
                        &envelope
                            .read_artifact_payload_bounded(8 * 1024 * 1024)
                            .map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?;
                    if body.get("correlation_id").and_then(Value::as_str)
                        == Some(edit.correlation_id.as_str())
                    {
                        let installed = octopus_interfaces::read_artifact_carrier_file_bounded(
                            &ctx.owner_blobs_dir().join(envelope.runtime().map_err(|error| error.to_string())?.run_record_hash.as_deref().ok_or("authoring result has no pinned snapshot")?),
                            ctx.owner_io_limits()?.max_payload_bytes,
                        )
                        .map_err(|e| e.to_string())?;
                        edit.validate_result(
                            &profile,
                            &own.movement,
                            &envelope,
                            call_seq,
                            &file_door,
                            ctx.owner_interface(),
                            &CheckpointId::new(&result_id),
                            &installed,
                        )?;
                        reader.commit_through(own)?;
                        return Ok::<_, String>(Some((own.movement.clone(), envelope)));
                    }
                    reader.commit_through(own)?;
                }
                if rows.len() == 256 {
                    continue;
                }
                reader.wait_for_change(&cancel).await?;
            }
        })
        .await
        .map_err(|_| "code mode checked authoring result timed out".to_string())??;
        let Some(result) = result else {
            let advanced = continuation::reconcile_checked_provider_advance(ctx, root, &state.config, None)?
                .ok_or("rejected authoring has no independent checked advance")?;
            if advanced.after_hash == edit.before_hash || advanced.after_hash == edit.after_hash {
                return Err("rejected authoring cannot retry without an independent checked effect".into());
            }
            state.expected_hash = advanced.after_hash;
            continue;
        };
        continuation::record_install(
            root,
            &InstalledHandoff {
                before_hash: edit.before_hash,
                after_hash: edit.after_hash.clone(),
                source_seq: call_seq,
                source_artifact: call_artifact,
                result_seq: result.0.seq.0,
                result_artifact: result.1.artifact.clone(),
            },
        )?;
        for application in continuation::applications(root)? {
            if application_change
                .as_ref()
                .is_some_and(|change| change.retiring() && change.application().pending_digest == application.pending_digest)
            {
                continue;
            }
            let after_record: octopus_ledger::ContentHash = ContentHash::of_bytes(edit.content.as_bytes());
            if after_record.to_hex() != edit.after_hash { return Err("application projection changed its installed prospective bytes".into()); }
            let suffixes = ["approved", "denied"];
            let checkpoints = suffixes.into_iter().map(|suffix|format!("application_{}_{}",application.pending_digest,suffix)).collect::<Vec<_>>();
            let after: Value = serde_json::from_str(&edit.content).map_err(|e| e.to_string())?;
            if checkpoints.iter().all(|id| {
                after
                    .get("active_checkpoints")
                    .and_then(Value::as_array)
                    .is_some_and(|ids| ids.iter().any(|v| v.as_str() == Some(id.as_str())))
            }) {
                continuation::record_application_projection(
                    root,
                    &ApplicationProjection {
                        request_id: application.request.request_id,
                        pending_digest: application.pending_digest,
                        checkpoints,
                        boundary: ReturnBoundary {
                            checkpoint: output_id.clone(),
                            epoch: 0,
                            run_record_hash: edit.after_hash.clone(),
                            binding: edge.binding.clone(),
                        },
                    },
                )?;
            }
        }
        if let Some(ApplicationChange::Retire(application)) = &application_change {
            let decision: ApplicationDecision = continuation::read_record(&root.join(format!(
                "{}.application-decision.json",
                application.pending_digest
            )))?;
            continuation::record_application_retirement(
                root,
                &ApplicationRetirement {
                    pending_digest: application.pending_digest.clone(),
                    before_hash: state.expected_hash.clone(),
                    after_hash: edit.after_hash.clone(),
                    decision_artifact: decision.artifact,
                    decision_seq: decision.source_seq,
                    paired_checkpoints: paired_to_remove,
                },
            )?;
        }
        state.expected_hash = edit.after_hash;
        state.authority = ToolDoorOutputSource {
            movement: result.0,
            envelope: result.1,
            formation: state.authority.formation.clone(),
        };
        return Ok(());
        }
    }

    pub async fn observe(self: Arc<Self>, ctx: InterfaceLiftContext) -> Result<(), String> {
        let watch_root = ctx.interface_var_dir();
        let changed = Arc::clone(&self.changed);
        let watch_error = Arc::new(std::sync::Mutex::new(None));
        let capture_error = Arc::clone(&watch_error);
        let mut watcher = octopus_tool_door_runtime::notify::recommended_watcher(
            move |event: Result<
                octopus_tool_door_runtime::notify::Event,
                octopus_tool_door_runtime::notify::Error,
            >| {
                if let Err(error) = event {
                    if let Ok(mut slot) = capture_error.lock() {
                        *slot = Some(error.to_string());
                    }
                }
                changed.notify_one();
            },
        )
        .map_err(|e| format!("owner readiness observer could not start: {e}"))?;
        watcher
            .watch(&watch_root, RecursiveMode::Recursive)
            .map_err(|e| e.to_string())?;
        let cancel = ctx.owner_cancel();
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut states = self.states.lock().await;
                for (root, state) in states.iter_mut() {
                    if !state.idle {
                        continue;
                    }
                    if let Some(ready) = continuation::ready_records(root)?.into_iter().next() {
                        if let Some(installed) = continuation::reconcile_checked_provider_advance(&ctx,root,&state.config,None)? {
                            state.expected_hash = installed.after_hash;
                        }
                        if ready.context_generation != continuation::context_generation(root)? {
                            return Err("readiness has a stale heap identity".into());
                        }
                        let file = InterfaceArtifactFile::write(
                            root.join("readiness-artifacts"),
                            "code_mode.frontier_ready",
                            &serde_json::to_vec(&ready).map_err(|e| e.to_string())?,
                        )
                        .map_err(|e| e.to_string())?;
                        let verdict = ctx.owner_hand_arrival(file).await?;
                        continuation::create_record(
                            &root.join(format!("{}.wake-sent.json", ready.identity()?)),
                            &verdict,
                        )?;
                        match verdict {
                            DeliveryVerdict::Admitted { .. } => state.idle = false,
                            _ => {
                                return Err(format!(
                                    "fresh readiness admission refused: {verdict:?}"
                                ));
                            }
                        }
                    }
                }
            }
            if let Some(error) = watch_error
                .lock()
                .map_err(|_| "readiness observer error lock poisoned")?
                .take()
            {
                return Err(format!(
                    "owner readiness observation failed after reconciliation: {error}"
                ));
            }
            tokio::select! { _ = cancel.cancelled() => return Ok(()), _ = &mut notified => {} }
        }
    }
}
