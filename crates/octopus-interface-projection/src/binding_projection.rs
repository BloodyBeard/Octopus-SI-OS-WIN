use std::collections::BTreeMap;

use octopus_descriptors::{DescriptorName, InterfaceId};
use octopus_interfaces::{Direction, InterfaceDescriptor};
pub use octopus_ledger::AttachedInterfaceBinding;
use octopus_policy::FormedPolicy;

pub fn direction_accepts_inbound(direction: Direction) -> bool {
    matches!(direction, Direction::Inbound | Direction::Bidirectional)
}

pub fn direction_accepts_outbound(direction: Direction) -> bool {
    matches!(direction, Direction::Outbound | Direction::Bidirectional)
}

pub fn interface_map(
    interfaces: &[(DescriptorName, InterfaceDescriptor)],
) -> BTreeMap<InterfaceId, InterfaceDescriptor> {
    interfaces
        .iter()
        .map(|(_, interface)| (interface.id.clone(), interface.clone()))
        .collect()
}

pub fn attached_bindings_from_grant(
    formed: &FormedPolicy,
    interfaces: &BTreeMap<InterfaceId, InterfaceDescriptor>,
) -> Vec<AttachedInterfaceBinding> {
    formed
        .context
        .interfaces
        .iter()
        .filter_map(|id| {
            interfaces
                .get(id)
                .map(|interface| AttachedInterfaceBinding {
                    interface: interface.id.clone(),
                    interface_type: interface.interface_type.as_label().to_string(),
                    direction: match interface.direction {
                        Direction::Inbound => "inbound",
                        Direction::Outbound => "outbound",
                        Direction::Bidirectional => "bidirectional",
                    }
                    .to_string(),
                    role: interface.role.0.clone(),
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_descriptors::{MarkId, ToolId, WorkerEnvironmentKindId};
    use octopus_policy::{CapSet, ContextGrant, DenySet, FormationPin};

    fn formed_with_interfaces(interface_ids: &[&str]) -> FormedPolicy {
        let ctx = ContextGrant {
            tools: CapSet::from_allowed(std::iter::empty::<ToolId>()),
            interfaces: CapSet::from_allowed(interface_ids.iter().map(|id| InterfaceId::new(*id))),
            wek: CapSet::from_allowed(std::iter::empty::<WorkerEnvironmentKindId>()),
            mark_outcomes: CapSet::from_allowed(std::iter::empty::<MarkId>()),
            hard_blocks_tools: DenySet::default(),
            interface_policy: Default::default(),
            ..Default::default()
        };
        FormedPolicy {
            boundary: ctx.clone(),
            context: ctx,
            pin: FormationPin::default(),
        }
    }

    fn loaded_interface(id: &str) -> InterfaceDescriptor {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "interface_type": "world",
            "role": "provider",
            "direction": "bidirectional",
            "source_names": ["probe"],
        }))
        .expect("interface fixture parses")
    }

    #[test]
    fn attached_bindings_are_the_granted_interface_folders_that_resolve() {
        let mut interfaces: BTreeMap<InterfaceId, InterfaceDescriptor> = BTreeMap::new();
        interfaces.insert(
            InterfaceId::new("world_present"),
            loaded_interface("world_present"),
        );

        let formed = formed_with_interfaces(&["world_present", "world_gone"]);
        let attached = attached_bindings_from_grant(&formed, &interfaces);
        assert_eq!(
            attached
                .iter()
                .map(|b| b.interface.as_str())
                .collect::<Vec<_>>(),
            vec!["world_present"],
            "a granted folder that resolves attaches; the missing folder's route refuses at use time"
        );
    }
}
