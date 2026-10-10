use std::collections::{BTreeMap, HashMap};
use std::ops::ControlFlow;
use std::time::{SystemTime, UNIX_EPOCH};

use octopus_descriptors::{
    ArtifactId, CheckpointId, DescriptorHash, EeId, InterfaceId, MarkId, RunId, ToolId, VerifierId,
};
use octopus_policy::{
    ArtifactAccess, ArtifactVerb, AuthoredInterfaceBinding, FormedPolicy, InterfacePolicyValues,
};
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LedgerError {
    #[error("malformed durable movement row at line {line}: {source}")]
    MalformedRow {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("malformed durable movement metadata at line {line}: {message}")]
    MalformedMovementMetadata { line: usize, message: String },
    #[error("durable movement row at line {line} has an empty trace id")]
    MissingTrace { line: usize },
    #[error("blob payload missing for content hash {0}")]
    BlobMissing(ContentHash),
    #[error("blob at content hash {expected} failed integrity on read: its bytes hash to {actual}")]
    BlobIntegrity {
        expected: ContentHash,
        actual: ContentHash,
    },
    #[error(
        "exact mark proof is ambiguous between pending InterfaceArrival rows at seq {first:?} and {second:?}"
    )]
    AmbiguousInterfaceArrivalMarkProof { first: Seq, second: Seq },
    #[error("durable ledger io error at {path}: {message}")]
    Io { path: String, message: String },
}

pub type LedgerResult<T> = std::result::Result<T, LedgerError>;

pub type ContentHash = DescriptorHash;

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
pub struct Seq(pub u64);

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
pub struct Lamport(pub u64);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct TraceId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ArtifactRef {
    pub id: ArtifactId,
    pub content: ContentHash,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum Endpoint {
    Interface(InterfaceId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum MovementKind {
    ArtifactMoved,
    MarkOutcomeAdmitted(MarkId),
    VerificationFailed,
    DeliveryRefused,
    InterfaceArrival,
    InterfaceOutbound,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RefusedReason {
    NoBinding,
    Policy,
    VerificationFailed,
    UnknownInterface,
    UnknownRun,
    StaleEpoch,
    CheckpointSpent,
    Settled,
    Malformed,
}

impl RefusedReason {
    pub fn as_str(self) -> &'static str {
        match self {
            RefusedReason::NoBinding => "no_binding",
            RefusedReason::Policy => "policy",
            RefusedReason::VerificationFailed => "verification_failed",
            RefusedReason::UnknownInterface => "unknown_interface",
            RefusedReason::UnknownRun => "unknown_run",
            RefusedReason::StaleEpoch => "stale_epoch",
            RefusedReason::CheckpointSpent => "checkpoint_spent",
            RefusedReason::Settled => "settled",
            RefusedReason::Malformed => "malformed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryVerdict {
    Admitted {
        seq: u64,
    },
    Refused {
        reason: RefusedReason,
        detail: serde_json::Value,
    },
}

impl DeliveryVerdict {
    pub fn refused(reason: RefusedReason, detail: serde_json::Value) -> Self {
        DeliveryVerdict::Refused { reason, detail }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeliveryMetadata {
    pub delivery_verdict: DeliveryVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeInvocationMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_movement_seq: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MarkClass {
    Success,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Mark {
    pub name: MarkId,
    pub subject: CheckpointId,
    pub run: RunId,
    pub run_epoch: u64,
    pub class: MarkClass,
    pub evidence: Option<ArtifactRef>,
    pub verifier: VerifierId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier_package: Option<VerifierPackagePin>,
    #[serde(default)]
    pub details: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerifierPackagePin {
    pub descriptor_content: ContentHash,
    pub script_content: ContentHash,
}

impl Mark {
    pub fn passed(
        mark: MarkId,
        checkpoint: CheckpointId,
        run: RunId,
        epoch: u64,
        artifact: ArtifactRef,
        verifier: VerifierId,
        verifier_package: VerifierPackagePin,
        details: serde_json::Value,
    ) -> Self {
        Self::passed_with_class(
            mark,
            checkpoint,
            run,
            epoch,
            MarkClass::Success,
            artifact,
            verifier,
            verifier_package,
            details,
        )
    }

    pub fn passed_with_class(
        mark: MarkId,
        checkpoint: CheckpointId,
        run: RunId,
        epoch: u64,
        class: MarkClass,
        artifact: ArtifactRef,
        verifier: VerifierId,
        verifier_package: VerifierPackagePin,
        details: serde_json::Value,
    ) -> Self {
        Self {
            name: mark,
            subject: checkpoint,
            run,
            run_epoch: epoch,
            class,
            evidence: Some(artifact),
            verifier,
            verifier_package: Some(verifier_package),
            details,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequiredVerifiedArtifact {
    pub mark: MarkId,
    pub artifact_label: String,
    pub verifier: VerifierId,
    #[serde(default)]
    pub params: serde_json::Value,
}

impl RequiredVerifiedArtifact {
    pub fn validate(&self, owner: &str) -> Result<(), String> {
        if self.mark.as_str().trim().is_empty() {
            return Err(format!(
                "{owner} declares a required_verified_artifact with an empty mark id"
            ));
        }
        if self.artifact_label.trim().is_empty() {
            return Err(format!(
                "{owner} required verified artifact '{}' has an empty artifact_label",
                self.mark.as_str()
            ));
        }
        if self.verifier.as_str().trim().is_empty() {
            return Err(format!(
                "{owner} required verified artifact '{}' has an empty verifier id",
                self.mark.as_str()
            ));
        }
        if !self.params.is_object() && !self.params.is_null() {
            return Err(format!(
                "{owner} required verified artifact '{}' params must be a JSON object",
                self.mark.as_str()
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InterfaceToolAffordance {
    pub name: ToolId,
    pub description: String,
    #[serde(alias = "input_schema")]
    pub args_schema: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttachedInterfaceBinding {
    pub interface: InterfaceId,
    pub interface_type: String,
    pub direction: String,
    pub role: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuntimeInvocationMetadata {
    pub run: RunId,
    pub ee: EeId,
    pub worker: InterfaceId,
    pub checkpoint: CheckpointId,
    pub interface: InterfaceId,
    pub epoch: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_record_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckpointBindingPolicySnapshot {
    pub inbound: AuthoredInterfaceBinding,
    #[serde(default)]
    pub outbound: Vec<AuthoredInterfaceBinding>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckpointAdmissionMetadata {
    pub artifact_label: String,
    #[serde(default)]
    pub required_artifact_labels: Vec<String>,
    pub complete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerificationFailureMetadata {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier: Option<VerifierId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<MarkId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<CheckpointId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier_package: Option<VerifierPackagePin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GrantProjection {
    #[serde(default)]
    pub formation_pin: octopus_policy::FormationPin,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub hard_blocked_tools: Vec<String>,
    #[serde(default)]
    pub interfaces: Vec<String>,
    #[serde(default)]
    pub worker_environment_kinds: Vec<String>,
    #[serde(default)]
    pub mark_outcomes: Vec<String>,
    #[serde(default)]
    pub interface_policy: BTreeMap<String, InterfacePolicyValues>,
    #[serde(default)]
    pub artifact_verbs: Vec<String>,
    #[serde(default)]
    pub artifact_access: BTreeMap<String, ArtifactAccess>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ee_root_slot_allowance: Option<Vec<String>>,
    #[serde(default)]
    pub role_artifacts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    #[serde(default)]
    pub interface_affordance_names: Vec<String>,
    #[serde(default)]
    pub interface_affordance_specs: Vec<InterfaceToolAffordance>,
    #[serde(default)]
    pub attached_interface_bindings: Vec<AttachedInterfaceBinding>,
}

impl GrantProjection {
    pub fn from_formed(formed: &FormedPolicy) -> Self {
        Self {
            formation_pin: formed.pin.clone(),
            tools: cap_ids(formed.context.tools.iter().map(|id| id.as_str())),
            hard_blocked_tools: cap_ids(
                formed
                    .context
                    .hard_blocks_tools
                    .iter()
                    .map(|id| id.as_str()),
            ),
            interfaces: cap_ids(formed.context.interfaces.iter().map(|id| id.as_str())),
            worker_environment_kinds: cap_ids(formed.context.wek.iter().map(|id| id.as_str())),
            mark_outcomes: cap_ids(formed.context.mark_outcomes.iter().map(|id| id.as_str())),
            interface_policy: formed
                .context
                .interface_policy
                .iter()
                .map(|(interface, values)| (interface.as_str().to_string(), values.clone()))
                .collect(),
            artifact_verbs: cap_ids(
                formed
                    .context
                    .artifact_verbs
                    .iter()
                    .map(|verb| verb.as_str()),
            ),
            artifact_access: formed
                .context
                .artifact_access
                .iter()
                .map(|(id, access)| (id.as_str().to_string(), *access))
                .collect(),
            ..Self::default()
        }
    }

    pub fn with_goal(mut self, goal: Option<String>) -> Self {
        self.goal = goal
            .map(|goal| goal.trim().to_string())
            .filter(|goal| !goal.is_empty());
        self
    }

    pub fn with_role_artifacts(mut self, role_artifacts: Vec<String>) -> Self {
        self.role_artifacts = role_artifacts;
        self
    }

    pub fn with_interface_affordances(
        mut self,
        names: Vec<String>,
        specs: Vec<InterfaceToolAffordance>,
    ) -> Self {
        self.interface_affordance_names = names;
        self.interface_affordance_specs = specs;
        self
    }

    pub fn with_attached_interface_bindings(
        mut self,
        attached: Vec<AttachedInterfaceBinding>,
    ) -> Self {
        self.attached_interface_bindings = attached;
        self
    }

    pub fn with_ee_root_slot_allowance(mut self, allowance: Option<Vec<String>>) -> Self {
        self.ee_root_slot_allowance = allowance;
        self
    }

    pub fn ee_root_slot_allowance_set(&self) -> Option<std::collections::BTreeSet<String>> {
        self.ee_root_slot_allowance
            .as_ref()
            .map(|slots| slots.iter().cloned().collect())
    }

    pub fn may_read_artifact(&self, id: &str) -> bool {
        self.artifact_access
            .get(id)
            .is_some_and(|access| access.read)
    }

    pub fn may_create_artifact(&self) -> bool {
        self.artifact_verbs
            .iter()
            .any(|verb| verb == ArtifactVerb::Create.as_str())
    }
}

fn cap_ids<'a>(ids: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = ids
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect();
    out.sort();
    out.dedup();
    out
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InvocationMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_movement_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeInvocationMetadata>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outbound_interface_bindings: Vec<InterfaceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_binding_policy: Option<CheckpointBindingPolicySnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_verified_artifacts: Vec<RequiredVerifiedArtifact>,
    #[serde(default)]
    pub grant: GrantProjection,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_binding_refs: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_admission: Option<CheckpointAdmissionMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_failure: Option<VerificationFailureMetadata>,
}

impl InvocationMetadata {
    pub fn new(grant: GrantProjection) -> Self {
        Self {
            source_movement_seq: None,
            runtime: None,
            outbound_interface_bindings: Vec::new(),
            checkpoint_binding_policy: None,
            required_verified_artifacts: Vec::new(),
            grant,
            artifact_binding_refs: Vec::new(),
            checkpoint_admission: None,
            verification_failure: None,
        }
    }

    pub fn with_runtime(mut self, runtime: RuntimeInvocationMetadata) -> Self {
        self.runtime = Some(runtime);
        self
    }

    pub fn with_outbound_interface_bindings(mut self, bindings: Vec<InterfaceId>) -> Self {
        self.outbound_interface_bindings = bindings;
        self
    }

    pub fn with_checkpoint_binding_policy(
        mut self,
        policy: CheckpointBindingPolicySnapshot,
    ) -> Self {
        self.checkpoint_binding_policy = Some(policy);
        self
    }

    pub fn with_required_verified_artifacts(
        mut self,
        artifacts: Vec<RequiredVerifiedArtifact>,
    ) -> Self {
        self.required_verified_artifacts = artifacts;
        self
    }

    pub fn with_artifact_binding_refs(mut self, refs: Vec<ArtifactRef>) -> Self {
        self.artifact_binding_refs = refs;
        self
    }

    pub fn with_checkpoint_admission(mut self, admission: CheckpointAdmissionMetadata) -> Self {
        self.checkpoint_admission = Some(admission);
        self
    }

    pub fn with_verification_failure(mut self, failure: VerificationFailureMetadata) -> Self {
        self.verification_failure = Some(failure);
        self
    }

    pub fn grant(&self) -> &GrantProjection {
        &self.grant
    }

    pub fn runtime(&self) -> Option<&RuntimeInvocationMetadata> {
        self.runtime.as_ref()
    }

    pub fn goal(&self) -> Option<&str> {
        self.grant.goal.as_deref()
    }

    pub fn role_artifacts(&self) -> &[String] {
        &self.grant.role_artifacts
    }

    pub fn worker_environment_kinds(&self) -> &[String] {
        &self.grant.worker_environment_kinds
    }

    pub fn interface_policy(&self) -> Option<&InterfacePolicyValues> {
        let interface = self.runtime.as_ref()?.interface.as_str();
        self.grant.interface_policy.get(interface)
    }

    pub fn allowed_interfaces(&self) -> &[String] {
        &self.grant.interfaces
    }

    pub fn interface_affordance_specs(&self) -> &[InterfaceToolAffordance] {
        &self.grant.interface_affordance_specs
    }

    pub fn granted_tool_names(&self) -> &[String] {
        &self.grant.tools
    }

    pub fn hard_blocked_tool_names(&self) -> &[String] {
        &self.grant.hard_blocked_tools
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum MovementMetadata {
    Delivery(DeliveryMetadata),
    MarkOutcome(Mark),
    Invocation(InvocationMetadata),
}

impl MovementMetadata {
    pub fn invocation(&self) -> Option<&InvocationMetadata> {
        match self {
            Self::Invocation(metadata) => Some(metadata),
            _ => None,
        }
    }

    pub fn delivery_verdict(&self) -> Option<&DeliveryVerdict> {
        match self {
            Self::Delivery(metadata) => Some(&metadata.delivery_verdict),
            _ => None,
        }
    }

    pub fn mark_outcome(&self) -> Option<&Mark> {
        match self {
            Self::MarkOutcome(mark) => Some(mark),
            _ => None,
        }
    }
}

impl Movement {
    pub fn runtime(&self) -> Option<&RuntimeInvocationMetadata> {
        match self.invocation_metadata.as_ref()? {
            MovementMetadata::Invocation(metadata) => metadata.runtime.as_ref(),
            MovementMetadata::Delivery(metadata) => metadata.runtime.as_ref(),
            MovementMetadata::MarkOutcome(_) => None,
        }
    }

    pub fn run(&self) -> Option<&RunId> {
        match self.invocation_metadata.as_ref()? {
            MovementMetadata::Invocation(metadata) => metadata.runtime.as_ref().map(|r| &r.run),
            MovementMetadata::Delivery(metadata) => metadata.runtime.as_ref().map(|r| &r.run),
            MovementMetadata::MarkOutcome(mark) => Some(&mark.run),
        }
    }
}

impl From<InvocationMetadata> for MovementMetadata {
    fn from(value: InvocationMetadata) -> Self {
        Self::Invocation(value)
    }
}

impl From<DeliveryVerdict> for MovementMetadata {
    fn from(delivery_verdict: DeliveryVerdict) -> Self {
        Self::Delivery(DeliveryMetadata {
            delivery_verdict,
            runtime: None,
            source_movement_seq: None,
        })
    }
}

impl From<Mark> for MovementMetadata {
    fn from(value: Mark) -> Self {
        Self::MarkOutcome(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Movement {
    pub seq: Seq,
    pub lamport: Lamport,
    pub recorded_at_unix_ms: u64,
    pub trace: TraceId,
    pub kind: MovementKind,
    pub from: Endpoint,
    pub to: Endpoint,
    pub artifact: Option<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_metadata: Option<MovementMetadata>,
}

const DURABLE_INVOCATION_GRANT_CONTENT: &str = "grant_content";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableMovementRow {
    seq: Seq,
    lamport: Lamport,
    recorded_at_unix_ms: u64,
    trace: TraceId,
    kind: MovementKind,
    from: Endpoint,
    to: Endpoint,
    artifact: Option<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    invocation_metadata: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DurableMovementRoute {
    pub seq: Seq,
    pub kind: MovementKind,
    pub from: Endpoint,
    pub to: Endpoint,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableMovementRouteRow {
    seq: Seq,
    lamport: Lamport,
    recorded_at_unix_ms: u64,
    trace: TraceId,
    kind: MovementKind,
    from: Endpoint,
    to: Endpoint,
    artifact: Option<ArtifactRef>,
    #[serde(default)]
    invocation_metadata: Option<BTreeMap<String, IgnoredAny>>,
}

/// Decodes the durable row envelope and routing coordinates without hydrating a
/// content-addressed invocation grant. Consumers must fully decode every row
/// selected by these coordinates before using its artifact or metadata.
pub fn decode_durable_movement_route(
    row: &[u8],
    line: usize,
) -> LedgerResult<DurableMovementRoute> {
    let row = serde_json::from_slice::<DurableMovementRouteRow>(row)
        .map_err(|source| LedgerError::MalformedRow { line, source })?;
    if row.trace.0.trim().is_empty() {
        return Err(LedgerError::MissingTrace { line });
    }
    let _ = (
        row.lamport,
        row.recorded_at_unix_ms,
        row.artifact,
        row.invocation_metadata,
    );
    Ok(DurableMovementRoute {
        seq: row.seq,
        kind: row.kind,
        from: row.from,
        to: row.to,
    })
}

pub fn encode_durable_movement_row(
    movement: &Movement,
    line: usize,
    mut pin_grant: impl FnMut(&[u8]) -> LedgerResult<ContentHash>,
) -> LedgerResult<String> {
    let invocation_metadata = movement
        .invocation_metadata
        .as_ref()
        .map(|metadata| {
            let mut value = serde_json::to_value(metadata).map_err(|error| {
                LedgerError::MalformedMovementMetadata {
                    line,
                    message: error.to_string(),
                }
            })?;
            if let MovementMetadata::Invocation(invocation) = metadata {
                let object = value.as_object_mut().ok_or_else(|| {
                    LedgerError::MalformedMovementMetadata {
                        line,
                        message: "invocation metadata did not encode as an object".to_string(),
                    }
                })?;
                let removed = object.remove("grant").ok_or_else(|| {
                    LedgerError::MalformedMovementMetadata {
                        line,
                        message: "invocation metadata encoded without its grant".to_string(),
                    }
                })?;
                let expected = serde_json::to_value(&invocation.grant).map_err(|error| {
                    LedgerError::MalformedMovementMetadata {
                        line,
                        message: error.to_string(),
                    }
                })?;
                if removed != expected {
                    return Err(LedgerError::MalformedMovementMetadata {
                        line,
                        message: "invocation metadata grant encoding drifted".to_string(),
                    });
                }
                let bytes = serde_json::to_vec(&invocation.grant).map_err(|error| {
                    LedgerError::MalformedMovementMetadata {
                        line,
                        message: error.to_string(),
                    }
                })?;
                object.insert(
                    DURABLE_INVOCATION_GRANT_CONTENT.to_string(),
                    serde_json::Value::String(pin_grant(&bytes)?.to_hex()),
                );
            }
            Ok(value)
        })
        .transpose()?;
    serde_json::to_string(&DurableMovementRow {
        seq: movement.seq,
        lamport: movement.lamport,
        recorded_at_unix_ms: movement.recorded_at_unix_ms,
        trace: movement.trace.clone(),
        kind: movement.kind.clone(),
        from: movement.from.clone(),
        to: movement.to.clone(),
        artifact: movement.artifact.clone(),
        invocation_metadata,
    })
    .map_err(|source| LedgerError::MalformedRow { line, source })
}

pub fn decode_durable_movement_row(
    row: &[u8],
    line: usize,
    mut resolve_grant: impl FnMut(ContentHash) -> LedgerResult<serde_json::Value>,
) -> LedgerResult<Movement> {
    let row = serde_json::from_slice::<DurableMovementRow>(row)
        .map_err(|source| LedgerError::MalformedRow { line, source })?;
    if row.trace.0.trim().is_empty() {
        return Err(LedgerError::MissingTrace { line });
    }
    let invocation_metadata = row
        .invocation_metadata
        .map(|mut value| {
            let object =
                value
                    .as_object_mut()
                    .ok_or_else(|| LedgerError::MalformedMovementMetadata {
                        line,
                        message: "movement metadata is not an object".to_string(),
                    })?;
            let inline_grant = object.contains_key("grant");
            let pinned_grant = object.remove(DURABLE_INVOCATION_GRANT_CONTENT);
            let has_pinned_grant = pinned_grant.is_some();
            if inline_grant && pinned_grant.is_some() {
                return Err(LedgerError::MalformedMovementMetadata {
                    line,
                    message: "invocation metadata carries both inline and content-addressed grant"
                        .to_string(),
                });
            }
            if let Some(content) = pinned_grant {
                let raw =
                    content
                        .as_str()
                        .ok_or_else(|| LedgerError::MalformedMovementMetadata {
                            line,
                            message: "invocation grant content address is not a string".to_string(),
                        })?;
                let hash = ContentHash::from_hex(raw).ok_or_else(|| {
                    LedgerError::MalformedMovementMetadata {
                        line,
                        message: "invocation grant content address is not canonical BLAKE3"
                            .to_string(),
                    }
                })?;
                if hash.to_hex() != raw {
                    return Err(LedgerError::MalformedMovementMetadata {
                        line,
                        message: "invocation grant content address is not canonical BLAKE3"
                            .to_string(),
                    });
                }
                object.insert("grant".to_string(), resolve_grant(hash)?);
            }
            let metadata = serde_json::from_value::<MovementMetadata>(value).map_err(|error| {
                LedgerError::MalformedMovementMetadata {
                    line,
                    message: error.to_string(),
                }
            })?;
            if matches!(metadata, MovementMetadata::Invocation(_))
                && !inline_grant
                && !has_pinned_grant
            {
                return Err(LedgerError::MalformedMovementMetadata {
                    line,
                    message: "invocation metadata has no grant carrier".to_string(),
                });
            }
            Ok(metadata)
        })
        .transpose()?;
    Ok(Movement {
        seq: row.seq,
        lamport: row.lamport,
        recorded_at_unix_ms: row.recorded_at_unix_ms,
        trace: row.trace,
        kind: row.kind,
        from: row.from,
        to: row.to,
        artifact: row.artifact,
        invocation_metadata,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct InterfaceArrivalMarkProofKey {
    from: InterfaceId,
    to: InterfaceId,
    trace: TraceId,
    run: RunId,
    checkpoint: CheckpointId,
    epoch: u64,
    mark: MarkId,
    verifier: VerifierId,
    artifact_id: ArtifactId,
    artifact_content: ContentHash,
}

pub fn interface_arrival_mark_proof_key(
    arrival: &Movement,
) -> Option<InterfaceArrivalMarkProofKey> {
    if arrival.kind != MovementKind::InterfaceArrival {
        return None;
    }
    let invocation = arrival
        .invocation_metadata
        .as_ref()
        .and_then(MovementMetadata::invocation)?;
    let runtime = invocation.runtime.as_ref()?;
    let admission = invocation.checkpoint_admission.as_ref()?;
    let required = invocation
        .required_verified_artifacts
        .iter()
        .find(|required| required.artifact_label == admission.artifact_label)?;
    let artifact = arrival.artifact.as_ref()?;
    let Endpoint::Interface(from) = &arrival.from;
    let Endpoint::Interface(to) = &arrival.to;
    (from == &runtime.interface && to == &runtime.worker).then(|| InterfaceArrivalMarkProofKey {
        from: from.clone(),
        to: to.clone(),
        trace: arrival.trace.clone(),
        run: runtime.run.clone(),
        checkpoint: runtime.checkpoint.clone(),
        epoch: runtime.epoch,
        mark: required.mark.clone(),
        verifier: required.verifier.clone(),
        artifact_id: artifact.id.clone(),
        artifact_content: artifact.content,
    })
}

pub fn admitted_mark_outcome_proof_key(
    mark_movement: &Movement,
) -> Option<InterfaceArrivalMarkProofKey> {
    let MovementKind::MarkOutcomeAdmitted(kind_mark) = &mark_movement.kind else {
        return None;
    };
    let mark = mark_movement
        .invocation_metadata
        .as_ref()
        .and_then(MovementMetadata::mark_outcome)?;
    let artifact = mark_movement.artifact.as_ref()?;
    let evidence = mark.evidence.as_ref()?;
    let Endpoint::Interface(from) = &mark_movement.from;
    let Endpoint::Interface(to) = &mark_movement.to;
    (kind_mark == &mark.name && mark.verifier_package.is_some() && evidence == artifact).then(
        || InterfaceArrivalMarkProofKey {
            from: from.clone(),
            to: to.clone(),
            trace: mark_movement.trace.clone(),
            run: mark.run.clone(),
            checkpoint: mark.subject.clone(),
            epoch: mark.run_epoch,
            mark: mark.name.clone(),
            verifier: mark.verifier.clone(),
            artifact_id: artifact.id.clone(),
            artifact_content: artifact.content,
        },
    )
}

#[derive(Default)]
pub struct InterfaceArrivalMarkProofIndex {
    proofs: HashMap<(Seq, InterfaceArrivalMarkProofKey), Lamport>,
}

impl InterfaceArrivalMarkProofIndex {
    pub fn from_movements<'a>(
        movements: impl IntoIterator<Item = &'a Movement>,
    ) -> LedgerResult<Self> {
        let mut pending: HashMap<InterfaceArrivalMarkProofKey, &Movement> = HashMap::new();
        let mut index = Self::default();
        for movement in movements {
            if let Some(key) = interface_arrival_mark_proof_key(movement) {
                if let Some(prior) = pending.insert(key, movement) {
                    return Err(LedgerError::AmbiguousInterfaceArrivalMarkProof {
                        first: prior.seq,
                        second: movement.seq,
                    });
                }
            }
            let Some(key) = admitted_mark_outcome_proof_key(movement) else {
                continue;
            };
            let Some(arrival) = pending.remove(&key) else {
                continue;
            };
            if mark_outcome_proves_interface_arrival(arrival, movement) {
                index.proofs.insert((arrival.seq, key), movement.lamport);
            } else {
                pending.insert(key, arrival);
            }
        }
        Ok(index)
    }

    pub fn proof_lamport(&self, arrival: &Movement) -> Option<Lamport> {
        let key = interface_arrival_mark_proof_key(arrival)?;
        self.proofs.get(&(arrival.seq, key)).copied()
    }
}

pub fn mark_outcome_proves_interface_arrival(arrival: &Movement, mark_movement: &Movement) -> bool {
    if arrival.kind != MovementKind::InterfaceArrival
        || mark_movement.seq <= arrival.seq
        || mark_movement.lamport <= arrival.lamport
    {
        return false;
    }
    matches!(
        (
            interface_arrival_mark_proof_key(arrival),
            admitted_mark_outcome_proof_key(mark_movement),
        ),
        (Some(arrival), Some(mark)) if arrival == mark
    )
}

pub fn movement_recorded_at_unix_ms_now() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before UNIX_EPOCH; cannot record movement timing")
        .as_millis();
    u64::try_from(millis).expect("movement timestamp overflowed u64 milliseconds")
}

#[derive(Clone, Debug)]
pub struct MovementDraft {
    pub trace: TraceId,
    pub kind: MovementKind,
    pub from: Endpoint,
    pub to: Endpoint,
    pub artifact: Option<ArtifactRef>,
    pub invocation_metadata: Option<MovementMetadata>,
    pub causal_floor: Option<Lamport>,
}

impl MovementDraft {
    pub fn validate(&self) -> Result<(), LedgerError> {
        if self.trace.0.trim().is_empty() {
            return Err(LedgerError::MissingTrace { line: 0 });
        }
        Ok(())
    }
}

pub trait MovementLog {
    fn append(&mut self, draft: MovementDraft) -> Movement;
    fn read_from(&self, from: Seq) -> Vec<Movement>;
    fn read_at(&self, seq: Seq) -> Option<Movement> {
        let mut found = None;
        self.scan_from(seq, &mut |movement| {
            if movement.seq == seq {
                found = Some(movement);
            }
            ControlFlow::Break(())
        });
        found
    }
    fn len(&self) -> usize;
    fn durable_end_offset(&self) -> Option<u64> {
        None
    }
    fn durable_row_byte_range(&self, _seq: Seq) -> Option<(u64, u64)> {
        None
    }
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn scan_from(&self, from: Seq, visit: &mut dyn FnMut(Movement) -> ControlFlow<()>) {
        for movement in self.read_from(from) {
            if visit(movement).is_break() {
                return;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Cursor(pub Seq);

impl Cursor {
    pub const START: Cursor = Cursor(Seq(0));

    pub fn seq(self) -> Seq {
        self.0
    }
}

impl Default for Cursor {
    fn default() -> Self {
        Cursor::START
    }
}

pub trait MovementCursor {
    fn read_from_cursor(&self, cursor: Cursor) -> (Vec<Movement>, Cursor);
}

impl<L: MovementLog + ?Sized> MovementCursor for L {
    fn read_from_cursor(&self, cursor: Cursor) -> (Vec<Movement>, Cursor) {
        let tail = self.read_from(cursor.seq());
        let next = tail
            .last()
            .map(|m| Cursor(Seq(m.seq.0 + 1)))
            .unwrap_or(cursor);
        (tail, next)
    }
}

#[derive(Default)]
pub struct InMemoryLog {
    rows: Vec<Movement>,
    clock: u64,
}

impl InMemoryLog {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MovementLog for InMemoryLog {
    fn append(&mut self, draft: MovementDraft) -> Movement {
        draft
            .validate()
            .expect("in-memory ledger movement trace must be non-empty");
        let floor = draft.causal_floor.map(|l| l.0).unwrap_or(0);
        self.clock = self.clock.max(floor) + 1;
        let m = Movement {
            seq: Seq(self.rows.len() as u64),
            lamport: Lamport(self.clock),
            recorded_at_unix_ms: movement_recorded_at_unix_ms_now(),
            trace: draft.trace,
            kind: draft.kind,
            from: draft.from,
            to: draft.to,
            artifact: draft.artifact,
            invocation_metadata: draft.invocation_metadata,
        };
        self.rows.push(m.clone());
        m
    }

    fn read_from(&self, from: Seq) -> Vec<Movement> {
        usize::try_from(from.0)
            .ok()
            .and_then(|start| self.rows.get(start..))
            .unwrap_or_default()
            .to_vec()
    }

    fn read_at(&self, seq: Seq) -> Option<Movement> {
        usize::try_from(seq.0)
            .ok()
            .and_then(|index| self.rows.get(index))
            .cloned()
    }

    fn scan_from(&self, from: Seq, visit: &mut dyn FnMut(Movement) -> ControlFlow<()>) {
        let Some(rows) = usize::try_from(from.0)
            .ok()
            .and_then(|start| self.rows.get(start..))
        else {
            return;
        };
        for movement in rows {
            if visit(movement.clone()).is_break() {
                return;
            }
        }
    }

    fn len(&self) -> usize {
        self.rows.len()
    }
}

pub struct LiveMovementLog<L: MovementLog> {
    inner: L,
    head: tokio::sync::watch::Sender<Seq>,
}

#[derive(Clone)]
pub struct LogTail {
    head: tokio::sync::watch::Receiver<Seq>,
}

impl<L: MovementLog> LiveMovementLog<L> {
    pub fn new(inner: L) -> Self {
        let head = Seq(inner.len() as u64);
        let (tx, _rx) = tokio::sync::watch::channel(head);
        Self { inner, head: tx }
    }

    pub fn tail(&self) -> LogTail {
        LogTail {
            head: self.head.subscribe(),
        }
    }

    pub fn log(&self) -> &L {
        &self.inner
    }
}

impl<L: MovementLog> MovementLog for LiveMovementLog<L> {
    fn append(&mut self, draft: MovementDraft) -> Movement {
        let m = self.inner.append(draft);
        let _ = self.head.send(Seq(m.seq.0 + 1));
        m
    }

    fn read_from(&self, from: Seq) -> Vec<Movement> {
        self.inner.read_from(from)
    }

    fn read_at(&self, seq: Seq) -> Option<Movement> {
        self.inner.read_at(seq)
    }

    fn scan_from(&self, from: Seq, visit: &mut dyn FnMut(Movement) -> ControlFlow<()>) {
        self.inner.scan_from(from, visit)
    }

    fn len(&self) -> usize {
        self.inner.len()
    }

    fn durable_end_offset(&self) -> Option<u64> {
        self.inner.durable_end_offset()
    }

    fn durable_row_byte_range(&self, seq: Seq) -> Option<(u64, u64)> {
        self.inner.durable_row_byte_range(seq)
    }

    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

impl LogTail {
    pub async fn wait_for(&mut self, cursor: Cursor) {
        if *self.head.borrow() > cursor.seq() {
            return;
        }
        loop {
            if self.head.changed().await.is_err() {
                return;
            }
            if *self.head.borrow() > cursor.seq() {
                return;
            }
        }
    }
}

pub trait BlobStore {
    fn put(&mut self, bytes: &[u8]) -> ContentHash;
    fn put_batch(&mut self, contents: &[&[u8]]) -> Vec<ContentHash> {
        contents.iter().map(|bytes| self.put(bytes)).collect()
    }
    fn get(&self, hash: &ContentHash) -> Option<Vec<u8>>;
    fn contains(&self, hash: &ContentHash) -> bool;
}

impl<T: BlobStore + ?Sized> BlobStore for &mut T {
    fn put(&mut self, bytes: &[u8]) -> ContentHash {
        (**self).put(bytes)
    }
    fn put_batch(&mut self, contents: &[&[u8]]) -> Vec<ContentHash> {
        (**self).put_batch(contents)
    }
    fn get(&self, hash: &ContentHash) -> Option<Vec<u8>> {
        (**self).get(hash)
    }
    fn contains(&self, hash: &ContentHash) -> bool {
        (**self).contains(hash)
    }
}

pub type SharedBlobs = std::sync::Arc<std::sync::Mutex<dyn BlobStore + Send + Sync>>;

#[derive(Default)]
pub struct InMemoryBlobs {
    map: BTreeMap<ContentHash, Vec<u8>>,
}

impl InMemoryBlobs {
    pub fn new() -> Self {
        Self::default()
    }
}

impl BlobStore for InMemoryBlobs {
    fn put(&mut self, bytes: &[u8]) -> ContentHash {
        let h = ContentHash::of_bytes(bytes);
        self.map.entry(h).or_insert_with(|| bytes.to_vec());
        h
    }
    fn get(&self, hash: &ContentHash) -> Option<Vec<u8>> {
        self.map.get(hash).cloned()
    }
    fn contains(&self, hash: &ContentHash) -> bool {
        self.map.contains_key(hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_route_decode_skips_the_heavy_invocation_body_but_keeps_outer_integrity() {
        let row = serde_json::to_vec(&serde_json::json!({
            "seq": 7,
            "lamport": 8,
            "recorded_at_unix_ms": 9,
            "trace": "trace-route",
            "kind": "InterfaceOutbound",
            "from": { "Interface": "world_provider" },
            "to": { "Interface": "system_room" },
            "artifact": null,
            "invocation_metadata": {
                "grant_content": "not-decoded-by-the-route-probe",
                "arbitrarily_large_projection": [1, 2, 3]
            }
        }))
        .unwrap();

        assert_eq!(
            decode_durable_movement_route(&row, 7).unwrap(),
            DurableMovementRoute {
                seq: Seq(7),
                kind: MovementKind::InterfaceOutbound,
                from: Endpoint::Interface(InterfaceId::new("world_provider")),
                to: Endpoint::Interface(InterfaceId::new("system_room")),
            }
        );

        let empty_trace = String::from_utf8(row)
            .unwrap()
            .replace("trace-route", "   ");
        assert!(matches!(
            decode_durable_movement_route(empty_trace.as_bytes(), 7),
            Err(LedgerError::MissingTrace { line: 7 })
        ));
    }

    #[test]
    fn log_is_append_only_lamport_causal_and_blobs_dedup() {
        let mut blobs = InMemoryBlobs::new();
        let h1 = blobs.put(b"hello");
        let h2 = blobs.put(b"hello");
        assert_eq!(h1, h2, "blobs dedup by content");
        assert!(blobs.contains(&h1));
        assert_eq!(blobs.get(&h1).unwrap(), b"hello");

        let mut log = InMemoryLog::new();
        let m0 = log.append(MovementDraft {
            trace: TraceId("t".into()),
            kind: MovementKind::ArtifactMoved,
            from: Endpoint::Interface(InterfaceId::new("r1")),
            to: Endpoint::Interface(InterfaceId::new("r2")),
            artifact: Some(ArtifactRef {
                id: ArtifactId::new("a1"),
                content: h1,
            }),
            invocation_metadata: None,
            causal_floor: None,
        });
        assert_eq!(m0.seq, Seq(0));
        assert!(
            m0.recorded_at_unix_ms > 0,
            "movement rows carry wall-clock timing"
        );

        let m1 = log.append(MovementDraft {
            trace: TraceId("t".into()),
            kind: MovementKind::ArtifactMoved,
            from: Endpoint::Interface(InterfaceId::new("r2")),
            to: Endpoint::Interface(InterfaceId::new("r2")),
            artifact: None,
            invocation_metadata: None,
            causal_floor: Some(m0.lamport),
        });
        assert_eq!(m1.seq, Seq(1));
        assert!(m1.lamport > m0.lamport, "lamport respects causal floor");
        assert!(
            m1.recorded_at_unix_ms >= m0.recorded_at_unix_ms,
            "movement timings do not move backwards inside the append sequence"
        );

        assert_eq!(log.read_from(Seq(1)).len(), 1);
        assert_eq!(log.len(), 2);
    }

    fn moved(from: &str, to: &str, floor: Option<Lamport>) -> MovementDraft {
        MovementDraft {
            trace: TraceId("c".into()),
            kind: MovementKind::ArtifactMoved,
            from: Endpoint::Interface(InterfaceId::new(from)),
            to: Endpoint::Interface(InterfaceId::new(to)),
            artifact: None,
            invocation_metadata: None,
            causal_floor: floor,
        }
    }

    #[test]
    fn cursor_reads_the_complete_tail_and_advances_past_all_scanned_rows() {
        let mut log = InMemoryLog::new();
        log.append(moved("a", "x", None));
        log.append(moved("b", "y", None));
        log.append(moved("z", "a", None));

        let (rows, next) = log.read_from_cursor(Cursor::START);
        assert_eq!(
            rows.len(),
            3,
            "the cursor does not impose a routing projection"
        );
        assert_eq!(rows[0].seq, Seq(0));
        assert_eq!(rows[1].seq, Seq(1));
        assert_eq!(rows[2].seq, Seq(2));
        assert_eq!(
            next,
            Cursor(Seq(3)),
            "resume past the LAST scanned row, not the last matched"
        );

        let (again, again_next) = log.read_from_cursor(Cursor::START);
        assert_eq!(again, rows);
        assert_eq!(again_next, next);

        let (empty, held) = log.read_from_cursor(next);
        assert!(empty.is_empty());
        assert_eq!(held, next, "an empty tail does not move the cursor");
    }

    #[tokio::test]
    async fn live_tail_wakes_on_append_without_busy_wait() {
        let mut live = LiveMovementLog::new(InMemoryLog::new());
        let mut tail = live.tail();

        let (rows, cursor) = live.log().read_from_cursor(Cursor::START);
        assert!(rows.is_empty());
        assert_eq!(cursor, Cursor::START);

        live.append(moved("a", "x", None));
        tail.wait_for(Cursor::START).await;
        let (rows, cursor) = live.log().read_from_cursor(Cursor::START);
        assert_eq!(rows.len(), 1);
        assert_eq!(cursor, Cursor(Seq(1)));

        live.append(moved("a", "y", None));
        tail.wait_for(cursor).await;
        let (rows, cursor) = live.log().read_from_cursor(cursor);
        assert_eq!(
            rows.len(),
            1,
            "exactly the one new row, from the durable cursor"
        );
        assert_eq!(cursor, Cursor(Seq(2)));

        drop(live);
        tail.wait_for(Cursor(Seq(99))).await;
    }
}
