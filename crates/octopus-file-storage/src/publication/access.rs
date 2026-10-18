use super::*;
use std::ffi::OsString;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PublicationFileSelector {
    Exact(PathBuf),
    Names { directory: PathBuf, prefix: OsString, suffix: OsString },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilePublicationAccess {
    pub directory_reads: Vec<PathBuf>,
    pub directory_creates: Vec<PathBuf>,
    pub file_reads: Vec<PublicationFileSelector>,
    pub file_writes: Vec<PublicationFileSelector>,
}

pub fn file_publication_access(path: &Path) -> io::Result<FilePublicationAccess> {
    namespace::validate_absolute(path)?;
    if path.file_name().is_none() || path.components().any(|part| part.as_os_str() == COORDINATION_DIRECTORY) {
        return Err(invalid("publication access requires an ordinary absolute file path"));
    }
    let history = history::file_access(path)?;
    let mut directories: BTreeSet<_> = history.ancestors.into_iter().collect();
    if history.writable_parent {
        directories.extend(path.parent().unwrap().ancestors().map(Path::to_path_buf));
    }
    let mut directory_reads = directories.clone();
    let mut file_reads: BTreeSet<_> = history.files.into_iter().map(PublicationFileSelector::Exact).collect();
    for endpoint in path.ancestors().filter(|endpoint| endpoint.file_name().is_some()) {
        if directories.contains(endpoint.parent().unwrap()) {
            file_reads.insert(PublicationFileSelector::Exact(directory::marker(endpoint)));
            file_reads.insert(PublicationFileSelector::Exact(reclaim::marker(endpoint)));
        }
    }
    for ancestor in &directories { directory_reads.insert(ancestor.join(COORDINATION_DIRECTORY)); }
    directory_reads.extend(history.directories);
    let mut access = FilePublicationAccess { directory_reads: directory_reads.into_iter().collect(),
        file_reads: Vec::new(), ..FilePublicationAccess::default() };
    if history.writable_parent {
        let target = Target::coordinates(path.to_path_buf());
        let names = |prefix: OsString, suffix: &str| PublicationFileSelector::Names {
            directory: target.root.clone(), prefix, suffix: suffix.into(),
        };
        let writes = [
            PublicationFileSelector::Exact(target.lock_path()),
            PublicationFileSelector::Exact(target.pending()),
            PublicationFileSelector::Exact(target.install()),
            names(target.stage_prefix().into(), ".bytes"),
            names(target.stage_prefix().into(), ".preimage"),
            names(target.receipt_prefix().into(), ".receipt"),
            names(record_temporary_prefix(&target.pending()), ".tmp"),
            names(target.receipt_prefix().into(), ".tmp"),
        ];
        file_reads.extend(writes.iter().cloned());
        file_reads.insert(names(target.stage_prefix().into(), ".receipt"));
        access.directory_creates.push(target.root);
        access.file_writes = BTreeSet::from(writes).into_iter().collect();
    }
    access.file_reads = file_reads.into_iter().collect();
    Ok(access)
}
