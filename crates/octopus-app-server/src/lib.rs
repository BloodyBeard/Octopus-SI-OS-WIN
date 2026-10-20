#![recursion_limit = "256"]
#![deny(clippy::print_stdout, clippy::print_stderr)]

pub use octopus_interfaces::InterfaceArtifactFile;

pub mod arrival;
pub use arrival::{
    ArrivalError, InterfaceArrivalRecord, InterfaceArtifactArrivalFromInterface,
    SystemInterfaceEmissionOutcome, recover_durable_checkpoint_outbound_tail,
};

#[cfg(feature = "owner-contract-tests")]
pub mod owner_contract_tests {
    pub async fn arrive_retained_artifact(
        owner: &mut super::InterfaceArtifactArrivalFromInterface<'_>,
        artifact: &super::InterfaceArtifactFile,
        identity: &str,
    ) -> Result<octopus_ledger::ArtifactDelivery, super::ArrivalError> {
        owner.arrive_retained_artifact(artifact, identity).await
    }
}

pub mod artifact_arrival;
pub use artifact_arrival::{
    InterfaceArtifactArrivalAdmission, InterfaceArtifactArrivalAdmissionError,
    InterfaceArtifactArrivalAdmissionOutcome, admit_interface_artifact_arrival,
    admit_retained_interface_artifact_arrival,
    admit_system_interface_emission, admit_system_interface_reference_emission,
};

mod context_artifact_cache;
mod registry_reference_graph;
pub use registry_reference_graph::{
    RegistryCandidateStage, RegistryCandidateValidation, RegistryCandidateWrite, RegistryValidationIssue,
    RegistryValidationRoots, RegistryValidationStage, registry_tool_output_sources,
    registry_worker_policy_issues, validate_registry_candidate, validate_registry_candidate_with_inspector,
};
pub mod spine_root;
pub use spine_root::{
    EmbeddedRuntime, SharedBlobHandle, SharedBlobs, SpineRootConfig, build_embedded_spine,
    context_artifact_content, project_ee_policies, registry_root_from_manifest,
};

pub mod worker_policy;
pub use worker_policy::{AuthoredBindings, WorkerPolicyError, form_worker_run_policy};
