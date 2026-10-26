use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use octopus_file_storage::publication::{self, PublicationOutcome, ReadGuard};
use serde::{Deserialize, Serialize};

pub const HOST_MANIFEST_VERSION: u32 = 1;

pub const HOST_MANIFEST_FILE_NAME: &str = "host.json";


pub const REPO_ROOT_SLOT: &str = "repo_root";
pub const WORKTREE_ROOT_SLOT: &str = "worktree_root";
pub const RUN_RECORDS_ROOT_SLOT: &str = "run_records_root";
pub const EE_INSTANCES_ROOT_SLOT: &str = "ee_instances_root";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclaredHostSlot {
    pub name: String,
    pub required: bool,
}

#[derive(Debug, Deserialize)]
struct WekSlotDeclaration {
    #[serde(default)]
    boundary_defaults: WekBoundaryDefaults,
}

#[derive(Debug, Default, Deserialize)]
struct WekBoundaryDefaults {
    #[serde(default)]
    read_root_refs: Vec<WekRootRef>,
    #[serde(default)]
    write_root_refs: Vec<WekRootRef>,
    #[serde(default)]
    protected_read_only_subpaths: Vec<WekRootRef>,
}

#[derive(Debug, Deserialize)]
struct WekRootRef {
    path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostManifest {
    pub host_manifest_version: u32,
    pub root_refs: BTreeMap<String, PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedHostRoots {
    slots: BTreeMap<String, PathBuf>,
}

impl ResolvedHostRoots {
    pub fn get(&self, slot: &str) -> Option<&Path> {
        self.slots.get(slot).map(PathBuf::as_path)
    }

    #[cfg(test)]
    fn slots(&self) -> &BTreeMap<String, PathBuf> {
        &self.slots
    }
}

#[derive(Debug)]
pub enum HostManifestError {
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    UnsupportedVersion {
        path: PathBuf,
        version: u32,
    },
    MissingSlot {
        path: PathBuf,
        slot: String,
    },
    NonAbsoluteSlot {
        path: PathBuf,
        slot: String,
        value: PathBuf,
    },
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    Canonicalize {
        path: PathBuf,
        source: std::io::Error,
    },
    WekRegistryRead {
        path: PathBuf,
        source: std::io::Error,
    },
    WekDescriptorParse {
        path: PathBuf,
        source: serde_json::Error,
    },

}

impl std::fmt::Display for HostManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(
                    f,
                    "failed to read host manifest '{}': {source}",
                    path.display()
                )
            }
            Self::Parse { path, source } => {
                write!(
                    f,
                    "host manifest '{}' is invalid JSON: {source}",
                    path.display()
                )
            }
            Self::UnsupportedVersion { path, version } => write!(
                f,
                "host manifest '{}' has unsupported host_manifest_version {version} (this build \
                 speaks version {HOST_MANIFEST_VERSION})",
                path.display()
            ),
            Self::MissingSlot { path, slot } => write!(
                f,
                "host manifest '{}' does not bind the required ref-slot '{slot}' declared by a \
                 worker_environment_kind descriptor",
                path.display()
            ),
            Self::NonAbsoluteSlot { path, slot, value } => write!(
                f,
                "host manifest '{}' binds ref-slot '{slot}' to a non-absolute path '{}' (host \
                 root-refs must be absolute)",
                path.display(),
                value.display()
            ),
            Self::Write { path, source } => write!(
                f,
                "failed to write generated host manifest '{}': {source}",
                path.display()
            ),
            Self::Canonicalize { path, source } => write!(
                f,
                "failed to canonicalize host path '{}' while generating the host manifest: {source}",
                path.display()
            ),
            Self::WekRegistryRead { path, source } => write!(
                f,
                "failed to read worker_environment_kind registry '{}' to collect declared host \
                 ref-slots: {source}",
                path.display()
            ),
            Self::WekDescriptorParse { path, source } => write!(
                f,
                "worker_environment_kind descriptor '{}' declares malformed slot data: {source}",
                path.display()
            ),

        }
    }
}

impl std::error::Error for HostManifestError {}

pub struct HostFactSeed {
    pub root_refs: BTreeMap<String, PathBuf>,
}

pub fn host_manifest_path(octopus_home: &Path) -> PathBuf {
    octopus_home.join(HOST_MANIFEST_FILE_NAME)
}

pub fn load_or_initialize_host_roots(
    octopus_home: &Path,
    registry_root: &Path,
    seed: impl FnOnce() -> Result<HostFactSeed, HostManifestError>,
) -> Result<ResolvedHostRoots, HostManifestError> {
    let declared = collect_declared_host_slots(registry_root)?;
    let path = host_manifest_path(octopus_home);
    if !path.exists() {
        let seed = seed()?;
        write_default_host_manifest(&path, &seed, &declared)?;
    }
    let manifest = read_host_manifest(&path)?;
    resolve_host_roots(&path, &manifest, &declared)
}

fn read_guarded(path: &Path) -> std::io::Result<Vec<u8>> {
    let guard = ReadGuard::try_acquire(&[path.to_path_buf()], &[])?;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::other("host state evidence must be a regular file"));
    }
    let bytes = std::fs::read(path)?;
    guard.verify()?;
    Ok(bytes)
}

pub fn read_host_manifest(path: &Path) -> Result<HostManifest, HostManifestError> {
    let bytes = read_guarded(path).map_err(|source| HostManifestError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    parse_host_manifest(path, &bytes)
}

fn parse_host_manifest(path: &Path, bytes: &[u8]) -> Result<HostManifest, HostManifestError> {
    let manifest: HostManifest =
        serde_json::from_slice(bytes).map_err(|source| HostManifestError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    if manifest.host_manifest_version != HOST_MANIFEST_VERSION {
        return Err(HostManifestError::UnsupportedVersion {
            path: path.to_path_buf(),
            version: manifest.host_manifest_version,
        });
    }
    Ok(manifest)
}

pub fn collect_declared_host_slots(
    registry_root: &Path,
) -> Result<Vec<DeclaredHostSlot>, HostManifestError> {
    let wek_dir = registry_root.join("worker_environment_kind");
    let mut slots = BTreeMap::from([
        (RUN_RECORDS_ROOT_SLOT.to_string(), true),
        (EE_INSTANCES_ROOT_SLOT.to_string(), true),
    ]);
    let entries = match std::fs::read_dir(&wek_dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(slots
                .into_iter()
                .map(|(name, required)| DeclaredHostSlot { name, required })
                .collect());
        }
        Err(source) => {
            return Err(HostManifestError::WekRegistryRead {
                path: wek_dir,
                source,
            });
        }
    };
    for entry in entries {
        let entry = entry.map_err(|source| HostManifestError::WekRegistryRead {
            path: wek_dir.clone(),
            source,
        })?;
        if entry.file_name() == publication::COORDINATION_DIRECTORY {
            let metadata = std::fs::symlink_metadata(entry.path()).map_err(|source| HostManifestError::WekRegistryRead { path: entry.path(), source })?;
            #[cfg(unix)]
            let private = { use std::os::unix::fs::PermissionsExt; metadata.permissions().mode() & 0o077 == 0 };
            #[cfg(not(unix))]
            let private = false;
            if !metadata.is_dir() || metadata.file_type().is_symlink() || !private {
                return Err(HostManifestError::WekRegistryRead { path: entry.path(), source: std::io::Error::other("reserved publication metadata is not an exact private directory") });
            }
            continue;
        }
        if !entry
            .file_type()
            .map_err(|source| HostManifestError::WekRegistryRead {
                path: entry.path(),
                source,
            })?
            .is_dir()
        {
            continue;
        }
        let descriptor_path = entry.path().join("worker_environment_kind.json");
        if !descriptor_path.exists() {
            continue;
        }
        let bytes = std::fs::read(&descriptor_path).map_err(|source| {
            HostManifestError::WekRegistryRead {
                path: descriptor_path.clone(),
                source,
            }
        })?;
        let declaration: WekSlotDeclaration = serde_json::from_slice(&bytes).map_err(|source| {
            HostManifestError::WekDescriptorParse {
                path: descriptor_path.clone(),
                source,
            }
        })?;
        fold_wek_declared_slots(&declaration, &mut slots);
    }
    Ok(slots
        .into_iter()
        .map(|(name, required)| DeclaredHostSlot { name, required })
        .collect())
}

fn fold_wek_declared_slots(declaration: &WekSlotDeclaration, slots: &mut BTreeMap<String, bool>) {
    for root_ref in declaration
        .boundary_defaults
        .read_root_refs
        .iter()
        .chain(&declaration.boundary_defaults.write_root_refs)
        .chain(&declaration.boundary_defaults.protected_read_only_subpaths)
    {
        let slot = root_ref.path.trim();
        if slot.is_empty() || slot == "cwd" {
            continue;
        }
        note_slot(slots, slot, false);
    }
}

fn note_slot(slots: &mut BTreeMap<String, bool>, slot: &str, required: bool) {
    let entry = slots.entry(slot.to_string()).or_insert(false);
    *entry = *entry || required;
}

pub fn resolve_host_roots(
    path: &Path,
    manifest: &HostManifest,
    declared: &[DeclaredHostSlot],
) -> Result<ResolvedHostRoots, HostManifestError> {
    for (slot, value) in &manifest.root_refs {
        if !value.is_absolute() {
            return Err(HostManifestError::NonAbsoluteSlot {
                path: path.to_path_buf(),
                slot: slot.clone(),
                value: value.clone(),
            });
        }
    }
    let mut slots = BTreeMap::new();
    for declared_slot in declared {
        match manifest.root_refs.get(&declared_slot.name) {
            Some(value) => {
                slots.insert(declared_slot.name.clone(), value.clone());
            }
            None if declared_slot.required => {
                return Err(HostManifestError::MissingSlot {
                    path: path.to_path_buf(),
                    slot: declared_slot.name.clone(),
                });
            }
            None => {}
        }
    }
    Ok(ResolvedHostRoots { slots })
}

fn write_default_host_manifest(
    path: &Path,
    seed: &HostFactSeed,
    declared: &[DeclaredHostSlot],
) -> Result<(), HostManifestError> {
    let mut root_refs = BTreeMap::new();
    for (slot, value) in &seed.root_refs {
        let value = normalize_generated_root(path, slot, value.to_path_buf())?;
        root_refs.insert(slot.clone(), value);
    }
    for declared_slot in declared {
        if declared_slot.required && !root_refs.contains_key(&declared_slot.name) {
            return Err(HostManifestError::MissingSlot {
                path: path.to_path_buf(),
                slot: declared_slot.name.clone(),
            });
        }
    }
    let manifest = HostManifest {
        host_manifest_version: HOST_MANIFEST_VERSION,
        root_refs,
    };
    if let Some(parent) = path.parent() {
        publication::create_dir_all(parent).map_err(|source| HostManifestError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let bytes =
        serde_json::to_vec_pretty(&manifest).map_err(|source| HostManifestError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    let target = std::fs::canonicalize(path.parent().ok_or_else(|| HostManifestError::Write { path: path.to_path_buf(), source: std::io::Error::other("host manifest has no parent") })?)
        .map_err(|source| HostManifestError::Write { path: path.to_path_buf(), source })?.join(path.file_name().unwrap());
    let operation = format!("host-manifest-initialize:{}", uuid::Uuid::new_v4().simple());
    let receipt = publication::publish(&target, &operation, None, &bytes).map_err(|source| HostManifestError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    if receipt.outcome != PublicationOutcome::Applied {
        return Err(HostManifestError::Write {
            path: target,
            source: std::io::Error::other(receipt.error.unwrap_or_else(|| "host manifest publication was not applied".into())),
        });
    }
    Ok(())
}

fn normalize_generated_root(
    manifest_path: &Path,
    slot: &str,
    value: PathBuf,
) -> Result<PathBuf, HostManifestError> {
    if !value.is_absolute() {
        return Err(HostManifestError::NonAbsoluteSlot {
            path: manifest_path.to_path_buf(),
            slot: slot.to_string(),
            value,
        });
    }
    if value.exists() {
        std::fs::canonicalize(&value).map_err(|source| HostManifestError::Canonicalize {
            path: value,
            source,
        })
    } else {
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_room_registry(registry_root: &Path) {
        let wek = registry_root.join("worker_environment_kind").join("room");
        std::fs::create_dir_all(&wek).expect("wek dir");
        std::fs::write(
            wek.join("worker_environment_kind.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "worker_environment_kind_id": "room",
                "boundary_defaults": {
                    "read_root_refs": [
                        { "path": "source_tree", "access": ["read"] },
                        { "path": "run_records_root", "access": ["read"] }
                    ],
                    "write_root_refs": [
                        { "path": "pointer_root", "access": ["read", "write"] },
                        { "path": "room_root", "access": ["read", "write"] },
                        { "path": "run_records_root", "access": ["read", "write"] }
                    ],
                    "protected_read_only_subpaths": [
                        { "path": "protected_metadata", "access": ["read"] },
                        { "path": "ee_instances_root", "access": ["read"] }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("write wek descriptor");
    }

    fn write_git_registry(registry_root: &Path) {
        let wek = registry_root
            .join("worker_environment_kind")
            .join("git_worktree");
        std::fs::create_dir_all(&wek).expect("git wek dir");
        std::fs::write(
            wek.join("worker_environment_kind.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "worker_environment_kind_id": "git_worktree",
                "boundary_defaults": {
                    "read_root_refs": [
                        { "path": REPO_ROOT_SLOT, "access": ["read"] }
                    ],
                    "write_root_refs": [
                        { "path": "git_common_root", "access": ["read", "write"] },
                        { "path": WORKTREE_ROOT_SLOT, "access": ["read", "write"] }
                    ]
                }
            }))
            .unwrap(),
        )
        .expect("write git wek descriptor");
    }


    /// An absolute fixture path that names nothing on disk: `/abs/<name>`,
    /// or `C:\abs\<name>` on Windows, where `/abs/…` has no drive and is
    /// not absolute.
    fn abs(name: &str) -> String {
        #[cfg(windows)]
        return format!(r"C:\abs\{name}");
        #[cfg(not(windows))]
        format!("/abs/{name}")
    }

    #[test]
    fn worker_environment_slots_are_collected_beside_required_state_slots() {
        let dir = tempfile::tempdir().expect("temp registry");
        write_room_registry(dir.path());
        let declared = collect_declared_host_slots(dir.path()).expect("collect declared slots");
        let by_name: BTreeMap<_, _> = declared
            .iter()
            .map(|slot| (slot.name.as_str(), slot.required))
            .collect();
        assert_eq!(by_name.get("room_root"), Some(&false));
        assert_eq!(by_name.get("source_tree"), Some(&false));
        assert_eq!(by_name.get("pointer_root"), Some(&false));
        assert_eq!(by_name.get("run_records_root"), Some(&true));
        assert_eq!(by_name.get("ee_instances_root"), Some(&true));
        assert_eq!(by_name.get("protected_metadata"), Some(&false));
        assert!(!by_name.contains_key("cwd"));
        let metadata = dir.path().join("worker_environment_kind").join(publication::COORDINATION_DIRECTORY);
        std::fs::write(metadata, b"foreign").unwrap();
        assert!(collect_declared_host_slots(dir.path()).unwrap_err().to_string().contains("reserved publication metadata"));
    }

    #[test]
    fn fresh_manifest_retains_machine_facts_for_packages_installed_after_boot() {
        let home = tempfile::tempdir().unwrap();
        let registry = tempfile::tempdir().unwrap();
        let source = std::fs::canonicalize(registry.path()).unwrap();
        let worktrees = home.path().join("state/worker-worktrees");
        let initial = load_or_initialize_host_roots(home.path(), registry.path(), || {
            Ok(HostFactSeed {
                root_refs: BTreeMap::from([
                    (REPO_ROOT_SLOT.to_string(), source.clone()),
                    (WORKTREE_ROOT_SLOT.to_string(), worktrees.clone()),
                    (
                        RUN_RECORDS_ROOT_SLOT.to_string(),
                        home.path().join("state/runs"),
                    ),
                    (
                        EE_INSTANCES_ROOT_SLOT.to_string(),
                        home.path().join("state/ee_instances"),
                    ),
                ]),
            })
        })
        .unwrap();
        assert_eq!(initial.get(REPO_ROOT_SLOT), None);
        let manifest = read_host_manifest(&host_manifest_path(home.path())).unwrap();
        assert_eq!(manifest.root_refs.get(REPO_ROOT_SLOT), Some(&source));
        assert_eq!(manifest.root_refs.get(WORKTREE_ROOT_SLOT), Some(&worktrees));
        write_git_registry(registry.path());
        let installed =
            load_or_initialize_host_roots(home.path(), registry.path(), || unreachable!()).unwrap();
        assert_eq!(installed.get(REPO_ROOT_SLOT), Some(source.as_path()));
        assert_eq!(installed.get(WORKTREE_ROOT_SLOT), Some(worktrees.as_path()));
    }

    #[test]
    fn missing_manifest_is_generated_from_seed_then_read_back_fail_closed() {
        let home_dir = tempfile::tempdir().expect("temp host home");
        let home = home_dir.path();
        let registry_dir = tempfile::tempdir().expect("temp registry");
        write_room_registry(registry_dir.path());

        let source = home.join("source");
        let common_root = home.join("shared-metadata");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::create_dir_all(&common_root).expect("shared metadata dir");
        let rooms = home.join("state/rooms");
        let runs = home.join("runs");
        let ees = home.join("ees");

        let resolved = load_or_initialize_host_roots(home, registry_dir.path(), || {
            Ok(HostFactSeed {
                root_refs: BTreeMap::from([
                    ("source_tree".to_string(), source.clone()),
                    ("pointer_root".to_string(), common_root.clone()),
                    ("room_root".to_string(), rooms.clone()),
                    (RUN_RECORDS_ROOT_SLOT.to_string(), runs.clone()),
                    (EE_INSTANCES_ROOT_SLOT.to_string(), ees.clone()),
                ]),
            })
        })
        .expect("first boot generates + reads the manifest");

        assert!(host_manifest_path(home).exists());
        let slots = resolved.slots();
        assert_eq!(
            slots.get("source_tree").map(PathBuf::as_path),
            Some(std::fs::canonicalize(&source).unwrap().as_path())
        );
        assert_eq!(
            slots.get("room_root").map(PathBuf::as_path),
            Some(rooms.as_path())
        );
        assert_eq!(
            slots.get("pointer_root").map(PathBuf::as_path),
            Some(std::fs::canonicalize(common_root).unwrap().as_path())
        );
        assert_eq!(
            slots.get(RUN_RECORDS_ROOT_SLOT).map(PathBuf::as_path),
            Some(runs.as_path())
        );
        assert_eq!(
            slots.get(EE_INSTANCES_ROOT_SLOT).map(PathBuf::as_path),
            Some(ees.as_path())
        );
        assert_eq!(slots.get("protected_metadata"), None);

        let again = load_or_initialize_host_roots(home, registry_dir.path(), || {
            panic!("seed must not run when the manifest already exists")
        })
        .expect("second boot reads the existing manifest");
        assert_eq!(again, resolved);
    }

    #[test]
    fn fresh_non_git_host_boots_without_a_git_common_root_binding() {
        let home_dir = tempfile::tempdir().expect("temp host home");
        let source_dir = tempfile::tempdir().expect("non-git source");
        let registry_dir = tempfile::tempdir().expect("temp registry");
        write_git_registry(registry_dir.path());
        let worktrees = home_dir.path().join("state/worker-worktrees");
        let runs = home_dir.path().join("state/runs");
        let ees = home_dir.path().join("state/ee_instances");

        let resolved = load_or_initialize_host_roots(home_dir.path(), registry_dir.path(), || {
            Ok(HostFactSeed {
                root_refs: BTreeMap::from([
                    (REPO_ROOT_SLOT.to_string(), source_dir.path().to_path_buf()),
                    (WORKTREE_ROOT_SLOT.to_string(), worktrees.clone()),
                    (RUN_RECORDS_ROOT_SLOT.to_string(), runs.clone()),
                    (EE_INSTANCES_ROOT_SLOT.to_string(), ees.clone()),
                ]),
            })
        })
        .expect("non-git hosts still initialize their machine slot registry");

        assert_eq!(
            resolved.get(REPO_ROOT_SLOT),
            Some(std::fs::canonicalize(source_dir.path()).unwrap().as_path())
        );
        assert_eq!(resolved.get(WORKTREE_ROOT_SLOT), Some(worktrees.as_path()));
        assert_eq!(resolved.get(RUN_RECORDS_ROOT_SLOT), Some(runs.as_path()));
        assert_eq!(resolved.get(EE_INSTANCES_ROOT_SLOT), Some(ees.as_path()));
        assert_eq!(resolved.get("git_common_root"), None);
    }

    #[test]
    fn malformed_manifest_fails_closed() {
        let home_dir = tempfile::tempdir().expect("temp host home");
        let home = home_dir.path();
        let registry_dir = tempfile::tempdir().expect("temp registry");
        write_room_registry(registry_dir.path());
        std::fs::write(host_manifest_path(home), b"{ not json").expect("write malformed");
        let err = load_or_initialize_host_roots(home, registry_dir.path(), || unreachable!())
            .expect_err("malformed manifest must fail closed");
        assert!(matches!(err, HostManifestError::Parse { .. }));
    }

    #[test]
    fn unsupported_version_fails_closed() {
        let home_dir = tempfile::tempdir().expect("temp host home");
        let home = home_dir.path();
        let registry_dir = tempfile::tempdir().expect("temp registry");
        write_room_registry(registry_dir.path());
        std::fs::write(
            host_manifest_path(home),
            serde_json::to_vec_pretty(&serde_json::json!({
                "host_manifest_version": 999,
                "root_refs": {}
            }))
            .unwrap(),
        )
        .expect("write versioned");
        let err = load_or_initialize_host_roots(home, registry_dir.path(), || unreachable!())
            .expect_err("unsupported version must fail closed");
        assert!(matches!(
            err,
            HostManifestError::UnsupportedVersion { version: 999, .. }
        ));
    }

    #[test]
    fn interface_door_slot_does_not_expand_the_runtime_wek_projection() {
        let home_dir = tempfile::tempdir().expect("temp host home");
        let home = home_dir.path();
        let registry_dir = tempfile::tempdir().expect("temp registry");
        write_room_registry(registry_dir.path());
        std::fs::write(
            host_manifest_path(home),
            serde_json::to_vec_pretty(&serde_json::json!({
                "host_manifest_version": HOST_MANIFEST_VERSION,
                "root_refs": {
                    "pointer_root": abs("pointer"),
                    "source_tree": abs("source"),
                    "run_records_root": abs("runs"),
                    "ee_instances_root": abs("ees"),
                    "timezone_database_root": abs("timezone")
                }
            }))
            .unwrap(),
        )
        .expect("write partial");
        let resolved = load_or_initialize_host_roots(home, registry_dir.path(), || unreachable!())
            .expect("machine slots used by interface doors do not widen the runtime projection");
        assert_eq!(resolved.get("timezone_database_root"), None);
        let manifest = read_host_manifest(&host_manifest_path(home)).unwrap();
        assert_eq!(
            manifest.root_refs.get("timezone_database_root"),
            Some(&PathBuf::from(abs("timezone")))
        );
    }

    #[test]
    fn optional_declared_slot_may_be_unbound() {
        let home_dir = tempfile::tempdir().expect("temp host home");
        let home = home_dir.path();
        let registry_dir = tempfile::tempdir().expect("temp registry");
        write_room_registry(registry_dir.path());
        std::fs::write(
            host_manifest_path(home),
            serde_json::to_vec_pretty(&serde_json::json!({
                "host_manifest_version": HOST_MANIFEST_VERSION,
                "root_refs": {
                    "pointer_root": abs("pointer"),
                    "source_tree": abs("source"),
                    "room_root": abs("room"),
                    "run_records_root": abs("runs"),
                    "ee_instances_root": abs("ees")
                }
            }))
            .unwrap(),
        )
        .expect("write required-only");
        let resolved = load_or_initialize_host_roots(home, registry_dir.path(), || unreachable!())
            .expect("required slot bound resolves");
        let slots = resolved.slots();
        assert_eq!(
            slots.get("source_tree").map(PathBuf::as_path),
            Some(Path::new(&abs("source")))
        );
        assert_eq!(
            slots.get("room_root").map(PathBuf::as_path),
            Some(Path::new(&abs("room")))
        );
        assert!(!slots.contains_key("base"));
        assert_eq!(slots.get("protected_metadata"), None);
    }

    #[test]
    fn non_absolute_slot_value_fails_closed() {
        let home_dir = tempfile::tempdir().expect("temp host home");
        let home = home_dir.path();
        let registry_dir = tempfile::tempdir().expect("temp registry");
        write_room_registry(registry_dir.path());
        std::fs::write(
            host_manifest_path(home),
            serde_json::to_vec_pretty(&serde_json::json!({
                "host_manifest_version": HOST_MANIFEST_VERSION,
                "root_refs": {
                    "pointer_root": abs("pointer"),
                    "source_tree": "relative/source",
                    "room_root": abs("room"),
                    "run_records_root": abs("runs"),
                    "protected_metadata": abs("protected")
                }
            }))
            .unwrap(),
        )
        .expect("write relative");
        let err = load_or_initialize_host_roots(home, registry_dir.path(), || unreachable!())
            .expect_err("a non-absolute ref-slot value must fail closed");
        assert!(matches!(err, HostManifestError::NonAbsoluteSlot { .. }));
    }
}
