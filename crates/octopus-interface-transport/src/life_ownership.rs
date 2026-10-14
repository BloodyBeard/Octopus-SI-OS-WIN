use octopus_descriptors::InterfaceId;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Component, Path};

pub struct InterfaceLifeOwnership {
    _file: File,
}

impl InterfaceLifeOwnership {
    pub fn try_acquire(
        private_root: &Path,
        interface: &InterfaceId,
    ) -> Result<Option<Self>, String> {
        Self::acquire(private_root, interface, false)
    }

    pub(crate) fn try_acquire_life(
        private_root: &Path,
        interface: &InterfaceId,
    ) -> Result<Option<Self>, String> {
        Self::acquire(private_root, interface, true)
    }

    #[cfg(unix)]
    pub(crate) fn inherit_into(&self, command: &mut tokio::process::Command) {
        use std::os::fd::AsRawFd;
        let fd = self._file.as_raw_fd();
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags == -1 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    fn acquire(
        private_root: &Path,
        interface: &InterfaceId,
        shared: bool,
    ) -> Result<Option<Self>, String> {
        let mut parts = Path::new(interface.as_str()).components();
        if !private_root.is_absolute()
            || !matches!(parts.next(), Some(Component::Normal(_)))
            || parts.next().is_some()
        {
            return Err(
                "interface life ownership requires an absolute private root and exact interface id"
                    .into(),
            );
        }
        std::fs::create_dir_all(private_root).map_err(|e| e.to_string())?;
        let root_meta = std::fs::symlink_metadata(private_root).map_err(|e| e.to_string())?;
        if !root_meta.is_dir() || root_meta.file_type().is_symlink() {
            return Err("interface life ownership root must be a regular directory".into());
        }
        let root = std::fs::canonicalize(private_root).map_err(|e| e.to_string())?;
        let locks = root.join(".life-ownership");
        match std::fs::create_dir(&locks) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.to_string()),
        }
        let metadata = std::fs::symlink_metadata(&locks).map_err(|e| e.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("interface life ownership lock root must be a regular directory".into());
        }
        let path = locks.join(format!("{}.lock", interface.as_str()));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        let file = options.open(&path).map_err(|e| e.to_string())?;
        if !file.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err("interface life ownership lock must be a regular file".into());
        }
        match if shared {
            file.try_lock_shared()
        } else {
            file.try_lock()
        } {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(error) => return Err(error.to_string()),
        }
        let named = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if !named.is_file() || named.file_type().is_symlink() {
            return Err("interface life ownership lock was replaced".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let opened = file.metadata().map_err(|e| e.to_string())?;
            if opened.dev() != named.dev() || opened.ino() != named.ino() {
                return Err("interface life ownership lock identity changed".into());
            }
        }
        Ok(Some(Self { _file: file }))
    }
}
