use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::path::Path;

#[cfg(target_vendor = "apple")]
mod compression;

pub mod publication;

pub fn sync_ordered(file: &File) -> io::Result<()> {
    #[cfg(target_vendor = "apple")]
    {
        use std::os::fd::AsRawFd;
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_BARRIERFSYNC) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    // Windows needs write access to flush; see `octopus_private_fs::sync_handle`.
    #[cfg(windows)]
    {
        octopus_private_fs::sync_handle(file)
    }
    #[cfg(not(any(target_vendor = "apple", windows)))]
    {
        file.sync_all()
    }
}

/// Makes the entries of a directory (new names, renames, removals) durable
/// with an ordering barrier, like `sync_ordered` for files.
pub fn sync_directory_ordered(path: &Path) -> io::Result<()> {
    // Windows cannot open a directory with `File::open`; see
    // `octopus_private_fs::sync_directory`.
    #[cfg(windows)]
    return octopus_private_fs::sync_directory(path);
    #[cfg(not(windows))]
    sync_ordered(&File::open(path)?)
}

pub fn write_new(path: &Path, bytes: &[u8]) -> io::Result<File> {
    #[cfg(target_vendor = "apple")]
    let prepared = compression::FileCompression::open(
        path.parent()
            .ok_or_else(|| io::Error::other("new file has no parent"))?,
    )?
    .map(|compression| compression.prepare(path, bytes))
    .transpose()?
    .flatten();
    #[cfg(not(target_vendor = "apple"))]
    let prepared: Option<File> = None;
    let mut file = match prepared {
        Some(file) => file,
        None => {
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(path)?;
            if let Err(error) = file.write_all(bytes) {
                drop(file);
                let _ = std::fs::remove_file(path);
                return Err(error);
            }
            file
        }
    };
    let verified = (|| {
        file.rewind()?;
        let mut buffer = [0u8; 65536];
        for chunk in bytes.chunks(buffer.len()) {
            file.read_exact(&mut buffer[..chunk.len()])?;
            if buffer[..chunk.len()] != *chunk {
                return Err(io::Error::other("new file failed exact byte verification"));
            }
        }
        if file.read(&mut buffer[..1])? != 0 {
            return Err(io::Error::other("new file contains additional bytes"));
        }
        Ok(())
    })();
    if let Err(error) = verified {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(file)
}

#[cfg(test)]
mod directory_sync_tests {
    #[test]
    fn a_directory_with_a_new_entry_syncs() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("entry"), b"bytes").unwrap();
        super::sync_directory_ordered(root.path()).unwrap();
    }
}
