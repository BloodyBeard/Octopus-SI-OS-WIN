#![deny(clippy::print_stdout, clippy::print_stderr)]

#[cfg(test)]
extern crate self as core_test_support;

pub mod config;
mod config_lock;
pub mod exec_env;
mod exec_policy;
pub use exec_policy::ExecPolicyError;
pub use exec_policy::check_execpolicy_for_warnings;
pub use exec_policy::format_exec_policy_error_with_source;
pub use exec_policy::load_exec_policy;

mod installation_id;
pub use installation_id::resolve_installation_id;

pub(crate) mod utils;
pub use utils::path_utils;

#[doc(hidden)]
pub(crate) mod prompt_debug;
#[doc(hidden)]
pub use prompt_debug::build_prompt_input;

pub mod windows_sandbox;
pub(crate) mod windows_sandbox_read_grants;
pub use windows_sandbox_read_grants::grant_read_root_non_elevated;

pub(crate) mod network_proxy_loader;
pub use network_proxy_loader::MtimeConfigReloader;
pub use network_proxy_loader::build_network_proxy_state;
pub use network_proxy_loader::build_network_proxy_state_and_reloader;

pub mod spawn;

#[cfg(test)]
pub mod test_support;
#[cfg(test)]
pub use test_support::PathBufExt;
#[cfg(test)]
pub use test_support::PathExt;
#[cfg(test)]
pub use test_support::TempDirExt;
#[cfg(test)]
pub use test_support::test_absolute_path;

pub mod util;

pub use octopus_protocol::config_types::ModelProviderAuthInfo;

pub mod config_types {
    pub use octopus_protocol::config_types::*;
}
pub mod openai_models {
    pub use octopus_protocol::openai_models::*;
}

pub mod otel_init;
