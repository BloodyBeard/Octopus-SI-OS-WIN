use std::sync::Arc;

use octopus_interfaces::{
    InterfaceArtifactFile, InterfaceBoundaryError as InterfaceDriverError,
    InterfaceInvocationEnvelope,
};
use octopus_process_session_tool_runtime::ToolProcessEnvironment;
use octopus_tool_handlers::ToolCallArtifact;
mod process_room;
pub use process_room::ProcessRoom;

const NEUTRAL_TOOL_CALL_LABEL: &str = "tool.call";

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct ExecInvocationScope {
    run: String,
    ee: String,
}

#[derive(Clone, Debug)]
pub(crate) struct DecodedExecEnvRequest {
    scope: ExecInvocationScope,
    tool: String,
    arguments: serde_json::Value,
    correlation_id: Option<String>,
}

pub async fn run_process_room_tool_process() -> Result<(), String> {
    let env = ToolProcessEnvironment::from_env()?;
    let process = Arc::new(ProcessRoom::new(env));
    octopus_process_session_tool_runtime::run_tool_process(process).await
}

fn invocation_scope_from_input(
    input: &InterfaceInvocationEnvelope,
) -> Result<ExecInvocationScope, InterfaceDriverError> {
    let runtime = input
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.runtime.as_ref())
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(
                "system_exec_env requires runtime metadata beside the artifact".into(),
            )
        })?;
    Ok(ExecInvocationScope {
        run: runtime.run.as_str().to_string(),
        ee: runtime.ee.as_str().to_string(),
    })
}

fn decode_exec_env_request(
    manifest_label: &str,
    payload: &[u8],
    input: &InterfaceInvocationEnvelope,
) -> Result<DecodedExecEnvRequest, InterfaceDriverError> {
    if manifest_label != NEUTRAL_TOOL_CALL_LABEL {
        return Err(InterfaceDriverError::Protocol(format!(
            "system_exec_env manifest label '{}' is not '{}'",
            manifest_label, NEUTRAL_TOOL_CALL_LABEL
        )));
    }
    let artifact: ToolCallArtifact = serde_json::from_slice(payload).map_err(|error| {
        InterfaceDriverError::Protocol(format!(
            "tool.call artifact is not a neutral tool-call JSON object: {error}"
        ))
    })?;
    let tool_id = admitted_tool_call_tool(input, &artifact)?;
    Ok(DecodedExecEnvRequest {
        scope: invocation_scope_from_input(input)?,
        tool: tool_id,
        arguments: artifact.arguments,
        correlation_id: artifact.correlation_id,
    })
}

fn admitted_tool_call_tool(
    input: &InterfaceInvocationEnvelope,
    call: &ToolCallArtifact,
) -> Result<String, InterfaceDriverError> {
    let tool = call.tool.trim();
    if tool.is_empty() {
        return Err(InterfaceDriverError::Protocol(
            "system_exec_env tool.call has empty tool name".into(),
        ));
    }
    let granted = input.granted_tool_names()?;
    if !granted.iter().any(|grant| grant == tool) {
        return Err(InterfaceDriverError::Protocol(format!(
            "system_exec_env denied tool.call '{}' because formed metadata did not grant it",
            tool
        )));
    }
    let hard_blocked = input.hard_blocked_tool_names()?;
    if hard_blocked.iter().any(|blocked| blocked == tool) {
        return Err(InterfaceDriverError::Protocol(format!(
            "system_exec_env denied hard-blocked tool.call '{}'",
            tool
        )));
    }
    Ok(tool.to_string())
}

fn materialize_system_exec_emitted_artifact(
    input: &InterfaceInvocationEnvelope,
    label: &str,
    payload_bytes: &[u8],
) -> Result<InterfaceArtifactFile, InterfaceDriverError> {
    let parent = input
        .artifact_file
        .manifest_path()
        .parent()
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(format!(
                "system_exec_env inbound artifact manifest '{}' has no parent directory",
                input.artifact_file.manifest_path().display()
            ))
        })?;
    InterfaceArtifactFile::write(parent.join("emitted"), label, payload_bytes)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))
}
