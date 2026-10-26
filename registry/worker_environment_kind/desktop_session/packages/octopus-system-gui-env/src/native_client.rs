use crate::backend::{DesktopExecutor, Execution};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
#[cfg(unix)]
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::sync::{oneshot, Notify};

type Pending = Arc<Mutex<BTreeMap<u64, oneshot::Sender<Result<Value, String>>>>>;

struct Connection {
    input: tokio::sync::Mutex<Option<tokio::process::ChildStdin>>,
    child: tokio::sync::Mutex<tokio::process::Child>,
    pending: Pending,
    next: AtomicU64,
    reader: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

pub struct NativeExecutor {
    program: PathBuf,
    sandbox: Option<(String, PathBuf)>,
    max_bytes: usize,
    timeout: Duration,
    connection: tokio::sync::Mutex<Option<Arc<Connection>>>,
    frames: Arc<Mutex<BTreeMap<String, Value>>>,
    frame_ready: Arc<Notify>,
    views: tokio::sync::Mutex<BTreeMap<String, String>>,
    closed_views: Mutex<BTreeSet<String>>,
    closed: AtomicBool,
    device_file: Option<Arc<std::fs::File>>,
    application_behavior: Option<crate::config::ApplicationBehavior>,
    application_policy: Option<crate::config::ApplicationPolicy>,
    application_instructions: Option<crate::config::ApplicationInstructionCatalog>,
}

impl NativeExecutor {
    pub fn new(program: PathBuf, max_bytes: usize, timeout: Duration) -> Self {
        Self {
            program,
            sandbox: None,
            max_bytes,
            timeout,
            connection: tokio::sync::Mutex::new(None),
            frames: Arc::new(Mutex::new(BTreeMap::new())),
            frame_ready: Arc::new(Notify::new()),
            views: tokio::sync::Mutex::new(BTreeMap::new()),
            closed_views: Mutex::new(BTreeSet::new()),
            closed: AtomicBool::new(false),
            device_file: None,
            application_behavior: None,
            application_policy: None,
            application_instructions: None,
        }
    }

    pub(crate) fn with_application_behavior(mut self, behavior: crate::config::ApplicationBehavior) -> Result<Self, String> {
        behavior.validate()?;
        self.application_behavior = Some(behavior);
        Ok(self)
    }

    pub(crate) fn with_application_policy(mut self, policy: Option<crate::config::ApplicationPolicy>) -> Result<Self, String> {
        if let Some(policy) = &policy { policy.validate()?; }
        self.application_policy = policy;
        Ok(self)
    }

    pub(crate) fn with_application_instructions(mut self, instructions: crate::config::ApplicationInstructionCatalog) -> Result<Self, String> {
        instructions.validate()?;
        self.application_instructions = Some(instructions);
        Ok(self)
    }

    #[cfg(unix)]
    pub fn with_device_file(mut self, file: Arc<std::fs::File>) -> Result<Self, String> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if file.as_raw_fd() < 3 || !metadata.is_file() || metadata.nlink() != 1
            || metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o600 {
            return Err("native inherited device reference must be one owned regular 0600 file".into());
        }
        self.device_file = Some(file);
        Ok(self)
    }

    #[cfg(not(unix))]
    #[allow(unused_variables, unused_mut)]
    pub fn with_device_file(mut self, file: Arc<std::fs::File>) -> Result<Self, String> {
        Err("desktop control requires macOS".into())
    }

    #[cfg(unix)]
    pub fn with_sandbox(mut self, profile: PathBuf, cancel_root: PathBuf) -> Result<Self, String> {
        if !cfg!(target_os = "macos") {
            return Err("native desktop sandbox requires macOS".into());
        }
        for path in [&self.program, &profile, &cancel_root] {
            if !path.is_absolute() || path.canonicalize().map_err(|error| error.to_string())? != *path {
                return Err("native desktop sandbox paths must be canonical".into());
            }
        }
        crate::native_connection::validate_directory(&cancel_root)?;
        use std::io::Read;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let mut file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW)
            .open(&profile).map_err(|error| format!("native desktop sandbox profile unavailable: {error}"))?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() > 16384 || metadata.mode() & 0o022 != 0 {
            return Err("native desktop sandbox profile must be a bounded protected regular file".into());
        }
        let mut bytes = Vec::new();
        file.by_ref().take(16385).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
        if bytes.is_empty() || bytes.len() > 16384 { return Err("native desktop sandbox profile exceeds its bounds".into()); }
        let profile = String::from_utf8(bytes).map_err(|_| "native desktop sandbox profile is not UTF-8")?;
        self.sandbox = Some((profile, cancel_root));
        Ok(self)
    }

    #[cfg(not(unix))]
    #[allow(unused_variables, unused_mut)]
    pub fn with_sandbox(mut self, profile: PathBuf, cancel_root: PathBuf) -> Result<Self, String> {
        Err("desktop control requires macOS".into())
    }

    fn native_command(&self) -> tokio::process::Command {
        if let Some((profile, cancel_root)) = &self.sandbox {
            let mut command = tokio::process::Command::new("/usr/bin/sandbox-exec");
            command.arg("-D").arg(format!("HELPER={}", self.program.display()))
                .arg("-D").arg(format!("CANCEL_ROOT={}", cancel_root.display()))
                .arg("-p").arg(profile).arg(&self.program);
            command
        } else {
            tokio::process::Command::new(&self.program)
        }
    }

    #[cfg(unix)]
    async fn connect(&self) -> Result<Arc<Connection>, String> {
        let mut slot = self.connection.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return Err("native executor is shut down".into());
        }
        if let Some(connection) = &*slot {
            return Ok(connection.clone());
        }
        let mut command = self.native_command();
        command.env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        if let Some(behavior) = &self.application_behavior {
            command.env("OCTOPUS_DESKTOP_APPLICATION_BEHAVIOR", serde_json::to_string(behavior).map_err(|error| error.to_string())?);
        }
        if let Some(policy) = &self.application_policy {
            command.env("OCTOPUS_DESKTOP_APPLICATION_POLICY", String::from_utf8(policy.bytes()?).map_err(|error| error.to_string())?);
        }
        if let Some(instructions) = &self.application_instructions {
            command.env("OCTOPUS_DESKTOP_APPLICATION_INSTRUCTIONS", serde_json::to_string(instructions).map_err(|error| error.to_string())?);
        }
        if let Some(file) = &self.device_file {
            use std::os::fd::AsRawFd;
            use std::os::unix::fs::MetadataExt;
            let fd = file.as_raw_fd();
            let metadata = file.metadata().map_err(|error| error.to_string())?;
            command.env("OCTOPUS_DESKTOP_DEVICE_REFERENCE", json!({"fd":fd,"device":metadata.dev(),
                "inode":metadata.ino(),"owner":metadata.uid()}).to_string());
            unsafe { command.pre_exec(move || {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            }); }
        }
        let mut child = command.spawn()
            .map_err(|e| format!("native_executor_unavailable: {e}"))?;
        let input = child
            .stdin
            .take()
            .ok_or("native executor stdin is absent")?;
        let output = child
            .stdout
            .take()
            .ok_or("native executor stdout is absent")?;
        let pending: Pending = Arc::new(Mutex::new(BTreeMap::new()));
        let connection = Arc::new(Connection {
            input: tokio::sync::Mutex::new(Some(input)),
            child: tokio::sync::Mutex::new(child),
            pending: pending.clone(),
            next: AtomicU64::new(1),
            reader: tokio::sync::Mutex::new(None),
        });
        let frames = self.frames.clone();
        let ready = self.frame_ready.clone();
        let max = self.max_bytes;
        let weak_connection = Arc::downgrade(&connection);
        let reader = tokio::spawn(async move {
            let mut output = BufReader::new(output);
            let failed = loop {
                let mut bytes = Vec::new();
                let count = (&mut output)
                    .take((max + 1) as u64)
                    .read_until(b'\n', &mut bytes)
                    .await;
                match count {
                    Ok(0) => {
                        break "native executor disconnected; pending effects require observation"
                            .to_string()
                    }
                    Err(error) => break format!("native executor output failed: {error}"),
                    _ => {}
                }
                if bytes.len() > max || bytes.last() != Some(&b'\n') {
                    break "native executor exceeded its bounded frame protocol".to_string();
                }
                let value: Value = match serde_json::from_slice(&bytes) {
                    Ok(value) => value,
                    Err(error) => break format!("native executor emitted malformed JSON: {error}"),
                };
                if value["event"] == "stream_error" {
                    let Some(screen) = value["screen_id"].as_str() else {
                        break "native stream error has no screen identity".into();
                    };
                    if let Ok(mut latest) = frames.lock() {
                        latest.insert(screen.into(), json!({"error":value["message"]}));
                    }
                    ready.notify_waiters();
                    continue;
                }
                if value["event"] == "frame" {
                    let Some(screen) = value["screen_id"].as_str() else {
                        break "native frame has no screen identity".into();
                    };
                    if let Ok(mut latest) = frames.lock() {
                        latest.insert(screen.into(), value["observation"].clone());
                    }
                    ready.notify_waiters();
                    continue;
                }
                let Some(id) = value["id"].as_u64() else {
                    break "native response has no request identity".into();
                };
                let sender = match pending.lock() {
                    Ok(mut pending) => pending.remove(&id),
                    Err(_) => None,
                };
                if let Some(sender) = sender {
                    let result = if value["ok"] == true {
                        Ok(value["result"].clone())
                    } else {
                        Err(format!(
                            "{}: {}",
                            value["code"].as_str().unwrap_or("native_failed"),
                            value["message"]
                                .as_str()
                                .unwrap_or("native execution failed")
                        ) + &value.get("details").map(|details| format!("; native_evidence={details}")).unwrap_or_default())
                    };
                    let _ = sender.send(result);
                }
            };
            if let Some(connection) = weak_connection.upgrade() {
                connection.input.lock().await.take();
                let mut child = connection.child.lock().await;
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
            if let Ok(mut pending) = pending.lock() {
                for (_, sender) in std::mem::take(&mut *pending) {
                    let _ = sender.send(Err(failed.clone()));
                }
            }
            if let Ok(mut latest) = frames.lock() {
                latest.clear();
                latest.insert("*".into(), json!({"error":failed}));
            }
            ready.notify_waiters();
        });
        *connection.reader.lock().await = Some(reader);
        *slot = Some(connection.clone());
        Ok(connection)
    }

    #[cfg(not(unix))]
    #[allow(unused_variables, unused_mut)]
    async fn connect(&self) -> Result<Arc<Connection>, String> {
        Err("desktop control requires macOS".into())
    }

    async fn command(&self, action: Value) -> Result<Value, String> {
        let connection = self.connect().await?;
        let id = connection.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        connection
            .pending
            .lock()
            .map_err(|_| "native pending map poisoned")?
            .insert(id, tx);
        let mut bytes =
            serde_json::to_vec(&json!({"id":id,"action":action})).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        let mut written = false;
        let response = async {
            {
                let mut input = connection.input.lock().await;
                let input = input.as_mut().ok_or(
                    "native executor input is closed; explicit host restart required".to_string(),
                )?;
                if action["type"] == "permissions" && action["operation"] == "open_settings" {
                    let cancel = action["cancel_file"].as_str().filter(|path| path.starts_with('/') && !path.contains('\0'))
                        .ok_or("native Settings continuation has no exact cancellation path")?;
                    if std::path::Path::new(cancel).try_exists().map_err(|error| error.to_string())? {
                        return Err("native Settings request was cancelled before dispatch".into());
                    }
                }
                input
                    .write_all(&bytes)
                    .await
                    .map_err(|e| format!("native executor write failed: {e}"))?;
                input.flush().await.map_err(|e| e.to_string())?;
            }
            written = true;
            rx.await
                .map_err(|_| "native executor request disconnected".to_string())?
        };
        match tokio::time::timeout(self.timeout, response).await {
            Ok(value) => {
                connection
                    .pending
                    .lock()
                    .map_err(|_| "native pending map poisoned")?
                    .remove(&id);
                value
            }
            Err(_) => {
                connection
                    .pending
                    .lock()
                    .map_err(|_| "native pending map poisoned")?
                    .remove(&id);
                if let Some(path) = action["cancel_file"].as_str() {
                    std::fs::write(path, b"deadline").map_err(|e| e.to_string())?;
                }
                if !written {
                    connection.input.lock().await.take();
                    connection
                        .child
                        .lock()
                        .await
                        .kill()
                        .await
                        .map_err(|e| e.to_string())?;
                }
                Err("native executor exceeded its declared request deadline; effect may be partial, inspect before retrying".into())
            }
        }
    }

    async fn view(&self, action: Value) -> Result<Value, String> {
        let id = action["view_id"]
            .as_str()
            .filter(|v| !v.is_empty())
            .ok_or("view_id is required")?;
        let screen = action["screen_id"]
            .as_str()
            .filter(|v| !v.is_empty())
            .ok_or("screen_id is required")?;
        let mut views = self.views.lock().await;
        if self
            .closed_views
            .lock()
            .map_err(|_| "closed views poisoned")?
            .contains(id)
        {
            return Err("view attachment was closed".into());
        }
        if views.get(id).is_some_and(|old| old != screen) {
            return Err("view attachment changed screen; attach a new view identity".into());
        }
        if !views.contains_key(id) {
            self.command(json!({"type":"view_start","view_id":id,"screen_id":screen}))
                .await?;
            views.insert(id.into(), screen.into());
        }
        drop(views);
        let frame = async {
            loop {
                let notified = self.frame_ready.notified();
                if let Some(error) = self
                    .frames
                    .lock()
                    .map_err(|_| "native frames poisoned")?
                    .get("*")
                    .and_then(|value| value["error"].as_str())
                {
                    return Err(error.to_string());
                }
                if self
                    .closed_views
                    .lock()
                    .map_err(|_| "closed views poisoned")?
                    .contains(id)
                {
                    return Err("view attachment was closed".to_string());
                }
                if let Some(observation) = self
                    .frames
                    .lock()
                    .map_err(|_| "native frames poisoned")?
                    .get(screen)
                    .cloned()
                {
                    if let Some(error) = observation["error"].as_str() {
                        return Err(format!("native stream failed: {error}"));
                    }
                    return Ok(observation);
                }
                notified.await;
            }
        };
        let observation = tokio::time::timeout(self.timeout, frame)
            .await
            .map_err(|_| "native stream produced no frame within the declared deadline")??;
        Ok(json!({"observation":observation}))
    }
}

impl DesktopExecutor for NativeExecutor {
    fn retained_process_id<'a>(&'a self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<u32, String>> + Send + 'a>> {
        Box::pin(async move {
            if self.closed.load(Ordering::Acquire) { return Err("native producer was shut down".into()); }
            let slot = self.connection.lock().await;
            let connection = slot.as_ref().ok_or("native presentation cannot create a producer process")?;
            let mut child = connection.child.lock().await;
            if child.try_wait().map_err(|_| "native process state unavailable")?.is_some() {
                return Err("native producer process has ended".into());
            }
            child.id().ok_or_else(|| "native producer has no retained process identity".into())
        })
    }
    fn execute<'a>(&'a self, action: Value) -> Execution<'a> {
        Box::pin(async move {
            if self.closed.load(Ordering::Acquire) {
                return Err("native executor is shut down".into());
            }
            if action["type"] == "view" {
                return self.view(action).await;
            }
            if action["type"] == "pause_view" {
                let id = action["view_id"].as_str().ok_or("view_id is required")?;
                self.closed_views
                    .lock()
                    .map_err(|_| "closed views poisoned")?
                    .insert(id.into());
                self.frame_ready.notify_waiters();
                let mut views = self.views.lock().await;
                self.command(json!({"type":"view_stop","view_id":id}))
                    .await?;
                if let Some(screen) = views.remove(id) {
                    if !views.values().any(|value| value == &screen) {
                        self.frames
                            .lock()
                            .map_err(|_| "native frames poisoned")?
                            .remove(&screen);
                    }
                }
                return Ok(json!({}));
            }
            self.command(action).await
        })
    }

    fn shutdown<'a>(&'a self) -> Execution<'a> {
        Box::pin(async move {
            self.closed.store(true, Ordering::Release);
            let connection = self.connection.lock().await.take();
            if let Some(connection) = connection {
                connection.input.lock().await.take();
                let mut child = connection.child.lock().await;
                match tokio::time::timeout(self.timeout, child.wait()).await {
                    Ok(result) => {
                        result.map_err(|e| e.to_string())?;
                    }
                    Err(_) => {
                        child.start_kill().map_err(|e| e.to_string())?;
                        child.wait().await.map_err(|e| e.to_string())?;
                    }
                }
                drop(child);
                if let Some(reader) = connection.reader.lock().await.take() {
                    let _ = reader.await;
                }
            }
            self.frames
                .lock()
                .map_err(|_| "native frames poisoned")?
                .clear();
            Ok(json!({"closed":true}))
        })
    }
}
