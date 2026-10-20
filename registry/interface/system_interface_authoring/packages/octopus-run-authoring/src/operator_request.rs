use super::*;

pub use octopus_app_server_interface::{OperatorPermissionDecision, OperatorPermissionPersistence, OperatorPermissionResponse};

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationOperatorRequest {
    pub schema_version: u32,
    pub request_id: String,
    pub call_id: String,
    pub original: OriginalCall,
    pub scope: AppScope,
    pub application: ApplicationTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_policy: Option<octopus_app_server_interface::application_consent_contract::NativeAppPolicy>,
    pub approval_scope: String,
    pub resume: ResumeCall,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_offer: Option<octopus_app_server_interface::OperatorPermissionOffer>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct OriginalCall {
    pub run: String,
    pub ee: String,
    pub worker: InterfaceId,
    pub door_interface: InterfaceId,
    pub tool: String,
    pub correlation_id: String,
    pub arguments: serde_json::Value,
    pub source_movement_seq: u64,
    pub source_artifact: ArtifactRef,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppScope {
    pub worker_environment_id: String,
    pub formation_pin: String,
    pub host_generation: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppRef {
    pub host_generation: String,
    pub app_instance_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationTarget {
    pub bundle_id: String,
    pub bundle_path: String,
    pub display_name: String,
    pub instance: Option<ApplicationInstance>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationInstance {
    pub app: AppRef,
    pub process_id: i32,
    pub launch_time_unix_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeCall {
    pub tool: String,
    pub correlation_id: String,
}

pub(crate) fn application_request_error(message: impl Into<String>) -> String {
    format!("application owner request: {}", message.into())
}

impl ApplicationOperatorRequest {
    pub(crate) fn digest(&self) -> Result<String, String> {
        let mut commitment = serde_json::json!({
            "schema_version": self.schema_version,
            "original": self.original,
            "scope": self.scope,
            "application": self.application,
            "approval_scope": self.approval_scope,
        });
        if let Some(offer) = &self.permission_offer { commitment["permission_offer"] = serde_json::json!(offer); }
        if let Some(policy) = &self.native_policy { commitment["native_policy"] = serde_json::json!(policy); }
        let bytes = octopus_descriptors::canonical_bytes(&commitment);
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }

    pub fn validate(&self) -> Result<(), String> {
        let fields = [
            self.call_id.as_str(),
            self.original.run.as_str(),
            self.original.ee.as_str(),
            self.original.worker.as_str(),
            self.original.door_interface.as_str(),
            self.original.tool.as_str(),
            self.resume.tool.as_str(),
            self.scope.worker_environment_id.as_str(),
            self.scope.formation_pin.as_str(),
            self.scope.host_generation.as_str(),
            self.application.bundle_id.as_str(),
            self.application.bundle_path.as_str(),
            self.application.display_name.as_str(),
        ];
        if self.schema_version != 1
            || self.approval_scope != "formed_app_target"
            || fields.iter().any(|value| {
                value.is_empty()
                    || value.trim() != *value
                    || value.contains('\0')
                    || value.len() > 4096
            })
            || self.original.tool == self.resume.tool
            || self.original.correlation_id != self.call_id
            || self.original.source_movement_seq == 0
            || self.original.source_movement_seq > 9_007_199_254_740_991
            || !self.original.arguments.is_object()
            || !Path::new(&self.application.bundle_path).is_absolute()
            || Path::new(&self.application.bundle_path)
                .components()
                .any(|part| {
                    matches!(
                        part,
                        std::path::Component::ParentDir | std::path::Component::CurDir
                    )
                })
        {
            return Err(application_request_error(
                "invalid immutable application/original-call coordinates",
            ));
        }
        if let Some(instance) = &self.application.instance {
            if instance.app.host_generation != self.scope.host_generation
                || instance.app.app_instance_id.is_empty()
                || instance.app.app_instance_id.trim() != instance.app.app_instance_id
                || instance.app.app_instance_id.contains('\0')
                || instance.app.app_instance_id.len() > 2048
                || self.scope.host_generation.len() > 2048
                || self.scope.worker_environment_id.len() > 2048
                || self.scope.formation_pin.len() > 2048
                || instance.process_id <= 0
                || instance.launch_time_unix_ms == Some(0)
            {
                return Err(application_request_error(
                    "invalid immutable running-application instance",
                ));
            }
        }
        if let Some(offer) = &self.permission_offer {
            use octopus_app_server_interface::{OperatorPermissionDecision, OperatorPermissionPersistence};
            use octopus_app_server_interface::application_consent_contract::NativeAppPolicyDecision;
            offer.validate()?;
            let native = self.native_policy.as_ref().ok_or_else(|| application_request_error("permission offer has no original native policy data"))?;
            if native.target.bundle_id != self.application.bundle_id || native.target.bundle_path != self.application.bundle_path
                || native.target.display_name != self.application.display_name || native.decision != NativeAppPolicyDecision::Allowed
                || native.warning_subtitle.as_ref().is_some_and(|value| value.len() > 4096 || value.contains('\0'))
                || offer.offered_choices.iter().any(|choice| choice.decision == OperatorPermissionDecision::Accept
                    && choice.persistence == OperatorPermissionPersistence::Always && !native.allow_persistent_approval) {
                return Err(application_request_error("permission offer changed original native target or eligibility data"));
            }
        } else if self.native_policy.is_some() {
            return Err(application_request_error("native policy display has no ordinary permission offer"));
        }
        let digest = self.digest()?;
        if self.request_id != format!("sha256:{digest}")
            || self.resume.correlation_id != format!("app-resume:{digest}")
        {
            return Err(application_request_error(
                "request/resume identities do not commit the immutable scope",
            ));
        }
        Ok(())
    }
}


#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserUploadFile {
    pub artifact_ref: ArtifactRef,
    pub filename: String,
    pub payload_sha256: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserUploadSubject {
    pub browser_context_id: String,
    pub target_id: String,
    pub reservation_id: String,
    pub file_chooser_id: String,
    pub document_token: String,
    pub backend_node_id: u64,
    pub top_level_origin: String,
    pub files: Vec<BrowserUploadFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserUploadOperatorRequest {
    pub schema_version: u32,
    pub request_id: String,
    pub call_id: String,
    pub original: OriginalCall,
    pub scope: AppScope,
    pub approval_scope: String,
    pub upload: BrowserUploadSubject,
    pub resume: ResumeCall,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_offer: Option<octopus_app_server_interface::OperatorPermissionOffer>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserHistoryQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queries: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserHistorySubject {
    pub source: ArtifactRef,
    pub source_payload_digest: String,
    pub source_epoch: String,
    pub query: BrowserHistoryQuery,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserHistoryOperatorRequest {
    pub schema_version: u32,
    pub request_id: String,
    pub call_id: String,
    pub original: OriginalCall,
    pub scope: AppScope,
    pub approval_scope: String,
    pub history: BrowserHistorySubject,
    pub resume: ResumeCall,
    pub permission_offer: octopus_app_server_interface::OperatorPermissionOffer,
}

impl BrowserHistoryOperatorRequest {
    pub fn validate(&self) -> Result<(), String> {
        use octopus_app_server_interface::OperatorPermissionPersistence;
        self.permission_offer.validate()?;
        let exact = |value: &str| !value.is_empty() && value.trim() == value && value.len() <= 4096 && !value.contains('\0');
        let hex = |value: &str, length: usize| value.len() == length && value.bytes().all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v));
        if self.schema_version != 1 || self.approval_scope != "browser_history_read"
            || self.call_id != self.original.correlation_id || self.original.tool == self.resume.tool
            || self.original.source_movement_seq == 0 || self.original.source_movement_seq > 9_007_199_254_740_991
            || [&self.call_id, &self.original.run, &self.original.ee, self.original.worker.as_str(), self.original.door_interface.as_str(),
                &self.original.tool, &self.resume.tool, &self.scope.worker_environment_id, &self.scope.formation_pin,
                self.history.source.id.as_str(), self.original.source_artifact.id.as_str(),
                self.permission_offer.client_interface.as_str()].into_iter().any(|value| !exact(value))
            || !hex(&self.scope.host_generation, 32) || !hex(&self.history.source_payload_digest, 64)
            || self.history.source_epoch.len() != 36
            || !self.history.source_epoch.split('-').map(str::len).eq([8, 4, 4, 4, 12])
            || !self.history.source_epoch.split('-').all(|part| hex(part, part.len()))
            || self.history.query.queries.as_ref().is_some_and(|queries| queries.is_empty()) || self.history.query.limit == Some(0)
            || self.permission_offer.offered_choices.iter().any(|choice| choice.persistence == OperatorPermissionPersistence::Session)
        {
            return Err("browser history changed its exact original call, source, native epoch or permission offer".into());
        }
        let mut arguments = self.original.arguments.as_object().ok_or("browser history query must be an object")?.clone();
        if arguments.remove("source") != Some(serde_json::json!(self.history.source))
            || serde_json::Value::Object(arguments) != serde_json::to_value(&self.history.query).map_err(|e| e.to_string())?
        {
            return Err("browser history changed its original query or replaced an omitted option".into());
        }
        let commitment = serde_json::json!({"schema_version":self.schema_version,"original":self.original,"scope":self.scope,
            "approval_scope":self.approval_scope,"history":self.history,"permission_offer":self.permission_offer});
        let digest = format!("{:x}", Sha256::digest(octopus_descriptors::canonical_bytes(&commitment)));
        if self.request_id != format!("sha256:{digest}") || self.resume.correlation_id != format!("browser-history-resume:{digest}") {
            return Err("browser history request/resume changed its immutable commitment".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OwnerApprovalSubject {
    Application(ApplicationTarget),
    BrowserUpload(BrowserUploadSubject),
    BrowserHistory(BrowserHistorySubject),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerOperatorRequest {
    pub schema_version: u32,
    pub request_id: String,
    pub call_id: String,
    pub original: OriginalCall,
    pub scope: AppScope,
    pub resume: ResumeCall,
    pub permission_offer: Option<octopus_app_server_interface::OperatorPermissionOffer>,
    pub(crate) native_policy: Option<octopus_app_server_interface::application_consent_contract::NativeAppPolicy>,
    pub(crate) subject: OwnerApprovalSubject,
}

impl From<ApplicationOperatorRequest> for OwnerOperatorRequest {
    fn from(value: ApplicationOperatorRequest) -> Self {
        Self { schema_version: value.schema_version, request_id: value.request_id, call_id: value.call_id,
            original: value.original, scope: value.scope, resume: value.resume, permission_offer: value.permission_offer, native_policy: value.native_policy, subject: OwnerApprovalSubject::Application(value.application) }
    }
}

impl From<BrowserUploadOperatorRequest> for OwnerOperatorRequest {
    fn from(value: BrowserUploadOperatorRequest) -> Self {
        Self { schema_version: value.schema_version, request_id: value.request_id, call_id: value.call_id,
            original: value.original, scope: value.scope, resume: value.resume, permission_offer: value.permission_offer, native_policy: None, subject: OwnerApprovalSubject::BrowserUpload(value.upload) }
    }
}

impl From<BrowserHistoryOperatorRequest> for OwnerOperatorRequest {
    fn from(value: BrowserHistoryOperatorRequest) -> Self {
        Self { schema_version: value.schema_version, request_id: value.request_id, call_id: value.call_id,
            original: value.original, scope: value.scope, resume: value.resume, permission_offer: Some(value.permission_offer), native_policy: None,
            subject: OwnerApprovalSubject::BrowserHistory(value.history) }
    }
}

impl serde::Serialize for OwnerOperatorRequest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.subject {
            OwnerApprovalSubject::Application(application) => ApplicationOperatorRequest { schema_version: self.schema_version,
                request_id: self.request_id.clone(), call_id: self.call_id.clone(), original: self.original.clone(), scope: self.scope.clone(),
                approval_scope: "formed_app_target".into(), application: application.clone(), native_policy: self.native_policy.clone(), resume: self.resume.clone(), permission_offer: self.permission_offer.clone() }.serialize(serializer),
            OwnerApprovalSubject::BrowserUpload(upload) => BrowserUploadOperatorRequest { schema_version: self.schema_version,
                request_id: self.request_id.clone(), call_id: self.call_id.clone(), original: self.original.clone(), scope: self.scope.clone(),
                approval_scope: "browser_file_upload".into(), upload: upload.clone(), resume: self.resume.clone(), permission_offer: self.permission_offer.clone() }.serialize(serializer),
            OwnerApprovalSubject::BrowserHistory(history) => BrowserHistoryOperatorRequest { schema_version: self.schema_version,
                request_id: self.request_id.clone(), call_id: self.call_id.clone(), original: self.original.clone(), scope: self.scope.clone(),
                approval_scope: "browser_history_read".into(), history: history.clone(), resume: self.resume.clone(),
                permission_offer: self.permission_offer.clone().ok_or_else(|| serde::ser::Error::custom("history requires its immutable ordinary permission offer"))? }.serialize(serializer),
        }
    }
}

impl<'de> serde::Deserialize<'de> for OwnerOperatorRequest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        if value.get("consent_offer").is_some() {
            return Err(serde::de::Error::custom("retired Home consent_offer cannot use ordinary permissions"));
        }
        if value.get("permission_offer").is_some_and(serde_json::Value::is_null)
            || value.get("native_policy").is_some_and(serde_json::Value::is_null) {
            return Err(serde::de::Error::custom("present permission data cannot be null"));
        }
        let request = match value.get("approval_scope").and_then(serde_json::Value::as_str) {
            Some("formed_app_target") => {
                let application: ApplicationOperatorRequest = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                application.validate().map_err(serde::de::Error::custom)?;
                Self::from(application)
            }
            Some("browser_file_upload") => {
                let upload: BrowserUploadOperatorRequest = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                upload.validate().map_err(serde::de::Error::custom)?;
                Self::from(upload)
            }
            Some("browser_history_read") => {
                let history: BrowserHistoryOperatorRequest = serde_json::from_value(value.clone()).map_err(serde::de::Error::custom)?;
                history.validate().map_err(serde::de::Error::custom)?;
                if serde_json::to_value(&history).map_err(serde::de::Error::custom)? != value {
                    return Err(serde::de::Error::custom("history pending request changed an omitted option"));
                }
                Self::from(history)
            }
            _ => return Err(serde::de::Error::custom("owner request has no supported explicit approval_scope")),
        };
        Ok(request)
    }
}

impl BrowserUploadOperatorRequest {
    pub fn validate(&self) -> Result<(), String> {
        let exact = |s: &str| !s.is_empty() && s.trim() == s && s.len() <= 4096 && !s.contains('\0');
        let hex = |s: &str, len: usize| s.len() == len && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if self.schema_version != 1 || self.approval_scope != "browser_file_upload"
            || self.call_id != self.original.correlation_id || self.original.tool == self.resume.tool
            || self.original.source_movement_seq == 0 || self.original.source_movement_seq > 9_007_199_254_740_991
            || !self.original.arguments.is_object() || self.upload.files.is_empty()
            || self.upload.backend_node_id == 0 || self.upload.backend_node_id > 9_007_199_254_740_991
            || [&self.call_id, &self.original.run, &self.original.ee, self.original.worker.as_str(), self.original.door_interface.as_str(),
                &self.original.tool, &self.resume.tool, &self.scope.worker_environment_id, &self.scope.formation_pin, &self.scope.host_generation,
                &self.upload.browser_context_id, &self.upload.target_id].into_iter().any(|s| !exact(s))
            || [&self.upload.reservation_id, &self.upload.file_chooser_id, &self.upload.document_token].into_iter().any(|s| !hex(s, 32)) {
            return Err("browser upload changed its exact original call or retained chooser scope".into());
        }
        let origin = url::Url::parse(&self.upload.top_level_origin).map_err(|e| e.to_string())?;
        if !matches!(origin.scheme(), "http" | "https") || origin.origin().ascii_serialization() != self.upload.top_level_origin {
            return Err("browser upload has no canonical observed HTTP(S) page origin".into());
        }
        let mut bytes = 0u64;
        for file in &self.upload.files {
            if file.filename.is_empty() || file.filename.len() > 255 || matches!(file.filename.as_str(), "." | "..")
                || file.filename.chars().any(|c| matches!(c, '\0' | '/' | '\\')) || !hex(&file.payload_sha256, 64) {
                return Err("browser upload has an invalid selected basename or captured payload SHA256".into());
            }
            bytes = bytes.checked_add(file.size_bytes).ok_or("browser upload byte count overflow")?;
        }
        if bytes > 268_435_456 { return Err("browser upload exceeds the declared room staging bound".into()); }
        let selections = self.upload.files.iter().map(|file| serde_json::json!({"artifact_ref":file.artifact_ref,"filename":file.filename})).collect::<Vec<_>>();
        if self.original.arguments.get("reservation_id") != Some(&serde_json::json!(self.upload.reservation_id))
            || self.original.arguments.get("file_chooser_id") != Some(&serde_json::json!(self.upload.file_chooser_id))
            || self.original.arguments.get("files") != Some(&serde_json::json!(selections)) {
            return Err("browser upload selections differ from the original admitted tool arguments".into());
        }
        let mut commitment = serde_json::json!({"schema_version":self.schema_version,"original":self.original,"scope":self.scope,
            "approval_scope":self.approval_scope,"upload":self.upload});
        if let Some(offer) = &self.permission_offer {
            offer.validate()?;
            commitment["permission_offer"] = serde_json::json!(offer);
        }
        let digest = format!("{:x}", Sha256::digest(octopus_descriptors::canonical_bytes(&commitment)));
        if self.request_id != format!("sha256:{digest}") || self.resume.correlation_id != format!("browser-upload-resume:{digest}") {
            return Err("browser upload request/resume identity changed its immutable commitment".into());
        }
        Ok(())
    }
}

impl OwnerOperatorRequest {
    pub fn validate(&self) -> Result<(), String> {
        let value = serde_json::to_value(self).map_err(|e| e.to_string())?;
        match self.subject {
            OwnerApprovalSubject::Application(_) => serde_json::from_value::<ApplicationOperatorRequest>(value).map_err(|e| e.to_string())?.validate(),
            OwnerApprovalSubject::BrowserUpload(_) => serde_json::from_value::<BrowserUploadOperatorRequest>(value).map_err(|e| e.to_string())?.validate(),
            OwnerApprovalSubject::BrowserHistory(_) => serde_json::from_value::<BrowserHistoryOperatorRequest>(value).map_err(|e| e.to_string())?.validate(),
        }
    }

    pub fn validate_operator_interface(&self, interface: &InterfaceId) -> Result<(), String> {
        if self.permission_offer.as_ref().is_some_and(|offer| &offer.client_interface != interface) {
            return Err("permission offer differs from the exact declared operator interface".into());
        }
        Ok(())
    }

    pub fn verification_params(&self) -> serde_json::Value {
        let mut params = serde_json::json!({"request_id":self.request_id,"call_id":self.call_id});
        if let Some(offer) = &self.permission_offer {
            params["offered_choices"] = serde_json::json!(offer.offered_choices);
        }
        params
    }

    pub fn parse_operator_response(&self, value: serde_json::Value) -> Result<octopus_app_server_interface::OperatorPermissionResponse, String> {
        let response: octopus_app_server_interface::OperatorPermissionResponse = serde_json::from_value(value).map_err(|e| e.to_string())?;
        response.validate(&self.request_id, &self.call_id, self.permission_offer.as_ref())?;
        Ok(response)
    }

    pub fn projection(&self) -> serde_json::Value {
        let mut projection = match &self.subject {
            OwnerApprovalSubject::Application(application) => serde_json::json!({"kind":"application","request_id":self.request_id,"call_id":self.call_id,
                "title":format!("Allow access to {}?",application.display_name),"detail":{"application":application,"approval_scope":"formed_app_target"}}),
            OwnerApprovalSubject::BrowserUpload(upload) => serde_json::json!({"kind":"browser_file_upload","request_id":self.request_id,"call_id":self.call_id,
                "title":"Allow file upload?","detail":{"upload":upload,"approval_scope":"browser_file_upload"}}),
            OwnerApprovalSubject::BrowserHistory(history) => serde_json::json!({"kind":"browser_history_read","request_id":self.request_id,"call_id":self.call_id,
                "title":"Allow browsing history access?","detail":{"history":history,"approval_scope":"browser_history_read"}}),
        };
        if let Some(offer) = &self.permission_offer { projection["detail"]["permission_offer"] = serde_json::json!(offer); }
        if let Some(policy) = &self.native_policy { projection["detail"]["native_policy"] = serde_json::json!(policy); }
        projection
    }

    pub fn validate_bound_files(&self, metadata: &octopus_interfaces::InvocationMetadata) -> Result<(), String> {
        if let OwnerApprovalSubject::BrowserHistory(history) = &self.subject {
            if self.permission_offer.is_none()
                || !metadata.artifact_binding_refs.iter().filter(|bound| bound.id == history.source.id).eq([&history.source])
                || !metadata.grant.artifact_access.get(history.source.id.as_str()).is_some_and(|access| access.read) {
                return Err("browser history lost its required offer or exact readable source binding".into());
            }
        }
        let OwnerApprovalSubject::BrowserUpload(upload) = &self.subject else { return Ok(()); };
        for file in &upload.files {
            if !metadata.artifact_binding_refs.iter().filter(|bound| bound.id == file.artifact_ref.id).eq([&file.artifact_ref])
                || !metadata.grant.artifact_access.get(file.artifact_ref.id.as_str()).is_some_and(|access| access.read) {
                return Err("browser upload lost an exact currently readable original file ArtifactRef; staging does not restore permission".into());
            }
        }
        Ok(())
    }
}
