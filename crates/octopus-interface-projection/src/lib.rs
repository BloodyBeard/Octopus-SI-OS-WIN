#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod interface_activation;
pub use interface_activation::{
    InterfaceActivation, InterfaceActivationFile, LoadedInterfaceActivationFile,
    load_interface_activation_file_for_interface, load_interface_activation_file_in_dir,
    load_interface_activation_files,
};

pub mod interface_arrival_payload;
pub use interface_arrival_payload::{
    InterfaceArrivalPayloadError, resolve_interface_arrival_payload,
};

pub mod registry_projection;
pub use registry_projection::{InterfaceRegistryProjectionError, RegistryProjection};

pub mod interface_affordance_projection;
pub use interface_affordance_projection::{
    InterfaceAffordanceProjectionError, InterfaceToolAffordance, InterfaceToolAffordanceList,
    InterfaceToolDescriptionView, InterfaceToolOwnerMap, admissible_interface_tools,
    project_interface_tool_affordances,
};

pub mod metadata_projection;
pub use metadata_projection::interface_metadata_projection_body;

pub mod binding_projection;
pub use binding_projection::{
    AttachedInterfaceBinding, attached_bindings_from_grant, direction_accepts_inbound,
    direction_accepts_outbound, interface_map,
};
