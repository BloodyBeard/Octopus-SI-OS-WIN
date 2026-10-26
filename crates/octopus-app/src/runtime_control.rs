use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

const SOCKET_FILE: &str = "control.sock";
const MAX_FRAME_BYTES: u64 = 4096;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeControlRequest {
    Status,
    Stop,
    Restart,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeControlStatus {
    pub pid: u32,
    pub phase: String,
    pub started_at_unix_ms: u64,
    pub stopping: Option<RuntimeControlRequest>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestFrame {
    request: RuntimeControlRequest,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "outcome", rename_all = "snake_case")]
enum ResponseFrame {
    Status(RuntimeControlStatus),
    Refused { error: String },
}

#[cfg(not(windows))]
pub fn socket_path(octopus_home: &Path) -> PathBuf {
    octopus_home
        .join("state")
        .join("runtime-instance")
        .join(SOCKET_FILE)
}

/// On Windows the control channel is a named pipe. Its name is derived from the
/// Octopus home so that separate homes never answer for each other.
#[cfg(windows)]
pub fn socket_path(octopus_home: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    let home = std::path::absolute(octopus_home).unwrap_or_else(|_| octopus_home.to_path_buf());
    let digest = Sha256::digest(home.to_string_lossy().to_lowercase().as_bytes());
    let suffix: String = digest[..12].iter().map(|byte| format!("{byte:02x}")).collect();
    PathBuf::from(format!(r"\\.\pipe\octopus-runtime-control-{suffix}"))
}

pub struct RuntimeControl {
    path: PathBuf,
    #[cfg(unix)]
    listener: tokio::net::UnixListener,
    #[cfg(windows)]
    listener: tokio::net::windows::named_pipe::NamedPipeServer,
    stopping: Arc<std::sync::Mutex<Option<RuntimeControlRequest>>>,
}

impl RuntimeControl {
    #[cfg(windows)]
    pub fn bind(octopus_home: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let path = socket_path(octopus_home);
        // The default pipe DACL grants write access only to the creating owner,
        // LocalSystem, and administrators, which matches the Unix 0600 socket.
        let listener = tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(&path)
            .map_err(|error| {
                format!(
                    "runtime control pipe '{}' cannot be created: {error}",
                    path.display()
                )
            })?;
        Ok(Self {
            path,
            listener,
            stopping: Arc::new(std::sync::Mutex::new(None)),
        })
    }

    #[cfg(unix)]
    pub fn bind(octopus_home: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let path = socket_path(octopus_home);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => std::fs::remove_file(&path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let listener = tokio::net::UnixListener::bind(&path).map_err(|error| {
            format!(
                "runtime control socket '{}' cannot be bound: {error}",
                path.display()
            )
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self {
            path,
            listener,
            stopping: Arc::new(std::sync::Mutex::new(None)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(unix)]
    async fn accept(&mut self) -> std::io::Result<Connection> {
        let (stream, _) = self.listener.accept().await?;
        let refusal = match stream.peer_cred() {
            Err(error) => Some(format!("control peer cannot be identified: {error}")),
            Ok(peer) if peer.uid() != current_uid() => Some(format!(
                "control peer uid {} is not the runtime owner",
                peer.uid()
            )),
            Ok(_) => None,
        };
        Ok(Connection { stream, refusal })
    }

    #[cfg(windows)]
    async fn accept(&mut self) -> std::io::Result<Connection> {
        self.listener.connect().await?;
        let next = tokio::net::windows::named_pipe::ServerOptions::new()
            .reject_remote_clients(true)
            .create(&self.path)?;
        Ok(Connection {
            stream: std::mem::replace(&mut self.listener, next),
            refusal: None,
        })
    }

    pub async fn serve(
        mut self,
        octopus_home: PathBuf,
        cancel: CancellationToken,
        restart: Arc<AtomicBool>,
    ) {
        loop {
            let stream = match self.accept().await {
                Ok(accepted) => accepted,
                Err(error) => {
                    eprintln!(
                        "octopus: runtime control socket '{}' stopped accepting: {error}",
                        self.path.display()
                    );
                    return;
                }
            };
            let home = octopus_home.clone();
            let cancel = cancel.clone();
            let restart = restart.clone();
            let stopping = self.stopping.clone();
            tokio::spawn(async move {
                if let Err(error) = tokio::time::timeout(
                    CONNECTION_TIMEOUT,
                    answer(stream, &home, &cancel, &restart, &stopping),
                )
                .await
                .map_err(|_| "control connection timed out".to_string())
                .and_then(|result| result)
                {
                    eprintln!("octopus: runtime control request failed: {error}");
                }
            });
        }
    }
}

#[cfg(unix)]
type ControlStream = tokio::net::UnixStream;
#[cfg(windows)]
type ControlStream = tokio::net::windows::named_pipe::NamedPipeServer;

struct Connection {
    stream: ControlStream,
    refusal: Option<String>,
}

async fn answer(
    connection: Connection,
    octopus_home: &Path,
    cancel: &CancellationToken,
    restart: &AtomicBool,
    stopping: &std::sync::Mutex<Option<RuntimeControlRequest>>,
) -> Result<(), String> {
    let Connection { stream, refusal } = connection;
    let (reader, mut writer) = tokio::io::split(stream);
    let response = if let Some(error) = refusal {
        ResponseFrame::Refused { error }
    } else {
        let mut line = String::new();
        tokio::io::BufReader::new(tokio::io::AsyncReadExt::take(reader, MAX_FRAME_BYTES))
            .read_line(&mut line)
            .await
            .map_err(|error| format!("control request cannot be read: {error}"))?;
        match serde_json::from_str::<RequestFrame>(&line) {
            Err(error) => ResponseFrame::Refused {
                error: format!("control request is not a strict request frame: {error}"),
            },
            Ok(frame) => {
                let (accepted, current) = {
                    let mut state = stopping
                        .lock()
                        .map_err(|_| "control state is poisoned".to_string())?;
                    let accepted = match (frame.request, *state) {
                        (RuntimeControlRequest::Status, _) | (_, Some(_)) => None,
                        (request, None) => {
                            *state = Some(request);
                            Some(request)
                        }
                    };
                    (accepted, *state)
                };
                if accepted == Some(RuntimeControlRequest::Restart) {
                    restart.store(true, Ordering::SeqCst);
                    eprintln!(
                        "octopus: restart requested through the control socket — stopping cleanly, then starting again"
                    );
                } else if accepted == Some(RuntimeControlRequest::Stop) {
                    eprintln!("octopus: stop requested through the control socket — stopping cleanly");
                }
                let status = read_status(octopus_home, current);
                if accepted.is_some() {
                    cancel.cancel();
                }
                match status {
                    Ok(status) => ResponseFrame::Status(status),
                    Err(error) => ResponseFrame::Refused { error },
                }
            }
        }
    };
    let mut bytes = serde_json::to_vec(&response).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .await
        .map_err(|error| format!("control response cannot be written: {error}"))?;
    writer.shutdown().await.map_err(|error| error.to_string())
}

fn read_status(
    octopus_home: &Path,
    stopping: Option<RuntimeControlRequest>,
) -> Result<RuntimeControlStatus, String> {
    let record = crate::runtime_instance::read_current(octopus_home).map_err(|error| error.to_string())?;
    Ok(RuntimeControlStatus {
        pid: record.pid,
        phase: record.phase,
        started_at_unix_ms: u64::try_from(record.started_at_unix_ms)
            .map_err(|_| "runtime start time exceeds the control frame range".to_string())?,
        stopping,
    })
}

#[cfg(unix)]
fn current_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

pub enum Answer {
    Status(RuntimeControlStatus),
    Unreachable(std::io::Error),
}

pub fn request(octopus_home: &Path, request: RuntimeControlRequest) -> Result<Answer, String> {
    let path = socket_path(octopus_home);
    let stream = match connect(&path) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(Answer::Unreachable(error));
        }
        Err(error) => {
            return Err(format!(
                "runtime control socket '{}' cannot be reached: {error}",
                path.display()
            ));
        }
    };
    exchange(stream, request)
}

#[cfg(unix)]
fn connect(path: &Path) -> std::io::Result<std::os::unix::net::UnixStream> {
    let stream = std::os::unix::net::UnixStream::connect(path)?;
    stream.set_read_timeout(Some(CONNECTION_TIMEOUT))?;
    stream.set_write_timeout(Some(CONNECTION_TIMEOUT))?;
    Ok(stream)
}

/// Opens the control pipe. A busy pipe means the runtime is between instances,
/// so it is retried until the connection timeout. The server bounds every
/// exchange with the same timeout, so a stalled exchange cannot hang forever.
#[cfg(windows)]
fn connect(path: &Path) -> std::io::Result<std::fs::File> {
    const ERROR_PIPE_BUSY: i32 = 231;
    let deadline = Instant::now() + CONNECTION_TIMEOUT;
    loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(path) {
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            result => return result,
        }
    }
}

fn exchange<S>(stream: S, request: RuntimeControlRequest) -> Result<Answer, String>
where
    S: std::io::Read + Write,
{
    let mut stream = stream;
    let mut frame = serde_json::to_vec(&RequestFrame { request }).map_err(|error| error.to_string())?;
    frame.push(b'\n');
    stream
        .write_all(&frame)
        .map_err(|error| format!("runtime control request cannot be sent: {error}"))?;
    let mut line = String::new();
    std::io::Read::take(std::io::BufReader::new(stream), MAX_FRAME_BYTES)
        .read_line(&mut line)
        .map_err(|error| format!("runtime control response cannot be read: {error}"))?;
    match serde_json::from_str::<ResponseFrame>(&line)
        .map_err(|error| format!("runtime control response is malformed: {error}"))?
    {
        ResponseFrame::Status(status) => Ok(Answer::Status(status)),
        ResponseFrame::Refused { error } => Err(format!("runtime refused the control request: {error}")),
    }
}

pub fn await_departure(octopus_home: &Path, deadline: Instant) -> Result<(), String> {
    loop {
        match request(octopus_home, RuntimeControlRequest::Status) {
            Ok(Answer::Unreachable(_)) => return Ok(()),
            Ok(Answer::Status(_)) | Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(Answer::Status(status)) => {
                return Err(format!(
                    "octopus pid {} is still answering in phase '{}'",
                    status.pid, status.phase
                ));
            }
            Err(error) => return Err(error),
        }
    }
}

pub fn await_successor(
    octopus_home: &Path,
    predecessor_started_at_unix_ms: u64,
    deadline: Instant,
    mut progress: impl FnMut(&RuntimeControlStatus),
) -> Result<RuntimeControlStatus, String> {
    let mut last_phase = String::new();
    loop {
        match request(octopus_home, RuntimeControlRequest::Status) {
            Ok(Answer::Status(status))
                if status.started_at_unix_ms != predecessor_started_at_unix_ms =>
            {
                if status.phase != last_phase {
                    last_phase = status.phase.clone();
                    progress(&status);
                }
                if status.phase == "ready" && status.stopping.is_none() {
                    return Ok(status);
                }
            }
            Ok(_) | Err(_) if Instant::now() < deadline => {}
            Ok(Answer::Status(status)) => {
                return Err(format!(
                    "octopus pid {} has not started again; it answers in phase '{}'",
                    status.pid, status.phase
                ));
            }
            Ok(Answer::Unreachable(error)) => {
                return Err(format!("no Octopus runtime answered after the restart: {error}"));
            }
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the restarted runtime did not become ready in time; last phase '{}'",
                if last_phase.is_empty() { "none" } else { &last_phase }
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn control_socket_path_is_the_right_to_stop_and_restart_the_instance() {
        let temporary = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(temporary.path()).unwrap();
        let instance = crate::runtime_instance::RuntimeInstance::acquire(&home).unwrap();
        let control = RuntimeControl::bind(&home).unwrap();
        assert_eq!(control.path(), socket_path(&home));
        let cancel = CancellationToken::new();
        let restart = Arc::new(AtomicBool::new(false));
        let server = tokio::spawn(control.serve(home.clone(), cancel.clone(), restart.clone()));
        let ask = |request| {
            let home = home.clone();
            tokio::task::spawn_blocking(move || super::request(&home, request))
        };
        let Answer::Status(status) = ask(RuntimeControlRequest::Status).await.unwrap().unwrap() else {
            panic!("a live instance answers its control socket");
        };
        assert_eq!((status.pid, status.phase.as_str(), status.stopping), (std::process::id(), "starting", None));
        assert!(!cancel.is_cancelled());
        let Answer::Status(accepted) = ask(RuntimeControlRequest::Restart).await.unwrap().unwrap() else {
            panic!("restart is answered");
        };
        assert_eq!(accepted.stopping, Some(RuntimeControlRequest::Restart));
        assert!(cancel.is_cancelled() && restart.load(Ordering::SeqCst));
        let Answer::Status(repeated) = ask(RuntimeControlRequest::Stop).await.unwrap().unwrap() else {
            panic!("a stopping instance still answers");
        };
        assert_eq!(repeated.stopping, Some(RuntimeControlRequest::Restart));
        server.abort();
        drop(instance);
        assert!(matches!(ask(RuntimeControlRequest::Status).await.unwrap().unwrap(), Answer::Unreachable(_)));
        let stopped_home = home.clone();
        tokio::task::spawn_blocking(move || await_departure(&stopped_home, Instant::now())).await.unwrap().unwrap();
    }
}
