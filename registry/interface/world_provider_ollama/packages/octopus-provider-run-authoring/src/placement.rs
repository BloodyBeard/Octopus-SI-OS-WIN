use super::*;

pub struct ProvenWrite {
    writer: CheckedRunWriter,
    call: Movement,
    result: Movement,
    write: serde_json::Value,
    before_hash: String,
    after_hash: String,
}

struct AdmittedCall {
    movement: Movement,
    label: String,
    call: octopus_tool_handlers::ToolCallArtifact,
}

fn admitted_under_grant(movement: &Movement, tool: &str, door: &InterfaceId, moved: &RunId) -> bool {
    movement.invocation_metadata.as_ref().and_then(|metadata| metadata.invocation()).is_some_and(|metadata| {
        metadata.verification_failure.is_none()
            && metadata.runtime.as_ref().is_some_and(|runtime| &runtime.interface == door && runtime.run != *moved)
            && metadata.grant.tools.iter().any(|granted| granted == tool)
            && !metadata.grant.hard_blocked_tools.iter().any(|blocked| blocked == tool)
            && metadata.grant.interfaces.iter().any(|granted| granted == door.as_str())
    })
}

fn returned_to(movement: &Movement, owner: &InterfaceId) -> bool {
    movement.invocation_metadata.as_ref().and_then(|metadata| metadata.invocation()).is_some_and(|metadata| {
        metadata.verification_failure.is_none()
            && metadata.runtime.as_ref().is_some_and(|runtime| &runtime.interface == owner)
    })
}

/// A run record's placement (`ee`) is changed by another run whose formed grant covers
/// the record, through an ordinary file-write tool, and the ledger proves it. A run never
/// changes its own place.
pub(super) fn proven_writes(
    reader: &mut (impl CheckedRunEvidenceReader + ?Sized),
    path: &Path,
    moved: &RunId,
    candidates: &[Movement],
) -> Result<Vec<ProvenWrite>, String> {
    let mut calls: BTreeMap<(InterfaceId, String), AdmittedCall> = BTreeMap::new();
    let mut writes = Vec::new();
    for movement in candidates {
        let (Endpoint::Interface(from), Endpoint::Interface(to)) = (&movement.from, &movement.to);
        let Some(reference) = movement.artifact.as_ref() else { continue; };
        let Ok((manifest, payload)) = reader.artifact(reference) else { continue; };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&payload) else { continue; };
        if value.get("arguments").is_some() {
            let Ok(call) = serde_json::from_value::<octopus_tool_handlers::ToolCallArtifact>(value) else { continue; };
            let Some(correlation) = call.correlation_id.clone() else { continue; };
            if call.arguments.get("ordered").is_some() || !admitted_under_grant(movement, &call.tool, to, moved) { continue; }
            let admitted = AdmittedCall { movement: movement.clone(), label: manifest.label, call };
            if calls.insert((to.clone(), correlation), admitted).is_some() {
                return Err("a proven placement write repeats its exact call correlation".into());
            }
            continue;
        }
        let Ok(result) = serde_json::from_value::<ToolResultArtifact>(value) else { continue; };
        let Some(correlation) = result.correlation_id.clone() else { continue; };
        let Some(admitted) = calls.remove(&(from.clone(), correlation)) else { continue; };
        if result.tool != admitted.call.tool || !result.ok || result.error.is_some()
            || admitted.movement.seq >= movement.seq || !returned_to(movement, to) {
            continue;
        }
        let Some(Ok(body)) = result.body.clone().map(serde_json::from_value::<FileWriteResultBody>) else { continue; };
        if body.result != "files_written" { continue; }
        let indexed = body.writes.iter().enumerate().filter(|(_, evidence)| evidence.path == path).collect::<Vec<_>>();
        let [(index, evidence)] = indexed.as_slice() else { continue; };
        let write = match admitted.call.arguments.get("writes") {
            Some(serde_json::Value::Array(entries)) if entries.len() == body.writes.len() => entries[*index].clone(),
            Some(_) => continue,
            None if body.writes.len() == 1 => admitted.call.arguments.clone(),
            None => continue,
        };
        let Some(before_hash) = evidence.before_hash.clone() else { continue; };
        let Endpoint::Interface(source) = &admitted.movement.from;
        writes.push(ProvenWrite {
            writer: CheckedRunWriter {
                owner: to.clone(),
                source: source.clone(),
                door: from.clone(),
                tool: admitted.call.tool.clone(),
                call_label: admitted.label,
                result_label: result.label.clone(),
            },
            call: admitted.movement,
            result: movement.clone(),
            write,
            before_hash,
            after_hash: evidence.after_hash.clone(),
        });
    }
    Ok(writes)
}

fn reconstructed_after(write: &serde_json::Value, before: &[u8]) -> Result<Vec<u8>, String> {
    match (write.get("content"), write.get("patch")) {
        (Some(serde_json::Value::String(content)), None) => Ok(content.as_bytes().to_vec()),
        (None, Some(patch)) => {
            let patch: json_patch::Patch = serde_json::from_value(patch.clone()).map_err(|error| error.to_string())?;
            let format: Option<JsonFormat> = write.get("json_format").cloned()
                .map(serde_json::from_value).transpose().map_err(|error| error.to_string())?;
            let mut value: serde_json::Value = serde_json::from_slice(before).map_err(|error| error.to_string())?;
            json_patch::patch(&mut value, &patch).map_err(|error| error.to_string())?;
            format.unwrap_or_default().encode(&value)
        }
        _ => Err("a proven placement write carries neither exact content nor one patch".into()),
    }
}

pub fn validate_placement_change(before: &[u8], after: &[u8]) -> Result<(), String> {
    let prior = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let next = RunRecord::from_bytes(after).map_err(|error| error.to_string())?;
    let mut prior_value: serde_json::Value = serde_json::from_slice(before).map_err(|error| error.to_string())?;
    let mut next_value: serde_json::Value = serde_json::from_slice(after).map_err(|error| error.to_string())?;
    for value in [&mut prior_value, &mut next_value] {
        value.as_object_mut().ok_or("run record is not an object")?.remove("ee");
    }
    if prior.id != next.id || prior.worker != next.worker || prior_value != next_value {
        return Err("a proven write by another run changed more than the run's placement".into());
    }
    Ok(())
}

pub(super) fn transition_from(
    writes: &[ProvenWrite],
    current: &[u8],
    previous_call: u64,
) -> Result<Option<CheckedRunTransition>, String> {
    let hash = ContentHash::of_bytes(current).to_hex();
    let matches = writes.iter().filter(|write| write.before_hash == hash && write.call.seq.0 > previous_call).collect::<Vec<_>>();
    let write = match matches.as_slice() {
        [] => return Ok(None),
        [write] => *write,
        _ => return Err("proven placement writes branch from one run snapshot".into()),
    };
    let after = reconstructed_after(&write.write, current)?;
    if ContentHash::of_bytes(&after).to_hex() != write.after_hash {
        return Err("a proven placement write does not reconstruct its admitted file effect".into());
    }
    validate_placement_change(current, &after)?;
    Ok(Some(CheckedRunTransition {
        writer: write.writer.clone(),
        before: current.to_vec(),
        after,
        call: write.call.clone(),
        result: write.result.clone(),
        placement: true,
    }))
}

fn placed_ee(reader: &mut (impl CheckedRunEvidenceReader + ?Sized), write: &ProvenWrite) -> Result<Option<String>, String> {
    if let Some(hash) = ContentHash::from_hex(&write.before_hash) {
        if let Ok(before) = reader.snapshot(&hash) {
            if ContentHash::of_bytes(&before) != hash { return Ok(None); }
            let Ok(after) = reconstructed_after(&write.write, &before) else { return Ok(None); };
            if ContentHash::of_bytes(&after).to_hex() != write.after_hash || validate_placement_change(&before, &after).is_err() {
                return Ok(None);
            }
            return RunRecord::from_bytes(&after).map(|record| Some(record.ee.as_str().to_string())).map_err(|error| error.to_string());
        }
    }
    let Some(serde_json::Value::Array(operations)) = write.write.get("patch") else { return Ok(None); };
    let mut placed = None;
    for operation in operations {
        if operation.get("op").and_then(serde_json::Value::as_str) != Some("replace")
            || operation.get("path").and_then(serde_json::Value::as_str) != Some("/ee")
        {
            return Ok(None);
        }
        placed = operation.get("value").and_then(serde_json::Value::as_str).map(str::to_string);
    }
    Ok(placed)
}

pub(super) fn prove_move(
    evidence: &CheckedRunEvidence<'_>,
    path: &Path,
    record: &RunRecord,
    proven_ee: &str,
    after_seq: u64,
    before_seq: u64,
) -> Result<(), String> {
    let mut reader = FileCheckedRunReader::open(evidence, Some(after_seq))?;
    let mut candidates = Vec::new();
    let mut sequence = reader.next_sequence();
    while sequence < before_seq {
        let Some(movement) = reader.movement(sequence)? else { break; };
        sequence = sequence.checked_add(1).ok_or("placement evidence sequence overflow")?;
        if movement.seq.0 > after_seq && movement.kind == MovementKind::InterfaceOutbound && movement.artifact.is_some() {
            candidates.push(movement);
        }
    }
    let writes = proven_writes(&mut reader, path, &record.id, &candidates)?;
    let mut placed = None;
    for write in writes.iter().rev() {
        if let Some(ee) = placed_ee(&mut reader, write)? {
            placed = Some(ee);
            break;
        }
    }
    if placed.as_deref() == Some(record.ee.as_str()) {
        return Ok(());
    }
    Err(format!(
        "run '{}' names EE '{}', but no write by another run whose grant covers its record proves that move; its proven place is '{}', and restoring it heals the run",
        record.id.as_str(),
        record.ee.as_str(),
        proven_ee
    ))
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PlacementReceipt {
    run: String,
    ee: String,
    through_seq: u64,
}

pub(super) fn read_receipt(root: &Path, run: &RunId) -> Result<Option<(u64, String)>, String> {
    let path = root.join(format!("{}.json", run.as_str()));
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("placement receipt '{}' cannot be read: {error}", path.display())),
    };
    let receipt: PlacementReceipt = serde_json::from_slice(&bytes)
        .map_err(|error| format!("placement receipt '{}' is malformed: {error}", path.display()))?;
    if receipt.run != run.as_str() {
        return Err(format!("placement receipt '{}' names run '{}'", path.display(), receipt.run));
    }
    Ok(Some((receipt.through_seq, receipt.ee)))
}

pub(super) fn write_receipt(root: &Path, run: &RunId, ee: &octopus_descriptors::EeId, through_seq: u64) -> Result<(), String> {
    use std::io::Write;
    std::fs::create_dir_all(root).map_err(|error| format!("placement receipts cannot be created: {error}"))?;
    let bytes = serde_json::to_vec(&PlacementReceipt { run: run.as_str().into(), ee: ee.as_str().into(), through_seq })
        .map_err(|error| error.to_string())?;
    let path = root.join(format!("{}.json", run.as_str()));
    let staged = root.join(format!(".{}.{}.tmp", run.as_str(), std::process::id()));
    let mut file = std::fs::File::create(&staged).map_err(|error| error.to_string())?;
    file.write_all(&bytes).and_then(|_| file.sync_all()).map_err(|error| error.to_string())?;
    std::fs::rename(&staged, &path).map_err(|error| error.to_string())?;
    std::fs::File::open(root).and_then(|directory| directory.sync_all()).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::validate_placement_change;

    #[test]
    fn a_placement_edit_changes_only_the_run_ee() {
        let before = br#"{"id":"run_a","ee":"ee_existence","worker":"system_worker_a","trace":"trace:run_a","status":"idle","interface_state":{},"checkpoints":[],"active_checkpoints":[]}"#;
        let moved = br#"{"id":"run_a","ee":"ee_memory","worker":"system_worker_a","trace":"trace:run_a","status":"idle","interface_state":{},"checkpoints":[],"active_checkpoints":[]}"#;
        let widened = br#"{"id":"run_a","ee":"ee_memory","worker":"system_worker_b","trace":"trace:run_a","status":"idle","interface_state":{},"checkpoints":[],"active_checkpoints":[]}"#;
        validate_placement_change(before, moved).unwrap();
        assert!(validate_placement_change(before, widened).is_err());
    }
}
