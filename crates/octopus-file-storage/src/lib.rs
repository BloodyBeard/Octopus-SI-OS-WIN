use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::path::Path;

#[cfg(target_vendor = "apple")]
mod compression;

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
