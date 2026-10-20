use super::*;

pub(crate) fn planned_tool_descriptor(
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
    copy_tool_descriptor(&mut descriptor, private_tool_id);
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

pub(crate) fn plan_private_tools(
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
        copy_tool_descriptor(&mut writer, &identity.authoring_tool_id);
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

pub(crate) fn narrow_run_authoring_tool_authority(descriptor: &mut ToolDescriptor) -> Result<(), String> {
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
