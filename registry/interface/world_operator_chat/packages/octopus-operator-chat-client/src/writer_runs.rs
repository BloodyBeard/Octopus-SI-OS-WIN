use octopus_provider_run_authoring::JsonFormat;
use super::*;
use octopus_provider_run_authoring::{
    CheckedRunWriter, WriterRunBinding, separate_owner_writer_run, separate_writer_run,
};

mod schema;
pub use schema::{OperatorWriterSchemaPlanRequest, plan_operator_writer_schema};

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorWriterRunPlanRequest {
    pub run_records_root: PathBuf,
    pub run_id: String,
    pub expected_before_sha256: String,
}

pub fn plan_operator_writer_runs(
    request: &OperatorWriterRunPlanRequest,
) -> Result<Vec<PlannedClientWrite>, String> {
    let root = require_absolute("run_records_root", &request.run_records_root)?;
    safe_segment(&request.run_id, "run_id")?;
    let path = root.join(format!("{}.json", request.run_id));
    let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if !metadata.file_type().is_file() {
        return Err("writer target must be a regular Run record".into());
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    if sha256_hex(&bytes) != request.expected_before_sha256 {
        return Err("writer target changed before planning".into());
    }
    let original = RunRecord::from_bytes(&bytes).map_err(|e| e.to_string())?;
    if original.id.as_str() != request.run_id {
        return Err("writer target identity does not match its path".into());
    }
    let existing: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let (content, mut writes) = wire_record_at_root(
        root, &request.run_id, Some(&existing), pretty_json(&existing)?, false,
    )?;
    if serde_json::from_str::<serde_json::Value>(&content).map_err(|e| e.to_string())? != existing {
        writes.push(PlannedClientWrite {
            path, content, tag: format!("run:{}", request.run_id),
            expected_before_hash: Some(request.expected_before_sha256.clone()),
        });
    }
    Ok(writes)
}

pub(super) fn wire(
    request: &OperatorChatPlanRequest,
    identity: &PlannedIdentity,
    content: String,
) -> Result<(String, Vec<PlannedClientWrite>), String> {
    wire_record(
        request,
        &identity.run_id,
        request.existing_run_record.as_ref(),
        content,
    )
}

pub(super) fn wire_preserving_existing(
    request: &OperatorChatPlanRequest,
    identity: &PlannedIdentity,
    content: String,
) -> Result<(String, Vec<PlannedClientWrite>), String> {
    let root = require_absolute("run_records_root",request.run_records_root.as_deref().ok_or("client writer Runs require their declared run_records_root")?)?;
    wire_record_at_root(root,&identity.run_id,request.existing_run_record.as_ref(),content,true)
}

pub(super) fn wire_record(
    request: &OperatorChatPlanRequest,
    run_id: &str,
    existing: Option<&serde_json::Value>,
    content: String,
) -> Result<(String, Vec<PlannedClientWrite>), String> {
    let root = require_absolute(
        "run_records_root",
        request
            .run_records_root
            .as_deref()
            .ok_or("client writer Runs require their declared run_records_root")?,
    )?;
    wire_record_at_root(root, run_id, existing, content, false)
}

pub(super) fn wire_new_record(
    root: &Path,
    run_id: &str,
    content: String,
) -> Result<(String, Vec<PlannedClientWrite>), String> {
    wire_record_at_root(require_absolute("run_records_root", root)?, run_id, None, content, false)
}

fn wire_record_at_root(
    root: &Path,
    run_id: &str,
    existing: Option<&serde_json::Value>,
    content: String,
    preserve_existing: bool,
) -> Result<(String, Vec<PlannedClientWrite>), String> {
    let mut record: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| e.to_string())?;
    let owners = record["interface_state"]
        .as_object()
        .ok_or("client Run state absent")?
        .iter()
        .filter(|(_, state)| {
            state.get("run_authoring_binding").is_some() || state.get("input_intake").is_some()
        })
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    let mut writes = Vec::new();
    for owner in owners {
        let provider = record["interface_state"][&owner]
            .get("run_authoring_binding")
            .is_some();
        let prior: Option<WriterRunBinding> = existing
            .and_then(|r| {
                if provider {
                    r["interface_state"][&owner]["run_authoring_binding"].get("writer_run")
                } else {
                    r["interface_state"][&owner].get("writer_run")
                }
            })
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| e.to_string())?;
        if preserve_existing && prior.is_none()
            && existing.and_then(|record|record.get("interface_state")).and_then(|states|states.get(&owner)).is_some() {
            continue;
        }
        let id = prior.as_ref().map(|w| w.run.clone()).unwrap_or_else(|| {
            let hash = blake3::hash(format!("{run_id}:{owner}").as_bytes()).to_hex();
            octopus_descriptors::RunId::new(format!("run_writer_{}", &hash[..24]))
        });
        safe_segment(id.as_str(), "writer Run")?;
        if provider
            && record["interface_state"][&owner]["run_authoring_binding"]
                .get("writer_run")
                .is_none()
        {
            if let Some(prior) = &prior {
                record["interface_state"][&owner]["run_authoring_binding"]["writer_run"] =
                    serde_json::to_value(prior).map_err(|e| e.to_string())?;
            }
        }
        let initial = if provider {
            separate_writer_run(&mut record, &InterfaceId::new(&owner), &id)?
        } else {
            let writer: CheckedRunWriter = serde_json::from_value(
                record["interface_state"][&owner]["input_intake"]["writer"].clone(),
            )
            .map_err(|e| e.to_string())?;
            separate_owner_writer_run(&mut record, &writer, &id)?
        };
        let path = root.join(format!("{id}.json"));
        match std::fs::read(&path) {
            Ok(bytes) => {
                let actual = RunRecord::from_bytes(&bytes).map_err(|e| e.to_string())?;
                let expected = RunRecord::from_bytes(pretty_json(&initial)?.as_bytes())
                    .map_err(|e| e.to_string())?;
                if actual.id != expected.id
                    || actual.ee != expected.ee
                    || actual.worker != expected.worker
                    || (prior.is_none()
                        && serde_json::from_slice::<serde_json::Value>(&bytes)
                            .map_err(|e| e.to_string())?
                            != initial)
                {
                    return Err(
                        "client writer Run collides with an independently authored record".into(),
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && prior.is_none() => writes
                .push(PlannedClientWrite {
                    path,
                    content: String::from_utf8(JsonFormat::Compact.encode(&initial)?).map_err(|error| error.to_string())?,
                    tag: format!("run:{id}"),
                    expected_before_hash: None,
                }),
            Err(error) => {
                return Err(format!(
                    "client writer Run '{}' cannot be preserved: {error}",
                    path.display()
                ));
            }
        }
    }
    Ok((String::from_utf8(JsonFormat::Compact.encode(&record)?).map_err(|error| error.to_string())?, writes))
}
