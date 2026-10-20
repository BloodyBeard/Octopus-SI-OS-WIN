use super::*;

pub(crate) fn read_pinned_artifact_payload(
    ctx: &InterfaceLiftContext,
    artifact: &ArtifactRef,
    expected_label: &str,
) -> Result<Vec<u8>, String> {
    let (manifest, payload) = read_pinned_artifact(ctx, artifact)?;
    if manifest.label != expected_label {
        return Err(format!(
            "provider recovery artifact label '{}' differs from '{}'",
            manifest.label, expected_label
        ));
    }
    Ok(payload)
}

pub(crate) fn read_artifact_ref_payload_from_cache(
    blobs_dir: &Path, limits: InterfaceHostIoLimits, artifact: &ArtifactRef,
) -> Result<Vec<u8>, String> {
    let carrier_path = blobs_dir.join(artifact.content.to_hex());
    let carrier = octopus_interfaces::read_artifact_carrier_file_bounded(
        &carrier_path,
        limits.max_manifest_bytes.max(limits.max_payload_bytes),
    )
    .map_err(|error| {
        format!(
            "provider bound artifact carrier '{}' cannot be read: {error}",
            carrier_path.display()
        )
    })?;
    if ContentHash::of_bytes(&carrier) != artifact.content {
        return Err("provider bound artifact carrier hash does not match its ref".to_string());
    }
    let Ok(manifest) = serde_json::from_slice::<InterfaceArtifactManifest>(&carrier) else {
        if carrier.len() as u64 > limits.max_payload_bytes {
            return Err("provider bound artifact payload exceeds the declared limit".to_string());
        }
        return Ok(carrier);
    };
    if manifest.payload_path.as_os_str().is_empty() {
        if carrier.len() as u64 > limits.max_payload_bytes {
            return Err("provider bound artifact payload exceeds the declared limit".to_string());
        }
        return Ok(carrier);
    }
    if carrier.len() as u64 > limits.max_manifest_bytes {
        return Err("provider bound artifact manifest exceeds the declared limit".to_string());
    }
    let expected_id = format!("{}:{}", manifest.label, artifact.content.to_hex());
    if artifact.id.as_str() != expected_id {
        return Err("provider bound artifact id is not its pinned manifest identity".to_string());
    }
    let payload_hash = manifest
        .payload_content_hash
        .as_deref()
        .and_then(ContentHash::from_hex)
        .ok_or_else(|| {
            "provider bound artifact manifest has no canonical payload hash".to_string()
        })?;
    let payload_path = blobs_dir.join(payload_hash.to_hex());
    let payload = octopus_interfaces::read_artifact_carrier_file_bounded(
        &payload_path,
        limits.max_payload_bytes,
    )
    .map_err(|error| {
        format!(
            "provider bound artifact payload '{}' cannot be read: {error}",
            payload_path.display()
        )
    })?;
    if ContentHash::of_bytes(&payload) != payload_hash {
        return Err("provider bound artifact payload hash does not match its manifest".to_string());
    }
    Ok(payload)
}

pub(crate) fn read_pinned_artifact(
    ctx: &InterfaceLiftContext,
    artifact: &ArtifactRef,
) -> Result<(InterfaceArtifactManifest, Vec<u8>), String> {
    let limits = ctx.required_host_io_limits()?;
    read_pinned_artifact_from_cache(&ctx.blobs_dir, limits, artifact)
}

pub(crate) fn read_pinned_artifact_from_cache(
    blobs_dir: &Path,
    limits: InterfaceHostIoLimits,
    artifact: &ArtifactRef,
) -> Result<(InterfaceArtifactManifest, Vec<u8>), String> {
    read_pinned_artifact_with_bounds(blobs_dir,limits.max_manifest_bytes,limits.max_payload_bytes,artifact)
}

pub(crate) fn read_pinned_artifact_with_bounds(
    blobs_dir: &Path,
    max_manifest_bytes: u64,
    max_payload_bytes: u64,
    artifact: &ArtifactRef,
) -> Result<(InterfaceArtifactManifest, Vec<u8>), String> {
    let carrier_path = blobs_dir.join(artifact.content.to_hex());
    let carrier = octopus_interfaces::read_artifact_carrier_file_bounded(
        &carrier_path,
        max_manifest_bytes,
    )
    .map_err(|error| {
        format!(
            "provider recovery artifact carrier '{}' cannot be read: {error}",
            carrier_path.display()
        )
    })?;
    if ContentHash::of_bytes(&carrier) != artifact.content {
        return Err("provider recovery artifact carrier hash does not match its ref".to_string());
    }
    let manifest: InterfaceArtifactManifest =
        serde_json::from_slice(&carrier).map_err(|error| {
            format!("provider recovery artifact carrier is not a manifest: {error}")
        })?;
    let expected_id = format!("{}:{}", manifest.label, artifact.content.to_hex());
    if artifact.id.as_str() != expected_id {
        return Err(
            "provider recovery artifact id is not its pinned manifest identity".to_string(),
        );
    }
    let payload_hash = manifest
        .payload_content_hash
        .as_deref()
        .and_then(ContentHash::from_hex)
        .ok_or_else(|| "provider recovery manifest has no canonical payload hash".to_string())?;
    let payload_path = blobs_dir.join(payload_hash.to_hex());
    let payload = octopus_interfaces::read_artifact_carrier_file_bounded(
        &payload_path,
        max_payload_bytes,
    )
    .map_err(|error| {
        format!(
            "provider recovery artifact payload '{}' cannot be read: {error}",
            payload_path.display()
        )
    })?;
    if ContentHash::of_bytes(&payload) != payload_hash {
        return Err("provider recovery payload hash does not match its manifest".to_string());
    }
    Ok((manifest, payload))
}

pub(crate) fn artifact_identity(
    artifact: &InterfaceArtifactFile,
    var_dir: &Path,
) -> Result<HandoffArtifactIdentity, String> {
    let canonical_var_dir = var_dir.canonicalize().map_err(|error| {
        format!(
            "provider interface var dir '{}' cannot be canonicalized: {error}",
            var_dir.display()
        )
    })?;
    let canonical_manifest = artifact.manifest_path().canonicalize().map_err(|error| {
        format!(
            "provider output manifest '{}' cannot be canonicalized: {error}",
            artifact.manifest_path().display()
        )
    })?;
    let manifest_relative_path = canonical_manifest
        .strip_prefix(&canonical_var_dir)
        .map_err(|_| {
            format!(
                "provider output manifest '{}' escapes interface var dir '{}'",
                canonical_manifest.display(),
                canonical_var_dir.display()
            )
        })?
        .to_path_buf();
    let manifest_bytes = std::fs::read(&canonical_manifest).map_err(|error| {
        format!(
            "provider output manifest '{}' cannot be read: {error}",
            canonical_manifest.display()
        )
    })?;
    let manifest = artifact
        .read_manifest()
        .map_err(|error| format!("provider output manifest failed closed: {error}"))?;
    let content = ContentHash::of_bytes(&manifest_bytes);
    Ok(HandoffArtifactIdentity {
        artifact: ArtifactRef {
            id: ArtifactId::new(format!("{}:{}", manifest.label, content.to_hex())),
            content,
        },
        label: manifest.label,
        manifest_relative_path,
    })
}

pub(crate) fn load_pending_artifact(
    var_dir: &Path,
    pending: &PendingHandoffItem,
) -> Result<(InterfaceArtifactFile, ArtifactRef), String> {
    if pending.manifest_relative_path.as_os_str().is_empty()
        || pending.manifest_relative_path.is_absolute()
        || pending
            .manifest_relative_path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(
            "provider pending handoff manifest path is not a safe relative path".to_string(),
        );
    }
    let path = var_dir.join(&pending.manifest_relative_path);
    let artifact = InterfaceArtifactFile::new(&path).map_err(|error| {
        format!(
            "provider pending handoff manifest '{}' cannot be opened: {error}",
            path.display()
        )
    })?;
    let identity = artifact_identity(&artifact, var_dir)?;
    if identity.artifact.id != pending.artifact_id
        || identity.artifact.content.to_hex() != pending.artifact_content_hash
        || identity.label != pending.artifact_label
        || identity.manifest_relative_path != pending.manifest_relative_path
    {
        return Err(format!(
            "provider pending handoff artifact '{}' no longer matches its run-record evidence",
            pending.artifact_id.as_str()
        ));
    }
    Ok((artifact, identity.artifact))
}

pub(crate) fn append_artifact(artifacts: &mut Vec<ArtifactId>, artifact: ArtifactId) -> bool {
    if artifacts.iter().any(|current| current == &artifact) {
        false
    } else {
        artifacts.push(artifact);
        true
    }
}

pub(crate) fn artifact_ref_key(artifact: &ArtifactRef) -> String {
    format!("{}:{}", artifact.id.as_str(), artifact.content.to_hex())
}
