use super::*;

pub(crate) fn preserve_selected_file_writer(
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
    pub(crate) fn owns_client(&self) -> bool {
        self.native_communication.as_ref().is_none_or(|routes| routes.client_template_interface_id.is_some())
    }
}

pub(crate) fn plan_identity(
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
