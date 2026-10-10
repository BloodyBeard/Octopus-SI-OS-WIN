use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const INSTALL_MANIFEST_FILE: &str = ".octopus-source-manifest.json";
const SOURCE_IDENTITY_FILE: &str = ".octopus-source.json";
const INSTALL_MANIFEST_VERSION: u32 = 2;
const SOURCE_IDENTITY_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstalledFile {
    sha256: String,
    mode: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallManifest {
    schema_version: u32,
    source_id: String,
    source_registry: PathBuf,
    files: BTreeMap<String, InstalledFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyInstallManifest {
    schema_version: u32,
    source_registry: PathBuf,
    files: BTreeMap<String, InstalledFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceIdentity {
    schema_version: u32,
    id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledRegistry {
    pub root: PathBuf,
    pub copied: usize,
    pub updated: usize,
    pub removed: usize,
    pub preserved_local: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistrySyncProgress<'a> {
    Scanned {
        files: usize,
    },
    Applying {
        done: usize,
        total: usize,
        relative: &'a str,
    },
}

#[derive(Debug)]
pub enum RegistryInstallError {
    SourceRoot {
        path: PathBuf,
        source: std::io::Error,
    },
    ActiveRoot {
        path: PathBuf,
        source: std::io::Error,
    },
    SameRoot {
        path: PathBuf,
    },
    OverlappingRoots {
        source_registry: PathBuf,
        active_registry: PathBuf,
    },
    ReadDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    EntryType {
        path: PathBuf,
        source: std::io::Error,
    },
    UnsupportedEntry {
        path: PathBuf,
    },
    ReservedSourcePath {
        path: PathBuf,
    },
    UnsafeManifestPath {
        path: String,
    },
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    ManifestParse {
        path: PathBuf,
        source: serde_json::Error,
    },
    ManifestVersion {
        path: PathBuf,
        version: u32,
    },
    SourceIdentity {
        path: PathBuf,
        reason: String,
    },
    SourceChanged {
        manifest: PathBuf,
        expected: String,
        actual: String,
    },
    Conflict {
        path: PathBuf,
        reason: String,
    },
    SourceFileChanged {
        path: PathBuf,
        expected_sha256: String,
        expected_mode: Option<u32>,
        actual_sha256: String,
        actual_mode: Option<u32>,
    },
    WriteFile {
        path: PathBuf,
        source: std::io::Error,
    },
    RemoveFile {
        path: PathBuf,
        source: std::io::Error,
    },
    EncodeManifest {
        path: PathBuf,
        source: serde_json::Error,
    },
}

impl std::fmt::Display for RegistryInstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SourceRoot { path, source } => write!(
                f,
                "source registry '{}' cannot be canonicalized: {source}",
                path.display()
            ),
            Self::ActiveRoot { path, source } => write!(
                f,
                "active registry root '{}' cannot be prepared: {source}",
                path.display()
            ),
            Self::SameRoot { path } => write!(
                f,
                "source and active registries resolve to the same path '{}'; the source catalog must remain read-only",
                path.display()
            ),
            Self::OverlappingRoots {
                source_registry,
                active_registry,
            } => write!(
                f,
                "source registry '{}' and active registry '{}' overlap; neither registry may contain the other",
                source_registry.display(),
                active_registry.display()
            ),
            Self::ReadDirectory { path, source } => write!(
                f,
                "registry directory '{}' cannot be read: {source}",
                path.display()
            ),
            Self::EntryType { path, source } => write!(
                f,
                "registry entry '{}' cannot be inspected: {source}",
                path.display()
            ),
            Self::UnsupportedEntry { path } => write!(
                f,
                "registry entry '{}' is neither a regular file nor a directory; links and special files are not installable",
                path.display()
            ),
            Self::ReservedSourcePath { path } => write!(
                f,
                "source registry contains reserved installer path '{}'",
                path.display()
            ),
            Self::UnsafeManifestPath { path } => {
                write!(
                    f,
                    "registry install manifest contains unsafe relative path '{path}'"
                )
            }
            Self::ReadFile { path, source } => {
                write!(
                    f,
                    "registry file '{}' cannot be read: {source}",
                    path.display()
                )
            }
            Self::ManifestParse { path, source } => write!(
                f,
                "registry install manifest '{}' is malformed: {source}",
                path.display()
            ),
            Self::ManifestVersion { path, version } => write!(
                f,
                "registry install manifest '{}' has schema_version {version}, expected {INSTALL_MANIFEST_VERSION}",
                path.display()
            ),
            Self::SourceIdentity { path, reason } => write!(
                f,
                "registry source identity '{}' is invalid: {reason}",
                path.display()
            ),
            Self::SourceChanged {
                manifest,
                expected,
                actual,
            } => write!(
                f,
                "registry install manifest '{}' belongs to source identity '{}', not '{}'; refusing an ambiguous source overlay",
                manifest.display(),
                expected,
                actual
            ),
            Self::Conflict { path, reason } => write!(
                f,
                "active registry path '{}' conflicts with the source catalog: {reason}",
                path.display()
            ),
            Self::SourceFileChanged {
                path,
                expected_sha256,
                expected_mode,
                actual_sha256,
                actual_mode,
            } => write!(
                f,
                "source registry file '{}' changed during installation (expected sha256 {} mode {:?}, read sha256 {} mode {:?})",
                path.display(),
                expected_sha256,
                expected_mode,
                actual_sha256,
                actual_mode
            ),
            Self::WriteFile { path, source } => write!(
                f,
                "registry installer cannot write '{}': {source}",
                path.display()
            ),
            Self::RemoveFile { path, source } => write!(
                f,
                "registry installer cannot remove '{}': {source}",
                path.display()
            ),
            Self::EncodeManifest { path, source } => write!(
                f,
                "registry install manifest '{}' cannot be encoded: {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for RegistryInstallError {}

#[derive(Debug)]
enum TargetEntry {
    Missing,
    File(InstalledFile),
    Directory,
    Unsupported,
}

#[derive(Debug)]
enum InstallOperation {
    Copy { relative: String, replacing: bool },
    Remove { relative: String },
}

#[derive(Debug)]
enum PreparedInstallOperation {
    Copy {
        relative: String,
        replacing: bool,
        staged: PathBuf,
    },
    Remove {
        relative: String,
    },
}

impl Drop for PreparedInstallOperation {
    fn drop(&mut self) {
        if let Self::Copy { staged, .. } = self {
            let _ = std::fs::remove_file(staged);
        }
    }
}

pub fn install_registry(
    source_registry: &Path,
    active_registry: &Path,
) -> Result<InstalledRegistry, RegistryInstallError> {
    install_registry_with_progress(source_registry, active_registry, &mut |_| {})
}

pub fn install_registry_with_progress(
    source_registry: &Path,
    active_registry: &Path,
    progress: &mut dyn FnMut(RegistrySyncProgress<'_>),
) -> Result<InstalledRegistry, RegistryInstallError> {
    let source_registry = std::fs::canonicalize(source_registry).map_err(|source| {
        RegistryInstallError::SourceRoot {
            path: source_registry.to_path_buf(),
            source,
        }
    })?;
    if !source_registry.is_dir() {
        return Err(RegistryInstallError::UnsupportedEntry {
            path: source_registry,
        });
    }
    std::fs::create_dir_all(active_registry).map_err(|source| {
        RegistryInstallError::ActiveRoot {
            path: active_registry.to_path_buf(),
            source,
        }
    })?;
    let active_registry = std::fs::canonicalize(active_registry).map_err(|source| {
        RegistryInstallError::ActiveRoot {
            path: active_registry.to_path_buf(),
            source,
        }
    })?;
    if source_registry == active_registry {
        return Err(RegistryInstallError::SameRoot {
            path: source_registry,
        });
    }
    if source_registry.starts_with(&active_registry)
        || active_registry.starts_with(&source_registry)
    {
        return Err(RegistryInstallError::OverlappingRoots {
            source_registry,
            active_registry,
        });
    }

    validate_tree_entries(&active_registry, &active_registry, true)?;
    let source_id = load_source_identity(&source_registry)?;
    let source_files = collect_source_files(&source_registry)?;
    let source_kind_roots = collect_source_kind_roots(&source_registry)?;
    progress(RegistrySyncProgress::Scanned {
        files: source_files.len(),
    });
    let manifest_path = active_registry.join(INSTALL_MANIFEST_FILE);
    let previous = load_manifest(&manifest_path)?;
    if let Some(previous) = &previous {
        if !previous.source_id.is_empty() && previous.source_id != source_id {
            return Err(RegistryInstallError::SourceChanged {
                manifest: manifest_path,
                expected: previous.source_id.clone(),
                actual: source_id,
            });
        }
    }

    let previous_files = previous
        .as_ref()
        .map(|manifest| &manifest.files)
        .cloned()
        .unwrap_or_default();
    for relative in previous_files.keys() {
        safe_relative_path(relative)?;
    }
    let mut operations = Vec::new();
    let mut preserved_local = 0usize;

    for (relative, source_file) in &source_files {
        let target = active_registry.join(safe_relative_path(relative)?);
        let target_entry = inspect_target(&target)?;
        match previous_files.get(relative) {
            None => match target_entry {
                TargetEntry::Missing => operations.push(InstallOperation::Copy {
                    relative: relative.clone(),
                    replacing: false,
                }),
                TargetEntry::File(existing) if existing == *source_file => {}
                TargetEntry::File(_) => {
                    return Err(RegistryInstallError::Conflict {
                        path: target,
                        reason: "a source file is new, but an unrelated active-local file already owns the same identity".to_string(),
                    });
                }
                TargetEntry::Directory | TargetEntry::Unsupported => {
                    return Err(RegistryInstallError::Conflict {
                        path: target,
                        reason: "a source file collides with a non-file active entry".to_string(),
                    });
                }
            },
            Some(previous_file) if previous_file == source_file => match target_entry {
                TargetEntry::Missing => preserved_local += 1,
                TargetEntry::File(existing) if existing == *source_file => {}
                TargetEntry::File(_) => preserved_local += 1,
                TargetEntry::Directory | TargetEntry::Unsupported => {
                    return Err(RegistryInstallError::Conflict {
                        path: target,
                        reason: "a source-owned file was replaced by a non-file active entry"
                            .to_string(),
                    });
                }
            },
            Some(previous_file) => {
                match target_entry {
                    TargetEntry::File(existing) if existing == *previous_file => {
                        operations.push(InstallOperation::Copy {
                            relative: relative.clone(),
                            replacing: true,
                        });
                    }
                    TargetEntry::File(existing) if existing == *source_file => {}
                    TargetEntry::Missing => {
                        return Err(RegistryInstallError::Conflict {
                            path: target,
                            reason: "the source changed after the active copy was locally deleted"
                                .to_string(),
                        });
                    }
                    TargetEntry::File(_) => {
                        return Err(RegistryInstallError::Conflict {
                        path: target,
                        reason: "both the source catalog and active copy changed since the last install".to_string(),
                    });
                    }
                    TargetEntry::Directory | TargetEntry::Unsupported => {
                        return Err(RegistryInstallError::Conflict {
                            path: target,
                            reason: "a changed source file collides with a non-file active entry"
                                .to_string(),
                        });
                    }
                }
            }
        }
    }

    for (relative, previous_file) in &previous_files {
        if source_files.contains_key(relative) {
            continue;
        }
        let target = active_registry.join(safe_relative_path(relative)?);
        match inspect_target(&target)? {
            TargetEntry::Missing => {}
            TargetEntry::File(existing) if existing == *previous_file => {
                operations.push(InstallOperation::Remove {
                    relative: relative.clone(),
                });
            }
            TargetEntry::File(_) => {
                return Err(RegistryInstallError::Conflict {
                    path: target,
                    reason:
                        "the source removed this file after the active copy was locally changed"
                            .to_string(),
                });
            }
            TargetEntry::Directory | TargetEntry::Unsupported => {
                return Err(RegistryInstallError::Conflict {
                    path: target,
                    reason: "a removed source file was replaced by a non-file active entry"
                        .to_string(),
                });
            }
        }
    }

    let mut prepared = Vec::with_capacity(operations.len());
    for operation in operations {
        let result = match operation {
            InstallOperation::Copy {
                relative,
                replacing,
            } => {
                let relative_path = safe_relative_path(&relative)?;
                let expected =
                    source_files
                        .get(&relative)
                        .ok_or_else(|| RegistryInstallError::Conflict {
                            path: source_registry.join(&relative_path),
                            reason: "a planned source copy disappeared from the source snapshot"
                                .to_string(),
                        })?;
                prepare_copy(
                    &source_registry.join(&relative_path),
                    &active_registry.join(&relative_path),
                    expected,
                )
                .map(|staged| PreparedInstallOperation::Copy {
                    relative,
                    replacing,
                    staged,
                })
            }
            InstallOperation::Remove { relative } => {
                Ok(PreparedInstallOperation::Remove { relative })
            }
        };
        match result {
            Ok(operation) => prepared.push(operation),
            Err(error) => return Err(error),
        }
    }

    let mut copied = 0usize;
    let mut updated = 0usize;
    let mut removed = 0usize;
    materialize_source_kind_roots(&active_registry, &source_kind_roots)?;
    let total_operations = prepared.len();
    for (index, operation) in prepared.iter().enumerate() {
        let relative = match operation {
            PreparedInstallOperation::Copy {
                relative,
                replacing,
                staged,
            } => {
                let target = active_registry.join(safe_relative_path(relative)?);
                commit_prepared_copy(staged, &target)?;
                if *replacing {
                    updated += 1;
                } else {
                    copied += 1;
                }
                relative
            }
            PreparedInstallOperation::Remove { relative } => {
                let target = active_registry.join(safe_relative_path(relative)?);
                remove_installed_file(&target)?;
                removed += 1;
                relative
            }
        };
        progress(RegistrySyncProgress::Applying {
            done: index + 1,
            total: total_operations,
            relative,
        });
    }

    let manifest = InstallManifest {
        schema_version: INSTALL_MANIFEST_VERSION,
        source_id,
        source_registry,
        files: source_files,
    };
    write_manifest(&manifest_path, &manifest)?;
    Ok(InstalledRegistry {
        root: active_registry,
        copied,
        updated,
        removed,
        preserved_local,
    })
}

fn collect_source_kind_roots(source_registry: &Path) -> Result<Vec<PathBuf>, RegistryInstallError> {
    let mut entries = std::fs::read_dir(source_registry)
        .map_err(|source| RegistryInstallError::ReadDirectory {
            path: source_registry.to_path_buf(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| RegistryInstallError::ReadDirectory {
            path: source_registry.to_path_buf(),
            source,
        })?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut roots = Vec::new();
    for entry in entries {
        let path = entry.path();
        if entry
            .file_type()
            .map_err(|source| RegistryInstallError::EntryType {
                path: path.clone(),
                source,
            })?
            .is_dir()
        {
            roots.push(PathBuf::from(entry.file_name()));
        }
    }
    Ok(roots)
}

fn materialize_source_kind_roots(
    active_registry: &Path,
    roots: &[PathBuf],
) -> Result<(), RegistryInstallError> {
    for root in roots {
        let path = active_registry.join(root);
        std::fs::create_dir_all(&path)
            .map_err(|source| RegistryInstallError::ActiveRoot { path, source })?;
    }
    sync_directory(active_registry)
}

fn collect_source_files(
    source_registry: &Path,
) -> Result<BTreeMap<String, InstalledFile>, RegistryInstallError> {
    let mut files = BTreeMap::new();
    collect_source_directory(source_registry, source_registry, &mut files)?;
    Ok(files)
}

fn collect_source_directory(
    source_registry: &Path,
    directory: &Path,
    files: &mut BTreeMap<String, InstalledFile>,
) -> Result<(), RegistryInstallError> {
    let mut entries = std::fs::read_dir(directory)
        .map_err(|source| RegistryInstallError::ReadDirectory {
            path: directory.to_path_buf(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| RegistryInstallError::ReadDirectory {
            path: directory.to_path_buf(),
            source,
        })?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source| RegistryInstallError::EntryType {
                path: path.clone(),
                source,
            })?;
        if file_type.is_dir() {
            collect_source_directory(source_registry, &path, files)?;
            continue;
        }
        if !file_type.is_file() {
            return Err(RegistryInstallError::UnsupportedEntry { path });
        }
        let relative = relative_key(source_registry, &path)?;
        if relative == INSTALL_MANIFEST_FILE {
            return Err(RegistryInstallError::ReservedSourcePath { path });
        }
        if relative == SOURCE_IDENTITY_FILE {
            continue;
        }
        files.insert(relative, digest_file(&path)?);
    }
    Ok(())
}

fn validate_tree_entries(
    root: &Path,
    directory: &Path,
    allow_manifest: bool,
) -> Result<(), RegistryInstallError> {
    let entries =
        std::fs::read_dir(directory).map_err(|source| RegistryInstallError::ReadDirectory {
            path: directory.to_path_buf(),
            source,
        })?;
    for entry in entries {
        let entry = entry.map_err(|source| RegistryInstallError::ReadDirectory {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source| RegistryInstallError::EntryType {
                path: path.clone(),
                source,
            })?;
        if file_type.is_dir() {
            validate_tree_entries(root, &path, allow_manifest)?;
        } else if file_type.is_file() {
            let relative = relative_key(root, &path)?;
            if !allow_manifest && relative == INSTALL_MANIFEST_FILE {
                return Err(RegistryInstallError::ReservedSourcePath { path });
            }
        } else {
            return Err(RegistryInstallError::UnsupportedEntry { path });
        }
    }
    Ok(())
}

fn relative_key(root: &Path, path: &Path) -> Result<String, RegistryInstallError> {
    let relative =
        path.strip_prefix(root)
            .map_err(|_| RegistryInstallError::UnsafeManifestPath {
                path: path.display().to_string(),
            })?;
    let mut parts = Vec::new();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err(RegistryInstallError::UnsafeManifestPath {
                path: relative.display().to_string(),
            });
        };
        let part = part
            .to_str()
            .ok_or_else(|| RegistryInstallError::UnsafeManifestPath {
                path: relative.display().to_string(),
            })?;
        parts.push(part);
    }
    if parts.is_empty() {
        return Err(RegistryInstallError::UnsafeManifestPath {
            path: relative.display().to_string(),
        });
    }
    Ok(parts.join("/"))
}

fn safe_relative_path(relative: &str) -> Result<PathBuf, RegistryInstallError> {
    let path = Path::new(relative);
    if path.is_absolute() || relative.is_empty() {
        return Err(RegistryInstallError::UnsafeManifestPath {
            path: relative.to_string(),
        });
    }
    let mut output = PathBuf::new();
    for component in path.components() {
        let Component::Normal(part) = component else {
            return Err(RegistryInstallError::UnsafeManifestPath {
                path: relative.to_string(),
            });
        };
        output.push(part);
    }
    if output.as_os_str().is_empty() {
        return Err(RegistryInstallError::UnsafeManifestPath {
            path: relative.to_string(),
        });
    }
    Ok(output)
}

fn inspect_target(path: &Path) -> Result<TargetEntry, RegistryInstallError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(TargetEntry::File(digest_file(path)?)),
        Ok(metadata) if metadata.file_type().is_dir() => Ok(TargetEntry::Directory),
        Ok(_) => Ok(TargetEntry::Unsupported),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(TargetEntry::Missing),
        Err(source) => Err(RegistryInstallError::EntryType {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn digest_file(path: &Path) -> Result<InstalledFile, RegistryInstallError> {
    let mut file = File::open(path).map_err(|source| RegistryInstallError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = file
        .metadata()
        .map_err(|source| RegistryInstallError::EntryType {
            path: path.to_path_buf(),
            source,
        })?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| RegistryInstallError::ReadFile {
                path: path.to_path_buf(),
                source,
            })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let sha256 = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(InstalledFile {
        sha256,
        mode: file_mode(&metadata),
    })
}

#[cfg(unix)]
fn file_mode(metadata: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(metadata.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn file_mode(_metadata: &std::fs::Metadata) -> Option<u32> {
    None
}

fn load_manifest(path: &Path) -> Result<Option<InstallManifest>, RegistryInstallError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(RegistryInstallError::ReadFile {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|source| RegistryInstallError::ManifestParse {
            path: path.to_path_buf(),
            source,
        })?;
    let version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(0);
    match version {
        INSTALL_MANIFEST_VERSION => serde_json::from_value(value).map(Some).map_err(|source| {
            RegistryInstallError::ManifestParse {
                path: path.to_path_buf(),
                source,
            }
        }),
        1 => {
            let legacy: LegacyInstallManifest =
                serde_json::from_value(value).map_err(|source| {
                    RegistryInstallError::ManifestParse {
                        path: path.to_path_buf(),
                        source,
                    }
                })?;
            Ok(Some(InstallManifest {
                schema_version: INSTALL_MANIFEST_VERSION,
                source_id: String::new(),
                source_registry: legacy.source_registry,
                files: legacy.files,
            }))
        }
        version => Err(RegistryInstallError::ManifestVersion {
            path: path.to_path_buf(),
            version,
        }),
    }
}

fn load_source_identity(source_registry: &Path) -> Result<String, RegistryInstallError> {
    let path = source_registry.join(SOURCE_IDENTITY_FILE);
    let bytes = std::fs::read(&path).map_err(|source| RegistryInstallError::ReadFile {
        path: path.clone(),
        source,
    })?;
    let identity: SourceIdentity =
        serde_json::from_slice(&bytes).map_err(|source| RegistryInstallError::ManifestParse {
            path: path.clone(),
            source,
        })?;
    if identity.schema_version != SOURCE_IDENTITY_VERSION {
        return Err(RegistryInstallError::SourceIdentity {
            path,
            reason: format!(
                "schema_version {} does not equal {}",
                identity.schema_version, SOURCE_IDENTITY_VERSION
            ),
        });
    }
    if identity.id.is_empty()
        || identity.id.trim() != identity.id
        || !identity
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(RegistryInstallError::SourceIdentity {
            path,
            reason: "id must be a non-empty portable identifier".to_string(),
        });
    }
    Ok(identity.id)
}

fn prepare_copy(
    source: &Path,
    target: &Path,
    expected: &InstalledFile,
) -> Result<PathBuf, RegistryInstallError> {
    let parent = target
        .parent()
        .ok_or_else(|| RegistryInstallError::WriteFile {
            path: target.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "target has no parent directory",
            ),
        })?;
    let mut source_file =
        File::open(source).map_err(|source_error| RegistryInstallError::ReadFile {
            path: source.to_path_buf(),
            source: source_error,
        })?;
    let source_metadata =
        source_file
            .metadata()
            .map_err(|source_error| RegistryInstallError::EntryType {
                path: source.to_path_buf(),
                source: source_error,
            })?;
    let mut bytes = Vec::new();
    source_file
        .read_to_end(&mut bytes)
        .map_err(|source_error| RegistryInstallError::ReadFile {
            path: source.to_path_buf(),
            source: source_error,
        })?;
    let actual = installed_bytes(&bytes, &source_metadata);
    if &actual != expected {
        return Err(RegistryInstallError::SourceFileChanged {
            path: source.to_path_buf(),
            expected_sha256: expected.sha256.clone(),
            expected_mode: expected.mode,
            actual_sha256: actual.sha256,
            actual_mode: actual.mode,
        });
    }
    std::fs::create_dir_all(parent).map_err(|source| RegistryInstallError::WriteFile {
        path: parent.to_path_buf(),
        source,
    })?;
    let temp = temp_path(target);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|source| RegistryInstallError::WriteFile {
                path: temp.clone(),
                source,
            })?;
        file.write_all(&bytes)
            .map_err(|source| RegistryInstallError::WriteFile {
                path: temp.clone(),
                source,
            })?;
        apply_source_permissions(&source_metadata, &temp)?;
        file.sync_all()
            .map_err(|source| RegistryInstallError::WriteFile {
                path: temp.clone(),
                source,
            })?;
        Ok(temp.clone())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(unix)]
fn apply_source_permissions(
    source_metadata: &std::fs::Metadata,
    target: &Path,
) -> Result<(), RegistryInstallError> {
    let permissions = source_metadata.permissions();
    std::fs::set_permissions(target, permissions).map_err(|source_error| {
        RegistryInstallError::WriteFile {
            path: target.to_path_buf(),
            source: source_error,
        }
    })
}

#[cfg(not(unix))]
fn apply_source_permissions(
    _source_metadata: &std::fs::Metadata,
    _target: &Path,
) -> Result<(), RegistryInstallError> {
    Ok(())
}

fn installed_bytes(bytes: &[u8], metadata: &std::fs::Metadata) -> InstalledFile {
    let sha256 = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    InstalledFile {
        sha256,
        mode: file_mode(metadata),
    }
}

fn commit_prepared_copy(staged: &Path, target: &Path) -> Result<(), RegistryInstallError> {
    std::fs::rename(staged, target).map_err(|source| RegistryInstallError::WriteFile {
        path: target.to_path_buf(),
        source,
    })?;
    sync_parent(target)
}

fn remove_installed_file(target: &Path) -> Result<(), RegistryInstallError> {
    std::fs::remove_file(target).map_err(|source| RegistryInstallError::RemoveFile {
        path: target.to_path_buf(),
        source,
    })?;
    sync_parent(target)
}

fn sync_parent(path: &Path) -> Result<(), RegistryInstallError> {
    let parent = path
        .parent()
        .ok_or_else(|| RegistryInstallError::WriteFile {
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path has no parent directory",
            ),
        })?;
    sync_directory(parent)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), RegistryInstallError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| RegistryInstallError::WriteFile {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), RegistryInstallError> {
    Ok(())
}

fn write_manifest(path: &Path, manifest: &InstallManifest) -> Result<(), RegistryInstallError> {
    let mut bytes = serde_json::to_vec_pretty(manifest).map_err(|source| {
        RegistryInstallError::EncodeManifest {
            path: path.to_path_buf(),
            source,
        }
    })?;
    bytes.push(b'\n');
    let temp = temp_path(path);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|source| RegistryInstallError::WriteFile {
                path: temp.clone(),
                source,
            })?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|source| RegistryInstallError::WriteFile {
                path: temp.clone(),
                source,
            })?;
        std::fs::rename(&temp, path).map_err(|source| RegistryInstallError::WriteFile {
            path: path.to_path_buf(),
            source,
        })?;
        sync_parent(path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("registry-install");
    path.with_file_name(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    fn prepare_source(path: &Path, id: &str) {
        write(
            &path.join(SOURCE_IDENTITY_FILE),
            &format!(r#"{{"schema_version":1,"id":"{id}"}}"#),
        );
    }

    #[test]
    fn install_materializes_empty_registry_kind_roots() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let active = temp.path().join("home/registry");
        prepare_source(&source, "test-baseline");
        for kind in ["artifact", "interface", "tool", "worker_environment_kind"] {
            std::fs::create_dir_all(source.join(kind)).unwrap();
        }

        let installed = install_registry(&source, &active).unwrap();

        assert_eq!(installed.copied, 0);
        for kind in ["artifact", "interface", "tool", "worker_environment_kind"] {
            assert!(active.join(kind).is_dir());
        }
    }

    #[test]
    fn install_updates_source_owned_files_and_preserves_private_additions() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let active = temp.path().join("home/registry");
        prepare_source(&source, "test-baseline");
        write(&source.join("interface/base/interface.json"), "source-v1");

        let first = install_registry(&source, &active).unwrap();
        assert_eq!(first.copied, 1);
        write(&active.join("interface/private/interface.json"), "private");
        write(&source.join("interface/base/interface.json"), "source-v2");

        let second = install_registry(&source, &active).unwrap();
        assert_eq!(second.updated, 1);
        assert_eq!(
            std::fs::read_to_string(active.join("interface/base/interface.json")).unwrap(),
            "source-v2"
        );
        assert_eq!(
            std::fs::read_to_string(active.join("interface/private/interface.json")).unwrap(),
            "private"
        );
    }

    #[test]
    fn install_preserves_local_override_until_source_changes_then_refuses() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let active = temp.path().join("home/registry");
        let relative = Path::new("interface/base/interface.json");
        prepare_source(&source, "test-baseline");
        write(&source.join(relative), "source-v1");
        install_registry(&source, &active).unwrap();
        write(&active.join(relative), "active-local");

        let preserved = install_registry(&source, &active).unwrap();
        assert_eq!(preserved.preserved_local, 1);
        assert_eq!(
            std::fs::read_to_string(active.join(relative)).unwrap(),
            "active-local"
        );

        write(&source.join(relative), "source-v2");
        let error = install_registry(&source, &active).unwrap_err();
        assert!(matches!(error, RegistryInstallError::Conflict { .. }));
        assert_eq!(
            std::fs::read_to_string(active.join(relative)).unwrap(),
            "active-local"
        );
    }

    #[test]
    fn install_preflights_all_conflicts_before_mutating_any_file() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let active = temp.path().join("home/registry");
        prepare_source(&source, "test-baseline");
        write(&source.join("tool/a/tool.json"), "a1");
        write(&source.join("tool/z/tool.json"), "z1");
        install_registry(&source, &active).unwrap();
        write(&active.join("tool/z/tool.json"), "z-local");
        write(&source.join("tool/a/tool.json"), "a2");
        write(&source.join("tool/z/tool.json"), "z2");

        let error = install_registry(&source, &active).unwrap_err();
        assert!(matches!(error, RegistryInstallError::Conflict { .. }));
        assert_eq!(
            std::fs::read_to_string(active.join("tool/a/tool.json")).unwrap(),
            "a1"
        );
    }

    #[test]
    fn install_refuses_registry_roots_nested_in_either_direction() {
        let first = tempfile::tempdir().unwrap();
        let source = first.path().join("source");
        prepare_source(&source, "test-baseline");
        write(&source.join("interface/base/interface.json"), "source");
        let active_inside_source = source.join("active");
        let error = install_registry(&source, &active_inside_source).unwrap_err();
        assert!(matches!(
            error,
            RegistryInstallError::OverlappingRoots { .. }
        ));

        let second = tempfile::tempdir().unwrap();
        let active = second.path().join("active");
        let source_inside_active = active.join("source");
        prepare_source(&source_inside_active, "test-baseline");
        write(
            &source_inside_active.join("interface/base/interface.json"),
            "source",
        );
        let error = install_registry(&source_inside_active, &active).unwrap_err();
        assert!(matches!(
            error,
            RegistryInstallError::OverlappingRoots { .. }
        ));
    }

    #[test]
    fn prepared_copy_refuses_source_drift_without_touching_active_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source/interface.json");
        let target = temp.path().join("active/interface.json");
        write(&source, "source-v1");
        write(&target, "active-local");
        let expected = digest_file(&source).unwrap();
        write(&source, "source-v2");

        let error = prepare_copy(&source, &target, &expected).unwrap_err();
        assert!(matches!(
            error,
            RegistryInstallError::SourceFileChanged { .. }
        ));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "active-local");
        assert_eq!(
            std::fs::read_dir(target.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn install_accepts_a_relocated_source_with_the_same_identity() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first/registry");
        let second = temp.path().join("second/registry");
        let active = temp.path().join("home/registry");
        prepare_source(&first, "release-baseline");
        prepare_source(&second, "release-baseline");
        write(&first.join("interface/base/interface.json"), "v1");
        write(&second.join("interface/base/interface.json"), "v2");

        install_registry(&first, &active).unwrap();
        let installed = install_registry(&second, &active).unwrap();

        assert_eq!(installed.updated, 1);
        assert_eq!(
            std::fs::read_to_string(active.join("interface/base/interface.json")).unwrap(),
            "v2"
        );
    }

    #[test]
    fn install_refuses_a_different_source_identity() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first/registry");
        let second = temp.path().join("second/registry");
        let active = temp.path().join("home/registry");
        prepare_source(&first, "release-baseline");
        prepare_source(&second, "different-baseline");
        write(&first.join("interface/base/interface.json"), "v1");
        write(&second.join("interface/base/interface.json"), "v1");

        install_registry(&first, &active).unwrap();
        let error = install_registry(&second, &active).unwrap_err();

        assert!(matches!(error, RegistryInstallError::SourceChanged { .. }));
    }
}
