use super::*;
use std::path::Component;

pub(super) fn validate_absolute(path: &Path) -> io::Result<()> {
    if !path.is_absolute() || path.components().any(|part| matches!(part, Component::ParentDir | Component::CurDir)) {
        return Err(invalid("publication path must be absolute and normalized"));
    }
    Ok(())
}

pub(super) struct NamespaceGuard {
    held: Vec<HeldFile>,
    retired: Vec<HeldFile>,
    names: BTreeMap<PathBuf, FileIdentity>,
}

pub(super) struct TreeFiles {
    held: Vec<HeldFile>,
}

impl TreeFiles {
    pub(super) fn acquire(requests: &[(PathBuf, FileIdentity)], wait: bool) -> io::Result<Self> {
        let mut files = BTreeMap::new();
        for (path, expected) in requests {
            let metadata = match std::fs::symlink_metadata(path) {
                Ok(metadata) => metadata,
                Err(error) if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => continue,
                Err(error) => return Err(error),
            };
            if metadata.is_file() && FileIdentity::of_path(path)? == *expected {
                files.entry(expected.clone()).or_insert_with(|| path.clone());
            }
        }
        let mut held = Vec::new();
        for (identity, path) in files {
            let file = HeldFile::existing(path, true, false, wait)?;
            if FileIdentity::of_file(&file.file)? != identity { return Err(invalid("directory member changed while its inode lock was acquired")); }
            held.push(file);
        }
        Ok(Self { held })
    }

    pub(super) fn moved(&mut self, source: &Path, destination: &Path) {
        for held in &mut self.held {
            if let Ok(relative) = held.path.strip_prefix(source) { held.path = destination.join(relative); }
        }
    }

    pub(super) fn verify(&self) -> io::Result<()> {
        for held in &self.held { held.verify()?; }
        Ok(())
    }
}

/// `canonicalize`'s answer in the form callers use: on Windows without the
/// `\\?\` prefix wherever that names the same file (see
/// `octopus_private_fs::ordinary_path`); unchanged on Unix.
pub(super) fn ordinary(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    return octopus_private_fs::ordinary_path(path);
    #[cfg(not(windows))]
    path
}

/// Whether two absolute paths spell the same location; on Windows also
/// across the `\\?\` prefix, separators and ASCII letter case.
pub(super) fn same_path(first: &Path, second: &Path) -> bool {
    #[cfg(windows)]
    return octopus_private_fs::same_path(first, second);
    #[cfg(not(windows))]
    { first == second }
}

pub(super) fn normalize(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() || path.components().any(|part| matches!(part, Component::ParentDir | Component::CurDir)) {
        return Err(invalid("publication path must be absolute and normalized"));
    }
    let mut cursor = path;
    let mut missing = Vec::new();
    loop {
        match std::fs::symlink_metadata(cursor) {
            Ok(metadata) => {
                if !missing.is_empty() && !metadata.is_dir() && !metadata.file_type().is_symlink() {
                    return Err(invalid("publication path has a non-directory ancestor"));
                }
                // Windows: `canonicalize` answers in `\\?\` form; keep paths in
                // the ordinary form callers use wherever that is the same file.
                let mut resolved = ordinary(std::fs::canonicalize(cursor)?);
                for name in missing.iter().rev() { resolved.push(name); }
                return Ok(resolved);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(cursor.file_name().ok_or_else(|| invalid("publication path has no existing ancestor"))?.to_os_string());
                cursor = cursor.parent().ok_or_else(|| invalid("publication path has no parent"))?;
            }
            Err(error) => return Err(error),
        }
    }
}

pub(super) fn normalize_entry(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() || path.components().any(|part| matches!(part, Component::ParentDir | Component::CurDir)) {
        return Err(invalid("publication entry must be an absolute normalized path"));
    }
    let name = path.file_name().ok_or_else(|| invalid("publication entry cannot be the filesystem root"))?;
    Ok(normalize(path.parent().unwrap())?.join(name))
}

impl NamespaceGuard {
    pub(super) fn identity(&self, path: &Path) -> io::Result<&FileIdentity> {
        self.names.get(path).ok_or_else(|| invalid("directory is outside the held publication namespace"))
    }

    pub(super) fn acquire(requests: &[(PathBuf, bool)], wait: bool) -> io::Result<Self> {
        let mut names = BTreeMap::new();
        let mut identities: BTreeMap<FileIdentity, (PathBuf, bool)> = BTreeMap::new();
        let mut desired = BTreeMap::new();
        for (requested, exclusive) in requests {
            validate_absolute(requested)?;
            *desired.entry(requested.clone()).or_insert(false) |= *exclusive;
            for ancestor in requested.ancestors().skip(1) { desired.entry(ancestor.to_path_buf()).or_insert(false); }
        }
        for (requested, exclusive) in desired {
            let requested = normalize(&requested)?;
            let existing = requested.ancestors().find(|path| path.is_dir())
                .ok_or_else(|| invalid("publication namespace has no existing directory"))?;
            for path in existing.ancestors() {
                let metadata = std::fs::symlink_metadata(path)?;
                if !metadata.is_dir() { return Err(invalid("publication ancestor is not a real directory")); }
                let identity = FileIdentity::of_path(path)?;
                names.insert(path.to_path_buf(), identity.clone());
                let mode = exclusive && path == existing;
                let entry = identities.entry(identity).or_insert_with(|| (path.to_path_buf(), false));
                if path < entry.0.as_path() { entry.0 = path.to_path_buf(); }
                entry.1 |= mode;
            }
        }
        let mut held = Vec::new();
        for (path, exclusive) in identities.into_values() { held.push(HeldFile::existing(path, exclusive, true, wait)?); }
        let guard = Self { held, retired: Vec::new(), names };
        guard.verify()?;
        Ok(guard)
    }

    pub(super) fn verify(&self) -> io::Result<()> {
        for held in &self.held { held.verify()?; }
        for (path, identity) in &self.names {
            let metadata = std::fs::symlink_metadata(path)?;
            if !metadata.is_dir() || FileIdentity::of_path(path)? != *identity {
                return Err(invalid("publication ancestry changed while held"));
            }
        }
        Ok(())
    }

    pub(super) fn moved(&mut self, source: &Path, destination: &Path) {
        let translate = |path: &Path| match path.strip_prefix(source) {
            Ok(relative) => destination.join(relative), Err(_) => path.to_path_buf(),
        };
        for held in &mut self.held { held.path = translate(&held.path); }
        self.names = std::mem::take(&mut self.names).into_iter().map(|(path, id)| (translate(&path), id)).collect();
    }

    pub(super) fn removed(&mut self, root: &Path) {
        for held in std::mem::take(&mut self.held) {
            if held.path.starts_with(root) { self.retired.push(held); } else { self.held.push(held); }
        }
        self.names.retain(|path, _| !path.starts_with(root));
    }
}

pub(super) fn member_directory(requests: &mut Vec<(PathBuf, bool)>, path: PathBuf) -> io::Result<()> {
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() => requests.push((path, true)),
        Ok(_) => (),
        Err(error) if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => (),
        Err(error) => return Err(error),
    }
    Ok(())
}

pub(super) fn create_missing(path: &Path) -> io::Result<()> {
    let mut absent = Vec::new();
    let mut cursor = path;
    while !cursor.try_exists()? {
        absent.push(cursor.to_path_buf());
        cursor = cursor.parent().ok_or_else(|| invalid("directory creation has no existing ancestor"))?;
    }
    if !std::fs::symlink_metadata(cursor)?.is_dir() { return Err(invalid("directory creation ancestor is not a directory")); }
    for directory in absent.into_iter().rev() {
        std::fs::create_dir(&directory)?;
        sync_directory(directory.parent().unwrap())?;
    }
    Ok(())
}
