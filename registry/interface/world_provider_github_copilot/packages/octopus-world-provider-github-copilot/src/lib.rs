mod anthropic_life;
mod driver;
mod life;

pub use anthropic_life::interface_life as anthropic_interface_life;
pub use driver::{
    GitHubCopilotAnthropicDriver, GitHubCopilotFailure, GitHubCopilotResponsesDriver,
};
pub use life::interface_life as responses_interface_life;
