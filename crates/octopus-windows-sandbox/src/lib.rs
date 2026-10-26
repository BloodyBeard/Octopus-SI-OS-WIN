//! Stand-in for Codex's Windows sandbox crate (`codex-rs/windows-sandbox-rs`).
//!
//! Upstream Octopus removed that crate but kept the code that calls it from the
//! terminal client and the debug sandbox launcher. This crate keeps those
//! callers compiling on Windows. Every operation reports that the Windows
//! sandbox is unavailable, which the callers already treat as "not set up".
//!
//! Agent tools are not sandboxed through this crate: process rooms run their
//! commands through the managed Codex CLI (`codex.exe` and its sandbox helpers),
//! which carries the Windows sandbox itself. Vendoring the real crate replaces
//! this one without touching its callers.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

pub const UNAVAILABLE: &str =
    "the Windows sandbox is not available from this Octopus component; the managed Codex CLI provides it";

fn unavailable() -> anyhow::Error {
    anyhow::anyhow!(UNAVAILABLE)
}

pub fn sandbox_setup_is_complete(_codex_home: &Path) -> bool {
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupErrorCode {
    OrchestratorHelperLaunchCanceled,
}

impl SetupErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OrchestratorHelperLaunchCanceled => "orchestrator_helper_launch_canceled",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SetupFailure {
    pub code: SetupErrorCode,
    pub message: String,
}

/// Setup never runs here, so no error carries a setup failure.
pub fn extract_setup_failure(_err: &anyhow::Error) -> Option<SetupFailure> {
    None
}

pub fn sanitize_setup_metric_tag_value(value: &str) -> String {
    value
        .chars()
        .map(|character| if character.is_ascii_alphanumeric() || character == '_' { character } else { '_' })
        .collect()
}

#[derive(Debug)]
pub struct ResolvedWindowsSandboxPermissions {
    _private: (),
}

impl ResolvedWindowsSandboxPermissions {
    pub fn try_from_permission_profile_for_workspace_roots<P: ?Sized, R>(
        _permission_profile: &P,
        _workspace_roots: &[R],
    ) -> anyhow::Result<Self> {
        Err(unavailable())
    }
}

pub struct SandboxSetupRequest<'a> {
    pub permissions: &'a ResolvedWindowsSandboxPermissions,
    pub command_cwd: &'a Path,
    pub env_map: &'a HashMap<String, String>,
    pub codex_home: &'a Path,
    pub proxy_enforced: bool,
}

#[derive(Debug, Default)]
pub struct SetupRootOverrides;

pub fn run_elevated_setup(
    _request: SandboxSetupRequest<'_>,
    _overrides: SetupRootOverrides,
) -> anyhow::Result<()> {
    Err(unavailable())
}

pub fn run_elevated_provisioning_setup(_codex_home: &Path, _real_user: &str) -> anyhow::Result<()> {
    Err(unavailable())
}

pub fn run_windows_sandbox_legacy_preflight<P: ?Sized, R>(
    _permission_profile: &P,
    _workspace_roots: &[R],
    _codex_home: &Path,
    _cwd: &Path,
    _env_map: &HashMap<String, String>,
) -> anyhow::Result<()> {
    Err(unavailable())
}

#[allow(clippy::too_many_arguments)]
pub fn run_setup_refresh_with_extra_read_roots<P: ?Sized, R>(
    _permission_profile: &P,
    _workspace_roots: &[R],
    _command_cwd: &Path,
    _env_map: &HashMap<String, String>,
    _codex_home: &Path,
    _extra_read_roots: Vec<PathBuf>,
    _proxy_enforced: bool,
) -> anyhow::Result<()> {
    Err(unavailable())
}

pub fn apply_world_writable_scan_and_denies_for_permissions(
    _logs_base_dir: &Path,
    _cwd: &Path,
    _env_map: &HashMap<String, String>,
    _permissions: &ResolvedWindowsSandboxPermissions,
    _scan_root: Option<&Path>,
) -> anyhow::Result<()> {
    Err(unavailable())
}

pub struct WindowsSandboxSessionRequest<'a, P: ?Sized, R, L> {
    pub permission_profile: &'a P,
    pub workspace_roots: &'a [R],
    pub octopus_cxhome: &'a Path,
    pub command: Vec<String>,
    pub cwd: &'a Path,
    pub env_map: HashMap<String, String>,
    pub windows_sandbox_level: L,
    pub proxy_enforced: bool,
    pub timeout_ms: Option<u64>,
    pub read_roots_override: Option<&'a [PathBuf]>,
    pub read_roots_include_platform_defaults: bool,
    pub write_roots_override: Option<&'a [PathBuf]>,
    pub deny_read_paths_override: &'a [R],
    pub deny_write_paths_override: &'a [R],
    pub tty: bool,
    pub stdin_open: bool,
    pub use_private_desktop: bool,
}

/// No sandboxed session can be spawned here, so none can exist.
pub enum SpawnedProcess {}

pub async fn spawn_windows_sandbox_session_for_level<P: ?Sized, R, L>(
    _request: WindowsSandboxSessionRequest<'_, P, R, L>,
) -> anyhow::Result<SpawnedProcess> {
    Err(unavailable())
}

pub async fn forward_sandbox_session_stdio(spawned: SpawnedProcess) -> i32 {
    match spawned {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setup_reports_the_sandbox_unavailable() {
        let roots: [PathBuf; 0] = [];
        assert!(!sandbox_setup_is_complete(Path::new("/home")));
        let error = ResolvedWindowsSandboxPermissions::try_from_permission_profile_for_workspace_roots(&(), &roots)
            .unwrap_err();
        assert_eq!(error.to_string(), UNAVAILABLE);
        assert!(extract_setup_failure(&error).is_none());
        assert!(run_elevated_provisioning_setup(Path::new("/home"), "user").is_err());
    }
}
