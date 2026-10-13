use crate::{CollectionError, Result};
use std::path::{Path, PathBuf};

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod platform {
    use super::*;
    use std::ffi::{CStr, CString};
    use std::fs::{File, Metadata};
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
    use std::os::unix::fs::MetadataExt;
    use std::path::Component;

    #[cfg(target_os = "macos")]
    const DIRECTORY_ACCESS: libc::c_int = libc::O_SEARCH;
    #[cfg(target_os = "linux")]
    const DIRECTORY_ACCESS: libc::c_int = libc::O_PATH | libc::O_DIRECTORY;

    fn open_at(parent: RawFd, name: &CStr, access: libc::c_int) -> std::io::Result<File> {
        let fd = unsafe {
            libc::openat(
                parent,
                name.as_ptr(),
                access | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) struct Fingerprint {
        dev: u64,
        ino: u64,
        len: u64,
        mtime: i64,
        mtime_ns: i64,
        ctime: i64,
        ctime_ns: i64,
    }
    impl From<Metadata> for Fingerprint {
        fn from(m: Metadata) -> Self {
            Self {
                dev: m.dev(),
                ino: m.ino(),
                len: m.len(),
                mtime: m.mtime(),
                mtime_ns: m.mtime_nsec(),
                ctime: m.ctime(),
                ctime_ns: m.ctime_nsec(),
            }
        }
    }
    impl Fingerprint {
        pub(crate) fn len(&self) -> u64 {
            self.len
        }
    }

    pub(crate) struct SafeDir {
        file: File,
        pub(crate) path: PathBuf,
    }
    pub(crate) struct FileRead {
        pub(crate) bytes: Vec<u8>,
        pub(crate) fingerprint: Fingerprint,
    }

    fn io(path: &Path, error: std::io::Error) -> CollectionError {
        if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) {
            CollectionError::UnsafePath(path.to_path_buf())
        } else {
            CollectionError::Io {
                path: path.to_path_buf(),
                source: error,
            }
        }
    }

    impl SafeDir {
        pub(crate) fn absolute(path: &Path) -> Result<Self> {
            if !path.is_absolute() {
                return Err(CollectionError::UnsafePath(path.to_path_buf()));
            }
            let file = open_at(libc::AT_FDCWD, c"/", DIRECTORY_ACCESS).map_err(|e| io(path, e))?;
            let mut dir = Self {
                file,
                path: PathBuf::from("/"),
            };
            for component in path.components() {
                match component {
                    Component::RootDir => {}
                    Component::Normal(name) => {
                        let name = name
                            .to_str()
                            .ok_or_else(|| CollectionError::UnsafePath(path.to_path_buf()))?;
                        dir = dir
                            .child(name, false)?
                            .ok_or_else(|| CollectionError::UnsafePath(path.to_path_buf()))?;
                    }
                    _ => return Err(CollectionError::UnsafePath(path.to_path_buf())),
                }
            }
            Ok(dir)
        }

        fn open_child(&self, name: &str, directory: bool) -> std::io::Result<File> {
            let name =
                CString::new(name).map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
            let access = if directory {
                DIRECTORY_ACCESS
            } else {
                libc::O_RDONLY | libc::O_NONBLOCK
            };
            open_at(self.file.as_raw_fd(), &name, access)
        }

        pub(crate) fn child(&self, name: &str, missing_ok: bool) -> Result<Option<Self>> {
            crate::schema::safe_id(name)?;
            let path = self.path.join(name);
            match self.open_child(name, true) {
                Ok(file) => Ok(Some(Self { file, path })),
                Err(e) if missing_ok && e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(io(&path, e)),
            }
        }

        pub(crate) fn fingerprint(&self) -> Result<Fingerprint> {
            self.file
                .metadata()
                .map(Fingerprint::from)
                .map_err(|e| io(&self.path, e))
        }

        pub(crate) fn same_identity(&self, other: &Self) -> Result<bool> {
            let first = self.file.metadata().map_err(|e| io(&self.path, e))?;
            let second = other.file.metadata().map_err(|e| io(&other.path, e))?;
            Ok(first.dev() == second.dev() && first.ino() == second.ino())
        }

        pub(crate) fn read(&self, name: &str, limit: u64) -> Result<FileRead> {
            crate::schema::safe_id(name)?;
            let path = self.path.join(name);
            let mut file = self.open_child(name, false).map_err(|e| io(&path, e))?;
            let before = file.metadata().map_err(|e| io(&path, e))?;
            if !before.is_file() {
                return Err(CollectionError::UnsafePath(path));
            }
            if before.len() > limit {
                return Err(CollectionError::LimitExceeded("file bytes"));
            }
            let fingerprint = Fingerprint::from(before);
            let mut bytes = Vec::new();
            (&mut file)
                .take(
                    limit
                        .checked_add(1)
                        .ok_or(CollectionError::LimitExceeded("file bytes"))?,
                )
                .read_to_end(&mut bytes)
                .map_err(|e| io(&path, e))?;
            if bytes.len() as u64 > limit {
                return Err(CollectionError::LimitExceeded("file bytes"));
            }
            if Fingerprint::from(file.metadata().map_err(|e| io(&path, e))?) != fingerprint
                || self.file_fingerprint(name)? != fingerprint
            {
                return Err(CollectionError::ChangedDuringRead);
            }
            Ok(FileRead { bytes, fingerprint })
        }

        pub(crate) fn file_fingerprint(&self, name: &str) -> Result<Fingerprint> {
            let path = self.path.join(name);
            let file = self.open_child(name, false).map_err(|e| io(&path, e))?;
            let metadata = file.metadata().map_err(|e| io(&path, e))?;
            if !metadata.is_file() {
                return Err(CollectionError::UnsafePath(path));
            }
            Ok(Fingerprint::from(metadata))
        }

        pub(crate) fn names(&self, limit: u32) -> Result<Vec<String>> {
            let fd = open_at(
                self.file.as_raw_fd(),
                c".",
                libc::O_RDONLY | libc::O_DIRECTORY,
            )
            .map_err(|e| io(&self.path, e))?
            .into_raw_fd();
            let raw = unsafe { libc::fdopendir(fd) };
            if raw.is_null() {
                let error = std::io::Error::last_os_error();
                unsafe {
                    libc::close(fd);
                }
                return Err(io(&self.path, error));
            }
            struct Stream(*mut libc::DIR);
            impl Drop for Stream {
                fn drop(&mut self) {
                    unsafe {
                        libc::closedir(self.0);
                    }
                }
            }
            let stream = Stream(raw);
            unsafe {
                libc::rewinddir(stream.0);
            }
            let mut names = Vec::new();
            loop {
                #[cfg(target_os = "macos")]
                unsafe {
                    *libc::__error() = 0;
                }
                #[cfg(target_os = "linux")]
                unsafe {
                    *libc::__errno_location() = 0;
                }
                let entry = unsafe { libc::readdir(stream.0) };
                if entry.is_null() {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(0) {
                        return Err(io(&self.path, error));
                    }
                    break;
                }
                let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
                if name == b"." || name == b".." {
                    continue;
                }
                if names.len() >= limit as usize {
                    return Err(CollectionError::LimitExceeded("entry count"));
                }
                let name = std::str::from_utf8(name)
                    .map_err(|_| CollectionError::UnsafePath(self.path.clone()))?;
                crate::schema::safe_id(name)?;
                names.push(name.to_string());
            }
            names.sort();
            Ok(names)
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) use platform::*;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    use super::*;
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) struct Fingerprint;
    impl Fingerprint {
        pub(crate) fn len(&self) -> u64 {
            0
        }
    }
    pub(crate) struct FileRead {
        pub(crate) bytes: Vec<u8>,
        pub(crate) fingerprint: Fingerprint,
    }
    pub(crate) struct SafeDir {
        pub(crate) path: PathBuf,
    }
    impl SafeDir {
        pub(crate) fn absolute(_: &Path) -> Result<Self> {
            Err(CollectionError::UnsupportedPlatform)
        }
        pub(crate) fn child(&self, _: &str, _: bool) -> Result<Option<Self>> {
            Err(CollectionError::UnsupportedPlatform)
        }
        pub(crate) fn fingerprint(&self) -> Result<Fingerprint> {
            Err(CollectionError::UnsupportedPlatform)
        }
        pub(crate) fn same_identity(&self, _: &Self) -> Result<bool> {
            Err(CollectionError::UnsupportedPlatform)
        }
        pub(crate) fn read(&self, _: &str, _: u64) -> Result<FileRead> {
            Err(CollectionError::UnsupportedPlatform)
        }
        pub(crate) fn file_fingerprint(&self, _: &str) -> Result<Fingerprint> {
            Err(CollectionError::UnsupportedPlatform)
        }
        pub(crate) fn names(&self, _: u32) -> Result<Vec<String>> {
            Err(CollectionError::UnsupportedPlatform)
        }
    }
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) use platform::*;
