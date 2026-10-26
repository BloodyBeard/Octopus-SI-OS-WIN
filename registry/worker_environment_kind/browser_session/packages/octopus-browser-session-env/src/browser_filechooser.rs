use super::*;
use crate::browser_consent::{self as consent, Offer, BrowserOriginConsentPolicy};
pub(crate) use crate::browser_consent::{Response, Decision};
use octopus_descriptors::InterfaceId;
use octopus_interfaces::{ArtifactRef, InterfaceInvocationEnvelope};
use octopus_tool_handlers::{ToolCallArtifact, ToolResultArtifact, ToolResultError};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::io::Read;
use crate::private_fs::{self, Kind};
use std::path::{Path, PathBuf};

pub(crate) const RESPONSE: &str = "operator.approval.response";
const QUOTA: usize = 268_435_456;
const RECORD_LIMIT: usize = 16 * 1024 * 1024;
const CHUNK: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct UploadApproval { pub request_tools: Vec<String>, pub operator_interface: InterfaceId, pub resume_tool: String, #[serde(default, skip_serializing_if = "Option::is_none")] pub permission_policy: Option<BrowserOriginConsentPolicy> }
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct FileSelection { artifact_ref: ArtifactRef, filename: String }
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct UploadFile { artifact_ref: ArtifactRef, filename: String, payload_sha256: String, size_bytes: u64 }
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Upload { browser_context_id: String, target_id: String, reservation_id: String, file_chooser_id: String, document_token: String, backend_node_id: u64, top_level_origin: String, files: Vec<UploadFile> }
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Scope { worker_environment_id: String, formation_pin: String, host_generation: String }
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct OriginalCall { run: String, ee: String, worker: InterfaceId, door_interface: InterfaceId, tool: String, correlation_id: String, arguments: Value, source_movement_seq: u64, source_artifact: ArtifactRef }
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Resume { tool: String, correlation_id: String }
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Request { schema_version: u32, request_id: String, call_id: String, original: OriginalCall, scope: Scope, approval_scope: String, upload: Upload, resume: Resume, #[serde(default, skip_serializing_if = "Option::is_none")] permission_offer: Option<Offer> }
impl Request {
    fn digest(&self) -> String {
        let mut body = json!({"schema_version":self.schema_version,"original":self.original,"scope":self.scope,"approval_scope":self.approval_scope,"upload":self.upload});
        if let Some(offer) = &self.permission_offer { body["permission_offer"] = json!(offer); }
        hash(&octopus_descriptors::canonical_bytes(&body))
    }
    fn validate(&self) -> Result<(), String> {
        let digest = self.digest();
        if self.schema_version != 1 || self.approval_scope != "browser_file_upload" || self.request_id != format!("sha256:{digest}") || self.resume.correlation_id != format!("browser-upload-resume:{digest}")
            || self.call_id != self.original.correlation_id || self.call_id.is_empty() || self.original.source_movement_seq == 0 || self.original.source_movement_seq > 9_007_199_254_740_991
            || self.upload.backend_node_id == 0 || self.upload.backend_node_id > 9_007_199_254_740_991 || self.upload.files.is_empty() {
            return Err("Browser upload immutable request is invalid".into());
        }
        for id in [&self.upload.reservation_id, &self.upload.file_chooser_id, &self.upload.document_token] { native_id(id)?; }
        validate_target_id(&self.upload.target_id)?;
        let origin = reqwest::Url::parse(&self.upload.top_level_origin).map_err(|error| error.to_string())?;
        if !matches!(origin.scheme(), "http" | "https") || origin.origin().ascii_serialization() != self.upload.top_level_origin { return Err("Browser upload has no exact observed HTTP(S) origin".into()); }
        if let Some(offer) = &self.permission_offer { offer.validate()?; }
        let mut bytes = 0u64;
        for file in &self.upload.files { validate_filename(&file.filename)?; digest_id(&format!("sha256:{}",file.payload_sha256))?; bytes = bytes.checked_add(file.size_bytes).ok_or("Upload byte size overflow")?; }
        if bytes > QUOTA as u64 { return Err("Upload exceeds room staging quota".into()); }
        Ok(())
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Pending { request: Request, formation: WorkerEnvironmentFormation, operator_interface: InterfaceId, provider_interface: InterfaceId }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseBody { operator_approval_response: Response }
#[derive(Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Consumption { response_artifact: ArtifactRef, decision: Decision }

fn hash(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }
pub(crate) fn digest_id(id: &str) -> Result<&str, String> { id.strip_prefix("sha256:").filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))).ok_or("Upload request must carry an exact SHA256 identity".into()) }
fn native_id(id: &str) -> Result<(), String> { if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) { return Err("File chooser requires an exact native identity".into()); } Ok(()) }
fn validate_filename(value: &str) -> Result<(), String> { if value.is_empty() || value.len() > 255 || matches!(value,"."|"..") || (value.contains('\0') || value.contains('/') || value.contains('\\')) { return Err("Upload filename must be one bounded basename".into()); } Ok(()) }
pub(crate) fn response(envelope: &InterfaceInvocationEnvelope) -> Result<Response, String> {
    if envelope.read_artifact_manifest().map_err(|error| error.to_string())?.label != RESPONSE { return Err("Browser upload resume requires the original operator response".into()); }
    let body: ResponseBody = serde_json::from_slice(&envelope.read_artifact_payload_bounded(65536).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
    digest_id(&body.operator_approval_response.request_id)?;
    if body.operator_approval_response.call_id.is_empty() || body.operator_approval_response.call_id.trim() != body.operator_approval_response.call_id { return Err("Upload response lost its exact call identity".into()); }
    Ok(body.operator_approval_response)
}
pub(crate) fn decoded_response(envelope: &InterfaceInvocationEnvelope, tool: &str) -> Result<ToolCallArtifact, String> {
    let value = response(envelope)?;
    Ok(ToolCallArtifact { tool: tool.into(), arguments: json!({"request_id":value.request_id}), correlation_id: Some(format!("browser-upload-resume:{}",digest_id(&value.request_id)?)), label: None, source_interface_evidence: BTreeMap::new() })
}
pub fn decode_filechooser_door_artifact(ctx: &octopus_tool_door_runtime::InterfaceLiftContext, envelope: &InterfaceInvocationEnvelope, purpose: octopus_tool_door_runtime::ToolDoorArtifactDecodePurpose) -> Result<Option<ToolCallArtifact>, octopus_tool_door_runtime::ToolDoorArtifactDecodeError> {
    let file = ctx.interface_registry_dir().join("interface.json");
    let mut input = private_fs::open_read_no_follow(&file).map_err(|error| error.to_string())?;
    let stat = input.metadata().map_err(|error| error.to_string())?;
    if !stat.is_file() || stat.len() > RECORD_LIMIT as u64 { return Err("Browser decoder descriptor is not a bounded regular file".into()); }
    let mut bytes = Vec::new(); Read::by_ref(&mut input).take(RECORD_LIMIT as u64 + 1).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
    if bytes.len() > RECORD_LIMIT { return Err("Browser decoder descriptor exceeds its byte bound".into()); }
    let descriptor: octopus_interfaces::InterfaceDescriptor = octopus_descriptors::parse_strict(&bytes).map_err(|error| error.to_string())?;
    if descriptor.id != *ctx.owner_interface() { return Err("Browser decoder descriptor changed its exact owner".into()); }
    let config = descriptor.interface_driver_config.as_ref().and_then(|value| value.private_config.get("browser_room"));
    let approval = config.and_then(|value| value.get("upload_approval")).filter(|value| !value.is_null())
        .map(|value| serde_json::from_value::<UploadApproval>(value.clone()).map_err(|error| error.to_string())).transpose()?;
    let history = config.and_then(|value| value.get("history_access")).filter(|value| !value.is_null())
        .map(|value| serde_json::from_value::<history::HistoryAccess>(value.clone()).map_err(|error| error.to_string())).transpose()?;
    if envelope.read_artifact_manifest().map_err(|error| error.to_string())?.label == RESPONSE {
        ctx.source_movement(envelope)?;
        let response = response(envelope)?;
        let runtime = envelope.runtime().map_err(|error| error.to_string())?;
        let parent = ctx.interface_var_dir().join(".tool-process-host-state/worker-environment-state");
        for path in [ctx.interface_var_dir(),ctx.interface_var_dir().join(".tool-process-host-state"),parent.clone()] {
            let stat=std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
            if !stat.is_dir() || stat.file_type().is_symlink() || path != ctx.interface_var_dir() && !private_fs::is_owner_only(&path).map_err(|error| error.to_string())?
                || !private_fs::is_canonical(&path).map_err(|error| error.to_string())? {return Err("Browser pending parent is not an exact private directory".into());}
        }
        let mut matched = None;
        let entries = std::fs::read_dir(&parent).map_err(|error| error.to_string())?;
        let mut count = 0usize;
        for entry in entries {
            let path = entry.map_err(|error| error.to_string())?.path();
            count += 1; if count > 65536 { return Err("Browser pending lookup exceeds its finite owner inventory".into()); }
            let stat = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
            if !stat.is_dir() || stat.file_type().is_symlink() || !private_fs::is_owner_only(&path).map_err(|error| error.to_string())? || !private_fs::is_canonical(&path).map_err(|error| error.to_string())? { return Err("Browser pending owner path is not canonical".into()); }
            for (directory, expected_scope, config) in [
                ("upload-pending","browser_file_upload",approval.as_ref().map(|c| (&c.request_tools,&c.resume_tool,&c.operator_interface))),
                ("history-pending","browser_history_read",history.as_ref().map(|c| (&c.request_tools,&c.resume_tool,&c.operator_interface)))
            ] {
                let Some((tools,resume,operator)) = config else { continue; };
                let pending_dir=path.join(directory);
                match std::fs::symlink_metadata(&pending_dir) {
                    Ok(stat) if stat.is_dir() && !stat.file_type().is_symlink() && private_fs::is_private(&pending_dir, Kind::Directory).map_err(|error| error.to_string())?
                        && private_fs::is_canonical(&pending_dir).map_err(|error| error.to_string())? => {},
                    Err(error) if error.kind()==std::io::ErrorKind::NotFound => continue,
                    _ => return Err("Browser pending request directory changed its private owner".into())
                }
                let file=pending_dir.join(format!("{}.request.json",digest_id(&response.request_id)?));
                if !file.try_exists().map_err(|error| error.to_string())? {continue;}
                let pending:Value=read_record(&file)?;let request=&pending["request"];let original=&request["original"];
                if request["request_id"] != response.request_id || request["call_id"] != response.call_id
                    || request["approval_scope"] != expected_scope || request["resume"]["tool"] != *resume
                    || !original["tool"].as_str().is_some_and(|tool| tools.iter().any(|id| id==tool))
                    || original["run"] != runtime.run.as_str() || original["ee"] != runtime.ee.as_str()
                    || original["worker"] != runtime.worker.as_str() || original["door_interface"] != ctx.owner_interface().as_str()
                    || pending["operator_interface"] != operator.as_str() {return Err("Browser response differs from its retained exact original request".into());}
                if matched.replace((resume.clone(),expected_scope)).is_some() {return Err("Browser response matches more than one retained owner request".into());}
            }
        }
        let (tool,scope)=matched.ok_or("Browser response has no retained original request")?;
        return if scope == "browser_history_read" { history::decoded_response(envelope,&tool).map(Some).map_err(Into::into) }
            else { decoded_response(envelope,&tool).map(Some).map_err(Into::into) };
    }
    let call = octopus_tool_door_runtime::default_tool_door_artifact_decoder(ctx, envelope, purpose)?;
    if call.as_ref().is_some_and(|call| approval.as_ref().is_some_and(|approval| approval.resume_tool == call.tool)
        || history.as_ref().is_some_and(|history| history.resume_tool == call.tool)) {
        return Err("Browser resume is private to the verifier-admitted human response".into());
    }
    Ok(call)
}
fn admitted_bytes<'a>(envelope: &'a InterfaceInvocationEnvelope, selected: &FileSelection) -> Result<Cow<'a, [u8]>, String> {
    validate_filename(&selected.filename)?;
    let metadata = envelope.metadata().map_err(|error| error.to_string())?;
    if !metadata.artifact_binding_refs.iter().filter(|item| item.id == selected.artifact_ref.id).eq([&selected.artifact_ref]) || !metadata.grant.artifact_access.get(selected.artifact_ref.id.as_str()).is_some_and(|access| access.read) { return Err("Upload artifact is not currently bound with read authority".into()); }
    let mut matches = envelope.artifact_binding_payloads.iter().filter(|item| item.artifact.id == selected.artifact_ref.id);
    let payload = matches.next().ok_or("Upload artifact has no captured bound payload")?;
    if matches.next().is_some() || payload.artifact != selected.artifact_ref || payload.payload_len() > QUOTA as u64 { return Err("Upload bound payload differs from its admitted ref or byte bound".into()); }
    payload.read_bounded(QUOTA as u64).map_err(|error| error.to_string())
}
pub(crate) fn private_directory(root: &Path, parts: &[&str]) -> Result<PathBuf, String> {
    let mut path = root.to_path_buf();
    for part in std::iter::once(&"").chain(parts) {
        if !part.is_empty() { path.push(part); match private_fs::create_private_dir(&path) { Ok(()) => {}, Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}, Err(error) => return Err(error.to_string()) } }
        if !private_fs::is_private(&path, Kind::Directory).map_err(|error| error.to_string())? || !private_fs::is_canonical(&path).map_err(|error| error.to_string())? { return Err("Browser pending storage must be canonical owned mode0700 directories".into()); }
    }
    Ok(path)
}
pub(crate) fn read_record<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    read_record_bounded(path, RECORD_LIMIT)
}
pub(crate) fn read_record_bounded<T: for<'de> Deserialize<'de>>(path: &Path, limit: usize) -> Result<T, String> {
    let mut file = private_fs::open_read_no_follow(path).map_err(|error| error.to_string())?;
    let stat = file.metadata().map_err(|error| error.to_string())?;
    if !private_fs::is_private_single_link_file(&file).map_err(|error| error.to_string())? || stat.len() > limit as u64 { return Err("Browser pending record ownership or byte bound is invalid".into()); }
    let mut bytes = Vec::new(); Read::by_ref(&mut file).take(limit as u64 + 1).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
    if bytes.len() > limit { return Err("Browser pending record exceeds its bound".into()); }
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}
pub(crate) fn create_record(path: &Path, value: &impl Serialize) -> Result<(), String> {
    create_record_bounded(path, value, RECORD_LIMIT)
}
pub(crate) fn create_record_bounded(path: &Path, value: &impl Serialize, limit: usize) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if bytes.len() > limit { return Err("Browser pending record exceeds its bound".into()); }
    let mut file = private_fs::create_private_file(path).map_err(|error| error.to_string())?;
    file.write_all(&bytes).and_then(|()| file.sync_all()).map_err(|error| error.to_string())?;
    octopus_private_fs::sync_directory(path.parent().ok_or("Browser pending record has no parent")?).map_err(|error| error.to_string())
}

impl BrowserRoom {
    pub(super) async fn filechooser(&self, decoded: &DecodedRoomInvocation, recorder: &dyn WorkerEnvironmentRecorder) -> Result<Value, String> {
        if !matches!(self.transport,BrowserTransport::Native(_)) { return Err("File choosers require the declared owned native browser transport".into()); }
        if decoded.operation == BrowserOperation::FilechooserResume {
            let config = self.upload_approval.as_ref().ok_or("This browser door declares no upload approval route")?;
            if self.max_upload_staged_bytes != Some(QUOTA) { return Err("Browser upload requires its explicit aggregate staging quota".into()); }
            return self.resume_upload(decoded,config).await;
        }
        let tool = decoded.call.tool_id.as_str();
        if decoded.operation == BrowserOperation::FilechooserSetFiles {
            let config = self.upload_approval.as_ref().ok_or("This browser door declares no upload approval route")?;
            if self.max_upload_staged_bytes != Some(QUOTA) { return Err("Browser upload requires its explicit aggregate staging quota".into()); }
            if config.request_tools.iter().filter(|id| id.as_str() == tool).count() != 1
                || config.request_tools.iter().collect::<std::collections::BTreeSet<_>>().len() != config.request_tools.len()
                || config.request_tools.iter().any(|id| id.is_empty() || id == &config.resume_tool) {
                return Err("Upload request tool is not an exact declared approval dependency".into());
            }
            let grant = decoded.envelope.grant().map_err(|error| error.to_string())?;
            if ["operator.approval", "operator.input.provided"].into_iter().any(|mark| !grant.mark_outcomes.iter().any(|value| value == mark)) {
                return Err("Browser upload requires both declared human approval and decline mark outcomes".into());
            }
        }
        let fields: &[&str] = match decoded.operation { BrowserOperation::FilechooserArm => &["target_id"], BrowserOperation::FilechooserWait => &["reservation_id","timeout_ms"], BrowserOperation::FilechooserCancel => &["reservation_id"], BrowserOperation::FilechooserSetFiles => &["reservation_id","file_chooser_id","files"], _ => return Err("Unknown file chooser operation".into()) };
        let object = strict_object(tool,&decoded.call.arguments,fields)?;
        let room = self.room(&decoded.formation).await?;
        let mut state = room.lock().await;
        if decoded.operation == BrowserOperation::FilechooserArm {
            let (_, page) = self.ensure_session(&mut state,false,decoded,recorder).await?;
            return page.session.as_mut().ok_or("File chooser has no exact page")?.command(tool,"OctopusBrowser.armFileChooser",json!({})).await;
        }
        let reservation = required_string(object,"reservation_id")?; native_id(reservation)?;
        self.ensure_browser_context(&mut state,false).await?;
        if decoded.operation == BrowserOperation::FilechooserWait {
            let duration = bounded_optional_u64(object,"timeout_ms",0,3000)?.unwrap_or(3000);
            return self.browser_command(&mut state,"OctopusBrowser.waitFileChooser",json!({"reservation_id":reservation,"timeout_ms":duration})).await;
        }
        if decoded.operation == BrowserOperation::FilechooserCancel { return self.browser_command(&mut state,"OctopusBrowser.cancelFileChooser",json!({"reservation_id":reservation})).await; }
        let config = self.upload_approval.as_ref().ok_or("This browser door declares no upload approval route")?;
        let chooser = required_string(object,"file_chooser_id")?; native_id(chooser)?;
        let selected: Vec<FileSelection> = serde_json::from_value(object.get("files").cloned().ok_or("File selection is missing")?).map_err(|error| error.to_string())?;
        if selected.is_empty() { return Err("File chooser requires at least one selected artifact".into()); }
        let mut total = 0usize; let mut files = Vec::new();
        for selection in &selected { let bytes = admitted_bytes(&decoded.envelope,selection)?; total = total.checked_add(bytes.len()).ok_or("Upload byte count overflow")?; if total > QUOTA { return Err("Upload exceeds room staging quota".into()); } files.push(UploadFile { artifact_ref:selection.artifact_ref.clone(), filename:selection.filename.clone(), payload_sha256:hash(&bytes), size_bytes:bytes.len() as u64 }); }
        let observed = self.browser_command(&mut state,"OctopusBrowser.inspectFileChooser",json!({"reservation_id":reservation,"file_chooser_id":chooser})).await?;
        let context = state.browser_context_id.as_deref().ok_or("Upload has no exact browser context")?;
        let generation = &state.native_identity.as_ref().ok_or("Upload has no native host identity")?.host_generation;
        if observed["browser_context_id"] != context || observed["host_generation"] != *generation || observed["reservation_id"] != reservation || observed["file_chooser_id"] != chooser { return Err("File chooser receipt changed its exact room or event".into()); }
        let metadata = decoded.envelope.metadata().map_err(|error| error.to_string())?;
        let bindings = metadata.checkpoint_binding_policy.as_ref().ok_or("Upload request has no current binding snapshot")?;
        if !metadata.grant.interfaces.contains(&config.operator_interface.to_string()) || !metadata.grant.tools.contains(&config.resume_tool) || metadata.grant.hard_blocked_tools.contains(&config.resume_tool) || !decoded.formation.granted_tools.contains(&config.resume_tool) { return Err("Upload operator and private resume must survive exact formed authority".into()); }
        let original = OriginalCall { run:decoded.formation.run.clone(), ee:decoded.formation.ee.clone(), worker:decoded.formation.worker.clone(), door_interface:decoded.formation.door_interface.clone(), tool:tool.into(), correlation_id:decoded.correlation_id.clone().ok_or("Upload needs its original call correlation")?, arguments:decoded.call.arguments.clone(), source_movement_seq:decoded.source_movement_seq, source_artifact:decoded.envelope.artifact.clone() };
        let mut request = Request { schema_version:1, request_id:String::new(), call_id:original.correlation_id.clone(), original, scope:Scope { worker_environment_id:decoded.formation.worker_environment_id.to_string(), formation_pin:decoded.formation.formation_pin.clone(), host_generation:generation.clone() }, approval_scope:"browser_file_upload".into(), upload:Upload { browser_context_id:context.into(), target_id:observed["target_id"].as_str().ok_or("File chooser omitted actual target")?.into(), reservation_id:reservation.into(), file_chooser_id:chooser.into(), document_token:observed["document_token"].as_str().ok_or("File chooser omitted document identity")?.into(), backend_node_id:observed["backend_node_id"].as_u64().ok_or("File chooser omitted native node")?, top_level_origin:observed["top_level_origin"].as_str().ok_or("File chooser omitted actual origin")?.into(), files }, resume:Resume { tool:config.resume_tool.clone(), correlation_id:String::new() }, permission_offer:None };
        request.permission_offer = config.permission_policy.as_ref().map(|policy| policy.offer(&config.operator_interface,&request.upload.top_level_origin)).transpose()?;
        let digest = request.digest(); request.request_id = format!("sha256:{digest}"); request.resume.correlation_id = format!("browser-upload-resume:{digest}"); request.validate()?;
        let prepared_result: Result<Value, String> = async {
        self.browser_command(&mut state,"OctopusBrowser.stageFileChooser",json!({"reservation_id":reservation,"request":request,"max_staged_bytes":QUOTA})).await?;
        for (index, selection) in selected.iter().enumerate() {
            let bytes = admitted_bytes(&decoded.envelope,selection)?;
            for (part, chunk) in bytes.chunks(CHUNK).enumerate() {
                let offset = part * CHUNK;
                let value = self.browser_command(&mut state,"OctopusBrowser.writeFileChooser",json!({"reservation_id":reservation,"request_id":request.request_id,"index":index,"offset":offset,"payload_base64":base64::engine::general_purpose::STANDARD.encode(chunk)})).await?;
                if value["offset"].as_u64() != Some((offset + chunk.len()) as u64) || value["index"].as_u64() != Some(index as u64) { return Err("Upload chunk receipt changed its exact byte position".into()); }
            }
        }
        let prepared = self.browser_command(&mut state,"OctopusBrowser.prepareFileChooser",json!({"reservation_id":reservation,"request_id":request.request_id})).await?;
        if prepared["status"] != "prepared" || prepared["request_id"] != request.request_id { return Err("Native file chooser did not prepare the exact upload".into()); }
        let pending = Pending { request:request.clone(), formation:decoded.formation.clone(), operator_interface:config.operator_interface.clone(), provider_interface:bindings.inbound.interface.clone() };
        let root = private_directory(&self.state_dir,&["upload-pending"])?;
        create_record(&root.join(format!("{digest}.request.json")),&pending)?;
        if request.permission_offer.is_some() {
            let permission=self.browser_command(&mut state,"OctopusBrowser.lookupPermission",json!({"request_id":request.request_id})).await?;
            if !permission.is_null() {
                let value=self.browser_command(&mut state,"OctopusBrowser.applyFileChooser",json!({"reservation_id":reservation,"request_id":request.request_id,"decision":permission["response"]["decision"],"permission_entry_id":permission["id"]})).await?;
                if value["status"] != "applied" || value["effect_performed"] != true { return Err(format!("Saved-consent upload did not complete: {value}")); }
                return Ok(json!({"effect_performed":true,"upload":request.upload,"native_result":value,"permission_reuse":permission}));
            }
        }
        Ok(json!({"result":tool,"effect_performed":false,"worker_environment_id":decoded.formation.worker_environment_id,"target_id":request.upload.target_id,"operator_request":request}))
        }.await;
        match prepared_result {
            Ok(value) => Ok(value),
            Err(mut error) => {
                if let Err(cleanup) = self.browser_command(&mut state,"OctopusBrowser.abandonFileChooser",json!({"reservation_id":reservation,"request_id":request.request_id})).await { error.push_str(&format!("; staging cleanup unconfirmed: {cleanup}")); }
                Err(error)
            }
        }
    }
    async fn resume_upload(&self, decoded: &DecodedRoomInvocation, config: &UploadApproval) -> Result<Value,String> {
        let response = response(&decoded.envelope)?;
        let digest = digest_id(&response.request_id)?;
        let root = private_directory(&self.state_dir,&["upload-pending"])?;
        let pending: Pending = read_record(&root.join(format!("{digest}.request.json")))?;
        let request = &pending.request; request.validate()?;
        let metadata = decoded.envelope.metadata().map_err(|error| error.to_string())?;
        let runtime = decoded.envelope.runtime().map_err(|error| error.to_string())?;
        let bindings = metadata.checkpoint_binding_policy.as_ref().ok_or("Upload approval lost its current binding snapshot")?;
        let admission = metadata.checkpoint_admission.as_ref().ok_or("Upload approval has no checkpoint admission")?;
        let (mark,verifier) = match response.decision { Decision::Accept => ("operator.approval","operator_approval"), Decision::Decline | Decision::Cancel => ("operator.input.provided","operator_input") };
        if decoded.source_movement_seq <= request.original.source_movement_seq || pending.formation != decoded.formation || response.request_id != request.request_id || response.call_id != request.call_id || request.resume.tool != decoded.call.tool_id || request.resume.tool != config.resume_tool || pending.operator_interface != config.operator_interface
            || runtime.interface != pending.formation.door_interface || runtime.worker != request.original.worker || runtime.run.as_str() != request.original.run || runtime.ee.as_str() != request.original.ee
            || bindings.inbound.interface != pending.operator_interface || !metadata.grant.interfaces.contains(&pending.operator_interface.to_string()) || !metadata.grant.interfaces.contains(&pending.formation.door_interface.to_string()) || !admission.complete || admission.artifact_label != RESPONSE || admission.required_artifact_labels != [RESPONSE]
            || metadata.verification_failure.is_some() || metadata.required_verified_artifacts.len() != 1 || metadata.required_verified_artifacts[0].mark.as_str() != mark || metadata.required_verified_artifacts[0].verifier.as_str() != verifier || metadata.required_verified_artifacts[0].artifact_label != RESPONSE
            || metadata.required_verified_artifacts[0].params != consent::params(&request.request_id,&request.call_id,request.permission_offer.as_ref()) || !metadata.grant.mark_outcomes.iter().any(|value| value == mark)
            || metadata.outbound_interface_bindings != [pending.provider_interface.clone(),pending.formation.door_interface.clone()] || bindings.outbound.iter().map(|binding| &binding.interface).collect::<Vec<_>>() != [&pending.provider_interface,&pending.formation.door_interface]
            || [&request.original.tool,&request.resume.tool].into_iter().any(|tool| !metadata.grant.tools.contains(tool) || metadata.grant.hard_blocked_tools.contains(tool)) {
            return Err("Upload decision differs from the exact verifier-admitted request and formed room".into());
        }
        let current_offer=config.permission_policy.as_ref().map(|policy| policy.offer(&config.operator_interface,&request.upload.top_level_origin)).transpose()?;
        consent::validate_offer(request.permission_offer.as_ref(),current_offer.as_ref())?;
        response.validate(&request.request_id,&request.call_id,request.permission_offer.as_ref())?;
        let consumption = Consumption { response_artifact:decoded.envelope.artifact.clone(), decision:response.decision };
        let consumed = root.join(format!("{digest}.consumed.json")); let result_path = root.join(format!("{digest}.result.json"));
        if consumed.try_exists().map_err(|error| error.to_string())? {
            if read_record::<Consumption>(&consumed)? != consumption { return Err("Upload request already consumed a different response".into()); }
            if result_path.try_exists().map_err(|error| error.to_string())? { return read_record(&result_path); }
            let original = ToolResultArtifact::failure(&request.original.tool,Some(request.call_id.clone()),ToolResultError::new("upload_execution_uncertain","Upload consumption was recorded without a completed result; the native effect will not be repeated"));
            return Ok(json!({"request_id":request.request_id,"original_result":original}));
        }
        create_record(&consumed,&consumption)?;
        let mut applied = async {
            if response.decision == Decision::Accept {
                for file in &request.upload.files { let selection = FileSelection { artifact_ref:file.artifact_ref.clone(),filename:file.filename.clone() }; let bytes = admitted_bytes(&decoded.envelope,&selection)?; if bytes.len() as u64 != file.size_bytes || hash(&bytes) != file.payload_sha256 { return Err("Approved upload lost its exact currently admitted file bytes".into()); } }
            }
            let room = self.room(&decoded.formation).await?; let mut state = room.lock().await;
            self.ensure_browser_context(&mut state,false).await?;
            if state.browser_context_id.as_deref() != Some(request.upload.browser_context_id.as_str()) || state.native_identity.as_ref().map(|value| value.host_generation.as_str()) != Some(request.scope.host_generation.as_str()) { return Err("Upload belongs to a retired native context or host generation".to_string()); }
            let saved = if request.permission_offer.is_some() {
                match self.browser_command(&mut state,"OctopusBrowser.rememberPermission",json!({"request_id":request.request_id,"response":response,"source_artifact":decoded.envelope.artifact})).await {
                    Ok(value)=>value, Err(error)=>json!({"status":"not_saved","error":error})
                }
            } else {json!({"status":"not_saved"})};
            let mut result = self.browser_command(&mut state,"OctopusBrowser.applyFileChooser",json!({"reservation_id":request.upload.reservation_id,"request_id":request.request_id,"decision":response.decision})).await?;
            result["permission_persistence"]=saved; Ok(result)
        }.await;
        if let Err(error) = &mut applied {
            let room = self.room(&decoded.formation).await?; let mut state = room.lock().await;
            if state.browser.is_some() {
                if let Err(cleanup) = self.browser_command(&mut state,"OctopusBrowser.abandonFileChooser",json!({"reservation_id":request.upload.reservation_id,"request_id":request.request_id})).await {
                    error.push_str(&format!("; unapplied staging cleanup unconfirmed: {cleanup}"));
                }
            }
        }
        let original = match applied {
            Ok(value) if response.decision == Decision::Accept && value["status"] == "applied" && value["effect_performed"] == true && value["target_id"] == request.upload.target_id && value["browser_context_id"] == request.upload.browser_context_id => ToolResultArtifact::success(&request.original.tool,Some(request.call_id.clone()),json!({"effect_performed":true,"upload":request.upload,"native_result":value})),
            Ok(value) => ToolResultArtifact::failure(&request.original.tool,Some(request.call_id.clone()),ToolResultError::new(if response.decision == Decision::Cancel { "upload_cancelled" } else if response.decision == Decision::Decline { "upload_declined" } else { "upload_execution_uncertain" },value["error"].as_str().unwrap_or("Upload was not applied")).with_details(Map::from_iter([("native_result".into(),value)]))),
            Err(error) => ToolResultArtifact::failure(&request.original.tool,Some(request.call_id.clone()),ToolResultError::new("upload_unavailable_or_uncertain",error)),
        };
        let result = json!({"request_id":request.request_id,"original_result":original}); create_record(&result_path,&result)?; Ok(result)
    }
}
