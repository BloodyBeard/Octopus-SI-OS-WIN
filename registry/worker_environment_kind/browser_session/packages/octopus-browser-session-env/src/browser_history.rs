use crate::browser_consent::{self as consent, Offer, HistoryPolicy};
use crate::DecodedRoomInvocation;
use octopus_descriptors::InterfaceId;
use octopus_interfaces::{ArtifactRef, InterfaceInvocationEnvelope};
use octopus_worker_env::WorkerEnvironmentFormation;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const SOURCE_BYTES: usize = 65536;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct HistoryAccess {
    pub request_tools: Vec<String>,
    pub operator_interface: InterfaceId,
    pub resume_tool: String,
    pub permission_policy: HistoryPolicy,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct HistoryCollection {
    pub source: ArtifactRef,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceDefinition {
    pub schema_version: u32,
    pub source_id: String,
    pub owner_interface: InterfaceId,
    pub collection: String,
    pub query_policy: QueryPolicy,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct QueryPolicy {
    pub model_read_enabled: bool,
    pub request_timeout_ms: u64,
    pub max_queries: usize,
    pub max_query_bytes: usize,
    pub max_results: usize,
    pub max_result_bytes: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Query {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queries: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeSourceIdentity {
    pub source_ref: ArtifactRef,
    pub source_payload_digest: String,
    pub source_epoch: String,
    pub host_generation: String,
    pub query: Query,
}

#[derive(Clone)]
pub(crate) struct CapturedSource {
    pub reference: ArtifactRef,
    pub bytes: Vec<u8>,
    pub payload_digest: String,
    pub definition: SourceDefinition,
}

impl CapturedSource {
    pub(crate) fn from_envelope(envelope: &InterfaceInvocationEnvelope, reference: &ArtifactRef) -> Result<Self, String> {
        let metadata = envelope.metadata().map_err(|error| error.to_string())?;
        if !metadata.artifact_binding_refs.iter().filter(|item| item.id == reference.id).eq([reference])
            || !metadata.grant.artifact_access.get(reference.id.as_str()).is_some_and(|access| access.read) {
            return Err("History source requires its own exact currently readable artifact binding".into());
        }
        let mut matches = envelope.artifact_binding_payloads.iter().filter(|item| item.artifact.id == reference.id);
        let payload = matches.next().ok_or("History source has no admitted captured payload")?;
        if matches.next().is_some() || payload.artifact != *reference || payload.payload_len() > SOURCE_BYTES as u64 {
            return Err("History source changed its bound identity or definition byte bound".into());
        }
        let bytes = payload.read_bounded(SOURCE_BYTES as u64).map_err(|error| error.to_string())?;
        let definition: SourceDefinition = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        definition.validate()?;
        let payload_digest = hash(&bytes);
        Ok(Self {reference:reference.clone(), bytes:bytes.into_owned(), payload_digest, definition})
    }

    pub(crate) fn same_source(&self, identity: &NativeSourceIdentity, query: &Query) -> Result<(), String> {
        if identity.source_ref != self.reference || identity.source_payload_digest != self.payload_digest
            || !uuid(&identity.source_epoch) || !hex(&identity.host_generation, 32) || identity.query != *query {
            return Err("Native history preparation changed its actual source, epoch, host or original query".into());
        }
        Ok(())
    }
}

impl SourceDefinition {
    fn validate(&self) -> Result<(), String> {
        let limits = &self.query_policy;
        let owner = self.owner_interface.as_str();
        if self.schema_version != 1 || !uuid(&self.source_id) || self.collection != "explicit_door_selection"
            || owner.is_empty() || owner.len() > 2048 || owner.trim() != owner || owner.chars().any(char::is_control)
            || !(1..=300000).contains(&limits.request_timeout_ms)
            || !(1..=i32::MAX as usize).contains(&limits.max_queries)
            || !(1..=SOURCE_BYTES).contains(&limits.max_query_bytes)
            || !(1..=i32::MAX as usize).contains(&limits.max_results)
            || !(1..=96 * 1024 * 1024).contains(&limits.max_result_bytes) {
            return Err("History source definition differs from its owned native contract".into());
        }
        Ok(())
    }
}

impl Query {
    pub(crate) fn from_arguments(arguments: &Value) -> Result<(ArtifactRef, Self), String> {
        let mut fields = arguments.as_object().ok_or("History query must be an object")?.clone();
        let source = serde_json::from_value(fields.remove("source").ok_or("History query requires its explicit source ref")?)
            .map_err(|error| error.to_string())?;
        let value = Value::Object(fields);
        let query: Self = serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
        if serde_json::to_value(&query).map_err(|error| error.to_string())? != value {
            return Err("History query cannot replace an omitted option with null".into());
        }
        Ok((source, query))
    }

    pub(crate) fn validate(&self, policy: &QueryPolicy) -> Result<(), String> {
        if !policy.model_read_enabled { return Err("Model history access is not enabled in this selected source".into()); }
        let default_queries = vec![String::new()];
        let queries = self.queries.as_ref().unwrap_or(&default_queries);
        if queries.is_empty() || queries.len() > policy.max_queries
            || serde_json::to_vec(queries).map_err(|error| error.to_string())?.len() > policy.max_query_bytes
            || self.limit.unwrap_or(100) == 0 || self.limit.unwrap_or(100) > policy.max_results {
            return Err("History query exceeds the selected source's declared bounds".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct OriginalCall {
    run: String, ee: String, worker: InterfaceId, door_interface: InterfaceId,
    tool: String, correlation_id: String, arguments: Value,
    source_movement_seq: u64, source_artifact: ArtifactRef,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Scope { worker_environment_id: String, formation_pin: String, host_generation: String }
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct History { source: ArtifactRef, source_payload_digest: String, source_epoch: String, query: Query }
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Resume { tool: String, correlation_id: String }

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    schema_version: u32, request_id: String, call_id: String, original: OriginalCall,
    scope: Scope, approval_scope: String, history: History, resume: Resume,
    permission_offer: Offer,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Pending {
    pub request: Request,
    pub formation: WorkerEnvironmentFormation,
    pub operator_interface: InterfaceId,
    pub provider_interface: InterfaceId,
}

impl Pending {
    pub(crate) fn prepare(decoded: &DecodedRoomInvocation, config: &HistoryAccess,
        source: &CapturedSource, native: &NativeSourceIdentity) -> Result<Self, String> {
        let (reference, query) = Query::from_arguments(&decoded.call.arguments)?;
        if reference != source.reference { return Err("History query selected another source".into()); }
        query.validate(&source.definition.query_policy)?;
        source.same_source(native, &query)?;
        let tool = decoded.call.tool_id.as_str();
        let unique: std::collections::BTreeSet<_> = config.request_tools.iter().collect();
        let metadata = decoded.envelope.metadata().map_err(|error| error.to_string())?;
        let bindings = metadata.checkpoint_binding_policy.as_ref().ok_or("History request has no exact binding snapshot")?;
        if config.request_tools.is_empty() || unique.len() != config.request_tools.len()
            || config.request_tools.iter().any(|id| id.trim().is_empty() || id == &config.resume_tool)
            || config.request_tools.iter().filter(|id| id.as_str() == tool).count() != 1
            || !metadata.grant.interfaces.contains(&config.operator_interface.to_string())
            || [&config.resume_tool, &decoded.call.tool_id].into_iter().any(|id| !metadata.grant.tools.contains(id)
                || metadata.grant.hard_blocked_tools.contains(id) || !decoded.formation.granted_tools.contains(id))
            || ["operator.approval", "operator.input.provided"].into_iter().any(|mark| !metadata.grant.mark_outcomes.iter().any(|id| id == mark))
            || decoded.source_movement_seq == 0 || decoded.source_movement_seq > MAX_SAFE_INTEGER {
            return Err("History request and private resume lack their exact current declared authority".into());
        }
        let correlation = decoded.correlation_id.clone().filter(|id| !id.trim().is_empty()).ok_or("History request has no exact original call id")?;
        let original = OriginalCall {run:decoded.formation.run.clone(), ee:decoded.formation.ee.clone(), worker:decoded.formation.worker.clone(),
            door_interface:decoded.formation.door_interface.clone(), tool:tool.into(), correlation_id:correlation.clone(), arguments:decoded.call.arguments.clone(),
            source_movement_seq:decoded.source_movement_seq, source_artifact:decoded.envelope.artifact.clone()};
        let offer = config.permission_policy.offer(&config.operator_interface)?;
        let mut request = Request {schema_version:1, request_id:String::new(), call_id:correlation, original,
            scope:Scope {worker_environment_id:decoded.formation.worker_environment_id.to_string(), formation_pin:decoded.formation.formation_pin.clone(), host_generation:native.host_generation.clone()},
            approval_scope:"browser_history_read".into(), history:History {source:source.reference.clone(), source_payload_digest:source.payload_digest.clone(), source_epoch:native.source_epoch.clone(), query},
            resume:Resume {tool:config.resume_tool.clone(), correlation_id:String::new()}, permission_offer:offer};
        let digest = request.commitment(); request.request_id = format!("sha256:{digest}"); request.resume.correlation_id = format!("browser-history-resume:{digest}");
        request.validate(&config.permission_policy.offer(&config.operator_interface)?)?;
        Ok(Self {request, formation:decoded.formation.clone(), operator_interface:config.operator_interface.clone(), provider_interface:bindings.inbound.interface.clone()})
    }
}

impl Request {
    fn commitment(&self) -> String {
        hash(&octopus_descriptors::canonical_bytes(&json!({"schema_version":self.schema_version,"original":self.original,"scope":self.scope,
            "approval_scope":self.approval_scope,"history":self.history,"permission_offer":self.permission_offer})))
    }
    pub(crate) fn validate(&self, offer: &Offer) -> Result<(), String> {
        consent::validate_offer(Some(&self.permission_offer),Some(offer))?;
        let (source, query) = Query::from_arguments(&self.original.arguments)?;
        let digest = self.commitment();
        if self.schema_version != 1 || self.approval_scope != "browser_history_read" || self.request_id != format!("sha256:{digest}")
            || self.resume.correlation_id != format!("browser-history-resume:{digest}") || self.call_id != self.original.correlation_id
            || self.resume.tool == self.original.tool || source != self.history.source || query != self.history.query
            || self.original.source_movement_seq == 0 || self.original.source_movement_seq > MAX_SAFE_INTEGER
            || !hex(&self.history.source_payload_digest, 64) || !uuid(&self.history.source_epoch) || !hex(&self.scope.host_generation, 32)
 {
            return Err("History pending request changed its immutable source/query/proof commitment".into());
        }
        Ok(())
    }
    pub(crate) fn body(&self) -> Value { json!({"effect_performed":false,"operator_request":self}) }

    pub(crate) fn validate_result(&self, value: &Value, policy: &QueryPolicy) -> Result<(), String> {
        let query_digest = hash(&octopus_descriptors::canonical_bytes(&json!(self.history.query)));
        let evidence_valid = match (value.get("native_query_evidence"), value.get("standard_query_evidence")) {
            (Some(evidence), None) => serde_json::from_value::<NativeQueryEvidence>(evidence.clone()).is_ok_and(|evidence|
                evidence.query_digest == query_digest && hex(&evidence.request_id, 32)
                && evidence.native_request_ids.len() == self.history.query.queries.as_ref().map_or(1, Vec::len)
                && evidence.native_request_ids.iter().all(|id| hex(id, 32))),
            (None, Some(evidence)) => serde_json::from_value::<StandardQueryEvidence>(evidence.clone()).is_ok_and(|evidence|
                evidence.api == "octopus.standardNavigationHistory.v1" && evidence.query_digest == query_digest
                && uuid(&evidence.request_id) && evidence.matching_algorithm == "literalSubstring"
                && evidence.timestamp == "main_process_event_observation" && evidence.day_timezone.is_null()
                && (1..=100000).contains(&evidence.retention.max_entries)
                && (1024..=96 * 1024 * 1024).contains(&evidence.retention.max_bytes)),
            _ => false,
        };
        if !evidence_valid || value["status"] != "completed" || value["effect_performed"] != false
            || value["request_id"] != self.request_id || value["host_generation"] != self.scope.host_generation
            || value.get("source_ref") != Some(&json!(self.history.source))
            || value["source_payload_digest"] != self.history.source_payload_digest || value["source_epoch"] != self.history.source_epoch
            || !value["items"].as_array().is_some_and(|items| items.len() <= self.history.query.limit.unwrap_or(100)
                && items.len() <= policy.max_results)
            || serde_json::to_vec(value).map_err(|error| error.to_string())?.len() > policy.max_result_bytes {
            return Err("History query did not prove the approved source, request and supported backend".into());
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeQueryEvidence { request_id: String, native_request_ids: Vec<String>, query_digest: String }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StandardQueryEvidence {
    api: String, request_id: String, query_digest: String, matching_algorithm: String,
    timestamp: String, day_timezone: Value, retention: StandardHistoryRetention,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StandardHistoryRetention { max_entries: usize, max_bytes: usize }

fn hash(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }
fn hex(value: &str, length: usize) -> bool { value.len() == length && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) }
fn uuid(value: &str) -> bool { value.len() == 36 && value.split('-').map(str::len).eq([8,4,4,4,12]) && value.split('-').all(|part| hex(part, part.len())) }

use super::{BrowserRoom, BrowserTransport};
use super::filechooser::{create_record, create_record_bounded, digest_id, private_directory, read_record, read_record_bounded, response, Decision, RESPONSE};

const RESULT_RECORD_BYTES: usize = (96 + 16) * 1024 * 1024;
use base64::Engine;
use octopus_interfaces::InterfaceArtifactFile;
use octopus_tool_door_runtime::ToolProcessError;
use octopus_tool_handlers::{ToolCallArtifact, ToolResultArtifact, ToolResultError};
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Consumption { response_artifact: ArtifactRef, decision: Decision }

pub(crate) fn decoded_response(envelope: &InterfaceInvocationEnvelope, tool: &str) -> Result<ToolCallArtifact, String> {
    let response = response(envelope)?;
    Ok(ToolCallArtifact {tool:tool.into(), arguments:json!({"request_id":response.request_id}),
        correlation_id:Some(format!("browser-history-resume:{}", digest_id(&response.request_id)?)),
        label:None, source_interface_evidence:BTreeMap::new()})
}

impl HistoryAccess {
    fn offered(&self, decoded: &DecodedRoomInvocation) -> Result<Offer, String> {
        let metadata = decoded.envelope.metadata().map_err(|error| error.to_string())?;
        let unique: std::collections::BTreeSet<_> = self.request_tools.iter().collect();
        if self.request_tools.is_empty() || unique.len() != self.request_tools.len() || self.resume_tool.trim().is_empty()
            || self.request_tools.iter().any(|id| id.trim().is_empty() || id.trim() != id.as_str() || id == &self.resume_tool)
            || self.resume_tool.trim() != self.resume_tool
            || (decoded.operation == crate::BrowserOperation::History && !self.request_tools.contains(&decoded.call.tool_id))
            || (decoded.operation == crate::BrowserOperation::HistoryResume && decoded.call.tool_id != self.resume_tool)
            || !metadata.grant.interfaces.contains(&self.operator_interface.to_string())
            || [&self.resume_tool, &decoded.call.tool_id].into_iter().any(|id| !metadata.grant.tools.contains(id)
                || metadata.grant.hard_blocked_tools.contains(id) || !decoded.formation.granted_tools.contains(id))
            || ["operator.approval", "operator.input.provided"].into_iter().any(|mark| !metadata.grant.mark_outcomes.iter().any(|id| id == mark)) {
            return Err("History read and private continuation require their exact current declared tools and operator boundary".into());
        }
        self.permission_policy.offer(&self.operator_interface)
    }
}

impl BrowserRoom {
    pub(super) async fn receive_history(&self, decoded: &DecodedRoomInvocation) -> Result<InterfaceArtifactFile, ToolProcessError> {
        let mut authorized = false;
        let result = async {
            let BrowserTransport::Native(transport) = &self.transport else { return Err("History requires the native browser host transport".into()); };
            let config = self.history_access.as_ref().ok_or("This browser door declares no history read selection")?;
            let offer = config.offered(decoded)?;
            transport.authorize(decoded).await?;
            authorized = true;
            match decoded.operation {
                crate::BrowserOperation::History => self.prepare_history(decoded, config, &offer).await,
                crate::BrowserOperation::HistoryResume => self.resume_history(decoded, config, &offer).await,
                _ => Err("Unexpected operation in history source dispatch".into()),
            }
        }.await;
        let response = crate::browser_result(&decoded.call.tool_id, decoded.correlation_id.clone(), result, Vec::new());
        let transport = match &self.transport { BrowserTransport::Native(value) if authorized => Some(value), _ => None };
        crate::browser_wait_completion::finish(&decoded.envelope, response, transport, None).await
    }

    async fn prepare_history(&self, decoded: &DecodedRoomInvocation, config: &HistoryAccess, offer: &Offer) -> Result<Value, String> {
        let (reference, query) = Query::from_arguments(&decoded.call.arguments)?;
        let source = CapturedSource::from_envelope(&decoded.envelope, &reference)?;
        query.validate(&source.definition.query_policy)?;
        let BrowserTransport::Native(transport) = &self.transport else { return Err("History has no native source transport".into()); };
        let mut connection = transport.connect(None).await?;
        let result = async {
            let value = connection.request("OctopusBrowser.prepareHistorySource", json!({"source_ref":source.reference,
                "source_payload_digest":source.payload_digest,"payload_base64":base64::engine::general_purpose::STANDARD.encode(&source.bytes),"query":query}),
                Duration::from_millis(source.definition.query_policy.request_timeout_ms)).await?;
            let native: NativeSourceIdentity = serde_json::from_value(value).map_err(|error| error.to_string())?;
            source.same_source(&native, &query)?;
            if native.host_generation != connection.generation { return Err("History preparation returned another native boot generation".into()); }
            let pending = Pending::prepare(decoded, config, &source, &native)?;
            pending.request.validate(offer)?;
            let prepared = connection.request("OctopusBrowser.stageHistoryRequest", json!({"request":pending.request}), transport.request_timeout).await?;
            if prepared != json!({"status":"prepared","request_id":pending.request.request_id,"source_epoch":native.source_epoch,"host_generation":native.host_generation}) {
                return Err("Native history request was not retained under its exact source epoch".into());
            }
            let root = private_directory(&self.state_dir, &["history-pending"])?;
            create_record(&root.join(format!("{}.request.json", digest_id(&pending.request.request_id)?)), &pending)?;
            let permission=connection.request("OctopusBrowser.lookupPermission",json!({"request_id":pending.request.request_id}),transport.request_timeout).await?;
            if !permission.is_null() {
                let mut value=connection.request("OctopusBrowser.applyHistoryRequest",json!({"request_id":pending.request.request_id,"decision":permission["response"]["decision"],"permission_entry_id":permission["id"]}),Duration::from_millis(source.definition.query_policy.request_timeout_ms)).await?;
                pending.request.validate_result(&value, &source.definition.query_policy)?;
                value["permission_reuse"]=permission; return Ok(value);
            }
            Ok(pending.request.body())
        }.await;
        connection.close().await;
        result
    }

    pub(super) async fn select_history_collection(&self, decoded: &DecodedRoomInvocation) -> Result<(), String> {
        let Some(config) = &self.history_collection else { return Ok(()); };
        if !matches!(decoded.operation, crate::BrowserOperation::Navigate | crate::BrowserOperation::TabNew) {
            return Err("History collection can be selected only by an admitted page-creation operation".into());
        }
        let source = CapturedSource::from_envelope(&decoded.envelope, &config.source)?;
        let BrowserTransport::Native(transport) = &self.transport else { return Err("Selected history collection requires the native browser host transport".into()); };
        let mut connection = transport.connect(None).await?;
        let result = connection.request("OctopusBrowser.selectHistoryCollection", json!({"source_ref":source.reference,
            "source_payload_digest":source.payload_digest,"payload_base64":base64::engine::general_purpose::STANDARD.encode(&source.bytes)}),
            Duration::from_millis(source.definition.query_policy.request_timeout_ms)).await;
        connection.close().await;
        let value = result?;
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Selected { source_ref:ArtifactRef, source_payload_digest:String, source_epoch:String, host_generation:String }
        let value: Selected = serde_json::from_value(value).map_err(|error| error.to_string())?;
        if value.source_ref != source.reference || value.source_payload_digest != source.payload_digest
            || !uuid(&value.source_epoch) || value.host_generation != connection.generation {
            return Err("Native collector differs from the door's exact selected source".into());
        }
        Ok(())
    }

    async fn resume_history(&self, decoded: &DecodedRoomInvocation, config: &HistoryAccess, offer: &Offer) -> Result<Value, String> {
        let response = response(&decoded.envelope)?;
        let digest = digest_id(&response.request_id)?;
        let root = private_directory(&self.state_dir, &["history-pending"])?;
        let pending: Pending = read_record(&root.join(format!("{digest}.request.json")))?;
        let request = &pending.request;
        request.validate(offer)?;
        let metadata = decoded.envelope.metadata().map_err(|error| error.to_string())?;
        let runtime = decoded.envelope.runtime().map_err(|error| error.to_string())?;
        let bindings = metadata.checkpoint_binding_policy.as_ref().ok_or("History resume lost its binding snapshot")?;
        let admission = metadata.checkpoint_admission.as_ref().ok_or("History resume has no checkpoint admission")?;
        let (mark, verifier) = match response.decision { Decision::Accept => ("operator.approval","operator_approval"), Decision::Decline | Decision::Cancel => ("operator.input.provided","operator_input") };
        if pending.formation != decoded.formation
            || response.request_id != request.request_id || response.call_id != request.call_id
            || decoded.source_movement_seq <= request.original.source_movement_seq || request.resume.tool != decoded.call.tool_id
            || request.resume.tool != config.resume_tool || !config.request_tools.contains(&request.original.tool)
            || pending.operator_interface != config.operator_interface || runtime.interface != pending.formation.door_interface
            || runtime.worker != request.original.worker || runtime.run.as_str() != request.original.run || runtime.ee.as_str() != request.original.ee
            || bindings.inbound.interface != pending.operator_interface || !metadata.grant.interfaces.contains(&pending.operator_interface.to_string())
            || !metadata.grant.interfaces.contains(&pending.formation.door_interface.to_string())
            || !admission.complete || admission.artifact_label != RESPONSE || admission.required_artifact_labels != [RESPONSE]
            || metadata.verification_failure.is_some() || metadata.required_verified_artifacts.len() != 1
            || metadata.required_verified_artifacts[0].mark.as_str() != mark || metadata.required_verified_artifacts[0].verifier.as_str() != verifier
            || metadata.required_verified_artifacts[0].artifact_label != RESPONSE
            || metadata.required_verified_artifacts[0].params != consent::params(&request.request_id,&request.call_id,Some(&request.permission_offer))
            || !metadata.grant.mark_outcomes.iter().any(|value| value == mark)
            || metadata.outbound_interface_bindings != [pending.provider_interface.clone(),pending.formation.door_interface.clone()]
            || bindings.outbound.iter().map(|binding| &binding.interface).collect::<Vec<_>>() != [&pending.provider_interface,&pending.formation.door_interface]
            || [&request.original.tool,&request.resume.tool].into_iter().any(|tool| !metadata.grant.tools.contains(tool)
                || metadata.grant.hard_blocked_tools.contains(tool) || !decoded.formation.granted_tools.contains(tool)) {
            return Err("History decision differs from the exact verifier-admitted request and formed room".into());
        }
        response.validate(&request.request_id,&request.call_id,Some(&request.permission_offer))?;
        let source = CapturedSource::from_envelope(&decoded.envelope, &request.history.source)?;
        if source.payload_digest != request.history.source_payload_digest { return Err("Approved history source bytes changed".into()); }
        request.history.query.validate(&source.definition.query_policy)?;
        let consumption = Consumption {response_artifact:decoded.envelope.artifact.clone(),decision:response.decision};
        let consumed = root.join(format!("{digest}.consumed.json"));
        let result_path = root.join(format!("{digest}.result.json"));
        if consumed.try_exists().map_err(|error| error.to_string())? {
            if read_record::<Consumption>(&consumed)? != consumption { return Err("History request consumed another response".into()); }
            if result_path.try_exists().map_err(|error| error.to_string())? { return read_record_bounded(&result_path, RESULT_RECORD_BYTES); }
            let original = ToolResultArtifact::failure(&request.original.tool,Some(request.call_id.clone()),ToolResultError::new("history_read_uncertain",
                "History request was consumed without a recorded terminal result; the read will not be repeated"));
            return Ok(json!({"request_id":request.request_id,"original_result":original}));
        }
        create_record(&consumed, &consumption)?;
        let applied = async {
            let BrowserTransport::Native(transport) = &self.transport else { return Err("History resume has no native transport".into()); };
            let mut connection = transport.connect(None).await?;
            if connection.generation != request.scope.host_generation { connection.close().await; return Err("History proof belongs to a retired native host generation".into()); }
            let saved = match connection.request("OctopusBrowser.rememberPermission",json!({"request_id":request.request_id,"response":response,"source_artifact":decoded.envelope.artifact}),transport.request_timeout).await {
                Ok(value)=>value,Err(error)=>json!({"status":"not_saved","error":error})
            };
            let result = connection.request("OctopusBrowser.applyHistoryRequest", json!({"request_id":request.request_id,"decision":response.decision}),
                Duration::from_millis(source.definition.query_policy.request_timeout_ms)).await;
            connection.close().await;
            result.map(|mut value| {value["permission_persistence"]=saved;value})
        }.await;
        let original = match applied {
            Ok(value) if response.decision == Decision::Accept && request.validate_result(&value, &source.definition.query_policy).is_ok() =>
                ToolResultArtifact::success(&request.original.tool,Some(request.call_id.clone()),value),
            Ok(value) => ToolResultArtifact::failure(&request.original.tool,Some(request.call_id.clone()),ToolResultError::new(
                if response.decision == Decision::Cancel {"history_cancelled"} else if response.decision == Decision::Decline {"history_declined"} else {"history_read_unconfirmed"},
                "History request did not return the approved source's completed query")
                .with_details(serde_json::Map::from_iter([("native_result".into(),value)]))),
            Err(error) => ToolResultArtifact::failure(&request.original.tool,Some(request.call_id.clone()),ToolResultError::new("history_read_unavailable_or_uncertain",error)),
        };
        let result = json!({"request_id":request.request_id,"original_result":original});
        create_record_bounded(&result_path, &result, RESULT_RECORD_BYTES)?;
        Ok(result)
    }
}
