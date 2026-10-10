use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use octopus_descriptors::canonical_bytes;
use octopus_interfaces::{ArtifactBindingPayload, GrantProjection, InvocationMetadata};
use octopus_ledger::{ArtifactRef, ContentHash};
use octopus_model_context_render::resolve_instruction_context;
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pack {
    artifacts: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    id: String,
    source: PathBuf,
    #[serde(default)]
    description: Option<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err(
            "usage: materialize_instruction_pack PACK_JSON TARGET_HOME STAGING_DIRECTORY".into(),
        );
    }
    let pack_path = Path::new(&args[0]).canonicalize()?;
    let target = Path::new(&args[1]);
    let output = Path::new(&args[2]);
    if !target.is_absolute() || !output.is_absolute() || output.exists() {
        return Err("target and staging paths must be absolute; staging must be new".into());
    }
    let pack: Pack = serde_json::from_slice(&std::fs::read(&pack_path)?)?;
    let root = pack_path.parent().ok_or("pack has no parent")?;
    let mut ids = BTreeSet::new();
    let mut files = std::collections::BTreeMap::new();
    let mut evidence = Vec::new();
    let mut bound_sources = std::collections::BTreeMap::new();
    let mut instructions = Vec::new();
    for entry in pack.artifacts {
        if entry.id.is_empty()
            || !entry
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || !ids.insert(entry.id.clone())
        {
            return Err("pack contains invalid or repeated artifact identity".into());
        }
        if entry.source.as_os_str().is_empty()
            || entry
                .source
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err("instruction source must be a relative file path inside the pack".into());
        }
        let source_path = root.join(&entry.source).canonicalize()?;
        if !source_path.starts_with(root) {
            return Err("instruction source leaves pack directory".into());
        }
        let bytes = std::fs::read(source_path)?;
        if std::str::from_utf8(&bytes)?.trim().is_empty() {
            return Err("instruction source is empty".into());
        }
        let hash = ContentHash::of_bytes(&bytes);
        let source_member = format!("instruction_sources/{}.md", hash.to_hex());
        let manifest = canonical_bytes(
            &json!({"label":"instruction.text", "payload_path":target.join(&source_member), "payload_content_hash":hash.to_hex()}),
        );
        let manifest_hash = ContentHash::of_bytes(&manifest);
        let reference = ArtifactRef {
            id: format!("instruction.text:{}", manifest_hash.to_hex()).into(),
            content: manifest_hash.clone(),
        };
        let mut instruction = json!({"id":entry.id,"label":"context.reference","sources":[reference]});
        if let Some(description) = entry.description {
            if description.trim().is_empty() { return Err("instruction description is empty".into()); }
            instruction["description"] = json!(description);
        }
        let instruction = canonical_bytes(&instruction);
        bound_sources.insert(
            reference.id.clone(),
            ArtifactBindingPayload::new(reference.clone(), bytes.clone()),
        );
        instructions.push(ArtifactBindingPayload::new(
            ArtifactRef {
                id: entry.id.clone().into(),
                content: ContentHash::of_bytes(&instruction),
            },
            instruction.clone(),
        ));
        files.insert(source_member, bytes.clone());
        files.insert(format!("blobs/{}", hash.to_hex()), bytes);
        files.insert(format!("blobs/{}", manifest_hash.to_hex()), manifest);
        files.insert(
            format!("registry/artifact/{}/artifact.json", entry.id),
            instruction.clone(),
        );
        evidence.push(json!({"id":entry.id,"source":entry.source,"artifact_content":ContentHash::of_bytes(&instruction),"source_artifact":reference,"source_content":hash}));
    }
    let source_count = bound_sources.len();
    let mut metadata = InvocationMetadata::new(GrantProjection::default());
    metadata.grant.role_artifacts = instructions
        .iter()
        .map(|p| p.artifact.id.as_str().to_string())
        .collect();
    instructions.extend(bound_sources.into_values());
    metadata.artifact_binding_refs = instructions.iter().map(|p| p.artifact.clone()).collect();
    for payload in &instructions {
        metadata
            .grant
            .artifact_access
            .insert(payload.artifact.id.as_str().to_string(), Default::default());
    }
    let resolved = resolve_instruction_context(&metadata, &instructions)?;
    let text = resolved.render(&metadata.grant.role_artifacts)?;
    if text.len() != source_count || !resolved.remaining_payloads.is_empty() {
        return Err(
            "compiled pack repeats instruction content or leaves an unconsumed source".into(),
        );
    }
    files.insert(
        "pack-evidence.json".into(),
        serde_json::to_vec_pretty(&json!({"target_home":target,"artifacts":evidence,"verification":{"resolved_instruction_sections":text.len(),"consumed_artifacts":resolved.source_artifacts.len(),"remaining_payloads":resolved.remaining_payloads.len(),"fingerprint":ContentHash::of_bytes(&resolved.fingerprint_bytes)}}))?,
    );
    for (member, bytes) in &files {
        let path = output.join(member);
        std::fs::create_dir_all(path.parent().ok_or("member has no parent")?)?;
        std::fs::write(path, bytes)?;
    }
    println!(
        "staged {} files for {} artifacts at {}",
        files.len(),
        ids.len(),
        output.display()
    );
    Ok(())
}
