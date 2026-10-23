use std::{sync::Arc, time::{Duration, SystemTime, UNIX_EPOCH}};
use octopus_interfaces::InterfaceArtifactFile;
use octopus_ledger::ArtifactRef;
use octopus_tool_door_runtime::{InterfaceLiftContext, LedgerTailReader, ToolDoorOutputCallback,
    ToolDoorOutputDispatch, ToolDoorOutputFuture, ToolDoorOutputHandler, ToolDoorOutputSource};
use octopus_tool_handlers::{ToolCallArtifact, ToolResultArtifact};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    schema: String,
    schema_version: u32,
    tool_call: ArtifactRef,
    request_artifact: String,
    request_id: String,
    admission: Value,
}

fn pending(source: &ToolDoorOutputSource, result: &ToolResultArtifact) -> Result<Option<Pending>, String> {
    let Some(body) = result.body.as_ref() else { return Ok(None); };
    let value = &body["result"]["structuredContent"];
    if value["schema"] != "octopus.client_action_pending" { return Ok(None); }
    let pending: Pending = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    let call: ToolCallArtifact = serde_json::from_slice(&source.envelope.read_artifact_payload()
        .map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    if pending.schema != "octopus.client_action_pending" || pending.schema_version != 1
        || pending.tool_call != source.envelope.artifact || pending.request_id.is_empty()
        || pending.admission.get("admitted").is_none()
        || result.tool != call.tool || result.correlation_id != call.correlation_id
        || call.arguments.pointer("/params/arguments/action/request_id") != Some(&json!(pending.request_id))
        || !pending.request_artifact.strip_prefix("client.action:").is_some_and(|hash|
            hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit())) {
        return Err("Client control wait does not match its admitted tool call".into());
    }
    Ok(Some(pending))
}

fn matching_result(pending: &Pending, value: &Value) -> Result<bool, String> {
    if value.get("tool_call") != Some(&json!(pending.tool_call)) { return Ok(false); }
    if value["schema"] != "octopus.client_action_result" || value["schema_version"] != 1
        || value["request_artifact"] != pending.request_artifact || value["request_id"] != pending.request_id
        || value["phase"] != "completed" || !value["ok"].is_boolean()
        || (value["ok"] == true && !value["widgets"].is_array() && !value["client"].is_object() && !value["relaunch"].is_object())
        || (value["ok"] == false && !value["error"].is_string()) {
        return Err("Client result conflicts with its exact pending request".into());
    }
    Ok(true)
}

struct ClientResult;

impl ToolDoorOutputHandler for ClientResult {
    fn output(&self, ctx: InterfaceLiftContext, source: ToolDoorOutputSource,
        artifact: InterfaceArtifactFile, dispatch: ToolDoorOutputDispatch) -> ToolDoorOutputFuture {
        Box::pin(async move {
            let limits = ctx.owner_io_limits()?;
            let captured = artifact.read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes).map_err(|e| e.to_string())?;
            if !captured.manifest.label.ends_with(".result") { return dispatch(ctx, artifact).await; }
            let mut result: ToolResultArtifact = serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())?;
            let Some(wait) = pending(&source, &result)? else { return dispatch(ctx, artifact).await; };
            let mut reader = LedgerTailReader::open_named(&ctx, &format!("client-result-{}", source.movement.seq.0))?;
            let cancel = ctx.owner_cancel();
            let expires_at_ms = source.movement.recorded_at_unix_ms.checked_add(60_000).ok_or("Client control deadline overflow")?;
            let run = source.envelope.runtime().map_err(|e| e.to_string())?.run.clone();
            let value = loop {
                let rows = reader.read_new_uncommitted_limit(256)?;
                let full = rows.len() == 256;
                let mut found = None;
                for own in rows {
                    if own.movement.seq.0 > source.movement.seq.0 {
                        let envelope = reader.envelope(&own)?;
                        if envelope.read_artifact_manifest().map_err(|e| e.to_string())?.label == "client.action.result"
                            && envelope.runtime().map_err(|e| e.to_string())?.run == run {
                            let bytes = envelope.read_artifact_payload_bounded(limits.max_payload_bytes).map_err(|e| e.to_string())?;
                            let candidate: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                            if matching_result(&wait, &candidate)? { found = Some(candidate); break; }
                        }
                    }
                    reader.commit_through(&own)?;
                }
                if let Some(value) = found { break value; }
                if full { continue; }
                let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e| e.to_string())?.as_millis() as u64;
                if now >= expires_at_ms {
                    break json!({"schema":"octopus.client_action_result","schema_version":1,"ok":false,
                        "request_id":wait.request_id,"request_artifact":wait.request_artifact,
                        "error":"The client did not return this control's result before its deadline. Its effect is unconfirmed. Inspect current state before requesting another change."});
                }
                tokio::select! {
                    result = reader.wait_for_change(&cancel) => result?,
                    _ = tokio::time::sleep(Duration::from_millis(expires_at_ms - now)) => {},
                }
                if cancel.is_cancelled() { return Err("Client control wait interrupted by interface shutdown".into()); }
            };
            result.body = Some(json!({"jsonrpc":"2.0","id":1,"result":{
                "content":[{"type":"text","text":value.to_string()}],"structuredContent":value,"isError":value["ok"] != true}}));
            let output = InterfaceArtifactFile::write(ctx.interface_var_dir().join("client-results")
                .join(source.movement.seq.0.to_string()), &result.label, &serde_json::to_vec(&result).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            dispatch(ctx, output).await
        })
    }

    fn can_resume_unsubmitted(&self, ctx: &InterfaceLiftContext, source: &ToolDoorOutputSource,
        artifact: &InterfaceArtifactFile) -> Result<bool, String> {
        let limits = ctx.owner_io_limits()?;
        let captured = artifact.read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes).map_err(|e| e.to_string())?;
        if !captured.manifest.label.ends_with(".result") { return Ok(false); }
        let result = serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())?;
        Ok(pending(source, &result)?.is_some())
    }
}

pub fn client_result_callback() -> ToolDoorOutputCallback { Arc::new(ClientResult) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn client_receipt_must_match_the_tool_call_and_exact_request() {
        let wait = Pending { schema: "octopus.client_action_pending".into(), schema_version: 1,
            tool_call: ArtifactRef { id: "tool.call:source".into(), content: octopus_ledger::ContentHash::of_bytes(b"source") },
            request_artifact: "client.action:request".into(), request_id: "request".into(), admission: json!({"admitted":{"seq":1}}) };
        let value = json!({"schema":"octopus.client_action_result","schema_version":1,"tool_call":wait.tool_call,
            "request_artifact":wait.request_artifact,"request_id":wait.request_id,"phase":"completed","ok":true,"widgets":[]});
        assert!(matching_result(&wait, &value).unwrap());
        let mut other = value.clone(); other["tool_call"]["id"] = json!("other");
        assert!(!matching_result(&wait, &other).unwrap());
        for field in ["request_artifact", "request_id", "phase", "ok", "widgets"] {
            let mut invalid = value.clone(); invalid[field] = Value::Null;
            assert!(matching_result(&wait, &invalid).is_err(), "{field}");
        }
        let mut failed = value; failed["ok"] = json!(false); failed["error"] = json!("stopped before execution");
        failed.as_object_mut().unwrap().remove("widgets");
        assert!(matching_result(&wait, &failed).unwrap());
    }
}
