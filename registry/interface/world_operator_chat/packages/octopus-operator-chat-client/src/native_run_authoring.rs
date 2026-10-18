use super::*;
use octopus_ledger::{ArtifactRef, ContentHash};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod result_receiving;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    catalog: CatalogTemplate,
    file_write_tool: String,
    file_read_tool: Option<String>,
    artifact_send_tool: String,
    read_root_slots: Vec<String>,
    output_selection: run_composition::CompositionOutputSelection,
    return_to_source: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogTemplate {
    label: String,
    text: String,
    operations: Vec<Value>,
}

pub(super) struct PlanContext<'a> {
    pub registry_root: &'a Path,
    pub ee_instances_root: &'a Path,
    pub run_records_root: Option<&'a Path>,
    pub interface_private_state_root: Option<&'a Path>,
    pub config: &'a ClientConfig,
    pub identity: &'a PlannedIdentity,
    pub provider: &'a ResolvedProvider,
    pub topology: &'a FormedCapabilityTopology,
    pub door_host_slots: &'a BTreeMap<String, BTreeMap<String, PathBuf>>,
}

pub(super) fn plan(
    context: PlanContext<'_>,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<Option<ArtifactBindingSeed>, String> {
    let Some(raw) = context
        .provider
        .interface
        .interface_driver_config
        .as_ref()
        .and_then(|config| config.private_config.get("run_affordance_profile"))
    else {
        return Ok(None);
    };
    let profile = parse_profile(raw)?;
    let identity = context.identity;
    let provider_tag = format!("interface:{}", identity.model_interface_id);
    let provider_index = retain_write(
        writes,
        context.registry_root.join("interface").join(&identity.model_interface_id).join("interface.json"),
        provider_tag,
    )?;
    let mut provider: Value = serde_json::from_str(&writes[provider_index].content).map_err(|error| error.to_string())?;
    let private = provider.pointer_mut("/interface_driver_config/private_config")
        .and_then(Value::as_object_mut).ok_or("planned provider private_config must be an object")?;
    private.remove("run_affordances");
    let (Some(writer), Some(sender)) = (
        identity.capability_tools.get(&profile.file_write_tool),
        identity.capability_tools.get(&profile.artifact_send_tool),
    ) else {
        writes[provider_index].content = pretty_json(&provider)?;
        return Ok(None);
    };
    if writer == &identity.authoring_tool_id {
        writes[provider_index].content = pretty_json(&provider)?;
        return Ok(None);
    }
    let reader = profile.file_read_tool.as_ref().and_then(|tool| identity.capability_tools.get(tool));
    let required_root = |root: Option<&Path>, label: &str| -> Result<PathBuf, String> {
        root.filter(|path| path.is_absolute()).map(Path::to_path_buf)
            .ok_or_else(|| format!("native run authoring requires explicit absolute {label}"))
    };
    let roots = run_composition::CompositionRoots {
        registry_root: required_root(Some(context.registry_root), "registry_root")?,
        ee_instances_root: required_root(Some(context.ee_instances_root), "ee_instances_root")?,
        run_records_root: required_root(context.run_records_root, "run_records_root")?,
        interface_private_state_root: required_root(context.interface_private_state_root, "interface_private_state_root")?,
    };
    let life_index = retain_write(writes,
        context.registry_root.join("interface").join(&identity.model_interface_id).join("life.json"),
        format!("life:{}", identity.model_interface_id))?;
    let mut life: SpawnLifeConfig = serde_json::from_str(&writes[life_index].content).map_err(|error| error.to_string())?;
    for slot in &profile.read_root_slots {
        for process in &mut life.processes { process.authority.read_root_slots.insert(slot.clone()); }
    }
    writes[life_index].content = pretty_json(&serde_json::to_value(life).map_err(|error| error.to_string())?)?;
    let catalog_id = format!("artifact-run-affordances-{}", identity.chat_id);
    let tools = context.topology.tool_bindings.iter().flat_map(|(interface, binding)| {
        binding.tools.iter().map(|tool| SelectedToolSeed {
            tool: tool.tool.as_str().to_owned(), interface: interface.as_str().to_owned(),
            worker_environment_kind: tool.worker_environment_kind.as_str().to_owned(),
        })
    }).collect::<Vec<_>>();
    let config = context.config;
    let defaults = json!({
        "ee_id":identity.ee_id,"worker_template_interface_id":identity.worker_choice_interface,
        "provider_interface_id":identity.provider_choice_interface,"model":context.provider.model,
        "reasoning_effort":context.provider.reasoning_effort,"service_tier":context.provider.service_tier,
        "authoring":{
            "source_interface_id":config.authoring.source_interface,"room_interface_id":config.authoring.room_interface,
            "tool_id":config.authoring.tool,"tool_call_label":config.artifact_labels.tool_call,
            "tool_call_mark":config.run.tool_call_mark,"tool_call_verifier":config.run.tool_call_verifier,
            "result_timeout_ms":config.authoring.result_timeout_ms,
        },
        "worker_environment_kinds":context.topology.granted_worker_environment_kinds,
        "tools":tools,"door_host_slots":context.door_host_slots,"context_artifacts":[],
        "return_interface_ids":[],
    });
    let (affordances, catalog) = render(&profile, &catalog_id, json!({
        "file_write_tool":writer,"file_read_tool":reader,"artifact_send_tool":sender,"roots":roots,"defaults":defaults,
    }), identity.owns_client().then_some(config.interface.as_str()))?;
    private.insert("run_affordances".into(), affordances);
    writes[provider_index].content = pretty_json(&provider)?;
    push_json_write(context.registry_root.join("artifact").join(&catalog_id).join("artifact.json"),
        pretty_json(&catalog)?, format!("artifact:{catalog_id}"), writes)?;
    Ok(Some(ArtifactBindingSeed { id: catalog_id }))
}

fn parse_profile(raw: &Value) -> Result<Profile, String> {
    let profile: Profile = serde_json::from_value(raw.clone())
        .map_err(|error| format!("provider run_affordance_profile: {error}"))?;
    for (field, value) in [
        ("file_write_tool", &profile.file_write_tool),
        ("artifact_send_tool", &profile.artifact_send_tool),
        ("catalog.label", &profile.catalog.label),
        ("catalog.text", &profile.catalog.text),
    ] {
        if value.is_empty() || value.trim() != value {
            return Err(format!(
                "run_affordance_profile.{field} must be nonempty and trimmed"
            ));
        }
    }
    if profile.file_write_tool == profile.artifact_send_tool
        || profile.catalog.operations.is_empty()
    {
        return Err(
            "run_affordance_profile requires distinct effect tools and a nonempty catalog".into(),
        );
    }
    if let Some(reader) = &profile.file_read_tool {
        if reader.is_empty() || reader.trim() != reader
            || reader == &profile.file_write_tool || reader == &profile.artifact_send_tool
        {
            return Err("run_affordance_profile.file_read_tool must name a distinct nonempty tool".into());
        }
    }
    if profile.catalog.operations.iter().any(current_run_operation) && profile.file_read_tool.is_none() {
        return Err("current-run authoring catalog requires an explicit file_read_tool".into());
    }
    let mut slots = BTreeSet::new();
    for slot in &profile.read_root_slots {
        safe_segment(slot, "native authoring read root slot")?;
        if !slots.insert(slot) {
            return Err("native authoring repeats a read root slot".into());
        }
    }
    Ok(profile)
}

fn current_run_operation(operation: &Value) -> bool {
    matches!(operation.get("operation").and_then(Value::as_str), Some("preview_current_run" | "amend_current_run"))
}

fn render(profile: &Profile, catalog_id: &str, mut affordances: Value, client_template: Option<&str>) -> Result<(Value, Value), String> {
    let reader = affordances.get("file_read_tool").filter(|value| !value.is_null());
    let operations = profile.catalog.operations.iter()
        .filter(|operation| reader.is_some() || !current_run_operation(operation))
        .collect::<Vec<_>>();
    let catalog = json!({
        "id":catalog_id,"label":profile.catalog.label,"text":profile.catalog.text,
        "operations":operations,
    });
    let reference = ArtifactRef {
        id: catalog_id.into(),
        content: ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&catalog)),
    };
    let fields = affordances.as_object_mut().ok_or("native affordances must be an object")?;
    fields.insert("catalog_artifact".into(), json!(reference));
    let defaults = fields.get_mut("defaults").and_then(Value::as_object_mut).ok_or("native defaults must be an object")?;
    if !defaults.contains_key("client_template_interface_id") {
        defaults.insert("client_template_interface_id".into(), json!(client_template));
    }
    defaults.entry("output_selection").or_insert_with(||json!(profile.output_selection));
    defaults.entry("return_to_source").or_insert_with(||json!(profile.return_to_source));
    Ok((affordances, catalog))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRunAdoptionRequest {
    pub roots: run_composition::CompositionRoots,
    pub ledger_root: PathBuf,
    pub client_template_interface: String,
    pub run_id: String,
    pub expected_run_hash: String,
    pub expected_provider_hash: String,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeRunAdoptionStatus { Unchanged, Deferred, Planned }

#[derive(Serialize)]
pub struct NativeRunAdoptionPreimage {
    pub path: PathBuf,
    pub sha256: Option<String>,
}

#[derive(Serialize)]
pub struct NativeRunAdoptionPlan {
    pub status: NativeRunAdoptionStatus,
    pub run_id: String,
    pub provider_interface_id: String,
    pub source_movement_seq: Option<u64>,
    pub reason: Option<String>,
    pub writes: Vec<PlannedClientWrite>,
    pub preimages: Vec<NativeRunAdoptionPreimage>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolContractUpdate {
    source_tool: String,
    source_interface: String,
    predecessors: Vec<BTreeMap<String, Value>>,
}

enum ToolContractAdoption {
    Planned(Vec<PlannedClientWrite>),
    Deferred {
        locally_changed: Vec<String>,
        selected_tools: BTreeMap<String, InterfaceId>,
    },
}

struct CatalogAdoption {
    reference: ArtifactRef,
    affordances: Value,
    next: Value,
    profile: Profile,
    raw_profile: Value,
    write: Option<PlannedClientWrite>,
}

fn plan_catalog_adoption(
    roots: &run_composition::CompositionRoots, config: &ClientConfig, chat_id: &str,
    source: &str, private: &serde_json::Map<String, Value>,
    preimages: &mut Vec<NativeRunAdoptionPreimage>,
) -> Result<Option<CatalogAdoption>, String> {
    let Some(affordances) = private.get("run_affordances").cloned() else { return Ok(None); };
    let prior_raw = private.get("run_affordance_profile").ok_or("native adoption has no prior owner profile")?;
    let prior = parse_profile(prior_raw)?;
    let reference: ArtifactRef = serde_json::from_value(affordances.get("catalog_artifact").cloned().ok_or("native adoption catalog pin is absent")?).map_err(|e|e.to_string())?;
    if reference.id.as_str() != format!("artifact-run-affordances-{chat_id}") { return Err("native adoption catalog is outside its client owner".into()); }
    let catalog_path = roots.registry_root.join("artifact").join(reference.id.as_str()).join("artifact.json");
    let (catalog, catalog_bytes) = adoption_document(&catalog_path, preimages)?;
    let (_, baseline) = render(&prior, reference.id.as_str(), affordances.clone(), Some(&config.interface))?;
    if catalog != baseline || ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&catalog)) != reference.content {
        return Err("native adoption catalog differs from its exact prior owner-generated profile and pin".into());
    }
    let (source_descriptor, _) = adoption_document(&roots.registry_root.join("interface").join(source).join("interface.json"), preimages)?;
    let raw_profile = source_descriptor.pointer("/interface_driver_config/private_config/run_affordance_profile")
        .ok_or("native adoption source has no declared run_affordance_profile")?.clone();
    let profile = parse_profile(&raw_profile)?;
    if profile.file_write_tool != prior.file_write_tool || profile.file_read_tool != prior.file_read_tool
        || profile.artifact_send_tool != prior.artifact_send_tool {
        return Err("native adoption changes declared effect tools; an explicit capability amendment is required".into());
    }
    let mut declared = affordances.clone();
    let defaults = declared.get_mut("defaults").and_then(Value::as_object_mut).ok_or("native defaults must be an object")?;
    for (field, baseline) in [("output_selection",json!(prior.output_selection)),("return_to_source",json!(prior.return_to_source))] {
        if defaults.get(field) == Some(&baseline) { defaults.remove(field); }
    }
    let (next, next_catalog) = render(&profile, reference.id.as_str(), declared, Some(&config.interface))?;
    let changed = next != affordances || &raw_profile != prior_raw || next_catalog != catalog;
    let write = if changed { Some(PlannedClientWrite {
        path: catalog_path, content: pretty_json(&next_catalog)?,
        tag: format!("artifact:{}", reference.id), expected_before_hash: Some(sha256_hex(&catalog_bytes)),
    }) } else { None };
    Ok(Some(CatalogAdoption { reference, affordances, next, profile, raw_profile, write }))
}

fn plan_tool_contract_adoption(
    registry: &Path, declarations: Option<&Value>, record: &RunRecord, client: &InterfaceId,
    chat_id: &str, surface_role: &str, preimages: &mut Vec<NativeRunAdoptionPreimage>,
) -> Result<ToolContractAdoption, String> {
    let updates: Vec<ToolContractUpdate> = declarations.map(|value| serde_json::from_value(value.clone()))
        .transpose().map_err(|e|format!("client tool_contract_updates: {e}"))?.unwrap_or_default();
    if updates.is_empty() { return Ok(ToolContractAdoption::Planned(Vec::new())); }
    let (_, worker_bytes) = adoption_document(&registry.join("interface").join(record.worker.as_str()).join("policy.json"), preimages)?;
    let worker: WorkerInterfacePolicy = serde_json::from_slice(&worker_bytes).map_err(|e|e.to_string())?;
    if worker.id != record.worker { return Err("tool contract adoption worker pin identity differs".into()); }
    let mut declared = BTreeSet::new();
    let mut writes = Vec::new();
    let mut locally_changed = Vec::new();
    let mut selected_tools = BTreeMap::new();
    for update in updates {
        safe_policy_axis_value(&update.source_tool, "tool contract source tool")?;
        safe_segment(&update.source_interface, "tool contract source interface")?;
        if update.source_tool.trim() != update.source_tool || matches!(update.source_tool.as_str(), "." | "..") {
            return Err("tool contract source tool must be an exact directory identity".into());
        }
        if !declared.insert(update.source_tool.clone()) || update.predecessors.is_empty() {
            return Err("tool contract updates require unique source tools and explicit predecessors".into());
        }
        let tool = format!("{}__{chat_id}", update.source_tool);
        let door = InterfaceId::new(format!("{}_{chat_id}", update.source_interface));
        if !worker.ceiling.tools.iter().any(|id| id.as_str() == tool)
            || !record.checkpoints.iter().any(|cp| record.active_checkpoints.contains(&cp.id)
                && (cp.inbound_interface() == Some(&door) || cp.outbound_interfaces().any(|id| id == &door))) { continue; }
        let door_root = registry.join("interface").join(door.as_str());
        let (_, door_bytes) = adoption_document(&door_root.join("interface.json"), preimages)?;
        let door_descriptor: InterfaceDescriptor = parse_strict(&door_bytes).map_err(|e|e.to_string())?;
        let (sidecar, _) = adoption_document(&door_root.join("octopus_os.json"), preimages)?;
        let sidecar: OctopusOsInterfaceSidecar = serde_json::from_value(sidecar).map_err(|e|e.to_string())?;
        if door_descriptor.id != door || sidecar.schema_version != 1 || sidecar.workspace != "octopus_os"
            || sidecar.interface != door.as_str() || sidecar.owner_interface != client.as_str()
            || sidecar.component != "worker_environment_door" || sidecar.surface_role != surface_role {
            return Err(format!("tool contract adoption door '{door}' is not owned by this exact client"));
        }
        let (_, source_bytes) = adoption_document(&registry.join("tool").join(&update.source_tool).join("tool.json"), preimages)?;
        let mut next: ToolDescriptor = parse_strict(&source_bytes).map_err(|e|e.to_string())?;
        if next.id.as_str() != update.source_tool || next.interface.as_str() != update.source_interface {
            return Err("tool contract adoption source tool has another exact owner".into());
        }
        next.id = ToolDescriptorId::new(&tool); next.interface = door.clone();
        let next = serde_json::to_value(next).map_err(|e|e.to_string())?;
        let path = registry.join("tool").join(&tool).join("tool.json");
        let (_, before) = adoption_document(&path, preimages)?;
        let current: ToolDescriptor = parse_strict(&before).map_err(|e|e.to_string())?;
        if current.id.as_str() != tool || current.interface != door {
            return Err(format!("tool contract adoption '{tool}' has another exact identity or door"));
        }
        selected_tools.insert(tool.clone(), door);
        let current = serde_json::to_value(current).map_err(|e|e.to_string())?;
        let mut eligible = current == next;
        for fields in update.predecessors {
            if fields.is_empty() { return Err("tool contract predecessor must declare changed fields".into()); }
            let mut prior = next.clone();
            for (pointer, value) in &fields {
                if !(pointer == "/description" || pointer == "/args_schema" || pointer.starts_with("/args_schema/")
                    || pointer.starts_with("/handler_config/"))
                    || fields.keys().any(|other| other != pointer && pointer.starts_with(&format!("{other}/"))) {
                    return Err(format!("tool contract predecessor field '{pointer}' is outside a disjoint tool contract"));
                }
                *prior.pointer_mut(pointer).ok_or_else(||format!("tool contract predecessor field '{pointer}' is absent"))? = value.clone();
            }
            let _: ToolDescriptor = serde_json::from_value(prior.clone()).map_err(|e|e.to_string())?;
            eligible |= current == prior;
        }
        if !eligible {
            locally_changed.push(tool);
        } else if current != next {
            writes.push(PlannedClientWrite { path, content: pretty_json(&next)?, tag: format!("tool:{tool}"), expected_before_hash: Some(sha256_hex(&before)) });
        }
    }
    if locally_changed.is_empty() { Ok(ToolContractAdoption::Planned(writes)) }
    else { Ok(ToolContractAdoption::Deferred { locally_changed, selected_tools }) }
}

pub fn plan_existing_adoption(request: &NativeRunAdoptionRequest) -> Result<NativeRunAdoptionPlan, String> {
    use octopus_provider_run_authoring::{CheckedRunEvidence, declared_provider_run_writer, provider_context_bindings, provider_input_payloads_ready};
    let roots = &request.roots;
    for root in [&roots.registry_root, &roots.ee_instances_root, &roots.run_records_root,
        &roots.interface_private_state_root, &request.ledger_root] {
        if !root.is_absolute() || root.components().any(|part| matches!(part, std::path::Component::ParentDir | std::path::Component::CurDir)) {
            return Err("native adoption requires explicit normalized absolute roots".into());
        }
    }
    safe_segment(&request.run_id, "native adoption run")?;
    let interface_root = roots.registry_root.join("interface");
    let config = load_named_client_config(&interface_root, &request.client_template_interface)?;
    let mut preimages = Vec::new();
    let (client_config, _) = adoption_document(&interface_root.join(&config.interface).join("client.json"), &mut preimages)?;
    let run_path = roots.run_records_root.join(format!("{}.json", request.run_id));
    let (current, bytes) = adoption_document(&run_path, &mut preimages)?;
    if sha256_hex(&bytes) != request.expected_run_hash { return Err("native adoption run preimage changed".into()); }
    let record = RunRecord::from_bytes(&bytes).map_err(|e|e.to_string())?;
    if record.id.as_str() != request.run_id { return Err("native adoption run identity differs from its path".into()); }
    let clients = record.interface_state.iter().filter(|(_, state)| matches!(state.get("client_surface_role").and_then(Value::as_str), Some("home" | "task"))).collect::<Vec<_>>();
    let [(client, state)] = clients.as_slice() else { return Err("native adoption requires one exact client owner".into()); };
    let chat_id = client.as_str().strip_prefix(&config.run.interface_prefix).ok_or("native adoption client belongs to another template")?;
    safe_segment(chat_id, "native adoption client identity")?;
    let state = json!(state);
    let provider = adoption_field(&state, "provider_runtime_interface")?;
    let source = adoption_field(&state, "provider_choice_interface")?;
    safe_segment(provider, "native adoption provider")?;
    safe_segment(source, "native adoption source provider")?;
    let provider_id = InterfaceId::new(provider);
    for (owner, component) in [(client.as_str(), "client"), (provider, "provider")] {
        let (sidecar, _) = adoption_document(&interface_root.join(owner).join("octopus_os.json"), &mut preimages)?;
        let sidecar: OctopusOsInterfaceSidecar = serde_json::from_value(sidecar).map_err(|e|e.to_string())?;
        if sidecar.schema_version != 1 || sidecar.workspace != "octopus_os" || sidecar.interface != owner
            || sidecar.owner_interface != client.as_str() || sidecar.component != component
            || sidecar.surface_role != adoption_field(&state, "client_surface_role")?
            || component == "client" && (sidecar.provider_choice_interface.as_deref() != Some(source)
                || sidecar.worker_choice_interface.as_deref() != state.get("worker_choice_interface").and_then(Value::as_str)) {
            return Err(format!("native adoption interface '{owner}' is not owned by this exact client"));
        }
    }
    let provider_path = interface_root.join(provider).join("interface.json");
    let (mut descriptor, provider_bytes) = adoption_document(&provider_path, &mut preimages)?;
    if sha256_hex(&provider_bytes) != request.expected_provider_hash { return Err("native adoption provider preimage changed".into()); }
    let _: InterfaceDescriptor = parse_strict(&provider_bytes).map_err(|e|e.to_string())?;
    if descriptor.get("id").and_then(Value::as_str) != Some(provider) { return Err("native adoption provider identity differs from its path".into()); }
    let private = descriptor.pointer_mut("/interface_driver_config/private_config").and_then(Value::as_object_mut)
        .ok_or("native adoption provider has no private configuration")?;
    let pending = current.pointer(&format!("/interface_state/{provider}/pending_handoff"));
    let source_movement_seq = pending.map(|pending| pending.get("source_movement_seq").and_then(Value::as_u64)
        .ok_or("native adoption retained source is malformed")).transpose()?;
    let mut result = NativeRunAdoptionPlan {
        status: NativeRunAdoptionStatus::Unchanged, run_id: request.run_id.clone(), provider_interface_id: provider.into(),
        source_movement_seq, reason: None, writes: Vec::new(), preimages: Vec::new(),
    };
    result_receiving::pin_authority(request, &record, &provider_id, &config, &mut preimages)?;
    let cutover = result_receiving::prepare(request, &config, &record, &current, client, &state, &mut preimages)?;
    if let Some(reason) = cutover.deferred {
        result.status = NativeRunAdoptionStatus::Deferred;
        result.reason = Some(reason);
        result.preimages = preimages;
        return Ok(result);
    }
    let catalog = plan_catalog_adoption(roots, &config, chat_id, source, private, &mut preimages)?;
    let mut writes = match plan_tool_contract_adoption(&roots.registry_root, client_config.get("tool_contract_updates"),
        &record, client, chat_id, adoption_field(&state, "client_surface_role")?, &mut preimages)? {
        ToolContractAdoption::Planned(writes) => writes,
        ToolContractAdoption::Deferred { locally_changed, selected_tools } => {
            let chain = resolve_live_ee_policy_chain(&roots.ee_instances_root, &record.ee).map_err(|e|e.to_string())?;
            let formed = form_capability_topology(&roots.registry_root, &CapabilityFormationRequest::from_live_chain(record.worker.clone(), chain))
                .map_err(|e|e.to_string())?;
            for (tool, door) in selected_tools {
                if !formed.tool_bindings.get(&door).is_some_and(|binding| binding.tools.iter().any(|entry| entry.tool.as_str() == tool)) {
                    return Err(format!("tool contract adoption '{tool}' is outside the actual formed grant"));
                }
            }
            result.status = NativeRunAdoptionStatus::Deferred;
            result.reason = Some(format!("automatic tool contract adoption deferred for locally authored tools [{}]; current descriptors are preserved", locally_changed.join(", ")));
            result.preimages = preimages;
            return Ok(result);
        }
    };
    if cutover.run.is_none() && writes.is_empty() && catalog.as_ref().is_none_or(|catalog| catalog.write.is_none()) {
        result.preimages = preimages;
        return Ok(result);
    }
    if record.status.as_str() != Some("idle") || pending.is_some_and(|pending| pending.get("terminal") != Some(&Value::Bool(true))
        || pending.get("context_publication").is_some_and(|value| !value.is_null())) {
        result.status = NativeRunAdoptionStatus::Deferred;
        result.reason = Some("accepted provider work or context publication has not settled".into());
        result.preimages = preimages;
        return Ok(result);
    }
    let (life, _) = adoption_document(&interface_root.join(provider).join("life.json"), &mut preimages)?;
    let life: SpawnLifeConfig = serde_json::from_value(life).map_err(|e|e.to_string())?;
    let [process] = life.processes.as_slice() else { return Err("native adoption requires one exact provider process".into()); };
    process.io_limits.validate()?;
    let movements = request.ledger_root.join("movements.jsonl");
    let index = request.ledger_root.join("ledger-index.sqlite");
    let blobs = request.ledger_root.join("blobs");
    let evidence = CheckedRunEvidence { movements_path: &movements, ledger_index_path: &index, blobs_dir: &blobs,
        max_control_frame_bytes: process.io_limits.max_control_frame_bytes, max_manifest_bytes: process.io_limits.max_manifest_bytes,
        max_payload_bytes: process.io_limits.max_payload_bytes };
    let writer = declared_provider_run_writer(&record, &provider_id, &serde_json::from_value(
        private.get("run_authoring_profile").cloned().ok_or("native adoption provider has no declared writer profile")?
    ).map_err(|error|error.to_string())?)?;
    if !provider_input_payloads_ready(&evidence, &run_path, &bytes, &writer, client)? {
        result.status = NativeRunAdoptionStatus::Deferred;
        result.reason = Some("retained handoff lacks complete admitted evidence or a newer provider input is already admitted".into());
        result.preimages = preimages;
        return Ok(result);
    }
    let mut affected_tools = cutover.tools;
    affected_tools.insert(writer.tool.clone(), writer.door.clone());
    for write in &writes {
        let descriptor: ToolDescriptor = parse_strict(write.content.as_bytes()).map_err(|e|e.to_string())?;
        affected_tools.insert(descriptor.id.to_string(), descriptor.interface);
    }
    if !result_receiving::outputs_settled(request, &record, &provider_id, pending, &affected_tools,
        process.io_limits.max_control_frame_bytes, &mut preimages)? {
        result.status = NativeRunAdoptionStatus::Deferred;
        result.reason = Some("pending outputs retain unresolved original fan-out or lack exact retained delivery proof".into());
        result.preimages = preimages;
        return Ok(result);
    }
    if !affected_tools.is_empty() {
        use octopus_process_session_tool_runtime::{InterfaceHostIoLimits, ToolContractUpdateRequest, tool_contract_update_quiescent};
        let limits = InterfaceHostIoLimits {
            max_control_frame_bytes: process.io_limits.max_control_frame_bytes,
            max_stderr_bytes: process.io_limits.max_stderr_bytes,
            max_manifest_bytes: process.io_limits.max_manifest_bytes,
            max_payload_bytes: process.io_limits.max_payload_bytes,
        };
        let proof = tool_contract_update_quiescent(ToolContractUpdateRequest {
            movements_path: &movements, ledger_index_path: &index, blobs_dir: &blobs,
            interface_private_state_root: &roots.interface_private_state_root,
            affected_tools: &affected_tools, limits: &limits,
        })?;
        if !proof.quiescent {
            result.status = NativeRunAdoptionStatus::Deferred;
            result.reason = Some("accepted tool work can still dispatch or recover under the previous contract".into());
            result.preimages = preimages;
            return Ok(result);
        }
        preimages.extend(proof.preimages.into_iter().map(|(path, sha256)| NativeRunAdoptionPreimage { path, sha256 }));
    }
    let chain = resolve_live_ee_policy_chain(&roots.ee_instances_root, &record.ee).map_err(|e|e.to_string())?;
    result_receiving::pin_authority(request, &record, &provider_id, &config, &mut preimages)?;
    let formed = form_capability_topology(&roots.registry_root, &CapabilityFormationRequest::from_live_chain(record.worker.clone(), chain))
        .map_err(|e|e.to_string())?;
    for write in &writes {
        let next: ToolDescriptor = parse_strict(write.content.as_bytes()).map_err(|e|e.to_string())?;
        if !formed.tool_bindings.get(&next.interface).is_some_and(|binding| binding.tools.iter().any(|entry| entry.tool.as_str() == next.id.as_str())) {
            return Err(format!("tool contract adoption '{}' is outside the actual formed grant", next.id));
        }
    }
    for (tool, door) in &affected_tools {
        if !formed.tool_bindings.get(door).is_some_and(|binding| binding.tools.iter().any(|entry| entry.tool.as_str() == tool)) {
            return Err(format!("native adoption tool '{tool}' is outside the actual formed grant"));
        }
    }
    writes.extend(cutover.writes);
    if let Some(next) = cutover.run {
        let next_record = RunRecord::from_bytes(pretty_json(&next)?.as_bytes()).map_err(|e|e.to_string())?;
        let profile = serde_json::from_value(private.get("run_authoring_profile").cloned().ok_or("native result adoption has no writer profile")?).map_err(|e|e.to_string())?;
        if declared_provider_run_writer(&next_record, &provider_id, &profile)? != writer {
            return Err("native result adoption changed the accepted writer contract".into());
        }
        writes.push(PlannedClientWrite { path: run_path, content: pretty_json(&next)?, tag: format!("run:{}", record.id), expected_before_hash: Some(request.expected_run_hash.clone()) });
    }
    if let Some(catalog) = catalog.filter(|catalog| catalog.write.is_some()) {
        if catalog.profile.read_root_slots.iter().any(|slot| !process.authority.read_root_slots.contains(slot)) {
            return Err("native adoption requires undeclared provider read roots; an explicit capability amendment is required".into());
        }
        let bound: BoundRunAuthoringTopology = serde_json::from_value(current["interface_state"][provider]["run_authoring_binding"].clone()).map_err(|e|e.to_string())?;
        for name in ["file_write_tool", "file_read_tool", "artifact_send_tool"] {
            if name == "file_read_tool" && catalog.affordances.get(name).is_none_or(Value::is_null) { continue; }
            let tool = adoption_field(&catalog.affordances, name)?;
            let granted = formed.tool_bindings.iter().any(|(door, binding)| binding.tools.iter().any(|entry| entry.tool.as_str() == tool)
                && bound.checkpoints.iter().any(|cp| cp.role == BoundCheckpointRole::ProviderToolOutput
                    && cp.outbound_interface_bindings.iter().any(|out| out.interface == *door)
                    && cp.required_verified_artifacts.iter().any(|required| required.params.get("tools").and_then(Value::as_array)
                        .is_some_and(|tools| tools.iter().any(|value| value.as_str() == Some(tool))))));
            if !granted { return Err(format!("native adoption effect tool '{tool}' is not in the actual formed and authored grant")); }
        }
        if !provider_context_bindings(&record, &provider_id)?.iter().any(|binding| binding.id == catalog.reference.id && binding.access.read) {
            return Err("native adoption catalog has no existing readable provider binding".into());
        }
        private.insert("run_affordances".into(), catalog.next);
        private.insert("run_affordance_profile".into(), catalog.raw_profile);
        writes.push(catalog.write.unwrap());
        writes.push(PlannedClientWrite { path: provider_path, content: pretty_json(&descriptor)?, tag: format!("interface:{provider}"), expected_before_hash: Some(request.expected_provider_hash.clone()) });
    }
    result.status = NativeRunAdoptionStatus::Planned;
    result.writes = writes;
    let mut unique = BTreeMap::new();
    for pin in preimages {
        if unique.insert(pin.path.clone(), pin.sha256.clone()).is_some_and(|prior| prior != pin.sha256) {
            return Err("native adoption dependency changed between proof reads".into());
        }
    }
    let mut preimages = Vec::new();
    for (path, expected) in unique {
        let actual = match std::fs::read(&path) {
            Ok(bytes) => Some(sha256_hex(&bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
        };
        if actual != expected { return Err(format!("native adoption dependency '{}' changed across inspection", path.display())); }
        preimages.push(NativeRunAdoptionPreimage { path, sha256: expected });
    }
    result.preimages = preimages;
    Ok(result)
}

fn adoption_field<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value.get(field).and_then(Value::as_str).filter(|text| !text.is_empty() && text.trim() == *text)
        .ok_or_else(|| format!("native adoption requires exact '{field}'"))
}

fn adoption_document(path: &Path, preimages: &mut Vec<NativeRunAdoptionPreimage>) -> Result<(Value, Vec<u8>), String> {
    let bytes = adoption_bytes(path, preimages)?;
    let value = serde_json::from_slice(&bytes).map_err(|e|format!("native adoption '{}': {e}", path.display()))?;
    Ok((value, bytes))
}

fn adoption_bytes(path: &Path, preimages: &mut Vec<NativeRunAdoptionPreimage>) -> Result<Vec<u8>, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e|format!("native adoption '{}': {e}", path.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || path.canonicalize().map_err(|e|e.to_string())? != path { return Err(format!("native adoption '{}' must be an exact regular file", path.display())); }
    let bytes = std::fs::read(path).map_err(|e|e.to_string())?;
    preimages.push(NativeRunAdoptionPreimage { path: path.to_path_buf(), sha256: Some(sha256_hex(&bytes)) });
    Ok(bytes)
}

fn retain_write(
    writes: &mut Vec<PlannedClientWrite>,
    path: PathBuf,
    tag: String,
) -> Result<usize, String> {
    if let Some(index) = writes
        .iter()
        .position(|write| write.tag == tag && write.path == path)
    {
        return Ok(index);
    }
    let content = std::fs::read_to_string(&path).map_err(|error| {
        format!(
            "native authoring owner '{}' is unavailable: {error}",
            path.display()
        )
    })?;
    serde_json::from_str::<Value>(&content).map_err(|error| error.to_string())?;
    let index = writes.len();
    writes.push(PlannedClientWrite {
        path,
        expected_before_hash: Some(sha256_hex(content.as_bytes())),
        content,
        tag,
    });
    Ok(index)
}
