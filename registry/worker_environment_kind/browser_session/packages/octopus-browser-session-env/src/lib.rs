use octopus_interfaces::{
    InterfaceArtifactFile, InterfaceBoundaryError, InterfaceInvocationEnvelope,
};
use octopus_tool_door_runtime::{ToolProcessEnvironment, ToolProcessError, ToolProcessInvocation};
use octopus_tool_handlers::{ToolCall, ToolCallArtifact, ToolResultArtifact, ToolResultError};
use octopus_worker_env::WorkerEnvironmentFormation;
use serde_json::Value;

mod browser_session;
mod browser_consent;
pub(crate) use browser_session::history as browser_history;
mod browser_wait_completion;
pub use browser_wait_completion::run_browser_wait_completion_tool_process;
mod native_transport;
pub(crate) use octopus_private_fs as private_fs;
pub use native_transport::prepare_private_native_host;
pub use browser_session::decode_filechooser_door_artifact;
#[cfg(test)]
mod tests;

use browser_session::BrowserRoom;

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
    operation: BrowserOperation,
    correlation_id: Option<String>,
    source_movement_seq: u64,
    effective_allow_network: bool,
    descriptor_bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BrowserOperation {
    Navigate,
    Snapshot,
    Click,
    Type,
    Key,
    Scroll,
    Back,
    Forward,
    Reload,
    ViewportSet,
    ViewportReset,
    Eval,
    EvaluateReadonly,
    LocatorQuery,
    LocatorFill,
    LocatorPress,
    LocatorSelect,
    LocatorCheck,
    LocatorWait,
    DomSnapshot,
    PlaywrightDomSnapshot,
    DomClick,
    DomScroll,
    PointerMove,
    Paste,
    ClipboardRead,
    ClipboardWrite,
    MediaDownload,
    VisibilityGet,
    VisibilitySet,
    Hover,
    Select,
    Drag,
    Wait,
    WaitComplete,
    History,
    HistoryResume,
    Images,
    Console,
    Dialog,
    Screenshot,
    DownloadArm,
    DownloadWait,
    DownloadCancel,
    FilechooserArm,
    FilechooserWait,
    FilechooserSetFiles,
    FilechooserCancel,
    FilechooserResume,
    Pdf,
    Cookies,
    CookieSet,
    CookieDelete,
    Tabs,
    TabNew,
    TabSelect,
    TabMark,
    TabClose,
    Close,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserHandlerConfig {
    operation: BrowserOperation,
}

fn browser_result(
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

pub async fn run_browser_session_tool_process() -> Result<(), String> {
    let environment = ToolProcessEnvironment::from_env()?;
    let room = std::sync::Arc::new(BrowserRoom::load(environment)?);
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
        source_movement_seq,
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
    let operation = browser_operation(&descriptor.handler_config, descriptor.id.as_str())?;
    let artifact: ToolCallArtifact = if operation == BrowserOperation::FilechooserResume {
        browser_session::decoded_response(&envelope, descriptor.id.as_str()).map_err(capability)?
    } else if operation == BrowserOperation::HistoryResume {
        browser_history::decoded_response(&envelope, descriptor.id.as_str()).map_err(capability)?
    } else {
        if manifest.label != NEUTRAL_TOOL_CALL_LABEL {
            return Err(capability(format!("browser door manifest label '{}' is not '{}'", manifest.label, NEUTRAL_TOOL_CALL_LABEL)));
        }
        serde_json::from_slice(&payload).map_err(|error| capability(format!("tool.call artifact is not a strict browser-door JSON object: {error}")))?
    };
    let tool = admitted_tool_call_tool(&envelope, &artifact)
        .map_err(|error| capability(error.to_string()))?;
    if descriptor.id.as_str() != tool {
        return Err(capability(format!(
            "tool descriptor '{}' does not match tool.call '{}'",
            descriptor.id, tool
        )));
    }
    let descriptor_bytes = octopus_descriptors::canonical_bytes(&serde_json::to_value(&descriptor).map_err(|error| capability(error.to_string()))?);
    let descriptor_hash = blake3::hash(&descriptor_bytes).to_hex().to_string();
    if formation.tool_descriptor_hashes.get(descriptor.id.as_str()).map(ToString::to_string).as_deref() != Some(descriptor_hash.as_str()) {
        return Err(capability("Original broker tool descriptor differs from the exact formed pin"));
    }
    let effective_allow_network = descriptor.process.authority.allow_network
        && formation.network_ceiling != octopus_worker_env::NetworkCeiling::Denied;
    Ok(DecodedRoomInvocation {
        call: ToolCall {
            tool_id: tool,
            arguments: artifact.arguments,
            handler_config: descriptor.handler_config.clone(),
        },
        operation,
        correlation_id: artifact.correlation_id,
        envelope,
        formation,
        source_movement_seq,
        effective_allow_network,
        descriptor_bytes,
    })
}

fn browser_operation(
    handler_config: &Value,
    tool: &str,
) -> Result<BrowserOperation, ToolProcessError> {
    serde_json::from_value::<BrowserHandlerConfig>(handler_config.clone())
        .map(|config| config.operation)
        .map_err(|error| {
            capability(format!(
                "tool '{tool}' has malformed browser handler_config: {error}"
            ))
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
