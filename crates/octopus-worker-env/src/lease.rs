use std::collections::BTreeSet;
use std::sync::Arc;

use crate::{
    EnvContext, WorkerEnvCaps, WorkerEnvDriver, WorkerEnvError, WorkerEnvKindSpec,
    WorkerEnvironment, WorkerEnvironmentFormation,
};

/// Open a worker environment for a run. Root caps are FORMED, not seeded from the
/// kind boundary alone (notes2 2026-07-11 #3): `WorkerEnvCaps::form` intersects the
/// worker_environment_kind boundary defaults with the host root-slots the door
/// instance exposes (derived from `ctx`) and with the run's EE-chain root
/// allowance. The door and the EE chain are the two legs a bare
/// `from_kind_boundary` seed lacks.
#[allow(clippy::too_many_arguments)]
pub async fn lease(
    spec: &WorkerEnvKindSpec,
    formation: &WorkerEnvironmentFormation,
    driver: Arc<dyn WorkerEnvDriver>,
    ctx: EnvContext,
    ee_root_allowance: Option<&BTreeSet<String>>,
) -> Result<WorkerEnvironment, WorkerEnvError> {
    formation.validate_against_kind(spec)?;
    let hash = formation.descriptor_hash()?;
    let door_root_bindings = ctx.provided_root_bindings();
    formation.attest_door_root_bindings(&door_root_bindings)?;
    let caps = WorkerEnvCaps::form(&spec, &door_root_bindings, ee_root_allowance);
    if caps != formation.caps() {
        return Err(WorkerEnvError::OpenFailed {
            substrate_family: spec.substrate_family.clone(),
            reason: "worker-environment caps differ from the door-formed record".to_string(),
        });
    }
    if driver.substrate_family() != spec.substrate_family {
        return Err(WorkerEnvError::UnknownSubstrateFamily(
            spec.substrate_family.clone(),
        ));
    }
    driver
        .open(
            &spec,
            hash,
            caps,
            ctx,
            formation.worker_environment_id.clone(),
        )
        .await
}
