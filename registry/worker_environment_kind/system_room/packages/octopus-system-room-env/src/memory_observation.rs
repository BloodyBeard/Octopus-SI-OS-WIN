use octopus_interfaces::InterfaceInvocationEnvelope;
use octopus_ledger::ArtifactRef;
use serde_json::Value;

use crate::memory_bound::MemoryHost;

const EXPERIENCE: &str = "memory.experience.input";

pub(crate) fn scope(
    input: &InterfaceInvocationEnvelope,
    host: &MemoryHost<'_>,
    run: &str,
) -> Result<String, String> {
    if run.is_empty() || run.trim() != run {
        return Err("memory door names an invalid observed run".into());
    }
    let metadata = input.metadata().map_err(|error| error.to_string())?;
    let prefix = format!("{EXPERIENCE}:");
    for reference in metadata.artifact_binding_refs.iter().rev() {
        if !reference.id.as_str().starts_with(&prefix) {
            continue;
        }
        let bound = crate::collection_tools::readable_source(input, reference.id.as_str())?;
        let bytes = bound
            .read_bounded(host.maximum_bytes)
            .map_err(|error| error.to_string())?;
        let value = serde_json::from_slice::<Value>(&bytes)
            .map_err(|error| format!("bound memory experience is malformed: {error}"))?;
        if reference.id.as_str() != format!("{EXPERIENCE}:{}", reference.content.to_hex()) {
            return Err("memory experience is not its exact admitted carrier".into());
        }
        return experience_scope(&value, host.collection, run);
    }
    Err("memory observation has no bound experience proving its EE; refusing before any memory effect".into())
}

fn experience_scope(value: &Value, collection: &str, run: &str) -> Result<String, String> {
    if value["schema_version"] != 1
        || value["label"] != EXPERIENCE
        || value["source"]["run"] != run
        || value["collection_source"] != collection
        || value["source"]["terminal_source_movement_seq"]
            .as_u64()
            .is_none()
        || value["source"]["projection_source_movement_seq"]
            .as_u64()
            .is_none()
        || serde_json::from_value::<ArtifactRef>(value["source"]["terminal_artifact"].clone())
            .is_err()
    {
        return Err("bound memory experience differs from the door's observed run or collection, or lacks source evidence".into());
    }
    value["source"]["ee"]
        .as_str()
        .filter(|ee| !ee.is_empty() && ee.trim() == *ee)
        .map(str::to_string)
        .ok_or_else(|| "bound memory experience has no observed EE".into())
}
