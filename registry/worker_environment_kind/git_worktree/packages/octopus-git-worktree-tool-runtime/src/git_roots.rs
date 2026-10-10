use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const REPO_ROOT_SLOT: &str = "repo_root";
pub const GIT_COMMON_ROOT_SLOT: &str = "git_common_root";

const MAX_GIT_POINTER_BYTES: u64 = 64 * 1024;

pub fn with_git_common_root(
    root_slots: &BTreeMap<String, PathBuf>,
) -> Result<BTreeMap<String, PathBuf>, String> {
    let repo_root = root_slots.get(REPO_ROOT_SLOT).ok_or_else(|| {
        format!("git_worktree requires an exact '{REPO_ROOT_SLOT}' door root binding")
    })?;
    let discovered = discover_git_common_root(repo_root)?;
    if let Some(bound) = root_slots.get(GIT_COMMON_ROOT_SLOT) {
        let bound = canonical_directory(bound, "bound git common root")?;
        if bound != discovered {
            return Err(format!(
                "git_worktree '{GIT_COMMON_ROOT_SLOT}' binding '{}' disagrees with repository metadata '{}'",
                bound.display(),
                discovered.display()
            ));
        }
    }
    let mut formed = root_slots.clone();
    formed.insert(GIT_COMMON_ROOT_SLOT.to_string(), discovered);
    Ok(formed)
}

pub(crate) fn discover_git_common_root(repo_root: &Path) -> Result<PathBuf, String> {
    let repo_root = canonical_directory(repo_root, "repository root")?;
    let marker = repo_root.join(".git");
    let metadata = match std::fs::symlink_metadata(&marker) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "git_worktree is unavailable: repository root '{}' has no .git marker",
                repo_root.display()
            ));
        }
        Err(error) => {
            return Err(format!(
                "git_worktree cannot inspect .git marker '{}': {error}",
                marker.display()
            ));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "git_worktree refuses symbolic .git marker '{}'",
            marker.display()
        ));
    }
    if metadata.is_dir() {
        return canonical_directory(&marker, "git common root");
    }
    if !metadata.is_file() {
        return Err(format!(
            "git_worktree .git marker '{}' is neither a directory nor a regular file",
            marker.display()
        ));
    }
    let marker_body = read_pointer_file(&marker, &metadata, "gitdir pointer")?;
    let git_dir = marker_body
        .strip_prefix("gitdir:")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            format!(
                "git_worktree .git file '{}' has no gitdir target",
                marker.display()
            )
        })?;
    let git_dir = resolve_pointer_target(&repo_root, git_dir);
    let git_dir = canonical_directory(&git_dir, "git directory")?;
    let common_marker = git_dir.join("commondir");
    let common_metadata = match std::fs::symlink_metadata(&common_marker) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(git_dir),
        Err(error) => {
            return Err(format!(
                "git_worktree cannot inspect commondir pointer '{}': {error}",
                common_marker.display()
            ));
        }
    };
    if common_metadata.file_type().is_symlink() || !common_metadata.is_file() {
        return Err(format!(
            "git_worktree commondir pointer '{}' is not a regular non-symlink file",
            common_marker.display()
        ));
    }
    let common = read_pointer_file(&common_marker, &common_metadata, "commondir pointer")?;
    canonical_directory(
        &resolve_pointer_target(&git_dir, &common),
        "git common root",
    )
}

fn read_pointer_file(
    path: &Path,
    metadata: &std::fs::Metadata,
    name: &str,
) -> Result<String, String> {
    if metadata.len() == 0 || metadata.len() > MAX_GIT_POINTER_BYTES {
        return Err(format!(
            "git_worktree {name} '{}' is {} bytes; expected 1..={MAX_GIT_POINTER_BYTES}",
            path.display(),
            metadata.len()
        ));
    }
    let body = std::fs::read_to_string(path).map_err(|error| {
        format!(
            "git_worktree cannot read {name} '{}': {error}",
            path.display()
        )
    })?;
    let value = body.trim();
    if value.is_empty() || value.contains('\n') || value.contains('\r') {
        return Err(format!(
            "git_worktree {name} '{}' must contain exactly one non-empty line",
            path.display()
        ));
    }
    Ok(value.to_string())
}

fn resolve_pointer_target(base: &Path, target: &str) -> PathBuf {
    let target = PathBuf::from(target);
    if target.is_absolute() {
        target
    } else {
        base.join(target)
    }
}

fn canonical_directory(path: &Path, name: &str) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!(
            "git_worktree {name} '{}' is not absolute",
            path.display()
        ));
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        format!(
            "git_worktree cannot inspect {name} '{}': {error}",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "git_worktree {name} '{}' is not a real directory",
            path.display()
        ));
    }
    std::fs::canonicalize(path).map_err(|error| {
        format!(
            "git_worktree cannot canonicalize {name} '{}': {error}",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_checkout_forms_its_git_directory() {
        let root = tempfile::tempdir().expect("repository root");
        let git = root.path().join(".git");
        std::fs::create_dir(&git).expect("git directory");
        let formed = with_git_common_root(&BTreeMap::from([(
            REPO_ROOT_SLOT.to_string(),
            root.path().to_path_buf(),
        )]))
        .expect("direct checkout root forms");
        assert_eq!(
            formed.get(GIT_COMMON_ROOT_SLOT),
            Some(&std::fs::canonicalize(git).unwrap())
        );
    }

    #[test]
    fn linked_checkout_resolves_commondir_inside_the_kind() {
        let root = tempfile::tempdir().expect("layout root");
        let checkout = root.path().join("checkout");
        let common = root.path().join("repository/.git");
        let git_dir = common.join("worktrees/checkout");
        std::fs::create_dir_all(&checkout).expect("checkout");
        std::fs::create_dir_all(&git_dir).expect("git dir");
        std::fs::write(
            checkout.join(".git"),
            format!("gitdir: {}\n", git_dir.display()),
        )
        .expect("gitfile");
        std::fs::write(git_dir.join("commondir"), "../..\n").expect("commondir");

        assert_eq!(
            discover_git_common_root(&checkout).expect("linked checkout root forms"),
            std::fs::canonicalize(common).unwrap()
        );
    }

    #[test]
    fn non_git_directory_is_a_kind_local_unavailability() {
        let root = tempfile::tempdir().expect("non-git root");
        let error = discover_git_common_root(root.path()).expect_err("git is unavailable");
        assert!(error.contains("git_worktree is unavailable"));
        assert!(error.contains("has no .git marker"));
    }
}
