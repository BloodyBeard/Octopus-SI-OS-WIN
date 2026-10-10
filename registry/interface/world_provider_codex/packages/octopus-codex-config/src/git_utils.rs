use std::ffi::OsStr;

use octopus_file_system::ExecutorFileSystem;
use octopus_util_abspath::AbsolutePathBuf;
use octopus_util_pathuri::PathUri;

async fn get_git_repo_root_with_fs(
    fs: &dyn ExecutorFileSystem,
    cwd: &AbsolutePathBuf,
) -> Option<AbsolutePathBuf> {
    let cwd_uri = PathUri::from_abs_path(cwd);
    let base = match fs.get_metadata(&cwd_uri, None).await {
        Ok(metadata) if metadata.is_directory => cwd.clone(),
        _ => cwd.parent()?,
    };
    find_ancestor_git_entry_with_fs(fs, &base)
        .await
        .map(|(repo_root, _)| repo_root)
}

async fn find_ancestor_git_entry_with_fs(
    fs: &dyn ExecutorFileSystem,
    base_dir: &AbsolutePathBuf,
) -> Option<(AbsolutePathBuf, AbsolutePathBuf)> {
    for dir in base_dir.ancestors() {
        let dot_git = dir.join(".git");
        let dot_git_uri = PathUri::from_abs_path(&dot_git);
        if fs.get_metadata(&dot_git_uri, None).await.is_ok() {
            return Some((dir, dot_git));
        }
    }
    None
}

pub async fn resolve_root_git_project_for_trust(
    fs: &dyn ExecutorFileSystem,
    cwd: &AbsolutePathBuf,
) -> Option<AbsolutePathBuf> {
    let repo_root = get_git_repo_root_with_fs(fs, cwd).await?;
    let dot_git = repo_root.join(".git");
    let dot_git_uri = PathUri::from_abs_path(&dot_git);
    if fs.get_metadata(&dot_git_uri, None).await.ok()?.is_directory {
        return Some(repo_root);
    }

    let git_dir_s = fs.read_file_text(&dot_git_uri, None).await.ok()?;
    let git_dir_rel = git_dir_s.trim().strip_prefix("gitdir:")?.trim();
    if git_dir_rel.is_empty() {
        return None;
    }

    let git_dir_path = AbsolutePathBuf::resolve_path_against_base(git_dir_rel, repo_root.as_path());
    let worktrees_dir = git_dir_path.parent()?;
    if worktrees_dir.as_path().file_name() != Some(OsStr::new("worktrees")) {
        return None;
    }

    let common_dir = worktrees_dir.parent()?;
    common_dir.parent()
}
