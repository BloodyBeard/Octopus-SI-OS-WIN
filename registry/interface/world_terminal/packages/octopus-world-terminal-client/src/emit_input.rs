use std::path::Path;

use octopus_app_server_interface::{
    InterfaceInputArtifactBody, write_interface_input_artifact_reference,
};
use octopus_interfaces::{InterfaceArtifactFile, InterfaceArtifactFileError};

pub const TERMINAL_INPUT_ARTIFACT_LABEL: &str = "operator.terminal.input";

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TerminalUserInput {
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        text_elements: Vec<serde_json::Value>,
    },
    Image {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<serde_json::Value>,
        url: String,
    },
    LocalImage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<serde_json::Value>,
        path: std::path::PathBuf,
    },
    Skill {
        name: String,
        path: std::path::PathBuf,
    },
    Mention {
        name: String,
        path: String,
    },
}

impl TerminalUserInput {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            text_elements: Vec::new(),
        }
    }

    pub fn to_input_items_value(items: &[TerminalUserInput]) -> serde_json::Value {
        serde_json::to_value(items).unwrap_or(serde_json::Value::Array(Vec::new()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TerminalInputError {
    #[error("terminal input body is invalid: {0}")]
    InvalidBody(String),
    #[error("terminal input artifact could not be materialized: {0}")]
    Materialize(String),
    #[error("terminal input artifact manifest is not a valid manifest: {0}")]
    Manifest(#[from] InterfaceArtifactFileError),
}

#[derive(Debug)]
pub struct TerminalInputArtifact {
    pub artifact_file: InterfaceArtifactFile,
    pub manifest_bytes: Vec<u8>,
}

pub fn terminal_input_manifest(
    artifact_root: impl AsRef<Path>,
    prompt: &str,
) -> Result<TerminalInputArtifact, TerminalInputError> {
    let body = InterfaceInputArtifactBody::new(prompt).map_err(TerminalInputError::InvalidBody)?;
    materialize(artifact_root.as_ref(), &body)
}

pub fn materialize_input_artifact(
    artifact_root: impl AsRef<Path>,
    label: &str,
    body: &InterfaceInputArtifactBody,
) -> Result<TerminalInputArtifact, TerminalInputError> {
    materialize_with_label(artifact_root.as_ref(), label, body)
}

pub fn terminal_input_manifest_from_items(
    artifact_root: impl AsRef<Path>,
    input_items: serde_json::Value,
) -> Result<TerminalInputArtifact, TerminalInputError> {
    let body = InterfaceInputArtifactBody::from_input_items(input_items)
        .map_err(TerminalInputError::InvalidBody)?;
    materialize(artifact_root.as_ref(), &body)
}

pub fn write_terminal_input_reference(
    artifact_root: impl AsRef<Path>,
    artifact_label: &str,
    body: &InterfaceInputArtifactBody,
) -> Result<Vec<u8>, TerminalInputError> {
    write_interface_input_artifact_reference(artifact_root.as_ref(), artifact_label, body)
        .map_err(TerminalInputError::Materialize)
}

fn materialize(
    artifact_root: &Path,
    body: &InterfaceInputArtifactBody,
) -> Result<TerminalInputArtifact, TerminalInputError> {
    materialize_with_label(artifact_root, TERMINAL_INPUT_ARTIFACT_LABEL, body)
}

fn materialize_with_label(
    artifact_root: &Path,
    label: &str,
    body: &InterfaceInputArtifactBody,
) -> Result<TerminalInputArtifact, TerminalInputError> {
    let manifest_bytes = write_interface_input_artifact_reference(artifact_root, label, body)
        .map_err(TerminalInputError::Materialize)?;
    let artifact_file = reopen_manifest(artifact_root, &manifest_bytes)?;
    Ok(TerminalInputArtifact {
        artifact_file,
        manifest_bytes,
    })
}

fn reopen_manifest(
    artifact_root: &Path,
    manifest_bytes: &[u8],
) -> Result<InterfaceArtifactFile, TerminalInputError> {
    let entries = std::fs::read_dir(artifact_root).map_err(|e| {
        TerminalInputError::Materialize(format!(
            "terminal artifact root '{}' is unreadable after write: {e}",
            artifact_root.display()
        ))
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_manifest = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".artifact.json"));
        if !is_manifest {
            continue;
        }
        match std::fs::read(&path) {
            Ok(bytes) if bytes == manifest_bytes => {
                return InterfaceArtifactFile::new(path).map_err(TerminalInputError::Manifest);
            }
            _ => continue,
        }
    }
    Err(TerminalInputError::Materialize(format!(
        "terminal input manifest not found under '{}' after write",
        artifact_root.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_reopenable_input_manifest_with_the_interface_body() {
        let dir = tempfile::tempdir().unwrap();
        let built = terminal_input_manifest(dir.path(), "hello runtime").unwrap();

        assert!(built.artifact_file.manifest_path().is_absolute());
        let reopened = InterfaceArtifactFile::new(built.artifact_file.manifest_path()).unwrap();
        let manifest = reopened.read_manifest().unwrap();
        assert_eq!(manifest.label, TERMINAL_INPUT_ARTIFACT_LABEL);

        let payload = reopened.read_payload_bytes().unwrap();
        let body: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(
            body.get("prompt").and_then(|p| p.as_str()),
            Some("hello runtime")
        );
    }

    #[test]
    fn empty_prompt_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let err = terminal_input_manifest(dir.path(), "   ").unwrap_err();
        assert!(matches!(err, TerminalInputError::InvalidBody(_)));
    }

    #[test]
    fn builds_from_structured_input_items() {
        let dir = tempfile::tempdir().unwrap();
        let items = serde_json::json!([
            { "type": "text", "text": "line one" },
            { "type": "text", "text": "line two" },
        ]);
        let built = terminal_input_manifest_from_items(dir.path(), items).unwrap();
        let manifest = built.artifact_file.read_manifest().unwrap();
        assert_eq!(manifest.label, TERMINAL_INPUT_ARTIFACT_LABEL);
        let payload = built.artifact_file.read_payload_bytes().unwrap();
        let body: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(
            body.get("prompt").and_then(|p| p.as_str()),
            Some("line one\nline two")
        );
        assert!(body.get("input_items").is_some());
    }
}
