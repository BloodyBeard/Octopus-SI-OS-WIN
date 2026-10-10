#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::sync::Arc;

use octopus_worker_env::{ArtifactSurface, ExecSurface, FileSurface, ProjectedSandbox};
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, Serialize)]
pub struct ToolCallArtifact {
    pub tool: String,
    pub arguments: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(flatten)]
    pub source_interface_evidence: BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct ToolCallArtifactWire {
    tool: String,
    arguments: serde_json::Value,
    #[serde(default)]
    correlation_id: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(flatten)]
    source_interface_evidence: BTreeMap<String, serde_json::Value>,
}

impl TryFrom<ToolCallArtifactWire> for ToolCallArtifact {
    type Error = String;

    fn try_from(wire: ToolCallArtifactWire) -> Result<Self, Self::Error> {
        if wire.tool.is_empty() || wire.tool.trim() != wire.tool {
            return Err("tool.call carries an empty or untrimmed tool id".to_string());
        }
        if !wire.arguments.is_object() {
            return Err("tool.call arguments must be a JSON object".to_string());
        }
        if wire
            .correlation_id
            .as_deref()
            .is_some_and(|value| value.is_empty() || value.trim() != value)
        {
            return Err("tool.call carries an empty or untrimmed correlation_id".to_string());
        }
        if wire
            .label
            .as_deref()
            .is_some_and(|label| label != "tool.call")
        {
            return Err("tool.call payload label does not match its manifest".to_string());
        }
        Ok(Self {
            tool: wire.tool,
            arguments: wire.arguments,
            correlation_id: wire.correlation_id,
            label: wire.label,
            source_interface_evidence: wire.source_interface_evidence,
        })
    }
}

impl<'de> Deserialize<'de> for ToolCallArtifact {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        ToolCallArtifactWire::deserialize(deserializer)?
            .try_into()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolResultError {
    pub code: String,
    pub message: String,
    pub details: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Serialize)]
struct ToolResultErrorRef<'a> {
    code: &'a str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<&'a serde_json::Map<String, serde_json::Value>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolResultErrorWire {
    code: String,
    message: String,
    #[serde(default, deserialize_with = "deserialize_present")]
    details: Option<serde_json::Map<String, serde_json::Value>>,
}

impl TryFrom<ToolResultErrorWire> for ToolResultError {
    type Error = String;

    fn try_from(wire: ToolResultErrorWire) -> Result<Self, Self::Error> {
        let result = Self {
            code: wire.code,
            message: wire.message,
            details: wire.details,
        };
        result.validate()?;
        Ok(result)
    }
}

impl<'de> Deserialize<'de> for ToolResultError {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        ToolResultErrorWire::deserialize(deserializer)?
            .try_into()
            .map_err(serde::de::Error::custom)
    }
}

impl Serialize for ToolResultError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.validate()
            .map_err(<S::Error as serde::ser::Error>::custom)?;
        ToolResultErrorRef {
            code: &self.code,
            message: &self.message,
            details: self.details.as_ref(),
        }
        .serialize(serializer)
    }
}

impl ToolResultError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: serde_json::Map<String, serde_json::Value>) -> Self {
        self.details = Some(details);
        self
    }

    pub fn validate(&self) -> Result<(), String> {
        for (field, value) in [
            ("code", self.code.as_str()),
            ("message", self.message.as_str()),
        ] {
            if value.is_empty() || value.trim() != value {
                return Err(format!(
                    "tool result error {field} must be non-empty and trimmed"
                ));
            }
        }
        if self.details.as_ref().is_some_and(serde_json::Map::is_empty) {
            return Err("tool result error details must be absent or non-empty".to_string());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolResultArtifact {
    pub label: String,
    pub tool: String,
    pub correlation_id: Option<String>,
    pub ok: bool,
    pub body: Option<serde_json::Value>,
    pub error: Option<ToolResultError>,
}

#[derive(Serialize)]
struct ToolResultArtifactRef<'a> {
    label: &'a str,
    tool: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    correlation_id: Option<&'a str>,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<&'a serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a ToolResultError>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolResultArtifactWire {
    label: String,
    tool: String,
    #[serde(default, deserialize_with = "deserialize_present")]
    correlation_id: Option<String>,
    ok: bool,
    #[serde(default, deserialize_with = "deserialize_present")]
    body: Option<serde_json::Value>,
    #[serde(default, deserialize_with = "deserialize_present")]
    error: Option<ToolResultError>,
}

fn deserialize_present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

impl TryFrom<ToolResultArtifactWire> for ToolResultArtifact {
    type Error = String;

    fn try_from(wire: ToolResultArtifactWire) -> Result<Self, Self::Error> {
        let result = Self {
            label: wire.label,
            tool: wire.tool,
            correlation_id: wire.correlation_id,
            ok: wire.ok,
            body: wire.body,
            error: wire.error,
        };
        result.validate()?;
        Ok(result)
    }
}

impl<'de> Deserialize<'de> for ToolResultArtifact {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        ToolResultArtifactWire::deserialize(deserializer)?
            .try_into()
            .map_err(serde::de::Error::custom)
    }
}

impl Serialize for ToolResultArtifact {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.validate()
            .map_err(<S::Error as serde::ser::Error>::custom)?;
        ToolResultArtifactRef {
            label: &self.label,
            tool: &self.tool,
            correlation_id: self.correlation_id.as_deref(),
            ok: self.ok,
            body: self.body.as_ref(),
            error: self.error.as_ref(),
        }
        .serialize(serializer)
    }
}

impl ToolResultArtifact {
    pub fn success(
        tool: impl Into<String>,
        correlation_id: Option<String>,
        body: serde_json::Value,
    ) -> Self {
        let tool = tool.into();
        Self {
            label: format!("{tool}.result"),
            tool,
            correlation_id,
            ok: true,
            body: Some(body),
            error: None,
        }
    }

    pub fn failure(
        tool: impl Into<String>,
        correlation_id: Option<String>,
        error: ToolResultError,
    ) -> Self {
        let tool = tool.into();
        Self {
            label: format!("{tool}.result"),
            tool,
            correlation_id,
            ok: false,
            body: None,
            error: Some(error),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.tool.is_empty() || self.tool.trim() != self.tool {
            return Err("tool result artifact has an empty or untrimmed tool id".to_string());
        }
        if self.label != format!("{}.result", self.tool) {
            return Err(format!(
                "tool result artifact label '{}' does not match tool '{}'",
                self.label, self.tool
            ));
        }
        if self
            .correlation_id
            .as_deref()
            .is_some_and(|value| value.is_empty() || value.trim() != value)
        {
            return Err(
                "tool result artifact has an empty or untrimmed correlation_id".to_string(),
            );
        }
        match (self.ok, self.body.as_ref(), self.error.as_ref()) {
            (true, Some(body), None) if !body.is_null() => Ok(()),
            (false, None, Some(error)) => error.validate(),
            _ => Err(format!(
                "tool result artifact '{}' has inconsistent ok/body/error fields",
                self.label
            )),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ToolCall {
    pub tool_id: String,
    pub arguments: serde_json::Value,
    pub handler_config: serde_json::Value,
}

#[derive(Clone)]
pub struct ToolHandlerContext {
    pub run: String,
    pub room_root: std::path::PathBuf,
    pub sandbox: ProjectedSandbox,
    pub fs: Arc<dyn FileSurface>,
    pub exec: Arc<dyn ExecSurface>,
    pub artifacts: Option<Arc<dyn ArtifactSurface>>,
}

#[derive(Clone, Debug)]
pub struct ToolHandlerResult {
    pub body: serde_json::Value,
}

impl ToolHandlerResult {
    pub fn new(body: serde_json::Value) -> Self {
        Self { body }
    }
}

#[derive(Debug, Error)]
pub enum ToolHandlerError {
    #[error("tool '{tool_id}' invalid arguments: {reason}")]
    InvalidArguments { tool_id: String, reason: String },
    #[error("tool handler family cannot execute tool '{tool_id}'")]
    UnsupportedTool { tool_id: String },
    #[error("tool '{tool_id}' execution failed: {reason}")]
    Execution { tool_id: String, reason: String },
    #[error("tool '{tool_id}' execution failed: {reason}")]
    ExecutionWithDetails {
        tool_id: String,
        reason: String,
        details: serde_json::Map<String, serde_json::Value>,
    },
}

impl ToolHandlerError {
    pub fn into_tool_result_error(self) -> ToolResultError {
        match self {
            Self::ExecutionWithDetails {
                reason, details, ..
            } => ToolResultError::new("tool_execution_failed", reason).with_details(details),
            other => ToolResultError::new("tool_execution_failed", other.to_string()),
        }
    }
}

pub trait ToolHandler: Send + Sync {
    fn execute<'a>(
        &'a self,
        ctx: &'a ToolHandlerContext,
        call: &'a ToolCall,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<ToolHandlerResult, ToolHandlerError>>
                + Send
                + 'a,
        >,
    >;
}

pub fn required_string_argument(
    tool_id: &str,
    args: &serde_json::Value,
    name: &str,
) -> Result<String, ToolHandlerError> {
    optional_string_argument(tool_id, args, name)?.ok_or_else(|| {
        ToolHandlerError::InvalidArguments {
            tool_id: tool_id.to_string(),
            reason: format!("arguments must include string '{name}'"),
        }
    })
}

pub fn optional_string_argument(
    tool_id: &str,
    args: &serde_json::Value,
    name: &str,
) -> Result<Option<String>, ToolHandlerError> {
    match args.get(name) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(ToolHandlerError::InvalidArguments {
            tool_id: tool_id.to_string(),
            reason: format!("arguments field '{name}' must be a string"),
        }),
    }
}

pub fn required_command_argument(
    tool_id: &str,
    args: &serde_json::Value,
) -> Result<Vec<String>, ToolHandlerError> {
    let Some(command) = args.get("command") else {
        return Err(ToolHandlerError::InvalidArguments {
            tool_id: tool_id.to_string(),
            reason: "arguments must include 'command'".to_string(),
        });
    };
    let invalid = |reason: String| ToolHandlerError::InvalidArguments {
        tool_id: tool_id.to_string(),
        reason,
    };
    match command {
        serde_json::Value::Array(items) => {
            let mut argv = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                let Some(part) = item.as_str().filter(|value| !value.trim().is_empty()) else {
                    return Err(invalid(format!(
                        "command[{index}] must be a non-empty string"
                    )));
                };
                argv.push(part.to_string());
            }
            if argv.is_empty() {
                return Err(invalid("command array must be non-empty".to_string()));
            }
            Ok(argv)
        }
        serde_json::Value::String(command) => {
            let argv: Vec<String> = command
                .split_whitespace()
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect();
            if argv.is_empty() {
                return Err(invalid("command string must be non-empty".to_string()));
            }
            Ok(argv)
        }
        _ => Err(invalid(
            "command must be a string or string array".to_string(),
        )),
    }
}

#[cfg(test)]
mod tool_call_artifact_tests {
    use super::{ToolCallArtifact, ToolResultArtifact, ToolResultError};

    #[test]
    fn strict_core_retains_source_interface_evidence() {
        let artifact: ToolCallArtifact = serde_json::from_value(serde_json::json!({
            "label": "tool.call",
            "tool": "exec.run",
            "arguments": {"command": ["/bin/pwd"]},
            "correlation_id": "call-1",
            "provider_call_id": "provider-1",
            "codex_continuity": {"context_generation": 7}
        }))
        .unwrap();
        assert_eq!(artifact.tool, "exec.run");
        assert_eq!(artifact.source_interface_evidence.len(), 2);
        assert!(
            serde_json::from_value::<ToolCallArtifact>(serde_json::json!({
                "label": "tool.result",
                "tool": "exec.run",
                "arguments": {}
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ToolCallArtifact>(serde_json::json!({
                "tool": "exec.run",
                "arguments": [],
                "correlation_id": " "
            }))
            .is_err()
        );
    }

    #[test]
    fn tool_result_wire_is_strict_and_validates_success_failure_exclusivity() {
        let success: ToolResultArtifact = serde_json::from_value(serde_json::json!({
            "label": "exec.run.result",
            "tool": "exec.run",
            "correlation_id": "call-1",
            "ok": true,
            "body": {"exit_code": 0}
        }))
        .unwrap();
        assert_eq!(success.body.unwrap()["exit_code"], 0);

        let failure: ToolResultArtifact = serde_json::from_value(serde_json::json!({
            "label": "container.status.result",
            "tool": "container.status",
            "correlation_id": "call-2",
            "ok": false,
            "error": {
                "code": "container_engine_unavailable",
                "message": "container engine is unavailable",
                "details": {"engine": "docker"}
            }
        }))
        .unwrap();
        assert_eq!(failure.error.unwrap().code, "container_engine_unavailable");

        for invalid in [
            serde_json::json!({
                "label": "exec.run.result",
                "tool": "exec.run",
                "ok": false,
                "error": "legacy string"
            }),
            serde_json::json!({
                "label": "exec.run.result",
                "tool": "exec.run",
                "ok": false,
                "error": {"code": "failed", "message": "failed", "unknown": true}
            }),
            serde_json::json!({
                "label": "exec.run.result",
                "tool": "exec.run",
                "ok": true,
                "body": null
            }),
            serde_json::json!({
                "label": "exec.run.result",
                "tool": "exec.run",
                "correlation_id": null,
                "ok": true,
                "body": {}
            }),
            serde_json::json!({
                "label": "exec.run.result",
                "tool": "exec.run",
                "ok": false,
                "body": null,
                "error": {"code": "failed", "message": "failed"}
            }),
            serde_json::json!({
                "label": "exec.run.result",
                "tool": "exec.run",
                "ok": false,
                "error": {"code": "failed", "message": "failed", "details": null}
            }),
            serde_json::json!({
                "label": "exec.run.result",
                "tool": "exec.run",
                "ok": true,
                "body": {},
                "error": {"code": "failed", "message": "failed"}
            }),
            serde_json::json!({
                "label": "exec.run.result",
                "tool": "exec.run",
                "ok": true,
                "body": {},
                "details": {"drift": true}
            }),
        ] {
            assert!(serde_json::from_value::<ToolResultArtifact>(invalid).is_err());
        }

        assert!(ToolResultError::new(" ", "failure").validate().is_err());
        assert!(
            serde_json::to_value(ToolResultArtifact::success(
                "exec.run",
                None,
                serde_json::Value::Null,
            ))
            .is_err()
        );
    }
}
