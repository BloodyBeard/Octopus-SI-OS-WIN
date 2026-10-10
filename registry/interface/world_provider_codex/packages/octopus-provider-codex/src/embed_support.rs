#![cfg(feature = "embed-support")]

pub use octopus_app_server_transport::app_server_control_socket_path;

pub use octopus_core::otel_init::build_provider as build_otel_provider;
pub use octopus_exec::EnvironmentManager;
pub use octopus_exec::ExecServerRuntimePaths;
