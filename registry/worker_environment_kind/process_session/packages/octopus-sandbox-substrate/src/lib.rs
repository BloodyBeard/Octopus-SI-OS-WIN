pub mod boundary;
#[cfg(target_os = "linux")]
mod bwrap;
mod deny_read_resolver;
pub mod landlock;
mod manager;
pub mod network_proxy;
pub mod policy_transforms;
#[cfg(target_os = "macos")]
pub mod seatbelt;
mod windows;
#[cfg(target_os = "windows")]
mod windows_sandbox_inline;

pub use boundary::project_sandbox;
pub use network_proxy::NetworkProxy;
pub use octopus_worker_env_floor::{
    FloorTranslationError, permission_profile_for_projected_sandbox,
    sandbox_policy_for_projected_sandbox,
};

#[cfg(target_os = "linux")]
pub use bwrap::find_bwrap_for_sandbox;
#[cfg(target_os = "linux")]
pub use bwrap::find_system_bwrap_in_path;
#[cfg(target_os = "linux")]
pub use bwrap::system_bwrap_warning;
pub use manager::SandboxCommand;
pub use manager::SandboxDirectSpawnTransformRequest;
pub use manager::SandboxExecRequest;
pub use manager::SandboxManager;
pub use manager::SandboxTransformError;
pub use manager::SandboxTransformRequest;
pub use manager::SandboxType;
pub use manager::SandboxablePreference;
#[allow(deprecated)]
pub use manager::compatibility_sandbox_policy_for_permission_profile;
pub use manager::get_platform_sandbox;
pub use manager::with_managed_mitm_ca_readable_root;
pub use windows::WindowsSandboxFilesystemOverrides;
pub use windows::permission_profile_supports_windows_restricted_token_sandbox;
pub use windows::resolve_windows_elevated_filesystem_overrides;
pub use windows::resolve_windows_restricted_token_filesystem_overrides;
pub use windows::unsupported_windows_restricted_token_sandbox_reason;
pub use windows::windows_sandbox_uses_elevated_backend;

#[cfg(not(target_os = "linux"))]
pub fn system_bwrap_warning(
    _permission_profile: &octopus_substrate_contract::permission_profile::PermissionProfile,
) -> Option<String> {
    None
}
