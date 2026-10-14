use super::*;
use sha2::{Digest, Sha256};

pub struct ToolContractUpdateRequest<'a> {
    pub movements_path: &'a Path,
    pub ledger_index_path: &'a Path,
    pub blobs_dir: &'a Path,
    pub interface_private_state_root: &'a Path,
    pub affected_tools: &'a BTreeMap<String, InterfaceId>,
    pub limits: &'a InterfaceHostIoLimits,
}

pub struct ToolRetirementRequest<'a> {
    pub movements_path: &'a Path,
    pub ledger_index_path: &'a Path,
    pub blobs_dir: &'a Path,
    pub interface_private_state_root: &'a Path,
    pub runs: &'a BTreeSet<octopus_descriptors::RunId>,
    pub tool_doors: &'a BTreeSet<InterfaceId>,
    pub limits: &'a InterfaceHostIoLimits,
}

#[derive(Debug)]
pub struct ToolContractUpdateProof {
    pub quiescent: bool,
    pub preimages: Vec<(PathBuf, Option<String>)>,
}

#[derive(Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    seq: u64,
    offset: u64,
}

fn snapshot(path: &Path, maximum: Option<u64>) -> Result<Option<Vec<u8>>, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("tool contract evidence '{}': {error}", path.display())),
    };
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || maximum.is_some_and(|limit| metadata.len() > limit) {
        return Err(format!("tool contract evidence '{}' is not a bounded regular file", path.display()));
    }
    let mut bytes = Vec::new();
    file.take(metadata.len().saturating_add(1)).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
    if bytes.len() as u64 != metadata.len() {
        return Err(format!("tool contract evidence '{}' changed while reading", path.display()));
    }
    Ok(Some(bytes))
}

fn digest(bytes: Option<&[u8]>) -> Option<String> {
    bytes.map(|bytes| format!("{:x}", Sha256::digest(bytes)))
}

pub fn tool_contract_update_quiescent(request: ToolContractUpdateRequest<'_>) -> Result<ToolContractUpdateProof, String> {
    inspect_quiescence(request, None)
}

pub fn tool_retirement_quiescent(request: ToolRetirementRequest<'_>) -> Result<ToolContractUpdateProof, String> {
    if request.runs.is_empty() { return Err("tool retirement requires exact selected runs".into()); }
    inspect_quiescence(ToolContractUpdateRequest {
        movements_path: request.movements_path,
        ledger_index_path: request.ledger_index_path,
        blobs_dir: request.blobs_dir,
        interface_private_state_root: request.interface_private_state_root,
        affected_tools: &BTreeMap::new(),
        limits: request.limits,
    }, Some((request.runs, request.tool_doors)))
}

fn inspect_quiescence(request: ToolContractUpdateRequest<'_>, retiring: Option<(&BTreeSet<octopus_descriptors::RunId>, &BTreeSet<InterfaceId>)>) -> Result<ToolContractUpdateProof, String> {
    request.limits.validate()?;
    for path in [request.movements_path, request.ledger_index_path, request.blobs_dir, request.interface_private_state_root] {
        if !path.is_absolute() { return Err("tool contract proof requires absolute evidence paths".into()); }
    }
    let mut proof = ToolContractUpdateProof { quiescent: true, preimages: Vec::new() };
    if retiring.is_none() && request.affected_tools.is_empty() { return Ok(proof); }
    let handles = InterfaceLedgerReadHandles {
        movements_path: request.movements_path, ledger_index_path: request.ledger_index_path,
        blobs_dir: request.blobs_dir, limits: *request.limits,
    };
    let bytes = snapshot(request.movements_path, None)?;
    proof.preimages.push((request.movements_path.to_path_buf(), digest(bytes.as_deref())));
    let ledger = bytes.as_deref().unwrap_or_default();
    let mut door_ids = request.affected_tools.values().cloned().collect::<BTreeSet<_>>();
    let mut boundaries = vec![0_u64];
    let mut sources = Vec::new();
    let mut reader = std::io::Cursor::new(ledger);
    let mut line = Vec::new();
    let mut sequence = 0_u64;
    let mut offset = 0_u64;
    loop {
        let read = read_bounded_tool_door_ledger_line(&mut reader, &mut line, request.limits.max_control_frame_bytes)?;
        if read == 0 { break; }
        let row = line.strip_suffix(b"\n").ok_or("tool contract ledger has an incomplete movement")?;
        let row = row.strip_suffix(b"\r").unwrap_or(row);
        let index = usize::try_from(sequence).map_err(|_| "tool contract sequence exceeds host range")?;
        let route = decode_durable_movement_route(row, index).map_err(|error| error.to_string())?;
        if route.seq.0 != sequence { return Err("tool contract ledger sequence is not contiguous".into()); }
        if route.kind == MovementKind::InterfaceOutbound
            && let Endpoint::Interface(door) = &route.to
            && (retiring.is_some() || door_ids.contains(door))
        {
            let movement = decode_tool_door_recovery_movement(request.movements_path, row, index, request.limits.max_control_frame_bytes)?;
            let metadata = movement.invocation_metadata.as_ref().and_then(|value| value.invocation())
                .ok_or("tool contract source lacks invocation metadata")?;
            let runtime = metadata.runtime().ok_or("tool contract source lacks runtime coordinates")?;
            if runtime.interface != *door || !matches!(movement.from, Endpoint::Interface(_)) || movement.artifact.is_none() {
                return Err("tool contract source has invalid door coordinates or artifact".into());
            }
            let selected = match retiring {
                Some((runs, _)) if runs.contains(&runtime.run) => {
                    door_ids.insert(door.clone());
                    true
                }
                Some(_) => false,
                None => metadata.grant().tools.iter().any(|tool| request.affected_tools.get(tool) == Some(door)),
            };
            if selected { sources.push(movement); }
        }
        sequence = sequence.checked_add(1).ok_or("tool contract sequence overflow")?;
        offset = offset.checked_add(read as u64).ok_or("tool contract offset overflow")?;
        boundaries.push(offset);
    }
    let mut doors = BTreeMap::new();
    for door in &door_ids {
        let mut components = Path::new(door.as_str()).components();
        if !matches!(components.next(), Some(std::path::Component::Normal(_))) || components.next().is_some() {
            return Err("tool contract proof door must be one exact path component".into());
        }
        let root = request.interface_private_state_root.join(door.as_str());
        let journal_path = root.join(TOOL_DOOR_EFFECT_JOURNAL_FILE);
        let journal = ToolDoorEffectJournal::read_only(journal_path.clone(), request.limits.max_control_frame_bytes)?;
        proof.preimages.push((journal_path, journal.source_sha256.clone()));
        let cursor_path = root.join("ledger-cursor.tool-door.json");
        let cursor_bytes = snapshot(&cursor_path, Some(request.limits.max_control_frame_bytes))?;
        let cursor: Cursor = cursor_bytes.as_deref().map(serde_json::from_slice).transpose()
            .map_err(|error| format!("tool contract cursor '{}': {error}", cursor_path.display()))?.unwrap_or_default();
        proof.preimages.push((cursor_path, digest(cursor_bytes.as_deref())));
        let known_door = retiring.is_none_or(|(_, declared)| declared.contains(door))
            || journal.source_sha256.is_some() || cursor_bytes.is_some();
        doors.insert(door.clone(), (journal, cursor, known_door));
    }
    for (_, cursor, _) in doors.values() {
        if usize::try_from(cursor.seq).ok().and_then(|seq| boundaries.get(seq)) != Some(&cursor.offset) {
            return Err("tool contract cursor is outside its exact ledger boundary".into());
        }
    }
    for source in sources {
        let Endpoint::Interface(door) = &source.to;
        let (journal, cursor, known_door) = &doors[door];
        if !known_door { continue; }
        let state = journal.state(source.seq.0, &tool_door_movement_digest(&source)?)?;
        if retiring.is_some() && matches!(state, Some(ToolDoorJournalState::Indeterminate | ToolDoorJournalState::InFlight)) {
            proof.quiescent = false;
            break;
        }
        if state.is_some_and(ToolDoorJournalState::is_terminal)
            && (retiring.is_none() || journal.entries.get(&source.seq.0).is_some_and(|entry| entry.schema_version == 3))
        { continue; }
        if state == Some(ToolDoorJournalState::Prepared)
            || !(cursor.seq > source.seq.0 || state == Some(ToolDoorJournalState::InFlight))
        {
            proof.quiescent = false;
            break;
        }
        let envelope = source_envelope(&handles, &source)?;
        let Some(call) = decode_tool_call(&envelope)? else { proof.quiescent = false; break; };
        let result = tool_door_admitted_result_state_at(&handles, door, &source, &call, &envelope, Some(ledger), |_, envelope| decode_tool_call(envelope))?;
        if result.is_none() || (retiring.is_some() && result != Some(ToolDoorJournalState::Completed)) {
            proof.quiescent = false;
            break;
        }
    }
    for (path, expected) in &proof.preimages {
        if digest(snapshot(path, None)?.as_deref()) != *expected {
            return Err(format!("tool contract evidence '{}' changed across inspection", path.display()));
        }
    }
    Ok(proof)
}

fn source_envelope(handles: &InterfaceLedgerReadHandles<'_>, source: &Movement) -> Result<InterfaceInvocationEnvelope, String> {
    let artifact = source.artifact.as_ref().ok_or("tool contract source has no artifact")?;
    let (file, _) = tool_door_recovery_artifact_at(handles, artifact)?;
    let captured = file.read_bounded(handles.limits.max_manifest_bytes, handles.limits.max_payload_bytes)
        .map_err(|error| error.to_string())?;
    Ok(InterfaceInvocationEnvelope {
        artifact: artifact.clone(),
        artifact_file: InterfaceArtifactFile::from_captured_snapshot(file.manifest_path().to_path_buf(), captured.manifest_bytes, captured.payload_bytes)
            .map_err(|error| error.to_string())?,
        artifact_binding_payloads: Vec::new(),
        metadata: source.invocation_metadata.as_ref().and_then(|value| value.invocation()).cloned(),
    })
}
