use super::*;

pub(crate) fn plan_worker_pin(
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

pub(crate) fn formed_worker_role_artifacts<'a>(
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

pub(crate) fn worker_role_dependency_interfaces<'a>(
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
