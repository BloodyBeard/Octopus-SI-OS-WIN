use super::*;
use octopus_ledger::{Endpoint, MovementMetadata};
use octopus_ledger_index::{ArrivalDedupIndex, RetainedArrivalState};

pub(super) struct Cutover {
    pub run: Option<Value>,
    pub writes: Vec<PlannedClientWrite>,
    pub tools: BTreeMap<String, InterfaceId>,
    pub deferred: Option<String>,
}

pub(super) fn pin_authority(
    request: &NativeRunAdoptionRequest,
    record: &RunRecord,
    provider: &InterfaceId,
    config: &ClientConfig,
    preimages: &mut Vec<NativeRunAdoptionPreimage>,
) -> Result<(), String> {
    let registry = &request.roots.registry_root;
    for owner in [&record.worker, provider] {
        adoption_document(
            &registry
                .join("interface")
                .join(owner.as_str())
                .join("interface.json"),
            preimages,
        )?;
    }
    adoption_document(
        &registry
            .join("interface")
            .join(record.worker.as_str())
            .join("policy.json"),
        preimages,
    )?;
    let mut ee = Some(record.ee.clone());
    let mut visited = BTreeSet::new();
    while let Some(id) = ee {
        safe_segment(id.as_str(), "adoption EE")?;
        if !visited.insert(id.clone()) {
            return Err("adoption EE chain cycles".into());
        }
        let (value, _) = adoption_document(
            &request.roots.ee_instances_root.join(format!("{id}.json")),
            preimages,
        )?;
        let instance: octopus_ee::EeInstance =
            serde_json::from_value(value).map_err(|e| e.to_string())?;
        safe_segment(instance.template.as_str(), "adoption EE template")?;
        adoption_document(
            &registry
                .join("ee_template")
                .join(instance.template.as_str())
                .join("ee_template.json"),
            preimages,
        )?;
        ee = instance.parent_ee;
    }
    if let Some(result) = &config.run.tool_result_receiving {
        adoption_document(
            &registry
                .join("mark")
                .join(result.mark.as_str())
                .join("mark.json"),
            preimages,
        )?;
        let folder = registry.join("verifier").join(result.verifier.as_str());
        let (_, bytes) = adoption_document(&folder.join("verifier.json"), preimages)?;
        let verifier: octopus_verifiers::VerifierDescriptor =
            parse_strict(&bytes).map_err(|e| e.to_string())?;
        let script = Path::new(&verifier.script);
        if script.components().next().is_none()
            || script
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err("adoption verifier script is not owner-relative".into());
        }
        adoption_bytes(&folder.join(script), preimages)?;
    }
    Ok(())
}

pub(super) fn prepare(
    request: &NativeRunAdoptionRequest,
    config: &ClientConfig,
    record: &RunRecord,
    current: &Value,
    client: &InterfaceId,
    state: &Value,
    preimages: &mut Vec<NativeRunAdoptionPreimage>,
) -> Result<Cutover, String> {
    let mut result = Cutover {
        run: None,
        writes: Vec::new(),
        tools: BTreeMap::new(),
        deferred: None,
    };
    let Some(receiving) = &config.run.tool_result_receiving else {
        return Ok(result);
    };
    let provider = InterfaceId::new(adoption_field(state, "provider_runtime_interface")?);
    let bound: BoundRunAuthoringTopology = serde_json::from_value(
        current["interface_state"][provider.as_str()]["run_authoring_binding"].clone(),
    )
    .map_err(|e| e.to_string())?;
    let context = octopus_provider_run_authoring::provider_context_bindings(record, &provider)?;
    let mut next = current.clone();
    let mut topology = bound.clone();
    let mut removed = BTreeSet::new();
    let mut changed = false;
    for output in topology
        .checkpoints
        .iter_mut()
        .filter(|cp| cp.role == BoundCheckpointRole::ProviderToolOutput)
    {
        let [door] = output.outbound_interface_bindings.as_slice() else {
            return Err("result adoption requires an exact output door".into());
        };
        if bound.checkpoints.iter().any(|input| {
            input.role == BoundCheckpointRole::OwnerInput
                && input.interface_binding.interface == door.interface
        }) {
            if !output.result_receiving.is_empty() {
                return Err("persistent owner also declares result receipts".into());
            }
            continue;
        }
        let desired = tool_result_requirements(output, receiving)?;
        for tool in desired.keys() {
            safe_policy_axis_value(tool, "result adoption tool")?;
            if result
                .tools
                .insert(tool.clone(), door.interface.clone())
                .is_some()
            {
                return Err("result adoption repeats an exact tool across output contracts".into());
            }
        }
        if output.result_receiving == desired {
            if bound.checkpoints.iter().any(|input| {
                input.interface_binding.interface == door.interface
                    && input
                        .outbound_interface_bindings
                        .iter()
                        .any(|out| out.interface == provider)
                    && input.role != BoundCheckpointRole::ResultInput
            }) {
                return Err("result adoption found duplicate ordinary and result receiving contracts at one door".into());
            }
            continue;
        }
        if !output.result_receiving.is_empty() {
            result.deferred = Some("result receiving contract was locally authored; its exact requirements are preserved".into());
            return Ok(result);
        }
        let original_output = bound_checkpoint_template(
            BoundCheckpointRole::ProviderToolOutput,
            output.interface_binding.clone(),
            output.outbound_interface_bindings.clone(),
            output.required_verified_artifacts.clone(),
        )?;
        if output.name != original_output.name {
            result.deferred = Some("tool output differs from its exact client-authored predecessor; local topology is preserved".into());
            return Ok(result);
        }
        let inputs = bound
            .checkpoints
            .iter()
            .filter(|cp| {
                cp.interface_binding.interface == door.interface
                    && cp
                        .outbound_interface_bindings
                        .iter()
                        .any(|out| out.interface == provider)
            })
            .collect::<Vec<_>>();
        let [input] = inputs.as_slice() else {
            return Err("legacy result door has no unique declared receiving contract".into());
        };
        let original_input = bound_checkpoint_template(
            BoundCheckpointRole::Stable,
            AuthoredInterfaceBinding::all(door.interface.clone()),
            vec![AuthoredInterfaceBinding::all(provider.clone())],
            Vec::new(),
        )?;
        if input.role != BoundCheckpointRole::Stable
            || input.name != original_input.name
            || !input.include_context
            || input.owner_authoring.is_some()
            || !input.required_verified_artifacts.is_empty()
            || !input.result_receiving.is_empty()
            || input.interface_binding != AuthoredInterfaceBinding::all(door.interface.clone())
            || input.outbound_interface_bindings
                != [AuthoredInterfaceBinding::all(provider.clone())]
        {
            result.deferred = Some("legacy result input differs from its ordinary Stable predecessor; its authored contract is preserved".into());
            return Ok(result);
        }
        let rows = record
            .checkpoints
            .iter()
            .filter(|cp| {
                record.active_checkpoints.contains(&cp.id)
                    && cp.interface_binding.as_ref() == Some(&input.interface_binding)
                    && cp.outbound_interface_bindings == input.outbound_interface_bindings
                    && cp.required_verified_artifacts == input.required_verified_artifacts
            })
            .collect::<Vec<_>>();
        let [row] = rows.as_slice() else {
            return Err("legacy result input has no unique exact active row".into());
        };
        if row.artifact_bindings != context {
            return Err("legacy result input changed its exact context rights".into());
        }
        if !removed.insert(row.id.clone()) {
            return Err("legacy result input is claimed by several outputs".into());
        }
        output.result_receiving = desired;
        changed = true;
    }
    if !changed {
        return Ok(result);
    }
    let removed_templates = bound
        .checkpoints
        .iter()
        .filter(|input| {
            input.role == BoundCheckpointRole::Stable
                && record.checkpoints.iter().any(|row| {
                    removed.contains(&row.id)
                        && row.interface_binding.as_ref() == Some(&input.interface_binding)
                        && row.outbound_interface_bindings == input.outbound_interface_bindings
                        && row.required_verified_artifacts == input.required_verified_artifacts
                })
        })
        .map(|input| input.name.clone())
        .collect::<BTreeSet<_>>();
    topology
        .checkpoints
        .retain(|template| !removed_templates.contains(&template.name));
    if !topology
        .checkpoints
        .iter()
        .any(|cp| cp.role == BoundCheckpointRole::Stable && cp.include_context)
    {
        return Err(
            "result adoption would remove every explicitly bound future context input".into(),
        );
    }
    next["active_checkpoints"]
        .as_array_mut()
        .ok_or("result adoption active checkpoints absent")?
        .retain(|id| !removed.iter().any(|old| id.as_str() == Some(old.as_str())));
    if let Some(ids) = next["interface_state"][client.as_str()]
        .get_mut("cycle_checkpoint_ids")
        .and_then(Value::as_array_mut)
    {
        ids.retain(|id| !removed.iter().any(|old| id.as_str() == Some(old.as_str())));
    }
    next["interface_state"][provider.as_str()]["run_authoring_binding"] = json!(topology);
    let source_worker = adoption_field(state, "worker_choice_interface")?;
    safe_segment(source_worker, "result adoption source worker")?;
    let worker_root = request
        .roots
        .registry_root
        .join("interface")
        .join(record.worker.as_str());
    let (sidecar, _) = adoption_document(&worker_root.join("octopus_os.json"), preimages)?;
    let sidecar: OctopusOsInterfaceSidecar =
        serde_json::from_value(sidecar).map_err(|e| e.to_string())?;
    if sidecar.schema_version != 1
        || sidecar.workspace != "octopus_os"
        || sidecar.interface != record.worker.as_str()
        || sidecar.owner_interface != client.as_str()
        || sidecar.component != "worker"
        || sidecar.surface_role != adoption_field(state, "client_surface_role")?
    {
        return Err("result adoption worker is not owned by the exact client".into());
    }
    let (mut policy_value, before) =
        adoption_document(&worker_root.join("policy.json"), preimages)?;
    let mut policy: WorkerInterfacePolicy =
        serde_json::from_value(policy_value.clone()).map_err(|e| e.to_string())?;
    if policy.id != record.worker {
        return Err("result adoption worker ceiling has another identity".into());
    }
    if !policy.ceiling.mark_outcomes.contains(&receiving.mark) {
        let predecessors = config
            .result_receiving_updates
            .iter()
            .filter(|prior| prior.worker_template_interface == source_worker)
            .collect::<Vec<_>>();
        let [prior] = predecessors.as_slice() else {
            result.deferred = Some("private worker requires an explicitly declared correspondence-mark ceiling amendment".into());
            return Ok(result);
        };
        let (_, source) = adoption_document(
            &request
                .roots
                .registry_root
                .join("interface")
                .join(source_worker)
                .join("policy.json"),
            preimages,
        )?;
        let source: WorkerInterfacePolicy =
            serde_json::from_slice(&source).map_err(|e| e.to_string())?;
        let expected = prior
            .mark_outcomes
            .iter()
            .cloned()
            .chain(std::iter::once(receiving.mark.clone()))
            .collect::<BTreeSet<_>>();
        if prior.mark_outcomes.contains(&receiving.mark)
            || policy.ceiling.mark_outcomes != prior.mark_outcomes
            || source.id.as_str() != source_worker
            || source
                .ceiling
                .mark_outcomes
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                != expected
            || expected.len() != prior.mark_outcomes.len() + 1
            || source.ceiling.mark_outcomes.len() != expected.len()
        {
            result.deferred = Some("private worker mark ceiling differs from its declared predecessor; local policy is preserved".into());
            return Ok(result);
        }
        policy_value["ceiling"]["mark_outcomes"]
            .as_array_mut()
            .ok_or("worker mark ceiling absent")?
            .push(json!(receiving.mark));
        policy = serde_json::from_value(policy_value.clone()).map_err(|e| e.to_string())?;
        result.writes.push(PlannedClientWrite {
            path: worker_root.join("policy.json"),
            content: pretty_json(&policy_value)?,
            tag: format!("policy:{}", record.worker),
            expected_before_hash: Some(sha256_hex(&before)),
        });
    }
    let chain = resolve_live_ee_policy_chain(&request.roots.ee_instances_root, &record.ee)
        .map_err(|e| e.to_string())?;
    if !result_receiving_authorized(&request.roots.registry_root, &chain, &policy, receiving)? {
        result.deferred = Some("current EE policy excludes the correspondence mark; no automatic policy widening is authorized".into());
        return Ok(result);
    }
    for (tool, door) in &result.tools {
        let (declaration, _) = adoption_document(
            &request
                .roots
                .registry_root
                .join("interface")
                .join(door.as_str())
                .join("octopus_os.json"),
            preimages,
        )?;
        let declaration: OctopusOsInterfaceSidecar =
            serde_json::from_value(declaration).map_err(|e| e.to_string())?;
        if declaration.owner_interface != client.as_str()
            || declaration.interface != door.as_str()
            || declaration.schema_version != 1
            || declaration.workspace != "octopus_os"
            || declaration.component != "worker_environment_door"
            || declaration.surface_role != adoption_field(state, "client_surface_role")?
        {
            return Err("result adoption tool door is not owned by the exact client".into());
        }
        let (_, bytes) = adoption_document(
            &request
                .roots
                .registry_root
                .join("tool")
                .join(tool)
                .join("tool.json"),
            preimages,
        )?;
        let descriptor: ToolDescriptor = parse_strict(&bytes).map_err(|e| e.to_string())?;
        if descriptor.id.as_str() != tool
            || descriptor.interface != *door
            || !policy.ceiling.tools.iter().any(|id| id.as_str() == tool)
        {
            return Err(
                "result adoption tool differs from its exact declared door and ceiling".into(),
            );
        }
    }
    result.run = Some(next);
    Ok(result)
}

pub(super) fn outputs_settled(
    request: &NativeRunAdoptionRequest,
    record: &RunRecord,
    provider: &InterfaceId,
    pending: Option<&Value>,
    tools: &BTreeMap<String, InterfaceId>,
    max_row: u64,
    preimages: &mut Vec<NativeRunAdoptionPreimage>,
) -> Result<bool, String> {
    let path = request.ledger_root.join("movements.jsonl");
    let bytes = adoption_bytes(&path, preimages)?;
    let decoder = octopus_ledger_store::DurableMovementDecoder::for_log(&path);
    let mut index = ArrivalDedupIndex::new();
    let mut scopes = Vec::new();
    let owned = tools
        .values()
        .chain(std::iter::once(provider))
        .collect::<BTreeSet<_>>();
    for (sequence, line) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
        if !line.ends_with(b"\n") || line.len() as u64 > max_row {
            return Err("result adoption requires a complete bounded ledger prefix".into());
        }
        let row = decoder.decode(line, sequence).map_err(|e| e.to_string())?;
        if matches!(
            row.invocation_metadata,
            Some(MovementMetadata::ArrivalFanout(_))
        ) && matches!(&row.from, Endpoint::Interface(id) if owned.contains(id))
        {
            scopes.push(row.clone());
        }
        index
            .catch_up_movements(std::iter::once(row))
            .map_err(|e| e.to_string())?;
    }
    for scope in &scopes {
        if index
            .retained_arrival_edges(scope.seq.0)
            .map_err(|e| e.to_string())?
            .iter()
            .any(|(_, state)| !matches!(state, RetainedArrivalState::Finished(_)))
        {
            return Ok(false);
        }
    }
    let Some(pending) = pending else {
        return Ok(true);
    };
    let source = pending
        .get("source_movement_seq")
        .and_then(Value::as_u64)
        .ok_or("retained handoff source is malformed")?;
    for item in pending
        .get("items")
        .and_then(Value::as_array)
        .ok_or("retained handoff items are malformed")?
    {
        let hash = item
            .get("artifact_content_hash")
            .and_then(Value::as_str)
            .and_then(ContentHash::from_hex)
            .ok_or("retained output hash is malformed")?;
        let artifact = ArtifactRef {
            id: adoption_field(item, "artifact_id")?.into(),
            content: hash,
        };
        let checkpoint = item
            .get("checkpoint")
            .ok_or("retained output checkpoint absent")?;
        let matching = scopes
            .iter()
            .filter(|scope| {
                scope.from == Endpoint::Interface(provider.clone())
                    && scope.artifact.as_ref() == Some(&artifact)
                    && scope.seq.0 > source
            })
            .filter(|scope| {
                index
                    .retained_arrival_edges(scope.seq.0)
                    .is_ok_and(|edges| {
                        edges.iter().any(|(coordinate, _)| {
                            coordinate.run == record.id
                                && coordinate.worker == record.worker
                                && coordinate.interface == *provider
                                && checkpoint.get("id").and_then(Value::as_str)
                                    == Some(coordinate.checkpoint.as_str())
                                && checkpoint.get("epoch").and_then(Value::as_u64)
                                    == Some(coordinate.epoch)
                        })
                    })
            })
            .collect::<Vec<_>>();
        let [scope] = matching.as_slice() else {
            return Ok(false);
        };
        let Some(MovementMetadata::ArrivalFanout(metadata)) = &scope.invocation_metadata else {
            unreachable!()
        };
        let digest = metadata
            .arrival_fanout
            .identity
            .strip_prefix("provider-output:")
            .and_then(ContentHash::from_hex)
            .ok_or("retained provider output has no exact native identity")?;
        let identity_path = request
            .roots
            .interface_private_state_root
            .join(provider.as_str())
            .join("handoff-delivery")
            .join(format!("{}.json", digest.to_hex()));
        let (identity, bytes) = adoption_document(&identity_path, preimages)?;
        if ContentHash::of_bytes(&bytes) != digest
            || identity
                != json!({"schema_version":1,"provider":provider,
            "run":record.id,"worker":record.worker,"source_movement_seq":source,"item":item})
        {
            return Err("retained provider output changed its original dispatch identity".into());
        }
    }
    Ok(true)
}
