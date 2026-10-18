use std::io;
use std::path::Path;

#[cfg(unix)]
use std::fs::{File, Metadata};
#[cfg(unix)]
use std::path::{Component, PathBuf};

pub struct RuntimeInstanceLock {
    #[cfg(unix)]
    directories: Vec<(PathBuf, File)>,
    #[cfg(unix)]
    file: File,
}

impl RuntimeInstanceLock {
    pub fn try_acquire(home: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use fs2::FileExt;
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
            FileExt::try_lock_exclusive(&file)?;
            let guard = Self { directories, file };
            guard.verify()?;
            guard.directories.last().unwrap().1.sync_all()?;
            Ok(guard)
        }
        #[cfg(not(unix))]
        {
            let _ = home;
            Err(io::Error::new(io::ErrorKind::Unsupported, "exact runtime ownership requires native file identity"))
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
        #[cfg(not(unix))]
        {
            Err(io::Error::new(io::ErrorKind::Unsupported, "exact runtime ownership requires native file identity"))
        }
    }
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

#[cfg(unix)]
fn invalid(message: &str) -> io::Error { io::Error::new(io::ErrorKind::InvalidData, message) }
