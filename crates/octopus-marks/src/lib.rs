use std::path::PathBuf;

use octopus_descriptors::{CheckpointId, Descriptor, MarkId, RunId, VerifierId};
use octopus_ledger::{ArtifactRef, BlobStore, ContentHash, VerifierPackagePin};
pub use octopus_ledger::{Mark, MarkClass, RequiredVerifiedArtifact};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MarkDescriptor {
    pub id: MarkId,
    pub package: String,
    pub class: MarkClass,
}

impl Descriptor for MarkDescriptor {
    const KIND: &'static str = "mark";
    const SCHEMA_VERSION: u32 = 1;
    const CONTRACT_VERSION: u32 = 2;

    fn declared_name(&self) -> Option<&str> {
        Some(self.package.as_str())
    }

    fn validate(&self) -> std::result::Result<(), String> {
        if self.id.as_str().trim().is_empty() {
            return Err("mark descriptor has empty id".to_string());
        }
        if self.package.trim().is_empty() {
            return Err(format!("mark '{}' has empty package", self.id.as_str()));
        }
        if self.package != self.id.as_str() {
            return Err(format!(
                "mark descriptor package '{}' must equal id '{}'",
                self.package,
                self.id.as_str()
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct VerificationInput {
    pub run: RunId,
    pub checkpoint: CheckpointId,
    pub epoch: u64,
    pub mark: MarkId,
    pub artifact: ArtifactRef,
    pub artifact_label: String,
    pub params: serde_json::Value,
    pub metadata: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerificationVerdict {
    pub passed: bool,
    pub details: serde_json::Value,
}

impl VerificationVerdict {
    pub fn passed(details: serde_json::Value) -> Self {
        Self {
            passed: true,
            details,
        }
    }

    pub fn failed(details: serde_json::Value) -> Self {
        Self {
            passed: false,
            details,
        }
    }
}

pub trait Verifier {
    fn verify(&self, input: &VerificationInput, blobs: &dyn BlobStore) -> VerificationVerdict;
}

#[derive(Debug, Deserialize)]
struct InterfaceArtifactManifestEvidence {
    label: String,
    payload_path: PathBuf,
    #[serde(default)]
    payload_content_hash: Option<String>,
}

pub fn verified_artifact_payload_bytes(
    input: &VerificationInput,
    blobs: &dyn BlobStore,
    handler: &str,
) -> Result<Vec<u8>, serde_json::Value> {
    let Some(blob_bytes) = blobs.get(&input.artifact.content) else {
        return Err(serde_json::json!({
            "handler": handler,
            "artifact": input.artifact.id.as_str(),
            "error": "artifact blob missing"
        }));
    };
    let Ok(raw) = std::str::from_utf8(&blob_bytes) else {
        return Ok(blob_bytes);
    };
    let Ok(manifest) = serde_json::from_str::<InterfaceArtifactManifestEvidence>(raw.trim()) else {
        return Ok(blob_bytes);
    };
    if manifest.label.trim().is_empty() || manifest.payload_path.as_os_str().is_empty() {
        return Ok(blob_bytes);
    }
    if !manifest.payload_path.is_absolute() {
        return Err(serde_json::json!({
            "handler": handler,
            "artifact": input.artifact.id.as_str(),
            "error": "artifact manifest payload_path is not absolute",
            "payload_path": manifest.payload_path.display().to_string()
        }));
    }
    let Some(expected) = manifest.payload_content_hash.as_deref() else {
        return Err(serde_json::json!({
            "handler": handler,
            "artifact": input.artifact.id.as_str(),
            "error": "artifact manifest has no pinned payload content hash",
            "payload_path": manifest.payload_path.display().to_string()
        }));
    };
    let Some(payload_hash) = ContentHash::from_hex(expected) else {
        return Err(serde_json::json!({
            "handler": handler,
            "artifact": input.artifact.id.as_str(),
            "error": "artifact manifest payload content hash is malformed",
            "payload_content_hash": expected
        }));
    };
    let payload_bytes = blobs.get(&payload_hash).ok_or_else(|| {
        serde_json::json!({
            "handler": handler,
            "artifact": input.artifact.id.as_str(),
            "error": "artifact pinned payload blob is missing",
            "payload_content_hash": expected
        })
    })?;
    let actual = ContentHash::of_bytes(&payload_bytes).to_hex();
    if actual != expected {
        return Err(serde_json::json!({
            "handler": handler,
            "artifact": input.artifact.id.as_str(),
            "error": "artifact pinned payload blob failed integrity",
            "expected": expected,
            "actual": actual
        }));
    }
    Ok(payload_bytes)
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VerifiedArtifactError {
    #[error("required verified artifact names mark '{mark}' which did not resolve")]
    MarkUnresolved { mark: String },
    #[error("required verified artifact '{0}' did not match artifact label '{1}'")]
    ArtifactLabelMismatch(String, String),
    #[error(
        "run '{run}' checkpoint '{checkpoint}' has required_verified_artifacts, but arriving artifact label '{artifact_label}' does not match any requirement"
    )]
    NoRequirementForLabel {
        run: String,
        checkpoint: String,
        artifact_label: String,
    },
    #[error(
        "required verified artifact '{mark}' names verifier '{verifier}' which did not resolve"
    )]
    VerifierUnresolved { mark: String, verifier: String },
    #[error("required verified artifact '{mark}' verifier '{verifier}' is unavailable: {details}")]
    VerifierUnavailable {
        mark: String,
        verifier: String,
        verifier_package: Option<VerifierPackagePin>,
        details: serde_json::Value,
    },
    #[error(
        "required verified artifact '{mark}' verifier '{verifier}' rejected artifact '{artifact}': {details}"
    )]
    VerifierRejected {
        mark: String,
        verifier: String,
        artifact: String,
        verifier_package: Option<VerifierPackagePin>,
        details: serde_json::Value,
    },
}

impl VerifiedArtifactError {
    pub fn mark_id(&self) -> Option<&str> {
        match self {
            Self::MarkUnresolved { mark }
            | Self::VerifierUnresolved { mark, .. }
            | Self::VerifierUnavailable { mark, .. }
            | Self::VerifierRejected { mark, .. } => Some(mark),
            Self::ArtifactLabelMismatch(mark, _) => Some(mark),
            Self::NoRequirementForLabel { .. } => None,
        }
    }

    pub fn failure_metadata(&self) -> octopus_ledger::VerificationFailureMetadata {
        let mut metadata = octopus_ledger::VerificationFailureMetadata {
            error: match self {
                Self::VerifierRejected { .. } => "verifier_rejected",
                Self::VerifierUnavailable { .. } => "verifier_unavailable",
                Self::VerifierUnresolved { .. } => "verifier_unresolved",
                Self::MarkUnresolved { .. } => "mark_unresolved",
                Self::ArtifactLabelMismatch(_, _) => "artifact_label_mismatch",
                Self::NoRequirementForLabel { .. } => "no_requirement_for_label",
            }
            .to_string(),
            verifier: None,
            mark: self.mark_id().map(MarkId::from),
            artifact_label: None,
            run: None,
            checkpoint: None,
            verifier_package: None,
            details: None,
        };
        match self {
            Self::VerifierRejected {
                verifier,
                verifier_package,
                details,
                ..
            }
            | Self::VerifierUnavailable {
                verifier,
                verifier_package,
                details,
                ..
            } => {
                metadata.verifier = Some(VerifierId::from(verifier.as_str()));
                metadata.verifier_package = verifier_package.clone();
                metadata.details = Some(details.clone());
            }
            Self::VerifierUnresolved { verifier, .. } => {
                metadata.verifier = Some(VerifierId::from(verifier.as_str()));
            }
            Self::ArtifactLabelMismatch(_, label) => {
                metadata.artifact_label = Some(label.clone());
            }
            Self::NoRequirementForLabel {
                run,
                checkpoint,
                artifact_label,
            } => {
                metadata.run = Some(RunId::from(run.as_str()));
                metadata.checkpoint = Some(CheckpointId::from(checkpoint.as_str()));
                metadata.artifact_label = Some(artifact_label.clone());
            }
            Self::MarkUnresolved { .. } => {}
        }
        metadata
    }
}

pub trait PreparedVerification: Send {
    fn run(self: Box<Self>) -> Result<Mark, VerifiedArtifactError>;
}

pub struct ReadyVerification(Result<Mark, VerifiedArtifactError>);

impl ReadyVerification {
    pub fn passed(mark: Mark) -> Self {
        Self(Ok(mark))
    }

    pub fn rejected(error: VerifiedArtifactError) -> Self {
        Self(Err(error))
    }
}

impl PreparedVerification for ReadyVerification {
    fn run(self: Box<Self>) -> Result<Mark, VerifiedArtifactError> {
        self.0
    }
}

pub trait VerifiedArtifactAdmission {
    fn prepare_required_verified_artifact(
        &self,
        required: &RequiredVerifiedArtifact,
        input: VerificationInput,
    ) -> Result<Box<dyn PreparedVerification>, VerifiedArtifactError>;

    fn admit_required_verified_artifact(
        &self,
        required: &RequiredVerifiedArtifact,
        input: VerificationInput,
    ) -> Result<Mark, VerifiedArtifactError> {
        self.prepare_required_verified_artifact(required, input)?
            .run()
    }
}

pub struct FailClosedVerifiedArtifactAdmission;

impl VerifiedArtifactAdmission for FailClosedVerifiedArtifactAdmission {
    fn prepare_required_verified_artifact(
        &self,
        required: &RequiredVerifiedArtifact,
        _input: VerificationInput,
    ) -> Result<Box<dyn PreparedVerification>, VerifiedArtifactError> {
        Err(VerifiedArtifactError::VerifierUnresolved {
            mark: required.mark.as_str().to_string(),
            verifier: required.verifier.as_str().to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_ledger::InMemoryBlobs;

    fn verification_input(artifact: ArtifactRef) -> VerificationInput {
        VerificationInput {
            run: RunId::new("run_test"),
            checkpoint: CheckpointId::new("checkpoint_test"),
            epoch: 0,
            mark: MarkId::new("test.mark"),
            artifact,
            artifact_label: "test.payload".to_string(),
            params: serde_json::json!({}),
            metadata: serde_json::json!({}),
        }
    }

    #[test]
    fn verifier_reads_the_pinned_payload_blob_instead_of_the_carrier_path() {
        let mut blobs = InMemoryBlobs::new();
        let payload = b"immutable verifier input";
        let payload_hash = blobs.put(payload);
        // Absolute on this platform, so only the missing file is at issue.
        let payload_path = if cfg!(windows) {
            r"C:\source\no-longer-exists.json"
        } else {
            "/source/no-longer-exists.json"
        };
        let manifest = serde_json::to_vec(&serde_json::json!({
            "label": "test.payload",
            "payload_path": payload_path,
            "payload_content_hash": payload_hash.to_hex()
        }))
        .unwrap();
        let manifest_hash = blobs.put(&manifest);
        let input = verification_input(ArtifactRef {
            id: octopus_descriptors::ArtifactId::new(format!(
                "test.payload:{}",
                manifest_hash.to_hex()
            )),
            content: manifest_hash,
        });

        assert_eq!(
            verified_artifact_payload_bytes(&input, &blobs, "test").unwrap(),
            payload
        );
    }

    #[test]
    fn verifier_fails_closed_when_the_manifest_has_no_pinned_payload() {
        let mut blobs = InMemoryBlobs::new();
        // An absolute payload path on this platform, so the manifest fails only
        // for its missing pinned hash.
        let payload_path = if cfg!(windows) {
            r"C:\source\still-not-authority.json"
        } else {
            "/source/still-not-authority.json"
        };
        let manifest = serde_json::to_vec(&serde_json::json!({
            "label": "test.payload",
            "payload_path": payload_path
        }))
        .unwrap();
        let manifest_hash = blobs.put(&manifest);
        let input = verification_input(ArtifactRef {
            id: octopus_descriptors::ArtifactId::new(format!(
                "test.payload:{}",
                manifest_hash.to_hex()
            )),
            content: manifest_hash,
        });

        let error = verified_artifact_payload_bytes(&input, &blobs, "test").unwrap_err();
        assert_eq!(
            error.get("error").and_then(serde_json::Value::as_str),
            Some("artifact manifest has no pinned payload content hash")
        );
    }
}
