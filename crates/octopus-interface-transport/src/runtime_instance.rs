use std::io;
use std::path::Path;

use std::fs::File;
#[cfg(unix)]
use std::fs::Metadata;
use std::path::{Component, PathBuf};

pub struct RuntimeInstanceLock {
    directories: Vec<(PathBuf, File)>,
    file: File,
}

impl RuntimeInstanceLock {
    pub fn try_acquire(home: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            if !home.is_absolute() || home.components().any(|part| !matches!(part, Component::RootDir | Component::Normal(_))) {
                return Err(invalid("runtime ownership requires an absolute normalized Home"));
            }
            let root = std::fs::OpenOptions::new().read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open("/")?;
            let mut directories = vec![(PathBuf::from("/"), root)];
            let parent = home.join("state");
            for part in parent.components() {
                let Component::Normal(name) = part else { continue; };
                let (path, held) = directories.last().unwrap();
                let directory = match open_child(held, name, libc::O_RDONLY | libc::O_DIRECTORY) {
                    Ok(directory) => directory,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        create_directory(held, name)?;
                        open_child(held, name, libc::O_RDONLY | libc::O_DIRECTORY)?
                    },
                    Err(error) => return Err(error),
                };
                directories.push((path.join(name), directory));
            }
            let held_parent = &directories.last().unwrap().1;
            let file = open_child(held_parent, std::ffi::OsStr::new("runtime-instance.lock"), libc::O_RDWR | libc::O_CREAT)?;
            if !file.metadata()?.is_file() { return Err(invalid("runtime ownership lock is not a regular file")); }
            file.try_lock().map_err(io::Error::from)?;
            let guard = Self { directories, file };
            guard.verify()?;
            guard.directories.last().unwrap().1.sync_all()?;
            Ok(guard)
        }
        // Windows: the same walk from the drive root. Each folder is held
        // by a handle to the folder itself (never through a symlink or
        // junction) and checked by volume serial number and file ID.
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            if !home.is_absolute()
                || home.components().any(|part| !matches!(part, Component::Prefix(_) | Component::RootDir | Component::Normal(_)))
            {
                return Err(invalid("runtime ownership requires an absolute normalized Home"));
            }
            let parent = home.join("state");
            let mut path = PathBuf::new();
            let mut directories = Vec::new();
            for part in parent.components() {
                path.push(part);
                if matches!(part, Component::Prefix(_)) { continue; }
                if let Component::Normal(_) = part {
                    match std::fs::create_dir(&path) {
                        Ok(()) => sync_parent(&path)?,
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(error),
                    }
                }
                directories.push((path.clone(), hold_directory(&path)?));
            }
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT).open(path.join("runtime-instance.lock"))?;
            if !file.metadata()?.is_file() { return Err(invalid("runtime ownership lock is not a regular file")); }
            file.try_lock().map_err(io::Error::from)?;
            let guard = Self { directories, file };
            guard.verify()?;
            octopus_private_fs::sync_directory(&path)?;
            Ok(guard)
        }
    }

    pub fn verify(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            for (path, file) in &self.directories {
                let named = std::fs::symlink_metadata(path)?;
                if !named.is_dir() || !same_identity(&named, &file.metadata()?) {
                    return Err(invalid("runtime ownership parent was replaced while held"));
                }
            }
            let path = self.directories.last().unwrap().0.join("runtime-instance.lock");
            let named = std::fs::symlink_metadata(path)?;
            if !named.is_file() || !same_identity(&named, &self.file.metadata()?) {
                return Err(invalid("runtime ownership lock was replaced while held"));
            }
            Ok(())
        }
        #[cfg(windows)]
        {
            for (path, held) in &self.directories {
                if !std::fs::symlink_metadata(path)?.is_dir()
                    || octopus_private_fs::identity_of_path_no_follow(path)?.device_and_file()
                        != octopus_private_fs::identity_of_file(held)?.device_and_file()
                {
                    return Err(invalid("runtime ownership parent was replaced while held"));
                }
            }
            let path = self.directories.last().unwrap().0.join("runtime-instance.lock");
            if !std::fs::symlink_metadata(&path)?.is_file()
                || octopus_private_fs::identity_of_path_no_follow(&path)?.device_and_file()
                    != octopus_private_fs::identity_of_file(&self.file)?.device_and_file()
            {
                return Err(invalid("runtime ownership lock was replaced while held"));
            }
            Ok(())
        }
    }
}

/// A handle to the directory at `path` itself, refusing symlinks and junctions.
#[cfg(windows)]
fn hold_directory(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    let held = octopus_private_fs::windows::open_entry_handle(path)?;
    let metadata = held.metadata()?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(invalid("runtime ownership path crosses a symlink, junction or non-directory"));
    }
    Ok(held)
}

#[cfg(windows)]
fn sync_parent(path: &Path) -> io::Result<()> {
    octopus_private_fs::sync_directory(path.parent().ok_or_else(|| invalid("runtime ownership folder has no parent"))?)
}

#[cfg(unix)]
fn same_identity(left: &Metadata, right: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(unix)]
fn entry_name(name: &std::ffi::OsStr) -> io::Result<std::ffi::CString> {
    std::ffi::CString::new(name.as_encoded_bytes()).map_err(|_| invalid("runtime ownership entry contains NUL"))
}

#[cfg(unix)]
fn open_child(parent: &File, name: &std::ffi::OsStr, flags: libc::c_int) -> io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = entry_name(name)?;
    let descriptor = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(),
        flags | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC, 0o600 as libc::c_uint) };
    if descriptor < 0 { return Err(io::Error::last_os_error()); }
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

#[cfg(unix)]
fn create_directory(parent: &File, name: &std::ffi::OsStr) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let name = entry_name(name)?;
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::AlreadyExists { return Err(error); }
    }
    parent.sync_all()
}

fn invalid(message: &str) -> io::Error { io::Error::new(io::ErrorKind::InvalidData, message) }

#[cfg(test)]
mod tests {
    use super::RuntimeInstanceLock;

    #[test]
    fn one_runtime_owns_a_home_at_a_time() {
        let home = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(home.path()).unwrap().join("home");
        let first = RuntimeInstanceLock::try_acquire(&home).unwrap();
        first.verify().unwrap();
        assert!(home.join("state").join("runtime-instance.lock").is_file());
        assert!(RuntimeInstanceLock::try_acquire(&home).is_err());
        drop(first);
        RuntimeInstanceLock::try_acquire(&home).unwrap().verify().unwrap();
    }
}
