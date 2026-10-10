use octopus_descriptors::ArtifactId;
use std::path::Path;

use octopus_interfaces::{InterfaceArtifactFile, RETIRED_INTERFACE_REQUEST_WORKER_METADATA_FIELD};
use octopus_ledger::{ArtifactRef, BlobStore};

pub const INTERFACE_INPUT_SOURCE_ARTIFACT_FIELD: &str = "source_artifact";

const RETIRED_INTERFACE_REQUEST_INPUT_ARTIFACTS_FIELD: &str = "input_artifacts";

pub fn store_interface_input_artifact<B: BlobStore + ?Sized>(
    blobs: &mut B,
    label: &str,
    bytes: &[u8],
) -> Result<ArtifactRef, String> {
    if label.trim().is_empty() {
        return Err("interface input artifact label must be non-empty".to_string());
    }
    let content = blobs.put(bytes);
    Ok(ArtifactRef {
        id: ArtifactId::new(format!("{}:{}", label, content.to_hex())),
        content,
    })
}

pub fn store_json_artifact<B: BlobStore + ?Sized>(
    blobs: &mut B,
    label: &str,
    value: &serde_json::Value,
) -> Result<ArtifactRef, String> {
    if label.trim().is_empty() {
        return Err("artifact label must be non-empty".to_string());
    }
    let bytes = serde_json::to_vec(value)
        .map_err(|e| format!("artifact label {label} failed to encode as JSON: {e}"))?;
    let content = blobs.put(&bytes);
    Ok(ArtifactRef {
        id: ArtifactId::new(format!("{}:{}", label, content.to_hex())),
        content,
    })
}

#[derive(Clone, Debug)]
pub struct InterfaceInputArtifactBody {
    value: serde_json::Value,
}

impl InterfaceInputArtifactBody {
    pub fn new(prompt: impl Into<String>) -> Result<Self, String> {
        let prompt = prompt.into();
        validate_interface_input_prompt(&prompt)?;
        let mut object = serde_json::Map::new();
        object.insert("prompt".to_string(), serde_json::Value::String(prompt));
        Ok(Self {
            value: serde_json::Value::Object(object),
        })
    }

    pub fn from_input_items(input_items: serde_json::Value) -> Result<Self, String> {
        let prompt = prompt_from_input_items(&input_items)?;
        Self::new(prompt)?.with_input_items(input_items)
    }

    pub fn from_json(mut value: serde_json::Value) -> Result<Self, String> {
        let object = value
            .as_object_mut()
            .ok_or_else(|| "interface input artifact body must be a JSON object".to_string())?;
        reject_retired_worker_metadata_field(object)?;
        reject_retired_input_artifacts_field(object)?;
        Ok(Self { value })
    }

    pub fn prompt(&self) -> Option<&str> {
        self.value.get("prompt").and_then(serde_json::Value::as_str)
    }

    pub fn with_input_items(self, input_items: serde_json::Value) -> Result<Self, String> {
        self.with_field("input_items", input_items)
    }

    pub fn with_turn_context(self, turn_context: serde_json::Value) -> Result<Self, String> {
        self.with_field("turn_context", turn_context)
    }

    pub fn with_field(mut self, key: &str, value: serde_json::Value) -> Result<Self, String> {
        if key.trim().is_empty() {
            return Err("interface input artifact field name must be non-empty".to_string());
        }
        if key == RETIRED_INTERFACE_REQUEST_WORKER_METADATA_FIELD {
            return Err(format!(
                "interface input artifact field '{RETIRED_INTERFACE_REQUEST_WORKER_METADATA_FIELD}' is retired; invocation metadata must ride the envelope"
            ));
        }
        if key == RETIRED_INTERFACE_REQUEST_INPUT_ARTIFACTS_FIELD {
            return Err(format!(
                "interface input artifact field '{RETIRED_INTERFACE_REQUEST_INPUT_ARTIFACTS_FIELD}' is retired; content artifacts must be admitted through run JSON artifact_bindings"
            ));
        }
        let object = self
            .value
            .as_object_mut()
            .expect("InterfaceInputArtifactBody stores an object");
        object.insert(key.to_string(), value);
        Ok(self)
    }

    pub fn as_value(&self) -> &serde_json::Value {
        &self.value
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(&self.value)
            .map_err(|e| format!("interface input artifact body failed to encode: {e}"))
    }
}

fn reject_retired_worker_metadata_field(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if object.contains_key(RETIRED_INTERFACE_REQUEST_WORKER_METADATA_FIELD) {
        return Err(format!(
            "interface input artifact body carries retired '{RETIRED_INTERFACE_REQUEST_WORKER_METADATA_FIELD}'; invocation metadata belongs to the envelope"
        ));
    }
    Ok(())
}

fn reject_retired_input_artifacts_field(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if object.contains_key(RETIRED_INTERFACE_REQUEST_INPUT_ARTIFACTS_FIELD) {
        return Err(format!(
            "interface input artifact body carries retired '{RETIRED_INTERFACE_REQUEST_INPUT_ARTIFACTS_FIELD}'; content artifacts must be admitted through run JSON artifact_bindings"
        ));
    }
    Ok(())
}

pub fn store_interface_input_artifact_body<B: BlobStore + ?Sized>(
    blobs: &mut B,
    artifact_label: &str,
    body: &InterfaceInputArtifactBody,
) -> Result<ArtifactRef, String> {
    validate_interface_input_artifact_label(artifact_label)?;
    let bytes = body.to_bytes()?;
    store_interface_input_artifact(blobs, artifact_label, &bytes)
}

pub fn write_interface_input_artifact_reference(
    root: impl AsRef<Path>,
    artifact_label: &str,
    body: &InterfaceInputArtifactBody,
) -> Result<Vec<u8>, String> {
    validate_interface_input_artifact_label(artifact_label)?;
    let bytes = body.to_bytes()?;
    let artifact_file =
        InterfaceArtifactFile::write(root, artifact_label, &bytes).map_err(|e| {
            format!("interface input artifact reference write failed for '{artifact_label}': {e}")
        })?;
    std::fs::read(artifact_file.manifest_path()).map_err(|e| {
        format!(
            "interface input artifact reference '{}' could not be read: {e}",
            artifact_file.manifest_path().display()
        )
    })
}

fn prompt_from_input_items(input_items: &serde_json::Value) -> Result<String, String> {
    let items = input_items
        .as_array()
        .ok_or_else(|| "interface input artifact input_items must be an array".to_string())?;
    let mut parts = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if item.get("type").and_then(serde_json::Value::as_str) == Some("text") {
            let text = item
                .get("text")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    format!(
                        "interface input artifact input_items[{index}] text item must carry string text"
                    )
                })?;
            if !text.is_empty() {
                parts.push(text.to_string());
            }
            continue;
        }
        parts.push(serde_json::to_string(item).map_err(|e| {
            format!("interface input artifact input_items[{index}] failed to encode: {e}")
        })?);
    }
    Ok(parts.join("\n"))
}

fn validate_interface_input_artifact_label(label: &str) -> Result<(), String> {
    if label.trim().is_empty() {
        return Err("interface input artifact label must be non-empty".to_string());
    }
    Ok(())
}

fn validate_interface_input_prompt(prompt: &str) -> Result<(), String> {
    if prompt.trim().is_empty() {
        return Err("interface input artifact prompt must be non-empty".to_string());
    }
    Ok(())
}
