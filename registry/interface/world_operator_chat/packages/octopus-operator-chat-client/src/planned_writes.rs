use super::*;

pub(crate) fn load_interface_descriptor(
    interface_root: &Path,
    interface_id: &str,
) -> Result<InterfaceDescriptor, String> {
    let id = safe_segment(interface_id, "interface id")?;
    let path = interface_root.join(&id).join("interface.json");
    let bytes = std::fs::read(&path).map_err(|error| {
        format!(
            "interface descriptor '{}' cannot be read: {error}",
            path.display()
        )
    })?;
    let descriptor = parse_strict::<InterfaceDescriptor>(&bytes).map_err(|error| {
        format!(
            "interface descriptor '{}' is malformed: {error}",
            path.display()
        )
    })?;
    if descriptor.id.as_str() != id {
        return Err(format!(
            "interface descriptor '{}' declares mismatched id '{}'",
            path.display(),
            descriptor.id
        ));
    }
    Ok(descriptor)
}

pub(crate) fn push_json_write(
    path: PathBuf,
    content: String,
    tag: String,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    let planned: serde_json::Value = serde_json::from_str(&content)
        .map_err(|error| format!("planned write '{}' is not JSON: {error}", path.display()))?;
    match std::fs::read(&path) {
        Ok(existing) => {
            if let Ok(current) = serde_json::from_slice::<serde_json::Value>(&existing) {
                if current == planned {
                    return Ok(());
                }
            }
            writes.push(PlannedClientWrite {
                path,
                content,
                tag,
                expected_before_hash: Some(sha256_hex(&existing)),
            });
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            writes.push(PlannedClientWrite {
                path,
                content,
                tag,
                expected_before_hash: None,
            });
            Ok(())
        }
        Err(error) => Err(format!(
            "planned write target '{}' cannot be read: {error}",
            path.display()
        )),
    }
}

pub(crate) fn push_text_write(
    path: PathBuf,
    content: String,
    tag: String,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    match std::fs::read(&path) {
        Ok(existing) => {
            if existing == content.as_bytes() {
                return Ok(());
            }
            writes.push(PlannedClientWrite {
                path,
                content,
                tag,
                expected_before_hash: Some(sha256_hex(&existing)),
            });
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            writes.push(PlannedClientWrite {
                path,
                content,
                tag,
                expected_before_hash: None,
            });
            Ok(())
        }
        Err(error) => Err(format!(
            "planned write target '{}' cannot be read: {error}",
            path.display()
        )),
    }
}

pub(crate) fn capability_interface_segment(interface: &str) -> String {
    interface
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

pub(crate) fn pretty_json(value: &serde_json::Value) -> Result<String, String> {
    serde_json::to_string_pretty(value)
        .map(|text| format!("{text}\n"))
        .map_err(|error| format!("planned JSON did not encode: {error}"))
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn remove_unchanged_writes(writes: &mut Vec<PlannedClientWrite>) -> Result<(), String> {
    let mut unchanged = BTreeSet::new();
    for write in writes.iter() {
        let Some(expected) = write.expected_before_hash.as_ref() else {
            continue;
        };
        let bytes = std::fs::read(&write.path).map_err(|error| {
            format!(
                "planned write target '{}' changed before final normalization: {error}",
                write.path.display()
            )
        })?;
        if sha256_hex(&bytes) != *expected {
            return Err(format!(
                "planned write target '{}' changed before final normalization",
                write.path.display()
            ));
        }
        let same_json = match (
            serde_json::from_slice::<serde_json::Value>(&bytes),
            serde_json::from_str::<serde_json::Value>(&write.content),
        ) {
            (Ok(current), Ok(planned)) => current == planned,
            _ => false,
        };
        let same_policy = if write.tag.starts_with("policy:") {
            let current: WorkerInterfacePolicy = serde_json::from_slice(&bytes)
                .map_err(|e| format!("existing worker ceiling is malformed: {e}"))?;
            let planned: WorkerInterfacePolicy = serde_json::from_str(&write.content)
                .map_err(|e| format!("planned worker ceiling is malformed: {e}"))?;
            current.id == planned.id
                && current.role_artifacts == planned.role_artifacts
                && current.ceiling.interface_policy == planned.ceiling.interface_policy
                && current.ceiling_grant() == planned.ceiling_grant()
        } else {
            false
        };
        if bytes == write.content.as_bytes() || same_json || same_policy {
            unchanged.insert(write.path.clone());
        }
    }
    writes.retain(|write| !unchanged.contains(&write.path));
    Ok(())
}

pub(crate) fn normalize_write_hash_algorithm(
    writes: &mut [PlannedClientWrite],
    algorithm: WriteHashAlgorithm,
) -> Result<(), String> {
    if algorithm == WriteHashAlgorithm::Sha256 {
        return Ok(());
    }
    for write in writes {
        let Some(expected) = write.expected_before_hash.as_mut() else {
            continue;
        };
        let bytes = std::fs::read(&write.path).map_err(|error| {
            format!(
                "planned write target '{}' changed before BLAKE3 formation: {error}",
                write.path.display()
            )
        })?;
        let observed_sha256 = sha256_hex(&bytes);
        if observed_sha256 != *expected {
            return Err(format!(
                "planned write target '{}' changed before BLAKE3 formation",
                write.path.display()
            ));
        }
        *expected = blake3::hash(&bytes).to_hex().to_string();
    }
    Ok(())
}
