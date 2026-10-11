use super::*;
use octopus_tool_handlers::ToolResultError;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Batch {
    ordered: bool,
    writes: Vec<Entry>,
}

#[derive(Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
enum Entry {
    Content {
        path: PathBuf,
        content: String,
        expected_before_content: serde_json::Value,
        expected_before_hash: serde_json::Value,
    },
    Patch {
        path: PathBuf,
        patch: json_patch::Patch,
        expected_before_content: Preimage,
        expected_before_hash: String,
        expected_after_hash: String,
        #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "octopus_file_write_contract::optional_json_format")]
        json_format: Option<JsonFormat>,
    },
}

struct PreparedEntry {
    path: PathBuf,
    content: String,
    expected_before_hash: Option<String>,
}

impl Entry {
    fn path_mut(&mut self) -> &mut PathBuf {
        match self {
            Self::Content { path, .. } | Self::Patch { path, .. } => path,
        }
    }

    fn prepare(&self, budget: &mut DecodeBudget) -> Result<PreparedEntry, String> {
        let decoded;
        let (path, content, before, expected) = match self {
            Self::Content { path, content, expected_before_content, expected_before_hash } => {
                let before = match expected_before_content {
                    serde_json::Value::Null => None,
                    serde_json::Value::String(value) => Some(value.as_str()),
                    _ => return Err("file.write ordered expected_before_content must be explicit text or null".into()),
                };
                let expected = match expected_before_hash {
                    serde_json::Value::Null => None,
                    serde_json::Value::String(value) => Some(value.as_str()),
                    _ => return Err("file.write ordered expected_before_hash must be explicit text or null".into()),
                };
                (path, content.clone(), before, expected)
            }
            Self::Patch { path, patch, expected_before_content, expected_before_hash, expected_after_hash, json_format } => {
                decoded = expected_before_content.decode(expected_before_hash, budget)?;
                let content = apply_checked_json_patch(path, decoded.as_bytes(), patch, expected_after_hash, json_format.unwrap_or_default())?;
                (path, content, Some(decoded.as_str()), Some(expected_before_hash.as_str()))
            }
        };
        let before_hash = before.map(|value| ContentHash::of_bytes(value.as_bytes()).to_hex());
        if before_hash.as_deref() != expected {
            return Err("file.write ordered preimage hash must match its explicit content".into());
        }
        if matches!(self, Self::Content { .. }) {
            budget.charge(before.map_or(0, |text| text.len() as u64))?;
        }
        budget.charge(content.len() as u64)?;
        Ok(PreparedEntry { path: path.clone(), content, expected_before_hash: before_hash })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    path: PathBuf,
    before_hash: Option<String>,
    after_hash: String,
    bytes: u64,
    outcome: Outcome,
    observed_hash: Option<String>,
    error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Applied,
    Refused,
    Uncertain,
    NotAttempted,
}

pub(super) fn execute(
    input: &InterfaceInvocationEnvelope,
    arguments: &serde_json::Value,
    room_kinds: &[String],
    root_slots: &BTreeMap<String, PathBuf>,
    effect_root: &Path,
    max_payload_bytes: u64,
) -> Result<serde_json::Value, ToolResultError> {
    let (receipts, retirement_error) = execute_batch(input, arguments, room_kinds, root_slots, effect_root, max_payload_bytes)
        .map_err(|error| ToolResultError::new("file_write_evidence_failed", error))?;
    let body = serde_json::json!({
        "result": "ordered_file_writes",
        "writes": receipts,
    });
    if let Some(error) = retirement_error {
        return Err(ToolResultError::new("file_write_evidence_failed", error)
            .with_details(serde_json::Map::from_iter([("file_write".into(), body)])));
    }
    match receipts
        .iter()
        .find(|receipt| receipt.outcome != Outcome::Applied)
    {
        None => Ok(body),
        Some(receipt) => Err(ToolResultError::new(
            "file_write_incomplete",
            receipt
                .error
                .clone()
                .unwrap_or_else(|| "file.write stopped before completing its ordered batch".into()),
        )
        .with_details(serde_json::Map::from_iter([("file_write".into(), body)]))),
    }
}

fn execute_batch(
    input: &InterfaceInvocationEnvelope,
    arguments: &serde_json::Value,
    room_kinds: &[String],
    root_slots: &BTreeMap<String, PathBuf>,
    effect_root: &Path,
    max_payload_bytes: u64,
) -> Result<(Vec<Receipt>, Option<String>), String> {
    ensure_room_kind_granted(input, room_kinds)?;
    let roots = configured_write_roots(&root_slots.values().cloned().collect::<Vec<_>>())?;
    let mut batch: Batch =
        serde_json::from_value(arguments.clone()).map_err(|error| error.to_string())?;
    if !batch.ordered || batch.writes.is_empty() || !effect_root.is_absolute() {
        return Err("file.write ordered batch requires ordered=true, nonempty writes and an absolute effect root".into());
    }
    let mut budget = DecodeBudget::new(max_payload_bytes)?;
    let mut paths = BTreeSet::new();
    let mut entries = Vec::with_capacity(batch.writes.len());
    for entry in &mut batch.writes {
        let requested = entry.path_mut();
        if !requested.is_absolute() {
            return Err("file.write ordered path must be absolute".into());
        }
        let path = canonicalize_declared_path(requested)?;
        if !roots.iter().any(|root| path.is_within(root))
            || !paths.insert(path.as_path().to_path_buf())
        {
            return Err(
                "file.write ordered paths must be distinct and within the granted roots".into(),
            );
        }
        *requested = path.as_path().to_path_buf();
        entries.push(entry.prepare(&mut budget)?);
    }
    let directory = effect_root
        .join("ordered")
        .join(input.artifact.content.to_hex());
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("lock"))
        .map_err(|error| error.to_string())?;
    lock.try_lock_exclusive()
        .map_err(|error| format!("file.write exact artifact is already executing: {error}"))?;
    let intent = serde_json::to_vec(&serde_json::json!({
        "schema_version": 2,
        "artifact": input.artifact,
        "batch_content_hash": ContentHash::of_bytes(
            &octopus_descriptors::canonical_bytes(
                &serde_json::to_value(&batch).map_err(|error| error.to_string())?
            )
        ).to_hex(),
        "writes": entries.iter().map(|entry| serde_json::json!({
            "path": entry.path,
            "before_hash": entry.expected_before_hash,
            "after_hash": ContentHash::of_bytes(entry.content.as_bytes()).to_hex(),
            "bytes": entry.content.len(),
        })).collect::<Vec<_>>(),
    }))
    .map_err(|error| error.to_string())?;
    install_effect_record(&directory.join("intent.json"), &intent)?;
    let mut receipts = Vec::with_capacity(entries.len());
    let mut stopped = false;
    for (index, entry) in entries.iter().enumerate() {
        let receipt_path = directory.join(format!("{index}.receipt.json"));
        let expected = Receipt {
            path: entry.path.clone(),
            before_hash: entry.expected_before_hash.clone(),
            after_hash: ContentHash::of_bytes(entry.content.as_bytes()).to_hex(),
            bytes: entry.content.len() as u64,
            outcome: Outcome::NotAttempted,
            observed_hash: None,
            error: None,
        };
        let receipt = match open_receipt(&receipt_path).and_then(|mut file| {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).map(|_| bytes)
        }) {
            Ok(bytes) => {
                let receipt: Receipt =
                    serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
                if receipt.path != expected.path
                    || receipt.before_hash != expected.before_hash
                    || receipt.after_hash != expected.after_hash
                    || receipt.bytes != expected.bytes
                    || (stopped && receipt.outcome != Outcome::NotAttempted)
                    || (!stopped && receipt.outcome == Outcome::NotAttempted)
                    || (receipt.outcome == Outcome::Applied
                        && (receipt.error.is_some()
                            || receipt.observed_hash.as_ref() != Some(&receipt.after_hash)))
                {
                    return Err("file.write ordered receipt differs from its immutable invocation or preceding outcome".into());
                }
                receipt
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let receipt = if stopped {
                    expected
                } else {
                    match commit_entry(entry, expected.clone(), &directory, index) {
                        Ok(receipt) => receipt,
                        Err(error) => Receipt {
                            outcome: Outcome::Uncertain,
                            error: Some(error),
                            ..expected
                        },
                    }
                };
                let bytes = serde_json::to_vec(&receipt).map_err(|error| error.to_string())?;
                install_effect_record(&receipt_path, &bytes)?;
                receipt
            }
            Err(error) => return Err(error.to_string()),
        };
        stopped |= receipt.outcome != Outcome::Applied;
        receipts.push(receipt);
    }
    let retirement = receipts.iter().enumerate()
        .filter(|(_, receipt)| receipt.outcome == Outcome::Applied)
        .try_for_each(|(index, receipt)| retire_stage(&directory, index, receipt));
    let unlocked = FileExt::unlock(&lock).map_err(|error| error.to_string());
    Ok((receipts, retirement.and(unlocked).err()))
}

fn open_receipt(path: &Path) -> std::io::Result<std::fs::File> {
    if !std::fs::symlink_metadata(path)?.is_file() {
        return Err(std::io::Error::other("file.write ordered receipt is not a regular file"));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("file.write ordered receipt handle is not a regular file"));
    }
    Ok(file)
}

fn retire_stage(directory: &Path, index: usize, receipt: &Receipt) -> Result<(), String> {
    let retire = || -> Result<(), String> {
        let receipt_path = directory.join(format!("{index}.receipt.json"));
        let mut file = open_receipt(&receipt_path).map_err(|error| error.to_string())?;
        let limit = serde_json::to_vec(receipt).map_err(|error| error.to_string())?.len() as u64;
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file).take(limit + 1).read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > limit
            || serde_json::from_slice::<Receipt>(&bytes).map_err(|error| error.to_string())? != *receipt
        {
            return Err("applied receipt changed before its durability barrier".into());
        }
        file.sync_all().map_err(|error| error.to_string())?;
        sync_parent(&receipt_path)?;
        let stage = directory.join(format!("{index}.bytes"));
        match std::fs::symlink_metadata(&stage) {
            Ok(metadata) if metadata.is_file() => {
                std::fs::remove_file(&stage).map_err(|error| error.to_string())?;
            }
            Ok(_) => return Err("applied stage is not a regular private file".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error.to_string()),
        }
        sync_parent(&stage)
    };
    retire().map_err(|error| format!("file.write applied entry {index} stage retirement failed: {error}"))
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Interruption { Prepared, DurableTarget }

#[cfg(test)]
thread_local! {
    static INTERRUPTION: std::cell::Cell<Option<Interruption>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn interrupt_at(point: Interruption) {
    INTERRUPTION.with(|selected| {
        if selected.get() == Some(point) {
            selected.set(None);
            panic!("interrupted ordered file.write before receipt publication");
        }
    });
}

#[cfg(all(test, unix))]
#[path = "../../tests/support/ordered_stage.rs"]
mod stage_tests;

fn commit_entry(
    entry: &PreparedEntry,
    mut receipt: Receipt,
    directory: &Path,
    index: usize,
) -> Result<Receipt, String> {
    let parent = entry
        .path
        .parent()
        .ok_or("file.write target has no parent")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let stage = directory.join(format!("{index}.bytes"));
    let prepared = directory.join(format!("{index}.prepared.json"));
    let resuming = prepared.try_exists().map_err(|error| error.to_string())?;
    let mut lock = TargetWriteLock::acquire(&entry.path)?;
    let observed = match lock.as_mut() {
        Some(lock) => Some(lock.current_hash()?),
        None => None,
    };
    if resuming {
        let saved: Receipt =
            serde_json::from_slice(&std::fs::read(&prepared).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        if saved != receipt || optional_hash(&stage)?.as_ref() != Some(&receipt.after_hash) {
            return Err("file.write prepared entry has inconsistent pinned evidence".into());
        }
        if observed.as_ref() == Some(&receipt.after_hash)
            && same_file_identity(
                &std::fs::metadata(&stage).map_err(|error| error.to_string())?,
                &std::fs::metadata(&entry.path).map_err(|error| error.to_string())?,
            )
        {
            sync_parent(&entry.path)?;
            receipt.outcome = Outcome::Applied;
            receipt.observed_hash = observed;
            return save_receipt(receipt, directory, index);
        }
    }
    if observed != receipt.before_hash {
        receipt.outcome = if resuming {
            Outcome::Uncertain
        } else {
            Outcome::Refused
        };
        receipt.error = Some(format!(
            "file.write ordered preimage differs at '{}': expected {}, observed {}{}",
            entry.path.display(),
            receipt.before_hash.as_deref().unwrap_or("<absent>"),
            observed.as_deref().unwrap_or("<absent>"),
            if resuming {
                "; interrupted effect cannot be attributed"
            } else {
                ""
            }
        ));
        receipt.observed_hash = observed;
        return save_receipt(receipt, directory, index);
    }
    install_effect_record(&stage, entry.content.as_bytes())?;
    let staged_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&stage)
        .map_err(|error| error.to_string())?;
    staged_file
        .lock_exclusive()
        .map_err(|error| error.to_string())?;
    if let Some(lock) = &lock {
        staged_file
            .set_permissions(
                lock.file
                    .metadata()
                    .map_err(|error| error.to_string())?
                    .permissions(),
            )
            .map_err(|error| error.to_string())?;
        staged_file.sync_all().map_err(|error| error.to_string())?;
    }
    install_effect_record(
        &prepared,
        &serde_json::to_vec(&receipt).map_err(|error| error.to_string())?,
    )?;
    #[cfg(test)]
    interrupt_at(Interruption::Prepared);
    let link = directory.join(format!("{index}.install"));
    let installed = if receipt.before_hash.is_none() {
        std::fs::hard_link(&stage, &entry.path)
    } else {
        match std::fs::hard_link(&stage, &link) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if !same_file_identity(
                    &std::fs::metadata(&stage).map_err(|error| error.to_string())?,
                    &std::fs::metadata(&link).map_err(|error| error.to_string())?,
                ) {
                    return Err("file.write pending install link has foreign identity".into());
                }
            }
            Err(error) => return Err(error.to_string()),
        }
        std::fs::rename(&link, &entry.path)
    };
    if let Err(error) = installed {
        receipt.outcome = Outcome::Refused;
        receipt.observed_hash = optional_hash(&entry.path)?;
        receipt.error = Some(format!(
            "file.write ordered replacement failed at '{}': {error}",
            entry.path.display()
        ));
        return save_receipt(receipt, directory, index);
    }
    receipt.observed_hash = optional_hash(&entry.path)?;
    match sync_parent(&entry.path) {
        Ok(()) if receipt.observed_hash.as_ref() == Some(&receipt.after_hash) => {
            receipt.outcome = Outcome::Applied
        }
        outcome => {
            receipt.outcome = Outcome::Uncertain;
            receipt.error = Some(format!(
                "file.write ordered replacement at '{}' could not prove durable readback: {outcome:?}",
                entry.path.display()
            ));
        }
    }
    #[cfg(test)]
    if receipt.outcome == Outcome::Applied {
        interrupt_at(Interruption::DurableTarget);
    }
    save_receipt(receipt, directory, index)
}

fn save_receipt(receipt: Receipt, directory: &Path, index: usize) -> Result<Receipt, String> {
    install_effect_record(
        &directory.join(format!("{index}.receipt.json")),
        &serde_json::to_vec(&receipt).map_err(|error| error.to_string())?,
    )?;
    Ok(receipt)
}
