use std::path::PathBuf;

use octopus_substrate_contract::permissions::{
    FileSystemAccessMode, FileSystemPath, FileSystemSandboxKind, NetworkSandboxPolicy,
};
use octopus_worker_env::{AbsPath, NetworkCeiling, ProjectedSandbox};

use crate::{permission_profile_for_projected_sandbox, sandbox_policy_for_projected_sandbox};

/// A fixture path absolute on this platform (`/tmp/wt`, or `C:\tmp\wt` on
/// Windows, where a path without a drive is not absolute).
fn native(p: &str) -> PathBuf {
    octopus_util_abspath::test_support::test_path_buf(p)
}

fn abs(p: &str) -> AbsPath {
    AbsPath::new(&native(p)).unwrap()
}

fn worktree_projection() -> ProjectedSandbox {
    ProjectedSandbox::from_parts(
        vec![abs("/tmp/wt")],
        vec![abs("/tmp/wt/.git")],
        vec![abs("/tmp/repo")],
        NetworkCeiling::Denied,
    )
}

fn has_entry(
    policy: &octopus_substrate_contract::permissions::FileSystemSandboxPolicy,
    abs_path: &str,
    access: FileSystemAccessMode,
) -> bool {
    policy.entries.iter().any(|e| match &e.path {
        FileSystemPath::Path { path } => {
            path.as_path() == native(abs_path) && e.access == access
        }
        _ => false,
    })
}

fn has_writable_entry(
    policy: &octopus_substrate_contract::permissions::FileSystemSandboxPolicy,
    abs_path: &str,
) -> bool {
    policy.entries.iter().any(|e| match &e.path {
        FileSystemPath::Path { path } => {
            path.as_path() == native(abs_path) && e.access == FileSystemAccessMode::Write
        }
        _ => false,
    })
}

#[test]
fn writable_only_at_projected_root_protected_subpath_read_only() {
    let projected = worktree_projection();

    let policy = sandbox_policy_for_projected_sandbox(&projected).unwrap();
    assert_eq!(policy.kind, FileSystemSandboxKind::Restricted);

    assert!(
        has_entry(&policy, "/tmp/wt", FileSystemAccessMode::Write),
        "projected writable root /tmp/wt is writable in the OS policy"
    );

    let writable_count = policy
        .entries
        .iter()
        .filter(|e| e.access == FileSystemAccessMode::Write)
        .count();
    assert_eq!(
        writable_count, 1,
        "exactly one writable entry — the floor cannot widen beyond the projection"
    );

    assert!(
        has_entry(&policy, "/tmp/wt/.git", FileSystemAccessMode::Read),
        ".git protected subpath is read-only in the OS policy"
    );
    assert!(
        !has_writable_entry(&policy, "/tmp/wt/.git"),
        ".git is never writable (constitution-lock guard survives translation)"
    );

    assert!(
        has_entry(&policy, "/tmp/repo", FileSystemAccessMode::Read),
        "declared readable root is readable"
    );

    let profile =
        permission_profile_for_projected_sandbox(&projected, &native("/tmp/wt")).unwrap();
    assert_eq!(
        profile.network_sandbox_policy(),
        NetworkSandboxPolicy::Restricted,
        "denied network ceiling never becomes Enabled"
    );
}

#[test]
fn read_only_projection_is_strict_no_ambient_default() {
    let projected = ProjectedSandbox::from_parts(
        vec![],
        vec![],
        vec![abs("/tmp/repo")],
        NetworkCeiling::Denied,
    );
    assert!(
        projected.writable_roots().is_empty(),
        "precondition: read-only projection has no writable roots"
    );

    let policy = sandbox_policy_for_projected_sandbox(&projected).unwrap();
    let writable_count = policy
        .entries
        .iter()
        .filter(|e| e.access == FileSystemAccessMode::Write)
        .count();
    assert_eq!(
        writable_count, 0,
        "read-only floor has zero writable entries — fail-closed, no defaulted workspace-write"
    );
    assert!(
        !policy.has_full_disk_write_access(),
        "read-only floor never grants full-disk write"
    );

    let profile =
        permission_profile_for_projected_sandbox(&projected, &native("/tmp/wt")).unwrap();
    assert_eq!(
        profile.network_sandbox_policy(),
        NetworkSandboxPolicy::Restricted,
        "denied net is Restricted, never Enabled"
    );
}

#[test]
fn determinism_same_projection_same_policy() {
    let a = sandbox_policy_for_projected_sandbox(&worktree_projection()).unwrap();
    let b = sandbox_policy_for_projected_sandbox(&worktree_projection()).unwrap();
    assert_eq!(
        a, b,
        "same projection → byte-identical OS policy (replay coherence)"
    );
}
