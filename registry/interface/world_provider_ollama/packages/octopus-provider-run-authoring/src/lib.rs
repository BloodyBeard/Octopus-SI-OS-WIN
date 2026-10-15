pub use octopus_file_write_contract::{DecodeBudget, JsonFormat, Preimage};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{BufRead, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};

use octopus_descriptors::{ArtifactId, CheckpointId, InterfaceId, RunId};
use octopus_interface_transport::InterfaceHostIoLimits;
use octopus_interface_transport::InterfaceLiftContext;
use octopus_interfaces::{
    CheckpointBindingPolicySnapshot, DeliveryVerdict, InterfaceArtifactFile,
    InterfaceArtifactManifest, InterfaceDescriptor, InterfaceInvocationEnvelope,
    InvocationMetadata,
};
use octopus_ledger::{ArtifactRef, Endpoint, Movement, MovementKind};
use octopus_ledger_index::LedgerSqliteReader;
use octopus_ledger_store::DurableMovementDecoder;
use octopus_runtime::{
    AuthoredInterfaceBinding, BindingArtifactPolicy, ContentHash, InterfaceBindingConfig,
    RequiredVerifiedArtifact,
    run_record::{ArtifactBinding, RunRecord},
};
use octopus_tool_handlers::ToolResultArtifact;
use octopus_world_pyecho_life_runtime::LedgerTailReader;
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::Message as WsMessage;

const FRONTIER_CHECKPOINT_IDS: &str = "frontier_checkpoint_ids";
const RUN_AUTHORING_BINDING: &str = "run_authoring_binding";
const CONTEXT_REFERENCE_LABEL: &str = "context.reference";
const INSTRUCTION_TEXT_LABEL: &str = "instruction.text";
const SESSION_CONTEXT_FIELD: &str = "session_context";
#[cfg(test)]
const TOOL_ROUTE_VERIFIER: &str = "tool_call_allowed";
const PROVIDER_HANDOFF_COMPLETE_LABEL: &str = "provider.handoff.complete";
const ONE_SHOT_PROGRESS_FIELD: &str = "provider_handoff_progress";
pub const PROVIDER_TRANSPORT_DISPATCH_LABEL: &str = "provider.transport.dispatch";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderEmissionRoute {
    ClientProjection,
    ClientProjectionWithVerifiedInputs { inputs: Vec<ProviderVerifiedInput> },
    Tool { tool: String },
    Interface { interface: InterfaceId },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderVerifiedInput {
    pub also_deliver_to_tool: Option<String>,
    pub artifact_label: String,
    pub required_verified_artifact: RequiredVerifiedArtifact,
}

pub struct ProviderEmission {
    pub artifact: InterfaceArtifactFile,
    pub route: ProviderEmissionRoute,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderOutgoingBinding {
    pub interface: InterfaceId,
    pub artifact_label: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_bindings: Vec<ArtifactId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ProviderHandoffRoute {
    ClientProjection,
    ClientProjectionWithVerifiedInputs { inputs: Vec<ProviderVerifiedInput> },
    Tool { tool: String },
    Interface { interface: InterfaceId },
    TransportDispatch { attempt: u32 },
}

struct ProviderHandoffEmission {
    artifact: InterfaceArtifactFile,
    route: ProviderHandoffRoute,
}

impl From<ProviderEmission> for ProviderHandoffEmission {
    fn from(emission: ProviderEmission) -> Self {
        let route = match emission.route {
            ProviderEmissionRoute::ClientProjection => ProviderHandoffRoute::ClientProjection,
            ProviderEmissionRoute::ClientProjectionWithVerifiedInputs { inputs } => {
                ProviderHandoffRoute::ClientProjectionWithVerifiedInputs { inputs }
            }
            ProviderEmissionRoute::Tool { tool } => ProviderHandoffRoute::Tool { tool },
            ProviderEmissionRoute::Interface { interface } => {
                ProviderHandoffRoute::Interface { interface }
            }
        };
        Self {
            artifact: emission.artifact,
            route,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderSourceRecovery {
    Process,
    Completed,
    Interrupted { last_artifact_label: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderHandoffOutcome {
    Admitted {
        seq: u64,
        artifact: ArtifactRef,
    },
    TerminalFailure {
        failure_seq: u64,
        client_seq: u64,
        failure_class: String,
    },
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderTransportDispatch {
    label: String,
    provider_interface: InterfaceId,
    source_movement_seq: u64,
    attempt: u32,
    method: String,
    transport: String,
    endpoint: ProviderTransportEndpoint,
    request: ProviderTransportRequest,
    credential_handle_fingerprint: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderTransportEndpoint {
    scheme: String,
    host: String,
    port: Option<u16>,
    path: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderTransportRequest {
    bytes: u64,
    blake3: String,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderHandoffCompletion {
    label: String,
    provider_interface: InterfaceId,
    source_movement_seq: u64,
    artifacts: Vec<ArtifactRef>,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct OneShotProgressIdentity {
    provider_interface: InterfaceId,
    run: RunId,
    source_movement_seq: u64,
}

#[derive(Clone)]
pub struct ProviderHandoffSession {
    interface: InterfaceId,
    movements_path: PathBuf,
    evidence: Arc<Mutex<ProviderLedgerEvidenceIndex>>,
    writer_history: Arc<Mutex<BTreeMap<RunId, writer_run::WriterHistory>>>,
    writer_call_generations: Arc<Mutex<writer_run::WriterCallIndex>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum HandoffDeliveryMode {
    OneShot(OneShotHandoffPlan),
    Rearm,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OneShotHandoffPlan {
    terminal: BTreeMap<String, OneShotHandoffSlot>,
    dispatch: BTreeMap<u32, OneShotHandoffSlot>,
    completion: OneShotHandoffSlot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OneShotHandoffSlot {
    checkpoint: CheckpointId,
    epoch: u64,
    required_mark: String,
    required_verifier: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OneShotAdmittedArtifact {
    artifact: ArtifactRef,
    arrival_seq: u64,
    mark_seq: u64,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct RuntimeCoordinateKey {
    run: String,
    worker: String,
    interface: String,
    checkpoint: String,
    epoch: u64,
}

#[derive(Clone, Debug)]
enum UniqueEvidence {
    One(u64),
    Repeated,
}

#[derive(Clone, Debug)]
enum UniqueArrivalCoordinate {
    One(RuntimeCoordinateKey),
    Repeated,
}

struct ProviderLedgerEvidenceIndex {
    interface: InterfaceId,
    movements_path: PathBuf,
    decoder: DurableMovementDecoder,
    max_row_bytes: u64,
    spotlight: Option<LedgerSqliteReader>,
    byte_offset: u64,
    observed_len: u64,
    next_seq: u64,
    through_seq: Option<u64>,
    poison: Option<String>,
    handoff_metadata_poison: HashMap<(String, String), String>,
    session_metadata_poison: HashMap<(String, String), String>,
    context_metadata_poison: HashMap<(String, String, String), String>,
    latest_run_movement: HashMap<(String, String), u64>,
    provider_run_arrivals: HashMap<(String, String, String), BTreeSet<u64>>,
    session_run_movements: HashMap<(String, String), BTreeSet<u64>>,
    session_provider_arrivals: HashMap<(String, String, String), BTreeSet<u64>>,
    session_provider_inputs: HashMap<(String, String), BTreeMap<u64, String>>,
    coordinate_arrivals: HashMap<(String, RuntimeCoordinateKey), UniqueEvidence>,
    mark_arrival_coordinates: HashMap<(String, String, String, u64), UniqueArrivalCoordinate>,
    coordinate_artifacts: HashMap<RuntimeCoordinateKey, Vec<(ArtifactRef, u64)>>,
    coordinate_outbounds: HashMap<RuntimeCoordinateKey, Vec<IndexedOutboundArtifact>>,
    coordinate_marks: HashMap<(String, RuntimeCoordinateKey, String, String), UniqueEvidence>,
}

#[derive(Clone, PartialEq, Eq)]
struct IndexedOutboundArtifact {
    source: InterfaceId,
    artifact: ArtifactRef,
    seq: u64,
    run_record_hash: Option<String>,
    outbound_interface_bindings: Vec<InterfaceId>,
    checkpoint_binding_policy: Option<CheckpointBindingPolicySnapshot>,
    required_verified_artifacts: Vec<RequiredVerifiedArtifact>,
    artifact_binding_refs: Vec<ArtifactRef>,
    readable_artifacts: BTreeSet<ArtifactId>,
    role_artifacts: Vec<String>,
}

struct AuthoringFailureSourceValidation<'a> {
    ctx: &'a InterfaceLiftContext,
    record: &'a RunRecord,
    record_bytes: &'a [u8],
    config: &'a RunAuthoringConfig,
    failure: &'a InterfaceFailureEnvelope,
    failure_seq: u64,
    after_seq: Option<u64>,
}

struct AuthoringAttemptSourceValidation<'a> {
    ctx: &'a InterfaceLiftContext,
    record: &'a RunRecord,
    record_bytes: &'a [u8],
    config: &'a RunAuthoringConfig,
    call: FileWriteCallEnvelope,
    manifest_path: PathBuf,
}

struct ValidatedAuthoringAttempt {
    manifest_path: PathBuf,
    provider_source_movement_seq: u64,
    authored: AuthoredRun,
}

enum AuthoringAttemptRecovery {
    None,
    InFlight(ValidatedAuthoringAttempt),
    OtherSource(u64),
}

struct ValidatedAuthoringFailureSource {
    provider_source_movement_seq: u64,
    correlation_id: String,
}

#[derive(Clone)]
pub struct ProviderOutputHandoff {
    ctx: InterfaceLiftContext,
    invocation_metadata: InvocationMetadata,
    config: RunAuthoringConfig,
    run_path: PathBuf,
    hand_in_root: PathBuf,
    var_dir: PathBuf,
    source_movement_seq: u64,
    source_carries_context: bool,
    failed_source: bool,
    checked_settlement_output: Option<(ArtifactRef, ArtifactRef)>,
    context_output_labels: BTreeSet<String>,
    session_snapshot_labels: BTreeSet<String>,
    granted_tools: BTreeSet<String>,
    granted_interfaces: BTreeSet<InterfaceId>,
    granted_mark_outcomes: BTreeSet<String>,
    bound_topology: BoundRunAuthoringTopology,
    delivery_mode: HandoffDeliveryMode,
    session: ProviderHandoffSession,
    invocation_session_snapshot: Option<ProviderInvocationSessionSnapshot>,
    state: Arc<tokio::sync::Mutex<HandoffState>>,
    result_reader: Arc<tokio::sync::Mutex<LedgerTailReader>>,
}

#[derive(Clone)]
struct HandoffState {
    validated_withdrawal_output: Option<ArtifactRef>,
    context_artifacts: Vec<ArtifactId>,
    static_context_artifacts: Vec<ArtifactId>,
    session_context: ResolvedSessionContext,
    session_snapshot_head: Option<SessionSnapshotHead>,
    run_record_bytes: Vec<u8>,
    run_record_evidence_seq: Option<u64>,
    consumed_input: bool,
    consumed_context_free_independent_input: Option<RunAuthoringCheckpoint>,
    consumed_verified_input: bool,
}

#[derive(Clone)]
struct ResolvedSessionContext {
    provider_interface: InterfaceId,
    artifacts: Vec<SessionContextArtifact>,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct SessionContextSnapshot {
    schema_version: u32,
    provider_interface: InterfaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<ArtifactRef>,
    artifacts: Vec<SessionContextArtifact>,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionContextArtifact {
    pub label: String,
    pub payload: serde_json::Value,
    pub source_artifact: ArtifactRef,
    pub source_movement_seq: Option<u64>,
}

#[derive(Clone)]
pub struct ProviderInvocationSessionSnapshot {
    pub carrier: ArtifactRef,
    pub artifacts: Arc<Vec<SessionContextArtifact>>,
}

#[derive(Clone)]
struct SessionSnapshotHead {
    artifact: ArtifactRef,
    covered_artifact_count: usize,
    covered_fingerprint: ContentHash,
}

#[derive(Clone, Default)]
struct BoundArtifactProvenance {
    movement_seq: Option<u64>,
    provider_arrival_seq: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct PendingHandoff {
    source_movement_seq: u64,
    items: Vec<PendingHandoffItem>,
    terminal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_publication: Option<Vec<ArtifactId>>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    source_without_context: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    settlement_of: Option<ArtifactRef>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    dispatch_fenced: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    withdrawal: Option<Box<source_withdrawal::Withdrawal>>,
}

mod context_publication;
mod output_connections;
mod input_revision;
pub use input_revision::{declared_owner_input_bindings, prepare_provider_input_revision, prepare_provider_input_rearm, prepare_provider_input_payloads, provider_input_rearm_ready, provider_input_payloads_ready, ProviderInputRevision};
mod authoring_retry;
mod failed_source;
pub use failed_source::{payload_carries_session_context, FailedSourceHandoffError};
mod writer_run;
pub use writer_run::{separate_writer_run, separate_owner_writer_run, owner_writer_binding, OwnerWriterEdit, OwnerWriterEvidence, WriterRunBinding};
mod source_replay;
mod source_settlement;
pub use source_settlement::PendingSourceSettlement;
mod source_withdrawal;
pub use source_withdrawal::{PendingSourceIntent, PendingSourceWithdrawal};
mod settled_definition;
pub mod capability_amendment;
pub use capability_amendment::ProviderCapabilityAmendment;

#[cfg(test)]
#[path = "../tests/support/source_replay.rs"]
mod source_replay_tests;

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct PendingHandoffItem {
    artifact_id: ArtifactId,
    artifact_content_hash: String,
    artifact_label: String,
    manifest_relative_path: PathBuf,
    checkpoint: PendingHandoffCheckpoint,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct PendingHandoffCheckpoint {
    id: CheckpointId,
    epoch: u64,
    interface_binding: AuthoredInterfaceBinding,
    outbound_interface_bindings: Vec<AuthoredInterfaceBinding>,
    artifact_bindings: Vec<ArtifactId>,
    required_verified_artifacts: Vec<RequiredVerifiedArtifact>,
}

#[derive(Clone)]
struct HandoffArtifactIdentity {
    artifact: ArtifactRef,
    label: String,
    manifest_relative_path: PathBuf,
}

#[derive(Clone, Copy)]
struct PendingHandoffLedgerCoordinate<'a> {
    interface: &'a InterfaceId,
    run: &'a RunId,
    worker: &'a InterfaceId,
    artifact: &'a ArtifactRef,
    required_evidence: Option<(&'a str, &'a str)>,
    checkpoint: &'a CheckpointId,
    epoch: u64,
}

struct PreparedHandoff {
    identity: HandoffArtifactIdentity,
    output_checkpoint: RunAuthoringCheckpoint,
    verified_inputs: Vec<RunAuthoringCheckpoint>,
}

fn split_handoff_batches_by_artifact_name(
    emissions: Vec<ProviderHandoffEmission>,
    session_snapshot_labels: &BTreeSet<String>,
) -> Result<Vec<Vec<ProviderHandoffEmission>>, String> {
    let mut batches = Vec::new();
    let mut batch = Vec::new();
    let mut labels = BTreeSet::new();
    for emission in emissions {
        let manifest = emission.artifact.read_manifest().map_err(|error| {
            format!("provider output artifact manifest cannot be read: {error}")
        })?;
        let isolated = session_snapshot_labels.contains(&manifest.label)
            || matches!(
                &emission.route,
                ProviderHandoffRoute::ClientProjectionWithVerifiedInputs { .. }
            );
        if !batch.is_empty() && (isolated || labels.contains(&manifest.label)) {
            batches.push(std::mem::take(&mut batch));
            labels.clear();
        }
        if isolated {
            batches.push(vec![emission]);
        } else {
            labels.insert(manifest.label);
            batch.push(emission);
        }
    }
    if !batch.is_empty() {
        batches.push(batch);
    }
    Ok(batches)
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct RunAuthoringConfig {
    run: RunId,
    worker: InterfaceId,
    provider_interface: InterfaceId,
    authoring_source_interface: InterfaceId,
    file_write_tool: String,
    file_write_label: String,
    file_write_result_label: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    payload_artifact_bindings: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    writer_run: Option<writer_run::WriterRunBinding>,
    checkpoints: Vec<RunAuthoringCheckpoint>,
    result_timeout_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunAuthoringProfile {
    pub file_write_tool: String,
    pub file_write_label: String,
    pub file_write_result_label: String,
    pub result_timeout_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct BoundRunAuthoringTopology {
    schema_version: u32,
    run: RunId,
    worker: InterfaceId,
    provider_interface: InterfaceId,
    authoring_source_interface: InterfaceId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    payload_artifact_bindings: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    writer_run: Option<writer_run::WriterRunBinding>,
    checkpoints: Vec<RunAuthoringCheckpoint>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct RunAuthoringCheckpoint {
    name: String,
    role: RunAuthoringCheckpointRole,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    static_artifact_bindings: Vec<ArtifactId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner_authoring: Option<CheckedRunWriter>,
    interface_binding: AuthoredInterfaceBinding,
    outbound_interface_bindings: Vec<AuthoredInterfaceBinding>,
    include_context: bool,
    required_verified_artifacts: Vec<RequiredVerifiedArtifact>,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
enum RunAuthoringCheckpointRole {
    Stable,
    IndependentInput,
    OwnerInput,
    VerifiedInput,
    ProviderArtifactOutput,
    ProviderInterfaceOutput,
    ProviderToolOutput,
    ProviderTransportDispatch,
    ProviderHandoffCompletion,
    AuthoringCall,
    AuthoringResult,
}

impl RunAuthoringCheckpointRole {
    fn is_provider_input(self) -> bool {
        matches!(
            self,
            Self::Stable | Self::IndependentInput | Self::OwnerInput | Self::VerifiedInput
        )
    }
}

impl RunAuthoringCheckpoint {
    fn interface(&self) -> &InterfaceId {
        &self.interface_binding.interface
    }

    fn outbound_interfaces(&self) -> impl Iterator<Item = &InterfaceId> {
        self.outbound_interface_bindings
            .iter()
            .map(|binding| &binding.interface)
    }

    fn binding_policy_snapshot(&self) -> CheckpointBindingPolicySnapshot {
        CheckpointBindingPolicySnapshot {
            inbound: self.interface_binding.clone(),
            outbound: self.outbound_interface_bindings.clone(),
        }
    }
}

fn metadata_matches_checkpoint(
    metadata: &InvocationMetadata,
    checkpoint: &RunAuthoringCheckpoint,
) -> bool {
    metadata.outbound_interface_bindings
        == checkpoint
            .outbound_interfaces()
            .cloned()
            .collect::<Vec<_>>()
        && metadata.checkpoint_binding_policy.as_ref()
            == Some(&checkpoint.binding_policy_snapshot())
        && metadata.required_verified_artifacts == checkpoint.required_verified_artifacts
}

fn indexed_evidence_matches_checkpoint(
    evidence: &IndexedOutboundArtifact,
    checkpoint: &RunAuthoringCheckpoint,
) -> bool {
    evidence.outbound_interface_bindings
        == checkpoint
            .outbound_interfaces()
            .cloned()
            .collect::<Vec<_>>()
        && evidence.checkpoint_binding_policy.as_ref()
            == Some(&checkpoint.binding_policy_snapshot())
        && evidence.required_verified_artifacts == checkpoint.required_verified_artifacts
}

fn indexed_source_run_record(
    ctx: &InterfaceLiftContext,
    evidence: &IndexedOutboundArtifact,
    movement_seq: u64,
) -> Result<RunRecord, String> {
    let expected = evidence
        .run_record_hash
        .as_deref()
        .and_then(ContentHash::from_hex)
        .ok_or_else(|| {
            format!(
                "provider source movement {movement_seq} carries no valid pinned run-record hash"
            )
        })?;
    pinned_run_record(
        ctx,
        &expected,
        &format!("provider source movement {movement_seq}"),
    )
    .map(|(_, record)| record)
}

fn pinned_run_record(
    ctx: &InterfaceLiftContext,
    expected: &ContentHash,
    surface: &str,
) -> Result<(Vec<u8>, RunRecord), String> {
    pinned_run_record_from_cache(&ctx.blobs_dir, ctx.required_host_io_limits()?, expected, surface)
}

fn pinned_run_record_from_cache(
    blobs_dir: &Path, limits: InterfaceHostIoLimits, expected: &ContentHash, surface: &str,
) -> Result<(Vec<u8>, RunRecord), String> {
    let path = blobs_dir.join(expected.to_hex());
    let bytes =
        octopus_interfaces::read_artifact_carrier_file_bounded(&path, limits.max_payload_bytes)
            .map_err(|error| {
                format!(
                    "{surface} pinned run record '{}' cannot be read: {error}",
                    path.display()
                )
            })?;
    if ContentHash::of_bytes(&bytes) != *expected {
        return Err(format!(
            "{surface} pinned run record does not match its hash"
        ));
    }
    let record = RunRecord::from_bytes(&bytes)
        .map_err(|error| format!("{surface} pinned run record is malformed: {error}"))?;
    Ok((bytes, record))
}

fn artifact_policies_overlap(left: &BindingArtifactPolicy, right: &BindingArtifactPolicy) -> bool {
    match (left, right) {
        (BindingArtifactPolicy::All, _) | (_, BindingArtifactPolicy::All) => true,
        (
            BindingArtifactPolicy::Allow {
                names: left_names,
                namespaces: left_namespaces,
            },
            BindingArtifactPolicy::Allow {
                names: right_names,
                namespaces: right_namespaces,
            },
        ) => {
            left_names.iter().any(|name| right.admits(name))
                || right_names.iter().any(|name| left.admits(name))
                || left_namespaces.iter().any(|left_namespace| {
                    right_namespaces.iter().any(|right_namespace| {
                        left_namespace == right_namespace
                            || left_namespace
                                .strip_prefix(right_namespace)
                                .is_some_and(|suffix| suffix.starts_with('.'))
                            || right_namespace
                                .strip_prefix(left_namespace)
                                .is_some_and(|suffix| suffix.starts_with('.'))
                    })
                })
        }
    }
}

fn output_artifact_policies_overlap(left: &RunAuthoringCheckpoint, right: &RunAuthoringCheckpoint) -> bool {
    let policies = std::iter::once(&left.interface_binding)
        .chain(&left.outbound_interface_bindings)
        .chain(std::iter::once(&right.interface_binding))
        .chain(&right.outbound_interface_bindings)
        .map(|binding| &binding.config.artifact_policy)
        .collect::<Vec<_>>();
    let candidates = policies.iter().flat_map(|policy| match policy {
        BindingArtifactPolicy::All => Vec::new(),
        BindingArtifactPolicy::Allow { names, namespaces } => names.iter().chain(namespaces).collect(),
    }).collect::<Vec<_>>();
    candidates.is_empty() || candidates.into_iter().any(|label| policies.iter().all(|policy| policy.admits(label)))
}

fn binding_admits_exact_name(binding: &AuthoredInterfaceBinding, artifact_label: &str) -> bool {
    matches!(
        &binding.config.artifact_policy,
        BindingArtifactPolicy::Allow { names, namespaces }
            if names.as_slice() == [artifact_label] && namespaces.is_empty()
    )
}

fn validate_pending_handoff_delivery_isolation(
    record: &RunRecord,
    pending: &PendingHandoff,
) -> Result<(), String> {
    for (index, left) in pending.items.iter().enumerate() {
        for right in pending.items.iter().skip(index + 1) {
            if artifact_policies_overlap(
                &left.checkpoint.interface_binding.config.artifact_policy,
                &right.checkpoint.interface_binding.config.artifact_policy,
            ) {
                return Err(format!(
                    "provider run '{}' pending handoff artifact labels '{}' and '{}' have overlapping inbound artifact policies",
                    record.id.as_str(),
                    left.artifact_label,
                    right.artifact_label
                ));
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AuthoredRun {
    json_format: Option<JsonFormat>,
    content: String,
    patch: json_patch::Patch,
    before_hash: String,
    after_hash: String,
    correlation_id: String,
    authoring_result_checkpoint: CheckpointId,
    authoring_result_interface: InterfaceId,
    pending_handoff: PendingHandoff,
}

type FileWriteResultEnvelope = ToolResultArtifact;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileWriteCallEnvelope {
    tool: String,
    arguments: FileWriteCallArguments,
    correlation_id: String,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct FileWriteCallArguments {
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "octopus_file_write_contract::optional_json_format")]
    json_format: Option<JsonFormat>,
    path: PathBuf,
    patch: json_patch::Patch,
    expected_before_hash: String,
    expected_after_hash: String,
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileWriteResultBody {
    result: String,
    writes: Vec<FileWriteEvidence>,
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileWriteEvidence {
    path: PathBuf,
    before_hash: Option<String>,
    after_hash: String,
    bytes: u64,
}

#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct InterfaceFailureEnvelope {
    source_movement_seq: u64,
    failure_class: String,
    details: serde_json::Value,
}

struct FileWriteFailureResult {
    correlation_id: String,
    failure: InterfaceFailureEnvelope,
}

enum AuthoredRunInstallOutcome {
    Installed,
    TerminalFailure(ProviderHandoffOutcome),
}

enum AuthoringWriteOutcome {
    Written,
    Rejected(authoring_retry::RejectedWrite),
    TerminalFailure {
        failure_seq: u64,
        failure: InterfaceFailureEnvelope,
    },
}

pub use octopus_app_server_interface::application_consent_contract::{
    CheckedRunWriter, CheckedRunInstall, ConsentEvidenceRef, ConsentHumanEvidence, ConsentIntake,
    ConsentPolicyAuthorization, SelectedConsentReturn, SelectedConsentBinding,
};

fn refuse_selected_consent_state(record: &RunRecord, provider: &InterfaceId) -> Result<(), String> {
    if record.interface_state.get(provider).is_some_and(|state| state.contains_key("selected_consent")) {
        return Err("selected Home consent state cannot use ordinary provider authoring".into());
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct CheckedRunTransition {
    pub writer: CheckedRunWriter,
    pub before: Vec<u8>,
    pub after: Vec<u8>,
    pub call: Movement,
    pub result: Movement,
}

#[derive(Debug)]
pub enum CheckedRunError {
    Invalid(String),
    AwaitingResults { call_sequences: Vec<u64> },
    ChangedDuringRead { captured: String, current: String },
}

impl From<String> for CheckedRunError {
    fn from(error: String) -> Self { Self::Invalid(error) }
}

impl From<&str> for CheckedRunError {
    fn from(error: &str) -> Self { Self::Invalid(error.to_string()) }
}

impl std::fmt::Display for CheckedRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(error) => formatter.write_str(error),
            Self::AwaitingResults { call_sequences } => write!(formatter, "run change awaits admitted checked file.write results for calls {call_sequences:?}"),
            Self::ChangedDuringRead { captured, current } => write!(formatter, "run changed during evidence collection from {captured} to {current}"),
        }
    }
}

impl From<CheckedRunError> for String {
    fn from(error: CheckedRunError) -> Self { error.to_string() }
}

pub mod generated_artifacts;
pub mod input_artifacts;
pub mod snapshot_projection;

pub struct CheckedRunEvidence<'a> {
    pub movements_path: &'a Path,
    pub ledger_index_path: &'a Path,
    pub blobs_dir: &'a Path,
    pub max_control_frame_bytes: u64,
    pub max_manifest_bytes: u64,
    pub max_payload_bytes: u64,
}

pub trait CheckedRunEvidenceReader {
    fn max_payload_bytes(&self) -> u64;
    fn source_context_snapshot(&mut self, _record: &RunRecord, _config: &RunAuthoringConfig, _source: u64) -> Result<(serde_json::Value, Vec<(ArtifactRef, u64)>), String> {
        Err("checked reader does not provide exact source context evidence".into())
    }
    fn pending_output_payload(&mut self, provider: &InterfaceId, reference: &ArtifactRef, relative: &Path) -> Result<Vec<u8>, String> {
        let _ = (provider, relative);
        self.artifact(reference).map(|(_, payload)| payload)
    }
    fn admitted_movement(&mut self, seq: u64) -> Result<Option<Movement>, String> { self.movement(seq) }
    fn visit_admitted_prefix(&mut self, before_seq: u64, visitor: &mut dyn FnMut(&Movement) -> Result<(), String>) -> Result<(), String> {
        for sequence in 0..before_seq {
            let row = self.admitted_movement(sequence)?.ok_or("checked evidence prefix is incomplete")?;
            if row.seq.0 != sequence { return Err("checked evidence prefix has a sequence gap".into()); }
            visitor(&row)?;
        }
        Ok(())
    }
    fn movement(&mut self, seq: u64) -> Result<Option<Movement>, String>;
    fn artifact(&mut self, reference: &ArtifactRef) -> Result<(InterfaceArtifactManifest, Vec<u8>), String>;
    fn snapshot(&mut self, hash: &ContentHash) -> Result<Vec<u8>, String>;
    fn concurrent_writer_start(&mut self, path: &Path, record: &RunRecord, before: &[u8], after_seq: u64, writers: &[CheckedRunWriter]) -> Result<u64, String> {
        let hash = ContentHash::of_bytes(before).to_hex();
        let mut first = None;
        let mut last_segment = None;
        validate_checked_run_writers(writers)?;
        for sequence in 0..=after_seq {
            let row = self.admitted_movement(sequence)?.ok_or("checked writer evidence interval is incomplete")?;
            if row.kind != MovementKind::InterfaceOutbound { continue; }
            let Some(runtime) = row.runtime().filter(|runtime| runtime.run == record.id) else { continue; };
            if runtime.run_record_hash.as_deref() == Some(hash.as_str()) {
                first.get_or_insert(sequence); last_segment = first;
            } else { first = None; }
        }
        let observed = last_segment.map_or(after_seq, |sequence| sequence.saturating_sub(1));
        for sequence in 0..=observed {
            let row = self.admitted_movement(sequence)?.ok_or("checked writer evidence interval is incomplete")?;
            if detached_writer_starts_at(self, &row, path, record, before, &hash, writers)? {
                return sequence.checked_sub(1).ok_or("observed writer call has no earlier evidence boundary".into());
            }
        }
        Ok(observed)
    }
    fn input_rearm_ready(&mut self, record: &RunRecord, provider: &InterfaceId, before_seq: u64) -> Result<bool, String> {
        input_revision::reader_readiness(self, record, provider, before_seq)
    }
    fn input_payloads_ready(&mut self, path: &Path, current: &[u8], writer: &CheckedRunWriter, source: &InterfaceId, before_seq: u64) -> Result<bool, String> {
        input_revision::reader_payload_readiness(self, path, current, writer, source, before_seq)
    }
}

pub struct FileCheckedRunReader<'a> {
    evidence: &'a CheckedRunEvidence<'a>,
    decoder: DurableMovementDecoder,
    reader: std::io::BufReader<std::fs::File>,
    next_seq: u64,
    snapshot_bytes: u64,
    provider_context: Option<InterfaceLiftContext>,
}

impl<'a> FileCheckedRunReader<'a> {
    pub fn open(evidence: &'a CheckedRunEvidence<'a>, after_seq: Option<u64>) -> Result<Self, String> {
    if [evidence.max_control_frame_bytes,evidence.max_manifest_bytes,evidence.max_payload_bytes].contains(&0)
        || [&evidence.movements_path,&evidence.ledger_index_path,&evidence.blobs_dir].iter().any(|path| !path.is_absolute()) {
        return Err("checked authoring requires explicit bounded owner evidence paths".into());
    }
        let decoder = DurableMovementDecoder::for_log(&evidence.movements_path);
        let file = std::fs::File::open(&evidence.movements_path).map_err(|e| e.to_string())?;
    let mut reader = std::io::BufReader::new(file);
    let mut sequence = 0u64;
    if let (Some(after_seq), Ok(index)) = (after_seq, LedgerSqliteReader::open(&evidence.ledger_index_path)) {
        if let Ok(Some(range)) = index.movement_row_range(after_seq) {
            let length = reader.get_ref().metadata().map_err(|e| e.to_string())?.len();
            if range.end_offset <= length && range.start_offset < range.end_offset && range.end_offset - range.start_offset <= evidence.max_control_frame_bytes {
                reader.seek(std::io::SeekFrom::Start(range.start_offset)).map_err(|e| e.to_string())?;
                let valid = match read_bounded_ledger_row(&mut reader,evidence.max_control_frame_bytes)? {
                    BoundedLedgerRow::Complete(row) => decoder.decode(&row,after_seq as usize).is_ok_and(|movement| movement.seq.0 == after_seq),
                    _ => false,
                };
                if valid && reader.stream_position().map_err(|e| e.to_string())? == range.end_offset {
                    sequence = after_seq.checked_add(1).ok_or("checked authoring sequence overflow")?;
                } else {
                    reader.seek(std::io::SeekFrom::Start(0)).map_err(|e| e.to_string())?;
                }
            }
        }
    }

        Ok(Self { evidence, decoder, reader, next_seq: sequence, snapshot_bytes: 0, provider_context: None })
    }

    fn with_provider_context(mut self, ctx: &InterfaceLiftContext) -> Self { self.provider_context = Some(ctx.clone()); self }

    pub fn next_sequence(&self) -> u64 { self.next_seq }
}

impl CheckedRunEvidenceReader for FileCheckedRunReader<'_> {
    fn max_payload_bytes(&self) -> u64 { self.evidence.max_payload_bytes }
    fn source_context_snapshot(&mut self, record: &RunRecord, config: &RunAuthoringConfig, source: u64) -> Result<(serde_json::Value, Vec<(ArtifactRef, u64)>), String> {
        source_withdrawal::source_snapshot_from_evidence(self.evidence, self.provider_context.as_ref(), record, config, source)
    }
    fn pending_output_payload(&mut self, provider: &InterfaceId, reference: &ArtifactRef, relative: &Path) -> Result<Vec<u8>, String> {
        if let Some(ctx) = self.provider_context.as_ref().filter(|ctx| ctx.interface == *provider) {
            if relative.as_os_str().is_empty() || relative.is_absolute() || relative.components().any(|component| !matches!(component, std::path::Component::Normal(_))) {
                return Err("withdrawal closure has an unsafe private artifact path".into());
            }
            let file = InterfaceArtifactFile::new(ctx.interface_var_dir().join(relative)).map_err(|error| error.to_string())?;
            if artifact_identity(&file, &ctx.interface_var_dir())?.artifact != *reference { return Err("withdrawal closure differs from its exact private artifact".into()); }
            return file.read_payload_bytes().map_err(|error| error.to_string());
        }
        self.artifact(reference).map(|(_, payload)| payload)
    }
    fn concurrent_writer_start(&mut self, path: &Path, record: &RunRecord, before: &[u8], after_seq: u64, writers: &[CheckedRunWriter]) -> Result<u64, String> {
        concurrent_writer_evidence_start(self.evidence, path, record, before, after_seq, writers)
    }
    fn input_rearm_ready(&mut self, record: &RunRecord, provider: &InterfaceId, before_seq: u64) -> Result<bool, String> {
        input_revision::readiness(self.evidence, record, provider, before_seq)
    }
    fn input_payloads_ready(&mut self, path: &Path, current: &[u8], writer: &CheckedRunWriter, source: &InterfaceId, before_seq: u64) -> Result<bool, String> {
        input_revision::payload_readiness(self.evidence, path, current, writer, source, before_seq)
    }
    fn admitted_movement(&mut self, seq: u64) -> Result<Option<Movement>, String> {
        generated_artifacts::read_movement(self.evidence,seq).map(Some)
    }
    fn visit_admitted_prefix(&mut self, before_seq: u64, visitor: &mut dyn FnMut(&Movement) -> Result<(), String>) -> Result<(), String> {
        let mut prefix = FileCheckedRunReader::open(self.evidence, None)?;
        for sequence in 0..before_seq {
            let row = prefix.movement(sequence)?.ok_or("checked evidence prefix is incomplete")?;
            visitor(&row)?;
        }
        Ok(())
    }
    fn movement(&mut self, seq: u64) -> Result<Option<Movement>, String> {
        if seq != self.next_seq { return Err("checked authoring reader lost its exact cursor".into()); }
        let row = match read_bounded_ledger_row(&mut self.reader, self.evidence.max_control_frame_bytes)? {
            BoundedLedgerRow::Complete(row) => row,
            BoundedLedgerRow::Partial | BoundedLedgerRow::Eof => return Ok(None),
        };
        let movement = self.decoder.decode(&row, seq as usize).map_err(|e| e.to_string())?;
        if movement.seq.0 != seq { return Err("checked authoring evidence ledger has a sequence gap".into()); }
        self.next_seq = seq.checked_add(1).ok_or("checked authoring sequence overflow")?;
        Ok(Some(movement))
    }
    fn artifact(&mut self, reference: &ArtifactRef) -> Result<(InterfaceArtifactManifest, Vec<u8>), String> {
        read_pinned_artifact_with_bounds(self.evidence.blobs_dir, self.evidence.max_manifest_bytes,
            self.evidence.max_payload_bytes, reference)
    }
    fn snapshot(&mut self, hash: &ContentHash) -> Result<Vec<u8>, String> {
        let snapshot = octopus_interfaces::read_artifact_carrier_file_bounded(
            &self.evidence.blobs_dir.join(hash.to_hex()), self.evidence.max_payload_bytes).map_err(|e| e.to_string())?;
        self.snapshot_bytes = self.snapshot_bytes.checked_add(snapshot.len() as u64).ok_or("checked authoring evidence byte overflow")?;
        let limit = self.evidence.max_payload_bytes.checked_mul(4).ok_or("checked authoring byte budget overflow")?;
        if self.snapshot_bytes > limit { return Err("checked authoring chain exceeds its bounded snapshot budget".into()); }
        Ok(snapshot)
    }
}

pub fn checked_run_file_chain(
    evidence: &CheckedRunEvidence<'_>, path: &Path, before: &[u8], after: &[u8],
    after_seq: u64, writers: &[CheckedRunWriter],
) -> Result<Vec<CheckedRunTransition>, String> {
    checked_run_file_chain_state(evidence, path, before, after, after_seq, writers).map_err(Into::into)
}

fn checked_run_file_chain_state(
    evidence: &CheckedRunEvidence<'_>, path: &Path, before: &[u8], after: &[u8],
    after_seq: u64, writers: &[CheckedRunWriter],
) -> Result<Vec<CheckedRunTransition>, CheckedRunError> {
    if ContentHash::of_bytes(before) == ContentHash::of_bytes(after) { return Ok(Vec::new()); }
    let mut reader = FileCheckedRunReader::open(evidence, Some(after_seq))?;
    let first = reader.next_seq;
    let transitions = checked_run_file_chain_with_reader(&mut reader, path, before, CheckedRunTarget::Snapshot(after), after_seq, first, None, writers, &[], None, None)?;
    Ok(transitions)
}

pub fn checked_run_file_chain_before(
    reader: &mut impl CheckedRunEvidenceReader, path: &Path, before: &[u8], after: &[u8],
    after_seq: u64, before_seq: u64, writers: &[CheckedRunWriter],
) -> Result<Vec<CheckedRunTransition>, String> {
    let first = after_seq.checked_add(1).ok_or("checked historical sequence overflow")?;
    if first > before_seq { return Err("checked historical interval is reversed".into()); }
    checked_run_file_chain_with_reader(reader, path, before, CheckedRunTarget::Snapshot(after), after_seq, first, Some(before_seq), writers, &[], None, None).map_err(Into::into)
}

fn validate_checked_run_writers(writers: &[CheckedRunWriter]) -> Result<(), String> {
    let mut unique = BTreeSet::new();
    let mut returns = BTreeSet::new();
    for writer in writers {
        if writer.owner == writer.door || !unique.insert((&writer.source, &writer.door)) || !returns.insert((&writer.door,&writer.owner)) {
            return Err("checked authoring chain has ambiguous writer identities".into());
        }
        for value in [&writer.tool, &writer.call_label, &writer.result_label] { non_empty_string(value, "checked run writer")?; }
    }
    Ok(())
}

enum CheckedRunTarget<'a> {
    Snapshot(&'a [u8]),
    Observed { hash: ContentHash, sequence: u64 },
}

fn checked_run_file_chain_with_reader(
    reader: &mut impl CheckedRunEvidenceReader, path: &Path, before: &[u8], target: CheckedRunTarget<'_>,
    after_seq: u64, first_seq: u64, before_seq: Option<u64>, writers: &[CheckedRunWriter],
    selected_reads: &[ArtifactRef],
    connection_config: Option<&RunAuthoringConfig>,
    selected_install: Option<&CheckedRunInstall>,
) -> Result<Vec<CheckedRunTransition>, CheckedRunError> {
    if connection_config.is_some() && (before_seq.is_some() || !selected_reads.is_empty() || matches!(target, CheckedRunTarget::Observed { .. })) {
        return Err("observed connections cannot bridge historical or selected-read evidence".into());
    }
    let start = ContentHash::of_bytes(before).to_hex();
    let (end, after, observed_sequence) = match target {
        CheckedRunTarget::Snapshot(after) => (ContentHash::of_bytes(after).to_hex(), Some(after), None),
        CheckedRunTarget::Observed { hash, sequence } => (hash.to_hex(), None, Some(sequence)),
    };
    let original = RunRecord::from_bytes(before).map_err(|e| e.to_string())?;
    if let Some(after) = after {
        let target = RunRecord::from_bytes(after).map_err(|e| e.to_string())?;
        if original.id != target.id || original.ee != target.ee || original.worker != target.worker {
            return Err("checked authoring chain changed run, EE or worker".into());
        }
    }
    validate_checked_run_writers(writers)?;
    struct Candidate {
        writer: CheckedRunWriter,
        call: Movement,
        arguments: FileWriteCallArguments,
        ordered: Option<writer_run::OrderedArguments>,
        before: Vec<u8>,
    }
    let mut calls: BTreeMap<(InterfaceId, String), Candidate> = BTreeMap::new();
    let mut transitions = Vec::new();
    let mut observations = Vec::new();
    let mut sequence = first_seq;
    loop {
        if before_seq.is_some_and(|end| sequence >= end) { break; }
        let Some(movement) = reader.movement(sequence)? else {
            if before_seq.is_some() { return Err("checked historical interval has no complete durable prefix".into()); }
            break;
        };
        if movement.seq.0 != sequence { return Err("checked authoring evidence ledger has a sequence gap".into()); }
        sequence = sequence.checked_add(1).ok_or("checked authoring evidence sequence overflow")?;
        if movement.seq.0 <= after_seq || movement.kind != MovementKind::InterfaceOutbound { continue; }
        if selected_install.is_some_and(|install| movement.seq.0 != install.call.movement_seq && movement.seq.0 != install.result.movement_seq) { continue; }
        if before_seq.is_some() && selected_install.is_none() {
            let metadata = movement.invocation_metadata.as_ref().and_then(|m| m.invocation())
                .ok_or("checked historical outbound has no actual admission metadata")?;
            let runtime = metadata.runtime.as_ref().ok_or("checked historical outbound has no run coordinate")?;
            if runtime.run == original.id {
                if runtime.ee != original.ee || runtime.worker != original.worker || metadata.verification_failure.is_some() {
                    return Err("checked historical interval changed run scope or admitted failed evidence".into());
                }
                let hash = runtime.run_record_hash.as_deref().and_then(ContentHash::from_hex)
                    .ok_or("checked historical outbound has no pinned snapshot")?;
                observations.push((movement.seq.0, hash, (!selected_reads.is_empty()).then(|| metadata.clone())));
            }
        }
        let source_writer = writers.iter().find(|w| movement.from == Endpoint::Interface(w.source.clone()) && movement.to == Endpoint::Interface(w.door.clone()));
        let result_writer = writers.iter().find(|w| movement.from == Endpoint::Interface(w.door.clone()) && movement.to == Endpoint::Interface(w.owner.clone()));
        if source_writer.is_none() && result_writer.is_none() { continue; }
        if source_writer.is_some() && observed_sequence.is_some_and(|sequence| movement.seq.0 >= sequence) { continue; }
        let metadata = movement.invocation_metadata.as_ref().and_then(|m| m.invocation()).ok_or("checked authoring evidence has no invocation metadata")?;
        let runtime = metadata.runtime.as_ref().ok_or("checked authoring evidence has no runtime coordinate")?;
        if runtime.ee != original.ee || runtime.worker != original.worker || metadata.verification_failure.is_some() {
            return Err("checked authoring evidence has another scope or a verification failure".into());
        }
        let artifact = movement.artifact.as_ref().ok_or("checked authoring movement has no artifact")?;
        let (manifest, payload) = reader.artifact(artifact)?;
        let snapshot_hash = runtime.run_record_hash.as_deref().and_then(ContentHash::from_hex).ok_or("checked authoring evidence has no pinned snapshot")?;
        let snapshot = reader.snapshot(&snapshot_hash)?;
        if ContentHash::of_bytes(&snapshot) != snapshot_hash { return Err("checked authoring snapshot failed integrity".into()); }
        let snapshot_record = RunRecord::from_bytes(&snapshot).map_err(|e| e.to_string())?;
        if snapshot_record.id != runtime.run || snapshot_record.ee != original.ee || snapshot_record.worker != original.worker {
            return Err("checked authoring snapshot differs from its movement scope".into());
        }
        if let Some(writer) = source_writer {
            if manifest.label != writer.call_label { continue; }
            let call: octopus_tool_handlers::ToolCallArtifact = serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
            if call.tool != writer.tool { continue; }
            if call.label.as_deref().is_some_and(|label| label != writer.call_label) { return Err("checked run call label differs from its admitted manifest".into()); }
            let (arguments, ordered, before) = if call.arguments.get("ordered").is_some() {
                let ordered: writer_run::OrderedArguments = serde_json::from_value(call.arguments).map_err(|e|e.to_string())?;
                if !ordered.targets(path) { continue; }
                let (before, arguments) = ordered.checked_call(writer,&snapshot,path,reader.max_payload_bytes())?;
                (arguments,Some(ordered),before)
            } else {
                if runtime.run != original.id { continue; }
                let arguments: FileWriteCallArguments = serde_json::from_value(call.arguments).map_err(|e|e.to_string())?;
                if arguments.path != path { continue; }
                (arguments,None,snapshot)
            };
            if runtime.interface != writer.door
                || !metadata.grant.tools.contains(&writer.tool) || metadata.grant.hard_blocked_tools.contains(&writer.tool)
                || !metadata.grant.interfaces.contains(&writer.door.to_string()) {
                return Err("checked run write was not admitted under its exact writer and before snapshot".into());
            }
            let correlation = call.correlation_id.ok_or("checked run write has no immutable correlation")?;
            if calls.len() >= 1024 || calls.insert((writer.owner.clone(), correlation), Candidate { writer: writer.clone(), call: movement, arguments, ordered, before }).is_some() {
                return Err("checked authoring calls exceed their bound or repeat a correlation".into());
            }
        } else if let Some(writer) = result_writer {
            if manifest.label != writer.result_label { continue; }
            let result: ToolResultArtifact = serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
            if result.tool != writer.tool || result.label != writer.result_label { continue; }
            let correlation = result.correlation_id.clone().ok_or("checked run result has no immutable correlation")?;
            let Some(candidate) = calls.remove(&(writer.owner.clone(), correlation)) else { continue; };
            if candidate.writer != *writer { return Err("checked run result came from a different declared writer".into()); }
            if let Some(ordered) = &candidate.ordered {
                if !ordered.checked_result(writer,metadata,&snapshot,&result,reader.max_payload_bytes())? { continue; }
                let prospective = apply_run_json_patch(&candidate.before,&candidate.arguments.patch,&candidate.arguments.expected_after_hash, candidate.arguments.json_format)?.into_bytes();
                let transition = CheckedRunTransition {writer:candidate.writer,before:candidate.before,after:prospective,call:candidate.call,result:movement};
                generated_artifacts::validate_generated_transition(reader,&transition)?;
                input_artifacts::validate_input_transition(reader, &transition)?;
                validate_verified_input_transition(reader,&transition)?;
                input_revision::validate_rearm_evidence(reader, &transition, path)?;
                source_withdrawal::validate_evidence(reader, &transition)?;
                if transitions.len() >= 1024 { return Err("checked authoring transitions exceed their evidence bound".into()); }
                transitions.push(transition);
                continue;
            }
            if runtime.run != original.id { return Err("same-run authoring result changed its execution scope".into()); }
            if !result.ok || result.error.is_some() { continue; }
            if candidate.arguments.expected_before_hash != ContentHash::of_bytes(&candidate.before).to_hex() {
                return Err("successful checked run write was not admitted under its exact before snapshot".into());
            }
            let body: FileWriteResultBody = serde_json::from_value(result.body.ok_or("checked run result has no body")?).map_err(|e| e.to_string())?;
            let [evidence] = body.writes.as_slice() else { return Err("checked run result must prove one exact file".into()); };
            let prospective = apply_run_json_patch(&candidate.before, &candidate.arguments.patch,
                &candidate.arguments.expected_after_hash, candidate.arguments.json_format)?.into_bytes();
            validate_unchanged_admitted_checkpoint(&prospective, &snapshot, &runtime.checkpoint)?;
            let checkpoint = snapshot_record.checkpoint(&runtime.checkpoint).ok_or("checked run result lost its admitted checkpoint")?;
            let binding = CheckpointBindingPolicySnapshot { inbound: checkpoint.interface_binding.clone().ok_or("checked run result has no inbound binding")?, outbound: checkpoint.outbound_interface_bindings.clone() };
            if body.result != "files_written" || evidence.path != path || evidence.before_hash.as_deref() != Some(candidate.arguments.expected_before_hash.as_str())
                || evidence.after_hash != candidate.arguments.expected_after_hash || evidence.bytes != prospective.len() as u64
                || runtime.interface != writer.owner || runtime.epoch != checkpoint.epoch.0
                || binding.inbound.interface != writer.door || binding.outbound.len() != 1 || binding.outbound[0].interface != writer.owner
                || metadata.checkpoint_binding_policy.as_ref() != Some(&binding) || metadata.outbound_interface_bindings != [writer.owner.clone()]
                || !metadata.required_verified_artifacts.is_empty() || !checkpoint.required_verified_artifacts.is_empty()
                || !metadata.checkpoint_admission.as_ref().is_some_and(|a| a.complete && a.artifact_label == manifest.label && a.required_artifact_labels.is_empty())
                || !snapshot_record.active_checkpoints.contains(&checkpoint.id) || candidate.call.seq >= movement.seq {
                return Err("checked run result differs from its exact admitted write and prospective snapshot".into());
            }
            let transition = CheckedRunTransition { writer: candidate.writer, before: candidate.before, after: prospective, call: candidate.call, result: movement };
            if transitions.len() >= 1024 { return Err("checked authoring transitions exceed their evidence bound".into()); }
            generated_artifacts::validate_generated_transition(reader,&transition)?;
            input_artifacts::validate_input_transition(reader, &transition)?;
            validate_verified_input_transition(reader, &transition)?;
            input_revision::validate_rearm_evidence(reader, &transition, path)?;
                source_withdrawal::validate_evidence(reader, &transition)?;
            transitions.push(transition);
        }
    }

    if before_seq.is_some() && !calls.is_empty() {
        return Err("checked historical interval ends during an unjoined authoring call".into());
    }
    let mut by_before = BTreeMap::new();
    for transition in transitions {
        if by_before.insert(ContentHash::of_bytes(&transition.before).to_hex(), transition).is_some() {
            return Err("checked authoring transitions branch or exceed their evidence bound".into());
        }
    }
    let mut hash = start.clone();
    let mut current = before.to_vec();
    let mut chain = Vec::new();
    let mut previous_call = after_seq;
    let mut connection_versions = BTreeMap::new();
    while hash != end {
        let Some(transition) = by_before.remove(&hash) else {
            if let Some(config) = connection_config {
                // These are validated endpoint bytes, never synthetic writer
                // receipts. Only routing may differ between adjacent segments.
                let mut successors = BTreeMap::new();
                for bytes in by_before.values().filter(|next| next.call.seq.0 > previous_call).map(|next| next.before.as_slice())
                    .chain(calls.values().filter(|next| next.call.seq.0 > previous_call).map(|next| next.before.as_slice()))
                    .chain(after.into_iter()) {
                    if output_connections::protected_record(&current, bytes, config)?
                        .is_some_and(|protected| protected == current) {
                        successors.insert(ContentHash::of_bytes(bytes).to_hex(), bytes);
                    }
                }
                if successors.len() > 1 {
                    return Err("observed outgoing connections have ambiguous checked successors".into());
                }
                if let Some((next, bytes)) = successors.into_iter().next() {
                    if connection_versions.insert(next.clone(), chain.len()).is_some() {
                        return Err("observed outgoing connections cycle through a snapshot".into());
                    }
                    hash = next;
                    current = bytes.to_vec();
                    continue;
                }
            }
            let mut reachable = BTreeSet::from([hash.clone()]);
            let mut pending_sequences = BTreeSet::new();
            loop {
                let count = reachable.len();
                for candidate in calls.values() {
                    if reachable.contains(&candidate.arguments.expected_before_hash)
                        && candidate.arguments.expected_before_hash == ContentHash::of_bytes(&candidate.before).to_hex()
                        && candidate.call.seq.0 > previous_call
                    {
                        apply_run_json_patch(&candidate.before, &candidate.arguments.patch, &candidate.arguments.expected_after_hash, candidate.arguments.json_format)?;
                        reachable.insert(candidate.arguments.expected_after_hash.clone());
                        pending_sequences.insert(candidate.call.seq.0);
                    }
                }
                for (before, known) in &by_before {
                    if reachable.contains(before) && known.call.seq.0 > previous_call {
                        reachable.insert(ContentHash::of_bytes(&known.after).to_hex());
                    }
                }
                if reachable.contains(&end) && !pending_sequences.is_empty() {
                    return Err(CheckedRunError::AwaitingResults { call_sequences: pending_sequences.into_iter().collect() });
                }
                if reachable.len() == count { break; }
            }
            return Err("run changed without a complete admitted checked file.write chain".into());
        };
        if transition.call.seq.0 <= previous_call {
            return Err("checked authoring chain has reversed effect causality".into());
        }
        hash = ContentHash::of_bytes(&transition.after).to_hex();
        current = transition.after.clone();
        previous_call = transition.call.seq.0;
        chain.push(transition);
    }
    if observed_sequence.is_some_and(|sequence| by_before.get(&end).is_some_and(|next| next.result.seq.0 <= sequence)) {
        return Err("observed run snapshot was already withdrawn by a checked effect".into());
    }
    // One receipt proves its exact file effect. Later coauthor state requires
    // the separate full-chain proof, not this installation's result-time snapshot.
    if selected_install.is_some() { return Ok(chain); }
    let mut versions = BTreeMap::from([(start, 0usize)]);
    for (index, transition) in chain.iter().enumerate() {
        if versions.insert(ContentHash::of_bytes(&transition.after).to_hex(), index + 1).is_some() {
            return Err("checked authoring chain cycles through a withdrawn snapshot".into());
        }
    }
    for (hash, position) in connection_versions {
        if versions.insert(hash, position).is_some() {
            return Err("observed connection reuses a checked snapshot".into());
        }
    }
    for (index, transition) in chain.iter().enumerate() {
        if transition.result.invocation_metadata.as_ref().and_then(|m|m.invocation()).and_then(|m|m.runtime.as_ref()).is_some_and(|r|r.run != original.id) { continue; }
        let hash = transition.result.invocation_metadata.as_ref().and_then(|m| m.invocation())
            .and_then(|m| m.runtime.as_ref()).and_then(|r| r.run_record_hash.as_ref())
            .ok_or("checked result has no snapshot")?;
        let position = *versions.get(hash).ok_or("checked result used an unproven snapshot")?;
        if position <= index || chain[position - 1].call.seq >= transition.result.seq {
            return Err("checked result predates its proven file effect".into());
        }
    }
    let mut observed_position = 0;
    for (seq, hash, metadata) in observations {
        let position = *versions.get(&hash.to_hex()).ok_or("checked historical movement used an unproven or withdrawn snapshot")?;
        if position < observed_position
            || (position > 0 && chain[position - 1].call.seq.0 >= seq)
            || chain.iter().skip(position).any(|transition| transition.result.seq.0 <= seq) {
            return Err("checked historical movement used an unproven or withdrawn snapshot".into());
        }
        observed_position = position;
        if let Some(metadata) = metadata {
            let record = if position == 0 { original.clone() } else {
                RunRecord::from_bytes(&chain[position - 1].after).map_err(|e| e.to_string())?
            };
            validate_observed_selected_reads(&record, &metadata, selected_reads)?;
        }
    }
    Ok(chain)
}

pub fn checked_run_install(
    reader: &mut impl CheckedRunEvidenceReader,
    path: &Path,
    install: &octopus_app_server_interface::application_consent_contract::CheckedRunInstall,
    writers: &[CheckedRunWriter],
) -> Result<CheckedRunTransition, String> {
    validate_checked_run_writers(writers)?;
    if install.call.movement_seq == 0 || install.call.movement_seq >= install.result.movement_seq {
        return Err("checked installation needs its actual ordered non-genesis call and result".into());
    }
    let call = reader.admitted_movement(install.call.movement_seq)?.ok_or("checked installation call is absent")?;
    let result = reader.admitted_movement(install.result.movement_seq)?.ok_or("checked installation result is absent")?;
    if call.artifact.as_ref() != Some(&install.call.artifact) || result.artifact.as_ref() != Some(&install.result.artifact) {
        return Err("checked installation changed its exact admitted evidence refs".into());
    }
    let snapshot_hash = |movement: &Movement| -> Result<ContentHash, String> {
        movement.invocation_metadata.as_ref().and_then(|m| m.invocation()).and_then(|m| m.runtime.as_ref())
            .and_then(|runtime| runtime.run_record_hash.as_deref()).and_then(ContentHash::from_hex)
            .ok_or("checked installation has no admitted snapshot".into())
    };
    let admitted = reader.snapshot(&snapshot_hash(&call)?)?;
    let (manifest, payload) = reader.artifact(&install.call.artifact)?;
    let writer = writers.iter().find(|writer| call.from == Endpoint::Interface(writer.source.clone())
        && call.to == Endpoint::Interface(writer.door.clone()))
        .ok_or("checked installation call has no exact declared writer")?;
    let artifact: octopus_tool_handlers::ToolCallArtifact = serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
    if manifest.label != writer.call_label || artifact.tool != writer.tool {
        return Err("checked installation call differs from its declared writer".into());
    }
    let (before, after) = if artifact.arguments.get("ordered").is_some() {
        let ordered: writer_run::OrderedArguments = serde_json::from_value(artifact.arguments).map_err(|e| e.to_string())?;
        let (before, arguments) = ordered.checked_call(writer, &admitted, path,reader.max_payload_bytes())?;
        let after = apply_run_json_patch(&before, &arguments.patch, &arguments.expected_after_hash, arguments.json_format)?.into_bytes();
        (before, after)
    } else {
        let arguments: FileWriteCallArguments = serde_json::from_value(artifact.arguments).map_err(|e| e.to_string())?;
        if arguments.path != path { return Err("checked installation targets another path".into()); }
        let after = apply_run_json_patch(&admitted, &arguments.patch, &arguments.expected_after_hash, arguments.json_format)?.into_bytes();
        (admitted, after)
    };
    let chain = checked_run_file_chain_with_reader(reader, path, &before, CheckedRunTarget::Snapshot(&after), install.call.movement_seq - 1,
        install.call.movement_seq, Some(install.result.movement_seq.checked_add(1).ok_or("checked installation sequence overflow")?),
        writers, &[], None, Some(install)).map_err(String::from)?;
    let [transition] = chain.as_slice() else { return Err("checked installation does not prove one exact write".into()); };
    if transition.call != call || transition.result != result {
        return Err("checked installation differs from its exact admitted write".into());
    }
    Ok(transition.clone())
}

pub fn checked_declared_run_install(
    evidence: &CheckedRunEvidence<'_>, path: &Path, declaration: &[u8],
    install: &CheckedRunInstall, seeds: Vec<CheckedRunWriter>,
) -> Result<CheckedRunTransition, String> {
    let record = RunRecord::from_bytes(declaration).map_err(|e| e.to_string())?;
    let writers = declared_writer_closure(&record, seeds)?;
    let prior = install.call.movement_seq.checked_sub(1).ok_or("checked installation call is genesis")?;
    let mut reader = FileCheckedRunReader::open(evidence, Some(prior))?;
    while reader.next_seq < install.call.movement_seq {
        reader.movement(reader.next_seq)?.ok_or("checked installation call is absent")?;
    }
    let transition = checked_run_install(&mut reader, path, install, &writers)?;
    let before = RunRecord::from_bytes(&transition.before).map_err(|e| e.to_string())?;
    if before.id != record.id || before.ee != record.ee || before.worker != record.worker {
        return Err("checked installation changed its declared target scope".into());
    }
    validate_checked_owner_transition(&transition)?;
    Ok(transition)
}

pub fn validate_selected_return_writers(
    record: &RunRecord,
    provider: &CheckedRunWriter,
    owner: Option<&CheckedRunWriter>,
) -> Result<(), String> {
    let state = record.interface_state.get(&provider.owner).ok_or("selected return lost its admitted provider state")?;
    let topology: BoundRunAuthoringTopology = serde_json::from_value(state.get(RUN_AUTHORING_BINDING)
        .cloned().ok_or("selected return lost its admitted provider topology")?).map_err(|e| e.to_string())?;
    topology.validate_identity(record, &provider.owner)?;
    let call = topology.checkpoints.iter().find(|cp| cp.role == RunAuthoringCheckpointRole::AuthoringCall)
        .ok_or("selected return has no declared provider write edge")?;
    let result = topology.checkpoints.iter().find(|cp| cp.role == RunAuthoringCheckpointRole::AuthoringResult)
        .ok_or("selected return has no declared provider write result")?;
    if provider.source != topology.authoring_source_interface || call.interface() != &provider.source
        || call.outbound_interfaces().collect::<Vec<_>>() != [&provider.door]
        || result.interface() != &provider.door || result.outbound_interfaces().collect::<Vec<_>>() != [&provider.owner]
        || !call.interface_binding.admits(&provider.call_label) || !result.interface_binding.admits(&provider.result_label)
        || !call.required_verified_artifacts.iter().any(|requirement| requirement.artifact_label == provider.call_label
            && requirement.verifier.as_str() == "tool_call_allowed"
            && requirement.params == serde_json::json!({"tools":[provider.tool]})) {
        return Err("selected return writer is not the exact admitted provider authoring contract".into());
    }
    let declared = topology.checkpoints.iter().filter_map(|cp| cp.owner_authoring.as_ref()).collect::<Vec<_>>();
    match owner {
        Some(owner) if owner.owner != provider.owner && declared.contains(&owner) => Ok(()),
        None => Ok(()),
        _ => Err("selected return owner is not an independently declared writer".into()),
    }
}

pub fn checked_return_interval(
    reader: &mut impl CheckedRunEvidenceReader,
    selected: &octopus_app_server_interface::application_consent_contract::SelectedConsentReturn,
    before: &[u8], after: &[u8], after_seq: u64, before_seq: u64,
    protected: &[CheckpointId],
) -> Result<Vec<CheckedRunTransition>, String> {
    let record = RunRecord::from_bytes(before).map_err(|e| e.to_string())?;
    if !octopus_app_server_interface::application_consent_contract::normal_path(&selected.run_path)
        || selected.run_path.file_name().and_then(|name| name.to_str()) != Some(&format!("{}.json", record.id)) {
        return Err("selected return changed its original exact task write path".into());
    }
    validate_selected_return_writers(&record, &selected.provider_writer, selected.owner_writer.as_ref())?;
    validate_payload_artifact_selection(&selected.selected_artifacts)?;
    let mut writers = vec![selected.provider_writer.clone()];
    writers.extend(selected.owner_writer.clone());
    let first = after_seq.checked_add(1).ok_or("selected return interval overflow")?;
    if first > before_seq { return Err("selected return interval is reversed".into()); }
    let chain = checked_run_file_chain_with_reader(reader, &selected.run_path, before, CheckedRunTarget::Snapshot(after), after_seq, first, Some(before_seq), &writers, &selected.selected_artifacts, None, None)?;
    for transition in &chain {
        let prior = RunRecord::from_bytes(&transition.before).map_err(|e| e.to_string())?;
        let next = RunRecord::from_bytes(&transition.after).map_err(|e| e.to_string())?;
        for record in [&prior, &next] {
            validate_selected_return_writers(record, &selected.provider_writer, selected.owner_writer.as_ref())?;
        }
        if transition.writer == selected.provider_writer { validate_checked_provider_transition(transition)?; }
        else if Some(&transition.writer) == selected.owner_writer.as_ref() { validate_checked_owner_transition(transition)?; }
        else { return Err("selected return changed the actual checked writer".into()); }
        for checkpoint in protected {
            validate_unchanged_admitted_checkpoint(&transition.before, &transition.after, checkpoint)?;
        }
        for (movement, record) in [(&transition.call, &prior), (&transition.result, &next)] {
            let metadata = movement.invocation_metadata.as_ref().and_then(|m| m.invocation())
                .ok_or("selected return write lost its admitted read bindings")?;
            validate_observed_selected_reads(record, metadata, &selected.selected_artifacts)?;
        }
    }
    for checkpoint in protected { validate_unchanged_admitted_checkpoint(before, after, checkpoint)?; }
    Ok(chain)
}

pub fn validate_selected_return_reads(metadata: &InvocationMetadata, selected: &[ArtifactRef]) -> Result<(), String> {
    for reference in selected {
        if !metadata.artifact_binding_refs.iter().filter(|bound| bound.id == reference.id).eq([reference])
            || metadata.grant.artifact_access.get(reference.id.as_str()).is_none_or(|access| !access.read) {
            return Err("selected return lost or replaced an original exact readable artifact".into());
        }
    }
    Ok(())
}

fn validate_observed_selected_reads(record: &RunRecord, metadata: &InvocationMetadata,
    selected: &[ArtifactRef]) -> Result<(), String> {
    let runtime = metadata.runtime.as_ref().ok_or("selected read witness has no admitted runtime scope")?;
    let checkpoint = record.checkpoint(&runtime.checkpoint).ok_or("selected read witness lost its actual authored checkpoint")?;
    if checkpoint.epoch.0 != runtime.epoch || !record.active_checkpoints.contains(&checkpoint.id) {
        return Err("selected read witness changed its actual authored checkpoint generation".into());
    }
    for reference in selected {
        let authored = checkpoint.artifact_bindings.iter().any(|binding| binding.id == reference.id && binding.access.read);
        let carried = metadata.artifact_binding_refs.iter().any(|bound| bound.id == reference.id);
        if authored || carried {
            validate_selected_return_reads(metadata, std::slice::from_ref(reference))?;
        }
    }
    Ok(())
}

fn observed_run_evidence_start(
    evidence: &CheckedRunEvidence<'_>, record: &RunRecord, before: &[u8], after_seq: u64,
) -> Result<u64, String> {
    let hash = ContentHash::of_bytes(before).to_hex();
    let upper = after_seq.checked_add(1).ok_or("checked writer evidence interval overflow")?;
    let snapshot = |row: &Movement| -> Option<String> {
        if row.kind != MovementKind::InterfaceOutbound { return None; }
        let runtime = row.invocation_metadata.as_ref()?.invocation()?.runtime.as_ref()?;
        (runtime.run == record.id).then(|| runtime.run_record_hash.clone()).flatten()
    };
    if let Ok(index) = LedgerSqliteReader::open(evidence.ledger_index_path) {
        if index.progress().is_ok_and(|progress| progress.watermark >= upper) {
            if let Ok(Some(rows)) = index.movement_rows_touching_run(record.id.as_str(), 0, upper) {
                let mut earliest = None;
                for indexed in rows.iter().rev() {
                    let row = generated_artifacts::read_movement(evidence, indexed.seq)?;
                    match snapshot(&row) {
                        Some(current) if current == hash => earliest = Some(row.seq.0),
                        Some(_) if earliest.is_some() => break,
                        _ => {},
                    }
                }
                return Ok(earliest.map_or(after_seq, |seq| seq.saturating_sub(1)));
            }
        }
    }
    let mut reader = FileCheckedRunReader::open(evidence, None)?;
    let mut first = None;
    let mut last_segment = None;
    while reader.next_seq < upper {
        let seq = reader.next_seq;
        let Some(row) = reader.movement(seq)? else { break; };
        if let Some(current) = snapshot(&row) {
            if current == hash {
                first.get_or_insert(seq);
                last_segment = first;
            } else { first = None; }
        }
    }
    Ok(last_segment.map_or(after_seq, |seq| seq.saturating_sub(1)))
}

fn detached_writer_starts_at(
    reader: &mut (impl CheckedRunEvidenceReader + ?Sized), call: &Movement, path: &Path,
    record: &RunRecord, before: &[u8], before_hash: &str, writers: &[CheckedRunWriter],
) -> Result<bool, String> {
    if call.kind != MovementKind::InterfaceOutbound { return Ok(false); }
    let Some(writer) = writers.iter().find(|writer| call.from == Endpoint::Interface(writer.source.clone())
        && call.to == Endpoint::Interface(writer.door.clone())) else { return Ok(false); };
    let (manifest, payload) = reader.artifact(call.artifact.as_ref().ok_or("observed writer call has no artifact")?)?;
    if manifest.label != writer.call_label { return Ok(false); }
    let artifact: octopus_tool_handlers::ToolCallArtifact = serde_json::from_slice(&payload).map_err(|error| error.to_string())?;
    if artifact.tool != writer.tool || artifact.arguments.get("ordered").is_none()
        || artifact.arguments.pointer("/writes/1/expected_before_hash").and_then(|value| value.as_str()) != Some(before_hash)
    { return Ok(false); }
    let ordered: writer_run::OrderedArguments = serde_json::from_value(artifact.arguments).map_err(|error| error.to_string())?;
    if !ordered.targets(path) { return Ok(false); }
    let runtime = call.runtime().ok_or("observed writer call has no admitted coordinate")?;
    let hash = runtime.run_record_hash.as_deref().and_then(ContentHash::from_hex)
        .ok_or("observed writer call has no admitted writer snapshot")?;
    let snapshot = reader.snapshot(&hash)?;
    if ContentHash::of_bytes(&snapshot) != hash { return Err("observed writer snapshot failed integrity".into()); }
    let declaration = RunRecord::from_bytes(&snapshot).map_err(|error| error.to_string())?;
    if declaration.id != runtime.run || declaration.ee != record.ee || declaration.worker != record.worker
        || runtime.ee != record.ee || runtime.worker != record.worker || runtime.interface != writer.door {
        return Err("observed writer snapshot changed its exact admitted scope".into());
    }
    let (preimage, _) = ordered.checked_call(writer, &snapshot, path, reader.max_payload_bytes())?;
    Ok(preimage == before)
}

fn concurrent_writer_evidence_start(
    evidence: &CheckedRunEvidence<'_>, path: &Path, record: &RunRecord, before: &[u8],
    after_seq: u64, writers: &[CheckedRunWriter],
) -> Result<u64, String> {
    validate_checked_run_writers(writers)?;
    let first = observed_run_evidence_start(evidence, record, before, after_seq)?;
    let before_hash = ContentHash::of_bytes(before).to_hex();
    let upper = first.checked_add(1).ok_or("checked writer evidence interval overflow")?;
    let mut reader = FileCheckedRunReader::open(evidence, None)?;
    // Detached calls pin their writer Run, while their ordered target write
    // carries the selected Run's preimage. Both precede the same full checker.
    let indexed = LedgerSqliteReader::open(evidence.ledger_index_path).ok().and_then(|index| {
        if !index.progress().is_ok_and(|progress| progress.watermark >= upper) { return None; }
        let mut sequences = BTreeSet::new();
        for writer in writers {
            for row in index.movement_rows_for_runtime_interface(writer.door.as_str(), 0, upper).ok()?? {
                sequences.insert(row.seq);
            }
        }
        Some(sequences)
    });
    if let Some(sequences) = indexed {
        for seq in sequences {
            let row = reader.admitted_movement(seq)?.ok_or("checked writer evidence interval is incomplete")?;
            if detached_writer_starts_at(&mut reader, &row, path, record, before, &before_hash, writers)? {
                return seq.checked_sub(1).ok_or("observed writer call has no earlier evidence boundary".into());
            }
        }
    } else {
        while reader.next_seq < upper {
            let seq = reader.next_seq;
            let row = reader.movement(seq)?.ok_or("checked writer evidence interval is incomplete")?;
            if detached_writer_starts_at(&mut reader, &row, path, record, before, &before_hash, writers)? {
                return seq.checked_sub(1).ok_or("observed writer call has no earlier evidence boundary".into());
            }
        }
    }
    Ok(first)
}

fn checked_selected_run_advance(
    ctx: &InterfaceLiftContext,
    path: &Path,
    before: &[u8],
    after: &[u8],
    after_seq: u64,
    config: &RunAuthoringConfig,
) -> Result<Vec<CheckedRunTransition>,String> {
    checked_selected_run_advance_state(ctx, path, before, after, after_seq, config).map_err(Into::into)
}

fn checked_selected_run_advance_state(
    ctx: &InterfaceLiftContext, path: &Path, before: &[u8], after: &[u8], after_seq: u64,
    config: &RunAuthoringConfig,
) -> Result<Vec<CheckedRunTransition>, CheckedRunError> {
    let mut writers = config.checkpoints.iter().filter_map(|cp| cp.owner_authoring.clone()).collect::<Vec<_>>();
    let original = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let current = RunRecord::from_bytes(after).map_err(|error| error.to_string())?;
    let payload_revision = input_revision::is_payload_revision(&original, &current, &config.provider_interface)?;
    if writers.is_empty() && !payload_revision
        && payload_artifact_bindings(&original, &config.provider_interface)?.is_empty()
        && payload_artifact_bindings(&current, &config.provider_interface)?.is_empty() {
        if let Some(protected) = output_connections::protected_record(before, after, config)? {
            if ContentHash::of_bytes(before) != ContentHash::of_bytes(&protected) {
                return Err("outgoing connection changed protected provider state without a checked writer chain".into());
            }
        }
        return Ok(Vec::new());
    }
    let provider_writer = CheckedRunWriter {
        owner:config.provider_interface.clone(),source:config.authoring_source_interface.clone(),
        door:config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?.interface().clone(),
        tool:config.file_write_tool.clone(),call_label:config.file_write_label.clone(),result_label:config.file_write_result_label.clone(),
    };
    writers.push(provider_writer.clone());
    let limits = ctx.required_host_io_limits()?;
    let evidence = CheckedRunEvidence { movements_path:&ctx.movements_path,ledger_index_path:&ctx.ledger_index_path,blobs_dir:&ctx.blobs_dir,max_control_frame_bytes:limits.max_control_frame_bytes,max_manifest_bytes:limits.max_manifest_bytes,max_payload_bytes:limits.max_payload_bytes };
    if ContentHash::of_bytes(before) == ContentHash::of_bytes(after) { return Ok(Vec::new()); }
    let record = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let writers = declared_writer_closure(&record, writers)?;
    let first = concurrent_writer_evidence_start(&evidence, path, &record, before, after_seq, &writers)?;
    let mut reader = FileCheckedRunReader::open(&evidence, Some(first))?.with_provider_context(ctx);
    let scan_start = reader.next_seq;
    let chain = checked_run_file_chain_with_reader(&mut reader, path, before, CheckedRunTarget::Snapshot(after), first, scan_start,
        None, &writers, &[], Some(config), None)?;
    for transition in &chain {
        let original = RunRecord::from_bytes(&transition.before).map_err(|error| error.to_string())?;
        let next = RunRecord::from_bytes(&transition.after).map_err(|error| error.to_string())?;
        if payload_artifact_bindings(&original, &config.provider_interface)? != payload_artifact_bindings(&next, &config.provider_interface)?
            && !(transition.writer == provider_writer && input_revision::is_payload_revision(&original, &next, &config.provider_interface)?) {
            return Err("checked owner advance changed the exact payload selection".into());
        }
        if transition.writer == provider_writer { validate_checked_provider_transition(transition)?; }
        else { validate_checked_owner_transition(transition)?; }
    }
    Ok(chain)
}

fn stable_run_observation<T>(
    path: &Path,
    captured: &[u8],
    max_bytes: u64,
    observe: impl FnOnce() -> Result<T, CheckedRunError>,
) -> Result<T, CheckedRunError> {
    let result = observe();
    let current = octopus_interfaces::read_artifact_carrier_file_bounded(path, max_bytes)
        .map_err(|error| error.to_string())?;
    if current != captured {
        return Err(CheckedRunError::ChangedDuringRead {
            captured: ContentHash::of_bytes(captured).to_hex(),
            current: ContentHash::of_bytes(&current).to_hex(),
        });
    }
    result
}

fn checked_live_run_advance(
    ctx: &InterfaceLiftContext,
    path: &Path,
    before: &[u8],
    after_seq: u64,
    config: &RunAuthoringConfig,
) -> Result<(Vec<u8>, Vec<CheckedRunTransition>), CheckedRunError> {
    let limit = ctx.required_host_io_limits()?.max_payload_bytes;
    let current = octopus_interfaces::read_artifact_carrier_file_bounded(path, limit)
        .map_err(|error| error.to_string())?;
    let chain = stable_run_observation(path, &current, limit, || {
        checked_selected_run_advance_state(ctx, path, before, &current, after_seq, config)
    })?;
    Ok((current, chain))
}

pub fn checked_provider_run_advance(
    ctx: &InterfaceLiftContext, path: &Path, before: &[u8], after: &[u8], after_seq: u64,
    provider: &InterfaceId, profile: &RunAuthoringProfile,
) -> Result<Vec<CheckedRunTransition>, String> {
    let record = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let config = config_from_persisted_profile(&record, provider, profile)?;
    checked_selected_run_advance(ctx, path, before, after, after_seq, &config)
}

fn extend_amendment_context_bindings(
    record: &RunRecord,
    provider: &InterfaceId,
    context: &mut Vec<ArtifactId>,
    static_context: &mut Vec<ArtifactId>,
) -> Result<(), String> {
    let prior_context = context.iter().cloned().collect::<BTreeSet<_>>();
    for binding in provider_context_bindings(record, provider)? {
        if binding.access.read && !prior_context.contains(&binding.id) {
            append_artifact(static_context, binding.id.clone());
            append_artifact(context, binding.id);
        }
    }
    Ok(())
}

pub fn provider_context_bindings(record: &RunRecord, provider: &InterfaceId) -> Result<Vec<ArtifactBinding>, String> {
    let topology: BoundRunAuthoringTopology = serde_json::from_value(record.interface_state.get(provider)
        .and_then(|state| state.get(RUN_AUTHORING_BINDING)).ok_or("provider context has no declared topology")?.clone())
        .map_err(|error| error.to_string())?;
    topology.validate_identity(record, provider)?;
    validate_checkpoint_topology(record, provider, &topology.checkpoints, None)?;
    let mut context = None;
    for template in topology.checkpoints.iter().filter(|template| template.role == RunAuthoringCheckpointRole::Stable && template.include_context) {
        let candidates = record.checkpoints.iter().filter(|cp| record.active_checkpoints.contains(&cp.id)
            && checkpoint_record_matches_template(cp, template, &record.id)).collect::<Vec<_>>();
        let [checkpoint] = candidates.as_slice() else { return Err("provider context has no unique stable input".into()); };
        if context.as_ref().is_some_and(|bound| bound != &checkpoint.artifact_bindings) {
            return Err("provider stable inputs carry different context".into());
        }
        context = Some(checkpoint.artifact_bindings.clone());
    }
    context.ok_or_else(|| "provider has no declared stable context".into())
}

pub fn validate_unchanged_run_neighbors(
    before: &[u8], after: &[u8], owner: &InterfaceId,
    before_owned: &BTreeSet<String>, after_owned: &BTreeSet<String>, permits_status_change: bool,
) -> Result<(), String> {
    let original: serde_json::Value = serde_json::from_slice(before).map_err(|e| e.to_string())?;
    let next: serde_json::Value = serde_json::from_slice(after).map_err(|e| e.to_string())?;
    let old_rows = original.get("checkpoints").and_then(serde_json::Value::as_array).ok_or("checked run has no checkpoints")?;
    for id in after_owned.difference(before_owned) {
        if old_rows.iter().any(|cp| cp.get("id").and_then(serde_json::Value::as_str) == Some(id.as_str())) {
            return Err("checked author tried to claim a preexisting neighboring checkpoint".into());
        }
    }
    let project = |mut value: serde_json::Value, owned: &BTreeSet<String>| -> Result<serde_json::Value, String> {
        let rows = value.get_mut("checkpoints").and_then(serde_json::Value::as_array_mut).ok_or("checked run has no checkpoints")?;
        for id in owned {
            if rows.iter().filter(|cp| cp.get("id").and_then(serde_json::Value::as_str) == Some(id.as_str())).count() != 1 { return Err("checked author owns a missing or duplicated checkpoint".into()); }
        }
        rows.retain(|cp| !cp.get("id").and_then(serde_json::Value::as_str).is_some_and(|id| owned.contains(id)));
        value.get_mut("active_checkpoints").and_then(serde_json::Value::as_array_mut).ok_or("checked run has no active checkpoints")?.retain(|id| !id.as_str().is_some_and(|id| owned.contains(id)));
        value.get_mut("interface_state").and_then(serde_json::Value::as_object_mut).ok_or("checked run has no interface state")?.remove(owner.as_str()).ok_or("checked author has no own interface state")?;
        if permits_status_change { value.as_object_mut().ok_or("checked run is not an object")?.remove("status"); }
        Ok(value)
    };
    if project(original, before_owned)? != project(next, after_owned)? { return Err("checked author changed a protected run field, owner or neighboring checkpoint".into()); }
    Ok(())
}

pub fn validate_unchanged_admitted_checkpoint(before: &[u8], after: &[u8], checkpoint: &CheckpointId) -> Result<(),String> {
    let before = RunRecord::from_bytes(before).map_err(|e| e.to_string())?;
    let after = RunRecord::from_bytes(after).map_err(|e| e.to_string())?;
    let original = before.checkpoint(checkpoint).ok_or("shared run input lost its original checkpoint")?;
    if before.id != after.id || before.ee != after.ee || before.worker != after.worker || after.checkpoint(checkpoint) != Some(original) || !after.active_checkpoints.contains(checkpoint) {
        return Err("shared run changed the exact admitted checkpoint before consumption".into());
    }
    Ok(())
}

pub fn validate_checked_provider_transition(transition: &CheckedRunTransition) -> Result<(), String> {
    let before = RunRecord::from_bytes(&transition.before).map_err(|e| e.to_string())?;
    let after = RunRecord::from_bytes(&transition.after).map_err(|e| e.to_string())?;
    let provider = &transition.writer.owner;
    source_settlement::validate_checked_transition(&before, &after, &transition.writer)?;
    source_withdrawal::validate_checked_transition(&before, &after, &transition.writer)?;
    if capability_amendment::validate_transition(transition)? { return Ok(()); }
    let read = |record: &RunRecord| -> Result<BoundRunAuthoringTopology, String> {
        let state = record.interface_state.get(provider).ok_or("checked provider lost its state")?;
        serde_json::from_value(state.get(RUN_AUTHORING_BINDING).cloned().ok_or("checked provider lost its topology")?).map_err(|e| e.to_string())
    };
    let topology = read(&before)?;
    topology.validate_identity(&before,provider)?;
    let next_topology = read(&after)?;
    next_topology.validate_identity(&after,provider)?;
    refuse_selected_consent_state(&before, provider)?;
    refuse_selected_consent_state(&after, provider)?;
    if input_revision::is_payload_revision(&before, &after, provider)? {
        return input_revision::validate_payload_revision(&transition.before, &transition.after, &transition.writer);
    }
    if input_revision::is_revision(&topology, &next_topology)
        || input_revision::is_rearm(&before, &after, provider)? {
        return input_revision::validate(&transition.before, &transition.after, &transition.writer);
    }
    if !preserves_provider_topology(&topology, &next_topology)
        || topology.authoring_source_interface != transition.writer.source {
        return Err("coauthor changed the provider's exact topology or writer".into());
    }
    for (role, source, target) in [
        (RunAuthoringCheckpointRole::AuthoringCall,&transition.writer.source,&transition.writer.door),
        (RunAuthoringCheckpointRole::AuthoringResult,&transition.writer.door,provider),
    ] {
        let matches = topology.checkpoints.iter().filter(|cp| cp.role == role && cp.interface_binding.interface == *source && cp.outbound_interface_bindings.len() == 1 && cp.outbound_interface_bindings[0].interface == *target).count();
        if matches != 1 { return Err("coauthor writer is not the provider's exact declared authoring edge".into()); }
    }
    validate_checkpoint_topology(&before,provider,&topology.checkpoints,None)?;
    validate_checkpoint_topology(&after,provider,&next_topology.checkpoints,None)?;
    let roles = |record: &RunRecord| -> Result<Vec<ArtifactBinding>,String> {
        if let Some(binding) = writer_run::binding(record,provider)? {
            return binding.roles(record,provider,&topology.checkpoints);
        }
        let ids = raw_frontier_checkpoint_ids(record.interface_state.get(provider).ok_or("provider state absent")?)?;
        let rows = ids.iter().map(|id| record.checkpoint(&CheckpointId::new(id)).ok_or("provider frontier absent")).collect::<Result<Vec<_>,_>>()?;
        let call = topology.checkpoints.iter().find(|cp| cp.role == RunAuthoringCheckpointRole::AuthoringCall).ok_or("provider authoring call absent")?;
        let result = topology.checkpoints.iter().find(|cp| cp.role == RunAuthoringCheckpointRole::AuthoringResult).ok_or("provider authoring result absent")?;
        canonical_role_bindings(&rows,call,result,&record.id)
    };
    let mut expected_roles = roles(&before)?;
    if topology.writer_run.is_none() {
        for checkpoint in next_topology.checkpoints.iter().filter(|cp| !topology.checkpoints.contains(cp) && cp.role != RunAuthoringCheckpointRole::VerifiedInput) {
            for id in &checkpoint.static_artifact_bindings {
                let binding: ArtifactBinding = serde_json::from_value(serde_json::json!({"id": id})).map_err(|error| error.to_string())?;
                if !expected_roles.contains(&binding) { expected_roles.push(binding); }
            }
        }
    }
    if expected_roles != roles(&after)? { return Err("coauthor changed the mandatory role binding closure beyond its authored additions".into()); }
    let prior = before.interface_state.get(provider).ok_or("provider state absent")?;
    let next = after.interface_state.get(provider).ok_or("provider state absent")?;
    if optional_u64(next,"authoring_generation")? != optional_u64(prior,"authoring_generation")?.checked_add(1).ok_or("provider generation overflow")? {
        return Err("checked provider transition skipped an authoring generation".into());
    }
    let old_context = optional_u64(prior,"context_generation")?;
    let new_context = optional_u64(next,"context_generation")?;
    if new_context != old_context && Some(new_context) != old_context.checked_add(1) { return Err("checked provider context generation skipped".into()); }
    let pending = persisted_pending_handoff(&after,provider)?.ok_or("checked provider write has no pending handoff")?;
    validate_verified_input_change(&topology, &next_topology, &pending)?;
    next_topology.config(&RunAuthoringProfile {
        file_write_tool: transition.writer.tool.clone(), file_write_label: transition.writer.call_label.clone(),
        file_write_result_label: transition.writer.result_label.clone(), result_timeout_ms: 1,
    }).validate(provider, &after.id)?;
    if pending.source_movement_seq >= transition.call.seq.0 || after.status != serde_json::json!(if pending.terminal { "idle" } else { "running" }) {
        return Err("checked provider write has invalid source timing or status".into());
    }
    let stable = |state: &BTreeMap<String,serde_json::Value>| {
        let mut state = state.clone();
        for key in [FRONTIER_CHECKPOINT_IDS,"context_generation","authoring_generation","last_authored_artifact","pending_handoff",RUN_AUTHORING_BINDING] { state.remove(key); }
        state
    };
    if stable(prior) != stable(next) { return Err("checked provider changed immutable interface state".into()); }
    let owned = |record: &RunRecord| -> Result<BTreeSet<String>,String> {
        let topology = read(record)?;
        let mut ids = raw_frontier_checkpoint_ids(record.interface_state.get(provider).ok_or("provider state absent")?)?.into_iter().collect::<BTreeSet<_>>();
        for cp in &record.checkpoints {
            if topology.checkpoints.iter().any(|template| matches!(template.role,RunAuthoringCheckpointRole::Stable | RunAuthoringCheckpointRole::IndependentInput | RunAuthoringCheckpointRole::VerifiedInput) && checkpoint_record_matches_template(cp,template,&record.id)) {
                ids.insert(cp.id.to_string());
            }
        }
        Ok(ids)
    };
    validate_unchanged_run_neighbors(&transition.before,&transition.after,provider,&owned(&before)?,&owned(&after)?,true)
}

fn preserves_provider_topology(before: &BoundRunAuthoringTopology, after: &BoundRunAuthoringTopology) -> bool {
    let mut prior = before.clone();
    let mut next = after.clone();
    prior.checkpoints.clear();
    next.checkpoints.clear();
    let old = before.checkpoints.iter().filter(|cp| cp.role != RunAuthoringCheckpointRole::VerifiedInput).collect::<Vec<_>>();
    let new = after.checkpoints.iter().filter(|cp| cp.role != RunAuthoringCheckpointRole::VerifiedInput).collect::<Vec<_>>();
    prior == next && new.starts_with(&old) && new[old.len()..].iter().all(|checkpoint| {
        checkpoint.role == RunAuthoringCheckpointRole::ProviderInterfaceOutput && checkpoint.owner_authoring.is_none()
    })
}

fn declared_writer_closure(record: &RunRecord, seeds: Vec<CheckedRunWriter>) -> Result<Vec<CheckedRunWriter>, String> {
    let mut pending = seeds;
    let mut selected = BTreeMap::new();
    while let Some(writer) = pending.pop() {
        if let Some(prior) = selected.get(&writer.owner) {
            if prior != &writer { return Err("declared coauthors disagree on an exact writer".into()); }
            continue;
        }
        if let Some(value) = record.interface_state.get(&writer.owner).and_then(|state| state.get(RUN_AUTHORING_BINDING)) {
            let topology: BoundRunAuthoringTopology = serde_json::from_value(value.clone()).map_err(|e|e.to_string())?;
            topology.validate_identity(record, &writer.owner)?;
            pending.extend(topology.checkpoints.iter().filter_map(|cp| cp.owner_authoring.clone()));
        }
        selected.insert(writer.owner.clone(), writer);
    }
    Ok(selected.into_values().collect())
}

pub fn validate_independent_provider_transition(record: &RunRecord, provider: &InterfaceId, profile: &RunAuthoringProfile, transition: &CheckedRunTransition) -> Result<(),String> {
    let config = config_from_persisted_profile(record,provider,profile)?;
    let own = CheckedRunWriter {owner:provider.clone(),source:config.authoring_source_interface.clone(),
        door:config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?.interface().clone(),tool:profile.file_write_tool.clone(),call_label:profile.file_write_label.clone(),result_label:profile.file_write_result_label.clone()};
    if !declared_writer_closure(record,vec![own])?.contains(&transition.writer) { return Err("independent transition has no exact declared writer".into()); }
    validate_checked_owner_transition(transition)
}

pub fn checked_declared_run_advance(
    evidence: &CheckedRunEvidence<'_>, path: &Path, before: &[u8], after: &[u8],
    after_seq: u64, seeds: Vec<CheckedRunWriter>,
) -> Result<Vec<CheckedRunTransition>, CheckedRunError> {
    let record = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let writers = declared_writer_closure(&record, seeds)?;
    let first = concurrent_writer_evidence_start(evidence, path, &record, before, after_seq, &writers)?;
    let transitions = checked_run_file_chain_state(evidence, path, before, after, first, &writers)?;
    for transition in &transitions { validate_checked_owner_transition(transition)?; }
    Ok(transitions)
}

pub fn checked_declared_run_snapshot(
    evidence: &CheckedRunEvidence<'_>, path: &Path, before: &[u8], after_seq: u64,
    hash: &str, observed_sequence: u64, seeds: Vec<CheckedRunWriter>,
) -> Result<Vec<u8>, CheckedRunError> {
    if after_seq >= observed_sequence { return Err("observed run snapshot has no earlier admitted anchor".into()); }
    let hash = ContentHash::from_hex(hash).filter(|pin| pin.to_hex() == hash)
        .ok_or("observed run snapshot has no canonical content hash")?;
    let record = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let writers = declared_writer_closure(&record, seeds)?;
    validate_checked_run_writers(&writers)?;
    let first = concurrent_writer_evidence_start(evidence, path, &record, before, after_seq, &writers)?;
    let mut reader = FileCheckedRunReader::open(evidence, Some(first))?;
    let scan_start = reader.next_seq;
    let transitions = checked_run_file_chain_with_reader(&mut reader, path, before,
        CheckedRunTarget::Observed { hash, sequence: observed_sequence }, first, scan_start,
        None, &writers, &[], None, None)?;
    for transition in &transitions { validate_checked_owner_transition(transition)?; }
    let after = transitions.last().map_or_else(|| before.to_vec(), |transition| transition.after.clone());
    let target = RunRecord::from_bytes(&after).map_err(|error| error.to_string())?;
    if ContentHash::of_bytes(&after) != hash || target.id != record.id || target.ee != record.ee || target.worker != record.worker {
        return Err("observed run snapshot differs from its exact checked target scope".into());
    }
    Ok(after)
}

fn validate_verified_input_change(before: &BoundRunAuthoringTopology, after: &BoundRunAuthoringTopology, pending: &PendingHandoff) -> Result<(), String> {
    let old = before.checkpoints.iter().filter(|cp| cp.role == RunAuthoringCheckpointRole::VerifiedInput).collect::<Vec<_>>();
    let new = after.checkpoints.iter().filter(|cp| cp.role == RunAuthoringCheckpointRole::VerifiedInput).collect::<Vec<_>>();
    if old == new { return Ok(()); }
    if !pending.terminal || old.iter().any(|cp| new.contains(cp)) {
        return Err("verified-input change must replace the complete set in a terminal handoff".into());
    }
    if !new.is_empty() {
        let clients = pending.items.iter().filter(|item| item.checkpoint.interface_binding.interface == before.provider_interface)
            .filter_map(|item| match item.checkpoint.outbound_interface_bindings.as_slice() { [client] => Some(client.interface.clone()), _ => None })
            .filter(|client| before.checkpoints.iter().any(|cp| cp.role == RunAuthoringCheckpointRole::ProviderArtifactOutput && cp.outbound_interfaces().eq([client])))
            .collect::<BTreeSet<_>>();
        if clients.len() != 1 || new.iter().any(|cp| !clients.contains(cp.interface())) {
            return Err("verified-input request lost its exact pending client projection".into());
        }
    }
    Ok(())
}

fn validate_verified_input_transition(reader: &mut impl CheckedRunEvidenceReader, transition: &CheckedRunTransition) -> Result<(), String> {
    let before = RunRecord::from_bytes(&transition.before).map_err(|e| e.to_string())?;
    let after = RunRecord::from_bytes(&transition.after).map_err(|e| e.to_string())?;
    let read = |record: &RunRecord| -> Result<Option<BoundRunAuthoringTopology>, String> {
        record.interface_state.get(&transition.writer.owner).and_then(|state| state.get(RUN_AUTHORING_BINDING))
            .map(|value|serde_json::from_value(value.clone()).map_err(|e|e.to_string())).transpose()
    };
    let (Some(old), Some(new)) = (read(&before)?, read(&after)?) else { return Ok(()); };
    let prior = old.checkpoints.iter().filter(|cp| cp.role == RunAuthoringCheckpointRole::VerifiedInput).collect::<Vec<_>>();
    let next = new.checkpoints.iter().filter(|cp| cp.role == RunAuthoringCheckpointRole::VerifiedInput).collect::<Vec<_>>();
    if prior == next { return Ok(()); }
    let pending = persisted_pending_handoff(&after, &transition.writer.owner)?.ok_or("verified-input change has no handoff")?;
    validate_verified_input_change(&old, &new, &pending)?;
    let metadata = transition.call.invocation_metadata.as_ref().and_then(|m|m.invocation()).ok_or("verified-input change has no admitted writer grant")?;
    if next.iter().flat_map(|cp| &cp.required_verified_artifacts).any(|r| !metadata.grant.mark_outcomes.iter().any(|id| id == r.mark.as_str())) {
        return Err("verified-input request names an ungranted mark".into());
    }
    if prior.is_empty() { return Ok(()); }
    let source = reader.admitted_movement(pending.source_movement_seq)?.ok_or("verified-input retirement has no admitted source")?;
    let metadata = source.invocation_metadata.as_ref().and_then(|m|m.invocation()).ok_or("verified-input source has no metadata")?;
    let runtime = metadata.runtime.as_ref().ok_or("verified-input source has no coordinate")?;
    let hash = runtime.run_record_hash.as_deref().and_then(ContentHash::from_hex).ok_or("verified-input source has no pinned snapshot")?;
    let bytes = reader.snapshot(&hash)?;
    if ContentHash::of_bytes(&bytes) != hash { return Err("verified-input source snapshot failed integrity".into()); }
    let pinned = RunRecord::from_bytes(&bytes).map_err(|e|e.to_string())?;
    let checkpoint = pinned.checkpoint(&runtime.checkpoint).ok_or("verified-input source checkpoint is absent")?;
    let matches = prior.iter().filter(|cp| checkpoint_record_matches_template(checkpoint, cp, &before.id)).collect::<Vec<_>>();
    if source.kind != MovementKind::InterfaceOutbound || source.seq.0 >= transition.call.seq.0
        || source.to != Endpoint::Interface(transition.writer.owner.clone()) || runtime.interface != transition.writer.owner
        || runtime.run != before.id || runtime.worker != before.worker || runtime.ee != before.ee
        || pinned.id != before.id || pinned.worker != before.worker || pinned.ee != before.ee
        || !pinned.active_checkpoints.contains(&runtime.checkpoint) || !before.active_checkpoints.contains(&runtime.checkpoint)
        || before.checkpoint(&runtime.checkpoint) != Some(checkpoint) || runtime.epoch != checkpoint.epoch.0 || matches.len() != 1
        || metadata.verification_failure.is_some() || metadata.checkpoint_binding_policy.as_ref() != Some(&matches[0].binding_policy_snapshot())
        || metadata.required_verified_artifacts != matches[0].required_verified_artifacts
        || metadata.outbound_interface_bindings.iter().ne(checkpoint.outbound_interfaces())
        || !artifact_refs_match_bindings(&metadata.artifact_binding_refs, &checkpoint.artifact_bindings)
        || !metadata.checkpoint_admission.as_ref().is_some_and(|a|a.complete && a.artifact_label == matches[0].required_verified_artifacts[0].artifact_label
            && a.required_artifact_labels == vec![a.artifact_label.clone()])
    { return Err("verified-input retirement did not consume its exact admitted verified source".into()); }
    let template = matches[0];
    if source.from != Endpoint::Interface(template.interface().clone()) { return Err("verified-input source owner differs".into()); }
    let (manifest, _) = reader.artifact(source.artifact.as_ref().ok_or("verified-input source has no artifact")?)?;
    if manifest.label != template.required_verified_artifacts[0].artifact_label { return Err("verified-input source label differs".into()); }
    Ok(())
}

pub fn validate_checked_owner_transition(transition: &CheckedRunTransition) -> Result<(), String> {
    let record = RunRecord::from_bytes(&transition.before).map_err(|e| e.to_string())?;
    if record.interface_state.get(&transition.writer.owner).is_some_and(|state| state.contains_key(RUN_AUTHORING_BINDING)) {
        validate_declared_provider_writer(&record, &transition.writer)?;
        validate_declared_provider_writer(&RunRecord::from_bytes(&transition.after).map_err(|e| e.to_string())?, &transition.writer)?;
        return validate_checked_provider_transition(transition);
    }
    let owner = &transition.writer.owner;
    let before = RunRecord::from_bytes(&transition.before).map_err(|e| e.to_string())?;
    let after = RunRecord::from_bytes(&transition.after).map_err(|e| e.to_string())?;
    let detached = owner_writer_binding(&before, owner)?;
    if detached != owner_writer_binding(&after, owner)? { return Err("checked owner changed its exact writer Run".into()); }
    let owner_frontier = |state: &BTreeMap<String, serde_json::Value>| {
        if detached.is_some() && state.get(FRONTIER_CHECKPOINT_IDS) == Some(&serde_json::json!([])) {
            Ok(BTreeSet::new())
        } else {
            raw_frontier_checkpoint_ids(state)
        }
    };
    let metadata = transition.call.invocation_metadata.as_ref().and_then(|m| m.invocation())
        .ok_or("checked owner write has no admitted role grant")?;
    let role_bindings = generated_artifacts::mandatory_role_bindings(&metadata.grant)?;
    let raw_ids = |record: &RunRecord| -> Result<BTreeSet<String>,String> {
        record.interface_state.get(owner).and_then(|state|state.get("output_artifact_checkpoints"))
            .map(|value|serde_json::from_value(value.clone()).map_err(|e|e.to_string())).unwrap_or_else(||Ok(BTreeSet::new()))
    };
    let ownership = |record: &RunRecord| -> Result<BTreeSet<String>, String> {
        let state = record.interface_state.get(owner).ok_or("checked owner has no interface state")?;
        let values = state.get("owned_checkpoint_ids").and_then(serde_json::Value::as_array).ok_or("checked owner has no exact checkpoint set")?;
        let mut ids = BTreeSet::new();
        for value in values {
            let id = value.as_str().filter(|s| !s.is_empty()).ok_or("checked owner checkpoint id is invalid")?;
            if !ids.insert(id.into()) { return Err("checked owner repeats a checkpoint id".into()); }
            let cp = record.checkpoint(&CheckpointId::new(id)).ok_or("checked owner checkpoint is absent")?;
            let raw = raw_ids(record)?.contains(id);
            if raw && (!cp.outbound_interface_bindings.is_empty() || cp.artifact_bindings != role_bindings || !cp.required_verified_artifacts.is_empty()
                || cp.interface_binding.as_ref().is_none_or(|binding| !matches!(&binding.config.artifact_policy,BindingArtifactPolicy::Allow { names,namespaces } if namespaces.is_empty() && names.len()==1 && names[0].strip_prefix("tool.output.").is_some_and(|hash| ContentHash::from_hex(hash).is_some())))) {
                return Err("checked owner raw output is not an exact admission-only checkpoint".into());
            }
            if !record.active_checkpoints.contains(&cp.id) || (!raw && cp.inbound_interface() != Some(owner) && cp.outbound_interfaces().next() != Some(owner)) {
                return Err("checked owner claims a foreign or inactive checkpoint".into());
            }
        }
        let frontier = owner_frontier(state)?;
        if detached.is_some() {
            if !frontier.is_empty() { return Err("detached owner retains a target authoring frontier".into()); }
            return Ok(ids);
        }
        if frontier.len() != 2 || frontier.iter().any(|id| !ids.contains(id)) { return Err("checked owner frontier is not two owned edges".into()); }
        let result = frontier.iter().filter_map(|id| record.checkpoint(&CheckpointId::new(id))).filter(|cp| cp.inbound_interface() == Some(&transition.writer.door) && cp.outbound_interfaces().collect::<Vec<_>>() == [owner]).count();
        if result != 1 { return Err("checked owner lost its one private authoring return".into()); }
        Ok(ids)
    };
    let before_owned = ownership(&before)?;
    let after_owned = ownership(&after)?;
    let stable = |record: &RunRecord| -> Result<BTreeMap<String,serde_json::Value>,String> {
        let mut state = record.interface_state.get(owner).ok_or("checked owner state absent")?.clone();
        state.remove(FRONTIER_CHECKPOINT_IDS);state.remove("owned_checkpoint_ids");state.remove("output_artifact_checkpoints");state.remove("generated_artifacts");state.remove("input_artifact_bindings");Ok(state)
    };
    if stable(&before)? != stable(&after)? { return Err("checked owner changed immutable interface configuration".into()); }
    let before_outputs = generated_artifacts::recorded_generated_artifacts(&before,owner)?;
    let after_outputs = generated_artifacts::recorded_generated_artifacts(&after,owner)?;
    if before_outputs.iter().any(|old|!after_outputs.contains(old)) { return Err("checked owner removed or replaced a generated artifact".into()); }
    let mut output_ids = BTreeSet::new();
    for output in &after_outputs {
        if !output_ids.insert(output.artifact.id.clone()) || output.admission_seq >= transition.call.seq.0
            || output.source_movement_seq >= output.admission_seq { return Err("checked owner generated artifact has invalid identity or timing".into()); }
    }
    let without_outputs = |bindings: &[ArtifactBinding]| bindings.iter().filter(|binding|!output_ids.contains(&binding.id)).cloned().collect::<Vec<_>>();
    let before_frontier = owner_frontier(before.interface_state.get(owner).ok_or("checked owner state absent")?)?;
    let returns = before_frontier.iter().filter_map(|id| before.checkpoint(&CheckpointId::new(id)))
        .filter(|cp| cp.inbound_interface() == Some(&transition.writer.door)
            && cp.outbound_interfaces().collect::<Vec<_>>() == [owner]).collect::<Vec<_>>();
    let original = if let Some(binding) = &detached {
        without_outputs(&binding.role_artifact_bindings)
    } else {
        let [first] = returns.as_slice() else { return Err("checked owner has no unique original authoring return".into()); };
        without_outputs(&first.artifact_bindings)
    };
    if role_bindings.iter().any(|role| !original.iter().any(|binding| binding.id == role.id && binding.access.read)) {
        return Err("checked owner role grant differs from its original admitted bindings".into());
    }
    let mut context_inputs = Vec::new();
    for (provider, state) in &before.interface_state {
        let Some(value) = state.get(RUN_AUTHORING_BINDING) else { continue; };
        let topology: BoundRunAuthoringTopology = serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
        topology.validate_identity(&before, provider)?;
        for template in topology.checkpoints.iter().filter(|template| template.role == RunAuthoringCheckpointRole::OwnerInput
            && template.include_context && template.owner_authoring.as_ref() == Some(&transition.writer)) {
            context_inputs.push((template.clone(), provider_context_bindings(&before, provider)?));
        }
    }
    for (record, ids, publishing) in [(&before,&before_owned,false),(&after,&after_owned,true)] {
        let raw = raw_ids(record)?;
        if !raw.is_subset(ids) { return Err("checked owner does not own its raw output checkpoints".into()); }
        for id in ids {
            if raw.contains(id) { continue; }
            let checkpoint = record.checkpoint(&CheckpointId::new(id)).ok_or("checked owner row absent")?;
            let bindings = &checkpoint.artifact_bindings;
            if let Some((template, context)) = context_inputs.iter().find(|(template, _)| checkpoint_record_matches_template(checkpoint, template, &record.id)) {
                if !checkpoint_role_bindings_match(checkpoint, &role_bindings, template)
                    || bindings.iter().any(|binding| !binding.access.read)
                    || (publishing && *bindings != input_artifacts::read_bindings(record, owner, context.clone())?) {
                    return Err("checked input owner changed its declared provider context bindings".into());
                }
                continue;
            }
            if bindings.iter().any(|binding|!binding.access.read || (output_ids.contains(&binding.id) && binding.access != octopus_runtime::ArtifactAccess::read_only())) || without_outputs(bindings) != original {
                return Err("checked owner changed its exact role, prelude or topology bindings".into());
            }
        }
    }
    validate_unchanged_run_neighbors(&transition.before,&transition.after,owner,&before_owned,&after_owned,false)
}

#[derive(Clone)]
pub struct RunAuthoringResultIdentity {
    provider_interface: InterfaceId,
    profile: RunAuthoringProfile,
    writer_history: Arc<Mutex<BTreeMap<RunId,writer_run::WriterHistory>>>,
    context_output_labels: BTreeSet<String>,
    session_snapshot_labels: BTreeSet<String>,
}

impl RunAuthoringResultIdentity {
    pub fn from_descriptor(descriptor: &InterfaceDescriptor) -> Result<Self, String> {
        let profile = run_authoring_profile(descriptor)?;
        let (context_output_labels, session_snapshot_labels) =
            provider_context_label_sets(descriptor)?;
        profile.validate(&descriptor.id)?;
        Ok(Self {
            provider_interface: descriptor.id.clone(),
            profile,
            writer_history: Arc::new(Mutex::new(BTreeMap::new())),
            context_output_labels,
            session_snapshot_labels,
        })
    }

    pub fn artifact_label(&self) -> &str {
        &self.profile.file_write_result_label
    }

    pub async fn consume_if_matches(
        &self,
        ctx: &InterfaceLiftContext,
        movement: &Movement,
        envelope: &InterfaceInvocationEnvelope,
        artifact_label: &str,
    ) -> Result<bool, String> {
        if ctx.interface != self.provider_interface {
            return Err(format!(
                "run authoring result identity belongs to '{}', lifted interface is '{}'",
                self.provider_interface.as_str(),
                ctx.interface.as_str()
            ));
        }
        if artifact_label == self.artifact_label() {
            let payload = envelope.read_artifact_payload().map_err(|error| {
                format!(
                    "provider movement {} authoring-result payload cannot be read: {error}",
                    movement.seq.0
                )
            })?;
            let pinned_run_record = ctx.pinned_run_record_bytes(envelope)?;
            let consumed = async {
                let record = RunRecord::from_bytes(&pinned_run_record).map_err(|error| {
                    format!("provider authoring result pinned run record is malformed: {error}")
                })?;
                if writer_run::consume_result(ctx, &self.writer_history, &self.profile, movement, &record)? { return Ok(()); }
                let config = self.config_for_record(&record)?;
                let result: FileWriteResultEnvelope =
                    serde_json::from_slice(&payload).map_err(|error| {
                        format!("provider authoring result payload is malformed: {error}")
                    })?;
                if !result.ok && result.error.as_ref().is_some_and(|error| error.details.is_none()) {
                    return authoring_retry::validate_rejected_result(
                        ctx, &config, movement, envelope, &record, &result,
                    );
                }
                if let Some(failed) = file_write_failure_result(&result, &config, movement.seq.0)? {
                    let authoring_result =
                        config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
                    validate_authoring_failure_movement(
                        &self.provider_interface,
                        movement,
                        envelope,
                        &record,
                        &config,
                        authoring_result,
                    )?;
                    let session = ProviderHandoffSession::new(ctx)?;
                    hand_validated_authoring_failure(
                        ctx,
                        &session,
                        &record,
                        &pinned_run_record,
                        &config,
                        &failed.failure,
                        movement.seq.0,
                        Some(&failed.correlation_id),
                        &self.context_output_labels,
                        &self.session_snapshot_labels,
                    )
                    .await
                } else {
                    self.validate(movement, envelope, &pinned_run_record, &payload)
                }
            }
            .await;
            if let Err(error) = consumed {
                ctx.hand_admitted_failure(
                    movement.seq.0,
                    "malformed_provider_authoring_result",
                    serde_json::json!({ "error": error }),
                )
                .await?;
            }
            return Ok(true);
        }
        if !artifact_label.ends_with(".failure") {
            return Ok(false);
        }
        let pinned_run_record = ctx.pinned_run_record_bytes(envelope)?;
        let record = RunRecord::from_bytes(&pinned_run_record).map_err(|error| {
            format!("provider authoring failure pinned run record is malformed: {error}")
        })?;
        let config = self.config_for_record(&record)?;
        let authoring_result =
            config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
        let expected_label = format!("{}.failure", authoring_result.interface().as_str());
        if artifact_label != expected_label {
            return Ok(false);
        }
        let payload = envelope.read_artifact_payload().map_err(|error| {
            format!(
                "provider movement {} authoring-failure payload cannot be read: {error}",
                movement.seq.0
            )
        })?;
        match self.validate_failure(
            movement,
            envelope,
            &record,
            &config,
            authoring_result,
            &payload,
        ) {
            Ok(failure) => {
                let session = ProviderHandoffSession::new(ctx)?;
                hand_validated_authoring_failure(
                    ctx,
                    &session,
                    &record,
                    &pinned_run_record,
                    &config,
                    &failure,
                    movement.seq.0,
                    None,
                    &self.context_output_labels,
                    &self.session_snapshot_labels,
                )
                .await?;
            }
            Err(error) => {
                ctx.hand_admitted_failure(
                    movement.seq.0,
                    "malformed_provider_authoring_failure",
                    serde_json::json!({ "error": error }),
                )
                .await?;
            }
        }
        Ok(true)
    }

    pub fn validate(
        &self,
        movement: &Movement,
        envelope: &InterfaceInvocationEnvelope,
        pinned_run_record: &[u8],
        payload: &[u8],
    ) -> Result<(), String> {
        let runtime = envelope.runtime().map_err(|error| error.to_string())?;
        let record = RunRecord::from_bytes(pinned_run_record).map_err(|error| {
            format!("provider authoring result pinned run record is malformed: {error}")
        })?;
        if record.id != runtime.run || record.worker != runtime.worker {
            return Err("provider authoring result pinned the wrong run topology".to_string());
        }
        let config = self.config_for_record(&record)?;
        config.validate(&self.provider_interface, &record.id)?;
        let authoring_result =
            config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
        let authoring_result_checkpoint = active_checkpoint_for_role(
            &record,
            &config,
            RunAuthoringCheckpointRole::AuthoringResult,
        )?;
        if movement.kind != MovementKind::InterfaceOutbound
            || movement.from != Endpoint::Interface(authoring_result.interface().clone())
            || movement.to != Endpoint::Interface(self.provider_interface.clone())
            || movement.artifact.as_ref() != Some(&envelope.artifact)
        {
            return Err("provider authoring result has invalid movement provenance".to_string());
        }
        if runtime.interface != self.provider_interface
            || runtime.run != record.id
            || runtime.worker != record.worker
            || runtime.checkpoint != authoring_result_checkpoint.id
            || runtime.epoch != 0
        {
            return Err("provider authoring result has invalid runtime coordinate".to_string());
        }
        let metadata = envelope.metadata().map_err(|error| error.to_string())?;
        if metadata.outbound_interface_bindings
            != authoring_result
                .outbound_interfaces()
                .cloned()
                .collect::<Vec<_>>()
            || metadata.checkpoint_binding_policy.as_ref()
                != Some(&authoring_result.binding_policy_snapshot())
            || metadata.required_verified_artifacts != authoring_result.required_verified_artifacts
            || !artifact_refs_match_bindings(
                &metadata.artifact_binding_refs,
                &authoring_result_checkpoint.artifact_bindings,
            )
        {
            return Err(
                "provider authoring result metadata does not match its authored checkpoint"
                    .to_string(),
            );
        }
        let result: FileWriteResultEnvelope = serde_json::from_slice(payload)
            .map_err(|error| format!("provider authoring result payload is malformed: {error}"))?;
        if result.label != config.file_write_result_label || result.tool != config.file_write_tool {
            return Err(
                "provider authoring result payload names the wrong label or tool".to_string(),
            );
        }
        let correlation_id = result
            .correlation_id
            .as_deref()
            .ok_or_else(|| "provider authoring result payload has no correlation_id".to_string())?;
        let generation = correlation_generation(&record.id, correlation_id)?;
        let provider_state = record
            .interface_state
            .get(&self.provider_interface)
            .ok_or_else(|| {
                "provider authoring result pinned a run without provider state".to_string()
            })?;
        if optional_u64(provider_state, "authoring_generation")? != generation {
            return Err(
                "provider authoring result correlation does not match the run generation"
                    .to_string(),
            );
        }
        let pending = pending_handoff(&record, &self.provider_interface)?.ok_or_else(|| {
            "provider authoring result pinned a run without pending handoff evidence".to_string()
        })?;
        pending.items.first().ok_or_else(|| {
            "provider authoring result pinned an empty pending handoff batch".to_string()
        })?;
        validate_pending_items_are_active(&record, &self.provider_interface, &pending)?;
        let frontier = frontier_checkpoint_ids(provider_state)?;
        if !frontier.contains(runtime.checkpoint.as_str())
            || !record.active_checkpoints.contains(&runtime.checkpoint)
        {
            return Err(
                "provider authoring result arrived outside the active provider-owned frontier"
                    .to_string(),
            );
        }
        let checkpoint = record.checkpoint(&runtime.checkpoint).ok_or_else(|| {
            "provider authoring result checkpoint is absent from the pinned run".to_string()
        })?;
        if checkpoint.epoch.0 != runtime.epoch {
            return Err(
                "provider authoring result checkpoint epoch differs from its runtime coordinate"
                    .to_string(),
            );
        }
        if !checkpoint_record_matches_template(checkpoint, authoring_result, &record.id) {
            return Err(
                "provider authoring result checkpoint topology differs from its descriptor template"
                    .to_string(),
            );
        }
        Ok(())
    }

    fn config_for_record(&self, record: &RunRecord) -> Result<RunAuthoringConfig, String> {
        config_from_persisted_profile(record, &self.provider_interface, &self.profile)
    }

    fn validate_failure(
        &self,
        movement: &Movement,
        envelope: &InterfaceInvocationEnvelope,
        record: &RunRecord,
        config: &RunAuthoringConfig,
        authoring_result: &RunAuthoringCheckpoint,
        payload: &[u8],
    ) -> Result<InterfaceFailureEnvelope, String> {
        validate_authoring_failure_delivery(
            &self.provider_interface,
            movement,
            envelope,
            record,
            config,
            authoring_result,
            payload,
        )
    }
}

fn validate_authoring_failure_delivery(
    provider_interface: &InterfaceId,
    movement: &Movement,
    envelope: &InterfaceInvocationEnvelope,
    record: &RunRecord,
    config: &RunAuthoringConfig,
    authoring_result: &RunAuthoringCheckpoint,
    payload: &[u8],
) -> Result<InterfaceFailureEnvelope, String> {
    let failure: InterfaceFailureEnvelope = serde_json::from_slice(payload)
        .map_err(|error| format!("provider authoring failure payload is malformed: {error}"))?;
    validate_authoring_failure_movement(
        provider_interface,
        movement,
        envelope,
        record,
        config,
        authoring_result,
    )?;
    validate_authoring_failure_fields(&failure, movement.seq.0)?;
    Ok(failure)
}

fn validate_authoring_failure_movement(
    provider_interface: &InterfaceId,
    movement: &Movement,
    envelope: &InterfaceInvocationEnvelope,
    record: &RunRecord,
    config: &RunAuthoringConfig,
    authoring_result: &RunAuthoringCheckpoint,
) -> Result<(), String> {
    let runtime = envelope.runtime().map_err(|error| error.to_string())?;
    if record.id != runtime.run || record.worker != runtime.worker {
        return Err("provider authoring failure pinned the wrong run topology".to_string());
    }
    config.validate(provider_interface, &record.id)?;
    if movement.kind != MovementKind::InterfaceOutbound
        || movement.from != Endpoint::Interface(authoring_result.interface().clone())
        || movement.to != Endpoint::Interface(provider_interface.clone())
        || movement.artifact.as_ref() != Some(&envelope.artifact)
    {
        return Err("provider authoring failure has invalid movement provenance".to_string());
    }
    if runtime.interface != *provider_interface
        || runtime.run != record.id
        || runtime.worker != record.worker
        || runtime.checkpoint
            != active_checkpoint_for_role(
                record,
                config,
                RunAuthoringCheckpointRole::AuthoringResult,
            )?
            .id
            .clone()
        || runtime.epoch != 0
    {
        return Err("provider authoring failure has invalid runtime coordinate".to_string());
    }
    let metadata = envelope.metadata().map_err(|error| error.to_string())?;
    if metadata.outbound_interface_bindings
        != authoring_result
            .outbound_interfaces()
            .cloned()
            .collect::<Vec<_>>()
        || metadata.checkpoint_binding_policy.as_ref()
            != Some(&authoring_result.binding_policy_snapshot())
        || metadata.required_verified_artifacts != authoring_result.required_verified_artifacts
    {
        return Err(
            "provider authoring failure metadata does not match its authored checkpoint"
                .to_string(),
        );
    }
    let frontier =
        frontier_checkpoint_ids(record.interface_state.get(provider_interface).ok_or_else(
            || "provider authoring failure pinned a run without provider state".to_string(),
        )?)?;
    if !frontier.contains(runtime.checkpoint.as_str())
        || !record.active_checkpoints.contains(&runtime.checkpoint)
    {
        return Err(
            "provider authoring failure arrived outside the active provider-owned frontier"
                .to_string(),
        );
    }
    let checkpoint = record.checkpoint(&runtime.checkpoint).ok_or_else(|| {
        "provider authoring failure checkpoint is absent from the pinned run".to_string()
    })?;
    if checkpoint.epoch.0 != runtime.epoch
        || !checkpoint_record_matches_template(checkpoint, authoring_result, &record.id)
        || !artifact_refs_match_bindings(
            &metadata.artifact_binding_refs,
            &checkpoint.artifact_bindings,
        )
    {
        return Err(
            "provider authoring failure checkpoint differs from its descriptor template"
                .to_string(),
        );
    }
    Ok(())
}

fn validate_authoring_failure_fields(
    failure: &InterfaceFailureEnvelope,
    movement_seq: u64,
) -> Result<(), String> {
    if failure.failure_class.trim().is_empty()
        || failure.failure_class.trim() != failure.failure_class
        || failure.source_movement_seq >= movement_seq
    {
        return Err(
            "provider authoring failure has invalid class or source movement coordinate"
                .to_string(),
        );
    }
    Ok(())
}

fn file_write_failure_result(
    result: &FileWriteResultEnvelope,
    config: &RunAuthoringConfig,
    movement_seq: u64,
) -> Result<Option<FileWriteFailureResult>, String> {
    if result.label != config.file_write_result_label || result.tool != config.file_write_tool {
        return Err(format!(
            "provider run '{}' authoring result names label='{}' tool='{}'",
            config.run.as_str(),
            result.label,
            result.tool
        ));
    }
    match (result.ok, result.error.as_ref()) {
        (true, None) => Ok(None),
        (false, Some(error)) if result.body.is_none() => {
            let correlation_id = result.correlation_id.clone().ok_or_else(|| {
                "provider failed authoring result has no correlation_id".to_string()
            })?;
            let details = error.details.as_ref().ok_or_else(|| {
                "provider failed authoring result error has no structured details".to_string()
            })?;
            let source_movement_seq = details
                .get("source_movement_seq")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| {
                    "provider failed authoring result error has no source_movement_seq".to_string()
                })?;
            let failure = InterfaceFailureEnvelope {
                source_movement_seq,
                failure_class: error.code.clone(),
                details: details.get("failure_details").cloned().ok_or_else(|| {
                    "provider failed authoring result error has no failure_details".to_string()
                })?,
            };
            validate_authoring_failure_fields(&failure, movement_seq)?;
            Ok(Some(FileWriteFailureResult {
                correlation_id,
                failure,
            }))
        }
        (false, Some(_)) => {
            Err("provider failed authoring result unexpectedly carries a body".to_string())
        }
        (false, None) => Err("provider failed authoring result has no error detail".to_string()),
        (true, Some(_)) => Err("provider successful authoring result carries an error".to_string()),
    }
}

fn active_checkpoint_for_role<'a>(
    record: &'a RunRecord,
    config: &RunAuthoringConfig,
    role: RunAuthoringCheckpointRole,
) -> Result<&'a octopus_runtime::run_record::CheckpointRecord, String> {
    let template = config.checkpoint_for_role(role)?;
    active_checkpoint_for_template(record, config, template)
}

fn active_checkpoint_for_template<'a>(
    record: &'a RunRecord,
    config: &RunAuthoringConfig,
    template: &RunAuthoringCheckpoint,
) -> Result<&'a octopus_runtime::run_record::CheckpointRecord, String> {
    let provider_state = record
        .interface_state
        .get(&config.provider_interface)
        .ok_or_else(|| {
            format!(
                "provider run '{}' has no interface_state for '{}'",
                record.id.as_str(),
                config.provider_interface.as_str()
            )
        })?;
    let frontier = frontier_checkpoint_ids(provider_state)?;
    let mut matches = frontier.iter().filter_map(|checkpoint_id| {
        let checkpoint_id = CheckpointId::new(checkpoint_id);
        record
            .active_checkpoints
            .contains(&checkpoint_id)
            .then(|| record.checkpoint(&checkpoint_id))
            .flatten()
            .filter(|checkpoint| {
                checkpoint_record_matches_template(checkpoint, template, &record.id)
            })
    });
    let checkpoint = matches.next().ok_or_else(|| {
        format!(
            "provider run '{}' frontier has no {:?} checkpoint",
            record.id.as_str(),
            template.name
        )
    })?;
    if matches.next().is_some() {
        return Err(format!(
            "provider run '{}' frontier has repeated {:?} checkpoints",
            record.id.as_str(),
            template.name
        ));
    }
    Ok(checkpoint)
}

fn validate_authoring_failure_source(
    validation: AuthoringFailureSourceValidation<'_>,
) -> Result<ValidatedAuthoringFailureSource, String> {
    let AuthoringFailureSourceValidation {
        ctx,
        record,
        record_bytes,
        config,
        failure,
        failure_seq,
        after_seq,
    } = validation;
    let hand_in_root = ctx
        .root_slot("interface_hand_in_root")?
        .join(config.authoring_source_interface.as_str());
    let attempt = persisted_authoring_attempt(
        ctx,
        &hand_in_root,
        record,
        record_bytes,
        config,
        true,
    )?
        .ok_or_else(|| {
            format!(
                "provider authoring failure movement {failure_seq} has no exact private authoring attempt"
            )
        })?;
    if after_seq.is_some_and(|source| attempt.provider_source_movement_seq != source)
        || failure.source_movement_seq <= attempt.provider_source_movement_seq
        || failure.source_movement_seq >= failure_seq
    {
        return Err(format!(
            "provider authoring failure movement {failure_seq} has invalid source movement {} for provider movement {}",
            failure.source_movement_seq, attempt.provider_source_movement_seq
        ));
    }
    Ok(ValidatedAuthoringFailureSource {
        provider_source_movement_seq: attempt.provider_source_movement_seq,
        correlation_id: attempt.authored.correlation_id,
    })
}

async fn hand_validated_authoring_failure(
    ctx: &InterfaceLiftContext,
    session: &ProviderHandoffSession,
    record: &RunRecord,
    record_bytes: &[u8],
    config: &RunAuthoringConfig,
    failure: &InterfaceFailureEnvelope,
    failure_seq: u64,
    result_correlation_id: Option<&str>,
    context_output_labels: &BTreeSet<String>,
    session_snapshot_labels: &BTreeSet<String>,
) -> Result<(), String> {
    if completed_authoring_failure_duplicate(
        ctx,
        session,
        record,
        record_bytes,
        config,
        failure,
        failure_seq,
    )? {
        return Ok(());
    }
    let validated = validate_authoring_failure_source(AuthoringFailureSourceValidation {
        ctx,
        record,
        record_bytes,
        config,
        failure,
        failure_seq,
        after_seq: None,
    })?;
    if result_correlation_id
        .is_some_and(|correlation| correlation != validated.correlation_id.as_str())
    {
        return Err("provider failed authoring result names the wrong correlation_id".to_string());
    }
    let (session_context, session_snapshot_head) = recover_source_session_context(
        ctx,
        session,
        record,
        config,
        validated.provider_source_movement_seq,
        context_output_labels,
        session_snapshot_labels,
    )?;
    hand_authoring_terminal_failure(
        ctx,
        session,
        record,
        config,
        failure_seq,
        failure,
        &session_context,
        session_snapshot_head.as_ref(),
        session_snapshot_labels,
    )
    .await?;
    Ok(())
}

fn validate_authoring_attempt_source(
    validation: AuthoringAttemptSourceValidation<'_>,
) -> Result<ValidatedAuthoringAttempt, String> {
    let AuthoringAttemptSourceValidation {
        ctx,
        record,
        record_bytes,
        config,
        call,
        manifest_path,
    } = validation;
    let authoring_result =
        config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
    let record_hash = ContentHash::of_bytes(record_bytes).to_hex();
    let current_generation = optional_u64(
        record
            .interface_state
            .get(&config.provider_interface)
            .ok_or_else(|| {
                "provider authoring failure source run has no provider state".to_string()
            })?,
        "authoring_generation",
    )?;
    let expected_generation = current_generation
        .checked_add(1)
        .ok_or_else(|| "provider authoring generation overflowed".to_string())?;
    let call_generation = correlation_generation(&record.id, &call.correlation_id)?;
    let expected_path = ctx
        .root_slot("run_records_root")?
        .canonicalize()
        .map_err(|error| format!("provider run-record root cannot be canonicalized: {error}"))?
        .join(format!("{}.json", record.id.as_str()));
    if call.tool != config.file_write_tool
        || call_generation != expected_generation
        || call.arguments.path != expected_path
        || call.arguments.expected_before_hash != record_hash
    {
        return Err(
            "provider authoring failure source call differs from the next exact file-write generation"
                .to_string(),
        );
    }
    let content = apply_run_json_patch(
        record_bytes,
        &call.arguments.patch,
        &call.arguments.expected_after_hash, call.arguments.json_format
    )?;
    let candidate = RunRecord::from_bytes(content.as_bytes()).map_err(|error| {
        format!("provider authoring failure source call carries a malformed run: {error}")
    })?;
    source_settlement::validate_transition(record, &candidate, config)?;
    source_withdrawal::validate_saved_attempt(ctx, record_bytes, content.as_bytes(), config)?;
    if candidate.id != record.id || candidate.worker != record.worker {
        return Err(
            "provider authoring failure source call carries the wrong run topology".to_string(),
        );
    }
    let candidate_generation = optional_u64(
        candidate
            .interface_state
            .get(&config.provider_interface)
            .ok_or_else(|| {
                "provider authoring failure source candidate has no provider state".to_string()
            })?,
        "authoring_generation",
    )?;
    if candidate_generation != expected_generation {
        return Err(
            "provider authoring failure source candidate has the wrong generation".to_string(),
        );
    }
    let pending = pending_handoff(&candidate, &config.provider_interface)?
        .ok_or_else(|| "provider authoring source candidate has no pending handoff".to_string())?;
    validate_pending_handoff_delivery_isolation(&candidate, &pending)?;
    if pending.items.is_empty() {
        return Err("provider authoring source candidate has invalid pending evidence".to_string());
    }
    validate_pending_items_are_active(&candidate, &config.provider_interface, &pending)?;
    validate_bound_run_authoring_topology(&candidate, config, None)?;
    let authoring_result_checkpoint = active_checkpoint_for_role(
        &candidate,
        config,
        RunAuthoringCheckpointRole::AuthoringResult,
    )?
    .id
    .clone();
    let after_hash = ContentHash::of_bytes(content.as_bytes()).to_hex();
    Ok(ValidatedAuthoringAttempt {
        manifest_path,
        provider_source_movement_seq: pending.source_movement_seq,
        authored: AuthoredRun {
            json_format: call.arguments.json_format,
            content,
            patch: call.arguments.patch,
            before_hash: record_hash,
            after_hash,
            correlation_id: call.correlation_id,
            authoring_result_checkpoint,
            authoring_result_interface: authoring_result.interface().clone(),
            pending_handoff: pending,
        },
    })
}

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

fn application_request_error(message: impl Into<String>) -> String {
    format!("application owner request: {}", message.into())
}

impl ApplicationOperatorRequest {
    fn digest(&self) -> Result<String, String> {
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
enum OwnerApprovalSubject {
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
    native_policy: Option<octopus_app_server_interface::application_consent_contract::NativeAppPolicy>,
    subject: OwnerApprovalSubject,
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
        use serde::Serialize;
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
        use serde::Deserialize;
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

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckedRunEdit {
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "octopus_file_write_contract::optional_json_format")]
    pub json_format: Option<JsonFormat>,
    pub path: PathBuf,
    pub before_hash: String,
    pub after_hash: String,
    pub correlation_id: String,
    pub content: String,
    pub patch: json_patch::Patch,
}

pub use authoring_retry::checked_edit_verification_failure;
pub use authoring_retry::checked_failed_run_write;

impl CheckedRunEdit {
    pub fn arguments(&self) -> Result<serde_json::Value, String> {
        serde_json::to_value(FileWriteCallArguments {
            path: self.path.clone(), patch: self.patch.clone(),
            expected_before_hash: self.before_hash.clone(), expected_after_hash: self.after_hash.clone(),
            json_format: self.json_format,
        }).map_err(|error| error.to_string())
    }
    pub fn prepare(
        path: PathBuf,
        current: &[u8],
        prospective: serde_json::Value,
        correlation_id: String,
    ) -> Result<Self, String> {
        Self::prepare_with_format(path, current, prospective, correlation_id, Some(JsonFormat::Compact))
    }

    pub fn prepare_with_format(
        path: PathBuf, current: &[u8], prospective: serde_json::Value,
        correlation_id: String, json_format: Option<JsonFormat>,
    ) -> Result<Self, String> {
        if !path.is_absolute() || correlation_id.is_empty() {
            return Err("checked run edit requires an absolute path and exact correlation".into());
        }
        let before = RunRecord::from_bytes(current).map_err(|e| e.to_string())?;
        let bytes = json_format.unwrap_or_default().encode(&prospective)?;
        let after = RunRecord::from_bytes(&bytes).map_err(|e| e.to_string())?;
        if before.id != after.id
            || before.ee != after.ee
            || before.worker != after.worker
            || before.trace != after.trace
        {
            return Err("checked run edit changed the original run identity".into());
        }
        let previous = serde_json::from_slice(current).map_err(|e| e.to_string())?;
        let patch = json_patch::diff(&previous, &prospective);
        let after_hash = ContentHash::of_bytes(&bytes).to_hex();
        let content = apply_run_json_patch(current, &patch, &after_hash, json_format)?;
        if content.as_bytes() != bytes {
            return Err("checked run edit did not reconstruct exact prospective bytes".into());
        }
        Ok(Self {
            json_format,
            path,
            before_hash: ContentHash::of_bytes(current).to_hex(),
            after_hash,
            correlation_id,
            content,
            patch,
        })
    }

    pub fn materialize(
        &self,
        root: &Path,
        profile: &RunAuthoringProfile,
    ) -> Result<InterfaceArtifactFile, String> {
        if profile.file_write_tool.is_empty()
            || profile.file_write_label.is_empty()
            || profile.file_write_result_label.is_empty()
            || profile.result_timeout_ms == 0
        {
            return Err("checked file-write profile is incomplete".into());
        }
        let bytes = serde_json::to_vec(&serde_json::json!({
            "tool":profile.file_write_tool,"correlation_id":self.correlation_id,
            "arguments":self.arguments()?
        })).map_err(|e| e.to_string())?;
        InterfaceArtifactFile::write(root, &profile.file_write_label, &bytes)
            .map_err(|e| e.to_string())
    }

    pub fn validate_result(
        &self,
        profile: &RunAuthoringProfile,
        movement: &Movement,
        envelope: &InterfaceInvocationEnvelope,
        call_seq: u64,
        source: &InterfaceId,
        target: &InterfaceId,
        checkpoint: &CheckpointId,
        installed: &[u8],
    ) -> Result<(), String> {
        let record = RunRecord::from_bytes(self.content.as_bytes()).map_err(|e| e.to_string())?;
        validate_unchanged_admitted_checkpoint(self.content.as_bytes(), installed, checkpoint)?;
        let expected = record
            .checkpoint(checkpoint)
            .ok_or("checked authoring result checkpoint is absent")?;
        let runtime = envelope.runtime().map_err(|e| e.to_string())?;
        let metadata = envelope.metadata().map_err(|e| e.to_string())?;
        let snapshot = CheckpointBindingPolicySnapshot {
            inbound: expected
                .interface_binding
                .clone()
                .ok_or("authoring result has no inbound binding")?,
            outbound: expected.outbound_interface_bindings.clone(),
        };
        if movement.kind != MovementKind::InterfaceOutbound
            || movement.seq.0 <= call_seq
            || movement.from != Endpoint::Interface(source.clone())
            || movement.to != Endpoint::Interface(target.clone())
            || movement.artifact.as_ref() != Some(&envelope.artifact)
            || movement
                .invocation_metadata
                .as_ref()
                .and_then(|m| m.invocation())
                != Some(metadata)
            || runtime.run != record.id
            || runtime.ee != record.ee
            || runtime.worker != record.worker
            || runtime.interface != *target
            || runtime.checkpoint != *checkpoint
            || runtime.epoch != expected.epoch.0
            || runtime.run_record_hash.as_deref() != Some(ContentHash::of_bytes(installed).to_hex().as_str())
            || metadata.checkpoint_binding_policy.as_ref() != Some(&snapshot)
            || metadata.outbound_interface_bindings
                != expected
                    .outbound_interface_bindings
                    .iter()
                    .map(|b| b.interface.clone())
                    .collect::<Vec<_>>()
            || metadata.required_verified_artifacts != expected.required_verified_artifacts
            || !artifact_refs_match_bindings(
                &metadata.artifact_binding_refs,
                &expected.artifact_bindings,
            )
            || metadata.verification_failure.is_some()
            || !metadata
                .checkpoint_admission
                .as_ref()
                .is_some_and(|a| a.complete && a.artifact_label == profile.file_write_result_label)
        {
            return Err(
                "checked file-write result differs from its exact installed prospective snapshot"
                    .into(),
            );
        }
        let captured = envelope
            .read_artifact_bounded(1024 * 1024, 8 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let result: ToolResultArtifact =
            serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())?;
        if captured.manifest.label != profile.file_write_result_label
            || result.correlation_id.as_deref() != Some(self.correlation_id.as_str())
        {
            return Err("checked file-write result has another label or correlation".into());
        }
        validate_file_write_evidence(
            profile,
            &self.path,
            result,
            &self.before_hash,
            &self.after_hash,
            self.content.len(),
        )
    }
}

fn validate_file_write_evidence(
    profile: &RunAuthoringProfile,
    path: &Path,
    result: ToolResultArtifact,
    before: &str,
    after: &str,
    bytes: usize,
) -> Result<(), String> {
    if result.label != profile.file_write_result_label
        || result.tool != profile.file_write_tool
        || !result.ok
        || result.error.is_some()
    {
        return Err("checked file.write returned a failed result or another tool".into());
    }
    let body: FileWriteResultBody =
        serde_json::from_value(result.body.ok_or("checked file.write result has no body")?)
            .map_err(|e| e.to_string())?;
    if body.result != "files_written" || body.writes.len() != 1 {
        return Err("checked file.write did not report one file".into());
    }
    let evidence = &body.writes[0];
    if evidence.path != path
        || evidence.before_hash.as_deref() != Some(before)
        || evidence.after_hash != after
        || evidence.bytes != bytes as u64
    {
        return Err("checked file.write evidence differs from its exact patch".into());
    }
    Ok(())
}

fn apply_run_json_patch(
    current: &[u8],
    patch: &json_patch::Patch,
    expected_after_hash: &str,
    json_format: Option<JsonFormat>,
) -> Result<String, String> {
    if patch.is_empty() {
        return Err("provider run authoring patch must not be empty".to_string());
    }
    let expected_after = ContentHash::from_hex(expected_after_hash).ok_or_else(|| {
        "provider run authoring patch carries an invalid expected_after_hash".to_string()
    })?;
    if expected_after.to_hex() != expected_after_hash {
        return Err(
            "provider run authoring patch expected_after_hash must be lowercase canonical hex"
                .to_string(),
        );
    }
    let mut value: serde_json::Value = serde_json::from_slice(current)
        .map_err(|error| format!("provider run authoring source is not JSON: {error}"))?;
    json_patch::patch(&mut value, patch)
        .map_err(|error| format!("provider run authoring patch cannot be applied: {error}"))?;
    let bytes = json_format.unwrap_or_default().encode(&value)?;
    let actual_after = ContentHash::of_bytes(&bytes);
    if actual_after != expected_after {
        return Err(format!(
            "provider run authoring patch produced hash {}, expected {}",
            actual_after.to_hex(),
            expected_after_hash
        ));
    }
    String::from_utf8(bytes)
        .map_err(|error| format!("provider patched run JSON is not UTF-8: {error}"))
}

fn attempt_config(record: &RunRecord, config: &RunAuthoringConfig) -> Result<RunAuthoringConfig, String> {
    config_from_persisted_profile(record, &config.provider_interface, &RunAuthoringProfile {
        file_write_tool: config.file_write_tool.clone(), file_write_label: config.file_write_label.clone(),
        file_write_result_label: config.file_write_result_label.clone(), result_timeout_ms: config.result_timeout_ms,
    })
}

// A definition write can occupy the generation of an already saved response.
// Discover only predecessors of the exact current pending source, through its
// admitted write chain; private files do not authorize an older generation.
fn revision_attempt_generations(
    ctx: &InterfaceLiftContext, record: &RunRecord, current: &[u8], config: &RunAuthoringConfig,
) -> Result<BTreeMap<u64, String>, String> {
    let mut generations = BTreeMap::new();
    if !config.checkpoints.iter().any(|checkpoint| checkpoint.role == RunAuthoringCheckpointRole::OwnerInput) {
        return Ok(generations);
    }
    let Some(pending) = persisted_pending_handoff(record, &config.provider_interface)? else { return Ok(generations); };
    let session = ProviderHandoffSession::new(ctx)?;
    let (_, source) = session.index()?.exact_provider_source_outbound(
        &record.id, &record.worker, &config.provider_interface, pending.source_movement_seq,
    )?;
    let hash = source.run_record_hash.as_deref().and_then(ContentHash::from_hex)
        .ok_or("input revision recovery source has no exact snapshot")?;
    let (before, original) = pinned_run_record(ctx, &hash, "input revision recovery source")?;
    let original_config = attempt_config(&original, config)?;
    let path = ctx.root_slot("run_records_root")?.canonicalize().map_err(|error| error.to_string())?
        .join(format!("{}.json", record.id));
    let chain = checked_selected_run_advance(ctx, &path, &before, current, pending.source_movement_seq, &original_config)?;
    for transition in chain.iter().rev().filter(|transition| transition.writer.owner == config.provider_interface) {
        if !input_revision::checked_revision(transition) { break; }
        let prior = RunRecord::from_bytes(&transition.before).map_err(|error| error.to_string())?;
        generations.insert(optional_u64(prior.interface_state.get(&config.provider_interface)
            .ok_or("revision predecessor has no provider state")?, "authoring_generation")?
            .checked_add(1).ok_or("revision predecessor generation overflow")?,
            ContentHash::of_bytes(&transition.after).to_hex());
    }
    Ok(generations)
}

fn validate_revision_attempt_predecessor(
    ctx: &InterfaceLiftContext, before: &[u8], current: &[u8], source: u64, config: &RunAuthoringConfig,
) -> Result<(), String> {
    let original = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let record = RunRecord::from_bytes(current).map_err(|error| error.to_string())?;
    if original.id != record.id || original.worker != record.worker || original.ee != record.ee || original.trace != record.trace {
        return Err("revision predecessor changed its original run identity".into());
    }
    let path = ctx.root_slot("run_records_root")?.canonicalize().map_err(|error| error.to_string())?
        .join(format!("{}.json", record.id));
    let chain = checked_selected_run_advance(ctx, &path, before, current, source, config)?;
    let provider = chain.iter().filter(|transition| transition.writer.owner == config.provider_interface).collect::<Vec<_>>();
    if provider.is_empty() || provider.iter().any(|transition| !input_revision::checked_revision(transition)) {
        return Err("saved response has no exact input-revision predecessor proof".into());
    }
    input_revision::revision_delta(before, current, config)?
        .ok_or("saved response revision changed its pending work or context")?;
    Ok(())
}

fn persisted_authoring_attempt(
    ctx: &InterfaceLiftContext,
    hand_in_root: &Path,
    record: &RunRecord,
    record_bytes: &[u8],
    config: &RunAuthoringConfig,
    recover_predecessors: bool,
) -> Result<Option<ValidatedAuthoringAttempt>, String> {
    let current_generation = optional_u64(
        record
            .interface_state
            .get(&config.provider_interface)
            .ok_or("provider authoring recovery run has no provider state")?,
        "authoring_generation",
    )?;
    let expected_generation = current_generation
        .checked_add(1)
        .ok_or("provider authoring generation overflowed")?;
    let current_hash = ContentHash::of_bytes(record_bytes).to_hex();
    let calls = private_authoring_calls(ctx, hand_in_root, config)?;
    if calls.is_empty() {
        return Ok(None);
    }
    let predecessors = if recover_predecessors {
        revision_attempt_generations(ctx, record, record_bytes, config)?
    } else { BTreeMap::new() };
    let mut attempts = Vec::new();
    for (path, call) in calls {
        let prefix = format!("provider-run-authoring:{}:", record.id);
        if !call.correlation_id.starts_with(&prefix) { continue; }
        let generation = correlation_generation(&record.id, &call.correlation_id)?;
        if generation != expected_generation && !predecessors.contains_key(&generation) { continue; }
        // The admitted definition write itself is not an unfinished response.
        if predecessors.get(&generation) == Some(&call.arguments.expected_after_hash) { continue; }
        let (before_bytes, before_record) = if call.arguments.expected_before_hash == current_hash {
            (record_bytes.to_vec(), record.clone())
        } else {
            let hash = ContentHash::from_hex(&call.arguments.expected_before_hash)
                .ok_or("provider authoring attempt has no canonical preimage")?;
            let before = pinned_run_record(ctx, &hash, "provider authoring attempt")?;
            if before.1.id != record.id
                || before.1.worker != record.worker
                || before.1.ee != record.ee
                || before.1.trace != record.trace
                || (generation == expected_generation && before.1.interface_state.get(&config.provider_interface)
                    != record.interface_state.get(&config.provider_interface))
            {
                return Err(
                    "provider authoring attempt changed its run or provider-owned state".into(),
                );
            }
            before
        };
        let original_config = attempt_config(&before_record, config)?;
        let attempt = validate_authoring_attempt_source(AuthoringAttemptSourceValidation {
            ctx,
            record: &before_record,
            record_bytes: &before_bytes,
            config: &original_config,
            call,
            manifest_path: path,
        })?;
        if generation != expected_generation {
            validate_revision_attempt_predecessor(ctx, &before_bytes, record_bytes,
                attempt.provider_source_movement_seq, &original_config)?;
        }
        attempts.push((attempt, before_bytes));
    }
    if attempts.len() <= 1 {
        return Ok(attempts.pop().map(|(attempt, _)| attempt));
    }
    let mut ordered = attempts
        .into_iter()
        .map(|(attempt, before)| {
            let rejected = authoring_retry::rejected_write(ctx, config, &attempt.authored)?;
            Ok((attempt, before, rejected))
        })
        .collect::<Result<Vec<_>, String>>()?;
    if ordered
        .iter()
        .filter(|(_, _, rejection)| rejection.is_none())
        .count()
        > 1
    {
        return Err("provider authoring has competing unaccounted generation attempts".into());
    }
    ordered.sort_by_key(|(_, _, rejected)| {
        rejected
            .as_ref()
            .map_or(u64::MAX, |rejected| rejected.result_seq)
    });
    for pair in ordered.windows(2) {
        let (previous, before, rejection) = &pair[0];
        let (next, current, _) = &pair[1];
        if rejection.is_none() {
            return Err("provider authoring retry precedes its actual rejection".into());
        }
        let path = ctx
            .root_slot("run_records_root")?
            .canonicalize()
            .map_err(|error| error.to_string())?
            .join(format!("{}.json", record.id));
        let chain = checked_selected_run_advance(
            ctx,
            &path,
            before,
            current,
            previous.provider_source_movement_seq,
            config,
        )?;
        if chain.is_empty()
            || chain
                .iter()
                .any(|transition| transition.writer.owner == config.provider_interface && !input_revision::checked_revision(transition))
        {
            return Err("provider authoring retry changed its original intent or independent write evidence".into());
        }
        authoring_retry::validate_rebased_authored_run(before, current, &previous.authored, &next.authored, config)?;
    }
    Ok(ordered.pop().map(|(attempt, _, _)| attempt))
}

fn private_authoring_calls(
    ctx: &InterfaceLiftContext,
    hand_in_root: &Path,
    config: &RunAuthoringConfig,
) -> Result<Vec<(PathBuf, FileWriteCallEnvelope)>, String> {
    let entries = match std::fs::read_dir(hand_in_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "provider authoring private outbox '{}' cannot be read: {error}",
                hand_in_root.display()
            ));
        }
    };
    let mut paths = entries
        .map(|entry| {
            entry.map(|entry| entry.path()).map_err(|error| {
                format!(
                    "provider authoring private outbox '{}' contains an unreadable entry: {error}",
                    hand_in_root.display()
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    let limits = ctx.required_host_io_limits()?;
    let mut calls = Vec::new();
    for path in paths {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return Err(format!(
                "provider authoring private outbox '{}' contains a non-UTF8 entry",
                hand_in_root.display()
            ));
        };
        if !name.ends_with(".artifact.json") {
            continue;
        }
        let artifact = InterfaceArtifactFile::new(path.clone()).map_err(|error| {
            format!(
                "provider authoring private artifact '{}' is invalid: {error}",
                path.display()
            )
        })?;
        let snapshot = artifact
            .read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
            .map_err(|error| {
                format!(
                    "provider authoring private artifact '{}' cannot be read: {error}",
                    path.display()
                )
            })?;
        if snapshot.manifest.label != config.file_write_label {
            continue;
        }
        let expected_prefix = format!("provider-run-authoring:{}:", config.run.as_str());
        let Some(call) = decode_owned_run_authoring_call(&snapshot.payload_bytes, &expected_prefix)
            .map_err(|error| {
                format!(
                    "provider authoring private call '{}' is malformed: {error}",
                    path.display()
                )
            })?
        else {
            continue;
        };
        calls.push((path, call));
    }
    Ok(calls)
}

fn decode_owned_run_authoring_call(
    payload: &[u8],
    expected_prefix: &str,
) -> Result<Option<FileWriteCallEnvelope>, serde_json::Error> {
    let value: serde_json::Value = match serde_json::from_slice(payload) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    if !value
        .get("correlation_id")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|correlation_id| correlation_id.starts_with(expected_prefix))
    {
        return Ok(None);
    }
    serde_json::from_value(value).map(Some)
}

fn authoring_success_evidence(
    ctx: &InterfaceLiftContext,
    session: &ProviderHandoffSession,
    config: &RunAuthoringConfig,
    authored: &AuthoredRun,
    after_seq: Option<u64>,
) -> Result<Option<u64>, String> {
    if config.writer_run.is_some() { return writer_run::success_evidence(ctx, session, config, authored, after_seq); }
    let candidate = RunRecord::from_bytes(authored.content.as_bytes()).map_err(|error| {
        format!(
            "provider recovered authoring candidate for run '{}' is malformed: {error}",
            config.run.as_str()
        )
    })?;
    let checkpoint = active_checkpoint_for_role(
        &candidate,
        config,
        RunAuthoringCheckpointRole::AuthoringResult,
    )?;
    if checkpoint.id != authored.authoring_result_checkpoint {
        return Err(format!(
            "provider recovered authoring candidate for run '{}' changed its result checkpoint",
            config.run.as_str()
        ));
    }
    let coordinate = RuntimeCoordinateKey::new(
        &candidate.id,
        &candidate.worker,
        &config.provider_interface,
        &checkpoint.id,
        checkpoint.epoch.0,
    );
    let Some(evidence) = session.index()?.unique_outbound_artifact(
        &coordinate,
        &authored.authoring_result_interface,
        after_seq,
    )?
    else {
        return Ok(None);
    };
    let expected = config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
    if !indexed_evidence_matches_checkpoint(&evidence, expected)
        || !artifact_refs_match_bindings(
            &evidence.artifact_binding_refs,
            &checkpoint.artifact_bindings,
        )
    {
        return Err(format!(
            "provider recovered authoring result for run '{}' differs from the authored checkpoint",
            config.run.as_str()
        ));
    }
    if evidence.run_record_hash.as_deref() != Some(&authored.after_hash) {
        if !config.checkpoints.iter().any(|cp| cp.owner_authoring.is_some()) {
            return Err("provider authoring result changed its prospective snapshot".into());
        }
        let hash = evidence.run_record_hash.as_deref().and_then(ContentHash::from_hex)
            .ok_or("provider authoring result has no pinned snapshot")?;
        let (pinned, _) = pinned_run_record(ctx, &hash, "provider authoring result")?;
        validate_unchanged_admitted_checkpoint(authored.content.as_bytes(), &pinned, &checkpoint.id)?;
    }
    let (manifest, payload) = read_pinned_artifact(ctx, &evidence.artifact)?;
    if manifest.label != config.file_write_result_label {
        return Err(format!(
            "provider recovered authoring result checkpoint for run '{}' delivered unexpected artifact '{}'",
            config.run.as_str(),
            manifest.label
        ));
    }
    let result: FileWriteResultEnvelope = serde_json::from_slice(&payload).map_err(|error| {
        format!(
            "provider recovered authoring result for run '{}' is malformed: {error}",
            config.run.as_str()
        )
    })?;
    if result.correlation_id.as_deref() != Some(&authored.correlation_id) {
        return Err(format!(
            "provider recovered authoring result for run '{}' names the wrong correlation",
            config.run.as_str()
        ));
    }
    let run_path = ctx
        .root_slot("run_records_root")?
        .canonicalize()
        .map_err(|error| format!("provider run-record root cannot be canonicalized: {error}"))?
        .join(format!("{}.json", config.run.as_str()));
    validate_write_result_evidence(config, &run_path, result, authored)?;
    Ok(Some(evidence.seq))
}

fn completed_authoring_failure_duplicate(
    ctx: &InterfaceLiftContext,
    session: &ProviderHandoffSession,
    record: &RunRecord,
    record_bytes: &[u8],
    config: &RunAuthoringConfig,
    failure: &InterfaceFailureEnvelope,
    failure_seq: u64,
) -> Result<bool, String> {
    let authoring_result =
        config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
    let hand_in_root = ctx
        .root_slot("interface_hand_in_root")?
        .join(config.authoring_source_interface.as_str());
    let expected_path = ctx
        .root_slot("run_records_root")?
        .canonicalize()
        .map_err(|error| format!("provider run-record root cannot be canonicalized: {error}"))?
        .join(format!("{}.json", record.id.as_str()));
    let current_generation = optional_u64(
        record
            .interface_state
            .get(&config.provider_interface)
            .ok_or_else(|| "provider completed authoring run has no provider state".to_string())?,
        "authoring_generation",
    )?;
    let current_hash = ContentHash::of_bytes(record_bytes).to_hex();
    let mut matched = None;
    for (_, call) in private_authoring_calls(ctx, &hand_in_root, config)? {
        let prefix = format!("provider-run-authoring:{}:", record.id.as_str());
        if !call.correlation_id.starts_with(&prefix)
            || correlation_generation(&record.id, &call.correlation_id)? != current_generation
            || call.tool != config.file_write_tool
            || call.arguments.path != expected_path
            || call.arguments.expected_after_hash != current_hash
        {
            continue;
        }
        let canonical_before =
            ContentHash::from_hex(&call.arguments.expected_before_hash).map(|hash| hash.to_hex());
        if call.arguments.patch.is_empty()
            || canonical_before.as_deref() != Some(call.arguments.expected_before_hash.as_str())
        {
            return Err(
                "provider completed authoring attempt carries malformed patch evidence".to_string(),
            );
        }
        let pending = pending_handoff(record, &config.provider_interface)?
            .ok_or_else(|| "provider completed authoring run has no pending handoff".to_string())?;
        if pending.items.is_empty()
            || pending.source_movement_seq >= failure.source_movement_seq
            || failure.source_movement_seq >= failure_seq
        {
            continue;
        }
        validate_pending_items_are_active(record, &config.provider_interface, &pending)?;
        validate_bound_run_authoring_topology(record, config, None)?;
        let authoring_result_checkpoint = active_checkpoint_for_role(
            record,
            config,
            RunAuthoringCheckpointRole::AuthoringResult,
        )?
        .id
        .clone();
        let authored = AuthoredRun {
            json_format: call.arguments.json_format,
            after_hash: current_hash.clone(),
            before_hash: call.arguments.expected_before_hash,
            content: String::from_utf8(record_bytes.to_vec()).map_err(|error| {
                format!("provider completed authored run is not UTF-8: {error}")
            })?,
            patch: call.arguments.patch,
            correlation_id: call.correlation_id,
            authoring_result_checkpoint,
            authoring_result_interface: authoring_result.interface().clone(),
            pending_handoff: pending,
        };
        if matched.replace(authored).is_some() {
            return Err(format!(
                "provider completed authoring run '{}' has repeated private attempts",
                record.id.as_str()
            ));
        }
    }
    let Some(authored) = matched else {
        return Ok(false);
    };
    let Some(result_seq) = authoring_success_evidence(ctx, session, config, &authored, None)?
    else {
        return Ok(false);
    };
    if result_seq >= failure_seq {
        return Err(format!(
            "provider late authoring failure movement {failure_seq} precedes its completed result {result_seq}"
        ));
    }
    Ok(true)
}

fn recover_source_session_context(
    ctx: &InterfaceLiftContext,
    session: &ProviderHandoffSession,
    record: &RunRecord,
    config: &RunAuthoringConfig,
    source_movement_seq: u64,
    context_output_labels: &BTreeSet<String>,
    session_snapshot_labels: &BTreeSet<String>,
) -> Result<(ResolvedSessionContext, Option<SessionSnapshotHead>), String> {
    let (context, head, _) = recover_source_session_context_from_cache(
        &ctx.blobs_dir, ctx.required_host_io_limits()?, session, record, config, source_movement_seq, context_output_labels, session_snapshot_labels,
    )?;
    Ok((context, head))
}

fn recover_source_session_context_from_cache(
    blobs_dir: &Path,
    limits: InterfaceHostIoLimits,
    session: &ProviderHandoffSession,
    record: &RunRecord,
    config: &RunAuthoringConfig,
    source_movement_seq: u64,
    context_output_labels: &BTreeSet<String>,
    session_snapshot_labels: &BTreeSet<String>,
) -> Result<(ResolvedSessionContext, Option<SessionSnapshotHead>, Vec<ArtifactId>), String> {
    recover_source_session_context_with_static_correction(
        blobs_dir, limits, session, record, config, source_movement_seq,
        context_output_labels, session_snapshot_labels, None,
    )
}

fn recover_source_session_context_with_static_correction(
    blobs_dir: &Path,
    limits: InterfaceHostIoLimits,
    session: &ProviderHandoffSession,
    record: &RunRecord,
    config: &RunAuthoringConfig,
    source_movement_seq: u64,
    context_output_labels: &BTreeSet<String>,
    session_snapshot_labels: &BTreeSet<String>,
    correction: Option<&input_revision::FailedSourceStaticContext>,
) -> Result<(ResolvedSessionContext, Option<SessionSnapshotHead>, Vec<ArtifactId>), String> {
    let (coordinate, source) = session.index()?.exact_provider_source_outbound(
        &record.id,
        &record.worker,
        &config.provider_interface,
        source_movement_seq,
    )?;
    let hash = source.run_record_hash.as_deref().and_then(ContentHash::from_hex).ok_or("source context has no pinned run record")?;
    let (_, pinned) = pinned_run_record_from_cache(blobs_dir, limits, &hash, "provider source context")?;
    if pinned.id != record.id || pinned.worker != record.worker {
        return Err(format!(
            "provider source movement {source_movement_seq} pinned a different run identity"
        ));
    }
    let checkpoint_id = CheckpointId::new(&coordinate.checkpoint);
    let checkpoint = pinned.checkpoint(&checkpoint_id).ok_or_else(|| {
        format!(
            "provider source movement {source_movement_seq} names checkpoint '{}' absent from its pinned run record",
            coordinate.checkpoint
        )
    })?;
    if !pinned.active_checkpoints.contains(&checkpoint_id) || checkpoint.epoch.0 != coordinate.epoch
    {
        return Err(format!(
            "provider source movement {source_movement_seq} does not name an active pinned checkpoint epoch"
        ));
    }
    let source_config = config_from_persisted_profile(&pinned, &config.provider_interface, &RunAuthoringProfile {
        file_write_tool: config.file_write_tool.clone(), file_write_label: config.file_write_label.clone(),
        file_write_result_label: config.file_write_result_label.clone(), result_timeout_ms: config.result_timeout_ms,
    })?;
    let matched = source_config
        .checkpoints
        .iter()
        .filter(|template| {
            template.role.is_provider_input()
                && template.include_context
                && source.source == *template.interface()
                && checkpoint_record_matches_template(checkpoint, template, &pinned.id)
        })
        .collect::<Vec<_>>();
    if matched.len() != 1 {
        return Err(format!(
            "provider source movement {source_movement_seq} matches {} context-bearing input checkpoints", matched.len()
        ));
    }
    if !checkpoint
        .outbound_interfaces()
        .eq(source.outbound_interface_bindings.iter())
        || source.checkpoint_binding_policy.as_ref()
            != Some(&CheckpointBindingPolicySnapshot {
                inbound: checkpoint.interface_binding.clone().ok_or_else(|| {
                    format!(
                        "provider source movement {source_movement_seq} pinned checkpoint has no inbound binding"
                    )
                })?,
                outbound: checkpoint.outbound_interface_bindings.clone(),
            })
        || source.required_verified_artifacts != checkpoint.required_verified_artifacts
        || checkpoint
            .artifact_bindings
            .iter()
            .map(|binding| &binding.id)
            .ne(source
                .artifact_binding_refs
                .iter()
                .map(|artifact| &artifact.id))
    {
        return Err(format!(
            "provider source movement {source_movement_seq} differs from its pinned run checkpoint"
        ));
    }
    let mut static_context_artifacts = Vec::new();
    let mut session_context = ResolvedSessionContext {
        provider_interface: config.provider_interface.clone(),
        artifacts: Vec::new(),
    };
    let mut session_snapshot_head = None;
    let bound_provenance = session.index()?.bound_provenance(
        &record.id,
        &config.provider_interface,
        source.artifact_binding_refs.iter(),
    )?;
    let (payloads, mut classified_static) = source_context_payloads_from_cache(blobs_dir, limits, &pinned,
        &source_config, checkpoint, matched[0], &source)?;
    if let Some(correction) = correction {
        classified_static.extend(correction.refs_for(&coordinate, &source, &hash, checkpoint)?
            .iter().map(|reference| reference.id.clone()));
    }
    for bound in &payloads {
        let artifact = &bound.artifact;
        let payload = bound.read_bytes().map_err(|error| error.to_string())?;
        if classified_static.contains(&artifact.id) {
            append_artifact(&mut static_context_artifacts, artifact.id.clone());
            continue;
        }
        collect_bound_context_from_cache(
            blobs_dir, limits,
            &mut static_context_artifacts,
            &mut session_context,
            &mut session_snapshot_head,
            artifact,
            &payload,
            bound_provenance
                .get(&artifact_ref_key(artifact))
                .cloned()
                .unwrap_or_default(),
            session_snapshot_labels,
            &config.provider_interface,
        )?;
    }
    validate_session_context_evidence(
        &mut session_context,
        SessionContextEvidenceScope {
            blobs_dir,
            limits,
            run: &record.id,
            provider: &config.provider_interface,
            context_output_labels,
            session_snapshot_labels,
        },
        session,
    )?;
    let (manifest, payload) = read_pinned_artifact_from_cache(blobs_dir, limits, &source.artifact)?;
    append_session_payload(
        &mut session_context,
        &manifest.label,
        &payload,
        &source.artifact,
        Some(source_movement_seq),
        "provider source movement",
    )?;
    ground_session_context_movements(&mut session_context, session, &record.id, config)?;
    validate_resolved_session_context(
        &session_context,
        "provider authoring failure source",
        &config.provider_interface,
    )?;
    reconcile_resolved_session_snapshot_head(&session_context, &mut session_snapshot_head)?;
    Ok((session_context, session_snapshot_head, static_context_artifacts))
}

fn source_context_payloads_from_cache(
    blobs_dir: &Path, limits: InterfaceHostIoLimits, pinned: &RunRecord, config: &RunAuthoringConfig,
    checkpoint: &octopus_runtime::run_record::CheckpointRecord, template: &RunAuthoringCheckpoint, source: &IndexedOutboundArtifact,
) -> Result<(Vec<octopus_interfaces::ArtifactBindingPayload>, BTreeSet<ArtifactId>), String> {
    let mut selected = payload_artifact_bindings(pinned, &config.provider_interface)?;
    for reference in input_artifacts::bound_sources(pinned, &config.provider_interface, &checkpoint.id, &source.artifact_binding_refs)? {
        if !selected.contains(&reference) { selected.push(reference); }
    }
    validate_payload_read_bindings(&selected, checkpoint, &source.artifact_binding_refs, &source.readable_artifacts, &source.role_artifacts)?;
    let payloads = source.artifact_binding_refs.iter().map(|artifact| {
        Ok(octopus_interfaces::ArtifactBindingPayload::new(artifact.clone(), read_artifact_ref_payload_from_cache(blobs_dir, limits, artifact)?))
    }).collect::<Result<Vec<_>, String>>()?;
    let mut static_ids = bound_instruction_sources(&payloads, &selected)?;
    static_ids.extend(selected.iter().map(|reference| reference.id.clone()));
    static_ids.extend(template.static_artifact_bindings.iter().cloned());
    Ok((payloads, static_ids))
}

fn ground_session_context_movements(
    session_context: &mut ResolvedSessionContext,
    session: &ProviderHandoffSession,
    run: &RunId,
    config: &RunAuthoringConfig,
) -> Result<(), String> {
    if session_context.artifacts.is_empty() {
        return Ok(());
    }
    let refs = session_context
        .artifacts
        .iter()
        .map(|artifact| artifact.source_artifact.clone())
        .collect::<Vec<_>>();
    let provenance =
        session
            .index()?
            .bound_provenance(run, &config.provider_interface, refs.iter())?;
    for artifact in session_context.artifacts.iter_mut() {
        if artifact.source_movement_seq.is_some() {
            continue;
        }
        let grounded = provenance
            .get(&artifact_ref_key(&artifact.source_artifact))
            .and_then(|found| found.provider_arrival_seq.or(found.movement_seq));
        artifact.source_movement_seq =
            Some(grounded.ok_or_else(|| {
                format!(
                    "session context artifact '{}' has no movement in run '{}'",
                    artifact.source_artifact.id.as_str(),
                    run.as_str()
                )
            })?);
    }
    Ok(())
}

fn materialize_authoring_turn_error(
    ctx: &InterfaceLiftContext,
    authoring_interface: &InterfaceId,
    failure: &InterfaceFailureEnvelope,
) -> Result<InterfaceArtifactFile, String> {
    let detail = failure
        .details
        .get("error")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(failure.failure_class.as_str());
    let payload = serde_json::to_vec(&serde_json::json!({
        "label": "turn.error",
        "class": "run_authoring_failed",
        "code": failure.failure_class,
        "message": format!("run authoring failed: {detail}"),
        "will_retry": false,
        "evidence": {
            "authoring_interface": authoring_interface,
            "source_movement_seq": failure.source_movement_seq,
            "failure_details": failure.details
        }
    }))
    .map_err(|error| format!("provider authoring turn.error did not encode: {error}"))?;
    InterfaceArtifactFile::write(
        ctx.interface_var_dir().join("emitted"),
        "turn.error",
        &payload,
    )
    .map_err(|error| format!("provider authoring turn.error did not materialize: {error}"))
}

async fn hand_authoring_terminal_failure(
    ctx: &InterfaceLiftContext,
    session: &ProviderHandoffSession,
    record: &RunRecord,
    config: &RunAuthoringConfig,
    failure_seq: u64,
    failure: &InterfaceFailureEnvelope,
    session_context: &ResolvedSessionContext,
    session_snapshot_head: Option<&SessionSnapshotHead>,
    session_snapshot_labels: &BTreeSet<String>,
) -> Result<ProviderHandoffOutcome, String> {
    let authoring_result =
        config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
    let output = config.output_checkpoint(&ProviderHandoffRoute::ClientProjection,
        &BTreeSet::new(), &BTreeSet::new(), "turn.error")?;
    let client = active_checkpoint_for_template(record, config, &output)?;
    let mut artifact =
        materialize_authoring_turn_error(ctx, authoring_result.interface(), failure)?;
    let manifest = artifact.read_manifest().map_err(|error| {
        format!("provider authoring terminal error manifest cannot be read: {error}")
    })?;
    if session_snapshot_labels.contains(&manifest.label) {
        artifact = materialize_session_snapshot(&artifact, session_context, session_snapshot_head)?;
    }
    let expected = artifact_identity(&artifact, &ctx.interface_var_dir())?.artifact;
    let coordinate = PendingHandoffLedgerCoordinate {
        interface: &ctx.interface,
        run: &record.id,
        worker: &record.worker,
        artifact: &expected,
        required_evidence: None,
        checkpoint: &client.id,
        epoch: client.epoch.0,
    };
    if let Some(client_seq) = session.index()?.admitted_handoff_seq(coordinate)? {
        if client_seq <= failure_seq {
            return Err(
                "provider authoring terminal error does not follow its failure movement"
                    .to_string(),
            );
        }
        return Ok(ProviderHandoffOutcome::TerminalFailure {
            failure_seq,
            client_seq,
            failure_class: failure.failure_class.clone(),
        });
    }
    let verdict = ctx.hand_arrival(artifact, Vec::new()).await?;
    let client_seq = session.index()?.admitted_handoff_seq(coordinate)?;
    match (verdict, client_seq) {
        (DeliveryVerdict::Admitted { .. }, Some(client_seq))
        | (DeliveryVerdict::Refused { .. }, Some(client_seq))
            if client_seq > failure_seq =>
        {
            Ok(ProviderHandoffOutcome::TerminalFailure {
                failure_seq,
                client_seq,
                failure_class: failure.failure_class.clone(),
            })
        }
        (DeliveryVerdict::Admitted { .. }, Some(_))
        | (DeliveryVerdict::Refused { .. }, Some(_)) => Err(
            "provider authoring terminal error does not follow its failure movement".to_string(),
        ),
        (DeliveryVerdict::Admitted { .. }, None) => Err(
            "provider authoring terminal error was admitted without exact client checkpoint evidence"
                .to_string(),
        ),
        (DeliveryVerdict::Refused { reason, detail }, None) => Err(format!(
            "provider authoring terminal error was refused as '{}': {}",
            reason.as_str(),
            detail
        )),
    }
}

enum BoundedLedgerRow {
    Complete(Vec<u8>),
    Partial,
    Eof,
}

async fn await_checked_evidence<T>(
    ctx: &InterfaceLiftContext,
    timeout_ms: u64,
    mut validate: impl FnMut() -> Result<T, CheckedRunError>,
) -> Result<T, CheckedRunError> {
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(timeout_ms))
        .ok_or("checked input evidence deadline overflow")?;
    let mut watcher = None;
    loop {
        if ctx.cancel.is_cancelled() {
            return Err("checked input preparation was cancelled".into());
        }
        match validate() {
            Ok(value) => return Ok(value),
            Err(error @ CheckedRunError::Invalid(_)) => return Err(error),
            Err(changed @ CheckedRunError::ChangedDuringRead { .. }) => {
                if Instant::now() >= deadline {
                    return Err(changed);
                }
                tokio::task::yield_now().await;
            }
            Err(waiting @ CheckedRunError::AwaitingResults { .. }) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(waiting);
                }
                if watcher.is_none() {
                    watcher = Some(LedgerTailReader::open_named(ctx, "checked-input-evidence")?);
                }
                tokio::time::timeout(
                    remaining,
                    watcher.as_mut().unwrap().wait_for_change(&ctx.cancel),
                )
                .await
                .map_err(|_| waiting)??;
            }
        }
    }
}

impl ProviderHandoffSession {
    pub fn new(ctx: &InterfaceLiftContext) -> Result<Self, String> {
        let limits = ctx.required_host_io_limits()?;
        let movements_path = ctx.movements_path.canonicalize().map_err(|error| {
            format!(
                "provider ledger '{}' cannot be canonicalized: {error}",
                ctx.movements_path.display()
            )
        })?;
        Ok(Self {
            interface: ctx.interface.clone(),
            movements_path: movements_path.clone(),
            writer_history: Arc::new(Mutex::new(BTreeMap::new())),
            writer_call_generations: Arc::new(Mutex::new(writer_run::WriterCallIndex::default())),
            evidence: Arc::new(Mutex::new(ProviderLedgerEvidenceIndex::with_spotlight(
                ctx.interface.clone(),
                movements_path,
                limits.max_control_frame_bytes.max(1024),
                &ctx.ledger_index_path,
            ))),
        })
    }

    pub fn from_invocation(
        &self,
        ctx: &InterfaceLiftContext,
        descriptor: &InterfaceDescriptor,
        run: &RunId,
        input: &InterfaceInvocationEnvelope,
        source_movement_seq: u64,
        pinned_run_record: &[u8],
    ) -> Result<ProviderOutputHandoff, String> {
        self.from_invocation_state(ctx, descriptor, run, input, source_movement_seq, pinned_run_record).map_err(Into::into)
    }

    pub async fn from_invocation_when_ready(
        &self, ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, run: &RunId,
        input: &InterfaceInvocationEnvelope, source_movement_seq: u64, pinned_run_record: &[u8],
    ) -> Result<ProviderOutputHandoff, CheckedRunError> {
        let profile = run_authoring_profile(descriptor)?;
        await_checked_evidence(ctx, profile.result_timeout_ms, || {
            self.from_invocation_state(ctx, descriptor, run, input, source_movement_seq, pinned_run_record)
        }).await
    }

    fn from_invocation_state(
        &self, ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, run: &RunId,
        input: &InterfaceInvocationEnvelope, source_movement_seq: u64, pinned_run_record: &[u8],
    ) -> Result<ProviderOutputHandoff, CheckedRunError> {
        let movements_path = ctx.movements_path.canonicalize().map_err(|error| {
            format!(
                "provider ledger '{}' cannot be canonicalized: {error}",
                ctx.movements_path.display()
            )
        })?;
        if self.interface != ctx.interface || self.movements_path != movements_path {
            return Err(format!(
                "provider handoff session belongs to interface '{}' ledger '{}', invocation uses interface '{}' ledger '{}'",
                self.interface.as_str(),
                self.movements_path.display(),
                ctx.interface.as_str(),
                movements_path.display()
            ).into());
        }
        ProviderOutputHandoff::from_invocation_with_session(
            self.clone(),
            ctx,
            descriptor,
            run,
            input,
            source_movement_seq,
            pinned_run_record,
        )
    }

    fn index(&self) -> Result<std::sync::MutexGuard<'_, ProviderLedgerEvidenceIndex>, String> {
        self.evidence
            .lock()
            .map_err(|_| "provider handoff ledger index lock is poisoned".to_string())
    }
}

impl ProviderLedgerEvidenceIndex {
    fn new(interface: InterfaceId, movements_path: PathBuf, max_row_bytes: u64) -> Self {
        Self {
            interface,
            decoder: DurableMovementDecoder::for_log(&movements_path),
            movements_path,
            max_row_bytes,
            spotlight: None,
            byte_offset: 0,
            observed_len: 0,
            next_seq: 0,
            through_seq: None,
            poison: None,
            handoff_metadata_poison: HashMap::new(),
            session_metadata_poison: HashMap::new(),
            context_metadata_poison: HashMap::new(),
            latest_run_movement: HashMap::new(),
            provider_run_arrivals: HashMap::new(),
            session_run_movements: HashMap::new(),
            session_provider_arrivals: HashMap::new(),
            session_provider_inputs: HashMap::new(),
            coordinate_arrivals: HashMap::new(),
            mark_arrival_coordinates: HashMap::new(),
            coordinate_artifacts: HashMap::new(),
            coordinate_outbounds: HashMap::new(),
            coordinate_marks: HashMap::new(),
        }
    }

    fn with_spotlight(
        interface: InterfaceId,
        movements_path: PathBuf,
        max_row_bytes: u64,
        ledger_index_path: &Path,
    ) -> Self {
        let mut index = Self::new(interface, movements_path, max_row_bytes);
        index.spotlight = LedgerSqliteReader::open(ledger_index_path).ok();
        index
    }

    fn catch_up(&mut self) -> Result<(), String> {
        if let Some(error) = &self.poison {
            return Err(error.clone());
        }
        if let Err(error) = self.catch_up_unpoisoned() {
            self.poison = Some(error.clone());
            return Err(error);
        }
        Ok(())
    }

    fn catch_up_unpoisoned(&mut self) -> Result<(), String> {
        let length = std::fs::metadata(&self.movements_path)
            .map_err(|error| {
                format!(
                    "provider ledger index cannot stat '{}': {error}",
                    self.movements_path.display()
                )
            })?
            .len();
        let watermark = self.byte_offset.max(self.observed_len);
        if length < watermark {
            return Err(format!(
                "provider ledger '{}' shrank behind observed watermark {} to {}",
                self.movements_path.display(),
                watermark,
                length
            ));
        }
        self.observed_len = length;
        if length == self.byte_offset {
            return Ok(());
        }
        if self.through_seq.is_none() && self.catch_up_spotlight_prefix(length)? && length == self.byte_offset {
            return Ok(());
        }
        self.catch_up_full_tail()
    }

    fn catch_up_spotlight_prefix(&mut self, length: u64) -> Result<bool, String> {
        let Some(spotlight) = &self.spotlight else {
            return Ok(false);
        };
        let progress = match spotlight.progress() {
            Ok(progress) => progress,
            Err(_) => {
                self.spotlight = None;
                return Ok(false);
            }
        };
        if progress.watermark <= self.next_seq {
            return Ok(false);
        }
        if progress.durable_end_offset < self.byte_offset || progress.durable_end_offset > length {
            self.spotlight = None;
            return Ok(false);
        }
        let indexed_rows = match spotlight.movement_rows_for_runtime_interface(
            self.interface.as_str(),
            self.next_seq,
            progress.watermark,
        ) {
            Ok(Some(rows)) => rows,
            Ok(None) => return Ok(false),
            Err(_) => {
                self.spotlight = None;
                return Ok(false);
            }
        };
        let Some(movements) = self.read_spotlight_rows(
            &indexed_rows,
            progress.watermark,
            progress.durable_end_offset,
        ) else {
            return Ok(false);
        };
        for movement in &movements {
            self.note(movement)?;
        }
        self.byte_offset = progress.durable_end_offset;
        self.observed_len = self.observed_len.max(progress.durable_end_offset);
        self.next_seq = progress.watermark;
        Ok(true)
    }

    fn read_spotlight_rows(
        &self,
        rows: &[octopus_ledger_index::IndexedMovementRowRange],
        watermark: u64,
        durable_end_offset: u64,
    ) -> Option<Vec<Movement>> {
        if rows.is_empty() {
            return Some(Vec::new());
        }
        let mut file = std::fs::File::open(&self.movements_path).ok()?;
        let mut movements = Vec::with_capacity(rows.len());
        let mut previous_seq = None;
        let mut previous_end = self.byte_offset;
        for indexed in rows {
            if indexed.seq < self.next_seq
                || indexed.seq >= watermark
                || indexed.start_offset < self.byte_offset
                || indexed.end_offset > durable_end_offset
                || indexed.end_offset <= indexed.start_offset
                || previous_seq.is_some_and(|seq| indexed.seq <= seq)
                || indexed.start_offset < previous_end
            {
                return None;
            }
            let row_len = indexed.end_offset.checked_sub(indexed.start_offset)?;
            if row_len > self.max_row_bytes {
                return None;
            }
            let row_len = usize::try_from(row_len).ok()?;
            file.seek(std::io::SeekFrom::Start(indexed.start_offset))
                .ok()?;
            let mut row = vec![0_u8; row_len];
            file.read_exact(&mut row).ok()?;
            if row.last() != Some(&b'\n') {
                return None;
            }
            let movement = self.decoder.decode(&row, indexed.seq as usize).ok()?;
            if movement.seq.0 != indexed.seq {
                return None;
            }
            previous_seq = Some(indexed.seq);
            previous_end = indexed.end_offset;
            movements.push(movement);
        }
        Some(movements)
    }

    fn catch_up_full_tail(&mut self) -> Result<(), String> {
        let mut file = std::fs::File::open(&self.movements_path).map_err(|error| {
            format!(
                "provider ledger index cannot open '{}': {error}",
                self.movements_path.display()
            )
        })?;
        file.seek(std::io::SeekFrom::Start(self.byte_offset))
            .map_err(|error| format!("provider ledger index seek failed: {error}"))?;
        let mut reader = std::io::BufReader::new(file);
        loop {
            if self.through_seq.is_some_and(|last| self.next_seq > last) { return Ok(()); }
            match read_bounded_ledger_row(&mut reader, self.max_row_bytes)? {
                BoundedLedgerRow::Complete(row) => {
                    let movement = self
                        .decoder
                        .decode(&row, self.next_seq as usize)
                        .map_err(|error| {
                        format!(
                            "provider ledger index encountered malformed movement at byte {}: {error}",
                            self.byte_offset
                        )
                    })?;
                    if movement.seq.0 != self.next_seq {
                        return Err(format!(
                            "provider ledger index expected movement seq {}, found {}",
                            self.next_seq, movement.seq.0
                        ));
                    }
                    self.note(&movement)?;
                    self.byte_offset = self
                        .byte_offset
                        .checked_add(row.len() as u64)
                        .ok_or_else(|| "provider ledger byte watermark overflowed".to_string())?;
                    self.observed_len = self.observed_len.max(self.byte_offset);
                    self.next_seq = self.next_seq.checked_add(1).ok_or_else(|| {
                        "provider ledger sequence watermark overflowed".to_string()
                    })?;
                }
                BoundedLedgerRow::Partial | BoundedLedgerRow::Eof => return Ok(()),
            }
        }
    }

    fn note(&mut self, movement: &Movement) -> Result<(), String> {
        let Some(artifact) = movement.artifact.as_ref() else {
            return Ok(());
        };
        let artifact_key = artifact_ref_key(artifact);
        if let MovementKind::MarkOutcomeAdmitted(mark) = &movement.kind {
            if let Some(run) = self.related_mark_run(movement, artifact) {
                self.note_admitted_mark(movement, artifact, mark, &run);
            }
            return Ok(());
        }
        let addressed_to_exact_interface = match (&movement.kind, &movement.from, &movement.to) {
            (MovementKind::InterfaceArrival, Endpoint::Interface(interface), Endpoint::Interface(_)) => {
                interface == &self.interface
            }
            (
                MovementKind::InterfaceOutbound,
                Endpoint::Interface(_),
                Endpoint::Interface(interface),
            ) => interface == &self.interface,
            _ => false,
        };
        if !addressed_to_exact_interface {
            return Ok(());
        }
        let metadata = movement
            .invocation_metadata
            .as_ref()
            .ok_or_else(|| {
                format!(
                    "provider exact-interface movement {} carries no invocation metadata",
                    movement.seq.0
                )
            })?
            .invocation()
            .cloned()
            .ok_or_else(|| {
                format!(
                    "provider exact-interface movement {} carries non-invocation metadata",
                    movement.seq.0
                )
            })?;
        let Some(runtime) = valid_interface_movement_runtime(movement, &metadata) else {
            return Err(format!(
                "provider exact-interface movement {} carries mismatched runtime coordinates",
                movement.seq.0
            ));
        };
        if runtime.interface != self.interface {
            return Err(format!(
                "provider exact-interface movement {} names interface '{}' instead of '{}'",
                movement.seq.0,
                runtime.interface.as_str(),
                self.interface.as_str()
            ));
        }
        let run_key = runtime.run.as_str().to_string();
        self.latest_run_movement
            .insert((artifact_key.clone(), run_key.clone()), movement.seq.0);
        self.session_run_movements
            .entry((artifact_key.clone(), run_key.clone()))
            .or_default()
            .insert(movement.seq.0);
        let coordinate = RuntimeCoordinateKey::from_runtime(runtime);
        if movement.kind == MovementKind::InterfaceArrival {
            let context_key = (
                artifact_key.clone(),
                run_key.clone(),
                runtime.interface.as_str().to_string(),
            );
            self.provider_run_arrivals
                .entry(context_key.clone())
                .or_default()
                .insert(movement.seq.0);
            self.session_provider_arrivals
                .entry(context_key)
                .or_default()
                .insert(movement.seq.0);
            let coordinate_key = (artifact_key.clone(), coordinate.clone());
            match self.coordinate_arrivals.entry(coordinate_key) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(UniqueEvidence::One(movement.seq.0));
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    entry.insert(UniqueEvidence::Repeated);
                }
            }
            let mark_coordinate_key = (
                artifact_key.clone(),
                run_key,
                coordinate.checkpoint.clone(),
                coordinate.epoch,
            );
            match self.mark_arrival_coordinates.entry(mark_coordinate_key) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(UniqueArrivalCoordinate::One(coordinate.clone()));
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    if !matches!(
                        entry.get(),
                        UniqueArrivalCoordinate::One(existing) if existing == &coordinate
                    ) {
                        entry.insert(UniqueArrivalCoordinate::Repeated);
                    }
                }
            }
            self.coordinate_artifacts
                .entry(coordinate.clone())
                .or_default()
                .push((artifact.clone(), movement.seq.0));
        } else {
            let Endpoint::Interface(source) = &movement.from;
            if let (Some(admission), Some(binding)) = (
                metadata.checkpoint_admission.as_ref(),
                metadata.checkpoint_binding_policy.as_ref(),
            ) {
                if source != &self.interface
                    && admission.complete
                    && metadata.verification_failure.is_none()
                    && binding.inbound.interface == *source
                    && binding.inbound.admits(&admission.artifact_label)
                    && metadata.outbound_interface_bindings.contains(&self.interface)
                    && binding.outbound.iter().any(|target| {
                        target.interface == self.interface
                            && target.admits(&admission.artifact_label)
                    })
                    && admission.required_artifact_labels == metadata.required_verified_artifacts
                        .iter().map(|required| required.artifact_label.clone()).collect::<Vec<_>>()
                {
                    self.session_provider_inputs
                        .entry((artifact_key, run_key))
                        .or_default()
                        .insert(movement.seq.0, admission.artifact_label.clone());
                }
            }
            self.coordinate_outbounds
                .entry(coordinate)
                .or_default()
                .push(IndexedOutboundArtifact {
                    source: source.clone(),
                    artifact: artifact.clone(),
                    seq: movement.seq.0,
                    run_record_hash: runtime.run_record_hash.clone(),
                    outbound_interface_bindings: metadata.outbound_interface_bindings.clone(),
                    checkpoint_binding_policy: metadata.checkpoint_binding_policy.clone(),
                    required_verified_artifacts: metadata.required_verified_artifacts.clone(),
                    artifact_binding_refs: metadata.artifact_binding_refs.clone(),
                    readable_artifacts: metadata.grant.artifact_access.iter().filter(|(_, access)| access.read).map(|(id, _)| ArtifactId::new(id)).collect(),
                    role_artifacts: metadata.grant.role_artifacts.clone(),
                });
        }
        Ok(())
    }

    fn related_mark_run(&self, movement: &Movement, artifact: &ArtifactRef) -> Option<String> {
        let artifact_key = artifact_ref_key(artifact);
        if let Some(mark) = movement
            .invocation_metadata
            .as_ref()
            .and_then(|metadata| metadata.mark_outcome())
        {
            let key = (
                artifact_key,
                mark.run.as_str().to_string(),
                mark.subject.as_str().to_string(),
                mark.run_epoch,
            );
            return self
                .mark_arrival_coordinates
                .contains_key(&key)
                .then(|| mark.run.as_str().to_string());
        }
        None
    }

    fn note_admitted_mark(
        &mut self,
        movement: &Movement,
        artifact: &ArtifactRef,
        admitted_mark: &octopus_descriptors::MarkId,
        related_run: &str,
    ) {
        let artifact_key = artifact_ref_key(artifact);
        let result = (|| -> Result<(RuntimeCoordinateKey, String), String> {
            let raw = movement.invocation_metadata.as_ref().ok_or_else(|| {
                format!(
                    "provider recovery admitted mark movement {} has no metadata",
                    movement.seq.0
                )
            })?;
            let mark = raw.mark_outcome().ok_or_else(|| {
                format!(
                    "provider recovery admitted mark movement {} metadata is not a Mark outcome",
                    movement.seq.0
                )
            })?;
            if mark.name != *admitted_mark {
                return Err(format!(
                    "provider recovery admitted mark movement {} kind differs from its metadata",
                    movement.seq.0
                ));
            }
            if mark.evidence.as_ref() != Some(artifact) {
                return Err(format!(
                    "provider recovery admitted mark movement {} evidence differs from its artifact",
                    movement.seq.0
                ));
            }
            if mark.verifier.as_str().is_empty()
                || mark.verifier.as_str().trim() != mark.verifier.as_str()
            {
                return Err(format!(
                    "provider recovery admitted mark movement {} has no canonical verifier id",
                    movement.seq.0
                ));
            }
            let mark_coordinate_key = (
                artifact_key.clone(),
                mark.run.as_str().to_string(),
                mark.subject.as_str().to_string(),
                mark.run_epoch,
            );
            let coordinate = match self.mark_arrival_coordinates.get(&mark_coordinate_key) {
                Some(UniqueArrivalCoordinate::One(coordinate)) => coordinate,
                Some(UniqueArrivalCoordinate::Repeated) => {
                    return Err(format!(
                        "provider recovery admitted mark movement {} matches multiple arrival coordinates",
                        movement.seq.0
                    ));
                }
                None => {
                    return Err(format!(
                        "provider recovery admitted mark movement {} has no exact preceding arrival",
                        movement.seq.0
                    ));
                }
            };
            if movement.from != Endpoint::Interface(InterfaceId::new(&coordinate.interface))
                || movement.to != Endpoint::Interface(InterfaceId::new(&coordinate.worker))
            {
                return Err(format!(
                    "provider recovery admitted mark movement {} endpoints differ from its proving arrival",
                    movement.seq.0
                ));
            }
            let arrival = self
                .coordinate_arrivals
                .get(&(artifact_key.clone(), coordinate.clone()))
                .ok_or_else(|| {
                    format!(
                        "provider recovery admitted mark movement {} has no exact preceding arrival",
                        movement.seq.0
                    )
                })?;
            let verifier = mark.verifier.as_str().to_string();
            match arrival {
                UniqueEvidence::One(seq) if *seq < movement.seq.0 => {
                    Ok((coordinate.clone(), verifier))
                }
                UniqueEvidence::One(_) => Err(format!(
                    "provider recovery admitted mark movement {} does not follow its exact arrival",
                    movement.seq.0
                )),
                UniqueEvidence::Repeated => Err(format!(
                    "provider recovery admitted mark movement {} follows repeated arrival evidence",
                    movement.seq.0
                )),
            }
        })();
        let (coordinate, verifier) = match result {
            Ok(evidence) => evidence,
            Err(error) => {
                if !related_run.is_empty() && related_run.trim() == related_run {
                    self.handoff_metadata_poison
                        .entry((artifact_key, related_run.to_string()))
                        .or_insert(error);
                }
                return;
            }
        };
        let mark_key = (
            artifact_key,
            coordinate,
            admitted_mark.as_str().to_string(),
            verifier,
        );
        match self.coordinate_marks.entry(mark_key) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(UniqueEvidence::One(movement.seq.0));
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                entry.insert(UniqueEvidence::Repeated);
            }
        }
    }

    fn require_interface(&self, interface: &InterfaceId) -> Result<(), String> {
        if interface != &self.interface {
            return Err(format!(
                "provider ledger evidence index belongs to exact interface '{}', requested '{}'",
                self.interface.as_str(),
                interface.as_str()
            ));
        }
        Ok(())
    }

    fn require_coordinate_interface(
        &self,
        coordinate: &RuntimeCoordinateKey,
    ) -> Result<(), String> {
        if coordinate.interface != self.interface.as_str() {
            return Err(format!(
                "provider ledger evidence index belongs to exact interface '{}', coordinate names '{}'",
                self.interface.as_str(),
                coordinate.interface
            ));
        }
        Ok(())
    }

    fn bound_provenance<'a>(
        &mut self,
        run: &RunId,
        provider: &InterfaceId,
        artifacts: impl Iterator<Item = &'a ArtifactRef>,
    ) -> Result<BTreeMap<String, BoundArtifactProvenance>, String> {
        self.require_interface(provider)?;
        self.catch_up()?;
        let mut result = BTreeMap::new();
        for artifact in artifacts {
            let artifact_key = artifact_ref_key(artifact);
            let context_key = (
                artifact_key.clone(),
                run.as_str().to_string(),
                provider.as_str().to_string(),
            );
            if let Some(error) = self.context_metadata_poison.get(&context_key) {
                return Err(error.clone());
            }
            result.insert(
                artifact_key.clone(),
                BoundArtifactProvenance {
                    movement_seq: self
                        .latest_run_movement
                        .get(&(artifact_key.clone(), run.as_str().to_string()))
                        .copied(),
                    provider_arrival_seq: self
                        .provider_run_arrivals
                        .get(&context_key)
                        .and_then(|seqs| seqs.iter().next_back().copied()),
                },
            );
        }
        Ok(result)
    }

    fn session_evidence(
        &mut self,
        run: &RunId,
        provider: &InterfaceId,
        artifacts: impl Iterator<Item = String>,
    ) -> Result<BTreeMap<String, SessionSourceLedgerEvidence>, String> {
        self.require_interface(provider)?;
        self.catch_up()?;
        let mut result = BTreeMap::new();
        for artifact_key in artifacts {
            if let Some(error) = self
                .session_metadata_poison
                .get(&(artifact_key.clone(), run.as_str().to_string()))
            {
                return Err(error.clone());
            }
            result.insert(
                artifact_key.clone(),
                SessionSourceLedgerEvidence {
                    movements: self
                        .session_run_movements
                        .get(&(artifact_key.clone(), run.as_str().to_string()))
                        .cloned()
                        .unwrap_or_default(),
                    provider_inputs: self
                        .session_provider_inputs
                        .get(&(artifact_key.clone(), run.as_str().to_string()))
                        .cloned()
                        .unwrap_or_default(),
                    provider_arrivals: self
                        .session_provider_arrivals
                        .get(&(
                            artifact_key,
                            run.as_str().to_string(),
                            provider.as_str().to_string(),
                        ))
                        .cloned()
                        .unwrap_or_default(),
                },
            );
        }
        Ok(result)
    }

    fn admitted_handoff_seq(
        &mut self,
        coordinate: PendingHandoffLedgerCoordinate<'_>,
    ) -> Result<Option<u64>, String> {
        self.require_interface(coordinate.interface)?;
        self.catch_up()?;
        self.indexed_handoff_seq(coordinate, u64::MAX)
    }

    fn indexed_handoff_seq(&self, coordinate: PendingHandoffLedgerCoordinate<'_>, before_seq: u64) -> Result<Option<u64>, String> {
        let artifact_key = artifact_ref_key(coordinate.artifact);
        if let Some(error) = self
            .handoff_metadata_poison
            .get(&(artifact_key.clone(), coordinate.run.as_str().to_string()))
        {
            return Err(error.clone());
        }
        let runtime = RuntimeCoordinateKey::new(
            coordinate.run,
            coordinate.worker,
            coordinate.interface,
            coordinate.checkpoint,
            coordinate.epoch,
        );
        let arrival = match self
            .coordinate_arrivals
            .get(&(artifact_key.clone(), runtime.clone()))
        {
            None => return Ok(None),
            Some(UniqueEvidence::One(seq)) => *seq,
            Some(UniqueEvidence::Repeated) => {
                return Err(format!(
                    "provider recovery found repeated admitted arrival evidence for artifact '{}' at checkpoint '{}'",
                    coordinate.artifact.id.as_str(),
                    coordinate.checkpoint.as_str()
                ));
            }
        };
        if arrival >= before_seq { return Ok(None); }
        if let Some((mark, verifier)) = coordinate.required_evidence {
            let mark_seq = match self.coordinate_marks.get(&(
                artifact_key,
                runtime,
                mark.to_string(),
                verifier.to_string(),
            )) {
                None => return Ok(None),
                Some(UniqueEvidence::One(seq)) => *seq,
                Some(UniqueEvidence::Repeated) => {
                    return Err(format!(
                        "provider recovery found repeated admitted mark '{}' for artifact '{}' at checkpoint '{}'",
                        mark,
                        coordinate.artifact.id.as_str(),
                        coordinate.checkpoint.as_str()
                    ));
                }
            };
            if mark_seq <= arrival {
                return Err(format!(
                    "provider recovery mark '{}' for artifact '{}' does not follow its arrival at checkpoint '{}'",
                    mark,
                    coordinate.artifact.id.as_str(),
                    coordinate.checkpoint.as_str()
                ));
            }
            if mark_seq >= before_seq { return Ok(None); }
        }
        Ok(Some(arrival))
    }

    fn admitted_slot_artifacts(
        &mut self,
        coordinate: &RuntimeCoordinateKey,
        required_mark: &str,
        required_verifier: &str,
    ) -> Result<Vec<OneShotAdmittedArtifact>, String> {
        self.require_coordinate_interface(coordinate)?;
        self.catch_up()?;
        let mut admitted = Vec::new();
        for (artifact, seq) in self
            .coordinate_artifacts
            .get(coordinate)
            .into_iter()
            .flatten()
        {
            let artifact_key = artifact_ref_key(artifact);
            if let Some(error) = self
                .handoff_metadata_poison
                .get(&(artifact_key.clone(), coordinate.run.clone()))
            {
                return Err(error.clone());
            }
            match self
                .coordinate_arrivals
                .get(&(artifact_key.clone(), coordinate.clone()))
            {
                Some(UniqueEvidence::Repeated) => {
                    return Err(format!(
                        "provider one-shot checkpoint '{}' has repeated arrival evidence for artifact '{}'",
                        coordinate.checkpoint,
                        artifact.id.as_str()
                    ));
                }
                Some(UniqueEvidence::One(_)) => {}
                None => continue,
            }
            let mark_seq = match self.coordinate_marks.get(&(
                artifact_key,
                coordinate.clone(),
                required_mark.to_string(),
                required_verifier.to_string(),
            )) {
                None => continue,
                Some(UniqueEvidence::One(mark_seq)) => *mark_seq,
                Some(UniqueEvidence::Repeated) => {
                    return Err(format!(
                        "provider one-shot checkpoint '{}' has repeated mark '{}' for artifact '{}'",
                        coordinate.checkpoint,
                        required_mark,
                        artifact.id.as_str()
                    ));
                }
            };
            if mark_seq <= *seq {
                return Err(format!(
                    "provider one-shot checkpoint '{}' mark '{}' for artifact '{}' does not follow its arrival",
                    coordinate.checkpoint,
                    required_mark,
                    artifact.id.as_str()
                ));
            }
            admitted.push(OneShotAdmittedArtifact {
                artifact: artifact.clone(),
                arrival_seq: *seq,
                mark_seq,
            });
        }
        if admitted.len() > 1 {
            return Err(format!(
                "provider one-shot checkpoint '{}' has {} admitted artifacts",
                coordinate.checkpoint,
                admitted.len()
            ));
        }
        Ok(admitted)
    }

    fn outbound_artifacts(
        &mut self, coordinate: &RuntimeCoordinateKey, source: &InterfaceId, after_seq: Option<u64>,
    ) -> Result<Vec<IndexedOutboundArtifact>, String> {
        self.require_coordinate_interface(coordinate)?;
        self.catch_up()?;
        Ok(self.coordinate_outbounds.get(coordinate).into_iter().flatten()
            .filter(|evidence| evidence.source == *source && after_seq.is_none_or(|after| evidence.seq > after))
            .cloned().collect())
    }

    fn unique_outbound_artifact(
        &mut self,
        coordinate: &RuntimeCoordinateKey,
        source: &InterfaceId,
        after_seq: Option<u64>,
    ) -> Result<Option<IndexedOutboundArtifact>, String> {
        self.require_coordinate_interface(coordinate)?;
        self.catch_up()?;
        let mut matches = self
            .coordinate_outbounds
            .get(coordinate)
            .into_iter()
            .flatten()
            .filter(|evidence| {
                evidence.source == *source && after_seq.map_or(true, |after| evidence.seq > after)
            });
        let evidence = matches.next().cloned();
        if matches.next().is_some() {
            return Err(format!(
                "provider checkpoint '{}' has repeated outbound artifact evidence from '{}'",
                coordinate.checkpoint,
                source.as_str()
            ));
        }
        Ok(evidence)
    }

    fn exact_provider_source_outbound(
        &mut self,
        run: &RunId,
        worker: &InterfaceId,
        provider: &InterfaceId,
        sequence: u64,
    ) -> Result<(RuntimeCoordinateKey, IndexedOutboundArtifact), String> {
        self.require_interface(provider)?;
        self.catch_up()?;
        let mut matches = self
            .coordinate_outbounds
            .iter()
            .filter(|(coordinate, _)| {
                coordinate.run == run.as_str()
                    && coordinate.worker == worker.as_str()
                    && coordinate.interface == provider.as_str()
            })
            .flat_map(|(coordinate, evidence)| {
                evidence
                    .iter()
                    .filter(move |item| item.seq == sequence)
                    .map(move |item| (coordinate.clone(), item.clone()))
            });
        let found = matches.next().ok_or_else(|| {
            format!(
                "provider source movement {sequence} has no exact outbound evidence for run '{}' and interface '{}'",
                run.as_str(),
                provider.as_str()
            )
        })?;
        if matches.next().is_some() {
            return Err(format!(
                "provider source movement {sequence} has repeated exact outbound evidence for run '{}' and interface '{}'",
                run.as_str(),
                provider.as_str()
            ));
        }
        Ok(found)
    }
}

fn valid_interface_movement_runtime<'a>(
    movement: &Movement,
    metadata: &'a InvocationMetadata,
) -> Option<&'a octopus_interfaces::RuntimeInvocationMetadata> {
    let runtime = metadata.runtime()?;
    match (&movement.kind, &movement.from, &movement.to) {
        (MovementKind::InterfaceArrival, Endpoint::Interface(source), Endpoint::Interface(worker))
            if source == &runtime.interface && worker == &runtime.worker =>
        {
            Some(runtime)
        }
        (MovementKind::InterfaceOutbound, Endpoint::Interface(_), Endpoint::Interface(target))
            if target == &runtime.interface =>
        {
            Some(runtime)
        }
        _ => None,
    }
}

impl RuntimeCoordinateKey {
    fn new(
        run: &RunId,
        worker: &InterfaceId,
        interface: &InterfaceId,
        checkpoint: &CheckpointId,
        epoch: u64,
    ) -> Self {
        Self {
            run: run.as_str().to_string(),
            worker: worker.as_str().to_string(),
            interface: interface.as_str().to_string(),
            checkpoint: checkpoint.as_str().to_string(),
            epoch,
        }
    }

    fn from_runtime(runtime: &octopus_interfaces::RuntimeInvocationMetadata) -> Self {
        Self::new(
            &runtime.run,
            &runtime.worker,
            &runtime.interface,
            &runtime.checkpoint,
            runtime.epoch,
        )
    }
}

fn read_bounded_ledger_row(
    reader: &mut impl BufRead,
    max_row_bytes: u64,
) -> Result<BoundedLedgerRow, String> {
    let mut row = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|error| format!("provider ledger index read failed: {error}"))?;
        if available.is_empty() {
            return Ok(if row.is_empty() {
                BoundedLedgerRow::Eof
            } else {
                BoundedLedgerRow::Partial
            });
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |index| index + 1);
        if row.len() as u64 + take as u64 > max_row_bytes {
            return Err(format!(
                "provider ledger row exceeds the declared {} byte limit",
                max_row_bytes
            ));
        }
        row.extend_from_slice(&available[..take]);
        reader.consume(take);
        if newline.is_some() {
            return Ok(BoundedLedgerRow::Complete(row));
        }
    }
}

impl ProviderOutputHandoff {
    fn contributed_context_output_labels(&self) -> BTreeSet<String> {
        // A context-free input can still append a declared response to the
        // existing bindings. It cannot replace history with a snapshot built
        // without that history (for example, an independent failure response).
        self.context_output_labels.iter()
            .filter(|label| self.source_carries_context || !self.session_snapshot_labels.contains(*label))
            .cloned().collect()
    }

    pub async fn input_changes_context(&self) -> bool {
        self.state.lock().await.consumed_input
    }

    pub fn validate_client_output_routes(&self, labels: &BTreeSet<String>) -> Result<(), String> {
        for label in labels {
            self.config.output_checkpoint(
                &ProviderHandoffRoute::ClientProjection,
                &self.granted_tools,
                &self.granted_interfaces,
                label,
            )?;
        }
        Ok(())
    }

    pub fn validate_model_tool_routes(
        &self,
        model_visible_tools: &BTreeSet<String>,
    ) -> Result<(), String> {
        for tool in model_visible_tools {
            self.config.output_checkpoint(
                &ProviderHandoffRoute::Tool { tool: tool.clone() },
                &self.granted_tools,
                &self.granted_interfaces,
                &self.config.file_write_label,
            )?;
        }
        Ok(())
    }

    pub fn callable_tool_names(&self, input: &InterfaceInvocationEnvelope) -> Result<BTreeSet<String>, String> {
        let metadata = input.metadata().map_err(|error| error.to_string())?;
        if metadata != &self.invocation_metadata { return Err("callable tool projection requires its exact accepted invocation metadata".into()); }
        let runtime = input.runtime().map_err(|error| error.to_string())?;
        if runtime.run != self.config.run || runtime.worker != self.config.worker || runtime.interface != self.config.provider_interface {
            return Err("callable tool projection differs from its accepted run, worker or provider".into());
        }
        self.config.callable_tool_names(metadata)
    }

    fn from_invocation_with_session(
        session: ProviderHandoffSession,
        ctx: &InterfaceLiftContext,
        descriptor: &InterfaceDescriptor,
        run: &RunId,
        input: &InterfaceInvocationEnvelope,
        source_movement_seq: u64,
        pinned_run_record: &[u8],
    ) -> Result<Self, CheckedRunError> {
        if descriptor.id != ctx.interface {
            return Err(format!(
                "provider descriptor '{}' does not match lifted interface '{}'",
                descriptor.id.as_str(),
                ctx.interface.as_str()
            ).into());
        }
        let profile = run_authoring_profile(descriptor)?;
        let runtime = input.runtime().map_err(|error| error.to_string())?;
        let granted_tools = input
            .granted_tool_names()
            .map_err(|error| error.to_string())?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let granted_interfaces = input
            .metadata()
            .map_err(|error| error.to_string())?
            .allowed_interfaces()
            .iter()
            .map(InterfaceId::new)
            .collect::<BTreeSet<_>>();
        let granted_mark_outcomes = input
            .metadata()
            .map_err(|error| error.to_string())?
            .grant()
            .mark_outcomes
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let (context_output_labels, session_snapshot_labels) =
            provider_context_label_sets(descriptor)?;
        let run_records_root = ctx.root_slot("run_records_root")?;
        if !run_records_root.is_absolute() {
            return Err(format!(
                "provider interface '{}' run_records_root is not absolute: {}",
                descriptor.id.as_str(),
                run_records_root.display()
            ).into());
        }
        let run_records_root = run_records_root.canonicalize().map_err(|error| {
            format!(
                "provider interface '{}' run_records_root '{}' cannot be canonicalized: {error}",
                descriptor.id.as_str(),
                run_records_root.display()
            )
        })?;
        let hand_in_root = ctx.root_slot("interface_hand_in_root")?;
        if !hand_in_root.is_absolute() {
            return Err(format!(
                "provider interface '{}' interface_hand_in_root is not absolute: {}",
                descriptor.id.as_str(),
                hand_in_root.display()
            ).into());
        }
        let initial_record = RunRecord::from_bytes(pinned_run_record).map_err(|error| {
            format!(
                "provider run '{}' pinned record is malformed: {error}",
                run.as_str()
            )
        })?;
        if initial_record.id != *run
            || runtime.run != *run
            || initial_record.worker != runtime.worker
            || runtime.interface != ctx.interface
        {
            return Err(format!(
                "provider pinned invocation topology has run='{}' worker='{}', metadata has run='{}' worker='{}' interface='{}', lifted interface is '{}'",
                initial_record.id.as_str(),
                initial_record.worker.as_str(),
                runtime.run.as_str(),
                runtime.worker.as_str(),
                runtime.interface.as_str(),
                ctx.interface.as_str()
            ).into());
        }
        let (config, bound_topology) = resolve_run_authoring_invocation(
            &profile,
            &ctx.interface,
            run,
            &initial_record,
            &runtime.checkpoint,
        )?;
        let mandatory_role_bindings = current_mandatory_role_bindings(&initial_record, &config)?;
        let consumed_checkpoint =
            initial_record
                .checkpoint(&runtime.checkpoint)
                .ok_or_else(|| {
                    format!(
                        "provider invocation names checkpoint '{}' absent from pinned run '{}'",
                        runtime.checkpoint.as_str(),
                        initial_record.id.as_str()
                    )
                })?;
        let consumed_templates = config
            .checkpoints
            .iter()
            .filter(|template| {
                template.role.is_provider_input()
                    && checkpoint_role_bindings_match(
                        consumed_checkpoint,
                        &mandatory_role_bindings,
                        template,
                    )
                    && consumed_checkpoint.interface_binding.as_ref()
                        == Some(&template.interface_binding)
                    && consumed_checkpoint.outbound_interface_bindings
                        == template.outbound_interface_bindings
                    && consumed_checkpoint.required_verified_artifacts
                        == template.required_verified_artifacts
            })
            .cloned()
            .collect::<Vec<_>>();
        if consumed_templates.len() > 1 {
            return Err(format!(
                "provider invocation checkpoint '{}' matches {} provider-input run_authoring templates",
                runtime.checkpoint.as_str(),
                consumed_templates.len()
            ).into());
        }
        if consumed_templates.is_empty() {
            return Err(format!(
                "provider invocation checkpoint '{}' is not one bound run_authoring input",
                runtime.checkpoint.as_str()
            ).into());
        }
        let consumed_context_free_independent_input = (consumed_templates[0].role
            == RunAuthoringCheckpointRole::IndependentInput
            && !consumed_templates[0].include_context)
            .then(|| consumed_templates[0].clone());
        let consumed_verified_input =
            consumed_templates[0].role == RunAuthoringCheckpointRole::VerifiedInput;
        if consumed_checkpoint.epoch.0 != runtime.epoch {
            return Err(format!(
                "provider invocation checkpoint '{}' epoch {} differs from attested epoch {}",
                runtime.checkpoint.as_str(),
                consumed_checkpoint.epoch.0,
                runtime.epoch
            ).into());
        }
        let requested_run_path = run_records_root.join(format!("{}.json", run.as_str()));
        let run_path = requested_run_path.canonicalize().map_err(|error| {
            format!(
                "provider run '{}' current record '{}' cannot be canonicalized: {error}",
                run.as_str(),
                requested_run_path.display()
            )
        })?;
        if run_path.parent() != Some(run_records_root.as_path()) {
            return Err(format!(
                "provider run '{}' record '{}' escapes the granted run_records_root '{}'",
                run.as_str(),
                run_path.display(),
                run_records_root.display()
            ).into());
        }
        input_revision::validate_initial_history(ctx, &run_path, pinned_run_record, &config, source_movement_seq)?;
        let current_run_record = std::fs::read(&run_path).map_err(|error| {
            format!(
                "provider run '{}' current record '{}' cannot be read: {error}",
                run.as_str(),
                run_path.display()
            )
        })?;
        let current_record = RunRecord::from_bytes(&current_run_record).map_err(|error| {
            format!(
                "provider run '{}' current record is malformed: {error}",
                run.as_str()
            )
        })?;
        if current_record.id != config.run || current_record.worker != config.worker {
            return Err(format!(
                "provider current run topology has run='{}' worker='{}', config requires run='{}' worker='{}'",
                current_record.id.as_str(),
                current_record.worker.as_str(),
                config.run.as_str(),
                config.worker.as_str()
            ).into());
        }
        let (current_config, current_topology) = resolve_profile_invocation(
            &profile,
            &ctx.interface,
            run,
            &current_record,
            current_record
                .interface_state
                .get(&ctx.interface)
                .and_then(|state| state.get(RUN_AUTHORING_BINDING))
                .is_none()
                .then_some(&runtime.checkpoint),
        )?;
        let independent_input_unchanged = consumed_context_free_independent_input.is_some()
            && current_record.checkpoint(&runtime.checkpoint) == Some(consumed_checkpoint)
            && current_record.active_checkpoints.contains(&runtime.checkpoint)
            && bound_topology.payload_artifact_bindings == current_topology.payload_artifact_bindings
            && !input_revision::is_payload_revision(&initial_record, &current_record, &config.provider_interface)?
            && bound_topology.checkpoints.iter().filter(|template| template.role == RunAuthoringCheckpointRole::OwnerInput)
                .eq(current_topology.checkpoints.iter().filter(|template| template.role == RunAuthoringCheckpointRole::OwnerInput));
        let (coauthor_chain, run_record_evidence_seq) = stable_run_observation(
            &run_path, &current_run_record, ctx.required_host_io_limits()?.max_payload_bytes, || {
                if independent_input_unchanged {
                    validate_unchanged_admitted_checkpoint(
                        pinned_run_record, &current_run_record, &runtime.checkpoint,
                    )?;
                    if runtime.run_record_hash.as_deref() != Some(ContentHash::of_bytes(pinned_run_record).to_hex().as_str())
                        || runtime.ee != initial_record.ee
                        || !initial_record.active_checkpoints.contains(&runtime.checkpoint)
                        || current_config.checkpoints.iter().filter(|template| *template == &consumed_templates[0]).count() != 1
                        || current_mandatory_role_bindings(&current_record, &current_config)? != mandatory_role_bindings
                    {
                        return Err("context-free independent input changed its admitted contract or scope".into());
                    }
                    let mut index = session.index()?;
                    let (coordinate, evidence) = index.exact_provider_source_outbound(
                        run, &runtime.worker, &ctx.interface, source_movement_seq,
                    )?;
                    let metadata = input.metadata().map_err(|error| error.to_string())?;
                    if coordinate.checkpoint != runtime.checkpoint.as_str() || coordinate.epoch != runtime.epoch
                        || evidence.artifact != input.artifact || evidence.run_record_hash != runtime.run_record_hash
                        || evidence.source != *consumed_templates[0].interface()
                        || !indexed_evidence_matches_checkpoint(&evidence, &consumed_templates[0])
                        || !metadata_matches_checkpoint(metadata, &consumed_templates[0])
                        || evidence.artifact_binding_refs != metadata.artifact_binding_refs
                        || !artifact_refs_match_bindings(&evidence.artifact_binding_refs, &consumed_checkpoint.artifact_bindings)
                    {
                        return Err("context-free independent input has no exact admitted source".into());
                    }
                    Ok((Vec::new(), index.next_seq.saturating_sub(1).max(source_movement_seq)))
                } else {
                    let chain = checked_selected_run_advance_state(
                        ctx, &run_path, pinned_run_record, &current_run_record, source_movement_seq, &config,
                    )?;
                    let evidence_seq = chain.last().map(|transition| transition.call.seq.0).unwrap_or(source_movement_seq);
                    Ok((chain, evidence_seq))
                }
            },
        )?;
        let added_outputs = output_connections::is_extension(&bound_topology, &current_topology);
        let revised_inputs = input_revision::is_revision(&bound_topology, &current_topology);
        let revised_payloads = coauthor_chain.iter().any(|transition| {
            transition.writer.owner == config.provider_interface
                && input_revision::validate_payload_revision(&transition.before, &transition.after, &transition.writer).is_ok()
        });
        let amended_capabilities = capability_amendment::checked_topology_advance(
            &config.provider_interface, &bound_topology, &current_topology, &coauthor_chain,
        )?;
        let retired_verified_input = !independent_input_unchanged && !added_outputs && !revised_inputs && !revised_payloads && !amended_capabilities
            && (current_config != config || current_topology != bound_topology);
        if retired_verified_input {
            validate_retired_verified_input(
                ctx,
                &session,
                &initial_record,
                &current_record,
                &config,
                &current_config,
                &bound_topology,
                &current_topology,
                &consumed_templates[0],
                source_movement_seq,
            )?;
        }
        let config = current_config;
        let bound_topology = current_topology;
        let effective_record = &current_record;
        let effective_record_bytes = current_run_record;
        let mut context_artifacts = current_context_artifacts(effective_record, &config)?;
        let mut static_context_artifacts = config.payload_artifact_bindings.iter().filter(|reference| context_artifacts.contains(&reference.id)).map(|reference| reference.id.clone()).collect::<Vec<_>>();
        let mut session_context = ResolvedSessionContext {
            provider_interface: config.provider_interface.clone(),
            artifacts: Vec::new(),
        };
        let mut session_snapshot_head = None;
        let consumes_context = consumed_templates[0].include_context;
        if !consumes_context {
            let payload_ids = input
                .artifact_binding_payloads
                .iter()
                .map(|bound| &bound.artifact.id)
                .collect::<Vec<_>>();
            let mut mandatory_ids = mandatory_role_bindings
                .iter()
                .map(|binding| &binding.id)
                .collect::<Vec<_>>();
            for id in &consumed_templates[0].static_artifact_bindings {
                if !mandatory_ids.contains(&id) { mandatory_ids.push(id); }
            }
            if payload_ids != mandatory_ids {
                return Err(format!(
                    "provider context-free invocation checkpoint '{}' does not carry exactly its mandatory role bindings",
                    runtime.checkpoint.as_str()
                ).into());
            }
        }
        let bound_provenance = session.index()?.bound_provenance(
            &config.run,
            &config.provider_interface,
            input
                .artifact_binding_payloads
                .iter()
                .map(|bound| &bound.artifact),
        )?;
        let input_sources = input_artifacts::bound_sources(&initial_record, &config.provider_interface, &runtime.checkpoint, &input.metadata().map_err(|error| error.to_string())?.artifact_binding_refs)?;
        for bound in &input.artifact_binding_payloads {
            if input_sources.contains(&bound.artifact) && consumes_context {
                append_artifact(&mut context_artifacts, bound.artifact.id.clone());
                append_artifact(&mut static_context_artifacts, bound.artifact.id.clone());
            }
        }
        let context_payloads = context_artifact_payloads(&initial_record, input.metadata().map_err(|error| error.to_string())?, &input.artifact_binding_payloads)?;
        let instruction_sources = bound_instruction_sources(&context_payloads, &[])?;
        for bound in &context_payloads {
            if consumes_context {
                append_artifact(&mut context_artifacts, bound.artifact.id.clone());
            }
            if instruction_sources.contains(&bound.artifact.id)
                || consumed_templates[0].static_artifact_bindings.contains(&bound.artifact.id) {
                append_artifact(&mut static_context_artifacts, bound.artifact.id.clone());
                continue;
            }
            collect_bound_context(
                ctx,
                &mut static_context_artifacts,
                &mut session_context,
                &mut session_snapshot_head,
                &bound.artifact,
                &bound.read_bytes().map_err(|error| error.to_string())?,
                bound_provenance
                    .get(&artifact_ref_key(&bound.artifact))
                    .cloned()
                    .unwrap_or_default(),
                &session_snapshot_labels,
                &config.provider_interface,
            )?;
        }
        ground_session_context_movements(&mut session_context, &session, &config.run, &config)?;
        validate_session_context_evidence(
            &mut session_context,
            SessionContextEvidenceScope {
                blobs_dir: &ctx.blobs_dir,
                limits: ctx.required_host_io_limits()?,
                run: &config.run,
                provider: &config.provider_interface,
                context_output_labels: &context_output_labels,
                session_snapshot_labels: &session_snapshot_labels,
            },
            &session,
        )?;
        let invocation_snapshot_head = session_snapshot_head.clone();
        reconcile_resolved_session_snapshot_head(&session_context, &mut session_snapshot_head)?;
        let invocation_session_snapshot =
            invocation_snapshot_head.map(|head| ProviderInvocationSessionSnapshot {
                carrier: head.artifact,
                artifacts: Arc::new(
                    session_context.artifacts[..=head.covered_artifact_count].to_vec(),
                ),
            });
        let consumed_input =
            if consumed_verified_input || consumed_context_free_independent_input.is_some() {
                false
            } else {
                let already_bound = context_artifacts
                    .iter()
                    .any(|artifact| artifact == &input.artifact.id);
                append_artifact(&mut context_artifacts, input.artifact.id.clone());
                let input_manifest = input
                    .read_artifact_manifest()
                    .map_err(|error| error.to_string())?;
                let input_payload = input
                    .read_artifact_payload()
                    .map_err(|error| error.to_string())?;
                let appended = append_session_payload(
                    &mut session_context,
                    &input_manifest.label,
                    &input_payload,
                    &input.artifact,
                    Some(source_movement_seq),
                    input.artifact.id.as_str(),
                )?;
                !already_bound && appended
            };
        let delivery_mode = derive_handoff_delivery_mode(
            input,
            &config,
            effective_record,
            &runtime.checkpoint,
            &granted_tools,
        )?;
        if consumed_context_free_independent_input.is_some()
            && !matches!(delivery_mode, HandoffDeliveryMode::Rearm)
        {
            return Err(format!(
                "provider context-free independent input '{}' has no run re-arm authority",
                runtime.checkpoint.as_str()
            ).into());
        }
        let result_reader = LedgerTailReader::open_named(ctx, "run-authoring")?;
        let handoff = Self {
            ctx: ctx.clone(),
            invocation_metadata: input.metadata().map_err(|error| error.to_string())?.clone(),
            run_path,
            hand_in_root: hand_in_root.join(config.authoring_source_interface.as_str()),
            var_dir: ctx.interface_var_dir(),
            source_movement_seq,
            source_carries_context: consumes_context,
            failed_source: false,
            checked_settlement_output: None,
            config,
            context_output_labels,
            session_snapshot_labels,
            granted_tools,
            granted_interfaces,
            granted_mark_outcomes,
            bound_topology,
            delivery_mode,
            session,
            invocation_session_snapshot,
            state: Arc::new(tokio::sync::Mutex::new(HandoffState {
            validated_withdrawal_output: None,
                context_artifacts,
                static_context_artifacts,
                session_context,
                session_snapshot_head,
                run_record_bytes: effective_record_bytes,
                run_record_evidence_seq: Some(run_record_evidence_seq),
                consumed_input,
                consumed_context_free_independent_input,
                consumed_verified_input,
            })),
            result_reader: Arc::new(tokio::sync::Mutex::new(result_reader)),
        };
        if !coauthor_chain.is_empty() {
            let mut state = handoff.state.try_lock()
                .map_err(|_| "new handoff state is already borrowed".to_string())?;
            let publications = handoff.checked_context_publications(&coauthor_chain, &state.run_record_bytes)?;
            let input_entries = take_unpublished_context_inputs(&mut state, source_movement_seq)?;
            for transition in &coauthor_chain {
                let authored = RunRecord::from_bytes(&transition.after).map_err(|error| error.to_string())?;
                handoff.restore_published_context(&mut state, &current_record, &authored, &publications)?;
            }
            restore_consumed_context_inputs(&mut state, input_entries)?;
        }
        if retired_verified_input {
            let pending =
                persisted_pending_handoff(&current_record, &handoff.config.provider_interface)?
                    .ok_or_else(|| {
                        "retired verified input lost its terminal handoff".to_string()
                    })?;
            if handoff
                .pending_handoff_evidence(&pending)?
                .iter()
                .any(Option::is_none)
            {
                return Err(
                    "queued tool result precedes the admitted provider decision handoff".into(),
                );
            }
            let mut state = handoff
                .state
                .try_lock()
                .map_err(|_| "new handoff state is already borrowed".to_string())?;
            handoff.restore_pending_context(&mut state, &current_record, &pending)?;
            if state.consumed_input {
                append_artifact(&mut state.context_artifacts, input.artifact.id.clone());
            }
        }
        {
            let mut state = handoff.state.try_lock()
                .map_err(|_| "new handoff state is already borrowed".to_string())?;
            handoff.restore_failed_context(&mut state, &coauthor_chain)?;
        }
        Ok(handoff)
    }

    pub fn invocation_session_snapshot(&self) -> Option<ProviderInvocationSessionSnapshot> {
        self.invocation_session_snapshot.clone()
    }

    pub async fn recovered_failed_context_sources(&self) -> Result<Vec<u64>, String> {
        let state = self.state.lock().await;
        let recovered = self.checked_failed_source_contexts(&state.run_record_bytes, &[], None)?;
        Self::restored_failed_context_sources(&state.session_context, recovered)
    }

    pub async fn recovered_failed_context_source(&self, sequence: u64) -> Result<bool, String> {
        let state = self.state.lock().await;
        let recovered = self.checked_failed_source_contexts(
            &state.run_record_bytes, &[], Some(sequence),
        )?;
        Ok(Self::restored_failed_context_sources(&state.session_context, recovered)?.contains(&sequence))
    }

    fn restored_failed_context_sources(
        restored: &ResolvedSessionContext, recovered: BTreeMap<u64, ResolvedSessionContext>,
    ) -> Result<Vec<u64>, String> {
        let mut sources = Vec::new();
        for (source, context) in recovered {
            let mut previous = None;
            for entry in &context.artifacts {
                let position = restored.artifacts.iter()
                    .position(|existing| existing.source_artifact == entry.source_artifact)
                    .ok_or("checked failed source context was not restored into this handoff")?;
                if previous.is_some_and(|prior| prior >= position)
                    || !session_context_entries_agree(&restored.artifacts[position], entry)
                {
                    return Err("checked failed source context conflicts with this handoff".into());
                }
                previous = Some(position);
            }
            sources.push(source);
        }
        Ok(sources)
    }

    pub async fn current_run_record_bytes(&self) -> Vec<u8> {
        self.state.lock().await.run_record_bytes.clone()
    }

    pub async fn reconciled_run_record_bytes(&self) -> Result<Vec<u8>, String> {
        let mut state = self.state.lock().await;
        self.reconcile_checked_run_state(&mut state).await?;
        Ok(state.run_record_bytes.clone())
    }

    pub async fn consumes_context_free_independent_input(&self) -> bool {
        self.state
            .lock()
            .await
            .consumed_context_free_independent_input
            .is_some()
    }

    pub async fn current_session_context_artifacts(&self) -> Arc<Vec<SessionContextArtifact>> {
        Arc::new(self.state.lock().await.session_context.artifacts.clone())
    }

    pub async fn hand(&self, emission: ProviderEmission) -> Result<ProviderHandoffOutcome, String> {
        self.hand_batch_with_terminal(vec![emission.into()], false)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| "provider handoff produced no outcome".to_string())
    }

    pub async fn hand_transport_dispatch(
        &self,
        artifact: InterfaceArtifactFile,
    ) -> Result<ProviderHandoffOutcome, String> {
        let manifest = artifact.read_manifest().map_err(|error| {
            format!("provider transport dispatch manifest cannot be read: {error}")
        })?;
        if manifest.label != PROVIDER_TRANSPORT_DISPATCH_LABEL {
            return Err(format!(
                "provider transport dispatch manifest label '{}' differs from '{}'",
                manifest.label, PROVIDER_TRANSPORT_DISPATCH_LABEL
            ));
        }
        let payload = artifact.read_payload_bytes().map_err(|error| {
            format!("provider transport dispatch payload cannot be read: {error}")
        })?;
        let dispatch = validate_transport_dispatch_payload(
            &payload,
            &self.ctx.interface,
            self.source_movement_seq,
        )?;
        match &self.delivery_mode {
            HandoffDeliveryMode::Rearm => self
                .hand_batch_with_terminal(
                    vec![ProviderHandoffEmission {
                        artifact,
                        route: ProviderHandoffRoute::TransportDispatch {
                            attempt: dispatch.attempt,
                        },
                    }],
                    false,
                )
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| "provider transport dispatch produced no outcome".to_string()),
            HandoffDeliveryMode::OneShot(plan) => {
                let _state = self.state.lock().await;
                let slot = plan.dispatch.get(&dispatch.attempt).ok_or_else(|| {
                    format!(
                        "provider one-shot topology has no preauthored transport slot for attempt {}",
                        dispatch.attempt
                    )
                })?;
                let completion_coordinate = RuntimeCoordinateKey::new(
                    &self.config.run,
                    &self.config.worker,
                    &self.config.provider_interface,
                    &plan.completion.checkpoint,
                    plan.completion.epoch,
                );
                let completion = self.session.index()?.admitted_slot_artifacts(
                    &completion_coordinate,
                    &plan.completion.required_mark,
                    &plan.completion.required_verifier,
                )?;
                if !completion.is_empty() {
                    return Err(
                        "provider one-shot transport dispatch follows completion evidence"
                            .to_string(),
                    );
                }
                if !self.one_shot_terminal_evidence(plan)?.is_empty() {
                    return Err(
                        "provider one-shot transport dispatch follows terminal evidence"
                            .to_string(),
                    );
                }
                self.hand_one_shot_artifact(artifact, slot).await
            }
        }
    }

    pub async fn hand_final(
        &self,
        emission: ProviderEmission,
    ) -> Result<ProviderHandoffOutcome, String> {
        self.hand_batch_with_terminal(vec![emission.into()], true)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| "provider handoff produced no outcome".to_string())
    }

    pub async fn hand_batch(
        &self,
        emissions: Vec<ProviderEmission>,
    ) -> Result<Vec<ProviderHandoffOutcome>, String> {
        self.hand_batch_with_terminal(emissions.into_iter().map(Into::into).collect(), true)
            .await
    }

    pub async fn hand_progress_batch(
        &self,
        emissions: Vec<ProviderEmission>,
    ) -> Result<Vec<ProviderHandoffOutcome>, String> {
        self.hand_batch_with_terminal(emissions.into_iter().map(Into::into).collect(), false)
            .await
    }

    pub async fn hand_observation(
        &self,
        emission: ProviderEmission,
        terminal: bool,
    ) -> Result<ProviderHandoffOutcome, String> {
        let label = emission.artifact.read_manifest().map_err(|error| error.to_string())?.label;
        if self.context_output_labels.contains(&label) || self.session_snapshot_labels.contains(&label) {
            return Err("a progress observation cannot change the selected context".into());
        }
        self.hand_batch_with_bindings(vec![emission.into()], terminal, &[], false, None, None).await?
            .into_iter().next().ok_or_else(|| "provider observation produced no outcome".into())
    }

    pub async fn hand_from_input(
        &self,
        emission: ProviderEmission,
        terminal: bool,
        source_movement_seq: u64,
        consume_context: bool,
    ) -> Result<ProviderHandoffOutcome, String> {
        if !matches!(self.delivery_mode, HandoffDeliveryMode::Rearm) {
            return Err("a response consuming an independent input requires run-authoring authority".into());
        }
        let label = emission
            .artifact
            .read_manifest()
            .map_err(|error| error.to_string())?
            .label;
        if !consume_context
            && (self.context_output_labels.contains(&label)
                || self.session_snapshot_labels.contains(&label))
        {
            return Err("a progress observation cannot change the selected context".into());
        }
        self.hand_batch_with_bindings(
            vec![emission.into()],
            terminal,
            &[],
            consume_context,
            Some(source_movement_seq),
            None,
        )
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| "provider input response produced no outcome".into())
    }

    fn independent_input_at_source(
        &self,
        current: &[u8],
        source_movement_seq: u64,
    ) -> Result<Option<RunAuthoringCheckpoint>, String> {
        let (coordinate, evidence) = self.session.index()?.exact_provider_source_outbound(
            &self.config.run,
            &self.config.worker,
            &self.config.provider_interface,
            source_movement_seq,
        )?;
        let hash = evidence
            .run_record_hash
            .as_deref()
            .and_then(ContentHash::from_hex)
            .ok_or("independent input has no exact admitted run snapshot")?;
        let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(
            &self.ctx.blobs_dir.join(hash.to_hex()),
            self.ctx.required_host_io_limits()?.max_payload_bytes,
        ).map_err(|error| error.to_string())?;
        if ContentHash::of_bytes(&bytes) != hash {
            return Err("independent input snapshot hash differs".into());
        }
        let pinned = RunRecord::from_bytes(&bytes).map_err(|error| error.to_string())?;
        if pinned.id != self.config.run || pinned.worker != self.config.worker {
            return Err("independent input snapshot changed its run or worker".into());
        }
        let id = CheckpointId::new(&coordinate.checkpoint);
        let checkpoint = pinned
            .checkpoint(&id)
            .ok_or("independent input snapshot lost its checkpoint")?;
        let templates = self
            .config
            .checkpoints
            .iter()
            .filter(|template| {
                template.role.is_provider_input()
                    && checkpoint_record_matches_template(checkpoint, template, &self.config.run)
            })
            .collect::<Vec<_>>();
        if templates.len() != 1
            || checkpoint.epoch.0 != coordinate.epoch
            || !pinned.active_checkpoints.contains(&id)
            || !indexed_evidence_matches_checkpoint(&evidence, templates[0])
            || !artifact_refs_match_bindings(
                &evidence.artifact_binding_refs,
                &checkpoint.artifact_bindings,
            )
        {
            return Err(
                "independent input source differs from its exact admitted checkpoint".into(),
            );
        }
        let template = templates[0];
        if template.role != RunAuthoringCheckpointRole::IndependentInput || template.include_context
        {
            return Ok(None);
        }
        validate_unchanged_admitted_checkpoint(&bytes, current, &id)?;
        Ok(Some(template.clone()))
    }

    pub async fn hand_batch_with_outgoing_bindings(
        &self,
        emissions: Vec<ProviderEmission>,
        terminal: bool,
        bindings: &[ProviderOutgoingBinding],
    ) -> Result<Vec<ProviderHandoffOutcome>, String> {
        self.hand_batch_with_bindings(emissions.into_iter().map(Into::into).collect(), terminal, bindings, true, None, None).await
    }

    pub async fn hand_batch_with_capability_amendment(
        &self,
        emissions: Vec<ProviderEmission>,
        terminal: bool,
        amendment: &ProviderCapabilityAmendment,
    ) -> Result<Vec<ProviderHandoffOutcome>, String> {
        let emissions = emissions.into_iter().map(Into::into).collect::<Vec<_>>();
        if emissions.len() != 1 {
            return Err("a capability amendment requires one exact tool effect".into());
        }
        capability_amendment::validate_emission(amendment, &emissions[0])?;
        self.hand_batch_with_bindings(emissions, terminal, &[], true, None, Some(amendment)).await
    }

    async fn hand_batch_with_terminal(
        &self,
        emissions: Vec<ProviderHandoffEmission>,
        terminal: bool,
    ) -> Result<Vec<ProviderHandoffOutcome>, String> {
        self.hand_batch_with_bindings(emissions, terminal, &[], true, None, None).await
    }

    async fn hand_batch_with_bindings(
        &self,
        emissions: Vec<ProviderHandoffEmission>,
        terminal: bool,
        bindings: &[ProviderOutgoingBinding],
        consume_input: bool,
        independent_source: Option<u64>,
        amendment: Option<&ProviderCapabilityAmendment>,
    ) -> Result<Vec<ProviderHandoffOutcome>, String> {
        if emissions.is_empty() {
            return Err("provider response batch contains no emissions".to_string());
        }
        if let HandoffDeliveryMode::OneShot(plan) = &self.delivery_mode {
            if !bindings.is_empty() || amendment.is_some() {
                return Err("an outgoing binding requires a granted file-authoring continuation".into());
            }
            return self.hand_one_shot_batch(emissions, plan, terminal).await;
        }
        let batches = split_handoff_batches_by_artifact_name(emissions, &self.session_snapshot_labels)?;
        let batch_count = batches.len();
        let mut outcomes = Vec::new();
        for (index, batch) in batches.into_iter().enumerate() {
            let delivered = self.hand_rearm_batch_with_bindings(batch, terminal && index + 1 == batch_count, bindings, consume_input, independent_source.filter(|_| index == 0), amendment, None, None)
                .await?;
            let failed = delivered.iter().any(|outcome| matches!(outcome, ProviderHandoffOutcome::TerminalFailure { .. }));
            outcomes.extend(delivered);
            if failed {
                break;
            }
        }
        Ok(outcomes)
    }

    async fn hand_rearm_batch_with_bindings(
        &self,
        emissions: Vec<ProviderHandoffEmission>,
        terminal: bool,
        bindings: &[ProviderOutgoingBinding],
        consume_input: bool,
        independent_source: Option<u64>,
        amendment: Option<&ProviderCapabilityAmendment>,
        settlement_of: Option<(&ArtifactRef, u64)>,
        withdrawal: Option<&source_withdrawal::Withdrawal>,
    ) -> Result<Vec<ProviderHandoffOutcome>, String> {
        let mut state = self.state.lock().await;
        let withdrawal_guard = if withdrawal.is_some() { Some(self.dispatch_ownership().await?) } else { None };
        self.reconcile_checked_run_state(&mut state).await?;
        let mut active = self.clone();
        let record = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?;
        if let Some((predecessor, _)) = settlement_of {
            let prior = persisted_pending_handoff(&record, &self.config.provider_interface)?.ok_or("source settlement has no predecessor")?;
            if !self.source_carries_context || prior.items.len() != 1
                || !self.session_snapshot_labels.contains(&prior.items[0].artifact_label)
                || state.session_snapshot_head.as_ref().map(|head| &head.artifact) != Some(predecessor) {
                return Err("source settlement requires its exact currently selected parent snapshot".into());
            }
        }
        let topology: BoundRunAuthoringTopology = serde_json::from_value(
            record.interface_state.get(&self.config.provider_interface)
                .and_then(|state| state.get(RUN_AUTHORING_BINDING)).cloned()
                .ok_or("provider handoff lost its authored topology")?
        ).map_err(|error| error.to_string())?;
        topology.validate_identity(&record, &self.config.provider_interface)?;
        active.config.checkpoints = topology.checkpoints.clone();
        active.config.payload_artifact_bindings = topology.payload_artifact_bindings.clone();
        active.bound_topology = topology;
        let mut next_session_context = state.session_context.clone();
        let mut next_session_snapshot_head = state.session_snapshot_head.clone();
        let mut next_context = state.context_artifacts.clone();
        let mut next_static_context = state.static_context_artifacts.clone();
        let contributed_context_labels = active.contributed_context_output_labels();
        for binding in bindings {
            for artifact in &binding.artifact_bindings {
                append_artifact(&mut next_context, artifact.clone());
                append_artifact(&mut next_static_context, artifact.clone());
            }
        }
        let mut prepared = Vec::with_capacity(emissions.len());
        for emission in emissions {
            let ProviderHandoffEmission {
                mut artifact,
                route,
            } = emission;
            let manifest = artifact.read_manifest().map_err(|error| {
                format!("provider output artifact manifest cannot be read: {error}")
            })?;
            let context_output = contributed_context_labels.contains(&manifest.label);
            let snapshot_output = active.source_carries_context && active.session_snapshot_labels.contains(&manifest.label);
            let payload = if context_output {
                Some(artifact.read_payload_bytes().map_err(|error| {
                    format!("provider output artifact payload cannot be read: {error}")
                })?)
            } else {
                None
            };
            if snapshot_output {
                artifact = materialize_session_snapshot(
                    &artifact,
                    &next_session_context,
                    next_session_snapshot_head.as_ref(),
                )?;
                next_context = next_static_context.clone();
            }
            let identity = artifact_identity(&artifact, &active.var_dir)?;
            if context_output {
                append_session_payload(
                    &mut next_session_context,
                    &manifest.label,
                    payload
                        .as_deref()
                        .ok_or_else(|| "provider context output lost its payload".to_string())?,
                    &identity.artifact,
                    None,
                    "provider output",
                )?;
                append_artifact(&mut next_context, identity.artifact.id.clone());
            }
            if snapshot_output {
                next_session_snapshot_head = Some(session_snapshot_head_after_output(
                    identity.artifact.clone(),
                    &next_session_context,
                )?);
            }
            let output_checkpoint = active.config.output_checkpoint(
                &route,
                &active.granted_tools,
                &active.granted_interfaces,
                &manifest.label,
            )?;
            let verified_inputs = active.config.verified_input_checkpoints(
                &route,
                &output_checkpoint,
                &active.granted_mark_outcomes,
                &active.granted_tools,
                &active.granted_interfaces,
            )?;
            prepared.push(PreparedHandoff {
                identity,
                output_checkpoint,
                verified_inputs,
            });
        }
        let additional_input = independent_source.map(|source| active.independent_input_at_source(&state.run_record_bytes, source)).transpose()?.flatten();
        if let (Some(own), Some(additional)) = (&state.consumed_context_free_independent_input, &additional_input) {
            if own != additional { return Err("one response cannot consume two independent input contracts".into()); }
        }
        let independent_input = additional_input.as_ref().or(state.consumed_context_free_independent_input.as_ref());
        let context_changed = next_context != state.context_artifacts;
        let refresh_context = (consume_input && state.consumed_input) || context_changed || amendment.is_some();
        let mut authored = active.author_run_with_bindings(
            &state.run_record_bytes,
            &next_context,
            &prepared,
            refresh_context,
            independent_input,
            consume_input && state.consumed_verified_input,
            terminal,
            bindings,
        )?;
        if let Some(amendment) = amendment {
            let topology = capability_amendment::apply_to_authored_run(
                &state.run_record_bytes, &mut authored, amendment, &active.config, &active.var_dir,
            )?;
            active.config.checkpoints = topology.checkpoints.clone();
            active.config.payload_artifact_bindings = topology.payload_artifact_bindings.clone();
            active.bound_topology = topology;
            let amended = RunRecord::from_bytes(authored.content.as_bytes()).map_err(|error| error.to_string())?;
            extend_amendment_context_bindings(
                &amended, &active.config.provider_interface, &mut next_context, &mut next_static_context,
            )?;
        }
        if let Some((predecessor, context_generation)) = settlement_of {
            active.attach_source_settlement(&state.run_record_bytes, &mut authored, predecessor, context_generation)?;
        }
        if let Some(proof) = withdrawal {
            active.attach_source_withdrawal(&state.run_record_bytes, &mut authored, proof)?;
            state.validated_withdrawal_output = Some(source_withdrawal::closure_ref(&authored.pending_handoff)?);
        }
        let pending = authored.pending_handoff.clone();
        match active.install_authored_run(&mut state, &authored).await? {
            AuthoredRunInstallOutcome::Installed => {}
            AuthoredRunInstallOutcome::TerminalFailure(outcome) => {
                return Ok(vec![outcome]);
            }
        }
        if withdrawal.is_some() { active.validate_installed_withdrawal(&state.run_record_bytes)?; }
        drop(withdrawal_guard);
        if consume_input {
            state.consumed_input = false;
        }
        state.consumed_context_free_independent_input = None;
        if terminal {
            state.consumed_verified_input = false;
        }
        let outcomes = active.hand_pending_items(&pending, &state.run_record_bytes).await?;
        Self::ground_admitted_context_outputs(
            &mut next_session_context,
            &pending,
            &outcomes,
            &contributed_context_labels,
        )?;
        state.static_context_artifacts = next_static_context;
        state.session_context = next_session_context;
        state.session_snapshot_head = next_session_snapshot_head;
        active.publish_admitted_context(&mut state).await?;
        if refresh_context {
            state.context_artifacts = current_context_artifacts(
                &RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?, &active.config,
            )?;
        }
        Ok(outcomes)
    }

    async fn reconcile_checked_run_state(&self, state: &mut HandoffState) -> Result<(), String> {
        let has_owner = self
            .config
            .checkpoints
            .iter()
            .any(|cp| cp.owner_authoring.is_some());
        if !has_owner && self.config.payload_artifact_bindings.is_empty() {
            let current = octopus_interfaces::read_artifact_carrier_file_bounded(
                &self.run_path, self.ctx.required_host_io_limits()?.max_payload_bytes,
            ).map_err(|error| error.to_string())?;
            let current = RunRecord::from_bytes(&current).map_err(|error| error.to_string())?;
            if payload_artifact_bindings(&current, &self.config.provider_interface)?.is_empty() {
                return Ok(());
            }
        }
        {
            let after_seq = state
                .run_record_evidence_seq
                .ok_or("selected provider lost its admitted run snapshot provenance")?;
            let (current, chain) =
                await_checked_evidence(&self.ctx, self.config.result_timeout_ms, || {
                    checked_live_run_advance(
                        &self.ctx,
                        &self.run_path,
                        &state.run_record_bytes,
                        after_seq,
                        &self.config,
                    )
                })
                .await?;
            if current != state.run_record_bytes {
                let record = RunRecord::from_bytes(&current).map_err(|error| error.to_string())?;
                let mut advanced = state.clone();
                let before = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?;
                let selected = payload_artifact_bindings(&record, &self.config.provider_interface)?;
                let classified = payload_artifact_bindings(&before, &self.config.provider_interface)?.into_iter()
                    .chain(selected.iter().cloned())
                    .map(|reference| reference.id).collect::<BTreeSet<_>>();
                if !classified.is_empty() {
                    let readable = provider_context_bindings(&record, &self.config.provider_interface)?.into_iter()
                        .filter(|binding| binding.access.read).map(|binding| binding.id).collect::<BTreeSet<_>>();
                    // Attachment classification is not a future read grant. Keep
                    // non-payload static sources and the original invocation intact.
                    advanced.static_context_artifacts.retain(|id| !classified.contains(id) || readable.contains(id));
                    advanced.context_artifacts.retain(|id| !classified.contains(id) || readable.contains(id));
                    for reference in selected.iter().filter(|reference| readable.contains(&reference.id)) {
                        append_artifact(&mut advanced.static_context_artifacts, reference.id.clone());
                        append_artifact(&mut advanced.context_artifacts, reference.id.clone());
                    }
                }
                if let Some(last) = chain.last() {
                    advanced.run_record_evidence_seq = Some(last.call.seq.0);
                }
                advanced.run_record_bytes = current;
                self.restore_checked_context(&mut advanced, &record, &chain)?;
                *state = advanced;
            }
        }
        Ok(())
    }

    async fn hand_one_shot_batch(
        &self,
        emissions: Vec<ProviderHandoffEmission>,
        plan: &OneShotHandoffPlan,
        terminal: bool,
    ) -> Result<Vec<ProviderHandoffOutcome>, String> {
        let mut state = self.state.lock().await;
        let completion_coordinate = RuntimeCoordinateKey::new(
            &self.config.run,
            &self.config.worker,
            &self.config.provider_interface,
            &plan.completion.checkpoint,
            plan.completion.epoch,
        );
        let completion = self.session.index()?.admitted_slot_artifacts(
            &completion_coordinate,
            &plan.completion.required_mark,
            &plan.completion.required_verifier,
        )?;
        if let Some(evidence) = completion.first() {
            self.validate_one_shot_completion_evidence(plan, evidence)?;
        }
        if !terminal
            && (!completion.is_empty() || !self.one_shot_terminal_evidence(plan)?.is_empty())
        {
            return Err("provider one-shot progress follows terminal evidence".into());
        }
        let mut next_session_context = state.session_context.clone();
        let mut next_session_snapshot_head = state.session_snapshot_head.clone();
        let mut next_context = state.context_artifacts.clone();
        let mut prepared = Vec::with_capacity(emissions.len());
        let mut checkpoints = HashSet::new();
        for emission in emissions {
            let ProviderHandoffEmission {
                mut artifact,
                route,
            } = emission;
            if route != ProviderHandoffRoute::ClientProjection {
                return Err(
                    "provider one-shot output batch contains a continuation route".to_string(),
                );
            }
            let manifest = artifact.read_manifest().map_err(|error| {
                format!("provider one-shot output manifest cannot be read: {error}")
            })?;
            let slot = plan.terminal.get(&manifest.label).ok_or_else(|| {
                format!(
                    "provider one-shot topology has no verifier-isolated output slot for '{}'",
                    manifest.label
                )
            })?;
            if !checkpoints.insert(slot.checkpoint.clone()) {
                return Err(format!(
                    "provider one-shot output batch spends checkpoint '{}' more than once",
                    slot.checkpoint.as_str()
                ));
            }
            let context_output = self.context_output_labels.contains(&manifest.label);
            let snapshot_output = self.session_snapshot_labels.contains(&manifest.label);
            artifact = self.prepare_one_shot_output(artifact, terminal)?;
            let payload = if context_output {
                Some(artifact.read_payload_bytes().map_err(|error| {
                    format!("provider one-shot output payload cannot be read: {error}")
                })?)
            } else {
                None
            };
            if snapshot_output {
                artifact = materialize_session_snapshot(
                    &artifact,
                    &next_session_context,
                    next_session_snapshot_head.as_ref(),
                )?;
                next_context = state.static_context_artifacts.clone();
            }
            let identity = artifact_identity(&artifact, &self.var_dir)?;
            if !completion.is_empty() {
                let coordinate = RuntimeCoordinateKey::new(
                    &self.config.run,
                    &self.config.worker,
                    &self.config.provider_interface,
                    &slot.checkpoint,
                    slot.epoch,
                );
                let admitted = self.session.index()?.admitted_slot_artifacts(
                    &coordinate,
                    &slot.required_mark,
                    &slot.required_verifier,
                )?;
                if !admitted
                    .iter()
                    .any(|evidence| evidence.artifact == identity.artifact)
                {
                    return Err(
                        "provider one-shot output would extend completed terminal evidence".into(),
                    );
                }
            }
            if context_output {
                append_session_payload(
                    &mut next_session_context,
                    &manifest.label,
                    payload.as_deref().ok_or_else(|| {
                        "provider one-shot context output lost its payload".to_string()
                    })?,
                    &identity.artifact,
                    None,
                    "provider one-shot output",
                )?;
                append_artifact(&mut next_context, identity.artifact.id.clone());
            }
            if snapshot_output {
                next_session_snapshot_head = Some(session_snapshot_head_after_output(
                    identity.artifact.clone(),
                    &next_session_context,
                )?);
            }
            prepared.push((artifact, slot.clone(), identity.artifact));
        }
        let mut outcomes = Vec::with_capacity(prepared.len());
        let mut previous_mark_seq = None;
        for (artifact, slot, _) in prepared {
            let evidence = self
                .hand_one_shot_artifact_evidence(artifact, &slot)
                .await?;
            if previous_mark_seq.is_some_and(|previous| previous >= evidence.arrival_seq) {
                return Err(
                    "provider one-shot output artifact order differs from ledger order".to_string(),
                );
            }
            previous_mark_seq = Some(evidence.mark_seq);
            outcomes.push(ProviderHandoffOutcome::Admitted {
                seq: evidence.arrival_seq,
                artifact: evidence.artifact.clone(),
            });
        }
        if terminal {
            let terminal_evidence = self.one_shot_terminal_evidence(plan)?;
            let completion_artifact =
                self.materialize_one_shot_completion_artifact(&terminal_evidence)?;
            let completion_evidence = self
                .hand_one_shot_artifact_evidence(completion_artifact, &plan.completion)
                .await?;
            self.validate_one_shot_completion_evidence(plan, &completion_evidence)?;
        }
        state.context_artifacts = next_context;
        state.session_context = next_session_context;
        state.session_snapshot_head = next_session_snapshot_head;
        state.consumed_input = false;
        state.consumed_context_free_independent_input = None;
        Ok(outcomes)
    }

    fn prepare_one_shot_output(
        &self,
        artifact: InterfaceArtifactFile,
        terminal: bool,
    ) -> Result<InterfaceArtifactFile, String> {
        let payload = artifact
            .read_payload_bytes()
            .map_err(|error| error.to_string())?;
        let value = serde_json::from_slice::<serde_json::Value>(&payload);
        if value
            .as_ref()
            .is_ok_and(|value| value.get(ONE_SHOT_PROGRESS_FIELD).is_some())
        {
            return Err(
                "provider output already contains reserved one-shot progress evidence".into(),
            );
        }
        if terminal {
            return Ok(artifact);
        }
        let mut value = value.map_err(|error| {
            format!("provider one-shot progress must be a JSON object: {error}")
        })?;
        let object = value
            .as_object_mut()
            .ok_or("provider one-shot progress must be a JSON object")?;
        object.insert(
            ONE_SHOT_PROGRESS_FIELD.into(),
            serde_json::to_value(OneShotProgressIdentity {
                provider_interface: self.config.provider_interface.clone(),
                run: self.config.run.clone(),
                source_movement_seq: self.source_movement_seq,
            })
            .map_err(|error| error.to_string())?,
        );
        let label = artifact
            .read_manifest()
            .map_err(|error| error.to_string())?
            .label;
        InterfaceArtifactFile::write(
            self.var_dir.join("one-shot-progress"),
            &label,
            &serde_json::to_vec(&value).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())
    }

    fn is_one_shot_progress(&self, payload: &[u8]) -> Result<bool, String> {
        let value = match serde_json::from_slice::<serde_json::Value>(payload) {
            Ok(value) => value,
            Err(_) => return Ok(false),
        };
        let Some(progress) = value.get(ONE_SHOT_PROGRESS_FIELD) else {
            return Ok(false);
        };
        let progress: OneShotProgressIdentity =
            serde_json::from_value(progress.clone()).map_err(|error| {
                format!("provider one-shot progress evidence is malformed: {error}")
            })?;
        if progress.provider_interface != self.config.provider_interface
            || progress.run != self.config.run
            || progress.source_movement_seq != self.source_movement_seq
        {
            return Err("provider one-shot progress evidence belongs to another invocation".into());
        }
        Ok(true)
    }

    async fn hand_one_shot_artifact(
        &self,
        artifact: InterfaceArtifactFile,
        slot: &OneShotHandoffSlot,
    ) -> Result<ProviderHandoffOutcome, String> {
        let evidence = self.hand_one_shot_artifact_evidence(artifact, slot).await?;
        Ok(ProviderHandoffOutcome::Admitted {
            seq: evidence.arrival_seq,
            artifact: evidence.artifact,
        })
    }

    async fn hand_one_shot_artifact_evidence(
        &self,
        artifact: InterfaceArtifactFile,
        slot: &OneShotHandoffSlot,
    ) -> Result<OneShotAdmittedArtifact, String> {
        let identity = artifact_identity(&artifact, &self.var_dir)?;
        let coordinate = RuntimeCoordinateKey::new(
            &self.config.run,
            &self.config.worker,
            &self.config.provider_interface,
            &slot.checkpoint,
            slot.epoch,
        );
        let existing = self.session.index()?.admitted_slot_artifacts(
            &coordinate,
            &slot.required_mark,
            &slot.required_verifier,
        )?;
        if let Some(admitted) = existing.first() {
            if admitted.artifact == identity.artifact {
                self.validate_one_shot_evidence_source(admitted)?;
                return Ok(admitted.clone());
            }
            return Err(format!(
                "provider one-shot checkpoint '{}' was already spent by artifact '{}'",
                slot.checkpoint.as_str(),
                admitted.artifact.id.as_str()
            ));
        }
        let verdict = self.ctx.hand_arrival(artifact, Vec::new()).await?;
        let admitted = self.session.index()?.admitted_slot_artifacts(
            &coordinate,
            &slot.required_mark,
            &slot.required_verifier,
        )?;
        match (verdict, admitted.first()) {
            (DeliveryVerdict::Admitted { seq }, Some(evidence)) => {
                if evidence.artifact != identity.artifact {
                    return Err(format!(
                        "provider one-shot checkpoint '{}' admitted artifact '{}' instead of '{}'",
                        slot.checkpoint.as_str(),
                        evidence.artifact.id.as_str(),
                        identity.artifact.id.as_str()
                    ));
                }
                if evidence.arrival_seq != seq {
                    return Err(format!(
                        "provider one-shot artifact '{}' verdict seq {} differs from exact arrival seq {}",
                        identity.artifact.id.as_str(),
                        seq,
                        evidence.arrival_seq
                    ));
                }
                self.validate_one_shot_evidence_source(evidence)?;
                Ok(evidence.clone())
            }
            (DeliveryVerdict::Refused { .. }, Some(evidence)) => {
                if evidence.artifact != identity.artifact {
                    return Err(format!(
                        "provider one-shot checkpoint '{}' admitted artifact '{}' instead of '{}'",
                        slot.checkpoint.as_str(),
                        evidence.artifact.id.as_str(),
                        identity.artifact.id.as_str()
                    ));
                }
                self.validate_one_shot_evidence_source(evidence)?;
                Ok(evidence.clone())
            }
            (DeliveryVerdict::Admitted { .. }, None) => Err(format!(
                "provider one-shot artifact '{}' was admitted without exact checkpoint/mark evidence",
                identity.artifact.id.as_str()
            )),
            (DeliveryVerdict::Refused { reason, detail }, None) => Err(format!(
                "provider one-shot artifact '{}' was refused as '{}': {}",
                identity.artifact.id.as_str(),
                reason.as_str(),
                detail
            )),
        }
    }

    fn validate_one_shot_evidence_source(
        &self,
        evidence: &OneShotAdmittedArtifact,
    ) -> Result<(), String> {
        if evidence.arrival_seq <= self.source_movement_seq {
            return Err(format!(
                "provider one-shot artifact '{}' arrival seq {} does not follow source movement {}",
                evidence.artifact.id.as_str(),
                evidence.arrival_seq,
                self.source_movement_seq
            ));
        }
        Ok(())
    }

    fn recover_authoring_attempt(
        &self,
        record: &RunRecord,
        record_bytes: &[u8],
    ) -> Result<AuthoringAttemptRecovery, String> {
        let recover_predecessors = !self.settled_input_definition(record, record_bytes)?;
        if self.config.writer_run.is_some() { return self.recover_writer_attempt(record, record_bytes, recover_predecessors); }
        let attempt = persisted_authoring_attempt(
            &self.ctx,
            &self.hand_in_root,
            record,
            record_bytes,
            &self.config,
            recover_predecessors,
        )?;
        Ok(match attempt {
            None => AuthoringAttemptRecovery::None,
            Some(attempt) if attempt.provider_source_movement_seq == self.source_movement_seq => {
                AuthoringAttemptRecovery::InFlight(attempt)
            }
            Some(attempt) => AuthoringAttemptRecovery::OtherSource(attempt.provider_source_movement_seq),
        })
    }

    fn recover_authoring_failure(
        &self,
        record: &RunRecord,
        record_bytes: &[u8],
    ) -> Result<Option<(u64, InterfaceFailureEnvelope)>, String> {
        if self.config.writer_run.is_some() { return Ok(None); }
        let template = self
            .config
            .checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
        let checkpoint = active_checkpoint_for_role(
            record,
            &self.config,
            RunAuthoringCheckpointRole::AuthoringResult,
        )?;
        let coordinate = RuntimeCoordinateKey::new(
            &record.id,
            &record.worker,
            &self.config.provider_interface,
            &checkpoint.id,
            checkpoint.epoch.0,
        );
        let mut failed = None;
        for evidence in self.session.index()?.outbound_artifacts(
            &coordinate,
            template.interface(),
            Some(self.source_movement_seq),
        )? {
            let (manifest, payload) = read_pinned_artifact(&self.ctx, &evidence.artifact)?;
            if manifest.label == self.config.file_write_result_label {
                let result: ToolResultArtifact =
                    serde_json::from_slice(&payload).map_err(|error| error.to_string())?;
                if result.label != manifest.label || result.tool != self.config.file_write_tool {
                    return Err(
                        "provider authoring result changed its declared tool or label".into(),
                    );
                }
                continue;
            }
            if failed.replace((evidence, manifest, payload)).is_some() {
                return Err(
                    "provider authoring checkpoint has repeated interface failure evidence".into(),
                );
            }
        }
        let Some((evidence, manifest, payload)) = failed else {
            return Ok(None);
        };
        let record_hash = ContentHash::of_bytes(record_bytes).to_hex();
        if evidence.run_record_hash.as_deref() != Some(&record_hash) {
            return Err(
                "provider recovery authoring-result evidence differs from its current checkpoint"
                    .into(),
            );
        }
        if !indexed_evidence_matches_checkpoint(&evidence, template)
            || !artifact_refs_match_bindings(
                &evidence.artifact_binding_refs,
                &checkpoint.artifact_bindings,
            )
        {
            return Err(
                "provider recovery authoring-result evidence differs from its current checkpoint"
                    .to_string(),
            );
        }
        let failure_label = format!("{}.failure", template.interface().as_str());
        if manifest.label != failure_label {
            return Err(format!(
                "provider recovery authoring-result checkpoint delivered unexpected artifact '{}'",
                manifest.label
            ));
        }
        let failure: InterfaceFailureEnvelope =
            serde_json::from_slice(&payload).map_err(|error| {
                format!("provider recovery authoring failure payload is malformed: {error}")
            })?;
        if failure.failure_class.trim().is_empty()
            || failure.failure_class.trim() != failure.failure_class
            || failure.source_movement_seq >= evidence.seq
        {
            return Err(
                "provider recovery authoring failure has invalid class or source movement coordinate"
                    .to_string(),
            );
        }
        if completed_authoring_failure_duplicate(
            &self.ctx,
            &self.session,
            record,
            record_bytes,
            &self.config,
            &failure,
            evidence.seq,
        )? {
            return Ok(None);
        }
        validate_authoring_failure_source(AuthoringFailureSourceValidation {
            ctx: &self.ctx,
            record,
            record_bytes,
            config: &self.config,
            failure: &failure,
            failure_seq: evidence.seq,
            after_seq: Some(self.source_movement_seq),
        })?;
        Ok(Some((evidence.seq, failure)))
    }

    fn authoring_success_evidence(
        &self,
        authored: &AuthoredRun,
        after_seq: Option<u64>,
    ) -> Result<Option<u64>, String> {
        authoring_success_evidence(&self.ctx, &self.session, &self.config, authored, after_seq)
    }

    pub async fn recover_source(&self) -> Result<ProviderSourceRecovery, String> {
        self.recover_source_with_outcomes()
            .await
            .map(|(recovery, _)| recovery)
    }

    pub async fn recover_source_with_outcomes(
        &self,
    ) -> Result<(ProviderSourceRecovery, Vec<ProviderHandoffOutcome>), String> {
        if let HandoffDeliveryMode::OneShot(plan) = &self.delivery_mode {
            let _state = self.state.lock().await;
            return self
                .recover_one_shot(plan)
                .await
                .map(|recovery| (recovery, Vec::new()));
        }
        let mut state = self.state.lock().await;
        let mut record = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| {
            format!(
                "provider run '{}' current recovery record is malformed: {error}",
                self.config.run.as_str()
            )
        })?;
        if let Some((failure_seq, failure)) =
            self.recover_authoring_failure(&record, &state.run_record_bytes)?
        {
            if self.checked_settlement_output.is_some() {
                return Err("checked settlement recovery cannot replace its closure with an authoring failure".into());
            }
            hand_authoring_terminal_failure(
                &self.ctx,
                &self.session,
                &record,
                &self.config,
                failure_seq,
                &failure,
                &state.session_context,
                state.session_snapshot_head.as_ref(),
                &self.session_snapshot_labels,
            )
            .await?;
            return Ok((ProviderSourceRecovery::Completed, Vec::new()));
        }
        let attempt = self.recover_authoring_attempt(&record, &state.run_record_bytes)?;
        if let AuthoringAttemptRecovery::OtherSource(owner) = attempt {
            if let Some(pending) = persisted_pending_handoff(&record, &self.config.provider_interface)?
                .filter(|pending| owner > self.source_movement_seq && pending.context_publication.is_none()) {
                let completed = if pending.source_movement_seq == self.source_movement_seq && pending.terminal {
                    validate_pending_items_are_active(&record, &self.config.provider_interface, &pending)?;
                    Some((ProviderSourceRecovery::Completed, self.admitted_replay_handoff(&pending)?))
                } else if pending.source_movement_seq > self.source_movement_seq {
                    let recovered = self.recover_overtaken_source(&state.run_record_bytes, &pending)?;
                    if recovered.0 == ProviderSourceRecovery::Completed {
                        self.restore_checked_context(&mut state, &record, &recovered.2)?;
                        Some((recovered.0, recovered.1))
                    } else { None }
                } else {
                    None
                };
                if let Some(completed) = completed {
                    self.restore_pending_context(&mut state, &record, &pending)?;
                    state.consumed_input = false;
                    state.consumed_context_free_independent_input = None;
                    state.consumed_verified_input = false;
                    return Ok(completed);
                }
            }
            return Err(format!(
                "provider authoring private attempt belongs to source movement {owner}, current movement is {}",
                self.source_movement_seq,
            ));
        }
        if let AuthoringAttemptRecovery::InFlight(attempt) = attempt {
            let withdrawal_guard = if attempt.authored.pending_handoff.withdrawal.is_some() { Some(self.dispatch_ownership().await?) } else { None };
            if let Some(prior) =
                persisted_pending_handoff(&record, &self.config.provider_interface)?
            {
                if prior.source_movement_seq > self.source_movement_seq {
                    self.recover_overtaken_source(&state.run_record_bytes, &prior)?;
                }
                if (prior.source_movement_seq < self.source_movement_seq && !prior.terminal)
                    || (prior.source_movement_seq == self.source_movement_seq && prior.terminal
                        && !context_publication::is_publication_completion(&prior, &attempt.authored.pending_handoff)
                        && !source_settlement::follows_completed_source(&prior, &attempt.authored.pending_handoff)
                        && !source_withdrawal::follows(&prior, &attempt.authored.pending_handoff))
                {
                    return Err(format!(
                        "provider authoring attempt for movement {} cannot supersede pending movement {} (terminal={})",
                        self.source_movement_seq, prior.source_movement_seq, prior.terminal
                    ));
                }
                if prior.source_movement_seq == self.source_movement_seq {
                    validate_pending_items_are_active(
                        &record,
                        &self.config.provider_interface,
                        &prior,
                    )?;
                }
                let withdrawing = source_withdrawal::follows(&prior, &attempt.authored.pending_handoff);
                let evidence = self.pending_handoff_evidence(&prior)?;
                if let Some((index, _)) = evidence
                    .iter()
                    .enumerate()
                    .find(|(_, admitted)| !withdrawing && admitted.is_none())
                {
                    return Err(format!(
                        "provider authoring attempt for movement {} cannot supersede pending artifact '{}' without admitted ledger evidence",
                        self.source_movement_seq,
                        prior.items[index].artifact_id.as_str()
                    ));
                }
                if prior.source_movement_seq == self.source_movement_seq && !withdrawing {
                    self.restore_pending_context(&mut state, &record, &prior)?;
                }
            }
            match self
                .install_authored_run(&mut state, &attempt.authored)
                .await?
            {
                AuthoredRunInstallOutcome::Installed => {
                    record = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| {
                        format!(
                            "provider run '{}' recovered authored record is malformed: {error}",
                            self.config.run.as_str()
                        )
                    })?;
                }
                AuthoredRunInstallOutcome::TerminalFailure(_) => {
                    return Ok((ProviderSourceRecovery::Completed, Vec::new()));
                }
            }
            if withdrawal_guard.is_some() { self.validate_installed_withdrawal(&state.run_record_bytes)?; }
            drop(withdrawal_guard);
        }
        let pending = persisted_pending_handoff(&record, &self.config.provider_interface)?;
        let Some(pending) = pending else {
            return Ok((ProviderSourceRecovery::Process, Vec::new()));
        };
        if pending.source_movement_seq < self.source_movement_seq {
            if !pending.terminal {
                return Err(format!(
                    "provider reader advanced to movement {} while movement {} retains a non-terminal pending handoff",
                    self.source_movement_seq, pending.source_movement_seq
                ));
            }
            let evidence = self.pending_handoff_evidence(&pending)?;
            if let Some((index, _)) = evidence
                .iter()
                .enumerate()
                .find(|(_, admitted)| admitted.is_none())
            {
                return Err(format!(
                    "provider reader advanced to movement {} while terminal pending artifact '{}' from movement {} has no admitted ledger evidence",
                    self.source_movement_seq,
                    pending.items[index].artifact_id.as_str(),
                    pending.source_movement_seq
                ));
            }
            let input_entries = take_unpublished_context_inputs(&mut state, self.source_movement_seq)?;
            if pending.context_publication.is_some() {
                self.publish_admitted_context(&mut state).await?;
            }
            let published = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?;
            self.restore_pending_context(&mut state, &published, &pending)?;
            restore_consumed_context_inputs(&mut state, input_entries)?;
            return Ok((ProviderSourceRecovery::Process, Vec::new()));
        }
        if pending.source_movement_seq > self.source_movement_seq {
            let recovered = self.recover_overtaken_source(&state.run_record_bytes, &pending)?;
            let input_entries = take_unpublished_context_inputs(&mut state, self.source_movement_seq)?;
            if pending.context_publication.is_some() {
                let mut publisher = self.clone();
                publisher.source_movement_seq = pending.source_movement_seq;
                publisher.publish_admitted_context(&mut state).await?;
            }
            let published = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?;
            self.restore_checked_context(&mut state, &published, &recovered.2)?;
            self.restore_pending_context(&mut state, &published, &pending)?;
            restore_consumed_context_inputs(&mut state, input_entries)?;
            if recovered.0 != ProviderSourceRecovery::Process {
                state.consumed_input = false;
                state.consumed_context_free_independent_input = None;
                if recovered.0 == ProviderSourceRecovery::Completed {
                    state.consumed_verified_input = false;
                }
            }
            return Ok((recovered.0, recovered.1));
        }
        if self.pending_handoff_evidence(&pending)?.iter().any(Option::is_none) {
            source_withdrawal::require_validated_output(&state, &pending)?;
        }
        validate_pending_items_are_active(&record, &self.config.provider_interface, &pending)?;
        if pending.items.is_empty() {
            return Err(format!(
                "provider pending handoff for movement {} contains no response artifacts",
                self.source_movement_seq
            ));
        }
        let outcomes = self.hand_pending_items(&pending, &state.run_record_bytes).await?;
        self.publish_admitted_context(&mut state).await?;
        let record = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?;
        self.restore_pending_context(&mut state, &record, &pending)?;
        state.consumed_input = false;
        state.consumed_context_free_independent_input = None;
        if pending.terminal {
            state.consumed_verified_input = false;
        }
        if pending.terminal {
            return Ok((ProviderSourceRecovery::Completed, outcomes));
        }
        Self::ground_admitted_context_outputs(
            &mut state.session_context,
            &pending,
            &outcomes,
            &self.contributed_context_output_labels(),
        )?;
        Ok((
            ProviderSourceRecovery::Interrupted {
                last_artifact_label: pending
                    .items
                    .last()
                    .map(|item| item.artifact_label.clone())
                    .ok_or_else(|| "provider pending handoff batch is empty".to_string())?,
            },
            outcomes,
        ))
    }

    fn checked_failed_source_contexts(
        &self,
        current: &[u8],
        observed_chain: &[CheckedRunTransition],
        requested: Option<u64>,
    ) -> Result<BTreeMap<u64, ResolvedSessionContext>, String> {
        if self.failed_source || !self.source_carries_context { return Ok(BTreeMap::new()); }
        let limits = self.ctx.required_host_io_limits()?;
        stable_run_observation(&self.run_path, current, limits.max_payload_bytes, || {
            let current_record = RunRecord::from_bytes(current).map_err(|error| error.to_string())?;
            let mut candidates = BTreeMap::new();
            if let Some(sequence) = requested {
                candidates.insert(sequence, None);
            } else {
                let mut seeds = vec![current_record.clone()];
                for transition in observed_chain {
                    seeds.push(RunRecord::from_bytes(&transition.before).map_err(|error| error.to_string())?);
                    seeds.push(RunRecord::from_bytes(&transition.after).map_err(|error| error.to_string())?);
                }
                let has_source = {
                    let mut index = self.session.index()?;
                    index.catch_up()?;
                    index.coordinate_outbounds.iter().any(|(coordinate, rows)| {
                        coordinate.run == self.config.run.as_str()
                            && coordinate.worker == self.config.worker.as_str()
                            && coordinate.interface == self.config.provider_interface.as_str()
                            && rows.iter().any(|row| row.seq == self.source_movement_seq)
                    })
                };
                if has_source {
                    let (_, source) = self.session.index()?.exact_provider_source_outbound(
                        &self.config.run, &self.config.worker, &self.config.provider_interface, self.source_movement_seq,
                    )?;
                    seeds.push(indexed_source_run_record(&self.ctx, &source, self.source_movement_seq)?);
                }
                for record in seeds {
                    if record.id != current_record.id || record.ee != current_record.ee
                        || record.worker != current_record.worker {
                        return Err("failed context history changed run scope".into());
                    }
                    if let Some(pending) = persisted_pending_handoff(&record, &self.config.provider_interface)? {
                        if pending.source_without_context && pending.terminal {
                            if candidates.insert(pending.source_movement_seq, Some(pending.clone()))
                                .is_some_and(|previous| previous.as_ref() != Some(&pending)) {
                                return Err("failed context source has conflicting terminal handoffs".into());
                            }
                        }
                    }
                }
            }
            let profile = RunAuthoringProfile {
                file_write_tool: self.config.file_write_tool.clone(),
                file_write_label: self.config.file_write_label.clone(),
                file_write_result_label: self.config.file_write_result_label.clone(),
                result_timeout_ms: self.config.result_timeout_ms,
            };
            let mut recovered = BTreeMap::new();
            for (sequence, expected_pending) in candidates {
                let (coordinate, source) = self.session.index()?.exact_provider_source_outbound(
                    &self.config.run, &self.config.worker, &self.config.provider_interface, sequence,
                )?;
                let hash = source.run_record_hash.as_deref().and_then(ContentHash::from_hex)
                    .ok_or("failed context source has no pinned run snapshot")?;
                let (before, original) = pinned_run_record(&self.ctx, &hash, "failed context source")?;
                let checkpoint_id = CheckpointId::new(&coordinate.checkpoint);
                let (config, _) = resolve_run_authoring_invocation(
                    &profile, &self.config.provider_interface, &self.config.run, &original, &checkpoint_id,
                )?;
                let checkpoint = original.checkpoint(&checkpoint_id)
                    .ok_or("failed context source lost its pinned checkpoint")?;
                let templates = config.checkpoints.iter().filter(|template| {
                    template.role.is_provider_input() && template.interface() == &source.source
                        && checkpoint_record_matches_template(checkpoint, template, &original.id)
                }).collect::<Vec<_>>();
                if templates.len() != 1 || !original.active_checkpoints.contains(&checkpoint_id)
                    || checkpoint.epoch.0 != coordinate.epoch
                    || !indexed_evidence_matches_checkpoint(&source, templates[0]) {
                    return Err("failed context source has no exact pinned input contract".into());
                }
                if !templates[0].include_context { continue; }
                if !config.checkpoints.iter().any(|template| template.owner_authoring.is_some()) {
                    return Err("failed context recovery has no declared checked owner writers".into());
                }
                let chain = checked_selected_run_advance_state(
                    &self.ctx, &self.run_path, &before, current, sequence, &config,
                )?;
                let mut terminal = None;
                let mut pending: Option<PendingHandoff> = None;
                let mut superseded_at = None;
                for (position, transition) in chain.iter().enumerate() {
                    let record = RunRecord::from_bytes(&transition.after).map_err(|error| error.to_string())?;
                    let next = persisted_pending_handoff(&record, &config.provider_interface)?;
                    if transition.writer.owner == config.provider_interface && next.as_ref().is_some_and(|next| {
                        next.source_movement_seq == sequence && next.terminal && next.source_without_context
                    }) {
                        if pending.as_ref().is_some_and(|pending| Some(pending) != next.as_ref()) {
                            return Err("checked failed context source changed its terminal handoff".into());
                        }
                        terminal.get_or_insert(position);
                        pending = next;
                    } else if terminal.is_some() && superseded_at.is_none()
                        && next.as_ref().is_none_or(|next| pending.as_ref().is_none_or(|pending| next.items != pending.items)) {
                        superseded_at = Some(transition.call.seq.0);
                    }
                }
                let terminal = terminal.ok_or("failed context source has no checked terminal failure write")?;
                let pending = pending.ok_or("failed context source lost its checked terminal failure")?;
                if expected_pending.as_ref().is_some_and(|expected| expected != &pending) {
                    return Err("observed failed handoff differs from its checked terminal failure".into());
                }
                let outcomes = self.admitted_replay_handoff(&pending)?;
                if outcomes.is_empty() || outcomes.iter().any(|outcome| match outcome {
                    ProviderHandoffOutcome::Admitted { seq, .. } => {
                        *seq <= chain[terminal].call.seq.0
                            || superseded_at.is_some_and(|superseded| *seq >= superseded)
                    }
                    _ => true,
                }) {
                    return Err("failed context terminal admission has invalid causality".into());
                }
                let evidence = CheckedRunEvidence {
                    movements_path: &self.ctx.movements_path, ledger_index_path: &self.ctx.ledger_index_path,
                    blobs_dir: &self.ctx.blobs_dir, max_control_frame_bytes: limits.max_control_frame_bytes,
                    max_manifest_bytes: limits.max_manifest_bytes, max_payload_bytes: limits.max_payload_bytes,
                };
                let correction = input_revision::checked_failed_source_static_context(
                    &evidence, &self.run_path, &chain, &config.provider_interface, &source,
                )?;
                let (context, _, _) = recover_source_session_context_with_static_correction(
                    &self.ctx.blobs_dir, limits, &self.session, &original, &config, sequence,
                    &self.context_output_labels, &self.session_snapshot_labels, correction.as_ref(),
                )?;
                recovered.insert(sequence, context);
            }
            Ok(recovered)
        }).map_err(Into::into)
    }

    fn restore_failed_context(
        &self, state: &mut HandoffState, chain: &[CheckedRunTransition],
    ) -> Result<(), String> {
        let recovered = self.checked_failed_source_contexts(&state.run_record_bytes, chain, None)?;
        if recovered.is_empty() { return Ok(()); }
        let mut next = state.clone();
        for (sequence, context) in recovered {
            let merged = merge_failed_source_context(&next.session_context.artifacts, &context.artifacts, sequence, Some(self.source_movement_seq))?;
            if session_artifacts_fingerprint(&merged)? != session_artifacts_fingerprint(&next.session_context.artifacts)? {
                next.session_snapshot_head = None;
            }
            next.session_context.artifacts = merged;
        }
        validate_session_context_evidence(&mut next.session_context, SessionContextEvidenceScope {
            blobs_dir: &self.ctx.blobs_dir, limits: self.ctx.required_host_io_limits()?,
            run: &self.config.run, provider: &self.config.provider_interface,
            context_output_labels: &self.context_output_labels, session_snapshot_labels: &self.session_snapshot_labels,
        }, &self.session)?;
        for entry in &next.session_context.artifacts {
            append_artifact(&mut next.context_artifacts, entry.source_artifact.id.clone());
        }
        *state = next;
        Ok(())
    }

    fn restore_checked_context(
        &self, state: &mut HandoffState, current: &RunRecord, chain: &[CheckedRunTransition],
    ) -> Result<(), String> {
        let mut next = state.clone();
        self.restore_replayed_context(&mut next, current, chain)?;
        self.restore_failed_context(&mut next, chain)?;
        *state = next;
        Ok(())
    }

    fn checked_context_publications(
        &self,
        chain: &[CheckedRunTransition],
        current: &[u8],
    ) -> Result<BTreeMap<String, u64>, String> {
        let mut publications = BTreeMap::new();
        let mut sources = BTreeSet::new();
        let profile = RunAuthoringProfile {
            file_write_tool: self.config.file_write_tool.clone(),
            file_write_label: self.config.file_write_label.clone(),
            file_write_result_label: self.config.file_write_result_label.clone(),
            result_timeout_ms: self.config.result_timeout_ms,
        };
        for transition in chain {
            let authored = RunRecord::from_bytes(&transition.after)
                .map_err(|error| error.to_string())?;
            let Some(mut pending) =
                persisted_pending_handoff(&authored, &self.config.provider_interface)?
            else {
                continue;
            };
            let authored_config = config_from_persisted_profile(&authored, &self.config.provider_interface, &profile)?;
            let visible = current_context_artifacts(&authored, &authored_config)?
                .into_iter().collect::<BTreeSet<_>>();
            pending.items.retain(|item| visible.contains(&item.artifact_id)
                && self.context_output_labels.contains(&item.artifact_label));
            self.extend_admitted_context_publications(
                &authored, &authored_config, &pending, &mut sources, &mut publications,
            )?;
        }
        for (_, context) in self.checked_failed_source_contexts(current, chain, None)? {
            for entry in context.artifacts {
                let sequence = entry.source_movement_seq.ok_or("recovered failed context has no admitted movement")?;
                if publications.insert(artifact_ref_key(&entry.source_artifact), sequence)
                    .is_some_and(|previous| previous != sequence)
                {
                    return Err("recovered failed context changed its published movement".into());
                }
            }
        }
        Ok(publications)
    }

    fn extend_admitted_context_publications(
        &self, record: &RunRecord, config: &RunAuthoringConfig, pending: &PendingHandoff,
        sources: &mut BTreeSet<u64>, publications: &mut BTreeMap<String, u64>,
    ) -> Result<(), String> {
        if pending.items.iter().any(|item| self.session_snapshot_labels.contains(&item.artifact_label))
            && !pending.source_without_context
            && sources.insert(pending.source_movement_seq)
            && !self.source_is_context_free(pending.source_movement_seq)?
        {
            let (context, _) = recover_source_session_context(
                &self.ctx, &self.session, record, config,
                pending.source_movement_seq, &self.context_output_labels,
                &self.session_snapshot_labels,
            )?;
            for entry in context.artifacts {
                let sequence = entry.source_movement_seq
                    .ok_or("checked context source has no admitted movement")?;
                if publications.insert(artifact_ref_key(&entry.source_artifact), sequence)
                    .is_some_and(|previous| previous != sequence)
                {
                    return Err("checked context source changed its admitted movement".into());
                }
            }
        }
        let evidence = self.pending_handoff_evidence(pending)?;
        for (item, sequence) in pending.items.iter().zip(evidence) {
            let sequence = sequence
                .ok_or("published context has no admitted provider output evidence")?;
            let (_, source) = load_pending_artifact(&self.var_dir, item)?;
            if publications.insert(artifact_ref_key(&source), sequence)
                .is_some_and(|previous| previous != sequence)
            {
                return Err("published context changed its admitted movement".into());
            }
        }
        Ok(())
    }

    fn restore_published_context(
        &self,
        state: &mut HandoffState,
        current: &RunRecord,
        authored: &RunRecord,
        publications: &BTreeMap<String, u64>,
    ) -> Result<(), String> {
        let Some(mut pending) = persisted_pending_handoff(authored, &self.config.provider_interface)? else {
            return Ok(());
        };
        let visible = current_context_artifacts(current, &self.config)?.into_iter().collect::<BTreeSet<_>>();
        pending.items.retain(|item| visible.contains(&item.artifact_id)
            && self.context_output_labels.contains(&item.artifact_label));
        if pending.items.is_empty() { return Ok(()); }
        if self.pending_handoff_evidence(&pending)?.iter().any(Option::is_none) {
            return Err("published context has no admitted provider output evidence".into());
        }
        self.restore_pending_context_with_publications(state, current, &pending, publications)
    }

    fn restore_pending_context(
        &self,
        state: &mut HandoffState,
        record: &RunRecord,
        pending: &PendingHandoff,
    ) -> Result<(), String> {
        self.restore_pending_context_with_publications(state, record, pending, &BTreeMap::new())
    }

    fn source_is_context_free(&self, source: u64) -> Result<bool, String> {
        if source == self.source_movement_seq {
            return Ok(!self.source_carries_context);
        }
        let (coordinate, evidence) = self.session.index()?.exact_provider_source_outbound(
            &self.config.run, &self.config.worker, &self.config.provider_interface, source,
        )?;
        let record = indexed_source_run_record(&self.ctx, &evidence, source)?;
        let checkpoint = record.checkpoint(&CheckpointId::new(&coordinate.checkpoint))
            .ok_or("context recovery source lost its admitted checkpoint")?;
        let profile = RunAuthoringProfile {
            file_write_tool: self.config.file_write_tool.clone(),
            file_write_label: self.config.file_write_label.clone(),
            file_write_result_label: self.config.file_write_result_label.clone(),
            result_timeout_ms: self.config.result_timeout_ms,
        };
        let (config, _) = resolve_run_authoring_invocation(
            &profile, &self.config.provider_interface, &self.config.run, &record, &checkpoint.id,
        )?;
        let templates = config.checkpoints.iter().filter(|template| {
            template.role.is_provider_input()
                && checkpoint_record_matches_template(checkpoint, template, &record.id)
        }).collect::<Vec<_>>();
        let [template] = templates.as_slice() else {
            return Err("context recovery source has no unique provider input contract".into());
        };
        if record.worker != self.config.worker
            || !record.active_checkpoints.contains(&checkpoint.id)
            || checkpoint.epoch.0 != coordinate.epoch
            || template.interface() != &evidence.source
            || !indexed_evidence_matches_checkpoint(&evidence, template)
        {
            return Err("context recovery source differs from its exact admitted checkpoint".into());
        }
        Ok(!template.include_context)
    }

    fn restore_pending_context_with_publications(
        &self,
        state: &mut HandoffState,
        record: &RunRecord,
        pending: &PendingHandoff,
        publications: &BTreeMap<String, u64>,
    ) -> Result<(), String> {
        let mut context = state.session_context.clone();
        let mut snapshot_head = state.session_snapshot_head.clone();
        let visible = current_context_artifacts(record, &self.config)?
            .into_iter().collect::<BTreeSet<_>>();
        for item in &pending.items {
            if !self.context_output_labels.contains(&item.artifact_label) {
                continue;
            }
            if pending.context_publication.is_none() && !visible.contains(&item.artifact_id)
                && (pending.source_without_context || self.source_is_context_free(pending.source_movement_seq)?)
            {
                continue;
            }
            let (artifact, source) = load_pending_artifact(&self.var_dir, item)?;
            let payload = artifact.read_payload_bytes().map_err(|error| {
                format!("provider recovered context payload cannot be read: {error}")
            })?;
            if self.session_snapshot_labels.contains(&item.artifact_label) {
                let mut recovered = resolve_session_snapshot_chain(
                    &self.ctx.blobs_dir,
                    self.ctx.required_host_io_limits()?,
                    &source,
                    &payload,
                    None,
                    &self.session_snapshot_labels,
                    &self.config.provider_interface,
                )?;
                let snapshot_artifact_count = recovered.len();
                let mut current_publications = std::borrow::Cow::Borrowed(publications);
                if !current_publications.contains_key(&artifact_ref_key(&source))
                    && {
                        let recovered_refs = recovered.iter().map(|entry| artifact_ref_key(&entry.source_artifact)).collect::<BTreeSet<_>>();
                        let existing_refs = context.artifacts.iter().map(|entry| artifact_ref_key(&entry.source_artifact)).collect::<BTreeSet<_>>();
                        !existing_refs.is_subset(&recovered_refs) && !recovered_refs.is_subset(&existing_refs)
                    }
                    && persisted_pending_handoff(record, &self.config.provider_interface)?.as_ref() == Some(pending)
                {
                    let mut owner = self.clone();
                    owner.source_carries_context = !self.source_is_context_free(pending.source_movement_seq)?;
                    owner.source_movement_seq = pending.source_movement_seq;
                    let chain = owner.completed_source_chain(&state.run_record_bytes)?;
                    current_publications = std::borrow::Cow::Owned(owner.checked_context_publications(&chain, &state.run_record_bytes)?);
                    self.extend_admitted_context_publications(
                        record, &self.config, pending, &mut BTreeSet::new(), current_publications.to_mut(),
                    )?;
                }
                if reconcile_recovered_session_snapshot(
                    &context.artifacts, &mut recovered, &current_publications, &source,
                )? {
                    context.artifacts = recovered;
                    snapshot_head = if context.artifacts.len() == snapshot_artifact_count {
                        Some(session_snapshot_head_after_output(source, &context)?)
                    } else {
                        None
                    };
                }
            } else {
                let payload: serde_json::Value = serde_json::from_slice(&payload)
                    .map_err(|error| format!("provider recovered output is not JSON: {error}"))?;
                if let Some(existing) = context
                    .artifacts
                    .iter()
                    .find(|entry| entry.source_artifact == source)
                {
                    if existing.label != item.artifact_label || existing.payload != payload {
                        return Err(format!(
                            "provider recovered output '{}' differs from existing session context",
                            source.id.as_str()
                        ));
                    }
                } else {
                    context.artifacts.push(SessionContextArtifact {
                        label: item.artifact_label.clone(),
                        payload,
                        source_artifact: source,
                        source_movement_seq: None,
                    });
                }
            }
        }
        validate_session_context_evidence(
            &mut context,
            SessionContextEvidenceScope {
                blobs_dir: &self.ctx.blobs_dir,
                limits: self.ctx.required_host_io_limits()?,
                run: &self.config.run,
                provider: &self.config.provider_interface,
                context_output_labels: &self.context_output_labels,
                session_snapshot_labels: &self.session_snapshot_labels,
            },
            &self.session,
        )?;
        if let Some(head) = snapshot_head.as_mut() {
            if context
                .artifacts
                .get(head.covered_artifact_count)
                .map(|entry| &entry.source_artifact)
                != Some(&head.artifact)
            {
                return Err(format!(
                    "provider recovered context snapshot '{}' expected at {}, found at {:?} among {} artifacts",
                    head.artifact.id, head.covered_artifact_count,
                    context.artifacts.iter().position(|entry| entry.source_artifact == head.artifact),
                    context.artifacts.len(),
                ));
            }
            head.covered_fingerprint =
                session_artifacts_fingerprint(&context.artifacts[..head.covered_artifact_count])?;
        }
        state.context_artifacts = current_context_artifacts(record, &self.config)?;
        if self.source_carries_context {
            // A prior publication predates this invocation's admitted read bindings.
            // Keep those static sources available to tools without inserting their
            // payloads into the recovered conversation snapshot.
            for artifact in &state.static_context_artifacts {
                append_artifact(&mut state.context_artifacts, artifact.clone());
            }
        }
        state.session_context = context;
        state.session_snapshot_head = snapshot_head;
        Ok(())
    }

    async fn recover_one_shot(
        &self,
        plan: &OneShotHandoffPlan,
    ) -> Result<ProviderSourceRecovery, String> {
        let completion_coordinate = RuntimeCoordinateKey::new(
            &self.config.run,
            &self.config.worker,
            &self.config.provider_interface,
            &plan.completion.checkpoint,
            plan.completion.epoch,
        );
        let completion = self.session.index()?.admitted_slot_artifacts(
            &completion_coordinate,
            &plan.completion.required_mark,
            &plan.completion.required_verifier,
        )?;
        if let Some(completion_evidence) = completion.first() {
            self.validate_one_shot_completion_evidence(plan, completion_evidence)?;
            return Ok(ProviderSourceRecovery::Completed);
        }

        let terminal = self.one_shot_terminal_evidence(plan)?;
        let dispatched = self.one_shot_dispatch_evidence(plan)?;
        Self::validate_one_shot_dispatch_order(&dispatched, &terminal)?;
        if !terminal.is_empty() {
            let completion_artifact = self.materialize_one_shot_completion_artifact(&terminal)?;
            let completion_evidence = self
                .hand_one_shot_artifact_evidence(completion_artifact, &plan.completion)
                .await?;
            self.validate_one_shot_completion_evidence(plan, &completion_evidence)?;
            return Ok(ProviderSourceRecovery::Completed);
        }
        match dispatched.last() {
            None => Ok(ProviderSourceRecovery::Process),
            Some(_) => Ok(ProviderSourceRecovery::Interrupted {
                last_artifact_label: PROVIDER_TRANSPORT_DISPATCH_LABEL.to_string(),
            }),
        }
    }

    fn materialize_one_shot_completion_artifact(
        &self,
        terminal: &[(String, OneShotAdmittedArtifact)],
    ) -> Result<InterfaceArtifactFile, String> {
        if terminal.is_empty() {
            return Err(
                "provider one-shot completion contains no terminal ledger evidence".to_string(),
            );
        }
        let completion = ProviderHandoffCompletion {
            label: PROVIDER_HANDOFF_COMPLETE_LABEL.to_string(),
            provider_interface: self.config.provider_interface.clone(),
            source_movement_seq: self.source_movement_seq,
            artifacts: terminal
                .iter()
                .map(|(_, evidence)| evidence.artifact.clone())
                .collect(),
        };
        let payload = serde_json::to_vec(&completion)
            .map_err(|error| format!("provider handoff completion did not encode: {error}"))?;
        InterfaceArtifactFile::write(
            self.var_dir.join("handoff-completion"),
            PROVIDER_HANDOFF_COMPLETE_LABEL,
            &payload,
        )
        .map_err(|error| {
            format!("provider handoff completion artifact could not be written: {error}")
        })
    }

    fn validate_one_shot_completion_evidence(
        &self,
        plan: &OneShotHandoffPlan,
        completion_evidence: &OneShotAdmittedArtifact,
    ) -> Result<(), String> {
        self.validate_one_shot_evidence_source(completion_evidence)?;
        let payload = read_pinned_artifact_payload(
            &self.ctx,
            &completion_evidence.artifact,
            PROVIDER_HANDOFF_COMPLETE_LABEL,
        )?;
        let completion = validate_handoff_completion_payload(
            &payload,
            &self.config.provider_interface,
            self.source_movement_seq,
        )?;
        let terminal = self.one_shot_terminal_evidence(plan)?;
        let admitted = terminal
            .iter()
            .map(|(_, evidence)| evidence.artifact.clone())
            .collect::<Vec<_>>();
        if completion.artifacts != admitted {
            let completion_set = completion
                .artifacts
                .iter()
                .map(artifact_ref_key)
                .collect::<BTreeSet<_>>();
            let admitted_set = admitted
                .iter()
                .map(artifact_ref_key)
                .collect::<BTreeSet<_>>();
            if completion_set != admitted_set {
                return Err(format!(
                    "provider one-shot completion artifact set differs from admitted terminal evidence: completion={completion_set:?}, admitted={admitted_set:?}"
                ));
            }
            return Err(
                "provider one-shot completion artifact order differs from ledger order".to_string(),
            );
        }
        let last_terminal_mark = terminal
            .last()
            .map(|(_, evidence)| evidence.mark_seq)
            .ok_or_else(|| {
                "provider one-shot completion contains no terminal ledger evidence".to_string()
            })?;
        if last_terminal_mark >= completion_evidence.arrival_seq {
            return Err(
                "provider one-shot completion evidence does not follow every terminal mark"
                    .to_string(),
            );
        }
        let dispatched = self.one_shot_dispatch_evidence(plan)?;
        Self::validate_one_shot_dispatch_order(&dispatched, &terminal)
    }

    fn validate_one_shot_dispatch_order(
        dispatched: &[OneShotAdmittedArtifact],
        terminal: &[(String, OneShotAdmittedArtifact)],
    ) -> Result<(), String> {
        if let Some(first_terminal_arrival) =
            terminal.first().map(|(_, evidence)| evidence.arrival_seq)
        {
            if dispatched
                .iter()
                .any(|evidence| evidence.mark_seq >= first_terminal_arrival)
            {
                return Err(
                    "provider one-shot transport dispatch evidence does not precede every terminal arrival"
                        .to_string(),
                );
            }
        }
        Ok(())
    }

    fn one_shot_terminal_evidence(
        &self,
        plan: &OneShotHandoffPlan,
    ) -> Result<Vec<(String, OneShotAdmittedArtifact)>, String> {
        let mut raw = Vec::new();
        let mut index = self.session.index()?;
        for (label, slot) in &plan.terminal {
            let coordinate = RuntimeCoordinateKey::new(
                &self.config.run,
                &self.config.worker,
                &self.config.provider_interface,
                &slot.checkpoint,
                slot.epoch,
            );
            for evidence in index.admitted_slot_artifacts(
                &coordinate,
                &slot.required_mark,
                &slot.required_verifier,
            )? {
                raw.push((label.clone(), slot.checkpoint.clone(), evidence));
            }
        }
        drop(index);
        let mut terminal = Vec::with_capacity(raw.len());
        let mut artifacts = HashSet::new();
        for (label, checkpoint, evidence) in raw {
            self.validate_one_shot_evidence_source(&evidence)?;
            let expected_id = format!("{label}:{}", evidence.artifact.content.to_hex());
            if evidence.artifact.id.as_str() != expected_id {
                return Err(format!(
                    "provider one-shot terminal checkpoint '{}' admitted artifact '{}' outside label '{}'",
                    checkpoint.as_str(),
                    evidence.artifact.id.as_str(),
                    label
                ));
            }
            let payload = read_pinned_artifact_payload(&self.ctx, &evidence.artifact, &label)?;
            if self.is_one_shot_progress(&payload)? {
                continue;
            }
            let key = artifact_ref_key(&evidence.artifact);
            if !artifacts.insert(key.clone()) {
                return Err(format!(
                    "provider one-shot terminal evidence repeats artifact '{key}'"
                ));
            }
            terminal.push((label, evidence));
        }
        terminal.sort_by_key(|(_, evidence)| evidence.arrival_seq);
        let mut previous_mark_seq = None;
        for (_, evidence) in &terminal {
            if previous_mark_seq.is_some_and(|previous| previous >= evidence.arrival_seq) {
                return Err(
                    "provider one-shot terminal evidence overlaps in ledger order".to_string(),
                );
            }
            previous_mark_seq = Some(evidence.mark_seq);
        }
        Ok(terminal)
    }

    fn one_shot_dispatch_evidence(
        &self,
        plan: &OneShotHandoffPlan,
    ) -> Result<Vec<OneShotAdmittedArtifact>, String> {
        let mut dispatched = Vec::new();
        let mut index = self.session.index()?;
        for (attempt, slot) in &plan.dispatch {
            let coordinate = RuntimeCoordinateKey::new(
                &self.config.run,
                &self.config.worker,
                &self.config.provider_interface,
                &slot.checkpoint,
                slot.epoch,
            );
            for evidence in index.admitted_slot_artifacts(
                &coordinate,
                &slot.required_mark,
                &slot.required_verifier,
            )? {
                self.validate_one_shot_evidence_source(&evidence)?;
                let payload = read_pinned_artifact_payload(
                    &self.ctx,
                    &evidence.artifact,
                    PROVIDER_TRANSPORT_DISPATCH_LABEL,
                )?;
                let dispatch = validate_transport_dispatch_payload(
                    &payload,
                    &self.ctx.interface,
                    self.source_movement_seq,
                )?;
                if dispatch.attempt != *attempt {
                    return Err(format!(
                        "provider one-shot dispatch checkpoint '{}' proves attempt {} instead of {}",
                        slot.checkpoint.as_str(),
                        dispatch.attempt,
                        attempt
                    ));
                }
                dispatched.push(evidence);
            }
        }
        drop(index);
        dispatched.sort_by_key(|evidence| evidence.arrival_seq);
        let mut previous_mark_seq = None;
        for evidence in &dispatched {
            if previous_mark_seq.is_some_and(|previous| previous >= evidence.arrival_seq) {
                return Err(
                    "provider one-shot transport dispatch evidence overlaps in ledger order"
                        .to_string(),
                );
            }
            previous_mark_seq = Some(evidence.mark_seq);
        }
        Ok(dispatched)
    }

    async fn install_authored_run(
        &self,
        state: &mut HandoffState,
        authored: &AuthoredRun,
    ) -> Result<AuthoredRunInstallOutcome, String> {
        self.require_settlement_output_pin(&authored.pending_handoff)?;
        source_withdrawal::require_validated_output(state, &authored.pending_handoff)?;
        if self.config.writer_run.is_some() { return self.install_with_writer_run(state, authored).await; }
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(self.config.result_timeout_ms))
            .ok_or("provider authoring deadline overflow")?;
        let mut planned = authored.clone();
        let current_record = RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| {
            format!(
                "provider run '{}' current authoring record is malformed: {error}",
                self.config.run.as_str()
            )
        })?;
        let mut call = match self
            .recover_authoring_attempt(&current_record, &state.run_record_bytes)?
        {
            AuthoringAttemptRecovery::InFlight(existing) => {
                if existing.authored != *authored {
                    return Err(format!(
                        "provider run '{}' authoring checkpoint already carries a different exact attempt",
                        self.config.run.as_str()
                    ));
                }
                if self.authoring_success_evidence(authored, None)?.is_some() {
                    if self
                        .config
                        .checkpoints
                        .iter()
                        .any(|cp| cp.owner_authoring.is_some())
                    {
                        state.run_record_evidence_seq =
                            Some(self.authoring_success_evidence(authored, None)?.ok_or(
                                "selected provider lost its actual admitted authoring result",
                            )?);
                    }
                    state.run_record_bytes = authored.content.as_bytes().to_vec();
                    return Ok(AuthoredRunInstallOutcome::Installed);
                }
                InterfaceArtifactFile::new(existing.manifest_path).map_err(|error| {
                    format!(
                        "provider run '{}' persisted authoring call is invalid: {error}",
                        self.config.run.as_str()
                    )
                })?
            }
            AuthoringAttemptRecovery::None => self.materialize_file_write_call(authored)?,
            AuthoringAttemptRecovery::OtherSource(owner) => return Err(format!(
                "provider authoring private attempt belongs to source movement {owner}, current movement is {}",
                self.source_movement_seq,
            )),
        };
        loop {
            if Instant::now() >= deadline {
                return Err("provider checked authoring retry timed out".into());
            }
            let authored = &planned;
            let outcome = if let Some(rejected) =
                authoring_retry::rejected_write(&self.ctx, &self.config, authored)?
            {
                AuthoringWriteOutcome::Rejected(rejected)
            } else {
                let manifest_bytes =
                    tokio::fs::read(call.manifest_path())
                        .await
                        .map_err(|error| {
                            format!(
                                "interface authoring manifest '{}' cannot be read: {error}",
                                call.manifest_path().display()
                            )
                        })?;
                let admitted_seq = match self.send_authoring_manifest(&manifest_bytes).await? {
                    DeliveryVerdict::Admitted { seq } => Some(seq),
                    DeliveryVerdict::Refused { reason, .. }
                        if reason == octopus_interfaces::RefusedReason::CheckpointSpent
                            || reason == octopus_interfaces::RefusedReason::VerificationFailed =>
                    {
                        None
                    }
                    DeliveryVerdict::Refused { reason, detail } => {
                        return Err(format!(
                            "provider run authoring was refused as '{}': {}",
                            reason.as_str(),
                            detail
                        ));
                    }
                };
                self.wait_for_write_result(
                    admitted_seq,
                    &state.run_record_bytes,
                    authored,
                    deadline,
                )
                .await?
            };
            match outcome {
                AuthoringWriteOutcome::Written => {
                    if self
                        .config
                        .checkpoints
                        .iter()
                        .any(|cp| cp.owner_authoring.is_some())
                    {
                        state.run_record_evidence_seq =
                            Some(self.authoring_success_evidence(authored, None)?.ok_or(
                                "selected provider lost its actual admitted authoring result",
                            )?);
                    }
                    state.run_record_bytes = authored.content.as_bytes().to_vec();
                    return Ok(AuthoredRunInstallOutcome::Installed);
                }
                AuthoringWriteOutcome::Rejected(rejected) => {
                    let remaining = deadline
                        .saturating_duration_since(Instant::now())
                        .as_millis()
                        .min(u64::MAX as u128) as u64;
                    let before = if ContentHash::of_bytes(&state.run_record_bytes).to_hex()
                        == authored.before_hash
                    {
                        state.run_record_bytes.clone()
                    } else {
                        let hash = ContentHash::from_hex(&authored.before_hash)
                            .ok_or("rejected authoring preimage is malformed")?;
                        pinned_run_record(&self.ctx, &hash, "rejected authoring")?.0
                    };
                    let (current, chain) = await_checked_evidence(&self.ctx, remaining, || {
                        checked_live_run_advance(
                            &self.ctx,
                            &self.run_path,
                            &before,
                            authored.pending_handoff.source_movement_seq,
                            &self.config,
                        )
                    })
                    .await?;
                    if chain.is_empty()
                        || chain.iter().any(|transition| {
                            transition.writer.owner == self.config.provider_interface && !input_revision::checked_revision(transition)
                        })
                    {
                        return Err(format!(
                            "provider file.write rejected at {} without a proven independent advance: {}",
                            rejected.result_seq,
                            rejected.description()
                        ));
                    }
                    planned = authoring_retry::rebase_authored_run(
                        &before,
                        &current,
                        authored,
                        &self.config,
                        Some(JsonFormat::Compact),
                    )?;
                    state.run_record_evidence_seq = Some(chain.last().unwrap().call.seq.0);
                    state.run_record_bytes = current;
                    call = self.materialize_file_write_call(&planned)?;
                }
                AuthoringWriteOutcome::TerminalFailure {
                    failure_seq,
                    failure,
                } => {
                    let record =
                        RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| {
                            format!(
                        "provider run '{}' current authoring-failure record is malformed: {error}",
                        self.config.run.as_str()
                    )
                        })?;
                    let outcome = hand_authoring_terminal_failure(
                        &self.ctx,
                        &self.session,
                        &record,
                        &self.config,
                        failure_seq,
                        &failure,
                        &state.session_context,
                        state.session_snapshot_head.as_ref(),
                        &self.session_snapshot_labels,
                    )
                    .await?;
                    return Ok(AuthoredRunInstallOutcome::TerminalFailure(outcome));
                }
            }
        }
    }

    fn pending_handoff_evidence(
        &self,
        pending: &PendingHandoff,
    ) -> Result<Vec<Option<u64>>, String> {
        let mut evidence = Vec::with_capacity(pending.items.len());
        let mut missing = false;
        for item in &pending.items {
            let (_, expected_ref) = load_pending_artifact(&self.var_dir, item)?;
            let admitted =
                self.session
                    .index()?
                    .admitted_handoff_seq(PendingHandoffLedgerCoordinate {
                        interface: &self.ctx.interface,
                        run: &self.config.run,
                        worker: &self.config.worker,
                        artifact: &expected_ref,
                        required_evidence: pending_required_evidence(item)?,
                        checkpoint: &item.checkpoint.id,
                        epoch: item.checkpoint.epoch,
                    })?;
            if admitted.is_none() {
                missing = true;
            } else if missing {
                return Err(format!(
                    "provider pending handoff for movement {} has out-of-order admitted evidence at artifact '{}'",
                    pending.source_movement_seq,
                    item.artifact_id.as_str()
                ));
            }
            evidence.push(admitted);
        }
        Ok(evidence)
    }

    async fn hand_pending_items(
        &self,
        pending: &PendingHandoff,
        installed_record: &[u8],
    ) -> Result<Vec<ProviderHandoffOutcome>, String> {
        let _ownership = self.dispatch_ownership().await?;
        self.require_settlement_output_pin(pending)?;
        self.validate_dispatch_frontier(pending, installed_record)?;
        let evidence = self.pending_handoff_evidence(pending)?;
        let mut outcomes = Vec::with_capacity(pending.items.len());
        for (index, item) in pending.items.iter().enumerate() {
            let (artifact, expected_ref) = load_pending_artifact(&self.var_dir, item)?;
            if let Some(seq) = evidence[index] {
                outcomes.push(ProviderHandoffOutcome::Admitted {
                    seq,
                    artifact: expected_ref,
                });
                continue;
            }
            self.record_pending_dispatch(pending, item)?;
            let verdict = self.ctx.hand_arrival(artifact, Vec::new()).await?;
            let admitted =
                self.session
                    .index()?
                    .admitted_handoff_seq(PendingHandoffLedgerCoordinate {
                        interface: &self.ctx.interface,
                        run: &self.config.run,
                        worker: &self.config.worker,
                        artifact: &expected_ref,
                        required_evidence: pending_required_evidence(item)?,
                        checkpoint: &item.checkpoint.id,
                        epoch: item.checkpoint.epoch,
                    })?;
            match (verdict, admitted) {
                (DeliveryVerdict::Admitted { .. }, Some(seq)) => {
                    outcomes.push(ProviderHandoffOutcome::Admitted {
                        seq,
                        artifact: expected_ref,
                    });
                }
                (DeliveryVerdict::Admitted { .. }, None) => {
                    return Err(format!(
                        "provider pending handoff artifact '{}' was admitted without exact checkpoint evidence",
                        item.artifact_id.as_str()
                    ));
                }
                (DeliveryVerdict::Refused { .. }, Some(seq)) => {
                    outcomes.push(ProviderHandoffOutcome::Admitted {
                        seq,
                        artifact: expected_ref,
                    });
                }
                (DeliveryVerdict::Refused { reason, detail }, None) => {
                    return Err(format!(
                        "provider pending handoff artifact '{}' for movement {} was refused as '{}': {}",
                        item.artifact_id.as_str(),
                        self.source_movement_seq,
                        reason.as_str(),
                        detail
                    ));
                }
            }
        }
        Ok(outcomes)
    }

    fn ground_admitted_context_outputs(
        session_context: &mut ResolvedSessionContext,
        pending: &PendingHandoff,
        outcomes: &[ProviderHandoffOutcome],
        context_output_labels: &BTreeSet<String>,
    ) -> Result<(), String> {
        if pending.items.len() != outcomes.len() {
            return Err(
                "provider handoff outcomes do not match the pending artifact batch".to_string(),
            );
        }
        for (item, outcome) in pending.items.iter().zip(outcomes) {
            if !context_output_labels.contains(&item.artifact_label) {
                continue;
            }
            let seq = match outcome {
                ProviderHandoffOutcome::Admitted { seq, .. } => *seq,
                ProviderHandoffOutcome::TerminalFailure { .. } => {
                    return Err(format!(
                        "provider context output '{}' ended in an internal terminal failure",
                        item.artifact_id.as_str()
                    ));
                }
            };
            let content = ContentHash::from_hex(&item.artifact_content_hash).ok_or_else(|| {
                format!(
                    "provider context output '{}' has an invalid pending content hash",
                    item.artifact_id.as_str()
                )
            })?;
            let source = ArtifactRef {
                id: item.artifact_id.clone(),
                content,
            };
            let mut matched = None;
            for (index, artifact) in session_context.artifacts.iter().enumerate() {
                if artifact.source_artifact != source {
                    continue;
                }
                if matched.replace(index).is_some() {
                    return Err(format!(
                        "provider session context repeats handed output '{}'",
                        source.id.as_str()
                    ));
                }
            }
            let index = matched.ok_or_else(|| {
                format!(
                    "provider session context lost handed output '{}'",
                    source.id.as_str()
                )
            })?;
            let artifact = &mut session_context.artifacts[index];
            if artifact
                .source_movement_seq
                .is_some_and(|current| current != seq)
            {
                return Err(format!(
                    "provider session context output '{}' already names a different movement",
                    source.id.as_str()
                ));
            }
            artifact.source_movement_seq = Some(seq);
        }
        Ok(())
    }

    #[cfg(test)]
    fn author_run(
        &self,
        current: &[u8],
        context_artifacts: &[ArtifactId],
        outputs: &[PreparedHandoff],
        refresh_context: bool,
        consumed_context_free_independent_input: Option<&RunAuthoringCheckpoint>,
        consumed_verified_input: bool,
        terminal: bool,
    ) -> Result<AuthoredRun, String> {
        self.author_run_with_bindings(current, context_artifacts, outputs, refresh_context,
            consumed_context_free_independent_input, consumed_verified_input, terminal, &[])
    }

    fn author_run_with_bindings(
        &self,
        current: &[u8],
        context_artifacts: &[ArtifactId],
        outputs: &[PreparedHandoff],
        refresh_context: bool,
        consumed_context_free_independent_input: Option<&RunAuthoringCheckpoint>,
        consumed_verified_input: bool,
        terminal: bool,
        bindings: &[ProviderOutgoingBinding],
    ) -> Result<AuthoredRun, String> {
        let first_output = outputs
            .first()
            .ok_or_else(|| "provider cannot author an empty response batch".to_string())?;
        let record = RunRecord::from_bytes(current).map_err(|error| {
            format!(
                "provider run '{}' file '{}' is malformed: {error}",
                self.config.run.as_str(),
                self.run_path.display()
            )
        })?;
        if record.id != self.config.run || record.worker != self.config.worker {
            return Err(format!(
                "provider run topology drifted: file has run='{}' worker='{}', config requires run='{}' worker='{}'",
                record.id.as_str(),
                record.worker.as_str(),
                self.config.run.as_str(),
                self.config.worker.as_str()
            ));
        }
        let verified_input_carriers = outputs
            .iter()
            .filter(|output| !output.verified_inputs.is_empty())
            .collect::<Vec<_>>();
        if !verified_input_carriers.is_empty() && !terminal {
            return Err(
                "provider must author verified inputs in the terminal batch before exposing their request artifact"
                    .to_string(),
            );
        }
        if verified_input_carriers.len() > 1 {
            return Err(
                "provider response batch declares verified inputs from more than one request artifact"
                    .to_string(),
            );
        }
        let mut next_topology = self.bound_topology.clone();
        for binding in bindings {
            if binding.interface.as_str().is_empty() || binding.interface == self.config.provider_interface
                || binding.artifact_label.is_empty() || binding.artifact_label.trim() != binding.artifact_label {
                return Err("provider outgoing binding has an invalid exact interface or artifact label".into());
            }
            let policy = BindingArtifactPolicy::Allow {
                names: vec![binding.artifact_label.clone()], namespaces: Vec::new(),
            };
            let outbound = AuthoredInterfaceBinding {
                interface: binding.interface.clone(),
                config: InterfaceBindingConfig { artifact_policy: policy.clone() },
            };
            if next_topology.checkpoints.iter().any(|checkpoint| {
                checkpoint.role == RunAuthoringCheckpointRole::ProviderInterfaceOutput
                    && checkpoint.outbound_interface_bindings == [outbound.clone()]
            }) { continue; }
            let identity = serde_json::to_vec(binding).map_err(|error| error.to_string())?;
            next_topology.checkpoints.push(RunAuthoringCheckpoint {
                name: format!("outgoing_{}", ContentHash::of_bytes(&identity).to_hex()),
                role: RunAuthoringCheckpointRole::ProviderInterfaceOutput,
                static_artifact_bindings: binding.artifact_bindings.clone(), owner_authoring: None,
                interface_binding: AuthoredInterfaceBinding {
                    interface: self.config.provider_interface.clone(),
                    config: InterfaceBindingConfig { artifact_policy: policy },
                },
                outbound_interface_bindings: vec![outbound],
                include_context: false, required_verified_artifacts: Vec::new(),
            });
        }
        if terminal && consumed_verified_input {
            next_topology
                .checkpoints
                .retain(|checkpoint| checkpoint.role != RunAuthoringCheckpointRole::VerifiedInput);
        }
        if let Some(carrier) = verified_input_carriers.first() {
            if next_topology
                .checkpoints
                .iter()
                .any(|checkpoint| checkpoint.role == RunAuthoringCheckpointRole::VerifiedInput)
            {
                return Err(
                    "provider cannot expose a second operator request while another verified input remains outstanding"
                        .to_string(),
                );
            }
            next_topology
                .checkpoints
                .extend(carrier.verified_inputs.iter().cloned());
        }
        let mut next_config = self.config.clone();
        next_config.checkpoints = next_topology.checkpoints.clone();
        next_config.validate(&self.config.provider_interface, &self.config.run)?;
        let provider_state = record
            .interface_state
            .get(&self.config.provider_interface)
            .ok_or_else(|| {
                format!(
                    "provider run '{}' has no interface_state for '{}'",
                    record.id.as_str(),
                    self.config.provider_interface.as_str()
                )
            })?;
        let previous_frontier = {
            validate_bound_run_authoring_topology(&record, &self.config, None)?;
            if provider_state.contains_key("pending_handoff") {
                frontier_checkpoint_ids(provider_state)?
            } else if optional_u64(provider_state, "context_generation")? == 0 {
                input_revision::validate_initial_history(&self.ctx, &self.run_path, current, &self.config, self.source_movement_seq)?;
                raw_frontier_checkpoint_ids(provider_state)?
            } else {
                return Err(format!(
                    "provider run '{}' profile-derived frontier lost pending handoff evidence",
                    record.id.as_str()
                ));
            }
        };
        let mut mandatory_role_artifacts = current_mandatory_role_artifacts(&record, &self.config)?;
        for binding in bindings {
            for artifact in &binding.artifact_bindings {
                append_artifact(&mut mandatory_role_artifacts, artifact.clone());
            }
        }
        let previous_context_generation = optional_u64(provider_state, "context_generation")?;
        let authoring_generation = optional_u64(provider_state, "authoring_generation")?;
        let generation = authoring_generation.checked_add(1).ok_or_else(|| {
            format!(
                "provider run '{}' authoring generation overflowed",
                self.config.run.as_str()
            )
        })?;
        let batch_prefix =
            checkpoint_prefix(&record.id, &first_output.identity.artifact.id, generation);
        let mut target_context = mandatory_role_artifacts.clone();
        for artifact in context_artifacts { append_artifact(&mut target_context, artifact.clone()); }
        let mut live_context = target_context.clone();
        if target_context.iter().any(|id| outputs.iter().any(|output| &output.identity.artifact.id == id)) {
            live_context = mandatory_role_artifacts.clone();
            for artifact in current_context_artifacts(&record, &self.config)? { append_artifact(&mut live_context, artifact); }
            for artifact in context_artifacts.iter().filter(|id| !outputs.iter().any(|output| &output.identity.artifact.id == *id)) {
                append_artifact(&mut live_context, artifact.clone());
            }
        }
        let artifact_bindings = live_context
            .iter()
            .map(|artifact| serde_json::json!({ "id": artifact }))
            .collect::<Vec<_>>();
        let mandatory_role_bindings = mandatory_role_artifacts
            .iter()
            .map(|artifact| serde_json::json!({ "id": artifact }))
            .collect::<Vec<_>>();
        let authoring_call = self
            .config
            .checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringCall)?;
        let authoring_result = self
            .config
            .checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
        let authoring_result_checkpoint =
            CheckpointId::new(format!("{}_{}", batch_prefix, authoring_result.name));
        let mut next_frontier = Vec::with_capacity(3);
        let mut next_frontier_ids = Vec::with_capacity(3);
        let mut pending_items = Vec::with_capacity(outputs.len());
        for output in outputs {
            let output_prefix =
                checkpoint_prefix(&record.id, &output.identity.artifact.id, generation);
            let id = format!("{output_prefix}_{}", output.output_checkpoint.name);
            let checkpoint = pending_output_checkpoint(
                &output.output_checkpoint,
                &id,
                &output.identity.label,
                context_artifacts,
                &mandatory_role_artifacts,
            );
            next_frontier_ids.push(id);
            next_frontier.push(materialize_pending_checkpoint(&checkpoint, &record.id));
            pending_items.push(PendingHandoffItem {
                artifact_id: output.identity.artifact.id.clone(),
                artifact_content_hash: output.identity.artifact.content.to_hex(),
                artifact_label: output.identity.label.clone(),
                manifest_relative_path: output.identity.manifest_relative_path.clone(),
                checkpoint,
            });
        }
        for template in next_config.checkpoints.iter().filter(|template| template.role == RunAuthoringCheckpointRole::ProviderInterfaceOutput) {
            if !pending_items.iter().any(|item| item.checkpoint.interface_binding == template.interface_binding
                && item.checkpoint.outbound_interface_bindings == template.outbound_interface_bindings) {
                let id = format!("{batch_prefix}_{}",template.name);
                next_frontier_ids.push(id.clone());
                next_frontier.push(materialize_checkpoint(template,&id,&record.id,&mandatory_role_bindings));
            }
        }
        for checkpoint in [authoring_call, authoring_result].into_iter().filter(|_| self.config.writer_run.is_none()) {
            let id = format!("{batch_prefix}_{}", checkpoint.name);
            next_frontier_ids.push(id.clone());
            next_frontier.push(materialize_checkpoint(
                checkpoint,
                &id,
                &record.id,
                &mandatory_role_bindings,
            ));
        }
        let mut value: serde_json::Value = serde_json::from_slice(current).map_err(|error| {
            format!(
                "provider run '{}' raw JSON cannot be preserved: {error}",
                record.id.as_str()
            )
        })?;
        let previous_value = value.clone();
        let root = value.as_object_mut().ok_or_else(|| {
            format!(
                "provider run '{}' JSON root is not an object",
                record.id.as_str()
            )
        })?;
        root.insert(
            "status".to_string(),
            serde_json::json!(if terminal { "idle" } else { "running" }),
        );
        replace_owned_frontier(root, &previous_frontier, next_frontier, &next_frontier_ids)?;
        if refresh_context {
            replace_context_checkpoints(
                root,
                &self.config,
                &record.id,
                &batch_prefix,
                &artifact_bindings,
            )?;
        }
        if let Some(template) = consumed_context_free_independent_input {
            replace_consumed_independent_input_checkpoint(
                root,
                template,
                &record.id,
                &batch_prefix,
                &mandatory_role_bindings,
            )?;
        }
        if self.config.checkpoints.iter().filter(|row|row.role == RunAuthoringCheckpointRole::VerifiedInput)
            .ne(next_config.checkpoints.iter().filter(|row|row.role == RunAuthoringCheckpointRole::VerifiedInput)) {
            replace_verified_input_checkpoints(
                root,
                &self.config,
                &next_config,
                &record.id,
                &batch_prefix,
                &artifact_bindings,
            )?;
        }
        let interface_state = root
            .get_mut("interface_state")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| {
                format!(
                    "provider run '{}' interface_state is not an object",
                    record.id.as_str()
                )
            })?;
        let provider_state = interface_state
            .get_mut(self.config.provider_interface.as_str())
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| {
                format!(
                    "provider run '{}' interface_state for '{}' is not an object",
                    record.id.as_str(),
                    self.config.provider_interface.as_str()
                )
            })?;
        let context_generation = if refresh_context && !self.failed_source {
            previous_context_generation.checked_add(1).ok_or_else(|| {
                format!(
                    "provider run '{}' context generation overflowed",
                    self.config.run.as_str()
                )
            })?
        } else {
            previous_context_generation
        };
        provider_state.insert(
            "context_generation".to_string(),
            serde_json::json!(context_generation),
        );
        provider_state.insert(
            "authoring_generation".to_string(),
            serde_json::json!(generation),
        );
        provider_state.insert(
            "last_authored_artifact".to_string(),
            serde_json::json!(
                outputs
                    .last()
                    .map(|output| output.identity.artifact.id.clone())
                    .ok_or_else(|| "provider response batch became empty".to_string())?
            ),
        );
        let pending_batch = PendingHandoff {
            source_movement_seq: self.source_movement_seq,
            items: pending_items,
            terminal,
            context_publication: (live_context != target_context).then_some(target_context),
            source_without_context: self.failed_source,
            settlement_of: None,
            dispatch_fenced: true,
            withdrawal: None,
        };
        provider_state.insert(
            "pending_handoff".to_string(),
            serde_json::to_value(&pending_batch)
                .map_err(|error| format!("provider pending handoff did not encode: {error}"))?,
        );
        provider_state.insert(
            RUN_AUTHORING_BINDING.to_string(),
            serde_json::to_value(&next_topology).map_err(|error| {
                format!("provider run authoring binding did not encode: {error}")
            })?,
        );
        provider_state.insert(
            FRONTIER_CHECKPOINT_IDS.to_string(),
            serde_json::json!(next_frontier_ids),
        );
        let bytes = JsonFormat::Compact.encode(&value)?;
        let authored_record = RunRecord::from_bytes(&bytes).map_err(|error| {
            format!(
                "provider authored malformed run '{}': {error}",
                self.config.run.as_str()
            )
        })?;
        let authored_pending = pending_handoff(&authored_record, &self.config.provider_interface)?
            .ok_or_else(|| "provider authored run lost its pending handoff batch".to_string())?;
        if authored_pending != pending_batch {
            return Err("provider authored run changed its pending handoff batch".to_string());
        }
        validate_pending_handoff_delivery_isolation(&authored_record, &authored_pending)?;
        validate_persisted_profile_binding(&authored_record, &next_config, &next_topology)?;
        let content = String::from_utf8(bytes)
            .map_err(|error| format!("provider authored non-UTF8 run JSON: {error}"))?;
        let before_hash = ContentHash::of_bytes(current).to_hex();
        let after_hash = ContentHash::of_bytes(content.as_bytes()).to_hex();
        let patch = json_patch::diff(&previous_value, &value);
        if patch.is_empty() {
            return Err(format!(
                "provider run '{}' authoring produced no JSON changes",
                self.config.run.as_str()
            ));
        }
        let reconstructed = apply_run_json_patch(current, &patch, &after_hash, Some(JsonFormat::Compact))?;
        if reconstructed != content {
            return Err(format!(
                "provider run '{}' JSON patch did not reconstruct the exact authored bytes",
                self.config.run.as_str()
            ));
        }
        Ok(AuthoredRun {
            json_format: Some(JsonFormat::Compact),
            before_hash,
            after_hash,
            correlation_id: format!(
                "provider-run-authoring:{}:{}",
                self.config.run.as_str(),
                generation
            ),
            authoring_result_checkpoint,
            authoring_result_interface: authoring_result.interface().clone(),
            pending_handoff: pending_batch,
            content,
            patch,
        })
    }

    fn materialize_file_write_call(
        &self,
        authored: &AuthoredRun,
    ) -> Result<InterfaceArtifactFile, String> {
        let payload = serde_json::to_vec(&serde_json::json!({
            "tool": self.config.file_write_tool,
            "arguments": FileWriteCallArguments {
                path: self.run_path.clone(), patch: authored.patch.clone(),
                expected_before_hash: authored.before_hash.clone(),
                expected_after_hash: authored.after_hash.clone(), json_format: authored.json_format,
            },
            "correlation_id": authored.correlation_id
        }))
        .map_err(|error| format!("provider file.write call did not encode: {error}"))?;
        InterfaceArtifactFile::write(&self.hand_in_root, &self.config.file_write_label, &payload)
            .map_err(|error| format!("provider file.write artifact could not be written: {error}"))
    }

    async fn send_authoring_manifest(
        &self,
        manifest_bytes: &[u8],
    ) -> Result<DeliveryVerdict, String> {
        send_authoring_manifest(&self.ctx, &self.config, manifest_bytes).await
    }

    async fn wait_for_write_result(
        &self,
        admitted_seq: Option<u64>,
        current_run_record: &[u8],
        authored: &AuthoredRun,
        deadline: Instant,
    ) -> Result<AuthoringWriteOutcome, String> {
        let mut reader = self.result_reader.lock().await;
        let current_record = RunRecord::from_bytes(current_run_record).map_err(|error| {
            format!(
                "provider run '{}' current authoring record is malformed: {error}",
                self.config.run.as_str()
            )
        })?;
        let authoring_result = self
            .config
            .checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
        let failure_label = format!("{}.failure", authoring_result.interface().as_str());
        loop {
            if let Some(rejected) = authoring_retry::rejected_write(&self.ctx, &self.config, authored)? {
                return Ok(AuthoringWriteOutcome::Rejected(rejected));
            }
            if let Some((failure_seq, failure)) =
                self.recover_authoring_failure(&current_record, current_run_record)?
            {
                if admitted_seq.is_some_and(|admitted| failure.source_movement_seq != admitted) {
                    return Err(format!(
                        "provider run '{}' recovered authoring failure names movement {} instead of admitted call {}",
                        self.config.run.as_str(),
                        failure.source_movement_seq,
                        admitted_seq.unwrap_or_default()
                    ));
                }
                return Ok(AuthoringWriteOutcome::TerminalFailure {
                    failure_seq,
                    failure,
                });
            }
            if self
                .authoring_success_evidence(authored, admitted_seq)?
                .is_some()
            {
                return Ok(AuthoringWriteOutcome::Written);
            }
            let movements = reader.read_new()?;
            let mut outcome = None;
            for movement in &movements {
                if admitted_seq.is_some_and(|admitted| movement.movement.seq.0 < admitted) {
                    continue;
                }
                let envelope = reader.envelope(movement)?;
                let manifest = envelope.artifact_file.read_manifest().map_err(|error| {
                    format!(
                        "provider authoring result movement {} has malformed manifest: {error}",
                        movement.movement.seq.0
                    )
                })?;
                if manifest.label != self.config.file_write_result_label
                    && manifest.label != failure_label
                {
                    continue;
                }
                let payload = envelope.read_artifact_payload().map_err(|error| {
                    format!(
                        "provider authoring result movement {} payload cannot be read: {error}",
                        movement.movement.seq.0
                    )
                })?;
                if manifest.label == failure_label {
                    let failure = validate_authoring_failure_delivery(
                        &self.config.provider_interface,
                        &movement.movement,
                        &envelope,
                        &current_record,
                        &self.config,
                        authoring_result,
                        &payload,
                    )?;
                    if completed_authoring_failure_duplicate(
                        &self.ctx,
                        &self.session,
                        &current_record,
                        current_run_record,
                        &self.config,
                        &failure,
                        movement.movement.seq.0,
                    )? {
                        continue;
                    }
                    validate_authoring_failure_source(AuthoringFailureSourceValidation {
                        ctx: &self.ctx,
                        record: &current_record,
                        record_bytes: current_run_record,
                        config: &self.config,
                        failure: &failure,
                        failure_seq: movement.movement.seq.0,
                        after_seq: Some(self.source_movement_seq),
                    })?;
                    outcome = Some(AuthoringWriteOutcome::TerminalFailure {
                        failure_seq: movement.movement.seq.0,
                        failure,
                    });
                    break;
                }
                let result: FileWriteResultEnvelope =
                    serde_json::from_slice(&payload).map_err(|error| {
                        format!(
                            "provider authoring result movement {} payload is malformed: {error}",
                            movement.movement.seq.0
                        )
                    })?;
                if result.correlation_id.as_deref() != Some(&authored.correlation_id) { continue; }
                if !result.ok && result.error.as_ref().is_some_and(|error| error.details.is_none()) {
                    if let Some(rejected) = authoring_retry::rejected_write(&self.ctx, &self.config, authored)? {
                        outcome = Some(AuthoringWriteOutcome::Rejected(rejected));
                        break;
                    }
                    authoring_retry::validate_rejected_result(&self.ctx, &self.config, &movement.movement, &envelope, &current_record, &result)?;
                    continue;
                }
                if let Some(failed) =
                    file_write_failure_result(&result, &self.config, movement.movement.seq.0)?
                {
                    validate_authoring_failure_movement(
                        &self.config.provider_interface,
                        &movement.movement,
                        &envelope,
                        &current_record,
                        &self.config,
                        authoring_result,
                    )?;
                    if completed_authoring_failure_duplicate(
                        &self.ctx,
                        &self.session,
                        &current_record,
                        current_run_record,
                        &self.config,
                        &failed.failure,
                        movement.movement.seq.0,
                    )? {
                        continue;
                    }
                    let validated =
                        validate_authoring_failure_source(AuthoringFailureSourceValidation {
                            ctx: &self.ctx,
                            record: &current_record,
                            record_bytes: current_run_record,
                            config: &self.config,
                            failure: &failed.failure,
                            failure_seq: movement.movement.seq.0,
                            after_seq: Some(self.source_movement_seq),
                        })?;
                    if failed.correlation_id != validated.correlation_id {
                        return Err(format!(
                            "provider run '{}' failed authoring result names the wrong correlation_id",
                            self.config.run.as_str()
                        ));
                    }
                    outcome = Some(AuthoringWriteOutcome::TerminalFailure {
                        failure_seq: movement.movement.seq.0,
                        failure: failed.failure,
                    });
                    break;
                }
                if result.correlation_id.as_deref() != Some(&authored.correlation_id) {
                    continue;
                }
                self.validate_authoring_result_movement(&movement.movement, &envelope, authored)?;
                self.validate_write_result(result, authored)?;
                outcome = Some(AuthoringWriteOutcome::Written);
                break;
            }
            if !movements.is_empty() {
                reader.commit()?;
            }
            if let Some(outcome) = outcome {
                return Ok(outcome);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "provider run '{}' authoring result did not arrive within {}ms",
                    self.config.run.as_str(),
                    self.config.result_timeout_ms
                ));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            tokio::time::timeout(remaining, reader.wait_for_change(&self.ctx.cancel))
                .await
                .map_err(|_| {
                    format!(
                        "provider run '{}' authoring result did not arrive within {}ms",
                        self.config.run.as_str(),
                        self.config.result_timeout_ms
                    )
                })??;
        }
    }

    fn validate_authoring_result_movement(
        &self,
        movement: &Movement,
        envelope: &InterfaceInvocationEnvelope,
        authored: &AuthoredRun,
    ) -> Result<(), String> {
        if movement.kind != MovementKind::InterfaceOutbound
            || movement.from != Endpoint::Interface(authored.authoring_result_interface.clone())
            || movement.to != Endpoint::Interface(self.config.provider_interface.clone())
            || movement.artifact.as_ref() != Some(&envelope.artifact)
        {
            return Err(format!(
                "provider run '{}' authoring result has invalid movement provenance",
                self.config.run.as_str()
            ));
        }
        let runtime = envelope.runtime().map_err(|error| error.to_string())?;
        if runtime.run != self.config.run
            || runtime.worker != self.config.worker
            || runtime.interface != self.config.provider_interface
            || runtime.checkpoint != authored.authoring_result_checkpoint
            || runtime.epoch != 0
        {
            return Err(format!(
                "provider run '{}' authoring result has invalid runtime coordinate",
                self.config.run.as_str()
            ));
        }
        let expected = self
            .config
            .checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
        let metadata = envelope.metadata().map_err(|error| error.to_string())?;
        let authored_record =
            RunRecord::from_bytes(authored.content.as_bytes()).map_err(|error| {
                format!(
                    "provider run '{}' authored result record is malformed: {error}",
                    self.config.run.as_str()
                )
            })?;
        let authored_checkpoint = authored_record
            .checkpoint(&authored.authoring_result_checkpoint)
            .ok_or_else(|| {
                format!(
                    "provider run '{}' authored result checkpoint is absent",
                    self.config.run.as_str()
                )
            })?;
        if !metadata_matches_checkpoint(metadata, expected)
            || !artifact_refs_match_bindings(
                &metadata.artifact_binding_refs,
                &authored_checkpoint.artifact_bindings,
            )
        {
            return Err(format!(
                "provider run '{}' authoring result metadata differs from its authored checkpoint",
                self.config.run.as_str()
            ));
        }
        let pinned = self.ctx.pinned_run_record_bytes(envelope)?;
        if pinned != authored.content.as_bytes() {
            if !self.config.checkpoints.iter().any(|cp| cp.owner_authoring.is_some()) {
                return Err("provider authoring result did not pin the exact authored run bytes".into());
            }
            validate_unchanged_admitted_checkpoint(authored.content.as_bytes(), &pinned,
                &authored.authoring_result_checkpoint)?;
        }
        Ok(())
    }

    fn validate_write_result(
        &self,
        result: FileWriteResultEnvelope,
        authored: &AuthoredRun,
    ) -> Result<(), String> {
        self.validate_write_result_evidence(result, authored)?;
        let actual = std::fs::read(&self.run_path).map_err(|error| {
            format!(
                "provider run '{}' authored file '{}' cannot be reread: {error}",
                self.config.run.as_str(),
                self.run_path.display()
            )
        })?;
        if actual != authored.content.as_bytes() {
            if !self.config.checkpoints.iter().any(|cp| cp.owner_authoring.is_some()) {
                return Err("provider file.write result does not match the bytes on disk".into());
            }
            validate_unchanged_admitted_checkpoint(authored.content.as_bytes(), &actual,
                &authored.authoring_result_checkpoint)?;
        }
        RunRecord::from_bytes(&actual).map_err(|error| {
            format!(
                "provider run '{}' authored file is malformed after write: {error}",
                self.config.run.as_str()
            )
        })?;
        Ok(())
    }

    fn validate_write_result_evidence(
        &self,
        result: FileWriteResultEnvelope,
        authored: &AuthoredRun,
    ) -> Result<(), String> {
        validate_write_result_evidence(&self.config, &self.run_path, result, authored)
    }
}

fn validate_write_result_evidence(
    config: &RunAuthoringConfig,
    run_path: &Path,
    result: FileWriteResultEnvelope,
    authored: &AuthoredRun,
) -> Result<(), String> {
    validate_file_write_evidence(
        &RunAuthoringProfile {
            file_write_tool: config.file_write_tool.clone(),
            file_write_label: config.file_write_label.clone(),
            file_write_result_label: config.file_write_result_label.clone(),
            result_timeout_ms: config.result_timeout_ms,
        },
        run_path,
        result,
        &authored.before_hash,
        &authored.after_hash,
        authored.content.len(),
    )
}

async fn send_authoring_manifest(
    ctx: &InterfaceLiftContext,
    config: &RunAuthoringConfig,
    manifest_bytes: &[u8],
) -> Result<DeliveryVerdict, String> {
    let payload: serde_json::Value = serde_json::from_slice(manifest_bytes)
        .map_err(|error| format!("interface authoring manifest is malformed: {error}"))?;
    if !payload.is_object() {
        return Err("interface authoring manifest must be a JSON object".to_string());
    }
    let timeout = Duration::from_millis(config.result_timeout_ms);
    let control_root = ctx.root_slot("app_server_control_root")?;
    let socket =
        octopus_app_server_interface::interface_control_socket_path_from_root(
            control_root,
            &config.authoring_source_interface,
        );
    tokio::time::timeout(timeout, async move {
        let stream = tokio::net::UnixStream::connect(&socket)
            .await
            .map_err(|error| {
                format!(
                    "interface authoring socket '{}' cannot be connected: {error}",
                    socket.display()
                )
            })?;
        let (mut ws, _) = tokio_tungstenite::client_async("ws://localhost/rpc", stream)
            .await
            .map_err(|error| format!("interface authoring websocket upgrade failed: {error}"))?;
        let text = serde_json::to_string(&payload)
            .map_err(|error| format!("interface authoring manifest did not encode: {error}"))?;
        ws.send(WsMessage::Text(text.into()))
            .await
            .map_err(|error| format!("interface authoring send failed: {error}"))?;
        let reply = ws
            .next()
            .await
            .ok_or_else(|| {
                "interface authoring socket closed before the delivery verdict".to_string()
            })?
            .map_err(|error| format!("interface authoring verdict read failed: {error}"))?;
        let reply = reply
            .into_text()
            .map_err(|error| format!("interface authoring verdict is not text: {error}"))?;
        let verdict: DeliveryVerdict = serde_json::from_str(&reply)
            .map_err(|error| format!("interface authoring verdict is malformed: {error}"))?;
        let _ = ws.close(None).await;
        Ok(verdict)
    })
    .await
    .map_err(|_| {
        format!(
            "interface authoring hand-in timed out after {}ms",
            config.result_timeout_ms
        )
    })?
}

fn run_authoring_profile(descriptor: &InterfaceDescriptor) -> Result<RunAuthoringProfile, String> {
    let interface_config = descriptor.interface_driver_config.as_ref().ok_or_else(|| {
        format!(
            "provider interface '{}' has no interface_driver_config",
            descriptor.id.as_str()
        )
    })?;
    if interface_config
        .private_config
        .contains_key("run_authoring")
    {
        return Err(format!(
            "provider interface '{}' declares removed private_config.run_authoring reverse topology",
            descriptor.id.as_str()
        ));
    }
    let value = interface_config
        .private_config
        .get("run_authoring_profile")
        .ok_or_else(|| {
            format!(
                "provider interface '{}' declares no private_config.run_authoring_profile",
                descriptor.id.as_str()
            )
        })?;
    let profile: RunAuthoringProfile = serde_json::from_value(value.clone()).map_err(|error| {
        format!(
            "provider interface '{}' run_authoring_profile is malformed: {error}",
            descriptor.id.as_str()
        )
    })?;
    profile.validate(&descriptor.id)?;
    let hidden_tools = interface_config
        .private_config
        .get("model_hidden_tools")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            format!(
                "provider interface '{}' private_config.model_hidden_tools must be an array containing its run-authoring tool",
                descriptor.id.as_str()
            )
        })?;
    let mut seen = BTreeSet::new();
    let mut hides_authoring_tool = false;
    for (index, value) in hidden_tools.iter().enumerate() {
        let name = value
            .as_str()
            .filter(|name| !name.is_empty() && name.trim() == *name)
            .ok_or_else(|| {
                format!(
                    "provider interface '{}' private_config.model_hidden_tools[{index}] must be a non-empty trimmed string",
                    descriptor.id.as_str()
                )
            })?;
        if !seen.insert(name.to_string()) {
            return Err(format!(
                "provider interface '{}' private_config.model_hidden_tools repeats '{name}'",
                descriptor.id.as_str()
            ));
        }
        if name == profile.file_write_tool.as_str() {
            hides_authoring_tool = true;
        }
    }
    if !hides_authoring_tool {
        return Err(format!(
            "provider interface '{}' exposes run-authoring tool '{}' to the model; private_config.model_hidden_tools must contain it",
            descriptor.id.as_str(),
            profile.file_write_tool
        ));
    }
    Ok(profile)
}

impl RunAuthoringProfile {
    fn validate(&self, provider: &InterfaceId) -> Result<(), String> {
        if self.result_timeout_ms == 0 {
            return Err(format!(
                "provider interface '{}' run_authoring_profile.result_timeout_ms must be positive",
                provider.as_str()
            ));
        }
        for (name, value) in [
            ("file_write_tool", self.file_write_tool.as_str()),
            ("file_write_label", self.file_write_label.as_str()),
            (
                "file_write_result_label",
                self.file_write_result_label.as_str(),
            ),
        ] {
            non_empty_string(value, &format!("run_authoring_profile.{name}"))?;
        }
        Ok(())
    }
}

pub fn validate_admitted_run_writer(
    snapshot: &[u8],
    metadata: &InvocationMetadata,
    writer: &CheckedRunWriter,
) -> Result<(), String> {
    let runtime = metadata.runtime.as_ref().ok_or("coauthor proof has no admitted coordinate")?;
    let record = RunRecord::from_bytes(snapshot).map_err(|e| e.to_string())?;
    if runtime.run_record_hash.as_deref() != Some(ContentHash::of_bytes(snapshot).to_hex().as_str())
        || runtime.run != record.id || runtime.worker != record.worker || runtime.ee != record.ee
        || !record.active_checkpoints.contains(&runtime.checkpoint)
        || record.checkpoint(&runtime.checkpoint).is_none_or(|checkpoint| checkpoint.epoch.0 != runtime.epoch)
    {
        return Err("coauthor proof differs from its admitted Run snapshot".into());
    }
    let topology = validate_declared_provider_writer(&record, writer)?;
    topology.validate_identity(&record, &writer.owner)?;
    if !metadata.grant.interfaces.iter().any(|id| id == writer.owner.as_str())
        || metadata.grant.hard_blocked_tools.contains(&writer.tool)
    {
        return Err("coauthor owner or writer is denied by this formation".into());
    }
    if let Some(binding) = &topology.writer_run {
        binding.roles(&record, &writer.owner, &topology.checkpoints)?;
    } else if [&writer.source, &writer.door].iter().any(|id|
        !metadata.grant.interfaces.iter().any(|granted| granted == id.as_str()))
        || !metadata.grant.tools.contains(&writer.tool)
    {
        return Err("coauthor's local authoring edge is absent from this formation".into());
    }
    Ok(())
}

fn validate_declared_provider_writer(record: &RunRecord, writer: &CheckedRunWriter) -> Result<BoundRunAuthoringTopology, String> {
    let topology: BoundRunAuthoringTopology = serde_json::from_value(record.interface_state.get(&writer.owner)
        .and_then(|state| state.get(RUN_AUTHORING_BINDING)).cloned()
        .ok_or("independent provider writer has no owner-authored topology")?).map_err(|e| e.to_string())?;
    if topology.schema_version != 2 || topology.run != record.id || topology.worker != record.worker
        || topology.provider_interface != writer.owner || topology.authoring_source_interface != writer.source {
        return Err("independent provider writer differs from its exact owner topology".into());
    }
    let calls = topology.checkpoints.iter().filter(|cp| cp.role == RunAuthoringCheckpointRole::AuthoringCall).collect::<Vec<_>>();
    let results = topology.checkpoints.iter().filter(|cp| cp.role == RunAuthoringCheckpointRole::AuthoringResult).collect::<Vec<_>>();
    let ([call], [result]) = (calls.as_slice(), results.as_slice()) else { return Err("independent provider writer requires one exact call and result".into()); };
    let [required] = call.required_verified_artifacts.as_slice() else { return Err("independent provider writer requires one verified call".into()); };
    if call.interface() != &writer.source || call.outbound_interfaces().collect::<Vec<_>>() != [&writer.door]
        || result.interface() != &writer.door || result.outbound_interfaces().collect::<Vec<_>>() != [&writer.owner]
        || !call.interface_binding.admits(&writer.call_label) || !result.interface_binding.admits(&writer.result_label)
        || required.artifact_label != writer.call_label || required.verifier.as_str() != "tool_call_allowed"
        || required.params.get("tools") != Some(&serde_json::json!([writer.tool])) {
        return Err("independent provider writer differs from its exact declared source, door, tool or labels".into());
    }
    topology.config(&RunAuthoringProfile {
        file_write_tool:writer.tool.clone(), file_write_label:writer.call_label.clone(),
        file_write_result_label:writer.result_label.clone(), result_timeout_ms:1,
    }).validate(&writer.owner, &record.id)?;
    Ok(topology)
}

impl BoundRunAuthoringTopology {
    fn config(&self, profile: &RunAuthoringProfile) -> RunAuthoringConfig {
        RunAuthoringConfig {
            run: self.run.clone(),
            worker: self.worker.clone(),
            provider_interface: self.provider_interface.clone(),
            authoring_source_interface: self.authoring_source_interface.clone(),
            file_write_tool: profile.file_write_tool.clone(),
            file_write_label: profile.file_write_label.clone(),
            file_write_result_label: profile.file_write_result_label.clone(),
            payload_artifact_bindings: self.payload_artifact_bindings.clone(),
            writer_run: self.writer_run.clone(),
            checkpoints: self.checkpoints.clone(),
            result_timeout_ms: profile.result_timeout_ms,
        }
    }

    fn validate_identity(&self, record: &RunRecord, provider: &InterfaceId) -> Result<(), String> {
        if self.schema_version != 2 {
            return Err(format!(
                "provider run '{}' run_authoring_binding schema_version must be 2",
                record.id.as_str()
            ));
        }
        if self.run != record.id
            || self.worker != record.worker
            || self.provider_interface != *provider
        {
            return Err(format!(
                "provider run '{}' run_authoring_binding identity does not match run='{}' worker='{}' provider='{}'",
                record.id.as_str(),
                record.id.as_str(),
                record.worker.as_str(),
                provider.as_str()
            ));
        }
        for input in &self.checkpoints {
            let Some(writer) = &input.owner_authoring else { continue; };
            if writer.source == writer.owner { continue; }
            let owner = validate_declared_provider_writer(record, writer)?;
            if !owner.checkpoints.iter().any(|output| matches!(output.role, RunAuthoringCheckpointRole::ProviderInterfaceOutput | RunAuthoringCheckpointRole::IndependentInput)
                && output.interface_binding == input.interface_binding
                && output.outbound_interface_bindings == input.outbound_interface_bindings
                && output.static_artifact_bindings == input.static_artifact_bindings
                && output.include_context == input.include_context
                && output.required_verified_artifacts == input.required_verified_artifacts) {
                return Err("independent provider input has no matching owner-authored output".into());
            }
        }
        Ok(())
    }
}

fn validate_persisted_profile_binding(
    record: &RunRecord,
    config: &RunAuthoringConfig,
    expected: &BoundRunAuthoringTopology,
) -> Result<(), String> {
    let provider_state = record
        .interface_state
        .get(&config.provider_interface)
        .ok_or_else(|| {
            format!(
                "provider run '{}' has no interface_state for '{}'",
                record.id.as_str(),
                config.provider_interface.as_str()
            )
        })?;
    let value = provider_state.get(RUN_AUTHORING_BINDING).ok_or_else(|| {
        format!(
            "provider run '{}' has no durable run_authoring_binding",
            record.id.as_str()
        )
    })?;
    let persisted: BoundRunAuthoringTopology =
        serde_json::from_value(value.clone()).map_err(|error| {
            format!(
                "provider run '{}' run_authoring_binding is malformed: {error}",
                record.id.as_str()
            )
        })?;
    persisted.validate_identity(record, &config.provider_interface)?;
    if &persisted != expected {
        return Err(format!(
            "provider run '{}' changed its durable run_authoring_binding",
            record.id.as_str()
        ));
    }
    refuse_selected_consent_state(record, &config.provider_interface)?;
    validate_bound_run_authoring_topology(record, config, None)
}

fn resolve_run_authoring_invocation(
    profile: &RunAuthoringProfile,
    provider: &InterfaceId,
    run: &RunId,
    record: &RunRecord,
    source_checkpoint: &CheckpointId,
) -> Result<(RunAuthoringConfig, BoundRunAuthoringTopology), String> {
    resolve_profile_invocation(profile, provider, run, record, Some(source_checkpoint))
}

fn resolve_profile_invocation(
    profile: &RunAuthoringProfile,
    provider: &InterfaceId,
    run: &RunId,
    record: &RunRecord,
    source_checkpoint: Option<&CheckpointId>,
) -> Result<(RunAuthoringConfig, BoundRunAuthoringTopology), String> {
    profile.validate(provider)?;
    if record.id != *run {
        return Err(format!(
            "run_authoring_profile invocation belongs to run '{}', pinned run is '{}'",
            run.as_str(),
            record.id.as_str()
        ));
    }
    let provider_state = record.interface_state.get(provider).ok_or_else(|| {
        format!(
            "provider run '{}' has no interface_state for '{}'",
            record.id.as_str(),
            provider.as_str()
        )
    })?;
    let topology = match provider_state.get(RUN_AUTHORING_BINDING) {
        Some(value) => {
            let generation = optional_u64(provider_state, "authoring_generation")?;
            let pending = persisted_pending_handoff(record, provider)?;
            if generation == 0 {
                if pending.is_some() {
                    return Err(format!(
                        "provider run '{}' declares pending handoff before provider authoring began",
                        record.id.as_str()
                    ));
                }
            } else if let Some(pending) = pending {
                pending.items.first().ok_or_else(|| {
                    format!(
                        "provider run '{}' persisted run_authoring_binding with an empty pending handoff batch",
                        record.id.as_str()
                    )
                })?;
            } else if optional_u64(provider_state, "context_generation")? != 0 {
                return Err("provider has context history without its pending handoff".into());
            }
            serde_json::from_value::<BoundRunAuthoringTopology>(value.clone()).map_err(|error| {
                format!(
                    "provider run '{}' run_authoring_binding is malformed: {error}",
                    record.id.as_str()
                )
            })?
        }
        None => {
            return Err(format!(
                "provider run '{}' has no explicitly authored run_authoring_binding",
                record.id.as_str()
            ));
        }
    };
    topology.validate_identity(record, provider)?;
    let config = topology.config(profile);
    config.validate(provider, run)?;
    refuse_selected_consent_state(record, provider)?;
    validate_bound_run_authoring_topology(record, &config, source_checkpoint)?;
    Ok((config, topology))
}

fn validate_retired_verified_input(
    ctx: &InterfaceLiftContext,
    session: &ProviderHandoffSession,
    initial_record: &RunRecord,
    current_record: &RunRecord,
    initial: &RunAuthoringConfig,
    current: &RunAuthoringConfig,
    initial_topology: &BoundRunAuthoringTopology,
    current_topology: &BoundRunAuthoringTopology,
    consumed: &RunAuthoringCheckpoint,
    source_movement_seq: u64,
) -> Result<(), String> {
    let mut without_verified = initial.clone();
    without_verified.checkpoints.retain(|checkpoint| checkpoint.role != RunAuthoringCheckpointRole::VerifiedInput);
    let mut topology_without_verified = initial_topology.clone();
    topology_without_verified.checkpoints = without_verified.checkpoints.clone();
    if without_verified == *initial
        || without_verified != *current
        || topology_without_verified != *current_topology
        || consumed.role != RunAuthoringCheckpointRole::Stable
        || !consumed.include_context
        || !current.checkpoints.contains(consumed)
    {
        return Err(
            "provider current run changed its profile-derived authoring topology".to_string(),
        );
    }
    let pending = persisted_pending_handoff(current_record, &current.provider_interface)?
        .ok_or_else(|| "retired verified input has no terminal provider handoff".to_string())?;
    if !pending.terminal || pending.source_movement_seq >= source_movement_seq {
        return Err("verified input retirement does not precede the queued source".to_string());
    }
    let (coordinate, evidence) = session.index()?.exact_provider_source_outbound(
        &current.run,
        &current.worker,
        &current.provider_interface,
        pending.source_movement_seq,
    )?;
    let pinned = indexed_source_run_record(ctx, &evidence, pending.source_movement_seq)?;
    let id = CheckpointId::new(&coordinate.checkpoint);
    let checkpoint = pinned
        .checkpoint(&id)
        .ok_or_else(|| "retirement source lost its pinned checkpoint".to_string())?;
    let candidates = initial
        .checkpoints
        .iter()
        .filter(|template| {
            template.role == RunAuthoringCheckpointRole::VerifiedInput
                && checkpoint_record_matches_template(checkpoint, template, &initial_record.id)
                && template.interface() == &evidence.source
                && template.outbound_interface_bindings.len() == 2
                && template
                    .outbound_interfaces()
                    .eq([&current.provider_interface, consumed.interface()])
        })
        .collect::<Vec<_>>();
    if pinned.id != initial_record.id
        || pinned.worker != initial_record.worker
        || pinned.ee != initial_record.ee
        || !pinned.active_checkpoints.contains(&id)
        || checkpoint.epoch.0 != coordinate.epoch
        || candidates.len() != 1
        || evidence.checkpoint_binding_policy.as_ref()
            != Some(&candidates[0].binding_policy_snapshot())
        || evidence.required_verified_artifacts != candidates[0].required_verified_artifacts
        || !checkpoint
            .outbound_interfaces()
            .eq(evidence.outbound_interface_bindings.iter())
        || !artifact_refs_match_bindings(
            &evidence.artifact_binding_refs,
            &checkpoint.artifact_bindings,
        )
    {
        return Err(
            "queued tool result cannot adopt an unrelated verified-input retirement".to_string(),
        );
    }
    Ok(())
}

pub fn bound_client_interface(
    value: &serde_json::Value,
    provider: &InterfaceId,
) -> Result<InterfaceId, String> {
    let record =
        RunRecord::from_bytes(&serde_json::to_vec(value).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let value = record
        .interface_state
        .get(provider)
        .and_then(|state| state.get(RUN_AUTHORING_BINDING))
        .ok_or_else(|| "provider client projection has no bound topology".to_string())?;
    let topology: BoundRunAuthoringTopology =
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    topology.validate_identity(&record, provider)?;
    let clients = topology.checkpoints.iter()
        .filter(|checkpoint| checkpoint.role == RunAuthoringCheckpointRole::ProviderArtifactOutput)
        .flat_map(|checkpoint| checkpoint.outbound_interfaces())
        .filter(|interface| *interface != &record.worker && topology.checkpoints.iter().any(|input| {
            input.role.is_provider_input() && input.interface() == *interface
        }))
        .cloned().collect::<BTreeSet<_>>();
    let clients = clients.into_iter().collect::<Vec<_>>();
    let [client] = clients.as_slice() else {
        return Err("provider has no unique bidirectional client projection".to_string());
    };
    Ok(client.clone())
}

pub fn bound_tool_door(
    value: &serde_json::Value,
    provider: &InterfaceId,
    tool: &str,
) -> Result<InterfaceId, String> {
    let record =
        RunRecord::from_bytes(&serde_json::to_vec(value).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let value = record
        .interface_state
        .get(provider)
        .and_then(|state| state.get(RUN_AUTHORING_BINDING))
        .ok_or_else(|| "provider tool route has no bound authoring topology".to_string())?;
    let topology: BoundRunAuthoringTopology = serde_json::from_value(value.clone())
        .map_err(|error| format!("provider tool topology is malformed: {error}"))?;
    topology.validate_identity(&record, provider)?;
    let mut owners = Vec::new();
    for checkpoint in &topology.checkpoints {
        if checkpoint.role == RunAuthoringCheckpointRole::ProviderToolOutput
            && checkpoint.interface() == provider
            && checkpoint_tool_ids(checkpoint)?.contains(tool)
        {
            let [door] = checkpoint.outbound_interface_bindings.as_slice() else {
                return Err("provider tool route has no exact door".to_string());
            };
            owners.push(door.interface.clone());
        }
    }
    match owners.as_slice() {
        [door] => Ok(door.clone()),
        _ => Err(format!(
            "provider tool '{tool}' has {} exact owners",
            owners.len()
        )),
    }
}

pub fn declared_provider_run_writer(
    record: &RunRecord, provider: &InterfaceId, profile: &RunAuthoringProfile,
) -> Result<CheckedRunWriter, String> {
    let config = config_from_persisted_profile(record, provider, profile)?;
    let writer = CheckedRunWriter { owner: provider.clone(), source: config.authoring_source_interface.clone(),
        door: config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?.interface().clone(),
        tool: profile.file_write_tool.clone(), call_label: profile.file_write_label.clone(), result_label: profile.file_write_result_label.clone() };
    validate_declared_provider_writer(record, &writer)?;
    Ok(writer)
}

fn config_from_persisted_profile(
    record: &RunRecord,
    provider: &InterfaceId,
    profile: &RunAuthoringProfile,
) -> Result<RunAuthoringConfig, String> {
    profile.validate(provider)?;
    let provider_state = record.interface_state.get(provider).ok_or_else(|| {
        format!(
            "provider run '{}' has no interface_state for '{}'",
            record.id.as_str(),
            provider.as_str()
        )
    })?;
    let value = provider_state.get(RUN_AUTHORING_BINDING).ok_or_else(|| {
        format!(
            "provider run '{}' authoring result has no durable run_authoring_binding",
            record.id.as_str()
        )
    })?;
    let topology: BoundRunAuthoringTopology =
        serde_json::from_value(value.clone()).map_err(|error| {
            format!(
                "provider run '{}' run_authoring_binding is malformed: {error}",
                record.id.as_str()
            )
        })?;
    topology.validate_identity(record, provider)?;
    let config = topology.config(profile);
    config.validate(provider, &record.id)?;
    validate_bound_run_authoring_topology(record, &config, None)?;
    Ok(config)
}

#[cfg(test)]
fn derive_bound_run_authoring_topology(
    record: &RunRecord,
    provider: &InterfaceId,
    source_checkpoint: &CheckpointId,
) -> Result<BoundRunAuthoringTopology, String> {
    let provider_state = record.interface_state.get(provider).ok_or_else(|| {
        format!(
            "provider run '{}' has no interface_state for '{}'",
            record.id.as_str(),
            provider.as_str()
        )
    })?;
    let frontier = raw_frontier_checkpoint_ids(provider_state)?;
    if frontier.len() < 3 {
        return Err(format!(
            "provider run '{}' bind-time frontier must contain output, authoring-call, and authoring-result checkpoints",
            record.id.as_str()
        ));
    }
    let active = record.active_checkpoints.iter().collect::<BTreeSet<_>>();
    let mut frontier_records = Vec::with_capacity(frontier.len());
    for id in &frontier {
        let checkpoint_id = CheckpointId::new(id);
        if !active.contains(&checkpoint_id) {
            return Err(format!(
                "provider run '{}' bind-time frontier checkpoint '{}' is not active",
                record.id.as_str(),
                id
            ));
        }
        frontier_records.push(record.checkpoint(&checkpoint_id).ok_or_else(|| {
            format!(
                "provider run '{}' bind-time frontier checkpoint '{}' is absent",
                record.id.as_str(),
                id
            )
        })?);
    }
    for checkpoint in record
        .active_checkpoints
        .iter()
        .filter(|id| !frontier.contains(id.as_str()))
        .filter_map(|id| record.checkpoint(id))
    {
        if checkpoint.inbound_interface() == Some(provider) {
            return Err(format!(
                "provider run '{}' active provider-bound checkpoint '{}' is outside the bind-time frontier",
                record.id.as_str(),
                checkpoint.id.as_str()
            ));
        }
    }
    let output_records = frontier_records
        .iter()
        .copied()
        .filter(|checkpoint| checkpoint.inbound_interface() == Some(provider))
        .collect::<Vec<_>>();
    if output_records.is_empty() {
        return Err(format!(
            "provider run '{}' bind-time frontier has no provider-bound output routes",
            record.id.as_str()
        ));
    }
    let mut output_templates = Vec::with_capacity(output_records.len());
    for checkpoint in output_records.iter().copied() {
        let role = match checkpoint.required_verified_artifacts.as_slice() {
            [] => RunAuthoringCheckpointRole::ProviderArtifactOutput,
            [required] if required.artifact_label == PROVIDER_HANDOFF_COMPLETE_LABEL => {
                RunAuthoringCheckpointRole::ProviderHandoffCompletion
            }
            [required] if required.artifact_label == PROVIDER_TRANSPORT_DISPATCH_LABEL => {
                RunAuthoringCheckpointRole::ProviderTransportDispatch
            }
            [required] if checkpoint_declares_tool_route(required) => {
                RunAuthoringCheckpointRole::ProviderToolOutput
            }
            [_] => RunAuthoringCheckpointRole::ProviderArtifactOutput,
            requirements => {
                return Err(format!(
                    "provider run '{}' bind-time output checkpoint '{}' has {} verified-artifact requirements",
                    record.id.as_str(),
                    checkpoint.id.as_str(),
                    requirements.len()
                ));
            }
        };
        let template = derived_checkpoint_template(role, checkpoint)?;
        match role {
            RunAuthoringCheckpointRole::ProviderToolOutput => {
                checkpoint_tool_ids(&template)?;
            }
            RunAuthoringCheckpointRole::ProviderTransportDispatch => {
                transport_dispatch_attempt(&template)?;
            }
            RunAuthoringCheckpointRole::ProviderHandoffCompletion => {
                validate_handoff_completion_checkpoint(&template)?;
            }
            _ => {}
        }
        output_templates.push(template);
    }
    let result_records = frontier_records
        .iter()
        .copied()
        .filter(|checkpoint| checkpoint.inbound_interface() != Some(provider))
        .filter(|checkpoint| {
            checkpoint.outbound_interface_bindings.len() == 1
                && checkpoint.outbound_interfaces().any(|id| id == provider)
        })
        .filter(|checkpoint| checkpoint.required_verified_artifacts.is_empty())
        .filter(|checkpoint| checkpoint.artifact_bindings.is_empty())
        .collect::<Vec<_>>();
    let [result_record] = result_records.as_slice() else {
        return Err(format!(
            "provider run '{}' bind-time frontier has {} structural authoring-result routes",
            record.id.as_str(),
            result_records.len()
        ));
    };
    let result_interface = result_record.inbound_interface().ok_or_else(|| {
        "provider bind-time authoring-result checkpoint has no interface binding".to_string()
    })?;
    let call_records = frontier_records
        .iter()
        .copied()
        .filter(|checkpoint| checkpoint.id != result_record.id)
        .filter(|checkpoint| checkpoint.inbound_interface() != Some(provider))
        .filter(|checkpoint| {
            checkpoint.outbound_interface_bindings.len() == 1
                && checkpoint.outbound_interfaces().next() == Some(result_interface)
        })
        .filter(|checkpoint| checkpoint.required_verified_artifacts.is_empty())
        .filter(|checkpoint| checkpoint.artifact_bindings.is_empty())
        .collect::<Vec<_>>();
    let [call_record] = call_records.as_slice() else {
        return Err(format!(
            "provider run '{}' bind-time frontier has {} structural authoring-call routes",
            record.id.as_str(),
            call_records.len()
        ));
    };
    if output_records.len() + 2 != frontier_records.len() {
        return Err(format!(
            "provider run '{}' bind-time frontier contains an unclassified checkpoint",
            record.id.as_str()
        ));
    }
    let mut input_templates = Vec::new();
    let mut current_source = false;
    for checkpoint in record
        .active_checkpoints
        .iter()
        .filter(|id| !frontier.contains(id.as_str()))
        .filter_map(|id| record.checkpoint(id))
        .filter(|checkpoint| {
            checkpoint.outbound_interface_bindings.len() == 1
                && checkpoint.outbound_interfaces().any(|id| id == provider)
        })
    {
        current_source |= checkpoint.id == *source_checkpoint;
        let role = if checkpoint.artifact_bindings.is_empty() {
            RunAuthoringCheckpointRole::IndependentInput
        } else {
            RunAuthoringCheckpointRole::Stable
        };
        input_templates.push(derived_checkpoint_template(role, checkpoint)?);
    }
    if !current_source {
        return Err(format!(
            "provider invocation checkpoint '{}' is not an active non-frontier input to '{}'",
            source_checkpoint.as_str(),
            provider.as_str()
        ));
    }
    if input_templates.is_empty() {
        return Err(format!(
            "provider run '{}' bind-time topology has no provider inputs",
            record.id.as_str()
        ));
    }
    let call_template =
        derived_checkpoint_template(RunAuthoringCheckpointRole::AuthoringCall, call_record)?;
    let result_template =
        derived_checkpoint_template(RunAuthoringCheckpointRole::AuthoringResult, result_record)?;
    let authoring_source_interface = call_template.interface().clone();
    let mut checkpoints = input_templates;
    checkpoints.extend(output_templates);
    checkpoints.push(call_template);
    checkpoints.push(result_template);
    Ok(BoundRunAuthoringTopology {
        schema_version: 2,
        run: record.id.clone(),
        worker: record.worker.clone(),
        provider_interface: provider.clone(),
        authoring_source_interface,
        payload_artifact_bindings: Vec::new(),
        writer_run: None,
        checkpoints,
    })
}

#[cfg(test)]
fn derived_checkpoint_template(
    role: RunAuthoringCheckpointRole,
    checkpoint: &octopus_runtime::run_record::CheckpointRecord,
) -> Result<RunAuthoringCheckpoint, String> {
    let interface_binding = checkpoint.interface_binding.clone().ok_or_else(|| {
        format!(
            "provider bind-time checkpoint '{}' has no interface binding",
            checkpoint.id.as_str()
        )
    })?;
    let include_context = role == RunAuthoringCheckpointRole::Stable;
    let name = derived_checkpoint_name(
        role,
        &interface_binding,
        &checkpoint.outbound_interface_bindings,
        include_context,
        &checkpoint.required_verified_artifacts,
    )?;
    Ok(RunAuthoringCheckpoint {
        name,
        role,
        interface_binding,
        outbound_interface_bindings: checkpoint.outbound_interface_bindings.clone(),
        include_context,
        static_artifact_bindings: Vec::new(),
        owner_authoring: None,
        required_verified_artifacts: checkpoint.required_verified_artifacts.clone(),
    })
}

fn derived_checkpoint_name(
    role: RunAuthoringCheckpointRole,
    interface_binding: &AuthoredInterfaceBinding,
    outbound_interface_bindings: &[AuthoredInterfaceBinding],
    include_context: bool,
    required_verified_artifacts: &[RequiredVerifiedArtifact],
) -> Result<String, String> {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "role": role,
        "interface_binding": interface_binding,
        "outbound_interface_bindings": outbound_interface_bindings,
        "include_context": include_context,
        "required_verified_artifacts": required_verified_artifacts,
    }))
    .map_err(|error| format!("provider derived checkpoint template did not encode: {error}"))?;
    let digest = format!("{:x}", Sha256::digest(bytes));
    Ok(format!("derived_{}", &digest[..20]))
}

fn validate_bound_run_authoring_topology(
    record: &RunRecord,
    config: &RunAuthoringConfig,
    current_source: Option<&CheckpointId>,
) -> Result<(), String> {
    validate_payload_artifact_selection(&config.payload_artifact_bindings)?;
    let roles = current_mandatory_role_bindings(record, config)?;
    for reference in &config.payload_artifact_bindings {
        if roles.iter().any(|binding| binding.id == reference.id) {
            return Err("payload selection collides with an authoring role binding".into());
        }
        for checkpoint in record.checkpoints.iter().filter(|checkpoint| record.active_checkpoints.contains(&checkpoint.id)) {
            let bound = checkpoint.artifact_bindings.iter().filter(|binding| binding.id == reference.id).collect::<Vec<_>>();
            if !bound.is_empty() && (bound.len() != 1 || !bound[0].access.read) {
                return Err("payload classification has an ambiguous or unreadable checkpoint binding".into());
            }
        }
    }
    validate_checkpoint_topology(record,&config.provider_interface,&config.checkpoints,current_source)
}

fn validate_checkpoint_topology(
    record: &RunRecord,
    provider: &InterfaceId,
    templates: &[RunAuthoringCheckpoint],
    current_source: Option<&CheckpointId>,
) -> Result<(), String> {
    let checkpoint_for_role = |role| {
        let candidates = templates.iter().filter(|template| template.role == role).collect::<Vec<_>>();
        if candidates.len() != 1 { return Err("provider topology has an ambiguous authoring role".to_string()); }
        Ok(candidates[0])
    };
    let provider_state = record
        .interface_state
        .get(provider)
        .ok_or_else(|| {
            format!(
                "provider run '{}' has no interface_state for '{}'",
                record.id.as_str(),
                provider.as_str()
            )
        })?;
    let frontier = raw_frontier_checkpoint_ids(provider_state)?;
    let active = record.active_checkpoints.iter().collect::<BTreeSet<_>>();
    let frontier_records = frontier
        .iter()
        .map(|id| {
            let checkpoint_id = CheckpointId::new(id);
            if !active.contains(&checkpoint_id) {
                return Err(format!(
                    "provider run '{}' frontier checkpoint '{}' is not active",
                    record.id.as_str(),
                    id
                ));
            }
            record.checkpoint(&checkpoint_id).ok_or_else(|| {
                format!(
                    "provider run '{}' frontier checkpoint '{}' is absent",
                    record.id.as_str(),
                    id
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    for checkpoint_id in record
        .active_checkpoints
        .iter()
        .filter(|checkpoint_id| !frontier.contains(checkpoint_id.as_str()))
    {
        let checkpoint = record.checkpoint(checkpoint_id).ok_or_else(|| {
            format!(
                "provider run '{}' lacks active checkpoint '{}'",
                record.id.as_str(),
                checkpoint_id.as_str()
            )
        })?;
        if checkpoint.inbound_interface() == Some(provider) {
            return Err(format!(
                "provider run '{}' active provider-bound checkpoint '{}' is outside the provider-owned frontier",
                record.id.as_str(),
                checkpoint.id.as_str()
            ));
        }
    }
    let input_templates = templates
        .iter()
        .filter(|template| template.role.is_provider_input())
        .collect::<Vec<_>>();
    let output_templates = templates
        .iter()
        .filter(|template| {
            matches!(
                template.role,
                RunAuthoringCheckpointRole::ProviderArtifactOutput
                    | RunAuthoringCheckpointRole::ProviderInterfaceOutput
                    | RunAuthoringCheckpointRole::ProviderToolOutput
                    | RunAuthoringCheckpointRole::ProviderTransportDispatch
                    | RunAuthoringCheckpointRole::ProviderHandoffCompletion
            )
        })
        .collect::<Vec<_>>();
    let authoring_call = checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringCall)?;
    let authoring_result =
        checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
    let writer_run = writer_run::binding(record, provider)?;
    let authoring_count = if writer_run.is_some() { 0 } else { 2 };
    let mandatory_role_bindings = match &writer_run {
        Some(binding) => binding.roles(record, provider, templates)?,
        None => canonical_role_bindings(&frontier_records, authoring_call, authoring_result, &record.id)?,
    };
    for checkpoint_id in record
        .active_checkpoints
        .iter()
        .filter(|checkpoint_id| !frontier.contains(checkpoint_id.as_str()))
    {
        let checkpoint = record.checkpoint(checkpoint_id).ok_or_else(|| {
            format!(
                "provider run '{}' lacks active checkpoint '{}'",
                record.id.as_str(),
                checkpoint_id.as_str()
            )
        })?;
        if checkpoint.inbound_interface() == Some(authoring_call.interface())
            || checkpoint.inbound_interface() == Some(authoring_result.interface())
        {
            return Err(format!(
                "provider run '{}' active checkpoint '{}' shares a provider-owned authoring interface outside the frontier",
                record.id.as_str(),
                checkpoint.id.as_str()
            ));
        }
    }
    for template in [authoring_call, authoring_result].into_iter().filter(|_| writer_run.is_none()) {
        let matches = frontier_records
            .iter()
            .filter(|checkpoint| {
                checkpoint_record_matches_template(checkpoint, template, &record.id)
                    && checkpoint_role_bindings_match(
                        checkpoint,
                        &mandatory_role_bindings,
                        template,
                    )
            })
            .count();
        if matches != 1 {
            return Err(format!(
                "provider run '{}' frontier matches {} {:?} templates",
                record.id.as_str(),
                matches,
                template.role
            ));
        }
    }
    let active_output_matches = frontier_records
        .iter()
        .filter(|checkpoint| {
            output_templates.iter().any(|template| {
                checkpoint_record_matches_template(checkpoint, template, &record.id)
                    && checkpoint_role_bindings_match(
                        checkpoint,
                        &mandatory_role_bindings,
                        template,
                    )
            })
        })
        .count();
    let validate_rearmed_frontier = || -> Result<(), String> {
        for template in &output_templates {
            let matches = frontier_records
                .iter()
                .filter(|checkpoint| {
                    checkpoint_record_matches_template(checkpoint, template, &record.id)
                        && checkpoint_role_bindings_match(
                            checkpoint,
                            &mandatory_role_bindings,
                            template,
                        )
                })
                .count();
            if matches != 1 {
                return Err(format!(
                    "provider run '{}' rearmed frontier matches {} output template '{}'",
                    record.id.as_str(),
                    matches,
                    template.name
                ));
            }
        }
        if active_output_matches != output_templates.len()
            || frontier.len() != output_templates.len() + authoring_count
        {
            return Err(format!(
                "provider run '{}' rearmed frontier contains unclassified checkpoint topology",
                record.id.as_str()
            ));
        }
        Ok(())
    };
    let pending = provider_state.contains_key("pending_handoff");
    if pending {
        let pending =
            persisted_pending_handoff(record, provider)?.ok_or_else(|| {
                format!(
                    "provider run '{}' frontier declares pending state without a pending handoff",
                    record.id.as_str()
                )
            })?;
        for item in &pending.items {
            if item.artifact_label == PROVIDER_TRANSPORT_DISPATCH_LABEL
                && item.checkpoint.outbound_interface_bindings.is_empty()
                && !item
                    .checkpoint
                    .artifact_bindings
                    .iter()
                    .eq(mandatory_role_bindings.iter().map(|binding| &binding.id))
            {
                return Err(format!(
                    "provider run '{}' transport dispatch checkpoint '{}' must carry exactly its mandatory role bindings",
                    record.id.as_str(),
                    item.checkpoint.id.as_str()
                ));
            }
        }
        match validate_pending_items_are_active(record, provider, &pending) {
            Ok(retained) => {
                if frontier.len() != pending.items.len() + retained + authoring_count {
                    return Err(format!(
                        "provider run '{}' live profile frontier must contain every pending output and two authoring checkpoints",
                        record.id.as_str(),
                    ));
                }
            }
            Err(error) if pending.terminal => {
                validate_rearmed_frontier().map_err(|rearmed_error| {
                    format!("{error}; completed terminal frontier is not rearmed: {rearmed_error}")
                })?;
            }
            Err(error) => return Err(error),
        }
    } else {
        validate_rearmed_frontier()?;
    }
    let mut active_inputs = Vec::new();
    for checkpoint in record
        .active_checkpoints
        .iter()
        .filter(|id| !frontier.contains(id.as_str()))
        .filter_map(|id| record.checkpoint(id))
        .filter(|checkpoint| {
            checkpoint.outbound_interfaces().any(|id| id == provider)
        })
    {
        let matches = input_templates
            .iter()
            .filter(|template| {
                checkpoint_record_matches_template(checkpoint, template, &record.id)
                    && checkpoint_role_bindings_match(
                        checkpoint,
                        &mandatory_role_bindings,
                        template,
                    )
            })
            .count();
        if matches != 1 {
            return Err(format!(
                "provider run '{}' provider input checkpoint '{}' matches {} bound input templates",
                record.id.as_str(),
                checkpoint.id.as_str(),
                matches
            ));
        }
        active_inputs.push(checkpoint.id.clone());
    }
    for template in &input_templates {
        let count = active_inputs.iter().filter_map(|id| record.checkpoint(id)).filter(|checkpoint| checkpoint_record_matches_template(checkpoint, template, &record.id)).count();
        if count > 1 || (count == 0 && template.role != RunAuthoringCheckpointRole::OwnerInput) {
            return Err(format!("provider run '{}' input '{}' has {} active rows", record.id.as_str(), template.name, count));
        }
    }
    if let Some(source) = current_source {
        if !active_inputs.contains(source) {
            return Err(format!(
                "provider invocation checkpoint '{}' is not one active bound provider input",
                source.as_str()
            ));
        }
    }
    Ok(())
}

fn checkpoint_record_matches_template(
    checkpoint: &octopus_runtime::run_record::CheckpointRecord,
    template: &RunAuthoringCheckpoint,
    run: &RunId,
) -> bool {
    checkpoint.run == *run
        && checkpoint.interface_binding.as_ref() == Some(&template.interface_binding)
        && checkpoint.outbound_interface_bindings == template.outbound_interface_bindings
        && checkpoint.required_verified_artifacts == template.required_verified_artifacts
}

fn canonical_role_bindings(
    frontier: &[&octopus_runtime::run_record::CheckpointRecord],
    authoring_call: &RunAuthoringCheckpoint,
    authoring_result: &RunAuthoringCheckpoint,
    run: &RunId,
) -> Result<Vec<ArtifactBinding>, String> {
    let checkpoint = |template: &RunAuthoringCheckpoint| {
        let matches = frontier
            .iter()
            .copied()
            .filter(|checkpoint| checkpoint_record_matches_template(checkpoint, template, run))
            .collect::<Vec<_>>();
        let [checkpoint] = matches.as_slice() else {
            return Err(format!(
                "provider run '{}' frontier matches {} {:?} checkpoint structures",
                run.as_str(),
                matches.len(),
                template.role
            ));
        };
        Ok(*checkpoint)
    };
    let call = checkpoint(authoring_call)?;
    let result = checkpoint(authoring_result)?;
    if call.artifact_bindings != result.artifact_bindings {
        return Err(format!(
            "provider run '{}' authoring checkpoints carry different mandatory role bindings",
            run.as_str()
        ));
    }
    if call
        .artifact_bindings
        .iter()
        .any(|binding| !binding.access.read)
    {
        return Err(format!(
            "provider run '{}' authoring checkpoints contain a non-readable mandatory role binding",
            run.as_str()
        ));
    }
    Ok(call.artifact_bindings.clone())
}

fn checkpoint_role_bindings_match(
    checkpoint: &octopus_runtime::run_record::CheckpointRecord,
    mandatory_role_bindings: &[ArtifactBinding],
    template: &RunAuthoringCheckpoint,
) -> bool {
    if template.include_context {
        return checkpoint.artifact_bindings.starts_with(mandatory_role_bindings)
            && mandatory_role_bindings.iter().all(|binding| binding.access.read)
            && template.static_artifact_bindings.iter().all(|id| {
                let mut matches = checkpoint.artifact_bindings.iter().filter(|binding| &binding.id == id);
                matches.next().is_some_and(|binding| {
                    match mandatory_role_bindings.iter().find(|role| &role.id == id) {
                        Some(role) => binding == role,
                        None => binding.access == Default::default(),
                    }
                })
                    && matches.next().is_none()
            });
    }
    let mut required = mandatory_role_bindings.to_vec();
    for id in &template.static_artifact_bindings {
        if required.iter().any(|binding| &binding.id == id) { continue; }
        required.push(ArtifactBinding { id: id.clone(), access: Default::default() });
    }
    let mandatory_role_bindings = required.as_slice();
    if checkpoint.artifact_bindings.len() != mandatory_role_bindings.len() {
        return false;
    }
    checkpoint
        .artifact_bindings
        .iter()
        .zip(mandatory_role_bindings)
        .all(|(binding, required)| binding == required && binding.access.read)
}

fn artifact_refs_match_bindings(refs: &[ArtifactRef], bindings: &[ArtifactBinding]) -> bool {
    refs.iter()
        .map(|artifact| &artifact.id)
        .eq(bindings.iter().map(|binding| &binding.id))
}

fn correlation_generation(run: &RunId, correlation_id: &str) -> Result<u64, String> {
    let prefix = format!("provider-run-authoring:{}:", run.as_str());
    let raw_generation = correlation_id.strip_prefix(&prefix).ok_or_else(|| {
        "provider authoring result correlation_id is outside the private authoring namespace"
            .to_string()
    })?;
    let generation = raw_generation
        .parse::<u64>()
        .map_err(|_| "provider authoring result correlation generation is malformed".to_string())?;
    if generation == 0 || generation.to_string() != raw_generation {
        return Err(
            "provider authoring result correlation generation is not canonical".to_string(),
        );
    }
    Ok(generation)
}

fn derive_handoff_delivery_mode(
    input: &InterfaceInvocationEnvelope,
    config: &RunAuthoringConfig,
    record: &RunRecord,
    source_checkpoint: &CheckpointId,
    granted_tools: &BTreeSet<String>,
) -> Result<HandoffDeliveryMode, String> {
    let metadata = input.metadata().map_err(|error| error.to_string())?;
    let hard_blocked = input
        .hard_blocked_tool_names()
        .map_err(|error| error.to_string())?
        .into_iter()
        .collect::<BTreeSet<_>>();
    if let Some(writer) = &config.writer_run {
        writer.roles(record, &config.provider_interface, &config.checkpoints)?;
        if hard_blocked.contains(&config.file_write_tool) {
            return Err("provider's declared writer tool is hard-blocked".into());
        }
        if metadata.grant().ee_root_slot_allowance_set()
            .is_some_and(|allowance| !allowance.contains("run_records_root")) {
            return Err("provider's declared writer Run is outside the EE root-slot allowance".into());
        }
        return Ok(HandoffDeliveryMode::Rearm);
    }
    if granted_tools.contains(&config.file_write_tool) {
        if hard_blocked.contains(&config.file_write_tool) {
            return Err(format!(
                "provider run '{}' grants and hard-blocks authoring tool '{}'",
                config.run.as_str(),
                config.file_write_tool
            ));
        }
        let authoring_call =
            config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringCall)?;
        let authoring_result =
            config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
        for interface in [
            &config.provider_interface,
            &config.authoring_source_interface,
            authoring_result.interface(),
        ] {
            if !metadata
                .allowed_interfaces()
                .iter()
                .any(|allowed| allowed == interface.as_str())
            {
                return Err(format!(
                    "provider run '{}' authoring path interface '{}' is absent from the formed grant",
                    config.run.as_str(),
                    interface.as_str()
                ));
            }
        }
        if authoring_call.outbound_interfaces().next() != Some(authoring_result.interface()) {
            return Err(format!(
                "provider run '{}' authoring path is incomplete",
                config.run.as_str()
            ));
        }
        if metadata.grant().ee_root_slot_allowance_set()
            .is_some_and(|allowance| !allowance.contains("run_records_root")) {
            return Err(format!(
                "provider run '{}' formed grant does not admit run_records_root",
                config.run.as_str()
            ));
        }
        return Ok(HandoffDeliveryMode::Rearm);
    }
    if !granted_tools.is_empty() {
        return Err(format!(
            "provider run '{}' exposes model-visible tool continuation without run re-arm authority",
            config.run.as_str()
        ));
    }
    if !record.active_checkpoints.contains(source_checkpoint) {
        return Err(format!(
            "provider one-shot source checkpoint '{}' is no longer active",
            source_checkpoint.as_str()
        ));
    }
    let source = record.checkpoint(source_checkpoint).ok_or_else(|| {
        format!(
            "provider one-shot source checkpoint '{}' is absent",
            source_checkpoint.as_str()
        )
    })?;
    let source_interface = source.inbound_interface().ok_or_else(|| {
        format!(
            "provider one-shot source checkpoint '{}' has no interface binding",
            source_checkpoint.as_str()
        )
    })?;
    let active_source_routes = record
        .active_checkpoints
        .iter()
        .filter_map(|checkpoint| record.checkpoint(checkpoint))
        .filter(|checkpoint| checkpoint.inbound_interface() == Some(source_interface))
        .count();
    if active_source_routes != 1 {
        return Err(format!(
            "provider one-shot source interface '{}' is bound to {} active checkpoints in run '{}'",
            source_interface.as_str(),
            active_source_routes,
            record.id.as_str()
        ));
    }
    let output_templates = config
        .checkpoints
        .iter()
        .filter(|checkpoint| {
            matches!(
                checkpoint.role,
                RunAuthoringCheckpointRole::ProviderArtifactOutput
                    | RunAuthoringCheckpointRole::ProviderInterfaceOutput
                    | RunAuthoringCheckpointRole::ProviderToolOutput
                    | RunAuthoringCheckpointRole::ProviderTransportDispatch
                    | RunAuthoringCheckpointRole::ProviderHandoffCompletion
            )
        })
        .collect::<Vec<_>>();
    let mut terminal = BTreeMap::new();
    let mut dispatch = BTreeMap::new();
    let mut completion = None;
    for checkpoint_id in &record.active_checkpoints {
        let checkpoint = record.checkpoint(checkpoint_id).ok_or_else(|| {
            format!(
                "provider one-shot active checkpoint '{}' is absent",
                checkpoint_id.as_str()
            )
        })?;
        if checkpoint.inbound_interface() != Some(&config.provider_interface) {
            continue;
        }
        let owners = output_templates
            .iter()
            .copied()
            .filter(|template| checkpoint_record_matches_template(checkpoint, template, &record.id))
            .collect::<Vec<_>>();
        let [owner] = owners.as_slice() else {
            return Err(format!(
                "provider one-shot checkpoint '{}' has {} descriptor-derived output owners",
                checkpoint.id.as_str(),
                owners.len()
            ));
        };
        let [required] = checkpoint.required_verified_artifacts.as_slice() else {
            return Err(format!(
                "provider one-shot checkpoint '{}' is not verifier-isolated",
                checkpoint.id.as_str()
            ));
        };
        let slot = OneShotHandoffSlot {
            checkpoint: checkpoint.id.clone(),
            epoch: checkpoint.epoch.0,
            required_mark: required.mark.as_str().to_string(),
            required_verifier: required.verifier.as_str().to_string(),
        };
        match owner.role {
            RunAuthoringCheckpointRole::ProviderArtifactOutput => {
                if terminal
                    .insert(required.artifact_label.clone(), slot)
                    .is_some()
                {
                    return Err(format!(
                        "provider one-shot topology repeats terminal artifact label '{}'",
                        required.artifact_label
                    ));
                }
            }
            RunAuthoringCheckpointRole::ProviderTransportDispatch => {
                let attempt = transport_dispatch_attempt(owner)?;
                if dispatch.insert(attempt, slot).is_some() {
                    return Err(format!(
                        "provider one-shot topology repeats transport attempt {attempt}"
                    ));
                }
            }
            RunAuthoringCheckpointRole::ProviderToolOutput | RunAuthoringCheckpointRole::ProviderInterfaceOutput => {
                return Err(format!(
                    "provider one-shot checkpoint '{}' carries tool continuation without re-arm authority",
                    checkpoint.id.as_str()
                ));
            }
            RunAuthoringCheckpointRole::ProviderHandoffCompletion => {
                validate_handoff_completion_checkpoint(owner)?;
                if completion.replace(slot).is_some() {
                    return Err(
                        "provider one-shot topology repeats the handoff completion slot"
                            .to_string(),
                    );
                }
            }
            _ => unreachable!(),
        }
    }
    let completion = completion.ok_or_else(|| {
        format!(
            "provider run '{}' one-shot topology has no preauthored handoff completion slot",
            config.run.as_str()
        )
    })?;
    if terminal.is_empty() || dispatch.is_empty() {
        return Err(format!(
            "provider run '{}' one-shot topology requires preauthored verifier-isolated terminal, completion, and transport-dispatch slots",
            config.run.as_str()
        ));
    }
    Ok(HandoffDeliveryMode::OneShot(OneShotHandoffPlan {
        terminal,
        dispatch,
        completion,
    }))
}

fn transport_dispatch_attempt(checkpoint: &RunAuthoringCheckpoint) -> Result<u32, String> {
    let [required] = checkpoint.required_verified_artifacts.as_slice() else {
        return Err(format!(
            "run_authoring checkpoint '{}' transport route requires exactly one verified-artifact requirement",
            checkpoint.name
        ));
    };
    if required.artifact_label != PROVIDER_TRANSPORT_DISPATCH_LABEL {
        return Err(format!(
            "run_authoring checkpoint '{}' transport route requires artifact label '{}'",
            checkpoint.name, PROVIDER_TRANSPORT_DISPATCH_LABEL
        ));
    }
    let params = required.params.as_object().ok_or_else(|| {
        format!(
            "run_authoring checkpoint '{}' transport route params are not an object",
            checkpoint.name
        )
    })?;
    let attempt = params
        .get("attempt")
        .and_then(serde_json::Value::as_u64)
        .and_then(|attempt| u32::try_from(attempt).ok())
        .filter(|attempt| *attempt > 0)
        .ok_or_else(|| {
            format!(
                "run_authoring checkpoint '{}' transport route has no positive u32 attempt",
                checkpoint.name
            )
        })?;
    Ok(attempt)
}

fn validate_handoff_completion_checkpoint(
    checkpoint: &RunAuthoringCheckpoint,
) -> Result<(), String> {
    let [required] = checkpoint.required_verified_artifacts.as_slice() else {
        return Err(format!(
            "run_authoring checkpoint '{}' completion route requires exactly one verified-artifact requirement",
            checkpoint.name
        ));
    };
    if required.artifact_label != PROVIDER_HANDOFF_COMPLETE_LABEL {
        return Err(format!(
            "run_authoring checkpoint '{}' completion route requires artifact label '{}'",
            checkpoint.name, PROVIDER_HANDOFF_COMPLETE_LABEL
        ));
    }
    Ok(())
}

impl RunAuthoringConfig {
    fn validate(&self, lifted_interface: &InterfaceId, run: &RunId) -> Result<(), String> {
        if &self.run != run {
            return Err(format!(
                "run_authoring config belongs to run '{}', invocation belongs to '{}'",
                self.run.as_str(),
                run.as_str()
            ));
        }
        if &self.provider_interface != lifted_interface {
            return Err(format!(
                "run_authoring provider '{}' does not match lifted interface '{}'",
                self.provider_interface.as_str(),
                lifted_interface.as_str()
            ));
        }
        if self.result_timeout_ms == 0 {
            return Err("run_authoring result_timeout_ms must be positive".to_string());
        }
        for (name, value) in [
            ("file_write_tool", self.file_write_tool.as_str()),
            ("file_write_label", self.file_write_label.as_str()),
            (
                "file_write_result_label",
                self.file_write_result_label.as_str(),
            ),
        ] {
            non_empty_string(value, name)?;
        }
        if self.checkpoints.is_empty() {
            return Err("run_authoring checkpoints must not be empty".to_string());
        }
        let mut checkpoint_names = BTreeSet::new();
        let mut role_counts = std::collections::BTreeMap::new();
        let mut tool_owners = BTreeMap::new();
        for (index, checkpoint) in self.checkpoints.iter().enumerate() {
            let owner = format!("checkpoints[{index}]");
            validate_id_fragment(&checkpoint.name, &format!("{owner}.name"))?;
            if !checkpoint_names.insert(checkpoint.name.as_str()) {
                return Err(format!(
                    "run_authoring checkpoints repeat name '{}'",
                    checkpoint.name
                ));
            }
            checkpoint.interface_binding.validate().map_err(|error| {
                format!("run_authoring {owner}.interface_binding is invalid: {error}")
            })?;
            if checkpoint.outbound_interface_bindings.is_empty()
                && !matches!(checkpoint.role, RunAuthoringCheckpointRole::ProviderHandoffCompletion | RunAuthoringCheckpointRole::ProviderArtifactOutput)
            {
                return Err(format!(
                    "run_authoring {owner}.outbound_interface_bindings must not be empty"
                ));
            }
            let mut outbound = BTreeSet::new();
            for (outbound_index, binding) in
                checkpoint.outbound_interface_bindings.iter().enumerate()
            {
                binding.validate().map_err(|error| {
                    format!(
                        "run_authoring {owner}.outbound_interface_bindings[{outbound_index}] is invalid: {error}"
                    )
                })?;
                if !outbound.insert(binding.interface.as_str()) {
                    return Err(format!(
                        "run_authoring {owner}.outbound_interface_bindings repeats interface '{}'",
                        binding.interface.as_str()
                    ));
                }
            }
            let mut artifact_labels = BTreeSet::new();
            for (required_index, required) in
                checkpoint.required_verified_artifacts.iter().enumerate()
            {
                required
                    .validate(&format!(
                        "run_authoring {owner}.required_verified_artifacts[{required_index}]"
                    ))
                    .map_err(|error| format!("run_authoring checkpoint is malformed: {error}"))?;
                if !artifact_labels.insert(required.artifact_label.as_str()) {
                    return Err(format!(
                        "run_authoring {owner}.required_verified_artifacts repeats artifact_label '{}'",
                        required.artifact_label
                    ));
                }
            }
            if let Some(writer) = &checkpoint.owner_authoring {
                if checkpoint.role != RunAuthoringCheckpointRole::OwnerInput || writer.owner == self.provider_interface || (writer.source == writer.owner && writer.owner != checkpoint.interface_binding.interface) || writer.source == self.provider_interface || writer.door == writer.owner || writer.source == writer.door || writer.door == *self.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?.interface() {
                    return Err("external owner writer differs from its exact independent input".into());
                }
                for value in [writer.owner.as_str(),writer.source.as_str(),writer.door.as_str(),&writer.tool,&writer.call_label,&writer.result_label] { non_empty_string(value,"external owner writer")?; }
            }
            if !checkpoint.static_artifact_bindings.is_empty() {
                if (checkpoint.include_context && !matches!(checkpoint.role, RunAuthoringCheckpointRole::Stable | RunAuthoringCheckpointRole::IndependentInput | RunAuthoringCheckpointRole::OwnerInput | RunAuthoringCheckpointRole::ProviderToolOutput)) || !matches!(checkpoint.role, RunAuthoringCheckpointRole::Stable | RunAuthoringCheckpointRole::IndependentInput | RunAuthoringCheckpointRole::OwnerInput | RunAuthoringCheckpointRole::ProviderToolOutput | RunAuthoringCheckpointRole::ProviderInterfaceOutput) {
                    return Err(format!("run_authoring {owner} static artifacts require a declared input or tool/interface output"));
                }
                let mut ids = BTreeSet::new();
                for id in &checkpoint.static_artifact_bindings {
                    non_empty_string(id.as_str(), "static artifact binding")?;
                    if !ids.insert(id) { return Err(format!("run_authoring {owner} repeats a static artifact")); }
                }
            }
            *role_counts.entry(checkpoint.role).or_insert(0usize) += 1;
            match checkpoint.role {
                RunAuthoringCheckpointRole::Stable => {
                    if checkpoint.interface() == &self.provider_interface
                        || checkpoint.interface() == &self.authoring_source_interface
                        || !checkpoint.include_context
                        || checkpoint.outbound_interface_bindings.len() != 1
                        || checkpoint.outbound_interfaces().next() != Some(&self.provider_interface)
                    {
                        return Err(format!(
                            "run_authoring {owner} context input must carry context through exactly one provider edge"
                        ));
                    }
                }
                RunAuthoringCheckpointRole::IndependentInput => {
                    if checkpoint.interface() == &self.provider_interface
                        || checkpoint.interface() == &self.authoring_source_interface
                        || !checkpoint.outbound_interfaces().any(|id| id == &self.provider_interface)
                    {
                        return Err(format!(
                            "run_authoring {owner} independent input must include its owning provider edge"
                        ));
                    }
                }
                RunAuthoringCheckpointRole::OwnerInput => {
                    if checkpoint.interface() == &self.provider_interface || checkpoint.interface() == &self.authoring_source_interface
                        || !checkpoint.outbound_interfaces().any(|id| id == &self.provider_interface)
                        || (checkpoint.owner_authoring.is_none() && !self.checkpoints.iter().any(|output| output.role == RunAuthoringCheckpointRole::ProviderToolOutput && output.outbound_interfaces().next() == Some(checkpoint.interface()))) {
                        return Err(format!("run_authoring {owner} owner input must name an exact provider edge with its declared writer or tool door"));
                    }
                }
                RunAuthoringCheckpointRole::VerifiedInput => {
                    let [required] = checkpoint.required_verified_artifacts.as_slice() else {
                        return Err(format!(
                            "run_authoring {owner} verified input must require exactly one verified artifact"
                        ));
                    };
                    if checkpoint.interface() == &self.provider_interface
                        || checkpoint.interface() == &self.authoring_source_interface
                        || !checkpoint.include_context
                        || !matches!(checkpoint.outbound_interface_bindings.len(), 1 | 2)
                        || checkpoint.outbound_interfaces().next() != Some(&self.provider_interface)
                        || !binding_admits_exact_name(
                            &checkpoint.interface_binding,
                            &required.artifact_label,
                        )
                        || checkpoint
                            .outbound_interface_bindings
                            .iter()
                            .any(|binding| {
                                !binding_admits_exact_name(binding, &required.artifact_label)
                            })
                    {
                        return Err(format!(
                            "run_authoring {owner} verified input must be one exact-name, context-bearing client-to-provider verifier edge"
                        ));
                    }
                    if let [_, door] = checkpoint.outbound_interface_bindings.as_slice() {
                        let connected = self.checkpoints.iter().any(|route| {
                            route.role == RunAuthoringCheckpointRole::ProviderToolOutput
                                && route.interface() == &self.provider_interface
                                && route.outbound_interface_bindings.len() == 1
                                && route.outbound_interfaces().next() == Some(&door.interface)
                        });
                        if !connected || door.interface == checkpoint.interface_binding.interface {
                            return Err(format!(
                                "run_authoring {owner} verified input tool door is not connected"
                            ));
                        }
                    }
                }
                RunAuthoringCheckpointRole::ProviderArtifactOutput => {
                    if checkpoint.interface() != &self.provider_interface
                        || checkpoint.include_context
                        || checkpoint.required_verified_artifacts.len() > 1
                    {
                        return Err(format!(
                            "run_authoring {owner} artifact output must be one provider-input edge with at most one verifier and no context"
                        ));
                    }
                }
                RunAuthoringCheckpointRole::ProviderInterfaceOutput => {
                    let [target] = checkpoint.outbound_interface_bindings.as_slice() else {
                        return Err(format!("run_authoring {owner} interface output must have one exact recipient"));
                    };
                    if checkpoint.interface() != &self.provider_interface || target.interface == self.provider_interface
                        || checkpoint.include_context || !checkpoint.required_verified_artifacts.is_empty()
                        || checkpoint.interface_binding.config.artifact_policy != target.config.artifact_policy
                        || !matches!(&target.config.artifact_policy, BindingArtifactPolicy::Allow { names, namespaces } if names.len()==1 && namespaces.is_empty()) {
                        return Err(format!("run_authoring {owner} interface output must preserve one exact name and declared recipient without context or verifier"));
                    }
                }
                RunAuthoringCheckpointRole::ProviderToolOutput => {
                    if checkpoint.interface() != &self.provider_interface
                        || checkpoint.outbound_interface_bindings.len() != 1
                    {
                        return Err(format!(
                            "run_authoring {owner} tool output must be one verified provider-input edge; include_context selects its bound artifact visibility"
                        ));
                    }
                    for tool in checkpoint_tool_ids(checkpoint)? {
                        if let Some(existing) =
                            tool_owners.insert(tool.clone(), checkpoint.name.clone())
                        {
                            return Err(format!(
                                "run_authoring tool '{}' is owned by both '{}' and '{}'",
                                tool, existing, checkpoint.name
                            ));
                        }
                    }
                }
                RunAuthoringCheckpointRole::ProviderTransportDispatch => {
                    if checkpoint.interface() != &self.provider_interface
                        || checkpoint.include_context
                        || checkpoint.outbound_interface_bindings.len() != 1
                    {
                        return Err(format!(
                            "run_authoring {owner} transport dispatch must be one verified provider-input edge without context"
                        ));
                    }
                    transport_dispatch_attempt(checkpoint)?;
                }
                RunAuthoringCheckpointRole::ProviderHandoffCompletion => {
                    if checkpoint.interface() != &self.provider_interface
                        || checkpoint.include_context
                        || !checkpoint.outbound_interface_bindings.is_empty()
                    {
                        return Err(format!(
                            "run_authoring {owner} handoff completion must be one private provider-input checkpoint without outbound edges or context"
                        ));
                    }
                    validate_handoff_completion_checkpoint(checkpoint)?;
                }
                RunAuthoringCheckpointRole::AuthoringCall => {
                    if checkpoint.interface() != &self.authoring_source_interface
                        || checkpoint.include_context
                        || checkpoint.outbound_interface_bindings.len() != 1
                    {
                        return Err(format!(
                            "run_authoring {owner} authoring call must be one authoring-source edge without context"
                        ));
                    }
                }
                RunAuthoringCheckpointRole::AuthoringResult => {
                    if checkpoint.interface() == &self.provider_interface
                        || checkpoint.interface() == &self.authoring_source_interface
                        || checkpoint.include_context
                        || checkpoint.outbound_interface_bindings.len() != 1
                        || checkpoint.outbound_interfaces().next() != Some(&self.provider_interface)
                        || !checkpoint.required_verified_artifacts.is_empty()
                    {
                        return Err(format!(
                            "run_authoring {owner} authoring result must return unverified to the provider interface"
                        ));
                    }
                }
            }
        }
        let authoring_call = self.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringCall)?;
        let authoring_result =
            self.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
        if authoring_call.outbound_interfaces().next() != Some(authoring_result.interface()) {
            return Err(
                "run_authoring authoring call must target the exact authoring-result interface"
                    .to_string(),
            );
        }
        if let Some(stable) = self.checkpoints.iter().find(|checkpoint| {
            checkpoint.role.is_provider_input()
                && checkpoint.interface() == authoring_result.interface()
        }) {
            return Err(format!(
                "run_authoring context input '{}' shares the authoring-result interface '{}'",
                stable.name,
                authoring_result.interface().as_str()
            ));
        }
        let context_inputs = self
            .checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.role.is_provider_input() && checkpoint.include_context)
            .collect::<Vec<_>>();
        if context_inputs.is_empty() {
            return Err(
                "run_authoring checkpoints require at least one context-bearing input".to_string(),
            );
        }
        for (index, left) in context_inputs.iter().enumerate() {
            for right in context_inputs.iter().skip(index + 1) {
                if left.interface_binding == right.interface_binding
                    && left.outbound_interface_bindings == right.outbound_interface_bindings
                    && left.required_verified_artifacts == right.required_verified_artifacts
                {
                    return Err(format!(
                        "run_authoring context inputs '{}' and '{}' declare the same route",
                        left.name, right.name
                    ));
                }
            }
        }
        let artifact_outputs = self
            .checkpoints
            .iter()
            .filter(|checkpoint| {
                checkpoint.role == RunAuthoringCheckpointRole::ProviderArtifactOutput
            })
            .collect::<Vec<_>>();
        for (index, left) in artifact_outputs.iter().enumerate() {
            for right in artifact_outputs.iter().skip(index + 1) {
                if output_artifact_policies_overlap(left, right) {
                    return Err(format!(
                        "run_authoring artifact outputs '{}' and '{}' have overlapping outbound artifact policies",
                        left.name, right.name
                    ));
                }
            }
        }
        let artifact_output_count = role_counts
            .get(&RunAuthoringCheckpointRole::ProviderArtifactOutput)
            .copied()
            .unwrap_or_default();
        if artifact_output_count == 0 {
            return Err(
                "run_authoring checkpoints require at least one ProviderArtifactOutput".to_string(),
            );
        }
        for role in [
            RunAuthoringCheckpointRole::AuthoringCall,
            RunAuthoringCheckpointRole::AuthoringResult,
        ] {
            let count = role_counts.get(&role).copied().unwrap_or_default();
            if count != 1 {
                return Err(format!(
                    "run_authoring checkpoints require exactly one {role:?}, found {count}"
                ));
            }
        }
        Ok(())
    }

    fn checkpoint_for_role(
        &self,
        role: RunAuthoringCheckpointRole,
    ) -> Result<&RunAuthoringCheckpoint, String> {
        let mut matches = self
            .checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.role == role);
        let checkpoint = matches
            .next()
            .ok_or_else(|| format!("run_authoring has no {role:?} checkpoint"))?;
        if matches.next().is_some() {
            return Err(format!(
                "run_authoring has more than one {role:?} checkpoint"
            ));
        }
        Ok(checkpoint)
    }

    fn callable_tool_names(&self, metadata: &InvocationMetadata) -> Result<BTreeSet<String>, String> {
        let granted_tools = metadata.granted_tool_names().iter().cloned().collect::<BTreeSet<_>>();
        let granted_interfaces = metadata.allowed_interfaces().iter().map(InterfaceId::new).collect::<BTreeSet<_>>();
        let mut selected = BTreeSet::new();
        for checkpoint in self.checkpoints.iter().filter(|checkpoint| checkpoint.role == RunAuthoringCheckpointRole::ProviderToolOutput) {
            for tool in checkpoint_tool_ids(checkpoint)? {
                if !selected.insert(tool.clone()) { return Err(format!("selected provider tool '{tool}' has duplicate output owners")); }
                if metadata.hard_blocked_tool_names().contains(&tool) {
                    return Err(format!("selected provider tool '{tool}' is hard-blocked"));
                }
                let owner = self.output_checkpoint(&ProviderHandoffRoute::Tool { tool: tool.clone() }, &granted_tools, &granted_interfaces, &self.file_write_label)?;
                if owner.interface() != &self.provider_interface || !owner.interface_binding.admits(&self.file_write_label)
                    || std::iter::once(owner.interface()).chain(owner.outbound_interfaces()).any(|interface| !granted_interfaces.contains(interface)) {
                    return Err(format!("selected provider tool '{tool}' has a denied output route"));
                }
                let specs = metadata.interface_affordance_specs().iter().filter(|spec| spec.name.as_str() == tool).collect::<Vec<_>>();
                let [spec] = specs.as_slice() else { return Err(format!("selected provider tool '{tool}' requires one exact descriptor affordance")); };
                if !spec.args_schema.is_object() { return Err(format!("selected provider tool '{tool}' has a malformed descriptor affordance")); }
            }
        }
        Ok(selected)
    }

    fn output_checkpoint(
        &self,
        route: &ProviderHandoffRoute,
        granted_tools: &BTreeSet<String>,
        granted_interfaces: &BTreeSet<InterfaceId>,
        artifact_label: &str,
    ) -> Result<RunAuthoringCheckpoint, String> {
        match route {
            ProviderHandoffRoute::ClientProjection
            | ProviderHandoffRoute::ClientProjectionWithVerifiedInputs { .. } => {
                let owners = self
                    .checkpoints
                    .iter()
                    .filter(|checkpoint| {
                        checkpoint.role == RunAuthoringCheckpointRole::ProviderArtifactOutput
                            && checkpoint.interface_binding.admits(artifact_label)
                            && checkpoint.outbound_interface_bindings.iter().all(|binding| binding.admits(artifact_label))
                            && match checkpoint.required_verified_artifacts.as_slice() {
                                [] => true,
                                [required] => required.artifact_label == artifact_label,
                                _ => false,
                            }
                    })
                    .collect::<Vec<_>>();
                match owners.as_slice() {
                    [owner] => Ok((*owner).clone()),
                    [] => Err(format!(
                        "provider emitted client artifact '{}' with no descriptor-derived output owner",
                        artifact_label
                    )),
                    _ => Err(format!(
                        "provider emitted client artifact '{}' with {} descriptor-derived output owners",
                        artifact_label,
                        owners.len()
                    )),
                }
            }
            ProviderHandoffRoute::Tool { tool } => {
                if !granted_tools.contains(tool) {
                    return Err(format!(
                        "provider emitted tool '{}' outside the formed invocation grant",
                        tool
                    ));
                }
                let mut owners = Vec::new();
                for checkpoint in self.checkpoints.iter().filter(|checkpoint| {
                    checkpoint.role == RunAuthoringCheckpointRole::ProviderToolOutput
                }) {
                    if checkpoint_tool_ids(checkpoint)?.contains(tool)
                        && checkpoint.outbound_interface_bindings.len() == 1
                        && checkpoint.outbound_interface_bindings[0].admits(artifact_label)
                        && checkpoint.required_verified_artifacts[0].artifact_label
                            == artifact_label
                    {
                        owners.push(checkpoint);
                    }
                }
                match owners.as_slice() {
                    [] => Err(format!(
                        "provider emitted granted tool '{}' with no descriptor-derived output owner",
                        tool
                    )),
                    [owner] => Ok((*owner).clone()),
                    _ => Err(format!(
                        "provider emitted granted tool '{}' with {} descriptor-derived output owners",
                        tool,
                        owners.len()
                    )),
                }
            }
            ProviderHandoffRoute::Interface { interface } => {
                if !granted_interfaces.contains(interface) {
                    return Err(format!(
                        "provider emitted to interface '{}' outside the formed invocation grant",
                        interface.as_str()
                    ));
                }
                let owners = self
                    .checkpoints
                    .iter()
                    .filter(|checkpoint| {
                        matches!(checkpoint.role, RunAuthoringCheckpointRole::ProviderArtifactOutput | RunAuthoringCheckpointRole::ProviderInterfaceOutput)
                            && checkpoint.outbound_interface_bindings.len() == 1
                            && checkpoint.outbound_interfaces().next() == Some(interface)
                            && checkpoint.outbound_interface_bindings[0].admits(artifact_label)
                            && checkpoint.required_verified_artifacts.is_empty()
                    })
                    .collect::<Vec<_>>();
                match owners.as_slice() {
                    [] => Err(format!(
                        "provider emitted interface artifact '{}' to '{}' with no descriptor-derived output owner",
                        artifact_label,
                        interface.as_str()
                    )),
                    [owner] => Ok((*owner).clone()),
                    _ => Err(format!(
                        "provider emitted interface artifact '{}' to '{}' with {} descriptor-derived output owners",
                        artifact_label,
                        interface.as_str(),
                        owners.len()
                    )),
                }
            }
            ProviderHandoffRoute::TransportDispatch { attempt } => {
                if *attempt == 0 {
                    return Err("provider transport dispatch attempt must be positive".to_string());
                }
                Ok(RunAuthoringCheckpoint {
                    name: format!("transport_dispatch_{attempt}"),
                    role: RunAuthoringCheckpointRole::ProviderTransportDispatch,
                    interface_binding: AuthoredInterfaceBinding::all(
                        self.provider_interface.clone(),
                    ),
                    outbound_interface_bindings: Vec::new(),
                    include_context: false,
                    static_artifact_bindings: Vec::new(),
        owner_authoring: None,
                    required_verified_artifacts: Vec::new(),
                })
            }
        }
    }

    fn verified_input_checkpoints(
        &self,
        route: &ProviderHandoffRoute,
        output_checkpoint: &RunAuthoringCheckpoint,
        granted_mark_outcomes: &BTreeSet<String>,
        granted_tools: &BTreeSet<String>,
        granted_interfaces: &BTreeSet<InterfaceId>,
    ) -> Result<Vec<RunAuthoringCheckpoint>, String> {
        let ProviderHandoffRoute::ClientProjectionWithVerifiedInputs { inputs } = route else {
            return Ok(Vec::new());
        };
        if inputs.is_empty() {
            return Err(
                "provider client projection declares an empty verified-input set".to_string(),
            );
        }
        let [client_binding] = output_checkpoint.outbound_interface_bindings.as_slice() else {
            return Err(
                "provider verified-input projection has no exact client interface owner"
                    .to_string(),
            );
        };
        let mut checkpoints = Vec::with_capacity(inputs.len());
        let mut requirements = BTreeSet::new();
        for (index, input) in inputs.iter().enumerate() {
            let owner = format!("provider verified input[{index}]");
            non_empty_string(&input.artifact_label, &format!("{owner}.artifact_label"))?;
            input
                .required_verified_artifact
                .validate(&format!("{owner}.required_verified_artifact"))?;
            if input.required_verified_artifact.artifact_label != input.artifact_label {
                return Err(format!(
                    "{owner} artifact label '{}' differs from verifier artifact label '{}'",
                    input.artifact_label, input.required_verified_artifact.artifact_label
                ));
            }
            if !granted_mark_outcomes.contains(input.required_verified_artifact.mark.as_str()) {
                return Err(format!(
                    "{owner} requires mark '{}' outside the formed invocation grant",
                    input.required_verified_artifact.mark.as_str()
                ));
            }
            let identity = serde_json::to_string(&input.required_verified_artifact)
                .map_err(|error| format!("{owner} verifier requirement did not encode: {error}"))?;
            if !requirements.insert(identity) {
                return Err(format!(
                    "provider verified-input projection repeats requirement for '{}'",
                    input.artifact_label
                ));
            }
            let policy = BindingArtifactPolicy::Allow {
                names: vec![input.artifact_label.clone()],
                namespaces: Vec::new(),
            };
            let interface_binding = AuthoredInterfaceBinding {
                interface: client_binding.interface.clone(),
                config: InterfaceBindingConfig {
                    artifact_policy: policy.clone(),
                },
            };
            let mut outbound_interface_bindings = vec![AuthoredInterfaceBinding {
                interface: self.provider_interface.clone(),
                config: InterfaceBindingConfig {
                    artifact_policy: policy.clone(),
                },
            }];
            if let Some(tool) = &input.also_deliver_to_tool {
                let route = self.output_checkpoint(
                    &ProviderHandoffRoute::Tool { tool: tool.clone() },
                    granted_tools,
                    granted_interfaces,
                    &self.file_write_label,
                )?;
                let [door] = route.outbound_interface_bindings.as_slice() else {
                    return Err(format!("{owner} has no exact tool door"));
                };
                if !granted_interfaces.contains(&door.interface)
                    || door.interface == self.provider_interface
                    || door.interface == client_binding.interface
                {
                    return Err(format!(
                        "{owner} tool door is not a distinct granted interface"
                    ));
                }
                outbound_interface_bindings.push(AuthoredInterfaceBinding {
                    interface: door.interface.clone(),
                    config: InterfaceBindingConfig {
                        artifact_policy: policy,
                    },
                });
            }
            let required_verified_artifacts = vec![input.required_verified_artifact.clone()];
            checkpoints.push(RunAuthoringCheckpoint {
                name: derived_checkpoint_name(
                    RunAuthoringCheckpointRole::VerifiedInput,
                    &interface_binding,
                    &outbound_interface_bindings,
                    true,
                    &required_verified_artifacts,
                )?,
                role: RunAuthoringCheckpointRole::VerifiedInput,
                interface_binding,
                outbound_interface_bindings,
                include_context: true,
                static_artifact_bindings: Vec::new(),
        owner_authoring: None,
                required_verified_artifacts,
            });
        }
        Ok(checkpoints)
    }
}

#[cfg(test)]
fn checkpoint_declares_tool_route(required: &RequiredVerifiedArtifact) -> bool {
    required.verifier.as_str() == TOOL_ROUTE_VERIFIER
}

fn checkpoint_tool_ids(checkpoint: &RunAuthoringCheckpoint) -> Result<BTreeSet<String>, String> {
    if checkpoint.required_verified_artifacts.len() != 1 {
        return Err(format!(
            "run_authoring checkpoint '{}' tool route requires exactly one verified-artifact requirement",
            checkpoint.name
        ));
    }
    let params = checkpoint.required_verified_artifacts[0]
        .params
        .as_object()
        .ok_or_else(|| {
            format!(
                "run_authoring checkpoint '{}' tool route params are not an object",
                checkpoint.name
            )
        })?;
    if params.len() != 1 || !params.contains_key("tools") {
        return Err(format!(
            "run_authoring checkpoint '{}' tool route params must contain only tools",
            checkpoint.name
        ));
    }
    let tools = params["tools"].as_array().ok_or_else(|| {
        format!(
            "run_authoring checkpoint '{}' tool route tools are not an array",
            checkpoint.name
        )
    })?;
    if tools.is_empty() {
        return Err(format!(
            "run_authoring checkpoint '{}' tool route tools are empty",
            checkpoint.name
        ));
    }
    let mut ids = BTreeSet::new();
    for value in tools {
        let tool = value.as_str().ok_or_else(|| {
            format!(
                "run_authoring checkpoint '{}' tool route contains a non-string tool id",
                checkpoint.name
            )
        })?;
        let tool = non_empty_string(tool, "checkpoint tool")?;
        if !ids.insert(tool.clone()) {
            return Err(format!(
                "run_authoring checkpoint '{}' repeats tool '{}'",
                checkpoint.name, tool
            ));
        }
    }
    Ok(ids)
}

fn pending_output_checkpoint(
    checkpoint: &RunAuthoringCheckpoint,
    id: &str,
    artifact_label: &str,
    context_artifacts: &[ArtifactId],
    mandatory_role_artifacts: &[ArtifactId],
) -> PendingHandoffCheckpoint {
    let mut artifact_bindings = if checkpoint.include_context {
        context_artifacts.to_vec()
    } else {
        mandatory_role_artifacts.to_vec()
    };
    for artifact in &checkpoint.static_artifact_bindings { append_artifact(&mut artifact_bindings, artifact.clone()); }
    let mut interface_binding = checkpoint.interface_binding.clone();
    interface_binding.config.artifact_policy = BindingArtifactPolicy::Allow {
        names: vec![artifact_label.to_string()],
        namespaces: Vec::new(),
    };
    PendingHandoffCheckpoint {
        id: CheckpointId::new(id),
        epoch: 0,
        interface_binding,
        outbound_interface_bindings: checkpoint.outbound_interface_bindings.clone(),
        artifact_bindings,
        required_verified_artifacts: checkpoint.required_verified_artifacts.clone(),
    }
}

fn materialize_pending_checkpoint(
    checkpoint: &PendingHandoffCheckpoint,
    run: &RunId,
) -> serde_json::Value {
    serde_json::json!({
        "id": checkpoint.id,
        "run": run,
        "trace": format!("trace:{}:{}", run.as_str(), checkpoint.id.as_str()),
        "epoch": checkpoint.epoch,
        "interface_binding": checkpoint.interface_binding,
        "outbound_interface_bindings": checkpoint.outbound_interface_bindings,
        "artifact_bindings": checkpoint
            .artifact_bindings
            .iter()
            .map(|artifact| serde_json::json!({ "id": artifact }))
            .collect::<Vec<_>>(),
        "required_verified_artifacts": checkpoint.required_verified_artifacts
    })
}

fn pending_required_evidence(item: &PendingHandoffItem) -> Result<Option<(&str, &str)>, String> {
    match item.checkpoint.required_verified_artifacts.as_slice() {
        [] => Ok(None),
        [required] if required.artifact_label == item.artifact_label => {
            Ok(Some((required.mark.as_str(), required.verifier.as_str())))
        }
        [required] => Err(format!(
            "provider pending handoff artifact '{}' has label '{}' but its checkpoint requires '{}'",
            item.artifact_id.as_str(),
            item.artifact_label,
            required.artifact_label
        )),
        requirements => Err(format!(
            "provider pending handoff artifact '{}' checkpoint has {} verified-artifact requirements",
            item.artifact_id.as_str(),
            requirements.len()
        )),
    }
}

fn checkpoint_matches_pending(
    checkpoint: &octopus_runtime::run_record::CheckpointRecord,
    pending: &PendingHandoffCheckpoint,
) -> bool {
    checkpoint.id == pending.id
        && checkpoint.epoch.0 == pending.epoch
        && checkpoint.interface_binding.as_ref() == Some(&pending.interface_binding)
        && checkpoint.outbound_interface_bindings == pending.outbound_interface_bindings
        && checkpoint
            .artifact_bindings
            .iter()
            .map(|binding| &binding.id)
            .eq(pending.artifact_bindings.iter())
        && checkpoint
            .artifact_bindings
            .iter()
            .all(|binding| binding.access == Default::default())
        && checkpoint.required_verified_artifacts == pending.required_verified_artifacts
}

fn validate_pending_items_are_active(
    record: &RunRecord,
    provider: &InterfaceId,
    pending: &PendingHandoff,
) -> Result<usize, String> {
    let provider_state = record.interface_state.get(provider).ok_or_else(|| {
        format!(
            "provider run '{}' has no interface_state for '{}'",
            record.id.as_str(),
            provider.as_str()
        )
    })?;
    let frontier = frontier_checkpoint_ids(provider_state)?;
    if frontier.len() < 3 && provider_state.get(RUN_AUTHORING_BINDING).and_then(|value| value.get("writer_run")).is_none() {
        return Err(format!(
            "provider run '{}' pending handoff frontier must contain its outputs and two authoring checkpoints",
            record.id.as_str()
        ));
    }
    let active = record.active_checkpoints.iter().collect::<BTreeSet<_>>();
    for id in &frontier {
        let checkpoint_id = CheckpointId::new(id);
        if !active.contains(&checkpoint_id) {
            return Err(format!(
                "provider run '{}' frontier checkpoint '{}' is not active",
                record.id.as_str(),
                id
            ));
        }
        record.checkpoint(&checkpoint_id).ok_or_else(|| {
            format!(
                "provider run '{}' frontier checkpoint '{}' is absent",
                record.id.as_str(),
                id
            )
        })?;
    }
    let provider_bound = record
        .active_checkpoints
        .iter()
        .filter_map(|id| record.checkpoint(id))
        .filter(|checkpoint| checkpoint.inbound_interface() == Some(provider))
        .collect::<Vec<_>>();
    let topology: Option<BoundRunAuthoringTopology> = provider_state.get(RUN_AUTHORING_BINDING).cloned()
        .map(serde_json::from_value).transpose().map_err(|e|e.to_string())?;
    if let Some(topology) = &topology { topology.validate_identity(record,provider)?; }
    let retained = topology.iter().flat_map(|topology| &topology.checkpoints).filter(|template| template.role == RunAuthoringCheckpointRole::ProviderInterfaceOutput
        && !pending.items.iter().any(|item| item.checkpoint.interface_binding == template.interface_binding
            && item.checkpoint.outbound_interface_bindings == template.outbound_interface_bindings)).collect::<Vec<_>>();
    let mut retained_ids = BTreeSet::new();
    if !retained.is_empty() {
        let topology = topology.as_ref().ok_or("retained output has no immutable topology")?;
        let authoring = |role| -> Result<&RunAuthoringCheckpoint,String> {
            let matches = topology.checkpoints.iter().filter(|template| template.role == role).collect::<Vec<_>>();
            match matches.as_slice() { [one] => Ok(*one), _ => Err("retained output has no unique authoring boundary".into()) }
        };
        let frontier_records = frontier.iter().map(|id| record.checkpoint(&CheckpointId::new(id)).ok_or("retained frontier checkpoint is absent".to_string())).collect::<Result<Vec<_>,_>>()?;
        let roles = match &topology.writer_run {
            Some(binding) => binding.roles(record, provider, &topology.checkpoints)?,
            None => canonical_role_bindings(&frontier_records,authoring(RunAuthoringCheckpointRole::AuthoringCall)?,authoring(RunAuthoringCheckpointRole::AuthoringResult)?,&record.id)?,
        };
        for template in &retained {
            let matches = provider_bound.iter().filter(|checkpoint| checkpoint_record_matches_template(checkpoint,template,&record.id)
                && checkpoint_role_bindings_match(checkpoint,&roles,template)).collect::<Vec<_>>();
            let [checkpoint] = matches.as_slice() else { return Err("pending provider lost or duplicated its exact retained interface output/read bindings".into()); };
            if !retained_ids.insert(checkpoint.id.clone()) { return Err("retained interface outputs overlap one checkpoint".into()); }
        }
    }
    if provider_bound.len() != pending.items.len() + retained.len() {
        return Err(format!(
            "provider run '{}' pending handoff has {} active provider checkpoints for {} items",
            record.id.as_str(),
            provider_bound.len(),
            pending.items.len()
        ));
    }
    for checkpoint in &provider_bound {
        if !frontier.contains(checkpoint.id.as_str()) {
            return Err(format!(
                "provider run '{}' active provider checkpoint '{}' is outside the provider-owned frontier",
                record.id.as_str(),
                checkpoint.id.as_str()
            ));
        }
    }
    for item in &pending.items {
        let matches = provider_bound
            .iter()
            .filter(|checkpoint| checkpoint_matches_pending(checkpoint, &item.checkpoint))
            .count();
        if matches != 1 {
            return Err(format!(
                "provider run '{}' pending item '{}' matches {} active provider checkpoints",
                record.id.as_str(),
                item.artifact_id.as_str(),
                matches
            ));
        }
    }
    Ok(retained.len())
}
fn frontier_checkpoint_ids(
    values: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Result<BTreeSet<String>, String> {
    let ids = raw_frontier_checkpoint_ids(values)?;
    if ids.len() < 2 && values.get(RUN_AUTHORING_BINDING).and_then(|value| value.get("writer_run")).is_none() {
        return Err(format!(
            "provider interface_state.{FRONTIER_CHECKPOINT_IDS} must contain the two authoring checkpoints"
        ));
    }
    Ok(ids)
}

fn raw_frontier_checkpoint_ids(
    values: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Result<BTreeSet<String>, String> {
    let raw = values
        .get(FRONTIER_CHECKPOINT_IDS)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            format!("provider interface_state.{FRONTIER_CHECKPOINT_IDS} must be an array")
        })?;
    if raw.is_empty() {
        return Err(format!(
            "provider interface_state.{FRONTIER_CHECKPOINT_IDS} must not be empty"
        ));
    }
    let mut ids = BTreeSet::new();
    for value in raw {
        let id = value.as_str().ok_or_else(|| {
            format!("provider interface_state.{FRONTIER_CHECKPOINT_IDS} contains a non-string id")
        })?;
        let id = non_empty_string(id, FRONTIER_CHECKPOINT_IDS)?;
        if !ids.insert(id.clone()) {
            return Err(format!(
                "provider interface_state.{FRONTIER_CHECKPOINT_IDS} repeats checkpoint '{id}'"
            ));
        }
    }
    Ok(ids)
}

fn materialize_checkpoint(
    checkpoint: &RunAuthoringCheckpoint,
    id: &str,
    run: &RunId,
    artifact_bindings: &[serde_json::Value],
) -> serde_json::Value {
    let mut artifact_bindings = artifact_bindings.to_vec();
    for artifact in &checkpoint.static_artifact_bindings {
        let binding = serde_json::json!({"id":artifact});
        if !artifact_bindings.iter().any(|existing| existing.get("id") == binding.get("id")) {
            artifact_bindings.push(binding);
        }
    }
    serde_json::json!({
        "id": id,
        "run": run,
        "trace": format!("trace:{}:{id}", run.as_str()),
        "epoch": 0,
        "interface_binding": checkpoint.interface_binding,
        "outbound_interface_bindings": checkpoint.outbound_interface_bindings,
        "artifact_bindings": artifact_bindings,
        "required_verified_artifacts": checkpoint.required_verified_artifacts
    })
}

fn replace_owned_frontier(
    root: &mut serde_json::Map<String, serde_json::Value>,
    previous_frontier: &BTreeSet<String>,
    next_frontier: Vec<serde_json::Value>,
    next_frontier_ids: &[String],
) -> Result<(), String> {
    let checkpoints = root
        .get_mut("checkpoints")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| "provider run checkpoints are not an array".to_string())?;
    let mut retained = Vec::with_capacity(checkpoints.len() + next_frontier.len());
    let mut removed_checkpoints = BTreeSet::new();
    for checkpoint in std::mem::take(checkpoints) {
        let id = checkpoint_value_id(&checkpoint)?;
        if previous_frontier.contains(id) {
            removed_checkpoints.insert(id.to_string());
        } else {
            retained.push(checkpoint);
        }
    }
    if &removed_checkpoints != previous_frontier {
        return Err(format!(
            "provider-owned frontier checkpoint set drifted: state={:?}, run={:?}",
            previous_frontier, removed_checkpoints
        ));
    }
    retained.extend(next_frontier);
    *checkpoints = retained;

    let active = root
        .get_mut("active_checkpoints")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| "provider run active_checkpoints are not an array".to_string())?;
    let mut retained_active = Vec::with_capacity(active.len() + next_frontier_ids.len());
    let mut removed_active = BTreeSet::new();
    for checkpoint in std::mem::take(active) {
        let id = checkpoint.as_str().ok_or_else(|| {
            "provider run active_checkpoints contains a non-string id".to_string()
        })?;
        if previous_frontier.contains(id) {
            removed_active.insert(id.to_string());
        } else {
            retained_active.push(checkpoint);
        }
    }
    if &removed_active != previous_frontier {
        return Err(format!(
            "provider-owned active frontier set drifted: state={:?}, run={:?}",
            previous_frontier, removed_active
        ));
    }
    retained_active.extend(
        next_frontier_ids
            .iter()
            .map(|id| serde_json::Value::String(id.clone())),
    );
    *active = retained_active;
    Ok(())
}

fn replace_context_checkpoints(
    root: &mut serde_json::Map<String, serde_json::Value>,
    config: &RunAuthoringConfig,
    run: &RunId,
    checkpoint_prefix: &str,
    artifact_bindings: &[serde_json::Value],
) -> Result<(), String> {
    let active_values = root
        .get("active_checkpoints")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "provider run active_checkpoints are not an array".to_string())?;
    let mut active_ids = BTreeSet::new();
    for value in active_values {
        let id = value.as_str().ok_or_else(|| {
            "provider run active_checkpoints contains a non-string id".to_string()
        })?;
        if !active_ids.insert(id.to_string()) {
            return Err(format!(
                "provider run active_checkpoints repeats checkpoint '{}'",
                id
            ));
        }
    }
    let checkpoint_values = root
        .get("checkpoints")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "provider run checkpoints are not an array".to_string())?;
    let mut existing_ids = BTreeSet::new();
    for checkpoint in checkpoint_values {
        let id = checkpoint_value_id(checkpoint)?;
        if !existing_ids.insert(id.to_string()) {
            return Err(format!(
                "provider run checkpoints repeats checkpoint '{}'",
                id
            ));
        }
    }

    let mut replaced_ids = BTreeSet::new();
    let mut replacements = Vec::new();
    for template in config.checkpoints.iter().filter(|template| {
        template.role == RunAuthoringCheckpointRole::Stable && template.include_context
    }) {
        let matches = checkpoint_values
            .iter()
            .filter(|checkpoint| {
                checkpoint_value_id(checkpoint)
                    .ok()
                    .is_some_and(|id| active_ids.contains(id))
            })
            .filter_map(
                |checkpoint| match checkpoint_matches_template(checkpoint, template, run) {
                    Ok(true) => Some(Ok(checkpoint)),
                    Ok(false) => None,
                    Err(error) => Some(Err(error)),
                },
            )
            .collect::<Result<Vec<_>, _>>()?;
        if matches.len() != 1 {
            return Err(format!(
                "provider context template '{}' matches {} active checkpoints",
                template.name,
                matches.len()
            ));
        }
        let previous_id = checkpoint_value_id(matches[0])?.to_string();
        if !replaced_ids.insert(previous_id.clone()) {
            return Err(format!(
                "provider context templates resolve the same checkpoint '{}'",
                previous_id
            ));
        }
        let replacement_id = format!("{checkpoint_prefix}_context_{}", template.name);
        if existing_ids.contains(&replacement_id) && !replaced_ids.contains(&replacement_id) {
            return Err(format!(
                "provider context replacement checkpoint '{}' already exists",
                replacement_id
            ));
        }
        replacements.push((
            replacement_id.clone(),
            materialize_checkpoint(template, &replacement_id, run, artifact_bindings),
        ));
    }
    if replacements.is_empty() {
        return Err("run_authoring has no context-bearing stable checkpoints".to_string());
    }

    let checkpoints = root
        .get_mut("checkpoints")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| "provider run checkpoints are not an array".to_string())?;
    let mut retained = Vec::with_capacity(checkpoints.len() + replacements.len());
    let mut removed = BTreeSet::new();
    for checkpoint in std::mem::take(checkpoints) {
        let id = checkpoint_value_id(&checkpoint)?;
        if replaced_ids.contains(id) {
            removed.insert(id.to_string());
        } else {
            retained.push(checkpoint);
        }
    }
    if removed != replaced_ids {
        return Err(format!(
            "provider context checkpoint set drifted: expected={:?}, removed={:?}",
            replaced_ids, removed
        ));
    }
    retained.extend(
        replacements
            .iter()
            .map(|(_, checkpoint)| checkpoint.clone()),
    );
    *checkpoints = retained;

    let active = root
        .get_mut("active_checkpoints")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| "provider run active_checkpoints are not an array".to_string())?;
    let mut retained_active = Vec::with_capacity(active.len() + replacements.len());
    let mut removed_active = BTreeSet::new();
    for checkpoint in std::mem::take(active) {
        let id = checkpoint.as_str().ok_or_else(|| {
            "provider run active_checkpoints contains a non-string id".to_string()
        })?;
        if replaced_ids.contains(id) {
            removed_active.insert(id.to_string());
        } else {
            retained_active.push(checkpoint);
        }
    }
    if removed_active != replaced_ids {
        return Err(format!(
            "provider active context checkpoint set drifted: expected={:?}, removed={:?}",
            replaced_ids, removed_active
        ));
    }
    retained_active.extend(
        replacements
            .iter()
            .map(|(id, _)| serde_json::Value::String(id.clone())),
    );
    *active = retained_active;
    Ok(())
}

fn checkpoint_artifact_binding_set(
    checkpoint: &serde_json::Value,
) -> Result<Vec<ArtifactBinding>, String> {
    let mut bindings = checkpoint
        .get("artifact_bindings")
        .cloned()
        .map(serde_json::from_value::<Vec<ArtifactBinding>>)
        .transpose()
        .map_err(|error| format!("provider checkpoint artifact bindings are malformed: {error}"))?
        .unwrap_or_default();
    bindings.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    Ok(bindings)
}

fn replace_consumed_independent_input_checkpoint(
    root: &mut serde_json::Map<String, serde_json::Value>,
    template: &RunAuthoringCheckpoint,
    run: &RunId,
    checkpoint_prefix: &str,
    artifact_bindings: &[serde_json::Value],
) -> Result<(), String> {
    if template.role != RunAuthoringCheckpointRole::IndependentInput || template.include_context {
        return Err(
            "provider can re-arm only a consumed context-free independent input".to_string(),
        );
    }
    let active_values = root
        .get("active_checkpoints")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "provider run active_checkpoints are not an array".to_string())?;
    let active_ids = active_values
        .iter()
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                "provider run active_checkpoints contains a non-string id".to_string()
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if active_ids.len() != active_values.len() {
        return Err("provider run active_checkpoints repeats an id".to_string());
    }
    let checkpoint_values = root
        .get("checkpoints")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "provider run checkpoints are not an array".to_string())?;
    let matches = checkpoint_values
        .iter()
        .filter(|checkpoint| {
            checkpoint_value_id(checkpoint)
                .ok()
                .is_some_and(|id| active_ids.contains(id))
        })
        .filter_map(
            |checkpoint| match checkpoint_matches_template(checkpoint, template, run) {
                Ok(true) => Some(Ok(checkpoint)),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            },
        )
        .collect::<Result<Vec<_>, _>>()?;
    if matches.len() != 1 {
        return Err(format!(
            "provider consumed independent-input template '{}' matches {} active checkpoints",
            template.name,
            matches.len()
        ));
    }
    let replacement_id = format!("{checkpoint_prefix}_independent_input_{}", template.name);
    let replacement = materialize_checkpoint(template, &replacement_id, run, artifact_bindings);
    if checkpoint_artifact_binding_set(matches[0])? != checkpoint_artifact_binding_set(&replacement)? {
        return Err(format!(
            "provider consumed context-free independent input '{}' differs from its mandatory role bindings",
            template.name
        ));
    }
    let previous_id = checkpoint_value_id(matches[0])?.to_string();
    if checkpoint_values.iter().any(|checkpoint| {
        checkpoint_value_id(checkpoint).ok() == Some(replacement_id.as_str())
            && checkpoint_value_id(checkpoint).ok() != Some(previous_id.as_str())
    }) {
        return Err(format!(
            "provider independent-input replacement checkpoint '{}' already exists",
            replacement_id
        ));
    }
    let checkpoints = root
        .get_mut("checkpoints")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| "provider run checkpoints are not an array".to_string())?;
    let mut retained = Vec::with_capacity(checkpoints.len());
    let mut removed = 0usize;
    for checkpoint in std::mem::take(checkpoints) {
        if checkpoint_value_id(&checkpoint)? == previous_id {
            removed += 1;
        } else {
            retained.push(checkpoint);
        }
    }
    if removed != 1 {
        return Err(format!(
            "provider independent-input checkpoint '{}' was removed {} times",
            previous_id, removed
        ));
    }
    retained.push(replacement);
    *checkpoints = retained;

    let active = root
        .get_mut("active_checkpoints")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| "provider run active_checkpoints are not an array".to_string())?;
    let mut retained_active = Vec::with_capacity(active.len());
    let mut removed_active = 0usize;
    for checkpoint in std::mem::take(active) {
        if checkpoint.as_str() == Some(previous_id.as_str()) {
            removed_active += 1;
        } else {
            retained_active.push(checkpoint);
        }
    }
    if removed_active != 1 {
        return Err(format!(
            "provider active independent-input checkpoint '{}' was removed {} times",
            previous_id, removed_active
        ));
    }
    retained_active.push(serde_json::Value::String(replacement_id));
    *active = retained_active;
    Ok(())
}

fn replace_verified_input_checkpoints(
    root: &mut serde_json::Map<String, serde_json::Value>,
    current_config: &RunAuthoringConfig,
    next_config: &RunAuthoringConfig,
    run: &RunId,
    checkpoint_prefix: &str,
    artifact_bindings: &[serde_json::Value],
) -> Result<(), String> {
    let active_values = root
        .get("active_checkpoints")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "provider run active_checkpoints are not an array".to_string())?;
    let active_ids = active_values
        .iter()
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                "provider run active_checkpoints contains a non-string id".to_string()
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if active_ids.len() != active_values.len() {
        return Err("provider run active_checkpoints repeats an id".to_string());
    }
    let checkpoint_values = root
        .get("checkpoints")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "provider run checkpoints are not an array".to_string())?;
    let mut existing_ids = BTreeSet::new();
    for checkpoint in checkpoint_values {
        let id = checkpoint_value_id(checkpoint)?;
        if !existing_ids.insert(id.to_string()) {
            return Err(format!(
                "provider run checkpoints repeats checkpoint '{}'",
                id
            ));
        }
    }

    let mut removed_ids = BTreeSet::new();
    for template in current_config
        .checkpoints
        .iter()
        .filter(|template| template.role == RunAuthoringCheckpointRole::VerifiedInput)
    {
        let matches = checkpoint_values
            .iter()
            .filter(|checkpoint| {
                checkpoint_value_id(checkpoint)
                    .ok()
                    .is_some_and(|id| active_ids.contains(id))
            })
            .filter_map(
                |checkpoint| match checkpoint_matches_template(checkpoint, template, run) {
                    Ok(true) => Some(Ok(checkpoint)),
                    Ok(false) => None,
                    Err(error) => Some(Err(error)),
                },
            )
            .collect::<Result<Vec<_>, _>>()?;
        if matches.len() != 1 {
            return Err(format!(
                "provider verified-input template '{}' matches {} active checkpoints",
                template.name,
                matches.len()
            ));
        }
        let id = checkpoint_value_id(matches[0])?.to_string();
        if !removed_ids.insert(id.clone()) {
            return Err(format!(
                "provider verified-input templates resolve the same checkpoint '{}'",
                id
            ));
        }
    }

    let mut replacements = Vec::new();
    for template in next_config
        .checkpoints
        .iter()
        .filter(|template| template.role == RunAuthoringCheckpointRole::VerifiedInput)
    {
        let id = format!("{checkpoint_prefix}_verified_input_{}", template.name);
        if existing_ids.contains(&id) && !removed_ids.contains(&id) {
            return Err(format!(
                "provider verified-input checkpoint '{}' already exists",
                id
            ));
        }
        replacements.push((
            id.clone(),
            materialize_checkpoint(template, &id, run, artifact_bindings),
        ));
    }

    let checkpoints = root
        .get_mut("checkpoints")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| "provider run checkpoints are not an array".to_string())?;
    let mut retained = Vec::with_capacity(checkpoints.len() + replacements.len());
    let mut removed = BTreeSet::new();
    for checkpoint in std::mem::take(checkpoints) {
        let id = checkpoint_value_id(&checkpoint)?;
        if removed_ids.contains(id) {
            removed.insert(id.to_string());
        } else {
            retained.push(checkpoint);
        }
    }
    if removed != removed_ids {
        return Err(format!(
            "provider verified-input checkpoint set drifted: expected={:?}, removed={:?}",
            removed_ids, removed
        ));
    }
    retained.extend(
        replacements
            .iter()
            .map(|(_, checkpoint)| checkpoint.clone()),
    );
    *checkpoints = retained;

    let active = root
        .get_mut("active_checkpoints")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| "provider run active_checkpoints are not an array".to_string())?;
    let mut retained_active = Vec::with_capacity(active.len() + replacements.len());
    let mut removed_active = BTreeSet::new();
    for checkpoint in std::mem::take(active) {
        let id = checkpoint.as_str().ok_or_else(|| {
            "provider run active_checkpoints contains a non-string id".to_string()
        })?;
        if removed_ids.contains(id) {
            removed_active.insert(id.to_string());
        } else {
            retained_active.push(checkpoint);
        }
    }
    if removed_active != removed_ids {
        return Err(format!(
            "provider active verified-input set drifted: expected={:?}, removed={:?}",
            removed_ids, removed_active
        ));
    }
    retained_active.extend(
        replacements
            .iter()
            .map(|(id, _)| serde_json::Value::String(id.clone())),
    );
    *active = retained_active;
    Ok(())
}

fn checkpoint_matches_template(
    checkpoint: &serde_json::Value,
    template: &RunAuthoringCheckpoint,
    run: &RunId,
) -> Result<bool, String> {
    let object = checkpoint
        .as_object()
        .ok_or_else(|| "provider run checkpoint is not an object".to_string())?;
    let checkpoint_run = object
        .get("run")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "provider run checkpoint has no string run".to_string())?;
    let interface = serde_json::from_value::<AuthoredInterfaceBinding>(
        object
            .get("interface_binding")
            .cloned()
            .ok_or_else(|| "provider run checkpoint has no interface_binding".to_string())?,
    )
    .map_err(|error| format!("provider checkpoint inbound binding is malformed: {error}"))?;
    let outbound = serde_json::from_value::<Vec<AuthoredInterfaceBinding>>(
        object
            .get("outbound_interface_bindings")
            .cloned()
            .ok_or_else(|| {
                "provider run checkpoint has no outbound_interface_bindings".to_string()
            })?,
    )
    .map_err(|error| format!("provider checkpoint outbound bindings are malformed: {error}"))?;
    let required = serde_json::from_value::<Vec<RequiredVerifiedArtifact>>(
        object
            .get("required_verified_artifacts")
            .cloned()
            .ok_or_else(|| {
                "provider run checkpoint has no required_verified_artifacts".to_string()
            })?,
    )
    .map_err(|error| format!("provider checkpoint requirements are malformed: {error}"))?;
    Ok(checkpoint_run == run.as_str()
        && interface == template.interface_binding
        && outbound == template.outbound_interface_bindings
        && required == template.required_verified_artifacts)
}

fn current_context_artifacts(
    record: &RunRecord,
    config: &RunAuthoringConfig,
) -> Result<Vec<ArtifactId>, String> {
    let active = record.active_checkpoints.iter().collect::<BTreeSet<_>>();
    let mut canonical: Option<Vec<ArtifactId>> = None;
    for template in config.checkpoints.iter().filter(|template| {
        template.role == RunAuthoringCheckpointRole::Stable && template.include_context
    }) {
        let matches = record
            .checkpoints
            .iter()
            .filter(|checkpoint| active.contains(&checkpoint.id))
            .filter(|checkpoint| {
                checkpoint.interface_binding.as_ref() == Some(&template.interface_binding)
                    && checkpoint.outbound_interface_bindings
                        == template.outbound_interface_bindings
                    && checkpoint.required_verified_artifacts
                        == template.required_verified_artifacts
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err(format!(
                "provider context template '{}' matches {} active checkpoints in current run",
                template.name,
                matches.len()
            ));
        }
        let artifacts = matches[0]
            .artifact_bindings
            .iter()
            .map(|binding| binding.id.clone())
            .collect::<Vec<_>>();
        if let Some(expected) = canonical.as_ref() {
            if expected != &artifacts {
                return Err(format!(
                    "provider context template '{}' carries artifact bindings different from the other provider inputs",
                    template.name
                ));
            }
        } else {
            canonical = Some(artifacts);
        }
    }
    canonical.ok_or_else(|| {
        "run_authoring has no context-bearing stable checkpoints in current run".to_string()
    })
}

fn current_mandatory_role_artifacts(
    record: &RunRecord,
    config: &RunAuthoringConfig,
) -> Result<Vec<ArtifactId>, String> {
    Ok(current_mandatory_role_bindings(record, config)?
        .into_iter()
        .map(|binding| binding.id)
        .collect())
}

fn current_mandatory_role_bindings(
    record: &RunRecord,
    config: &RunAuthoringConfig,
) -> Result<Vec<ArtifactBinding>, String> {
    if let Some(binding) = &config.writer_run {
        return binding.roles(record, &config.provider_interface, &config.checkpoints);
    }
    let call =
        active_checkpoint_for_role(record, config, RunAuthoringCheckpointRole::AuthoringCall)?;
    let result =
        active_checkpoint_for_role(record, config, RunAuthoringCheckpointRole::AuthoringResult)?;
    if call.artifact_bindings != result.artifact_bindings {
        return Err(format!(
            "provider run '{}' authoring checkpoints carry different mandatory role bindings",
            record.id.as_str()
        ));
    }
    if call
        .artifact_bindings
        .iter()
        .any(|binding| !binding.access.read)
    {
        return Err(format!(
            "provider run '{}' authoring checkpoints contain a non-readable mandatory role binding",
            record.id.as_str()
        ));
    }
    Ok(call.artifact_bindings.clone())
}

fn persisted_pending_handoff(
    record: &RunRecord,
    provider: &InterfaceId,
) -> Result<Option<PendingHandoff>, String> {
    let state = record.interface_state.get(provider).ok_or_else(|| {
        format!(
            "provider run '{}' has no interface_state for '{}'",
            record.id.as_str(),
            provider.as_str()
        )
    })?;
    state
        .get("pending_handoff")
        .cloned()
        .map(|value| {
            let pending: PendingHandoff = serde_json::from_value(value).map_err(|error| {
                format!(
                    "provider run '{}' pending_handoff is malformed: {error}",
                    record.id.as_str()
                )
            })?;
            if pending.items.is_empty() {
                return Err(format!(
                    "provider run '{}' pending_handoff contains no items",
                    record.id.as_str()
                ));
            }
            let mut artifacts = BTreeSet::new();
            let mut checkpoints = BTreeSet::new();
            for (index, item) in pending.items.iter().enumerate() {
                non_empty_string(
                    item.artifact_label.as_str(),
                    &format!("pending_handoff.items[{index}].artifact_label"),
                )?;
                if ContentHash::from_hex(&item.artifact_content_hash).is_none() {
                    return Err(format!(
                        "provider run '{}' pending_handoff.items[{index}] has invalid artifact_content_hash",
                        record.id.as_str()
                    ));
                }
                if !artifacts.insert(item.artifact_id.clone()) {
                    return Err(format!(
                        "provider run '{}' pending_handoff repeats artifact '{}'",
                        record.id.as_str(),
                        item.artifact_id.as_str()
                    ));
                }
                non_empty_string(
                    item.checkpoint.id.as_str(),
                    &format!("pending_handoff.items[{index}].checkpoint.id"),
                )?;
                item.checkpoint.interface_binding.validate().map_err(|error| {
                    format!(
                        "provider run '{}' pending_handoff.items[{index}] inbound binding is invalid: {error}",
                        record.id.as_str()
                    )
                })?;
                if item.checkpoint.interface_binding.interface != *provider {
                    return Err(format!(
                        "provider run '{}' pending_handoff.items[{index}] checkpoint binds interface '{}' instead of provider '{}'",
                        record.id.as_str(),
                        item.checkpoint.interface_binding.interface.as_str(),
                        provider.as_str()
                    ));
                }
                if !item
                    .checkpoint
                    .interface_binding
                    .admits(&item.artifact_label)
                {
                    return Err(format!(
                        "provider run '{}' pending_handoff.items[{index}] inbound binding denies its own artifact label '{}'",
                        record.id.as_str(),
                        item.artifact_label
                    ));
                }
                let private_transport_dispatch = item.artifact_label
                    == PROVIDER_TRANSPORT_DISPATCH_LABEL
                    && item.checkpoint.outbound_interface_bindings.is_empty();
                if private_transport_dispatch
                    && (pending.terminal
                        || !item.checkpoint.required_verified_artifacts.is_empty())
                {
                    return Err(format!(
                        "provider run '{}' pending_handoff.items[{index}] private transport dispatch must be non-terminal and carry no verifier",
                        record.id.as_str()
                    ));
                }
                if item.checkpoint.outbound_interface_bindings.is_empty()
                    && !private_transport_dispatch
                {
                    let topology: BoundRunAuthoringTopology = serde_json::from_value(
                        state.get(RUN_AUTHORING_BINDING).ok_or("recorded provider output has no bound topology")?.clone()
                    ).map_err(|error| format!("recorded provider output topology is malformed: {error}"))?;
                    topology.validate_identity(record, provider)?;
                    if !topology.checkpoints.iter().any(|template| {
                        template.role == RunAuthoringCheckpointRole::ProviderArtifactOutput
                            && template.interface() == provider
                            && template.outbound_interface_bindings.is_empty()
                            && template.interface_binding.admits(&item.artifact_label)
                            && template.required_verified_artifacts == item.checkpoint.required_verified_artifacts
                    }) {
                        return Err(format!("provider run '{}' pending_handoff.items[{index}] has no declared recorded-output owner", record.id.as_str()));
                    }
                }
                let mut outbound = BTreeSet::new();
                for binding in &item.checkpoint.outbound_interface_bindings {
                    binding.validate().map_err(|error| {
                        format!(
                            "provider run '{}' pending_handoff.items[{index}] outbound binding is invalid: {error}",
                            record.id.as_str()
                        )
                    })?;
                    if !outbound.insert(&binding.interface) {
                        return Err(format!(
                            "provider run '{}' pending_handoff.items[{index}] checkpoint repeats outbound interface '{}'",
                            record.id.as_str(),
                            binding.interface.as_str()
                        ));
                    }
                }
                let mut bindings = BTreeSet::new();
                for artifact in &item.checkpoint.artifact_bindings {
                    non_empty_string(
                        artifact.as_str(),
                        &format!(
                            "pending_handoff.items[{index}].checkpoint.artifact_bindings"
                        ),
                    )?;
                    if !bindings.insert(artifact) {
                        return Err(format!(
                            "provider run '{}' pending_handoff.items[{index}] checkpoint repeats artifact binding '{}'",
                            record.id.as_str(),
                            artifact.as_str()
                        ));
                    }
                }
                let mut labels = BTreeSet::new();
                for (required_index, required) in item
                    .checkpoint
                    .required_verified_artifacts
                    .iter()
                    .enumerate()
                {
                    required
                        .validate(&format!(
                            "pending_handoff.items[{index}].checkpoint.required_verified_artifacts[{required_index}]"
                        ))
                        .map_err(|error| {
                            format!("provider pending checkpoint is malformed: {error}")
                        })?;
                    if !labels.insert(required.artifact_label.as_str()) {
                        return Err(format!(
                            "provider run '{}' pending_handoff.items[{index}] checkpoint repeats required artifact label '{}'",
                            record.id.as_str(),
                            required.artifact_label
                        ));
                    }
                }
                pending_required_evidence(item)?;
                if !checkpoints.insert(item.checkpoint.id.clone()) {
                    return Err(format!(
                        "provider run '{}' pending_handoff repeats checkpoint '{}'",
                        record.id.as_str(),
                        item.checkpoint.id.as_str()
                    ));
                }
            }
            Ok(pending)
        })
        .transpose()
}

fn pending_handoff(
    record: &RunRecord,
    provider: &InterfaceId,
) -> Result<Option<PendingHandoff>, String> {
    let pending = persisted_pending_handoff(record, provider)?;
    if let Some(pending) = pending.as_ref() {
        validate_pending_items_are_active(record, provider, pending)?;
    }
    Ok(pending)
}

fn checkpoint_value_id(checkpoint: &serde_json::Value) -> Result<&str, String> {
    checkpoint
        .as_object()
        .and_then(|checkpoint| checkpoint.get("id"))
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.trim().is_empty() && id.trim() == *id)
        .ok_or_else(|| "provider run checkpoint has no non-empty trimmed id".to_string())
}

fn optional_u64(
    values: &std::collections::BTreeMap<String, serde_json::Value>,
    key: &str,
) -> Result<u64, String> {
    match values.get(key) {
        None => Ok(0),
        Some(value) => value
            .as_u64()
            .ok_or_else(|| format!("provider interface_state.{key} must be an unsigned integer")),
    }
}

fn non_empty_string(value: &str, name: &str) -> Result<String, String> {
    if value.is_empty() || value.trim() != value {
        return Err(format!(
            "run_authoring {name} must be non-empty and trimmed"
        ));
    }
    Ok(value.to_string())
}

fn provider_context_label_sets(
    descriptor: &InterfaceDescriptor,
) -> Result<(BTreeSet<String>, BTreeSet<String>), String> {
    let interface_config = descriptor.interface_driver_config.as_ref().ok_or_else(|| {
        format!(
            "provider interface '{}' has no interface_driver_config",
            descriptor.id.as_str()
        )
    })?;
    let read_labels = |name: &str| -> Result<BTreeSet<String>, String> {
        interface_config
            .private_config
            .get(name)
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                format!(
                    "provider interface '{}' has no private_config.{name} array",
                    descriptor.id.as_str()
                )
            })?
            .iter()
            .enumerate()
            .map(|(index, value)| {
                non_empty_string(
                    value.as_str().unwrap_or_default(),
                    &format!("{name}[{index}]"),
                )
            })
            .collect::<Result<BTreeSet<_>, _>>()
    };
    let context_output_labels = read_labels("context_output_labels")?;
    if context_output_labels.is_empty() {
        return Err(format!(
            "provider interface '{}' context_output_labels must not be empty",
            descriptor.id.as_str()
        ));
    }
    let session_snapshot_labels = read_labels("session_snapshot_labels")?;
    if session_snapshot_labels.is_empty()
        || !session_snapshot_labels.is_subset(&context_output_labels)
    {
        return Err(format!(
            "provider interface '{}' session_snapshot_labels must be a non-empty subset of context_output_labels",
            descriptor.id.as_str()
        ));
    }
    Ok((context_output_labels, session_snapshot_labels))
}

fn validate_id_fragment(value: &str, name: &str) -> Result<(), String> {
    non_empty_string(value, name)?;
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(format!(
            "run_authoring {name} must contain only ASCII letters, digits, '_', '-' or '.'"
        ));
    }
    Ok(())
}

fn validate_transport_dispatch_payload(
    payload: &[u8],
    provider: &InterfaceId,
    source_movement_seq: u64,
) -> Result<ProviderTransportDispatch, String> {
    let dispatch: ProviderTransportDispatch = serde_json::from_slice(payload)
        .map_err(|error| format!("provider transport dispatch payload is malformed: {error}"))?;
    if dispatch.label != PROVIDER_TRANSPORT_DISPATCH_LABEL
        || dispatch.provider_interface != *provider
        || dispatch.source_movement_seq != source_movement_seq
    {
        return Err(
            "provider transport dispatch identity differs from the active handoff".to_string(),
        );
    }
    if dispatch.attempt == 0 {
        return Err("provider transport dispatch attempt must be positive".to_string());
    }
    for (name, value) in [
        ("method", dispatch.method.as_str()),
        ("transport", dispatch.transport.as_str()),
        ("endpoint.scheme", dispatch.endpoint.scheme.as_str()),
        ("endpoint.path", dispatch.endpoint.path.as_str()),
    ] {
        if value.is_empty() || value.trim() != value {
            return Err(format!(
                "provider transport dispatch {name} must be non-empty and trimmed"
            ));
        }
    }
    match dispatch.transport.as_str() {
        "http" if !canonical_http_method(&dispatch.method) => {
            return Err(
                "provider HTTP dispatch method must be a bounded canonical uppercase token"
                    .to_string(),
            );
        }
        "websocket" if !canonical_websocket_method(&dispatch.method) => {
            return Err(
                "provider WebSocket dispatch method must be a bounded canonical lowercase event token"
                    .to_string(),
            );
        }
        "process" | "local_ipc" if !canonical_local_method(&dispatch.method) => {
            return Err(
                "provider local dispatch method must be a bounded canonical method token"
                    .to_string(),
            );
        }
        _ => {}
    }
    if !dispatch
        .endpoint
        .scheme
        .bytes()
        .enumerate()
        .all(|(index, byte)| {
            if index == 0 {
                byte.is_ascii_lowercase()
            } else {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'+' | b'-' | b'.')
            }
        })
    {
        return Err("provider transport dispatch endpoint.scheme is not canonical".to_string());
    }
    if !dispatch.endpoint.path.starts_with('/') {
        return Err("provider transport dispatch endpoint.path must be absolute".to_string());
    }
    match dispatch.transport.as_str() {
        "http" | "websocket" => {
            let compatible = match dispatch.transport.as_str() {
                "http" => matches!(dispatch.endpoint.scheme.as_str(), "http" | "https"),
                "websocket" => matches!(dispatch.endpoint.scheme.as_str(), "ws" | "wss"),
                _ => false,
            };
            if !compatible {
                return Err(format!(
                    "provider transport dispatch '{}' transport is incompatible with '{}' scheme",
                    dispatch.transport, dispatch.endpoint.scheme
                ));
            }
            validate_network_endpoint(&dispatch.endpoint)?;
        }
        "process" => {
            if dispatch.endpoint.scheme != "process" {
                return Err(format!(
                    "provider transport dispatch '{}' transport is incompatible with '{}' scheme",
                    dispatch.transport, dispatch.endpoint.scheme
                ));
            }
            if dispatch.endpoint.host.is_empty()
                || dispatch.endpoint.port.is_some()
                || malformed_endpoint_host(&dispatch.endpoint.host)
            {
                return Err(
                    "provider process dispatch endpoint requires one canonical host identity and no port"
                        .to_string(),
                );
            }
        }
        "local_ipc" => {
            if dispatch.endpoint.scheme != "unix" {
                return Err(format!(
                    "provider transport dispatch '{}' transport is incompatible with '{}' scheme",
                    dispatch.transport, dispatch.endpoint.scheme
                ));
            }
            if !dispatch.endpoint.host.is_empty() || dispatch.endpoint.port.is_some() {
                return Err(
                    "provider local IPC dispatch endpoint requires an empty host and no port"
                        .to_string(),
                );
            }
        }
        _ => {
            return Err(format!(
                "provider transport dispatch transport '{}' is unsupported",
                dispatch.transport
            ));
        }
    }
    if !canonical_lower_hex_64(&dispatch.request.blake3) {
        return Err(
            "provider transport dispatch request.blake3 must be canonical lowercase hex"
                .to_string(),
        );
    }
    if dispatch.transport == "http" && dispatch.method == "GET" {
        let empty_hash = blake3::hash(&[]).to_hex().to_string();
        if dispatch.request.bytes != 0 || dispatch.request.blake3 != empty_hash {
            return Err(
                "provider transport dispatch GET must prove the canonical empty request body"
                    .to_string(),
            );
        }
    } else if dispatch.request.bytes == 0 {
        return Err(format!(
            "provider transport dispatch {} request.bytes must be positive",
            dispatch.method
        ));
    }
    if let Some(fingerprint) = &dispatch.credential_handle_fingerprint {
        if !canonical_lower_hex_64(fingerprint) {
            return Err(
                "provider transport dispatch credential_handle_fingerprint must be canonical lowercase hex"
                    .to_string(),
            );
        }
    }
    Ok(dispatch)
}

fn canonical_http_method(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().any(|byte| byte.is_ascii_uppercase())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'-')
}

fn canonical_websocket_method(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.split('.').all(|segment| {
            !segment.is_empty()
                && segment.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b'-')
                })
        })
}

fn canonical_local_method(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.split(['.', '/', ':']).all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'$'))
        })
}

fn malformed_endpoint_host(value: &str) -> bool {
    value
        .bytes()
        .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b'/' | b'@' | b'?' | b'#'))
}

fn validate_network_endpoint(endpoint: &ProviderTransportEndpoint) -> Result<(), String> {
    if endpoint.host.is_empty() || malformed_endpoint_host(&endpoint.host) {
        return Err("provider transport dispatch endpoint.host is malformed".to_string());
    }
    if endpoint.port == Some(0) {
        return Err("provider transport dispatch endpoint.port must be positive".to_string());
    }
    Ok(())
}

fn canonical_lower_hex_64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_handoff_completion_payload(
    payload: &[u8],
    provider: &InterfaceId,
    source_movement_seq: u64,
) -> Result<ProviderHandoffCompletion, String> {
    let completion: ProviderHandoffCompletion = serde_json::from_slice(payload)
        .map_err(|error| format!("provider handoff completion payload is malformed: {error}"))?;
    if completion.label != PROVIDER_HANDOFF_COMPLETE_LABEL
        || completion.provider_interface != *provider
        || completion.source_movement_seq != source_movement_seq
    {
        return Err(
            "provider handoff completion identity differs from the active handoff".to_string(),
        );
    }
    if completion.artifacts.is_empty() {
        return Err("provider handoff completion contains no terminal artifacts".to_string());
    }
    let mut artifacts = HashSet::new();
    for artifact in &completion.artifacts {
        let key = artifact_ref_key(artifact);
        if !artifacts.insert(key.clone()) {
            return Err(format!(
                "provider handoff completion repeats terminal artifact '{key}'"
            ));
        }
    }
    Ok(completion)
}

fn read_pinned_artifact_payload(
    ctx: &InterfaceLiftContext,
    artifact: &ArtifactRef,
    expected_label: &str,
) -> Result<Vec<u8>, String> {
    let (manifest, payload) = read_pinned_artifact(ctx, artifact)?;
    if manifest.label != expected_label {
        return Err(format!(
            "provider recovery artifact label '{}' differs from '{}'",
            manifest.label, expected_label
        ));
    }
    Ok(payload)
}

fn read_artifact_ref_payload(
    ctx: &InterfaceLiftContext,
    artifact: &ArtifactRef,
) -> Result<Vec<u8>, String> {
    read_artifact_ref_payload_from_cache(&ctx.blobs_dir, ctx.required_host_io_limits()?, artifact)
}

fn read_artifact_ref_payload_from_cache(
    blobs_dir: &Path, limits: InterfaceHostIoLimits, artifact: &ArtifactRef,
) -> Result<Vec<u8>, String> {
    let carrier_path = blobs_dir.join(artifact.content.to_hex());
    let carrier = octopus_interfaces::read_artifact_carrier_file_bounded(
        &carrier_path,
        limits.max_manifest_bytes.max(limits.max_payload_bytes),
    )
    .map_err(|error| {
        format!(
            "provider bound artifact carrier '{}' cannot be read: {error}",
            carrier_path.display()
        )
    })?;
    if ContentHash::of_bytes(&carrier) != artifact.content {
        return Err("provider bound artifact carrier hash does not match its ref".to_string());
    }
    let Ok(manifest) = serde_json::from_slice::<InterfaceArtifactManifest>(&carrier) else {
        if carrier.len() as u64 > limits.max_payload_bytes {
            return Err("provider bound artifact payload exceeds the declared limit".to_string());
        }
        return Ok(carrier);
    };
    if manifest.payload_path.as_os_str().is_empty() {
        if carrier.len() as u64 > limits.max_payload_bytes {
            return Err("provider bound artifact payload exceeds the declared limit".to_string());
        }
        return Ok(carrier);
    }
    if carrier.len() as u64 > limits.max_manifest_bytes {
        return Err("provider bound artifact manifest exceeds the declared limit".to_string());
    }
    let expected_id = format!("{}:{}", manifest.label, artifact.content.to_hex());
    if artifact.id.as_str() != expected_id {
        return Err("provider bound artifact id is not its pinned manifest identity".to_string());
    }
    let payload_hash = manifest
        .payload_content_hash
        .as_deref()
        .and_then(ContentHash::from_hex)
        .ok_or_else(|| {
            "provider bound artifact manifest has no canonical payload hash".to_string()
        })?;
    let payload_path = blobs_dir.join(payload_hash.to_hex());
    let payload = octopus_interfaces::read_artifact_carrier_file_bounded(
        &payload_path,
        limits.max_payload_bytes,
    )
    .map_err(|error| {
        format!(
            "provider bound artifact payload '{}' cannot be read: {error}",
            payload_path.display()
        )
    })?;
    if ContentHash::of_bytes(&payload) != payload_hash {
        return Err("provider bound artifact payload hash does not match its manifest".to_string());
    }
    Ok(payload)
}

fn read_pinned_artifact(
    ctx: &InterfaceLiftContext,
    artifact: &ArtifactRef,
) -> Result<(InterfaceArtifactManifest, Vec<u8>), String> {
    let limits = ctx.required_host_io_limits()?;
    read_pinned_artifact_from_cache(&ctx.blobs_dir, limits, artifact)
}

fn read_pinned_artifact_from_cache(
    blobs_dir: &Path,
    limits: InterfaceHostIoLimits,
    artifact: &ArtifactRef,
) -> Result<(InterfaceArtifactManifest, Vec<u8>), String> {
    read_pinned_artifact_with_bounds(blobs_dir,limits.max_manifest_bytes,limits.max_payload_bytes,artifact)
}

fn read_pinned_artifact_with_bounds(
    blobs_dir: &Path,
    max_manifest_bytes: u64,
    max_payload_bytes: u64,
    artifact: &ArtifactRef,
) -> Result<(InterfaceArtifactManifest, Vec<u8>), String> {
    let carrier_path = blobs_dir.join(artifact.content.to_hex());
    let carrier = octopus_interfaces::read_artifact_carrier_file_bounded(
        &carrier_path,
        max_manifest_bytes,
    )
    .map_err(|error| {
        format!(
            "provider recovery artifact carrier '{}' cannot be read: {error}",
            carrier_path.display()
        )
    })?;
    if ContentHash::of_bytes(&carrier) != artifact.content {
        return Err("provider recovery artifact carrier hash does not match its ref".to_string());
    }
    let manifest: InterfaceArtifactManifest =
        serde_json::from_slice(&carrier).map_err(|error| {
            format!("provider recovery artifact carrier is not a manifest: {error}")
        })?;
    let expected_id = format!("{}:{}", manifest.label, artifact.content.to_hex());
    if artifact.id.as_str() != expected_id {
        return Err(
            "provider recovery artifact id is not its pinned manifest identity".to_string(),
        );
    }
    let payload_hash = manifest
        .payload_content_hash
        .as_deref()
        .and_then(ContentHash::from_hex)
        .ok_or_else(|| "provider recovery manifest has no canonical payload hash".to_string())?;
    let payload_path = blobs_dir.join(payload_hash.to_hex());
    let payload = octopus_interfaces::read_artifact_carrier_file_bounded(
        &payload_path,
        max_payload_bytes,
    )
    .map_err(|error| {
        format!(
            "provider recovery artifact payload '{}' cannot be read: {error}",
            payload_path.display()
        )
    })?;
    if ContentHash::of_bytes(&payload) != payload_hash {
        return Err("provider recovery payload hash does not match its manifest".to_string());
    }
    Ok((manifest, payload))
}

fn artifact_identity(
    artifact: &InterfaceArtifactFile,
    var_dir: &Path,
) -> Result<HandoffArtifactIdentity, String> {
    let canonical_var_dir = var_dir.canonicalize().map_err(|error| {
        format!(
            "provider interface var dir '{}' cannot be canonicalized: {error}",
            var_dir.display()
        )
    })?;
    let canonical_manifest = artifact.manifest_path().canonicalize().map_err(|error| {
        format!(
            "provider output manifest '{}' cannot be canonicalized: {error}",
            artifact.manifest_path().display()
        )
    })?;
    let manifest_relative_path = canonical_manifest
        .strip_prefix(&canonical_var_dir)
        .map_err(|_| {
            format!(
                "provider output manifest '{}' escapes interface var dir '{}'",
                canonical_manifest.display(),
                canonical_var_dir.display()
            )
        })?
        .to_path_buf();
    let manifest_bytes = std::fs::read(&canonical_manifest).map_err(|error| {
        format!(
            "provider output manifest '{}' cannot be read: {error}",
            canonical_manifest.display()
        )
    })?;
    let manifest = artifact
        .read_manifest()
        .map_err(|error| format!("provider output manifest failed closed: {error}"))?;
    let content = ContentHash::of_bytes(&manifest_bytes);
    Ok(HandoffArtifactIdentity {
        artifact: ArtifactRef {
            id: ArtifactId::new(format!("{}:{}", manifest.label, content.to_hex())),
            content,
        },
        label: manifest.label,
        manifest_relative_path,
    })
}

fn load_pending_artifact(
    var_dir: &Path,
    pending: &PendingHandoffItem,
) -> Result<(InterfaceArtifactFile, ArtifactRef), String> {
    if pending.manifest_relative_path.as_os_str().is_empty()
        || pending.manifest_relative_path.is_absolute()
        || pending
            .manifest_relative_path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(
            "provider pending handoff manifest path is not a safe relative path".to_string(),
        );
    }
    let path = var_dir.join(&pending.manifest_relative_path);
    let artifact = InterfaceArtifactFile::new(&path).map_err(|error| {
        format!(
            "provider pending handoff manifest '{}' cannot be opened: {error}",
            path.display()
        )
    })?;
    let identity = artifact_identity(&artifact, var_dir)?;
    if identity.artifact.id != pending.artifact_id
        || identity.artifact.content.to_hex() != pending.artifact_content_hash
        || identity.label != pending.artifact_label
        || identity.manifest_relative_path != pending.manifest_relative_path
    {
        return Err(format!(
            "provider pending handoff artifact '{}' no longer matches its run-record evidence",
            pending.artifact_id.as_str()
        ));
    }
    Ok((artifact, identity.artifact))
}

fn append_artifact(artifacts: &mut Vec<ArtifactId>, artifact: ArtifactId) -> bool {
    if artifacts.iter().any(|current| current == &artifact) {
        false
    } else {
        artifacts.push(artifact);
        true
    }
}

fn artifact_ref_key(artifact: &ArtifactRef) -> String {
    format!("{}:{}", artifact.id.as_str(), artifact.content.to_hex())
}

#[derive(Default)]
struct SessionSourceLedgerEvidence {
    movements: BTreeSet<u64>,
    provider_arrivals: BTreeSet<u64>,
    provider_inputs: BTreeMap<u64, String>,
}

struct SessionContextEvidenceScope<'a> {
    blobs_dir: &'a Path,
    limits: InterfaceHostIoLimits,
    run: &'a RunId,
    provider: &'a InterfaceId,
    context_output_labels: &'a BTreeSet<String>,
    session_snapshot_labels: &'a BTreeSet<String>,
}

fn validate_session_context_evidence(
    context: &mut ResolvedSessionContext,
    scope: SessionContextEvidenceScope<'_>,
    session: &ProviderHandoffSession,
) -> Result<(), String> {
    if context.artifacts.is_empty() {
        return Ok(());
    }
    validate_resolved_session_context(context, "bound provider history", scope.provider)?;
    let sources = context
        .artifacts
        .iter()
        .map(|entry| artifact_ref_key(&entry.source_artifact));
    let evidence = session
        .index()?
        .session_evidence(scope.run, scope.provider, sources)?;
    for (index, entry) in context.artifacts.iter_mut().enumerate() {
        validate_session_source_payload(
            entry,
            scope.blobs_dir,
            scope.limits,
            scope.session_snapshot_labels,
            index,
            scope.provider,
        )?;
        let key = artifact_ref_key(&entry.source_artifact);
        let found = evidence.get(&key).ok_or_else(|| {
            format!("session context artifact[{index}] has no ledger evidence slot")
        })?;
        if let Some(label) = entry.source_movement_seq
            .and_then(|sequence| found.provider_inputs.get(&sequence))
        {
            if label != &entry.label {
                return Err(format!(
                    "session context artifact[{index}] label differs from its exact admitted input"
                ));
            }
            continue;
        }
        let eligible = if scope.context_output_labels.contains(&entry.label) {
            &found.provider_arrivals
        } else {
            &found.movements
        };
        if eligible.is_empty() {
            return Err(format!(
                "session context artifact[{index}] source '{}' has no eligible movement in run '{}'",
                entry.source_artifact.id.as_str(),
                scope.run.as_str()
            ));
        }
        match entry.source_movement_seq {
            Some(sequence) if !eligible.contains(&sequence) => {
                return Err(format!(
                    "session context artifact[{index}] source movement {sequence} does not prove its artifact in run '{}'",
                    scope.run.as_str()
                ));
            }
            Some(_) => {}
            None => {
                entry.source_movement_seq = eligible.iter().next().copied();
            }
        }
    }
    Ok(())
}

fn validate_session_source_payload(
    entry: &SessionContextArtifact,
    blobs_dir: &Path,
    limits: InterfaceHostIoLimits,
    session_snapshot_labels: &BTreeSet<String>,
    index: usize,
    provider: &InterfaceId,
) -> Result<(), String> {
    let source_path = blobs_dir.join(entry.source_artifact.content.to_hex());
    let carrier_limit = limits.max_manifest_bytes.max(limits.max_payload_bytes);
    let carrier =
        octopus_interfaces::read_artifact_carrier_file_bounded(&source_path, carrier_limit)
            .map_err(|error| {
                format!(
                    "session context artifact[{index}] source blob '{}' cannot be read: {error}",
                    source_path.display()
                )
            })?;
    let carrier_hash = ContentHash::of_bytes(&carrier);
    if carrier_hash != entry.source_artifact.content {
        return Err(format!(
            "session context artifact[{index}] source blob hash does not match its ArtifactRef"
        ));
    }
    let payload = match serde_json::from_slice::<InterfaceArtifactManifest>(&carrier) {
        Ok(manifest) if !manifest.payload_path.as_os_str().is_empty() => {
            if carrier.len() as u64 > limits.max_manifest_bytes {
                return Err(format!(
                    "session context artifact[{index}] source manifest exceeds the declared limit"
                ));
            }
            if manifest.label != entry.label {
                return Err(format!(
                    "session context artifact[{index}] label '{}' differs from source manifest label '{}'",
                    entry.label, manifest.label
                ));
            }
            let expected_id = format!("{}:{}", manifest.label, carrier_hash.to_hex());
            if entry.source_artifact.id.as_str() != expected_id {
                return Err(format!(
                    "session context artifact[{index}] source id is not the pinned manifest identity"
                ));
            }
            let payload_hash = manifest
                .payload_content_hash
                .as_deref()
                .and_then(ContentHash::from_hex)
                .ok_or_else(|| {
                    format!(
                        "session context artifact[{index}] source manifest has no valid payload hash"
                    )
                })?;
            let payload_path = blobs_dir.join(payload_hash.to_hex());
            let payload = octopus_interfaces::read_artifact_carrier_file_bounded(
                &payload_path,
                limits.max_payload_bytes,
            )
            .map_err(|error| {
                format!(
                    "session context artifact[{index}] payload blob '{}' cannot be read: {error}",
                    payload_path.display()
                )
            })?;
            if ContentHash::of_bytes(&payload) != payload_hash {
                return Err(format!(
                    "session context artifact[{index}] payload blob hash does not match its manifest"
                ));
            }
            payload
        }
        _ => {
            if carrier.len() as u64 > limits.max_payload_bytes {
                return Err(format!(
                    "session context artifact[{index}] source payload exceeds the declared limit"
                ));
            }
            carrier
        }
    };
    let mut value: serde_json::Value = serde_json::from_slice(&payload).map_err(|error| {
        format!("session context artifact[{index}] source payload is not JSON: {error}")
    })?;
    if session_snapshot_labels.contains(&entry.label) {
        let owner = value.get(SESSION_CONTEXT_FIELD)
            .and_then(|snapshot| snapshot.get("provider_interface"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("session context artifact[{index}] snapshot source has no provider identity"))?;
        if owner != provider.as_str() {
            if value != entry.payload {
                return Err(format!("session context artifact[{index}] foreign snapshot differs from its pinned source artifact"));
            }
            return Ok(());
        }
        let removed = value
            .as_object_mut()
            .and_then(|object| object.remove(SESSION_CONTEXT_FIELD));
        if removed.is_none() {
            return Err(format!(
                "session context artifact[{index}] snapshot source has no session context carrier"
            ));
        }
    }
    if value != entry.payload {
        return Err(format!(
            "session context artifact[{index}] payload differs from its pinned source artifact"
        ));
    }
    Ok(())
}

fn bound_session_snapshot(
    value: &serde_json::Value,
    artifact: &ArtifactRef,
) -> Result<Option<SessionContextSnapshot>, String> {
    let Some(raw) = value.get(SESSION_CONTEXT_FIELD) else {
        return Ok(None);
    };
    serde_json::from_value(raw.clone())
        .map(Some)
        .map_err(|error| {
            format!(
                "bound session context artifact '{}' is malformed: {error}",
                artifact.id.as_str()
            )
        })
}

pub fn payload_artifact_bindings(
    record: &RunRecord,
    provider: &InterfaceId,
) -> Result<Vec<ArtifactRef>, String> {
    let Some(value) = record.interface_state.get(provider).and_then(|state| state.get(RUN_AUTHORING_BINDING)) else {
        return Ok(Vec::new());
    };
    let topology: BoundRunAuthoringTopology = serde_json::from_value(value.clone())
        .map_err(|error| format!("payload selection has malformed bound authoring topology: {error}"))?;
    topology.validate_identity(record, provider)?;
    validate_payload_artifact_selection(&topology.payload_artifact_bindings)?;
    Ok(topology.payload_artifact_bindings)
}

pub fn validate_payload_artifact_selection(selected: &[ArtifactRef]) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for reference in selected {
        non_empty_string(reference.id.as_str(), "payload_artifact_bindings.id")?;
        if reference.id.as_str().trim() != reference.id.as_str() || !ids.insert(&reference.id) {
            return Err("payload_artifact_bindings contains an untrimmed or repeated identity".into());
        }
    }
    Ok(())
}

fn validate_payload_read_bindings(
    selected: &[ArtifactRef],
    checkpoint: &octopus_runtime::run_record::CheckpointRecord,
    references: &[ArtifactRef],
    readable: &BTreeSet<ArtifactId>,
    roles: &[String],
) -> Result<(), String> {
    validate_payload_artifact_selection(selected)?;
    for reference in selected {
        if roles.iter().any(|id| id == reference.id.as_str()) {
            return Err("payload selection collides with a formed role artifact".into());
        }
        let authored = checkpoint.artifact_bindings.iter().filter(|binding| binding.id == reference.id).collect::<Vec<_>>();
        let admitted = references.iter().filter(|bound| bound.id == reference.id).collect::<Vec<_>>();
        if authored.is_empty() && admitted.is_empty() { continue; }
        if authored.len() != 1 || !authored[0].access.read
            || admitted != vec![reference] || !readable.contains(&reference.id) {
            return Err(format!("payload artifact '{}' differs from its exact authored and admitted read binding", reference.id));
        }
    }
    Ok(())
}

pub fn context_artifact_payloads(
    record: &RunRecord,
    metadata: &InvocationMetadata,
    payloads: &[octopus_interfaces::ArtifactBindingPayload],
) -> Result<Vec<octopus_interfaces::ArtifactBindingPayload>, String> {
    let runtime = metadata.runtime.as_ref().ok_or("context projection has no admitted runtime coordinate")?;
    let mut selected = payload_artifact_bindings(record, &runtime.interface)?;
    for reference in input_artifacts::bound_sources(record, &runtime.interface, &runtime.checkpoint, &metadata.artifact_binding_refs)? {
        if !selected.contains(&reference) { selected.push(reference); }
    }
    if selected.is_empty() { return Ok(payloads.to_vec()); }
    if runtime.run != record.id || runtime.worker != record.worker {
        return Err("payload selection belongs to a different admitted run/worker".into());
    }
    let checkpoint = record.checkpoint(&runtime.checkpoint).ok_or("payload selection has no pinned admitted checkpoint")?;
    if checkpoint.epoch.0 != runtime.epoch || !record.active_checkpoints.contains(&checkpoint.id) {
        return Err("payload selection has a different pinned checkpoint epoch".into());
    }
    if payloads.iter().map(|bound| &bound.artifact).ne(metadata.artifact_binding_refs.iter()) {
        return Err("payload selection differs from actual admitted binding payloads".into());
    }
    let readable = metadata.grant.artifact_access.iter().filter(|(_, access)| access.read).map(|(id, _)| ArtifactId::new(id)).collect();
    validate_payload_read_bindings(&selected, checkpoint, &metadata.artifact_binding_refs, &readable, &metadata.grant.role_artifacts)?;
    Ok(payloads.iter().filter(|bound| !selected.contains(&bound.artifact)).cloned().collect())
}

fn bound_instruction_sources(
    payloads: &[octopus_interfaces::ArtifactBindingPayload],
    selected_files: &[ArtifactRef],
) -> Result<BTreeSet<ArtifactId>, String> {
    let bound = payloads.iter().map(|payload| (&payload.artifact.id, &payload.artifact))
        .collect::<BTreeMap<_, _>>();
    let mut sources = BTreeSet::new();
    for payload in payloads {
        if selected_files.contains(&payload.artifact) { continue; }
        let bytes = payload.read_bytes().map_err(|error| error.to_string())?;
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes.as_ref()) else { continue; };
        if value.get("label").and_then(serde_json::Value::as_str) != Some(CONTEXT_REFERENCE_LABEL) { continue; }
        let Some(refs) = value.get("sources") else { continue; };
        let refs: Vec<ArtifactRef> = serde_json::from_value(refs.clone()).map_err(|error| {
            format!("instruction artifact '{}' sources are malformed: {error}", payload.artifact.id)
        })?;
        for source in refs {
            if let Some(actual) = bound.get(&source.id) {
                if **actual != source || selected_files.contains(&source) {
                    return Err(format!("instruction source '{}' differs from its explicit context binding", source.id));
                }
                sources.insert(source.id);
            }
        }
    }
    Ok(sources)
}

fn collect_bound_context(
    ctx: &InterfaceLiftContext,
    static_context_artifacts: &mut Vec<ArtifactId>,
    session_context: &mut ResolvedSessionContext,
    session_snapshot_head: &mut Option<SessionSnapshotHead>,
    artifact: &ArtifactRef,
    payload: &[u8],
    provenance: BoundArtifactProvenance,
    session_snapshot_labels: &BTreeSet<String>,
    provider: &InterfaceId,
) -> Result<(), String> {
    collect_bound_context_from_cache(&ctx.blobs_dir, ctx.required_host_io_limits()?, static_context_artifacts, session_context, session_snapshot_head, artifact, payload, provenance, session_snapshot_labels, provider)
}

fn collect_bound_context_from_cache(
    blobs_dir: &Path,
    limits: InterfaceHostIoLimits,
    static_context_artifacts: &mut Vec<ArtifactId>,
    session_context: &mut ResolvedSessionContext,
    session_snapshot_head: &mut Option<SessionSnapshotHead>,
    artifact: &ArtifactRef,
    payload: &[u8],
    provenance: BoundArtifactProvenance,
    session_snapshot_labels: &BTreeSet<String>,
    provider: &InterfaceId,
) -> Result<(), String> {
    if artifact.id.as_str() == format!("{INSTRUCTION_TEXT_LABEL}:{}", artifact.content.to_hex()) {
        let (manifest, pinned_payload) = read_pinned_artifact_from_cache(blobs_dir, limits, artifact)?;
        if manifest.label != INSTRUCTION_TEXT_LABEL || pinned_payload != payload {
            return Err(format!("bound instruction '{}' differs from its pinned text carrier", artifact.id));
        }
        let text = std::str::from_utf8(payload)
            .map_err(|error| format!("bound instruction '{}' is not UTF-8: {error}", artifact.id))?;
        if text.trim().is_empty() {
            return Err(format!("bound instruction '{}' is empty", artifact.id));
        }
        append_artifact(static_context_artifacts, artifact.id.clone());
        return Ok(());
    }
    let value: serde_json::Value = serde_json::from_slice(payload).map_err(|error| {
        format!(
            "bound context artifact '{}' is not JSON: {error}",
            artifact.id.as_str()
        )
    })?;
    let label = value
        .get("label")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_string)
        .or_else(|| {
            artifact
                .id
                .as_str()
                .rsplit_once(':')
                .and_then(|(label, suffix)| {
                    (suffix == artifact.content.to_hex())
                        .then(|| label.trim().to_string())
                        .filter(|label| !label.is_empty())
                })
        });
    if let Some(snapshot) = bound_session_snapshot(&value, artifact)? {
        validate_session_snapshot(
            &snapshot,
            artifact.id.as_str(),
            &snapshot.provider_interface,
        )?;
        if snapshot.provider_interface != *provider {
            return Ok(());
        }
        let label = label.as_deref().ok_or_else(|| {
            format!(
                "bound session context artifact '{}' has no self-identifying label",
                artifact.id.as_str()
            )
        })?;
        if !session_snapshot_labels.contains(label) {
            return Err(format!(
                "bound artifact '{}' carries reserved session context outside descriptor-declared snapshot labels",
                artifact.id.as_str()
            ));
        }
        if provenance.provider_arrival_seq.is_none() {
            return Err(format!(
                "bound session context artifact '{}' has no provider arrival evidence",
                artifact.id.as_str()
            ));
        }
        if session_snapshot_head.is_some() {
            return Err(
                "provider invocation carries multiple current session snapshots".to_string(),
            );
        }
        if !session_context.artifacts.is_empty() {
            return Err(
                "provider invocation places dynamic context before its current session snapshot"
                    .to_string(),
            );
        }
        let resolved_artifacts = resolve_session_snapshot_chain(
            blobs_dir,
            limits,
            artifact,
            payload,
            provenance.provider_arrival_seq,
            session_snapshot_labels,
            provider,
        )?;
        let covered_artifact_count = resolved_artifacts.len().saturating_sub(1);
        let covered_fingerprint =
            session_artifacts_fingerprint(&resolved_artifacts[..covered_artifact_count])?;
        session_context.artifacts.extend(resolved_artifacts);
        *session_snapshot_head = Some(SessionSnapshotHead {
            artifact: artifact.clone(),
            covered_artifact_count,
            covered_fingerprint,
        });
        return Ok(());
    }
    if label
        .as_ref()
        .is_some_and(|label| session_snapshot_labels.contains(label))
    {
        return Err(format!(
            "bound session snapshot artifact '{}' has no session context carrier",
            artifact.id.as_str()
        ));
    }
    if label.as_deref() == Some(CONTEXT_REFERENCE_LABEL) {
        append_artifact(static_context_artifacts, artifact.id.clone());
        return Ok(());
    }
    let label = label.ok_or_else(|| {
        format!(
            "bound context artifact '{}' has no self-identifying label",
            artifact.id.as_str()
        )
    })?;
    session_context.artifacts.push(SessionContextArtifact {
        label,
        payload: value,
        source_artifact: artifact.clone(),
        source_movement_seq: provenance.movement_seq,
    });
    Ok(())
}

struct SessionSnapshotCarrier {
    snapshot: SessionContextSnapshot,
    label: String,
    payload: serde_json::Value,
}

pub fn resolve_session_snapshot_chain(
    blobs_dir: &Path,
    limits: InterfaceHostIoLimits,
    head: &ArtifactRef,
    head_payload: &[u8],
    head_movement_seq: Option<u64>,
    session_snapshot_labels: &BTreeSet<String>,
    provider: &InterfaceId,
) -> Result<Vec<SessionContextArtifact>, String> {
    let mut reverse = Vec::new();
    let mut seen = BTreeSet::new();
    let mut current_artifact = head.clone();
    let mut current_payload = head_payload.to_vec();
    let mut current_label = None;
    let mut current_movement_seq = head_movement_seq;
    loop {
        let key = artifact_ref_key(&current_artifact);
        if !seen.insert(key) {
            return Err(format!(
                "session snapshot '{}' contains a parent cycle",
                head.id.as_str()
            ));
        }
        let carrier = decode_session_snapshot_carrier(
            &current_artifact,
            &current_payload,
            current_label.as_deref(),
            session_snapshot_labels,
            provider,
        )?;
        let parent = carrier.snapshot.parent.clone();
        reverse.push((current_artifact.clone(), current_movement_seq, carrier));
        let Some(parent) = parent else {
            break;
        };
        let (manifest, payload) = read_pinned_artifact_from_cache(blobs_dir, limits, &parent)?;
        current_artifact = parent;
        current_payload = payload;
        current_label = Some(manifest.label);
        current_movement_seq = None;
    }
    reverse.reverse();
    let mut resolved = ResolvedSessionContext {
        provider_interface: provider.clone(),
        artifacts: Vec::new(),
    };
    let head_index = reverse.len().saturating_sub(1);
    for (index, (artifact, source_movement_seq, carrier)) in reverse.into_iter().enumerate() {
        resolved.artifacts.extend(carrier.snapshot.artifacts);
        if index == head_index {
            resolved.artifacts.push(SessionContextArtifact {
                label: carrier.label,
                payload: carrier.payload,
                source_artifact: artifact,
                source_movement_seq,
            });
        }
    }
    validate_resolved_session_context(&resolved, head.id.as_str(), provider)?;
    Ok(resolved.artifacts)
}

fn decode_session_snapshot_carrier(
    artifact: &ArtifactRef,
    payload: &[u8],
    pinned_label: Option<&str>,
    session_snapshot_labels: &BTreeSet<String>,
    provider: &InterfaceId,
) -> Result<SessionSnapshotCarrier, String> {
    let mut value: serde_json::Value = serde_json::from_slice(payload).map_err(|error| {
        format!(
            "session snapshot artifact '{}' payload is not JSON: {error}",
            artifact.id.as_str()
        )
    })?;
    let payload_label = value
        .get("label")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_string);
    let identity_label = artifact
        .id
        .as_str()
        .rsplit_once(':')
        .and_then(|(label, suffix)| (suffix == artifact.content.to_hex()).then_some(label))
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_string);
    let label = pinned_label
        .map(str::to_string)
        .or_else(|| payload_label.clone())
        .or_else(|| identity_label.clone())
        .ok_or_else(|| {
            format!(
                "session snapshot artifact '{}' has no self-identifying label",
                artifact.id.as_str()
            )
        })?;
    if pinned_label.is_some_and(|pinned| {
        payload_label
            .as_deref()
            .is_some_and(|payload| payload != pinned)
    }) || pinned_label.is_some_and(|pinned| {
        identity_label
            .as_deref()
            .is_some_and(|identity| identity != pinned)
    }) {
        return Err(format!(
            "session snapshot artifact '{}' label differs across its pinned carrier",
            artifact.id.as_str()
        ));
    }
    if !session_snapshot_labels.contains(&label) {
        return Err(format!(
            "session snapshot artifact '{}' label '{}' is not descriptor-declared",
            artifact.id.as_str(),
            label
        ));
    }
    let raw = value
        .as_object_mut()
        .ok_or_else(|| {
            format!(
                "session snapshot artifact '{}' payload is not an object",
                artifact.id.as_str()
            )
        })?
        .remove(SESSION_CONTEXT_FIELD)
        .ok_or_else(|| {
            format!(
                "session snapshot artifact '{}' has no session context carrier",
                artifact.id.as_str()
            )
        })?;
    let snapshot: SessionContextSnapshot = serde_json::from_value(raw).map_err(|error| {
        format!(
            "session snapshot artifact '{}' is malformed: {error}",
            artifact.id.as_str()
        )
    })?;
    validate_session_snapshot(&snapshot, artifact.id.as_str(), provider)?;
    Ok(SessionSnapshotCarrier {
        snapshot,
        label,
        payload: value,
    })
}

fn append_session_payload(
    session_context: &mut ResolvedSessionContext,
    label: &str,
    payload: &[u8],
    source_artifact: &ArtifactRef,
    source_movement_seq: Option<u64>,
    source: &str,
) -> Result<bool, String> {
    let label = non_empty_string(label, "session artifact label")?;
    let value = serde_json::from_slice(payload)
        .map_err(|error| format!("{source} session payload is not JSON: {error}"))?;
    let entry = SessionContextArtifact {
        label,
        payload: value,
        source_artifact: source_artifact.clone(),
        source_movement_seq,
    };
    if let Some(existing) = session_context.artifacts.iter()
        .find(|existing| existing.source_artifact == entry.source_artifact)
    {
        if !session_context_entries_agree(existing, &entry) {
            return Err(format!("{source} session payload conflicts with its recorded artifact '{}'", source_artifact.id));
        }
        return Ok(false);
    }
    session_context.artifacts.push(entry);
    Ok(true)
}

fn session_context_entries_agree(
    existing: &SessionContextArtifact,
    recovered: &SessionContextArtifact,
) -> bool {
    existing.source_artifact == recovered.source_artifact
        && existing.label == recovered.label
        && existing.payload == recovered.payload
        && !matches!((existing.source_movement_seq, recovered.source_movement_seq),
            (Some(left), Some(right)) if left != right)
}

fn reconcile_recovered_session_snapshot(
    existing: &[SessionContextArtifact],
    recovered: &mut Vec<SessionContextArtifact>,
    publications: &BTreeMap<String, u64>,
    source: &ArtifactRef,
) -> Result<bool, String> {
    let mismatch = |detail: &str| format!(
        "provider recovered snapshot '{}' differs from existing session context: {detail}",
        source.id.as_str()
    );
    let positions = existing.iter().enumerate().map(|(index, entry)| {
        (artifact_ref_key(&entry.source_artifact), index)
    }).collect::<BTreeMap<_, _>>();
    if positions.len() != existing.len() {
        return Err(mismatch("existing context repeats an artifact"));
    }
    let mut shared = BTreeSet::new();
    let mut last_shared = None;
    for candidate in recovered.iter_mut() {
        if let Some(index) = positions.get(&artifact_ref_key(&candidate.source_artifact)).copied() {
            if last_shared.is_some_and(|previous| previous >= index)
                || !session_context_entries_agree(&existing[index], candidate)
            {
                return Err(mismatch("shared artifacts changed content or order"));
            }
            candidate.source_movement_seq = candidate.source_movement_seq.or(existing[index].source_movement_seq);
            last_shared = Some(index);
            shared.insert(index);
        }
    }
    let omitted = shared.len() < existing.len();
    let additions = recovered.len() > shared.len();
    if !additions {
        return Ok(recovered.len() == existing.len());
    }
    if omitted && !publications.contains_key(&artifact_ref_key(source)) {
        return Err(mismatch("snapshot additions have no checked publication"));
    }
    let mut merged = Vec::with_capacity(existing.len() + recovered.len() - shared.len());
    let mut existing_position = 0;
    let mut pending = Vec::new();
    let append_gap = |merged: &mut Vec<SessionContextArtifact>, gap: &[SessionContextArtifact],
        pending: &mut Vec<SessionContextArtifact>| -> Result<(), String> {
        if !gap.is_empty() && !pending.is_empty() {
            let first = pending.first().and_then(|entry| entry.source_movement_seq)
                .ok_or_else(|| mismatch("inserted context has no admitted movement"))?;
            if gap.iter().any(|entry| entry.source_movement_seq.is_none_or(|sequence| sequence >= first)) {
                return Err(mismatch("existing and inserted context have conflicting movement order"));
            }
        }
        merged.extend_from_slice(gap);
        merged.append(pending);
        Ok(())
    };
    for mut candidate in recovered.iter().cloned() {
        if let Some(index) = positions.get(&artifact_ref_key(&candidate.source_artifact)).copied() {
            append_gap(&mut merged, &existing[existing_position..index], &mut pending)?;
            merged.push(candidate);
            existing_position = index + 1;
        } else {
            let admitted = publications.get(&artifact_ref_key(&candidate.source_artifact)).copied();
            if (omitted || last_shared.is_some_and(|index| existing_position <= index)) && admitted.is_none() {
                return Err(mismatch("inserted artifact has no checked admission"));
            }
            if let Some(admitted) = admitted {
                if candidate.source_movement_seq.is_some_and(|sequence| sequence != admitted) {
                    return Err(mismatch("inserted artifact changed its admitted movement"));
                }
                candidate.source_movement_seq = Some(admitted);
            }
            pending.push(candidate);
        }
    }
    append_gap(&mut merged, &existing[existing_position..], &mut pending)?;
    *recovered = merged;
    Ok(true)
}

fn merge_failed_source_context(
    existing: &[SessionContextArtifact], recovered: &[SessionContextArtifact], source: u64,
    invocation_source: Option<u64>,
) -> Result<Vec<SessionContextArtifact>, String> {
    let input_successor = invocation_source.filter(|sequence| *sequence != source).and_then(|sequence| {
        existing.iter().position(|entry| entry.source_movement_seq == Some(sequence))
            .and_then(|position| existing[position + 1..].iter().find(|entry| entry.source_movement_seq != Some(sequence)))
            .map(|entry| entry.source_artifact.clone())
    });
    let (input, existing): (Vec<_>, Vec<_>) = existing.iter().cloned().partition(|entry| {
        invocation_source.is_some_and(|sequence| sequence != source && entry.source_movement_seq == Some(sequence))
    });
    let source_ref = recovered.iter().find(|entry| entry.source_movement_seq == Some(source))
        .ok_or("recovered failed context has no exact original source entry")?.source_artifact.clone();
    let publications = recovered.iter().map(|entry| {
        let sequence = entry.source_movement_seq.ok_or("recovered failed history has no exact movement")?;
        if sequence > source { return Err("recovered failed history follows its source".to_string()); }
        Ok((artifact_ref_key(&entry.source_artifact), sequence))
    }).collect::<Result<BTreeMap<_, _>, String>>()?;
    if publications.len() != recovered.len() { return Err("recovered failed history repeats an artifact".into()); }
    let split = existing.iter().position(|entry| entry.source_movement_seq.is_some_and(|seq| seq > source))
        .unwrap_or(existing.len());
    if existing[split..].iter().any(|entry| entry.source_movement_seq.is_none_or(|seq| seq <= source)) {
        return Err("existing context interleaves later inputs with failed source history".into());
    }
    let mut merged = recovered.to_vec();
    if !reconcile_recovered_session_snapshot(&existing[..split], &mut merged, &publications, &source_ref)? {
        merged = existing[..split].to_vec();
    }
    for entry in &existing[split..] {
        if merged.iter().any(|prior| prior.source_artifact == entry.source_artifact) {
            return Err("later context repeats a recovered failed source artifact".into());
        }
        merged.push(entry.clone());
    }
    let mut input_position = match input_successor {
        Some(successor) => merged.iter().position(|entry| entry.source_artifact == successor)
            .ok_or("failed context recovery lost the queued input's successor")?,
        None => merged.len(),
    };
    for entry in input {
        if merged.iter().any(|prior| prior.source_artifact == entry.source_artifact) {
            return Err("queued invocation input is already part of the failed source history".into());
        }
        merged.insert(input_position, entry);
        input_position += 1;
    }
    Ok(merged)
}

fn take_unpublished_context_inputs(
    state: &mut HandoffState,
    source_movement_seq: u64,
) -> Result<Vec<SessionContextArtifact>, String> {
    let covered = match state.session_snapshot_head.as_ref() {
        Some(head) => {
            if state.session_context.artifacts.get(head.covered_artifact_count)
                .map(|entry| &entry.source_artifact) != Some(&head.artifact)
            {
                return Err("provider input recovery lost its pinned snapshot carrier".into());
            }
            head.covered_artifact_count + 1
        }
        None => 0,
    };
    let mut inputs = Vec::new();
    let mut position = 0;
    state.session_context.artifacts.retain(|entry| {
        let take = position >= covered && entry.source_movement_seq == Some(source_movement_seq);
        position += 1;
        if take { inputs.push(entry.clone()); }
        !take
    });
    Ok(inputs)
}

fn restore_consumed_context_inputs(
    state: &mut HandoffState,
    inputs: Vec<SessionContextArtifact>,
) -> Result<(), String> {
    for input in inputs {
        if let Some(existing) = state.session_context.artifacts.iter()
            .find(|entry| entry.source_artifact == input.source_artifact)
        {
            if !session_context_entries_agree(existing, &input) {
                return Err(format!(
                    "provider recovered input '{}' differs from existing session context",
                    input.source_artifact.id.as_str()
                ));
            }
            state.consumed_input = false;
        } else {
            append_artifact(&mut state.context_artifacts, input.source_artifact.id.clone());
            state.session_context.artifacts.push(input);
        }
    }
    Ok(())
}

fn validate_resolved_session_context(
    context: &ResolvedSessionContext,
    source: &str,
    provider: &InterfaceId,
) -> Result<(), String> {
    if &context.provider_interface != provider {
        return Err(format!(
            "session context from '{source}' belongs to provider '{}' instead of '{}'",
            context.provider_interface.as_str(),
            provider.as_str()
        ));
    }
    let mut sources = BTreeSet::new();
    for (index, artifact) in context.artifacts.iter().enumerate() {
        non_empty_string(
            &artifact.label,
            &format!("session context from '{source}' artifact[{index}].label"),
        )?;
        let key = artifact_ref_key(&artifact.source_artifact);
        if !sources.insert((key, artifact.label.clone())) {
            return Err(format!(
                "session context from '{source}' repeats artifact source at index {index}"
            ));
        }
    }
    Ok(())
}

fn validate_session_snapshot(
    snapshot: &SessionContextSnapshot,
    source: &str,
    provider: &InterfaceId,
) -> Result<(), String> {
    if snapshot.schema_version != 3 {
        return Err(format!(
            "session snapshot from '{source}' has unsupported schema_version {}",
            snapshot.schema_version
        ));
    }
    let resolved = ResolvedSessionContext {
        provider_interface: snapshot.provider_interface.clone(),
        artifacts: snapshot.artifacts.clone(),
    };
    validate_resolved_session_context(&resolved, source, provider)?;
    if let Some(parent) = snapshot.parent.as_ref() {
        if parent.id.as_str().trim().is_empty() || parent.id.as_str().trim() != parent.id.as_str() {
            return Err(format!(
                "session snapshot from '{source}' has an invalid parent artifact identity"
            ));
        }
    }
    Ok(())
}

fn session_artifacts_fingerprint(
    artifacts: &[SessionContextArtifact],
) -> Result<ContentHash, String> {
    serde_json::to_vec(artifacts)
        .map(|bytes| ContentHash::of_bytes(&bytes))
        .map_err(|error| format!("session context fingerprint could not be encoded: {error}"))
}

fn reconcile_resolved_session_snapshot_head(
    session_context: &ResolvedSessionContext,
    session_snapshot_head: &mut Option<SessionSnapshotHead>,
) -> Result<(), String> {
    let Some(head) = session_snapshot_head.as_ref() else {
        return Ok(());
    };
    if head.covered_artifact_count >= session_context.artifacts.len() {
        return Err(format!(
            "provider session snapshot head covers {} artifacts but resolved context has only {}",
            head.covered_artifact_count,
            session_context.artifacts.len()
        ));
    }
    if session_context.artifacts[head.covered_artifact_count].source_artifact != head.artifact {
        return Err(
            "provider resolved session context current carrier differs from its snapshot head"
                .to_string(),
        );
    }
    let grounded_fingerprint =
        session_artifacts_fingerprint(&session_context.artifacts[..head.covered_artifact_count])?;
    if grounded_fingerprint != head.covered_fingerprint {
        *session_snapshot_head = None;
    }
    Ok(())
}

fn session_snapshot_head_after_output(
    artifact: ArtifactRef,
    session_context: &ResolvedSessionContext,
) -> Result<SessionSnapshotHead, String> {
    let covered_artifact_count =
        session_context
            .artifacts
            .len()
            .checked_sub(1)
            .ok_or_else(|| {
                "provider snapshot output was not appended to session context".to_string()
            })?;
    if session_context.artifacts[covered_artifact_count].source_artifact != artifact {
        return Err(
            "provider snapshot output does not match the appended session carrier".to_string(),
        );
    }
    Ok(SessionSnapshotHead {
        artifact,
        covered_artifact_count,
        covered_fingerprint: session_artifacts_fingerprint(
            &session_context.artifacts[..covered_artifact_count],
        )?,
    })
}

fn session_snapshot_for_context(
    session_context: &ResolvedSessionContext,
    session_snapshot_head: Option<&SessionSnapshotHead>,
) -> Result<SessionContextSnapshot, String> {
    validate_resolved_session_context(
        session_context,
        "provider output",
        &session_context.provider_interface,
    )?;
    let covered_artifact_count = session_snapshot_head
        .map(|head| head.covered_artifact_count)
        .unwrap_or_default();
    if covered_artifact_count > session_context.artifacts.len() {
        return Err(format!(
            "provider session snapshot head covers {} artifacts but current context has only {}",
            covered_artifact_count,
            session_context.artifacts.len()
        ));
    }
    if let Some(head) = session_snapshot_head {
        if session_artifacts_fingerprint(&session_context.artifacts[..head.covered_artifact_count])?
            != head.covered_fingerprint
        {
            return Err(
                "provider session context no longer has its pinned snapshot prefix".to_string(),
            );
        }
        let carrier = session_context
            .artifacts
            .get(head.covered_artifact_count)
            .ok_or_else(|| {
                "provider session context lost its current snapshot carrier".to_string()
            })?;
        if carrier.source_artifact != head.artifact {
            return Err(
                "provider session context current carrier differs from its snapshot head"
                    .to_string(),
            );
        }
    }
    let snapshot = SessionContextSnapshot {
        schema_version: 3,
        provider_interface: session_context.provider_interface.clone(),
        parent: session_snapshot_head.map(|head| head.artifact.clone()),
        artifacts: session_context.artifacts[covered_artifact_count..].to_vec(),
    };
    validate_session_snapshot(
        &snapshot,
        "provider output",
        &session_context.provider_interface,
    )?;
    Ok(snapshot)
}

fn materialize_session_snapshot(
    artifact: &InterfaceArtifactFile,
    session_context: &ResolvedSessionContext,
    session_snapshot_head: Option<&SessionSnapshotHead>,
) -> Result<InterfaceArtifactFile, String> {
    let snapshot = session_snapshot_for_context(session_context, session_snapshot_head)?;
    let payload = artifact
        .read_payload_bytes()
        .map_err(|error| format!("provider output payload cannot be read: {error}"))?;
    let mut value: serde_json::Value = serde_json::from_slice(&payload)
        .map_err(|error| format!("provider output payload is not JSON: {error}"))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| "provider session snapshot output must be a JSON object".to_string())?;
    if object.contains_key(SESSION_CONTEXT_FIELD) {
        return Err(format!(
            "provider output already contains reserved field '{SESSION_CONTEXT_FIELD}'"
        ));
    }
    object.insert(
        SESSION_CONTEXT_FIELD.to_string(),
        serde_json::to_value(snapshot)
            .map_err(|error| format!("session context could not be encoded: {error}"))?,
    );
    let bytes = serde_json::to_vec(&value)
        .map_err(|error| format!("provider session snapshot could not be encoded: {error}"))?;
    artifact
        .rewrite_payload(&bytes)
        .map_err(|error| format!("provider session snapshot could not be materialized: {error}"))
}

fn checkpoint_prefix(run: &RunId, artifact: &ArtifactId, generation: u64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(run.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(artifact.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(generation.to_le_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("cp_{}", &digest[..20])
}

#[cfg(test)]
mod tests {
    use super::*;
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/ordered_source_replay.rs"));
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/independent_provider.rs"));
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/one_shot_progress.rs"));
    use octopus_descriptors::{EeId, VerifierId};
    use octopus_interfaces::{GrantProjection, RuntimeInvocationMetadata};
    use octopus_ledger::{MovementDraft, MovementLog, Seq};
    use octopus_ledger_index::LedgerSqliteIndex;

    fn test_verifier_package_pin() -> octopus_ledger::VerifierPackagePin {
        octopus_ledger::VerifierPackagePin {
            descriptor_content: octopus_ledger::ContentHash::of_bytes(b"test verifier descriptor"),
            script_content: octopus_ledger::ContentHash::of_bytes(b"test verifier script"),
        }
    }

    fn all_binding(interface: impl Into<String>) -> AuthoredInterfaceBinding {
        AuthoredInterfaceBinding::all(InterfaceId::new(interface))
    }

    fn names_binding(interface: impl Into<String>, names: &[&str]) -> AuthoredInterfaceBinding {
        AuthoredInterfaceBinding {
            interface: InterfaceId::new(interface),
            config: octopus_runtime::InterfaceBindingConfig {
                artifact_policy: BindingArtifactPolicy::Allow {
                    names: names.iter().map(|name| (*name).to_string()).collect(),
                    namespaces: Vec::new(),
                },
            },
        }
    }

    struct SnapshotMovementLog {
        rows: Vec<Movement>,
        durable_end_offset: u64,
        row_ranges: Vec<(u64, u64)>,
    }

    impl MovementLog for SnapshotMovementLog {
        fn append(&mut self, _draft: MovementDraft) -> Movement {
            unreachable!()
        }

        fn read_from(&self, from: Seq) -> Vec<Movement> {
            self.rows
                .iter()
                .filter(|movement| movement.seq >= from)
                .cloned()
                .collect()
        }

        fn len(&self) -> usize {
            self.rows.len()
        }

        fn durable_end_offset(&self) -> Option<u64> {
            Some(self.durable_end_offset)
        }

        fn durable_row_byte_range(&self, seq: Seq) -> Option<(u64, u64)> {
            self.row_ranges.get(seq.0 as usize).copied()
        }
    }

    fn typed_invocation_metadata(
        run: &RunId,
        checkpoint: &str,
        interface: &InterfaceId,
        epoch: u64,
    ) -> InvocationMetadata {
        InvocationMetadata::new(GrantProjection::default()).with_runtime(
            RuntimeInvocationMetadata {
                run: run.clone(),
                ee: EeId::new("ee_test"),
                worker: InterfaceId::new("system_worker_test"),
                checkpoint: CheckpointId::new(checkpoint),
                interface: interface.clone(),
                epoch,
                run_record_hash: None,
            },
        )
    }

    #[test]
    fn checked_prefix_stream_preserves_cursor_and_requires_every_row() {
        let dir = tempfile::tempdir().unwrap();
        let movements = dir.path().join("movements.jsonl");
        let index = dir.path().join("absent-index.sqlite");
        let blobs = dir.path().join("blobs");
        let bytes = (0..15_000).map(|seq| format!("{}\n", serde_json::to_string(&ledger_row(seq)).unwrap())).collect::<String>();
        std::fs::write(&movements, &bytes).unwrap();
        let evidence = CheckedRunEvidence { movements_path:&movements, ledger_index_path:&index, blobs_dir:&blobs,
            max_control_frame_bytes:1<<20, max_manifest_bytes:1<<20, max_payload_bytes:1<<26 };
        let mut reader = FileCheckedRunReader::open(&evidence, None).unwrap();
        reader.movement(0).unwrap().unwrap();
        let position = reader.reader.stream_position().unwrap();
        let started = std::time::Instant::now();
        let mut count = 0;
        reader.visit_admitted_prefix(15_000, &mut |row| { assert_eq!(row.seq.0, count); count += 1; Ok(()) }).unwrap();
        eprintln!("checked prefix: {count} rows without SQLite in {:?}", started.elapsed());
        assert_eq!((count, reader.next_sequence(), reader.reader.stream_position().unwrap(), reader.snapshot_bytes), (15_000, 1, position, 0));
        assert_eq!(reader.movement(1).unwrap().unwrap().seq.0, 1);
        for suffix in ["".to_string(), "{".to_string(), format!("{}\n", serde_json::to_string(&ledger_row(2)).unwrap())] {
            std::fs::write(&movements, format!("{}\n{suffix}", serde_json::to_string(&ledger_row(0)).unwrap())).unwrap();
            reader.visit_admitted_prefix(1, &mut |_| Ok(())).unwrap();
            assert!(reader.visit_admitted_prefix(2, &mut |_| Ok(())).is_err());
            assert_eq!(reader.next_sequence(), 2);
        }
    }

    fn transport_dispatch_payload(
        method: &str,
        transport: &str,
        source_movement_seq: u64,
        bytes: u64,
        digest: String,
        fingerprint: Option<&str>,
    ) -> Vec<u8> {
        let (scheme, host) = match transport {
            "websocket" => ("wss", "example.test"),
            "process" => ("process", "codex_app_server_cli"),
            "local_ipc" => ("unix", ""),
            _ => ("https", "example.test"),
        };
        serde_json::to_vec(&serde_json::json!({
            "label": PROVIDER_TRANSPORT_DISPATCH_LABEL,
            "provider_interface": "world_provider_test",
            "source_movement_seq": source_movement_seq,
            "attempt": 1,
            "method": method,
            "transport": transport,
            "endpoint": {
                "scheme": scheme,
                "host": host,
                "path": "/v1/dispatch"
            },
            "request": {
                "bytes": bytes,
                "blake3": digest
            },
            "credential_handle_fingerprint": fingerprint
        }))
        .unwrap()
    }

    #[test]
    fn transport_dispatch_contract_is_generic_across_network_process_and_local_ipc() {
        let provider = InterfaceId::new("world_provider_test");
        let empty = blake3::hash(&[]).to_hex().to_string();
        let get = transport_dispatch_payload("GET", "http", 7, 0, empty, Some(&"a".repeat(64)));
        validate_transport_dispatch_payload(&get, &provider, 7).unwrap();

        let request = br#"{"type":"response.create"}"#;
        let websocket = transport_dispatch_payload(
            "response.create",
            "websocket",
            7,
            request.len() as u64,
            blake3::hash(request).to_hex().to_string(),
            None,
        );
        validate_transport_dispatch_payload(&websocket, &provider, 7).unwrap();
        let malformed_websocket = transport_dispatch_payload(
            "response..create",
            "websocket",
            7,
            request.len() as u64,
            blake3::hash(request).to_hex().to_string(),
            None,
        );
        assert!(
            validate_transport_dispatch_payload(&malformed_websocket, &provider, 7)
                .err()
                .unwrap()
                .contains("canonical lowercase event token")
        );
        let alternate_websocket = transport_dispatch_payload(
            "response.cancel",
            "websocket",
            7,
            request.len() as u64,
            blake3::hash(request).to_hex().to_string(),
            None,
        );
        validate_transport_dispatch_payload(&alternate_websocket, &provider, 7).unwrap();
        let process = transport_dispatch_payload(
            "thread/start",
            "process",
            7,
            request.len() as u64,
            blake3::hash(request).to_hex().to_string(),
            None,
        );
        validate_transport_dispatch_payload(&process, &provider, 7).unwrap();
        let local_ipc = transport_dispatch_payload(
            "session/prompt",
            "local_ipc",
            7,
            request.len() as u64,
            blake3::hash(request).to_hex().to_string(),
            None,
        );
        validate_transport_dispatch_payload(&local_ipc, &provider, 7).unwrap();

        let bad_get = transport_dispatch_payload(
            "GET",
            "http",
            7,
            0,
            blake3::hash(b"not empty").to_hex().to_string(),
            None,
        );
        assert!(
            validate_transport_dispatch_payload(&bad_get, &provider, 7)
                .err()
                .unwrap()
                .contains("canonical empty request body")
        );
        let bad_fingerprint = transport_dispatch_payload(
            "POST",
            "http",
            7,
            1,
            blake3::hash(b"x").to_hex().to_string(),
            Some("raw-secret-handle"),
        );
        assert!(
            validate_transport_dispatch_payload(&bad_fingerprint, &provider, 7)
                .err()
                .unwrap()
                .contains("credential_handle_fingerprint")
        );
    }

    fn ledger_row(seq: u64) -> Movement {
        Movement {
            seq: octopus_ledger::Seq(seq),
            lamport: octopus_ledger::Lamport(seq + 1),
            recorded_at_unix_ms: seq + 1,
            trace: octopus_ledger::TraceId(format!("trace-index-{seq}")),
            kind: MovementKind::ArtifactMoved,
            from: Endpoint::Interface(InterfaceId::new("world_provider_test")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_test")),
            artifact: None,
            invocation_metadata: None,
        }
    }

    #[test]
    fn ledger_index_advances_only_complete_delta_and_poison_is_permanent() {
        use std::io::Write as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("movements.jsonl");
        let first = format!("{}\n", serde_json::to_string(&ledger_row(0)).unwrap());
        std::fs::write(&path, first.as_bytes()).unwrap();
        let mut index = ProviderLedgerEvidenceIndex::new(
            InterfaceId::new("world_provider_test"),
            path.clone(),
            1_048_576,
        );
        index.catch_up().unwrap();
        assert_eq!(index.next_seq, 1);
        assert_eq!(index.byte_offset, first.len() as u64);

        let second = serde_json::to_string(&ledger_row(1)).unwrap();
        let mut ledger = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        ledger.write_all(second.as_bytes()).unwrap();
        ledger.sync_all().unwrap();
        index.catch_up().unwrap();
        assert_eq!(index.next_seq, 1);
        assert_eq!(index.byte_offset, first.len() as u64);
        ledger.write_all(b"\n").unwrap();
        ledger.sync_all().unwrap();
        index.catch_up().unwrap();
        assert_eq!(index.next_seq, 2);

        drop(ledger);
        std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        let first_error = index.catch_up().unwrap_err();
        assert!(first_error.contains("shrank behind observed watermark"));
        assert_eq!(index.catch_up().unwrap_err(), first_error);

        let gap_path = dir.path().join("gap.jsonl");
        std::fs::write(
            &gap_path,
            format!("{}\n", serde_json::to_string(&ledger_row(1)).unwrap()),
        )
        .unwrap();
        let mut gap = ProviderLedgerEvidenceIndex::new(
            InterfaceId::new("world_provider_test"),
            gap_path,
            1_048_576,
        );
        assert!(
            gap.catch_up()
                .unwrap_err()
                .contains("expected movement seq 0, found 1")
        );

        let duplicate_path = dir.path().join("duplicate.jsonl");
        std::fs::write(
            &duplicate_path,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&ledger_row(0)).unwrap(),
                serde_json::to_string(&ledger_row(0)).unwrap()
            ),
        )
        .unwrap();
        let mut duplicate = ProviderLedgerEvidenceIndex::new(
            InterfaceId::new("world_provider_test"),
            duplicate_path,
            1_048_576,
        );
        assert!(
            duplicate
                .catch_up()
                .unwrap_err()
                .contains("expected movement seq 1, found 0")
        );
    }

    #[test]
    fn provider_session_index_accepts_only_exact_interface_movement_provenance() {
        use octopus_ledger::{Lamport, Seq, TraceId};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("movements.jsonl");
        let artifact = ArtifactRef {
            id: ArtifactId::new("provider.output:exact"),
            content: ContentHash::of_bytes(b"provider output"),
        };
        let metadata = typed_invocation_metadata(
            &RunId::new("run_test"),
            "client_output",
            &InterfaceId::new("world_provider_test"),
            0,
        );
        let rows = [
            Movement {
                seq: Seq(0),
                lamport: Lamport(1),
                recorded_at_unix_ms: 1,
                trace: TraceId("trace-legacy-outbound".to_string()),
                kind: MovementKind::InterfaceOutbound,
                from: Endpoint::Interface(InterfaceId::new("world_provider_test")),
                to: Endpoint::Interface(InterfaceId::new("world_client_test")),
                artifact: Some(artifact.clone()),
                invocation_metadata: Some(metadata.clone().into()),
            },
            Movement {
                seq: Seq(1),
                lamport: Lamport(2),
                recorded_at_unix_ms: 2,
                trace: TraceId("x".repeat(8_192)),
                kind: MovementKind::InterfaceOutbound,
                from: Endpoint::Interface(InterfaceId::new("world_foreign")),
                to: Endpoint::Interface(InterfaceId::new("world_foreign_target")),
                artifact: Some(artifact.clone()),
                invocation_metadata: Some(DeliveryVerdict::Admitted { seq: 1 }.into()),
            },
            Movement {
                seq: Seq(2),
                lamport: Lamport(3),
                recorded_at_unix_ms: 3,
                trace: TraceId("trace-exact-outbound".to_string()),
                kind: MovementKind::InterfaceOutbound,
                from: Endpoint::Interface(InterfaceId::new("world_client_test")),
                to: Endpoint::Interface(InterfaceId::new("world_provider_test")),
                artifact: Some(artifact.clone()),
                invocation_metadata: Some(metadata.into()),
            },
        ];
        let mut ledger = String::new();
        let mut row_ranges = Vec::new();
        for row in &rows {
            let start = ledger.len() as u64;
            ledger.push_str(&serde_json::to_string(row).unwrap());
            ledger.push('\n');
            row_ranges.push((start, ledger.len() as u64));
        }
        std::fs::write(&path, &ledger).unwrap();
        let mut index = ProviderLedgerEvidenceIndex::new(
            InterfaceId::new("world_provider_test"),
            path.clone(),
            1_048_576,
        );
        let key = artifact_ref_key(&artifact);
        let evidence = index
            .session_evidence(
                &RunId::new("run_test"),
                &InterfaceId::new("world_provider_test"),
                std::iter::once(key.clone()),
            )
            .unwrap();
        assert_eq!(evidence.get(&key).unwrap().movements, BTreeSet::from([2]));
        let provenance = index
            .bound_provenance(
                &RunId::new("run_test"),
                &InterfaceId::new("world_provider_test"),
                std::iter::once(&artifact),
            )
            .unwrap();
        assert_eq!(provenance.get(&key).unwrap().movement_seq, Some(2));

        let spotlight_path = dir.path().join("ledger-index.sqlite");
        let mut spotlight = LedgerSqliteIndex::open(&spotlight_path).unwrap();
        spotlight
            .ingest_from(&SnapshotMovementLog {
                rows: rows.to_vec(),
                durable_end_offset: ledger.len() as u64,
                row_ranges,
            })
            .unwrap();
        drop(spotlight);
        let mut scoped = ProviderLedgerEvidenceIndex::with_spotlight(
            InterfaceId::new("world_provider_test"),
            path,
            4_096,
            &spotlight_path,
        );
        let evidence = scoped
            .session_evidence(
                &RunId::new("run_test"),
                &InterfaceId::new("world_provider_test"),
                std::iter::once(key.clone()),
            )
            .unwrap();
        assert_eq!(evidence.get(&key).unwrap().movements, BTreeSet::from([2]));
        assert_eq!(scoped.next_seq, 3);
        assert_eq!(scoped.byte_offset, ledger.len() as u64);
    }

    fn one_shot_movement(
        seq: u64,
        artifact: &ArtifactRef,
        checkpoint: &str,
        kind: MovementKind,
    ) -> Movement {
        let invocation_metadata = match &kind {
            MovementKind::MarkOutcomeAdmitted(mark) => octopus_interfaces::Mark::passed(
                mark.clone(),
                CheckpointId::new(checkpoint),
                RunId::new("run_test"),
                0,
                artifact.clone(),
                VerifierId::new("artifact_exists"),
                test_verifier_package_pin(),
                serde_json::json!({}),
            )
            .into(),
            _ => typed_invocation_metadata(
                &RunId::new("run_test"),
                checkpoint,
                &InterfaceId::new("world_provider_test"),
                0,
            )
            .into(),
        };
        let from = Endpoint::Interface(InterfaceId::new("world_provider_test"));
        let to = Endpoint::Interface(InterfaceId::new("system_worker_test"));
        Movement {
            seq: octopus_ledger::Seq(seq),
            lamport: octopus_ledger::Lamport(seq + 1),
            recorded_at_unix_ms: seq + 1,
            trace: octopus_ledger::TraceId(format!("trace-one-shot-{seq}")),
            kind,
            from,
            to,
            artifact: Some(artifact.clone()),
            invocation_metadata: Some(invocation_metadata),
        }
    }

    fn pin_artifact(file: &InterfaceArtifactFile, blobs_dir: &Path) -> ArtifactRef {
        let identity = artifact_identity(file, file.manifest_path().parent().unwrap())
            .unwrap()
            .artifact;
        let manifest_bytes = std::fs::read(file.manifest_path()).unwrap();
        let manifest: InterfaceArtifactManifest = serde_json::from_slice(&manifest_bytes).unwrap();
        let payload = file.read_payload_bytes().unwrap();
        std::fs::write(blobs_dir.join(identity.content.to_hex()), manifest_bytes).unwrap();
        std::fs::write(
            blobs_dir.join(manifest.payload_content_hash.unwrap()),
            payload,
        )
        .unwrap();
        identity
    }

    #[test]
    fn foreign_snapshot_evidence_preserves_the_complete_source_payload() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = dir.path().join("blobs");
        std::fs::create_dir(&blobs).unwrap();
        let payload = serde_json::json!({
            "label": "interface.artifact.output", "text": "peer output",
            "session_context": {"schema_version":3,"provider_interface":"world_provider_peer","artifacts":[]}
        });
        let file = InterfaceArtifactFile::write(&dir.path().join("emitted"), "interface.artifact.output", &serde_json::to_vec(&payload).unwrap()).unwrap();
        let mut entry = SessionContextArtifact {
            label: "interface.artifact.output".into(), payload,
            source_artifact: pin_artifact(&file, &blobs), source_movement_seq: Some(8),
        };
        let limits = InterfaceHostIoLimits { max_control_frame_bytes: 1_048_576, max_stderr_bytes: 262_144, max_manifest_bytes: 1_048_576, max_payload_bytes: 1_048_576 };
        let labels = BTreeSet::from(["interface.artifact.output".into()]);
        let local = InterfaceId::new("world_provider_local");
        let peer = InterfaceId::new("world_provider_peer");
        validate_session_source_payload(&entry, &blobs, limits, &labels, 0, &local).unwrap();
        assert!(validate_session_source_payload(&entry, &blobs, limits, &labels, 0, &peer).is_err());
        entry.payload.as_object_mut().unwrap().remove(SESSION_CONTEXT_FIELD);
        assert!(validate_session_source_payload(&entry, &blobs, limits, &labels, 0, &local).is_err());
        validate_session_source_payload(&entry, &blobs, limits, &labels, 0, &peer).unwrap();
    }

    #[tokio::test]
    async fn one_shot_recovery_reuses_terminal_evidence_after_multiple_dispatches() {
        use std::io::Write as _;
        use tokio_util::sync::CancellationToken;

        let dir = tempfile::tempdir().unwrap();
        let var_dir = dir.path().join("var/provider");
        let blobs_dir = dir.path().join("ledger/blobs");
        let movements_path = dir.path().join("ledger/movements.jsonl");
        std::fs::create_dir_all(&blobs_dir).unwrap();
        let request = b"request";
        let dispatch_payload = transport_dispatch_payload(
            "POST",
            "http",
            0,
            request.len() as u64,
            blake3::hash(request).to_hex().to_string(),
            None,
        );
        let dispatch_file = InterfaceArtifactFile::write(
            var_dir.join("dispatch"),
            PROVIDER_TRANSPORT_DISPATCH_LABEL,
            &dispatch_payload,
        )
        .unwrap();
        let dispatch = pin_artifact(&dispatch_file, &blobs_dir);
        let mut dispatch_two_value: serde_json::Value =
            serde_json::from_slice(&dispatch_payload).unwrap();
        dispatch_two_value["attempt"] = serde_json::json!(2);
        let dispatch_two_file = InterfaceArtifactFile::write(
            var_dir.join("dispatch-2"),
            PROVIDER_TRANSPORT_DISPATCH_LABEL,
            &serde_json::to_vec(&dispatch_two_value).unwrap(),
        )
        .unwrap();
        let dispatch_two = pin_artifact(&dispatch_two_file, &blobs_dir);
        let dispatch_arrival =
            one_shot_movement(1, &dispatch, "dispatch_1", MovementKind::InterfaceArrival);
        let dispatch_mark = one_shot_movement(
            2,
            &dispatch,
            "dispatch_1",
            MovementKind::MarkOutcomeAdmitted(octopus_descriptors::MarkId::new(
                "provider.dispatch.admitted",
            )),
        );
        let dispatch_two_arrival = one_shot_movement(
            3,
            &dispatch_two,
            "dispatch_2",
            MovementKind::InterfaceArrival,
        );
        let dispatch_two_mark = one_shot_movement(
            4,
            &dispatch_two,
            "dispatch_2",
            MovementKind::MarkOutcomeAdmitted(octopus_descriptors::MarkId::new(
                "provider.dispatch.admitted",
            )),
        );
        std::fs::write(
            &movements_path,
            format!(
                "{}\n{}\n{}\n{}\n{}\n",
                serde_json::to_string(&ledger_row(0)).unwrap(),
                serde_json::to_string(&dispatch_arrival).unwrap(),
                serde_json::to_string(&dispatch_mark).unwrap(),
                serde_json::to_string(&dispatch_two_arrival).unwrap(),
                serde_json::to_string(&dispatch_two_mark).unwrap()
            ),
        )
        .unwrap();
        let (arrivals, mut arrival_rx) = tokio::sync::mpsc::channel(1);
        let ctx = InterfaceLiftContext::unprivileged(
            InterfaceId::new("world_provider_test"),
            dir.path().join("registry/interface/world_provider_test"),
            None,
            var_dir.clone(),
            movements_path.clone(),
            blobs_dir.clone(),
            dir.path().join("ledger/ledger-index.sqlite"),
            Default::default(),
            arrivals,
            CancellationToken::new(),
        )
        .with_host_io_limits(InterfaceHostIoLimits {
            max_control_frame_bytes: 1_048_576,
            max_stderr_bytes: 262_144,
            max_manifest_bytes: 1_048_576,
            max_payload_bytes: 1_048_576,
        })
        .unwrap();
        let session = ProviderHandoffSession::new(&ctx).unwrap();
        let result_reader = LedgerTailReader::open_named(&ctx, "run-authoring").unwrap();
        let handoff = ProviderOutputHandoff {
            invocation_metadata: InvocationMetadata::new(Default::default()),
            ctx,
            config: config_with_tool_routes(&[]),
            run_path: dir.path().join("runs/run_test.json"),
            hand_in_root: dir.path().join("hand-in"),
            var_dir: var_dir.clone(),
            source_movement_seq: 0,
            source_carries_context: true,
            failed_source: false,
            checked_settlement_output: None,
            context_output_labels: BTreeSet::new(),
            session_snapshot_labels: BTreeSet::new(),
            granted_tools: BTreeSet::new(),
            granted_interfaces: BTreeSet::new(),
            granted_mark_outcomes: BTreeSet::new(),
            bound_topology: test_bound_topology(&config_with_tool_routes(&[])),
            delivery_mode: HandoffDeliveryMode::OneShot(OneShotHandoffPlan {
                terminal: BTreeMap::from([(
                    "interface.artifact.output".to_string(),
                    OneShotHandoffSlot {
                        checkpoint: CheckpointId::new("terminal"),
                        epoch: 0,
                        required_mark: "provider.output.admitted".to_string(),
                        required_verifier: "artifact_exists".to_string(),
                    },
                )]),
                dispatch: BTreeMap::from([
                    (
                        1,
                        OneShotHandoffSlot {
                            checkpoint: CheckpointId::new("dispatch_1"),
                            epoch: 0,
                            required_mark: "provider.dispatch.admitted".to_string(),
                            required_verifier: "artifact_exists".to_string(),
                        },
                    ),
                    (
                        2,
                        OneShotHandoffSlot {
                            checkpoint: CheckpointId::new("dispatch_2"),
                            epoch: 0,
                            required_mark: "provider.dispatch.admitted".to_string(),
                            required_verifier: "artifact_exists".to_string(),
                        },
                    ),
                    (
                        3,
                        OneShotHandoffSlot {
                            checkpoint: CheckpointId::new("dispatch_3"),
                            epoch: 0,
                            required_mark: "provider.dispatch.admitted".to_string(),
                            required_verifier: "artifact_exists".to_string(),
                        },
                    ),
                ]),
                completion: OneShotHandoffSlot {
                    checkpoint: CheckpointId::new("completion"),
                    epoch: 0,
                    required_mark: "provider.handoff.completed".to_string(),
                    required_verifier: "artifact_exists".to_string(),
                },
            }),
            session,
            invocation_session_snapshot: None,
            state: Arc::new(tokio::sync::Mutex::new(HandoffState {
            validated_withdrawal_output: None,
                context_artifacts: Vec::new(),
                static_context_artifacts: Vec::new(),
                session_context: ResolvedSessionContext {
                    provider_interface: InterfaceId::new("world_provider_test"),
                    artifacts: Vec::new(),
                },
                session_snapshot_head: None,
                run_record_bytes: Vec::new(),
                run_record_evidence_seq: None,
                consumed_input: false,
                consumed_context_free_independent_input: None,
                consumed_verified_input: false,
            })),
            result_reader: Arc::new(tokio::sync::Mutex::new(result_reader)),
        };
        assert_eq!(
            handoff.recover_source().await.unwrap(),
            ProviderSourceRecovery::Interrupted {
                last_artifact_label: PROVIDER_TRANSPORT_DISPATCH_LABEL.to_string()
            }
        );

        let terminal_file = InterfaceArtifactFile::write(
            var_dir.join("terminal"),
            "interface.artifact.output",
            br#"{"text":"done"}"#,
        )
        .unwrap();
        let terminal = pin_artifact(&terminal_file, &blobs_dir);
        let terminal_arrival =
            one_shot_movement(5, &terminal, "terminal", MovementKind::InterfaceArrival);
        let terminal_mark = one_shot_movement(
            6,
            &terminal,
            "terminal",
            MovementKind::MarkOutcomeAdmitted(octopus_descriptors::MarkId::new(
                "provider.output.admitted",
            )),
        );
        let mut ledger = std::fs::OpenOptions::new()
            .append(true)
            .open(&movements_path)
            .unwrap();
        writeln!(
            ledger,
            "{}",
            serde_json::to_string(&terminal_arrival).unwrap()
        )
        .unwrap();
        writeln!(ledger, "{}", serde_json::to_string(&terminal_mark).unwrap()).unwrap();
        ledger.sync_all().unwrap();
        drop(ledger);
        let terminal_for_completion = terminal.clone();
        let movements_for_completion = movements_path.clone();
        let blobs_for_completion = blobs_dir.clone();
        let completion_delivery = async {
            let delivery = arrival_rx.recv().await.unwrap();
            let manifest = delivery.artifact.read_manifest().unwrap();
            assert_eq!(manifest.label, PROVIDER_HANDOFF_COMPLETE_LABEL);
            let completion = validate_handoff_completion_payload(
                &delivery.artifact.read_payload_bytes().unwrap(),
                &InterfaceId::new("world_provider_test"),
                0,
            )
            .unwrap();
            assert_eq!(completion.artifacts, vec![terminal_for_completion]);
            let completion_artifact = pin_artifact(&delivery.artifact, &blobs_for_completion);
            let completion_arrival = one_shot_movement(
                7,
                &completion_artifact,
                "completion",
                MovementKind::InterfaceArrival,
            );
            let completion_mark = one_shot_movement(
                8,
                &completion_artifact,
                "completion",
                MovementKind::MarkOutcomeAdmitted(octopus_descriptors::MarkId::new(
                    "provider.handoff.completed",
                )),
            );
            let mut ledger = std::fs::OpenOptions::new()
                .append(true)
                .open(&movements_for_completion)
                .unwrap();
            writeln!(
                ledger,
                "{}",
                serde_json::to_string(&completion_arrival).unwrap()
            )
            .unwrap();
            writeln!(
                ledger,
                "{}",
                serde_json::to_string(&completion_mark).unwrap()
            )
            .unwrap();
            ledger.sync_all().unwrap();
            delivery
                .verdict_tx
                .unwrap()
                .send(DeliveryVerdict::Admitted { seq: 7 })
                .unwrap();
        };
        let (recovery, ()) = tokio::join!(handoff.recover_source(), completion_delivery);
        assert_eq!(recovery.unwrap(), ProviderSourceRecovery::Completed);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), handoff.recover_source())
                .await
                .unwrap()
                .unwrap(),
            ProviderSourceRecovery::Completed
        );
        assert!(matches!(
            arrival_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        assert!(
            handoff
                .hand_transport_dispatch(dispatch_file)
                .await
                .unwrap_err()
                .contains("follows completion evidence")
        );

        let mut late_dispatch_value: serde_json::Value =
            serde_json::from_slice(&dispatch_payload).unwrap();
        late_dispatch_value["attempt"] = serde_json::json!(3);
        let late_dispatch_file = InterfaceArtifactFile::write(
            var_dir.join("late-dispatch"),
            PROVIDER_TRANSPORT_DISPATCH_LABEL,
            &serde_json::to_vec(&late_dispatch_value).unwrap(),
        )
        .unwrap();
        let late_dispatch = pin_artifact(&late_dispatch_file, &blobs_dir);
        let mut ledger = std::fs::OpenOptions::new()
            .append(true)
            .open(&movements_path)
            .unwrap();
        writeln!(
            ledger,
            "{}",
            serde_json::to_string(&one_shot_movement(
                9,
                &late_dispatch,
                "dispatch_3",
                MovementKind::InterfaceArrival,
            ))
            .unwrap()
        )
        .unwrap();
        writeln!(
            ledger,
            "{}",
            serde_json::to_string(&one_shot_movement(
                10,
                &late_dispatch,
                "dispatch_3",
                MovementKind::MarkOutcomeAdmitted(octopus_descriptors::MarkId::new(
                    "provider.dispatch.admitted",
                )),
            ))
            .unwrap()
        )
        .unwrap();
        ledger.sync_all().unwrap();
        assert!(
            handoff
                .recover_source()
                .await
                .unwrap_err()
                .contains("does not precede every terminal arrival")
        );
    }

    #[tokio::test]
    async fn one_shot_progress_then_terminal_batch_never_authors_the_run() {
        use std::io::Write as _;
        use tokio_util::sync::CancellationToken;

        let dir = tempfile::tempdir().unwrap();
        let var_dir = dir.path().join("var/provider");
        let movements_path = dir.path().join("ledger/movements.jsonl");
        let blobs_dir = dir.path().join("ledger/blobs");
        std::fs::create_dir_all(&blobs_dir).unwrap();
        std::fs::write(
            &movements_path,
            format!("{}\n", serde_json::to_string(&ledger_row(0)).unwrap()),
        )
        .unwrap();
        let answer = InterfaceArtifactFile::write(
            var_dir.join("terminal-answer"),
            "interface.artifact.output",
            br#"{"text":"done"}"#,
        )
        .unwrap();
        let reasoning = InterfaceArtifactFile::write(
            var_dir.join("terminal-reasoning"),
            "provider.reasoning",
            br#"{"summary":"because"}"#,
        )
        .unwrap();
        let expected_answer = artifact_identity(&answer, &var_dir).unwrap().artifact;
        let expected_reasoning = artifact_identity(&reasoning, &var_dir).unwrap().artifact;
        let (arrivals, mut arrival_rx) = tokio::sync::mpsc::channel(3);
        let ctx = InterfaceLiftContext::unprivileged(
            InterfaceId::new("world_provider_test"),
            dir.path().join("registry/interface/world_provider_test"),
            None,
            var_dir.clone(),
            movements_path.clone(),
            blobs_dir.clone(),
            dir.path().join("ledger/ledger-index.sqlite"),
            Default::default(),
            arrivals,
            CancellationToken::new(),
        )
        .with_host_io_limits(InterfaceHostIoLimits {
            max_control_frame_bytes: 1_048_576,
            max_stderr_bytes: 262_144,
            max_manifest_bytes: 1_048_576,
            max_payload_bytes: 1_048_576,
        })
        .unwrap();
        let session = ProviderHandoffSession::new(&ctx).unwrap();
        let result_reader = LedgerTailReader::open_named(&ctx, "run-authoring").unwrap();
        let run_path = dir.path().join("runs/run_test.json");
        let mut handoff = ProviderOutputHandoff {
            invocation_metadata: InvocationMetadata::new(Default::default()),
            ctx,
            config: config_with_tool_routes(&[]),
            run_path: run_path.clone(),
            hand_in_root: dir.path().join("hand-in"),
            var_dir,
            source_movement_seq: 0,
            source_carries_context: true,
            failed_source: false,
            checked_settlement_output: None,
            context_output_labels: BTreeSet::new(),
            session_snapshot_labels: BTreeSet::new(),
            granted_tools: BTreeSet::new(),
            granted_interfaces: BTreeSet::new(),
            granted_mark_outcomes: BTreeSet::new(),
            bound_topology: test_bound_topology(&config_with_tool_routes(&[])),
            delivery_mode: HandoffDeliveryMode::OneShot(OneShotHandoffPlan {
                terminal: BTreeMap::from([
                    (
                        "interface.artifact.output".to_string(),
                        OneShotHandoffSlot {
                            checkpoint: CheckpointId::new("terminal_answer"),
                            epoch: 0,
                            required_mark: "provider.output.admitted".to_string(),
                            required_verifier: "artifact_exists".to_string(),
                        },
                    ),
                    (
                        "provider.reasoning".to_string(),
                        OneShotHandoffSlot {
                            checkpoint: CheckpointId::new("terminal_reasoning"),
                            epoch: 0,
                            required_mark: "provider.reasoning.admitted".to_string(),
                            required_verifier: "artifact_exists".to_string(),
                        },
                    ),
                ]),
                dispatch: BTreeMap::from([(
                    1,
                    OneShotHandoffSlot {
                        checkpoint: CheckpointId::new("dispatch_1"),
                        epoch: 0,
                        required_mark: "provider.dispatch.admitted".to_string(),
                        required_verifier: "artifact_exists".to_string(),
                    },
                )]),
                completion: OneShotHandoffSlot {
                    checkpoint: CheckpointId::new("completion"),
                    epoch: 0,
                    required_mark: "provider.handoff.completed".to_string(),
                    required_verifier: "artifact_exists".to_string(),
                },
            }),
            session,
            invocation_session_snapshot: None,
            state: Arc::new(tokio::sync::Mutex::new(HandoffState {
            validated_withdrawal_output: None,
                context_artifacts: Vec::new(),
                static_context_artifacts: Vec::new(),
                session_context: ResolvedSessionContext {
                    provider_interface: InterfaceId::new("world_provider_test"),
                    artifacts: Vec::new(),
                },
                session_snapshot_head: None,
                run_record_bytes: b"not authored".to_vec(),
                run_record_evidence_seq: None,
                consumed_input: false,
                consumed_context_free_independent_input: None,
                consumed_verified_input: false,
            })),
            result_reader: Arc::new(tokio::sync::Mutex::new(result_reader)),
        };
        let first_terminal_seq = exercise_one_shot_progress(&mut handoff, &mut arrival_rx).await;
        let movements_for_arrival = movements_path.clone();
        let blobs_for_arrival = blobs_dir.clone();
        let answer_for_arrival = expected_answer.clone();
        let reasoning_for_arrival = expected_reasoning.clone();
        let arrival = tokio::spawn(async move {
            for index in 0..3u64 {
                let delivery = arrival_rx.recv().await.unwrap();
                let manifest = delivery.artifact.read_manifest().unwrap();
                let artifact = artifact_identity(
                    &delivery.artifact,
                    delivery.artifact.manifest_path().parent().unwrap(),
                )
                .unwrap()
                .artifact;
                assert_eq!(
                    pin_artifact(&delivery.artifact, &blobs_for_arrival),
                    artifact
                );
                let (checkpoint, mark) = match index {
                    0 => {
                        assert_eq!(manifest.label, "interface.artifact.output");
                        assert_eq!(artifact, answer_for_arrival);
                        ("terminal_answer", "provider.output.admitted")
                    }
                    1 => {
                        assert_eq!(manifest.label, "provider.reasoning");
                        assert_eq!(artifact, reasoning_for_arrival);
                        ("terminal_reasoning", "provider.reasoning.admitted")
                    }
                    2 => {
                        assert_eq!(manifest.label, PROVIDER_HANDOFF_COMPLETE_LABEL);
                        let completion = validate_handoff_completion_payload(
                            &delivery.artifact.read_payload_bytes().unwrap(),
                            &InterfaceId::new("world_provider_test"),
                            0,
                        )
                        .unwrap();
                        assert_eq!(
                            completion.artifacts,
                            vec![answer_for_arrival.clone(), reasoning_for_arrival.clone()]
                        );
                        ("completion", "provider.handoff.completed")
                    }
                    _ => unreachable!(),
                };
                let seq = first_terminal_seq + index * 2;
                let admitted =
                    one_shot_movement(seq, &artifact, checkpoint, MovementKind::InterfaceArrival);
                let mark = one_shot_movement(
                    seq + 1,
                    &artifact,
                    checkpoint,
                    MovementKind::MarkOutcomeAdmitted(octopus_descriptors::MarkId::new(mark)),
                );
                let mut ledger = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&movements_for_arrival)
                    .unwrap();
                writeln!(ledger, "{}", serde_json::to_string(&admitted).unwrap()).unwrap();
                writeln!(ledger, "{}", serde_json::to_string(&mark).unwrap()).unwrap();
                ledger.sync_all().unwrap();
                delivery
                    .verdict_tx
                    .unwrap()
                    .send(DeliveryVerdict::Admitted { seq })
                    .unwrap();
            }
        });
        assert_eq!(
            handoff
                .hand_batch(vec![
                    ProviderEmission {
                        artifact: answer,
                        route: ProviderEmissionRoute::ClientProjection,
                    },
                    ProviderEmission {
                        artifact: reasoning,
                        route: ProviderEmissionRoute::ClientProjection,
                    },
                ])
                .await
                .unwrap(),
            vec![
                ProviderHandoffOutcome::Admitted {
                    seq: first_terminal_seq,
                    artifact: expected_answer,
                },
                ProviderHandoffOutcome::Admitted {
                    seq: first_terminal_seq + 2,
                    artifact: expected_reasoning,
                }
            ]
        );
        arrival.await.unwrap();
        assert_eq!(
            handoff.recover_source().await.unwrap(),
            ProviderSourceRecovery::Completed
        );
        let late = InterfaceArtifactFile::write(
            handoff.var_dir.join("late-progress"),
            "hook.catalog",
            br#"{"hooks":[]}"#,
        )
        .unwrap();
        assert!(
            handoff
                .hand(ProviderEmission {
                    artifact: late,
                    route: ProviderEmissionRoute::ClientProjection
                })
                .await
                .unwrap_err()
                .contains("progress follows terminal evidence")
        );
        assert!(!run_path.exists());
        assert_eq!(
            handoff.state.lock().await.run_record_bytes,
            b"not authored".to_vec()
        );
    }

    fn session_entry(
        label: &str,
        payload: serde_json::Value,
        source: &str,
    ) -> SessionContextArtifact {
        let content = ContentHash::of_bytes(source.as_bytes());
        SessionContextArtifact {
            label: label.to_string(),
            payload,
            source_artifact: ArtifactRef {
                id: ArtifactId::new(format!("{label}:{}", content.to_hex())),
                content,
            },
            source_movement_seq: None,
        }
    }

    #[test]
    fn recovered_snapshot_preserves_checked_publication_order_and_rejects_conflicts() {
        let entry = |label, sequence| {
            let mut entry = session_entry(label, serde_json::json!({"sequence": sequence}), label);
            entry.source_movement_seq = Some(sequence);
            entry
        };
        let prior = vec![entry("input", 1), entry("tool.call", 5), entry("tool.result", 7)];
        let snapshot = vec![prior[0].clone(), prior[1].clone(), entry("reasoning", 9),
            prior[2].clone(), entry("output", 10)];
        let source = snapshot[4].source_artifact.clone();
        let publications = BTreeMap::from([(artifact_ref_key(&snapshot[2].source_artifact), 9)]);
        let mut recovered = snapshot.clone();
        assert!(reconcile_recovered_session_snapshot(&prior, &mut recovered, &publications, &source).unwrap());
        assert_eq!(recovered.iter().map(|entry| entry.source_movement_seq.unwrap()).collect::<Vec<_>>(), [1, 5, 9, 7, 10]);
        let exact = session_artifacts_fingerprint(&recovered).unwrap();
        assert!(reconcile_recovered_session_snapshot(&recovered.clone(), &mut recovered, &publications, &source).unwrap());
        assert_eq!(session_artifacts_fingerprint(&recovered).unwrap(), exact);
        assert!(reconcile_recovered_session_snapshot(&prior, &mut snapshot.clone(), &BTreeMap::new(), &source).is_err());
        let wrong_publication = BTreeMap::from([(artifact_ref_key(&snapshot[2].source_artifact), 8)]);
        assert!(reconcile_recovered_session_snapshot(&prior, &mut snapshot.clone(), &wrong_publication, &source).is_err());
        for corruption in ["payload", "source", "sequence", "label", "order"] {
            let mut invalid = snapshot.clone();
            match corruption {
                "payload" => invalid[3].payload = serde_json::json!({"corrupt": true}),
                "source" => invalid[3].source_artifact = entry("other", 7).source_artifact,
                "sequence" => invalid[3].source_movement_seq = Some(8),
                "label" => invalid[3].label = "other".into(),
                "order" => invalid.swap(1, 3),
                _ => unreachable!(),
            }
            assert!(reconcile_recovered_session_snapshot(&prior, &mut invalid, &publications, &source).is_err(), "{corruption}");
        }
        let mut ungrounded = snapshot.clone();
        ungrounded[2].source_movement_seq = None;
        assert!(reconcile_recovered_session_snapshot(&prior, &mut ungrounded, &publications, &source).unwrap());
        assert_eq!(ungrounded[2].source_movement_seq, Some(9));
        let mut extended = snapshot.clone();
        extended.push(entry("next.input", 11));
        assert!(!reconcile_recovered_session_snapshot(&extended, &mut snapshot.clone(), &BTreeMap::new(), &source).unwrap());
        let mut admitted = prior.clone();
        admitted.insert(2, entry("peer.input", 6));
        assert!(reconcile_recovered_session_snapshot(&admitted, &mut snapshot.clone(), &publications, &source).is_err());
        let mut checked = publications.clone();
        checked.insert(artifact_ref_key(&source), 10);
        let mut repaired = snapshot.clone();
        assert!(reconcile_recovered_session_snapshot(&admitted, &mut repaired, &checked, &source).unwrap());
        assert_eq!(repaired.iter().map(|entry| entry.source_movement_seq.unwrap()).collect::<Vec<_>>(), [1, 5, 6, 9, 7, 10]);
        let retained = repaired.clone();
        assert!(!reconcile_recovered_session_snapshot(&retained, &mut snapshot.clone(), &BTreeMap::new(), &source).unwrap());
        let mut ambiguous = admitted.clone();
        ambiguous[2].source_movement_seq = Some(11);
        assert!(reconcile_recovered_session_snapshot(&ambiguous, &mut snapshot.clone(), &checked, &source).is_err());
        let missing = (20..54).map(|seq| {
            let label = format!("history.{seq}");
            let mut item = session_entry(&label, serde_json::json!({"seq":seq}), &label);
            item.source_movement_seq = Some(seq);
            item
        }).collect::<Vec<_>>();
        let result = entry("completed.result", 68);
        let mut full = prior.clone();
        full.extend(missing);
        full.push(result.clone());
        let mut bound = prior.clone();
        bound.extend([entry("continuity", 80), entry("later.input", 98)]);
        let restored = merge_failed_source_context(&bound, &full, 68, Some(98)).unwrap();
        assert_eq!(session_artifacts_fingerprint(&restored[..full.len()]).unwrap(), session_artifacts_fingerprint(&full).unwrap());
        assert_eq!(session_artifacts_fingerprint(&restored[full.len()..]).unwrap(), session_artifacts_fingerprint(&bound[prior.len()..]).unwrap());
        assert_eq!(restored.iter().filter(|item| item.source_artifact == result.source_artifact).count(), 1);
        let mut published = restored.clone();
        let result_position = published.iter().position(|item| item.source_artifact == result.source_artifact).unwrap();
        published.insert(result_position, entry("provider.tool.call", 67));
        let retained_publication = merge_failed_source_context(&published, &full, 68, Some(98)).unwrap();
        assert_eq!(session_artifacts_fingerprint(&retained_publication).unwrap(), session_artifacts_fingerprint(&published).unwrap());
        assert_eq!(session_artifacts_fingerprint(&merge_failed_source_context(&restored, &full, 68, Some(98)).unwrap()).unwrap(), session_artifacts_fingerprint(&restored).unwrap());
        let mut conflicting = bound.clone();
        conflicting[1].payload = serde_json::json!({"wrong":true});
        assert!(merge_failed_source_context(&conflicting, &full, 68, Some(98)).is_err());
        let mut reversed = full.clone();
        reversed.swap(0, 1);
        assert!(merge_failed_source_context(&bound, &reversed, 68, Some(98)).is_err());
        let mut unproven = full.clone();
        unproven[3].source_movement_seq = None;
        assert!(merge_failed_source_context(&bound, &unproven, 68, Some(98)).is_err());
        let mut early_input = bound.clone();
        early_input.last_mut().unwrap().source_movement_seq = Some(60);
        let late_result = merge_failed_source_context(&early_input, &full, 68, Some(60)).unwrap();
        assert_eq!(session_artifacts_fingerprint(&late_result[..full.len()]).unwrap(), session_artifacts_fingerprint(&full).unwrap());
        assert_eq!(late_result.last().unwrap().source_movement_seq, Some(60));
        for (mut completed, input) in [(restored.clone(), 98), (late_result, 60)] {
            completed.push(entry("current.reply", 100));
            let replayed = merge_failed_source_context(&completed, &full, 68, Some(input)).unwrap();
            assert_eq!(session_artifacts_fingerprint(&replayed).unwrap(), session_artifacts_fingerprint(&completed).unwrap());
        }
        let dir = tempfile::tempdir().unwrap();
        let output = InterfaceArtifactFile::write(dir.path(),
            "interface.context.updated", br#"{"label":"interface.context.updated"}"#).unwrap();
        let context = ResolvedSessionContext { provider_interface: InterfaceId::new("provider"), artifacts: repaired };
        let rebased = materialize_session_snapshot(&output, &context, None).unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&rebased.read_payload_bytes().unwrap()).unwrap();
        let rebased: SessionContextSnapshot = serde_json::from_value(payload[SESSION_CONTEXT_FIELD].clone()).unwrap();
        assert!(rebased.parent.is_none());
        assert_eq!(session_artifacts_fingerprint(&rebased.artifacts).unwrap(), session_artifacts_fingerprint(&retained).unwrap());
    }

    #[test]
    fn recovered_snapshot_keeps_checked_source_order_after_concurrent_progress() {
        let entry = |label, sequence| {
            let mut entry = session_entry(label, serde_json::json!({"sequence":sequence}), label);
            entry.source_movement_seq = Some(sequence);
            entry
        };
        let prior = vec![entry("earlier", 1), entry("progress", 10)];
        let snapshot = vec![prior[0].clone(), entry("call", 12), entry("queued.input", 9), entry("error", 14)];
        let source = snapshot.last().unwrap().source_artifact.clone();
        let proof = snapshot.iter().map(|entry| (artifact_ref_key(&entry.source_artifact), entry.source_movement_seq.unwrap())).collect::<BTreeMap<_,_>>();
        let mut recovered = snapshot.clone();
        assert!(reconcile_recovered_session_snapshot(&prior, &mut recovered, &proof, &source).unwrap());
        assert_eq!(recovered.iter().map(|entry| entry.source_movement_seq.unwrap()).collect::<Vec<_>>(), [1,10,12,9,14]);
        for missing in &snapshot[1..] {
            let mut incomplete = proof.clone();
            incomplete.remove(&artifact_ref_key(&missing.source_artifact));
            assert!(reconcile_recovered_session_snapshot(&prior, &mut snapshot.clone(), &incomplete, &source).is_err());
        }
        let conflicting = vec![prior[0].clone(), entry("later.progress", 13)];
        assert!(reconcile_recovered_session_snapshot(&conflicting, &mut snapshot.clone(), &proof, &source).is_err());
    }

    #[test]
    fn session_snapshot_round_trip_replaces_dynamic_bindings_without_losing_history() {
        let dir = tempfile::tempdir().unwrap();
        let blobs_dir = dir.path().join("blobs");
        let movements_path = dir.path().join("movements.jsonl");
        std::fs::create_dir_all(&blobs_dir).unwrap();
        std::fs::write(&movements_path, b"").unwrap();
        let (arrivals, _arrival_rx) = tokio::sync::mpsc::channel(1);
        let ctx = InterfaceLiftContext::unprivileged(
            InterfaceId::new("world_provider_test"),
            dir.path().join("registry/interface/world_provider_test"),
            None,
            dir.path().join("var"),
            movements_path,
            blobs_dir.clone(),
            dir.path().join("ledger-index.sqlite"),
            Default::default(),
            arrivals,
            tokio_util::sync::CancellationToken::new(),
        )
        .with_host_io_limits(InterfaceHostIoLimits {
            max_control_frame_bytes: 1_048_576,
            max_stderr_bytes: 262_144,
            max_manifest_bytes: 1_048_576,
            max_payload_bytes: 1_048_576,
        })
        .unwrap();
        let session_context = ResolvedSessionContext {
            provider_interface: InterfaceId::new("world_provider_test"),
            artifacts: vec![
                session_entry(
                    "interface.artifact.input",
                    serde_json::json!({"text": "first"}),
                    "one",
                ),
                session_entry(
                    "tool.call",
                    serde_json::json!({"tool": "file.write"}),
                    "two",
                ),
                session_entry("tool.result", serde_json::json!({"status": "ok"}), "three"),
                session_entry(
                    "interface.artifact.output",
                    serde_json::json!({"text": "first answer"}),
                    "four",
                ),
                session_entry(
                    "interface.artifact.input",
                    serde_json::json!({"text": "second"}),
                    "five",
                ),
            ],
        };
        let output = InterfaceArtifactFile::write(
            dir.path(),
            "interface.artifact.output",
            br#"{"text":"second answer"}"#,
        )
        .unwrap();
        let snapshot = materialize_session_snapshot(&output, &session_context, None).unwrap();
        let snapshot_payload = snapshot.read_payload_bytes().unwrap();
        let snapshot_value: serde_json::Value = serde_json::from_slice(&snapshot_payload).unwrap();
        assert_eq!(
            snapshot_value[SESSION_CONTEXT_FIELD]["artifacts"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
        let snapshot_ref = pin_artifact(&snapshot, &blobs_dir);
        let reference_ref = ArtifactRef {
            id: ArtifactId::new("context.reference:system"),
            content: ContentHash::of_bytes(b"system"),
        };
        let mut static_context = Vec::new();
        let mut restored = ResolvedSessionContext {
            provider_interface: InterfaceId::new("world_provider_test"),
            artifacts: Vec::new(),
        };
        let mut snapshot_head = None;
        let snapshot_labels = BTreeSet::from(["interface.artifact.output".to_string()]);

        collect_bound_context(
            &ctx,
            &mut static_context,
            &mut restored,
            &mut snapshot_head,
            &reference_ref,
            br#"{"label":"context.reference","text":"system"}"#,
            BoundArtifactProvenance::default(),
            &snapshot_labels,
            &InterfaceId::new("world_provider_test"),
        )
        .unwrap();
        collect_bound_context(
            &ctx,
            &mut static_context,
            &mut restored,
            &mut snapshot_head,
            &snapshot_ref,
            &snapshot_payload,
            BoundArtifactProvenance {
                movement_seq: Some(9),
                provider_arrival_seq: Some(9),
            },
            &snapshot_labels,
            &InterfaceId::new("world_provider_test"),
        )
        .unwrap();

        assert_eq!(static_context, vec![reference_ref.id.clone()]);
        assert_eq!(snapshot_head.as_ref().unwrap().covered_artifact_count, 5);
        assert_eq!(restored.artifacts.len(), 6);
        let recorded = restored.artifacts[0].clone();
        let fingerprint = session_artifacts_fingerprint(&restored.artifacts).unwrap();
        assert!(!append_session_payload(&mut restored, &recorded.label,
            &serde_json::to_vec(&recorded.payload).unwrap(), &recorded.source_artifact,
            recorded.source_movement_seq, "replayed input").unwrap());
        assert_eq!(session_artifacts_fingerprint(&restored.artifacts).unwrap(), fingerprint);
        for (label, payload, sequence) in [
            ("different", recorded.payload.clone(), recorded.source_movement_seq),
            (recorded.label.as_str(), serde_json::json!({"changed": true}), recorded.source_movement_seq),
        ] {
            assert!(append_session_payload(&mut restored, label, &serde_json::to_vec(&payload).unwrap(),
                &recorded.source_artifact, sequence, "conflicting input").is_err());
        }
        let mut foreign = ResolvedSessionContext {
            provider_interface: InterfaceId::new("world_provider_other"),
            artifacts: Vec::new(),
        };
        collect_bound_context(
            &ctx,
            &mut Vec::new(),
            &mut foreign,
            &mut None,
            &snapshot_ref,
            &snapshot_payload,
            BoundArtifactProvenance::default(),
            &snapshot_labels,
            &InterfaceId::new("world_provider_other"),
        )
        .unwrap();
        assert!(foreign.artifacts.is_empty());
        assert_eq!(
            restored
                .artifacts
                .iter()
                .map(|artifact| artifact.label.as_str())
                .collect::<Vec<_>>(),
            vec![
                "interface.artifact.input",
                "tool.call",
                "tool.result",
                "interface.artifact.output",
                "interface.artifact.input",
                "interface.artifact.output"
            ]
        );

        let mut changed_prefix = restored.clone();
        changed_prefix.artifacts[0].source_movement_seq = Some(99);
        let rejected_output = InterfaceArtifactFile::write(
            dir.path().join("rejected"),
            "interface.artifact.output",
            br#"{"text":"must fail"}"#,
        )
        .unwrap();
        assert!(materialize_session_snapshot(
            &rejected_output,
            &changed_prefix,
            snapshot_head.as_ref(),
        )
        .unwrap_err()
        .contains("pinned snapshot prefix"));

        let mut normalized = restored.clone();
        let mut normalized_head = snapshot_head.clone();
        for (index, artifact) in normalized.artifacts[..5].iter_mut().enumerate() {
            artifact.source_movement_seq = Some(index as u64 + 1);
        }
        reconcile_resolved_session_snapshot_head(&normalized, &mut normalized_head).unwrap();
        assert!(normalized_head.is_none());
        let rebased_output = InterfaceArtifactFile::write(
            dir.path().join("rebased"),
            "interface.artifact.output",
            br#"{"text":"rebased answer"}"#,
        )
        .unwrap();
        let rebased_snapshot =
            materialize_session_snapshot(&rebased_output, &normalized, normalized_head.as_ref())
                .unwrap();
        let rebased_payload = rebased_snapshot.read_payload_bytes().unwrap();
        let rebased_value: serde_json::Value = serde_json::from_slice(&rebased_payload).unwrap();
        assert!(rebased_value[SESSION_CONTEXT_FIELD]["parent"].is_null());
        assert_eq!(
            rebased_value[SESSION_CONTEXT_FIELD]["artifacts"]
                .as_array()
                .unwrap()
                .len(),
            normalized.artifacts.len()
        );

        restored.artifacts.push(session_entry(
            "interface.artifact.input",
            serde_json::json!({"text": "third"}),
            "six",
        ));
        let next_output = InterfaceArtifactFile::write(
            dir.path().join("next"),
            "interface.artifact.output",
            br#"{"text":"third answer"}"#,
        )
        .unwrap();
        let next_snapshot =
            materialize_session_snapshot(&next_output, &restored, snapshot_head.as_ref()).unwrap();
        let next_payload = next_snapshot.read_payload_bytes().unwrap();
        let next_value: serde_json::Value = serde_json::from_slice(&next_payload).unwrap();
        assert_eq!(
            serde_json::from_value::<ArtifactRef>(
                next_value[SESSION_CONTEXT_FIELD]["parent"].clone(),
            )
            .unwrap(),
            snapshot_ref
        );
        assert_eq!(
            next_value[SESSION_CONTEXT_FIELD]["artifacts"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let next_ref = artifact_identity(&next_snapshot, dir.path())
            .unwrap()
            .artifact;
        let mut chained = ResolvedSessionContext {
            provider_interface: InterfaceId::new("world_provider_test"),
            artifacts: Vec::new(),
        };
        let mut chained_head = None;
        collect_bound_context(
            &ctx,
            &mut Vec::new(),
            &mut chained,
            &mut chained_head,
            &next_ref,
            &next_payload,
            BoundArtifactProvenance {
                movement_seq: Some(10),
                provider_arrival_seq: Some(10),
            },
            &snapshot_labels,
            &InterfaceId::new("world_provider_test"),
        )
        .unwrap();
        assert_eq!(chained.artifacts.len(), 8);
        assert_eq!(chained_head.unwrap().covered_artifact_count, 7);
        let active_bindings = [reference_ref.id, snapshot_ref.id.clone()];
        assert_eq!(active_bindings.len(), 2);
        let latest = &restored.artifacts[5];
        assert_eq!(latest.source_artifact, snapshot_ref);
        assert_eq!(latest.source_movement_seq, Some(9));
    }

    #[test]
    fn admitted_context_output_is_grounded_in_its_ledger_movement() {
        let entry = session_entry(
            "interface.artifact.output",
            serde_json::json!({"text": "ground me"}),
            "grounded-output",
        );
        let source = entry.source_artifact.clone();
        let mut context = ResolvedSessionContext {
            provider_interface: InterfaceId::new("world_provider_test"),
            artifacts: vec![entry],
        };
        let pending = PendingHandoff {
            source_movement_seq: 41,
            context_publication: None,
            source_without_context: false,
            settlement_of: None,
            dispatch_fenced: true,
            withdrawal: None,
            items: vec![PendingHandoffItem {
                artifact_id: source.id.clone(),
                artifact_content_hash: source.content.to_hex(),
                artifact_label: "interface.artifact.output".to_string(),
                manifest_relative_path: PathBuf::from("unused-in-grounding-test"),
                checkpoint: PendingHandoffCheckpoint {
                    id: CheckpointId::new("output"),
                    epoch: 0,
                    interface_binding: all_binding("world_provider_test"),
                    outbound_interface_bindings: vec![all_binding("world_client_test")],
                    artifact_bindings: Vec::new(),
                    required_verified_artifacts: Vec::new(),
                },
            }],
            terminal: false,
        };

        ProviderOutputHandoff::ground_admitted_context_outputs(
            &mut context,
            &pending,
            &[ProviderHandoffOutcome::Admitted {
                seq: 42,
                artifact: source.clone(),
            }],
            &BTreeSet::from(["interface.artifact.output".to_string()]),
        )
        .unwrap();

        assert_eq!(context.artifacts[0].source_movement_seq, Some(42));
    }

    #[test]
    fn session_context_requires_ledger_and_blob_proof_for_each_embedded_payload() {
        use octopus_ledger::{Lamport, Seq, TraceId};

        let dir = tempfile::tempdir().unwrap();
        let source_root = dir.path().join("source");
        let blobs_dir = dir.path().join("blobs");
        std::fs::create_dir_all(&blobs_dir).unwrap();
        let source = InterfaceArtifactFile::write(
            &source_root,
            "interface.artifact.output",
            br#"{"text":"proved"}"#,
        )
        .unwrap();
        let identity = artifact_identity(&source, dir.path()).unwrap().artifact;
        let manifest_bytes = std::fs::read(source.manifest_path()).unwrap();
        let manifest: InterfaceArtifactManifest = serde_json::from_slice(&manifest_bytes).unwrap();
        let payload_bytes = source.read_payload_bytes().unwrap();
        std::fs::write(blobs_dir.join(identity.content.to_hex()), &manifest_bytes).unwrap();
        std::fs::write(
            blobs_dir.join(manifest.payload_content_hash.unwrap()),
            &payload_bytes,
        )
        .unwrap();
        let movements_path = dir.path().join("movements.jsonl");
        let movement = Movement {
            seq: Seq(0),
            lamport: Lamport(8),
            recorded_at_unix_ms: 1,
            trace: TraceId("trace-session-proof".to_string()),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(InterfaceId::new("world_provider_test")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_test")),
            artifact: Some(identity.clone()),
            invocation_metadata: Some(
                typed_invocation_metadata(
                    &RunId::new("run_test"),
                    "output",
                    &InterfaceId::new("world_provider_test"),
                    0,
                )
                .into(),
            ),
        };
        std::fs::write(
            &movements_path,
            format!("{}\n", serde_json::to_string(&movement).unwrap()),
        )
        .unwrap();
        let mut context = ResolvedSessionContext {
            provider_interface: InterfaceId::new("world_provider_test"),
            artifacts: vec![SessionContextArtifact {
                label: "interface.artifact.output".to_string(),
                payload: serde_json::json!({"text": "proved"}),
                source_artifact: identity,
                source_movement_seq: None,
            }],
        };
        let limits = InterfaceHostIoLimits {
            max_control_frame_bytes: 1_048_576,
            max_stderr_bytes: 262_144,
            max_manifest_bytes: 1_048_576,
            max_payload_bytes: 1_048_576,
        };
        let session = ProviderHandoffSession {
            interface: InterfaceId::new("world_provider_test"),
            movements_path: movements_path.canonicalize().unwrap(),
            writer_history: Arc::new(Mutex::new(BTreeMap::new())),
            writer_call_generations: Arc::new(Mutex::new(writer_run::WriterCallIndex::default())),
            evidence: Arc::new(Mutex::new(ProviderLedgerEvidenceIndex::new(
                InterfaceId::new("world_provider_test"),
                movements_path.canonicalize().unwrap(),
                limits.max_control_frame_bytes,
            ))),
        };
        validate_session_context_evidence(
            &mut context,
            SessionContextEvidenceScope {
                blobs_dir: &blobs_dir,
                limits,
                run: &RunId::new("run_test"),
                provider: &InterfaceId::new("world_provider_test"),
                context_output_labels: &BTreeSet::from(["interface.artifact.output".to_string()]),
                session_snapshot_labels: &BTreeSet::new(),
            },
            &session,
        )
        .unwrap();
        assert_eq!(context.artifacts[0].source_movement_seq, Some(0));
        for corruption in ["payload", "source", "sequence"] {
            let mut invalid = context.clone();
            match corruption {
                "payload" => invalid.artifacts[0].payload = serde_json::json!({"text": "forged"}),
                "source" => invalid.artifacts[0].source_artifact.id = ArtifactId::new("forged"),
                "sequence" => invalid.artifacts[0].source_movement_seq = Some(1),
                _ => unreachable!(),
            }
            let error = validate_session_context_evidence(
                &mut invalid,
                SessionContextEvidenceScope {
                    blobs_dir: &blobs_dir,
                    limits,
                    run: &RunId::new("run_test"),
                    provider: &InterfaceId::new("world_provider_test"),
                    context_output_labels: &BTreeSet::from(["interface.artifact.output".to_string()]),
                    session_snapshot_labels: &BTreeSet::new(),
                },
                &session,
            ).unwrap_err();
            assert!(error.contains(match corruption {
                "payload" => "payload differs from its pinned source artifact",
                "source" => "source id is not the pinned manifest identity",
                "sequence" => "does not prove its artifact",
                _ => unreachable!(),
            }), "{corruption}: {error}");
        }

        let input_file = InterfaceArtifactFile::write(&source_root, "tool.call", br#"{"tool":"new.tool","arguments":{}}"#).unwrap();
        let input_ref = pin_artifact(&input_file, &blobs_dir);
        let input_context = ResolvedSessionContext {
            provider_interface: context.provider_interface.clone(),
            artifacts: vec![SessionContextArtifact {
                label: "tool.call".into(), payload: serde_json::json!({"tool":"new.tool","arguments":{}}),
                source_artifact: input_ref.clone(), source_movement_seq: Some(0),
            }],
        };
        let mut incoming = movement.clone();
        incoming.kind = MovementKind::InterfaceOutbound;
        incoming.from = Endpoint::Interface(InterfaceId::new("system_worker_test"));
        incoming.to = Endpoint::Interface(context.provider_interface.clone());
        incoming.artifact = Some(input_ref.clone());
        let mut metadata = typed_invocation_metadata(&RunId::new("run_test"), "input", &context.provider_interface, 0);
        metadata.checkpoint_admission = Some(serde_json::from_value(serde_json::json!({"artifact_label":"tool.call","complete":true,"required_artifact_labels":[]})).unwrap());
        metadata.checkpoint_binding_policy = Some(CheckpointBindingPolicySnapshot {
            inbound: all_binding("system_worker_test"), outbound: vec![all_binding("world_provider_test")],
        });
        metadata.outbound_interface_bindings = vec![context.provider_interface.clone()];
        incoming.invocation_metadata = Some(metadata.into());
        let from_rows = |rows: &[Movement]| {
            std::fs::write(&movements_path, rows.iter().map(|row| format!("{}\n", serde_json::to_string(row).unwrap())).collect::<String>()).unwrap();
            ProviderHandoffSession {
                evidence: Arc::new(Mutex::new(ProviderLedgerEvidenceIndex::new(context.provider_interface.clone(), movements_path.clone(), limits.max_control_frame_bytes))),
                ..session.clone()
            }
        };
        let check = |context: &mut ResolvedSessionContext, session: &ProviderHandoffSession| validate_session_context_evidence(
            context, SessionContextEvidenceScope {
                blobs_dir: &blobs_dir, limits, run: &RunId::new("run_test"), provider: &InterfaceId::new("world_provider_test"),
                context_output_labels: &BTreeSet::from(["tool.call".into(), "interface.artifact.output".into()]), session_snapshot_labels: &BTreeSet::new(),
            }, session,
        );
        for case in ["valid", "unadmitted", "incomplete", "binding", "recipient", "run", "label", "verification", "sequence", "unpinned", "output_without_arrival", "outbound_binding"] {
            let mut row = serde_json::to_value(&incoming).unwrap();
            let mut candidate = input_context.clone();
            match case {
                "unadmitted" => row["invocation_metadata"]["checkpoint_admission"] = serde_json::Value::Null,
                "incomplete" => row["invocation_metadata"]["checkpoint_admission"]["complete"] = serde_json::json!(false),
                "binding" => row["invocation_metadata"]["checkpoint_binding_policy"]["inbound"]["interface"] = serde_json::json!("other_source"),
                "recipient" => row["to"] = serde_json::json!({"Interface":"other_provider"}),
                "run" => row["invocation_metadata"]["runtime"]["run"] = serde_json::json!("other_run"),
                "label" => row["invocation_metadata"]["checkpoint_admission"]["artifact_label"] = serde_json::json!("other.label"),
                "verification" => row["invocation_metadata"]["verification_failure"] = serde_json::json!({"error":"verification_failed","verifier":"check","details":{}}),
                "sequence" => candidate.artifacts[0].source_movement_seq = Some(1),
                "unpinned" => candidate.artifacts[0].source_movement_seq = None,
                "output_without_arrival" => {
                    row["from"] = serde_json::json!({"Interface":"world_provider_test"});
                    row["invocation_metadata"]["checkpoint_binding_policy"]["inbound"]["interface"] = serde_json::json!("world_provider_test");
                }
                "outbound_binding" => row["invocation_metadata"]["checkpoint_binding_policy"]["outbound"] = serde_json::json!([]),
                _ => {}
            }
            let session = from_rows(&[serde_json::from_value(row).unwrap()]);
            let result = check(&mut candidate, &session);
            assert_eq!(result.is_ok(), case == "valid", "{case}: {result:?}");
        }
        let later = one_shot_movement(1, &input_ref, "output", MovementKind::InterfaceArrival);
        let session = from_rows(&[incoming, later]);
        for sequence in [0, 2] {
            let mut candidate = input_context.clone();
            candidate.artifacts[0].source_movement_seq = Some(sequence);
            ground_session_context_movements(&mut candidate, &session, &RunId::new("run_test"), &config_with_tool_routes(&[])).unwrap();
            assert_eq!(candidate.artifacts[0].source_movement_seq, Some(sequence));
            assert_eq!(check(&mut candidate, &session).is_ok(), sequence == 0);
        }
    }

    fn config_with_tool_routes(routes: &[(&str, &[&str])]) -> RunAuthoringConfig {
        let mut checkpoints = vec![
            serde_json::json!({
                "name": "terminal_input",
                "role": "stable",
                "interface_binding": all_binding("world_input_test"),
                "outbound_interface_bindings": [all_binding("world_provider_test")],
                "include_context": true,
                "required_verified_artifacts": []
            }),
            serde_json::json!({
                "name": "client_output",
                "role": "provider_artifact_output",
                "interface_binding": all_binding("world_provider_test"),
                "outbound_interface_bindings": [all_binding("world_client_test")],
                "include_context": false,
                "required_verified_artifacts": []
            }),
            serde_json::json!({
                "name": "authoring_call",
                "role": "authoring_call",
                "interface_binding": all_binding("system_interface_authoring_test"),
                "outbound_interface_bindings": [all_binding("system_context_authoring_test")],
                "include_context": false,
                "required_verified_artifacts": []
            }),
            serde_json::json!({
                "name": "authoring_result",
                "role": "authoring_result",
                "interface_binding": all_binding("system_context_authoring_test"),
                "outbound_interface_bindings": [all_binding("world_provider_test")],
                "include_context": false,
                "required_verified_artifacts": []
            }),
        ];
        for (name, tools) in routes {
            checkpoints.push(serde_json::json!({
                "name": name,
                "role": "provider_tool_output",
                "interface_binding": all_binding("world_provider_test"),
                "outbound_interface_bindings": [all_binding(format!("system_{name}"))],
                "include_context": false,
                "required_verified_artifacts": [{
                    "mark": "tool.call.allowed",
                    "artifact_label": "tool.call",
                    "verifier": "tool_call_allowed",
                    "params": { "tools": tools }
                }]
            }));
        }
        serde_json::from_value(serde_json::json!({
            "run": "run_test",
            "worker": "system_worker_test",
            "provider_interface": "world_provider_test",
            "authoring_source_interface": "system_interface_authoring_test",
            "file_write_tool": "file.write",
            "file_write_label": "tool.call",
            "file_write_result_label": "file.write.result",
            "checkpoints": checkpoints,
            "result_timeout_ms": 1000
        }))
        .unwrap()
    }

    fn test_bound_topology(config: &RunAuthoringConfig) -> BoundRunAuthoringTopology {
        BoundRunAuthoringTopology {
            schema_version: 2,
            run: config.run.clone(),
            worker: config.worker.clone(),
            provider_interface: config.provider_interface.clone(),
            authoring_source_interface: config.authoring_source_interface.clone(),
            payload_artifact_bindings: config.payload_artifact_bindings.clone(),
            writer_run: config.writer_run.clone(),
            checkpoints: config.checkpoints.clone(),
        }
    }

    #[test]
    fn recorded_output_is_explicit_and_disjoint_from_return_bindings() {
        let mut config = config_with_tool_routes(&[]);
        let output = config.checkpoints.iter_mut().find(|cp| cp.role == RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap();
        output.interface_binding = names_binding("world_provider_test", &["interface.artifact.output", "turn.error"]);
        let mut recorded = output.clone();
        recorded.name = "recorded_output".into();
        recorded.interface_binding = names_binding("world_provider_test", &["turn.stream"]);
        recorded.outbound_interface_bindings.clear();
        config.checkpoints.push(recorded);
        config.validate(&config.provider_interface, &config.run).unwrap();
        let selected = config.output_checkpoint(&ProviderHandoffRoute::ClientProjection, &BTreeSet::new(), &BTreeSet::new(), "turn.stream").unwrap();
        assert!(selected.outbound_interface_bindings.is_empty());
        let error = config.output_checkpoint(&ProviderHandoffRoute::ClientProjection, &BTreeSet::new(), &BTreeSet::new(), "turn.error").unwrap();
        assert_eq!(error.outbound_interface_bindings.len(), 1);
        config.checkpoints.last_mut().unwrap().interface_binding = AuthoredInterfaceBinding::all(config.provider_interface.clone());
        assert!(config.validate(&config.provider_interface, &config.run).unwrap_err().contains("overlapping"));
    }

    #[test]
    fn private_run_authoring_recovery_owns_only_its_exact_correlation_namespace() {
        let prefix = "provider-run-authoring:run_test:";
        let foreign = br#"{"tool":"file.write","arguments":{"writes":[]},"correlation_id":"provider-other:run_test:batch"}"#;
        assert!(
            decode_owned_run_authoring_call(foreign, prefix)
                .unwrap()
                .is_none()
        );
        let owned = br#"{"tool":"file.write","arguments":{"path":"/tmp/run.json","patch":[],"expected_before_hash":"before","expected_after_hash":"after"},"correlation_id":"provider-run-authoring:run_test:1"}"#;
        assert!(
            decode_owned_run_authoring_call(owned, prefix)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn delivery_mode_rejects_partial_authority_and_accepts_strict_one_shot_slots() {
        use octopus_descriptors::EeId;
        use octopus_interfaces::{GrantProjection, RuntimeInvocationMetadata};

        let mut config = config_with_tool_routes(&[]);
        config
            .checkpoints
            .iter_mut()
            .find(|checkpoint| {
                checkpoint.role == RunAuthoringCheckpointRole::ProviderArtifactOutput
            })
            .unwrap()
            .required_verified_artifacts = serde_json::from_value(serde_json::json!([{
            "mark": "provider.output.admitted",
            "artifact_label": "interface.artifact.output",
            "verifier": "artifact_exists",
            "params": {}
        }]))
        .unwrap();
        config.checkpoints.push(
            serde_json::from_value(serde_json::json!({
                "name": "dispatch_attempt_1",
                "role": "provider_transport_dispatch",
                "interface_binding": all_binding("world_provider_test"),
                "outbound_interface_bindings": [all_binding("world_dispatch_sink_test")],
                "include_context": false,
                "required_verified_artifacts": [{
                    "mark": "provider.dispatch.admitted",
                    "artifact_label": "provider.transport.dispatch",
                    "verifier": "artifact_exists",
                    "params": {"attempt": 1}
                }]
            }))
            .unwrap(),
        );
        config.checkpoints.push(
            serde_json::from_value(serde_json::json!({
                "name": "handoff_completion",
                "role": "provider_handoff_completion",
                "interface_binding": all_binding("world_provider_test"),
                "outbound_interface_bindings": [],
                "include_context": false,
                "required_verified_artifacts": [{
                    "mark": "provider.handoff.completed",
                    "artifact_label": "provider.handoff.complete",
                    "verifier": "artifact_exists",
                    "params": {}
                }]
            }))
            .unwrap(),
        );
        config
            .validate(
                &InterfaceId::new("world_provider_test"),
                &RunId::new("run_test"),
            )
            .unwrap();
        let checkpoints = config
            .checkpoints
            .iter()
            .map(|checkpoint| {
                materialize_checkpoint(checkpoint, &checkpoint.name, &RunId::new("run_test"), &[])
            })
            .collect::<Vec<_>>();
        let active = config
            .checkpoints
            .iter()
            .map(|checkpoint| checkpoint.name.clone())
            .collect::<Vec<_>>();
        let record = RunRecord::from_bytes(
            &serde_json::to_vec(&serde_json::json!({
                "id": "run_test",
                "ee": "ee_test",
                "worker": "system_worker_test",
                "trace": "trace:run_test",
                "status": "running",
                "interface_state": {},
                "checkpoints": checkpoints,
                "active_checkpoints": active
            }))
            .unwrap(),
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let artifact_file = InterfaceArtifactFile::write(
            dir.path(),
            "interface.artifact.input",
            br#"{"text":"one shot"}"#,
        )
        .unwrap();
        let artifact = artifact_identity(&artifact_file, dir.path())
            .unwrap()
            .artifact;
        let runtime = RuntimeInvocationMetadata {
            run: RunId::new("run_test"),
            ee: EeId::new("ee_test"),
            worker: InterfaceId::new("system_worker_test"),
            checkpoint: CheckpointId::new("terminal_input"),
            interface: InterfaceId::new("world_provider_test"),
            epoch: 0,
            run_record_hash: None,
        };
        let mut input = InterfaceInvocationEnvelope {
            artifact,
            artifact_file,
            artifact_binding_payloads: Vec::new(),
            metadata: Some(
                InvocationMetadata::new(GrantProjection::default()).with_runtime(runtime),
            ),
        };
        let mode = derive_handoff_delivery_mode(
            &input,
            &config,
            &record,
            &CheckpointId::new("terminal_input"),
            &BTreeSet::new(),
        )
        .unwrap();
        let HandoffDeliveryMode::OneShot(plan) = mode else {
            panic!("expected one-shot delivery mode")
        };
        assert!(plan.terminal.contains_key("interface.artifact.output"));
        assert!(plan.dispatch.contains_key(&1));
        assert_eq!(
            plan.completion.checkpoint,
            CheckpointId::new("handoff_completion")
        );

        input.metadata.as_mut().unwrap().grant.tools = vec!["coder.exec".to_string()];
        let error = derive_handoff_delivery_mode(
            &input,
            &config,
            &record,
            &CheckpointId::new("terminal_input"),
            &BTreeSet::from(["coder.exec".to_string()]),
        )
        .unwrap_err();
        assert!(error.contains("tool continuation without run re-arm authority"));

        let grant = &mut input.metadata.as_mut().unwrap().grant;
        grant.tools = vec!["file.write".to_string()];
        grant.interfaces = vec![
            "world_provider_test".to_string(),
            "system_interface_authoring_test".to_string(),
            "system_context_authoring_test".to_string(),
        ];
        let unrestricted = derive_handoff_delivery_mode(
            &input,
            &config,
            &record,
            &CheckpointId::new("terminal_input"),
            &BTreeSet::from(["file.write".to_string()]),
        ).unwrap();
        assert_eq!(unrestricted, HandoffDeliveryMode::Rearm);
        input.metadata.as_mut().unwrap().grant.ee_root_slot_allowance = Some(Vec::new());
        let error = derive_handoff_delivery_mode(
            &input, &config, &record, &CheckpointId::new("terminal_input"),
            &BTreeSet::from(["file.write".to_string()]),
        ).unwrap_err();
        assert!(error.contains("does not admit run_records_root"));
        input
            .metadata
            .as_mut()
            .unwrap()
            .grant
            .ee_root_slot_allowance = Some(vec!["run_records_root".to_string()]);
        assert_eq!(
            derive_handoff_delivery_mode(
                &input,
                &config,
                &record,
                &CheckpointId::new("terminal_input"),
                &BTreeSet::from(["file.write".to_string()]),
            )
            .unwrap(),
            HandoffDeliveryMode::Rearm
        );
        let dispatch = config
            .output_checkpoint(
                &ProviderHandoffRoute::TransportDispatch { attempt: 2 },
                &BTreeSet::from(["file.write".to_string()]),
                &BTreeSet::new(),
                PROVIDER_TRANSPORT_DISPATCH_LABEL,
            )
            .unwrap();
        assert_eq!(
            dispatch.role,
            RunAuthoringCheckpointRole::ProviderTransportDispatch
        );
        assert!(dispatch.outbound_interface_bindings.is_empty());
        assert!(dispatch.required_verified_artifacts.is_empty());
        assert!(
            pending_output_checkpoint(
                &dispatch,
                "transport_dispatch_2",
                PROVIDER_TRANSPORT_DISPATCH_LABEL,
                &[ArtifactId::new("interface.context.updated:dynamic")],
                &[ArtifactId::new("context.reference:role")],
            )
            .artifact_bindings
                == vec![ArtifactId::new("context.reference:role")]
        );
    }

    fn provider_descriptor(folder: &str) -> InterfaceDescriptor {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join(folder)
            .join("interface.json");
        let bytes = std::fs::read(path).unwrap();
        octopus_descriptors::parse_strict(&bytes).unwrap()
    }

    fn local_profile_descriptor() -> InterfaceDescriptor {
        provider_descriptor("world_provider_ollama")
    }

    fn profile_run_value(provider: &InterfaceId, run: &RunId) -> serde_json::Value {
        let mut value = serde_json::json!({
            "id": run,
            "ee": "ee_profile_test",
            "worker": "system_worker_profile_test",
            "trace": format!("trace:{}", run.as_str()),
            "status": "running",
            "interface_state": {
                provider.as_str(): {
                    "context_generation": 0,
                    "authoring_generation": 0,
                    "frontier_checkpoint_ids": [
                        "client_output",
                        "tool_output",
                        "authoring_call",
                        "authoring_result"
                    ]
                }
            },
            "checkpoints": [
                {
                    "id": "stable_input",
                    "run": run,
                    "trace": format!("trace:{}:stable_input", run.as_str()),
                    "epoch": 4,
                    "interface_binding": all_binding("world_input_profile_test"),
                    "outbound_interface_bindings": [all_binding(provider.as_str())],
                    "artifact_bindings": [{"id": "artifact-context"}],
                    "required_verified_artifacts": []
                },
                {
                    "id": "client_output",
                    "run": run,
                    "trace": format!("trace:{}:client_output", run.as_str()),
                    "epoch": 0,
                    "interface_binding": all_binding(provider.as_str()),
                    "outbound_interface_bindings": [all_binding("world_client_profile_test")],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                },
                {
                    "id": "tool_output",
                    "run": run,
                    "trace": format!("trace:{}:tool_output", run.as_str()),
                    "epoch": 0,
                    "interface_binding": all_binding(provider.as_str()),
                    "outbound_interface_bindings": [all_binding("system_tool_profile_test")],
                    "artifact_bindings": [],
                    "required_verified_artifacts": [{
                        "mark": "tool.call.allowed",
                        "artifact_label": "tool.call",
                        "verifier": "tool_call_allowed",
                        "params": {"tools": ["coder.exec"]}
                    }]
                },
                {
                    "id": "authoring_call",
                    "run": run,
                    "trace": format!("trace:{}:authoring_call", run.as_str()),
                    "epoch": 0,
                    "interface_binding": all_binding("system_interface_authoring_profile_test"),
                    "outbound_interface_bindings": [all_binding("system_context_authoring_profile_test")],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                },
                {
                    "id": "authoring_result",
                    "run": run,
                    "trace": format!("trace:{}:authoring_result", run.as_str()),
                    "epoch": 0,
                    "interface_binding": all_binding("system_context_authoring_profile_test"),
                    "outbound_interface_bindings": [all_binding(provider.as_str())],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                }
            ],
            "active_checkpoints": [
                "stable_input",
                "client_output",
                "tool_output",
                "authoring_call",
                "authoring_result"
            ]
        });
        let record = RunRecord::from_bytes(&serde_json::to_vec(&value).unwrap()).unwrap();
        let topology = derive_bound_run_authoring_topology(
            &record,
            provider,
            &CheckpointId::new("stable_input"),
        )
        .unwrap();
        value["interface_state"][provider.as_str()][RUN_AUTHORING_BINDING] =
            serde_json::to_value(topology).unwrap();
        value
    }

    fn pending_movement(seq: u64, item: &PendingHandoffItem, artifact: &ArtifactRef) -> Movement {
        Movement {
            seq: octopus_ledger::Seq(seq),
            lamport: octopus_ledger::Lamport(seq + 1),
            recorded_at_unix_ms: seq + 1,
            trace: octopus_ledger::TraceId(format!("trace-pending-{seq}")),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(InterfaceId::new("world_provider_test")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_test")),
            artifact: Some(artifact.clone()),
            invocation_metadata: Some(
                typed_invocation_metadata(
                    &RunId::new("run_test"),
                    item.checkpoint.id.as_str(),
                    &InterfaceId::new("world_provider_test"),
                    item.checkpoint.epoch,
                )
                .into(),
            ),
        }
    }

    #[test]
    fn run_authoring_result_identity_requires_the_private_correlation_shape() {
        let descriptor = local_profile_descriptor();
        assert_eq!(
            RunAuthoringResultIdentity::from_descriptor(&descriptor)
                .unwrap()
                .artifact_label(),
            "file.write.result"
        );
        assert_eq!(
            correlation_generation(&RunId::new("run_test"), "provider-run-authoring:run_test:7")
                .unwrap(),
            7
        );
        assert!(correlation_generation(&RunId::new("run_test"), "call_model_123").is_err());
        assert!(
            correlation_generation(
                &RunId::new("run_test"),
                "provider-run-authoring:run_test:07"
            )
            .is_err()
        );
    }

    #[test]
    fn run_authoring_profile_requires_its_file_tool_to_be_hidden_from_the_model() {
        let mut descriptor = local_profile_descriptor();
        descriptor
            .interface_driver_config
            .as_mut()
            .unwrap()
            .private_config
            .insert(
                "model_hidden_tools".to_string(),
                serde_json::json!(["coder.exec"]),
            );
        let error = run_authoring_profile(&descriptor).unwrap_err();
        assert!(error.contains("exposes run-authoring tool 'file.write' to the model"));
    }

    #[test]
    fn context_free_provider_outputs_keep_only_mandatory_role_bindings() {
        let config = config_with_tool_routes(&[("coder", &["coder.exec"])]);
        let mut seeded = config.clone();
        let stable = seeded.checkpoints.iter().position(|checkpoint| checkpoint.role == RunAuthoringCheckpointRole::Stable).unwrap();
        seeded.checkpoints[stable].static_artifact_bindings = vec![ArtifactId::new("declared-seed")];
        seeded.validate(&seeded.provider_interface, &seeded.run).unwrap();
        seeded.checkpoints[stable].static_artifact_bindings.push(ArtifactId::new("declared-seed"));
        assert!(seeded.validate(&seeded.provider_interface, &seeded.run).is_err());
        for role in [RunAuthoringCheckpointRole::ProviderArtifactOutput, RunAuthoringCheckpointRole::AuthoringCall, RunAuthoringCheckpointRole::AuthoringResult] {
            let mut invalid = config.clone();
            invalid.checkpoints.iter_mut().find(|checkpoint| checkpoint.role == role).unwrap().static_artifact_bindings = vec![ArtifactId::new("forbidden-seed")];
            assert!(invalid.validate(&invalid.provider_interface, &invalid.run).is_err());
        }
        let tool = config
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.role == RunAuthoringCheckpointRole::ProviderToolOutput)
            .unwrap();
        let full = vec![
            ArtifactId::new("context.reference:role"),
            ArtifactId::new("interface.context.updated:dynamic"),
        ];
        let mandatory_role = vec![ArtifactId::new("context.reference:role")];
        let pending =
            pending_output_checkpoint(tool, "tool_output", "tool.call", &full, &mandatory_role);
        assert_eq!(pending.artifact_bindings, mandatory_role);
        let mut declared = config.clone();
        let output = declared.checkpoints.iter_mut().find(|checkpoint| checkpoint.role == RunAuthoringCheckpointRole::ProviderToolOutput).unwrap();
        output.include_context = true;
        output.static_artifact_bindings = vec![ArtifactId::new("explicit-object")];
        declared.validate(&declared.provider_interface, &declared.run).unwrap();
        let output = declared.checkpoints.iter().find(|checkpoint| checkpoint.role == RunAuthoringCheckpointRole::ProviderToolOutput).unwrap();
        let bound = pending_output_checkpoint(output, "bound_output", "tool.call", &full, &mandatory_role);
        assert_eq!(bound.artifact_bindings, [full.clone(), vec![ArtifactId::new("explicit-object")]].concat());
        let roles = vec![ArtifactBinding { id: mandatory_role[0].clone(), access: Default::default() }];
        let mut projected: octopus_runtime::run_record::CheckpointRecord = serde_json::from_value(
            materialize_pending_checkpoint(&bound, &config.run),
        ).unwrap();
        assert!(checkpoint_role_bindings_match(&projected, &roles, output));
        projected.artifact_bindings.swap(1, 2);
        assert!(checkpoint_role_bindings_match(&projected, &roles, output));
        projected.artifact_bindings[1].access.read = false;
        assert!(!checkpoint_role_bindings_match(&projected, &roles, output));
        projected.artifact_bindings.remove(1);
        assert!(!checkpoint_role_bindings_match(&projected, &roles, output));
        let mut overlap = output.clone();
        overlap.static_artifact_bindings.push(mandatory_role[0].clone());
        let mut roles = roles;
        roles[0].access.change = true;
        let mut projected: octopus_runtime::run_record::CheckpointRecord = serde_json::from_value(
            materialize_pending_checkpoint(&bound, &config.run),
        ).unwrap();
        projected.artifact_bindings[0] = roles[0].clone();
        assert!(checkpoint_role_bindings_match(&projected, &roles, &overlap));
        projected.artifact_bindings[2].access.change = true;
        assert!(!checkpoint_role_bindings_match(&projected, &roles, &overlap));
        projected.artifact_bindings[2].access.change = false;
        projected.artifact_bindings[0].access.change = false;
        assert!(!checkpoint_role_bindings_match(&projected, &roles, &overlap));
        projected.artifact_bindings[0] = roles[0].clone();
        projected.artifact_bindings.push(roles[0].clone());
        assert!(!checkpoint_role_bindings_match(&projected, &roles, &overlap));
        projected.artifact_bindings.pop();
        projected.artifact_bindings.swap(0, 1);
        assert!(!checkpoint_role_bindings_match(&projected, &roles, &overlap));
        assert_eq!(
            pending.interface_binding.config.artifact_policy,
            BindingArtifactPolicy::Allow {
                names: vec!["tool.call".to_string()],
                namespaces: Vec::new(),
            }
        );

        let client = config
            .checkpoints
            .iter()
            .find(|checkpoint| {
                checkpoint.role == RunAuthoringCheckpointRole::ProviderArtifactOutput
            })
            .unwrap();
        assert_eq!(
            pending_output_checkpoint(
                client,
                "client_output",
                "interface.artifact.output",
                &full,
                &mandatory_role,
            )
            .artifact_bindings,
            mandatory_role,
        );
    }

    #[test]
    fn rearm_batches_preserve_order_and_isolate_repeated_names_and_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let emitted = dir.path().join("emitted");
        let emissions = [
            ("agent.message.delta", br#"{"text":"one"}"#.as_slice()),
            ("account.rate_limits", br#"{"limit":1}"#.as_slice()),
            ("agent.message.delta", br#"{"text":"two"}"#.as_slice()),
        ]
        .into_iter()
        .map(|(label, payload)| ProviderHandoffEmission {
            artifact: InterfaceArtifactFile::write(&emitted, label, payload).unwrap(),
            route: ProviderHandoffRoute::ClientProjection,
        })
        .collect();
        let batches = split_handoff_batches_by_artifact_name(emissions, &BTreeSet::new()).unwrap();
        assert_eq!(
            batches.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert_eq!(batches.iter().flatten().map(|emission| emission.artifact.read_manifest().unwrap().label).collect::<Vec<_>>(),
            vec!["agent.message.delta", "account.rate_limits", "agent.message.delta"]);
        let emissions = ["tool.call", "turn.completed", "turn.stream", "operator.input.request", "provider.account.updated"]
            .into_iter().map(|label| ProviderHandoffEmission {
                artifact: InterfaceArtifactFile::write(&emitted, label, br#"{}"#).unwrap(),
                route: if label == "operator.input.request" {
                    ProviderHandoffRoute::ClientProjectionWithVerifiedInputs { inputs: Vec::new() }
                } else { ProviderHandoffRoute::ClientProjection },
            }).collect();
        let batches = split_handoff_batches_by_artifact_name(emissions, &BTreeSet::from(["turn.completed".into()])).unwrap();
        assert_eq!(batches.iter().map(Vec::len).collect::<Vec<_>>(), vec![1, 1, 1, 1, 1]);
        let names = batches.iter().flatten().map(|emission| emission.artifact.read_manifest().unwrap().label).collect::<Vec<_>>();
        assert_eq!(names, vec!["tool.call", "turn.completed", "turn.stream", "operator.input.request", "provider.account.updated"]);
    }

    #[test]
    fn verified_operator_inputs_are_exact_granted_one_shot_edges() {
        let config = config_with_tool_routes(&[]);
        let output = config
            .checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput)
            .unwrap();
        let requirement = |mark: &str, verifier: &str| RequiredVerifiedArtifact {
            mark: octopus_descriptors::MarkId::new(mark),
            artifact_label: "operator.approval.response".to_string(),
            verifier: octopus_descriptors::VerifierId::new(verifier),
            params: serde_json::json!({"request_id": "request-7"}),
        };
        let route = ProviderHandoffRoute::ClientProjectionWithVerifiedInputs {
            inputs: vec![
                ProviderVerifiedInput {
                    also_deliver_to_tool: None,
                    artifact_label: "operator.approval.response".to_string(),
                    required_verified_artifact: requirement(
                        "operator.approval",
                        "operator_approval",
                    ),
                },
                ProviderVerifiedInput {
                    also_deliver_to_tool: None,
                    artifact_label: "operator.approval.response".to_string(),
                    required_verified_artifact: requirement(
                        "operator.input.provided",
                        "operator_input",
                    ),
                },
            ],
        };
        let marks = BTreeSet::from([
            "operator.approval".to_string(),
            "operator.input.provided".to_string(),
        ]);
        let verified = config
            .verified_input_checkpoints(&route, output, &marks, &BTreeSet::new(), &BTreeSet::new())
            .unwrap();
        assert_eq!(verified.len(), 2);
        assert!(verified.iter().all(|checkpoint| {
            checkpoint.role == RunAuthoringCheckpointRole::VerifiedInput
                && checkpoint.include_context
                && binding_admits_exact_name(
                    &checkpoint.interface_binding,
                    "operator.approval.response",
                )
                && binding_admits_exact_name(
                    &checkpoint.outbound_interface_bindings[0],
                    "operator.approval.response",
                )
        }));

        let mut next = config.clone();
        next.checkpoints.extend(verified);
        next.validate(&next.provider_interface, &next.run).unwrap();
        let run = next.run.clone();
        let mut root = serde_json::json!({
            "checkpoints": config.checkpoints.iter().map(|checkpoint| {
                materialize_checkpoint(checkpoint, &checkpoint.name, &run, &[])
            }).collect::<Vec<_>>(),
            "active_checkpoints": config.checkpoints.iter().map(|checkpoint| {
                checkpoint.name.clone()
            }).collect::<Vec<_>>()
        })
        .as_object()
        .unwrap()
        .clone();
        replace_verified_input_checkpoints(&mut root, &config, &next, &run, "request", &[])
            .unwrap();
        assert_eq!(
            root["active_checkpoints"].as_array().unwrap().len(),
            config.checkpoints.len() + 2
        );
        replace_verified_input_checkpoints(&mut root, &next, &config, &run, "response", &[])
            .unwrap();
        assert_eq!(
            root["active_checkpoints"].as_array().unwrap().len(),
            config.checkpoints.len()
        );

        let error = config
            .verified_input_checkpoints(
                &route,
                output,
                &BTreeSet::from(["operator.approval".to_string()]),
                &BTreeSet::new(),
                &BTreeSet::new(),
            )
            .unwrap_err();
        assert!(error.contains("outside the formed invocation grant"));
    }

    #[test]
    fn base_provider_profile_lifts_derives_and_persists_exact_run_handoff() {
        use octopus_descriptors::EeId;
        use octopus_interfaces::{GrantProjection, RuntimeInvocationMetadata};
        use octopus_ledger::{Lamport, Seq, TraceId};
        use tokio_util::sync::CancellationToken;

        let descriptor = local_profile_descriptor();
        let result_identity = RunAuthoringResultIdentity::from_descriptor(&descriptor).unwrap();
        assert_eq!(result_identity.artifact_label(), "file.write.result");
        let profile = run_authoring_profile(&descriptor).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let provider = descriptor.id.clone();
        let run = RunId::new("run_profile_test");
        let runs_root = dir.path().join("runs");
        let hand_in_root = dir.path().join("hand-in");
        let control_root = dir.path().join("control");
        let var_dir = dir.path().join("var/provider");
        let blobs_dir = dir.path().join("ledger/blobs");
        let movements_path = dir.path().join("ledger/movements.jsonl");
        std::fs::create_dir_all(&runs_root).unwrap();
        std::fs::create_dir_all(&blobs_dir).unwrap();
        std::fs::create_dir_all(&control_root).unwrap();
        std::fs::write(&movements_path, b"").unwrap();
        let run_value = profile_run_value(&provider, &run);
        let mut run_bytes = serde_json::to_vec_pretty(&run_value).unwrap();
        run_bytes.push(b'\n');
        let run_path = runs_root.join(format!("{}.json", run.as_str()));
        std::fs::write(&run_path, &run_bytes).unwrap();
        let input_file = InterfaceArtifactFile::write(
            var_dir.join("input"),
            "interface.artifact.input",
            br#"{"text":"profile input"}"#,
        )
        .unwrap();
        let input_ref = artifact_identity(&input_file, &var_dir).unwrap().artifact;
        let metadata = InvocationMetadata::new(GrantProjection {
            tools: vec!["coder.exec".to_string(), "file.write".to_string()],
            interfaces: vec![
                provider.as_str().to_string(),
                "system_interface_authoring_profile_test".to_string(),
                "system_context_authoring_profile_test".to_string(),
            ],
            ee_root_slot_allowance: Some(vec!["run_records_root".to_string()]),
            ..GrantProjection::default()
        })
        .with_runtime(RuntimeInvocationMetadata {
            run: run.clone(),
            ee: EeId::new("ee_profile_test"),
            worker: InterfaceId::new("system_worker_profile_test"),
            checkpoint: CheckpointId::new("stable_input"),
            interface: provider.clone(),
            epoch: 4,
            run_record_hash: Some(ContentHash::of_bytes(&run_bytes).to_hex()),
        });
        let input = InterfaceInvocationEnvelope {
            artifact: input_ref,
            artifact_file: input_file,
            artifact_binding_payloads: Vec::new(),
            metadata: Some(metadata),
        };
        let (arrivals, _arrival_rx) = tokio::sync::mpsc::channel(1);
        let ctx = InterfaceLiftContext::unprivileged(
            provider.clone(),
            dir.path()
                .join(format!("registry/interface/{}", provider.as_str())),
            Some(descriptor.clone()),
            var_dir.clone(),
            movements_path,
            blobs_dir,
            dir.path().join("ledger/ledger-index.sqlite"),
            BTreeMap::from([
                ("run_records_root".to_string(), runs_root.clone()),
                ("interface_hand_in_root".to_string(), hand_in_root.clone()),
                ("app_server_control_root".to_string(), control_root),
            ]),
            arrivals,
            CancellationToken::new(),
        )
        .with_host_io_limits(InterfaceHostIoLimits {
            max_control_frame_bytes: 1_048_576,
            max_stderr_bytes: 262_144,
            max_manifest_bytes: 1_048_576,
            max_payload_bytes: 268_435_456,
        })
        .unwrap();
        let mut mismatched_ctx = ctx.clone();
        mismatched_ctx.interface = InterfaceId::new("world_provider_profile_mismatch");
        let error = ProviderHandoffSession::new(&mismatched_ctx)
            .unwrap()
            .from_invocation(&mismatched_ctx, &descriptor, &run, &input, 23, &run_bytes)
            .err()
            .unwrap();
        assert!(error.contains("does not match lifted interface"));
        let session = ProviderHandoffSession::new(&ctx).unwrap();
        {
            let mut plain: serde_json::Value = serde_json::from_slice(&run_bytes).unwrap();
            plain["interface_state"].as_object_mut().unwrap().remove(provider.as_str());
            let plain_bytes = serde_json::to_vec(&plain).unwrap();
            std::fs::write(&run_path, &plain_bytes).unwrap();
            for (name, pinned, expected_error) in [("plain", &plain_bytes, false), ("lost", &run_bytes, true)] {
                let mut probe_ctx = ctx.clone();
                probe_ctx.movements_path = ctx.movements_path.with_file_name(format!("candidate-{name}.jsonl"));
                probe_ctx.ledger_index_path = ctx.ledger_index_path.with_file_name(format!("candidate-{name}.sqlite"));
                let hash = ContentHash::of_bytes(pinned);
                std::fs::write(ctx.blobs_dir.join(hash.to_hex()), pinned).unwrap();
                let mut metadata = input.metadata().unwrap().clone();
                metadata.runtime.as_mut().unwrap().run_record_hash = Some(hash.to_hex());
                let row = Movement { seq: Seq(0), lamport: Lamport(0), recorded_at_unix_ms: 0,
                    trace: TraceId("candidate".into()), kind: MovementKind::InterfaceOutbound,
                    from: Endpoint::Interface(InterfaceId::new("world_input_profile_test")), to: Endpoint::Interface(provider.clone()),
                    artifact: Some(input.artifact.clone()), invocation_metadata: Some(metadata.into()) };
                std::fs::write(&probe_ctx.movements_path, format!("{}\n", serde_json::to_string(&row).unwrap())).unwrap();
                let candidates = ProviderHandoffSession::new(&probe_ctx).unwrap().completed_source_candidates(&probe_ctx);
                if expected_error { assert!(candidates.unwrap_err().contains("lost its declared authoring state")); }
                else { assert!(candidates.unwrap().is_empty()); }
            }
            std::fs::write(&run_path, &run_bytes).unwrap();
        }
        let mut handoff = session
            .from_invocation(&ctx, &descriptor, &run, &input, 23, &run_bytes)
            .unwrap();
        assert_eq!(
            handoff.hand_in_root,
            hand_in_root.join("system_interface_authoring_profile_test")
        );
        let independent_run = exercise_independent_provider_owner(&handoff, &run_bytes);
        {
            let mut probe: serde_json::Value = serde_json::from_slice(&independent_run).unwrap();
            let mut topology: BoundRunAuthoringTopology = serde_json::from_value(
                probe["interface_state"][provider.as_str()][RUN_AUTHORING_BINDING].clone(),
            ).unwrap();
            let mut independent = topology.checkpoints.iter().find(|cp| cp.role == RunAuthoringCheckpointRole::Stable).unwrap().clone();
            independent.name = "independent_probe".into();
            independent.role = RunAuthoringCheckpointRole::IndependentInput;
            independent.include_context = false;
            independent.interface_binding = all_binding("world_peer_probe");
            probe["checkpoints"].as_array_mut().unwrap().push(materialize_checkpoint(&independent, &independent.name, &run, &[]));
            probe["active_checkpoints"].as_array_mut().unwrap().push(serde_json::json!(independent.name));
            topology.checkpoints.push(independent.clone());
            probe["interface_state"][provider.as_str()][RUN_AUTHORING_BINDING] = serde_json::json!(topology);
            let probe_bytes = serde_json::to_vec(&probe).unwrap();
            std::fs::write(&run_path, &probe_bytes).unwrap();
            let mut probe_ctx = ctx.clone();
            probe_ctx.movements_path = ctx.movements_path.with_file_name("consumed-inputs.jsonl");
            probe_ctx.ledger_index_path = ctx.ledger_index_path.with_file_name("unused-inputs.sqlite");
            let rows = [(0, "stable_input", "world_input_profile_test", 4), (1, "independent_probe", "world_peer_probe", 0)]
                .into_iter().map(|(seq, checkpoint, source, epoch)| {
                    let mut metadata = input.metadata().unwrap().clone();
                    metadata.runtime.as_mut().unwrap().checkpoint = CheckpointId::new(checkpoint);
                    metadata.runtime.as_mut().unwrap().epoch = epoch;
                    metadata.runtime.as_mut().unwrap().run_record_hash = Some(ContentHash::of_bytes(&probe_bytes).to_hex());
                    if checkpoint == "independent_probe" {
                        metadata = metadata.with_checkpoint_binding_policy(independent.binding_policy_snapshot())
                            .with_outbound_interface_bindings(vec![provider.clone()]);
                    }
                    Movement { seq: Seq(seq), lamport: Lamport(seq), recorded_at_unix_ms: 0,
                        trace: TraceId("probe".into()), kind: MovementKind::InterfaceOutbound,
                        from: Endpoint::Interface(InterfaceId::new(source)), to: Endpoint::Interface(provider.clone()),
                        artifact: Some(input.artifact.clone()), invocation_metadata: Some(metadata.into()), }
                }).collect::<Vec<_>>();
            std::fs::write(&probe_ctx.movements_path, rows.iter().map(|row| format!("{}\n", serde_json::to_string(row).unwrap())).collect::<String>()).unwrap();
            let probe_session = ProviderHandoffSession::new(&probe_ctx).unwrap();
            assert_eq!(probe_session.consumed_context_input_sources(&probe_ctx, &descriptor).unwrap(),
                vec![(run.clone(), 0), (run.clone(), 1)]);
            let mut independent_input = input.clone();
            independent_input.metadata = Some(rows[1].invocation_metadata.as_ref().unwrap().invocation().unwrap().clone());
            let mut changed = probe.clone();
            changed["interface_state"]["system_neighbor"] = serde_json::json!({"revision":7});
            let changed_bytes = serde_json::to_vec(&changed).unwrap();
            std::fs::write(&run_path, &changed_bytes).unwrap();
            let independent_handoff = probe_session.from_invocation(
                &probe_ctx, &descriptor, &run, &independent_input, 1, &probe_bytes,
            ).unwrap();
            assert!(!independent_handoff.source_carries_context);
            assert!(independent_handoff.invocation_session_snapshot().is_none());
            let state = independent_handoff.state.try_lock().unwrap();
            assert!(state.session_context.artifacts.is_empty());
            assert_eq!(state.run_record_bytes, changed_bytes);
            assert_eq!(state.consumed_context_free_independent_input.as_ref(), Some(&independent));
            assert!(probe_session.from_invocation(&probe_ctx, &descriptor, &run, &input, 0, &probe_bytes).is_err());
            assert!(probe_session.from_invocation(&probe_ctx, &descriptor, &run, &independent_input, 0, &probe_bytes).is_err());
            let cp_index = changed["checkpoints"].as_array().unwrap().iter().position(|cp| cp["id"] == "independent_probe").unwrap();
            let template_index = topology.checkpoints.len() - 1;
            for (pointer, value) in [
                ("/ee".to_string(), serde_json::json!("other_ee")),
                (format!("/checkpoints/{cp_index}/epoch"), serde_json::json!(1)),
                (format!("/checkpoints/{cp_index}/interface_binding/interface"), serde_json::json!("other_source")),
                (format!("/interface_state/{provider}/run_authoring_binding/checkpoints/{template_index}/include_context"), serde_json::json!(true)),
            ] {
                let mut drift = changed.clone();
                *drift.pointer_mut(&pointer).unwrap() = value;
                std::fs::write(&run_path, serde_json::to_vec(&drift).unwrap()).unwrap();
                assert!(probe_session.from_invocation(&probe_ctx, &descriptor, &run, &independent_input, 1, &probe_bytes).is_err(), "{pointer}");
            }
            std::fs::write(&run_path, &changed_bytes).unwrap();
            let mut forged = independent_input.clone();
            forged.metadata.as_mut().unwrap().runtime.as_mut().unwrap().run_record_hash = Some(ContentHash::of_bytes(b"wrong").to_hex());
            assert!(probe_session.from_invocation(&probe_ctx, &descriptor, &run, &forged, 1, &probe_bytes).is_err());
            forged = independent_input.clone();
            forged.artifact.content = ContentHash::of_bytes(b"wrong");
            assert!(probe_session.from_invocation(&probe_ctx, &descriptor, &run, &forged, 1, &probe_bytes).is_err());
            let mut rearmed = probe;
            rearmed["checkpoints"].as_array_mut().unwrap().iter_mut().find(|cp| cp["id"] == "independent_probe").unwrap()["epoch"] = serde_json::json!(1);
            std::fs::write(&run_path, serde_json::to_vec(&rearmed).unwrap()).unwrap();
            assert_eq!(probe_session.consumed_context_input_sources(&probe_ctx, &descriptor).unwrap(), vec![(run.clone(), 0)]);
            std::fs::write(&run_path, &run_bytes).unwrap();
        }
        let topology = handoff.bound_topology.clone();
        assert_eq!(topology.run, run);
        assert_eq!(topology.provider_interface, provider);
        assert_eq!(topology.checkpoints.len(), 5);
        {
            let failed = session
                .failed_source_handoff(&ctx, &descriptor, &run, &input, 23)
                .unwrap();
            assert!(failed.failed_source && !failed.source_carries_context);
            assert!(failed.context_output_labels.is_empty());
            assert!(failed.session_snapshot_labels.is_empty());
            let error_file = InterfaceArtifactFile::write(
                var_dir.join("emitted"),
                "turn.error",
                br#"{"label":"turn.error","message":"turn stopped before dispatch"}"#,
            )
            .unwrap();
            let state = failed.state.try_lock().unwrap();
            assert!(state.consumed_input);
            assert_eq!(state.context_artifacts, vec![ArtifactId::new("artifact-context")]);
            let rearmed = failed
                .author_run_with_bindings(
                    &state.run_record_bytes,
                    &state.context_artifacts,
                    &[PreparedHandoff {
                        identity: artifact_identity(&error_file, &var_dir).unwrap(),
                        output_checkpoint: failed
                            .config
                            .checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput)
                            .unwrap()
                            .clone(),
                        verified_inputs: Vec::new(),
                    }],
                    state.consumed_input,
                    None,
                    false,
                    true,
                    &[],
                )
                .unwrap();
            let rearmed_record = RunRecord::from_bytes(rearmed.content.as_bytes()).unwrap();
            assert_eq!(rearmed_record.status, serde_json::json!("idle"));
            assert!(rearmed.pending_handoff.terminal);
            assert!(rearmed.pending_handoff.source_without_context);
            assert_eq!(
                rearmed_record.interface_state[&provider]["context_generation"],
                RunRecord::from_bytes(&state.run_record_bytes).unwrap().interface_state
                    [&provider]["context_generation"]
            );
            assert_eq!(rearmed.pending_handoff.source_movement_seq, 23);
            let mut probe = handoff.clone();
            probe.run_path = var_dir.join("failed-context-probe.json");
            probe.ctx.movements_path = var_dir.join("failed-context-probe.jsonl");
            probe.ctx.ledger_index_path = var_dir.join("failed-context-probe.sqlite");
            std::fs::write(&probe.run_path, rearmed.content.as_bytes()).unwrap();
            std::fs::write(ctx.blobs_dir.join(ContentHash::of_bytes(&run_bytes).to_hex()), &run_bytes).unwrap();
            let source_ref = pin_artifact(&input.artifact_file, &ctx.blobs_dir);
            let source_template = handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::Stable).unwrap();
            let metadata = input.metadata().unwrap().clone()
                .with_checkpoint_binding_policy(source_template.binding_policy_snapshot())
                .with_outbound_interface_bindings(vec![provider.clone()]);
            let mut rows = (0..24).map(ledger_row).collect::<Vec<_>>();
            rows[23].kind = MovementKind::InterfaceOutbound;
            rows[23].from = Endpoint::Interface(source_template.interface().clone());
            rows[23].to = Endpoint::Interface(provider.clone());
            rows[23].artifact = Some(source_ref);
            rows[23].invocation_metadata = Some(metadata.into());
            std::fs::write(&probe.ctx.movements_path, rows.iter()
                .map(|row| format!("{}\n", serde_json::to_string(row).unwrap())).collect::<String>()).unwrap();
            probe.session = ProviderHandoffSession::new(&probe.ctx).unwrap();
            let mut probe_state = handoff.state.try_lock().unwrap().clone();
            probe_state.run_record_bytes = rearmed.content.as_bytes().to_vec();
            let before = session_artifacts_fingerprint(&probe_state.session_context.artifacts).unwrap();
            let error = probe.restore_failed_context(&mut probe_state, &[]).unwrap_err();
            assert!(error.contains("no declared checked owner writers"), "{error}");
            assert_eq!(session_artifacts_fingerprint(&probe_state.session_context.artifacts).unwrap(), before);
            probe.source_carries_context = false;
            assert!(probe.checked_failed_source_contexts(&probe_state.run_record_bytes, &[], None).unwrap().is_empty());
            assert!(!rearmed_record
                .active_checkpoints
                .contains(&CheckpointId::new("stable_input")));
            let rearmed_input = rearmed_record
                .checkpoints
                .iter()
                .find(|checkpoint| {
                    rearmed_record.active_checkpoints.contains(&checkpoint.id)
                        && checkpoint.inbound_interface().map(InterfaceId::as_str)
                            == Some("world_input_profile_test")
                })
                .unwrap();
            assert_eq!(
                rearmed_input
                    .artifact_bindings
                    .iter()
                    .map(|binding| binding.id.as_str())
                    .collect::<Vec<_>>(),
                ["artifact-context"]
            );
            let tool_file = InterfaceArtifactFile::write(var_dir.join("emitted"), "tool.call",
                br#"{"tool":"coder.exec","arguments":{},"correlation_id":"refused-tool"}"#).unwrap();
            let tool = handoff.author_run(&run_bytes, &state.context_artifacts,
                &[PreparedHandoff {
                    identity: artifact_identity(&tool_file, &var_dir).unwrap(),
                    output_checkpoint: handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderToolOutput).unwrap().clone(),
                    verified_inputs: Vec::new(),
                }], true, None, false, true).unwrap();
            let tool_record = RunRecord::from_bytes(tool.content.as_bytes()).unwrap();
            assert!(!tool_record.checkpoints.iter().any(|checkpoint| checkpoint.outbound_interfaces()
                .any(|interface| interface.as_str() == "world_client_profile_test")));
            let mut recovery = failed.clone();
            recovery.ctx.movements_path = var_dir.join("refused-pending.jsonl");
            recovery.ctx.ledger_index_path = var_dir.join("refused-pending.sqlite");
            let hash = ContentHash::of_bytes(tool.content.as_bytes());
            std::fs::write(ctx.blobs_dir.join(hash.to_hex()), tool.content.as_bytes()).unwrap();
            let mut rows = (0..24).map(ledger_row).collect::<Vec<_>>();
            let save = |rows: &[Movement]| std::fs::write(&recovery.ctx.movements_path,
                rows.iter().map(|row| format!("{}\n", serde_json::to_string(row).unwrap())).collect::<String>()).unwrap();
            save(&rows);
            assert!(recovery.validate_refused_source_handoff(&tool_record, &tool.pending_handoff).is_err());
            let mut refusal = ledger_row(24);
            refusal.kind = MovementKind::DeliveryRefused;
            refusal.from = Endpoint::Interface(provider.clone());
            refusal.to = Endpoint::Interface(tool_record.worker.clone());
            refusal.artifact = Some(artifact_identity(&tool_file, &var_dir).unwrap().artifact);
            refusal.invocation_metadata = Some(octopus_ledger::MovementMetadata::Delivery(octopus_ledger::DeliveryMetadata {
                delivery_verdict: DeliveryVerdict::Refused { reason: octopus_ledger::RefusedReason::Policy, detail: serde_json::json!({"message":"descriptor unavailable"}) },
                runtime: Some(RuntimeInvocationMetadata {
                    run: run.clone(), ee: tool_record.ee.clone(), worker: tool_record.worker.clone(),
                    checkpoint: tool.pending_handoff.items[0].checkpoint.id.clone(), interface: provider.clone(),
                    epoch: tool.pending_handoff.items[0].checkpoint.epoch, run_record_hash: Some(hash.to_hex()),
                }), source_movement_seq: None,
            }));
            rows.push(refusal.clone()); save(&rows);
            recovery.validate_refused_source_handoff(&tool_record, &tool.pending_handoff).unwrap();
            let error_projection = recovery.author_run_with_bindings(tool.content.as_bytes(), &state.context_artifacts,
                &[PreparedHandoff {
                    identity: artifact_identity(&error_file, &var_dir).unwrap(),
                    output_checkpoint: recovery.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap().clone(),
                    verified_inputs: Vec::new(),
                }], true, None, false, true, &[]).unwrap();
            assert!(error_projection.pending_handoff.source_without_context);
            assert_eq!(error_projection.pending_handoff.items[0].artifact_label, "turn.error");
            assert_eq!(error_projection.pending_handoff.items[0].checkpoint.outbound_interface_bindings[0].interface.as_str(), "world_client_profile_test");
            assert_eq!(error_projection.before_hash, hash.to_hex());
            let mut drift = tool.pending_handoff.clone(); drift.items[0].checkpoint.epoch += 1;
            assert!(recovery.validate_refused_source_handoff(&tool_record, &drift).is_err());
            let mut admitted = refusal.clone(); admitted.seq = Seq(25); admitted.kind = MovementKind::InterfaceArrival;
            rows.push(admitted); save(&rows);
            assert!(recovery.validate_refused_source_handoff(&tool_record, &tool.pending_handoff).is_err());
            rows.pop(); rows[24].artifact.as_mut().unwrap().content = ContentHash::of_bytes(b"other"); save(&rows);
            assert!(recovery.validate_refused_source_handoff(&tool_record, &tool.pending_handoff).is_err());
        }
        let output_file = InterfaceArtifactFile::write(
            var_dir.join("emitted"),
            "interface.artifact.output",
            br#"{"text":"profile output"}"#,
        )
        .unwrap();
        let output_identity = artifact_identity(&output_file, &var_dir).unwrap();
        let output_checkpoint = handoff
            .config
            .checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput)
            .unwrap()
            .clone();
        let authored = handoff
            .author_run(
                &run_bytes,
                &[],
                &[PreparedHandoff {
                    identity: output_identity,
                    output_checkpoint,
                    verified_inputs: Vec::new(),
                }],
                true,
                None,
                false,
                false,
            )
            .unwrap();
        let authored_record = RunRecord::from_bytes(authored.content.as_bytes()).unwrap();
        assert_eq!(authored_record.status, serde_json::json!("running"));
        validate_persisted_profile_binding(&authored_record, &handoff.config, &topology).unwrap();
        let extra_role = ArtifactId::new("artifact-connected-worker-role");
        let connected = handoff.author_run_with_bindings(
            &run_bytes, std::slice::from_ref(&extra_role),
            &[PreparedHandoff {
                identity: artifact_identity(&output_file, &var_dir).unwrap(),
                output_checkpoint: handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap().clone(),
                verified_inputs: Vec::new(),
            }], true, None, false, false,
            &[ProviderOutgoingBinding {
                interface: InterfaceId::new("system_worker_peer"),
                artifact_label: "worker.message".into(),
                artifact_bindings: vec![extra_role.clone()],
            }],
        ).unwrap();
        let connected_record = RunRecord::from_bytes(connected.content.as_bytes()).unwrap();
        let connected_config = config_from_persisted_profile(&connected_record, &provider, &profile).unwrap();
        let connected_topology: BoundRunAuthoringTopology = serde_json::from_value(
            connected_record.interface_state[&provider][RUN_AUTHORING_BINDING].clone()
        ).unwrap();
        assert!(preserves_provider_topology(&topology, &connected_topology));
        assert_eq!(current_mandatory_role_artifacts(&connected_record, &connected_config).unwrap(), vec![extra_role.clone()]);
        assert!(connected_record.checkpoints.iter().any(|checkpoint| {
            connected_record.active_checkpoints.contains(&checkpoint.id)
                && checkpoint.outbound_interface_bindings.iter().any(|binding| {
                    binding.interface.as_str() == "system_worker_peer"
                        && binding.admits("worker.message") && !binding.admits("interface.artifact.output")
                })
        }));
        let mut stolen = connected_topology.clone();
        stolen.checkpoints[0].outbound_interface_bindings[0].interface = InterfaceId::new("system_stolen");
        assert!(!preserves_provider_topology(&topology, &stolen));
        {
            let identity = artifact_identity(&output_file, &var_dir).unwrap();
            let authored = handoff.author_run(authored.content.as_bytes(), std::slice::from_ref(&identity.artifact.id.clone()),
                &[PreparedHandoff { identity, output_checkpoint: handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap().clone(), verified_inputs: Vec::new() }],
                true, None, false, true).unwrap();
            let authored_record = RunRecord::from_bytes(authored.content.as_bytes()).unwrap();
            let mut route = connected_topology.checkpoints.last().unwrap().clone();
            route.static_artifact_bindings.clear();
            let mut observed: serde_json::Value = serde_json::from_str(&authored.content).unwrap();
            let id = "new_outgoing_connection";
            let roles = current_mandatory_role_bindings(&authored_record, &handoff.config).unwrap()
                .iter().map(serde_json::to_value).collect::<Result<Vec<_>, _>>().unwrap();
            observed["checkpoints"].as_array_mut().unwrap().push(materialize_checkpoint(&route, id, &run, &roles));
            observed["active_checkpoints"].as_array_mut().unwrap().push(serde_json::json!(id));
            observed["interface_state"][provider.as_str()][FRONTIER_CHECKPOINT_IDS].as_array_mut().unwrap().push(serde_json::json!(id));
            observed["interface_state"][provider.as_str()][RUN_AUTHORING_BINDING]["checkpoints"].as_array_mut().unwrap().push(serde_json::json!(route));
            let encode = |value: &serde_json::Value| { let mut bytes = serde_json::to_vec_pretty(value).unwrap(); bytes.push(b'\n'); bytes };
            let projection = output_connections::protected_record(authored.content.as_bytes(), &encode(&observed), &handoff.config).unwrap().unwrap();
            assert_eq!(projection, authored.content.as_bytes(), "routing projection must end at the exact evidenced bytes");
            let publication = handoff.author_context_publication(authored.content.as_bytes(), &authored.pending_handoff).unwrap();
            let rebased = authoring_retry::rebase_authored_run(authored.content.as_bytes(), &encode(&observed), &publication, &handoff.config, Some(JsonFormat::Compact)).unwrap();
            let rebased_record = RunRecord::from_bytes(rebased.content.as_bytes()).unwrap();
            assert_eq!(rebased.pending_handoff, publication.pending_handoff);
            assert_eq!(rebased_record.checkpoint(&CheckpointId::new(id)), RunRecord::from_bytes(&encode(&observed)).unwrap().checkpoint(&CheckpointId::new(id)));
            assert_eq!(output_connections::protected_record(publication.content.as_bytes(), rebased.content.as_bytes(), &handoff.config).unwrap().unwrap(), publication.content.as_bytes());
            assert!(checked_selected_run_advance_state(&ctx, &run_path, authored.content.as_bytes(), &encode(&observed), 23, &handoff.config).unwrap().is_empty());
            let mut history = observed.clone();
            history["interface_state"][provider.as_str()]["context_generation"] = serde_json::json!(200);
            assert!(checked_selected_run_advance_state(&ctx, &run_path, authored.content.as_bytes(), &encode(&history), 23, &handoff.config).is_err(), "an empty writer set cannot prove changed history");
            assert!(authoring_retry::rebase_authored_run(authored.content.as_bytes(), &encode(&history), &publication, &handoff.config, Some(JsonFormat::Compact)).is_err());
            for mutation in 0..4 {
                let mut invalid = observed.clone();
                match mutation {
                    0 => invalid["interface_state"][provider.as_str()][RUN_AUTHORING_BINDING]["checkpoints"].as_array_mut().unwrap().last_mut().unwrap()["static_artifact_bindings"] = serde_json::json!(["ungranted-context"]),
                    1 => invalid["checkpoints"].as_array_mut().unwrap().last_mut().unwrap()["artifact_bindings"] = serde_json::json!([{"id":"ungranted-context"}]),
                    2 => { invalid["active_checkpoints"].as_array_mut().unwrap().pop(); },
                    _ => invalid["interface_state"][provider.as_str()]["pending_handoff"]["items"][0]["checkpoint"]["id"] = serde_json::json!(id),
                }
                assert!(output_connections::protected_record(authored.content.as_bytes(), &encode(&invalid), &handoff.config).is_err(), "invalid outgoing mutation {mutation}");
            }
            let mut input_change = observed;
            input_change["interface_state"][provider.as_str()][RUN_AUTHORING_BINDING]["checkpoints"][0]["include_context"] = serde_json::json!(false);
            assert!(output_connections::protected_record(authored.content.as_bytes(), &encode(&input_change), &handoff.config).unwrap().is_none(), "input changes are not routing additions");
        }
        std::fs::write(&run_path, authored.content.as_bytes()).unwrap();
        let replay = session
            .from_invocation(&ctx, &descriptor, &run, &input, 23, &run_bytes)
            .unwrap();
        assert_eq!(replay.bound_topology, topology);
        assert_eq!(
            frontier_checkpoint_ids(authored_record.interface_state.get(&provider).unwrap())
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            config_from_persisted_profile(&authored_record, &provider, &profile).unwrap(),
            handoff.config
        );
        let result_payload = serde_json::to_vec(&serde_json::json!({
            "label": "file.write.result",
            "tool": "file.write",
            "correlation_id": authored.correlation_id,
            "ok": true,
            "body": {"result": "files_written", "writes": []}
        }))
        .unwrap();
        let result_file = InterfaceArtifactFile::write(
            var_dir.join("result"),
            "file.write.result",
            &result_payload,
        )
        .unwrap();
        let result_ref = artifact_identity(&result_file, &var_dir).unwrap().artifact;
        let result_binding_policy = handoff
            .config
            .checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)
            .unwrap()
            .binding_policy_snapshot();
        let result_envelope = InterfaceInvocationEnvelope {
            artifact: result_ref.clone(),
            artifact_file: result_file,
            artifact_binding_payloads: Vec::new(),
            metadata: Some(
                InvocationMetadata::new(GrantProjection::default())
                    .with_runtime(RuntimeInvocationMetadata {
                        run: run.clone(),
                        ee: EeId::new("ee_profile_test"),
                        worker: InterfaceId::new("system_worker_profile_test"),
                        checkpoint: authored.authoring_result_checkpoint.clone(),
                        interface: provider.clone(),
                        epoch: 0,
                        run_record_hash: Some(authored.after_hash.clone()),
                    })
                    .with_outbound_interface_bindings(vec![provider.clone()])
                    .with_checkpoint_binding_policy(result_binding_policy),
            ),
        };
        let movement = Movement {
            seq: Seq(31),
            lamport: Lamport(32),
            recorded_at_unix_ms: 33,
            trace: TraceId("trace-profile-result".to_string()),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(authored.authoring_result_interface.clone()),
            to: Endpoint::Interface(provider.clone()),
            artifact: Some(result_ref),
            invocation_metadata: result_envelope.metadata.clone().map(Into::into),
        };
        result_identity
            .validate(
                &movement,
                &result_envelope,
                authored.content.as_bytes(),
                &result_payload,
            )
            .unwrap();
        let mut without_binding: serde_json::Value =
            serde_json::from_str(&authored.content).unwrap();
        without_binding["interface_state"][provider.as_str()]
            .as_object_mut()
            .unwrap()
            .remove(RUN_AUTHORING_BINDING);
        let error = result_identity
            .validate(
                &movement,
                &result_envelope,
                &serde_json::to_vec(&without_binding).unwrap(),
                &result_payload,
            )
            .unwrap_err();
        assert!(error.contains("no durable run_authoring_binding"));
        let mut stale_pending: serde_json::Value = serde_json::from_str(&authored.content).unwrap();
        stale_pending["interface_state"][provider.as_str()]["pending_handoff"]["items"][0]["checkpoint"]
            ["id"] = serde_json::json!("forged_pending_checkpoint");
        let error = result_identity
            .validate(
                &movement,
                &result_envelope,
                &serde_json::to_vec(&stale_pending).unwrap(),
                &result_payload,
            )
            .unwrap_err();
        assert!(error.contains("matches 0 active provider checkpoints"));
        let original = RunRecord::from_bytes(&run_bytes).unwrap();
        let call_file = handoff.materialize_file_write_call(&authored).unwrap();
        std::fs::write(ctx.blobs_dir.join(authored.before_hash.clone()), &run_bytes).unwrap();
        let mut rejection_ctx = ctx.clone();
        rejection_ctx.movements_path = dir.path().join("ledger/rejected-write.jsonl");
        rejection_ctx.ledger_index_path = dir.path().join("ledger/rejected-write.sqlite");
        let result_template = handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult).unwrap();
        let rejected_file = InterfaceArtifactFile::write(var_dir.join("rejected"), "file.write.result",
            &serde_json::to_vec(&serde_json::json!({
                "label":"file.write.result", "tool":"file.write", "ok":false,
                "correlation_id":authored.correlation_id,
                "error":{"code":"tool_execution_failed","message":"ordinary refusal"}
            })).unwrap()).unwrap();
        let mut rejected_envelope = result_envelope.clone();
        rejected_envelope.artifact = pin_artifact(&rejected_file, &ctx.blobs_dir);
        rejected_envelope.artifact_file = rejected_file;
        let rejected_metadata = rejected_envelope.metadata.as_mut().unwrap();
        rejected_metadata.runtime.as_mut().unwrap().checkpoint = active_checkpoint_for_role(
            &original, &handoff.config, RunAuthoringCheckpointRole::AuthoringResult).unwrap().id.clone();
        rejected_metadata.runtime.as_mut().unwrap().run_record_hash = Some(authored.before_hash.clone());
        let mut rejected_movement = movement.clone();
        rejected_movement.seq = Seq(25);
        rejected_movement.artifact = Some(rejected_envelope.artifact.clone());
        rejected_movement.invocation_metadata = rejected_envelope.metadata.clone().map(Into::into);
        let mut call_movement = ledger_row(24);
        call_movement.kind = MovementKind::InterfaceOutbound;
        call_movement.from = Endpoint::Interface(handoff.config.authoring_source_interface.clone());
        call_movement.to = Endpoint::Interface(result_template.interface().clone());
        call_movement.artifact = Some(pin_artifact(&call_file, &ctx.blobs_dir));
        let mut call_metadata = rejected_envelope.metadata.clone().unwrap();
        call_metadata.runtime.as_mut().unwrap().interface = result_template.interface().clone();
        call_metadata.grant.tools = vec![handoff.config.file_write_tool.clone()];
        call_metadata.grant.interfaces = vec![result_template.interface().to_string()];
        call_movement.invocation_metadata = Some(call_metadata.into());
        let mut rows = (0..24).map(ledger_row).collect::<Vec<_>>();
        rows.extend([call_movement, rejected_movement.clone()]);
        let journal = rows.iter().map(|row| format!("{}\n", serde_json::to_string(row).unwrap())).collect::<String>();
        std::fs::write(&rejection_ctx.movements_path, &journal).unwrap();
        let rejected: ToolResultArtifact = serde_json::from_slice(&rejected_envelope.read_artifact_payload().unwrap()).unwrap();
        authoring_retry::validate_rejected_result(&rejection_ctx, &handoff.config, &rejected_movement, &rejected_envelope, &original, &rejected).unwrap();
        let consumed = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(
            result_identity.consume_if_matches(&rejection_ctx, &rejected_movement, &rejected_envelope, "file.write.result")
        ).unwrap();
        assert!(consumed);
        assert_eq!(std::fs::read_to_string(&rejection_ctx.movements_path).unwrap(), journal);
        {
            let mut concurrent: serde_json::Value = serde_json::from_slice(&run_bytes).unwrap();
            concurrent["interface_state"]["system_tool_profile_test"] = serde_json::json!({"generation": 2});
            let current = serde_json::to_vec(&concurrent).unwrap();
            let hash = ContentHash::of_bytes(&current);
            std::fs::write(ctx.blobs_dir.join(hash.to_hex()), &current).unwrap();
            let mut refused = rejected_movement.clone();
            refused.kind = MovementKind::DeliveryRefused;
            refused.to = Endpoint::Interface(original.worker.clone());
            let mut runtime = rejected_envelope.metadata.as_ref().unwrap().runtime.clone().unwrap();
            runtime.interface = result_template.interface().clone();
            runtime.run_record_hash = Some(hash.to_hex());
            refused.invocation_metadata = Some(octopus_ledger::MovementMetadata::Delivery(octopus_ledger::DeliveryMetadata {
                delivery_verdict: DeliveryVerdict::Refused { reason: octopus_ledger::RefusedReason::CheckpointSpent, detail: serde_json::json!({}) },
                runtime: Some(runtime), source_movement_seq: None,
            }));
            let mut refused_rows = rows.clone();
            let save = |rows: &[Movement]| std::fs::write(&rejection_ctx.movements_path,
                rows.iter().map(|row| format!("{}\n", serde_json::to_string(row).unwrap())).collect::<String>()).unwrap();
            refused_rows[25] = refused.clone(); save(&refused_rows);
            assert!(authoring_retry::rejected_write(&rejection_ctx, &handoff.config, &authored).err().unwrap()
                .contains("complete admitted checked file.write chain"));
            if let Some(octopus_ledger::MovementMetadata::Delivery(metadata)) = refused.invocation_metadata.as_mut() {
                metadata.runtime.as_mut().unwrap().run_record_hash = Some(authored.before_hash.clone());
            }
            refused_rows[25] = refused.clone(); save(&refused_rows);
            let rejection = authoring_retry::rejected_write(&rejection_ctx, &handoff.config, &authored).unwrap().unwrap();
            assert_eq!(rejection.result_seq, 25);
            assert!(matches!(rejection.evidence, authoring_retry::RejectionEvidence::ToolResult { call_seq: 24, .. }));
            let rebased = authoring_retry::rebase_authored_run(&run_bytes, &current, &authored, &handoff.config, Some(JsonFormat::Compact)).unwrap();
            assert_eq!(rebased.before_hash, hash.to_hex());
            assert_eq!(rebased.pending_handoff, authored.pending_handoff);
            assert_eq!(serde_json::from_str::<serde_json::Value>(&rebased.content).unwrap()["interface_state"]["system_tool_profile_test"], concurrent["interface_state"]["system_tool_profile_test"]);
            let mut saved_handoff = handoff.clone();
            saved_handoff.hand_in_root = var_dir.join("saved-retries");
            let mut old = authored.clone();
            old.json_format = None;
            old.content = String::from_utf8(JsonFormat::Pretty.encode(&serde_json::from_str::<serde_json::Value>(&old.content).unwrap()).unwrap()).unwrap();
            old.after_hash = ContentHash::of_bytes(old.content.as_bytes()).to_hex();
            let cold_read = |before: &[u8], planned: &AuthoredRun| -> Result<AuthoredRun, String> {
                let file = saved_handoff.materialize_file_write_call(planned)?;
                validate_authoring_attempt_source(AuthoringAttemptSourceValidation {
                    ctx: &ctx, record: &RunRecord::from_bytes(before).unwrap(), record_bytes: before,
                    config: &handoff.config,
                    call: serde_json::from_slice(&file.read_payload_bytes().unwrap()).unwrap(),
                    manifest_path: file.manifest_path().to_path_buf(),
                }).map(|attempt| attempt.authored)
            };
            let old = cold_read(&run_bytes, &old).unwrap();
            for format in [None, Some(JsonFormat::Compact)] {
                let planned = authoring_retry::rebase_authored_run(&run_bytes, &current, &old, &handoff.config, format).unwrap();
                let saved = cold_read(&current, &planned).unwrap();
                assert_eq!(saved, planned);
                assert_eq!(saved.json_format, format);
                authoring_retry::validate_rebased_authored_run(&run_bytes, &current, &old, &saved, &handoff.config).unwrap();
                let mut altered = saved.clone();
                altered.json_format = if format.is_none() { Some(JsonFormat::Compact) } else { None };
                assert!(cold_read(&current, &altered).is_err());
                assert!(authoring_retry::validate_rebased_authored_run(&run_bytes, &current, &old, &altered, &handoff.config).is_err());
                altered = saved.clone();
                altered.content.push(' ');
                assert!(authoring_retry::validate_rebased_authored_run(&run_bytes, &current, &old, &altered, &handoff.config).is_err());
                altered.after_hash = ContentHash::of_bytes(altered.content.as_bytes()).to_hex();
                assert!(cold_read(&current, &altered).is_err());
                assert!(authoring_retry::validate_rebased_authored_run(&run_bytes, &current, &old, &altered, &handoff.config).is_err());
            }
            if let Some(octopus_ledger::MovementMetadata::Delivery(metadata)) = refused_rows[25].invocation_metadata.as_mut() {
                metadata.runtime.as_mut().unwrap().epoch += 1;
            }
            save(&refused_rows);
            assert!(authoring_retry::rejected_write(&rejection_ctx, &handoff.config, &authored).is_err());
            refused_rows[25] = refused;
            refused_rows[24].artifact = Some(rejected_envelope.artifact.clone()); save(&refused_rows);
            assert!(authoring_retry::rejected_write(&rejection_ctx, &handoff.config, &authored).unwrap().is_none());
            save(&rows);
        }
        let mut malformed = rejected.clone();
        malformed.body = Some(serde_json::json!({"result":"files_written"}));
        assert!(authoring_retry::validate_rejected_result(&rejection_ctx, &handoff.config, &rejected_movement, &rejected_envelope, &original, &malformed).is_err());
        malformed = rejected.clone();
        malformed.correlation_id = Some("provider-run-authoring:run_profile_test:999".into());
        assert!(authoring_retry::validate_rejected_result(&rejection_ctx, &handoff.config, &rejected_movement, &rejected_envelope, &original, &malformed).is_err());
        std::fs::write(&rejection_ctx.movements_path, rows[..24].iter().map(|row| format!("{}\n", serde_json::to_string(row).unwrap())).collect::<String>()).unwrap();
        assert!(authoring_retry::validate_rejected_result(&rejection_ctx, &handoff.config, &rejected_movement, &rejected_envelope, &original, &rejected).is_err());
        exercise_checked_verification_failure(&ctx, &handoff.config, &run_bytes, &run_path);
        exercise_ordered_verified_tool_recovery(&mut handoff, &run_bytes, &profile);
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
            .block_on(exercise_interrupted_source_order(&ctx, &descriptor, &run_bytes, &input));
    }

    fn exercise_checked_verification_failure(
        ctx: &InterfaceLiftContext,
        config: &RunAuthoringConfig,
        initial: &[u8],
        path: &Path,
    ) {
        let requirement: RequiredVerifiedArtifact = serde_json::from_value(serde_json::json!({
            "mark":"tool.call.allowed", "artifact_label":config.file_write_label, "verifier":"tool_call_allowed",
            "params":{"tools":[config.file_write_tool],"metadata_matches":[{"payload":"/arguments/expected_before_hash","metadata":"/runtime/run_record_hash"}]}
        })).unwrap();
        let mut original = RunRecord::from_bytes(initial).unwrap();
        let call_id = active_checkpoint_for_role(
            &original,
            config,
            RunAuthoringCheckpointRole::AuthoringCall,
        )
        .unwrap()
        .id
        .clone();
        original
            .checkpoints
            .iter_mut()
            .find(|cp| cp.id == call_id)
            .unwrap()
            .required_verified_artifacts = vec![requirement.clone()];
        let before = serde_json::to_vec(&original).unwrap();
        let mut target = serde_json::to_value(&original).unwrap();
        target["interface_state"][config.provider_interface.as_str()]["fixture_checked_intent"] =
            serde_json::json!(true);
        let edit = CheckedRunEdit::prepare(
            path.to_path_buf(),
            &before,
            target,
            "checked-gate-attempt".into(),
        )
        .unwrap();
        let writer = CheckedRunWriter {
            owner: config.provider_interface.clone(),
            source: config.authoring_source_interface.clone(),
            door: config
                .checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)
                .unwrap()
                .interface()
                .clone(),
            tool: config.file_write_tool.clone(),
            call_label: config.file_write_label.clone(),
            result_label: config.file_write_result_label.clone(),
        };
        let profile = RunAuthoringProfile {
            file_write_tool: writer.tool.clone(),
            file_write_label: writer.call_label.clone(),
            file_write_result_label: writer.result_label.clone(),
            result_timeout_ms: 1000,
        };
        let file = edit
            .materialize(&ctx.var_dir.join("checked-gate"), &profile)
            .unwrap();
        let reference = pin_artifact(&file, &ctx.blobs_dir);
        std::fs::write(ctx.blobs_dir.join(&edit.before_hash), &before).unwrap();
        let mut concurrent = serde_json::to_value(&original).unwrap();
        concurrent["interface_state"]["system_independent_fixture"] =
            serde_json::json!({"generation":1});
        let concurrent = serde_json::to_vec(&concurrent).unwrap();
        let hash = ContentHash::of_bytes(&concurrent);
        std::fs::write(ctx.blobs_dir.join(hash.to_hex()), &concurrent).unwrap();
        let checkpoint = original.checkpoint(&call_id).unwrap();
        let mut metadata = InvocationMetadata::new(GrantProjection {
            tools: vec![writer.tool.clone()],
            interfaces: vec![writer.door.to_string()],
            mark_outcomes: vec![requirement.mark.to_string()],
            ..Default::default()
        })
        .with_runtime(octopus_interfaces::RuntimeInvocationMetadata {
            run: original.id.clone(),
            ee: original.ee.clone(),
            worker: original.worker.clone(),
            checkpoint: call_id.clone(),
            interface: writer.source.clone(),
            epoch: checkpoint.epoch.0,
            run_record_hash: Some(hash.to_hex()),
        })
        .with_outbound_interface_bindings(vec![writer.door.clone()])
        .with_checkpoint_binding_policy(CheckpointBindingPolicySnapshot {
            inbound: checkpoint.interface_binding.clone().unwrap(),
            outbound: checkpoint.outbound_interface_bindings.clone(),
        })
        .with_required_verified_artifacts(vec![requirement.clone()])
        .with_checkpoint_admission(octopus_ledger::CheckpointAdmissionMetadata {
            artifact_label: writer.call_label.clone(),
            required_artifact_labels: vec![writer.call_label.clone()],
            complete: true,
        });
        metadata.verification_failure = Some(octopus_ledger::VerificationFailureMetadata {
            error: "verifier_rejected".into(),
            verifier: Some(requirement.verifier),
            mark: Some(requirement.mark),
            artifact_label: None,
            run: None,
            checkpoint: None,
            verifier_package: None,
            details: Some(serde_json::json!({"reason":"metadata_mismatch"})),
        });
        let mut arrival = ledger_row(24);
        arrival.kind = MovementKind::InterfaceArrival;
        arrival.trace = original.trace.clone();
        arrival.from = Endpoint::Interface(writer.source.clone());
        arrival.to = Endpoint::Interface(original.worker.clone());
        arrival.artifact = Some(reference);
        arrival.invocation_metadata = Some(metadata.clone().into());
        let mut failure = arrival.clone();
        failure.seq = octopus_ledger::Seq(25);
        failure.kind = MovementKind::VerificationFailed;
        let mut failure_ctx = ctx.clone();
        failure_ctx.movements_path = ctx.var_dir.join("checked-gate-ledger.jsonl");
        failure_ctx.ledger_index_path = ctx.var_dir.join("checked-gate-ledger.sqlite");
        let mut rows = (0..24).map(ledger_row).collect::<Vec<_>>();
        rows.extend([arrival, failure.clone()]);
        let save = |rows: &[Movement]| {
            std::fs::write(
                &failure_ctx.movements_path,
                rows.iter()
                    .map(|row| format!("{}\n", serde_json::to_string(row).unwrap()))
                    .collect::<String>(),
            )
            .unwrap()
        };
        save(&rows);
        assert_eq!(
            checked_edit_verification_failure(&failure_ctx, &writer, &edit, 23).unwrap(),
            Some(failure)
        );
        let mut different = edit.clone();
        different.correlation_id.push_str("other");
        assert!(
            checked_edit_verification_failure(&failure_ctx, &writer, &different, 23)
                .unwrap()
                .is_none()
        );
        rows[24].kind = MovementKind::ArtifactMoved;
        save(&rows);
        assert!(checked_edit_verification_failure(&failure_ctx, &writer, &edit, 23).is_err());
        rows[24].kind = MovementKind::InterfaceArrival;
        metadata.runtime.as_mut().unwrap().checkpoint = CheckpointId::new("foreign");
        rows[25].invocation_metadata = Some(metadata.into());
        save(&rows);
        assert!(checked_edit_verification_failure(&failure_ctx, &writer, &edit, 23).is_err());
        let capture_path = ctx.var_dir.join("coherent-capture.json");
        std::fs::write(&capture_path, &before).unwrap();
        let captured = stable_run_observation(
            &capture_path,
            &before,
            ctx.required_host_io_limits().unwrap().max_payload_bytes,
            || -> Result<(), CheckedRunError> {
                std::fs::write(&capture_path, &concurrent).unwrap();
                Err("checked result used an unproven snapshot".into())
            },
        );
        assert!(matches!(
            captured,
            Err(CheckedRunError::ChangedDuringRead { .. })
        ));
        assert!(matches!(
            stable_run_observation(
                &capture_path,
                &concurrent,
                ctx.required_host_io_limits().unwrap().max_payload_bytes,
                || -> Result<(), CheckedRunError> { Err("broken stable evidence".into()) }
            ),
            Err(CheckedRunError::Invalid(_))
        ));
        assert!(
            stable_run_observation(
                &capture_path,
                &concurrent,
                ctx.required_host_io_limits().unwrap().max_payload_bytes,
                || Ok(())
            )
            .is_ok()
        );
    }

    fn exercise_ordered_verified_tool_recovery(
        handoff: &mut ProviderOutputHandoff,
        initial: &[u8],
        profile: &RunAuthoringProfile,
    ) {
        let provider = handoff.config.provider_interface.clone();
        let tool_door = InterfaceId::new("system_tool_profile_test");
        let mut consumed = handoff
            .config
            .checkpoint_for_role(RunAuthoringCheckpointRole::Stable)
            .unwrap()
            .clone();
        consumed.name = "tool_result_input".into();
        consumed.interface_binding = AuthoredInterfaceBinding::all(tool_door.clone());
        handoff.config.checkpoints.push(consumed.clone());
        handoff.bound_topology.checkpoints.push(consumed.clone());
        handoff.source_movement_seq = 0;
        let mut initial: serde_json::Value = serde_json::from_slice(initial).unwrap();
        initial["checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(materialize_checkpoint(
                &consumed,
                &consumed.name,
                &handoff.config.run,
                &[],
            ));
        initial["active_checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!(consumed.name));
        initial["interface_state"][provider.as_str()][RUN_AUTHORING_BINDING] =
            serde_json::to_value(&handoff.bound_topology).unwrap();
        let initial = serde_json::to_vec(&initial).unwrap();
        let output = handoff
            .config
            .checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput)
            .unwrap()
            .clone();
        let route = ProviderHandoffRoute::ClientProjectionWithVerifiedInputs {
            inputs: [
                ("operator.approval", "operator_approval"),
                ("operator.input.provided", "operator_input"),
            ]
            .into_iter()
            .map(|(mark, verifier)| ProviderVerifiedInput {
                also_deliver_to_tool: Some("coder.exec".into()),
                artifact_label: "operator.approval.response".into(),
                required_verified_artifact: RequiredVerifiedArtifact {
                    mark: octopus_descriptors::MarkId::new(mark),
                    verifier: octopus_descriptors::VerifierId::new(verifier),
                    artifact_label: "operator.approval.response".into(),
                    params: serde_json::json!({"request_id":"request-1","call_id":"call-1"}),
                },
            })
            .collect(),
        };
        let marks = BTreeSet::from(["operator.approval".into(), "operator.input.provided".into()]);
        let interfaces = BTreeSet::from([provider.clone(), tool_door.clone()]);
        let tools = BTreeSet::from(["coder.exec".into()]);
        let verified = handoff
            .config
            .verified_input_checkpoints(&route, &output, &marks, &tools, &interfaces)
            .unwrap();
        assert!(
            verified
                .iter()
                .all(|checkpoint| checkpoint.outbound_interfaces().eq([&provider, &tool_door]))
        );
        assert!(
            handoff
                .config
                .verified_input_checkpoints(&route, &output, &marks, &BTreeSet::new(), &interfaces)
                .is_err()
        );
        let request = InterfaceArtifactFile::write(
            handoff.var_dir.join("approval-request"),
            "operator.approval.request",
            br#"{"request_id":"request-1"}"#,
        )
        .unwrap();
        let authored_request = handoff
            .author_run(
                &initial,
                &[],
                &[PreparedHandoff {
                    identity: artifact_identity(&request, &handoff.var_dir).unwrap(),
                    output_checkpoint: output.clone(),
                    verified_inputs: verified.clone(),
                }],
                true,
                None,
                false,
                true,
            )
            .unwrap();
        let request_record = RunRecord::from_bytes(authored_request.content.as_bytes()).unwrap();
        let request_config =
            config_from_persisted_profile(&request_record, &provider, profile).unwrap();
        let request_topology: BoundRunAuthoringTopology = serde_json::from_value(
            request_record.interface_state[&provider][RUN_AUTHORING_BINDING].clone(),
        )
        .unwrap();
        handoff.config = request_config.clone();
        handoff.bound_topology = request_topology.clone();
        handoff.source_movement_seq = 1;
        let carrier = InterfaceArtifactFile::write(
            handoff.var_dir.join("approval-phase"),
            "interface.context.updated",
            br#"{"phase":"awaiting_resume"}"#,
        )
        .unwrap();
        let authored_response = handoff
            .author_run(
                authored_request.content.as_bytes(),
                &[],
                &[PreparedHandoff {
                    identity: artifact_identity(&carrier, &handoff.var_dir).unwrap(),
                    output_checkpoint: output,
                    verified_inputs: vec![],
                }],
                true,
                None,
                true,
                true,
            )
            .unwrap();
        let response_record = RunRecord::from_bytes(authored_response.content.as_bytes()).unwrap();
        let response_config =
            config_from_persisted_profile(&response_record, &provider, profile).unwrap();
        let response_topology: BoundRunAuthoringTopology = serde_json::from_value(
            response_record.interface_state[&provider][RUN_AUTHORING_BINDING].clone(),
        )
        .unwrap();
        assert!(
            !response_config
                .checkpoints
                .iter()
                .any(|checkpoint| checkpoint.role == RunAuthoringCheckpointRole::VerifiedInput)
        );
        let source_checkpoint = request_record
            .checkpoints
            .iter()
            .find(|checkpoint| {
                checkpoint_record_matches_template(checkpoint, &verified[0], &request_record.id)
            })
            .unwrap();
        assert!(response_record.checkpoint(&source_checkpoint.id).is_none());
        let decision = InterfaceArtifactFile::write(handoff.var_dir.join("approval-decision"), "operator.approval.response", br#"{"operator_approval_response":{"request_id":"request-1","call_id":"call-1","decision":"accept"}}"#).unwrap();
        let artifact = pin_artifact(&decision, &handoff.ctx.blobs_dir);
        let pinned_hash = ContentHash::of_bytes(authored_request.content.as_bytes());
        std::fs::write(
            handoff.ctx.blobs_dir.join(pinned_hash.to_hex()),
            authored_request.content.as_bytes(),
        )
        .unwrap();
        let runtime = octopus_interfaces::RuntimeInvocationMetadata {
            run: request_record.id.clone(),
            ee: request_record.ee.clone(),
            worker: request_record.worker.clone(),
            checkpoint: source_checkpoint.id.clone(),
            interface: provider.clone(),
            epoch: source_checkpoint.epoch.0,
            run_record_hash: Some(pinned_hash.to_hex()),
        };
        let metadata = InvocationMetadata::new(Default::default())
            .with_runtime(runtime)
            .with_outbound_interface_bindings(vec![provider.clone(), tool_door.clone()])
            .with_checkpoint_binding_policy(verified[0].binding_policy_snapshot())
            .with_required_verified_artifacts(verified[0].required_verified_artifacts.clone())
            .with_checkpoint_admission(octopus_ledger::CheckpointAdmissionMetadata {artifact_label:"operator.approval.response".into(),required_artifact_labels:vec!["operator.approval.response".into()],complete:true});
        let mut movement = ledger_row(1);
        movement.kind = MovementKind::InterfaceOutbound;
        movement.from = Endpoint::Interface(verified[0].interface().clone());
        movement.to = Endpoint::Interface(provider.clone());
        movement.artifact = Some(artifact);
        movement.invocation_metadata = Some(metadata.into());
        std::fs::write(
            &handoff.ctx.movements_path,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&ledger_row(0)).unwrap(),
                serde_json::to_string(&movement).unwrap()
            ),
        )
        .unwrap();
        let evidence = CheckedRunEvidence { movements_path:&handoff.ctx.movements_path,ledger_index_path:&handoff.ctx.ledger_index_path,blobs_dir:&handoff.ctx.blobs_dir,max_control_frame_bytes:1_048_576,max_manifest_bytes:1_048_576,max_payload_bytes:268_435_456 };
        let mut reader = FileCheckedRunReader::open(&evidence, None).unwrap();
        let writer = CheckedRunWriter {owner:provider.clone(),source:handoff.config.authoring_source_interface.clone(),door:handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult).unwrap().interface().clone(),tool:profile.file_write_tool.clone(),call_label:profile.file_write_label.clone(),result_label:profile.file_write_result_label.clone()};
        let mut call = ledger_row(100);
        call.invocation_metadata = Some(InvocationMetadata::new(GrantProjection { mark_outcomes:marks.iter().cloned().collect(), ..Default::default() }).into());
        for (before, after) in [(&initial, authored_request.content.as_bytes()), (&authored_request.content.as_bytes().to_vec(), authored_response.content.as_bytes())] {
            let change = CheckedRunTransition {writer:writer.clone(),before:before.to_vec(),after:after.to_vec(),call:call.clone(),result:ledger_row(101)};
            validate_checked_provider_transition(&change).unwrap();
            validate_verified_input_transition(&mut reader, &change).unwrap();
        }
        let mut wrong:serde_json::Value=serde_json::from_str(&authored_response.content).unwrap();
        wrong["interface_state"][provider.as_str()]["pending_handoff"]["source_movement_seq"]=serde_json::json!(0);
        let forged=CheckedRunTransition {writer,before:authored_request.content.as_bytes().to_vec(),after:serde_json::to_vec(&wrong).unwrap(),call,result:ledger_row(101)};
        assert!(validate_verified_input_transition(&mut reader, &forged).is_err());
        let session = ProviderHandoffSession::new(&handoff.ctx).unwrap();
        validate_retired_verified_input(
            &handoff.ctx,
            &session,
            &request_record,
            &response_record,
            &request_config,
            &response_config,
            &request_topology,
            &response_topology,
            &consumed,
            2,
        )
        .unwrap();
        let mut wrong_source = consumed.clone();
        wrong_source.interface_binding =
            AuthoredInterfaceBinding::all(InterfaceId::new("other_door"));
        assert!(
            validate_retired_verified_input(
                &handoff.ctx,
                &session,
                &request_record,
                &response_record,
                &request_config,
                &response_config,
                &request_topology,
                &response_topology,
                &wrong_source,
                2
            )
            .is_err()
        );
        assert!(
            validate_retired_verified_input(
                &handoff.ctx,
                &session,
                &request_record,
                &response_record,
                &request_config,
                &response_config,
                &request_topology,
                &response_topology,
                &consumed,
                1
            )
            .is_err()
        );
        let mut changed_neighbor = response_config.clone();
        changed_neighbor.checkpoints[0].interface_binding =
            AuthoredInterfaceBinding::all(InterfaceId::new("unrelated_change"));
        assert!(
            validate_retired_verified_input(
                &handoff.ctx,
                &session,
                &request_record,
                &response_record,
                &request_config,
                &changed_neighbor,
                &request_topology,
                &response_topology,
                &consumed,
                2
            )
            .is_err()
        );

        let tool_checkpoint = request_record
            .checkpoints
            .iter()
            .find(|checkpoint| {
                checkpoint_record_matches_template(checkpoint, &consumed, &request_record.id)
            })
            .unwrap();
        let result_file = InterfaceArtifactFile::write(handoff.var_dir.join("queued-tool-result"), "coder.exec.result", br#"{"label":"coder.exec.result","tool":"coder.exec","ok":true,"correlation_id":"resume-1","body":{}}"#).unwrap();
        let result_ref = pin_artifact(&result_file, &handoff.ctx.blobs_dir);
        let queued_metadata = InvocationMetadata::new(octopus_interfaces::GrantProjection {
            tools: vec!["coder.exec".into(), "file.write".into()],
            interfaces: vec![
                provider.as_str().into(),
                tool_door.as_str().into(),
                handoff.config.authoring_source_interface.as_str().into(),
                "system_context_authoring_profile_test".into(),
            ],
            ee_root_slot_allowance: Some(vec!["run_records_root".into()]),
            ..Default::default()
        })
        .with_runtime(octopus_interfaces::RuntimeInvocationMetadata {
            run: request_record.id.clone(),
            ee: request_record.ee.clone(),
            worker: request_record.worker.clone(),
            interface: provider.clone(),
            checkpoint: tool_checkpoint.id.clone(),
            epoch: tool_checkpoint.epoch.0,
            run_record_hash: Some(pinned_hash.to_hex()),
        })
        .with_checkpoint_binding_policy(consumed.binding_policy_snapshot())
        .with_outbound_interface_bindings(vec![provider.clone()]);
        let queued = InterfaceInvocationEnvelope {
            artifact: result_ref.clone(),
            artifact_file: result_file,
            artifact_binding_payloads: vec![],
            metadata: Some(queued_metadata.clone()),
        };
        let mut queued_movement = ledger_row(2);
        queued_movement.kind = MovementKind::InterfaceOutbound;
        queued_movement.from = Endpoint::Interface(tool_door);
        queued_movement.to = Endpoint::Interface(provider.clone());
        queued_movement.artifact = Some(result_ref);
        queued_movement.invocation_metadata = Some(queued_metadata.into());
        let mut log = format!(
            "{}\n{}\n{}\n",
            serde_json::to_string(&ledger_row(0)).unwrap(),
            serde_json::to_string(&movement).unwrap(),
            serde_json::to_string(&queued_movement).unwrap()
        );
        std::fs::write(&handoff.ctx.movements_path, &log).unwrap();
        std::fs::write(&handoff.run_path, authored_response.content.as_bytes()).unwrap();
        let descriptor = local_profile_descriptor();
        let before = ProviderHandoffSession::new(&handoff.ctx)
            .unwrap()
            .from_invocation(
                &handoff.ctx,
                &descriptor,
                &request_record.id,
                &queued,
                2,
                authored_request.content.as_bytes(),
            )
            .err()
            .unwrap();
        assert!(
            before.contains("precedes the admitted provider decision handoff"),
            "{before}"
        );
        let phase_ref = pin_artifact(&carrier, &handoff.ctx.blobs_dir);
        let phase_item = &authored_response.pending_handoff.items[0];
        let mut admitted_phase = ledger_row(3);
        admitted_phase.kind = MovementKind::InterfaceArrival;
        admitted_phase.from = Endpoint::Interface(provider.clone());
        admitted_phase.to = Endpoint::Interface(request_record.worker.clone());
        admitted_phase.artifact = Some(phase_ref);
        admitted_phase.invocation_metadata = Some(
            InvocationMetadata::new(Default::default())
                .with_runtime(octopus_interfaces::RuntimeInvocationMetadata {
                    run: request_record.id.clone(),
                    ee: request_record.ee.clone(),
                    worker: request_record.worker.clone(),
                    interface: provider.clone(),
                    checkpoint: phase_item.checkpoint.id.clone(),
                    epoch: phase_item.checkpoint.epoch,
                    run_record_hash: Some(authored_response.after_hash.clone()),
                })
                .into(),
        );
        log.push_str(&format!(
            "{}\n",
            serde_json::to_string(&admitted_phase).unwrap()
        ));
        std::fs::write(&handoff.ctx.movements_path, &log).unwrap();
        let resumed = ProviderHandoffSession::new(&handoff.ctx)
            .unwrap()
            .from_invocation(
                &handoff.ctx,
                &descriptor,
                &request_record.id,
                &queued,
                2,
                authored_request.content.as_bytes(),
            )
            .unwrap();
        assert_eq!(resumed.config, response_config);
        let state = resumed.state.try_lock().unwrap();
        assert!(state.session_context.artifacts.iter().any(|entry| {
            entry
                .payload
                .get("phase")
                .and_then(serde_json::Value::as_str)
                == Some("awaiting_resume")
        }));
        assert!(state.context_artifacts.contains(&queued.artifact.id));
        assert_eq!(
            queued.runtime().unwrap().run_record_hash.as_deref(),
            Some(pinned_hash.to_hex().as_str())
        );
    }

    #[test]
    fn bind_time_separates_context_free_control_from_conversation_inputs() {
        let provider = InterfaceId::new("world_provider_ollama");
        let run = RunId::new("run_profile_test");
        let profile = run_authoring_profile(&local_profile_descriptor()).unwrap();
        let mut value = profile_run_value(&provider, &run);
        value["checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": "turn_control_input",
                "run": run,
                "trace": format!("trace:{}:turn_control_input", run.as_str()),
                "epoch": 0,
                "interface_binding": names_binding(
                    "world_operator_chat_profile_test",
                    &["operator.turn.control"],
                ),
                "outbound_interface_bindings": [all_binding(provider.as_str())],
                "artifact_bindings": [],
                "required_verified_artifacts": []
            }));
        value["active_checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("turn_control_input"));
        let record = RunRecord::from_bytes(&serde_json::to_vec(&value).unwrap()).unwrap();

        let topology = derive_bound_run_authoring_topology(
            &record,
            &provider,
            &CheckpointId::new("stable_input"),
        )
        .unwrap();
        let stable = topology
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.interface().as_str() == "world_input_profile_test")
            .unwrap();
        assert_eq!(stable.role, RunAuthoringCheckpointRole::Stable);
        assert!(stable.include_context);
        let control = topology
            .checkpoints
            .iter()
            .find(|checkpoint| {
                checkpoint.interface().as_str() == "world_operator_chat_profile_test"
            })
            .unwrap();
        assert_eq!(control.role, RunAuthoringCheckpointRole::IndependentInput);
        assert!(!control.include_context);
        topology.config(&profile).validate(&provider, &run).unwrap();
    }

    #[test]
    fn explicit_topology_keeps_role_bindings_out_of_context_semantics() {
        let provider = InterfaceId::new("world_provider_ollama");
        let run = RunId::new("run_profile_test");
        let profile = run_authoring_profile(&local_profile_descriptor()).unwrap();
        let mut value = profile_run_value(&provider, &run);
        for checkpoint in value["checkpoints"].as_array_mut().unwrap() {
            checkpoint["artifact_bindings"] = serde_json::json!([{"id": "artifact-role"}]);
        }
        value["checkpoints"][0]["artifact_bindings"] = serde_json::json!([
            {"id": "artifact-role"},
            {"id": "artifact-conversation"}
        ]);
        let record = RunRecord::from_bytes(&serde_json::to_vec(&value).unwrap()).unwrap();
        let (config, _) = resolve_profile_invocation(
            &profile,
            &provider,
            &run,
            &record,
            Some(&CheckpointId::new("stable_input")),
        )
        .unwrap();
        assert!(
            config
                .checkpoint_for_role(RunAuthoringCheckpointRole::Stable)
                .unwrap()
                .include_context
        );
        assert!(
            !config
                .checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringCall)
                .unwrap()
                .include_context
        );

        value["checkpoints"][4]["artifact_bindings"] = serde_json::json!([]);
        let drifted = RunRecord::from_bytes(&serde_json::to_vec(&value).unwrap()).unwrap();
        let error = resolve_profile_invocation(
            &profile,
            &provider,
            &run,
            &drifted,
            Some(&CheckpointId::new("stable_input")),
        )
        .unwrap_err();
        assert!(error.contains("different mandatory role bindings"));
    }

    #[test]
    fn profile_topology_rejects_structural_ambiguity() {
        let provider = InterfaceId::new("world_provider_ollama");
        let run = RunId::new("run_profile_test");
        let profile = run_authoring_profile(&local_profile_descriptor()).unwrap();
        let mut duplicate_client = profile_run_value(&provider, &run);
        duplicate_client["checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": "client_output_duplicate",
                "run": run,
                "trace": format!("trace:{}:client_output_duplicate", run.as_str()),
                "epoch": 0,
                "interface_binding": all_binding(provider.as_str()),
                "outbound_interface_bindings": [all_binding("world_client_profile_duplicate")],
                "artifact_bindings": [],
                "required_verified_artifacts": []
            }));
        duplicate_client["active_checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("client_output_duplicate"));
        duplicate_client["interface_state"][provider.as_str()][FRONTIER_CHECKPOINT_IDS]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("client_output_duplicate"));
        let record =
            RunRecord::from_bytes(&serde_json::to_vec(&duplicate_client).unwrap()).unwrap();
        let topology = derive_bound_run_authoring_topology(
            &record,
            &provider,
            &CheckpointId::new("stable_input"),
        )
        .unwrap();
        let error = topology
            .config(&profile)
            .validate(&provider, &run)
            .unwrap_err();
        assert!(error.contains("overlapping outbound artifact policies"));

        let mut duplicate_call = profile_run_value(&provider, &run);
        duplicate_call["checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": "authoring_call_duplicate",
                "run": run,
                "trace": format!("trace:{}:authoring_call_duplicate", run.as_str()),
                "epoch": 0,
                "interface_binding": all_binding("system_interface_authoring_profile_duplicate"),
                "outbound_interface_bindings": [all_binding("system_context_authoring_profile_test")],
                "artifact_bindings": [],
                "required_verified_artifacts": []
            }));
        duplicate_call["active_checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("authoring_call_duplicate"));
        duplicate_call["interface_state"][provider.as_str()][FRONTIER_CHECKPOINT_IDS]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("authoring_call_duplicate"));
        let record = RunRecord::from_bytes(&serde_json::to_vec(&duplicate_call).unwrap()).unwrap();
        let error = derive_bound_run_authoring_topology(
            &record,
            &provider,
            &CheckpointId::new("stable_input"),
        )
        .unwrap_err();
        assert!(error.contains("2 structural authoring-call routes"));

        let mut duplicate_tool = profile_run_value(&provider, &run);
        duplicate_tool["checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": "tool_output_duplicate",
                "run": run,
                "trace": format!("trace:{}:tool_output_duplicate", run.as_str()),
                "epoch": 0,
                "interface_binding": all_binding(provider.as_str()),
                "outbound_interface_bindings": [all_binding("system_tool_profile_duplicate")],
                "artifact_bindings": [],
                "required_verified_artifacts": [{
                    "mark": "tool.call.allowed",
                    "artifact_label": "tool.call",
                    "verifier": "tool_call_allowed",
                    "params": {"tools": ["coder.exec"]}
                }]
            }));
        duplicate_tool["active_checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("tool_output_duplicate"));
        duplicate_tool["interface_state"][provider.as_str()][FRONTIER_CHECKPOINT_IDS]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("tool_output_duplicate"));
        let record = RunRecord::from_bytes(&serde_json::to_vec(&duplicate_tool).unwrap()).unwrap();
        let error = resolve_profile_invocation(
            &profile,
            &provider,
            &run,
            &record,
            Some(&CheckpointId::new("stable_input")),
        )
        .unwrap_err();
        assert!(error.contains("unclassified checkpoint topology"));

        let mut preseeded = profile_run_value(&provider, &run);
        let initial = RunRecord::from_bytes(&serde_json::to_vec(&preseeded).unwrap()).unwrap();
        let topology = derive_bound_run_authoring_topology(
            &initial,
            &provider,
            &CheckpointId::new("stable_input"),
        )
        .unwrap();
        preseeded["interface_state"][provider.as_str()][RUN_AUTHORING_BINDING] =
            serde_json::to_value(topology).unwrap();
        let record = RunRecord::from_bytes(&serde_json::to_vec(&preseeded).unwrap()).unwrap();
        let (resolved, persisted) = resolve_profile_invocation(
            &profile,
            &provider,
            &run,
            &record,
            Some(&CheckpointId::new("stable_input")),
        )
        .unwrap();
        assert_eq!(resolved.checkpoints, persisted.checkpoints);

        let mut control_collision = profile_run_value(&provider, &run);
        control_collision["checkpoints"][0]["interface_binding"] =
            serde_json::to_value(all_binding("system_context_authoring_profile_test")).unwrap();
        let record =
            RunRecord::from_bytes(&serde_json::to_vec(&control_collision).unwrap()).unwrap();
        let error = resolve_profile_invocation(
            &profile,
            &provider,
            &run,
            &record,
            Some(&CheckpointId::new("stable_input")),
        )
        .unwrap_err();
        assert!(
            error.contains("shares a provider-owned authoring interface outside the frontier"),
            "{error}"
        );
    }

    #[test]
    fn tool_route_requires_exactly_one_descriptor_owner_inside_grant() {
        let config = config_with_tool_routes(&[("coder", &["coder.exec"])]);
        config
            .validate(
                &InterfaceId::new("world_provider_test"),
                &RunId::new("run_test"),
            )
            .unwrap();
        let granted = BTreeSet::from(["coder.exec".to_string(), "file.write".to_string()]);
        let owner = config
            .output_checkpoint(
                &ProviderHandoffRoute::Tool {
                    tool: "coder.exec".to_string(),
                },
                &granted,
                &BTreeSet::new(),
                "tool.call",
            )
            .unwrap();
        assert_eq!(owner.name, "coder");

        let no_owner = config
            .output_checkpoint(
                &ProviderHandoffRoute::Tool {
                    tool: "file.write".to_string(),
                },
                &granted,
                &BTreeSet::new(),
                "tool.call",
            )
            .err()
            .unwrap();
        assert!(no_owner.contains("no descriptor-derived output owner"));

        let duplicate =
            config_with_tool_routes(&[("coder_a", &["coder.exec"]), ("coder_b", &["coder.exec"])]);
        let duplicate_error = duplicate
            .validate(
                &InterfaceId::new("world_provider_test"),
                &RunId::new("run_test"),
            )
            .unwrap_err();
        assert!(duplicate_error.contains("owned by both 'coder_a' and 'coder_b'"));
        let duplicate_owner = duplicate
            .output_checkpoint(
                &ProviderHandoffRoute::Tool {
                    tool: "coder.exec".to_string(),
                },
                &granted,
                &BTreeSet::new(),
                "tool.call",
            )
            .err()
            .unwrap();
        assert!(duplicate_owner.contains("2 descriptor-derived output owners"));

        let outside_grant = config
            .output_checkpoint(
                &ProviderHandoffRoute::Tool {
                    tool: "coder.exec".to_string(),
                },
                &BTreeSet::new(),
                &BTreeSet::new(),
                "tool.call",
            )
            .err()
            .unwrap();
        assert!(outside_grant.contains("outside the formed invocation grant"));
        let metadata = InvocationMetadata::new(octopus_interfaces::GrantProjection {
            tools: vec!["coder.exec".into(), "candidate".into()],
            interfaces: vec!["world_provider_test".into(), "system_coder".into()],
            interface_affordance_specs: ["coder.exec", "candidate"].into_iter().map(|name| octopus_interfaces::InterfaceToolAffordance {
                name:name.into(),description:name.into(),args_schema:serde_json::json!({"type":"object"}),
            }).collect(),
            ..Default::default()
        });
        let before = metadata.clone();
        assert_eq!(config.callable_tool_names(&metadata).unwrap(), BTreeSet::from(["coder.exec".into()]));
        assert_eq!(metadata, before);
        assert!(duplicate.callable_tool_names(&metadata).is_err());
        for case in ["missing", "duplicate", "malformed", "denied", "blocked", "door"] {
            let mut metadata = before.clone();
            match case {
                "missing" => { metadata.grant.interface_affordance_specs.remove(0); }
                "duplicate" => metadata.grant.interface_affordance_specs.push(metadata.grant.interface_affordance_specs[0].clone()),
                "malformed" => metadata.grant.interface_affordance_specs[0].args_schema = serde_json::json!(false),
                "denied" => metadata.grant.tools.retain(|tool| tool != "coder.exec"),
                "blocked" => metadata.grant.hard_blocked_tools.push("coder.exec".into()),
                "door" => metadata.grant.interfaces.retain(|interface| interface != "system_coder"),
                _ => unreachable!(),
            }
            assert!(config.callable_tool_names(&metadata).is_err(), "{case}");
        }
        let mut malformed = config.clone();
        malformed.checkpoints.last_mut().unwrap().required_verified_artifacts[0].params = serde_json::json!({"tools":[]});
        assert!(malformed.callable_tool_names(&before).is_err());
    }

    #[test]
    fn interface_route_requires_one_exact_descriptor_owner_inside_grant() {
        let mut config = config_with_tool_routes(&[]);
        config
            .checkpoints
            .iter_mut()
            .find(|checkpoint| {
                checkpoint.role == RunAuthoringCheckpointRole::ProviderArtifactOutput
            })
            .unwrap()
            .outbound_interface_bindings = vec![names_binding(
            "world_client_test",
            &["interface.artifact.output"],
        )];
        let interface = InterfaceId::new("world_operation_exact");
        config.checkpoints.push(RunAuthoringCheckpoint {
            name: "operation_output".to_string(),
            role: RunAuthoringCheckpointRole::ProviderArtifactOutput,
            interface_binding: all_binding("world_provider_test"),
            outbound_interface_bindings: vec![names_binding(
                interface.as_str(),
                &["operation.request"],
            )],
            include_context: false,
            static_artifact_bindings: Vec::new(),
        owner_authoring: None,
            required_verified_artifacts: Vec::new(),
        });
        config
            .validate(
                &InterfaceId::new("world_provider_test"),
                &RunId::new("run_test"),
            )
            .unwrap();
        let owner = config
            .output_checkpoint(
                &ProviderHandoffRoute::Interface {
                    interface: interface.clone(),
                },
                &BTreeSet::new(),
                &BTreeSet::from([interface.clone()]),
                "operation.request",
            )
            .unwrap();
        assert_eq!(owner.name, "operation_output");
        let refused = config
            .output_checkpoint(
                &ProviderHandoffRoute::Interface { interface },
                &BTreeSet::new(),
                &BTreeSet::new(),
                "operation.request",
            )
            .unwrap_err();
        assert!(refused.contains("outside the formed invocation grant"));
    }

    #[test]
    fn frontier_replacement_preserves_foreign_checkpoint_values() {
        let stable = serde_json::json!({
            "id": "stable_input",
            "run": "run_test",
            "trace": "trace:run_test:stable_input",
            "epoch": 7,
            "interface_binding": all_binding("world_input_test"),
            "outbound_interface_bindings": [all_binding("world_provider_test")],
            "artifact_bindings": [{ "id": "artifact_seed", "extension": [3, 2, 1] }],
            "required_verified_artifacts": [],
            "foreign_extension": { "preserve": true, "nested": { "value": 17 } }
        });
        let stable_bytes = serde_json::to_vec(&stable).unwrap();
        let mut root = serde_json::json!({
            "checkpoints": [
                stable,
                { "id": "old_authoring_call", "foreign": "old-call" },
                { "id": "old_authoring_result", "foreign": "old-result" }
            ],
            "active_checkpoints": [
                "stable_input",
                "old_authoring_call",
                "old_authoring_result"
            ],
            "foreign_root": { "untouched": [1, 4, 9] }
        })
        .as_object()
        .unwrap()
        .clone();
        let previous = BTreeSet::from([
            "old_authoring_call".to_string(),
            "old_authoring_result".to_string(),
        ]);
        let next_ids = vec![
            "new_output".to_string(),
            "new_authoring_call".to_string(),
            "new_authoring_result".to_string(),
        ];
        let next = next_ids
            .iter()
            .map(|id| serde_json::json!({ "id": id }))
            .collect();

        replace_owned_frontier(&mut root, &previous, next, &next_ids).unwrap();

        let checkpoints = root["checkpoints"].as_array().unwrap();
        assert_eq!(serde_json::to_vec(&checkpoints[0]).unwrap(), stable_bytes);
        assert_eq!(
            checkpoints
                .iter()
                .map(|checkpoint| checkpoint["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "stable_input",
                "new_output",
                "new_authoring_call",
                "new_authoring_result"
            ]
        );
        assert_eq!(
            root["active_checkpoints"],
            serde_json::json!([
                "stable_input",
                "new_output",
                "new_authoring_call",
                "new_authoring_result"
            ])
        );
        assert_eq!(
            root["foreign_root"],
            serde_json::json!({ "untouched": [1, 4, 9] })
        );
    }

    #[test]
    fn context_refresh_updates_every_provider_input_and_preserves_artifact_order() {
        let mut config = config_with_tool_routes(&[]);
        config.checkpoints.push(
            serde_json::from_value(serde_json::json!({
                "name": "tool_result_input",
                "role": "stable",
                "interface_binding": all_binding("system_context_test"),
                "outbound_interface_bindings": [all_binding("world_provider_test")],
                "include_context": true,
                "required_verified_artifacts": []
            }))
            .unwrap(),
        );
        config.checkpoints.push(
            serde_json::from_value(serde_json::json!({
                "name": "scheduled_input",
                "role": "independent_input",
                "interface_binding": all_binding("world_schedule_test"),
                "outbound_interface_bindings": [all_binding("world_provider_test")],
                "include_context": true,
                "required_verified_artifacts": []
            }))
            .unwrap(),
        );
        config
            .validate(
                &InterfaceId::new("world_provider_test"),
                &RunId::new("run_test"),
            )
            .unwrap();
        let mut root = serde_json::json!({
            "checkpoints": [
                {
                    "id": "terminal_input_old",
                    "run": "run_test",
                    "trace": "trace:run_test:terminal_input_old",
                    "epoch": 0,
                    "interface_binding": all_binding("world_input_test"),
                    "outbound_interface_bindings": [all_binding("world_provider_test")],
                    "artifact_bindings": [{"id": "artifact_old"}],
                    "required_verified_artifacts": []
                },
                {
                    "id": "tool_result_input_old",
                    "run": "run_test",
                    "trace": "trace:run_test:tool_result_input_old",
                    "epoch": 0,
                    "interface_binding": all_binding("system_context_test"),
                    "outbound_interface_bindings": [all_binding("world_provider_test")],
                    "artifact_bindings": [{"id": "artifact_old"}],
                    "required_verified_artifacts": []
                },
                {
                    "id": "foreign_wait",
                    "run": "run_test",
                    "trace": "trace:run_test:foreign_wait",
                    "epoch": 9,
                    "interface_binding": all_binding("world_schedule_test"),
                    "outbound_interface_bindings": [all_binding("world_provider_test")],
                    "artifact_bindings": [{"id": "artifact_schedule"}],
                    "required_verified_artifacts": [],
                    "foreign": {"preserve": true}
                }
            ],
            "active_checkpoints": [
                "terminal_input_old",
                "tool_result_input_old",
                "foreign_wait"
            ],
            "foreign_root": [1, 2, 3]
        })
        .as_object()
        .unwrap()
        .clone();
        let ordered_bindings = vec![
            serde_json::json!({"id": "artifact_z"}),
            serde_json::json!({"id": "artifact_a"}),
        ];
        replace_context_checkpoints(
            &mut root,
            &config,
            &RunId::new("run_test"),
            "cp_next",
            &ordered_bindings,
        )
        .unwrap();

        let checkpoints = root["checkpoints"].as_array().unwrap();
        assert_eq!(
            checkpoints[0]["foreign"],
            serde_json::json!({"preserve": true})
        );
        assert_eq!(
            checkpoints[0]["artifact_bindings"],
            serde_json::json!([{"id": "artifact_schedule"}])
        );
        assert_eq!(
            checkpoints
                .iter()
                .map(|checkpoint| checkpoint["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "foreign_wait",
                "cp_next_context_terminal_input",
                "cp_next_context_tool_result_input"
            ]
        );
        assert_eq!(
            checkpoints[1]["artifact_bindings"],
            serde_json::Value::Array(ordered_bindings.clone())
        );
        assert_eq!(
            checkpoints[2]["artifact_bindings"],
            serde_json::Value::Array(ordered_bindings)
        );
        assert_eq!(
            root["active_checkpoints"],
            serde_json::json!([
                "foreign_wait",
                "cp_next_context_terminal_input",
                "cp_next_context_tool_result_input"
            ])
        );
        assert_eq!(root["foreign_root"], serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn consumed_context_free_input_rearms_without_touching_external_input() {
        let control: RunAuthoringCheckpoint = serde_json::from_value(serde_json::json!({
            "name": "turn_control",
            "role": "independent_input",
            "interface_binding": names_binding(
                "world_operator_chat_test",
                &["operator.turn.control"],
            ),
            "outbound_interface_bindings": [all_binding("world_provider_test")],
            "include_context": false,
            "static_artifact_bindings": ["instruction-source"],
            "required_verified_artifacts": []
        }))
        .unwrap();
        let mut consumed = materialize_checkpoint(
            &control,
            "turn_control_old",
            &RunId::new("run_test"),
            &[serde_json::json!({"id": "role-context"})],
        );
        consumed["artifact_bindings"] = serde_json::json!([
            {"id": "instruction-source"},
            {
                "id": "role-context",
                "access": {"read": true, "change": false, "create": false, "delete": false}
            }
        ]);
        let mut root = serde_json::json!({
            "checkpoints": [
                consumed,
                {
                    "id": "scheduled_input",
                    "run": "run_test",
                    "trace": "trace:run_test:scheduled_input",
                    "epoch": 7,
                    "interface_binding": all_binding("world_schedule_test"),
                    "outbound_interface_bindings": [all_binding("world_provider_test")],
                    "artifact_bindings": [{"id": "schedule-context"}],
                    "required_verified_artifacts": [],
                    "foreign": {"preserve": true}
                }
            ],
            "active_checkpoints": ["turn_control_old", "scheduled_input"]
        })
        .as_object()
        .unwrap()
        .clone();

        replace_consumed_independent_input_checkpoint(
            &mut root,
            &control,
            &RunId::new("run_test"),
            "cp_next",
            &[serde_json::json!({"id": "role-context"})],
        )
        .unwrap();

        assert_eq!(
            root["active_checkpoints"],
            serde_json::json!(["scheduled_input", "cp_next_independent_input_turn_control"])
        );
        let checkpoints = root["checkpoints"].as_array().unwrap();
        assert_eq!(checkpoints[0]["id"], serde_json::json!("scheduled_input"));
        assert_eq!(
            checkpoints[0]["foreign"],
            serde_json::json!({"preserve": true})
        );
        assert_eq!(
            checkpoints[1]["id"],
            serde_json::json!("cp_next_independent_input_turn_control")
        );
        assert_eq!(
            checkpoints[1]["artifact_bindings"],
            serde_json::json!([{"id": "role-context"}, {"id": "instruction-source"}])
        );
    }

    #[tokio::test]
    async fn batch_frontier_recovers_each_output_without_provider_replay() {
        use tokio_util::sync::CancellationToken;

        let dir = tempfile::tempdir().unwrap();
        let var_dir = dir.path().join("var/provider");
        let emitted = var_dir.join("emitted");
        let first = InterfaceArtifactFile::write(
            &emitted,
            "interface.artifact.output",
            br#"{"text":"answer"}"#,
        )
        .unwrap();
        let second = InterfaceArtifactFile::write(
            &emitted,
            "interface.artifact.reasoning",
            br#"{"text":"reasoning"}"#,
        )
        .unwrap();
        let first_identity = artifact_identity(&first, &var_dir).unwrap();
        let second_identity = artifact_identity(&second, &var_dir).unwrap();
        let config = config_with_tool_routes(&[]);
        let output = config
            .checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput)
            .unwrap()
            .clone();
        let bound_topology = test_bound_topology(&config);
        let run_value = serde_json::json!({
            "id": "run_test",
            "ee": "ee_test",
            "worker": "system_worker_test",
            "trace": "trace:run_test",
            "status": "running",
            "interface_state": {
                "world_provider_test": {
                    "context_generation": 0,
                    "authoring_generation": 0,
                    "frontier_checkpoint_ids": ["output_old", "authoring_call_old", "authoring_result_old"]
                }
            },
            "checkpoints": [
                {
                    "id": "terminal_input",
                    "run": "run_test",
                    "trace": "trace:run_test:terminal_input",
                    "epoch": 0,
                    "interface_binding": all_binding("world_input_test"),
                    "outbound_interface_bindings": [all_binding("world_provider_test")],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                },
                {
                    "id": "output_old",
                    "run": "run_test",
                    "trace": "trace:run_test:output_old",
                    "epoch": 0,
                    "interface_binding": all_binding("world_provider_test"),
                    "outbound_interface_bindings": [all_binding("world_client_test")],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                },
                {
                    "id": "authoring_call_old",
                    "run": "run_test",
                    "trace": "trace:run_test:authoring_call_old",
                    "epoch": 0,
                    "interface_binding": all_binding("system_interface_authoring_test"),
                    "outbound_interface_bindings": [all_binding("system_context_authoring_test")],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                },
                {
                    "id": "authoring_result_old",
                    "run": "run_test",
                    "trace": "trace:run_test:authoring_result_old",
                    "epoch": 0,
                    "interface_binding": all_binding("system_context_authoring_test"),
                    "outbound_interface_bindings": [all_binding("world_provider_test")],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                }
            ],
            "active_checkpoints": ["terminal_input", "output_old", "authoring_call_old", "authoring_result_old"]
        });
        let run_bytes = serde_json::to_vec_pretty(&run_value).unwrap();
        let run_path = dir.path().join("runs/run_test.json");
        std::fs::create_dir_all(run_path.parent().unwrap()).unwrap();
        std::fs::write(&run_path, &run_bytes).unwrap();
        let movements_path = dir.path().join("ledger/movements.jsonl");
        std::fs::create_dir_all(movements_path.parent().unwrap()).unwrap();
        std::fs::write(&movements_path, b"").unwrap();
        let (arrivals, mut arrival_rx) = tokio::sync::mpsc::channel(2);
        let descriptor_snapshot = serde_json::from_value(serde_json::json!({
            "id": "world_provider_test",
            "interface_type": "world",
            "role": "provider",
            "direction": "bidirectional",
            "source_names": ["test"],
            "interface_driver_ref": "test_provider"
        }))
        .unwrap();
        let ctx = InterfaceLiftContext::unprivileged(
            InterfaceId::new("world_provider_test"),
            dir.path().join("registry/interface/world_provider_test"),
            Some(descriptor_snapshot),
            var_dir.clone(),
            movements_path.clone(),
            dir.path().join("ledger/blobs"),
            dir.path().join("ledger/ledger-index.sqlite"),
            Default::default(),
            arrivals,
            CancellationToken::new(),
        )
        .with_host_io_limits(InterfaceHostIoLimits {
            max_control_frame_bytes: 1_048_576,
            max_stderr_bytes: 262_144,
            max_manifest_bytes: 1_048_576,
            max_payload_bytes: 268_435_456,
        })
        .unwrap();
        let result_reader = LedgerTailReader::open_named(&ctx, "run-authoring").unwrap();
        let session = ProviderHandoffSession::new(&ctx).unwrap();
        let handoff = ProviderOutputHandoff {
            invocation_metadata: InvocationMetadata::new(Default::default()),
            ctx,
            config,
            run_path: run_path.clone(),
            hand_in_root: dir.path().join("hand-in"),
            var_dir: var_dir.clone(),
            source_movement_seq: 17,
            source_carries_context: true,
            failed_source: false,
            checked_settlement_output: None,
            context_output_labels: BTreeSet::new(),
            session_snapshot_labels: BTreeSet::new(),
            granted_tools: BTreeSet::new(),
            granted_interfaces: BTreeSet::new(),
            granted_mark_outcomes: BTreeSet::new(),
            bound_topology,
            delivery_mode: HandoffDeliveryMode::Rearm,
            session,
            invocation_session_snapshot: None,
            state: Arc::new(tokio::sync::Mutex::new(HandoffState {
            validated_withdrawal_output: None,
                context_artifacts: Vec::new(),
                static_context_artifacts: Vec::new(),
                session_context: ResolvedSessionContext {
                    provider_interface: InterfaceId::new("world_provider_test"),
                    artifacts: Vec::new(),
                },
                session_snapshot_head: None,
                run_record_bytes: run_bytes.clone(),
                run_record_evidence_seq: None,
                consumed_input: false,
                consumed_context_free_independent_input: None,
                consumed_verified_input: false,
            })),
            result_reader: Arc::new(tokio::sync::Mutex::new(result_reader)),
        };
        let prepared = vec![
            PreparedHandoff {
                identity: first_identity.clone(),
                output_checkpoint: output.clone(),
                verified_inputs: Vec::new(),
            },
            PreparedHandoff {
                identity: second_identity.clone(),
                output_checkpoint: output,
                verified_inputs: Vec::new(),
            },
        ];
        let initial = handoff
            .author_run(&run_bytes, &[], &prepared, false, None, false, true)
            .unwrap();
        let initial_record = RunRecord::from_bytes(initial.content.as_bytes()).unwrap();
        assert_eq!(initial_record.status, serde_json::json!("idle"));
        let pending = pending_handoff(&initial_record, &InterfaceId::new("world_provider_test"))
            .unwrap()
            .unwrap();
        assert_eq!(pending.items.len(), 2);
        validate_pending_items_are_active(
            &initial_record,
            &InterfaceId::new("world_provider_test"),
            &pending,
        )
        .unwrap();
        let first_movement = pending_movement(0, &pending.items[0], &first_identity.artifact);
        std::fs::write(
            &movements_path,
            format!("{}\n", serde_json::to_string(&first_movement).unwrap()),
        )
        .unwrap();
        std::fs::write(&run_path, initial.content.as_bytes()).unwrap();
        handoff.state.lock().await.run_record_bytes = initial.content.as_bytes().to_vec();
        let pending_for_arrival = pending.clone();
        let movements_for_arrival = movements_path.clone();
        let expected_label = second_identity.label.clone();
        let expected_ref = second_identity.artifact.clone();
        let arrival = tokio::spawn(async move {
            let output = arrival_rx.recv().await.unwrap();
            assert_eq!(
                output.artifact.read_manifest().unwrap().label,
                expected_label
            );
            let movement = pending_movement(1, &pending_for_arrival.items[1], &expected_ref);
            use std::io::Write as _;
            let mut ledger = std::fs::OpenOptions::new()
                .append(true)
                .open(&movements_for_arrival)
                .unwrap();
            writeln!(ledger, "{}", serde_json::to_string(&movement).unwrap()).unwrap();
            ledger.sync_all().unwrap();
            output
                .verdict_tx
                .unwrap()
                .send(DeliveryVerdict::Admitted { seq: 1 })
                .unwrap();
            tokio::time::timeout(Duration::from_millis(25), arrival_rx.recv())
                .await
                .is_err()
        });
        assert_eq!(
            handoff.recover_source().await.unwrap(),
            ProviderSourceRecovery::Completed
        );
        assert!(arrival.await.unwrap());
    }

    #[tokio::test]
    async fn terminal_pending_handoff_recovers_from_ledger_without_provider_replay() {
        use octopus_ledger::{Lamport, Seq, TraceId};
        use tokio_util::sync::CancellationToken;

        let dir = tempfile::tempdir().unwrap();
        let var_dir = dir.path().join("var/provider");
        let emitted = var_dir.join("emitted");
        let artifact = InterfaceArtifactFile::write(
            &emitted,
            "interface.artifact.output",
            br#"{"text":"already handed"}"#,
        )
        .unwrap();
        let artifact = materialize_session_snapshot(
            &artifact,
            &ResolvedSessionContext {
                provider_interface: InterfaceId::new("world_provider_test"),
                artifacts: Vec::new(),
            },
            None,
        )
        .unwrap();
        let blobs_dir = dir.path().join("ledger/blobs");
        std::fs::create_dir_all(&blobs_dir).unwrap();
        pin_artifact(&artifact, &blobs_dir);
        let identity = artifact_identity(&artifact, &var_dir).unwrap();
        let pending = PendingHandoff {
            source_movement_seq: 17,
            context_publication: None,
            source_without_context: false,
            settlement_of: None,
            dispatch_fenced: true,
            withdrawal: None,
            items: vec![PendingHandoffItem {
                artifact_id: identity.artifact.id.clone(),
                artifact_content_hash: identity.artifact.content.to_hex(),
                artifact_label: identity.label.clone(),
                manifest_relative_path: identity.manifest_relative_path.clone(),
                checkpoint: PendingHandoffCheckpoint {
                    id: CheckpointId::new("output"),
                    epoch: 0,
                    interface_binding: all_binding("world_provider_test"),
                    outbound_interface_bindings: vec![all_binding("world_client_test")],
                    artifact_bindings: Vec::new(),
                    required_verified_artifacts: Vec::new(),
                },
            }],
            terminal: true,
        };
        let config = config_with_tool_routes(&[]);
        let run_value = serde_json::json!({
            "id": "run_test",
            "ee": "ee_test",
            "worker": "system_worker_test",
            "trace": "trace:run_test",
            "status": "running",
            "interface_state": {
                "world_provider_test": {
                    "context_generation": 1,
                    "authoring_generation": 47,
                    "run_authoring_binding": test_bound_topology(&config),
                    "frontier_checkpoint_ids": ["output", "authoring_call", "authoring_result"],
                    "pending_handoff": pending
                }
            },
            "checkpoints": [
                {
                    "id": "terminal_input",
                    "run": "run_test",
                    "trace": "trace:run_test:terminal_input",
                    "epoch": 0,
                    "interface_binding": all_binding("world_input_test"),
                    "outbound_interface_bindings": [all_binding("world_provider_test")],
                    "artifact_bindings": [{"id": "context.reference:seed"}, {"id": identity.artifact.id}],
                    "required_verified_artifacts": []
                },
                {
                    "id": "output",
                    "run": "run_test",
                    "trace": "trace:run_test:output",
                    "epoch": 0,
                    "interface_binding": all_binding("world_provider_test"),
                    "outbound_interface_bindings": [all_binding("world_client_test")],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                },
                {
                    "id": "authoring_call",
                    "run": "run_test",
                    "trace": "trace:run_test:authoring_call",
                    "epoch": 0,
                    "interface_binding": all_binding("system_interface_authoring_test"),
                    "outbound_interface_bindings": [all_binding("system_context_authoring_test")],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                },
                {
                    "id": "authoring_result",
                    "run": "run_test",
                    "trace": "trace:run_test:authoring_result",
                    "epoch": 0,
                    "interface_binding": all_binding("system_context_authoring_test"),
                    "outbound_interface_bindings": [all_binding("world_provider_test")],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                }
            ],
            "active_checkpoints": ["terminal_input", "output", "authoring_call", "authoring_result"]
        });
        let run_bytes = serde_json::to_vec_pretty(&run_value).unwrap();
        let run_path = dir.path().join("runs/run_test.json");
        std::fs::create_dir_all(run_path.parent().unwrap()).unwrap();
        std::fs::write(&run_path, &run_bytes).unwrap();

        let movements_path = dir.path().join("ledger/movements.jsonl");
        std::fs::create_dir_all(movements_path.parent().unwrap()).unwrap();
        let movement = Movement {
            seq: Seq(18),
            lamport: Lamport(19),
            recorded_at_unix_ms: 19,
            trace: TraceId("trace-recovery".to_string()),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(InterfaceId::new("world_provider_test")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_test")),
            artifact: Some(identity.artifact.clone()),
            invocation_metadata: Some(
                typed_invocation_metadata(
                    &RunId::new("run_test"),
                    "output",
                    &InterfaceId::new("world_provider_test"),
                    0,
                )
                .into(),
            ),
        };
        let mut rows = (0..18).map(|seq| format!("{}\n", serde_json::to_string(&ledger_row(seq)).unwrap())).collect::<String>();
        rows.push_str(&format!("{}\n", serde_json::to_string(&movement).unwrap()));
        std::fs::write(&movements_path, rows).unwrap();

        let (arrivals, mut arrival_rx) = tokio::sync::mpsc::channel(1);
        let descriptor_snapshot = serde_json::from_value(serde_json::json!({
            "id": "world_provider_test",
            "interface_type": "world",
            "role": "provider",
            "direction": "bidirectional",
            "source_names": ["test"],
            "interface_driver_ref": "test_provider"
        }))
        .unwrap();
        let ctx = InterfaceLiftContext::unprivileged(
            InterfaceId::new("world_provider_test"),
            dir.path().join("registry/interface/world_provider_test"),
            Some(descriptor_snapshot),
            var_dir.clone(),
            movements_path.clone(),
            dir.path().join("ledger/blobs"),
            dir.path().join("ledger/ledger-index.sqlite"),
            Default::default(),
            arrivals,
            CancellationToken::new(),
        )
        .with_host_io_limits(InterfaceHostIoLimits {
            max_control_frame_bytes: 1_048_576,
            max_stderr_bytes: 262_144,
            max_manifest_bytes: 1_048_576,
            max_payload_bytes: 268_435_456,
        })
        .unwrap();
        let result_reader = LedgerTailReader::open_named(&ctx, "run-authoring").unwrap();
        let session = ProviderHandoffSession::new(&ctx).unwrap();
        let bound_topology = test_bound_topology(&config);
        let mut handoff = ProviderOutputHandoff {
            invocation_metadata: InvocationMetadata::new(Default::default()),
            ctx,
            config,
            run_path,
            hand_in_root: dir.path().join("hand-in"),
            var_dir,
            source_movement_seq: 17,
            source_carries_context: true,
            failed_source: false,
            checked_settlement_output: None,
            context_output_labels: BTreeSet::from(["interface.artifact.output".to_string()]),
            session_snapshot_labels: BTreeSet::from(["interface.artifact.output".to_string()]),
            granted_tools: BTreeSet::new(),
            granted_interfaces: BTreeSet::new(),
            granted_mark_outcomes: BTreeSet::new(),
            bound_topology,
            delivery_mode: HandoffDeliveryMode::Rearm,
            session,
            invocation_session_snapshot: None,
            state: Arc::new(tokio::sync::Mutex::new(HandoffState {
            validated_withdrawal_output: None,
                context_artifacts: vec![ArtifactId::new("context.reference:seed")],
                static_context_artifacts: vec![ArtifactId::new("context.reference:seed")],
                session_context: ResolvedSessionContext {
                    provider_interface: InterfaceId::new("world_provider_test"),
                    artifacts: Vec::new(),
                },
                session_snapshot_head: None,
                run_record_bytes: run_bytes.clone(),
                run_record_evidence_seq: None,
                consumed_input: false,
                consumed_context_free_independent_input: None,
                consumed_verified_input: false,
            })),
            result_reader: Arc::new(tokio::sync::Mutex::new(result_reader)),
        };

        assert_eq!(
            handoff.recover_source().await.unwrap(),
            ProviderSourceRecovery::Completed
        );
        let mut rearmed_value: serde_json::Value = serde_json::from_slice(&run_bytes).unwrap();
        {
            let mut diagnostic = handoff.clone();
            diagnostic.source_carries_context = false;
            diagnostic.context_output_labels.insert("turn.error".into());
            diagnostic.session_snapshot_labels.insert("turn.error".into());
            let file = InterfaceArtifactFile::write(&emitted, "turn.error", br#"{"label":"turn.error","message":"independent input failed"}"#).unwrap();
            let error = artifact_identity(&file, &handoff.var_dir).unwrap();
            let mut pending = pending.clone();
            pending.items[0].artifact_id = error.artifact.id;
            pending.items[0].artifact_content_hash = error.artifact.content.to_hex();
            pending.items[0].artifact_label = error.label;
            pending.items[0].manifest_relative_path = error.manifest_relative_path;
            let mut state = diagnostic.state.lock().await;
            let before = serde_json::to_value(&state.session_context.artifacts).unwrap();
            let record = RunRecord::from_bytes(&state.run_record_bytes).unwrap();
            diagnostic.restore_pending_context(&mut state, &record, &pending).unwrap();
            assert_eq!(serde_json::to_value(&state.session_context.artifacts).unwrap(), before);
        }
        {
            let mut owner_config = handoff.config.clone();
            let owner_input: RunAuthoringCheckpoint = serde_json::from_value(serde_json::json!({
                "name":"owner_input", "role":"owner_input", "include_context":false,
                "interface_binding":all_binding("system_owner_test"),
                "outbound_interface_bindings":[all_binding("world_provider_test")],
                "required_verified_artifacts":[],
                "owner_authoring":{"owner":"system_owner_test","source":"system_owner_test",
                    "door":"system_owner_writer_test","tool":"file.write.owner",
                    "call_label":"tool.call","result_label":"file.write.owner.result"}
            })).unwrap();
            owner_config.checkpoints.push(owner_input.clone());
            let mut pinned = run_value.clone();
            pinned["interface_state"]["world_provider_test"][RUN_AUTHORING_BINDING] =
                serde_json::to_value(test_bound_topology(&owner_config)).unwrap();
            pinned["checkpoints"].as_array_mut().unwrap().push(serde_json::json!({
                "id":"owner_input", "run":"run_test", "trace":"trace:owner-input", "epoch":3,
                "interface_binding":owner_input.interface_binding,
                "outbound_interface_bindings":owner_input.outbound_interface_bindings,
                "artifact_bindings":[],"required_verified_artifacts":[]
            }));
            pinned["active_checkpoints"].as_array_mut().unwrap().push(serde_json::json!("owner_input"));
            let pinned_bytes = serde_json::to_vec(&pinned).unwrap();
            let pinned_hash = ContentHash::of_bytes(&pinned_bytes);
            std::fs::write(blobs_dir.join(pinned_hash.to_hex()), &pinned_bytes).unwrap();
            let input = InterfaceArtifactFile::write(&emitted, "tool.result", br#"{"output":"31"}"#).unwrap();
            let source = pin_artifact(&input, &blobs_dir);
            let mut metadata = typed_invocation_metadata(&handoff.config.run, "owner_input", &handoff.config.provider_interface, 3);
            metadata.runtime.as_mut().unwrap().run_record_hash = Some(pinned_hash.to_hex());
            metadata.checkpoint_binding_policy = Some(owner_input.binding_policy_snapshot());
            metadata.outbound_interface_bindings = vec![handoff.config.provider_interface.clone()];
            let movement = Movement {
                seq:Seq(19), lamport:Lamport(20), recorded_at_unix_ms:20,
                trace:TraceId("trace:owner-input".into()), kind:MovementKind::InterfaceOutbound,
                from:Endpoint::Interface(owner_input.interface().clone()),
                to:Endpoint::Interface(handoff.config.provider_interface.clone()),
                artifact:Some(source), invocation_metadata:Some(metadata.into()),
            };
            use std::io::Write as _;
            let mut ledger = std::fs::OpenOptions::new().append(true).open(&movements_path).unwrap();
            writeln!(ledger, "{}", serde_json::to_string(&movement).unwrap()).unwrap();
            let mut wrong_epoch = movement.clone();
            wrong_epoch.seq = Seq(20);
            wrong_epoch.lamport = Lamport(21);
            let mut wrong_metadata = wrong_epoch.invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
            wrong_metadata.runtime.as_mut().unwrap().epoch = 4;
            wrong_epoch.invocation_metadata = Some(wrong_metadata.into());
            writeln!(ledger, "{}", serde_json::to_string(&wrong_epoch).unwrap()).unwrap();
            ledger.sync_all().unwrap();
            let mut later = handoff.clone();
            later.source_movement_seq = 21;
            assert!(later.source_is_context_free(19).unwrap());
            assert!(later.source_is_context_free(20).unwrap_err().contains("exact admitted checkpoint"));
            let file = InterfaceArtifactFile::write(&emitted, "interface.artifact.output", br#"{"text":"31"}"#).unwrap();
            let output = artifact_identity(&file, &handoff.var_dir).unwrap();
            let mut context_free = pending.clone();
            context_free.source_movement_seq = 19;
            context_free.items[0].artifact_id = output.artifact.id;
            context_free.items[0].artifact_content_hash = output.artifact.content.to_hex();
            context_free.items[0].manifest_relative_path = output.manifest_relative_path;
            let mut state = later.state.lock().await;
            let before = serde_json::to_value(&state.session_context.artifacts).unwrap();
            let record = RunRecord::from_bytes(&state.run_record_bytes).unwrap();
            later.restore_pending_context(&mut state, &record, &context_free).unwrap();
            assert_eq!(serde_json::to_value(&state.session_context.artifacts).unwrap(), before);
            let mut visible = record.clone();
            visible.checkpoints.iter_mut().find(|cp| cp.id.as_str() == "terminal_input").unwrap()
                .artifact_bindings.push(ArtifactBinding {
                    id:context_free.items[0].artifact_id.clone(), access:octopus_runtime::ArtifactAccess::read_only(),
                });
            assert!(later.restore_pending_context(&mut state, &visible, &context_free).is_err());
        }
        {
            let mut later = handoff.clone();
            later.source_movement_seq = pending.source_movement_seq + 1;
            later.context_output_labels.insert("turn.error".into());
            later.session_snapshot_labels.insert("turn.error".into());
            let payload = br#"{"label":"turn.error","message":"turn stopped before dispatch"}"#;
            assert!(!payload_carries_session_context(payload).unwrap());
            let file = InterfaceArtifactFile::write(&emitted, "turn.error", payload).unwrap();
            let error = artifact_identity(&file, &handoff.var_dir).unwrap();
            let mut failed = pending.clone();
            failed.items[0].artifact_id = error.artifact.id;
            failed.items[0].artifact_content_hash = error.artifact.content.to_hex();
            failed.items[0].artifact_label = error.label;
            failed.items[0].manifest_relative_path = error.manifest_relative_path;
            let mut state = later.state.lock().await;
            let before = serde_json::to_value(&state.session_context.artifacts).unwrap();
            let record = RunRecord::from_bytes(&state.run_record_bytes).unwrap();
            assert!(later.restore_pending_context(&mut state, &record, &failed).is_err());
            failed.source_without_context = true;
            assert_eq!(
                serde_json::to_value(&failed).unwrap()["source_without_context"],
                serde_json::json!(true)
            );
            later.restore_pending_context(&mut state, &record, &failed).unwrap();
            assert_eq!(serde_json::to_value(&state.session_context.artifacts).unwrap(), before);
        }
        for checkpoint in rearmed_value["checkpoints"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
        {
            let Some(id) = checkpoint["id"].as_str() else {
                continue;
            };
            if ["output", "authoring_call", "authoring_result"].contains(&id) {
                checkpoint["id"] = serde_json::json!(format!("{id}_next"));
            }
        }
        rearmed_value["interface_state"]["world_provider_test"]["frontier_checkpoint_ids"] =
            serde_json::json!([
                "output_next",
                "authoring_call_next",
                "authoring_result_next"
            ]);
        rearmed_value["active_checkpoints"] = serde_json::json!([
            "terminal_input",
            "output_next",
            "authoring_call_next",
            "authoring_result_next"
        ]);
        let rearmed_bytes = serde_json::to_vec(&rearmed_value).unwrap();
        let rearmed_record = RunRecord::from_bytes(&rearmed_bytes).unwrap();
        validate_bound_run_authoring_topology(
            &rearmed_record,
            &handoff.config,
            Some(&CheckpointId::new("terminal_input")),
        )
        .unwrap();
        let mut rearmed_handoff = handoff.clone();
        rearmed_handoff.source_movement_seq = 18;
        rearmed_handoff.state = Arc::new(tokio::sync::Mutex::new(HandoffState {
            validated_withdrawal_output: None,
            context_artifacts: vec![ArtifactId::new("context.reference:seed")],
            static_context_artifacts: vec![ArtifactId::new("context.reference:seed")],
            session_context: ResolvedSessionContext {
                provider_interface: InterfaceId::new("world_provider_test"),
                artifacts: Vec::new(),
            },
            session_snapshot_head: None,
            run_record_bytes: rearmed_bytes,
                run_record_evidence_seq: None,
            consumed_input: false,
            consumed_context_free_independent_input: None,
            consumed_verified_input: false,
        }));
        assert_eq!(
            rearmed_handoff.recover_source().await.unwrap(),
            ProviderSourceRecovery::Process
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(25), arrival_rx.recv())
                .await
                .is_err()
        );
        {
            let mut state = handoff.state.lock().await;
            let mut value: serde_json::Value =
                serde_json::from_slice(&state.run_record_bytes).unwrap();
            value["interface_state"]["world_provider_test"]["pending_handoff"]["terminal"] =
                serde_json::Value::Bool(false);
            state.run_record_bytes = serde_json::to_vec(&value).unwrap();
            state.session_context.artifacts = vec![SessionContextArtifact {
                label: identity.label.clone(),
                payload: serde_json::json!({"text": "already handed"}),
                source_artifact: identity.artifact.clone(),
                source_movement_seq: Some(18),
            }];
            state.session_snapshot_head = None;
        }
        assert_eq!(
            handoff.recover_source().await.unwrap(),
            ProviderSourceRecovery::Interrupted {
                last_artifact_label: "interface.artifact.output".to_string()
            }
        );
        assert_eq!(
            handoff.state.lock().await.session_context.artifacts[0].source_movement_seq,
            Some(18)
        );

        use std::io::Write as _;
        handoff.run_path = handoff.run_path.canonicalize().unwrap();
        handoff.ctx.root_slots.insert(
            "run_records_root".into(),
            handoff.run_path.parent().unwrap().to_path_buf(),
        );
        let control_dir = tempfile::tempdir_in("/tmp").unwrap();
        handoff.ctx.root_slots.insert(
            "app_server_control_root".into(),
            control_dir.path().to_path_buf(),
        );
        handoff
            .context_output_labels
            .insert("interface.artifact.reasoning".into());
        handoff
            .session_snapshot_labels
            .insert("interface.artifact.reasoning".into());
        handoff
            .context_output_labels
            .insert("interface.artifact.progress".into());
        for (generation, terminal) in [(48, false), (50, true)] {
            let label = if terminal {
                "interface.artifact.progress"
            } else {
                "interface.artifact.reasoning"
            };
            let next_file = InterfaceArtifactFile::write(
                &handoff.var_dir.join("emitted"),
                label,
                &serde_json::to_vec(&serde_json::json!({"text": format!("output {generation}")}))
                    .unwrap(),
            )
            .unwrap();
            let mut state = handoff.state.lock().await;
            let next_file = if terminal {
                next_file
            } else {
                materialize_session_snapshot(
                    &next_file,
                    &state.session_context,
                    state.session_snapshot_head.as_ref(),
                )
                .unwrap()
            };
            let next_identity = artifact_identity(&next_file, &handoff.var_dir).unwrap();
            let mut next_context = if terminal {
                state.context_artifacts.clone()
            } else {
                state.static_context_artifacts.clone()
            };
            next_context.push(next_identity.artifact.id.clone());
            let next = handoff
                .author_run(
                    &state.run_record_bytes,
                    &next_context,
                    &[PreparedHandoff {
                        identity: next_identity.clone(),
                        output_checkpoint: handoff
                            .config
                            .checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput)
                            .unwrap()
                            .clone(),
                        verified_inputs: Vec::new(),
                    }],
                    true,
                    None,
                    false,
                    terminal,
                )
                .unwrap();
            assert_eq!(
                next.correlation_id,
                format!("provider-run-authoring:run_test:{generation}")
            );
            let call_file = handoff.materialize_file_write_call(&next).unwrap();
            let call_bytes = std::fs::read(call_file.manifest_path()).unwrap();
            let staged_record = RunRecord::from_bytes(next.content.as_bytes()).unwrap();
            assert!(!current_context_artifacts(&staged_record, &handoff.config).unwrap().contains(&next_identity.artifact.id));
            let publication = handoff.author_context_publication(next.content.as_bytes(), &next.pending_handoff).unwrap();
            let published_record = RunRecord::from_bytes(publication.content.as_bytes()).unwrap();
            let staged_state = &staged_record.interface_state[&handoff.config.provider_interface];
            let published_state = &published_record.interface_state[&handoff.config.provider_interface];
            assert_eq!(published_state["context_generation"], staged_state["context_generation"]);
            assert_eq!(published_state["authoring_generation"].as_u64(), staged_state["authoring_generation"].as_u64().map(|generation| generation + 1));
            let publication_file = handoff.materialize_file_write_call(&publication).unwrap();
            let publication_bytes = std::fs::read(publication_file.manifest_path()).unwrap();
            let before = state.run_record_bytes.clone();
            let mut concurrent: serde_json::Value = serde_json::from_slice(&before).unwrap();
            let mut independent = concurrent["checkpoints"][0].clone();
            independent["id"] = serde_json::json!("independent_input");
            independent["interface_binding"]["interface"] = serde_json::json!("system_independent_input");
            independent["outbound_interface_bindings"] = serde_json::json!([]);
            concurrent["checkpoints"].as_array_mut().unwrap().push(independent.clone());
            concurrent["active_checkpoints"].as_array_mut().unwrap().push(serde_json::json!("independent_input"));
            concurrent["interface_state"]["system_independent_input"] = serde_json::json!({"owned_checkpoint_ids":["independent_input"]});
            let mut current_bytes = serde_json::to_vec_pretty(&concurrent).unwrap(); current_bytes.push(b'\n');
            let rebased = authoring_retry::rebase_authored_run(&before, &current_bytes, &next, &handoff.config, Some(JsonFormat::Compact)).unwrap();
            let merged: serde_json::Value = serde_json::from_str(&rebased.content).unwrap();
            assert_eq!(merged["interface_state"]["system_independent_input"], concurrent["interface_state"]["system_independent_input"]);
            assert!(merged["checkpoints"].as_array().unwrap().contains(&independent));
            assert!(merged["active_checkpoints"].as_array().unwrap().contains(&serde_json::json!("independent_input")));
            assert_eq!(rebased.pending_handoff, next.pending_handoff);
            assert_ne!(rebased.after_hash, next.after_hash);
            concurrent["interface_state"][handoff.config.provider_interface.as_str()]["context_generation"] = serde_json::json!(9999);
            assert!(authoring_retry::rebase_authored_run(&before, &serde_json::to_vec(&concurrent).unwrap(), &next, &handoff.config, Some(JsonFormat::Compact)).is_err());
            std::fs::write(&handoff.run_path, &before).unwrap();
            let mut different = next.clone();
            different.content.push(' ');
            let error = handoff.install_authored_run(&mut state, &different).await.err().unwrap();
            assert!(error.contains("different exact attempt"), "{error}");
            let mut previous = handoff.clone();
            previous.state = Arc::new(tokio::sync::Mutex::new(state.clone()));
            drop(state);
            let mut wrong_source = previous.clone();
            wrong_source.source_movement_seq += 1;
            let mut foreign_state = previous.state.lock().await.clone();
            assert!(wrong_source.install_authored_run(&mut foreign_state, &next).await.err().unwrap()
                .contains("private attempt belongs to source movement"));
            assert_eq!(std::fs::read(call_file.manifest_path()).unwrap(), call_bytes);
            assert_eq!(std::fs::read(&handoff.run_path).unwrap(), before);
            let mut unadmitted = previous.clone();
            unadmitted.ctx.movements_path =
                dir.path().join(format!("unadmitted-{generation}.jsonl"));
            std::fs::write(&unadmitted.ctx.movements_path, b"").unwrap();
            unadmitted.session = ProviderHandoffSession::new(&unadmitted.ctx).unwrap();
            let socket = octopus_app_server_interface::interface_control_socket_path_from_root(
                control_dir.path(), &handoff.config.authoring_source_interface,
            );
            std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            let journal_prefix = std::fs::read(&movements_path).unwrap();
            let call_seq = journal_prefix
                .split(|b| *b == b'\n')
                .filter(|row| !row.is_empty())
                .count() as u64;
            let result_checkpoint = handoff
                .config
                .checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)
                .unwrap()
                .clone();
            let writes_for_host = [(next.clone(), call_file, call_bytes, call_seq),
                (publication.clone(), publication_file, publication_bytes, call_seq + 3)];
            let blobs = blobs_dir.clone();
            let journal = movements_path.clone();
            let path = handoff.run_path.clone();
            let host = tokio::spawn(async move {
              for (next_for_host, call_file, call_bytes, call_seq) in writes_for_host {
                let (stream, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                let received = ws.next().await.unwrap().unwrap().into_text().unwrap();
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&received).unwrap(),
                    serde_json::from_slice::<serde_json::Value>(&call_bytes).unwrap()
                );
                let call: FileWriteCallEnvelope =
                    serde_json::from_slice(&call_file.read_payload_bytes().unwrap()).unwrap();
                let current = std::fs::read(&path).unwrap();
                assert_eq!(
                    ContentHash::of_bytes(&current).to_hex(),
                    call.arguments.expected_before_hash
                );
                let content = apply_run_json_patch(
                    &current,
                    &call.arguments.patch,
                    &call.arguments.expected_after_hash, call.arguments.json_format
                )
                .unwrap();
                assert_eq!(content, next_for_host.content);
                let call_ref = pin_artifact(&call_file, &blobs);
                let mut arrival = ledger_row(call_seq);
                arrival.kind = MovementKind::InterfaceArrival;
                arrival.from =
                    Endpoint::Interface(InterfaceId::new("system_interface_authoring_test"));
                arrival.artifact = Some(call_ref);
                let mut journal = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&journal)
                    .unwrap();
                writeln!(journal, "{}", serde_json::to_string(&arrival).unwrap()).unwrap();
                journal.sync_all().unwrap();
                std::fs::write(&path, &content).unwrap();
                let result = InterfaceArtifactFile::write(blobs.join("results"), "file.write.result", &serde_json::to_vec(&serde_json::json!({
                    "label": "file.write.result", "tool": "file.write", "ok": true,
                    "correlation_id": call.correlation_id,
                    "body": {"result": "files_written", "writes": [{"path": path, "before_hash": call.arguments.expected_before_hash,
                        "after_hash": call.arguments.expected_after_hash, "bytes": content.len()}]}
                })).unwrap()).unwrap();
                let mut result_movement = ledger_row(call_seq + 1);
                result_movement.kind = MovementKind::InterfaceOutbound;
                result_movement.from = Endpoint::Interface(result_checkpoint.interface().clone());
                result_movement.to = Endpoint::Interface(InterfaceId::new("world_provider_test"));
                result_movement.artifact = Some(pin_artifact(&result, &blobs));
                let mut metadata = typed_invocation_metadata(
                    &RunId::new("run_test"),
                    next_for_host.authoring_result_checkpoint.as_str(),
                    &InterfaceId::new("world_provider_test"),
                    0,
                );
                metadata.runtime.as_mut().unwrap().run_record_hash = Some(next_for_host.after_hash);
                metadata.outbound_interface_bindings =
                    result_checkpoint.outbound_interfaces().cloned().collect();
                metadata.checkpoint_binding_policy =
                    Some(result_checkpoint.binding_policy_snapshot());
                result_movement.invocation_metadata = Some(metadata.into());
                writeln!(
                    journal,
                    "{}",
                    serde_json::to_string(&result_movement).unwrap()
                )
                .unwrap();
                journal.sync_all().unwrap();
                ws.send(WsMessage::Text(
                    serde_json::to_string(&DeliveryVerdict::Admitted { seq: call_seq })
                        .unwrap()
                        .into(),
                ))
                .await
                .unwrap();
                let _ = ws.next().await;
              }
            });
            let deliver = async {
                let output = arrival_rx.recv().await.unwrap();
                assert_eq!(
                    pin_artifact(&output.artifact, &blobs_dir),
                    next_identity.artifact
                );
                let movement = pending_movement(
                    call_seq + 2,
                    &next.pending_handoff.items[0],
                    &next_identity.artifact,
                );
                let mut journal = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&movements_path)
                    .unwrap();
                writeln!(journal, "{}", serde_json::to_string(&movement).unwrap()).unwrap();
                journal.sync_all().unwrap();
                output
                    .verdict_tx
                    .unwrap()
                    .send(DeliveryVerdict::Admitted { seq: call_seq + 2 })
                    .unwrap();
            };
            let expected_recovery = if terminal {
                ProviderSourceRecovery::Completed
            } else {
                ProviderSourceRecovery::Interrupted {
                    last_artifact_label: next_identity.label.clone(),
                }
            };
            let recover = async {
                let recovered = handoff.recover_source_with_outcomes().await.unwrap();
                assert_eq!(
                    recovered.0, expected_recovery,
                    "persisted next generation {generation} was skipped"
                );
                Ok::<_, String>(recovered)
            };
            let (recovery, ()) = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::join!(recover, deliver)
            })
            .await
            .unwrap();
            let (recovery, outcomes) = recovery.unwrap();
            assert_eq!(
                recovery,
                if terminal {
                    ProviderSourceRecovery::Completed
                } else {
                    ProviderSourceRecovery::Interrupted {
                        last_artifact_label: next_identity.label.clone(),
                    }
                }
            );
            assert_eq!(
                outcomes,
                vec![ProviderHandoffOutcome::Admitted {
                    seq: call_seq + 2,
                    artifact: next_identity.artifact.clone()
                }]
            );
            host.await.unwrap();
            std::fs::remove_file(&socket).unwrap();
            assert!(
                std::fs::read(&movements_path)
                    .unwrap()
                    .starts_with(&journal_prefix)
            );
            let state = handoff.state.lock().await;
            assert_eq!(state.run_record_bytes, publication.content.as_bytes());
            assert_eq!(state.context_artifacts, next_context);
            assert_eq!(
                state
                    .session_context
                    .artifacts
                    .last()
                    .unwrap()
                    .source_movement_seq,
                Some(call_seq + 2)
            );
            if !terminal {
                assert_eq!(
                    state.session_snapshot_head.as_ref().unwrap().artifact,
                    next_identity.artifact
                );
            }
            drop(state);
            let settled_journal = std::fs::read(&movements_path).unwrap();
            assert_eq!(handoff.recover_source().await.unwrap(), recovery);
            assert_eq!(std::fs::read(&movements_path).unwrap(), settled_journal);
            if terminal {
                let mut next_source = handoff.clone();
                next_source.source_movement_seq = (call_seq + 5).max(handoff.source_movement_seq + 1);
                let mut early_context = handoff.state.lock().await.clone();
                let selected = ArtifactId::new("admitted.read");
                early_context.static_context_artifacts.push(selected.clone());
                early_context.context_artifacts.push(selected.clone());
                assert_eq!(early_context.session_context.artifacts.pop().unwrap().source_artifact, next_identity.artifact);
                let published_record = RunRecord::from_bytes(&early_context.run_record_bytes).unwrap();
                let mut observation_record = published_record.clone();
                observation_record.interface_state.get_mut(&handoff.config.provider_interface).unwrap()
                    .remove("pending_handoff");
                let mut recovered_after_observation = early_context.clone();
                handoff.restore_published_context(&mut recovered_after_observation, &observation_record, &published_record, &BTreeMap::new()).unwrap();
                assert_eq!(recovered_after_observation.session_context.artifacts.last().unwrap().source_artifact, next_identity.artifact);
                let recovered_input = recovered_after_observation.session_context.artifacts.last().unwrap().clone();
                let fingerprint = session_artifacts_fingerprint(&recovered_after_observation.session_context.artifacts).unwrap();
                restore_consumed_context_inputs(&mut recovered_after_observation, vec![recovered_input.clone(), recovered_input.clone()]).unwrap();
                assert_eq!(session_artifacts_fingerprint(&recovered_after_observation.session_context.artifacts).unwrap(), fingerprint);
                let mut corrupt_input = recovered_input;
                corrupt_input.source_movement_seq = Some(u64::MAX);
                assert!(restore_consumed_context_inputs(&mut recovered_after_observation, vec![corrupt_input]).is_err());
                assert!(unadmitted.restore_published_context(&mut early_context.clone(), &observation_record, &published_record, &BTreeMap::new()).is_err());
                next_source.state = Arc::new(tokio::sync::Mutex::new(early_context));
                assert_eq!(next_source.recover_source().await.unwrap(), ProviderSourceRecovery::Process);
                assert_eq!(next_source.current_session_context_artifacts().await.last().unwrap().source_artifact, next_identity.artifact);
                assert!(!next_source.current_session_context_artifacts().await.iter().any(|entry| entry.source_artifact.id == selected));
                assert_eq!(std::fs::read(&movements_path).unwrap(), settled_journal);
                let later = next_source.state.lock().await.clone();
                let next_attempt = next_source.author_run(&later.run_record_bytes, &later.context_artifacts,
                    &[PreparedHandoff { identity: next_identity.clone(), output_checkpoint: handoff.config
                        .checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap().clone(), verified_inputs: vec![] }],
                    true, None, false, true).unwrap();
                assert!(current_context_artifacts(&RunRecord::from_bytes(next_attempt.content.as_bytes()).unwrap(), &next_source.config).unwrap().contains(&selected));
                let private = next_source.materialize_file_write_call(&next_attempt).unwrap();
                let private_bytes = std::fs::read(private.manifest_path()).unwrap();
                assert_eq!(handoff.recover_source().await.unwrap(), ProviderSourceRecovery::Completed);
                assert_eq!(std::fs::read(private.manifest_path()).unwrap(), private_bytes);
                assert!(matches!(next_source.recover_authoring_attempt(&RunRecord::from_bytes(&later.run_record_bytes).unwrap(),
                    &later.run_record_bytes).unwrap(), AuthoringAttemptRecovery::InFlight(_)));
                assert_eq!(std::fs::read(&movements_path).unwrap(), settled_journal);
            }
            assert!(
                wrong_source
                    .recover_source()
                    .await
                    .err()
                    .unwrap()
                    .contains("private attempt belongs to source movement")
            );
            assert!(
                unadmitted
                    .recover_source()
                    .await
                    .err()
                    .unwrap()
                    .contains("without admitted ledger evidence")
            );
            if !terminal {
                let mut state = handoff.state.lock().await;
                state.session_context.artifacts.clear();
                state.session_snapshot_head = None;
                drop(state);
                assert_eq!(handoff.recover_source().await.unwrap(), recovery);
                assert_eq!(std::fs::read(&movements_path).unwrap(), settled_journal);
            }
        }
    }
}
