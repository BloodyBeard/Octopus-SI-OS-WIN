use std::future::Future;
use std::pin::Pin;

use octopus_util_abspath::AbsolutePathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserInstructions {
    pub text: String,
    pub source: AbsolutePathBuf,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LoadedUserInstructions {
    pub instructions: Option<UserInstructions>,
    pub warnings: Vec<String>,
}

pub type LoadUserInstructionsFuture<'a> =
    Pin<Box<dyn Future<Output = LoadedUserInstructions> + Send + 'a>>;

pub trait UserInstructionsProvider: Send + Sync {
    fn load_user_instructions(&self) -> LoadUserInstructionsFuture<'_>;
}
