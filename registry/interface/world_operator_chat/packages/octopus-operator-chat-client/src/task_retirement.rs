use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::{BufRead, Read, Write};
use std::path::{Component, Path, PathBuf};

use super::{BoundCheckpointRole, BoundRunAuthoringTopology};
use octopus_descriptors::{EeId, InterfaceId, RunId};
use octopus_interface_transport::{InterfaceLifeOwnership, SpawnLifeConfig};
use octopus_process_session_tool_runtime::{ToolRetirementRequest, tool_retirement_quiescent};
use octopus_provider_run_authoring::{
    CheckedRunEvidence, RunAuthoringProfile, declared_provider_run_writer,
    provider_input_payloads_ready,
};
use octopus_runtime::run_record::RunRecord;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRetirementSelection {
    pub schema_version: u32,
    pub source_run: RunId,
    pub interface_ids: Vec<InterfaceId>,
    pub records: Vec<TaskRetirementRecord>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRetirementRecord {
    pub run: RunId,
    pub file: PathBuf,
    pub expected_digest: String,
    pub worker: InterfaceId,
    pub ee: EeId,
    pub role: String,
    pub provider_interface: InterfaceId,
    pub target_run: RunId,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRetirementRequest {
    pub registry_root: PathBuf,
    pub run_records_root: PathBuf,
    pub ledger_root: PathBuf,
    pub interface_private_state_root: PathBuf,
    pub selection: TaskRetirementSelection,
    pub original_records: BTreeMap<RunId, String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRetirementProof {
    pub schema_version: u32,
    pub selection_digest: String,
    pub preimages: Vec<TaskRetirementPreimage>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRetirementPreimage {
    pub path: PathBuf,
    pub sha256: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRetirementRoots {
    pub registry_root: PathBuf,
    pub run_records_root: PathBuf,
    pub ledger_root: PathBuf,
    pub interface_private_state_root: PathBuf,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetirementOriginal {
    pub file: PathBuf,
    pub expected_digest: String,
    pub content: String,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetirementFilePin {
    pub file: PathBuf,
    pub expected_digest: Option<String>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetirementLedgerBoundary {
    pub complete_offset: u64,
    pub sequence: u64,
    pub sha256: String,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeldRetirementProof {
    pub schema_version: u32,
    pub phase: String,
    pub operation_id: String,
    pub selection_digest: String,
    pub interface_ids: Vec<InterfaceId>,
    pub originals: Vec<RetirementFilePin>,
    pub retired: Vec<RetirementFilePin>,
    pub ledger_boundary: RetirementLedgerBoundary,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRetirementContext {
    pub operation_id: String,
    pub roots: TaskRetirementRoots,
    pub selection: TaskRetirementSelection,
    pub selection_digest: String,
    pub originals: Vec<RetirementOriginal>,
    pub retired: Vec<RetirementFilePin>,
    pub prior_proof: Option<HeldRetirementProof>,
}

fn selection_digest(selection: &TaskRetirementSelection) -> Result<String, String> {
    Ok(digest(&octopus_descriptors::canonical_bytes(
        &serde_json::to_value(selection).map_err(|error| error.to_string())?,
    )))
}

impl TaskRetirementContext {
    fn request(&self) -> Result<TaskRetirementRequest, String> {
        component(&self.operation_id)?;
        if self.operation_id.is_empty()
            || self.selection_digest != selection_digest(&self.selection)?
            || self.originals.len() != self.selection.records.len()
            || self.retired.len() != self.originals.len()
        {
            return Err(
                "retirement context has a different operation, selection or write set".into(),
            );
        }
        let mut originals = BTreeMap::new();
        for original in &self.originals {
            let selected = self
                .selection
                .records
                .iter()
                .find(|record| record.file == original.file)
                .ok_or("retirement before image is outside its selection")?;
            if original.expected_digest != selected.expected_digest
                || digest(original.content.as_bytes()) != original.expected_digest
                || originals
                    .insert(selected.run.clone(), original.content.clone())
                    .is_some()
            {
                return Err("retirement before image changed or repeats a selected record".into());
            }
        }
        let retired = self
            .retired
            .iter()
            .map(|pin| &pin.file)
            .collect::<BTreeSet<_>>();
        if retired.len() != self.retired.len()
            || self.retired.iter().any(|pin| {
                pin.expected_digest.is_some()
                    || !self
                        .originals
                        .iter()
                        .any(|original| original.file == pin.file)
            })
        {
            return Err("retirement targets must be the exact removed run records".into());
        }
        Ok(TaskRetirementRequest {
            registry_root: self.roots.registry_root.clone(),
            run_records_root: self.roots.run_records_root.clone(),
            ledger_root: self.roots.ledger_root.clone(),
            interface_private_state_root: self.roots.interface_private_state_root.clone(),
            selection: self.selection.clone(),
            original_records: originals,
        })
    }

    fn proof(&self, ledger_boundary: RetirementLedgerBoundary) -> HeldRetirementProof {
        let originals = self
            .originals
            .iter()
            .map(|original| RetirementFilePin {
                file: original.file.clone(),
                expected_digest: Some(original.expected_digest.clone()),
            })
            .collect::<Vec<_>>();
        let retired = self.retired.clone();
        let interface_ids = self.selection.interface_ids.clone();
        HeldRetirementProof {
            schema_version: 1,
            phase: "held".into(),
            operation_id: self.operation_id.clone(),
            selection_digest: self.selection_digest.clone(),
            interface_ids,
            originals,
            retired,
            ledger_boundary,
        }
    }

    pub fn verify_before(&self) -> Result<(), String> {
        if self.prior_proof.is_some() {
            return Err("retirement preflight cannot substitute for a retained held proof".into());
        }
        self.request()?.inspect(false)?;
        Ok(())
    }
}

fn ledger_boundary(
    request: &TaskRetirementRequest,
    prior: Option<&RetirementLedgerBoundary>,
) -> Result<RetirementLedgerBoundary, String> {
    let path = request.ledger_root.join("movements.jsonl");
    let bytes = regular(&path, None)?.unwrap_or_default();
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        return Err("retirement ledger has an incomplete tail".into());
    }
    let prior_offset = match prior {
        Some(prior) => {
            let offset = usize::try_from(prior.complete_offset)
                .map_err(|_| "retirement prefix exceeds host range")?;
            if offset > bytes.len()
                || digest(&bytes[..offset]) != prior.sha256
                || offset > 0 && bytes[offset - 1] != b'\n'
            {
                return Err("retirement ledger prefix changed after the held proof".into());
            }
            offset
        }
        None => 0,
    };
    let interfaces = request
        .selection
        .interface_ids
        .iter()
        .collect::<BTreeSet<_>>();
    let runs = request
        .selection
        .records
        .iter()
        .map(|record| &record.run)
        .collect::<BTreeSet<_>>();
    let mut sequence = 0;
    let mut offset = 0;
    let decoder = octopus_ledger_store::DurableMovementDecoder::for_log(&path);
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let movement = decoder
            .decode(line, sequence as usize)
            .map_err(|error| error.to_string())?;
        if movement.seq.0 != sequence {
            return Err("retirement ledger is not contiguous".into());
        }
        if offset == prior_offset && prior.is_some_and(|prior| prior.sequence != sequence) {
            return Err("retirement ledger prefix has a different sequence boundary".into());
        }
        if prior.is_some() && offset >= prior_offset {
            let octopus_ledger::Endpoint::Interface(from) = &movement.from;
            let octopus_ledger::Endpoint::Interface(to) = &movement.to;
            if interfaces.contains(from)
                || interfaces.contains(to)
                || movement
                    .runtime()
                    .is_some_and(|runtime| runs.contains(&runtime.run))
            {
                return Err(
                    "retired task acquired new movement evidence after its held proof".into(),
                );
            }
        }
        offset += line.len();
        sequence += 1;
    }
    if offset == prior_offset && prior.is_some_and(|prior| prior.sequence != sequence) {
        return Err("retirement ledger ends at a different sequence boundary".into());
    }
    Ok(RetirementLedgerBoundary {
        complete_offset: bytes.len() as u64,
        sequence,
        sha256: digest(&bytes),
    })
}

fn verify_held(
    context: &TaskRetirementContext,
    request: &TaskRetirementRequest,
    previous: Option<&HeldRetirementProof>,
) -> Result<HeldRetirementProof, String> {
    request.validate(true)?;
    request.assert_unbound()?;
    let boundary = if let Some(previous) = previous {
        if context.proof(previous.ledger_boundary.clone()) != *previous {
            return Err("retirement held proof belongs to another exact transaction".into());
        }
        ledger_boundary(request, Some(&previous.ledger_boundary))?
    } else {
        let proof = request.inspect(true)?;
        let boundary = ledger_boundary(request, None)?;
        if proof
            .preimages
            .iter()
            .find(|pin| pin.path == request.ledger_root.join("movements.jsonl"))
            .and_then(|pin| pin.sha256.as_ref())
            .is_some_and(|hash| hash != &boundary.sha256)
        {
            return Err("retirement ledger advanced while capturing its held proof".into());
        }
        proof.assert_unchanged()?;
        boundary
    };
    request.validate(true)?;
    request.assert_unbound()?;
    Ok(context.proof(boundary))
}

pub fn serve_task_retirement() -> Result<(), String> {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut line = String::new();
    input
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    let context: TaskRetirementContext =
        serde_json::from_str(&line).map_err(|error| error.to_string())?;
    let request = context.request()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let _holds = loop {
        if let Some(holds) = request.try_hold()? {
            break holds;
        }
        if std::time::Instant::now() >= deadline {
            return Err("retirement is waiting for its interface processes to stop".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    let mut proof = verify_held(&context, &request, context.prior_proof.as_ref())?;
    let mut output = std::io::stdout().lock();
    let send =
        |output: &mut std::io::StdoutLock<'_>, proof: &HeldRetirementProof| -> Result<(), String> {
            serde_json::to_writer(&mut *output, proof).map_err(|error| error.to_string())?;
            output
                .write_all(b"\n")
                .and_then(|_| output.flush())
                .map_err(|error| error.to_string())
        };
    send(&mut output, &proof)?;
    loop {
        line.clear();
        if input
            .read_line(&mut line)
            .map_err(|error| error.to_string())?
            == 0
        {
            return Ok(());
        }
        let command: Value = serde_json::from_str(&line).map_err(|error| error.to_string())?;
        if command == serde_json::json!({"operation": "release"}) {
            return Ok(());
        }
        if command != serde_json::json!({"operation": "assert_held"}) {
            return Err("unknown retirement hold operation".into());
        }
        proof = verify_held(&context, &request, Some(&proof))?;
        send(&mut output, &proof)?;
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn regular(path: &Path, maximum: Option<u64>) -> Result<Option<Vec<u8>>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || maximum.is_some_and(|maximum| metadata.len() > maximum)
    {
        return Err(format!(
            "retirement evidence '{}' is not a bounded regular file",
            path.display()
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|error| error.to_string())?;
    let opened = file.metadata().map_err(|error| error.to_string())?;
    if !opened.is_file() || opened.len() != metadata.len() {
        return Err("retirement evidence changed during open".into());
    }
    let mut bytes = Vec::new();
    file.take(metadata.len().saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 != metadata.len() {
        return Err("retirement evidence changed during reading".into());
    }
    Ok(Some(bytes))
}

fn document(path: &Path, pins: &mut BTreeMap<PathBuf, Option<String>>) -> Result<Value, String> {
    let bytes = regular(path, Some(268_435_456))?
        .ok_or_else(|| format!("retirement evidence '{}' is absent", path.display()))?;
    pins.insert(path.to_path_buf(), Some(digest(&bytes)));
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

fn component(value: &str) -> Result<(), String> {
    let mut parts = Path::new(value).components();
    if !matches!(parts.next(), Some(Component::Normal(_)))
        || parts.next().is_some()
        || value.contains('\\')
    {
        return Err("retirement identity must be one exact path component".into());
    }
    Ok(())
}

impl TaskRetirementRequest {
    pub fn validate(&self, retired: bool) -> Result<BTreeMap<RunId, RunRecord>, String> {
        for root in [
            &self.registry_root,
            &self.run_records_root,
            &self.ledger_root,
            &self.interface_private_state_root,
        ] {
            if !root.is_absolute()
                || root
                    .components()
                    .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
            {
                return Err("retirement roots must be absolute normalized paths".into());
            }
            let metadata = std::fs::symlink_metadata(root).map_err(|error| error.to_string())?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err("retirement root is not a regular directory".into());
            }
        }
        let selection = &self.selection;
        let interfaces = selection.interface_ids.iter().collect::<BTreeSet<_>>();
        if selection.schema_version != 1
            || interfaces.is_empty()
            || interfaces.len() != selection.interface_ids.len()
            || selection.records.len() != self.original_records.len()
            || selection.records.is_empty()
        {
            return Err("retirement selection has invalid version or duplicate ownership".into());
        }
        for id in &interfaces {
            component(id.as_str())?;
        }
        let mut records = BTreeMap::new();
        for selected in &selection.records {
            component(selected.run.as_str())?;
            let original = self
                .original_records
                .get(&selected.run)
                .ok_or("retirement original record is absent")?;
            if selected.file != self.run_records_root.join(format!("{}.json", selected.run))
                || digest(original.as_bytes()) != selected.expected_digest
                || !interfaces.contains(&selected.worker)
                || !interfaces.contains(&selected.provider_interface)
                || !matches!(
                    selected.role.as_str(),
                    "source" | "observer" | "projection" | "writer"
                )
            {
                return Err("retirement record changed its exact path, preimage or owner".into());
            }
            let record =
                RunRecord::from_bytes(original.as_bytes()).map_err(|error| error.to_string())?;
            if record.id != selected.run
                || record.worker != selected.worker
                || record.ee != selected.ee
                || records.insert(record.id.clone(), record).is_some()
            {
                return Err("retirement record changed its identity or repeats a run".into());
            }
            let current = regular(&selected.file, Some(268_435_456))?;
            if retired && current.is_some()
                || !retired && current.as_deref() != Some(original.as_bytes())
            {
                return Err("retirement record differs from its expected publication state".into());
            }
        }
        let sources = selection
            .records
            .iter()
            .filter(|row| row.role == "source")
            .collect::<Vec<_>>();
        if sources.len() != 1 || sources[0].run != selection.source_run {
            return Err("retirement selection requires one exact source".into());
        }
        for selected in &selection.records {
            if selected.role == "writer" {
                let target = records
                    .get(&selected.target_run)
                    .ok_or("retirement writer target is absent")?;
                let state = target
                    .interface_state
                    .get(&selected.provider_interface)
                    .ok_or("retirement writer owner is absent")?;
                if state
                    .get("run_authoring_binding").and_then(|binding| binding.pointer("/writer_run/run"))
                    .or_else(|| state.get("writer_run").and_then(|binding| binding.get("run")))
                    .and_then(Value::as_str)
                    != Some(selected.run.as_str())
                {
                    return Err(
                        "retirement writer differs from its exact target declaration".into(),
                    );
                }
            } else if selected.target_run != selected.run {
                return Err("retirement target changed identity".into());
            }
        }
        Ok(records)
    }

    pub fn assert_unbound(&self) -> Result<(), String> {
        let selected = self
            .selection
            .records
            .iter()
            .map(|record| &record.run)
            .collect::<BTreeSet<_>>();
        let interfaces = self.selection.interface_ids.iter().collect::<BTreeSet<_>>();
        for entry in std::fs::read_dir(&self.run_records_root).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            if entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("json")
            {
                continue;
            }
            let bytes = regular(&entry.path(), Some(268_435_456))?
                .ok_or("run disappeared during retirement ownership scan")?;
            let record = RunRecord::from_bytes(&bytes).map_err(|error| error.to_string())?;
            if selected.contains(&record.id) {
                return Err("retired run is still published".into());
            }
            if interfaces.contains(&record.worker)
                || record
                    .interface_state
                    .keys()
                    .any(|id| interfaces.contains(id))
                || record.checkpoints.iter().any(|checkpoint| {
                    checkpoint
                        .interface_binding
                        .as_ref()
                        .is_some_and(|binding| interfaces.contains(&binding.interface))
                        || checkpoint
                            .outbound_interface_bindings
                            .iter()
                            .any(|binding| interfaces.contains(&binding.interface))
                })
            {
                return Err(format!(
                    "retired interface is still bound by run '{}'",
                    record.id
                ));
            }
        }
        Ok(())
    }

    pub fn inspect(&self, retired: bool) -> Result<TaskRetirementProof, String> {
        let records = self.validate(retired)?;
        if retired {
            self.assert_unbound()?;
        }
        let movements = self.ledger_root.join("movements.jsonl");
        let index = self.ledger_root.join("ledger-index.sqlite");
        let blobs = self.ledger_root.join("blobs");
        let mut pins = BTreeMap::new();
        pins.insert(
            movements.clone(),
            regular(&movements, None)?.as_deref().map(digest),
        );
        let mut limits = None;
        for selected in self
            .selection
            .records
            .iter()
            .filter(|record| record.role != "writer")
        {
            let record = &records[&selected.run];
            let provider = &selected.provider_interface;
            let root = self.registry_root.join("interface").join(provider.as_str());
            let descriptor = document(&root.join("interface.json"), &mut pins)?;
            if descriptor.get("id").and_then(Value::as_str) != Some(provider.as_str()) {
                return Err("retirement provider changed identity".into());
            }
            let profile: RunAuthoringProfile = serde_json::from_value(
                descriptor
                    .pointer("/interface_driver_config/private_config/run_authoring_profile")
                    .cloned()
                    .ok_or("retirement provider has no declared writer profile")?,
            )
            .map_err(|error| error.to_string())?;
            let config: SpawnLifeConfig =
                serde_json::from_value(document(&root.join("life.json"), &mut pins)?)
                    .map_err(|error| error.to_string())?;
            let [process] = config.processes.as_slice() else {
                return Err("retirement provider requires one declared life process".into());
            };
            process.io_limits.validate()?;
            limits = Some(process.io_limits);
            let evidence = CheckedRunEvidence {
                movements_path: &movements,
                ledger_index_path: &index,
                blobs_dir: &blobs,
                max_control_frame_bytes: process.io_limits.max_control_frame_bytes,
                max_manifest_bytes: process.io_limits.max_manifest_bytes,
                max_payload_bytes: process.io_limits.max_payload_bytes,
            };
            let writer = declared_provider_run_writer(record, provider, &profile)?;
            let topology: BoundRunAuthoringTopology = serde_json::from_value(
                record
                    .interface_state
                    .get(provider)
                    .and_then(|state| state.get("run_authoring_binding"))
                    .cloned()
                    .ok_or("retirement provider has no declared topology")?,
            )
            .map_err(|error| error.to_string())?;
            let sources = topology
                .checkpoints
                .iter()
                .filter(|checkpoint| {
                    checkpoint.role == BoundCheckpointRole::Stable && checkpoint.include_context
                })
                .map(|checkpoint| &checkpoint.interface_binding.interface)
                .collect::<BTreeSet<_>>();
            if sources.is_empty() {
                return Err("retirement provider has no declared context input".into());
            }
            for source in sources {
                if !provider_input_payloads_ready(
                    &evidence,
                    &selected.file,
                    self.original_records[&selected.run].as_bytes(),
                    &writer,
                    source,
                )? {
                    return Err(format!(
                        "run '{}' still has unhandled input or an unsettled handoff",
                        selected.run
                    ));
                }
            }
        }
        let limits = limits.ok_or("retirement selection has no provider")?;
        let limits = octopus_process_session_tool_runtime::InterfaceHostIoLimits {
            max_control_frame_bytes: limits.max_control_frame_bytes,
            max_stderr_bytes: limits.max_stderr_bytes,
            max_manifest_bytes: limits.max_manifest_bytes,
            max_payload_bytes: limits.max_payload_bytes,
        };
        let mut drivers = BTreeSet::new();
        for (id, value) in registry_documents(&self.registry_root.join("worker_environment_kind"), "worker_environment_kind.json", &mut pins)? {
            let spec: octopus_worker_env::WorkerEnvKindSpec = serde_json::from_value(value).map_err(|error| error.to_string())?;
            octopus_descriptors::Descriptor::validate(&spec)?;
            if spec.worker_environment_kind_id.as_str() != id {
                return Err("retirement kind descriptor changed identity".into());
            }
            drivers.extend(spec.door_driver_refs);
        }
        let mut tool_doors = BTreeSet::new();
        for (id, value) in registry_documents(&self.registry_root.join("interface"), "interface.json", &mut pins)? {
            let descriptor: octopus_interfaces::InterfaceDescriptor = serde_json::from_value(value).map_err(|error| error.to_string())?;
            octopus_descriptors::Descriptor::validate(&descriptor)?;
            if descriptor.id.as_str() != id {
                return Err("retirement interface descriptor changed identity".into());
            }
            if drivers.contains(&descriptor.interface_driver_ref) { tool_doors.insert(descriptor.id); }
        }
        let proof = tool_retirement_quiescent(ToolRetirementRequest {
            movements_path: &movements,
            ledger_index_path: &index,
            blobs_dir: &blobs,
            interface_private_state_root: &self.interface_private_state_root,
            runs: &records.keys().cloned().collect(),
            tool_doors: &tool_doors,
            limits: &limits,
        })?;
        if !proof.quiescent {
            return Err("retirement has an accepted unresolved tool effect".into());
        }
        for (path, hash) in proof.preimages {
            if pins.get(&path).is_some_and(|before| before != &hash) {
                return Err("retirement evidence changed across inspection".into());
            }
            pins.insert(path, hash);
        }
        let proof = TaskRetirementProof {
            schema_version: 1,
            selection_digest: selection_digest(&self.selection)?,
            preimages: pins
                .into_iter()
                .map(|(path, sha256)| TaskRetirementPreimage { path, sha256 })
                .collect(),
        };
        proof.assert_unchanged()?;
        self.validate(retired)?;
        Ok(proof)
    }

    pub fn try_hold(&self) -> Result<Option<Vec<InterfaceLifeOwnership>>, String> {
        self.validate(true)?;
        let mut ids = self.selection.interface_ids.clone();
        ids.sort();
        let mut guards = Vec::new();
        for id in ids {
            let Some(guard) =
                InterfaceLifeOwnership::try_acquire(&self.interface_private_state_root, &id)?
            else {
                return Ok(None);
            };
            guards.push(guard);
        }
        self.assert_unbound()?;
        Ok(Some(guards))
    }
}

fn registry_documents(root: &Path, filename: &str, pins: &mut BTreeMap<PathBuf, Option<String>>) -> Result<Vec<(String, Value)>, String> {
    let mut documents = Vec::new();
    for entry in std::fs::read_dir(root).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let kind = entry.file_type().map_err(|error| error.to_string())?;
        if kind.is_symlink() { return Err("retirement registry cannot follow an aliased owner".into()); }
        if !kind.is_dir() { continue; }
        let id = entry.file_name().into_string().map_err(|_| "retirement registry owner is not UTF-8")?;
        component(&id)?;
        documents.push((id, document(&entry.path().join(filename), pins)?));
    }
    Ok(documents)
}

impl TaskRetirementProof {
    pub fn assert_unchanged(&self) -> Result<(), String> {
        for pin in &self.preimages {
            if regular(&pin.path, None)?.as_deref().map(digest) != pin.sha256 {
                return Err(format!(
                    "retirement evidence '{}' changed",
                    pin.path.display()
                ));
            }
        }
        Ok(())
    }
}
