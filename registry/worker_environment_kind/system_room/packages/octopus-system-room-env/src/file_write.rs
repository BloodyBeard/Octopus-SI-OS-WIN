use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fs2::FileExt;
use octopus_file_write_contract::{DecodeBudget, JsonFormat, Preimage};
use octopus_interfaces::InterfaceInvocationEnvelope;
use octopus_ledger::ContentHash;
use octopus_worker_env::AbsPath;

#[path = "file_write/ordered.rs"]
mod ordered;

pub fn execute_file_write_result(
    input: &InterfaceInvocationEnvelope,
    arguments: &serde_json::Value,
    room_kinds: &[String],
    root_slots: &BTreeMap<String, PathBuf>,
    effect_root: &Path,
    max_payload_bytes: u64,
) -> Result<serde_json::Value, octopus_tool_handlers::ToolResultError> {
    if arguments.get("ordered").is_some() {
        ordered::execute(input, arguments, room_kinds, root_slots, effect_root, max_payload_bytes)
    } else {
        execute_file_write(input, arguments, room_kinds, root_slots, effect_root, max_payload_bytes)
            .map_err(|error| octopus_tool_handlers::ToolResultError::new("tool_execution_failed", error))
    }
}

pub fn execute_file_write(
    input: &InterfaceInvocationEnvelope,
    arguments: &serde_json::Value,
    room_kinds: &[String],
    root_slots: &BTreeMap<String, PathBuf>,
    effect_root: &Path,
    max_payload_bytes: u64,
) -> Result<serde_json::Value, String> {
    ensure_room_kind_granted(input, room_kinds)?;
    let roots = configured_write_roots(&root_slots.values().cloned().collect::<Vec<_>>())?;
    let mut requests = parse_write_requests(arguments, &roots, max_payload_bytes)?;
    bind_effect_record(input, effect_root, &mut requests)?;
    retain_write_inputs(effect_root, &requests)?;
    let staging_root = effect_root.join("staging");
    std::fs::create_dir_all(&staging_root).map_err(|error| {
        format!(
            "file.write staging root '{}' cannot be created: {error}",
            staging_root.display()
        )
    })?;
    let mut staged: Vec<PreparedWrite> = Vec::with_capacity(requests.len());
    for request in requests {
        let parent = request.target.parent().ok_or_else(|| {
            format!(
                "file.write path '{}' has no parent directory",
                request.target.display()
            )
        })?;
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("file.write could not create '{}': {e}", parent.display()))?;
        let staged_path = if request.already_applied {
            None
        } else {
            match stage_write(&staging_root, &request.target, request.content.as_bytes()) {
                Ok(path) => Some(path),
                Err(error) => {
                    for prepared in &staged {
                        if let Some(path) = prepared.staged_path.as_ref() {
                            let _ = std::fs::remove_file(path);
                        }
                    }
                    return Err(error);
                }
            }
        };
        staged.push(PreparedWrite {
            target: request.target,
            staged_path,
            content: request.content,
            before_hash: request.before_hash,
        });
    }

    let mut evidence = Vec::with_capacity(staged.len());
    let mut committed = Vec::new();
    for (index, prepared) in staged.iter().enumerate() {
        let mut lock = TargetWriteLock::acquire(&prepared.target)?;
        let after_hash = ContentHash::of_bytes(prepared.content.as_bytes()).to_hex();
        let current_hash = match lock.as_mut() {
            Some(lock) => Some(lock.current_hash()?),
            None => None,
        };
        let expected_hash = if prepared.staged_path.is_some() {
            prepared.before_hash.as_deref()
        } else {
            Some(after_hash.as_str())
        };
        if current_hash.as_deref() != expected_hash {
            return Err(format!(
                "file.write optimistic hash changed before commit for '{}': expected {}, observed {}",
                prepared.target.display(),
                expected_hash.unwrap_or("<absent>"),
                current_hash.as_deref().unwrap_or("<absent>")
            ));
        }
        if let Some(staged_path) = prepared.staged_path.as_ref() {
            let installed = if prepared.before_hash.is_none() {
                std::fs::hard_link(staged_path, &prepared.target)
                    .and_then(|()| std::fs::remove_file(staged_path))
            } else {
                if lock.is_none() {
                    return Err(format!(
                        "file.write target '{}' disappeared after optimistic validation",
                        prepared.target.display()
                    ));
                }
                std::fs::rename(staged_path, &prepared.target)
            };
            if let Err(error) = installed {
                if prepared.before_hash.is_none()
                    && error.kind() == std::io::ErrorKind::AlreadyExists
                {
                    let observed = optional_hash(&prepared.target)?;
                    if observed.as_deref() == Some(after_hash.as_str()) {
                        let _ = std::fs::remove_file(staged_path);
                        committed.push(prepared.target.display().to_string());
                        if let Some(lock) = lock.take() {
                            lock.release()?;
                        }
                        evidence.push(serde_json::json!({
                            "path": prepared.target.display().to_string(),
                            "before_hash": prepared.before_hash,
                            "after_hash": after_hash,
                            "bytes": prepared.content.len(),
                        }));
                        continue;
                    }
                }
                for pending in staged.iter().skip(index) {
                    if let Some(path) = pending.staged_path.as_ref() {
                        let _ = std::fs::remove_file(path);
                    }
                }
                return Err(format!(
                    "file.write could not move the staged bytes into '{}': {error}; already replaced [{}]",
                    prepared.target.display(),
                    committed.join(", ")
                ));
            }
            committed.push(prepared.target.display().to_string());
            if let Err(error) = sync_parent(&prepared.target) {
                for pending in staged.iter().skip(index + 1) {
                    if let Some(path) = pending.staged_path.as_ref() {
                        let _ = std::fs::remove_file(path);
                    }
                }
                return Err(format!(
                    "{error}; already replaced [{}]",
                    committed.join(", ")
                ));
            }
        }
        let installed_hash = optional_hash(&prepared.target)?;
        if installed_hash.as_deref() != Some(after_hash.as_str()) {
            return Err(format!(
                "file.write installed unexpected bytes at '{}': expected {}, observed {}",
                prepared.target.display(),
                after_hash,
                installed_hash.as_deref().unwrap_or("<absent>")
            ));
        }
        if let Some(lock) = lock {
            lock.release()?;
        }
        evidence.push(serde_json::json!({
            "path": prepared.target.display().to_string(),
            "before_hash": prepared.before_hash,
            "after_hash": after_hash,
            "bytes": prepared.content.len(),
        }));
    }

    Ok(serde_json::json!({
        "result": "files_written",
        "writes": evidence,
    }))
}

struct WriteRequest {
    target: PathBuf,
    content: String,
    before_hash: Option<String>,
    observed_hash: Option<String>,
    observed_bytes: Option<Vec<u8>>,
    already_applied: bool,
}

struct PreparedWrite {
    target: PathBuf,
    staged_path: Option<PathBuf>,
    content: String,
    before_hash: Option<String>,
}

struct TargetWriteLock {
    file: std::fs::File,
}

impl TargetWriteLock {
    fn acquire(target: &Path) -> Result<Option<Self>, String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let file = match OpenOptions::new().read(true).write(true).open(target) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => {
                    return Err(format!(
                        "file.write target '{}' cannot be opened for locking: {error}",
                        target.display()
                    ));
                }
            };
            match file.try_lock_exclusive() {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        return Err(format!(
                            "file.write timed out waiting for target lock '{}'",
                            target.display()
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                    continue;
                }
                Err(error) => {
                    return Err(format!(
                        "file.write target '{}' cannot be locked: {error}",
                        target.display()
                    ));
                }
            }
            let locked = file.metadata().map_err(|error| {
                format!(
                    "file.write locked target '{}' cannot be inspected: {error}",
                    target.display()
                )
            })?;
            let current = match std::fs::metadata(target) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    FileExt::unlock(&file).map_err(|unlock_error| {
                        format!(
                            "file.write vanished target '{}' cannot be unlocked: {unlock_error}",
                            target.display()
                        )
                    })?;
                    continue;
                }
                Err(error) => {
                    return Err(format!(
                        "file.write target '{}' cannot be reinspected after locking: {error}",
                        target.display()
                    ));
                }
            };
            if !same_file_identity(&locked, &current) {
                FileExt::unlock(&file).map_err(|error| {
                    format!(
                        "file.write replaced target '{}' cannot be unlocked: {error}",
                        target.display()
                    )
                })?;
                continue;
            }
            return Ok(Some(Self { file }));
        }
    }

    fn current_hash(&mut self) -> Result<String, String> {
        self.file
            .seek(std::io::SeekFrom::Start(0))
            .map_err(|error| format!("file.write locked target cannot seek: {error}"))?;
        let mut bytes = Vec::new();
        self.file
            .read_to_end(&mut bytes)
            .map_err(|error| format!("file.write locked target cannot be read: {error}"))?;
        Ok(ContentHash::of_bytes(&bytes).to_hex())
    }

    fn release(self) -> Result<(), String> {
        FileExt::unlock(&self.file)
            .map_err(|error| format!("file.write target lock cannot be released: {error}"))
    }
}

#[cfg(unix)]
fn same_file_identity(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file_identity(_left: &std::fs::Metadata, _right: &std::fs::Metadata) -> bool {
    true
}

fn optional_hash(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(ContentHash::of_bytes(&bytes).to_hex())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "file.write could not read target '{}' under its write lock: {error}",
            path.display()
        )),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileWriteEffectRecord {
    schema_version: u32,
    artifact_id: String,
    writes: Vec<FileWriteEffectEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileWriteEffectEntry {
    path: PathBuf,
    before_hash: Option<String>,
    after_hash: String,
}

fn parse_write_requests(
    arguments: &serde_json::Value,
    roots: &[AbsPath],
    max_payload_bytes: u64,
) -> Result<Vec<WriteRequest>, String> {
    let mut budget = DecodeBudget::new(max_payload_bytes)?;
    let values = match arguments.get("writes") {
        Some(serde_json::Value::Array(values)) if !values.is_empty() => values.clone(),
        Some(serde_json::Value::Array(_)) => {
            return Err("file.write 'writes' must not be empty".to_string());
        }
        Some(_) => return Err("file.write 'writes' must be an array".to_string()),
        None => vec![arguments.clone()],
    };
    let mut seen = BTreeSet::new();
    let mut requests = Vec::with_capacity(values.len());
    for value in values {
        let path = value
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "file.write entry must include non-empty string 'path'".to_string())?;
        let requested = PathBuf::from(path);
        if !requested.is_absolute() {
            return Err(format!(
                "file.write path must be absolute (the path is the permission): {path}"
            ));
        }
        let declared = AbsPath::new(&requested)
            .map_err(|e| format!("file.write path failed closed before the root check: {e}"))?;
        let requested = canonicalize_declared_path(declared.as_path())?;
        if !roots.iter().any(|root| requested.is_within(root)) {
            return Err(format!(
                "file.write refused closed: path '{}' is outside this room's permission — the \
                 granted roots are [{}]",
                declared.as_str(),
                roots
                    .iter()
                    .map(AbsPath::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let target = requested.as_path().to_path_buf();
        if !seen.insert(target.clone()) {
            return Err(format!(
                "file.write batch repeats path '{}'",
                target.display()
            ));
        }
        let observed_before_bytes = match std::fs::File::open(&target) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(max_payload_bytes.checked_add(1).ok_or("file.write read limit overflow")?)
                    .read_to_end(&mut bytes).map_err(|error| error.to_string())?;
                if bytes.len() as u64 > max_payload_bytes { return Err("file.write target exceeds host payload limit".into()); }
                Some(bytes)
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(format!(
                    "file.write could not read existing target '{}': {error}",
                    target.display()
                ));
            }
        };
        if let Some(before) = &observed_before_bytes { budget.charge(before.len() as u64)?; }
        let format: Option<JsonFormat> = value.get("json_format").cloned()
            .map(serde_json::from_value).transpose().map_err(|error| error.to_string())?;
        if format.is_some() && value.get("patch").is_none() {
            return Err("file.write json_format belongs only to patch writes".into());
        }
        let observed_before_hash = observed_before_bytes
            .as_deref()
            .map(ContentHash::of_bytes)
            .map(|hash| hash.to_hex());
        let (content, evidence_before_hash, already_applied) = match (
            value.get("content"),
            value.get("patch"),
        ) {
            (Some(_), Some(_)) => {
                return Err(
                    "file.write entry accepts exactly one of 'content' or 'patch'".to_string(),
                );
            }
            (None, None) => {
                return Err(
                    "file.write entry requires exactly one of 'content' or 'patch'".to_string(),
                );
            }
            (Some(serde_json::Value::String(content)), None) => {
                if value.get("expected_after_hash").is_some() {
                    return Err(
                        "file.write expected_after_hash belongs only to patch writes".to_string(),
                    );
                }
                let desired_after_hash = ContentHash::of_bytes(content.as_bytes()).to_hex();
                let expected_before = match (
                    value.get("expected_before_hash"),
                    value.get("expected_before_content"),
                ) {
                    (None, None) => None,
                    (Some(_), Some(_)) => {
                        return Err(
                                "file.write accepts only one of expected_before_hash or expected_before_content"
                                    .to_string(),
                            );
                    }
                    (Some(serde_json::Value::Null), None) => {
                        Some((observed_before_bytes.is_none(), None))
                    }
                    (Some(serde_json::Value::String(expected)), None) => {
                        let expected =
                            parse_content_hash(expected, "expected_before_hash")?.to_string();
                        Some((
                            observed_before_hash.as_deref() == Some(expected.as_str()),
                            Some(expected),
                        ))
                    }
                    (Some(_), None) => {
                        return Err(
                                "file.write expected_before_hash must be a lowercase 64-character hash or null"
                                    .to_string(),
                            );
                    }
                    (None, Some(serde_json::Value::Null)) => {
                        Some((observed_before_bytes.is_none(), None))
                    }
                    (None, Some(serde_json::Value::String(expected))) => Some((
                        observed_before_bytes.as_deref() == Some(expected.as_bytes()),
                        Some(ContentHash::of_bytes(expected.as_bytes()).to_hex()),
                    )),
                    (None, Some(_)) => {
                        return Err(
                            "file.write expected_before_content must be a string or null"
                                .to_string(),
                        );
                    }
                };
                let already_applied =
                    observed_before_hash.as_deref() == Some(desired_after_hash.as_str());
                let evidence_before_hash = match expected_before.as_ref() {
                    Some((false, expected)) if already_applied => expected.clone(),
                    Some((false, expected)) => {
                        return Err(format!(
                            "file.write optimistic hash mismatch for '{}': expected {}, observed {}",
                            target.display(),
                            expected.as_deref().unwrap_or("<absent>"),
                            observed_before_hash.as_deref().unwrap_or("<absent>")
                        ));
                    }
                    Some((true, expected)) => expected.clone(),
                    None => observed_before_hash.clone(),
                };
                (content.clone(), evidence_before_hash, already_applied)
            }
            (Some(_), None) => {
                return Err("file.write entry 'content' must be a string".to_string());
            }
            (None, Some(patch_value)) => {
                if value.get("expected_before_content").is_some() {
                    return Err(
                        "file.write patch does not accept expected_before_content".to_string()
                    );
                }
                let expected_before = value
                    .get("expected_before_hash")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        "file.write patch requires string expected_before_hash".to_string()
                    })?;
                let expected_before =
                    parse_content_hash(expected_before, "expected_before_hash")?.to_string();
                let expected_after = value
                    .get("expected_after_hash")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        "file.write patch requires string expected_after_hash".to_string()
                    })?;
                let expected_after =
                    parse_content_hash(expected_after, "expected_after_hash")?.to_string();
                let patch: json_patch::Patch = serde_json::from_value(patch_value.clone())
                    .map_err(|error| format!("file.write patch is malformed: {error}"))?;
                if patch.is_empty() {
                    return Err("file.write patch must not be empty".to_string());
                }
                if observed_before_hash.as_deref() == Some(expected_after.as_str()) {
                    let bytes = observed_before_bytes.as_ref().ok_or_else(|| {
                        "file.write patch observed an after hash without target bytes".to_string()
                    })?;
                    serde_json::from_slice::<serde_json::Value>(bytes).map_err(|error| {
                        format!(
                            "file.write already-applied patch target '{}' is not JSON: {error}",
                            target.display()
                        )
                    })?;
                    let content = String::from_utf8(bytes.clone()).map_err(|error| {
                        format!(
                            "file.write already-applied patch target '{}' is not UTF-8: {error}",
                            target.display()
                        )
                    })?;
                    (content, Some(expected_before), true)
                } else {
                    if observed_before_hash.as_deref() != Some(expected_before.as_str()) {
                        return Err(format!(
                            "file.write patch optimistic hash mismatch for '{}': expected {}, observed {}",
                            target.display(),
                            expected_before,
                            observed_before_hash.as_deref().unwrap_or("<absent>")
                        ));
                    }
                    let before = observed_before_bytes.as_ref().ok_or_else(|| {
                        format!(
                            "file.write patch target '{}' does not exist",
                            target.display()
                        )
                    })?;
                    let content = apply_checked_json_patch(&target, before, &patch, &expected_after, format.unwrap_or_default())?;
                    (content, Some(expected_before), false)
                }
            }
        };
        budget.charge(content.len() as u64)?;
        requests.push(WriteRequest {
            target,
            content,
            before_hash: evidence_before_hash,
            observed_hash: observed_before_hash,
            observed_bytes: observed_before_bytes,
            already_applied,
        });
    }
    Ok(requests)
}

fn apply_checked_json_patch(
    target: &Path,
    before: &[u8],
    patch: &json_patch::Patch,
    expected_after: &str,
    format: JsonFormat,
) -> Result<String, String> {
    parse_content_hash(expected_after, "expected_after_hash")?;
    if patch.is_empty() {
        return Err("file.write patch must not be empty".into());
    }
    let mut document: serde_json::Value = serde_json::from_slice(before)
        .map_err(|error| format!("file.write patch target '{}' is not JSON: {error}", target.display()))?;
    json_patch::patch(&mut document, patch)
        .map_err(|error| format!("file.write patch could not apply to '{}': {error}", target.display()))?;
    let bytes = format.encode(&document)
        .map_err(|error| format!("file.write patched JSON '{}' could not encode: {error}", target.display()))?;
    let observed_after = ContentHash::of_bytes(&bytes).to_hex();
    if observed_after != expected_after {
        return Err(format!("file.write patch for '{}' produced hash {}, expected {}", target.display(), observed_after, expected_after));
    }
    String::from_utf8(bytes)
        .map_err(|error| format!("file.write patched JSON '{}' is not UTF-8: {error}", target.display()))
}

fn parse_content_hash<'a>(value: &'a str, field: &str) -> Result<&'a str, String> {
    if value.len() != 64
        || value.bytes().any(|byte| !byte.is_ascii_hexdigit())
        || value.bytes().any(|byte| byte.is_ascii_uppercase())
        || ContentHash::from_hex(value).is_none()
    {
        return Err(format!(
            "file.write {field} must be a lowercase 64-character content hash"
        ));
    }
    Ok(value)
}

fn bind_effect_record(
    input: &InterfaceInvocationEnvelope,
    effect_root: &Path,
    requests: &mut [WriteRequest],
) -> Result<(), String> {
    if !effect_root.is_absolute() {
        return Err(format!(
            "file.write effect root must be absolute: {}",
            effect_root.display()
        ));
    }
    std::fs::create_dir_all(effect_root).map_err(|error| {
        format!(
            "file.write effect root '{}' cannot be created: {error}",
            effect_root.display()
        )
    })?;
    let effect_path = effect_root.join(format!("{}.json", input.artifact.content.to_hex()));
    let record = match read_effect_record(&effect_path)? {
        Some(record) => record,
        None => {
            let record = FileWriteEffectRecord {
                schema_version: 1,
                artifact_id: input.artifact.id.as_str().to_string(),
                writes: requests
                    .iter()
                    .map(|request| FileWriteEffectEntry {
                        path: request.target.clone(),
                        before_hash: request.before_hash.clone(),
                        after_hash: ContentHash::of_bytes(request.content.as_bytes()).to_hex(),
                    })
                    .collect(),
            };
            let mut bytes = serde_json::to_vec(&record)
                .map_err(|error| format!("file.write effect record did not encode: {error}"))?;
            bytes.push(b'\n');
            install_effect_record(&effect_path, &bytes)?;
            read_effect_record(&effect_path)?.ok_or_else(|| {
                format!(
                    "file.write effect record '{}' disappeared after installation",
                    effect_path.display()
                )
            })?
        }
    };
    if record.schema_version != 1 {
        return Err(format!(
            "file.write effect record '{}' has schema_version {}, expected 1",
            effect_path.display(),
            record.schema_version
        ));
    }
    if record.artifact_id != input.artifact.id.as_str() {
        return Err(format!(
            "file.write effect record '{}' belongs to artifact '{}', not '{}'",
            effect_path.display(),
            record.artifact_id,
            input.artifact.id.as_str()
        ));
    }
    if record.writes.len() != requests.len() {
        return Err(format!(
            "file.write effect record '{}' contains {} writes, request contains {}",
            effect_path.display(),
            record.writes.len(),
            requests.len()
        ));
    }
    for (index, (request, recorded)) in requests.iter_mut().zip(record.writes.iter()).enumerate() {
        let desired_after = ContentHash::of_bytes(request.content.as_bytes()).to_hex();
        if recorded.path != request.target || recorded.after_hash != desired_after {
            return Err(format!(
                "file.write effect record '{}' row {} does not match the retried request",
                effect_path.display(),
                index
            ));
        }
        let observed = request.observed_hash.as_deref();
        if observed == Some(recorded.after_hash.as_str()) {
            request.already_applied = true;
        } else if observed == recorded.before_hash.as_deref() {
            request.already_applied = false;
        } else {
            return Err(format!(
                "file.write effect record '{}' row {} expects materialized hash {} or {}, observed {}",
                effect_path.display(),
                index,
                recorded.before_hash.as_deref().unwrap_or("<absent>"),
                recorded.after_hash,
                observed.unwrap_or("<absent>")
            ));
        }
        request.before_hash = recorded.before_hash.clone();
    }
    Ok(())
}

fn read_effect_record(path: &Path) -> Result<Option<FileWriteEffectRecord>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "file.write effect record '{}' cannot be read: {error}",
                path.display()
            ));
        }
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|error| {
        format!(
            "file.write effect record '{}' is malformed: {error}",
            path.display()
        )
    })
}

fn install_effect_record(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let staging_root = path.parent().ok_or_else(|| {
        format!(
            "file.write effect record '{}' has no staging parent",
            path.display()
        )
    })?;
    let staged = stage_write(staging_root, path, bytes)?;
    let linked = std::fs::hard_link(&staged, path);
    let _ = std::fs::remove_file(&staged);
    match linked {
        Ok(()) => sync_parent(path),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = std::fs::read(path).map_err(|read_error| {
                format!(
                    "file.write concurrent effect record '{}' cannot be read: {read_error}",
                    path.display()
                )
            })?;
            if existing != bytes {
                return Err(format!(
                    "file.write concurrent effect record '{}' has different bytes",
                    path.display()
                ));
            }
            Ok(())
        }
        Err(error) => Err(format!(
            "file.write effect record '{}' cannot be installed: {error}",
            path.display()
        )),
    }
}

fn ensure_room_kind_granted(
    input: &InterfaceInvocationEnvelope,
    room_kinds: &[String],
) -> Result<(), String> {
    if room_kinds.is_empty() {
        return Err(
            "file.write registry row names no supported_worker_environment_kinds — the tool \
             has no room to be granted through"
                .to_string(),
        );
    }
    let granted = invocation_granted_wek(input)?;
    if !room_kinds
        .iter()
        .any(|kind| granted.iter().any(|g| g == kind))
    {
        return Err(format!(
            "file.write refused closed: none of this tool's rooms [{}] is inside the formed \
             worker-environment grant [{}]",
            room_kinds.join(", "),
            granted.join(", ")
        ));
    }
    Ok(())
}

fn invocation_granted_wek(input: &InterfaceInvocationEnvelope) -> Result<Vec<String>, String> {
    Ok(input
        .worker_environment_kinds()
        .map_err(|e| e.to_string())?
        .to_vec())
}

fn configured_write_roots(values: &[PathBuf]) -> Result<Vec<AbsPath>, String> {
    let mut roots = Vec::with_capacity(values.len());
    for path in values {
        if !path.is_absolute() {
            return Err(format!(
                "file.write configured root must be absolute: {}",
                path.display()
            ));
        }
        roots.push(canonicalize_declared_path(path).map_err(|error| {
            format!(
                "file.write configured root '{}' failed closed: {error}",
                path.display()
            )
        })?);
    }
    if roots.is_empty() {
        return Err(
            "file.write refused closed: configured write roots are empty — the room holds no \
             roots, so it holds no permission"
                .to_string(),
        );
    }
    Ok(roots)
}

fn retain_write_inputs(effect_root: &Path, requests: &[WriteRequest]) -> Result<(), String> {
    let blobs = effect_root.join("blobs");
    std::fs::create_dir_all(&blobs).map_err(|error| {
        format!(
            "file.write evidence root '{}' cannot be created: {error}",
            blobs.display()
        )
    })?;
    for request in requests {
        for bytes in request
            .observed_bytes
            .as_deref()
            .into_iter()
            .chain(std::iter::once(request.content.as_bytes()))
        {
            let hash = ContentHash::of_bytes(bytes).to_hex();
            let path = blobs.join(hash);
            install_effect_record(&path, bytes)?;
            if std::fs::read(&path).map_err(|error| {
                format!(
                    "file.write evidence '{}' cannot be read: {error}",
                    path.display()
                )
            })? != bytes
            {
                return Err(format!(
                    "file.write evidence '{}' has inconsistent bytes",
                    path.display()
                ));
            }
        }
        if let Some(before_hash) = &request.before_hash {
            parse_content_hash(before_hash, "recorded before_hash")?;
            let path = blobs.join(before_hash);
            let bytes = std::fs::read(&path).map_err(|error| {
                format!(
                    "file.write preimage '{}' is unavailable: {error}",
                    path.display()
                )
            })?;
            if ContentHash::of_bytes(&bytes).to_hex() != *before_hash {
                return Err(format!(
                    "file.write preimage '{}' differs from its pinned hash",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

fn canonicalize_declared_path(path: &Path) -> Result<AbsPath, String> {
    let normalized = AbsPath::new(path)
        .map_err(|error| format!("path '{}' is invalid: {error}", path.display()))?;
    let mut existing = normalized.as_path().to_path_buf();
    let mut suffix = Vec::new();
    loop {
        match std::fs::canonicalize(&existing) {
            Ok(mut canonical) => {
                for component in suffix.iter().rev() {
                    canonical.push(component);
                }
                return AbsPath::new(&canonical).map_err(|error| {
                    format!(
                        "canonical path '{}' is invalid: {error}",
                        canonical.display()
                    )
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let component = existing.file_name().ok_or_else(|| {
                    format!("path '{}' has no existing ancestor", normalized.as_str())
                })?;
                suffix.push(component.to_os_string());
                existing = existing
                    .parent()
                    .ok_or_else(|| {
                        format!("path '{}' has no existing ancestor", normalized.as_str())
                    })?
                    .to_path_buf();
            }
            Err(error) => {
                return Err(format!(
                    "path '{}' cannot be canonicalized: {error}",
                    existing.display()
                ));
            }
        }
    }
}

fn stage_write(
    staging_root: &std::path::Path,
    target: &std::path::Path,
    bytes: &[u8],
) -> Result<PathBuf, String> {
    static NONCE: AtomicU64 = AtomicU64::new(0);
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            format!(
                "file.write path '{}' has no file name to stage beside",
                target.display()
            )
        })?;
    for _ in 0..128 {
        let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
        let staged = staging_root.join(format!(
            ".{name}.octopus-file-write-{}-{nonce}.tmp",
            std::process::id()
        ));
        let mut file = match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staged)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "file.write could not create staging file '{}': {error}",
                    staged.display()
                ));
            }
        };
        if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
            let _ = std::fs::remove_file(&staged);
            return Err(format!(
                "file.write could not durably stage '{}': {error}",
                staged.display()
            ));
        }
        return Ok(staged);
    }
    Err(format!(
        "file.write could not reserve a staging file beside '{}'",
        target.display()
    ))
}

fn sync_parent(target: &std::path::Path) -> Result<(), String> {
    let parent = target.parent().ok_or_else(|| {
        format!(
            "file.write target '{}' has no parent to synchronize",
            target.display()
        )
    })?;
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            format!(
                "file.write could not synchronize parent directory '{}': {error}",
                parent.display()
            )
        })
}

#[cfg(all(test, unix))]
mod path_tests {
    use super::*;

    #[test]
    fn canonical_roots_accept_aliases_and_reject_symlink_escape() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        let alias = temp.path().join("alias");
        let foreign = temp.path().join("foreign");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::create_dir_all(&foreign).unwrap();
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        std::os::unix::fs::symlink(&foreign, real.join("escape")).unwrap();

        let roots = configured_write_roots(std::slice::from_ref(&real)).unwrap();
        let through_alias = canonicalize_declared_path(&alias.join("inside.json")).unwrap();
        let escaped = canonicalize_declared_path(&real.join("escape/outside.json")).unwrap();

        assert!(through_alias.is_within(&roots[0]));
        assert!(!escaped.is_within(&roots[0]));
    }
}
