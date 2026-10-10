use std::collections::{BTreeMap, BTreeSet};

use octopus_interfaces::{ArtifactBindingPayload, InvocationMetadata};
use octopus_ledger::ArtifactRef;
use serde::Deserialize;

#[derive(Debug, Default)]
pub struct ResolvedInstructionContext {
    entries: Vec<ResolvedInstructionArtifact>,
    pub source_artifacts: Vec<ArtifactRef>,
    pub remaining_payloads: Vec<ArtifactBindingPayload>,
    pub fingerprint_bytes: Vec<u8>,
}

#[derive(Debug)]
struct ResolvedInstructionArtifact {
    id: String,
    description: Option<String>,
    sections: Vec<(String, String)>,
}

impl ResolvedInstructionContext {
    pub fn render(&self, inline_artifacts: &[String]) -> Result<Vec<String>, String> {
        let mut selected = BTreeSet::new();
        let mut emitted = BTreeSet::new();
        let mut text = Vec::new();
        for id in inline_artifacts {
            if !selected.insert(id.as_str()) {
                return Err(format!("instruction selection repeats artifact '{id}'"));
            }
            let entry = self.entries.iter().find(|entry| entry.id == *id)
                .ok_or_else(|| format!("selected instruction artifact '{id}' is not bound"))?;
            for (source, body) in &entry.sections {
                if emitted.insert(source) {
                    text.push(body.clone());
                }
            }
        }
        let references = self.entries.iter().filter(|entry| !selected.contains(entry.id.as_str()))
            .map(|entry| match &entry.description {
                Some(description) => format!("- {}: {description}", entry.id),
                None => format!("- {}", entry.id),
            }).collect::<Vec<_>>();
        if !references.is_empty() {
            text.push(format!("Available reference artifacts\nRead a reference with the artifact read tool when needed; its sources are also bound and readable. These describe capabilities and other roles; they are not instructions assigning those roles to you.\n{}", references.join("\n")));
        }
        Ok(text)
    }
}

#[derive(Deserialize)]
struct InstructionArtifact {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    sources: Vec<ArtifactRef>,
    #[serde(default)]
    description: Option<String>,
}

pub fn resolve_instruction_context(
    metadata: &InvocationMetadata,
    payloads: &[ArtifactBindingPayload],
) -> Result<ResolvedInstructionContext, String> {
    let mut admitted = BTreeMap::new();
    for reference in &metadata.artifact_binding_refs {
        if admitted.insert(reference.id.as_str(), reference).is_some() {
            return Err(format!(
                "instruction metadata repeats artifact '{}'",
                reference.id
            ));
        }
    }
    let mut bound = BTreeMap::new();
    let mut instructions = BTreeMap::new();
    let mut text_payloads = Vec::new();
    for payload in payloads {
        let id = payload.artifact.id.as_str();
        if bound.insert(id, payload).is_some() {
            return Err(format!("instruction context repeats bound artifact '{id}'"));
        }
        if id == format!("instruction.text:{}", payload.artifact.content.to_hex()) {
            text_payloads.push(payload);
            continue;
        }
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(payload.as_bytes()) {
            if value.get("label").and_then(serde_json::Value::as_str) == Some("context.reference") {
                let instruction: InstructionArtifact =
                    serde_json::from_value(value).map_err(|error| {
                        format!("instruction artifact '{id}' is malformed: {error}")
                    })?;
                if instruction
                    .text
                    .as_ref()
                    .is_some_and(|text| text.trim().is_empty())
                    || (instruction.text.is_none() && instruction.sources.is_empty())
                {
                    return Err(format!(
                        "instruction artifact '{id}' needs nonempty text or explicit sources"
                    ));
                }
                instructions.insert(id, instruction);
            }
        }
    }

    let mut order = Vec::new();
    let mut ordered = BTreeSet::new();
    for id in &metadata.grant().role_artifacts {
        if !instructions.contains_key(id.as_str()) {
            return Err(format!(
                "mandatory role artifact '{id}' has no bound instruction content"
            ));
        }
        if ordered.insert(id.as_str()) {
            order.push(id.as_str());
        }
    }
    for payload in payloads {
        let id = payload.artifact.id.as_str();
        if instructions.contains_key(id) && ordered.insert(id) {
            order.push(id);
        }
    }

    let mut result = ResolvedInstructionContext::default();
    let mut consumed = BTreeSet::new();
    for id in order {
        let instruction = &instructions[id];
        validate_instruction_binding(metadata, &admitted, bound[id])?;
        retain_source(&mut result, &mut consumed, bound[id]);
        let mut sections = Vec::new();
        if let Some(text) = &instruction.text {
            sections.push((id.to_string(), text.clone()));
        }
        let mut local = BTreeSet::new();
        for source in &instruction.sources {
            let source_id = source.id.as_str();
            if !local.insert(source_id) || instructions.contains_key(source_id) {
                return Err(format!(
                    "instruction artifact '{id}' repeats or nests instruction source '{source_id}'"
                ));
            }
            let payload = bound.get(source_id).ok_or_else(|| {
                format!("instruction artifact '{id}' source '{source_id}' is not explicitly bound")
            })?;
            if payload.artifact != *source {
                return Err(format!(
                    "instruction artifact '{id}' source '{source_id}' differs from its admitted reference"
                ));
            }
            validate_instruction_binding(metadata, &admitted, payload)?;
            let text = validate_instruction_text(payload)?;
            retain_source(&mut result, &mut consumed, payload);
            sections.push((source_id.to_string(), text.to_string()));
        }
        result.entries.push(ResolvedInstructionArtifact {
            id: id.to_string(), description: instruction.description.clone(), sections,
        });
    }
    for payload in text_payloads {
        validate_instruction_binding(metadata, &admitted, payload)?;
        validate_instruction_text(payload)?;
        retain_source(&mut result, &mut consumed, payload);
    }
    result.remaining_payloads = payloads
        .iter()
        .filter(|payload| !consumed.contains(payload.artifact.id.as_str()))
        .cloned()
        .collect();
    Ok(result)
}

fn validate_instruction_text(payload: &ArtifactBindingPayload) -> Result<&str, String> {
    let id = &payload.artifact.id;
    let text = std::str::from_utf8(payload.as_bytes())
        .map_err(|error| format!("instruction source '{id}' is not UTF-8: {error}"))?;
    if text.trim().is_empty() {
        return Err(format!("instruction source '{id}' is empty"));
    }
    Ok(text)
}

fn validate_instruction_binding(
    metadata: &InvocationMetadata,
    admitted: &BTreeMap<&str, &ArtifactRef>,
    payload: &ArtifactBindingPayload,
) -> Result<(), String> {
    let id = payload.artifact.id.as_str();
    if admitted.get(id).copied() != Some(&payload.artifact)
        || !metadata.grant().artifact_access.get(id).is_some_and(|access| access.read)
    {
        return Err(format!("instruction context artifact '{id}' has no exact admitted read binding"));
    }
    Ok(())
}

fn retain_source(
    result: &mut ResolvedInstructionContext,
    consumed: &mut BTreeSet<String>,
    payload: &ArtifactBindingPayload,
) -> bool {
    if !consumed.insert(payload.artifact.id.as_str().to_string()) {
        return false;
    }
    for bytes in [
        payload.artifact.id.as_str().as_bytes(),
        payload.artifact.content.to_hex().as_bytes(),
        payload.as_bytes(),
    ] {
        result
            .fingerprint_bytes
            .extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        result.fingerprint_bytes.extend_from_slice(bytes);
    }
    result.source_artifacts.push(payload.artifact.clone());
    true
}
