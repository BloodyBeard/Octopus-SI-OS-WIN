//! Owner-private files and directories, and directory sync, on Unix and
//! Windows.
//!
//! Unix: owned by the effective user, mode 0600 (files, sockets) or 0700
//! (directories), and never reached through a symlink.
//!
//! Windows: owned by the current user (or the token's default owner), never a
//! reparse point, and the access list grants access only to that user, SYSTEM
//! and Administrators, which are the Windows counterparts of root. Entries this
//! module creates get a protected access list that grants only the current
//! user and SYSTEM. Local sockets are named pipes there (see
//! `octopus-local-socket`), so they have no file to check; [`windows`] has the
//! pieces that pipes use instead.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
    /// A Unix domain socket file. Windows local sockets are named pipes, which
    /// are checked when they are connected to instead.
    #[cfg(unix)]
    Socket,
}

/// Opens an existing file for reading without following a final symlink or
/// reparse point.
pub fn open_read_no_follow(path: &Path) -> io::Result<File> {
    platform::open_read_no_follow(path)
}

/// Creates a new owner-private file for writing; fails if anything exists at
/// `path`, including a symlink.
pub fn create_private_file(path: &Path) -> io::Result<File> {
    platform::create_private_file(path)
}

/// Creates an owner-private directory; fails with `AlreadyExists` if `path`
/// exists.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    platform::create_private_dir(path)
}

/// Whether `path` is an owner-private entry of `kind` with exactly the
/// private permissions (Unix 0600/0700), and not a symlink.
pub fn is_private(path: &Path, kind: Kind) -> io::Result<bool> {
    platform::is_private(path, kind)
}

/// Whether `path` is owned by the current user and grants nothing to group
/// or others (Unix `mode & 0o077 == 0`), and is not a symlink.
pub fn is_owner_only(path: &Path) -> io::Result<bool> {
    platform::is_owner_only(path)
}

/// Whether an open file or directory is owned by the current user and grants
/// nothing to anyone else (Unix `mode & 0o077 == 0`), and is not a symlink
/// or reparse point.
pub fn is_owner_only_handle(file: &File) -> io::Result<bool> {
    platform::is_owner_only_handle(file)
}

/// Whether an open file is an owner-private (Unix 0600) regular file with a
/// single link.
pub fn is_private_single_link_file(file: &File) -> io::Result<bool> {
    platform::is_private_single_link_file(file)
}

/// Makes a directory's entries (new names, renames, removals) durable.
///
/// Unix: fsync on the directory. Windows opens a directory only with
/// `FILE_FLAG_BACKUP_SEMANTICS`, and flushes it only through a handle with
/// write access, so a plain `File::open(dir)` fails there with "Access is
/// denied". NTFS then flushes the directory's journaled metadata. A file
/// system that cannot flush a directory (`ERROR_INVALID_FUNCTION`, for
/// example FAT) has nothing more to make durable.
pub fn sync_directory(path: impl AsRef<Path>) -> io::Result<()> {
    platform::sync_directory(path.as_ref())
}

/// Makes an open file's data, or an open directory's entries, durable.
///
/// Unix: fsync. Windows flushes only through a handle with write access, so a
/// read-only handle (a file opened for reading, a directory opened to list)
/// is reopened with write access for the flush (`ReOpenFile`, same file, no
/// path lookup). As in [`sync_directory`], a file system that cannot flush a
/// directory has nothing more to make durable.
pub fn sync_handle(file: &File) -> io::Result<()> {
    platform::sync_handle(file)
}

/// Opens a directory so that `sync_all` on the handle makes its entries
/// durable, for callers that report opening and syncing separately. Prefer
/// [`sync_directory`], which also accepts file systems that cannot flush a
/// directory.
pub fn open_directory_for_sync(path: impl AsRef<Path>) -> io::Result<File> {
    platform::open_directory_for_sync(path.as_ref())
}

/// Which file an entry is, and when its contents or metadata last changed.
///
/// Unix: device, inode and ctime. Windows: volume serial number, the 128-bit
/// file ID (NTFS and ReFS) and NTFS's change time, the counterpart of ctime.
/// Two equal identities name the same file with no change in between, so a
/// replacement with the same size and modification time still differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileIdentity {
    pub device: u64,
    pub file: u128,
    pub changed: (i64, i64),
}

impl FileIdentity {
    /// Which file this is, without its change time.
    pub fn device_and_file(&self) -> (u64, u128) {
        (self.device, self.file)
    }
}

/// The identity of an open file.
pub fn identity_of_file(file: &File) -> io::Result<FileIdentity> {
    platform::identity_of_file(file)
}

/// The identity of the file at `path`, following a final symlink like
/// `std::fs::metadata`.
pub fn identity_of_path(path: impl AsRef<Path>) -> io::Result<FileIdentity> {
    platform::identity_of_path(path.as_ref())
}

/// The identity of the entry at `path` itself: a final symlink (Windows: any
/// reparse point) is not followed, like `std::fs::symlink_metadata`.
pub fn identity_of_path_no_follow(path: impl AsRef<Path>) -> io::Result<FileIdentity> {
    platform::identity_of_path_no_follow(path.as_ref())
}

/// Whether `path` is already canonical: absolute, without symlinks, `.` or
/// `..`. On Windows the comparison ignores the `\\?\` prefix and letter case.
pub fn is_canonical(path: &Path) -> io::Result<bool> {
    let canonical = std::fs::canonicalize(path)?;
    Ok(platform::same_path(&canonical, path))
}

/// Whether two absolute paths spell the same location: equal on Unix; on
/// Windows equal once the `\\?\` prefix and `/` separators are set aside and
/// ignoring ASCII letter case (NTFS names are case-insensitive).
pub fn same_path(first: &Path, second: &Path) -> bool {
    platform::same_path(first, second)
}

/// `path` in its ordinary form. Unix: unchanged. Windows: a `\\?\C:\…` or
/// `\\?\UNC\server\share\…` path (what `canonicalize` returns) becomes
/// `C:\…` or `\\server\share\…` when that names the same file: every
/// component is one Win32 path parsing leaves alone (no trailing dot or
/// space, no reserved device name such as `CON` or `NUL`, no `<>:"|?*` or
/// control characters). Otherwise the verbatim form is kept.
pub fn ordinary_path(path: PathBuf) -> PathBuf {
    platform::ordinary_path(path)
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};

    fn euid() -> u32 {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() }
    }

    pub(super) fn open_read_no_follow(path: &Path) -> io::Result<File> {
        OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path)
    }

    pub(super) fn create_private_file(path: &Path) -> io::Result<File> {
        OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(path)
    }

    pub(super) fn create_private_dir(path: &Path) -> io::Result<()> {
        std::fs::create_dir(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }

    pub(super) fn is_private(path: &Path, kind: Kind) -> io::Result<bool> {
        let metadata = std::fs::symlink_metadata(path)?;
        let file_type = metadata.file_type();
        let (expected_type, mode) = match kind {
            Kind::File => (file_type.is_file(), 0o600),
            Kind::Directory => (file_type.is_dir(), 0o700),
            Kind::Socket => (file_type.is_socket(), 0o600),
        };
        Ok(expected_type && !file_type.is_symlink() && metadata.uid() == euid() && metadata.mode() & 0o777 == mode)
    }

    pub(super) fn is_owner_only(path: &Path) -> io::Result<bool> {
        let metadata = std::fs::symlink_metadata(path)?;
        Ok(!metadata.file_type().is_symlink() && metadata.uid() == euid() && metadata.mode() & 0o077 == 0)
    }

    pub(super) fn is_private_single_link_file(file: &File) -> io::Result<bool> {
        let metadata = file.metadata()?;
        Ok(metadata.is_file() && metadata.uid() == euid() && metadata.mode() & 0o777 == 0o600 && metadata.nlink() == 1)
    }

    pub(super) fn same_path(canonical: &Path, path: &Path) -> bool {
        canonical == path
    }

    pub(super) fn ordinary_path(path: PathBuf) -> PathBuf {
        path
    }

    pub(super) fn sync_directory(path: &Path) -> io::Result<()> {
        File::open(path)?.sync_all()
    }

    pub(super) fn open_directory_for_sync(path: &Path) -> io::Result<File> {
        File::open(path)
    }

    fn identity(metadata: &std::fs::Metadata) -> FileIdentity {
        FileIdentity {
            device: metadata.dev(),
            file: u128::from(metadata.ino()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }

    pub(super) fn identity_of_file(file: &File) -> io::Result<FileIdentity> {
        Ok(identity(&file.metadata()?))
    }

    pub(super) fn identity_of_path(path: &Path) -> io::Result<FileIdentity> {
        Ok(identity(&std::fs::metadata(path)?))
    }

    pub(super) fn identity_of_path_no_follow(path: &Path) -> io::Result<FileIdentity> {
        Ok(identity(&std::fs::symlink_metadata(path)?))
    }

    pub(super) fn sync_handle(file: &File) -> io::Result<()> {
        file.sync_all()
    }

    pub(super) fn is_owner_only_handle(file: &File) -> io::Result<bool> {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        // SAFETY: geteuid has no preconditions.
        Ok(!metadata.file_type().is_symlink() && metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o077 == 0)
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;

    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
        SetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        GetAce, GetSecurityDescriptorDacl, GetTokenInformation, TokenOwner, TokenUser, ACCESS_ALLOWED_ACE,
        ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_OWNER, TOKEN_QUERY,
        TOKEN_USER,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO, FILE_ID_INFO,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES,
        READ_CONTROL, WRITE_DAC,
    };
    use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
    use windows_sys::Win32::System::SystemServices::{ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// SYSTEM, Administrators, OWNER RIGHTS and CREATOR OWNER may appear in a
    /// private access list next to the owner.
    const TRUSTED: [&str; 4] = ["S-1-5-18", "S-1-5-32-544", "S-1-3-4", "S-1-3-0"];

    pub(crate) struct LocalMemory(pub(crate) HLOCAL);
    impl Drop for LocalMemory {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: the pointer was allocated by a Win32 call that documents LocalFree.
                unsafe { LocalFree(self.0) };
            }
        }
    }

    fn handle(file: &File) -> HANDLE {
        file.as_raw_handle() as HANDLE
    }

    fn reparse_point(attributes: u32) -> bool {
        attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }

    fn not_a_reparse_point(file: File) -> io::Result<File> {
        if reparse_point(file.metadata()?.file_attributes()) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "path is a symlink or reparse point"));
        }
        Ok(file)
    }

    pub(super) fn open_read_no_follow(path: &Path) -> io::Result<File> {
        not_a_reparse_point(OpenOptions::new().read(true).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT).open(path)?)
    }

    pub(super) fn create_private_file(path: &Path) -> io::Result<File> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .access_mode(FILE_GENERIC_WRITE | READ_CONTROL | WRITE_DAC)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        set_owner_only(&file, false)?;
        Ok(file)
    }

    pub(super) fn create_private_dir(path: &Path) -> io::Result<()> {
        std::fs::create_dir(path)?;
        let directory = open_entry(path, READ_CONTROL | WRITE_DAC)?;
        set_owner_only(&directory, true)
    }

    fn open_entry(path: &Path, access: u32) -> io::Result<File> {
        OpenOptions::new()
            .access_mode(access | FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
    }

    pub(super) fn is_private(path: &Path, kind: Kind) -> io::Result<bool> {
        let metadata = std::fs::symlink_metadata(path)?;
        let reparse = reparse_point(metadata.file_attributes());
        let shape = match kind {
            Kind::File => metadata.is_file(),
            Kind::Directory => metadata.is_dir(),
        };
        if reparse || !shape {
            return Ok(false);
        }
        owner_only(&open_entry(path, READ_CONTROL)?)
    }

    pub(super) fn is_owner_only(path: &Path) -> io::Result<bool> {
        let metadata = std::fs::symlink_metadata(path)?;
        if reparse_point(metadata.file_attributes()) {
            return Ok(false);
        }
        owner_only(&open_entry(path, READ_CONTROL)?)
    }

    pub(super) fn is_private_single_link_file(file: &File) -> io::Result<bool> {
        let metadata = file.metadata()?;
        if !metadata.is_file() || reparse_point(metadata.file_attributes()) {
            return Ok(false);
        }
        let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: the handle is open for the lifetime of `file` and `information` is writable.
        if unsafe { GetFileInformationByHandle(handle(file), &mut information) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(information.nNumberOfLinks == 1 && owner_only(file)?)
    }

    pub(super) fn same_path(canonical: &Path, path: &Path) -> bool {
        plain(canonical).eq_ignore_ascii_case(&plain(path))
    }

    pub(super) fn ordinary_path(path: PathBuf) -> PathBuf {
        use std::path::{Component, Prefix};
        let mut components = path.components();
        let mut ordinary = match components.next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::VerbatimDisk(letter) => PathBuf::from(format!("{}:", letter as char)),
                Prefix::VerbatimUNC(server, share) => {
                    let mut root = std::ffi::OsString::from(r"\\");
                    root.push(server);
                    root.push(r"\");
                    root.push(share);
                    PathBuf::from(root)
                }
                _ => return path,
            },
            _ => return path,
        };
        for component in components {
            match component {
                Component::RootDir => ordinary.push(r"\"),
                Component::Normal(name) if win32_keeps(name) => ordinary.push(name),
                _ => return path,
            }
        }
        ordinary
    }

    /// Whether Win32 path parsing passes this name through unchanged.
    fn win32_keeps(name: &std::ffi::OsStr) -> bool {
        const RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
        let Some(name) = name.to_str() else { return false };
        if name.is_empty()
            || name.ends_with(['.', ' '])
            || name.chars().any(|c| c < ' ' || "<>:\"|?*/\\".contains(c))
        {
            return false;
        }
        let stem = name.split('.').next().unwrap_or(name).trim_end().to_ascii_uppercase();
        let numbered = |base: &str| {
            stem.strip_prefix(base).is_some_and(|digit| {
                matches!(digit, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")
            })
        };
        !(RESERVED.contains(&stem.as_str()) || numbered("COM") || numbered("LPT"))
    }

    const ERROR_INVALID_FUNCTION: i32 = 1;

    pub(super) fn sync_directory(path: &Path) -> io::Result<()> {
        match open_directory_for_sync(path)?.sync_all() {
            Err(error) if error.raw_os_error() == Some(ERROR_INVALID_FUNCTION) => Ok(()),
            result => result,
        }
    }

    /// A second handle to the same open file or directory with `access`,
    /// without a path lookup.
    fn reopen(file: &File, access: u32) -> io::Result<File> {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::Storage::FileSystem::{
            ReOpenFile, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        // SAFETY: `file` is an open handle; the result is checked and owned below.
        let reopened = unsafe {
            ReOpenFile(
                handle(file),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            )
        };
        if reopened == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: ReOpenFile returned a new handle this call owns.
        Ok(unsafe { File::from_raw_handle(reopened as _) })
    }

    pub(super) fn is_owner_only_handle(file: &File) -> io::Result<bool> {
        if reparse_point(file.metadata()?.file_attributes()) {
            return Ok(false);
        }
        owner_only(&reopen(file, READ_CONTROL | FILE_READ_ATTRIBUTES)?)
    }

    pub(super) fn sync_handle(file: &File) -> io::Result<()> {
        use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
        const GENERIC_WRITE: u32 = 0x4000_0000;
        let result = match file.sync_all() {
            Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
                reopen(file, GENERIC_WRITE)?.sync_all()
            }
            result => result,
        };
        match result {
            Err(error) if error.raw_os_error() == Some(ERROR_INVALID_FUNCTION) => Ok(()),
            result => result,
        }
    }

    pub(super) fn identity_of_file(file: &File) -> io::Result<FileIdentity> {
        let mut id: FILE_ID_INFO = unsafe { std::mem::zeroed() };
        // SAFETY: the buffer is a FILE_ID_INFO of the size passed.
        if unsafe {
            GetFileInformationByHandleEx(
                handle(file),
                FileIdInfo,
                (&mut id as *mut FILE_ID_INFO).cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut basic: FILE_BASIC_INFO = unsafe { std::mem::zeroed() };
        // SAFETY: the buffer is a FILE_BASIC_INFO of the size passed.
        if unsafe {
            GetFileInformationByHandleEx(
                handle(file),
                FileBasicInfo,
                (&mut basic as *mut FILE_BASIC_INFO).cast(),
                std::mem::size_of::<FILE_BASIC_INFO>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(FileIdentity {
            device: id.VolumeSerialNumber,
            file: u128::from_le_bytes(id.FileId.Identifier),
            changed: (basic.ChangeTime, 0),
        })
    }

    pub(super) fn identity_of_path(path: &Path) -> io::Result<FileIdentity> {
        // Attribute access only, and every sharing mode, so querying never
        // blocks or is blocked by writers; backup semantics also opens folders.
        let file = OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)?;
        identity_of_file(&file)
    }

    pub(super) fn identity_of_path_no_follow(path: &Path) -> io::Result<FileIdentity> {
        identity_of_file(&open_entry(path, 0)?)
    }

    /// A handle to the file or directory at `path` itself (never through a
    /// reparse point) with attribute access only, enough to hold its identity.
    pub fn open_entry_handle(path: &Path) -> io::Result<File> {
        open_entry(path, 0)
    }

    pub(super) fn open_directory_for_sync(path: &Path) -> io::Result<File> {
        const GENERIC_READ: u32 = 0x8000_0000;
        const GENERIC_WRITE: u32 = 0x4000_0000;
        OpenOptions::new()
            .access_mode(GENERIC_READ | GENERIC_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
    }

    /// The path without its `\\?\` (verbatim) prefix and with `\` separators.
    fn plain(path: &Path) -> String {
        let text = path.to_string_lossy().replace('/', "\\");
        match text.strip_prefix(r"\\?\UNC\") {
            Some(rest) => format!(r"\\{rest}"),
            None => text.strip_prefix(r"\\?\").unwrap_or(&text).to_string(),
        }
    }


    fn sid_string(sid: PSID) -> io::Result<String> {
        let mut text = std::ptr::null_mut();
        // SAFETY: `sid` is a valid SID owned by the caller; the result is freed below.
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let _free = LocalMemory(text.cast());
        let mut length = 0;
        // SAFETY: ConvertSidToStringSidW returns a NUL-terminated wide string.
        while unsafe { *text.add(length) } != 0 {
            length += 1;
        }
        // SAFETY: `length` UTF-16 units precede the terminator.
        Ok(String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) }))
    }

    /// The current user's SID and the token's default owner SID (an elevated
    /// administrator's new files are owned by Administrators).
    fn principals() -> io::Result<&'static [String]> {
        static PRINCIPALS: OnceLock<Vec<String>> = OnceLock::new();
        if let Some(principals) = PRINCIPALS.get() {
            return Ok(principals);
        }
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: GetCurrentProcess returns a pseudo handle; `token` receives a handle closed below.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let result = (|| -> io::Result<Vec<String>> {
            let user = token_information(token, TokenUser)?;
            let owner = token_information(token, TokenOwner)?;
            // SAFETY: the buffers hold TOKEN_USER / TOKEN_OWNER as requested.
            let user_sid = unsafe { (*(user.as_ptr() as *const TOKEN_USER)).User.Sid };
            let owner_sid = unsafe { (*(owner.as_ptr() as *const TOKEN_OWNER)).Owner };
            Ok(vec![sid_string(user_sid)?, sid_string(owner_sid)?])
        })();
        // SAFETY: `token` was opened above.
        unsafe { CloseHandle(token) };
        let found = result?;
        Ok(PRINCIPALS.get_or_init(|| found).as_slice())
    }

    fn token_information(token: HANDLE, class: i32) -> io::Result<Vec<u64>> {
        let mut length = 0u32;
        // SAFETY: a null buffer with zero length asks only for the required size.
        unsafe { GetTokenInformation(token, class, std::ptr::null_mut(), 0, &mut length) };
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0u64; (length as usize).div_ceil(8)];
        // SAFETY: the buffer is at least `length` bytes and 8-byte aligned.
        if unsafe { GetTokenInformation(token, class, buffer.as_mut_ptr().cast(), length, &mut length) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(buffer)
    }

    /// The entry is owned by the current user and its access list grants
    /// access to nobody else except SYSTEM and Administrators.
    fn owner_only(entry: &File) -> io::Result<bool> {
        let principals = principals()?;
        let mut owner: PSID = std::ptr::null_mut();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: the handle was opened with READ_CONTROL; outputs point into `descriptor`.
        let status = unsafe {
            GetSecurityInfo(
                handle(entry),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let _free = LocalMemory(descriptor);
        if owner.is_null() || !principals.contains(&sid_string(owner)?) || dacl.is_null() {
            // A missing access list grants everyone everything.
            return Ok(false);
        }
        // SAFETY: `dacl` points into the live security descriptor.
        let count = unsafe { (*dacl).AceCount };
        for index in 0..u32::from(count) {
            let mut ace = std::ptr::null_mut();
            // SAFETY: `index` is below the ACE count of this access list.
            if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: every ACE starts with an ACE_HEADER.
            let kind = u32::from(unsafe { (*(ace as *const ACE_HEADER)).AceType });
            if kind == ACCESS_DENIED_ACE_TYPE {
                continue;
            }
            if kind != ACCESS_ALLOWED_ACE_TYPE {
                return Ok(false);
            }
            // SAFETY: an ACCESS_ALLOWED_ACE stores its SID starting at SidStart.
            let sid = unsafe { std::ptr::addr_of!((*(ace as *const ACCESS_ALLOWED_ACE)).SidStart) } as PSID;
            let sid = sid_string(sid)?;
            if !principals.contains(&sid) && !TRUSTED.contains(&sid.as_str()) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// A self-relative security descriptor whose protected access list grants
    /// full access only to the current user and SYSTEM. In a directory's list
    /// the entries are inherited by new children.
    pub(crate) fn owner_only_descriptor(directory: bool) -> io::Result<LocalMemory> {
        let user = &principals()?[0];
        let inherit = if directory { "OICI" } else { "" };
        let sddl: Vec<u16> = format!("D:P(A;{inherit};FA;;;{user})(A;{inherit};FA;;;SY)")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated; the caller frees the descriptor.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut descriptor, std::ptr::null_mut())
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(LocalMemory(descriptor))
    }

    /// Replaces the entry's access list with a protected one that grants only
    /// the current user and SYSTEM; directories pass it on to new children.
    fn set_owner_only(entry: &File, directory: bool) -> io::Result<()> {
        let descriptor = owner_only_descriptor(directory)?;
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl: *mut ACL = std::ptr::null_mut();
        // SAFETY: `descriptor` is a valid self-relative security descriptor.
        if unsafe { GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted) } == 0 || present == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the handle was opened with WRITE_DAC; `dacl` lives in `descriptor`.
        let status = unsafe {
            SetSecurityInfo(
                handle(entry),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null(),
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(())
    }

    /// Security attributes for a kernel object (such as a named pipe) that only
    /// the current user and SYSTEM may open.
    pub struct OwnerOnlySecurity {
        _descriptor: LocalMemory,
        attributes: SECURITY_ATTRIBUTES,
    }

    // SAFETY: the descriptor is never written after creation and is freed once.
    unsafe impl Send for OwnerOnlySecurity {}
    // SAFETY: as above; shared use only reads it.
    unsafe impl Sync for OwnerOnlySecurity {}

    impl OwnerOnlySecurity {
        pub fn new() -> io::Result<Self> {
            let descriptor = owner_only_descriptor(false)?;
            let attributes = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: 0,
            };
            Ok(Self { _descriptor: descriptor, attributes })
        }

        /// A `SECURITY_ATTRIBUTES` pointer that stays valid while `self` lives.
        pub fn as_ptr(&self) -> *mut std::ffi::c_void {
            std::ptr::addr_of!(self.attributes).cast_mut().cast()
        }
    }

    /// Whether process `pid` runs as the current user. Used to check the other
    /// end of a named pipe, which has no file owner to compare.
    pub fn process_is_current_user(pid: u32) -> io::Result<bool> {
        let user = &principals()?[0];
        // SAFETY: OpenProcess has no memory preconditions; the handle is closed below.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: `process` is open with query rights; `token` is closed below.
        let opened = unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) };
        // SAFETY: `process` was opened above.
        unsafe { CloseHandle(process) };
        if opened == 0 {
            return Err(io::Error::last_os_error());
        }
        let result = token_information(token, TokenUser).and_then(|buffer| {
            // SAFETY: the buffer holds TOKEN_USER as requested.
            sid_string(unsafe { (*(buffer.as_ptr() as *const TOKEN_USER)).User.Sid })
        });
        // SAFETY: `token` was opened above.
        unsafe { CloseHandle(token) };
        Ok(&result? == user)
    }

    /// Gives an existing directory owned by the current user a protected
    /// access list for that user and SYSTEM, inherited by its contents: the
    /// counterpart of `chmod 0700` on a directory the user owns.
    pub fn make_dir_owner_only(path: &Path) -> io::Result<()> {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_dir() || reparse_point(metadata.file_attributes()) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "path is not a real directory"));
        }
        let directory = open_entry(path, READ_CONTROL | WRITE_DAC)?;
        let mut owner: PSID = std::ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: the handle was opened with READ_CONTROL; `owner` points into `descriptor`.
        let status = unsafe {
            GetSecurityInfo(
                handle(&directory),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let _free = LocalMemory(descriptor);
        if owner.is_null() || !principals()?.contains(&sid_string(owner)?) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "directory is not owned by the current user"));
        }
        set_owner_only(&directory, true)
    }

    /// Whether the server end of a connected named pipe client runs as the
    /// current user.
    pub fn pipe_server_is_current_user(pipe: &File) -> io::Result<bool> {
        let mut server = 0;
        // SAFETY: the handle is an open pipe client; `server` is writable.
        if unsafe { GetNamedPipeServerProcessId(handle(pipe), &mut server) } == 0 {
            return Err(io::Error::last_os_error());
        }
        process_is_current_user(server)
    }
}

#[cfg(windows)]
mod beneath_windows;

/// Windows-only pieces: objects that are not files, and no-follow access to
/// entries beneath an open directory handle.
#[cfg(windows)]
pub mod windows {
    pub use super::platform::{
        make_dir_owner_only, open_entry_handle, pipe_server_is_current_user, process_is_current_user,
        OwnerOnlySecurity,
    };

    /// Entries opened by name beneath an open directory, never through a
    /// symlink or other reparse point.
    pub mod beneath {
        pub use crate::beneath_windows::*;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn created_entries_are_private_and_symlinks_are_refused() {
        let root = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let directory = root.join("private");
        create_private_dir(&directory).unwrap();
        assert!(is_private(&directory, Kind::Directory).unwrap());
        assert!(is_owner_only(&directory).unwrap());
        assert_eq!(create_private_dir(&directory).unwrap_err().kind(), io::ErrorKind::AlreadyExists);

        let file = directory.join("record.json");
        std::io::Write::write_all(&mut create_private_file(&file).unwrap(), b"{}").unwrap();
        assert!(is_private(&file, Kind::File).unwrap());
        assert!(!is_private(&file, Kind::Directory).unwrap());
        assert!(is_private_single_link_file(&open_read_no_follow(&file).unwrap()).unwrap());
        assert!(create_private_file(&file).is_err(), "an existing file is never replaced");
        assert!(is_canonical(&file).unwrap());
        // Windows drops `..` when joining onto a `\\?\` path, so build the
        // dotted path from the plain form for the `..` to survive.
        let plain = std::path::PathBuf::from(directory.to_string_lossy().trim_start_matches(r"\\?\").to_string());
        assert!(!is_canonical(&plain.join("..").join("private").join("record.json")).unwrap());
        sync_directory(&directory).unwrap();

        // The same file keeps its identity; a same-size replacement does not.
        let tracked = directory.join("tracked.json");
        std::fs::write(&tracked, b"{}").unwrap();
        let before = identity_of_path(&tracked).unwrap();
        assert_eq!(before, identity_of_file(&open_read_no_follow(&tracked).unwrap()).unwrap());
        assert_eq!(before, identity_of_path(&tracked).unwrap());
        let replacement = directory.join("replacement.json");
        std::fs::write(&replacement, b"[]").unwrap();
        std::fs::rename(&replacement, &tracked).unwrap();
        assert_ne!(before, identity_of_path(&tracked).unwrap());
        assert_ne!(identity_of_path(&directory).unwrap(), identity_of_path(&tracked).unwrap());
        assert_eq!(identity_of_path(&tracked).unwrap(), identity_of_path_no_follow(&tracked).unwrap());
        std::fs::remove_file(&tracked).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let link = directory.join("link.json");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            assert!(open_read_no_follow(&link).is_err());
            assert!(!is_private(&link, Kind::File).unwrap());
            assert!(create_private_file(&link).is_err());
            std::fs::hard_link(&file, directory.join("second.json")).unwrap();
            assert!(!is_private_single_link_file(&open_read_no_follow(&file).unwrap()).unwrap());
            std::fs::remove_file(directory.join("second.json")).unwrap();
            assert!(is_private_single_link_file(&open_read_no_follow(&file).unwrap()).unwrap());
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(!is_private(&file, Kind::File).unwrap());
        }
    }
}
