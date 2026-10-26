use std::collections::BTreeMap;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub const CONNECTION_FILE: &str = "desktop-host/connection.json";
const MAX_BYTES: u64 = 4096;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PrivateConnection {
    schema_version: u32,
    pub interface: String,
    pub host_generation: String,
    pub action_url: String,
    pub viewer_url: String,
    pub token: String,
}

impl PrivateConnection {
    pub fn read(path: &Path, interface: &str) -> Result<Self, String> {
        let bytes = read_private(path)?;
        let value: Self = serde_json::from_slice(&bytes).map_err(|_| "desktop private connection is malformed")?;
        if value.schema_version != 1 || value.interface != interface
            || !valid_hex(&value.host_generation, 32) || !valid_hex(&value.token, 64) {
            return Err("desktop private connection has invalid identity or credential shape".into());
        }
        let action = url::Url::parse(&value.action_url).map_err(|_| "desktop private action URL is invalid")?;
        let viewer = url::Url::parse(&value.viewer_url).map_err(|_| "desktop private viewer URL is invalid")?;
        for (url, route) in [(&action, "/v1/action"), (&viewer, "/view")] {
            if url.scheme() != "http" || url.host_str() != Some("127.0.0.1")
                || !url.port().is_some_and(|port| port > 0) || url.path() != route
                || !url.username().is_empty() || url.password().is_some()
                || url.query().is_some() || url.fragment().is_some() {
                return Err("desktop private connection must name its exact loopback endpoint".into());
            }
        }
        if action.origin() != viewer.origin() { return Err("desktop private endpoint origins disagree".into()); }
        Ok(value)
    }
}

pub(crate) struct HostPublication {
    path: PathBuf,
    _lock: std::fs::File,
    pub connection: PrivateConnection,
}

impl HostPublication {
    #[cfg(unix)]
    pub fn prepare(private: &Path, interface: &str) -> Result<Self, String> {
        validate_directory(private)?;
        if private.file_name().and_then(|part| part.to_str()) != Some(interface) {
            return Err("desktop host private directory does not identify its exact interface".into());
        }
        let directory = private.join("desktop-host");
        match std::fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => {},
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {},
            Err(_) => return Err("desktop host private directory cannot be created".into()),
        }
        validate_directory(&directory)?;
        let lock = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
            .mode(0o600).custom_flags(libc::O_NOFOLLOW).open(directory.join("host.lock"))
            .map_err(|_| "desktop host private lock cannot be opened")?;
        validate_file_metadata(&lock.metadata().map_err(|_| "desktop host lock metadata unavailable")?)?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("desktop native host already owns this exact interface".into());
        }
        let path = private.join(CONNECTION_FILE);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => { PrivateConnection::read(&path, interface)?; },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(_) => return Err("desktop private connection cannot be inspected".into()),
        }
        Ok(Self {
            path,
            _lock: lock,
            connection: PrivateConnection {
                schema_version: 1,
                interface: interface.into(),
                host_generation: random_hex(16)?,
                action_url: String::new(),
                viewer_url: String::new(),
                token: random_hex(32)?,
            },
        })
    }

    #[cfg(not(unix))]
    #[allow(unused_variables, unused_mut)]
    pub fn prepare(private: &Path, interface: &str) -> Result<Self, String> {
        Err("desktop control requires macOS".into())
    }

    #[cfg(unix)]
    pub fn publish(&mut self, address: std::net::SocketAddr) -> Result<(), String> {
        if address.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST) || address.port() == 0 {
            return Err("desktop native host did not bind its declared loopback port".into());
        }
        self.connection.action_url = format!("http://{address}/v1/action");
        self.connection.viewer_url = format!("http://{address}/view");
        let bytes = serde_json::to_vec(&self.connection).map_err(|_| "desktop private connection cannot encode")?;
        if bytes.len() as u64 > MAX_BYTES { return Err("desktop private connection exceeds its bound".into()); }
        let directory = self.path.parent().ok_or("desktop private connection has no parent")?;
        let temporary = directory.join(format!(".connection-{}", self.connection.host_generation));
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
            .custom_flags(libc::O_NOFOLLOW).open(&temporary).map_err(|_| "desktop private publication cannot open")?;
        let result = (|| {
            file.write_all(&bytes).map_err(|_| "desktop private publication cannot write")?;
            file.sync_all().map_err(|_| "desktop private publication cannot sync")?;
            std::fs::rename(&temporary, &self.path).map_err(|_| "desktop private publication cannot commit")?;
            std::fs::File::open(directory).and_then(|file| file.sync_all()).map_err(|_| "desktop private directory cannot sync")?;
            Ok(())
        })();
        if result.is_err() { let _ = std::fs::remove_file(&temporary); }
        result
    }

    #[cfg(not(unix))]
    #[allow(unused_variables, unused_mut)]
    pub fn publish(&mut self, address: std::net::SocketAddr) -> Result<(), String> {
        Err("desktop control requires macOS".into())
    }
}

impl Drop for HostPublication {
    fn drop(&mut self) {
        if PrivateConnection::read(&self.path, &self.connection.interface)
            .is_ok_and(|value| value.host_generation == self.connection.host_generation) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub fn prepare_private_desktop_host(roots: &BTreeMap<String, PathBuf>) -> Result<BTreeMap<String, PathBuf>, String> {
    let private = roots.get("activation_secrets_home").ok_or("desktop door has no host-formed private interface root")?;
    validate_directory(private)?;
    let interface = private.file_name().and_then(|part| part.to_str()).ok_or("desktop private root has no exact interface")?;
    let interfaces = roots.get("registry_interface_root").ok_or("desktop door has no declared interface registry root")?;
    let config = crate::config::DesktopConfig::read(&interfaces.join(interface).join(crate::config::CONFIG_FILE))?;
    if config.host.is_none() { return Ok(roots.clone()); }
    let path = private.join(CONNECTION_FILE);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match std::fs::symlink_metadata(&path) {
            Ok(_) => { PrivateConnection::read(&path, interface)?; return Ok(roots.clone()); },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if Instant::now() >= deadline { return Err("desktop native host did not publish rendezvous within 30 seconds".into()); }
                std::thread::sleep(Duration::from_millis(25));
            },
            Err(_) => return Err("desktop private connection cannot be inspected".into()),
        }
    }
}

#[cfg(unix)]
pub(crate) fn validate_directory(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| "desktop private directory is unavailable")?;
    if !path.is_absolute() || !metadata.is_dir() || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o700
        || std::fs::canonicalize(path).map_err(|_| "desktop private directory cannot resolve")? != path {
        return Err("desktop private directory must be canonical, owner-only and mode 0700".into());
    }
    Ok(())
}

#[cfg(not(unix))]
#[allow(unused_variables, unused_mut)]
pub(crate) fn validate_directory(path: &Path) -> Result<(), String> {
    Err("desktop control requires macOS".into())
}

#[cfg(unix)]
fn validate_file_metadata(metadata: &std::fs::Metadata) -> Result<(), String> {
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600 || metadata.nlink() != 1 || metadata.len() > MAX_BYTES {
        return Err("desktop private file must be a bounded owner-only single-link mode 0600 file".into());
    }
    Ok(())
}

#[cfg(not(unix))]
#[allow(unused_variables, unused_mut)]
fn validate_file_metadata(metadata: &std::fs::Metadata) -> Result<(), String> {
    Err("desktop control requires macOS".into())
}

#[cfg(unix)]
fn read_private(path: &Path) -> Result<Vec<u8>, String> {
    validate_directory(path.parent().ok_or("desktop private file has no parent")?)?;
    let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW)
        .open(path).map_err(|_| "desktop private file cannot open")?;
    validate_file_metadata(&file.metadata().map_err(|_| "desktop private file metadata unavailable")?)?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes).map_err(|_| "desktop private file cannot read")?;
    if bytes.len() as u64 > MAX_BYTES { return Err("desktop private file exceeds its bound".into()); }
    Ok(bytes)
}

#[cfg(not(unix))]
#[allow(unused_variables, unused_mut)]
fn read_private(path: &Path) -> Result<Vec<u8>, String> {
    Err("desktop control requires macOS".into())
}

fn random_hex(count: usize) -> Result<String, String> {
    let mut bytes = vec![0u8; count];
    getrandom::fill(&mut bytes).map_err(|_| "desktop native credential entropy unavailable")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn valid_hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
