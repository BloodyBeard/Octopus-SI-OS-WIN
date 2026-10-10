use std::path::PathBuf;
use std::sync::Arc;

use octopus_cxhome::LoadUserInstructionsFuture;
use octopus_cxhome::LoadedUserInstructions;
use octopus_cxhome::UserInstructionsProvider;
use octopus_login::AuthManager;
use octopus_login::CodexAuth;
use octopus_util_abspath::AbsolutePathBuf;
use std::path::Path;
use tempfile::TempDir;

#[derive(Debug, Default)]
pub struct EmptyUserInstructionsProvider;

impl UserInstructionsProvider for EmptyUserInstructionsProvider {
    fn load_user_instructions(&self) -> LoadUserInstructionsFuture<'_> {
        Box::pin(async { LoadedUserInstructions::default() })
    }
}

pub fn auth_manager_from_auth(auth: CodexAuth) -> Arc<AuthManager> {
    AuthManager::from_auth_for_testing(auth)
}

pub fn auth_manager_from_auth_with_home(auth: CodexAuth, codex_home: PathBuf) -> Arc<AuthManager> {
    AuthManager::from_auth_for_testing_with_home(auth, codex_home)
}

pub trait PathExt {
    fn abs(&self) -> AbsolutePathBuf;
}

impl PathExt for Path {
    fn abs(&self) -> AbsolutePathBuf {
        AbsolutePathBuf::from_absolute_path(self).unwrap_or_else(|err| {
            panic!(
                "test path should resolve to an absolute path ({}): {err}",
                self.display()
            )
        })
    }
}

pub trait PathBufExt {
    fn abs(&self) -> AbsolutePathBuf;
}

impl PathBufExt for PathBuf {
    fn abs(&self) -> AbsolutePathBuf {
        self.as_path().abs()
    }
}

pub trait TempDirExt {
    fn abs(&self) -> AbsolutePathBuf;
}

impl TempDirExt for TempDir {
    fn abs(&self) -> AbsolutePathBuf {
        self.path().abs()
    }
}

pub fn test_absolute_path(path: &str) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path_checked(Path::new(path))
        .unwrap_or_else(|err| panic!("test path should already be absolute ({path}): {err}"))
}
