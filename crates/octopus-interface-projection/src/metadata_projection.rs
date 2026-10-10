use octopus_interfaces::GrantProjection;
use octopus_policy::FormedPolicy;

use crate::{AttachedInterfaceBinding, InterfaceToolAffordanceList};

pub fn interface_metadata_projection_body(
    goal: Option<String>,
    role_artifacts: Vec<String>,
    interface_affordances: &InterfaceToolAffordanceList,
    formed: &FormedPolicy,
    attached_interface_bindings: Vec<AttachedInterfaceBinding>,
) -> Result<GrantProjection, String> {
    let interface_affordance_names = interface_affordances.sorted_names();
    let interface_affordance_specs = interface_affordances.to_metadata()?;

    Ok(GrantProjection::from_formed(formed)
        .with_goal(goal)
        .with_role_artifacts(role_artifacts)
        .with_interface_affordances(interface_affordance_names, interface_affordance_specs)
        .with_attached_interface_bindings(attached_interface_bindings))
}
