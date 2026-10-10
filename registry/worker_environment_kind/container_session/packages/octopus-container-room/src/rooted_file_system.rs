use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const AARCH64: &[u8] = include_bytes!("../assets/rooted-fs-aarch64-linux-musl");
const X86_64: &[u8] = include_bytes!("../assets/rooted-fs-x86_64-linux-musl");
const PROTOCOL: &str = "octopus.container.rooted-fs.v1";

pub(crate) struct HelperAsset {
    pub(crate) architecture: &'static str,
    pub(crate) bytes: &'static [u8],
    pub(crate) digest: String,
}

pub(crate) struct ReadResult<'a> {
    pub(crate) file_size: u64,
    pub(crate) declared_bytes: u64,
    pub(crate) truncated: bool,
    pub(crate) content: &'a [u8],
}

pub(crate) fn asset_for_architecture(value: &str) -> Result<HelperAsset, String> {
    let (architecture, bytes) = match value.trim() {
        "aarch64" | "arm64" => ("aarch64", AARCH64),
        "x86_64" | "amd64" => ("x86_64", X86_64),
        other => {
            return Err(format!(
                "container architecture '{other}' has no rooted helper"
            ))
        }
    };
    Ok(HelperAsset {
        architecture,
        bytes,
        digest: blake3::hash(bytes).to_hex().to_string(),
    })
}

pub(crate) fn remote_path(asset: &HelperAsset) -> String {
    format!("/run/octopus/rooted-fs-{}", &asset.digest[..24])
}

pub(crate) fn materialize(root: &Path, asset: &HelperAsset) -> Result<PathBuf, String> {
    let directory = root.join("container-rooted-fs").join(&asset.digest);
    std::fs::create_dir_all(&directory).map_err(|error| {
        format!(
            "container rooted helper directory '{}' cannot be created: {error}",
            directory.display()
        )
    })?;
    let path = directory.join(asset.architecture);
    if path.exists() {
        verify_materialized(&path, asset)?;
        #[cfg(unix)]
        {
            let mut permissions = std::fs::metadata(&path)
                .map_err(|error| format!("container rooted helper cannot be inspected: {error}"))?
                .permissions();
            permissions.set_mode(0o555);
            std::fs::set_permissions(&path, permissions).map_err(|error| {
                format!("container rooted helper permissions cannot be applied: {error}")
            })?;
        }
        return Ok(path);
    }
    let temporary = directory.join(format!(
        ".{}.{}.tmp",
        asset.architecture,
        std::process::id()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o555);
    let mut file = options.open(&temporary).map_err(|error| {
        format!(
            "container rooted helper temporary '{}' cannot be created: {error}",
            temporary.display()
        )
    })?;
    let result = (|| {
        file.write_all(asset.bytes).map_err(|error| {
            format!(
                "container rooted helper temporary '{}' cannot be written: {error}",
                temporary.display()
            )
        })?;
        file.sync_all().map_err(|error| {
            format!(
                "container rooted helper temporary '{}' cannot be synchronized: {error}",
                temporary.display()
            )
        })?;
        drop(file);
        std::fs::rename(&temporary, &path).map_err(|error| {
            format!(
                "container rooted helper '{}' cannot be installed atomically: {error}",
                path.display()
            )
        })?;
        #[cfg(unix)]
        {
            let mut permissions = std::fs::metadata(&path)
                .map_err(|error| format!("container rooted helper cannot be inspected: {error}"))?
                .permissions();
            permissions.set_mode(0o555);
            std::fs::set_permissions(&path, permissions).map_err(|error| {
                format!("container rooted helper permissions cannot be applied: {error}")
            })?;
        }
        verify_materialized(&path, asset)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map(|_| path)
}

pub(crate) fn validate_version(output: &[u8], asset: &HelperAsset) -> Result<(), String> {
    let value = std::str::from_utf8(output)
        .map_err(|_| "container rooted helper version is not UTF-8".to_string())?
        .trim();
    let expected = format!("{PROTOCOL} {}", asset.architecture);
    if value != expected {
        return Err(format!(
            "container rooted helper reported '{value}', expected '{expected}'"
        ));
    }
    Ok(())
}

pub(crate) fn parse_read_result(bytes: &[u8]) -> Result<ReadResult<'_>, String> {
    let newline = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| "container rooted read omitted its framing header".to_string())?;
    let header = std::str::from_utf8(&bytes[..newline])
        .map_err(|_| "container rooted read header is not UTF-8".to_string())?;
    let mut fields = header.split('\t');
    let file_size = parse_header_integer(fields.next(), "file_size")?;
    let declared_bytes = parse_header_integer(fields.next(), "bytes_read")?;
    let truncated = match fields.next() {
        Some("0") => false,
        Some("1") => true,
        _ => return Err("container rooted read header has invalid truncated flag".to_string()),
    };
    if fields.next().is_some() {
        return Err("container rooted read header has extra fields".to_string());
    }
    let content = &bytes[newline + 1..];
    if content.len() as u64 != declared_bytes {
        return Err("container rooted read body length disagrees with its header".to_string());
    }
    Ok(ReadResult {
        file_size,
        declared_bytes,
        truncated,
        content,
    })
}

fn parse_header_integer(value: Option<&str>, field: &str) -> Result<u64, String> {
    value
        .ok_or_else(|| format!("container rooted read header omitted {field}"))?
        .parse::<u64>()
        .map_err(|_| format!("container rooted read header has invalid {field}"))
}

fn verify_materialized(path: &Path, asset: &HelperAsset) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        format!(
            "container rooted helper '{}' cannot be inspected: {error}",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "container rooted helper '{}' is not a regular file",
            path.display()
        ));
    }
    let bytes = std::fs::read(path).map_err(|error| {
        format!(
            "container rooted helper '{}' cannot be read: {error}",
            path.display()
        )
    })?;
    if blake3::hash(&bytes).to_hex().as_str() != asset.digest {
        return Err(format!(
            "container rooted helper '{}' has an unexpected digest",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_frame_keeps_binary_body_separate() {
        let parsed = parse_read_result(b"9\t4\t1\n\0\xffab").unwrap();
        assert_eq!(parsed.file_size, 9);
        assert_eq!(parsed.declared_bytes, 4);
        assert!(parsed.truncated);
        assert_eq!(parsed.content, b"\0\xffab");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn embedded_helper_refuses_symlink_swap_escape() {
        let root = tempfile::tempdir().unwrap();
        let asset = asset_for_architecture(std::env::consts::ARCH).unwrap();
        let helper = materialize(root.path(), &asset).unwrap();
        let status = std::process::Command::new(helper)
            .arg("self-test-symlink-swap")
            .status()
            .unwrap();
        assert!(status.success());
    }
}
