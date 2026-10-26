use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use base64::Engine;
use hmac::{Hmac, Mac};
use octopus_tool_door_runtime::{ToolProcessEnvironment, WorkerEnvironmentReleaseContext};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use octopus_local_socket::UnixStream;
use tokio::io::{ReadHalf, WriteHalf};

use crate::private_fs::{self, Kind};
use tokio::time::Instant;

use crate::DecodedRoomInvocation;

const MAX_LINE: usize = 16 * 1024 * 1024;
const MAX_RESPONSE: usize = 96 * 1024 * 1024;
const CHUNK_SIZE: usize = 8 * 1024 * 1024;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeHostConfig {
    pub host_root_slot: String,
    pub request_timeout_ms: u64,
    pub navigation_timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostManifest {
    schema_version: u32,
    host_generation: String,
    socket_path: PathBuf,
}

#[derive(Clone)]
struct Invocation {
    body: Value,
    formation: octopus_worker_env::WorkerEnvironmentFormation,
}

pub(crate) struct NativeTransport {
    credential: PathBuf,
    pub request_timeout: Duration,
    pub navigation_timeout: Duration,
    owner: tokio::sync::Mutex<Option<NativeConnection>>,
    instance: RwLock<String>,
    invocation: Arc<RwLock<Option<Arc<Invocation>>>>,
}

pub(crate) struct NativeConnection {
    reader: BufReader<ReadHalf<UnixStream>>,
    writer: WriteHalf<UnixStream>,
    key: [u8; 32],
    pub generation: String,
    challenge: String,
    target: Option<String>,
    instance: String,
    invocation: Arc<RwLock<Option<Arc<Invocation>>>>,
    next_id: i64,
    pending: Option<(i64, Arc<Invocation>)>,
    poisoned: bool,
    line: Vec<u8>,
    chunks: Option<ChunkAssembly>,
}

struct ChunkAssembly {
    id: u64,
    total: u64,
    next: u64,
    bytes: Vec<u8>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrivateCredential {
    schema_version: u32,
    host_root_slot: String,
    host_generation: String,
    socket_path: PathBuf,
    auth_key: String,
}

pub const HOST_ROOT_SLOT: &str = "browser_native_host_root";
pub const PRIVATE_CREDENTIAL: &str = "secrets/browser-native-host.auth";

impl NativeTransport {
    pub fn new(environment: &ToolProcessEnvironment, config: &NativeHostConfig) -> Result<Self, String> {
        if config.host_root_slot != HOST_ROOT_SLOT { return Err("native browser host slot does not match its owning package".into()); }
        let credential = environment.private_read_path(PRIVATE_CREDENTIAL)?.to_path_buf();
        validate_private_path(&credential, false)?;
        if !(1..=300_000).contains(&config.request_timeout_ms)
            || !(1..=300_000).contains(&config.navigation_timeout_ms) {
            return Err("native browser timeouts must be between 1 and 300000 ms".into());
        }
        Ok(Self {
            credential,
            request_timeout: Duration::from_millis(config.request_timeout_ms),
            navigation_timeout: Duration::from_millis(config.navigation_timeout_ms),
            invocation: Arc::new(RwLock::new(None)),
            owner: tokio::sync::Mutex::new(None),
            instance: RwLock::new(random_nonce()?),
        })
    }

    pub async fn authorize(&self, decoded: &DecodedRoomInvocation) -> Result<(), String> {
        if decoded.source_movement_seq == 0 || decoded.source_movement_seq > MAX_SAFE_INTEGER {
            return Err("native browser source movement sequence is outside the wire contract".into());
        }
        if self.invocation.read().map_err(|_| "native invocation lock is poisoned")?.is_some() {
            return Err("native execution owner still has an admitted invocation".into());
        }
        let mut owner = self.owner.lock().await;
        if owner.as_ref().is_some_and(|value| value.poisoned) {
            if let Some(mut old) = owner.take() { old.close().await; }
            *self.instance.write().map_err(|_| "native executor identity lock is poisoned")? = random_nonce()?;
        }
        if owner.is_none() { *owner = Some(self.open(None).await?); }
        let connection = owner.as_mut().ok_or("native execution owner is absent")?;
        let mut body = json!({
            "schema_version":2,"request_kind":"invocation",
            "executor":{"instance_nonce":self.instance.read().map_err(|_| "native executor identity lock is poisoned")?.clone(),"connection_role":"owner"},
            "tool_descriptor":base64::engine::general_purpose::STANDARD.encode(&decoded.descriptor_bytes),
            "invocation_nonce":random_nonce()?,"invocation_handle":null,
            "source_movement_seq":decoded.source_movement_seq,
            "artifact_content_hash":decoded.envelope.artifact.content.to_hex(),
            "formation":decoded.formation,
            "tool":{"id":decoded.call.tool_id,"operation":decoded.call.handler_config["operation"]},
            "arguments":decoded.call.arguments,"effective_allow_network":decoded.effective_allow_network,
        });
        let result = match connection.control(&body, "OctopusBrowser.beginInvocation", json!({}), self.request_timeout).await {
            Ok(result) => result,
            Err(error) => { connection.close().await; return Err(error); }
        };
        let Some(handle) = result.get("invocation_handle").and_then(Value::as_str).filter(|value| valid_hex(value,16)) else {
            connection.close().await; return Err("native invocation begin returned an invalid handle".into());
        };
        body["invocation_handle"] = json!(handle);
        *self.invocation.write().map_err(|_| "native invocation lock is poisoned")? = Some(Arc::new(Invocation { body, formation: decoded.formation.clone() }));
        Ok(())
    }

    pub async fn end(&self, captured_wait: Option<Value>) -> Result<(), String> {
        let invocation = self.invocation.read().map_err(|_| "native invocation lock is poisoned")?.clone().ok_or("native invocation has already ended")?;
        let mut owner = self.owner.lock().await;
        let connection = owner.as_mut().ok_or("native execution owner disappeared")?;
        let params = captured_wait.map(|value| json!({"captured_wait":value})).unwrap_or_else(|| json!({}));
        let result = connection.control(&invocation.body, "OctopusBrowser.endInvocation", params, self.request_timeout).await;
        *self.invocation.write().map_err(|_| "native invocation lock is poisoned")? = None;
        match result {
            Ok(result) if result["ended"] == true => Ok(()),
            result => { connection.close().await; Err(result.err().unwrap_or_else(|| "native invocation end was not confirmed".into())) }
        }
    }

    pub async fn release(&self, context: WorkerEnvironmentReleaseContext<'_>) -> Result<(), String> {
        if self.invocation.read().map_err(|_| "native invocation lock is poisoned")?.is_some() { return Err("native release cannot overtake an active invocation".into()); }
        let mut connection = self.open(None).await?;
        let body = json!({"schema_version":2,"request_kind":"release_formation","release_nonce":random_nonce()?,
            "formation":context.formation,"worker_environment_kind_descriptor_hash":context.formation.worker_environment_kind_descriptor_hash,
            "release_mode":context.mode});
        let result = connection.exchange(body, self.request_timeout).await?;
        connection.close().await;
        if !matches!(result.get("status").and_then(Value::as_str), Some("released" | "already_absent" | "already_released")) { return Err("native release returned an invalid lifecycle status".into()); }
        self.shutdown().await;
        Ok(())
    }

    pub async fn shutdown(&self) {
        if let Some(mut owner) = self.owner.lock().await.take() { owner.close().await; }
        if let Ok(mut invocation) = self.invocation.write() { *invocation = None; }
    }

    pub async fn connect(&self, target: Option<&str>) -> Result<NativeConnection, String> {
        if self.invocation.read().map_err(|_| "native invocation lock is poisoned")?.is_none() { return Err("native connection requires its admitted invocation".into()); }
        self.open(target).await
    }

    async fn open(&self, target: Option<&str>) -> Result<NativeConnection, String> {
        let deadline = Instant::now() + self.request_timeout;
        let bytes = read_private_file(&self.credential, 4096)?;
        let credential: PrivateCredential = serde_json::from_slice(&bytes).map_err(|_| "native browser private credential is malformed")?;
        if credential.schema_version != 1 || credential.host_root_slot != HOST_ROOT_SLOT || !valid_hex(&credential.host_generation, 16) {
            return Err("native browser private credential has invalid provenance".into());
        }
        let key: [u8; 32] = base64::engine::general_purpose::STANDARD.decode(&credential.auth_key)
            .map_err(|_| "native browser private key is malformed")?.try_into().map_err(|_| "native browser key length is invalid")?;
        let manifest = HostManifest { schema_version: 1, host_generation: credential.host_generation, socket_path: credential.socket_path };
        validate_socket_path(&manifest.socket_path)?;
        let stream = tokio::time::timeout_at(deadline, UnixStream::connect(&manifest.socket_path)).await
            .map_err(|_| "native browser host connection timed out")?
            .map_err(|_| "native browser host connection failed")?;
        let (reader, writer) = tokio::io::split(stream);
        let mut reader = BufReader::new(reader);
        let bytes = tokio::time::timeout_at(deadline, read_line(&mut reader, &mut Vec::new())).await
            .map_err(|_| "native browser host challenge timed out")??;
        let challenge: Value = serde_json::from_slice(&bytes).map_err(|_| "native browser host challenge is malformed")?;
        let challenge_value = challenge.get("challenge").and_then(Value::as_str).ok_or("native browser host challenge is missing")?;
        if challenge["type"] != "challenge" || challenge["schema_version"] != 1
            || challenge["host_generation"] != manifest.host_generation || !valid_hex(challenge_value, 16) {
            return Err("native browser host challenge identity is invalid".into());
        }
        let supplied = decode_mac(challenge.get("mac").and_then(Value::as_str).ok_or("native browser host challenge MAC is missing")?)?;
        let mut mac = HmacSha256::new_from_slice(&key).map_err(|_| "native browser MAC initialization failed")?;
        mac.update(format!("browser.native.v1.challenge\n{}\n{}", manifest.host_generation, challenge_value).as_bytes());
        mac.verify_slice(&supplied).map_err(|_| "native browser host challenge authentication failed")?;
        Ok(NativeConnection {
            reader, writer, key,
            generation: manifest.host_generation,
            challenge: challenge_value.to_string(),
            target: target.map(str::to_string),
            instance: self.instance.read().map_err(|_| "native executor identity lock is poisoned")?.clone(),
            invocation: Arc::clone(&self.invocation),
            next_id: 1, pending: None, poisoned: false,
            line: Vec::new(),
            chunks: None,
        })
    }
}

impl NativeConnection {
    pub fn is_current_owner(&self) -> bool {
        !self.poisoned && self.invocation.read().ok().and_then(|value| value.as_ref().map(|value| value.body["executor"]["instance_nonce"] == self.instance)).unwrap_or(false)
    }

    async fn control(&mut self, original: &Value, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let mut body = original.clone();
        body["executor"]["connection_role"] = json!("owner");
        body["operation"] = json!({"target_id":null,"session_id":null,"method":method,"params":params});
        self.exchange(body, timeout).await
    }

    async fn exchange(&mut self, body: Value, duration: Duration) -> Result<Value, String> {
        let id = self.next_id;
        let outcome = tokio::time::timeout(duration, async {
            self.write_body(id, body).await?;
            let response = self.next().await?;
            if response.get("id").and_then(Value::as_i64) != Some(id) { return Err("native control response changed identity".into()); }
            if let Some(error) = response.get("error") { return Err(format!("native browser control refused: {error}")); }
            response.get("result").cloned().ok_or_else(|| "native control response has no result".into())
        }).await;
        match outcome {
            Ok(value) => value,
            Err(_) => { self.close().await; Err("native browser control timed out; sent operation was not replayed".into()) }
        }
    }

    pub async fn request(&mut self, method: &str, params: Value, duration: Duration) -> Result<Value, String> {
        let id = self.next_id;
        let outcome = tokio::time::timeout(duration, async {
            self.send(id, None, method, params).await?;
            loop {
                let response = self.next().await?;
                if response.get("id").is_none() { continue; }
                if response["id"] != id { return Err("native result changed invocation request".into()); }
                if let Some(error) = response.get("error") { return Err(format!("native browser operation refused: {error}")); }
                return response.get("result").cloned().ok_or_else(|| "native response has no result".into());
            }
        }).await;
        match outcome { Ok(result) => result, Err(_) => { self.close().await; Err("native browser operation timed out; sent operation was not replayed".into()) } }
    }

    pub async fn send(&mut self, id: i64, session_id: Option<&str>, method: &str, params: Value) -> Result<(), String> {
        if !self.is_current_owner() { return Err("native connection belongs to an ended executor; no effect was dispatched".into()); }
        if self.pending.is_some() { return Err("native connection already has an in-flight command".into()); }
        let invocation = self.invocation.read().map_err(|_| "native invocation lock is poisoned")?.clone().ok_or("native browser has no admitted invocation")?;
        let mut body = invocation.body.clone();
        body["executor"]["connection_role"] = json!("member");
        body["operation"] = json!({"target_id":self.target,"session_id":session_id,"method":method,"params":params});
        self.write_body(id, body).await?;
        self.pending = Some((id, invocation));
        Ok(())
    }

    async fn write_body(&mut self, id: i64, mut body: Value) -> Result<(), String> {
        if self.poisoned { return Err("native browser connection is closed; prior sent effects are not replayed".into()); }
        if id <= 0 || id as u64 > MAX_SAFE_INTEGER || id != self.next_id { return Err("native browser request id exceeds its wire limit or counter".into()); }
        self.next_id += 1;
        body["host_generation"] = json!(self.generation);
        body["challenge"] = json!(self.challenge);
        body["request_id"] = json!(id);
        let payload = serde_json::to_vec(&body).map_err(|_| "native browser request serialization failed")?;
        let mut mac = HmacSha256::new_from_slice(&self.key).map_err(|_| "native browser MAC initialization failed")?;
        mac.update(&payload);
        let mut frame = serde_json::to_vec(&json!({
            "payload":base64::engine::general_purpose::STANDARD.encode(payload),
            "mac":hex(&mac.finalize().into_bytes()),
        })).map_err(|_| "native browser frame serialization failed")?;
        if frame.len() + 1 > MAX_LINE { return Err("native browser request exceeds the 16MiB wire limit".into()); }
        frame.push(b'\n');
        self.writer.write_all(&frame).await.map_err(|_| {
            self.poisoned = true;
            "native browser send failed; execution may have started and was not replayed".to_string()
        })
    }

    pub async fn next(&mut self) -> Result<Value, String> {
        if self.poisoned { return Err("native browser connection is closed".into()); }
        let result = self.read_response().await.and_then(|response| {
            if response.get("id").is_some() {
                if let Some((id, invocation)) = self.pending.take() {
                    let current = self.invocation.read().map_err(|_| "native invocation lock is poisoned")?;
                    if response["id"] != id || current.as_ref().is_none_or(|current| !Arc::ptr_eq(current, &invocation) || current.formation != invocation.formation) {
                        return Err("native result outlived its exact immutable invocation; sent effect was not replayed".into());
                    }
                }
            }
            Ok(response)
        });
        if result.is_err() { self.poisoned = true; }
        result
    }

    async fn read_response(&mut self) -> Result<Value, String> {
        loop {
            let bytes = read_line(&mut self.reader, &mut self.line).await?;
            let frame: Value = serde_json::from_slice(&bytes).map_err(|_| "native browser response is malformed")?;
            if frame["type"] != "response_chunk" {
                if self.chunks.is_some() { return Err("native browser response chunks were interleaved".into()); }
                return Ok(frame);
            }
            let id = frame.get("id").and_then(Value::as_u64).filter(|id| *id > 0 && *id <= MAX_SAFE_INTEGER).ok_or("native browser chunk id is invalid")?;
            let total = frame.get("total").and_then(Value::as_u64).filter(|value| *value > 0 && *value <= (MAX_RESPONSE / CHUNK_SIZE) as u64).ok_or("native browser chunk total is invalid")?;
            let assembly = self.chunks.get_or_insert_with(|| ChunkAssembly { id, total, next: 0, bytes: Vec::new() });
            if id != assembly.id || total != assembly.total || frame["index"] != assembly.next { return Err("native browser response chunks are out of order".into()); }
            let chunk = base64::engine::general_purpose::STANDARD.decode(frame.get("data").and_then(Value::as_str).ok_or("native browser chunk has no data")?).map_err(|_| "native browser chunk is not base64")?;
            if chunk.is_empty() || chunk.len() > CHUNK_SIZE || (assembly.next + 1 < total && chunk.len() != CHUNK_SIZE) || assembly.bytes.len() + chunk.len() > MAX_RESPONSE {
                return Err("native browser chunk exceeds its byte contract".into());
            }
            assembly.bytes.extend_from_slice(&chunk);
            assembly.next += 1;
            if assembly.next == total {
                let completed = self.chunks.take().ok_or("native browser response assembly disappeared")?;
                let response: Value = serde_json::from_slice(&completed.bytes).map_err(|_| "native browser assembled response is malformed")?;
                if response["id"] != completed.id { return Err("native browser assembled response changed request id".into()); }
                return Ok(response);
            }
        }
    }

    pub async fn close(&mut self) { self.poisoned = true; let _ = self.writer.shutdown().await; }
}

async fn read_line(reader: &mut BufReader<ReadHalf<UnixStream>>, bytes: &mut Vec<u8>) -> Result<Vec<u8>, String> {
    loop {
        let available = reader.fill_buf().await.map_err(|_| "native browser read failed; sent effects were not replayed")?;
        if available.is_empty() { return Err("native browser host disconnected; a sent effect may have executed and was not replayed".into()); }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let count = newline.map(|position| position + 1).unwrap_or(available.len());
        if bytes.len() + count > MAX_LINE { return Err("native browser frame exceeds 16MiB".into()); }
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count);
        if newline.is_some() { bytes.pop(); return Ok(std::mem::take(bytes)); }
    }
}

fn validate_private_path(path: &Path, directory: bool) -> Result<(), String> {
    if !path.is_absolute() { return Err("native browser private path must be absolute".into()); }
    let kind = if directory { Kind::Directory } else { Kind::File };
    if !private_fs::is_private(path, kind).map_err(|_| "native browser private path is unavailable")? {
        return Err("native browser path must be an owner-only regular file or directory".into());
    }
    if !private_fs::is_canonical(path).map_err(|_| "native browser private path cannot be resolved")? {
        return Err("native browser private path must not contain symlinks".into());
    }
    Ok(())
}

fn validate_socket_path(path: &Path) -> Result<(), String> {
    validate_private_path(path.parent().ok_or("native browser socket has no parent")?, true)?;
    #[cfg(unix)]
    if !private_fs::is_private(path, Kind::Socket).map_err(|_| "native browser socket is unavailable")? {
        return Err("native browser socket must be owner-only and must not be a symlink".into());
    }
    // Windows: the socket is a named pipe with no file at `path`. Connecting
    // refuses a pipe whose server is not running as the current user.
    Ok(())
}

fn valid_hex(value: &str, bytes: usize) -> bool {
    value.len() == bytes * 2 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn decode_mac(value: &str) -> Result<Vec<u8>, String> {
    if !valid_hex(value, 32) { return Err("native browser MAC is malformed".into()); }
    value.as_bytes().chunks_exact(2).map(|pair| {
        u8::from_str_radix(std::str::from_utf8(pair).map_err(|_| "native browser MAC is malformed")?, 16)
            .map_err(|_| "native browser MAC is malformed".to_string())
    }).collect()
}

fn random_nonce() -> Result<String, String> { let mut bytes = [0u8;16]; getrandom::fill(&mut bytes).map_err(|_| "native browser nonce unavailable")?; Ok(hex(&bytes)) }

fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }

fn read_private_file(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    validate_private_path(path, false)?;
    let file = private_fs::open_read_no_follow(path).map_err(|_| "native browser private file cannot be opened")?;
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes).map_err(|_| "native browser private file cannot be read")?;
    if bytes.len() as u64 > max { return Err("native browser private file exceeds its byte limit".into()); }
    Ok(bytes)
}

pub fn prepare_private_native_host(roots: &std::collections::BTreeMap<String, PathBuf>) -> Result<std::collections::BTreeMap<String, PathBuf>, String> {
    use std::io::Write;
    let root = roots.get(HOST_ROOT_SLOT).ok_or("browser door has no native host slot")?;
    validate_private_path(root, true)?;
    let interface = roots.get("activation_secrets_home").ok_or("browser door has no host-formed private interface root")?;
    validate_private_path(interface, true)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let manifest_path = root.join("host.json");
    let key_path = root.join("auth.key");
    while !manifest_path.try_exists().map_err(|_| "native host manifest cannot be inspected")? || !key_path.try_exists().map_err(|_| "native host key cannot be inspected")? {
        if std::time::Instant::now() >= deadline { return Err("native host rendezvous was not published before the browser door deadline".into()); }
        std::thread::sleep(Duration::from_millis(25));
    }
    let manifest_bytes = read_private_file(&manifest_path, 4096)?;
    let manifest: HostManifest = serde_json::from_slice(&manifest_bytes).map_err(|_| "native host manifest is malformed")?;
    let key = read_private_file(&key_path, 32)?;
    if key.len() != 32 || manifest.schema_version != 1 || !valid_hex(&manifest.host_generation, 16) || read_private_file(&manifest_path, 4096)? != manifest_bytes {
        return Err("native browser credential publication changed or is malformed".into());
    }
    let bundle = serde_json::to_vec(&json!({
        "schema_version":1,"host_root_slot":HOST_ROOT_SLOT,"host_generation":manifest.host_generation,
        "socket_path":manifest.socket_path,"auth_key":base64::engine::general_purpose::STANDARD.encode(key),
    })).map_err(|_| "native browser private bundle cannot encode")?;
    if bundle.len() > 4096 { return Err("native browser private bundle exceeds owning door limit".into()); }
    let secrets = interface.join("secrets");
    if !secrets.try_exists().map_err(|_| "browser door secret directory cannot be inspected")? {
        match private_fs::create_private_dir(&secrets) {
            Ok(()) => {}, Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}, Err(_) => return Err("browser door secret directory cannot be created".into()),
        }
    }
    validate_private_path(&secrets, true)?;
    let target = interface.join(PRIVATE_CREDENTIAL);
    if target.try_exists().map_err(|_| "browser door private bundle cannot be inspected")? { validate_private_path(&target, false)?; }
    let mut nonce = [0u8;16];
    getrandom::fill(&mut nonce).map_err(|_| "browser private publication nonce is unavailable")?;
    let temporary = secrets.join(format!(".browser-native-host-{}", hex(&nonce)));
    let mut file = private_fs::create_private_file(&temporary).map_err(|_| "browser private publication cannot open")?;
    let result = (|| {
        file.write_all(&bundle).map_err(|_| "browser private publication cannot write")?;
        file.sync_all().map_err(|_| "browser private publication cannot sync")?;
        std::fs::rename(&temporary, &target).map_err(|_| "browser private publication cannot commit")?;
        std::fs::File::open(&secrets).and_then(|file| file.sync_all()).map_err(|_| "browser private directory cannot sync")?;
        Ok(roots.clone())
    })();
    if result.is_err() { let _ = std::fs::remove_file(&temporary); }
    result
}
