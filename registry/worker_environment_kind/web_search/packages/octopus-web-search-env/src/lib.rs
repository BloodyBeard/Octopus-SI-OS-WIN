use octopus_interfaces::{
    InterfaceArtifactFile, InterfaceBoundaryError, InterfaceInvocationEnvelope,
};
use octopus_tool_door_runtime::{ToolProcessEnvironment, ToolProcessError, ToolProcessInvocation};
use octopus_tool_handlers::{ToolCall, ToolCallArtifact, ToolResultArtifact, ToolResultError};
use octopus_worker_env::WorkerEnvironmentFormation;
use serde_json::Value;

#[cfg(test)]
mod tests;
mod web_search;

use web_search::WebSearchRoom;

const NEUTRAL_TOOL_CALL_LABEL: &str = "tool.call";

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct BrowserInvocationScope {
    run: String,
    ee: String,
}

struct DecodedRoomInvocation {
    envelope: InterfaceInvocationEnvelope,
    formation: WorkerEnvironmentFormation,
    call: ToolCall,
    correlation_id: Option<String>,
}

fn web_search_result(
    tool: &str,
    correlation_id: Option<String>,
    result: Result<Value, String>,
    evidence_write_errors: Vec<Value>,
) -> ToolResultArtifact {
    match result {
        Ok(body) if evidence_write_errors.is_empty() => {
            ToolResultArtifact::success(tool, correlation_id, body)
        }
        Ok(body) => {
            let mut details = serde_json::Map::new();
            details.insert("operation_body".to_string(), body);
            details.insert(
                "evidence_write_errors".to_string(),
                Value::Array(evidence_write_errors),
            );
            ToolResultArtifact::failure(
                tool,
                correlation_id,
                ToolResultError::new(
                    "worker_environment_evidence_failed",
                    format!(
                        "{tool} completed but its worker-environment evidence could not be fully written"
                    ),
                )
                .with_details(details),
            )
        }
        Err(error) => {
            let mut failure = ToolResultError::new("tool_execution_failed", error);
            if !evidence_write_errors.is_empty() {
                let mut details = serde_json::Map::new();
                details.insert(
                    "evidence_write_errors".to_string(),
                    Value::Array(evidence_write_errors),
                );
                failure = failure.with_details(details);
            }
            ToolResultArtifact::failure(tool, correlation_id, failure)
        }
    }
}

fn evidence_write_error(stage: &str, error: impl Into<String>) -> Value {
    serde_json::json!({
        "stage": stage,
        "message": error.into(),
    })
}

pub async fn run_web_search_tool_process() -> Result<(), String> {
    let environment = ToolProcessEnvironment::from_env()?;
    let room = std::sync::Arc::new(WebSearchRoom::new(environment));
    octopus_tool_door_runtime::run_tool_process(room).await
}

fn decode_room_invocation(
    environment: &ToolProcessEnvironment,
    invocation: ToolProcessInvocation,
    substrate_family: &str,
) -> Result<DecodedRoomInvocation, ToolProcessError> {
    let ToolProcessInvocation {
        descriptor,
        envelope,
        worker_environment,
        worker_environment_kind,
        ..
    } = invocation;
    if descriptor.interface != environment.interface {
        return Err(capability(format!(
            "tool descriptor '{}' belongs to interface '{}', not browser door '{}'",
            descriptor.id, descriptor.interface, environment.interface
        )));
    }
    let formation = worker_environment.ok_or_else(|| {
        capability(format!(
            "tool '{}' invocation has no formed worker environment",
            descriptor.id
        ))
    })?;
    if formation.door_interface != environment.interface {
        return Err(capability(format!(
            "tool '{}' formation belongs to door '{}', not '{}'",
            descriptor.id, formation.door_interface, environment.interface
        )));
    }
    let kind = worker_environment_kind.ok_or_else(|| {
        capability(format!(
            "tool '{}' invocation has no formed kind descriptor",
            descriptor.id
        ))
    })?;
    if kind.worker_environment_kind_id != formation.worker_environment_kind
        || kind.substrate_family != substrate_family
    {
        return Err(capability(format!(
            "tool '{}' was formed for incompatible worker_environment_kind '{}'",
            descriptor.id, formation.worker_environment_kind
        )));
    }
    if !descriptor
        .supported_worker_environment_kinds
        .iter()
        .any(|candidate| candidate == &kind.worker_environment_kind_id)
        || !formation
            .granted_tools
            .iter()
            .any(|tool| tool == descriptor.id.as_str())
        || !formation
            .tool_descriptor_hashes
            .contains_key(descriptor.id.as_str())
    {
        return Err(capability(format!(
            "tool '{}' is not part of the exact formed room",
            descriptor.id
        )));
    }
    let manifest = envelope
        .read_artifact_manifest()
        .map_err(|error| capability(error.to_string()))?;
    let payload = envelope
        .read_artifact_payload()
        .map_err(|error| capability(error.to_string()))?;
    let scope =
        invocation_scope_from_input(&envelope).map_err(|error| capability(error.to_string()))?;
    if scope.run != formation.run || scope.ee != formation.ee {
        return Err(capability(format!(
            "tool '{}' formation does not match invocation coordinates",
            descriptor.id
        )));
    }
    if manifest.label != NEUTRAL_TOOL_CALL_LABEL {
        return Err(capability(format!(
            "browser door manifest label '{}' is not '{}'",
            manifest.label, NEUTRAL_TOOL_CALL_LABEL
        )));
    }
    let artifact: ToolCallArtifact = serde_json::from_slice(&payload).map_err(|error| {
        capability(format!(
            "tool.call artifact is not a strict browser-door JSON object: {error}"
        ))
    })?;
    let tool = admitted_tool_call_tool(&envelope, &artifact)
        .map_err(|error| capability(error.to_string()))?;
    if descriptor.id.as_str() != tool {
        return Err(capability(format!(
            "tool descriptor '{}' does not match tool.call '{}'",
            descriptor.id, tool
        )));
    }
    Ok(DecodedRoomInvocation {
        call: ToolCall {
            tool_id: tool,
            arguments: artifact.arguments,
            handler_config: descriptor.handler_config.clone(),
        },
        correlation_id: artifact.correlation_id,
        envelope,
        formation,
    })
}

fn invocation_scope_from_input(
    input: &InterfaceInvocationEnvelope,
) -> Result<BrowserInvocationScope, InterfaceBoundaryError> {
    let runtime = input
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.runtime.as_ref())
        .ok_or_else(|| {
            InterfaceBoundaryError::Protocol(
                "browser door requires runtime metadata beside the artifact".into(),
            )
        })?;
    Ok(BrowserInvocationScope {
        run: runtime.run.as_str().to_string(),
        ee: runtime.ee.as_str().to_string(),
    })
}

fn admitted_tool_call_tool(
    input: &InterfaceInvocationEnvelope,
    call: &ToolCallArtifact,
) -> Result<String, InterfaceBoundaryError> {
    let tool = call.tool.trim();
    if tool.is_empty() {
        return Err(InterfaceBoundaryError::Protocol(
            "browser door tool.call has empty tool name".into(),
        ));
    }
    if !input
        .granted_tool_names()?
        .iter()
        .any(|grant| grant == tool)
    {
        return Err(InterfaceBoundaryError::Protocol(format!(
            "browser door denied tool.call '{}' because formed metadata did not grant it",
            tool
        )));
    }
    if input
        .hard_blocked_tool_names()?
        .iter()
        .any(|blocked| blocked == tool)
    {
        return Err(InterfaceBoundaryError::Protocol(format!(
            "browser door denied hard-blocked tool.call '{}'",
            tool
        )));
    }
    Ok(tool.to_string())
}

fn response_artifact(
    envelope: &InterfaceInvocationEnvelope,
    response: ToolResultArtifact,
) -> Result<InterfaceArtifactFile, ToolProcessError> {
    let bytes = serde_json::to_vec(&response).map_err(|error| {
        capability(format!(
            "browser door response artifact did not serialize: {error}"
        ))
    })?;
    let parent = envelope
        .artifact_file
        .manifest_path()
        .parent()
        .ok_or_else(|| {
            capability(format!(
                "browser door inbound artifact manifest '{}' has no parent directory",
                envelope.artifact_file.manifest_path().display()
            ))
        })?;
    InterfaceArtifactFile::write(parent.join("emitted"), &response.label, &bytes)
        .map_err(|error| capability(error.to_string()))
}

fn capability(error: impl Into<String>) -> ToolProcessError {
    ToolProcessError::Capability(error.into())
}
