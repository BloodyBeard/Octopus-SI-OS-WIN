use std::sync::Arc;

use octopus_cxhome::UserInstructionsProvider;
use octopus_protocol::error::CodexErr;
use octopus_protocol::error::Result as CodexResult;
use octopus_protocol::models::ResponseItem;
use octopus_protocol::user_input::UserInput;

use crate::config::Config;
#[doc(hidden)]
pub async fn build_prompt_input(
    _config: Config,
    _input: Vec<UserInput>,
    _user_instructions_provider: Arc<dyn UserInstructionsProvider>,
) -> CodexResult<Vec<ResponseItem>> {
    Err(CodexErr::Fatal(
        "`octopus debug prompt-input` is not available in this build: it drove the now-deleted \
         Codex engine (ThreadManager/Session) to assemble the debug prompt. Re-express it on the \
         spine's prompt-assembly primitives to restore it."
            .to_string(),
    ))
}
