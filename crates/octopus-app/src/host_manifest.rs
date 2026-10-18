use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use octopus_file_storage::publication::{self, DigestAlgorithm, DirectoryMove, DirectoryMoveReceipt,
    DirectoryPreimage, FileIdentity, PublicationOutcome, PublicationReceipt, ReadGuard};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const HOST_MANIFEST_VERSION: u32 = 1;

pub const HOST_MANIFEST_FILE_NAME: &str = "host.json";

const STATE_MIGRATION_FILE_NAME: &str = ".state-layout-migration-v1.json";
const STATE_MIGRATION_VERSION: u32 = 2;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    publication: Option<StateLayoutPublication>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateLayoutPublication {
    operation_id: String,
    manifest_before: String,
    manifest_after: String,
    sources: Vec<StateLayoutSource>,
    cleanup: Vec<DirectoryMove>,
    cleanup_receipts: Vec<DirectoryMoveReceipt>,
    receipts: Vec<DirectoryMoveReceipt>,
    manifest_receipt: Option<PublicationReceipt>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateLayoutSource {
    path: PathBuf,
    expected: Option<DirectoryPreimage>,
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

fn migrate_legacy_state_layout(
    octopus_home: &Path,
    manifest_path: &Path,
) -> Result<(), HostManifestError> {
    let journal_path = octopus_home.join(STATE_MIGRATION_FILE_NAME);
    let (mut migration, mut saved) = if journal_path.try_exists().map_err(|source| migration_io(&journal_path, source))? {
        let bytes = read_guarded(&journal_path).map_err(|source| migration_io(&journal_path, source))?;
        let migration: StateLayoutMigration = serde_json::from_slice(&bytes).map_err(|source| HostManifestError::StateMigrationParse {
            path: journal_path.clone(), source,
        })?;
        (migration, Some(bytes))
    } else {
        let manifest = read_host_manifest(manifest_path)?;
        let moves = state_layout_moves(octopus_home);
        let legacy_slots = moves.iter().filter(|movement| manifest.root_refs.get(&movement.slot) == Some(&movement.from)).count();
        if legacy_slots == 0 { return Ok(()); }
        if legacy_slots != moves.len() {
            return Err(migration_conflict(manifest_path, "only one of run_records_root and ee_instances_root matches the retired generated layout; the state ownership is ambiguous"));
        }
        (StateLayoutMigration { schema_version: STATE_MIGRATION_VERSION, moves, publication: None }, None)
    };
    validate_state_moves(octopus_home, &journal_path, &migration)?;
    if migration.publication.is_none() {
        if saved.is_some() && migration.schema_version != 1 {
            return Err(migration_conflict(&journal_path, "native migration journal lost its original publication plans"));
        }
        prepare_state_migration(octopus_home, manifest_path, &mut migration, saved.is_some())?;
        save_state_migration(&journal_path, &migration, &mut saved)?;
    }
    validate_state_publication(octopus_home, manifest_path, &journal_path, &migration)?;
    apply_state_migration(octopus_home, manifest_path, &journal_path, migration, saved)
}

fn state_layout_moves(home: &Path) -> Vec<StateLayoutMove> {
    [(RUN_RECORDS_ROOT_SLOT, "runs"), (EE_INSTANCES_ROOT_SLOT, "ee_instances")].into_iter()
        .map(|(slot, name)| StateLayoutMove { slot: slot.into(), from: home.join("ledger/handler-interface-artifacts").join(name), to: home.join("state").join(name) }).collect()
}

fn migration_conflict(path: &Path, detail: impl Into<String>) -> HostManifestError {
    HostManifestError::StateMigrationConflict { path: path.to_path_buf(), detail: detail.into() }
}

fn migration_io(path: &Path, source: std::io::Error) -> HostManifestError {
    HostManifestError::StateMigrationWrite { path: path.to_path_buf(), source }
}

fn state_hash(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }

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

fn state_json<T: Serialize>(path: &Path, value: &T) -> Result<Vec<u8>, HostManifestError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|source| HostManifestError::StateMigrationParse {
        path: path.to_path_buf(), source,
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn validate_state_moves(home: &Path, journal: &Path, migration: &StateLayoutMigration) -> Result<(), HostManifestError> {
    if !matches!(migration.schema_version, 1 | STATE_MIGRATION_VERSION) {
        return Err(HostManifestError::StateMigrationVersion { path: journal.to_path_buf(), version: migration.schema_version });
    }
    if migration.schema_version == 1 && migration.publication.is_some() {
        return Err(migration_conflict(journal, "legacy migration cannot claim native publication evidence"));
    }
    let expected = state_layout_moves(home);
    if migration.moves.len() != expected.len() || expected.iter().any(|row| migration.moves.iter().filter(|found| *found == row).count() != 1) {
        return Err(migration_conflict(journal, "journal does not contain the exact complete run/EE state move set"));
    }
    exact_state_path(home)?;
    Ok(())
}

fn exact_state_path(path: &Path) -> Result<(), HostManifestError> {
    let text = path.to_str().ok_or_else(|| migration_conflict(path, "state path is not UTF-8"))?;
    if !path.is_absolute() || text.contains('\\') || text.chars().any(char::is_control)
        || text.split('/').skip(1).any(|part| part.is_empty() || matches!(part, "." | ".." | ".octopus-publication"))
    { return Err(migration_conflict(path, "state path must be an ordinary absolute normalized path")); }
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(migration_conflict(ancestor, "state root or ancestor is not an exact regular directory")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(migration_io(ancestor, error)),
        }
    }
    Ok(())
}

fn state_snapshot(path: &Path) -> Result<Option<DirectoryPreimage>, HostManifestError> {
    exact_state_path(path)?;
    match std::fs::symlink_metadata(path) {
        Ok(_) => publication::snapshot_directory(path, DigestAlgorithm::Sha256).map(Some).map_err(|source| migration_io(path, source)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(migration_io(path, error)),
    }
}

fn migration_scratch(home: &Path, operation: &str, index: usize, purpose: &str) -> PathBuf {
    home.join("state").join(format!(".{operation}-{purpose}-{index}"))
}

fn prepare_state_migration(home: &Path, manifest_path: &Path, migration: &mut StateLayoutMigration, legacy: bool) -> Result<(), HostManifestError> {
    let before = read_guarded(manifest_path).map_err(|source| migration_io(manifest_path, source))?;
    let mut manifest = parse_host_manifest(manifest_path, &before)?;
    let operation_id = format!("state-layout-{}", uuid::Uuid::new_v4().simple());
    let mut sources = Vec::new();
    let mut cleanup = Vec::new();
    for (index, movement) in migration.moves.iter().enumerate() {
        if manifest.root_refs.get(&movement.slot) != Some(&movement.from) {
            return Err(migration_conflict(manifest_path, "legacy migration already changed a slot without original native proof"));
        }
        let source = state_snapshot(&movement.from)?;
        let destination = state_snapshot(&movement.to)?;
        if legacy && (source.is_none() || destination.is_some()) {
            return Err(migration_conflict(&movement.to, "legacy migration is moved or ambiguous without original native proof; both original roots and absent destinations are required"));
        }
        if let Some(expected) = destination {
            if !expected.entries.is_empty() {
                return Err(migration_conflict(&movement.to, "the new state root is non-empty while its slot still points at the retired root"));
            }
            cleanup.push(DirectoryMove { source: movement.to.clone(), destination: migration_scratch(home, &operation_id, index, "empty"), expected });
        }
        sources.push(StateLayoutSource { path: if source.is_some() { movement.from.clone() } else { migration_scratch(home, &operation_id, index, "new") }, expected: source });
        manifest.root_refs.insert(movement.slot.clone(), movement.to.clone());
    }
    migration.schema_version = STATE_MIGRATION_VERSION;
    migration.publication = Some(StateLayoutPublication { operation_id,
        manifest_before: String::from_utf8(before).map_err(|_| migration_conflict(manifest_path, "manifest bytes are not UTF-8"))?,
        manifest_after: String::from_utf8(state_json(manifest_path, &manifest)?).map_err(|_| migration_conflict(manifest_path, "manifest bytes are not UTF-8"))?,
        sources, cleanup, cleanup_receipts: Vec::new(), receipts: Vec::new(), manifest_receipt: None });
    Ok(())
}

fn native_state_moves(migration: &StateLayoutMigration) -> Result<Vec<DirectoryMove>, HostManifestError> {
    let publication = migration.publication.as_ref().unwrap();
    migration.moves.iter().zip(&publication.sources).map(|(movement, source)| {
        Ok(DirectoryMove { source: source.path.clone(), destination: movement.to.clone(),
            expected: source.expected.clone().ok_or_else(|| migration_conflict(&source.path, "migration has an unpinned new state root"))? })
    }).collect()
}

fn validate_directory_receipts(path: &Path, operation: &str, moves: &[DirectoryMove], receipts: &[DirectoryMoveReceipt]) -> Result<(), HostManifestError> {
    let plan_hash = state_hash(&serde_json::to_vec(moves).map_err(|source| HostManifestError::StateMigrationParse { path: path.to_path_buf(), source })?);
    if receipts.len() > moves.len() { return Err(migration_conflict(path, "migration repeats native directory receipts")); }
    for (index, receipt) in receipts.iter().enumerate() {
        if receipt.schema_version != 1 || receipt.operation_id != operation || receipt.index != index || receipt.plan_hash != plan_hash
            || receipt.movement != moves[index] || (receipt.outcome == PublicationOutcome::Applied) != receipt.error.is_none()
            || index > 0 && receipts[index - 1].outcome != PublicationOutcome::Applied
        { return Err(migration_conflict(path, "migration receipt changed its original native plan")); }
    }
    Ok(())
}

fn validate_state_publication(home: &Path, manifest_path: &Path, journal_path: &Path, migration: &StateLayoutMigration) -> Result<(), HostManifestError> {
    let publication = migration.publication.as_ref().ok_or_else(|| migration_conflict(journal_path, "migration has no original native plans"))?;
    let id = publication.operation_id.strip_prefix("state-layout-").unwrap_or("");
    if migration.schema_version != STATE_MIGRATION_VERSION || id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
        || publication.sources.len() != migration.moves.len()
    { return Err(migration_conflict(journal_path, "migration has invalid operation identity or source inventory")); }
    let mut manifest = parse_host_manifest(manifest_path, publication.manifest_before.as_bytes())?;
    for (index, (movement, source)) in migration.moves.iter().zip(&publication.sources).enumerate() {
        if manifest.root_refs.get(&movement.slot) != Some(&movement.from)
            || source.path != movement.from && source.path != migration_scratch(home, &publication.operation_id, index, "new")
            || source.path == movement.from && source.expected.is_none()
            || source.expected.as_ref().is_some_and(|expected| expected.digest_algorithm != DigestAlgorithm::Sha256
                || source.path != movement.from && !expected.entries.is_empty())
        { return Err(migration_conflict(journal_path, "migration changed its original source or host roots")); }
        manifest.root_refs.insert(movement.slot.clone(), movement.to.clone());
    }
    if state_json(manifest_path, &manifest)? != publication.manifest_after.as_bytes() {
        return Err(migration_conflict(manifest_path, "migration changed unrelated host facts or original manifest bytes"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for cleanup in &publication.cleanup {
        let index = migration.moves.iter().position(|movement| movement.to == cleanup.source)
            .ok_or_else(|| migration_conflict(journal_path, "migration cleanup names a foreign state root"))?;
        if !seen.insert(index) || cleanup.destination != migration_scratch(home, &publication.operation_id, index, "empty")
            || cleanup.expected.digest_algorithm != DigestAlgorithm::Sha256 || !cleanup.expected.entries.is_empty()
        { return Err(migration_conflict(journal_path, "migration cleanup changed its exact empty predecessor")); }
    }
    validate_directory_receipts(journal_path, &format!("{}:empty", publication.operation_id), &publication.cleanup, &publication.cleanup_receipts)?;
    if !publication.receipts.is_empty() {
        validate_directory_receipts(journal_path, &format!("{}:move", publication.operation_id), &native_state_moves(migration)?, &publication.receipts)?;
    }
    if let Some(receipt) = &publication.manifest_receipt {
        validate_file_receipt(manifest_path, &format!("{}:manifest", publication.operation_id), Some(&state_hash(publication.manifest_before.as_bytes())),
            Some(publication.manifest_after.as_bytes()), receipt)?;
    }
    Ok(())
}

fn validate_file_receipt(path: &Path, operation: &str, before: Option<&str>, after: Option<&[u8]>, receipt: &PublicationReceipt) -> Result<(), HostManifestError> {
    if receipt.path != path || receipt.operation_id != operation || receipt.digest_algorithm != DigestAlgorithm::Sha256
        || receipt.before_hash.as_deref() != before || receipt.after_hash != after.map(state_hash)
        || receipt.bytes != after.map_or(0, |bytes| bytes.len() as u64) || receipt.outcome != PublicationOutcome::Applied || receipt.error.is_some()
    { return Err(migration_conflict(path, format!("native file publication was not Applied for its exact original request: {:?}", receipt.error))); }
    Ok(())
}

fn save_state_migration(path: &Path, migration: &StateLayoutMigration, saved: &mut Option<Vec<u8>>) -> Result<(), HostManifestError> {
    let bytes = state_json(path, migration)?;
    if saved.as_ref() == Some(&bytes) { return Ok(()); }
    let operation = format!("{}:journal:{}", migration.publication.as_ref().unwrap().operation_id, state_hash(&bytes));
    let before = saved.as_deref().map(state_hash);
    let receipt = publication::publish(path, &operation, before.as_deref(), &bytes).map_err(|source| migration_io(path, source))?;
    validate_file_receipt(path, &operation, before.as_deref(), Some(&bytes), &receipt)?;
    if read_guarded(path).map_err(|source| migration_io(path, source))? != bytes {
        return Err(migration_conflict(path, "migration journal changed after exact publication"));
    }
    *saved = Some(bytes);
    Ok(())
}

fn resume_state_move(journal_path: &Path, migration: &mut StateLayoutMigration, saved: &mut Option<Vec<u8>>, cleanup: bool) -> Result<(), HostManifestError> {
    let publication = migration.publication.as_ref().unwrap();
    let operation = format!("{}:{}", publication.operation_id, if cleanup { "empty" } else { "move" });
    let moves = if cleanup { publication.cleanup.clone() } else { native_state_moves(migration)? };
    if moves.is_empty() { return Ok(()); }
    let result = publication::publish_directory_moves(&operation, &moves).map_err(|source| migration_io(journal_path, source))?;
    validate_directory_receipts(journal_path, &operation, &moves, &result.receipts)?;
    let publication = migration.publication.as_mut().unwrap();
    let receipts = if cleanup { &mut publication.cleanup_receipts } else { &mut publication.receipts };
    if !result.receipts.starts_with(receipts) { return Err(migration_conflict(journal_path, "native publication replaced immutable original receipts")); }
    *receipts = result.receipts.clone();
    save_state_migration(journal_path, migration, saved)?;
    if result.error.is_some() || result.receipts.len() != moves.len() || result.receipts.iter().any(|receipt| receipt.outcome != PublicationOutcome::Applied) {
        return Err(migration_conflict(journal_path, format!("native state directory publication is incomplete: {:?}", result.error)));
    }
    Ok(())
}

fn apply_state_migration(home: &Path, manifest_path: &Path, journal_path: &Path, mut migration: StateLayoutMigration, mut saved: Option<Vec<u8>>) -> Result<(), HostManifestError> {
    let publication = migration.publication.as_ref().unwrap();
    let current = read_guarded(manifest_path).map_err(|source| migration_io(manifest_path, source))?;
    if current != publication.manifest_before.as_bytes() {
        let operation = format!("{}:manifest", publication.operation_id);
        let receipt = publication::read_receipt(manifest_path, &operation).map_err(|source| migration_io(manifest_path, source))?
            .ok_or_else(|| migration_conflict(manifest_path, "manifest changed without its original native publication proof"))?;
        validate_file_receipt(manifest_path, &operation, Some(&state_hash(publication.manifest_before.as_bytes())), Some(publication.manifest_after.as_bytes()), &receipt)?;
        if current != publication.manifest_after.as_bytes() { return Err(migration_conflict(manifest_path, "manifest changed after the original migration publication")); }
    }
    let state = home.join("state");
    exact_state_path(&state)?;
    publication::create_dir_all(&state).map_err(|source| migration_io(&state, source))?;
    for index in 0..migration.moves.len() {
        let source = &migration.publication.as_ref().unwrap().sources[index];
        if source.expected.is_some() { continue; }
        if std::fs::symlink_metadata(&source.path).is_ok() || state_snapshot(&migration.moves[index].from)?.is_some() {
            return Err(migration_conflict(&source.path, "new state root exists without its original native preimage; ownership is uncertain"));
        }
        exact_state_path(&source.path)?;
        publication::create_dir_all(&source.path).map_err(|error| migration_io(&source.path, error))?;
        let expected = state_snapshot(&source.path)?.ok_or_else(|| migration_conflict(&source.path, "new state root disappeared before its native pin"))?;
        if !expected.entries.is_empty() { return Err(migration_conflict(&source.path, "new state staging root contains foreign data")); }
        migration.publication.as_mut().unwrap().sources[index].expected = Some(expected);
        save_state_migration(journal_path, &migration, &mut saved)?;
    }
    resume_state_move(journal_path, &mut migration, &mut saved, true)?;
    for receipt in &migration.publication.as_ref().unwrap().cleanup_receipts {
        publication::reclaim_directory(&receipt.movement.destination, receipt, &receipt.movement.expected)
            .map_err(|source| migration_io(&receipt.movement.destination, source))?;
    }
    resume_state_move(journal_path, &mut migration, &mut saved, false)?;
    let moves = native_state_moves(&migration)?;
    let installed = ReadGuard::try_acquire(&[], &moves.iter().map(|movement| movement.destination.clone()).collect::<Vec<_>>())
        .map_err(|source| migration_io(manifest_path, source))?;
    for movement in &moves {
        exact_state_path(&movement.destination)?;
        let metadata = std::fs::symlink_metadata(&movement.destination).map_err(|source| migration_io(&movement.destination, source))?;
        #[cfg(unix)]
        let identity = { use std::os::unix::fs::MetadataExt; FileIdentity { device: metadata.dev(), inode: metadata.ino() } };
        #[cfg(not(unix))]
        return Err(migration_conflict(&movement.destination, "native state-root identity is unavailable"));
        #[cfg(unix)]
        if !metadata.is_dir() || metadata.file_type().is_symlink() || identity != movement.expected.identity
            || movement.source.try_exists().map_err(|source| migration_io(&movement.source, source))?
        { return Err(migration_conflict(&movement.destination, "installed state root differs from its original native identity")); }
    }
    let publication = migration.publication.as_ref().unwrap();
    let operation = format!("{}:manifest", publication.operation_id);
    let before = state_hash(publication.manifest_before.as_bytes());
    let receipt = publication::publish(manifest_path, &operation, Some(&before), publication.manifest_after.as_bytes())
        .map_err(|source| migration_io(manifest_path, source))?;
    validate_file_receipt(manifest_path, &operation, Some(&before), Some(publication.manifest_after.as_bytes()), &receipt)?;
    if read_guarded(manifest_path).map_err(|source| migration_io(manifest_path, source))? != publication.manifest_after.as_bytes() {
        return Err(migration_conflict(manifest_path, "published host manifest changed before migration completion"));
    }
    installed.verify().map_err(|source| migration_io(manifest_path, source))?;
    drop(installed);
    migration.publication.as_mut().unwrap().manifest_receipt = Some(receipt);
    save_state_migration(journal_path, &migration, &mut saved)?;
    let operation = format!("{}:journal-remove", migration.publication.as_ref().unwrap().operation_id);
    let before = state_hash(saved.as_ref().unwrap());
    let receipt = publication::remove(journal_path, &operation, &before).map_err(|source| migration_io(journal_path, source))?;
    validate_file_receipt(journal_path, &operation, Some(&before), None, &receipt)
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
    let target = std::fs::canonicalize(path.parent().ok_or_else(|| migration_conflict(path, "host manifest has no parent"))?)
        .map_err(|source| HostManifestError::Write { path: path.to_path_buf(), source })?.join(path.file_name().unwrap());
    let operation = format!("host-manifest-initialize:{}", uuid::Uuid::new_v4().simple());
    let receipt = publication::publish(&target, &operation, None, &bytes).map_err(|source| HostManifestError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    validate_file_receipt(&target, &operation, None, Some(&bytes), &receipt)?;
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
    fn exact_legacy_generated_state_roots_move_once_into_home_state() {
        let home_dir = tempfile::tempdir().expect("temp host home");
        let home_path = home_dir.path().canonicalize().unwrap();
        let home = home_path.as_path();
        let registry_dir = tempfile::tempdir().expect("temp registry");
        write_room_registry(registry_dir.path());
        let legacy = home.join("ledger/handler-interface-artifacts");
        let old_runs = legacy.join("runs");
        let old_ees = legacy.join("ee_instances");
        std::fs::create_dir_all(&old_runs).unwrap();
        std::fs::create_dir_all(&old_ees).unwrap();
        let original = publication::publish(&old_runs.join("run-a.json"), "original-run", None, b"run").unwrap();
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
        assert_eq!(publication::publish(&old_runs.join("run-a.json"), "original-run", None, b"run").unwrap(), original);
        assert!(!old_runs.exists());
        load_or_initialize_host_roots(home, registry_dir.path(), || unreachable!()).unwrap();
    }

    #[test]
    fn legacy_state_migration_refuses_a_nonempty_new_root_before_moving() {
        let home_dir = tempfile::tempdir().expect("temp host home");
        let home_path = home_dir.path().canonicalize().unwrap();
        let home = home_path.as_path();
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

    fn staged_state_migration(home: &Path, empty_destination: bool) -> (StateLayoutMigration, Option<Vec<u8>>) {
        let moves = state_layout_moves(home);
        let mut root_refs = BTreeMap::from([("unrelated_root".into(), home.join("unrelated"))]);
        for movement in &moves {
            std::fs::create_dir_all(&movement.from).unwrap();
            std::fs::write(movement.from.join("record.json"), &movement.slot).unwrap();
            root_refs.insert(movement.slot.clone(), movement.from.clone());
        }
        if empty_destination { publication::create_dir_all(&moves[0].to).unwrap(); }
        let manifest_path = host_manifest_path(home);
        std::fs::write(&manifest_path, state_json(&manifest_path, &HostManifest { host_manifest_version: HOST_MANIFEST_VERSION, root_refs }).unwrap()).unwrap();
        let mut migration = StateLayoutMigration { schema_version: STATE_MIGRATION_VERSION, moves, publication: None };
        prepare_state_migration(home, &manifest_path, &mut migration, false).unwrap();
        let mut saved = None;
        save_state_migration(&home.join(STATE_MIGRATION_FILE_NAME), &migration, &mut saved).unwrap();
        (migration, saved)
    }

    #[test]
    fn state_migration_restarts_original_moves_after_cleanup_and_preserves_history() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().canonicalize().unwrap();
        let (mut migration, mut saved) = staged_state_migration(&home, true);
        let journal = home.join(STATE_MIGRATION_FILE_NAME);
        let native = migration.publication.as_ref().unwrap();
        let cleanup = publication::publish_directory_moves(&format!("{}:empty", native.operation_id), &native.cleanup).unwrap();
        assert!(cleanup.error.is_none());
        resume_state_move(&journal, &mut migration, &mut saved, true).unwrap();
        let receipt = &migration.publication.as_ref().unwrap().cleanup_receipts[0];
        publication::reclaim_directory(&receipt.movement.destination, receipt, &receipt.movement.expected).unwrap();
        let moves = native_state_moves(&migration).unwrap();
        let operation = format!("{}:move", migration.publication.as_ref().unwrap().operation_id);
        let moved = publication::publish_directory_moves(&operation, &moves).unwrap();
        assert!(moved.error.is_none());
        migrate_legacy_state_layout(&home, &host_manifest_path(&home)).unwrap();
        assert_eq!(publication::publish_directory_moves(&operation, &moves).unwrap(), moved);
        assert_eq!(publication::publish_directory_moves(&format!("{}:empty", migration.publication.as_ref().unwrap().operation_id),
            &migration.publication.as_ref().unwrap().cleanup).unwrap(), cleanup);
        assert!(!journal.exists());
        assert_eq!(read_host_manifest(&host_manifest_path(&home)).unwrap().root_refs["unrelated_root"], home.join("unrelated"));
        for movement in &migration.moves { assert_eq!(std::fs::read_to_string(movement.to.join("record.json")).unwrap(), movement.slot); }
    }

    #[test]
    fn state_migration_preserves_foreign_directory_and_manifest_successors() {
        for change in ["destination", "manifest", "equal_successor"] {
            let temporary = tempfile::tempdir().unwrap();
            let home = temporary.path().canonicalize().unwrap();
            let (migration, _) = staged_state_migration(&home, false);
            publication::create_dir_all(&home.join("state")).unwrap();
            let manifest_path = host_manifest_path(&home);
            if change == "manifest" {
                let mut manifest = read_host_manifest(&manifest_path).unwrap();
                manifest.root_refs.insert("concurrent_root".into(), home.join("concurrent"));
                std::fs::write(&manifest_path, state_json(&manifest_path, &manifest).unwrap()).unwrap();
            } else {
                if change == "equal_successor" {
                    let result = publication::publish_directory_moves(&format!("{}:move", migration.publication.as_ref().unwrap().operation_id), &native_state_moves(&migration).unwrap()).unwrap();
                    assert!(result.error.is_none());
                    std::fs::rename(&migration.moves[0].to, home.join("original-root")).unwrap();
                }
                std::fs::create_dir_all(&migration.moves[0].to).unwrap();
                std::fs::write(migration.moves[0].to.join("record.json"), &migration.moves[0].slot).unwrap();
            }
            let before = std::fs::read(&manifest_path).unwrap();
            assert!(migrate_legacy_state_layout(&home, &manifest_path).is_err());
            assert_eq!(std::fs::read(&manifest_path).unwrap(), before);
            assert!(home.join(STATE_MIGRATION_FILE_NAME).exists());
            let preserved = if change == "equal_successor" { home.join("original-root") } else { migration.moves[0].from.clone() };
            assert_eq!(std::fs::read_to_string(preserved.join("record.json")).unwrap(), migration.moves[0].slot);
            if change != "manifest" { assert!(migration.moves[0].to.join("record.json").exists()); }
        }
    }

    #[test]
    fn legacy_migration_journal_only_adopts_an_unmoved_exact_layout() {
        for already_moved in [false, true] {
            let temporary = tempfile::tempdir().unwrap();
            let home = temporary.path().canonicalize().unwrap();
            let (mut migration, _) = staged_state_migration(&home, false);
            migration.schema_version = 1;
            migration.publication = None;
            let journal = home.join(STATE_MIGRATION_FILE_NAME);
            std::fs::write(&journal, state_json(&journal, &migration).unwrap()).unwrap();
            if already_moved {
                std::fs::create_dir_all(home.join("state")).unwrap();
                std::fs::rename(&migration.moves[0].from, &migration.moves[0].to).unwrap();
            }
            let before = std::fs::read(host_manifest_path(&home)).unwrap();
            let result = migrate_legacy_state_layout(&home, &host_manifest_path(&home));
            if already_moved {
                assert!(result.unwrap_err().to_string().contains("without original native proof"));
                assert_eq!(std::fs::read(host_manifest_path(&home)).unwrap(), before);
                assert!(journal.exists() && migration.moves[0].to.join("record.json").exists());
            } else { result.unwrap(); assert!(!journal.exists()); }
        }
    }

    #[test]
    fn missing_state_roots_publish_empty_stages_but_unpinned_stages_refuse() {
        for interrupted in [false, true] {
            let temporary = tempfile::tempdir().unwrap();
            let home = temporary.path().canonicalize().unwrap();
            let manifest_path = host_manifest_path(&home);
            let moves = state_layout_moves(&home);
            let manifest = HostManifest { host_manifest_version: HOST_MANIFEST_VERSION,
                root_refs: moves.iter().map(|movement| (movement.slot.clone(), movement.from.clone())).collect() };
            std::fs::write(&manifest_path, state_json(&manifest_path, &manifest).unwrap()).unwrap();
            if interrupted {
                let mut migration = StateLayoutMigration { schema_version: STATE_MIGRATION_VERSION, moves: moves.clone(), publication: None };
                prepare_state_migration(&home, &manifest_path, &mut migration, false).unwrap();
                save_state_migration(&home.join(STATE_MIGRATION_FILE_NAME), &migration, &mut None).unwrap();
                let stage = &migration.publication.as_ref().unwrap().sources[0].path;
                publication::create_dir_all(stage).unwrap();
                assert!(migrate_legacy_state_layout(&home, &manifest_path).unwrap_err().to_string().contains("ownership is uncertain"));
                assert!(stage.is_dir());
                assert_eq!(read_host_manifest(&manifest_path).unwrap(), manifest);
            } else {
                migrate_legacy_state_layout(&home, &manifest_path).unwrap();
                for movement in moves {
                    assert!(state_snapshot(&movement.to).unwrap().unwrap().entries.is_empty());
                    assert!(!movement.from.exists());
                }
            }
        }
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
