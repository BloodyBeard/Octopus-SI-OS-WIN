#![cfg(target_os = "linux")]

use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};

const PROTOCOL: &str = "octopus.container.rooted-fs.v1";
const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
const RESOLVE_NO_SYMLINKS: u64 = 0x04;
const RESOLVE_BENEATH: u64 = 0x08;

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let operation = arguments
        .first()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "rooted helper operation is missing".to_string())?;
    match operation {
        "version" if arguments.len() == 1 => {
            println!("{PROTOCOL} {}", std::env::consts::ARCH);
            Ok(())
        }
        "read" => read_operation(&arguments[1..]),
        "write" => write_operation(&arguments[1..]),
        "list" => list_operation(&arguments[1..]),
        "exec-cwd" => exec_cwd_operation(&arguments[1..]),
        "self-test-symlink-swap" if arguments.len() == 1 => symlink_swap_self_test(),
        _ => Err("rooted helper operation or argument shape is invalid".to_string()),
    }
}

fn exec_cwd_operation(arguments: &[OsString]) -> Result<(), String> {
    if arguments.len() < 4 {
        return Err("rooted exec requires cwd, root count, roots and argv".to_string());
    }
    let requested = absolute_normal_path(&arguments[0])?;
    let root_count = parse_usize(&arguments[1], "root count")?;
    if root_count == 0 || arguments.len() < 2 + root_count + 1 {
        return Err("rooted exec root count does not match its arguments".to_string());
    }
    let roots = &arguments[2..2 + root_count];
    let command = &arguments[2 + root_count..];
    let (root, relative) = select_root(&requested, roots)?;
    let root = open_root(&root)?;
    let directory = open_beneath(
        &root,
        &relative,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
        RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS,
    )?;
    if unsafe { libc::fchdir(directory.as_raw_fd()) } != 0 {
        return Err(io_error(io::Error::last_os_error()));
    }
    let command = command
        .iter()
        .map(|value| {
            CString::new(value.as_bytes())
                .map_err(|_| "rooted exec argument contains null".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut pointers = command
        .iter()
        .map(|value| value.as_ptr())
        .collect::<Vec<_>>();
    pointers.push(std::ptr::null());
    unsafe {
        libc::execvp(command[0].as_ptr(), pointers.as_ptr());
    }
    Err(io_error(io::Error::last_os_error()))
}

fn read_operation(arguments: &[OsString]) -> Result<(), String> {
    if arguments.len() < 5 {
        return Err(
            "rooted read requires path, offset, maximum, partial flag and roots".to_string(),
        );
    }
    let requested = absolute_normal_path(&arguments[0])?;
    let offset = parse_u64(&arguments[1], "offset")?;
    let maximum = parse_u64(&arguments[2], "maximum")?;
    if maximum == 0 {
        return Err("rooted read maximum must be positive".to_string());
    }
    let allow_partial = match arguments[3].to_str() {
        Some("0") => false,
        Some("1") => true,
        _ => return Err("rooted read partial flag is invalid".to_string()),
    };
    let (root, relative) = select_root(&requested, &arguments[4..])?;
    let root = open_root(&root)?;
    let target = open_beneath(
        &root,
        &relative,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
        RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS,
    )?;
    let mut file = File::from(target);
    let metadata = file.metadata().map_err(io_error)?;
    if !metadata.file_type().is_file() {
        return Err("rooted read target is not a regular file".to_string());
    }
    let file_size = metadata.len();
    if offset > file_size {
        return Err("rooted read offset exceeds file size".to_string());
    }
    let remaining = file_size - offset;
    if remaining > maximum && !allow_partial {
        return Err("rooted read exceeds admitted maximum".to_string());
    }
    let count = remaining.min(maximum);
    file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
    let mut stdout = io::stdout().lock();
    writeln!(
        stdout,
        "{file_size}\t{count}\t{}",
        u8::from(remaining > count)
    )
    .map_err(io_error)?;
    let copied = io::copy(&mut file.take(count), &mut stdout).map_err(io_error)?;
    if copied != count {
        return Err("rooted read target changed before the admitted bytes were read".to_string());
    }
    Ok(())
}

fn write_operation(arguments: &[OsString]) -> Result<(), String> {
    if arguments.len() < 4 {
        return Err("rooted write requires path, maximum, parent flag and roots".to_string());
    }
    let requested = absolute_normal_path(&arguments[0])?;
    let maximum = parse_u64(&arguments[1], "maximum")?;
    if maximum == 0 {
        return Err("rooted write maximum must be positive".to_string());
    }
    let create_parents = match arguments[2].to_str() {
        Some("0") => false,
        Some("1") => true,
        _ => return Err("rooted write parent flag is invalid".to_string()),
    };
    let (root, relative) = select_root(&requested, &arguments[3..])?;
    let leaf = relative
        .file_name()
        .ok_or_else(|| "rooted write path must name a file below its root".to_string())?;
    let parent = relative.parent().unwrap_or_else(|| Path::new("."));
    let root = open_root(&root)?;
    let parent = open_parent(&root, parent, create_parents)?;
    let bytes = read_stdin_bounded(maximum)?;
    let leaf = c_component(leaf)?;
    let temporary = create_temporary(&parent)?;
    let temporary_name = temporary.0;
    let mut temporary_file = File::from(temporary.1);
    let result = (|| {
        temporary_file.write_all(&bytes).map_err(io_error)?;
        temporary_file.sync_all().map_err(io_error)?;
        if unsafe {
            libc::renameat(
                parent.as_raw_fd(),
                temporary_name.as_ptr(),
                parent.as_raw_fd(),
                leaf.as_ptr(),
            )
        } != 0
        {
            return Err(io_error(io::Error::last_os_error()));
        }
        let status = unsafe { libc::fsync(parent.as_raw_fd()) };
        if status != 0 {
            return Err(io_error(io::Error::last_os_error()));
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

fn open_parent(root: &OwnedFd, path: &Path, create: bool) -> Result<OwnedFd, String> {
    if !create {
        return open_beneath(
            root,
            path,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
            RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS,
        );
    }
    let duplicated = unsafe { libc::fcntl(root.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if duplicated < 0 {
        return Err(io_error(io::Error::last_os_error()));
    }
    let mut current = unsafe { OwnedFd::from_raw_fd(duplicated) };
    for component in path.components() {
        let Component::Normal(component) = component else {
            if component == Component::CurDir {
                continue;
            }
            return Err("rooted write parent contains a non-normal component".to_string());
        };
        let component = c_component(component)?;
        let next = open_beneath_io(
            &current,
            Path::new(OsStr::from_bytes(component.as_bytes())),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
            RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS,
        );
        current = match next {
            Ok(next) => next,
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
                if unsafe { libc::mkdirat(current.as_raw_fd(), component.as_ptr(), 0o700) } != 0 {
                    let mkdir_error = io::Error::last_os_error();
                    if mkdir_error.raw_os_error() != Some(libc::EEXIST) {
                        return Err(io_error(mkdir_error));
                    }
                }
                open_beneath_io(
                    &current,
                    Path::new(OsStr::from_bytes(component.as_bytes())),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                    0,
                    RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS,
                )
                .map_err(io_error)?
            }
            Err(error) => return Err(io_error(error)),
        };
    }
    Ok(current)
}

fn list_operation(arguments: &[OsString]) -> Result<(), String> {
    if arguments.len() < 3 {
        return Err("rooted list requires path, maximum and roots".to_string());
    }
    let requested = absolute_normal_path(&arguments[0])?;
    let maximum = parse_usize(&arguments[1], "maximum")?;
    if maximum == 0 {
        return Err("rooted list maximum must be positive".to_string());
    }
    let (root, relative) = select_root(&requested, &arguments[2..])?;
    let root = open_root(&root)?;
    let directory = open_beneath(
        &root,
        &relative,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
        RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS,
    )?;
    let mut entries = read_directory_entries(directory, maximum)?;
    let mut stdout = io::stdout().lock();
    if entries.len() > maximum {
        stdout.write_all(b"overflow\0\0").map_err(io_error)?;
        return Ok(());
    }
    entries.sort_by(|left, right| left.1.as_bytes().cmp(right.1.as_bytes()));
    for (kind, name) in entries {
        stdout.write_all(kind.as_bytes()).map_err(io_error)?;
        stdout.write_all(&[0]).map_err(io_error)?;
        stdout.write_all(name.as_bytes()).map_err(io_error)?;
        stdout.write_all(&[0]).map_err(io_error)?;
    }
    Ok(())
}

fn open_root(path: &Path) -> Result<OwnedFd, String> {
    let root = unsafe {
        libc::open(
            c_path(Path::new("/"))?.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if root < 0 {
        return Err(io_error(io::Error::last_os_error()));
    }
    let root = unsafe { OwnedFd::from_raw_fd(root) };
    let relative = path
        .strip_prefix("/")
        .map_err(|_| "rooted helper root is not absolute".to_string())?;
    open_beneath(
        &root,
        relative,
        libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
        RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS,
    )
}

fn open_beneath(
    root: &OwnedFd,
    relative: &Path,
    flags: i32,
    mode: u32,
    resolve: u64,
) -> Result<OwnedFd, String> {
    open_beneath_io(root, relative, flags, mode, resolve).map_err(io_error)
}

fn open_beneath_io(
    root: &OwnedFd,
    relative: &Path,
    flags: i32,
    mode: u32,
    resolve: u64,
) -> Result<OwnedFd, io::Error> {
    let path = if relative.as_os_str().is_empty() {
        CString::new(".").unwrap()
    } else {
        c_path(relative).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?
    };
    let how = OpenHow {
        flags: flags as u64,
        mode: mode as u64,
        resolve,
    };
    let result = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            root.as_raw_fd(),
            path.as_ptr(),
            &how as *const OpenHow,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(result as i32) })
    }
}

fn create_temporary(parent: &OwnedFd) -> Result<(CString, OwnedFd), String> {
    for attempt in 0..128_u32 {
        let name = CString::new(format!(".octopus-write-{}-{attempt}", unsafe {
            libc::getpid()
        }))
        .unwrap();
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd >= 0 {
            return Ok((name, unsafe { OwnedFd::from_raw_fd(fd) }));
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EEXIST) {
            return Err(io_error(error));
        }
    }
    Err("rooted write could not allocate a private temporary file".to_string())
}

fn read_directory_entries(
    directory: OwnedFd,
    maximum: usize,
) -> Result<Vec<(&'static str, OsString)>, String> {
    let raw = directory.into_raw_fd();
    let stream = unsafe { libc::fdopendir(raw) };
    if stream.is_null() {
        unsafe {
            libc::close(raw);
        }
        return Err(io_error(io::Error::last_os_error()));
    }
    let mut entries = Vec::new();
    loop {
        unsafe {
            *libc::__errno_location() = 0;
        }
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            let error = io::Error::last_os_error();
            unsafe {
                libc::closedir(stream);
            }
            if error.raw_os_error().unwrap_or(0) != 0 {
                return Err(io_error(error));
            }
            break;
        }
        let entry = unsafe { &*entry };
        let name = unsafe { CStr::from_ptr(entry.d_name.as_ptr()) }.to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        let kind = entry_kind(unsafe { libc::dirfd(stream) }, name, entry.d_type)?;
        entries.push((kind, OsString::from_vec(name.to_vec())));
        if entries.len() > maximum {
            unsafe {
                libc::closedir(stream);
            }
            break;
        }
    }
    Ok(entries)
}

fn entry_kind(directory: i32, name: &[u8], data_type: u8) -> Result<&'static str, String> {
    match data_type {
        libc::DT_REG => Ok("file"),
        libc::DT_DIR => Ok("directory"),
        libc::DT_LNK => Ok("symlink"),
        libc::DT_UNKNOWN => {
            let name =
                CString::new(name).map_err(|_| "directory entry contains null".to_string())?;
            let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
            if unsafe {
                libc::fstatat(
                    directory,
                    name.as_ptr(),
                    &mut stat,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
            {
                return Err(io_error(io::Error::last_os_error()));
            }
            match stat.st_mode & libc::S_IFMT {
                libc::S_IFREG => Ok("file"),
                libc::S_IFDIR => Ok("directory"),
                libc::S_IFLNK => Ok("symlink"),
                _ => Ok("other"),
            }
        }
        _ => Ok("other"),
    }
}

fn select_root(requested: &Path, roots: &[OsString]) -> Result<(PathBuf, PathBuf), String> {
    for root in roots {
        let root = absolute_normal_path(root)?;
        if let Ok(relative) = requested.strip_prefix(&root) {
            let relative = normal_relative_path(relative)?;
            return Ok((root, relative));
        }
    }
    Err("requested path is outside every formed root".to_string())
}

fn absolute_normal_path(value: &OsStr) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if !path.is_absolute() {
        return Err("rooted helper path must be absolute".to_string());
    }
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(value) if !value.as_bytes().is_empty() => normalized.push(value),
            _ => return Err("rooted helper path contains a non-normal component".to_string()),
        }
    }
    Ok(normalized)
}

fn normal_relative_path(path: &Path) -> Result<PathBuf, String> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) if !value.as_bytes().is_empty() => normalized.push(value),
            _ => {
                return Err(
                    "rooted helper relative path contains a non-normal component".to_string(),
                )
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        normalized.push(".");
    }
    Ok(normalized)
}

fn read_stdin_bounded(maximum: u64) -> Result<Vec<u8>, String> {
    let limit = maximum
        .checked_add(1)
        .ok_or_else(|| "rooted write maximum overflowed".to_string())?;
    let mut bytes = Vec::new();
    io::stdin()
        .lock()
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() as u64 > maximum {
        return Err("rooted write input exceeds admitted maximum".to_string());
    }
    Ok(bytes)
}

fn parse_u64(value: &OsStr, field: &str) -> Result<u64, String> {
    value
        .to_str()
        .ok_or_else(|| format!("rooted helper {field} is not UTF-8"))?
        .parse::<u64>()
        .map_err(|_| format!("rooted helper {field} is invalid"))
}

fn parse_usize(value: &OsStr, field: &str) -> Result<usize, String> {
    let value = parse_u64(value, field)?;
    usize::try_from(value).map_err(|_| format!("rooted helper {field} exceeds usize"))
}

fn c_component(value: &OsStr) -> Result<CString, String> {
    if value.as_bytes().is_empty() || value.as_bytes().contains(&b'/') {
        return Err("rooted helper leaf is not one path component".to_string());
    }
    CString::new(value.as_bytes()).map_err(|_| "rooted helper leaf contains null".to_string())
}

fn c_path(value: &Path) -> Result<CString, String> {
    CString::new(value.as_os_str().as_bytes())
        .map_err(|_| "rooted helper path contains null".to_string())
}

fn io_error(error: io::Error) -> String {
    format!("rooted helper system operation failed: {error}")
}

fn symlink_swap_self_test() -> Result<(), String> {
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let base = std::env::temp_dir().join(format!("octopus-rooted-fs-self-test-{}", unsafe {
        libc::getpid()
    }));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("root");
    let inside = root.join("inside");
    let outside = base.join("outside");
    std::fs::create_dir_all(&inside).map_err(io_error)?;
    std::fs::create_dir_all(&outside).map_err(io_error)?;
    std::fs::write(inside.join("value"), b"inside").map_err(io_error)?;
    std::fs::write(outside.join("value"), b"outside").map_err(io_error)?;
    symlink("inside", root.join("slot")).map_err(io_error)?;
    let stop = Arc::new(AtomicBool::new(false));
    let attacker_stop = stop.clone();
    let attacker_root = root.clone();
    let attacker = std::thread::spawn(move || {
        let next = attacker_root.join("slot.next");
        while !attacker_stop.load(Ordering::Acquire) {
            let _ = symlink("../outside", &next);
            let _ = std::fs::rename(&next, attacker_root.join("slot"));
            let _ = symlink("inside", &next);
            let _ = std::fs::rename(&next, attacker_root.join("slot"));
        }
    });
    let root_fd = open_root(&root)?;
    for _ in 0..20_000 {
        let target = open_beneath(
            &root_fd,
            Path::new("slot/value"),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
            RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS,
        );
        if let Ok(target) = target {
            let mut bytes = Vec::new();
            File::from(target)
                .read_to_end(&mut bytes)
                .map_err(io_error)?;
            if bytes != b"inside" {
                stop.store(true, Ordering::Release);
                let _ = attacker.join();
                return Err("rooted helper symlink swap escaped its held root".to_string());
            }
        }
    }
    stop.store(true, Ordering::Release);
    attacker
        .join()
        .map_err(|_| "rooted helper symlink swap attacker panicked".to_string())?;
    std::fs::remove_dir_all(&base).map_err(io_error)?;
    println!("{PROTOCOL} symlink-swap-safe");
    Ok(())
}
