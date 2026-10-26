//! Local stream sockets addressed by a filesystem path, on Unix and Windows.
//!
//! Unix uses Tokio's Unix domain sockets. Windows uses named pipes, because
//! Node (and so the Electron client) can reach only named pipes there. Each
//! socket path maps to the pipe [`pipe_name`] returns. The pipe's access list
//! admits only the current user and SYSTEM, and a client refuses a pipe served
//! by another user, which stands in for the Unix socket file's owner and mode.

use std::io::Result as IoResult;
use std::path::Path;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::io::ReadBuf;

pub async fn prepare_private_socket_directory(socket_dir: impl AsRef<Path>) -> IoResult<()> {
    platform::prepare_private_socket_directory(socket_dir.as_ref()).await
}

pub async fn is_stale_socket_path(socket_path: impl AsRef<Path>) -> IoResult<bool> {
    platform::is_stale_socket_path(socket_path.as_ref()).await
}

/// The Windows named pipe that stands for `socket_path`:
/// `\\.\pipe\octopus-` and the first 32 hex digits of the SHA-256 of the
/// path. The path is first written without a `\\?\` prefix, with `\`
/// separators and in lower case, since Windows paths ignore case. The desktop
/// app's `localSocket.cjs` computes the same name.
pub fn pipe_name(socket_path: &Path) -> String {
    use sha2::Digest;
    let text = socket_path.to_string_lossy().replace('/', "\\");
    let text = match text.strip_prefix(r"\\?\UNC\") {
        Some(rest) => format!(r"\\{rest}"),
        None => text.strip_prefix(r"\\?\").unwrap_or(&text).to_string(),
    };
    let digest = sha2::Sha256::digest(text.to_lowercase().as_bytes());
    let hex: String = digest[..16].iter().map(|byte| format!("{byte:02x}")).collect();
    format!(r"\\.\pipe\octopus-{hex}")
}

pub struct UnixListener {
    inner: platform::Listener,
}

impl UnixListener {
    pub async fn bind(socket_path: impl AsRef<Path>) -> IoResult<Self> {
        platform::bind_listener(socket_path.as_ref())
            .await
            .map(|inner| Self { inner })
    }

    pub async fn accept(&mut self) -> IoResult<UnixStream> {
        self.inner.accept().await.map(|inner| UnixStream { inner })
    }
}

pub struct UnixStream {
    inner: platform::Stream,
}

impl UnixStream {
    pub async fn connect(socket_path: impl AsRef<Path>) -> IoResult<Self> {
        platform::connect_stream(socket_path.as_ref())
            .await
            .map(|inner| Self { inner })
    }
}

impl AsyncRead for UnixStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<IoResult<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for UnixStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<IoResult<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<IoResult<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<IoResult<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

#[cfg(unix)]
mod platform {
    use std::io;
    use std::io::ErrorKind;
    use std::io::Result as IoResult;
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use tokio::fs;
    use tokio::net::UnixListener;
    use tokio::net::UnixStream;

    const SOCKET_DIR_MODE: u32 = 0o700;
    const SOCKET_DIR_PERMISSION_BITS: u32 = 0o777;

    pub(super) type Stream = UnixStream;

    pub(super) struct Listener(UnixListener);

    pub(super) async fn prepare_private_socket_directory(socket_dir: &Path) -> IoResult<()> {
        let mut dir_builder = fs::DirBuilder::new();
        dir_builder.mode(SOCKET_DIR_MODE);
        match dir_builder.create(socket_dir).await {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }

        let metadata = fs::symlink_metadata(socket_dir).await?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                ErrorKind::AlreadyExists,
                format!(
                    "socket directory path exists and is not a directory: {}",
                    socket_dir.display()
                ),
            ));
        }

        let permissions = metadata.permissions();
        if permissions.mode() & SOCKET_DIR_PERMISSION_BITS != SOCKET_DIR_MODE {
            fs::set_permissions(socket_dir, std::fs::Permissions::from_mode(SOCKET_DIR_MODE))
                .await?;
        }

        Ok(())
    }

    pub(super) async fn bind_listener(socket_path: &Path) -> IoResult<Listener> {
        UnixListener::bind(socket_path).map(Listener)
    }

    impl Listener {
        pub(super) async fn accept(&mut self) -> IoResult<Stream> {
            self.0.accept().await.map(|(stream, _addr)| stream)
        }
    }

    pub(super) async fn connect_stream(socket_path: &Path) -> IoResult<Stream> {
        UnixStream::connect(socket_path).await
    }

    pub(super) async fn is_stale_socket_path(socket_path: &Path) -> IoResult<bool> {
        Ok(fs::symlink_metadata(socket_path)
            .await?
            .file_type()
            .is_socket())
    }
}

#[cfg(windows)]
mod platform {
    use std::io;
    use std::io::ErrorKind;
    use std::io::Result as IoResult;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;
    use std::pin::Pin;
    use std::task::Context;
    use std::task::Poll;
    use std::time::Duration;
    use std::time::Instant;

    use octopus_private_fs::windows::OwnerOnlySecurity;
    use octopus_private_fs::windows::process_is_current_user;
    use tokio::io::AsyncRead;
    use tokio::io::AsyncWrite;
    use tokio::io::ReadBuf;
    use tokio::net::windows::named_pipe::ClientOptions;
    use tokio::net::windows::named_pipe::NamedPipeClient;
    use tokio::net::windows::named_pipe::NamedPipeServer;
    use tokio::net::windows::named_pipe::ServerOptions;
    use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
    use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;
    use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;

    use super::pipe_name;

    /// How long `connect` waits for a busy pipe (all instances taken) to
    /// offer a free instance, matching a full Unix listen backlog.
    const BUSY_PIPE_WAIT: Duration = Duration::from_secs(5);

    pub(super) async fn prepare_private_socket_directory(socket_dir: &Path) -> IoResult<()> {
        // Pipes live in the kernel's pipe namespace, not in this directory, and
        // carry their own owner-only access list. The directory still holds
        // the lock files that sit next to sockets, so a new one is private.
        if let Some(parent) = socket_dir.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let socket_dir = socket_dir.to_path_buf();
        tokio::task::spawn_blocking(move || {
            match octopus_private_fs::create_private_dir(&socket_dir) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                    if std::fs::symlink_metadata(&socket_dir)?.is_dir() {
                        Ok(())
                    } else {
                        Err(io::Error::new(
                            ErrorKind::AlreadyExists,
                            format!(
                                "socket directory path exists and is not a directory: {}",
                                socket_dir.display()
                            ),
                        ))
                    }
                }
                Err(err) => Err(err),
            }
        })
        .await
        .map_err(|err| io::Error::other(format!("socket directory task failed: {err}")))?
    }

    pub(super) struct Listener {
        name: String,
        security: OwnerOnlySecurity,
        next: NamedPipeServer,
    }

    fn create_instance(
        name: &str,
        security: &OwnerOnlySecurity,
        first: bool,
    ) -> IoResult<NamedPipeServer> {
        let mut options = ServerOptions::new();
        options.first_pipe_instance(first).reject_remote_clients(true);
        // SAFETY: `security` outlives the call and points at valid
        // SECURITY_ATTRIBUTES with an owner-only descriptor.
        let created = unsafe { options.create_with_security_attributes_raw(name, security.as_ptr()) };
        created.map_err(|err| {
            if first && err.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
                // Another server already owns this name: Unix reports a bound
                // socket path the same way.
                io::Error::new(ErrorKind::AddrInUse, format!("local socket {name} is already in use"))
            } else {
                err
            }
        })
    }

    pub(super) async fn bind_listener(socket_path: &Path) -> IoResult<Listener> {
        let name = pipe_name(socket_path);
        let security = OwnerOnlySecurity::new()?;
        let next = create_instance(&name, &security, true)?;
        Ok(Listener {
            name,
            security,
            next,
        })
    }

    impl Listener {
        pub(super) async fn accept(&mut self) -> IoResult<Stream> {
            self.next.connect().await?;
            let next = create_instance(&self.name, &self.security, false)?;
            Ok(Stream::Server(std::mem::replace(&mut self.next, next)))
        }
    }

    pub(super) enum Stream {
        Server(NamedPipeServer),
        Client(NamedPipeClient),
    }

    pub(super) async fn connect_stream(socket_path: &Path) -> IoResult<Stream> {
        let name = pipe_name(socket_path);
        let deadline = Instant::now() + BUSY_PIPE_WAIT;
        let client = loop {
            match ClientOptions::new().open(&name) {
                Ok(client) => break client,
                Err(err)
                    if err.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
                        && Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(err) => return Err(err),
            }
        };
        // A Unix socket file is owned by whoever bound it, and callers check
        // that owner. A pipe has no file, so check the serving process.
        let mut server_pid = 0;
        // SAFETY: the handle is a connected pipe client; `server_pid` is writable.
        if unsafe { GetNamedPipeServerProcessId(client.as_raw_handle(), &mut server_pid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if !process_is_current_user(server_pid)? {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                format!("local socket {name} is served by another user"),
            ));
        }
        Ok(Stream::Client(client))
    }

    pub(super) async fn is_stale_socket_path(_socket_path: &Path) -> IoResult<bool> {
        // A pipe disappears with the last handle to it, so nothing stale can
        // remain at a socket path.
        Ok(false)
    }

    impl AsyncRead for Stream {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<IoResult<()>> {
            match self.get_mut() {
                Stream::Server(pipe) => Pin::new(pipe).poll_read(cx, buf),
                Stream::Client(pipe) => Pin::new(pipe).poll_read(cx, buf),
            }
        }
    }

    impl AsyncWrite for Stream {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<IoResult<usize>> {
            match self.get_mut() {
                Stream::Server(pipe) => Pin::new(pipe).poll_write(cx, buf),
                Stream::Client(pipe) => Pin::new(pipe).poll_write(cx, buf),
            }
        }

        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<IoResult<()>> {
            match self.get_mut() {
                Stream::Server(pipe) => Pin::new(pipe).poll_flush(cx),
                Stream::Client(pipe) => Pin::new(pipe).poll_flush(cx),
            }
        }

        fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<IoResult<()>> {
            match self.get_mut() {
                Stream::Server(pipe) => Pin::new(pipe).poll_shutdown(cx),
                Stream::Client(pipe) => Pin::new(pipe).poll_shutdown(cx),
            }
        }
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;
