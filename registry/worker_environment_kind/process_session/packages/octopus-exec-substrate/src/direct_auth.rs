use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::time::timeout;
use zeroize::Zeroizing;

const PROTOCOL: &str = "octopus.exec.direct-auth.v1";
const DOMAIN: &[u8] = b"octopus.exec.direct-auth.v1\0";
const NONCE_BYTES: usize = 32;
const TAG_BYTES: usize = 32;
const MAX_FRAME_BYTES: usize = 1024;
const MIN_SECRET_BYTES: usize = 32;
const MAX_SECRET_BYTES: usize = 4096;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct DirectExecServerAuth {
    secret: Arc<Zeroizing<Vec<u8>>>,
}

impl std::fmt::Debug for DirectExecServerAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirectExecServerAuth")
            .finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Challenge {
    protocol: String,
    challenge: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    protocol: String,
    proof: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Accepted {
    protocol: String,
    authenticated: bool,
}

impl DirectExecServerAuth {
    pub fn load_private_file(path: &Path) -> Result<Self, std::io::Error> {
        platform::load_private_file(path).map(Self::from_bytes)
    }

    pub fn load_beneath(root: &Path, relative: &str) -> Result<Self, std::io::Error> {
        platform::load_beneath(root, relative).map(Self::from_bytes)
    }

    fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            secret: Arc::new(Zeroizing::new(bytes)),
        }
    }

    pub fn identity(&self) -> [u8; 32] {
        blake3::derive_key(
            "octopus.exec.direct-auth.identity.v1",
            self.secret.as_slice(),
        )
    }

    fn prove(&self, challenge: &[u8]) -> Result<[u8; TAG_BYTES], String> {
        let mut mac = Hmac::<Sha256>::new_from_slice(self.secret.as_slice())
            .map_err(|_| "direct exec-server authentication failed".to_string())?;
        mac.update(DOMAIN);
        mac.update(challenge);
        Ok(mac.finalize().into_bytes().into())
    }

    fn verify(&self, challenge: &[u8], proof: &[u8]) -> Result<(), String> {
        let mut mac = Hmac::<Sha256>::new_from_slice(self.secret.as_slice())
            .map_err(|_| "direct exec-server authentication failed".to_string())?;
        mac.update(DOMAIN);
        mac.update(challenge);
        mac.verify_slice(proof)
            .map_err(|_| "direct exec-server authentication failed".to_string())
    }
}

pub(crate) async fn authenticate_tungstenite<S>(
    websocket: &mut tokio_tungstenite::WebSocketStream<S>,
    auth: &DirectExecServerAuth,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    timeout(HANDSHAKE_TIMEOUT, async {
        let challenge = websocket
            .next()
            .await
            .ok_or_else(auth_failed)?
            .map_err(|_| auth_failed())?;
        let challenge: Challenge = decode_tungstenite(challenge)?;
        if challenge.protocol != PROTOCOL {
            return Err(auth_failed());
        }
        let challenge_bytes = BASE64
            .decode(challenge.challenge)
            .map_err(|_| auth_failed())?;
        if challenge_bytes.len() != NONCE_BYTES {
            return Err(auth_failed());
        }
        let proof = Proof {
            protocol: PROTOCOL.to_string(),
            proof: BASE64.encode(auth.prove(&challenge_bytes)?),
        };
        let encoded = serde_json::to_vec(&proof).map_err(|_| auth_failed())?;
        websocket
            .send(tokio_tungstenite::tungstenite::Message::Binary(
                encoded.into(),
            ))
            .await
            .map_err(|_| auth_failed())?;
        let accepted = websocket
            .next()
            .await
            .ok_or_else(auth_failed)?
            .map_err(|_| auth_failed())?;
        let accepted: Accepted = decode_tungstenite(accepted)?;
        if accepted.protocol != PROTOCOL || !accepted.authenticated {
            return Err(auth_failed());
        }
        Ok(())
    })
    .await
    .map_err(|_| "direct exec-server authentication timed out".to_string())?
}

pub(crate) async fn authenticate_axum(
    websocket: &mut axum::extract::ws::WebSocket,
    auth: &DirectExecServerAuth,
) -> Result<(), String> {
    timeout(HANDSHAKE_TIMEOUT, async {
        let mut challenge_bytes = [0u8; NONCE_BYTES];
        challenge_bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        challenge_bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        let challenge = Challenge {
            protocol: PROTOCOL.to_string(),
            challenge: BASE64.encode(challenge_bytes),
        };
        let encoded = serde_json::to_vec(&challenge).map_err(|_| auth_failed())?;
        websocket
            .send(axum::extract::ws::Message::Binary(encoded.into()))
            .await
            .map_err(|_| auth_failed())?;
        let proof = websocket
            .next()
            .await
            .ok_or_else(auth_failed)?
            .map_err(|_| auth_failed())?;
        let proof: Proof = decode_axum(proof)?;
        if proof.protocol != PROTOCOL {
            return Err(auth_failed());
        }
        let proof = BASE64.decode(proof.proof).map_err(|_| auth_failed())?;
        if proof.len() != TAG_BYTES {
            return Err(auth_failed());
        }
        auth.verify(&challenge_bytes, &proof)?;
        let accepted = serde_json::to_vec(&Accepted {
            protocol: PROTOCOL.to_string(),
            authenticated: true,
        })
        .map_err(|_| auth_failed())?;
        websocket
            .send(axum::extract::ws::Message::Binary(accepted.into()))
            .await
            .map_err(|_| auth_failed())?;
        Ok(())
    })
    .await
    .map_err(|_| "direct exec-server authentication timed out".to_string())?
}

fn decode_tungstenite<T: for<'de> Deserialize<'de>>(
    message: tokio_tungstenite::tungstenite::Message,
) -> Result<T, String> {
    let bytes = match message {
        tokio_tungstenite::tungstenite::Message::Text(text) => text.as_bytes().to_vec(),
        tokio_tungstenite::tungstenite::Message::Binary(bytes) => bytes.to_vec(),
        _ => return Err(auth_failed()),
    };
    decode_frame(&bytes)
}

fn decode_axum<T: for<'de> Deserialize<'de>>(
    message: axum::extract::ws::Message,
) -> Result<T, String> {
    let bytes = match message {
        axum::extract::ws::Message::Text(text) => text.as_bytes().to_vec(),
        axum::extract::ws::Message::Binary(bytes) => bytes.to_vec(),
        _ => return Err(auth_failed()),
    };
    decode_frame(&bytes)
}

fn decode_frame<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, String> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME_BYTES {
        return Err(auth_failed());
    }
    serde_json::from_slice(bytes).map_err(|_| auth_failed())
}

fn auth_failed() -> String {
    "direct exec-server authentication failed".to_string()
}

#[cfg(unix)]
mod platform {
    use std::ffi::{CStr, CString, OsStr};
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Component, Path};

    use super::{MAX_SECRET_BYTES, MIN_SECRET_BYTES};

    pub(super) fn load_private_file(path: &Path) -> Result<Vec<u8>, std::io::Error> {
        if !path.is_absolute() {
            return Err(invalid(
                "direct exec-server secret handle must be an absolute path",
            ));
        }
        let parent = path
            .parent()
            .ok_or_else(|| invalid("direct exec-server secret handle has no parent"))?;
        let leaf = path
            .file_name()
            .ok_or_else(|| invalid("direct exec-server secret handle has no file name"))?;
        let directory = open_path_directory(parent)?;
        load_from_parent(&directory, &c_component(leaf)?)
    }

    pub(super) fn load_beneath(root: &Path, relative: &str) -> Result<Vec<u8>, std::io::Error> {
        let relative = Path::new(relative);
        if relative.is_absolute() {
            return Err(invalid(
                "direct exec-server secret handle must be relative to its private root",
            ));
        }
        let mut components = Vec::new();
        for component in relative.components() {
            match component {
                Component::Normal(value) if !value.as_bytes().is_empty() => components.push(value),
                _ => {
                    return Err(invalid(
                        "direct exec-server secret handle contains a non-normal component",
                    ));
                }
            }
        }
        let (leaf, parents) = components
            .split_last()
            .ok_or_else(|| invalid("direct exec-server secret handle is empty"))?;
        let mut directory = open_path_directory(root)?;
        for component in parents {
            directory = open_at(
                &directory,
                &c_component(component)?,
                libc::O_RDONLY | libc::O_DIRECTORY,
                0,
            )?;
        }
        load_from_parent(&directory, &c_component(leaf)?)
    }

    fn load_from_parent(parent: &OwnedFd, leaf: &CStr) -> Result<Vec<u8>, std::io::Error> {
        let file = open_at(parent, leaf, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
        let before = stat_fd(&file)?;
        validate_stat(&before)?;
        let mut file = std::fs::File::from(file);
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take((MAX_SECRET_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if !(MIN_SECRET_BYTES..=MAX_SECRET_BYTES).contains(&bytes.len()) {
            return Err(invalid(format!(
                "direct exec-server secret handle must contain {MIN_SECRET_BYTES}..={MAX_SECRET_BYTES} bytes"
            )));
        }
        let after = file.metadata()?;
        if before.st_dev as u64 != after.dev()
            || before.st_ino as u64 != after.ino()
            || before.st_size as u64 != after.len()
            || before.st_mode as u32 != after.mode()
        {
            return Err(invalid(
                "direct exec-server secret handle changed while it was read",
            ));
        }
        let reopened = open_at(parent, leaf, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
        let named = stat_fd(&reopened)?;
        if before.st_dev != named.st_dev || before.st_ino != named.st_ino {
            return Err(invalid(
                "direct exec-server secret handle inode changed while it was read",
            ));
        }
        Ok(bytes)
    }

    fn validate_stat(stat: &libc::stat) -> Result<(), std::io::Error> {
        if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(invalid(
                "direct exec-server secret handle is not a regular file",
            ));
        }
        if stat.st_mode & 0o777 != 0o600 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "direct exec-server secret handle must have mode 0600",
            ));
        }
        if stat.st_uid != unsafe { libc::geteuid() } {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "direct exec-server secret handle must be owned by the effective user",
            ));
        }
        Ok(())
    }

    fn open_path_directory(path: &Path) -> Result<OwnedFd, std::io::Error> {
        let path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| invalid("direct exec-server secret directory contains NUL"))?;
        let raw = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if raw < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(unsafe { OwnedFd::from_raw_fd(raw) })
        }
    }

    fn open_at(
        parent: &OwnedFd,
        name: &CStr,
        flags: i32,
        mode: libc::mode_t,
    ) -> Result<OwnedFd, std::io::Error> {
        let raw = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                mode as libc::c_uint,
            )
        };
        if raw < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(unsafe { OwnedFd::from_raw_fd(raw) })
        }
    }

    fn stat_fd(fd: &OwnedFd) -> Result<libc::stat, std::io::Error> {
        let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
        if unsafe { libc::fstat(fd.as_raw_fd(), &mut stat) } != 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(stat)
        }
    }

    fn c_component(value: &OsStr) -> Result<CString, std::io::Error> {
        CString::new(value.as_bytes())
            .map_err(|_| invalid("direct exec-server secret handle component contains NUL"))
    }

    fn invalid(message: impl Into<String>) -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;

    fn auth(value: u8) -> DirectExecServerAuth {
        DirectExecServerAuth::from_bytes(vec![value; MIN_SECRET_BYTES])
    }

    #[test]
    fn unauthenticated_frame_is_refused() {
        let frame = br#"{"protocol":"octopus.exec.direct-auth.v1"}"#;
        assert!(decode_frame::<Proof>(frame).is_err());
    }

    #[test]
    fn wrong_secret_is_refused() {
        let challenge = [7u8; NONCE_BYTES];
        let proof = auth(1).prove(&challenge).unwrap();
        assert!(auth(2).verify(&challenge, &proof).is_err());
    }

    #[test]
    fn replayed_proof_is_bound_to_one_challenge() {
        let first = [3u8; NONCE_BYTES];
        let second = [4u8; NONCE_BYTES];
        let auth = auth(9);
        let proof = auth.prove(&first).unwrap();
        auth.verify(&first, &proof).unwrap();
        assert!(auth.verify(&second, &proof).is_err());
    }

    #[test]
    fn private_handle_requires_regular_owned_mode_0600_without_symlink_following() {
        let root = tempfile::tempdir().unwrap();
        let secret = root.path().join("secret");
        std::fs::write(&secret, vec![5u8; MIN_SECRET_BYTES]).unwrap();
        let mut permissions = std::fs::metadata(&secret).unwrap().permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&secret, permissions).unwrap();
        DirectExecServerAuth::load_beneath(root.path(), "secret").unwrap();

        let link = root.path().join("link");
        symlink(&secret, &link).unwrap();
        assert!(DirectExecServerAuth::load_beneath(root.path(), "link").is_err());

        let mut permissions = std::fs::metadata(&secret).unwrap().permissions();
        permissions.set_mode(0o644);
        std::fs::set_permissions(&secret, permissions).unwrap();
        assert!(DirectExecServerAuth::load_beneath(root.path(), "secret").is_err());
    }
}

#[cfg(not(unix))]
mod platform {
    use std::path::Path;

    pub(super) fn load_private_file(_path: &Path) -> Result<Vec<u8>, std::io::Error> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "direct exec-server secret handles require a Unix host",
        ))
    }

    pub(super) fn load_beneath(_root: &Path, _relative: &str) -> Result<Vec<u8>, std::io::Error> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "direct exec-server secret handles require a Unix host",
        ))
    }
}
