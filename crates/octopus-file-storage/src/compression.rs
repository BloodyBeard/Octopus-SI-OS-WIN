use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use rustix::fs::{OFlags, XattrFlags};

const BLOCK_BYTES: usize = 65536;
const LZFSE: u32 = 0x801;

#[link(name = "compression")]
unsafe extern "C" {
    fn compression_encode_scratch_buffer_size(algorithm: u32) -> usize;
    fn compression_encode_buffer(
        destination: *mut u8,
        destination_size: usize,
        source: *const u8,
        source_size: usize,
        scratch: *mut u8,
        algorithm: u32,
    ) -> usize;
}

unsafe extern "C" {
    fn fchflags(fd: std::ffi::c_int, flags: u32) -> std::ffi::c_int;
}

pub(crate) struct FileCompression {
    allocation_bytes: usize,
}

impl FileCompression {
    pub(crate) fn open(directory: &Path) -> io::Result<Option<Self>> {
        let file = File::open(directory)?;
        let fs = rustix::fs::fstatfs(&file)?;
        let name: Vec<u8> = fs
            .f_fstypename
            .iter()
            .take_while(|c| **c != 0)
            .map(|c| *c as u8)
            .collect();
        if name != b"apfs" {
            return Ok(None);
        }
        let allocation_bytes = usize::try_from(fs.f_bsize)
            .ok()
            .filter(|size| size.is_power_of_two())
            .ok_or_else(|| io::Error::other("APFS reports an invalid allocation block size"))?;
        Ok(Some(Self { allocation_bytes }))
    }

    fn encode(&self, bytes: &[u8]) -> io::Result<Option<Vec<u8>>> {
        let raw_blocks = bytes.len().div_ceil(self.allocation_bytes);
        if raw_blocks < 3 {
            return Ok(None);
        }
        let count = bytes.len().div_ceil(BLOCK_BYTES);
        let table_bytes = count
            .checked_add(1)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| io::Error::other("compressed file offset table is too large"))?;
        if table_bytes >= bytes.len() || table_bytes > u32::MAX as usize {
            return Ok(None);
        }
        let mut fork = vec![0u8; table_bytes];
        fork[..4].copy_from_slice(&(table_bytes as u32).to_le_bytes());
        let scratch_bytes = unsafe { compression_encode_scratch_buffer_size(LZFSE) };
        let mut scratch = vec![0u8; scratch_bytes];
        let mut encoded = vec![0u8; BLOCK_BYTES + BLOCK_BYTES / 16 + 1024];
        for (index, chunk) in bytes.chunks(BLOCK_BYTES).enumerate() {
            let length = unsafe {
                compression_encode_buffer(
                    encoded.as_mut_ptr(),
                    encoded.len(),
                    chunk.as_ptr(),
                    chunk.len(),
                    scratch.as_mut_ptr(),
                    LZFSE,
                )
            };
            if length == 0 || length > encoded.len() {
                return Err(io::Error::other("LZFSE failed to encode an file block"));
            }
            let end = fork
                .len()
                .checked_add(length)
                .ok_or_else(|| io::Error::other("compressed file size overflow"))?;
            if end > u32::MAX as usize
                || end.div_ceil(self.allocation_bytes).saturating_add(1) >= raw_blocks
            {
                return Ok(None);
            }
            fork.extend_from_slice(&encoded[..length]);
            fork[(index + 1) * 4..(index + 2) * 4].copy_from_slice(&(end as u32).to_le_bytes());
        }
        Ok(Some(fork))
    }

    pub(crate) fn prepare(&self, temporary: &Path, bytes: &[u8]) -> io::Result<Option<File>> {
        let Some(encoded) = self.encode(bytes)? else {
            return Ok(None);
        };
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .custom_flags(OFlags::NOFOLLOW.bits() as i32)
            .open(temporary)?;
        let result = (|| {
            let mut fork = OpenOptions::new()
                .write(true)
                .create(true)
                .custom_flags(OFlags::NOFOLLOW.bits() as i32)
                .open(temporary.join("..namedfork/rsrc"))?;
            let main_identity = file.metadata()?;
            let fork_identity = fork.metadata()?;
            if !main_identity.is_file()
                || main_identity.nlink() != 1
                || main_identity.dev() != fork_identity.dev()
                || main_identity.ino() != fork_identity.ino()
            {
                return Err(io::Error::other(
                    "compressed file resource fork changed identity",
                ));
            }
            fork.write_all(&encoded)?;
            rustix::fs::fsync(&fork)?;
            let mut header = Vec::with_capacity(16);
            header.extend_from_slice(&0x636d7066u32.to_le_bytes());
            header.extend_from_slice(&12u32.to_le_bytes());
            header.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            rustix::fs::fsetxattr(&file, c"com.apple.decmpfs", &header, XattrFlags::CREATE)?;
            if unsafe { fchflags(file.as_raw_fd(), 0x20) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        })();
        if let Err(error) = result {
            drop(file);
            let _ = std::fs::remove_file(temporary);
            return Err(error);
        }
        Ok(Some(file))
    }
}
