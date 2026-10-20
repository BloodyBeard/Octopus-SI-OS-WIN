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
                let content = apply_checked_json_patch(path, decoded.as_bytes(), patch, Some(expected_after_hash), json_format.unwrap_or_default())?;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    publication: Option<PublicationReceipt>,
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
        "writes": receipts.iter().map(|receipt| serde_json::json!({
            "path": receipt.path,
            "before_hash": receipt.before_hash,
            "after_hash": receipt.after_hash,
            "bytes": receipt.bytes,
            "outcome": receipt.outcome,
            "observed_hash": receipt.observed_hash,
            "error": receipt.error,
        })).collect::<Vec<_>>(),
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
    let lock = lock_invocation(&directory)?;
    let schema = match open_receipt(&directory.join("intent.json")) {
        Ok(file) => {
            let mut bytes = Vec::new();
            file.take(max_payload_bytes.saturating_add(64 * 1024)).read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            match serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|error| error.to_string())?
                .get("schema_version").and_then(serde_json::Value::as_u64)
            {
                Some(2) => 2,
                Some(3) => 3,
                _ => return Err("file.write ordered intent has an unsupported retained format".into()),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if std::fs::read_dir(&directory).map_err(|error| error.to_string())?
                .any(|entry| entry.map_or(true, |entry| entry.file_name() != "lock"))
            {
                return Err("file.write ordered evidence exists without its original intent".into());
            }
            3
        }
        Err(error) => return Err(error.to_string()),
    };
    let intent = serde_json::to_vec(&serde_json::json!({
        "schema_version": schema,
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
            publication: None,
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
                validate_receipt_publication(&receipt, input, index, schema)?;
                receipt
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let receipt = if stopped {
                    expected
                } else {
                    match commit_entry(entry, expected.clone(), &directory, index, input, schema, &roots) {
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
        .filter(|(_, receipt)| schema == 2 && receipt.outcome == Outcome::Applied)
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
        octopus_file_storage::sync_ordered(&file).map_err(|error| error.to_string())?;
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
enum Interruption { Published }

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

fn validate_receipt_publication(receipt: &Receipt, input: &InterfaceInvocationEnvelope, index: usize, schema: u32) -> Result<(), String> {
    if schema == 2 {
        if receipt.publication.is_some() {
            return Err("file.write retained schema-2 receipt contains schema-3 publication evidence".into());
        }
        return Ok(());
    }
    match &receipt.publication {
        Some(publication) => {
            let operation = publication_operation_id(input, true, index)?;
            check_publication(publication, &operation, &receipt.path, receipt.before_hash.as_deref(), &receipt.after_hash, receipt.bytes)?;
            let outcome = match publication.outcome {
                PublicationOutcome::Applied => Outcome::Applied,
                PublicationOutcome::NotApplied => Outcome::Refused,
                PublicationOutcome::Uncertain => Outcome::Uncertain,
            };
            if receipt.outcome != outcome || receipt.error != publication.error {
                return Err("file.write native outcome differs from its publication receipt".into());
            }
        }
        None if matches!(receipt.outcome, Outcome::Applied | Outcome::Refused) => {
            return Err("file.write schema-3 outcome has no attributable publication receipt".into());
        }
        None => (),
    }
    Ok(())
}

fn commit_entry(
    entry: &PreparedEntry,
    mut receipt: Receipt,
    directory: &Path,
    index: usize,
    input: &InterfaceInvocationEnvelope,
    schema: u32,
    roots: &[AbsPath],
) -> Result<Receipt, String> {
    let operation = publication_operation_id(input, true, index)?;
    let guard = acquire_publication(&entry.path, receipt.before_hash.as_deref(), roots, &operation)?;
    let prepared = directory.join(format!("{index}.prepared.json"));
    let publication = if schema == 2 {
        match open_receipt(&prepared) {
            Ok(file) => {
                let expected = serde_json::to_vec(&receipt).map_err(|error| error.to_string())?;
                let mut bytes = Vec::new();
                file.take(expected.len() as u64 + 1).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
                if bytes != expected {
                    return Err("file.write retained prepared entry differs from its original invocation".into());
                }
                guard.publish_retained(&operation, receipt.before_hash.as_deref(), entry.content.as_bytes(), DigestAlgorithm::Blake3,
                    &directory.join(format!("{index}.bytes")))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                guard.publish(&operation, receipt.before_hash.as_deref(), entry.content.as_bytes(), DigestAlgorithm::Blake3)
            }
            Err(error) => return Err(error.to_string()),
        }
    } else {
        guard.publish(&operation, receipt.before_hash.as_deref(), entry.content.as_bytes(), DigestAlgorithm::Blake3)
    }.map_err(|error| format!("file.write ordered publication at '{}' requires reconciliation: {error}", entry.path.display()))?;
    check_publication(&publication, &operation, &entry.path, receipt.before_hash.as_deref(), &receipt.after_hash, receipt.bytes)?;
    receipt.outcome = match publication.outcome {
        PublicationOutcome::Applied => Outcome::Applied,
        PublicationOutcome::NotApplied => Outcome::Refused,
        PublicationOutcome::Uncertain => Outcome::Uncertain,
    };
    receipt.observed_hash = if receipt.outcome == Outcome::Applied {
        Some(receipt.after_hash.clone())
    } else {
        optional_hash(&entry.path)?
    };
    receipt.error = publication.error.clone();
    if schema == 3 { receipt.publication = Some(publication); }
    #[cfg(test)]
    interrupt_at(Interruption::Published);
    save_receipt(receipt, directory, index)
}

fn save_receipt(receipt: Receipt, directory: &Path, index: usize) -> Result<Receipt, String> {
    install_effect_record(
        &directory.join(format!("{index}.receipt.json")),
        &serde_json::to_vec(&receipt).map_err(|error| error.to_string())?,
    )?;
    Ok(receipt)
}
