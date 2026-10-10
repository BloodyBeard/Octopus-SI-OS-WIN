use std::path::Path;

use octopus_app_server_interface::{
    InterfaceInputArtifactBody, OPERATOR_APPROVAL_RESPONSE_LABEL,
    OPERATOR_ELICITATION_RESPONSE_LABEL,
};

use crate::emit_input::{TerminalInputArtifact, TerminalInputError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub request_id: String,
    pub kind: String,
    pub title: String,
    pub detail: serde_json::Value,
    pub method: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElicitationRequest {
    pub request_id: String,
    pub message: String,
    pub requested_schema: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperatorRequestResolved {
    pub request_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RichRequestError {
    #[error("rich-feature request payload is not a JSON object: {0}")]
    NotAnObject(String),
    #[error("rich-feature request payload has no non-empty '{0}' field")]
    MissingField(&'static str),
    #[error("rich-feature response could not be emitted: {0}")]
    Emit(#[from] TerminalInputError),
    #[error("rich-feature response hand-in failed: {0}")]
    HandIn(#[from] crate::daemon_arrival::DaemonArrivalError),
}

impl ApprovalRequest {
    pub fn from_payload(bytes: &[u8]) -> Result<Self, RichRequestError> {
        let value = parse_object(bytes)?;
        let request_id = required_string_field(&value, "request_id")?;
        Ok(Self {
            request_id,
            kind: string_field(&value, "kind"),
            title: string_field(&value, "title"),
            detail: value
                .get("detail")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            method: required_string_field(&value, "method")?,
        })
    }
}

impl ElicitationRequest {
    pub fn from_payload(bytes: &[u8]) -> Result<Self, RichRequestError> {
        let value = parse_object(bytes)?;
        let request_id = required_string_field(&value, "request_id")?;
        Ok(Self {
            request_id,
            message: string_field(&value, "message"),
            requested_schema: value
                .get("requested_schema")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        })
    }
}

impl OperatorRequestResolved {
    pub fn from_payload(bytes: &[u8]) -> Result<Self, RichRequestError> {
        let value = parse_object(bytes)?;
        let request_id = value
            .get("request_id")
            .or_else(|| value.get("requestId"))
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned)
            .ok_or(RichRequestError::MissingField("request_id/requestId"))?;
        Ok(Self { request_id })
    }
}

pub fn emit_approval_response(
    artifact_root: impl AsRef<Path>,
    request_id: &str,
    decision: serde_json::Value,
) -> Result<TerminalInputArtifact, RichRequestError> {
    let response = serde_json::json!({
        "request_id": request_id,
        "decision": decision,
    });
    emit_operator_response(
        artifact_root.as_ref(),
        OPERATOR_APPROVAL_RESPONSE_LABEL,
        "operator_approval_response",
        &format!("operator approval decision for request '{request_id}'"),
        response,
    )
}

pub fn emit_elicitation_response(
    artifact_root: impl AsRef<Path>,
    request_id: &str,
    answer: serde_json::Value,
) -> Result<TerminalInputArtifact, RichRequestError> {
    let response = serde_json::json!({
        "request_id": request_id,
        "answer": answer,
    });
    emit_operator_response(
        artifact_root.as_ref(),
        OPERATOR_ELICITATION_RESPONSE_LABEL,
        "operator_elicitation_response",
        &format!("operator elicitation answer for request '{request_id}'"),
        response,
    )
}

pub(crate) fn emit_operator_response(
    artifact_root: &Path,
    label: &str,
    field: &str,
    prompt: &str,
    response: serde_json::Value,
) -> Result<TerminalInputArtifact, RichRequestError> {
    let body = InterfaceInputArtifactBody::new(prompt)
        .and_then(|body| body.with_field(field, response))
        .map_err(|e| RichRequestError::Emit(TerminalInputError::InvalidBody(e)))?;
    crate::emit_input::materialize_input_artifact(artifact_root, label, &body)
        .map_err(RichRequestError::Emit)
}

fn parse_object(
    bytes: &[u8],
) -> Result<serde_json::Map<String, serde_json::Value>, RichRequestError> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| RichRequestError::NotAnObject(e.to_string()))?;
    match value {
        serde_json::Value::Object(map) => Ok(map),
        other => Err(RichRequestError::NotAnObject(format!(
            "expected a JSON object, got {}",
            json_kind(&other)
        ))),
    }
}

fn string_field(value: &serde_json::Map<String, serde_json::Value>, key: &str) -> String {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn required_string_field(
    value: &serde_json::Map<String, serde_json::Value>,
    key: &'static str,
) -> Result<String, RichRequestError> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or(RichRequestError::MissingField(key))
}

fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_interfaces::InterfaceArtifactFile;

    #[test]
    fn decodes_approval_request_from_payload() {
        let payload = serde_json::json!({
            "request_id": "call-42",
            "kind": "exec",
            "title": "rm -rf /tmp/x",
            "detail": { "command": ["rm", "-rf", "/tmp/x"], "cwd": "/work" },
            "method": "item/commandExecution/requestApproval",
        });
        let req = ApprovalRequest::from_payload(&serde_json::to_vec(&payload).unwrap()).unwrap();
        assert_eq!(req.request_id, "call-42");
        assert_eq!(req.kind, "exec");
        assert_eq!(req.title, "rm -rf /tmp/x");
        assert_eq!(req.detail["cwd"], "/work");
        assert_eq!(req.method, "item/commandExecution/requestApproval");
    }

    #[test]
    fn approval_request_requires_exact_id_and_object_shape() {
        assert!(matches!(
            ApprovalRequest::from_payload(br#"{"kind":"patch"}"#),
            Err(RichRequestError::MissingField("request_id"))
        ));
        assert!(matches!(
            ApprovalRequest::from_payload(b"[1,2,3]"),
            Err(RichRequestError::NotAnObject(_))
        ));
        assert!(matches!(
            ApprovalRequest::from_payload(b"not json"),
            Err(RichRequestError::NotAnObject(_))
        ));
    }

    #[test]
    fn decodes_elicitation_request_from_payload() {
        let payload = serde_json::json!({
            "request_id": "elic-7",
            "message": "Which environment?",
            "requested_schema": { "type": "string", "enum": ["dev", "prod"] },
        });
        let req = ElicitationRequest::from_payload(&serde_json::to_vec(&payload).unwrap()).unwrap();
        assert_eq!(req.request_id, "elic-7");
        assert_eq!(req.message, "Which environment?");
        assert_eq!(req.requested_schema["type"], "string");
    }

    #[test]
    fn decodes_operator_request_resolution_from_provider_wire() {
        let resolved = OperatorRequestResolved::from_payload(
            br#"{"threadId":"thread-1","requestId":"request-9"}"#,
        )
        .unwrap();
        assert_eq!(resolved.request_id, "request-9");
    }

    #[test]
    fn emits_approval_response_as_input_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let built =
            emit_approval_response(dir.path(), "call-42", serde_json::json!("accept")).unwrap();

        let reopened = InterfaceArtifactFile::new(built.artifact_file.manifest_path()).unwrap();
        let manifest = reopened.read_manifest().unwrap();
        assert_eq!(manifest.label, OPERATOR_APPROVAL_RESPONSE_LABEL);

        let payload = reopened.read_payload_bytes().unwrap();
        let body: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(body["operator_approval_response"]["request_id"], "call-42");
        assert_eq!(body["operator_approval_response"]["decision"], "accept");
        assert!(body.get("prompt").and_then(|p| p.as_str()).is_some());
    }

    #[test]
    fn emits_elicitation_response_as_input_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let built = emit_elicitation_response(
            dir.path(),
            "elic-7",
            serde_json::json!({ "accept": true, "content": { "env": "dev" } }),
        )
        .unwrap();
        let manifest = built.artifact_file.read_manifest().unwrap();
        assert_eq!(manifest.label, OPERATOR_ELICITATION_RESPONSE_LABEL);
        let payload = built.artifact_file.read_payload_bytes().unwrap();
        let body: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(
            body["operator_elicitation_response"]["request_id"],
            "elic-7"
        );
        assert_eq!(
            body["operator_elicitation_response"]["answer"]["content"]["env"],
            "dev"
        );
    }
}
