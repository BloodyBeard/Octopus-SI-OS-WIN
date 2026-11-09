use octopus_file_storage::metadata::FileStamp;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use octopus_descriptors::ArtifactId;
use octopus_interfaces::InterfaceArtifactManifest;
use octopus_ledger::{ContentHash, SharedBlobs};

#[derive(Clone, Debug)]
struct CachedArtifact {
    fingerprint: FileStamp,
    settled: bool,
    content: ContentHash,
}

pub(crate) struct RegistryContextArtifactCache {
    artifact_root: PathBuf,
    blobs: SharedBlobs,
    entries: Mutex<BTreeMap<ArtifactId, CachedArtifact>>,
}

impl RegistryContextArtifactCache {
    pub(crate) fn new(registry_root: PathBuf, blobs: SharedBlobs) -> Result<Self, String> {
        let artifact_root =
            std::fs::canonicalize(registry_root.join("artifact")).map_err(|error| {
                format!(
                    "context artifact registry root '{}' cannot be canonicalized: {error}",
                    registry_root.join("artifact").display()
                )
            })?;
        Ok(Self {
            artifact_root,
            blobs,
            entries: Mutex::new(BTreeMap::new()),
        })
    }

    pub(crate) fn resolve(&self, id: &ArtifactId) -> Result<ContentHash, String> {
        if let Some((label, hash)) = content_hash_identity(id) {
            let manifest_bytes = self
                .blobs
                .lock()
                .map_err(|_| {
                    "blob store lock is poisoned during content-addressed artifact lookup"
                        .to_string()
                })?
                .get(&hash)
                .ok_or_else(|| {
                    format!(
                        "content-addressed artifact '{}' names absent blob '{}'",
                        id.as_str(),
                        hash.to_hex()
                    )
                })?;
            let manifest: InterfaceArtifactManifest = serde_json::from_slice(&manifest_bytes)
                .map_err(|error| {
                    format!(
                        "content-addressed artifact '{}' blob '{}' is not an interface artifact manifest: {error}",
                        id.as_str(),
                        hash.to_hex()
                    )
                })?;
            if manifest.label != label {
                return Err(format!(
                    "content-addressed artifact '{}' claims label '{}' but manifest '{}' declares '{}'",
                    id.as_str(),
                    label,
                    hash.to_hex(),
                    manifest.label
                ));
            }
            return Ok(hash);
        }
        let path = artifact_path(&self.artifact_root, id)?;
        let observed_before_read = SystemTime::now();
        let before = file_fingerprint(&path)?;
        let settled = before.is_settled(observed_before_read);
        let cached = self
            .entries
            .lock()
            .map_err(|_| "context artifact cache lock is poisoned".to_string())?
            .get(id)
            .filter(|entry| entry.settled && settled && entry.fingerprint == before)
            .cloned();
        if let Some(cached) = cached {
            let present = self
                .blobs
                .lock()
                .map_err(|_| {
                    "blob store lock is poisoned during context-artifact lookup".to_string()
                })?
                .contains(&cached.content);
            if present {
                return Ok(cached.content);
            }
        }

        let bytes = std::fs::read(&path).map_err(|error| {
            format!(
                "context artifact '{}' at '{}' cannot be read: {error}",
                id.as_str(),
                path.display()
            )
        })?;
        let artifact_json: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "context artifact '{}' at '{}' is not valid JSON: {error}",
                id.as_str(),
                path.display()
            )
        })?;
        let after = file_fingerprint(&path)?;
        if before != after {
            return Err(format!(
                "context artifact '{}' changed while '{}' was being materialized",
                id.as_str(),
                path.display()
            ));
        }
        let canonical = octopus_descriptors::canonical_bytes(&artifact_json);
        let expected = ContentHash::of_bytes(&canonical);
        let stored = self
            .blobs
            .lock()
            .map_err(|_| {
                "blob store lock is poisoned during context-artifact materialization".to_string()
            })?
            .put(&canonical);
        if stored != expected {
            return Err(format!(
                "context artifact '{}' materialized as '{}' instead of '{}'",
                id.as_str(),
                stored.to_hex(),
                expected.to_hex()
            ));
        }
        self.entries
            .lock()
            .map_err(|_| "context artifact cache lock is poisoned".to_string())?
            .insert(
                id.clone(),
                CachedArtifact {
                    fingerprint: after,
                    settled,
                    content: stored.clone(),
                },
            );
        Ok(stored)
    }
}

impl octopus_runtime::ContextArtifactResolver for RegistryContextArtifactCache {
    fn resolve_artifact_content(&self, id: &ArtifactId) -> Result<ContentHash, String> {
        self.resolve(id)
    }
}

fn content_hash_identity(id: &ArtifactId) -> Option<(&str, ContentHash)> {
    id.as_str().rsplit_once(':').and_then(|(label, hex)| {
        (!label.trim().is_empty())
            .then(|| ContentHash::from_hex(hex).map(|hash| (label, hash)))
            .flatten()
    })
}

fn artifact_path(artifact_root: &Path, id: &ArtifactId) -> Result<PathBuf, String> {
    let name = id.as_str();
    if name.trim().is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
    {
        return Err(format!("invalid context artifact id: {name:?}"));
    }
    let package = artifact_root.join(name);
    let package_metadata = std::fs::symlink_metadata(&package).map_err(|error| {
        format!(
            "context artifact '{}' package '{}' cannot be inspected: {error}",
            id.as_str(),
            package.display()
        )
    })?;
    if package_metadata.file_type().is_symlink() || !package_metadata.is_dir() {
        return Err(format!(
            "context artifact '{}' package '{}' is not a regular non-symlink directory",
            id.as_str(),
            package.display()
        ));
    }
    let path = package.join("artifact.json");
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
        format!(
            "context artifact '{}' at '{}' cannot be inspected: {error}",
            id.as_str(),
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "context artifact '{}' path '{}' is not a regular non-symlink file",
            id.as_str(),
            path.display()
        ));
    }
    Ok(path)
}

fn file_fingerprint(path: &Path) -> Result<FileStamp, String> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        format!(
            "context artifact file '{}' cannot be inspected: {error}",
            path.display()
        )
    })?;
    if !metadata.is_file() {
        return Err(format!(
            "context artifact path '{}' is not a file",
            path.display()
        ));
    }
    Ok(FileStamp::of(&metadata))
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_ledger::{BlobStore, InMemoryBlobs};
    use std::sync::Arc;

    fn temp_root() -> PathBuf {
        tempfile::tempdir().expect("unique test directory").keep()
    }

    #[test]
    fn cache_tracks_registry_file_replacement_and_fails_closed_after_removal() {
        let root = temp_root();
        let artifact_dir = root.join("artifact").join("artifact-probe");
        std::fs::create_dir_all(&artifact_dir).expect("artifact directory writes");
        let path = artifact_dir.join("artifact.json");
        std::fs::write(&path, br#"{"id":"artifact-probe","text":"one"}"#)
            .expect("first artifact writes");
        let blobs: SharedBlobs = Arc::new(Mutex::new(InMemoryBlobs::new()));
        let cache = RegistryContextArtifactCache::new(root.clone(), blobs)
            .expect("context artifact cache opens");
        let id = ArtifactId::new("artifact-probe");

        let first = cache.resolve(&id).expect("first artifact resolves");
        let repeated = cache.resolve(&id).expect("cached artifact resolves");
        assert_eq!(first, repeated);

        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, br#"{"id":"artifact-probe","text":"two"}"#)
            .expect("same-size replacement artifact writes");
        std::fs::File::open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        let replacement = cache.resolve(&id).expect("replacement artifact resolves");
        assert_ne!(first, replacement);

        std::fs::remove_file(&path).expect("artifact removes");
        assert!(cache.resolve(&id).is_err());
        std::fs::remove_dir_all(root).expect("temp root removes");
    }

    #[test]
    fn content_addressed_binding_label_must_match_its_manifest() {
        let root = temp_root();
        std::fs::create_dir_all(root.join("artifact")).expect("artifact root writes");
        let mut store = InMemoryBlobs::new();
        let manifest = br#"{"label":"payload.candidate","payload_path":"/tmp/payload.json","payload_content_hash":"1111111111111111111111111111111111111111111111111111111111111111"}"#;
        let hash = store.put(manifest);
        let blobs: SharedBlobs = Arc::new(Mutex::new(store));
        let cache = RegistryContextArtifactCache::new(root.clone(), blobs)
            .expect("context artifact cache opens");

        let exact = ArtifactId::new(format!("payload.candidate:{}", hash.to_hex()));
        assert_eq!(cache.resolve(&exact).unwrap(), hash);
        let renamed = ArtifactId::new(format!("context.reference:{}", hash.to_hex()));
        assert!(cache.resolve(&renamed).is_err());

        std::fs::remove_dir_all(root).expect("temp root removes");
    }
}
