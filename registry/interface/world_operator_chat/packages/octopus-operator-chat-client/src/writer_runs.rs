use super::*;
use octopus_provider_run_authoring::{
    CheckedRunWriter, WriterRunBinding, separate_owner_writer_run, separate_writer_run,
};

pub(super) fn wire(
    request: &OperatorChatPlanRequest,
    identity: &PlannedIdentity,
    content: String,
) -> Result<(String, Vec<PlannedClientWrite>), String> {
    if identity.voice.is_none() {
        return Ok((content, Vec::new()));
    }
    wire_record(
        request,
        &identity.run_id,
        request.existing_run_record.as_ref(),
        content,
    )
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
            .ok_or("Home writer Runs require their declared run_records_root")?,
    )?;
    let mut record: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| e.to_string())?;
    let owners = record["interface_state"]
        .as_object()
        .ok_or("Home state absent")?
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
                        "Home writer Run collides with an independently authored record".into(),
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && prior.is_none() => writes
                .push(PlannedClientWrite {
                    path,
                    content: pretty_json(&initial)?,
                    tag: format!("run:{id}"),
                    expected_before_hash: None,
                }),
            Err(error) => {
                return Err(format!(
                    "Home writer Run '{}' cannot be preserved: {error}",
                    path.display()
                ));
            }
        }
    }
    Ok((pretty_json(&record)?, writes))
}
