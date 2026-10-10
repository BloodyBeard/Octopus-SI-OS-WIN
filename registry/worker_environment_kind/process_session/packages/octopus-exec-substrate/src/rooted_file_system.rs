use crate::jsonrpc_lite::JSONRPCErrorError;
use crate::protocol::{FsRootedParams, FsRootedResponse};
use crate::rpc::{internal_error, invalid_request, not_found};

pub(crate) fn execute(params: FsRootedParams) -> Result<FsRootedResponse, JSONRPCErrorError> {
    platform::execute(params).map_err(map_error)
}

fn map_error(error: std::io::Error) -> JSONRPCErrorError {
    match error.kind() {
        std::io::ErrorKind::NotFound => not_found(error.to_string()),
        std::io::ErrorKind::InvalidInput
        | std::io::ErrorKind::PermissionDenied
        | std::io::ErrorKind::AlreadyExists
        | std::io::ErrorKind::Unsupported => invalid_request(error.to_string()),
        _ => internal_error(error.to_string()),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod platform {
    use std::ffi::{CStr, CString, OsStr, OsString};
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::path::{Component, Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::protocol::{
        FsRootedDirectoryEntry, FsRootedMetadata, FsRootedOperation, FsRootedParams,
        FsRootedResponse,
    };

    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

    pub(super) fn execute(params: FsRootedParams) -> std::io::Result<FsRootedResponse> {
        let rooted = RootedFileSystem::open(&params.root)?;
        rooted.execute(params.operation)
    }

    struct RootedFileSystem {
        root: OwnedFd,
        root_path: PathBuf,
    }

    struct TraversalBudget {
        remaining: usize,
    }

    impl TraversalBudget {
        fn new(limit: usize) -> std::io::Result<Self> {
            if limit == 0 {
                return Err(invalid("filesystem traversal limit must be positive"));
            }
            Ok(Self { remaining: limit })
        }

        fn consume(&mut self) -> std::io::Result<()> {
            self.remaining = self
                .remaining
                .checked_sub(1)
                .ok_or_else(|| invalid("filesystem traversal exceeded its declared limit"))?;
            Ok(())
        }
    }

    impl RootedFileSystem {
        fn open(root: &octopus_util_pathuri::PathUri) -> std::io::Result<Self> {
            let root_path = root
                .to_abs_path()
                .map_err(|error| invalid(format!("root path is not native to this host: {error}")))?
                .into_path_buf();
            let root_name = c_path(&root_path)?;
            let raw = unsafe {
                libc::open(
                    root_name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
            if raw < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(Self {
                root: unsafe { OwnedFd::from_raw_fd(raw) },
                root_path,
            })
        }

        fn execute(&self, operation: FsRootedOperation) -> std::io::Result<FsRootedResponse> {
            match operation {
                FsRootedOperation::ReadFile { path, max_bytes } => {
                    let components = self.components(&path)?;
                    let mut file = std::fs::File::from(
                        self.open_target(&components, libc::O_RDONLY | libc::O_NONBLOCK)?,
                    );
                    let metadata = file.metadata()?;
                    if !metadata.is_file() {
                        return Err(invalid("rooted read target is not a regular file"));
                    }
                    if max_bytes == 0 {
                        return Err(invalid("rooted read max_bytes must be positive"));
                    }
                    if metadata.len() > max_bytes as u64 {
                        return Err(invalid(format!(
                            "rooted read target is {} bytes, above declared limit {max_bytes}",
                            metadata.len()
                        )));
                    }
                    let mut data = Vec::new();
                    std::io::Read::by_ref(&mut file)
                        .take(max_bytes.saturating_add(1) as u64)
                        .read_to_end(&mut data)?;
                    if data.len() > max_bytes {
                        return Err(invalid(format!(
                            "rooted read target grew above declared limit {max_bytes}"
                        )));
                    }
                    Ok(FsRootedResponse::ReadFile { data: data.into() })
                }
                FsRootedOperation::WriteFile {
                    path,
                    data,
                    create_parents,
                } => {
                    let components = self.components(&path)?;
                    let (parent, leaf) = self.open_parent(&components, create_parents)?;
                    let mut bytes = data.0.as_slice();
                    self.atomic_write(&parent, &leaf, &mut bytes)?;
                    Ok(FsRootedResponse::WriteFile {
                        bytes_written: data.0.len(),
                    })
                }
                FsRootedOperation::ReadDirectory { path, max_entries } => {
                    let components = self.components(&path)?;
                    let directory = self.open_directory(&components, false)?;
                    let mut budget = TraversalBudget::new(max_entries)?;
                    let mut entries = Vec::new();
                    for name in directory_names(&directory, max_entries)? {
                        budget.consume()?;
                        let component = c_component(&name)?;
                        let stat = stat_at(&directory, &component)?;
                        entries.push(FsRootedDirectoryEntry {
                            file_name: name.to_string_lossy().into_owned(),
                            is_directory: file_type(stat.st_mode) == libc::S_IFDIR,
                            is_file: file_type(stat.st_mode) == libc::S_IFREG,
                            is_symlink: file_type(stat.st_mode) == libc::S_IFLNK,
                        });
                    }
                    Ok(FsRootedResponse::ReadDirectory { entries })
                }
                FsRootedOperation::GetMetadata { path } => {
                    let components = self.components(&path)?;
                    let stat = self.stat_target(&components)?;
                    Ok(FsRootedResponse::GetMetadata {
                        metadata: metadata_from_stat(&stat),
                    })
                }
                FsRootedOperation::Remove {
                    path,
                    recursive,
                    force,
                    max_entries,
                } => {
                    let components = self.components(&path)?;
                    if components.is_empty() {
                        return Err(invalid("rooted remove cannot remove its root"));
                    }
                    let (parent, leaf) = self.open_parent(&components, false)?;
                    let mut budget = TraversalBudget::new(max_entries)?;
                    match self.remove_at(&parent, &leaf, recursive, &mut budget) {
                        Ok(()) => Ok(FsRootedResponse::Remove),
                        Err(error) if force && error.kind() == std::io::ErrorKind::NotFound => {
                            Ok(FsRootedResponse::Remove)
                        }
                        Err(error) => Err(error),
                    }
                }
                FsRootedOperation::Copy {
                    source_path,
                    destination_path,
                    recursive,
                    create_parents,
                    max_entries,
                } => {
                    let source = self.components(&source_path)?;
                    let destination = self.components(&destination_path)?;
                    if source.is_empty() || destination.is_empty() {
                        return Err(invalid(
                            "rooted copy cannot use its root as source or target",
                        ));
                    }
                    if source == destination {
                        return Err(invalid("rooted copy source and destination are identical"));
                    }
                    let source_stat = self.stat_target(&source)?;
                    if file_type(source_stat.st_mode) == libc::S_IFDIR
                        && destination.starts_with(&source)
                    {
                        return Err(invalid(
                            "rooted copy cannot copy a directory into its own descendant",
                        ));
                    }
                    let (source_parent, source_leaf) = self.open_parent(&source, false)?;
                    let (destination_parent, destination_leaf) =
                        self.open_parent(&destination, create_parents)?;
                    let mut budget = TraversalBudget::new(max_entries)?;
                    self.copy_at(
                        &source_parent,
                        &source_leaf,
                        &destination_parent,
                        &destination_leaf,
                        recursive,
                        &mut budget,
                    )?;
                    Ok(FsRootedResponse::Copy)
                }
                FsRootedOperation::CreateDirectory { path, recursive } => {
                    let components = self.components(&path)?;
                    if components.is_empty() {
                        return Ok(FsRootedResponse::CreateDirectory);
                    }
                    if recursive {
                        self.open_directory(&components, true)?;
                    } else {
                        let (parent, leaf) = self.open_parent(&components, false)?;
                        mkdir_at(&parent, &leaf)?;
                    }
                    Ok(FsRootedResponse::CreateDirectory)
                }
            }
        }

        fn components(
            &self,
            target: &octopus_util_pathuri::PathUri,
        ) -> std::io::Result<Vec<OsString>> {
            let target = target
                .to_abs_path()
                .map_err(|error| {
                    invalid(format!("target path is not native to this host: {error}"))
                })?
                .into_path_buf();
            let relative = target.strip_prefix(&self.root_path).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "target '{}' is outside rooted workspace '{}'",
                        target.display(),
                        self.root_path.display()
                    ),
                )
            })?;
            relative
                .components()
                .map(|component| match component {
                    Component::Normal(name) if !name.as_bytes().is_empty() => {
                        Ok(name.to_os_string())
                    }
                    _ => Err(invalid(format!(
                        "target '{}' has a non-normal rooted component",
                        target.display()
                    ))),
                })
                .collect()
        }

        fn open_parent(
            &self,
            components: &[OsString],
            create: bool,
        ) -> std::io::Result<(OwnedFd, CString)> {
            let (leaf, parents) = components
                .split_last()
                .ok_or_else(|| invalid("rooted operation requires a path below its root"))?;
            let parent = self.open_directory(parents, create)?;
            Ok((parent, c_component(leaf)?))
        }

        fn open_directory(
            &self,
            components: &[OsString],
            create: bool,
        ) -> std::io::Result<OwnedFd> {
            let mut directory = duplicate_fd(&self.root)?;
            for component in components {
                let name = c_component(component)?;
                match open_at(&directory, &name, libc::O_RDONLY | libc::O_DIRECTORY, 0) {
                    Ok(opened) => directory = opened,
                    Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => {
                        match mkdir_at(&directory, &name) {
                            Ok(()) => {}
                            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                            Err(error) => return Err(error),
                        }
                        directory =
                            open_at(&directory, &name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
                    }
                    Err(error) => return Err(error),
                }
            }
            Ok(directory)
        }

        fn open_target(&self, components: &[OsString], flags: i32) -> std::io::Result<OwnedFd> {
            if components.is_empty() {
                return duplicate_fd(&self.root);
            }
            let (parent, leaf) = self.open_parent(components, false)?;
            open_at(&parent, &leaf, flags, 0)
        }

        fn stat_target(&self, components: &[OsString]) -> std::io::Result<libc::stat> {
            if components.is_empty() {
                return stat_fd(&self.root);
            }
            let (parent, leaf) = self.open_parent(components, false)?;
            stat_at(&parent, &leaf)
        }

        fn atomic_write<R: Read>(
            &self,
            parent: &OwnedFd,
            leaf: &CStr,
            reader: &mut R,
        ) -> std::io::Result<()> {
            match stat_at(parent, leaf) {
                Ok(stat) => {
                    let kind = file_type(stat.st_mode);
                    if kind == libc::S_IFLNK {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            "rooted write refuses a symbolic-link target",
                        ));
                    }
                    if kind != libc::S_IFREG {
                        return Err(invalid("rooted write target is not a regular file"));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            let (temporary, temporary_name) = create_temporary(parent)?;
            let result = (|| {
                let mut file = std::fs::File::from(temporary);
                std::io::copy(reader, &mut file)?;
                file.flush()?;
                file.sync_all()?;
                let renamed = unsafe {
                    libc::renameat(
                        parent.as_raw_fd(),
                        temporary_name.as_ptr(),
                        parent.as_raw_fd(),
                        leaf.as_ptr(),
                    )
                };
                if renamed != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            })();
            if result.is_err() {
                unsafe {
                    libc::unlinkat(parent.as_raw_fd(), temporary_name.as_ptr(), 0);
                }
            }
            result
        }

        fn remove_at(
            &self,
            parent: &OwnedFd,
            leaf: &CStr,
            recursive: bool,
            budget: &mut TraversalBudget,
        ) -> std::io::Result<()> {
            let stat = stat_at(parent, leaf)?;
            if file_type(stat.st_mode) == libc::S_IFDIR {
                if recursive {
                    let directory = open_at(parent, leaf, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
                    for name in directory_names(&directory, budget.remaining)? {
                        budget.consume()?;
                        let child = c_component(&name)?;
                        self.remove_at(&directory, &child, true, budget)?;
                    }
                }
                let removed = unsafe {
                    libc::unlinkat(parent.as_raw_fd(), leaf.as_ptr(), libc::AT_REMOVEDIR)
                };
                if removed != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                return Ok(());
            }
            let removed = unsafe { libc::unlinkat(parent.as_raw_fd(), leaf.as_ptr(), 0) };
            if removed != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }

        fn copy_at(
            &self,
            source_parent: &OwnedFd,
            source_leaf: &CStr,
            destination_parent: &OwnedFd,
            destination_leaf: &CStr,
            recursive: bool,
            budget: &mut TraversalBudget,
        ) -> std::io::Result<()> {
            budget.consume()?;
            let stat = stat_at(source_parent, source_leaf)?;
            match file_type(stat.st_mode) {
                kind if kind == libc::S_IFREG => {
                    let source = open_at(
                        source_parent,
                        source_leaf,
                        libc::O_RDONLY | libc::O_NONBLOCK,
                        0,
                    )?;
                    let mut source = std::fs::File::from(source);
                    self.atomic_write(destination_parent, destination_leaf, &mut source)
                }
                kind if kind == libc::S_IFDIR => {
                    if !recursive {
                        return Err(invalid(
                            "rooted copy requires recursive=true for a directory",
                        ));
                    }
                    match mkdir_at(destination_parent, destination_leaf) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(error),
                    }
                    let source = open_at(
                        source_parent,
                        source_leaf,
                        libc::O_RDONLY | libc::O_DIRECTORY,
                        0,
                    )?;
                    let destination = open_at(
                        destination_parent,
                        destination_leaf,
                        libc::O_RDONLY | libc::O_DIRECTORY,
                        0,
                    )?;
                    for name in directory_names(&source, budget.remaining)? {
                        let child = c_component(&name)?;
                        self.copy_at(&source, &child, &destination, &child, true, budget)?;
                    }
                    Ok(())
                }
                kind if kind == libc::S_IFLNK => Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "rooted copy refuses symbolic links",
                )),
                _ => Err(invalid("rooted copy source has an unsupported file type")),
            }
        }
    }

    fn open_at(
        parent: &OwnedFd,
        name: &CStr,
        flags: i32,
        mode: libc::mode_t,
    ) -> std::io::Result<OwnedFd> {
        let raw = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                mode as libc::c_uint,
            )
        };
        if raw < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(unsafe { OwnedFd::from_raw_fd(raw) })
        }
    }

    fn mkdir_at(parent: &OwnedFd, name: &CStr) -> std::io::Result<()> {
        let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o777) };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    fn create_temporary(parent: &OwnedFd) -> std::io::Result<(OwnedFd, CString)> {
        for _ in 0..128 {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let name = CString::new(format!(".octopus-write-{}-{sequence}", std::process::id()))
                .map_err(|_| invalid("temporary file name contains NUL"))?;
            match open_at(
                parent,
                &name,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                0o666,
            ) {
                Ok(file) => return Ok((file, name)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate a unique rooted write temporary",
        ))
    }

    fn stat_at(parent: &OwnedFd, name: &CStr) -> std::io::Result<libc::stat> {
        let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
        let result = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result == 0 {
            Ok(stat)
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    fn stat_fd(fd: &OwnedFd) -> std::io::Result<libc::stat> {
        let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
        let result = unsafe { libc::fstat(fd.as_raw_fd(), &mut stat) };
        if result == 0 {
            Ok(stat)
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    fn file_type(mode: libc::mode_t) -> libc::mode_t {
        mode & libc::S_IFMT
    }

    fn metadata_from_stat(stat: &libc::stat) -> FsRootedMetadata {
        FsRootedMetadata {
            is_directory: file_type(stat.st_mode) == libc::S_IFDIR,
            is_file: file_type(stat.st_mode) == libc::S_IFREG,
            is_symlink: file_type(stat.st_mode) == libc::S_IFLNK,
            size: u64::try_from(stat.st_size).unwrap_or(0),
            created_at_ms: created_at_ms(stat),
            modified_at_ms: modified_at_ms(stat),
        }
    }

    #[cfg(target_os = "macos")]
    fn created_at_ms(stat: &libc::stat) -> i64 {
        timestamp_ms(stat.st_birthtime, stat.st_birthtime_nsec)
    }

    #[cfg(target_os = "linux")]
    fn created_at_ms(_stat: &libc::stat) -> i64 {
        0
    }

    #[cfg(target_os = "macos")]
    fn modified_at_ms(stat: &libc::stat) -> i64 {
        timestamp_ms(stat.st_mtime, stat.st_mtime_nsec)
    }

    #[cfg(target_os = "linux")]
    fn modified_at_ms(stat: &libc::stat) -> i64 {
        timestamp_ms(stat.st_mtime, stat.st_mtime_nsec)
    }

    fn timestamp_ms(seconds: i64, nanoseconds: i64) -> i64 {
        seconds
            .saturating_mul(1000)
            .saturating_add(nanoseconds.saturating_div(1_000_000))
    }

    fn duplicate_fd(fd: &OwnedFd) -> std::io::Result<OwnedFd> {
        let raw = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if raw < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(unsafe { OwnedFd::from_raw_fd(raw) })
        }
    }

    fn directory_names(directory: &OwnedFd, maximum: usize) -> std::io::Result<Vec<OsString>> {
        let duplicated = duplicate_fd(directory)?;
        let raw = duplicated.into_raw_fd();
        let pointer = unsafe { libc::fdopendir(raw) };
        if pointer.is_null() {
            unsafe {
                libc::close(raw);
            }
            return Err(std::io::Error::last_os_error());
        }
        let guard = DirectoryPointer(pointer);
        let mut names = Vec::new();
        loop {
            set_errno(0);
            let entry = unsafe { libc::readdir(guard.0) };
            if entry.is_null() {
                let error = current_errno();
                if error == 0 {
                    break;
                }
                return Err(std::io::Error::from_raw_os_error(error));
            }
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            if names.len() == maximum {
                return Err(invalid("filesystem traversal exceeded its declared limit"));
            }
            names.push(OsString::from_vec(name.to_bytes().to_vec()));
        }
        names.sort();
        Ok(names)
    }

    struct DirectoryPointer(*mut libc::DIR);

    impl Drop for DirectoryPointer {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn set_errno(value: i32) {
        unsafe {
            *libc::__errno_location() = value;
        }
    }

    #[cfg(target_os = "macos")]
    fn set_errno(value: i32) {
        unsafe {
            *libc::__error() = value;
        }
    }

    #[cfg(target_os = "linux")]
    fn current_errno() -> i32 {
        unsafe { *libc::__errno_location() }
    }

    #[cfg(target_os = "macos")]
    fn current_errno() -> i32 {
        unsafe { *libc::__error() }
    }

    fn c_component(name: &OsStr) -> std::io::Result<CString> {
        CString::new(name.as_bytes()).map_err(|_| invalid("path component contains NUL"))
    }

    fn c_path(path: &Path) -> std::io::Result<CString> {
        CString::new(path.as_os_str().as_bytes()).map_err(|_| invalid("path contains NUL"))
    }

    fn invalid(message: impl Into<String>) -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into())
    }

    #[cfg(test)]
    mod tests {
        use std::os::unix::fs::symlink;

        use octopus_util_pathuri::PathUri;

        use super::*;

        #[test]
        fn parent_symlink_swap_cannot_redirect_rooted_write() {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("root");
            let outside = temp.path().join("outside");
            std::fs::create_dir_all(root.join("slot")).unwrap();
            std::fs::create_dir_all(&outside).unwrap();
            let rooted = RootedFileSystem::open(&PathUri::from_path(&root).unwrap()).unwrap();
            let target = PathUri::from_path(root.join("slot/result.txt")).unwrap();
            let components = rooted.components(&target).unwrap();
            let (parent, leaf) = rooted.open_parent(&components, false).unwrap();

            std::fs::rename(root.join("slot"), root.join("retained")).unwrap();
            symlink(&outside, root.join("slot")).unwrap();
            let mut payload = b"bounded".as_slice();
            rooted.atomic_write(&parent, &leaf, &mut payload).unwrap();

            assert_eq!(
                std::fs::read(root.join("retained/result.txt")).unwrap(),
                b"bounded"
            );
            assert!(!outside.join("result.txt").exists());
        }

        #[test]
        fn symbolic_link_parent_is_refused_without_following() {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("root");
            let outside = temp.path().join("outside");
            std::fs::create_dir_all(&root).unwrap();
            std::fs::create_dir_all(&outside).unwrap();
            std::fs::write(outside.join("secret.txt"), b"secret").unwrap();
            symlink(&outside, root.join("link")).unwrap();
            let result = execute(FsRootedParams {
                root: PathUri::from_path(&root).unwrap(),
                operation: FsRootedOperation::ReadFile {
                    path: PathUri::from_path(root.join("link/secret.txt")).unwrap(),
                    max_bytes: 1024,
                },
            });
            assert!(result.is_err());
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use crate::protocol::{FsRootedParams, FsRootedResponse};

    pub(super) fn execute(_params: FsRootedParams) -> std::io::Result<FsRootedResponse> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "descriptor-relative rooted filesystem operations are unsupported on this host",
        ))
    }
}
