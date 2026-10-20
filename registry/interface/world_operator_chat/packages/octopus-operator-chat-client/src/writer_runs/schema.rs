use super::*;
use octopus_ledger::ArtifactRef;
use octopus_run_authoring::{RunAuthoringProfile, declared_provider_run_writer};
use octopus_tool_registry::ToolArgumentValidator;
use serde_json::Value;

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorWriterSchemaPlanRequest {
    pub registry_root: PathBuf,
    pub run_records_root: PathBuf,
    pub run_id: String,
    pub expected_run_sha256: String,
    pub writer: CheckedRunWriter,
    pub expected_owner_sha256: String,
    pub expected_tool_sha256: String,
    pub expected_args_schema: Value,
    pub args_schema: Value,
}

fn regular_bytes(path: &Path) -> Result<Vec<u8>, String> {
    if !std::fs::symlink_metadata(path)
        .map_err(|error| error.to_string())?
        .file_type()
        .is_file()
        || path.canonicalize().map_err(|error| error.to_string())? != path
    {
        return Err(format!(
            "writer schema source is not an exact regular path: {}",
            path.display()
        ));
    }
    std::fs::read(path).map_err(|error| error.to_string())
}

fn checked_bytes(path: &Path, expected: &str) -> Result<Vec<u8>, String> {
    let bytes = regular_bytes(path)?;
    if sha256_hex(&bytes) != expected {
        return Err(format!(
            "writer schema source changed before planning: {}",
            path.display()
        ));
    }
    Ok(bytes)
}

fn code_mode_topologies(
    registry: &Path,
    selected_tool: &str,
) -> Result<BTreeMap<String, Value>, String> {
    let mut topologies = BTreeMap::new();
    for entry in std::fs::read_dir(registry.join("interface")).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let sidecar = entry.path().join("code_mode.json");
        if !sidecar.try_exists().map_err(|error| error.to_string())? {
            continue;
        }
        let config: Value =
            serde_json::from_slice(&regular_bytes(&sidecar)?).map_err(|error| error.to_string())?;
        let Some(handoff) = config.get("handoff").filter(|value| !value.is_null()) else {
            continue;
        };
        let reference: ArtifactRef = serde_json::from_value(
            handoff
                .get("topology_artifact")
                .cloned()
                .ok_or("code mode handoff has no topology artifact")?,
        )
        .map_err(|error| error.to_string())?;
        safe_segment(reference.id.as_str(), "topology artifact")?;
        let topology: Value = serde_json::from_slice(&regular_bytes(
            &registry
                .join("artifact")
                .join(reference.id.as_str())
                .join("artifact.json"),
        )?)
        .map_err(|error| error.to_string())?;
        let owner = entry
            .file_name()
            .to_str()
            .ok_or("non-UTF8 code mode owner")?
            .to_owned();
        if topology.get("id").and_then(Value::as_str) != Some(reference.id.as_str())
            || topology.get("owner_interface").and_then(Value::as_str) != Some(owner.as_str())
            || octopus_ledger::ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(
                &topology,
            )) != reference.content
        {
            return Err("code mode schema adoption found a changed topology pin".into());
        }
        let routes = topology
            .get("routes")
            .and_then(Value::as_array)
            .ok_or("code mode topology routes absent")?;
        for route in routes {
            if route.get("tool").and_then(Value::as_str) == Some(selected_tool)
                || route
                    .pointer("/process_descriptor/id")
                    .and_then(Value::as_str)
                    == Some(selected_tool)
                || route
                    .pointer("/app_approval/resume_tool")
                    .and_then(Value::as_str)
                    == Some(selected_tool)
                || route
                    .pointer("/app_approval/resume_descriptor/id")
                    .and_then(Value::as_str)
                    == Some(selected_tool)
            {
                return Err("writer schema adoption refuses a tool pinned in a nested CodeMode route; settle and re-form its exact topology first".into());
            }
        }
        topologies.insert(owner, topology);
    }
    Ok(topologies)
}

pub fn plan_operator_writer_schema(
    request: &OperatorWriterSchemaPlanRequest,
) -> Result<Vec<PlannedClientWrite>, String> {
    let registry = require_absolute("registry_root", &request.registry_root)?
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let runs = require_absolute("run_records_root", &request.run_records_root)?
        .canonicalize()
        .map_err(|error| error.to_string())?;
    safe_segment(&request.run_id, "run_id")?;
    for id in [
        request.writer.owner.as_str(),
        request.writer.source.as_str(),
        request.writer.door.as_str(),
    ] {
        safe_segment(id, "writer identity")?;
    }
    let tool_id = safe_policy_axis_value(&request.writer.tool, "writer tool")?;
    if tool_id != request.writer.tool || matches!(tool_id.as_str(), "." | "..") {
        return Err("writer tool must name an exact registry path segment".into());
    }
    let path = runs.join(format!("{}.json", request.run_id));
    let record = RunRecord::from_bytes(&checked_bytes(&path, &request.expected_run_sha256)?)
        .map_err(|error| error.to_string())?;
    if record.id.as_str() != request.run_id {
        return Err("writer schema target Run identity differs from its path".into());
    }
    let owner_path = registry
        .join("interface")
        .join(request.writer.owner.as_str())
        .join("interface.json");
    let descriptor = parse_strict::<InterfaceDescriptor>(&checked_bytes(
        &owner_path,
        &request.expected_owner_sha256,
    )?)
    .map_err(|error| error.to_string())?;
    if descriptor.id != request.writer.owner {
        return Err("writer schema owner identity differs from its path".into());
    }
    let private = &descriptor
        .interface_driver_config
        .as_ref()
        .ok_or("writer schema owner has no configuration")?
        .private_config;
    let state = record
        .interface_state
        .get(&request.writer.owner)
        .ok_or("writer schema owner has no Run declaration")?;
    let topologies = code_mode_topologies(&registry, &request.writer.tool)?;
    let declared = if let Some(intake) = state.get("input_intake") {
        let config: crate::input_intake::InputIntakeConfig =
            serde_json::from_value(intake.clone()).map_err(|error| error.to_string())?;
        config.validate(&request.writer.owner)?;
        if config.run != request.run_id
            || !config.matches_declaration(
                private
                    .get("input_intake")
                    .ok_or("intake owner declaration absent")?,
            )?
        {
            return Err("writer schema intake differs from its exact Run and descriptor".into());
        }
        config.writer
    } else {
        let profile: RunAuthoringProfile = serde_json::from_value(
            private
                .get("run_authoring_profile")
                .cloned()
                .ok_or("writer schema owner profile absent")?,
        )
        .map_err(|error| error.to_string())?;
        if state.get("run_authoring_binding").is_some() {
            declared_provider_run_writer(&record, &request.writer.owner, &profile)?
        } else {
            let topology = topologies
                .get(request.writer.owner.as_str())
                .ok_or("writer schema owner has no declared topology")?;
            let result: octopus_interfaces::CheckpointBindingPolicySnapshot =
                serde_json::from_value(
                    topology
                        .pointer("/authoring_result/binding")
                        .cloned()
                        .ok_or("writer schema CodeMode return absent")?,
                )
                .map_err(|error| error.to_string())?;
            if topology.get("schema_version").and_then(Value::as_u64) != Some(1)
                || topology.get("run").and_then(Value::as_str) != Some(request.run_id.as_str())
                || topology.get("worker").and_then(Value::as_str) != Some(record.worker.as_str())
                || topology.get("file_write_tool").and_then(Value::as_str)
                    != Some(profile.file_write_tool.as_str())
                || result.outbound.len() != 1
                || result.outbound[0].interface != request.writer.owner
                || !result.inbound.admits(&profile.file_write_result_label)
                || !result.outbound[0].admits(&profile.file_write_result_label)
                || profile.result_timeout_ms == 0
            {
                return Err("writer schema CodeMode topology differs from its Run, writer or return boundary".into());
            }
            CheckedRunWriter {
                owner: request.writer.owner.clone(),
                source: request.writer.owner.clone(),
                door: result.inbound.interface,
                tool: profile.file_write_tool,
                call_label: profile.file_write_label,
                result_label: profile.file_write_result_label,
            }
        }
    };
    if declared != request.writer {
        return Err("writer schema request differs from the exact declared writer".into());
    }
    let tool_path = registry.join("tool").join(&declared.tool).join("tool.json");
    let before = checked_bytes(&tool_path, &request.expected_tool_sha256)?;
    let tool = parse_strict::<ToolDescriptor>(&before).map_err(|error| error.to_string())?;
    if tool.id.as_str() != declared.tool
        || tool.interface != declared.door
        || tool.args_schema != request.expected_args_schema
    {
        return Err(
            "writer schema descriptor differs from its exact ID, door or approved previous schema"
                .into(),
        );
    }
    ToolArgumentValidator::compile(&request.expected_args_schema)?;
    ToolArgumentValidator::compile(&request.args_schema)?;
    let mut updated: Value = serde_json::from_slice(&before).map_err(|error| error.to_string())?;
    updated["args_schema"] = request.args_schema.clone();
    let content = pretty_json(&updated)?;
    parse_strict::<ToolDescriptor>(content.as_bytes()).map_err(|error| error.to_string())?;
    if tool.args_schema == request.args_schema {
        return Ok(Vec::new());
    }
    Ok(vec![PlannedClientWrite {
        path: tool_path,
        content,
        tag: format!("tool:{}", declared.tool),
        expected_before_hash: Some(request.expected_tool_sha256.clone()),
    }])
}
