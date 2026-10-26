//! Windows counterparts of `openat(…, O_NOFOLLOW)`, `fstatat(…,
//! AT_SYMLINK_NOFOLLOW)`, `readlinkat`, `readdir`, `renameat2(…,
//! RENAME_NOREPLACE)` and `unlinkat`.
//!
//! Each entry is opened by one name relative to an open directory handle
//! (`NtCreateFile` with a root directory), never through a path a concurrent
//! rename could redirect. `FILE_OPEN_REPARSE_POINT` opens a symlink, junction
//! or other reparse point itself instead of what it points at, and every
//! opener here then refuses it with `ERROR_STOPPED_ON_SYMLINK` (see
//! [`is_link_refusal`]), the counterpart of `ELOOP`. Only [`entry`] and
//! [`read_link`] look at a link without refusing it.

use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Component, Path, PathBuf};

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    NtCreateFile, NtSetInformationFile, FileRenameInformation, FileRenameInformationEx, FILE_CREATE,
    FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT, FILE_RENAME_INFORMATION,
    FILE_SYNCHRONOUS_IO_NONALERT,
};
use windows_sys::Win32::Foundation::{
    RtlNtStatusToDosError, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED, ERROR_NO_MORE_FILES,
    ERROR_STOPPED_ON_SYMLINK, HANDLE, NTSTATUS, STATUS_INVALID_INFO_CLASS, STATUS_INVALID_PARAMETER,
    STATUS_NOT_SUPPORTED, UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    FileAttributeTagInfo, FileBasicInfo, FileDispositionInfo, FileDispositionInfoEx, FileFullDirectoryInfo,
    FileFullDirectoryRestartInfo, FileStandardInfo, GetFileInformationByHandleEx, SetFileInformationByHandle,
    DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_BASIC_INFO,
    FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO,
    FILE_DISPOSITION_INFO_EX, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_FULL_DIR_INFO,
    FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_STANDARD_INFO, FILE_TRAVERSE, READ_CONTROL, SYNCHRONIZE,
};
use windows_sys::Win32::System::Kernel::OBJ_CASE_INSENSITIVE;
use windows_sys::Win32::System::WindowsProgramming::{FILE_RENAME_FLAG_POSIX_SEMANTICS, FILE_RENAME_FLAG_REPLACE_IF_EXISTS};
use windows_sys::Win32::System::SystemServices::IO_REPARSE_TAG_SYMLINK;
use windows_sys::Win32::System::IO::{DeviceIoControl, IO_STATUS_BLOCK};

use crate::FileIdentity;

const FSCTL_GET_REPARSE_POINT: u32 = 0x0009_00A8;
const MAXIMUM_REPARSE_DATA_BUFFER_SIZE: usize = 16 * 1024;
const SYMLINK_FLAG_RELATIVE: u32 = 1;

/// What an entry is, seen without following it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    /// A symbolic link (`IO_REPARSE_TAG_SYMLINK`), to a file or a directory.
    Symlink,
    /// Any other reparse point: a junction, an AF_UNIX socket, a cloud
    /// placeholder. Callers treat these like Unix special files.
    Other,
}

/// An entry's `lstat`: its identity, kind, size and times (100 ns ticks
/// since 1601, the units of NTFS timestamps).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub identity: FileIdentity,
    pub kind: EntryKind,
    pub len: u64,
    pub modified: i64,
    pub changed: i64,
    pub links: u32,
}

/// A symbolic link's stored target, and whether it is relative to the
/// link's directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkTarget {
    pub target: OsString,
    pub relative: bool,
}

/// Whether `error` is the refusal of a symlink or other reparse point by one
/// of the no-follow openers here (the counterpart of Unix `ELOOP`).
pub fn is_link_refusal(error: &io::Error) -> bool {
    error.raw_os_error() == Some(ERROR_STOPPED_ON_SYMLINK as i32)
}

fn link_refusal() -> io::Error {
    io::Error::from_raw_os_error(ERROR_STOPPED_ON_SYMLINK as i32)
}

fn handle(file: &File) -> HANDLE {
    file.as_raw_handle() as HANDLE
}

/// Opens the directory at `path` without following a link at any component.
///
/// The walk starts at the path's root (`C:\`, `\\server\share\`, or their
/// `\\?\` forms) and opens every following component by name beneath the one
/// before it. `.` and `..` are refused rather than resolved.
pub fn open_directory_path(path: &Path) -> io::Result<File> {
    let mut root = PathBuf::new();
    let mut names = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) if names.is_empty() => root.push(prefix.as_os_str()),
            Component::RootDir if names.is_empty() => root.push(r"\"),
            Component::Normal(name) => names.push(name),
            _ => return Err(invalid("a no-follow directory walk needs an absolute path without . or ..")),
        }
    }
    if !path.is_absolute() {
        return Err(invalid("a no-follow directory walk needs an absolute path without . or .."));
    }
    let mut directory = OpenOptions::new()
        .access_mode(DIRECTORY_ACCESS)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&root)?;
    refuse_reparse(&directory)?;
    for name in names {
        directory = open_directory(&directory, name)?;
    }
    Ok(directory)
}

/// What directory handles here are opened for: listing, opening beneath,
/// attributes, and reading the access list (privacy checks read it from the
/// handle, see `is_owner_only_handle`).
const DIRECTORY_ACCESS: u32 = FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE;

/// Opens the directory `name` beneath `parent` for listing and further
/// opens; a link there is refused. A directory whose access list does not
/// let this user read it (another user's folder on the way to one's own) is
/// opened without that right.
pub fn open_directory(parent: &File, name: &OsStr) -> io::Result<File> {
    let directory = match open_relative(parent, name, DIRECTORY_ACCESS, FILE_DIRECTORY_FILE) {
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            open_relative(parent, name, DIRECTORY_ACCESS & !READ_CONTROL, FILE_DIRECTORY_FILE)?
        }
        result => result?,
    };
    refuse_reparse(&directory)?;
    Ok(directory)
}

/// Opens the regular file `name` beneath `parent` for reading. A link there
/// is refused; a directory fails with `ErrorKind::IsADirectory`.
pub fn open_file(parent: &File, name: &OsStr) -> io::Result<File> {
    let file = open_relative(parent, name, FILE_GENERIC_READ | SYNCHRONIZE, FILE_NON_DIRECTORY_FILE).map_err(|error| {
        // NTFS answers STATUS_FILE_IS_A_DIRECTORY, which reads as "access
        // denied"; say what it is when the name is a directory.
        match entry(parent, name) {
            Ok(found) if found.kind == EntryKind::Directory => {
                io::Error::new(io::ErrorKind::IsADirectory, "entry is a directory, not a file")
            }
            _ => error,
        }
    })?;
    refuse_reparse(&file)?;
    Ok(file)
}

/// Opens the regular file at `relative` (plain names) beneath the directory
/// `root`, opening every directory on the way by name beneath the one before
/// it. A link anywhere on the way, or at the file, is refused.
pub fn open_file_at(root: &Path, relative: &[&OsStr]) -> io::Result<File> {
    let (leaf, directories) = relative.split_last().ok_or_else(|| invalid("a file beneath a root needs a name"))?;
    let mut directory = open_directory_path(root)?;
    for name in directories {
        directory = open_directory(&directory, name)?;
    }
    open_file(&directory, leaf)
}

/// The entry `name` beneath `parent` itself, without following it.
pub fn entry(parent: &File, name: &OsStr) -> io::Result<Entry> {
    entry_of(&open_relative(parent, name, FILE_READ_ATTRIBUTES | SYNCHRONIZE, 0)?)
}

/// The entry an open handle names (the link itself if the handle was opened
/// on one).
pub fn entry_of(file: &File) -> io::Result<Entry> {
    let tag: FILE_ATTRIBUTE_TAG_INFO = query(file, FileAttributeTagInfo)?;
    let basic: FILE_BASIC_INFO = query(file, FileBasicInfo)?;
    let standard: FILE_STANDARD_INFO = query(file, FileStandardInfo)?;
    let kind = if tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        if tag.ReparseTag == IO_REPARSE_TAG_SYMLINK { EntryKind::Symlink } else { EntryKind::Other }
    } else if tag.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        EntryKind::Directory
    } else {
        EntryKind::File
    };
    Ok(Entry {
        identity: crate::identity_of_file(file)?,
        kind,
        len: u64::try_from(standard.EndOfFile).map_err(|_| invalid("file reports a negative size"))?,
        modified: basic.LastWriteTime,
        changed: basic.ChangeTime,
        links: standard.NumberOfLinks,
    })
}

/// The target stored in the symbolic link `name` beneath `parent`.
pub fn read_link(parent: &File, name: &OsStr) -> io::Result<LinkTarget> {
    let link = open_relative(parent, name, FILE_READ_ATTRIBUTES | SYNCHRONIZE, 0)?;
    let mut buffer = vec![0u8; MAXIMUM_REPARSE_DATA_BUFFER_SIZE];
    let mut returned = 0u32;
    // SAFETY: the output buffer is writable for the length passed; no overlapped I/O.
    if unsafe {
        DeviceIoControl(
            handle(&link),
            FSCTL_GET_REPARSE_POINT,
            std::ptr::null(),
            0,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let buffer = &buffer[..returned as usize];
    let field = |offset: usize| -> io::Result<u16> {
        buffer.get(offset..offset + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or_else(|| invalid("short reparse data"))
    };
    let tag = u32::from_le_bytes(buffer.get(0..4).ok_or_else(|| invalid("short reparse data"))?.try_into().unwrap());
    if tag != IO_REPARSE_TAG_SYMLINK {
        return Err(invalid("entry is a reparse point but not a symbolic link"));
    }
    // REPARSE_DATA_BUFFER: tag (4), data length (2), reserved (2), then the
    // symbolic-link buffer: substitute offset/length, print offset/length
    // (2 each), flags (4), and the path buffer.
    let (substitute_offset, substitute_length) = (field(8)? as usize, field(10)? as usize);
    let (print_offset, print_length) = (field(12)? as usize, field(14)? as usize);
    let flags = u32::from_le_bytes(buffer.get(16..20).ok_or_else(|| invalid("short reparse data"))?.try_into().unwrap());
    let names = &buffer[20..];
    let (offset, length) = if print_length != 0 { (print_offset, print_length) } else { (substitute_offset, substitute_length) };
    let bytes = names.get(offset..offset + length).ok_or_else(|| invalid("reparse name lies outside its buffer"))?;
    let wide: Vec<u16> = bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
    Ok(LinkTarget { target: OsString::from_wide(&wide), relative: flags & SYMLINK_FLAG_RELATIVE != 0 })
}

/// The names in an open directory, without `.` and `..`, sorted. Fails once
/// there are more than `limit`.
pub fn names(directory: &File, limit: usize) -> io::Result<Vec<OsString>> {
    let mut buffer = vec![0u64; 8 * 1024];
    let mut names = Vec::new();
    let mut class = FileFullDirectoryRestartInfo;
    loop {
        // SAFETY: the buffer is writable for the byte length passed and 8-byte aligned.
        if unsafe {
            GetFileInformationByHandleEx(handle(directory), class, buffer.as_mut_ptr().cast(), (buffer.len() * 8) as u32)
        } == 0
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                break;
            }
            return Err(error);
        }
        class = FileFullDirectoryInfo;
        let bytes = buffer.as_ptr().cast::<u8>();
        let mut offset = 0usize;
        loop {
            // SAFETY: the system wrote a chain of FILE_FULL_DIR_INFO records
            // starting at offset 0, each linked by NextEntryOffset.
            let record = unsafe { &*bytes.add(offset).cast::<FILE_FULL_DIR_INFO>() };
            // SAFETY: FileName holds FileNameLength bytes inside the record.
            let name = unsafe {
                std::slice::from_raw_parts(record.FileName.as_ptr(), record.FileNameLength as usize / 2)
            };
            if name != [b'.' as u16] && name != [b'.' as u16, b'.' as u16] {
                names.push(OsString::from_wide(name));
                if names.len() > limit {
                    return Err(invalid("directory holds more entries than its bound"));
                }
            }
            if record.NextEntryOffset == 0 {
                break;
            }
            offset += record.NextEntryOffset as usize;
        }
    }
    names.sort();
    Ok(names)
}

/// Moves `source_name` beneath `source_parent` to `destination_name` beneath
/// `destination_parent`, failing with `AlreadyExists` if the destination
/// name is taken (`renameat2(…, RENAME_NOREPLACE)`). A link at the source is
/// moved as itself.
pub fn rename_no_replace(
    source_parent: &File,
    source_name: &OsStr,
    destination_parent: &File,
    destination_name: &OsStr,
) -> io::Result<()> {
    rename(source_parent, source_name, destination_parent, destination_name, false)
}

/// Like [`rename_no_replace`], but an existing destination file is replaced
/// in one step (`renameat`). POSIX semantics let it replace a file that is
/// still open; file systems without them use the classic replace.
pub fn rename_replace(
    source_parent: &File,
    source_name: &OsStr,
    destination_parent: &File,
    destination_name: &OsStr,
) -> io::Result<()> {
    rename(source_parent, source_name, destination_parent, destination_name, true)
}

fn rename(
    source_parent: &File,
    source_name: &OsStr,
    destination_parent: &File,
    destination_name: &OsStr,
    replace: bool,
) -> io::Result<()> {
    let source = open_relative(source_parent, source_name, DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE, 0)?;
    let name = single_name(destination_name)?;
    if replace {
        let flags = FILE_RENAME_FLAG_REPLACE_IF_EXISTS | FILE_RENAME_FLAG_POSIX_SEMANTICS;
        let status = set_rename(&source, destination_parent, &name, FileRenameInformationEx, flags);
        if ![STATUS_INVALID_PARAMETER, STATUS_INVALID_INFO_CLASS, STATUS_NOT_SUPPORTED].contains(&status) {
            return nt_result(status);
        }
    }
    // `ReplaceIfExists` is the low byte of the classic structure's flags.
    nt_result(set_rename(&source, destination_parent, &name, FileRenameInformation, u32::from(replace)))
}

fn set_rename(source: &File, destination_parent: &File, name: &[u16], class: i32, flags: u32) -> NTSTATUS {
    let header = std::mem::offset_of!(FILE_RENAME_INFORMATION, FileName);
    let length = header + name.len() * 2;
    let mut buffer = vec![0u64; length.div_ceil(8)];
    let information = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    // SAFETY: the buffer is 8-byte aligned and long enough for the header and
    // the name written after it.
    unsafe {
        (*information).Anonymous.Flags = flags;
        (*information).RootDirectory = handle(destination_parent);
        (*information).FileNameLength = (name.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(name.as_ptr(), std::ptr::addr_of_mut!((*information).FileName).cast::<u16>(), name.len());
    }
    let mut status_block: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is open with DELETE access; the buffer is valid for `length` bytes.
    unsafe { NtSetInformationFile(handle(source), &mut status_block, information.cast(), length as u32, class) }
}

/// Creates the directory `name` beneath `parent`, private to the current
/// user (as `create_private_dir`), and opens it; fails with `AlreadyExists`
/// if anything, a link included, has that name.
pub fn create_directory(parent: &File, name: &OsStr) -> io::Result<File> {
    create(parent, name, DIRECTORY_ACCESS, FILE_DIRECTORY_FILE, true)
}

/// Creates the file `name` beneath `parent`, private to the current user,
/// open for reading and writing; fails with `AlreadyExists` if anything, a
/// link included, has that name (`O_CREAT | O_EXCL | O_NOFOLLOW`, mode 0600).
pub fn create_file(parent: &File, name: &OsStr) -> io::Result<File> {
    create(parent, name, FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE | SYNCHRONIZE, FILE_NON_DIRECTORY_FILE, false)
}

fn create(parent: &File, name: &OsStr, access: u32, options: u32, directory: bool) -> io::Result<File> {
    let descriptor = crate::platform::owner_only_descriptor(directory)?;
    open_with(parent, name, access, options, FILE_CREATE, descriptor.0)
}

/// Removes the entry `name` beneath `parent`, which must still be the file
/// `expected` names (device and file ID). `directory` says whether it is a
/// real directory (which must be empty); a link, to a file or a directory, is
/// removed as itself like a file, as `unlinkat` does.
pub fn remove(parent: &File, name: &OsStr, directory: bool, expected: (u64, u128)) -> io::Result<()> {
    let entry = open_relative(parent, name, DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE, 0)?;
    let found = entry_of(&entry)?;
    if found.identity.device_and_file() != expected || (found.kind == EntryKind::Directory) != directory {
        return Err(invalid("entry to remove is no longer the expected file"));
    }
    let posix = FILE_DISPOSITION_INFO_EX { Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS };
    // SAFETY: the structure is valid for its size; the handle has DELETE access.
    if unsafe {
        SetFileInformationByHandle(
            handle(&entry),
            FileDispositionInfoEx,
            (&posix as *const FILE_DISPOSITION_INFO_EX).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    } != 0
    {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    // File systems and Windows versions without POSIX deletion take the
    // classic flag; the name then goes when the last handle closes.
    if !matches!(error.raw_os_error(), Some(code) if code == ERROR_INVALID_PARAMETER as i32 || code == ERROR_NOT_SUPPORTED as i32) {
        return Err(error);
    }
    let classic = FILE_DISPOSITION_INFO { DeleteFile: 1 };
    // SAFETY: as above.
    if unsafe {
        SetFileInformationByHandle(
            handle(&entry),
            FileDispositionInfo,
            (&classic as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// A second handle to the same open file or directory with `access`: an
/// open relative to the handle with an empty name, so no path is looked up.
pub(crate) fn reopen(file: &File, access: u32) -> io::Result<File> {
    let name = UNICODE_STRING { Length: 0, MaximumLength: 0, Buffer: std::ptr::null_mut() };
    let attributes = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: handle(file),
        ObjectName: &name,
        Attributes: 0,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };
    let mut opened: HANDLE = std::ptr::null_mut();
    let mut status_block: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: every pointer refers to a live local for the duration of the call.
    let status = unsafe {
        NtCreateFile(
            &mut opened,
            access | SYNCHRONIZE,
            &attributes,
            &mut status_block,
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };
    nt_result(status)?;
    // SAFETY: NtCreateFile succeeded, so `opened` is a handle this call owns.
    Ok(unsafe { File::from_raw_handle(opened as _) })
}

fn refuse_reparse(file: &File) -> io::Result<()> {
    let tag: FILE_ATTRIBUTE_TAG_INFO = query(file, FileAttributeTagInfo)?;
    if tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(link_refusal());
    }
    Ok(())
}

fn query<T>(file: &File, class: i32) -> io::Result<T> {
    let mut value = std::mem::MaybeUninit::<T>::zeroed();
    // SAFETY: the buffer is a T of the size passed; T is the structure `class` returns.
    if unsafe { GetFileInformationByHandleEx(handle(file), class, value.as_mut_ptr().cast(), std::mem::size_of::<T>() as u32) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call filled the structure.
    Ok(unsafe { value.assume_init() })
}

/// One path component as UTF-16: not empty, `.`, `..`, and without
/// separators, a stream suffix (`:`) or NUL, so it can only name a direct
/// child of the directory it is opened beneath.
fn single_name(name: &OsStr) -> io::Result<Vec<u16>> {
    let wide: Vec<u16> = name.encode_wide().collect();
    let dot = b'.' as u16;
    if wide.is_empty()
        || wide == [dot]
        || wide == [dot, dot]
        || wide.iter().any(|&unit| unit == 0 || unit == b'\\' as u16 || unit == b'/' as u16 || unit == b':' as u16)
    {
        return Err(invalid("a no-follow open takes one plain name"));
    }
    if wide.len() * 2 > u16::MAX as usize {
        return Err(invalid("name is too long"));
    }
    Ok(wide)
}

fn open_relative(parent: &File, name: &OsStr, access: u32, options: u32) -> io::Result<File> {
    open_with(parent, name, access, options, FILE_OPEN, std::ptr::null_mut())
}

fn open_with(
    parent: &File,
    name: &OsStr,
    access: u32,
    options: u32,
    disposition: u32,
    security: *mut std::ffi::c_void,
) -> io::Result<File> {
    let mut wide = single_name(name)?;
    let name = UNICODE_STRING {
        Length: (wide.len() * 2) as u16,
        MaximumLength: (wide.len() * 2) as u16,
        Buffer: wide.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: handle(parent),
        ObjectName: &name,
        Attributes: OBJ_CASE_INSENSITIVE as u32,
        SecurityDescriptor: security,
        SecurityQualityOfService: std::ptr::null(),
    };
    let mut opened: HANDLE = std::ptr::null_mut();
    let mut status_block: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: every pointer refers to a live local for the duration of the call.
    let status = unsafe {
        NtCreateFile(
            &mut opened,
            access,
            &attributes,
            &mut status_block,
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            disposition,
            options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };
    nt_result(status)?;
    // SAFETY: NtCreateFile succeeded, so `opened` is a handle this call owns.
    Ok(unsafe { File::from_raw_handle(opened as _) })
}

fn nt_result(status: NTSTATUS) -> io::Result<()> {
    if status >= 0 {
        return Ok(());
    }
    // SAFETY: a pure status-code translation.
    Err(io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn entries_are_reached_only_beneath_their_parent_and_never_through_links() {
        let root = tempfile::tempdir().unwrap();
        let root_path = std::fs::canonicalize(root.path()).unwrap();
        let top = open_directory_path(&root_path).unwrap();
        assert!(open_directory_path(&crate::ordinary_path(root_path.clone())).unwrap().metadata().unwrap().is_dir());

        let inner = create_directory(&top, OsStr::new("inner")).unwrap();
        assert!(crate::is_owner_only_handle(&inner).unwrap());
        assert_eq!(create_directory(&top, OsStr::new("inner")).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        let mut file = create_file(&inner, OsStr::new("a.txt")).unwrap();
        file.write_all(b"alpha").unwrap();
        crate::sync_handle(&file).unwrap();
        crate::sync_handle(&inner).unwrap();
        assert!(crate::is_owner_only_handle(&file).unwrap());
        assert_eq!(create_file(&inner, OsStr::new("a.txt")).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        drop(file);

        let mut read = String::new();
        open_file(&inner, OsStr::new("a.txt")).unwrap().read_to_string(&mut read).unwrap();
        assert_eq!(read, "alpha");
        assert_eq!(open_file(&top, OsStr::new("inner")).unwrap_err().kind(), io::ErrorKind::IsADirectory);
        assert_eq!(open_directory(&inner, OsStr::new("a.txt")).unwrap_err().kind(), io::ErrorKind::NotADirectory);
        assert_eq!(open_file(&inner, OsStr::new("missing")).unwrap_err().kind(), io::ErrorKind::NotFound);
        for bad in ["", ".", "..", r"a\b", "a/b", "a.txt:stream"] {
            assert!(open_file(&inner, OsStr::new(bad)).is_err(), "{bad:?} must not open");
        }

        let found = entry(&inner, OsStr::new("a.txt")).unwrap();
        assert_eq!((found.kind, found.len, found.links), (EntryKind::File, 5, 1));
        assert_eq!(entry(&top, OsStr::new("inner")).unwrap().kind, EntryKind::Directory);
        assert_eq!(names(&inner, 10).unwrap(), vec![OsString::from("a.txt")]);
        assert!(names(&inner, 0).is_err());

        // No-replace refuses a taken name; replace swaps it in one step.
        create_file(&inner, OsStr::new("b.txt")).unwrap().write_all(b"beta").unwrap();
        assert_eq!(
            rename_no_replace(&inner, OsStr::new("b.txt"), &inner, OsStr::new("a.txt")).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        rename_replace(&inner, OsStr::new("b.txt"), &inner, OsStr::new("a.txt")).unwrap();
        assert_eq!(names(&inner, 10).unwrap(), vec![OsString::from("a.txt")]);
        rename_no_replace(&inner, OsStr::new("a.txt"), &top, OsStr::new("moved.txt")).unwrap();
        assert_eq!(std::fs::read(root_path.join("moved.txt")).unwrap(), b"beta");

        // Removal checks the entry is still the expected file.
        let moved = entry(&top, OsStr::new("moved.txt")).unwrap().identity.device_and_file();
        let directory = entry(&top, OsStr::new("inner")).unwrap().identity.device_and_file();
        assert!(remove(&top, OsStr::new("moved.txt"), false, directory).is_err());
        assert!(remove(&top, OsStr::new("moved.txt"), true, moved).is_err());
        remove(&top, OsStr::new("moved.txt"), false, moved).unwrap();
        drop(inner);
        remove(&top, OsStr::new("inner"), true, directory).unwrap();
        assert!(names(&top, 10).unwrap().is_empty());

        // Symbolic links need Developer Mode or an elevated token; without
        // that privilege the link half of the test has nothing to create.
        const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;
        let target = root_path.join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("t.txt"), b"t").unwrap();
        match std::os::windows::fs::symlink_dir(&target, root_path.join("link")) {
            Err(error) if error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD) => return,
            result => result.unwrap(),
        }
        std::os::windows::fs::symlink_file(target.join("t.txt"), root_path.join("file-link")).unwrap();
        assert!(is_link_refusal(&open_directory(&top, OsStr::new("link")).unwrap_err()));
        assert!(is_link_refusal(&open_file(&top, OsStr::new("file-link")).unwrap_err()));
        assert!(is_link_refusal(&open_directory_path(&root_path.join("link")).unwrap_err()));
        assert!(open_directory_path(&root_path.join("link").join("nested")).is_err());
        assert_eq!(entry(&top, OsStr::new("link")).unwrap().kind, EntryKind::Symlink);
        let stored = read_link(&top, OsStr::new("file-link")).unwrap();
        assert!(!stored.relative);
        assert!(crate::same_path(std::path::Path::new(&stored.target), &target.join("t.txt")), "{stored:?}");
        let link = entry(&top, OsStr::new("link")).unwrap().identity.device_and_file();
        remove(&top, OsStr::new("link"), false, link).unwrap();
        assert!(target.join("t.txt").exists(), "removing a link leaves its target");
    }

    /// Runs each handle operation the folder-handle code depends on by
    /// itself and prints every result (`--nocapture` shows them), so a
    /// failure names the exact call.
    #[test]
    fn handle_operations_report_each_step() {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::Storage::FileSystem::ReOpenFile;
        const GENERIC_WRITE: u32 = 0x4000_0000;
        let root = tempfile::tempdir().unwrap();
        let top = open_directory_path(&std::fs::canonicalize(root.path()).unwrap()).unwrap();
        let directory = create_directory(&top, OsStr::new("d")).unwrap();
        create_file(&directory, OsStr::new("f")).unwrap().write_all(b"x").unwrap();
        let file = open_file(&directory, OsStr::new("f")).unwrap();
        let kernel32_reopen = |target: &File, access: u32| -> io::Result<()> {
            // SAFETY: an open handle; the result is checked and closed by File.
            let reopened = unsafe {
                ReOpenFile(
                    handle(target),
                    access,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                )
            };
            if reopened == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            drop(unsafe { File::from_raw_handle(reopened as _) });
            Ok(())
        };
        let steps: Vec<(&str, io::Result<()>)> = vec![
            ("top: owner-only check", crate::is_owner_only_handle(&top).map(|_| ())),
            ("created directory: owner-only check", crate::is_owner_only_handle(&directory).and_then(|private| {
                if private { Ok(()) } else { Err(invalid("created directory is not owner-only")) }
            })),
            ("created file (read handle): owner-only check", crate::is_owner_only_handle(&file).and_then(|private| {
                if private { Ok(()) } else { Err(invalid("created file is not owner-only")) }
            })),
            ("directory: NT reopen for READ_CONTROL", reopen(&directory, READ_CONTROL).map(drop)),
            ("directory: NT reopen for write", reopen(&directory, GENERIC_WRITE).map(drop)),
            ("file: NT reopen for write", reopen(&file, GENERIC_WRITE).map(drop)),
            ("directory: ReOpenFile for READ_CONTROL", kernel32_reopen(&directory, READ_CONTROL)),
            ("directory: ReOpenFile for write", kernel32_reopen(&directory, GENERIC_WRITE)),
            ("file: ReOpenFile for write", kernel32_reopen(&file, GENERIC_WRITE)),
            ("directory: sync_handle", crate::sync_handle(&directory)),
            ("file (read handle): sync_handle", crate::sync_handle(&file)),
            ("top: sync_handle", crate::sync_handle(&top)),
        ];
        for (step, result) in &steps {
            eprintln!("{step}: {result:?}");
        }
        // File publication closes member handles before moving a tree,
        // because NTFS is expected to refuse renaming a directory with
        // anything inside it open. Report what this machine does.
        let moving = create_directory(&top, OsStr::new("moving")).unwrap();
        let child = create_file(&moving, OsStr::new("child")).unwrap();
        drop(moving);
        let open_child = rename_no_replace(&top, OsStr::new("moving"), &top, OsStr::new("moved"));
        eprintln!("directory rename with an open child (expected refused): {open_child:?}");
        drop(child);
        let source = if open_child.is_ok() { "moved" } else { "moving" };
        let closed = rename_no_replace(&top, OsStr::new(source), &top, OsStr::new("moved-again"));
        eprintln!("directory rename with no open child: {closed:?}");
        closed.unwrap();
        // The kernel32 reopen is only a fallback; everything else must work.
        let failed: Vec<_> = steps.iter().filter(|(step, result)| result.is_err() && !step.contains("ReOpenFile")).collect();
        assert!(failed.is_empty(), "failed steps: {failed:?}");
    }

    #[test]
    fn verbatim_paths_become_ordinary_only_when_win32_keeps_every_name() {
        use std::path::Path;
        let ordinary = |text: &str| crate::ordinary_path(PathBuf::from(text));
        assert_eq!(ordinary(r"\\?\C:\Users\me\file.txt"), Path::new(r"C:\Users\me\file.txt"));
        assert_eq!(ordinary(r"\\?\UNC\server\share\dir"), Path::new(r"\\server\share\dir"));
        assert_eq!(ordinary(r"C:\already"), Path::new(r"C:\already"));
        assert_eq!(ordinary("C:/mixed/separators\\here").as_os_str(), r"C:\mixed\separators\here");
        for kept in [r"\\?\C:\dir\trailing.", r"\\?\C:\dir\space ", r"\\?\C:\dir\CON", r"\\?\C:\dir\nul.txt", r"\\?\C:\dir\COM1", r"\\?\GLOBALROOT\x"] {
            assert_eq!(ordinary(kept), Path::new(kept), "{kept} must stay verbatim");
        }
        assert!(crate::same_path(Path::new(r"\\?\C:\Dir\x"), Path::new("c:/dir/x")));
        assert!(!crate::same_path(Path::new(r"C:\dir\x"), Path::new(r"C:\dir\y")));
    }
}
