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
#[serde(deny_unknown_fields)]
struct Entry {
    path: PathBuf,
    content: String,
    expected_before_content: serde_json::Value,
    expected_before_hash: Option<String>,
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
) -> Result<serde_json::Value, ToolResultError> {
    let receipts = execute_batch(input, arguments, room_kinds, root_slots, effect_root)
        .map_err(|error| ToolResultError::new("file_write_evidence_failed", error))?;
    let body = serde_json::json!({
        "result": "ordered_file_writes",
        "writes": receipts,
    });
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
) -> Result<Vec<Receipt>, String> {
    ensure_room_kind_granted(input, room_kinds)?;
    let roots = configured_write_roots(&root_slots.values().cloned().collect::<Vec<_>>())?;
    let mut batch: Batch =
        serde_json::from_value(arguments.clone()).map_err(|error| error.to_string())?;
    if !batch.ordered || batch.writes.is_empty() || !effect_root.is_absolute() {
        return Err("file.write ordered batch requires ordered=true, nonempty writes and an absolute effect root".into());
    }
    let mut paths = BTreeSet::new();
    for entry in &mut batch.writes {
        if !entry.path.is_absolute() {
            return Err("file.write ordered path must be absolute".into());
        }
        let path = canonicalize_declared_path(&entry.path)?;
        if !roots.iter().any(|root| path.is_within(root))
            || !paths.insert(path.as_path().to_path_buf())
        {
            return Err(
                "file.write ordered paths must be distinct and within the granted roots".into(),
            );
        }
        if !entry.expected_before_content.is_null() && !entry.expected_before_content.is_string() {
            return Err(
                "file.write ordered expected_before_content must be explicit text or null".into(),
            );
        }
        if entry.expected_before_hash
            != entry
                .expected_before_content
                .as_str()
                .map(|s| ContentHash::of_bytes(s.as_bytes()).to_hex())
        {
            return Err("file.write ordered preimage hash must match its explicit content".into());
        }
        entry.path = path.as_path().to_path_buf();
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
        "artifact": input.artifact,
        "batch": batch,
    }))
    .map_err(|error| error.to_string())?;
    install_effect_record(&directory.join("intent.json"), &intent)?;
    let mut receipts = Vec::with_capacity(batch.writes.len());
    let mut stopped = false;
    for (index, entry) in batch.writes.iter().enumerate() {
        let receipt_path = directory.join(format!("{index}.receipt.json"));
        let expected = Receipt {
            path: entry.path.clone(),
            before_hash: entry
                .expected_before_content
                .as_str()
                .map(|s| ContentHash::of_bytes(s.as_bytes()).to_hex()),
            after_hash: ContentHash::of_bytes(entry.content.as_bytes()).to_hex(),
            bytes: entry.content.len() as u64,
            outcome: Outcome::NotAttempted,
            observed_hash: None,
            error: None,
        };
        let receipt = match std::fs::read(&receipt_path) {
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
    FileExt::unlock(&lock).map_err(|error| error.to_string())?;
    Ok(receipts)
}

fn commit_entry(
    entry: &Entry,
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
    if let Some(before) = entry.expected_before_content.as_str() {
        install_effect_record(
            &directory.join(format!("{index}.before")),
            before.as_bytes(),
        )?;
    }
    install_effect_record(
        &prepared,
        &serde_json::to_vec(&receipt).map_err(|error| error.to_string())?,
    )?;
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
    save_receipt(receipt, directory, index)
}

fn save_receipt(receipt: Receipt, directory: &Path, index: usize) -> Result<Receipt, String> {
    install_effect_record(
        &directory.join(format!("{index}.receipt.json")),
        &serde_json::to_vec(&receipt).map_err(|error| error.to_string())?,
    )?;
    Ok(receipt)
}
