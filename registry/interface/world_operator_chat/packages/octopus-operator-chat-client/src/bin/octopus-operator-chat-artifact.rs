use std::io::Read;
use std::path::{Path, PathBuf};

use octopus_interfaces::InterfaceArtifactManifest;
use octopus_ledger::ContentHash;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    blobs_root: PathBuf,
    label: String,
    payload: Value,
}

#[derive(Serialize)]
struct Response {
    artifact_id: String,
    manifest_hash: String,
    payload_hash: String,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("operator chat artifact materialization failed closed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| format!("stdin read failed: {error}"))?;
    let request: Request =
        serde_json::from_str(&input).map_err(|error| format!("request is malformed: {error}"))?;
    if !request.blobs_root.is_absolute() {
        return Err("blobs_root must be absolute".to_string());
    }
    if request.label.is_empty() || request.label.trim() != request.label {
        return Err("label must be a non-empty trimmed string".to_string());
    }
    std::fs::create_dir_all(&request.blobs_root).map_err(|error| {
        format!(
            "blob root '{}' could not be created: {error}",
            request.blobs_root.display()
        )
    })?;
    let blobs_root = std::fs::canonicalize(&request.blobs_root).map_err(|error| {
        format!(
            "blob root '{}' could not be canonicalized: {error}",
            request.blobs_root.display()
        )
    })?;
    let payload = serde_json::to_vec(&request.payload)
        .map_err(|error| format!("payload did not encode: {error}"))?;
    let payload_hash = ContentHash::of_bytes(&payload).to_hex();
    write_content_addressed(&blobs_root, &payload_hash, &payload)?;
    let manifest = serde_json::to_vec(&InterfaceArtifactManifest {
        label: request.label.clone(),
        payload_path: blobs_root.join(&payload_hash),
        payload_content_hash: Some(payload_hash.clone()),
    })
    .map_err(|error| format!("manifest did not encode: {error}"))?;
    let manifest_hash = ContentHash::of_bytes(&manifest).to_hex();
    write_content_addressed(&blobs_root, &manifest_hash, &manifest)?;
    println!(
        "{}",
        serde_json::to_string(&Response {
            artifact_id: format!("{}:{manifest_hash}", request.label),
            manifest_hash,
            payload_hash,
        })
        .map_err(|error| format!("response did not encode: {error}"))?
    );
    Ok(())
}

fn write_content_addressed(root: &Path, hash: &str, bytes: &[u8]) -> Result<(), String> {
    let path = root.join(hash);
    if path.exists() {
        return verify_blob(&path, bytes);
    }
    let temporary = root.join(format!(".{hash}.{}.tmp", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| {
            format!(
                "blob staging '{}' create failed: {error}",
                temporary.display()
            )
        })?;
    let staged = (|| {
        std::io::Write::write_all(&mut file, bytes).map_err(|error| {
            format!(
                "blob staging '{}' write failed: {error}",
                temporary.display()
            )
        })?;
        file.sync_all().map_err(|error| {
            format!(
                "blob staging '{}' sync failed: {error}",
                temporary.display()
            )
        })?;
        match std::fs::hard_link(&temporary, &path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                verify_blob(&path, bytes)
            }
            Err(error) => Err(format!("blob '{}' publish failed: {error}", path.display())),
        }
    })();
    drop(file);
    let cleanup = std::fs::remove_file(&temporary);
    staged?;
    cleanup.map_err(|error| {
        format!(
            "blob staging '{}' cleanup failed: {error}",
            temporary.display()
        )
    })
}

fn verify_blob(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let existing = std::fs::read(path)
        .map_err(|error| format!("blob '{}' read failed: {error}", path.display()))?;
    if existing != bytes {
        return Err(format!(
            "blob '{}' does not match its content hash",
            path.display()
        ));
    }
    Ok(())
}
