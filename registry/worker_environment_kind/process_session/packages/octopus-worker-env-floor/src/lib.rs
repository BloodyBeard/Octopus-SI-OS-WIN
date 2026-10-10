use std::path::Path;

use octopus_substrate_contract::permission_profile::PermissionProfile;
use octopus_substrate_contract::permissions::{
    FileSystemAccessMode, FileSystemPath, FileSystemSandboxEntry, FileSystemSandboxPolicy,
    FileSystemSpecialPath, NetworkSandboxPolicy,
};
use octopus_util_abspath::AbsolutePathBuf;
use octopus_worker_env::{NetworkCeiling, ProjectedSandbox};

#[derive(Debug)]
pub struct FloorTranslationError {
    pub path: String,
    pub reason: String,
}

impl std::fmt::Display for FloorTranslationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cannot express projected sandbox root `{}` as an OS sandbox path: {}",
            self.path, self.reason
        )
    }
}

impl std::error::Error for FloorTranslationError {}

fn network_policy_for(net: NetworkCeiling) -> NetworkSandboxPolicy {
    match net {
        NetworkCeiling::Full => NetworkSandboxPolicy::Enabled,
        NetworkCeiling::Restricted | NetworkCeiling::Denied => NetworkSandboxPolicy::Restricted,
    }
}

fn fs_path_for(p: &octopus_worker_env::AbsPath) -> Result<FileSystemPath, FloorTranslationError> {
    let abs =
        AbsolutePathBuf::from_absolute_path(p.as_path()).map_err(|e| FloorTranslationError {
            path: p.as_str().to_string(),
            reason: e.to_string(),
        })?;
    Ok(FileSystemPath::Path { path: abs })
}

fn minimal_platform_read_entry() -> FileSystemSandboxEntry {
    FileSystemSandboxEntry {
        path: FileSystemPath::Special {
            value: FileSystemSpecialPath::Minimal,
        },
        access: FileSystemAccessMode::Read,
    }
}

pub fn sandbox_policy_for_projected_sandbox(
    projected: &ProjectedSandbox,
) -> Result<FileSystemSandboxPolicy, FloorTranslationError> {
    if projected.writable_roots().is_empty() {
        if projected.readable_roots().is_empty() {
            return Ok(FileSystemSandboxPolicy::read_only());
        }
        let mut entries = Vec::with_capacity(projected.readable_roots().len() + 1);
        entries.push(minimal_platform_read_entry());
        for r in projected.readable_roots() {
            entries.push(FileSystemSandboxEntry {
                path: fs_path_for(r)?,
                access: FileSystemAccessMode::Read,
            });
        }
        return Ok(FileSystemSandboxPolicy::restricted(entries));
    }

    let mut entries = Vec::with_capacity(
        projected.writable_roots().len()
            + projected.readable_roots().len()
            + projected.read_only_subpaths().len()
            + 1,
    );
    entries.push(minimal_platform_read_entry());

    for r in projected.writable_roots() {
        entries.push(FileSystemSandboxEntry {
            path: fs_path_for(r)?,
            access: FileSystemAccessMode::Write,
        });
    }

    for r in projected.readable_roots() {
        entries.push(FileSystemSandboxEntry {
            path: fs_path_for(r)?,
            access: FileSystemAccessMode::Read,
        });
    }

    for r in projected.read_only_subpaths() {
        entries.push(FileSystemSandboxEntry {
            path: fs_path_for(r)?,
            access: FileSystemAccessMode::Read,
        });
    }

    Ok(FileSystemSandboxPolicy::restricted(entries))
}

pub fn permission_profile_for_projected_sandbox(
    projected: &ProjectedSandbox,
    _sandbox_policy_cwd: &Path,
) -> Result<PermissionProfile, FloorTranslationError> {
    let file_system = sandbox_policy_for_projected_sandbox(projected)?;
    let network = network_policy_for(projected.network());
    Ok(PermissionProfile::from_runtime_permissions(
        &file_system,
        network,
    ))
}

#[cfg(test)]
#[path = "worker_env_floor_tests.rs"]
mod tests;
