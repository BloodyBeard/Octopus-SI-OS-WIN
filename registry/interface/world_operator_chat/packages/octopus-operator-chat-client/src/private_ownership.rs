use super::*;

#[derive(Default)]
pub(crate) struct ExistingPrivateCapabilityOwnership {
    pub(crate) interfaces: BTreeSet<String>,
    pub(crate) tools: BTreeSet<String>,
}

pub(crate) fn existing_private_capability_ownership(
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

pub(crate) fn attest_private_interface_owner(
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

pub(crate) fn attest_private_tool_owner(
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

pub(crate) fn planned_retained_registry_paths(
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

pub(crate) fn plan_private_capability_removals(
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

pub(crate) fn snapshot_planned_removal(path: &Path, tag: String) -> Result<PlannedClientRemoval, String> {
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

pub(crate) fn preserve_exact_worker_role_artifacts(
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

pub(crate) fn append_ordered_unique_ids<'a>(
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
