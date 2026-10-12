use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, Notify, RwLock, oneshot};
use tokio_util::codec::{FramedRead, LinesCodec};

use crate::config::McpStdioConfig;
use crate::event_journal::StdioEventJournal;

#[derive(Clone, Debug, Default)]
pub(crate) struct McpCapabilities {
    pub(crate) tools: bool,
    pub(crate) tools_list_changed: bool,
    pub(crate) resources: bool,
    pub(crate) resources_subscribe: bool,
    pub(crate) resources_list_changed: bool,
    pub(crate) prompts: bool,
    pub(crate) prompts_list_changed: bool,
    pub(crate) logging: bool,
    pub(crate) completions: bool,
}

impl McpCapabilities {
    pub(crate) fn as_value(&self) -> Value {
        json!({
            "tools": { "supported": self.tools, "list_changed": self.tools_list_changed },
            "resources": {
                "supported": self.resources,
                "subscribe": self.resources_subscribe,
                "list_changed": self.resources_list_changed,
            },
            "prompts": { "supported": self.prompts, "list_changed": self.prompts_list_changed },
            "logging": self.logging,
            "completions": self.completions,
        })
    }
}

#[derive(Clone)]
pub(crate) struct StdioMcpClient {
    inner: Arc<StdioMcpClientInner>,
}

struct StdioMcpClientInner {
    child: Mutex<tokio::process::Child>,
    stdin: Mutex<tokio::io::BufWriter<tokio::process::ChildStdin>>,
    pending: std::sync::Mutex<BTreeMap<i64, oneshot::Sender<Result<Value, String>>>>,
    retired: std::sync::Mutex<BTreeSet<i64>>,
    next_id: AtomicI64,
    closed: AtomicBool,
    metadata: RwLock<StdioClientMetadata>,
    journal: Mutex<StdioEventJournal>,
    event_changed: Notify,
    request_timeout: Duration,
    max_message_bytes: usize,
    event_log: PathBuf,
    stderr_log: PathBuf,
    command: String,
    args: Vec<String>,
}

struct StdioClientMetadata {
    initialize_result: Value,
    capabilities: McpCapabilities,
    protocol_version: String,
}

struct StdioPendingRegistration {
    id: i64,
    pending: Arc<StdioMcpClientInner>,
}

impl Drop for StdioPendingRegistration {
    fn drop(&mut self) {
        let removed = self
            .pending
            .pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(&self.id))
            .is_some();
        if removed {
            self.pending.retire_id(self.id);
        }
    }
}

impl StdioMcpClient {
    pub(crate) async fn spawn(
        config: &McpStdioConfig,
        cwd: &Path,
        room_root: &Path,
    ) -> Result<Self, String> {
        let executable = std::fs::canonicalize(&config.command).map_err(|error| {
            format!(
                "MCP stdio command '{}' is unavailable: {error}",
                config.command
            )
        })?;
        if executable != Path::new(&config.command) {
            return Err("MCP stdio executable changed after command resolution".to_string());
        }
        std::fs::create_dir_all(room_root).map_err(|error| {
            format!(
                "MCP stdio room root '{}' cannot be created: {error}",
                room_root.display()
            )
        })?;
        let event_log = room_root.join("server_events.jsonl");
        let stderr_log = room_root.join("server_stderr.log");
        let event_journal = StdioEventJournal::open(
            event_log.clone(),
            config.event_journal_segment_bytes,
            config.event_journal_retained_segments,
        )?;
        validate_stderr_history(
            &stderr_log,
            config.max_stderr_segment_bytes,
            config.stderr_retained_segments,
        )?;
        let mut command = tokio::process::Command::new(&executable);
        command
            .args(&config.args)
            .current_dir(cwd)
            .env_clear()
            .envs(child_environment(config, room_root)?)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| {
            format!(
                "MCP stdio command '{}' failed to spawn: {error}",
                config.command
            )
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "MCP stdio child stdin is unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "MCP stdio child stdout is unavailable".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "MCP stdio child stderr is unavailable".to_string())?;
        let inner = Arc::new(StdioMcpClientInner {
            child: Mutex::new(child),
            stdin: Mutex::new(tokio::io::BufWriter::new(stdin)),
            pending: std::sync::Mutex::new(BTreeMap::new()),
            retired: std::sync::Mutex::new(BTreeSet::new()),
            next_id: AtomicI64::new(1),
            closed: AtomicBool::new(false),
            metadata: RwLock::new(StdioClientMetadata {
                initialize_result: Value::Null,
                capabilities: McpCapabilities::default(),
                protocol_version: config.protocol_versions[0].clone(),
            }),
            journal: Mutex::new(event_journal),
            event_changed: Notify::new(),
            request_timeout: Duration::from_millis(config.request_timeout_ms),
            max_message_bytes: config.max_message_bytes,
            event_log,
            stderr_log: stderr_log.clone(),
            command: config.command.clone(),
            args: config.args.clone(),
        });
        let reader_inner = Arc::downgrade(&inner);
        let max_message_bytes = config.max_message_bytes;
        tokio::spawn(async move {
            let mut framed =
                FramedRead::new(stdout, LinesCodec::new_with_max_length(max_message_bytes));
            while let Some(message) = framed.next().await {
                let Some(inner) = reader_inner.upgrade() else {
                    return;
                };
                let message = match message {
                    Ok(line) => serde_json::from_str::<Value>(&line).map_err(|error| {
                        format!("MCP stdio server emitted malformed JSON: {error}")
                    }),
                    Err(error) => Err(format!("MCP stdio stdout framing failed: {error}")),
                };
                match message {
                    Ok(message) => {
                        if let Err(error) = inner.route_message(message).await {
                            inner.fail(&error).await;
                            return;
                        }
                    }
                    Err(error) => {
                        inner.fail(&error).await;
                        return;
                    }
                }
            }
            if let Some(inner) = reader_inner.upgrade() {
                inner.fail("MCP stdio server closed stdout").await;
            }
        });
        let stderr_inner = Arc::downgrade(&inner);
        let max_stderr_segment_bytes = config.max_stderr_segment_bytes;
        let stderr_retained_segments = config.stderr_retained_segments;
        tokio::spawn(async move {
            if let Err(error) = capture_stderr(
                stderr,
                &stderr_log,
                max_stderr_segment_bytes,
                stderr_retained_segments,
            )
            .await
            {
                if let Some(inner) = stderr_inner.upgrade() {
                    inner.fail(&error).await;
                    let mut child = inner.child.lock().await;
                    if child.try_wait().is_ok_and(|status| status.is_none()) {
                        let _ = child.kill().await;
                    }
                    let _ = child.wait().await;
                }
            }
        });
        let client = Self { inner };
        let result = match client
            .request(
                "initialize",
                json!({
                    "protocolVersion": config.protocol_versions[0],
                    "capabilities": {},
                    "clientInfo": { "name": config.client_name, "version": config.client_version }
                }),
            )
            .await
        {
            Ok(result) => result,
            Err(error) => {
                let _ = client.terminate().await;
                return Err(error);
            }
        };
        let (protocol_version, capabilities) =
            validate_initialize(&result, &config.protocol_versions)?;
        {
            let mut metadata = client.inner.metadata.write().await;
            metadata.initialize_result = result;
            metadata.protocol_version = protocol_version;
            metadata.capabilities = capabilities;
        }
        client
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(client)
    }

    pub(crate) async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        self.request_with_timeout(method, params, self.inner.request_timeout)
            .await
    }

    pub(crate) async fn request_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        self.ensure_alive().await?;
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        if id <= 0 {
            return Err("MCP stdio request id overflowed".to_string());
        }
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self
                .inner
                .pending
                .lock()
                .map_err(|_| "MCP stdio pending map is poisoned".to_string())?;
            if pending.insert(id, sender).is_some() {
                return Err(format!("MCP stdio request id {id} is already pending"));
            }
        }
        let registration = StdioPendingRegistration {
            id,
            pending: Arc::clone(&self.inner),
        };
        if let Err(error) = self
            .send(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            }))
            .await
        {
            drop(registration);
            return Err(error);
        }
        let response = match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => Err("MCP stdio response router stopped".to_string()),
            Err(_) => {
                drop(registration);
                let _ = self
                    .notify(
                        "notifications/cancelled",
                        json!({ "requestId": id, "reason": "client request timeout" }),
                    )
                    .await;
                return Err(format!(
                    "MCP stdio request '{method}' timed out after {}ms",
                    timeout.as_millis()
                ));
            }
        };
        drop(registration);
        response
    }

    pub(crate) async fn cancel_request(
        &self,
        request_id: Value,
        reason: Option<&str>,
    ) -> Result<(), String> {
        let id = request_id
            .as_i64()
            .ok_or_else(|| "MCP cancel request_id must be an integer".to_string())?;
        let sender = self
            .inner
            .pending
            .lock()
            .map_err(|_| "MCP stdio pending map is poisoned".to_string())?
            .remove(&id);
        if let Some(sender) = sender {
            self.inner.retire_id(id);
            let _ = sender.send(Err(format!("MCP stdio request {id} was cancelled")));
        }
        let mut params = json!({ "requestId": id });
        if let Some(reason) = reason {
            params["reason"] = json!(reason);
        }
        self.notify("notifications/cancelled", params).await
    }

    pub(crate) async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.ensure_alive().await?;
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
    }

    pub(crate) async fn poll_events(
        &self,
        after_cursor: u64,
        max_events: usize,
        wait_ms: u64,
    ) -> Result<Vec<Value>, String> {
        let notified = self.inner.event_changed.notified();
        let current = self.event_cursor().await;
        if after_cursor > current {
            return Err(format!(
                "MCP stdio event cursor {after_cursor} is ahead of durable cursor {current}"
            ));
        }
        if after_cursor == current && wait_ms > 0 {
            let _ = tokio::time::timeout(Duration::from_millis(wait_ms), notified).await;
        }
        self.read_events(after_cursor, max_events).await
    }

    pub(crate) async fn read_events(
        &self,
        after_cursor: u64,
        max_events: usize,
    ) -> Result<Vec<Value>, String> {
        let journal = self.inner.journal.lock().await;
        journal.read_after(after_cursor, max_events)
    }

    pub(crate) async fn event_cursor(&self) -> u64 {
        self.inner.journal.lock().await.sequence()
    }

    pub(crate) async fn initialize_result(&self) -> Value {
        self.inner.metadata.read().await.initialize_result.clone()
    }

    pub(crate) async fn capabilities(&self) -> McpCapabilities {
        self.inner.metadata.read().await.capabilities.clone()
    }

    pub(crate) async fn protocol_version(&self) -> String {
        self.inner.metadata.read().await.protocol_version.clone()
    }

    pub(crate) async fn pid(&self) -> Option<u32> {
        self.inner.child.lock().await.id()
    }

    pub(crate) fn command(&self) -> &str {
        &self.inner.command
    }

    pub(crate) fn args(&self) -> &[String] {
        &self.inner.args
    }

    pub(crate) fn event_log(&self) -> &Path {
        &self.inner.event_log
    }

    pub(crate) fn stderr_log(&self) -> &Path {
        &self.inner.stderr_log
    }

    pub(crate) fn in_flight_request_ids(&self) -> Result<Vec<i64>, String> {
        self.inner
            .pending
            .lock()
            .map(|pending| pending.keys().copied().collect())
            .map_err(|_| "MCP stdio pending map is poisoned".to_string())
    }

    pub(crate) async fn ensure_alive(&self) -> Result<(), String> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err("MCP stdio transport is closed".to_string());
        }
        match self.inner.child.lock().await.try_wait() {
            Ok(None) => Ok(()),
            Ok(Some(status)) => Err(format!("MCP stdio server exited with {status}")),
            Err(error) => Err(format!("MCP stdio child status failed: {error}")),
        }
    }

    pub(crate) async fn terminate(&self) -> Result<(), String> {
        self.inner.closed.store(true, Ordering::Release);
        self.inner.fail_pending("MCP stdio transport closed");
        let mut child = self.inner.child.lock().await;
        if child
            .try_wait()
            .map_err(|error| error.to_string())?
            .is_none()
        {
            child
                .kill()
                .await
                .map_err(|error| format!("MCP stdio child termination failed: {error}"))?;
        }
        child
            .wait()
            .await
            .map_err(|error| format!("MCP stdio child wait failed: {error}"))?;
        Ok(())
    }

    async fn send(&self, value: &Value) -> Result<(), String> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err("MCP stdio transport is closed".to_string());
        }
        let mut bytes = serde_json::to_vec(value)
            .map_err(|error| format!("MCP stdio message did not serialize: {error}"))?;
        if bytes.len() > self.inner.max_message_bytes {
            return Err(format!(
                "MCP stdio outbound message exceeds {} bytes",
                self.inner.max_message_bytes
            ));
        }
        bytes.push(b'\n');
        let mut stdin = self.inner.stdin.lock().await;
        if self.inner.closed.load(Ordering::Acquire) {
            return Err("MCP stdio transport is closed".to_string());
        }
        stdin
            .write_all(&bytes)
            .await
            .map_err(|error| format!("MCP stdio stdin write failed: {error}"))?;
        stdin
            .flush()
            .await
            .map_err(|error| format!("MCP stdio stdin flush failed: {error}"))
    }
}

impl StdioMcpClientInner {
    async fn route_message(&self, message: Value) -> Result<(), String> {
        let object = message
            .as_object()
            .ok_or_else(|| "MCP stdio message is not an object".to_string())?;
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err("MCP stdio message jsonrpc is not '2.0'".to_string());
        }
        if let Some(method) = object.get("method") {
            let method = method
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| "MCP stdio server method is empty or non-string".to_string())?;
            let event = if let Some(id) = object.get("id") {
                self.send_value(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": "server-initiated requests require an authored Octopus artifact path"
                    }
                }))
                .await?;
                json!({ "event": "server_request_refused", "method": method, "message": message })
            } else {
                json!({ "event": "server_notification", "method": method, "message": message })
            };
            self.record_event(event).await?;
            return Ok(());
        }
        let id = object
            .get("id")
            .and_then(Value::as_i64)
            .ok_or_else(|| "MCP stdio response id is absent or non-integer".to_string())?;
        let sender = self
            .pending
            .lock()
            .map_err(|_| "MCP stdio pending map is poisoned".to_string())?
            .remove(&id);
        if let Some(sender) = sender {
            let result = match (object.get("result"), object.get("error")) {
                (Some(result), None) => Ok(result.clone()),
                (None, Some(error)) => Err(render_server_error(error)?),
                _ => {
                    return Err(
                        "MCP stdio response must carry exactly one of result or error".to_string(),
                    );
                }
            };
            let _ = sender.send(result);
            return Ok(());
        }
        let retired = self
            .retired
            .lock()
            .is_ok_and(|mut retired| retired.remove(&id));
        if retired {
            self.record_event(json!({
                "event": "late_cancelled_response",
                "request_id": id,
                "message": message,
            }))
            .await?;
            return Ok(());
        }
        Err(format!("MCP stdio response id {id} has no pending request"))
    }

    async fn send_value(&self, value: &Value) -> Result<(), String> {
        if self.closed.load(Ordering::Acquire) {
            return Err("MCP stdio transport is closed".to_string());
        }
        let mut bytes = serde_json::to_vec(value)
            .map_err(|error| format!("MCP stdio message did not serialize: {error}"))?;
        if bytes.len() > self.max_message_bytes {
            return Err(format!(
                "MCP stdio outbound message exceeds {} bytes",
                self.max_message_bytes
            ));
        }
        bytes.push(b'\n');
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(&bytes)
            .await
            .map_err(|error| format!("MCP stdio stdin write failed: {error}"))?;
        stdin
            .flush()
            .await
            .map_err(|error| format!("MCP stdio stdin flush failed: {error}"))
    }

    async fn record_event(&self, event: Value) -> Result<(), String> {
        let mut journal = self.journal.lock().await;
        journal.append(event)?;
        drop(journal);
        self.event_changed.notify_waiters();
        Ok(())
    }

    fn retire_id(&self, id: i64) {
        if let Ok(mut retired) = self.retired.lock() {
            if retired.len() >= 4096 {
                if let Some(first) = retired.first().copied() {
                    retired.remove(&first);
                }
            }
            retired.insert(id);
        }
    }

    fn fail_pending(&self, reason: &str) {
        if let Ok(mut pending) = self.pending.lock() {
            let requests = std::mem::take(&mut *pending);
            drop(pending);
            for (_, sender) in requests {
                let _ = sender.send(Err(reason.to_string()));
            }
        }
    }

    async fn fail(&self, reason: &str) {
        let already_closed = self.closed.swap(true, Ordering::AcqRel);
        self.fail_pending(reason);
        if !already_closed {
            let _ = self
                .record_event(json!({ "event": "transport_failed", "reason": reason }))
                .await;
        }
    }
}

fn validate_stderr_history(
    path: &Path,
    max_segment_bytes: u64,
    retained_segments: usize,
) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("MCP stdio stderr log '{}' has no parent", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            format!(
                "MCP stdio stderr log '{}' has a non-UTF-8 name",
                path.display()
            )
        })?;
    let archive_prefix = format!("{file_name}.");
    for entry in std::fs::read_dir(parent).map_err(|error| {
        format!(
            "MCP stdio stderr directory '{}' cannot be read: {error}",
            parent.display()
        )
    })? {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name();
        let Some(index) = name
            .to_str()
            .and_then(|name| name.strip_prefix(&archive_prefix))
            .and_then(|index| index.parse::<usize>().ok())
        else {
            continue;
        };
        if index == 0 || index >= retained_segments {
            let metadata =
                std::fs::symlink_metadata(entry.path()).map_err(|error| error.to_string())?;
            if !metadata.file_type().is_file() {
                return Err(format!(
                    "MCP stdio stderr archive '{}' is not a regular file",
                    entry.path().display()
                ));
            }
            std::fs::remove_file(entry.path()).map_err(|error| error.to_string())?;
        }
    }
    for index in 0..retained_segments {
        let candidate = stderr_segment_path(path, index);
        let metadata = match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        if !metadata.file_type().is_file() {
            return Err(format!(
                "MCP stdio stderr segment '{}' is not a regular file",
                candidate.display()
            ));
        }
        if metadata.len() > max_segment_bytes {
            return Err(format!(
                "MCP stdio stderr segment '{}' has {} bytes, exceeding configured ceiling {max_segment_bytes}",
                candidate.display(),
                metadata.len()
            ));
        }
    }
    Ok(())
}

async fn capture_stderr<R: AsyncRead + Unpin>(
    mut stderr: R,
    path: &Path,
    max_segment_bytes: u64,
    retained_segments: usize,
) -> Result<(), String> {
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .map_err(|error| {
            format!(
                "MCP stdio stderr log '{}' cannot be opened: {error}",
                path.display()
            )
        })?;
    let mut written = file
        .metadata()
        .await
        .map_err(|error| error.to_string())?
        .len();
    let mut buffer = vec![0u8; 8192];
    loop {
        let read = stderr.read(&mut buffer).await.map_err(|error| {
            format!(
                "MCP stdio stderr stream cannot be read for '{}': {error}",
                path.display()
            )
        })?;
        if read == 0 {
            file.flush().await.map_err(|error| error.to_string())?;
            file.sync_data().await.map_err(|error| error.to_string())?;
            return Ok(());
        }
        let mut offset = 0usize;
        while offset < read {
            if written == max_segment_bytes {
                file = rotate_stderr_segment(file, path, retained_segments).await?;
                written = 0;
            }
            let remaining = usize::try_from(max_segment_bytes - written)
                .unwrap_or(usize::MAX)
                .min(read - offset);
            file.write_all(&buffer[offset..offset + remaining])
                .await
                .map_err(|error| {
                    format!(
                        "MCP stdio stderr log '{}' cannot be written: {error}",
                        path.display()
                    )
                })?;
            written = written
                .checked_add(remaining as u64)
                .ok_or_else(|| "MCP stdio stderr byte count overflowed".to_string())?;
            offset += remaining;
        }
    }
}

async fn rotate_stderr_segment(
    mut file: tokio::fs::File,
    path: &Path,
    retained_segments: usize,
) -> Result<tokio::fs::File, String> {
    file.flush().await.map_err(|error| error.to_string())?;
    file.sync_data().await.map_err(|error| error.to_string())?;
    drop(file);
    if retained_segments == 1 {
        tokio::fs::remove_file(path)
            .await
            .map_err(|error| error.to_string())?;
    } else {
        let oldest = stderr_segment_path(path, retained_segments - 1);
        match tokio::fs::remove_file(&oldest).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        for index in (1..retained_segments - 1).rev() {
            let source = stderr_segment_path(path, index);
            let target = stderr_segment_path(path, index + 1);
            match tokio::fs::rename(&source, &target).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        tokio::fs::rename(path, stderr_segment_path(path, 1))
            .await
            .map_err(|error| error.to_string())?;
    }
    let file = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .await
        .map_err(|error| error.to_string())?;
    sync_stderr_parent(path)?;
    Ok(file)
}

fn stderr_segment_path(path: &Path, index: usize) -> PathBuf {
    if index == 0 {
        path.to_path_buf()
    } else {
        let mut name = path.as_os_str().to_os_string();
        name.push(format!(".{index}"));
        PathBuf::from(name)
    }
}

fn sync_stderr_parent(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("MCP stdio stderr log '{}' has no parent", path.display()))?;
    std::fs::File::open(parent)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{capture_stderr, stderr_segment_path};

    #[tokio::test]
    async fn stderr_capture_rotates_with_a_bounded_retained_set() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("server_stderr.log");
        let payload = vec![b'x'; 3072];
        capture_stderr(&payload[..], &path, 1024, 2).await.unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 1024);
        assert_eq!(
            std::fs::metadata(stderr_segment_path(&path, 1))
                .unwrap()
                .len(),
            1024
        );
        assert!(!stderr_segment_path(&path, 2).exists());
    }
}

fn child_environment(
    config: &McpStdioConfig,
    room_root: &Path,
) -> Result<BTreeMap<String, std::ffi::OsString>, String> {
    let mut environment = BTreeMap::new();
    let path =
        std::env::var_os("PATH").ok_or_else(|| "MCP stdio tool process has no PATH".to_string())?;
    environment.insert("PATH".to_string(), path);
    environment.insert("HOME".to_string(), room_root.as_os_str().to_os_string());
    environment.insert("TMPDIR".to_string(), room_root.as_os_str().to_os_string());
    for name in &config.forward_env {
        let value = std::env::var_os(name).ok_or_else(|| {
            format!("MCP stdio forwarded environment variable '{name}' disappeared")
        })?;
        environment.insert(name.clone(), value);
    }
    for (name, value) in &config.env {
        environment.insert(name.clone(), std::ffi::OsString::from(value));
    }
    Ok(environment)
}

fn validate_initialize(
    result: &Value,
    supported_protocols: &[String],
) -> Result<(String, McpCapabilities), String> {
    let object = result
        .as_object()
        .ok_or_else(|| "MCP initialize result is not an object".to_string())?;
    let protocol = object
        .get("protocolVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| "MCP initialize result has no protocolVersion".to_string())?;
    if !supported_protocols
        .iter()
        .any(|supported| supported == protocol)
    {
        return Err(format!(
            "MCP server selected unsupported protocolVersion '{protocol}'; supported versions are {supported_protocols:?}"
        ));
    }
    let capabilities = require_object(object, "capabilities", "MCP initialize result")?;
    require_object(object, "serverInfo", "MCP initialize result")?;
    Ok((protocol.to_string(), parse_capabilities(capabilities)?))
}

fn parse_capabilities(capabilities: &Map<String, Value>) -> Result<McpCapabilities, String> {
    Ok(McpCapabilities {
        tools: capability_object(capabilities, "tools")?.is_some(),
        tools_list_changed: capability_flag(capabilities, "tools", "listChanged")?,
        resources: capability_object(capabilities, "resources")?.is_some(),
        resources_subscribe: capability_flag(capabilities, "resources", "subscribe")?,
        resources_list_changed: capability_flag(capabilities, "resources", "listChanged")?,
        prompts: capability_object(capabilities, "prompts")?.is_some(),
        prompts_list_changed: capability_flag(capabilities, "prompts", "listChanged")?,
        logging: capability_object(capabilities, "logging")?.is_some(),
        completions: capability_object(capabilities, "completions")?.is_some(),
    })
}

fn capability_object<'a>(
    capabilities: &'a Map<String, Value>,
    name: &str,
) -> Result<Option<&'a Map<String, Value>>, String> {
    match capabilities.get(name) {
        None => Ok(None),
        Some(Value::Object(value)) => Ok(Some(value)),
        Some(_) => Err(format!(
            "MCP initialize capability '{name}' is not an object"
        )),
    }
}

fn capability_flag(
    capabilities: &Map<String, Value>,
    name: &str,
    flag: &str,
) -> Result<bool, String> {
    let Some(object) = capability_object(capabilities, name)? else {
        return Ok(false);
    };
    match object.get(flag) {
        None => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(format!(
            "MCP initialize capability '{name}.{flag}' is not boolean"
        )),
    }
}

fn require_object<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    context: &str,
) -> Result<&'a Map<String, Value>, String> {
    object
        .get(field)
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{context} has no '{field}' object"))
}

fn render_server_error(error: &Value) -> Result<String, String> {
    let object = error
        .as_object()
        .ok_or_else(|| "MCP JSON-RPC error is not an object".to_string())?;
    let code = object
        .get("code")
        .and_then(Value::as_i64)
        .ok_or_else(|| "MCP JSON-RPC error has no integer code".to_string())?;
    let message = object
        .get("message")
        .and_then(Value::as_str)
        .ok_or_else(|| "MCP JSON-RPC error has no message".to_string())?;
    Ok(format!("MCP server error code={code}: {message}"))
}
