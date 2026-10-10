use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const HOST_MANIFEST_VERSION: u32 = 1;

pub const HOST_MANIFEST_FILE_NAME: &str = "host.json";

const STATE_MIGRATION_FILE_NAME: &str = ".state-layout-migration-v1.json";
const STATE_MIGRATION_VERSION: u32 = 1;

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
    StateMigrationRead {
        path: PathBuf,
        source: std::io::Error,
    },
    StateMigrationParse {
        path: PathBuf,
        source: serde_json::Error,
    },
    StateMigrationVersion {
        path: PathBuf,
        version: u32,
    },
    StateMigrationConflict {
        path: PathBuf,
        detail: String,
    },
    StateMigrationWrite {
        path: PathBuf,
        source: std::io::Error,
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
            Self::StateMigrationRead { path, source } => write!(
                f,
                "state-layout migration surface '{}' cannot be read: {source}",
                path.display()
            ),
            Self::StateMigrationParse { path, source } => write!(
                f,
                "state-layout migration surface '{}' is malformed: {source}",
                path.display()
            ),
            Self::StateMigrationVersion { path, version } => write!(
                f,
                "state-layout migration journal '{}' has version {version}, expected {STATE_MIGRATION_VERSION}",
                path.display()
            ),
            Self::StateMigrationConflict { path, detail } => write!(
                f,
                "state-layout migration refuses '{}': {detail}",
                path.display()
            ),
            Self::StateMigrationWrite { path, source } => write!(
                f,
                "state-layout migration cannot write '{}': {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for HostManifestError {}

pub struct HostFactSeed {
    pub root_refs: BTreeMap<String, PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateLayoutMove {
    slot: String,
    from: PathBuf,
    to: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateLayoutMigration {
    schema_version: u32,
    moves: Vec<StateLayoutMove>,
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
    migrate_legacy_state_layout(octopus_home, &path)?;
    let manifest = read_host_manifest(&path)?;
    resolve_host_roots(&path, &manifest, &declared)
}

pub fn read_host_manifest(path: &Path) -> Result<HostManifest, HostManifestError> {
    let bytes = std::fs::read(path).map_err(|source| HostManifestError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let manifest: HostManifest =
        serde_json::from_slice(&bytes).map_err(|source| HostManifestError::Parse {
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

fn migrate_legacy_state_layout(
    octopus_home: &Path,
    manifest_path: &Path,
) -> Result<(), HostManifestError> {
    let journal_path = octopus_home.join(STATE_MIGRATION_FILE_NAME);
    if journal_path.exists() {
        let journal = read_state_migration(&journal_path)?;
        validate_state_migration(octopus_home, &journal_path, &journal)?;
        return apply_state_migration(manifest_path, &journal_path, journal);
    }

    let manifest = read_host_manifest(manifest_path)?;
    let old_root = octopus_home
        .join("ledger")
        .join("handler-interface-artifacts");
    let expected = [
        (
            RUN_RECORDS_ROOT_SLOT,
            old_root.join("runs"),
            octopus_home.join("state").join("runs"),
        ),
        (
            EE_INSTANCES_ROOT_SLOT,
            old_root.join("ee_instances"),
            octopus_home.join("state").join("ee_instances"),
        ),
    ];
    let legacy_slots = expected
        .iter()
        .filter(|(slot, from, _)| manifest.root_refs.get(*slot) == Some(from))
        .count();
    if legacy_slots == 0 {
        return Ok(());
    }
    if legacy_slots != expected.len() {
        return Err(HostManifestError::StateMigrationConflict {
            path: manifest_path.to_path_buf(),
            detail: "only one of run_records_root and ee_instances_root matches the retired generated layout; the state ownership is ambiguous".to_string(),
        });
    }
    let migration = StateLayoutMigration {
        schema_version: STATE_MIGRATION_VERSION,
        moves: expected
            .into_iter()
            .map(|(slot, from, to)| StateLayoutMove {
                slot: slot.to_string(),
                from,
                to,
            })
            .collect(),
    };
    validate_state_migration(octopus_home, &journal_path, &migration)?;
    preflight_state_migration(&migration)?;
    write_json_atomic(&journal_path, &migration)?;
    apply_state_migration(manifest_path, &journal_path, migration)
}

fn read_state_migration(path: &Path) -> Result<StateLayoutMigration, HostManifestError> {
    let bytes = std::fs::read(path).map_err(|source| HostManifestError::StateMigrationRead {
        path: path.to_path_buf(),
        source,
    })?;
    let migration: StateLayoutMigration = serde_json::from_slice(&bytes).map_err(|source| {
        HostManifestError::StateMigrationParse {
            path: path.to_path_buf(),
            source,
        }
    })?;
    if migration.schema_version != STATE_MIGRATION_VERSION {
        return Err(HostManifestError::StateMigrationVersion {
            path: path.to_path_buf(),
            version: migration.schema_version,
        });
    }
    Ok(migration)
}

fn validate_state_migration(
    octopus_home: &Path,
    journal_path: &Path,
    migration: &StateLayoutMigration,
) -> Result<(), HostManifestError> {
    let old_root = octopus_home
        .join("ledger")
        .join("handler-interface-artifacts");
    let expected = BTreeMap::from([
        (
            RUN_RECORDS_ROOT_SLOT,
            (old_root.join("runs"), octopus_home.join("state/runs")),
        ),
        (
            EE_INSTANCES_ROOT_SLOT,
            (
                old_root.join("ee_instances"),
                octopus_home.join("state/ee_instances"),
            ),
        ),
    ]);
    if migration.moves.len() != expected.len() {
        return Err(HostManifestError::StateMigrationConflict {
            path: journal_path.to_path_buf(),
            detail: "journal does not contain the complete run/EE state move set".to_string(),
        });
    }
    let mut seen = std::collections::BTreeSet::new();
    for movement in &migration.moves {
        let Some((from, to)) = expected.get(movement.slot.as_str()) else {
            return Err(HostManifestError::StateMigrationConflict {
                path: journal_path.to_path_buf(),
                detail: format!("journal names unknown state slot '{}'", movement.slot),
            });
        };
        if !seen.insert(movement.slot.as_str()) || &movement.from != from || &movement.to != to {
            return Err(HostManifestError::StateMigrationConflict {
                path: journal_path.to_path_buf(),
                detail: format!(
                    "journal move for '{}' does not match the deterministic home state layout",
                    movement.slot
                ),
            });
        }
    }
    Ok(())
}

fn preflight_state_migration(migration: &StateLayoutMigration) -> Result<(), HostManifestError> {
    for movement in &migration.moves {
        require_directory_or_missing(&movement.from)?;
        match directory_state(&movement.to)? {
            DirectoryState::Missing | DirectoryState::Empty => {}
            DirectoryState::NonEmpty => {
                return Err(HostManifestError::StateMigrationConflict {
                    path: movement.to.clone(),
                    detail: format!(
                        "the new state root is non-empty while '{}' still points at the retired root",
                        movement.slot
                    ),
                });
            }
        }
    }
    Ok(())
}

fn apply_state_migration(
    manifest_path: &Path,
    journal_path: &Path,
    migration: StateLayoutMigration,
) -> Result<(), HostManifestError> {
    let mut manifest = read_host_manifest(manifest_path)?;
    for movement in &migration.moves {
        let configured = manifest.root_refs.get(&movement.slot);
        if configured != Some(&movement.from) && configured != Some(&movement.to) {
            return Err(HostManifestError::StateMigrationConflict {
                path: manifest_path.to_path_buf(),
                detail: format!(
                    "slot '{}' changed while the state migration journal was active",
                    movement.slot
                ),
            });
        }
    }
    for movement in &migration.moves {
        resume_state_move(movement)?;
    }
    for movement in &migration.moves {
        manifest
            .root_refs
            .insert(movement.slot.clone(), movement.to.clone());
    }
    write_json_atomic(manifest_path, &manifest)?;
    std::fs::remove_file(journal_path).map_err(|source| {
        HostManifestError::StateMigrationWrite {
            path: journal_path.to_path_buf(),
            source,
        }
    })?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirectoryState {
    Missing,
    Empty,
    NonEmpty,
}

fn directory_state(path: &Path) -> Result<DirectoryState, HostManifestError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DirectoryState::Missing);
        }
        Err(source) => {
            return Err(HostManifestError::StateMigrationRead {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(HostManifestError::StateMigrationConflict {
            path: path.to_path_buf(),
            detail: "state root is not a regular directory".to_string(),
        });
    }
    let mut entries =
        std::fs::read_dir(path).map_err(|source| HostManifestError::StateMigrationRead {
            path: path.to_path_buf(),
            source,
        })?;
    if entries.next().is_some() {
        Ok(DirectoryState::NonEmpty)
    } else {
        Ok(DirectoryState::Empty)
    }
}

fn require_directory_or_missing(path: &Path) -> Result<(), HostManifestError> {
    match directory_state(path)? {
        DirectoryState::Missing | DirectoryState::Empty | DirectoryState::NonEmpty => Ok(()),
    }
}

fn resume_state_move(movement: &StateLayoutMove) -> Result<(), HostManifestError> {
    let from = directory_state(&movement.from)?;
    let to = directory_state(&movement.to)?;
    match (from, to) {
        (DirectoryState::Missing, DirectoryState::Missing) => {
            std::fs::create_dir_all(&movement.to).map_err(|source| {
                HostManifestError::StateMigrationWrite {
                    path: movement.to.clone(),
                    source,
                }
            })?;
        }
        (DirectoryState::Missing, DirectoryState::Empty | DirectoryState::NonEmpty) => {}
        (DirectoryState::Empty | DirectoryState::NonEmpty, DirectoryState::Missing) => {
            let parent =
                movement
                    .to
                    .parent()
                    .ok_or_else(|| HostManifestError::StateMigrationConflict {
                        path: movement.to.clone(),
                        detail: "new state root has no parent".to_string(),
                    })?;
            std::fs::create_dir_all(parent).map_err(|source| {
                HostManifestError::StateMigrationWrite {
                    path: parent.to_path_buf(),
                    source,
                }
            })?;
            std::fs::rename(&movement.from, &movement.to).map_err(|source| {
                HostManifestError::StateMigrationWrite {
                    path: movement.to.clone(),
                    source,
                }
            })?;
        }
        (DirectoryState::Empty | DirectoryState::NonEmpty, DirectoryState::Empty) => {
            std::fs::remove_dir(&movement.to).map_err(|source| {
                HostManifestError::StateMigrationWrite {
                    path: movement.to.clone(),
                    source,
                }
            })?;
            std::fs::rename(&movement.from, &movement.to).map_err(|source| {
                HostManifestError::StateMigrationWrite {
                    path: movement.to.clone(),
                    source,
                }
            })?;
        }
        (DirectoryState::Empty | DirectoryState::NonEmpty, DirectoryState::NonEmpty) => {
            return Err(HostManifestError::StateMigrationConflict {
                path: movement.to.clone(),
                detail: format!(
                    "both retired and new state roots for '{}' contain data",
                    movement.slot
                ),
            });
        }
    }
    Ok(())
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), HostManifestError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| {
            HostManifestError::StateMigrationWrite {
                path: parent.to_path_buf(),
                source,
            }
        })?;
    }
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|source| {
        HostManifestError::StateMigrationParse {
            path: path.to_path_buf(),
            source,
        }
    })?;
    bytes.push(b'\n');
    let temp = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state-layout"),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&temp, bytes).map_err(|source| HostManifestError::StateMigrationWrite {
        path: temp.clone(),
        source,
    })?;
    std::fs::rename(&temp, path).map_err(|source| HostManifestError::StateMigrationWrite {
        path: path.to_path_buf(),
        source,
    })
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
        std::fs::create_dir_all(parent).map_err(|source| HostManifestError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let bytes =
        serde_json::to_vec_pretty(&manifest).map_err(|source| HostManifestError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes).map_err(|source| HostManifestError::Write {
        path: tmp.clone(),
        source,
    })?;
    std::fs::rename(&tmp, path).map_err(|source| HostManifestError::Write {
        path: path.to_path_buf(),
        source,
    })?;
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
    fn exact_legacy_generated_state_roots_move_once_into_home_state() {
        let home_dir = tempfile::tempdir().expect("temp host home");
        let home = home_dir.path();
        let registry_dir = tempfile::tempdir().expect("temp registry");
        write_room_registry(registry_dir.path());
        let legacy = home.join("ledger/handler-interface-artifacts");
        let old_runs = legacy.join("runs");
        let old_ees = legacy.join("ee_instances");
        std::fs::create_dir_all(&old_runs).unwrap();
        std::fs::create_dir_all(&old_ees).unwrap();
        std::fs::write(old_runs.join("run-a.json"), "run").unwrap();
        std::fs::write(old_ees.join("ee-a.json"), "ee").unwrap();
        std::fs::write(
            host_manifest_path(home),
            serde_json::to_vec_pretty(&serde_json::json!({
                "host_manifest_version": HOST_MANIFEST_VERSION,
                "root_refs": {
                    "pointer_root": home.join("shared-metadata"),
                    "source_tree": home.join("source"),
                    "room_root": home.join("rooms"),
                    "run_records_root": old_runs,
                    "ee_instances_root": old_ees
                }
            }))
            .unwrap(),
        )
        .unwrap();

        load_or_initialize_host_roots(home, registry_dir.path(), || unreachable!()).unwrap();

        let manifest = read_host_manifest(&host_manifest_path(home)).unwrap();
        assert_eq!(
            manifest.root_refs.get(RUN_RECORDS_ROOT_SLOT),
            Some(&home.join("state/runs"))
        );
        assert_eq!(
            manifest.root_refs.get(EE_INSTANCES_ROOT_SLOT),
            Some(&home.join("state/ee_instances"))
        );
        assert_eq!(
            std::fs::read_to_string(home.join("state/runs/run-a.json")).unwrap(),
            "run"
        );
        assert_eq!(
            std::fs::read_to_string(home.join("state/ee_instances/ee-a.json")).unwrap(),
            "ee"
        );
        assert!(!home.join(STATE_MIGRATION_FILE_NAME).exists());
    }

    #[test]
    fn legacy_state_migration_refuses_a_nonempty_new_root_before_moving() {
        let home_dir = tempfile::tempdir().expect("temp host home");
        let home = home_dir.path();
        let registry_dir = tempfile::tempdir().expect("temp registry");
        write_room_registry(registry_dir.path());
        let old_runs = home.join("ledger/handler-interface-artifacts/runs");
        let old_ees = home.join("ledger/handler-interface-artifacts/ee_instances");
        std::fs::create_dir_all(&old_runs).unwrap();
        std::fs::create_dir_all(&old_ees).unwrap();
        std::fs::write(old_runs.join("run-a.json"), "old").unwrap();
        std::fs::create_dir_all(home.join("state/runs")).unwrap();
        std::fs::write(home.join("state/runs/run-b.json"), "new").unwrap();
        std::fs::write(
            host_manifest_path(home),
            serde_json::to_vec_pretty(&serde_json::json!({
                "host_manifest_version": HOST_MANIFEST_VERSION,
                "root_refs": {
                    "pointer_root": home.join("shared-metadata"),
                    "source_tree": home.join("source"),
                    "room_root": home.join("rooms"),
                    "run_records_root": old_runs,
                    "ee_instances_root": old_ees
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let error = load_or_initialize_host_roots(home, registry_dir.path(), || unreachable!())
            .unwrap_err();
        assert!(matches!(
            error,
            HostManifestError::StateMigrationConflict { .. }
        ));
        assert_eq!(
            std::fs::read_to_string(old_runs.join("run-a.json")).unwrap(),
            "old"
        );
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
                    "pointer_root": "/abs/pointer",
                    "source_tree": "/abs/source",
                    "run_records_root": "/abs/runs",
                    "ee_instances_root": "/abs/ees",
                    "timezone_database_root": "/abs/timezone"
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
            Some(&PathBuf::from("/abs/timezone"))
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
                    "pointer_root": "/abs/pointer",
                    "source_tree": "/abs/source",
                    "room_root": "/abs/room",
                    "run_records_root": "/abs/runs",
                    "ee_instances_root": "/abs/ees"
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
            Some(Path::new("/abs/source"))
        );
        assert_eq!(
            slots.get("room_root").map(PathBuf::as_path),
            Some(Path::new("/abs/room"))
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
                    "pointer_root": "/abs/pointer",
                    "source_tree": "relative/source",
                    "room_root": "/abs/room",
                    "run_records_root": "/abs/runs",
                    "protected_metadata": "/abs/protected"
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
