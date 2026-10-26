use super::*;
use std::path::Component;

const MAX_MOVES: usize = 128;
const MAX_ENTRIES: usize = 16384;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectoryEntryKind { Directory, File, Symlink }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryEntry {
    pub relative_path: PathBuf,
    pub kind: DirectoryEntryKind,
    pub identity: FileIdentity,
    pub mode: u32,
    pub bytes: u64,
    pub digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryPreimage {
    pub identity: FileIdentity,
    pub mode: u32,
    pub digest_algorithm: DigestAlgorithm,
    pub entries: Vec<DirectoryEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryMove {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub expected: DirectoryPreimage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryMoveReceipt {
    pub schema_version: u32,
    pub operation_id: String,
    pub index: usize,
    pub plan_hash: String,
    pub movement: DirectoryMove,
    pub outcome: PublicationOutcome,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryPublicationResult {
    pub receipts: Vec<DirectoryMoveReceipt>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema_version: u32,
    operation_id: String,
    plan_hash: String,
    moves: Vec<DirectoryMove>,
    parents: BTreeMap<PathBuf, FileIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    schema_version: u32,
    operation_id: String,
    plan_hash: String,
    plan_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedMove {
    schema_version: u32,
    plan_hash: String,
    index: usize,
}

fn mode(metadata: &Metadata) -> u32 {
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; metadata.permissions().mode() & 0o7777 }
    #[cfg(not(unix))] { let _ = metadata; 0 }
}

fn ordinary_path(path: &Path) -> io::Result<()> {
    namespace::validate_absolute(path)?;
    if path.file_name().is_none() || path.to_str().is_none()
        || path.components().any(|part| part.as_os_str() == COORDINATION_DIRECTORY)
    { return Err(invalid("directory publication requires an ordinary absolute UTF-8 path")); }
    Ok(())
}

pub(super) fn exact_path(path: &Path) -> io::Result<()> {
    ordinary_path(path)?;
    if !namespace::same_path(&namespace::normalize_entry(path)?, path) {
        return Err(invalid("directory publication rejects aliased endpoint ancestors"));
    }
    Ok(())
}

pub(super) fn validate_preimage(preimage: &DirectoryPreimage) -> io::Result<()> {
    if preimage.mode > 0o7777 || preimage.entries.len() > MAX_ENTRIES {
        return Err(invalid("directory manifest exceeds its declared bounds"));
    }
    let mut kinds = BTreeMap::new();
    let mut previous: Option<&Path> = None;
    for entry in &preimage.entries {
        let path = &entry.relative_path;
        if path.as_os_str().is_empty() || path.to_str().is_none()
            || path.components().any(|part| !matches!(part, Component::Normal(_)))
            || previous.is_some_and(|previous| previous >= path.as_path()) || entry.mode > 0o7777
        { return Err(invalid("directory manifest has an invalid or duplicate relative path")); }
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            if kinds.get(parent) != Some(&DirectoryEntryKind::Directory) {
                return Err(invalid("directory manifest entry has no declared directory parent"));
            }
        }
        validate_hash(entry.digest.as_deref())?;
        if match entry.kind {
            DirectoryEntryKind::Directory => entry.bytes != 0 || entry.digest.is_some(),
            DirectoryEntryKind::File | DirectoryEntryKind::Symlink => entry.digest.is_none(),
        } { return Err(invalid("directory manifest entry has invalid typed content evidence")); }
        kinds.insert(path.clone(), entry.kind.clone());
        previous = Some(path);
    }
    Ok(())
}

fn validate_move(movement: &DirectoryMove) -> io::Result<()> {
    ordinary_path(&movement.source)?;
    ordinary_path(&movement.destination)?;
    validate_preimage(&movement.expected)?;
    if movement.source == movement.destination || movement.destination.starts_with(&movement.source)
        || movement.source.starts_with(&movement.destination)
        || movement.expected.entries.iter().any(|entry| entry.relative_path.components().any(|part| part.as_os_str() == COORDINATION_DIRECTORY))
    { return Err(invalid("directory movement endpoints or manifest overlap reserved publication state")); }
    Ok(())
}

fn plan_hash(moves: &[DirectoryMove]) -> io::Result<String> {
    Ok(hash(&serde_json::to_vec(moves).map_err(|error| invalid(error.to_string()))?))
}

fn validate_request(plan: &Plan, path: &Path) -> io::Result<()> {
    validate_operation(&plan.operation_id)?;
    if plan.schema_version != 1 || plan.moves.is_empty() || plan.moves.len() > MAX_MOVES
        || plan.plan_hash != plan_hash(&plan.moves)?
        || plan.moves.iter().map(|movement| movement.expected.entries.len()).sum::<usize>() > MAX_ENTRIES
    { return Err(invalid("invalid bounded directory publication plan")); }
    let root = path.parent().ok_or_else(|| invalid("directory plan has no metadata parent"))?;
    if root.file_name().is_none_or(|name| name != COORDINATION_DIRECTORY)
        || path.file_name().is_none_or(|name| name != format!("dir-{}.plan", hash(plan.operation_id.as_bytes())).as_str())
        || root.parent() != plan.moves[0].source.parent()
    { return Err(invalid("directory plan has different original coordinates")); }
    namespace::validate_absolute(path)?;
    let mut sources = BTreeSet::new();
    let mut destinations = BTreeSet::new();
    for (index, movement) in plan.moves.iter().enumerate() {
        validate_move(movement)?;
        if !sources.insert(movement.source.clone()) || !destinations.insert(movement.destination.clone())
            || plan.moves[..index].iter().any(|prior| prior.destination == movement.source)
        { return Err(invalid("directory batch repeats a source, destination, or previously installed source")); }
    }
    let endpoints = plan.moves.iter().flat_map(|movement| [&movement.source, &movement.destination]).collect::<Vec<_>>();
    for endpoint in &endpoints {
        for other in &endpoints {
            if endpoint != other && endpoint.starts_with(other) {
                return Err(invalid("directory batch contains nested endpoints"));
            }
        }
        for parent in plan.moves.iter().flat_map(|movement| [movement.source.parent().unwrap(), movement.destination.parent().unwrap()]) {
            if parent.starts_with(endpoint) { return Err(invalid("directory batch moves a coordination parent")); }
        }
    }
    Ok(())
}

fn validate_plan(plan: &Plan, path: &Path) -> io::Result<()> {
    validate_request(plan, path)?;
    let parents = plan.moves.iter().flat_map(|movement| [movement.source.parent().unwrap(), movement.destination.parent().unwrap()]).collect::<BTreeSet<_>>();
    if plan.parents.keys().map(PathBuf::as_path).collect::<BTreeSet<_>>() != parents {
        return Err(invalid("directory plan lost its exact endpoint parent identities"));
    }
    let members = plan.moves.iter().flat_map(|movement| std::iter::once(&movement.expected.identity).chain(movement.expected.entries.iter()
        .filter(|entry| entry.kind == DirectoryEntryKind::Directory).map(|entry| &entry.identity))).collect::<BTreeSet<_>>();
    if plan.parents.values().any(|identity| members.contains(identity)) {
        return Err(invalid("directory publication coordination parent aliases a moved subtree"));
    }
    Ok(())
}

fn parents_match(plan: &Plan) -> io::Result<bool> {
    for (path, identity) in &plan.parents {
        let parent = match native::directory(path) {
            Ok(parent) => parent,
            Err(error) if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => return Ok(false),
            Err(error) => return Err(error),
        };
        if FileIdentity::of_file(&parent)? != *identity { return Ok(false); }
    }
    Ok(true)
}

pub(super) fn validate_receipt(receipt: &DirectoryMoveReceipt) -> io::Result<()> {
    validate_operation(&receipt.operation_id)?;
    validate_hash(Some(&receipt.plan_hash))?;
    validate_move(&receipt.movement)?;
    if receipt.schema_version != 1 || receipt.index >= MAX_MOVES
        || (receipt.outcome == PublicationOutcome::Applied) != receipt.error.is_none()
    { return Err(invalid("directory receipt has invalid effect evidence")); }
    Ok(())
}

fn collect(root: &File, relative: &Path, algorithm: DigestAlgorithm, metadata: bool, entries: &mut Vec<DirectoryEntry>) -> io::Result<()> {
    let before = root.metadata()?;
    for name in native::names(root, MAX_ENTRIES)? {
        let relative_path = relative.join(&name);
        if name == COORDINATION_DIRECTORY && !metadata {
            let state = native::state(root, &name)?;
            let child = native::child(root, &name, true)?;
            if state.kind != DirectoryEntryKind::Directory || !native::private_directory(&state, &child)?
                || FileIdentity::of_file(&child)? != state.identity
            { return Err(invalid("reserved publication metadata is not an exact private directory")); }
            continue;
        }
        let item = capture_entry_at(root, &name, relative_path.clone(), algorithm)?;
        let child = if item.kind == DirectoryEntryKind::Directory {
            let child = native::child(root, &name, true)?;
            if FileIdentity::of_file(&child)? != item.identity { return Err(invalid("manifest directory changed identity")); }
            Some(child)
        } else { None };
        entries.push(item);
        if entries.len() > MAX_ENTRIES { return Err(invalid("directory manifest exceeds its entry limit")); }
        if let Some(child) = child { collect(&child, &relative_path, algorithm, metadata, entries)?; }
    }
    let after = root.metadata()?;
    if FileVersion::of(&before)? != FileVersion::of(&after)? || mode(&before) != mode(&after) {
        return Err(invalid("directory membership changed while its manifest was captured"));
    }
    Ok(())
}

fn capture_entry_at(parent: &File, name: &std::ffi::OsStr, relative_path: PathBuf, algorithm: DigestAlgorithm) -> io::Result<DirectoryEntry> {
    let before = native::state(parent, name)?;
    let (bytes, digest) = match &before.kind {
        DirectoryEntryKind::Directory => (0, None),
        DirectoryEntryKind::File => {
            let file = native::child(parent, name, false)?;
            if FileIdentity::of_file(&file)? != before.identity { return Err(invalid("manifest file changed identity")); }
            (before.bytes, Some(digest_file(&file, algorithm)?))
        },
        DirectoryEntryKind::Symlink => {
            let bytes = native::link_target(parent, name)?;
            (bytes.len() as u64, Some(algorithm.digest(&bytes)))
        },
    };
    if native::state(parent, name)? != before { return Err(invalid("manifest entry changed during capture")); }
    Ok(DirectoryEntry { relative_path, kind: before.kind, identity: before.identity, mode: before.mode, bytes, digest })
}

pub(super) fn capture_entry(path: &Path, relative_path: PathBuf, algorithm: DigestAlgorithm) -> io::Result<DirectoryEntry> {
    let parent = native::directory(path.parent().ok_or_else(|| invalid("manifest entry has no parent"))?)?;
    capture_entry_at(&parent, path.file_name().ok_or_else(|| invalid("manifest entry has no name"))?, relative_path, algorithm)
}

pub(super) fn capture(path: &Path, algorithm: DigestAlgorithm, metadata: bool, identity: &FileIdentity) -> io::Result<DirectoryPreimage> {
    let root = native::directory(path)?;
    let before = root.metadata()?;
    if FileIdentity::of_file(&root)? != *identity { return Err(invalid("directory publication source changed its held identity")); }
    let mut entries = Vec::new();
    collect(&root, Path::new(""), algorithm, metadata, &mut entries)?;
    entries.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    let after = root.metadata()?;
    if FileVersion::of(&before)? != FileVersion::of(&after)? || mode(&before) != mode(&after) {
        return Err(invalid("directory root changed during manifest capture"));
    }
    let preimage = DirectoryPreimage { identity: identity.clone(), mode: mode(&before), digest_algorithm: algorithm, entries };
    validate_preimage(&preimage)?;
    Ok(preimage)
}

pub fn snapshot_directory(path: &Path, algorithm: DigestAlgorithm) -> io::Result<DirectoryPreimage> {
    exact_path(path)?;
    let path = path.to_path_buf();
    let guard = NamespaceGuard::acquire(&[(path.clone(), true)], true)?;
    exact_path(&path)?;
    check_fences(std::slice::from_ref(&path), None, true)?;
    let preimage = capture(&path, algorithm, false, guard.identity(&path)?)?;
    guard.verify()?;
    drop(guard);
    let mut requests = vec![(path.clone(), true)];
    for entry in &preimage.entries {
        if entry.kind == DirectoryEntryKind::Directory { namespace::member_directory(&mut requests, path.join(&entry.relative_path))?; }
    }
    let guard = NamespaceGuard::acquire(&requests, true)?;
    exact_path(&path)?;
    check_fences(std::slice::from_ref(&path), None, true)?;
    let members = preimage.entries.iter().filter(|entry| entry.kind == DirectoryEntryKind::File)
        .map(|entry| (path.join(&entry.relative_path), entry.identity.clone())).collect::<Vec<_>>();
    let files = namespace::TreeFiles::acquire(&members, true)?;
    if capture(&path, algorithm, false, &preimage.identity)? != preimage { return Err(invalid("directory changed before its complete manifest guard was acquired")); }
    guard.verify()?;
    files.verify()?;
    Ok(preimage)
}

pub(super) fn absent(path: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error),
    }
}

fn matches(path: &Path, expected: &DirectoryPreimage) -> io::Result<bool> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if !metadata.is_dir() || FileIdentity::of_path(path)? != expected.identity { return Ok(false); }
    Ok(capture(path, expected.digest_algorithm, false, &expected.identity)? == *expected)
}

fn receipt_for(plan: &Plan, index: usize, outcome: PublicationOutcome, error: Option<String>) -> DirectoryMoveReceipt {
    DirectoryMoveReceipt { schema_version: 1, operation_id: plan.operation_id.clone(), index,
        plan_hash: plan.plan_hash.clone(), movement: plan.moves[index].clone(), outcome, error }
}

fn existing_receipt(plan: &Plan, index: usize) -> io::Result<Option<DirectoryMoveReceipt>> {
    let receipt = history::find_directory(&plan.moves[index].source, &plan.operation_id, index)?;
    if let Some(receipt) = &receipt {
        validate_receipt(receipt)?;
        if receipt.plan_hash != plan.plan_hash || receipt.movement != plan.moves[index] {
            return Err(invalid("directory operation identity was reused for a different batch"));
        }
    }
    Ok(receipt)
}

fn save_receipt(receipt: &DirectoryMoveReceipt) -> io::Result<()> {
    let root = coordination_root(receipt.movement.source.parent().unwrap())?;
    history::save_directory(&root, receipt)
}

fn prepared_path(plan_path: &Path, index: usize) -> PathBuf { plan_path.with_extension(format!("{index}.prepared")) }
pub(super) fn marker(endpoint: &Path) -> PathBuf { endpoint.parent().unwrap().join(COORDINATION_DIRECTORY).join(format!("{}.directory", hash(endpoint.file_name().unwrap().as_encoded_bytes()))) }
fn reference(plan: &Plan, path: &Path) -> Reference {
    Reference { schema_version: 1, operation_id: plan.operation_id.clone(), plan_hash: plan.plan_hash.clone(), plan_path: path.to_path_buf() }
}

fn load_plan(path: &Path) -> io::Result<Plan> {
    validate_coordination_root(path.parent().ok_or_else(|| invalid("directory plan has no parent"))?)?;
    let plan = read_record_bounded(path, history::DIRECTORY_LIMIT)?.ok_or_else(|| invalid("directory fence lost its durable plan"))?;
    validate_plan(&plan, path)?;
    Ok(plan)
}

fn verify_prepared(plan: &Plan, index: usize, prepared: &PreparedMove) -> io::Result<()> {
    if prepared.schema_version != 1 || prepared.plan_hash != plan.plan_hash || prepared.index != index {
        return Err(invalid("prepared directory move differs from its durable plan"));
    }
    Ok(())
}

fn retire(plan: &Plan, plan_path: &Path) -> io::Result<()> {
    let expected = reference(plan, plan_path);
    for endpoint in plan.moves.iter().flat_map(|movement| [&movement.source, &movement.destination]).collect::<BTreeSet<_>>() {
        let path = marker(endpoint);
        if let Some(found) = read_record::<Reference>(&path)? {
            if found != expected { return Err(invalid("directory fence was replaced by a different operation")); }
            std::fs::remove_file(&path)?;
            sync_directory(path.parent().unwrap())?;
        }
    }
    for index in 0..plan.moves.len() {
        let path = prepared_path(plan_path, index);
        if let Some(prepared) = read_record::<PreparedMove>(&path)? {
            verify_prepared(plan, index, &prepared)?;
            std::fs::remove_file(path)?;
        }
    }
    sync_directory(plan_path.parent().unwrap())?;
    std::fs::remove_file(plan_path)?;
    sync_directory(plan_path.parent().unwrap())
}

pub(super) fn settle_path(path: &Path, resume: Option<&str>, wait: bool) -> io::Result<Vec<DirectoryMoveReceipt>> {
    let plan = load_plan(path)?;
    let _lock = HeldFile::acquire(path.with_extension("lock"), true, wait)?;
    let mut receipts = Vec::new();
    let mut stopped = false;
    for index in 0..plan.moves.len() {
        let prepared: Option<PreparedMove> = read_record(&prepared_path(path, index))?;
        if let Some(prepared) = &prepared { verify_prepared(&plan, index, prepared)?; }
        if let Some(receipt) = existing_receipt(&plan, index)? {
            if receipt.outcome == PublicationOutcome::Uncertain { return Err(invalid("directory publication remains uncertain")); }
            if stopped && receipt.outcome == PublicationOutcome::Applied { return Err(invalid("directory batch applied a step after a stopped predecessor")); }
            stopped |= receipt.outcome != PublicationOutcome::Applied;
            receipts.push(receipt);
            continue;
        }
        if stopped && prepared.is_some() { return Err(invalid("directory batch prepared a step after a stopped predecessor")); }
        let movement = &plan.moves[index];
        let receipt = if prepared.is_some() {
            let observed = (|| -> io::Result<Option<bool>> {
                if !parents_match(&plan)? { return Ok(None); }
                if absent(&movement.source)? && matches(&movement.destination, &movement.expected)? {
                    sync_directory(movement.source.parent().unwrap())?;
                    sync_directory(movement.destination.parent().unwrap())?;
                    Ok(Some(true))
                } else if absent(&movement.destination)? && matches(&movement.source, &movement.expected)? { Ok(Some(false)) }
                else { Ok(None) }
            })();
            match observed {
                Ok(Some(true)) => receipt_for(&plan, index, PublicationOutcome::Applied, None),
                Ok(Some(false)) => {
                    if resume == Some(plan.operation_id.as_str()) { return Ok(receipts); }
                    receipt_for(&plan, index, PublicationOutcome::NotApplied, Some("publisher stopped before moving the exact prepared directory".into()))
                },
                observation => receipt_for(&plan, index, PublicationOutcome::Uncertain, Some(match observation {
                    Err(error) => format!("directory movement cannot be observed with its exact manifest: {error}"),
                    _ => "directory movement cannot be attributed to its exact manifest and endpoints".into(),
                })),
            }
        } else {
            if !stopped && resume == Some(plan.operation_id.as_str()) { return Ok(receipts); }
            receipt_for(&plan, index, PublicationOutcome::NotApplied, Some("directory batch stopped before preparing this step".into()))
        };
        save_receipt(&receipt)?;
        stopped |= receipt.outcome != PublicationOutcome::Applied;
        let uncertain = receipt.outcome == PublicationOutcome::Uncertain;
        receipts.push(receipt);
        if uncertain { return Err(invalid("directory publication remains uncertain; endpoints and descendants are fenced")); }
    }
    retire(&plan, path)?;
    Ok(receipts)
}

pub(super) fn settle_reference(path: &Path, resume: Option<&str>, wait: bool) -> io::Result<()> {
    let Some(found): Option<Reference> = read_record(path)? else { return Ok(()); };
    if found.schema_version != 1 { return Err(invalid("unsupported directory publication fence")); }
    let plan = load_plan(&found.plan_path)?;
    let canonical = namespace::normalize_entry(path)?;
    if found != reference(&plan, &found.plan_path)
        || !plan.moves.iter().flat_map(|movement| [&movement.source, &movement.destination]).any(|endpoint| marker(endpoint) == canonical)
    { return Err(invalid("directory publication fence has different original endpoints")); }
    settle_path(&found.plan_path, resume, wait).map(|_| ())
}

pub(super) fn check_fences(paths: &[PathBuf], resume: Option<&str>, wait: bool) -> io::Result<()> {
    check_fences_reclaim(paths, resume, wait, None)
}

pub(super) fn check_fences_reclaim(paths: &[PathBuf], resume: Option<&str>, wait: bool, own_reclaim: Option<&Path>) -> io::Result<()> {
    let mut checked = BTreeSet::new();
    for path in paths {
        if path.parent().is_none() { continue; }
        namespace::validate_absolute(path)?;
        for original in [true, false] {
            let normalized;
            let path = if original { path.as_path() } else { normalized = namespace::normalize_entry(path)?; &normalized };
            for endpoint in path.ancestors().filter(|path| path.file_name().is_some()) {
                let root = endpoint.parent().unwrap().join(COORDINATION_DIRECTORY);
                if absent(&root)? { continue; }
                validate_coordination_root(&root)?;
                let marker = marker(endpoint);
                if checked.insert(marker.clone()) { settle_reference(&marker, resume, wait)?; }
                if own_reclaim != Some(endpoint) { reclaim::check_fence(endpoint)?; }
            }
        }
    }
    Ok(())
}

fn rename_no_replace(movement: &DirectoryMove, parents: &BTreeMap<PathBuf, FileIdentity>) -> io::Result<()> {
    let source = &movement.source;
    let destination = &movement.destination;
    #[cfg(unix)] {
        use std::ffi::CString;
        use std::os::fd::AsRawFd;
        let parent = |path: &Path| native::pinned_directory(path, parents.get(path).ok_or_else(|| invalid("move lost its original parent identity"))?);
        let source_parent = parent(source.parent().ok_or_else(|| invalid("move source has no parent"))?)?;
        let destination_parent = parent(destination.parent().ok_or_else(|| invalid("move destination has no parent"))?)?;
        let source_name = CString::new(source.file_name().ok_or_else(|| invalid("move source has no name"))?.as_encoded_bytes()).map_err(|_| invalid("move source contains NUL"))?;
        let destination_name = CString::new(destination.file_name().ok_or_else(|| invalid("move destination has no name"))?.as_encoded_bytes()).map_err(|_| invalid("move destination contains NUL"))?;
        let state = native::state(&source_parent, source.file_name().unwrap())?;
        if state.kind != DirectoryEntryKind::Directory || state.identity != movement.expected.identity {
            return Err(invalid("native move source differs from its original inode"));
        }
        #[cfg(target_vendor = "apple")]
        let result = unsafe { libc::renameatx_np(source_parent.as_raw_fd(), source_name.as_ptr(), destination_parent.as_raw_fd(), destination_name.as_ptr(), libc::RENAME_EXCL) };
        #[cfg(target_os = "linux")]
        let result = unsafe { libc::renameat2(source_parent.as_raw_fd(), source_name.as_ptr(), destination_parent.as_raw_fd(), destination_name.as_ptr(), libc::RENAME_NOREPLACE) };
        #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
        return Err(io::Error::new(io::ErrorKind::Unsupported, "atomic no-replace directory publication is unavailable"));
        #[cfg(any(target_vendor = "apple", target_os = "linux"))]
        return if result == 0 { crate::sync_ordered(&source_parent)?; crate::sync_ordered(&destination_parent) } else { Err(io::Error::last_os_error()) };
    }
    // Windows renames the source directory's handle into the destination
    // parent handle without replacing (FileRenameInformation).
    #[cfg(windows)] {
        let parent = |path: &Path| native::pinned_directory(path, parents.get(path).ok_or_else(|| invalid("move lost its original parent identity"))?);
        let source_parent = parent(source.parent().ok_or_else(|| invalid("move source has no parent"))?)?;
        let destination_parent = parent(destination.parent().ok_or_else(|| invalid("move destination has no parent"))?)?;
        let source_name = source.file_name().ok_or_else(|| invalid("move source has no name"))?;
        let destination_name = destination.file_name().ok_or_else(|| invalid("move destination has no name"))?;
        let state = native::state(&source_parent, source_name)?;
        if state.kind != DirectoryEntryKind::Directory || state.identity != movement.expected.identity {
            return Err(invalid("native move source differs from its original inode"));
        }
        octopus_private_fs::windows::beneath::rename_no_replace(&source_parent, source_name, &destination_parent, destination_name)?;
        crate::sync_ordered(&source_parent)?;
        crate::sync_ordered(&destination_parent)
    }
    #[cfg(not(any(unix, windows)))] { let _ = (source, destination, parents); Err(io::Error::new(io::ErrorKind::Unsupported, "directory publication requires native no-replace moves")) }
}

fn sync_preimage(path: &Path, expected: &DirectoryPreimage) -> io::Result<()> {
    for entry in &expected.entries {
        if entry.kind == DirectoryEntryKind::File {
            let entry_path = path.join(&entry.relative_path);
            let parent = native::directory(entry_path.parent().unwrap())?;
            let file = native::child(&parent, entry_path.file_name().unwrap(), false)?;
            if FileIdentity::of_file(&file)? != entry.identity { return Err(invalid("directory member changed before durable preparation")); }
            crate::sync_ordered(&file)?;
        }
    }
    for entry in expected.entries.iter().rev() {
        if entry.kind == DirectoryEntryKind::Directory { crate::sync_ordered(&native::pinned_directory(&path.join(&entry.relative_path), &entry.identity)?)?; }
    }
    crate::sync_ordered(&native::pinned_directory(path, &expected.identity)?)
}

fn result(plan: &Plan, error: Option<String>) -> io::Result<DirectoryPublicationResult> {
    let receipts = (0..plan.moves.len()).map(|index| existing_receipt(plan, index))
        .collect::<io::Result<Vec<_>>>()?.into_iter().flatten().collect::<Vec<_>>();
    let error = error.or_else(|| receipts.iter().find_map(|receipt| receipt.error.clone()));
    Ok(DirectoryPublicationResult { receipts, error })
}

fn original_result(plan: &Plan) -> io::Result<Option<DirectoryPublicationResult>> {
    let result = result(plan, None)?;
    Ok((result.receipts.len() == plan.moves.len()).then_some(result))
}

fn execute(plan: &Plan, plan_path: &Path, index: usize, namespace: &mut NamespaceGuard, files: &mut namespace::TreeFiles) -> io::Result<()> {
    let movement = &plan.moves[index];
    exact_path(&movement.source)?;
    exact_path(&movement.destination)?;
    if !parents_match(plan)? || !absent(&movement.destination)? || !matches(&movement.source, &movement.expected)? {
        return Err(invalid("directory move predecessor or destination differs from the exact requested manifest"));
    }
    let outside = coordination_root(movement.source.parent().unwrap())?;
    history::export_tree(&movement.source, &movement.expected.identity, &outside)?;
    if !absent(&movement.destination)? || !matches(&movement.source, &movement.expected)? {
        return Err(invalid("directory move changed during history export"));
    }
    sync_preimage(&movement.source, &movement.expected)?;
    let prepared = PreparedMove { schema_version: 1, plan_hash: plan.plan_hash.clone(), index };
    write_record_once(&prepared_path(plan_path, index), &prepared)?;
    #[cfg(test)] interrupt_at(Interruption::DirectoryPrepared(index));
    namespace.verify()?;
    files.verify()?;
    exact_path(&movement.source)?;
    exact_path(&movement.destination)?;
    rename_no_replace(movement, &plan.parents)?;
    namespace.moved(&movement.source, &movement.destination);
    files.moved(&movement.source, &movement.destination);
    namespace.verify()?;
    files.verify()?;
    #[cfg(test)] interrupt_at(Interruption::DirectoryInstalled(index));
    let receipt = if absent(&movement.source)? && matches(&movement.destination, &movement.expected)? {
        receipt_for(plan, index, PublicationOutcome::Applied, None)
    } else {
        receipt_for(plan, index, PublicationOutcome::Uncertain, Some("installed directory lost its exact original inode or manifest".into()))
    };
    save_receipt(&receipt)?;
    if receipt.outcome == PublicationOutcome::Uncertain { return Err(invalid("directory movement remains uncertain")); }
    Ok(())
}

pub fn publish_directory_moves(operation_id: &str, moves: &[DirectoryMove]) -> io::Result<DirectoryPublicationResult> {
    validate_operation(operation_id)?;
    if moves.is_empty() { return Err(invalid("directory publication requires at least one move")); }
    let make_plan = |moves: Vec<DirectoryMove>| -> io::Result<(Plan, PathBuf)> {
        let path = moves[0].source.parent().ok_or_else(|| invalid("directory source has no parent"))?
            .join(COORDINATION_DIRECTORY).join(format!("dir-{}.plan", hash(operation_id.as_bytes())));
        let plan = Plan { schema_version: 1, operation_id: operation_id.into(), plan_hash: plan_hash(&moves)?, moves, parents: BTreeMap::new() };
        validate_request(&plan, &path)?;
        Ok((plan, path))
    };
    let (mut plan, plan_path) = make_plan(moves.to_vec())?;
    if let Some(result) = original_result(&plan)? { return Ok(result); }
    for movement in &plan.moves {
        exact_path(&movement.source)?;
        exact_path(&movement.destination)?;
    }
    let endpoints = plan.moves.iter().flat_map(|movement| [movement.source.clone(), movement.destination.clone()]).collect::<BTreeSet<_>>();
    let mut requests = endpoints.iter().flat_map(|path| [(path.parent().unwrap().to_path_buf(), true), (path.clone(), true)]).collect::<Vec<_>>();
    for movement in &plan.moves {
        for entry in &movement.expected.entries {
            if entry.kind == DirectoryEntryKind::Directory {
                for endpoint in [&movement.source, &movement.destination] { namespace::member_directory(&mut requests, endpoint.join(&entry.relative_path))?; }
            }
        }
    }
    let mut namespace = NamespaceGuard::acquire(&requests, true)?;
    if let Some(result) = original_result(&plan)? { return Ok(result); }
    for endpoint in &endpoints { exact_path(endpoint)?; }
    namespace.verify()?;
    let fenced = endpoints.iter().cloned().collect::<Vec<_>>();
    if let Err(error) = check_fences(&fenced, Some(operation_id), true) {
        return result(&plan, Some(error.to_string()));
    }
    if let Some(result) = original_result(&plan)? { return Ok(result); }
    for endpoint in &endpoints {
        if !std::fs::symlink_metadata(endpoint.parent().unwrap())?.is_dir() {
            return Err(invalid("directory publication requires existing real endpoint parents"));
        }
        if !absent(endpoint)? && !std::fs::symlink_metadata(endpoint)?.is_dir() {
            return Err(invalid("directory publication endpoint is neither a real directory nor absent"));
        }
        coordination_root(endpoint.parent().unwrap())?;
    }
    if !absent(&plan_path)? {
        let existing = load_plan(&plan_path)?;
        if existing.operation_id != plan.operation_id || existing.plan_hash != plan.plan_hash || existing.moves != plan.moves {
            return Err(invalid("directory operation identity was reused for a different plan"));
        }
        plan = existing;
        if let Err(error) = settle_path(&plan_path, Some(operation_id), true) {
            return result(&plan, Some(error.to_string()));
        }
        if let Some(result) = original_result(&plan)? { return Ok(result); }
    } else if !result(&plan, None)?.receipts.is_empty() {
        return Err(invalid("partial directory batch lost its original durable plan"));
    } else {
        for movement in &plan.moves {
            for parent in [movement.source.parent().unwrap(), movement.destination.parent().unwrap()] {
                plan.parents.insert(parent.to_path_buf(), FileIdentity::of_path(parent)?);
            }
        }
    }
    validate_plan(&plan, &plan_path)?;
    let members = plan.moves.iter().flat_map(|movement| movement.expected.entries.iter()
        .filter(|entry| entry.kind == DirectoryEntryKind::File)
        .flat_map(|entry| [&movement.source, &movement.destination].map(|endpoint| (endpoint.join(&entry.relative_path), entry.identity.clone())))).collect::<Vec<_>>();
    let mut files = namespace::TreeFiles::acquire(&members, true)?;
    let lock = HeldFile::acquire(plan_path.with_extension("lock"), true, true)?;
    write_record_once_bounded(&plan_path, &plan, history::DIRECTORY_LIMIT)?;
    let attempt = (|| -> io::Result<()> {
        let reference = reference(&plan, &plan_path);
        for endpoint in &endpoints { write_record_once(&marker(endpoint), &reference)?; }
        namespace.verify()?;
        for index in 0..plan.moves.len() {
            if let Some(receipt) = existing_receipt(&plan, index)? {
                if receipt.outcome != PublicationOutcome::Applied { return Err(invalid("directory batch already stopped at this step")); }
                continue;
            }
            execute(&plan, &plan_path, index, &mut namespace, &mut files)?;
        }
        retire(&plan, &plan_path)
    })();
    drop(lock);
    if let Err(error) = attempt {
        let recovery = settle_path(&plan_path, None, false);
        let detail = match recovery { Ok(_) => error.to_string(), Err(recovery) => format!("{error}; settlement: {recovery}") };
        return result(&plan, Some(detail));
    }
    result(&plan, None)
}
