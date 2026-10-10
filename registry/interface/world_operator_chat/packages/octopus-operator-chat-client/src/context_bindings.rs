use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use octopus_descriptors::{ArtifactId, InterfaceId};
use octopus_interfaces::InterfaceDescriptor;
use octopus_runtime::run_record::RunRecord;

use crate::{ArtifactBindingSeed, BoundCheckpointRole, BoundRunAuthoringTopology};

pub(crate) fn bind_selected_context<'a>(
    record: &mut serde_json::Value,
    previous: Option<&serde_json::Value>,
    provider: &str,
    selected: impl IntoIterator<Item = &'a ArtifactBindingSeed>,
    excluded: impl IntoIterator<Item = &'a ArtifactId>,
) -> Result<(), String> {
    let excluded = excluded.into_iter().collect::<BTreeSet<_>>();
    let mut additions = Vec::new();
    let mut seen = BTreeSet::new();
    for binding in selected {
        validate_artifact_id(&binding.id)?;
        let id = ArtifactId::new(&binding.id);
        if !excluded.contains(&id) && seen.insert(id.clone()) {
            additions.push(id);
        }
    }
    let parsed = RunRecord::from_bytes(&serde_json::to_vec(record).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let provider = InterfaceId::new(provider);
    let state = parsed
        .interface_state
        .get(&provider)
        .ok_or("tool context has no provider state")?;
    let mut topology: BoundRunAuthoringTopology = serde_json::from_value(
        state
            .get("run_authoring_binding")
            .cloned()
            .ok_or("tool context has no authoring topology")?,
    )
    .map_err(|e| e.to_string())?;
    let previous_topology: Option<BoundRunAuthoringTopology> = previous
        .and_then(|record| record.get("interface_state"))
        .and_then(|state| state.get(provider.as_str()))
        .and_then(|state| state.get("run_authoring_binding"))
        .map(|value| serde_json::from_value(value.clone()).map_err(|e| e.to_string()))
        .transpose()?;
    let authoring = topology
        .checkpoints
        .iter()
        .find(|t| t.role == BoundCheckpointRole::AuthoringCall)
        .ok_or("tool context has no declared authoring call")?;
    let roles = parsed
        .checkpoints
        .iter()
        .find(|cp| {
            parsed.active_checkpoints.contains(&cp.id)
                && cp.interface_binding.as_ref() == Some(&authoring.interface_binding)
                && cp.outbound_interface_bindings == authoring.outbound_interface_bindings
        })
        .ok_or("tool context has no active authoring call")?;
    additions.retain(|id| {
        !roles
            .artifact_bindings
            .iter()
            .any(|binding| &binding.id == id)
    });
    let mut changed_rows = BTreeSet::new();
    let mut owner_updates = BTreeMap::new();
    let mut tool_doors = BTreeSet::new();
    for template in topology
        .checkpoints
        .iter_mut()
        .filter(|t| matches!(t.role, BoundCheckpointRole::ProviderToolOutput | BoundCheckpointRole::IndependentInput))
    {
        let [door] = template.outbound_interface_bindings.as_slice() else {
            return Err("selected context requires one exact outbound interface".into());
        };
        let matches = parsed
            .checkpoints
            .iter()
            .filter(|cp| {
                parsed.active_checkpoints.contains(&cp.id)
                    && cp.interface_binding.as_ref() == Some(&template.interface_binding)
                    && cp.outbound_interface_bindings == template.outbound_interface_bindings
                    && cp.required_verified_artifacts == template.required_verified_artifacts
            })
            .collect::<Vec<_>>();
        let [checkpoint] = matches.as_slice() else {
            return Err("selected context has no unique active checkpoint".into());
        };
        changed_rows.insert(checkpoint.id.clone());
        if template.role == BoundCheckpointRole::ProviderToolOutput {
            tool_doors.insert(door.interface.clone());
        }
        append_static_bindings(&mut template.static_artifact_bindings, &additions);
    }
    for template in topology.checkpoints.iter_mut().filter(|t| {
        t.role == BoundCheckpointRole::OwnerInput
            && !t.include_context
            && tool_doors.contains(&t.interface_binding.interface)
    }) {
        let owner = &template.interface_binding.interface;
        let old_static = previous_topology
            .as_ref()
            .and_then(|topology| {
                topology.checkpoints.iter().find(|t| {
                    t.role == BoundCheckpointRole::OwnerInput
                        && !t.include_context
                        && t.interface_binding.interface == *owner
                })
            })
            .map(|t| t.static_artifact_bindings.clone())
            .unwrap_or_else(|| template.static_artifact_bindings.clone());
        append_static_bindings(&mut template.static_artifact_bindings, &additions);
        let owned = parsed
            .interface_state
            .get(owner)
            .and_then(|state| state.get("owned_checkpoint_ids"))
            .and_then(serde_json::Value::as_array)
            .ok_or("tool context owner has no declared checkpoints")?;
        for id in owned {
            let id = id
                .as_str()
                .ok_or("tool context owner checkpoint id is malformed")?;
            let checkpoint = parsed
                .checkpoints
                .iter()
                .find(|cp| cp.id.as_str() == id && parsed.active_checkpoints.contains(&cp.id))
                .ok_or("tool context owner checkpoint is not active")?;
            if parsed.interface_state.iter().any(|(other, state)| {
                other != owner
                    && state
                        .get("owned_checkpoint_ids")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|ids| ids.iter().any(|other_id| other_id.as_str() == Some(id)))
            }) {
                return Err("tool context checkpoint is claimed by another owner".into());
            }
            if old_static.iter().all(|id| {
                checkpoint
                    .artifact_bindings
                    .iter()
                    .any(|binding| &binding.id == id)
            }) {
                owner_updates.insert(
                    checkpoint.id.clone(),
                    (
                        old_static.clone(),
                        template.static_artifact_bindings.clone(),
                    ),
                );
            }
        }
    }
    let rows = record
        .get_mut("checkpoints")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or("tool context checkpoints are malformed")?;
    for row in rows {
        let id = row
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or("tool context row has no id")?;
        let owner_update = owner_updates
            .iter()
            .find(|(changed, _)| changed.as_str() == id)
            .map(|(_, update)| update);
        if owner_update.is_none() && !changed_rows.iter().any(|changed| changed.as_str() == id) {
            continue;
        }
        let bindings = row
            .get_mut("artifact_bindings")
            .and_then(serde_json::Value::as_array_mut)
            .ok_or("tool context bindings are malformed")?;
        let selected = if let Some((old, new)) = owner_update {
            let mut ordered = Vec::new();
            for id in roles.artifact_bindings.iter().map(|b| &b.id).chain(new) {
                let binding = bindings
                    .iter()
                    .find(|binding| {
                        binding.get("id").and_then(serde_json::Value::as_str) == Some(id.as_str())
                    })
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({"id":id}));
                if !ordered.contains(&binding) {
                    ordered.push(binding);
                }
            }
            for binding in bindings.iter() {
                if !old.iter().any(|id| {
                    binding.get("id").and_then(serde_json::Value::as_str) == Some(id.as_str())
                }) && !ordered.contains(binding)
                {
                    ordered.push(binding.clone());
                }
            }
            *bindings = ordered;
            new
        } else {
            &additions
        };
        for id in selected {
            if let Some(binding) = bindings
                .iter()
                .find(|b| b.get("id").and_then(serde_json::Value::as_str) == Some(id.as_str()))
            {
                let binding: octopus_runtime::run_record::ArtifactBinding =
                    serde_json::from_value(binding.clone()).map_err(|e| e.to_string())?;
                if !binding.access.read {
                    return Err("tool context cannot widen an existing denied read".into());
                }
            } else {
                bindings.push(serde_json::json!({"id":id}));
            }
        }
    }
    record["interface_state"][provider.as_str()]["run_authoring_binding"] =
        serde_json::to_value(topology).map_err(|e| e.to_string())?;
    Ok(())
}

fn append_static_bindings(target: &mut Vec<ArtifactId>, additions: &[ArtifactId]) {
    for id in additions {
        if !target.contains(id) {
            target.push(id.clone());
        }
    }
}

pub fn declared_context_bindings(
    registry_root: &Path,
    selected_interfaces: impl IntoIterator<Item = InterfaceId>,
    explicit: &[ArtifactBindingSeed],
) -> Result<Vec<ArtifactBindingSeed>, String> {
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for binding in explicit {
        if seen.contains(&binding.id) {
            return Err(format!(
                "explicit context repeats artifact '{}'",
                binding.id
            ));
        }
        append_binding(&mut result, &mut seen, &binding.id)?;
    }
    let interfaces = selected_interfaces.into_iter().collect::<BTreeSet<_>>();
    for interface in interfaces {
        let id = crate::safe_segment(interface.as_str(), "context dependency interface")?;
        let path = registry_root
            .join("interface")
            .join(id)
            .join("interface.json");
        let descriptor: InterfaceDescriptor = serde_json::from_slice(
            &std::fs::read(&path)
                .map_err(|error| format!("context dependency '{}': {error}", path.display()))?,
        )
        .map_err(|error| format!("context dependency '{}': {error}", path.display()))?;
        if descriptor.id != interface {
            return Err(format!(
                "context dependency '{}' has a different interface identity",
                path.display()
            ));
        }
        let Some(value) = descriptor
            .interface_driver_config
            .as_ref()
            .and_then(|config| config.private_config.get("context_artifact_bindings"))
        else {
            continue;
        };
        let bindings: Vec<ArtifactBindingSeed> =
            serde_json::from_value(value.clone()).map_err(|error| {
                format!("interface '{interface}' context_artifact_bindings: {error}")
            })?;
        let mut declared = BTreeSet::new();
        for binding in bindings {
            if !declared.insert(binding.id.clone()) {
                return Err(format!(
                    "interface '{interface}' repeats context artifact '{}'",
                    binding.id
                ));
            }
            append_binding(&mut result, &mut seen, &binding.id)?;
        }
    }
    Ok(result)
}

pub fn with_instruction_source_bindings(
    registry_root: &Path,
    instruction_ids: impl IntoIterator<Item = String>,
    context: &[ArtifactBindingSeed],
) -> Result<Vec<ArtifactBindingSeed>, String> {
    let mut result = context.to_vec();
    let mut seen = result
        .iter()
        .map(|binding| binding.id.clone())
        .collect::<BTreeSet<_>>();
    let mut inspected = BTreeSet::new();
    for id in instruction_ids {
        if !inspected.insert(id.clone()) {
            continue;
        }
        validate_artifact_id(&id)?;
        if id
            .rsplit_once(':')
            .is_some_and(|(_, hash)| octopus_ledger::ContentHash::from_hex(hash).is_some())
        {
            continue;
        }
        let path = registry_root
            .join("artifact")
            .join(&id)
            .join("artifact.json");
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).map_err(|error| {
                format!("instruction dependency '{}': {error}", path.display())
            })?)
            .map_err(|error| format!("instruction dependency '{}': {error}", path.display()))?;
        if value.get("label").and_then(serde_json::Value::as_str) != Some("context.reference") {
            continue;
        }
        let Some(sources) = value.get("sources") else {
            continue;
        };
        let sources: Vec<octopus_ledger::ArtifactRef> = serde_json::from_value(sources.clone())
            .map_err(|error| format!("instruction artifact '{id}' sources: {error}"))?;
        let mut declared = BTreeSet::new();
        for source in sources {
            if source.id.as_str() == id || !declared.insert(source.id.clone()) {
                return Err(format!(
                    "instruction artifact '{id}' repeats or references itself"
                ));
            }
            append_binding(&mut result, &mut seen, source.id.as_str())?;
        }
    }
    Ok(result)
}

fn append_binding(
    result: &mut Vec<ArtifactBindingSeed>,
    seen: &mut BTreeSet<String>,
    id: &str,
) -> Result<(), String> {
    validate_artifact_id(id)?;
    if seen.insert(id.to_string()) {
        result.push(ArtifactBindingSeed { id: id.to_string() });
    }
    Ok(())
}

fn validate_artifact_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.trim() != id
        || matches!(id, "." | "..")
        || id.contains(['/', '\\', '\0'])
    {
        return Err(format!("invalid context artifact identity '{id}'"));
    }
    Ok(())
}

pub(crate) fn configure_tool_context(
    registry_root: &Path, identity: &crate::PlannedIdentity, content: String,
    previous: Option<&serde_json::Value>,
) -> Result<String, String> {
    let mut record: serde_json::Value = serde_json::from_str(&content).map_err(|error| error.to_string())?;
    let parsed = RunRecord::from_bytes(content.as_bytes()).map_err(|error| error.to_string())?;
    let provider = InterfaceId::new(&identity.model_interface_id);
    let mut topology: BoundRunAuthoringTopology = serde_json::from_value(record["interface_state"][provider.as_str()]["run_authoring_binding"].clone()).map_err(|error| error.to_string())?;
    let prior: Option<BoundRunAuthoringTopology> = previous.and_then(|record| record.get("interface_state"))
        .and_then(|states| states.get(provider.as_str())).and_then(|state| state.get("run_authoring_binding"))
        .map(|value| serde_json::from_value(value.clone()).map_err(|error| error.to_string())).transpose()?;
    let context = octopus_provider_run_authoring::provider_context_bindings(&parsed, &provider)?;
    for template in topology.checkpoints.iter_mut().filter(|template| template.role == BoundCheckpointRole::ProviderToolOutput) {
        let [door] = template.outbound_interface_bindings.as_slice() else { return Err("tool context requires one exact outbound interface".into()); };
        let source = identity.capability_doors.iter().find(|(_, target)| *target == door.interface.as_str())
            .map(|(source, _)| source).ok_or("tool context has no exact owning descriptor")?;
        let path = registry_root.join("interface").join(source).join("interface.json");
        let descriptor: InterfaceDescriptor = serde_json::from_slice(&std::fs::read(path).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
        if descriptor.id.as_str() != source { return Err("tool context descriptor identity differs from its source".into()); }
        let selected = descriptor.interface_driver_config.as_ref().and_then(|config| config.private_config.get("provider_tool_include_context"))
            .map(|value| serde_json::from_value::<bool>(value.clone()).map_err(|error| format!("provider_tool_include_context must be a boolean: {error}"))).transpose()?;
        let retained = prior.as_ref().and_then(|topology| topology.checkpoints.iter().find(|old| old.role == template.role
            && old.outbound_interface_bindings == template.outbound_interface_bindings && old.required_verified_artifacts == template.required_verified_artifacts));
        template.include_context = retained.map(|template| template.include_context).or(selected).unwrap_or(template.include_context);
        if !template.include_context { continue; }
        let matches = parsed.checkpoints.iter().enumerate().filter(|(_, checkpoint)| parsed.active_checkpoints.contains(&checkpoint.id)
            && checkpoint.interface_binding.as_ref() == Some(&template.interface_binding)
            && checkpoint.outbound_interface_bindings == template.outbound_interface_bindings
            && checkpoint.required_verified_artifacts == template.required_verified_artifacts).collect::<Vec<_>>();
        let [(index, checkpoint)] = matches.as_slice() else { return Err("tool context selection has no unique active checkpoint".into()); };
        let mut bindings = context.clone();
        if checkpoint.artifact_bindings.iter().any(|binding| !binding.access.read && context.iter().any(|bound| bound.id == binding.id)) {
            return Err("tool context selection cannot widen an existing denied read".into());
        }
        for binding in &checkpoint.artifact_bindings { if !bindings.iter().any(|bound| bound.id == binding.id) { bindings.push(binding.clone()); } }
        record["checkpoints"][*index]["artifact_bindings"] = serde_json::to_value(bindings).map_err(|error| error.to_string())?;
    }
    record["interface_state"][provider.as_str()]["run_authoring_binding"] = serde_json::to_value(topology).map_err(|error| error.to_string())?;
    crate::pretty_json(&record)
}
