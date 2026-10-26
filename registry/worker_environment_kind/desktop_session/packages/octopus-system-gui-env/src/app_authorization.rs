#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use octopus_descriptors::InterfaceId;
use octopus_app_server_interface::{OperatorPermissionChoice, OperatorPermissionOffer, OperatorPermissionPersistence, OperatorPermissionResponse};
pub(crate) use octopus_app_server_interface::OperatorPermissionDecision as Decision;
use octopus_interfaces::{ArtifactRef, InterfaceInvocationEnvelope};
use octopus_tool_handlers::ToolCallArtifact;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::protocol::application::{AppRef, Scope};

const MAX_RECORD_BYTES: u64 = 2 * 1024 * 1024;
const RESPONSE_LABEL: &str = "operator.approval.response";

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApplicationInstance {
    pub app: AppRef,
    pub process_id: i32,
    pub launch_time_unix_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApplicationTarget {
    pub bundle_id: String,
    pub bundle_path: String,
    pub display_name: String,
    pub instance: Option<ApplicationInstance>,
}

impl ApplicationTarget {
    pub(crate) fn validate_selector(&self, value: &Value) -> Result<(), String> {
        let selector: crate::protocol::application::Selector = serde_json::from_value(value.clone())
            .map_err(|error| format!("application selector: {error}"))?;
        let matches = match selector {
            crate::protocol::application::Selector::BundleId(id) => id == self.bundle_id,
            crate::protocol::application::Selector::BundlePath(path) => path == self.bundle_path,
        };
        if !matches { return Err("resolved application does not match the exact requested selector".into()); }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OriginalCall {
    pub run: String,
    pub ee: String,
    pub worker: InterfaceId,
    pub door_interface: InterfaceId,
    pub tool: String,
    pub correlation_id: String,
    pub arguments: Value,
    pub source_movement_seq: u64,
    pub source_artifact: ArtifactRef,
}

impl OriginalCall {
    pub(crate) fn source(&self) -> crate::protocol::application::Source {
        crate::protocol::application::Source { tool: self.tool.clone(), correlation_id: self.correlation_id.clone(),
            source_movement_seq: self.source_movement_seq, source_artifact: self.source_artifact.clone() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResumeCall {
    pub tool: String,
    pub correlation_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OperatorRequest {
    pub schema_version: u32,
    pub request_id: String,
    pub call_id: String,
    pub original: OriginalCall,
    pub scope: Scope,
    pub application: ApplicationTarget,
    pub approval_scope: String,
    pub resume: ResumeCall,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_policy: Option<crate::protocol::NativeAppPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_offer: Option<OperatorPermissionOffer>,
}

impl OperatorRequest {
    pub(crate) fn new(original: OriginalCall, scope: Scope, application: ApplicationTarget,
        resume_tool: String, native_policy: Option<crate::protocol::NativeAppPolicy>,
        permission_offer: Option<OperatorPermissionOffer>) -> Result<Self, String> {
        let mut value = Self {
            schema_version: 1, request_id: String::new(), call_id: original.correlation_id.clone(),
            original, scope, application, approval_scope: "formed_app_target".into(),
            resume: ResumeCall { tool: resume_tool, correlation_id: String::new() }, native_policy, permission_offer,
        };
        let digest = value.digest();
        value.request_id = format!("sha256:{digest}");
        value.resume.correlation_id = format!("app-resume:{digest}");
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn verifier_params(&self) -> Value {
        let mut params = json!({"request_id":self.request_id,"call_id":self.call_id});
        if let Some(offer) = &self.permission_offer { params["offered_choices"] = json!(offer.offered_choices); }
        params
    }

    fn digest(&self) -> String {
        let mut value = json!({
            "schema_version":self.schema_version,"original":self.original,"scope":self.scope,
            "application":self.application,"approval_scope":self.approval_scope,
        });
        if let Some(policy) = &self.native_policy { value["native_policy"] = json!(policy); }
        if let Some(offer) = &self.permission_offer { value["permission_offer"] = json!(offer); }
        let bytes = octopus_descriptors::canonical_bytes(&value);
        format!("{:x}", Sha256::digest(bytes))
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        let fields = [&self.call_id, &self.original.run, &self.original.ee, &self.original.tool,
            &self.original.correlation_id, &self.resume.tool, &self.scope.host_generation,
            &self.scope.worker_environment_id, &self.scope.formation_pin, &self.application.bundle_id,
            &self.application.bundle_path, &self.application.display_name];
        if self.schema_version != 1 || self.approval_scope != "formed_app_target"
            || fields.into_iter().any(|value| value.is_empty() || value.trim() != value.as_str() || value.contains('\0') || value.len() > 4096)
            || !self.original.arguments.is_object() || self.call_id != self.original.correlation_id
            || self.original.source_movement_seq == 0 || self.original.source_movement_seq > 9_007_199_254_740_991
            || !Path::new(&self.application.bundle_path).is_absolute()
            || Path::new(&self.application.bundle_path).components().any(|part| matches!(part, std::path::Component::ParentDir | std::path::Component::CurDir)) {
            return Err("application approval request has invalid immutable coordinates".into());
        }
        if let Some(instance) = &self.application.instance {
            instance.app.validate(&self.scope)?;
            if instance.process_id <= 0 || instance.launch_time_unix_ms == Some(0) {
                return Err("application approval request has invalid running-instance evidence".into());
            }
        }
        if let Some(policy) = &self.native_policy {
            policy.validate(&self.application)?;
            policy.require_allowed()?;
        }
        if let Some(offer) = &self.permission_offer {
            offer.validate()?;
            let native = self.native_policy.as_ref().ok_or("application permission offer has no actual native eligibility")?;
            if offer.offered_choices.iter().any(|choice| choice.persistence == OperatorPermissionPersistence::Always
                && (!native.allow_persistent_approval || choice.decision != Decision::Accept)) {
                return Err("application permission offer exceeds native persistent-approval eligibility".into());
            }
        }
        let digest = self.digest();
        if self.request_id != format!("sha256:{digest}") || self.resume.correlation_id != format!("app-resume:{digest}") {
            return Err("application approval identities do not commit the exact request".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingRequest {
    pub request: OperatorRequest,
    pub native_target_handle: String,
    pub operator_interface: InterfaceId,
    pub provider_interface: InterfaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_policy: Option<crate::protocol::NativeAppPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_policy_digest: Option<String>,
}

impl PendingRequest {
    pub(crate) fn validate(&self) -> Result<(), String> {
        self.request.validate()?;
        if self.request.permission_offer.as_ref().is_some_and(|offer| offer.client_interface != self.operator_interface) {
            return Err("application permission offer changed its exact operator interface".into());
        }
        if self.request.native_policy != self.native_policy {
            return Err("application request changed its preserved native eligibility policy".into());
        }
        if self.native_policy.is_some() != self.native_policy_digest.is_some()
            || self.native_policy_digest.as_ref().is_some_and(|digest| digest.len() != 64
                || !digest.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))) {
            return Err("preserved native application policy has no exact configuration digest".into());
        }
        if let Some(policy) = &self.native_policy {
            policy.validate(&self.request.application)?;
            policy.require_allowed()?;
        }
        Ok(())
    }
}

pub(crate) fn validate_native_policy(application: &ApplicationTarget, native: Option<&crate::protocol::NativeAppPolicy>,
    digest: Option<&str>, configured: Option<&crate::config::ApplicationPolicy>) -> Result<(), String> {
    match (native, digest, configured) {
        (None, None, None) => Ok(()),
        (Some(native), Some(digest), Some(configured)) => {
            configured.validate()?;
            native.validate(application)?;
            if digest != configured.digest()? || native.allow_persistent_approval != configured.allow_persistent_approval
                || native.warning_subtitle.as_ref().is_some_and(|text| text != &configured.high_risk_warning_subtitle) {
                return Err("native application policy differs from the exact private configuration snapshot".into());
            }
            native.require_allowed()
        },
        _ => Err("native application eligibility requires its selected configuration and actual native result".into()),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CallContext {
    pub scope: Scope,
    pub call: OriginalCall,
    pub inbound_interface: InterfaceId,
}

impl CallContext {
    pub(crate) fn from_request(request: &crate::protocol::Request) -> Result<Self, String> {
        let context: Self = serde_json::from_value(request.action["invocation"].clone())
            .map_err(|error| format!("application invocation coordinates: {error}"))?;
        if context.scope.worker_environment_id != request.worker_environment_id || context.call.tool != request.tool
            || context.call.source_movement_seq == 0 || context.call.source_movement_seq > 9_007_199_254_740_991
            || context.call.correlation_id.is_empty() || !context.call.arguments.is_object()
            || context.call.run.is_empty() || context.call.ee.is_empty() || context.scope.formation_pin.is_empty() {
            return Err("application invocation does not match the admitted room and tool".into());
        }
        Ok(context)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApplicationCatalogEntry {
    pub application: ApplicationTarget,
    pub active: bool,
    pub last_used_unix_ms: Option<u64>,
    pub use_count: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApplicationCatalog {
    pub scope: Scope,
    pub applications: Vec<ApplicationCatalogEntry>,
}

impl ApplicationCatalog {
    pub(crate) fn validate(&self, scope: &Scope) -> Result<(), String> {
        if &self.scope != scope || self.applications.len() > 1024 {
            return Err("native application catalog has a different scope or exceeds its bound".into());
        }
        let mut paths = std::collections::BTreeSet::new();
        for entry in &self.applications {
            let app = &entry.application;
            let path = Path::new(&app.bundle_path);
            if [&app.bundle_id, &app.bundle_path, &app.display_name].into_iter().any(|s| s.is_empty() || s.contains('\0') || s.len() > 4096)
                || !path.is_absolute() || path.extension().and_then(|s| s.to_str()) != Some("app")
                || path.components().any(|part| matches!(part, std::path::Component::ParentDir | std::path::Component::CurDir))
                || !paths.insert(&app.bundle_path) || (entry.active && app.instance.is_none())
                || entry.last_used_unix_ms.is_some_and(|n| n == 0 || n > 9_007_199_254_740_991)
                || entry.use_count.is_some_and(|n| n > 9_007_199_254_740_991) {
                return Err("native application catalog entry has invalid identity or usage evidence".into());
            }
            if let Some(instance) = &app.instance {
                instance.app.validate(scope)?;
                if instance.process_id <= 0 || instance.launch_time_unix_ms.is_some_and(|n| n == 0 || n > 9_007_199_254_740_991) {
                    return Err("native application catalog has invalid running-instance evidence".into());
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ApplicationResult {
    Catalog { schema_version: u32, catalog: ApplicationCatalog },
    Action { schema_version: u32, action: crate::protocol::application::ActionResult },
    AwaitingApproval { schema_version: u32, operator_request: OperatorRequest, effect_performed: bool },
    State { schema_version: u32, grant_request_id: String, application: crate::protocol::application::State },
    Resumed { schema_version: u32, operator_request: OperatorRequest, original_result: octopus_tool_handlers::ToolResultArtifact },
}

pub(crate) fn validate_result(value: &Value, request: &crate::protocol::Request) -> Result<(), String> {
    let context = CallContext::from_request(request)?;
    let result: ApplicationResult = serde_json::from_value(value.clone()).map_err(|error| format!("application result schema: {error}"))?;
    match result {
        ApplicationResult::Catalog { schema_version, catalog } => {
            if schema_version != 3 || request.action["type"] != "apps" || context.call.arguments != json!({}) {
                return Err("application catalog has an unexpected schema, operation or arguments".into());
            }
            catalog.validate(&context.scope)?;
        },
        ApplicationResult::Action { schema_version, action } => {
            let operation = request.action["type"].as_str().ok_or("application action has no operation")?;
            let call = crate::protocol::application::ActionCall::parse(operation, &context.call.arguments)?;
            if schema_version != 3 || !crate::protocol::is_app_effect(operation) { return Err("application action result has an unexpected schema or operation".into()); }
            action.validate(&context.scope, &call)?;
        },
        ApplicationResult::AwaitingApproval { schema_version, operator_request, effect_performed } => {
            operator_request.validate()?;
            operator_request.application.validate_selector(&context.call.arguments["app"])?;
            if operator_request.permission_offer.as_ref().is_some_and(|offer|
                request.action["operator_interface"].as_str() != Some(offer.client_interface.as_str())) {
                return Err("application permission result changed its exact operator interface".into());
            }
            if schema_version != 3 || effect_performed || request.action["type"] != "app_state"
                || operator_request.original != context.call || operator_request.scope != context.scope
                || request.action["resume_tool"].as_str() != Some(&operator_request.resume.tool) {
                return Err("application approval result does not preserve the exact original call".into());
            }
        },
        ApplicationResult::State { schema_version, grant_request_id, application } => {
            record_path(Path::new("/"), &grant_request_id, "request")?;
            if schema_version != 3 || request.action["type"] != "app_state" {
                return Err("application state has an unexpected operation or schema".into());
            }
            application.validate(&context.scope, &application.application.app)?;
            ApplicationTarget { bundle_id: application.application.bundle_id, bundle_path: application.application.bundle_path,
                display_name: application.application.display_name, instance: None }.validate_selector(&context.call.arguments["app"])?;
        },
        ApplicationResult::Resumed { schema_version, operator_request, original_result } => {
            operator_request.validate()?;
            if schema_version != 3 || request.action["type"] != "app_resume"
                || request.action["request_id"].as_str() != Some(&operator_request.request_id)
                || operator_request.scope != context.scope || operator_request.original.run != context.call.run
                || operator_request.original.ee != context.call.ee || operator_request.original.worker != context.call.worker
                || operator_request.original.door_interface != context.call.door_interface
                || operator_request.resume.tool != context.call.tool || operator_request.resume.correlation_id != context.call.correlation_id
                || original_result.tool != operator_request.original.tool
                || original_result.correlation_id.as_deref() != Some(&operator_request.call_id) {
                return Err("application resume result does not preserve its exact pending original call".into());
            }
            if let Some(state) = original_result.body.as_ref().and_then(|body| body.pointer("/evidence/application")) {
                if !original_result.ok || original_result.error.is_some()
                    || original_result.body.as_ref().and_then(|body| body.get("app_grant_request_id")).and_then(Value::as_str) != Some(&operator_request.request_id) {
                    return Err("resumed application state has no successful exact grant result".into());
                }
                let state: crate::protocol::application::State = serde_json::from_value(state.clone()).map_err(|error| error.to_string())?;
                state.validate(&context.scope, &state.application.app)?;
                if state.application.bundle_id != operator_request.application.bundle_id || state.application.bundle_path != operator_request.application.bundle_path {
                    return Err("resumed native application differs from the approved bundle".into());
                }
            }
        },
    }
    Ok(())
}

pub(crate) fn live_app_source(value: &Value, request: &crate::protocol::Request) -> Result<Option<crate::protocol::application::LiveAppSource>, String> {
    validate_result(value, request)?;
    let result: ApplicationResult = serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    let (state, grant, source) = match result {
        ApplicationResult::State { application, grant_request_id, .. } =>
            (application, grant_request_id, CallContext::from_request(request)?.call.source()),
        ApplicationResult::Resumed { operator_request, original_result, .. } if original_result.ok => {
            let body = original_result.body.as_ref().ok_or("successful application resume has no original body")?;
            let application = body.pointer("/evidence/application").ok_or("successful application resume has no original state")?;
            (serde_json::from_value(application.clone()).map_err(|error| error.to_string())?,
                operator_request.request_id, operator_request.original.source())
        },
        _ => return Ok(None),
    };
    crate::protocol::application::LiveAppSource::from_state(&state, &grant, source).map(Some)
}

pub(crate) fn project_result(value: Value, evidence: Value, viewer: Value, request: &crate::protocol::Request) -> Result<Value, String> {
    let mut evidence: crate::protocol::Evidence = serde_json::from_value(evidence).map_err(|error| error.to_string())?;
    if let Some(source) = live_app_source(&value, request)? {
        evidence.applications.retain(|entry| entry.scope != source.scope || entry.application.app != source.application.app);
        evidence.applications.push(source.clone());
        evidence.presentation_intent = Some(crate::protocol::application::PresentationIntent::from_source(&source));
    }
    let result: ApplicationResult = serde_json::from_value(value).map_err(|error| error.to_string())?;
    Ok(match result {
        ApplicationResult::Catalog { catalog, .. } => json!({"accepted":true,"evidence":evidence,"viewer":viewer,"application_catalog":catalog}),
        ApplicationResult::Action { action, .. } => json!({"accepted":true,"evidence":evidence,"viewer":viewer,"application_action":action}),
        ApplicationResult::AwaitingApproval { operator_request, .. } => json!({"accepted":true,"evidence":evidence,"viewer":viewer,
            "operator_request":operator_request,"effect_performed":false}),
        ApplicationResult::State { application, grant_request_id, .. } => {
            let mut body = json!({"accepted":true,"evidence":evidence,"viewer":viewer,"app_grant_request_id":grant_request_id});
            body["evidence"]["application"] = serde_json::to_value(application).map_err(|error| error.to_string())?;
            body
        },
        ApplicationResult::Resumed { operator_request, original_result, .. } => json!({"request_id":operator_request.request_id,
            "evidence":evidence,"viewer":viewer,"original_result":original_result}),
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdmittedDecision {
    pub request_id: String,
    pub call_id: String,
    pub decision: Decision,
    pub operator_artifact: ArtifactRef,
    pub operator_interface: InterfaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_persistence: Option<OperatorPermissionPersistence>,
}

impl AdmittedDecision {
    pub(crate) fn validate(&self, request: &OperatorRequest) -> Result<(), String> {
        OperatorPermissionResponse { request_id: self.request_id.clone(), call_id: self.call_id.clone(),
            decision: self.decision, selected_persistence: self.selected_persistence }
            .validate(&request.request_id, &request.call_id, request.permission_offer.as_ref())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalBody { operator_approval_response: OperatorPermissionResponse }

pub(crate) fn decode_resume(envelope: &InterfaceInvocationEnvelope, tool: &str) -> Result<ToolCallArtifact, String> {
    if envelope.read_artifact_manifest().map_err(|error| error.to_string())?.label != RESPONSE_LABEL {
        return Err("application resume only accepts the original operator response artifact, never a model tool.call".into());
    }
    let payload = envelope.read_artifact_payload().map_err(|error| error.to_string())?;
    let body: ApprovalBody = serde_json::from_slice(&payload).map_err(|error| error.to_string())?;
    let response = body.operator_approval_response;
    record_path(Path::new("/"), &response.request_id, "decision")?;
    if response.call_id.is_empty() || response.call_id.trim() != response.call_id {
        return Err("application response has no exact original correlation".into());
    }
    Ok(ToolCallArtifact { tool: tool.into(), arguments: json!({"request_id":response.request_id}),
        correlation_id: Some(format!("app-resume:{}", &response.request_id[7..])), label: None,
        source_interface_evidence: std::collections::BTreeMap::new() })
}

pub(crate) fn record_path(root: &Path, request_id: &str, suffix: &str) -> Result<PathBuf, String> {
    let digest = request_id.strip_prefix("sha256:").filter(|value| value.len() == 64
        && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        .ok_or("application request must have an exact SHA-256 identity")?;
    Ok(root.join(format!("app-{digest}.{suffix}.json")))
}

pub(crate) fn read_record<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    read_record_bounded(path, MAX_RECORD_BYTES)
}

#[cfg(unix)]
pub(crate) fn read_record_bounded<T: for<'de> Deserialize<'de>>(path: &Path, limit: u64) -> Result<T, String> {
    crate::native_connection::validate_directory(path.parent().ok_or("application record has no private parent")?)?;
    let mut file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW)
        .open(path).map_err(|error| format!("application private record unavailable: {error}"))?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600 || metadata.nlink() != 1 || metadata.len() > limit {
        return Err("application private record must be a bounded owned single-link mode 0600 file".into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file).take(limit + 1).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > limit { return Err("application private record exceeds its byte bound".into()); }
    serde_json::from_slice(&bytes).map_err(|error| format!("application private record is malformed: {error}"))
}

#[cfg(not(unix))]
#[allow(unused_variables, unused_mut)]
pub(crate) fn read_record_bounded<T: for<'de> Deserialize<'de>>(path: &Path, limit: u64) -> Result<T, String> {
    Err("desktop control requires macOS".into())
}

pub(crate) fn create_record<T: Serialize>(path: &Path, record: &T) -> Result<(), String> {
    create_record_bounded(path, record, MAX_RECORD_BYTES)
}

#[cfg(unix)]
pub(crate) fn create_record_bounded<T: Serialize>(path: &Path, record: &T, limit: u64) -> Result<(), String> {
    let parent = path.parent().ok_or("application record has no private parent")?;
    crate::native_connection::validate_directory(parent)?;
    let bytes = serde_json::to_vec(record).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > limit { return Err("application private record exceeds its byte bound".into()); }
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW)
        .open(path).map_err(|error| format!("application private record cannot be created: {error}"))?;
    file.write_all(&bytes).and_then(|_| file.sync_all()).map_err(|error| error.to_string())?;
    std::fs::File::open(parent).and_then(|file| file.sync_all()).map_err(|error| error.to_string())
}

#[cfg(not(unix))]
#[allow(unused_variables, unused_mut)]
pub(crate) fn create_record_bounded<T: Serialize>(path: &Path, record: &T, limit: u64) -> Result<(), String> {
    Err("desktop control requires macOS".into())
}

pub(crate) fn admit_response(_ctx: &octopus_tool_door_runtime::InterfaceLiftContext, root: &Path, door: &InterfaceId, envelope: &InterfaceInvocationEnvelope, config: &crate::config::DesktopConfig) -> Result<ToolCallArtifact, String> {
    let manifest = envelope.read_artifact_manifest().map_err(|error| error.to_string())?;
    if manifest.label != RESPONSE_LABEL { return Err("application decision requires the original operator response artifact".into()); }
    let payload = envelope.read_artifact_payload().map_err(|error| error.to_string())?;
    let response: ApprovalBody = serde_json::from_slice(&payload).map_err(|error| format!("application approval response is malformed: {error}"))?;
    let response = response.operator_approval_response;
    let pending: PendingRequest = read_record(&record_path(root, &response.request_id, "request")?)?;
    pending.validate()?;
    let request = &pending.request;
    validate_native_policy(&request.application, pending.native_policy.as_ref(), pending.native_policy_digest.as_deref(), config.host.as_ref().and_then(|host| host.application_policy.as_ref()))?;
    let approval = config.app_approval.as_ref().ok_or("application approval configuration is absent")?;
    pending.validate_config(approval)?;
    let metadata = envelope.metadata().map_err(|error| error.to_string())?;
    let runtime = envelope.runtime().map_err(|error| error.to_string())?;
    let bindings = metadata.checkpoint_binding_policy.as_ref().ok_or("application approval has no admitted binding snapshot")?;
    let admission = metadata.checkpoint_admission.as_ref().ok_or("application approval has no completed checkpoint admission")?;
    let (mark, verifier) = match response.decision {
        Decision::Accept => ("operator.approval", "operator_approval"),
        Decision::Decline | Decision::Cancel => ("operator.input.provided", "operator_input"),
    };
    let requirements = &metadata.required_verified_artifacts;
    if response.request_id != request.request_id || response.call_id != request.call_id
        || request.original.door_interface != *door || runtime.interface != *door
        || runtime.run.as_str() != request.original.run || runtime.ee.as_str() != request.original.ee
        || runtime.worker != request.original.worker || bindings.inbound.interface != pending.operator_interface
        || !admission.complete || admission.artifact_label != RESPONSE_LABEL
        || admission.required_artifact_labels != [RESPONSE_LABEL]
        || metadata.verification_failure.is_some() || requirements.len() != 1
        || requirements[0].mark.as_str() != mark || requirements[0].verifier.as_str() != verifier
        || requirements[0].artifact_label != RESPONSE_LABEL
        || requirements[0].params != request.verifier_params()
        || !metadata.grant.mark_outcomes.iter().any(|value| value == mark)
        || metadata.outbound_interface_bindings != [pending.provider_interface.clone(), door.clone()]
        || bindings.outbound.iter().map(|binding| &binding.interface).collect::<Vec<_>>() != [&pending.provider_interface, door]
        || !metadata.grant.interfaces.contains(&door.to_string())
        || !metadata.grant.interfaces.contains(&pending.operator_interface.to_string())
        || [&request.original.tool, &request.resume.tool].into_iter().any(|tool| !metadata.grant.tools.contains(tool) || metadata.grant.hard_blocked_tools.contains(tool)) {
        return Err("application decision does not match the exact verifier-admitted request and scope".into());
    }
    response.validate(&request.request_id, &request.call_id, request.permission_offer.as_ref())?;
    let connection = crate::native_connection::PrivateConnection::read(&root.join("connection.json"), door.as_str())?;
    if connection.host_generation != request.scope.host_generation {
        return Err("application approval belongs to a retired native host".into());
    }
    let decision = AdmittedDecision { request_id: response.request_id, call_id: response.call_id,
        decision: response.decision, operator_artifact: envelope.artifact.clone(),
        operator_interface: pending.operator_interface, selected_persistence: response.selected_persistence };
    let path = record_path(root, &request.request_id, "decision")?;
    if path.try_exists().map_err(|error| error.to_string())? {
        if read_record::<AdmittedDecision>(&path)? != decision { return Err("application request already has a different admitted decision".into()); }
    } else { create_record(&path, &decision)?; }
    Ok(ToolCallArtifact { tool: request.resume.tool.clone(), arguments: json!({"request_id":request.request_id}),
        correlation_id: Some(request.resume.correlation_id.clone()), label: None,
        source_interface_evidence: std::collections::BTreeMap::new() })
}

pub(crate) fn historical_identity(_ctx: &octopus_tool_door_runtime::InterfaceLiftContext, root: &Path, door: &InterfaceId, envelope: &InterfaceInvocationEnvelope) -> Result<ToolCallArtifact, String> {
    let manifest = envelope.read_artifact_manifest().map_err(|error| error.to_string())?;
    if manifest.label != RESPONSE_LABEL { return Err("historical application identity requires the original operator response".into()); }
    let payload = envelope.read_artifact_payload().map_err(|error| error.to_string())?;
    let response: ApprovalBody = serde_json::from_slice(&payload).map_err(|error| error.to_string())?;
    let response = response.operator_approval_response;
    let pending: PendingRequest = read_record(&record_path(root, &response.request_id, "request")?)?;
    pending.validate()?;
    let request = &pending.request;
    let runtime = envelope.runtime().map_err(|error| error.to_string())?;
    let metadata = envelope.metadata().map_err(|error| error.to_string())?;
    let bindings = metadata.checkpoint_binding_policy.as_ref().ok_or("historical application identity has no exact binding evidence")?;
    if response.request_id != request.request_id || response.call_id != request.call_id
        || runtime.run.as_str() != request.original.run || runtime.ee.as_str() != request.original.ee
        || runtime.worker != request.original.worker || runtime.interface != *door || request.original.door_interface != *door
        || bindings.inbound.interface != pending.operator_interface {
        return Err("historical application source does not match its preserved immutable request".into());
    }
    response.validate(&request.request_id, &request.call_id, request.permission_offer.as_ref())?;
    Ok(ToolCallArtifact { tool: request.resume.tool.clone(), arguments: json!({"request_id":request.request_id}),
        correlation_id: Some(request.resume.correlation_id.clone()), label: None,
        source_interface_evidence: std::collections::BTreeMap::new() })
}

impl PendingRequest {
    pub(crate) fn validate_config(&self, config: &crate::config::AppApprovalConfig) -> Result<(), String> {
        self.validate()?;
        if self.operator_interface != config.operator_interface || self.request.resume.tool != config.resume_tool {
            return Err("application pending request differs from the exact configured operator/resume identities".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PermissionRecord {
    schema_version: u32,
    pub request: OperatorRequest,
    pub decision: AdmittedDecision,
}

impl PermissionRecord {
    fn validate(&self, persistence: OperatorPermissionPersistence) -> Result<(), String> {
        self.request.validate()?;
        self.decision.validate(&self.request)?;
        let offer = self.request.permission_offer.as_ref().ok_or("stored application permission has no immutable offer")?;
        if self.schema_version != 1 || self.decision.operator_interface != offer.client_interface
            || self.decision.selected_persistence != Some(persistence) || self.decision.decision == Decision::Cancel
            || (persistence == OperatorPermissionPersistence::Always && self.decision.decision != Decision::Accept) {
            return Err("stored application permission changed its admitted source or scope".into());
        }
        Ok(())
    }
}

pub(crate) fn permission_offer(client: InterfaceId, policy: &crate::protocol::NativeAppPolicy,
    always_available: bool) -> OperatorPermissionOffer {
    let mut choices = vec![
        OperatorPermissionChoice { decision: Decision::Accept, persistence: OperatorPermissionPersistence::Session },
        OperatorPermissionChoice { decision: Decision::Decline, persistence: OperatorPermissionPersistence::Once },
    ];
    if always_available && policy.allow_persistent_approval {
        choices.push(OperatorPermissionChoice { decision: Decision::Accept, persistence: OperatorPermissionPersistence::Always });
    }
    OperatorPermissionOffer { client_interface: client, offered_choices: choices }
}

fn permission_path(root: &Path, request: &OperatorRequest, persistence: OperatorPermissionPersistence) -> Result<PathBuf, String> {
    let offer = request.permission_offer.as_ref().ok_or("application permission has no immutable offer")?;
    let (name, key) = match persistence {
        OperatorPermissionPersistence::Session => ("session", json!({"client_interface":offer.client_interface,
            "run":request.original.run,"bundle_id":request.application.bundle_id})),
        OperatorPermissionPersistence::Always => ("always", json!({"bundle_id":request.application.bundle_id})),
        _ => return Err("one-time application permission is not a stored preference".into()),
    };
    let digest = blake3::hash(&octopus_descriptors::canonical_bytes(&key)).to_hex();
    Ok(root.join(format!("app-permission-{name}-{digest}.json")))
}

#[cfg(unix)]
fn permission_lock(root: &Path) -> Result<std::fs::File, String> {
    crate::native_connection::validate_directory(root)?;
    let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false)
        .mode(0o600).custom_flags(libc::O_NOFOLLOW).open(root.join("application-permissions.lock"))
        .map_err(|error| format!("application preferences lock: {error}"))?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600 || metadata.nlink() != 1 || metadata.len() != 0 {
        return Err("application preferences lock is not an owned empty mode-0600 file".into());
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("application_preferences_busy: another settings transaction holds the private preferences".into());
    }
    Ok(file)
}

#[cfg(not(unix))]
#[allow(unused_variables, unused_mut)]
fn permission_lock(root: &Path) -> Result<std::fs::File, String> {
    Err("desktop control requires macOS".into())
}

fn read_permission(root: &Path, request: &OperatorRequest, persistence: OperatorPermissionPersistence) -> Result<Option<PermissionRecord>, String> {
    let path = permission_path(root, request, persistence)?;
    if !path.try_exists().map_err(|error| error.to_string())? { return Ok(None); }
    let record: PermissionRecord = read_record(&path)?;
    record.validate(persistence)?;
    if permission_path(root, &record.request, persistence)? != path
        || record.request.application.bundle_id != request.application.bundle_id
        || (persistence == OperatorPermissionPersistence::Session
            && (record.request.original.run != request.original.run
                || record.request.permission_offer.as_ref().map(|offer| &offer.client_interface)
                    != request.permission_offer.as_ref().map(|offer| &offer.client_interface))) {
        return Err("stored application permission changed its exact subject or conversation".into());
    }
    Ok(Some(record))
}

pub(crate) fn lookup_permission(private: &Path, shared: Option<&Path>, request: &OperatorRequest) -> Result<Option<PermissionRecord>, String> {
    if request.permission_offer.is_none() { return Ok(None); }
    if let Some(record) = read_permission(private, request, OperatorPermissionPersistence::Session)? { return Ok(Some(record)); }
    if !request.native_policy.as_ref().is_some_and(|policy| policy.allow_persistent_approval) { return Ok(None); }
    let Some(shared) = shared else { return Ok(None); };
    let _lock = permission_lock(shared)?;
    read_permission(shared, request, OperatorPermissionPersistence::Always)
}

pub(crate) fn remember_permission(private: &Path, shared: Option<&Path>, request: &OperatorRequest,
    decision: &AdmittedDecision) -> Result<(), String> {
    decision.validate(request)?;
    if decision.decision == Decision::Cancel { return Ok(()); }
    let persistence = match decision.selected_persistence {
        Some(OperatorPermissionPersistence::Session) => OperatorPermissionPersistence::Session,
        Some(OperatorPermissionPersistence::Always) => OperatorPermissionPersistence::Always,
        _ => return Ok(()),
    };
    let root = if persistence == OperatorPermissionPersistence::Always {
        shared.ok_or("always application permission has no declared private preference store")?
    } else { private };
    let _lock = permission_lock(root)?;
    let record = PermissionRecord { schema_version: 1, request: request.clone(), decision: decision.clone() };
    record.validate(persistence)?;
    let path = permission_path(root, request, persistence)?;
    let _ = read_permission(root, request, persistence)?;
    let mut nonce = [0u8; 16]; getrandom::fill(&mut nonce).map_err(|error| error.to_string())?;
    let nonce = nonce.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    let temporary = root.join(format!(".application-permission-{nonce}.json"));
    create_record(&temporary, &record)?;
    let result = std::fs::rename(&temporary, &path)
        .and_then(|_| std::fs::File::open(root)?.sync_all()).map_err(|error| error.to_string());
    if result.is_err() { let _ = std::fs::remove_file(&temporary); }
    result
}

pub(crate) fn computer_use_enabled(root: &Path) -> Result<bool, String> {
    crate::native_connection::validate_directory(root)?;
    let path = root.join("computer-use.json");
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
        Ok(_) => {}
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Preference { schema_version: u32, enabled: bool }
    let record: Preference = read_record_bounded(&path, 1024)?;
    if record.schema_version != 1 { return Err("computer_use_preference_invalid: unsupported saved preference".into()); }
    Ok(record.enabled)
}

pub(crate) fn application_preferences(root: &Path, bundle: Option<&str>) -> Result<Value, String> {
    let _lock = permission_lock(root)?;
    if bundle.is_some_and(|id| id.is_empty() || id.trim() != id || id.len() > 4096 || id.contains('\0')) {
        return Err("application preference removal requires its exact bundle identifier".into());
    }
    let mut applications = std::collections::BTreeMap::new();
    let mut remove = None;
    let mut entries = 0;
    for entry in std::fs::read_dir(root).map_err(|error| error.to_string())? {
        entries += 1;
        if entries > 2048 { return Err("application preferences exceed the directory entry bound".into()); }
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { return Err("application preferences have an invalid filename".into()); };
        if !name.starts_with("app-permission-always-") || !name.ends_with(".json") { continue; }
        if applications.len() >= 1024 { return Err("application preferences exceed the bounded app inventory".into()); }
        let record: PermissionRecord = read_record(&entry.path())?;
        record.validate(OperatorPermissionPersistence::Always)?;
        if permission_path(root, &record.request, OperatorPermissionPersistence::Always)? != entry.path() {
            return Err("application preference filename differs from its validated native subject".into());
        }
        let application = &record.request.application;
        if bundle == Some(application.bundle_id.as_str()) {
            remove = Some(entry.path());
        } else {
            applications.insert(application.bundle_id.clone(), json!({"bundle_id":application.bundle_id,"display_name":application.display_name}));
        }
    }
    let removed = if let Some(path) = remove {
        std::fs::remove_file(path).map_err(|error| error.to_string())?;
        std::fs::File::open(root).and_then(|file| file.sync_all())
            .map_err(|error| format!("application preference removed but directory sync failed: {error}"))?;
        true
    } else { false };
    Ok(json!({"applications":applications.into_values().collect::<Vec<_>>(),"removed":removed}))
}
